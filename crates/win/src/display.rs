//! Monitor enumeration and process DPI awareness.

use screenilux_core::geometry::{Monitor, Rect};
use screenilux_core::ports::{CaptureError, DisplayEnumerator};
use windows::Win32::Foundation::RECT;
use windows::Win32::Graphics::Gdi::{GetMonitorInfoW, HMONITOR, MONITORINFO};
use windows::Win32::UI::HiDpi::{
    DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetProcessDpiAwarenessContext,
};
use windows_capture::monitor::Monitor as WcMonitor;

const MONITORINFOF_PRIMARY: u32 = 1;

/// Declare per-monitor DPI awareness so captures are in physical pixels.
///
/// Without this a scaled display reports virtualised coordinates and every
/// capture comes out the wrong size. Must run before any window or capture
/// exists, so call it first thing in `main`.
///
/// Idempotent in effect: Windows rejects a second call, which is harmless.
pub fn set_dpi_awareness() {
    // SAFETY: a plain process-wide setting with no pointer arguments.
    let ok = unsafe { SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) };
    if ok.is_err() {
        tracing::debug!("per-monitor DPI awareness was already set, or is unavailable");
    }
}

/// Monitor bounds and primary flag, in virtual-desktop coordinates.
///
/// `windows-capture` reports size but not position, and position is the whole
/// coordinate model here, so this goes to Win32 directly.
fn monitor_info(m: &WcMonitor) -> Option<(Rect, bool)> {
    let mut info = MONITORINFO {
        cbSize: size_of::<MONITORINFO>() as u32,
        ..Default::default()
    };
    // SAFETY: `info.cbSize` is set as the API requires, and the HMONITOR comes
    // straight from an enumeration that is still alive.
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
    let bounds = Rect::new(left, top, (right - left) as u32, (bottom - top) as u32);
    Some((bounds, info.dwFlags & MONITORINFOF_PRIMARY != 0))
}

/// Enumerate the displays, in the order Windows reports them.
///
/// Labels are positional (`Screen 1`, `Screen 2`), matching what the UI shows
/// and what the session manifest records.
pub fn monitors() -> Result<Vec<Monitor>, CaptureError> {
    let raw = WcMonitor::enumerate().map_err(|e| CaptureError::Backend(e.to_string()))?;
    let mut out = Vec::with_capacity(raw.len());
    for (i, m) in raw.iter().enumerate() {
        let Some((bounds, primary)) = monitor_info(m) else {
            tracing::warn!(index = i, "skipping a monitor with no readable bounds");
            continue;
        };
        out.push(Monitor::new(format!("Screen {}", i + 1), bounds, primary));
    }
    if out.is_empty() {
        return Err(CaptureError::NoDisplays);
    }
    Ok(out)
}

/// The handle `windows-capture` needs for a given monitor, matched by bounds.
///
/// Re-enumerating and matching on geometry rather than caching a handle keeps
/// this correct across a display change between listing and recording.
pub(crate) fn find_raw(bounds: Rect) -> Result<WcMonitor, CaptureError> {
    let raw = WcMonitor::enumerate().map_err(|e| CaptureError::Backend(e.to_string()))?;
    raw.into_iter()
        .find(|m| monitor_info(m).map(|(b, _)| b == bounds).unwrap_or(false))
        .ok_or(CaptureError::DisplayLost)
}

/// Enumerator handle for the [`DisplayEnumerator`] port.
#[derive(Debug, Clone, Copy, Default)]
pub struct Displays;

impl DisplayEnumerator for Displays {
    fn monitors(&self) -> Result<Vec<Monitor>, CaptureError> {
        monitors()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use screenilux_core::geometry::virtual_bounds;

    #[test]
    fn enumerates_the_attached_displays() {
        set_dpi_awareness();
        let found = match monitors() {
            Ok(m) => m,
            Err(CaptureError::NoDisplays) => return, // headless CI
            Err(e) => panic!("enumeration failed: {e}"),
        };
        assert!(!found.is_empty());

        for m in &found {
            assert!(m.bounds.width > 0 && m.bounds.height > 0);
            assert!(m.label.starts_with("Screen "));
        }
        assert_eq!(
            found.iter().filter(|m| m.is_primary).count(),
            1,
            "Windows always designates exactly one primary"
        );
    }

    #[test]
    fn the_primary_sits_at_the_origin() {
        set_dpi_awareness();
        let Ok(found) = monitors() else { return };
        let primary = found.iter().find(|m| m.is_primary).unwrap();
        assert_eq!(
            (primary.bounds.left, primary.bounds.top),
            (0, 0),
            "Windows anchors the virtual desktop at the primary"
        );
    }

    #[test]
    fn displays_do_not_overlap() {
        set_dpi_awareness();
        let Ok(found) = monitors() else { return };
        for (i, a) in found.iter().enumerate() {
            for b in found.iter().skip(i + 1) {
                assert!(
                    !a.bounds.intersects(&b.bounds),
                    "{} and {} overlap",
                    a.label,
                    b.label
                );
            }
        }
    }

    #[test]
    fn virtual_bounds_covers_every_display() {
        set_dpi_awareness();
        let Ok(found) = monitors() else { return };
        let all = virtual_bounds(&found).unwrap();
        for m in &found {
            assert!(all.intersects(&m.bounds));
            assert!(m.bounds.left >= all.left && m.bounds.top >= all.top);
        }
    }

    #[test]
    fn each_enumerated_monitor_can_be_found_again_by_bounds() {
        set_dpi_awareness();
        let Ok(found) = monitors() else { return };
        for m in &found {
            assert!(
                find_raw(m.bounds).is_ok(),
                "{} could not be matched back",
                m.label
            );
        }
    }
}
