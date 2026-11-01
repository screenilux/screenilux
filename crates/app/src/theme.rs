//! The look of the app: Windows' own typefaces and icons, the brand's teal
//! palette in a light and a dark set, and the few custom widgets the stock
//! ones do not cover.
//!
//! The brand is eight shades, each with one job. Depth comes from tone, not
//! shadow: the window is Ground, grouped things sit on Surface, and Mist draws
//! the lines where layers meet and marks what is chosen. Action carries the
//! words of the main buttons; Signal only ever marks something live or
//! chosen, and never sits behind text.

use std::sync::Arc;

use eframe::egui::{
    self, Color32, CornerRadius, FontData, FontDefinitions, FontFamily, FontId, Stroke, TextStyle,
    Theme, Visuals,
};

/// Glyphs from Segoe Fluent Icons (Windows 11), which keeps the codepoints of
/// Segoe MDL2 Assets (Windows 10), so either font serves.
#[cfg(windows)]
pub mod icon {
    pub const PLAY: &str = "\u{E768}";
    pub const PAUSE: &str = "\u{E769}";
    pub const FOLDER: &str = "\u{ED25}";
    pub const EXPAND: &str = "\u{E740}";
    pub const SEARCH: &str = "\u{E721}";
    pub const CAMERA: &str = "\u{E714}";
    pub const MIC: &str = "\u{E720}";
    pub const SPEAKER: &str = "\u{E767}";
    pub const WARNING: &str = "\u{E7BA}";
    pub const REFRESH: &str = "\u{E72C}";
    pub const MONITOR: &str = "\u{E7F4}";
    pub const TEXT: &str = "\u{E8D2}";
    pub const CHEVRON_DOWN: &str = "\u{E70D}";
    pub const CHEVRON_UP: &str = "\u{E70E}";
    pub const FULL_SCREEN: &str = "\u{E740}";
    pub const LEAVE_FULL_SCREEN: &str = "\u{E73F}";
    pub const CHECK: &str = "\u{E73E}";
    pub const COPY: &str = "\u{E8C8}";
    pub const LINK: &str = "\u{E71B}";
    pub const LOCK: &str = "\u{E72E}";
    pub const OPEN: &str = "\u{E8A7}";
}

/// The same icons as symbols in egui's own fonts, which every system has.
#[cfg(not(windows))]
pub mod icon {
    pub const PLAY: &str = "\u{25B6}";
    pub const PAUSE: &str = "\u{23F8}";
    pub const FOLDER: &str = "\u{1F4C2}";
    pub const EXPAND: &str = "\u{26F6}";
    pub const SEARCH: &str = "\u{1F50D}";
    pub const CAMERA: &str = "\u{1F4F7}";
    pub const MIC: &str = "\u{1F3A4}";
    pub const SPEAKER: &str = "\u{1F50A}";
    pub const WARNING: &str = "\u{26A0}";
    pub const REFRESH: &str = "\u{1F504}";
    pub const MONITOR: &str = "\u{1F5B5}";
    pub const TEXT: &str = "\u{1F5DB}";
    pub const CHEVRON_DOWN: &str = "\u{23F7}";
    pub const CHEVRON_UP: &str = "\u{23F6}";
    pub const FULL_SCREEN: &str = "\u{26F6}";
    pub const LEAVE_FULL_SCREEN: &str = "\u{1F5D7}";
    pub const CHECK: &str = "\u{2714}";
    pub const COPY: &str = "\u{29C9}";
    pub const LINK: &str = "\u{1F517}";
    pub const LOCK: &str = "\u{1F512}";
    pub const OPEN: &str = "\u{2197}";
}

