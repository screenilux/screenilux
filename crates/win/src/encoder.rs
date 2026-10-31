//! Hardware H.264 encoding through Media Foundation.
//!
//! This replaces the ffmpeg child process entirely. Windows already ships a
//! hardware encoder; bundling one would have made the download three to five
//! times the size of the whole application to do a job the OS does natively at
//! roughly ten times realtime.
//!
//! It also simplifies the recorder. Because we own the timestamps, a pause is
//! just "stop sending frames and keep numbering contiguous" — no segments, no
//! concat pass, and none of the failure modes that come with joining files
//! encoded at different times.

use std::path::{Path, PathBuf};

use screenilux_core::ports::{SinkError, VideoSink, WriteStatus};
use windows_capture::encoder::{
    AudioSettingsBuilder, AudioSettingsSubType, ContainerSettingsBuilder, VideoEncoder,
    VideoSettingsBuilder, VideoSettingsSubType,
};

/// Media Foundation timestamps are in 100-nanosecond units.
const HNS_PER_SECOND: i64 = 10_000_000;

pub const ENCODER_LABEL: &str = "Media Foundation (H.264)";

/// A rough quality target. Screen content is mostly flat colour and text, so
/// this is generous for 1080p and keeps text crisp.
fn bitrate_for(width: u32, height: u32, fps: u32) -> u32 {
    let pixels = width as u64 * height as u64;
    let scaled = pixels * fps as u64 / 30;
    // ~4 bits per pixel per frame: about 8 Mbit at 1080p30, scaling with area
    // and frame rate. Generous for screen content, which is mostly flat colour
    // and text, and keeps small text crisp.
    ((scaled * 4) as u32).clamp(2_000_000, 40_000_000)
}

pub struct NativeSink {
    encoder: Option<VideoEncoder>,
    /// Where the encoder writes. Moved to the caller's destination on finish.
    scratch: PathBuf,
    width: u32,
    height: u32,
    fps: u32,
    /// Monotonic across pauses: paused time contributes no frames and so
    /// simply does not exist in the output.
    frames: u64,
    /// Rows reversed for the encoder, reused rather than reallocated.
    flipped: Vec<u8>,
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
            encoder: None,
            scratch,
            width,
            height,
            fps,
            frames: 0,
            flipped: vec![0u8; width as usize * height as usize * 4],
            has_frame: false,
            audio,
        })
    }

    fn ensure_encoder(&mut self) -> Result<&mut VideoEncoder, SinkError> {
        if self.encoder.is_none() {
            if let Some(parent) = self.scratch.parent() {
                std::fs::create_dir_all(parent)?;
            }
            let video = VideoSettingsBuilder::new(self.width, self.height)
                // The builder defaults to HEVC; H.264 is what plays everywhere.
                .sub_type(VideoSettingsSubType::H264)
                .frame_rate(self.fps)
                .bitrate(bitrate_for(self.width, self.height, self.fps));
            let audio = if self.audio {
                AudioSettingsBuilder::new().sub_type(AudioSettingsSubType::AAC)
            } else {
                AudioSettingsBuilder::new().disabled(true)
            };
            let encoder =
                VideoEncoder::new(video, audio, ContainerSettingsBuilder::new(), &self.scratch)
                    .map_err(|e| SinkError::StartFailed(e.to_string()))?;
            self.encoder = Some(encoder);
        }
        Ok(self.encoder.as_mut().expect("just created"))
    }

    /// Copy rows in reverse order.
    ///
    /// `send_frame_buffer` documents that Windows wants BGRA bottom-to-top on
    /// this path, and it does: feeding top-to-bottom produces a vertically
    /// mirrored video.
    fn flip_into_scratch(&mut self, frame: &[u8]) {
        let stride = self.width as usize * 4;
        for (dst, src) in self
            .flipped
            .chunks_exact_mut(stride)
            .zip(frame.chunks_exact(stride).rev())
        {
            dst.copy_from_slice(src);
        }
    }

    fn send_current(&mut self) -> Result<(), SinkError> {
        let ts = (self.frames as i64) * HNS_PER_SECOND / self.fps as i64;
        // Taken out and put back so the encoder can be borrowed mutably while
        // the scratch buffer is read.
        let buffer = std::mem::take(&mut self.flipped);
        let result = self.ensure_encoder().and_then(|enc| {
            enc.send_frame_buffer(&buffer, ts)
                .map_err(|e| SinkError::EncoderDied(e.to_string()))
        });
        self.flipped = buffer;
        result?;
        self.frames += 1;
        Ok(())
    }
}

