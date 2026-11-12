//! The Recordings tab's list: every recording in the save folder as a card
//! with a picture from it, a page at a time.
//!
//! Every recording Screenilux makes is named for when it was made, so a list
//! of names is a list of dates, and the picture is what tells two of them
//! apart. Cards put it first. Pages, rather than one long scroll, keep the
//! place when a recording is opened and closed again, and mean pictures are
//! only read for the dozen on show.

use std::collections::HashMap;
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use eframe::egui;

use crate::player_view::{age_text, clock_text, friendly_name, size_text};
use crate::sharing::Link;
use crate::theme::{self, ON_DARK, Palette, icon};
use crate::thumbs::{Found, Thumbs};

/// Cards on a page: whole rows at one, two, three or four across.
pub const PAGE_SIZE: usize = 12;
/// The narrowest a card gets before a column is dropped.
const CARD_MIN: f32 = 210.0;
const MOST_COLUMNS: usize = 4;
const GAP: f32 = 16.0;
/// Under the picture: the name and one line of details.
const CAPTION: f32 = 52.0;
/// Below this width the filter gets a row of its own.
const NARROW: f32 = 620.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Sort {
    #[default]
    Newest,
    Oldest,
    Largest,
}

impl Sort {
    pub const ALL: [Sort; 3] = [Sort::Newest, Sort::Oldest, Sort::Largest];

    pub fn label(self) -> &'static str {
        match self {
            Sort::Newest => "Newest first",
            Sort::Oldest => "Oldest first",
            Sort::Largest => "Largest first",
        }
    }
}

/// A recording in the save folder, as the folder describes it.
#[derive(Debug, Clone)]
pub struct Entry {
    pub path: PathBuf,
    pub name: String,
    pub size: u64,
    pub modified: SystemTime,
}

impl Entry {
    fn is_video(&self) -> bool {
        self.path
            .extension()
            .is_some_and(|e| e.eq_ignore_ascii_case("mp4"))
    }
}

/// What the worker found for a recording, kept while the file is unchanged.
enum Details {
    Waiting,
    Ready {
        screens: usize,
        duration: f64,
        picture: Option<egui::TextureHandle>,
    },
    Failed(String),
}

struct Slot {
    /// Size and time the file had when this was found, so a recording that
    /// changes (effects saved into it) is read again.
    stamp: (u64, SystemTime),
    details: Details,
}

/// What the person asked for.
pub enum Pick {
    Open(PathBuf),
    Browse,
}

pub struct Library {
    /// Recordings in the save folder, read when first shown.
    entries: Option<Vec<Entry>>,
    page: usize,
    sort: Sort,
    filter: String,
    details: HashMap<PathBuf, Slot>,
    thumbs: Option<Thumbs>,
}

impl Library {
    pub fn new() -> Self {
        Self {
            entries: None,
            page: 0,
            sort: Sort::Newest,
            filter: String::new(),
            details: HashMap::new(),
            thumbs: None,
        }
    }

    /// Read the folder again next time. The page is kept, as far as there
    /// still are pages.
    pub fn rescan(&mut self) {
        self.entries = None;
    }

    /// Take in what the worker has found, as textures.
    fn take_found(&mut self, ctx: &egui::Context) {
        let Some(thumbs) = &self.thumbs else { return };
        for (path, found) in thumbs.take() {
            let Some(slot) = self.details.get_mut(&path) else {
                continue;
            };
            slot.details = match found {
                Ok(Found {
                    screens,
                    duration,
                    picture,
                }) => Details::Ready {
                    screens,
                    duration,
                    picture: picture.map(|image| {
                        ctx.load_texture(
                            format!("library-{}", path.display()),
                            image,
                            egui::TextureOptions::LINEAR,
                        )
                    }),
                },
                Err(e) => Details::Failed(e),
            };
        }
    }