/// The named colours. Two sets, picked by the system theme.
#[derive(Debug, Clone, Copy)]
pub struct Palette {
    /// Ground: the window background.
    pub base: Color32,
    /// Surface: rows, tabs, the video card, fields.
    pub surface: Color32,
    /// Mist: lines between layers, switches when off, chosen fills.
    pub stroke: Color32,
    /// Ink.
    pub text: Color32,
    /// Ink at 68%, for hints and details.
    pub weak: Color32,
    /// Main buttons and switches that are on. Carries white text.
    pub action: Color32,
    /// Something live or chosen: the record dot, chosen borders, the
    /// playhead. Never behind text.
    pub signal: Color32,
    /// The quieter signal: paused, the second screen on the timeline.
    pub soft: Color32,
    /// Behind video.
    pub stage: Color32,
    /// The recording bar's background, dark in both themes so it reads over
    /// whatever is on screen.
    pub bar: Color32,
}

pub const LIGHT: Palette = Palette {
    base: Color32::from_rgb(0xF3, 0xFF, 0xFE),
    surface: Color32::from_rgb(0xE1, 0xF7, 0xF7),
    stroke: Color32::from_rgb(0xA5, 0xE8, 0xE7),
    text: Color32::from_rgb(0x01, 0x26, 0x25),
    weak: Color32::from_rgb(0x4E, 0x6B, 0x6A),
    action: Color32::from_rgb(0x04, 0x73, 0x6F),
    signal: Color32::from_rgb(0x06, 0x9A, 0x95),
    soft: Color32::from_rgb(0x07, 0xBE, 0xB8),
    stage: Color32::from_rgb(0xE1, 0xF7, 0xF7),
    bar: Color32::from_rgb(0x01, 0x26, 0x25),
};

pub const DARK: Palette = Palette {
    base: Color32::from_rgb(0x01, 0x26, 0x25),
    surface: Color32::from_rgb(0x0D, 0x33, 0x32),
    stroke: Color32::from_rgb(0x16, 0x54, 0x52),
    text: Color32::from_rgb(0xE1, 0xF7, 0xF7),
    weak: Color32::from_rgb(0x99, 0xB4, 0xB4),
    action: Color32::from_rgb(0x04, 0x73, 0x6F),
    signal: Color32::from_rgb(0x2D, 0xA8, 0xA4),
    soft: Color32::from_rgb(0x36, 0xCB, 0xC9),
    stage: Color32::from_rgb(0x0D, 0x33, 0x32),
    bar: Color32::from_rgb(0x0D, 0x33, 0x32),
};

/// Text on the recording bar and on Action, in either theme.
pub const ON_DARK: Color32 = Color32::from_rgb(0xF3, 0xFF, 0xFE);

pub fn palette(ui: &egui::Ui) -> Palette {
    if ui.visuals().dark_mode { DARK } else { LIGHT }
}

pub fn palette_of(ctx: &egui::Context) -> Palette {
    if ctx.theme() == Theme::Dark {
        DARK
    } else {
        LIGHT
    }
}

/// Semibold, for labels and buttons.
pub fn strong() -> FontFamily {
    FontFamily::Name("semibold".into())
}

/// Bold, for titles and the main button.
pub fn bold() -> FontFamily {
    FontFamily::Name("bold".into())
}

/// Fonts and both themes. Called once at startup.
pub fn install(ctx: &egui::Context) {
    ctx.set_fonts(fonts());
    for (theme, p) in [(Theme::Dark, DARK), (Theme::Light, LIGHT)] {
        ctx.style_mut_of(theme, |style| {
            style.visuals = visuals(theme, p);
            style.text_styles = [
                (TextStyle::Heading, FontId::new(17.0, bold())),
                (TextStyle::Body, FontId::new(14.0, FontFamily::Proportional)),
                (TextStyle::Button, FontId::new(14.0, strong())),
                (
                    TextStyle::Small,
                    FontId::new(12.0, FontFamily::Proportional),
                ),
                (
                    TextStyle::Monospace,
                    FontId::new(13.0, FontFamily::Monospace),
                ),
            ]
            .into();
            let s = &mut style.spacing;
            s.item_spacing = egui::vec2(8.0, 8.0);
            s.button_padding = egui::vec2(14.0, 6.0);
            s.interact_size.y = 32.0;
            s.combo_height = 240.0;
            // The interface's own words are not content to copy, and a text
            // cursor over a heading or a clock reads as a bug. Text worth
            // copying (the recordings' own, in the player) is selectable by
            // its own means.
            style.interaction.selectable_labels = false;
        });
    }
}

