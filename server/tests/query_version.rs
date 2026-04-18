use reqwest::StatusCode;
use serde_json::json;

#[tokio::test]
async fn post_query_returns_version_json() {
    let (address, _handle) = server::start_for_test().await.expect("start test server");

    let client = reqwest::Client::new();
    let response = client
        .post(format!("http://{address}/query"))
        .json(&json!({ "sql": "select version() as version" }))
        .send()
        .await
        .expect("send request");

    assert_eq!(response.status(), StatusCode::OK);
    let body: serde_json::Value = response.json().await.expect("read json");
    assert_eq!(
        body["schema"],
        json!([
            {
                "name": "version",
                "type": "Utf8"
            }
        ])
    );
    assert_eq!(body["data"].as_array().unwrap().len(), 1);
    assert!(body["data"][0]["version"].as_str().unwrap().contains('-'));
}

#[tokio::test]
async fn post_query_supports_general_output_shape() {
    let (address, _handle) = server::start_for_test().await.expect("start test server");

    let client = reqwest::Client::new();
    let response = client
        .post(format!("http://{address}/query"))
        .json(&json!({ "sql": "select 'Alice' as name, 30 as age" }))
        .send()
        .await
        .expect("send request");

    assert_eq!(response.status(), StatusCode::OK);
    let body: serde_json::Value = response.json().await.expect("read json");
    assert_eq!(
        body["schema"],
        json!([
            {
                "name": "name",
                "type": "Utf8"
            },
            {
                "name": "age",
                "type": "Int64"
            }
        ])
    );
    assert_eq!(body["data"], json!([{ "name": "Alice", "age": 30 }]));
}

#[tokio::test]
async fn post_query_rejects_invalid_json() {
    let (address, _handle) = server::start_for_test().await.expect("start test server");

    let client = reqwest::Client::new();
    let response = client
        .post(format!("http://{address}/query"))
        .header("content-type", "application/json")
        .body("{not-json}")
        .send()
        .await
        .expect("send request");

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let body: serde_json::Value = response.json().await.expect("read json");
    assert_eq!(body["error"], json!("invalid request"));
}
