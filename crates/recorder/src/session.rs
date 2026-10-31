//! Recording several screens as one session.
//!
//! Each screen runs its own capture thread and its own encoder, but they share
//! a start instant so frame N of every screen is the same moment, and they
//! share one run state so pause and stop reach them together.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;
use std::time::Duration;

use crossbeam_channel::{Receiver, Sender, unbounded};
use screenilux_core::focus::{DEFAULT_DWELL, FocusTracker};
use screenilux_core::geometry::Monitor;
use screenilux_core::ports::VideoSink;
use screenilux_core::session::{Focus, FocusEntry, Manifest, Role, Screen};
use screenilux_core::timing::SyncClock;
use screenilux_media::command::{EncoderSpec, SegmentParams};
use screenilux_media::segment::FfmpegSink;
use screenilux_media::util::unique_path;
use screenilux_win::capture::ScreenCapture;

use crate::control::Control;
use crate::screen::{Elapsed, ScreenJob, run};

const FOCUS_INTERVAL: Duration = Duration::from_millis(80);
const MANIFEST_NAME: &str = screenilux_core::session::MANIFEST_NAME;

#[derive(Debug, thiserror::Error)]
pub enum SessionError {
    #[error("no screens were selected")]
    NoScreens,
    #[error("nothing was recorded")]
    Empty,
    #[error("{0}")]
    Capture(String),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("could not write the manifest: {0}")]
    Manifest(String),
}

/// What to record.
#[derive(Debug, Clone)]
pub struct SessionConfig {
    pub monitors: Vec<Monitor>,
    pub fps: u32,
    /// Rides on the first screen only: Windows gives a capture device a
    /// single consumer.
    pub audio_device: Option<String>,
    pub encoder: EncoderSpec,
    pub ffmpeg: PathBuf,
    pub camera: bool,
}

struct ScreenSlot {
    monitor: Monitor,
    elapsed: Arc<Elapsed>,
    audio: bool,
    handle: Option<JoinHandle<Result<&'static str, crate::screen::ScreenError>>>,
    sink: SinkHandle,
}

/// The sink is handed to the capture thread, so finishing needs a second
/// reference. `FfmpegSink` is not clonable, so the thread gives it back.
enum SinkHandle {
    Pending(Receiver<Box<FfmpegSink>>),
    Ready(Box<FfmpegSink>),
}

/// A live recording.
pub struct Session {
    config: SessionConfig,
    control: Arc<Control>,
    sync: Arc<SyncClock>,
    screens: Vec<ScreenSlot>,
    focus: Option<FocusWorker>,
    notices_rx: Receiver<String>,
    started: std::time::SystemTime,
}

struct FocusWorker {
    stop: Arc<AtomicBool>,
    handle: Option<JoinHandle<Vec<screenilux_core::focus::Switch>>>,
}