    /// Ask the worker for whatever on this page it has not found yet.
    fn want(&mut self, ctx: &egui::Context, shown: &[&Entry]) {
        let mut wanted = Vec::new();
        for e in shown {
            let stamp = (e.size, e.modified);
            let slot = self.details.entry(e.path.clone()).or_insert(Slot {
                stamp,
                details: Details::Waiting,
            });
            if slot.stamp != stamp {
                *slot = Slot {
                    stamp,
                    details: Details::Waiting,
                };
            }
            if matches!(slot.details, Details::Waiting) {
                wanted.push(e.path.clone());
            }
        }
        self.thumbs
            .get_or_insert_with(|| Thumbs::start(ctx))
            .want(wanted);
    }

    pub fn ui(
        &mut self,
        ui: &mut egui::Ui,
        ctx: &egui::Context,
        out_dir: &Path,
        link_of: &dyn Fn(&Path) -> Link,
        opening: Option<&str>,
        status: Option<&str>,
    ) -> Option<Pick> {
        let p = theme::palette(ui);
        let area = ui.available_rect_before_wrap();
        let dragging = ctx.input(|i| !i.raw.hovered_files.is_empty());
        self.take_found(ctx);
        let entries = self.entries.get_or_insert_with(|| scan(out_dir)).clone();
        let mut pick = None;

        if entries.is_empty() {
            pick = empty_state(ui, &p, dragging, out_dir);
            under_header(ui, opening, status);
            return pick;
        }

        let order = arrange(&entries, &self.filter, self.sort);
        let pages = page_count(order.len(), PAGE_SIZE);
        if !ctx.egui_wants_keyboard_input() {
            let (up, down) = ctx.input(|i| {
                (
                    i.key_pressed(egui::Key::PageUp),
                    i.key_pressed(egui::Key::PageDown),
                )
            });
            if up {
                self.page = self.page.saturating_sub(1);
            }
            if down {
                self.page += 1;
            }
        }
        self.page = clamp_page(self.page, order.len(), PAGE_SIZE);
        let span = page_span(self.page, order.len(), PAGE_SIZE);
        let shown: Vec<&Entry> = order[span.clone()].iter().map(|&i| &entries[i]).collect();
        self.want(ctx, &shown);

        if self.header(ui, &p, entries.len()) {
            pick = Some(Pick::Browse);
        }
        under_header(ui, opening, status);
        ui.add_space(14.0);

        if shown.is_empty() {
            ui.vertical_centered(|ui| {
                ui.add_space(40.0);
                let text = format!(
                    "No recordings match \u{201C}{}\u{201D}.",
                    self.filter.trim()
                );
                ui.label(egui::RichText::new(text).size(15.0));
                ui.add_space(8.0);
                if ui.add(theme::outline("Clear the filter")).clicked() {
                    self.filter.clear();
                }
            });
        } else {
            let footer = if pages > 1 { 56.0 } else { 0.0 };
            let width = ui.available_width();
            let columns = columns(width);
            let card_w = ((width - GAP * (columns - 1) as f32) / columns as f32).floor();
            let card_h = (card_w * 9.0 / 16.0).round() + CAPTION;
            egui::ScrollArea::vertical()
                // A page starts at its top, whichever page was scrolled.
                .id_salt(("library page", self.page))
                .max_height((ui.available_height() - footer).max(card_h))
                .auto_shrink([false, true])
                .show(ui, |ui| {
                    for row in shown.chunks(columns) {
                        let (band, _) =
                            ui.allocate_exact_size(egui::vec2(width, card_h), egui::Sense::hover());
                        for (i, e) in row.iter().enumerate() {
                            let rect = egui::Rect::from_min_size(
                                band.min + egui::vec2(i as f32 * (card_w + GAP), 0.0),
                                egui::vec2(card_w, card_h),
                            );
                            let details = self.details.get(&e.path).map(|s| &s.details);
                            if card(ui, &p, rect, e, details, &link_of(&e.path)).clicked() {
                                pick = Some(Pick::Open(e.path.clone()));
                            }
                        }
                        ui.add_space(GAP);
                    }
                });
            if pages > 1 {
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    ui.label(
                        egui::RichText::new(format!(
                            "{}\u{2013}{} of {}",
                            span.start + 1,
                            span.end,
                            order.len()
                        ))
                        .size(13.0)
                        .color(p.weak),
                    );
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if let Some(page) = pager(ui, &p, self.page, pages) {
                            self.page = page;
                        }
                    });
                });
            }
        }

        if dragging {
            drop_overlay(ui, &p, area);
        }
        pick
    }

    /// The title and count, then filter, sort and open. Returns whether
    /// opening a file was asked for.
    fn header(&mut self, ui: &mut egui::Ui, p: &Palette, total: usize) -> bool {
        let narrow = ui.available_width() < NARROW;
        let mut browse = false;
        ui.horizontal(|ui| {
            ui.label(
                egui::RichText::new("Recordings").font(egui::FontId::new(22.0, theme::bold())),
            );
            ui.label(
                egui::RichText::new(total.to_string())
                    .size(14.0)
                    .color(p.weak),
            );
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let open = format!("{}  Open a file", icon::FOLDER);
                if ui
                    .add(theme::outline(open))
                    .on_hover_text("Open a .screenilux file or an MP4 from anywhere")
                    .clicked()
                {
                    browse = true;
                }
                ui.add_space(4.0);
                self.sort_menu(ui, p);
                if !narrow {
                    ui.add_space(4.0);
                    self.filter_field(ui, p, 200.0);
                }
            });
        });
        if narrow {
            ui.add_space(8.0);
            let w = ui.available_width();
            self.filter_field(ui, p, w);
        }
        browse
    }

    fn filter_field(&mut self, ui: &mut egui::Ui, p: &Palette, width: f32) {
        let before = self.filter.clone();
        let response = ui.add(
            egui::TextEdit::singleline(&mut self.filter)
                .hint_text(
                    egui::RichText::new(format!("{}  Filter by name", icon::SEARCH)).color(p.weak),
                )
                .desired_width(width - 26.0)
                .margin(egui::Margin::symmetric(12, 11)),
        );
        if response.has_focus() && ui.input(|i| i.key_pressed(egui::Key::Escape)) {
            self.filter.clear();
        }
        if self.filter != before {
            self.page = 0;
        }
    }

    fn sort_menu(&mut self, ui: &mut egui::Ui, p: &Palette) {
        let label = format!("{}  {}", self.sort.label(), icon::CHEVRON_DOWN);
        let mut picked = None;
        egui::containers::menu::MenuButton::from_button(theme::outline(label)).ui(ui, |ui| {
            ui.set_width(200.0);
            ui.spacing_mut().item_spacing.y = 0.0;
            for sort in Sort::ALL {
                let hint = match sort {
                    Sort::Newest => "The last one you made at the top",
                    Sort::Oldest => "In the order they were made",
                    Sort::Largest => "The biggest files at the top",
                };
                if crate::edit_view::item(ui, p, sort.label(), hint) {
                    picked = Some(sort);
                    ui.close();
                }
            }
        });
        if let Some(sort) = picked
            && sort != self.sort
        {
            self.sort = sort;
            self.page = 0;
        }
    }
}

