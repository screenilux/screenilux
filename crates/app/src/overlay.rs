//! The draggable camera bubble.
//!
//! A second native window, borderless and always on top, showing the live
//! camera. It is hidden from screen capture: the recorder composites the
//! camera into the video itself, so without the exclusion a recording would
//! carry two copies — the clean composited one, and this preview caught by the
//! grab.

use std::sync::Arc;

use eframe::egui;
use screenilux_core::Rect;
use screenilux_core::ports::CameraSource;
use screenilux_win::CameraFeed;

/// The window title is how we find our own HWND: eframe exposes no
/// per-viewport handle.
pub const OVERLAY_TITLE: &str = "Screenilux camera";

pub struct Overlay {
    texture: Option<egui::TextureHandle>,
    last_seq: u64,
    hwnd: Option<isize>,
    excluded: bool,
    /// Clipped to a circle, which needs the window to exist first.
    shaped: bool,
    /// Set once if the platform refuses the exclusion, so the UI can say so
    /// rather than silently recording a doubled camera.
    pub exclusion_error: Option<String>,
    position: Rect,
}

impl Overlay {
    pub fn new(position: Rect) -> Self {
        Self {
            texture: None,
            last_seq: u64::MAX,
            hwnd: None,
            excluded: false,
            shaped: false,
            exclusion_error: None,
            position,
        }
    }

    pub fn position(&self) -> Rect {
        self.position
    }

    /// Draw the bubble and keep its capture exclusion applied.
    pub fn show(&mut self, ctx: &egui::Context, feed: &Arc<CameraFeed>, size: (u32, u32)) {
        self.position = Rect::new(self.position.left, self.position.top, size.0, size.1);
        let pos = self.position;

        ctx.show_viewport_immediate(
            egui::ViewportId::from_hash_of("screenilux-camera"),
            egui::ViewportBuilder::default()
                .with_title(OVERLAY_TITLE)
                .with_decorations(false)
                .with_always_on_top()
                .with_resizable(false)
                .with_taskbar(false)
                .with_inner_size([size.0 as f32, size.1 as f32])
                .with_position([pos.left as f32, pos.top as f32]),
            |vp_ctx, _class| {
                let vp = vp_ctx.clone();
                egui::CentralPanel::default()
                    .frame(egui::Frame::NONE.fill(egui::Color32::from_rgb(16, 16, 16)))
                    .show(vp_ctx, |ui| {
                        self.paint(ui, feed);

                        let drag = ui.interact(
                            ui.max_rect(),
                            ui.scope_id().with("camera-drag"),
                            egui::Sense::click_and_drag(),
                        );
                        if drag.dragged() {
                            vp.send_viewport_cmd(egui::ViewportCommand::StartDrag);
                        }
                    });

                // Track where the user dragged it to, in desktop coordinates.
                if let Some(outer) = vp.input(|i| i.viewport().outer_rect) {
                    self.position =
                        Rect::new(outer.min.x as i32, outer.min.y as i32, size.0, size.1);
                }
            },
        );

        self.apply_exclusion();
    }

    fn paint(&mut self, ui: &mut egui::Ui, feed: &Arc<CameraFeed>) {
        if let Some(err) = feed.error() {
            ui.centered_and_justified(|ui| {
                ui.colored_label(egui::Color32::from_rgb(220, 120, 120), err);
            });
            return;
        }

        if let Some(frame) = feed.latest()
            && frame.seq != self.last_seq
        {
            // Only convert when the camera has actually moved on; the UI
            // repaints far faster than the camera delivers.
            self.last_seq = frame.seq;
            let pixels: Vec<egui::Color32> = frame
                .pixels
                .as_chunks::<4>()
                .0
                .iter()
                // the feed is BGRA
                .map(|p| egui::Color32::from_rgb(p[2], p[1], p[0]))
                .collect();
            let image = egui::ColorImage {
                size: [frame.width as usize, frame.height as usize],
                source_size: egui::vec2(frame.width as f32, frame.height as f32),
                pixels,
            };
            match &mut self.texture {
                Some(tex) => tex.set(image, egui::TextureOptions::LINEAR),
                None => {
                    self.texture = Some(ui.ctx().load_texture(
                        "camera",
                        image,
                        egui::TextureOptions::LINEAR,
                    ))
                }
            }
        }

        match &self.texture {
            Some(tex) => {
                // Fill the bubble, cropping the middle of the picture to its
                // shape, as the recording does.
                let rect = ui.max_rect();
                let [tw, th] = tex.size().map(|v| v as f32);
                let (want, have) = (rect.width() / rect.height(), tw / th);
                let uv = if have > want {
                    let w = want / have;
                    egui::Rect::from_min_max(
                        egui::pos2((1.0 - w) / 2.0, 0.0),
                        egui::pos2((1.0 + w) / 2.0, 1.0),
                    )
                } else {
                    let h = have / want;
                    egui::Rect::from_min_max(
                        egui::pos2(0.0, (1.0 - h) / 2.0),
                        egui::pos2(1.0, (1.0 + h) / 2.0),
                    )
                };
                ui.painter().image(tex.id(), rect, uv, egui::Color32::WHITE);
            }
            None => {
                ui.centered_and_justified(|ui| {
                    ui.label("starting the camera…");
                });
            }
        }
    }

    /// Hide the window from screen capture, once it exists.
    ///
    /// Must run in this process: `SetWindowDisplayAffinity` returns
    /// `ERROR_ACCESS_DENIED` when called on another process's window.
    fn apply_exclusion(&mut self) {
        if self.excluded {
            return;
        }
        if self.hwnd.is_none() {
            self.hwnd = find_window(OVERLAY_TITLE);
        }
        let Some(h) = self.hwnd else { return };
        if !self.shaped {
            // SAFETY: as below.
            unsafe { screenilux_win::set_circular(h) };
            self.shaped = true;
        }
        // SAFETY: a live top-level handle, found by title, owned by us.
        match unsafe { screenilux_win::exclude_from_capture(h) } {
            Ok(()) => self.excluded = true,
            Err(e) => {
                self.exclusion_error = Some(format!(
                    "the camera preview cannot be hidden from recordings ({e}); \
                     it may appear twice"
                ));
                // Do not retry every frame once the platform has refused.
                self.excluded = true;
            }
        }
    }
}

fn find_window(title: &str) -> Option<isize> {
    use std::ffi::OsStr;
    use std::os::windows::ffi::OsStrExt;
    unsafe extern "system" {
        fn FindWindowW(class: *const u16, title: *const u16) -> isize;
    }
    let wide: Vec<u16> = OsStr::new(title).encode_wide().chain(Some(0)).collect();
    // SAFETY: null class, NUL-terminated title.
    let h = unsafe { FindWindowW(std::ptr::null(), wide.as_ptr()) };
    (h != 0).then_some(h)
}
