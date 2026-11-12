import { useEffect, useState } from "react";
import { Player } from "./Player";
import { Locked, type Manifest, Missing, readManifest, readStatus, readText, type Status, type TextIndex } from "./share";

// How often a page waiting for an upload checks on it.
const POLL_MS = 3000;
// Where the service worker finds each page's key (public/sw.js).
const KEYS = "screenilux-keys";

/** What the address says to open. The key is always after the #. */
type Link =
  | { kind: "hosted"; id: string; key: string }
  | { kind: "bucket"; files: string; key: string }
  | { kind: "incomplete" };

/**
 * /v/<id>#k=<key> on the Screenilux storage, or /b#k=<key>&u=<address> in
 * someone's own bucket.
 */
function locate(): Link | undefined {
  const { pathname, hash } = window.location;
  const params = new URLSearchParams(hash.slice(1));
  const key = params.get("k") ?? "";
  const whole = /^[A-Za-z0-9_-]{43}$/.test(key);
  const hosted = /^\/v\/([A-Za-z0-9_-]{22})\/?$/.exec(pathname);
  if (hosted) return whole ? { kind: "hosted", id: hosted[1]!, key } : { kind: "incomplete" };
  if (pathname === "/b") {
    try {
      const url = new URL(params.get("u") ?? "");
      // Plain http only for a bucket on this computer, while testing.
      const local = url.hostname === "localhost" || url.hostname === "127.0.0.1";
      if (url.protocol !== "https:" && !local) return { kind: "incomplete" };
      if (!whole) return { kind: "incomplete" };
      return { kind: "bucket", files: url.href.endsWith("/") ? url.href : `${url.href}/`, key };
    } catch {
      return { kind: "incomplete" };
    }
  }
  return undefined;
}

export function App() {
  const [link, setLink] = useState(locate);
  // Bucket links differ only after the #, which does not load a new page.
  useEffect(() => {
    const onHash = () => setLink(locate());
    window.addEventListener("hashchange", onHash);
    return () => window.removeEventListener("hashchange", onHash);
  }, []);
  if (!link) return <Home />;
  if (link.kind === "incomplete") {
    return (
      <Notice title="This link is incomplete">
        Part of the link is missing. Ask whoever sent it to send it again, and copy all of it, including everything
        after the #.
      </Notice>
    );
  }
  return <Shared key={JSON.stringify(link)} link={link} />;
}

class Unsupported extends Error {}

/**
 * Make sure the service worker that decrypts is in charge of this page. The
 * first visit has to wait for it to install; a hard reload bypasses it, so
 * the page reloads once to put it back in charge.
 */
async function decrypter(): Promise<void> {
  if (!("serviceWorker" in navigator) || !("caches" in window)) throw new Unsupported();
  await navigator.serviceWorker.register("/sw.js");
  await navigator.serviceWorker.ready;
  if (navigator.serviceWorker.controller) return;
  const claimed = new Promise<boolean>((resolve) => {
    navigator.serviceWorker.addEventListener("controllerchange", () => resolve(true), { once: true });
    window.setTimeout(() => resolve(false), 3000);
  });
  if (await claimed) return;
  if (!sessionStorage.getItem("reloaded")) {
    sessionStorage.setItem("reloaded", "1");
    window.location.reload();
    await new Promise(() => {});
  }
  throw new Unsupported();
}

/** Hand the service worker this page's key and where the files are. */
async function openSession(files: string, key: string): Promise<{ base: string; close: () => void }> {
  await decrypter();
  sessionStorage.removeItem("reloaded");
  const id = crypto.randomUUID();
  const cache = await caches.open(KEYS);
  await cache.put(`/__keys/${id}`, Response.json({ base: files, key }));
  return { base: `/__decrypt/${id}/`, close: () => void cache.delete(`/__keys/${id}`) };
}

type State =
  | { kind: "loading" }
  | { kind: "missing" }
  | { kind: "locked" }
  | { kind: "unsupported" }
  | { kind: "error" }
  | { kind: "waiting"; status?: Status }
  | { kind: "stalled" }
  | { kind: "ready"; base: string; manifest: Manifest; text?: TextIndex; status?: Status };

