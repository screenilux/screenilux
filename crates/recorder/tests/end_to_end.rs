//! End-to-end recording against the real display, encoder and filesystem.
//!
//! These are `#[ignore]`d: each one records for several seconds of wall time
//! and touches real hardware, so they are opt-in rather than part of every
//! `cargo test`. Run them with:
//!
//! ```text
//! cargo test -p screenilux-recorder -- --ignored --test-threads=1
//! ```
//!
//! `--test-threads=1` matters: two recordings at once would contend for the
//! encoder and the capture device and tell you nothing useful.

use std::path::Path;
use std::process::Command;
use std::time::{Duration, Instant};

use screenilux_core::session::Manifest;
use screenilux_media::{detect, locate_ffmpeg};
use screenilux_recorder::{Session, SessionConfig};
use screenilux_win::{display, set_dpi_awareness};

/// Duration of a finished file, straight from ffprobe.
fn duration_of(ffmpeg: &Path, file: &Path) -> f64 {
    let probe = ffmpeg.with_file_name(if cfg!(windows) {
        "ffprobe.exe"
    } else {
        "ffprobe"
    });
    let out = Command::new(probe)
        .args([
            "-v",
            "error",
            "-show_entries",
            "format=duration",
            "-of",
            "csv=p=0",
        ])
        .arg(file)
        .output()
        .expect("ffprobe should run");
    String::from_utf8_lossy(&out.stdout)
        .trim()
        .parse()
        .unwrap_or(0.0)
}

fn frame_count(ffmpeg: &Path, file: &Path) -> u64 {
    let probe = ffmpeg.with_file_name(if cfg!(windows) {
        "ffprobe.exe"
    } else {
        "ffprobe"
    });
    let out = Command::new(probe)
        .args([
            "-v",
            "error",
            "-select_streams",
            "v:0",
            "-count_frames",
            "-show_entries",
            "stream=nb_read_frames",
            "-of",
            "csv=p=0",
        ])
        .arg(file)
        .output()
        .expect("ffprobe should run");
    String::from_utf8_lossy(&out.stdout)
        .trim()
        .parse()
        .unwrap_or(0)
}

struct Fixture {
    dir: screenilux_media::TempDir,
    ffmpeg: std::path::PathBuf,
}

fn fixture() -> Option<Fixture> {
    set_dpi_awareness();
    Some(Fixture {
        dir: screenilux_media::TempDir::new("screenilux-e2e").ok()?,
        ffmpeg: locate_ffmpeg()?,
    })
}

/// Cursor and zoom on, matching the shipping defaults, so the end-to-end runs
/// exercise the compositing path rather than the raw passthrough.
fn effects() -> screenilux_core::EffectOptions {
    screenilux_core::EffectOptions {
        cursor: true,
        zoom: true,
        trail: false,
        clicks: false,
        camera: false,
    }
}

fn config(f: &Fixture, monitors: Vec<screenilux_core::Monitor>) -> SessionConfig {
    SessionConfig {
        monitors,
        fps: 30,
        audio_device: None,
        encoder: detect(&f.ffmpeg),
        ffmpeg: f.ffmpeg.clone(),
        effects: effects(),
        camera: None,
        // No hook here: these runs verify capture and timing, not overlays
        // driven by the pointer.
        pointer_events: None,
    }
}

#[test]
#[ignore = "records the real screen for several seconds"]
fn records_the_primary_screen_to_a_single_file() {
    let Some(f) = fixture() else { return };
    let Ok(all) = display::monitors() else { return };
    let primary: Vec<_> = all.into_iter().filter(|m| m.is_primary).collect();

    let session = Session::start(config(&f, primary)).expect("session should start");
    std::thread::sleep(Duration::from_secs(4));
    let recorded = session.duration().as_secs_f64();
    let path = session.finish(f.dir.path()).expect("should save");

    assert!(path.is_file(), "one screen should give a single mp4");
    assert_eq!(path.extension().unwrap(), "mp4");

    let on_disk = duration_of(&f.ffmpeg, &path);
    assert!(
        (on_disk - recorded).abs() < 0.4,
        "file is {on_disk:.2}s but {recorded:.2}s was recorded"
    );
    assert!(
        (recorded - 4.0).abs() < 0.8,
        "expected roughly 4s of recording, got {recorded:.2}s"
    );
}

