//! Decoding recordings for playback, through Media Foundation.
//!
//! The counterpart to [`encoder`](crate::encoder): Windows decodes H.264 and
//! AAC itself, so the player needs no ffmpeg either.
//!
//! Video is read as NV12 and converted here rather than asking the reader for
//! RGB: on 1080p the reader's own RGB path decodes at about 100fps against
//! about 540fps for NV12, and two 60fps screens need 120.
//!
//! Neither decoder is `Send`: a COM source reader stays on the thread that
//! made it, so each playback thread opens its own.

use std::path::Path;
use std::sync::Once;

use screenilux_core::yuv::{Matrix, nv12_len, nv12_to_rgba};
use windows::Win32::Media::MediaFoundation::*;
use windows::Win32::System::Com::StructuredStorage::PROPVARIANT;
use windows::Win32::System::Com::{COINIT_MULTITHREADED, CoInitializeEx};
use windows::Win32::System::Variant::VT_I8;
use windows::core::{GUID, HSTRING};

/// Media Foundation timestamps are in 100-nanosecond units.
const HNS: f64 = 10_000_000.0;

const VIDEO: u32 = MF_SOURCE_READER_FIRST_VIDEO_STREAM.0 as u32;
const AUDIO: u32 = MF_SOURCE_READER_FIRST_AUDIO_STREAM.0 as u32;

#[derive(Debug, thiserror::Error)]
pub enum DecodeError {
    #[error("could not open the video: {0}")]
    Open(String),
    #[error("the file has no {0} stream")]
    NoStream(&'static str),
    #[error("decoding failed: {0}")]
    Read(String),
}

fn read_err(e: windows::core::Error) -> DecodeError {
    DecodeError::Read(e.to_string())
}

/// Media Foundation and COM, ready on this thread.
///
/// `MFStartup` is reference-counted and process-wide, so it is called once and
/// never shut down: the player opens and closes decoders freely and an
/// accidental final `MFShutdown` would break the next one. COM is per thread.
fn init_thread() {
    static STARTUP: Once = Once::new();
    // SAFETY: plain initialisation calls. A thread already in another COM
    // apartment reports RPC_E_CHANGED_MODE, which is fine: COM is usable.
    unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        STARTUP.call_once(|| {
            if let Err(e) = MFStartup(MF_VERSION, MFSTARTUP_FULL) {
                tracing::error!(error = %e, "Media Foundation did not start");
            }
        });
    }
}

fn open_reader(path: &Path) -> Result<IMFSourceReader, DecodeError> {
    init_thread();
    // SAFETY: documented Media Foundation calls on owned interfaces.
    unsafe {
        let mut attrs: Option<IMFAttributes> = None;
        MFCreateAttributes(&mut attrs, 1).map_err(|e| DecodeError::Open(e.to_string()))?;
        let attrs = attrs.ok_or_else(|| DecodeError::Open("no attribute store".into()))?;
        MFCreateSourceReaderFromURL(&HSTRING::from(path.as_os_str()), &attrs)
            .map_err(|e| DecodeError::Open(e.to_string()))
    }
}

/// Seconds of media, from the container.
fn duration_of(reader: &IMFSourceReader) -> f64 {
    // SAFETY: MF_PD_DURATION is documented as VT_UI8; the union member read
    // matches it, and an absent attribute is an error rather than garbage.
    unsafe {
        reader
            .GetPresentationAttribute(MF_SOURCE_READER_MEDIASOURCE.0 as u32, &MF_PD_DURATION)
            .map(|p| p.Anonymous.Anonymous.Anonymous.uhVal as f64 / HNS)
            .unwrap_or(0.0)
    }
}

fn seek_reader(reader: &IMFSourceReader, t: f64) -> Result<(), DecodeError> {
    let mut pos = PROPVARIANT::default();
    // SAFETY: setting the tag and its matching union member on a zeroed
    // PROPVARIANT. VT_I8 owns nothing, so no clear is needed.
    unsafe {
        let inner = &mut *pos.Anonymous.Anonymous;
        inner.vt = VT_I8;
        inner.Anonymous.hVal = (t.max(0.0) * HNS) as i64;
        reader
            .SetCurrentPosition(&GUID::zeroed(), &pos)
            .map_err(read_err)
    }
}

