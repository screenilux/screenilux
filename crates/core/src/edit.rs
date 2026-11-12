//! Edits to a recording, kept as data rather than baked into pixels.
//!
//! A demo is edited by placing *effects*: a zoom that follows the pointer, a
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
//!
//! A *cut* is the other way to take footage out. It is a moment rather than
//! a stretch: it splits the recording there, every screen and the camera at
//! once since they share one clock, and on its own changes nothing. What it
//! gives is parts, one either side, and removing a part lays an ordinary
//! [`Tool::Cut`] over it, so the gap closes up wherever the time mapping is
//! read: playing, exporting and sharing.

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

/// What a effect does.
/// Where a caption sits on the picture.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Place {
    /// Along the bottom, out of the way of what is being shown.
    #[default]
    Bottom,
    Top,
    /// Across the middle, for a title rather than a subtitle.
    Centre,
}

/// What a backdrop is filled with.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Backdrop {
    /// A soft gradient, which is what most demos use.
    #[default]
    Gradient,
    /// One flat colour.
    Solid,
    /// The recording itself, blown up and blurred behind.
    Blurred,
}

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
    /// Soften a region rather than covering it, so the shape of what was
    /// there still reads. The usual way a demo redacts a name or a key.
    Blur {
        area: Area,
        #[serde(default = "default_blur")]
        strength: f32,
    },
    /// Keep a region lit and dim everything around it. A callout without
    /// the label, for walking through one part of a crowded screen.
    Spotlight {
        area: Area,
        #[serde(default = "default_dim")]
        dim: f32,
    },
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
    /// Words over the picture for this stretch. A caption carries the
    /// narration a silent demo would otherwise need a voice for.
    Caption {
        text: String,
        #[serde(default)]
        place: Place,
    },
    /// Inset the picture on a background with rounded corners and a shadow.
    /// The look most demos are published in, and the reason a raw recording
    /// reads as a screenshot while an edited one reads as a product video.
    Backdrop {
        #[serde(default = "default_pad")]
        pad: f32,
        #[serde(default)]
        style: Backdrop,
    },
    /// Hold on one frame, so a point has time to land before moving on.
    /// The stretch keeps its length and the frame at its start is shown
    /// throughout, so the recording still runs for as long as it did and
    /// the time mapping is untouched.
    Freeze,
    /// Show a particular screen, overriding the pointer-driven choice.
    Screen { file: String },
}

impl Tool {
    /// A blur over `area` at the strength one placed by hand starts at.
    pub fn blur(area: Area) -> Self {
        Tool::Blur {
            area,
            strength: default_blur(),
        }
    }
}

/// How the pointer is drawn on playback and in an export.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct PointerStyle {
    /// Leave it out altogether.
    pub hidden: bool,
    /// 1 is an ordinary pointer.
    pub size: f32,
    /// Move along the eased path rather than every reading.
    pub smooth: bool,
    /// Fade it out while it rests.
    pub hide_idle: bool,
}

impl Default for PointerStyle {
    fn default() -> Self {
        Self {
            hidden: false,
            size: 1.5,
            smooth: true,
            hide_idle: true,
        }
    }
}

impl PointerStyle {
    pub fn is_default(&self) -> bool {
        *self == Self::default()
    }
}