/// The system's own interface font, so the app reads as part of it: Segoe UI
/// on Windows, Noto Sans or DejaVu Sans on Linux. Read from the system's
/// fonts rather than bundled, so the program stays small, and falling back to
/// egui's own fonts where none is found.
fn fonts() -> FontDefinitions {
    let mut defs = FontDefinitions::default();
    let mut load = |name: &str, files: &[std::path::PathBuf]| -> bool {
        for f in files {
            if let Ok(bytes) = std::fs::read(f) {
                defs.font_data
                    .insert(name.to_string(), Arc::new(FontData::from_owned(bytes)));
                return true;
            }
        }
        false
    };
    let faces = system_fonts();
    let regular = load("ui", &faces.regular);
    let semibold = load("ui-semibold", &faces.semibold);
    let heavy = load("ui-bold", &faces.bold);
    let icons = load("icons", &faces.icons);

    let mut fallback = defs
        .families
        .get(&FontFamily::Proportional)
        .cloned()
        .unwrap_or_default();
    // Without a system icon font the icons come from egui's own fonts. Its
    // icon font draws them as interface symbols, where its emoji font draws
    // some, such as the microphone, as an empty box; so the icon font first.
    if !icons {
        fallback.sort_by_key(|name| !name.contains("emoji-icon-font"));
    }
    let family = |first: Option<&str>| -> Vec<String> {
        let mut v: Vec<String> = first.map(str::to_string).into_iter().collect();
        if icons {
            v.push("icons".into());
        }
        v.extend(fallback.iter().cloned());
        v
    };
    let body = regular.then_some("ui");
    defs.families.insert(FontFamily::Proportional, family(body));
    let semi = semibold.then_some("ui-semibold").or(body);
    defs.families.insert(strong(), family(semi));
    defs.families
        .insert(bold(), family(heavy.then_some("ui-bold").or(semi)));
    defs
}

/// Where to look for each weight of the interface font, best first.
struct Faces {
    regular: Vec<std::path::PathBuf>,
    semibold: Vec<std::path::PathBuf>,
    bold: Vec<std::path::PathBuf>,
    icons: Vec<std::path::PathBuf>,
}

