//! Small filesystem helpers.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

static COUNTER: AtomicU64 = AtomicU64::new(0);

/// A scratch directory that deletes itself.
///
/// Hand-rolled rather than pulling in a crate: the needs are a unique name and
/// cleanup on drop, and segments are large enough that leaking a directory
/// would be noticed in gigabytes.
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
/// The recording name is timestamped to the second, so stopping two recordings
/// within the same second would otherwise overwrite the first without warning —
/// ffmpeg is invoked with `-y`, and the session folder is created with
/// create-if-missing.
pub fn unique_path(dir: &Path, stem: &str, ext: &str) -> PathBuf {
    let first = dir.join(if ext.is_empty() {
        stem.to_string()
    } else {
        format!("{stem}.{ext}")
    });
    if !first.exists() {
        return first;
    }
    for n in 2..1000 {
        let candidate = dir.join(if ext.is_empty() {
            format!("{stem} ({n})")
        } else {
            format!("{stem} ({n}).{ext}")
        });
        if !candidate.exists() {
            return candidate;
        }
    }
    first
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
    fn unique_path_leaves_a_free_name_alone() {
        let d = TempDir::new("u").unwrap();
        assert_eq!(
            unique_path(d.path(), "recording", "mp4"),
            d.path().join("recording.mp4")
        );
    }

    #[test]
    fn unique_path_sidesteps_a_same_second_collision() {
        let d = TempDir::new("u").unwrap();
        std::fs::write(d.path().join("recording.mp4"), b"x").unwrap();
        assert_eq!(
            unique_path(d.path(), "recording", "mp4"),
            d.path().join("recording (2).mp4")
        );

        std::fs::write(d.path().join("recording (2).mp4"), b"x").unwrap();
        assert_eq!(
            unique_path(d.path(), "recording", "mp4"),
            d.path().join("recording (3).mp4")
        );
    }

    #[test]
    fn unique_path_handles_extensionless_session_folders() {
        let d = TempDir::new("u").unwrap();
        std::fs::create_dir(d.path().join("session")).unwrap();
        assert_eq!(
            unique_path(d.path(), "session", ""),
            d.path().join("session (2)")
        );
    }
}