/// Opening a recording, or why something could not be opened, under the
/// header where it is seen.
fn under_header(ui: &mut egui::Ui, opening: Option<&str>, status: Option<&str>) {
    if let Some(name) = opening {
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            ui.spinner();
            ui.label(format!("Opening {name}\u{2026}"));
        });
    }
    if let Some(s) = status {
        ui.add_space(8.0);
        theme::notice(ui, s);
    }
}

/// One recording: its picture with its length over it, then its name and
/// details.
fn card(
    ui: &mut egui::Ui,
    p: &Palette,
    rect: egui::Rect,
    e: &Entry,
    details: Option<&Details>,
    link: &Link,
) -> egui::Response {
    let response = ui
        .interact(
            rect,
            ui.scope_id().with(("card", &e.path)),
            egui::Sense::click(),
        )
        .on_hover_cursor(egui::CursorIcon::PointingHand);
    response.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, &e.name));
    let painter = ui.painter_at(rect.expand(4.0));
    let lit = response.hovered() || response.has_focus();

    let picture =
        egui::Rect::from_min_size(rect.min, egui::vec2(rect.width(), rect.height() - CAPTION));
    painter.rect_filled(picture, 14.0, p.stage);
    match details {
        Some(Details::Ready {
            picture: Some(tex), ..
        }) => {
            egui::Image::new(tex)
                .uv(cover(tex.size_vec2(), picture.size()))
                .corner_radius(14)
                .paint_at(ui, picture);
        }
        Some(Details::Failed(why)) => {
            painter.text(
                picture.center() - egui::vec2(0.0, 10.0),
                egui::Align2::CENTER_CENTER,
                icon::WARNING,
                egui::FontId::proportional(22.0),
                p.weak,
            );
            painter.text(
                picture.center() + egui::vec2(0.0, 16.0),
                egui::Align2::CENTER_CENTER,
                why,
                egui::FontId::proportional(12.0),
                p.weak,
            );
        }
        Some(Details::Ready { picture: None, .. }) => {
            painter.text(
                picture.center(),
                egui::Align2::CENTER_CENTER,
                icon::MONITOR,
                egui::FontId::proportional(26.0),
                p.weak,
            );
        }
        Some(Details::Waiting) | None => {
            let at = egui::Rect::from_center_size(picture.center(), egui::vec2(18.0, 18.0));
            // Painted, not placed: placing would move the grid's cursor.
            egui::Spinner::new().color(p.weak).paint_at(ui, at);
        }
    }
    if lit {
        painter.rect_stroke(
            picture,
            14.0,
            egui::Stroke::new(2.0, p.signal),
            egui::StrokeKind::Inside,
        );
    }

    let (screens, duration) = match details {
        Some(Details::Ready {
            screens, duration, ..
        }) => (*screens, *duration),
        _ => (0, 0.0),
    };
    if duration > 0.0 {
        badge(
            &painter,
            p,
            picture.right_bottom() + egui::vec2(-8.0, -8.0),
            egui::Align2::RIGHT_BOTTOM,
            &clock_text(duration),
        );
    }
    let shared = match link {
        Link::Ready { .. } => Some("Shared".to_string()),
        Link::Uploading { done, .. } => Some(format!("Uploading {:.0}%", done * 100.0)),
        Link::Starting => Some("Sharing\u{2026}".to_string()),
        _ => None,
    };
    if let Some(text) = shared {
        badge(
            &painter,
            p,
            picture.left_top() + egui::vec2(8.0, 8.0),
            egui::Align2::LEFT_TOP,
            &format!("{}  {text}", icon::LINK),
        );
    }

    let name_at = egui::pos2(rect.left() + 2.0, picture.bottom() + 10.0);
    let name = one_line(
        ui,
        &e.name,
        rect.width() - 4.0,
        if lit { p.signal } else { p.text },
    );
    painter.galley(name_at, name, p.text);

    let mut parts = Vec::new();
    if e.is_video() {
        parts.push("MP4".to_string());
    }
    if screens > 1 {
        parts.push(format!("{screens} screens"));
    }
    parts.push(size_text(e.size));
    let age = SystemTime::now()
        .duration_since(e.modified)
        .unwrap_or_default();
    parts.push(age_text(age));
    painter.text(
        egui::pos2(rect.left() + 2.0, picture.bottom() + 32.0),
        egui::Align2::LEFT_TOP,
        parts.join(" \u{00B7} "),
        egui::FontId::proportional(12.0),
        p.weak,
    );

    if response.gained_focus() {
        // Tabbing onto a card below the fold brings it into view.
        ui.scroll_to_rect(rect, None);
    }
    response.on_hover_text(e.path.display().to_string())
}

