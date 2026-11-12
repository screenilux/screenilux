// Share links: the desktop app uploads a recording here, and the web player
// reads it back.
//
// A shared recording is a folder of files under rec/<id>/ in R2, every one
// encrypted by the app with a key that only the link carries (after the #,
// which browsers never send). This server cannot read any of them:
//
//   recording.json  the manifest
//   screen1.mp4 ... one video per screen
//   camera.mp4      the camera, if it was recorded
//   text.json       the text index, if the recording was made searchable
//   _state          the upload session, kept by this server; never served
//
// Anyone with the app can share, so nothing secret is in the app. Instead:
//
// - Starting a recording needs nothing, but each address can start only a few
//   a minute.
// - Storage costs money past Cloudflare's free allowance, so each computer
//   can keep SHARE_PER_DEVICE bytes shared at a time, counted over the week
//   a link lasts. There are no accounts: the app makes a random device id
//   once. Since a new id is easy to make, each network (address) is also held
//   to SHARE_PER_NETWORK. The counts are in D1 (migrations/).
// - Starting one hands back an upload token that works for that recording
//   only: it cannot read, list or change any other. The token is a signature
//   of the recording's id, so the server keeps nothing to check it against.
// - A recording can hold at most the bytes it asked for, counted as they
//   arrive. A file or part sent again replaces what was counted for it.
// - The upload's progress is what this server counted, and it is ready when
//   the app says it has finished. Anyone with the id can ask how it is going.
// - Reading needs only the id, which is random and long enough that links
//   cannot be guessed. R2 deletes everything a week after it was uploaded (a
//   lifecycle rule on the bucket).
// - The admin key (UPLOAD_KEY) can delete any recording, to take one down.

// Recording and device ids: 128 random bits, base64url.
const ID = /^[A-Za-z0-9_-]{22}$/;
// Any plain file name can be read, but only the files a recording is made of
// can be written.
const NAME = /^[A-Za-z0-9][A-Za-z0-9._-]{0,99}$/;
const WRITABLE = /^(recording|text)\.json$|^screen[1-9][0-9]?\.mp4$|^camera\.mp4$/;
const PART = /^[1-9][0-9]{0,4}$/;

/** What one computer can have shared at a time, over the week links last. */
const SHARE_PER_DEVICE = 1024 ** 3;
/** What one network can have shared at a time, however many computers. */
const SHARE_PER_NETWORK = 5 * 1024 ** 3;
/** The most one upload request can carry. The app sends 32MB parts. */
const MAX_REQUEST = 50 * 1024 ** 2;
/**
 * A recording's text index can be added once the recording is done, when it is
 * made searchable later, so it is allowed beyond what the recording asked
 * for, up to this.
 */
const MAX_TEXT = 16 * 1024 ** 2;
/** How long a recording is kept; the bucket's lifecycle rule does the deleting. */
const KEPT_FOR_MS = 7 * 24 * 60 * 60 * 1000;
/** An upload that has sent nothing for this long has stopped. */
const STALLED_AFTER_MS = 10 * 60 * 1000;

/** What the server knows about a recording's upload. */
interface UploadState {
  status: "uploading" | "ready";
  /** Bytes the app said it would send, and bytes received so far. */
  total: number;
  received: number;
  /**
   * The bytes counted for each file, or each part of one ("screen1.mp4/3"),
   * so one sent again after a dropped connection replaces its count instead
   * of adding to it. Missing in uploads started before this was kept.
   */
  written?: Record<string, number>;
  /** When the upload started, and when bytes last arrived, in ms. */
  started: number;
  updated: number;
}