#[cfg(windows)]
fn system_fonts() -> Faces {
    let dir = std::env::var_os("WINDIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| "C:\\Windows".into())
        .join("Fonts");
    let at = |files: &[&str]| files.iter().map(|f| dir.join(f)).collect();
    Faces {
        regular: at(&["segoeui.ttf"]),
        semibold: at(&["seguisb.ttf", "segoeuib.ttf"]),
        bold: at(&["segoeuib.ttf", "seguisb.ttf"]),
        icons: at(&["SegoeIcons.ttf", "segmdl2.ttf"]),
    }
}

#[cfg(not(windows))]
fn system_fonts() -> Faces {
    // Debian and Ubuntu, then Fedora and Arch layouts.
    let dirs = [
        "/usr/share/fonts/truetype/noto",
        "/usr/share/fonts/google-noto",
        "/usr/share/fonts/noto",
        "/usr/share/fonts/truetype/dejavu",
        "/usr/share/fonts/dejavu-sans-fonts",
        "/usr/share/fonts/TTF",
    ];
    let at = |files: &[&str]| {
        files
            .iter()
            .flat_map(|f| dirs.iter().map(move |d| std::path::Path::new(d).join(f)))
            .collect()
    };
    Faces {
        regular: at(&["NotoSans-Regular.ttf", "DejaVuSans.ttf"]),
        semibold: at(&[
            "NotoSans-SemiBold.ttf",
            "NotoSans-Medium.ttf",
            "DejaVuSans-Bold.ttf",
        ]),
        bold: at(&["NotoSans-Bold.ttf", "DejaVuSans-Bold.ttf"]),
        // egui's own fonts carry the symbols used as icons here.
        icons: Vec::new(),
    }
}

fn visuals(theme: Theme, p: Palette) -> Visuals {
    let mut v = match theme {
        Theme::Dark => Visuals::dark(),
        Theme::Light => Visuals::light(),
    };
    let control = CornerRadius::same(10);
    v.panel_fill = p.base;
    v.window_fill = p.base;
    v.window_stroke = Stroke::new(1.0, p.stroke);
    v.window_corner_radius = CornerRadius::same(16);
    v.menu_corner_radius = CornerRadius::same(12);
    // No shadows anywhere: depth comes from tone and lines.
    v.window_shadow = egui::Shadow::NONE;
    v.popup_shadow = egui::Shadow::NONE;
    v.faint_bg_color = p.surface;
    v.extreme_bg_color = p.surface;
    v.text_edit_bg_color = Some(p.surface);
    v.override_text_color = Some(p.text);
    v.weak_text_color = Some(p.weak);
    v.hyperlink_color = p.action;
    v.warn_fg_color = p.text;
    v.error_fg_color = p.text;
    v.selection.bg_fill = p.stroke;
    v.selection.stroke = Stroke::new(1.0, p.action);
    v.slider_trailing_fill = true;

    let w = &mut v.widgets;
    w.noninteractive.bg_fill = p.surface;
    w.noninteractive.weak_bg_fill = p.surface;
    w.noninteractive.bg_stroke = Stroke::new(1.0, p.stroke);
    w.noninteractive.fg_stroke = Stroke::new(1.0, p.text);
    w.noninteractive.corner_radius = control;

    // Buttons at rest are outlined on the ground; hover steps one tone in.
    w.inactive.bg_fill = p.base;
    w.inactive.weak_bg_fill = p.base;
    w.inactive.bg_stroke = Stroke::new(1.0, p.stroke);
    w.inactive.fg_stroke = Stroke::new(1.0, p.text);
    w.inactive.corner_radius = control;

    w.hovered.bg_fill = p.surface;
    w.hovered.weak_bg_fill = p.surface;
    w.hovered.bg_stroke = Stroke::new(1.0, p.stroke);
    w.hovered.fg_stroke = Stroke::new(1.0, p.text);
    w.hovered.corner_radius = control;
    w.hovered.expansion = 0.0;

    w.active.bg_fill = p.stroke;
    w.active.weak_bg_fill = p.stroke;
    w.active.bg_stroke = Stroke::new(1.0, p.action);
    w.active.fg_stroke = Stroke::new(1.0, p.text);
    w.active.corner_radius = control;
    w.active.expansion = 0.0;

    w.open.bg_fill = p.surface;
    w.open.weak_bg_fill = p.surface;
    w.open.bg_stroke = Stroke::new(1.0, p.stroke);
    w.open.corner_radius = control;
    v
}

/// `a` moved `t` of the way towards `b`.
pub fn blend(a: Color32, b: Color32, t: f32) -> Color32 {
    let mix = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * t).round() as u8;
    Color32::from_rgb(mix(a.r(), b.r()), mix(a.g(), b.g()), mix(a.b(), b.b()))
}

