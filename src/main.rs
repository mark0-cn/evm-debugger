mod executor;
mod fetcher;
mod inspector;
mod server;
mod session;
mod trace_cache;
mod types;

use dashmap::DashMap;
use server::{router, AppState, SessionMap};
use std::sync::Arc;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    std::env::set_var("https_proxy", "http://127.0.0.1:7890");
    std::env::set_var("http_proxy", "http://127.0.0.1:7890");
    std::env::set_var("all_proxy", "socks5://127.0.0.1:7890");
    std::env::set_var("HTTPS_PROXY", "http://127.0.0.1:7890");
    std::env::set_var("HTTP_PROXY", "http://127.0.0.1:7890");
    std::env::set_var("ALL_PROXY", "socks5://127.0.0.1:7890");

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
