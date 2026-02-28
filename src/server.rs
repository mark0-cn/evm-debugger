use crate::{
    executor::spawn_evm_thread,
    fetcher::fetch_tx_info,
    session::DebugSession,
    trace_cache::{load_trace_cache, save_trace_cache, trace_cache_path},
    types::{CreateSessionRequest, CreateSessionResponse, SessionState},
};
use axum::{
    extract::{Path, Request, State},
    http::StatusCode,
    response::IntoResponse,
    routing::{get, post},
    Json, Router,
};
use dashmap::DashMap;
use serde_json::json;
use std::sync::{atomic::AtomicBool, Arc};
use tower_http::cors::{Any, CorsLayer};
use tower_http::trace::TraceLayer;

pub type SessionMap = Arc<DashMap<String, Arc<DebugSession>>>;

fn format_anyhow_chain(e: &anyhow::Error) -> String {
    let mut msg = e.to_string();
    for cause in e.chain().skip(1) {
        msg.push_str("; caused by: ");
        msg.push_str(&cause.to_string());
    }
    msg
}

#[derive(Clone)]
pub struct AppState {
    pub sessions: SessionMap,
}

pub fn router(state: AppState) -> Router {
    let cors = CorsLayer::new()
        .allow_origin(Any)
        .allow_methods(Any)
        .allow_headers(Any);

    Router::new()
        .route("/", get(serve_index))
        .route("/api/session", post(create_session))
        .route("/api/session/:id", get(get_session))
        .route("/api/session/:id/step_into", post(step_into))
        .route("/api/session/:id/step_over", post(step_over))
        .route("/api/session/:id/continue", post(continue_exec))
        .route("/api/session/:id/abort", post(abort_session))
        .fallback(fallback_handler)
        .layer(TraceLayer::new_for_http())
        .layer(cors)
        .with_state(state)
}

async fn fallback_handler(req: Request) -> impl IntoResponse {
    let method = req.method().to_string();
    let path = req.uri().path().to_string();
    tracing::warn!("[fallback] no route matched: {} {}", method, path);
    (
        StatusCode::NOT_FOUND,
        Json(json!({ "error": "route not found", "method": method, "path": path })),
    )
}

async fn serve_index() -> impl IntoResponse {
    let html = include_str!("../static/index.html");
    axum::response::Html(html)
}

async fn create_session(
    State(state): State<AppState>,
    Json(req): Json<CreateSessionRequest>,
) -> impl IntoResponse {
    let session_id = uuid::Uuid::new_v4().to_string();

    let trimmed = req.tx_hash.trim();
    let canonical_hash = match trimmed.parse::<alloy_primitives::B256>() {
        Ok(h) => format!("{h:#x}"),
        Err(_) => {
            let stripped = trimmed
                .strip_prefix("0x")
                .or_else(|| trimmed.strip_prefix("0X"))
                .unwrap_or(trimmed);
            let with_prefix = format!("0x{}", stripped);
            match with_prefix.parse::<alloy_primitives::B256>() {
                Ok(h) => format!("{h:#x}"),
                Err(e) => {
                    return (
                        StatusCode::BAD_REQUEST,
                        Json(json!({ "error": format!("invalid tx hash: {}", e) })),
                    )
                        .into_response();
                }
            }
        }
    };

    let tx_info = match fetch_tx_info(&canonical_hash, &req.rpc_url).await {
        Ok(info) => info,
        Err(e) => {
            tracing::warn!(tx_hash = %canonical_hash, error = ?e, "[create_session] fetch_tx_info failed");
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({ "error": format_anyhow_chain(&e) })),
            )
                .into_response();
        }
    };

    let trace_cache_path = trace_cache_path(&canonical_hash, tx_info.chain_id, tx_info.block_number);
    if let Ok(Some(file)) = load_trace_cache(&trace_cache_path) {
        let session = DebugSession::from_cache(file.snapshots, file.result);
        state.sessions.insert(session_id.clone(), session.clone());
        let initial_state = session.current_state();
        let trace_steps = session.get_trace_steps();
        return Json(CreateSessionResponse {
            session_id,
            state: initial_state,
            trace_steps,
        })
        .into_response();
    }

    let (snap_tx, snap_rx) = std::sync::mpsc::sync_channel::<crate::types::ChannelMessage>(1);
    let abort_flag = Arc::new(AtomicBool::new(false));

    let session = DebugSession::new(snap_rx, abort_flag.clone());
    state.sessions.insert(session_id.clone(), session.clone());

    spawn_evm_thread(tx_info, req.rpc_url, snap_tx, abort_flag);

    // Block until the EVM thread finishes and sends all snapshots.
    let initial_state = tokio::task::spawn_blocking(move || session.wait_for_snapshots())
        .await
        .unwrap_or(SessionState::Error {
            message: "Spawn blocking failed".to_string(),
        });

    if let Some((snapshots, result)) = match state.sessions.get(&session_id) {
        Some(s) => s.value().snapshots_for_cache(),
        None => None,
    } {
        let _ = save_trace_cache(&trace_cache_path, &snapshots, &result);
    }

    // Extract lightweight trace for the opcode list.
    let trace_steps = match state.sessions.get(&session_id) {
        Some(s) => s.get_trace_steps(),
        None => vec![],
    };

    Json(CreateSessionResponse {
        session_id,
        state: initial_state,
        trace_steps,
    })
    .into_response()
}

