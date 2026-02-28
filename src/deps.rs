use crate::trace_cache::TraceCacheFile;
use crate::types::{CachedTxInfo, ChannelMessage, ExecutionResultInfo, StepSnapshot};
use std::future::Future;
use std::sync::{atomic::AtomicBool, mpsc::SyncSender, Arc};

pub trait TxFetcher: Send + Sync + 'static {
    type Fut<'a>: Future<Output = anyhow::Result<CachedTxInfo>> + Send + 'a
    where
        Self: 'a;

    fn fetch<'a>(&'a self, tx_hash: &'a str, rpc_url: &'a str) -> Self::Fut<'a>;
}

pub trait TraceCache: Send + Sync + 'static {
    fn trace_cache_path(&self, tx_hash: &str, chain_id: Option<u64>, block_number: u64) -> String;
    fn load(&self, path: &str) -> anyhow::Result<Option<TraceCacheFile>>;
    fn save(
        &self,
        path: &str,
        snapshots: &[StepSnapshot],
        result: &Option<ExecutionResultInfo>,
    ) -> anyhow::Result<()>;
}

pub trait Executor: Send + Sync + 'static {
    fn spawn(
        &self,
        tx_info: CachedTxInfo,
        rpc_url: String,
        snap_tx: SyncSender<ChannelMessage>,
        abort_flag: Arc<AtomicBool>,
        runtime: tokio::runtime::Handle,
    );
}
