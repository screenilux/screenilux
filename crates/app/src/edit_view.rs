//! The Edit tab: shape a demo without a track for every kind of thing.
//!
//! Two surfaces, split by what a tool needs to know. Tools that need a *where*
//! — zoom, hide, point at — are placed by clicking the picture, which is the
//! only place that question has an answer. Tools that need a *when* — cut,
//! speed up, mute, show a screen — come from dragging the timeline. Moments
//! then live on one track and in a list that opens where it sits, so an edit
//! is changed in the same place it is read.
//!
//! An edit's length comes from the recording wherever it can: a zoom holds
//! until the pointer leaves the area it was placed on. Nothing here renders
//! anything permanently; the edit list is saved beside the recording and the
//! picture is drawn through it while playing.
//!
//! Two limits worth knowing. A spatial edit does not yet name which screen it
//! belongs to, so on a multi-screen recording it applies wherever it lands;
//! that needs a screen on [`Tool`](screenilux_core::edit::Tool). And a zoom
//! cannot follow the pointer until the recorder keeps a pointer track, so
//! "follow" currently holds on the area it was placed on.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use eframe::egui;
use screenilux_core::edit::{Area, EditList, MIN_SPAN, Moment, Problem, Tool};
use screenilux_core::session::{MANIFEST_NAME, Manifest};
use screenilux_player::{Opened, Player, open};

use crate::theme;

/// How tall the timeline track is.
const TRACK_H: f32 = 58.0;

/// The transport row: the play button, the clocks and Save.
const TRANSPORT_H: f32 = 38.0;

/// The least picture worth clicking into, and the least list worth
/// scrolling. Below these the window has run out of room and stops.
const PICTURE_MIN: f32 = 150.0;
const LIST_MIN: f32 = 150.0;
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
    /// Moving a whole moment, holding where it was grabbed.
    Moving {
        id: u32,
        offset: f64,
    },
    /// Dragging one end of a moment.
    Edge {
        id: u32,
        start_side: bool,
    },
}

/// A menu waiting to be answered.
enum Asking {
    /// Placed on the picture: spatial tools, at a point on a screen.
    Spot { at: f64, area: Area },
    /// Dragged on the timeline: tools that change time.
    Span { from: f64, to: f64 },
}

pub struct EditView {
    loaded: Option<Loaded>,
    error: Option<String>,
    grab: Grab,
    selection: Option<(f64, f64)>,
    asking: Option<Asking>,
    /// Which moment is open in the list, which is also the one highlighted on
    /// the track: selecting in one place selects in the other.
    opened: Option<u32>,
    textures: Vec<Option<(u64, egui::TextureHandle)>>,
    saved: bool,
    /// Set by the Save button, acted on once the borrows are released.
    wants_save: bool,
}

