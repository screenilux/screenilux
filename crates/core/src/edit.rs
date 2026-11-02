//! Edits to a recording, kept as data rather than baked into pixels.
//!
//! A demo is edited by placing *moments*: a zoom that follows the pointer, a
//! region to hide, a dull stretch to cut or speed up. Each one names a span of
//! the original recording and what to do with it. Nothing is rendered here —
//! the player draws them while playing and export renders them once — so an
//! edit stays reversible for as long as the recording exists.
//!
//! The only hard part is time. Cutting and speeding up mean the recording has
//! two clocks: *source* time, which is where a frame sits in the files on
//! disk, and *output* time, which is where the viewer sees it. Everything that
//! shows an edit to the user speaks output time; everything that reads a frame
//! speaks source time. [`EditList::to_output`] and [`EditList::to_source`]
//! convert, and are the reason this module is tested as carefully as it is.

use serde::{Deserialize, Serialize};

use crate::geometry::Rect;

/// A region of one screen, as a fraction of its width and height.
///
/// Stored proportionally rather than in pixels so an edit survives the
/// recording being scaled, and so the same list applies to a screen whatever
/// its resolution.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Area {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl Area {
    pub fn new(x: f32, y: f32, w: f32, h: f32) -> Self {
        Self {
            x: x.clamp(0.0, 1.0),
            y: y.clamp(0.0, 1.0),
            w: w.clamp(0.0, 1.0),
            h: h.clamp(0.0, 1.0),
        }
    }

    pub fn centre(&self) -> (f32, f32) {
        (self.x + self.w / 2.0, self.y + self.h / 2.0)
    }

    pub fn contains(&self, x: f32, y: f32) -> bool {
        x >= self.x && x <= self.x + self.w && y >= self.y && y <= self.y + self.h
    }

    /// In pixels on a screen of the given size.
    pub fn to_pixels(&self, width: u32, height: u32) -> Rect {
        Rect::new(
            (self.x * width as f32) as i32,
            (self.y * height as f32) as i32,
            (self.w * width as f32).max(1.0) as u32,
            (self.h * height as f32).max(1.0) as u32,
        )
    }
}

/// What a moment does.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "tool", rename_all = "kebab-case")]
pub enum Tool {
    /// Magnify part of a screen. `follow` tracks the pointer within it, which
    /// needs a pointer track in the recording.
    Zoom {
        area: Area,
        #[serde(default = "one_follow")]
        follow: bool,
        #[serde(default = "default_scale")]
        scale: f32,
    },
    /// Cover a region, for anything that should not have been on screen.
    Hide { area: Area },
    /// Point at a region and dim the rest.
    Callout { area: Area, text: String },
    /// Draw a ring wherever a click was recorded.
    MarkClicks,
    /// Remove this stretch.
    Cut,
    /// Remove this stretch from one end. Kept apart from `Cut` so the editor
    /// can show it as trimming rather than as a hole in the middle.
    Trim,
    /// Play this stretch faster.
    Speed {
        #[serde(default = "default_rate")]
        rate: f32,
    },
    /// Silence this stretch; the picture keeps playing.
    Mute,
    /// Show a particular screen, overriding the pointer-driven choice.
    Screen { file: String },
}

/// `4.0` reads as "4", `2.5` stays "2.5".
fn trim_zero(v: f32) -> String {
    if (v - v.round()).abs() < 1e-3 {
        format!("{}", v.round() as i64)
    } else {
        format!("{v}")
    }
}

fn one_follow() -> bool {
    true
}
fn default_scale() -> f32 {
    2.1
}
fn default_rate() -> f32 {
    4.0
}

impl Tool {
    /// A short name for the editor's list.
    pub fn label(&self) -> String {
        match self {
            Tool::Zoom { follow: true, .. } => "Zoom & follow".into(),
            Tool::Zoom { .. } => "Zoom, held".into(),
            Tool::Hide { .. } => "Hide region".into(),
            Tool::Callout { .. } => "Callout".into(),
            Tool::MarkClicks => "Mark every click".into(),
            Tool::Cut => "Cut".into(),
            Tool::Trim => "Trim".into(),
            Tool::Speed { rate } => format!("Speed up {}×", trim_zero(*rate)),
            Tool::Mute => "Mute".into(),
            Tool::Screen { .. } => "Show one screen".into(),
        }
    }

