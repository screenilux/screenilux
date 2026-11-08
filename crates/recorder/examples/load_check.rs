//! What a recording costs the rest of the computer.
//!
//! Records the primary screen with the shipping effects for a few seconds and
//! reports how often the capture handed over a frame. Run it with something
//! moving on screen, and read the processor and graphics time it used from
//! outside (Task Manager, or the `GPU Engine` performance counters for its
//! process id, which it prints).
//!
//!   cargo run -p screenilux-recorder --release --example load_check -- [fps] [seconds]

use std::time::Duration;

use screenilux_platform::{display, set_dpi_awareness};
use screenilux_recorder::paths::TempDir;
use screenilux_recorder::{Session, SessionConfig};

fn main() {
    let mut args = std::env::args().skip(1);
    let fps = args.next().and_then(|a| a.parse().ok()).unwrap_or(60);
    let seconds = args.next().and_then(|a| a.parse().ok()).unwrap_or(10);

    set_dpi_awareness();
    let primary: Vec<_> = display::monitors()
        .expect("monitors")
        .into_iter()
        .filter(|m| m.is_primary)
        .collect();
    let out = TempDir::new("screenilux-load-check").expect("temp dir");

    println!("pid {}", std::process::id());
    let session = Session::start(SessionConfig {
        monitors: primary,
        fps,
        microphone: false,
        system_sound: false,
        click_sound: false,
        effects: screenilux_core::EffectOptions {
            cursor: true,
            zoom: true,
            trail: false,
            clicks: false,
            camera: false,
        },
        camera: None,
        pointer_events: None,
    })
    .expect("session should start");
    std::thread::sleep(Duration::from_secs(seconds));
    let recorded = session.duration();
    session.finish(out.path()).expect("should save");
    println!("recorded {:.1}s at {fps} fps", recorded.as_secs_f64());
}
