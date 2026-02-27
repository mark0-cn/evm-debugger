use crate::inspector::StepDebugInspector;
use crate::types::{CachedTxInfo, ChannelMessage, ExecutionResultInfo, StepSnapshot};
use alloy_eips::BlockId;
use alloy_primitives::{TxKind, U256};
use alloy_provider::ProviderBuilder;
use alloy_rpc_client::ClientBuilder;
use alloy_transport::layers::RetryBackoffLayer;
use revm::{
    context::TxEnv,
    database::{AlloyDB, CacheDB},
    database_interface::WrapDatabaseAsync,
    inspector::InspectEvm,
    Context, MainBuilder, MainContext,
};
use std::sync::atomic::AtomicBool;
use std::sync::{mpsc::SyncSender, Arc, Mutex};

/// Spawn an OS thread that runs the EVM to completion and sends all snapshots at once.
///
/// Architecture:
/// - EVM runs without pausing (no channel back-and-forth per step).
/// - Inspector collects all StepSnapshots into a shared Arc<Mutex<Vec>>.
/// - After inspect_one_tx returns, snapshots are sent via snap_tx as one AllSnapshots message.
/// - "Stepping" in the HTTP layer is pure index navigation over the stored Vec.
pub fn spawn_evm_thread(
    tx_info: CachedTxInfo,
    rpc_url: String,
    snap_tx: SyncSender<ChannelMessage>,
    abort_flag: Arc<AtomicBool>,
) {
    std::thread::spawn(move || {
        let rt = match tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
        {
            Ok(rt) => rt,
            Err(e) => {
                let _ = snap_tx.send(ChannelMessage::Error(format!("Failed to build runtime: {}", e)));
                return;
            }
        };

        let _guard = rt.enter();

        let provider = match rpc_url.parse::<url::Url>() {
            Ok(url) => {
                // Wrap the HTTP transport with a retry-backoff layer so that
                // transient 429 / 5xx responses from rate-limited public RPCs are
                // automatically retried instead of failing immediately.
                let rpc_client = ClientBuilder::default()
                    .layer(RetryBackoffLayer::new(
                        10,   // max retries on rate-limit errors
                        1000, // initial backoff ms
                        100,  // compute-units/sec (conservative for free endpoints)
                    ))
                    .http(url);
                ProviderBuilder::new().connect_client(rpc_client)
            }
            Err(_) => {
                let _ = snap_tx.send(ChannelMessage::Error(format!("Invalid RPC URL: {}", rpc_url)));
                return;
            }
        };

        let prev_block_id: BlockId = (tx_info.block_number.saturating_sub(1)).into();
        let alloy_db = AlloyDB::new(provider, prev_block_id);

        let wrapped = match WrapDatabaseAsync::new(alloy_db) {
            Some(w) => w,
            None => {
                let _ = snap_tx.send(ChannelMessage::Error(
                    "WrapDatabaseAsync::new failed: need multi-thread runtime".to_string(),
                ));
                return;
            }
        };
        let cache_db = CacheDB::new(wrapped);

        let caller: alloy_primitives::Address = match tx_info.caller.parse() {
            Ok(a) => a,
            Err(_) => {
                let _ = snap_tx.send(ChannelMessage::Error(
                    format!("Invalid caller address: {}", tx_info.caller),
                ));
                return;
            }
        };

        let kind = if let Some(to) = &tx_info.to {
            match to.parse::<alloy_primitives::Address>() {
                Ok(addr) => TxKind::Call(addr),
                Err(_) => {
                    let _ = snap_tx.send(ChannelMessage::Error(format!("Invalid to address: {}", to)));
                    return;
                }
            }
        } else {
            TxKind::Create
        };

        let value =
            U256::from_str_radix(tx_info.value.trim_start_matches("0x"), 16).unwrap_or(U256::ZERO);
        let data: alloy_primitives::Bytes = {
            let hex_str = tx_info.data.trim_start_matches("0x");
            alloy_primitives::Bytes::from(hex::decode(hex_str).unwrap_or_default())
        };
        let block_beneficiary: alloy_primitives::Address =
            tx_info.block_beneficiary.parse().unwrap_or_default();
        let block_difficulty =
            U256::from_str_radix(tx_info.block_difficulty.trim_start_matches("0x"), 16)
                .unwrap_or(U256::ZERO);
        let chain_id = tx_info.chain_id.unwrap_or(1);

        let tx_env = match TxEnv::builder()
            .caller(caller)
            .gas_limit(tx_info.gas_limit)
            .gas_price(tx_info.gas_price)
            .gas_priority_fee(tx_info.max_priority_fee_per_gas)
            .value(value)
            .data(data)
            .nonce(tx_info.nonce)
            .kind(kind)
            .chain_id(Some(chain_id))
            .build()
        {
            Ok(tx) => tx,
            Err(e) => {
                let _ = snap_tx.send(ChannelMessage::Error(format!("TxEnv build error: {:?}", e)));
                return;
            }
        };

        let ctx = Context::mainnet()
            .with_db(cache_db)
            .modify_block_chained(|b| {
                b.number = U256::from(tx_info.block_number);
                b.beneficiary = block_beneficiary;
                b.timestamp = U256::from(tx_info.block_timestamp);
                b.difficulty = block_difficulty;
                b.gas_limit = tx_info.block_gas_limit;
                b.basefee = tx_info.block_basefee as u64;
            })
            .modify_cfg_chained(|c| {
                c.chain_id = chain_id;
                // Disable validation checks that produce false failures when replaying a
                // confirmed tx against block N-1 state.  Other txs from the same sender
                // in the same block may have advanced the nonce or spent part of the
                // balance, so both checks must be bypassed.
                c.disable_nonce_check = true;
                c.disable_balance_check = true;
            });

        // Shared snapshot buffer between inspector and this thread.
        let snapshots_arc: Arc<Mutex<Vec<StepSnapshot>>> = Arc::new(Mutex::new(Vec::new()));
        let inspector = StepDebugInspector::new(snapshots_arc.clone(), abort_flag);
        let mut evm = ctx.build_mainnet_with_inspector(inspector);

        // Run EVM to completion — inspector collects every step without pausing.
        let exec_result = evm.inspect_one_tx(tx_env);

        // Extract all collected snapshots.
        let snapshots = std::mem::take(&mut *snapshots_arc.lock().unwrap());

        // Build result info (if available).
        let result = match exec_result {
            Ok(r) => {
                use revm::context::result::ExecutionResult;
                let (success, gas_used, output, reason) = match r {
                    ExecutionResult::Success {
                        gas_used,
                        output,
                        reason,
                        ..
                    } => (
                        true,
                        gas_used,
                        format!("0x{}", hex::encode(output.data())),
                        format!("{:?}", reason),
                    ),
                    ExecutionResult::Revert { gas_used, output, .. } => (
                        false,
                        gas_used,
                        format!("0x{}", hex::encode(output.as_ref())),
                        "Revert".to_string(),
                    ),
                    ExecutionResult::Halt { gas_used, reason, .. } => {
                        (false, gas_used, String::new(), format!("{:?}", reason))
                    }
                };
                Some(ExecutionResultInfo { success, gas_used, output, reason })
            }
            Err(e) => {
                // Execution-level error — still send whatever snapshots we got.
                let msg = format!("{:?}", e);
                tracing::warn!("EVM execution error: {}", msg);
                // If no opcodes ran at all, surface the error directly so the
                // frontend shows a useful message instead of "Execution produced
                // no steps".
                if snapshots.is_empty() {
                    let _ = snap_tx.send(ChannelMessage::Error(msg));
                    return;
                }
                None
            }
        };

        let _ = snap_tx.send(ChannelMessage::AllSnapshots { snapshots, result });

        drop(rt);
    });
}
