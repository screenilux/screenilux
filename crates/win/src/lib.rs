//! Windows adapters for Screenilux.
//!
//! Everything platform-specific lives here, behind the traits in
//! `screenilux_core::ports`. This is the only crate that needs `unsafe`, and
//! each use is confined to a single Win32 call with the reason stated.

pub mod camera;
pub mod capture;
pub mod display;
pub mod input;
pub mod window;

pub use camera::{CameraFeed, CameraHandle, cameras};
pub use capture::ScreenCapture;
pub use display::{Displays, monitors, set_dpi_awareness};
pub use input::{Hotkey, InputHook, cursor_pos};
pub use window::{exclude_from_capture, include_in_capture};
