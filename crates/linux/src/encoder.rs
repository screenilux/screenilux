//! H.264 and AAC into an MP4 through GStreamer.
//!
//! The same contract as the Windows encoder: frames are numbered from one
//! running count, so paused time contributes no frames and does not exist
//! in the output, and sound is placed by counting samples. There are no
//! segments and no join pass.
//!
//! The H.264 encoder is the first one installed of x264, OpenH264 and the
//! VA-API hardware encoders; the AAC encoder likewise.

use std::path::{Path, PathBuf};
use std::time::Duration;

use gstreamer as gst;
use gstreamer::prelude::*;
use gstreamer_app as gst_app;
use screenilux_core::ports::{SinkError, VideoSink};

use crate::audio::{BITS_PER_SAMPLE, CHANNELS, PCM_CAPS, SAMPLE_RATE};
use crate::gst as g;

pub const ENCODER_LABEL: &str = "GStreamer (H.264)";

/// The same budget as on Windows: about 4 bits per pixel per frame at 30fps,
/// scaling with area and frame rate, which keeps small text crisp.
fn bitrate_for(width: u32, height: u32, fps: u32) -> u32 {
    let pixels = width as u64 * height as u64;
    let scaled = pixels * fps as u64 / 30;
    ((scaled * 4) as u32).clamp(2_000_000, 40_000_000)
}

/// The video encoder to use, configured, or `None` if none is installed.
fn video_encoder(width: u32, height: u32, fps: u32) -> Option<String> {
    let bps = bitrate_for(width, height, fps);
    let kbps = bps / 1000;
    let keyframes = fps * 2;
    match g::first_of(&["x264enc", "openh264enc", "vah264enc", "vaapih264enc"])? {
        "x264enc" => Some(format!(
            "x264enc speed-preset=veryfast tune=zerolatency bitrate={kbps} key-int-max={keyframes}"
        )),
        "openh264enc" => Some(format!("openh264enc bitrate={bps} gop-size={keyframes}")),
        other => Some(format!("{other} bitrate={kbps}")),
    }
}

fn audio_encoder() -> Option<&'static str> {
    g::first_of(&["avenc_aac", "fdkaacenc", "voaacenc"])
}

pub struct NativeSink {
    pipeline: Option<gst::Pipeline>,
    video: Option<gst_app::AppSrc>,
    audio_src: Option<gst_app::AppSrc>,
    /// Where the encoder writes. Moved to the caller's destination on finish.
    scratch: PathBuf,
    width: u32,
    height: u32,
    fps: u32,
    /// Monotonic across pauses.
    frames: u64,
    /// Sample frames of sound written.
    samples: u64,
    /// The last frame, sent again to top up a stretch.
    last: Vec<u8>,
    has_frame: bool,
    audio: bool,
}

impl NativeSink {
    pub fn new(
        width: u32,
        height: u32,
        fps: u32,
        audio: bool,
        scratch: PathBuf,
    ) -> Result<Self, SinkError> {
        Ok(Self {
            pipeline: None,
            video: None,
            audio_src: None,
            scratch,
            width,
            height,
            fps: fps.max(1),
            frames: 0,
            samples: 0,
            last: Vec::new(),
            has_frame: false,
            audio,
        })
    }

