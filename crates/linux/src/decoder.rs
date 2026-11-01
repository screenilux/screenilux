//! Decoding recordings for playback through GStreamer.
//!
//! Recordings are MP4s, so the demuxer is named and each decoder takes only
//! its own stream: the picture as RGBA, the sound as 48kHz 16-bit stereo.

use std::path::Path;

use gstreamer as gst;
use gstreamer::prelude::*;
use gstreamer_app as gst_app;
use gstreamer_pbutils as pbutils;

use crate::gst as g;

#[derive(Debug, thiserror::Error)]
pub enum DecodeError {
    #[error("could not open the video: {0}")]
    Open(String),
    #[error("the file has no {0} stream")]
    NoStream(&'static str),
    #[error("decoding failed: {0}")]
    Read(String),
}

/// The PCM the player plays: 48kHz, 16-bit, stereo.
pub const PLAYBACK_RATE: u32 = 48_000;
pub const PLAYBACK_CHANNELS: u16 = 2;
/// Bytes per sample frame of that PCM.
pub const PLAYBACK_BLOCK: usize = 4;

/// How long to wait for a frame before calling the file broken.
const PULL: gst::ClockTime = gst::ClockTime::from_seconds(10);

/// What a file holds, without decoding it.
#[derive(Debug, Clone, PartialEq)]
pub struct MediaInfo {
    pub width: u32,
    pub height: u32,
    pub duration: f64,
    pub has_audio: bool,
}

/// Size, length and whether there is sound.
pub fn probe(path: &Path) -> Result<MediaInfo, DecodeError> {
    g::init();
    let uri =
        gst::glib::filename_to_uri(path, None).map_err(|e| DecodeError::Open(e.to_string()))?;
    let discoverer = pbutils::Discoverer::new(gst::ClockTime::from_seconds(10))
        .map_err(|e| DecodeError::Open(e.to_string()))?;
    let info = discoverer
        .discover_uri(&uri)
        .map_err(|e| DecodeError::Open(e.to_string()))?;
    let video = info
        .video_streams()
        .into_iter()
        .next()
        .ok_or(DecodeError::NoStream("video"))?;
    Ok(MediaInfo {
        width: video.width(),
        height: video.height(),
        duration: info.duration().map(g::seconds).unwrap_or(0.0),
        has_audio: !info.audio_streams().is_empty(),
    })
}

/// A decoded picture, tightly packed RGBA8.
#[derive(Debug, Clone)]
pub struct VideoFrame {
    pub t: f64,
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

/// Open `path` for one of its streams, paused on its first sample.
fn open_stream(
    path: &Path,
    stream: &str,
    convert: &str,
) -> Result<(gst::Pipeline, gst_app::AppSink), DecodeError> {
    let pipeline = g::pipeline(&format!(
        "filesrc location={} ! qtdemux name=demux demux.{stream}_0 ! queue ! decodebin ! {convert} ! \
         appsink name=sink sync=false max-buffers=8",
        g::quoted(path)
    ))
    .map_err(DecodeError::Open)?;
    let sink: gst_app::AppSink = g::element(&pipeline, "sink").map_err(DecodeError::Open)?;
    g::set_state(&pipeline, gst::State::Paused).map_err(DecodeError::Open)?;
    Ok((pipeline, sink))
}

/// Frames of one video, in order.
pub struct VideoDecoder {
    pipeline: gst::Pipeline,
    sink: gst_app::AppSink,
    width: u32,
    height: u32,
    duration: f64,
    playing: bool,
    /// After a seek the paused frame is handed out again once playback
    /// starts; anything up to here has been seen.
    seen: Option<f64>,
}

impl VideoDecoder {
    pub fn open(path: &Path) -> Result<Self, DecodeError> {
        let info = probe(path)?;
        let (pipeline, sink) =
            open_stream(path, "video", "videoconvert ! video/x-raw,format=RGBA")?;
        Ok(Self {
            pipeline,
            sink,
            width: info.width,
            height: info.height,
            duration: info.duration,
            playing: false,
            seen: None,
        })
    }

    pub fn width(&self) -> u32 {
        self.width
    }

    pub fn height(&self) -> u32 {
        self.height
    }

    pub fn duration(&self) -> f64 {
        self.duration
    }

    fn play(&mut self) -> Result<(), DecodeError> {
        if !self.playing {
            g::set_state(&self.pipeline, gst::State::Playing).map_err(DecodeError::Read)?;
            self.playing = true;
        }
        Ok(())
    }

