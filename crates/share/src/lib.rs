//! Share links: upload a finished recording and get a link that plays it in a
//! browser.
//!
//! A shared recording is a folder of files: each screen's video, the text
//! index if there is one, and the manifest. Every file is encrypted here
//! first (see [`seal`]), with a key that only the link carries, so whoever
//! keeps the files cannot watch them.
//!
//! The folder goes either to the Screenilux storage, which hands out an
//! upload token for each recording and keeps track of how its upload is
//! going, or to the user's own S3-compatible bucket. The link exists before
//! anything is uploaded, so it can be sent at once; the web player (`web/`)
//! waits for the upload to finish. In a bucket there is no server to ask, so
//! the manifest goes up last and its arrival is what says the upload is done.

mod contents;
mod seal;
mod store;

use std::io::Read;
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

use screenilux_core::session::MANIFEST_NAME;
use serde::{Deserialize, Serialize};

pub use contents::{Contents, Item};
use seal::Sealer;
pub use seal::{ShareKey, open, sealed_len};
use store::{Hosted, Own, Store};

/// Where the web player lives. Links point here.
pub const PLAYER: &str = "https://screenilux.hassen-ben-mbarek.workers.dev";

/// Files bigger than this go up in parts of this size. The Screenilux storage
/// takes at most 100MB in one request; buckets need parts of at least 5MB.
const PART: usize = 32 * 1024 * 1024;

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

fn run(
    destination: &Destination,
    contents: &Contents,
    progress: &Mutex<Progress>,
) -> Result<(), ShareError> {
    let agent = ureq::Agent::new_with_defaults();
    let key = ShareKey::generate()?;
    let manifest = seal::seal(&key, &titled_manifest(contents)?)?;
    let files: Vec<&Item> = contents
        .items
        .iter()
        .filter(|i| i.name != MANIFEST_NAME)
        .collect();
    let total = files.iter().map(|i| sealed_len(i.len)).sum::<u64>() + manifest.len() as u64;
    lock(progress).total = total;

    let (store, link): (Box<dyn Store>, String) = match destination {
        Destination::Screenilux { server } => {
            let (hosted, link) = Hosted::create(agent, server, total)?;
            (Box::new(hosted), format!("{link}#k={}", key.to_link()))
        }
        Destination::Bucket(bucket) => {
            let (own, link) = Own::create(agent, bucket, &new_id()?, &key.to_link())?;
            (Box::new(own), link)
        }
    };
    {
        let mut p = lock(progress);
        p.link = Some(link);
        p.state = State::Uploading;
    }

    let mut uploaded = 0u64;
    let mut sent = Vec::new();
    let result = (|| {
        for item in &files {
            let sealer = Sealer::new(item.open()?, item.len, &key)?;
            upload(
                store.as_ref(),
                &item.name,
                sealer,
                sealed_len(item.len),
                &mut uploaded,
                progress,
            )?;
            sent.push(item.name.clone());
        }
        store.put(MANIFEST_NAME, &manifest)?;
        sent.push(MANIFEST_NAME.to_string());
        uploaded += manifest.len() as u64;
        lock(progress).uploaded = uploaded;
        store.complete()
    })();

    match result {
        Ok(()) => {
            lock(progress).state = State::Ready;
            Ok(())
        }
        Err(e) => {
            // Half a recording is no use to anyone; take back what went up.
            let _ = store.discard(&sent);
            Err(e)
        }
    }
}

/// The manifest as uploaded: the recording's own, plus its title, which the
/// player shows. Both are encrypted, so the title stays private.
fn titled_manifest(contents: &Contents) -> Result<Vec<u8>, ShareError> {
    let item = contents
        .items
        .iter()
        .find(|i| i.name == MANIFEST_NAME)
        .ok_or_else(|| ShareError::Read("the recording has no manifest".into()))?;
    let mut json = Vec::new();
    item.open()?
        .read_to_end(&mut json)
        .map_err(|e| ShareError::Read(e.to_string()))?;
    let mut manifest: serde_json::Value =
        serde_json::from_slice(&json).map_err(|e| ShareError::Read(e.to_string()))?;
    manifest["title"] = contents.title.clone().into();
    Ok(serde_json::to_vec(&manifest).expect("a JSON value serialises"))
}

/// Upload one file of `len` bytes, read from `reader`, in parts if it is big.
fn upload(
    store: &dyn Store,
    name: &str,
    mut reader: impl Read,
    len: u64,
    uploaded: &mut u64,
    progress: &Mutex<Progress>,
) -> Result<(), ShareError> {
    if len <= PART as u64 {
        let mut body = Vec::with_capacity(len as usize);
        reader
            .read_to_end(&mut body)
            .map_err(|e| ShareError::Read(e.to_string()))?;
        store.put(name, &body)?;
        *uploaded += body.len() as u64;
        lock(progress).uploaded = *uploaded;
        return Ok(());
    }
    let upload_id = store.start_parts(name)?;
    let mut etags = Vec::new();
    let mut buf = vec![0u8; PART];
    let parts = (|| {
        loop {
            let n = fill(&mut reader, &mut buf)?;
            if n == 0 {
                break;
            }
            let number = etags.len() as u16 + 1;
            etags.push(store.put_part(name, &upload_id, number, &buf[..n])?);
            *uploaded += n as u64;
            lock(progress).uploaded = *uploaded;
        }
        store.finish_parts(name, &upload_id, &etags)
    })();
    if parts.is_err() {
        let _ = store.abort_parts(name, &upload_id);
    }
    parts
}

/// Read until `buf` is full or the file ends.
fn fill(reader: &mut impl Read, buf: &mut [u8]) -> Result<usize, ShareError> {
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
        let mut reader = Read::chain(&data[..30], &data[30..]);
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
