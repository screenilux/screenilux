//! The window's size and shape: per tab, and as the recording bar.
//!
//! The Record tab is a column of settings, so it gets a window just wide
//! enough for that column and just as tall as what it shows: the window
//! grows when a section opens and shrinks when it closes, and only once the
//! column is taller than the screen does it stop growing and scroll. Watching wants
//! every pixel, so the Play tab maximises. Editing is the other way around
//! from the column: the picture, the track and the list all want to be on
//! screen together, so Edit opens a wide window and fits its contents to
//! that, rather than the window to its contents. While recording, the
//! window can shrink to a small borderless bar at the bottom of the screen.
//!
//! Each shape remembers what the window looked like when it was left, so
//! resizing the window on a tab, or moving the bar, sticks.

use eframe::egui;

/// The Record tab's width: two screen tiles side by side plus margins, the
/// width of a compact launcher.
pub const RECORD_WIDTH: f32 = 420.0;

/// The smallest the window can be made, other than as the bar. Narrower and
/// the column's rows start to wrap; it can be short, for a short page such as
/// Settings, since it fits itself to what it shows.
pub const MIN_SIZE: egui::Vec2 = egui::vec2(400.0, 320.0);

/// The Edit tab's window when it is first opened: room for the picture at a
/// size worth clicking into, with the track and a few moments below it.
/// Clamped to the work area, so a small screen simply gets what it has.
const EDIT_SIZE: egui::Vec2 = egui::vec2(880.0, 720.0);

/// The smallest the Edit tab can be made. Below this the picture and the
/// list start fighting over the same pixels, so the window stops rather
/// than letting the editor fold up.
const MIN_EDIT: egui::Vec2 = egui::vec2(560.0, 460.0);

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
    /// The column: Record, Recordings and Settings.
    Settings,
    /// The Edit tab: wide, and as tall as the screen allows.
    Edit,
    /// The recording bar.
    Bar,
}

/// The smallest window `fit` may be made into.
fn floor(fit: Fit) -> egui::Vec2 {
    match fit {
        Fit::Edit => MIN_EDIT,
        _ => MIN_SIZE,
    }
}

pub struct Sizer {
    current: Option<Fit>,
    settings: Option<Shape>,
    edit: Option<Shape>,
    bar: Option<Shape>,
    /// What the window adds around its content, measured once it exists.
    frame: egui::Vec2,
    /// The Record tab's size last asked for, so a resize still on its way is
    /// not asked for again every frame.
    asked: Option<egui::Vec2>,
}

impl Sizer {
    pub fn new() -> Self {
        Self {
            current: None,
            settings: None,
            edit: None,
            bar: None,
            frame: FRAME_GUESS,
            asked: None,
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
            (_, Some(Shape::Maximised)) => Shape::Maximised,
            (_, Some(Shape::Normal { pos, size })) => self.within(reach, fit, pos, size),
            (Fit::Settings, None) => self.column(reach, outer),
            (Fit::Edit, None) => self.stage(reach, outer),
        };

        if fit == Fit::Bar {
            dress(ctx, false);
        } else {
            if left == Some(Fit::Bar) {
                dress(ctx, true);
            }
            // The floor differs per tab, so it travels with the shape: a
            // window squeezed down to the column's width on Record must not
            // hold the editor at that width once Edit is showing.
            ctx.send_viewport_cmd(egui::ViewportCommand::MinInnerSize(floor(fit)));
        }
        apply(ctx, shape, maximised);
    }

