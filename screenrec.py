"""Simple Windows screen recorder: pick a display, start / pause / stop & save.

Frames are captured with mss and piped as raw BGRA into ffmpeg. Each
pause/resume produces its own ffmpeg segment, and the segments are concatenated
on stop -- that keeps optional microphone audio in sync across pauses.
"""

import ctypes
import ctypes.wintypes as wintypes
import json
import math
import os
import queue
import re
import shutil
import subprocess
import sys
import tempfile
import threading
import time
import tkinter as tk
from collections import deque
from tkinter import filedialog, messagebox, ttk

import cv2
import mss
import numpy as np

APP_NAME = "Screen Recorder"
CREATE_NO_WINDOW = 0x08000000

# Win32 low-level mouse hook
WH_MOUSE_LL = 14
WM_QUIT = 0x0012
WM_MOUSEMOVE = 0x0200
WM_LBUTTONDOWN = 0x0201
WM_RBUTTONDOWN = 0x0204
WM_MBUTTONDOWN = 0x0207
WM_MOUSEWHEEL = 0x020A
VK_CONTROL, VK_MENU = 0x11, 0x12

MIN_ZOOM, MAX_ZOOM = 1.0, 5.0
TRAIL_SECONDS = 0.45
CLICK_SECONDS = 0.5


def make_dpi_aware():
    """Capture physical pixels on mixed-DPI setups."""
    try:
        # PROCESS_PER_MONITOR_DPI_AWARE_V2
        ctypes.windll.user32.SetProcessDpiAwarenessContext(ctypes.c_void_p(-4))
    except Exception:
        try:
            ctypes.windll.shcore.SetProcessDpiAwareness(2)
        except Exception:
            ctypes.windll.user32.SetProcessDPIAware()


def cursor_pos():
    pt = wintypes.POINT()
    ctypes.windll.user32.GetCursorPos(ctypes.byref(pt))
    return pt.x, pt.y


class MSLLHOOKSTRUCT(ctypes.Structure):
    _fields_ = [("pt", wintypes.POINT),
                ("mouseData", wintypes.DWORD),
                ("flags", wintypes.DWORD),
                ("time", wintypes.DWORD),
                ("dwExtraInfo", ctypes.POINTER(wintypes.ULONG))]


HOOKPROC = ctypes.WINFUNCTYPE(ctypes.c_ssize_t, ctypes.c_int,
                              wintypes.WPARAM, wintypes.LPARAM)


class MouseHook(threading.Thread):
    """Watches the mouse globally on its own thread with a message loop.

    Ctrl+Alt+wheel is swallowed so the window underneath does not also scroll;
    every other event is passed straight through.
    """

    def __init__(self, on_wheel, on_click, on_reset):
        super().__init__(daemon=True)
        self.on_wheel = on_wheel
        self.on_click = on_click
        self.on_reset = on_reset
        self._tid = None
        self._hook = None
        self._ready = threading.Event()

    def _configure(self, user32):
        user32.SetWindowsHookExW.restype = wintypes.HHOOK
        user32.SetWindowsHookExW.argtypes = [ctypes.c_int, HOOKPROC,
                                             wintypes.HINSTANCE, wintypes.DWORD]
        user32.CallNextHookEx.restype = ctypes.c_ssize_t
        user32.CallNextHookEx.argtypes = [wintypes.HHOOK, ctypes.c_int,
                                          wintypes.WPARAM, wintypes.LPARAM]
        user32.UnhookWindowsHookEx.argtypes = [wintypes.HHOOK]

    def _callback(self, ncode, wparam, lparam):
        user32 = ctypes.windll.user32
        if ncode == 0:
            try:
                info = ctypes.cast(lparam,
                                   ctypes.POINTER(MSLLHOOKSTRUCT)).contents
                if wparam == WM_MOUSEWHEEL:
                    held = (user32.GetAsyncKeyState(VK_CONTROL) & 0x8000 and
                            user32.GetAsyncKeyState(VK_MENU) & 0x8000)
                    if held:
                        # high word of mouseData is a signed wheel delta
                        delta = ctypes.c_short(info.mouseData >> 16).value
                        self.on_wheel(delta / 120.0)
                        return 1  # consume it
                elif wparam == WM_MBUTTONDOWN:
                    held = (user32.GetAsyncKeyState(VK_CONTROL) & 0x8000 and
                            user32.GetAsyncKeyState(VK_MENU) & 0x8000)
                    if held:
                        self.on_reset()
                        return 1
                elif wparam in (WM_LBUTTONDOWN, WM_RBUTTONDOWN):
                    self.on_click(info.pt.x, info.pt.y)
            except Exception:
                pass  # a hook callback must never raise back into Windows
        return user32.CallNextHookEx(None, ncode, wparam, lparam)

    def run(self):
        user32 = ctypes.windll.user32
        self._configure(user32)
        self._proc = HOOKPROC(self._callback)  # kept alive on the instance
        self._tid = ctypes.windll.kernel32.GetCurrentThreadId()
        self._hook = user32.SetWindowsHookExW(WH_MOUSE_LL, self._proc, None, 0)
        self._ready.set()
        if not self._hook:
            return
        msg = wintypes.MSG()
        while user32.GetMessageW(ctypes.byref(msg), None, 0, 0) > 0:
            user32.TranslateMessage(ctypes.byref(msg))
            user32.DispatchMessageW(ctypes.byref(msg))
        user32.UnhookWindowsHookEx(self._hook)

    def stop(self):
        self._ready.wait(timeout=2)
        if self._tid:
            ctypes.windll.user32.PostThreadMessageW(self._tid, WM_QUIT, 0, 0)
        self.join(timeout=2)