/// The name on one line, cut with an ellipsis when it does not fit.
fn one_line(
    ui: &egui::Ui,
    text: &str,
    width: f32,
    ink: egui::Color32,
) -> std::sync::Arc<egui::Galley> {
    let mut job = egui::text::LayoutJob::single_section(
        text.to_string(),
        egui::TextFormat::simple(egui::FontId::new(14.0, theme::strong()), ink),
    );
    job.wrap = egui::text::TextWrapping::truncate_at_width(width);
    ui.painter().layout_job(job)
}

/// A small dark label over a picture, readable on any content.
fn badge(painter: &egui::Painter, p: &Palette, at: egui::Pos2, align: egui::Align2, text: &str) {
    let galley = painter.layout_no_wrap(
        text.to_string(),
        egui::FontId::new(12.0, theme::strong()),
        ON_DARK,
    );
    let size = galley.size() + egui::vec2(14.0, 6.0);
    let rect = align.anchor_size(at, size);
    painter.rect_filled(rect, 8.0, p.bar.gamma_multiply(0.85));
    painter.galley(rect.min + egui::vec2(7.0, 3.0), galley, ON_DARK);
}

/// The part of a picture that fills `frame` without stretching: the middle,
/// trimmed at the sides or the top and bottom.
fn cover(picture: egui::Vec2, frame: egui::Vec2) -> egui::Rect {
    let (pa, fa) = (picture.x / picture.y.max(1.0), frame.x / frame.y.max(1.0));
    if pa > fa {
        let keep = fa / pa;
        egui::Rect::from_min_max(
            egui::pos2((1.0 - keep) / 2.0, 0.0),
            egui::pos2((1.0 + keep) / 2.0, 1.0),
        )
    } else {
        let keep = pa / fa;
        egui::Rect::from_min_max(
            egui::pos2(0.0, (1.0 - keep) / 2.0),
            egui::pos2(1.0, (1.0 + keep) / 2.0),
        )
    }
}

