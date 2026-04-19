use std::sync::Arc;

use datafusion::arrow::array::{Int64Array, StringArray};
use datafusion::arrow::datatypes::{DataType, Field, Schema};
use datafusion::arrow::record_batch::RecordBatch;
use server::{CatalogManager, ClientSession, EngineState, TableStore};

fn demo_schema() -> Arc<Schema> {
    Arc::new(Schema::new(vec![
        Field::new("name", DataType::Utf8, false),
        Field::new("age", DataType::Int64, false),
    ]))
}

#[test]
fn exports_new_server_building_blocks() {
    let _ = std::any::type_name::<CatalogManager>();
    let _ = std::any::type_name::<TableStore>();
    let _ = std::any::type_name::<ClientSession>();
    let _ = std::any::type_name::<EngineState>();
}

#[test]
fn create_and_drop_table_updates_catalog() {
    let catalog = CatalogManager::new();
    catalog
        .create_table("default", "people", demo_schema())
        .expect("create table");

    let table = catalog.table("default", "people").expect("table exists");
    assert_eq!(table.name, "people");

    catalog.drop_table("default", "people").expect("drop table");
    assert!(catalog.table("default", "people").is_none());
}

#[test]
fn append_rows_updates_table_store() {
    let store = TableStore::new();
    let schema = demo_schema();
    store
        .create_table("default.people", schema.clone())
        .expect("create store table");

    let batch = RecordBatch::try_new(
        schema,
        vec![
            Arc::new(StringArray::from(vec!["Alice"])),
            Arc::new(Int64Array::from(vec![30])),
        ],
    )
    .expect("build batch");

    store.append("default.people", batch).expect("append batch");
    let rows = store
        .table("default.people")
        .expect("table data")
        .row_count();
    assert_eq!(rows, 1);
}