/// `4.0` reads as "4", `2.5` stays "2.5".
/// A rate without a pointless trailing zero: 4 rather than 4.0.
pub fn trim_zero(v: f32) -> String {
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

fn default_blur() -> f32 {
    0.6
}

fn default_dim() -> f32 {
    0.55
}

fn default_pad() -> f32 {
    0.06
}

impl Tool {
    /// A short name for the editor's list.
    pub fn label(&self) -> String {
        match self {
            Tool::Zoom { follow: true, .. } => "Zoom & follow".into(),
            Tool::Zoom { .. } => "Zoom, held".into(),
            Tool::Hide { .. } => "Hide region".into(),
            Tool::Blur { .. } => "Blur region".into(),
            Tool::Spotlight { .. } => "Spotlight".into(),
            Tool::Callout { .. } => "Callout".into(),
            Tool::MarkClicks => "Mark every click".into(),
            Tool::Cut => "Cut out".into(),
            Tool::Trim => "Trim".into(),
            Tool::Speed { rate } => format!("Speed up {}×", trim_zero(*rate)),
            Tool::Mute => "Mute".into(),
            Tool::Caption { .. } => "Caption".into(),
            Tool::Backdrop { .. } => "Backdrop".into(),
            Tool::Freeze => "Hold the frame".into(),
            Tool::Screen { .. } => "Show one screen".into(),
        }
    }

    /// Does this change how long the recording runs?
    pub fn alters_time(&self) -> bool {
        matches!(self, Tool::Cut | Tool::Trim | Tool::Speed { .. })
    }

    /// Does this take its stretch out of the finished video?
    pub fn removes(&self) -> bool {
        matches!(self, Tool::Cut | Tool::Trim)
    }

    /// The region it applies to, if it has one.
    pub fn area(&self) -> Option<Area> {
        match self {
            Tool::Zoom { area, .. }
            | Tool::Hide { area }
            | Tool::Blur { area, .. }
            | Tool::Spotlight { area, .. }
            | Tool::Callout { area, .. } => Some(*area),
            _ => None,
        }
    }
}

/// One edit: a span of the original recording and what to do with it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Effect {
    pub id: u32,
    /// Seconds into the original recording.
    pub start: f64,
    pub end: f64,
    /// Which screen this happens on, by the file it was recorded to. A
    /// effect placed on the picture knows its screen; one dragged out of
    /// the timeline changes time rather than pixels, so it has none and
    /// applies to the recording as a whole.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub screen: Option<String>,
    #[serde(flatten)]
    pub tool: Tool,
}

impl Effect {
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

/// The shortest an edit may be. Below this a effect is impossible to grab on
/// the timeline and rounds away in the time arithmetic.
pub const MIN_SPAN: f64 = 0.1;

/// Something wrong with an edit list, reported rather than silently fixed.
#[derive(Debug, Clone, PartialEq)]
pub enum Problem {
    /// Two stretches that both remove or retime the same footage.
    Overlap { a: u32, b: u32 },
    /// A effect that runs past the end of the recording.
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
    pub effects: Vec<Effect>,
    /// How the pointer is drawn, for a recording that left it out of the
    /// picture.
    #[serde(default, skip_serializing_if = "PointerStyle::is_default")]
    pub pointer: PointerStyle,
    /// Leave the shortcuts pressed while recording off the picture.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub hide_shortcuts: bool,
    /// Leave the camera bubble off the picture, for a recording that has
    /// the camera as a video of its own.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub hide_camera: bool,
    /// Where the recording is cut, in seconds of the original and in order.
    /// Written only when there is one, so a list from before cuts existed
    /// reads back, and is written back, unchanged.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub cuts: Vec<f64>,
    /// Highest id handed out, so ids stay unique across sessions.
    #[serde(default)]
    next_id: u32,
}

/// The stretch between two cuts, or between a cut and an end of the
/// recording.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Part {
    pub start: f64,
    pub end: f64,
    /// Taken out of the finished video altogether.
    pub removed: bool,
}

impl Part {
    pub fn covers(&self, t: f64) -> bool {
        t >= self.start && t < self.end
    }
}

/// Two times this close are the same moment. Cuts and the edges of the
/// parts removed at them are copies of one number, so this only has to
/// absorb arithmetic, not judgement.
const SAME: f64 = 1e-9;

impl EditList {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn is_empty(&self) -> bool {
        self.effects.is_empty() && self.cuts.is_empty()
    }

    pub fn len(&self) -> usize {
        self.effects.len()
    }

    /// Add a effect and return its id. Kept in start order, which everything
    /// else in this module relies on.
    pub fn add(&mut self, start: f64, end: f64, tool: Tool) -> u32 {
        self.add_on(start, end, tool, None)
    }

    /// Add a effect that happens on one screen, named by its file. A
    /// spatial edit placed on a multi-screen recording belongs to the
    /// picture it was clicked on, not to whichever one it lands over.
    pub fn add_on(&mut self, start: f64, end: f64, tool: Tool, screen: Option<String>) -> u32 {
        self.next_id += 1;
        let id = self.next_id;
        let mut m = Effect::new(id, start, end, tool);
        m.screen = screen;
        self.effects.push(m);
        self.sort();
        id
    }

    pub fn remove(&mut self, id: u32) {
        self.effects.retain(|m| m.id != id);
    }

