//! Share links: when a recording stops, upload it and copy a link to it.
//!
//! Where it goes is a setting kept between runs, in the user's config folder,
//! since sharing to their own bucket takes keys that take a while to find and
//! paste. The keys stay on this computer; nothing else reads that file.
//! Sharing to Screenilux needs no key at all.
//!
//! The links themselves are kept too ([`Shares`]), so a recording's link is
//! there in the Play tab, next time as well, until it expires.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use eframe::egui;
use screenilux_core::package;
use screenilux_core::text::TextIndex;
use screenilux_share::{Bucket, Contents, Destination, Rights, State, Upload};
use serde::{Deserialize, Serialize};

use crate::player_view::friendly_name;
use crate::theme::{self, icon};

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
    /// This computer's id on the Screenilux storage, made the first time it
    /// is needed. What it may share is counted against it.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub device: String,
    /// The Screenilux storage, if not the usual one; for testing a local copy.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub server: String,
    pub bucket: Bucket,
}

impl Sharing {
    pub fn load() -> Self {
        read_json(SETTINGS).unwrap_or_default()
    }

    pub fn save(&self) {
        write_json(SETTINGS, self);
    }

    /// Where a finished recording goes, if sharing is on and set up. Sharing
    /// to Screenilux the first time gives this computer its id, and saves it.
    pub fn destination(&mut self) -> Option<Destination> {
        match self.mode {
            Mode::Off => None,
            Mode::Screenilux => {
                if self.device.is_empty() {
                    self.device = screenilux_share::new_device_id();
                    self.save();
                }
                Some(Destination::Screenilux {
                    server: match self.server.trim() {
                        "" => screenilux_share::PLAYER.to_string(),
                        s => s.to_string(),
                    },
                    device: self.device.clone(),
                })
            }
            Mode::Bucket if !self.bucket.is_complete() => None,
            Mode::Bucket => Some(Destination::Bucket(self.bucket.clone())),
        }
    }
}

const SETTINGS: &str = "sharing.json";
const LINKS: &str = "shares.json";

/// `%APPDATA%\Screenilux` on Windows, and `~/.config/screenilux` (or under
/// `$XDG_CONFIG_HOME`) on Linux.
fn config_dir() -> Option<PathBuf> {
    Some(if cfg!(windows) {
        PathBuf::from(std::env::var_os("APPDATA")?).join("Screenilux")
    } else if let Some(xdg) = std::env::var_os("XDG_CONFIG_HOME").filter(|v| !v.is_empty()) {
        PathBuf::from(xdg).join("screenilux")
    } else {
        PathBuf::from(std::env::var_os("HOME")?)
            .join(".config")
            .join("screenilux")
    })
}

pub(crate) fn read_json<T: for<'de> Deserialize<'de>>(name: &str) -> Option<T> {
    let path = config_dir()?.join(name);
    let json = std::fs::read_to_string(&path).ok()?;
    serde_json::from_str(&json)
        .inspect_err(
            |e| tracing::warn!(path = %path.display(), error = %e, "ignoring an unreadable file"),
        )
        .ok()
}

pub(crate) fn write_json(name: &str, value: &impl Serialize) {
    let Some(dir) = config_dir() else { return };
    let path = dir.join(name);
    let written = std::fs::create_dir_all(&dir)
        .and_then(|()| std::fs::write(&path, serde_json::to_vec_pretty(value).unwrap_or_default()));
    if let Err(e) = written {
        tracing::warn!(path = %path.display(), error = %e, "could not save");
    }
}

/// A recording's text index as it is shared. A single video goes up as
/// `screen1.mp4`, so its text is moved to that name.
fn shared_text(path: &Path, mut index: TextIndex) -> Option<Vec<u8>> {
    if !package::is_package(path) {
        for entry in &mut index.entries {
            entry.screen = "screen1.mp4".into();
        }
    }
    index.to_json().ok().map(String::into_bytes)
}

/// What to upload for a recording the app saved, with its text if it has
/// been made searchable, so the link can search and copy it too.
pub fn contents(path: &Path) -> Result<Contents, String> {
    let mut contents = recording(path)?;
    if !package::is_package(path)
        && let Some(json) =
            screenilux_player::open::stored_text(path).and_then(|i| shared_text(path, i))
    {
        contents.add_text(json);
    }
    Ok(contents)
}

