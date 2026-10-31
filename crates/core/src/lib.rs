//! Pure domain logic for Screenilux.
//!
//! This crate deliberately depends on neither Windows, nor ffmpeg, nor any UI
//! toolkit. Everything here runs — and is tested — without a screen, a camera
//! or an encoder, and the future session player can reuse it as-is.
//!
//! The platform-specific pieces plug in through [`ports`].

#![forbid(unsafe_code)]

pub mod focus;
pub mod geometry;
pub mod ports;
pub mod session;
pub mod timing;

pub use focus::{DEFAULT_DWELL, FocusTracker, Switch};
pub use geometry::{Monitor, Point, Rect, monitor_at, virtual_bounds};
pub use session::{Focus, FocusEntry, Manifest, Role, SESSION_VERSION, Screen};
pub use timing::{FramePacer, Rendezvous, SyncClock};
