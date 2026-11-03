//! Caption: words over the picture.
//!
//! Sized to the picture rather than fixed, so a caption reads the same on a
//! window and on a full screen.

use eframe::egui;
use screenilux_core::edit::Place;

use crate::theme;

/// Words over the picture, where the effect says to put them.
pub(super) fn caption(
    painter: &egui::Painter,
    p: &theme::Palette,
    rect: egui::Rect,
    text: &str,
    place: Place,
) {
    let size = (rect.height() * 0.045).clamp(13.0, 30.0);
    let galley = painter.layout(
        text.to_string(),
        egui::FontId::new(size, theme::strong()),
        theme::ON_DARK,
        rect.width() * 0.8,
    );
    let pad = egui::vec2(size * 0.8, size * 0.45);
    let y = match place {
        Place::Bottom => rect.bottom() - rect.height() * 0.08 - galley.size().y / 2.0,
        Place::Centre => rect.center().y,
        Place::Top => rect.top() + rect.height() * 0.08 + galley.size().y / 2.0,
    };
    let box_rect =
        egui::Rect::from_center_size(egui::pos2(rect.center().x, y), galley.size() + pad * 2.0);
    painter.rect_filled(
        box_rect,
        egui::CornerRadius::same(10),
        egui::Color32::from_black_alpha(190),
    );
    painter.galley(box_rect.min + pad, galley, theme::ON_DARK);
    let _ = p;
}
