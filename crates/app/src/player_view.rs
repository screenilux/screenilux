//! The Play tab: open a recording, watch every screen in step, and copy or
//! search the text in it.

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

/// Wait this long after the picture stops changing before reading its text,
/// so scrubbing does not queue OCR for every frame passed over.
const SETTLE: Duration = Duration::from_millis(200);
const GAP: f32 = 6.0;
const WARN: egui::Color32 = egui::Color32::from_rgb(200, 140, 60);
const SELECT: egui::Color32 = egui::Color32::from_rgba_premultiplied(40, 110, 220, 90);

/// Colours for telling screens apart on the focus strip.
const TRACK_COLOURS: [egui::Color32; 4] = [
    egui::Color32::from_rgb(70, 130, 220),
    egui::Color32::from_rgb(220, 140, 60),
    egui::Color32::from_rgb(90, 180, 110),
    egui::Color32::from_rgb(190, 90, 170),
];

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
    /// The window was enlarged once for playback; do not fight the user after.
    resized: bool,
}

impl PlayerView {
    pub fn new() -> Self {
        Self {
            loaded: None,
            opening: None,
            status: None,
            layout: Layout::Auto,
            pinned: None,
            worker: None,
            requested: HashMap::new(),
            pages: HashMap::new(),
            last_change: Instant::now(),
            last_serials: Vec::new(),
            selection: None,
            indexer: None,
            query: String::new(),
            hits: Vec::new(),
            resized: false,
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

    fn pick_file(&mut self, ctx: &egui::Context) {
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
                    opened.title
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

    /// Size the window for watching, once.
    fn ensure_room(&mut self, ctx: &egui::Context) {
        if self.resized {
            return;
        }
        self.resized = true;
        if ctx.content_rect().width() >= 1000.0 {
            return;
        }
        // Large enough to watch, but never taller than the screen: a fixed
        // size put the controls under the taskbar on a 1080p display.
        let monitor = ctx
            .input(|i| i.viewport().monitor_size)
            .unwrap_or(egui::vec2(1920.0, 1080.0));
        // Room for the title bar and the taskbar, which the monitor size
        // includes.
        let usable = egui::vec2(monitor.x, monitor.y - 100.0);
        let size = egui::vec2(1280.0f32.min(usable.x * 0.9), 860.0f32.min(usable.y * 0.92));
        ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(size));
        ctx.send_viewport_cmd(egui::ViewportCommand::OuterPosition(egui::pos2(
            ((monitor.x - size.x) / 2.0).max(0.0),
            ((usable.y - size.y) / 2.0).max(0.0),
        )));
    }

    pub fn ui(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        self.ensure_room(ctx);
        self.finish_opening(ctx);
        self.poll_indexer();

        self.toolbar(ui, ctx);
        ui.add_space(4.0);

        let Some(loaded) = &self.loaded else {
            self.empty_state(ui, ctx);
            return;
        };
        if loaded.player.clock().settle() {
            ctx.request_repaint();
        }

        // Search results take a column on the right when there are any.
        let show_results = !self.query.trim().is_empty();
        let avail = ui.available_size();
        let controls_h = 74.0;
        let results_w = if show_results { 280.0 } else { 0.0 };
        let video_size = egui::vec2(
            (avail.x - results_w - if show_results { 8.0 } else { 0.0 }).max(100.0),
            (avail.y - controls_h).max(100.0),
        );

        ui.horizontal_top(|ui| {
            ui.vertical(|ui| {
                // Pinned, or the right-aligned buttons under the video widen
                // this column to the whole window and push the results out.
                ui.set_width(video_size.x);
                self.video(ui, ctx, video_size);
                self.controls(ui, video_size.x);
            });
            if show_results {
                ui.add_space(8.0);
                ui.vertical(|ui| {
                    ui.set_width(results_w);
                    self.results(ui);
                });
            }
        });

        self.handle_keys(ctx);
    }

    fn toolbar(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        ui.horizontal(|ui| {
            if ui.button("Open…").clicked() {
                self.pick_file(ctx);
            }
            if let Some((name, _)) = &self.opening {
                ui.spinner();
                ui.label(format!("Opening {name}…"));
            } else if let Some(l) = &self.loaded {
                ui.label(egui::RichText::new(&l.opened.title).strong());
            }

            let multi = self
                .loaded
                .as_ref()
                .is_some_and(|l| l.opened.recording.is_multi());
            if multi {
                ui.add_space(12.0);
                egui::ComboBox::from_id_salt("layout")
                    .selected_text(self.layout.label())
                    .width(120.0)
                    .show_ui(ui, |ui| {
                        for l in Layout::ALL {
                            ui.selectable_value(&mut self.layout, l, l.label());
                        }
                    });
            }

            if self.loaded.is_some() {
                ui.add_space(12.0);
                self.search_box(ui);
            }
        });
        if let Some(s) = &self.status {
            ui.colored_label(WARN, s);
        }
    }

    fn search_box(&mut self, ui: &mut egui::Ui) {
        let Some(loaded) = &self.loaded else { return };
        let indexed = loaded.opened.text.is_some();
        let recording = loaded.opened.recording.clone();
        if indexed {
            let edit = egui::TextEdit::singleline(&mut self.query)
                .hint_text("Search text in the video")
                .desired_width(220.0);
            if ui.add(edit).changed() {
                self.run_search();
            }
        }
        if let Some(ix) = &self.indexer {
            ui.add(
                egui::ProgressBar::new(ix.progress())
                    .desired_width(160.0)
                    .text(format!("Reading text {:.0}%", ix.progress() * 100.0)),
            );
            if ui.button("Cancel").clicked() {
                self.indexer = None;
            }
        } else {
            let label = if indexed {
                "Re-read text"
            } else {
                "Make text searchable"
            };
            let tip = "Reads the text on every screen every couple of seconds, so you \
                       can search it. Saved with the recording.";
            if ui.button(label).on_hover_text(tip).clicked() {
                let repaint = ui.ctx().clone();
                self.indexer = Some(Indexer::start(
                    recording,
                    INDEX_INTERVAL,
                    Arc::new(move || repaint.request_repaint()),
                ));
            }
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

    fn empty_state(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        ui.add_space(80.0);
        ui.vertical_centered(|ui| {
            ui.label(egui::RichText::new("Nothing open").size(20.0));
            ui.add_space(8.0);
            ui.label(
                "Open a .screenilux recording, a session folder's recording.json, or any MP4.\n\
                 You can also drop a file onto this window.",
            );
            ui.add_space(12.0);
            if ui.button("Open a recording…").clicked() {
                self.pick_file(ctx);
            }
        });
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
        painter.rect_filled(rect, 4.0, egui::Color32::from_gray(12));

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
                        egui::Color32::GRAY,
                    );
                }
            }
            if let Some(err) = loaded.player.feed(p.track).and_then(|f| f.error()) {
                painter.text(
                    r.center(),
                    egui::Align2::CENTER_CENTER,
                    err,
                    egui::FontId::proportional(13.0),
                    WARN,
                );
            }
            if multi {
                let label = &loaded.opened.recording.tracks[p.track].label;
                painter.text(
                    r.left_top() + egui::vec2(8.0, 6.0),
                    egui::Align2::LEFT_TOP,
                    label,
                    egui::FontId::proportional(12.0),
                    egui::Color32::from_white_alpha(200),
                );
            }
        }

        let paused = !loaded.player.clock().is_playing();
        if paused {
            self.request_text(&placements, ctx);
            self.draw_text(&painter, &placements, &response, ctx);
        } else {
            self.reset_text();
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
                    painter.rect_filled(rect, 2.0, SELECT);
                } else {
                    painter.rect_stroke(
                        rect,
                        2.0,
                        egui::Stroke::new(
                            1.0,
                            egui::Color32::from_rgba_premultiplied(120, 170, 255, 70),
                        ),
                        egui::StrokeKind::Outside,
                    );
                }
            }
        }
        if reading && !placements.is_empty() {
            let r = to_rect(placements[0].area);
            painter.text(
                r.right_top() + egui::vec2(-8.0, 6.0),
                egui::Align2::RIGHT_TOP,
                "Reading text…",
                egui::FontId::proportional(12.0),
                egui::Color32::from_white_alpha(180),
            );
        }

        let has_text = self
            .pages
            .values()
            .any(|p| p.page.as_ref().is_ok_and(|pg| !pg.is_empty()));
        if has_text {
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

    fn controls(&mut self, ui: &mut egui::Ui, width: f32) {
        let Some(loaded) = &self.loaded else { return };
        let clock = loaded.player.clock();
        let duration = clock.duration();
        let mut pos = clock.position();
        let playing = clock.is_playing();

        ui.add_space(4.0);
        // The focus strip: which screen auto-switch shows when.
        if loaded.opened.recording.is_multi() {
            let (strip, _) = ui.allocate_exact_size(egui::vec2(width, 5.0), egui::Sense::hover());
            for (a, b, track) in loaded.opened.recording.focus_spans() {
                let x0 = strip.min.x + (a / duration.max(0.001)) as f32 * strip.width();
                let x1 = strip.min.x + (b / duration.max(0.001)) as f32 * strip.width();
                ui.painter().rect_filled(
                    egui::Rect::from_min_max(
                        egui::pos2(x0, strip.min.y),
                        egui::pos2(x1, strip.max.y),
                    ),
                    1.0,
                    TRACK_COLOURS[track % TRACK_COLOURS.len()],
                );
            }
        }

        ui.spacing_mut().slider_width = width - 16.0;
        let slider = egui::Slider::new(&mut pos, 0.0..=duration.max(0.001))
            .show_value(false)
            .trailing_fill(true);
        if ui.add(slider).changed() {
            clock.seek(pos);
            self.pinned = None;
        }

        let mut toggle = false;
        let mut copy_all = false;
        let mut copy_sel = false;
        ui.horizontal(|ui| {
            let label = if playing { "Pause" } else { "Play" };
            if ui
                .add(egui::Button::new(label).min_size(egui::vec2(70.0, 26.0)))
                .clicked()
            {
                toggle = true;
            }
            ui.label(
                egui::RichText::new(format!("{} / {}", clock_text(pos), clock_text(duration)))
                    .monospace(),
            );
            if let Some(e) = loaded.player.sound_error() {
                ui.colored_label(WARN, format!("No sound: {e}"));
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if !playing {
                    let any_text = !self.visible_text().is_empty();
                    if ui
                        .add_enabled(any_text, egui::Button::new("Copy all text"))
                        .clicked()
                    {
                        copy_all = true;
                    }
                    let any_sel = self.selected_text().is_some();
                    if ui
                        .add_enabled(any_sel, egui::Button::new("Copy selection"))
                        .clicked()
                    {
                        copy_sel = true;
                    }
                    ui.label(
                        egui::RichText::new("Drag across text to select it")
                            .weak()
                            .small(),
                    );
                }
            });
        });

        if toggle {
            self.toggle_play();
        }
        if copy_all {
            ui.ctx().copy_text(self.visible_text());
        }
        if copy_sel && let Some(t) = self.selected_text() {
            ui.ctx().copy_text(t);
        }
    }

    fn toggle_play(&mut self) {
        if let Some(l) = &self.loaded {
            if !l.player.clock().is_playing() {
                self.pinned = None;
            }
            l.player.clock().toggle();
        }
    }

    fn results(&mut self, ui: &mut egui::Ui) {
        ui.label(egui::RichText::new(format!("{} matches", self.hits.len())).strong());
        ui.separator();
        let mut jump = None;
        egui::ScrollArea::vertical()
            .id_salt("hits")
            .auto_shrink([false; 2])
            .show(ui, |ui| {
                let Some(loaded) = &self.loaded else { return };
                for (i, hit) in self.hits.iter().enumerate() {
                    let screen = loaded
                        .opened
                        .recording
                        .tracks
                        .iter()
                        .find(|t| t.id == hit.screen)
                        .map(|t| t.label.as_str())
                        .unwrap_or(hit.screen.as_str());
                    let text = format!("{}  {}\n{}", clock_text(hit.t), screen, hit.snippet);
                    if ui
                        .add(
                            egui::Button::new(text)
                                .wrap()
                                .min_size(egui::vec2(ui.available_width(), 0.0)),
                        )
                        .clicked()
                    {
                        jump = Some(i);
                    }
                }
            });
        if let Some(i) = jump {
            self.jump_to(i);
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
        let (space, left, right) = ctx.input(|i| {
            (
                i.key_pressed(egui::Key::Space),
                i.key_pressed(egui::Key::ArrowLeft),
                i.key_pressed(egui::Key::ArrowRight),
            )
        });
        if space {
            self.toggle_play();
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