/// The app's mark: two screens overlapping at a corner, joined by the live
/// recording dot, as in `docs/logo.svg`. Drawn rather than loaded so it is
/// crisp at any size, and in ink so it follows the light or dark theme.
pub fn logo(ui: &mut egui::Ui, size: f32) {
    let p = palette(ui);
    let (rect, _) = ui.allocate_exact_size(egui::vec2(size, size), egui::Sense::hover());
    // The logo's own 64 grid, fitted to `size`.
    let k = size / 64.0;
    let at = |x: f32, y: f32| rect.min + egui::vec2(x * k, y * k);
    let corner = CornerRadius::same((8.0 * k).round() as u8);
    let painter = ui.painter();
    painter.rect_filled(
        egui::Rect::from_min_max(at(3.0, 7.0), at(43.0, 36.0)),
        corner,
        p.soft,
    );
    painter.rect_filled(
        egui::Rect::from_min_max(at(21.0, 28.0), at(61.0, 57.0)),
        corner,
        p.text,
    );
    // The gap around the dot, cut back to the window's own colour.
    painter.circle_filled(at(32.0, 32.0), 10.5 * k, p.base);
    painter.circle_filled(at(32.0, 32.0), 6.5 * k, p.signal);
}

/// An outlined pill button, the quiet kind.
pub fn outline(text: impl Into<egui::WidgetText>) -> egui::Button<'static> {
    egui::Button::new(text)
        .corner_radius(CornerRadius::same(20))
        .min_size(egui::vec2(0.0, 40.0))
}

/// The main action: a full-width pill in Action with white bold text and,
/// for recording, a dot before the words.
pub fn big_button(ui: &mut egui::Ui, text: &str, dot: bool) -> egui::Response {
    let p = palette(ui);
    let width = ui.available_width();
    let (rect, response) = ui.allocate_exact_size(egui::vec2(width, 56.0), egui::Sense::click());
    response.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, text));
    if ui.is_rect_visible(rect) {
        let fill = if response.is_pointer_button_down_on() {
            blend(p.action, p.text, 0.2)
        } else if response.hovered() {
            blend(p.action, ON_DARK, 0.1)
        } else {
            p.action
        };
        let painter = ui.painter();
        painter.rect_filled(rect, CornerRadius::same(28), fill);
        let galley = painter.layout_no_wrap(text.to_string(), FontId::new(16.0, bold()), ON_DARK);
        let dot_w = if dot { 22.0 } else { 0.0 };
        let start = rect.center().x - (galley.size().x + dot_w) / 2.0;
        if dot {
            painter.circle_filled(egui::pos2(start + 6.0, rect.center().y), 6.0, ON_DARK);
        }
        painter.galley(
            egui::pos2(start + dot_w, rect.center().y - galley.size().y / 2.0),
            galley,
            ON_DARK,
        );
        if response.has_focus() {
            painter.rect_stroke(
                rect.expand(2.0),
                CornerRadius::same(30),
                Stroke::new(2.0, p.action),
                egui::StrokeKind::Outside,
            );
        }
    }
    response
}

/// A round button with an icon, for players and the recording bar.
pub fn round_button(
    ui: &mut egui::Ui,
    glyph: &str,
    diameter: f32,
    fill: Color32,
    ink: Color32,
    tip: &str,
) -> egui::Response {
    let (rect, response) =
        ui.allocate_exact_size(egui::vec2(diameter, diameter), egui::Sense::click());
    response.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, tip));
    if ui.is_rect_visible(rect) {
        let fill = if response.hovered() {
            if fill == Color32::TRANSPARENT {
                ink.gamma_multiply(0.14)
            } else {
                blend(fill, Color32::WHITE, 0.1)
            }
        } else {
            fill
        };
        let painter = ui.painter();
        painter.circle_filled(rect.center(), diameter / 2.0, fill);
        painter.text(
            rect.center(),
            egui::Align2::CENTER_CENTER,
            glyph,
            FontId::proportional(diameter * 0.36),
            ink,
        );
        if response.has_focus() {
            painter.circle_stroke(
                rect.center(),
                diameter / 2.0 + 2.0,
                Stroke::new(1.5, palette(ui).action),
            );
        }
    }
    response.on_hover_text(tip)
}

