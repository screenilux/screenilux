//! Text read off recorded frames: what makes a video's text copyable and
//! searchable.
//!
//! The OCR engine itself lives in the `win` crate. This is what is done with
//! its output: selecting a run of words by dragging across them, and an index
//! of the text over a whole recording that can be searched and saved.

use serde::{Deserialize, Serialize};

pub const TEXT_FORMAT: &str = "screenilux-text";
pub const TEXT_VERSION: u32 = 1;
/// Name of the index inside a package or session folder.
pub const TEXT_INDEX_NAME: &str = "text.json";

/// One recognised word, in frame pixels.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Word {
    pub text: String,
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl Word {
    fn distance_sq(&self, px: f32, py: f32) -> f32 {
        let dx = (self.x - px).max(0.0).max(px - (self.x + self.w));
        let dy = (self.y - py).max(0.0).max(py - (self.y + self.h));
        dx * dx + dy * dy
    }
}

/// Words the engine grouped into one line, in reading order.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct Line {
    pub words: Vec<Word>,
}

/// All the text on one frame, lines in reading order.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct Page {
    pub lines: Vec<Line>,
}

/// A word's position in a page: `(line, word)`.
pub type WordRef = (usize, usize);

impl Page {
    pub fn is_empty(&self) -> bool {
        self.lines.iter().all(|l| l.words.is_empty())
    }

    pub fn words(&self) -> impl Iterator<Item = (WordRef, &Word)> {
        self.lines
            .iter()
            .enumerate()
            .flat_map(|(li, l)| l.words.iter().enumerate().map(move |(wi, w)| ((li, wi), w)))
    }

    /// Everything, one line per line.
    pub fn text(&self) -> String {
        self.lines
            .iter()
            .filter(|l| !l.words.is_empty())
            .map(|l| {
                l.words
                    .iter()
                    .map(|w| w.text.as_str())
                    .collect::<Vec<_>>()
                    .join(" ")
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// The word under a point, or failing that the nearest one.
    pub fn word_near(&self, x: f32, y: f32) -> Option<WordRef> {
        self.words()
            .min_by(|(_, a), (_, b)| a.distance_sq(x, y).total_cmp(&b.distance_sq(x, y)))
            .map(|(r, _)| r)
    }

    /// The words between two points in reading order, as a text editor
    /// selects: dragging from the middle of one line to the middle of the next
    /// takes the rest of the first line and the start of the second, not a
    /// rectangle.
    pub fn select(&self, from: (f32, f32), to: (f32, f32)) -> Option<(WordRef, WordRef)> {
        let a = self.word_near(from.0, from.1)?;
        let b = self.word_near(to.0, to.1)?;
        Some(if a <= b { (a, b) } else { (b, a) })
    }

    /// Whether a word falls inside a selection.
    pub fn is_selected(word: WordRef, selection: (WordRef, WordRef)) -> bool {
        word >= selection.0 && word <= selection.1
    }

    /// The selected words as text, keeping line breaks.
    pub fn selected_text(&self, (start, end): (WordRef, WordRef)) -> String {
        let mut out = String::new();
        let mut last_line = None;
        for ((li, wi), w) in self.words() {
            if !Self::is_selected((li, wi), (start, end)) {
                continue;
            }
            match last_line {
                Some(l) if l != li => out.push('\n'),
                Some(_) => out.push(' '),
                None => {}
            }
            out.push_str(&w.text);
            last_line = Some(li);
        }
        out
    }
}

/// The text one screen showed over a span of the recording.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Entry {
    /// The track's id: its file name in the manifest.
    pub screen: String,
    /// Recorded seconds this text was first seen.
    pub t: f64,
    /// Last moment it was still seen.
    pub until: f64,
    pub page: Page,
}

/// A search result.
#[derive(Debug, Clone, PartialEq)]
pub struct Hit {
    pub entry: usize,
    pub screen: String,
    pub t: f64,
    /// The matching line, for showing in a result list.
    pub snippet: String,
}

/// The text of a whole recording, sampled every `interval` seconds.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TextIndex {
    pub format: String,
    pub version: u32,
    pub interval: f64,
    pub entries: Vec<Entry>,
}

#[derive(Debug, thiserror::Error)]
pub enum TextIndexError {
    #[error("not a {TEXT_FORMAT} index")]
    WrongFormat,
    #[error("unsupported text index version {0}")]
    UnsupportedVersion(u32),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
}

impl TextIndex {
    pub fn new(interval: f64) -> Self {
        Self {
            format: TEXT_FORMAT.into(),
            version: TEXT_VERSION,
            interval,
            entries: Vec::new(),
        }
    }

