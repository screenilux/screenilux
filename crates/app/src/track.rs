//! The timeline track: a ruler, the effects as blocks, and the playhead.
//!
//! The track is the editor's one *when* surface. Everything that has a time
//! is read and changed here, so it is built around three things a strip of
//! pixels has to do well.
//!
//! **Be readable at any length.** A recording is four seconds or forty
//! minutes, and the same ruler has to suit both, so the marks come off a
//! ladder of round steps and the smallest one that still leaves room for
//! its own label wins. Effects are packed into lanes rather than stacked on
//! one row, because two effects over the same stretch is the normal case —
//! a zoom while a caption shows — and one hiding the other is a mistake the
//! user cannot see.
//!
//! **Be smooth.** Dragging the playhead used to ask the decoder for a new
//! frame on every mouse move, each one cancelling the last, so the picture
//! lagged the pointer and the line stuttered. A scrub now paints from where
//! the pointer is and only asks the decoder once it has moved a frame's
//! worth, which is the difference between scrubbing and waiting.
//!
//! **Be exact.** A dragged edge pulls onto the ends of the recording, the
//! playhead and the other effects' edges when it comes within a few pixels,
//! so placing a cut by eye and placing it exactly cost the same.
//!
//! The track owns what it shows — which effect is lit, which stretch is
//! marked, what the pointer is doing — and hands back only what the editor
//! has to answer: a stretch to ask about, and whether a timing changed.

use eframe::egui;
use screenilux_core::edit::{EditList, Effect, MIN_SPAN};
use screenilux_player::SharedClock;

use crate::edit_view::{stamp, stamp_short, tool_colour};
use crate::theme;

/// The band the ruler stands in, above the lanes.
const RULER_H: f32 = 17.0;
/// One lane: a name at 11pt with room above and below it.
const LANE_H: f32 = 22.0;
/// Between two lanes, so blocks in neighbouring lanes read as separate.
const LANE_GAP: f32 = 4.0;
/// Inside the groove: above the ruler, under it, and below the last lane.
const PAD: f32 = 5.0;
/// Lanes the track grows to before effects start sharing one. Past four the
/// strip would crowd out the picture it belongs under.
const MAX_LANES: usize = 4;

/// How close to a block's edge counts as grabbing that edge.
const GRIP: f32 = 6.0;
/// The narrowest a block is drawn, so a half-second effect is still there
/// to be seen and grabbed.
const MIN_BLOCK: f32 = 7.0;
/// A drag shorter than this is a click: move the playhead, do not mark.
const TAP: f64 = 0.35;
/// How near, in pixels, a dragged edge has to come to something before it
/// pulls onto it.
const SNAP: f32 = 6.0;
/// The playhead's head, which is the part that is grabbed.
const HEAD_W: f32 = 11.0;
const HEAD_H: f32 = 9.0;
/// Don't ask the decoder for another frame until the scrub has moved this
/// far. A frame at 30fps: closer than that and the picture cannot show the
/// difference anyway.
const SCRUB_STEP: f64 = 1.0 / 30.0;
/// The room a ruler label wants to itself, label and gap together.
const LABEL_ROOM: f32 = 56.0;

/// How tall the track is for a given number of lanes.
pub fn height(lanes: usize) -> f32 {
    let n = lanes.clamp(1, MAX_LANES) as f32;
    PAD + RULER_H + PAD + n * LANE_H + (n - 1.0) * LANE_GAP + PAD
}

/// How many lanes a set of effects needs, which is what `height` wants.
pub fn lanes_needed(edits: &EditList) -> usize {
    packed(&edits.effects).1
}

/// What the pointer is doing on the track.
#[derive(Default)]
enum Grab {
    #[default]
    None,
    /// Dragging the playhead. Playback stands down for the duration and
    /// picks up again where the drag left it.
    Scrub { was_playing: bool },
    /// Dragging out a stretch for a tool that changes time.
    Selecting { from: f64 },
    /// Moving a whole effect, holding where it was grabbed.
    Moving { id: u32, offset: f64 },
    /// Dragging one end of an effect.
    Edge { id: u32, start_side: bool },
    /// Moving a cut, which is where it is now.
    Cut { at: f64 },
}

