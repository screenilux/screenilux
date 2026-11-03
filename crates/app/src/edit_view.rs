//! The Edit tab: shape a demo without a track for every kind of thing.
//!
//! Playing and editing are one thing here, not two. The tab is the player —
//! its picture and its controls — with a side panel that carries the text
//! read off the screen and the effects on one set of tabs. So there is one
//! recording open, one decoder, one clock and one picture, and no second
//! window to arrange. The editor never opens a recording of its own; it
//! follows whatever the player has.
//!
//! Two surfaces, split by what a tool needs to know. Tools that need a *where*
//! — zoom, hide, point at — are placed by clicking the picture, which is the
//! only place that question has an answer. Tools that need a *when* — cut,
//! speed up, mute, show a screen — come from dragging the timeline. Effects
//! then live on one track and in a list that opens where it sits, so an edit
//! is changed in the same place it is read.
//!
//! An edit's length comes from the recording wherever it can: a zoom holds
//! until the pointer leaves the area it was placed on. Nothing here renders
//! anything permanently; the edit list is saved beside the recording and the
//! picture is drawn through it while playing.
//!
//! One limit worth knowing: a zoom cannot follow the pointer until the
//! recorder keeps a pointer track, so "follow" currently holds on the area
//! it was placed on. A spatial edit does know which screen it belongs to:
//! the click that placed it says which picture it landed on.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::mpsc::{Receiver, channel};

use eframe::egui;
use screenilux_core::edit::{
    Area, Backdrop, EditList, Effect, MIN_SPAN, Place, Problem, Tool, trim_zero,
};
use screenilux_core::session::{MANIFEST_NAME, Manifest};
use screenilux_export::{Gif, Written};
use screenilux_player::SharedClock;

use crate::player_view::{Exporting, PlayerView};
use crate::theme::{self, icon};

/// How tall the timeline is. It carries a ruler, the effects as blocks and
/// the playhead, and its edges are grabbed to re-time a effect, so it wants
/// more than a scrubber's few pixels.
const TRACK_H: f32 = 64.0;

/// The timeline with the room around it: what the panel under the picture
/// asks for.
pub const TIMELINE_H: f32 = TRACK_H + 26.0;

/// The least list worth scrolling, when the panel is short.
const LIST_MIN: f32 = 120.0;

/// The effect menu: wide enough for a name and what it does on one line
/// each, and tall enough to show six without scrolling.
const MENU_W: f32 = 420.0;
const MENU_LIST_H: f32 = 330.0;
/// A drag shorter than this is a click: move the playhead, do not select.
const TAP: f64 = 0.35;
/// How close to a block's edge counts as grabbing that edge.
const GRIP: f32 = 7.0;

/// What the pointer is doing on the timeline.
enum Grab {
    None,
    /// Dragging out a new selection.
    Selecting {
        from: f64,
    },
    /// Moving a whole effect, holding where it was grabbed.
    Moving {
        id: u32,
        offset: f64,
    },
    /// Dragging one end of a effect.
    Edge {
        id: u32,
        start_side: bool,
    },
}

/// A GIF on its way to disk, written off the interface's thread so the
/// picture keeps playing while it is made.
struct Writing {
    done: Arc<AtomicU32>,
    result: Receiver<Result<Written, String>>,
    to: PathBuf,
}

/// A menu waiting to be answered.
enum Asking {
    /// Placed on the picture: spatial tools, at a point on a named screen.
    Spot {
        at: f64,
        area: Area,
        screen: Option<String>,
    },
    /// Dragged on the timeline: tools that change time.
    Span { from: f64, to: f64 },
}

pub struct EditView {
    loaded: Option<Loaded>,
    error: Option<String>,
    grab: Grab,
    selection: Option<(f64, f64)>,
    asking: Option<Asking>,
    /// Which effect is open in the list, which is also the one highlighted on
    /// the track: selecting in one place selects in the other.
    opened: Option<u32>,
    saved: bool,
    /// Set by the Save button, acted on once the borrows are released.
    wants_save: bool,
    /// Where the timeline was drawn last frame, for the tour to point at.
    track_at: Option<egui::Rect>,
    /// A GIF being written: how far along, and what came of it.
    writing: Option<Writing>,
    wrote: Option<Result<String, String>>,
    /// What is typed into the effect menu's search box.
    find: String,
    /// Whether that box has been given focus since the menu opened.
    found_focus: bool,
}

struct Loaded {
    path: PathBuf,
    manifest: Manifest,
    edits: EditList,
    duration: f64,
    /// Screen files in display order, for the screen-override menu.
    screens: Vec<String>,
}

impl Default for EditView {
    fn default() -> Self {
        Self::new()
    }
}

impl EditView {
    pub fn new() -> Self {
        Self {
            loaded: None,
            error: None,
            grab: Grab::None,
            selection: None,
            asking: None,
            opened: None,
            saved: true,
            wants_save: false,
            track_at: None,
            writing: None,
            wrote: None,
            find: String::new(),
            found_focus: false,
        }
    }

    /// Follow whatever the player has open. The editor never opens a
    /// recording of its own: one decoder, one clock, one picture, and
    /// nothing to keep in step.
    /// Hand the player whatever is live at the clock's position, so the
    /// picture shows the effects instead of just listing them.
    pub fn show_live(&self, player: &mut PlayerView) {
        let at = player.clock().map_or(0.0, |c| c.position());
        let live = self
            .loaded
            .as_ref()
            .map(|l| l.edits.active_at(at).cloned().collect())
            .unwrap_or_default();
        player.set_live(live);
    }

