//! Clicks, the zoom wheel and shortcuts anywhere on the desktop, through
//! XInput 2. Of the keys, only those that make a shortcut (see
//! `screenilux_core::keys`) go any further: ordinary typing never leaves
//! this module.
//!
//! Raw button events reach every client that asks for them on the root
//! window, whichever window is under the pointer. Unlike on Windows they
//! cannot be swallowed, so Ctrl+Alt+wheel still scrolls whatever is under
//! the pointer while it zooms the recording.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;
use std::time::Duration;

use crossbeam_channel::{Receiver, Sender, unbounded};
use screenilux_core::geometry::Point;
use screenilux_core::keys::{self, Key};
use screenilux_core::ports::{PointerEvent, PointerSource};
use x11rb::connection::Connection;
use x11rb::protocol::Event;
use x11rb::protocol::xinput::{self, ConnectionExt as _};
use x11rb::protocol::xproto::{ConnectionExt as _, KeyButMask};

use crate::x11::{self, Display};

/// Owns the listening thread; drop or [`stop`](InputHook::stop) to end it.
pub struct InputHook {
    thread: Option<JoinHandle<()>>,
    stop: Arc<AtomicBool>,
    pointer_rx: Receiver<PointerEvent>,
}

impl InputHook {
    /// Start listening. Without an X server or XInput 2 the hook stays quiet
    /// and zoom and click effects simply do not fire.
    pub fn start() -> Self {
        let (tx, pointer_rx) = unbounded();
        let stop = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&stop);
        let thread = std::thread::Builder::new()
            .name("screenilux-input".into())
            .spawn(move || {
                if let Err(e) = listen(&flag, &tx) {
                    tracing::warn!(error = %e, "no global clicks; zoom and click effects are unavailable");
                }
            })
            .ok();
        Self {
            thread,
            stop,
            pointer_rx,
        }
    }

    /// A handle on the pointer stream, as on Windows.
    pub fn pointer_events(&self) -> Receiver<PointerEvent> {
        self.pointer_rx.clone()
    }

    pub fn stop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
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

/// Where the pointer is on the desktop.
pub fn cursor_pos() -> Point {
    let Some(guard) = x11::shared() else {
        return Point::new(0, 0);
    };
    let Some(d) = guard.as_ref() else {
        return Point::new(0, 0);
    };
    pointer(d).map(|(p, _)| p).unwrap_or(Point::new(0, 0))
}

fn pointer(d: &Display) -> Option<(Point, KeyButMask)> {
    let r = d.conn.query_pointer(d.root).ok()?.reply().ok()?;
    Some((Point::new(r.root_x as i32, r.root_y as i32), r.mask))
}

/// What a button press means, given whether Ctrl and Alt are held.
fn meaning(button: u32, ctrl_alt: bool, at: Point) -> Option<PointerEvent> {
    match (button, ctrl_alt) {
        (1 | 3, _) => Some(PointerEvent::Click { at }),
        (2, true) => Some(PointerEvent::Reset),
        // X11 reports the wheel as buttons 4 (away) and 5 (towards).
        (4, true) => Some(PointerEvent::Wheel { steps: 1.0 }),
        (5, true) => Some(PointerEvent::Wheel { steps: -1.0 }),
        _ => None,
    }
}

/// The modifiers held, as `keys` counts them, from the pointer's mask.
fn modifiers(mask: KeyButMask) -> u8 {
    let mut m = 0;
    if mask.contains(KeyButMask::CONTROL) {
        m |= keys::CTRL;
    }
    if mask.contains(KeyButMask::MOD1) {
        m |= keys::ALT;
    }
    if mask.contains(KeyButMask::SHIFT) {
        m |= keys::SHIFT;
    }
    if mask.contains(KeyButMask::MOD4) {
        m |= keys::SUPER;
    }
    m
}