/// Previous, the page numbers, next.
fn pager(ui: &mut egui::Ui, p: &Palette, page: usize, pages: usize) -> Option<usize> {
    let mut picked = None;
    // Laid out right to left: next first.
    let arrow = |ui: &mut egui::Ui, glyph: &str, enabled: bool, hint: &str| {
        ui.add_enabled(
            enabled,
            egui::Button::new(egui::RichText::new(glyph).size(13.0))
                .fill(p.base)
                .stroke(egui::Stroke::new(1.0, p.stroke))
                .corner_radius(10)
                .min_size(egui::vec2(36.0, 36.0)),
        )
        .on_hover_text(hint)
        .clicked()
    };
    if arrow(
        ui,
        icon::CHEVRON_RIGHT,
        page + 1 < pages,
        "Next page (Page Down)",
    ) {
        picked = Some(page + 1);
    }
    ui.add_space(4.0);
    let slots = page_buttons(page, pages);
    let font = egui::FontId::new(13.0, theme::strong());
    let w = 34.0;
    let (track, _) = ui.allocate_exact_size(
        egui::vec2(w * slots.len() as f32 + 6.0, 38.0),
        egui::Sense::hover(),
    );
    ui.painter().rect_filled(track, 12.0, p.surface);
    for (i, slot) in slots.iter().enumerate() {
        let rect = egui::Rect::from_min_size(
            egui::pos2(track.left() + 3.0 + i as f32 * w, track.top() + 3.0),
            egui::vec2(w, track.height() - 6.0),
        );
        let Some(n) = *slot else {
            ui.painter().text(
                rect.center(),
                egui::Align2::CENTER_CENTER,
                "\u{2026}",
                font.clone(),
                p.weak,
            );
            continue;
        };
        let response = ui
            .interact(rect, ui.scope_id().with(("page", n)), egui::Sense::click())
            .on_hover_cursor(egui::CursorIcon::PointingHand);
        response.widget_info(|| {
            let label = format!("Page {}", n + 1);
            egui::WidgetInfo::selected(egui::Role::Button, true, n == page, label)
        });
        if n == page {
            ui.painter().rect(
                rect,
                9.0,
                p.base,
                egui::Stroke::new(1.0, p.stroke),
                egui::StrokeKind::Inside,
            );
        }
        let ink = if n == page || response.hovered() {
            p.text
        } else {
            p.weak
        };
        ui.painter().text(
            rect.center(),
            egui::Align2::CENTER_CENTER,
            (n + 1).to_string(),
            font.clone(),
            ink,
        );
        theme::focus_ring(ui, &response, rect, 10);
        if response.clicked() {
            picked = Some(n);
        }
    }
    ui.add_space(4.0);
    if arrow(ui, icon::CHEVRON_LEFT, page > 0, "Previous page (Page Up)") {
        picked = Some(page - 1);
    }
    picked
}

