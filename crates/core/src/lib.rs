//! Pure domain logic for Screenilux.
//!
//! This crate deliberately depends on neither Windows nor any UI toolkit. Everything here runs — and is tested — without a screen, a camera
//! or an encoder, and the recorder and the player share it.
//!
//! The platform-specific pieces plug in through [`ports`].

#![forbid(unsafe_code)]

pub mod compositor;
pub mod focus;
pub mod geometry;
pub mod package;
pub mod playback;
pub mod ports;
pub mod session;
pub mod text;
pub mod timing;
pub mod yuv;

pub use compositor::{CameraView, Compositor, EffectOptions};
pub use focus::{DEFAULT_DWELL, FocusTracker, Switch};
pub use geometry::{Monitor, Point, Rect, monitor_at, virtual_bounds};
pub use playback::{Area, Layout, PlaybackClock, Recording, Track, arrange};
pub use session::{Focus, FocusEntry, Manifest, Role, SESSION_VERSION, Screen};
pub use timing::{FramePacer, Rendezvous, SyncClock};
