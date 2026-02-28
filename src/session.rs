use crate::types::{ChannelMessage, ExecutionResultInfo, SessionState, StepSnapshot, TraceStep};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    mpsc::Receiver,
    Arc, Mutex,
};

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
}

impl DebugSession {
    pub fn new(snap_rx: Receiver<ChannelMessage>, abort_flag: Arc<AtomicBool>) -> Arc<Self> {
        Arc::new(Self {
            data: Arc::new(Mutex::new(SessionData::Loading)),
            snap_rx: Mutex::new(Some(snap_rx)),
            abort_flag,
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
        })
    }

    pub fn snapshots_for_cache(&self) -> Option<(Vec<StepSnapshot>, Option<ExecutionResultInfo>)> {
        let data = self.data.lock().unwrap();
        match &*data {
            SessionData::Ready {
                snapshots, result, ..
            } => Some((snapshots.clone(), result.clone())),
            _ => None,
        }
    }

    /// Block until the EVM thread sends all snapshots (called once, from spawn_blocking).
    pub fn wait_for_snapshots(&self) -> SessionState {
        let rx = match self.snap_rx.lock().unwrap().take() {
            Some(r) => r,
            None => {
                return SessionState::Error {
                    message: "Snapshot receiver already consumed".to_string(),
                };
            }
        };

        let state = match rx.recv() {
            Ok(ChannelMessage::AllSnapshots { snapshots, result }) => {
                if snapshots.is_empty() {
                    let s = match result.as_ref() {
                        Some(r) => SessionState::Finished { result: r.clone() },
                        None => SessionState::Error {
                            message: "Execution produced no steps".to_string(),
                        },
                    };
                    *self.data.lock().unwrap() = SessionData::Ready {
                        snapshots: vec![],
                        current_index: 0,
                        result,
                    };
                    return s;
                }
                let first = snapshots[0].clone();
                *self.data.lock().unwrap() = SessionData::Ready {
                    snapshots,
                    current_index: 0,
                    result,
                };
                SessionState::Paused { snapshot: first }
            }
            Ok(ChannelMessage::Error(msg)) => {
                *self.data.lock().unwrap() = SessionData::Error(msg.clone());
                SessionState::Error { message: msg }
            }
            Err(_) => {
                let msg = "EVM thread disconnected".to_string();
                *self.data.lock().unwrap() = SessionData::Error(msg.clone());
                SessionState::Error { message: msg }
            }
        };
        state
    }

    /// Advance one step. Instant — just increments the index.
    pub fn step_into(&self) -> SessionState {
        let mut data = self.data.lock().unwrap();
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
        let mut data = self.data.lock().unwrap();
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
        let mut data = self.data.lock().unwrap();
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
        *self.data.lock().unwrap() = SessionData::Aborted;
    }

    /// Non-blocking snapshot of the current state.
    pub fn current_state(&self) -> SessionState {
        let data = self.data.lock().unwrap();
        self.current_state_locked(&data)
    }

    /// Extract lightweight trace (step, pc, opcode_name) for the full opcode list.
    pub fn get_trace_steps(&self) -> Vec<TraceStep> {
        let data = self.data.lock().unwrap();
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
