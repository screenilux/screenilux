//! What a player needs to know, independent of how frames are decoded.
//!
//! A recording is one or more tracks plus, for several screens, the pointer
//! focus timeline. This module turns that into "which screens go where at time
//! t" and keeps the playback position, so the windowing and decoding code
//! above it stays thin.

use std::path::{Path, PathBuf};
use std::time::Instant;

use crate::session::{CameraVideo, Focus, Manifest};

/// One screen's video.
#[derive(Debug, Clone, PartialEq)]
pub struct Track {
    /// Where the decoder reads it from.
    pub path: PathBuf,
    /// The manifest's name for it, which the focus timeline and the text
    /// index refer to.
    pub id: String,
    pub label: String,
    pub width: u32,
    pub height: u32,
    /// Carries the microphone. At most one track does.
    pub audio: bool,
}

/// A recording ready to play: tracks in display order.
#[derive(Debug, Clone, PartialEq)]
pub struct Recording {
    pub tracks: Vec<Track>,
    pub duration: f64,
    pub focus: Option<Focus>,
    /// The camera's own video and where its bubble sat, when it was
    /// recorded apart from the screens rather than into them.
    pub camera: Option<Camera>,
}

/// The camera, recorded to a video of its own.
#[derive(Debug, Clone, PartialEq)]
pub struct Camera {
    /// Where the decoder reads it from.
    pub path: PathBuf,
    pub video: CameraVideo,
}

impl Recording {
    /// Tracks from a session manifest whose files sit in `dir`.
    pub fn from_manifest(manifest: &Manifest, dir: &Path) -> Self {
        let tracks = manifest
            .by_order()
            .into_iter()
            .map(|s| Track {
                path: dir.join(&s.file),
                id: s.file.clone(),
                label: s.label.clone(),
                width: s.width,
                height: s.height,
                audio: s.audio,
            })
            .collect();
        Self {
            tracks,
            duration: manifest.duration,
            focus: manifest.focus.clone(),
            camera: manifest.camera_video.as_ref().map(|v| Camera {
                path: dir.join(&v.file),
                video: v.clone(),
            }),
        }
    }

    /// A plain video file: one track, nothing to switch between.
    pub fn single(path: &Path, width: u32, height: u32, duration: f64, audio: bool) -> Self {
        let id = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        Self {
            tracks: vec![Track {
                path: path.to_path_buf(),
                id,
                label: "Screen".into(),
                width,
                height,
                audio,
            }],
            duration,
            focus: None,
            camera: None,
        }
    }

    pub fn is_multi(&self) -> bool {
        self.tracks.len() > 1
    }

    /// The track auto-switch shows at `t`. Falls back to the first track,
    /// which is the primary, before the timeline's first entry or when there
    /// is no timeline.
    pub fn active_at(&self, t: f64) -> usize {
        self.focus
            .as_ref()
            .and_then(|f| f.screen_at(t))
            .and_then(|id| self.tracks.iter().position(|tr| tr.id == id))
            .unwrap_or(0)
    }

    /// Spans of the timeline as `(start, end, track)`, for drawing which
    /// screen is shown when.
    pub fn focus_spans(&self) -> Vec<(f64, f64, usize)> {
        let Some(focus) = &self.focus else {
            return vec![(0.0, self.duration, 0)];
        };
        let mut spans: Vec<(f64, f64, usize)> = Vec::new();
        let mut start = 0.0;
        let mut current = self.active_at(0.0);
        for entry in &focus.timeline {
            let Some(track) = self.tracks.iter().position(|t| t.id == entry.screen) else {
                continue;
            };
            if track == current {
                continue;
            }
            if entry.t > start {
                spans.push((start, entry.t, current));
            }
            start = entry.t;
            current = track;
        }
        if self.duration > start {
            spans.push((start, self.duration, current));
        }
        spans
    }
}

/// How several screens share the view.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Layout {
    /// One screen at a time, following the recorded pointer.
    #[default]
    Auto,
    /// All screens in a row.
    SideBySide,
    /// All screens in a column.
    Stacked,
}

