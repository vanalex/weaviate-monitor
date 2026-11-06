mod client;
mod http;

use std::net::SocketAddr;

use anyhow::Context;
use tracing_subscriber::EnvFilter;

const DEFAULT_BIND_ADDR: &str = "127.0.0.1:3000";

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| EnvFilter::new("weaviate_monitor=info,tower_http=info")),
        )
        .init();

    let addr: SocketAddr = std::env::var("BIND_ADDR")
        .unwrap_or_else(|_| DEFAULT_BIND_ADDR.to_string())
        .parse()
        .context("BIND_ADDR must be a socket address, e.g. 127.0.0.1:3000")?;

    let weaviate = client::WeaviateClient::from_env()?;
    tracing::info!(url = %weaviate.base_url(), "monitoring weaviate");

    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .with_context(|| format!("failed to bind {addr}"))?;
    tracing::info!("listening on http://{}", listener.local_addr()?);

    axum::serve(listener, http::router(http::AppState::new(weaviate)))
        .with_graceful_shutdown(shutdown_signal())
        .await?;

    tracing::info!("shut down");
    Ok(())
}

/// Resolves on Ctrl+C or SIGTERM (e.g. `docker stop`).
async fn shutdown_signal() {
    let ctrl_c = async {
        tokio::signal::ctrl_c()
            .await
            .expect("failed to install Ctrl+C handler");
    };

    #[cfg(unix)]
    let terminate = async {
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("failed to install SIGTERM handler")
            .recv()
            .await;
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => {},
        _ = terminate => {},
    }
}
