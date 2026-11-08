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
use screenilux_platform::CameraFeed;

/// The window title is how we find our own HWND: eframe exposes no
/// per-viewport handle.
pub const OVERLAY_TITLE: &str = "Screenilux camera";

pub struct Overlay {
    texture: Option<egui::TextureHandle>,
    last_seq: u64,
    hwnd: Option<isize>,
    excluded: bool,
    /// The size in pixels the window was last cut to a pill at, which needs
    /// the window to exist first and again whenever it changes size.
    shaped_to: Option<(u32, u32)>,
    /// Where the window was first put, in points. Handed to egui unchanged
    /// every frame: egui moves the window whenever this value changes, so
    /// feeding back where a drag has got to would fight the drag, a frame
    /// behind and rounded, and the bubble would shake under the cursor.
    spawn: Option<egui::Pos2>,
    /// The bubble window's own scale, physical pixels per point.
    ppp: Option<f32>,
    /// The camera's own shape, once a frame has shown what it is. The
    /// bubble is sized to it rather than to a square, so faces are not
    /// cropped to fit a circle that nothing was framed for.
    aspect: Option<f32>,
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
            shaped_to: None,
            spawn: None,
            ppp: None,
            aspect: None,
            exclusion_error: None,
            position,
        }
    }

    pub fn position(&self) -> Rect {
        self.position
    }

    /// Draw the bubble and keep its capture exclusion applied.
    pub fn show(&mut self, ctx: &egui::Context, feed: &Arc<CameraFeed>, size: (u32, u32)) {
        // The first frame says what shape the camera actually is; the
        // bubble takes that shape, scaled down to the size asked for, so a
        // widescreen camera is not squeezed into a square.
        if self.aspect.is_none()
            && let Some(f) = feed.latest()
            && f.height > 0
        {
            self.aspect = Some(f.width as f32 / f.height as f32);
        }
        let (w, h) = match self.aspect {
            Some(a) if a > 0.0 => {
                let long = size.0.max(size.1) as f32;
                if a >= 1.0 {
                    (long.round() as u32, (long / a).round() as u32)
                } else {
                    ((long * a).round() as u32, long.round() as u32)
                }
            }
            _ => size,
        };
        let (w, h) = (w.max(2), h.max(2));
        self.position = Rect::new(self.position.left, self.position.top, w, h);

        // The bubble's place and size are desktop pixels, as the recording
        // uses them; egui wants points, which differ whenever Windows scales
        // the screen. Without converting, at 150% the preview would sit and
        // measure half as much again as the bubble in the video.
        let ppp = self.ppp.unwrap_or_else(|| ctx.pixels_per_point());
        let spawn = *self.spawn.get_or_insert_with(|| {
            egui::pos2(self.position.left as f32, self.position.top as f32) / ppp
        });
        let mut inner_px = None;

        ctx.show_viewport_immediate(
            egui::ViewportId::from_hash_of("screenilux-camera"),
            egui::ViewportBuilder::default()
                .with_title(OVERLAY_TITLE)
                .with_decorations(false)
                .with_always_on_top()
                .with_resizable(false)
                .with_taskbar(false)
                .with_inner_size([w as f32 / ppp, h as f32 / ppp])
                .with_position(spawn),
            |vp_ctx, _class| {
                let vp = vp_ctx.clone();
                egui::CentralPanel::default()
                    .frame(egui::Frame::NONE.fill(egui::Color32::BLACK))
                    .show(vp_ctx, |ui| {
                        self.paint(ui, feed);

                        let drag = ui.interact(
                            ui.max_rect(),
                            ui.scope_id().with("camera-drag"),
                            egui::Sense::click_and_drag(),
                        );
                        // Once, as the drag begins: Windows moves the window
                        // from there until the button is let go. Asking again
                        // every frame restarts the move over and over.
                        if drag.drag_started_by(egui::PointerButton::Primary) {
                            vp.send_viewport_cmd(egui::ViewportCommand::StartDrag);
                        }
                    });

                // Track where the user dragged it to, in desktop pixels.
                let scale = vp.pixels_per_point();
                self.ppp = Some(scale);
                let (outer, inner) =
                    vp.input(|i| (i.viewport().outer_rect, i.viewport().inner_rect));
                if let Some(outer) = outer {
                    let at = outer.min * scale;
                    self.position = Rect::new(at.x.round() as i32, at.y.round() as i32, w, h);
                }
                if let Some(inner) = inner {
                    let s = inner.size() * scale;
                    inner_px = Some((s.x.round() as u32, s.y.round() as u32));
                }
            },
        );

        self.apply_exclusion();
        self.apply_shape(inner_px);
    }

    /// Cut the window to the bubble's pill, so it is the same shape on
    /// screen as in the recording rather than a box with the corners of the
    /// picture showing round it.
    fn apply_shape(&mut self, size: Option<(u32, u32)>) {
        let (Some(h), Some(size)) = (self.hwnd, size) else {
            return;
        };
        if self.shaped_to == Some(size) {
            return;
        }
        // SAFETY: a live top-level handle, found by title, owned by us.
        unsafe { screenilux_platform::set_pill_shape(h, size.0, size.1) };
        self.shaped_to = Some(size);
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
                // Rounded here as well as by the window's own shape: the
                // window's edge is cut pixel by pixel, this one is smooth.
                let round = egui::CornerRadius::same(
                    (rect.width().min(rect.height()) / 2.0).min(255.0) as u8,
                );
                egui::Image::from_texture((tex.id(), rect.size()))
                    .uv(uv)
                    .corner_radius(round)
                    .paint_at(ui, rect);
                // The same faint rim the recording draws.
                ui.painter().rect_stroke(
                    rect,
                    round,
                    egui::Stroke::new(1.0, egui::Color32::from_black_alpha(60)),
                    egui::StrokeKind::Inside,
                );
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
            self.hwnd = screenilux_platform::find_window(OVERLAY_TITLE);
        }
        let Some(h) = self.hwnd else { return };
        // Where the system cannot hide it, the preview is what the recording
        // shows, so there is nothing to warn about.
        if !screenilux_platform::CAN_HIDE_FROM_CAPTURE {
            self.excluded = true;
            return;
        }
        // SAFETY: a live top-level handle, found by title, owned by us.
        match unsafe { screenilux_platform::exclude_from_capture(h) } {
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
