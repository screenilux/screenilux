//! The Play tab: open a recording, watch every screen in step, and copy or
//! search the text in it. Full screen gives the picture the whole display,
//! with the controls floating over it.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc::{Receiver, channel};
use std::time::{Duration, Instant};

use eframe::egui;
use screenilux_core::playback::{Area, Layout, arrange};
use screenilux_core::text::{Hit, Page, WordRef};
use screenilux_player::Player;
use screenilux_player::open::{Opened, save_text};
use screenilux_player::text::{INDEX_INTERVAL, Indexer, TextWorker};

use crate::theme::{self, ON_DARK, Palette, icon};

/// Wait this long after the picture stops changing before reading its text,
/// so scrubbing does not queue OCR for every frame passed over.
const SETTLE: Duration = Duration::from_millis(200);
const GAP: f32 = 16.0;
/// Space between the window's edge and the content.
const MARGIN: i8 = 24;
/// The text panel's width.
const PANEL: f32 = 360.0;
/// In full screen, the controls hide after the pointer rests this long.
const IDLE: f32 = 2.5;

/// Telling screens apart on the timeline, in labels and in search results.
fn track_colour(p: &Palette, track: usize) -> egui::Color32 {
    [p.signal, p.soft, p.action, p.stroke][track % 4]
}

/// A recording found in the save folder, for the empty state.
struct Recent {
    path: PathBuf,
    name: String,
    detail: String,
}

struct Loaded {
    opened: Opened,
    player: Player,
    /// Per track: the feed serial uploaded, and the texture.
    textures: Vec<Option<(u64, egui::TextureHandle)>>,
}

/// Text read off a paused frame.
struct PageAt {
    serial: u64,
    page: Result<Page, String>,
}

#[derive(Default)]
struct Selection {
    track: usize,
    /// Drag start, in frame pixels.
    anchor: (f32, f32),
    words: Option<(WordRef, WordRef)>,
}

pub struct PlayerView {
    loaded: Option<Loaded>,
    opening: Option<(String, Receiver<Result<Opened, String>>)>,
    status: Option<String>,
    layout: Layout,
    /// In Auto, a screen chosen by jumping to a search result. Cleared on play,
    /// so following the pointer resumes.
    pinned: Option<usize>,
    /// The picture fills the display and the controls float over it.
    full: bool,
    /// In full screen, the text panel is showing.
    text_open: bool,

    worker: Option<TextWorker>,
    /// Per track: the serial a read was asked for.
    requested: HashMap<usize, u64>,
    pages: HashMap<usize, PageAt>,
    last_change: Instant,
    last_serials: Vec<u64>,
    selection: Option<Selection>,

    indexer: Option<Indexer>,
    query: String,
    hits: Vec<Hit>,
    /// Recordings in the save folder, read when first shown.
    recents: Option<Vec<Recent>>,
}

impl PlayerView {
    pub fn new() -> Self {
        Self {
            loaded: None,
            opening: None,
            status: None,
            layout: Layout::Auto,
            pinned: None,
            full: false,
            text_open: false,
            worker: None,
            requested: HashMap::new(),
            pages: HashMap::new(),
            last_change: Instant::now(),
            last_serials: Vec::new(),
            selection: None,
            indexer: None,
            query: String::new(),
            hits: Vec::new(),
            recents: None,
        }
    }

    /// Open a file off the UI thread. A package is unpacked first, which
    /// takes a moment for a long recording.
    pub fn open(&mut self, path: PathBuf, ctx: &egui::Context) {
        let (tx, rx) = channel();
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let ctx2 = ctx.clone();
        std::thread::Builder::new()
            .name("screenilux-open".into())
            .spawn(move || {
                let _ = tx.send(screenilux_player::open(&path).map_err(|e| e.to_string()));
                ctx2.request_repaint();
            })
            .ok();
        self.opening = Some((name, rx));
        self.status = None;
    }

    pub fn pick_file(&mut self, ctx: &egui::Context) {
        let picked = rfd::FileDialog::new()
            .set_title("Open a recording")
            .add_filter("Recordings", &["screenilux", "mp4", "json"])
            .add_filter("All files", &["*"])
            .pick_file();
        if let Some(path) = picked {
            self.open(path, ctx);
        }
    }

    fn finish_opening(&mut self, ctx: &egui::Context) {
        let Some((_, rx)) = &self.opening else { return };
        let Ok(result) = rx.try_recv() else { return };
        self.opening = None;
        match result {
            Ok(opened) => {
                // Drop the previous player first so its threads let go of
                // their files before new ones open.
                self.loaded = None;
                self.indexer = None;
                self.reset_text();
                self.hits.clear();
                let repaint_ctx = ctx.clone();
                let player = Player::start(
                    opened.recording.clone(),
                    opened.fps,
                    Arc::new(move || repaint_ctx.request_repaint()),
                );
                let n = opened.recording.tracks.len();
                ctx.send_viewport_cmd(egui::ViewportCommand::Title(format!(
                    "{} - Screenilux",
                    friendly_name(&opened.title)
                )));
                self.loaded = Some(Loaded {
                    opened,
                    player,
                    textures: (0..n).map(|_| None).collect(),
                });
                if !self.query.is_empty() {
                    self.run_search();
                }
            }
            Err(e) => self.status = Some(format!("Could not open it: {e}")),
        }
    }

    fn reset_text(&mut self) {
        self.requested.clear();
        self.pages.clear();
        self.selection = None;
        self.last_serials.clear();
    }

    fn run_search(&mut self) {
        self.hits = self
            .loaded
            .as_ref()
            .and_then(|l| l.opened.text.as_ref())
            .map(|idx| idx.search(&self.query))
            .unwrap_or_default();
    }

