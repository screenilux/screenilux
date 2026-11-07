//! The pointer and the shortcuts, drawn over the picture for a recording
//! that left them out of it.
//!
//! Where the pointer is, how visible, and which shortcut is up come from
//! core's `look`, which the export draws from too.

use eframe::egui;
use screenilux_core::edit::PointerStyle;
use screenilux_core::geometry::Rect as Desktop;
use screenilux_core::keys::Shortcut;
use screenilux_core::look;
use screenilux_core::pointer::Track;

use super::geometry::{on_screen, spot_on};

/// The arrow, on a 20-unit-tall grid, in two convex halves so it can be
/// filled: the head, and the tail.
const HEAD: [(f32, f32); 3] = [(0.0, 0.0), (0.0, 17.0), (12.4, 12.2)];
const TAIL: [(f32, f32); 4] = [(4.2, 13.2), (7.0, 19.6), (9.8, 18.4), (7.0, 12.2)];
const OUTLINE: [(f32, f32); 7] = [
    (0.0, 0.0),
    (0.0, 17.0),
    (4.2, 13.2),
    (7.0, 19.6),
    (9.8, 18.4),
    (7.0, 12.2),
    (12.4, 12.2),
];

/// Draw the pointer where it was at `at`, on the screen at `bounds`.
#[allow(clippy::too_many_arguments)]
pub fn cursor(
    painter: &egui::Painter,
    rect: egui::Rect,
    shown: egui::Rect,
    track: &Track,
    bounds: Desktop,
    at: f64,
    style: &PointerStyle,
) {
    let Some(((x, y), alpha)) = look::cursor_at(track, at, style) else {
        return;
    };
    let Some(spot) = on_screen(x.round() as i32, y.round() as i32, bounds) else {
        return;
    };
    let Some(tip) = spot_on(rect, shown, spot) else {
        return;
    };
    // The pointer grows with the zoom, as it would if it were in the picture.
    let magnified = (1.0 / shown.width().max(0.05)).clamp(1.0, 4.0).sqrt();
    let size = rect.height() / 1080.0 * 22.0 * style.size * magnified;
    let k = size / 20.0;
    let points = |shape: &[(f32, f32)]| -> Vec<egui::Pos2> {
        shape
            .iter()
            .map(|(px, py)| tip + egui::vec2(px * k, py * k))
            .collect()
    };
    let white = egui::Color32::from_white_alpha((alpha * 255.0) as u8);
    let edge = egui::Color32::from_black_alpha((alpha * 230.0) as u8);
    painter.add(egui::Shape::convex_polygon(
        points(&HEAD),
        white,
        egui::Stroke::NONE,
    ));
    painter.add(egui::Shape::convex_polygon(
        points(&TAIL),
        white,
        egui::Stroke::NONE,
    ));
    painter.add(egui::Shape::closed_line(
        points(&OUTLINE),
        egui::Stroke::new((1.4 * k).max(1.0), edge),
    ));
}

/// The shortcut pressed just before `at`, in a dark pill near the bottom.
pub fn shortcut(painter: &egui::Painter, rect: egui::Rect, shortcuts: &[Shortcut], at: f64) {
    let Some((keys, fade)) = look::shortcut_at(shortcuts, at) else {
        return;
    };
    let size = (rect.height() * 0.036).clamp(12.0, 26.0);
    let galley = painter.layout_no_wrap(
        keys.to_string(),
        egui::FontId::new(size, crate::theme::strong()),
        egui::Color32::from_white_alpha((fade * 255.0) as u8),
    );
    let pad = egui::vec2(size * 0.7, size * 0.35);
    let centre = egui::pos2(rect.center().x, rect.bottom() - rect.height() * 0.11);
    let pill = egui::Rect::from_center_size(centre, galley.size() + pad * 2.0);
    painter.rect_filled(
        pill,
        egui::CornerRadius::same((size * 0.5) as u8),
        egui::Color32::from_rgba_unmultiplied(0x01, 0x14, 0x13, (fade * 220.0) as u8),
    );
    painter.galley(pill.min + pad, galley, egui::Color32::WHITE);
}
