//! Small pictures of what is on a screen, for previews.

use screenilux_core::geometry::Rect;

/// An RGBA picture.
pub struct Thumbnail {
    pub width: u32,
    pub height: u32,
    /// Tightly packed RGBA8, opaque.
    pub rgba: Vec<u8>,
}

/// The part of the desktop at `area`, scaled to fit within `max_width` by
/// `max_height` with its aspect kept.
pub fn thumbnail(area: Rect, max_width: u32, max_height: u32) -> Option<Thumbnail> {
    if area.width == 0 || area.height == 0 || max_width == 0 || max_height == 0 {
        return None;
    }
    let bgra = {
        let guard = crate::x11::shared()?;
        let d = guard.as_ref()?;
        d.grab(area.left, area.top, area.width, area.height).ok()?
    };
    Some(shrink(
        &bgra,
        area.width,
        area.height,
        max_width,
        max_height,
    ))
}

/// Average each block of source pixels into one, so small text shrinks to
/// grey rather than to noise, and turn BGRA into RGBA on the way.
fn shrink(bgra: &[u8], width: u32, height: u32, max_width: u32, max_height: u32) -> Thumbnail {
    let scale = (max_width as f64 / width as f64)
        .min(max_height as f64 / height as f64)
        .min(1.0);
    let out_w = ((width as f64 * scale).round() as u32).max(1);
    let out_h = ((height as f64 * scale).round() as u32).max(1);
    let mut rgba = Vec::with_capacity((out_w * out_h * 4) as usize);
    for oy in 0..out_h {
        let y0 = (oy as u64 * height as u64 / out_h as u64) as u32;
        let y1 = (((oy + 1) as u64 * height as u64 / out_h as u64) as u32).max(y0 + 1);
        for ox in 0..out_w {
            let x0 = (ox as u64 * width as u64 / out_w as u64) as u32;
            let x1 = (((ox + 1) as u64 * width as u64 / out_w as u64) as u32).max(x0 + 1);
            let mut sum = [0u64; 3];
            for y in y0..y1 {
                let row = (y * width) as usize * 4;
                for x in x0..x1 {
                    let p = row + x as usize * 4;
                    sum[0] += bgra[p + 2] as u64;
                    sum[1] += bgra[p + 1] as u64;
                    sum[2] += bgra[p] as u64;
                }
            }
            let n = ((y1 - y0) * (x1 - x0)) as u64;
            rgba.extend_from_slice(&[
                (sum[0] / n) as u8,
                (sum[1] / n) as u8,
                (sum[2] / n) as u8,
                255,
            ]);
        }
    }
    Thumbnail {
        width: out_w,
        height: out_h,
        rgba,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shrinking_keeps_the_shape_and_averages_colour() {
        // 4x2 BGRA: left half pure blue, right half pure red.
        let mut bgra = Vec::new();
        for _ in 0..2 {
            for x in 0..4 {
                bgra.extend_from_slice(if x < 2 {
                    &[255, 0, 0, 255]
                } else {
                    &[0, 0, 255, 255]
                });
            }
        }
        let t = shrink(&bgra, 4, 2, 2, 2);
        assert_eq!((t.width, t.height), (2, 1));
        assert_eq!(&t.rgba[..4], &[0, 0, 255, 255], "blue, now as RGBA");
        assert_eq!(&t.rgba[4..], &[255, 0, 0, 255], "red, now as RGBA");
    }

    #[test]
    fn an_empty_area_has_no_thumbnail() {
        assert!(thumbnail(Rect::new(0, 0, 0, 10), 100, 100).is_none());
    }

    #[test]
    fn a_thumbnail_of_a_real_screen_fits() {
        let Ok(monitors) = crate::monitors() else {
            return;
        };
        let t = thumbnail(monitors[0].bounds, 320, 320).expect("a thumbnail of a real screen");
        assert!(t.width <= 320 && t.height <= 320);
        assert_eq!(t.rgba.len(), (t.width * t.height * 4) as usize);
    }
}