    pub fn ui(&mut self, ui: &mut egui::Ui, ctx: &egui::Context, out_dir: &Path) {
        self.finish_opening(ctx);
        self.poll_indexer();
        let p = theme::palette(ui);

        if self.loaded.is_none() {
            self.set_full(ctx, false);
            egui::CentralPanel::default_margins()
                .frame(egui::Frame::new().fill(p.base).inner_margin(24))
                .show(ui, |ui| self.empty_state(ui, ctx, out_dir));
            return;
        }
        if let Some(l) = &self.loaded
            && l.player.clock().settle()
        {
            ctx.request_repaint();
        }

        if self.full {
            self.full_screen(ui, ctx);
            self.handle_keys(ctx);
            return;
        }

        egui::Panel::right("play-text")
            .exact_size(PANEL + MARGIN as f32)
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
                        self.text_panel(ui);
                    });
            });

        egui::CentralPanel::default_margins()
            .frame(egui::Frame::new().fill(p.base).inner_margin(egui::Margin {
                left: MARGIN,
                right: MARGIN,
                top: 0,
                bottom: MARGIN,
            }))
            .show(ui, |ui| {
                ui.spacing_mut().item_spacing.y = 16.0;
                self.title_row(ui);
                if let Some(s) = &self.status {
                    theme::notice(ui, s.clone());
                }
                let card_h = (ui.available_height() - 56.0).max(200.0);
                let width = ui.available_width();
                let (card, _) =
                    ui.allocate_exact_size(egui::vec2(width, card_h), egui::Sense::hover());
                ui.painter().rect_filled(card, 20.0, p.stage);
                let stage = egui::Rect::from_min_max(
                    card.min + egui::vec2(16.0, 16.0),
                    egui::pos2(card.max.x - 16.0, card.max.y - 72.0),
                );
                let controls = egui::Rect::from_min_max(
                    egui::pos2(card.min.x + 16.0, card.max.y - 72.0),
                    egui::pos2(card.max.x - 16.0, card.max.y - 8.0),
                );
                let mut stage_ui = ui.new_child(egui::UiBuilder::new().max_rect(stage));
                self.video(&mut stage_ui, ctx, stage.size());
                let mut controls_ui = ui.new_child(
                    egui::UiBuilder::new()
                        .max_rect(controls)
                        .layout(egui::Layout::left_to_right(egui::Align::Center)),
                );
                self.controls_row(&mut controls_ui, ctx);
                self.copy_row(ui);
            });

        self.handle_keys(ctx);
    }

    /// The recording's name and size on the left, the layouts on the right.
    fn title_row(&mut self, ui: &mut egui::Ui) {
        let p = theme::palette(ui);
        ui.horizontal(|ui| {
            if let Some((name, _)) = &self.opening {
                ui.spinner();
                ui.label(format!("Opening {name}…"));
                return;
            }
            let Some(l) = &self.loaded else { return };
            let rec = &l.opened.recording;
            ui.vertical(|ui| {
                ui.spacing_mut().item_spacing.y = 2.0;
                ui.label(
                    egui::RichText::new(friendly_name(&l.opened.title))
                        .font(egui::FontId::new(26.0, theme::bold())),
                );
                ui.label(
                    egui::RichText::new(describe(rec.tracks.len(), rec.duration))
                        .size(14.0)
                        .color(p.weak),
                );
            });
            if rec.is_multi() {
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Max), |ui| {
                    let labels = ["Follow the pointer", "Side by side", "Stacked"];
                    let chosen = Layout::ALL
                        .iter()
                        .position(|l| *l == self.layout)
                        .unwrap_or(0);
                    if let Some(i) = theme::segmented(ui, &labels, chosen) {
                        self.layout = Layout::ALL[i];
                    }
                });
            }
        });
    }

    /// Under the video: what to do while paused, and the copy buttons.
    fn copy_row(&mut self, ui: &mut egui::Ui) {
        let Some(loaded) = &self.loaded else { return };
        let playing = loaded.player.clock().is_playing();
        let sound_error = loaded.player.sound_error();
        let any_text = !playing && !self.visible_text().is_empty();
        let any_sel = !playing && self.selected_text().is_some();
        let mut copy_all = false;
        let mut copy_sel = false;
        ui.horizontal(|ui| {
            match sound_error {
                Some(e) => theme::notice(ui, format!("No sound: {e}")),
                None if playing => {
                    theme::hint(ui, "Click the picture to pause it and select text.")
                }
                None => theme::hint(
                    ui,
                    "Paused. Drag across text in the picture to select it. Space plays, F fills \
                     the screen.",
                ),
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui
                    .add_enabled(any_text, theme::outline("Copy all text"))
                    .on_hover_text("Copy all the text on screen")
                    .clicked()
                {
                    copy_all = true;
                }
                if ui
                    .add_enabled(any_sel, theme::outline("Copy selection"))
                    .on_hover_text("Ctrl+C")
                    .clicked()
                {
                    copy_sel = true;
                }
            });
        });
        if copy_all {
            ui.ctx().copy_text(self.visible_text());
        }
        if copy_sel && let Some(t) = self.selected_text() {
            ui.ctx().copy_text(t);
        }
    }

    /// The text panel: make the recording searchable, then search it and
    /// jump to what was found.
    fn text_panel(&mut self, ui: &mut egui::Ui) {
        let p = theme::palette(ui);
        let Some(loaded) = &self.loaded else { return };
        let indexed = loaded.opened.text.is_some();
        let recording = loaded.opened.recording.clone();

        ui.horizontal(|ui| {
            ui.label(
                egui::RichText::new("Text on screen").font(egui::FontId::new(16.0, theme::bold())),
            );
            if indexed {
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.label(
                        egui::RichText::new("Read every 2 seconds")
                            .size(12.0)
                            .color(p.weak),
                    );
                });
            }
        });
        ui.add_space(4.0);

        if let Some(ix) = &self.indexer {
            ui.add(
                egui::ProgressBar::new(ix.progress())
                    .desired_width(ui.available_width())
                    .corner_radius(6)
                    .fill(p.action)
                    .text(format!("Reading the text, {:.0}%", ix.progress() * 100.0)),
            );
            if ui.add(theme::outline("Stop reading")).clicked() {
                self.indexer = None;
            }
            return;
        }
        if !indexed {
            theme::hint(
                ui,
                "Read the text on every screen every two seconds, so you can search the \
                 recording for any word that was on screen. It is saved with the recording.",
            );
            ui.add_space(4.0);
            if ui
                .add(theme::outline(format!(
                    "{}  Make text searchable",
                    icon::TEXT
                )))
                .clicked()
            {
                let repaint = ui.ctx().clone();
                self.indexer = Some(Indexer::start(
                    recording,
                    INDEX_INTERVAL,
                    Arc::new(move || repaint.request_repaint()),
                ));
            }
            return;
        }

        // The search field: a pill on the surface with its icon inside.
        let field = egui::Frame::new()
            .fill(p.surface)
            .corner_radius(22)
            .inner_margin(egui::Margin::symmetric(14, 10))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new(icon::SEARCH).color(p.weak));
                    ui.add(
                        egui::TextEdit::singleline(&mut self.query)
                            .hint_text("Find a word you saw")
                            .frame(egui::Frame::NONE)
                            .desired_width(f32::INFINITY),
                    )
                })
                .inner
            });
        let response = field.inner;
        if response.changed() {
            self.run_search();
        }
        // Ctrl+F, as everywhere else.
        if ui.input(|i| i.modifiers.command && i.key_pressed(egui::Key::F)) {
            response.request_focus();
        }

        let note = match (self.query.trim().is_empty(), self.hits.len()) {
            (true, _) => "Type a word to find every place it was on screen.".to_string(),
            (false, 0) => format!(
                "Nothing matching \u{201c}{}\u{201d} was on screen. Try a shorter word, or pause \
                 on it to select it directly.",
                self.query.trim()
            ),
            (false, 1) => "1 place. Choose it to jump there.".to_string(),
            (false, n) => format!("{n} places. Choose one to jump there."),
        };
        theme::hint(ui, &note);

        let mut jump = None;
        let mut again = false;
        let query = self.query.trim().to_lowercase();
        egui::ScrollArea::vertical()
            .id_salt("hits")
            .auto_shrink([false; 2])
            .max_height(ui.available_height() - 40.0)
            .show(ui, |ui| {
                let Some(loaded) = &self.loaded else { return };
                let tracks = &loaded.opened.recording.tracks;
                for (i, hit) in self.hits.iter().enumerate() {
                    let track = tracks.iter().position(|t| t.id == hit.screen);
                    let screen = track
                        .map(|t| tracks[t].label.as_str())
                        .unwrap_or(hit.screen.as_str());
                    let colour = track.map(|t| track_colour(&p, t)).unwrap_or(p.weak);
                    if hit_row(ui, hit, screen, colour, &query, tracks.len() > 1, &p).clicked() {
                        jump = Some(i);
                    }
                }
            });
        ui.with_layout(egui::Layout::bottom_up(egui::Align::Min), |ui| {
            if ui
                .add(
                    egui::Button::new(
                        egui::RichText::new(format!("{}  Read the text again", icon::REFRESH))
                            .size(12.0)
                            .color(p.weak),
                    )
                    .frame(false),
                )
                .clicked()
            {
                again = true;
            }
        });
        if again && let Some(l) = &self.loaded {
            let repaint = ui.ctx().clone();
            self.indexer = Some(Indexer::start(
                l.opened.recording.clone(),
                INDEX_INTERVAL,
                Arc::new(move || repaint.request_repaint()),
            ));
        }
        if let Some(i) = jump {
            self.jump_to(i);
        }
    }

    fn poll_indexer(&mut self) {
        let Some(ix) = &self.indexer else { return };
        let Some(result) = ix.finished() else { return };
        self.indexer = None;
        let Some(loaded) = &mut self.loaded else {
            return;
        };
        match result {
            Ok(index) => {
                let words: usize = index.entries.iter().map(|e| e.page.words().count()).sum();
                self.status = match save_text(&loaded.opened.origin, &index) {
                    Ok(_) => None,
                    Err(e) => Some(format!("Text was read but could not be saved: {e}")),
                };
                if words == 0 {
                    self.status = Some("No text was found in this recording.".into());
                }
                loaded.opened.text = Some(index);
                self.run_search();
            }
            Err(e) => self.status = Some(format!("Could not read the text: {e}")),
        }
    }

    /// Nothing open: a drop target, and the recordings already in the save
    /// folder so the common case is one click.
    fn empty_state(&mut self, ui: &mut egui::Ui, ctx: &egui::Context, out_dir: &Path) {
        let p = theme::palette(ui);
        let dragging = ctx.input(|i| !i.raw.hovered_files.is_empty());
        let width = ui.available_width().min(620.0);

        ui.vertical_centered(|ui| {
            ui.set_max_width(width);
            ui.add_space(24.0);

            let (rect, _) = ui.allocate_exact_size(egui::vec2(width, 200.0), egui::Sense::hover());
            let colour = if dragging { p.signal } else { p.stroke };
            let fill = if dragging {
                theme::blend(p.base, p.signal, 0.10)
            } else {
                p.surface
            };
            ui.painter().rect_filled(rect, 20.0, fill);
            let r = rect.shrink(1.0);
            let corners = [
                r.left_top(),
                r.right_top(),
                r.right_bottom(),
                r.left_bottom(),
                r.left_top(),
            ];
            ui.painter().extend(egui::Shape::dashed_line(
                &corners,
                egui::Stroke::new(1.5, colour),
                7.0,
                5.0,
            ));

            let mut inner = ui.new_child(
                egui::UiBuilder::new()
                    .max_rect(rect.shrink(20.0))
                    .layout(egui::Layout::top_down(egui::Align::Center)),
            );
            inner.add_space(18.0);
            inner.label(
                egui::RichText::new(icon::MONITOR)
                    .size(34.0)
                    .color(if dragging { p.signal } else { p.weak }),
            );
            inner.add_space(6.0);
            if dragging {
                inner.label(
                    egui::RichText::new("Drop to open")
                        .font(egui::FontId::new(16.0, theme::strong())),
                );
            } else {
                if inner
                    .add(
                        egui::Button::new(
                            egui::RichText::new("Open a recording")
                                .font(egui::FontId::new(15.0, theme::bold()))
                                .color(ON_DARK),
                        )
                        .fill(p.action)
                        .corner_radius(22)
                        .min_size(egui::vec2(200.0, 44.0)),
                    )
                    .clicked()
                {
                    self.pick_file(ctx);
                }
                inner.add_space(2.0);
                inner.label(
                    egui::RichText::new("or drop a .screenilux file or an MP4 here").color(p.weak),
                );
            }
            if let Some((name, _)) = &self.opening {
                inner.horizontal(|ui| {
                    ui.spinner();
                    ui.label(format!("Opening {name}…"));
                });
            }
        });

        if let Some(s) = &self.status {
            ui.add_space(8.0);
            ui.vertical_centered(|ui| theme::notice(ui, s.clone()));
        }

        let recents = self.recents.get_or_insert_with(|| scan_recent(out_dir));
        if recents.is_empty() {
            return;
        }
        ui.add_space(20.0);
        let mut open = None;
        ui.vertical_centered(|ui| {
            ui.set_max_width(width);
            ui.with_layout(egui::Layout::top_down(egui::Align::Min), |ui| {
                ui.label(
                    egui::RichText::new("Recent recordings")
                        .font(egui::FontId::new(14.0, theme::strong())),
                );
                egui::ScrollArea::vertical()
                    .auto_shrink([false, true])
                    .max_height(ui.available_height())
                    .show(ui, |ui| {
                        for r in recents.iter() {
                            if recent_row(ui, r).clicked() {
                                open = Some(r.path.clone());
                            }
                        }
                    });
            });
        });
        if let Some(path) = open {
            self.open(path, ctx);
        }
    }

    /// Forget the list of recent recordings, so it is read again next time.
    pub fn recordings_changed(&mut self) {
        self.recents = None;
    }

    /// Which track is shown where, at the current position.
    fn placements(&self, view: egui::Rect) -> Vec<screenilux_core::playback::Placement> {
        let Some(l) = &self.loaded else {
            return Vec::new();
        };
        let rec = &l.opened.recording;
        let sizes: Vec<(u32, u32)> = rec.tracks.iter().map(|t| (t.width, t.height)).collect();
        let active = self
            .pinned
            .unwrap_or_else(|| rec.active_at(l.player.clock().position()));
        arrange(
            &sizes,
            self.layout,
            active,
            Area::new(view.min.x, view.min.y, view.width(), view.height()),
            GAP,
        )
    }

    fn video(&mut self, ui: &mut egui::Ui, ctx: &egui::Context, size: egui::Vec2) {
        let (rect, response) = ui.allocate_exact_size(size, egui::Sense::click_and_drag());
        let painter = ui.painter_at(rect);
        let pal = theme::palette(ui);

        let placements = self.placements(rect);
        let Some(loaded) = &mut self.loaded else {
            return;
        };

        // Upload any new frames.
        for (i, tex) in loaded.textures.iter_mut().enumerate() {
            let Some(feed) = loaded.player.feed(i) else {
                continue;
            };
            let serial = feed.serial();
            if tex.as_ref().is_some_and(|(s, _)| *s == serial) {
                continue;
            }
            let Some(frame) = feed.frame() else { continue };
            // Frames are opaque, so premultiplied and straight alpha agree
            // and the cheaper conversion is exact.
            let image = egui::ColorImage::from_rgba_premultiplied(
                [frame.width as usize, frame.height as usize],
                &frame.rgba,
            );
            match tex {
                Some((s, handle)) => {
                    handle.set(image, egui::TextureOptions::LINEAR);
                    *s = serial;
                }
                None => {
                    let handle = ctx.load_texture(
                        format!("screen-{i}"),
                        image,
                        egui::TextureOptions::LINEAR,
                    );
                    *tex = Some((serial, handle));
                }
            }
        }

        let multi = placements.len() > 1;
        for p in &placements {
            let r = to_rect(p.area);
            match &loaded.textures[p.track] {
                Some((_, tex)) => {
                    painter.image(
                        tex.id(),
                        r,
                        egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                        egui::Color32::WHITE,
                    );
                }
                None => {
                    painter.text(
                        r.center(),
                        egui::Align2::CENTER_CENTER,
                        "Loading…",
                        egui::FontId::proportional(14.0),
                        pal.weak,
                    );
                }
            }
            if let Some(err) = loaded.player.feed(p.track).and_then(|f| f.error()) {
                painter.text(
                    r.center(),
                    egui::Align2::CENTER_CENTER,
                    err,
                    egui::FontId::proportional(13.0),
                    pal.text,
                );
            }
            if multi {
                let label = &loaded.opened.recording.tracks[p.track].label;
                chip(&painter, r.left_top() + egui::vec2(8.0, 8.0), label, &pal);
            }
        }

        let paused = !loaded.player.clock().is_playing();
        if paused {
            self.request_text(&placements, ctx);
            self.draw_text(&painter, &placements, &response, ctx);
        } else {
            self.reset_text();
        }

        if response.double_clicked() {
            let to = !self.full;
            self.set_full(ctx, to);
        }
        // Clicking the picture while it plays pauses it, ready for selecting.
        if response.clicked()
            && !paused
            && let Some(l) = &self.loaded
        {
            l.player.clock().pause();
        }
    }

    /// Once the paused picture has settled, ask for the text on each visible
    /// screen that has not been read yet.
    fn request_text(
        &mut self,
        placements: &[screenilux_core::playback::Placement],
        ctx: &egui::Context,
    ) {
        let Some(loaded) = &self.loaded else { return };
        let serials: Vec<u64> = (0..loaded.opened.recording.tracks.len())
            .map(|i| loaded.player.feed(i).map(|f| f.serial()).unwrap_or(0))
            .collect();
        if serials != self.last_serials {
            self.last_serials = serials.clone();
            self.last_change = Instant::now();
            // The picture moved on: old text and selections no longer apply.
            self.pages.retain(|t, p| serials.get(*t) == Some(&p.serial));
            if self
                .selection
                .as_ref()
                .is_some_and(|s| !self.pages.contains_key(&s.track))
            {
                self.selection = None;
            }
            return;
        }
        if self.last_change.elapsed() < SETTLE {
            return;
        }
        let worker = self.worker.get_or_insert_with(|| {
            let ctx = ctx.clone();
            TextWorker::start(Arc::new(move || ctx.request_repaint()))
        });
        for p in placements {
            let serial = serials[p.track];
            if self.requested.get(&p.track) == Some(&serial) {
                continue;
            }
            if let Some(frame) = loaded.player.feed(p.track).and_then(|f| f.frame()) {
                worker.read(p.track, frame);
                self.requested.insert(p.track, serial);
            }
        }
        for r in worker.results() {
            let serial = serials.get(r.track).copied().unwrap_or(0);
            if self.requested.get(&r.track) == Some(&serial) {
                self.pages.insert(
                    r.track,
                    PageAt {
                        serial,
                        page: r.page,
                    },
                );
            }
        }
    }

    /// Word boxes over the paused picture, drag-to-select, and copy.
    fn draw_text(
        &mut self,
        painter: &egui::Painter,
        placements: &[screenilux_core::playback::Placement],
        response: &egui::Response,
        ctx: &egui::Context,
    ) {
        let Some(loaded) = &self.loaded else { return };
        let to_screen = |track: usize, area: Area| {
            let t = &loaded.opened.recording.tracks[track];
            let r = to_rect(area);
            let (sx, sy) = (r.width() / t.width as f32, r.height() / t.height as f32);
            move |x: f32, y: f32| r.min + egui::vec2(x * sx, y * sy)
        };
        let to_frame = |track: usize, area: Area, pos: egui::Pos2| {
            let t = &loaded.opened.recording.tracks[track];
            let r = to_rect(area);
            (
                (pos.x - r.min.x) * t.width as f32 / r.width(),
                (pos.y - r.min.y) * t.height as f32 / r.height(),
            )
        };

        // Drag handling: start on a screen that has text, extend while held.
        if let Some(pos) = response.interact_pointer_pos() {
            if response.drag_started() || response.clicked() {
                // egui reports a drag only once the pointer has moved, so
                // anchor where the button went down, not where it is now.
                let origin = ctx.input(|i| i.pointer.press_origin()).unwrap_or(pos);
                self.selection = placements
                    .iter()
                    .find(|p| to_rect(p.area).contains(origin))
                    .filter(|p| {
                        matches!(self.pages.get(&p.track), Some(PageAt { page: Ok(_), .. }))
                    })
                    .map(|p| Selection {
                        track: p.track,
                        anchor: to_frame(p.track, p.area, origin),
                        words: None,
                    });
            }
            if let Some(sel) = &mut self.selection
                && let Some(p) = placements.iter().find(|p| p.track == sel.track)
                && let Some(PageAt { page: Ok(page), .. }) = self.pages.get(&sel.track)
            {
                sel.words = page.select(sel.anchor, to_frame(p.track, p.area, pos));
            }
        }

        let pal = theme::palette_of(ctx);
        let select = pal.soft.gamma_multiply(0.45);
        let mut reading = false;
        for p in placements {
            let Some(at) = self.pages.get(&p.track) else {
                reading = true;
                continue;
            };
            let Ok(page) = &at.page else { continue };
            let map = to_screen(p.track, p.area);
            let selected = self
                .selection
                .as_ref()
                .filter(|s| s.track == p.track)
                .and_then(|s| s.words);
            for (r, w) in page.words() {
                let rect =
                    egui::Rect::from_min_max(map(w.x, w.y), map(w.x + w.w, w.y + w.h)).expand(1.5);
                if selected.is_some_and(|sel| Page::is_selected(r, sel)) {
                    painter.rect_filled(rect, 2.0, select);
                } else {
                    painter.rect_stroke(
                        rect,
                        2.0,
                        egui::Stroke::new(1.0, pal.soft.gamma_multiply(0.5)),
                        egui::StrokeKind::Outside,
                    );
                }
            }
        }
        if reading && !placements.is_empty() {
            let r = to_rect(placements[0].area);
            chip(
                painter,
                r.right_top() + egui::vec2(-120.0, 8.0),
                "Reading text…",
                &pal,
            );
        }

        // The text cursor only over a screen whose text can be selected, and
        // only when the picture itself is under the pointer, not a button or
        // panel drawn over or beside it.
        let over_text = response.hover_pos().is_some_and(|pos| {
            placements.iter().any(|p| {
                to_rect(p.area).contains(pos)
                    && matches!(
                        self.pages.get(&p.track),
                        Some(PageAt { page: Ok(page), .. }) if !page.is_empty()
                    )
            })
        });
        if over_text {
            ctx.set_cursor_icon(egui::CursorIcon::Text);
        }

        // Ctrl+C arrives as a Copy event, not a key press.
        let copy = ctx.input(|i| i.events.iter().any(|e| matches!(e, egui::Event::Copy)));
        if copy && let Some(text) = self.selected_text() {
            ctx.copy_text(text);
        }
    }

    fn selected_text(&self) -> Option<String> {
        let sel = self.selection.as_ref()?;
        let words = sel.words?;
        match &self.pages.get(&sel.track)?.page {
            Ok(page) => Some(page.selected_text(words)).filter(|s| !s.is_empty()),
            Err(_) => None,
        }
    }

    /// All text on the visible screens, in display order.
    fn visible_text(&self) -> String {
        let mut tracks: Vec<&usize> = self.pages.keys().collect();
        tracks.sort();
        tracks
            .into_iter()
            .filter_map(|t| self.pages[t].page.as_ref().ok().map(Page::text))
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
            .join("\n\n")
    }

    /// The transport: play, the position, the timeline with the screens
    /// beneath it, and full screen.
    fn controls_row(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        let p = theme::palette(ui);
        let Some(loaded) = &self.loaded else { return };
        let clock = loaded.player.clock();
        let duration = clock.duration();
        let pos = clock.position();
        let playing = clock.is_playing();
        let multi = loaded.opened.recording.is_multi();
        let spans = if multi {
            loaded.opened.recording.focus_spans()
        } else {
            Vec::new()
        };
        let labels: Vec<String> = loaded
            .opened
            .recording
            .tracks
            .iter()
            .map(|t| t.label.clone())
            .collect();
        let marks: Vec<f64> = self.hits.iter().map(|h| h.t).collect();

        let mut toggle = false;
        let mut fill = false;
        let mut layout = None;
        ui.spacing_mut().item_spacing.x = 14.0;
        let (glyph, tip) = if playing {
            (icon::PAUSE, "Pause (Space)")
        } else {
            (icon::PLAY, "Play (Space)")
        };
        if theme::round_button(ui, glyph, 44.0, p.action, ON_DARK, tip).clicked() {
            toggle = true;
        }
        ui.label(
            egui::RichText::new(format!("{} / {}", clock_text(pos), clock_text(duration)))
                .font(egui::FontId::new(14.0, theme::strong())),
        );

        // Right to left from the edge: full screen, the layouts when they
        // float in full screen, the screens' key; the timeline takes the rest.
        let right = ui.available_width();
        let mut tail = 54.0;
        if multi {
            tail += labels.len() as f32 * 76.0;
        }
        if self.full && multi {
            tail += 124.0;
        }
        let seek = scrubber(
            ui,
            (right - tail).max(80.0),
            pos,
            duration,
            &spans,
            &marks,
            &p,
        );

        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.spacing_mut().item_spacing.x = 10.0;
            let (glyph, tip) = if self.full {
                (icon::LEAVE_FULL_SCREEN, "Leave full screen (Esc)")
            } else {
                (icon::FULL_SCREEN, "Full screen (F)")
            };
            if theme::round_button(ui, glyph, 40.0, p.base, p.text, tip).clicked() {
                fill = true;
            }
            if self.full && multi {
                layout = layout_icons(ui, self.layout);
            }
            if multi {
                for (i, label) in labels.iter().enumerate().rev() {
                    ui.label(egui::RichText::new(label).size(12.0).color(p.weak));
                    let (dot, _) =
                        ui.allocate_exact_size(egui::vec2(8.0, 8.0), egui::Sense::hover());
                    ui.painter()
                        .circle_filled(dot.center(), 4.0, track_colour(&p, i));
                }
            }
        });

        if let Some(t) = seek
            && let Some(l) = &self.loaded
        {
            l.player.clock().seek(t);
            self.pinned = None;
        }
        if toggle {
            self.toggle_play();
        }
        if let Some(l) = layout {
            self.layout = l;
        }
        if fill {
            let to = !self.full;
            self.set_full(ctx, to);
        }
    }

    /// Enter or leave full screen.
    fn set_full(&mut self, ctx: &egui::Context, full: bool) {
        if self.full == full {
            return;
        }
        self.full = full;
        self.text_open = false;
        ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(full));
    }

    /// The picture across the whole display, the controls floating over it
    /// and stepping aside while the pointer rests during playback.
    fn full_screen(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        let p = theme::palette(ui);
        egui::CentralPanel::default_margins()
            .frame(egui::Frame::new().fill(p.stage).inner_margin(0))
            .show(ui, |ui| {
                let size = ui.available_size();
                self.video(ui, ctx, size);
            });

        let screen = ctx.content_rect();
        let playing = self.is_playing();
        let idle = ctx.input(|i| i.pointer.time_since_last_movement()) > IDLE;
        if playing && idle && !self.text_open {
            ctx.set_cursor_icon(egui::CursorIcon::None);
            return;
        }
        if playing {
            ctx.request_repaint_after(Duration::from_secs_f32(IDLE));
        }
        let floating = |p: &Palette| {
            egui::Frame::new()
                .fill(p.base.gamma_multiply(0.9))
                .stroke(egui::Stroke::new(1.0, p.stroke))
                .corner_radius(20)
        };

        // The recording's name, top left.
        if let Some(l) = &self.loaded {
            let rec = &l.opened.recording;
            let title = friendly_name(&l.opened.title);
            let detail = format!(
                "{}. Esc leaves full screen.",
                describe(rec.tracks.len(), rec.duration)
            );
            egui::Area::new(egui::Id::unique("full-title"))
                .fixed_pos(screen.min + egui::vec2(24.0, 24.0))
                .order(egui::Order::Foreground)
                .show(ctx, |ui| {
                    floating(&p)
                        .corner_radius(16)
                        .inner_margin(egui::Margin::symmetric(16, 10))
                        .show(ui, |ui| {
                            ui.spacing_mut().item_spacing.y = 2.0;
                            ui.label(
                                egui::RichText::new(title)
                                    .font(egui::FontId::new(15.0, theme::bold())),
                            );
                            ui.label(egui::RichText::new(detail).size(12.0).color(p.weak));
                        });
                });
        }

        // The text panel's switch, top right.
        egui::Area::new(egui::Id::unique("full-text-button"))
            .anchor(egui::Align2::RIGHT_TOP, egui::vec2(-24.0, 24.0))
            .order(egui::Order::Foreground)
            .show(ctx, |ui| {
                let label = format!("{}  Text on screen", icon::SEARCH);
                let button = theme::outline(label)
                    .fill(if self.text_open {
                        p.surface
                    } else {
                        p.base.gamma_multiply(0.9)
                    })
                    .selected(self.text_open);
                if ui.add(button).clicked() {
                    self.text_open = !self.text_open;
                }
            });

        if self.text_open {
            let top = screen.top() + 92.0;
            let height = (screen.height() - 92.0 - 120.0).max(200.0);
            egui::Area::new(egui::Id::unique("full-text"))
                .fixed_pos(egui::pos2(screen.right() - 24.0 - PANEL, top))
                .order(egui::Order::Foreground)
                .show(ctx, |ui| {
                    floating(&p).inner_margin(20).show(ui, |ui| {
                        ui.set_width(PANEL - 40.0);
                        ui.set_height(height - 40.0);
                        self.text_panel(ui);
                    });
                });
        }

        // The controls, along the bottom.
        egui::Area::new(egui::Id::unique("full-controls"))
            .fixed_pos(egui::pos2(
                screen.left() + 24.0,
                screen.bottom() - 24.0 - 64.0,
            ))
            .order(egui::Order::Foreground)
            .show(ctx, |ui| {
                floating(&p)
                    .inner_margin(egui::Margin::symmetric(16, 10))
                    .show(ui, |ui| {
                        ui.set_width(screen.width() - 48.0 - 32.0);
                        ui.set_height(44.0);
                        ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                            self.controls_row(ui, ctx);
                        });
                    });
            });
    }

    pub fn is_full(&self) -> bool {
        self.full
    }

    fn toggle_play(&mut self) {
        if let Some(l) = &self.loaded {
            if !l.player.clock().is_playing() {
                self.pinned = None;
            }
            l.player.clock().toggle();
        }
    }

    /// Pause on a search hit, showing the screen it was found on.
    fn jump_to(&mut self, hit: usize) {
        let Some(loaded) = &self.loaded else { return };
        let Some(h) = self.hits.get(hit) else { return };
        let clock = loaded.player.clock();
        clock.pause();
        clock.seek(h.t);
        self.pinned = loaded
            .opened
            .recording
            .tracks
            .iter()
            .position(|t| t.id == h.screen);
    }

    fn handle_keys(&mut self, ctx: &egui::Context) {
        // Leave keys alone while typing in the search box.
        if ctx.egui_wants_keyboard_input() {
            return;
        }
        let (space, left, right, esc, f) = ctx.input(|i| {
            (
                i.key_pressed(egui::Key::Space),
                i.key_pressed(egui::Key::ArrowLeft),
                i.key_pressed(egui::Key::ArrowRight),
                i.key_pressed(egui::Key::Escape),
                i.key_pressed(egui::Key::F) && !i.modifiers.command,
            )
        });
        if space {
            self.toggle_play();
        }
        if esc {
            self.set_full(ctx, false);
        }
        if f {
            let to = !self.full;
            self.set_full(ctx, to);
        }
        if let Some(l) = &self.loaded {
            let clock = l.player.clock();
            if left {
                clock.seek(clock.position() - 5.0);
            }
            if right {
                clock.seek(clock.position() + 5.0);
            }
        }
    }

    pub fn is_playing(&self) -> bool {
        self.loaded
            .as_ref()
            .is_some_and(|l| l.player.clock().is_playing())
    }
}

