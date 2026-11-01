// Share links: the desktop app uploads a recording here, and the web player
// reads it back.
//
// A shared recording is a folder of plain files under rec/<id>/ in R2:
//
//   share.json      written first and last by the app: whether the upload is
//                   still going, and what the recording is (see web/src/share.ts)
//   recording.json  the manifest
//   screen1.mp4 ... one video per screen
//   text.json       the text index, if the recording was made searchable
//   _usage          how many bytes the recording has used; never served
//
// Anyone with the app can share, so nothing secret is in the app. Instead:
//
// - Starting a recording needs nothing, but each address can start only a few
//   a minute.
// - Starting one hands back an upload token that works for that recording
//   only: it cannot read, list or change any other. The token is a signature
//   of the recording's id, so the server keeps nothing to check it against.
// - A recording can hold at most MAX_RECORDING bytes.
// - Reading needs only the id, which is random and long enough that links
//   cannot be guessed. R2 deletes everything a week after it was uploaded (a
//   lifecycle rule on the bucket).
// - The admin key (UPLOAD_KEY) can change or delete any recording, to take
//   one down.

const ID = /^[A-Za-z0-9_-]{22}$/;
// Any plain file name can be read, but only the files a recording is made of
// can be written.
const NAME = /^[A-Za-z0-9][A-Za-z0-9._-]{0,99}$/;
const WRITABLE = /^(share|recording|text)\.json$|^screen[1-9][0-9]?\.mp4$/;
const PART = /^[1-9][0-9]{0,4}$/;

/** The most one shared recording can hold: about an hour of two screens. */
const MAX_RECORDING = 4 * 1024 ** 3;
/** The most one upload request can carry. The app sends 32MB parts. */
const MAX_REQUEST = 50 * 1024 ** 2;
/** share.json is rewritten as the upload goes, so it is not counted, only capped. */
const MAX_SHARE_DOC = 16 * 1024;

const TYPES: Record<string, string> = {
  json: "application/json",
  mp4: "video/mp4",
};

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
 * Count `bytes` against a recording's allowance before they are stored.
 * Several requests can arrive at once, so the count is only replaced if no
 * one else changed it in between.
 */
