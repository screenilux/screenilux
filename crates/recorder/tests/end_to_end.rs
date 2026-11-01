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
use screenilux_platform::{display, set_dpi_awareness};
use screenilux_recorder::paths::TempDir;
use screenilux_recorder::{Session, SessionConfig};

/// Duration of a finished file, straight from ffprobe.
fn duration_of(ffmpeg: &Path, file: &Path) -> f64 {
    let out = Command::new(ffmpeg)
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
    let out = Command::new(ffmpeg)
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
    dir: TempDir,
    /// ffprobe is a *development* tool here: the application itself no longer
    /// depends on ffmpeg in any form.
    ffmpeg: std::path::PathBuf,
}

fn fixture() -> Option<Fixture> {
    set_dpi_awareness();
    Some(Fixture {
        dir: TempDir::new("screenilux-e2e").ok()?,
        ffmpeg: which_ffprobe()?,
    })
}

fn which_ffprobe() -> Option<std::path::PathBuf> {
    let name = format!("ffprobe{}", std::env::consts::EXE_SUFFIX);
    std::env::split_paths(&std::env::var_os("PATH")?)
        .map(|d| d.join(&name))
        .find(|p| p.is_file())
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

fn config(_f: &Fixture, monitors: Vec<screenilux_core::Monitor>) -> SessionConfig {
    SessionConfig {
        monitors,
        fps: 30,
        microphone: false,
        system_sound: false,
        click_sound: false,
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
    let package = session.finish(f.dir.path()).expect("should save");

    assert!(
        package.is_file() && screenilux_core::package::is_package(&package),
        "several screens should give one .screenilux file, got {}",
        package.display()
    );
    // Unpacked, it is the session folder the player and older tools read.
    let folder = f.dir.path().join("unpacked");
    let manifest = screenilux_core::package::unpack(&package, &folder).expect("should unpack");
    assert_eq!(
        Manifest::from_json(&std::fs::read_to_string(folder.join("recording.json")).unwrap())
            .unwrap(),
        manifest
    );

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

/// Duration of the first audio stream, or `None` if the file has none.
fn audio_duration_of(ffmpeg: &Path, file: &Path) -> Option<f64> {
    let out = Command::new(ffmpeg)
        .args([
            "-v",
            "error",
            "-select_streams",
            "a:0",
            "-show_entries",
            "stream=duration",
            "-of",
            "csv=p=0",
        ])
        .arg(file)
        .output()
        .expect("ffprobe should run");
    String::from_utf8_lossy(&out.stdout).trim().parse().ok()
}

fn video_duration_of(ffmpeg: &Path, file: &Path) -> f64 {
    let out = Command::new(ffmpeg)
        .args([
            "-v",
            "error",
            "-select_streams",
            "v:0",
            "-show_entries",
            "stream=duration",
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

/// The encoder places audio by counting samples, not by timestamp. So any
/// microphone audio captured while paused that reaches the encoder both plays
/// in the output and pushes every later sample out of sync with the picture.
#[test]
#[ignore = "records the real screen and microphone for several seconds"]
fn a_pause_does_not_leak_microphone_audio() {
    let Some(f) = fixture() else { return };
    if !screenilux_platform::has_microphone() {
        eprintln!("no microphone; skipping the audio check");
        return;
    }
    let Ok(all) = display::monitors() else { return };
    let primary: Vec<_> = all.into_iter().filter(|m| m.is_primary).collect();

    let mut cfg = config(&f, primary);
    cfg.microphone = true;
    let session = Session::start(cfg).expect("session should start");
    std::thread::sleep(Duration::from_millis(1500));
    session.pause();
    // Long enough for the capture queue to fill completely.
    std::thread::sleep(Duration::from_millis(3000));
    session.resume();
    std::thread::sleep(Duration::from_millis(1500));
    let path = session.finish(f.dir.path()).expect("should save");

    let video = video_duration_of(&f.ffmpeg, &path);
    let Some(audio) = audio_duration_of(&f.ffmpeg, &path) else {
        // A microphone blocked by privacy settings yields no stream at all.
        eprintln!("no audio stream was written; skipping");
        return;
    };
    assert!(
        (audio - video).abs() < 0.15,
        "audio is {audio:.3}s against {video:.3}s of video; paused audio leaked in"
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

/// Loudness of the decoded sound in each tenth of a second.
fn loudness_by_tenth(file: &Path) -> Vec<f64> {
    let mut decoder =
        screenilux_platform::AudioDecoder::open(file).expect("the file should have sound");
    let mut samples: Vec<i16> = Vec::new();
    while let Some((_, pcm)) = decoder.next_chunk().expect("sound should decode") {
        samples.extend(
            pcm.as_chunks::<2>()
                .0
                .iter()
                .map(|b| i16::from_le_bytes(*b)),
        );
    }
    // 48kHz stereo: 9,600 samples per tenth of a second.
    samples
        .chunks(9_600)
        .map(|c| (c.iter().map(|&s| (s as f64).powi(2)).sum::<f64>() / c.len() as f64).sqrt())
        .collect()
}

#[test]
#[ignore = "records the real screen and plays a short tone through the speakers"]
fn system_sound_lands_in_the_recording_at_the_right_time() {
    let Some(f) = fixture() else { return };
    if !screenilux_platform::has_system_sound() {
        return;
    }
    let Ok(all) = display::monitors() else { return };
    let primary: Vec<_> = all.into_iter().filter(|m| m.is_primary).collect();
    let mut cfg = config(&f, primary);
    cfg.system_sound = true;

    let session = Session::start(cfg).expect("session should start");
    let started = Instant::now();
    // A quiet 880Hz tone for one second, from 1.5s in. The speaker stream is
    // not Send, so it lives on its own thread.
    let tone = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(1500));
        let mut speaker = screenilux_platform::Speaker::open().expect("speakers");
        let rate = screenilux_platform::decoder::PLAYBACK_RATE as f64;
        let pcm: Vec<u8> = (0..rate as usize)
            .flat_map(|i| {
                let s = ((i as f64 * 880.0 * std::f64::consts::TAU / rate).sin() * 4_000.0) as i16;
                [s.to_le_bytes(), s.to_le_bytes()].concat()
            })
            .collect();
        let mut sent = 0;
        speaker.start();
        while sent < pcm.len() {
            sent += speaker.write(&pcm[sent..]);
            std::thread::sleep(Duration::from_millis(10));
        }
        std::thread::sleep(Duration::from_millis(300));
    });
    tone.join().unwrap();
    std::thread::sleep(Duration::from_secs(4).saturating_sub(started.elapsed()));
    let path = session.finish(f.dir.path()).expect("should save");

    let loud = loudness_by_tenth(&path);
    let quiet_before = loud[..12].iter().cloned().fold(0.0, f64::max);
    let during = loud[17..24].iter().sum::<f64>() / 7.0;
    assert!(
        during > 500.0,
        "the tone should be in the recording, got {during:.0} RMS ({loud:.0?})"
    );
    assert!(
        quiet_before < during / 4.0,
        "the tone should start around 1.5s, not before ({loud:.0?})"
    );
    let audio = audio_duration_of(&f.ffmpeg, &path).expect("an audio stream");
    let video = video_duration_of(&f.ffmpeg, &path);
    assert!(
        (audio - video).abs() < 0.15,
        "sound {audio:.2}s and picture {video:.2}s should match"
    );
}