/// "2 screens, 17 seconds".
fn describe(screens: usize, duration: f64) -> String {
    let screens = match screens {
        1 => "1 screen".to_string(),
        n => format!("{n} screens"),
    };
    let secs = duration.round() as u64;
    let length = match secs {
        1 => "1 second".to_string(),
        0..60 => format!("{secs} seconds"),
        _ => clock_text(duration),
    };
    format!("{screens}, {length}")
}

/// The timeline: the screens shown while following the pointer as a band
/// above, a track filled up to the playhead, and a tick for each search
/// result. Returns where to seek when clicked or dragged.
fn scrubber(
    ui: &mut egui::Ui,
    width: f32,
    pos: f64,
    duration: f64,
    spans: &[(f64, f64, usize)],
    marks: &[f64],
    p: &Palette,
) -> Option<f64> {
    let (rect, response) =
        ui.allocate_exact_size(egui::vec2(width, 36.0), egui::Sense::click_and_drag());
    let painter = ui.painter_at(rect.expand(10.0));
    let d = duration.max(0.001);
    let x_of = |t: f64| rect.left() + (t / d).clamp(0.0, 1.0) as f32 * rect.width();
    let band = rect.center().y - 7.0;
    let mid = rect.center().y + 5.0;
    let hot = response.hovered() || response.dragged();

    for (a, b, t) in spans {
        painter.rect_filled(
            egui::Rect::from_min_max(
                egui::pos2(x_of(*a) + 1.0, band - 2.0),
                egui::pos2(x_of(*b) - 1.0, band + 2.0),
            ),
            2.0,
            track_colour(p, *t),
        );
    }
    let track = egui::Rect::from_min_max(
        egui::pos2(rect.left(), mid - 3.0),
        egui::pos2(rect.right(), mid + 3.0),
    );
    painter.rect_filled(track, 3.0, p.stroke);
    painter.rect_filled(
        egui::Rect::from_min_max(track.min, egui::pos2(x_of(pos), track.max.y)),
        3.0,
        p.action,
    );
    for t in marks {
        let x = x_of(*t);
        painter.line_segment(
            [egui::pos2(x, mid - 9.0), egui::pos2(x, mid - 5.0)],
            egui::Stroke::new(2.0, p.text),
        );
    }
    let knob = if hot { 9.0 } else { 8.0 };
    painter.circle(
        egui::pos2(x_of(pos), mid),
        knob,
        p.base,
        egui::Stroke::new(3.0, p.action),
    );

    if let Some(hover) = response.hover_pos() {
        let t = ((hover.x - rect.left()) / rect.width()).clamp(0.0, 1.0) as f64 * d;
        response.clone().on_hover_text_at_pointer(clock_text(t));
    }
    if (response.clicked() || response.dragged())
        && let Some(at) = response.interact_pointer_pos()
    {
        return Some(((at.x - rect.left()) / rect.width()).clamp(0.0, 1.0) as f64 * d);
    }
    None
}

