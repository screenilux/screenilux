//! The window's shape while recording, and getting it back afterwards.
//!
//! The window belongs to whoever is using it. Where it sits, how big it is
//! and whether it is maximised are theirs to set, and nothing in the app
//! changes any of that while they work: switching tabs changes what is
//! inside the window, never the window itself.
//!
//! Recording is the one exception, and it is one the user asks for. The
//! window shrinks to a small borderless bar at the bottom of the screen so
//! it is not sitting on top of whatever is being recorded, and stopping
//! puts the window back exactly as it was found, down to the maximised
//! state. Drag the bar somewhere better and it stays there.

use eframe::egui;

/// The width content is laid out at on the column tabs. Wider than this and
/// a settings row turns into a long reach between its label and its switch,
/// so on a wide window the column is centred rather than stretched.
pub const COLUMN: f32 = 460.0;

/// The width the Record tab lays its screens out at. A live preview is the
/// thing being judged, so it gets more room than a column of settings.
pub const STAGE: f32 = 920.0;

/// The smallest the window can be made, other than as the bar.
pub const MIN_SIZE: egui::Vec2 = egui::vec2(560.0, 420.0);

/// The window at its first ever launch, before there is anything of the
/// user's own to restore. It opens maximised over this, so this is the size
/// restoring down gives.
pub const FIRST_RUN: egui::Vec2 = egui::vec2(1180.0, 760.0);

/// The recording bar: stop, discard, the clock, pause and expand in a row.
pub const BAR_SIZE: egui::Vec2 = egui::vec2(314.0, 56.0);

/// The bar's gap above the taskbar.
const BAR_MARGIN: f32 = 12.0;

/// What the window is shaped for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fit {
    /// An ordinary window, shaped by whoever is using it.
    Window,
    /// The recording bar.
    Bar,
}

/// A whole geometry: enough to put a window back exactly as it was.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Shape {
    /// Outer position, in points.
    pos: egui::Pos2,
    /// Inner size, in points.
    size: egui::Vec2,
    maximised: bool,
}

pub struct Sizer {
    current: Fit,
    /// The window as it was before it became the bar, to be put back.
    before: Option<Shape>,
    /// Where the bar was dragged to during this recording, so going to the
    /// full window and back does not undo the drag. Each recording starts
    /// with the bar centred again.
    bar_pos: Option<egui::Pos2>,
    /// Where the bar was just sent, and how many more frames to insist on
    /// it. Coming out of maximised, Windows puts the window back where it
    /// was before maximising a moment after it is asked to move, so the move
    /// can land first and be undone. Until it has landed, where the window
    /// happens to be is not somewhere anyone dragged it.
    settling: Option<(egui::Pos2, u8)>,
}

/// How many frames the bar insists on its place before giving up.
const SETTLE_TRIES: u8 = 20;

impl Sizer {
    pub fn new() -> Self {
        Self {
            current: Fit::Window,
            before: None,
            bar_pos: None,
            settling: None,
        }
    }

    /// The recording is over: the next one starts with the bar centred.
    pub fn forget_bar(&mut self) {
        self.bar_pos = None;
        self.settling = None;
    }

    /// Shape the window for `fit`, if it is not already.
    pub fn update(&mut self, ctx: &egui::Context, fit: Fit) {
        let vp = ctx.input(|i| i.viewport().clone());
        let (Some(outer), Some(inner)) = (vp.outer_rect, vp.inner_rect) else {
            return;
        };
        if vp.minimized == Some(true) {
            return;
        }
        let maximised = vp.maximized == Some(true);

        if self.current == fit {
            if fit == Fit::Bar {
                match self.settling {
                    Some((target, tries)) if tries > 0 && (outer.min - target).length() > 2.0 => {
                        use egui::ViewportCommand as C;
                        ctx.send_viewport_cmd(C::InnerSize(BAR_SIZE));
                        ctx.send_viewport_cmd(C::OuterPosition(target));
                        self.settling = Some((target, tries - 1));
                    }
                    Some(_) => self.settling = None,
                    // The bar can be dragged anywhere; remember where it was
                    // left.
                    None => self.bar_pos = Some(outer.min),
                }
            }
            return;
        }
        self.current = fit;

        match fit {
            Fit::Bar => {
                self.before = Some(Shape {
                    pos: outer.min,
                    size: inner.size(),
                    maximised,
                });
                let work = work_area(ctx, outer);
                let pos = self.bar_pos.unwrap_or_else(|| {
                    egui::pos2(
                        work.center().x - BAR_SIZE.x / 2.0,
                        work.bottom() - BAR_SIZE.y - BAR_MARGIN,
                    )
                });
                let pos = clamp_onto(work, pos, BAR_SIZE);
                dress(ctx, false);
                apply(
                    ctx,
                    Shape {
                        pos,
                        size: BAR_SIZE,
                        maximised: false,
                    },
                    maximised,
                );
                self.settling = Some((pos, SETTLE_TRIES));
            }
            Fit::Window => {
                dress(ctx, true);
                // Nothing to put back on the first frame, when the window
                // has never been the bar: it is already an ordinary window.
                if let Some(mut shape) = self.before.take() {
                    // A screen may have been unplugged while recording.
                    let work = work_area(ctx, outer);
                    shape.pos = clamp_onto(work, shape.pos, shape.size);
                    apply(ctx, shape, maximised);
                }
            }
        }
    }
}

