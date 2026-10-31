//! One screen's capture-and-encode loop.
//!
//! Everything here is expressed against the `core` ports, so the whole loop —
//! pacing, pause handling, audio forwarding — is testable with fakes, without
//! a display or an encoder.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, Sender};
use screenilux_core::click::ClickMixer;
use screenilux_core::compositor::{CameraView, Compositor};
use screenilux_core::geometry::Point;
use screenilux_core::ports::{
    CameraSource, CaptureError, PointerEvent, ScreenSource, SinkError, VideoSink, WriteStatus,
};
use screenilux_core::timing::{FramePacer, SyncClock};

use crate::control::{Control, RunState};

/// How long a capture may stall before we give up on a fresh frame and reuse
/// the last one.
///
/// This also bounds how long stopping takes, because a thread blocked here
/// cannot notice the stop until it returns. At 30fps a frame is 33ms, so this
/// is several frames of grace while keeping stop responsive. An idle screen
/// legitimately spends the whole timeout here and then reuses its last frame;
/// the pacer still emits the right number of frames either way.
const FRAME_TIMEOUT: Duration = Duration::from_millis(120);
/// How long a screen waits for its peers at a segment boundary.
const BARRIER_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Debug, thiserror::Error)]
pub enum ScreenError {
    #[error("capture failed: {0}")]
    Capture(#[from] CaptureError),
    #[error("encoding failed: {0}")]
    Sink(#[from] SinkError),
}

/// Recorded time for one screen, readable while it runs.
///
/// Microseconds in an atomic rather than a locked float: the session, the
/// focus tracker and the UI all read this at least once per frame, and none
/// of them should ever block a capture thread to do it.
#[derive(Debug, Default)]
pub struct Elapsed(AtomicU64);

impl Elapsed {
    pub fn get(&self) -> Duration {
        Duration::from_micros(self.0.load(Ordering::Relaxed))
    }

    fn set(&self, d: Duration) {
        self.0.store(d.as_micros() as u64, Ordering::Relaxed);
    }
}

pub struct ScreenJob {
    pub source: Box<dyn ScreenSource>,
    pub sink: Box<dyn VideoSink>,
    pub fps: u32,
    pub control: Arc<Control>,
    pub sync: Arc<SyncClock>,
    pub elapsed: Arc<Elapsed>,
    pub label: String,
    pub notices: Sender<String>,
    /// Overlay compositing. `None` means frames go to the encoder untouched,
    /// which skips a full-frame copy per frame.
    pub compositor: Option<Compositor>,
    /// Pointer events already routed to this screen.
    pub events: Option<Receiver<PointerEvent>>,
    /// Where the pointer is, sampled per frame for the overlays.
    pub pointer: Arc<dyn Fn() -> Point + Send + Sync>,
    pub camera: Option<Arc<dyn CameraSource>>,
    /// The sound track, on the one screen that carries it.
    pub sound: Option<Sound>,
}

/// Bytes per sample frame of the PCM the encoder takes.
const BLOCK: usize = screenilux_win::audio::CHANNELS as usize * 2;

/// What goes into the sound track: the microphone, clicks, or both.
///
/// With no microphone the track is silence written at the pace of the
/// recording, so clicks have somewhere to go and land at the right time.
pub struct Sound {
    mic: Option<screenilux_win::AudioCapture>,
    /// One message per mouse click, anywhere on the desktop.
    clicks: Option<Receiver<()>>,
    mixer: ClickMixer,
    /// Sample frames written so far.
    written: u64,
}

impl Sound {
    pub fn new(mic: Option<screenilux_win::AudioCapture>, clicks: Option<Receiver<()>>) -> Self {
        Self {
            mic,
            clicks,
            mixer: ClickMixer::new(
                screenilux_win::audio::SAMPLE_RATE,
                screenilux_win::audio::CHANNELS,
            ),
            written: 0,
        }
    }

    /// Drop what was heard while nothing was being recorded: before frame
    /// zero, or during a pause.
    fn discard(&mut self) {
        if let Some(mic) = &self.mic {
            mic.discard();
        }
        if let Some(clicks) = &self.clicks {
            while clicks.try_recv().is_ok() {}
        }
        self.mixer.silence();
    }

    /// The audio since the last call, clicks mixed in. `recorded` is how
    /// much has been recorded, which paces the silence when there is no
    /// microphone.
    fn take(&mut self, recorded: Duration) -> Vec<Vec<u8>> {
        if let Some(clicks) = &self.clicks {
            for () in clicks.try_iter() {
                self.mixer.trigger();
            }
        }
        let mut chunks = match &self.mic {
            Some(mic) => mic.drain(),
            None => {
                let due =
                    (recorded.as_secs_f64() * screenilux_win::audio::SAMPLE_RATE as f64) as u64;
                match due.saturating_sub(self.written) {
                    0 => Vec::new(),
                    n => vec![vec![0u8; n as usize * BLOCK]],
                }
            }
        };
        for chunk in &mut chunks {
            self.mixer.mix(chunk);
            self.written += (chunk.len() / BLOCK) as u64;
        }
        chunks
    }
}

/// Run one screen to completion. Returns the encoder label actually used,
/// which may differ from the one it started with.
pub fn run(mut job: ScreenJob) -> Result<&'static str, ScreenError> {
    let result = capture_loop(&mut job);
    if result.is_err() {
        // Never leave peers parked at the barrier waiting for a screen that
        // has already died.
        job.sync.abort();
    }
    result
}

fn capture_loop(job: &mut ScreenJob) -> Result<&'static str, ScreenError> {
    let mut pacer = FramePacer::new(job.fps);

