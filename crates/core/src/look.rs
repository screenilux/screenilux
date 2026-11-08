//! How an edit list shapes what is seen at a moment: which effects apply to a
//! screen, how far through its own span each is, and which part of the
//! picture a zoom shows.
//!
//! The app's preview and the export both draw from this, so the picture in
//! the editor and the one in the finished video come from the same numbers.
//! Positions are fractions of a screen, 0 to 1 on each axis.

use crate::edit::{Area, Effect, Tool};

/// How long an effect takes to come on, and to let go again. Short enough
/// not to be waited through, long enough that the eye follows the picture
/// in rather than being dropped into it.
pub const RAMP: f64 = 0.45;

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

/// The whole picture.
pub const WHOLE: Area = Area {
    x: 0.0,
    y: 0.0,
    w: 1.0,
    h: 1.0,
};

/// The part of the picture a zoom shows at `at`, able to follow the pointer
/// when `pointer` says where it is on this screen.
pub fn lens(live: &[&Effect], at: f64, pointer: Option<(f32, f32)>) -> Area {
    let Some((area, scale, follow, start, end)) = live.iter().find_map(|e| match &e.tool {
        Tool::Zoom {
            area,
            scale,
            follow,
        } => Some((*area, *scale, *follow, e.start, e.end)),
        _ => None,
    }) else {
        return WHOLE;
    };
    let t = ramp(at, start, end);
    if t <= 0.0 {
        return WHOLE;
    }
    // Closeness is how many times nearer, so the slice shown is its
    // reciprocal. The same fraction on both axes keeps the picture's shape.
    let side = (1.0 / scale.max(1.01)).clamp(0.08, 1.0);
    let half = side / 2.0;
    // Following keeps the pointer in the middle of the view. Without a path
    // to follow it holds on what was clicked, which is all a recording made
    // before the pointer was written down can do. Either way the view is
    // pulled back inside the picture, so a zoom near an edge shows picture
    // rather than nothing.
    let (cx, cy) = match (follow, pointer) {
        (true, Some(p)) => p,
        _ => area.centre(),
    };
    let cx = cx.clamp(half, 1.0 - half);
    let cy = cy.clamp(half, 1.0 - half);
    let lerp = |whole: f32, near: f32| whole + (near - whole) * t;
    let x0 = lerp(0.0, cx - half);
    let y0 = lerp(0.0, cy - half);
    let x1 = lerp(1.0, cx + half);
    let y1 = lerp(1.0, cy + half);
    Area {
        x: x0,
        y: y0,
        w: x1 - x0,
        h: y1 - y0,
    }
}

/// Narrow `shown` to the shape `out_aspect` (width over height) once it is on
/// a screen `screen_aspect` wide, keeping `centre` in view where it can.
///
/// This is how a wide recording becomes a vertical or square video: the
/// slice of picture that fits the new shape, following the action rather
/// than always taking the middle.
pub fn fit(shown: Area, screen_aspect: f32, out_aspect: f32, centre: (f32, f32)) -> Area {
    let have = shown.w * screen_aspect / shown.h.max(1e-6);
    if (have - out_aspect).abs() < 1e-3 {
        return shown;
    }
    if have > out_aspect {
        // Too wide: keep the height, take a narrower slice.
        let w = shown.h * out_aspect / screen_aspect;
        let x = (centre.0 - w / 2.0).clamp(shown.x, shown.x + shown.w - w);
        Area {
            x,
            y: shown.y,
            w,
            h: shown.h,
        }
    } else {
        // Too tall: keep the width, take a shorter slice.
        let h = shown.w * screen_aspect / out_aspect;
        let y = (centre.1 - h / 2.0).clamp(shown.y, shown.y + shown.h - h);
        Area {
            x: shown.x,
            y,
            w: shown.w,
            h,
        }
    }
}

/// Where `area`, a part of the screen, lands once only `shown` of the
/// screen fills the view: as a fraction of the view. `None` when it is out
/// of view altogether.
pub fn place(area: Area, shown: Area) -> Option<Area> {
    let x0 = ((area.x - shown.x) / shown.w).max(0.0);
    let y0 = ((area.y - shown.y) / shown.h).max(0.0);
    let x1 = ((area.x + area.w - shown.x) / shown.w).min(1.0);
    let y1 = ((area.y + area.h - shown.y) / shown.h).min(1.0);
    (x1 > x0 && y1 > y0).then_some(Area {
        x: x0,
        y: y0,
        w: x1 - x0,
        h: y1 - y0,
    })
}

