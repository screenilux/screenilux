//! Turning whatever the user picked into something playable.
//!
//! Three things can be opened: a `.screenilux` package, a session folder (or
//! the `recording.json` inside one), and a plain video file. All three come
//! out as a [`Recording`] plus any text index already made for it.

use std::path::{Path, PathBuf};

use screenilux_core::package;
use screenilux_core::playback::Recording;
use screenilux_core::session::{MANIFEST_NAME, Manifest};
use screenilux_core::text::{TEXT_INDEX_NAME, TextIndex};
use screenilux_recorder::paths::TempDir;

#[derive(Debug, thiserror::Error)]
pub enum OpenError {
    #[error("{0}")]
    Package(#[from] package::PackageError),
    #[error("could not read the recording: {0}")]
    Manifest(String),
    #[error("could not play {0}: {1}")]
    Video(String, String),
    #[error("{0} is missing from the recording")]
    MissingTrack(String),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}

/// Where a recording came from, which decides where its text index is saved.
#[derive(Debug, Clone, PartialEq)]
pub enum Origin {
    Package(PathBuf),
    Folder(PathBuf),
    Video(PathBuf),
}

/// A recording ready for the player.
pub struct Opened {
    pub recording: Recording,
    pub fps: f64,
    pub text: Option<TextIndex>,
    pub origin: Origin,
    /// For the window title.
    pub title: String,
    /// A package is unpacked here for the decoders; removed on drop.
    _workspace: Option<TempDir>,
}

impl std::fmt::Debug for Opened {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Opened")
            .field("title", &self.title)
            .field("origin", &self.origin)
            .field("tracks", &self.recording.tracks.len())
            .finish()
    }
}

fn title_of(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string())
}

fn read_manifest(dir: &Path) -> Result<Manifest, OpenError> {
    let json = std::fs::read_to_string(dir.join(MANIFEST_NAME))?;
    Manifest::from_json(&json).map_err(|e| OpenError::Manifest(e.to_string()))
}

fn read_index(path: &Path) -> Option<TextIndex> {
    let json = std::fs::read_to_string(path).ok()?;
    match TextIndex::from_json(&json) {
        Ok(idx) => Some(idx),
        Err(e) => {
            tracing::warn!(path = %path.display(), error = %e, "ignoring an unreadable text index");
            None
        }
    }
}

fn check_tracks(recording: &Recording) -> Result<(), OpenError> {
    match recording.tracks.iter().find(|t| !t.path.is_file()) {
        Some(t) => Err(OpenError::MissingTrack(t.id.clone())),
        None => Ok(()),
    }
}

/// Open a package, session folder, manifest or video.
///
/// A package is unpacked to a scratch folder first, which takes a moment for
/// a long recording, so call this off the UI thread.
pub fn open(path: &Path) -> Result<Opened, OpenError> {
    if package::is_package(path) {
        let workspace = TempDir::new("screenilux-play")?;
        let manifest = package::unpack(path, workspace.path())?;
        let recording = Recording::from_manifest(&manifest, workspace.path());
        check_tracks(&recording)?;
        return Ok(Opened {
            fps: manifest.fps as f64,
            text: read_index(&workspace.path().join(TEXT_INDEX_NAME)),
            recording,
            origin: Origin::Package(path.to_path_buf()),
            title: title_of(path),
            _workspace: Some(workspace),
        });
    }

    let folder = if path.is_dir() {
        Some(path.to_path_buf())
    } else if path.file_name().is_some_and(|n| n == MANIFEST_NAME) {
        path.parent().map(Path::to_path_buf)
    } else {
        None
    };
    if let Some(dir) = folder {
        let manifest = read_manifest(&dir)?;
        let recording = Recording::from_manifest(&manifest, &dir);
        check_tracks(&recording)?;
        return Ok(Opened {
            fps: manifest.fps as f64,
            text: read_index(&dir.join(TEXT_INDEX_NAME)),
            recording,
            origin: Origin::Folder(dir.clone()),
            title: title_of(&dir),
            _workspace: None,
        });
    }

    let info =
        screenilux_win::probe(path).map_err(|e| OpenError::Video(title_of(path), e.to_string()))?;
    let recording = Recording::single(path, info.width, info.height, info.duration, info.has_audio);
    Ok(Opened {
        // Only used as a seek tolerance for a plain video; 30 is the default
        // Screenilux records at.
        fps: 30.0,
        text: cache_path(path).and_then(|p| read_index(&p)),
        recording,
        origin: Origin::Video(path.to_path_buf()),
        title: title_of(path),
        _workspace: None,
    })
}

/// Save a text index where the recording keeps it. Returns where it went.
///
/// Packages and session folders carry their own index, so it travels with
/// them. A plain video has nowhere to put one without leaving a stray file
/// beside it, so its index is cached per user instead.
pub fn save_text(origin: &Origin, index: &TextIndex) -> Result<PathBuf, OpenError> {
    match origin {
        Origin::Package(p) => {
            package::write_text_index(p, index)?;
            Ok(p.clone())
        }
        Origin::Folder(dir) => {
            let dest = dir.join(TEXT_INDEX_NAME);
            write_json(&dest, index)?;
            Ok(dest)
        }
        Origin::Video(v) => {
            let dest = cache_path(v).ok_or_else(|| {
                OpenError::Io(std::io::Error::other("no per-user folder to cache text in"))
            })?;
            write_json(&dest, index)?;
            Ok(dest)
        }
    }
}

