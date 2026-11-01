//! The main window: the Record tab, and the frame around both tabs.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use eframe::egui;
use screenilux_recorder::{Session, SessionConfig};

use crate::overlay::Overlay;
use crate::player_view::{PlayerView, dropped_path, playable};
use crate::sizing::{Fit, Sizer};
use crate::state::{FPS_CHOICES, Phase, Probes, Runtime, Settings};
use crate::theme::{self, Palette, icon};

/// The widest the Record tab's settings get. Wider, and a label sits too far
/// from the switch it describes.
const COLUMN: f32 = 760.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Tab {
    Record,
    Play,
}

pub struct App {
    tab: Tab,
    sizer: Sizer,
    player: PlayerView,
    settings: Settings,
    probes: Probes,
    runtime: Runtime,
    phase: Phase,
    session: Option<Session>,
    overlay: Option<Overlay>,
    status: String,
    last_save: Option<PathBuf>,
    started_at: Option<Instant>,
    /// The main window's handle, when the platform gave one.
    hwnd: Option<isize>,
    /// Shrunk to the recording bar.
    compact: bool,
    /// The window's opacity and corners as last set, so Windows is only
    /// asked when they change.
    alpha: u8,
    rounded: bool,
    /// Listens for displays coming and going; kept alive with the app.
    _displays: screenilux_win::DisplayWatcher,
    display_rx: std::sync::mpsc::Receiver<()>,
    /// When the displays last changed and have not been re-read since.
    displays_changed: Option<Instant>,
}

/// Windows sends several messages for one change, and the new layout can
/// take a moment to settle; reading once after a short quiet spell catches
/// the final state once.
const DISPLAY_SETTLE: Duration = Duration::from_millis(400);

/// The recording bar's opacity when the pointer is elsewhere: there, but out
/// of the way of what is being recorded behind it.
const BAR_ALPHA: u8 = 170;

impl App {
    pub fn new(hwnd: Option<isize>, ctx: &egui::Context) -> Self {
        let (tx, display_rx) = std::sync::mpsc::channel();
        let repaint = ctx.clone();
        let displays = screenilux_win::DisplayWatcher::start(move || {
            let _ = tx.send(());
            repaint.request_repaint();
        });
        let runtime = Runtime::new();
        let probes = Probes::start();
        Self {
            tab: Tab::Record,
            sizer: Sizer::new(),
            player: PlayerView::new(),
            settings: Settings::new(),
            probes,
            runtime,
            phase: Phase::Idle,
            session: None,
            overlay: None,
            status: "Ready".into(),
            last_save: None,
            started_at: None,
            hwnd,
            compact: false,
            alpha: 255,
            rounded: false,
            _displays: displays,
            display_rx,
            displays_changed: None,
        }
    }