class Effects:
    """Zoom state and the cursor overlays, composited onto each frame."""

    def __init__(self, monitor, show_cursor, zoom, trail, clicks, camera=None):
        self.left = monitor["left"]
        self.top = monitor["top"]
        self.w = monitor["width"]
        self.h = monitor["height"]
        self.show_cursor = show_cursor
        self.zoom_enabled = zoom
        self.trail_enabled = trail
        self.clicks_enabled = clicks

        self._lock = threading.Lock()
        self.target_zoom = 1.0
        self.zoom = 1.0
        self.cx, self.cy = self.w / 2.0, self.h / 2.0  # eased view centre
        self.trail = deque(maxlen=90)
        self.clicks = deque(maxlen=12)
        self.camera = camera
        self._masks = {}

    @property
    def active(self):
        return (self.show_cursor or self.trail_enabled or self.clicks_enabled
                or self.zoom_enabled or self.camera is not None)

    # -- events from the hook thread --------------------------------------
    def on_wheel(self, steps):
        if not self.zoom_enabled:
            return
        with self._lock:
            self.target_zoom = min(MAX_ZOOM, max(
                MIN_ZOOM, self.target_zoom * (1.18 ** steps)))

    def on_reset(self):
        with self._lock:
            self.target_zoom = 1.0

    def on_click(self, x, y):
        if self.clicks_enabled:
            self.clicks.append((x - self.left, y - self.top, time.perf_counter()))

    def contains(self, x, y):
        """Is this virtual-desktop point on the screen we are recording?"""
        return (self.left <= x < self.left + self.w and
                self.top <= y < self.top + self.h)

    # -- per-frame ---------------------------------------------------------
    def _view(self, mx, my, dt, inside):
        """Ease zoom and pan toward the cursor; return the crop rectangle."""
        with self._lock:
            target = self.target_zoom
        self.zoom += (target - self.zoom) * min(1.0, dt * 9.0)
        if abs(self.zoom - target) < 0.002:
            self.zoom = target
        if self.zoom <= 1.001:
            self.zoom = 1.0
            self.cx, self.cy = self.w / 2.0, self.h / 2.0
            return 0, 0, self.w, self.h

        cw, ch = self.w / self.zoom, self.h / self.zoom
        if inside:
            # keep the view inside the screen, then ease toward the cursor
            tx = min(max(mx, cw / 2), self.w - cw / 2)
            ty = min(max(my, ch / 2), self.h - ch / 2)
            ease = min(1.0, dt * 7.0)
            self.cx += (tx - self.cx) * ease
            self.cy += (ty - self.cy) * ease
        # cursor is on another screen: hold this view rather than drifting
        x0 = int(round(min(max(self.cx - cw / 2, 0), self.w - cw)))
        y0 = int(round(min(max(self.cy - ch / 2, 0), self.h - ch)))
        return x0, y0, max(2, int(round(cw))), max(2, int(round(ch)))

    def render(self, frame, dt):
        """Apply zoom and draw the overlays. Returns the frame to encode."""
        gx, gy = cursor_pos()
        inside = self.contains(gx, gy)
        mx = gx - self.left
        my = gy - self.top
        now = time.perf_counter()

        if self.trail_enabled:
            self.trail.append((mx, my, now))

        x0, y0, cw, ch = self._view(mx, my, dt, inside)
        if self.zoom > 1.0:
            frame = cv2.resize(frame[y0:y0 + ch, x0:x0 + cw], (self.w, self.h),
                               interpolation=cv2.INTER_LINEAR)
            scale = self.w / float(cw)
        else:
            frame = frame.copy()  # do not scribble on the capture buffer
            scale = 1.0

        def to_frame(px, py):
            return int(round((px - x0) * scale)), int(round((py - y0) * scale))

        if self.trail_enabled:
            self._draw_trail(frame, now, to_frame)
        if self.clicks_enabled:
            self._draw_clicks(frame, now, to_frame)
        if self.camera is not None:
            self._draw_camera(frame)
        if self.show_cursor and inside:
            fx, fy = to_frame(mx, my)
            cv2.circle(frame, (fx, fy), 9, (0, 0, 0, 255), 2, cv2.LINE_AA)
            cv2.circle(frame, (fx, fy), 6, (255, 255, 255, 255), -1, cv2.LINE_AA)
        return frame

    def _round_mask(self, w, h, radius=16):
        key = (w, h, radius)
        if key not in self._masks:
            m = np.zeros((h, w), np.uint8)
            cv2.rectangle(m, (radius, 0), (w - radius, h), 255, -1)
            cv2.rectangle(m, (0, radius), (w, h - radius), 255, -1)
            for cx, cy in ((radius, radius), (w - radius, radius),
                           (radius, h - radius), (w - radius, h - radius)):
                cv2.circle(m, (cx, cy), radius, 255, -1, cv2.LINE_AA)
            self._masks[key] = m
        return self._masks[key]

    def _draw_camera(self, frame):
        """Composite the webcam bubble, clipped to this screen."""
        left, top, cw, ch = self.camera.rect()
        x, y = left - self.left, top - self.top          # position on this screen
        if x + cw <= 0 or y + ch <= 0 or x >= self.w or y >= self.h:
            return                                        # bubble is on another screen
        shared = self.camera.tile(cw, ch)
        if shared is None:
            return
        tile = shared.copy()                              # do not draw on the shared tile
        mask = self._round_mask(cw, ch)
        cv2.rectangle(tile, (1, 1), (cw - 2, ch - 2), (235, 235, 235), 2, cv2.LINE_AA)

        sx, sy = max(0, -x), max(0, -y)                   # clip against the frame edges
        dx, dy = max(0, x), max(0, y)
        vw = min(cw - sx, self.w - dx)
        vh = min(ch - sy, self.h - dy)
        if vw <= 0 or vh <= 0:
            return
        roi = frame[dy:dy + vh, dx:dx + vw, :3]
        src = tile[sy:sy + vh, sx:sx + vw]
        a = (mask[sy:sy + vh, sx:sx + vw].astype(np.float32) / 255.0)[:, :, None]
        roi[:] = (src * a + roi * (1.0 - a)).astype(np.uint8)

    def _blend(self, frame, pts, pad, alpha, draw):
        """Alpha-blend an effect, copying only the region it touches.

        `draw` receives a scratch copy of that region and the offset to shift
        frame coordinates into it.
        """
        if alpha <= 0.01:
            return
        h, w = frame.shape[:2]
        x0 = max(0, min(p[0] for p in pts) - pad)
        y0 = max(0, min(p[1] for p in pts) - pad)
        x1 = min(w, max(p[0] for p in pts) + pad)
        y1 = min(h, max(p[1] for p in pts) + pad)
        if x1 <= x0 or y1 <= y0:
            return
        roi = frame[y0:y1, x0:x1]
        scratch = roi.copy()
        draw(scratch, x0, y0)
        cv2.addWeighted(scratch, alpha, roi, 1.0 - alpha, 0, dst=roi)

    def _draw_trail(self, frame, now, to_frame):
        while self.trail and now - self.trail[0][2] > TRAIL_SECONDS:
            self.trail.popleft()
        pts = [to_frame(x, y) for x, y, _ in self.trail]
        if len(pts) < 2:
            return

        def draw(scratch, ox, oy):
            n = len(pts)
            for i in range(1, n):
                # taper from thin (oldest) to thick (at the cursor)
                width = max(1, int(round(1 + 7.0 * i / n)))
                cv2.line(scratch,
                         (pts[i - 1][0] - ox, pts[i - 1][1] - oy),
                         (pts[i][0] - ox, pts[i][1] - oy),
                         (80, 200, 255, 255), width, cv2.LINE_AA)

        self._blend(frame, pts, 10, 0.55, draw)

    def _draw_clicks(self, frame, now, to_frame):
        while self.clicks and now - self.clicks[0][2] > CLICK_SECONDS:
            self.clicks.popleft()
        for x, y, t in list(self.clicks):
            p = (now - t) / CLICK_SECONDS
            if not (0.0 <= p <= 1.0):
                continue
            fx, fy = to_frame(x, y)
            radius = int(round(8 + 46 * math.sqrt(p)))  # expands fast, then eases
            self._blend(
                frame, [(fx, fy)], radius + 4, 0.75 * (1.0 - p),
                lambda s, ox, oy, r=radius, cx=fx, cy=fy: cv2.circle(
                    s, (cx - ox, cy - oy), r, (90, 220, 255, 255), 3, cv2.LINE_AA))


# Video encoders in preference order. Each is (label, ffmpeg args, extra -vf).
# Quick Sync leads because it measured smaller *and* slightly sharper than x264
# on real screen content, at lower CPU; libx264 is the universal fallback.
ENCODERS = [
    ("Quick Sync", ["-c:v", "h264_qsv", "-global_quality", "23"], ",format=nv12"),
    ("NVENC", ["-c:v", "h264_nvenc", "-cq", "23", "-pix_fmt", "yuv420p"], ""),
    ("CPU (x264)", ["-c:v", "libx264", "-preset", "veryfast", "-crf", "23",
                    "-pix_fmt", "yuv420p"], ""),
]