/// What the track keeps between frames.
#[derive(Default)]
pub struct State {
    grab: Grab,
    /// Where the playhead is being dragged to. The track paints from this
    /// rather than from the clock, so the line follows the pointer whether
    /// or not the decoder has caught up with it.
    scrub: Option<f64>,
    /// The last time handed to the clock while scrubbing, so a drag of one
    /// pixel does not ask for a frame the player is already showing.
    asked: f64,
    /// A stretch marked out, waiting for a tool.
    pub selection: Option<(f64, f64)>,
    /// Which effect is lit, which is also the one open in the list:
    /// selecting in one place selects in the other.
    pub opened: Option<u32>,
    /// Which cut is lit, by where it is. At most one of this and `opened`
    /// is set.
    pub cut: Option<f64>,
}

impl State {
    /// Forget everything about the recording that was open.
    pub fn clear(&mut self) {
        *self = Self::default();
    }
}

/// What a frame of the track leaves for the editor to answer.
pub struct Out {
    /// Where the track was drawn, for the tour to point at.
    pub rect: egui::Rect,
    /// A stretch was marked out: ask what to do with it.
    pub marked: Option<(f64, f64)>,
    /// A timing changed, so the edits are no longer saved.
    pub changed: bool,
}

/// One effect as it was laid out this frame.
struct Block {
    id: u32,
    rect: egui::Rect,
    tint: egui::Color32,
    label: String,
    span: f64,
}

