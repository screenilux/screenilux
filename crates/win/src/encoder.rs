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
//!
//! # Why the sink writer, and not a transcoder
//!
//! Frames arrive here faster than they can be encoded: a capture thread hands
//! over one every frame period whatever the encoder is doing, and an export
//! hands them over as fast as the file decodes. So the one property this sink
//! must have is **backpressure** — when the encoder falls behind, the caller
//! has to wait, because the alternative is a queue of uncompressed frames and
//! every one of those is `width * height * 4` bytes. At 1080p that is 8MB a
//! frame, so a queue nobody bounds reaches a gigabyte in four seconds of
//! deficit, and the machine, not just the recording, is what pays: the
//! desktop compositor is the next thing to ask the driver for memory, and it
//! is killed outright when there is none.
//!
//! `IMFSinkWriter` throttles by default: `WriteSample` blocks once the
//! encoder is behind, which is exactly the shape the recorder wants and the
//! same shape the Linux backend gets from `appsrc block=true` with a bounded
//! queue. A `MediaTranscoder` driven by a `MediaStreamSource` — the obvious
//! alternative, and what this module used to do through `windows-capture` —
//! accepts every frame immediately into an unbounded channel instead, which
//! is the bug above rather than a trade-off.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use screenilux_core::ports::{SinkError, VideoSink};
use windows::Win32::Media::MediaFoundation::*;
use windows::core::{GUID, HSTRING};

use crate::audio::{BITS_PER_SAMPLE, CHANNELS, SAMPLE_RATE};

/// Media Foundation timestamps are in 100-nanosecond units.
const HNS_PER_SECOND: i64 = 10_000_000;

pub const ENCODER_LABEL: &str = "Media Foundation (H.264)";

/// Bytes per sample frame of the PCM the audio stream takes.
const BLOCK: usize = (CHANNELS * BITS_PER_SAMPLE / 8) as usize;

/// AAC at 128 kbit/s. The encoder only accepts a few rates, and this is the
/// highest of them: speech over a screen recording, so clarity over size.
const AAC_BYTES_PER_SECOND: u32 = 16_000;

/// How many uncompressed frames the writer may be holding before the caller
/// is made to wait, which is also the sink's memory budget.
///
/// Three, as on Linux: enough to keep the encoder busy across a hiccup,
/// little enough that falling behind costs time rather than memory. The sink
/// writer's own throttle was measured letting sixteen through — 132MB at
/// 1080p, over half a gigabyte at 4K — so it is a backstop here, not the
/// bound.
const QUEUE_FRAMES: u64 = 3;

/// The longest one frame waits for room. An encoder further behind than this
/// is not catching up, so the frame goes in regardless: the recorder's pacing
/// steps over the gap, and a capture thread stays able to notice a stop.
const ROOM_TIMEOUT: Duration = Duration::from_millis(500);

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

/// The two halves of a `UINT64` attribute: a size, a frame rate, a ratio.
fn packed(hi: u32, lo: u32) -> u64 {
    ((hi as u64) << 32) | lo as u64
}

fn died(e: windows::core::Error) -> SinkError {
    SinkError::EncoderDied(e.to_string())
}

/// An empty media type of the given kind.
fn media_type(major: &GUID, sub: &GUID) -> windows::core::Result<IMFMediaType> {
    // SAFETY: a freshly created attribute store; every call below sets a
    // documented attribute on it.
    unsafe {
        let ty = MFCreateMediaType()?;
        ty.SetGUID(&MF_MT_MAJOR_TYPE, major)?;
        ty.SetGUID(&MF_MT_SUBTYPE, sub)?;
        Ok(ty)
    }
}

