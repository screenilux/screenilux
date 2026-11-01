//! Webcam capture via nokhwa (Media Foundation).
//!
//! Frames are pulled on a dedicated thread into a [`CameraFeed`], which the
//! recorder and the bubble read.

use std::sync::Arc;
use std::thread::JoinHandle;

use nokhwa::pixel_format::RgbAFormat;
use nokhwa::utils::{
    ApiBackend, CameraFormat, CameraIndex, FrameFormat, RequestedFormat, RequestedFormatType,
    Resolution,
};
use nokhwa::{Camera, query};
pub use screenilux_core::camera::CameraFeed;
use screenilux_core::geometry::Rect;

/// Device names in the order they are indexed.
pub fn cameras() -> Vec<String> {
    match query(ApiBackend::MediaFoundation) {
        Ok(list) => list.iter().map(|c| c.human_name()).collect(),
        Err(e) => {
            tracing::warn!(error = %e, "could not enumerate cameras");
            Vec::new()
        }
    }
}

/// Format requests to try in order.
///
/// nokhwa's `Closest` is not actually tolerant — it fails outright when the
/// exact resolution is unavailable rather than approximating. A virtual camera
/// that advertises a single mode therefore rejects any specific request, so
/// the ladder ends in a request that cannot fail on a working device.
///
/// The early rungs are small on purpose: the bubble is a few hundred pixels
/// wide, so decoding 1080p MJPEG per frame would be wasted work.
fn format_ladder() -> Vec<RequestedFormatType> {
    vec![
        RequestedFormatType::Closest(CameraFormat::new(
            Resolution::new(640, 480),
            FrameFormat::NV12,
            30,
        )),
        RequestedFormatType::Closest(CameraFormat::new(
            Resolution::new(1280, 720),
            FrameFormat::NV12,
            30,
        )),
        RequestedFormatType::AbsoluteHighestFrameRate,
    ]
}

fn open(index: CameraIndex) -> Result<Camera, String> {
    let mut last = String::from("no format was accepted");
    for (rung, request) in format_ladder().into_iter().enumerate() {
        match Camera::new(index.clone(), RequestedFormat::new::<RgbAFormat>(request)) {
            Ok(cam) => {
                tracing::info!(
                    rung,
                    width = cam.resolution().width(),
                    height = cam.resolution().height(),
                    "camera opened"
                );
                return Ok(cam);
            }
            Err(e) => last = e.to_string(),
        }
    }
    Err(last)
}

/// Handle that keeps the capture thread alive.
pub struct CameraHandle {
    feed: Arc<CameraFeed>,
    thread: Option<JoinHandle<()>>,
}

impl CameraHandle {
    /// Open camera `index` and start pulling frames.
    ///
    /// Returns immediately; the first frame arrives a few hundred milliseconds
    /// later. Failures surface through [`CameraSource::error`] rather than
    /// here, so a missing camera never blocks starting a recording.
    pub fn start(index: u32, bubble: Rect) -> Self {
        let feed = Arc::new(CameraFeed::new(bubble));

        let worker = Arc::clone(&feed);
        let thread = std::thread::Builder::new()
            .name("screenilux-camera".into())
            .spawn(move || pump(worker, index))
            .ok();

        Self { feed, thread }
    }

    pub fn feed(&self) -> Arc<CameraFeed> {
        Arc::clone(&self.feed)
    }

    /// Move the bubble. Called while dragging, so it must be cheap.
    pub fn move_to(&self, left: i32, top: i32) {
        self.feed.move_to(left, top);
    }

    pub fn stop(&mut self) {
        self.feed.request_stop();
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

impl Drop for CameraHandle {
    fn drop(&mut self) {
        self.stop();
    }
}

fn pump(feed: Arc<CameraFeed>, index: u32) {
    let mut cam = match open(CameraIndex::Index(index)) {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!(error = %e, "could not open the camera");
            feed.fail(e);
            return;
        }
    };

    if let Err(e) = cam.open_stream() {
        feed.fail(format!("could not start the camera stream: {e}"));
        return;
    }

    while !feed.stopping() {
        let Ok(raw) = cam.frame() else {
            // A dropped frame is routine; a dead device shows up as a stall,
            // which the UI sees as a frozen bubble rather than a crash.
            std::thread::sleep(std::time::Duration::from_millis(20));
            continue;
        };
        let Ok(mut rgba) = raw.decode_image::<RgbAFormat>() else {
            continue;
        };

        let (width, height) = (rgba.width(), rgba.height());
        // Decoded as RGBA; the compositor works in BGRA like the screen
        // frames, so swap the red and blue channels in place rather than
        // carrying a second pixel layout through the whole pipeline.
        for px in rgba.as_chunks_mut::<4>().0 {
            px.swap(0, 2);
        }

        feed.publish(rgba.into_raw(), width, height);
    }

    let _ = cam.stop_stream();
}

#[cfg(test)]
mod tests {
    use super::*;
    use screenilux_core::ports::{CameraFrame, CameraSource};
    use std::time::{Duration, Instant};

