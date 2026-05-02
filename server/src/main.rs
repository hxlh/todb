use std::sync::Arc;

use server::config::ServerConfig;
use server::engine::EngineState;
use server::pgwire::TodbHandlers;
use server::version::current_build_version;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let config = ServerConfig::default();
    let build_version = current_build_version();
    let engine = Arc::new(EngineState::new(build_version)?);
    let handlers = Arc::new(TodbHandlers::new(engine));

    let listener = tokio::net::TcpListener::bind(&config.listen_addr).await?;
    eprintln!("todb listening on {}", config.listen_addr);

    loop {
        let (socket, _) = listener.accept().await?;
        let handlers = handlers.clone();
        tokio::spawn(async move {
            let _ = pgwire::tokio::process_socket(socket, None, handlers).await;
        });
    }
}