impl Layout {
    pub const ALL: [Layout; 3] = [Layout::Auto, Layout::SideBySide, Layout::Stacked];

    pub fn label(self) -> &'static str {
        match self {
            Layout::Auto => "Auto switch",
            Layout::SideBySide => "Side by side",
            Layout::Stacked => "Stacked",
        }
    }
}

/// A rectangle in view coordinates. Floats, because views are scaled.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Area {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl Area {
    pub const fn new(x: f32, y: f32, w: f32, h: f32) -> Self {
        Self { x, y, w, h }
    }

    pub fn contains(&self, x: f32, y: f32) -> bool {
        x >= self.x && x < self.x + self.w && y >= self.y && y < self.y + self.h
    }
}

/// Where a track is drawn.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Placement {
    pub track: usize,
    pub area: Area,
}

/// Fit the tracks into `view`.
///
/// Every screen keeps its aspect ratio. In a row they share a height and in a
/// column a width, so a 1440p screen beside a 1080p one is shown at the same
/// physical size rather than squeezed into an equal-width cell. The result is
/// centred in `view`.
pub fn arrange(
    sizes: &[(u32, u32)],
    layout: Layout,
    active: usize,
    view: Area,
    gap: f32,
) -> Vec<Placement> {
    if sizes.is_empty() || view.w <= 0.0 || view.h <= 0.0 {
        return Vec::new();
    }
    let aspect = |i: usize| {
        let (w, h) = sizes[i];
        if h == 0 { 1.0 } else { w as f32 / h as f32 }
    };

    let single = sizes.len() == 1 || layout == Layout::Auto;
    if single {
        let track = active.min(sizes.len() - 1);
        let a = aspect(track);
        let (w, h) = if view.w / view.h > a {
            (view.h * a, view.h)
        } else {
            (view.w, view.w / a)
        };
        return vec![Placement {
            track,
            area: Area::new(
                view.x + (view.w - w) / 2.0,
                view.y + (view.h - h) / 2.0,
                w,
                h,
            ),
        }];
    }

    let n = sizes.len();
    let gaps = gap * (n - 1) as f32;
    match layout {
        Layout::SideBySide => {
            let sum: f32 = (0..n).map(aspect).sum();
            let h = view.h.min((view.w - gaps).max(0.0) / sum);
            let total = h * sum + gaps;
            let mut x = view.x + (view.w - total) / 2.0;
            let y = view.y + (view.h - h) / 2.0;
            (0..n)
                .map(|i| {
                    let w = h * aspect(i);
                    let p = Placement {
                        track: i,
                        area: Area::new(x, y, w, h),
                    };
                    x += w + gap;
                    p
                })
                .collect()
        }
        Layout::Stacked => {
            let sum: f32 = (0..n).map(|i| 1.0 / aspect(i)).sum();
            let w = view.w.min((view.h - gaps).max(0.0) / sum);
            let total = w * sum + gaps;
            let x = view.x + (view.w - w) / 2.0;
            let mut y = view.y + (view.h - total) / 2.0;
            (0..n)
                .map(|i| {
                    let h = w / aspect(i);
                    let p = Placement {
                        track: i,
                        area: Area::new(x, y, w, h),
                    };
                    y += h + gap;
                    p
                })
                .collect()
        }
        Layout::Auto => unreachable!("handled above"),
    }
}

/// The playback position, driven by the wall clock while playing.
///
/// Time is passed in rather than read, so behaviour around pause, seek and the
/// end of the recording is tested without sleeping.
#[derive(Debug, Clone)]
pub struct PlaybackClock {
    duration: f64,
    /// Position when `anchor` was taken.
    base: f64,
    /// Set while playing.
    anchor: Option<Instant>,
}

impl PlaybackClock {
    pub fn new(duration: f64) -> Self {
        Self {
            duration: duration.max(0.0),
            base: 0.0,
            anchor: None,
        }
    }

    pub fn duration(&self) -> f64 {
        self.duration
    }

    pub fn is_playing(&self) -> bool {
        self.anchor.is_some()
    }

