//! The operating system, behind one set of names.
//!
//! The recorder, the player and the app reach the platform only through this
//! crate. Each backend, `screenilux-win` on Windows and `screenilux-linux` on
//! Linux, provides the same modules and items, and this crate picks one at
//! compile time, so nothing above it ever names an operating system.
//!
//! What every backend provides:
//!
//! - `audio`: the PCM format the encoder takes (`SAMPLE_RATE`, `CHANNELS`,
//!   `BITS_PER_SAMPLE`), [`AudioCapture`] for the microphone and the
//!   system's own sound, [`has_microphone`] and [`has_system_sound`].
//! - `capture`: [`ScreenCapture`], one display as a
//!   [`ScreenSource`](screenilux_core::ports::ScreenSource).
//! - `encoder`: [`NativeSink`], H.264 and AAC into an MP4, as a
//!   [`VideoSink`](screenilux_core::ports::VideoSink), and its label.
//! - `decoder`: [`VideoDecoder`], [`AudioDecoder`] and [`probe`] for playing
//!   recordings back, with the playback PCM format.
//! - `display`: [`monitors`], the work area, DPI setup.
//! - [`Speaker`], [`CameraHandle`] and [`cameras`], [`InputHook`] and
//!   [`cursor_pos`], [`TextReader`] for the text on screen, [`thumbnail`],
//!   [`DisplayWatcher`], [`now_local`], file types and [`reveal`].
//! - The window's look and visibility: [`exclude_from_capture`] and
//!   [`CAN_HIDE_FROM_CAPTURE`], opacity and rounded or circular shapes.
//!
//! A backend that cannot do something says so through these same names (an
//! error, `false`, or a call that changes nothing) rather than leaving a name
//! out, so the code above compiles the same everywhere.

#[cfg(windows)]
use screenilux_win as backend;

#[cfg(target_os = "linux")]
use screenilux_linux as backend;

#[cfg(not(any(windows, target_os = "linux")))]
compile_error!("Screenilux runs on Windows and Linux.");

pub use backend::{
    AudioCapture, AudioDecoder, CAN_HIDE_FROM_CAPTURE, CameraFeed, CameraHandle, DisplayWatcher,
    InputHook, MediaInfo, NativeSink, ScreenCapture, Speaker, TextReader, Thumbnail, VideoDecoder,
    VideoFrame, cameras, cursor_pos, exclude_from_capture, find_window, has_microphone,
    has_system_sound, include_in_capture, monitors, now_local, probe, register_file_types, reveal,
    set_circular, set_dpi_awareness, set_opacity, set_rounded, thumbnail,
};
pub use backend::{audio, capture, decoder, display, encoder};
