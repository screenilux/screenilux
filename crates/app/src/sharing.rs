//! Share links: when a recording stops, upload it and copy a link to it.
//!
//! Where it goes is a setting kept between runs, in the user's config folder,
//! since it holds keys that take a while to find and paste: the Screenilux
//! upload key, or the access keys of the user's own bucket. The keys stay on
//! this computer; nothing else reads that file.

use std::path::{Path, PathBuf};

use screenilux_core::package;
use screenilux_share::{Bucket, Contents, Destination};
use serde::{Deserialize, Serialize};

use crate::player_view::friendly_name;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    #[default]
    Off,
    Screenilux,
    Bucket,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Sharing {
    pub mode: Mode,
    /// The Screenilux upload key.
    pub key: String,
    /// The Screenilux storage, if not the usual one; for testing a local copy.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub server: String,
    pub bucket: Bucket,
}

impl Sharing {
    pub fn load() -> Self {
        let Some(path) = config_path() else {
            return Self::default();
        };
        match std::fs::read_to_string(&path) {
            Ok(json) => serde_json::from_str(&json).unwrap_or_else(|e| {
                tracing::warn!(path = %path.display(), error = %e, "ignoring unreadable sharing settings");
                Self::default()
            }),
            Err(_) => Self::default(),
        }
    }

    pub fn save(&self) {
        let Some(path) = config_path() else { return };
        let written = path
            .parent()
            .map_or(Ok(()), std::fs::create_dir_all)
            .and_then(|()| {
                std::fs::write(&path, serde_json::to_vec_pretty(self).unwrap_or_default())
            });
        if let Err(e) = written {
            tracing::warn!(path = %path.display(), error = %e, "could not save sharing settings");
        }
    }

    /// Where a finished recording goes, if sharing is on and set up.
    pub fn destination(&self) -> Option<Destination> {
        match self.mode {
            Mode::Off => None,
            Mode::Screenilux if self.key.trim().is_empty() => None,
            Mode::Screenilux => Some(Destination::Screenilux {
                server: match self.server.trim() {
                    "" => screenilux_share::PLAYER.to_string(),
                    s => s.to_string(),
                },
                key: self.key.clone(),
            }),
            Mode::Bucket if !self.bucket.is_complete() => None,
            Mode::Bucket => Some(Destination::Bucket(self.bucket.clone())),
        }
    }
}

/// `%APPDATA%\Screenilux\sharing.json` on Windows, and
/// `~/.config/screenilux/sharing.json` (or under `$XDG_CONFIG_HOME`) on Linux.
fn config_path() -> Option<PathBuf> {
    let dir = if cfg!(windows) {
        PathBuf::from(std::env::var_os("APPDATA")?).join("Screenilux")
    } else if let Some(xdg) = std::env::var_os("XDG_CONFIG_HOME").filter(|v| !v.is_empty()) {
        PathBuf::from(xdg).join("screenilux")
    } else {
        PathBuf::from(std::env::var_os("HOME")?)
            .join(".config")
            .join("screenilux")
    };
    Some(dir.join("sharing.json"))
}

/// What to upload for a recording the app just saved.
pub fn contents(path: &Path) -> Result<Contents, String> {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let title = friendly_name(&name);
    if package::is_package(path) {
        return Contents::of_package(path, title).map_err(|e| e.to_string());
    }
    let info = screenilux_platform::probe(path).map_err(|e| e.to_string())?;
    Contents::of_video(
        path,
        title,
        (info.width, info.height),
        info.duration,
        info.has_audio,
    )
    .map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sharing_is_off_until_set_up() {
        let mut s = Sharing::default();
        assert!(s.destination().is_none());
        s.mode = Mode::Screenilux;
        assert!(s.destination().is_none(), "no key yet");
        s.key = "k".into();
        assert!(
            matches!(s.destination(), Some(Destination::Screenilux { server, .. }) if server == screenilux_share::PLAYER)
        );
        s.mode = Mode::Bucket;
        assert!(s.destination().is_none(), "the bucket is not filled in");
    }

    #[test]
    fn settings_read_back_and_tolerate_missing_fields() {
        let s = Sharing {
            mode: Mode::Bucket,
            key: "k".into(),
            server: String::new(),
            bucket: Bucket {
                bucket: "b".into(),
                ..Bucket::default()
            },
        };
        let json = serde_json::to_string(&s).unwrap();
        assert!(!json.contains("server"));
        assert_eq!(serde_json::from_str::<Sharing>(&json).unwrap(), s);
        assert_eq!(
            serde_json::from_str::<Sharing>(r#"{"mode":"screenilux"}"#)
                .unwrap()
                .mode,
            Mode::Screenilux
        );
    }
}
