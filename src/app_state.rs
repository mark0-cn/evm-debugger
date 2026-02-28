use crate::session::DebugSession;
use dashmap::DashMap;
use std::sync::Arc;
use tokio::sync::Semaphore;

pub type SessionMap = Arc<DashMap<String, Arc<DebugSession>>>;

#[derive(Clone)]
pub struct AppState {
    pub sessions: SessionMap,
    pub evm_semaphore: Arc<Semaphore>,
}
