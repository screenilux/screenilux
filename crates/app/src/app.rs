//! The main window: the Record tab, the recording bar, and the header both
//! tabs share.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use eframe::egui;
use screenilux_recorder::{Session, SessionConfig};

use crate::edit_view::EditView;
use crate::overlay::Overlay;
use crate::player_view::{PlayerView, dropped_path, friendly_name, playable};
use crate::previews::Previews;
use crate::sharing::{self, Asked, Link, Mode, Shares, Sharing};
use crate::sizing::{self, Fit, Sizer};
use crate::state::{FPS_CHOICES, Phase, Prefs, Probes, Runtime, Settings};
use crate::theme::{self, ON_DARK, icon};

/// Space between the window's edge and the Record tab's content.
const MARGIN: i8 = 24;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Tab {
    Record,
    /// The library, and the editor that opens out of it.
    Recordings,
    Settings,
}

pub struct App {
    tab: Tab,
    sizer: Sizer,
    /// The window title as last set, so it is only sent when it changes.
    titled: String,
    tour: crate::tour::Tour,
    player: PlayerView,
    editor: EditView,
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
    /// Every shared recording and its link.
    shares: Shares,
    /// What is kept between runs besides sharing: where recordings go and
    /// whether a copy stays here once shared.
    prefs: Prefs,
    /// How tall the Record tab last wanted to be; the Recordings tab keeps
    /// to it.
    /// The player window's icon, made once.
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
    /// When Discard was clicked once, waiting for the click that confirms it.
    discard_armed: Option<Instant>,
}

