use std::path::PathBuf;

use datafusion::arrow::util::display::array_value_to_string;
use sql::{catalog::register_system_catalog, create_session_context};

fn system_table_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../bootstrap/system_tables.yaml")
}

#[tokio::test]
async fn test_session_context_can_query_system_types() {
    let ctx = create_session_context();
    register_system_catalog(&ctx, &system_table_dir()).expect("register system catalog");

    let dataframe = ctx
        .sql("select type_name, category from system.types order by type_id limit 1")
        .await
        .expect("build sql");
    let batches = dataframe.collect().await.expect("collect batches");

    assert_eq!(batches.len(), 1);
    let batch = &batches[0];
    assert_eq!(batch.num_rows(), 1);
    assert_eq!(batch.schema().field(0).name(), "type_name");
    assert_eq!(batch.schema().field(1).name(), "category");
    assert_eq!(
        array_value_to_string(batch.column(0).as_ref(), 0).expect("render type_name"),
        "Boolean"
    );
    assert_eq!(
        array_value_to_string(batch.column(1).as_ref(), 0).expect("render category"),
        "boolean"
    );
}