    fn ensure_encoder(&mut self) -> Result<(), SinkError> {
        if self.pipeline.is_some() {
            return Ok(());
        }
        if let Some(parent) = self.scratch.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let start = |e: String| SinkError::StartFailed(e);
        let encoder = video_encoder(self.width, self.height, self.fps).ok_or_else(|| {
            start("no H.264 encoder is installed: add GStreamer's ugly or bad plugins".into())
        })?;
        // H.264 in 4:2:0 wants even sides; an odd screen loses its last line.
        let (w, h) = (self.width & !1, self.height & !1);
        let mut description = format!(
            "appsrc name=video format=time block=true caps=video/x-raw,format=BGRA,width={},height={},framerate={}/1 ! \
             videoconvert ! videocrop right={} bottom={} ! video/x-raw,format=I420,width={w},height={h} ! \
             {encoder} ! h264parse ! queue ! mp4mux name=mux faststart=true ! filesink location={}",
            self.width,
            self.height,
            self.fps,
            self.width - w,
            self.height - h,
            g::quoted(&self.scratch),
        );
        if self.audio {
            let aac = audio_encoder().ok_or_else(|| {
                start("no AAC encoder is installed: add GStreamer's libav plugins".into())
            })?;
            description.push_str(&format!(
                " appsrc name=audio format=time block=true caps={PCM_CAPS} ! audioconvert ! {aac} ! aacparse ! queue ! mux."
            ));
        }
        let pipeline = g::pipeline(&description).map_err(start)?;
        let video: gst_app::AppSrc = g::element(&pipeline, "video").map_err(start)?;
        // A few frames of queue: enough to keep the encoder busy, and the
        // recorder waits rather than piling frames up in memory.
        video.set_max_bytes(self.width as u64 * self.height as u64 * 4 * 3);
        let audio_src = if self.audio {
            Some(g::element::<gst_app::AppSrc>(&pipeline, "audio").map_err(start)?)
        } else {
            None
        };
        // Not waited on: the pipeline only finishes starting once the first
        // frame arrives, which is the caller's next step. Failures surface on
        // the bus and at the first write.
        pipeline.set_state(gst::State::Playing).map_err(|_| {
            start(g::error_so_far(&pipeline).unwrap_or_else(|| "could not start".into()))
        })?;
        self.pipeline = Some(pipeline);
        self.video = Some(video);
        self.audio_src = audio_src;
        Ok(())
    }

    fn send_last(&mut self) -> Result<(), SinkError> {
        let video = self
            .video
            .as_ref()
            .ok_or_else(|| died("the encoder is not running"))?;
        let mut buffer = gst::Buffer::from_slice(self.last.clone());
        if let Some(b) = buffer.get_mut() {
            b.set_pts(gst::ClockTime::from_nseconds(
                self.frames * 1_000_000_000 / self.fps as u64,
            ));
            b.set_duration(gst::ClockTime::from_nseconds(
                1_000_000_000 / self.fps as u64,
            ));
        }
        video
            .push_buffer(buffer)
            .map_err(|e| died(&format!("{e:?}")))?;
        self.frames += 1;
        Ok(())
    }
}

fn died(e: &str) -> SinkError {
    SinkError::EncoderDied(e.to_string())
}

impl VideoSink for NativeSink {
    /// The first call starts the encoder; after a pause it is a no-op, as
    /// the same encoder continues.
    fn open_segment(&mut self) -> Result<(), SinkError> {
        self.ensure_encoder()
    }

    fn write(&mut self, frame: &[u8]) -> Result<(), SinkError> {
        let expected = self.width as usize * self.height as usize * 4;
        if frame.len() != expected {
            return Err(died(&format!(
                "frame is {} bytes, expected {expected}",
                frame.len()
            )));
        }
        self.ensure_encoder()?;
        self.last.clear();
        self.last.extend_from_slice(frame);
        self.send_last()?;
        self.has_frame = true;
        Ok(())
    }

    /// Top up the owed frames; a pause is not a boundary in the output.
    fn close_segment(&mut self, pad: u64) -> Result<(), SinkError> {
        if !self.has_frame {
            return Ok(());
        }
        for _ in 0..pad {
            self.send_last()?;
        }
        Ok(())
    }

