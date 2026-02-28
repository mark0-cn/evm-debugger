use crate::{
    app_state::SessionMap,
    deps::{Executor, TraceCache, TxFetcher},
    session::DebugSession,
    types::{
        CachedTxInfo, ChannelMessage, CreateSessionRequest, CreateSessionResponse,
        ExecutionResultInfo, SessionState, StepSnapshot,
    },
};
use anyhow::{Context, Result};
use std::future::Future;
use std::pin::Pin;
use std::sync::{atomic::AtomicBool, mpsc::SyncSender, Arc};
use tokio::sync::Semaphore;

pub struct SessionService<F: TxFetcher, C: TraceCache, E: Executor> {
    sessions: SessionMap,
    evm_semaphore: Arc<Semaphore>,
    fetcher: Arc<F>,
    cache: Arc<C>,
    executor: Arc<E>,
}

pub struct DefaultTxFetcher;
pub struct DefaultTraceCache;
pub struct DefaultExecutor;

impl TxFetcher for DefaultTxFetcher {
    type Fut<'a> = Pin<Box<dyn Future<Output = anyhow::Result<CachedTxInfo>> + Send + 'a>>;

    fn fetch<'a>(&'a self, tx_hash: &'a str, rpc_url: &'a str) -> Self::Fut<'a> {
        Box::pin(crate::fetcher::fetch_tx_info(tx_hash, rpc_url))
    }
}

impl TraceCache for DefaultTraceCache {
    fn trace_cache_path(&self, tx_hash: &str, chain_id: Option<u64>, block_number: u64) -> String {
        crate::trace_cache::trace_cache_path(tx_hash, chain_id, block_number)
    }

    fn load(&self, path: &str) -> anyhow::Result<Option<crate::trace_cache::TraceCacheFile>> {
        crate::trace_cache::load_trace_cache(path)
    }

    fn save(
        &self,
        path: &str,
        snapshots: &[StepSnapshot],
        result: &Option<ExecutionResultInfo>,
    ) -> anyhow::Result<()> {
        crate::trace_cache::save_trace_cache(path, snapshots, result)
    }
}

impl Executor for DefaultExecutor {
    fn spawn(
        &self,
        tx_info: CachedTxInfo,
        rpc_url: String,
        snap_tx: SyncSender<ChannelMessage>,
        abort_flag: Arc<AtomicBool>,
        runtime: tokio::runtime::Handle,
    ) {
        crate::executor::spawn_evm_thread(tx_info, rpc_url, snap_tx, abort_flag, runtime);
    }
}

impl SessionService<DefaultTxFetcher, DefaultTraceCache, DefaultExecutor> {
    pub fn new(sessions: SessionMap, evm_semaphore: Arc<Semaphore>) -> Self {
        Self::new_with(
            sessions,
            evm_semaphore,
            Arc::new(DefaultTxFetcher),
            Arc::new(DefaultTraceCache),
            Arc::new(DefaultExecutor),
        )
    }
}

impl<F: TxFetcher, C: TraceCache, E: Executor> SessionService<F, C, E> {
    pub fn new_with(
        sessions: SessionMap,
        evm_semaphore: Arc<Semaphore>,
        fetcher: Arc<F>,
        cache: Arc<C>,
        executor: Arc<E>,
    ) -> Self {
        Self {
            sessions,
            evm_semaphore,
            fetcher,
            cache,
            executor,
        }
    }

