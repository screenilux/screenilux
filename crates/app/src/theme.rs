//! The look of the app: Windows' own typefaces and icons, a palette for each
//! system theme, and the few custom widgets the stock ones do not cover.
//!
//! The vocabulary is a broadcast control room. Red is a tally light and means
//! one thing only, "this is being recorded"; amber is paused; a single signal
//! blue marks what can be acted on. Everything else stays neutral, so those
//! three always read at a glance.

use std::sync::Arc;

use eframe::egui::{
    self, Color32, CornerRadius, FontData, FontDefinitions, FontFamily, FontId, Stroke, TextStyle,
    Theme, Visuals,
};

/// Glyphs from Segoe Fluent Icons (Windows 11), which keeps the codepoints of
/// Segoe MDL2 Assets (Windows 10), so either font serves.
pub mod icon {
    pub const RECORD: &str = "\u{E7C8}";
    pub const STOP: &str = "\u{E71A}";
    pub const PLAY: &str = "\u{E768}";
    pub const PAUSE: &str = "\u{E769}";
    pub const OPEN: &str = "\u{E8E5}";
    pub const FOLDER: &str = "\u{ED25}";
    pub const EXPAND: &str = "\u{E740}";
    pub const SEARCH: &str = "\u{E721}";
    pub const COPY: &str = "\u{E8C8}";
    pub const CAMERA: &str = "\u{E722}";
    pub const MIC: &str = "\u{E720}";
    pub const WARNING: &str = "\u{E7BA}";
    pub const REFRESH: &str = "\u{E72C}";
    pub const CANCEL: &str = "\u{E711}";
    pub const MONITOR: &str = "\u{E7F4}";
    pub const TEXT: &str = "\u{E8D2}";
}

/// The named colours. Two sets, picked by the system theme.
#[derive(Debug, Clone, Copy)]
pub struct Palette {
    /// Window background.
    pub base: Color32,
    /// Grouped content sitting on the base.
    pub surface: Color32,
    /// Controls at rest.
    pub raised: Color32,
    pub stroke: Color32,
    pub text: Color32,
    pub weak: Color32,
    /// Recording, and nothing else.
    pub tally: Color32,
    pub paused: Color32,
    /// What can be acted on: selection, playhead, focus.
    pub signal: Color32,
    /// Behind video, where any tint would colour the picture.
    pub stage: Color32,
}

pub const DARK: Palette = Palette {
    base: Color32::from_rgb(0x1C, 0x1F, 0x24),
    surface: Color32::from_rgb(0x24, 0x28, 0x2E),
    raised: Color32::from_rgb(0x2E, 0x33, 0x3A),
    stroke: Color32::from_rgb(0x3A, 0x40, 0x48),
    text: Color32::from_rgb(0xE7, 0xE9, 0xEC),
    weak: Color32::from_rgb(0x9B, 0xA3, 0xAD),
    tally: Color32::from_rgb(0xE5, 0x48, 0x4D),
    paused: Color32::from_rgb(0xF0, 0xA5, 0x3A),
    signal: Color32::from_rgb(0x4C, 0x8D, 0xF6),
    stage: Color32::from_rgb(0x14, 0x16, 0x1A),
};

pub const LIGHT: Palette = Palette {
    base: Color32::from_rgb(0xF2, 0xF3, 0xF5),
    surface: Color32::from_rgb(0xFF, 0xFF, 0xFF),
    raised: Color32::from_rgb(0xE9, 0xEC, 0xEF),
    stroke: Color32::from_rgb(0xD3, 0xD8, 0xDE),
    text: Color32::from_rgb(0x1C, 0x1F, 0x24),
    weak: Color32::from_rgb(0x5E, 0x66, 0x72),
    tally: Color32::from_rgb(0xD6, 0x34, 0x3A),
    paused: Color32::from_rgb(0xB8, 0x76, 0x0F),
    signal: Color32::from_rgb(0x2D, 0x6B, 0xE0),
    stage: Color32::from_rgb(0x20, 0x23, 0x28),
};

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

/// Semibold, for headings and primary buttons.
pub fn strong() -> FontFamily {
    FontFamily::Name("semibold".into())
}

/// Light, for the large clock figures.
pub fn light() -> FontFamily {
    FontFamily::Name("light".into())
}