    /// Seconds into the recording, clamped to its length.
    pub fn position(&self, now: Instant) -> f64 {
        let t = match self.anchor {
            Some(a) => self.base + now.saturating_duration_since(a).as_secs_f64(),
            None => self.base,
        };
        t.clamp(0.0, self.duration)
    }

    /// True once playback has run off the end. The caller pauses on this, so
    /// the clock is not left "playing" at a position that cannot advance.
    pub fn ended(&self, now: Instant) -> bool {
        self.is_playing() && self.position(now) >= self.duration
    }

    pub fn play(&mut self, now: Instant) {
        if self.anchor.is_some() {
            return;
        }
        // Playing from the very end starts again, as every player does.
        if self.base >= self.duration {
            self.base = 0.0;
        }
        self.anchor = Some(now);
    }

    pub fn pause(&mut self, now: Instant) {
        self.base = self.position(now);
        self.anchor = None;
    }

    pub fn seek(&mut self, t: f64, now: Instant) {
        self.base = t.clamp(0.0, self.duration);
        if self.anchor.is_some() {
            self.anchor = Some(now);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::{FocusEntry, Role, Screen};
    use std::time::Duration;

    fn manifest() -> Manifest {
        let mut m = Manifest::new("2026-10-31T12:00:00".into(), 10.0, 30, "x".into(), false);
        for (i, primary) in [(1, false), (2, true)] {
            m.screens.push(Screen {
                file: format!("screen{i}.mp4"),
                label: format!("Screen {i}"),
                width: 1920,
                height: 1080,
                left: if primary { 0 } else { -1920 },
                top: 0,
                primary,
                audio: primary,
                role: if primary {
                    Role::Primary
                } else {
                    Role::Secondary
                },
                order: 0,
            });
        }
        m.assign_order();
        m.focus = Some(Focus::new(
            0.6,
            vec![
                FocusEntry {
                    t: 0.0,
                    screen: "screen2.mp4".into(),
                },
                FocusEntry {
                    t: 4.0,
                    screen: "screen1.mp4".into(),
                },
                FocusEntry {
                    t: 7.5,
                    screen: "screen2.mp4".into(),
                },
            ],
        ));
        m
    }

    #[test]
    fn tracks_follow_display_order_with_the_primary_first() {
        let r = Recording::from_manifest(&manifest(), Path::new("C:/rec"));
        assert_eq!(r.tracks[0].id, "screen2.mp4");
        assert!(r.tracks[0].audio);
        assert_eq!(r.tracks[1].path, Path::new("C:/rec").join("screen1.mp4"));
    }

    #[test]
    fn auto_switch_follows_the_timeline() {
        let r = Recording::from_manifest(&manifest(), Path::new("."));
        assert_eq!(r.active_at(0.0), 0);
        assert_eq!(r.active_at(3.99), 0);
        assert_eq!(r.active_at(4.0), 1);
        assert_eq!(r.active_at(8.0), 0);
    }

    #[test]
    fn without_a_timeline_the_primary_is_shown() {
        let mut m = manifest();
        m.focus = None;
        let r = Recording::from_manifest(&m, Path::new("."));
        assert_eq!(r.active_at(5.0), 0);
        assert_eq!(r.focus_spans(), vec![(0.0, 10.0, 0)]);
    }

    #[test]
    fn focus_spans_cover_the_whole_recording() {
        let r = Recording::from_manifest(&manifest(), Path::new("."));
        assert_eq!(
            r.focus_spans(),
            vec![(0.0, 4.0, 0), (4.0, 7.5, 1), (7.5, 10.0, 0)]
        );
    }

    #[test]
    fn a_timeline_naming_a_missing_file_is_ignored() {
        let mut m = manifest();
        m.focus.as_mut().unwrap().timeline[1].screen = "gone.mp4".into();
        let r = Recording::from_manifest(&m, Path::new("."));
        assert_eq!(r.active_at(5.0), 0);
    }

    #[test]
    fn a_single_file_is_one_track() {
        let r = Recording::single(Path::new("C:/v/clip.mp4"), 1280, 720, 3.0, true);
        assert!(!r.is_multi());
        assert_eq!(r.tracks[0].id, "clip.mp4");
        assert_eq!(r.active_at(2.0), 0);
    }

    const VIEW: Area = Area::new(0.0, 0.0, 1000.0, 600.0);

    #[test]
    fn auto_shows_one_screen_letterboxed() {
        let p = arrange(&[(1920, 1080), (1920, 1080)], Layout::Auto, 1, VIEW, 8.0);
        assert_eq!(p.len(), 1);
        assert_eq!(p[0].track, 1);
        let a = p[0].area;
        assert!((a.w - 1000.0).abs() < 0.01);
        assert!((a.h - 562.5).abs() < 0.01);
        assert!((a.y - 18.75).abs() < 0.01, "centred vertically");
    }

    #[test]
    fn side_by_side_shares_a_height_and_fits() {
        let sizes = [(1920, 1080), (2560, 1440), (1080, 1920)];
        let p = arrange(&sizes, Layout::SideBySide, 0, VIEW, 10.0);
        assert_eq!(p.len(), 3);
        let h = p[0].area.h;
        assert!(p.iter().all(|pl| (pl.area.h - h).abs() < 0.01));
        let right = p[2].area.x + p[2].area.w;
        assert!(p[0].area.x >= 0.0 && right <= 1000.01);
        // order is preserved and screens do not overlap
        assert!(p[1].area.x >= p[0].area.x + p[0].area.w);
        // aspect is kept
        assert!((p[2].area.w / p[2].area.h - 1080.0 / 1920.0).abs() < 0.001);
    }

    #[test]
    fn stacked_shares_a_width_and_fits() {
        let p = arrange(
            &[(1920, 1080), (1920, 1080)],
            Layout::Stacked,
            0,
            VIEW,
            10.0,
        );
        assert!((p[0].area.w - p[1].area.w).abs() < 0.01);
        let bottom = p[1].area.y + p[1].area.h;
        assert!(p[0].area.y >= 0.0 && bottom <= 600.01);
        assert!(p[1].area.y >= p[0].area.y + p[0].area.h);
    }

    #[test]
    fn a_single_track_ignores_the_split_layouts() {
        let p = arrange(&[(1280, 720)], Layout::SideBySide, 0, VIEW, 8.0);
        assert_eq!(p.len(), 1);
    }

    #[test]
    fn an_empty_view_places_nothing() {
        assert!(arrange(&[(1, 1)], Layout::Auto, 0, Area::default(), 0.0).is_empty());
    }

    #[test]
    fn the_clock_advances_only_while_playing() {
        let t0 = Instant::now();
        let mut c = PlaybackClock::new(10.0);
        assert_eq!(c.position(t0 + Duration::from_secs(3)), 0.0);
        c.play(t0);
        assert!((c.position(t0 + Duration::from_secs(2)) - 2.0).abs() < 1e-9);
        c.pause(t0 + Duration::from_secs(2));
        assert!((c.position(t0 + Duration::from_secs(9)) - 2.0).abs() < 1e-9);
    }

    #[test]
    fn seeking_while_playing_continues_from_the_new_spot() {
        let t0 = Instant::now();
        let mut c = PlaybackClock::new(10.0);
        c.play(t0);
        c.seek(6.0, t0 + Duration::from_secs(1));
        assert!((c.position(t0 + Duration::from_secs(2)) - 7.0).abs() < 1e-9);
    }

    #[test]
    fn the_clock_stops_at_the_end_and_replays_from_the_start() {
        let t0 = Instant::now();
        let mut c = PlaybackClock::new(2.0);
        c.play(t0);
        let late = t0 + Duration::from_secs(5);
        assert_eq!(c.position(late), 2.0);
        assert!(c.ended(late));
        c.pause(late);
        c.play(late);
        assert_eq!(c.position(late), 0.0, "play at the end starts over");
    }

    #[test]
    fn seeks_are_clamped() {
        let t0 = Instant::now();
        let mut c = PlaybackClock::new(5.0);
        c.seek(-3.0, t0);
        assert_eq!(c.position(t0), 0.0);
        c.seek(99.0, t0);
        assert_eq!(c.position(t0), 5.0);
    }
}