/// Draw the track and act on whatever the pointer did to it.
pub fn show(
    ui: &mut egui::Ui,
    p: &theme::Palette,
    st: &mut State,
    edits: &mut EditList,
    duration: f64,
    clock: &SharedClock,
) -> Out {
    let dur = duration.max(MIN_SPAN);
    let (lane_of, lanes) = packed(&edits.effects);
    let width = ui.available_width();
    let (rect, response) = ui.allocate_exact_size(
        egui::vec2(width, height(lanes)),
        egui::Sense::click_and_drag(),
    );
    let painter = ui.painter_at(rect);
    let mut out = Out {
        rect,
        marked: None,
        changed: false,
    };

    // A groove in the card it sits in, not a box on top of it: one tone
    // down from the card, with no border of its own.
    painter.rect_filled(
        rect,
        egui::CornerRadius::same(9),
        theme::blend(p.stage, p.text, 0.07),
    );

    let x_of = |t: f64| rect.left() + (t / dur) as f32 * rect.width();
    let t_of = |x: f32| (((x - rect.left()) / rect.width()) as f64 * dur).clamp(0.0, dur);
    // Seconds to the pixel, for reading a reach in pixels as one in time.
    let per_px = dur / rect.width().max(1.0) as f64;

    // The two bands the pointer is read against: the ruler, where a drag
    // scrubs, and the lanes, where a drag marks a stretch or moves a block.
    let base = rect.top() + PAD + RULER_H;
    let lanes_top = base + PAD;
    let lanes_bottom = rect.bottom() - PAD;
    let ruler_band = egui::Rect::from_min_max(rect.left_top(), egui::pos2(rect.right(), base));

    ruler(&painter, p, rect, base, dur, &x_of);

    // The marked stretch, under the blocks: it is the ground a new effect
    // will be laid on, not something sitting over the ones already there.
    if let Some((a, b)) = st.selection {
        let (lo, hi) = (a.min(b), a.max(b));
        let band = egui::Rect::from_min_max(
            egui::pos2(x_of(lo), lanes_top - 2.0),
            egui::pos2(x_of(hi), lanes_bottom + 2.0),
        );
        let round = egui::CornerRadius::same(5);
        painter.rect_filled(band, round, theme::blend(p.signal, p.surface, 0.78));
        painter.rect_stroke(
            band,
            round,
            egui::Stroke::new(1.0, p.signal),
            egui::StrokeKind::Inside,
        );
        if band.width() > 44.0 {
            painter.text(
                band.center_top() + egui::vec2(0.0, 2.0),
                egui::Align2::CENTER_TOP,
                span_text(hi - lo),
                egui::FontId::new(10.0, theme::strong()),
                p.text,
            );
        }
    }

    // What is removed reads as a gap under everything: a hatched band, so
    // the blocks over it still show what was there.
    for part in edits.parts(dur).iter().filter(|q| q.removed) {
        let band = egui::Rect::from_min_max(
            egui::pos2(x_of(part.start), lanes_top - 2.0),
            egui::pos2(x_of(part.end), lanes_bottom + 2.0),
        );
        gap(&painter, p, band);
    }

    // Lay the blocks out before painting any, so hit testing and painting
    // agree about where they are.
    let blocks: Vec<Block> = edits
        .effects
        .iter()
        .map(|m| {
            let lane = lane_of
                .iter()
                .find(|(id, _)| *id == m.id)
                .map_or(0, |(_, l)| *l);
            let top = lanes_top + lane as f32 * (LANE_H + LANE_GAP);
            let left = x_of(m.start);
            Block {
                id: m.id,
                rect: egui::Rect::from_min_max(
                    egui::pos2(left, top),
                    egui::pos2(x_of(m.end).max(left + MIN_BLOCK), top + LANE_H),
                ),
                tint: tool_colour(&m.tool, p),
                label: m.tool.label(),
                span: m.span(),
            }
        })
        .collect();

    // What a given point is on: the last block drawn there, and whether it
    // is near enough to either edge to be pulling that edge rather than
    // the whole block.
    let hit_at = |pos: egui::Pos2| {
        blocks
            .iter()
            .rev()
            .find(|b| b.rect.expand2(egui::vec2(GRIP, 0.0)).contains(pos))
            .map(|b| {
                (
                    b.id,
                    (pos.x - b.rect.left()).abs() <= GRIP,
                    (pos.x - b.rect.right()).abs() <= GRIP,
                )
            })
    };
    // Where the pointer is now, which is what the lit block and the cursor
    // are read off.
    let pointer = response.hover_pos().or(response.interact_pointer_pos());
    let hit = pointer.and_then(hit_at);
    // A cut is grabbed anywhere down its line in the lanes, and wins over
    // the block whose edge it shares: moving the cut moves that edge too.
    let cuts = edits.cuts.clone();
    let cut_at = |pos: egui::Pos2| {
        (pos.y >= lanes_top - 4.0)
            .then(|| {
                cuts.iter()
                    .copied()
                    .map(|c| (c, (x_of(c) - pos.x).abs()))
                    .filter(|(_, away)| *away <= GRIP)
                    .min_by(|a, b| a.1.total_cmp(&b.1))
                    .map(|(c, _)| c)
            })
            .flatten()
    };
    let on_cut = pointer.and_then(cut_at);

    for b in &blocks {
        let on = st.opened == Some(b.id);
        let over = hit.map(|(id, ..)| id) == Some(b.id);
        block(&painter, p, b, on, over);
    }

    for &c in &cuts {
        let lit = st.cut == Some(c) || on_cut == Some(c);
        cut_line(&painter, p, x_of(c), lanes_top, lanes_bottom, lit);
    }

    // The playhead last, over everything: where you are reading from
    // matters more than whatever is under it.
    let now = st.scrub.unwrap_or_else(|| clock.position());
    let hx = x_of(now.clamp(0.0, dur));
    painter.vline(
        hx,
        base..=lanes_bottom + 2.0,
        egui::Stroke::new(1.5, p.signal),
    );
    let head = egui::Rect::from_center_size(egui::pos2(hx, base), egui::vec2(HEAD_W, HEAD_H));
    painter.rect_filled(head, egui::CornerRadius::same(3), p.signal);
    // While scrubbing, the ruler says where you are, which is the one thing
    // worth covering its labels for.
    if st.scrub.is_some() {
        readout(&painter, p, rect, now, hx);
    }

    // --- what the pointer did -------------------------------------------

    let on_head = pointer.is_some_and(|pos| head.expand2(egui::vec2(4.0, 3.0)).contains(pos));
    let in_ruler = pointer.is_some_and(|pos| ruler_band.contains(pos));
    let scrubbing = matches!(st.grab, Grab::Scrub { .. });

    let cursor = match (&st.grab, hit) {
        (Grab::Moving { .. }, _) => Some(egui::CursorIcon::Grabbing),
        (Grab::Edge { .. } | Grab::Scrub { .. } | Grab::Cut { .. }, _) => {
            Some(egui::CursorIcon::ResizeHorizontal)
        }
        (Grab::Selecting { .. }, _) => None,
        (Grab::None, _) if on_head || in_ruler || on_cut.is_some() => {
            Some(egui::CursorIcon::ResizeHorizontal)
        }
        (Grab::None, Some((_, true, _) | (_, _, true))) => Some(egui::CursorIcon::ResizeHorizontal),
        (Grab::None, Some(_)) => Some(egui::CursorIcon::Grab),
        (Grab::None, None) => None,
    };
    if let Some(cursor) = cursor
        && (response.hovered() || response.dragged())
    {
        ui.ctx().set_cursor_icon(cursor);
    }

    if response.drag_started()
        && let Some(pos) = ui
            .input(|i| i.pointer.press_origin())
            .or_else(|| response.interact_pointer_pos())
    {
        // A gesture is whatever it started on, read from where the button
        // went down rather than from where the pointer has got to: a quick
        // flick can carry it over a block before the drag is reported, and
        // grabbing that block is not what the hand meant.
        let t = t_of(pos.x);
        let hit = hit_at(pos);
        let on_head = head.expand2(egui::vec2(4.0, 3.0)).contains(pos);
        let in_ruler = ruler_band.contains(pos);
        st.grab = if on_head || in_ruler {
            let was_playing = clock.is_playing();
            clock.pause();
            st.scrub = Some(t);
            st.asked = t;
            clock.seek(t);
            Grab::Scrub { was_playing }
        } else if let Some(c) = cut_at(pos) {
            st.cut = Some(c);
            st.opened = None;
            Grab::Cut { at: c }
        } else {
            st.cut = None;
            match hit {
                Some((id, true, _)) => {
                    st.opened = Some(id);
                    Grab::Edge {
                        id,
                        start_side: true,
                    }
                }
                Some((id, _, true)) => {
                    st.opened = Some(id);
                    Grab::Edge {
                        id,
                        start_side: false,
                    }
                }
                Some((id, ..)) => {
                    st.opened = Some(id);
                    let from = edits.get(id).map_or(t, |m| m.start);
                    Grab::Moving {
                        id,
                        offset: t - from,
                    }
                }
                None => {
                    st.selection = Some((t, t));
                    Grab::Selecting { from: t }
                }
            }
        };
    }

    if response.dragged()
        && let Some(pos) = response.interact_pointer_pos()
    {
        let t = t_of(pos.x);
        match st.grab {
            Grab::Scrub { .. } => {
                st.scrub = Some(t);
                // One seek per frame's worth of movement, not one per mouse
                // move: each seek cancels the decoder's last, so asking too
                // often is how a scrub ends up showing nothing at all.
                if (t - st.asked).abs() >= SCRUB_STEP {
                    st.asked = t;
                    clock.seek(t);
                }
            }
            Grab::Selecting { from } => st.selection = Some((from, t)),
            Grab::Cut { at } => {
                // A cut pulls onto the playhead only: the edges near it
                // are its own gap's, which move with it.
                let t = if (t - now).abs() <= SNAP as f64 * per_px {
                    now
                } else {
                    t
                };
                let to = edits.move_cut(at, t, dur);
                if to != at {
                    st.grab = Grab::Cut { at: to };
                    st.cut = Some(to);
                    out.changed = true;
                }
            }
            Grab::Moving { id, offset } => {
                let span = edits.get(id).map_or(MIN_SPAN, |m| m.span());
                let want = (t - offset).clamp(0.0, (dur - span).max(0.0));
                // A moved block snaps by whichever of its ends lands on
                // something first, so it keeps its length either way.
                let start = match (
                    snap(want, id, edits, dur, now, per_px),
                    snap(want + span, id, edits, dur, now, per_px),
                ) {
                    (Some(s), _) => s,
                    (None, Some(e)) => e - span,
                    (None, None) => want,
                }
                .clamp(0.0, (dur - span).max(0.0));
                edits.update(id, |m| {
                    m.start = start;
                    m.end = start + span;
                });
                out.changed = true;
            }
            Grab::Edge { id, start_side } => {
                let t = snap(t, id, edits, dur, now, per_px).unwrap_or(t);
                edits.update(id, |m| {
                    if start_side {
                        m.start = t.min(m.end - MIN_SPAN).max(0.0);
                    } else {
                        m.end = t.max(m.start + MIN_SPAN).min(dur);
                    }
                });
                out.changed = true;
            }
            Grab::None => {}
        }
    }

    if response.drag_stopped() {
        match std::mem::take(&mut st.grab) {
            Grab::Scrub { was_playing } => {
                if let Some(t) = st.scrub.take() {
                    clock.seek(t);
                }
                if was_playing {
                    clock.play();
                }
            }
            Grab::Selecting { .. } => {
                if let Some((a, b)) = st.selection {
                    let (lo, hi) = (a.min(b), a.max(b));
                    if hi - lo < TAP {
                        st.selection = None;
                        clock.seek(lo);
                    } else {
                        out.marked = Some((lo, hi));
                    }
                }
            }
            _ => {}
        }
    }

    if response.clicked()
        && matches!(st.grab, Grab::None)
        && let Some(pos) = response.interact_pointer_pos()
    {
        let cut = cut_at(pos).filter(|_| !ruler_band.contains(pos));
        match (cut, hit) {
            (Some(c), _) => {
                st.cut = Some(c);
                st.opened = None;
            }
            (None, Some((id, ..))) if !ruler_band.contains(pos) => {
                st.opened = Some(id);
                st.cut = None;
            }
            _ => {
                st.selection = None;
                clock.seek(t_of(pos.x));
            }
        }
    }

    // The playhead moves on its own while the recording plays, and follows
    // the pointer while it is dragged; neither is an input event, so the
    // track has to ask for the frames that show it moving.
    if scrubbing || clock.is_playing() {
        ui.ctx().request_repaint();
    }

    out
}

