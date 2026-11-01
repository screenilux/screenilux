// Opening the encrypted files of a shared recording, in the viewer's browser.
//
// The desktop app writes the format (crates/share/src/seal.rs):
//
//   "SXE1" | salt (16 bytes) | chunk 0 | chunk 1 | ...
//
// Each chunk is up to CHUNK bytes of the file under AES-256-GCM, so TAG bytes
// longer. The key for the file comes from the share's key (in the link, after
// the #) and the salt, through HKDF-SHA256. A chunk's nonce is its number,
// with the last byte set on the final chunk, so a file cannot be reordered or
// cut short without decryption failing.
//
// Loaded by the service worker (sw.js) with importScripts.

self.Seal = (() => {
  const MAGIC = [0x53, 0x58, 0x45, 0x31];
  const SALT = 16;
  const HEADER = MAGIC.length + SALT;
  const CHUNK = 64 * 1024;
  const TAG = 16;
  const SEALED = CHUNK + TAG;
  const INFO = new TextEncoder().encode("screenilux file");

  /** The share's key from how a link writes it (base64url). */
  function keyFromLink(text) {
    const plain = atob(text.replace(/-/g, "+").replace(/_/g, "/"));
    const key = Uint8Array.from(plain, (c) => c.charCodeAt(0));
    if (key.length !== 32) throw new Error("the key in the link is not whole");
    return key;
  }

  /** The salt from a file's first HEADER bytes. */
  function saltOf(header) {
    if (header.length < HEADER || MAGIC.some((b, i) => header[i] !== b)) {
      throw Object.assign(new Error("not an encrypted Screenilux file"), { locked: true });
    }
    return header.slice(MAGIC.length, HEADER);
  }

  async function fileKey(shareKey, salt) {
    const base = await crypto.subtle.importKey("raw", shareKey, "HKDF", false, ["deriveKey"]);
    return crypto.subtle.deriveKey(
      { name: "HKDF", hash: "SHA-256", salt, info: INFO },
      base,
      { name: "AES-GCM", length: 256 },
      false,
      ["decrypt"],
    );
  }

  function nonce(index, last) {
    const n = new Uint8Array(12);
    let i = index;
    for (let b = 10; b >= 3; b--) {
      n[b] = i % 256;
      i = Math.floor(i / 256);
    }
    n[11] = last ? 1 : 0;
    return n;
  }

  function chunkCount(sealedSize) {
    return Math.max(1, Math.ceil((sealedSize - HEADER) / SEALED));
  }

  /** How long a sealed file is once opened. */
  function plainSize(sealedSize) {
    return sealedSize - HEADER - chunkCount(sealedSize) * TAG;
  }

  async function openChunk(key, index, last, sealed) {
    return new Uint8Array(await crypto.subtle.decrypt({ name: "AES-GCM", iv: nonce(index, last) }, key, sealed));
  }

  /** A whole sealed file, opened. Throws if the key is wrong or the file changed. */
  async function openAll(shareKey, sealed) {
    const key = await fileKey(shareKey, saltOf(sealed));
    const count = chunkCount(sealed.length);
    const parts = [];
    for (let i = 0; i < count; i++) {
      const start = HEADER + i * SEALED;
      parts.push(await openChunk(key, i, i === count - 1, sealed.subarray(start, Math.min(start + SEALED, sealed.length))));
    }
    return concat(parts);
  }

  /**
   * Bytes `start` to `end` (inclusive) of a sealed file of `sealedSize` bytes,
   * opened. `fetchRange(from, to)` gets sealed bytes from..to inclusive.
   */
  async function openRange(key, sealedSize, start, end, fetchRange) {
    const count = chunkCount(sealedSize);
    const first = Math.floor(start / CHUNK);
    const last = Math.floor(end / CHUNK);
    const from = HEADER + first * SEALED;
    const to = Math.min(HEADER + (last + 1) * SEALED, sealedSize) - 1;
    const sealed = await fetchRange(from, to);
    const parts = [];
    for (let i = first; i <= last; i++) {
      const at = (i - first) * SEALED;
      parts.push(await openChunk(key, i, i === count - 1, sealed.subarray(at, Math.min(at + SEALED, sealed.length))));
    }
    const plain = concat(parts);
    return plain.subarray(start - first * CHUNK, end - first * CHUNK + 1);
  }

  function concat(parts) {
    const out = new Uint8Array(parts.reduce((n, p) => n + p.length, 0));
    let at = 0;
    for (const p of parts) {
      out.set(p, at);
      at += p.length;
    }
    return out;
  }

  return { HEADER, keyFromLink, saltOf, fileKey, plainSize, openAll, openRange };
})();
