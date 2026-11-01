//! Can the player decode our own MP4s natively, and can Windows OCR read them?
//!
//! The player has to decode without ffmpeg, the same way recording encodes
//! without it. This settles the questions the player design depends on:
//!
//! - Does Media Foundation's source reader hand back BGRA (RGB32) we can
//!   upload as-is, and which way up?
//! - H.264 codes 1080 lines as 1088. Is the padding visible in the buffer, and
//!   does the stride say so?
//! - Are timestamps the ones the encoder wrote, and does seeking land where
//!   asked rather than on the previous keyframe?
//! - How fast is it, against the 60fps a player needs per screen?
//! - Does the built-in OCR engine exist here, and what does it read off a real
//!   screen capture?
//!
//!   cargo run --release -p screenilux-win --example spike_decode

use std::path::Path;
use std::time::{Duration, Instant};

use screenilux_core::ports::{ScreenSource, VideoSink};
use windows::Win32::Media::MediaFoundation::*;
use windows::Win32::System::Com::StructuredStorage::PROPVARIANT;
use windows::Win32::System::Com::{COINIT_MULTITHREADED, CoInitializeEx};
use windows::Win32::System::Variant::VT_I8;
use windows::core::HSTRING;

const W: u32 = 1920;
const H: u32 = 1080;
const FPS: u32 = 30;
const FRAMES: u32 = 90;

/// Top third red, the rest blue, with frame `n` drawn as a white bar whose
/// x position encodes the frame number, so a decoded frame can say which
/// frame it is.
fn frame(n: u32) -> Vec<u8> {
    let mut buf = vec![0u8; (W * H * 4) as usize];
    for y in 0..H {
        for x in 0..W {
            let i = ((y * W + x) * 4) as usize;
            let (b, g, r) = if y < H / 3 { (0, 0, 220) } else { (220, 0, 0) };
            let bar = x / 16 == n && y > H / 2;
            buf[i] = if bar { 255 } else { b };
            buf[i + 1] = if bar { 255 } else { g };
            buf[i + 2] = if bar { 255 } else { r };
            buf[i + 3] = 255;
        }
    }
    buf
}

fn encode(path: &Path) {
    let scratch = path.with_extension("part.mp4");
    let mut sink = screenilux_win::NativeSink::new(W, H, FPS, false, scratch).unwrap();
    sink.open_segment().unwrap();
    for n in 0..FRAMES {
        sink.write(&frame(n)).unwrap();
    }
    sink.close_segment(0).unwrap();
    sink.finish(path).unwrap();
}

fn i8_variant(v: i64) -> PROPVARIANT {
    let mut p = PROPVARIANT::default();
    // SAFETY: writing the tag and the matching union member of a zeroed
    // PROPVARIANT; nothing is owned, so no clear is needed.
    unsafe {
        let inner = &mut *p.Anonymous.Anonymous;
        inner.vt = VT_I8;
        inner.Anonymous.hVal = v;
    }
    p
}

/// Which frame does this decoded buffer show? Finds the white bar.
fn frame_number(buf: &[u8], stride: usize, top_down: bool) -> Option<u32> {
    let y = H * 3 / 4;
    let row = if top_down { y } else { H - 1 - y } as usize;
    (0..W).find_map(|x| {
        let i = row * stride + x as usize * 4;
        (buf[i] > 200 && buf[i + 1] > 200 && buf[i + 2] > 200).then_some(x / 16)
    })
}

