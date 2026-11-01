//! Frame pacing and the cross-screen start rendezvous.

use std::sync::{Condvar, Mutex};
use std::time::{Duration, Instant};

/// Decides how many frames a segment owes at a given moment.
///
/// Pacing is pegged to the wall clock rather than counting iterations: the
/// frame index is always `floor(elapsed * fps)`, so a slow capture is corrected
/// on the next pass instead of accumulating drift. When capture falls behind,
/// the caller writes the previous frame `due` times, which keeps playback at
/// real-time speed rather than letting it run fast.
#[derive(Debug, Clone)]
pub struct FramePacer {
    fps: u32,
    frames: u64,
}

impl FramePacer {
    pub fn new(fps: u32) -> Self {
        assert!(fps > 0, "fps must be positive");
        Self { fps, frames: 0 }
    }

    pub fn fps(&self) -> u32 {
        self.fps
    }

    /// Frames already written to the current segment.
    pub fn frames(&self) -> u64 {
        self.frames
    }

    /// How many frames are owed at `elapsed` into the segment. Zero means the
    /// caller is ahead of schedule and should idle briefly.
    pub fn due(&self, elapsed: Duration) -> u64 {
        let target = (elapsed.as_secs_f64() * self.fps as f64).floor();
        let target = if target <= 0.0 { 0 } else { target as u64 };
        target.saturating_sub(self.frames)
    }

    /// Record that `n` frames were written.
    pub fn advance(&mut self, n: u64) {
        self.frames += n;
    }

    /// Begin a new segment. Called on resume, and after an encoder swap.
    pub fn restart(&mut self) {
        self.frames = 0;
    }

    /// How long to idle when nothing is due. Short enough that we never
    /// oversleep past the next frame boundary.
    pub fn idle(&self) -> Duration {
        Duration::from_secs_f64((1.0 / self.fps as f64 / 4.0).min(0.01))
    }
}

/// Why a rendezvous returned.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Rendezvous {
    /// Every party arrived; the returned instant is shared by all of them.
    Synced,
    /// The rendezvous was abandoned (a peer died, or we are shutting down).
    /// The returned instant is freshly taken for this caller alone.
    Broken,
}

/// One start instant shared by every screen in a recording.
///
/// Encoders do not all spawn in the same millisecond, so each capture thread
/// waits here until its peers are ready and the last arrival stamps a single
/// `t0`. Numbering frames from that shared instant is what keeps frame N of
/// every screen the same moment in time — measured at 0-1 frames of skew,
/// against 30-70ms without it.
///
/// Unlike `std::sync::Barrier` this supports a timeout and an abort, and
/// **always returns a fresh instant on the broken path**. Returning a stale
/// `t0` there is how the Python build could compute a frame debt of thousands
/// and write a burst of duplicate frames; see the port notes.
pub struct SyncClock {
    parties: usize,
    state: Mutex<SyncState>,
    cv: Condvar,
}

#[derive(Debug)]
struct SyncState {
    waiting: usize,
    generation: u64,
    t0: Option<Instant>,
    aborted: bool,
}

impl SyncClock {
    pub fn new(parties: usize) -> Self {
        assert!(parties > 0, "a rendezvous needs at least one party");
        Self {
            parties,
            state: Mutex::new(SyncState {
                waiting: 0,
                generation: 0,
                t0: None,
                aborted: false,
            }),
            cv: Condvar::new(),
        }
    }

    /// Wait for every party, then return the shared start instant.
    ///
    /// Each call is its own round, so the clock re-arms automatically after a
    /// pause without an explicit reset — removing a race where resetting while
    /// a peer was parked would break that peer.
    pub fn wait(&self, timeout: Duration) -> (Instant, Rendezvous) {
        let mut state = self.state.lock().expect("sync clock poisoned");
        if state.aborted {
            return (Instant::now(), Rendezvous::Broken);
        }

        let generation = state.generation;
        state.waiting += 1;

        if state.waiting == self.parties {
            // last to arrive: stamp the shared instant and release everyone
            let now = Instant::now();
            state.t0 = Some(now);
            state.waiting = 0;
            state.generation = state.generation.wrapping_add(1);
            drop(state);
            self.cv.notify_all();
            return (now, Rendezvous::Synced);
        }

        let deadline = Instant::now() + timeout;
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                state.waiting = state.waiting.saturating_sub(1);
                return (Instant::now(), Rendezvous::Broken);
            }
            let (next, wait_result) = self
                .cv
                .wait_timeout(state, remaining)
                .expect("sync clock poisoned");
            state = next;

            if state.generation != generation {
                let t0 = state.t0.unwrap_or_else(Instant::now);
                return (t0, Rendezvous::Synced);
            }
            if state.aborted {
                state.waiting = state.waiting.saturating_sub(1);
                return (Instant::now(), Rendezvous::Broken);
            }
            if wait_result.timed_out() {
                state.waiting = state.waiting.saturating_sub(1);
                return (Instant::now(), Rendezvous::Broken);
            }
        }
    }

    /// Abandon the rendezvous permanently and wake every waiter. Called when a
    /// capture thread dies so its peers cannot block, and on stop.
    pub fn abort(&self) {
        let mut state = self.state.lock().expect("sync clock poisoned");
        state.aborted = true;
        drop(state);
        self.cv.notify_all();
    }
}

impl std::fmt::Debug for SyncClock {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SyncClock")
            .field("parties", &self.parties)
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::thread;