async function reserve(env: Env, id: string, bytes: number): Promise<"ok" | "gone" | "full"> {
  const key = `rec/${id}/_usage`;
  for (let attempt = 0; attempt < 8; attempt++) {
    const current = await env.RECORDINGS.get(key);
    if (!current) return "gone";
    const used = (await current.json<{ bytes: number }>()).bytes;
    if (used + bytes > MAX_RECORDING) return "full";
    const stored = await env.RECORDINGS.put(key, JSON.stringify({ bytes: used + bytes }), {
      onlyIf: { etagMatches: current.etag },
    });
    if (stored) return "ok";
  }
  throw new Error("the usage count kept changing");
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

  // POST /api/recordings, optionally with {"total": bytes}: a new, empty
  // recording and its upload token.
  if (path.length === 1 && method === "POST") {
    const from = request.headers.get("CF-Connecting-IP") ?? "unknown";
    if (!(await env.CREATES.limit({ key: from })).success) {
      return problem(429, "Too many recordings were shared from here in the last minute. Try again in a minute.");
    }
    const asked = await request.json<{ total?: number }>().catch(() => ({ total: undefined }));
    if (typeof asked.total === "number" && asked.total > MAX_RECORDING) {
      return problem(413, `This recording is bigger than ${MAX_RECORDING / 1024 ** 3}GB, the most Screenilux can share.`);
    }
    const id = base64url(crypto.getRandomValues(new Uint8Array(16)));
    await env.RECORDINGS.put(`rec/${id}/_usage`, JSON.stringify({ bytes: 0 }));
    const origin = new URL(request.url).origin;
    console.log(JSON.stringify({ message: "recording started", id }));
    return Response.json(
      { id, link: `${origin}/v/${id}`, token: await tokenFor(env, id), limit: MAX_RECORDING },
      { status: 201 },
    );
  }

  if (!id || !ID.test(id)) return problem(404, "No such recording.");
  if (!(await mayChange(request, env, id))) return problem(401, "This upload token is not for this recording.");

  // DELETE /api/recordings/:id: the whole recording.
  if (path.length === 2 && method === "DELETE") {
    let cursor: string | undefined;
    do {
      const list = await env.RECORDINGS.list({ prefix: `rec/${id}/`, cursor });
      if (list.objects.length) await env.RECORDINGS.delete(list.objects.map((o) => o.key));
      cursor = list.truncated ? list.cursor : undefined;
    } while (cursor);
    return new Response(null, { status: 204 });
  }

  if (!name || !WRITABLE.test(name)) return problem(400, "That file is not part of a recording.");
  const key = `rec/${id}/${name}`;
  const httpMetadata = { contentType: typeOf(name) };
  const size = length(request);
  const gone = problem(404, "This recording has been deleted, or it has expired.");
  const tooBig = problem(413, `The recording is bigger than ${MAX_RECORDING / 1024 ** 3}GB, the most Screenilux can share.`);

  // Writing the body: its size must be stated and small enough, and it
  // counts against the recording's allowance (share.json aside).
  const admit = async (): Promise<Response | undefined> => {
    if (!request.body || !size) return problem(411, "The upload must say how big it is.");
    if (size > MAX_REQUEST) return problem(413, "That upload is too big for one request.");
    if (name === "share.json") {
      if (size > MAX_SHARE_DOC) return problem(413, "share.json is too big.");
      return (await env.RECORDINGS.head(`rec/${id}/_usage`)) ? undefined : gone;
    }
    const reserved = await reserve(env, id, size);
    return reserved === "ok" ? undefined : reserved === "gone" ? gone : tooBig;
  };

  // PUT /api/recordings/:id/:name: a whole file, up to MAX_REQUEST.
  if (path.length === 3 && method === "PUT") {
    const refused = await admit();
    if (refused) return refused;
    await env.RECORDINGS.put(key, request.body, { httpMetadata });
    return new Response(null, { status: 204 });
  }

  // Larger files go up in parts.
  if (uploads !== "uploads" || name === "share.json") return problem(404, "Not found.");

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

/** GET or HEAD /r/:id/:name: one file of a shared recording. */
async function read(request: Request, env: Env, path: string[]): Promise<Response> {
  const [id, name] = path;
  if (path.length !== 2 || !id || !ID.test(id) || !name || !NAME.test(name)) return problem(404, "Not found.");
  if (request.method !== "GET" && request.method !== "HEAD") return problem(405, "That method is not allowed here.");
  const key = `rec/${id}/${name}`;

  const headers = new Headers({ "Accept-Ranges": "bytes" });
  // share.json changes while the upload runs; the rest never changes once
  // written.
  headers.set("Cache-Control", name === "share.json" ? "no-store" : "public, max-age=86400, immutable");

  if (request.method === "HEAD") {
    const head = await env.RECORDINGS.head(key);
    if (!head) return problem(404, "This recording does not exist, or it has expired.");
    head.writeHttpMetadata(headers);
    headers.set("ETag", head.httpEtag);
    headers.set("Content-Length", String(head.size));
    return new Response(null, { headers });
  }

  const object = await env.RECORDINGS.get(key, { range: request.headers, onlyIf: request.headers });
  if (!object) return problem(404, "This recording does not exist, or it has expired.");
  object.writeHttpMetadata(headers);
  headers.set("ETag", object.httpEtag);
  if (!("body" in object)) return new Response(null, { status: 304, headers });

  const range = object.range as { offset?: number; length?: number; suffix?: number } | undefined;
  if (range && request.headers.has("Range")) {
    const offset = range.suffix !== undefined ? object.size - range.suffix : (range.offset ?? 0);
    const length = range.suffix ?? range.length ?? object.size - offset;
    headers.set("Content-Range", `bytes ${offset}-${offset + length - 1}/${object.size}`);
    headers.set("Content-Length", String(length));
    return new Response(object.body, { status: 206, headers });
  }
  headers.set("Content-Length", String(object.size));
  return new Response(object.body, { headers });
}

function typeOf(name: string): string {
  return TYPES[name.split(".").pop()?.toLowerCase() ?? ""] ?? "application/octet-stream";
}

function problem(status: number, message: string): Response {
  return Response.json({ error: message }, { status, headers: { "Cache-Control": "no-store" } });
}