/// The three layouts as small drawings, for full screen where their names
/// would crowd the controls. Right to left, so the last drawn sits leftmost.
fn layout_icons(ui: &mut egui::Ui, current: Layout) -> Option<Layout> {
    let p = theme::palette(ui);
    let mut picked = None;
    for l in Layout::ALL.iter().rev() {
        let selected = current == *l;
        let (rect, response) = ui.allocate_exact_size(egui::vec2(36.0, 32.0), egui::Sense::click());
        if selected {
            ui.painter().rect(
                rect,
                9.0,
                p.surface,
                egui::Stroke::new(1.0, p.stroke),
                egui::StrokeKind::Inside,
            );
        } else if response.hovered() {
            ui.painter().rect_filled(rect, 9.0, p.surface);
        }
        let ink = if selected { p.text } else { p.weak };
        let g = egui::Rect::from_center_size(rect.center(), egui::vec2(18.0, 12.0));
        let stroke = egui::Stroke::new(1.4, ink);
        let boxes = match l {
            Layout::Auto => vec![g],
            Layout::SideBySide => vec![
                egui::Rect::from_min_max(g.min, egui::pos2(g.center().x - 1.5, g.max.y)),
                egui::Rect::from_min_max(egui::pos2(g.center().x + 1.5, g.min.y), g.max),
            ],
            Layout::Stacked => vec![
                egui::Rect::from_min_max(g.min, egui::pos2(g.max.x, g.center().y - 1.0)),
                egui::Rect::from_min_max(egui::pos2(g.min.x, g.center().y + 1.0), g.max),
            ],
        };
        for b in boxes {
            ui.painter()
                .rect_stroke(b, 1.5, stroke, egui::StrokeKind::Inside);
        }
        if *l == Layout::Auto {
            let tip = g.center() + egui::vec2(-2.0, -2.0);
            ui.painter().add(egui::Shape::convex_polygon(
                vec![tip, tip + egui::vec2(0.0, 7.0), tip + egui::vec2(5.0, 5.0)],
                ink,
                egui::Stroke::NONE,
            ));
        }
        let tip = match l {
            Layout::Auto => "Follow the pointer",
            Layout::SideBySide => "Side by side",
            Layout::Stacked => "Stacked",
        };
        if response.on_hover_text(tip).clicked() {
            picked = Some(*l);
        }
    }
    picked
}