    /// Add what `screen` showed at `t`.
    ///
    /// A screen often shows the same text for minutes. Rather than storing a
    /// copy per sample, an unchanged page extends the previous entry for that
    /// screen, so the index grows with how much the text changes, not with
    /// the length of the recording.
    pub fn record(&mut self, screen: &str, t: f64, page: Page) {
        if let Some(last) = self.entries.iter_mut().rev().find(|e| e.screen == screen)
            && last.page.text() == page.text()
        {
            last.until = last.until.max(t);
            return;
        }
        if page.is_empty() {
            return;
        }
        self.entries.push(Entry {
            screen: screen.to_string(),
            t,
            until: t,
            page,
        });
    }

    /// Every appearance of `query`, ignoring case, in time order.
    ///
    /// A line that stays on screen while other text around it changes is in
    /// several entries in a row. That is one appearance, reported where it
    /// began, not one result per sample; it counts again only once it has
    /// left the screen and come back.
    pub fn search(&self, query: &str) -> Vec<Hit> {
        let needle = query.trim().to_lowercase();
        if needle.is_empty() {
            return Vec::new();
        }
        let mut order: Vec<usize> = (0..self.entries.len()).collect();
        order.sort_by(|&a, &b| self.entries[a].t.total_cmp(&self.entries[b].t));
        // Per screen, the line its previous entry matched on, if it did.
        let mut previous: std::collections::HashMap<&str, Option<String>> =
            std::collections::HashMap::new();

        let mut hits: Vec<Hit> = Vec::new();
        for i in order {
            let e = &self.entries[i];
            let snippet = Self::matching_line(&e.page.text(), &needle);
            let before = previous.insert(e.screen.as_str(), snippet.clone());
            let Some(snippet) = snippet else { continue };
            if before.flatten().as_deref() == Some(snippet.as_str()) {
                continue;
            }
            hits.push(Hit {
                entry: i,
                screen: e.screen.clone(),
                t: e.t,
                snippet,
            });
        }
        hits
    }

    /// The line of `text` where `needle` (already lowercase) first appears.
    fn matching_line(text: &str, needle: &str) -> Option<String> {
        // Search the whole page so a phrase that wraps still matches, then
        // report the line it starts on.
        let lower = text.to_lowercase();
        let at = lower.find(needle)?;
        let line_start = lower[..at].rfind('\n').map(|p| p + 1).unwrap_or(0);
        let line_end = lower[at..]
            .find('\n')
            .map(|p| at + p)
            .unwrap_or(lower.len());
        // Lowercasing can change byte lengths; fall back to the whole
        // text rather than slicing mid-character.
        Some(
            text.get(line_start..line_end)
                .unwrap_or(text)
                .trim()
                .to_string(),
        )
    }

    pub fn to_json(&self) -> Result<String, TextIndexError> {
        Ok(serde_json::to_string(self)?)
    }