impl Session {
    /// Open every capture and encoder, then start the threads.
    ///
    /// Captures are opened before any thread starts so a failure surfaces here
    /// rather than halfway into a recording the user thinks is running.
    pub fn start(config: SessionConfig) -> Result<Self, SessionError> {
        if config.monitors.is_empty() {
            return Err(SessionError::NoScreens);
        }

        let control = Arc::new(Control::new());
        let sync = Arc::new(SyncClock::new(config.monitors.len()));
        let (notices, notices_rx) = unbounded();

        let mut screens = Vec::new();
        for (i, monitor) in config.monitors.iter().enumerate() {
            let mut capture = ScreenCapture::start(monitor.bounds)
                .map_err(|e| SessionError::Capture(format!("{}: {e}", monitor.label)))?;
            // A recording must not open on black.
            capture
                .wait_for_first_frame(Duration::from_secs(5))
                .map_err(|e| SessionError::Capture(format!("{}: {e}", monitor.label)))?;

            let audio = i == 0 && config.audio_device.is_some();
            let params = SegmentParams {
                width: monitor.bounds.width,
                height: monitor.bounds.height,
                fps: config.fps,
                audio_device: if audio {
                    config.audio_device.clone()
                } else {
                    None
                },
            };
            let sink = FfmpegSink::new(&config.ffmpeg, params, config.encoder)
                .map_err(|e| SessionError::Capture(format!("{}: {e}", monitor.label)))?;

            let elapsed = Arc::new(Elapsed::default());
            let (give_back, take_back) = crossbeam_channel::bounded(1);

            let job = ScreenJob {
                source: Box::new(capture),
                sink: Box::new(SinkCarrier {
                    inner: Some(Box::new(sink)),
                    give_back: Some(give_back),
                }),
                fps: config.fps,
                control: Arc::clone(&control),
                sync: Arc::clone(&sync),
                elapsed: Arc::clone(&elapsed),
                label: monitor.label.clone(),
                notices: notices.clone(),
            };

            let label = monitor.label.clone();
            let handle = std::thread::Builder::new()
                .name(format!("screenilux-capture-{}", i + 1))
                .spawn(move || {
                    let r = run(job);
                    if let Err(e) = &r {
                        tracing::error!(screen = label, error = %e, "capture thread failed");
                    }
                    r
                })?;

            screens.push(ScreenSlot {
                monitor: monitor.clone(),
                elapsed,
                audio,
                handle: Some(handle),
                sink: SinkHandle::Pending(take_back),
            });
        }

        // Focus is only meaningful across several screens.
        let focus = (config.monitors.len() > 1).then(|| {
            let monitors = config.monitors.clone();
            let elapsed: Vec<_> = screens.iter().map(|s| Arc::clone(&s.elapsed)).collect();
            let ctl = Arc::clone(&control);
            let stop = Arc::new(AtomicBool::new(false));
            let stop_flag = Arc::clone(&stop);
            let handle = std::thread::Builder::new()
                .name("screenilux-focus".into())
                .spawn(move || track_focus(monitors, elapsed, ctl, stop_flag))
                .ok();
            FocusWorker { stop, handle }
        });

        Ok(Self {
            config,
            control,
            sync,
            screens,
            focus,
            notices_rx,
            started: std::time::SystemTime::now(),
        })
    }

    pub fn pause(&self) {
        self.control.pause();
    }

    pub fn resume(&self) {
        self.control.resume();
    }

    pub fn is_paused(&self) -> bool {
        self.control.state() == crate::control::RunState::Paused
    }

    /// Recorded seconds, excluding paused time.
    pub fn duration(&self) -> Duration {
        self.screens
            .iter()
            .map(|s| s.elapsed.get())
            .max()
            .unwrap_or_default()
    }

    /// Anything worth telling the user, such as an encoder fallback.
    pub fn notices(&self) -> Vec<String> {
        self.notices_rx.try_iter().collect()
    }

    /// Stop every screen and join its thread.
    fn halt(&mut self) -> Vec<String> {
        self.control.stop();
        // Release anyone parked at a segment boundary so stop cannot hang on a
        // peer that will never arrive.
        self.sync.abort();

        let mut failures = Vec::new();
        for slot in &mut self.screens {
            if let Some(h) = slot.handle.take() {
                match h.join() {
                    Ok(Ok(_)) => {}
                    Ok(Err(e)) => failures.push(format!("{}: {e}", slot.monitor.label)),
                    Err(_) => {
                        failures.push(format!("{}: capture thread panicked", slot.monitor.label))
                    }
                }
            }
            if let SinkHandle::Pending(rx) = &slot.sink
                && let Ok(sink) = rx.recv_timeout(Duration::from_secs(5))
            {
                slot.sink = SinkHandle::Ready(sink);
            }
        }
        failures
    }