/// One block: a tinted slab with a bar of full colour down its leading
/// edge, so it says which effect it is even when it is too narrow to carry
/// its own name.
fn block(painter: &egui::Painter, p: &theme::Palette, b: &Block, on: bool, over: bool) {
    let round = egui::CornerRadius::same(6);
    let mix = if on {
        0.3
    } else if over {
        0.42
    } else {
        0.54
    };
    painter.rect_filled(b.rect, round, theme::blend(b.tint, p.surface, mix));
    painter.rect_filled(
        egui::Rect::from_min_size(b.rect.min, egui::vec2(3.0, b.rect.height())),
        egui::CornerRadius {
            nw: 6,
            sw: 6,
            ne: 0,
            se: 0,
        },
        b.tint,
    );
    painter.rect_stroke(
        b.rect,
        round,
        egui::Stroke::new(if on { 1.8 } else { 1.0 }, b.tint),
        egui::StrokeKind::Inside,
    );

    // Text is clipped rather than dropped: half a name still says which
    // effect this is, where no name at all says nothing.
    if b.rect.width() > 26.0 {
        let text = painter.with_clip_rect(b.rect.shrink2(egui::vec2(9.0, 0.0)));
        text.text(
            b.rect.left_center() + egui::vec2(10.0, 0.0),
            egui::Align2::LEFT_CENTER,
            &b.label,
            egui::FontId::new(10.5, theme::strong()),
            p.text,
        );
        // Its length, once there is room for it without crowding the name.
        if b.rect.width() > 150.0 {
            text.text(
                b.rect.right_center() - egui::vec2(14.0, 0.0),
                egui::Align2::RIGHT_CENTER,
                span_text(b.span),
                egui::FontId::new(9.5, theme::strong()),
                p.weak,
            );
        }
    }

    // Grips, so the edges say they can be pulled before they are tried.
    if (on || over) && b.rect.width() > 20.0 {
        for x in [b.rect.left() + 5.5, b.rect.right() - 5.5] {
            painter.vline(
                x,
                b.rect.top() + 6.0..=b.rect.bottom() - 6.0,
                egui::Stroke::new(1.5, theme::blend(b.tint, p.text, 0.35)),
            );
        }
    }
}

