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
//!
//! Each effect is its own module, small enough to read in one go, and this
//! one does three things only: it says what is live, it weighs how far
//! through its own span each one is, and it calls them in the order they
//! have to be painted — the backdrop first because it moves the picture,
//! then the regions, then anything laid over the whole of it.

mod backdrop;
mod blur;
mod caption;
mod geometry;
mod marks;
mod region;
mod zoom;

use eframe::egui;
use screenilux_core::edit::{Effect, Tool};
use screenilux_core::geometry::Rect as Desktop;
use screenilux_core::pointer::Track;

use crate::theme;

pub use backdrop::backdrop;
pub use zoom::lens_following;

/// How long an effect takes to come on, and to let go again. Short enough
/// not to be waited through, long enough that the eye follows the picture
/// in rather than being dropped into it.
const RAMP: f64 = 0.45;

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

/// Paint everything that goes over the picture, in painting order.
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
        let Some(on) = geometry::place(rect, shown, area) else {
            continue;
        };
        match &effect.tool {
            Tool::Hide { .. } => region::hide(painter, p, on),
            Tool::Blur { strength, .. } => match texture {
                Some(tex) => blur::pixelate(painter, tex, on, shown, area, *strength),
                None => region::hide(painter, p, on),
            },
            Tool::Spotlight { dim, .. } => region::spotlight(painter, p, rect, on, *dim, t),
            Tool::Callout { text, .. } => region::callout(painter, p, rect, on, text, t),
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
        caption::caption(painter, p, rect, text, *place);
    }

    if live.iter().any(|e| matches!(e.tool, Tool::MarkClicks))
        && let Some((track, bounds)) = marks
    {
        marks::clicks(painter, p, rect, shown, track, bounds, at);
    }
}

/// Where the pointer is on one screen now, as a fraction of it, if the
/// recording wrote its path down.
pub fn pointer_on(track: Option<&Track>, bounds: Desktop, at: f64) -> Option<egui::Pos2> {
    geometry::on_screen(track?.at(at)?.x, track?.at(at)?.y, bounds)
}

#[cfg(test)]
mod tests {
    use super::*;
    use screenilux_core::edit::EditList;

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
}