    /// Stop recording and write the result into `outdir`.
    ///
    /// One screen yields a single MP4; several yield a session folder with a
    /// manifest. Returns the path written.
    pub fn finish(mut self, outdir: &Path) -> Result<PathBuf, SessionError> {
        let failures = self.halt();
        let focus_timeline = self
            .focus
            .take()
            .map(FocusWorker::collect)
            .unwrap_or_default();

        std::fs::create_dir_all(outdir)?;
        let stamp = timestamp(self.started);

        if self.screens.len() == 1 {
            let dest = unique_path(outdir, &format!("recording_{stamp}"), "mp4");
            let slot = &mut self.screens[0];
            finish_sink(slot, &dest)
                .map_err(|e| SessionError::Capture(join_errors(&failures, &e)))?;
            return Ok(dest);
        }

        let folder = unique_path(outdir, &format!("recording_{stamp}"), "");
        std::fs::create_dir_all(&folder)?;

        let mut manifest = Manifest::new(
            iso_timestamp(self.started),
            round3(self.duration().as_secs_f64()),
            self.config.fps,
            self.config.encoder.label.to_string(),
            self.config.camera,
        );

        let mut saved_files = Vec::new();
        let mut lost = failures;
        for (i, slot) in self.screens.iter_mut().enumerate() {
            let file = format!("screen{}.mp4", i + 1);
            let dest = folder.join(&file);
            if let Err(e) = finish_sink(slot, &dest) {
                lost.push(format!("{}: {e}", slot.monitor.label));
                continue;
            }
            let b = slot.monitor.bounds;
            manifest.screens.push(Screen {
                file: file.clone(),
                label: slot.monitor.label.clone(),
                width: b.width,
                height: b.height,
                left: b.left,
                top: b.top,
                primary: slot.monitor.is_primary,
                audio: slot.audio,
                role: if slot.monitor.is_primary {
                    Role::Primary
                } else {
                    Role::Secondary
                },
                order: 0,
            });
            saved_files.push((i, file));
        }

        if manifest.screens.is_empty() {
            return Err(SessionError::Empty);
        }
        manifest.assign_order();

        if !focus_timeline.is_empty() {
            let by_index: std::collections::HashMap<usize, String> =
                saved_files.into_iter().collect();
            let entries: Vec<FocusEntry> = focus_timeline
                .iter()
                .filter_map(|s| {
                    by_index.get(&s.screen).map(|f| FocusEntry {
                        t: s.t,
                        screen: f.clone(),
                    })
                })
                .collect();
            if !entries.is_empty() {
                manifest.focus = Some(Focus::new(DEFAULT_DWELL, entries));
            }
        }

        let json = manifest
            .to_json()
            .map_err(|e| SessionError::Manifest(e.to_string()))?;
        std::fs::write(folder.join(MANIFEST_NAME), json)?;

        if !lost.is_empty() {
            tracing::warn!(lost = lost.join("; "), "some screens did not save");
        }
        Ok(folder)
    }
}

fn join_errors(failures: &[String], e: &str) -> String {
    if failures.is_empty() {
        e.to_string()
    } else {
        format!("{e} ({})", failures.join("; "))
    }
}

fn finish_sink(slot: &mut ScreenSlot, dest: &Path) -> Result<(), String> {
    match &mut slot.sink {
        SinkHandle::Ready(sink) => sink.finish(dest).map_err(|e| e.to_string()),
        SinkHandle::Pending(_) => Err("the encoder was never handed back".to_string()),
    }
}

/// Wraps the sink so the capture thread can return it for finishing.
struct SinkCarrier {
    inner: Option<Box<FfmpegSink>>,
    give_back: Option<Sender<Box<FfmpegSink>>>,
}

impl screenilux_core::ports::VideoSink for SinkCarrier {
    fn open_segment(&mut self) -> Result<(), screenilux_core::ports::SinkError> {
        self.inner.as_mut().expect("sink taken").open_segment()
    }
    fn write(
        &mut self,
        frame: &[u8],
    ) -> Result<screenilux_core::ports::WriteStatus, screenilux_core::ports::SinkError> {
        self.inner.as_mut().expect("sink taken").write(frame)
    }
    fn close_segment(&mut self, pad: u64) -> Result<(), screenilux_core::ports::SinkError> {
        self.inner.as_mut().expect("sink taken").close_segment(pad)
    }
    fn finish(&mut self, dest: &Path) -> Result<(), screenilux_core::ports::SinkError> {
        self.inner.as_mut().expect("sink taken").finish(dest)
    }
    fn encoder_label(&self) -> &'static str {
        self.inner.as_ref().expect("sink taken").encoder_label()
    }
}