/// Where a point on the screen lands in the view, if it is in view.
pub fn spot(point: (f32, f32), shown: Area) -> Option<(f32, f32)> {
    let x = (point.0 - shown.x) / shown.w;
    let y = (point.1 - shown.y) / shown.h;
    ((0.0..=1.0).contains(&x) && (0.0..=1.0).contains(&y)).then_some((x, y))
}

/// How long the pointer rests before it starts to fade, and how long the
/// fade takes.
const IDLE_AFTER: f64 = 2.0;
const IDLE_FADE: f64 = 0.5;
/// How long a shortcut stays up.
const SHORTCUT_LIFE: f64 = 1.4;

/// Where to draw the pointer at `t` on the desktop, and how visible, for a
/// recording that left it out of the picture. `None` when it is hidden.
pub fn cursor_at(
    track: &crate::pointer::Track,
    t: f64,
    style: &crate::edit::PointerStyle,
) -> Option<((f64, f64), f32)> {
    if style.hidden {
        return None;
    }
    let at = if style.smooth {
        track.smooth_at(t, 0.12)?
    } else {
        let p = track.at(t)?;
        (f64::from(p.x), f64::from(p.y))
    };
    let alpha = if style.hide_idle {
        (1.0 - (track.resting_for(t) - IDLE_AFTER) / IDLE_FADE).clamp(0.0, 1.0) as f32
    } else {
        1.0
    };
    (alpha > 0.0).then_some((at, alpha))
}

