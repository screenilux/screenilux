//! Which screen the pointer is on, over time.
//!
//! This is what lets a player show one screen at a time instead of a split
//! view: at any moment it knows which screen the user was actually working on.
//!
//! Deliberately a pure state machine with time passed in. The threading lives
//! in the recorder crate, so the debounce can be tested exactly, without
//! sleeping and without a pointer.

use crate::geometry::{Monitor, Point, monitor_at};

/// The pointer must settle this long on a screen before the view switches.
/// A pointer crossing one monitor to reach another should not count as a visit.
pub const DEFAULT_DWELL: f64 = 0.6;

/// One switch in the timeline: from `t` seconds, show screen `screen`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Switch {
    /// Recorded seconds, excluding paused time.
    pub t: f64,
    /// Index into the monitor list the tracker was built with.
    pub screen: usize,
}

#[derive(Debug, Clone)]
pub struct FocusTracker {
    monitors: Vec<Monitor>,
    dwell: f64,
    active: Option<usize>,
    candidate: Option<(usize, f64)>,
    timeline: Vec<Switch>,
}

impl FocusTracker {
    pub fn new(monitors: Vec<Monitor>, dwell: f64) -> Self {
        Self {
            monitors,
            dwell,
            active: None,
            candidate: None,
            timeline: Vec::new(),
        }
    }

    pub fn dwell(&self) -> f64 {
        self.dwell
    }

    pub fn active(&self) -> Option<usize> {
        self.active
    }

    pub fn timeline(&self) -> &[Switch] {
        &self.timeline
    }

    /// Feed one pointer sample.
    ///
    /// `recorded` is elapsed recording time in seconds, which must not advance
    /// while paused. `recording` is false while paused — pointer movement then
    /// is ignored entirely, so parking the mouse elsewhere during a pause does
    /// not rewrite the timeline.
    pub fn sample(&mut self, pointer: Point, recorded: f64, recording: bool) {
        let here = monitor_at(&self.monitors, pointer);

        if self.active.is_none() {
            // Seed on the first sample: whichever screen holds the pointer,
            // else the primary, so the timeline always opens with a screen.
            let start = here.or_else(|| self.monitors.iter().position(|m| m.is_primary));
            if let Some(idx) = start {
                self.active = Some(idx);
                self.timeline.push(Switch {
                    t: 0.0,
                    screen: idx,
                });
            }
            return;
        }

        if !recording {
            self.candidate = None;
            return;
        }

        let Some(here) = here else {
            // between screens, or off the desktop entirely
            self.candidate = None;
            return;
        };

        if Some(here) == self.active {
            self.candidate = None;
            return;
        }

        match self.candidate {
            Some((idx, since)) if idx == here => {
                if recorded - since >= self.dwell {
                    self.active = Some(here);
                    // Credit the switch to when the pointer *arrived*, not when
                    // the dwell expired, so playback cuts at the real moment
                    // instead of `dwell` seconds late.
                    self.timeline.push(Switch {
                        t: round3(since),
                        screen: here,
                    });
                    self.candidate = None;
                }
            }
            _ => self.candidate = Some((here, recorded)),
        }
    }
}