/// One search result: a time chip, the line with the match picked out, and
/// the screen it was on.
fn hit_row(
    ui: &mut egui::Ui,
    hit: &Hit,
    screen: &str,
    colour: egui::Color32,
    query: &str,
    multi: bool,
    p: &Palette,
) -> egui::Response {
    let width = ui.available_width();
    let mut job = egui::text::LayoutJob::default();
    let normal = egui::TextFormat {
        font_id: egui::FontId::proportional(14.0),
        color: p.text,
        ..Default::default()
    };
    let found = egui::TextFormat {
        background: p.stroke,
        ..normal.clone()
    };
    let lower = hit.snippet.to_lowercase();
    match lower
        .find(query)
        .filter(|_| lower.len() == hit.snippet.len())
    {
        Some(at) if !query.is_empty() => {
            job.append(&hit.snippet[..at], 0.0, normal.clone());
            job.append(&hit.snippet[at..at + query.len()], 0.0, found);
            job.append(&hit.snippet[at + query.len()..], 0.0, normal);
        }
        _ => job.append(&hit.snippet, 0.0, normal),
    }
    let text_width = width - 20.0 - 72.0;
    job.wrap.max_width = text_width;

    let inner = egui::Frame::new()
        .inner_margin(egui::Margin::symmetric(10, 10))
        .corner_radius(8)
        .show(ui, |ui| {
            ui.set_width(width - 20.0);
            ui.horizontal_top(|ui| {
                let chip = ui.painter().layout_no_wrap(
                    clock_text(hit.t),
                    egui::FontId::new(12.0, theme::strong()),
                    p.text,
                );
                let (rect, _) =
                    ui.allocate_exact_size(egui::vec2(62.0, 22.0), egui::Sense::hover());
                ui.painter().rect_filled(rect, 11.0, p.surface);
                ui.painter()
                    .circle_filled(rect.left_center() + egui::vec2(10.0, 0.0), 3.0, colour);
                ui.painter().galley(
                    egui::pos2(rect.left() + 18.0, rect.center().y - chip.size().y / 2.0),
                    chip,
                    p.text,
                );
                ui.vertical(|ui| {
                    ui.spacing_mut().item_spacing.y = 2.0;
                    ui.set_width(text_width);
                    ui.label(job);
                    if multi {
                        ui.label(egui::RichText::new(screen).size(12.0).color(p.weak));
                    }
                });
            });
        });
    let response = ui.interact(
        inner.response.rect,
        ui.scope_id().with(("hit", hit.entry, hit.screen.as_str())),
        egui::Sense::click(),
    );
    if response.hovered() {
        ui.painter()
            .rect_filled(inner.response.rect, 8.0, p.text.gamma_multiply(0.05));
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    response
}

/// A small dark label over the picture, readable on any content.
fn chip(painter: &egui::Painter, at: egui::Pos2, text: &str, p: &Palette) {
    let galley = painter.layout_no_wrap(
        text.to_string(),
        egui::FontId::new(12.0, theme::strong()),
        ON_DARK,
    );
    let rect = egui::Rect::from_min_size(at, galley.size() + egui::vec2(16.0, 8.0));
    painter.rect_filled(rect, 10.0, p.bar.gamma_multiply(0.82));
    painter.galley(rect.min + egui::vec2(8.0, 4.0), galley, ON_DARK);
}

/// One recording in the empty state's list.
fn recent_row(ui: &mut egui::Ui, r: &Recent) -> egui::Response {
    let p = theme::palette(ui);
    let width = ui.available_width();
    let inner = egui::Frame::new()
        .inner_margin(egui::Margin::symmetric(10, 7))
        .show(ui, |ui| {
            ui.set_width(width - 20.0);
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new(icon::PLAY).color(p.signal));
                ui.label(&r.name);
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.label(egui::RichText::new(&r.detail).small().color(p.weak));
                });
            });
        });
    let response = ui.interact(
        inner.response.rect,
        ui.scope_id().with(("recent", &r.path)),
        egui::Sense::click(),
    );
    if response.hovered() {
        ui.painter()
            .rect_filled(inner.response.rect, 6.0, p.text.gamma_multiply(0.06));
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    response.on_hover_text(r.path.display().to_string())
}

