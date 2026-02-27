mod executor;
mod fetcher;
mod inspector;
mod server;
mod session;
mod types;

use server::{router, AppState, SessionMap};
use std::sync::Arc;
use dashmap::DashMap;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let sessions: SessionMap = Arc::new(DashMap::new());
    let state = AppState { sessions };

    let app = router(state);

    let listener = tokio::net::TcpListener::bind("0.0.0.0:8080").await?;
    tracing::info!("EVM Debugger listening on http://0.0.0.0:8080");

    axum::serve(listener, app).await?;
    Ok(())
}