export default {
  async fetch(request, env): Promise<Response> {
    const url = new URL(request.url);
    try {
      if (url.pathname.startsWith("/api/")) {
        const response = await api(request, env, url.pathname.slice("/api/".length).split("/"));
        // A refused upload is answered without reading it; drop what is left
        // so the connection is not left waiting on it.
        if (request.body && !request.bodyUsed) await request.body.cancel();
        return response;
      }
      if (url.pathname.startsWith("/r/")) {
        return await read(request, env, url.pathname.slice("/r/".length).split("/"));
      }
      // /__decrypt/ is answered inside the viewer's browser (web/public/sw.js);
      // reaching the server means that did not happen.
      return problem(404, "Not found.");
    } catch (e) {
      console.error(JSON.stringify({ message: "request failed", path: url.pathname, error: String(e) }));
      return problem(500, "Something went wrong on the server.");
    }
  },
} satisfies ExportedHandler<Env>;

const encoder = new TextEncoder();

function base64url(bytes: ArrayBuffer | Uint8Array): string {
  const array = bytes instanceof Uint8Array ? bytes : new Uint8Array(bytes);
  return btoa(String.fromCharCode(...array)).replace(/\+/g, "-").replace(/\//g, "_").replace(/=+$/, "");
}

function fromBase64url(text: string): Uint8Array | undefined {
  try {
    const plain = atob(text.replace(/-/g, "+").replace(/_/g, "/"));
    return Uint8Array.from(plain, (c) => c.charCodeAt(0));
  } catch {
    return undefined;
  }
}

function signingKey(env: Env): Promise<CryptoKey> {
  return crypto.subtle.importKey("raw", encoder.encode(env.SIGNING_KEY), { name: "HMAC", hash: "SHA-256" }, false, [
    "sign",
    "verify",
  ]);
}

/** The upload token for one recording. */
async function tokenFor(env: Env, id: string): Promise<string> {
  return base64url(await crypto.subtle.sign("HMAC", await signingKey(env), encoder.encode(`upload:${id}`)));
}

function bearer(request: Request): string {
  return request.headers.get("Authorization")?.replace(/^Bearer /, "") ?? "";
}

async function isAdmin(request: Request, env: Env): Promise<boolean> {
  if (!env.UPLOAD_KEY) return false;
  const [a, b] = await Promise.all([
    crypto.subtle.digest("SHA-256", encoder.encode(bearer(request))),
    crypto.subtle.digest("SHA-256", encoder.encode(env.UPLOAD_KEY)),
  ]);
  return crypto.subtle.timingSafeEqual(a, b);
}

/** Whether the request may change recording `id`: its own token, or the admin key. */
async function mayChange(request: Request, env: Env, id: string): Promise<boolean> {
  const signature = fromBase64url(bearer(request));
  // `verify` compares in constant time.
  if (signature && (await crypto.subtle.verify("HMAC", await signingKey(env), signature, encoder.encode(`upload:${id}`)))) {
    return true;
  }
  return isAdmin(request, env);
}

/**
 * Change a recording's upload state. Several requests can arrive at once, so
 * the state is only replaced if no one else changed it in between. `change`
 * returns the new state, or a reason to refuse.
 */
async function updateState(
  env: Env,
  id: string,
  change: (state: UploadState) => UploadState | "full",
): Promise<"ok" | "gone" | "full"> {
  const key = `rec/${id}/_state`;
  for (let attempt = 0; attempt < 8; attempt++) {
    const current = await env.RECORDINGS.get(key);
    if (!current) return "gone";
    const next = change(await current.json<UploadState>());
    if (next === "full") return "full";
    const stored = await env.RECORDINGS.put(key, JSON.stringify(next), { onlyIf: { etagMatches: current.etag } });
    if (stored) return "ok";
  }
  throw new Error("the upload state kept changing");
}

/** A keyed hash of the address a request came from, so no address is stored. */
async function networkOf(request: Request, env: Env): Promise<string> {
  const address = request.headers.get("CF-Connecting-IP") ?? "unknown";
  const mac = await crypto.subtle.sign("HMAC", await signingKey(env), encoder.encode(`network:${address}`));
  return base64url(new Uint8Array(mac).slice(0, 16));
}

function size(bytes: number): string {
  return bytes >= 1024 ** 3 ? `${(bytes / 1024 ** 3).toFixed(1)}GB` : `${Math.ceil(bytes / 1024 ** 2)}MB`;
}

/**
 * Count a new share of `total` bytes against its device and network, if both
 * have room. One statement checks and records, so two shares at once cannot
 * both squeeze into the same space. Returns why not, if not.
 */
async function claim(env: Env, id: string, device: string, network: string, total: number): Promise<string | undefined> {
  const now = Date.now();
  const since = now - KEPT_FOR_MS;
  await env.SHARES.prepare("DELETE FROM shares WHERE started <= ?1").bind(since).run();
  const claimed = await env.SHARES.prepare(
    `INSERT INTO shares (id, device, network, bytes, started)
     SELECT ?1, ?2, ?3, ?4, ?5
     WHERE (SELECT COALESCE(SUM(bytes), 0) FROM shares WHERE device = ?2 AND started > ?6) + ?4 <= ?7
       AND (SELECT COALESCE(SUM(bytes), 0) FROM shares WHERE network = ?3 AND started > ?6) + ?4 <= ?8`,
  )
    .bind(id, device, network, total, now, since, SHARE_PER_DEVICE, SHARE_PER_NETWORK)
    .run();
  if (claimed.meta.changes === 1) return undefined;

  const used = async (column: "device" | "network", value: string) =>
    (
      await env.SHARES.prepare(`SELECT COALESCE(SUM(bytes), 0) AS used FROM shares WHERE ${column} = ?1 AND started > ?2`)
        .bind(value, since)
        .first<{ used: number }>()
    )?.used ?? 0;
  const mine = await used("device", device);
  if (mine + total > SHARE_PER_DEVICE) {
    return total > SHARE_PER_DEVICE
      ? `This recording is ${size(total)}, more than the ${size(SHARE_PER_DEVICE)} Screenilux can share for you. Share it to your own storage instead.`
      : `You have ${size(SHARE_PER_DEVICE - mine)} of your ${size(SHARE_PER_DEVICE)} left this week, and this recording is ${size(total)}. Space frees up as your links expire, 7 days after you shared them.`;
  }
  return `Recordings shared from your network have used its ${size(SHARE_PER_NETWORK)} for this week. Space frees up as those links expire, 7 days after they were shared.`;
}

function length(request: Request): number | undefined {
  const n = Number(request.headers.get("Content-Length"));
  return Number.isSafeInteger(n) && n > 0 ? n : undefined;
}

async function api(request: Request, env: Env, path: string[]): Promise<Response> {
  const [collection, id, name, uploads, uploadId, part] = path;
  if (collection !== "recordings") return problem(404, "Not found.");
  if (!env.SIGNING_KEY) return problem(503, "Sharing is not set up on this server.");
  const method = request.method;

  // POST /api/recordings with {"total": bytes, "device": id}: a new, empty
  // recording and its upload token, if the device and network have room.
  if (path.length === 1 && method === "POST") {
    const from = request.headers.get("CF-Connecting-IP") ?? "unknown";
    if (!(await env.CREATES.limit({ key: from })).success) {
      return problem(429, "Too many recordings were shared from here in the last minute. Try again in a minute.");
    }
    const asked = await request
      .json<{ total?: unknown; device?: unknown }>()
      .catch(() => ({ total: undefined, device: undefined }));
    const total = Number(asked.total);
    if (!Number.isSafeInteger(total) || total <= 0) return problem(400, "Say how big the recording is.");
    if (typeof asked.device !== "string" || !ID.test(asked.device)) return problem(400, "Say which device is sharing.");
    const id = base64url(crypto.getRandomValues(new Uint8Array(16)));
    const refused = await claim(env, id, asked.device, await networkOf(request, env), total);
    if (refused) return problem(413, refused);
    const now = Date.now();
    const state: UploadState = { status: "uploading", total, received: 0, started: now, updated: now };
    await env.RECORDINGS.put(`rec/${id}/_state`, JSON.stringify(state));
    const origin = new URL(request.url).origin;
    console.log(JSON.stringify({ message: "recording started", id, total }));
    return Response.json(
      { id, link: `${origin}/v/${id}`, token: await tokenFor(env, id), limit: total },
      { status: 201 },
    );
  }

  if (!id || !ID.test(id)) return problem(404, "No such recording.");

  // GET /api/recordings/:id: how the upload is going. Anyone with the id may
  // ask; it says nothing about what was recorded.
  if (path.length === 2 && method === "GET") {
    const stored = await env.RECORDINGS.get(`rec/${id}/_state`);
    if (!stored) return problem(404, "This recording does not exist, or it has expired.");
    const state = await stored.json<UploadState>();
    const stalled = state.status === "uploading" && Date.now() - state.updated > STALLED_AFTER_MS;
    return Response.json(
      {
        status: stalled ? "stalled" : state.status,
        received: state.received,
        total: state.total,
        started: new Date(state.started).toISOString(),
        expires: new Date(state.started + KEPT_FOR_MS).toISOString(),
      },
      { headers: { "Cache-Control": "no-store" } },
    );
  }

  if (!(await mayChange(request, env, id))) return problem(401, "This upload token is not for this recording.");
  const gone = problem(404, "This recording has been deleted, or it has expired.");

  // DELETE /api/recordings/:id: the whole recording.
  if (path.length === 2 && method === "DELETE") {
    let cursor: string | undefined;
    do {
      const list = await env.RECORDINGS.list({ prefix: `rec/${id}/`, cursor });
      if (list.objects.length) await env.RECORDINGS.delete(list.objects.map((o) => o.key));
      cursor = list.truncated ? list.cursor : undefined;
    } while (cursor);
    // Its space is free again.
    await env.SHARES.prepare("DELETE FROM shares WHERE id = ?1").bind(id).run();
    return new Response(null, { status: 204 });
  }

  // POST /api/recordings/:id/complete: every file is up.
  if (path.length === 3 && name === "complete" && method === "POST") {
    const done = await updateState(env, id, (s) => ({ ...s, status: "ready", updated: Date.now() }));
    return done === "ok" ? new Response(null, { status: 204 }) : gone;
  }

  if (!name || !WRITABLE.test(name)) return problem(400, "That file is not part of a recording.");
  const key = `rec/${id}/${name}`;
  // Every file is encrypted; its type says nothing more.
  const httpMetadata = { contentType: "application/octet-stream" };
  const bytes = length(request);

  // Writing a body: its size must be stated and small enough, and it counts
  // against what the recording asked for.
  const admit = async (): Promise<Response | undefined> => {
    if (!request.body || !bytes) return problem(411, "The upload must say how big it is.");
    if (bytes > MAX_REQUEST) return problem(413, "That upload is too big for one request.");
    const late = name === "text.json";
    if (late && bytes > MAX_TEXT) return problem(413, "The text index is too big.");
    // The app tries a request again when its connection drops, and the bytes
    // of the try that failed were already counted: count each file or part
    // once, at the size it was last sent.
    const slot = part ? `${name}/${part}` : name;
    const counted = await updateState(env, id, (s) => {
      const before = s.written?.[slot] ?? 0;
      const written = { ...s.written, [slot]: bytes };
      const received = s.received - before + bytes;
      if (late) return { ...s, total: s.total - before + bytes, received, written, updated: Date.now() };
      return received > s.total ? "full" : { ...s, received, written, updated: Date.now() };
    });
    if (counted === "full") return problem(413, "The recording is bigger than it said it would be.");
    return counted === "ok" ? undefined : gone;
  };

  // PUT /api/recordings/:id/:name: a whole file, up to MAX_REQUEST.
  if (path.length === 3 && method === "PUT") {
    const refused = await admit();
    if (refused) return refused;
    await env.RECORDINGS.put(key, request.body, { httpMetadata });
    return new Response(null, { status: 204 });
  }

  // Larger files go up in parts.
  if (uploads !== "uploads") return problem(404, "Not found.");

  // POST /api/recordings/:id/:name/uploads: start one.
  if (path.length === 4 && method === "POST") {
    const upload = await env.RECORDINGS.createMultipartUpload(key, { httpMetadata });
    return Response.json({ uploadId: upload.uploadId }, { status: 201 });
  }

  if (!uploadId) return problem(404, "Not found.");
  const upload = env.RECORDINGS.resumeMultipartUpload(key, uploadId);

  // PUT /api/recordings/:id/:name/uploads/:uploadId/:part: one part.
  if (path.length === 6 && method === "PUT") {
    if (!part || !PART.test(part)) return problem(400, "That is not a valid part.");
    const refused = await admit();
    if (refused) return refused;
    const done = await upload.uploadPart(Number(part), request.body!);
    return Response.json(done);
  }

  // POST /api/recordings/:id/:name/uploads/:uploadId: finish, with the list
  // of parts.
  if (path.length === 5 && method === "POST") {
    const { parts } = await request.json<{ parts: R2UploadedPart[] }>();
    await upload.complete(parts);
    return new Response(null, { status: 204 });
  }

  // DELETE /api/recordings/:id/:name/uploads/:uploadId: give up.
  if (path.length === 5 && method === "DELETE") {
    await upload.abort();
    return new Response(null, { status: 204 });
  }

  return problem(405, "That method is not allowed here.");
}

/** GET or HEAD /r/:id/:name: one encrypted file of a shared recording. */
async function read(request: Request, env: Env, path: string[]): Promise<Response> {
  const [id, name] = path;
  if (path.length !== 2 || !id || !ID.test(id) || !name || !NAME.test(name)) return problem(404, "Not found.");
  if (request.method !== "GET" && request.method !== "HEAD") return problem(405, "That method is not allowed here.");
  const key = `rec/${id}/${name}`;

  // Files never change once written, so they can be cached for long, and
  // conditional requests from a browser's cache are simply answered in
  // full: only the range matters.
  const headers = new Headers({ "Accept-Ranges": "bytes", "Cache-Control": "public, max-age=86400, immutable" });
  const head = await env.RECORDINGS.head(key);
  if (!head) return problem(404, "This recording does not exist, or it has expired.");
  head.writeHttpMetadata(headers);
  headers.set("ETag", head.httpEtag);
  if (request.method === "HEAD") {
    headers.set("Content-Length", String(head.size));
    return new Response(null, { headers });
  }

  const asked = request.headers.get("Range");
  if (asked) {
    const range = byteRange(asked, head.size);
    if (!range) {
      headers.set("Content-Range", `bytes */${head.size}`);
      return new Response(null, { status: 416, headers });
    }
    const object = await env.RECORDINGS.get(key, { range: { offset: range.start, length: range.end - range.start + 1 } });
    if (!object) return problem(404, "This recording does not exist, or it has expired.");
    headers.set("Content-Range", `bytes ${range.start}-${range.end}/${head.size}`);
    headers.set("Content-Length", String(range.end - range.start + 1));
    return new Response(object.body, { status: 206, headers });
  }
  const object = await env.RECORDINGS.get(key);
  if (!object) return problem(404, "This recording does not exist, or it has expired.");
  headers.set("Content-Length", String(object.size));
  return new Response(object.body, { headers });
}

/** A single `bytes=` range within a file of `size` bytes, or undefined if it is not one. */
function byteRange(header: string, size: number): { start: number; end: number } | undefined {
  const m = /^bytes=(\d*)-(\d*)$/.exec(header.trim());
  if (!m || (m[1] === "" && m[2] === "")) return undefined;
  let start: number;
  let end: number;
  if (m[1] === "") {
    start = Math.max(0, size - Number(m[2]));
    end = size - 1;
  } else {
    start = Number(m[1]);
    end = m[2] === "" ? size - 1 : Math.min(Number(m[2]), size - 1);
  }
  return start <= end && start < size ? { start, end } : undefined;
}

function problem(status: number, message: string): Response {
  return Response.json({ error: message }, { status, headers: { "Cache-Control": "no-store" } });
}
