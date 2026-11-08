//! What the window is showing and what the user has chosen.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::mpsc::{Receiver, channel};

use screenilux_core::EffectOptions;
use screenilux_core::geometry::Monitor;

/// The camera bubble, in pixels: a circle big enough to read a face, small
/// enough to leave the screen readable.
/// The bubble's long side. The short side follows the camera's own
/// shape once a frame has arrived; until then this is a sensible
/// widescreen guess, which is what nearly every webcam is.
pub const CAMERA_SIZE: (u32, u32) = (320, 180);
pub const CAMERA_MARGIN: i32 = 28;
/// How much higher than the bottom margin the bubble opens, clear of the
/// taskbar and of whatever sits along a window's bottom edge.
pub const CAMERA_LIFT: i32 = 160;
pub const FPS_CHOICES: [u32; 5] = [10, 15, 24, 30, 60];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    Idle,
    Recording,
    Paused,
    /// Stopping and writing files, which can take a moment on a long
    /// recording. The window stays responsive and says what it is doing.
    Saving,
}

/// Results of the slow startup probes, which run off the UI thread.
///
/// Enumerating a camera through Media Foundation takes a moment. Doing it
/// inline would hold the window back before it appeared.
pub struct Probes {
    pub has_microphone: bool,
    pub has_system_sound: bool,
    pub cameras: Vec<String>,
    pub ready: bool,
    rx: Option<Receiver<ProbeResult>>,
}

struct ProbeResult {
    has_microphone: bool,
    has_system_sound: bool,
    cameras: Vec<String>,
}

impl Probes {
    pub fn start() -> Self {
        let (tx, rx) = channel();
        std::thread::Builder::new()
            .name("screenilux-probe".into())
            .spawn(move || {
                // Both come from Media Foundation. Enumerating a camera takes
                // a moment, which is why this is off the UI thread at all.
                let _ = tx.send(ProbeResult {
                    has_microphone: screenilux_platform::has_microphone(),
                    has_system_sound: screenilux_platform::has_system_sound(),
                    cameras: screenilux_platform::cameras(),
                });
            })
            .ok();
        Self {
            has_microphone: false,
            has_system_sound: false,
            cameras: Vec::new(),
            ready: false,
            rx: Some(rx),
        }
    }

    /// Fold in results if they have arrived. Cheap to call every frame.
    pub fn poll(&mut self) {
        let Some(rx) = &self.rx else { return };
        if let Ok(r) = rx.try_recv() {
            self.has_microphone = r.has_microphone;
            self.has_system_sound = r.has_system_sound;
            self.cameras = r.cameras;
            self.ready = true;
            self.rx = None;
        }
    }
}

/// Everything the user can choose.
/// What is kept between runs besides sharing (see `sharing`): where
/// recordings are saved, and whether one stays here once it is shared.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct Prefs {
    /// Where recordings are saved, if not the usual Videos folder.
    pub out_dir: Option<PathBuf>,
    /// Keep a recording on this computer once it is shared. Without sharing,
    /// recordings always stay.
    pub keep_local: bool,
    /// Whether the Edit tab's tour has been seen. Finishing it and skipping
    /// it both count: it is offered once, not until it is completed.
    #[serde(default)]
    pub toured: bool,
}

impl Default for Prefs {
    fn default() -> Self {
        Self {
            out_dir: None,
            keep_local: true,
            toured: false,
        }
    }
}

impl Prefs {
    const FILE: &str = "settings.json";

    pub fn load() -> Self {
        crate::sharing::read_json(Self::FILE).unwrap_or_default()
    }

    pub fn save(&self) {
        crate::sharing::write_json(Self::FILE, self);
    }
}

pub struct Settings {
    pub monitors: Vec<Monitor>,
    /// Parallel to `monitors`.
    pub selected: Vec<bool>,
    pub whole_desktop: bool,
    pub fps: u32,
    /// Shrink the window to the recording bar while recording.
    pub shrink: bool,
    pub effects: EffectOptions,
    pub microphone: bool,
    /// What the computer plays: video, calls, app sounds.
    pub system_sound: bool,
    /// A quiet click in the recording for each mouse click.
    pub click_sound: bool,
    pub camera_device: usize,
    pub out_dir: PathBuf,
}

impl Settings {
    pub fn new() -> Self {
        let monitors = screenilux_platform::monitors().unwrap_or_default();
        let selected = monitors.iter().map(|m| m.is_primary).collect();
        Self {
            monitors,
            selected,
            whole_desktop: false,
            fps: 60,
            shrink: true,
            effects: EffectOptions::default(),
            microphone: false,
            system_sound: true,
            click_sound: true,
            camera_device: 0,
            out_dir: default_output_dir(),
        }
    }

    /// Re-enumerate displays, keeping ticks on screens that are still there.
    pub fn refresh_monitors(&mut self) {
        let kept: Vec<String> = self
            .monitors
            .iter()
            .zip(&self.selected)
            .filter(|&(_, &on)| on)
            .map(|(m, _)| m.label.clone())
            .collect();
        self.monitors = screenilux_platform::monitors().unwrap_or_default();
        self.selected = self
            .monitors
            .iter()
            .map(|m| {
                if kept.is_empty() {
                    m.is_primary
                } else {
                    kept.contains(&m.label)
                }
            })
            .collect();
        if !self.selected.iter().any(|&on| on)
            && let Some(first) = self.selected.first_mut()
        {
            *first = true;
        }
    }

