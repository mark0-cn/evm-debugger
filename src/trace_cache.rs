use crate::types::{ExecutionResultInfo, StepSnapshot};
use anyhow::{Context, Result};
use std::path::Path;

#[derive(Debug, serde::Serialize, serde::Deserialize)]
pub struct TraceCacheFile {
    pub snapshots: Vec<StepSnapshot>,
    pub result: Option<ExecutionResultInfo>,
}

pub fn trace_cache_path(tx_hash: &str, chain_id: Option<u64>, block_number: u64) -> String {
    let chain = chain_id
        .map(|v| v.to_string())
        .unwrap_or_else(|| "unknown".to_string());
    let hash = tx_hash.trim().trim_start_matches("0x");
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

pub fn save_trace_cache(path: &str, snapshots: &[StepSnapshot], result: &Option<ExecutionResultInfo>) -> Result<()> {
    std::fs::create_dir_all("cache").ok();
    #[derive(serde::Serialize)]
    struct TraceCacheRef<'a> {
        snapshots: &'a [StepSnapshot],
        result: &'a Option<ExecutionResultInfo>,
    }
    let json = serde_json::to_string_pretty(&TraceCacheRef { snapshots, result })
        .with_context(|| "serializing trace cache")?;
    std::fs::write(path, json).with_context(|| format!("writing {}", path))?;
    Ok(())
}