/// No recordings yet: a place to drop or open one, and where new ones go.
fn empty_state(ui: &mut egui::Ui, p: &Palette, dragging: bool, out_dir: &Path) -> Option<Pick> {
    let mut pick = None;
    let width = ui.available_width();
    ui.add_space(4.0);
    let (rect, _) = ui.allocate_exact_size(egui::vec2(width, 270.0), egui::Sense::hover());
    let colour = if dragging { p.signal } else { p.stroke };
    let fill = if dragging {
        theme::blend(p.base, p.signal, 0.10)
    } else {
        p.surface
    };
    ui.painter().rect_filled(rect, 20.0, fill);
    dashed(ui, rect, colour);
    let mut inner = ui.new_child(
        egui::UiBuilder::new()
            .max_rect(rect.shrink(20.0))
            .layout(egui::Layout::top_down(egui::Align::Center)),
    );
    inner.add_space(14.0);
    inner.label(
        egui::RichText::new(icon::MONITOR)
            .size(34.0)
            .color(if dragging { p.signal } else { p.weak }),
    );
    inner.add_space(6.0);
    if dragging {
        inner.label(
            egui::RichText::new("Drop to open").font(egui::FontId::new(16.0, theme::strong())),
        );
        return None;
    }
    inner.label(
        egui::RichText::new("No recordings yet").font(egui::FontId::new(16.0, theme::strong())),
    );
    inner.label(
        egui::RichText::new(format!(
            "Recordings you make are saved in {}.",
            out_dir.display()
        ))
        .size(13.0)
        .color(p.weak),
    );
    inner.add_space(10.0);
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
        pick = Some(Pick::Browse);
    }
    inner.add_space(2.0);
    inner.label(
        egui::RichText::new("or drop a .screenilux file or an MP4 here")
            .size(13.0)
            .color(p.weak),
    );
    pick
}

/// Over the whole list while a file is dragged across the window.
fn drop_overlay(ui: &egui::Ui, p: &Palette, area: egui::Rect) {
    let painter = ui.ctx().layer_painter(egui::LayerId::new(
        egui::Order::Foreground,
        egui::Id::unique("library drop"),
    ));
    painter.rect_filled(
        area,
        20.0,
        theme::blend(p.base, p.signal, 0.10).gamma_multiply(0.94),
    );
    dashed_on(&painter, area.shrink(1.0), p.signal);
    painter.text(
        area.center() - egui::vec2(0.0, 18.0),
        egui::Align2::CENTER_CENTER,
        icon::MONITOR,
        egui::FontId::proportional(34.0),
        p.signal,
    );
    painter.text(
        area.center() + egui::vec2(0.0, 20.0),
        egui::Align2::CENTER_CENTER,
        "Drop to open",
        egui::FontId::new(16.0, theme::strong()),
        p.text,
    );
}