/// One frame or one run of PCM, ready to hand to the writer.
fn sample(bytes: &[u8], time: i64, duration: i64) -> windows::core::Result<IMFSample> {
    // SAFETY: the buffer is created at `bytes.len()`, locked for exactly that
    // many bytes, and unlocked before anything else touches it.
    unsafe {
        let buffer = MFCreateMemoryBuffer(bytes.len() as u32)?;
        let mut dst = std::ptr::null_mut();
        buffer.Lock(&mut dst, None, None)?;
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), dst, bytes.len());
        buffer.Unlock()?;
        buffer.SetCurrentLength(bytes.len() as u32)?;

        let sample = MFCreateSample()?;
        sample.AddBuffer(&buffer)?;
        sample.SetSampleTime(time)?;
        sample.SetSampleDuration(duration)?;
        Ok(sample)
    }
}

/// A started writer and the streams it was given.
#[derive(Clone)]
struct Writer {
    sink: IMFSinkWriter,
    video: u32,
    /// Present only when the sink was asked for sound.
    audio: Option<u32>,
}

pub struct NativeSink {
    writer: Option<Writer>,
    /// Where the encoder writes. Moved to the caller's destination on finish.
    scratch: PathBuf,
    width: u32,
    height: u32,
    fps: u32,
    /// Monotonic across pauses: paused time contributes no frames and so
    /// simply does not exist in the output.
    frames: u64,
    /// The last frame written, so a segment can be topped up without the
    /// caller having to hand it over again.
    last: Vec<u8>,
    has_frame: bool,
    audio: bool,
    /// Sample frames of sound handed over, which is the audio clock: the
    /// stream's timestamps are counted rather than taken from the source, so
    /// a late or dropped packet cannot shift everything after it.
    audio_frames: u64,
}

/// SAFETY: the writer is created on the thread that first writes to the sink —
/// `NativeSink::new` makes no Media Foundation calls — and every method takes
/// `&mut self`, so calls to it are serialised and stay on that one thread.
/// Media Foundation objects are usable from any thread of the multithreaded
/// apartment, which [`crate::decoder::init_thread`] puts each of ours in.
unsafe impl Send for NativeSink {}

impl NativeSink {
    pub fn new(
        width: u32,
        height: u32,
        fps: u32,
        audio: bool,
        scratch: PathBuf,
    ) -> Result<Self, SinkError> {
        Ok(Self {
            writer: None,
            scratch,
            width,
            height,
            fps,
            frames: 0,
            last: vec![0u8; width as usize * height as usize * 4],
            has_frame: false,
            audio,
            audio_frames: 0,
        })
    }