impl Drop for SinkCarrier {
    fn drop(&mut self) {
        if let (Some(sink), Some(tx)) = (self.inner.take(), self.give_back.take()) {
            let _ = tx.send(sink);
        }
    }
}

fn track_focus(
    monitors: Vec<Monitor>,
    elapsed: Vec<Arc<Elapsed>>,
    control: Arc<Control>,
    stop: Arc<AtomicBool>,
) -> Vec<screenilux_core::focus::Switch> {
    let mut tracker = FocusTracker::new(monitors, DEFAULT_DWELL);
    while !stop.load(Ordering::Relaxed) {
        let state = control.state();
        if state == crate::control::RunState::Stopped {
            break;
        }
        let recorded = elapsed
            .iter()
            .map(|e| e.get())
            .max()
            .unwrap_or_default()
            .as_secs_f64();
        tracker.sample(
            screenilux_win::cursor_pos(),
            recorded,
            state == crate::control::RunState::Running,
        );
        std::thread::sleep(FOCUS_INTERVAL);
    }
    tracker.timeline().to_vec()
}

impl FocusWorker {
    fn collect(mut self) -> Vec<screenilux_core::focus::Switch> {
        self.stop.store(true, Ordering::Relaxed);
        self.handle
            .take()
            .and_then(|h| h.join().ok())
            .unwrap_or_default()
    }
}

fn round3(v: f64) -> f64 {
    (v * 1000.0).round() / 1000.0
}

/// `YYYY-MM-DD_HH-MM-SS` in local time, matching the Python build's filenames.
fn timestamp(t: std::time::SystemTime) -> String {
    let (y, mo, d, h, mi, s) = civil_local(t);
    format!("{y:04}-{mo:02}-{d:02}_{h:02}-{mi:02}-{s:02}")
}

/// `YYYY-MM-DDTHH:MM:SS`, the manifest's `created` format.
fn iso_timestamp(t: std::time::SystemTime) -> String {
    let (y, mo, d, h, mi, s) = civil_local(t);
    format!("{y:04}-{mo:02}-{d:02}T{h:02}:{mi:02}:{s:02}")
}

/// Civil local time, delegated to the platform layer so this crate stays
/// free of unsafe.
fn civil_local(_t: std::time::SystemTime) -> (i32, u32, u32, u32, u32, u32) {
    screenilux_win::now_local()
}

#[allow(dead_code)]
fn civil_from_unix(secs: i64) -> (i32, u32, u32, u32, u32, u32) {
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    (
        y as i32,
        m as u32,
        d as u32,
        (rem / 3600) as u32,
        ((rem % 3600) / 60) as u32,
        (rem % 60) as u32,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn civil_conversion_matches_known_instants() {
        assert_eq!(civil_from_unix(0), (1970, 1, 1, 0, 0, 0));
        assert_eq!(civil_from_unix(1_000_000_000), (2001, 9, 9, 1, 46, 40));
        // a leap day
        assert_eq!(civil_from_unix(1_709_164_800), (2024, 2, 29, 0, 0, 0));
    }

    #[test]
    fn timestamps_are_filename_safe_and_sortable() {
        let t = std::time::SystemTime::now();
        let name = timestamp(t);
        assert_eq!(name.len(), 19, "{name}");
        assert!(
            !name.contains(':'),
            "a colon is illegal in a Windows filename"
        );
        assert!(name.as_bytes()[4] == b'-' && name.as_bytes()[10] == b'_');
    }

    #[test]
    fn iso_timestamp_has_the_manifest_shape() {
        let s = iso_timestamp(std::time::SystemTime::now());
        assert_eq!(s.len(), 19);
        assert_eq!(s.as_bytes()[10], b'T');
        assert_eq!(s.matches(':').count(), 2);
    }

    #[test]
    fn rounding_matches_the_manifest_precision() {
        assert_eq!(round3(5.0829999), 5.083);
        assert_eq!(round3(0.0004), 0.0);
    }
}
