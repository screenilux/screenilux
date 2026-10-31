//! Record every screen for a few seconds from the command line.
//!
//!   cargo run --release -p screenilux-recorder --example record -- <seconds> <out-dir>
//!
//! For producing real recordings to check the player against, without
//! driving the window. `--twice` records the primary display as two screens,
//! which exercises multi-screen saving on a machine with one monitor.

use std::path::PathBuf;
use std::time::Duration;

use screenilux_recorder::{Session, SessionConfig};

fn main() {
    let twice = std::env::args().any(|a| a == "--twice");
    let mut args = std::env::args().skip(1).filter(|a| a != "--twice");
    let secs: u64 = args.next().and_then(|s| s.parse().ok()).unwrap_or(5);
    let out = args.next().map(PathBuf::from).unwrap_or_else(|| ".".into());

    screenilux_win::set_dpi_awareness();
    let mut monitors = screenilux_win::monitors().expect("displays");
    if twice {
        let mut copy = monitors[0].clone();
        copy.label = format!("{} (again)", copy.label);
        copy.is_primary = false;
        monitors.truncate(1);
        monitors.push(copy);
    }
    let session = Session::start(SessionConfig {
        monitors,
        fps: 30,
        microphone: false,
        effects: screenilux_core::EffectOptions::default(),
        camera: None,
        pointer_events: None,
    })
    .expect("recording should start");
    std::thread::sleep(Duration::from_secs(secs));
    let saved = session.finish(&out).expect("recording should save");
    println!("{}", saved.display());
}
