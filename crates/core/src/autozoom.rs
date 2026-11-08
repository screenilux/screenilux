//! Zooms made from clicks: the editor's "Zoom in on clicks".
//!
//! A demo's clicks are where the viewer should be looking, which is what a
//! zoom is for. Placing one by hand per click is the slowest part of
//! editing, so this places them from the clicks the recording kept: clicks
//! close together in time and place become one zoom that starts a little
//! before the first and lets go a little after the last.
//!
//! Each is an ordinary Zoom effect that follows the pointer, so one in the
//! wrong place is moved or deleted like any other.

use crate::edit::{Area, Effect, Tool};
use crate::geometry::Rect;
use crate::pointer::Track;

/// How long before the first click a zoom starts moving in, so it has
/// arrived by the time the click lands.
const LEAD: f64 = 0.9;
/// How long after the last click it holds before letting go.
const HOLD: f64 = 1.8;
/// Clicks further apart than this in time start a new zoom.
const GAP: f64 = 3.0;
/// Clicks further apart than this on screen, as a fraction of it, start a
/// new zoom: the view would have to swing too far to keep both in.
const REACH: f32 = 0.3;
/// How close a zoom made this way gets.
const SCALE: f32 = 1.8;
/// The shortest a zoom is made, so it is never in and out in a blink.
const SHORTEST: f64 = 2.4;

/// A zoom to add: on which screen, when, and around what.
#[derive(Debug, Clone, PartialEq)]
pub struct Planned {
    pub screen: String,
    pub start: f64,
    pub end: f64,
    pub area: Area,
}

impl Planned {
    pub fn tool(&self, follow: bool) -> Tool {
        Tool::Zoom {
            area: self.area,
            follow,
            scale: SCALE,
        }
    }
}

/// Zooms for the clicks in `track`, on `screens` (each a name and where it
/// was on the desktop), within a recording `duration` long. Clicks inside a
/// zoom already in `existing` on the same screen are left to it.
pub fn plan(
    track: &Track,
    screens: &[(String, Rect)],
    duration: f64,
    existing: &[Effect],
) -> Vec<Planned> {
    // Each click as a place on a screen.
    let clicks: Vec<(f64, usize, (f32, f32))> = track
        .clicks
        .iter()
        .filter_map(|c| {
            screens.iter().enumerate().find_map(|(i, (_, d))| {
                let fx = (c.x - d.left) as f32 / d.width.max(1) as f32;
                let fy = (c.y - d.top) as f32 / d.height.max(1) as f32;
                ((0.0..=1.0).contains(&fx) && (0.0..=1.0).contains(&fy)).then_some((
                    c.t,
                    i,
                    (fx, fy),
                ))
            })
        })
        .collect();

    // Group them: same screen, close in time, close on screen.
    let mut groups: Vec<(usize, Vec<Click>)> = Vec::new();
    for (t, screen, at) in clicks {
        let joins = groups.last().is_some_and(|(s, g)| {
            let (last_t, _) = *g.last().expect("a group has a click");
            let centre = centre(g);
            *s == screen
                && t - last_t <= GAP
                && (at.0 - centre.0).abs() <= REACH
                && (at.1 - centre.1).abs() <= REACH
        });
        if joins {
            groups.last_mut().expect("just checked").1.push((t, at));
        } else {
            groups.push((screen, vec![(t, at)]));
        }
    }

    let mut planned: Vec<Planned> = Vec::new();
    for (screen, group) in groups {
        let name = &screens[screen].0;
        let first = group.first().expect("a group has a click").0;
        let last = group.last().expect("a group has a click").0;
        let covered = existing.iter().any(|e| {
            matches!(e.tool, Tool::Zoom { .. })
                && e.screen.as_deref().is_none_or(|s| s == name)
                && e.start <= first
                && e.end >= last
        });
        if covered {
            continue;
        }
        let mut start = (first - LEAD).max(0.0);
        let mut end = (last + HOLD).min(duration);
        if end - start < SHORTEST {
            end = (start + SHORTEST).min(duration);
            start = (end - SHORTEST).max(0.0);
        }
        // Not into the one before: a zoom starts where the last let go.
        if let Some(prev) = planned.last()
            && prev.screen == *name
            && start < prev.end
        {
            start = prev.end;
        }
        if end - start < 0.8 {
            continue;
        }
        let (cx, cy) = centre(&group);
        planned.push(Planned {
            screen: name.clone(),
            start,
            end,
            area: Area::new(cx - 0.1, cy - 0.1, 0.2, 0.2),
        });
    }
    planned
}

