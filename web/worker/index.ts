// Share links: the desktop app uploads a recording here, and the web player
// reads it back.
//
// A shared recording is a folder of plain files under rec/<id>/ in R2:
//
//   share.json      written first and last by the app: whether the upload is
//                   still going, and what the recording is (see web/src/share.ts)
//   recording.json  the manifest, for a multi-screen recording
//   screen1.mp4 ... one video per screen, or a single video
//   text.json       the text index, if the recording was made searchable
//
// Writing needs the upload key. Reading needs only the id, which is random
// and long enough that links cannot be guessed. R2 deletes everything a week
// after it was uploaded (a lifecycle rule on the bucket).

const ID = /^[A-Za-z0-9_-]{22}$/;
// A plain file name, as the desktop app's packages allow: no folders, nothing
// hidden.
const NAME = /^[A-Za-z0-9][A-Za-z0-9._-]{0,99}$/;
const PART = /^[1-9][0-9]{0,4}$/;

const TYPES: Record<string, string> = {
  json: "application/json",
  mp4: "video/mp4",
};

export default {
  async fetch(request, env): Promise<Response> {
    const url = new URL(request.url);
    try {
      if (url.pathname.startsWith("/api/")) {
        if (!(await authorised(request, env))) {
          return problem(401, "The upload key is missing or wrong.");
        }
        return await api(request, env, url.pathname.slice("/api/".length).split("/"));
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

async function authorised(request: Request, env: Env): Promise<boolean> {
  const given = request.headers.get("Authorization")?.replace(/^Bearer /, "") ?? "";
  if (!env.UPLOAD_KEY) return false;
  const encoder = new TextEncoder();
  const [a, b] = await Promise.all([
    crypto.subtle.digest("SHA-256", encoder.encode(given)),
    crypto.subtle.digest("SHA-256", encoder.encode(env.UPLOAD_KEY)),
  ]);
  return crypto.subtle.timingSafeEqual(a, b);
}

/** Uploading and deleting, all behind the upload key. */
async function api(request: Request, env: Env, path: string[]): Promise<Response> {
  const [collection, id, name, uploads, uploadId, part] = path;
  if (collection !== "recordings") return problem(404, "Not found.");
  const method = request.method;

  // POST /api/recordings: a new, empty recording.
  if (path.length === 1 && method === "POST") {
    const bytes = crypto.getRandomValues(new Uint8Array(16));
    const id = btoa(String.fromCharCode(...bytes)).replace(/\+/g, "-").replace(/\//g, "_").replace(/=+$/, "");
    const origin = new URL(request.url).origin;
    return Response.json({ id, link: `${origin}/v/${id}`, files: `${origin}/r/${id}/` }, { status: 201 });
  }

  if (!ID.test(id ?? "")) return problem(404, "No such recording.");

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

  if (!name || !NAME.test(name)) return problem(400, "That is not a valid file name.");
  const key = `rec/${id}/${name}`;
  const httpMetadata = { contentType: typeOf(name) };

  // PUT /api/recordings/:id/:name: a whole file, up to the request size limit.
  if (path.length === 3 && method === "PUT") {
    if (!request.body) return problem(400, "The file is empty.");
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
    if (!PART.test(part ?? "") || !request.body) return problem(400, "That is not a valid part.");
    const done = await upload.uploadPart(Number(part), request.body);
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
  if (path.length !== 2 || !ID.test(id ?? "") || !NAME.test(name ?? "")) return problem(404, "Not found.");
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