impl VideoSink for NativeSink {
    /// There are no segments here. The first call opens the encoder; a call
    /// after a pause is a no-op, because the same encoder continues and the
    /// frame numbering never jumped.
    fn open_segment(&mut self) -> Result<(), SinkError> {
        self.ensure_encoder()?;
        Ok(())
    }

    fn write(&mut self, frame: &[u8]) -> Result<WriteStatus, SinkError> {
        let expected = self.width as usize * self.height as usize * 4;
        if frame.len() != expected {
            return Err(SinkError::EncoderDied(format!(
                "frame is {} bytes, expected {expected}",
                frame.len()
            )));
        }
        self.flip_into_scratch(frame);
        self.send_current()?;
        self.has_frame = true;
        // Nothing to fall back to: this *is* the platform encoder.
        Ok(WriteStatus::Ok)
    }

    /// Top up the owed frames, but keep the encoder open: a pause is not a
    /// boundary in the output, only in the input.
    fn close_segment(&mut self, pad: u64) -> Result<(), SinkError> {
        if !self.has_frame {
            return Ok(());
        }
        for _ in 0..pad {
            self.send_current()?;
        }
        Ok(())
    }

    fn finish(&mut self, dest: &Path) -> Result<(), SinkError> {
        let Some(encoder) = self.encoder.take() else {
            return Err(SinkError::EncoderDied("nothing was recorded".into()));
        };
        encoder
            .finish()
            .map_err(|e| SinkError::EncoderDied(e.to_string()))?;

        if !self.has_frame {
            let _ = std::fs::remove_file(&self.scratch);
            return Err(SinkError::EncoderDied("nothing was recorded".into()));
        }
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent)?;
        }
        // A rename across volumes fails, so fall back to a copy.
        if std::fs::rename(&self.scratch, dest).is_err() {
            std::fs::copy(&self.scratch, dest)?;
            let _ = std::fs::remove_file(&self.scratch);
        }
        Ok(())
    }

    /// The encoder derives audio timestamps itself to guarantee monotonic
    /// timing, so none is passed here.
    fn write_audio(&mut self, pcm: &[u8]) -> Result<(), SinkError> {
        let Some(enc) = self.encoder.as_mut() else {
            return Ok(());
        };
        enc.send_audio_buffer(pcm, 0)
            .map_err(|e| SinkError::EncoderDied(format!("audio: {e}")))
    }

    fn encoder_label(&self) -> &'static str {
        ENCODER_LABEL
    }
}

impl Drop for NativeSink {
    fn drop(&mut self) {
        // An abandoned recording should not leave a part-written file behind.
        if self.encoder.take().is_some() {
            let _ = std::fs::remove_file(&self.scratch);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    fn temp(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "screenilux-sink-{}-{}-{name}.mp4",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ))
    }

