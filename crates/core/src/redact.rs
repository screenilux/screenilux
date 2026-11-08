//! Finding what should not be shown: email addresses, keys, passwords and
//! card numbers in the text read off a recording, and where they were.
//!
//! The editor turns each find into an ordinary Blur effect, so one that turns
//! out not to be private is deleted like any other edit. That decides which
//! way to err: a blur too many costs a click, a key left showing cannot be
//! taken back once the video is sent. Each find is blurred from one sample
//! before it was read until one sample after it was last seen, since the text
//! may have appeared or lingered between samples.

use crate::edit::Area;
use crate::text::{Entry, TextIndex, Word};

/// What was found.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Kind {
    Email,
    Key,
    Password,
    Card,
}

impl Kind {
    /// What it is called, for `n` of them.
    pub fn noun(self, n: usize) -> &'static str {
        match (self, n == 1) {
            (Kind::Email, true) => "email address",
            (Kind::Email, false) => "email addresses",
            (Kind::Key, true) => "key",
            (Kind::Key, false) => "keys",
            (Kind::Password, true) => "password",
            (Kind::Password, false) => "passwords",
            (Kind::Card, true) => "card number",
            (Kind::Card, false) => "card numbers",
        }
    }
}

/// One private thing, where and when it was on screen.
#[derive(Debug, Clone, PartialEq)]
pub struct Found {
    pub kind: Kind,
    /// The screen it was on, by the name the text index gives it.
    pub screen: String,
    /// Seconds into the original recording.
    pub start: f64,
    pub end: f64,
    pub area: Area,
}

/// Everything private on screen in `index`. `size` gives a screen's size in
/// pixels by its name; text on a screen it does not know is skipped.
///
/// The same thing seen in sample after sample, in the same place, is one find
/// spanning all of them rather than one per sample.
pub fn find(index: &TextIndex, size: impl Fn(&str) -> Option<(u32, u32)>) -> Vec<Found> {
    let mut found: Vec<Found> = Vec::new();
    let mut entries: Vec<&Entry> = index.entries.iter().collect();
    entries.sort_by(|a, b| a.t.total_cmp(&b.t));
    for entry in entries {
        let Some((w, h)) = size(&entry.screen) else {
            continue;
        };
        if w == 0 || h == 0 {
            continue;
        }
        let start = (entry.t - index.interval).max(0.0);
        let end = entry.until + index.interval;
        for (kind, rect) in secrets_on(entry) {
            let area = to_area(rect, w as f32, h as f32);
            let same = found.iter_mut().find(|f| {
                f.screen == entry.screen
                    && f.kind == kind
                    && start <= f.end
                    && overlap(&f.area, &area) > 0.5
            });
            match same {
                Some(f) => {
                    f.end = f.end.max(end);
                    f.area = union(&f.area, &area);
                }
                None => found.push(Found {
                    kind,
                    screen: entry.screen.clone(),
                    start,
                    end,
                    area,
                }),
            }
        }
    }
    found
}

/// A rectangle in pixels: x, y, width, height.
type Px = [f32; 4];

fn word_px(w: &Word) -> Px {
    [w.x, w.y, w.w, w.h]
}

fn union_px(a: Px, b: Px) -> Px {
    let x0 = a[0].min(b[0]);
    let y0 = a[1].min(b[1]);
    let x1 = (a[0] + a[2]).max(b[0] + b[2]);
    let y1 = (a[1] + a[3]).max(b[1] + b[3]);
    [x0, y0, x1 - x0, y1 - y0]
}

/// A rectangle as a share of the screen, grown a little so the edges of the
/// letters are covered too.
fn to_area(r: Px, w: f32, h: f32) -> Area {
    let pad = (r[3] * 0.35).max(3.0);
    Area::new(
        (r[0] - pad) / w,
        (r[1] - pad) / h,
        (r[2] + 2.0 * pad) / w,
        (r[3] + 2.0 * pad) / h,
    )
}

/// Whether `a` already covers most of `b`, so a blur over `a` hides what is
/// at `b`.
pub fn covers(a: &Area, b: &Area) -> bool {
    overlap(a, b) > 0.5
}