/// A removed stretch: the groove darkened and struck through, so it reads
/// as footage that is not there rather than as one more block.
fn gap(painter: &egui::Painter, p: &theme::Palette, band: egui::Rect) {
    painter.rect_filled(
        band,
        egui::CornerRadius::same(5),
        theme::blend(p.stage, p.text, 0.16),
    );
    let hatch = painter.with_clip_rect(band.intersect(painter.clip_rect()));
    let stroke = egui::Stroke::new(1.0, theme::blend(p.weak, p.stage, 0.55));
    let mut x = band.left() - band.height();
    while x < band.right() {
        hatch.line_segment(
            [
                egui::pos2(x, band.bottom()),
                egui::pos2(x + band.height(), band.top()),
            ],
            stroke,
        );
        x += 7.0;
    }
}

/// A cut: a line down through the lanes with a tab at its head to grab.
fn cut_line(painter: &egui::Painter, p: &theme::Palette, x: f32, top: f32, bottom: f32, lit: bool) {
    let colour = if lit {
        p.action
    } else {
        theme::blend(p.action, p.text, 0.25)
    };
    painter.vline(
        x,
        top - 3.0..=bottom + 3.0,
        egui::Stroke::new(if lit { 2.5 } else { 1.8 }, colour),
    );
    let tab = egui::Rect::from_center_size(egui::pos2(x, top + 1.0), egui::vec2(9.0, 9.0));
    painter.rect_filled(tab, egui::CornerRadius::same(2), colour);
}