def detect_encoder(ffmpeg):
    """Return the first encoder that actually opens on this machine.

    Hardware encoders fail for reasons you cannot tell from the model name --
    a too-old driver, a disabled iGPU, a laptop on battery -- so probe rather
    than assume.
    """
    for entry in ENCODERS[:-1]:
        label, args, vf = entry
        cmd = [ffmpeg, "-hide_banner", "-loglevel", "error", "-y",
               "-f", "lavfi", "-i", "color=c=black:s=640x480:r=30", "-frames:v", "5"]
        if vf:
            cmd += ["-vf", vf.lstrip(",")]
        cmd += args + ["-f", "null", "-"]
        try:
            if subprocess.run(cmd, capture_output=True, timeout=30,
                              creationflags=CREATE_NO_WINDOW).returncode == 0:
                return entry
        except Exception:
            continue
    return ENCODERS[-1]


CAMERA_SIZES = [("Small", 240, 180), ("Medium", 320, 240), ("Large", 440, 330)]
CAMERA_MARGIN = 28
WDA_EXCLUDEFROMCAPTURE = 0x11
GA_ROOT = 2


def exclude_from_capture(tk_window):
    """Hide a window from screen capture while leaving it visible on screen.

    Lets the camera preview be a real draggable window without it being
    recorded twice -- once as the overlay we composite, once as a window the
    screen grab happened to catch.
    """
    try:
        u = ctypes.windll.user32
        u.SetWindowDisplayAffinity.argtypes = [wintypes.HWND, wintypes.DWORD]
        u.SetWindowDisplayAffinity.restype = wintypes.BOOL
        u.GetAncestor.argtypes = [wintypes.HWND, ctypes.c_uint]
        u.GetAncestor.restype = wintypes.HWND
        root = u.GetAncestor(wintypes.HWND(tk_window.winfo_id()), GA_ROOT)
        return bool(u.SetWindowDisplayAffinity(root, WDA_EXCLUDEFROMCAPTURE))
    except Exception:
        return False


def list_video_devices(ffmpeg):
    """DirectShow camera names, in the order OpenCV indexes them."""
    try:
        out = subprocess.run(
            [ffmpeg, "-hide_banner", "-list_devices", "true", "-f", "dshow", "-i", "dummy"],
            capture_output=True, text=True, timeout=20,
            creationflags=CREATE_NO_WINDOW, errors="replace",
        ).stderr
    except Exception:
        return []
    names, seen = [], set()
    for line in out.splitlines():
        if "(video)" not in line:
            continue
        m = re.search(r'"([^"]+)"', line)
        if m and m.group(1) not in seen:
            seen.add(m.group(1))
            names.append(m.group(1))
    return names


class CameraFeed:
    """Webcam frames, plus the rectangle the overlay occupies on the desktop."""

    def __init__(self, index, width, height):
        self.index = index
        self.w = width
        self.h = height
        self.left = 0
        self.top = 0
        self.error = None
        self._frame = None
        self._seq = 0
        self._tile = None          # resized once per frame, shared by every screen
        self._tile_key = None
        self._lock = threading.Lock()
        self._stop = threading.Event()
        self._thread = None

    def start(self):
        self._thread = threading.Thread(target=self._run, daemon=True)
        self._thread.start()

    def _run(self):
        cap = None
        try:
            cap = cv2.VideoCapture(self.index, cv2.CAP_DSHOW)
            if not cap.isOpened():
                self.error = "Could not open that camera."
                return
            cap.set(cv2.CAP_PROP_FRAME_WIDTH, 640)
            cap.set(cv2.CAP_PROP_FRAME_HEIGHT, 480)
            while not self._stop.is_set():
                ok, img = cap.read()
                if not ok:
                    time.sleep(0.05)
                    continue
                with self._lock:
                    self._frame = img
                    self._seq += 1
        except Exception as exc:
            self.error = "%s: %s" % (type(exc).__name__, exc)
        finally:
            if cap is not None:
                cap.release()

    def frame(self):
        with self._lock:
            return self._frame

    def tile(self, w, h):
        """The camera scaled to the overlay size, computed once per frame."""
        with self._lock:
            img, key = self._frame, (self._seq, w, h)
            if img is None:
                return None
            if self._tile_key == key:
                return self._tile
        tile = camera_tile(img, w, h)
        with self._lock:
            self._tile, self._tile_key = tile, key
        return tile

    def rect(self):
        with self._lock:
            return self.left, self.top, self.w, self.h

    def move_to(self, left, top):
        with self._lock:
            self.left, self.top = int(left), int(top)

    def stop(self):
        self._stop.set()
        if self._thread:
            self._thread.join(timeout=3)


class CameraOverlay:
    """The draggable on-screen bubble showing the live camera."""

    def __init__(self, master, feed):
        self.feed = feed
        self.win = tk.Toplevel(master)
        self.win.overrideredirect(True)
        self.win.attributes("-topmost", True)
        self.win.geometry("%dx%d+%d+%d" % (feed.w, feed.h, feed.left, feed.top))
        self.label = tk.Label(self.win, bd=0, highlightthickness=0, bg="#101010",
                              cursor="fleur", text="camera starting...",
                              fg="#999999", font=("Segoe UI", 9))
        self.label.pack(fill="both", expand=True)
        for w in (self.win, self.label):
            w.bind("<Button-1>", self._grab)
            w.bind("<B1-Motion>", self._drag)
        self.win.update_idletasks()
        exclude_from_capture(self.win)
        self._alive = True
        self._tick()

    def _grab(self, ev):
        self._dx, self._dy = ev.x_root - self.win.winfo_x(), ev.y_root - self.win.winfo_y()

    def _drag(self, ev):
        x, y = ev.x_root - self._dx, ev.y_root - self._dy
        self.win.geometry("+%d+%d" % (x, y))
        self.feed.move_to(x, y)

    def _tick(self):
        if not self._alive:
            return
        tile = self.feed.tile(self.feed.w, self.feed.h)
        if tile is not None:
            rgb = cv2.cvtColor(tile, cv2.COLOR_BGR2RGB)
            photo = tk.PhotoImage(
                data=b"P6\n%d %d\n255\n" % (rgb.shape[1], rgb.shape[0]) + rgb.tobytes())
            self.label.configure(image=photo, text="")
            self.label.image = photo
        elif self.feed.error:
            self.label.configure(text=self.feed.error, image="")
        self.win.after(40, self._tick)

    def raise_above(self):
        try:
            self.win.attributes("-topmost", True)
            self.win.lift()
            self.win.deiconify()
        except tk.TclError:
            pass

    def destroy(self):
        self._alive = False
        try:
            self.win.destroy()
        except tk.TclError:
            pass


