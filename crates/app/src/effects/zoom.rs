//! Zoom: which part of the picture is on screen.
//!
//! Alone among the effects this one is not painted. It decides the slice of
//! the texture everything else is drawn against, which is why magnifying
//! costs nothing per frame.

use eframe::egui;
use screenilux_core::edit::Effect;

/// The lens, able to follow the pointer when `on` says where it is. The
/// arithmetic is core's, shared with the export.
pub fn lens_following(live: &[&Effect], at: f64, on: Option<egui::Pos2>) -> egui::Rect {
    let a = screenilux_core::look::lens(live, at, on.map(|p| (p.x, p.y)));
    egui::Rect::from_min_size(egui::pos2(a.x, a.y), egui::vec2(a.w, a.h))
}
