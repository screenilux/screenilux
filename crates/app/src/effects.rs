//! Drawing the edit list over a frame.
//!
//! Effects are held as data beside a recording and drawn on the way out,
//! which is what makes any of them undoable. This is the "on the way out"
//! for the picture on screen: given the effects live at the current moment,
//! it works out what the viewer should see and paints it.
//!
//! It is a preview, not the export. A zoom is a sub-rectangle of the
//! texture rather than a resampled frame, and a blur is the picture sampled
//! coarsely rather than a gaussian. Both read the way the finished thing
//! will and cost nothing per frame, which is what a scrubbing editor needs.

use eframe::egui;
use screenilux_core::edit::{Area, Effect, Place, Tool};

use crate::theme;

/// How coarsely a blurred region is sampled. Low enough to be unreadable,
/// high enough to keep the shape of what was there.
const BLUR_STEPS: u32 = 22;

/// The effects that apply to one screen: those naming it, and those naming
/// no screen at all.
pub fn for_screen<'a>(live: &'a [Effect], screen: Option<&str>) -> Vec<&'a Effect> {
    live.iter()
        .filter(|e| match (&e.screen, screen) {
            (None, _) => true,
            (Some(want), Some(have)) => want == have,
            (Some(_), None) => false,
        })
        .collect()
}

/// The part of the texture to draw, which is the whole of it unless a zoom
/// is on. Returned in texture coordinates, so the picture magnifies without
/// anything being resampled.
pub fn lens(live: &[&Effect]) -> egui::Rect {
    let whole = egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0));
    let Some(area) = live.iter().find_map(|e| match &e.tool {
        Tool::Zoom { area, .. } => Some(*area),
        _ => None,
    }) else {
        return whole;
    };
    // The area is a fraction of the screen, which is what texture
    // coordinates are, so it is already the rectangle to sample.
    let r = egui::Rect::from_min_max(
        egui::pos2(area.x.clamp(0.0, 1.0), area.y.clamp(0.0, 1.0)),
        egui::pos2(
            (area.x + area.w).clamp(0.0, 1.0),
            (area.y + area.h).clamp(0.0, 1.0),
        ),
    );
    if r.width() < 0.02 || r.height() < 0.02 {
        whole
    } else {
        r
    }
}