    /// Switch to the Play tab and open `path`.
    pub fn play(&mut self, path: PathBuf, ctx: &egui::Context) {
        self.tab = Tab::Play;
        self.player.open(path, ctx);
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
            microphone: self.settings.microphone && self.probes.has_microphone,
            click_sound: self.settings.click_sound,
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
                // The window stays out of the recording, as the bar or not.
                self.set_captured(false);
                self.compact = self.settings.shrink;
                ctx.request_repaint();
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
        self.compact = false;
        self.set_captured(true);
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
                self.player.recordings_changed();
            }
            Err(e) => self.status = format!("Could not save: {e}"),
        }
        self.phase = Phase::Idle;
        self.started_at = None;
    }

    /// Re-read the displays once they have settled after a change. Not
    /// during a recording: what is being recorded is fixed at the start.
    fn follow_displays(&mut self, ctx: &egui::Context) {
        if self.display_rx.try_iter().count() > 0 {
            self.displays_changed = Some(Instant::now());
        }
        let Some(at) = self.displays_changed else {
            return;
        };
        if self.busy() || self.phase == Phase::Saving {
            return;
        }
        let quiet = at.elapsed();
        if quiet < DISPLAY_SETTLE {
            ctx.request_repaint_after(DISPLAY_SETTLE - quiet);
            return;
        }
        self.displays_changed = None;
        self.settings.refresh_monitors();
    }

    /// Let screen capture see this window, or hide it from capture.
    fn set_captured(&self, captured: bool) {
        let Some(h) = self.hwnd else { return };
        // SAFETY: the handle eframe gave for this window, which lives as
        // long as the app does.
        let result = unsafe {
            if captured {
                screenilux_win::include_in_capture(h)
            } else {
                screenilux_win::exclude_from_capture(h)
            }
        };
        if let Err(e) = result {
            tracing::warn!(error = %e, "could not change the window's capture visibility");
        }
    }

    /// The bar is see-through until the pointer is over it, and has round
    /// corners; the normal window has neither.
    fn dress_window(&mut self, ctx: &egui::Context) {
        let Some(h) = self.hwnd else { return };
        let hovered = ctx.input(|i| i.pointer.hover_pos().is_some());
        let alpha = if self.compact && !hovered {
            BAR_ALPHA
        } else {
            255
        };
        // SAFETY: as in `set_captured`.
        unsafe {
            if self.compact != self.rounded {
                screenilux_win::set_rounded(h, self.compact);
                self.rounded = self.compact;
            }
            if alpha != self.alpha {
                screenilux_win::set_opacity(h, alpha);
                self.alpha = alpha;
            }
        }
    }

    /// The window shrunk to a row at the bottom of the screen while
    /// recording: the clock, pause and stop, and a way back to the full
    /// window. Drag it anywhere to move it.
    fn bar(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        let p = theme::palette(ui);
        egui::CentralPanel::default_margins()
            .frame(
                egui::Frame::new()
                    .fill(p.surface)
                    .stroke(egui::Stroke::new(1.0, p.stroke))
                    .inner_margin(egui::Margin::symmetric(14, 0)),
            )
            .show(ui, |ui| {
                let grip = ui.interact(
                    ui.max_rect(),
                    egui::Id::unique("bar-drag"),
                    egui::Sense::drag(),
                );
                if grip.drag_started() {
                    ctx.send_viewport_cmd(egui::ViewportCommand::StartDrag);
                }
                let paused = self.phase == Phase::Paused;
                let accent = if paused { p.paused } else { p.tally };
                ui.horizontal_centered(|ui| {
                    let (dot, _) =
                        ui.allocate_exact_size(egui::vec2(12.0, 12.0), egui::Sense::hover());
                    ui.painter().circle_filled(dot.center(), 5.5, accent);
                    let secs = self.recorded().as_secs();
                    ui.label(
                        egui::RichText::new(format!(
                            "{:02}:{:02}:{:02}",
                            secs / 3600,
                            (secs % 3600) / 60,
                            secs % 60
                        ))
                        .font(egui::FontId::new(22.0, theme::light()))
                        .color(p.text),
                    );
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let stop = theme::primary(format!("{}  Stop", icon::STOP), p.signal)
                            .min_size(egui::vec2(0.0, 32.0));
                        if ui.add(stop).on_hover_text("Stop and save").clicked() {
                            self.stop(ctx);
                        }
                        let (glyph, text) = if paused {
                            (icon::RECORD, "Resume")
                        } else {
                            (icon::PAUSE, "Pause")
                        };
                        if ui
                            .add(
                                egui::Button::new(format!("{glyph}  {text}"))
                                    .min_size(egui::vec2(0.0, 32.0)),
                            )
                            .clicked()
                        {
                            self.toggle_pause();
                        }
                        if ui
                            .add(
                                egui::Button::new(icon::EXPAND)
                                    .frame(false)
                                    .min_size(egui::vec2(32.0, 32.0)),
                            )
                            .on_hover_text("Show the full window")
                            .clicked()
                        {
                            self.compact = false;
                        }
                    });
                });
            });
    }

    /// The camera bubble, shown whenever the camera is on, recording or not,
    /// so it can be positioned before starting.
    fn show_overlay(&mut self, ctx: &egui::Context) {
        if let (Some(overlay), Some(feed)) =
            (self.overlay.as_mut(), self.runtime.camera_feed.as_ref())
        {
            let size = self.settings.camera_dimensions();
            overlay.show(ctx, feed, size);
            if let Some(cam) = &self.runtime.camera {
                let p = overlay.position();
                cam.move_to(p.left, p.top);
            }
        }
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

    // -- Record tab --------------------------------------------------------

    fn record_tab(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        let p = theme::palette(ui);
        egui::Panel::bottom("transport")
            .frame(
                egui::Frame::new()
                    .fill(p.surface)
                    .inner_margin(egui::Margin::symmetric(20, 14)),
            )
            .show_separator_line(true)
            .show(ui, |ui| {
                theme::centered(ui, COLUMN, |ui| self.transport(ui, ctx))
            });

        egui::CentralPanel::default_margins()
            .frame(
                egui::Frame::new()
                    .fill(p.base)
                    .inner_margin(egui::Margin::symmetric(20, 14)),
            )
            .show(ui, |ui| {
                egui::ScrollArea::vertical()
                    .auto_shrink([false; 2])
                    .show(ui, |ui| {
                        theme::centered(ui, COLUMN, |ui| {
                            self.warnings(ui);
                            self.screens_section(ui);
                            ui.add_space(6.0);
                            self.sound_section(ui);
                            ui.add_space(6.0);
                            self.pointer_section(ui);
                            ui.add_space(6.0);
                            self.output_section(ui);
                        });
                    });
            });
    }

    fn warnings(&self, ui: &mut egui::Ui) {
        let mut any = false;
        if let Some(err) = self
            .overlay
            .as_ref()
            .and_then(|o| o.exclusion_error.clone())
        {
            theme::notice(ui, err);
            any = true;
        }
        if any {
            ui.add_space(4.0);
        }
    }

    fn screens_section(&mut self, ui: &mut egui::Ui) {
        let busy = self.busy();
        let phase = self.phase;
        let bubble = self
            .overlay
            .as_ref()
            .filter(|_| self.settings.effects.camera)
            .map(|o| o.position());
        ui.label(egui::RichText::new("Screens").font(egui::FontId::new(14.0, theme::strong())));
        let p = theme::palette(ui);
        egui::Frame::new()
            .fill(p.surface)
            .stroke(egui::Stroke::new(1.0, p.stroke))
            .corner_radius(8)
            .inner_margin(egui::Margin::symmetric(14, 12))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                if self.settings.monitors.is_empty() {
                    ui.label(
                        "No displays were found. One will show here as soon as it is connected.",
                    );
                    return;
                }
                let s = &mut self.settings;
                let whole = s.whole_desktop && s.monitors.len() > 1;
                monitor_map(
                    ui,
                    &s.monitors,
                    &mut s.selected,
                    whole,
                    !busy,
                    phase,
                    bubble,
                );
                let chosen = s.selected.iter().filter(|&&on| on).count();
                let summary = if whole {
                    "The whole desktop is recorded as one video.".to_string()
                } else if chosen > 1 {
                    format!("{chosen} screens, each its own video, saved together as one file.")
                } else if s.monitors.len() > 1 {
                    "Click another screen to record it too.".to_string()
                } else {
                    "This screen is recorded.".to_string()
                };
                theme::hint(ui, &summary);
                if s.monitors.len() > 1 {
                    ui.add_enabled_ui(!busy, |ui| {
                        theme::switch(
                            ui,
                            &mut s.whole_desktop,
                            "Record the whole desktop as one video",
                        );
                    });
                }
            });
    }

    fn sound_section(&mut self, ui: &mut egui::Ui) {
        let busy = self.busy();
        theme::section(ui, "Sound and camera", |ui| {
            ui.horizontal(|ui| {
                let available = self.probes.has_microphone;
                ui.label(icon::MIC);
                ui.add_enabled_ui(!busy && available, |ui| {
                    theme::switch(ui, &mut self.settings.microphone, "Microphone");
                });
                if self.probes.ready && !available {
                    theme::hint(ui, "No microphone found");
                }
            });

            ui.horizontal(|ui| {
                ui.label(icon::CAMERA);
                let mut on = self.settings.effects.camera;
                let changed = ui
                    .add_enabled_ui(!busy, |ui| theme::switch(ui, &mut on, "Camera"))
                    .inner
                    .changed();
                if changed {
                    self.settings.effects.camera = on;
                    self.toggle_camera(on);
                }
                ui.add_space(8.0);
                let current = self
                    .probes
                    .cameras
                    .get(self.settings.camera_device)
                    .cloned()
                    .unwrap_or_else(|| {
                        if self.probes.ready {
                            "No camera found".into()
                        } else {
                            "Looking for cameras…".into()
                        }
                    });
                ui.add_enabled_ui(!busy && !self.probes.cameras.is_empty(), |ui| {
                    let mut changed = false;
                    egui::ComboBox::from_id_salt("cam")
                        .selected_text(current)
                        .width(190.0)
                        .show_ui(ui, |ui| {
                            for (i, name) in self.probes.cameras.iter().enumerate() {
                                changed |= ui
                                    .selectable_value(&mut self.settings.camera_device, i, name)
                                    .changed();
                            }
                        });
                    if changed {
                        self.restart_camera();
                    }
                });
            });
            theme::hint(
                ui,
                "Drag the camera bubble where you want it, even while recording. It is \
                 recorded into the screen it sits on.",
            );
        });
    }

    fn pointer_section(&mut self, ui: &mut egui::Ui) {
        let busy = self.busy();
        theme::section(ui, "Pointer", |ui| {
            let fx = &mut self.settings.effects;
            ui.add_enabled_ui(!busy, |ui| {
                egui::Grid::new("pointer")
                    .num_columns(2)
                    .spacing(egui::vec2(32.0, 10.0))
                    .show(ui, |ui| {
                        theme::switch(ui, &mut fx.cursor, "Show the pointer");
                        theme::switch(ui, &mut fx.zoom, "Zoom with the wheel");
                        ui.end_row();
                        theme::switch(ui, &mut fx.trail, "Trail behind the pointer");
                        theme::switch(ui, &mut self.settings.click_sound, "Click sound");
                        ui.end_row();
                    });
            });
            theme::hint(
                ui,
                "While recording, Ctrl+Alt+wheel zooms in where the pointer is and \
                 Ctrl+Alt+middle-click zooms back out.",
            );
        });
    }

    fn output_section(&mut self, ui: &mut egui::Ui) {
        let busy = self.busy();
        theme::section(ui, "Video", |ui| {
            ui.horizontal(|ui| {
                ui.label("Frame rate");
                ui.add_enabled_ui(!busy, |ui| {
                    egui::ComboBox::from_id_salt("fps")
                        .selected_text(format!("{} fps", self.settings.fps))
                        .width(84.0)
                        .show_ui(ui, |ui| {
                            for f in FPS_CHOICES {
                                ui.selectable_value(&mut self.settings.fps, f, format!("{f} fps"));
                            }
                        });
                });
                ui.add_space(24.0);
                theme::switch(
                    ui,
                    &mut self.settings.shrink,
                    "Shrink to a bar while recording",
                );
            });
            ui.horizontal(|ui| {
                ui.label(icon::FOLDER);
                ui.label(egui::RichText::new(self.settings.out_dir.display().to_string()).weak())
                    .on_hover_text("Where recordings are saved");
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button("Open folder").clicked() {
                        let _ = std::fs::create_dir_all(&self.settings.out_dir);
                        reveal(&self.settings.out_dir);
                    }
                    if ui
                        .add_enabled(!busy, egui::Button::new("Change…"))
                        .clicked()
                        && let Some(dir) = rfd::FileDialog::new()
                            .set_title("Save recordings to")
                            .set_directory(&self.settings.out_dir)
                            .pick_folder()
                    {
                        self.settings.out_dir = dir;
                        self.player.recordings_changed();
                    }
                });
            });
        });
    }

    fn transport(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        let p = theme::palette(ui);
        let (accent, label) = match self.phase {
            Phase::Recording => (p.tally, "Recording"),
            Phase::Paused => (p.paused, "Paused"),
            Phase::Saving => (p.weak, "Saving…"),
            Phase::Idle => (p.weak, "Ready"),
        };

        // The tally light: a red rule along the top while anything is being
        // recorded, so the state reads from across the room.
        if matches!(self.phase, Phase::Recording | Phase::Paused) {
            let r = ui.max_rect();
            let bar = egui::Rect::from_min_max(
                egui::pos2(r.left() - 20.0, r.top() - 14.0),
                egui::pos2(r.right() + 20.0, r.top() - 11.0),
            );
            ui.painter().rect_filled(bar, 0.0, accent);
        }

        ui.horizontal(|ui| {
            ui.vertical(|ui| {
                let secs = self.recorded().as_secs();
                let clock = format!(
                    "{:02}:{:02}:{:02}",
                    secs / 3600,
                    (secs % 3600) / 60,
                    secs % 60
                );
                let colour = if self.phase == Phase::Idle {
                    p.weak
                } else {
                    p.text
                };
                ui.label(
                    egui::RichText::new(clock)
                        .font(egui::FontId::new(40.0, theme::light()))
                        .color(colour),
                );
                ui.horizontal(|ui| {
                    let (dot, _) =
                        ui.allocate_exact_size(egui::vec2(10.0, 10.0), egui::Sense::hover());
                    ui.painter().circle_filled(dot.center(), 4.5, accent);
                    let status = if self.phase == Phase::Idle && self.status != "Ready" {
                        self.status.clone()
                    } else {
                        label.to_string()
                    };
                    ui.label(egui::RichText::new(status).color(p.weak));
                });
            });

            ui.with_layout(
                egui::Layout::right_to_left(egui::Align::Center),
                |ui| match self.phase {
                    Phase::Idle => {
                        let start =
                            theme::primary(format!("{}  Start recording", icon::RECORD), p.tally)
                                .min_size(egui::vec2(170.0, 38.0));
                        if ui.add(start).clicked() {
                            self.start(ctx);
                        }
                        if let Some(saved) = self.last_save.clone() {
                            if ui
                                .add(
                                    egui::Button::new(format!("{}  Play", icon::PLAY))
                                        .min_size(egui::vec2(0.0, 38.0)),
                                )
                                .on_hover_text("Watch the recording you just made")
                                .clicked()
                            {
                                self.play(saved.clone(), ctx);
                            }
                            if ui
                                .add(
                                    egui::Button::new(format!("{}  Show", icon::FOLDER))
                                        .min_size(egui::vec2(0.0, 38.0)),
                                )
                                .on_hover_text("Show it in File Explorer")
                                .clicked()
                            {
                                reveal(&saved);
                            }
                        }
                    }
                    Phase::Recording | Phase::Paused => {
                        let stop =
                            theme::primary(format!("{}  Stop and save", icon::STOP), p.signal)
                                .min_size(egui::vec2(160.0, 38.0));
                        if ui.add(stop).clicked() {
                            self.stop(ctx);
                        }
                        let (glyph, text) = if self.phase == Phase::Paused {
                            (icon::RECORD, "Resume")
                        } else {
                            (icon::PAUSE, "Pause")
                        };
                        if ui
                            .add(
                                egui::Button::new(format!("{glyph}  {text}"))
                                    .min_size(egui::vec2(110.0, 38.0)),
                            )
                            .clicked()
                        {
                            self.toggle_pause();
                        }
                    }
                    Phase::Saving => {
                        ui.spinner();
                    }
                },
            );
        });
    }

    /// Tabs on the left; recording state stays visible from either tab.
    fn header(&mut self, ui: &mut egui::Ui) {
        let p = theme::palette(ui);
        ui.horizontal(|ui| {
            for (tab, glyph, name) in [
                (Tab::Record, icon::RECORD, "Record"),
                (Tab::Play, icon::PLAY, "Play"),
            ] {
                if tab_button(ui, self.tab == tab, glyph, name).clicked() {
                    self.tab = tab;
                }
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if self.tab == Tab::Play && self.busy() {
                    let paused = self.phase == Phase::Paused;
                    let colour = if paused { p.paused } else { p.tally };
                    let text = if paused {
                        "Recording paused"
                    } else {
                        "Recording"
                    };
                    let button = egui::Button::new(
                        egui::RichText::new(format!("{}  {text}", icon::RECORD)).color(colour),
                    )
                    .frame(false);
                    if ui
                        .add(button)
                        .on_hover_text("Go to the recording")
                        .clicked()
                    {
                        self.tab = Tab::Record;
                    }
                }
            });
        });
    }
}