/// The switch track and knob alone, painted into `rect`.
fn paint_switch(ui: &egui::Ui, rect: egui::Rect, on: bool, t: f32, enabled: bool) {
    let p = palette(ui);
    let fill = if on { p.action } else { p.stroke };
    let fill = if enabled {
        fill
    } else {
        fill.gamma_multiply(0.5)
    };
    let painter = ui.painter();
    painter.rect_filled(rect, CornerRadius::same(10), fill);
    let x = egui::lerp(rect.left() + 10.0..=rect.right() - 10.0, t);
    painter.circle_filled(egui::pos2(x, rect.center().y), 8.0, ON_DARK);
}

/// A full-width setting: its label on the left, the switch on the right.
pub fn setting(ui: &mut egui::Ui, on: &mut bool, label: &str) -> egui::Response {
    let p = palette(ui);
    let width = ui.available_width();
    let (rect, mut response) =
        ui.allocate_exact_size(egui::vec2(width, 44.0), egui::Sense::click());
    if response.clicked() {
        *on = !*on;
        response.mark_changed();
    }
    response.widget_info(|| {
        egui::WidgetInfo::selected(egui::Role::CheckBox, ui.is_enabled(), *on, label)
    });
    if ui.is_rect_visible(rect) {
        let t = ui.ctx().animate_bool_responsive(response.id, *on);
        let track_rect = egui::Rect::from_min_size(
            egui::pos2(rect.right() - 36.0, rect.center().y - 10.0),
            egui::vec2(36.0, 20.0),
        );
        paint_switch(ui, track_rect, *on, t, ui.is_enabled());
        focus_ring(ui, &response, track_rect, 12);
        let colour = if ui.is_enabled() { p.text } else { p.weak };
        ui.painter().text(
            egui::pos2(rect.left(), rect.center().y),
            egui::Align2::LEFT_CENTER,
            label,
            FontId::proportional(14.0),
            colour,
        );
    }
    response
}

/// A source the recording can use, as a rounded row: a round badge with its
/// icon, its name over the device, and a switch. Clicking anywhere on the row
/// flips it.
pub fn source_row(
    ui: &mut egui::Ui,
    glyph: &str,
    title: &str,
    detail: &str,
    on: bool,
) -> egui::Response {
    let p = palette(ui);
    let width = ui.available_width();
    let (rect, response) = ui.allocate_exact_size(egui::vec2(width, 60.0), egui::Sense::click());
    response.widget_info(|| {
        egui::WidgetInfo::selected(egui::Role::CheckBox, ui.is_enabled(), on, title)
    });
    if ui.is_rect_visible(rect) {
        let enabled = ui.is_enabled();
        let fill = if response.hovered() && enabled {
            p.stroke
        } else {
            p.surface
        };
        let painter = ui.painter();
        painter.rect_filled(rect, CornerRadius::same(16), fill);
        let badge = egui::pos2(rect.left() + 32.0, rect.center().y);
        painter.circle_filled(badge, 18.0, if on { p.stroke } else { p.base });
        let ink = if on { p.action } else { p.weak };
        if glyph == icon::MIC && !cfg!(windows) {
            // egui's fonts draw the microphone as an empty box, so it is drawn.
            paint_microphone(painter, badge, ink);
        } else {
            painter.text(
                badge,
                egui::Align2::CENTER_CENTER,
                glyph,
                FontId::proportional(16.0),
                ink,
            );
        }
        painter.text(
            egui::pos2(rect.left() + 62.0, rect.center().y - 9.0),
            egui::Align2::LEFT_CENTER,
            title,
            FontId::new(14.0, strong()),
            if enabled { p.text } else { p.weak },
        );
        painter.text(
            egui::pos2(rect.left() + 62.0, rect.center().y + 10.0),
            egui::Align2::LEFT_CENTER,
            detail,
            FontId::proportional(12.0),
            p.weak,
        );
        let t = ui.ctx().animate_bool_responsive(response.id, on);
        let track_rect = egui::Rect::from_min_size(
            egui::pos2(rect.right() - 50.0, rect.center().y - 10.0),
            egui::vec2(36.0, 20.0),
        );
        paint_switch(ui, track_rect, on, t, enabled);
        painter.text(
            egui::pos2(track_rect.left() - 10.0, rect.center().y),
            egui::Align2::RIGHT_CENTER,
            if on { "On" } else { "Off" },
            FontId::new(12.0, strong()),
            if on { p.text } else { p.weak },
        );
        focus_ring(ui, &response, rect, 18);
    }
    response
}

