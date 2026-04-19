use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Result;
use datafusion::arrow::datatypes::SchemaRef;
use datafusion::arrow::record_batch::RecordBatch;
use datafusion::catalog::TableProvider;
use datafusion::execution::context::SessionConfig;
use datafusion::physical_plan::ExecutionPlan;
use datafusion::physical_plan::ExecutionPlanProperties;
use futures::StreamExt;

use crate::client_session::ClientSession;
use crate::engine::EngineState;
use crate::error::ServerError;

pub enum ExecutionResult {
    Query {
        schema: SchemaRef,
        batches: Vec<RecordBatch>,
    },
    AffectedRows {
        rows: u64,
    },
    CommandComplete {
        tag: String,
    },
}

pub struct QueryExecutor;

impl QueryExecutor {
    pub async fn execute(
        engine: &EngineState,
        session: &ClientSession,
        sql: &str,
    ) -> Result<ExecutionResult> {
        let stmt_type = classify_statement(sql);

        match stmt_type {
            StatementType::CreateTable => Self::execute_create_table(engine, session, sql).await,
            StatementType::DropTable => Self::execute_drop_table(engine, session, sql).await,
            StatementType::Insert => Self::execute_insert(engine, session, sql).await,
            StatementType::Query => Self::execute_query(engine, session, sql).await,
            StatementType::Unsupported => {
                Err(ServerError::UnsupportedStatement(sql.to_string()).into())
            }
        }
    }

    fn build_session_context(engine: &EngineState) -> Result<datafusion::execution::context::SessionContext> {
        let config = SessionConfig::new()
            .with_information_schema(true);
        let ctx = datafusion::execution::context::SessionContext::new_with_config(config);

        let system_table_dir = default_system_table_dir();
        sql::catalog::register_system_catalog(&ctx, &system_table_dir)?;
        sql::udf::register_version_udf(
            &ctx,
            sql::udf::VersionInfo {
                commit_short: engine.build_version.commit_short.clone(),
                build_time: engine.build_version.build_time.clone(),
            },
        )?;

        register_user_tables(&ctx, engine);

        Ok(ctx)
    }

    async fn execute_create_table(
        engine: &EngineState,
        _session: &ClientSession,
        sql: &str,
    ) -> Result<ExecutionResult> {
        let ctx = Self::build_session_context(engine)?;
        ctx.sql(sql).await?.collect().await?;

        let table_name = extract_table_name(sql, "CREATE TABLE")
            .ok_or_else(|| anyhow::anyhow!("cannot parse table name from CREATE TABLE"))?;

        let database = "default";

        if engine.catalog.table(database, &table_name).is_some() {
            return Err(anyhow::anyhow!("table already exists: {database}.{table_name}"));
        }

        let schema = ctx
            .table_provider(&table_name)
            .await?
            .schema();

        engine.catalog.create_table(database, &table_name, schema.clone())?;
        engine.store.create_table(format!("{database}.{table_name}"), schema)?;

        Ok(ExecutionResult::CommandComplete {
            tag: "CREATE TABLE".to_string(),
        })
    }

    async fn execute_drop_table(
        engine: &EngineState,
        _session: &ClientSession,
        sql: &str,
    ) -> Result<ExecutionResult> {
        let table_name = extract_table_name(sql, "DROP TABLE")
            .ok_or_else(|| anyhow::anyhow!("cannot parse table name from DROP TABLE"))?;

        let database = "default";

        engine.catalog.drop_table(database, &table_name)?;
        engine.store.drop_table(format!("{database}.{table_name}"))?;

        Ok(ExecutionResult::CommandComplete {
            tag: "DROP TABLE".to_string(),
        })
    }

