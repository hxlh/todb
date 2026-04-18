#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let router = server::build_router()?;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:3000").await?;
    axum::serve(listener, router).await?;
    Ok(())
}