    /// The next sample, or `None` at the end.
    fn pull(&mut self) -> Result<Option<gst::Sample>, DecodeError> {
        self.play()?;
        loop {
            match self.sink.try_pull_sample(PULL) {
                Some(sample) => {
                    let t = time_of(&sample);
                    if self.seen.is_some_and(|seen| t <= seen) {
                        continue;
                    }
                    self.seen = None;
                    return Ok(Some(sample));
                }
                None if self.sink.is_eos() => return Ok(None),
                None => {
                    return Err(DecodeError::Read(
                        g::error_so_far(&self.pipeline)
                            .unwrap_or_else(|| "no frame arrived".into()),
                    ));
                }
            }
        }
    }

    /// The next frame, or `None` at the end.
    pub fn next_frame(&mut self) -> Result<Option<VideoFrame>, DecodeError> {
        Ok(self.pull()?.map(|s| frame_of(&s, self.width, self.height)))
    }

    /// For walking a whole video in one pass, as the text indexer does: the
    /// frame is handed back only when `want` says so.
    pub fn next_frame_if(
        &mut self,
        want: impl Fn(f64) -> bool,
    ) -> Result<Option<(f64, Option<VideoFrame>)>, DecodeError> {
        Ok(self.pull()?.map(|s| {
            let t = time_of(&s);
            (t, want(t).then(|| frame_of(&s, self.width, self.height)))
        }))
    }

    /// The frame showing at `t`.
    pub fn frame_at(&mut self, t: f64, fps: f64) -> Result<Option<VideoFrame>, DecodeError> {
        let half_frame = 0.5 / fps.max(1.0);
        let t = if self.duration > 0.0 {
            t.min(self.duration - half_frame).max(0.0)
        } else {
            t.max(0.0)
        };
        g::set_state(&self.pipeline, gst::State::Paused).map_err(DecodeError::Read)?;
        self.playing = false;
        self.pipeline
            .seek_simple(
                gst::SeekFlags::FLUSH | gst::SeekFlags::ACCURATE,
                g::clock_time(t),
            )
            .map_err(|e| DecodeError::Read(e.to_string()))?;
        let _ = self.pipeline.state(gst::ClockTime::from_seconds(10));
        match self.sink.try_pull_preroll(PULL) {
            Some(sample) => {
                let frame = frame_of(&sample, self.width, self.height);
                self.seen = Some(frame.t);
                Ok(Some(frame))
            }
            None => Ok(None),
        }
    }
}

impl Drop for VideoDecoder {
    fn drop(&mut self) {
        let _ = self.pipeline.set_state(gst::State::Null);
    }
}

fn time_of(sample: &gst::Sample) -> f64 {
    sample
        .buffer()
        .and_then(|b| b.pts())
        .map(g::seconds)
        .unwrap_or(0.0)
}

fn frame_of(sample: &gst::Sample, fallback_w: u32, fallback_h: u32) -> VideoFrame {
    let (w, h) = sample
        .caps()
        .and_then(|c| c.structure(0))
        .and_then(|s| Some((s.get::<i32>("width").ok()?, s.get::<i32>("height").ok()?)))
        .map(|(w, h)| (w as u32, h as u32))
        .unwrap_or((fallback_w, fallback_h));
    let row = w as usize * 4;
    let rgba = match sample.buffer().and_then(|b| b.map_readable().ok()) {
        Some(map) => {
            let stride = map.len() / (h as usize).max(1);
            if stride == row {
                map.as_slice()[..row * h as usize].to_vec()
            } else {
                map.as_slice()
                    .chunks(stride)
                    .take(h as usize)
                    .flat_map(|r| &r[..row])
                    .copied()
                    .collect()
            }
        }
        None => vec![0; row * h as usize],
    };
    VideoFrame {
        t: time_of(sample),
        width: w,
        height: h,
        rgba,
    }
}

/// A file's sound, as 48kHz 16-bit stereo PCM.
pub struct AudioDecoder {
    pipeline: gst::Pipeline,
    sink: gst_app::AppSink,
    playing: bool,
}

impl AudioDecoder {
    pub fn open(path: &Path) -> Result<Self, DecodeError> {
        if !probe(path)?.has_audio {
            return Err(DecodeError::NoStream("audio"));
        }
        let convert = format!("audioconvert ! audioresample ! {}", crate::audio::PCM_CAPS);
        let (pipeline, sink) = open_stream(path, "audio", &convert)?;
        Ok(Self {
            pipeline,
            sink,
            playing: false,
        })
    }

