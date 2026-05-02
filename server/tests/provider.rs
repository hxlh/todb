use std::sync::Arc;

use datafusion::arrow::array::{Int32Array, StringArray};
use datafusion::arrow::datatypes::{DataType, Field, Schema};
use datafusion::arrow::record_batch::RecordBatch;
use datafusion::catalog::TableProvider;
use datafusion::execution::context::SessionContext;
use datafusion::logical_expr::TableType;
use server::TodbTableProvider;

fn test_schema() -> Arc<Schema> {
    Arc::new(Schema::new(vec![
        Field::new("name", DataType::Utf8, false),
        Field::new("age", DataType::Int32, false),
    ]))
}

fn make_batch(names: Vec<&str>, ages: Vec<i32>) -> RecordBatch {
    RecordBatch::try_new(
        test_schema(),
        vec![
            Arc::new(StringArray::from(names)),
            Arc::new(Int32Array::from(ages)),
        ],
    )
    .expect("build batch")
}

#[tokio::test]
async fn empty_table_scan_returns_zero_rows() {
    let provider = TodbTableProvider::new(test_schema());
    assert_eq!(provider.row_count(), 0);
    assert_eq!(provider.table_type(), TableType::Base);

    let ctx = SessionContext::new();
    ctx.register_table("t", Arc::new(provider))
        .expect("register");

    let df = ctx.sql("SELECT * FROM t").await.expect("query");
    let batches = df.collect().await.expect("collect");
    let rows: usize = batches.iter().map(|b| b.num_rows()).sum();
    assert_eq!(rows, 0);
}

#[tokio::test]
async fn insert_into_appends_batches() {
    let provider = TodbTableProvider::new(test_schema());
    let ctx = SessionContext::new();
    ctx.register_table("people", Arc::new(provider))
        .expect("register");

    ctx.sql("INSERT INTO people VALUES ('Alice', 30)")
        .await
        .expect("insert")
        .collect()
        .await
        .expect("collect insert");

    let df = ctx.sql("SELECT * FROM people").await.expect("select");
    let batches = df.collect().await.expect("collect select");
    let rows: usize = batches.iter().map(|b| b.num_rows()).sum();
    assert_eq!(rows, 1);
}

#[tokio::test]
async fn multiple_inserts_preserve_all_data() {
    let provider = TodbTableProvider::new(test_schema());
    let ctx = SessionContext::new();
    ctx.register_table("t", Arc::new(provider))
        .expect("register");

    ctx.sql("INSERT INTO t VALUES ('a', 1)")
        .await
        .expect("insert 1")
        .collect()
        .await
        .expect("collect 1");

    ctx.sql("INSERT INTO t VALUES ('b', 2)")
        .await
        .expect("insert 2")
        .collect()
        .await
        .expect("collect 2");

    ctx.sql("INSERT INTO t VALUES ('c', 3)")
        .await
        .expect("insert 3")
        .collect()
        .await
        .expect("collect 3");

    let df = ctx.sql("SELECT * FROM t").await.expect("select");
    let batches = df.collect().await.expect("collect");
    let rows: usize = batches.iter().map(|b| b.num_rows()).sum();
    assert_eq!(rows, 3);
}

#[tokio::test]
async fn pre_loaded_data_is_readable() {
    let batch = make_batch(vec!["Bob"], vec![25]);
    let provider = TodbTableProvider::new_with_data(test_schema(), vec![batch]);
    assert_eq!(provider.row_count(), 1);

    let ctx = SessionContext::new();
    ctx.register_table("t", Arc::new(provider))
        .expect("register");

    let df = ctx.sql("SELECT * FROM t").await.expect("select");
    let batches = df.collect().await.expect("collect");
    let rows: usize = batches.iter().map(|b| b.num_rows()).sum();
    assert_eq!(rows, 1);
}

#[tokio::test]
async fn projection_pushdown() {
    let batch = make_batch(vec!["Alice", "Bob"], vec![30, 25]);
    let provider = TodbTableProvider::new_with_data(test_schema(), vec![batch]);

    let ctx = SessionContext::new();
    ctx.register_table("t", Arc::new(provider))
        .expect("register");

    let df = ctx.sql("SELECT name FROM t").await.expect("select");
    let batches = df.collect().await.expect("collect");
    assert_eq!(batches[0].num_columns(), 1);
    assert_eq!(batches[0].schema().field(0).name(), "name");
}
