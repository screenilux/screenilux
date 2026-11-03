//! Where the pointer was while a recording was made.
//!
//! The recorder has always known this — it reads the cursor every frame to
//! draw the pointer, and the input hook tells it about every click — but it
//! threw both away when the recording stopped. That left two effects
//! permanently unavailable: a zoom cannot follow something no one wrote
//! down, and clicks cannot be marked if nobody remembers them.
//!
//! So the track is kept beside the recording. It is sampled rather than
//! recorded per frame: the pointer is a smooth thing and ten readings a
//! second is enough to follow it, while a sample per frame at 60fps would
//! be six times the data for motion nobody can see.

use serde::{Deserialize, Serialize};

use crate::geometry::Point;

/// How often the pointer is written down, in readings a second. Enough to
/// follow a hand moving across a screen; little enough that a long
/// recording's track stays small.
pub const RATE: f64 = 10.0;

/// A reading: when, and where on the desktop.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Where {
    /// Seconds into the recording.
    pub t: f64,
    pub x: i32,
    pub y: i32,
}

impl Where {
    pub fn new(t: f64, at: Point) -> Self {
        Self {
            t: (t * 1000.0).round() / 1000.0,
            x: at.x,
            y: at.y,
        }
    }

    pub fn at(&self) -> Point {
        Point::new(self.x, self.y)
    }
}

/// The pointer's path through a recording, and what it clicked.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Track {
    /// Readings in time order.
    #[serde(default)]
    pub moves: Vec<Where>,
    /// Clicks in time order. Kept apart from the path because a click is a
    /// moment rather than a position, and marking one wants its own time.
    #[serde(default)]
    pub clicks: Vec<Where>,
}

impl Track {
    pub fn is_empty(&self) -> bool {
        self.moves.is_empty() && self.clicks.is_empty()
    }

    /// Where the pointer was at `t`, interpolated between the readings
    /// either side of it so following is smooth rather than stepped.
    ///
    /// Before the first reading or after the last it holds at that end,
    /// which is what the pointer was doing.
    pub fn at(&self, t: f64) -> Option<Point> {
        let first = self.moves.first()?;
        let last = self.moves.last()?;
        if t <= first.t {
            return Some(first.at());
        }
        if t >= last.t {
            return Some(last.at());
        }
        // The readings are in time order, so the one at or before `t` is
        // found by bisection rather than by walking the whole track.
        let i = self
            .moves
            .partition_point(|m| m.t <= t)
            .saturating_sub(1)
            .min(self.moves.len() - 2);
        let (a, b) = (self.moves[i], self.moves[i + 1]);
        let span = b.t - a.t;
        if span <= 0.0 {
            return Some(a.at());
        }
        let f = ((t - a.t) / span).clamp(0.0, 1.0);
        Some(Point::new(
            a.x + ((b.x - a.x) as f64 * f).round() as i32,
            a.y + ((b.y - a.y) as f64 * f).round() as i32,
        ))
    }

    /// Clicks within `window` seconds after `t`, which is how a mark is
    /// drawn: it appears when the click happens and fades over the window.
    pub fn clicks_at(&self, t: f64, window: f64) -> impl Iterator<Item = (&Where, f64)> {
        self.clicks.iter().filter_map(move |c| {
            let age = t - c.t;
            (age >= 0.0 && age <= window).then_some((c, age / window.max(1e-6)))
        })
    }
}

/// Collects readings while a recording runs.
///
/// Holds the last reading rather than every one: the pointer rests far more
/// than it moves, and a track of ten identical readings a second says
/// nothing a single one does not.
#[derive(Debug, Default)]
pub struct Watcher {
    track: Track,
    last_sample: Option<f64>,
    last_at: Option<Point>,
}

impl Watcher {
    pub fn new() -> Self {
        Self::default()
    }

    /// Offer the pointer's position at `t`. Kept only if enough time has
    /// passed since the last reading and it has actually moved.
    pub fn saw(&mut self, t: f64, at: Point) {
        let due = self
            .last_sample
            .is_none_or(|last| t - last >= 1.0 / RATE - 1e-9);
        if !due {
            return;
        }
        self.last_sample = Some(t);
        // A reading that repeats the one before it is dropped, but the
        // first reading after a rest is kept so the path does not jump
        // straight from where it stopped to where it next appears.
        if self.last_at == Some(at) && self.track.moves.len() > 1 {
            if let Some(end) = self.track.moves.last_mut() {
                end.t = (t * 1000.0).round() / 1000.0;
            }
            return;
        }
        self.last_at = Some(at);
        self.track.moves.push(Where::new(t, at));
    }

