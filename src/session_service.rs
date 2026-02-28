use crate::{
    app_state::SessionMap,
    executor::spawn_evm_thread,
    fetcher::fetch_tx_info,
    session::DebugSession,
    trace_cache::{load_trace_cache, save_trace_cache, trace_cache_path},
    types::{CreateSessionRequest, CreateSessionResponse, SessionState},
};
use anyhow::{Context, Result};
use std::sync::{atomic::AtomicBool, Arc};
use tokio::sync::Semaphore;

pub struct SessionService {
    sessions: SessionMap,
    evm_semaphore: Arc<Semaphore>,
}

impl SessionService {
    pub fn new(sessions: SessionMap, evm_semaphore: Arc<Semaphore>) -> Self {
        Self {
            sessions,
            evm_semaphore,
        }
    }

    pub async fn create_session(&self, req: CreateSessionRequest) -> Result<CreateSessionResponse> {
        let session_id = uuid::Uuid::new_v4().to_string();
        let canonical_hash = canonicalize_tx_hash(&req.tx_hash)?;
        let tx_info = fetch_tx_info(&canonical_hash, &req.rpc_url).await?;

        let trace_path = trace_cache_path(&canonical_hash, tx_info.chain_id, tx_info.block_number);
        if let Ok(Some(file)) = load_trace_cache(&trace_path) {
            let session = DebugSession::from_cache(file.snapshots, file.result);
            self.sessions.insert(session_id.clone(), session.clone());
            return Ok(CreateSessionResponse {
                session_id,
                state: session.current_state(),
                trace_steps: session.get_trace_steps(),
            });
        }

        let (snap_tx, snap_rx) = std::sync::mpsc::sync_channel::<crate::types::ChannelMessage>(1);
        let abort_flag = Arc::new(AtomicBool::new(false));

        let session = DebugSession::new(snap_rx, abort_flag.clone());
        self.sessions.insert(session_id.clone(), session.clone());

        let session_for_bg = session.clone();
        let trace_path_for_bg = trace_path.clone();
        let sem = self.evm_semaphore.clone();
        tokio::spawn(async move {
            let _permit = sem.acquire_owned().await.ok();
            let runtime = tokio::runtime::Handle::current();
            spawn_evm_thread(tx_info, req.rpc_url, snap_tx, abort_flag, runtime);
            let _ = tokio::task::spawn_blocking(move || {
                let _ = session_for_bg.wait_for_snapshots();
                if let Some((snapshots, result)) = session_for_bg.snapshots_for_cache() {
                    let _ = save_trace_cache(&trace_path_for_bg, &snapshots, &result);
                }
            })
            .await;
        });

        Ok(CreateSessionResponse {
            session_id,
            state: SessionState::Loading,
            trace_steps: vec![],
        })
    }
}

fn canonicalize_tx_hash(tx_hash: &str) -> Result<String> {
    let trimmed = tx_hash.trim();
    let h = match trimmed.parse::<alloy_primitives::B256>() {
        Ok(h) => h,
        Err(_) => {
            let stripped = trimmed
                .strip_prefix("0x")
                .or_else(|| trimmed.strip_prefix("0X"))
                .unwrap_or(trimmed);
            let with_prefix = format!("0x{}", stripped);
            with_prefix
                .parse::<alloy_primitives::B256>()
                .with_context(|| "invalid tx hash")?
        }
    };
    Ok(format!("{h:#x}"))
}

#[cfg(test)]
mod tests {
    use super::canonicalize_tx_hash;

    #[test]
    fn canonicalize_tx_hash_accepts_prefix_and_case() {
        let input = format!("  0X{}  ", "A".repeat(64));
        let out = canonicalize_tx_hash(&input).unwrap();
        assert_eq!(out, format!("0x{}", "a".repeat(64)));
    }

    #[test]
    fn canonicalize_tx_hash_adds_prefix() {
        let input = "b".repeat(64);
        let out = canonicalize_tx_hash(&input).unwrap();
        assert_eq!(out, format!("0x{}", "b".repeat(64)));
    }
}