/// Fonts and both themes. Called once at startup.
pub fn install(ctx: &egui::Context) {
    ctx.set_fonts(fonts());
    for (theme, p) in [(Theme::Dark, DARK), (Theme::Light, LIGHT)] {
        ctx.style_mut_of(theme, |style| {
            style.visuals = visuals(theme, p);
            style.text_styles = [
                (TextStyle::Heading, FontId::new(17.0, strong())),
                (TextStyle::Body, FontId::new(14.0, FontFamily::Proportional)),
                (
                    TextStyle::Button,
                    FontId::new(14.0, FontFamily::Proportional),
                ),
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
            s.button_padding = egui::vec2(12.0, 5.0);
            s.interact_size.y = 28.0;
            s.combo_height = 240.0;
            // The interface's own words are not content to copy, and a text
            // cursor over a heading or a clock reads as a bug. Text worth
            // copying (the recordings' own, in the player) is selectable by
            // its own means.
            style.interaction.selectable_labels = false;
        });
    }
}

/// Segoe UI throughout, falling back to egui's own fonts on a machine
/// without it. Read from the Windows fonts folder rather than bundled, so the
/// executable stays small and the app reads as part of Windows.
fn fonts() -> FontDefinitions {
    let mut defs = FontDefinitions::default();
    let dir = std::env::var_os("WINDIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| "C:\\Windows".into())
        .join("Fonts");
    let mut load = |name: &str, files: &[&str]| -> bool {
        for f in files {
            if let Ok(bytes) = std::fs::read(dir.join(f)) {
                defs.font_data
                    .insert(name.to_string(), Arc::new(FontData::from_owned(bytes)));
                return true;
            }
        }
        false
    };
    let regular = load("segoe", &["segoeui.ttf"]);
    let semibold = load("segoe-semibold", &["seguisb.ttf", "segoeuib.ttf"]);
    let thin = load("segoe-light", &["segoeuil.ttf", "segoeuisl.ttf"]);
    let icons = load("icons", &["SegoeIcons.ttf", "segmdl2.ttf"]);

    let fallback = defs
        .families
        .get(&FontFamily::Proportional)
        .cloned()
        .unwrap_or_default();
    let family = |first: Option<&str>| -> Vec<String> {
        let mut v: Vec<String> = first.map(str::to_string).into_iter().collect();
        if icons {
            v.push("icons".into());
        }
        v.extend(fallback.iter().cloned());
        v
    };
    let body = regular.then_some("segoe");
    defs.families.insert(FontFamily::Proportional, family(body));
    defs.families.insert(
        strong(),
        family(semibold.then_some("segoe-semibold").or(body)),
    );
    defs.families
        .insert(light(), family(thin.then_some("segoe-light").or(body)));
    defs
}

fn visuals(theme: Theme, p: Palette) -> Visuals {
    let mut v = match theme {
        Theme::Dark => Visuals::dark(),
        Theme::Light => Visuals::light(),
    };
    let control = CornerRadius::same(6);
    v.panel_fill = p.base;
    v.window_fill = p.surface;
    v.window_stroke = Stroke::new(1.0, p.stroke);
    v.window_corner_radius = CornerRadius::same(10);
    v.menu_corner_radius = CornerRadius::same(8);
    v.faint_bg_color = p.surface;
    v.extreme_bg_color = p.surface;
    v.text_edit_bg_color = Some(p.surface);
    v.override_text_color = Some(p.text);
    v.weak_text_color = Some(p.weak);
    v.hyperlink_color = p.signal;
    v.warn_fg_color = p.paused;
    v.error_fg_color = p.tally;
    v.selection.bg_fill = p.signal.gamma_multiply(0.35);
    v.selection.stroke = Stroke::new(1.0, p.signal);
    v.slider_trailing_fill = true;

    let w = &mut v.widgets;
    w.noninteractive.bg_fill = p.surface;
    w.noninteractive.weak_bg_fill = p.surface;
    w.noninteractive.bg_stroke = Stroke::new(1.0, p.stroke);
    w.noninteractive.fg_stroke = Stroke::new(1.0, p.text);
    w.noninteractive.corner_radius = control;

    w.inactive.bg_fill = p.raised;
    w.inactive.weak_bg_fill = p.raised;
    w.inactive.bg_stroke = Stroke::NONE;
    w.inactive.fg_stroke = Stroke::new(1.0, p.text);
    w.inactive.corner_radius = control;

    let hover = blend(p.raised, p.text, 0.08);
    w.hovered.bg_fill = hover;
    w.hovered.weak_bg_fill = hover;
    w.hovered.bg_stroke = Stroke::new(1.0, p.stroke);
    w.hovered.fg_stroke = Stroke::new(1.0, p.text);
    w.hovered.corner_radius = control;
    w.hovered.expansion = 0.0;

    let press = blend(p.raised, p.signal, 0.25);
    w.active.bg_fill = press;
    w.active.weak_bg_fill = press;
    w.active.bg_stroke = Stroke::new(1.0, p.signal);
    w.active.fg_stroke = Stroke::new(1.0, p.text);
    w.active.corner_radius = control;
    w.active.expansion = 0.0;

    w.open.bg_fill = press;
    w.open.weak_bg_fill = press;
    w.open.corner_radius = control;
    v
}

/// `a` moved `t` of the way towards `b`.
pub fn blend(a: Color32, b: Color32, t: f32) -> Color32 {
    let mix = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * t).round() as u8;
    Color32::from_rgb(mix(a.r(), b.r()), mix(a.g(), b.g()), mix(a.b(), b.b()))
}

/// A filled call-to-action button in `fill`, with white semibold text.
pub fn primary(text: impl Into<String>, fill: Color32) -> egui::Button<'static> {
    egui::Button::new(
        egui::RichText::new(text.into())
            .font(FontId::new(14.0, strong()))
            .color(Color32::WHITE),
    )
    .fill(fill)
    .min_size(egui::vec2(0.0, 34.0))
}