    /// Does this change how long the recording runs?
    pub fn alters_time(&self) -> bool {
        matches!(self, Tool::Cut | Tool::Trim | Tool::Speed { .. })
    }

    /// The region it applies to, if it has one.
    pub fn area(&self) -> Option<Area> {
        match self {
            Tool::Zoom { area, .. } | Tool::Hide { area } | Tool::Callout { area, .. } => {
                Some(*area)
            }
            _ => None,
        }
    }
}

/// One edit: a span of the original recording and what to do with it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Moment {
    pub id: u32,
    /// Seconds into the original recording.
    pub start: f64,
    pub end: f64,
    /// Which screen this happens on, by the file it was recorded to. A
    /// moment placed on the picture knows its screen; one dragged out of
    /// the timeline changes time rather than pixels, so it has none and
    /// applies to the recording as a whole.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub screen: Option<String>,
    #[serde(flatten)]
    pub tool: Tool,
}

impl Moment {
    pub fn new(id: u32, start: f64, end: f64, tool: Tool) -> Self {
        let (a, b) = if start <= end {
            (start, end)
        } else {
            (end, start)
        };
        Self {
            id,
            start: a.max(0.0),
            end: b.max(a + MIN_SPAN),
            screen: None,
            tool,
        }
    }

    pub fn span(&self) -> f64 {
        self.end - self.start
    }

    pub fn covers(&self, t: f64) -> bool {
        t >= self.start && t <= self.end
    }
}

/// The shortest an edit may be. Below this a moment is impossible to grab on
/// the timeline and rounds away in the time arithmetic.
pub const MIN_SPAN: f64 = 0.1;

/// Something wrong with an edit list, reported rather than silently fixed.
#[derive(Debug, Clone, PartialEq)]
pub enum Problem {
    /// Two stretches that both remove or retime the same footage.
    Overlap { a: u32, b: u32 },
    /// A moment that runs past the end of the recording.
    PastEnd { id: u32 },
    /// A cut or trim that would leave nothing at all.
    RemovesEverything,
}

impl std::fmt::Display for Problem {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Problem::Overlap { .. } => write!(f, "two edits change the same stretch of time"),
            Problem::PastEnd { .. } => write!(f, "an edit runs past the end of the recording"),
            Problem::RemovesEverything => write!(f, "these edits would remove the whole recording"),
        }
    }
}

/// Every edit on one recording.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct EditList {
    #[serde(default)]
    pub moments: Vec<Moment>,
    /// Highest id handed out, so ids stay unique across sessions.
    #[serde(default)]
    next_id: u32,
}

impl EditList {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn is_empty(&self) -> bool {
        self.moments.is_empty()
    }

    pub fn len(&self) -> usize {
        self.moments.len()
    }

    /// Add a moment and return its id. Kept in start order, which everything
    /// else in this module relies on.
    pub fn add(&mut self, start: f64, end: f64, tool: Tool) -> u32 {
        self.add_on(start, end, tool, None)
    }

    /// Add a moment that happens on one screen, named by its file. A
    /// spatial edit placed on a multi-screen recording belongs to the
    /// picture it was clicked on, not to whichever one it lands over.
    pub fn add_on(&mut self, start: f64, end: f64, tool: Tool, screen: Option<String>) -> u32 {
        self.next_id += 1;
        let id = self.next_id;
        let mut m = Moment::new(id, start, end, tool);
        m.screen = screen;
        self.moments.push(m);
        self.sort();
        id
    }

    pub fn remove(&mut self, id: u32) {
        self.moments.retain(|m| m.id != id);
    }

    pub fn get(&self, id: u32) -> Option<&Moment> {
        self.moments.iter().find(|m| m.id == id)
    }

