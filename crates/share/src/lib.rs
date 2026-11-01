//! Share links: upload a finished recording and get a link that plays it in a
//! browser.
//!
//! A shared recording is a folder of plain files: `share.json`, then the
//! manifest, each screen's video and the text index if there is one. That
//! folder goes either to the Screenilux storage, which hands out an upload
//! token for each recording, or to the user's own S3-compatible bucket. Either way the link exists
//! before anything else is uploaded: `share.json` is written first, saying
//! the upload is still going, so the link can be sent at once and the web
//! player (`web/`) waits for it to finish.

mod contents;
mod store;

use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant, SystemTime};

use serde::{Deserialize, Serialize};

pub use contents::{Contents, Item};
use store::{Hosted, Own, Store};

/// Where the web player lives. Links point here.
pub const PLAYER: &str = "https://screenilux.hassen-ben-mbarek.workers.dev";

/// How long the Screenilux storage keeps a recording. The bucket's lifecycle
/// rule does the deleting; this is only what the player tells viewers.
pub const KEPT_FOR: Duration = Duration::from_secs(7 * 24 * 60 * 60);

/// Files bigger than this go up in parts of this size. The Screenilux storage
/// takes at most 100MB in one request; buckets need parts of at least 5MB.
const PART: usize = 32 * 1024 * 1024;

/// How often `share.json` is rewritten with the progress, at most.
const REPORT_EVERY: Duration = Duration::from_secs(2);

#[derive(Debug, thiserror::Error)]
pub enum ShareError {
    #[error("could not reach the storage: {0}")]
    Network(String),
    #[error("the storage refused the upload ({0})")]
    Refused(u16),
    #[error("the storage did not accept the upload")]
    NotAllowed,
    #[error("it is bigger than Screenilux can share (4GB); share it to your own bucket instead")]
    TooBig,
    #[error("too many recordings were shared from this network just now; try again in a minute")]
    TooMany,
    #[error("the bucket settings are not usable: {0}")]
    Bucket(String),
    #[error("could not read the recording: {0}")]
    Read(String),
}

impl From<ureq::Error> for ShareError {
    fn from(e: ureq::Error) -> Self {
        match e {
            ureq::Error::StatusCode(401 | 403) => ShareError::NotAllowed,
            ureq::Error::StatusCode(413) => ShareError::TooBig,
            ureq::Error::StatusCode(429) => ShareError::TooMany,
            ureq::Error::StatusCode(code) => ShareError::Refused(code),
            other => ShareError::Network(other.to_string()),
        }
    }
}

/// The user's own S3-compatible bucket: Cloudflare R2, Amazon S3, Backblaze
/// B2, Wasabi and the like.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Bucket {
    /// The S3 API address, e.g. `https://<account>.r2.cloudflarestorage.com`.
    pub endpoint: String,
    /// `auto` for R2; the bucket's region elsewhere.
    pub region: String,
    pub bucket: String,
    pub access_key: String,
    pub secret_key: String,
    /// Where the bucket's files can be read by anyone, e.g. an r2.dev
    /// address or a custom domain. The player reads from here.
    pub public_url: String,
    /// A folder in the bucket to keep shared recordings in.
    pub folder: String,
}

impl Bucket {
    pub fn is_complete(&self) -> bool {
        [
            &self.endpoint,
            &self.bucket,
            &self.access_key,
            &self.secret_key,
            &self.public_url,
        ]
        .iter()
        .all(|s| !s.trim().is_empty())
    }
}

/// Where a recording is shared to.
#[derive(Debug, Clone)]
pub enum Destination {
    /// The Screenilux storage, behind `server`.
    Screenilux {
        server: String,
    },
    Bucket(Bucket),
}

/// How an upload is going.
#[derive(Debug, Clone, PartialEq)]
pub enum State {
    Starting,
    Uploading,
    Ready,
    Failed(String),
}

#[derive(Debug, Clone)]
pub struct Progress {
    /// The link to send, known once the upload has started.
    pub link: Option<String>,
    pub uploaded: u64,
    pub total: u64,
    pub state: State,
}

/// An upload running on its own thread.
pub struct Upload {
    progress: Arc<Mutex<Progress>>,
    _thread: JoinHandle<()>,
}

impl Upload {
    pub fn start(destination: Destination, contents: Contents) -> Self {
        let progress = Arc::new(Mutex::new(Progress {
            link: None,
            uploaded: 0,
            total: contents.total(),
            state: State::Starting,
        }));
        let shared = Arc::clone(&progress);
        let thread = std::thread::Builder::new()
            .name("share".into())
            .spawn(move || {
                if let Err(e) = run(&destination, &contents, &shared) {
                    tracing::warn!(error = %e, "sharing failed");
                    lock(&shared).state = State::Failed(e.to_string());
                }
            })
            .expect("spawning the upload thread");
        Self {
            progress,
            _thread: thread,
        }
    }

    pub fn progress(&self) -> Progress {
        lock(&self.progress).clone()
    }
}

fn lock(p: &Mutex<Progress>) -> std::sync::MutexGuard<'_, Progress> {
    p.lock().unwrap_or_else(|e| e.into_inner())
}

/// What `share.json` says. The web player reads it (web/src/share.ts).
#[derive(Serialize)]
struct ShareDoc<'a> {
    format: &'static str,
    version: u32,
    status: &'static str,
    title: &'a str,
    created: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    expires: Option<String>,
    uploaded: u64,
    total: u64,
    text: bool,
}

