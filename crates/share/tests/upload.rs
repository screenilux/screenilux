//! Uploads a real package and reads it back the way the web player does.
//!
//! These need somewhere to upload to, so each skips unless told where:
//!
//! - SCREENILUX_TEST_SERVER: a Screenilux storage, e.g. the local one
//!   `npm run dev` starts in web/.
//! - SCREENILUX_TEST_S3: the address of an S3-compatible server that takes
//!   any keys and serves files to anyone, with a bucket called `recordings`,
//!   e.g. `docker run -p 9090:9090 -e initialBuckets=recordings adobe/s3mock`.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use screenilux_core::session::{MANIFEST_NAME, Manifest, Role, Screen};
use screenilux_share::{Bucket, Contents, Destination, Progress, State, Upload};

/// The second screen is big enough to go up in parts.
const SIZES: [usize; 2] = [1000, 70 * 1024 * 1024 + 123];

fn byte(i: usize) -> u8 {
    (i % 251) as u8
}

/// A two-screen package in a folder of its own.
fn package(tag: &str) -> (PathBuf, PathBuf) {
    let dir = std::env::temp_dir().join(format!("screenilux-{tag}-{}", std::process::id()));
    let folder = dir.join("session");
    std::fs::create_dir_all(&folder).unwrap();
    let mut manifest = Manifest::new("2026-11-01T10:00:00Z".into(), 3.0, 30, "test".into(), false);
    for (i, size) in SIZES.iter().enumerate() {
        let n = i as u32 + 1;
        manifest.screens.push(Screen {
            file: format!("screen{n}.mp4"),
            label: format!("Screen {n}"),
            width: 1920,
            height: 1080,
            left: 1920 * i as i32,
            top: 0,
            primary: i == 0,
            audio: i == 0,
            role: if i == 0 {
                Role::Primary
            } else {
                Role::Secondary
            },
            order: n,
        });
        let body: Vec<u8> = (0..*size).map(byte).collect();
        std::fs::write(folder.join(format!("screen{n}.mp4")), body).unwrap();
    }
    std::fs::write(folder.join(MANIFEST_NAME), manifest.to_json().unwrap()).unwrap();
    let package = dir.join("r.screenilux");
    screenilux_core::package::pack(&folder, &package).unwrap();
    (dir, package)
}

fn finish(upload: &Upload) -> Progress {
    let started = Instant::now();
    loop {
        let p = upload.progress();
        if matches!(p.state, State::Ready | State::Failed(_))
            || started.elapsed() > Duration::from_secs(300)
        {
            return p;
        }
        std::thread::sleep(Duration::from_millis(200));
    }
}

/// What the player reads: share.json says ready, and the big video's last
/// bytes come back from a range request.
fn check_files(files: &str, progress: &Progress) {
    let agent = ureq::Agent::new_with_defaults();
    let share: serde_json::Value = agent
        .get(format!("{files}share.json"))
        .call()
        .unwrap()
        .body_mut()
        .read_json()
        .unwrap();
    assert_eq!(share["status"], "ready");
    assert_eq!(share["total"], progress.total);
    let manifest: Manifest = agent
        .get(format!("{files}recording.json"))
        .call()
        .unwrap()
        .body_mut()
        .read_json()
        .unwrap();
    assert_eq!(manifest.screens.len(), 2);
    let big = agent
        .get(format!("{files}screen2.mp4"))
        .header("Range", "bytes=-3")
        .call()
        .unwrap();
    assert_eq!(big.status(), 206);
    let last = SIZES[1];
    assert_eq!(
        big.into_body().read_to_vec().unwrap(),
        [byte(last - 3), byte(last - 2), byte(last - 1)]
    );
}

#[test]
fn a_package_uploads_to_the_screenilux_storage() {
    let Ok(server) = std::env::var("SCREENILUX_TEST_SERVER") else {
        eprintln!("skipped: no SCREENILUX_TEST_SERVER");
        return;
    };
    let (dir, package) = package("hosted");
    let contents = Contents::of_package(&package, "Upload test".into()).unwrap();
    let upload = Upload::start(
        Destination::Screenilux {
            server: server.clone(),
        },
        contents,
    );
    let progress = finish(&upload);
    assert_eq!(progress.state, State::Ready);

    let link = progress.link.clone().unwrap();
    let id = link.rsplit('/').next().unwrap();
    check_files(
        &format!("{}/r/{id}/", server.trim_end_matches('/')),
        &progress,
    );
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn a_package_uploads_to_your_own_bucket() {
    let Ok(server) = std::env::var("SCREENILUX_TEST_S3") else {
        eprintln!("skipped: no SCREENILUX_TEST_S3");
        return;
    };
    let server = server.trim_end_matches('/').to_string();
    let (dir, package) = package("bucket");
    let contents = Contents::of_package(&package, "Upload test".into()).unwrap();
    let upload = Upload::start(
        Destination::Bucket(Bucket {
            endpoint: server.clone(),
            region: "us-east-1".into(),
            bucket: "recordings".into(),
            access_key: "test".into(),
            secret_key: "test".into(),
            public_url: format!("{server}/recordings"),
            folder: "shared".into(),
        }),
        contents,
    );
    let progress = finish(&upload);
    assert_eq!(progress.state, State::Ready, "{progress:?}");

    // The link carries the files' address after the #.
    let link = progress.link.clone().unwrap();
    let encoded = link.split_once("/b#").unwrap().1;
    let files: String = url::form_urlencoded::parse(format!("x={encoded}").as_bytes())
        .next()
        .unwrap()
        .1
        .into_owned();
    assert!(files.starts_with(&format!("{server}/recordings/shared/")));
    check_files(&files, &progress);
    eprintln!("player: /b#{encoded}");
    std::fs::remove_dir_all(dir).unwrap();
}