fn decode(path: &Path) {
    unsafe {
        let mut attrs: Option<IMFAttributes> = None;
        MFCreateAttributes(&mut attrs, 2).unwrap();
        let attrs = attrs.unwrap();
        attrs
            .SetUINT32(&MF_SOURCE_READER_ENABLE_VIDEO_PROCESSING, 1)
            .unwrap();
        let reader = MFCreateSourceReaderFromURL(&HSTRING::from(path.as_os_str()), &attrs).unwrap();

        let stream = MF_SOURCE_READER_FIRST_VIDEO_STREAM.0 as u32;
        let ty = MFCreateMediaType().unwrap();
        ty.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video).unwrap();
        ty.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_RGB32).unwrap();
        reader.SetCurrentMediaType(stream, None, &ty).unwrap();

        let cur = reader.GetCurrentMediaType(stream).unwrap();
        let size = cur.GetUINT64(&MF_MT_FRAME_SIZE).unwrap();
        let (fw, fh) = ((size >> 32) as u32, size as u32);
        let stride = cur.GetUINT32(&MF_MT_DEFAULT_STRIDE).map(|s| s as i32);
        println!("output frame size {fw}x{fh}, default stride {stride:?}");

        let dur = reader
            .GetPresentationAttribute(MF_SOURCE_READER_MEDIASOURCE.0 as u32, &MF_PD_DURATION)
            .unwrap();
        let hns = dur.Anonymous.Anonymous.Anonymous.uhVal;
        println!("duration {:.3}s", hns as f64 / 1e7);

        let mut n = 0u32;
        let mut first_ts = None;
        let start = Instant::now();
        let mut mismatches = 0;
        loop {
            let mut flags = 0u32;
            let mut ts = 0i64;
            let mut sample: Option<IMFSample> = None;
            reader
                .ReadSample(
                    stream,
                    0,
                    None,
                    Some(&mut flags),
                    Some(&mut ts),
                    Some(&mut sample),
                )
                .unwrap();
            if flags & MF_SOURCE_READERF_ENDOFSTREAM.0 as u32 != 0 {
                break;
            }
            let Some(sample) = sample else { continue };
            let buffer = sample.ConvertToContiguousBuffer().unwrap();
            let mut data = std::ptr::null_mut();
            let mut len = 0u32;
            buffer.Lock(&mut data, None, Some(&mut len)).unwrap();
            let bytes = std::slice::from_raw_parts(data, len as usize);
            if n == 0 {
                first_ts = Some(ts);
                println!(
                    "buffer {len} bytes = {} rows of {}",
                    len as usize / (fw as usize * 4),
                    fw * 4
                );
                // Top third should be red in BGRA if the buffer is top-down.
                let top = &bytes[(10 * fw * 4) as usize..][..4];
                let bottom = &bytes[((H - 10) * fw * 4) as usize..][..4];
                println!("row 10 BGRA {top:?}, row {} BGRA {bottom:?}", H - 10);
            }
            let top_down = true;
            if frame_number(bytes, fw as usize * 4, top_down) != Some(n) {
                mismatches += 1;
            }
            buffer.Unlock().unwrap();
            n += 1;
        }
        let secs = start.elapsed().as_secs_f64();
        println!(
            "decoded {n} frames in {secs:.3}s = {:.0} fps; first ts {first_ts:?}; {mismatches} frames not where expected",
            n as f64 / secs
        );

        // Seek to frame 45 (1.5s) and see which frame comes out first.
        let target = 45i64 * 10_000_000 / FPS as i64;
        reader
            .SetCurrentPosition(&windows::core::GUID::zeroed(), &i8_variant(target))
            .unwrap();
        let mut flags = 0u32;
        let mut ts = 0i64;
        let mut sample: Option<IMFSample> = None;
        reader
            .ReadSample(
                stream,
                0,
                None,
                Some(&mut flags),
                Some(&mut ts),
                Some(&mut sample),
            )
            .unwrap();
        let sample = sample.expect("a frame after seeking");
        let buffer = sample.ConvertToContiguousBuffer().unwrap();
        let mut data = std::ptr::null_mut();
        let mut len = 0u32;
        buffer.Lock(&mut data, None, Some(&mut len)).unwrap();
        let bytes = std::slice::from_raw_parts(data, len as usize);
        let shown = frame_number(bytes, fw as usize * 4, true);
        buffer.Unlock().unwrap();
        println!(
            "after seek to {:.3}s: first sample ts {:.3}s shows frame {shown:?}",
            target as f64 / 1e7,
            ts as f64 / 1e7
        );
    }
}

/// Decode every frame and time it, for one output format and reader setup.
fn decode_speed(
    path: &Path,
    label: &str,
    subtype: windows::core::GUID,
    attr: Option<windows::core::GUID>,
) {
    unsafe {
        let mut attrs: Option<IMFAttributes> = None;
        MFCreateAttributes(&mut attrs, 2).unwrap();
        let attrs = attrs.unwrap();
        if let Some(a) = attr {
            attrs.SetUINT32(&a, 1).unwrap();
        }
        let reader = MFCreateSourceReaderFromURL(&HSTRING::from(path.as_os_str()), &attrs).unwrap();
        let stream = MF_SOURCE_READER_FIRST_VIDEO_STREAM.0 as u32;
        let ty = MFCreateMediaType().unwrap();
        ty.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video).unwrap();
        ty.SetGUID(&MF_MT_SUBTYPE, &subtype).unwrap();
        if let Err(e) = reader.SetCurrentMediaType(stream, None, &ty) {
            println!("{label}: refused ({e})");
            return;
        }
        let start = Instant::now();
        let mut n = 0;
        loop {
            let mut flags = 0u32;
            let mut sample: Option<IMFSample> = None;
            reader
                .ReadSample(stream, 0, None, Some(&mut flags), None, Some(&mut sample))
                .unwrap();
            if flags & MF_SOURCE_READERF_ENDOFSTREAM.0 as u32 != 0 {
                break;
            }
            if let Some(s) = sample {
                let b = s.ConvertToContiguousBuffer().unwrap();
                let mut data = std::ptr::null_mut();
                let mut len = 0u32;
                b.Lock(&mut data, None, Some(&mut len)).unwrap();
                b.Unlock().unwrap();
                n += 1;
            }
        }
        let secs = start.elapsed().as_secs_f64();
        println!("{label}: {n} frames, {:.0} fps", n as f64 / secs);
    }
}