    pub fn follow(&mut self, player: &PlayerView) {
        let path = player.shareable();
        if self.loaded.as_ref().map(|l| &l.path) == path.as_ref() {
            return;
        }
        match path {
            None => {
                self.loaded = None;
                self.opened = None;
                self.selection = None;
                self.grab = Grab::None;
                self.asking = None;
            }
            Some(path) => self.adopt(&path, player),
        }
    }

    /// Read the edits saved beside `path`, if there are any. A recording
    /// with none is simply one nobody has edited yet.
    fn adopt(&mut self, path: &Path, player: &PlayerView) {
        self.error = None;
        self.opened = None;
        self.selection = None;
        self.grab = Grab::None;
        self.asking = None;
        let duration = player
            .clock()
            .map_or(MIN_SPAN, |c| c.duration())
            .max(MIN_SPAN);
        let screens = player.screen_names();
        let manifest = read_manifest(path)
            .unwrap_or_else(|| Manifest::new(String::new(), duration, 30, String::new(), false));
        let edits = manifest.edits.clone().unwrap_or_default();
        self.loaded = Some(Loaded {
            path: path.to_path_buf(),
            manifest,
            edits,
            duration,
            screens,
        });
        self.saved = true;
    }

    /// Write the edit list back beside the recording.
    pub fn save(&mut self) {
        let Some(l) = self.loaded.as_mut() else {
            return;
        };
        l.manifest.edits = if l.edits.is_empty() {
            None
        } else {
            Some(l.edits.clone())
        };
        match write_manifest(&l.path, &l.manifest) {
            Ok(()) => {
                self.saved = true;
                self.error = None;
            }
            Err(e) => self.error = Some(e),
        }
    }

    // -- the side panel ----------------------------------------------------

    /// The panel beside the picture: the text read off the screen, and the
    /// effects, on one set of tabs. Both are about the recording that is
    /// open, so they share an edge rather than each taking one.
    pub fn side_panel(&mut self, ui: &mut egui::Ui, player: &mut PlayerView) {
        let p = theme::palette(ui);

        // A click on the paused picture is an edit placed where it landed,
        // and it is the editor being asked for, so bring it forward. Taken
        // before the clock is borrowed, so the player is free.
        if let Some(spot) = player.take_placed() {
            let at = player.clock().map_or(0.0, |c| c.position());
            // A click means "around here", not a pixel: give it a region.
            let area = Area::new(spot.x - 0.13, spot.y - 0.11, 0.26, 0.22);
            // The screen it was clicked on, so the edit belongs to that
            // picture rather than to whichever one it lands over later.
            let screen = self
                .loaded
                .as_ref()
                .and_then(|l| l.screens.get(spot.track).cloned());
            self.asking = Some(Asking::Spot { at, area, screen });
        }

        self.panel(ui, &p, player);
    }

    /// Where the timeline is on screen, for the tour to point at.
    pub fn track_at(&self) -> Option<egui::Rect> {
        self.track_at
    }

    /// The panel: what to save, and the effects on the recording.
    fn panel(&mut self, ui: &mut egui::Ui, p: &theme::Palette, player: &mut PlayerView) {
        if let Some(e) = self.error.clone() {
            theme::notice(ui, e);
            ui.add_space(6.0);
        }
        if player.clock().is_none() {
            self.empty(ui, p);
            return;
        }

        self.save_row(ui, p);
        ui.add_space(14.0);
        let Some(clock) = player.clock() else { return };
        let rest = ui.available_height().max(LIST_MIN);
        self.list(ui, p, clock, rest);
    }

    /// Keep up with a GIF being written and tell the player how it is
    /// going, since the button that started it lives up in the title row
    /// beside the link.
    pub fn poll_export(&mut self, player: &mut PlayerView) {
        if let Some(w) = &self.writing {
            match w.result.try_recv() {
                Ok(Ok(done)) => {
                    self.wrote = Some(Ok(format!(
                        "{} · {}×{} · {}",
                        w.to.file_name().unwrap_or_default().to_string_lossy(),
                        done.width,
                        done.height,
                        weight(done.bytes),
                    )));
                    self.writing = None;
                }
                Ok(Err(e)) => {
                    self.wrote = Some(Err(e));
                    self.writing = None;
                }
                Err(std::sync::mpsc::TryRecvError::Disconnected) => self.writing = None,
                Err(std::sync::mpsc::TryRecvError::Empty) => {}
            }
        }
        let state = match (&self.writing, &self.wrote) {
            (Some(w), _) => Exporting::Busy(w.done.load(Ordering::Relaxed) as f32 / 1000.0),
            (None, Some(Ok(text))) => Exporting::Done(text.clone()),
            (None, Some(Err(e))) => Exporting::Failed(e.clone()),
            (None, None) => Exporting::Idle,
        };
        player.set_export(state);
    }

