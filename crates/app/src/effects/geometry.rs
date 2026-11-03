//! Turning positions on a recording into positions on screen.
//!
//! Every effect is placed in the recording's own terms — a fraction of a
//! screen, or a point on the desktop — and has to be drawn somewhere on a
//! picture that may be zoomed into part of it. That conversion is here, so
//! each effect can say where it belongs and let this work out where that
//! lands.

use eframe::egui;
use screenilux_core::edit::Area;
use screenilux_core::geometry::Rect as Desktop;

/// A desktop position as a fraction of one screen, or `None` when it is on
/// another one.
pub(super) fn on_screen(x: i32, y: i32, b: Desktop) -> Option<egui::Pos2> {
    if b.width == 0 || b.height == 0 {
        return None;
    }
    let fx = (x - b.left) as f32 / b.width as f32;
    let fy = (y - b.top) as f32 / b.height as f32;
    ((0.0..=1.0).contains(&fx) && (0.0..=1.0).contains(&fy)).then_some(egui::pos2(fx, fy))
}

/// Where one point lands on screen, given which part of the picture is
/// shown. `None` once the zoom has pushed it out of view. A point has no
/// width, so it cannot go through `place`, which measures what is left of
/// a rectangle after clipping.
pub(super) fn spot_on(rect: egui::Rect, shown: egui::Rect, at: egui::Pos2) -> Option<egui::Pos2> {
    let x = rect.left() + (at.x - shown.left()) / shown.width() * rect.width();
    let y = rect.top() + (at.y - shown.top()) / shown.height() * rect.height();
    let p = egui::pos2(x, y);
    rect.contains(p).then_some(p)
}

/// Where an area lands on screen, given which part of the picture is shown.
/// `None` when the zoom has pushed it out of view.
pub(super) fn place(rect: egui::Rect, shown: egui::Rect, area: Area) -> Option<egui::Rect> {
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
pub(super) fn around(outer: egui::Rect, hole: egui::Rect) -> [egui::Rect; 4] {
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

#[cfg(test)]
mod tests {
    use super::*;

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
    #[test]
    fn a_pointer_on_another_screen_is_not_placed_on_this_one() {
        let left = screenilux_core::Rect::new(0, 0, 1920, 1080);
        assert!(on_screen(100, 100, left).is_some());
        assert!(
            on_screen(2500, 100, left).is_none(),
            "that is the other monitor"
        );
    }
}
