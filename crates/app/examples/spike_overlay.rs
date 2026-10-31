//! Validates the two risky parts of the UI before it is built on them.
//!
//! 1. A borderless, always-on-top, draggable overlay as a second egui
//!    viewport alongside the main window — multi-viewport is egui's rougher
//!    corner.
//! 2. Getting that overlay's raw HWND so `SetWindowDisplayAffinity` can hide
//!    it from screen capture. Without that the recording carries two copies of
//!    the camera: the one we composite, and the preview the grab caught.
//!
//! The overlay paints itself magenta, and the app samples a real capture of
//! that screen region to report, live, whether the window is still visible to
//! a recording.
//!
//!   cargo run -p screenilux --example spike_overlay

use std::time::{Duration, Instant};

use eframe::egui;
use screenilux_core::geometry::Rect;
use screenilux_core::ports::ScreenSource;
use screenilux_win::capture::ScreenCapture;

const OVERLAY_TITLE: &str = "screenilux-overlay-spike";
const POS: (f32, f32) = (300.0, 300.0);
const SIZE: (f32, f32) = (320.0, 240.0);

struct Spike {
    want_excluded: bool,
    applied: Option<bool>,
    hwnd: Option<isize>,
    verdict: String,
    last_probe: Instant,
    capture: Option<ScreenCapture>,
    bounds: Rect,
    uploads: u64,
    upload_ms: f64,
    worst_upload_ms: f64,
    texture: Option<egui::TextureHandle>,
    started: Instant,
}

impl Spike {
    fn new() -> Self {
        screenilux_win::set_dpi_awareness();
        let bounds = screenilux_win::monitors()
            .ok()
            .and_then(|m| m.into_iter().find(|m| m.is_primary))
            .map(|m| m.bounds)
            .unwrap_or(Rect::new(0, 0, 1920, 1080));
        Self {
            want_excluded: false,
            applied: None,
            hwnd: None,
            verdict: "probing…".into(),
            last_probe: Instant::now(),
            capture: ScreenCapture::start(bounds).ok(),
            bounds,
            uploads: 0,
            upload_ms: 0.0,
            worst_upload_ms: 0.0,
            texture: None,
            started: Instant::now(),
        }
    }

    /// Sample a capture of the region the overlay covers. Magenta is far more
    /// saturated than any normal desktop content, so "is the window there" is
    /// answerable straight from the pixels.
    fn probe(&mut self) {
        let Some(cap) = self.capture.as_mut() else {
            self.verdict = "no capture available".into();
            return;
        };
        let Ok(frame) = cap.next_frame(Duration::from_millis(200)) else {
            return;
        };
        // Sample below the preview image, which is opaque and not magenta.
        let (x0, y0) = (POS.0 as u32 + 20, POS.1 as u32 + 170);
        let (w, h) = (240u32, 50u32);
        let stride = self.bounds.width as usize * 4;
        let (mut magenta, mut total) = (0usize, 0usize);
        for y in y0..(y0 + h).min(self.bounds.height) {
            for x in x0..(x0 + w).min(self.bounds.width) {
                let i = y as usize * stride + x as usize * 4;
                let (b, g, r) = (frame.pixels[i], frame.pixels[i + 1], frame.pixels[i + 2]);
                if r > 180 && b > 180 && g < 90 {
                    magenta += 1;
                }
                total += 1;
            }
        }
        let share = magenta as f64 / total.max(1) as f64;
        // Report to stdout so the result is readable without watching the UI.
        println!(
            "[probe] excluded={} magenta={:>3.0}%  uploads={} last={:.2}ms worst={:.2}ms",
            self.want_excluded,
            share * 100.0,
            self.uploads,
            self.upload_ms,
            self.worst_upload_ms
        );
        self.verdict = format!(
            "{:>3.0}% magenta  ->  overlay is {}",
            share * 100.0,
            if share > 0.5 {
                "VISIBLE to capture"
            } else {
                "hidden from capture"
            }
        );
    }

    fn apply_affinity(&mut self) {
        let Some(h) = self.hwnd else { return };
        if self.applied == Some(self.want_excluded) {
            return;
        }
        // SAFETY: a live top-level handle found by title.
        let result = unsafe {
            if self.want_excluded {
                screenilux_win::exclude_from_capture(h)
            } else {
                screenilux_win::include_in_capture(h)
            }
        };
        match result {
            Ok(()) => self.applied = Some(self.want_excluded),
            Err(e) => self.verdict = format!("affinity failed: {e}"),
        }
    }
}

impl eframe::App for Spike {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();

