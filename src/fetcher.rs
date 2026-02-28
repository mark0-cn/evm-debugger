use crate::types::CachedTxInfo;
use alloy_consensus::Transaction as _;
use alloy_eips::BlockNumberOrTag;
use alloy_provider::{Provider, ProviderBuilder};
use alloy_rpc_client::ClientBuilder;
use alloy_transport::layers::RetryBackoffLayer;
use anyhow::{anyhow, Context, Result};
use std::path::Path;

use crate::fs_utils::write_atomic;

/// Load transaction info: check cache first, then fetch from RPC.
pub async fn fetch_tx_info(tx_hash: &str, rpc_url: &str) -> Result<CachedTxInfo> {
    let raw = tx_hash.trim();
    let normalized_input = if raw.starts_with("0x") || raw.starts_with("0X") {
        raw.to_string()
    } else {
        format!("0x{}", raw)
    };

    let hash: alloy_primitives::B256 = normalized_input
        .parse()
        .with_context(|| format!("parsing tx hash: {}", tx_hash))?;
    let canonical_hash = format!("{hash:#x}");

    let cache_path = format!("cache/{}.json", canonical_hash);
    let legacy_cache_path = format!("cache/{}.json", raw);

    let read_path = if Path::new(&cache_path).exists() {
        Some(cache_path.as_str())
    } else if Path::new(&legacy_cache_path).exists() {
        Some(legacy_cache_path.as_str())
    } else {
        None
    };

    if let Some(path) = read_path {
        let data = std::fs::read_to_string(path)
            .with_context(|| format!("reading cache file {}", path))?;
        let info: CachedTxInfo =
            serde_json::from_str(&data).with_context(|| "deserializing cached tx")?;
        tracing::info!("Loaded tx {} from cache", canonical_hash);
        if path != cache_path.as_str() {
            let _ = write_atomic(&cache_path, &data);
        }
        return Ok(info);
    }

    tracing::info!("Fetching tx {} from RPC", canonical_hash);

    let rpc_url_parsed: url::Url = rpc_url
        .parse()
        .with_context(|| format!("parsing RPC URL: {}", rpc_url))?;

    let rpc_client = ClientBuilder::default()
        .layer(RetryBackoffLayer::new(
            5,    // max retries
            1000, // initial backoff ms
            100,  // compute-units/sec
        ))
        .http(rpc_url_parsed);
    let provider = ProviderBuilder::new().connect_client(rpc_client);

    let tx = provider
        .get_transaction_by_hash(hash)
        .await
        .with_context(|| format!("fetching transaction {}", canonical_hash))?
        .ok_or_else(|| anyhow!("transaction not found: {}", canonical_hash))?;

    let block_number = tx
        .block_number
        .ok_or_else(|| anyhow!("transaction is pending (no block number)"))?;

    let block = provider
        .get_block_by_number(BlockNumberOrTag::Number(block_number))
        .await
        .with_context(|| format!("fetching block {}", block_number))?
        .ok_or_else(|| anyhow!("block {} not found", block_number))?;

    let inner = &tx.inner;
    let caller = format!("{:#x}", inner.signer());

    let to = inner.to().map(|addr| format!("{:#x}", addr));

    let info = CachedTxInfo {
        caller,
        gas_limit: inner.gas_limit(),
        gas_price: inner.gas_price().unwrap_or_else(|| inner.max_fee_per_gas()),
        max_priority_fee_per_gas: inner.max_priority_fee_per_gas(),
        value: format!("{:#x}", inner.value()),
        data: format!("0x{}", hex::encode(inner.input())),
        nonce: inner.nonce(),
        to,
        chain_id: inner.chain_id(),
        block_number,
        block_beneficiary: format!("{:#x}", block.header.beneficiary),
        block_timestamp: block.header.timestamp,
        block_difficulty: format!("{:#x}", block.header.difficulty),
        block_gas_limit: block.header.gas_limit,
        block_basefee: block.header.base_fee_per_gas.unwrap_or(0) as u128,
    };

    let json = serde_json::to_string_pretty(&info)?;
    write_atomic(&cache_path, &json).with_context(|| format!("writing cache {}", cache_path))?;
    Ok(info)
}
