//! Hearing about display changes as they happen.
//!
//! Windows tells every top-level window when a display is connected,
//! disconnected or rearranged (`WM_DISPLAYCHANGE`), and when the taskbar
//! moves or resizes (`WM_SETTINGCHANGE` for the work area). A message-only
//! window would not hear these broadcasts, so this makes a hidden top-level
//! window on a thread of its own and pumps its messages.

use std::cell::RefCell;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::thread::JoinHandle;

use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Threading::GetCurrentThreadId;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GetMessageW, MSG,
    PostThreadMessageW, RegisterClassW, SPI_SETWORKAREA, WINDOW_EX_STYLE, WM_DISPLAYCHANGE,
    WM_QUIT, WM_SETTINGCHANGE, WNDCLASSW, WS_OVERLAPPED,
};
use windows::core::w;

thread_local! {
    /// The callback for the window on this thread. Window procedures carry
    /// no user data of their own, and each watcher has its own thread.
    static ON_CHANGE: RefCell<Option<Box<dyn Fn()>>> = const { RefCell::new(None) };
}

unsafe extern "system" fn window_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    let changed = msg == WM_DISPLAYCHANGE
        || (msg == WM_SETTINGCHANGE && wparam.0 as u32 == SPI_SETWORKAREA.0);
    if changed {
        ON_CHANGE.with(|f| {
            if let Some(f) = f.borrow().as_ref() {
                f();
            }
        });
    }
    // SAFETY: every message is passed on to the default handling.
    unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
}

/// Calls back whenever the displays change, until dropped.
pub struct DisplayWatcher {
    thread: Option<JoinHandle<()>>,
    tid: Arc<AtomicU32>,
}

impl DisplayWatcher {
    /// Start listening. `on_change` runs on the watcher's own thread, so it
    /// should only signal, not do the work.
    pub fn start(on_change: impl Fn() + Send + 'static) -> Self {
        let tid = Arc::new(AtomicU32::new(0));
        let published = Arc::clone(&tid);
        let (ready_tx, ready_rx) = std::sync::mpsc::channel();
        let thread = std::thread::Builder::new()
            .name("screenilux-displays".into())
            .spawn(move || {
                ON_CHANGE.with(|f| *f.borrow_mut() = Some(Box::new(on_change)));
                // SAFETY: registering a class and creating a hidden window
                // owned by this thread, then pumping its messages until
                // WM_QUIT. A class already registered by an earlier watcher
                // is reused.
                unsafe {
                    published.store(GetCurrentThreadId(), Ordering::SeqCst);
                    let instance = GetModuleHandleW(None).unwrap_or_default();
                    let class = WNDCLASSW {
                        lpfnWndProc: Some(window_proc),
                        hInstance: instance.into(),
                        lpszClassName: w!("ScreeniluxDisplayWatcher"),
                        ..Default::default()
                    };
                    RegisterClassW(&class);
                    let hwnd = CreateWindowExW(
                        WINDOW_EX_STYLE(0),
                        w!("ScreeniluxDisplayWatcher"),
                        w!("Screenilux displays"),
                        WS_OVERLAPPED,
                        0,
                        0,
                        0,
                        0,
                        None,
                        None,
                        Some(instance.into()),
                        None,
                    );
                    let _ = ready_tx.send(());
                    let Ok(hwnd) = hwnd else {
                        tracing::warn!("could not listen for display changes");
                        return;
                    };
                    let mut msg = MSG::default();
                    while GetMessageW(&mut msg, None, 0, 0).0 > 0 {
                        DispatchMessageW(&msg);
                    }
                    let _ = DestroyWindow(hwnd);
                }
            })
            .ok();
        // Wait for the thread id, so dropping straight away still stops it.
        let _ = ready_rx.recv_timeout(std::time::Duration::from_secs(2));
        Self { thread, tid }
    }
}

impl Drop for DisplayWatcher {
    fn drop(&mut self) {
        let tid = self.tid.swap(0, Ordering::SeqCst);
        if tid != 0 {
            // SAFETY: posting WM_QUIT to the watcher's own thread.
            unsafe {
                let _ = PostThreadMessageW(tid, WM_QUIT, WPARAM(0), LPARAM(0));
            }
        }
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;
    use windows::Win32::UI::WindowsAndMessaging::{
        HWND_BROADCAST, SMTO_ABORTIFHUNG, SendMessageTimeoutW,
    };

    #[test]
    fn a_display_change_is_heard_and_dropping_stops_listening() {
        let heard = Arc::new(AtomicUsize::new(0));
        let count = Arc::clone(&heard);
        let watcher = DisplayWatcher::start(move || {
            count.fetch_add(1, Ordering::SeqCst);
        });
        // What Windows sends when the work area changes, delivered the same
        // way: broadcast to every top-level window.
        // SAFETY: a broadcast with a timeout, so a hung window cannot block.
        unsafe {
            let _ = SendMessageTimeoutW(
                HWND_BROADCAST,
                WM_SETTINGCHANGE,
                WPARAM(SPI_SETWORKAREA.0 as usize),
                LPARAM(0),
                SMTO_ABORTIFHUNG,
                2000,
                None,
            );
        }
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
        while heard.load(Ordering::SeqCst) == 0 && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        assert!(heard.load(Ordering::SeqCst) > 0, "the change was not heard");

        let start = std::time::Instant::now();
        drop(watcher);
        assert!(start.elapsed() < std::time::Duration::from_secs(2));
    }
}