/// The displays drawn at their real arrangement, as Windows' display settings
/// shows them: click one to choose it. While recording, the screens being
/// captured carry the tally colour.
fn monitor_map(
    ui: &mut egui::Ui,
    monitors: &[screenilux_core::Monitor],
    selected: &mut [bool],
    whole: bool,
    enabled: bool,
    phase: Phase,
    bubble: Option<screenilux_core::Rect>,
) {
    let p = theme::palette(ui);
    let Some(all) = screenilux_core::virtual_bounds(monitors) else {
        return;
    };
    let width = ui.available_width();
    let scale = (width / all.width as f32).min(150.0 / all.height as f32);
    let used = egui::vec2(all.width as f32 * scale, all.height as f32 * scale);
    let (area, _) = ui.allocate_exact_size(egui::vec2(width, used.y + 4.0), egui::Sense::hover());
    let origin = egui::pos2(area.center().x - used.x / 2.0, area.top() + 2.0);
    let map = |r: screenilux_core::Rect| {
        egui::Rect::from_min_size(
            origin
                + egui::vec2(
                    (r.left - all.left) as f32 * scale,
                    (r.top - all.top) as f32 * scale,
                ),
            egui::vec2(r.width as f32 * scale, r.height as f32 * scale),
        )
        .shrink(3.0)
    };
    let live = match phase {
        Phase::Recording => Some(p.tally),
        Phase::Paused => Some(p.paused),
        _ => None,
    };
    let clickable = enabled && !whole && monitors.len() > 1;

    for (i, m) in monitors.iter().enumerate() {
        let rect = map(m.bounds);
        let sense = if clickable {
            egui::Sense::click()
        } else {
            egui::Sense::hover()
        };
        let response = ui
            .interact(rect, ui.scope_id().with(("monitor", i)), sense)
            .on_hover_text(format!(
                "{}, {}×{}",
                m.label, m.bounds.width, m.bounds.height
            ));
        if response.clicked() {
            selected[i] = !selected[i];
            // Never leave nothing to record.
            if !selected.iter().any(|&on| on) {
                selected[i] = true;
            }
        }
        if clickable && response.hovered() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
        }
        let on = whole || selected[i];
        paint_monitor(ui, rect, m, on, live, clickable && response.hovered(), &p);
    }

    if whole {
        let colour = live.unwrap_or(p.signal);
        ui.painter().rect_stroke(
            map(all).expand(4.0),
            8.0,
            egui::Stroke::new(2.0, colour),
            egui::StrokeKind::Outside,
        );
    }

    // Where the camera bubble sits, so it is clear which screen carries it.
    if let Some(b) = bubble {
        let r = map(b).expand(3.0);
        let radius = (r.width().min(r.height()) / 2.0).clamp(3.0, 9.0);
        ui.painter().circle_filled(r.center(), radius, p.text);
    }
}

