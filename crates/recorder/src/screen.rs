//! One screen's capture-and-encode loop.
//!
//! Everything here is expressed against the `core` ports, so the whole loop —
//! pacing, pause handling, encoder fallback — is testable with fakes, without
//! a display or ffmpeg.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use crossbeam_channel::Sender;
use screenilux_core::ports::{CaptureError, ScreenSource, SinkError, VideoSink, WriteStatus};
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
    let mut banked = Duration::ZERO;

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
                continue;
            }
            RunState::Running => {}
        }

        let in_segment = seg_start.elapsed();
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

        // Written `due` times rather than once: when capture falls behind,
        // repeating the last frame keeps playback at real-time speed instead
        // of letting it run fast.
        let mut restarted = false;
        for _ in 0..due {
            match job.sink.write(frame.pixels)? {
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
    job.sink.close_segment(owed)?;
    banked += in_segment;
    job.elapsed.set(banked);
    Ok(job.sink.encoder_label())
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
