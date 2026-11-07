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
pub mod pointer;
mod region;
mod zoom;

use eframe::egui;
use screenilux_core::edit::{Effect, Tool};
use screenilux_core::geometry::Rect as Desktop;
use screenilux_core::pointer::Track;

use crate::theme;

pub use backdrop::backdrop;
pub use zoom::lens_following;

// Which effects apply to a screen, and how far through its span each one
// is, are worked out in core, so the export draws from the same numbers.
pub use screenilux_core::look::{for_screen, ramp};

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
