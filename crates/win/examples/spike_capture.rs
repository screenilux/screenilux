//! Validates `windows-capture` against the real display layout before the
//! rest of the crate is built on it.
//!
//! Checks the things the port actually depends on: that monitors enumerate
//! with usable positions, that frames arrive at the expected size in BGRA,
//! that the buffer's row padding is handled, and that dirty regions report
//! something sensible so an idle screen can be skipped.
//!
//! Run with: cargo run -p screenilux-win --example spike_capture

use std::sync::mpsc;
use std::time::{Duration, Instant};

use windows::Win32::Foundation::RECT;
use windows::Win32::Graphics::Gdi::HMONITOR;
use windows::Win32::Graphics::Gdi::{GetMonitorInfoW, MONITORINFO};
use windows::Win32::UI::HiDpi::{
    DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetProcessDpiAwarenessContext,
};
use windows_capture::capture::{Context, GraphicsCaptureApiHandler};
use windows_capture::frame::Frame;
use windows_capture::graphics_capture_api::InternalCaptureControl;
use windows_capture::monitor::Monitor;
use windows_capture::settings::{
    ColorFormat, CursorCaptureSettings, DirtyRegionSettings, DrawBorderSettings,
    MinimumUpdateIntervalSettings, SecondaryWindowSettings, Settings,
};

struct Report {
    width: u32,
    height: u32,
    bytes: usize,
    row_pitch: u32,
    has_padding: bool,
    dirty: usize,
}

struct Probe {
    tx: mpsc::Sender<Report>,
    left: u32,
}

impl GraphicsCaptureApiHandler for Probe {
    type Flags = (mpsc::Sender<Report>, u32);
    type Error = Box<dyn std::error::Error + Send + Sync>;

    fn new(ctx: Context<Self::Flags>) -> Result<Self, Self::Error> {
        let (tx, left) = ctx.flags;
        Ok(Self { tx, left })
    }

    fn on_frame_arrived(
        &mut self,
        frame: &mut Frame,
        control: InternalCaptureControl,
    ) -> Result<(), Self::Error> {
        let dirty = frame.dirty_regions().map(|r| r.len()).unwrap_or(usize::MAX);
        let mut buf = frame.buffer()?;
        let report = Report {
            width: buf.width(),
            height: buf.height(),
            bytes: buf.as_raw_buffer().len(),
            row_pitch: buf.row_pitch(),
            has_padding: buf.has_padding(),
            dirty,
        };
        let _ = self.tx.send(report);
        self.left -= 1;
        if self.left == 0 {
            control.stop();
        }
        Ok(())
    }
}

/// `windows-capture`'s Monitor exposes size but not position, and the whole
/// coordinate model depends on position, so fetch it from Win32 directly.
fn monitor_origin(m: &Monitor) -> Option<(i32, i32, i32, i32)> {
    let mut info = MONITORINFO {
        cbSize: size_of::<MONITORINFO>() as u32,
        ..Default::default()
    };
    let ok = unsafe { GetMonitorInfoW(HMONITOR(m.as_raw_hmonitor()), &mut info) };
    if !ok.as_bool() {
        return None;
    }
    let RECT {
        left,
        top,
        right,
        bottom,
    } = info.rcMonitor;
    Some((left, top, right, bottom))
}

const MONITORINFOF_PRIMARY: u32 = 1;

fn is_primary(m: &Monitor) -> bool {
    let mut info = MONITORINFO {
        cbSize: size_of::<MONITORINFO>() as u32,
        ..Default::default()
    };
    let ok = unsafe { GetMonitorInfoW(HMONITOR(m.as_raw_hmonitor()), &mut info) };
    ok.as_bool() && info.dwFlags & MONITORINFOF_PRIMARY != 0
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    unsafe {
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
    }

    let monitors = Monitor::enumerate()?;
    println!("enumerated {} monitor(s)\n", monitors.len());

    for (i, m) in monitors.iter().enumerate() {
        let name = m.name().unwrap_or_else(|_| "?".into());
        let device = m.device_name().unwrap_or_else(|_| "?".into());
        let (w, h) = (m.width()?, m.height()?);
        let origin = monitor_origin(m);
        println!(
            "[{i}] {name} ({device})  {w}x{h}  origin={:?}  primary={}  refresh={}Hz",
            origin.map(|(l, t, _, _)| (l, t)),
            is_primary(m),
            m.refresh_rate().unwrap_or(0),
        );

        let (tx, rx) = mpsc::channel();
        let settings = Settings::new(
            *m,
            CursorCaptureSettings::WithoutCursor,
            DrawBorderSettings::WithoutBorder,
            SecondaryWindowSettings::Default,
            MinimumUpdateIntervalSettings::Default,
            DirtyRegionSettings::ReportOnly,
            ColorFormat::Bgra8,
            (tx, 5u32),
        );

        let started = Instant::now();
        let control = Probe::start_free_threaded(settings)?;

        let mut got = 0usize;
        let deadline = Instant::now() + Duration::from_secs(5);
        while got < 5 && Instant::now() < deadline {
            match rx.recv_timeout(Duration::from_millis(500)) {
                Ok(r) => {
                    if got == 0 {
                        println!(
                            "     frame: {}x{}  {} bytes  row_pitch={}  padding={}  (first in {:?})",
                            r.width,
                            r.height,
                            r.bytes,
                            r.row_pitch,
                            r.has_padding,
                            started.elapsed()
                        );
                        let expected = r.width as usize * r.height as usize * 4;
                        println!(
                            "     tight BGRA would be {expected} bytes -> {}",
                            if r.bytes == expected {
                                "no padding to strip"
                            } else {
                                "PADDED, must strip rows"
                            }
                        );
                    }
                    println!("     dirty regions: {}", r.dirty);
                    got += 1;
                }
                Err(_) => break,
            }
        }
        let _ = control.stop();
        if got == 0 {
            println!("     !! no frames arrived within 5s");
        }
        println!();
    }

    println!("idle-screen behaviour: watching the primary for 3s without touching it");
    let m = Monitor::primary()?;
    let (tx, rx) = mpsc::channel();
    let settings = Settings::new(
        m,
        CursorCaptureSettings::WithoutCursor,
        DrawBorderSettings::WithoutBorder,
        SecondaryWindowSettings::Default,
        MinimumUpdateIntervalSettings::Default,
        DirtyRegionSettings::ReportOnly,
        ColorFormat::Bgra8,
        (tx, 100_000u32),
    );
    let control = Probe::start_free_threaded(settings)?;
    let start = Instant::now();
    let mut frames = 0usize;
    let mut dirty_total = 0usize;
    while start.elapsed() < Duration::from_secs(3) {
        if let Ok(r) = rx.recv_timeout(Duration::from_millis(200)) {
            frames += 1;
            if r.dirty != usize::MAX {
                dirty_total += r.dirty;
            }
        }
    }
    let _ = control.stop();
    println!(
        "  {frames} frames in 3s ({:.1}/s), {dirty_total} dirty regions total",
        frames as f64 / 3.0
    );
    println!("  -> a quiet screen delivering few frames is what makes idle capture cheap");

    Ok(())
}