/// Everything that is painted over the picture rather than sampled out of
/// it: what is hidden, blurred, lit or pointed at.
///
/// `shown` is the part of the texture on screen, so an overlay placed on a
/// zoomed picture lands where the viewer sees it rather than where it was
/// clicked.
pub fn over(
    painter: &egui::Painter,
    p: &theme::Palette,
    rect: egui::Rect,
    shown: egui::Rect,
    texture: Option<egui::TextureId>,
    live: &[&Effect],
) {
    for effect in live {
        let Some(area) = effect.tool.area() else {
            continue;
        };
        let Some(on) = place(rect, shown, area) else {
            continue;
        };
        match &effect.tool {
            Tool::Hide { .. } => {
                painter.rect_filled(on, egui::CornerRadius::same(4), p.bar);
            }
            Tool::Blur { strength, .. } => {
                if let Some(tex) = texture {
                    pixelate(painter, tex, on, shown, area, *strength);
                } else {
                    painter.rect_filled(on, egui::CornerRadius::same(4), p.surface);
                }
            }
            Tool::Spotlight { dim, .. } => {
                // The dark is painted as the four panes around the lit part,
                // since a painter has no hole to punch.
                let veil = egui::Color32::from_black_alpha((dim.clamp(0.0, 1.0) * 255.0) as u8);
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
            Tool::Callout { text, .. } => {
                for pane in around(rect, on) {
                    painter.rect_filled(pane, 0.0, egui::Color32::from_black_alpha(96));
                }
                painter.rect_stroke(
                    on,
                    egui::CornerRadius::same(6),
                    egui::Stroke::new(2.0, p.signal),
                    egui::StrokeKind::Outside,
                );
                if !text.is_empty() {
                    let at = egui::pos2(on.center().x, on.top() - 10.0);
                    let galley = painter.layout_no_wrap(
                        text.clone(),
                        egui::FontId::new(15.0, theme::strong()),
                        theme::ON_DARK,
                    );
                    let pad = egui::vec2(10.0, 5.0);
                    let box_rect = egui::Rect::from_center_size(at, galley.size() + pad * 2.0);
                    painter.rect_filled(box_rect, egui::CornerRadius::same(8), p.action);
                    painter.galley(box_rect.min + pad, galley, theme::ON_DARK);
                }
            }
            _ => {}
        }
    }

    // Captions sit on the picture as a whole rather than on a region.
    for effect in live {
        let Tool::Caption { text, place } = &effect.tool else {
            continue;
        };
        if text.is_empty() {
            continue;
        }
        caption(painter, p, rect, text, *place);
    }
}

/// Where an area lands on screen, given which part of the picture is shown.
/// `None` when the zoom has pushed it out of view.
fn place(rect: egui::Rect, shown: egui::Rect, area: Area) -> Option<egui::Rect> {
    let to_x = |f: f32| rect.left() + (f - shown.left()) / shown.width() * rect.width();
    let to_y = |f: f32| rect.top() + (f - shown.top()) / shown.height() * rect.height();
    let on = egui::Rect::from_min_max(
        egui::pos2(to_x(area.x), to_y(area.y)),
        egui::pos2(to_x(area.x + area.w), to_y(area.y + area.h)),
    );
    let clipped = on.intersect(rect);
    (clipped.width() > 1.0 && clipped.height() > 1.0).then_some(clipped)
}

/// The four panes of `outer` left over once `hole` is taken out of it.
fn around(outer: egui::Rect, hole: egui::Rect) -> [egui::Rect; 4] {
    [
        egui::Rect::from_min_max(outer.min, egui::pos2(outer.right(), hole.top())),
        egui::Rect::from_min_max(egui::pos2(outer.left(), hole.bottom()), outer.max),
        egui::Rect::from_min_max(
            egui::pos2(outer.left(), hole.top()),
            egui::pos2(hole.left(), hole.bottom()),
        ),
        egui::Rect::from_min_max(
            egui::pos2(hole.right(), hole.top()),
            egui::pos2(outer.right(), hole.bottom()),
        ),
    ]
}

/// A region drawn coarsely, which reads as blurred and costs one quad per
/// block rather than a pass over the pixels.
fn pixelate(
    painter: &egui::Painter,
    texture: egui::TextureId,
    on: egui::Rect,
    shown: egui::Rect,
    area: Area,
    strength: f32,
) {
    // More strength, fewer and bigger blocks.
    let steps = (BLUR_STEPS as f32 * (1.05 - strength.clamp(0.0, 1.0)))
        .round()
        .clamp(3.0, BLUR_STEPS as f32) as u32;
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

/// Words over the picture, where the effect says to put them.
fn caption(
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

#[cfg(test)]
mod tests {
    use super::*;
    use screenilux_core::edit::EditList;

    fn zoom_on(area: Area) -> Effect {
        let mut edits = EditList::default();
        let id = edits.add(
            0.0,
            5.0,
            Tool::Zoom {
                area,
                follow: false,
                scale: 2.0,
            },
        );
        edits.get(id).unwrap().clone()
    }

    #[test]
    fn with_no_effects_the_whole_picture_is_shown() {
        assert_eq!(
            lens(&[]),
            egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0))
        );
    }

    #[test]
    fn a_zoom_shows_the_part_it_was_placed_on() {
        let e = zoom_on(Area::new(0.25, 0.1, 0.5, 0.4));
        let shown = lens(&[&e]);
        assert!((shown.left() - 0.25).abs() < 1e-6);
        assert!((shown.top() - 0.1).abs() < 1e-6);
        assert!((shown.width() - 0.5).abs() < 1e-6);
        assert!((shown.height() - 0.4).abs() < 1e-6);
    }

    #[test]
    fn a_zoom_too_small_to_sample_is_ignored() {
        let e = zoom_on(Area::new(0.5, 0.5, 0.001, 0.001));
        assert_eq!(lens(&[&e]).width(), 1.0, "a sliver would magnify to mush");
    }

    #[test]
    fn an_effect_only_lands_on_the_screen_it_names() {
        let mut edits = EditList::default();
        let one = edits.add_on(0.0, 1.0, Tool::MarkClicks, Some("screen1.mp4".into()));
        let any = edits.add(0.0, 1.0, Tool::Mute);
        let all: Vec<Effect> = edits.effects.clone();
        assert_eq!(for_screen(&all, Some("screen1.mp4")).len(), 2);
        assert_eq!(
            for_screen(&all, Some("screen2.mp4")).len(),
            1,
            "only the one naming no screen"
        );
        let _ = (one, any);
    }

    #[test]
    fn an_area_is_placed_where_the_zoom_puts_it() {
        let rect = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(100.0, 100.0));
        let whole = egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0));
        // Unzoomed: a half-width area covers half the picture.
        let on = place(rect, whole, Area::new(0.0, 0.0, 0.5, 0.5)).unwrap();
        assert_eq!(
            on,
            egui::Rect::from_min_size(rect.min, egui::vec2(50.0, 50.0))
        );
        // Zoomed into the left half: the same area now fills the view.
        let shown = egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(0.5, 0.5));
        let on = place(rect, shown, Area::new(0.0, 0.0, 0.5, 0.5)).unwrap();
        assert_eq!(on, rect);
    }

    #[test]
    fn an_area_the_zoom_left_behind_is_not_drawn() {
        let rect = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(100.0, 100.0));
        let shown = egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(0.4, 0.4));
        assert!(place(rect, shown, Area::new(0.6, 0.6, 0.2, 0.2)).is_none());
    }

    #[test]
    fn the_panes_around_a_hole_cover_the_rest_of_the_picture() {
        let outer = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(100.0, 100.0));
        let hole = egui::Rect::from_min_size(egui::pos2(25.0, 25.0), egui::vec2(50.0, 50.0));
        let panes = around(outer, hole);
        let covered: f32 = panes.iter().map(|r| r.width() * r.height()).sum();
        assert!(
            (covered - (100.0 * 100.0 - 50.0 * 50.0)).abs() < 0.5,
            "the panes and the hole should account for the whole picture"
        );
        assert!(panes.iter().all(|r| !r.intersects(hole.shrink(0.5))));
    }
}