fn paint_monitor(
    ui: &egui::Ui,
    rect: egui::Rect,
    m: &screenilux_core::Monitor,
    on: bool,
    live: Option<egui::Color32>,
    hovered: bool,
    p: &Palette,
) {
    let painter = ui.painter();
    let fill = match (on, hovered) {
        (true, _) => theme::blend(p.raised, p.signal, 0.22),
        (false, true) => theme::blend(p.raised, p.text, 0.08),
        (false, false) => p.raised,
    };
    let stroke = match (on, live) {
        (true, Some(tally)) => egui::Stroke::new(2.5, tally),
        (true, None) => egui::Stroke::new(2.0, p.signal),
        (false, _) => egui::Stroke::new(1.0, p.stroke),
    };
    painter.rect(rect, 6.0, fill, stroke, egui::StrokeKind::Inside);

    // The screen's number, large, as Windows shows it; its size beneath.
    let number = m
        .label
        .rsplit(' ')
        .next()
        .filter(|n| n.chars().all(|c| c.is_ascii_digit()))
        .unwrap_or(&m.label)
        .to_string();
    let big = (rect.height() * 0.32).clamp(14.0, 34.0);
    let colour = if on { p.text } else { p.weak };
    painter.text(
        rect.center() - egui::vec2(0.0, big * 0.25),
        egui::Align2::CENTER_CENTER,
        number,
        egui::FontId::new(big, theme::light()),
        colour,
    );
    if rect.height() > 60.0 {
        let detail = if m.is_primary {
            format!("{}×{}, main", m.bounds.width, m.bounds.height)
        } else {
            format!("{}×{}", m.bounds.width, m.bounds.height)
        };
        painter.text(
            rect.center() + egui::vec2(0.0, big * 0.6),
            egui::Align2::CENTER_CENTER,
            detail,
            egui::FontId::proportional(11.5),
            p.weak,
        );
    }
    if let (true, Some(tally)) = (on, live) {
        painter.circle_filled(rect.right_top() + egui::vec2(-12.0, 12.0), 4.5, tally);
    }
}