/// How long Discard waits for its second click before standing down.
const DISCARD_WINDOW: Duration = Duration::from_secs(3);

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
        let mut app = Self {
            tab: Tab::Record,
            sizer: Sizer::new(),
            titled: String::new(),
            tour: crate::tour::Tour::new(),
            player: PlayerView::new(),
            editor: EditView::new(),
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
            shares: Shares::load(),
            prefs: Prefs::default(),
            started_at: None,
            more: false,
            hwnd,
            compact: false,
            alpha: 255,
            rounded: false,
            _displays: displays,
            display_rx,
            displays_changed: None,
            discard_armed: None,
        };
        app.apply_prefs();
        app
    }

    /// Open `path` in the player window.
    pub fn play(&mut self, path: PathBuf, ctx: &egui::Context) {
        self.player.open(path, ctx);
        // A recording is watched and shaped inside Recordings, so opening
        // one goes there. Without this, Play on the saved card and a
        // recording double-clicked in Explorer both opened it somewhere
        // nobody was looking.
        self.tab = Tab::Recordings;
    }

    /// Where recordings go and whether a copy stays here, as last chosen.
    fn apply_prefs(&mut self) {
        self.prefs = Prefs::load();
        if let Some(dir) = self.prefs.out_dir.clone() {
            self.settings.out_dir = dir;
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
        let targets = self.settings.targets();
        if targets.is_empty() {
            self.status = "Pick a screen to record".into();
            return;
        }
        if let Err(e) = std::fs::create_dir_all(&self.settings.out_dir) {
            self.status = format!("Cannot use that folder: {e}");
            return;
        }

        // The camera is released when a recording ends, so a setting left
        // switched on has to open it again here.
        if self.settings.effects.camera && self.runtime.camera_feed.is_none() {
            self.toggle_camera(true);
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
        self.leave_bar();
        // Painted once so the window shows "Saving…" before the blocking work.
        ctx.request_repaint();

        let outdir = self.settings.out_dir.clone();
        match session.finish(&outdir) {
            Ok(path) => {
                self.status.clear();
                self.share_new(&path);
                self.last_save = Some(path);
                self.player.recordings_changed();
            }
            Err(e) => self.status = format!("Could not save: {e}"),
        }
        self.wind_down();
    }

    /// Asked for on the first click of Discard, done on the second. A
    /// recording is too much to lose to one slip of the mouse beside Stop.
    fn discard(&mut self) {
        let armed = self
            .discard_armed
            .is_some_and(|at| at.elapsed() < DISCARD_WINDOW);
        if !armed {
            self.discard_armed = Some(Instant::now());
            return;
        }
        let Some(session) = self.session.take() else {
            return;
        };
        self.leave_bar();
        session.cancel();
        self.status = "Recording discarded".into();
        self.wind_down();
    }

    /// Whether Discard has been clicked once and is waiting for the second.
    fn discard_pending(&self, ctx: &egui::Context) -> bool {
        match self.discard_armed {
            Some(at) if at.elapsed() < DISCARD_WINDOW => {
                ctx.request_repaint_after(DISCARD_WINDOW - at.elapsed());
                true
            }
            _ => false,
        }
    }

    /// Back to the full window, visible to capture again.
    fn leave_bar(&mut self) {
        self.compact = false;
        self.sizer.forget_bar();
        self.set_captured(true);
    }

    /// What every recording's end shares, kept or not.
    fn wind_down(&mut self) {
        // Let the camera go. Holding it open between recordings leaves the
        // light on and the device busy for other apps, for a preview nobody
        // asked to keep; the setting stays on and `start` opens it again.
        if self.settings.effects.camera {
            self.runtime.stop_camera();
            self.overlay = None;
        }
        self.phase = Phase::Idle;
        self.started_at = None;
        self.discard_armed = None;
    }

    /// Upload a recording, if sharing is set up. The link is copied as soon
    /// as it exists, before the upload finishes: the page it opens waits for
    /// the rest.
    fn share(&mut self, path: &Path) {
        if let Some(destination) = self.sharing.destination() {
            self.shares.start(path, destination, false);
        }
    }

    /// Share a recording just made: like [`share`](Self::share), but if no
    /// copy is to stay here, the file goes once its upload has finished.
    fn share_new(&mut self, path: &Path) {
        if let Some(destination) = self.sharing.destination() {
            self.shares.start(path, destination, !self.prefs.keep_local);
        }
    }

    /// Whether sharing is on and has what it needs.
    fn can_share(&self) -> bool {
        match self.sharing.mode {
            Mode::Off => false,
            Mode::Screenilux => true,
            Mode::Bucket => self.sharing.bucket.is_complete(),
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
                    let (fill, tip) = if self.discard_pending(ctx) {
                        (p.signal, "Click again to discard this recording")
                    } else {
                        (egui::Color32::TRANSPARENT, "Discard this recording")
                    };
                    if theme::round_button(ui, icon::DISCARD, 40.0, fill, ON_DARK, tip).clicked() {
                        self.discard();
                    }
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

    /// The Record tab. Returns how tall it would like to be, to show
    /// everything without scrolling.
    /// The Record tab: a question, the screens it is asking about, and one
    /// row along the bottom holding what gets recorded with them.
    fn record_tab(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        let p = theme::palette(ui);
        egui::Panel::bottom("transport")
            .frame(egui::Frame::new().fill(p.base).inner_margin(egui::Margin {
                left: MARGIN,
                right: MARGIN,
                top: 12,
                bottom: 18,
            }))
            .show_separator_line(false)
            .show(ui, |ui| self.deck(ui, ctx));

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
                        stage(ui, |ui| {
                            self.warnings(ui);
                            if self.phase == Phase::Idle
                                && let Some(saved) =
                                    self.last_save.clone().filter(|_| self.status.is_empty())
                            {
                                self.saved_card(ui, ctx, saved);
                                ui.add_space(10.0);
                            }
                            if self.phase == Phase::Idle {
                                self.ask(ui);
                            }
                            self.screens(ui);
                            ui.add_space(14.0);
                        });
                    });
            });
    }

    /// The question at the top of the Record tab. It names the job rather
    /// than the controls, so the screens below it read as the answer.
    fn ask(&mut self, ui: &mut egui::Ui) {
        let p = theme::palette(ui);
        let many = self.settings.monitors.len() > 1;
        ui.vertical_centered(|ui| {
            ui.add_space(26.0);
            ui.label(
                egui::RichText::new("What would you like to record?")
                    .font(egui::FontId::new(27.0, theme::bold()))
                    .color(p.text),
            );
            ui.add_space(6.0);
            theme::hint(
                ui,
                if many {
                    "Tick a screen, or several. Each one is recorded at its own size."
                } else {
                    "Your screen, recorded at its own size."
                },
            );
            ui.add_space(26.0);
        });
    }

    /// The row along the bottom: what is recorded alongside the screens, and
    /// the one button that starts it.
    fn deck(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        if self.phase != Phase::Idle {
            stage(ui, |ui| self.transport(ui, ctx));
            return;
        }
        // The deck runs the width of the window while the screens stay in
        // the middle: a row of switches has no reading measure to keep, and
        // crowding them would make the one that is on harder to spot.
        const ROW: f32 = 44.0;
        const BUTTON: f32 = 196.0;
        const GAP: f32 = 12.0;
        let full = ui.available_width();
        let beside = full - BUTTON - GAP;

        if beside < 520.0 {
            // Too narrow for both on one line: the switches wrap and the
            // button takes a line of its own underneath.
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing = egui::vec2(8.0, 8.0);
                self.sources(ui, false);
            });
            ui.add_space(10.0);
            if theme::big_button(ui, "Start recording", true).clicked() {
                self.start(ctx);
            }
        } else {
            // The row is split by hand rather than by nesting one layout
            // inside its opposite: the button's room is taken out first, so
            // the switches cannot run under it and the row keeps its height.
            let (band, _) = ui.allocate_exact_size(egui::vec2(full, ROW), egui::Sense::hover());
            let left = egui::Rect::from_min_size(band.min, egui::vec2(beside.max(0.0), ROW));
            let right = egui::Rect::from_min_size(
                egui::pos2(band.right() - BUTTON, band.top()),
                egui::vec2(BUTTON, ROW),
            );

            let mut switches = ui.new_child(
                egui::UiBuilder::new()
                    .max_rect(left)
                    .layout(egui::Layout::left_to_right(egui::Align::Center)),
            );
            switches.spacing_mut().item_spacing.x = 10.0;
            // The device names are the first thing to go when the row is
            // tight: which switch is on matters more than what it would use.
            self.sources(&mut switches, beside > 760.0);

            let mut go = ui.new_child(
                egui::UiBuilder::new()
                    .max_rect(right)
                    .layout(egui::Layout::right_to_left(egui::Align::Center)),
            );
            if theme::go_button(&mut go, "Start recording").clicked() {
                self.start(ctx);
            }
        }

        ui.add_space(10.0);
        self.more_options(ui);
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

    /// The camera, the microphone and the computer's own sound, each a pill
    /// that says what it would use and switches on and off.
    fn sources(&mut self, ui: &mut egui::Ui, names: bool) {
        let busy = self.busy();

        let camera_name = self
            .probes
            .cameras
            .get(self.settings.camera_device)
            .cloned()
            .unwrap_or_else(|| {
                if self.probes.ready {
                    "none found".into()
                } else {
                    "looking…".into()
                }
            });
        let can_camera = !busy && !self.probes.cameras.is_empty();
        let on = self.settings.effects.camera;
        let hit = ui.add_enabled_ui(can_camera, |ui| {
            theme::pill(
                ui,
                icon::CAMERA,
                if on { "Camera on" } else { "Camera off" },
                if names { &camera_name } else { "" },
                on,
            )
        });
        if hit.inner.clicked() {
            self.settings.effects.camera = !on;
            self.toggle_camera(!on);
        }

        let mic = self.probes.has_microphone;
        let detail = if mic {
            "default microphone"
        } else if self.probes.ready {
            "none found"
        } else {
            "looking…"
        };
        let on = self.settings.microphone && mic;
        let hit = ui.add_enabled_ui(!busy && mic, |ui| {
            theme::pill(
                ui,
                icon::MIC,
                if on {
                    "Microphone on"
                } else {
                    "Microphone off"
                },
                if names { detail } else { "" },
                on,
            )
        });
        if hit.inner.clicked() {
            self.settings.microphone = !self.settings.microphone;
        }

        let speakers = self.probes.has_system_sound;
        let detail = if speakers {
            "videos, calls, apps"
        } else if self.probes.ready {
            "none found"
        } else {
            "looking…"
        };
        let on = self.settings.system_sound && speakers;
        let hit = ui.add_enabled_ui(!busy && speakers, |ui| {
            theme::pill(
                ui,
                icon::SPEAKER,
                if on {
                    "System sound on"
                } else {
                    "System sound off"
                },
                if names { detail } else { "" },
                on,
            )
        });
        if hit.inner.clicked() {
            self.settings.system_sound = !self.settings.system_sound;
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
    }

    /// After a recording: what was saved, playing it, and its link.
    fn saved_card(&mut self, ui: &mut egui::Ui, ctx: &egui::Context, saved: PathBuf) {
        let p = theme::palette(ui);
        let name = saved
            .file_name()
            .map(|n| friendly_name(&n.to_string_lossy()))
            .unwrap_or_default();
        let link = self.shares.of(&saved);
        let can_share = self.can_share();
        let mut asked = None;
        let mut set_up = false;
        let mut dismissed = false;
        egui::Frame::new()
            .fill(p.surface)
            .stroke(egui::Stroke::new(1.0, p.stroke))
            .corner_radius(16)
            .inner_margin(egui::Margin::same(14))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new(icon::CHECK).size(16.0).color(p.signal));
                    // The buttons take their room first, so a long name is
                    // cut short instead of running under them.
                    let name_width = (ui.available_width() - 150.0).max(60.0);
                    ui.vertical(|ui| {
                        ui.set_max_width(name_width);
                        ui.spacing_mut().item_spacing.y = 1.0;
                        ui.label(
                            egui::RichText::new("Saved")
                                .font(egui::FontId::new(15.0, theme::strong())),
                        );
                        ui.add(
                            egui::Label::new(egui::RichText::new(name).size(13.0).color(p.weak))
                                .truncate(),
                        );
                    });
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let close = egui::Button::new(
                            egui::RichText::new("\u{2715}").size(11.0).color(p.weak),
                        )
                        .frame(false);
                        if ui.add(close).on_hover_text("Put this away").clicked() {
                            dismissed = true;
                        }
                        if !saved.exists() {
                            ui.label(
                                egui::RichText::new("Kept in the cloud only")
                                    .size(12.0)
                                    .color(p.weak),
                            );
                            return;
                        }
                        let folder =
                            egui::Button::new(egui::RichText::new(icon::FOLDER).color(p.text))
                                .fill(p.base)
                                .stroke(egui::Stroke::new(1.0, p.stroke))
                                .corner_radius(10)
                                .min_size(egui::vec2(34.0, 34.0));
                        if ui.add(folder).on_hover_text("Show in folder").clicked() {
                            screenilux_platform::reveal(&saved);
                        }
                        let play = egui::Button::new(
                            egui::RichText::new(format!("{}  Play", icon::PLAY))
                                .color(p.text)
                                .strong(),
                        )
                        .fill(p.base)
                        .stroke(egui::Stroke::new(1.0, p.stroke))
                        .corner_radius(10)
                        .min_size(egui::vec2(0.0, 34.0));
                        if ui.add(play).clicked() {
                            self.play(saved.clone(), ctx);
                        }
                    });
                });

                ui.add_space(4.0);
                ui.separator();
                ui.add_space(4.0);
                match link {
                    Link::None if can_share => {
                        if ui
                            .add(theme::outline(format!("{}  Share link", icon::LINK)))
                            .clicked()
                        {
                            asked = Some(Asked::Share);
                        }
                    }
                    Link::None => {
                        ui.horizontal(|ui| {
                            theme::hint(ui, "Want a link to send?");
                            if ui.link("Set up sharing").clicked() {
                                set_up = true;
                            }
                        });
                    }
                    link => asked = sharing::link_panel(ui, &link),
                }
            });
        if set_up {
            self.tab = Tab::Settings;
        }
        if dismissed {
            self.last_save = None;
        }
        match asked {
            Some(Asked::Share) => self.share(&saved),
            Some(Asked::Delete) => self.shares.delete(&saved, &self.sharing.bucket),
            _ => {}
        }
    }

    /// Where finished recordings are shared, in Settings.
    fn sharing_options(&mut self, ui: &mut egui::Ui) {
        let p = theme::palette(ui);
        let before = self.sharing.clone();
        ui.add_space(6.0);
        // The label above the choice, not beside it: the column is too
        // narrow for both on one line.
        ui.label("Share a link when you stop");
        ui.add_space(2.0);
        let modes = [Mode::Off, Mode::Screenilux, Mode::Bucket];
        let chosen = modes
            .iter()
            .position(|&m| m == self.sharing.mode)
            .unwrap_or(0);
        if let Some(i) = theme::segmented(ui, &["Off", "Screenilux", "My bucket"], chosen) {
            self.sharing.mode = modes[i];
        }
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
                    "Only people with the link can watch. Links keep working until the \
                     recording is deleted, and you can share up to 1GB a week.",
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
                {
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
                        let discard = if self.discard_pending(ctx) {
                            egui::RichText::new("Click again to discard").color(p.signal)
                        } else {
                            egui::RichText::new("Discard")
                        };
                        if ui
                            .add(theme::outline(discard))
                            .on_hover_text("Stop without saving anything")
                            .clicked()
                        {
                            self.discard();
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

    /// The mark, the tabs, and the way to Settings.
    fn header(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        let p = theme::palette(ui);
        let row = ui.horizontal(|ui| {
            theme::logo(ui, 26.0);
            ui.label(
                egui::RichText::new("Screenilux").font(egui::FontId::new(18.0, theme::bold())),
            );
            let names = ui.min_rect().right();
            let controls = ui
                .with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    // The window's own buttons sit where Windows would have put
                    // them, in the corner the hand already goes to.
                    crate::chrome::buttons(ui, ctx);
                    ui.add_space(10.0);
                    let on = self.tab == Tab::Settings;
                    let gear = egui::Button::new(
                        egui::RichText::new(icon::SETTINGS).size(15.0).color(if on {
                            p.signal
                        } else {
                            p.text
                        }),
                    )
                    .fill(if on { p.surface } else { p.base })
                    .stroke(egui::Stroke::new(1.0, if on { p.signal } else { p.stroke }))
                    .corner_radius(10)
                    .min_size(egui::vec2(34.0, 34.0));
                    if ui.add(gear).on_hover_text("Settings").clicked() {
                        self.tab = if on { Tab::Record } else { Tab::Settings };
                    }
                    ui.add_space(4.0);
                    let chosen = match self.tab {
                        Tab::Record => 0,
                        Tab::Recordings => 1,
                        Tab::Settings => 2,
                    };
                    match theme::segmented(ui, &["Record", "Recordings"], chosen) {
                        Some(0) => self.tab = Tab::Record,
                        Some(1) => self.tab = Tab::Recordings,
                        _ => {}
                    }
                    // Where the controls begin; the strip to drag ends here.
                    ui.min_rect().left()
                })
                .inner;
            (names, controls)
        });
        // Between the name and the first control: the only part of the row
        // that is not already something to press. Taking the whole right
        // side would put the strip on top of the buttons and swallow them.
        let (names, controls) = row.inner;
        let band = egui::Rect::from_min_max(
            egui::pos2(names, row.response.rect.top()),
            egui::pos2(controls.max(names), row.response.rect.bottom()),
        );
        crate::chrome::grab(ui, ctx, band);
    }

    /// The Edit tab: play a recording and shape it, in one place. The player
    /// draws the picture and its controls; the editor adds a strip of
    /// timeline and an effects panel beside it.
    fn edit_tab(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        let p = theme::palette(ui);
        let out_dir = self.settings.out_dir.clone();
        let can_share = self.can_share();
        // Disjoint fields, so both can be held at once.
        let App {
            player,
            editor,
            shares,
            titled,
            ..
        } = self;

        editor.follow(player);
        editor.poll_export(player);
        editor.show_live(player);
        // The timeline belongs in the picture's card, as its bottom band,
        // not in a panel of its own under it.
        player.set_footer(if player.is_full() {
            0.0
        } else {
            editor.timeline_height()
        });
        // One window, so its title is whatever is open in it.
        let want = player
            .title()
            .map_or_else(|| "Screenilux".to_string(), |t| format!("{t} - Screenilux"));
        if *titled != want {
            ctx.send_viewport_cmd(egui::ViewportCommand::Title(want.clone()));
            *titled = want;
        }
        // Plain clicks on the paused picture are the editor's while this tab
        // is showing, and nobody else's while it is not.
        player.set_editing(true);

        // Full screen is the picture and nothing else, so the panel stands
        // down rather than floating over the thing being watched.
        let mut side = None;
        if !player.is_full() {
            side = Some(
                egui::Panel::right("edit-side")
                    .exact_size(crate::player_view::SIDE_WIDTH)
                    .resizable(false)
                    .frame(egui::Frame::new().fill(p.base).inner_margin(egui::Margin {
                        left: 0,
                        right: MARGIN,
                        top: 0,
                        bottom: MARGIN,
                    }))
                    .show_separator_line(false)
                    .show(ui, |ui| {
                        egui::Frame::new()
                            .stroke(egui::Stroke::new(1.0, p.stroke))
                            .corner_radius(20)
                            .inner_margin(20)
                            .show(ui, |ui| {
                                ui.set_width(ui.available_width());
                                ui.set_height(ui.available_height());
                                editor.side_panel(ui, player);
                            });
                    })
                    .response
                    .rect,
            );
        }

        let open = player.shareable();
        let link = open.as_deref().map_or(Link::None, |path| shares.of(path));
        player.set_link(link, open.is_some() && can_share);
        player.ui(ui, ctx, &out_dir);
        if let Some(band) = player.footer_at() {
            let mut band_ui = ui.new_child(egui::UiBuilder::new().max_rect(band));
            editor.timeline(&mut band_ui, ctx, player);
        }
        match (player.take_asked(), open) {
            (Some(Asked::Share), Some(path)) => self.share(&path),
            (Some(Asked::Delete), Some(path)) => self.shares.delete(&path, &self.sharing.bucket),
            (Some(Asked::Export(format, shape)), _) => {
                self.editor.start_export(format, shape, &mut self.player);
            }
            // Going back puts the recording away, which is what returns the
            // tab to its list. Effects are written first: they are data
            // beside the recording, and losing them to a misread arrow
            // would be worse than keeping a change somebody tried out.
            (Some(Asked::Back), _) => {
                self.editor.save();
                self.player.close();
            }
            _ => {}
        }
        // Over everything, including the tour: an export is the one thing
        // that has to be waited for rather than worked around.
        self.editor.show_export(ctx);
        self.guide(ctx, side);
    }

    /// The tour, on the first visit to the Edit tab with something open. It
    /// points at the real panels, so it is drawn after them, once they have
    /// said where they are.
    fn guide(&mut self, ctx: &egui::Context, side: Option<egui::Rect>) {
        let ready = self.player.clock().is_some() && !self.player.is_full();
        if !self.prefs.toured && !self.tour.running() && ready {
            self.tour.start();
        }
        if !ready {
            return;
        }
        let stops = vec![
            crate::tour::Stop {
                spot: None,
                title: "This is where a recording becomes a demo",
                body: "Nothing you do here changes the recording. Effects are kept beside it and drawn on the way out, so you can undo any of it by deleting it.",
            },
            crate::tour::Stop {
                spot: self.player.stage(),
                title: "Click the picture to put an effect there",
                body: "Zoom, spotlight, blur, hide, or point at something. Where you click is what the effect is about, and on a two-screen recording it remembers which screen.",
            },
            crate::tour::Stop {
                spot: self.editor.track_at(),
                title: "Drag the timeline to pick a stretch",
                body: "Then cut it, speed it up, hold on a frame, caption it or mute it. Click it instead to move the playhead, and drag an effect's edge to re-time it.",
            },
            crate::tour::Stop {
                spot: side,
                title: "Everything you add lands here",
                body: "Click one to open it and change it. Save when you are happy — the effects are written beside the recording, not into it.",
            },
        ];
        if self.tour.show(ctx, &stops) {
            self.prefs.toured = true;
            self.prefs.save();
        }
    }

    /// The Recordings tab: open a file, or one saved here. Either plays in
    /// the player window. It keeps the Record tab's height, so switching
    /// between the two does not resize the window, and its list scrolls.
    /// The Recordings tab: the list of what has been recorded, and the
    /// editor that opens out of it. One tab rather than two, because
    /// picking a recording and working on it is one errand; the back arrow
    /// in the editor returns to the list.
    fn recordings_tab(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        if self.player.has_window() {
            self.edit_tab(ui, ctx);
            return;
        }
        let p = theme::palette(ui);
        let out_dir = self.settings.out_dir.clone();
        egui::CentralPanel::default_margins()
            .frame(egui::Frame::new().fill(p.base).inner_margin(egui::Margin {
                left: MARGIN,
                right: MARGIN,
                top: 4,
                bottom: 20,
            }))
            .show(ui, |ui| {
                let shares = &self.shares;
                self.player.library(ui, ctx, &out_dir, &|p| shares.of(p));
            });
    }

    /// Settings: only what is set once and left. Recording options stay on
    /// the Record tab, by the button they change.
    fn settings_tab(&mut self, ui: &mut egui::Ui) {
        let p = theme::palette(ui);
        egui::CentralPanel::default_margins()
            .frame(egui::Frame::new().fill(p.base).inner_margin(egui::Margin {
                left: MARGIN,
                right: MARGIN,
                top: 4,
                bottom: 20,
            }))
            .show(ui, |ui| {
                egui::ScrollArea::vertical()
                    .auto_shrink([false; 2])
                    .show(ui, |ui| {
                        column(ui, |ui| {
                            ui.add_enabled_ui(!self.busy(), |ui| self.settings_page(ui));
                        });
                    });
            });
    }

    fn settings_page(&mut self, ui: &mut egui::Ui) {
        let p = theme::palette(ui);
        let heading = |ui: &mut egui::Ui, text: &str| {
            ui.label(egui::RichText::new(text).font(egui::FontId::new(15.0, theme::strong())));
        };
        let before = self.prefs.clone();

        heading(ui, "Recordings");
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            ui.vertical(|ui| {
                ui.set_max_width((ui.available_width() - 150.0).max(80.0));
                ui.spacing_mut().item_spacing.y = 2.0;
                ui.label("Save to");
                ui.add(
                    egui::Label::new(
                        egui::RichText::new(self.settings.out_dir.display().to_string())
                            .size(12.0)
                            .color(p.weak),
                    )
                    .truncate(),
                )
                .on_hover_text(self.settings.out_dir.display().to_string());
            });
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.button("Open").clicked() {
                    let _ = std::fs::create_dir_all(&self.settings.out_dir);
                    screenilux_platform::reveal(&self.settings.out_dir);
                }
                if ui.button("Change").clicked()
                    && let Some(dir) = rfd::FileDialog::new()
                        .set_title("Save recordings to")
                        .set_directory(&self.settings.out_dir)
                        .pick_folder()
                {
                    self.settings.out_dir = dir.clone();
                    self.prefs.out_dir = Some(dir);
                    self.player.recordings_changed();
                }
            });
        });
        ui.add_space(6.0);
        let sharing_on = self.can_share();
        ui.add_enabled_ui(sharing_on, |ui| {
            let mut keep = self.prefs.keep_local || !sharing_on;
            theme::setting(ui, &mut keep, "Keep a copy on this computer");
            if sharing_on {
                self.prefs.keep_local = keep;
            }
        });
        theme::hint(
            ui,
            if !sharing_on {
                "Recordings stay on this computer while sharing is off."
            } else if self.prefs.keep_local {
                "Each recording stays here, and is uploaded too."
            } else {
                "Each recording is removed from this computer once its upload has \
                 finished."
            },
        );

        ui.add_space(18.0);
        heading(ui, "Sharing");
        self.sharing_options(ui);

        if self.prefs != before {
            self.prefs.save();
        }
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
        if let Some(link) = self.shares.poll() {
            ctx.copy_text(link);
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

        let header = {
            let p = theme::palette(ui);
            egui::Panel::top("header")
                .frame(egui::Frame::new().fill(p.base).inner_margin(egui::Margin {
                    left: MARGIN,
                    right: MARGIN,
                    top: 14,
                    bottom: 14,
                }))
                .show_separator_line(false)
                .show(ui, |ui| self.header(ui, &ctx))
                .response
                .rect
                .height()
        };

        self.sizer.update(&ctx, Fit::Window);
        // A frameless window has no border to pull, so it carries its own.
        crate::chrome::edges(&ctx);
        let _ = header;
        match self.tab {
            Tab::Record => self.record_tab(ui, &ctx),
            Tab::Recordings => self.recordings_tab(ui, &ctx),
            Tab::Settings => self.settings_tab(ui),
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

/// Lay `add` out at the width the Record tab's pictures want: wider than a
/// settings column, because a live preview is the thing being judged.
fn stage<R>(ui: &mut egui::Ui, add: impl FnOnce(&mut egui::Ui) -> R) -> R {
    hold(ui, sizing::STAGE, add)
}

/// Lay `add` out as a column of at most `sizing::COLUMN` points, centred in
/// whatever width there is. A full-screen window should not stretch a row of
/// settings across a metre of desk: the reach from a label to the switch it
/// belongs to is the thing being kept short.
fn column<R>(ui: &mut egui::Ui, add: impl FnOnce(&mut egui::Ui) -> R) -> R {
    hold(ui, sizing::COLUMN, add)
}

/// `add`, at most `most` points wide and centred in whatever room there is.
fn hold<R>(ui: &mut egui::Ui, most: f32, add: impl FnOnce(&mut egui::Ui) -> R) -> R {
    let width = ui.available_width().min(most);
    let side = ((ui.available_width() - width) / 2.0).max(0.0);
    let rect = egui::Rect::from_min_size(
        ui.max_rect().min + egui::vec2(side, 0.0),
        egui::vec2(width, ui.available_height()),
    );
    ui.scope_builder(egui::UiBuilder::new().max_rect(rect), |ui| {
        ui.set_width(width);
        add(ui)
    })
    .inner
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