fn write_json(dest: &Path, index: &TextIndex) -> Result<(), OpenError> {
    let json = index
        .to_json()
        .map_err(|e| OpenError::Manifest(e.to_string()))?;
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(dest, json)?;
    Ok(())
}

/// Where a plain video's text index is cached: keyed by path, size and
/// modification time, so editing or replacing the file invalidates it.
fn cache_path(video: &Path) -> Option<PathBuf> {
    let meta = std::fs::metadata(video).ok()?;
    let modified = meta
        .modified()
        .ok()
        .and_then(|m| m.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let key = format!(
        "{}|{}|{modified}",
        video.canonicalize().ok()?.display(),
        meta.len()
    );
    let base = std::env::var_os("LOCALAPPDATA").map(PathBuf::from)?;
    Some(
        base.join("Screenilux")
            .join("text")
            .join(format!("{:016x}.json", fnv1a(key.as_bytes()))),
    )
}

/// A stable hash for cache file names. `std`'s hasher is randomly seeded per
/// process, which would orphan every cache entry on restart.
fn fnv1a(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325, |h, &b| {
        (h ^ b as u64).wrapping_mul(0x0000_0100_0000_01b3)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use screenilux_core::session::{Role, Screen};
    use screenilux_core::text::{Line, Page, Word};

    fn scratch(name: &str) -> TempDir {
        TempDir::new(&format!("screenilux-open-{name}")).unwrap()
    }

    fn session(dir: &Path) -> Manifest {
        let mut m = Manifest::new("2026-10-31T12:00:00".into(), 3.0, 30, "x".into(), false);
        for i in 1..=2 {
            let file = format!("screen{i}.mp4");
            std::fs::write(dir.join(&file), b"video").unwrap();
            m.screens.push(Screen {
                file,
                label: format!("Screen {i}"),
                width: 1280,
                height: 720,
                left: 0,
                top: 0,
                primary: i == 2,
                audio: false,
                role: if i == 2 {
                    Role::Primary
                } else {
                    Role::Secondary
                },
                order: 0,
            });
        }
        m.assign_order();
        std::fs::write(dir.join(MANIFEST_NAME), m.to_json().unwrap()).unwrap();
        m
    }

    fn index() -> TextIndex {
        let mut idx = TextIndex::new(2.0);
        idx.record(
            "screen1.mp4",
            0.0,
            Page {
                lines: vec![Line {
                    words: vec![Word {
                        text: "hello".into(),
                        x: 0.0,
                        y: 0.0,
                        w: 1.0,
                        h: 1.0,
                    }],
                }],
            },
        );
        idx
    }

    #[test]
    fn a_folder_opens_with_its_tracks_in_display_order() {
        let s = scratch("folder");
        session(s.path());
        let o = open(s.path()).unwrap();
        assert_eq!(o.recording.tracks.len(), 2);
        assert_eq!(o.recording.tracks[0].id, "screen2.mp4", "primary first");
        assert_eq!(o.origin, Origin::Folder(s.path().to_path_buf()));
        assert!(o.text.is_none());
    }

    #[test]
    fn the_manifest_itself_opens_its_folder() {
        let s = scratch("manifest");
        session(s.path());
        let o = open(&s.path().join(MANIFEST_NAME)).unwrap();
        assert_eq!(o.origin, Origin::Folder(s.path().to_path_buf()));
    }

    #[test]
    fn a_folder_missing_a_video_says_which() {
        let s = scratch("missing");
        session(s.path());
        std::fs::remove_file(s.path().join("screen1.mp4")).unwrap();
        match open(s.path()) {
            Err(OpenError::MissingTrack(id)) => assert_eq!(id, "screen1.mp4"),
            other => panic!("expected a missing track, got {other:?}"),
        }
    }

    #[test]
    fn a_package_unpacks_and_its_text_round_trips() {
        let s = scratch("package");
        let folder = s.path().join("rec");
        std::fs::create_dir_all(&folder).unwrap();
        session(&folder);
        let pkg = package::package_path(s.path(), "rec");
        package::pack(&folder, &pkg).unwrap();

        let o = open(&pkg).unwrap();
        assert_eq!(o.origin, Origin::Package(pkg.clone()));
        assert!(o.recording.tracks.iter().all(|t| t.path.is_file()));
        assert!(o.text.is_none());

        save_text(&o.origin, &index()).unwrap();
        drop(o);
        let again = open(&pkg).unwrap();
        assert_eq!(again.text, Some(index()));
    }

    #[test]
    fn a_folder_keeps_its_text_beside_the_manifest() {
        let s = scratch("foldertext");
        session(s.path());
        let o = open(s.path()).unwrap();
        let at = save_text(&o.origin, &index()).unwrap();
        assert_eq!(at, s.path().join(TEXT_INDEX_NAME));
        assert_eq!(open(s.path()).unwrap().text, Some(index()));
    }

    #[test]
    fn something_unplayable_is_an_error() {
        let s = scratch("junk");
        let p = s.path().join("junk.mp4");
        std::fs::write(&p, b"not a video").unwrap();
        assert!(matches!(open(&p), Err(OpenError::Video(..))));
    }

    #[test]
    fn the_cache_key_is_stable() {
        assert_eq!(fnv1a(b"abc"), fnv1a(b"abc"));
        assert_ne!(fnv1a(b"abc"), fnv1a(b"abd"));
    }
}
