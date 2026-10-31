//! The seams between pure logic and the outside world.
//!
//! Everything here is a trait so `core` stays free of Windows, ffmpeg and the
//! UI. The adapters live in the `win` and `media` crates; the tests in this
//! crate use fakes.

use crate::geometry::{Monitor, Point, Rect};
use std::time::Duration;

/// A captured frame, borrowed from whatever buffer the adapter owns.
///
/// Immutable on purpose. The buffer behind it is the source's cache of the
/// most recent *pristine* capture, which it hands out again whenever the
/// screen has not changed. Compositing overlays into it would bake them in and
/// they would reappear, smeared, on every later unchanged frame.
pub struct Frame<'a> {
    /// Tightly packed BGRA8, `width * height * 4` bytes.
    pub pixels: &'a [u8],
    pub width: u32,
    pub height: u32,
    /// False when this is the previous frame handed out again because the
    /// screen has not changed.
    ///
    /// Windows only delivers a frame when something actually changes, so an
    /// idle screen costs nothing to capture. The overlays may still need
    /// redrawing — the pointer moves over a static screen — so this says the
    /// *capture* can be skipped, not necessarily the composite.
    pub changed: bool,
}

impl Frame<'_> {
    pub fn stride(&self) -> usize {
        self.width as usize * 4
    }

    pub fn bounds(&self) -> Rect {
        Rect::new(0, 0, self.width, self.height)
    }
}

#[derive(Debug, thiserror::Error)]
pub enum CaptureError {
    #[error("no displays were found")]
    NoDisplays,
    #[error("the display went away (resolution change, undock, or session lock)")]
    DisplayLost,
    #[error("timed out waiting for a frame")]
    Timeout,
    #[error("{0}")]
    Backend(String),
}

/// A source of screen frames for one monitor.
pub trait ScreenSource: Send {
    /// Block until the next frame, or `Timeout` if nothing arrives. A source
    /// with change detection should report `changed: false` rather than
    /// blocking forever on a static screen.
    fn next_frame(&mut self, timeout: Duration) -> Result<Frame<'_>, CaptureError>;

    fn bounds(&self) -> Rect;
}

/// Enumerates displays. Separate from `ScreenSource` so the UI can list
/// monitors without opening a capture session.
pub trait DisplayEnumerator {
    fn monitors(&self) -> Result<Vec<Monitor>, CaptureError>;
}

/// Something the pointer did, as seen by the global hook.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum PointerEvent {
    /// Wheel notches; positive is away from the user. Already filtered to the
    /// zoom modifier, so the recorder does not re-check keyboard state.
    Wheel {
        steps: f64,
    },
    Click {
        at: Point,
    },
    /// Reset zoom to 1x.
    Reset,
}

/// The global pointer hook.
pub trait PointerSource: Send {
    /// Current pointer position on the virtual desktop.
    fn position(&self) -> Point;
    /// Drain whatever has arrived since the last call. Never blocks: the hook
    /// callback must return promptly or Windows silently drops the hook, so it
    /// only enqueues and this drains.
    fn drain(&mut self, out: &mut Vec<PointerEvent>);
}

/// The latest webcam frame, if any.
pub trait CameraSource: Send + Sync {
    /// Most recent frame as tightly packed BGRA8 at the given size, or `None`
    /// before the first frame arrives.
    fn tile(&self, width: u32, height: u32) -> Option<Vec<u8>>;
    /// Where the bubble currently sits on the virtual desktop.
    fn rect(&self) -> Rect;
    fn error(&self) -> Option<String>;
}

#[derive(Debug, thiserror::Error)]
pub enum SinkError {
    #[error("the encoder exited: {0}")]
    EncoderDied(String),
    #[error("could not start the encoder: {0}")]
    StartFailed(String),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}

/// What a write did, beyond writing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WriteStatus {
    /// The frame went to the current segment.
    Ok,
    /// The encoder died and the sink recovered by dropping to a software
    /// encoder and opening a fresh segment. The frames written to the dead
    /// segment are gone, so the caller must restart its pacing and must not
    /// count the abandoned interval as recorded time.
    SegmentRestarted { encoder: &'static str },
}

/// Where composited frames go. One segment per pause interval; the sink joins
/// them when finished.
pub trait VideoSink: Send {
    /// Open a new segment. Called at start and on every resume.
    fn open_segment(&mut self) -> Result<(), SinkError>;
    /// Write one frame of BGRA8. Called `due` times with the same buffer when
    /// capture has fallen behind.
    fn write(&mut self, frame: &[u8]) -> Result<WriteStatus, SinkError>;
    /// Close the current segment cleanly, first topping it up with `pad`
    /// copies of the last frame so a stall before a pause or stop cannot
    /// silently shorten it.
    fn close_segment(&mut self, pad: u64) -> Result<(), SinkError>;
    /// Join every segment into `dest`.
    fn finish(&mut self, dest: &std::path::Path) -> Result<(), SinkError>;
    /// Label of the encoder actually in use, which may differ from the one
    /// chosen at start if a fallback happened.
    fn encoder_label(&self) -> &'static str;
}

/// Time source, injected so pacing tests are deterministic.
pub trait Clock: Send + Sync {
    fn now(&self) -> std::time::Instant;
}

/// The real clock.
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> std::time::Instant {
        std::time::Instant::now()
    }
}