/// Recordings in `dir`, newest first.
fn scan_recent(dir: &Path) -> Vec<Recent> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let now = std::time::SystemTime::now();
    let mut found: Vec<(std::time::SystemTime, Recent)> = entries
        .flatten()
        .filter_map(|e| {
            let path = e.path();
            let ext = path.extension()?.to_string_lossy().to_lowercase();
            if ext != "screenilux" && ext != "mp4" {
                return None;
            }
            let meta = e.metadata().ok()?;
            let modified = meta.modified().ok()?;
            let screens = if ext == "screenilux" {
                screenilux_core::package::read_manifest(&path)
                    .map(|m| m.screens.len())
                    .unwrap_or(0)
            } else {
                1
            };
            let screens = match screens {
                0 => "unreadable".to_string(),
                1 => "1 screen".to_string(),
                n => format!("{n} screens"),
            };
            let age = now.duration_since(modified).unwrap_or_default();
            let detail = format!("{screens}, {}, {}", size_text(meta.len()), age_text(age));
            let name = friendly_name(&path.file_stem()?.to_string_lossy());
            Some((modified, Recent { path, name, detail }))
        })
        .collect();
    found.sort_by_key(|f| std::cmp::Reverse(f.0));
    found.into_iter().take(12).map(|(_, r)| r).collect()
}

