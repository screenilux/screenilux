//! Playing a recording's sound through GStreamer's default output.

use gstreamer as gst;
use gstreamer::prelude::*;
use gstreamer_app as gst_app;

use crate::decoder::{PLAYBACK_BLOCK, PLAYBACK_RATE};
use crate::gst as g;

/// How much may wait in the output: enough to ride out a busy moment, little
/// enough that a pause or seek is heard at once.
const QUEUE_BYTES: u64 = PLAYBACK_RATE as u64 / 5 * PLAYBACK_BLOCK as u64; // 200ms

/// An open playback stream.
pub struct Speaker {
    pipeline: gst::Pipeline,
    src: gst_app::AppSrc,
    /// Sample frames queued since the last start, for timestamps.
    frames: u64,
    running: bool,
}

impl Speaker {
    pub fn open() -> Result<Self, String> {
        let pipeline = g::pipeline(&format!(
            "appsrc name=src is-live=true format=time caps={} ! \
             audioconvert ! audioresample ! autoaudiosink",
            crate::audio::PCM_CAPS
        ))?;
        let src: gst_app::AppSrc = g::element(&pipeline, "src")?;
        src.set_max_bytes(QUEUE_BYTES);
        g::set_state(&pipeline, gst::State::Ready).map_err(|e| format!("no speakers: {e}"))?;
        Ok(Self {
            pipeline,
            src,
            frames: 0,
            running: false,
        })
    }

    /// Bytes the output can take right now.
    pub fn room(&self) -> usize {
        let queued = self.src.current_level_bytes();
        (QUEUE_BYTES.saturating_sub(queued) as usize) / PLAYBACK_BLOCK * PLAYBACK_BLOCK
    }

    /// Queue as much of `pcm` as fits. Returns the bytes taken, always whole
    /// sample frames.
    pub fn write(&mut self, pcm: &[u8]) -> usize {
        let bytes = self.room().min(pcm.len()) / PLAYBACK_BLOCK * PLAYBACK_BLOCK;
        if bytes == 0 {
            return 0;
        }
        let frames = (bytes / PLAYBACK_BLOCK) as u64;
        let mut buffer = gst::Buffer::from_slice(pcm[..bytes].to_vec());
        if let Some(b) = buffer.get_mut() {
            b.set_pts(gst::ClockTime::from_nseconds(
                self.frames * 1_000_000_000 / PLAYBACK_RATE as u64,
            ));
            b.set_duration(gst::ClockTime::from_nseconds(
                frames * 1_000_000_000 / PLAYBACK_RATE as u64,
            ));
        }
        if self.src.push_buffer(buffer).is_err() {
            return 0;
        }
        self.frames += frames;
        bytes
    }

    pub fn start(&mut self) {
        if !self.running {
            self.running = g::set_state(&self.pipeline, gst::State::Playing).is_ok();
        }
    }

    /// Stop and throw away whatever was queued, so a pause or seek is heard
    /// immediately.
    pub fn stop(&mut self) {
        let _ = self.pipeline.set_state(gst::State::Ready);
        self.frames = 0;
        self.running = false;
    }
}

impl Drop for Speaker {
    fn drop(&mut self) {
        let _ = self.pipeline.set_state(gst::State::Null);
    }
}