/// The ruler: labels along the top of its band, marks standing on the line
/// under them, and finer marks between where they fit.
fn ruler(
    painter: &egui::Painter,
    p: &theme::Palette,
    rect: egui::Rect,
    base: f32,
    dur: f64,
    x_of: &impl Fn(f64) -> f32,
) {
    let step = ruler_step(dur, rect.width());
    let faint = theme::blend(p.stroke, p.stage, 0.45);
    let tick = theme::blend(p.stroke, p.text, 0.1);

    // Finer marks, a fifth of a step or a half of one: the first of those
    // that does not crowd.
    let per_second = rect.width() / dur as f32;
    if let Some(small) = [step / 5.0, step / 2.0]
        .into_iter()
        .find(|s| *s as f32 * per_second >= 7.0)
    {
        let ratio = (step / small).round() as usize;
        for k in 1..=(dur / small) as usize {
            if ratio > 0 && k % ratio == 0 {
                continue;
            }
            painter.vline(
                x_of(k as f64 * small),
                base - 3.0..=base,
                egui::Stroke::new(1.0, faint),
            );
        }
    }

    let last = (dur / step) as usize;
    for k in 0..=last {
        let t = k as f64 * step;
        let x = x_of(t);
        painter.vline(x, base - 6.0..=base, egui::Stroke::new(1.0, tick));
        // The end labels turn inwards rather than hang off the edge.
        let (align, dx) = if k == 0 {
            (egui::Align2::LEFT_TOP, 2.0)
        } else if k == last && x > rect.right() - LABEL_ROOM * 0.5 {
            (egui::Align2::RIGHT_TOP, -2.0)
        } else {
            (egui::Align2::CENTER_TOP, 0.0)
        };
        painter.text(
            egui::pos2(x + dx, rect.top() + 3.0),
            align,
            stamp_short(t),
            egui::FontId::new(9.5, theme::strong()),
            p.weak,
        );
    }

    painter.hline(
        rect.left() + 6.0..=rect.right() - 6.0,
        base + 2.5,
        egui::Stroke::new(1.0, faint),
    );
}