    job.sink.open_segment()?;
    let (mut seg_start, _) = job.sync.wait(BARRIER_TIMEOUT);
    // Audio heard while the other screens were still opening predates frame
    // zero, so it would play early.
    if let Some(sound) = &mut job.sound {
        sound.discard();
    }
    let mut banked = Duration::ZERO;
    // Effect animation advances on real elapsed time between composites,
    // which is the capture rate, not the output frame rate.
    let mut last_draw = Instant::now();

    loop {
        match job.control.state() {
            RunState::Stopped => break,
            RunState::Paused => {
                // Top the segment up before closing it. Without this a stall
                // right before the pause silently shortens the clip, and with
                // audio it desyncs the tail.
                // Measure before closing: close_segment waits for the
                // encoder to finalise its container, and that shutdown time
                // is not recorded footage.
                let in_segment = seg_start.elapsed();
                let owed = pacer.due(in_segment);
                forward_audio(job, banked + in_segment);
                job.sink.close_segment(owed)?;
                banked += in_segment;
                job.elapsed.set(banked);

                if job.control.wait_while_paused() == RunState::Stopped {
                    return Ok(job.sink.encoder_label());
                }

                job.sink.open_segment()?;
                pacer.restart();
                let (t0, _) = job.sync.wait(BARRIER_TIMEOUT);
                seg_start = t0;
                // The microphone kept capturing through the pause. That audio
                // is not part of the recording, just as the paused frames
                // are not.
                if let Some(sound) = &mut job.sound {
                    sound.discard();
                }
                continue;
            }
            RunState::Running => {}
        }

        // Drain the microphone before pacing, so audio keeps flowing even on
        // a still screen where no video frame is due.
        let in_segment = seg_start.elapsed();
        forward_audio(job, banked + in_segment);

        job.elapsed.set(banked + in_segment);

        let due = pacer.due(in_segment);
        if due == 0 {
            std::thread::sleep(pacer.idle());
            continue;
        }

        let frame = match job.source.next_frame(FRAME_TIMEOUT) {
            Ok(f) => f,
            // A screen that has produced nothing yet is not an error worth
            // killing the recording over; the deadline just expired.
            Err(CaptureError::Timeout) => continue,
            Err(e) => return Err(e.into()),
        };

        let encoded: &[u8] = match job.compositor.as_mut() {
            None => frame.pixels,
            Some(c) => {
                if let Some(rx) = &job.events {
                    for event in rx.try_iter() {
                        match event {
                            PointerEvent::Wheel { steps } => c.on_wheel(steps),
                            PointerEvent::Reset => c.on_reset(),
                            PointerEvent::Click { at } => c.on_click(at),
                        }
                    }
                }
                let dt = last_draw.elapsed().as_secs_f64();
                last_draw = Instant::now();
                // Hold the camera frame alive for the borrow the view takes.
                let cam_frame = job.camera.as_ref().and_then(|c| c.latest());
                let cam = match (&cam_frame, &job.camera) {
                    (Some(f), Some(src)) => Some(CameraView {
                        frame: f,
                        rect: src.rect(),
                    }),
                    _ => None,
                };
                c.render(frame.pixels, (job.pointer)(), dt, cam)
            }
        };

        // Written `due` times rather than once: when capture falls behind,
        // repeating the last frame keeps playback at real-time speed instead
        // of letting it run fast.
        let mut restarted = false;
        for _ in 0..due {
            match job.sink.write(encoded)? {
                WriteStatus::Ok => {}
                WriteStatus::SegmentRestarted { encoder } => {
                    let _ = job.notices.send(format!(
                        "{}: encoder failed, continuing with {encoder}",
                        job.label
                    ));
                    restarted = true;
                    break;
                }
            }
        }

        if restarted {
            // The dead segment's frames are gone. Restart pacing from now and
            // do not bank the abandoned interval, or the reported duration
            // would count footage that no longer exists.
            pacer.restart();
            seg_start = Instant::now();
            continue;
        }
        pacer.advance(due);
    }