    /// Make the Record tab's window `content` tall, as far as the screen
    /// allows, keeping its bottom on the screen. It also stays at least as
    /// wide as the column, which can be lost when the window moves to a
    /// screen with another scale: narrower, the rows overlap.
    pub fn fit_height(&mut self, ctx: &egui::Context, content: f32) {
        if self.current != Some(Fit::Settings) {
            return;
        }
        let vp = ctx.input(|i| i.viewport().clone());
        let (Some(outer), Some(inner)) = (vp.outer_rect, vp.inner_rect) else {
            return;
        };
        if vp.maximized == Some(true) || vp.minimized == Some(true) || vp.fullscreen == Some(true) {
            return;
        }
        let reach = self.reach(work_area(ctx, outer), ctx.pixels_per_point());
        let height = content
            .ceil()
            .clamp(MIN_SIZE.y, (reach.height() - self.frame.y).max(MIN_SIZE.y));
        let width = inner.width().max(RECORD_WIDTH);
        let wanted = egui::vec2(width, height);
        if (inner.size() - wanted).abs().max_elem() < 1.0 {
            self.asked = None;
            return;
        }
        if self.asked == Some(wanted) {
            return;
        }
        self.asked = Some(wanted);
        let outer_size = wanted + self.frame;
        let pos = clamp_onto(reach, outer.min, outer_size);
        if pos != outer.min {
            ctx.send_viewport_cmd(egui::ViewportCommand::OuterPosition(pos));
        }
        ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(wanted));
    }

    fn slot(&mut self, fit: Fit) -> &mut Option<Shape> {
        match fit {
            Fit::Settings => &mut self.settings,
            Fit::Edit => &mut self.edit,
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
        self.within(reach, Fit::Settings, egui::pos2(x, reach.top()), size)
    }

    /// The Edit tab's default: a wide window grown out from where the
    /// window already is, so the jump from the column reads as the window
    /// opening up rather than leaping across the screen.
    fn stage(&self, reach: egui::Rect, outer: egui::Rect) -> Shape {
        let room = (reach.size() - self.frame).max(MIN_EDIT);
        let size = EDIT_SIZE.min(room);
        let outer_size = size + self.frame;
        let pos = outer.center() - outer_size / 2.0;
        self.within(reach, Fit::Edit, pos, size)
    }

    /// `pos` and `size`, pulled onto the screen: a remembered shape can be
    /// off-screen after a display is unplugged or the taskbar moves.
    fn within(&self, reach: egui::Rect, fit: Fit, pos: egui::Pos2, size: egui::Vec2) -> Shape {
        let min = floor(fit);
        let room = (reach.size() - self.frame).max(min);
        let size = size.min(room).max(min);
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
    match screenilux_platform::display::work_area_at(centre.x as i32, centre.y as i32) {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn editing_may_be_made_smaller_than_it_opens_but_not_crushed() {
        const { assert!(MIN_EDIT.x < EDIT_SIZE.x && MIN_EDIT.y < EDIT_SIZE.y) };
        // Wider than the column, or the editor gains nothing by having its
        // own shape at all.
        const { assert!(MIN_EDIT.x > RECORD_WIDTH) };
        assert_eq!(floor(Fit::Edit), MIN_EDIT);
        assert_eq!(floor(Fit::Settings), MIN_SIZE);
    }

    #[test]
    fn a_shape_left_on_one_tab_is_not_handed_to_another() {
        let mut sizer = Sizer::new();
        let shape = Shape::Normal {
            pos: egui::pos2(10.0, 10.0),
            size: egui::vec2(900.0, 700.0),
        };
        *sizer.slot(Fit::Edit) = Some(shape);
        assert_eq!(*sizer.slot(Fit::Settings), None);
        assert_eq!(*sizer.slot(Fit::Bar), None);
        assert_eq!(*sizer.slot(Fit::Edit), Some(shape));
    }

    #[test]
    fn a_window_too_big_for_the_screen_is_brought_back_onto_it() {
        let sizer = Sizer::new();
        let work = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(1366.0, 768.0));
        let reach = sizer.reach(work, 1.0);
        let Shape::Normal { pos, size } = sizer.stage(reach, work) else {
            panic!("the editor does not open maximised");
        };
        assert!(size.x <= EDIT_SIZE.x && size.y <= EDIT_SIZE.y);
        assert!(size.x >= MIN_EDIT.x && size.y >= MIN_EDIT.y);
        assert!(pos.x >= reach.left() && pos.y >= reach.top());
        assert!(pos.x + size.x + FRAME_GUESS.x <= reach.right() + 1.0);
        assert!(pos.y + size.y + FRAME_GUESS.y <= reach.bottom() + 1.0);
    }
}
