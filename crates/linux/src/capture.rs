//! Capturing a screen from the X server.
//!
//! Each capture keeps a connection of its own, so several screens are read
//! in parallel. The X server draws no pointer into the picture, which suits
//! the recorder: it draws its own, so the marker keeps its size under zoom.

use std::time::{Duration, Instant};

use screenilux_core::geometry::Rect;
use screenilux_core::ports::{CaptureError, Frame, ScreenSource};

use crate::x11::{self, Display};

/// A live capture of one screen.
pub struct ScreenCapture {
    display: Display,
    bounds: Rect,
    pixels: Vec<u8>,
    have_frame: bool,
}

impl ScreenCapture {
    /// Start capturing the screen occupying `bounds`.
    ///
    /// `fps` is the Windows backend's: X11 is read only when the recorder
    /// asks for a frame, so it never captures more than the recording takes.
    pub fn start(bounds: Rect, _fps: u32) -> Result<Self, CaptureError> {
        let display = x11::connect().map_err(CaptureError::Backend)?;
        Ok(Self {
            display,
            bounds,
            pixels: Vec::new(),
            have_frame: false,
        })
    }

    /// Read the first frame, so a recording never opens on black.
    pub fn wait_for_first_frame(&mut self, timeout: Duration) -> Result<(), CaptureError> {
        let deadline = Instant::now() + timeout;
        loop {
            match self.grab() {
                Ok(()) => return Ok(()),
                Err(e) if Instant::now() >= deadline => return Err(e),
                Err(_) => std::thread::sleep(Duration::from_millis(50)),
            }
        }
    }

    fn grab(&mut self) -> Result<(), CaptureError> {
        let b = self.bounds;
        self.pixels = self
            .display
            .grab(b.left, b.top, b.width, b.height)
            .map_err(CaptureError::Backend)?;
        self.have_frame = true;
        Ok(())
    }
}

impl ScreenSource for ScreenCapture {
    /// The X server has no "nothing changed" signal, so every call reads the
    /// screen afresh. A failed read hands out the last picture again rather
    /// than ending the recording over one bad frame.
    fn next_frame(&mut self, _timeout: Duration) -> Result<Frame<'_>, CaptureError> {
        let fresh = match self.grab() {
            Ok(()) => true,
            Err(e) if !self.have_frame => return Err(e),
            Err(e) => {
                tracing::debug!(error = %e, "reusing the last frame");
                false
            }
        };
        Ok(Frame {
            pixels: &self.pixels,
            width: self.bounds.width,
            height: self.bounds.height,
            changed: fresh,
        })
    }

    fn bounds(&self) -> Rect {
        self.bounds
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn captures_the_first_screen_at_its_size() {
        let Ok(monitors) = crate::monitors() else {
            return; // no X display
        };
        let m = &monitors[0];
        let mut cap = ScreenCapture::start(m.bounds, 30).expect("a capture of a real screen");
        cap.wait_for_first_frame(Duration::from_secs(2)).unwrap();
        let frame = cap.next_frame(Duration::from_millis(100)).unwrap();
        assert_eq!(frame.width, m.bounds.width);
        assert_eq!(
            frame.pixels.len(),
            (m.bounds.width * m.bounds.height * 4) as usize,
            "tightly packed BGRA"
        );
        assert!(frame.pixels.as_chunks::<4>().0.iter().all(|p| p[3] == 255));
    }
}
