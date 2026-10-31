//! The settings window.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use eframe::egui;
use screenilux_recorder::{Session, SessionConfig};
use screenilux_win::Hotkey;

use crate::overlay::Overlay;
use crate::state::{CAMERA_SIZES, FPS_CHOICES, Phase, Probes, Runtime, Settings};

pub struct App {
    settings: Settings,
    probes: Probes,
    runtime: Runtime,
    phase: Phase,
    session: Option<Session>,
    overlay: Option<Overlay>,
    status: String,
    notices: Vec<String>,
    last_save: Option<PathBuf>,
    /// Re-shown after a recording stops, if the window minimised itself.
    was_minimised: bool,
    started_at: Option<Instant>,
    unavailable_hotkeys: Vec<String>,
}

impl App {
    pub fn new() -> Self {
        let runtime = Runtime::new();
        let probes = Probes::start(runtime.ffmpeg.clone());
        let unavailable_hotkeys = runtime.hook.unavailable_hotkeys();
        Self {
            settings: Settings::new(),
            probes,
            runtime,
            phase: Phase::Idle,
            session: None,
            overlay: None,
            status: "Ready".into(),
            notices: Vec::new(),
            last_save: None,
            was_minimised: false,
            started_at: None,
            unavailable_hotkeys,
        }
    }

    fn busy(&self) -> bool {
        matches!(self.phase, Phase::Recording | Phase::Paused)
    }

    fn recorded(&self) -> Duration {
        self.session
            .as_ref()
            .map(|s| s.duration())
            .unwrap_or_default()
    }

    // -- actions ----------------------------------------------------------