#[test]
#[ignore = "records the real screen for several seconds"]
fn a_pause_is_excluded_from_the_recording() {
    let Some(f) = fixture() else { return };
    let Ok(all) = display::monitors() else { return };
    let primary: Vec<_> = all.into_iter().filter(|m| m.is_primary).collect();

    let session = Session::start(config(&f, primary)).expect("session should start");
    let wall = Instant::now();
    std::thread::sleep(Duration::from_millis(1500));
    session.pause();
    std::thread::sleep(Duration::from_millis(1500)); // excluded
    session.resume();
    std::thread::sleep(Duration::from_millis(1500));

    let recorded = session.duration().as_secs_f64();
    let elapsed = wall.elapsed().as_secs_f64();
    let path = session.finish(f.dir.path()).expect("should save");

    assert!(
        recorded < elapsed - 1.0,
        "recorded {recorded:.2}s of {elapsed:.2}s wall; the pause should be excluded"
    );
    let on_disk = duration_of(&f.ffmpeg, &path);
    assert!(
        (on_disk - recorded).abs() < 0.5,
        "joined file is {on_disk:.2}s but {recorded:.2}s was recorded"
    );
}

#[test]
#[ignore = "records every attached screen for several seconds"]
fn records_every_screen_in_sync_with_a_manifest() {
    let Some(f) = fixture() else { return };
    let Ok(all) = display::monitors() else { return };
    if all.len() < 2 {
        eprintln!("only one display attached; skipping the multi-screen check");
        return;
    }
    let count = all.len();

    let session = Session::start(config(&f, all)).expect("session should start");
    std::thread::sleep(Duration::from_secs(4));
    let recorded = session.duration().as_secs_f64();
    let folder = session.finish(f.dir.path()).expect("should save");

    assert!(
        folder.is_dir(),
        "several screens should give a session folder"
    );
    let manifest_path = folder.join("recording.json");
    let text = std::fs::read_to_string(&manifest_path).expect("manifest should exist");
    let manifest = Manifest::from_json(&text).expect("manifest should parse");

    assert_eq!(manifest.screens.len(), count);
    assert_eq!(manifest.fps, 30);
    // `recorded` is a snapshot taken just before stopping, so the manifest may
    // be a hair longer - but only by the final loop iteration, never by the
    // encoder's shutdown time.
    // `recorded` is a snapshot taken just before stopping. The manifest may be
    // slightly longer because a capture thread can be waiting on a frame when
    // the stop arrives - bounded by the frame timeout - but it must never
    // include the encoder's shutdown time.
    let overshoot = manifest.duration - recorded;
    assert!(
        (0.0..0.35).contains(&overshoot),
        "manifest says {:.3}s against {recorded:.3}s recorded (overshoot {overshoot:.3}s)",
        manifest.duration
    );

    // the primary leads the display order
    let ordered = manifest.by_order();
    assert_eq!(ordered[0].order, 1);
    assert!(ordered[0].primary, "the primary should be shown first");

    // exactly one file carries audio (none here, since no mic was requested)
    assert!(manifest.screens.iter().all(|s| !s.audio));

    // every file exists, and they agree on length to within a frame or two
    let mut frames = Vec::new();
    for s in &manifest.screens {
        let p = folder.join(&s.file);
        assert!(p.is_file(), "{} is missing", s.file);
        let d = duration_of(&f.ffmpeg, &p);
        assert!(
            (d - recorded).abs() < 0.5,
            "{} is {d:.2}s but {recorded:.2}s was recorded",
            s.file
        );
        frames.push(frame_count(&f.ffmpeg, &p));
    }
    let spread = frames.iter().max().unwrap() - frames.iter().min().unwrap();
    assert!(
        spread <= 2,
        "screens differ by {spread} frames ({frames:?}); the shared clock should hold them within one or two"
    );
}

#[test]
#[ignore = "records the real screen for several seconds"]
fn two_recordings_in_the_same_second_do_not_overwrite_each_other() {
    let Some(f) = fixture() else { return };
    let Ok(all) = display::monitors() else { return };
    let primary: Vec<_> = all.into_iter().filter(|m| m.is_primary).collect();

    let mut saved = Vec::new();
    for _ in 0..2 {
        let session = Session::start(config(&f, primary.clone())).expect("start");
        std::thread::sleep(Duration::from_millis(700));
        saved.push(session.finish(f.dir.path()).expect("save"));
    }

    assert_ne!(
        saved[0], saved[1],
        "the second recording must not land on the first's path"
    );
    assert!(saved.iter().all(|p| p.is_file()));
}
