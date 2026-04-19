use std::{
    net::SocketAddr,
    path::{Path, PathBuf},
    sync::Arc,
};

pub mod http;
pub mod query;
pub mod version;

use anyhow::Result;
use axum::{Router, routing::post};
use http::AppState;
use query::QueryEngine;
use version::current_build_version;

pub fn default_system_table_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("bootstrap/system_tables.yaml")
}

pub fn app_state() -> Result<AppState> {
    app_state_with_dir(&default_system_table_dir())
}

pub fn app_state_with_dir(system_table_dir: &Path) -> Result<AppState> {
    let version = current_build_version();
    let engine = QueryEngine::new(version, system_table_dir)?;
    Ok(AppState {
        engine: Arc::new(engine),
    })
}

pub fn build_router() -> Result<Router> {
    let state = app_state()?;
    Ok(Router::new()
        .route("/query", post(http::query_handler))
        .with_state(state))
}

pub async fn start_for_test() -> Result<(SocketAddr, tokio::task::JoinHandle<()>)> {
    start_for_test_with_dir(&default_system_table_dir()).await
}

pub async fn start_for_test_with_dir(
    system_table_dir: &Path,
) -> Result<(SocketAddr, tokio::task::JoinHandle<()>)> {
    let state = app_state_with_dir(system_table_dir)?;
    let router = Router::new()
        .route("/query", post(http::query_handler))
        .with_state(state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let handle = tokio::spawn(async move {
        axum::serve(listener, router).await.expect("serve test app");
    });
    Ok((address, handle))
}
