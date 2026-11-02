//! The first time through the Edit tab: a short tour that points at things.
//!
//! Three surfaces, each with its own job, and nothing on screen says which
//! is which until someone tries. So on the first visit the tour dims the
//! window, lights one surface at a time and says what it is for. The light
//! slides from one to the next rather than jumping, because watching it
//! travel is what tells you the two places are related.
//!
//! It runs once. Finishing it or skipping it both count, and the answer is
//! kept with the rest of the preferences, so it never greets the same
//! person twice.

use eframe::egui;

use crate::theme;

/// How fast the light travels between stops, as a fraction closed per
/// second. Fast enough not to be waited on, slow enough to be followed.
const GLIDE: f32 = 9.0;

/// How far the card sits from the thing it is describing.
const GAP: f32 = 14.0;

/// One thing the tour points at.
pub struct Stop<'a> {
    /// What to light up. `None` puts the card in the middle and lights
    /// nothing, for a stop that is about the tab as a whole.
    pub spot: Option<egui::Rect>,
    pub title: &'a str,
    pub body: &'a str,
}

pub struct Tour {
    step: usize,
    /// Where the light is now, which chases where it is going.
    at: Option<egui::Rect>,
    running: bool,
}

impl Default for Tour {
    fn default() -> Self {
        Self::new()
    }
}

impl Tour {
    pub fn new() -> Self {
        Self {
            step: 0,
            at: None,
            running: false,
        }
    }

    /// Start the tour from the top.
    pub fn start(&mut self) {
        self.step = 0;
        self.at = None;
        self.running = true;
    }

    pub fn running(&self) -> bool {
        self.running
    }