/// An on/off switch with a label, the Windows idiom for settings that take
/// effect immediately, where a checkbox reads like part of a form to submit.
pub fn switch(ui: &mut egui::Ui, on: &mut bool, label: &str) -> egui::Response {
    let p = palette(ui);
    let track = egui::vec2(36.0, 20.0);
    let text = egui::WidgetText::from(label).into_galley(
        ui,
        Some(egui::TextWrapMode::Extend),
        f32::INFINITY,
        TextStyle::Body,
    );
    let size = egui::vec2(track.x + 10.0 + text.size().x, track.y.max(text.size().y));
    let (rect, mut response) = ui.allocate_exact_size(size, egui::Sense::click());
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
            egui::pos2(rect.min.x, rect.center().y - track.y / 2.0),
            track,
        );
        let enabled = ui.is_enabled();
        let fill = if *on {
            p.signal
        } else {
            blend(
                p.raised,
                p.text,
                if response.hovered() { 0.12 } else { 0.05 },
            )
        };
        let fill = if enabled {
            fill
        } else {
            fill.gamma_multiply(0.5)
        };
        let painter = ui.painter();
        painter.rect(
            track_rect,
            CornerRadius::same(10),
            fill,
            if *on {
                Stroke::NONE
            } else {
                Stroke::new(1.0, p.stroke)
            },
            egui::StrokeKind::Inside,
        );
        let x = egui::lerp(track_rect.left() + 10.0..=track_rect.right() - 10.0, t);
        let knob = if *on { Color32::WHITE } else { p.weak };
        painter.circle_filled(egui::pos2(x, track_rect.center().y), 6.0, knob);
        if response.has_focus() {
            painter.rect_stroke(
                track_rect.expand(2.0),
                CornerRadius::same(12),
                Stroke::new(1.5, p.signal),
                egui::StrokeKind::Outside,
            );
        }
        let colour = if enabled { p.text } else { p.weak };
        painter.galley(
            egui::pos2(
                track_rect.right() + 10.0,
                rect.center().y - text.size().y / 2.0,
            ),
            text,
            colour,
        );
    }
    response
}

/// A titled group: a semibold heading over content on a surface panel.
pub fn section<R>(ui: &mut egui::Ui, title: &str, add: impl FnOnce(&mut egui::Ui) -> R) -> R {
    let p = palette(ui);
    ui.label(egui::RichText::new(title).font(FontId::new(14.0, strong())));
    egui::Frame::new()
        .fill(p.surface)
        .stroke(Stroke::new(1.0, p.stroke))
        .corner_radius(CornerRadius::same(8))
        .inner_margin(egui::Margin::symmetric(14, 12))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            add(ui)
        })
        .inner
}

/// A warning line: icon and text in the paused colour.
pub fn notice(ui: &mut egui::Ui, text: impl Into<String>) {
    let p = palette(ui);
    ui.horizontal_wrapped(|ui| {
        ui.label(egui::RichText::new(icon::WARNING).color(p.paused));
        ui.label(egui::RichText::new(text.into()).color(p.paused));
    });
}

/// Lay `add` out at most `max` wide, centred in what is available, so a
/// settings page stays a readable column in a wide window.
pub fn centered<R>(ui: &mut egui::Ui, max: f32, add: impl FnOnce(&mut egui::Ui) -> R) -> R {
    let avail = ui.available_width();
    let width = avail.min(max);
    ui.horizontal_top(|ui| {
        ui.add_space(((avail - width) / 2.0).max(0.0));
        ui.vertical(|ui| {
            ui.set_width(width);
            add(ui)
        })
        .inner
    })
    .inner
}

/// Weak explanatory text under a control.
pub fn hint(ui: &mut egui::Ui, text: &str) {
    ui.label(egui::RichText::new(text).small().weak());
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

    /// Text on each surface must stay readable: WCAG AA wants 4.5:1.
    #[test]
    fn text_contrast_meets_aa_in_both_themes() {
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
        let ratio = |a: Color32, b: Color32| {
            let (x, y) = (lum(a), lum(b));
            (x.max(y) + 0.05) / (x.min(y) + 0.05)
        };
        for p in [DARK, LIGHT] {
            for bg in [p.base, p.surface, p.raised] {
                assert!(ratio(p.text, bg) >= 4.5, "text on {bg:?}");
                assert!(ratio(p.weak, bg) >= 4.5, "weak text on {bg:?}");
            }
            assert!(
                ratio(Color32::WHITE, p.tally) >= 3.0,
                "label on the record button"
            );
            assert!(
                ratio(Color32::WHITE, p.signal) >= 3.0,
                "label on a primary button"
            );
        }
    }
}
