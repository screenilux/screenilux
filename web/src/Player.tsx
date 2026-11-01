import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { byOrder, clock, focusAt, type Hit, type Manifest, pageAt, type Screen, search, type TextIndex } from "./share";

type Layout = "follow" | "side" | "stack";

const LAYOUTS: { id: Layout; label: string }[] = [
  { id: "follow", label: "Follow pointer" },
  { id: "side", label: "Side by side" },
  { id: "stack", label: "Stacked" },
];
const RATES = [1, 1.5, 2];
const GAP = 12;
// How far a screen may drift from the one carrying the sound before it is
// pulled back. Smaller makes playback stutter on slow connections.
const DRIFT = 0.2;

interface Props {
  base: string;
  manifest: Manifest;
  text?: TextIndex;
}

/** Every screen of a recording, played together in step. */
export function Player({ base, manifest, text }: Props) {
  const screens = useMemo(() => byOrder(manifest), [manifest]);
  const lead = screens.find((s) => s.audio) ?? screens[0]!;
  const many = screens.length > 1;

  const videos = useRef(new Map<string, HTMLVideoElement>());
  const shell = useRef<HTMLDivElement>(null);
  const stage = useRef<HTMLDivElement>(null);

  const [playing, setPlaying] = useState(false);
  const [time, setTime] = useState(0);
  const [duration, setDuration] = useState(manifest.duration);
  const [layout, setLayout] = useState<Layout>("follow");
  // A screen picked from a search result, shown until playback resumes.
  const [picked, setPicked] = useState<string>();
  const [muted, setMuted] = useState(false);
  const [rate, setRate] = useState(1);
  const [full, setFull] = useState(false);
  const [idle, setIdle] = useState(false);
  const [size, setSize] = useState({ w: 0, h: 0 });
  const [query, setQuery] = useState("");

  const shown = picked ?? focusAt(manifest, time) ?? lead.file;
  const visible = layout === "follow" || !many ? screens.filter((s) => s.file === (many ? shown : lead.file)) : screens;
  const hits: Hit[] = useMemo(() => (text ? search(text, query) : []), [text, query]);

  // Keep every screen at the lead's time while playing.
  useEffect(() => {
    let frame = 0;
    const tick = () => {
      const master = videos.current.get(lead.file);
      if (master) {
        setTime((t) => (Math.abs(t - master.currentTime) > 0.04 ? master.currentTime : t));
        if (!master.paused) {
          for (const [file, v] of videos.current) {
            if (file !== lead.file && Math.abs(v.currentTime - master.currentTime) > DRIFT) {
              v.currentTime = master.currentTime;
            }
          }
        }
      }
      frame = requestAnimationFrame(tick);
    };
    frame = requestAnimationFrame(tick);
    return () => cancelAnimationFrame(frame);
  }, [lead.file]);

  const all = () => [...videos.current.values()];

  const play = useCallback(async () => {
    setPicked(undefined);
    const master = videos.current.get(lead.file);
    if (!master) return;
    if (master.ended) for (const v of all()) v.currentTime = 0;
    try {
      await Promise.all(all().map((v) => v.play()));
      setPlaying(true);
    } catch {
      for (const v of all()) v.pause();
      setPlaying(false);
    }
  }, [lead.file]);

  const pause = useCallback(() => {
    for (const v of all()) v.pause();
    setPlaying(false);
  }, []);

  const seek = useCallback(
    (t: number) => {
      const to = Math.min(Math.max(0, t), duration);
      for (const v of all()) v.currentTime = to;
      setTime(to);
    },
    [duration],
  );

  const toggleFull = useCallback(() => {
    if (document.fullscreenElement) void document.exitFullscreen();
    else void shell.current?.requestFullscreen();
  }, []);

  useEffect(() => {
    const onChange = () => setFull(document.fullscreenElement === shell.current);
    document.addEventListener("fullscreenchange", onChange);
    return () => document.removeEventListener("fullscreenchange", onChange);
  }, []);

  // Space or K plays and pauses, F goes full screen, arrows skip 5 seconds.
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.target instanceof HTMLInputElement || e.ctrlKey || e.metaKey || e.altKey) return;
      if (e.key === " " || e.key === "k") {
        e.preventDefault();
        if (playing) pause();
        else void play();
      } else if (e.key === "f") toggleFull();
      else if (e.key === "ArrowRight") seek(time + 5);
      else if (e.key === "ArrowLeft") seek(time - 5);
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [playing, play, pause, seek, time, toggleFull]);

  // In full screen the controls get out of the way while it plays.
  useEffect(() => {
    if (!full || !playing) {
      setIdle(false);
      return;
    }
    let timer = window.setTimeout(() => setIdle(true), 2500);
    const wake = () => {
      setIdle(false);
      window.clearTimeout(timer);
      timer = window.setTimeout(() => setIdle(true), 2500);
    };
    window.addEventListener("pointermove", wake);
    return () => {
      window.clearTimeout(timer);
      window.removeEventListener("pointermove", wake);
    };
  }, [full, playing]);

  useEffect(() => {
    const el = stage.current;
    if (!el) return;
    const observer = new ResizeObserver(([entry]) => {
      if (entry) setSize({ w: entry.contentRect.width, h: entry.contentRect.height });
    });
    observer.observe(el);
    return () => observer.disconnect();
  }, []);

  useEffect(() => {
    for (const [file, v] of videos.current) {
      v.muted = muted || file !== lead.file;
      v.playbackRate = rate;
    }
  }, [muted, rate, lead.file]);

  // The scale that fits the visible screens into the stage, laid out as asked.
  const arranged = layout === "stack" && visible.length > 1 ? "stack" : "side";
  const scale = (() => {
    if (!size.w || !size.h) return 0;
    const gaps = GAP * (visible.length - 1);
    if (arranged === "stack") {
      return Math.min(size.w / Math.max(...visible.map((s) => s.width)), (size.h - gaps) / sum(visible.map((s) => s.height)));
    }
    return Math.min((size.w - gaps) / sum(visible.map((s) => s.width)), size.h / Math.max(...visible.map((s) => s.height)));
  })();

  const onStageClick = (e: React.MouseEvent) => {
    if ((e.target as HTMLElement).closest(".word") || window.getSelection()?.toString()) return;
    if (playing) pause();
    else void play();
  };

  const pick = (hit: Hit) => {
    pause();
    seek(hit.t);
    setLayout("follow");
    setPicked(hit.screen);
  };

  const controls = (
    <Controls
      playing={playing}
      time={time}
      duration={duration}
      muted={muted}
      rate={rate}
      layout={many ? layout : undefined}
      full={full}
      onPlay={() => (playing ? pause() : void play())}
      onSeek={seek}
      onMute={() => setMuted((m) => !m)}
      onRate={() => setRate((r) => RATES[(RATES.indexOf(r) + 1) % RATES.length]!)}
      onLayout={(l) => {
        setLayout(l);
        setPicked(undefined);
      }}
      onFull={toggleFull}
    />
  );

  return (
    <div className="flex min-h-0 flex-1 flex-col gap-4 lg:flex-row">
      <div
        ref={shell}
        className={
          full
            ? "relative flex h-full w-full flex-col bg-black"
            : "flex min-h-[320px] flex-1 flex-col overflow-hidden rounded-[20px] border border-mist bg-surface"
        }
        style={full && idle ? { cursor: "none" } : undefined}
      >
        <div
          ref={stage}
          className={`relative min-h-0 flex-1 ${full ? "" : "m-3 mb-0 rounded-[12px] bg-stage"}`}
          onClick={onStageClick}
          onDoubleClick={(e) => {
            if (!(e.target as HTMLElement).closest(".word")) toggleFull();
          }}
        >
          <div
            className={`absolute inset-0 flex items-center justify-center ${arranged === "stack" ? "flex-col" : "flex-row"}`}
            style={{ gap: GAP }}
          >
            {screens.map((s) => {
              const on = visible.includes(s);
              // Laid out as the screens stood on the desk, left to right or
              // top to bottom.
              const place = arranged === "stack" ? s.top : s.left;
              return (
                <ScreenView
                  key={s.file}
                  screen={s}
                  place={place}
                  src={`${base}${s.file}`}
                  width={on ? s.width * scale : 1}
                  height={on ? s.height * scale : 1}
                  hidden={!on}
                  label={many ? s.label : undefined}
                  text={!playing && on && text ? pageAt(text, s.file, time) : undefined}
                  register={(v) => {
                    if (v) videos.current.set(s.file, v);
                    else videos.current.delete(s.file);
                  }}
                  onDuration={s.file === lead.file ? setDuration : undefined}
                  onEnded={s.file === lead.file ? () => setPlaying(false) : undefined}
                />
              );
            })}
          </div>
        </div>
        {full ? (
          <div
            className={`absolute inset-x-0 bottom-0 p-4 transition-opacity duration-300 ${idle ? "pointer-events-none opacity-0" : "opacity-100"}`}
          >
            <div className="mx-auto max-w-4xl rounded-[16px] border border-mist bg-ground/95">{controls}</div>
          </div>
        ) : (
          controls
        )}
      </div>

      {text && !full && (
        <aside className="flex max-h-[60vh] w-full flex-col rounded-[20px] border border-mist bg-surface p-3 lg:max-h-none lg:w-80">
          <label className="mb-2 px-1 text-sm font-semibold" htmlFor="search">
            Search the recording
          </label>
          <input
            id="search"
            type="search"
            value={query}
            onChange={(e) => setQuery(e.target.value)}
            placeholder="A word you saw on screen"
            className="h-10 rounded-[10px] border border-mist bg-ground px-3 text-sm placeholder:text-weak focus:border-signal focus:outline-none"
          />
          <div className="mt-2 min-h-0 flex-1 overflow-y-auto">
            {query.trim() && !hits.length && <p className="px-1 py-3 text-sm text-weak">Not found in this recording.</p>}
            {!query.trim() && (
              <p className="px-1 py-3 text-sm text-weak">
                Pause anywhere to select and copy the text on screen, or search for a word to jump to it.
              </p>
            )}
            <ul>
              {hits.map((h) => (
                <li key={`${h.screen}-${h.t}`}>
                  <button
                    type="button"
                    onClick={() => pick(h)}
                    className="w-full rounded-[10px] px-2 py-2 text-left hover:bg-ground"
                  >
                    <span className="block text-xs text-weak">
                      {clock(h.t)}
                      {many && ` · ${screens.find((s) => s.file === h.screen)?.label ?? h.screen}`}
                    </span>
                    <span className="block truncate text-sm">{h.snippet}</span>
                  </button>
                </li>
              ))}
            </ul>
          </div>
        </aside>
      )}
    </div>
  );
}