fn recording(path: &Path) -> Result<Contents, String> {
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

/// A finished share, as kept between runs.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct Kept {
    link: String,
    /// When it stops working, in seconds since 1970, if it does.
    expires: Option<u64>,
    /// The right to add to the share, such as its text later.
    #[serde(default)]
    rights: Option<Rights>,
}

/// Where one recording's link stands.
#[derive(Debug, Clone, PartialEq)]
pub enum Link {
    /// Not shared.
    None,
    /// Asking for a link.
    Starting,
    /// The link works already; the page waits for the rest.
    Uploading {
        link: String,
        done: f32,
    },
    Ready {
        link: String,
        expires: Option<SystemTime>,
    },
    Failed(String),
}

enum Live {
    /// The upload, and whether the file goes once it has finished: the
    /// recording is kept in the cloud only.
    Uploading(Upload, bool),
    Refused(String),
}

/// Every recording shared from this computer and its link.
pub struct Shares {
    kept: HashMap<PathBuf, Kept>,
    live: HashMap<PathBuf, Live>,
    /// Links put on the clipboard already, so each is copied once.
    copied: Vec<String>,
}

/// The same file, however it was reached.
fn key(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

fn seconds(t: SystemTime) -> u64 {
    t.duration_since(UNIX_EPOCH).unwrap_or_default().as_secs()
}

impl Shares {
    pub fn load() -> Self {
        let now = seconds(SystemTime::now());
        let kept: HashMap<PathBuf, Kept> = read_json::<HashMap<PathBuf, Kept>>(LINKS)
            .unwrap_or_default()
            .into_iter()
            .filter(|(path, k)| k.expires.is_none_or(|e| e > now) && path.exists())
            .collect();
        Self {
            kept,
            live: HashMap::new(),
            copied: Vec::new(),
        }
    }

    /// Share `path` to `destination`, unless it is being shared already.
    /// With `remove_after`, the file is deleted from this computer once its
    /// upload has finished, never before.
    pub fn start(&mut self, path: &Path, destination: Destination, remove_after: bool) {
        let key = key(path);
        if matches!(self.live.get(&key), Some(Live::Uploading(u, _)) if !matches!(u.progress().state, State::Failed(_)))
        {
            return;
        }
        self.kept.remove(&key);
        let live = match contents(path) {
            Ok(contents) => Live::Uploading(Upload::start(destination, contents), remove_after),
            Err(e) => Live::Refused(e),
        };
        self.live.insert(key, live);
    }

    pub fn of(&self, path: &Path) -> Link {
        let key = key(path);
        match self.live.get(&key) {
            Some(Live::Refused(e)) => return Link::Failed(e.clone()),
            Some(Live::Uploading(upload, _)) => {
                let p = upload.progress();
                return match (p.state, p.link) {
                    (State::Failed(e), _) => Link::Failed(e),
                    (State::Ready, Some(link)) => Link::Ready {
                        link,
                        expires: p.expires,
                    },
                    (_, Some(link)) => Link::Uploading {
                        link,
                        done: if p.total == 0 {
                            0.0
                        } else {
                            p.uploaded as f32 / p.total as f32
                        },
                    },
                    (_, None) => Link::Starting,
                };
            }
            None => {}
        }
        match self.kept.get(&key) {
            Some(k) => Link::Ready {
                link: k.link.clone(),
                expires: k.expires.map(|s| UNIX_EPOCH + Duration::from_secs(s)),
            },
            None => Link::None,
        }
    }

    /// Keep links whose upload has finished, and return a link that has
    /// just come to exist, to put on the clipboard once.
    pub fn poll(&mut self) -> Option<String> {
        let mut fresh = None;
        let mut finished = Vec::new();
        for (path, live) in &self.live {
            let Live::Uploading(upload, remove_after) = live else {
                continue;
            };
            let p = upload.progress();
            if let Some(link) = &p.link
                && !self.copied.contains(link)
                && !matches!(p.state, State::Failed(_))
            {
                self.copied.push(link.clone());
                fresh = Some(link.clone());
            }
            if let (State::Ready, Some(link)) = (&p.state, &p.link) {
                finished.push((path.clone(), link.clone(), p.expires, p.rights.clone()));
                if *remove_after && let Err(e) = std::fs::remove_file(path) {
                    tracing::warn!(path = %path.display(), error = %e, "could not remove the uploaded recording");
                }
            }
        }
        if !finished.is_empty() {
            for (path, link, expires, rights) in finished {
                self.live.remove(&path);
                self.kept.insert(
                    path,
                    Kept {
                        link,
                        expires: expires.map(seconds),
                        rights,
                    },
                );
            }
            write_json(LINKS, &self.kept);
        }
        fresh
    }
}

/// What the person asked of a link.
pub enum Asked {
    Share,
    /// Write the recording out as a GIF.
    Export,
    /// Put the recording away and go back to the list.
    Back,
}

/// How long "Copied" shows after copying.
const COPIED_FOR: Duration = Duration::from_millis(1600);

/// The link as it reads in the window: no scheme, no key.
fn readable(link: &str) -> String {
    let without = link.split_once('#').map_or(link, |(page, _)| page);
    without
        .trim_start_matches("https://")
        .trim_start_matches("http://")
        .to_string()
}

fn when(expires: SystemTime) -> String {
    let left = expires
        .duration_since(SystemTime::now())
        .unwrap_or_default()
        .as_secs();
    match left / 86_400 {
        0 if left < 3600 => "expires within the hour".into(),
        0 => "expires today".into(),
        1 => "expires tomorrow".into(),
        days => format!("expires in {days} days"),
    }
}

/// A copy button that says "Copied" for a moment after it is used.
fn copy_button(ui: &mut egui::Ui, link: &str, primary: bool) {
    let p = theme::palette(ui);
    let id = egui::Id::NULL.with(("copied", link));
    let copied = ui
        .data(|d| d.get_temp::<Instant>(id))
        .is_some_and(|t| t.elapsed() < COPIED_FOR);
    let (label, fill, text) = match (copied, primary) {
        (true, _) => (format!("{}  Copied", icon::CHECK), p.surface, p.signal),
        (false, true) => (
            format!("{}  Copy link", icon::COPY),
            p.action,
            theme::ON_DARK,
        ),
        (false, false) => (format!("{}  Copy link", icon::COPY), p.base, p.text),
    };
    let button = egui::Button::new(egui::RichText::new(label).color(text).strong())
        .fill(fill)
        .stroke(egui::Stroke::new(
            1.0,
            if primary { fill } else { p.stroke },
        ))
        .corner_radius(egui::CornerRadius::same(10))
        .min_size(egui::vec2(0.0, 34.0));
    if ui.add(button).on_hover_text(readable(link)).clicked() {
        ui.ctx().copy_text(link.to_string());
        ui.data_mut(|d| d.insert_temp(id, Instant::now()));
    }
    if copied {
        ui.ctx().request_repaint_after(COPIED_FOR);
    }
}

fn open_button(ui: &mut egui::Ui, link: &str) {
    let p = theme::palette(ui);
    let button = egui::Button::new(egui::RichText::new(icon::OPEN).color(p.text))
        .fill(p.base)
        .stroke(egui::Stroke::new(1.0, p.stroke))
        .corner_radius(egui::CornerRadius::same(10))
        .min_size(egui::vec2(34.0, 34.0));
    if ui
        .add(button)
        .on_hover_text("Open in the browser")
        .clicked()
    {
        ui.ctx().open_url(egui::OpenUrl::new_tab(link));
    }
}

/// The link in full, for the Record tab after a recording: where it stands,
/// the link itself, and copying or opening it.
pub fn link_panel(ui: &mut egui::Ui, link: &Link) -> Option<Asked> {
    let p = theme::palette(ui);
    // A mark, then text that wraps under it rather than running off the edge.
    let status = |ui: &mut egui::Ui, mark: &str, colour: egui::Color32, text: String| {
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing.x = 6.0;
            ui.label(egui::RichText::new(mark).color(colour));
            ui.label(egui::RichText::new(text).size(13.0).color(p.weak));
        });
    };
    let mut asked = None;
    match link {
        Link::None => {}
        Link::Starting => {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label(
                    egui::RichText::new("Getting a link…")
                        .size(13.0)
                        .color(p.weak),
                );
            });
        }
        Link::Uploading { link, done } => {
            status(
                ui,
                icon::LOCK,
                p.signal,
                "Link copied. You can send it now; it plays as soon as the upload ends.".into(),
            );
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                theme::progress(ui, *done, ui.available_width() - 44.0);
                ui.label(
                    egui::RichText::new(format!("{:.0}%", done * 100.0))
                        .size(12.0)
                        .color(p.weak),
                );
            });
            ui.add_space(6.0);
            link_row(ui, link, false);
        }
        Link::Ready { link, expires } => {
            let mut text = "Ready to watch. Only people with the link can.".to_string();
            if let Some(e) = expires {
                text = format!(
                    "Ready to watch, {}. Only people with the link can.",
                    when(*e)
                );
            }
            status(ui, icon::CHECK, p.signal, text);
            ui.add_space(6.0);
            link_row(ui, link, true);
        }
        Link::Failed(e) => {
            theme::notice(ui, format!("Could not share it: {e}"));
            if ui.add(theme::outline("Try again")).clicked() {
                asked = Some(Asked::Share);
            }
        }
    }
    asked
}