    /// Mutable access that keeps the list sorted afterwards, since moving a
    /// moment is the common edit.
    pub fn update(&mut self, id: u32, f: impl FnOnce(&mut Moment)) {
        if let Some(m) = self.moments.iter_mut().find(|m| m.id == id) {
            f(m);
            if m.end < m.start + MIN_SPAN {
                m.end = m.start + MIN_SPAN;
            }
        }
        self.sort();
    }

    fn sort(&mut self) {
        self.moments.sort_by(|a, b| {
            a.start
                .partial_cmp(&b.start)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
    }

    /// Moments covering a source time, in list order.
    pub fn active_at(&self, t: f64) -> impl Iterator<Item = &Moment> {
        self.moments.iter().filter(move |m| m.covers(t))
    }

    /// Only the ones that change the clock, in start order.
    fn retimed(&self) -> impl Iterator<Item = &Moment> {
        self.moments.iter().filter(|m| m.tool.alters_time())
    }

    /// Where a source time lands in the finished video.
    ///
    /// `None` means the time was cut and the viewer never sees it.
    pub fn to_output(&self, source: f64) -> Option<f64> {
        let mut out = 0.0;
        let mut cursor = 0.0;
        for m in self.retimed() {
            if m.start > source {
                break;
            }
            // untouched footage between the last edit and this one
            out += (m.start - cursor).max(0.0);
            let reached = m.end.min(source);
            match &m.tool {
                Tool::Cut | Tool::Trim => {
                    if source < m.end {
                        return None;
                    }
                }
                Tool::Speed { rate } => {
                    out += (reached - m.start).max(0.0) / (*rate).max(0.01) as f64;
                }
                _ => {}
            }
            cursor = cursor.max(m.end);
        }
        out += (source - cursor).max(0.0);
        Some(out)
    }

    /// Where an output time came from in the original recording.
    ///
    /// The inverse of [`to_output`](Self::to_output). A time inside a cut has
    /// no output position at all, so this never returns one.
    pub fn to_source(&self, output: f64) -> f64 {
        let mut remaining = output.max(0.0);
        let mut cursor = 0.0;
        for m in self.retimed() {
            let plain = (m.start - cursor).max(0.0);
            if remaining < plain {
                return cursor + remaining;
            }
            remaining -= plain;
            cursor = cursor.max(m.start);
            match &m.tool {
                Tool::Cut | Tool::Trim => {}
                Tool::Speed { rate } => {
                    let r = (*rate).max(0.01) as f64;
                    let shown = m.span() / r;
                    if remaining < shown {
                        return m.start + remaining * r;
                    }
                    remaining -= shown;
                }
                _ => {}
            }
            cursor = cursor.max(m.end);
        }
        cursor + remaining
    }

    /// How long the finished video runs, given the original length.
    pub fn output_duration(&self, source_duration: f64) -> f64 {
        self.to_output(source_duration)
            .unwrap_or_else(|| {
                // the tail is cut: the output ends where that cut begins
                self.retimed()
                    .filter(|m| matches!(m.tool, Tool::Cut | Tool::Trim))
                    .filter(|m| m.covers(source_duration))
                    .map(|m| self.to_output(m.start).unwrap_or(0.0))
                    .fold(0.0_f64, f64::max)
            })
            .max(0.0)
    }

    /// Seconds removed altogether, for "48s → 31s" in the editor.
    pub fn time_saved(&self, source_duration: f64) -> f64 {
        (source_duration - self.output_duration(source_duration)).max(0.0)
    }

    /// Is this source time removed from the finished video?
    pub fn is_removed(&self, t: f64) -> bool {
        self.moments
            .iter()
            .any(|m| matches!(m.tool, Tool::Cut | Tool::Trim) && t >= m.start && t < m.end)
    }

    /// How fast a source time plays.
    pub fn rate_at(&self, t: f64) -> f32 {
        self.moments
            .iter()
            .find(|m| matches!(m.tool, Tool::Speed { .. }) && m.covers(t))
            .map(|m| match m.tool {
                Tool::Speed { rate } => rate.max(0.01),
                _ => 1.0,
            })
            .unwrap_or(1.0)
    }

    pub fn is_muted(&self, t: f64) -> bool {
        self.moments
            .iter()
            .any(|m| matches!(m.tool, Tool::Mute) && m.covers(t))
    }

    /// The screen forced on at this time, if any.
    pub fn forced_screen(&self, t: f64) -> Option<&str> {
        self.moments.iter().find_map(|m| match &m.tool {
            Tool::Screen { file } if m.covers(t) => Some(file.as_str()),
            _ => None,
        })
    }

    /// Everything wrong with the list, worst first. An empty result means it
    /// can be exported as it stands.
    pub fn problems(&self, source_duration: f64) -> Vec<Problem> {
        let mut found = Vec::new();
        let retimed: Vec<&Moment> = self.retimed().collect();
        for (i, a) in retimed.iter().enumerate() {
            for b in retimed.iter().skip(i + 1) {
                if a.end > b.start + f64::EPSILON && a.start < b.end {
                    found.push(Problem::Overlap { a: a.id, b: b.id });
                }
            }
        }
        for m in &self.moments {
            if m.start > source_duration + f64::EPSILON {
                found.push(Problem::PastEnd { id: m.id });
            }
        }
        if source_duration > 0.0 && self.output_duration(source_duration) < MIN_SPAN {
            found.push(Problem::RemovesEverything);
        }
        found
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_moment_placed_on_a_screen_remembers_which_one() {
        let mut edits = EditList::default();
        let zoom = Tool::Zoom {
            area: Area::new(0.1, 0.1, 0.2, 0.2),
            follow: false,
            scale: 2.0,
        };
        let on = edits.add_on(1.0, 3.0, zoom, Some("screen-1.mp4".into()));
        let anywhere = edits.add(4.0, 5.0, Tool::Mute);
        assert_eq!(
            edits.get(on).unwrap().screen.as_deref(),
            Some("screen-1.mp4")
        );
        assert_eq!(
            edits.get(anywhere).unwrap().screen,
            None,
            "a moment that changes time belongs to no one screen"
        );

        // The screen is only written when there is one, so an edit list
        // from before this existed reads back unchanged.
        let json = serde_json::to_string(edits.get(anywhere).unwrap()).unwrap();
        assert!(!json.contains("screen"), "{json}");
        let back: Moment =
            serde_json::from_str(r#"{"id":9,"start":0.0,"end":2.0,"tool":"mute"}"#).unwrap();
        assert_eq!(back.screen, None);
    }

    fn list(moments: &[(f64, f64, Tool)]) -> EditList {
        let mut l = EditList::new();
        for (a, b, t) in moments {
            l.add(*a, *b, t.clone());
        }
        l
    }

    #[test]
    fn an_empty_list_changes_nothing() {
        let l = EditList::new();
        assert_eq!(l.to_output(12.0), Some(12.0));
        assert_eq!(l.to_source(12.0), 12.0);
        assert_eq!(l.output_duration(48.0), 48.0);
        assert_eq!(l.time_saved(48.0), 0.0);
    }

    #[test]
    fn a_cut_shortens_what_follows_it() {
        let l = list(&[(10.0, 20.0, Tool::Cut)]);
        assert_eq!(l.to_output(5.0), Some(5.0), "before the cut is untouched");
        assert_eq!(l.to_output(15.0), None, "inside the cut is never shown");
        assert_eq!(
            l.to_output(30.0),
            Some(20.0),
            "after it, ten seconds earlier"
        );
        assert_eq!(l.output_duration(48.0), 38.0);
        assert_eq!(l.time_saved(48.0), 10.0);
    }

    #[test]
    fn speeding_up_compresses_rather_than_removes() {
        let l = list(&[(10.0, 20.0, Tool::Speed { rate: 4.0 })]);
        assert_eq!(l.to_output(10.0), Some(10.0));
        assert_eq!(l.to_output(15.0), Some(11.25), "half of it, at a quarter");
        assert_eq!(l.to_output(20.0), Some(12.5));
        assert_eq!(l.to_output(30.0), Some(22.5));
        assert_eq!(l.output_duration(48.0), 40.5);
    }

    #[test]
    fn output_and_source_are_inverses() {
        let l = list(&[
            (3.0, 6.0, Tool::Trim),
            (12.0, 18.0, Tool::Speed { rate: 3.0 }),
            (30.0, 34.0, Tool::Cut),
        ]);
        for step in 0..=480 {
            let source = step as f64 * 0.1;
            if let Some(out) = l.to_output(source) {
                let back = l.to_source(out);
                assert!(
                    (back - source).abs() < 1e-6,
                    "source {source} -> output {out} -> {back}"
                );
            }
        }
    }

    #[test]
    fn a_time_exactly_on_a_cut_is_inside_it() {
        let l = list(&[(10.0, 20.0, Tool::Cut)]);
        assert_eq!(
            l.to_output(10.0),
            None,
            "the first removed frame is removed"
        );
        assert_eq!(
            l.to_output(20.0),
            Some(10.0),
            "the first kept frame is kept"
        );
    }

    #[test]
    fn a_time_exactly_on_a_speed_up_keeps_its_place() {
        let l = list(&[(12.0, 18.0, Tool::Speed { rate: 3.0 })]);
        assert_eq!(l.to_output(12.0), Some(12.0));
        assert!((l.to_source(12.0) - 12.0).abs() < 1e-9);
    }

    #[test]
    fn to_source_lands_at_the_far_side_of_a_cut() {
        let l = list(&[(10.0, 20.0, Tool::Cut)]);
        // output 10.0 is the first frame after the cut
        assert!((l.to_source(10.0) - 20.0).abs() < 1e-9);
        assert!((l.to_source(12.0) - 22.0).abs() < 1e-9);
    }

    #[test]
    fn several_cuts_accumulate() {
        let l = list(&[(5.0, 10.0, Tool::Cut), (20.0, 23.0, Tool::Cut)]);
        assert_eq!(l.to_output(30.0), Some(22.0));
        assert_eq!(l.output_duration(40.0), 32.0);
    }

    #[test]
    fn a_trimmed_tail_ends_the_video_there() {
        let l = list(&[(40.0, 48.0, Tool::Trim)]);
        assert_eq!(l.output_duration(48.0), 40.0);
    }

    #[test]
    fn a_trimmed_opening_shifts_everything_back() {
        let l = list(&[(0.0, 3.4, Tool::Trim)]);
        assert_eq!(l.to_output(1.0), None);
        assert_eq!(l.to_output(10.0), Some(6.6));
        assert!((l.to_source(0.0) - 3.4).abs() < 1e-9);
    }

    #[test]
    fn spatial_edits_leave_the_clock_alone() {
        let l = list(&[
            (
                5.0,
                9.0,
                Tool::Hide {
                    area: Area::new(0.1, 0.1, 0.2, 0.1),
                },
            ),
            (
                6.0,
                8.0,
                Tool::Callout {
                    area: Area::new(0.4, 0.4, 0.2, 0.2),
                    text: "here".into(),
                },
            ),
            (0.0, 48.0, Tool::MarkClicks),
        ]);
        assert_eq!(l.to_output(20.0), Some(20.0));
        assert_eq!(l.output_duration(48.0), 48.0);
        assert!(l.problems(48.0).is_empty(), "overlap only matters for time");
    }

    #[test]
    fn overlapping_cuts_are_reported() {
        let l = list(&[
            (10.0, 20.0, Tool::Cut),
            (15.0, 25.0, Tool::Speed { rate: 2.0 }),
        ]);
        let found = l.problems(48.0);
        assert!(
            found.iter().any(|p| matches!(p, Problem::Overlap { .. })),
            "got {found:?}"
        );
    }

    #[test]
    fn touching_cuts_are_not_an_overlap() {
        let l = list(&[(10.0, 20.0, Tool::Cut), (20.0, 25.0, Tool::Cut)]);
        assert!(l.problems(48.0).is_empty());
    }

    #[test]
    fn cutting_everything_is_reported_rather_than_exported() {
        let l = list(&[(0.0, 48.0, Tool::Cut)]);
        assert!(l.problems(48.0).contains(&Problem::RemovesEverything));
    }

    #[test]
    fn rate_mute_and_screen_read_back_at_a_time() {
        let mut l = list(&[
            (10.0, 20.0, Tool::Speed { rate: 8.0 }),
            (12.0, 14.0, Tool::Mute),
            (
                30.0,
                35.0,
                Tool::Screen {
                    file: "screen2.mp4".into(),
                },
            ),
        ]);
        assert_eq!(l.rate_at(15.0), 8.0);
        assert_eq!(l.rate_at(25.0), 1.0);
        assert!(l.is_muted(13.0));
        assert!(!l.is_muted(18.0));
        assert_eq!(l.forced_screen(32.0), Some("screen2.mp4"));
        assert_eq!(l.forced_screen(40.0), None);
        l.remove(l.moments[0].id);
        assert_eq!(l.rate_at(15.0), 1.0);
    }

    #[test]
    fn ids_stay_unique_and_the_list_stays_in_order() {
        let mut l = EditList::new();
        let late = l.add(30.0, 32.0, Tool::Cut);
        let early = l.add(5.0, 6.0, Tool::Cut);
        assert_ne!(late, early);
        assert_eq!(l.moments[0].id, early, "sorted by start, not by insertion");
        l.update(early, |m| {
            m.start = 40.0;
            m.end = 41.0;
        });
        assert_eq!(l.moments[0].id, late, "moving one re-sorts the list");
    }

    #[test]
    fn a_moment_cannot_be_inverted_or_vanishingly_short() {
        let m = Moment::new(1, 9.0, 4.0, Tool::Cut);
        assert!(m.start < m.end, "reversed drag is normalised");
        let mut l = EditList::new();
        let id = l.add(5.0, 5.0, Tool::Mute);
        let floor = MIN_SPAN - 1e-9; // span() is float arithmetic
        assert!(l.get(id).unwrap().span() >= floor);
        l.update(id, |m| m.end = m.start);
        assert!(l.get(id).unwrap().span() >= floor);
    }

    #[test]
    fn areas_are_proportional_so_they_survive_a_resize() {
        let a = Area::new(0.25, 0.5, 0.25, 0.25);
        assert_eq!(a.to_pixels(1920, 1080), Rect::new(480, 540, 480, 270));
        assert_eq!(a.to_pixels(3840, 2160), Rect::new(960, 1080, 960, 540));
        assert!(a.contains(0.3, 0.6));
        assert!(!a.contains(0.9, 0.6));
    }

    #[test]
    fn an_edit_list_round_trips_through_json() {
        let l = list(&[
            (
                1.0,
                4.0,
                Tool::Zoom {
                    area: Area::new(0.3, 0.3, 0.3, 0.3),
                    follow: true,
                    scale: 2.4,
                },
            ),
            (
                5.0,
                9.0,
                Tool::Hide {
                    area: Area::new(0.1, 0.1, 0.2, 0.1),
                },
            ),
            (10.0, 14.0, Tool::Speed { rate: 4.0 }),
        ]);
        let text = serde_json::to_string(&l).unwrap();
        assert!(text.contains("\"tool\":\"zoom\""), "{text}");
        assert!(text.contains("\"tool\":\"hide\""));
        let back: EditList = serde_json::from_str(&text).unwrap();
        assert_eq!(back, l);
    }

    #[test]
    fn labels_read_as_the_editor_shows_them() {
        assert_eq!(Tool::Speed { rate: 4.0 }.label(), "Speed up 4×");
        assert_eq!(
            Tool::Zoom {
                area: Area::new(0.0, 0.0, 0.1, 0.1),
                follow: true,
                scale: 2.0
            }
            .label(),
            "Zoom & follow"
        );
        assert_eq!(
            Tool::Zoom {
                area: Area::new(0.0, 0.0, 0.1, 0.1),
                follow: false,
                scale: 2.0
            }
            .label(),
            "Zoom, held"
        );
    }
}