    pub fn get(&self, id: u32) -> Option<&Effect> {
        self.effects.iter().find(|m| m.id == id)
    }

    /// Mutable access that keeps the list sorted afterwards, since moving a
    /// effect is the common edit.
    pub fn update(&mut self, id: u32, f: impl FnOnce(&mut Effect)) {
        if let Some(m) = self.effects.iter_mut().find(|m| m.id == id) {
            f(m);
            if m.end < m.start + MIN_SPAN {
                m.end = m.start + MIN_SPAN;
            }
        }
        self.sort();
    }

    fn sort(&mut self) {
        self.effects.sort_by(|a, b| {
            a.start
                .partial_cmp(&b.start)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
    }

    /// Effects covering a source time, in list order.
    pub fn active_at(&self, t: f64) -> impl Iterator<Item = &Effect> {
        self.effects.iter().filter(move |m| m.covers(t))
    }

    /// Only the ones that change the clock, in start order.
    fn retimed(&self) -> impl Iterator<Item = &Effect> {
        self.effects.iter().filter(|m| m.tool.alters_time())
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
        self.effects
            .iter()
            .any(|m| matches!(m.tool, Tool::Cut | Tool::Trim) && t >= m.start && t < m.end)
    }

    /// How fast a source time plays.
    pub fn rate_at(&self, t: f64) -> f32 {
        self.effects
            .iter()
            .find(|m| matches!(m.tool, Tool::Speed { .. }) && m.covers(t))
            .map(|m| match m.tool {
                Tool::Speed { rate } => rate.max(0.01),
                _ => 1.0,
            })
            .unwrap_or(1.0)
    }

    pub fn is_muted(&self, t: f64) -> bool {
        self.effects
            .iter()
            .any(|m| matches!(m.tool, Tool::Mute) && m.covers(t))
    }

    /// The screen forced on at this time, if any.
    pub fn forced_screen(&self, t: f64) -> Option<&str> {
        self.effects.iter().find_map(|m| match &m.tool {
            Tool::Screen { file } if m.covers(t) => Some(file.as_str()),
            _ => None,
        })
    }

    /// Cut the recording at `t`. Every screen and the camera are cut there
    /// together, since they play to one clock.
    ///
    /// Refused, returning `false`, where a cut would make a part too short
    /// to see or grab (at either end, or beside another cut) and where the
    /// footage is already removed, since there is nothing there to split.
    pub fn cut_at(&mut self, t: f64, duration: f64) -> bool {
        if !self.can_cut(t, duration) {
            return false;
        }
        self.cuts.push(t);
        self.cuts.sort_by(f64::total_cmp);
        true
    }

    /// Would [`cut_at`](Self::cut_at) take a cut here?
    pub fn can_cut(&self, t: f64, duration: f64) -> bool {
        let fits = t >= MIN_SPAN && t <= duration - MIN_SPAN;
        let crowded = self.cuts.iter().any(|c| (c - t).abs() < MIN_SPAN);
        fits && !crowded && !self.is_removed(t)
    }

    /// Take the cut at `at` away, joining the parts either side. A part
    /// removed beside it stays removed: that is an edit of its own, listed
    /// and undone on its own.
    pub fn join(&mut self, at: f64) {
        self.cuts.retain(|c| (c - at).abs() > SAME);
    }

    /// Move the cut at `from` towards `to`, and return where it landed. It
    /// stays between its neighbours, and the edge of a part removed at it
    /// moves with it, so dragging a cut moves where the gap begins or ends
    /// rather than leaving a sliver of removed footage behind.
    pub fn move_cut(&mut self, from: f64, to: f64, duration: f64) -> f64 {
        let Some(i) = self.cuts.iter().position(|c| (c - from).abs() <= SAME) else {
            return from;
        };
        let mut lo = if i == 0 { 0.0 } else { self.cuts[i - 1] } + MIN_SPAN;
        let mut hi = self.cuts.get(i + 1).copied().unwrap_or(duration) - MIN_SPAN;
        for e in self.effects.iter().filter(|e| e.tool.removes()) {
            if (e.start - from).abs() <= SAME {
                hi = hi.min(e.end - MIN_SPAN);
            }
            if (e.end - from).abs() <= SAME {
                lo = lo.max(e.start + MIN_SPAN);
            }
        }
        if lo > hi {
            return from;
        }
        let to = to.clamp(lo, hi);
        self.cuts[i] = to;
        for e in self.effects.iter_mut().filter(|e| e.tool.removes()) {
            if (e.start - from).abs() <= SAME {
                e.start = to;
            }
            if (e.end - from).abs() <= SAME {
                e.end = to;
            }
        }
        self.sort();
        to
    }

    /// The recording split at its cuts, in order. With no cuts it is one
    /// part, the whole of it.
    pub fn parts(&self, duration: f64) -> Vec<Part> {
        let mut edges = vec![0.0];
        edges.extend(
            self.cuts
                .iter()
                .copied()
                .filter(|c| *c > 0.0 && *c < duration),
        );
        edges.push(duration);
        edges
            .windows(2)
            .filter(|w| w[1] > w[0])
            .map(|w| Part {
                start: w[0],
                end: w[1],
                removed: self.resume_after(w[0]).is_some_and(|r| r >= w[1] - SAME),
            })
            .collect()
    }

    /// The part a source time falls in.
    pub fn part_at(&self, t: f64, duration: f64) -> Option<Part> {
        let parts = self.parts(duration);
        parts
            .iter()
            .find(|p| p.covers(t))
            .or(parts.last().filter(|p| t >= p.end))
            .copied()
    }

    /// Take a stretch out of the finished video, so the gap closes up, and
    /// return the id of the [`Tool::Cut`] that does it.
    ///
    /// Anything else that changed time inside the stretch goes with it — its
    /// footage is gone, and two edits over one stretch of time is a problem
    /// — and one that ran across an edge keeps the part outside.
    pub fn remove_part(&mut self, start: f64, end: f64) -> u32 {
        self.clear_time(start, end, Tool::alters_time);
        self.add(start, end, Tool::Cut)
    }

    /// Put a removed stretch back.
    pub fn restore_part(&mut self, start: f64, end: f64) {
        self.clear_time(start, end, Tool::removes);
    }

    /// Clear the effects `which` picks out of `start..end`: those inside it
    /// go, and those across an edge are trimmed back to it, in two if they
    /// ran across both.
    fn clear_time(&mut self, start: f64, end: f64, which: fn(&Tool) -> bool) {
        let mut tails = Vec::new();
        self.effects.retain_mut(|e| {
            if !which(&e.tool) || e.end <= start + SAME || e.start >= end - SAME {
                return true;
            }
            if e.end > end + MIN_SPAN && e.start < start - MIN_SPAN {
                tails.push((end, e.end, e.tool.clone(), e.screen.clone()));
            }
            if e.start < start - MIN_SPAN {
                e.end = start;
                true
            } else if e.end > end + MIN_SPAN {
                e.start = end;
                true
            } else {
                false
            }
        });
        for (a, b, tool, screen) in tails {
            self.add_on(a, b, tool, screen);
        }
        self.sort();
    }

    /// Where playback picks up from a source time that has been removed:
    /// the far end of the removed run it is in, through any removed
    /// stretches that touch. `None` when the time is kept.
    pub fn resume_after(&self, t: f64) -> Option<f64> {
        let mut at = t;
        while let Some(end) = self
            .effects
            .iter()
            .filter(|e| e.tool.removes() && at >= e.start - SAME && at < e.end - SAME)
            .map(|e| e.end)
            .reduce(f64::max)
        {
            at = end;
        }
        (at > t).then_some(at)
    }

    /// Everything wrong with the list, worst first. An empty result means it
    /// can be exported as it stands.
    pub fn problems(&self, source_duration: f64) -> Vec<Problem> {
        let mut found = Vec::new();
        let retimed: Vec<&Effect> = self.retimed().collect();
        for (i, a) in retimed.iter().enumerate() {
            for b in retimed.iter().skip(i + 1) {
                if a.end > b.start + f64::EPSILON && a.start < b.end {
                    found.push(Problem::Overlap { a: a.id, b: b.id });
                }
            }
        }
        for m in &self.effects {
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
    fn an_effect_placed_on_a_screen_remembers_which_one() {
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
            "a effect that changes time belongs to no one screen"
        );

        // The screen is only written when there is one, so an edit list
        // from before this existed reads back unchanged.
        let json = serde_json::to_string(edits.get(anywhere).unwrap()).unwrap();
        assert!(!json.contains("screen"), "{json}");
        let back: Effect =
            serde_json::from_str(r#"{"id":9,"start":0.0,"end":2.0,"tool":"mute"}"#).unwrap();
        assert_eq!(back.screen, None);
    }

    fn list(effects: &[(f64, f64, Tool)]) -> EditList {
        let mut l = EditList::new();
        for (a, b, t) in effects {
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
        l.remove(l.effects[0].id);
        assert_eq!(l.rate_at(15.0), 1.0);
    }

    #[test]
    fn ids_stay_unique_and_the_list_stays_in_order() {
        let mut l = EditList::new();
        let late = l.add(30.0, 32.0, Tool::Cut);
        let early = l.add(5.0, 6.0, Tool::Cut);
        assert_ne!(late, early);
        assert_eq!(l.effects[0].id, early, "sorted by start, not by insertion");
        l.update(early, |m| {
            m.start = 40.0;
            m.end = 41.0;
        });
        assert_eq!(l.effects[0].id, late, "moving one re-sorts the list");
    }

    #[test]
    fn an_effect_cannot_be_inverted_or_vanishingly_short() {
        let m = Effect::new(1, 9.0, 4.0, Tool::Cut);
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
    fn a_cut_splits_the_recording_in_two_and_changes_nothing_alone() {
        let mut l = EditList::new();
        assert_eq!(
            l.parts(30.0),
            vec![Part {
                start: 0.0,
                end: 30.0,
                removed: false
            }],
            "uncut, the recording is one part"
        );
        assert!(l.cut_at(12.0, 30.0));
        let parts = l.parts(30.0);
        assert_eq!(parts.len(), 2);
        assert_eq!((parts[0].start, parts[0].end), (0.0, 12.0));
        assert_eq!((parts[1].start, parts[1].end), (12.0, 30.0));
        assert!(parts.iter().all(|p| !p.removed));
        assert_eq!(l.output_duration(30.0), 30.0, "a cut alone removes nothing");
        assert_eq!(l.to_output(20.0), Some(20.0));
        assert!(l.problems(30.0).is_empty());
        assert!(!l.is_empty(), "a cut is an edit worth saving");
    }

    #[test]
    fn removing_the_part_after_a_cut_closes_the_gap() {
        let mut l = EditList::new();
        l.cut_at(10.0, 30.0);
        l.cut_at(18.0, 30.0);
        let middle = l.part_at(12.0, 30.0).unwrap();
        assert_eq!((middle.start, middle.end), (10.0, 18.0));
        l.remove_part(middle.start, middle.end);

        assert!(l.parts(30.0)[1].removed);
        assert!(!l.parts(30.0)[0].removed && !l.parts(30.0)[2].removed);
        assert_eq!(l.output_duration(30.0), 22.0);
        assert_eq!(l.to_output(9.0), Some(9.0));
        assert_eq!(l.to_output(12.0), None);
        assert_eq!(l.to_output(18.0), Some(10.0), "the far side follows on");
        assert_eq!(l.resume_after(12.0), Some(18.0), "playing skips the gap");
        assert_eq!(l.resume_after(9.0), None);
        assert!(l.problems(30.0).is_empty());

        // Either side can go too, and the first part going is the opening
        // being trimmed off.
        l.remove_part(0.0, 10.0);
        assert_eq!(l.output_duration(30.0), 12.0);
        assert_eq!(l.resume_after(0.0), Some(18.0), "through touching gaps");
        assert!((l.to_source(0.0) - 18.0).abs() < 1e-9);
    }

    #[test]
    fn a_cut_is_refused_where_it_would_split_nothing() {
        let mut l = EditList::new();
        assert!(!l.cut_at(0.0, 30.0), "at the very start");
        assert!(!l.cut_at(30.0, 30.0), "at the very end");
        assert!(l.cut_at(10.0, 30.0));
        assert!(!l.cut_at(10.05, 30.0), "on top of another");
        l.add(20.0, 25.0, Tool::Cut);
        assert!(!l.cut_at(22.0, 30.0), "inside footage already removed");
        assert_eq!(l.cuts, vec![10.0]);
    }

    #[test]
    fn a_removed_part_can_be_put_back_and_a_cut_joined() {
        let mut l = EditList::new();
        l.cut_at(10.0, 30.0);
        l.remove_part(10.0, 30.0);
        assert_eq!(l.output_duration(30.0), 10.0);
        l.restore_part(10.0, 30.0);
        assert_eq!(l.output_duration(30.0), 30.0);
        assert!(l.effects.is_empty());

        // Joining only takes the cut away: what was removed beside it is an
        // edit of its own, still listed and still removed.
        l.remove_part(10.0, 30.0);
        l.join(10.0);
        assert!(l.cuts.is_empty());
        assert_eq!(l.output_duration(30.0), 10.0);
    }

    #[test]
    fn removing_a_part_takes_the_time_edits_inside_it_along() {
        let mut l = list(&[
            (11.0, 13.0, Tool::Speed { rate: 2.0 }),
            (5.0, 25.0, Tool::Mute),
            (14.0, 28.0, Tool::Speed { rate: 4.0 }),
        ]);
        l.cut_at(10.0, 30.0);
        l.cut_at(20.0, 30.0);
        l.remove_part(10.0, 20.0);

        assert!(l.problems(30.0).is_empty(), "{:?}", l.problems(30.0));
        assert!(
            !l.effects
                .iter()
                .any(|e| matches!(e.tool, Tool::Speed { rate } if rate == 2.0)),
            "the speed-up inside the part went with it"
        );
        let speed = l
            .effects
            .iter()
            .find(|e| matches!(e.tool, Tool::Speed { rate } if rate == 4.0))
            .unwrap();
        assert_eq!(
            (speed.start, speed.end),
            (20.0, 28.0),
            "trimmed to the edge"
        );
        assert!(
            l.effects
                .iter()
                .any(|e| e.tool == Tool::Mute && e.span() == 20.0),
            "an edit that changes no time is left alone"
        );
    }

    #[test]
    fn an_edit_across_a_removed_part_keeps_both_sides() {
        let mut l = list(&[(2.0, 28.0, Tool::Speed { rate: 2.0 })]);
        l.remove_part(10.0, 20.0);
        let mut spans: Vec<(f64, f64)> = l
            .effects
            .iter()
            .filter(|e| matches!(e.tool, Tool::Speed { .. }))
            .map(|e| (e.start, e.end))
            .collect();
        spans.sort_by(|a, b| a.0.total_cmp(&b.0));
        assert_eq!(spans, vec![(2.0, 10.0), (20.0, 28.0)]);
        assert!(l.problems(30.0).is_empty());
    }

    #[test]
    fn moving_a_cut_moves_the_edge_of_the_gap_with_it() {
        let mut l = EditList::new();
        l.cut_at(10.0, 30.0);
        l.cut_at(20.0, 30.0);
        l.remove_part(10.0, 20.0);
        assert_eq!(l.move_cut(10.0, 8.0, 30.0), 8.0);
        assert_eq!(l.cuts, vec![8.0, 20.0]);
        let gap = l.effects.iter().find(|e| e.tool == Tool::Cut).unwrap();
        assert_eq!((gap.start, gap.end), (8.0, 20.0));
        // It stops short of its neighbour rather than crossing it.
        let landed = l.move_cut(8.0, 25.0, 30.0);
        assert!(landed < 20.0, "landed at {landed}");
        assert!(
            l.parts(30.0)
                .iter()
                .all(|p| p.end - p.start >= MIN_SPAN - 1e-9)
        );
    }

    #[test]
    fn cuts_round_trip_and_an_old_list_without_them_still_loads() {
        let mut l = EditList::new();
        l.cut_at(4.5, 30.0);
        let text = serde_json::to_string(&l).unwrap();
        assert!(text.contains("\"cuts\":[4.5]"), "{text}");
        let back: EditList = serde_json::from_str(&text).unwrap();
        assert_eq!(back, l);

        let uncut = serde_json::to_string(&EditList::new()).unwrap();
        assert!(
            !uncut.contains("cuts"),
            "nothing written when uncut: {uncut}"
        );
        let old: EditList = serde_json::from_str(
            r#"{"effects":[{"id":1,"start":1.0,"end":2.0,"tool":"cut"}],"next_id":1}"#,
        )
        .unwrap();
        assert!(old.cuts.is_empty());
        assert_eq!(old.parts(10.0).len(), 1);
        assert_eq!(old.output_duration(10.0), 9.0);
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
