//! Playback for Screenilux.
//!
//! The counterpart to the recorder: one decode thread per screen, all
//! following a single clock so the screens stay in step, plus sound and the
//! text read off the video. The pure parts — layouts, the clock, the text
//! model, the package format — live in `core`; this crate runs them.

#![forbid(unsafe_code)]

pub mod engine;
pub mod open;
pub mod text;

pub use engine::{Feed, Player, SharedClock};
pub use open::{Opened, Origin, open};
pub use text::{Indexer, TextWorker};
