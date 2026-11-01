//! End-to-end encryption of shared recordings.
//!
//! Every file of a share is encrypted on this computer with a key that only
//! travels in the link, after the `#`, which browsers never send to a server.
//! The storage, Screenilux's or the user's own, holds nothing it can read.
//!
//! The format lets any part of a file be read without the rest, so the web
//! player can seek in a video (web/public/seal.js reads it):
//!
//! ```text
//! "SXE1" | salt (16 bytes) | chunk 0 | chunk 1 | ...
//! ```
//!
//! Each chunk is up to [`CHUNK`] bytes of the file encrypted with AES-256-GCM,
//! so [`TAG`] bytes longer, under a key derived for this file from the share's
//! key and the salt with HKDF-SHA256. The nonce is the chunk's number with
//! its last byte marking the final chunk, so chunks cannot be reordered,
//! dropped or cut short without the player noticing (the STREAM
//! construction). An empty file is one empty final chunk.

use std::io::Read;

use aes_gcm::aead::{Aead, KeyInit};
use aes_gcm::{Aes256Gcm, Nonce};
use hkdf::Hkdf;
use sha2::Sha256;

use crate::ShareError;

pub const MAGIC: &[u8; 4] = b"SXE1";
pub const SALT: usize = 16;
pub const HEADER: usize = MAGIC.len() + SALT;
pub const CHUNK: usize = 64 * 1024;
pub const TAG: usize = 16;
const INFO: &[u8] = b"screenilux file";

/// The key of one share. It is in the link and nowhere else.
pub struct ShareKey([u8; 32]);

impl ShareKey {
    pub fn generate() -> Result<Self, ShareError> {
        let mut key = [0u8; 32];
        getrandom::fill(&mut key).map_err(|e| ShareError::Network(e.to_string()))?;
        Ok(Self(key))
    }

    /// How the key is written in a link.
    pub fn to_link(&self) -> String {
        use base64::Engine;
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(self.0)
    }

    /// The key from how a link writes it.
    pub fn from_link(text: &str) -> Option<Self> {
        use base64::Engine;
        let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(text)
            .ok()?;
        Some(Self(bytes.try_into().ok()?))
    }

    fn cipher_for(&self, salt: &[u8; SALT]) -> Aes256Gcm {
        let mut file_key = [0u8; 32];
        Hkdf::<Sha256>::new(Some(salt), &self.0)
            .expand(INFO, &mut file_key)
            .expect("32 bytes is a valid HKDF-SHA256 output length");
        Aes256Gcm::new_from_slice(&file_key).expect("a 32-byte AES-256 key")
    }
}

/// How long a file of `plain` bytes is once sealed.
pub fn sealed_len(plain: u64) -> u64 {
    let chunks = plain.div_ceil(CHUNK as u64).max(1);
    HEADER as u64 + plain + chunks * TAG as u64
}

fn nonce(index: u64, last: bool) -> Nonce<aes_gcm::aead::consts::U12> {
    let mut n = [0u8; 12];
    n[3..11].copy_from_slice(&index.to_be_bytes());
    n[11] = u8::from(last);
    Nonce::from(n)
}

/// Seals a file of a known length as it is read.
pub struct Sealer<R> {
    reader: R,
    cipher: Aes256Gcm,
    /// Plain bytes not read yet.
    remaining: u64,
    index: u64,
    /// Sealed bytes ready to hand out, and how many were handed out.
    out: Vec<u8>,
    at: usize,
    finished: bool,
}

impl<R: Read> Sealer<R> {
    pub fn new(reader: R, len: u64, key: &ShareKey) -> Result<Self, ShareError> {
        let mut salt = [0u8; SALT];
        getrandom::fill(&mut salt).map_err(|e| ShareError::Network(e.to_string()))?;
        Ok(Self::with_salt(reader, len, key, salt))
    }

    fn with_salt(reader: R, len: u64, key: &ShareKey, salt: [u8; SALT]) -> Self {
        let mut out = MAGIC.to_vec();
        out.extend_from_slice(&salt);
        Self {
            reader,
            cipher: key.cipher_for(&salt),
            remaining: len,
            index: 0,
            out,
            at: 0,
            finished: false,
        }
    }

    /// Seal the next chunk into `out`.
    fn next_chunk(&mut self) -> std::io::Result<()> {
        let take = self.remaining.min(CHUNK as u64) as usize;
        let mut plain = vec![0u8; take];
        self.reader.read_exact(&mut plain)?;
        self.remaining -= take as u64;
        let last = self.remaining == 0;
        self.out = self
            .cipher
            .encrypt(&nonce(self.index, last), plain.as_slice())
            .map_err(|_| std::io::Error::other("encryption failed"))?;
        self.at = 0;
        self.index += 1;
        self.finished = last;
        Ok(())
    }
}