    let in_segment = seg_start.elapsed();
    let owed = pacer.due(in_segment);
    forward_audio(job, banked + in_segment);
    job.sink.close_segment(owed)?;
    banked += in_segment;
    job.elapsed.set(banked);
    Ok(job.sink.encoder_label())
}

/// Hand the sound since the last call to the encoder.
fn forward_audio(job: &mut ScreenJob, recorded: Duration) {
    let Some(sound) = job.sound.as_mut() else {
        return;
    };
    for chunk in sound.take(recorded) {
        if let Err(e) = job.sink.write_audio(&chunk) {
            tracing::warn!(error = %e, "dropping audio");
        }
    }
}

#[cfg(test)]
pub(crate) mod fakes {
    use super::*;
    use screenilux_core::geometry::Rect;
    use screenilux_core::ports::Frame;
    use std::path::Path;
    use std::sync::Mutex;

    /// A screen that always has a frame ready.
    pub struct FakeSource {
        pub pixels: Vec<u8>,
        pub bounds: Rect,
        /// Frames to report as unchanged before the next fresh one.
        pub stall: Arc<AtomicU64>,
    }

    impl FakeSource {
        pub fn new(width: u32, height: u32) -> Self {
            Self {
                pixels: vec![7u8; width as usize * height as usize * 4],
                bounds: Rect::new(0, 0, width, height),
                stall: Arc::new(AtomicU64::new(0)),
            }
        }
    }

    impl ScreenSource for FakeSource {
        fn next_frame(&mut self, _timeout: Duration) -> Result<Frame<'_>, CaptureError> {
            let changed = self.stall.load(Ordering::Relaxed) == 0;
            Ok(Frame {
                pixels: &self.pixels,
                width: self.bounds.width,
                height: self.bounds.height,
                changed,
            })
        }

        fn bounds(&self) -> Rect {
            self.bounds
        }
    }

    #[derive(Debug, Default)]
    pub struct SinkLog {
        pub frames: u64,
        pub segments: Vec<u64>,
        pub pads: Vec<u64>,
        pub finished: bool,
        /// Write number at which to simulate an encoder death, once.
        pub fail_at: Option<u64>,
        pub fell_back: bool,
    }

    #[derive(Clone, Default)]
    pub struct FakeSink(pub Arc<Mutex<SinkLog>>);

    impl FakeSink {
        pub fn log(&self) -> std::sync::MutexGuard<'_, SinkLog> {
            self.0.lock().expect("sink log poisoned")
        }
    }

    impl VideoSink for FakeSink {
        fn open_segment(&mut self) -> Result<(), SinkError> {
            self.log().segments.push(0);
            Ok(())
        }

        fn write(&mut self, _frame: &[u8]) -> Result<WriteStatus, SinkError> {
            let mut log = self.log();
            log.frames += 1;
            if let Some(at) = log.fail_at
                && log.frames >= at
            {
                log.fail_at = None;
                log.fell_back = true;
                log.segments.push(0);
                return Ok(WriteStatus::SegmentRestarted { encoder: "CPU" });
            }
            if let Some(current) = log.segments.last_mut() {
                *current += 1;
            }
            Ok(WriteStatus::Ok)
        }

        fn close_segment(&mut self, pad: u64) -> Result<(), SinkError> {
            let mut log = self.log();
            log.pads.push(pad);
            if let Some(current) = log.segments.last_mut() {
                *current += pad;
            }
            log.frames += pad;
            Ok(())
        }

        fn finish(&mut self, _dest: &Path) -> Result<(), SinkError> {
            self.log().finished = true;
            Ok(())
        }

        fn encoder_label(&self) -> &'static str {
            "fake"
        }
    }
}

#[cfg(test)]
mod tests {
    use super::fakes::*;
    use super::*;
    use crossbeam_channel::unbounded;

    fn job(sink: FakeSink, fps: u32, control: Arc<Control>) -> (ScreenJob, Arc<Elapsed>) {
        let elapsed = Arc::new(Elapsed::default());
        let (notices, _rx) = unbounded();
        (
            ScreenJob {
                source: Box::new(FakeSource::new(64, 48)),
                sink: Box::new(sink),
                fps,
                control,
                sync: Arc::new(SyncClock::new(1)),
                elapsed: Arc::clone(&elapsed),
                label: "Screen 1".into(),
                notices,
                compositor: None,
                events: None,
                pointer: Arc::new(|| Point::new(0, 0)),
                camera: None,
                sound: None,
            },
            elapsed,
        )
    }