    #[test]
    fn due_tracks_the_wall_clock() {
        let p = FramePacer::new(30);
        assert_eq!(p.due(Duration::from_millis(0)), 0);
        assert_eq!(p.due(Duration::from_millis(33)), 0); // 0.99 frames
        assert_eq!(p.due(Duration::from_millis(34)), 1);
        assert_eq!(p.due(Duration::from_secs(1)), 30);
        assert_eq!(p.due(Duration::from_secs(2)), 60);
    }

    #[test]
    fn a_stall_is_repaid_in_duplicates_not_dropped() {
        let mut p = FramePacer::new(30);
        // keeping up for the first second
        let due = p.due(Duration::from_secs(1));
        assert_eq!(due, 30);
        p.advance(due);

        // then the capture stalls for a full second
        let due = p.due(Duration::from_secs(2));
        assert_eq!(due, 30, "the whole stall must be repaid at once");
        p.advance(due);
        assert_eq!(p.frames(), 60, "output stays at real-time length");
    }

    #[test]
    fn pacing_does_not_accumulate_drift() {
        let mut p = FramePacer::new(30);
        // feed it irregular, jittery elapsed values
        for ms in [40u64, 70, 101, 140, 171, 200, 233, 270, 300] {
            let due = p.due(Duration::from_millis(ms));
            p.advance(due);
        }
        // after 300ms at 30fps the count is exactly floor(0.3*30) = 9
        assert_eq!(p.frames(), 9);
    }

    #[test]
    fn restart_zeroes_the_segment() {
        let mut p = FramePacer::new(30);
        p.advance(p.due(Duration::from_secs(5)));
        assert_eq!(p.frames(), 150);
        p.restart();
        assert_eq!(p.frames(), 0);
        assert_eq!(p.due(Duration::from_millis(100)), 3);
    }

    #[test]
    fn single_party_syncs_immediately() {
        let clock = SyncClock::new(1);
        let (_, how) = clock.wait(Duration::from_secs(1));
        assert_eq!(how, Rendezvous::Synced);
    }

    #[test]
    fn every_party_receives_the_same_instant() {
        let clock = Arc::new(SyncClock::new(3));
        let handles: Vec<_> = (0..3)
            .map(|_| {
                let c = Arc::clone(&clock);
                thread::spawn(move || c.wait(Duration::from_secs(5)))
            })
            .collect();
        let results: Vec<_> = handles.into_iter().map(|h| h.join().unwrap()).collect();
        assert!(results.iter().all(|(_, how)| *how == Rendezvous::Synced));
        let first = results[0].0;
        assert!(
            results.iter().all(|(t, _)| *t == first),
            "all screens must number frames from one instant"
        );
    }

    #[test]
    fn the_clock_rearms_for_the_next_segment() {
        let clock = Arc::new(SyncClock::new(2));
        let mut seen = Vec::new();
        for _ in 0..3 {
            let c = Arc::clone(&clock);
            let h = thread::spawn(move || c.wait(Duration::from_secs(5)));
            let (mine, how) = clock.wait(Duration::from_secs(5));
            let (theirs, _) = h.join().unwrap();
            assert_eq!(how, Rendezvous::Synced);
            assert_eq!(mine, theirs);
            seen.push(mine);
        }
        assert!(
            seen[0] < seen[1] && seen[1] < seen[2],
            "each round is fresh"
        );
    }

    /// The bug this design exists to prevent: a survivor must never resume
    /// numbering from an instant minutes in the past.
    #[test]
    fn broken_rendezvous_returns_a_fresh_instant_not_a_stale_one() {
        let clock = Arc::new(SyncClock::new(2));

        // round one succeeds, so an old t0 is now cached
        let c = Arc::clone(&clock);
        let h = thread::spawn(move || c.wait(Duration::from_secs(5)));
        let (old, _) = clock.wait(Duration::from_secs(5));
        h.join().unwrap();

        thread::sleep(Duration::from_millis(50));

        // now a peer dies and the survivor waits alone
        clock.abort();
        let (recovered, how) = clock.wait(Duration::from_secs(5));
        assert_eq!(how, Rendezvous::Broken);
        assert!(
            recovered > old,
            "a broken wait must re-stamp, or the frame debt explodes"
        );

        // and the debt computed from it is sane, not thousands of frames
        let pacer = FramePacer::new(30);
        let elapsed = recovered.elapsed();
        assert!(pacer.due(elapsed) < 5, "expected a near-zero frame debt");
    }

    #[test]
    fn abort_wakes_a_parked_peer_instead_of_hanging_it() {
        let clock = Arc::new(SyncClock::new(2));
        let c = Arc::clone(&clock);
        let waiter = thread::spawn(move || c.wait(Duration::from_secs(30)));
        thread::sleep(Duration::from_millis(50));

        let start = Instant::now();
        clock.abort();
        let (_, how) = waiter.join().unwrap();
        assert_eq!(how, Rendezvous::Broken);
        assert!(
            start.elapsed() < Duration::from_secs(2),
            "abort must release immediately, not wait out the timeout"
        );
    }

    #[test]
    fn a_lone_waiter_times_out_rather_than_blocking_forever() {
        let clock = SyncClock::new(2);
        let start = Instant::now();
        let (_, how) = clock.wait(Duration::from_millis(100));
        assert_eq!(how, Rendezvous::Broken);
        assert!(start.elapsed() < Duration::from_secs(1));
    }
}
