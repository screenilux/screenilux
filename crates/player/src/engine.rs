//! Playing a recording: one decode thread per screen and one for sound, all
//! following a single clock.
//!
//! Every thread reads the same [`SharedClock`] and decides for itself which
//! frame is due, the way the recorder's capture threads all number frames
//! from one start instant. Nothing is pushed between threads except the
//! finished frame each screen publishes for the UI to pick up.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use screenilux_core::playback::{PlaybackClock, Recording, Track};
use screenilux_win::decoder::{AudioDecoder, VideoDecoder, VideoFrame};

/// How often an idle thread looks at the clock. Short enough that a seek or
/// play is picked up within a frame at 60fps.
const POLL: Duration = Duration::from_millis(5);
/// A screen this far behind the clock jumps ahead rather than racing through
/// every late frame, which would show as fast-forward.
const RESYNC: f64 = 0.25;

/// What every thread needs from the clock in one consistent read.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Snapshot {
    pub position: f64,
    pub playing: bool,
    /// Bumped on every jump in position, so a thread knows to seek rather
    /// than keep decoding forward.
    pub generation: u64,
}

/// The playback clock, shared by the UI and every playback thread.
#[derive(Debug)]
pub struct SharedClock {
    clock: Mutex<PlaybackClock>,
    generation: AtomicU64,
}

