//! Linux adapters for Screenilux.
//!
//! The Linux backend of `screenilux-platform`, which is how the rest of the
//! app reaches it. It provides the same names as the Windows backend, built
//! on what a Linux desktop already has:
//!
//! - **X11** for the screens, capturing them, the pointer and global clicks.
//!   Wayland sessions are served through XWayland; native Wayland capture
//!   goes through a permission portal and is not done here yet.
//! - **GStreamer** for everything media: H.264 and AAC into MP4, decoding,
//!   the microphone, the system's own sound, the speakers and the camera.
//! - **Tesseract** for reading the text on screen, with word positions.
//! - The **XDG** conventions for file types and opening folders.
//!
//! Empty on any other system, so the whole workspace builds everywhere.

#![cfg(target_os = "linux")]

pub mod audio;
pub mod camera;
pub mod capture;
pub mod clock;
pub mod decoder;
pub mod display;
pub mod encoder;
mod gst;
pub mod input;
pub mod ocr;
pub mod shell;
pub mod snapshot;
pub mod speaker;
pub mod watch;
pub mod window;
mod x11;

pub use audio::{AudioCapture, has_microphone, has_system_sound};
pub use camera::{CameraFeed, CameraHandle, cameras};
pub use capture::ScreenCapture;
pub use clock::now_local;
pub use decoder::{AudioDecoder, MediaInfo, VideoDecoder, VideoFrame, probe};
pub use display::{monitors, set_dpi_awareness};
pub use encoder::{ENCODER_LABEL, NativeSink};
pub use input::{InputHook, cursor_pos};
pub use ocr::TextReader;
pub use shell::{register_file_types, reveal};
pub use snapshot::{Thumbnail, thumbnail};
pub use speaker::Speaker;
pub use watch::DisplayWatcher;
pub use window::{
    CAN_HIDE_FROM_CAPTURE, exclude_from_capture, find_window, include_in_capture, set_opacity,
    set_pill_shape, set_rounded,
};
