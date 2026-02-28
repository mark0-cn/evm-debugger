use crate::types::{ExecutionResultInfo, StepSnapshot};
use anyhow::{Context, Result};
use std::path::Path;

use crate::fs_utils::write_atomic;

#[derive(Debug, serde::Serialize, serde::Deserialize)]
pub struct TraceCacheFile {
    pub snapshots: Vec<StepSnapshot>,
    pub result: Option<ExecutionResultInfo>,
}

pub fn trace_cache_path(tx_hash: &str, chain_id: Option<u64>, block_number: u64) -> String {
    let chain = chain_id
        .map(|v| v.to_string())
        .unwrap_or_else(|| "unknown".to_string());
    let trimmed = tx_hash.trim();
    let hash = trimmed
        .strip_prefix("0x")
        .or_else(|| trimmed.strip_prefix("0X"))
        .unwrap_or(trimmed);
    format!("cache/trace_{}_{}_{}.json", chain, block_number, hash)
}

pub fn load_trace_cache(path: &str) -> Result<Option<TraceCacheFile>> {
    if !Path::new(path).exists() {
        return Ok(None);
    }
    let data = std::fs::read_to_string(path).with_context(|| format!("reading {}", path))?;
    let file: TraceCacheFile =
        serde_json::from_str(&data).with_context(|| format!("deserializing {}", path))?;
    Ok(Some(file))
}

pub fn save_trace_cache(
    path: &str,
    snapshots: &[StepSnapshot],
    result: &Option<ExecutionResultInfo>,
) -> Result<()> {
    let file = TraceCacheFile {
        snapshots: snapshots.to_vec(),
        result: result.clone(),
    };
    let json = serde_json::to_string_pretty(&file).with_context(|| "serializing trace cache")?;
    write_atomic(path, &json).with_context(|| format!("writing {}", path))?;
    Ok(())
}
