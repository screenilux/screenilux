//! Windows adapters for Screenilux.
//!
//! The Windows backend of `screenilux-platform`, which is how the rest of the
//! app reaches it. Much of it sits behind the traits in
//! `screenilux_core::ports`. With the Linux backend, this is one of the only
//! crates that needs `unsafe`, and each use is confined to a single Win32 call
//! with the reason stated.
//!
//! Empty on any other system, so the whole workspace builds everywhere.

#![cfg(windows)]

pub mod audio;
pub mod camera;
pub mod capture;
pub mod clock;
pub mod decoder;
pub mod display;
pub mod encoder;
pub mod input;
pub mod ocr;
pub mod shell;
pub mod snapshot;
pub mod speaker;
pub mod watch;
pub mod window;

pub use audio::{AudioCapture, has_microphone, has_system_sound};
pub use camera::{CameraFeed, CameraHandle, cameras};
pub use capture::ScreenCapture;
pub use clock::now_local;
pub use decoder::{AudioDecoder, MediaInfo, VideoDecoder, VideoFrame, probe};
pub use display::{Displays, monitors, set_dpi_awareness};
pub use encoder::{ENCODER_LABEL, NativeSink};
pub use input::{InputHook, cursor_pos};
pub use ocr::TextReader;
pub use shell::{register_file_types, reveal};
pub use snapshot::{Thumbnail, thumbnail};
pub use speaker::Speaker;
pub use watch::DisplayWatcher;
pub use window::{
    CAN_HIDE_FROM_CAPTURE, exclude_from_capture, find_window, include_in_capture, set_circular,
    set_opacity, set_rounded,
};