    pub fn clicked(&mut self, t: f64, at: Point) {
        self.track.clicks.push(Where::new(t, at));
    }

    /// The track, in time order.
    pub fn finish(mut self) -> Track {
        self.track
            .moves
            .sort_by(|a, b| a.t.partial_cmp(&b.t).unwrap_or(std::cmp::Ordering::Equal));
        self.track
            .clicks
            .sort_by(|a, b| a.t.partial_cmp(&b.t).unwrap_or(std::cmp::Ordering::Equal));
        self.track
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn track(points: &[(f64, i32, i32)]) -> Track {
        Track {
            moves: points.iter().map(|&(t, x, y)| Where { t, x, y }).collect(),
            clicks: Vec::new(),
        }
    }

    #[test]
    fn an_empty_track_knows_nothing() {
        assert_eq!(Track::default().at(1.0), None);
        assert!(Track::default().is_empty());
    }

    #[test]
    fn a_position_between_readings_is_interpolated() {
        let t = track(&[(0.0, 0, 0), (1.0, 100, 200)]);
        assert_eq!(t.at(0.5), Some(Point::new(50, 100)));
        assert_eq!(t.at(0.25), Some(Point::new(25, 50)));
    }

    #[test]
    fn before_and_after_the_track_it_holds_still() {
        let t = track(&[(1.0, 10, 10), (2.0, 20, 20)]);
        assert_eq!(t.at(0.0), Some(Point::new(10, 10)));
        assert_eq!(t.at(9.0), Some(Point::new(20, 20)));
    }

    #[test]
    fn a_long_track_is_read_without_walking_it() {
        let moves: Vec<(f64, i32, i32)> = (0..2000).map(|i| (i as f64 * 0.1, i, i * 2)).collect();
        let t = track(&moves);
        assert_eq!(t.at(100.0), Some(Point::new(1000, 2000)));
        // Halfway between two readings, give or take a rounding step: the
        // times are built by repeated addition and do not land exactly.
        let mid = t.at(100.05).unwrap();
        assert!(
            (mid.x - 1000).abs() <= 1 && (mid.y - 2001).abs() <= 1,
            "{mid:?}"
        );
    }

    #[test]
    fn readings_are_taken_at_the_rate_not_per_frame() {
        let mut w = Watcher::new();
        // Sixty offers a second, as a 60fps recording would make.
        for i in 0..60 {
            w.saw(i as f64 / 60.0, Point::new(i, i));
        }
        let got = w.finish();
        assert!(
            got.moves.len() <= RATE as usize + 1,
            "kept {} readings for one second",
            got.moves.len()
        );
        assert!(got.moves.len() >= 9, "and did not drop the lot");
    }

    #[test]
    fn a_pointer_that_rests_does_not_fill_the_track() {
        let mut w = Watcher::new();
        for i in 0..100 {
            w.saw(i as f64 * 0.1, Point::new(7, 7));
        }
        let got = w.finish();
        assert!(
            got.moves.len() <= 2,
            "a still pointer wrote {} readings",
            got.moves.len()
        );
        // The last reading still reaches the end, so following holds there.
        assert!(got.moves.last().unwrap().t > 9.0);
    }

    #[test]
    fn clicks_are_kept_with_their_moment() {
        let mut w = Watcher::new();
        w.clicked(2.0, Point::new(5, 6));
        w.clicked(0.5, Point::new(1, 2));
        let got = w.finish();
        assert_eq!(got.clicks.len(), 2);
        assert!(
            got.clicks[0].t < got.clicks[1].t,
            "clicks come out in order"
        );
        let live: Vec<_> = got.clicks_at(2.1, 0.6).collect();
        assert_eq!(live.len(), 1, "only the one just gone");
        assert!(
            live[0].1 > 0.0 && live[0].1 < 1.0,
            "and it is part way through"
        );
        assert_eq!(
            got.clicks_at(1.0, 0.6).count(),
            1,
            "the earlier click is still fading"
        );
        assert_eq!(
            got.clicks_at(0.1, 0.6).count(),
            0,
            "nothing is marked before it is clicked"
        );
    }
}
