//! The main window: the Record tab, the recording bar, and the header both
//! tabs share.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use eframe::egui;
use screenilux_recorder::{Session, SessionConfig};
use screenilux_share::{State as ShareState, Upload};

use crate::overlay::Overlay;
use crate::player_view::{PlayerView, dropped_path, friendly_name, playable};
use crate::previews::Previews;
use crate::sharing::{self, Mode, Sharing};
use crate::sizing::{Fit, Sizer};
use crate::state::{FPS_CHOICES, Phase, Probes, Runtime, Settings};
use crate::theme::{self, ON_DARK, icon};

/// Space between the window's edge and the Record tab's content.
const MARGIN: i8 = 24;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Tab {
    Record,
    Play,
}

pub struct App {
    tab: Tab,
    sizer: Sizer,
    player: PlayerView,
    previews: Previews,
    settings: Settings,
    probes: Probes,
    runtime: Runtime,
    phase: Phase,
    session: Option<Session>,
    overlay: Option<Overlay>,
    status: String,
    last_save: Option<PathBuf>,
    /// Where finished recordings are shared to.
    sharing: Sharing,
    /// The last recording's upload, if it is being shared.
    upload: Option<Upload>,
    /// Its link has been put on the clipboard.
    link_copied: bool,
    /// Why the last recording could not be shared, if it could not start.
    share_error: Option<String>,
    started_at: Option<Instant>,
    /// "More options" is open.
    more: bool,
    /// The main window's handle, when the platform gave one.
    hwnd: Option<isize>,
    /// Shrunk to the recording bar.
    compact: bool,
    /// The window's opacity and corners as last set, so Windows is only
    /// asked when they change.
    alpha: u8,
    rounded: bool,
    /// Listens for displays coming and going; kept alive with the app.
    _displays: screenilux_platform::DisplayWatcher,
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
const BAR_ALPHA: u8 = 190;

impl App {
    pub fn new(hwnd: Option<isize>, ctx: &egui::Context) -> Self {
        let (tx, display_rx) = std::sync::mpsc::channel();
        let repaint = ctx.clone();
        let displays = screenilux_platform::DisplayWatcher::start(move || {
            let _ = tx.send(());
            repaint.request_repaint();
        });
        let runtime = Runtime::new();
        let probes = Probes::start();
        Self {
            tab: Tab::Record,
            sizer: Sizer::new(),
            player: PlayerView::new(),
            previews: Previews::start(ctx),
            settings: Settings::new(),
            probes,
            runtime,
            phase: Phase::Idle,
            session: None,
            overlay: None,
            status: String::new(),
            last_save: None,
            sharing: Sharing::load(),
            upload: None,
            link_copied: false,
            share_error: None,
            started_at: None,
            more: false,
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
            self.status = "Pick a screen to record".into();
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
            system_sound: self.settings.system_sound && self.probes.has_system_sound,
            click_sound: self.settings.click_sound,
            // Where the camera preview cannot be kept out of the capture it
            // is recorded as it is, so it is not drawn in a second time.
            effects: screenilux_core::EffectOptions {
                camera: self.settings.effects.camera && screenilux_platform::CAN_HIDE_FROM_CAPTURE,
                ..self.settings.effects
            },
            camera,
            pointer_events: Some(self.runtime.hook.pointer_events()),
        };

        match Session::start(config) {
            Ok(s) => {
                self.session = Some(s);
                self.phase = Phase::Recording;
                self.status.clear();
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
            }
            Phase::Paused => {
                session.resume();
                self.phase = Phase::Recording;
            }
            _ => {}
        }
    }

    fn stop(&mut self, ctx: &egui::Context) {
        let Some(session) = self.session.take() else {
            return;
        };
        self.phase = Phase::Saving;
        self.compact = false;
        self.set_captured(true);
        // Painted once so the window shows "Saving…" before the blocking work.
        ctx.request_repaint();

        let outdir = self.settings.out_dir.clone();
        match session.finish(&outdir) {
            Ok(path) => {
                self.status.clear();
                self.share(&path);
                self.last_save = Some(path);
                self.player.recordings_changed();
            }
            Err(e) => self.status = format!("Could not save: {e}"),
        }
        self.phase = Phase::Idle;
        self.started_at = None;
    }

