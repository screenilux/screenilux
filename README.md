# Screenilux

A screen recorder for Windows. Record one screen or several at once, with a
draggable camera bubble, pointer zoom and click effects.

![The Screenilux window](screenshot.png)

## Features

- **Record several screens at once.** Each gets its own file, captured in
  parallel and numbered from a shared clock, so frame *N* of one screen is the
  same instant as frame *N* of the others. Measured at 0–2 frames of skew.
- **Start / pause / resume / stop.** Paused time is excluded; the video cuts
  straight from pause to resume.
- **Mouse effects:** a cursor marker (Windows capture omits the pointer),
  `Ctrl+Alt+wheel` zoom up to 5× with `Ctrl+Alt+middle-click` to reset, a
  fading trail, and expanding rings on click.
- **Camera bubble**, draggable anywhere — including mid-recording — and burned
  into that screen's recording. The preview window is hidden from capture, so
  the recording gets exactly one copy.
- **Hardware encoding**, chosen automatically: Quick Sync, then NVENC, then
  software, with the choice shown in the window.
- **Global hotkeys**: `Ctrl+Alt+R` start/stop, `Ctrl+Alt+P` pause/resume.
  If another application already owns one, the window says so.

## Building

Needs a Rust toolchain (1.95+) and ffmpeg on `PATH`.

```
cargo run --release -p screenilux
```

## Output

One screen gives a single H.264 MP4. Several give a session folder:

```
recording_2026-10-31_12-12-49/
  recording.json
  screen1.mp4
  screen2.mp4
```

`recording.json` is the `screenrec-session` format. It records each screen's
size, its virtual-desktop position (so a player can rebuild the real monitor
arrangement, vertical offsets included), which file carries the microphone,
and a display `order` with the primary first.

Multi-screen sessions also carry a **focus timeline** for auto-switch playback:

```json
"focus": {
  "source": "pointer", "dwell": 0.6,
  "timeline": [
    { "t": 0.0,   "screen": "screen1.mp4" },
    { "t": 1.539, "screen": "screen2.mp4" }
  ]
}
```

Each entry means *from time `t` (recorded seconds), show this screen*. Switches
are debounced: a pointer crossing one monitor to reach another is not a visit,
and a switch is stamped at the moment the pointer *arrived*, not when the dwell
expired, so playback cuts at the real moment.

## How it works

```
crates/core      pure logic - geometry, pacing, focus, compositor, manifest
crates/win       Windows adapters - capture, pointer hook, camera, affinity
crates/media     ffmpeg process management - probing, segments, concat
crates/recorder  orchestration - per-screen threads, shared clock, manifest
crates/app       the egui window and the camera overlay
```

`core` depends on neither Windows, ffmpeg nor any UI toolkit, so everything
worth testing is testable without a screen, a camera or an encoder — and a
future session player can reuse it. All `unsafe` is confined to `crates/win`;
the other four are `#![forbid(unsafe_code)]`.

A few decisions that are load-bearing and look removable:

- **Pacing is pegged to the wall clock**, not a frame counter. When capture
  falls behind, the last frame is written again so playback stays real-time
  rather than running fast. A segment is also topped up before it closes, or a
  stall right before a pause would silently shorten the clip.
- **Pause closes the encoder; resume opens a new one.** A single long-lived
  ffmpeg would keep pulling microphone audio while the video was paused,
  pushing the two apart with every pause. Segments are joined afterwards.
- **`-af aresample=async=1:first_pts=0` with `-shortest`** is a pair. The
  filter pads the microphone's late start; `-shortest` is also what makes
  ffmpeg *exit*, because a DirectShow input never ends.
- **Overlays are composited after the zoom**, so the cursor, trail and rings
  keep a constant pixel size. The camera deliberately does not move or scale
  with zoom — it is a picture-in-picture pinned to the frame.
- **The encoder falls back on the first failed write**, not at spawn: ffmpeg
  reading a pipe does not open its encoder until frames arrive, so a liveness
  check after spawning always looks healthy.

Capture uses Windows Graphics Capture, which only delivers a frame when the
screen actually changes — an idle screen costs almost nothing, and unlike the
old GDI path it sees fullscreen games and hardware-overlay video.

## Testing

```
cargo test --workspace
cargo test -p screenilux-recorder -- --ignored --test-threads=1
```

The second set records real hardware for several seconds each, so it is opt-in.
It covers a single-screen recording, a pause being excluded, every screen
recorded in sync with a valid manifest, and two recordings in the same second
not overwriting each other.

## Known limitations

- **Microphone only, no system audio.** Capturing what your speakers play needs
  a loopback device Windows does not expose to ffmpeg by default.
- **Hotkeys and the pointer hook do not see events over an elevated window**
  unless Screenilux is elevated too.
- **Zoom is a crop scaled back up**, so zooming in softens the image; it does
  not re-capture at higher detail.
- Stopping takes up to ~120 ms, bounded by the capture frame timeout.

## Licence

GPL-3.0. That permits anyone to copy, modify and sell this software; what it
forbids is distributing a modified version without source under a more
restrictive licence. It blocks closed-source forks, not commercial use.

`legacy/` holds the original Python build, kept for comparison.