    /// Start writing the GIF, off this thread.
    pub fn start_gif(&mut self, player: &mut PlayerView) {
        let (Some(video), Some(l)) = (player.track_file(0), self.loaded.as_ref()) else {
            return;
        };
        // Beside the recording, not beside whatever the package unpacked
        // to: the scratch folder is cleared when the recording is closed.
        let to = screenilux_export::beside(&l.path);
        let edits = l.edits.clone();
        let done = Arc::new(AtomicU32::new(0));
        let (tx, rx) = channel();
        let ticker = Arc::clone(&done);
        let out = to.clone();
        std::thread::spawn(move || {
            let result = screenilux_export::gif(&video, &out, &edits, Gif::default(), |at| {
                ticker.store((at * 1000.0) as u32, Ordering::Relaxed);
            })
            .map_err(|e| e.to_string());
            let _ = tx.send(result);
        });
        self.wrote = None;
        self.writing = Some(Writing {
            done,
            result: rx,
            to,
        });
    }

    /// The timeline, under the picture and as wide as it is. Click to move
    /// the playhead, drag to mark a stretch to cut or speed up, and drag a
    /// block or either of its edges to re-time it — none of which fits in
    /// the width of a side panel.
    pub fn timeline(&mut self, ui: &mut egui::Ui, ctx: &egui::Context, player: &mut PlayerView) {
        let p = theme::palette(ui);
        let Some(clock) = player.clock() else {
            return;
        };
        self.track(ui, &p, clock);
        // Drawn after both panels, so a stretch marked here is answered on
        // the same frame it was marked.
        self.menu(ctx, &p);
        if std::mem::take(&mut self.wants_save) {
            self.save();
        }
    }

    fn empty(&mut self, ui: &mut egui::Ui, p: &theme::Palette) {
        ui.vertical_centered(|ui| {
            ui.add_space(6.0);
            theme::hint(ui, "Open a recording in Recordings to play and edit it.");
        });
        let _ = p;
    }