/// The shortcut to show at `t`, if one was pressed just before, and how far
/// in it has faded.
pub fn shortcut_at(shortcuts: &[crate::keys::Shortcut], t: f64) -> Option<(&str, f32)> {
    let k = shortcuts
        .iter()
        .rev()
        .find(|k| k.t <= t && t - k.t <= SHORTCUT_LIFE)?;
    let age = t - k.t;
    // In quickly, out gently.
    let fade = (age / 0.12)
        .min((SHORTCUT_LIFE - age) / 0.35)
        .clamp(0.0, 1.0) as f32;
    Some((k.keys.as_str(), fade))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::edit::EditList;

    fn zoom_on(area: Area, scale: f32, follow: bool) -> Effect {
        let mut edits = EditList::default();
        let id = edits.add(
            0.0,
            5.0,
            Tool::Zoom {
                area,
                follow,
                scale,
            },
        );
        edits.get(id).unwrap().clone()
    }

    /// Halfway through a span long enough to have a flat middle.
    const SETTLED: f64 = 2.5;

    #[test]
    fn the_ramp_leaves_and_arrives_at_rest() {
        assert_eq!(ramp(0.0, 0.0, 4.0), 0.0);
        assert_eq!(ramp(4.0, 0.0, 4.0), 0.0);
        assert_eq!(ramp(2.0, 0.0, 4.0), 1.0);
        let mut last = 0.0;
        for i in 0..=20 {
            let now = ramp(f64::from(i) * 0.02, 0.0, 4.0);
            assert!(now >= last, "the ramp went backwards at {i}");
            last = now;
        }
    }

    #[test]
    fn closeness_decides_how_near_the_zoom_gets() {
        let area = Area::new(0.4, 0.4, 0.2, 0.2);
        let near = lens(&[&zoom_on(area, 3.0, false)], SETTLED, None);
        let far = lens(&[&zoom_on(area, 1.5, false)], SETTLED, None);
        assert!((near.w - 1.0 / 3.0).abs() < 1e-5);
        assert!((far.w - 1.0 / 1.5).abs() < 1e-5);
    }

    #[test]
    fn a_zoom_is_centred_and_kept_on_the_picture() {
        let e = zoom_on(Area::new(0.3, 0.2, 0.2, 0.2), 2.0, false);
        let shown = lens(&[&e], SETTLED, None);
        assert!((shown.centre().0 - 0.4).abs() < 1e-5);
        let edge = zoom_on(Area::new(0.0, 0.0, 0.05, 0.05), 2.0, false);
        let shown = lens(&[&edge], SETTLED, None);
        assert!(shown.x >= -1e-6 && shown.y >= -1e-6);
        assert!((shown.w - 0.5).abs() < 1e-5);
    }

    #[test]
    fn following_centres_on_the_pointer() {
        let e = zoom_on(Area::new(0.1, 0.1, 0.1, 0.1), 2.0, true);
        let shown = lens(&[&e], SETTLED, Some((0.8, 0.6)));
        assert!((shown.centre().0 - 0.75).abs() < 1e-5);
        assert!((shown.centre().1 - 0.6).abs() < 1e-5);
    }

    #[test]
    fn a_zoom_eases_in_and_out() {
        let e = zoom_on(Area::new(0.4, 0.4, 0.2, 0.2), 2.0, false);
        assert!((lens(&[&e], 0.0, None).w - 1.0).abs() < 1e-6);
        let early = lens(&[&e], 0.15, None).w;
        assert!(early < 1.0 && early > 0.5);
        assert!((lens(&[&e], 5.0, None).w - 1.0).abs() < 1e-6);
    }

    #[test]
    fn a_wide_picture_fits_a_vertical_frame_around_the_action() {
        // A 16:9 screen into 9:16: a slice about a third of the width.
        let slice = fit(WHOLE, 16.0 / 9.0, 9.0 / 16.0, (0.8, 0.5));
        assert!((slice.h - 1.0).abs() < 1e-6);
        assert!((slice.w * 16.0 / 9.0 - 9.0 / 16.0).abs() < 1e-4);
        assert!(
            (slice.centre().0 - 0.8).abs() < 1e-4,
            "centred on the action"
        );
        // Never past the edge of the picture.
        let edge = fit(WHOLE, 16.0 / 9.0, 9.0 / 16.0, (1.0, 0.5));
        assert!((edge.x + edge.w - 1.0).abs() < 1e-6);
        // A square from a wide picture keeps the height.
        let square = fit(WHOLE, 16.0 / 9.0, 1.0, (0.5, 0.5));
        assert!((square.w * 16.0 / 9.0 - 1.0).abs() < 1e-4);
    }

    #[test]
    fn a_resting_pointer_fades_and_a_moving_one_shows() {
        use crate::geometry::Point;
        use crate::pointer::{Track, Where};
        let track = Track {
            moves: vec![
                Where::new(0.0, Point::new(0, 0)),
                Where::new(1.0, Point::new(100, 0)),
            ],
            clicks: Vec::new(),
        };
        let style = crate::edit::PointerStyle::default();
        assert!(cursor_at(&track, 1.5, &style).is_some(), "just stopped");
        assert!(cursor_at(&track, 10.0, &style).is_none(), "long at rest");
        let hidden = crate::edit::PointerStyle {
            hidden: true,
            ..style
        };
        assert!(cursor_at(&track, 0.5, &hidden).is_none());
    }

    #[test]
    fn a_shortcut_shows_for_a_moment_after_it_is_pressed() {
        let list = vec![crate::keys::Shortcut {
            t: 3.0,
            keys: "Ctrl + S".into(),
        }];
        assert!(shortcut_at(&list, 2.9).is_none());
        assert_eq!(shortcut_at(&list, 3.5).map(|(k, _)| k), Some("Ctrl + S"));
        assert!(shortcut_at(&list, 6.0).is_none());
    }

    #[test]
    fn places_and_spots_follow_the_zoom() {
        let shown = Area::new(0.0, 0.0, 0.5, 0.5);
        let on = place(Area::new(0.0, 0.0, 0.5, 0.5), shown).unwrap();
        assert!((on.w - 1.0).abs() < 1e-6 && (on.h - 1.0).abs() < 1e-6);
        assert!(place(Area::new(0.6, 0.6, 0.2, 0.2), shown).is_none());
        assert_eq!(spot((0.25, 0.25), shown), Some((0.5, 0.5)));
        assert_eq!(spot((0.75, 0.25), shown), None);
    }
}
