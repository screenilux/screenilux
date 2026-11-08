//! Screen capture via Windows Graphics Capture.
//!
//! `windows-capture` is callback-driven: it owns a thread, runs a WinRT
//! dispatcher on it and hands frames to a trait implementation. The recorder
//! wants the opposite shape — a pull with a deadline — so this bridges the
//! two through a single-slot mailbox.
//!
//! The mailbox deliberately keeps only the newest frame. A queue would build
//! latency whenever the encoder fell behind, and a recording wants the most
//! recent pixels, not every pixel.

use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use screenilux_core::geometry::Rect;
use screenilux_core::ports::{CaptureError, Frame, ScreenSource};
use windows::Win32::Graphics::Direct3D11::{
    D3D11_CPU_ACCESS_READ, D3D11_MAP_READ, D3D11_MAPPED_SUBRESOURCE, D3D11_TEXTURE2D_DESC,
    D3D11_USAGE_STAGING, ID3D11Texture2D,
};
use windows::Win32::Graphics::Dxgi::Common::DXGI_SAMPLE_DESC;
use windows_capture::capture::{Context, GraphicsCaptureApiHandler};
use windows_capture::frame::Frame as WcFrame;
use windows_capture::graphics_capture_api::{GraphicsCaptureApi, InternalCaptureControl};
use windows_capture::settings::{
    ColorFormat, CursorCaptureSettings, DirtyRegionSettings, DrawBorderSettings,
    MinimumUpdateIntervalSettings, SecondaryWindowSettings, Settings,
};

use crate::display;

/// Hand-off point between the capture thread and the recorder.
#[derive(Default)]
struct MailboxState {
    latest: Option<Vec<u8>>,
    /// Buffers returned for reuse. Frames are megabytes each; reallocating one
    /// per frame would churn the allocator at 30-60Hz.
    spare: Vec<Vec<u8>>,
    closed: bool,
    error: Option<String>,
}

#[derive(Default)]
struct Mailbox {
    state: Mutex<MailboxState>,
    ready: Condvar,
}

impl Mailbox {
    /// A buffer of `len` bytes to copy the next frame into.
    ///
    /// Handed back as it is rather than zeroed: `on_frame_arrived` writes
    /// every byte of it, and clearing first costs a memset of the whole frame
    /// — a quarter of a gigabyte a second at 1080p30, ten times that at 4K60,
    /// on the one thread that must keep up with the display.
    fn take_spare(&self, len: usize) -> Vec<u8> {
        let mut st = self.state.lock().expect("mailbox poisoned");
        match st.spare.pop() {
            Some(b) if b.len() == len => b,
            _ => vec![0u8; len],
        }
    }

    fn deliver(&self, frame: Vec<u8>) {
        let mut st = self.state.lock().expect("mailbox poisoned");
        if let Some(stale) = st.latest.replace(frame) {
            st.spare.push(stale);
        }
        drop(st);
        self.ready.notify_one();
    }

    fn close(&self, error: Option<String>) {
        let mut st = self.state.lock().expect("mailbox poisoned");
        st.closed = true;
        if error.is_some() {
            st.error = error;
        }
        drop(st);
        self.ready.notify_all();
    }
}

/// The `windows-capture` side of the bridge.
struct Pump {
    mailbox: Arc<Mailbox>,
    /// The size frames are delivered at, which is the monitor's.
    width: usize,
    height: usize,
    /// Where frames are copied to be read on the CPU, made once and kept.
    ///
    /// `windows-capture`'s own `Frame::buffer` makes a new one for every
    /// frame: a monitor-sized texture created and released dozens of times a
    /// second, on the graphics device the desktop is composited on.
    staging: Option<(D3D11_TEXTURE2D_DESC, ID3D11Texture2D)>,
}

impl GraphicsCaptureApiHandler for Pump {
    type Flags = (Arc<Mailbox>, Rect);
    type Error = Box<dyn std::error::Error + Send + Sync>;

