//! Recording several screens as one session.
//!
//! Each screen runs its own capture thread and its own encoder, but they share
//! a start instant so frame N of every screen is the same moment, and they
//! share one run state so pause and stop reach them together.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use crossbeam_channel::{Receiver, Sender, unbounded};
use screenilux_core::compositor::{Compositor, EffectOptions};
use screenilux_core::focus::{DEFAULT_DWELL, FocusTracker};
use screenilux_core::geometry::Monitor;
use screenilux_core::keys::Shortcut;
use screenilux_core::ports::VideoSink;
use screenilux_core::ports::{CameraSource, PointerEvent};
use screenilux_core::session::{Focus, FocusEntry, Manifest, Role, Screen};
use screenilux_core::timing::SyncClock;
use screenilux_platform::capture::ScreenCapture;
use screenilux_platform::encoder::{ENCODER_LABEL, NativeSink};

use crate::paths::{TempDir, unique_path};

use crate::control::Control;
use crate::screen::{Elapsed, ScreenJob, Sound, run};

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
#[derive(Clone)]
pub struct SessionConfig {
    pub monitors: Vec<Monitor>,
    pub fps: u32,
    /// Record the default microphone alongside the first screen.
    pub microphone: bool,
    /// Record what the computer plays, mixed with the microphone if both.
    pub system_sound: bool,
    /// Put a quiet click in the sound track for each mouse click.
    pub click_sound: bool,
    pub effects: EffectOptions,
    /// The live camera, if the bubble is enabled.
    pub camera: Option<Arc<dyn CameraSource>>,
    /// Pointer events from the app's hook. Without this, zoom and click
    /// effects simply do not fire.
    pub pointer_events: Option<Receiver<PointerEvent>>,
}

impl std::fmt::Debug for SessionConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SessionConfig")
            .field("monitors", &self.monitors.len())
            .field("fps", &self.fps)
            .field("microphone", &self.microphone)
            .field("system_sound", &self.system_sound)
            .field("click_sound", &self.click_sound)
            .field("effects", &self.effects)
            .field("camera", &self.camera.is_some())
            .finish()
    }
}

impl SessionConfig {
    fn camera_enabled(&self) -> bool {
        self.camera.is_some() && self.effects.camera
    }
}

struct ScreenSlot {
    monitor: Monitor,
    elapsed: Arc<Elapsed>,
    audio: bool,
    handle: Option<JoinHandle<Result<(), crate::screen::ScreenError>>>,
    sink: SinkHandle,
}

/// The sink is handed to the capture thread, so finishing needs a second
/// reference. `NativeSink` is not clonable, so the thread gives it back.
enum SinkHandle {
    Pending(Receiver<Box<NativeSink>>),
    Ready(Box<NativeSink>),
}

/// A live recording.
pub struct Session {
    config: SessionConfig,
    control: Arc<Control>,
    sync: Arc<SyncClock>,
    screens: Vec<ScreenSlot>,
    /// Kept alive for the session; removes the scratch files when dropped.
    workspace: TempDir,
    focus: Option<FocusWorker>,
    router: Option<RouterWorker>,
    /// Where the pointer went, written down as the recording runs so a zoom
    /// can follow it and its clicks can be marked afterwards.
    watcher: Arc<Mutex<screenilux_core::pointer::Watcher>>,
    /// Shortcuts pressed while recording, to show over the picture.
    shortcuts: Arc<Mutex<Vec<Shortcut>>>,
    sampler: Option<RouterWorker>,
    started: std::time::SystemTime,
}

/// Fans one global hook out to the screen each event belongs to.
///
/// There is a single hook for the whole process, but zoom is per-screen: a
/// wheel gesture should magnify the screen under the pointer and leave the
/// others alone.
struct RouterWorker {
    stop: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
}