/// A click: when, and where on its screen.
type Click = (f64, (f32, f32));

fn centre(group: &[Click]) -> (f32, f32) {
    let n = group.len().max(1) as f32;
    let (x, y) = group
        .iter()
        .fold((0.0, 0.0), |(x, y), (_, p)| (x + p.0, y + p.1));
    (x / n, y / n)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::edit::EditList;
    use crate::geometry::Point;
    use crate::pointer::Where;

    fn track(clicks: &[(f64, i32, i32)]) -> Track {
        Track {
            moves: Vec::new(),
            clicks: clicks
                .iter()
                .map(|&(t, x, y)| Where::new(t, Point::new(x, y)))
                .collect(),
        }
    }

    fn two_screens() -> Vec<(String, Rect)> {
        vec![
            ("screen1.mp4".into(), Rect::new(0, 0, 1920, 1080)),
            ("screen2.mp4".into(), Rect::new(1920, 0, 1920, 1080)),
        ]
    }

    #[test]
    fn clicks_close_together_become_one_zoom() {
        let t = track(&[(5.0, 960, 540), (6.0, 1000, 560), (7.5, 980, 530)]);
        let plan = plan(&t, &two_screens(), 60.0, &[]);
        assert_eq!(plan.len(), 1, "{plan:?}");
        assert_eq!(plan[0].screen, "screen1.mp4");
        assert!((plan[0].start - 4.1).abs() < 1e-6);
        assert!((plan[0].end - 9.3).abs() < 1e-6);
        let (cx, cy) = plan[0].area.centre();
        assert!((cx - 0.51).abs() < 0.02 && (cy - 0.5).abs() < 0.02);
    }

    #[test]
    fn far_apart_in_time_or_place_or_screen_are_separate() {
        let t = track(&[
            (5.0, 200, 200),
            (20.0, 200, 200),  // much later
            (21.0, 1700, 900), // across the screen
            (22.0, 2500, 500), // the other screen
        ]);
        let plan = plan(&t, &two_screens(), 60.0, &[]);
        assert_eq!(plan.len(), 4, "{plan:?}");
        assert_eq!(plan[3].screen, "screen2.mp4");
        // Consecutive zooms on one screen never overlap.
        for w in plan.windows(2) {
            if w[0].screen == w[1].screen {
                assert!(w[1].start >= w[0].end - 1e-9);
            }
        }
    }

    #[test]
    fn a_click_already_zoomed_is_left_alone() {
        let mut edits = EditList::default();
        edits.add_on(
            0.0,
            10.0,
            Tool::Zoom {
                area: Area::new(0.4, 0.4, 0.2, 0.2),
                follow: true,
                scale: 2.0,
            },
            Some("screen1.mp4".into()),
        );
        let t = track(&[(5.0, 960, 540)]);
        assert!(plan(&t, &two_screens(), 60.0, &edits.effects).is_empty());
    }

    #[test]
    fn a_zoom_stays_inside_the_recording() {
        let t = track(&[(0.2, 960, 540), (59.5, 960, 540)]);
        let plan = plan(&t, &two_screens(), 60.0, &[]);
        assert!(plan.iter().all(|p| p.start >= 0.0 && p.end <= 60.0));
        assert!(plan.iter().all(|p| p.end - p.start >= SHORTEST - 1e-9));
    }
}
