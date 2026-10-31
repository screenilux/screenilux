//! Can the native encoder sustain 1080p30, and how does it compare to ffmpeg?
//! Frames are built once up front so the measurement is the encoder, not us.
use std::time::Instant;
use windows_capture::encoder::{
    AudioSettingsBuilder, ContainerSettingsBuilder, VideoEncoder, VideoSettingsBuilder,
    VideoSettingsSubType,
};

const FPS: u32 = 30;
const HNS: i64 = 10_000_000;

fn bank(w: u32, h: u32, count: usize) -> Vec<Vec<u8>> {
    (0..count)
        .map(|n| {
            let mut buf = vec![0u8; (w * h * 4) as usize];
            for y in 0..h {
                let base = ((h - 1 - y) * w * 4) as usize;
                for x in 0..w {
                    let i = base + (x * 4) as usize;
                    buf[i] = ((x + n as u32 * 9) % 255) as u8;
                    buf[i + 1] = ((y + n as u32 * 3) % 255) as u8;
                    buf[i + 2] = 128;
                    buf[i + 3] = 255;
                }
            }
            buf
        })
        .collect()
}

fn run(w: u32, h: u32, seconds: u32) {
    let frames = bank(w, h, 15);
    let total = FPS * seconds;
    let path = std::env::temp_dir().join(format!("screenilux-speed-{w}x{h}.mp4"));
    let mut enc = match VideoEncoder::new(
        VideoSettingsBuilder::new(w, h)
            .sub_type(VideoSettingsSubType::H264)
            .frame_rate(FPS)
            .bitrate(10_000_000),
        AudioSettingsBuilder::new().disabled(true),
        ContainerSettingsBuilder::new(),
        &path,
    ) {
        Ok(e) => e,
        Err(e) => return println!("{w}x{h}: could not open encoder: {e}"),
    };

    let t = Instant::now();
    for n in 0..total {
        let ts = (n as i64) * HNS / FPS as i64;
        if let Err(e) = enc.send_frame_buffer(&frames[n as usize % frames.len()], ts) {
            return println!("{w}x{h}: failed at {n}: {e}");
        }
    }
    let sent = t.elapsed().as_secs_f64();
    let _ = enc.finish();
    let wall = t.elapsed().as_secs_f64();
    println!(
        "{w}x{h}: {total} frames  send={sent:.2}s ({:.0} fps)  +finish={wall:.2}s  realtime x{:.1}",
        total as f64 / sent,
        seconds as f64 / wall
    );
}

fn main() {
    run(1280, 720, 5);
    run(1920, 1080, 5);
    run(1920, 1080, 10);
}
