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
use screenilux_share::{
    Bucket, Contents, Destination, Progress, ShareKey, State, Upload, open, sealed_len,
};

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

/// The value of `name` in the part of `link` after the #.
fn fragment(link: &str, name: &str) -> String {
    let (_, fragment) = link.split_once('#').unwrap();
    url::form_urlencoded::parse(fragment.as_bytes())
        .find(|(k, _)| k == name)
        .unwrap()
        .1
        .into_owned()
}

fn get(url: &str) -> Vec<u8> {
    ureq::get(url)
        .call()
        .unwrap()
        .into_body()
        .with_config()
        .limit(200 * 1024 * 1024)
        .read_to_vec()
        .unwrap()
}

/// What the player reads: every file is there, unreadable without the key,
/// and opens with the key from the link.
fn check_files(files: &str, link: &str, progress: &Progress) {
    let key = ShareKey::from_link(&fragment(link, "k")).unwrap();

    let manifest_sealed = get(&format!("{files}{MANIFEST_NAME}"));
    assert!(!String::from_utf8_lossy(&manifest_sealed).contains("screen1.mp4"));
    let manifest: serde_json::Value =
        serde_json::from_slice(&open(&key, &manifest_sealed).unwrap()).unwrap();
    assert_eq!(manifest["title"], "Upload test");
    assert_eq!(manifest["screens"].as_array().unwrap().len(), 2);

    let sealed = get(&format!("{files}screen2.mp4"));
    assert_eq!(sealed.len() as u64, sealed_len(SIZES[1] as u64));
    let plain = open(&key, &sealed).unwrap();
    assert_eq!(plain.len(), SIZES[1]);
    assert!(plain.iter().enumerate().all(|(i, &b)| b == byte(i)));

    let expected = sealed_len(SIZES[0] as u64) + sealed.len() as u64 + manifest_sealed.len() as u64;
    assert_eq!(progress.total, expected);
}

#[test]
fn a_package_uploads_to_the_screenilux_storage() {
    let Ok(server) = std::env::var("SCREENILUX_TEST_SERVER") else {
        eprintln!("skipped: no SCREENILUX_TEST_SERVER");
        return;
    };
    let server = server.trim_end_matches('/').to_string();
    let (dir, package) = package("hosted");
    let contents = Contents::of_package(&package, "Upload test".into()).unwrap();
    let upload = Upload::start(
        Destination::Screenilux {
            server: server.clone(),
            device: screenilux_share::new_device_id(),
        },
        contents,
    );
    let progress = finish(&upload);
    assert_eq!(progress.state, State::Ready, "{progress:?}");

    // The server says it is ready, and knows nothing else about it.
    let link = progress.link.clone().unwrap();
    let page = link.split_once('#').unwrap().0;
    let id = page.rsplit('/').next().unwrap();
    let status: serde_json::Value = ureq::get(format!("{server}/api/recordings/{id}"))
        .call()
        .unwrap()
        .body_mut()
        .read_json()
        .unwrap();
    assert_eq!(status["status"], "ready");
    assert_eq!(status["received"], progress.total);
    check_files(&format!("{server}/r/{id}/"), &link, &progress);
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

    let link = progress.link.clone().unwrap();
    let files = fragment(&link, "u");
    assert!(files.starts_with(&format!("{server}/recordings/shared/")));
    check_files(&files, &link, &progress);
    eprintln!("player: /b#{}", link.split_once('#').unwrap().1);
    std::fs::remove_dir_all(dir).unwrap();
}
