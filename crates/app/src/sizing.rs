//! The window's size and shape: per tab, and as the recording bar.
//!
//! The Record tab is a column of settings, so it gets a window just wide
//! enough for that column and as tall as the screen allows: everything shows
//! without scrolling and the desktop beside it stays free. Watching wants
//! every pixel, so the Play tab maximises. While recording, the window can
//! shrink to a small borderless bar at the bottom of the screen.
//!
//! Each shape remembers what the window looked like when it was left, so
//! resizing the window on a tab, or moving the bar, sticks.

use eframe::egui;

/// The Record tab's width: two screen tiles side by side plus margins, the
/// width of a compact launcher.
pub const RECORD_WIDTH: f32 = 420.0;

/// The smallest the window can be made, other than as the bar. Below this the
/// Record tab's rows start to wrap and the Play tab's controls overlap.
pub const MIN_SIZE: egui::Vec2 = egui::vec2(400.0, 520.0);

/// The recording bar: stop, the clock, pause and expand in a row.
pub const BAR_SIZE: egui::Vec2 = egui::vec2(268.0, 56.0);

/// The bar's gap above the taskbar.
const BAR_MARGIN: f32 = 12.0;

/// Title bar and borders on Windows 11 at any scale, until the real ones can
/// be measured from a window that is not maximised.
const FRAME_GUESS: egui::Vec2 = egui::vec2(16.0, 39.0);

#[derive(Debug, Clone, Copy, PartialEq)]
enum Shape {
    Maximised,
    /// Outer position and inner size, in points.
    Normal {
        pos: egui::Pos2,
        size: egui::Vec2,
    },
}

/// What the window is shaped for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fit {
    /// The Record tab.
    Settings,
    /// The Play tab.
    Stage,
    /// The recording bar.
    Bar,
}

pub struct Sizer {
    current: Option<Fit>,
    settings: Option<Shape>,
    stage: Option<Shape>,
    bar: Option<Shape>,
    /// What the window adds around its content, measured once it exists.
    frame: egui::Vec2,
}

impl Sizer {
    pub fn new() -> Self {
        Self {
            current: None,
            settings: None,
            stage: None,
            bar: None,
            frame: FRAME_GUESS,
        }
    }

    /// Shape the window for `fit` when it changes, remembering the shape the
    /// window had for the previous one.
    pub fn update(&mut self, ctx: &egui::Context, fit: Fit) {
        let vp = ctx.input(|i| i.viewport().clone());
        let (Some(outer), Some(inner)) = (vp.outer_rect, vp.inner_rect) else {
            return;
        };
        if vp.minimized == Some(true) {
            return;
        }
        let maximised = vp.maximized == Some(true);
        let frame = outer.size() - inner.size();
        // The bar has no frame to measure, and for a frame or two after it
        // the title bar is still on its way back.
        if !maximised && self.current != Some(Fit::Bar) && frame.y > 0.0 {
            self.frame = frame.max(egui::Vec2::ZERO);
        }
        if self.current == Some(fit) {
            return;
        }

        let now = if maximised {
            Shape::Maximised
        } else {
            Shape::Normal {
                pos: outer.min,
                size: inner.size(),
            }
        };
        let left = self.current.replace(fit);
        if let Some(left) = left {
            *self.slot(left) = Some(now);
        }

        let work = work_area(ctx, outer);
        let reach = self.reach(work, ctx.pixels_per_point());
        let shape = match (fit, *self.slot(fit)) {
            (Fit::Bar, Some(Shape::Normal { pos, .. })) => bar_within(work, pos),
            (Fit::Bar, _) => bar_within(
                work,
                egui::pos2(
                    work.center().x - BAR_SIZE.x / 2.0,
                    work.bottom() - BAR_SIZE.y - BAR_MARGIN,
                ),
            ),
            (_, Some(Shape::Maximised)) | (Fit::Stage, None) => Shape::Maximised,
            (_, Some(Shape::Normal { pos, size })) => self.within(reach, pos, size),
            (Fit::Settings, None) => self.column(reach, outer),
        };

        if fit == Fit::Bar {
            dress(ctx, false);
        } else if left == Some(Fit::Bar) {
            dress(ctx, true);
        }
        apply(ctx, shape, maximised);
    }

