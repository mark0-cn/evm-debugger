use crate::{
    executor::spawn_evm_thread,
    fetcher::fetch_tx_info,
    session::DebugSession,
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

    let tx_info = match fetch_tx_info(&req.tx_hash, &req.rpc_url).await {
        Ok(info) => info,
        Err(e) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({ "error": e.to_string() })),
            )
                .into_response();
        }
    };

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
