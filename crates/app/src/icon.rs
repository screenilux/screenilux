//! The app's icon, drawn in code.
//!
//! The Screenilux mark on a deep teal tile: two screens overlapping at a
//! corner, joined by the live recording dot. The tile keeps it readable on a
//! light taskbar and a dark one alike. Drawn rather than shipped as an image
//! so the window icon and the executable's icon (rendered by `build.rs`,
//! which includes this file) cannot drift apart from each other, and match
//! `docs/logo-dark.svg`.

use tiny_skia::{Color, FillRule, Paint, PathBuilder, Pixmap, Transform};

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
    let tile = paint(0x01, 0x26, 0x25);
    pm.fill_path(
        &rounded(8.0, 8.0, 240.0, 240.0, 56.0),
        &tile,
        FillRule::Winding,
        t,
        None,
    );

    // The mark, drawn on the logo's own 64 grid and fitted inside the tile.
    let m = t.pre_translate(40.0, 40.0).pre_scale(2.75, 2.75);
    let fill = |pm: &mut Pixmap, path: &tiny_skia::Path, p: &Paint| {
        pm.fill_path(path, p, FillRule::Winding, m, None);
    };
    let circle = |r: f32| PathBuilder::from_circle(32.0, 32.0, r).expect("circle");
    fill(
        &mut pm,
        &rounded(3.0, 7.0, 40.0, 29.0, 8.0),
        &paint(0x07, 0xBE, 0xB8),
    );
    fill(
        &mut pm,
        &rounded(21.0, 28.0, 40.0, 29.0, 8.0),
        &paint(0xE1, 0xF7, 0xF7),
    );
    // The gap around the dot, cut back to the tile.
    fill(&mut pm, &circle(10.5), &tile);
    fill(&mut pm, &circle(6.5), &paint(0x2D, 0xA8, 0xA4));
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
