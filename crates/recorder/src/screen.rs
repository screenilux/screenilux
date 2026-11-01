//! One screen's capture-and-encode loop.
//!
//! Everything here is expressed against the `core` ports, so the whole loop —
//! pacing, pause handling, audio forwarding — is testable with fakes, without
//! a display or an encoder.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use crossbeam_channel::Receiver;
use screenilux_core::click::ClickMixer;
use screenilux_core::compositor::{CameraView, Compositor};
use screenilux_core::geometry::Point;
use screenilux_core::mix::Mixer;
use screenilux_core::ports::{
    CameraSource, CaptureError, PointerEvent, ScreenSource, SinkError, VideoSink,
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
const BLOCK: usize = screenilux_platform::audio::CHANNELS as usize * 2;

/// How far behind the picture the sound is written while recording, so each
/// source has a little queued and a late packet does not leave a gap. Flushed
/// at every pause and at the end, so it never shifts anything.
const CUSHION: Duration = Duration::from_millis(80);

/// The most a sound source may queue before its oldest sound is dropped.
const MAX_QUEUED: Duration = Duration::from_millis(500);

/// What goes into the sound track: the microphone, the computer's own sound,
/// clicks, or any mix of them.
///
/// The microphone alone sets the pace of the track, as it always has. With
/// the computer's sound, which only arrives while something plays, or with
/// neither, the track is written at the pace of the recording and every
/// source is mixed into it, so clicks and sound land at the right time.
pub struct Sound {
    mic: Option<screenilux_platform::AudioCapture>,
    system: Option<screenilux_platform::AudioCapture>,
    /// One message per mouse click, anywhere on the desktop.
    clicks: Option<Receiver<()>>,
    mixer: ClickMixer,
    /// The microphone and the system sound, queued until written.
    bus: Mixer,
    /// Sample frames written so far.
    written: u64,
}

impl Sound {
    pub fn new(
        mic: Option<screenilux_platform::AudioCapture>,
        system: Option<screenilux_platform::AudioCapture>,
        clicks: Option<Receiver<()>>,
    ) -> Self {
        let rate = screenilux_platform::audio::SAMPLE_RATE;
        Self {
            mic,
            system,
            clicks,
            mixer: ClickMixer::new(rate, screenilux_platform::audio::CHANNELS),
            bus: Mixer::new(
                2,
                screenilux_platform::audio::CHANNELS,
                (MAX_QUEUED.as_secs_f64() * rate as f64) as usize,
            ),
            written: 0,
        }
    }

    /// Drop what was heard while nothing was being recorded: before frame
    /// zero, or during a pause.
    fn discard(&mut self) {
        for source in [&self.mic, &self.system].into_iter().flatten() {
            source.discard();
        }
        if let Some(clicks) = &self.clicks {
            while clicks.try_recv().is_ok() {}
        }
        self.mixer.silence();
        self.bus.clear();
    }

    /// The audio since the last call, clicks mixed in. `recorded` is how
    /// much has been recorded, which paces the track unless the microphone
    /// is its only source. `flush` writes right up to `recorded`, at the end
    /// of a stretch of recording.
    fn take(&mut self, recorded: Duration, flush: bool) -> Vec<Vec<u8>> {
        if let Some(clicks) = &self.clicks {
            for () in clicks.try_iter() {
                self.mixer.trigger();
            }
        }
        let mut chunks = match (&self.mic, &self.system) {
            (Some(mic), None) => mic.drain(),
            (mic, system) => {
                for (i, source) in [mic, system].into_iter().enumerate() {
                    if let Some(source) = source {
                        for chunk in source.drain() {
                            self.bus.push(i, &chunk);
                        }
                    }
                }
                let upto = if flush || system.is_none() {
                    recorded
                } else {
                    recorded.saturating_sub(CUSHION)
                };
                let due =
                    (upto.as_secs_f64() * screenilux_platform::audio::SAMPLE_RATE as f64) as u64;
                match due.saturating_sub(self.written) {
                    0 => Vec::new(),
                    n => vec![self.bus.pull(n as usize)],
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

/// Run one screen to completion.
pub fn run(mut job: ScreenJob) -> Result<(), ScreenError> {
    let result = capture_loop(&mut job);
    if result.is_err() {
        // Never leave peers parked at the barrier waiting for a screen that
        // has already died.
        job.sync.abort();
    }
    result
}

fn capture_loop(job: &mut ScreenJob) -> Result<(), ScreenError> {
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
                forward_audio(job, banked + in_segment, true);
                job.sink.close_segment(owed)?;
                banked += in_segment;
                job.elapsed.set(banked);

                if job.control.wait_while_paused() == RunState::Stopped {
                    return Ok(());
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
        forward_audio(job, banked + in_segment, false);

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
        for _ in 0..due {
            job.sink.write(encoded)?;
        }
        pacer.advance(due);
    }

    let in_segment = seg_start.elapsed();
    let owed = pacer.due(in_segment);
    forward_audio(job, banked + in_segment, true);
    job.sink.close_segment(owed)?;
    banked += in_segment;
    job.elapsed.set(banked);
    Ok(())
}

/// Hand the sound since the last call to the encoder.
fn forward_audio(job: &mut ScreenJob, recorded: Duration, flush: bool) {
    let Some(sound) = job.sound.as_mut() else {
        return;
    };
    for chunk in sound.take(recorded, flush) {
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

        fn write(&mut self, _frame: &[u8]) -> Result<(), SinkError> {
            let mut log = self.log();
            log.frames += 1;
            if let Some(current) = log.segments.last_mut() {
                *current += 1;
            }
            Ok(())
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
    }
}

#[cfg(test)]
mod tests {
    use super::fakes::*;
    use super::*;

    fn job(sink: FakeSink, fps: u32, control: Arc<Control>) -> (ScreenJob, Arc<Elapsed>) {
        let elapsed = Arc::new(Elapsed::default());
        (
            ScreenJob {
                source: Box::new(FakeSource::new(64, 48)),
                sink: Box::new(sink),
                fps,
                control,
                sync: Arc::new(SyncClock::new(1)),
                elapsed: Arc::clone(&elapsed),
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