    #[test]
    fn records_roughly_real_time_worth_of_frames() {
        let sink = FakeSink::default();
        let control = Arc::new(Control::new());
        let (j, elapsed) = job(sink.clone(), 30, Arc::clone(&control));

        let stopper = Arc::clone(&control);
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(400));
            stopper.stop();
        });
        run(j).unwrap();

        let frames = sink.log().frames;
        let secs = elapsed.get().as_secs_f64();
        let expected = (secs * 30.0).round() as u64;
        assert!(
            frames.abs_diff(expected) <= 2,
            "wrote {frames} frames for {secs:.3}s at 30fps (expected ~{expected})"
        );
    }

    #[test]
    fn a_pause_closes_a_segment_and_a_resume_opens_another() {
        let sink = FakeSink::default();
        let control = Arc::new(Control::new());
        let (j, _) = job(sink.clone(), 30, Arc::clone(&control));

        let c = Arc::clone(&control);
        let driver = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(200));
            c.pause();
            std::thread::sleep(Duration::from_millis(200));
            c.resume();
            std::thread::sleep(Duration::from_millis(200));
            c.stop();
        });
        run(j).unwrap();
        driver.join().unwrap();

        let log = sink.log();
        assert_eq!(
            log.segments.len(),
            2,
            "expected one segment per run interval"
        );
        assert!(log.segments.iter().all(|&n| n > 0), "both must hold frames");
    }

    #[test]
    fn paused_time_is_not_counted_as_recorded() {
        let sink = FakeSink::default();
        let control = Arc::new(Control::new());
        let (j, elapsed) = job(sink.clone(), 30, Arc::clone(&control));

        let c = Arc::clone(&control);
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(200));
            c.pause();
            std::thread::sleep(Duration::from_millis(400)); // long pause
            c.resume();
            std::thread::sleep(Duration::from_millis(200));
            c.stop();
        });
        let start = Instant::now();
        run(j).unwrap();
        let wall = start.elapsed();

        let recorded = elapsed.get();
        assert!(
            recorded < wall - Duration::from_millis(250),
            "recorded {recorded:?} of {wall:?} wall time; the pause should be excluded"
        );
    }

    #[test]
    fn every_segment_is_padded_before_it_closes() {
        let sink = FakeSink::default();
        let control = Arc::new(Control::new());
        let (j, _) = job(sink.clone(), 30, Arc::clone(&control));

        let c = Arc::clone(&control);
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(150));
            c.pause();
            std::thread::sleep(Duration::from_millis(100));
            c.resume();
            std::thread::sleep(Duration::from_millis(150));
            c.stop();
        });
        run(j).unwrap();

        let log = sink.log();
        assert_eq!(log.pads.len(), 2, "each segment close must offer a pad");
    }

    #[test]
    fn an_encoder_failure_restarts_the_segment_and_keeps_recording() {
        let sink = FakeSink::default();
        sink.log().fail_at = Some(5);
        let control = Arc::new(Control::new());
        let (j, _) = job(sink.clone(), 30, Arc::clone(&control));

        let c = Arc::clone(&control);
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(400));
            c.stop();
        });
        run(j).unwrap();

        let log = sink.log();
        assert!(log.fell_back, "the failure should have been exercised");
        assert_eq!(log.segments.len(), 2, "a fresh segment should have opened");
        assert!(
            log.segments[1] > 0,
            "recording must continue after the fallback"
        );
    }

    #[test]
    fn an_abandoned_segment_does_not_inflate_the_duration() {
        let sink = FakeSink::default();
        sink.log().fail_at = Some(3);
        let control = Arc::new(Control::new());
        let (j, elapsed) = job(sink.clone(), 30, Arc::clone(&control));

        let c = Arc::clone(&control);
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(400));
            c.stop();
        });
        let start = Instant::now();
        run(j).unwrap();
        let wall = start.elapsed();

        assert!(
            elapsed.get() <= wall,
            "reported {:?} for {wall:?} of wall time",
            elapsed.get()
        );
    }

    #[test]
    fn stopping_while_paused_still_finishes_cleanly() {
        let sink = FakeSink::default();
        let control = Arc::new(Control::new());
        let (j, _) = job(sink.clone(), 30, Arc::clone(&control));

        let c = Arc::clone(&control);
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(150));
            c.pause();
            std::thread::sleep(Duration::from_millis(100));
            c.stop();
        });
        let start = Instant::now();
        run(j).unwrap();
        assert!(
            start.elapsed() < Duration::from_secs(3),
            "stopping from paused must not hang"
        );
    }
}
