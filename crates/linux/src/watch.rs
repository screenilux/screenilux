//! Hearing about screens coming and going.
//!
//! X11 announces this through RandR events, but only to a client that keeps
//! a connection and asks. Reading the list every second and a half is
//! simpler and catches the same changes, a little later.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;
use std::time::Duration;

const EVERY: Duration = Duration::from_millis(1500);

/// Calls back whenever the screens change, until dropped.
pub struct DisplayWatcher {
    thread: Option<JoinHandle<()>>,
    stop: Arc<AtomicBool>,
}

impl DisplayWatcher {
    /// Start listening. `on_change` runs on the watcher's own thread, so it
    /// should only signal, not do the work.
    pub fn start(on_change: impl Fn() + Send + 'static) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&stop);
        let thread = std::thread::Builder::new()
            .name("screenilux-displays".into())
            .spawn(move || {
                let mut last = crate::monitors().ok();
                'watch: loop {
                    // Short sleeps, so dropping the watcher never waits long.
                    let mut slept = Duration::ZERO;
                    while slept < EVERY {
                        if flag.load(Ordering::Relaxed) {
                            break 'watch;
                        }
                        std::thread::sleep(Duration::from_millis(100));
                        slept += Duration::from_millis(100);
                    }
                    let now = crate::monitors().ok();
                    if now != last {
                        last = now;
                        on_change();
                    }
                }
            })
            .ok();
        Self { thread, stop }
    }
}

impl Drop for DisplayWatcher {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dropping_stops_listening_quickly() {
        let watcher = DisplayWatcher::start(|| {});
        let start = std::time::Instant::now();
        drop(watcher);
        assert!(start.elapsed() < Duration::from_millis(500));
    }
}