impl SharedClock {
    pub fn new(duration: f64) -> Self {
        Self {
            clock: Mutex::new(PlaybackClock::new(duration)),
            generation: AtomicU64::new(0),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, PlaybackClock> {
        self.clock.lock().expect("playback clock poisoned")
    }

    pub fn snapshot(&self) -> Snapshot {
        let c = self.lock();
        Snapshot {
            position: c.position(Instant::now()),
            playing: c.is_playing(),
            generation: self.generation.load(Ordering::Acquire),
        }
    }

    pub fn duration(&self) -> f64 {
        self.lock().duration()
    }

    pub fn position(&self) -> f64 {
        self.lock().position(Instant::now())
    }

    pub fn is_playing(&self) -> bool {
        self.lock().is_playing()
    }

    pub fn play(&self) {
        let now = Instant::now();
        let mut c = self.lock();
        // Playing from the end starts over, which is a jump.
        let restart = c.position(now) >= c.duration();
        c.play(now);
        if restart {
            self.generation.fetch_add(1, Ordering::AcqRel);
        }
    }

    pub fn pause(&self) {
        self.lock().pause(Instant::now());
    }

    pub fn toggle(&self) {
        if self.is_playing() {
            self.pause();
        } else {
            self.play();
        }
    }

    pub fn seek(&self, t: f64) {
        self.lock().seek(t, Instant::now());
        self.generation.fetch_add(1, Ordering::AcqRel);
    }

    /// Pause on reaching the end. Returns true if it just did.
    pub fn settle(&self) -> bool {
        let now = Instant::now();
        let mut c = self.lock();
        if c.ended(now) {
            c.pause(now);
            true
        } else {
            false
        }
    }
}

/// The frame one screen is currently showing.
#[derive(Debug, Default)]
pub struct Feed {
    frame: Mutex<Option<Arc<VideoFrame>>>,
    /// Increments per new frame, so the UI uploads a texture only when the
    /// picture actually changed.
    serial: AtomicU64,
    error: Mutex<Option<String>>,
}

impl Feed {
    fn publish(&self, frame: VideoFrame) {
        *self.frame.lock().expect("feed poisoned") = Some(Arc::new(frame));
        self.serial.fetch_add(1, Ordering::AcqRel);
    }

    fn fail(&self, e: String) {
        *self.error.lock().expect("feed poisoned") = Some(e);
    }

    pub fn serial(&self) -> u64 {
        self.serial.load(Ordering::Acquire)
    }

    pub fn frame(&self) -> Option<Arc<VideoFrame>> {
        self.frame.lock().expect("feed poisoned").clone()
    }

    pub fn error(&self) -> Option<String> {
        self.error.lock().expect("feed poisoned").clone()
    }
}

type Repaint = Arc<dyn Fn() + Send + Sync>;

/// A recording being played.
pub struct Player {
    recording: Recording,
    clock: Arc<SharedClock>,
    feeds: Vec<Arc<Feed>>,
    sound_error: Arc<Mutex<Option<String>>>,
    stop: Arc<AtomicBool>,
    threads: Vec<JoinHandle<()>>,
}

impl Player {
    /// Start a thread per screen, and one for sound if a track has it.
    /// Playback starts paused on the first frame.
    ///
    /// `repaint` is called whenever a screen has a new frame, so the UI only
    /// redraws when there is something new to show.
    pub fn start(recording: Recording, fps: f64, repaint: Repaint) -> Self {
        let clock = Arc::new(SharedClock::new(recording.duration));
        let stop = Arc::new(AtomicBool::new(false));
        let mut feeds = Vec::new();
        let mut threads = Vec::new();

        for (i, track) in recording.tracks.iter().enumerate() {
            let feed = Arc::new(Feed::default());
            feeds.push(Arc::clone(&feed));
            let (track, clock, stop, repaint) = (
                track.clone(),
                Arc::clone(&clock),
                Arc::clone(&stop),
                Arc::clone(&repaint),
            );
            let spawned = std::thread::Builder::new()
                .name(format!("screenilux-play-{}", i + 1))
                .spawn(move || {
                    if let Err(e) = play_video(&track, fps, &clock, &feed, &stop, &*repaint) {
                        tracing::error!(screen = track.label, error = %e, "playback failed");
                        feed.fail(e);
                        repaint();
                    }
                });
            if let Ok(h) = spawned {
                threads.push(h);
            }
        }

        let sound_error = Arc::new(Mutex::new(None));
        if let Some(track) = recording.tracks.iter().find(|t| t.audio).cloned() {
            let (clock, stop, err) = (
                Arc::clone(&clock),
                Arc::clone(&stop),
                Arc::clone(&sound_error),
            );
            let spawned = std::thread::Builder::new()
                .name("screenilux-play-sound".into())
                .spawn(move || {
                    if let Err(e) = play_sound(&track, &clock, &stop) {
                        tracing::warn!(error = %e, "sound playback stopped");
                        if let Ok(mut slot) = err.lock() {
                            *slot = Some(e);
                        }
                    }
                });
            if let Ok(h) = spawned {
                threads.push(h);
            }
        }

        Self {
            recording,
            clock,
            feeds,
            sound_error,
            stop,
            threads,
        }
    }

    pub fn recording(&self) -> &Recording {
        &self.recording
    }

    pub fn clock(&self) -> &SharedClock {
        &self.clock
    }

    pub fn feed(&self, track: usize) -> Option<&Feed> {
        self.feeds.get(track).map(|f| &**f)
    }

    pub fn sound_error(&self) -> Option<String> {
        self.sound_error.lock().ok().and_then(|e| e.clone())
    }
}

impl Drop for Player {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        for h in self.threads.drain(..) {
            let _ = h.join();
        }
    }
}

/// One screen: decode ahead of the clock and publish each frame when due.
fn play_video(
    track: &Track,
    fps: f64,
    clock: &SharedClock,
    feed: &Feed,
    stop: &AtomicBool,
    repaint: &(dyn Fn() + Send + Sync),
) -> Result<(), String> {
    let mut dec = VideoDecoder::open(&track.path).map_err(|e| e.to_string())?;
    // The opening frame, so a paused player is not a black rectangle.
    if let Some(f) = dec.frame_at(0.0, fps).map_err(|e| e.to_string())? {
        feed.publish(f);
        repaint();
    }
    let mut seen_generation = clock.snapshot().generation;
    let mut pending: Option<VideoFrame> = None;
    let mut at_end = false;

    while !stop.load(Ordering::Relaxed) {
        let now = clock.snapshot();

        if now.generation != seen_generation {
            seen_generation = now.generation;
            pending = None;
            at_end = false;
            if let Some(f) = dec.frame_at(now.position, fps).map_err(|e| e.to_string())? {
                feed.publish(f);
                repaint();
            }
            continue;
        }
        if !now.playing || at_end {
            std::thread::sleep(POLL);
            continue;
        }

        if pending.is_none() {
            pending = dec.next_frame().map_err(|e| e.to_string())?;
            if pending.is_none() {
                at_end = true;
                continue;
            }
        }
        let due = pending.as_ref().map(|f| f.t).unwrap_or(f64::MAX);
        if now.position - due > RESYNC {
            // Fell behind (a slow disk, a starved CPU): jump to the present
            // rather than play the backlog quickly.
            pending = None;
            if let Some(f) = dec.frame_at(now.position, fps).map_err(|e| e.to_string())? {
                feed.publish(f);
                repaint();
            }
        } else if due <= now.position {
            if let Some(f) = pending.take() {
                feed.publish(f);
                repaint();
            }
        } else {
            std::thread::sleep(Duration::from_secs_f64((due - now.position).min(0.005)));
        }
    }
    Ok(())
}

/// Sound: keep the speaker fed from the decoder while playing, and restart
/// both from the clock's position after any pause or seek.
fn play_sound(track: &Track, clock: &SharedClock, stop: &AtomicBool) -> Result<(), String> {
    let mut dec = AudioDecoder::open(&track.path).map_err(|e| e.to_string())?;
    let mut speaker = screenilux_win::Speaker::open()?;
    let mut queue: Vec<u8> = Vec::new();
    let mut seen_generation = u64::MAX;
    let mut was_playing = false;
    let mut exhausted = false;

    while !stop.load(Ordering::Relaxed) {
        let now = clock.snapshot();
        let jumped = now.generation != seen_generation;
        if !now.playing {
            if was_playing {
                speaker.stop();
            }
            was_playing = false;
            seen_generation = now.generation;
            std::thread::sleep(POLL);
            continue;
        }
        // Starting, resuming and seeking all land here: the speaker's queue
        // no longer matches the clock, so drop it and decode from the clock.
        if jumped || !was_playing {
            speaker.stop();
            queue = dec.seek(now.position).map_err(|e| e.to_string())?;
            exhausted = false;
            seen_generation = now.generation;
            was_playing = true;
            speaker.start();
        }

        while queue.len() < speaker.room() && !exhausted {
            match dec.next_chunk().map_err(|e| e.to_string())? {
                Some((_, pcm)) => queue.extend_from_slice(&pcm),
                None => exhausted = true,
            }
        }
        let taken = speaker.write(&queue);
        queue.drain(..taken);
        std::thread::sleep(POLL);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use screenilux_core::ports::VideoSink;
    use std::path::{Path, PathBuf};

    #[test]
    fn a_seek_bumps_the_generation_and_play_does_not() {
        let c = SharedClock::new(10.0);
        let g0 = c.snapshot().generation;
        c.play();
        assert_eq!(c.snapshot().generation, g0, "resuming is not a jump");
        c.seek(4.0);
        assert_eq!(c.snapshot().generation, g0 + 1);
        assert!((c.position() - 4.0).abs() < 0.05);
    }

    #[test]
    fn playing_from_the_end_starts_over_as_a_jump() {
        let c = SharedClock::new(1.0);
        c.seek(1.0);
        let g = c.snapshot().generation;
        c.play();
        assert!(c.position() < 0.1);
        assert_eq!(c.snapshot().generation, g + 1);
    }

    #[test]
    fn settle_pauses_at_the_end() {
        let c = SharedClock::new(0.05);
        c.play();
        std::thread::sleep(Duration::from_millis(80));
        assert!(c.settle());
        assert!(!c.is_playing());
        assert!(!c.settle(), "only once");
    }

    fn encode(dir: &Path, name: &str, frames: u32, shade: impl Fn(u32) -> u8) -> PathBuf {
        let (w, h) = (320u32, 240u32);
        let dest = dir.join(name);
        let mut sink =
            screenilux_win::NativeSink::new(w, h, 30, false, dir.join(format!("{name}.part")))
                .unwrap();
        sink.open_segment().unwrap();
        for n in 0..frames {
            sink.write(&vec![shade(n); (w * h * 4) as usize]).unwrap();
        }
        sink.close_segment(0).unwrap();
        sink.finish(&dest).unwrap();
        dest
    }

    fn wait_for(mut cond: impl FnMut() -> bool) -> bool {
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            if cond() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        false
    }

    /// Brightness of the frame a feed is showing.
    fn shade(feed: &Feed) -> Option<u8> {
        feed.frame().map(|f| f.rgba[(f.rgba.len() / 2) & !3])
    }

    #[test]
    fn screens_show_their_first_frame_then_follow_seeks_and_play() {
        let dir = screenilux_recorder::paths::TempDir::new("screenilux-engine").unwrap();
        // Dark for the first second, bright for the second.
        let step = |n: u32| if n < 30 { 30 } else { 220 };
        let a = encode(dir.path(), "a.mp4", 60, step);
        let b = encode(dir.path(), "b.mp4", 60, step);
        let mut rec = Recording::single(&a, 320, 240, 2.0, false);
        let mut second = rec.tracks[0].clone();
        second.path = b;
        second.id = "b.mp4".into();
        rec.tracks.push(second);

        let player = Player::start(rec, 30.0, Arc::new(|| {}));
        let (fa, fb) = (player.feed(0).unwrap(), player.feed(1).unwrap());
        assert!(
            wait_for(|| shade(fa).is_some() && shade(fb).is_some()),
            "both screens should show a frame while paused"
        );
        assert!(shade(fa).unwrap() < 100, "starts on the dark half");

        // A seek while paused updates the picture without playing.
        player.clock().seek(1.5);
        assert!(wait_for(
            || shade(fa).unwrap_or(0) > 150 && shade(fb).unwrap_or(0) > 150
        ));
        assert!(!player.clock().is_playing());

        // Play from the start: frames advance on their own, and both screens
        // cross into the bright half together.
        player.clock().seek(0.0);
        assert!(wait_for(|| shade(fa).unwrap_or(255) < 100));
        player.clock().play();
        assert!(wait_for(|| shade(fa).unwrap_or(0) > 150));
        assert!(wait_for(|| shade(fb).unwrap_or(0) > 150));
        assert!(fa.error().is_none() && fb.error().is_none());
    }

    #[test]
    fn sound_plays_alongside_the_picture() {
        if screenilux_win::Speaker::open().is_err() {
            return; // no playback device on this machine
        }
        let dir = screenilux_recorder::paths::TempDir::new("screenilux-engine-sound").unwrap();
        let (w, h) = (320u32, 240u32);
        let path = dir.path().join("sound.mp4");
        let mut sink =
            screenilux_win::NativeSink::new(w, h, 30, true, dir.path().join("sound.part.mp4"))
                .unwrap();
        sink.open_segment().unwrap();
        // Quiet: this really plays through the speakers.
        let second: Vec<u8> = (0..48_000u32)
            .flat_map(|i| {
                let s =
                    ((i as f64 * 330.0 * std::f64::consts::TAU / 48_000.0).sin() * 600.0) as i16;
                let b = s.to_le_bytes();
                [b[0], b[1], b[0], b[1]]
            })
            .collect();
        for chunk in second.chunks(48_000 * 4 / 30) {
            sink.write(&vec![90u8; (w * h * 4) as usize]).unwrap();
            sink.write_audio(chunk).unwrap();
        }
        sink.close_segment(0).unwrap();
        sink.finish(&path).unwrap();

        let rec = Recording::single(&path, w, h, 1.0, true);
        let player = Player::start(rec, 30.0, Arc::new(|| {}));
        player.clock().play();
        std::thread::sleep(Duration::from_millis(400));
        player.clock().seek(0.2);
        std::thread::sleep(Duration::from_millis(300));
        player.clock().pause();
        std::thread::sleep(Duration::from_millis(50));
        assert_eq!(player.sound_error(), None, "sound should decode and play");
    }

    #[test]
    fn a_broken_track_reports_its_error_instead_of_hanging() {
        let dir = screenilux_recorder::paths::TempDir::new("screenilux-engine-bad").unwrap();
        let bad = dir.path().join("bad.mp4");
        std::fs::write(&bad, b"nope").unwrap();
        let rec = Recording::single(&bad, 320, 240, 1.0, false);
        let player = Player::start(rec, 30.0, Arc::new(|| {}));
        let feed = player.feed(0).unwrap();
        assert!(wait_for(|| feed.error().is_some()));
    }
}