    fn new(ctx: Context<Self::Flags>) -> Result<Self, Self::Error> {
        let (mailbox, bounds) = ctx.flags;
        Ok(Self {
            mailbox,
            width: bounds.width as usize,
            height: bounds.height as usize,
            staging: None,
        })
    }

    fn on_frame_arrived(
        &mut self,
        frame: &mut WcFrame,
        _control: InternalCaptureControl,
    ) -> Result<(), Self::Error> {
        let mut out = self.mailbox.take_spare(self.width * self.height * 4);
        self.read_back(frame, &mut out)?;
        self.mailbox.deliver(out);
        Ok(())
    }

    fn on_closed(&mut self) -> Result<(), Self::Error> {
        self.mailbox.close(None);
        Ok(())
    }
}

impl Pump {
    /// Copy `frame` into `out` as tightly packed BGRA, through the kept
    /// staging texture.
    fn read_back(&mut self, frame: &WcFrame, out: &mut [u8]) -> windows::core::Result<()> {
        let source = frame.as_raw_texture();
        let context = frame.device_context();
        let mut desc = D3D11_TEXTURE2D_DESC::default();
        // SAFETY: a live texture describing itself into a local.
        unsafe { source.GetDesc(&mut desc) };

        let fits = |kept: &D3D11_TEXTURE2D_DESC| {
            (kept.Width, kept.Height, kept.Format) == (desc.Width, desc.Height, desc.Format)
        };
        let staging = match &self.staging {
            Some((kept, texture)) if fits(kept) => texture.clone(),
            _ => {
                let want = D3D11_TEXTURE2D_DESC {
                    MipLevels: 1,
                    ArraySize: 1,
                    SampleDesc: DXGI_SAMPLE_DESC {
                        Count: 1,
                        Quality: 0,
                    },
                    Usage: D3D11_USAGE_STAGING,
                    BindFlags: 0,
                    CPUAccessFlags: D3D11_CPU_ACCESS_READ.0 as u32,
                    MiscFlags: 0,
                    ..desc
                };
                let mut made = None;
                // SAFETY: a staging description with no initial data, on the
                // device the frame came from.
                unsafe {
                    frame
                        .device()
                        .CreateTexture2D(&want, None, Some(&mut made))?
                };
                let made = made.ok_or_else(|| {
                    windows::core::Error::from(windows::Win32::Foundation::E_POINTER)
                })?;
                self.staging = Some((want, made.clone()));
                made
            }
        };

        let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
        // SAFETY: both textures are on this frame's device and the same size
        // and format; the mapping is released below before anything else
        // uses the texture.
        unsafe {
            context.CopyResource(&staging, source);
            context.Map(&staging, 0, D3D11_MAP_READ, 0, Some(&mut mapped))?;
        }

        // Rows can be padded past width * 4, and the frame can differ from
        // the monitor's size for a moment while it changes mode: copy what
        // overlaps, and black for the rest rather than whatever the buffer
        // held last time round.
        let row = self.width * 4;
        let copy = (desc.Width as usize).min(self.width) * 4;
        let rows = (desc.Height as usize).min(self.height);
        let pitch = mapped.RowPitch as usize;
        // SAFETY: the mapping holds `pitch` bytes for each of the texture's
        // rows, `rows` is no more than those, and `copy` is at most a row.
        let src = unsafe { std::slice::from_raw_parts(mapped.pData as *const u8, pitch * rows) };
        for (y, dst) in out.chunks_exact_mut(row).enumerate() {
            if y < rows {
                dst[..copy].copy_from_slice(&src[y * pitch..][..copy]);
                dst[copy..].fill(0);
            } else {
                dst.fill(0);
            }
        }
        // SAFETY: mapped above, and nothing borrows `src` past here.
        unsafe { context.Unmap(&staging, 0) };
        Ok(())
    }
}

/// A live capture of one monitor.
pub struct ScreenCapture {
    mailbox: Arc<Mailbox>,
    control: Option<
        windows_capture::capture::CaptureControl<Pump, <Pump as GraphicsCaptureApiHandler>::Error>,
    >,
    bounds: Rect,
    /// The most recent capture, kept pristine so it can be handed out again
    /// when nothing has changed.
    last: Vec<u8>,
    have_frame: bool,
}