/// One sample's bytes, with the stream flags and timestamp.
enum Read {
    Sample {
        t: f64,
        bytes: Vec<u8>,
    },
    /// A gap or format notice with no data; read again.
    Nothing,
    End,
}

/// Read one sample from `stream`, copying its bytes only if `copy` is set.
///
/// Skipping the copy matters when seeking: the frames between a keyframe and
/// the target are decoded but never shown.
fn read(
    reader: &IMFSourceReader,
    stream: u32,
    copy: bool,
    on_type_change: &mut dyn FnMut(),
) -> Result<Read, DecodeError> {
    let mut flags = 0u32;
    let mut ts = 0i64;
    let mut sample: Option<IMFSample> = None;
    // SAFETY: synchronous ReadSample with valid out pointers; the buffer is
    // locked only for the copy and unlocked on every path.
    unsafe {
        reader
            .ReadSample(
                stream,
                0,
                None,
                Some(&mut flags),
                Some(&mut ts),
                Some(&mut sample),
            )
            .map_err(read_err)?;
        if flags & MF_SOURCE_READERF_ERROR.0 as u32 != 0 {
            return Err(DecodeError::Read("the stream reported an error".into()));
        }
        if flags & MF_SOURCE_READERF_CURRENTMEDIATYPECHANGED.0 as u32 != 0 {
            on_type_change();
        }
        if flags & MF_SOURCE_READERF_ENDOFSTREAM.0 as u32 != 0 {
            return Ok(Read::End);
        }
        let Some(sample) = sample else {
            return Ok(Read::Nothing);
        };
        let t = ts as f64 / HNS;
        if !copy {
            return Ok(Read::Sample {
                t,
                bytes: Vec::new(),
            });
        }
        let buffer = sample.ConvertToContiguousBuffer().map_err(read_err)?;
        let mut data = std::ptr::null_mut();
        let mut len = 0u32;
        buffer
            .Lock(&mut data, None, Some(&mut len))
            .map_err(read_err)?;
        let bytes = std::slice::from_raw_parts(data, len as usize).to_vec();
        let _ = buffer.Unlock();
        Ok(Read::Sample { t, bytes })
    }
}

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
    let dec = VideoDecoder::open(path)?;
    let has_audio = AudioDecoder::open(path).is_ok();
    Ok(MediaInfo {
        width: dec.width,
        height: dec.height,
        duration: dec.duration,
        has_audio,
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

/// Frames of one video, in order.
pub struct VideoDecoder {
    reader: IMFSourceReader,
    width: u32,
    height: u32,
    stride: usize,
    matrix: Matrix,
    duration: f64,
    /// Set when the reader announces a new output format, so the next frame
    /// re-reads its geometry before converting.
    stale_format: bool,
}

impl VideoDecoder {
    pub fn open(path: &Path) -> Result<Self, DecodeError> {
        let reader = open_reader(path)?;
        // SAFETY: documented Media Foundation calls on owned interfaces.
        unsafe {
            let ty = MFCreateMediaType().map_err(read_err)?;
            ty.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)
                .map_err(read_err)?;
            ty.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_NV12)
                .map_err(read_err)?;
            reader
                .SetCurrentMediaType(VIDEO, None, &ty)
                .map_err(|_| DecodeError::NoStream("video"))?;
            // Only video: an unread audio stream would otherwise queue up
            // decoded samples behind every video read.
            let _ = reader.SetStreamSelection(MF_SOURCE_READER_ALL_STREAMS.0 as u32, false);
            reader.SetStreamSelection(VIDEO, true).map_err(read_err)?;
        }
        let duration = duration_of(&reader);
        let mut dec = Self {
            reader,
            width: 0,
            height: 0,
            stride: 0,
            matrix: Matrix::Bt709,
            duration,
            stale_format: false,
        };
        dec.refresh_format()?;
        Ok(dec)
    }

    /// Visible size, stride and colour matrix from the current output type.
    ///
    /// H.264 codes in 16-line blocks, so 1080p arrives as 1088 rows. The
    /// visible size is the display aperture when the type carries one.
    fn refresh_format(&mut self) -> Result<(), DecodeError> {
        // SAFETY: attribute reads on the reader's current type. The aperture
        // blob is read into a correctly sized, plain-data struct.
        unsafe {
            let ty = self.reader.GetCurrentMediaType(VIDEO).map_err(read_err)?;
            let size = ty.GetUINT64(&MF_MT_FRAME_SIZE).map_err(read_err)?;
            let (coded_w, coded_h) = ((size >> 32) as u32, size as u32);

            let mut area = MFVideoArea::default();
            let blob = std::slice::from_raw_parts_mut(
                (&mut area as *mut MFVideoArea).cast::<u8>(),
                std::mem::size_of::<MFVideoArea>(),
            );
            let (w, h) = match ty.GetBlob(&MF_MT_MINIMUM_DISPLAY_APERTURE, blob, None) {
                Ok(()) if area.Area.cx > 0 && area.Area.cy > 0 => {
                    (area.Area.cx as u32, area.Area.cy as u32)
                }
                _ => (coded_w, coded_h),
            };
            self.width = w.min(coded_w);
            self.height = h.min(coded_h);

            self.stride = ty
                .GetUINT32(&MF_MT_DEFAULT_STRIDE)
                .map(|s| (s as i32).unsigned_abs() as usize)
                .unwrap_or(coded_w as usize)
                .max(self.width as usize);

            self.matrix = match ty.GetUINT32(&MF_MT_YUV_MATRIX) {
                Ok(m) if m == MFVideoTransferMatrix_BT601.0 as u32 => Matrix::Bt601,
                Ok(m) if m == MFVideoTransferMatrix_BT709.0 as u32 => Matrix::Bt709,
                _ => Matrix::for_height(self.height),
            };
        }
        self.stale_format = false;
        Ok(())
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

    fn read(&mut self, copy: bool) -> Result<Option<Read>, DecodeError> {
        let mut changed = false;
        let r = read(&self.reader, VIDEO, copy, &mut || changed = true)?;
        if changed {
            self.stale_format = true;
        }
        Ok(Some(r))
    }

    fn convert(&mut self, t: f64, bytes: &[u8]) -> Result<VideoFrame, DecodeError> {
        if self.stale_format {
            self.refresh_format()?;
        }
        let rows = bytes.len() / self.stride * 2 / 3;
        if rows < self.height as usize || bytes.len() < nv12_len(self.stride, rows) {
            return Err(DecodeError::Read(format!(
                "a {}-byte frame is too small for {}x{}",
                bytes.len(),
                self.width,
                self.height
            )));
        }
        let mut rgba = vec![0u8; self.width as usize * self.height as usize * 4];
        nv12_to_rgba(
            bytes,
            self.stride,
            rows,
            self.width as usize,
            self.height as usize,
            self.matrix,
            &mut rgba,
        );
        Ok(VideoFrame {
            t,
            width: self.width,
            height: self.height,
            rgba,
        })
    }

    /// The next frame, or `None` at the end.
    pub fn next_frame(&mut self) -> Result<Option<VideoFrame>, DecodeError> {
        loop {
            match self.read(true)? {
                Some(Read::Sample { t, bytes }) => return self.convert(t, &bytes).map(Some),
                Some(Read::Nothing) => continue,
                Some(Read::End) | None => return Ok(None),
            }
        }
    }

    /// The next frame's timestamp, with its pixels only if `want` says so.
    ///
    /// For walking a whole video in one pass, as the text indexer does: every
    /// frame has to be decoded, but converting the ones that are not sampled
    /// would be most of the work.
    pub fn next_frame_if(
        &mut self,
        want: impl Fn(f64) -> bool,
    ) -> Result<Option<(f64, Option<VideoFrame>)>, DecodeError> {
        loop {
            match self.read(true)? {
                Some(Read::Sample { t, bytes }) => {
                    let frame = if want(t) {
                        Some(self.convert(t, &bytes)?)
                    } else {
                        None
                    };
                    return Ok(Some((t, frame)));
                }
                Some(Read::Nothing) => continue,
                Some(Read::End) | None => return Ok(None),
            }
        }
    }

    /// The frame showing at `t`: the first whose timestamp reaches `t`, less
    /// half a frame of tolerance.
    ///
    /// Media Foundation seeks to the keyframe at or before the target, which
    /// can be seconds early, so this decodes forward from there. Frames on
    /// the way are decoded but not converted.
    pub fn frame_at(&mut self, t: f64, fps: f64) -> Result<Option<VideoFrame>, DecodeError> {
        let half_frame = 0.5 / fps.max(1.0);
        // Media Foundation refuses a position past the end outright, so ask
        // for the last frame instead.
        let t = if self.duration > 0.0 {
            t.min(self.duration - half_frame).max(0.0)
        } else {
            t.max(0.0)
        };
        seek_reader(&self.reader, t)?;
        let target = t - half_frame;
        let mut last = None;
        loop {
            // Copy only once near the target; most of the way is skipped.
            match self.read(true)? {
                Some(Read::Sample { t: ts, bytes }) => {
                    if ts >= target {
                        return self.convert(ts, &bytes).map(Some);
                    }
                    last = Some((ts, bytes));
                }
                Some(Read::Nothing) => continue,
                // Past the last frame: show the last one there was.
                Some(Read::End) | None => {
                    return match last {
                        Some((ts, bytes)) => self.convert(ts, &bytes).map(Some),
                        None => Ok(None),
                    };
                }
            }
        }
    }
}

/// What [`AudioDecoder`] produces.
pub const PLAYBACK_RATE: u32 = 48_000;
pub const PLAYBACK_CHANNELS: u16 = 2;
/// Bytes per sample frame: 16-bit stereo.
pub const PLAYBACK_BLOCK: usize = 4;

/// A file's sound, as 48kHz 16-bit stereo PCM.
pub struct AudioDecoder {
    reader: IMFSourceReader,
}

impl AudioDecoder {
    pub fn open(path: &Path) -> Result<Self, DecodeError> {
        let reader = open_reader(path)?;
        // SAFETY: documented Media Foundation calls on owned interfaces. The
        // reader inserts its own resampler for a partial PCM type.
        unsafe {
            let ty = MFCreateMediaType().map_err(read_err)?;
            ty.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Audio)
                .map_err(read_err)?;
            ty.SetGUID(&MF_MT_SUBTYPE, &MFAudioFormat_PCM)
                .map_err(read_err)?;
            ty.SetUINT32(&MF_MT_AUDIO_SAMPLES_PER_SECOND, PLAYBACK_RATE)
                .map_err(read_err)?;
            ty.SetUINT32(&MF_MT_AUDIO_NUM_CHANNELS, PLAYBACK_CHANNELS as u32)
                .map_err(read_err)?;
            ty.SetUINT32(&MF_MT_AUDIO_BITS_PER_SAMPLE, 16)
                .map_err(read_err)?;
            reader
                .SetCurrentMediaType(AUDIO, None, &ty)
                .map_err(|_| DecodeError::NoStream("audio"))?;
            let _ = reader.SetStreamSelection(MF_SOURCE_READER_ALL_STREAMS.0 as u32, false);
            reader.SetStreamSelection(AUDIO, true).map_err(read_err)?;
        }
        Ok(Self { reader })
    }

    /// Start reading from `t`. The first chunk is trimmed so its first sample
    /// is at `t`, not at the start of the compressed frame containing it.
    pub fn seek(&mut self, t: f64) -> Result<Vec<u8>, DecodeError> {
        seek_reader(&self.reader, t)?;
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
        loop {
            match read(&self.reader, AUDIO, true, &mut || {})? {
                Read::Sample { t, bytes } => return Ok(Some((t, bytes))),
                Read::Nothing => continue,
                Read::End => return Ok(None),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use screenilux_core::ports::VideoSink;

    fn temp(name: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!(
            "screenilux-decode-{}-{}-{name}.mp4",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ))
    }

    /// Frame `n`: top half red, bottom half blue, and a white marker whose x
    /// position encodes `n`, so a decoded frame says which frame it is.
    fn frame(w: u32, h: u32, n: u32) -> Vec<u8> {
        let mut buf = vec![0u8; (w * h * 4) as usize];
        for y in 0..h {
            for x in 0..w {
                let i = ((y * w + x) * 4) as usize;
                let marker = x / 16 == n && y >= h * 3 / 4;
                let bgra = if marker {
                    [255, 255, 255, 255]
                } else if y < h / 2 {
                    [0, 0, 220, 255]
                } else {
                    [220, 0, 0, 255]
                };
                buf[i..i + 4].copy_from_slice(&bgra);
            }
        }
        buf
    }

    fn which(f: &VideoFrame) -> Option<u32> {
        let y = f.height * 7 / 8;
        (0..f.width).find_map(|x| {
            let i = ((y * f.width + x) * 4) as usize;
            (f.rgba[i] > 200 && f.rgba[i + 1] > 200 && f.rgba[i + 2] > 200).then_some(x / 16)
        })
    }

    fn encode(w: u32, h: u32, frames: u32) -> std::path::PathBuf {
        let _turn = crate::encoder::one_encoder_at_a_time();
        let dest = temp("src");
        let mut sink =
            crate::encoder::NativeSink::new(w, h, 30, false, temp("src-scratch")).unwrap();
        sink.open_segment().unwrap();
        for n in 0..frames {
            sink.write(&frame(w, h, n)).unwrap();
        }
        sink.close_segment(0).unwrap();
        sink.finish(&dest).unwrap();
        dest
    }

    /// Whether `f`, decoded as frame `n`, shows it.
    ///
    /// Frame 0 may show frame 1 instead. On some GPUs (seen on NVIDIA) the
    /// transcoder Windows runs the encoder in loses the first sample and fills
    /// its slot with the next picture; anything sent within the first frame's
    /// interval is lost the same way, so it cannot be worked around from here.
    /// Every later frame keeps its picture and its time.
    fn shows(f: &VideoFrame, n: u32) -> bool {
        match which(f) {
            Some(0 | 1) if n == 0 => true,
            got => got == Some(n),
        }
    }

    #[test]
    fn decodes_every_frame_in_order_at_the_right_size() {
        // 1080 is not a multiple of 16, so this also covers cropping.
        let path = encode(1920, 1080, 45);
        let mut dec = VideoDecoder::open(&path).unwrap();
        assert_eq!((dec.width(), dec.height()), (1920, 1080));
        assert!((dec.duration() - 1.5).abs() < 0.05, "{}", dec.duration());

        let mut n = 0;
        while let Some(f) = dec.next_frame().unwrap() {
            assert_eq!(f.rgba.len(), 1920 * 1080 * 4);
            assert!(shows(&f, n), "frame {n} decoded as {:?}", which(&f));
            assert!((f.t - n as f64 / 30.0).abs() < 0.01);
            n += 1;
        }
        assert_eq!(n, 45);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn colours_come_back_right_side_up() {
        let path = encode(1280, 720, 3);
        let mut dec = VideoDecoder::open(&path).unwrap();
        let f = dec.next_frame().unwrap().unwrap();
        let px = |x: u32, y: u32| {
            let i = ((y * f.width + x) * 4) as usize;
            [f.rgba[i], f.rgba[i + 1], f.rgba[i + 2], f.rgba[i + 3]]
        };
        let top = px(640, 100);
        let bottom = px(640, 500);
        assert!(
            top[0] > 190 && top[2] < 40,
            "top should be red, got {top:?}"
        );
        assert!(
            bottom[2] > 190 && bottom[0] < 40,
            "bottom should be blue, got {bottom:?}"
        );
        assert_eq!(top[3], 255, "opaque");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn seeking_lands_on_the_asked_frame_not_the_keyframe() {
        // Wide enough for 60 markers 16px apart.
        let path = encode(1280, 480, 60);
        let mut dec = VideoDecoder::open(&path).unwrap();
        for target in [40u32, 7, 59, 0] {
            let f = dec.frame_at(target as f64 / 30.0, 30.0).unwrap().unwrap();
            assert!(shows(&f, target), "seek to {target} gave {:?}", which(&f));
        }
        // Past the end shows the last frame rather than nothing.
        let f = dec.frame_at(99.0, 30.0).unwrap().unwrap();
        assert_eq!(which(&f), Some(59));
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn a_silent_recording_has_no_audio_to_decode() {
        let path = encode(320, 240, 5);
        assert!(matches!(
            AudioDecoder::open(&path),
            Err(DecodeError::NoStream("audio"))
        ));
        let info = probe(&path).unwrap();
        assert!(!info.has_audio);
        assert_eq!((info.width, info.height), (320, 240));
        let _ = std::fs::remove_file(&path);
    }

    /// Two seconds of picture with a second of tone written alongside.
    fn encode_with_sound() -> std::path::PathBuf {
        let (w, h) = (320u32, 240u32);
        let _turn = crate::encoder::one_encoder_at_a_time();
        let dest = temp("sound");
        let mut sink =
            crate::encoder::NativeSink::new(w, h, 30, true, temp("sound-scratch")).unwrap();
        sink.open_segment().unwrap();
        let tone: Vec<u8> = (0..PLAYBACK_RATE * 2)
            .flat_map(|i| {
                let s = ((i as f64 * 440.0 * std::f64::consts::TAU / PLAYBACK_RATE as f64).sin()
                    * 8000.0) as i16;
                let b = s.to_le_bytes();
                [b[0], b[1], b[0], b[1]]
            })
            .collect();
        for (n, chunk) in tone
            .chunks(PLAYBACK_RATE as usize * PLAYBACK_BLOCK / 30)
            .enumerate()
        {
            sink.write(&frame(w, h, n as u32 % 20)).unwrap();
            sink.write_audio(chunk).unwrap();
        }
        sink.close_segment(0).unwrap();
        sink.finish(&dest).unwrap();
        dest
    }

    #[test]
    fn sound_decodes_to_its_full_length_and_seeks_to_the_sample() {
        let path = encode_with_sound();
        assert!(probe(&path).unwrap().has_audio);

        let mut dec = AudioDecoder::open(&path).unwrap();
        let mut bytes = 0usize;
        let mut loud = false;
        while let Some((_, pcm)) = dec.next_chunk().unwrap() {
            bytes += pcm.len();
            loud |= pcm
                .as_chunks::<2>()
                .0
                .iter()
                .any(|s| i16::from_le_bytes([s[0], s[1]]).abs() > 4000);
        }
        let secs = (bytes / PLAYBACK_BLOCK) as f64 / PLAYBACK_RATE as f64;
        assert!((secs - 2.0).abs() < 0.1, "decoded {secs:.3}s of sound");
        assert!(loud, "the tone should survive encoding");

        // Seeking trims to the exact sample: what remains is the rest.
        let head = dec.seek(1.5).unwrap();
        let mut rest = head.len();
        while let Some((_, pcm)) = dec.next_chunk().unwrap() {
            rest += pcm.len();
        }
        let left = (rest / PLAYBACK_BLOCK) as f64 / PLAYBACK_RATE as f64;
        assert!((left - 0.5).abs() < 0.05, "{left:.3}s left after 1.5s");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn a_missing_file_is_an_error() {
        assert!(VideoDecoder::open(Path::new("C:/definitely/not/here.mp4")).is_err());
    }
}