fn dashed(ui: &egui::Ui, rect: egui::Rect, colour: egui::Color32) {
    dashed_on(ui.painter(), rect.shrink(1.0), colour);
}

fn dashed_on(painter: &egui::Painter, r: egui::Rect, colour: egui::Color32) {
    let corners = [
        r.left_top(),
        r.right_top(),
        r.right_bottom(),
        r.left_bottom(),
        r.left_top(),
    ];
    painter.extend(egui::Shape::dashed_line(
        &corners,
        egui::Stroke::new(1.5, colour),
        7.0,
        5.0,
    ));
}

/// Recordings in `dir`: packages and MP4s, in no particular order.
fn scan(dir: &Path) -> Vec<Entry> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    entries
        .flatten()
        .filter_map(|e| {
            let path = e.path();
            let ext = path.extension()?.to_string_lossy().to_lowercase();
            if ext != "screenilux" && ext != "mp4" {
                return None;
            }
            let meta = e.metadata().ok()?;
            if !meta.is_file() {
                return None;
            }
            Some(Entry {
                name: friendly_name(&path.file_stem()?.to_string_lossy()),
                size: meta.len(),
                modified: meta.modified().ok()?,
                path,
            })
        })
        .collect()
}

/// The recordings to list, as indices into `entries`: those whose name or
/// file name holds `filter` (any case), in `sort` order.
pub fn arrange(entries: &[Entry], filter: &str, sort: Sort) -> Vec<usize> {
    let needle = filter.trim().to_lowercase();
    let mut order: Vec<usize> = (0..entries.len())
        .filter(|&i| {
            let e = &entries[i];
            needle.is_empty()
                || e.name.to_lowercase().contains(&needle)
                || e.path
                    .file_name()
                    .is_some_and(|n| n.to_string_lossy().to_lowercase().contains(&needle))
        })
        .collect();
    order.sort_by(|&a, &b| {
        let (a, b) = (&entries[a], &entries[b]);
        match sort {
            Sort::Newest => b.modified.cmp(&a.modified),
            Sort::Oldest => a.modified.cmp(&b.modified),
            Sort::Largest => b.size.cmp(&a.size).then(b.modified.cmp(&a.modified)),
        }
        .then_with(|| a.path.cmp(&b.path))
    });
    order
}

/// How many pages `total` items fill. An empty list is still one page.
pub fn page_count(total: usize, size: usize) -> usize {
    total.div_ceil(size.max(1)).max(1)
}

/// `page`, or the last page if there are no longer that many.
pub fn clamp_page(page: usize, total: usize, size: usize) -> usize {
    page.min(page_count(total, size) - 1)
}

/// Which items `page` shows.
pub fn page_span(page: usize, total: usize, size: usize) -> Range<usize> {
    let size = size.max(1);
    let start = (clamp_page(page, total, size) * size).min(total);
    start..(start + size).min(total)
}

/// The page buttons to show: every page when there are up to seven, else
/// seven slots that always hold the first, the last, and the pages either
/// side of `current`, with `None` for a gap. A fixed count keeps the
/// arrows still as the pages turn.
pub fn page_buttons(current: usize, pages: usize) -> Vec<Option<usize>> {
    const SLOTS: usize = 7;
    if pages <= SLOTS {
        return (0..pages).map(Some).collect();
    }
    let last = pages - 1;
    let current = current.min(last);
    if current <= 3 {
        let mut v: Vec<_> = (0..5).map(Some).collect();
        v.extend([None, Some(last)]);
        v
    } else if current >= last - 3 {
        let mut v = vec![Some(0), None];
        v.extend((last - 4..=last).map(Some));
        v
    } else {
        vec![
            Some(0),
            None,
            Some(current - 1),
            Some(current),
            Some(current + 1),
            None,
            Some(last),
        ]
    }
}

