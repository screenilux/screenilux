//! The global pointer hook and the recording hotkeys.
//!
//! Both live on one thread because both need a Win32 message pump: a
//! `WH_MOUSE_LL` hook is only serviced while its installing thread pumps
//! messages, and `RegisterHotKey` posts `WM_HOTKEY` to that same queue.
//!
//! The hook callback does nothing but decode and enqueue. Windows silently
//! removes a low-level hook whose callback is too slow to return, which would
//! disable zoom and click effects mid-recording with no error anywhere.

use std::sync::Mutex;
use std::sync::atomic::{AtomicU32, Ordering};
use std::thread::JoinHandle;

use crossbeam_channel::{Receiver, Sender, unbounded};
use screenilux_core::geometry::Point;
use screenilux_core::ports::{PointerEvent, PointerSource};
use windows::Win32::Foundation::{LPARAM, LRESULT, POINT, WPARAM};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetAsyncKeyState, HOT_KEY_MODIFIERS, MOD_ALT, MOD_CONTROL, MOD_NOREPEAT, RegisterHotKey,
    UnregisterHotKey, VK_CONTROL, VK_MENU, VK_P, VK_R,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, DispatchMessageW, GetCursorPos, GetMessageW, HHOOK, KBDLLHOOKSTRUCT_FLAGS, MSG,
    MSLLHOOKSTRUCT, PostThreadMessageW, SetWindowsHookExW, TranslateMessage, UnhookWindowsHookEx,
    WH_MOUSE_LL, WM_HOTKEY, WM_LBUTTONDOWN, WM_MBUTTONDOWN, WM_MOUSEWHEEL, WM_QUIT, WM_RBUTTONDOWN,
};

/// A recording command triggered by a global hotkey.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Hotkey {
    StartStop,
    PauseResume,
}

const HOTKEY_START_STOP: i32 = 1;
const HOTKEY_PAUSE_RESUME: i32 = 2;

/// Where the hook callback posts what it sees.
///
/// A static because `SetWindowsHookEx` callbacks cannot carry user data. Only
/// one hook exists per process, so this is not a general-purpose registry.
static POINTER_SINK: Mutex<Option<Sender<PointerEvent>>> = Mutex::new(None);
/// Thread id of a pump, shared with the handle that owns it so `stop` can
/// break its message loop. Per-instance rather than global: a global would let
/// one handle post WM_QUIT to a different handle's thread, leaving the first
/// blocked forever in `join`.
type ThreadId = std::sync::Arc<AtomicU32>;

fn ctrl_alt_held() -> bool {
    // SAFETY: reads a process-global key state; no pointers involved.
    unsafe {
        (GetAsyncKeyState(VK_CONTROL.0 as i32) as u16 & 0x8000) != 0
            && (GetAsyncKeyState(VK_MENU.0 as i32) as u16 & 0x8000) != 0
    }
}

fn emit(event: PointerEvent) {
    if let Ok(guard) = POINTER_SINK.lock()
        && let Some(tx) = guard.as_ref()
    {
        let _ = tx.send(event);
    }
}

/// The low-level mouse hook.
///
/// Returning 1 swallows the event. Only the two zoom gestures are swallowed —
/// everything else is passed on, so ordinary scrolling and clicking reach
/// whatever is being recorded untouched.
unsafe extern "system" fn hook_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code == 0 {
        let info = lparam.0 as *const MSLLHOOKSTRUCT;
        if !info.is_null() {
            // SAFETY: for code == 0 Windows guarantees lparam points at a
            // live MSLLHOOKSTRUCT for the duration of this call.
            let info = unsafe { &*info };
            match wparam.0 as u32 {
                WM_MOUSEWHEEL if ctrl_alt_held() => {
                    // the wheel delta is the signed high word of mouseData
                    let delta = ((info.mouseData >> 16) & 0xFFFF) as u16 as i16;
                    emit(PointerEvent::Wheel {
                        steps: f64::from(delta) / 120.0,
                    });
                    return LRESULT(1);
                }
                WM_MBUTTONDOWN if ctrl_alt_held() => {
                    emit(PointerEvent::Reset);
                    return LRESULT(1);
                }
                WM_LBUTTONDOWN | WM_RBUTTONDOWN => {
                    emit(PointerEvent::Click {
                        at: Point::new(info.pt.x, info.pt.y),
                    });
                }
                _ => {}
            }
        }
    }
    // SAFETY: passing the event along is required of every hook.
    unsafe { CallNextHookEx(None, code, wparam, lparam) }
}

/// Owns the hook thread; drop or [`stop`](InputHook::stop) to uninstall.
pub struct InputHook {
    thread: Option<JoinHandle<()>>,
    tid: ThreadId,
    pointer_rx: Receiver<PointerEvent>,
    hotkey_rx: Receiver<Hotkey>,
}

