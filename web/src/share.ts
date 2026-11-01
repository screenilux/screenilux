// What a shared recording is made of, and how to read one.
//
// A share is a folder of plain files at some address: on the Screenilux
// storage (/r/<id>/) or in someone's own bucket. The desktop app writes
// share.json there first, uploads the rest, then rewrites share.json as
// ready. The formats of recording.json and text.json are the desktop app's
// own (crates/core/src/session.rs and text.rs).

export interface Share {
  format: "screenilux-share";
  version: 1;
  status: "uploading" | "ready" | "failed";
  title: string;
  /** When it was recorded, as an RFC 3339 time. */
  created: string;
  /** When the link stops working, if it does. */
  expires?: string;
  /** Bytes uploaded so far, of how many. */
  uploaded: number;
  total: number;
  /** Whether text.json is there. */
  text: boolean;
}

export interface Screen {
  file: string;
  label: string;
  width: number;
  height: number;
  left: number;
  top: number;
  primary: boolean;
  audio: boolean;
  order: number;
}

export interface Manifest {
  format: "screenrec-session";
  duration: number;
  fps: number;
  screens: Screen[];
  focus?: { timeline: { t: number; screen: string }[] };
}

export interface Word {
  text: string;
  x: number;
  y: number;
  w: number;
  h: number;
}

export interface Entry {
  screen: string;
  t: number;
  until: number;
  page: { lines: { words: Word[] }[] };
}

export interface TextIndex {
  format: "screenilux-text";
  interval: number;
  entries: Entry[];
}

export interface Hit {
  screen: string;
  t: number;
  snippet: string;
}

export class Missing extends Error {}

async function json<T>(url: string, init?: RequestInit): Promise<T> {
  const res = await fetch(url, init);
  if (res.status === 404 || res.status === 403) throw new Missing(url);
  if (!res.ok) throw new Error(`${url}: ${res.status}`);
  return (await res.json()) as T;
}

export function readShare(base: string): Promise<Share> {
  return json<Share>(`${base}share.json`, { cache: "no-store" });
}

export function readManifest(base: string): Promise<Manifest> {
  return json<Manifest>(`${base}recording.json`);
}

export async function readText(base: string): Promise<TextIndex | undefined> {
  try {
    return await json<TextIndex>(`${base}text.json`);
  } catch {
    return undefined;
  }
}

/** Screens in the order the desktop app shows them: the primary first. */
export function byOrder(m: Manifest): Screen[] {
  return [...m.screens].sort((a, b) => a.order - b.order);
}

/** Which screen the pointer was on at `t`. */
export function focusAt(m: Manifest, t: number): string | undefined {
  let current: string | undefined;
  for (const e of m.focus?.timeline ?? []) {
    if (e.t > t) break;
    current = e.screen;
  }
  return current;
}

export function pageText(entry: Entry): string {
  return entry.page.lines
    .filter((l) => l.words.length)
    .map((l) => l.words.map((w) => w.text).join(" "))
    .join("\n");
}

/** The text on `screen` at `t`, if the index has it. */
export function pageAt(index: TextIndex, screen: string, t: number): Entry | undefined {
  let found: Entry | undefined;
  for (const e of index.entries) {
    if (e.screen === screen && e.t <= t && t < e.until + index.interval) {
      if (!found || e.t > found.t) found = e;
    }
  }
  return found;
}

/**
 * Every appearance of `query`, ignoring case, in time order. A line that
 * stays on screen across samples counts once, where it began: the same rule
 * as the desktop app's search.
 */
export function search(index: TextIndex, query: string): Hit[] {
  const needle = query.trim().toLowerCase();
  if (!needle) return [];
  const previous = new Map<string, string | undefined>();
  const hits: Hit[] = [];
  for (const e of [...index.entries].sort((a, b) => a.t - b.t)) {
    const text = pageText(e);
    const lower = text.toLowerCase();
    const at = lower.indexOf(needle);
    let snippet: string | undefined;
    if (at >= 0) {
      const start = lower.lastIndexOf("\n", at) + 1;
      const end = lower.indexOf("\n", at);
      snippet = text.slice(start, end < 0 ? undefined : end).trim();
    }
    const before = previous.get(e.screen);
    previous.set(e.screen, snippet);
    if (snippet !== undefined && snippet !== before) hits.push({ screen: e.screen, t: e.t, snippet });
  }
  return hits;
}

export function clock(seconds: number): string {
  const s = Math.max(0, Math.floor(seconds));
  const h = Math.floor(s / 3600);
  const m = Math.floor((s % 3600) / 60);
  const ss = String(s % 60).padStart(2, "0");
  return h ? `${h}:${String(m).padStart(2, "0")}:${ss}` : `${m}:${ss}`;
}
