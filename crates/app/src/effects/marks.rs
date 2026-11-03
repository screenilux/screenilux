//! Mark every click: a ring where the pointer was pressed.
//!
//! The ring grows and fades over its own short life, so the eye is drawn to
//! a click after it happens rather than having to be watching for it. The
//! clicks come from the pointer track the recorder keeps, so a recording
//! made before it did has nothing to mark.

use eframe::egui;
use screenilux_core::geometry::Rect as Desktop;
use screenilux_core::pointer::Track;

use super::geometry::{on_screen, spot_on};
use crate::theme;

/// How long a mark takes to fade.
pub const LIFE: f64 = 0.55;

pub fn clicks(
    painter: &egui::Painter,
    p: &theme::Palette,
    rect: egui::Rect,
    shown: egui::Rect,
    track: &Track,
    bounds: Desktop,
    at: f64,
) {
    for (click, age) in track.clicks_at(at, LIFE) {
        let Some(spot) = on_screen(click.x, click.y, bounds) else {
            continue;
        };
        let Some(on) = spot_on(rect, shown, spot) else {
            continue;
        };
        let grow = rect.width().min(rect.height()) * 0.04;
        let fade = (1.0 - age as f32).clamp(0.0, 1.0);
        painter.circle_stroke(
            on,
            grow * (0.35 + age as f32),
            egui::Stroke::new(2.5 * fade, p.signal),
        );
    }
}