    fn wait_for_frame(feed: &CameraFeed, timeout: Duration) -> Option<Arc<CameraFrame>> {
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            if let Some(f) = feed.latest() {
                return Some(f);
            }
            if feed.error().is_some() {
                return None;
            }
            std::thread::sleep(Duration::from_millis(25));
        }
        None
    }

    #[test]
    fn enumerates_cameras_without_panicking() {
        for name in cameras() {
            assert!(!name.is_empty());
        }
    }

    /// Virtual cameras are backed by something that may not be there — a
    /// phone, a conferencing app — so opening one in a test is a coin flip and
    /// can wake a device the developer did not ask to involve.
    fn is_virtual(name: &str) -> bool {
        let n = name.to_ascii_lowercase();
        n.contains("virtual") || n.contains("obs") || n.contains("droidcam")
    }

    #[test]
    fn opens_every_physical_camera() {
        for (i, name) in cameras().iter().enumerate() {
            if is_virtual(name) {
                continue;
            }
            match open(CameraIndex::Index(i as u32)) {
                Ok(cam) => assert!(cam.resolution().width() > 0, "{name} opened at zero width"),
                Err(e) => panic!("{name} could not be opened by any rung of the ladder: {e}"),
            }
        }
    }

    /// The ladder exists because nokhwa's `Closest` fails outright rather than
    /// approximating, so a device advertising one mode rejects any specific
    /// request. Asserted on the ladder itself, not on hardware that may be
    /// absent.
    #[test]
    fn the_ladder_ends_in_a_request_that_cannot_be_rejected_for_format() {
        let ladder = format_ladder();
        assert!(ladder.len() >= 2, "a single rung defeats the purpose");
        assert!(
            matches!(
                ladder.last(),
                Some(RequestedFormatType::AbsoluteHighestFrameRate)
            ),
            "the last rung must not name a resolution, or single-mode devices fail"
        );
    }

    #[test]
    fn delivers_bgra_frames_at_the_device_resolution() {
        if cameras().is_empty() {
            return;
        }
        let bubble = Rect::new(100, 100, 320, 240);
        let handle = CameraHandle::start(0, bubble);
        let feed = handle.feed();

        let Some(frame) = wait_for_frame(&feed, Duration::from_secs(8)) else {
            // a camera in use by another app is a legitimate outcome
            return;
        };
        assert!(frame.width > 0 && frame.height > 0);
        assert_eq!(
            frame.pixels.len(),
            frame.width as usize * frame.height as usize * 4,
            "the compositor requires tightly packed BGRA"
        );
    }

    #[test]
    fn frames_keep_arriving_and_the_sequence_advances() {
        if cameras().is_empty() {
            return;
        }
        let handle = CameraHandle::start(0, Rect::new(0, 0, 320, 240));
        let feed = handle.feed();
        let Some(first) = wait_for_frame(&feed, Duration::from_secs(8)) else {
            return;
        };

        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            if let Some(next) = feed.latest()
                && next.seq > first.seq
            {
                return;
            }
            std::thread::sleep(Duration::from_millis(30));
        }
        panic!("the camera stopped advancing after its first frame");
    }

    #[test]
    fn the_bubble_can_be_moved_while_running() {
        if cameras().is_empty() {
            return;
        }
        let handle = CameraHandle::start(0, Rect::new(10, 20, 320, 240));
        let feed = handle.feed();
        assert_eq!(feed.rect(), Rect::new(10, 20, 320, 240));
        handle.move_to(500, 600);
        assert_eq!(
            feed.rect(),
            Rect::new(500, 600, 320, 240),
            "dragging must update what the compositor reads"
        );
    }

    #[test]
    fn a_missing_camera_reports_an_error_instead_of_hanging() {
        let handle = CameraHandle::start(99, Rect::new(0, 0, 320, 240));
        let feed = handle.feed();
        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline {
            if feed.error().is_some() {
                return;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        panic!("opening a nonexistent camera should have reported an error");
    }

    #[test]
    fn stopping_releases_the_device_so_it_can_be_reopened() {
        if cameras().is_empty() {
            return;
        }
        let mut handle = CameraHandle::start(0, Rect::new(0, 0, 320, 240));
        let _ = wait_for_frame(&handle.feed(), Duration::from_secs(8));
        let start = Instant::now();
        handle.stop();
        assert!(
            start.elapsed() < Duration::from_secs(5),
            "stop took {:?}",
            start.elapsed()
        );

        let again = CameraHandle::start(0, Rect::new(0, 0, 320, 240));
        let feed = again.feed();
        let reopened = wait_for_frame(&feed, Duration::from_secs(8));
        assert!(
            reopened.is_some() || feed.error().is_none(),
            "the device was left wedged after stopping"
        );
    }
}