impl InputHook {
    /// Install the hook and register the hotkeys.
    pub fn start() -> Self {
        let (ptx, pointer_rx) = unbounded();
        let (htx, hotkey_rx) = unbounded();
        *POINTER_SINK.lock().expect("sink poisoned") = Some(ptx);

        let ready = std::sync::Arc::new((Mutex::new(false), std::sync::Condvar::new()));
        let signal = std::sync::Arc::clone(&ready);
        let tid: ThreadId = std::sync::Arc::new(AtomicU32::new(0));
        let tid_for_pump = std::sync::Arc::clone(&tid);

        let thread = std::thread::Builder::new()
            .name("screenilux-input".into())
            .spawn(move || pump(htx, signal, tid_for_pump))
            .expect("failed to spawn the input thread");

        // Wait for the pump to publish its thread id, so an immediate stop
        // cannot race past it and leave the hook installed.
        let (lock, cv) = &*ready;
        let mut started = lock.lock().expect("ready poisoned");
        while !*started {
            let (next, timeout) = cv
                .wait_timeout(started, std::time::Duration::from_secs(2))
                .expect("ready poisoned");
            started = next;
            if timeout.timed_out() {
                break;
            }
        }

        Self {
            thread: Some(thread),
            tid,
            pointer_rx,
            hotkey_rx,
        }
    }

    pub fn hotkeys(&self) -> &Receiver<Hotkey> {
        &self.hotkey_rx
    }

    pub fn stop(&mut self) {
        let tid = self.tid.swap(0, Ordering::SeqCst);
        if tid != 0 {
            // SAFETY: posting WM_QUIT to a thread id we published ourselves.
            unsafe {
                let _ = PostThreadMessageW(tid, WM_QUIT, WPARAM(0), LPARAM(0));
            }
        }
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
        *POINTER_SINK.lock().expect("sink poisoned") = None;
    }
}

impl Drop for InputHook {
    fn drop(&mut self) {
        self.stop();
    }
}

impl PointerSource for InputHook {
    fn position(&self) -> Point {
        cursor_pos()
    }

    fn drain(&mut self, out: &mut Vec<PointerEvent>) {
        out.extend(self.pointer_rx.try_iter());
    }
}

/// Where the pointer is on the virtual desktop.
pub fn cursor_pos() -> Point {
    let mut p = POINT::default();
    // SAFETY: writes through a pointer to a local we own.
    let ok = unsafe { GetCursorPos(&mut p) };
    if ok.is_err() {
        return Point::new(0, 0);
    }
    Point::new(p.x, p.y)
}

