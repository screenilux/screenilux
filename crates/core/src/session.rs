//! The `screenrec-session` manifest.
//!
//! A multi-screen recording is a folder of per-screen MP4s plus this manifest.
//! It is the contract between the recorder, the future player and the importer,
//! so the field names and shapes here are load-bearing: they must stay
//! byte-compatible with what the Python build emits.

use serde::{Deserialize, Serialize};

pub const SESSION_FORMAT: &str = "screenrec-session";
pub const SESSION_VERSION: u32 = 2;
pub const MANIFEST_NAME: &str = "recording.json";

/// Whether a screen is the OS primary. Serialises as `"primary"`/`"secondary"`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    Primary,
    Secondary,
}

/// One recorded screen.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Screen {
    /// Filename within the session folder, e.g. `screen1.mp4`.
    pub file: String,
    pub label: String,
    pub width: u32,
    pub height: u32,
    /// Virtual-desktop coordinates, so a player can rebuild the real layout.
    pub left: i32,
    pub top: i32,
    pub primary: bool,
    /// Whether this file carries the microphone track. Only one does: Windows
    /// gives the capture device a single consumer.
    pub audio: bool,
    pub role: Role,
    /// Display priority for a split view; the primary leads, then capture
    /// order. `1` is shown first / largest.
    pub order: u32,
}

/// One entry in the auto-switch timeline.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FocusEntry {
    /// Recorded seconds, excluding paused time.
    pub t: f64,
    /// The `file` of the screen to show from `t` onwards.
    pub screen: String,
}

/// Pointer-driven auto-switch data, present only for multi-screen sessions.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Focus {
    pub source: String,
    pub dwell: f64,
    pub note: String,
    pub timeline: Vec<FocusEntry>,
}

impl Focus {
    pub const NOTE: &'static str =
        "Auto-switch playback: at time t (recorded seconds), show this screen. Already debounced.";

    pub fn new(dwell: f64, timeline: Vec<FocusEntry>) -> Self {
        Self {
            source: "pointer".to_string(),
            dwell,
            note: Self::NOTE.to_string(),
            timeline,
        }
    }

    /// Which screen file should be on screen at `t` seconds.
    pub fn screen_at(&self, t: f64) -> Option<&str> {
        self.timeline
            .iter()
            .take_while(|e| e.t <= t)
            .last()
            .map(|e| e.screen.as_str())
    }
}

/// The root manifest object.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Manifest {
    pub format: String,
    pub version: u32,
    /// Local time, `%Y-%m-%dT%H:%M:%S`, no timezone suffix.
    pub created: String,
    pub duration: f64,
    pub fps: u32,
    /// Human-readable encoder name actually used, e.g. `"Quick Sync"`.
    pub encoder: String,
    pub camera: bool,
    pub screens: Vec<Screen>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub focus: Option<Focus>,
}

#[derive(Debug, thiserror::Error)]
pub enum ManifestError {
    #[error("not a {SESSION_FORMAT} manifest (found {0:?})")]
    WrongFormat(String),
    #[error("unsupported manifest version {0} (this build understands {SESSION_VERSION})")]
    UnsupportedVersion(u32),
    #[error("manifest lists no screens")]
    NoScreens,
    #[error(transparent)]
    Json(#[from] serde_json::Error),
}

impl Manifest {
    pub fn new(created: String, duration: f64, fps: u32, encoder: String, camera: bool) -> Self {
        Self {
            format: SESSION_FORMAT.to_string(),
            version: SESSION_VERSION,
            created,
            duration,
            fps,
            encoder,
            camera,
            screens: Vec::new(),
            focus: None,
        }
    }

    /// Assign `order` so the primary screen leads and the rest keep capture
    /// order. Call after all screens have been pushed.
    pub fn assign_order(&mut self) {
        let mut ranked: Vec<usize> = (0..self.screens.len()).collect();
        // stable sort keeps capture order within each role
        ranked.sort_by_key(|&i| self.screens[i].role != Role::Primary);
        for (rank, i) in ranked.into_iter().enumerate() {
            self.screens[i].order = rank as u32 + 1;
        }
    }

    /// Screens in display priority.
    pub fn by_order(&self) -> Vec<&Screen> {
        let mut v: Vec<&Screen> = self.screens.iter().collect();
        v.sort_by_key(|s| s.order);
        v
    }

    pub fn to_json(&self) -> Result<String, ManifestError> {
        Ok(serde_json::to_string_pretty(self)?)
    }