    /// Verification goes through ffprobe, which is a development tool here -
    /// the application itself no longer needs ffmpeg at all.
    fn probe(file: &Path, entries: &str) -> String {
        let Some(ffprobe) = which_ffprobe() else {
            return String::new();
        };
        let out = Command::new(ffprobe)
            .args([
                "-v",
                "error",
                "-select_streams",
                "v:0",
                "-count_frames",
                "-show_entries",
                entries,
                "-of",
                "default=nw=1:nk=1",
            ])
            .arg(file)
            .output()
            .expect("ffprobe should run");
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    fn which_ffprobe() -> Option<PathBuf> {
        std::env::var_os("PATH")?
            .to_string_lossy()
            .split(';')
            .map(|d| PathBuf::from(d).join("ffprobe.exe"))
            .find(|p| p.is_file())
    }

    fn frame(w: u32, h: u32, shade: u8) -> Vec<u8> {
        vec![shade; w as usize * h as usize * 4]
    }

    #[test]
    fn encodes_a_playable_file() {
        let (w, h, fps) = (640u32, 480u32, 30u32);
        let dest = temp("basic");
        let mut sink = NativeSink::new(w, h, fps, false, temp("basic-scratch")).unwrap();
        sink.open_segment().unwrap();
        for i in 0..60 {
            assert_eq!(sink.write(&frame(w, h, i as u8)).unwrap(), WriteStatus::Ok);
        }
        sink.close_segment(0).unwrap();
        sink.finish(&dest).unwrap();

        assert!(dest.is_file());
        let info = probe(&dest, "stream=codec_name,width,height,nb_read_frames");
        if !info.is_empty() {
            let lines: Vec<&str> = info.lines().collect();
            assert_eq!(lines[0], "h264");
            assert_eq!(lines[1], "640");
            assert_eq!(lines[2], "480");
            assert_eq!(lines[3], "60", "every frame should be present");
        }
        let _ = std::fs::remove_file(&dest);
    }

    #[test]
    fn padding_lengthens_the_output() {
        let (w, h, fps) = (320u32, 240u32, 30u32);
        let dest = temp("pad");
        let mut sink = NativeSink::new(w, h, fps, false, temp("pad-scratch")).unwrap();
        sink.open_segment().unwrap();
        for _ in 0..15 {
            sink.write(&frame(w, h, 9)).unwrap();
        }
        sink.close_segment(15).unwrap(); // owed another half second
        sink.finish(&dest).unwrap();

        let frames = probe(&dest, "stream=nb_read_frames");
        if !frames.is_empty() {
            assert_eq!(frames, "30", "padding should have topped the file up");
        }
        let _ = std::fs::remove_file(&dest);
    }

    /// The property that removes segments and concat entirely: a pause
    /// contributes no frames, so the output simply omits it.
    #[test]
    fn a_pause_leaves_no_gap_in_the_output() {
        let (w, h, fps) = (320u32, 240u32, 30u32);
        let dest = temp("pause");
        let mut sink = NativeSink::new(w, h, fps, false, temp("pause-scratch")).unwrap();

        sink.open_segment().unwrap();
        for _ in 0..30 {
            sink.write(&frame(w, h, 1)).unwrap();
        }
        sink.close_segment(0).unwrap(); // pause
        sink.open_segment().unwrap(); // resume
        for _ in 0..30 {
            sink.write(&frame(w, h, 2)).unwrap();
        }
        sink.close_segment(0).unwrap();
        sink.finish(&dest).unwrap();

        let out = probe(&dest, "stream=nb_read_frames");
        if !out.is_empty() {
            assert_eq!(out, "60", "two seconds of frames, no hole for the pause");
        }
        let d: f64 = {
            let Some(ffprobe) = which_ffprobe() else {
                return;
            };
            let o = Command::new(ffprobe)
                .args([
                    "-v",
                    "error",
                    "-show_entries",
                    "format=duration",
                    "-of",
                    "csv=p=0",
                ])
                .arg(&dest)
                .output()
                .unwrap();
            String::from_utf8_lossy(&o.stdout)
                .trim()
                .parse()
                .unwrap_or(0.0)
        };
        assert!((d - 2.0).abs() < 0.2, "expected ~2s, got {d}");
        let _ = std::fs::remove_file(&dest);
    }

    #[test]
    fn a_wrong_sized_frame_is_rejected() {
        let mut sink = NativeSink::new(320, 240, 30, false, temp("bad-scratch")).unwrap();
        sink.open_segment().unwrap();
        assert!(sink.write(&[0u8; 16]).is_err());
    }

    #[test]
    fn finishing_with_no_frames_is_an_error_not_an_empty_file() {
        let dest = temp("empty");
        let mut sink = NativeSink::new(320, 240, 30, false, temp("empty-scratch")).unwrap();
        sink.open_segment().unwrap();
        assert!(sink.finish(&dest).is_err());
        assert!(!dest.exists());
    }

    #[test]
    fn bitrate_scales_with_area_and_frame_rate() {
        let sd = bitrate_for(1280, 720, 30);
        let hd = bitrate_for(1920, 1080, 30);
        let hd60 = bitrate_for(1920, 1080, 60);
        assert!(hd > sd, "1080p should get more than 720p");
        assert!(hd60 > hd, "60fps should get more than 30fps");
        assert!((6_000_000..=12_000_000).contains(&hd), "1080p30 got {hd}");
    }

    #[test]
    fn an_abandoned_sink_leaves_no_file_behind() {
        let scratch = temp("abandoned-scratch");
        {
            let mut sink = NativeSink::new(320, 240, 30, false, scratch.clone()).unwrap();
            sink.open_segment().unwrap();
            sink.write(&frame(320, 240, 5)).unwrap();
        }
        assert!(!scratch.exists(), "dropping must not leak a partial file");
    }
}
