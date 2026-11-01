import { useEffect, useState } from "react";
import { Player } from "./Player";
import { type Manifest, Missing, readManifest, readShare, readText, type Share, type TextIndex } from "./share";

// How often a page waiting for an upload checks on it.
const POLL_MS = 3000;

/**
 * Where the shared recording's files are, from the address:
 * /v/<id> on the Screenilux storage, or /b#<address> in someone's own bucket.
 */
function locate(): string | undefined {
  const { pathname, hash, origin } = window.location;
  const hosted = /^\/v\/([A-Za-z0-9_-]{22})\/?$/.exec(pathname);
  if (hosted) return `${origin}/r/${hosted[1]}/`;
  if (pathname === "/b" && hash.length > 1) {
    try {
      const url = new URL(decodeURIComponent(hash.slice(1)));
      // Plain http only for a bucket on this computer, while testing.
      const local = url.hostname === "localhost" || url.hostname === "127.0.0.1";
      if (url.protocol !== "https:" && !local) return undefined;
      return url.href.endsWith("/") ? url.href : `${url.href}/`;
    } catch {
      return undefined;
    }
  }
  return undefined;
}

export function App() {
  const [base, setBase] = useState(locate);
  // A bucket link differs only after the #, which does not load a new page.
  useEffect(() => {
    const onHash = () => setBase(locate());
    window.addEventListener("hashchange", onHash);
    return () => window.removeEventListener("hashchange", onHash);
  }, []);
  return (
    <div className="flex min-h-dvh flex-col">
      <main className="mx-auto flex w-full max-w-[1600px] flex-1 flex-col px-4 pt-4 pb-6 sm:px-6">
        {base ? <Shared key={base} base={base} /> : <Home />}
      </main>
    </div>
  );
}

type State =
  | { kind: "loading" }
  | { kind: "missing" }
  | { kind: "error" }
  | { kind: "waiting"; share: Share }
  | { kind: "failed"; share: Share }
  | { kind: "ready"; share: Share; manifest: Manifest; text?: TextIndex };

function Shared({ base }: { base: string }) {
  const [state, setState] = useState<State>({ kind: "loading" });

  useEffect(() => {
    let stopped = false;
    let timer = 0;
    const check = async () => {
      try {
        const share = await readShare(base);
        if (stopped) return;
        if (share.status === "uploading") {
          setState({ kind: "waiting", share });
          timer = window.setTimeout(check, POLL_MS);
        } else if (share.status === "failed") {
          setState({ kind: "failed", share });
        } else {
          const [manifest, text] = await Promise.all([readManifest(base), share.text ? readText(base) : undefined]);
          if (!stopped) setState({ kind: "ready", share, manifest, text });
        }
      } catch (e) {
        if (!stopped) setState({ kind: e instanceof Missing ? "missing" : "error" });
      }
    };
    void check();
    return () => {
      stopped = true;
      window.clearTimeout(timer);
    };
  }, [base]);

  useEffect(() => {
    if ("share" in state) document.title = `${state.share.title} · Screenilux`;
  }, [state]);

  switch (state.kind) {
    case "loading":
      return <Notice title="Opening the recording…" />;
    case "missing":
      return (
        <Notice title="This recording isn't here">
          The link may be mistyped, or the recording has expired. Recordings shared on Screenilux last 7 days.
        </Notice>
      );
    case "error":
      return (
        <Notice title="The recording couldn't be opened">
          Check your connection and reload the page. If it keeps happening, ask whoever sent the link to share it again.
        </Notice>
      );
    case "failed":
      return (
        <Notice title={state.share.title}>
          The upload didn't finish. Ask whoever sent the link to share the recording again.
        </Notice>
      );
    case "waiting": {
      const done = state.share.total ? Math.floor((state.share.uploaded / state.share.total) * 100) : 0;
      return (
        <Notice title="Still uploading">
          <span className="block">
            {state.share.title} will play here as soon as it's uploaded. Keep this page open.
          </span>
          <span className="mt-5 block h-1.5 overflow-hidden rounded-full bg-mist" role="progressbar" aria-valuenow={done} aria-valuemin={0} aria-valuemax={100}>
            <span className="block h-full rounded-full bg-signal transition-[width] duration-500" style={{ width: `${done}%` }} />
          </span>
          <span className="mt-2 block text-sm tabular-nums">{done}% uploaded</span>
        </Notice>
      );
    }
    case "ready":
      return (
        <>
          <Title share={state.share} screens={state.manifest.screens.length} />
          <Player base={base} manifest={state.manifest} text={state.text} />
        </>
      );
  }
}

function Title({ share, screens }: { share: Share; screens: number }) {
  const when = (iso: string) =>
    new Date(iso).toLocaleString(undefined, { day: "numeric", month: "long", hour: "numeric", minute: "2-digit" });
  return (
    <header className="mb-4 flex flex-wrap items-center gap-x-4 gap-y-1">
      <a href="https://github.com/screenilux/screenilux" aria-label="Screenilux" className="shrink-0">
        <Logo size={32} />
      </a>
      <div className="min-w-0 flex-1">
        <h1 className="truncate text-lg font-semibold">{share.title}</h1>
        <p className="text-sm text-weak">
          {screens > 1 ? `${screens} screens` : "1 screen"} · Recorded {when(share.created)}
          {share.expires && ` · Available until ${when(share.expires)}`}
        </p>
      </div>
    </header>
  );
}

function Notice({ title, children }: { title: string; children?: React.ReactNode }) {
  return (
    <div className="grid flex-1 place-items-center">
      <div className="w-full max-w-md rounded-[20px] border border-mist bg-surface p-8 text-center">
        <div className="mb-5 flex justify-center">
          <Logo size={48} />
        </div>
        <h1 className="text-xl font-semibold">{title}</h1>
        {children && <div className="mt-2 text-weak">{children}</div>}
      </div>
    </div>
  );
}

function Home() {
  return (
    <Notice title="Screenilux">
      <span className="block">
        Record every screen at once, play them back together, and copy any text you saw. Links to recordings shared
        from the Screenilux app open here.
      </span>
      <a
        href="https://github.com/screenilux/screenilux"
        className="mt-6 inline-flex h-10 items-center rounded-full bg-action px-5 font-semibold text-on-dark hover:bg-signal"
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