interface ScreenProps {
  screen: Screen;
  place: number;
  src: string;
  width: number;
  height: number;
  hidden: boolean;
  label?: string;
  text?: ReturnType<typeof pageAt>;
  register: (v: HTMLVideoElement | null) => void;
  onDuration?: (d: number) => void;
  onEnded?: () => void;
}

function ScreenView({ screen, place, src, width, height, hidden, label, text, register, onDuration, onEnded }: ScreenProps) {
  return (
    <div
      className={hidden ? "pointer-events-none absolute opacity-0" : "relative shrink-0 overflow-hidden rounded-[6px] bg-black"}
      style={{ width, height, order: place, containerType: "size" }}
    >
      <video
        ref={register}
        src={src}
        preload="auto"
        playsInline
        className="h-full w-full"
        onLoadedMetadata={(e) => onDuration?.(e.currentTarget.duration)}
        onEnded={onEnded}
      />
      {text && (
        <div className="absolute inset-0" aria-label="Text on screen">
          {text.page.lines.map((line, li) => (
            <div key={li} style={{ fontSize: 0 }}>
              {line.words.map((w, wi) => (
                <span key={wi}>
                  {wi > 0 && " "}
                  <span
                    className="word"
                    style={{
                      left: `${(w.x / screen.width) * 100}%`,
                      top: `${(w.y / screen.height) * 100}%`,
                      width: `${(w.w / screen.width) * 100}%`,
                      height: `${(w.h / screen.height) * 100}%`,
                      fontSize: `${(w.h / screen.height) * 90}cqh`,
                    }}
                  >
                    {w.text}
                  </span>
                </span>
              ))}
            </div>
          ))}
        </div>
      )}
      {label && !hidden && (
        <span className="pointer-events-none absolute top-2 left-2 rounded-full bg-ink/70 px-2 py-0.5 text-xs text-on-dark">
          {label}
        </span>
      )}
    </div>
  );
}