    pub async fn create_session(&self, req: CreateSessionRequest) -> Result<CreateSessionResponse> {
        let session_id = uuid::Uuid::new_v4().to_string();
        let canonical_hash = canonicalize_tx_hash(&req.tx_hash)?;
        let tx_info = self.fetcher.fetch(&canonical_hash, &req.rpc_url).await?;

        let trace_path =
            self.cache
                .trace_cache_path(&canonical_hash, tx_info.chain_id, tx_info.block_number);
        if let Some(file) = self.cache.load(&trace_path)? {
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
        let executor = self.executor.clone();
        let cache = self.cache.clone();
        tokio::spawn(async move {
            let _permit = sem.acquire_owned().await.ok();
            let runtime = tokio::runtime::Handle::current();
            executor.spawn(tx_info, req.rpc_url, snap_tx, abort_flag, runtime);
            let _ = tokio::task::spawn_blocking(move || {
                let _ = session_for_bg.wait_for_snapshots();
                if let Some((snapshots, result)) = session_for_bg.snapshots_for_cache() {
                    let _ = cache.save(&trace_path_for_bg, &snapshots, &result);
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
    use super::{canonicalize_tx_hash, DefaultExecutor, SessionService};
    use crate::app_state::SessionMap;
    use crate::deps::{Executor, TraceCache, TxFetcher};
    use crate::types::{
        CachedTxInfo, ChannelMessage, CreateSessionRequest, ExecutionResultInfo, SessionState,
        StepSnapshot,
    };
    use dashmap::DashMap;
    use std::collections::HashMap;
    use std::future::Future;
    use std::pin::Pin;
    use std::sync::{atomic::AtomicBool, mpsc::SyncSender, Arc};
    use tokio::sync::Semaphore;

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

    struct FakeFetcher;
    impl TxFetcher for FakeFetcher {
        type Fut<'a> = Pin<Box<dyn Future<Output = anyhow::Result<CachedTxInfo>> + Send + 'a>>;
        fn fetch<'a>(&'a self, _tx_hash: &'a str, _rpc_url: &'a str) -> Self::Fut<'a> {
            Box::pin(async move {
                Ok(CachedTxInfo {
                    caller: "0x0000000000000000000000000000000000000000".to_string(),
                    gas_limit: 0,
                    gas_price: 0,
                    max_priority_fee_per_gas: None,
                    value: "0x0".to_string(),
                    data: "0x".to_string(),
                    nonce: 0,
                    to: None,
                    chain_id: Some(1),
                    block_number: 1,
                    block_beneficiary: "0x0000000000000000000000000000000000000000".to_string(),
                    block_timestamp: 0,
                    block_difficulty: "0x0".to_string(),
                    block_gas_limit: 0,
                    block_basefee: 0,
                })
            })
        }
    }

    struct FakeCache {
        hit: bool,
    }

    impl TraceCache for FakeCache {
        fn trace_cache_path(
            &self,
            _tx_hash: &str,
            _chain_id: Option<u64>,
            _block_number: u64,
        ) -> String {
            "cache/test.json".to_string()
        }

        fn load(&self, _path: &str) -> anyhow::Result<Option<crate::trace_cache::TraceCacheFile>> {
            if !self.hit {
                return Ok(None);
            }
            Ok(Some(crate::trace_cache::TraceCacheFile {
                snapshots: vec![dummy_snapshot(0, 0)],
                result: None,
            }))
        }

        fn save(
            &self,
            _path: &str,
            _snapshots: &[StepSnapshot],
            _result: &Option<ExecutionResultInfo>,
        ) -> anyhow::Result<()> {
            Ok(())
        }
    }

    struct FakeExecutor;
    impl Executor for FakeExecutor {
        fn spawn(
            &self,
            _tx_info: CachedTxInfo,
            _rpc_url: String,
            snap_tx: SyncSender<ChannelMessage>,
            _abort_flag: Arc<AtomicBool>,
            _runtime: tokio::runtime::Handle,
        ) {
            let _ = snap_tx.send(ChannelMessage::AllSnapshots {
                snapshots: vec![dummy_snapshot(0, 0)],
                result: None,
            });
        }
    }

    fn dummy_snapshot(step: u64, depth: usize) -> StepSnapshot {
        StepSnapshot {
            step_number: step,
            pc: 0,
            opcode: 0,
            opcode_name: "STOP".to_string(),
            call_depth: depth,
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
    async fn create_session_cache_hit_returns_ready() {
        let sessions: SessionMap = Arc::new(DashMap::new());
        let svc = SessionService::new_with(
            sessions,
            Arc::new(Semaphore::new(1)),
            Arc::new(FakeFetcher),
            Arc::new(FakeCache { hit: true }),
            Arc::new(DefaultExecutor),
        );
        let resp = svc
            .create_session(CreateSessionRequest {
                tx_hash: "0x".to_string() + &"a".repeat(64),
                rpc_url: "http://localhost".to_string(),
            })
            .await
            .unwrap();
        assert!(!resp.trace_steps.is_empty());
        assert!(matches!(
            resp.state,
            SessionState::Paused { .. }
                | SessionState::Finished { .. }
                | SessionState::Aborted
                | SessionState::Error { .. }
                | SessionState::Loading
        ));
    }

    #[tokio::test]
    async fn create_session_no_cache_returns_loading() {
        let sessions: SessionMap = Arc::new(DashMap::new());
        let svc = SessionService::new_with(
            sessions.clone(),
            Arc::new(Semaphore::new(1)),
            Arc::new(FakeFetcher),
            Arc::new(FakeCache { hit: false }),
            Arc::new(FakeExecutor),
        );
        let resp = svc
            .create_session(CreateSessionRequest {
                tx_hash: "0x".to_string() + &"b".repeat(64),
                rpc_url: "http://localhost".to_string(),
            })
            .await
            .unwrap();
        assert!(matches!(resp.state, SessionState::Loading));
        let id = resp.session_id;
        for _ in 0..50 {
            if let Some(s) = sessions.get(&id) {
                let state = s.current_state();
                if matches!(state, SessionState::Paused { .. }) {
                    return;
                }
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        panic!("session did not become ready");
    }
}
