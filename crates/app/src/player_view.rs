//! The Play tab: open a recording, watch every screen in step, and copy or
//! search the text in it. Full screen gives the picture the whole display,
//! with the controls floating over it.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc::{Receiver, channel};
use std::time::Duration;

use eframe::egui;
use screenilux_core::playback::{Area, Layout, arrange};
use screenilux_export::{Format, Shape};
use screenilux_player::Player;
use screenilux_player::open::{Opened, Origin};

use crate::sharing::{self, Asked, Link};
use crate::theme::{self, ON_DARK, Palette, icon};

const GAP: f32 = 16.0;
/// Space between the window's edge and the content.
const MARGIN: i8 = 24;
/// The text panel's width.
const PANEL: f32 = 360.0;

/// The side panel's width, including the margin beside it.
pub const SIDE_WIDTH: f32 = PANEL + MARGIN as f32;
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

/// The export menu: wide enough for a format's name and what it is for,
/// each on one line.
const EXPORT_MENU_W: f32 = 320.0;

struct Loaded {
    opened: Opened,
    player: Player,
    /// Per track: the feed serial uploaded, and the texture.
    textures: Vec<Option<(u64, egui::TextureHandle)>>,
    /// The same for the camera's own video, when there is one.
    camera: Option<(u64, egui::TextureHandle)>,
}