/// How much of the smaller of two areas the other covers.
fn overlap(a: &Area, b: &Area) -> f32 {
    let x = (a.x + a.w).min(b.x + b.w) - a.x.max(b.x);
    let y = (a.y + a.h).min(b.y + b.h) - a.y.max(b.y);
    if x <= 0.0 || y <= 0.0 {
        return 0.0;
    }
    let smaller = (a.w * a.h).min(b.w * b.h).max(f32::EPSILON);
    x * y / smaller
}

fn union(a: &Area, b: &Area) -> Area {
    let x0 = a.x.min(b.x);
    let y0 = a.y.min(b.y);
    Area::new(
        x0,
        y0,
        (a.x + a.w).max(b.x + b.w) - x0,
        (a.y + a.h).max(b.y + b.h) - y0,
    )
}

/// A word without the punctuation text reading leaves around it.
fn clean(text: &str) -> &str {
    text.trim_matches(|c: char| {
        matches!(
            c,
            ',' | ';' | '"' | '\'' | '(' | ')' | '[' | ']' | '{' | '}' | '<' | '>' | '`'
        )
    })
    .trim_end_matches('.')
}

/// Every private thing on one sampled screen, with where it was.
fn secrets_on(entry: &Entry) -> Vec<(Kind, Px)> {
    let mut out = Vec::new();
    for line in &entry.page.lines {
        let words = &line.words;
        let mut i = 0;
        while i < words.len() {
            // A card number is usually written in groups, so it is looked for
            // across several words first.
            if let Some(n) = card_at(words, i) {
                let rect = words[i..i + n]
                    .iter()
                    .map(word_px)
                    .reduce(union_px)
                    .expect("a card is at least one word");
                out.push((Kind::Card, rect));
                i += n;
                continue;
            }
            let text = clean(&words[i].text);
            if is_email(text) {
                out.push((Kind::Email, word_px(&words[i])));
            } else if is_key(text) {
                out.push((Kind::Key, word_px(&words[i])));
            } else if labelled_inline(text) {
                out.push((Kind::Password, word_px(&words[i])));
            } else if is_label(text, words.get(i + 1).map(|w| w.text.as_str()))
                && let Some(next) = words.iter().skip(i + 1).find(|w| !is_separator(&w.text))
            {
                out.push((Kind::Password, word_px(next)));
                i += 1;
            }
            i += 1;
        }
    }
    out
}

fn is_email(text: &str) -> bool {
    let Some((local, domain)) = text.split_once('@') else {
        return false;
    };
    let Some((name, tld)) = domain.rsplit_once('.') else {
        return false;
    };
    !local.is_empty()
        && !name.is_empty()
        && !domain.contains('@')
        && local
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "._%+-".contains(c))
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || ".-".contains(c))
        && tld.len() >= 2
        && tld.chars().all(|c| c.is_ascii_alphabetic())
}

/// The way well-known services start their keys and tokens.
const KEY_PREFIXES: &[&str] = &[
    "sk-",
    "sk_live_",
    "sk_test_",
    "rk_live_",
    "pk_live_",
    "ghp_",
    "gho_",
    "ghu_",
    "ghs_",
    "ghr_",
    "github_pat_",
    "glpat-",
    "xoxb-",
    "xoxp-",
    "xoxa-",
    "xoxs-",
    "AKIA",
    "ASIA",
    "AIza",
    "hf_",
    "npm_",
];

fn is_key(text: &str) -> bool {
    let token = text
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || "_-+/=.".contains(c));
    if !token {
        return false;
    }
    if text.len() >= 16 && KEY_PREFIXES.iter().any(|p| text.starts_with(p)) {
        return true;
    }
    // A JSON Web Token: three base64 parts, the first always `eyJ`.
    if text.starts_with("eyJ") && text.matches('.').count() == 2 && text.len() >= 30 {
        return true;
    }
    random_looking(text)
}

/// A long run of mixed letters and digits with no words in it, the way a
/// generated secret looks. Both cases are required, which keeps out hashes
/// and ids written in hex, and long ordinary words never have digits mixed
/// through them.
fn random_looking(text: &str) -> bool {
    if text.len() < 24 || text.contains('.') {
        return false;
    }
    let upper = text.chars().any(|c| c.is_ascii_uppercase());
    let lower = text.chars().any(|c| c.is_ascii_lowercase());
    let digits = text.chars().filter(|c| c.is_ascii_digit()).count();
    upper && lower && digits >= 2 && entropy(text) >= 3.5
}

