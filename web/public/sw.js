// Decrypts shared recordings for the web player, inside the viewer's browser.
//
// The page asks for /__decrypt/<session>/<file>. This worker fetches the
// encrypted file from where the recording is kept, opens the part that was
// asked for, and answers with plain bytes, so a <video> can play and seek as
// if the video were not encrypted. The share's key never leaves the browser.
//
// The page leaves each session's key and address in Cache Storage under
// /__keys/<session>, since this worker can be stopped and started again at
// any time and would lose anything kept only in memory.

importScripts("/seal.js");

const KEYS = "screenilux-keys";
// The most one answer to an open-ended range carries; the video asks again.
const MAX_SPAN = 4 * 1024 * 1024;

self.addEventListener("install", () => self.skipWaiting());
self.addEventListener("activate", (event) => event.waitUntil(self.clients.claim()));

self.addEventListener("fetch", (event) => {
  const url = new URL(event.request.url);
  if (url.origin !== self.location.origin || !url.pathname.startsWith("/__decrypt/")) return;
  event.respondWith(
    serve(event.request, url).catch((e) => {
      console.error("decrypting failed", e);
      // A file that does not open with the key is 422; anything else, such
      // as the storage not answering, is 502.
      const locked = e?.name === "OperationError" || e?.locked;
      return new Response(String(e?.message ?? e), { status: locked ? 422 : 502 });
    }),
  );
});

const sessions = new Map();
// Per encrypted file: its size and key, worked out once.
const files = new Map();

async function session(id) {
  if (!sessions.has(id)) {
    const stored = await (await caches.open(KEYS)).match(`/__keys/${id}`);
    if (!stored) return undefined;
    const { base, key } = await stored.json();
    sessions.set(id, { base, key: Seal.keyFromLink(key) });
  }
  return sessions.get(id);
}

function status(code) {
  return new Response(null, { status: code });
}

function typeOf(name) {
  return name.endsWith(".mp4") ? "video/mp4" : name.endsWith(".json") ? "application/json" : "application/octet-stream";
}

/** fetch, tried once more if the network or the storage stumbles. */
async function fetchAgain(url, init) {
  try {
    const res = await fetch(url, init);
    if (res.status < 500) return res;
  } catch {}
  return fetch(url, init);
}

async function fetchRange(source, from, to) {
  const res = await fetchAgain(source, { headers: { Range: `bytes=${from}-${to}` } });
  if (res.status !== 206 && res.status !== 200) throw Object.assign(new Error("fetch"), { code: res.status });
  const bytes = new Uint8Array(await res.arrayBuffer());
  // A server that ignores Range sends the whole file.
  return res.status === 200 ? bytes.subarray(from, to + 1) : bytes;
}

async function fileInfo(source, shareKey) {
  if (!files.has(source)) {
    const info = (async () => {
      const res = await fetchAgain(source, { headers: { Range: `bytes=0-${Seal.HEADER - 1}` } });
      if (!res.ok) throw Object.assign(new Error("fetch"), { code: res.status });
      const total = Number((res.headers.get("Content-Range") ?? "").split("/")[1] ?? res.headers.get("Content-Length"));
      const header = new Uint8Array(await res.arrayBuffer()).subarray(0, Seal.HEADER);
      return { size: total, key: await Seal.fileKey(shareKey, Seal.saltOf(header)) };
    })();
    files.set(source, info);
    info.catch(() => files.delete(source));
  }
  return files.get(source);
}

async function serve(request, url) {
  const [, , id, ...rest] = url.pathname.split("/");
  const name = rest.join("/");
  const s = await session(id);
  if (!s) return status(410);
  const source = s.base + name;
  const headers = { "Content-Type": typeOf(name), "Accept-Ranges": "bytes", "Cache-Control": "no-store" };

  const range = /^bytes=(\d*)-(\d*)$/.exec(request.headers.get("Range") ?? "");
  if (!range) {
    // A small file, such as the manifest: fetch and open all of it.
    const res = await fetchAgain(source);
    if (!res.ok) return status(res.status);
    const plain = await Seal.openAll(s.key, new Uint8Array(await res.arrayBuffer()));
    return new Response(plain, { headers });
  }

  let info;
  try {
    info = await fileInfo(source, s.key);
  } catch (e) {
    if (e.code) return status(e.code);
    throw e;
  }
  const size = Seal.plainSize(info.size);
  let start;
  let end;
  if (range[1] === "") {
    start = Math.max(0, size - Number(range[2]));
    end = size - 1;
  } else {
    start = Number(range[1]);
    end = range[2] === "" ? Math.min(size - 1, start + MAX_SPAN - 1) : Math.min(size - 1, Number(range[2]));
  }
  if (start >= size || end < start) {
    return new Response(null, { status: 416, headers: { "Content-Range": `bytes */${size}` } });
  }
  const plain = await Seal.openRange(info.key, info.size, start, end, (from, to) => fetchRange(source, from, to));
  return new Response(plain, {
    status: 206,
    headers: { ...headers, "Content-Range": `bytes ${start}-${end}/${size}`, "Content-Length": String(plain.length) },
  });
}
