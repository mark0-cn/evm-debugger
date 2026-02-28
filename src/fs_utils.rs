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

#[cfg(test)]
mod tests {
    use super::write_atomic;

    #[test]
    fn write_atomic_writes_full_file() {
        let path =
            std::env::temp_dir().join(format!("evm-debugger-test-{}.txt", uuid::Uuid::new_v4()));
        let path_str = path.to_string_lossy().to_string();
        write_atomic(&path_str, "hello").unwrap();
        let out = std::fs::read_to_string(&path_str).unwrap();
        assert_eq!(out, "hello");
        let _ = std::fs::remove_file(&path_str);
    }
}
