//! Blur: a region sampled too coarsely to read.
//!
//! Not a gaussian. The region is drawn as a grid of blocks, each taking its
//! colour from one texel in the middle of it, and the linear filtering does
//! the averaging. It reads as a blur, costs one quad per block, and needs
//! no pass over the pixels.

use eframe::egui;
use screenilux_core::edit::Area;

/// How coarsely a blurred region is sampled. Low enough to be unreadable,
/// high enough to keep the shape of what was there.
const STEPS: u32 = 22;

/// A region drawn coarsely, which reads as blurred and costs one quad per
/// block rather than a pass over the pixels.
pub(super) fn pixelate(
    painter: &egui::Painter,
    texture: egui::TextureId,
    on: egui::Rect,
    shown: egui::Rect,
    area: Area,
    strength: f32,
) {
    // More strength, fewer and bigger blocks.
    let steps = (STEPS as f32 * (1.05 - strength.clamp(0.0, 1.0)))
        .round()
        .clamp(3.0, STEPS as f32) as u32;
    let uv = egui::Rect::from_min_max(
        egui::pos2(
            (area.x - shown.left()) / shown.width(),
            (area.y - shown.top()) / shown.height(),
        ),
        egui::pos2(
            (area.x + area.w - shown.left()) / shown.width(),
            (area.y + area.h - shown.top()) / shown.height(),
        ),
    );
    // The texture coordinates of the blurred part, in the picture's own
    // space rather than the shown part's.
    let src = egui::Rect::from_min_max(
        egui::pos2(
            shown.left() + uv.left() * shown.width(),
            shown.top() + uv.top() * shown.height(),
        ),
        egui::pos2(
            shown.left() + uv.right() * shown.width(),
            shown.top() + uv.bottom() * shown.height(),
        ),
    );
    let cols = steps.max(1);
    let rows = ((steps as f32 * on.height() / on.width().max(1.0)).round() as u32).clamp(2, steps);
    for row in 0..rows {
        for col in 0..cols {
            let fx = col as f32 / cols as f32;
            let fy = row as f32 / rows as f32;
            let fx1 = (col + 1) as f32 / cols as f32;
            let fy1 = (row + 1) as f32 / rows as f32;
            let block = egui::Rect::from_min_max(
                egui::pos2(on.left() + fx * on.width(), on.top() + fy * on.height()),
                egui::pos2(on.left() + fx1 * on.width(), on.top() + fy1 * on.height()),
            );
            // One texel from the middle of the block: linear filtering over
            // a sliver this small is what does the averaging.
            let cx = src.left() + (fx + fx1) / 2.0 * src.width();
            let cy = src.top() + (fy + fy1) / 2.0 * src.height();
            let dot = egui::Rect::from_center_size(egui::pos2(cx, cy), egui::vec2(0.001, 0.001));
            painter.image(texture, block, dot, egui::Color32::WHITE);
        }
    }
}
