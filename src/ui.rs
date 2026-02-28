use crate::{
    app_state::SessionMap,
    session_service::SessionService,
    types::{
        CallFrame, CreateSessionRequest, CreateSessionResponse, LogEntry, SessionState,
        StepSnapshot, TraceStep,
    },
};
use dioxus::prelude::*;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Semaphore;

const CSS: &str = r#"
* { box-sizing: border-box; margin: 0; padding: 0; }
body { font-family: "Consolas", "Monaco", monospace; background: #1e1e1e; color: #d4d4d4; height: 100vh; display: flex; flex-direction: column; }
.toolbar { display: flex; align-items: center; gap: 10px; padding: 8px 12px; background: #2d2d2d; border-bottom: 1px solid #3d3d3d; flex-shrink: 0; }
.toolbar h1 { font-size: 14px; font-weight: bold; color: #569cd6; margin-right: 10px; }
.toolbar input { background: #3c3c3c; border: 1px solid #555; color: #d4d4d4; padding: 4px 8px; font-family: inherit; font-size: 12px; border-radius: 3px; }
#txInput { width: 340px; }
#rpcInput { width: 280px; }
.btn { padding: 4px 12px; border: none; border-radius: 3px; cursor: pointer; font-family: inherit; font-size: 12px; font-weight: bold; }
.btn-primary { background: #0e639c; color: #fff; }
.btn-primary:hover { background: #1177bb; }
.btn-green { background: #4caf50; color: #fff; }
.btn-green:hover { background: #66bb6a; }
.btn-blue { background: #2196f3; color: #fff; }
.btn-blue:hover { background: #42a5f5; }
.btn-orange { background: #ff9800; color: #fff; }
.btn-orange:hover { background: #ffa726; }
.btn-red { background: #f44336; color: #fff; }
.btn-red:hover { background: #ef5350; }
.btn:disabled { opacity: 0.4; cursor: not-allowed; }
.status-bar { padding: 4px 12px; background: #007acc; color: #fff; font-size: 11px; display: flex; gap: 20px; flex-shrink: 0; }
.main-grid { display: grid; grid-template-columns: 220px 1fr 220px; grid-template-rows: 1fr auto; flex: 1; overflow: hidden; gap: 1px; background: #3d3d3d; }
.panel { background: #1e1e1e; overflow: auto; display: flex; flex-direction: column; }
.panel-header { padding: 6px 10px; font-size: 11px; font-weight: bold; color: #888; background: #252526; text-transform: uppercase; letter-spacing: 1px; flex-shrink: 0; border-bottom: 1px solid #3d3d3d; }
.panel-content { padding: 8px; flex: 1; overflow: auto; font-size: 12px; }
.bottom-grid { display: grid; grid-template-columns: 1fr 1fr; gap: 1px; background: #3d3d3d; height: 180px; grid-column: 1 / -1; }
.opcode-list { font-size: 12px; }
.opcode-row { padding: 2px 6px; display: flex; gap: 10px; cursor: default; border-radius: 2px; }
.opcode-row:hover { background: #2a2d2e; }
.opcode-row.current { background: #094771; color: #fff; }
.opcode-row .op-step { color: #888; width: 60px; flex-shrink: 0; }
.opcode-row .op-pc { color: #888; width: 70px; flex-shrink: 0; }
.opcode-row .op-name { color: #9cdcfe; font-weight: bold; width: 120px; }
.stack-item { padding: 2px 6px; font-size: 11px; color: #9cdcfe; border-bottom: 1px solid #2d2d2d; }
.stack-item .idx { color: #888; width: 28px; display: inline-block; }
.memory-row { font-size: 11px; color: #ce9178; margin-bottom: 2px; }
.memory-row .offset { color: #888; margin-right: 8px; }
.callframe { padding: 3px 6px; font-size: 11px; border-left: 2px solid #569cd6; margin-bottom: 4px; }
.callframe .cf-kind { color: #c586c0; font-size: 10px; }
.callframe .cf-addr { color: #4ec9b0; }
.log-item { padding: 4px 6px; border-left: 2px solid #dcdcaa; margin-bottom: 4px; font-size: 11px; }
.storage-item { padding: 2px 6px; font-size: 11px; border-bottom: 1px solid #2d2d2d; }
.storage-item .s-key { color: #569cd6; }
.storage-item .s-val { color: #4ec9b0; }
.loading { color: #888; text-align: center; padding: 20px; }
.sep { width: 1px; background: #555; height: 20px; display: inline-block; margin: 0 4px; }
"#;

#[derive(Clone)]
pub struct UiContext {
    pub sessions: SessionMap,
    pub evm_semaphore: Arc<Semaphore>,
}

fn short_hex(s: &str, n: usize) -> String {
    if s.len() <= n {
        s.to_string()
    } else {
        format!("{}…", &s[..n])
    }
}

fn normalize_error(mut msg: String) -> String {
    if msg.contains("429") || msg.contains("rate") || msg.contains("1015") {
        msg = "RPC rate limited (429). Try a paid RPC or wait and retry.".to_string();
    } else if msg.contains("LackOfFund") || msg.contains("InsufficientFunds") {
        msg = "Insufficient balance at block N-1 (sender's ETH was spent by earlier txs in the same block)."
            .to_string();
    }
    msg
}

fn memory_rows(memory_hex: &str) -> Vec<(String, String)> {
    if memory_hex.len() < 2 {
        return vec![];
    }
    let mut bytes: Vec<&str> = Vec::with_capacity(memory_hex.len() / 2);
    let mut i = 0;
    while i + 2 <= memory_hex.len() {
        bytes.push(&memory_hex[i..i + 2]);
        i += 2;
    }
    let mut rows = Vec::new();
    let mut offset = 0usize;
    let mut idx = 0usize;
    while idx < bytes.len() {
        let end = (idx + 32).min(bytes.len());
        let chunk = bytes[idx..end].join(" ");
        rows.push((format!("0x{:04x}", offset), chunk));
        offset += 32;
        idx = end;
    }
    rows
}

fn fmt_step(step: u64) -> String {
    format!("{:06}", step)
}

fn fmt_pc(pc: usize) -> String {
    format!("0x{:04x}", pc)
}

fn controls_enabled(state: &SessionState) -> bool {
    matches!(state, SessionState::Paused { .. })
}

fn current_step(state: &SessionState) -> Option<u64> {
    match state {
        SessionState::Paused { snapshot } => Some(snapshot.step_number),
        _ => None,
    }
}

fn status_bar_from_snapshot(
    snap: &StepSnapshot,
) -> (String, String, String, String, String, String, String) {
    let step = format!("Step: {}", snap.step_number);
    let gas_used = format!("Gas Used: {}", snap.gas_used);
    let gas_left = format!("Gas Left: {}", snap.gas_remaining);
    let depth = format!("Depth: {}", snap.call_depth);
    let pc = format!("PC: 0x{:x}", snap.pc);
    let op = format!("Op: {}", snap.opcode_name);
    let contract = format!("Contract: {}", short_hex(&snap.contract_address, 10));
    (step, gas_used, gas_left, depth, pc, op, contract)
}

fn render_call_stack(call_stack: &[CallFrame]) -> Element {
    if call_stack.is_empty() {
        return rsx! { p { class: "loading", "(none)" } };
    }
    rsx! {
        for f in call_stack.iter() {
            div { class: "callframe",
                div { class: "cf-kind", "[{f.depth}] {f.kind}" }
                div { class: "cf-addr", "{f.contract}" }
                div { style: "color:#888;font-size:10px", "from: {short_hex(&f.caller, 10)}" }
            }
        }
    }
}

fn render_stack(stack: &[String]) -> Element {
    if stack.is_empty() {
        return rsx! { p { class: "loading", "(empty)" } };
    }
    rsx! {
        for (i, v) in stack.iter().enumerate() {
            div { class: "stack-item",
                span { class: "idx", "[{i}]" }
                " {v}"
            }
        }
    }
}

fn render_memory(memory_hex: &str, truncated: bool, total_size: usize) -> Element {
    if memory_hex.is_empty() {
        return rsx! { p { class: "loading", "(empty)" } };
    }
    let rows = memory_rows(memory_hex);
    rsx! {
        if truncated {
            p { class: "loading", "(truncated, total {total_size} bytes)" }
        }
        for (off, chunk) in rows {
            div { class: "memory-row",
                span { class: "offset", "{off}" }
                "{chunk}"
            }
        }
    }
}

fn render_storage(storage_changes: &HashMap<String, HashMap<String, String>>) -> Element {
    if storage_changes.is_empty() {
        return rsx! { p { class: "loading", "(none)" } };
    }
    rsx! {
        for (addr, slots) in storage_changes.iter() {
            div { style: "color:#888;font-size:10px;margin:4px 0 2px", "{addr}" }
            for (k, v) in slots.iter() {
                div { class: "storage-item",
                    span { class: "s-key", "{short_hex(k, 18)}" }
                    " → "
                    span { class: "s-val", "{short_hex(v, 18)}" }
                }
            }
        }
    }
}

fn render_logs(logs: &[LogEntry]) -> Element {
    if logs.is_empty() {
        return rsx! { p { class: "loading", "(none)" } };
    }
    rsx! {
        for l in logs.iter() {
            div { class: "log-item",
                div { style: "color:#dcdcaa", "{l.contract}" }
                for (i, t) in l.topics.iter().enumerate() {
                    div { style: "color:#888;font-size:10px", "topic{i}: {short_hex(t, 20)}" }
                }
                div { style: "color:#ce9178;font-size:10px", "data: {short_hex(&l.data, 30)}" }
            }
        }
    }
}

pub fn app() -> Element {
    let ctx = use_context::<UiContext>();
    let svc = Arc::new(SessionService::new(
        ctx.sessions.clone(),
        ctx.evm_semaphore.clone(),
    ));

    let tx_hash = use_signal(|| "".to_string());
    let rpc_url = use_signal(|| "https://eth.llamarpc.com".to_string());
    let session_id = use_signal(|| None::<String>);
    let trace_steps = use_signal(Vec::<TraceStep>::new);
    let state = use_signal(|| SessionState::Loading);
    let load_in_flight = use_signal(|| false);
    let status_msg = use_signal(|| "".to_string());
    let error_msg = use_signal(|| None::<String>);

    let on_load = {
        let sessions = ctx.sessions.clone();
        let svc = svc.clone();
        use_callback(move |_| {
            let svc = svc.clone();
            let tx = tx_hash();
            let rpc = rpc_url();
            let mut load_in_flight = load_in_flight;
            let mut status_msg = status_msg;
            let mut error_msg = error_msg;
            if tx.trim().is_empty() {
                error_msg.set(Some("Enter a transaction hash".to_string()));
                return;
            }
            if rpc.trim().is_empty() {
                error_msg.set(Some("Enter an RPC URL".to_string()));
                return;
            }
            if load_in_flight() {
                return;
            }
            load_in_flight.set(true);
            error_msg.set(None);
            status_msg.set("Loading (running full trace)...".to_string());

            let sessions = sessions.clone();
            let session_id_for_async = session_id;
            let trace_steps_for_async = trace_steps;
            let state_for_async = state;
            let load_flag_for_async = load_in_flight;
            let status_for_async = status_msg;
            let error_for_async = error_msg;

            spawn(async move {
                let mut session_id = session_id_for_async;
                let mut trace_steps = trace_steps_for_async;
                let mut state = state_for_async;
                let mut load_in_flight = load_flag_for_async;
                let mut status_msg = status_for_async;
                let mut error_msg = error_for_async;

                trace_steps.set(vec![]);
                state.set(SessionState::Loading);

                let req = CreateSessionRequest {
                    tx_hash: tx,
                    rpc_url: rpc,
                };
                let resp: anyhow::Result<CreateSessionResponse> = svc.create_session(req).await;
                match resp {
                    Ok(resp) => {
                        session_id.set(Some(resp.session_id.clone()));
                        trace_steps.set(resp.trace_steps.clone());
                        state.set(resp.state.clone());

                        if matches!(resp.state, SessionState::Loading) {
                            loop {
                                tokio::time::sleep(Duration::from_millis(500)).await;
                                let Some(s) = sessions.get(&resp.session_id) else {
                                    error_msg.set(Some("session not found".to_string()));
                                    break;
                                };
                                let cur = s.current_state();
                                state.set(cur.clone());
                                let steps = s.get_trace_steps();
                                if !steps.is_empty() {
                                    trace_steps.set(steps);
                                }
                                if !matches!(cur, SessionState::Loading) {
                                    break;
                                }
                            }
                        }

                        match &state() {
                            SessionState::Paused { .. } => status_msg.set("".to_string()),
                            SessionState::Finished { result } => {
                                status_msg.set(format!(
                                    "{} | Gas: {}",
                                    if result.success { "✓ Success" } else { "✗" },
                                    result.gas_used
                                ));
                            }
                            SessionState::Error { message } => {
                                status_msg
                                    .set(format!("Error: {}", normalize_error(message.clone())));
                            }
                            SessionState::Aborted => status_msg.set("Aborted".to_string()),
                            SessionState::Loading => {}
                        }
                    }
                    Err(e) => {
                        status_msg.set(format!("Error: {}", normalize_error(e.to_string())));
                    }
                }
                load_in_flight.set(false);
            });
        })
    };

    let call_session = |f: fn(&crate::session::DebugSession) -> SessionState| {
        let sessions = ctx.sessions.clone();
        move |_| {
            let Some(id) = session_id() else {
                return;
            };
            let Some(s) = sessions.get(&id) else {
                let mut status_msg = status_msg;
                status_msg.set("Error: session not found".to_string());
                return;
            };
            s.touch();
            let next = f(&s);
            let mut state = state;
            state.set(next);
        }
    };

    let on_step_into = call_session(|s| s.step_into());
    let on_step_over = call_session(|s| s.step_over());
    let on_continue = {
        let sessions = ctx.sessions.clone();
        move |_| {
            let Some(id) = session_id() else {
                return;
            };
            let Some(s) = sessions.get(&id) else {
                let mut status_msg = status_msg;
                status_msg.set("Error: session not found".to_string());
                return;
            };
            let mut status_msg = status_msg;
            status_msg.set("Running to completion...".to_string());
            s.touch();
            let next = s.continue_exec();
            let mut state = state;
            state.set(next);
        }
    };

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
                let mut status_msg = status_msg;
                status_msg.set("Aborted".to_string());
            }
        }
    };

    let enabled = controls_enabled(&state());
    let cur_step = current_step(&state());
    let snap_opt = match state() {
        SessionState::Paused { snapshot } => Some(snapshot),
        _ => None,
    };

    let (s_step, s_gas_used, s_gas_left, s_depth, s_pc, s_op, s_contract) =
        snap_opt.as_ref().map(status_bar_from_snapshot).unwrap_or((
            "Step: -".to_string(),
            "Gas Used: -".to_string(),
            "Gas Left: -".to_string(),
            "Depth: -".to_string(),
            "PC: -".to_string(),
            "Op: -".to_string(),
            "Contract: -".to_string(),
        ));

    rsx! {
        style { "{CSS}" }

        div { class: "toolbar",
            h1 { "⚡ EVM Debugger" }
            input {
                id: "txInput",
                r#type: "text",
                placeholder: "0x transaction hash...",
                value: "{tx_hash}",
                oninput: move |evt| {
                    let mut tx_hash = tx_hash;
                    tx_hash.set(evt.value());
                },
                onkeydown: move |evt| {
                    if evt.key() == Key::Enter {
                        on_load(());
                    }
                }
            }
            input {
                id: "rpcInput",
                r#type: "text",
                placeholder: "RPC URL",
                value: "{rpc_url}",
                oninput: move |evt| {
                    let mut rpc_url = rpc_url;
                    rpc_url.set(evt.value());
                }
            }
            button { class: "btn btn-primary", disabled: load_in_flight(), onclick: move |_| on_load(()), "Load (Enter)" }
            span { class: "sep" }
            button { class: "btn btn-green", disabled: !enabled, onclick: on_step_into, "Step Into (F11)" }
            button { class: "btn btn-blue", disabled: !enabled, onclick: on_step_over, "Step Over (F10)" }
            button { class: "btn btn-orange", disabled: !enabled, onclick: on_continue, "Continue (F5)" }
            button { class: "btn btn-red", disabled: session_id().is_none(), onclick: on_abort, "Abort" }
            span { style: "color:#888;font-size:12px;margin-left:8px", "{status_msg}" }
        }

        div { class: "status-bar",
            span { "{s_step}" }
            span { "{s_gas_used}" }
            span { "{s_gas_left}" }
            span { "{s_depth}" }
            span { "{s_pc}" }
            span { "{s_op}" }
            span { "{s_contract}" }
        }

        if let Some(e) = error_msg() {
            div { style: "padding: 6px 12px; color: #ff6b6b; font-size: 12px;", "{e}" }
        }

        div { class: "main-grid",
            div { class: "panel",
                div { class: "panel-header", "Call Stack" }
                div { class: "panel-content",
                    match snap_opt.as_ref() {
                        Some(snap) => render_call_stack(&snap.call_stack),
                        None => rsx! { p { class: "loading", "—" } },
                    }
                }
            }

            div { class: "panel",
                div { class: "panel-header", "Bytecode / Opcodes" }
                div { class: "panel-content",
                    if trace_steps().is_empty() {
                        p { class: "loading", "Load a transaction to begin debugging." }
                    } else {
                        div { class: "opcode-list",
                            for h in trace_steps().iter() {
                                div {
                                    class: if cur_step == Some(h.step) { "opcode-row current" } else { "opcode-row" },
                                    span { class: "op-step", {fmt_step(h.step)} }
                                    span { class: "op-pc", {fmt_pc(h.pc)} }
                                    span { class: "op-name", "{h.opcode_name}" }
                                }
                            }
                        }
                    }
                }
            }

            div { class: "panel",
                div { style: "flex:1;overflow:auto;display:flex;flex-direction:column",
                    div { class: "panel-header", "Stack" }
                    div { class: "panel-content",
                        match snap_opt.as_ref() {
                            Some(snap) => render_stack(&snap.stack),
                            None => rsx! { p { class: "loading", "—" } },
                        }
                    }
                }
                div { style: "flex:1;overflow:auto;display:flex;flex-direction:column;border-top:1px solid #3d3d3d",
                    div { class: "panel-header", "Memory" }
                    div { class: "panel-content",
                        match snap_opt.as_ref() {
                            Some(snap) => render_memory(&snap.memory_hex, snap.memory_truncated, snap.memory_size),
                            None => rsx! { p { class: "loading", "—" } },
                        }
                    }
                }
            }

            div { class: "bottom-grid",
                div { class: "panel",
                    div { class: "panel-header", "Storage Changes" }
                    div { class: "panel-content",
                        match snap_opt.as_ref() {
                            Some(snap) => render_storage(&snap.storage_changes),
                            None => rsx! { p { class: "loading", "—" } },
                        }
                    }
                }
                div { class: "panel",
                    div { class: "panel-header", "Logs" }
                    div { class: "panel-content",
                        match snap_opt.as_ref() {
                            Some(snap) => render_logs(&snap.logs),
                            None => rsx! { p { class: "loading", "—" } },
                        }
                    }
                }
            }
        }
    }
}