    /// Save, and anything wrong with the edits as they stand.
    fn save_row(&mut self, ui: &mut egui::Ui, p: &theme::Palette) {
        let Some(l) = self.loaded.as_ref() else {
            return;
        };
        let out = l.edits.output_duration(l.duration);
        let saved_time = l.edits.time_saved(l.duration);
        let problems: Vec<Problem> = l.edits.problems(l.duration);

        let n = l.edits.effects.len();
        let count = match n {
            0 => "Nothing yet".to_string(),
            1 => "1 effect".to_string(),
            n => format!("{n} effects"),
        };

        ui.horizontal(|ui| {
            ui.vertical(|ui| {
                ui.spacing_mut().item_spacing.y = 1.0;
                ui.label(
                    egui::RichText::new("Effects")
                        .font(egui::FontId::new(15.0, theme::bold()))
                        .color(p.text),
                );
                let under = if saved_time > 0.05 {
                    format!("{count} · exports as {}", stamp(out))
                } else {
                    count.clone()
                };
                ui.label(egui::RichText::new(under).size(11.5).color(p.weak));
            });
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let label = if self.saved { "Saved" } else { "Save" };
                if ui.add_enabled(!self.saved, theme::outline(label)).clicked() {
                    self.wants_save = true;
                }
            });
        });

        for problem in problems {
            let text = match problem {
                Problem::Overlap { .. } => "Two edits change the same stretch of time.",
                Problem::PastEnd { .. } => "An edit runs past the end of the recording.",
                Problem::RemovesEverything => "These edits would remove the whole recording.",
            };
            theme::hint(ui, text);
        }

        // A rule under the header, so the panel has a head and a body
        // rather than a stack of things that happen to start at the top.
        ui.add_space(12.0);
        let line = ui.available_rect_before_wrap();
        ui.painter().line_segment(
            [
                egui::pos2(line.left(), line.top()),
                egui::pos2(line.right(), line.top()),
            ],
            egui::Stroke::new(1.0, p.stroke),
        );
    }

    /// One track: effects as blocks, with what happened kept faint behind.
    fn track(&mut self, ui: &mut egui::Ui, p: &theme::Palette, clock: &SharedClock) {
        let Some(l) = self.loaded.as_mut() else {
            return;
        };
        let width = ui.available_width();
        let (rect, response) =
            ui.allocate_exact_size(egui::vec2(width, TRACK_H), egui::Sense::click_and_drag());
        self.track_at = Some(rect);
        let painter = ui.painter_at(rect);
        // A groove in the card it sits in, not a box on top of it: one tone
        // down from the card, with no border of its own.
        painter.rect_filled(
            rect,
            egui::CornerRadius::same(9),
            theme::blend(p.stage, p.text, 0.07),
        );

        let dur = l.duration;
        let x_of = |t: f64| rect.left() + (t / dur) as f32 * rect.width();
        let t_of = |x: f32| (((x - rect.left()) / rect.width()) as f64 * dur).clamp(0.0, dur);

        // a ruler, every ten seconds
        let mut mark = 0.0;
        while mark <= dur {
            let x = x_of(mark);
            painter.line_segment(
                [
                    egui::pos2(x, rect.top() + 3.0),
                    egui::pos2(x, rect.top() + 9.0),
                ],
                egui::Stroke::new(1.0, theme::blend(p.stroke, p.surface, 0.3)),
            );
            painter.text(
                egui::pos2(x + 5.0, rect.top() + 2.0),
                egui::Align2::LEFT_TOP,
                stamp_short(mark),
                egui::FontId::new(9.5, theme::strong()),
                p.weak,
            );
            mark += 10.0;
        }

        // selection
        if let Some((a, b)) = self.selection {
            let r = egui::Rect::from_min_max(
                egui::pos2(x_of(a.min(b)), rect.top() + 13.0),
                egui::pos2(x_of(a.max(b)), rect.bottom() - 4.0),
            );
            painter.rect_filled(
                r,
                egui::CornerRadius::same(4),
                theme::blend(p.signal, p.surface, 0.75),
            );
            painter.rect_stroke(
                r,
                egui::CornerRadius::same(4),
                egui::Stroke::new(1.0, p.signal),
                egui::StrokeKind::Inside,
            );
        }

        // effects
        let mut hit: Option<(u32, bool, bool)> = None; // id, on-left-edge, on-right-edge
        let pointer = response.hover_pos().or(response.interact_pointer_pos());
        for m in &l.edits.effects {
            let r = egui::Rect::from_min_max(
                egui::pos2(x_of(m.start), rect.top() + 13.0),
                egui::pos2(x_of(m.end).max(x_of(m.start) + 6.0), rect.bottom() - 4.0),
            );
            let tint = tool_colour(&m.tool, p);
            let on = self.opened == Some(m.id);
            let round = egui::CornerRadius::same(7);
            painter.rect_filled(r, round, theme::blend(tint, p.surface, 0.68));
            // A bar of full colour down the leading edge, so a block says
            // which effect it is even when it is too narrow for its name.
            let edge = egui::Rect::from_min_size(r.min, egui::vec2(3.0, r.height()));
            painter.rect_filled(edge, egui::CornerRadius::same(2), tint);
            painter.rect_stroke(
                r,
                round,
                egui::Stroke::new(if on { 1.6 } else { 1.0 }, tint),
                egui::StrokeKind::Inside,
            );
            if r.width() > 52.0 {
                painter.text(
                    r.left_center() + egui::vec2(11.0, 0.0),
                    egui::Align2::LEFT_CENTER,
                    m.tool.label(),
                    egui::FontId::new(11.0, theme::strong()),
                    p.text,
                );
            }
            if let Some(pos) = pointer
                && r.expand2(egui::vec2(GRIP, 0.0)).contains(pos)
            {
                hit = Some((
                    m.id,
                    (pos.x - r.left()).abs() <= GRIP,
                    (pos.x - r.right()).abs() <= GRIP,
                ));
            }
        }

        // playhead
        let hx = x_of(clock.position());
        painter.line_segment(
            [egui::pos2(hx, rect.top()), egui::pos2(hx, rect.bottom())],
            egui::Stroke::new(2.0, p.text),
        );

        // dragging
        if response.drag_started()
            && let Some(pos) = response.interact_pointer_pos()
        {
            let t = t_of(pos.x);
            self.grab = match hit {
                Some((id, true, _)) => Grab::Edge {
                    id,
                    start_side: true,
                },
                Some((id, _, true)) => Grab::Edge {
                    id,
                    start_side: false,
                },
                Some((id, _, _)) => {
                    let m = l.edits.get(id).map(|m| m.start).unwrap_or(t);
                    self.opened = Some(id);
                    Grab::Moving { id, offset: t - m }
                }
                None => {
                    self.selection = Some((t, t));
                    Grab::Selecting { from: t }
                }
            };
            if let Some((id, _, _)) = hit {
                self.opened = Some(id);
            }
        }
        if response.dragged()
            && let Some(pos) = response.interact_pointer_pos()
        {
            let t = t_of(pos.x);
            match &self.grab {
                Grab::Selecting { from } => self.selection = Some((*from, t)),
                Grab::Moving { id, offset } => {
                    let id = *id;
                    let offset = *offset;
                    let span = l.edits.get(id).map(|m| m.span()).unwrap_or(1.0);
                    let start = (t - offset).clamp(0.0, (dur - span).max(0.0));
                    l.edits.update(id, |m| {
                        m.start = start;
                        m.end = start + span;
                    });
                    self.saved = false;
                }
                Grab::Edge { id, start_side } => {
                    let (id, left) = (*id, *start_side);
                    l.edits.update(id, |m| {
                        if left {
                            m.start = t.min(m.end - MIN_SPAN).max(0.0);
                        } else {
                            m.end = t.max(m.start + MIN_SPAN).min(dur);
                        }
                    });
                    self.saved = false;
                }
                Grab::None => {}
            }
        }
        if response.drag_stopped() {
            if let (Grab::Selecting { from }, Some((a, b))) = (&self.grab, self.selection) {
                let _ = from;
                let (lo, hi) = (a.min(b), a.max(b));
                if hi - lo < TAP {
                    self.selection = None;
                    clock.seek(lo);
                } else {
                    self.asking = Some(Asking::Span { from: lo, to: hi });
                }
            }
            self.grab = Grab::None;
        }
        if response.clicked()
            && matches!(self.grab, Grab::None)
            && let Some(pos) = response.interact_pointer_pos()
        {
            match hit {
                Some((id, _, _)) => self.opened = Some(id),
                None => {
                    self.selection = None;
                    clock.seek(t_of(pos.x));
                }
            }
        }
    }

    /// Effects, each opening where it sits.
    fn list(&mut self, ui: &mut egui::Ui, p: &theme::Palette, clock: &SharedClock, tall: f32) {
        // Read what the list needs, then let go: the scroll closure needs the
        // whole view, so nothing may still be borrowed out of it.
        let (at, duration, effects) = {
            let Some(l) = self.loaded.as_ref() else {
                return;
            };
            (clock.position(), l.duration, l.edits.effects.clone())
        };
        if effects.is_empty() {
            theme::hint(
                ui,
                "Click the picture to zoom, hide or point at something. Drag the track to cut or speed up a stretch.",
            );
            return;
        }

        let mut remove: Option<u32> = None;
        let mut seek: Option<f64> = None;

        egui::ScrollArea::vertical()
            .max_height(tall)
            .show(ui, |ui| {
                for m in &effects {
                    let id = m.id;
                    let live = m.covers(at);
                    let open = self.opened == Some(id);
                    let tint = tool_colour(&m.tool, p);

                    egui::Frame::NONE
                        .fill(if open {
                            p.base
                        } else if live {
                            theme::blend(tint, p.surface, 0.86)
                        } else {
                            p.surface
                        })
                        .stroke(if open {
                            egui::Stroke::new(1.5, tint)
                        } else {
                            egui::Stroke::NONE
                        })
                        .corner_radius(12)
                        .inner_margin(egui::Margin::symmetric(12, 10))
                        .show(ui, |ui| {
                            let head = ui.horizontal(|ui| {
                                let (dot, _) = ui.allocate_exact_size(
                                    egui::vec2(8.0, 8.0),
                                    egui::Sense::hover(),
                                );
                                ui.painter().circle_filled(dot.center(), 4.0, tint);
                                ui.label(
                                    egui::RichText::new(m.tool.label())
                                        .family(theme::strong())
                                        .color(p.text),
                                );
                                ui.with_layout(
                                    egui::Layout::right_to_left(egui::Align::Center),
                                    |ui| {
                                        ui.label(
                                            egui::RichText::new(format!(
                                                "{} → {}",
                                                stamp(m.start),
                                                stamp(m.end)
                                            ))
                                            .monospace()
                                            .size(11.0)
                                            .color(p.weak),
                                        );
                                    },
                                );
                            });
                            if head.response.interact(egui::Sense::click()).clicked() {
                                self.opened = if open { None } else { Some(id) };
                                seek = Some(m.start);
                            }
                            if open {
                                ui.add_space(6.0);
                                self.editor(ui, p, m, duration, &mut remove, &mut seek);
                            }
                        });
                    ui.add_space(5.0);
                }
            });

        let Some(l) = self.loaded.as_mut() else {
            return;
        };
        if let Some(t) = seek {
            clock.seek(t);
        }
        if let Some(id) = remove {
            l.edits.remove(id);
            if self.opened == Some(id) {
                self.opened = None;
            }
            self.saved = false;
        }
    }

    /// The fields for one effect, by what that tool actually has.
    #[allow(clippy::too_many_arguments)]
    fn editor(
        &mut self,
        ui: &mut egui::Ui,
        p: &theme::Palette,
        m: &Effect,
        duration: f64,
        remove: &mut Option<u32>,
        seek: &mut Option<f64>,
    ) {
        let Some(l) = self.loaded.as_mut() else {
            return;
        };
        let id = m.id;
        let mut start = m.start;
        let mut end = m.end;

        ui.horizontal(|ui| {
            ui.label(egui::RichText::new("Starts").size(11.0).color(p.weak));
            if ui
                .add(
                    egui::DragValue::new(&mut start)
                        .speed(0.05)
                        .range(0.0..=duration)
                        .fixed_decimals(2)
                        .suffix("s"),
                )
                .changed()
            {
                l.edits
                    .update(id, |m| m.start = start.min(m.end - MIN_SPAN));
                self.saved = false;
            }
            ui.add_space(10.0);
            ui.label(egui::RichText::new("Ends").size(11.0).color(p.weak));
            if ui
                .add(
                    egui::DragValue::new(&mut end)
                        .speed(0.05)
                        .range(0.0..=duration)
                        .fixed_decimals(2)
                        .suffix("s"),
                )
                .changed()
            {
                l.edits.update(id, |m| m.end = end.max(m.start + MIN_SPAN));
                self.saved = false;
            }
        });

        match &m.tool {
            Tool::Zoom {
                area,
                follow,
                scale,
            } => {
                let (area, mut follow, mut scale) = (*area, *follow, *scale);
                if dial(ui, p, "Closeness", &mut scale, 1.4..=3.2, |v| {
                    format!("×{v:.1}")
                }) {
                    l.edits.update(id, |m| {
                        m.tool = Tool::Zoom {
                            area,
                            follow,
                            scale,
                        };
                    });
                    self.saved = false;
                }
                // Following needs a note of where the pointer was, which
                // the recorder does not keep yet. The switch is shown
                // rather than hidden, because the choice is saved and will
                // start working on recordings made after it does; but it
                // says so instead of looking like it is already on.
                ui.add_enabled_ui(false, |ui| {
                    if theme::setting(ui, &mut follow, "Follow the pointer").changed() {
                        l.edits.update(id, |m| {
                            m.tool = Tool::Zoom {
                                area,
                                follow,
                                scale,
                            };
                        });
                        self.saved = false;
                    }
                })
                .response
                .on_hover_text(
                    "Not yet: following needs the recorder to keep track of where the                      pointer was, which it does not do. The zoom holds on this area.",
                );
            }
            Tool::Callout { area, text } => {
                let area = *area;
                let mut text = text.clone();
                if ui.text_edit_singleline(&mut text).changed() {
                    l.edits.update(id, |m| {
                        m.tool = Tool::Callout {
                            area,
                            text: text.clone(),
                        };
                    });
                    self.saved = false;
                }
            }
            Tool::Blur { area, strength } => {
                let area = *area;
                let mut strength = *strength;
                if dial(ui, p, "Softness", &mut strength, 0.2..=1.0, percent) {
                    l.edits
                        .update(id, |m| m.tool = Tool::Blur { area, strength });
                    self.saved = false;
                }
            }
            Tool::Spotlight { area, dim } => {
                let area = *area;
                let mut dim = *dim;
                if dial(ui, p, "Dim the rest", &mut dim, 0.1..=0.9, percent) {
                    l.edits
                        .update(id, |m| m.tool = Tool::Spotlight { area, dim });
                    self.saved = false;
                }
            }
            Tool::Caption { text, place } => {
                let mut text = text.clone();
                let mut place = *place;
                if ui.text_edit_singleline(&mut text).changed() {
                    let (t, pl) = (text.clone(), place);
                    l.edits.update(id, |m| {
                        m.tool = Tool::Caption {
                            text: t.clone(),
                            place: pl,
                        };
                    });
                    self.saved = false;
                }
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new("Sits").size(11.0).color(p.weak));
                    for (option, label) in [
                        (Place::Bottom, "Bottom"),
                        (Place::Centre, "Middle"),
                        (Place::Top, "Top"),
                    ] {
                        if ui.selectable_label(place == option, label).clicked() {
                            place = option;
                            let t = text.clone();
                            l.edits.update(id, |m| {
                                m.tool = Tool::Caption {
                                    text: t.clone(),
                                    place,
                                };
                            });
                            self.saved = false;
                        }
                    }
                });
            }
            Tool::Backdrop { pad, style } => {
                let mut pad = *pad;
                let mut style = *style;
                if dial(ui, p, "Inset", &mut pad, 0.0..=0.2, percent) {
                    l.edits
                        .update(id, |m| m.tool = Tool::Backdrop { pad, style });
                    self.saved = false;
                }
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new("Behind").size(11.0).color(p.weak));
                    for (option, label) in [
                        (Backdrop::Gradient, "Gradient"),
                        (Backdrop::Solid, "Solid"),
                        (Backdrop::Blurred, "Blurred"),
                    ] {
                        if ui.selectable_label(style == option, label).clicked() {
                            style = option;
                            l.edits
                                .update(id, |m| m.tool = Tool::Backdrop { pad, style });
                            self.saved = false;
                        }
                    }
                });
            }
            Tool::Speed { rate } => {
                let mut rate = *rate;
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new("Rate").size(11.0).color(p.weak));
                    for option in [2.0_f32, 4.0, 8.0, 16.0] {
                        let on = (rate - option).abs() < 0.01;
                        if ui.selectable_label(on, format!("{option:.0}×")).clicked() {
                            rate = option;
                            l.edits.update(id, |m| m.tool = Tool::Speed { rate });
                            self.saved = false;
                        }
                    }
                });
            }
            Tool::Screen { file } => {
                let current = file.clone();
                let options = l.screens.clone();
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new("Show").size(11.0).color(p.weak));
                    for (i, name) in options.iter().enumerate() {
                        let on = *name == current;
                        if ui
                            .selectable_label(on, format!("Screen {}", i + 1))
                            .clicked()
                        {
                            let file = name.clone();
                            l.edits
                                .update(id, |m| m.tool = Tool::Screen { file: file.clone() });
                            self.saved = false;
                        }
                    }
                });
            }
            _ => {}
        }

        ui.add_space(4.0);
        ui.horizontal(|ui| {
            if ui.add(theme::outline("Jump here")).clicked() {
                *seek = Some(m.start);
            }
            if ui.add(theme::outline("Remove")).clicked() {
                *remove = Some(id);
            }
        });
    }

    /// The menu a click or a drag asked for.
    /// Everything that can be put here, searched rather than scrolled. The
    /// list grows every time an effect is added, and a name you half
    /// remember is faster to type than to find.
    fn menu(&mut self, ctx: &egui::Context, p: &theme::Palette) {
        let Some(asking) = self.asking.as_ref() else {
            return;
        };
        let Some(l) = self.loaded.as_ref() else {
            self.asking = None;
            return;
        };

        let (title, choices) = match asking {
            Asking::Spot { at, area, screen } => (
                format!("Here, at {}", stamp(*at)),
                spot_choices(*at, *area, screen.clone(), l.duration),
            ),
            Asking::Span { from, to } => (
                format!("{} → {}", stamp(*from), stamp(*to)),
                span_choices(*from, *to, l.duration, &l.screens),
            ),
        };

        let query = self.find.trim().to_lowercase();
        let shown: Vec<&Choice> = choices
            .iter()
            .filter(|c| {
                query.is_empty()
                    || c.name.to_lowercase().contains(&query)
                    || c.hint.to_lowercase().contains(&query)
            })
            .collect();

        let mut close = false;
        let mut chosen: Option<usize> = None;

        egui::Window::new(title)
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
            .show(ctx, |ui| {
                ui.set_width(MENU_W);
                let field = ui.add(
                    egui::TextEdit::singleline(&mut self.find)
                        .hint_text(format!("{}  Search effects", icon::SEARCH))
                        .margin(egui::Margin::symmetric(12, 8))
                        .desired_width(f32::INFINITY),
                );
                // The menu opens ready to be typed into, so reaching for an
                // effect never means reaching for the mouse first.
                if !self.found_focus {
                    field.request_focus();
                    self.found_focus = true;
                }
                ui.add_space(6.0);

                if shown.is_empty() {
                    ui.add_space(6.0);
                    theme::hint(ui, "Nothing by that name.");
                    ui.add_space(6.0);
                }
                egui::ScrollArea::vertical()
                    .max_height(MENU_LIST_H)
                    .auto_shrink([false, true])
                    .show(ui, |ui| {
                        // The rows carry their own spacing, so the style's
                        // does not stack on top and push the last one out.
                        ui.spacing_mut().item_spacing.y = 0.0;
                        for (i, c) in shown.iter().enumerate() {
                            if item(ui, p, &c.name, &c.hint) {
                                chosen = Some(i);
                            }
                        }
                    });
                ui.add_space(4.0);
                ui.separator();

                // Enter takes the first match, which is what the typing was
                // narrowing towards.
                if ui.input(|i| i.key_pressed(egui::Key::Enter)) && !shown.is_empty() {
                    chosen = Some(0);
                }
                if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                    close = true;
                }
                ui.add_space(4.0);
                if ui.add(theme::outline("Cancel")).clicked() {
                    close = true;
                }
            });

        let picked = chosen.map(|i| shown[i].clone());
        drop(shown);
        if let Some(c) = picked
            && let Some(l) = self.loaded.as_mut()
        {
            let id = l.edits.add_on(c.start, c.end, c.tool, c.screen);
            self.opened = Some(id);
            self.saved = false;
            self.selection = None;
            close = true;
        }
        if close {
            self.asking = None;
            self.find.clear();
            self.found_focus = false;
        }
    }
}

