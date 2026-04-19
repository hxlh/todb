#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let _ = server::config::ServerConfig::default();
    Ok(())
}
