//! Rectangles and monitors in virtual-desktop coordinates.
//!
//! Windows places every monitor on one signed coordinate plane, so a screen
//! left of the primary has a negative `left` and a screen mounted higher has a
//! negative `top`. Every position in this crate — pointer samples, the camera
//! bubble, monitor bounds — lives on that plane unless explicitly described as
//! "frame" coordinates, which are relative to one screen's own top-left.

use serde::{Deserialize, Serialize};

/// A point on the virtual desktop.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Point {
    pub x: i32,
    pub y: i32,
}

impl Point {
    pub const fn new(x: i32, y: i32) -> Self {
        Self { x, y }
    }
}

/// A rectangle on the virtual desktop. `left`/`top` may be negative.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Rect {
    pub left: i32,
    pub top: i32,
    pub width: u32,
    pub height: u32,
}

impl Rect {
    pub const fn new(left: i32, top: i32, width: u32, height: u32) -> Self {
        Self {
            left,
            top,
            width,
            height,
        }
    }

    pub const fn right(&self) -> i32 {
        self.left + self.width as i32
    }

    pub const fn bottom(&self) -> i32 {
        self.top + self.height as i32
    }

    /// Half-open containment: the right and bottom edges belong to the
    /// neighbouring screen, so adjacent monitors never both claim a pointer.
    pub const fn contains(&self, p: Point) -> bool {
        p.x >= self.left && p.x < self.right() && p.y >= self.top && p.y < self.bottom()
    }

    /// Translate a virtual-desktop point into this rect's own frame.
    pub const fn to_frame(&self, p: Point) -> Point {
        Point::new(p.x - self.left, p.y - self.top)
    }

    /// Does this rect overlap `other` at all?
    pub const fn intersects(&self, other: &Rect) -> bool {
        self.left < other.right()
            && other.left < self.right()
            && self.top < other.bottom()
            && other.top < self.bottom()
    }

    /// The smallest rect covering both. Used for the whole-desktop target.
    pub fn union(&self, other: &Rect) -> Rect {
        let left = self.left.min(other.left);
        let top = self.top.min(other.top);
        let right = self.right().max(other.right());
        let bottom = self.bottom().max(other.bottom());
        Rect::new(left, top, (right - left) as u32, (bottom - top) as u32)
    }
}

/// A display we can record.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Monitor {
    /// Human label, e.g. "Screen 1". Stable within one enumeration.
    pub label: String,
    pub bounds: Rect,
    pub is_primary: bool,
}

impl Monitor {
    pub fn new(label: impl Into<String>, bounds: Rect, is_primary: bool) -> Self {
        Self {
            label: label.into(),
            bounds,
            is_primary,
        }
    }

    pub const fn contains(&self, p: Point) -> bool {
        self.bounds.contains(p)
    }
}

/// Index of the monitor containing `p`, if any.
pub fn monitor_at(monitors: &[Monitor], p: Point) -> Option<usize> {
    monitors.iter().position(|m| m.contains(p))
}

/// The bounding box of every monitor — the "whole desktop" capture target.
/// Returns `None` for an empty list.
pub fn virtual_bounds(monitors: &[Monitor]) -> Option<Rect> {
    monitors
        .iter()
        .map(|m| m.bounds)
        .reduce(|acc, r| acc.union(&r))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Mirrors the real layout this was developed against: a primary at the
    /// origin and a second screen to its right, mounted 122px lower.
    fn two_screens() -> Vec<Monitor> {
        vec![
            Monitor::new("Screen 1", Rect::new(1920, 122, 1920, 1080), false),
            Monitor::new("Screen 2", Rect::new(0, 0, 1920, 1080), true),
        ]
    }

    #[test]
    fn contains_is_half_open_so_adjacent_screens_do_not_overlap() {
        let a = Rect::new(0, 0, 1920, 1080);
        let b = Rect::new(1920, 0, 1920, 1080);
        let seam = Point::new(1920, 500);
        assert!(!a.contains(seam), "right edge must belong to the neighbour");
        assert!(b.contains(seam));
        assert!(a.contains(Point::new(1919, 500)));
    }

    #[test]
    fn negative_origins_are_handled() {
        let left_screen = Rect::new(-1920, 0, 1920, 1080);
        assert!(left_screen.contains(Point::new(-1, 0)));
        assert!(left_screen.contains(Point::new(-1920, 0)));
        assert!(!left_screen.contains(Point::new(0, 0)));
        assert_eq!(
            left_screen.to_frame(Point::new(-1920, 10)),
            Point::new(0, 10)
        );
    }

    #[test]
    fn monitor_at_finds_the_right_screen() {
        let m = two_screens();
        assert_eq!(monitor_at(&m, Point::new(10, 10)), Some(1));
        assert_eq!(monitor_at(&m, Point::new(2000, 200)), Some(0));
        // the 122px vertical offset leaves a genuine gap above screen 1
        assert_eq!(monitor_at(&m, Point::new(2000, 50)), None);
    }

    #[test]
    fn virtual_bounds_covers_offset_screens() {
        let b = virtual_bounds(&two_screens()).unwrap();
        // matches the 3840x1202 the Python build reports for this layout
        assert_eq!(b, Rect::new(0, 0, 3840, 1202));
    }

    #[test]
    fn virtual_bounds_of_nothing_is_none() {
        assert!(virtual_bounds(&[]).is_none());
    }

    #[test]
    fn intersects_detects_a_straddling_bubble() {
        let screen = Rect::new(0, 0, 1920, 1080);
        let straddling = Rect::new(1800, 900, 320, 240);
        let elsewhere = Rect::new(2500, 300, 320, 240);
        assert!(screen.intersects(&straddling));
        assert!(!screen.intersects(&elsewhere));
    }
}