/// A click on the paused picture while the editor is showing: which screen
/// it landed on, and where on that screen as a fraction of it. Knowing the
/// screen is what lets an edit on a multi-screen recording say which picture
/// it belongs to.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Placed {
    pub track: usize,
    pub x: f32,
    pub y: f32,
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

    /// Recordings in the save folder, read when first shown.
    recents: Option<Vec<Recent>>,
    /// The open recording's link, and whether it can be shared, as the app
    /// last said; and sharing it, if asked for.
    link: (Link, bool),
    asked: Option<Asked>,
    /// The shape the export menu is set to: kept, so the next export is
    /// the same shape as the last.
    export_shape: Shape,
    /// The editor is showing, so a plain click on the paused picture places
    /// an edit instead of clearing the text selection.
    editing: bool,
    placed: Option<Placed>,
    /// Where the picture was drawn last frame, for anything that needs to
    /// point at it.
    stage: Option<egui::Rect>,
    /// How much of the card's bottom to keep for whoever is editing, and
    /// where that band ended up.
    footer: f32,
    footer_at: Option<egui::Rect>,
    /// The effects live at the current moment, as the editor last worked
    /// them out. The player draws them; the editor owns them.
    live: Vec<screenilux_core::edit::Effect>,
    /// Where the pointer went while the recording was made, when it was
    /// written down. What lets a zoom follow it and clicks be marked.
    path: Option<screenilux_core::pointer::Track>,
    /// Where each screen sat on the desktop, by its file name. The pointer
    /// is recorded in desktop coordinates, so placing it on a picture needs
    /// to know which part of the desktop that picture was.
    desks: Vec<(String, screenilux_core::Rect)>,
    /// How to draw the pointer, for a recording that left it out of the
    /// picture, and the shortcuts to show.
    drawn_pointer: Option<screenilux_core::edit::PointerStyle>,
    shortcuts: Vec<screenilux_core::keys::Shortcut>,
    /// Whether to lay the camera's own video over the picture, as the
    /// editor last said. Shown unless it has been switched off.
    camera_shown: bool,
    /// Whether an export is being written, as the editor last reported it.
    /// The menu stands down while one is, so a second cannot be started on
    /// top of the first.
    exporting: bool,
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
            recents: None,
            link: (Link::None, false),
            asked: None,
            export_shape: Shape::Wide,
            editing: false,
            placed: None,
            stage: None,
            footer: 0.0,
            footer_at: None,
            live: Vec::new(),
            path: None,
            desks: Vec::new(),
            drawn_pointer: None,
            shortcuts: Vec::new(),
            camera_shown: true,
            exporting: false,
        }
    }

    /// Let the editor take plain clicks on the paused picture. Dragging
    /// still selects text either way, so reading a recording and editing it
    /// do not fight over the same gesture.
    pub fn set_editing(&mut self, on: bool) {
        self.editing = on;
    }

    pub fn take_placed(&mut self) -> Option<Placed> {
        self.placed.take()
    }

    /// The effects to draw over the picture, worked out by the editor for
    /// wherever the clock is now.
    pub fn set_live(&mut self, live: Vec<screenilux_core::edit::Effect>) {
        self.live = live;
    }

    /// The pointer's path through this recording and where its screens sat,
    /// both read from the manifest by whoever opened it.
    pub fn set_path(
        &mut self,
        path: Option<screenilux_core::pointer::Track>,
        desks: Vec<(String, screenilux_core::Rect)>,
    ) {
        self.path = path;
        self.desks = desks;
    }

    /// What to draw that the recording left out of the picture: the
    /// pointer, in this style, and these shortcuts.
    pub fn set_drawn(
        &mut self,
        pointer: Option<screenilux_core::edit::PointerStyle>,
        shortcuts: Vec<screenilux_core::keys::Shortcut>,
    ) {
        self.drawn_pointer = pointer;
        self.shortcuts = shortcuts;
    }

    /// Lay the camera's own video over the picture, or leave it off.
    pub fn set_camera_shown(&mut self, shown: bool) {
        self.camera_shown = shown;
    }

    /// Tell the title row whether an export is running, so the menu can
    /// stand down while one is.
    pub fn set_exporting(&mut self, busy: bool) {
        self.exporting = busy;
    }

    /// Where the picture is on screen, once there has been a frame to draw.
    pub fn stage(&self) -> Option<egui::Rect> {
        self.stage
    }

    /// Keep `height` points at the bottom of the card for an editor's own
    /// controls, so they sit inside the picture's card instead of floating
    /// under it.
    pub fn set_footer(&mut self, height: f32) {
        self.footer = height;
    }

    /// Where that band ended up, once there has been a frame to draw.
    pub fn footer_at(&self) -> Option<egui::Rect> {
        self.footer_at
    }

    /// The picture fills the display, with the controls floating over it.
    pub fn is_full(&self) -> bool {
        self.full
    }

    /// The open recording's clock, which is the only clock: an editor reads
    /// and moves the position through this rather than decoding its own.
    pub fn clock(&self) -> Option<&screenilux_player::SharedClock> {
        self.loaded.as_ref().map(|l| l.player.clock())
    }

    /// The screens' files in display order, for an edit that names one.
    /// The file a screen was decoded from, which for a package is wherever
    /// it was unpacked to. What an export actually reads.
    pub fn track_file(&self, track: usize) -> Option<PathBuf> {
        let l = self.loaded.as_ref()?;
        l.opened.recording.tracks.get(track).map(|t| t.path.clone())
    }

    /// Every screen of the open recording, for writing it out: its name,
    /// the file its video was decoded from and its size.
    pub fn export_screens(&self) -> Vec<(String, PathBuf, (u32, u32))> {
        self.loaded.as_ref().map_or_else(Vec::new, |l| {
            l.opened
                .recording
                .tracks
                .iter()
                .map(|t| {
                    let name = t
                        .path
                        .file_name()
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or_else(|| t.id.clone());
                    (name, t.path.clone(), (t.width, t.height))
                })
                .collect()
        })
    }

    /// The open recording, for reading its text.
    pub fn recording(&self) -> Option<screenilux_core::playback::Recording> {
        self.loaded.as_ref().map(|l| l.opened.recording.clone())
    }

    /// Each screen's name, as the text read off it names it, and its size
    /// in pixels.
    pub fn screen_sizes(&self) -> Vec<(String, (u32, u32))> {
        self.loaded.as_ref().map_or_else(Vec::new, |l| {
            l.opened
                .recording
                .tracks
                .iter()
                .map(|t| (t.id.clone(), (t.width, t.height)))
                .collect()
        })
    }

    /// The text read off the open recording before, if it has been.
    pub fn stored_text(&self) -> Option<screenilux_core::text::TextIndex> {
        screenilux_player::open::stored_text(&self.shareable()?)
    }

    /// Keep the text read off the open recording with it, so it is not read
    /// again and a link to it can be searched.
    pub fn keep_text(&self, index: &screenilux_core::text::TextIndex) {
        if let Some(l) = &self.loaded
            && let Err(e) = screenilux_player::open::save_text(&l.opened.origin, index)
        {
            tracing::warn!(error = %e, "could not keep the text read off the recording");
        }
    }

    pub fn screen_names(&self) -> Vec<String> {
        self.loaded.as_ref().map_or_else(Vec::new, |l| {
            l.opened
                .recording
                .tracks
                .iter()
                .map(|t| {
                    t.path
                        .file_name()
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or_default()
                })
                .collect()
        })
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
                    camera: None,
                });
                self.camera_shown = true;
            }
            Err(e) => self.status = Some(format!("Could not open it: {e}")),
        }
    }

    /// The file of the open recording, if it is one that can be shared: a
    /// package or a video, not an unpacked folder.
    pub fn shareable(&self) -> Option<PathBuf> {
        match &self.loaded.as_ref()?.opened.origin {
            Origin::Package(p) | Origin::Video(p) => Some(p.clone()),
            Origin::Folder(_) => None,
        }
    }

    /// Where the open recording's link stands, to show beside its title.
    pub fn set_link(&mut self, link: Link, can_share: bool) {
        self.link = (link, can_share);
    }

    /// Sharing the open recording, if it was asked for since last time.
    pub fn take_asked(&mut self) -> Option<Asked> {
        self.asked.take()
    }

    pub fn ui(&mut self, ui: &mut egui::Ui, ctx: &egui::Context, out_dir: &Path) {
        self.finish_opening(ctx);
        let p = theme::palette(ui);

        if self.loaded.is_none() {
            self.set_full(ctx, false);
            egui::CentralPanel::default_margins()
                .frame(egui::Frame::new().fill(p.base).inner_margin(24))
                .show(ui, |ui| {
                    ui.centered_and_justified(|ui| {
                        if let Some((name, _)) = &self.opening {
                            ui.horizontal(|ui| {
                                ui.spinner();
                                ui.label(format!("Opening {name}…"));
                            });
                        }
                    });
                });
            let _ = out_dir;
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
                // The picture, its controls and whatever an editor puts
                // below them are one object, so they share one card: three
                // bands of it rather than a card and something floating
                // under the card.
                let foot = self.footer;
                let bottom = card.max.y - foot;
                let stage = egui::Rect::from_min_max(
                    card.min + egui::vec2(16.0, 16.0),
                    egui::pos2(card.max.x - 16.0, bottom - 72.0),
                );
                let controls = egui::Rect::from_min_max(
                    egui::pos2(card.min.x + 16.0, bottom - 72.0),
                    egui::pos2(card.max.x - 16.0, bottom - 8.0),
                );
                self.footer_at = (foot > 0.0).then(|| {
                    let band = egui::Rect::from_min_max(
                        egui::pos2(card.min.x, bottom),
                        egui::pos2(card.max.x, card.max.y),
                    );
                    // A hairline where the bands meet, so the card reads as
                    // sections of one thing rather than one flat expanse.
                    ui.painter().line_segment(
                        [
                            egui::pos2(band.left() + 16.0, band.top()),
                            egui::pos2(band.right() - 16.0, band.top()),
                        ],
                        egui::Stroke::new(1.0, theme::blend(p.stroke, p.stage, 0.45)),
                    );
                    band.shrink2(egui::vec2(16.0, 12.0))
                });
                let mut stage_ui = ui.new_child(egui::UiBuilder::new().max_rect(stage));
                self.video(&mut stage_ui, ctx, stage.size());
                let mut controls_ui = ui.new_child(
                    egui::UiBuilder::new()
                        .max_rect(controls)
                        .layout(egui::Layout::left_to_right(egui::Align::Center)),
                );
                self.controls_row(&mut controls_ui, ctx);
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
            // The way back to the list, where the recording came from.
            if self.editing {
                let back =
                    egui::Button::new(egui::RichText::new(icon::BACK).size(15.0).color(p.text))
                        .fill(p.base)
                        .stroke(egui::Stroke::new(1.0, p.stroke))
                        .corner_radius(10)
                        .min_size(egui::vec2(38.0, 38.0));
                if ui
                    .add(back)
                    .on_hover_text("Back to your recordings. Effects are saved first.")
                    .clicked()
                {
                    self.asked = Some(Asked::Back);
                }
                ui.add_space(12.0);
            }
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
            let multi = rec.is_multi();
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                // The link first, at the far right, then the GIF beside it:
                // both are ways of handing the demo to someone. Then the
                // layouts.
                if let Some(asked) = sharing::link_chip(ui, &self.link.0, self.link.1) {
                    self.asked = Some(asked);
                }
                if self.editing {
                    ui.add_space(8.0);
                    self.export_menu(ui, &p);
                }
                if multi {
                    ui.add_space(12.0);
                    let labels = ["Follow the pointer", "Side by side", "Stacked"];
                    let chosen = Layout::ALL
                        .iter()
                        .position(|l| *l == self.layout)
                        .unwrap_or(0);
                    if let Some(i) = theme::segmented(ui, &labels, chosen) {
                        self.layout = Layout::ALL[i];
                    }
                }
            });
        });
    }

    /// The export button: a menu, not a verb.
    ///
    /// One button that wrote a GIF hid the two exports people actually
    /// wanted, because a button can only say one thing and there was no
    /// room for the other two. A menu has room for all three and, more to
    /// the point, says out loud that a choice exists.
    fn export_menu(&mut self, ui: &mut egui::Ui, p: &theme::Palette) {
        if self.exporting {
            ui.add_enabled(false, theme::outline("Exporting…"));
            return;
        }
        let label = format!("{}  Export  {}", icon::EXPORT, icon::CHEVRON_DOWN);
        let mut picked = None;
        let shape = &mut self.export_shape;
        egui::containers::menu::MenuButton::from_button(theme::outline(label)).ui(ui, |ui| {
            ui.set_width(EXPORT_MENU_W);
            // The shape first: it applies to the video and the GIF alike,
            // and is the question people forget to ask before exporting.
            ui.horizontal(|ui| {
                ui.add_space(10.0);
                ui.label(egui::RichText::new("Shape").size(13.0).color(p.weak));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.add_space(10.0);
                    let labels: Vec<&str> = Shape::ALL.iter().map(|s| s.label()).collect();
                    let chosen = Shape::ALL.iter().position(|s| s == shape).unwrap_or(0);
                    if let Some(i) = theme::segmented(ui, &labels, chosen) {
                        *shape = Shape::ALL[i];
                    }
                });
            });
            ui.add_space(6.0);
            // The rows carry their own spacing, so the style's does not
            // stack on top of it.
            ui.spacing_mut().item_spacing.y = 0.0;
            for format in Format::ALL {
                if crate::edit_view::item(ui, p, format.label(), format.hint()) {
                    picked = Some(format);
                    ui.close();
                }
            }
        });
        if let Some(format) = picked {
            self.asked = Some(Asked::Export(format, self.export_shape));
        }
    }

    /// The Recordings tab, in the main window: opening a file, and the
    /// recordings in the save folder. Playing one opens the player window.
    pub fn library(&mut self, ui: &mut egui::Ui, ctx: &egui::Context, out_dir: &Path) {
        let p = theme::palette(ui);
        let dragging = ctx.input(|i| !i.raw.hovered_files.is_empty());
        let width = ui.available_width();

        ui.vertical_centered(|ui| {
            ui.set_max_width(width);
            ui.add_space(4.0);

            let (rect, _) = ui.allocate_exact_size(egui::vec2(width, 170.0), egui::Sense::hover());
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
            inner.add_space(6.0);
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
                    egui::RichText::new("or drop a .screenilux file or an MP4 here")
                        .size(13.0)
                        .color(p.weak),
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
                // The list scrolls in whatever height the window has left.
                egui::ScrollArea::vertical()
                    .auto_shrink([false; 2])
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

    /// Whether the player window should be open: a recording is open or
    /// opening.
    pub fn has_window(&self) -> bool {
        self.loaded.is_some() || self.opening.is_some()
    }

    /// The open recording's name, for the player window's title.
    pub fn title(&self) -> Option<String> {
        if let Some((name, _)) = &self.opening {
            return Some(friendly_name(name));
        }
        Some(friendly_name(&self.loaded.as_ref()?.opened.title))
    }

    /// Close the open recording, as when its window is closed.
    pub fn close(&mut self) {
        self.loaded = None;
        self.opening = None;
        self.full = false;
        self.pinned = None;
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
        // A "show one screen" effect overrides what the pointer chose; it
        // is the one thing the edit list has to say about which screen is
        // on, so it outranks the recording's own focus track.
        let forced = self.live.iter().find_map(|e| match &e.tool {
            screenilux_core::edit::Tool::Screen { file } => rec
                .tracks
                .iter()
                .position(|t| t.path.file_name().is_some_and(|n| n == file.as_str())),
            _ => None,
        });
        let active = forced
            .or(self.pinned)
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
        self.stage = Some(rect);
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

        if let Some(feed) = loaded.player.camera_feed() {
            upload(ctx, "camera", feed, &mut loaded.camera);
        }

        let multi = placements.len() > 1;
        for p in &placements {
            let r = to_rect(p.area);
            // What the edit list says is happening to this screen now.
            let screen = loaded.opened.recording.tracks[p.track]
                .path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned());
            let live = crate::effects::for_screen(&self.live, screen.as_deref());
            let at = loaded.player.clock().position();
            let bounds = screen
                .as_deref()
                .and_then(|name| self.desks.iter().find(|(f, _)| f == name).map(|(_, b)| *b));
            let on = bounds.and_then(|b| crate::effects::pointer_on(self.path.as_ref(), b, at));
            let shown = crate::effects::lens_following(&live, at, on);
            // A backdrop insets the picture, so it decides the rectangle
            // everything else is measured against.
            let r = crate::effects::backdrop(&painter, &pal, r, &live, at);
            match &loaded.textures[p.track] {
                Some((_, tex)) => {
                    painter.image(tex.id(), r, shown, egui::Color32::WHITE);
                    let marks = self.path.as_ref().zip(bounds);
                    crate::effects::over(
                        &painter,
                        &pal,
                        r,
                        shown,
                        Some(tex.id()),
                        &live,
                        at,
                        marks,
                    );
                    // Over the picture and under the pointer, unmoved by a zoom.
                    if self.camera_shown
                        && let (Some(cam), Some((_, face))) =
                            (&loaded.opened.recording.camera, &loaded.camera)
                    {
                        let t = &loaded.opened.recording.tracks[p.track];
                        let desk = bounds
                            .unwrap_or_else(|| screenilux_core::Rect::new(0, 0, t.width, t.height));
                        bubble(&painter, face, &cam.video, at, desk, r);
                    }
                    if let (Some(style), Some(track), Some(b)) =
                        (self.drawn_pointer.as_ref(), self.path.as_ref(), bounds)
                    {
                        crate::effects::pointer::cursor(&painter, r, shown, track, b, at, style);
                    }
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

        let at = loaded.player.clock().position();
        crate::effects::pointer::shortcut(&painter, rect, &self.shortcuts, at);

        let paused = !loaded.player.clock().is_playing();

        if response.double_clicked() {
            let to = !self.full;
            self.set_full(ctx, to);
        }
        if response.clicked() {
            if !paused {
                // Clicking while it plays pauses it, ready for selecting
                // text or for placing an edit on what is on screen.
                if let Some(l) = &self.loaded {
                    l.player.clock().pause();
                }
            } else if self.editing
                && let Some(pos) = response.interact_pointer_pos()
                && let Some(p) = placements.iter().find(|p| to_rect(p.area).contains(pos))
            {
                let r = to_rect(p.area);
                self.placed = Some(Placed {
                    track: p.track,
                    x: (pos.x - r.left()) / r.width(),
                    y: (pos.y - r.top()) / r.height(),
                });
            }
        }
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
        let marks: Vec<f64> = Vec::new();

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
        // While the editor is showing it puts a timeline under the picture,
        // and that timeline is a scrubber with the moments drawn on it. One
        // time axis, not two stacked bars saying the same thing.
        let seek = if self.editing && !self.full {
            None
        } else {
            let right = ui.available_width();
            let mut tail = 54.0;
            if multi {
                tail += labels.len() as f32 * 76.0;
            }
            if self.full && multi {
                tail += 124.0;
            }
            scrubber(
                ui,
                (right - tail).max(80.0),
                pos,
                duration,
                &spans,
                &marks,
                &p,
            )
        };

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
        if playing && idle {
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

    fn toggle_play(&mut self) {
        if let Some(l) = &self.loaded {
            if !l.player.clock().is_playing() {
                self.pinned = None;
            }
            l.player.clock().toggle();
        }
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
/// Put a feed's newest frame into `slot`'s texture, if it has moved on.
fn upload(
    ctx: &egui::Context,
    name: &str,
    feed: &screenilux_player::Feed,
    slot: &mut Option<(u64, egui::TextureHandle)>,
) {
    let serial = feed.serial();
    if slot.as_ref().is_some_and(|(s, _)| *s == serial) {
        return;
    }
    let Some(frame) = feed.frame() else { return };
    // Frames are opaque, so premultiplied and straight alpha agree and the
    // cheaper conversion is exact.
    let image = egui::ColorImage::from_rgba_premultiplied(
        [frame.width as usize, frame.height as usize],
        &frame.rgba,
    );
    match slot {
        Some((s, handle)) => {
            handle.set(image, egui::TextureOptions::LINEAR);
            *s = serial;
        }
        None => {
            let handle = ctx.load_texture(name, image, egui::TextureOptions::LINEAR);
            *slot = Some((serial, handle));
        }
    }
}

/// The camera in its pill over the picture `pic` of the screen that sat at
/// `desk`, where it was at `at`, its middle cropped to the pill's shape.
fn bubble(
    painter: &egui::Painter,
    face: &egui::TextureHandle,
    video: &screenilux_core::session::CameraVideo,
    at: f64,
    desk: screenilux_core::Rect,
    pic: egui::Rect,
) {
    let Some((x, y, w, h)) =
        video.placed(at, desk, (pic.min.x, pic.min.y, pic.width(), pic.height()))
    else {
        return;
    };
    let rect = egui::Rect::from_min_size(egui::pos2(x, y), egui::vec2(w, h));
    let [tw, th] = face.size().map(|v| v as f32);
    let (want, have) = (w / h.max(1.0), tw / th.max(1.0));
    let uv = if have > want {
        let k = want / have;
        egui::Rect::from_min_max(
            egui::pos2((1.0 - k) / 2.0, 0.0),
            egui::pos2((1.0 + k) / 2.0, 1.0),
        )
    } else {
        let k = have / want;
        egui::Rect::from_min_max(
            egui::pos2(0.0, (1.0 - k) / 2.0),
            egui::pos2(1.0, (1.0 + k) / 2.0),
        )
    };
    let round = egui::CornerRadius::same((w.min(h) / 2.0).min(255.0) as u8);
    painter.add(
        egui::epaint::RectShape::filled(rect, round, egui::Color32::WHITE)
            .with_texture(face.id(), uv),
    );
    // The same faint rim the export draws.
    painter.rect_stroke(
        rect,
        round,
        egui::Stroke::new(1.5, egui::Color32::from_black_alpha(60)),
        egui::StrokeKind::Inside,
    );
}

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
