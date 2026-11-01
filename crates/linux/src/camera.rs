//! Webcam capture through GStreamer's camera sources (Video4Linux, or
//! PipeWire's camera portal where the desktop provides it).
//!
//! Frames are pulled on a dedicated thread into a [`CameraFeed`], which the
//! recorder and the bubble read.

use std::sync::Arc;
use std::thread::JoinHandle;

use gstreamer as gst;
use gstreamer::prelude::*;
use gstreamer_app as gst_app;
pub use screenilux_core::camera::CameraFeed;
use screenilux_core::geometry::Rect;

use crate::gst as g;

/// Device names in the order they are indexed.
pub fn cameras() -> Vec<String> {
    crate::audio::devices("Video/Source")
        .iter()
        .map(|d| d.display_name().to_string())
        .collect()
}

/// Handle that keeps the capture thread alive.
pub struct CameraHandle {
    feed: Arc<CameraFeed>,
    thread: Option<JoinHandle<()>>,
}

impl CameraHandle {
    /// Open camera `index` and start pulling frames. Failures surface
    /// through the feed's error rather than here, so a missing camera never
    /// blocks starting a recording.
    pub fn start(index: u32, bubble: Rect) -> Self {
        let feed = Arc::new(CameraFeed::new(bubble));
        let worker = Arc::clone(&feed);
        let thread = std::thread::Builder::new()
            .name("screenilux-camera".into())
            .spawn(move || {
                if let Err(e) = pump(&worker, index) {
                    tracing::warn!(error = %e, "camera stopped");
                    worker.fail(e);
                }
            })
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

fn pump(feed: &CameraFeed, index: u32) -> Result<(), String> {
    let device = crate::audio::devices("Video/Source")
        .into_iter()
        .nth(index as usize)
        .ok_or("no camera was found")?;
    let source = device
        .create_element(None)
        .map_err(|e| format!("could not open the camera: {e}"))?;
    let pipeline = gst::Pipeline::new();
    let convert = gst::ElementFactory::make("videoconvert")
        .build()
        .map_err(|e| e.to_string())?;
    // Small on purpose: the bubble is a few hundred pixels wide.
    let scale = gst::ElementFactory::make("videoscale")
        .build()
        .map_err(|e| e.to_string())?;
    let sink = gst_app::AppSink::builder()
        .caps(
            &gst::Caps::builder("video/x-raw")
                .field("format", "BGRA")
                .field("width", gst::IntRange::new(1, 1280))
                .build(),
        )
        .max_buffers(1)
        .drop(true)
        .sync(false)
        .build();
    pipeline
        .add_many([&source, &convert, &scale, sink.upcast_ref()])
        .map_err(|e| e.to_string())?;
    gst::Element::link_many([&source, &convert, &scale, sink.upcast_ref()])
        .map_err(|e| format!("the camera's pictures could not be read: {e}"))?;

    let result = (|| {
        g::set_state(&pipeline, gst::State::Playing)?;
        while !feed.stopping() {
            if let Some(e) = g::error_so_far(&pipeline) {
                return Err(e);
            }
            let Some(sample) = sink.try_pull_sample(gst::ClockTime::from_mseconds(100)) else {
                continue;
            };
            let Some(s) = sample.caps().and_then(|c| c.structure(0)) else {
                continue;
            };
            let (Ok(w), Ok(h)) = (s.get::<i32>("width"), s.get::<i32>("height")) else {
                continue;
            };
            let Some(map) = sample.buffer().and_then(|b| b.map_readable().ok()) else {
                continue;
            };
            let (w, h) = (w as usize, h as usize);
            let row = w * 4;
            let stride = map.len() / h.max(1);
            // Rows can be padded; the compositor wants them tightly packed.
            let pixels = if stride == row {
                map.as_slice()[..row * h].to_vec()
            } else {
                map.as_slice()
                    .chunks(stride)
                    .take(h)
                    .flat_map(|r| &r[..row])
                    .copied()
                    .collect()
            };
            feed.publish(pixels, w as u32, h as u32);
        }
        Ok(())
    })();
    let _ = pipeline.set_state(gst::State::Null);
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use screenilux_core::ports::CameraSource;
    use std::time::{Duration, Instant};

    #[test]
    fn enumerating_cameras_is_safe() {
        for name in cameras() {
            assert!(!name.is_empty());
        }
    }

    #[test]
    fn a_missing_camera_reports_an_error_instead_of_hanging() {
        let handle = CameraHandle::start(99, Rect::new(0, 0, 240, 240));
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
}