    /// The screens a recording would capture.
    pub fn targets(&self) -> Vec<Monitor> {
        if self.whole_desktop
            && self.monitors.len() > 1
            && let Some(all) = screenilux_core::virtual_bounds(&self.monitors)
        {
            return vec![Monitor::new("Whole desktop", all, true)];
        }
        self.monitors
            .iter()
            .zip(&self.selected)
            .filter(|&(_, &on)| on)
            .map(|(m, _)| m.clone())
            .collect()
    }

    pub fn camera_dimensions(&self) -> (u32, u32) {
        CAMERA_SIZE
    }

    /// Bottom-right of the primary screen, inset by a margin and lifted a
    /// little further off the bottom.
    pub fn default_bubble(&self) -> screenilux_core::Rect {
        let (w, h) = self.camera_dimensions();
        let target = self
            .monitors
            .iter()
            .find(|m| m.is_primary)
            .or_else(|| self.monitors.first());
        match target {
            Some(m) => screenilux_core::Rect::new(
                m.bounds.left + m.bounds.width as i32 - w as i32 - CAMERA_MARGIN,
                m.bounds.top + m.bounds.height as i32 - h as i32 - CAMERA_MARGIN - CAMERA_LIFT,
                w,
                h,
            ),
            None => screenilux_core::Rect::new(CAMERA_MARGIN, CAMERA_MARGIN, w, h),
        }
    }
}

fn default_output_dir() -> PathBuf {
    std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
        .join("Videos")
        .join("ScreenRec")
}

/// Shared runtime handles the UI needs.
pub struct Runtime {
    pub hook: screenilux_platform::InputHook,
    pub camera: Option<screenilux_platform::CameraHandle>,
    pub camera_feed: Option<Arc<screenilux_platform::CameraFeed>>,
}

impl Runtime {
    pub fn new() -> Self {
        Self {
            hook: screenilux_platform::InputHook::start(),
            camera: None,
            camera_feed: None,
        }
    }

    pub fn start_camera(&mut self, index: u32, bubble: screenilux_core::Rect) {
        self.stop_camera();
        let handle = screenilux_platform::CameraHandle::start(index, bubble);
        self.camera_feed = Some(handle.feed());
        self.camera = Some(handle);
    }

    pub fn stop_camera(&mut self) {
        self.camera = None;
        self.camera_feed = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_output_lands_under_videos() {
        let d = default_output_dir();
        assert!(d.ends_with("Videos/ScreenRec") || d.ends_with("Videos\\ScreenRec"));
    }

    #[test]
    fn the_camera_bubble_starts_widescreen() {
        let (w, h) = CAMERA_SIZE;
        assert!(w > h, "nearly every webcam is wider than it is tall");
        let aspect = w as f32 / h as f32;
        assert!(
            (aspect - 16.0 / 9.0).abs() < 0.05,
            "a guess worth making until a frame says otherwise: {aspect}"
        );
    }

    #[test]
    fn fps_choices_include_the_default() {
        assert!(FPS_CHOICES.contains(&Settings::new().fps));
    }

    #[test]
    fn refreshing_keeps_a_tick_on_a_screen_that_is_still_there() {
        let mut s = Settings::new();
        if s.monitors.is_empty() {
            return;
        }
        s.selected.iter_mut().for_each(|v| *v = false);
        s.selected[0] = true;
        let kept = s.monitors[0].label.clone();
        s.refresh_monitors();
        let now_on: Vec<_> = s
            .monitors
            .iter()
            .zip(&s.selected)
            .filter(|&(_, &on)| on)
            .map(|(m, _)| m.label.clone())
            .collect();
        assert!(now_on.contains(&kept), "ticks should survive a refresh");
    }

    #[test]
    fn something_is_always_selected_after_a_refresh() {
        let mut s = Settings::new();
        if s.monitors.is_empty() {
            return;
        }
        s.selected.iter_mut().for_each(|v| *v = false);
        s.refresh_monitors();
        assert!(
            s.selected.iter().any(|&on| on),
            "a refresh must not leave nothing recordable"
        );
    }

    #[test]
    fn whole_desktop_collapses_to_one_target() {
        let mut s = Settings::new();
        if s.monitors.len() < 2 {
            return;
        }
        s.whole_desktop = true;
        let t = s.targets();
        assert_eq!(t.len(), 1);
        assert_eq!(t[0].label, "Whole desktop");
    }

    #[test]
    fn the_default_bubble_sits_inside_the_primary() {
        let s = Settings::new();
        if s.monitors.is_empty() {
            return;
        }
        let b = s.default_bubble();
        let primary = s.monitors.iter().find(|m| m.is_primary).unwrap();
        assert!(primary.bounds.intersects(&b));
        assert!(b.right() <= primary.bounds.right());
        assert!(b.bottom() <= primary.bounds.bottom());
    }
}