    fn start(&mut self, ctx: &egui::Context) {
        if self.busy() {
            return;
        }
        let Some(ffmpeg) = self.runtime.ffmpeg.clone() else {
            self.status = "ffmpeg was not found next to the app or on PATH".into();
            return;
        };
        let Some(encoder) = self.probes.encoder else {
            self.status = "still choosing an encoder, try again in a moment".into();
            return;
        };
        let targets = self.settings.targets();
        if targets.is_empty() {
            self.status = "Tick at least one screen to record".into();
            return;
        }
        if let Err(e) = std::fs::create_dir_all(&self.settings.out_dir) {
            self.status = format!("Cannot use that folder: {e}");
            return;
        }

        let camera = self
            .runtime
            .camera_feed
            .clone()
            .map(|f| f as Arc<dyn screenilux_core::ports::CameraSource>);
        let config = SessionConfig {
            monitors: targets,
            fps: self.settings.fps,
            audio_device: self
                .settings
                .microphone
                .and_then(|i| self.probes.microphones.get(i).cloned()),
            encoder,
            ffmpeg,
            effects: self.settings.effects,
            camera,
            pointer_events: Some(self.runtime.hook.pointer_events()),
        };

        match Session::start(config) {
            Ok(s) => {
                self.session = Some(s);
                self.phase = Phase::Recording;
                self.status = "Recording".into();
                self.started_at = Some(Instant::now());
                if self.settings.minimize {
                    self.was_minimised = true;
                    ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(true));
                }
            }
            Err(e) => self.status = format!("Could not start: {e}"),
        }
    }

    fn toggle_pause(&mut self) {
        let Some(session) = &self.session else { return };
        match self.phase {
            Phase::Recording => {
                session.pause();
                self.phase = Phase::Paused;
                self.status = "Paused".into();
            }
            Phase::Paused => {
                session.resume();
                self.phase = Phase::Recording;
                self.status = "Recording".into();
            }
            _ => {}
        }
    }

    fn stop(&mut self, ctx: &egui::Context) {
        let Some(session) = self.session.take() else {
            return;
        };
        self.phase = Phase::Saving;
        self.status = "Saving…".into();
        if self.was_minimised {
            ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(false));
            self.was_minimised = false;
        }
        // Painted once so the window shows "Saving…" before the blocking work.
        ctx.request_repaint();

        let outdir = self.settings.out_dir.clone();
        match session.finish(&outdir) {
            Ok(path) => {
                self.status = format!(
                    "Saved {}",
                    path.file_name().unwrap_or_default().to_string_lossy()
                );
                self.last_save = Some(path);
            }
            Err(e) => self.status = format!("Could not save: {e}"),
        }
        self.phase = Phase::Idle;
        self.started_at = None;
    }

    fn toggle_camera(&mut self, on: bool) {
        if !on {
            self.runtime.stop_camera();
            self.overlay = None;
            return;
        }
        if self.probes.cameras.is_empty() {
            self.status = if self.probes.ready {
                "No camera was found".into()
            } else {
                "Still looking for cameras, try again in a moment".into()
            };
            self.settings.effects.camera = false;
            return;
        }
        let bubble = self.settings.default_bubble();
        self.runtime
            .start_camera(self.settings.camera_device as u32, bubble);
        self.overlay = Some(Overlay::new(bubble));
    }

    fn restart_camera(&mut self) {
        if self.settings.effects.camera {
            self.toggle_camera(false);
            self.toggle_camera(true);
        }
    }

    fn handle_hotkeys(&mut self, ctx: &egui::Context) {
        let pressed: Vec<Hotkey> = self.runtime.hook.hotkeys().try_iter().collect();
        for key in pressed {
            match key {
                Hotkey::StartStop => {
                    if self.busy() {
                        self.stop(ctx);
                    } else {
                        self.start(ctx);
                    }
                }
                Hotkey::PauseResume => self.toggle_pause(),
            }
        }
    }

    // -- panels -----------------------------------------------------------

    fn screens_panel(&mut self, ui: &mut egui::Ui) {
        let busy = self.busy();
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new("Screens to record").strong());
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui
                    .add_enabled(!busy, egui::Button::new("Refresh"))
                    .clicked()
                {
                    self.settings.refresh_monitors();
                }
            });
        });

        egui::Frame::group(ui.style()).show(ui, |ui| {
            let multiple = self.settings.monitors.len() > 1;
            let locked = busy || (multiple && self.settings.whole_desktop);
            for (i, monitor) in self.settings.monitors.iter().enumerate() {
                let label = format!(
                    "{}  –  {}×{}{}",
                    monitor.label,
                    monitor.bounds.width,
                    monitor.bounds.height,
                    if monitor.is_primary {
                        "  (primary)"
                    } else {
                        ""
                    }
                );
                ui.add_enabled(
                    !locked,
                    egui::Checkbox::new(&mut self.settings.selected[i], label),
                );
            }
            if multiple {
                ui.separator();
                let all = screenilux_core::virtual_bounds(&self.settings.monitors);
                let label = match all {
                    Some(r) => format!("Whole desktop as one video  –  {}×{}", r.width, r.height),
                    None => "Whole desktop as one video".to_string(),
                };
                ui.add_enabled(
                    !busy,
                    egui::Checkbox::new(&mut self.settings.whole_desktop, label),
                );
            }
            if self.settings.monitors.is_empty() {
                ui.label("No displays detected");
            }
        });
    }

    fn options_panel(&mut self, ui: &mut egui::Ui) {
        let busy = self.busy();

        ui.horizontal(|ui| {
            ui.label("Microphone");
            let current = self
                .settings
                .microphone
                .and_then(|i| self.probes.microphones.get(i).cloned())
                .unwrap_or_else(|| "None".into());
            ui.add_enabled_ui(!busy, |ui| {
                egui::ComboBox::from_id_salt("mic")
                    .selected_text(current)
                    .width(260.0)
                    .show_ui(ui, |ui| {
                        ui.selectable_value(&mut self.settings.microphone, None, "None");
                        for (i, name) in self.probes.microphones.iter().enumerate() {
                            ui.selectable_value(&mut self.settings.microphone, Some(i), name);
                        }
                    });
            });
        });

        ui.horizontal(|ui| {
            ui.label("FPS");
            ui.add_enabled_ui(!busy, |ui| {
                egui::ComboBox::from_id_salt("fps")
                    .selected_text(self.settings.fps.to_string())
                    .width(70.0)
                    .show_ui(ui, |ui| {
                        for f in FPS_CHOICES {
                            ui.selectable_value(&mut self.settings.fps, f, f.to_string());
                        }
                    });
            });
            ui.add_space(12.0);
            ui.checkbox(&mut self.settings.minimize, "Minimize while recording");
        });

        ui.add_space(6.0);
        egui::Frame::group(ui.style()).show(ui, |ui| {
            ui.label("Mouse");
            ui.separator();
            let fx = &mut self.settings.effects;
            ui.add_enabled_ui(!busy, |ui| {
                ui.horizontal(|ui| {
                    ui.checkbox(&mut fx.cursor, "Show cursor");
                    ui.add_space(16.0);
                    ui.checkbox(&mut fx.zoom, "Wheel zoom");
                });
                ui.horizontal(|ui| {
                    ui.checkbox(&mut fx.trail, "Mouse trail");
                    ui.add_space(16.0);
                    ui.checkbox(&mut fx.clicks, "Click effect");
                });
            });
            ui.label(
                egui::RichText::new(
                    "Zoom with Ctrl+Alt+wheel while recording; Ctrl+Alt+middle-click resets it.",
                )
                .weak()
                .small(),
            );
        });

        ui.add_space(6.0);
        egui::Frame::group(ui.style()).show(ui, |ui| {
            ui.label("Camera");
            ui.separator();
            ui.horizontal(|ui| {
                let mut on = self.settings.effects.camera;
                if ui
                    .add_enabled(!busy, egui::Checkbox::new(&mut on, "Show camera"))
                    .changed()
                {
                    self.settings.effects.camera = on;
                    self.toggle_camera(on);
                }

                let current = self
                    .probes
                    .cameras
                    .get(self.settings.camera_device)
                    .cloned()
                    .unwrap_or_else(|| {
                        if self.probes.ready {
                            "(no camera found)".into()
                        } else {
                            "(looking…)".into()
                        }
                    });
                ui.add_enabled_ui(!busy, |ui| {
                    let mut changed = false;
                    egui::ComboBox::from_id_salt("cam")
                        .selected_text(current)
                        .width(200.0)
                        .show_ui(ui, |ui| {
                            for (i, name) in self.probes.cameras.iter().enumerate() {
                                changed |= ui
                                    .selectable_value(&mut self.settings.camera_device, i, name)
                                    .changed();
                            }
                        });
                    egui::ComboBox::from_id_salt("camsize")
                        .selected_text(CAMERA_SIZES[self.settings.camera_size].0)
                        .width(90.0)
                        .show_ui(ui, |ui| {
                            for (i, (name, _, _)) in CAMERA_SIZES.iter().enumerate() {
                                changed |= ui
                                    .selectable_value(&mut self.settings.camera_size, i, *name)
                                    .changed();
                            }
                        });
                    if changed {
                        self.restart_camera();
                    }
                });
            });
            ui.label(
                egui::RichText::new(
                    "Drag the bubble anywhere; it is burned into that screen's recording.",
                )
                .weak()
                .small(),
            );
        });

        ui.add_space(6.0);
        ui.horizontal(|ui| {
            ui.label("Save to");
            let mut text = self.settings.out_dir.display().to_string();
            if ui
                .add_enabled(
                    !busy,
                    egui::TextEdit::singleline(&mut text).desired_width(300.0),
                )
                .changed()
            {
                self.settings.out_dir = PathBuf::from(text);
            }
            if ui.add_enabled(!busy, egui::Button::new("Open")).clicked() {
                let target = self
                    .last_save
                    .clone()
                    .unwrap_or_else(|| self.settings.out_dir.clone());
                reveal(&target);
            }
        });
    }

    fn transport(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        let secs = self.recorded().as_secs();
        ui.vertical_centered(|ui| {
            ui.label(
                egui::RichText::new(format!(
                    "{:02}:{:02}:{:02}",
                    secs / 3600,
                    (secs % 3600) / 60,
                    secs % 60
                ))
                .size(34.0)
                .monospace(),
            );
            let colour = match self.phase {
                Phase::Recording => egui::Color32::from_rgb(200, 64, 56),
                Phase::Paused => egui::Color32::from_rgb(190, 130, 30),
                _ => ui.visuals().weak_text_color(),
            };
            ui.colored_label(colour, &self.status);
        });

        ui.add_space(8.0);
        let saving = self.phase == Phase::Saving;
        ui.columns(3, |cols| {
            if cols[0]
                .add_enabled(
                    !self.busy() && !saving,
                    egui::Button::new("Start recording").min_size(egui::vec2(0.0, 30.0)),
                )
                .clicked()
            {
                self.start(ctx);
            }
            let pause_label = if self.phase == Phase::Paused {
                "Resume"
            } else {
                "Pause"
            };
            if cols[1]
                .add_enabled(
                    self.busy(),
                    egui::Button::new(pause_label).min_size(egui::vec2(0.0, 30.0)),
                )
                .clicked()
            {
                self.toggle_pause();
            }
            if cols[2]
                .add_enabled(
                    self.busy(),
                    egui::Button::new("Stop & save").min_size(egui::vec2(0.0, 30.0)),
                )
                .clicked()
            {
                self.stop(ctx);
            }
        });

        ui.add_space(6.0);
        ui.vertical_centered(|ui| {
            ui.label(
                egui::RichText::new(format!("Encoder:  {}", self.probes.encoder_label()))
                    .weak()
                    .small(),
            );
            ui.label(
                egui::RichText::new("Ctrl+Alt+R start/stop      Ctrl+Alt+P pause/resume")
                    .weak()
                    .small(),
            );
        });
    }
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.probes.poll();
        self.handle_hotkeys(&ctx);

        if let Some(session) = &self.session {
            let fresh = session.notices();
            if !fresh.is_empty() {
                self.notices.extend(fresh);
            }
        }

        // The root Ui from eframe 0.37 carries no margin or background of its
        // own; its docs point at this helper, which supplies both. Applied
        // outside the scroll area, or the first row is clipped at the top.
        egui::Frame::central_panel(ui.style()).show(ui, |ui| {
            egui::ScrollArea::vertical()
                // Fill the window rather than shrinking to the content, or the
                // area below the controls is left unpainted.
                .auto_shrink([false; 2])
                .show(ui, |ui| {
                    self.screens_panel(ui);
                    ui.add_space(8.0);
                    self.options_panel(ui);
                    ui.separator();
                    self.transport(ui, &ctx);

                    if let Some(err) = self
                        .overlay
                        .as_ref()
                        .and_then(|o| o.exclusion_error.clone())
                    {
                        ui.add_space(4.0);
                        ui.colored_label(egui::Color32::from_rgb(200, 140, 60), err);
                    }
                    for notice in &self.notices {
                        ui.colored_label(egui::Color32::from_rgb(200, 140, 60), notice);
                    }
                    for key in self.unavailable_hotkeys.iter() {
                        ui.colored_label(
                            egui::Color32::from_rgb(200, 140, 60),
                            format!("{key} is already used by another application"),
                        );
                    }
                });
        });

        // The bubble is shown whenever the camera is on, recording or not, so
        // it can be positioned before starting.
        if let (Some(overlay), Some(feed)) =
            (self.overlay.as_mut(), self.runtime.camera_feed.as_ref())
        {
            let size = self.settings.camera_dimensions();
            overlay.show(&ctx, feed, size);
            if let Some(cam) = &self.runtime.camera {
                let p = overlay.position();
                cam.move_to(p.left, p.top);
            }
        }

        // Repaint steadily so the timer ticks and hotkeys are serviced even
        // when nothing is being interacted with.
        ctx.request_repaint_after(Duration::from_millis(if self.busy() { 100 } else { 250 }));
    }
}

/// Show a path in Explorer.
fn reveal(path: &std::path::Path) {
    let mut cmd = std::process::Command::new("explorer");
    if path.is_file() {
        cmd.arg("/select,");
    }
    cmd.arg(path);
    let _ = cmd.spawn();
}
