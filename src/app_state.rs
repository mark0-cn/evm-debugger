use crate::session::DebugSession;
use dashmap::DashMap;
use std::sync::Arc;

pub type SessionMap = Arc<DashMap<String, Arc<DebugSession>>>;

#[derive(Clone)]
pub struct AppState {
    pub sessions: SessionMap,
}
