//! Shortcuts pressed while recording, for showing over the picture.
//!
//! A tutorial is easier to follow when the keys pressed are shown, since a
//! shortcut changes the screen with nothing on it to say how. Only
//! shortcuts are kept: a key pressed with Ctrl, Alt or the Windows (Super)
//! key held, or one of the few keys that act alone, such as Enter or Escape.
//! Ordinary typing, with or without Shift, is never kept, so a password or
//! a message typed while recording cannot end up in the recording.

use serde::{Deserialize, Serialize};

pub const CTRL: u8 = 1;
pub const ALT: u8 = 2;
pub const SHIFT: u8 = 4;
/// The Windows key, or Super on Linux.
pub const SUPER: u8 = 8;

/// A key, as far as showing it needs to know.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Key {
    Letter(char),
    Digit(u8),
    F(u8),
    Enter,
    Escape,
    Tab,
    Space,
    Backspace,
    Delete,
    Up,
    Down,
    Left,
    Right,
    Home,
    End,
    PageUp,
    PageDown,
    /// Punctuation and the rest, by what it types.
    Symbol(char),
}

impl Key {
    fn name(self) -> String {
        match self {
            Key::Letter(c) => c.to_ascii_uppercase().to_string(),
            Key::Digit(d) => d.to_string(),
            Key::F(n) => format!("F{n}"),
            Key::Enter => "Enter".into(),
            Key::Escape => "Esc".into(),
            Key::Tab => "Tab".into(),
            Key::Space => "Space".into(),
            Key::Backspace => "Backspace".into(),
            Key::Delete => "Delete".into(),
            Key::Up => "↑".into(),
            Key::Down => "↓".into(),
            Key::Left => "←".into(),
            Key::Right => "→".into(),
            Key::Home => "Home".into(),
            Key::End => "End".into(),
            Key::PageUp => "Page Up".into(),
            Key::PageDown => "Page Down".into(),
            Key::Symbol(c) => c.to_string(),
        }
    }

    /// Keys that do something on their own, worth showing without a
    /// modifier held.
    fn acts_alone(self) -> bool {
        matches!(
            self,
            Key::F(_)
                | Key::Enter
                | Key::Escape
                | Key::Tab
                | Key::Up
                | Key::Down
                | Key::Left
                | Key::Right
                | Key::PageUp
                | Key::PageDown
        )
    }
}

/// A shortcut pressed while recording: when, and how it reads.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Shortcut {
    /// Seconds into the recording.
    pub t: f64,
    pub keys: String,
}

/// How a key pressed with `mods` held reads, if it is a shortcut worth
/// showing. Anything that types text is not.
pub fn shortcut(mods: u8, key: Key) -> Option<String> {
    let commanding = mods & (CTRL | ALT | SUPER) != 0;
    if !commanding && !key.acts_alone() {
        return None;
    }
    let mut parts: Vec<&str> = Vec::new();
    if mods & CTRL != 0 {
        parts.push("Ctrl");
    }
    if mods & ALT != 0 {
        parts.push("Alt");
    }
    if mods & SHIFT != 0 {
        parts.push("Shift");
    }
    if mods & SUPER != 0 {
        parts.push(if cfg!(windows) { "Win" } else { "Super" });
    }
    let name = key.name();
    parts.push(&name);
    Some(parts.join(" + "))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn combinations_read_the_way_people_say_them() {
        assert_eq!(
            shortcut(CTRL | SHIFT, Key::Letter('p')).as_deref(),
            Some("Ctrl + Shift + P")
        );
        assert_eq!(shortcut(ALT, Key::Tab).as_deref(), Some("Alt + Tab"));
        assert_eq!(shortcut(CTRL, Key::Digit(1)).as_deref(), Some("Ctrl + 1"));
    }

    #[test]
    fn keys_that_act_alone_are_shown() {
        assert_eq!(shortcut(0, Key::Enter).as_deref(), Some("Enter"));
        assert_eq!(shortcut(0, Key::Escape).as_deref(), Some("Esc"));
        assert_eq!(shortcut(0, Key::F(5)).as_deref(), Some("F5"));
    }

    #[test]
    fn typing_is_never_kept() {
        // Letters, digits, spaces and punctuation, with or without Shift.
        for key in [
            Key::Letter('a'),
            Key::Digit(7),
            Key::Space,
            Key::Symbol('!'),
            Key::Backspace,
        ] {
            assert_eq!(shortcut(0, key), None, "{key:?}");
            assert_eq!(shortcut(SHIFT, key), None, "Shift + {key:?}");
        }
    }
}