def camera_tile(img, w, h):
    """Centre-crop the camera frame to the overlay's aspect, then resize."""
    ih, iw = img.shape[:2]
    want = w / float(h)
    if iw / float(ih) > want:
        nw = max(1, int(round(ih * want)))
        img = img[:, (iw - nw) // 2:(iw - nw) // 2 + nw]
    else:
        nh = max(1, int(round(iw / want)))
        img = img[(ih - nh) // 2:(ih - nh) // 2 + nh, :]
    return cv2.resize(img, (w, h), interpolation=cv2.INTER_AREA)


def list_audio_devices(ffmpeg):
    """Names of DirectShow audio inputs (microphones)."""
    try:
        out = subprocess.run(
            [ffmpeg, "-hide_banner", "-list_devices", "true", "-f", "dshow", "-i", "dummy"],
            capture_output=True, text=True, timeout=20,
            creationflags=CREATE_NO_WINDOW, errors="replace",
        ).stderr
    except Exception:
        return []
    names, seen = [], set()
    for line in out.splitlines():
        if "(audio)" not in line:
            continue
        m = re.search(r'"([^"]+)"', line)
        if m and m.group(1) not in seen:
            seen.add(m.group(1))
            names.append(m.group(1))
    return names


FOCUS_DWELL = 0.6      # the pointer must settle this long before the view switches
FOCUS_INTERVAL = 0.08


class FocusTracker(threading.Thread):
    """Logs which screen the pointer is on, as a timeline in recorded time.

    This is what lets a player show one screen at a time instead of a split
    view: at any moment it knows which screen the user was actually working on.
    Switches are debounced, because a pointer crossing one monitor to reach
    another should not register as a visit.
    """

    def __init__(self, monitors, clock, recording, dwell=FOCUS_DWELL):
        super().__init__(daemon=True)
        self.monitors = monitors
        self.clock = clock            # -> recorded seconds, frozen while paused
        self.recording = recording    # -> False while paused
        self.dwell = dwell
        self.timeline = []            # [(t, screen index)]
        self.active = None
        self._cand = None
        self._cand_at = 0.0
        self._halt = threading.Event()
        self._lock = threading.Lock()

    def screen_at(self, x, y):
        for i, m in enumerate(self.monitors):
            if (m["left"] <= x < m["left"] + m["width"] and
                    m["top"] <= y < m["top"] + m["height"]):
                return i
        return None

    def run(self):
        start = self.screen_at(*cursor_pos())
        if start is None:
            start = next((i for i, m in enumerate(self.monitors)
                          if m.get("is_primary")), 0)
        with self._lock:
            self.active = start
            self.timeline.append((0.0, start))
        while not self._halt.wait(FOCUS_INTERVAL):
            if not self.recording():
                self._cand = None
                continue
            here = self.screen_at(*cursor_pos())
            if here is None or here == self.active:
                self._cand = None
                continue
            now = self.clock()
            if here != self._cand:
                self._cand, self._cand_at = here, now
            elif now - self._cand_at >= self.dwell:
                with self._lock:
                    self.active = here
                    # credit the switch to when the pointer arrived, not when
                    # the dwell expired, so playback cuts at the real moment
                    self.timeline.append((round(self._cand_at, 3), here))
                self._cand = None

    def stop(self):
        self._halt.set()
        self.join(timeout=2)

    def entries(self, names):
        with self._lock:
            return [{"t": t, "screen": names[i]} for t, i in self.timeline
                    if i < len(names)]


class SyncClock:
    """One start instant shared by every screen in a recording.

    Encoders do not all spawn in the same millisecond, so each capture thread
    waits here until its peers are ready; the barrier then stamps a single t0.
    Pegging frame numbering to that shared instant is what keeps frame N of
    every screen the same moment in time.
    """

    def __init__(self, parties):
        self.t0 = 0.0
        self._barrier = threading.Barrier(parties, action=self._mark)

    def _mark(self):
        self.t0 = time.perf_counter()

    def wait(self, timeout=30.0):
        try:
            self._barrier.wait(timeout)
        except (threading.BrokenBarrierError, RuntimeError):
            # a peer died or we are shutting down: fall back to our own clock
            if not self.t0:
                self.t0 = time.perf_counter()
        return self.t0

    def reset(self):
        try:
            self._barrier.reset()
        except threading.BrokenBarrierError:
            pass

    def abort(self):
        self._barrier.abort()


class Recorder:
    """Capture thread plus ffmpeg segment management."""

    def __init__(self, ffmpeg, monitor, fps, audio_device, effects, on_error,
                 sync=None, encoder=None):
        self.ffmpeg = ffmpeg
        self.monitor = monitor
        self.fps = fps
        self.audio_device = audio_device
        self.effects = effects
        self.on_error = on_error
        self.sync = sync or SyncClock(1)
        self.encoder = encoder or ENCODERS[-1]

        self.width = monitor["width"]
        self.height = monitor["height"]
        self.tmpdir = tempfile.mkdtemp(prefix="screenrec_")
        self.segments = []

        self._proc = None
        self._log = None
        self._thread = None
        self._stop = threading.Event()
        self._resume = threading.Event()
        self._lock = threading.Lock()
        self.elapsed = 0.0  # recorded seconds, excludes paused time

    # -- ffmpeg segments -------------------------------------------------
    def _fallback_to_cpu(self):
        """Drop to CPU encoding after a hardware encoder dies mid-stream.

        ffmpeg reading from a pipe does not open its encoder until frames
        arrive, so a hardware encoder that probed fine can still fail on the
        real stream -- the first write is where we find out.
        """
        if self.encoder is ENCODERS[-1]:
            return False
        why = ""
        if self._log and not self._log.closed:
            self._log.seek(0)
            lines = self._log.read().strip().splitlines()
            why = lines[-1] if lines else ""
            self._log.close()
        if self._proc:
            try:
                self._proc.kill()
            except Exception:
                pass
            self._proc = None
        if self.segments:
            dead = self.segments.pop()
            if os.path.exists(dead):
                try:
                    os.remove(dead)
                except OSError:
                    pass
        self.on_error("%s encoding failed, switching to CPU encoding.\n%s"
                      % (self.encoder[0], why))
        self.encoder = ENCODERS[-1]
        self._start_segment()
        return True

    def _start_segment(self):
        path = os.path.join(self.tmpdir, "seg%03d.mp4" % len(self.segments))
        cmd = [
            self.ffmpeg, "-hide_banner", "-loglevel", "error", "-y",
            "-f", "rawvideo", "-pix_fmt", "bgra",
            "-s", "%dx%d" % (self.width, self.height), "-r", str(self.fps),
            "-thread_queue_size", "512", "-i", "pipe:0",
        ]
        if self.audio_device:
            cmd += ["-f", "dshow", "-thread_queue_size", "1024",
                    "-i", "audio=" + self.audio_device]
        _, venc, extra_vf = self.encoder
        cmd += ["-vf", "pad=ceil(iw/2)*2:ceil(ih/2)*2" + extra_vf] + venc
        if self.audio_device:
            # first_pts=0 pads the mic's late start with silence, so the audio
            # stream is as long as the video and -shortest clips nothing.
            # -shortest is also what makes ffmpeg exit: the dshow input never ends.
            cmd += ["-af", "aresample=async=1:first_pts=0",
                    "-c:a", "aac", "-b:a", "160k", "-shortest"]
        cmd.append(path)

        self._log = open(path + ".log", "w+", encoding="utf-8", errors="replace")
        self._proc = subprocess.Popen(
            cmd, stdin=subprocess.PIPE, stdout=self._log, stderr=self._log,
            creationflags=CREATE_NO_WINDOW,
        )
        self.segments.append(path)

    def _end_segment(self):
        if not self._proc:
            return
        proc, log, self._proc, self._log = self._proc, self._log, None, None
        try:
            proc.stdin.close()
        except Exception:
            pass
        try:
            proc.wait(timeout=90)
        except subprocess.TimeoutExpired:
            proc.kill()
        if proc.returncode:
            log.seek(0)
            self.on_error(log.read()[-800:] or "ffmpeg exited with an error.")
        log.close()

    # -- public control --------------------------------------------------
    def start(self):
        self._resume.set()
        self._thread = threading.Thread(target=self._run, daemon=True)
        self._thread.start()

    def pause(self):
        self._resume.clear()

    def resume(self):
        self._resume.set()

    def stop(self):
        self._stop.set()
        self._resume.set()
        if self._thread:
            self._thread.join(timeout=120)

    def duration(self):
        with self._lock:
            return self.elapsed

    def _pad_segment(self, last_data, frames, seg_start):
        """Write any frames the loop still owes before a segment is closed.

        Without this, a stall right before a pause or stop silently shortens the
        segment -- and with audio, desyncs its tail.
        """
        if last_data is None:
            return frames
        due = int((time.perf_counter() - seg_start) * self.fps) - frames
        for _ in range(max(0, due)):
            self._proc.stdin.write(last_data)
        return frames + max(0, due)

    # -- capture loop ----------------------------------------------------
    def _run(self):
        try:
            with mss.MSS() as sct:
                self._start_segment()
                frames = 0
                seg_start = last_draw = self.sync.wait()
                base_elapsed = 0.0
                frame_time = 1.0 / self.fps
                last_data = None

                while not self._stop.is_set():
                    if not self._resume.is_set():
                        # close the current segment, wait, then open a new one
                        frames = self._pad_segment(last_data, frames, seg_start)
                        base_elapsed += time.perf_counter() - seg_start
                        with self._lock:
                            self.elapsed = base_elapsed
                        self._end_segment()
                        self._resume.wait()
                        if self._stop.is_set():
                            return
                        self._start_segment()
                        frames = 0
                        last_data = None
                        seg_start = last_draw = self.sync.wait()
                        if self._stop.is_set():
                            return
                        continue

                    now = time.perf_counter()
                    seg_elapsed = now - seg_start
                    with self._lock:
                        self.elapsed = base_elapsed + seg_elapsed

                    due = int(seg_elapsed * self.fps) - frames
                    if due <= 0:
                        time.sleep(min(frame_time / 4, 0.01))
                        continue

                    shot = sct.grab(self.monitor)
                    if self.effects.active:
                        frame = np.frombuffer(shot.raw, np.uint8).reshape(
                            shot.height, shot.width, 4)
                        data = self.effects.render(frame, now - last_draw).tobytes()
                        last_draw = now
                    else:
                        data = shot.raw
                    # duplicate frames if capture fell behind, so playback is realtime
                    try:
                        for _ in range(due):
                            self._proc.stdin.write(data)
                    except (BrokenPipeError, OSError):
                        if not self._fallback_to_cpu():
                            raise
                        frames = 0
                        last_data = None
                        seg_start = last_draw = time.perf_counter()
                        continue
                    frames += due
                    last_data = data

                frames = self._pad_segment(last_data, frames, seg_start)
                base_elapsed += time.perf_counter() - seg_start
                with self._lock:
                    self.elapsed = base_elapsed
                self._end_segment()
        except (BrokenPipeError, OSError) as exc:
            self.on_error("Encoder stopped unexpectedly: %s" % exc)
        except Exception as exc:  # keep the GUI alive on any capture failure
            self.on_error("%s: %s" % (type(exc).__name__, exc))
        finally:
            self.sync.abort()
            try:
                self._end_segment()
            except Exception:
                pass

    # -- finalise --------------------------------------------------------
    def save(self, dest):
        segs = [s for s in self.segments
                if os.path.exists(s) and os.path.getsize(s) > 0]
        if not segs:
            raise RuntimeError("Nothing was recorded.")
        if len(segs) == 1:
            try:
                shutil.move(segs[0], dest)
            except OSError:  # source still locked; copy and let cleanup deal with it
                shutil.copyfile(segs[0], dest)
            return dest

        listfile = os.path.join(self.tmpdir, "segments.txt")
        with open(listfile, "w", encoding="utf-8") as fh:
            for s in segs:
                fh.write("file '%s'\n" % s.replace("\\", "/"))
        res = subprocess.run(
            [self.ffmpeg, "-hide_banner", "-loglevel", "error", "-y",
             "-f", "concat", "-safe", "0", "-i", listfile, "-c", "copy", dest],
            capture_output=True, text=True, errors="replace",
            creationflags=CREATE_NO_WINDOW,
        )
        if res.returncode != 0:
            raise RuntimeError(res.stderr[-800:] or "Failed to join the segments.")
        return dest

    def cleanup(self):
        shutil.rmtree(self.tmpdir, ignore_errors=True)


SESSION_FORMAT = "screenrec-session"
SESSION_VERSION = 2
MANIFEST_NAME = "recording.json"


class MultiRecorder:
    """Records one or more screens at once, each to its own file.

    Every screen gets its own capture thread and its own encoder, but they share
    a single mouse hook: wheel and click events are routed to whichever screen
    the pointer is on, so zooming one screen does not zoom the others.
    """

    def __init__(self, ffmpeg, targets, fps, audio_device, opts, on_error,
                 encoder=None, camera=None):
        self.fps = fps
        self.encoder = encoder or ENCODERS[-1]
        self.audio_device = audio_device
        self.on_error = on_error
        self.hook = None
        self.paused = False
        self.focus = None
        self.recorders = []
        self.sync = SyncClock(len(targets))
        for i, mon in enumerate(targets):
            fx = Effects(mon, show_cursor=opts["cursor"], zoom=opts["zoom"],
                         trail=opts["trail"], clicks=opts["clicks"], camera=camera)
            self.recorders.append(Recorder(
                ffmpeg=ffmpeg, monitor=mon, fps=fps,
                # one capture device, one consumer: the mic rides on the first file
                audio_device=audio_device if i == 0 else None,
                effects=fx, on_error=on_error, sync=self.sync,
                encoder=self.encoder))

    # -- hook routing ------------------------------------------------------
    def _effects_at(self, x, y):
        for r in self.recorders:
            if r.effects.contains(x, y):
                return r.effects
        return None

    def _on_wheel(self, steps):
        fx = self._effects_at(*cursor_pos())
        if fx:
            fx.on_wheel(steps)

    def _on_click(self, x, y):
        fx = self._effects_at(x, y)
        if fx:
            fx.on_click(x, y)

    def _on_reset(self):
        for r in self.recorders:
            r.effects.on_reset()

    # -- control -----------------------------------------------------------
    def start(self):
        if len(self.recorders) > 1:   # only meaningful across several screens
            self.focus = FocusTracker(
                [r.monitor for r in self.recorders],
                clock=self.duration,
                recording=lambda: not self.paused)
            self.focus.start()
        if any(r.effects.zoom_enabled or r.effects.clicks_enabled
               for r in self.recorders):
            self.hook = MouseHook(self._on_wheel, self._on_click, self._on_reset)
            self.hook.start()
        for r in self.recorders:
            r.start()

    def pause(self):
        self.paused = True
        for r in self.recorders:
            r.pause()

    def resume(self):
        self.sync.reset()
        self.paused = False
        for r in self.recorders:
            r.resume()

    def stop(self):
        for r in self.recorders:
            r._stop.set()
            r._resume.set()
        self.sync.abort()
        for r in self.recorders:
            r.stop()
        if self.hook:
            self.hook.stop()
            self.hook = None
        if self.focus:
            self.focus.stop()

    def duration(self):
        return max((r.duration() for r in self.recorders), default=0.0)

    # -- finalise ----------------------------------------------------------
    def save(self, outdir):
        """Write the recording. Returns (path, is_session_folder)."""
        stamp = time.strftime("recording_%Y-%m-%d_%H-%M-%S")
        if len(self.recorders) == 1:
            dest = os.path.join(outdir, stamp + ".mp4")
            self.recorders[0].save(dest)
            return dest, False

        folder = os.path.join(outdir, stamp)
        os.makedirs(folder, exist_ok=True)
        screens, failures = [], []
        for i, r in enumerate(self.recorders, start=1):
            name = "screen%d.mp4" % i
            try:
                r.save(os.path.join(folder, name))
            except Exception as exc:
                failures.append("%s: %s" % (r.monitor.get("label", name), exc))
                continue
            mon = r.monitor
            screens.append({
                "file": name,
                "label": mon.get("label", "Screen %d" % i),
                "width": mon["width"], "height": mon["height"],
                "left": mon["left"], "top": mon["top"],
                "primary": bool(mon.get("is_primary")),
                "audio": r.audio_device is not None,
                "role": "primary" if mon.get("is_primary") else "secondary",
            })
        # display priority for a split view: the primary screen leads, the rest
        # follow in the order they were captured. 1 is shown first / largest.
        for rank, entry in enumerate(
                sorted(screens, key=lambda s: (s["role"] != "primary",)), start=1):
            entry["order"] = rank
        if not screens:
            raise RuntimeError("Nothing was recorded.\n" + "\n".join(failures))

        manifest = {
            "format": SESSION_FORMAT,
            "version": SESSION_VERSION,
            "created": time.strftime("%Y-%m-%dT%H:%M:%S"),
            "duration": round(self.duration(), 3),
            "fps": self.fps,
            "encoder": self.encoder[0],
            "camera": bool(self.recorders[0].effects.camera),
            "screens": screens,
        }
        if self.focus and len(screens) > 1:
            # index by recorder position, matching how the files were named
            saved = {s["file"] for s in screens}
            names = ["screen%d.mp4" % i
                     for i in range(1, len(self.recorders) + 1)]
            timeline = [e for e in self.focus.entries(names)
                        if e["screen"] in saved]
            manifest["focus"] = {
                "source": "pointer",
                "dwell": self.focus.dwell,
                "note": "Auto-switch playback: at time t (recorded seconds), "
                        "show this screen. Already debounced.",
                "timeline": timeline,
            }
        with open(os.path.join(folder, MANIFEST_NAME), "w", encoding="utf-8") as fh:
            json.dump(manifest, fh, indent=2)
        if failures:
            raise PartialSave(folder, failures)
        return folder, True

    def cleanup(self):
        for r in self.recorders:
            r.cleanup()


class PartialSave(Exception):
    """Some screens saved, some did not."""

    def __init__(self, folder, failures):
        super().__init__("\n".join(failures))
        self.folder = folder
        self.failures = failures


class App(ttk.Frame):
    def __init__(self, master, ffmpeg):
        super().__init__(master, padding=14)
        self.ffmpeg = ffmpeg
        self.rec = None
        self.state = "idle"  # idle | recording | paused
        self.errors = queue.Queue()
        self.camera = None
        self.camera_overlay = None
        self.devices_ready = False
        self.encoder = None
        self._encoder_ready = threading.Event()
        self.grid(sticky="nsew")
        master.columnconfigure(0, weight=1)
        self.columnconfigure(1, weight=1)

        self._build()
        self.refresh_monitors()
        self._sync_ui()
        # probing costs a second or two; do not hold up the window
        threading.Thread(target=self._detect_encoder, daemon=True).start()
        threading.Thread(target=self._probe_devices, daemon=True).start()
        self.after(200, self._tick)
        self.after(120, self._poll_hotkeys)

    # -- ui ---------------------------------------------------------------
    def _build(self):
        r = 0
        head = ttk.Frame(self)
        head.grid(row=r, column=0, columnspan=3, sticky="ew")
        head.columnconfigure(0, weight=1)
        ttk.Label(head, text="Screens to record").grid(row=0, column=0, sticky="w")
        ttk.Button(head, text="Refresh", width=9,
                   command=self.refresh_monitors).grid(row=0, column=1)

        r += 1
        self.screens_frame = ttk.LabelFrame(self, padding=(10, 4, 10, 8))
        self.screens_frame.grid(row=r, column=0, columnspan=3, sticky="ew",
                                pady=(4, 2))

        r += 1
        ttk.Label(self, text="Microphone").grid(row=r, column=0, sticky="w", pady=4)
        self.audio_var = tk.StringVar(value="None")
        self.audio_box = ttk.Combobox(self, textvariable=self.audio_var,
                                      state="readonly", width=42, values=["None"])
        self.audio_box.grid(row=r, column=1, sticky="ew", pady=4)

        r += 1
        opts = ttk.Frame(self)
        opts.grid(row=r, column=0, columnspan=3, sticky="ew", pady=(10, 4))
        ttk.Label(opts, text="FPS").pack(side="left")
        self.fps_var = tk.StringVar(value="30")
        self.fps_box = ttk.Combobox(opts, textvariable=self.fps_var, width=5,
                                    state="readonly",
                                    values=["10", "15", "24", "30", "60"])
        self.fps_box.pack(side="left", padx=(6, 16))
        self.minimize_var = tk.BooleanVar(value=True)
        ttk.Checkbutton(opts, text="Minimize while recording",
                        variable=self.minimize_var).pack(side="left")

        r += 1
        self.fx_frame = fx = ttk.LabelFrame(self, text="Mouse",
                                            padding=(10, 6, 10, 8))
        fx.grid(row=r, column=0, columnspan=3, sticky="ew", pady=(8, 2))
        self.cursor_var = tk.BooleanVar(value=True)
        self.zoom_var = tk.BooleanVar(value=True)
        self.trail_var = tk.BooleanVar(value=False)
        self.click_var = tk.BooleanVar(value=False)
        ttk.Checkbutton(fx, text="Show cursor",
                        variable=self.cursor_var).grid(row=0, column=0, sticky="w")
        ttk.Checkbutton(fx, text="Wheel zoom",
                        variable=self.zoom_var).grid(row=0, column=1, sticky="w",
                                                     padx=(18, 0))
        ttk.Checkbutton(fx, text="Mouse trail",
                        variable=self.trail_var).grid(row=1, column=0, sticky="w",
                                                      pady=(4, 0))
        ttk.Checkbutton(fx, text="Click effect",
                        variable=self.click_var).grid(row=1, column=1, sticky="w",
                                                      padx=(18, 0), pady=(4, 0))
        ttk.Label(fx, foreground="#888888",
                  text="Zoom with Ctrl+Alt+wheel while recording;\n"
                       "Ctrl+Alt+middle-click resets it.",
                  ).grid(row=0, column=2, rowspan=2, sticky="w", padx=(24, 0))

        r += 1
        cam = ttk.LabelFrame(self, text="Camera", padding=(10, 6, 10, 8))
        cam.grid(row=r, column=0, columnspan=3, sticky="ew", pady=(8, 2))
        cam.columnconfigure(1, weight=1)
        self.camera_var = tk.BooleanVar(value=False)
        self.cam_check = ttk.Checkbutton(cam, text="Show camera", variable=self.camera_var,
                                         command=self.toggle_camera)
        self.cam_check.grid(row=0, column=0, sticky="w")
        self.cam_device_var = tk.StringVar()
        self.cam_box = ttk.Combobox(cam, textvariable=self.cam_device_var, width=26,
                                    state="readonly", values=["(looking...)"])
        self.cam_box.current(0)
        self.cam_box.grid(row=0, column=1, sticky="ew", padx=(10, 6))
        self.cam_size_var = tk.StringVar(value=CAMERA_SIZES[1][0])
        self.cam_size_box = ttk.Combobox(cam, textvariable=self.cam_size_var, width=8,
                                         state="readonly",
                                         values=[n for n, _, _ in CAMERA_SIZES])
        self.cam_size_box.grid(row=0, column=2)
        for w in (self.cam_box, self.cam_size_box):
            w.bind("<<ComboboxSelected>>", lambda _e: self.restart_camera())
        ttk.Label(cam, foreground="#888888",
                  text="Drag the bubble anywhere; it is burned into that screen's "
                       "recording.").grid(row=1, column=0, columnspan=3, sticky="w",
                                          pady=(6, 0))

        r += 1
        ttk.Label(self, text="Save to").grid(row=r, column=0, sticky="w", pady=4)
        self.dir_var = tk.StringVar(
            value=os.path.join(os.path.expanduser("~"), "Videos", "ScreenRec"))
        ttk.Entry(self, textvariable=self.dir_var).grid(row=r, column=1,
                                                       sticky="ew", pady=4)
        ttk.Button(self, text="Browse", width=9,
                   command=self.choose_dir).grid(row=r, column=2, padx=(8, 0))

        r += 1
        ttk.Separator(self).grid(row=r, column=0, columnspan=3, sticky="ew", pady=12)

        r += 1
        self.timer = ttk.Label(self, text="00:00:00", font=("Segoe UI", 26))
        self.timer.grid(row=r, column=0, columnspan=3)

        r += 1
        self.status = ttk.Label(self, text="Ready", foreground="#666666")
        self.status.grid(row=r, column=0, columnspan=3, pady=(0, 10))

        r += 1
        btns = ttk.Frame(self)
        btns.grid(row=r, column=0, columnspan=3, sticky="ew")
        for i in range(3):
            btns.columnconfigure(i, weight=1)
        self.start_btn = ttk.Button(btns, text="Start recording", command=self.on_start)
        self.start_btn.grid(row=0, column=0, sticky="ew", padx=3, ipady=6)
        self.pause_btn = ttk.Button(btns, text="Pause", command=self.on_pause)
        self.pause_btn.grid(row=0, column=1, sticky="ew", padx=3, ipady=6)
        self.stop_btn = ttk.Button(btns, text="Stop & save", command=self.on_stop)
        self.stop_btn.grid(row=0, column=2, sticky="ew", padx=3, ipady=6)

        r += 1
        self.enc_label = ttk.Label(self, foreground="#888888",
                                   text="Encoder:  detecting...")
        self.enc_label.grid(row=r, column=0, columnspan=3, pady=(10, 0))

        r += 1
        ttk.Label(self, foreground="#888888",
                  text="Hotkeys:  Ctrl+Alt+R start/stop      Ctrl+Alt+P pause/resume",
                  ).grid(row=r, column=0, columnspan=3, pady=(12, 0))

    def refresh_monitors(self):
        """Rebuild the screen checkboxes from the current display layout."""
        if self.state != "idle":
            return
        with mss.MSS() as sct:
            mons = sct.monitors
        previously = {m["label"] for m, v in getattr(self, "screens", [])
                      if v.get()}

        for child in self.screens_frame.winfo_children():
            child.destroy()
        self.screens = []
        for i, m in enumerate(mons[1:], start=1):
            mon = dict(m)
            mon["label"] = "Screen %d" % i
            tag = "  (primary)" if mon.get("is_primary") else ""
            var = tk.BooleanVar(
                value=mon["label"] in previously if previously
                else bool(mon.get("is_primary")))
            ttk.Checkbutton(
                self.screens_frame, variable=var,
                text="%s  -  %dx%d%s" % (mon["label"], mon["width"],
                                         mon["height"], tag),
            ).grid(row=i - 1, column=0, sticky="w", pady=1)
            self.screens.append((mon, var))

        if len(mons) > 2:  # the whole desktop, gaps and all, as a single video
            desk = dict(mons[0])
            desk["label"] = "Whole desktop"
            self.desktop_var = tk.BooleanVar(value=False)
            ttk.Separator(self.screens_frame).grid(
                row=len(self.screens), column=0, sticky="ew", pady=5)
            ttk.Checkbutton(
                self.screens_frame, variable=self.desktop_var,
                command=self._sync_ui,
                text="Whole desktop as one video  -  %dx%d" % (desk["width"],
                                                               desk["height"]),
            ).grid(row=len(self.screens) + 1, column=0, sticky="w")
            self.desktop = desk
        else:
            self.desktop = None
            self.desktop_var = tk.BooleanVar(value=False)

        if not any(v.get() for _, v in self.screens) and self.screens:
            self.screens[0][1].set(True)

    def selected_targets(self):
        if self.desktop_var.get() and self.desktop:
            return [self.desktop]
        return [m for m, v in self.screens if v.get()]

    # -- camera -----------------------------------------------------------
    def _default_camera_pos(self, w, h):
        """Bottom-right of the primary screen, inset by a margin."""
        target = next((m for m, _ in self.screens if m.get("is_primary")),
                      self.screens[0][0] if self.screens else None)
        if not target:
            return CAMERA_MARGIN, CAMERA_MARGIN
        return (target["left"] + target["width"] - w - CAMERA_MARGIN,
                target["top"] + target["height"] - h - CAMERA_MARGIN)

    def toggle_camera(self):
        if not self.camera_var.get():
            self.stop_camera()
            return
        names = list(self.cam_box["values"])
        if not names or names[0].startswith("("):
            messagebox.showerror(APP_NAME, "Still looking for cameras, try again "
                                 "in a moment." if not self.devices_ready
                                 else "No camera was found.")
            self.camera_var.set(False)
            return
        _, w, h = next(s for s in CAMERA_SIZES if s[0] == self.cam_size_var.get())
        index = self.cam_box.current()
        feed = CameraFeed(index, w, h)
        feed.left, feed.top = self._default_camera_pos(w, h)
        feed.start()
        self.camera = feed
        self.camera_overlay = CameraOverlay(self.master, feed)

    def stop_camera(self):
        if self.camera_overlay:
            self.camera_overlay.destroy()
            self.camera_overlay = None
        if self.camera:
            self.camera.stop()
            self.camera = None

    def restart_camera(self):
        if self.camera_var.get():
            self.stop_camera()
            self.toggle_camera()

    def _probe_devices(self):
        """Enumerating dshow devices shells out to ffmpeg twice; keep it off
        the startup path so the window appears immediately."""
        mics = list_audio_devices(self.ffmpeg)
        cams = list_video_devices(self.ffmpeg)
        self.after(0, lambda: self._apply_devices(mics, cams))

    def _apply_devices(self, mics, cams):
        self.audio_box["values"] = ["None"] + mics
        if self.audio_var.get() not in (["None"] + mics):
            self.audio_box.current(0)
        self.cam_box["values"] = cams or ["(no camera found)"]
        self.cam_box.current(0)
        self.devices_ready = True

    def _detect_encoder(self):
        enc = detect_encoder(self.ffmpeg)
        self.encoder = enc
        self._encoder_ready.set()
        self.after(0, lambda: self.enc_label.config(
            text="Encoder:  %s" % enc[0]))

    def choose_dir(self):
        d = filedialog.askdirectory(
            initialdir=self.dir_var.get() or os.path.expanduser("~"))
        if d:
            self.dir_var.set(os.path.normpath(d))

    # -- actions ----------------------------------------------------------
    def on_start(self):
        if self.state != "idle":
            return
        targets = self.selected_targets()
        if not targets:
            messagebox.showerror(APP_NAME, "Tick at least one screen to record.")
            return
        outdir = self.dir_var.get().strip()
        try:
            os.makedirs(outdir, exist_ok=True)
        except Exception as exc:
            messagebox.showerror(APP_NAME, "Cannot use that folder:\n%s" % exc)
            return

        self._encoder_ready.wait(timeout=20)
        audio = self.audio_var.get()
        self.rec = MultiRecorder(
            ffmpeg=self.ffmpeg,
            targets=targets,
            fps=int(self.fps_var.get()),
            audio_device=None if audio == "None" else audio,
            opts={"cursor": self.cursor_var.get(), "zoom": self.zoom_var.get(),
                  "trail": self.trail_var.get(), "clicks": self.click_var.get()},
            on_error=self.errors.put,
            encoder=self.encoder,
            camera=self.camera,
        )
        self.rec.start()
        self.state = "recording"
        self._sync_ui()
        if self.minimize_var.get():
            self.master.iconify()
            if self.camera_overlay:
                self.after(250, self.camera_overlay.raise_above)

    def on_pause(self):
        if self.state == "recording":
            self.rec.pause()
            self.state = "paused"
        elif self.state == "paused":
            self.rec.resume()
            self.state = "recording"
        else:
            return
        self._sync_ui()

    def on_stop(self):
        if self.state == "idle" or not self.rec:
            return
        self.master.deiconify()
        self.state = "idle"
        rec, self.rec = self.rec, None
        self.status.config(text="Saving...", foreground="#666666")
        for b in (self.start_btn, self.pause_btn, self.stop_btn):
            b.state(["disabled"])
        self.update_idletasks()

        rec.stop()
        dest, is_session, note = None, False, ""
        try:
            dest, is_session = rec.save(self.dir_var.get().strip())
        except PartialSave as exc:
            dest, is_session = exc.folder, True
            note = "\n\nThese screens did not save:\n" + "\n".join(exc.failures)
        except Exception as exc:
            messagebox.showerror(APP_NAME,
                                 "Could not save the recording:\n%s" % exc)
        finally:
            rec.cleanup()
        self._sync_ui()
        if not dest:
            self.status.config(text="Save failed")
            return

        n = len(rec.recorders)
        self.status.config(text="Saved  %s" % os.path.basename(dest))
        what = ("%d screens to\n%s" % (n, dest)) if is_session else ("Saved to:\n%s" % dest)
        if messagebox.askyesno(APP_NAME, what + note + "\n\nOpen the folder?"):
            subprocess.Popen(["explorer", "/select,", os.path.normpath(dest)])

    # -- loops ------------------------------------------------------------
    def _sync_ui(self):
        busy = self.state in ("recording", "paused")
        self.start_btn.state(["disabled"] if busy else ["!disabled"])
        self.pause_btn.state(["!disabled"] if busy else ["disabled"])
        self.stop_btn.state(["!disabled"] if busy else ["disabled"])
        self.pause_btn.config(text="Resume" if self.state == "paused" else "Pause")
        for w in (self.audio_box, self.fps_box, self.cam_box, self.cam_size_box):
            w.config(state="disabled" if busy else "readonly")
        self.cam_check.state(["disabled"] if busy else ["!disabled"])
        picked_desktop = self.desktop_var.get()
        for w in self.screens_frame.winfo_children():
            if isinstance(w, ttk.Checkbutton):
                # the desktop option and the per-screen list are exclusive
                is_desktop = "desktop" in str(w.cget("text")).lower()
                off = busy or (picked_desktop and not is_desktop)
                w.state(["disabled"] if off else ["!disabled"])
        for w in self.fx_frame.winfo_children():
            if isinstance(w, ttk.Checkbutton):
                w.state(["disabled"] if busy else ["!disabled"])
        n = len(self.rec.recorders) if self.rec else 0
        suffix = "  -  %d screens" % n if n > 1 else ""
        if self.state == "recording":
            self.status.config(text="Recording" + suffix, foreground="#c0392b")
        elif self.state == "paused":
            self.status.config(text="Paused" + suffix, foreground="#b7791f")
        else:
            self.status.config(foreground="#666666")
            self.timer.config(text="00:00:00")

    def _tick(self):
        if self.rec and self.state in ("recording", "paused"):
            s = int(self.rec.duration())
            clock = "%02d:%02d:%02d" % (s // 3600, s % 3600 // 60, s % 60)
            self.timer.config(text=clock)
            label = "Recording" if self.state == "recording" else "Paused"
            self.master.title("%s %s - %s" % (label, clock, APP_NAME))
        else:
            self.master.title(APP_NAME)
        try:
            while True:
                msg = self.errors.get_nowait()
                self.master.deiconify()
                messagebox.showerror(APP_NAME, msg)
        except queue.Empty:
            pass
        self.after(200, self._tick)

    def _poll_hotkeys(self):
        """Global hotkeys via key-state polling, so they work while unfocused."""
        gk = ctypes.windll.user32.GetAsyncKeyState
        if (gk(0x11) & 0x8000) and (gk(0x12) & 0x8000):  # Ctrl + Alt
            if gk(0x52) & 1:  # R
                self.on_start() if self.state == "idle" else self.on_stop()
            elif gk(0x50) & 1:  # P
                self.on_pause()
        else:
            gk(0x52)  # drain the pressed-since-last-call bits
            gk(0x50)
        self.after(120, self._poll_hotkeys)


def main():
    make_dpi_aware()
    ffmpeg = shutil.which("ffmpeg")
    if not ffmpeg:
        root = tk.Tk()
        root.withdraw()
        messagebox.showerror(APP_NAME, "ffmpeg was not found on your PATH.")
        return 1

    root = tk.Tk()
    root.title(APP_NAME)
    root.resizable(False, False)
    try:
        ttk.Style().theme_use("vista")
    except tk.TclError:
        pass
    app = App(root, ffmpeg)

    root.update_idletasks()
    try:  # scale the UI to the DPI of the display it opened on
        dpi = ctypes.windll.user32.GetDpiForWindow(root.winfo_id()) or 96
        if dpi != 96:
            root.tk.call("tk", "scaling", dpi / 72.0)
            root.geometry("")
    except Exception:
        pass

    def on_close():
        if app.state != "idle" and not messagebox.askokcancel(
                APP_NAME, "A recording is in progress. Discard it and quit?"):
            return
        if app.rec:
            app.rec.stop()
            app.rec.cleanup()
        app.stop_camera()
        root.destroy()

    root.protocol("WM_DELETE_WINDOW", on_close)
    root.mainloop()
    return 0


if __name__ == "__main__":
    sys.exit(main())
