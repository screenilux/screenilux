//! Shared run state for every screen in a recording.
//!
//! One `Control` is shared by all capture threads, so pause and stop reach
//! them together rather than screen by screen. Threads park on the condvar
//! while paused instead of spinning on a flag.

use std::sync::{Condvar, Mutex};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunState {
    Running,
    Paused,
    Stopped,
}

#[derive(Debug)]
pub struct Control {
    state: Mutex<RunState>,
    changed: Condvar,
}

impl Default for Control {
    fn default() -> Self {
        Self::new()
    }
}

impl Control {
    pub fn new() -> Self {
        Self {
            state: Mutex::new(RunState::Running),
            changed: Condvar::new(),
        }
    }

    pub fn state(&self) -> RunState {
        *self.state.lock().expect("control poisoned")
    }

    fn set(&self, next: RunState) {
        let mut st = self.state.lock().expect("control poisoned");
        // Stopped is terminal: a late pause must not resurrect a recording
        // that is already tearing down.
        if *st == RunState::Stopped {
            return;
        }
        *st = next;
        drop(st);
        self.changed.notify_all();
    }

    pub fn pause(&self) {
        self.set(RunState::Paused);
    }

    pub fn resume(&self) {
        self.set(RunState::Running);
    }

    pub fn stop(&self) {
        let mut st = self.state.lock().expect("control poisoned");
        *st = RunState::Stopped;
        drop(st);
        self.changed.notify_all();
    }

    /// Park while paused. Returns the state to act on, which is never
    /// `Paused`.
    pub fn wait_while_paused(&self) -> RunState {
        let mut st = self.state.lock().expect("control poisoned");
        while *st == RunState::Paused {
            st = self.changed.wait(st).expect("control poisoned");
        }
        *st
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    #[test]
    fn starts_running() {
        assert_eq!(Control::new().state(), RunState::Running);
    }

    #[test]
    fn pause_and_resume_round_trip() {
        let c = Control::new();
        c.pause();
        assert_eq!(c.state(), RunState::Paused);
        c.resume();
        assert_eq!(c.state(), RunState::Running);
    }

    #[test]
    fn stop_is_terminal() {
        let c = Control::new();
        c.stop();
        c.pause();
        c.resume();
        assert_eq!(
            c.state(),
            RunState::Stopped,
            "a late pause must not revive a stopping recording"
        );
    }

    #[test]
    fn a_parked_thread_wakes_on_resume() {
        let c = Arc::new(Control::new());
        c.pause();
        let worker = Arc::clone(&c);
        let h = std::thread::spawn(move || worker.wait_while_paused());
        std::thread::sleep(Duration::from_millis(50));
        c.resume();
        assert_eq!(h.join().unwrap(), RunState::Running);
    }

    #[test]
    fn a_parked_thread_wakes_on_stop_rather_than_hanging() {
        let c = Arc::new(Control::new());
        c.pause();
        let worker = Arc::clone(&c);
        let h = std::thread::spawn(move || worker.wait_while_paused());
        std::thread::sleep(Duration::from_millis(50));
        let start = Instant::now();
        c.stop();
        assert_eq!(h.join().unwrap(), RunState::Stopped);
        assert!(start.elapsed() < Duration::from_secs(2));
    }

    #[test]
    fn every_screen_sees_one_pause() {
        let c = Arc::new(Control::new());
        c.pause();
        let handles: Vec<_> = (0..3)
            .map(|_| {
                let w = Arc::clone(&c);
                std::thread::spawn(move || w.wait_while_paused())
            })
            .collect();
        std::thread::sleep(Duration::from_millis(50));
        c.resume();
        for h in handles {
            assert_eq!(h.join().unwrap(), RunState::Running);
        }
    }
}