/// The key a keysym stands for, if a shortcut can end in it.
fn key_of(sym: u32) -> Option<Key> {
    Some(match sym {
        0x61..=0x7A => Key::Letter(char::from(sym as u8)),
        0x41..=0x5A => Key::Letter(char::from(sym as u8)),
        0x30..=0x39 => Key::Digit((sym - 0x30) as u8),
        0xFFBE..=0xFFD5 => Key::F((sym - 0xFFBE + 1) as u8),
        0xFF0D => Key::Enter,
        0xFF1B => Key::Escape,
        0xFF09 => Key::Tab,
        0x20 => Key::Space,
        0xFF08 => Key::Backspace,
        0xFFFF => Key::Delete,
        0xFF51 => Key::Left,
        0xFF52 => Key::Up,
        0xFF53 => Key::Right,
        0xFF54 => Key::Down,
        0xFF50 => Key::Home,
        0xFF57 => Key::End,
        0xFF55 => Key::PageUp,
        0xFF56 => Key::PageDown,
        0x21..=0x7E => Key::Symbol(char::from(sym as u8)),
        _ => return None,
    })
}

fn listen(stop: &AtomicBool, tx: &Sender<PointerEvent>) -> Result<(), String> {
    let d = x11::connect()?;
    d.conn
        .xinput_xi_query_version(2, 0)
        .map_err(|e| e.to_string())?
        .reply()
        .map_err(|e| format!("XInput 2 is not available: {e}"))?;
    d.conn
        .xinput_xi_select_events(
            d.root,
            &[xinput::EventMask {
                deviceid: xinput::Device::ALL_MASTER.into(),
                mask: vec![
                    xinput::XIEventMask::RAW_BUTTON_PRESS | xinput::XIEventMask::RAW_KEY_PRESS,
                ],
            }],
        )
        .map_err(|e| e.to_string())?
        .check()
        .map_err(|e| e.to_string())?;
    d.conn.flush().map_err(|e| e.to_string())?;

    // Which keysym each keycode types, unshifted, read once.
    let setup = d.conn.setup();
    let (first, last) = (setup.min_keycode, setup.max_keycode);
    let mapping = d
        .conn
        .get_keyboard_mapping(first, last - first + 1)
        .map_err(|e| e.to_string())?
        .reply()
        .map_err(|e| e.to_string())?;
    let per = usize::from(mapping.keysyms_per_keycode.max(1));
    let keysym = |code: u32| -> u32 {
        let i = (code as usize).saturating_sub(usize::from(first)) * per;
        mapping.keysyms.get(i).copied().unwrap_or(0)
    };

    while !stop.load(Ordering::Relaxed) {
        match d.conn.poll_for_event().map_err(|e| e.to_string())? {
            Some(Event::XinputRawButtonPress(ev)) => {
                let Some((at, mask)) = pointer(&d) else {
                    continue;
                };
                let ctrl_alt =
                    mask.contains(KeyButMask::CONTROL) && mask.contains(KeyButMask::MOD1);
                if let Some(event) = meaning(ev.detail, ctrl_alt, at) {
                    let _ = tx.send(event);
                }
            }
            Some(Event::XinputRawKeyPress(ev)) => {
                let Some(key) = key_of(keysym(ev.detail)) else {
                    continue;
                };
                let mods = pointer(&d).map_or(0, |(_, mask)| modifiers(mask));
                if keys::shortcut(mods, key).is_some() {
                    let _ = tx.send(PointerEvent::Key { mods, key });
                }
            }
            Some(_) => {}
            None => std::thread::sleep(Duration::from_millis(5)),
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn buttons_mean_what_they_do_on_windows() {
        let at = Point::new(3, 4);
        assert_eq!(meaning(1, false, at), Some(PointerEvent::Click { at }));
        assert_eq!(meaning(3, true, at), Some(PointerEvent::Click { at }));
        assert_eq!(
            meaning(2, false, at),
            None,
            "a plain middle click is nothing"
        );
        assert_eq!(meaning(2, true, at), Some(PointerEvent::Reset));
        assert_eq!(
            meaning(4, true, at),
            Some(PointerEvent::Wheel { steps: 1.0 })
        );
        assert_eq!(
            meaning(5, true, at),
            Some(PointerEvent::Wheel { steps: -1.0 })
        );
        assert_eq!(meaning(4, false, at), None, "plain scrolling is not zoom");
    }

    #[test]
    fn the_hook_starts_and_stops_promptly() {
        let mut hook = InputHook::start();
        std::thread::sleep(Duration::from_millis(100));
        let start = std::time::Instant::now();
        hook.stop();
        assert!(start.elapsed() < Duration::from_secs(1));
    }
}