    /// Parse and validate. Rejects foreign or future manifests rather than
    /// half-reading them.
    pub fn from_json(text: &str) -> Result<Self, ManifestError> {
        let m: Manifest = serde_json::from_str(text)?;
        if m.format != SESSION_FORMAT {
            return Err(ManifestError::WrongFormat(m.format));
        }
        if m.version > SESSION_VERSION {
            return Err(ManifestError::UnsupportedVersion(m.version));
        }
        if m.screens.is_empty() {
            return Err(ManifestError::NoScreens);
        }
        Ok(m)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn screen(file: &str, left: i32, top: i32, primary: bool) -> Screen {
        Screen {
            file: file.to_string(),
            label: file.trim_end_matches(".mp4").to_string(),
            width: 1920,
            height: 1080,
            left,
            top,
            primary,
            audio: false,
            role: if primary {
                Role::Primary
            } else {
                Role::Secondary
            },
            order: 0,
        }
    }

    fn sample() -> Manifest {
        let mut m = Manifest::new(
            "2026-10-31T12:12:49".to_string(),
            5.08,
            30,
            "Quick Sync".to_string(),
            false,
        );
        m.screens.push(screen("screen1.mp4", 1920, 122, false));
        m.screens.push(screen("screen2.mp4", 0, 0, true));
        m.assign_order();
        m.focus = Some(Focus::new(
            DEFAULT_DWELL_FOR_TEST,
            vec![
                FocusEntry {
                    t: 0.0,
                    screen: "screen1.mp4".into(),
                },
                FocusEntry {
                    t: 1.539,
                    screen: "screen2.mp4".into(),
                },
                FocusEntry {
                    t: 4.222,
                    screen: "screen1.mp4".into(),
                },
            ],
        ));
        m
    }

    const DEFAULT_DWELL_FOR_TEST: f64 = 0.6;

    #[test]
    fn round_trips() {
        let m = sample();
        let back = Manifest::from_json(&m.to_json().unwrap()).unwrap();
        assert_eq!(m, back);
    }

    #[test]
    fn primary_leads_the_display_order() {
        let m = sample();
        let ordered = m.by_order();
        assert_eq!(ordered[0].file, "screen2.mp4");
        assert_eq!(ordered[0].role, Role::Primary);
        assert_eq!(ordered[0].order, 1);
        assert_eq!(ordered[1].file, "screen1.mp4");
        assert_eq!(ordered[1].order, 2);
    }

    #[test]
    fn order_keeps_capture_order_among_secondaries() {
        let mut m = Manifest::new("t".into(), 1.0, 30, "e".into(), false);
        m.screens.push(screen("screen1.mp4", 0, 0, false));
        m.screens.push(screen("screen2.mp4", 1920, 0, false));
        m.screens.push(screen("screen3.mp4", 3840, 0, true));
        m.assign_order();
        let ordered: Vec<_> = m.by_order().iter().map(|s| s.file.clone()).collect();
        assert_eq!(ordered, vec!["screen3.mp4", "screen1.mp4", "screen2.mp4"]);
    }

    #[test]
    fn roles_serialise_lowercase_like_the_python_build() {
        let json = sample().to_json().unwrap();
        assert!(json.contains("\"role\": \"primary\""));
        assert!(json.contains("\"role\": \"secondary\""));
    }

    #[test]
    fn focus_is_omitted_when_absent_rather_than_null() {
        let mut m = sample();
        m.focus = None;
        assert!(!m.to_json().unwrap().contains("focus"));
    }

    #[test]
    fn screen_at_resolves_the_auto_switch_view() {
        let m = sample();
        let f = m.focus.as_ref().unwrap();
        assert_eq!(f.screen_at(0.0), Some("screen1.mp4"));
        assert_eq!(f.screen_at(1.0), Some("screen1.mp4"));
        assert_eq!(f.screen_at(1.539), Some("screen2.mp4"));
        assert_eq!(f.screen_at(3.0), Some("screen2.mp4"));
        assert_eq!(f.screen_at(99.0), Some("screen1.mp4"));
    }

    #[test]
    fn foreign_and_future_manifests_are_rejected() {
        let mut m = sample();
        m.format = "something-else".into();
        assert!(matches!(
            Manifest::from_json(&serde_json::to_string(&m).unwrap()),
            Err(ManifestError::WrongFormat(_))
        ));

        let mut m = sample();
        m.version = SESSION_VERSION + 1;
        assert!(matches!(
            Manifest::from_json(&serde_json::to_string(&m).unwrap()),
            Err(ManifestError::UnsupportedVersion(_))
        ));

        let mut m = sample();
        m.screens.clear();
        assert!(matches!(
            Manifest::from_json(&serde_json::to_string(&m).unwrap()),
            Err(ManifestError::NoScreens)
        ));
    }

    /// Verbatim output from the Python build, to prove the formats match.
    #[test]
    fn parses_a_manifest_written_by_the_python_build() {
        let text = r#"{
  "format": "screenrec-session",
  "version": 2,
  "created": "2026-10-31T12:12:49",
  "duration": 5.08,
  "fps": 30,
  "encoder": "Quick Sync",
  "camera": false,
  "screens": [
    {
      "file": "screen1.mp4",
      "label": "Screen 1",
      "width": 1920,
      "height": 1080,
      "left": 1920,
      "top": 122,
      "primary": false,
      "audio": false,
      "role": "secondary",
      "order": 2
    },
    {
      "file": "screen2.mp4",
      "label": "Screen 2",
      "width": 1920,
      "height": 1080,
      "left": 0,
      "top": 0,
      "primary": true,
      "audio": true,
      "role": "primary",
      "order": 1
    }
  ],
  "focus": {
    "source": "pointer",
    "dwell": 0.6,
    "note": "Auto-switch playback: at time t (recorded seconds), show this screen. Already debounced.",
    "timeline": [
      { "t": 0.0, "screen": "screen1.mp4" },
      { "t": 1.539, "screen": "screen2.mp4" }
    ]
  }
}"#;
        let m = Manifest::from_json(text).expect("python manifest must parse");
        assert_eq!(m.version, SESSION_VERSION);
        assert_eq!(m.screens.len(), 2);
        assert_eq!(m.by_order()[0].file, "screen2.mp4");
        assert_eq!(m.screens[1].role, Role::Primary);
        assert!(m.screens[1].audio);
        let f = m.focus.unwrap();
        assert_eq!(f.note, Focus::NOTE, "note text must match exactly");
        assert_eq!(f.timeline.len(), 2);
    }
}
