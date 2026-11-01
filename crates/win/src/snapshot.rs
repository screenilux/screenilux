//! Small pictures of what is on a screen, for previews.
//!
//! GDI scales the desktop down in one call, so a thumbnail costs a few
//! milliseconds and never touches a full-size frame. It misses what only
//! Windows Graphics Capture sees, such as some hardware-overlay video, which
//! is fine for a preview and is why recording does not use it.

use screenilux_core::geometry::Rect;
use windows::Win32::Graphics::Gdi::{
    BI_RGB, BITMAPINFO, BITMAPINFOHEADER, CreateCompatibleDC, CreateDIBSection, DIB_RGB_COLORS,
    DeleteDC, DeleteObject, GetDC, HALFTONE, ReleaseDC, SRCCOPY, SelectObject, SetBrushOrgEx,
    SetStretchBltMode, StretchBlt,
};

/// An RGBA picture.
pub struct Thumbnail {
    pub width: u32,
    pub height: u32,
    /// Tightly packed RGBA8, opaque.
    pub rgba: Vec<u8>,
}

/// The part of the desktop at `area`, scaled to fit within `max_width` by
/// `max_height` with its aspect kept. `None` if Windows refuses, which it can
/// briefly while displays change.
pub fn thumbnail(area: Rect, max_width: u32, max_height: u32) -> Option<Thumbnail> {
    if area.width == 0 || area.height == 0 || max_width == 0 || max_height == 0 {
        return None;
    }
    let scale = (max_width as f64 / area.width as f64).min(max_height as f64 / area.height as f64);
    let width = ((area.width as f64 * scale).round() as u32).max(1);
    let height = ((area.height as f64 * scale).round() as u32).max(1);

    let info = BITMAPINFO {
        bmiHeader: BITMAPINFOHEADER {
            biSize: size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: width as i32,
            // Negative: rows top to bottom, as images are everywhere else.
            biHeight: -(height as i32),
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB.0,
            ..Default::default()
        },
        ..Default::default()
    };

    // SAFETY: every handle made here is released before returning, and the
    // DIB's bits are only read while the bitmap is alive and sized as
    // declared above.
    unsafe {
        let screen = GetDC(None);
        if screen.is_invalid() {
            return None;
        }
        let memory = CreateCompatibleDC(Some(screen));
        let mut bits = std::ptr::null_mut();
        let bitmap = CreateDIBSection(Some(memory), &info, DIB_RGB_COLORS, &mut bits, None, 0);
        let result = match bitmap {
            Ok(bitmap) if !bits.is_null() => {
                let old = SelectObject(memory, bitmap.into());
                // Halftone averages the pixels a source block covers, so
                // small text shrinks to grey rather than to noise.
                SetStretchBltMode(memory, HALFTONE);
                let _ = SetBrushOrgEx(memory, 0, 0, None);
                let ok = StretchBlt(
                    memory,
                    0,
                    0,
                    width as i32,
                    height as i32,
                    Some(screen),
                    area.left,
                    area.top,
                    area.width as i32,
                    area.height as i32,
                    SRCCOPY,
                )
                .as_bool();
                let picture = ok.then(|| {
                    let bgra = std::slice::from_raw_parts(
                        bits as *const u8,
                        (width * height * 4) as usize,
                    );
                    let mut rgba = Vec::with_capacity(bgra.len());
                    for px in bgra.as_chunks::<4>().0 {
                        rgba.extend_from_slice(&[px[2], px[1], px[0], 255]);
                    }
                    Thumbnail {
                        width,
                        height,
                        rgba,
                    }
                });
                SelectObject(memory, old);
                let _ = DeleteObject(bitmap.into());
                picture
            }
            _ => None,
        };
        let _ = DeleteDC(memory);
        ReleaseDC(None, screen);
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_thumbnail_keeps_the_screens_shape() {
        let Ok(monitors) = crate::monitors() else {
            return; // headless
        };
        let Some(m) = monitors.first() else { return };
        let t = thumbnail(m.bounds, 320, 320).expect("a thumbnail of a real screen");
        assert!(t.width <= 320 && t.height <= 320);
        assert!(t.width == 320 || t.height == 320, "it fills one side");
        let aspect = m.bounds.width as f64 / m.bounds.height as f64;
        let got = t.width as f64 / t.height as f64;
        assert!((aspect - got).abs() < 0.02, "{aspect} vs {got}");
        assert_eq!(t.rgba.len(), (t.width * t.height * 4) as usize);
        assert!(t.rgba.as_chunks::<4>().0.iter().all(|p| p[3] == 255));
    }

    #[test]
    fn an_empty_area_has_no_thumbnail() {
        assert!(thumbnail(Rect::new(0, 0, 0, 10), 100, 100).is_none());
        assert!(thumbnail(Rect::new(0, 0, 10, 10), 0, 100).is_none());
    }
}
