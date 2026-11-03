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
use screenilux_core::edit::{Area, Backdrop, Effect, Place, Tool};
use screenilux_core::geometry::Rect as Desktop;
use screenilux_core::pointer::Track;

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

/// How long a zoom takes to come on, and to let go again. Short enough not
/// to be waited through, long enough that the eye follows the picture in
/// rather than being dropped into it.
const RAMP: f64 = 0.45;

/// How far through an effect's own span `at` is, as a weight from 0 to 1
/// that rises at the start and falls at the end.
///
/// Without this an effect snaps on: one frame the picture is whole, the
/// next it is magnified, and the viewer has to work out what they are
/// looking at. The curve is a smoothstep, so it leaves and arrives at rest.
pub fn ramp(at: f64, start: f64, end: f64) -> f32 {
    let span = (end - start).max(1e-6);
    // A short effect never reaches full strength rather than snapping: it
    // gets half its length to rise and half to fall.
    let r = RAMP.min(span / 2.0).max(1e-6);
    let rise = ((at - start) / r).clamp(0.0, 1.0);
    let fall = ((end - at) / r).clamp(0.0, 1.0);
    let x = rise.min(fall) as f32;
    x * x * (3.0 - 2.0 * x)
}

/// The part of the texture to draw, which is the whole of it unless a zoom
/// is on.
///
/// How close it gets is the effect's own `scale`, not the size of the
/// rectangle that was clicked: a click says *where* to look, and the
/// Closeness slider says *how near*. Using the clicked rectangle as the
/// viewport is what made every zoom the same hard jump whatever the slider
/// said.
/// How long a click's mark takes to fade.
const MARK: f64 = 0.55;

/// Where the pointer is on one screen now, as a fraction of it, if the
/// recording wrote its path down.
pub fn pointer_on(track: Option<&Track>, bounds: Desktop, at: f64) -> Option<egui::Pos2> {
    let p = track?.at(at)?;
    on_screen(p.x, p.y, bounds)
}

/// A desktop position as a fraction of one screen, or `None` when it is on
/// another one.
fn on_screen(x: i32, y: i32, b: Desktop) -> Option<egui::Pos2> {
    if b.width == 0 || b.height == 0 {
        return None;
    }
    let fx = (x - b.left) as f32 / b.width as f32;
    let fy = (y - b.top) as f32 / b.height as f32;
    ((0.0..=1.0).contains(&fx) && (0.0..=1.0).contains(&fy)).then_some(egui::pos2(fx, fy))
}