interface ControlsProps {
  playing: boolean;
  time: number;
  duration: number;
  muted: boolean;
  rate: number;
  layout?: Layout;
  full: boolean;
  onPlay: () => void;
  onSeek: (t: number) => void;
  onMute: () => void;
  onRate: () => void;
  onLayout: (l: Layout) => void;
  onFull: () => void;
}

function Controls(p: ControlsProps) {
  const done = p.duration ? (p.time / p.duration) * 100 : 0;
  return (
    <div className="flex flex-col gap-1 px-4 pt-2 pb-3">
      <input
        type="range"
        className="scrub w-full"
        aria-label="Position"
        min={0}
        max={p.duration || 0}
        step={0.1}
        value={Math.min(p.time, p.duration)}
        onChange={(e) => p.onSeek(Number(e.target.value))}
        style={{ "--done": `${done}%` } as React.CSSProperties}
      />
      <div className="flex flex-wrap items-center gap-2">
        <button
          type="button"
          onClick={p.onPlay}
          aria-label={p.playing ? "Pause" : "Play"}
          className="grid size-10 place-items-center rounded-full bg-action text-on-dark hover:bg-signal"
        >
          {p.playing ? <PauseIcon /> : <PlayIcon />}
        </button>
        <span className="min-w-24 text-sm tabular-nums">
          {clock(p.time)} <span className="text-weak">/ {clock(p.duration)}</span>
        </span>
        <div className="ml-auto flex flex-wrap items-center gap-2">
          {p.layout && (
            <div className="flex rounded-full border border-mist p-0.5" role="group" aria-label="Layout">
              {LAYOUTS.map((l) => (
                <button
                  key={l.id}
                  type="button"
                  aria-pressed={p.layout === l.id}
                  onClick={() => p.onLayout(l.id)}
                  className={`rounded-full px-3 py-1 text-xs font-semibold ${p.layout === l.id ? "bg-ink text-ground" : "text-weak hover:text-ink"}`}
                >
                  {l.label}
                </button>
              ))}
            </div>
          )}
          <button type="button" onClick={p.onRate} className="h-8 min-w-12 rounded-full border border-mist px-2 text-xs font-semibold tabular-nums hover:border-signal">
            {p.rate}×
          </button>
          <IconButton label={p.muted ? "Turn sound on" : "Mute"} onClick={p.onMute}>
            {p.muted ? <MutedIcon /> : <SoundIcon />}
          </IconButton>
          <IconButton label={p.full ? "Leave full screen" : "Full screen"} onClick={p.onFull}>
            <FullIcon out={p.full} />
          </IconButton>
        </div>
      </div>
    </div>
  );
}

