use crate::{app_state::AppState, session_service::SessionService, types::CreateSessionRequest};
use axum::{
    extract::{Path, Request, State},
    http::StatusCode,
    http::{header::CONTENT_TYPE, HeaderValue, Method},
    response::Html,
    response::IntoResponse,
    routing::{get, post},
    Json, Router,
};
use dioxus::prelude::VirtualDom;
use dioxus_liveview::{interpreter_glue, LiveviewRouter};
use serde_json::json;
use tower_http::cors::{AllowOrigin, CorsLayer};
use tower_http::trace::TraceLayer;

async fn liveview_index() -> Html<String> {
    let glue = interpreter_glue("/ws");
    Html(format!(
        r#"<!DOCTYPE html><html><head><title>EVM Debugger</title></head><body><div id="main"></div>{glue}</body></html>"#
    ))
}

fn format_anyhow_chain(e: &anyhow::Error) -> String {
    let mut msg = e.to_string();
    for cause in e.chain().skip(1) {
        msg.push_str("; caused by: ");
        msg.push_str(&cause.to_string());
    }
    msg
}

pub fn router(state: AppState) -> Router {
    let cors = cors_layer();
    let ui_ctx = crate::ui::UiContext {
        sessions: state.sessions.clone(),
        evm_semaphore: state.evm_semaphore.clone(),
    };
    Router::new()
        .route("/", get(liveview_index))
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
        .with_virtual_dom("/", move || {
            VirtualDom::new(crate::ui::app).with_root_context(ui_ctx.clone())
        })
}

fn cors_layer() -> CorsLayer {
    let origins = std::env::var("EVM_DEBUGGER_CORS_ALLOW_ORIGINS")
        .unwrap_or_else(|_| "http://localhost:8080,http://127.0.0.1:8080".to_string());

    let list: Vec<HeaderValue> = origins
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .filter_map(|s| s.parse::<HeaderValue>().ok())
        .collect();

    let allow_origin = if list.is_empty() {
        AllowOrigin::exact(HeaderValue::from_static("http://localhost:8080"))
    } else {
        AllowOrigin::list(list)
    };

    CorsLayer::new()
        .allow_origin(allow_origin)
        .allow_methods([Method::GET, Method::POST])
        .allow_headers([CONTENT_TYPE])
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

#[cfg(test)]
mod tests {
    use super::router;
    use crate::{
        app_state::{AppState, SessionMap},
        session::DebugSession,
        types::StepSnapshot,
    };
    use dashmap::DashMap;
    use http_body_util::BodyExt;
    use std::collections::HashMap;
    use std::sync::Arc;
    use tokio::sync::Semaphore;
    use tower::ServiceExt;

    fn dummy_snapshot(step: u64) -> StepSnapshot {
        StepSnapshot {
            step_number: step,
            pc: 0,
            opcode: 0,
            opcode_name: "STOP".to_string(),
            call_depth: 0,
            gas_remaining: 0,
            gas_used: 0,
            stack: vec![],
            memory_size: 0,
            memory_hex: String::new(),
            memory_truncated: false,
            storage_changes: HashMap::new(),
            call_stack: vec![],
            logs: vec![],
            contract_address: "0x0000000000000000000000000000000000000000".to_string(),
        }
    }

    #[tokio::test]
    async fn get_session_404() {
        let sessions: SessionMap = Arc::new(DashMap::new());
        let app = router(AppState {
            sessions,
            evm_semaphore: Arc::new(Semaphore::new(1)),
        });
        let res = app
            .oneshot(
                axum::http::Request::builder()
                    .uri("/api/session/nope")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), axum::http::StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn trace_steps_returns_json_array() {
        let sessions: SessionMap = Arc::new(DashMap::new());
        let session = DebugSession::from_cache(vec![dummy_snapshot(0)], None);
        sessions.insert("s1".to_string(), session);
        let app = router(AppState {
            sessions,
            evm_semaphore: Arc::new(Semaphore::new(1)),
        });
        let res = app
            .oneshot(
                axum::http::Request::builder()
                    .uri("/api/session/s1/trace_steps")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), axum::http::StatusCode::OK);
        let body = res.into_body().collect().await.unwrap().to_bytes();
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert!(v.is_array());
        assert_eq!(v.as_array().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn cors_allows_localhost_origin_by_default() {
        let sessions: SessionMap = Arc::new(DashMap::new());
        let app = router(AppState {
            sessions,
            evm_semaphore: Arc::new(Semaphore::new(1)),
        });
        let res = app
            .oneshot(
                axum::http::Request::builder()
                    .uri("/api/session/nope")
                    .header(axum::http::header::ORIGIN, "http://localhost:8080")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), axum::http::StatusCode::NOT_FOUND);
        assert_eq!(
            res.headers()
                .get(axum::http::header::ACCESS_CONTROL_ALLOW_ORIGIN)
                .unwrap(),
            "http://localhost:8080"
        );
    }

    #[tokio::test]
    async fn cors_does_not_allow_random_origin_by_default() {
        let sessions: SessionMap = Arc::new(DashMap::new());
        let app = router(AppState {
            sessions,
            evm_semaphore: Arc::new(Semaphore::new(1)),
        });
        let res = app
            .oneshot(
                axum::http::Request::builder()
                    .uri("/api/session/nope")
                    .header(axum::http::header::ORIGIN, "http://evil.example")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), axum::http::StatusCode::NOT_FOUND);
        assert!(res
            .headers()
            .get(axum::http::header::ACCESS_CONTROL_ALLOW_ORIGIN)
            .is_none());
    }
}