impl ScreenCapture {
    /// Start capturing the monitor occupying `bounds`, for a recording at
    /// `fps`.
    ///
    /// Windows hands over a frame whenever the screen changes, which on a
    /// high refresh display is up to 144 or 240 times a second, and each one
    /// is copied off the graphics device. The recorder takes `fps` of them
    /// and drops the rest, so asking for more is load on the very device the
    /// desktop and whatever is being recorded draw with, for nothing. Where
    /// Windows can be asked for no more than `fps`, it is.
    ///
    /// The cursor is excluded: Windows would composite its own pointer into
    /// the frame, and the recorder draws its own so the marker stays a
    /// constant size under zoom. The capture border is disabled too — it is a
    /// yellow rectangle around the recorded screen, which belongs in a
    /// screen-share, not a recording.
    pub fn start(bounds: Rect, fps: u32) -> Result<Self, CaptureError> {
        let monitor = display::find_raw(bounds)?;
        let bytes = bounds.width as usize * bounds.height as usize * 4;
        let mailbox = Arc::new(Mailbox::default());

        let capped =
            fps > 0 && GraphicsCaptureApi::is_minimum_update_interval_supported().unwrap_or(false);
        let interval = if capped {
            MinimumUpdateIntervalSettings::Custom(Duration::from_secs(1) / fps)
        } else {
            MinimumUpdateIntervalSettings::Default
        };
        let settings = Settings::new(
            monitor,
            CursorCaptureSettings::WithoutCursor,
            DrawBorderSettings::WithoutBorder,
            SecondaryWindowSettings::Default,
            interval,
            DirtyRegionSettings::Default,
            ColorFormat::Bgra8,
            (Arc::clone(&mailbox), bounds),
        );

        let control = Pump::start_free_threaded(settings)
            .map_err(|e| CaptureError::Backend(e.to_string()))?;

        Ok(Self {
            mailbox,
            control: Some(control),
            bounds,
            last: vec![0u8; bytes],
            have_frame: false,
        })
    }

    /// Block until the first frame, so a recording never opens on black.
    pub fn wait_for_first_frame(&mut self, timeout: Duration) -> Result<(), CaptureError> {
        let deadline = Instant::now() + timeout;
        while !self.have_frame {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Err(CaptureError::Timeout);
            }
            self.pull(left)?;
        }
        Ok(())
    }

    /// Move the newest frame into `last`, if one arrived before the deadline.
    fn pull(&mut self, timeout: Duration) -> Result<bool, CaptureError> {
        let mut st = self.mailbox.state.lock().expect("mailbox poisoned");
        loop {
            if let Some(fresh) = st.latest.take() {
                let stale = std::mem::replace(&mut self.last, fresh);
                st.spare.push(stale);
                self.have_frame = true;
                return Ok(true);
            }
            if let Some(err) = st.error.clone() {
                return Err(CaptureError::Backend(err));
            }
            if st.closed {
                return Err(CaptureError::DisplayLost);
            }
            let (next, timed_out) = self
                .mailbox
                .ready
                .wait_timeout(st, timeout)
                .expect("mailbox poisoned");
            st = next;
            if timed_out.timed_out() {
                return Ok(false);
            }
        }
    }
}

impl ScreenSource for ScreenCapture {
    fn next_frame(&mut self, timeout: Duration) -> Result<Frame<'_>, CaptureError> {
        let fresh = self.pull(timeout)?;
        if !fresh && !self.have_frame {
            // nothing has ever arrived; black pixels would be a lie
            return Err(CaptureError::Timeout);
        }
        Ok(Frame {
            pixels: &self.last,
            width: self.bounds.width,
            height: self.bounds.height,
            changed: fresh,
        })
    }

    fn bounds(&self) -> Rect {
        self.bounds
    }
}

