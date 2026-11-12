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
use std::time::{Duration, SystemTime};

use screenilux_core::session::MANIFEST_NAME;
use serde::{Deserialize, Serialize};

pub use contents::{Contents, Item};
use seal::Sealer;
pub use seal::{ShareKey, open, sealed_len};
pub use store::Rights;
use store::{Hosted, Own, Sent, Store};

/// Where the web player lives. Links point here.
pub const PLAYER: &str = "https://screenilux.hassen-ben-mbarek.workers.dev";

/// How long the Screenilux storage keeps a shared recording.
pub const KEPT_FOR: Duration = Duration::from_secs(7 * 24 * 60 * 60);

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
    #[error("too many recordings were shared from this network just now; try again in a minute")]
    TooMany,
    /// The storage said why it would not take the recording, in words meant
    /// for the person sharing: no room left this week, for instance.
    #[error("{0}")]
    Declined(String),
    #[error("the bucket settings are not usable: {0}")]
    Bucket(String),
    #[error("could not read the recording: {0}")]
    Read(String),
}

impl From<ureq::Error> for ShareError {
    fn from(e: ureq::Error) -> Self {
        match e {
            ureq::Error::StatusCode(401 | 403) => ShareError::NotAllowed,
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
    /// The Screenilux storage, behind `server`. `device` is this computer's
    /// id there, which its allowance is counted against (see
    /// [`new_device_id`]).
    Screenilux {
        server: String,
        device: String,
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
    /// When the link stops working, if it does.
    pub expires: Option<SystemTime>,
    /// The right to add files to the share later, such as its text once it
    /// has been read: keep it with the link.
    pub rights: Option<Rights>,
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
            expires: None,
            rights: None,
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
        Destination::Screenilux { server, device } => {
            let (hosted, link) = Hosted::create(agent, server, device, total)?;
            lock(progress).expires = Some(SystemTime::now() + KEPT_FOR);
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
        p.rights = Some(store.rights());
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
        sending(&mut uploaded, progress, |counted| {
            store.put(MANIFEST_NAME, &manifest, counted)
        })?;
        sent.push(MANIFEST_NAME.to_string());
        retrying(|| store.complete())
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
        return sending(uploaded, progress, |counted| {
            store.put(name, &body, counted)
        });
    }
    let upload_id = retrying(|| store.start_parts(name))?;
    let mut etags = Vec::new();
    let mut buf = vec![0u8; PART];
    let parts = (|| {
        loop {
            let n = fill(&mut reader, &mut buf)?;
            if n == 0 {
                break;
            }
            let number = etags.len() as u16 + 1;
            etags.push(sending(uploaded, progress, |counted| {
                store.put_part(name, &upload_id, number, &buf[..n], counted)
            })?);
        }
        retrying(|| store.finish_parts(name, &upload_id, &etags))
    })();
    if parts.is_err() {
        let _ = store.abort_parts(name, &upload_id);
    }
    parts
}

/// How many times one request is tried before the upload gives up. A long
/// upload over a shaky connection will see the odd dropped connection, and
/// losing the whole recording to one of them is not worth it.
const TRIES: u32 = 5;

/// The wait before trying again, doubled each time: 1, 2, 4 and 8 seconds.
const BACKOFF: Duration = if cfg!(test) {
    Duration::ZERO
} else {
    Duration::from_secs(1)
};

impl ShareError {
    /// Whether trying the same request again could go better: the connection
    /// failed, or the storage had trouble of its own.
    fn is_passing(&self) -> bool {
        match self {
            ShareError::Network(_) => true,
            ShareError::Refused(code) => *code == 408 || *code >= 500,
            _ => false,
        }
    }
}

/// Run `request`, trying again after a passing failure. Every request an
/// upload makes can be repeated: a file or part sent again replaces the one
/// before.
fn retrying<T>(mut request: impl FnMut() -> Result<T, ShareError>) -> Result<T, ShareError> {
    let mut tries = 1;
    loop {
        match request() {
            Err(e) if tries < TRIES && e.is_passing() => {
                tracing::info!(error = %e, tries, "an upload request failed; trying again");
                std::thread::sleep(BACKOFF * (1 << (tries - 1)));
                tries += 1;
            }
            result => return result,
        }
    }
}

/// Run `request`, which sends a body and reports the bytes it hands out to
/// the callback it is given, trying again as [`retrying`] does. Progress
/// goes back to where it was before each try, so bytes are counted once.
fn sending<T>(
    uploaded: &mut u64,
    progress: &Mutex<Progress>,
    mut request: impl FnMut(Sent<'_>) -> Result<T, ShareError>,
) -> Result<T, ShareError> {
    let before = *uploaded;
    retrying(|| {
        *uploaded = before;
        lock(progress).uploaded = before;
        request(&mut |n| {
            *uploaded += n as u64;
            lock(progress).uploaded = *uploaded;
        })
    })
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

/// Add a recording's text index to a share that already exists, so its link
/// can search and copy text too. `bucket` is needed for a share in the
/// user's own bucket, and is the bucket settings as they are now.
pub fn add_text(
    rights: &Rights,
    bucket: Option<&Bucket>,
    link: &str,
    index_json: &[u8],
) -> Result<(), ShareError> {
    let key = link
        .split_once("#")
        .and_then(|(_, fragment)| {
            url::form_urlencoded::parse(fragment.as_bytes()).find(|(k, _)| k == "k")
        })
        .and_then(|(_, v)| ShareKey::from_link(&v))
        .ok_or_else(|| ShareError::Read("the link has no key".into()))?;
    let sealed = seal::seal(&key, index_json)?;
    let agent = ureq::Agent::new_with_defaults();
    let store: Box<dyn Store> = match rights {
        Rights::Screenilux { recording, token } => {
            Box::new(Hosted::resume(agent, recording, token))
        }
        Rights::Bucket { prefix } => {
            let bucket = bucket.ok_or_else(|| {
                ShareError::Bucket("sharing to your own bucket is not set up".into())
            })?;
            Box::new(Own::resume(agent, bucket, prefix)?)
        }
    };
    retrying(|| store.put(screenilux_core::text::TEXT_INDEX_NAME, &sealed, &mut |_| {}))
}

/// A new id for this computer on the Screenilux storage, made once and kept.
/// It is random, so it says nothing about the computer or who uses it.
pub fn new_device_id() -> String {
    new_id().expect("the system has randomness")
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

    /// A store whose requests fail, the way `fail` says, before they work.
    struct Flaky {
        fail: Mutex<Vec<ShareError>>,
        tries: Mutex<u32>,
        parts: Mutex<Vec<u16>>,
    }

    impl Flaky {
        fn new(fail: Vec<ShareError>) -> Self {
            Self {
                fail: Mutex::new(fail),
                tries: Mutex::new(0),
                parts: Mutex::new(Vec::new()),
            }
        }

        /// Send half of `body`, then fail if a failure is due.
        fn send(&self, body: &[u8], sent: Sent<'_>) -> Result<(), ShareError> {
            *self.tries.lock().unwrap() += 1;
            let mut fail = self.fail.lock().unwrap();
            if fail.is_empty() {
                sent(body.len());
                Ok(())
            } else {
                sent(body.len() / 2);
                Err(fail.remove(0))
            }
        }
    }

    impl Store for Flaky {
        fn rights(&self) -> Rights {
            Rights::Bucket {
                prefix: String::new(),
            }
        }
        fn put(&self, _: &str, body: &[u8], sent: Sent<'_>) -> Result<(), ShareError> {
            self.send(body, sent)
        }
        fn start_parts(&self, _: &str) -> Result<String, ShareError> {
            Ok("up".into())
        }
        fn put_part(
            &self,
            _: &str,
            _: &str,
            number: u16,
            body: &[u8],
            sent: Sent<'_>,
        ) -> Result<String, ShareError> {
            self.send(body, sent)?;
            self.parts.lock().unwrap().push(number);
            Ok(format!("e{number}"))
        }
        fn finish_parts(&self, _: &str, _: &str, etags: &[String]) -> Result<(), ShareError> {
            assert_eq!(etags.len(), self.parts.lock().unwrap().len());
            Ok(())
        }
        fn abort_parts(&self, _: &str, _: &str) -> Result<(), ShareError> {
            Ok(())
        }
        fn complete(&self) -> Result<(), ShareError> {
            Ok(())
        }
        fn discard(&self, _: &[String]) -> Result<(), ShareError> {
            Ok(())
        }
    }

    fn progress() -> Mutex<Progress> {
        Mutex::new(Progress {
            link: None,
            uploaded: 0,
            total: 0,
            state: State::Uploading,
            expires: None,
            rights: None,
        })
    }

    fn reset() -> ShareError {
        ShareError::Network("io: An existing connection was forcibly closed".into())
    }

    #[test]
    fn a_dropped_part_is_sent_again_and_counted_once() {
        let store = Flaky::new(vec![reset(), ShareError::Refused(502)]);
        let data = vec![7u8; PART + 1000];
        let (mut uploaded, p) = (0, progress());
        upload(
            &store,
            "screen1.mp4",
            &data[..],
            data.len() as u64,
            &mut uploaded,
            &p,
        )
        .unwrap();
        assert_eq!(*store.tries.lock().unwrap(), 4);
        assert_eq!(*store.parts.lock().unwrap(), vec![1, 2]);
        assert_eq!(uploaded, data.len() as u64);
        assert_eq!(p.lock().unwrap().uploaded, data.len() as u64);
    }

    #[test]
    fn an_upload_gives_up_after_a_few_tries() {
        let store = Flaky::new((0..TRIES).map(|_| reset()).collect());
        let (mut uploaded, p) = (0, progress());
        let e = upload(
            &store,
            "recording.json",
            &[1u8; 10][..],
            10,
            &mut uploaded,
            &p,
        );
        assert!(matches!(e, Err(ShareError::Network(_))));
        assert_eq!(*store.tries.lock().unwrap(), TRIES);
    }

    #[test]
    fn a_refusal_is_not_tried_again() {
        let store = Flaky::new(vec![ShareError::Refused(413)]);
        let (mut uploaded, p) = (0, progress());
        let e = upload(
            &store,
            "recording.json",
            &[1u8; 10][..],
            10,
            &mut uploaded,
            &p,
        );
        assert!(matches!(e, Err(ShareError::Refused(413))));
        assert_eq!(*store.tries.lock().unwrap(), 1);
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