/// Where the scrub is, said plainly over the ruler it is covering.
fn readout(painter: &egui::Painter, p: &theme::Palette, rect: egui::Rect, now: f64, hx: f32) {
    let text = painter.layout_no_wrap(
        stamp(now),
        egui::FontId::new(10.0, theme::strong()),
        theme::ON_DARK,
    );
    let size = text.size();
    let pill = egui::Rect::from_center_size(
        egui::pos2(
            hx.clamp(
                rect.left() + size.x * 0.5 + 8.0,
                rect.right() - size.x * 0.5 - 8.0,
            ),
            rect.top() + PAD + RULER_H * 0.5 - 3.0,
        ),
        size + egui::vec2(10.0, 4.0),
    );
    painter.rect_filled(pill, egui::CornerRadius::same(4), p.signal);
    painter.galley(pill.center() - size * 0.5, text, theme::ON_DARK);
}

/// The seconds between labelled marks: the smallest round step that still
/// leaves each label room to itself.
fn ruler_step(dur: f64, width: f32) -> f64 {
    const LADDER: [f64; 13] = [
        0.5, 1.0, 2.0, 5.0, 10.0, 15.0, 30.0, 60.0, 120.0, 300.0, 600.0, 1800.0, 3600.0,
    ];
    let per_second = width / dur.max(MIN_SPAN) as f32;
    LADDER
        .into_iter()
        .find(|s| *s as f32 * per_second >= LABEL_ROOM)
        .unwrap_or(3600.0)
}

/// Pull a dragged time onto whatever is near it: the ends of the recording,
/// the playhead, and the other effects' edges. `None` when nothing is in
/// reach, so a caller can tell a snapped time from one left where it was.
fn snap(t: f64, moving: u32, edits: &EditList, dur: f64, at: f64, per_px: f64) -> Option<f64> {
    let reach = SNAP as f64 * per_px;
    let edges = edits
        .effects
        .iter()
        .filter(|m| m.id != moving)
        .flat_map(|m| [m.start, m.end]);
    let mut best: Option<(f64, f64)> = None;
    for c in [0.0, dur, at].into_iter().chain(edges) {
        let away = (c - t).abs();
        if away <= reach && best.is_none_or(|(b, _)| away < b) {
            best = Some((away, c));
        }
    }
    best.map(|(_, c)| c)
}

/// Put each effect in the first lane whose last effect has already ended,
/// taking them in start order so a lane reads left to right. Past the last
/// lane they share the one that frees up soonest: a track that grew without
/// limit would crowd out the picture it belongs under.
fn packed(effects: &[Effect]) -> (Vec<(u32, usize)>, usize) {
    let mut order: Vec<&Effect> = effects.iter().collect();
    order.sort_by(|a, b| a.start.total_cmp(&b.start));
    let mut ends: Vec<f64> = Vec::new();
    let mut lanes = Vec::with_capacity(order.len());
    for m in order {
        let lane = match ends.iter().position(|e| *e <= m.start) {
            Some(i) => i,
            None if ends.len() < MAX_LANES => {
                ends.push(f64::NEG_INFINITY);
                ends.len() - 1
            }
            None => ends
                .iter()
                .enumerate()
                .min_by(|a, b| a.1.total_cmp(b.1))
                .map_or(0, |(i, _)| i),
        };
        ends[lane] = m.end;
        lanes.push((m.id, lane));
    }
    (lanes, ends.len().max(1))
}