impl Drop for ScreenCapture {
    fn drop(&mut self) {
        self.mailbox.close(None);
        if let Some(control) = self.control.take() {
            let _ = control.stop();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::display;

    fn primary() -> Option<Rect> {
        display::set_dpi_awareness();
        display::monitors()
            .ok()?
            .into_iter()
            .find(|m| m.is_primary)
            .map(|m| m.bounds)
    }

    #[test]
    fn captures_frames_at_the_monitor_size() {
        let Some(bounds) = primary() else { return };
        let mut cap = ScreenCapture::start(bounds, 30).expect("capture should start");
        cap.wait_for_first_frame(Duration::from_secs(5))
            .expect("a frame should arrive");

        let frame = cap.next_frame(Duration::from_secs(2)).unwrap();
        assert_eq!(frame.width, bounds.width);
        assert_eq!(frame.height, bounds.height);
        assert_eq!(
            frame.pixels.len(),
            bounds.width as usize * bounds.height as usize * 4,
            "the recorder requires tightly packed BGRA"
        );
    }

    #[test]
    fn a_captured_desktop_is_not_uniformly_blank() {
        let Some(bounds) = primary() else { return };
        let mut cap = ScreenCapture::start(bounds, 30).unwrap();
        if cap.wait_for_first_frame(Duration::from_secs(5)).is_err() {
            return;
        }
        let frame = cap.next_frame(Duration::from_secs(2)).unwrap();
        let first = frame.pixels[0];
        assert!(
            frame.pixels.iter().step_by(4096).any(|&b| b != first),
            "a real desktop should not be one flat colour"
        );
    }

    /// The saving that justifies this backend: a quiet screen stops delivering
    /// frames, and we report that rather than re-capturing.
    #[test]
    fn an_unchanged_screen_reports_not_changed_rather_than_blocking() {
        let Some(bounds) = primary() else { return };
        let mut cap = ScreenCapture::start(bounds, 30).unwrap();
        if cap.wait_for_first_frame(Duration::from_secs(5)).is_err() {
            return;
        }

        // drain whatever is queued, then ask again with a short deadline
        for _ in 0..3 {
            let _ = cap.next_frame(Duration::from_millis(50));
        }
        let start = Instant::now();
        let frame = cap.next_frame(Duration::from_millis(80)).unwrap();
        assert!(
            start.elapsed() < Duration::from_millis(500),
            "a deadline must be honoured, not ignored"
        );
        // `changed` may be either here - the desktop is not guaranteed still -
        // but the pixels must always be valid to composite from
        assert_eq!(
            frame.pixels.len(),
            bounds.width as usize * bounds.height as usize * 4
        );
    }

    #[test]
    fn the_cached_frame_survives_between_calls() {
        let Some(bounds) = primary() else { return };
        let mut cap = ScreenCapture::start(bounds, 30).unwrap();
        if cap.wait_for_first_frame(Duration::from_secs(5)).is_err() {
            return;
        }
        let len = cap
            .next_frame(Duration::from_millis(200))
            .unwrap()
            .pixels
            .len();
        for _ in 0..5 {
            let f = cap.next_frame(Duration::from_millis(80)).unwrap();
            assert_eq!(f.pixels.len(), len);
        }
    }

    #[test]
    fn capturing_a_display_that_is_not_there_fails_cleanly() {
        display::set_dpi_awareness();
        let nowhere = Rect::new(-99_000, -99_000, 640, 480);
        assert!(matches!(
            ScreenCapture::start(nowhere, 30),
            Err(CaptureError::DisplayLost)
        ));
    }

    #[test]
    fn every_attached_display_can_be_captured() {
        display::set_dpi_awareness();
        let Ok(found) = display::monitors() else {
            return;
        };
        for m in found {
            let mut cap = ScreenCapture::start(m.bounds, 30)
                .unwrap_or_else(|e| panic!("{} failed to start: {e}", m.label));
            cap.wait_for_first_frame(Duration::from_secs(5))
                .unwrap_or_else(|e| panic!("{} produced no frame: {e}", m.label));
        }
    }
}