        ui.heading("Overlay spike");
        ui.separator();
        ui.label(format!(
            "overlay HWND: {}",
            self.hwnd
                .map(|h| format!("0x{h:x}"))
                .unwrap_or_else(|| "not found yet".into())
        ));
        ui.checkbox(&mut self.want_excluded, "exclude the overlay from capture");
        ui.label(&self.verdict);
        ui.label(format!(
            "texture uploads: {}  last {:.2} ms  worst {:.2} ms",
            self.uploads, self.upload_ms, self.worst_upload_ms
        ));
        ui.separator();
        ui.label("Drag the magenta window; toggle the checkbox and watch the verdict.");
        if ui.button("quit").clicked() {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }

        // A second native window: borderless, always on top, draggable.
        ctx.show_viewport_immediate(
            egui::ViewportId::from_hash_of("overlay"),
            egui::ViewportBuilder::default()
                .with_title(OVERLAY_TITLE)
                .with_decorations(false)
                .with_always_on_top()
                .with_resizable(false)
                .with_taskbar(false)
                .with_inner_size([SIZE.0, SIZE.1])
                .with_position([POS.0, POS.1]),
            |ctx, _class| {
                // Context is cheap to clone (it is reference counted), and
                // cloning sidesteps borrowing the callback's parameter while
                // the panel closure also needs it.
                let vp = ctx.clone();
                egui::CentralPanel::default()
                    .frame(egui::Frame::NONE.fill(egui::Color32::from_rgb(255, 0, 255)))
                    .show(ctx, |ui| {
                        // Upload a texture every frame: the camera-preview cost.
                        let started = Instant::now();
                        let size = [160usize, 120usize];
                        let shift = self.uploads as usize;
                        let pixels: Vec<egui::Color32> = (0..size[0] * size[1])
                            .map(|i| {
                                let v = ((i + shift) % 255) as u8;
                                egui::Color32::from_rgb(v, 64, 255 - v)
                            })
                            .collect();
                        let image = egui::ColorImage {
                            size,
                            source_size: egui::vec2(size[0] as f32, size[1] as f32),
                            pixels,
                        };
                        match &mut self.texture {
                            Some(tex) => tex.set(image, egui::TextureOptions::LINEAR),
                            None => {
                                self.texture = Some(vp.load_texture(
                                    "preview",
                                    image,
                                    egui::TextureOptions::LINEAR,
                                ));
                            }
                        }
                        self.upload_ms = started.elapsed().as_secs_f64() * 1000.0;
                        self.worst_upload_ms = self.worst_upload_ms.max(self.upload_ms);
                        self.uploads += 1;

                        if let Some(tex) = &self.texture {
                            ui.image((tex.id(), egui::vec2(160.0, 120.0)));
                        }

                        let drag = ui.interact(
                            ui.max_rect(),
                            ui.scope_id().with("drag"),
                            egui::Sense::click_and_drag(),
                        );
                        if drag.dragged() {
                            vp.send_viewport_cmd(egui::ViewportCommand::StartDrag);
                        }
                    });
            },
        );

        if self.hwnd.is_none() {
            self.hwnd = find_window(OVERLAY_TITLE);
        }
        self.apply_affinity();

        // Drive the toggle automatically so the spike reports a verdict
        // without anyone clicking: visible, then excluded, then restored.
        let age = self.started.elapsed().as_secs_f64();
        self.want_excluded = (3.0..6.0).contains(&age);
        if age > 8.0 {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }

        if self.last_probe.elapsed() > Duration::from_millis(350) {
            self.last_probe = Instant::now();
            self.probe();
        }
        ctx.request_repaint_after(Duration::from_millis(16));
    }
}

/// eframe exposes no per-viewport window handle, so find the overlay by title.
fn find_window(title: &str) -> Option<isize> {
    use std::ffi::OsStr;
    use std::os::windows::ffi::OsStrExt;
    unsafe extern "system" {
        fn FindWindowW(class: *const u16, title: *const u16) -> isize;
    }
    let wide: Vec<u16> = OsStr::new(title).encode_wide().chain(Some(0)).collect();
    // SAFETY: a null class and a NUL-terminated title.
    let h = unsafe { FindWindowW(std::ptr::null(), wide.as_ptr()) };
    (h != 0).then_some(h)
}

fn main() -> eframe::Result<()> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default().with_inner_size([560.0, 300.0]),
        ..Default::default()
    };
    eframe::run_native(
        "Screenilux overlay spike",
        options,
        Box::new(|_cc| Ok(Box::new(Spike::new()) as Box<dyn eframe::App>)),
    )
}