/// One thing the menu offers: what it is called, what it does, and the
/// effect it would add.
#[derive(Clone)]
struct Choice {
    name: String,
    hint: String,
    start: f64,
    end: f64,
    tool: Tool,
    screen: Option<String>,
}

impl Choice {
    fn new(name: &str, hint: String, start: f64, end: f64, tool: Tool) -> Self {
        Self {
            name: name.to_string(),
            hint,
            start,
            end,
            tool,
            screen: None,
        }
    }
}

/// What can be put at a point on the picture: everything that needs a
/// *where*.
fn spot_choices(at: f64, area: Area, screen: Option<String>, duration: f64) -> Vec<Choice> {
    let till = |secs: f64| (at + secs).min(duration);
    let mut out = vec![
        Choice::new(
            "Zoom & follow the pointer",
            "Holds until the pointer leaves this area".into(),
            at,
            till(6.0),
            Tool::Zoom {
                area,
                follow: true,
                scale: 2.1,
            },
        ),
        Choice::new(
            "Zoom and hold still",
            "Stays on this area".into(),
            at,
            till(6.0),
            Tool::Zoom {
                area,
                follow: false,
                scale: 2.1,
            },
        ),
        Choice::new(
            "Spotlight this",
            "Keeps it lit and dims the rest".into(),
            at,
            till(5.0),
            Tool::Spotlight { area, dim: 0.55 },
        ),
        Choice::new(
            "Blur this region",
            "Softened, so the shape still reads".into(),
            at,
            till(8.0),
            Tool::Blur {
                area,
                strength: 0.6,
            },
        ),
        Choice::new(
            "Hide this region",
            "Covered while it is on screen".into(),
            at,
            till(8.0),
            Tool::Hide { area },
        ),
        Choice::new(
            "Point at this",
            "A label, with everything else dimmed".into(),
            at,
            till(4.0),
            Tool::Callout {
                area,
                text: "Look here".into(),
            },
        ),
    ];
    for c in &mut out {
        c.screen = screen.clone();
    }
    out
}