/// A tab: text with an underline in the signal colour when selected.
fn tab_button(ui: &mut egui::Ui, selected: bool, glyph: &str, name: &str) -> egui::Response {
    let p = theme::palette(ui);
    let colour = if selected { p.text } else { p.weak };
    let font = if selected {
        egui::FontId::new(15.0, theme::strong())
    } else {
        egui::FontId::proportional(15.0)
    };
    let response = ui.add(
        egui::Button::new(
            egui::RichText::new(format!("{glyph}  {name}"))
                .font(font)
                .color(colour),
        )
        .frame(false)
        .min_size(egui::vec2(0.0, 30.0)),
    );
    if selected {
        let r = response.rect;
        ui.painter().line_segment(
            [
                egui::pos2(r.left() + 6.0, r.bottom() + 7.0),
                egui::pos2(r.right() - 6.0, r.bottom() + 7.0),
            ],
            egui::Stroke::new(2.5, p.signal),
        );
    }
    response
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.probes.poll();
        self.follow_displays(&ctx);

        // A recording dropped onto the window opens in the player.
        if let Some(path) = dropped_path(&ctx)
            && playable(&path)
        {
            self.play(path, &ctx);
        }

        if !self.busy() {
            self.compact = false;
        }
        self.dress_window(&ctx);
        if self.compact {
            self.sizer.update(&ctx, Fit::Bar);
            self.bar(ui, &ctx);
            self.show_overlay(&ctx);
            ctx.request_repaint_after(Duration::from_millis(100));
            return;
        }

        let p = theme::palette(ui);
        egui::Panel::top("header")
            .frame(egui::Frame::new().fill(p.base).inner_margin(egui::Margin {
                left: 16,
                right: 16,
                top: 10,
                bottom: 8,
            }))
            .show_separator_line(true)
            .show(ui, |ui| self.header(ui));

        let fit = match self.tab {
            Tab::Record => Fit::Settings,
            Tab::Play => Fit::Stage,
        };
        self.sizer.update(&ctx, fit);

        match self.tab {
            Tab::Record => self.record_tab(ui, &ctx),
            Tab::Play => {
                let out_dir = self.settings.out_dir.clone();
                self.player.ui(ui, &ctx, &out_dir);
            }
        }

        self.show_overlay(&ctx);

        // While playing, each new frame asks for its own repaint; this only
        // keeps the clocks moving between them.
        let tick = if self.busy() || self.player.is_playing() {
            100
        } else {
            250
        };
        ctx.request_repaint_after(Duration::from_millis(tick));
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