fn round3(v: f64) -> f64 {
    (v * 1000.0).round() / 1000.0
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geometry::Rect;

    fn screens() -> Vec<Monitor> {
        vec![
            Monitor::new("Screen 1", Rect::new(0, 0, 1920, 1080), true),
            Monitor::new("Screen 2", Rect::new(1920, 0, 1920, 1080), false),
        ]
    }

    const ON_1: Point = Point::new(500, 500);
    const ON_2: Point = Point::new(2500, 500);

    fn feed(t: &mut FocusTracker, p: Point, from: f64, to: f64, recording: bool) {
        // 80ms sampling, matching the recorder's poll interval
        let mut now = from;
        while now <= to {
            t.sample(p, now, recording);
            now += 0.08;
        }
    }

    #[test]
    fn timeline_opens_on_the_screen_under_the_pointer() {
        let mut t = FocusTracker::new(screens(), DEFAULT_DWELL);
        t.sample(ON_2, 0.0, true);
        assert_eq!(t.timeline(), &[Switch { t: 0.0, screen: 1 }]);
    }

    #[test]
    fn timeline_falls_back_to_primary_when_pointer_is_nowhere() {
        let mut t = FocusTracker::new(screens(), DEFAULT_DWELL);
        t.sample(Point::new(9999, 9999), 0.0, true);
        assert_eq!(t.timeline(), &[Switch { t: 0.0, screen: 0 }]);
    }

    #[test]
    fn a_settled_move_switches() {
        let mut t = FocusTracker::new(screens(), DEFAULT_DWELL);
        feed(&mut t, ON_1, 0.0, 1.0, true);
        feed(&mut t, ON_2, 1.1, 2.5, true);
        assert_eq!(t.timeline().len(), 2);
        assert_eq!(t.timeline()[1].screen, 1);
    }

    /// The behaviour the whole debounce exists for: without it a player
    /// flickers every time the pointer crosses a monitor.
    #[test]
    fn a_brief_flick_does_not_switch() {
        let mut t = FocusTracker::new(screens(), DEFAULT_DWELL);
        feed(&mut t, ON_1, 0.0, 1.0, true);
        feed(&mut t, ON_2, 1.1, 1.35, true); // 0.25s - well under the dwell
        feed(&mut t, ON_1, 1.4, 2.0, true);
        assert_eq!(
            t.timeline().len(),
            1,
            "a 0.25s excursion must leave no trace"
        );
        assert_eq!(t.active(), Some(0));
    }

    #[test]
    fn a_switch_is_credited_to_arrival_not_to_dwell_expiry() {
        let mut t = FocusTracker::new(screens(), DEFAULT_DWELL);
        feed(&mut t, ON_1, 0.0, 1.0, true);
        feed(&mut t, ON_2, 1.5, 3.0, true);
        let switch = t.timeline()[1];
        assert!(
            (switch.t - 1.5).abs() < 0.1,
            "expected ~1.5s (arrival), got {} - a dwell-expiry stamp would be ~2.1",
            switch.t
        );
    }

    #[test]
    fn movement_while_paused_is_ignored() {
        let mut t = FocusTracker::new(screens(), DEFAULT_DWELL);
        feed(&mut t, ON_1, 0.0, 1.0, true);
        // paused: the clock is frozen at 1.0 and the pointer parks elsewhere
        for _ in 0..40 {
            t.sample(ON_2, 1.0, false);
        }
        assert_eq!(
            t.timeline().len(),
            1,
            "a pause must not rewrite the timeline"
        );
        assert_eq!(t.active(), Some(0));
    }

    #[test]
    fn resuming_after_a_pause_still_tracks() {
        let mut t = FocusTracker::new(screens(), DEFAULT_DWELL);
        feed(&mut t, ON_1, 0.0, 1.0, true);
        for _ in 0..10 {
            t.sample(ON_2, 1.0, false);
        }
        feed(&mut t, ON_2, 1.0, 2.5, true);
        assert_eq!(t.timeline().len(), 2);
        assert_eq!(t.timeline()[1].screen, 1);
    }

    #[test]
    fn the_gap_between_offset_screens_switches_nothing() {
        let offset = vec![
            Monitor::new("Screen 1", Rect::new(0, 0, 1920, 1080), true),
            Monitor::new("Screen 2", Rect::new(1920, 122, 1920, 1080), false),
        ];
        let mut t = FocusTracker::new(offset, DEFAULT_DWELL);
        feed(&mut t, ON_1, 0.0, 1.0, true);
        feed(&mut t, Point::new(2500, 50), 1.1, 3.0, true); // in the dead band
        assert_eq!(t.timeline().len(), 1);
    }

    #[test]
    fn repeated_crossings_each_need_their_own_dwell() {
        let mut t = FocusTracker::new(screens(), DEFAULT_DWELL);
        feed(&mut t, ON_1, 0.0, 0.5, true);
        feed(&mut t, ON_2, 0.6, 1.4, true); // settles
        feed(&mut t, ON_1, 1.5, 2.3, true); // settles
        feed(&mut t, ON_2, 2.4, 2.6, true); // too brief
        let screens: Vec<_> = t.timeline().iter().map(|s| s.screen).collect();
        assert_eq!(screens, vec![0, 1, 0]);
    }
}
