mod app_state;
mod deps;
mod executor;
mod fetcher;
mod fs_utils;
mod inspector;
mod rpc_url;
mod server;
mod session;
mod session_service;
mod trace_cache;
mod types;

use app_state::{AppState, SessionMap};
use dashmap::DashMap;
use server::router;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::sync::Semaphore;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let _ = dotenvy::dotenv();

    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let sessions: SessionMap = Arc::new(DashMap::new());
    tokio::spawn(session_gc_task(sessions.clone()));
    let evm_concurrency = std::env::var("EVM_DEBUGGER_EVM_CONCURRENCY")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(2);
    let state = AppState {
        sessions,
        evm_semaphore: Arc::new(Semaphore::new(evm_concurrency)),
    };

    let app = router(state);

    let bind_addr =
        std::env::var("EVM_DEBUGGER_BIND_ADDR").unwrap_or_else(|_| "127.0.0.1:8080".to_string());
    let listener = tokio::net::TcpListener::bind(&bind_addr).await?;
    tracing::info!("EVM Debugger listening on http://{}", bind_addr);

    axum::serve(listener, app).await?;
    Ok(())
}

async fn session_gc_task(sessions: SessionMap) {
    let ttl_secs = std::env::var("EVM_DEBUGGER_SESSION_TTL_SECS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(1800);
    let cache_ttl_secs = std::env::var("EVM_DEBUGGER_CACHE_TTL_SECS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(7 * 24 * 60 * 60);

    let mut tick: u64 = 0;
    loop {
        tokio::time::sleep(Duration::from_secs(60)).await;

        let now_secs = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        let mut remove_keys = Vec::new();
        for entry in sessions.iter() {
            let last = entry.value().last_access_secs();
            if now_secs.saturating_sub(last) > ttl_secs {
                remove_keys.push(entry.key().clone());
            }
        }
        for k in remove_keys {
            sessions.remove(&k);
        }

        tick = tick.wrapping_add(1);
        if tick.is_multiple_of(10) {
            let _ = tokio::task::spawn_blocking(move || cleanup_cache_dir(cache_ttl_secs)).await;
        }
    }
}

fn cleanup_cache_dir(ttl_secs: u64) -> anyhow::Result<usize> {
    let dir = std::path::Path::new("cache");
    if !dir.exists() {
        return Ok(0);
    }
    let now = SystemTime::now();
    let mut removed = 0usize;
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let Ok(meta) = entry.metadata() else {
            continue;
        };
        let Ok(modified) = meta.modified() else {
            continue;
        };
        let age = now.duration_since(modified).unwrap_or_default().as_secs();
        if age > ttl_secs && std::fs::remove_file(&path).is_ok() {
            removed += 1;
        }
    }
    Ok(removed)
}
