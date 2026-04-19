use server::{ClientSession, EngineState, QueryExecutor};
use server::executor::ExecutionResult;
use server::version::BuildVersion;

fn test_engine() -> EngineState {
    EngineState::new(BuildVersion {
        commit_short: "test0000".to_string(),
        build_time: "20260419000000".to_string(),
    })
    .expect("engine state")
}

#[tokio::test]
async fn create_table_registers_in_catalog_and_store() {
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

    assert!(engine.catalog.table("default", "t").is_some());
    assert!(engine.store.table("default.t").is_some());
}

#[tokio::test]
async fn insert_then_select_roundtrip() {
    let engine = test_engine();
    let session = ClientSession::new("default");

    QueryExecutor::execute(&engine, &session, "CREATE TABLE people(name VARCHAR, age INT)")
        .await
        .expect("create table");

    let result = QueryExecutor::execute(
        &engine,
        &session,
        "INSERT INTO people VALUES ('Alice', 30)",
    )
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
async fn drop_table_removes_from_catalog_and_store() {
    let engine = test_engine();
    let session = ClientSession::new("default");

    QueryExecutor::execute(&engine, &session, "CREATE TABLE tmp(x INT)")
        .await
        .expect("create");

    QueryExecutor::execute(&engine, &session, "DROP TABLE tmp")
        .await
        .expect("drop");

    assert!(engine.catalog.table("default", "tmp").is_none());
    assert!(engine.store.table("default.tmp").is_none());
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