impl RouterWorker {
    fn stop(mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
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

        // One scratch directory for the whole session; encoders write here
        // and the finished files are moved out.
        let workspace = TempDir::new("screenilux-session")?;
        let control = Arc::new(Control::new());
        let sync = Arc::new(SyncClock::new(config.monitors.len()));

        let mut screens = Vec::new();
        let mut event_senders: Vec<(screenilux_core::geometry::Rect, Sender<PointerEvent>)> =
            Vec::new();
        // Clicks anywhere on the desktop, for the screen carrying the sound.
        let (click_tx, click_rx) = if config.click_sound {
            let (tx, rx) = unbounded();
            (Some(tx), Some(rx))
        } else {
            (None, None)
        };
        let mut click_rx = click_rx;
        for (i, monitor) in config.monitors.iter().enumerate() {
            let mut capture = ScreenCapture::start(monitor.bounds)
                .map_err(|e| SessionError::Capture(format!("{}: {e}", monitor.label)))?;
            // A recording must not open on black.
            capture
                .wait_for_first_frame(Duration::from_secs(5))
                .map_err(|e| SessionError::Capture(format!("{}: {e}", monitor.label)))?;

            // Only the first screen carries sound; a single file with audio
            // is what the manifest records and what a player expects.
            let audio = i == 0 && (config.microphone || config.system_sound || config.click_sound);
            let scratch = workspace.path().join(format!("screen{}.mp4", i + 1));
            let sink = NativeSink::new(
                monitor.bounds.width,
                monitor.bounds.height,
                config.fps,
                audio,
                scratch,
            )
            .map_err(|e| SessionError::Capture(format!("{}: {e}", monitor.label)))?;

            let elapsed = Arc::new(Elapsed::default());
            let (give_back, take_back) = crossbeam_channel::bounded(1);

            // The pointer is drawn on playback from its path, not into the
            // picture; the rest of the live effects still are.
            let drawn = EffectOptions {
                cursor: false,
                ..config.effects
            };
            let compositor = drawn.any().then(|| Compositor::new(monitor.bounds, drawn));
            let (event_tx, event_rx) = unbounded();
            event_senders.push((monitor.bounds, event_tx));

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
                compositor,
                events: Some(event_rx),
                pointer: Arc::new(screenilux_platform::cursor_pos),
                camera: if config.camera_enabled() {
                    config.camera.clone()
                } else {
                    None
                },
                sound: audio.then(|| {
                    Sound::new(
                        config
                            .microphone
                            .then(screenilux_platform::AudioCapture::start),
                        config
                            .system_sound
                            .then(screenilux_platform::AudioCapture::start_system),
                        click_rx.take(),
                    )
                }),
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

        let watcher = Arc::new(Mutex::new(screenilux_core::pointer::Watcher::new()));
        let shortcuts = Arc::new(Mutex::new(Vec::new()));

        // Reading the pointer is worth doing whether or not any effect is
        // switched on: the track is what makes a zoom able to follow it
        // later, and that is decided while editing, not while recording.
        let sampler = {
            let stop = Arc::new(AtomicBool::new(false));
            let flag = Arc::clone(&stop);
            let watcher = Arc::clone(&watcher);
            let clock = screens.first().map(|s| Arc::clone(&s.elapsed));
            let handle = std::thread::Builder::new()
                .name("screenilux-pointer".into())
                .spawn(move || {
                    let step = Duration::from_secs_f64(1.0 / screenilux_core::pointer::RATE);
                    while !flag.load(Ordering::Relaxed) {
                        if let Some(clock) = &clock {
                            let t = clock.get().as_secs_f64();
                            let at = screenilux_platform::cursor_pos();
                            if let Ok(mut w) = watcher.lock() {
                                w.saw(t, at);
                            }
                        }
                        std::thread::sleep(step);
                    }
                })
                .ok();
            Some(RouterWorker { stop, handle })
        };

        // The hook is only worth installing if something consumes it, and
        // the track counts: a click is part of it.
        let router = config.pointer_events.clone().map(|events| {
            let stop = Arc::new(AtomicBool::new(false));
            let flag = Arc::clone(&stop);
            let watcher = Arc::clone(&watcher);
            let shortcuts = Arc::clone(&shortcuts);
            let clock = screens.first().map(|s| Arc::clone(&s.elapsed));
            let handle = std::thread::Builder::new()
                .name("screenilux-router".into())
                .spawn(move || {
                    route_events(
                        events,
                        event_senders,
                        click_tx,
                        flag,
                        Written { watcher, shortcuts },
                        clock,
                    )
                })
                .ok();
            RouterWorker { stop, handle }
        });

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
            workspace,
            focus,
            router,
            watcher,
            shortcuts,
            sampler,
            started: std::time::SystemTime::now(),
        })
    }

    pub fn pause(&self) {
        self.control.pause();
    }

    pub fn resume(&self) {
        self.control.resume();
    }

    /// Recorded seconds, excluding paused time.
    pub fn duration(&self) -> Duration {
        self.screens
            .iter()
            .map(|s| s.elapsed.get())
            .max()
            .unwrap_or_default()
    }

    /// Stop every screen and join its thread.
    fn halt(&mut self) -> Vec<String> {
        // Stop routing first so the global mouse hook comes down with the
        // recording rather than outliving it.
        if let Some(r) = self.router.take() {
            r.stop();
        }
        if let Some(s) = self.sampler.take() {
            s.stop();
        }

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
    /// One screen yields a single MP4, which plays anywhere. Several yield a
    /// `.screenilux` package: the per-screen MP4s and their manifest in one
    /// file. Returns the path written.
    pub fn finish(mut self, outdir: &Path) -> Result<PathBuf, SessionError> {
        let failures = self.halt();
        let focus_timeline = self
            .focus
            .take()
            .map(FocusWorker::collect)
            .unwrap_or_default();

        std::fs::create_dir_all(outdir)?;
        let stamp = timestamp(self.started);

        // The pointer's path and the shortcuts, if anything was written
        // down. Taken before the screens are finished, since nothing adds to
        // them after the capture threads have stopped.
        let pointer = self
            .watcher
            .lock()
            .ok()
            .map(|mut w| std::mem::take(&mut *w).finish())
            .filter(|t| !t.is_empty());
        let shortcuts = self
            .shortcuts
            .lock()
            .map(|mut s| std::mem::take(&mut *s))
            .unwrap_or_default();

        let mut manifest = Manifest::new(
            iso_timestamp(self.started),
            round3(self.duration().as_secs_f64()),
            self.config.fps,
            ENCODER_LABEL.to_string(),
            self.config.camera_enabled(),
        );
        // The pointer is no longer drawn into the picture: it is drawn on
        // playback from its path, so it can be smoothed, sized and hidden.
        manifest.cursor_in_picture = false;
        manifest.pointer = pointer;
        manifest.shortcuts = shortcuts;
        if !self.config.effects.cursor {
            // "Show the pointer" was off: start the recording with it hidden,
            // which the editor can still turn back on.
            let mut edits = screenilux_core::edit::EditList::default();
            edits.pointer.hidden = true;
            manifest.edits = Some(edits);
        }

        if self.screens.len() == 1 {
            let dest = unique_path(outdir, &format!("recording_{stamp}"), "mp4");
            let slot = &mut self.screens[0];
            finish_sink(slot, &dest)
                .map_err(|e| SessionError::Capture(join_errors(&failures, &e)))?;
            // A plain video has nowhere inside it for the pointer's path, so
            // that is written beside it. The video still plays anywhere.
            let b = slot.monitor.bounds;
            manifest.screens.push(Screen {
                file: dest
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default(),
                label: slot.monitor.label.clone(),
                width: b.width,
                height: b.height,
                left: b.left,
                top: b.top,
                primary: slot.monitor.is_primary,
                audio: slot.audio,
                role: Role::Primary,
                order: 1,
            });
            if let Ok(json) = manifest.to_json()
                && let Err(e) = std::fs::write(details_path(&dest), json)
            {
                tracing::warn!(error = %e, "could not write the recording's details beside it");
            }
            return Ok(dest);
        }

        // Assembled as a session folder in scratch space, then packed. The
        // finished videos are moved, not copied, into the folder, so the
        // only full copy is the one into the package.
        let folder = self.workspace.path().join("session");
        std::fs::create_dir_all(&folder)?;

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
        let dest = unique_path(
            outdir,
            &format!("recording_{stamp}"),
            screenilux_core::package::PACKAGE_EXT,
        );
        screenilux_core::package::pack(&folder, &dest)
            .map_err(|e| SessionError::Manifest(e.to_string()))?;
        Ok(dest)
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
    inner: Option<Box<NativeSink>>,
    give_back: Option<Sender<Box<NativeSink>>>,
}

impl screenilux_core::ports::VideoSink for SinkCarrier {
    fn open_segment(&mut self) -> Result<(), screenilux_core::ports::SinkError> {
        self.inner.as_mut().expect("sink taken").open_segment()
    }
    fn write(&mut self, frame: &[u8]) -> Result<(), screenilux_core::ports::SinkError> {
        self.inner.as_mut().expect("sink taken").write(frame)
    }
    fn close_segment(&mut self, pad: u64) -> Result<(), screenilux_core::ports::SinkError> {
        self.inner.as_mut().expect("sink taken").close_segment(pad)
    }
    fn finish(&mut self, dest: &Path) -> Result<(), screenilux_core::ports::SinkError> {
        self.inner.as_mut().expect("sink taken").finish(dest)
    }
    fn write_audio(&mut self, pcm: &[u8]) -> Result<(), screenilux_core::ports::SinkError> {
        self.inner.as_mut().expect("sink taken").write_audio(pcm)
    }
}

impl Drop for SinkCarrier {
    fn drop(&mut self) {
        if let (Some(sink), Some(tx)) = (self.inner.take(), self.give_back.take()) {
            let _ = tx.send(sink);
        }
    }
}

/// Where a recording's details are kept beside a plain video: the pointer's
/// path, the shortcuts and, once edited, its effects. The video's own name
/// with `.json` after it, so the two stay together and never collide with
/// another recording's.
pub fn details_path(video: &Path) -> PathBuf {
    let mut name = video.as_os_str().to_owned();
    name.push(".json");
    PathBuf::from(name)
}

/// What the router writes down as it goes.
struct Written {
    watcher: Arc<Mutex<screenilux_core::pointer::Watcher>>,
    shortcuts: Arc<Mutex<Vec<Shortcut>>>,
}

/// Drain the global hook and hand each event to the screen it belongs to.
fn route_events(
    events: Receiver<PointerEvent>,
    screens: Vec<(screenilux_core::geometry::Rect, Sender<PointerEvent>)>,
    clicks: Option<Sender<()>>,
    stop: Arc<AtomicBool>,
    written: Written,
    clock: Option<Arc<crate::screen::Elapsed>>,
) {
    let Written { watcher, shortcuts } = written;
    while !stop.load(Ordering::Relaxed) {
        // A short block keeps the thread responsive to stop without spinning.
        let Ok(event) = events.recv_timeout(Duration::from_millis(50)) else {
            continue;
        };
        {
            match event {
                // A reset clears every screen: middle-clicking to escape a
                // zoom should not leave another screen magnified.
                PointerEvent::Reset => {
                    for (_, tx) in &screens {
                        let _ = tx.send(PointerEvent::Reset);
                    }
                }
                PointerEvent::Click { at } => {
                    if let Some(clock) = &clock
                        && let Ok(mut w) = watcher.lock()
                    {
                        w.clicked(clock.get().as_secs_f64(), at);
                    }
                    if let Some((_, tx)) = screens.iter().find(|(b, _)| b.contains(at)) {
                        let _ = tx.send(event);
                    }
                    if let Some(c) = &clicks {
                        let _ = c.send(());
                    }
                }
                PointerEvent::Wheel { .. } => {
                    let at = screenilux_platform::cursor_pos();
                    if let Some((_, tx)) = screens.iter().find(|(b, _)| b.contains(at)) {
                        let _ = tx.send(event);
                    }
                }
                PointerEvent::Key { mods, key } => {
                    if let (Some(clock), Some(keys)) =
                        (&clock, screenilux_core::keys::shortcut(mods, key))
                        && let Ok(mut list) = shortcuts.lock()
                    {
                        list.push(Shortcut {
                            t: (clock.get().as_secs_f64() * 1000.0).round() / 1000.0,
                            keys,
                        });
                    }
                }
            }
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
            screenilux_platform::cursor_pos(),
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
    screenilux_platform::now_local()
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
