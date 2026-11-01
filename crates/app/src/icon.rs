//! The app's icon, drawn in code.
//!
//! Two screens, the nearer one outlined in the signal blue, with a red tally
//! dot: several screens, one being recorded. Drawn rather than shipped as an
//! image so the window icon and the executable's icon (rendered by
//! `build.rs`, which includes this file) cannot drift apart.

use tiny_skia::{Color, FillRule, Paint, PathBuilder, Pixmap, Stroke, Transform};

fn rounded(x: f32, y: f32, w: f32, h: f32, r: f32) -> tiny_skia::Path {
    let mut pb = PathBuilder::new();
    pb.move_to(x + r, y);
    pb.line_to(x + w - r, y);
    pb.quad_to(x + w, y, x + w, y + r);
    pb.line_to(x + w, y + h - r);
    pb.quad_to(x + w, y + h, x + w - r, y + h);
    pb.line_to(x + r, y + h);
    pb.quad_to(x, y + h, x, y + h - r);
    pb.line_to(x, y + r);
    pb.quad_to(x, y, x + r, y);
    pb.close();
    pb.finish().expect("a rounded rectangle is a valid path")
}

fn paint(r: u8, g: u8, b: u8) -> Paint<'static> {
    let mut p = Paint::default();
    p.set_color(Color::from_rgba8(r, g, b, 255));
    p.anti_alias = true;
    p
}

/// The icon at `size`×`size`, as a pixmap (premultiplied RGBA).
pub fn render(size: u32) -> Pixmap {
    let mut pm = Pixmap::new(size, size).expect("icon size is non-zero");
    // Drawn on a 256 grid and scaled.
    let t = Transform::from_scale(size as f32 / 256.0, size as f32 / 256.0);
    // Small sizes get thicker lines, or the screens dissolve into a blur.
    let line = if size <= 24 {
        22.0
    } else if size <= 48 {
        18.0
    } else {
        14.0
    };

    pm.fill_path(
        &rounded(8.0, 8.0, 240.0, 240.0, 56.0),
        &paint(0x24, 0x28, 0x2E),
        FillRule::Winding,
        t,
        None,
    );
    let stroke = Stroke {
        width: line,
        ..Stroke::default()
    };
    // The screen behind.
    pm.stroke_path(
        &rounded(40.0, 52.0, 128.0, 88.0, 14.0),
        &paint(0x9B, 0xA3, 0xAD),
        &stroke,
        t,
        None,
    );
    // The screen in front, filled so it hides the one behind.
    let front = rounded(84.0, 104.0, 132.0, 92.0, 14.0);
    pm.fill_path(&front, &paint(0x2E, 0x33, 0x3A), FillRule::Winding, t, None);
    pm.stroke_path(&front, &paint(0x4C, 0x8D, 0xF6), &stroke, t, None);
    // The tally: ringed in the background colour so it reads at 16px.
    let ring = PathBuilder::from_circle(206.0, 70.0, 34.0).expect("circle");
    pm.fill_path(&ring, &paint(0x24, 0x28, 0x2E), FillRule::Winding, t, None);
    let dot = PathBuilder::from_circle(206.0, 70.0, 24.0).expect("circle");
    pm.fill_path(&dot, &paint(0xE5, 0x48, 0x4D), FillRule::Winding, t, None);
    pm
}

/// Straight (not premultiplied) RGBA, as window icons expect.
#[allow(dead_code)] // used by the app, not by build.rs, which includes this file
pub fn rgba(size: u32) -> Vec<u8> {
    render(size)
        .pixels()
        .iter()
        .flat_map(|p| {
            let c = p.demultiply();
            [c.red(), c.green(), c.blue(), c.alpha()]
        })
        .collect()
}

/// A Windows `.ico` holding the icon at every size Explorer asks for, each
/// stored as PNG (supported since Windows Vista).
#[allow(dead_code)] // used by build.rs, which includes this file
pub fn ico() -> Vec<u8> {
    let sizes = [16u32, 20, 24, 32, 40, 48, 64, 128, 256];
    let images: Vec<Vec<u8>> = sizes
        .iter()
        .map(|&s| render(s).encode_png().expect("PNG encoding"))
        .collect();
    let mut out = Vec::new();
    out.extend_from_slice(&[0, 0, 1, 0]);
    out.extend_from_slice(&(sizes.len() as u16).to_le_bytes());
    let mut offset = 6 + 16 * sizes.len() as u32;
    for (s, png) in sizes.iter().zip(&images) {
        let dim = if *s >= 256 { 0 } else { *s as u8 };
        out.extend_from_slice(&[dim, dim, 0, 0]);
        out.extend_from_slice(&1u16.to_le_bytes()); // colour planes
        out.extend_from_slice(&32u16.to_le_bytes()); // bits per pixel
        out.extend_from_slice(&(png.len() as u32).to_le_bytes());
        out.extend_from_slice(&offset.to_le_bytes());
        offset += png.len() as u32;
    }
    for png in images {
        out.extend_from_slice(&png);
    }
    out
}
