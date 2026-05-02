use datafusion::arrow::util::display::array_value_to_string;
use server::executor::ExecutionResult;
use server::version::BuildVersion;
use server::{ClientSession, EngineState, QueryExecutor, TodbTableProvider};

fn test_engine() -> EngineState {
    EngineState::new(BuildVersion {
        commit_short: "test0000".to_string(),
        build_time: "20260419000000".to_string(),
    })
    .expect("engine state")
}

#[tokio::test]
async fn create_table_registers_in_shared_schema_provider() {
    let engine = test_engine();
    let session = ClientSession::new("default");

    let result = QueryExecutor::execute(&engine, &session, "CREATE TABLE t(a INT)")
        .await
        .expect("execute create table");

    match result {
        ExecutionResult::CommandComplete { tag } => {
            assert_eq!(tag, "CREATE TABLE");
        }
        _ => panic!("expected CommandComplete for DDL"),
    }

    let table = engine
        .schema_provider()
        .table("t")
        .await
        .expect("lookup table")
        .expect("table exists");
    assert!(table.as_any().is::<TodbTableProvider>());
}

#[tokio::test]
async fn insert_then_select_roundtrip() {
    let engine = test_engine();
    let session = ClientSession::new("default");

    QueryExecutor::execute(
        &engine,
        &session,
        "CREATE TABLE people(name VARCHAR, age INT)",
    )
    .await
    .expect("create table");

    let result =
        QueryExecutor::execute(&engine, &session, "INSERT INTO people VALUES ('Alice', 30)")
            .await
            .expect("insert");

    match result {
        ExecutionResult::AffectedRows { rows } => {
            assert_eq!(rows, 1);
        }
        _ => panic!("expected AffectedRows for INSERT"),
    }

    let result = QueryExecutor::execute(&engine, &session, "SELECT * FROM people")
        .await
        .expect("select");

    match result {
        ExecutionResult::Query { batches, schema } => {
            assert_eq!(schema.fields().len(), 2);
            let total_rows: usize = batches.iter().map(|b| b.num_rows()).sum();
            assert_eq!(total_rows, 1);
        }
        _ => panic!("expected Query for SELECT"),
    }
}

#[tokio::test]
async fn drop_table_removes_from_shared_schema_provider() {
    let engine = test_engine();
    let session = ClientSession::new("default");

    QueryExecutor::execute(&engine, &session, "CREATE TABLE tmp(x INT)")
        .await
        .expect("create");

    QueryExecutor::execute(&engine, &session, "DROP TABLE tmp")
        .await
        .expect("drop");

    let found = engine.schema_provider().table("tmp").await.expect("lookup");
    assert!(found.is_none());
}

#[tokio::test]
async fn select_version_works() {
    let engine = test_engine();
    let session = ClientSession::new("default");

    let result = QueryExecutor::execute(&engine, &session, "SELECT version()")
        .await
        .expect("select version");

    match result {
        ExecutionResult::Query { schema, batches } => {
            assert_eq!(schema.fields().len(), 1);
            assert_eq!(batches.len(), 1);
            assert_eq!(batches[0].num_rows(), 1);
        }
        _ => panic!("expected Query for SELECT version()"),
    }
}

#[tokio::test]
async fn select_system_types_works() {
    let engine = test_engine();
    let session = ClientSession::new("default");

    let result = QueryExecutor::execute(
        &engine,
        &session,
        "SELECT type_name, category FROM types ORDER BY type_id LIMIT 1",
    )
    .await
    .expect("select system table");

    match result {
        ExecutionResult::Query { schema, batches } => {
            assert_eq!(schema.fields().len(), 2);
            assert_eq!(batches.len(), 1);
            assert_eq!(batches[0].num_rows(), 1);
            assert_eq!(
                array_value_to_string(batches[0].column(0).as_ref(), 0).expect("type_name"),
                "Boolean"
            );
            assert_eq!(
                array_value_to_string(batches[0].column(1).as_ref(), 0).expect("category"),
                "boolean"
            );
        }
        _ => panic!("expected Query for system table select"),
    }
}

#[tokio::test]
async fn set_time_zone_returns_command_complete() {
    let engine = test_engine();
    let session = ClientSession::new("default");

    let result = QueryExecutor::execute(
        &engine,
        &session,
        r#"SET TIME ZONE "Asia/Shanghai""#,
    )
    .await
    .expect("execute set time zone");

    match result {
        ExecutionResult::CommandComplete { tag } => {
            assert_eq!(tag, "SET");
        }
        _ => panic!("expected CommandComplete for SET"),
    }
}

#[tokio::test]
async fn information_schema_lists_user_tables() {
    let engine = test_engine();
    let session = ClientSession::new("default");

    QueryExecutor::execute(&engine, &session, "CREATE TABLE info_demo(x INT)")
        .await
        .expect("create info_demo");

    let result = QueryExecutor::execute(
        &engine,
        &session,
        "SELECT table_name FROM information_schema.tables WHERE table_schema = 'public' AND table_name = 'info_demo'",
    )
    .await
    .expect("query information_schema");

    match result {
        ExecutionResult::Query { schema, batches } => {
            assert_eq!(schema.fields().len(), 1);
            let total_rows: usize = batches.iter().map(|b| b.num_rows()).sum();
            assert_eq!(total_rows, 1);
        }
        _ => panic!("expected Query for information_schema"),
    }
}