function Shared({ link }: { link: Exclude<Link, { kind: "incomplete" }> }) {
  const [state, setState] = useState<State>({ kind: "loading" });

  useEffect(() => {
    let stopped = false;
    let timer = 0;
    let close = () => {};
    const fail = (e: unknown) => {
      if (stopped) return;
      if (e instanceof Missing) setState({ kind: "missing" });
      else if (e instanceof Locked) setState({ kind: "locked" });
      else if (e instanceof Unsupported) setState({ kind: "unsupported" });
      else setState({ kind: "error" });
    };
    const again = (check: () => Promise<void>) => {
      timer = window.setTimeout(() => void check().catch(fail), POLL_MS);
    };
    const load = async (base: string, status?: Status) => {
      const [manifest, text] = await Promise.all([readManifest(base), readText(base)]);
      if (!stopped) setState({ kind: "ready", base, manifest, text, status });
    };

    void (async () => {
      const files = link.kind === "hosted" ? `${window.location.origin}/r/${link.id}/` : link.files;
      const session = await openSession(files, link.key);
      close = session.close;
      if (stopped) return close();

      if (link.kind === "hosted") {
        // The server counts what has arrived, so the page can show progress.
        const check = async () => {
          const status = await readStatus(link.id);
          if (stopped) return;
          if (status.status === "ready") return load(session.base, status);
          setState(status.status === "stalled" ? { kind: "stalled" } : { kind: "waiting", status });
          again(check);
        };
        await check();
      } else {
        // A bucket has no server to ask; the manifest goes up last, so it
        // being there means the upload is done.
        const check = async () => {
          try {
            await load(session.base);
          } catch (e) {
            if (!(e instanceof Missing)) throw e;
            if (stopped) return;
            setState({ kind: "waiting" });
            again(check);
          }
        };
        await check();
      }
    })().catch(fail);

    return () => {
      stopped = true;
      window.clearTimeout(timer);
      close();
    };
  }, [link]);

  const title = state.kind === "ready" ? titleOf(state.manifest) : undefined;
  useEffect(() => {
    document.title = title ? `${title} · Screenilux` : "Screenilux";
  }, [title]);

  switch (state.kind) {
    case "loading":
      return <Notice title="Opening the recording…" />;
    case "missing":
      return (
        <Notice title="This recording isn't here">
          The link may be mistyped, or the recording has been deleted.
        </Notice>
      );
    case "locked":
      return (
        <Notice title="This link doesn't open the recording">
          The part of the link after the # is the key to the recording, and it doesn't match. Ask whoever sent the link
          to send it again, and copy all of it.
        </Notice>
      );
    case "unsupported":
      return (
        <Notice title="This window can't open the recording">
          Recordings are encrypted, and this browser window can't decrypt them. Private windows in some browsers can't.
          Try a normal window of Chrome, Edge, Firefox or Safari.
        </Notice>
      );
    case "error":
      return (
        <Notice title="The recording couldn't be opened">
          Check your connection and reload the page. If it keeps happening, ask whoever sent the link to share it again.
        </Notice>
      );
    case "stalled":
      return (
        <Notice title="The upload stopped">
          This recording stopped uploading before it finished. Ask whoever sent the link to share it again.
        </Notice>
      );
    case "waiting": {
      const status = state.status;
      const done = status?.total ? Math.floor((status.received / status.total) * 100) : undefined;
      return (
        <Notice title="Still uploading">
          <span className="block">
            {link.kind === "hosted"
              ? "This recording will play here as soon as it's uploaded. Keep this page open."
              : "This recording isn't here yet. If it was just shared, it's still uploading; this page will keep checking."}
          </span>
          {done !== undefined && (
            <>
              <span
                className="mt-5 block h-1.5 overflow-hidden rounded-full bg-mist"
                role="progressbar"
                aria-valuenow={done}
                aria-valuemin={0}
                aria-valuemax={100}
              >
                <span className="block h-full rounded-full bg-soft transition-[width] duration-500" style={{ width: `${done}%` }} />
              </span>
              <span className="mt-2 block text-sm tabular-nums">{done}% uploaded</span>
            </>
          )}
        </Notice>
      );
    }
    case "ready":
      return (
        // The player is the page: a slim bar on top, the screens edge to edge.
        <div className="flex min-h-dvh flex-col lg:h-dvh">
          <Title title={title!} manifest={state.manifest} />
          <Player base={state.base} manifest={state.manifest} text={state.text} />
        </div>
      );
  }
}