function IconButton({ label, onClick, children }: { label: string; onClick: () => void; children: React.ReactNode }) {
  return (
    <button
      type="button"
      onClick={onClick}
      aria-label={label}
      title={label}
      className="grid size-8 place-items-center rounded-full border border-mist hover:border-signal"
    >
      {children}
    </button>
  );
}

const icon = { width: 16, height: 16, viewBox: "0 0 16 16", fill: "currentColor", "aria-hidden": true } as const;

function PlayIcon() {
  return (
    <svg {...icon}>
      <path d="M4.5 2.8v10.4c0 .6.6.9 1.1.6l8-5.2c.5-.3.5-.9 0-1.2l-8-5.2c-.5-.3-1.1 0-1.1.6Z" />
    </svg>
  );
}

function PauseIcon() {
  return (
    <svg {...icon}>
      <rect x="3.5" y="2.5" width="3" height="11" rx="1" />
      <rect x="9.5" y="2.5" width="3" height="11" rx="1" />
    </svg>
  );
}

function SoundIcon() {
  return (
    <svg {...icon}>
      <path d="M2 6h2.5L8 3v10l-3.5-3H2z" />
      <path d="M10.5 5.5a3.5 3.5 0 0 1 0 5" fill="none" stroke="currentColor" strokeWidth="1.4" strokeLinecap="round" />
      <path d="M12.3 3.6a6 6 0 0 1 0 8.8" fill="none" stroke="currentColor" strokeWidth="1.4" strokeLinecap="round" />
    </svg>
  );
}

function MutedIcon() {
  return (
    <svg {...icon}>
      <path d="M2 6h2.5L8 3v10l-3.5-3H2z" />
      <path d="m10.5 6 4 4m0-4-4 4" fill="none" stroke="currentColor" strokeWidth="1.4" strokeLinecap="round" />
    </svg>
  );
}

function FullIcon({ out }: { out: boolean }) {
  const d = out ? "M6 2v4H2M10 2v4h4M6 14v-4H2M10 14v-4h4" : "M2 6V2h4M14 6V2h-4M2 10v4h4M14 10v4h-4";
  return (
    <svg {...icon} fill="none" stroke="currentColor" strokeWidth="1.5" strokeLinecap="round" strokeLinejoin="round">
      <path d={d} />
    </svg>
  );
}

function sum(xs: number[]): number {
  return xs.reduce((a, b) => a + b, 0);
}
