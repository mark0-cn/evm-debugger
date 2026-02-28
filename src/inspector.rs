use crate::types::{CallFrame, LogEntry, StepSnapshot};
use revm::inspector::Inspector;
use revm::interpreter::{
    interpreter_types::{Jumps, MemoryTr, StackTr},
    CallInputs, CallOutcome, CreateInputs, CreateOutcome, Interpreter, InterpreterTypes,
    CallScheme, InstructionResult,
};
use revm::context::ContextTr;
use revm::context_interface::{JournalTr, Transaction};
use revm::primitives::{hex, Address, Log};
use revm::state::bytecode::opcode::OpCode;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

/// Inspector that collects ALL step snapshots without pausing.
/// Snapshots are pushed to a shared Vec; the executor extracts them after execution.
pub struct StepDebugInspector {
    /// Shared with the executor — snapshots are extracted after inspect_one_tx returns.
    pub snapshots: Arc<Mutex<Vec<StepSnapshot>>>,
    /// Set to true by the abort handler; inspector halts EVM at next step.
    pub abort_flag: Arc<AtomicBool>,
    call_stack: Vec<CallFrame>,
    logs: Vec<LogEntry>,
    storage_changes: HashMap<String, HashMap<String, String>>,
    step_number: u64,
    gas_initial: u64,
    max_memory_bytes: usize,
}

impl StepDebugInspector {
    pub fn new(snapshots: Arc<Mutex<Vec<StepSnapshot>>>, abort_flag: Arc<AtomicBool>) -> Self {
        let max_memory_bytes = std::env::var("EVM_DEBUGGER_MAX_MEMORY_BYTES")
            .ok()
            .and_then(|v| v.parse::<usize>().ok())
            .unwrap_or(4096);
        Self {
            snapshots,
            abort_flag,
            call_stack: Vec::new(),
            logs: Vec::new(),
            storage_changes: HashMap::new(),
            step_number: 0,
            gas_initial: 0,
            max_memory_bytes,
        }
    }

    fn encode_memory_hex(bytes: &[u8], max_bytes: usize) -> (String, bool) {
        if bytes.is_empty() {
            return (String::new(), false);
        }
        let truncated = bytes.len() > max_bytes;
        let end = bytes.len().min(max_bytes);
        (hex::encode(&bytes[..end]), truncated)
    }

