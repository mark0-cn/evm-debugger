use crate::types::{ChannelMessage, ExecutionResultInfo, SessionState, StepSnapshot, TraceStep};
use std::sync::atomic::AtomicU64;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    mpsc::Receiver,
    Arc, Mutex, MutexGuard,
};
use std::time::{SystemTime, UNIX_EPOCH};

/// Internal session data, stored once EVM finishes.
enum SessionData {
    Loading,
    Ready {
        snapshots: Vec<StepSnapshot>,
        current_index: usize,
        result: Option<ExecutionResultInfo>,
    },
    Aborted,
    Error(String),
}

pub struct DebugSession {
    data: Arc<Mutex<SessionData>>,
    /// Receiver consumed once on creation to receive all snapshots.
    snap_rx: Mutex<Option<Receiver<ChannelMessage>>>,
    /// Set by abort handler; inspector checks this and halts.
    pub abort_flag: Arc<AtomicBool>,
    last_access_secs: AtomicU64,
}

impl DebugSession {
    fn now_secs() -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs()
    }

    fn data_lock(&self) -> MutexGuard<'_, SessionData> {
        self.data.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn snap_rx_lock(&self) -> MutexGuard<'_, Option<Receiver<ChannelMessage>>> {
        self.snap_rx.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn new(snap_rx: Receiver<ChannelMessage>, abort_flag: Arc<AtomicBool>) -> Arc<Self> {
        Arc::new(Self {
            data: Arc::new(Mutex::new(SessionData::Loading)),
            snap_rx: Mutex::new(Some(snap_rx)),
            abort_flag,
            last_access_secs: AtomicU64::new(Self::now_secs()),
        })
    }

    pub fn from_cache(
        snapshots: Vec<StepSnapshot>,
        result: Option<ExecutionResultInfo>,
    ) -> Arc<Self> {
        Arc::new(Self {
            data: Arc::new(Mutex::new(SessionData::Ready {
                snapshots,
                current_index: 0,
                result,
            })),
            snap_rx: Mutex::new(None),
            abort_flag: Arc::new(AtomicBool::new(false)),
            last_access_secs: AtomicU64::new(Self::now_secs()),
        })
    }

    pub fn touch(&self) {
        self.last_access_secs
            .store(Self::now_secs(), Ordering::Relaxed);
    }

    pub fn last_access_secs(&self) -> u64 {
        self.last_access_secs.load(Ordering::Relaxed)
    }

    pub fn snapshots_for_cache(&self) -> Option<(Vec<StepSnapshot>, Option<ExecutionResultInfo>)> {
        let data = self.data_lock();
        match &*data {
            SessionData::Ready {
                snapshots, result, ..
            } => Some((snapshots.clone(), result.clone())),
            _ => None,
        }
    }

    /// Block until the EVM thread sends all snapshots (called once, from spawn_blocking).
    pub fn wait_for_snapshots(&self) -> SessionState {
        let rx = match self.snap_rx_lock().take() {
            Some(r) => r,
            None => {
                return SessionState::Error {
                    message: "Snapshot receiver already consumed".to_string(),
                };
            }
        };

        let msg = if let Ok(m) = rx.recv() {
            m
        } else {
            ChannelMessage::Error("EVM thread disconnected".to_string())
        };

        let mut data = self.data_lock();
        if matches!(&*data, SessionData::Aborted) {
            return SessionState::Aborted;
        }

        match msg {
            ChannelMessage::AllSnapshots { snapshots, result } => {
                if snapshots.is_empty() {
                    let s = match result.as_ref() {
                        Some(r) => SessionState::Finished { result: r.clone() },
                        None => SessionState::Error {
                            message: "Execution produced no steps".to_string(),
                        },
                    };
                    *data = SessionData::Ready {
                        snapshots: vec![],
                        current_index: 0,
                        result,
                    };
                    return s;
                }
                let first = snapshots[0].clone();
                *data = SessionData::Ready {
                    snapshots,
                    current_index: 0,
                    result,
                };
                SessionState::Paused { snapshot: first }
            }
            ChannelMessage::Error(msg) => {
                *data = SessionData::Error(msg.clone());
                SessionState::Error { message: msg }
            }
        }
    }

    /// Advance one step. Instant — just increments the index.
    pub fn step_into(&self) -> SessionState {
        let mut data = self.data_lock();
        match &mut *data {
            SessionData::Ready {
                snapshots,
                current_index,
                result,
            } => {
                *current_index = (*current_index + 1).min(snapshots.len());
                Self::state_at(snapshots, *current_index, result)
            }
            _ => self.current_state_locked(&data),
        }
    }

    /// Advance past the current call depth (step over inner CALLs).
    pub fn step_over(&self) -> SessionState {
        let mut data = self.data_lock();
        match &mut *data {
            SessionData::Ready {
                snapshots,
                current_index,
                result,
            } => {
                if *current_index >= snapshots.len() {
                    return Self::state_at(snapshots, *current_index, result);
                }
                let target_depth = snapshots[*current_index].call_depth;
                *current_index += 1;
                while *current_index < snapshots.len()
                    && snapshots[*current_index].call_depth > target_depth
                {
                    *current_index += 1;
                }
                Self::state_at(snapshots, *current_index, result)
            }
            _ => self.current_state_locked(&data),
        }
    }

    /// Jump to the end of execution.
    pub fn continue_exec(&self) -> SessionState {
        let mut data = self.data_lock();
        match &mut *data {
            SessionData::Ready {
                snapshots,
                current_index,
                result,
            } => {
                *current_index = snapshots.len();
                Self::state_at(snapshots, *current_index, result)
            }
            _ => self.current_state_locked(&data),
        }
    }

    /// Signal abort: stops the EVM (if still running) and marks the session.
    pub fn abort(&self) {
        self.abort_flag.store(true, Ordering::Relaxed);
        *self.data_lock() = SessionData::Aborted;
    }

    /// Non-blocking snapshot of the current state.
    pub fn current_state(&self) -> SessionState {
        let data = self.data_lock();
        self.current_state_locked(&data)
    }

    /// Extract lightweight trace (step, pc, opcode_name) for the full opcode list.
    pub fn get_trace_steps(&self) -> Vec<TraceStep> {
        let data = self.data_lock();
        match &*data {
            SessionData::Ready { snapshots, .. } => snapshots
                .iter()
                .map(|s| TraceStep {
                    step: s.step_number,
                    pc: s.pc,
                    opcode: s.opcode,
                    opcode_name: s.opcode_name.clone(),
                })
                .collect(),
            _ => vec![],
        }
    }

    // --- helpers ---

    fn state_at(
        snapshots: &[StepSnapshot],
        index: usize,
        result: &Option<ExecutionResultInfo>,
    ) -> SessionState {
        if index < snapshots.len() {
            SessionState::Paused {
                snapshot: snapshots[index].clone(),
            }
        } else {
            match result {
                Some(r) => SessionState::Finished { result: r.clone() },
                None => SessionState::Aborted,
            }
        }
    }

    fn current_state_locked(&self, data: &SessionData) -> SessionState {
        match data {
            SessionData::Loading => SessionState::Loading,
            SessionData::Ready {
                snapshots,
                current_index,
                result,
            } => Self::state_at(snapshots, *current_index, result),
            SessionData::Aborted => SessionState::Aborted,
            SessionData::Error(msg) => SessionState::Error {
                message: msg.clone(),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::DebugSession;
    use crate::types::{ChannelMessage, ExecutionResultInfo, SessionState, StepSnapshot};
    use std::collections::HashMap;
    use std::sync::{atomic::AtomicBool, Arc};

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
            memory_hex: "0x".to_string(),
            memory_truncated: false,
            storage_changes: HashMap::new(),
            call_stack: vec![],
            logs: vec![],
            contract_address: "0x0000000000000000000000000000000000000000".to_string(),
        }
    }

    #[test]
    fn abort_state_is_not_overwritten_by_late_snapshots() {
        let (tx, rx) = std::sync::mpsc::sync_channel::<ChannelMessage>(1);
        let abort_flag = Arc::new(AtomicBool::new(false));
        let session = DebugSession::new(rx, abort_flag);
        session.abort();

        let send_thread = std::thread::spawn(move || {
            let _ = tx.send(ChannelMessage::AllSnapshots {
                snapshots: vec![dummy_snapshot(0, 0)],
                result: Some(ExecutionResultInfo {
                    success: true,
                    gas_used: 1,
                    output: "0x".to_string(),
                    reason: "Stop".to_string(),
                }),
            });
        });

        let state = session.wait_for_snapshots();
        assert!(matches!(state, SessionState::Aborted));
        assert!(matches!(session.current_state(), SessionState::Aborted));
        let _ = send_thread.join();
    }

    #[test]
    fn wait_for_snapshots_sets_ready_and_pauses_on_first_step() {
        let (tx, rx) = std::sync::mpsc::sync_channel::<ChannelMessage>(1);
        let abort_flag = Arc::new(AtomicBool::new(false));
        let session = DebugSession::new(rx, abort_flag);

        let send_thread = std::thread::spawn(move || {
            let _ = tx.send(ChannelMessage::AllSnapshots {
                snapshots: vec![dummy_snapshot(7, 0)],
                result: None,
            });
        });

        let state = session.wait_for_snapshots();
        match state {
            SessionState::Paused { snapshot } => assert_eq!(snapshot.step_number, 7),
            _ => panic!("expected paused"),
        }
        let _ = send_thread.join();
    }

    #[test]
    fn poisoned_mutex_does_not_panic_on_read() {
        let session = DebugSession::from_cache(vec![dummy_snapshot(0, 0)], None);
        let s = session.clone();
        let _ = std::panic::catch_unwind(move || {
            let _guard = s.data.lock().unwrap();
            panic!("poison");
        });
        let _ = session.current_state();
    }

    #[test]
    fn touch_updates_last_access() {
        let session = DebugSession::from_cache(vec![dummy_snapshot(0, 0)], None);
        session
            .last_access_secs
            .store(1, std::sync::atomic::Ordering::Relaxed);
        session.touch();
        assert!(session.last_access_secs() >= 1);
    }
}
