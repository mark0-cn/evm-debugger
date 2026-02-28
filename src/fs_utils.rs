use anyhow::{Context, Result};
use std::path::Path;

pub fn write_atomic(path: &str, contents: &str) -> Result<()> {
    let p = Path::new(path);
    if let Some(parent) = p.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("create dir {}", parent.display()))?;
        }
    }
    let tmp_path = format!("{}.tmp.{}", path, uuid::Uuid::new_v4());
    std::fs::write(&tmp_path, contents).with_context(|| format!("writing temp {}", tmp_path))?;
    std::fs::rename(&tmp_path, path)
        .with_context(|| format!("renaming {} -> {}", tmp_path, path))?;
    Ok(())
}