/// How long a stretch runs, at the precision that length deserves.
fn span_text(s: f64) -> String {
    if s < 10.0 {
        format!("{s:.1}s")
    } else if s < 60.0 {
        format!("{s:.0}s")
    } else {
        let m = (s / 60.0).floor() as i64;
        format!("{m}m {:.0}s", s - m as f64 * 60.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use screenilux_core::edit::Tool;

    fn list(spans: &[(f64, f64)]) -> EditList {
        let mut l = EditList::new();
        for (a, b) in spans {
            l.add(*a, *b, Tool::Mute);
        }
        l
    }

    #[test]
    fn effects_that_do_not_touch_share_one_lane() {
        let (lanes, n) = packed(&list(&[(0.0, 1.0), (2.0, 3.0), (4.0, 5.0)]).effects);
        assert_eq!(n, 1);
        assert!(lanes.iter().all(|(_, l)| *l == 0));
    }

    #[test]
    fn effects_over_the_same_stretch_get_a_lane_each() {
        let (lanes, n) = packed(&list(&[(0.0, 5.0), (1.0, 6.0), (2.0, 7.0)]).effects);
        assert_eq!(n, 3);
        let mut seen: Vec<usize> = lanes.iter().map(|(_, l)| *l).collect();
        seen.sort_unstable();
        assert_eq!(seen, vec![0, 1, 2]);
    }

    #[test]
    fn the_track_never_grows_past_its_last_lane() {
        let spans: Vec<(f64, f64)> = (0..12).map(|i| (i as f64 * 0.1, 10.0)).collect();
        let (lanes, n) = packed(&list(&spans).effects);
        assert_eq!(n, MAX_LANES);
        assert!(lanes.iter().all(|(_, l)| *l < MAX_LANES));
        assert_eq!(height(n), height(99));
    }

    #[test]
    fn the_ruler_steps_up_as_the_recording_gets_longer() {
        // A short recording is marked every second or two; a long one
        // cannot be, or the labels would sit on top of each other.
        assert!(ruler_step(12.0, 800.0) <= 2.0);
        assert!(ruler_step(3600.0, 800.0) >= 300.0);
        // However long it runs, the marks stay far enough apart to read.
        for dur in [2.0, 30.0, 600.0, 7200.0] {
            let step = ruler_step(dur, 800.0);
            let gap = (step / dur) as f32 * 800.0;
            assert!(
                gap >= LABEL_ROOM || step == 3600.0,
                "{dur}s crowds its labels at {gap}px"
            );
        }
    }

    #[test]
    fn a_dragged_edge_pulls_onto_a_neighbour_but_not_across_the_track() {
        let edits = list(&[(0.0, 2.0), (5.0, 8.0)]);
        let id = edits.effects[0].id;
        // A hundredth off its neighbour's start, at fifty pixels a second:
        // within reach, so it pulls on.
        assert_eq!(snap(4.99, id, &edits, 20.0, 15.0, 0.02), Some(5.0));
        // Two seconds off: nothing in reach.
        assert_eq!(snap(3.0, id, &edits, 20.0, 15.0, 0.02), None);
        // Its own edges never pull it, or it could not be moved at all.
        assert_eq!(snap(2.0, id, &edits, 20.0, 15.0, 0.02), None);
    }

    #[test]
    fn a_stretch_reads_the_way_an_editor_says_it() {
        assert_eq!(span_text(2.45), "2.5s");
        assert_eq!(span_text(42.0), "42s");
        assert_eq!(span_text(95.0), "1m 35s");
    }
}
