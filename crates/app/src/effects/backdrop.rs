//! Backdrop: the picture inset on a ground of its own.
//!
//! The look most demos are published in. It is the one effect that changes
//! where the picture is rather than what is drawn over it, so it runs first
//! and hands back the rectangle the others are measured against.

use eframe::egui;
use screenilux_core::edit::{Backdrop as Style, Effect, Tool};

use super::ramp;
use crate::theme;

/// Where the picture sits once a backdrop has inset it, and what is drawn
/// behind. `None` when no backdrop is on, which is the ordinary case.
///
/// This is the look most demos are published in: the recording pulled off
/// the edges of the frame so it reads as a thing being shown rather than a
/// screen you are sitting at.
pub fn backdrop(
    painter: &egui::Painter,
    p: &theme::Palette,
    rect: egui::Rect,
    live: &[&Effect],
    at: f64,
) -> egui::Rect {
    let Some((pad, style, start, end)) = live.iter().find_map(|e| match &e.tool {
        Tool::Backdrop { pad, style } => Some((*pad, *style, e.start, e.end)),
        _ => None,
    }) else {
        return rect;
    };
    let t = ramp(at, start, end);
    if t <= 0.0 {
        return rect;
    }
    let inset = rect.width().min(rect.height()) * pad.clamp(0.0, 0.3) * t;
    let inner = rect.shrink(inset);
    if inner.width() < 8.0 || inner.height() < 8.0 {
        return rect;
    }
    match style {
        Style::Solid => {
            painter.rect_filled(rect, 0.0, p.stage);
        }
        Style::Gradient | Style::Blurred => {
            // A gradient in bands: a painter has no shader, and at this
            // size the steps are below what the eye picks out. The blurred
            // style falls back to it until the picture can be sampled
            // behind itself.
            let bands = 24;
            let top = theme::blend(p.stage, p.signal, 0.35);
            for i in 0..bands {
                let f = i as f32 / bands as f32;
                let next = (i + 1) as f32 / bands as f32;
                let band = egui::Rect::from_min_max(
                    egui::pos2(rect.left(), rect.top() + f * rect.height()),
                    egui::pos2(rect.right(), rect.top() + next * rect.height()),
                );
                painter.rect_filled(band, 0.0, theme::blend(top, p.stage, f));
            }
        }
    }
    painter.rect_filled(
        inner.expand(1.0),
        egui::CornerRadius::same((inset * 0.35).clamp(0.0, 18.0) as u8),
        egui::Color32::from_black_alpha(40),
    );
    inner
}
