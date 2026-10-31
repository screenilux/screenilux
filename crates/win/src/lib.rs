//! Windows adapters for Screenilux.
//!
//! Everything platform-specific lives here, behind the traits in
//! `screenilux_core::ports`. This is the only crate that needs `unsafe`, and
//! each use is confined to a single Win32 call with the reason stated.

pub mod audio;
pub mod camera;
pub mod capture;
pub mod clock;
pub mod decoder;
pub mod display;
pub mod encoder;
pub mod input;
pub mod install;
pub mod ocr;
pub mod shell;
pub mod speaker;
pub mod watch;
pub mod window;

pub use audio::{AudioCapture, has_microphone};
pub use camera::{CameraFeed, CameraHandle, cameras};
pub use capture::ScreenCapture;
pub use clock::now_local;
pub use decoder::{AudioDecoder, MediaInfo, VideoDecoder, VideoFrame, probe};
pub use display::{Displays, monitors, set_dpi_awareness};
pub use encoder::{ENCODER_LABEL, NativeSink};
pub use input::{InputHook, cursor_pos};
pub use ocr::TextReader;
pub use shell::register_file_types;
pub use speaker::Speaker;
pub use watch::DisplayWatcher;
pub use window::{exclude_from_capture, include_in_capture, set_opacity, set_rounded};
