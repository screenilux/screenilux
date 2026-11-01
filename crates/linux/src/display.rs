//! The screens, from the X server's RandR extension.

use screenilux_core::geometry::{Monitor, Point, Rect};
use screenilux_core::monitor_at;
use screenilux_core::ports::CaptureError;
use x11rb::connection::Connection;
use x11rb::protocol::randr::ConnectionExt as _;

/// Nothing to do: the X server already reports every screen in physical
/// pixels.
pub fn set_dpi_awareness() {}

/// The screens, in the order the X server lists them.
///
/// Labels are positional (`Screen 1`, `Screen 2`), matching what the UI shows
/// and what the session manifest records, as on every system.
pub fn monitors() -> Result<Vec<Monitor>, CaptureError> {
    let guard = crate::x11::shared().ok_or(CaptureError::NoDisplays)?;
    let d = guard.as_ref().ok_or(CaptureError::NoDisplays)?;
    let backend = |e: &dyn std::fmt::Display| CaptureError::Backend(e.to_string());
    let reply = d
        .conn
        .randr_get_monitors(d.root, true)
        .map_err(|e| backend(&e))?
        .reply()
        .map_err(|e| backend(&e))?;
    let mut found: Vec<(Rect, bool)> = reply
        .monitors
        .iter()
        .filter(|m| m.width > 0 && m.height > 0)
        .map(|m| {
            (
                Rect::new(m.x as i32, m.y as i32, m.width as u32, m.height as u32),
                m.primary,
            )
        })
        .collect();
    // No RandR monitors (a bare X server): the whole root window is one.
    if found.is_empty() {
        let screen = &d.conn.setup().roots[d.screen];
        found.push((
            Rect::new(
                0,
                0,
                screen.width_in_pixels as u32,
                screen.height_in_pixels as u32,
            ),
            true,
        ));
    }
    if !found.iter().any(|(_, p)| *p) {
        found[0].1 = true;
    }
    Ok(found
        .into_iter()
        .enumerate()
        .map(|(i, (bounds, primary))| Monitor::new(format!("Screen {}", i + 1), bounds, primary))
        .collect())
}

/// The screen at or nearest `(x, y)`. X11 has no standard work area per
/// screen, so this is the whole screen.
pub fn work_area_at(x: i32, y: i32) -> Option<Rect> {
    let list = monitors().ok()?;
    let i = monitor_at(&list, Point::new(x, y)).unwrap_or(0);
    list.get(i).map(|m| m.bounds)
}