/// The hook thread: install, pump, clean up.
fn pump(
    hotkeys: Sender<Hotkey>,
    ready: std::sync::Arc<(Mutex<bool>, std::sync::Condvar)>,
    tid_out: ThreadId,
) {
    // SAFETY: installing a thread-local-scoped global hook with a 'static fn.
    let hook = unsafe { SetWindowsHookExW(WH_MOUSE_LL, Some(hook_proc), None, 0) };
    let hook: HHOOK = match hook {
        Ok(h) => h,
        Err(e) => {
            tracing::error!(error = %e, "could not install the mouse hook; zoom and click effects are unavailable");
            HHOOK::default()
        }
    };

    let tid = unsafe { windows::Win32::System::Threading::GetCurrentThreadId() };
    tid_out.store(tid, Ordering::SeqCst);

    // Registering rather than polling key state: Windows delivers an edge per
    // press, so a fast tap cannot be missed and nothing races on the shared
    // "pressed since last read" bit that GetAsyncKeyState clears process-wide.
    let modifiers: HOT_KEY_MODIFIERS = MOD_CONTROL | MOD_ALT | MOD_NOREPEAT;
    // SAFETY: registering against the calling thread (no window handle).
    unsafe {
        if RegisterHotKey(None, HOTKEY_START_STOP, modifiers, VK_R.0 as u32).is_err() {
            tracing::warn!("Ctrl+Alt+R is taken by another application");
        }
        if RegisterHotKey(None, HOTKEY_PAUSE_RESUME, modifiers, VK_P.0 as u32).is_err() {
            tracing::warn!("Ctrl+Alt+P is taken by another application");
        }
    }

    {
        let (lock, cv) = &*ready;
        *lock.lock().expect("ready poisoned") = true;
        cv.notify_all();
    }

    let mut msg = MSG::default();
    // GetMessageW returns 0 for WM_QUIT and -1 on error. Testing `.as_bool()`
    // alone would treat -1 as "keep going" and spin forever.
    loop {
        // SAFETY: standard message loop over this thread's own queue.
        let got = unsafe { GetMessageW(&mut msg, None, 0, 0) }.0;
        if got <= 0 {
            break;
        }
        if msg.message == WM_HOTKEY {
            let which = match msg.wParam.0 as i32 {
                HOTKEY_START_STOP => Some(Hotkey::StartStop),
                HOTKEY_PAUSE_RESUME => Some(Hotkey::PauseResume),
                _ => None,
            };
            if let Some(h) = which {
                let _ = hotkeys.send(h);
            }
        }
        unsafe {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }

    // SAFETY: unregistering what this thread registered.
    unsafe {
        let _ = UnregisterHotKey(None, HOTKEY_START_STOP);
        let _ = UnregisterHotKey(None, HOTKEY_PAUSE_RESUME);
        if !hook.is_invalid() {
            let _ = UnhookWindowsHookEx(hook);
        }
    }
    tid_out.store(0, Ordering::SeqCst);
}

/// Unused import guard: `KBDLLHOOKSTRUCT_FLAGS` is pulled in by the windows
/// feature set but not needed here.
#[allow(dead_code)]
type _Unused = KBDLLHOOKSTRUCT_FLAGS;

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    /// Only one hook exists in a real process, so exercise them one at a time
    /// rather than letting the harness run these in parallel.
    static ONE_AT_A_TIME: Mutex<()> = Mutex::new(());

    fn exclusive() -> std::sync::MutexGuard<'static, ()> {
        ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner())
    }

    #[test]
    fn cursor_position_is_on_the_virtual_desktop() {
        crate::display::set_dpi_awareness();
        let p = cursor_pos();
        let Ok(monitors) = crate::display::monitors() else {
            return;
        };
        let all = screenilux_core::geometry::virtual_bounds(&monitors).unwrap();
        // generous: the pointer can legitimately sit on a seam
        assert!(
            p.x >= all.left - 1 && p.x <= all.right() + 1,
            "x={} not in {all:?}",
            p.x
        );
        assert!(
            p.y >= all.top - 1 && p.y <= all.bottom() + 1,
            "y={} not in {all:?}",
            p.y
        );
    }

    #[test]
    fn the_hook_installs_and_uninstalls_cleanly() {
        let _g = exclusive();
        let mut hook = InputHook::start();
        assert_ne!(
            hook.tid.load(Ordering::SeqCst),
            0,
            "the pump should have published its thread id"
        );
        hook.stop();
        assert_eq!(hook.tid.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn stopping_is_prompt_rather_than_waiting_on_input() {
        let _g = exclusive();
        let mut hook = InputHook::start();
        let start = std::time::Instant::now();
        hook.stop();
        assert!(
            start.elapsed() < Duration::from_secs(2),
            "stop took {:?}; the pump must exit on WM_QUIT, not on the next mouse move",
            start.elapsed()
        );
    }

    #[test]
    fn dropping_uninstalls_without_an_explicit_stop() {
        let _g = exclusive();
        let watch;
        {
            let hook = InputHook::start();
            watch = std::sync::Arc::clone(&hook.tid);
            assert_ne!(watch.load(Ordering::SeqCst), 0);
        }
        assert_eq!(watch.load(Ordering::SeqCst), 0, "drop must uninstall");
    }

    #[test]
    fn draining_an_idle_hook_yields_nothing_and_does_not_block() {
        let _g = exclusive();
        let mut hook = InputHook::start();
        let mut events = Vec::new();
        let start = std::time::Instant::now();
        hook.drain(&mut events);
        assert!(
            start.elapsed() < Duration::from_millis(200),
            "drain must not block"
        );
        hook.stop();
    }

    #[test]
    fn restarting_the_hook_works() {
        let _g = exclusive();
        for _ in 0..3 {
            let mut hook = InputHook::start();
            assert_ne!(hook.tid.load(Ordering::SeqCst), 0);
            hook.stop();
        }
    }

    #[test]
    fn wheel_delta_decodes_with_sign() {
        // the high word of mouseData, as a signed short
        let decode = |raw: u32| -> f64 {
            let d = ((raw >> 16) & 0xFFFF) as u16 as i16;
            f64::from(d) / 120.0
        };
        // Windows packs the delta into the *high word*, so build the input the
        // same way: widen the signed short, then shift.
        let packed = |delta: i16| ((delta as u16) as u32) << 16;
        assert_eq!(decode(packed(120)), 1.0);
        assert_eq!(decode(packed(240)), 2.0);
        assert_eq!(
            decode(packed(-120)),
            -1.0,
            "wheel-down must come back negative"
        );
        assert_eq!(decode(packed(-360)), -3.0);
    }
}
