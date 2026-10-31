//! Recording orchestration for Screenilux.
//!
//! Ties the pure logic in `core` to the Windows and ffmpeg adapters: one
//! capture thread and one encoder per screen, a shared clock so every screen
//! numbers frames from the same instant, and one run state so pause and stop
//! reach them together.

#![forbid(unsafe_code)]

pub mod control;
pub mod screen;
pub mod session;

pub use control::{Control, RunState};
pub use session::{Session, SessionConfig, SessionError};
