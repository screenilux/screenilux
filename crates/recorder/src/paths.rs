//! Scratch directories and collision-free output names.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

static COUNTER: AtomicU64 = AtomicU64::new(0);

/// A scratch directory that deletes itself.
#[derive(Debug)]
pub struct TempDir {
    path: PathBuf,
}

impl TempDir {
    pub fn new(prefix: &str) -> std::io::Result<Self> {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let path =
            std::env::temp_dir().join(format!("{prefix}-{}-{nanos:x}-{n}", std::process::id()));
        std::fs::create_dir_all(&path)?;
        Ok(Self { path })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

/// A path in `dir` that does not already exist.
///
/// Recording names are timestamped to the second, so stopping two recordings
/// within the same second would otherwise overwrite the first silently.
pub fn unique_path(dir: &Path, stem: &str, ext: &str) -> PathBuf {
    let name = |n: Option<u32>| -> PathBuf {
        let base = match n {
            None => stem.to_string(),
            Some(n) => format!("{stem} ({n})"),
        };
        dir.join(if ext.is_empty() {
            base
        } else {
            format!("{base}.{ext}")
        })
    };
    let first = name(None);
    if !first.exists() {
        return first;
    }
    (2..1000)
        .map(|n| name(Some(n)))
        .find(|p| !p.exists())
        .unwrap_or(first)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn temp_dirs_are_unique_and_self_cleaning() {
        let a = TempDir::new("t").unwrap();
        let b = TempDir::new("t").unwrap();
        assert_ne!(a.path(), b.path());
        let kept = a.path().to_path_buf();
        drop(a);
        assert!(!kept.exists());
        assert!(b.path().is_dir());
    }

    #[test]
    fn unique_path_sidesteps_a_same_second_collision() {
        let d = TempDir::new("u").unwrap();
        assert_eq!(
            unique_path(d.path(), "rec", "mp4"),
            d.path().join("rec.mp4")
        );
        std::fs::write(d.path().join("rec.mp4"), b"x").unwrap();
        assert_eq!(
            unique_path(d.path(), "rec", "mp4"),
            d.path().join("rec (2).mp4")
        );
    }

    #[test]
    fn unique_path_handles_extensionless_session_folders() {
        let d = TempDir::new("u").unwrap();
        std::fs::create_dir(d.path().join("s")).unwrap();
        assert_eq!(unique_path(d.path(), "s", ""), d.path().join("s (2)"));
    }
}
