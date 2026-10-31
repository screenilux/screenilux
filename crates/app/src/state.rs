//! What the window is showing and what the user has chosen.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::mpsc::{Receiver, channel};

use screenilux_core::EffectOptions;
use screenilux_core::geometry::Monitor;
use screenilux_media::{EncoderSpec, locate_ffmpeg};

/// Camera bubble sizes, matching the Python build.
pub const CAMERA_SIZES: [(&str, u32, u32); 3] = [
    ("Small", 240, 180),
    ("Medium", 320, 240),
    ("Large", 440, 330),
];
pub const CAMERA_MARGIN: i32 = 28;
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
/// Enumerating DirectShow devices shells out to ffmpeg twice and probing
/// encoders runs short real encodes. Doing either inline would hold the window
/// back for seconds before it appeared.
pub struct Probes {
    pub encoder: Option<EncoderSpec>,
    pub microphones: Vec<String>,
    pub cameras: Vec<String>,
    pub ready: bool,
    rx: Option<Receiver<ProbeResult>>,
}

struct ProbeResult {
    encoder: EncoderSpec,
    microphones: Vec<String>,
    cameras: Vec<String>,
}

impl Probes {
    pub fn start(ffmpeg: Option<PathBuf>) -> Self {
        let (tx, rx) = channel();
        std::thread::Builder::new()
            .name("screenilux-probe".into())
            .spawn(move || {
                let Some(ffmpeg) = ffmpeg else { return };
                let encoder = screenilux_media::detect(&ffmpeg);
                let devices = screenilux_media::list_devices(&ffmpeg);
                let _ = tx.send(ProbeResult {
                    encoder,
                    microphones: devices.audio,
                    cameras: screenilux_win::cameras(),
                });
            })
            .ok();
        Self {
            encoder: None,
            microphones: Vec::new(),
            cameras: Vec::new(),
            ready: false,
            rx: Some(rx),
        }
    }

    /// Fold in results if they have arrived. Cheap to call every frame.
    pub fn poll(&mut self) {
        let Some(rx) = &self.rx else { return };
        if let Ok(r) = rx.try_recv() {
            self.encoder = Some(r.encoder);
            self.microphones = r.microphones;
            self.cameras = r.cameras;
            self.ready = true;
            self.rx = None;
        }
    }

    pub fn encoder_label(&self) -> String {
        match &self.encoder {
            Some(e) => e.label.to_string(),
            None => "detecting…".to_string(),
        }
    }
}

/// Everything the user can choose.
pub struct Settings {
    pub monitors: Vec<Monitor>,
    /// Parallel to `monitors`.
    pub selected: Vec<bool>,
    pub whole_desktop: bool,
    pub fps: u32,
    pub minimize: bool,
    pub effects: EffectOptions,
    pub microphone: Option<usize>,
    pub camera_device: usize,
    pub camera_size: usize,
    pub out_dir: PathBuf,
}

impl Settings {
    pub fn new() -> Self {
        let monitors = screenilux_win::monitors().unwrap_or_default();
        let selected = monitors.iter().map(|m| m.is_primary).collect();
        Self {
            monitors,
            selected,
            whole_desktop: false,
            fps: 30,
            minimize: true,
            effects: EffectOptions::default(),
            microphone: None,
            camera_device: 0,
            camera_size: 1,
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
        self.monitors = screenilux_win::monitors().unwrap_or_default();
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
        let (_, w, h) = CAMERA_SIZES[self.camera_size.min(CAMERA_SIZES.len() - 1)];
        (w, h)
    }

    /// Bottom-right of the primary screen, inset by a margin.
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
                m.bounds.top + m.bounds.height as i32 - h as i32 - CAMERA_MARGIN,
                w,
                h,
            ),
            None => screenilux_core::Rect::new(CAMERA_MARGIN, CAMERA_MARGIN, w, h),
        }
    }
}

fn default_output_dir() -> PathBuf {
    std::env::var_os("USERPROFILE")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
        .join("Videos")
        .join("ScreenRec")
}

/// Shared runtime handles the UI needs.
pub struct Runtime {
    pub ffmpeg: Option<PathBuf>,
    pub hook: screenilux_win::InputHook,
    pub camera: Option<screenilux_win::CameraHandle>,
    pub camera_feed: Option<Arc<screenilux_win::CameraFeed>>,
}

impl Runtime {
    pub fn new() -> Self {
        Self {
            ffmpeg: locate_ffmpeg(),
            hook: screenilux_win::InputHook::start(),
            camera: None,
            camera_feed: None,
        }
    }

    pub fn start_camera(&mut self, index: u32, bubble: screenilux_core::Rect) {
        self.stop_camera();
        let handle = screenilux_win::CameraHandle::start(index, bubble);
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
    fn camera_sizes_are_ordered_and_sane() {
        let mut last = 0;
        for (name, w, h) in CAMERA_SIZES {
            assert!(!name.is_empty());
            assert!(w > last, "sizes should increase");
            assert!(w > h, "a camera bubble is landscape");
            last = w;
        }
    }

    #[test]
    fn fps_choices_include_the_default() {
        assert!(FPS_CHOICES.contains(&30));
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