    async fn execute_insert(
        engine: &EngineState,
        session: &ClientSession,
        sql: &str,
    ) -> Result<ExecutionResult> {
        let database = session.current_database();
        let table_name = extract_table_name(sql, "INSERT INTO")
            .ok_or_else(|| anyhow::anyhow!("cannot parse table name from INSERT"))?;

        let table_key = format!("{database}.{table_name}");

        let table_data = engine
            .store
            .table(&table_key)
            .ok_or_else(|| anyhow::anyhow!("table not found: {table_key}"))?;

        let mem_table = table_data.mem_table()?;

        let ctx = Self::build_session_context(engine)?;
        ctx.deregister_table(&table_name)?;
        ctx.register_table(table_name.clone(), mem_table)?;

        let df = ctx.sql(sql).await?;
        let result = df.collect().await?;

        let inserted_rows: usize = result
            .iter()
            .map(|b| {
                if b.num_columns() > 0 {
                    let col = b.column(0);
                    col.as_any()
                        .downcast_ref::<datafusion::arrow::array::UInt64Array>()
                        .map(|arr| arr.iter().map(|v| v.unwrap_or(0) as usize).sum::<usize>())
                        .unwrap_or_else(|| b.num_rows())
                } else {
                    b.num_rows()
                }
            })
            .sum();

        let provider = ctx.table_provider(&table_name).await?;
        let written_mem = provider
            .as_any()
            .downcast_ref::<datafusion::datasource::MemTable>()
            .ok_or_else(|| anyhow::anyhow!("expected MemTable for {table_name}"))?;

        let state = ctx.state();
        let all_batches = scan_mem_table_batches(written_mem, &state).await?;

        table_data.replace_batches(all_batches)?;

        Ok(ExecutionResult::AffectedRows {
            rows: inserted_rows as u64,
        })
    }

    async fn execute_query(
        engine: &EngineState,
        _session: &ClientSession,
        sql: &str,
    ) -> Result<ExecutionResult> {
        let ctx = Self::build_session_context(engine)?;
        let df = ctx.sql(sql).await?;
        let batches = df.collect().await?;

        let schema = batches
            .first()
            .map(|b| b.schema())
            .unwrap_or_else(|| {
                std::sync::Arc::new(datafusion::arrow::datatypes::Schema::empty())
            });

        Ok(ExecutionResult::Query { schema, batches })
    }
}

enum StatementType {
    CreateTable,
    DropTable,
    Insert,
    Query,
    Unsupported,
}

fn classify_statement(sql: &str) -> StatementType {
    let normalized = sql.trim().to_uppercase();
    if normalized.starts_with("CREATE TABLE") {
        StatementType::CreateTable
    } else if normalized.starts_with("DROP TABLE") {
        StatementType::DropTable
    } else if normalized.starts_with("INSERT") {
        StatementType::Insert
    } else if normalized.starts_with("SELECT")
        || normalized.starts_with("EXPLAIN")
        || normalized.starts_with("SHOW")
        || normalized.starts_with("WITH")
    {
        StatementType::Query
    } else {
        StatementType::Unsupported
    }
}

fn extract_table_name(sql: &str, prefix: &str) -> Option<String> {
    let upper = sql.to_uppercase();
    let start = upper.find(prefix)?;
    let after_prefix = &sql[start + prefix.len()..].trim_start();

    let first_token = after_prefix.split(&[' ', '(', '\t', '\n'][..]).next()?;
    let name = first_token.trim();
    if name.is_empty() {
        return None;
    }
    Some(name.to_string())
}

fn default_system_table_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("bootstrap/system_tables.yaml")
}

fn register_user_tables(
    ctx: &datafusion::execution::context::SessionContext,
    engine: &EngineState,
) {
    let databases = match engine.catalog.databases.read() {
        Ok(d) => d,
        Err(_) => return,
    };

    for (_db_name, db_meta) in databases.iter() {
        for (table_name, _table_meta) in &db_meta.tables {
            let table_key = format!("{}.{}", db_meta.name, table_name);
            if let Some(table_data) = engine.store.table(&table_key) {
                if let Ok(mem_table) = table_data.mem_table() {
                    let _ = ctx.register_table(table_name.clone(), mem_table);
                }
            }
        }
    }
}

async fn scan_mem_table_batches(
    mem_table: &datafusion::datasource::MemTable,
    state: &datafusion::execution::context::SessionState,
) -> Result<Vec<RecordBatch>> {
    let scan_plan: Arc<dyn ExecutionPlan> = mem_table.scan(state, None, &[], None).await?;
    let task_ctx = state.task_ctx();
    let mut all_batches = Vec::new();
    for i in 0..scan_plan.output_partitioning().partition_count() {
        let mut stream = scan_plan.execute(i, task_ctx.clone())?;
        loop {
            match tokio::time::timeout(
                std::time::Duration::from_secs(10),
                stream.next(),
            )
            .await
            {
                Ok(Some(batch)) => all_batches.push(batch?),
                Ok(None) => break,
                Err(_) => anyhow::bail!("timeout scanning mem table"),
            }
        }
    }
    Ok(all_batches)
}
