//! Does a native Media Foundation encoder work for our frames?
//!
//! If it does, Screenilux needs no bundled ffmpeg at all — the difference
//! between a single self-contained `.exe` and one shipped alongside tens of
//! megabytes of encoder.
//!
//! The question is specifically whether `send_frame_buffer` accepts *our*
//! frames: composited BGRA we produced ourselves, not the untouched capture
//! texture. It also checks the quirk in its docs — "Windows expects BGRA and
//! bottom-to-top layout for this path" — and whether pausing works, since the
//! recorder cuts a segment at every pause.
//!
//!   cargo run -p screenilux-win --example spike_native_encode

use std::time::Instant;

use windows_capture::encoder::{
    AudioSettingsBuilder, ContainerSettingsBuilder, VideoEncoder, VideoSettingsBuilder,
    VideoSettingsSubType,
};

const W: u32 = 1280;
const H: u32 = 720;
const FPS: u32 = 30;
const SECONDS: u32 = 3;
/// Media Foundation timestamps are in 100-nanosecond units.
const HNS_PER_SECOND: i64 = 10_000_000;

/// A frame with a moving band, so the result is visibly not a frozen image.
fn frame(n: u32, flip: bool) -> Vec<u8> {
    let mut buf = vec![0u8; (W * H * 4) as usize];
    let band = (n * 7) % H;
    for y in 0..H {
        // bottom-to-top if the encoder wants it that way
        let row = if flip { H - 1 - y } else { y };
        let base = (row * W * 4) as usize;
        let near = y.abs_diff(band) < 40;
        for x in 0..W {
            let i = base + (x * 4) as usize;
            let shade = (x * 255 / W) as u8;
            // BGRA
            buf[i] = if near { 240 } else { 40 };
            buf[i + 1] = shade;
            buf[i + 2] = if near { 60 } else { 180 };
            buf[i + 3] = 255;
        }
    }
    buf
}

fn encode(path: &str, flip: bool, pause_after: Option<u32>) -> Result<f64, String> {
    let mut encoder = VideoEncoder::new(
        VideoSettingsBuilder::new(W, H)
            // The builder defaults to HEVC; H264 is what plays everywhere.
            .sub_type(VideoSettingsSubType::H264)
            .frame_rate(FPS)
            .bitrate(8_000_000),
        AudioSettingsBuilder::new().disabled(true),
        ContainerSettingsBuilder::new(),
        path,
    )
    .map_err(|e| format!("could not open the encoder: {e}"))?;

    let total = FPS * SECONDS;
    let started = Instant::now();
    for n in 0..total {
        // Simulate a pause by skipping a stretch of timestamps, which is what
        // a single-encoder design would have to do instead of cutting a
        // segment. If the container handles the gap, pausing needs no segments.
        let effective = match pause_after {
            Some(at) if n >= at => n + FPS, // a one-second hole
            _ => n,
        };
        let timestamp = (effective as i64) * HNS_PER_SECOND / FPS as i64;
        encoder
            .send_frame_buffer(&frame(n, flip), timestamp)
            .map_err(|e| format!("send_frame_buffer failed at frame {n}: {e}"))?;
    }
    encoder
        .finish()
        .map_err(|e| format!("finish failed: {e}"))?;
    Ok(started.elapsed().as_secs_f64())
}

fn main() {
    let dir = std::env::temp_dir().join("screenilux-native-spike");
    let _ = std::fs::create_dir_all(&dir);
    println!("writing to {}\n", dir.display());

    let plain = dir.join("native.mp4");
    match encode(&plain.to_string_lossy(), true, None) {
        Ok(secs) => {
            let bytes = std::fs::metadata(&plain).map(|m| m.len()).unwrap_or(0);
            println!(
                "native encode: {} frames of {W}x{H} in {secs:.2}s -> {:.2} MB",
                FPS * SECONDS,
                bytes as f64 / 1_048_576.0
            );
            println!("  realtime factor: {:.1}x", SECONDS as f64 / secs);
        }
        Err(e) => {
            println!("native encode FAILED: {e}");
            println!("  -> the bundled-ffmpeg plan stands");
            return;
        }
    }

    // Orientation: encode the same content without the flip, so the two files
    // can be compared and the documented quirk confirmed rather than trusted.
    let unflipped = dir.join("native_unflipped.mp4");
    match encode(&unflipped.to_string_lossy(), false, None) {
        Ok(_) => println!("\nalso wrote native_unflipped.mp4 for an orientation check"),
        Err(e) => println!("\nunflipped variant failed: {e}"),
    }

    // Pausing without segments: can one encoder swallow a gap in timestamps?
    let paused = dir.join("native_paused.mp4");
    match encode(&paused.to_string_lossy(), true, Some(FPS)) {
        Ok(_) => println!("wrote native_paused.mp4 with a one-second timestamp gap"),
        Err(e) => println!("paused variant failed: {e}"),
    }

    println!("\nverify with ffprobe; the spike deliberately does not self-certify");
}