/// Bits per character.
fn entropy(text: &str) -> f32 {
    let mut counts = std::collections::HashMap::new();
    for c in text.chars() {
        *counts.entry(c).or_insert(0u32) += 1;
    }
    let n = text.chars().count() as f32;
    counts
        .values()
        .map(|&k| {
            let p = k as f32 / n;
            -p * p.log2()
        })
        .sum()
}

/// What a private value is introduced with.
const LABELS: &[&str] = &[
    "password",
    "passwd",
    "pwd",
    "passphrase",
    "secret",
    "token",
    "apikey",
    "api_key",
    "api-key",
    "access_key",
    "secret_key",
    "client_secret",
    "private_key",
];

fn is_label_word(word: &str) -> bool {
    let w = word.to_ascii_lowercase();
    let w = w.trim_start_matches('$');
    LABELS
        .iter()
        .any(|l| w == *l || w.ends_with(&format!("_{l}")) || w.ends_with(&format!("-{l}")))
}

/// `PASSWORD=hunter2` or `token:abc123` as one word.
fn labelled_inline(text: &str) -> bool {
    let Some(at) = text.find(['=', ':']) else {
        return false;
    };
    let (label, value) = (&text[..at], &text[at + 1..]);
    is_label_word(label) && value.trim_matches(['"', '\'']).len() >= 3
}

/// A label for the word that follows: `Password:` before the value, or
/// `Password` then a lone `:` or `=`.
fn is_label(text: &str, next: Option<&str>) -> bool {
    match text.strip_suffix([':', '=']) {
        Some(label) => is_label_word(label),
        None => is_label_word(text) && next.is_some_and(is_separator),
    }
}

fn is_separator(text: &str) -> bool {
    matches!(text.trim(), ":" | "=" | "=>")
}

/// How many words from `i` make up a card number, if they do: 13 to 19
/// digits in one or more groups, passing the Luhn check every card number
/// passes. The longest such run is taken.
fn card_at(words: &[Word], i: usize) -> Option<usize> {
    let mut digits = String::new();
    let mut best = None;
    for (n, word) in words[i..].iter().take(5).enumerate() {
        let text = clean(&word.text);
        if text.is_empty() || !text.chars().all(|c| c.is_ascii_digit() || c == '-') {
            break;
        }
        digits.extend(text.chars().filter(char::is_ascii_digit));
        if (13..=19).contains(&digits.len()) && luhn(&digits) {
            best = Some(n + 1);
        }
        if digits.len() > 19 {
            break;
        }
    }
    best
}