    /// Start reading from `t`. The first chunk is trimmed so its first sample
    /// is at `t`.
    pub fn seek(&mut self, t: f64) -> Result<Vec<u8>, DecodeError> {
        g::set_state(&self.pipeline, gst::State::Paused).map_err(DecodeError::Read)?;
        self.playing = false;
        self.pipeline
            .seek_simple(
                gst::SeekFlags::FLUSH | gst::SeekFlags::ACCURATE,
                g::clock_time(t),
            )
            .map_err(|e| DecodeError::Read(e.to_string()))?;
        loop {
            match self.next_chunk()? {
                Some((ts, mut pcm)) => {
                    let end = ts + (pcm.len() / PLAYBACK_BLOCK) as f64 / PLAYBACK_RATE as f64;
                    if end <= t {
                        continue;
                    }
                    let skip =
                        (((t - ts).max(0.0) * PLAYBACK_RATE as f64) as usize) * PLAYBACK_BLOCK;
                    pcm.drain(..skip.min(pcm.len()));
                    return Ok(pcm);
                }
                None => return Ok(Vec::new()),
            }
        }
    }

    /// The next chunk of PCM with its start time, or `None` at the end.
    pub fn next_chunk(&mut self) -> Result<Option<(f64, Vec<u8>)>, DecodeError> {
        if !self.playing {
            g::set_state(&self.pipeline, gst::State::Playing).map_err(DecodeError::Read)?;
            self.playing = true;
        }
        match self.sink.try_pull_sample(PULL) {
            Some(sample) => {
                let pcm = sample
                    .buffer()
                    .and_then(|b| b.map_readable().ok())
                    .map(|m| m.as_slice().to_vec())
                    .unwrap_or_default();
                Ok(Some((time_of(&sample), pcm)))
            }
            None if self.sink.is_eos() => Ok(None),
            None => Err(DecodeError::Read(
                g::error_so_far(&self.pipeline).unwrap_or_else(|| "no sound arrived".into()),
            )),
        }
    }
}

impl Drop for AudioDecoder {
    fn drop(&mut self) {
        let _ = self.pipeline.set_state(gst::State::Null);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::encoder::tests::{frame, temp};
    use screenilux_core::ports::VideoSink;

    /// Two seconds at 30fps whose colour encodes the frame number.
    fn encode(sound: bool) -> Option<std::path::PathBuf> {
        let dest = temp("decode");
        let mut sink = crate::NativeSink::new(64, 48, 30, sound, temp("decode-scratch")).ok()?;
        sink.open_segment().ok()?;
        for i in 0..60u8 {
            sink.write(&frame(64, 48, i * 4)).ok()?;
            if sound {
                sink.write_audio(&vec![0u8; 1600 * 4]).ok()?;
            }
        }
        sink.close_segment(0).ok()?;
        sink.finish(&dest).ok()?;
        Some(dest)
    }

    #[test]
    fn frames_come_back_in_order_at_the_right_size() {
        let Some(path) = encode(false) else { return };
        let mut dec = VideoDecoder::open(&path).unwrap();
        assert_eq!((dec.width(), dec.height()), (64, 48));
        let mut count = 0;
        let mut last = -1.0;
        while let Some(f) = dec.next_frame().unwrap() {
            assert!(f.t > last, "timestamps rise");
            assert_eq!(f.rgba.len(), 64 * 48 * 4);
            last = f.t;
            count += 1;
        }
        assert!((58..=61).contains(&count), "about 60 frames, got {count}");
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn seeking_lands_on_the_frame_and_playing_continues_after_it() {
        let Some(path) = encode(false) else { return };
        let mut dec = VideoDecoder::open(&path).unwrap();
        let f = dec.frame_at(1.0, 30.0).unwrap().expect("a frame at 1s");
        assert!((f.t - 1.0).abs() < 0.05, "landed at {}", f.t);
        let next = dec.next_frame().unwrap().expect("the frame after");
        assert!(next.t > f.t, "{} should follow {}", next.t, f.t);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn sound_decodes_and_a_silent_file_says_so() {
        let Some(with) = encode(true) else { return };
        let mut dec = AudioDecoder::open(&with).unwrap();
        let first = dec.seek(0.5).unwrap();
        assert_eq!(first.len() % PLAYBACK_BLOCK, 0);
        assert!(dec.next_chunk().unwrap().is_some());
        let Some(without) = encode(false) else { return };
        assert!(matches!(
            AudioDecoder::open(&without),
            Err(DecodeError::NoStream("audio"))
        ));
        let _ = std::fs::remove_file(with);
        let _ = std::fs::remove_file(without);
    }
}