/// The link across the whole width, then copy and open under it.
fn link_row(ui: &mut egui::Ui, link: &str, ready: bool) {
    let p = theme::palette(ui);
    egui::Frame::new()
        .fill(p.base)
        .stroke(egui::Stroke::new(1.0, p.stroke))
        .corner_radius(egui::CornerRadius::same(10))
        .inner_margin(egui::Margin::symmetric(10, 8))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.add(
                egui::Label::new(egui::RichText::new(readable(link)).size(13.0).color(p.weak))
                    .truncate(),
            )
            .on_hover_text(readable(link));
        });
    ui.add_space(6.0);
    ui.horizontal(|ui| {
        copy_button(ui, link, ready);
        open_button(ui, link);
    });
}

/// The link in brief, for the Play tab's title row.
pub fn link_chip(ui: &mut egui::Ui, link: &Link, can_share: bool) -> Option<Asked> {
    let p = theme::palette(ui);
    let mut asked = None;
    match link {
        Link::None if can_share => {
            if ui
                .add(theme::outline(format!("{}  Share link", icon::LINK)))
                .on_hover_text("Upload this recording and copy a link to it")
                .clicked()
            {
                asked = Some(Asked::Share);
            }
        }
        Link::None => {}
        Link::Starting => {
            ui.spinner();
        }
        Link::Uploading { link, done } => {
            open_button(ui, link);
            copy_button(ui, link, false);
            ui.label(
                egui::RichText::new(format!("Uploading {:.0}%", done * 100.0))
                    .size(13.0)
                    .color(p.weak),
            );
        }
        Link::Ready { link, expires } => {
            open_button(ui, link);
            copy_button(ui, link, true);
            if let Some(e) = expires {
                ui.label(
                    egui::RichText::new(format!("Link {}", when(*e)))
                        .size(13.0)
                        .color(p.weak),
                );
            }
        }
        Link::Failed(e) => {
            if ui
                .add(theme::outline(format!("{}  Share again", icon::WARNING)))
                .on_hover_text(format!("Could not share it: {e}"))
                .clicked()
            {
                asked = Some(Asked::Share);
            }
        }
    }
    asked
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sharing_is_off_until_set_up() {
        let mut s = Sharing::default();
        assert!(s.destination().is_none());
        s.mode = Mode::Screenilux;
        // A device id is made here; keep the test from saving it.
        s.device = "d".repeat(22);
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
            device: String::new(),
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

    #[test]
    fn a_link_reads_without_its_key() {
        assert_eq!(
            readable("https://example.dev/v/abc#k=secret"),
            "example.dev/v/abc"
        );
    }

    #[test]
    fn expiry_reads_in_days() {
        let now = SystemTime::now();
        assert_eq!(
            when(now + Duration::from_secs(7 * 86_400 + 60)),
            "expires in 7 days"
        );
        assert_eq!(
            when(now + Duration::from_secs(86_400 + 60)),
            "expires tomorrow"
        );
        assert_eq!(
            when(now + Duration::from_secs(600)),
            "expires within the hour"
        );
    }
}
