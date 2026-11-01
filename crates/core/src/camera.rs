//! A live camera feed as the rest of the app sees it: the latest frame, where
//! the bubble sits, and why the camera stopped if it did.
//!
//! The same on every platform; only the thread that fills it differs. Frames
//! are published as the latest value rather than queued: the camera runs at
//! its own pace, every screen's compositor reads the same frame, and a stale
//! frame is worse than a repeated one.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock};

use crate::geometry::Rect;
use crate::ports::{CameraFrame, CameraSource};

/// Shared between the thread pulling frames and everything reading them.
pub struct CameraFeed {
    latest: RwLock<Option<Arc<CameraFrame>>>,
    position: Mutex<Rect>,
    error: Mutex<Option<String>>,
    seq: AtomicU64,
    stop: AtomicBool,
}

impl CameraFeed {
    /// A feed with no frames yet, its bubble at `bubble`.
    pub fn new(bubble: Rect) -> Self {
        Self {
            latest: RwLock::new(None),
            position: Mutex::new(bubble),
            error: Mutex::new(None),
            seq: AtomicU64::new(0),
            stop: AtomicBool::new(false),
        }
    }

    /// Make `pixels`, tightly packed BGRA, the latest frame.
    pub fn publish(&self, pixels: Vec<u8>, width: u32, height: u32) {
        let frame = Arc::new(CameraFrame {
            pixels,
            width,
            height,
            seq: self.seq.fetch_add(1, Ordering::Relaxed),
        });
        if let Ok(mut slot) = self.latest.write() {
            *slot = Some(frame);
        }
    }

    /// Say why the camera is not producing frames.
    pub fn fail(&self, reason: impl Into<String>) {
        if let Ok(mut e) = self.error.lock() {
            *e = Some(reason.into());
        }
    }

    /// Move the bubble. Called while dragging, so it is cheap.
    pub fn move_to(&self, left: i32, top: i32) {
        if let Ok(mut p) = self.position.lock() {
            p.left = left;
            p.top = top;
        }
    }

    /// Ask the thread filling the feed to finish.
    pub fn request_stop(&self) {
        self.stop.store(true, Ordering::SeqCst);
    }

    /// Whether the thread filling the feed should finish.
    pub fn stopping(&self) -> bool {
        self.stop.load(Ordering::SeqCst)
    }
}

impl CameraSource for CameraFeed {
    fn latest(&self) -> Option<Arc<CameraFrame>> {
        self.latest.read().ok()?.clone()
    }

    fn rect(&self) -> Rect {
        self.position
            .lock()
            .map(|p| *p)
            .unwrap_or(Rect::new(0, 0, 0, 0))
    }

    fn error(&self) -> Option<String> {
        self.error.lock().ok()?.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frames_are_numbered_as_they_are_published() {
        let feed = CameraFeed::new(Rect::new(0, 0, 10, 10));
        assert!(feed.latest().is_none());
        feed.publish(vec![0; 16], 2, 2);
        feed.publish(vec![0; 16], 2, 2);
        let f = feed.latest().unwrap();
        assert_eq!((f.seq, f.width, f.height), (1, 2, 2));
    }

    #[test]
    fn the_bubble_moves_and_keeps_its_size() {
        let feed = CameraFeed::new(Rect::new(10, 20, 240, 240));
        feed.move_to(500, 600);
        assert_eq!(feed.rect(), Rect::new(500, 600, 240, 240));
    }

    #[test]
    fn a_failure_and_a_stop_are_reported() {
        let feed = CameraFeed::new(Rect::new(0, 0, 1, 1));
        assert!(feed.error().is_none() && !feed.stopping());
        feed.fail("no camera");
        feed.request_stop();
        assert_eq!(feed.error().as_deref(), Some("no camera"));
        assert!(feed.stopping());
    }
}
