//! The X server connection the screens, the pointer and capture share.
//!
//! Wayland sessions run X11 programs through XWayland, so this works there
//! too, within what XWayland can see.

use std::sync::{Mutex, OnceLock};

use x11rb::connection::Connection;
use x11rb::protocol::xproto::{ConnectionExt as _, ImageFormat, Window};
use x11rb::rust_connection::RustConnection;

/// A connection, its screen and that screen's root window.
pub struct Display {
    pub conn: RustConnection,
    pub screen: usize,
    pub root: Window,
}

/// Open a connection of one's own, for a thread that keeps it busy.
pub fn connect() -> Result<Display, String> {
    let (conn, screen) = x11rb::connect(None).map_err(|e| format!("no X display: {e}"))?;
    let root = conn.setup().roots[screen].root;
    Ok(Display { conn, screen, root })
}

/// One connection for the short questions asked from anywhere: where the
/// pointer is, which screens there are.
pub fn shared() -> Option<std::sync::MutexGuard<'static, Option<Display>>> {
    static SHARED: OnceLock<Mutex<Option<Display>>> = OnceLock::new();
    let lock = SHARED.get_or_init(|| Mutex::new(connect().ok()));
    let mut guard = lock.lock().ok()?;
    if guard.is_none() {
        *guard = connect().ok();
    }
    guard.is_some().then_some(guard)
}

impl Display {
    /// The pixels of `x, y, width, height` on the root window as tightly
    /// packed BGRA, opaque. `None` if the server refuses or uses a pixel
    /// layout other than 32 bits per pixel.
    pub fn grab(&self, x: i32, y: i32, width: u32, height: u32) -> Result<Vec<u8>, String> {
        let reply = self
            .conn
            .get_image(
                ImageFormat::Z_PIXMAP,
                self.root,
                x as i16,
                y as i16,
                width as u16,
                height as u16,
                u32::MAX,
            )
            .map_err(|e| e.to_string())?
            .reply()
            .map_err(|e| e.to_string())?;
        let want = width as usize * height as usize * 4;
        if reply.data.len() != want {
            return Err(format!(
                "the screen is not 32 bits per pixel (depth {}, {} bytes for {want})",
                reply.depth,
                reply.data.len()
            ));
        }
        let mut pixels = reply.data;
        // 24-bit depth leaves the fourth byte undefined; the picture is opaque.
        for px in pixels.as_chunks_mut::<4>().0 {
            px[3] = 255;
        }
        Ok(pixels)
    }
}