/// What can be done to a stretch of time: everything that needs a *when*.
fn span_choices(from: f64, to: f64, duration: f64, screens: &[String]) -> Vec<Choice> {
    let span = to - from;
    let at_end = from < 1.2 || to > duration - 1.2;
    let mut out = Vec::new();
    if at_end {
        out.push(Choice::new(
            "Trim this end",
            format!("Drops {span:.1}s"),
            from,
            to,
            Tool::Trim,
        ));
    }
    out.push(Choice::new(
        "Cut this out",
        format!("{span:.1}s removed"),
        from,
        to,
        Tool::Cut,
    ));
    for rate in [2.0_f32, 4.0, 8.0] {
        out.push(Choice::new(
            &format!("Speed up {}×", trim_zero(rate)),
            format!("{span:.1}s becomes {:.1}s", span / rate as f64),
            from,
            to,
            Tool::Speed { rate },
        ));
    }
    out.push(Choice::new(
        "Hold the frame",
        "Stays on one frame for this stretch".into(),
        from,
        to,
        Tool::Freeze,
    ));
    out.push(Choice::new(
        "Caption this",
        "Words along the bottom".into(),
        from,
        to,
        Tool::Caption {
            text: "Say something here".into(),
            place: Place::Bottom,
        },
    ));
    out.push(Choice::new(
        "Backdrop",
        "Insets the picture on a background".into(),
        from,
        to,
        Tool::Backdrop {
            pad: 0.06,
            style: Backdrop::Gradient,
        },
    ));
    out.push(Choice::new(
        "Mute this stretch",
        "The picture keeps playing".into(),
        from,
        to,
        Tool::Mute,
    ));
    out.push(Choice::new(
        "Mark every click",
        "A ring wherever a click was recorded".into(),
        from,
        to,
        Tool::MarkClicks,
    ));
    if screens.len() > 1 {
        out.push(Choice::new(
            "Show one screen",
            "Overrides what Screenilux picked".into(),
            from,
            to,
            Tool::Screen {
                file: screens.first().cloned().unwrap_or_default(),
            },
        ));
    }
    out
}