/// A microphone in outline, about the size of a 16px icon: a capsule, the
/// cradle around its lower half, and a stand.
fn paint_microphone(painter: &egui::Painter, centre: egui::Pos2, ink: Color32) {
    let stroke = Stroke::new(1.5, ink);
    let capsule =
        egui::Rect::from_center_size(centre - egui::vec2(0.0, 2.5), egui::vec2(6.0, 10.0));
    painter.rect_stroke(
        capsule,
        CornerRadius::same(3),
        stroke,
        egui::StrokeKind::Middle,
    );
    let cradle: Vec<egui::Pos2> = (0..=12)
        .map(|i| {
            let a = std::f32::consts::PI * i as f32 / 12.0;
            centre + egui::vec2(-5.5 * a.cos(), 0.5 + 5.0 * a.sin())
        })
        .collect();
    painter.add(egui::Shape::line(cradle, stroke));
    painter.line_segment(
        [centre + egui::vec2(0.0, 5.5), centre + egui::vec2(0.0, 8.0)],
        stroke,
    );
}

/// A row of choices in a tinted track, the chosen one lifted onto the ground
/// with a fine line. Returns the index clicked, if any.
pub fn segmented(ui: &mut egui::Ui, labels: &[&str], chosen: usize) -> Option<usize> {
    let p = palette(ui);
    let font = FontId::new(13.0, strong());
    let galleys: Vec<_> = labels
        .iter()
        .map(|l| {
            ui.painter()
                .layout_no_wrap(l.to_string(), font.clone(), p.text)
        })
        .collect();
    let widths: Vec<f32> = galleys.iter().map(|g| g.size().x + 24.0).collect();
    let total = widths.iter().sum::<f32>() + 6.0;
    let (outer, _) = ui.allocate_exact_size(egui::vec2(total, 38.0), egui::Sense::hover());
    ui.painter()
        .rect_filled(outer, CornerRadius::same(12), p.surface);
    let mut x = outer.left() + 3.0;
    let mut picked = None;
    for (i, (label, w)) in labels.iter().zip(&widths).enumerate() {
        let rect = egui::Rect::from_min_size(
            egui::pos2(x, outer.top() + 3.0),
            egui::vec2(*w, outer.height() - 6.0),
        );
        x += w;
        let response = ui
            .interact(
                rect,
                ui.scope_id().with(("segment", label)),
                egui::Sense::click(),
            )
            .on_hover_cursor(egui::CursorIcon::PointingHand);
        response.widget_info(|| {
            egui::WidgetInfo::selected(egui::Role::Button, true, i == chosen, *label)
        });
        if i == chosen {
            ui.painter().rect(
                rect,
                CornerRadius::same(9),
                p.base,
                Stroke::new(1.0, p.stroke),
                egui::StrokeKind::Inside,
            );
        }
        let ink = if i == chosen || response.hovered() {
            p.text
        } else {
            p.weak
        };
        ui.painter().text(
            rect.center(),
            egui::Align2::CENTER_CENTER,
            *label,
            font.clone(),
            ink,
        );
        focus_ring(ui, &response, rect, 10);
        if response.clicked() {
            picked = Some(i);
        }
    }
    picked
}