fn ocr() {
    use windows::Graphics::Imaging::{BitmapPixelFormat, SoftwareBitmap};
    use windows::Media::Ocr::OcrEngine;
    use windows::Security::Cryptography::CryptographicBuffer;

    let monitors = screenilux_win::monitors().unwrap();
    let primary = monitors.iter().find(|m| m.is_primary).unwrap();
    let mut cap = screenilux_win::ScreenCapture::start(primary.bounds).unwrap();
    cap.wait_for_first_frame(Duration::from_secs(5)).unwrap();
    let f = cap.next_frame(Duration::from_millis(500)).unwrap();
    let (w, h) = (f.width, f.height);
    let pixels = f.pixels.to_vec();

    println!(
        "OCR max image dimension: {:?}",
        OcrEngine::MaxImageDimension()
    );
    let engine = match OcrEngine::TryCreateFromUserProfileLanguages() {
        Ok(e) => e,
        Err(e) => {
            println!("no OCR engine: {e}");
            return;
        }
    };
    let start = Instant::now();
    let buf = CryptographicBuffer::CreateFromByteArray(&pixels).unwrap();
    let bmp =
        SoftwareBitmap::CreateCopyFromBuffer(&buf, BitmapPixelFormat::Bgra8, w as i32, h as i32)
            .unwrap();
    let result = engine.RecognizeAsync(&bmp).unwrap().join().unwrap();
    let lines = result.Lines().unwrap();
    let mut words = 0;
    let mut sample = Vec::new();
    for line in &lines {
        for word in &line.Words().unwrap() {
            words += 1;
            if sample.len() < 12 {
                let r = word.BoundingRect().unwrap();
                sample.push(format!(
                    "{:?}@({:.0},{:.0} {:.0}x{:.0})",
                    word.Text().unwrap().to_string(),
                    r.X,
                    r.Y,
                    r.Width,
                    r.Height
                ));
            }
        }
    }
    println!(
        "OCR {w}x{h}: {} lines, {words} words in {:.0}ms",
        lines.Size().unwrap(),
        start.elapsed().as_secs_f64() * 1000.0
    );
    println!("  {}", sample.join("  "));

    // The same frame through TextReader, which sends greyscale.
    let rgba: Vec<u8> = pixels
        .as_chunks::<4>()
        .0
        .iter()
        .flat_map(|p| [p[2], p[1], p[0], 255])
        .collect();
    let reader = screenilux_win::TextReader::new().unwrap();
    let start = Instant::now();
    let page = reader.read(&rgba, w, h).unwrap();
    println!(
        "TextReader (greyscale): {} words in {:.0}ms",
        page.words().count(),
        start.elapsed().as_secs_f64() * 1000.0
    );
}

fn main() {
    screenilux_win::set_dpi_awareness();
    unsafe {
        CoInitializeEx(None, COINIT_MULTITHREADED).ok().unwrap();
        MFStartup(MF_VERSION, MFSTARTUP_FULL).unwrap();
    }
    let path = std::env::temp_dir().join(format!(
        "screenilux-spike-decode-{}.mp4",
        std::process::id()
    ));
    encode(&path);
    decode(&path);
    decode_speed(
        &path,
        "RGB32 + video processing",
        MFVideoFormat_RGB32,
        Some(MF_SOURCE_READER_ENABLE_VIDEO_PROCESSING),
    );
    decode_speed(
        &path,
        "RGB32 + advanced processing",
        MFVideoFormat_RGB32,
        Some(MF_SOURCE_READER_ENABLE_ADVANCED_VIDEO_PROCESSING),
    );
    decode_speed(&path, "NV12 native", MFVideoFormat_NV12, None);
    decode_speed(
        &path,
        "NV12 + hardware transforms",
        MFVideoFormat_NV12,
        Some(MF_READWRITE_ENABLE_HARDWARE_TRANSFORMS),
    );
    let _ = std::fs::remove_file(&path);
    if std::env::args().any(|a| a == "--ocr") {
        ocr();
    }
}
