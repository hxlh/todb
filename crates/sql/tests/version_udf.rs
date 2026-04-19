use sql::{create_session_context, udf::VersionInfo, udf::register_version_udf};

#[tokio::test]
async fn test_version_udf_returns_single_value() {
    let ctx = create_session_context();
    register_version_udf(
        &ctx,
        VersionInfo {
            commit_short: "abc1234".to_string(),
            build_time: "20260418215711".to_string(),
        },
    )
    .expect("register version udf");

    let dataframe = ctx
        .sql("select version() as version")
        .await
        .expect("build sql");
    let batches = dataframe.collect().await.expect("collect batches");

    assert_eq!(batches.len(), 1);
    let batch = &batches[0];
    assert_eq!(batch.num_columns(), 1);
    assert_eq!(batch.num_rows(), 1);
    assert_eq!(batch.schema().field(0).name(), "version");

    let rendered = format!("{:?}", batch.column(0));
    assert!(rendered.contains("abc1234-20260418215711"));
}
