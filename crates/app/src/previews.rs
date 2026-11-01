//! Live thumbnails of each screen for the Record tab, refreshed about once a
//! second on a thread of their own.
//!
//! Only while the Record tab is showing and nothing is being recorded: a
//! preview is for choosing what to record, and grabbing the desktop during a
//! recording would only cost time the capture threads need.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, channel};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use eframe::egui;
use screenilux_core::Rect;

/// How often each screen is grabbed.
const EVERY: Duration = Duration::from_millis(900);
/// The largest a thumbnail is grabbed at, in pixels: twice a tile's width,
/// so it stays sharp on a scaled display.
const GRAB: (u32, u32) = (360, 240);

pub struct Previews {
    /// The screens to grab, in the order the UI shows them.
    areas: Arc<Mutex<Vec<Rect>>>,
    active: Arc<AtomicBool>,
    stop: Arc<AtomicBool>,
    rx: Receiver<(Rect, screenilux_win::Thumbnail)>,
    textures: Vec<(Rect, egui::TextureHandle)>,
}

impl Previews {
    pub fn start(ctx: &egui::Context) -> Self {
        let areas: Arc<Mutex<Vec<Rect>>> = Arc::default();
        let active = Arc::new(AtomicBool::new(false));
        let stop = Arc::new(AtomicBool::new(false));
        let (tx, rx) = channel();
        let (a, on, quit, repaint) = (areas.clone(), active.clone(), stop.clone(), ctx.clone());
        std::thread::Builder::new()
            .name("screenilux-previews".into())
            .spawn(move || {
                while !quit.load(Ordering::Relaxed) {
                    if on.load(Ordering::Relaxed) {
                        let rects = a.lock().map(|r| r.clone()).unwrap_or_default();
                        for r in rects {
                            if let Some(t) = screenilux_win::thumbnail(r, GRAB.0, GRAB.1)
                                && tx.send((r, t)).is_err()
                            {
                                return;
                            }
                        }
                        repaint.request_repaint();
                    }
                    std::thread::sleep(EVERY);
                }
            })
            .ok();
        Self {
            areas,
            active,
            stop,
            rx,
            textures: Vec::new(),
        }
    }

    /// Say which screens to show and whether to keep grabbing them, and take
    /// in whatever has arrived. Cheap to call every frame.
    pub fn update(&mut self, ctx: &egui::Context, screens: &[Rect], active: bool) {
        self.active.store(active, Ordering::Relaxed);
        if let Ok(mut a) = self.areas.lock()
            && a.as_slice() != screens
        {
            *a = screens.to_vec();
        }
        // A screen that went away takes its picture with it.
        self.textures.retain(|(r, _)| screens.contains(r));
        for (rect, t) in self.rx.try_iter() {
            let image = egui::ColorImage::from_rgba_unmultiplied(
                [t.width as usize, t.height as usize],
                &t.rgba,
            );
            match self.textures.iter_mut().find(|(r, _)| *r == rect) {
                Some((_, tex)) => tex.set(image, egui::TextureOptions::LINEAR),
                None => self.textures.push((
                    rect,
                    ctx.load_texture(
                        format!("preview-{}-{}", rect.left, rect.top),
                        image,
                        egui::TextureOptions::LINEAR,
                    ),
                )),
            }
        }
    }

    /// The latest picture of the screen at `area`, once there is one.
    pub fn get(&self, area: Rect) -> Option<&egui::TextureHandle> {
        self.textures
            .iter()
            .find(|(r, _)| *r == area)
            .map(|(_, t)| t)
    }
}

impl Drop for Previews {
    fn drop(&mut self) {
        // The thread notices within one sleep and exits on its own.
        self.stop.store(true, Ordering::Relaxed);
    }
}
