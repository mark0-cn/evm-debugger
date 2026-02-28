use crate::{app_state::AppState, session_service::SessionService, types::CreateSessionRequest};
use axum::{
    extract::{Path, Request, State},
    http::StatusCode,
    response::IntoResponse,
    routing::{get, post},
    Json, Router,
};
use serde_json::json;
use tower_http::cors::{Any, CorsLayer};
use tower_http::trace::TraceLayer;

fn format_anyhow_chain(e: &anyhow::Error) -> String {
    let mut msg = e.to_string();
    for cause in e.chain().skip(1) {
        msg.push_str("; caused by: ");
        msg.push_str(&cause.to_string());
    }
    msg
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
        .route("/api/session/:id/trace_steps", get(get_trace_steps))
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
    let svc = SessionService::new(state.sessions.clone(), state.evm_semaphore.clone());
    match svc.create_session(req).await {
        Ok(resp) => Json(resp).into_response(),
        Err(e) => {
            tracing::warn!(error = ?e, "[create_session] failed");
            (
                StatusCode::BAD_REQUEST,
                Json(json!({ "error": format_anyhow_chain(&e) })),
            )
                .into_response()
        }
    }
}

async fn get_session(Path(id): Path<String>, State(state): State<AppState>) -> impl IntoResponse {
    match state.sessions.get(&id) {
        Some(session) => {
            session.touch();
            Json(session.current_state()).into_response()
        }
        None => (
            StatusCode::NOT_FOUND,
            Json(json!({ "error": "session not found" })),
        )
            .into_response(),
    }
}

async fn get_trace_steps(
    Path(id): Path<String>,
    State(state): State<AppState>,
) -> impl IntoResponse {
    match state.sessions.get(&id) {
        Some(session) => {
            session.touch();
            Json(session.get_trace_steps()).into_response()
        }
        None => (
            StatusCode::NOT_FOUND,
            Json(json!({ "error": "session not found" })),
        )
            .into_response(),
    }
}

async fn step_into(Path(id): Path<String>, State(state): State<AppState>) -> impl IntoResponse {
    let session = match state.sessions.get(&id) {
        Some(s) => {
            s.value().touch();
            s.value().clone()
        }
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
        Some(s) => {
            s.value().touch();
            s.value().clone()
        }
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
        Some(s) => {
            s.value().touch();
            s.value().clone()
        }
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
        Some(s) => {
            s.value().touch();
            s.value().clone()
        }
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