    pub fn from_json(s: &str) -> Result<Self, TextIndexError> {
        let idx: Self = serde_json::from_str(s)?;
        if idx.format != TEXT_FORMAT {
            return Err(TextIndexError::WrongFormat);
        }
        if idx.version != TEXT_VERSION {
            return Err(TextIndexError::UnsupportedVersion(idx.version));
        }
        Ok(idx)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn word(text: &str, x: f32, y: f32) -> Word {
        Word {
            text: text.into(),
            x,
            y,
            w: 40.0,
            h: 10.0,
        }
    }

    /// Two lines of three words each.
    fn page() -> Page {
        Page {
            lines: vec![
                Line {
                    words: vec![
                        word("alpha", 0.0, 0.0),
                        word("beta", 50.0, 0.0),
                        word("gamma", 100.0, 0.0),
                    ],
                },
                Line {
                    words: vec![
                        word("delta", 0.0, 20.0),
                        word("epsilon", 50.0, 20.0),
                        word("zeta", 100.0, 20.0),
                    ],
                },
            ],
        }
    }

    #[test]
    fn page_text_keeps_lines() {
        assert_eq!(page().text(), "alpha beta gamma\ndelta epsilon zeta");
    }

    #[test]
    fn a_point_finds_the_word_under_it_or_the_nearest() {
        let p = page();
        assert_eq!(p.word_near(55.0, 5.0), Some((0, 1)));
        assert_eq!(
            p.word_near(400.0, 25.0),
            Some((1, 2)),
            "far right of line two"
        );
        assert_eq!(Page::default().word_near(0.0, 0.0), None);
    }

    #[test]
    fn dragging_across_lines_selects_in_reading_order() {
        let p = page();
        let sel = p.select((55.0, 5.0), (55.0, 25.0)).unwrap();
        assert_eq!(p.selected_text(sel), "beta gamma\ndelta epsilon");
    }

    #[test]
    fn dragging_backwards_selects_the_same_words() {
        let p = page();
        let fwd = p.select((5.0, 5.0), (105.0, 5.0)).unwrap();
        let back = p.select((105.0, 5.0), (5.0, 5.0)).unwrap();
        assert_eq!(fwd, back);
        assert_eq!(p.selected_text(fwd), "alpha beta gamma");
    }

    #[test]
    fn unchanged_text_extends_an_entry_instead_of_repeating_it() {
        let mut idx = TextIndex::new(2.0);
        idx.record("screen1.mp4", 0.0, page());
        idx.record("screen1.mp4", 2.0, page());
        idx.record("screen1.mp4", 4.0, page());
        assert_eq!(idx.entries.len(), 1);
        assert_eq!(idx.entries[0].until, 4.0);
    }

    #[test]
    fn screens_are_tracked_separately() {
        let mut idx = TextIndex::new(2.0);
        idx.record("screen1.mp4", 0.0, page());
        idx.record("screen2.mp4", 0.0, page());
        idx.record("screen1.mp4", 2.0, page());
        assert_eq!(idx.entries.len(), 2);
    }

    #[test]
    fn changed_text_starts_a_new_entry() {
        let mut idx = TextIndex::new(2.0);
        idx.record("s", 0.0, page());
        let mut other = page();
        other.lines[0].words[0].text = "omega".into();
        idx.record("s", 2.0, other);
        assert_eq!(idx.entries.len(), 2);
        assert_eq!(idx.entries[1].t, 2.0);
    }

    #[test]
    fn blank_frames_are_not_stored() {
        let mut idx = TextIndex::new(2.0);
        idx.record("s", 0.0, Page::default());
        assert!(idx.entries.is_empty());
    }

    #[test]
    fn search_ignores_case_and_reports_the_line() {
        let mut idx = TextIndex::new(2.0);
        idx.record("s", 3.0, page());
        let hits = idx.search("EPSILON");
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].t, 3.0);
        assert_eq!(hits[0].snippet, "delta epsilon zeta");
    }

    #[test]
    fn a_phrase_spanning_a_line_break_still_matches() {
        let mut idx = TextIndex::new(2.0);
        idx.record("s", 0.0, page());
        assert_eq!(idx.search("gamma\ndelta").len(), 1);
        assert!(idx.search("").is_empty());
        assert!(idx.search("nothing").is_empty());
    }

    #[test]
    fn a_line_that_stays_on_screen_is_one_result() {
        let mut idx = TextIndex::new(2.0);
        let mut changing = page();
        for (n, t) in [0.0, 2.0, 4.0].into_iter().enumerate() {
            // The first line stays; the second changes every sample.
            changing.lines[1].words[2].text = format!("tick{n}");
            idx.record("s", t, changing.clone());
        }
        assert_eq!(idx.entries.len(), 3);
        let hits = idx.search("beta");
        assert_eq!(hits.len(), 1, "one appearance, not one per sample");
        assert_eq!(hits[0].t, 0.0, "reported where it began");
    }

    #[test]
    fn a_line_that_leaves_and_returns_counts_again() {
        let mut idx = TextIndex::new(2.0);
        idx.record("s", 0.0, page());
        let mut other = page();
        other.lines[0].words[1].text = "other".into();
        idx.record("s", 2.0, other);
        idx.record("s", 4.0, page());
        let hits = idx.search("beta");
        assert_eq!(hits.iter().map(|h| h.t).collect::<Vec<_>>(), vec![0.0, 4.0]);
    }

    #[test]
    fn the_same_line_on_two_screens_is_two_results() {
        let mut idx = TextIndex::new(2.0);
        idx.record("a", 0.0, page());
        idx.record("b", 0.0, page());
        assert_eq!(idx.search("beta").len(), 2);
    }

    #[test]
    fn results_come_back_in_time_order() {
        let mut idx = TextIndex::new(2.0);
        idx.record("b", 8.0, page());
        idx.record("a", 1.0, page());
        let hits = idx.search("alpha");
        assert_eq!(hits.iter().map(|h| h.t).collect::<Vec<_>>(), vec![1.0, 8.0]);
    }

    #[test]
    fn the_index_round_trips_through_json() {
        let mut idx = TextIndex::new(2.0);
        idx.record("s", 0.0, page());
        let back = TextIndex::from_json(&idx.to_json().unwrap()).unwrap();
        assert_eq!(back, idx);
    }

    #[test]
    fn a_foreign_json_file_is_rejected() {
        let mut idx = TextIndex::new(2.0);
        idx.format = "something-else".into();
        let json = serde_json::to_string(&idx).unwrap();
        assert!(matches!(
            TextIndex::from_json(&json),
            Err(TextIndexError::WrongFormat)
        ));
    }
}
