use std::sync::Arc;

use datafusion::arrow::datatypes::{DataType, Field, Schema};
use datafusion::catalog::SchemaProvider;
use server::EngineState;
use server::engine::{DEFAULT_CATALOG, DEFAULT_SCHEMA};
use server::provider::TodbTableProvider;
use server::schema_provider::TodbSchemaProvider;
use server::version::BuildVersion;

fn int_schema() -> Arc<Schema> {
    Arc::new(Schema::new(vec![Field::new("a", DataType::Int32, false)]))
}

#[tokio::test]
async fn register_table_and_retrieve() {
    let schema_provider = Arc::new(TodbSchemaProvider::new());
    let table = Arc::new(TodbTableProvider::new(int_schema()));

    schema_provider
        .register_table("t".to_string(), table.clone())
        .expect("register");

    let found = schema_provider.table("t").await.expect("lookup");
    assert!(found.is_some());

    let names = schema_provider.table_names();
    assert_eq!(names, vec!["t"]);
}

#[tokio::test]
async fn duplicate_register_returns_error() {
    let schema_provider = Arc::new(TodbSchemaProvider::new());
    let table = Arc::new(TodbTableProvider::new(int_schema()));

    schema_provider
        .register_table("t".to_string(), table.clone())
        .expect("register");

    let result = schema_provider.register_table("t".to_string(), table);
    assert!(result.is_err());
}

#[tokio::test]
async fn deregister_removes_table() {
    let schema_provider = Arc::new(TodbSchemaProvider::new());
    let table = Arc::new(TodbTableProvider::new(int_schema()));

    schema_provider
        .register_table("t".to_string(), table)
        .expect("register");

    let removed = schema_provider.deregister_table("t").expect("deregister");
    assert!(removed.is_some());

    let found = schema_provider.table("t").await.expect("lookup");
    assert!(found.is_none());
}

#[tokio::test]
async fn system_table_retrievable_but_not_in_table_names() {
    let schema_provider = Arc::new(TodbSchemaProvider::new());
    let sys_table = Arc::new(TodbTableProvider::new(int_schema()));

    schema_provider.register_system_table("version_t", sys_table);

    let found = schema_provider.table("version_t").await.expect("lookup");
    assert!(found.is_some());

    let names = schema_provider.table_names();
    assert!(
        names.is_empty(),
        "system tables should not appear in table_names"
    );
}

#[tokio::test]
async fn table_exist_covers_both_user_and_system() {
    let schema_provider = Arc::new(TodbSchemaProvider::new());
    let table = Arc::new(TodbTableProvider::new(int_schema()));

    schema_provider
        .register_table("users".to_string(), table.clone())
        .expect("register users");
    schema_provider.register_system_table("version_t", table);

    assert!(schema_provider.table_exist("users"));
    assert!(schema_provider.table_exist("version_t"));
    assert!(!schema_provider.table_exist("nonexistent"));
}

#[tokio::test]
async fn engine_state_uses_custom_catalog_provider_list() {
    let engine = EngineState::new(BuildVersion {
        commit_short: "test0000".to_string(),
        build_time: "20260419000000".to_string(),
    })
    .expect("engine state");

    let catalog = engine
        .catalog_list
        .catalog(DEFAULT_CATALOG)
        .expect("default catalog exists");
    let schema = catalog
        .schema(DEFAULT_SCHEMA)
        .expect("default schema exists");

    assert!(
        !engine
            .catalog_list
            .as_any()
            .is::<datafusion::catalog_common::MemoryCatalogProviderList>()
    );
    assert!(
        !catalog
            .as_any()
            .is::<datafusion::catalog_common::MemoryCatalogProvider>()
    );
    assert!(schema.as_any().is::<TodbSchemaProvider>());
}

#[tokio::test]
async fn full_sql_roundtrip_through_session_context() {
    let engine = EngineState::new(BuildVersion {
        commit_short: "test0000".to_string(),
        build_time: "20260419000000".to_string(),
    })
    .expect("engine state");
    let ctx = engine.session_context().expect("session context");

    ctx.sql("CREATE TABLE t(a INT)")
        .await
        .expect("create table")
        .collect()
        .await
        .expect("collect");

    ctx.sql("INSERT INTO t VALUES (1), (2)")
        .await
        .expect("insert")
        .collect()
        .await
        .expect("collect insert");

    let batches = ctx
        .sql("SELECT * FROM t ORDER BY a")
        .await
        .expect("select")
        .collect()
        .await
        .expect("collect select");

    let rows: usize = batches.iter().map(|b| b.num_rows()).sum();
    assert_eq!(rows, 2);
}