    /// Draw the tour over everything else. Returns true on the frame it
    /// ends, whether it was finished or skipped, so the caller can write
    /// that down and not offer it again.
    pub fn show(&mut self, ctx: &egui::Context, stops: &[Stop]) -> bool {
        if !self.running || stops.is_empty() {
            return false;
        }
        if self.step >= stops.len() {
            return self.stop();
        }
        let stop = &stops[self.step];
        let screen = ctx.content_rect();
        let p = theme::palette_for(ctx);

        // The light chases the stop rather than cutting to it, so the eye is
        // carried from one surface to the next instead of hunting for it.
        let target = stop.spot.map(|r| r.expand(6.0));
        let shown = match (self.at, target) {
            (Some(now), Some(want)) => {
                let dt = ctx.input(|i| i.stable_dt).clamp(0.0, 0.1);
                let k = 1.0 - (-GLIDE * dt).exp();
                let next = egui::Rect::from_min_max(
                    now.min + (want.min - now.min) * k,
                    now.max + (want.max - now.max) * k,
                );
                if (next.min - want.min).length() > 0.4 || (next.max - want.max).length() > 0.4 {
                    ctx.request_repaint();
                }
                Some(next)
            }
            (None, Some(want)) => Some(want),
            (_, None) => None,
        };
        self.at = shown;

        let mut next = false;
        let mut skip = false;

        egui::Area::new(egui::Id::unique("tour"))
            .fixed_pos(screen.min)
            .order(egui::Order::Foreground)
            .interactable(true)
            .show(ctx, |ui| {
                // The veil takes the clicks that land on it, so moving the
                // tour on never also presses whatever is underneath.
                ui.allocate_rect(screen, egui::Sense::click());
                let painter = ui.painter();
                let veil = egui::Color32::from_black_alpha(150);
                match shown {
                    // Four panes around the lit rectangle: the painter has no
                    // hole to punch, so the dark is drawn as what is left.
                    Some(r) => {
                        for part in [
                            egui::Rect::from_min_max(
                                screen.min,
                                egui::pos2(screen.right(), r.top()),
                            ),
                            egui::Rect::from_min_max(
                                egui::pos2(screen.left(), r.bottom()),
                                screen.max,
                            ),
                            egui::Rect::from_min_max(
                                egui::pos2(screen.left(), r.top()),
                                egui::pos2(r.left(), r.bottom()),
                            ),
                            egui::Rect::from_min_max(
                                egui::pos2(r.right(), r.top()),
                                egui::pos2(screen.right(), r.bottom()),
                            ),
                        ] {
                            painter.rect_filled(part, 0.0, veil);
                        }
                        painter.rect_stroke(
                            r,
                            egui::CornerRadius::same(12),
                            egui::Stroke::new(2.0, p.signal),
                            egui::StrokeKind::Outside,
                        );
                    }
                    None => {
                        painter.rect_filled(screen, 0.0, veil);
                    }
                }
            });

        let card = egui::Area::new(egui::Id::unique("tour-card"))
            .order(egui::Order::Tooltip)
            .fixed_pos(self.card_at(screen, shown))
            .show(ctx, |ui| {
                ui.set_max_width(330.0);
                egui::Frame::new()
                    .fill(p.base)
                    .stroke(egui::Stroke::new(1.0, p.stroke))
                    .corner_radius(16)
                    .inner_margin(18)
                    .shadow(egui::epaint::Shadow {
                        offset: [0, 8],
                        blur: 28,
                        spread: 0,
                        color: egui::Color32::from_black_alpha(90),
                    })
                    .show(ui, |ui| {
                        ui.label(
                            egui::RichText::new(stop.title)
                                .font(egui::FontId::new(15.0, theme::bold()))
                                .color(p.text),
                        );
                        ui.add_space(6.0);
                        ui.label(egui::RichText::new(stop.body).size(13.0).color(p.weak));
                        ui.add_space(14.0);
                        ui.horizontal(|ui| {
                            ui.label(
                                egui::RichText::new(format!(
                                    "{} of {}",
                                    self.step + 1,
                                    stops.len()
                                ))
                                .size(11.0)
                                .color(p.weak),
                            );
                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    let last = self.step + 1 == stops.len();
                                    let label = if last { "Done" } else { "Next" };
                                    let go = egui::Button::new(
                                        egui::RichText::new(label)
                                            .family(theme::strong())
                                            .color(theme::ON_DARK),
                                    )
                                    .fill(p.action)
                                    .corner_radius(10)
                                    .min_size(egui::vec2(76.0, 32.0));
                                    if ui.add(go).clicked() {
                                        next = true;
                                    }
                                    if !last && ui.add(theme::outline("Skip")).clicked() {
                                        skip = true;
                                    }
                                },
                            );
                        });
                    });
            });
        // Clicking anywhere outside the card moves on too, since that is
        // what someone reaching for the thing being pointed at will do.
        let outside = ctx.input(|i| i.pointer.any_pressed())
            && !ctx.input(|i| {
                i.pointer
                    .interact_pos()
                    .is_some_and(|pos| card.response.rect.contains(pos))
            });

        if skip {
            return self.stop();
        }
        if next || outside {
            self.step += 1;
            if self.step >= stops.len() {
                return self.stop();
            }
        }
        false
    }

    fn stop(&mut self) -> bool {
        self.running = false;
        self.at = None;
        self.step = 0;
        true
    }

    /// Below the lit rectangle when there is room, above it when there is
    /// not, and in the middle when nothing is lit.
    fn card_at(&self, screen: egui::Rect, shown: Option<egui::Rect>) -> egui::Pos2 {
        const CARD: egui::Vec2 = egui::vec2(330.0, 190.0);
        let Some(r) = shown else {
            return screen.center() - CARD / 2.0;
        };
        let x = (r.center().x - CARD.x / 2.0).clamp(
            screen.left() + 12.0,
            (screen.right() - CARD.x - 12.0).max(12.0),
        );
        let below = r.bottom() + GAP;
        let y = if below + CARD.y <= screen.bottom() - 12.0 {
            below
        } else {
            (r.top() - GAP - CARD.y).max(screen.top() + 12.0)
        };
        egui::pos2(x, y)
    }
}