async fn get_session(Path(id): Path<String>, State(state): State<AppState>) -> impl IntoResponse {
    match state.sessions.get(&id) {
        Some(session) => Json(session.current_state()).into_response(),
        None => (
            StatusCode::NOT_FOUND,
            Json(json!({ "error": "session not found" })),
        )
            .into_response(),
    }
}

async fn step_into(Path(id): Path<String>, State(state): State<AppState>) -> impl IntoResponse {
    let session = match state.sessions.get(&id) {
        Some(s) => s.value().clone(),
        None => {
            return (
                StatusCode::NOT_FOUND,
                Json(json!({ "error": "session not found" })),
            )
                .into_response()
        }
    };
    match tokio::task::spawn_blocking(move || session.step_into()).await {
        Ok(s) => Json(s).into_response(),
        Err(_) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": "internal error" })),
        )
            .into_response(),
    }
}

async fn step_over(Path(id): Path<String>, State(state): State<AppState>) -> impl IntoResponse {
    let session = match state.sessions.get(&id) {
        Some(s) => s.value().clone(),
        None => {
            tracing::warn!("[step_over] session not found: {}", id);
            return (
                StatusCode::NOT_FOUND,
                Json(json!({ "error": "session not found" })),
            )
                .into_response();
        }
    };
    match tokio::task::spawn_blocking(move || session.step_over()).await {
        Ok(s) => Json(s).into_response(),
        Err(e) => {
            tracing::error!("[step_over] spawn_blocking error (panic?): {}", e);
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "error": "internal error" })),
            )
                .into_response()
        }
    }
}

async fn continue_exec(Path(id): Path<String>, State(state): State<AppState>) -> impl IntoResponse {
    let session = match state.sessions.get(&id) {
        Some(s) => s.value().clone(),
        None => {
            return (
                StatusCode::NOT_FOUND,
                Json(json!({ "error": "session not found" })),
            )
                .into_response()
        }
    };
    match tokio::task::spawn_blocking(move || session.continue_exec()).await {
        Ok(s) => Json(s).into_response(),
        Err(_) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": "internal error" })),
        )
            .into_response(),
    }
}

async fn abort_session(Path(id): Path<String>, State(state): State<AppState>) -> impl IntoResponse {
    let session = match state.sessions.get(&id) {
        Some(s) => s.value().clone(),
        None => {
            return (
                StatusCode::NOT_FOUND,
                Json(json!({ "error": "session not found" })),
            )
                .into_response();
        }
    };
    session.abort();
    Json(json!({ "status": "aborted" })).into_response()
}
