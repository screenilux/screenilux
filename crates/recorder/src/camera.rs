//! The camera as one more thing to record, beside the screens.
//!
//! It goes through the same capture-and-encode loop a screen does, so it
//! starts on the same instant, pauses and resumes with them and ends up the
//! same length, and is written to a video of its own. The bubble is then
//! drawn over the screens on playback and export, which is what lets it be
//! moved or left out after the fact instead of being part of their pictures.

use std::sync::Arc;
use std::time::{Duration, Instant};

use screenilux_core::geometry::Rect;
use screenilux_core::ports::{CameraSource, CaptureError, Frame, ScreenSource};

/// How long to wait for the camera's first frame when a recording starts.
/// It is normally already running, for the preview.
const FIRST_FRAME: Duration = Duration::from_secs(3);

/// The camera's latest picture, read as if it were a screen.
pub struct CameraScreen {
    source: Arc<dyn CameraSource>,
    width: u32,
    height: u32,
    pixels: Vec<u8>,
    seq: u64,
}

impl CameraScreen {
    /// Wait for the camera to show what size it is, then read it at that
    /// size, rounded down to even, which the encoder needs. `None` if it
    /// never produced a frame.
    pub fn start(source: Arc<dyn CameraSource>) -> Option<Self> {
        let deadline = Instant::now() + FIRST_FRAME;
        let first = loop {
            if let Some(f) = source.latest() {
                break f;
            }
            if Instant::now() >= deadline || source.error().is_some() {
                return None;
            }
            std::thread::sleep(Duration::from_millis(20));
        };
        let (width, height) = (first.width & !1, first.height & !1);
        if width < 2 || height < 2 {
            return None;
        }
        let mut cam = Self {
            source,
            width,
            height,
            pixels: vec![0; width as usize * height as usize * 4],
            seq: u64::MAX,
        };
        cam.take(&first);
        Some(cam)
    }

    /// The size the camera is recorded at.
    pub fn size(&self) -> (u32, u32) {
        (self.width, self.height)
    }

    /// Copy `frame` in, cropping to the recorded size should the camera
    /// change resolution part way through.
    fn take(&mut self, frame: &screenilux_core::ports::CameraFrame) {
        self.seq = frame.seq;
        let row = self.width as usize * 4;
        let have = frame.width as usize * 4;
        let copy = row.min(have);
        let rows = (self.height as usize).min(frame.height as usize);
        for y in 0..rows {
            self.pixels[y * row..y * row + copy]
                .copy_from_slice(&frame.pixels[y * have..y * have + copy]);
        }
    }
}

impl ScreenSource for CameraScreen {
    fn next_frame(&mut self, _timeout: Duration) -> Result<Frame<'_>, CaptureError> {
        // Never waits: the loop only asks when a frame is due, and the last
        // picture is the right one to repeat until the camera moves on.
        let fresh = self.source.latest().filter(|f| f.seq != self.seq);
        let changed = fresh.is_some();
        if let Some(f) = fresh {
            self.take(&f);
        }
        Ok(Frame {
            pixels: &self.pixels,
            width: self.width,
            height: self.height,
            changed,
        })
    }

    fn bounds(&self) -> Rect {
        Rect::new(0, 0, self.width, self.height)
    }
}

/// The bubble's size on the desktop: its long side as asked for, its short
/// side from the camera's own shape, as the preview window sizes itself.
pub fn bubble_size(asked: Rect, camera: (u32, u32)) -> (u32, u32) {
    let long = asked.width.max(asked.height) as f32;
    let aspect = camera.0 as f32 / camera.1.max(1) as f32;
    let (w, h) = if aspect >= 1.0 {
        (long, long / aspect)
    } else {
        (long * aspect, long)
    };
    ((w.round() as u32).max(2), (h.round() as u32).max(2))
}

#[cfg(test)]
mod tests {
    use super::*;
    use screenilux_core::camera::CameraFeed;

    fn feed_with(width: u32, height: u32, shade: u8) -> Arc<CameraFeed> {
        let feed = Arc::new(CameraFeed::new(Rect::new(0, 0, 320, 180)));
        feed.publish(
            vec![shade; width as usize * height as usize * 4],
            width,
            height,
        );
        feed
    }

    #[test]
    fn the_camera_reads_as_a_screen_of_its_own_size() {
        let feed = feed_with(641, 480, 9);
        let mut cam = CameraScreen::start(feed.clone()).expect("a frame is there");
        assert_eq!(cam.size(), (640, 480), "rounded down to even");
        let f = cam.next_frame(Duration::ZERO).unwrap();
        assert!(!f.changed, "nothing new since the first frame");
        assert_eq!(f.pixels.len(), 640 * 480 * 4);
        assert!(f.pixels.iter().all(|&p| p == 9));

        feed.publish(vec![200; 641 * 480 * 4], 641, 480);
        let f = cam.next_frame(Duration::ZERO).unwrap();
        assert!(f.changed);
        assert!(f.pixels.iter().all(|&p| p == 200));
    }

    #[test]
    fn a_camera_that_never_shows_anything_is_not_recorded() {
        let feed = Arc::new(CameraFeed::new(Rect::new(0, 0, 320, 180)));
        feed.fail("unplugged");
        assert!(CameraScreen::start(feed).is_none());
    }

    #[test]
    fn the_bubble_takes_the_cameras_shape() {
        let asked = Rect::new(0, 0, 320, 180);
        assert_eq!(bubble_size(asked, (1280, 720)), (320, 180));
        assert_eq!(bubble_size(asked, (640, 480)), (320, 240));
        assert_eq!(bubble_size(asked, (480, 640)), (240, 320));
    }
}