struct Loaded {
    path: PathBuf,
    manifest: Manifest,
    player: Player,
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
            textures: Vec::new(),
            saved: true,
            wants_save: false,
        }
    }

    pub fn has_recording(&self) -> bool {
        self.loaded.is_some()
    }

    /// Open a recording to edit. Any edits already saved beside it come back.
    pub fn open(&mut self, path: &Path, ctx: &egui::Context) {
        self.error = None;
        self.opened = None;
        self.selection = None;
        self.textures.clear();
        match open(path) {
            Ok(opened) => self.adopt(path, opened, ctx),
            Err(e) => self.error = Some(e.to_string()),
        }
    }

    fn adopt(&mut self, path: &Path, opened: Opened, ctx: &egui::Context) {
        let Opened { recording, fps, .. } = opened;
        let duration = recording.duration.max(MIN_SPAN);
        let screens: Vec<String> = recording
            .tracks
            .iter()
            .map(|t| {
                t.path
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default()
            })
            .collect();
        let manifest = read_manifest(path).unwrap_or_else(|| {
            Manifest::new(
                String::new(),
                duration,
                fps.round() as u32,
                String::new(),
                false,
            )
        });
        let edits = manifest.edits.clone().unwrap_or_default();
        let repaint = {
            let ctx = ctx.clone();
            Arc::new(move || ctx.request_repaint())
        };
        let player = Player::start(recording, fps, repaint);
        player.clock().pause();
        self.textures = vec![None; screens.len()];
        self.loaded = Some(Loaded {
            path: path.to_path_buf(),
            manifest,
            player,
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

    // -- the tab -----------------------------------------------------------

    pub fn ui(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        let p = theme::palette(ui);
        if let Some(e) = self.error.clone() {
            theme::notice(ui, e);
            ui.add_space(8.0);
        }
        if self.loaded.is_none() {
            self.empty(ui, &p);
            return;
        }

        // Share the height out before drawing. The transport, the track and
        // a readable stretch of list are served first and the picture takes
        // what is left, so a short window loses picture instead of pushing
        // the moments off the bottom of it.
        let width = ui.available_width();
        let spare = ui.available_height() - (TRANSPORT_H + TRACK_H + LIST_MIN + 26.0);
        let tall = spare.clamp(PICTURE_MIN, width * 9.0 / 16.0);

        self.picture(ui, ctx, &p, tall);
        ui.add_space(8.0);
        self.transport(ui, &p);
        ui.add_space(8.0);
        self.track(ui, &p);
        ui.add_space(10.0);
        // Measured rather than budgeted: the transport grows when an edit
        // has a problem to report, and the list gives up the room.
        let rest = ui.available_height().max(LIST_MIN);
        self.list(ui, &p, rest);
        self.menu(ctx, &p);
        if std::mem::take(&mut self.wants_save) {
            self.save();
        }
    }

    fn empty(&mut self, ui: &mut egui::Ui, p: &theme::Palette) {
        ui.vertical_centered(|ui| {
            ui.add_space(26.0);
            ui.label(
                egui::RichText::new("Nothing open to edit")
                    .size(15.0)
                    .color(p.text),
            );
            ui.add_space(4.0);
            theme::hint(ui, "Pick a recording in Recordings, then come back here.");
            ui.add_space(26.0);
        });
    }

    /// The picture, and the click that places a spatial edit on it.
    fn picture(&mut self, ui: &mut egui::Ui, ctx: &egui::Context, p: &theme::Palette, tall: f32) {
        let Some(l) = self.loaded.as_mut() else {
            return;
        };
        let at = l.player.clock().position();

        let width = ui.available_width();
        let (band, response) =
            ui.allocate_exact_size(egui::vec2(width, tall), egui::Sense::click());
        ui.painter()
            .rect_filled(band, egui::CornerRadius::same(10), p.stage);
        // The picture itself keeps the shape it will be watched in, whatever
        // shape the window is, so an edit placed on it lands where the
        // viewer will see it. The stage shows through on either side.
        let rect = fit_inside(band, 16.0 / 9.0);

        // Which screen is on: an override wins, otherwise the first track.
        let forced = l.edits.forced_screen(at);
        let shown = forced
            .and_then(|f| l.screens.iter().position(|s| s == f))
            .unwrap_or(0);

        if let Some(feed) = l.player.feed(shown) {
            let serial = feed.serial();
            let stale = self
                .textures
                .get(shown)
                .and_then(|t| t.as_ref())
                .is_none_or(|(s, _)| *s != serial);
            if stale && let Some(frame) = feed.frame() {
                let image = egui::ColorImage::from_rgba_premultiplied(
                    [frame.width as usize, frame.height as usize],
                    &frame.rgba,
                );
                let slot = &mut self.textures[shown];
                match slot {
                    Some((s, handle)) => {
                        handle.set(image, egui::TextureOptions::LINEAR);
                        *s = serial;
                    }
                    None => {
                        *slot = Some((
                            serial,
                            ctx.load_texture(
                                format!("edit-{shown}"),
                                image,
                                egui::TextureOptions::LINEAR,
                            ),
                        ));
                    }
                }
            }
        }
        if let Some(Some((_, tex))) = self.textures.get(shown) {
            let inner = rect.shrink(1.0);
            ui.painter().image(
                tex.id(),
                inner,
                egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                egui::Color32::WHITE,
            );
        }

        // What is on this frame right now, drawn over it.
        let painter = ui.painter_at(rect);
        for m in l.edits.active_at(at) {
            let Some(area) = m.tool.area() else { continue };
            let r = egui::Rect::from_min_size(
                rect.min + egui::vec2(area.x * rect.width(), area.y * rect.height()),
                egui::vec2(area.w * rect.width(), area.h * rect.height()),
            );
            match &m.tool {
                Tool::Hide { .. } => {
                    painter.rect_filled(r, egui::CornerRadius::same(4), p.bar);
                    painter.text(
                        r.center(),
                        egui::Align2::CENTER_CENTER,
                        "hidden",
                        egui::FontId::proportional(11.0),
                        p.weak,
                    );
                }
                Tool::Callout { text, .. } => {
                    painter.rect_stroke(
                        r,
                        egui::CornerRadius::same(5),
                        egui::Stroke::new(2.0, p.signal),
                        egui::StrokeKind::Inside,
                    );
                    if !text.is_empty() {
                        painter.text(
                            r.left_top() + egui::vec2(0.0, -14.0),
                            egui::Align2::LEFT_CENTER,
                            text,
                            egui::FontId::proportional(12.0),
                            p.signal,
                        );
                    }
                }
                Tool::Zoom { .. } => {
                    painter.rect_stroke(
                        r,
                        egui::CornerRadius::same(5),
                        egui::Stroke::new(1.5, p.soft),
                        egui::StrokeKind::Inside,
                    );
                }
                _ => {}
            }
        }

        if response.clicked()
            && let Some(pos) = response.interact_pointer_pos()
            && rect.contains(pos)
        {
            let fx = ((pos.x - rect.left()) / rect.width()).clamp(0.0, 1.0);
            let fy = ((pos.y - rect.top()) / rect.height()).clamp(0.0, 1.0);
            // A click means "around here", not a pixel: give it a region.
            let area = Area::new(fx - 0.13, fy - 0.11, 0.26, 0.22);
            self.asking = Some(Asking::Spot { at, area });
        }
    }

    fn transport(&mut self, ui: &mut egui::Ui, p: &theme::Palette) {
        let Some(l) = self.loaded.as_mut() else {
            return;
        };
        let at = l.player.clock().position();
        let out = l.edits.output_duration(l.duration);
        let saved = l.edits.time_saved(l.duration);
        let playing = l.player.clock().is_playing();

        ui.horizontal(|ui| {
            let glyph = if playing { "❚❚" } else { "▶" };
            if theme::round_button(ui, glyph, 34.0, p.action, p.base, "Play").clicked() {
                l.player.clock().toggle();
            }
            ui.add_space(6.0);
            ui.label(
                egui::RichText::new(format!("{}  /  {}", clock(at), clock(l.duration)))
                    .family(theme::strong())
                    .color(p.text),
            );
            if saved > 0.05 {
                ui.add_space(8.0);
                ui.label(
                    egui::RichText::new(format!("exports as {}", clock(out)))
                        .color(p.signal)
                        .size(12.0),
                );
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let label = if self.saved { "Saved" } else { "Save edits" };
                if ui.add_enabled(!self.saved, theme::outline(label)).clicked() {
                    self.wants_save = true;
                }
            });
        });

        for problem in l.edits.problems(l.duration) {
            let text = match problem {
                Problem::Overlap { .. } => "Two edits change the same stretch of time.",
                Problem::PastEnd { .. } => "An edit runs past the end of the recording.",
                Problem::RemovesEverything => "These edits would remove the whole recording.",
            };
            theme::hint(ui, text);
        }
    }

    /// One track: moments as blocks, with what happened kept faint behind.
    fn track(&mut self, ui: &mut egui::Ui, p: &theme::Palette) {
        let Some(l) = self.loaded.as_mut() else {
            return;
        };
        let width = ui.available_width();
        let (rect, response) =
            ui.allocate_exact_size(egui::vec2(width, TRACK_H), egui::Sense::click_and_drag());
        let painter = ui.painter_at(rect);
        painter.rect_filled(rect, egui::CornerRadius::same(8), p.surface);
        painter.rect_stroke(
            rect,
            egui::CornerRadius::same(8),
            egui::Stroke::new(1.0, p.stroke),
            egui::StrokeKind::Inside,
        );

        let dur = l.duration;
        let x_of = |t: f64| rect.left() + (t / dur) as f32 * rect.width();
        let t_of = |x: f32| (((x - rect.left()) / rect.width()) as f64 * dur).clamp(0.0, dur);

        // a ruler, every ten seconds
        let mut mark = 0.0;
        while mark <= dur {
            let x = x_of(mark);
            painter.line_segment(
                [egui::pos2(x, rect.top()), egui::pos2(x, rect.top() + 9.0)],
                egui::Stroke::new(1.0, p.stroke),
            );
            painter.text(
                egui::pos2(x + 3.0, rect.top() + 2.0),
                egui::Align2::LEFT_TOP,
                clock_short(mark),
                egui::FontId::monospace(9.0),
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

        // moments
        let mut hit: Option<(u32, bool, bool)> = None; // id, on-left-edge, on-right-edge
        let pointer = response.hover_pos().or(response.interact_pointer_pos());
        for m in &l.edits.moments {
            let r = egui::Rect::from_min_max(
                egui::pos2(x_of(m.start), rect.top() + 13.0),
                egui::pos2(x_of(m.end).max(x_of(m.start) + 6.0), rect.bottom() - 4.0),
            );
            let tint = tool_colour(&m.tool, p);
            let on = self.opened == Some(m.id);
            painter.rect_filled(
                r,
                egui::CornerRadius::same(5),
                theme::blend(tint, p.surface, 0.72),
            );
            painter.rect_stroke(
                r,
                egui::CornerRadius::same(5),
                egui::Stroke::new(if on { 2.0 } else { 1.0 }, tint),
                egui::StrokeKind::Inside,
            );
            if r.width() > 46.0 {
                painter.text(
                    r.left_center() + egui::vec2(6.0, 0.0),
                    egui::Align2::LEFT_CENTER,
                    m.tool.label(),
                    egui::FontId::proportional(10.5),
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
        let hx = x_of(l.player.clock().position());
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
                    l.player.clock().seek(lo);
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
                    l.player.clock().seek(t_of(pos.x));
                }
            }
        }
    }

    /// Moments, each opening where it sits.
    fn list(&mut self, ui: &mut egui::Ui, p: &theme::Palette, tall: f32) {
        // Read what the list needs, then let go: the scroll closure needs the
        // whole view, so nothing may still be borrowed out of it.
        let (at, duration, moments) = {
            let Some(l) = self.loaded.as_ref() else {
                return;
            };
            (
                l.player.clock().position(),
                l.duration,
                l.edits.moments.clone(),
            )
        };
        if moments.is_empty() {
            theme::hint(
                ui,
                "Click the picture to zoom, hide or point at something. Drag the track to cut or speed up a stretch.",
            );
            return;
        }

        let mut remove: Option<u32> = None;
        let mut seek: Option<f64> = None;
        let mut refit: Option<u32> = None;

        egui::ScrollArea::vertical()
            .max_height(tall)
            .show(ui, |ui| {
                for m in &moments {
                    let id = m.id;
                    let live = m.covers(at);
                    let open = self.opened == Some(id);
                    let tint = tool_colour(&m.tool, p);

                    egui::Frame::NONE
                        .fill(if live {
                            theme::blend(tint, p.surface, 0.82)
                        } else {
                            p.surface
                        })
                        .stroke(egui::Stroke::new(1.0, if open { tint } else { p.stroke }))
                        .corner_radius(8)
                        .inner_margin(egui::Margin::symmetric(10, 8))
                        .show(ui, |ui| {
                            let head = ui.horizontal(|ui| {
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
                                                clock(m.start),
                                                clock(m.end)
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
                                self.editor(ui, p, m, duration, &mut remove, &mut seek, &mut refit);
                            }
                        });
                    ui.add_space(5.0);
                }
            });

        let Some(l) = self.loaded.as_mut() else {
            return;
        };
        if let Some(id) = refit {
            // Put the end back where the recording says it belongs. Without a
            // pointer track this is the placed default; with one it becomes
            // the moment the pointer actually left the area.
            if let Some(m) = l.edits.get(id).cloned() {
                let end = (m.start + 6.0).min(l.duration);
                l.edits.update(id, |m| m.end = end);
                self.saved = false;
            }
        }
        if let Some(t) = seek {
            l.player.clock().seek(t);
        }
        if let Some(id) = remove {
            l.edits.remove(id);
            if self.opened == Some(id) {
                self.opened = None;
            }
            self.saved = false;
        }
    }

    /// The fields for one moment, by what that tool actually has.
    #[allow(clippy::too_many_arguments)]
    fn editor(
        &mut self,
        ui: &mut egui::Ui,
        p: &theme::Palette,
        m: &Moment,
        duration: f64,
        remove: &mut Option<u32>,
        seek: &mut Option<f64>,
        refit: &mut Option<u32>,
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
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new("Closeness").size(11.0).color(p.weak));
                    if ui
                        .add(egui::Slider::new(&mut scale, 1.4..=3.2).show_value(false))
                        .changed()
                    {
                        l.edits.update(id, |m| {
                            m.tool = Tool::Zoom {
                                area,
                                follow,
                                scale,
                            };
                        });
                        self.saved = false;
                    }
                    ui.label(
                        egui::RichText::new(format!("×{scale:.1}"))
                            .monospace()
                            .size(11.0)
                            .color(p.weak),
                    );
                });
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
                if ui
                    .add(theme::outline("Re-fit to the pointer"))
                    .on_hover_text("Put the end where the pointer left this area")
                    .clicked()
                {
                    *refit = Some(id);
                }
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
    fn menu(&mut self, ctx: &egui::Context, p: &theme::Palette) {
        let Some(asking) = self.asking.as_ref() else {
            return;
        };
        let Some(l) = self.loaded.as_mut() else {
            self.asking = None;
            return;
        };
        let mut close = false;
        let mut placed: Option<(f64, f64, Tool)> = None;

        let title = match asking {
            Asking::Spot { at, .. } => format!("Here, at {}", clock(*at)),
            Asking::Span { from, to } => format!("{} → {}", clock(*from), clock(*to)),
        };

        egui::Window::new(title)
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
            .show(ctx, |ui| {
                ui.set_max_width(290.0);
                match asking {
                    Asking::Spot { at, area } => {
                        let (at, area) = (*at, *area);
                        let end = (at + 6.0).min(l.duration);
                        if item(
                            ui,
                            p,
                            "Zoom & follow the pointer",
                            "Holds until the pointer leaves this area",
                        ) {
                            placed = Some((
                                at,
                                end,
                                Tool::Zoom {
                                    area,
                                    follow: true,
                                    scale: 2.1,
                                },
                            ));
                        }
                        if item(ui, p, "Zoom and hold still", "Stays on this area") {
                            placed = Some((
                                at,
                                end,
                                Tool::Zoom {
                                    area,
                                    follow: false,
                                    scale: 2.1,
                                },
                            ));
                        }
                        if item(ui, p, "Hide this region", "Covered while it is on screen") {
                            placed = Some((at, (at + 8.0).min(l.duration), Tool::Hide { area }));
                        }
                        if item(ui, p, "Point at this", "Dims everything else") {
                            placed = Some((
                                at,
                                (at + 4.0).min(l.duration),
                                Tool::Callout {
                                    area,
                                    text: "Look here".into(),
                                },
                            ));
                        }
                    }
                    Asking::Span { from, to } => {
                        let (from, to) = (*from, *to);
                        let span = to - from;
                        let at_end = from < 1.2 || to > l.duration - 1.2;
                        if at_end && item(ui, p, "Trim this end", &format!("Drops {span:.1}s")) {
                            placed = Some((from, to, Tool::Trim));
                        }
                        if item(ui, p, "Cut this out", &format!("{span:.1}s removed")) {
                            placed = Some((from, to, Tool::Cut));
                        }
                        if item(
                            ui,
                            p,
                            "Speed up 4×",
                            &format!("{span:.1}s becomes {:.1}s", span / 4.0),
                        ) {
                            placed = Some((from, to, Tool::Speed { rate: 4.0 }));
                        }
                        if item(ui, p, "Mute this stretch", "The picture keeps playing") {
                            placed = Some((from, to, Tool::Mute));
                        }
                        if l.screens.len() > 1
                            && item(ui, p, "Show one screen", "Overrides what Screenilux picked")
                        {
                            let file = l.screens.first().cloned().unwrap_or_default();
                            placed = Some((from, to, Tool::Screen { file }));
                        }
                    }
                }
                ui.add_space(4.0);
                if ui.add(theme::outline("Cancel")).clicked() {
                    close = true;
                }
            });

        if let Some((a, b, tool)) = placed {
            let id = l.edits.add(a, b, tool);
            self.opened = Some(id);
            self.saved = false;
            self.selection = None;
            close = true;
        }
        if close {
            self.asking = None;
        }
    }
}

fn item(ui: &mut egui::Ui, p: &theme::Palette, title: &str, sub: &str) -> bool {
    let clicked = ui
        .vertical(|ui| {
            ui.label(
                egui::RichText::new(title)
                    .family(theme::strong())
                    .color(p.text),
            );
            ui.label(egui::RichText::new(sub).size(11.0).color(p.weak));
        })
        .response
        .interact(egui::Sense::click())
        .clicked();
    ui.add_space(6.0);
    clicked
}

/// The largest rectangle of `aspect` that fits inside `outer`, centred on it.
fn fit_inside(outer: egui::Rect, aspect: f32) -> egui::Rect {
    let w = outer.width().min(outer.height() * aspect);
    egui::Rect::from_center_size(outer.center(), egui::vec2(w, w / aspect))
}

fn tool_colour(tool: &Tool, p: &theme::Palette) -> egui::Color32 {
    match tool {
        Tool::Zoom { .. } | Tool::MarkClicks => p.signal,
        Tool::Hide { .. } => p.action,
        Tool::Callout { .. } | Tool::Screen { .. } => p.soft,
        _ => p.weak,
    }
}

fn clock(t: f64) -> String {
    let m = (t / 60.0).floor() as i64;
    format!("{m}:{:04.1}", t - m as f64 * 60.0)
}
fn clock_short(t: f64) -> String {
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
        assert_eq!(clock(0.0), "0:00.0");
        assert_eq!(clock(9.25), "0:09.2");
        assert_eq!(clock(75.5), "1:15.5");
        assert_eq!(clock_short(0.0), "0:00");
        assert_eq!(clock_short(90.0), "1:30");
    }

    #[test]
    fn a_fresh_view_has_nothing_open() {
        let v = EditView::new();
        assert!(!v.has_recording());
        assert!(v.saved, "nothing to save before anything is open");
    }
}