    fn slot(&mut self, fit: Fit) -> &mut Option<Shape> {
        match fit {
            Fit::Settings => &mut self.settings,
            Fit::Stage => &mut self.stage,
            Fit::Bar => &mut self.bar,
        }
    }

    /// Where the window's outer rectangle may go on `work`.
    ///
    /// Windows draws a one-pixel border and leaves the rest of the resize
    /// border invisible, on the left, right and bottom. Snapped windows let
    /// that invisible part hang past the work area; without it, a window
    /// sized to the work area stops seven pixels short of the taskbar.
    fn reach(&self, work: egui::Rect, ppp: f32) -> egui::Rect {
        let invisible = (self.frame.x / 2.0 - 1.0 / ppp).max(0.0);
        work.expand2(egui::vec2(invisible, 0.0))
            .with_max_y(work.bottom() + invisible)
    }

    /// The Record tab's default: the full height of the work area, centred
    /// where the window already is.
    fn column(&self, reach: egui::Rect, outer: egui::Rect) -> Shape {
        let size = egui::vec2(RECORD_WIDTH, reach.height() - self.frame.y);
        let width = size.x + self.frame.x;
        let x = (outer.center().x - width / 2.0)
            .clamp(reach.left(), (reach.right() - width).max(reach.left()));
        self.within(reach, egui::pos2(x, reach.top()), size)
    }

    /// `pos` and `size`, pulled onto the screen: a remembered shape can be
    /// off-screen after a display is unplugged or the taskbar moves.
    fn within(&self, reach: egui::Rect, pos: egui::Pos2, size: egui::Vec2) -> Shape {
        let room = (reach.size() - self.frame).max(MIN_SIZE);
        let size = size.min(room).max(MIN_SIZE);
        Shape::Normal {
            pos: clamp_onto(reach, pos, size + self.frame),
            size,
        }
    }
}

/// The bar at `pos`, kept on the work area.
fn bar_within(work: egui::Rect, pos: egui::Pos2) -> Shape {
    Shape::Normal {
        pos: clamp_onto(work, pos, BAR_SIZE),
        size: BAR_SIZE,
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
    match screenilux_win::display::work_area_at(centre.x as i32, centre.y as i32) {
        Some(r) => egui::Rect::from_min_size(
            egui::pos2(r.left as f32, r.top as f32) / ppp,
            egui::vec2(r.width as f32, r.height as f32) / ppp,
        ),
        None => outer,
    }
}

/// A normal window, or the bar: no title bar, not resizable, above
/// everything else so it stays reachable over whatever is being recorded.
fn dress(ctx: &egui::Context, normal: bool) {
    use egui::ViewportCommand as C;
    if normal {
        ctx.send_viewport_cmd(C::Decorations(true));
        ctx.send_viewport_cmd(C::Resizable(true));
        ctx.send_viewport_cmd(C::WindowLevel(egui::WindowLevel::Normal));
        ctx.send_viewport_cmd(C::MinInnerSize(MIN_SIZE));
    } else {
        ctx.send_viewport_cmd(C::MinInnerSize(egui::Vec2::ZERO));
        ctx.send_viewport_cmd(C::Decorations(false));
        ctx.send_viewport_cmd(C::Resizable(false));
        ctx.send_viewport_cmd(C::WindowLevel(egui::WindowLevel::AlwaysOnTop));
    }
}

fn apply(ctx: &egui::Context, shape: Shape, maximised: bool) {
    match shape {
        Shape::Maximised => {
            if !maximised {
                ctx.send_viewport_cmd(egui::ViewportCommand::Maximized(true));
            }
        }
        Shape::Normal { pos, size } => {
            if maximised {
                ctx.send_viewport_cmd(egui::ViewportCommand::Maximized(false));
            }
            ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(size));
            ctx.send_viewport_cmd(egui::ViewportCommand::OuterPosition(pos));
        }
    }
}