fn clamp_onto(area: egui::Rect, pos: egui::Pos2, outer: egui::Vec2) -> egui::Pos2 {
    egui::pos2(
        pos.x
            .clamp(area.left(), (area.right() - outer.x).max(area.left())),
        pos.y
            .clamp(area.top(), (area.bottom() - outer.y).max(area.top())),
    )
}

/// The work area of the display the window is mostly on, in points.
fn work_area(ctx: &egui::Context, outer: egui::Rect) -> egui::Rect {
    let ppp = ctx.pixels_per_point();
    let centre = outer.center() * ppp;
    match screenilux_platform::display::work_area_at(centre.x as i32, centre.y as i32) {
        Some(r) => egui::Rect::from_min_size(
            egui::pos2(r.left as f32, r.top as f32) / ppp,
            egui::vec2(r.width as f32, r.height as f32) / ppp,
        ),
        None => outer,
    }
}

/// A normal window, or the bar: not resizable, above everything else so it
/// stays reachable over whatever is being recorded. Neither is decorated —
/// the app draws its own frame; see `chrome`.
fn dress(ctx: &egui::Context, normal: bool) {
    use egui::ViewportCommand as C;
    if normal {
        ctx.send_viewport_cmd(C::Resizable(true));
        ctx.send_viewport_cmd(C::WindowLevel(egui::WindowLevel::Normal));
        ctx.send_viewport_cmd(C::MinInnerSize(MIN_SIZE));
    } else {
        ctx.send_viewport_cmd(C::MinInnerSize(egui::Vec2::ZERO));
        ctx.send_viewport_cmd(C::Resizable(false));
        ctx.send_viewport_cmd(C::WindowLevel(egui::WindowLevel::AlwaysOnTop));
    }
}

fn apply(ctx: &egui::Context, shape: Shape, maximised: bool) {
    use egui::ViewportCommand as C;
    if shape.maximised {
        if !maximised {
            ctx.send_viewport_cmd(C::Maximized(true));
        }
        return;
    }
    if maximised {
        ctx.send_viewport_cmd(C::Maximized(false));
    }
    ctx.send_viewport_cmd(C::InnerSize(shape.size));
    ctx.send_viewport_cmd(C::OuterPosition(shape.pos));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_column_fits_the_smallest_window() {
        const { assert!(COLUMN <= MIN_SIZE.x) };
        const { assert!(FIRST_RUN.x >= MIN_SIZE.x && FIRST_RUN.y >= MIN_SIZE.y) };
    }

    #[test]
    fn a_window_is_pulled_back_onto_a_screen_it_no_longer_fits() {
        let work = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(1366.0, 768.0));
        // Left on a second screen that has since been unplugged.
        let pos = clamp_onto(work, egui::pos2(2400.0, 900.0), egui::vec2(800.0, 600.0));
        assert_eq!(pos, egui::pos2(566.0, 168.0));
        // One that already fits is not nudged.
        let same = clamp_onto(work, egui::pos2(40.0, 30.0), egui::vec2(800.0, 600.0));
        assert_eq!(same, egui::pos2(40.0, 30.0));
    }

    #[test]
    fn stopping_a_recording_puts_the_window_back_as_it_was() {
        let mut sizer = Sizer::new();
        assert_eq!(sizer.current, Fit::Window);
        // Standing in for the frames the real `update` would run: becoming
        // the bar banks the shape, and becoming a window again spends it.
        let was = Shape {
            pos: egui::pos2(120.0, 80.0),
            size: egui::vec2(900.0, 700.0),
            maximised: true,
        };
        sizer.before = Some(was);
        assert_eq!(sizer.before.take(), Some(was));
        assert_eq!(sizer.before, None, "the shape is put back once, not twice");
    }
}
