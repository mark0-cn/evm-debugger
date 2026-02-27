use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// EVM thread → HTTP handler (sent once after full execution)
#[derive(Debug)]
pub enum ChannelMessage {
    /// All snapshots collected from the full execution run.
    AllSnapshots {
        snapshots: Vec<StepSnapshot>,
        result: Option<ExecutionResultInfo>,
    },
    Error(String),
}

/// Snapshot of EVM state at a given step
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StepSnapshot {
    pub step_number: u64,
    pub pc: usize,
    pub opcode: u8,
    pub opcode_name: String,
    pub call_depth: usize,
    pub gas_remaining: u64,
    pub gas_used: u64,
    /// Stack entries as hex strings, index 0 = top of stack
    pub stack: Vec<String>,
    pub memory_size: usize,
    /// Full memory as hex dump
    pub memory_hex: String,
    /// Storage changes grouped by contract address
    pub storage_changes: HashMap<String, HashMap<String, String>>,
    pub call_stack: Vec<CallFrame>,
    pub logs: Vec<LogEntry>,
    pub contract_address: String,
}

/// Lightweight trace entry: step#, pc, opcode name only (for the full opcode list).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TraceStep {
    pub step: u64,
    pub pc: usize,
    pub opcode: u8,
    pub opcode_name: String,
}

/// A call frame in the call stack
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CallFrame {
    pub depth: usize,
    pub contract: String,
    pub caller: String,
    pub value: String,
    pub kind: String, // "CALL", "STATICCALL", "DELEGATECALL", "CREATE"
}

/// A log entry emitted during execution
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LogEntry {
    pub contract: String,
    pub topics: Vec<String>,
    pub data: String,
}

/// Final execution result
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecutionResultInfo {
    pub success: bool,
    pub gas_used: u64,
    pub output: String,
    pub reason: String,
}

/// Cached transaction info stored on disk
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CachedTxInfo {
    pub caller: String,
    pub gas_limit: u64,
    pub gas_price: u128,
    pub max_priority_fee_per_gas: Option<u128>,
    pub value: String,
    pub data: String,
    pub nonce: u64,
    pub to: Option<String>,
    pub chain_id: Option<u64>,
    // Block environment
    pub block_number: u64,
    pub block_beneficiary: String,
    pub block_timestamp: u64,
    pub block_difficulty: String,
    pub block_gas_limit: u64,
    pub block_basefee: u128,
}

/// State of a debug session
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum SessionState {
    Loading,
    Paused { snapshot: StepSnapshot },
    Finished { result: ExecutionResultInfo },
    Error { message: String },
    Aborted,
}

/// Request body for creating a session
#[derive(Debug, Deserialize)]
pub struct CreateSessionRequest {
    pub tx_hash: String,
    pub rpc_url: String,
}

/// Response for session creation
#[derive(Debug, Serialize)]
pub struct CreateSessionResponse {
    pub session_id: String,
    pub state: SessionState,
    /// Full lightweight opcode trace so the frontend can show all opcodes immediately.
    pub trace_steps: Vec<TraceStep>,
}
