//! Validates nokhwa before the camera module is built on it.
//!
//! nokhwa is the weakest dependency in the port — one maintainer, pre-1.0, a
//! yanked release in its history — and the Python build used OpenCV's
//! `VideoCapture`, which is boring and works. So this checks the specific
//! things the camera bubble needs before committing: that devices enumerate
//! with usable names, that a stream opens and delivers frames, that the pixel
//! format converts to something we can composite, and that it shuts down
//! without wedging the device.
//!
//! Run with: cargo run -p screenilux-win --example spike_camera

use std::time::{Duration, Instant};

use nokhwa::pixel_format::RgbFormat;
use nokhwa::utils::{ApiBackend, RequestedFormat, RequestedFormatType, Resolution};
use nokhwa::{Camera, query};

fn main() {
    println!("querying cameras via MSMF...\n");

    let devices = match query(ApiBackend::MediaFoundation) {
        Ok(d) => d,
        Err(e) => {
            println!("!! query failed: {e}");
            return;
        }
    };

    if devices.is_empty() {
        println!("no cameras found; nothing to validate");
        return;
    }

    for info in &devices {
        println!(
            "[{}] {}  ({})",
            info.index(),
            info.human_name(),
            info.description()
        );
    }
    println!();

    for info in &devices {
        println!("--- opening {} ---", info.human_name());
        let index = info.index().clone();

        let requested = RequestedFormat::new::<RgbFormat>(RequestedFormatType::Closest(
            nokhwa::utils::CameraFormat::new(
                Resolution::new(640, 480),
                nokhwa::utils::FrameFormat::MJPEG,
                30,
            ),
        ));

        let opened = Instant::now();
        let mut cam = match Camera::new(index, requested) {
            Ok(c) => c,
            Err(e) => {
                println!("  !! could not open: {e}\n");
                continue;
            }
        };
        println!("  opened in {:?}", opened.elapsed());
        println!(
            "  negotiated: {}x{} @{}fps, format {:?}",
            cam.resolution().width(),
            cam.resolution().height(),
            cam.frame_rate(),
            cam.frame_format()
        );

        if let Err(e) = cam.open_stream() {
            println!("  !! open_stream failed: {e}\n");
            continue;
        }

        let mut decoded = 0usize;
        let mut first_latency = None;
        let started = Instant::now();
        let deadline = started + Duration::from_secs(4);
        let mut last_dims = (0u32, 0u32);

        while decoded < 20 && Instant::now() < deadline {
            match cam.frame() {
                Ok(frame) => match frame.decode_image::<RgbFormat>() {
                    Ok(img) => {
                        if first_latency.is_none() {
                            first_latency = Some(started.elapsed());
                        }
                        last_dims = (img.width(), img.height());
                        decoded += 1;
                    }
                    Err(e) => {
                        println!("  !! decode failed: {e}");
                        break;
                    }
                },
                Err(e) => {
                    println!("  !! frame failed: {e}");
                    break;
                }
            }
        }

        let elapsed = started.elapsed().as_secs_f64();
        println!(
            "  decoded {decoded} frames in {elapsed:.2}s ({:.1}/s), first after {:?}",
            decoded as f64 / elapsed.max(0.001),
            first_latency
        );
        println!("  decoded size: {}x{}", last_dims.0, last_dims.1);
        println!(
            "  RGB bytes per frame: {} (we need BGRA: {} after conversion)",
            last_dims.0 * last_dims.1 * 3,
            last_dims.0 * last_dims.1 * 4
        );

        let stopping = Instant::now();
        match cam.stop_stream() {
            Ok(()) => println!("  stopped cleanly in {:?}", stopping.elapsed()),
            Err(e) => println!("  !! stop_stream failed: {e}"),
        }
        drop(cam);

        // Reopening is the real test: a backend that wedges the device would
        // fail here, and the app reopens whenever the size or device changes.
        let reopen = Instant::now();
        let again = Camera::new(
            info.index().clone(),
            RequestedFormat::new::<RgbFormat>(RequestedFormatType::AbsoluteHighestFrameRate),
        );
        match again {
            Ok(mut c) => {
                let ok = c.open_stream().is_ok() && c.frame().is_ok();
                println!(
                    "  reopened in {:?}: {}",
                    reopen.elapsed(),
                    if ok {
                        "frames flowing"
                    } else {
                        "opened but no frame"
                    }
                );
                let _ = c.stop_stream();
            }
            Err(e) => println!("  !! reopen failed: {e}"),
        }
        println!();
    }

    println!("verdict: if every camera opened, delivered frames and reopened,");
    println!("nokhwa is good enough; otherwise fall back to hand-rolled MSMF.");
}
