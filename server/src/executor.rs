use std::sync::Arc;

use anyhow::Result;
use datafusion::arrow::datatypes::SchemaRef;
use datafusion::arrow::record_batch::RecordBatch;

use crate::client_session::ClientSession;
use crate::engine::EngineState;

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
        _session: &ClientSession,
        sql: &str,
    ) -> Result<ExecutionResult> {
        let normalized = sql.trim().to_uppercase();
        if normalized.starts_with("SET") {
            return Ok(ExecutionResult::CommandComplete {
                tag: "SET".to_string(),
            });
        }

        let ctx = engine.session_context()?;

        let df = ctx.sql(sql).await?;
        let schema: SchemaRef = Arc::new(df.schema().into());
        let batches = df.collect().await?;

        if normalized.starts_with("INSERT") {
            let inserted_rows: usize = batches
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
            return Ok(ExecutionResult::AffectedRows {
                rows: inserted_rows as u64,
            });
        }

        if normalized.starts_with("CREATE") || normalized.starts_with("DROP") {
            return Ok(ExecutionResult::CommandComplete {
                tag: if normalized.starts_with("CREATE") {
                    "CREATE TABLE".to_string()
                } else {
                    "DROP TABLE".to_string()
                },
            });
        }

        Ok(ExecutionResult::Query { schema, batches })
    }
}