    fn capture_snapshot<INTR>(&self, interp: &Interpreter<INTR>, depth: usize) -> StepSnapshot
    where
        INTR: InterpreterTypes,
        INTR::Stack: StackTr,
        INTR::Memory: MemoryTr,
        INTR::Bytecode: Jumps,
    {
        let pc = interp.bytecode.pc();
        let opcode = interp.bytecode.opcode();
        let gas_remaining = interp.gas.remaining();
        let gas_used = self.gas_initial.saturating_sub(gas_remaining);

        let stack_data = interp.stack.data();
        let stack: Vec<String> = stack_data
            .iter()
            .rev()
            .map(|v| format!("{:#066x}", v))
            .collect();

        let mem_size = interp.memory.size();
        let (memory_hex, memory_truncated) = if mem_size > 0 {
            let slice = interp.memory.slice(0..mem_size);
            Self::encode_memory_hex(slice.as_ref(), self.max_memory_bytes)
        } else {
            (String::new(), false)
        };

        let contract_address = if let Some(frame) = self.call_stack.last() {
            frame.contract.clone()
        } else {
            String::from("0x0000000000000000000000000000000000000000")
        };

        let opcode_name = OpCode::new(opcode)
            .map(|o| o.as_str().to_string())
            .unwrap_or_else(|| format!("0x{:02x}", opcode));

        StepSnapshot {
            step_number: self.step_number,
            pc,
            opcode,
            opcode_name,
            call_depth: depth,
            gas_remaining,
            gas_used,
            stack,
            memory_size: mem_size,
            memory_hex,
            memory_truncated,
            storage_changes: self.storage_changes.clone(),
            call_stack: self.call_stack.clone(),
            logs: self.logs.clone(),
            contract_address,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::StepDebugInspector;

    #[test]
    fn encode_memory_hex_truncates() {
        let bytes = vec![0u8; 10];
        let (hex, truncated) = StepDebugInspector::encode_memory_hex(&bytes, 4);
        assert_eq!(hex.len(), 8);
        assert!(truncated);
    }

    #[test]
    fn encode_memory_hex_no_truncate() {
        let bytes = vec![1u8, 2u8];
        let (hex, truncated) = StepDebugInspector::encode_memory_hex(&bytes, 4);
        assert_eq!(hex, "0102");
        assert!(!truncated);
    }
}

impl<CTX, INTR> Inspector<CTX, INTR> for StepDebugInspector
where
    CTX: ContextTr,
    <CTX as ContextTr>::Journal: JournalTr,
    <CTX as ContextTr>::Tx: Transaction,
    INTR: InterpreterTypes,
    INTR::Stack: StackTr,
    INTR::Memory: MemoryTr,
    INTR::Bytecode: Jumps,
{
    fn initialize_interp(&mut self, interp: &mut Interpreter<INTR>, _context: &mut CTX) {
        self.gas_initial = interp.gas.limit();
    }

    fn step(&mut self, interp: &mut Interpreter<INTR>, context: &mut CTX) {
        // Honour abort requests.
        if self.abort_flag.load(Ordering::Relaxed) {
            interp.halt(InstructionResult::Stop);
            return;
        }

        let depth = context.journal().depth();

        // Capture SSTORE key/value before the opcode executes.
        let opcode = interp.bytecode.opcode();
        if opcode == 0x55 {
            let stack = interp.stack.data();
            if stack.len() >= 2 {
                let key = stack[stack.len() - 1];
                let value = stack[stack.len() - 2];
                let contract = if let Some(frame) = self.call_stack.last() {
                    frame.contract.clone()
                } else {
                    String::from("0x0000000000000000000000000000000000000000")
                };
                self.storage_changes
                    .entry(contract)
                    .or_default()
                    .insert(format!("{:#066x}", key), format!("{:#066x}", value));
            }
        }

        let snapshot = self.capture_snapshot(interp, depth);
        self.step_number += 1;

        // Push to shared Vec — no blocking, no channel.
        if let Ok(mut v) = self.snapshots.lock() {
            v.push(snapshot);
        }
    }

    fn call(&mut self, context: &mut CTX, inputs: &mut CallInputs) -> Option<CallOutcome> {
        let kind = match inputs.scheme {
            CallScheme::Call => "CALL",
            CallScheme::StaticCall => "STATICCALL",
            CallScheme::DelegateCall => "DELEGATECALL",
            CallScheme::CallCode => "CALLCODE",
        };
        let depth = context.journal().depth();
        self.call_stack.push(CallFrame {
            depth,
            contract: format!("{:#x}", inputs.target_address),
            caller: format!("{:#x}", inputs.caller),
            value: format!("{:#x}", inputs.call_value()),
            kind: kind.to_string(),
        });
        None
    }

    fn call_end(&mut self, _context: &mut CTX, _inputs: &CallInputs, _outcome: &mut CallOutcome) {
        self.call_stack.pop();
    }

    fn create(&mut self, context: &mut CTX, inputs: &mut CreateInputs) -> Option<CreateOutcome> {
        let depth = context.journal().depth();
        self.call_stack.push(CallFrame {
            depth,
            contract: format!("{:#x}", Address::ZERO),
            caller: format!("{:#x}", inputs.caller()),
            value: format!("{:#x}", inputs.value()),
            kind: "CREATE".to_string(),
        });
        None
    }

    fn create_end(
        &mut self,
        _context: &mut CTX,
        _inputs: &CreateInputs,
        _outcome: &mut CreateOutcome,
    ) {
        self.call_stack.pop();
    }

    fn log_full(&mut self, _interp: &mut Interpreter<INTR>, _context: &mut CTX, log: Log) {
        self.logs.push(crate::types::LogEntry {
            contract: format!("{:#x}", log.address),
            topics: log
                .data
                .topics()
                .iter()
                .map(|t| format!("{:#x}", t))
                .collect(),
            data: format!("0x{}", hex::encode(&log.data.data)),
        });
    }
}
