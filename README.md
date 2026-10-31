# Screen Recorder

A small Windows screen recorder with a tkinter GUI. Pick a display, hit start,
pause and resume as needed, then stop and save an MP4.

![The Screen Recorder window](screenshot.png)

## Features

- **Automatic screen detection** — every connected display is listed with its
  resolution, with the primary one preselected. Refresh re-scans after you plug
  or unplug a monitor.
- **Record several screens at once** — tick as many as you like. Each gets its
  own file, captured in parallel and aligned to a shared clock, so frame *N* of
  one screen is the same instant as frame *N* of the others. A multi-screen
  recording is written as a [session folder](#session-format).
- **Whole desktop as one video** — the old single-file behaviour, still there as
  its own option. Note it includes the dead space between mismatched or offset
  monitors; separate files avoid that.
- **Start / Pause / Resume / Stop & save** — paused time is excluded from the
  recording; the video cuts straight from pause to resume.
- **Global hotkeys** that work while the window is minimized:
  - `Ctrl+Alt+R` — start / stop
  - `Ctrl+Alt+P` — pause / resume
- **Optional microphone track**, picked from your DirectShow audio devices.
- **Optional webcam bubble** — draggable, burned into the recording. See
  [Camera](#camera).
- Configurable frame rate (10–60), a chosen output folder, and an option to
  minimize the window while recording so it stays out of the shot.
- **Hardware encoding, picked automatically** — see
  [Encoding](#encoding). The window shows which encoder is in use.

### Mouse effects

All four are toggles in the **Mouse** box, and each is baked into the recording
as it is captured — there is no post-processing pass.

| Option | What it does |
| --- | --- |
| **Show cursor** | Draws a cursor marker. GDI capture does not include the pointer, so without this the recording has no cursor at all. |
| **Wheel zoom** | **Ctrl+Alt+wheel** zooms the recording in and out, up to 5×, centred on the pointer. **Ctrl+Alt+middle-click** snaps back to 1×. |
| **Mouse trail** | A short amber comet trailing the pointer, tapering and fading over ~0.45 s. |
| **Click effect** | A ring that expands and fades over ~0.5 s at each left or right click. |

Zooming eases in and out rather than snapping, and the view pans to follow the
pointer while staying inside the screen edges. The cursor, trail, and click rings
are drawn *after* the zoom, so they keep a constant on-screen size instead of
being magnified with the content.

## Camera

Tick **Show camera** and a live webcam bubble appears, bottom-right of your
primary screen. Drag it anywhere — including mid-recording — and it is burned
into that screen's recording at the position you left it.

The bubble is a real always-on-top window, so you can see yourself while
recording. It is hidden from screen capture via `SetWindowDisplayAffinity`
(`WDA_EXCLUDEFROMCAPTURE`), so the recording gets exactly one copy: the clean
frame composited into the video, never the preview window caught by the grab.

Like the cursor overlays, it is composited *after* the zoom, so zooming the
screen does not move or scale it. It is drawn only on the screen it sits on, and
is clipped properly when straddling a screen edge. Size is Small / Medium /
Large; the feed is centre-cropped to fit.

Only `Ctrl+Alt+wheel` and `Ctrl+Alt+middle-click` are swallowed — plain scrolling
and clicking pass through to whatever you are recording, untouched.

Recordings go to `%USERPROFILE%\Videos\ScreenRec` by default. One screen gives
you a single H.264 MP4, `recording_YYYY-MM-DD_HH-MM-SS.mp4`.

### Session format

Recording two or more screens writes a folder instead:

```
recording_2026-10-31_12-12-49/
  recording.json
  screen1.mp4
  screen2.mp4
```

`recording.json` describes the layout so a player can rebuild the original
desktop arrangement:

```json
{
  "format": "screenrec-session",
  "version": 1,
  "created": "2026-10-31T12:12:49",
  "duration": 5.08,
  "fps": 30,
  "screens": [
    { "file": "screen1.mp4", "label": "Screen 1", "width": 1920, "height": 1080,
      "left": 1920, "top": 122, "primary": false, "audio": false },
    { "file": "screen2.mp4", "label": "Screen 2", "width": 1920, "height": 1080,
      "left": 0, "top": 0, "primary": true, "audio": true }
  ]
}
```

`left`/`top` are virtual-desktop coordinates, so they preserve how the monitors
were physically arranged, including vertical offsets. `audio` marks which file
carries the microphone track — there is one capture device, so it rides on the
first screen only.

**Display order.** Each screen carries `role` (`primary`/`secondary`, from
Windows) and `order` — display priority for a split view, with the primary
screen first and the rest in capture order. `order: 1` is the one to show first
or largest.

**Auto-switch.** A multi-screen session also records a `focus` timeline, so a
player can show *one* screen at a time instead of a split view, cutting to
whichever screen you were actually working on:

```json
"focus": {
  "source": "pointer",
  "dwell": 0.6,
  "timeline": [
    { "t": 0.0,   "screen": "screen1.mp4" },
    { "t": 1.539, "screen": "screen2.mp4" },
    { "t": 4.222, "screen": "screen1.mp4" }
  ]
}
```

Each entry means *from time `t` (recorded seconds), show this screen*. The
pointer decides: whichever monitor it is on is the active screen at that moment.

Switches are debounced by `dwell` (0.6 s), because a pointer crossing one monitor
to reach another should not register as a visit — without it a player would
flicker on every pass. A switch is timestamped at the moment the pointer
*arrived*, not when the dwell expired, so playback cuts at the real moment rather
than 0.6 s late. Pointer movement while paused is ignored, and the timeline is
already in recorded time, so it lines up with the video without adjusting for
pauses.

If one screen fails to save, the others and the manifest are still written, and
the app tells you which were lost.

## Encoding

The app probes for a working hardware encoder at startup and uses the first one
that actually opens, falling back to CPU encoding. The window shows which is
active. There is nothing to configure.

Preference order is **Quick Sync → NVENC → libx264**. Quick Sync leads because
it measured better on every axis that matters here. On an 8-second 1080p capture
of a real desktop:

| Encoder | Size | VMAF | CPU | Text RMSE vs source |
| --- | --- | --- | --- | --- |
| libx264 veryfast crf23 | 1.28 MB | 91.84 | 5.38 s | 1.54 |
| libx264 at QSV's bitrate | 0.62 MB | — | — | 3.86 |
| **h264_qsv gq23** | **0.55 MB** | **93.82** | **3.77 s** | **1.42** |

Less than half the size, slightly *higher* measured quality, and lower CPU. Text
stays sharper too — forcing libx264 down to the same bitrate visibly degrades it
(RMSE 3.86 vs 1.42), which is the comparison that matters for screen recording.

Two fallbacks, because a hardware encoder can fail for reasons the model name
does not reveal:

1. **At startup** — each candidate is probed with a short real encode. This is
   what rejects NVENC on a machine whose driver is too old.
2. **Mid-recording** — ffmpeg reading from a pipe does not open its encoder until
   frames arrive, so an encoder that probed fine can still fail on the real
   stream. The first failed write drops that screen to CPU encoding, restarts the
   segment, and tells you. The recording survives.

NVENC is probed but **untested here** — this machine's driver (595.97) predates
the 610.00 that ffmpeg 8.1.2 requires for NVENC API 13.1, so it never opens.
Updating the NVIDIA driver would make it available; it sits behind Quick Sync in
the order either way.

## Requirements

- **Windows** (uses Win32 APIs for cursor position, DPI awareness, and hotkeys)
- **Python 3.9+** with tkinter — included in the python.org installer
- **ffmpeg** on your `PATH` — the encoder. Check with `ffmpeg -version`; if it is
  missing, install it with `winget install Gyan.FFmpeg` and reopen your terminal.

## Setup

```
pip install -r requirements.txt
```

## Running

Double-click `ScreenRecorder.bat`, or:

```
python screenrec.py
```

The `.bat` launches it with `pythonw`, so no console window appears alongside
the app.

## How it works

Frames are captured with [mss](https://pypi.org/project/mss/) and piped as raw
BGRA straight into ffmpeg, which encodes them to H.264 — no intermediate image
files.

Pausing does not just stop the pipe. Each pause closes the current ffmpeg
segment and each resume starts a new one; on stop, the segments are joined with
the concat demuxer and `-c copy` (no re-encode, so it is near-instant). A single
long-running ffmpeg process would keep pulling microphone audio while the video
was paused, and every pause would push audio and video further out of sync.

The capture loop is paced against the wall clock rather than counting frames. If
capture briefly falls behind the target frame rate, the last frame is duplicated
to fill the gap, so a recording always plays back at real-time speed instead of
appearing sped up. A segment is also topped up with whatever frames it still owes
just before it closes — otherwise a stall right before a pause or stop would
quietly shorten the clip.

Effects are composited per frame: the mss buffer is wrapped as a numpy array with
no copy, zoom is an OpenCV crop-and-resize, and the trail and click rings are
alpha-blended over only the region they touch rather than the whole frame. With
every effect on at full zoom this costs about 2 ms per 1080p frame, against
roughly 19 ms to grab the screen.

Wheel and click events come from a `WH_MOUSE_LL` low-level hook running its own
message loop on a separate thread; it is installed only while recording. The
callback does no work beyond appending to a deque, since Windows silently drops a
hook that is too slow to return. There is one hook for the whole recording, not
one per screen, and events are routed to whichever screen the pointer is on — so
zooming one screen leaves the others alone.

Each screen gets its own capture thread and its own encoder. Encoders do not all
spawn in the same millisecond, so the threads meet at a `threading.Barrier` that
stamps a single `t0`; every screen numbers its frames from that shared instant.
Without it the files drifted apart by 30–70 ms. The barrier is aborted if a
capture thread dies, so one failing screen cannot hang the rest.

With the microphone enabled, the audio filter `aresample=async=1:first_pts=0`
pads the start with silence to cover the capture device's warm-up delay. Without
it the audio stream is shorter than the video, and `-shortest` clips the tail off
every segment. That flag is also what makes ffmpeg exit when the video pipe
closes — a DirectShow input never ends on its own.

## Known limitations

- **Microphone only, no system audio.** Recording what your speakers are playing
  needs a loopback device that Windows does not expose to ffmpeg by default; it
  requires installing something like VB-Cable or the screen-capture-recorder
  DirectShow filter.
- Hotkeys are polled with `GetAsyncKeyState` every 120 ms rather than registered
  system-wide, so they are ignored while an elevated (admin) window has focus.
  The mouse hook has the same limitation: zoom and click effects will not respond
  over an app running as administrator unless this app is elevated too.
- Zoom is a crop of the captured frame scaled back up, so zooming in softens the
  image — it does not re-capture at higher detail.
- Recording starts roughly 0.2–0.3 s after you press the button, the time the
  encoders take to spawn. The same gap applies on each resume.
- Microphone audio lands on the first selected screen's file only; Windows gives
  one consumer to the capture device.

## Roadmap

Planned, not built yet:

1. **Session player** — open a session folder and play its screens together.
   Split view uses the per-screen `order`; auto-switch mode uses the `focus`
   timeline to show a single screen that follows the pointer. Both inputs are
   already written by the recorder.
2. **OCR** — make the text in a recording selectable, via Cloudflare Workers AI
   `@cf/moondream/moondream3.1-9B-A2B` (the Moondream vision model, billed at
   $0.30/M input and $1.00/M output tokens).
3. **Import** — open a session folder someone else recorded and play it, making
   `screenrec-session` a portable format.
- Capture uses GDI, which does not record hardware-overlay content such as some
  fullscreen exclusive games or DRM-protected video players.