/// A tick box: an empty rounded square, or one filled with Action and a
/// white tick.
pub fn paint_checkbox(ui: &egui::Ui, rect: egui::Rect, on: bool, enabled: bool) {
    let p = palette(ui);
    let painter = ui.painter();
    if on {
        let fill = if enabled {
            p.action
        } else {
            p.action.gamma_multiply(0.5)
        };
        painter.rect_filled(rect, CornerRadius::same(6), fill);
        painter.text(
            rect.center(),
            egui::Align2::CENTER_CENTER,
            icon::CHECK,
            FontId::proportional(12.0),
            ON_DARK,
        );
    } else {
        painter.rect(
            rect,
            CornerRadius::same(6),
            p.base,
            Stroke::new(2.0, p.weak),
            egui::StrokeKind::Inside,
        );
    }
}

fn focus_ring(ui: &egui::Ui, response: &egui::Response, rect: egui::Rect, radius: u8) {
    if response.has_focus() {
        ui.painter().rect_stroke(
            rect.expand(2.0),
            CornerRadius::same(radius),
            Stroke::new(2.0, palette(ui).action),
            egui::StrokeKind::Outside,
        );
    }
}

/// A short status line: icon and text in ink.
pub fn notice(ui: &mut egui::Ui, text: impl Into<String>) {
    let p = palette(ui);
    ui.horizontal_wrapped(|ui| {
        ui.label(egui::RichText::new(icon::WARNING).color(p.text));
        ui.label(egui::RichText::new(text.into()).color(p.text));
    });
}

/// A thin bar showing how far along something is, `done` from 0 to 1.
pub fn progress(ui: &mut egui::Ui, done: f32, width: f32) {
    let p = palette(ui);
    let (rect, _) = ui.allocate_exact_size(egui::vec2(width.max(8.0), 6.0), egui::Sense::hover());
    let painter = ui.painter();
    painter.rect_filled(rect, CornerRadius::same(3), p.stroke);
    let filled = rect.with_max_x(rect.left() + rect.width() * done.clamp(0.0, 1.0));
    if filled.width() > 0.5 {
        painter.rect_filled(filled, CornerRadius::same(3), p.signal);
    }
}

/// Weak explanatory text under a control.
pub fn hint(ui: &mut egui::Ui, text: &str) {
    ui.label(egui::RichText::new(text).size(13.0).color(palette(ui).weak));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blending_reaches_both_ends() {
        let (a, b) = (Color32::from_rgb(0, 0, 0), Color32::from_rgb(200, 100, 50));
        assert_eq!(blend(a, b, 0.0), a);
        assert_eq!(blend(a, b, 1.0), b);
        assert_eq!(blend(a, b, 0.5), Color32::from_rgb(100, 50, 25));
    }

    fn ratio(a: Color32, b: Color32) -> f64 {
        fn lum(c: Color32) -> f64 {
            let f = |v: u8| {
                let v = v as f64 / 255.0;
                if v <= 0.03928 {
                    v / 12.92
                } else {
                    ((v + 0.055) / 1.055).powf(2.4)
                }
            };
            0.2126 * f(c.r()) + 0.7152 * f(c.g()) + 0.0722 * f(c.b())
        }
        let (x, y) = (lum(a), lum(b));
        (x.max(y) + 0.05) / (x.min(y) + 0.05)
    }

    /// Text on each surface must stay readable: WCAG AA wants 4.5:1.
    #[test]
    fn text_contrast_meets_aa_in_both_themes() {
        for p in [DARK, LIGHT] {
            for bg in [p.base, p.surface] {
                assert!(ratio(p.text, bg) >= 4.5, "text on {bg:?}");
                assert!(ratio(p.weak, bg) >= 4.5, "weak text on {bg:?}");
            }
            assert!(ratio(ON_DARK, p.action) >= 4.5, "label on a main button");
            assert!(ratio(ON_DARK, p.bar) >= 4.5, "clock on the recording bar");
        }
    }

    /// A chosen screen's border and the record dot must stand out from the
    /// ground: WCAG wants 3:1 for parts of controls.
    #[test]
    fn signal_stands_out_from_the_ground() {
        for p in [DARK, LIGHT] {
            assert!(ratio(p.signal, p.base) >= 3.0, "signal on {:?}", p.base);
        }
    }
}