/// "31 Oct 2026, 17:55" for a name Screenilux gave a recording, so a list of
/// them reads as dates. Any other name is shown as it is, less its extension.
pub fn friendly_name(name: &str) -> String {
    const MONTHS: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    let stem = name
        .strip_suffix(".screenilux")
        .or_else(|| name.strip_suffix(".mp4"))
        .unwrap_or(name);
    let parse = || -> Option<String> {
        let rest = stem.strip_prefix("recording_")?;
        // A second recording in the same second is saved as "... (2)".
        let (stamp, copy) = match rest.split_once(" (") {
            Some((s, n)) => (s, Some(n.trim_end_matches(')'))),
            None => (rest, None),
        };
        let (date, time) = stamp.split_once('_')?;
        let mut d = date.split('-');
        let (y, m, day) = (d.next()?, d.next()?, d.next()?);
        let mut t = time.split('-');
        let (h, min) = (t.next()?, t.next()?);
        let month = MONTHS.get(m.parse::<usize>().ok()?.checked_sub(1)?)?;
        let day: u32 = day.parse().ok()?;
        let base = format!("{day} {month} {y}, {h}:{min}");
        Some(match copy {
            Some(n) => format!("{base} ({n})"),
            None => base,
        })
    };
    parse().unwrap_or_else(|| stem.to_string())
}