/// How many cards fit across `width`.
pub fn columns(width: f32) -> usize {
    (((width + GAP) / (CARD_MIN + GAP)).floor() as usize).clamp(1, MOST_COLUMNS)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn entry(name: &str, size: u64, age_secs: u64) -> Entry {
        Entry {
            path: PathBuf::from(format!("{name}.screenilux")),
            name: friendly_name(name),
            size,
            modified: SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000 - age_secs),
        }
    }

    #[test]
    fn pages_are_counted_and_spanned() {
        assert_eq!(page_count(0, 12), 1);
        assert_eq!(page_count(12, 12), 1);
        assert_eq!(page_count(13, 12), 2);
        assert_eq!(page_count(38, 12), 4);

        assert_eq!(page_span(0, 38, 12), 0..12);
        assert_eq!(page_span(3, 38, 12), 36..38);
        assert_eq!(page_span(0, 0, 12), 0..0);
        assert_eq!(
            page_span(1, 5, 12),
            0..5,
            "a page past the end shows the last"
        );
    }

    #[test]
    fn a_page_past_the_end_falls_back_to_the_last() {
        // Kept across a rescan that found fewer recordings, or a filter.
        assert_eq!(clamp_page(5, 38, 12), 3);
        assert_eq!(clamp_page(2, 38, 12), 2);
        assert_eq!(clamp_page(4, 0, 12), 0);
    }

    #[test]
    fn page_buttons_keep_a_steady_count() {
        let all = |n: usize| (0..n).map(Some).collect::<Vec<_>>();
        assert_eq!(page_buttons(0, 1), all(1));
        assert_eq!(page_buttons(4, 7), all(7));

        let s = Some;
        assert_eq!(
            page_buttons(0, 10),
            vec![s(0), s(1), s(2), s(3), s(4), None, s(9)]
        );
        assert_eq!(
            page_buttons(3, 10),
            vec![s(0), s(1), s(2), s(3), s(4), None, s(9)]
        );
        assert_eq!(
            page_buttons(4, 10),
            vec![s(0), None, s(3), s(4), s(5), None, s(9)]
        );
        assert_eq!(
            page_buttons(6, 10),
            vec![s(0), None, s(5), s(6), s(7), s(8), s(9)]
        );
        assert_eq!(
            page_buttons(9, 10),
            vec![s(0), None, s(5), s(6), s(7), s(8), s(9)]
        );
        for pages in 8..40 {
            for current in 0..pages {
                let b = page_buttons(current, pages);
                assert_eq!(b.len(), 7);
                assert!(b.contains(&Some(current)), "{current} of {pages}");
                assert_eq!(b[0], Some(0));
                assert_eq!(b[6], Some(pages - 1));
            }
        }
    }

    #[test]
    fn columns_follow_the_width() {
        assert_eq!(columns(150.0), 1);
        assert_eq!(columns(440.0), 2);
        assert_eq!(columns(700.0), 3);
        assert_eq!(columns(2000.0), 4);
        // Whole rows at every column count.
        for c in 1..=MOST_COLUMNS {
            assert_eq!(PAGE_SIZE % c, 0);
        }
    }

    #[test]
    fn recordings_sort_and_filter() {
        let e = vec![
            entry("recording_2026-11-03_19-09-11", 1_700, 300),
            entry("recording_2026-11-04_07-57-24", 1_100, 100),
            entry("holiday", 5_000, 200),
        ];
        assert_eq!(arrange(&e, "", Sort::Newest), vec![1, 2, 0]);
        assert_eq!(arrange(&e, "", Sort::Oldest), vec![0, 2, 1]);
        assert_eq!(arrange(&e, "", Sort::Largest), vec![2, 0, 1]);
        // By the name shown, and by the file's own name.
        assert_eq!(arrange(&e, "3 nov", Sort::Newest), vec![0]);
        assert_eq!(arrange(&e, "2026-11-04", Sort::Newest), vec![1]);
        assert_eq!(arrange(&e, "  HOLI ", Sort::Newest), vec![2]);
        assert!(arrange(&e, "nothing", Sort::Newest).is_empty());
    }
}