/// The lens, able to follow the pointer when `on` says where it is.
pub fn lens_following(live: &[&Effect], at: f64, on: Option<egui::Pos2>) -> egui::Rect {
    let whole = egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0));
    let Some((area, scale, follow, start, end)) = live.iter().find_map(|e| match &e.tool {
        Tool::Zoom {
            area,
            scale,
            follow,
        } => Some((*area, *scale, *follow, e.start, e.end)),
        _ => None,
    }) else {
        return whole;
    };
    let t = ramp(at, start, end);
    if t <= 0.0 {
        return whole;
    }
    // Closeness is how many times nearer, so the slice shown is its
    // reciprocal. The same fraction on both axes keeps the picture's shape.
    let side = (1.0 / scale.max(1.01)).clamp(0.08, 1.0);
    let half = side / 2.0;
    // Centred on what was clicked, pulled back inside the picture so a zoom
    // near an edge shows picture rather than nothing.
    // Following keeps the pointer in the middle of the view. Without a
    // path to follow it holds on what was clicked, which is all a
    // recording made before the pointer was written down can do.
    let (cx, cy) = match (follow, on) {
        (true, Some(p)) => (p.x, p.y),
        _ => (area.x + area.w / 2.0, area.y + area.h / 2.0),
    };
    let cx = cx.clamp(half, 1.0 - half);
    let cy = cy.clamp(half, 1.0 - half);
    let near = egui::Rect::from_center_size(egui::pos2(cx, cy), egui::vec2(side, side));
    egui::Rect::from_min_max(
        whole.min + (near.min - whole.min) * t,
        whole.max + (near.max - whole.max) * t,
    )
}

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
        Backdrop::Solid => {
            painter.rect_filled(rect, 0.0, p.stage);
        }
        Backdrop::Gradient | Backdrop::Blurred => {
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

/// Everything that is painted over the picture rather than sampled out of
/// it: what is hidden, blurred, lit or pointed at.
///
/// `shown` is the part of the texture on screen, so an overlay placed on a
/// zoomed picture lands where the viewer sees it rather than where it was
/// clicked.
#[allow(clippy::too_many_arguments)]
pub fn over(
    painter: &egui::Painter,
    p: &theme::Palette,
    rect: egui::Rect,
    shown: egui::Rect,
    texture: Option<egui::TextureId>,
    live: &[&Effect],
    at: f64,
    marks: Option<(&Track, Desktop)>,
) {
    for effect in live {
        // Overlays come and go on the same curve as the zoom, so a stretch
        // with several effects on it reads as one move rather than a set of
        // things switching on at slightly different moments.
        let t = ramp(at, effect.start, effect.end);
        if t <= 0.0 {
            continue;
        }
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
                let veil = egui::Color32::from_black_alpha((dim.clamp(0.0, 1.0) * t * 255.0) as u8);
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
                    painter.rect_filled(
                        pane,
                        0.0,
                        egui::Color32::from_black_alpha((96.0 * t) as u8),
                    );
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

    // A ring wherever a click landed, growing and fading so the eye is
    // drawn to it after the fact rather than having to be watching.
    if live.iter().any(|e| matches!(e.tool, Tool::MarkClicks))
        && let Some((track, bounds)) = marks
    {
        for (click, age) in track.clicks_at(at, MARK) {
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
}

/// Where one point lands on screen, given which part of the picture is
/// shown. `None` once the zoom has pushed it out of view. A point has no
/// width, so it cannot go through `place`, which measures what is left of
/// a rectangle after clipping.
fn spot_on(rect: egui::Rect, shown: egui::Rect, at: egui::Pos2) -> Option<egui::Pos2> {
    let x = rect.left() + (at.x - shown.left()) / shown.width() * rect.width();
    let y = rect.top() + (at.y - shown.top()) / shown.height() * rect.height();
    let p = egui::pos2(x, y);
    rect.contains(p).then_some(p)
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

    fn zoom_on(area: Area, scale: f32) -> Effect {
        let mut edits = EditList::default();
        let id = edits.add(
            0.0,
            5.0,
            Tool::Zoom {
                area,
                follow: false,
                scale,
            },
        );
        edits.get(id).unwrap().clone()
    }

    /// Halfway through a span long enough to have a flat middle.
    const SETTLED: f64 = 2.5;

    #[test]
    fn with_no_effects_the_whole_picture_is_shown() {
        assert_eq!(
            lens_following(&[], SETTLED, None),
            egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0))
        );
    }

    #[test]
    fn closeness_decides_how_near_the_zoom_gets() {
        let area = Area::new(0.4, 0.4, 0.2, 0.2);
        let near = lens_following(&[&zoom_on(area, 3.0)], SETTLED, None);
        let far = lens_following(&[&zoom_on(area, 1.5)], SETTLED, None);
        assert!((near.width() - 1.0 / 3.0).abs() < 1e-5);
        assert!((far.width() - 1.0 / 1.5).abs() < 1e-5);
        assert!(
            near.width() < far.width(),
            "a higher closeness has to show less of the picture"
        );
    }

    #[test]
    fn the_zoom_is_centred_on_what_was_clicked() {
        let e = zoom_on(Area::new(0.3, 0.2, 0.2, 0.2), 2.0);
        let shown = lens_following(&[&e], SETTLED, None);
        assert!((shown.center().x - 0.4).abs() < 1e-5);
        assert!((shown.center().y - 0.3).abs() < 1e-5);
    }

    #[test]
    fn a_zoom_near_an_edge_is_pulled_back_onto_the_picture() {
        let e = zoom_on(Area::new(0.0, 0.0, 0.05, 0.05), 2.0);
        let shown = lens_following(&[&e], SETTLED, None);
        assert!(shown.left() >= -1e-6 && shown.top() >= -1e-6);
        assert!(shown.right() <= 1.0 + 1e-6 && shown.bottom() <= 1.0 + 1e-6);
        assert!(
            (shown.width() - 0.5).abs() < 1e-5,
            "and keeps its closeness"
        );
    }

    #[test]
    fn a_zoom_eases_in_and_out_rather_than_snapping() {
        let e = zoom_on(Area::new(0.4, 0.4, 0.2, 0.2), 2.0);
        let whole = 1.0;
        let at_start = lens_following(&[&e], 0.0, None).width();
        let early = lens_following(&[&e], 0.15, None).width();
        let settled = lens_following(&[&e], SETTLED, None).width();
        let at_end = lens_following(&[&e], 5.0, None).width();
        assert!(
            (at_start - whole).abs() < 1e-6,
            "it starts from the whole picture"
        );
        assert!(
            early < whole && early > settled,
            "part way in it is part way zoomed: {early}"
        );
        assert!(
            (settled - 0.5).abs() < 1e-5,
            "and reaches the full closeness"
        );
        assert!((at_end - whole).abs() < 1e-6, "then lets go again");
    }

    #[test]
    fn following_centres_on_the_pointer_rather_than_the_click() {
        let mut edits = EditList::default();
        let id = edits.add(
            0.0,
            5.0,
            Tool::Zoom {
                area: Area::new(0.1, 0.1, 0.1, 0.1),
                follow: true,
                scale: 2.0,
            },
        );
        let e = edits.get(id).unwrap().clone();
        let at_pointer = egui::pos2(0.8, 0.6);
        let shown = lens_following(&[&e], SETTLED, Some(at_pointer));
        // Pulled back inside the picture, so it cannot centre exactly.
        assert!((shown.center().x - 0.75).abs() < 1e-5);
        assert!((shown.center().y - 0.6).abs() < 1e-5);
    }

    #[test]
    fn without_a_path_following_holds_where_it_was_placed() {
        let mut edits = EditList::default();
        let id = edits.add(
            0.0,
            5.0,
            Tool::Zoom {
                area: Area::new(0.3, 0.3, 0.2, 0.2),
                follow: true,
                scale: 2.0,
            },
        );
        let e = edits.get(id).unwrap().clone();
        let shown = lens_following(&[&e], SETTLED, None);
        assert!((shown.center().x - 0.4).abs() < 1e-5);
    }

    #[test]
    fn a_pointer_on_another_screen_is_not_placed_on_this_one() {
        let left = screenilux_core::Rect::new(0, 0, 1920, 1080);
        assert!(on_screen(100, 100, left).is_some());
        assert!(
            on_screen(2500, 100, left).is_none(),
            "that is the other monitor"
        );
    }

    #[test]
    fn the_ramp_leaves_and_arrives_at_rest() {
        assert_eq!(ramp(0.0, 0.0, 4.0), 0.0);
        assert_eq!(ramp(4.0, 0.0, 4.0), 0.0);
        assert_eq!(ramp(2.0, 0.0, 4.0), 1.0);
        // Monotonic on the way up, so nothing jitters.
        let mut last = 0.0;
        for i in 0..=20 {
            let now = ramp(f64::from(i) * 0.02, 0.0, 4.0);
            assert!(now >= last, "the ramp went backwards at {i}");
            last = now;
        }
    }

    #[test]
    fn an_effect_too_short_to_ramp_still_behaves() {
        let mut edits = EditList::default();
        let id = edits.add(1.0, 1.1, Tool::Mute);
        let e = edits.get(id).unwrap();
        let peak = ramp(1.05, e.start, e.end);
        assert!((0.0..=1.0).contains(&peak), "weight out of range: {peak}");
        assert_eq!(ramp(1.0, e.start, e.end), 0.0);
        assert_eq!(ramp(1.1, e.start, e.end), 0.0);
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
