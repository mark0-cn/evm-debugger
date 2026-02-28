use crate::{
    app_state::SessionMap,
    session_service::SessionService,
    types::{CreateSessionRequest, CreateSessionResponse, SessionState, TraceStep},
};
use dioxus::prelude::*;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Semaphore;

const CSS: &str = r#"
body { margin: 0; background: #1e1e1e; color: #d4d4d4; font-family: ui-monospace, SFMono-Regular, Menlo, Monaco, Consolas, "Liberation Mono", monospace; }
.toolbar { display: flex; gap: 8px; align-items: center; padding: 10px 12px; background: #2d2d2d; border-bottom: 1px solid #3d3d3d; }
input { background: #3c3c3c; border: 1px solid #555; color: #d4d4d4; padding: 6px 8px; border-radius: 4px; width: 360px; }
.rpc { width: 320px; }
button { background: #0e639c; color: #fff; border: none; padding: 6px 10px; border-radius: 4px; cursor: pointer; }
button.secondary { background: #444; }
button.danger { background: #a12626; }
button:disabled { opacity: 0.5; cursor: not-allowed; }
.grid { display: grid; grid-template-columns: 360px 1fr 420px; gap: 0; height: calc(100vh - 52px); }
.pane { border-right: 1px solid #3d3d3d; overflow: auto; }
.pane:last-child { border-right: none; }
.title { padding: 10px 12px; font-weight: 700; color: #569cd6; border-bottom: 1px solid #3d3d3d; }
.list { padding: 6px 0; }
.row { display: flex; gap: 8px; padding: 4px 12px; white-space: nowrap; }
.row.active { background: #094771; }
.muted { color: #9aa0a6; }
pre { margin: 0; padding: 10px 12px; white-space: pre-wrap; word-break: break-word; }
.error { color: #ff6b6b; padding: 8px 12px; }
"#;

#[derive(Clone)]
pub struct UiContext {
    pub sessions: SessionMap,
    pub evm_semaphore: Arc<Semaphore>,
}

pub fn app() -> Element {
    let ctx = use_context::<UiContext>();
    let svc = Arc::new(SessionService::new(
        ctx.sessions.clone(),
        ctx.evm_semaphore.clone(),
    ));

    let tx_hash = use_signal(|| "".to_string());
    let rpc_url = use_signal(|| "".to_string());
    let session_id = use_signal(|| None::<String>);
    let trace_steps = use_signal(Vec::<TraceStep>::new);
    let state = use_signal(|| SessionState::Loading);
    let loading = use_signal(|| false);
    let error = use_signal(|| None::<String>);

    let on_load = {
        let svc = svc.clone();
        let sessions = ctx.sessions.clone();
        move |_| {
            let svc = svc.clone();
            let tx_hash = tx_hash();
            let rpc_url = rpc_url();
            let mut loading = loading;
            let mut error = error;
            if tx_hash.trim().is_empty() || rpc_url.trim().is_empty() {
                error.set(Some("tx_hash 与 rpc_url 不能为空".to_string()));
                return;
            }
            loading.set(true);
            error.set(None);
            let sessions = sessions.clone();
            let session_id_for_async = session_id;
            let trace_steps_for_async = trace_steps;
            let state_for_async = state;
            let loading_for_async = loading;
            let error_for_async = error;
            spawn(async move {
                let mut session_id = session_id_for_async;
                let mut trace_steps = trace_steps_for_async;
                let mut state = state_for_async;
                let mut loading = loading_for_async;
                let mut error = error_for_async;
                let req = CreateSessionRequest { tx_hash, rpc_url };
                let resp: anyhow::Result<CreateSessionResponse> = svc.create_session(req).await;
                match resp {
                    Ok(resp) => {
                        session_id.set(Some(resp.session_id.clone()));
                        trace_steps.set(resp.trace_steps.clone());
                        state.set(resp.state.clone());
                        if matches!(resp.state, SessionState::Loading) {
                            loop {
                                tokio::time::sleep(Duration::from_millis(200)).await;
                                let Some(s) = sessions.get(&resp.session_id) else {
                                    error.set(Some("session not found".to_string()));
                                    break;
                                };
                                let current = s.current_state();
                                state.set(current.clone());
                                let steps = s.get_trace_steps();
                                if !steps.is_empty() {
                                    trace_steps.set(steps);
                                }
                                if !matches!(current, SessionState::Loading) {
                                    break;
                                }
                            }
                        }
                    }
                    Err(e) => {
                        error.set(Some(e.to_string()));
                    }
                }
                loading.set(false);
            });
        }
    };

    let call_session = |f: fn(&crate::session::DebugSession) -> SessionState| {
        let sessions = ctx.sessions.clone();
        move |_| {
            let Some(id) = session_id() else {
                return;
            };
            let Some(s) = sessions.get(&id) else {
                let mut error = error;
                error.set(Some("session not found".to_string()));
                return;
            };
            s.touch();
            let mut state = state;
            state.set(f(&s));
        }
    };

    let on_step_into = call_session(|s| s.step_into());
    let on_step_over = call_session(|s| s.step_over());
    let on_continue = call_session(|s| s.continue_exec());
    let on_abort = {
        let sessions = ctx.sessions.clone();
        move |_| {
            let Some(id) = session_id() else {
                return;
            };
            if let Some(s) = sessions.get(&id) {
                s.abort();
                let mut state = state;
                state.set(SessionState::Aborted);
            }
        }
    };

    let current_step = match &state() {
        SessionState::Paused { snapshot } => Some(snapshot.step_number),
        _ => None,
    };

    rsx! {
        style { "{CSS}" }

        div { class: "toolbar",
            div { style: "font-weight: 800; color: #569cd6;", "EVM Debugger" }
            input {
                placeholder: "TX Hash",
                value: "{tx_hash}",
                oninput: move |evt| {
                    let mut tx_hash = tx_hash;
                    tx_hash.set(evt.value());
                }
            }
            input {
                class: "rpc",
                placeholder: "RPC URL",
                value: "{rpc_url}",
                oninput: move |evt| {
                    let mut rpc_url = rpc_url;
                    rpc_url.set(evt.value());
                }
            }
            button { disabled: loading(), onclick: on_load, "Load" }
            button { class: "secondary", onclick: on_step_into, "Step Into" }
            button { class: "secondary", onclick: on_step_over, "Step Over" }
            button { class: "secondary", onclick: on_continue, "Continue" }
            button { class: "danger", onclick: on_abort, "Abort" }
            div { class: "muted", style: "margin-left: auto;",
                {session_id().as_deref().unwrap_or("-")}
            }
        }

        if let Some(e) = error() {
            div { class: "error", "{e}" }
        }

        div { class: "grid",
            div { class: "pane",
                div { class: "title", "Opcodes" }
                div { class: "list",
                    for s in trace_steps().iter() {
                        div {
                            class: if current_step == Some(s.step) { "row active" } else { "row" },
                            span { class: "muted", "{s.step}" }
                            span { class: "muted", "pc={s.pc}" }
                            span { "{s.opcode_name}" }
                        }
                    }
                }
            }

            div { class: "pane",
                div { class: "title", "Snapshot" }
                match state() {
                    SessionState::Loading => rsx! { pre { "Loading..." } },
                    SessionState::Aborted => rsx! { pre { "Aborted" } },
                    SessionState::Error { message } => rsx! { pre { "{message}" } },
                    SessionState::Finished { result } => rsx! {
                        pre { "{serde_json::to_string_pretty(&result).unwrap_or_default()}" }
                    },
                    SessionState::Paused { snapshot } => rsx! {
                        pre { "{serde_json::to_string_pretty(&snapshot).unwrap_or_default()}" }
                    },
                }
            }

            div { class: "pane",
                div { class: "title", "Tips" }
                pre {
                    "当前为最小可用版 Dioxus LiveView UI：\n- Load 创建会话并轮询至就绪\n- Step Into/Over/Continue/Abort 直接操作内存快照\n\n下一步可以把 Snapshot 面板拆成 Stack/Memory/CallStack/Storage/Logs 等子视图。"
                }
            }
        }
    }
}