    fn finish(&mut self, dest: &Path) -> Result<(), SinkError> {
        let Some(pipeline) = self.pipeline.take() else {
            return Err(died("nothing was recorded"));
        };
        for src in [self.video.take(), self.audio_src.take()]
            .into_iter()
            .flatten()
        {
            let _ = src.end_of_stream();
        }
        let ended = g::wait_for_end(&pipeline, Duration::from_secs(60));
        let _ = pipeline.set_state(gst::State::Null);
        ended.map_err(|e| died(&e))?;

        if !self.has_frame {
            let _ = std::fs::remove_file(&self.scratch);
            return Err(died("nothing was recorded"));
        }
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent)?;
        }
        // A rename across file systems fails, so fall back to a copy.
        if std::fs::rename(&self.scratch, dest).is_err() {
            std::fs::copy(&self.scratch, dest)?;
            let _ = std::fs::remove_file(&self.scratch);
        }
        Ok(())
    }

    /// Placed by counting samples, like the picture by counting frames.
    fn write_audio(&mut self, pcm: &[u8]) -> Result<(), SinkError> {
        let Some(src) = &self.audio_src else {
            return Ok(());
        };
        let frame_bytes = (CHANNELS * BITS_PER_SAMPLE / 8) as usize;
        let frames = (pcm.len() / frame_bytes) as u64;
        if frames == 0 {
            return Ok(());
        }
        let mut buffer = gst::Buffer::from_slice(pcm[..frames as usize * frame_bytes].to_vec());
        if let Some(b) = buffer.get_mut() {
            b.set_pts(gst::ClockTime::from_nseconds(
                self.samples * 1_000_000_000 / SAMPLE_RATE as u64,
            ));
            b.set_duration(gst::ClockTime::from_nseconds(
                frames * 1_000_000_000 / SAMPLE_RATE as u64,
            ));
        }
        src.push_buffer(buffer)
            .map_err(|e| died(&format!("audio: {e:?}")))?;
        self.samples += frames;
        Ok(())
    }
}

impl Drop for NativeSink {
    fn drop(&mut self) {
        // An abandoned recording should not leave a part-written file behind.
        if let Some(p) = self.pipeline.take() {
            let _ = p.set_state(gst::State::Null);
            let _ = std::fs::remove_file(&self.scratch);
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub fn temp(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "screenilux-linux-{}-{}-{name}.mp4",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ))
    }

    /// Frame `n`: a solid colour that changes with `n`.
    pub fn frame(w: u32, h: u32, n: u8) -> Vec<u8> {
        [n, 255 - n, 128, 255].repeat((w * h) as usize)
    }

    #[test]
    fn bitrate_scales_and_is_clamped() {
        assert_eq!(bitrate_for(1920, 1080, 30), 8_294_400);
        assert_eq!(bitrate_for(100, 100, 30), 2_000_000);
        assert_eq!(bitrate_for(7680, 4320, 60), 40_000_000);
    }

    #[test]
    fn writes_a_video_with_sound_that_decodes_at_the_right_length() {
        if video_encoder(64, 48, 30).is_none() || audio_encoder().is_none() {
            return;
        }
        let dest = temp("basic");
        let mut sink = NativeSink::new(64, 48, 30, true, temp("scratch")).unwrap();
        sink.open_segment().unwrap();
        for i in 0..60 {
            sink.write(&frame(64, 48, i as u8 * 4)).unwrap();
            // A thirtieth of a second of a 440Hz tone per frame.
            let pcm: Vec<u8> = (0..1600)
                .flat_map(|s| {
                    let v = ((s as f64 * 440.0 * std::f64::consts::TAU / 48_000.0).sin() * 8000.0)
                        as i16;
                    [v.to_le_bytes(), v.to_le_bytes()].concat()
                })
                .collect();
            sink.write_audio(&pcm).unwrap();
        }
        sink.close_segment(0).unwrap();
        sink.finish(&dest).unwrap();

        let info = crate::probe(&dest).expect("the file should probe");
        assert_eq!((info.width, info.height), (64, 48));
        assert!(
            (info.duration - 2.0).abs() < 0.15,
            "two seconds, got {}",
            info.duration
        );
        assert!(info.has_audio);
        let _ = std::fs::remove_file(dest);
    }

    #[test]
    fn finishing_with_no_frames_is_an_error_not_an_empty_file() {
        if video_encoder(64, 48, 30).is_none() {
            return;
        }
        let mut sink = NativeSink::new(64, 48, 30, false, temp("empty-scratch")).unwrap();
        sink.open_segment().unwrap();
        assert!(sink.finish(&temp("empty")).is_err());
    }
}
