//! The effects that act on a region of the picture: hide, spotlight and
//! callout. Blur is one of these too, but has enough of its own to say
//! that it lives next door.
//!
//! Each is a few lines, and each takes the same two rectangles: where the
//! region landed on screen, and the picture it landed on. They are grouped
//! rather than given a file each because what they have in common — a
//! region, and something done to what is around it — is most of what there
//! is to say about them.

use eframe::egui;

use super::geometry::around;
use crate::theme;

/// Hide: the region covered over. For whatever should not have been on
/// screen, which is why it is solid rather than softened.
pub fn hide(painter: &egui::Painter, p: &theme::Palette, on: egui::Rect) {
    painter.rect_filled(on, egui::CornerRadius::same(4), p.bar);
}

/// Spotlight: the region left alone and everything else dimmed.
///
/// The dark is painted as the four panes around the lit part, since a
/// painter has no hole to punch.
pub fn spotlight(
    painter: &egui::Painter,
    p: &theme::Palette,
    rect: egui::Rect,
    on: egui::Rect,
    dim: f32,
    weight: f32,
) {
    let veil = egui::Color32::from_black_alpha((dim.clamp(0.0, 1.0) * weight * 255.0) as u8);
    for pane in around(rect, on) {
        painter.rect_filled(pane, 0.0, veil);
    }
    painter.rect_stroke(
        on,
        egui::CornerRadius::same(6),
        egui::Stroke::new(1.5, p.signal),
        egui::StrokeKind::Outside,
    );
}

/// Callout: a spotlight with something to say, in a label above it.
pub fn callout(
    painter: &egui::Painter,
    p: &theme::Palette,
    rect: egui::Rect,
    on: egui::Rect,
    text: &str,
    weight: f32,
) {
    for pane in around(rect, on) {
        painter.rect_filled(
            pane,
            0.0,
            egui::Color32::from_black_alpha((96.0 * weight) as u8),
        );
    }
    painter.rect_stroke(
        on,
        egui::CornerRadius::same(6),
        egui::Stroke::new(2.0, p.signal),
        egui::StrokeKind::Outside,
    );
    if text.is_empty() {
        return;
    }
    let at = egui::pos2(on.center().x, on.top() - 10.0);
    let galley = painter.layout_no_wrap(
        text.to_string(),
        egui::FontId::new(15.0, theme::strong()),
        theme::ON_DARK,
    );
    let pad = egui::vec2(10.0, 5.0);
    let box_rect = egui::Rect::from_center_size(at, galley.size() + pad * 2.0);
    painter.rect_filled(box_rect, egui::CornerRadius::same(8), p.action);
    painter.galley(box_rect.min + pad, galley, theme::ON_DARK);
}