/// A labelled slider that shows where it is. egui's own puts a number box
/// beside the rail, which reads as a form field; this keeps the label, the
/// rail and the reading on one line and gives the rail the room it needs to
/// be worth dragging.
/// A file size someone can hold in their head.
fn weight(bytes: u64) -> String {
    const MB: f64 = 1024.0 * 1024.0;
    let mb = bytes as f64 / MB;
    if mb >= 1.0 {
        format!("{mb:.1} MB")
    } else {
        format!("{:.0} KB", bytes as f64 / 1024.0)
    }
}

fn percent(v: f32) -> String {
    format!("{:.0}%", v * 100.0)
}

fn dial(
    ui: &mut egui::Ui,
    p: &theme::Palette,
    label: &str,
    value: &mut f32,
    range: std::ops::RangeInclusive<f32>,
    read: impl Fn(f32) -> String,
) -> bool {
    let mut changed = false;
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new(label).size(11.0).color(p.weak));
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.label(
                egui::RichText::new(read(*value))
                    .family(theme::strong())
                    .size(11.5)
                    .color(p.text),
            );
            ui.spacing_mut().slider_width = (ui.available_width() - 10.0).max(60.0);
            changed = ui
                .add(egui::Slider::new(value, range).show_value(false))
                .changed();
        });
    });
    changed
}