fn luhn(digits: &str) -> bool {
    let sum: u32 = digits
        .chars()
        .rev()
        .filter_map(|c| c.to_digit(10))
        .enumerate()
        .map(|(i, d)| {
            if i % 2 == 1 {
                let d = d * 2;
                if d > 9 { d - 9 } else { d }
            } else {
                d
            }
        })
        .sum();
    sum.is_multiple_of(10)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::text::{Line, Page};

    /// A line of words laid out left to right, 10 pixels a letter.
    fn line(text: &str, y: f32) -> Line {
        let mut x = 10.0;
        let words = text
            .split(' ')
            .map(|t| {
                let w = Word {
                    text: t.to_string(),
                    x,
                    y,
                    w: t.len() as f32 * 10.0,
                    h: 20.0,
                };
                x += w.w + 10.0;
                w
            })
            .collect();
        Line { words }
    }

    fn kinds(text: &str) -> Vec<Kind> {
        let entry = Entry {
            screen: "screen1.mp4".into(),
            t: 0.0,
            until: 0.0,
            page: Page {
                lines: vec![line(text, 100.0)],
            },
        };
        secrets_on(&entry).into_iter().map(|(k, _)| k).collect()
    }

    #[test]
    fn finds_email_addresses() {
        assert_eq!(
            kinds("Write to jane.doe+demo@example.co.uk today"),
            [Kind::Email]
        );
        assert_eq!(kinds("(ops@acme.io),"), [Kind::Email]);
        assert!(kinds("an @mention and a.b and x@y").is_empty());
    }

    #[test]
    fn finds_keys_by_their_shape() {
        assert_eq!(kinds("OPENAI sk-proj-abc123DEF456ghi789"), [Kind::Key]);
        assert_eq!(
            kinds("ghp_A1b2C3d4E5f6G7h8I9j0K1l2M3n4O5p6Q7r8"),
            [Kind::Key]
        );
        assert_eq!(kinds("AKIAIOSFODNN7EXAMPLE"), [Kind::Key]);
        assert_eq!(
            kinds(
                "eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxMjM0In0.dozjgNryP4J3jVmNHl0w5N_XgL0n3I9PlFUP0THsR8U"
            ),
            [Kind::Key]
        );
        assert_eq!(kinds("Q2x8bR7TzP4wN9kLm3VdF6sJ"), [Kind::Key]);
    }

    #[test]
    fn leaves_ordinary_text_alone() {
        // Words, a commit hash, a date, a version, a long word, a sentence.
        for text in [
            "Recording saved to Videos ScreenRec",
            "commit 3f9a2c4e1b7d8f0a6c5e4d3b2a1f0e9d8c7b6a5f",
            "2026-11-06 14:03:22",
            "version 1.99.1 released",
            "internationalisation",
            "Forgot your password? Reset it here",
            "1234 5678",
        ] {
            assert!(kinds(text).is_empty(), "{text}");
        }
    }

    #[test]
    fn finds_card_numbers_split_into_groups() {
        assert_eq!(kinds("Card 4242 4242 4242 4242 exp 12/30"), [Kind::Card]);
        assert_eq!(kinds("5555-5555-5555-4444"), [Kind::Card]);
        // Sixteen digits that fail the check are not a card.
        assert!(kinds("4242 4242 4242 4241").is_empty());
    }

    #[test]
    fn finds_values_after_a_label() {
        assert_eq!(kinds("export API_KEY=abc123xyz"), [Kind::Password]);
        assert_eq!(kinds("Password: hunter2"), [Kind::Password]);
        assert_eq!(kinds("DB_PASSWORD = s3cret!"), [Kind::Password]);
        assert_eq!(kinds("token:abcdef"), [Kind::Password]);
    }

    #[test]
    fn a_label_blurs_the_value_not_the_label() {
        let entry = Entry {
            screen: "s".into(),
            t: 0.0,
            until: 0.0,
            page: Page {
                lines: vec![line("Password: hunter2", 0.0)],
            },
        };
        let found = secrets_on(&entry);
        // "Password:" is 9 letters at x=10; the value starts after it.
        assert_eq!(found.len(), 1);
        assert!(found[0].1[0] > 100.0);
    }

    #[test]
    fn one_find_spans_every_sample_it_stays_on_screen() {
        let sample = |t: f64| Entry {
            screen: "screen1.mp4".into(),
            t,
            until: t,
            page: Page {
                lines: vec![line("mail me at sam@example.com", 300.0)],
            },
        };
        let index = TextIndex {
            format: crate::text::TEXT_FORMAT.into(),
            version: crate::text::TEXT_VERSION,
            interval: 2.0,
            entries: vec![sample(4.0), sample(6.0), sample(8.0), sample(30.0)],
        };
        let found = find(&index, |_| Some((1920, 1080)));
        assert_eq!(found.len(), 2, "{found:?}");
        // From a sample before it was read to a sample after it was last seen.
        assert_eq!((found[0].start, found[0].end), (2.0, 10.0));
        assert_eq!((found[1].start, found[1].end), (28.0, 32.0));
        let a = found[0].area;
        assert!(a.x > 0.0 && a.x + a.w < 1.0 && a.w > 0.0 && a.h > 0.0);
    }

    #[test]
    fn text_on_an_unknown_screen_is_skipped() {
        let index = TextIndex {
            format: crate::text::TEXT_FORMAT.into(),
            version: crate::text::TEXT_VERSION,
            interval: 2.0,
            entries: vec![Entry {
                screen: "gone.mp4".into(),
                t: 0.0,
                until: 0.0,
                page: Page {
                    lines: vec![line("sam@example.com", 0.0)],
                },
            }],
        };
        assert!(find(&index, |_| None).is_empty());
    }
}
