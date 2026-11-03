//! Zoom: which part of the picture is on screen.
//!
//! Alone among the effects this one is not painted. It decides the slice of
//! the texture everything else is drawn against, which is why magnifying
//! costs nothing per frame.

use eframe::egui;
use screenilux_core::edit::{Effect, Tool};

use super::ramp;

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

#[cfg(test)]
mod tests {
    use super::*;
    use screenilux_core::edit::{Area, EditList};

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
    fn with_no_effects_the_whole_picture_is_shown() {
        assert_eq!(
            lens_following(&[], SETTLED, None),
            egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0))
        );
    }
}