/// One row of a chooser: a name, what it does, and the whole width of the
/// list as its target. It lights up under the pointer, so the list reads as
/// something to pick from rather than something to read.
fn item(ui: &mut egui::Ui, p: &theme::Palette, title: &str, sub: &str) -> bool {
    let width = ui.available_width();
    let height = 50.0;
    let (rect, response) = ui.allocate_exact_size(egui::vec2(width, height), egui::Sense::click());
    let on = response.hovered();
    if on {
        ui.painter()
            .rect_filled(rect, egui::CornerRadius::same(10), p.surface);
    }
    let text = rect.shrink2(egui::vec2(12.0, 8.0));
    ui.painter().text(
        text.left_top(),
        egui::Align2::LEFT_TOP,
        title,
        egui::FontId::new(14.0, theme::strong()),
        p.text,
    );
    ui.painter().text(
        text.left_bottom(),
        egui::Align2::LEFT_BOTTOM,
        sub,
        egui::FontId::proportional(11.5),
        p.weak,
    );
    if on {
        response
            .clone()
            .on_hover_cursor(egui::CursorIcon::PointingHand);
    }
    ui.add_space(2.0);
    response.clicked()
}

fn tool_colour(tool: &Tool, p: &theme::Palette) -> egui::Color32 {
    match tool {
        Tool::Zoom { .. } | Tool::MarkClicks => p.signal,
        Tool::Hide { .. } => p.action,
        Tool::Callout { .. } | Tool::Screen { .. } => p.soft,
        _ => p.weak,
    }
}

fn stamp(t: f64) -> String {
    let m = (t / 60.0).floor() as i64;
    format!("{m}:{:04.1}", t - m as f64 * 60.0)
}
fn stamp_short(t: f64) -> String {
    let m = (t / 60.0).floor() as i64;
    format!("{m}:{:02}", (t - m as f64 * 60.0).floor() as i64)
}

/// The manifest beside a recording, whether it is a folder or a package.
fn read_manifest(path: &Path) -> Option<Manifest> {
    let file = if path.is_dir() {
        path.join(MANIFEST_NAME)
    } else {
        path.parent()?.join(MANIFEST_NAME)
    };
    Manifest::from_json(&std::fs::read_to_string(file).ok()?).ok()
}

fn write_manifest(path: &Path, manifest: &Manifest) -> Result<(), String> {
    let file = if path.is_dir() {
        path.join(MANIFEST_NAME)
    } else {
        path.parent()
            .ok_or_else(|| "this recording has nowhere to save edits".to_string())?
            .join(MANIFEST_NAME)
    };
    let json = manifest.to_json().map_err(|e| e.to_string())?;
    std::fs::write(file, json).map_err(|e| format!("could not save edits: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clocks_read_the_way_the_editor_shows_them() {
        assert_eq!(stamp(0.0), "0:00.0");
        assert_eq!(stamp(9.25), "0:09.2");
        assert_eq!(stamp(75.5), "1:15.5");
        assert_eq!(stamp_short(0.0), "0:00");
        assert_eq!(stamp_short(90.0), "1:30");
    }

    #[test]
    fn a_fresh_view_has_nothing_open() {
        let v = EditView::new();
        assert!(v.loaded.is_none());
        assert!(v.saved, "nothing to save before anything is open");
        assert!(!v.wants_save);
    }
}
