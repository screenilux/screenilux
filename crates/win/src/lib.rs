//! Windows adapters for Screenilux.
//!
//! Everything platform-specific lives here, behind the traits in
//! `screenilux_core::ports`. This is the only crate that needs `unsafe`, and
//! each use is confined to a single Win32 call with the reason stated.

pub mod capture;
pub mod display;
pub mod input;

pub use capture::ScreenCapture;
pub use display::{Displays, monitors, set_dpi_awareness};
pub use input::{Hotkey, InputHook, cursor_pos};