impl<R: Read> Read for Sealer<R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        if self.at == self.out.len() {
            if self.finished {
                return Ok(0);
            }
            self.next_chunk()?;
        }
        let n = buf.len().min(self.out.len() - self.at);
        buf[..n].copy_from_slice(&self.out[self.at..self.at + n]);
        self.at += n;
        Ok(n)
    }
}

/// Seal a small file held in memory.
pub fn seal(key: &ShareKey, plain: &[u8]) -> Result<Vec<u8>, ShareError> {
    let mut sealed = Vec::with_capacity(sealed_len(plain.len() as u64) as usize);
    Sealer::new(plain, plain.len() as u64, key)?
        .read_to_end(&mut sealed)
        .map_err(|e| ShareError::Read(e.to_string()))?;
    Ok(sealed)
}

/// Open a whole sealed file: what the web player does, from this side.
/// `None` if the key is wrong or the file was changed or cut short.
pub fn open(key: &ShareKey, sealed: &[u8]) -> Option<Vec<u8>> {
    if sealed.len() < HEADER + TAG || &sealed[..MAGIC.len()] != MAGIC {
        return None;
    }
    let salt: [u8; SALT] = sealed[MAGIC.len()..HEADER].try_into().ok()?;
    let cipher = key.cipher_for(&salt);
    let chunks: Vec<&[u8]> = sealed[HEADER..].chunks(CHUNK + TAG).collect();
    let mut plain = Vec::new();
    for (i, c) in chunks.iter().enumerate() {
        let last = i + 1 == chunks.len();
        plain.extend(cipher.decrypt(&nonce(i as u64, last), *c).ok()?);
    }
    Some(plain)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn data(len: usize) -> Vec<u8> {
        (0..len).map(|i| (i * 7 % 256) as u8).collect()
    }

    #[test]
    fn round_trips_at_every_awkward_length() {
        let key = ShareKey::generate().unwrap();
        for len in [0, 1, CHUNK - 1, CHUNK, CHUNK + 1, 3 * CHUNK, 3 * CHUNK + 5] {
            let plain = data(len);
            let sealed = seal(&key, &plain).unwrap();
            assert_eq!(sealed.len() as u64, sealed_len(len as u64), "length {len}");
            assert_eq!(open(&key, &sealed).unwrap(), plain, "length {len}");
        }
    }

    #[test]
    fn another_key_cannot_open_it() {
        let sealed = seal(&ShareKey::generate().unwrap(), b"secret").unwrap();
        assert!(open(&ShareKey::generate().unwrap(), &sealed).is_none());
    }

    #[test]
    fn a_key_survives_the_link() {
        let key = ShareKey::generate().unwrap();
        let back = ShareKey::from_link(&key.to_link()).unwrap();
        assert_eq!(back.0, key.0);
        assert!(ShareKey::from_link("short").is_none());
    }

    #[test]
    fn a_cut_or_reordered_file_does_not_open() {
        let key = ShareKey::generate().unwrap();
        let sealed = seal(&key, &data(3 * CHUNK)).unwrap();
        // Dropping the final chunk leaves a chunk that was not sealed as last.
        assert!(open(&key, &sealed[..HEADER + 2 * (CHUNK + TAG)]).is_none());
        let mut swapped = sealed.clone();
        let (a, b) = (HEADER, HEADER + CHUNK + TAG);
        let first = sealed[a..b].to_vec();
        swapped[a..b].copy_from_slice(&sealed[b..b + CHUNK + TAG]);
        swapped[b..b + CHUNK + TAG].copy_from_slice(&first);
        assert!(open(&key, &swapped).is_none());
    }

    #[test]
    fn each_file_gets_its_own_salt() {
        let key = ShareKey::generate().unwrap();
        let (a, b) = (seal(&key, b"same").unwrap(), seal(&key, b"same").unwrap());
        assert_ne!(a[4..HEADER], b[4..HEADER]);
        assert_ne!(a[HEADER..], b[HEADER..]);
    }

    #[test]
    fn sealing_through_small_reads_matches() {
        let key = ShareKey([9; 32]);
        let plain = data(CHUNK + 100);
        let whole = {
            let mut v = Vec::new();
            Sealer::with_salt(&plain[..], plain.len() as u64, &key, [1; SALT])
                .read_to_end(&mut v)
                .unwrap();
            v
        };
        let mut pieces = Vec::new();
        let mut sealer = Sealer::with_salt(&plain[..], plain.len() as u64, &key, [1; SALT]);
        let mut buf = [0u8; 1000];
        loop {
            let n = sealer.read(&mut buf).unwrap();
            if n == 0 {
                break;
            }
            pieces.extend_from_slice(&buf[..n]);
        }
        assert_eq!(pieces, whole);
    }
}
