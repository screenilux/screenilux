//! The global pointer hook, and the shortcuts pressed.
//!
//! It lives on a thread of its own because a `WH_MOUSE_LL` hook is only
//! serviced while its installing thread pumps messages. A `WH_KEYBOARD_LL`
//! hook on the same thread sees keys, and passes on only those that make a
//! shortcut (see `screenilux_core::keys`): ordinary typing never leaves it.
//!
//! The hook callback does nothing but decode and enqueue. Windows silently
//! removes a low-level hook whose callback is too slow to return, which would
//! disable zoom and click effects mid-recording with no error anywhere.

use std::sync::Mutex;
use std::sync::atomic::{AtomicU32, Ordering};
use std::thread::JoinHandle;

use crossbeam_channel::{Receiver, Sender, unbounded};
use screenilux_core::geometry::Point;
use screenilux_core::keys::{self, Key};
use screenilux_core::ports::{PointerEvent, PointerSource};
use windows::Win32::Foundation::{LPARAM, LRESULT, POINT, WPARAM};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetAsyncKeyState, VK_CONTROL, VK_LWIN, VK_MENU, VK_RWIN, VK_SHIFT,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, DispatchMessageW, GetCursorPos, GetMessageW, HHOOK, KBDLLHOOKSTRUCT,
    KBDLLHOOKSTRUCT_FLAGS, MSG, MSLLHOOKSTRUCT, PostThreadMessageW, SetWindowsHookExW,
    TranslateMessage, UnhookWindowsHookEx, WH_KEYBOARD_LL, WH_MOUSE_LL, WM_KEYDOWN, WM_LBUTTONDOWN,
    WM_MBUTTONDOWN, WM_MOUSEWHEEL, WM_QUIT, WM_RBUTTONDOWN, WM_SYSKEYDOWN,
};

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

fn held(vk: u16) -> bool {
    // SAFETY: reads a process-global key state; no pointers involved.
    unsafe { (GetAsyncKeyState(i32::from(vk)) as u16 & 0x8000) != 0 }
}

/// The modifiers held now, as `keys` counts them.
fn modifiers() -> u8 {
    let mut m = 0;
    if held(VK_CONTROL.0) {
        m |= keys::CTRL;
    }
    if held(VK_MENU.0) {
        m |= keys::ALT;
    }
    if held(VK_SHIFT.0) {
        m |= keys::SHIFT;
    }
    if held(VK_LWIN.0) || held(VK_RWIN.0) {
        m |= keys::SUPER;
    }
    m
}

/// The key a virtual-key code stands for, if it is one a shortcut can end
/// in. Modifiers themselves are not.
fn key_of(vk: u32) -> Option<Key> {
    Some(match vk {
        0x41..=0x5A => Key::Letter(char::from(vk as u8)),
        0x30..=0x39 => Key::Digit((vk - 0x30) as u8),
        0x70..=0x87 => Key::F((vk - 0x70 + 1) as u8),
        0x0D => Key::Enter,
        0x1B => Key::Escape,
        0x09 => Key::Tab,
        0x20 => Key::Space,
        0x08 => Key::Backspace,
        0x2E => Key::Delete,
        0x25 => Key::Left,
        0x26 => Key::Up,
        0x27 => Key::Right,
        0x28 => Key::Down,
        0x24 => Key::Home,
        0x23 => Key::End,
        0x21 => Key::PageUp,
        0x22 => Key::PageDown,
        0xBB => Key::Symbol('+'),
        0xBD => Key::Symbol('-'),
        0xBC => Key::Symbol(','),
        0xBE => Key::Symbol('.'),
        0xBF => Key::Symbol('/'),
        0xBA => Key::Symbol(';'),
        0xDB => Key::Symbol('['),
        0xDD => Key::Symbol(']'),
        0xDC => Key::Symbol('\\'),
        0xDE => Key::Symbol('\''),
        0xC0 => Key::Symbol('`'),
        _ => return None,
    })
}

/// The low-level keyboard hook. It never swallows a key, and only a key
/// that makes a shortcut goes any further than this function.
unsafe extern "system" fn key_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code == 0 && matches!(wparam.0 as u32, WM_KEYDOWN | WM_SYSKEYDOWN) {
        let info = lparam.0 as *const KBDLLHOOKSTRUCT;
        if !info.is_null() {
            // SAFETY: for code == 0 Windows guarantees lparam points at a
            // live KBDLLHOOKSTRUCT for the duration of this call.
            let vk = unsafe { (*info).vkCode };
            if let Some(key) = key_of(vk) {
                let mods = modifiers();
                if keys::shortcut(mods, key).is_some() {
                    emit(PointerEvent::Key { mods, key });
                }
            }
        }
    }
    // SAFETY: passing the event along is required of every hook.
    unsafe { CallNextHookEx(None, code, wparam, lparam) }
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
}

impl InputHook {
    /// Install the hook.
    pub fn start() -> Self {
        let (ptx, pointer_rx) = unbounded();
        *POINTER_SINK.lock().expect("sink poisoned") = Some(ptx);

        let ready = std::sync::Arc::new((Mutex::new(false), std::sync::Condvar::new()));
        let signal = std::sync::Arc::clone(&ready);
        let tid: ThreadId = std::sync::Arc::new(AtomicU32::new(0));
        let tid_for_pump = std::sync::Arc::clone(&tid);

        let thread = std::thread::Builder::new()
            .name("screenilux-input".into())
            .spawn(move || pump(signal, tid_for_pump))
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
        }
    }

    /// A handle on the pointer stream.
    ///
    /// The app owns one hook for its whole life and hands this to each
    /// recording session. Crossbeam receivers are multi-consumer,
    /// so events are split rather than duplicated; only one consumer should
    /// drain pointer events at a time.
    pub fn pointer_events(&self) -> Receiver<PointerEvent> {
        self.pointer_rx.clone()
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
fn pump(ready: std::sync::Arc<(Mutex<bool>, std::sync::Condvar)>, tid_out: ThreadId) {
    // SAFETY: installing a thread-local-scoped global hook with a 'static fn.
    let hook = unsafe { SetWindowsHookExW(WH_MOUSE_LL, Some(hook_proc), None, 0) };
    let hook: HHOOK = match hook {
        Ok(h) => h,
        Err(e) => {
            tracing::error!(error = %e, "could not install the mouse hook; zoom and click effects are unavailable");
            HHOOK::default()
        }
    };
    // SAFETY: as above, for the keyboard.
    let key_hook = unsafe { SetWindowsHookExW(WH_KEYBOARD_LL, Some(key_proc), None, 0) };
    let key_hook: HHOOK = match key_hook {
        Ok(h) => h,
        Err(e) => {
            tracing::warn!(error = %e, "could not install the keyboard hook; shortcuts are not shown");
            HHOOK::default()
        }
    };

    let tid = unsafe { windows::Win32::System::Threading::GetCurrentThreadId() };
    tid_out.store(tid, Ordering::SeqCst);

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
        unsafe {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }

    // SAFETY: removing the hooks this thread installed.
    unsafe {
        if !hook.is_invalid() {
            let _ = UnhookWindowsHookEx(hook);
        }
        if !key_hook.is_invalid() {
            let _ = UnhookWindowsHookEx(key_hook);
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
