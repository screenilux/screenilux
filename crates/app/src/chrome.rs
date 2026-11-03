//! The window's own frame, drawn by the app.
//!
//! Windows' title bar is a grey strip carrying a second copy of the icon and
//! the window's title, directly above a header that already says what the
//! app is. Two bars saying the same thing, one of them in someone else's
//! colours. So the native frame is off and this draws what it was for: a
//! strip to drag, a way to minimise, maximise and close, and an edge to
//! pull.
//!
//! What the frame gives for free has to be given back by hand. Dragging the
//! strip moves the window and double-clicking it maximises, both through
//! the same commands the title bar used. Resizing needs a border that is no
//! longer there, so the edges and corners carry invisible grips.

use eframe::egui;

use crate::theme;

/// How far in from each edge counts as grabbing it. Windows' own invisible
/// resize border is about this wide.
const GRIP: f32 = 6.0;

/// The buttons at the top right: minimise, maximise or restore, and close.
///
/// Laid out right to left, so close sits in the corner where Windows puts
/// it and the hand goes where it already knows to go.
pub fn buttons(ui: &mut egui::Ui, ctx: &egui::Context) {
    let maximised = ctx.input(|i| i.viewport().maximized.unwrap_or(false));
    if button(ui, Mark::Close, true) {
        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
    }
    if button(ui, Mark::Maximise(maximised), false) {
        ctx.send_viewport_cmd(egui::ViewportCommand::Maximized(!maximised));
    }
    if button(ui, Mark::Minimise, false) {
        ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(true));
    }
}

enum Mark {
    Minimise,
    /// True when the window is already maximised, which flips the glyph.
    Maximise(bool),
    Close,
}

/// One window button: a glyph drawn rather than set, so it looks the same
/// whatever fonts the system has, and the same size as its neighbours.
fn button(ui: &mut egui::Ui, mark: Mark, danger: bool) -> bool {
    let p = theme::palette(ui);
    let (rect, response) = ui.allocate_exact_size(egui::vec2(38.0, 30.0), egui::Sense::click());
    let name = match mark {
        Mark::Minimise => "Minimise",
        Mark::Maximise(true) => "Restore down",
        Mark::Maximise(false) => "Maximise",
        Mark::Close => "Close",
    };
    response.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, name));
    if !ui.is_rect_visible(rect) {
        return response.clicked();
    }

    let hot = response.hovered();
    if hot {
        let fill = if danger {
            egui::Color32::from_rgb(0xC4, 0x2B, 0x1C)
        } else {
            p.surface
        };
        ui.painter()
            .rect_filled(rect, egui::CornerRadius::same(8), fill);
    }
    let ink = if hot && danger {
        theme::ON_DARK
    } else {
        p.text
    };
    let c = rect.center();
    let line = egui::Stroke::new(1.2, ink);
    let painter = ui.painter();
    match mark {
        Mark::Minimise => {
            painter.line_segment(
                [egui::pos2(c.x - 5.0, c.y), egui::pos2(c.x + 5.0, c.y)],
                line,
            );
        }
        Mark::Maximise(false) => {
            painter.rect_stroke(
                egui::Rect::from_center_size(c, egui::vec2(10.0, 10.0)),
                egui::CornerRadius::same(2),
                line,
                egui::StrokeKind::Inside,
            );
        }
        Mark::Maximise(true) => {
            // Two overlapping squares, the way Windows draws "restore".
            painter.rect_stroke(
                egui::Rect::from_min_size(egui::pos2(c.x - 5.0, c.y - 2.0), egui::vec2(9.0, 9.0)),
                egui::CornerRadius::same(2),
                line,
                egui::StrokeKind::Inside,
            );
            painter.rect_stroke(
                egui::Rect::from_min_size(egui::pos2(c.x - 2.0, c.y - 5.0), egui::vec2(9.0, 9.0)),
                egui::CornerRadius::same(2),
                line,
                egui::StrokeKind::Inside,
            );
        }
        Mark::Close => {
            painter.line_segment(
                [
                    egui::pos2(c.x - 5.0, c.y - 5.0),
                    egui::pos2(c.x + 5.0, c.y + 5.0),
                ],
                line,
            );
            painter.line_segment(
                [
                    egui::pos2(c.x + 5.0, c.y - 5.0),
                    egui::pos2(c.x - 5.0, c.y + 5.0),
                ],
                line,
            );
        }
    }
    response.clicked()
}