fn size_text(bytes: u64) -> String {
    match bytes {
        b if b >= 1 << 30 => format!("{:.1} GB", b as f64 / (1u64 << 30) as f64),
        b if b >= 1 << 20 => format!("{:.0} MB", b as f64 / (1u64 << 20) as f64),
        b => format!("{} KB", (b / 1024).max(1)),
    }
}

/// "just now", "5 minutes ago", "3 days ago".
fn age_text(age: Duration) -> String {
    let s = age.as_secs();
    let plural = |n: u64, unit: &str| {
        if n == 1 {
            format!("1 {unit} ago")
        } else {
            format!("{n} {unit}s ago")
        }
    };
    match s {
        0..60 => "just now".into(),
        60..3600 => plural(s / 60, "minute"),
        3600..86_400 => plural(s / 3600, "hour"),
        _ => plural(s / 86_400, "day"),
    }
}

fn to_rect(a: Area) -> egui::Rect {
    egui::Rect::from_min_size(egui::pos2(a.x, a.y), egui::vec2(a.w, a.h))
}

fn clock_text(t: f64) -> String {
    let s = t.max(0.0) as u64;
    if s >= 3600 {
        format!("{}:{:02}:{:02}", s / 3600, (s % 3600) / 60, s % 60)
    } else {
        format!("{:02}:{:02}", s / 60, s % 60)
    }
}

/// A path dropped onto the window, if any.
pub fn dropped_path(ctx: &egui::Context) -> Option<PathBuf> {
    ctx.input(|i| i.raw.dropped_files.first().map(|f| f.path().to_path_buf()))
}

/// Whether a path is something the player opens.
pub fn playable(path: &Path) -> bool {
    path.is_dir()
        || path.extension().is_some_and(|e| {
            ["screenilux", "mp4", "json"]
                .iter()
                .any(|x| e.eq_ignore_ascii_case(x))
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recording_names_read_as_dates() {
        assert_eq!(
            friendly_name("recording_2026-10-31_17-55-17.screenilux"),
            "31 Oct 2026, 17:55"
        );
        assert_eq!(
            friendly_name("recording_2026-01-05_09-03-00 (2)"),
            "5 Jan 2026, 09:03 (2)"
        );
        assert_eq!(friendly_name("holiday.mp4"), "holiday");
        assert_eq!(friendly_name("recording_garbage"), "recording_garbage");
    }

    #[test]
    fn ages_and_sizes_read_naturally() {
        assert_eq!(age_text(Duration::from_secs(5)), "just now");
        assert_eq!(age_text(Duration::from_secs(60)), "1 minute ago");
        assert_eq!(age_text(Duration::from_secs(7300)), "2 hours ago");
        assert_eq!(size_text(17 * 1024 * 1024), "17 MB");
        assert_eq!(size_text(200 * 1024), "200 KB");
    }

    #[test]
    fn times_read_like_a_player() {
        assert_eq!(clock_text(0.0), "00:00");
        assert_eq!(clock_text(75.9), "01:15");
        assert_eq!(clock_text(3725.0), "1:02:05");
        assert_eq!(clock_text(-3.0), "00:00");
    }

    #[test]
    fn packages_videos_and_manifests_are_playable() {
        assert!(playable(Path::new("x.screenilux")));
        assert!(playable(Path::new("x.MP4")));
        assert!(playable(Path::new("recording.json")));
        assert!(!playable(Path::new("notes.txt")));
    }
}