    /// Upload a recording just saved, if sharing is set up. The link is
    /// copied as soon as it exists, before the upload finishes: the page it
    /// opens waits for the rest.
    fn share(&mut self, path: &Path) {
        self.upload = None;
        self.link_copied = false;
        self.share_error = None;
        let Some(destination) = self.sharing.destination() else {
            return;
        };
        match sharing::contents(path) {
            Ok(contents) => self.upload = Some(Upload::start(destination, contents)),
            Err(e) => self.share_error = Some(e),
        }
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
                screenilux_platform::include_in_capture(h)
            } else {
                screenilux_platform::exclude_from_capture(h)
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
                screenilux_platform::set_rounded(h, self.compact);
                self.rounded = self.compact;
            }
            if alpha != self.alpha {
                screenilux_platform::set_opacity(h, alpha);
                self.alpha = alpha;
            }
        }
    }

    /// The window shrunk to a dark pill at the bottom of the screen while
    /// recording: stop, the clock, pause, and a way back to the full window.
    /// Drag it anywhere to move it.
    fn bar(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        let p = theme::palette(ui);
        egui::CentralPanel::default_margins()
            .frame(
                egui::Frame::new()
                    .fill(p.bar)
                    .inner_margin(egui::Margin::symmetric(8, 0)),
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
                ui.horizontal_centered(|ui| {
                    ui.spacing_mut().item_spacing.x = 6.0;
                    if theme::round_button(ui, "", 40.0, p.signal, ON_DARK, "Stop and save")
                        .clicked()
                    {
                        self.stop(ctx);
                    }
                    // The stop square, painted so it sits dead centre.
                    let square = egui::Rect::from_center_size(
                        ui.min_rect().right_center() - egui::vec2(20.0, 0.0),
                        egui::vec2(12.0, 12.0),
                    );
                    ui.painter().rect_filled(square, 3.0, ON_DARK);
                    ui.add_space(6.0);
                    let (dot, _) =
                        ui.allocate_exact_size(egui::vec2(8.0, 8.0), egui::Sense::hover());
                    let lamp = if paused { p.soft } else { p.signal };
                    ui.painter().circle_filled(dot.center(), 4.0, lamp);
                    ui.label(
                        egui::RichText::new(bar_clock(self.recorded()))
                            .font(egui::FontId::new(17.0, theme::strong()))
                            .color(ON_DARK),
                    );
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if theme::round_button(
                            ui,
                            icon::EXPAND,
                            40.0,
                            egui::Color32::TRANSPARENT,
                            ON_DARK,
                            "Show the full window",
                        )
                        .clicked()
                        {
                            self.compact = false;
                        }
                        let (glyph, tip) = if paused {
                            (icon::PLAY, "Resume")
                        } else {
                            (icon::PAUSE, "Pause")
                        };
                        if theme::round_button(
                            ui,
                            glyph,
                            40.0,
                            egui::Color32::TRANSPARENT,
                            ON_DARK,
                            tip,
                        )
                        .clicked()
                        {
                            self.toggle_pause();
                        }
                        let (line, _) =
                            ui.allocate_exact_size(egui::vec2(1.0, 24.0), egui::Sense::hover());
                        ui.painter()
                            .rect_filled(line, 0.0, ON_DARK.gamma_multiply(0.2));
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
            .frame(egui::Frame::new().fill(p.base).inner_margin(egui::Margin {
                left: MARGIN,
                right: MARGIN,
                top: 12,
                bottom: 20,
            }))
            .show_separator_line(false)
            .show(ui, |ui| self.transport(ui, ctx));

        egui::CentralPanel::default_margins()
            .frame(egui::Frame::new().fill(p.base).inner_margin(egui::Margin {
                left: MARGIN,
                right: MARGIN,
                top: 4,
                bottom: 0,
            }))
            .show(ui, |ui| {
                egui::ScrollArea::vertical()
                    .auto_shrink([false; 2])
                    .show(ui, |ui| {
                        ui.spacing_mut().item_spacing.y = 10.0;
                        self.warnings(ui);
                        self.screens(ui);
                        ui.add_space(6.0);
                        self.sources(ui);
                        ui.add_space(4.0);
                        self.more_options(ui);
                        ui.add_space(8.0);
                    });
            });
    }

    fn warnings(&self, ui: &mut egui::Ui) {
        if let Some(err) = self
            .overlay
            .as_ref()
            .and_then(|o| o.exclusion_error.clone())
        {
            theme::notice(ui, err);
        }
    }

    /// One tile per screen, in their left-to-right order: a live thumbnail
    /// over a tick box and the screen's name. Click a tile to tick it.
    fn screens(&mut self, ui: &mut egui::Ui) {
        let busy = self.busy();
        let s = &self.settings;
        if s.monitors.is_empty() {
            theme::hint(
                ui,
                "No displays were found. One will show here as soon as it is connected.",
            );
            return;
        }
        let bubble = self
            .overlay
            .as_ref()
            .filter(|_| s.effects.camera)
            .map(|o| o.position());
        let whole = s.whole_desktop && s.monitors.len() > 1;
        let mut order: Vec<usize> = (0..s.monitors.len()).collect();
        order.sort_by_key(|&i| (s.monitors[i].bounds.left, s.monitors[i].bounds.top));
        let live = !busy && self.tab == Tab::Record;

        let columns = if s.monitors.len() == 1 { 1 } else { 2 };
        let gap = 12.0;
        let width = (ui.available_width() - gap * (columns - 1) as f32) / columns as f32;
        let mut toggled = None;
        for row in order.chunks(columns) {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = gap;
                for &i in row {
                    let m = &s.monitors[i];
                    let on = whole || s.selected[i];
                    let clickable = !busy && !whole && s.monitors.len() > 1;
                    let tile = Tile {
                        monitor: m,
                        detail: where_text(m, &s.monitors),
                        on,
                        clickable,
                        live,
                        preview: self.previews.get(m.bounds),
                        bubble: bubble.filter(|b| b.intersects(&m.bounds)),
                    };
                    if tile.show(ui, width).clicked() {
                        toggled = Some(i);
                    }
                }
            });
        }
        if let Some(i) = toggled {
            let sel = &mut self.settings.selected;
            sel[i] = !sel[i];
            // Never leave nothing to record.
            if !sel.iter().any(|&on| on) {
                sel[i] = true;
            }
        }

        let s = &self.settings;
        let chosen = s.selected.iter().filter(|&&on| on).count();
        let summary = if whole {
            "Every screen together, as one wide video.".to_string()
        } else if chosen > 1 {
            "Each screen is its own video, saved together as one file.".to_string()
        } else if s.monitors.len() > 1 {
            "One screen, saved as an MP4 that plays anywhere. Tick another to add it.".to_string()
        } else {
            "Saved as an MP4 that plays anywhere.".to_string()
        };
        ui.vertical_centered(|ui| theme::hint(ui, &summary));
    }

    /// The camera, the microphone and the computer's own sound, each a row
    /// that switches on and off.
    fn sources(&mut self, ui: &mut egui::Ui) {
        let busy = self.busy();
        let camera_name = self
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
        let can_camera = !busy && !self.probes.cameras.is_empty();
        let on = self.settings.effects.camera;
        let row = ui.add_enabled_ui(can_camera, |ui| {
            theme::source_row(ui, icon::CAMERA, "Camera", &camera_name, on)
        });
        if row.inner.clicked() {
            self.settings.effects.camera = !on;
            self.toggle_camera(!on);
        }

        let mic = self.probes.has_microphone;
        let detail = if mic {
            "Default microphone"
        } else if self.probes.ready {
            "No microphone found"
        } else {
            "Looking for a microphone…"
        };
        let on = self.settings.microphone && mic;
        let row = ui.add_enabled_ui(!busy && mic, |ui| {
            theme::source_row(ui, icon::MIC, "Microphone", detail, on)
        });
        if row.inner.clicked() {
            self.settings.microphone = !self.settings.microphone;
        }

        let speakers = self.probes.has_system_sound;
        let detail = if speakers {
            "What your PC plays: videos, calls, apps"
        } else if self.probes.ready {
            "No speakers or headphones found"
        } else {
            "Looking for speakers…"
        };
        let on = self.settings.system_sound && speakers;
        let row = ui.add_enabled_ui(!busy && speakers, |ui| {
            theme::source_row(ui, icon::SPEAKER, "System sound", detail, on)
        });
        if row.inner.clicked() {
            self.settings.system_sound = !self.settings.system_sound;
        }
        if self.settings.effects.camera {
            theme::hint(
                ui,
                "Drag the camera bubble anywhere, even while recording. It is recorded into \
                 the screen it sits on.",
            );
        }
    }

    /// Everything else, folded away under one row that says what is set.
    fn more_options(&mut self, ui: &mut egui::Ui) {
        let p = theme::palette(ui);
        let s = &self.settings;
        let mut bits = Vec::new();
        if s.effects.cursor {
            bits.push("pointer shown".to_string());
        }
        if s.click_sound {
            bits.push("click sound".to_string());
        }
        bits.push(format!("{} fps", s.fps));
        let mut summary = bits.join(", ");
        if let Some(first) = summary.get_mut(0..1) {
            first.make_ascii_uppercase();
        }

        let width = ui.available_width();
        let (rect, header) = ui.allocate_exact_size(egui::vec2(width, 48.0), egui::Sense::click());
        header.widget_info(|| {
            egui::WidgetInfo::selected(egui::Role::Button, true, self.more, "More options")
        });
        if header.hovered() {
            ui.painter().rect_filled(rect, 12.0, p.surface);
        }
        let painter = ui.painter();
        let title = painter.layout_no_wrap(
            "More options".into(),
            egui::FontId::new(14.0, theme::strong()),
            p.text,
        );
        let title_w = title.size().x;
        painter.galley(
            egui::pos2(rect.left() + 6.0, rect.center().y - title.size().y / 2.0),
            title,
            p.text,
        );
        let room = rect.width() - title_w - 60.0;
        let summary = painter.layout(
            summary,
            egui::FontId::proportional(12.0),
            p.weak,
            f32::INFINITY,
        );
        if summary.size().x <= room {
            painter.galley(
                egui::pos2(
                    rect.left() + title_w + 16.0,
                    rect.center().y - summary.size().y / 2.0,
                ),
                summary,
                p.weak,
            );
        }
        painter.text(
            egui::pos2(rect.right() - 10.0, rect.center().y),
            egui::Align2::RIGHT_CENTER,
            if self.more {
                icon::CHEVRON_UP
            } else {
                icon::CHEVRON_DOWN
            },
            egui::FontId::proportional(12.0),
            p.text,
        );
        if header.clicked() {
            self.more = !self.more;
        }
        if !self.more {
            return;
        }

        let busy = self.busy();
        let multi = self.settings.monitors.len() > 1;
        ui.add_enabled_ui(!busy, |ui| {
            ui.spacing_mut().item_spacing.y = 0.0;
            let s = &mut self.settings;
            let fx = &mut s.effects;
            theme::setting(ui, &mut fx.cursor, "Show the pointer");
            theme::setting(ui, &mut fx.zoom, "Zoom with Ctrl+Alt and the wheel");
            theme::setting(ui, &mut fx.trail, "Trail behind the pointer");
            theme::setting(ui, &mut s.click_sound, "Click sound");
            if multi {
                theme::setting(
                    ui,
                    &mut s.whole_desktop,
                    "Record the whole desktop as one video",
                );
            }
            theme::setting(ui, &mut s.shrink, "Shrink to a bar while recording");

            ui.add_space(6.0);
            ui.horizontal(|ui| {
                ui.label("Frame rate");
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let labels: Vec<String> = FPS_CHOICES.iter().map(|f| f.to_string()).collect();
                    let labels: Vec<&str> = labels.iter().map(String::as_str).collect();
                    let chosen = FPS_CHOICES.iter().position(|&f| f == s.fps).unwrap_or(0);
                    if let Some(i) = theme::segmented(ui, &labels, chosen) {
                        s.fps = FPS_CHOICES[i];
                    }
                });
            });
        });

        if self.probes.cameras.len() > 1 {
            ui.add_space(6.0);
            ui.add_enabled_ui(!busy, |ui| {
                ui.horizontal(|ui| {
                    ui.label("Camera");
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let current = self.probes.cameras[self.settings.camera_device].clone();
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
                        if changed {
                            self.restart_camera();
                        }
                    });
                });
            });
        }

        ui.add_space(10.0);
        ui.horizontal(|ui| {
            ui.vertical(|ui| {
                ui.spacing_mut().item_spacing.y = 2.0;
                ui.label("Save to");
                ui.label(
                    egui::RichText::new(self.settings.out_dir.display().to_string())
                        .size(12.0)
                        .color(p.weak),
                );
            });
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.button("Open").clicked() {
                    let _ = std::fs::create_dir_all(&self.settings.out_dir);
                    screenilux_platform::reveal(&self.settings.out_dir);
                }
                if ui.add_enabled(!busy, egui::Button::new("Change")).clicked()
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
        ui.add_enabled_ui(!self.busy(), |ui| self.sharing_options(ui));
    }

    /// How sharing the last recording is going, and its link.
    fn share_status(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        let p = theme::palette(ui);
        if let Some(e) = &self.share_error {
            theme::notice(ui, format!("Could not share it: {e}"));
            return;
        }
        let Some(progress) = self.upload.as_ref().map(Upload::progress) else {
            return;
        };
        let line = match &progress.state {
            ShareState::Starting => "Getting a link…".to_string(),
            ShareState::Uploading => {
                let done = (progress.uploaded * 100)
                    .checked_div(progress.total)
                    .unwrap_or(0);
                format!("Link copied. Uploading, {done}%")
            }
            ShareState::Ready => "Link copied. Ready to watch".to_string(),
            ShareState::Failed(e) => {
                theme::notice(ui, format!("Could not share it: {e}"));
                return;
            }
        };
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new(line).size(13.0).color(p.weak));
            if let Some(link) = progress.link {
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.link("Open").clicked() {
                        ctx.open_url(egui::OpenUrl::new_tab(&link));
                    }
                    if ui.link("Copy link").clicked() {
                        ctx.copy_text(link);
                    }
                });
            }
        });
    }

    /// Where finished recordings are shared, in More options.
    fn sharing_options(&mut self, ui: &mut egui::Ui) {
        let p = theme::palette(ui);
        let before = self.sharing.clone();
        ui.add_space(10.0);
        ui.horizontal(|ui| {
            ui.label("Share a link when you stop");
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let modes = [Mode::Off, Mode::Screenilux, Mode::Bucket];
                let chosen = modes
                    .iter()
                    .position(|&m| m == self.sharing.mode)
                    .unwrap_or(0);
                if let Some(i) = theme::segmented(ui, &["Off", "Screenilux", "My bucket"], chosen) {
                    self.sharing.mode = modes[i];
                }
            });
        });
        let field =
            |ui: &mut egui::Ui, label: &str, value: &mut String, hint: &str, secret: bool| {
                ui.add_space(6.0);
                ui.label(egui::RichText::new(label).size(12.0).color(p.weak));
                ui.add(
                    egui::TextEdit::singleline(value)
                        .hint_text(hint)
                        .password(secret)
                        .desired_width(f32::INFINITY),
                );
            };
        match self.sharing.mode {
            Mode::Off => {}
            Mode::Screenilux => {
                ui.add_space(4.0);
                theme::hint(
                    ui,
                    "Links last 7 days, for recordings up to 4GB. Anyone with the link \
                     can watch.",
                );
            }
            Mode::Bucket => {
                let b = &mut self.sharing.bucket;
                field(
                    ui,
                    "S3 address",
                    &mut b.endpoint,
                    "https://<account>.r2.cloudflarestorage.com",
                    false,
                );
                field(ui, "Bucket", &mut b.bucket, "recordings", false);
                field(ui, "Region", &mut b.region, "auto", false);
                field(ui, "Access key ID", &mut b.access_key, "", false);
                field(ui, "Secret access key", &mut b.secret_key, "", true);
                field(
                    ui,
                    "Public address",
                    &mut b.public_url,
                    "https://pub-….r2.dev",
                    false,
                );
                field(ui, "Folder", &mut b.folder, "screenilux", false);
                ui.add_space(4.0);
                theme::hint(
                    ui,
                    "Any S3-compatible storage: Cloudflare R2, Amazon S3, Backblaze B2, \
                     Wasabi. Anyone must be able to read its files at the public address, \
                     and it must let the Screenilux site read them (CORS).",
                );
            }
        }
        if self.sharing != before {
            self.sharing.save();
        }
    }

    /// The bottom of the Record tab: the main button, and what happened last.
    fn transport(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        let p = theme::palette(ui);
        match self.phase {
            Phase::Idle => {
                if theme::big_button(ui, "Start recording", true).clicked() {
                    self.start(ctx);
                }
                ui.add_space(2.0);
                if let Some(saved) = self.last_save.clone().filter(|_| self.status.is_empty()) {
                    ui.horizontal(|ui| {
                        let name = saved
                            .file_name()
                            .map(|n| friendly_name(&n.to_string_lossy()))
                            .unwrap_or_default();
                        ui.label(
                            egui::RichText::new(format!("Saved {name}"))
                                .size(13.0)
                                .color(p.weak),
                        );
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if ui.link("Show in folder").clicked() {
                                screenilux_platform::reveal(&saved);
                            }
                            if ui.link("Play it").clicked() {
                                self.play(saved.clone(), ctx);
                            }
                        });
                    });
                    self.share_status(ui, ctx);
                } else {
                    let line = if self.status.is_empty() {
                        "Saved to Videos\\ScreenRec when you stop".to_string()
                    } else {
                        self.status.clone()
                    };
                    let line = if self.status.is_empty()
                        && !self.settings.out_dir.ends_with("Videos\\ScreenRec")
                    {
                        format!("Saved to {} when you stop", self.settings.out_dir.display())
                    } else {
                        line
                    };
                    ui.vertical_centered(|ui| theme::hint(ui, &line));
                }
            }
            Phase::Recording | Phase::Paused => {
                let paused = self.phase == Phase::Paused;
                ui.horizontal(|ui| {
                    let (dot, _) =
                        ui.allocate_exact_size(egui::vec2(10.0, 10.0), egui::Sense::hover());
                    ui.painter().circle_filled(
                        dot.center(),
                        5.0,
                        if paused { p.soft } else { p.signal },
                    );
                    ui.label(
                        egui::RichText::new(bar_clock(self.recorded()))
                            .font(egui::FontId::new(22.0, theme::strong())),
                    );
                    ui.label(
                        egui::RichText::new(if paused { "Paused" } else { "Recording" })
                            .color(p.weak),
                    );
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let text = if paused { "Resume" } else { "Pause" };
                        if ui.add(theme::outline(text)).clicked() {
                            self.toggle_pause();
                        }
                    });
                });
                if theme::big_button(ui, "Stop and save", false).clicked() {
                    self.stop(ctx);
                }
            }
            Phase::Saving => {
                ui.vertical_centered(|ui| {
                    ui.add_space(14.0);
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label("Saving…");
                    });
                });
            }
        }
    }

    /// The mark and the tabs. On Play there is room for the open button too,
    /// and a way back to a recording in progress.
    fn header(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        let p = theme::palette(ui);
        ui.horizontal(|ui| {
            theme::logo(ui, 26.0);
            ui.label(
                egui::RichText::new("Screenilux").font(egui::FontId::new(18.0, theme::bold())),
            );
            let tabs = |ui: &mut egui::Ui, tab: &mut Tab| {
                let chosen = if *tab == Tab::Record { 0 } else { 1 };
                match theme::segmented(ui, &["Record", "Play"], chosen) {
                    Some(0) => *tab = Tab::Record,
                    Some(_) => *tab = Tab::Play,
                    None => {}
                }
            };
            if self.tab == Tab::Play {
                ui.add_space(16.0);
                tabs(ui, &mut self.tab);
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if self.tab == Tab::Record {
                    tabs(ui, &mut self.tab);
                    return;
                }
                if ui
                    .add(theme::outline(format!("{}  Open recording", icon::FOLDER)))
                    .clicked()
                {
                    self.player.pick_file(ctx);
                }
                if self.busy() {
                    let paused = self.phase == Phase::Paused;
                    let text = if paused {
                        "Recording paused"
                    } else {
                        "Recording"
                    };
                    let button = egui::Button::new(
                        egui::RichText::new(format!("●  {text}")).color(p.action),
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

/// One screen's tile on the Record tab.
struct Tile<'a> {
    monitor: &'a screenilux_core::Monitor,
    detail: String,
    on: bool,
    clickable: bool,
    /// The thumbnail is being refreshed.
    live: bool,
    preview: Option<&'a egui::TextureHandle>,
    /// The camera bubble, when it sits on this screen.
    bubble: Option<screenilux_core::Rect>,
}

impl Tile<'_> {
    fn show(&self, ui: &mut egui::Ui, width: f32) -> egui::Response {
        let p = theme::palette(ui);
        let b = self.monitor.bounds;
        let inset = 10.0;
        let picture_w = width - inset * 2.0;
        let picture_h = (picture_w * b.height as f32 / b.width.max(1) as f32).min(picture_w);
        let height = inset + picture_h + 10.0 + 36.0 + 10.0;
        let sense = if self.clickable {
            egui::Sense::click()
        } else {
            egui::Sense::hover()
        };
        let (rect, response) = ui.allocate_exact_size(egui::vec2(width, height), sense);
        response.widget_info(|| {
            egui::WidgetInfo::selected(
                egui::Role::CheckBox,
                self.clickable,
                self.on,
                &self.monitor.label,
            )
        });
        if self.clickable && response.hovered() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
        }
        if !ui.is_rect_visible(rect) {
            return response;
        }

        let hovered = self.clickable && response.hovered();
        let (fill, border) = match (self.on, hovered) {
            (true, false) => (p.surface, p.signal),
            (true, true) => (p.surface, p.action),
            (false, false) => (p.base, p.surface),
            (false, true) => (p.base, p.stroke),
        };
        let painter = ui.painter();
        painter.rect(
            rect,
            16.0,
            fill,
            egui::Stroke::new(2.0, border),
            egui::StrokeKind::Inside,
        );

        // The picture, faded when the screen is not chosen.
        let picture = egui::Rect::from_min_size(
            rect.min + egui::vec2(inset, inset),
            egui::vec2(picture_w, picture_h),
        );
        match self.preview {
            Some(tex) => {
                let tint = if self.on {
                    egui::Color32::WHITE
                } else {
                    egui::Color32::from_white_alpha(110)
                };
                egui::Image::new(tex)
                    .corner_radius(6)
                    .tint(tint)
                    .paint_at(ui, picture);
            }
            None => {
                painter.rect_filled(picture, 6.0, p.stage);
                painter.text(
                    picture.center(),
                    egui::Align2::CENTER_CENTER,
                    icon::MONITOR,
                    egui::FontId::proportional(28.0),
                    p.weak,
                );
            }
        }
        painter.rect_stroke(
            picture,
            6.0,
            egui::Stroke::new(1.0, p.stroke),
            egui::StrokeKind::Inside,
        );

        // Where the camera bubble sits, so it is clear which screen has it.
        if let Some(bub) = self.bubble {
            let sx = picture_w / b.width as f32;
            let sy = picture_h / b.height as f32;
            let centre = picture.min
                + egui::vec2(
                    (bub.left - b.left) as f32 * sx + bub.width as f32 * sx / 2.0,
                    (bub.top - b.top) as f32 * sy + bub.height as f32 * sy / 2.0,
                );
            let r = (bub.width as f32 * sx / 2.0).max(4.0);
            painter.circle(centre, r, p.soft, egui::Stroke::new(1.5, ON_DARK));
        }

        if self.live && self.preview.is_some() {
            let chip = painter.layout_no_wrap(
                "Live".into(),
                egui::FontId::new(11.0, theme::strong()),
                ON_DARK,
            );
            let size = egui::vec2(chip.size().x + 22.0, 18.0);
            let at = egui::Rect::from_min_size(
                egui::pos2(picture.right() - size.x - 6.0, picture.top() + 6.0),
                size,
            );
            painter.rect_filled(at, 9.0, p.bar.gamma_multiply(0.82));
            painter.circle_filled(at.left_center() + egui::vec2(9.0, 0.0), 2.5, p.soft);
            painter.galley(
                egui::pos2(at.left() + 15.0, at.center().y - chip.size().y / 2.0),
                chip,
                ON_DARK,
            );
        }

        // The tick box and the screen's name.
        let row_y = picture.bottom() + 10.0 + 18.0;
        let check = egui::Rect::from_center_size(
            egui::pos2(rect.left() + inset + 12.0, row_y),
            egui::vec2(20.0, 20.0),
        );
        theme::paint_checkbox(ui, check, self.on, self.clickable || self.on);
        let painter = ui.painter();
        painter.text(
            egui::pos2(check.right() + 10.0, row_y - 8.0),
            egui::Align2::LEFT_CENTER,
            &self.monitor.label,
            egui::FontId::new(14.0, theme::strong()),
            p.text,
        );
        painter.text(
            egui::pos2(check.right() + 10.0, row_y + 10.0),
            egui::Align2::LEFT_CENTER,
            &self.detail,
            egui::FontId::proportional(12.0),
            p.weak,
        );
        if response.has_focus() {
            painter.rect_stroke(
                rect.expand(2.0),
                18.0,
                egui::Stroke::new(2.0, p.action),
                egui::StrokeKind::Outside,
            );
        }
        response
    }
}

/// "Main, on the left": which screen this is, in words, for telling two
/// apart. With more than two, their sizes say it better.
fn where_text(m: &screenilux_core::Monitor, all: &[screenilux_core::Monitor]) -> String {
    let size = format!("{}×{}", m.bounds.width, m.bounds.height);
    let side = match all {
        [a, b] => {
            let other = if a == m { b } else { a };
            let (mx, my) = centre(m.bounds);
            let (ox, oy) = centre(other.bounds);
            Some(if (mx - ox).abs() >= (my - oy).abs() {
                if mx < ox {
                    "on the left"
                } else {
                    "on the right"
                }
            } else if my < oy {
                "above"
            } else {
                "below"
            })
        }
        _ => None,
    };
    match (m.is_primary, side) {
        (true, Some(side)) => format!("Main, {side}"),
        (false, Some(side)) => {
            let mut s = side.to_string();
            s[..1].make_ascii_uppercase();
            s
        }
        (true, None) => format!("Main, {size}"),
        (false, None) => size,
    }
}

fn centre(r: screenilux_core::Rect) -> (i64, i64) {
    (
        r.left as i64 + r.width as i64 / 2,
        r.top as i64 + r.height as i64 / 2,
    )
}

/// "0:07", "12:30", "1:02:05".
fn bar_clock(d: Duration) -> String {
    let s = d.as_secs();
    if s >= 3600 {
        format!("{}:{:02}:{:02}", s / 3600, (s % 3600) / 60, s % 60)
    } else {
        format!("{}:{:02}", s / 60, s % 60)
    }
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.probes.poll();
        self.follow_displays(&ctx);
        if !self.link_copied
            && let Some(link) = self.upload.as_ref().and_then(|u| u.progress().link)
        {
            ctx.copy_text(link);
            self.link_copied = true;
        }
        let areas: Vec<_> = self.settings.monitors.iter().map(|m| m.bounds).collect();
        let watching = self.tab == Tab::Record && !self.compact && self.phase == Phase::Idle;
        self.previews.update(&ctx, &areas, watching);

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

        let full = self.tab == Tab::Play && self.player.is_full();
        if !full {
            let p = theme::palette(ui);
            egui::Panel::top("header")
                .frame(egui::Frame::new().fill(p.base).inner_margin(egui::Margin {
                    left: MARGIN,
                    right: MARGIN,
                    top: 14,
                    bottom: 14,
                }))
                .show_separator_line(false)
                .show(ui, |ui| self.header(ui, &ctx));
        }

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

#[cfg(test)]
mod tests {
    use super::*;
    use screenilux_core::{Monitor, Rect};

    #[test]
    fn two_screens_are_told_apart_by_side() {
        let main = Monitor::new("Screen 2", Rect::new(0, 0, 1920, 1080), true);
        let right = Monitor::new("Screen 1", Rect::new(1920, 122, 1920, 1080), false);
        let all = vec![main.clone(), right.clone()];
        assert_eq!(where_text(&main, &all), "Main, on the left");
        assert_eq!(where_text(&right, &all), "On the right");
        let above = Monitor::new("Screen 3", Rect::new(0, -1080, 1920, 1080), false);
        let pair = vec![main.clone(), above.clone()];
        assert_eq!(where_text(&above, &pair), "Above");
        assert_eq!(
            where_text(&main, std::slice::from_ref(&main)),
            "Main, 1920×1080"
        );
    }

    #[test]
    fn the_bar_clock_reads_like_a_stopwatch() {
        assert_eq!(bar_clock(Duration::from_secs(7)), "0:07");
        assert_eq!(bar_clock(Duration::from_secs(750)), "12:30");
        assert_eq!(bar_clock(Duration::from_secs(3725)), "1:02:05");
    }
}
