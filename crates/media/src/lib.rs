//! ffmpeg process management for Screenilux.
//!
//! The recorder never touches ffmpeg directly: it writes composited BGRA
//! frames into a [`VideoSink`](screenilux_core::ports::VideoSink), and this
//! crate turns that into child processes, segments and a finished MP4.
//!
//! Keeping ffmpeg as a child process rather than linking libav is deliberate.
//! It needs no C toolchain, it isolates encoder crashes from the app, and it
//! is the configuration already proven by the Python build.

#![forbid(unsafe_code)]

pub mod command;
pub mod probe;
pub mod proc;
pub mod segment;
pub mod util;

pub use command::{CPU_X264, EncoderSpec, NVENC, QUICK_SYNC, SegmentParams};
pub use probe::{Devices, detect, list_devices};
pub use proc::locate_ffmpeg;
pub use segment::FfmpegSink;
pub use util::{TempDir, unique_path};