fn run(
    destination: &Destination,
    contents: &Contents,
    progress: &Mutex<Progress>,
) -> Result<(), ShareError> {
    let agent = ureq::Agent::new_with_defaults();
    let (store, link, expires): (Box<dyn Store>, String, Option<String>) = match destination {
        Destination::Screenilux { server } => {
            let (hosted, link) = Hosted::create(agent, server, contents.total())?;
            let expires =
                humantime::format_rfc3339_seconds(SystemTime::now() + KEPT_FOR).to_string();
            (Box::new(hosted), link, Some(expires))
        }
        Destination::Bucket(bucket) => {
            let (own, link) = Own::create(agent, bucket, &new_id()?)?;
            (Box::new(own), link, None)
        }
    };

    let total = contents.total();
    let report = |status: &'static str, uploaded: u64| -> Result<(), ShareError> {
        let doc = ShareDoc {
            format: "screenilux-share",
            version: 1,
            status,
            title: &contents.title,
            created: &contents.created,
            expires: expires.clone(),
            uploaded,
            total,
            text: contents
                .items
                .iter()
                .any(|i| i.name == screenilux_core::text::TEXT_INDEX_NAME),
        };
        let json = serde_json::to_vec_pretty(&doc).expect("share.json serialises");
        store.put("share.json", &json, "application/json")
    };

    report("uploading", 0)?;
    {
        let mut p = lock(progress);
        p.link = Some(link);
        p.state = State::Uploading;
    }

    let mut uploaded = 0u64;
    let mut reported = Instant::now();
    let result = (|| {
        for item in &contents.items {
            let mut reader = item.open()?;
            let kind = content_type(&item.name);
            if item.len <= PART as u64 {
                let mut body = Vec::with_capacity(item.len as usize);
                std::io::Read::read_to_end(&mut reader, &mut body)
                    .map_err(|e| ShareError::Read(e.to_string()))?;
                store.put(&item.name, &body, kind)?;
                uploaded += body.len() as u64;
            } else {
                let upload_id = store.start_parts(&item.name, kind)?;
                let mut etags = Vec::new();
                let mut buf = vec![0u8; PART];
                let parts = (|| {
                    loop {
                        let n = fill(&mut reader, &mut buf)?;
                        if n == 0 {
                            break;
                        }
                        let number = etags.len() as u16 + 1;
                        etags.push(store.put_part(&item.name, &upload_id, number, &buf[..n])?);
                        uploaded += n as u64;
                        lock(progress).uploaded = uploaded;
                        if reported.elapsed() >= REPORT_EVERY {
                            report("uploading", uploaded)?;
                            reported = Instant::now();
                        }
                    }
                    store.finish_parts(&item.name, &upload_id, &etags)
                })();
                if let Err(e) = parts {
                    let _ = store.abort_parts(&item.name, &upload_id);
                    return Err(e);
                }
            }
            lock(progress).uploaded = uploaded;
        }
        Ok(())
    })();

    match result {
        Ok(()) => {
            report("ready", total)?;
            lock(progress).state = State::Ready;
            Ok(())
        }
        Err(e) => {
            // Tell anyone already watching the link, if the storage still
            // answers.
            let _ = report("failed", uploaded);
            Err(e)
        }
    }
}

/// Read until `buf` is full or the file ends.
fn fill(reader: &mut impl std::io::Read, buf: &mut [u8]) -> Result<usize, ShareError> {
    let mut n = 0;
    while n < buf.len() {
        match reader.read(&mut buf[n..]) {
            Ok(0) => break,
            Ok(k) => n += k,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(e) => return Err(ShareError::Read(e.to_string())),
        }
    }
    Ok(n)
}

fn content_type(name: &str) -> &'static str {
    if name.ends_with(".mp4") {
        "video/mp4"
    } else if name.ends_with(".json") {
        "application/json"
    } else {
        "application/octet-stream"
    }
}

/// A random id that cannot be guessed: 128 bits, as the Screenilux storage
/// makes its own.
fn new_id() -> Result<String, ShareError> {
    use base64::Engine;
    let mut bytes = [0u8; 16];
    getrandom::fill(&mut bytes).map_err(|e| ShareError::Network(e.to_string()))?;
    Ok(base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_are_long_and_url_safe() {
        let a = new_id().unwrap();
        assert_eq!(a.len(), 22);
        assert!(
            a.chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        );
        assert_ne!(a, new_id().unwrap());
    }

    #[test]
    fn fill_reads_across_short_reads() {
        let data: Vec<u8> = (0..100).collect();
        let mut reader = std::io::Read::chain(&data[..30], &data[30..]);
        let mut buf = [0u8; 64];
        assert_eq!(fill(&mut reader, &mut buf).unwrap(), 64);
        assert_eq!(&buf[..], &data[..64]);
        assert_eq!(fill(&mut reader, &mut buf).unwrap(), 36);
    }

    #[test]
    fn a_bucket_needs_its_address_keys_and_public_address() {
        let mut b = Bucket {
            endpoint: "https://x.r2.cloudflarestorage.com".into(),
            bucket: "videos".into(),
            access_key: "a".into(),
            secret_key: "s".into(),
            ..Bucket::default()
        };
        assert!(!b.is_complete());
        b.public_url = "https://pub-x.r2.dev".into();
        assert!(b.is_complete());
    }
}