function when(iso: string): string {
  return new Date(iso).toLocaleString(undefined, { day: "numeric", month: "long", hour: "numeric", minute: "2-digit" });
}

/** The title the app gave it, or failing that, when it was recorded. */
function titleOf(manifest: Manifest): string {
  return manifest.title?.trim() || `Recording, ${when(manifest.created)}`;
}

function Title({ title, manifest }: { title: string; manifest: Manifest }) {
  const screens = manifest.screens.length;
  return (
    <header className="flex shrink-0 items-center gap-3 border-b border-mist px-4 py-2 sm:px-5">
      <a href="https://github.com/screenilux/screenilux" aria-label="Screenilux" className="shrink-0">
        <Logo size={24} />
      </a>
      <div className="flex min-w-0 flex-1 flex-wrap items-baseline gap-x-3">
        <h1 className="truncate font-semibold">{title}</h1>
        <p className="truncate text-sm text-weak">
          {screens > 1 ? `${screens} screens` : "1 screen"} · Recorded {when(manifest.created)}
        </p>
      </div>
    </header>
  );
}

function Notice({ title, children }: { title: string; children?: React.ReactNode }) {
  return (
    <main className="grid min-h-dvh place-items-center px-4 py-6">
      <div className="w-full max-w-md rounded-[20px] border border-mist bg-surface p-8 text-center">
        <div className="mb-5 flex justify-center">
          <Logo size={48} />
        </div>
        <h1 className="text-xl font-semibold">{title}</h1>
        {children && <div className="mt-2 text-weak">{children}</div>}
      </div>
    </main>
  );
}

function Home() {
  return (
    <Notice title="Screenilux">
      <span className="block">
        The screen recorder for people who work across more than one screen.
      </span>
      <ul className="mt-4 space-y-1 text-left text-sm">
        <li>
          <strong className="text-ink">Every screen, in sync.</strong> All your monitors, recorded at once and played
          back together.
        </li>
        <li>
          <strong className="text-ink">Copy any text you saw.</strong> Select it on a paused frame, or search for it.
        </li>
        <li>
          <strong className="text-ink">Links only your recipients can watch.</strong> Encrypted before they leave the
          computer.
        </li>
      </ul>
      <span className="mt-4 block text-sm">Links to recordings shared from the Screenilux app open here.</span>
      <a
        href="https://github.com/screenilux/screenilux"
        className="mt-6 inline-flex h-10 items-center rounded-full bg-mist px-5 font-semibold text-ink hover:bg-soft/50"
      >
        Get Screenilux
      </a>
    </Notice>
  );
}

/** The mark from docs/logo.svg, in the page's colours. */
function Logo({ size }: { size: number }) {
  return (
    <svg width={size} height={size} viewBox="0 0 64 64" aria-hidden="true">
      <defs>
        <mask id="logo-gap">
          <rect width="64" height="64" fill="#fff" />
          <circle cx="32" cy="32" r="10.5" fill="#000" />
        </mask>
      </defs>
      <g mask="url(#logo-gap)">
        <rect x="3" y="7" width="40" height="29" rx="8" fill="var(--soft)" />
        <rect x="21" y="28" width="40" height="29" rx="8" fill="var(--ink)" />
      </g>
      <circle cx="32" cy="32" r="6.5" fill="var(--signal)" />
    </svg>
  );
}