/// Make `rect` behave like a title bar: drag it to move the window, double
/// click it to maximise or restore.
///
/// Called with whatever is left of the header once its buttons have taken
/// their room, so dragging never fights a control for the same press.
pub fn grab(ui: &egui::Ui, ctx: &egui::Context, rect: egui::Rect) {
    if rect.width() <= 0.0 {
        return;
    }
    let response = ui.interact(
        rect,
        egui::Id::unique("window-grab"),
        egui::Sense::click_and_drag(),
    );
    if response.double_clicked() {
        let maximised = ctx.input(|i| i.viewport().maximized.unwrap_or(false));
        ctx.send_viewport_cmd(egui::ViewportCommand::Maximized(!maximised));
    } else if response.drag_started() {
        ctx.send_viewport_cmd(egui::ViewportCommand::StartDrag);
    }
}

/// The invisible border a frameless window still needs to be resizable.
///
/// Eight zones: four edges and four corners, each handing the drag back to
/// Windows so the resize feels like any other window's, snapping included.
pub fn edges(ctx: &egui::Context) {
    use egui::ResizeDirection as D;
    if ctx.input(|i| i.viewport().maximized.unwrap_or(false)) {
        return;
    }
    let screen = ctx.content_rect();
    if screen.width() <= GRIP * 3.0 || screen.height() <= GRIP * 3.0 {
        return;
    }
    let c = GRIP * 2.0;
    // Corners first: they overlap the edges and must win the press.
    let zones = [
        (
            egui::Rect::from_min_size(screen.left_top(), egui::vec2(c, c)),
            D::NorthWest,
            egui::CursorIcon::ResizeNwSe,
        ),
        (
            egui::Rect::from_min_size(
                egui::pos2(screen.right() - c, screen.top()),
                egui::vec2(c, c),
            ),
            D::NorthEast,
            egui::CursorIcon::ResizeNeSw,
        ),
        (
            egui::Rect::from_min_size(
                egui::pos2(screen.left(), screen.bottom() - c),
                egui::vec2(c, c),
            ),
            D::SouthWest,
            egui::CursorIcon::ResizeNeSw,
        ),
        (
            egui::Rect::from_min_size(
                egui::pos2(screen.right() - c, screen.bottom() - c),
                egui::vec2(c, c),
            ),
            D::SouthEast,
            egui::CursorIcon::ResizeNwSe,
        ),
        (
            egui::Rect::from_min_size(screen.left_top(), egui::vec2(screen.width(), GRIP)),
            D::North,
            egui::CursorIcon::ResizeVertical,
        ),
        (
            egui::Rect::from_min_size(
                egui::pos2(screen.left(), screen.bottom() - GRIP),
                egui::vec2(screen.width(), GRIP),
            ),
            D::South,
            egui::CursorIcon::ResizeVertical,
        ),
        (
            egui::Rect::from_min_size(screen.left_top(), egui::vec2(GRIP, screen.height())),
            D::West,
            egui::CursorIcon::ResizeHorizontal,
        ),
        (
            egui::Rect::from_min_size(
                egui::pos2(screen.right() - GRIP, screen.top()),
                egui::vec2(GRIP, screen.height()),
            ),
            D::East,
            egui::CursorIcon::ResizeHorizontal,
        ),
    ];

    for (i, (rect, dir, cursor)) in zones.into_iter().enumerate() {
        let response = egui::Area::new(egui::Id::unique(("resize", i)))
            .order(egui::Order::Foreground)
            .fixed_pos(rect.min)
            .interactable(true)
            .show(ctx, |ui| {
                ui.allocate_response(rect.size(), egui::Sense::click_and_drag())
            })
            .inner;
        if response.hovered() || response.is_pointer_button_down_on() {
            ctx.set_cursor_icon(cursor);
        }
        if response.drag_started() {
            ctx.send_viewport_cmd(egui::ViewportCommand::BeginResize(dir));
        }
    }
}