    /// What comes out: H.264 at a bitrate scaled to the picture.
    fn video_out(&self) -> windows::core::Result<IMFMediaType> {
        let ty = media_type(&MFMediaType_Video, &MFVideoFormat_H264)?;
        // SAFETY: documented attributes of an H.264 output type.
        unsafe {
            ty.SetUINT32(
                &MF_MT_AVG_BITRATE,
                bitrate_for(self.width, self.height, self.fps),
            )?;
            ty.SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32)?;
            ty.SetUINT64(&MF_MT_FRAME_SIZE, packed(self.width, self.height))?;
            ty.SetUINT64(&MF_MT_FRAME_RATE, packed(self.fps, 1))?;
            ty.SetUINT64(&MF_MT_PIXEL_ASPECT_RATIO, packed(1, 1))?;
        }
        Ok(ty)
    }

    /// What goes in: the tightly packed BGRA the capture and the compositor
    /// both work in, top row first.
    ///
    /// A positive `MF_MT_DEFAULT_STRIDE` is what says "top row first". The
    /// old transcoder path wanted the rows the other way up and had to
    /// reverse every frame to suit it; declaring the layout costs nothing and
    /// saves a full-frame copy per frame.
    fn video_in(&self) -> windows::core::Result<IMFMediaType> {
        let ty = media_type(&MFMediaType_Video, &MFVideoFormat_RGB32)?;
        // SAFETY: documented attributes of an uncompressed RGB input type.
        unsafe {
            ty.SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32)?;
            ty.SetUINT64(&MF_MT_FRAME_SIZE, packed(self.width, self.height))?;
            ty.SetUINT64(&MF_MT_FRAME_RATE, packed(self.fps, 1))?;
            ty.SetUINT64(&MF_MT_PIXEL_ASPECT_RATIO, packed(1, 1))?;
            ty.SetUINT32(&MF_MT_DEFAULT_STRIDE, self.width * 4)?;
            ty.SetUINT32(&MF_MT_ALL_SAMPLES_INDEPENDENT, 1)?;
        }
        Ok(ty)
    }

    /// The sound, in and out: PCM from the recorder, AAC in the file.
    fn audio_types() -> windows::core::Result<(IMFMediaType, IMFMediaType)> {
        let out = media_type(&MFMediaType_Audio, &MFAudioFormat_AAC)?;
        let input = media_type(&MFMediaType_Audio, &MFAudioFormat_PCM)?;
        // SAFETY: documented attributes of AAC and PCM audio types.
        unsafe {
            for ty in [&out, &input] {
                ty.SetUINT32(&MF_MT_AUDIO_SAMPLES_PER_SECOND, SAMPLE_RATE)?;
                ty.SetUINT32(&MF_MT_AUDIO_NUM_CHANNELS, CHANNELS as u32)?;
                ty.SetUINT32(&MF_MT_AUDIO_BITS_PER_SAMPLE, BITS_PER_SAMPLE as u32)?;
            }
            out.SetUINT32(&MF_MT_AUDIO_AVG_BYTES_PER_SECOND, AAC_BYTES_PER_SECOND)?;
            input.SetUINT32(&MF_MT_AUDIO_BLOCK_ALIGNMENT, BLOCK as u32)?;
            input.SetUINT32(
                &MF_MT_AUDIO_AVG_BYTES_PER_SECOND,
                SAMPLE_RATE * BLOCK as u32,
            )?;
            input.SetUINT32(&MF_MT_ALL_SAMPLES_INDEPENDENT, 1)?;
        }
        Ok((out, input))
    }

    /// The writer, started if this is the first call.
    ///
    /// Returned by value: the interface is reference counted, and a clone
    /// ends the borrow of `self` so the frame can be read out of `last` while
    /// it is written.
    fn ready(&mut self) -> Result<Writer, SinkError> {
        if let Some(writer) = &self.writer {
            return Ok(writer.clone());
        }
        if let Some(parent) = self.scratch.parent() {
            std::fs::create_dir_all(parent)?;
        }
        crate::decoder::init_thread();

        let start = |e: windows::core::Error| SinkError::StartFailed(e.to_string());
        // SAFETY: documented Media Foundation calls on owned interfaces, in
        // the order the sink writer requires: add every stream, describe what
        // will be fed to it, then begin.
        let writer = unsafe {
            let mut attrs: Option<IMFAttributes> = None;
            MFCreateAttributes(&mut attrs, 3).map_err(start)?;
            let attrs = attrs.ok_or_else(|| {
                SinkError::StartFailed("no Media Foundation attribute store".into())
            })?;
            attrs
                .SetUINT32(&MF_READWRITE_ENABLE_HARDWARE_TRANSFORMS, 1)
                .map_err(start)?;
            // Throttling is the whole reason this path exists: with it on,
            // `WriteSample` blocks while the encoder is behind instead of
            // taking an unbounded queue of uncompressed frames. It is the
            // default, and set here so that nobody removes it by accident.
            attrs
                .SetUINT32(&MF_SINK_WRITER_DISABLE_THROTTLING, 0)
                .map_err(start)?;
            // The container by name rather than by the scratch file's
            // extension, which is a temporary name and not always ".mp4".
            attrs
                .SetGUID(&MF_TRANSCODE_CONTAINERTYPE, &MFTranscodeContainerType_MPEG4)
                .map_err(start)?;

            let sink =
                MFCreateSinkWriterFromURL(&HSTRING::from(self.scratch.as_os_str()), None, &attrs)
                    .map_err(start)?;
            let video = sink
                .AddStream(&self.video_out().map_err(start)?)
                .map_err(start)?;
            let audio = if self.audio {
                let (out, input) = Self::audio_types().map_err(start)?;
                let i = sink.AddStream(&out).map_err(start)?;
                sink.SetInputMediaType(i, &input, None).map_err(start)?;
                Some(i)
            } else {
                None
            };
            sink.SetInputMediaType(video, &self.video_in().map_err(start)?, None)
                .map_err(start)?;
            sink.BeginWriting().map_err(start)?;
            Writer { sink, video, audio }
        };
        Ok(self.writer.insert(writer).clone())
    }

    /// Wait until the writer is holding less than [`QUEUE_FRAMES`], so the
    /// frame about to be handed over cannot add to a pile.
    ///
    /// Polling rather than waiting on anything: Media Foundation offers no
    /// "there is room" signal, and a millisecond of granularity is a
    /// thirtieth of a frame.
    fn make_room(&self, writer: &Writer) {
        let budget = QUEUE_FRAMES * self.width as u64 * self.height as u64 * 4;
        let deadline = Instant::now() + ROOM_TIMEOUT;
        // A writer that stops reporting is left to its own throttle.
        while let Some(queued) = queued_bytes(writer) {
            if u64::from(queued) <= budget || Instant::now() >= deadline {
                return;
            }
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    /// Hand the held frame to the encoder, waiting if it is behind.
    fn send_last(&mut self) -> Result<(), SinkError> {
        let time = self.frames as i64 * HNS_PER_SECOND / self.fps as i64;
        let duration = HNS_PER_SECOND / self.fps as i64;
        let writer = self.ready()?;
        self.make_room(&writer);
        let frame = sample(&self.last, time, duration).map_err(died)?;
        // SAFETY: a sample built for this stream's input type. This is where
        // the encoder's backpressure is felt.
        unsafe { writer.sink.WriteSample(writer.video, &frame) }.map_err(died)?;
        self.frames += 1;
        Ok(())
    }

    /// How much the writer is holding for the picture, in bytes. `None`
    /// before it has started.
    #[cfg(test)]
    fn queued(&self) -> Option<u32> {
        queued_bytes(self.writer.as_ref()?)
    }
}

/// Uncompressed video the writer has taken but not yet encoded, in bytes.
/// `None` if it will not say, which is not an error worth failing a frame
/// over — it only means the queue cannot be measured.
fn queued_bytes(writer: &Writer) -> Option<u32> {
    let mut stats = MF_SINK_WRITER_STATISTICS {
        cb: size_of::<MF_SINK_WRITER_STATISTICS>() as u32,
        ..Default::default()
    };
    // SAFETY: `cb` is set to the size of the struct being passed, as the call
    // requires, and the struct outlives the call.
    unsafe { writer.sink.GetStatistics(writer.video, &mut stats) }.ok()?;
    Some(stats.dwByteCountQueued)
}

impl VideoSink for NativeSink {
    /// There are no segments here. The first call opens the encoder; a call
    /// after a pause is a no-op, because the same encoder continues and the
    /// frame numbering never jumped.
    fn open_segment(&mut self) -> Result<(), SinkError> {
        self.ready()?;
        Ok(())
    }

    fn write(&mut self, frame: &[u8]) -> Result<(), SinkError> {
        let expected = self.width as usize * self.height as usize * 4;
        if frame.len() != expected {
            return Err(SinkError::EncoderDied(format!(
                "frame is {} bytes, expected {expected}",
                frame.len()
            )));
        }
        self.last.copy_from_slice(frame);
        self.send_last()?;
        self.has_frame = true;
        Ok(())
    }

    /// Top up the owed frames, but keep the encoder open: a pause is not a
    /// boundary in the output, only in the input.
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
        let Some(writer) = self.writer.take() else {
            return Err(SinkError::EncoderDied("nothing was recorded".into()));
        };
        // SAFETY: the writer was begun in `ready` and is finalised once.
        let finalised = unsafe { writer.sink.Finalize() };
        // Let go of the file before anything moves or removes it.
        drop(writer);

        if !self.has_frame {
            let _ = std::fs::remove_file(&self.scratch);
            return Err(SinkError::EncoderDied("nothing was recorded".into()));
        }
        finalised.map_err(died)?;

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

    /// Timestamps are counted from the samples handed over rather than taken
    /// from the caller, which is what keeps the sound monotonic.
    fn write_audio(&mut self, pcm: &[u8]) -> Result<(), SinkError> {
        let Some(writer) = self.writer.clone() else {
            return Ok(());
        };
        let Some(stream) = writer.audio else {
            return Ok(());
        };
        let frames = (pcm.len() / BLOCK) as u64;
        if frames == 0 {
            return Ok(());
        }
        let time = self.audio_frames as i64 * HNS_PER_SECOND / SAMPLE_RATE as i64;
        let duration = frames as i64 * HNS_PER_SECOND / SAMPLE_RATE as i64;
        let chunk = sample(&pcm[..frames as usize * BLOCK], time, duration)
            .map_err(|e| SinkError::EncoderDied(format!("audio: {e}")))?;
        // SAFETY: a sample built for this stream's input type.
        unsafe { writer.sink.WriteSample(stream, &chunk) }
            .map_err(|e| SinkError::EncoderDied(format!("audio: {e}")))?;
        self.audio_frames += frames;
        Ok(())
    }
}

impl Drop for NativeSink {
    fn drop(&mut self) {
        // An abandoned recording should not leave a part-written file behind.
        if let Some(writer) = self.writer.take() {
            drop(writer);
            let _ = std::fs::remove_file(&self.scratch);
        }
    }
}

/// Held by each test that encodes, so they take turns.
///
/// Laptop GPUs cap how many encoding sessions run at once; past the cap,
/// creating one fails with "Invalid pointer". The recorder opens one per
/// screen, well under it, but a parallel test run opens a dozen.
#[cfg(test)]
pub(crate) fn one_encoder_at_a_time() -> std::sync::MutexGuard<'static, ()> {
    static TURN: std::sync::Mutex<()> = std::sync::Mutex::new(());
    TURN.lock().unwrap_or_else(|e| e.into_inner())
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
        let _turn = one_encoder_at_a_time();
        let mut sink = NativeSink::new(w, h, fps, false, temp("basic-scratch")).unwrap();
        sink.open_segment().unwrap();
        for i in 0..60 {
            sink.write(&frame(w, h, i as u8)).unwrap();
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

    /// The picture goes in top row first and has to come out the same way up.
    /// Checked by decoding it again rather than by eye, because the layout is
    /// declared to Media Foundation rather than obvious from the bytes.
    #[test]
    fn the_picture_comes_back_the_same_way_up() {
        let (w, h, fps) = (320u32, 240u32, 30u32);
        let dest = temp("upright");
        let _turn = one_encoder_at_a_time();
        let mut sink = NativeSink::new(w, h, fps, false, temp("upright-scratch")).unwrap();

        // Bright top half, dark bottom half.
        let mut picture = vec![0u8; (w * h * 4) as usize];
        let half = (h / 2) as usize * (w as usize) * 4;
        picture[..half].fill(235);
        sink.open_segment().unwrap();
        for _ in 0..30 {
            sink.write(&picture).unwrap();
        }
        sink.close_segment(0).unwrap();
        sink.finish(&dest).unwrap();

        let mut dec = crate::decoder::VideoDecoder::open(&dest).expect("the file should decode");
        let got = dec
            .frame_at(0.0, fps as f64)
            .expect("a frame should decode")
            .expect("there should be a first frame");
        let row = |y: u32| {
            let i = (y as usize) * (got.width as usize) * 4;
            got.rgba[i] as u32 + got.rgba[i + 1] as u32 + got.rgba[i + 2] as u32
        };
        let (top, bottom) = (row(h / 8), row(h - h / 8));
        assert!(
            top > bottom + 150,
            "the bright half came back at the bottom: top {top}, bottom {bottom}"
        );
        let _ = std::fs::remove_file(&dest);
    }

    /// The property this sink exists for: a caller that outruns the encoder
    /// is made to wait, rather than having its frames taken into a queue
    /// that grows until the machine runs out of memory.
    #[test]
    fn frames_handed_over_flat_out_do_not_pile_up() {
        let (w, h, fps) = (1920u32, 1080u32, 30u32);
        let dest = temp("backpressure");
        let frame_bytes = w * h * 4;
        let _turn = one_encoder_at_a_time();
        let mut sink = NativeSink::new(w, h, fps, false, temp("backpressure-scratch")).unwrap();
        sink.open_segment().unwrap();

        // Ten seconds of 1080p30: 2.5GB if none of it is encoded on the way.
        let picture = frame(w, h, 90);
        assert!(
            sink.queued().is_some(),
            "without a queue reading this test proves nothing"
        );
        let mut peak = 0u32;
        for n in 0..300 {
            sink.write(&picture).unwrap();
            peak = peak.max(sink.queued().unwrap_or(0));
            // The frame being written, plus the budget, plus one for the
            // moment between the two.
            let allowed = frame_bytes * (QUEUE_FRAMES as u32 + 2);
            assert!(
                peak <= allowed,
                "the writer was holding {peak} bytes after {n} frames, over the \
                 {allowed} this sink allows"
            );
        }
        sink.close_segment(0).unwrap();
        sink.finish(&dest).unwrap();
        let _ = std::fs::remove_file(&dest);
    }

    #[test]
    fn padding_lengthens_the_output() {
        let (w, h, fps) = (320u32, 240u32, 30u32);
        let dest = temp("pad");
        let _turn = one_encoder_at_a_time();
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
        let _turn = one_encoder_at_a_time();
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
    fn sound_is_written_alongside_the_picture() {
        let (w, h, fps) = (320u32, 240u32, 30u32);
        let dest = temp("sound");
        let _turn = one_encoder_at_a_time();
        let mut sink = NativeSink::new(w, h, fps, true, temp("sound-scratch")).unwrap();
        sink.open_segment().unwrap();
        // A second of a tone, a frame at a time.
        let per_frame = SAMPLE_RATE as usize / fps as usize;
        for n in 0..fps {
            sink.write(&frame(w, h, 60)).unwrap();
            let pcm: Vec<u8> = (0..per_frame)
                .flat_map(|i| {
                    let t = (n as usize * per_frame + i) as f64 / SAMPLE_RATE as f64;
                    let s = ((t * 330.0 * std::f64::consts::TAU).sin() * 6000.0) as i16;
                    let b = s.to_le_bytes();
                    [b[0], b[1], b[0], b[1]]
                })
                .collect();
            sink.write_audio(&pcm).unwrap();
        }
        sink.close_segment(0).unwrap();
        sink.finish(&dest).unwrap();

        assert!(
            crate::decoder::AudioDecoder::open(&dest).is_ok(),
            "the file should carry a readable sound track"
        );
        let _ = std::fs::remove_file(&dest);
    }

    #[test]
    fn a_wrong_sized_frame_is_rejected() {
        let _turn = one_encoder_at_a_time();
        let mut sink = NativeSink::new(320, 240, 30, false, temp("bad-scratch")).unwrap();
        sink.open_segment().unwrap();
        assert!(sink.write(&[0u8; 16]).is_err());
    }

    #[test]
    fn finishing_with_no_frames_is_an_error_not_an_empty_file() {
        let dest = temp("empty");
        let scratch = temp("empty-scratch");
        let _turn = one_encoder_at_a_time();
        let mut sink = NativeSink::new(320, 240, 30, false, scratch.clone()).unwrap();
        sink.open_segment().unwrap();
        assert!(sink.finish(&dest).is_err());
        assert!(!dest.exists());
        assert!(!scratch.exists(), "the part-written file should be gone");
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
            let _turn = one_encoder_at_a_time();
            let mut sink = NativeSink::new(320, 240, 30, false, scratch.clone()).unwrap();
            sink.open_segment().unwrap();
            sink.write(&frame(320, 240, 5)).unwrap();
        }
        assert!(!scratch.exists(), "dropping must not leak a partial file");
    }
}
