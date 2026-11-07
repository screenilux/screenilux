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

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use eframe::egui;
use screenilux_core::edit::{
    Area, Backdrop, EditList, Effect, MIN_SPAN, Place, Problem, Tool, trim_zero,
};
use screenilux_core::redact;
use screenilux_core::session::{MANIFEST_NAME, Manifest};
use screenilux_core::text::TextIndex;
use screenilux_player::SharedClock;
use screenilux_player::text::{INDEX_INTERVAL, Indexer};

use crate::exporting::{Exports, Source};
use crate::player_view::PlayerView;
use crate::theme::{self, icon};
use crate::track;

/// The room the timeline wants around itself inside the picture's card.
const TIMELINE_PAD: f32 = 26.0;

/// The least list worth scrolling, when the panel is short.
const LIST_MIN: f32 = 120.0;

/// The effect menu: wide enough for a name and what it does on one line
/// each, and tall enough to show six without scrolling.
const MENU_W: f32 = 420.0;
const MENU_LIST_H: f32 = 330.0;
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
    asking: Option<Asking>,
    /// The timeline's own state: what the pointer is doing to it, the
    /// stretch marked out on it, and which effect is lit. The list reads
    /// the last of those, so selecting in one place selects in the other.
    track: track::State,
    saved: bool,
    /// Set by the Save button, acted on once the borrows are released.
    wants_save: bool,
    /// Where the timeline was drawn last frame, for the tour to point at.
    track_at: Option<egui::Rect>,
    /// Writing the recording out: which format, how far along, and what
    /// came of it.
    exports: Exports,
    /// Reading the screen to find what to blur, while it runs.
    redacting: Option<Indexer>,
    /// What the last search for private details came to.
    redact_note: Option<String>,
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
            asking: None,
            track: track::State::default(),
            saved: true,
            wants_save: false,
            track_at: None,
            exports: Exports::default(),
            redacting: None,
            redact_note: None,
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
        self.share_path(player);
    }

    /// Hand over the pointer's path and where the screens were, so a zoom
    /// can follow it and clicks can be marked.
    fn share_path(&self, player: &mut PlayerView) {
        let (path, desks) = match self.loaded.as_ref() {
            Some(l) => (
                l.manifest.pointer.clone(),
                l.manifest
                    .screens
                    .iter()
                    .map(|s| {
                        (
                            s.file.clone(),
                            screenilux_core::Rect::new(s.left, s.top, s.width, s.height),
                        )
                    })
                    .collect(),
            ),
            None => (None, Vec::new()),
        };
        player.set_path(path, desks);
        let (pointer, shortcuts) = match self.loaded.as_ref() {
            Some(l) => (
                (!l.manifest.cursor_in_picture && l.manifest.pointer.is_some())
                    .then_some(l.edits.pointer),
                if l.edits.hide_shortcuts {
                    Vec::new()
                } else {
                    l.manifest.shortcuts.clone()
                },
            ),
            None => (None, Vec::new()),
        };
        player.set_drawn(pointer, shortcuts);
    }

    pub fn follow(&mut self, player: &PlayerView) {
        let path = player.shareable();
        if self.loaded.as_ref().map(|l| &l.path) == path.as_ref() {
            return;
        }
        match path {
            None => {
                self.loaded = None;
                self.track.clear();
                self.asking = None;
                self.redacting = None;
                self.redact_note = None;
            }
            Some(path) => self.adopt(&path, player),
        }
    }

    /// Read the edits saved beside `path`, if there are any. A recording
    /// with none is simply one nobody has edited yet.
    fn adopt(&mut self, path: &Path, player: &PlayerView) {
        self.error = None;
        self.track.clear();
        self.asking = None;
        self.redacting = None;
        self.redact_note = None;
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

    /// The band the picture's card has to leave under itself. The track
    /// grows a lane at a time as effects start overlapping, so the answer
    /// depends on what is loaded rather than being one number forever.
    pub fn timeline_height(&self) -> f32 {
        let lanes = self
            .loaded
            .as_ref()
            .map_or(1, |l| track::lanes_needed(&l.edits));
        track::height(lanes) + TIMELINE_PAD
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
        self.redact_row(ui, p, player);
        ui.add_space(14.0);
        let Some(clock) = player.clock() else { return };
        let rest = ui.available_height().max(LIST_MIN);
        self.list(ui, p, clock, rest);
    }

    /// Finding what should not be shown and blurring it: the button, the
    /// screen being read while that runs, and what it came to.
    fn redact_row(&mut self, ui: &mut egui::Ui, p: &theme::Palette, player: &mut PlayerView) {
        if self.loaded.is_none() {
            return;
        }
        if let Some(result) = self.redacting.as_ref().and_then(Indexer::finished) {
            self.redacting = None;
            match result {
                Ok(index) => {
                    player.keep_text(&index);
                    self.blur_private(&index, player);
                }
                Err(e) => self.redact_note = Some(format!("Could not read the screen: {e}")),
            }
        }

        ui.add_space(10.0);
        self.pointer_row(ui, p);
        self.click_zooms(ui);
        ui.add_space(6.0);
        if let Some(reading) = &self.redacting {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label(
                    egui::RichText::new(format!(
                        "Reading the screen for private details, {:.0}%",
                        reading.progress() * 100.0
                    ))
                    .size(13.0)
                    .color(p.weak),
                );
            });
            ui.ctx()
                .request_repaint_after(std::time::Duration::from_millis(250));
        } else if ui
            .add(theme::outline(format!(
                "{}  Blur private details",
                icon::LOCK
            )))
            .on_hover_text(
                "Find email addresses, keys, passwords and card numbers on screen, and blur \
                 each one where and while it shows",
            )
            .clicked()
        {
            self.redact_note = None;
            match player.stored_text() {
                // Read before: no need to read it again.
                Some(index) => self.blur_private(&index, player),
                None => {
                    if let Some(recording) = player.recording() {
                        let ctx = ui.ctx().clone();
                        self.redacting = Some(Indexer::start(
                            recording,
                            INDEX_INTERVAL,
                            Arc::new(move || ctx.request_repaint()),
                        ));
                    }
                }
            }
        }
        if let Some(note) = &self.redact_note {
            ui.add_space(4.0);
            theme::hint(ui, note);
        }
    }

    /// How the pointer and the shortcuts are drawn, for a recording that left
    /// them out of the picture. Older recordings have the pointer in the
    /// picture, and nothing here applies to them.
    fn pointer_row(&mut self, ui: &mut egui::Ui, p: &theme::Palette) {
        let Some(l) = self.loaded.as_mut() else {
            return;
        };
        let drawn = !l.manifest.cursor_in_picture && l.manifest.pointer.is_some();
        let has_keys = !l.manifest.shortcuts.is_empty();
        if !drawn && !has_keys {
            return;
        }
        let before = (l.edits.pointer, l.edits.hide_shortcuts);
        if drawn {
            let style = &mut l.edits.pointer;
            let mut show = !style.hidden;
            theme::setting(ui, &mut show, "Show the pointer");
            style.hidden = !show;
            if show {
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new("Size").size(13.0).color(p.weak));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        const SIZES: [f32; 3] = [1.0, 1.5, 2.2];
                        let chosen = SIZES
                            .iter()
                            .position(|s| (s - style.size).abs() < 0.05)
                            .unwrap_or(1);
                        if let Some(i) = theme::segmented(ui, &["Small", "Medium", "Large"], chosen)
                        {
                            style.size = SIZES[i];
                        }
                    });
                });
                theme::setting(ui, &mut style.smooth, "Smooth its movement");
                theme::setting(ui, &mut style.hide_idle, "Hide it while it rests");
            }
        }
        if has_keys {
            let mut show = !l.edits.hide_shortcuts;
            theme::setting(ui, &mut show, "Show shortcuts pressed");
            l.edits.hide_shortcuts = !show;
        }
        if (l.edits.pointer, l.edits.hide_shortcuts) != before {
            self.saved = false;
        }
        ui.add_space(6.0);
    }

    /// The button that zooms in on every click, and what it did.
    fn click_zooms(&mut self, ui: &mut egui::Ui) {
        let Some(l) = self.loaded.as_mut() else {
            return;
        };
        let clicks = l
            .manifest
            .pointer
            .as_ref()
            .map_or(0, |track| track.clicks.len());
        let button = theme::outline(format!("{}  Zoom in on clicks", icon::SEARCH));
        let response = ui
            .add_enabled(clicks > 0, button)
            .on_hover_text("Add a zoom that follows the pointer wherever you clicked")
            .on_disabled_hover_text("This recording kept no clicks to zoom in on");
        if !response.clicked() {
            return;
        }
        let screens: Vec<(String, screenilux_core::Rect)> = l
            .manifest
            .screens
            .iter()
            .map(|s| {
                (
                    s.file.clone(),
                    screenilux_core::Rect::new(s.left, s.top, s.width, s.height),
                )
            })
            .collect();
        let Some(track) = l.manifest.pointer.as_ref() else {
            return;
        };
        let planned =
            screenilux_core::autozoom::plan(track, &screens, l.duration, &l.edits.effects);
        for zoom in &planned {
            l.edits.add_on(
                zoom.start,
                zoom.end,
                zoom.tool(true),
                Some(zoom.screen.clone()),
            );
        }
        self.redact_note = Some(match planned.len() {
            0 => "Every click is in a zoom already.".to_string(),
            1 => "Added 1 zoom. Drag it on the timeline to change when it happens.".to_string(),
            n => format!("Added {n} zooms. Drag one on the timeline to change when it happens."),
        });
        if !planned.is_empty() {
            self.saved = false;
        }
    }

    /// Blur everything private in `index`, where and while it shows, unless a
    /// blur already covers it. Each is an ordinary Blur effect, so one that
    /// is not private after all is deleted like any other.
    fn blur_private(&mut self, index: &TextIndex, player: &PlayerView) {
        let Some(l) = self.loaded.as_mut() else {
            return;
        };
        let sizes = player.screen_sizes();
        let found = redact::find(index, |screen| {
            sizes
                .iter()
                .find(|(name, _)| name == screen)
                .map(|(_, size)| *size)
        });
        let mut added: BTreeMap<redact::Kind, usize> = BTreeMap::new();
        for f in &found {
            let end = f.end.min(l.duration);
            let covered = l.edits.effects.iter().any(|e| {
                e.screen.as_deref() == Some(f.screen.as_str())
                    && e.start <= f.start + 0.01
                    && e.end >= end - 0.01
                    && matches!(&e.tool, Tool::Blur { area, .. } if redact::covers(area, &f.area))
            });
            if covered || end <= f.start {
                continue;
            }
            l.edits
                .add_on(f.start, end, Tool::blur(f.area), Some(f.screen.clone()));
            *added.entry(f.kind).or_default() += 1;
        }
        self.redact_note = Some(if added.is_empty() {
            if found.is_empty() {
                "Nothing private was found on screen.".to_string()
            } else {
                "Everything private on screen is blurred already.".to_string()
            }
        } else {
            self.saved = false;
            let parts: Vec<String> = added
                .iter()
                .map(|(kind, n)| format!("{n} {}", kind.noun(*n)))
                .collect();
            format!(
                "Blurred {}. Check them on the timeline; any that is not private can be deleted.",
                join(&parts)
            )
        });
    }

    /// Keep up with an export and let the title row know whether one is
    /// running, since the menu that starts them lives up there.
    pub fn poll_export(&mut self, player: &mut PlayerView) {
        self.exports.poll();
        player.set_exporting(self.exports.busy());
    }

    /// The export popup, over everything: a bar while it writes, and what
    /// came of it after.
    pub fn show_export(&mut self, ctx: &egui::Context) {
        self.exports.show(ctx, &theme::palette_for(ctx));
    }

    /// Ask where the recording should go, and start writing it there.
    pub fn start_export(
        &mut self,
        format: screenilux_export::Format,
        shape: screenilux_export::Shape,
        player: &mut PlayerView,
    ) {
        let (Some(video), Some(l)) = (player.track_file(0), self.loaded.as_ref()) else {
            return;
        };
        // A package is made of the folder the videos actually live in,
        // which for a `.screenilux` is wherever it was unpacked, not the
        // file itself.
        let Some(folder) = video.parent().map(Path::to_path_buf) else {
            return;
        };
        // The dialog opens where the recording came from, which is where
        // people keep looking for what came out of it.
        let start_in = l
            .path
            .parent()
            .filter(|d| d.is_dir())
            .map_or_else(|| folder.clone(), Path::to_path_buf);

        // Each screen where it stood on the desktop, which is how the
        // pointer's path lands on it. A plain video has no record of that,
        // and its one screen is the whole of where the pointer could be.
        let screens: Vec<screenilux_export::Screen> = player
            .export_screens()
            .into_iter()
            .map(|(name, path, (w, h))| {
                let desk = l
                    .manifest
                    .screens
                    .iter()
                    .find(|s| s.file == name)
                    .map_or(screenilux_core::Rect::new(0, 0, w, h), |s| {
                        screenilux_core::Rect::new(s.left, s.top, s.width, s.height)
                    });
                screenilux_export::Screen { name, path, desk }
            })
            .collect();
        // The sound is in the screen marked as carrying it; a plain video, or
        // a recording made before screens said so, has it in its first.
        let sound = l
            .manifest
            .screens
            .iter()
            .find(|s| s.audio)
            .and_then(|s| screens.iter().find(|e| e.name == s.file))
            .or(screens.first())
            .map(|s| s.path.clone());
        let recording = screenilux_export::Recording {
            screens,
            sound,
            pointer: l.manifest.pointer.clone(),
            focus: l.manifest.focus.clone(),
            cursor: (!l.manifest.cursor_in_picture && !l.edits.pointer.hidden)
                .then_some(l.edits.pointer),
            shortcuts: if l.edits.hide_shortcuts {
                Vec::new()
            } else {
                l.manifest.shortcuts.clone()
            },
        };

        self.exports.start(
            format,
            Source {
                folder,
                stem: friendly_stem(&l.path),
                edits: l.edits.clone(),
                fps: f64::from(l.manifest.fps.max(1)),
                start_in,
                recording,
                shape,
            },
        );
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

    /// The track, and whatever the pointer just did to it. The track keeps
    /// its own state and paints itself; what comes back here is only what
    /// the rest of the editor has to answer — a stretch to ask about, and
    /// whether the edits still match what is on disk.
    fn track(&mut self, ui: &mut egui::Ui, p: &theme::Palette, clock: &SharedClock) {
        let Some(l) = self.loaded.as_mut() else {
            return;
        };
        let out = track::show(ui, p, &mut self.track, &mut l.edits, l.duration, clock);
        self.track_at = Some(out.rect);
        if out.changed {
            self.saved = false;
        }
        if let Some((from, to)) = out.marked {
            self.asking = Some(Asking::Span { from, to });
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
                    let open = self.track.opened == Some(id);
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
                                self.track.opened = if open { None } else { Some(id) };
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
            if self.track.opened == Some(id) {
                self.track.opened = None;
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
            self.track.opened = Some(id);
            self.saved = false;
            self.track.selection = None;
            close = true;
        }
        if close {
            self.asking = None;
            // The stretch was marked to ask this question. Closing without
            // an answer drops it too, rather than leaving a band on the
            // track that nothing will ever be laid on.
            self.track.selection = None;
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
pub(crate) fn item(ui: &mut egui::Ui, p: &theme::Palette, title: &str, sub: &str) -> bool {
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

pub(crate) fn tool_colour(tool: &Tool, p: &theme::Palette) -> egui::Color32 {
    match tool {
        Tool::Zoom { .. } | Tool::MarkClicks => p.signal,
        Tool::Hide { .. } => p.action,
        Tool::Callout { .. } | Tool::Screen { .. } => p.soft,
        _ => p.weak,
    }
}

/// What an export of this recording is called, before its extension: the
/// recording's own name, so the two sit together in a folder.
fn friendly_stem(path: &Path) -> String {
    path.file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "recording".to_string())
}

pub(crate) fn stamp(t: f64) -> String {
    let m = (t / 60.0).floor() as i64;
    format!("{m}:{:04.1}", t - m as f64 * 60.0)
}
pub(crate) fn stamp_short(t: f64) -> String {
    let m = (t / 60.0).floor() as i64;
    format!("{m}:{:02}", (t - m as f64 * 60.0).floor() as i64)
}

/// The manifest beside a recording, whether it is a folder or a package.
/// Where a recording's details and edits are kept: inside a session
/// folder, or in a file of its own beside a package or a video, named after
/// it. Never a file shared by the recordings in one folder, which is how one
/// recording's edits used to turn up on another.
fn details_file(path: &Path) -> PathBuf {
    if path.is_dir() {
        path.join(MANIFEST_NAME)
    } else {
        screenilux_recorder::session::details_path(path)
    }
}

/// The recording's details: its own file if it has one, else what a package
/// carries inside it. A video with neither has none.
fn read_manifest(path: &Path) -> Option<Manifest> {
    if let Some(m) = std::fs::read_to_string(details_file(path))
        .ok()
        .and_then(|json| Manifest::from_json(&json).ok())
    {
        return Some(m);
    }
    screenilux_core::package::is_package(path)
        .then(|| screenilux_core::package::read_manifest(path).ok())
        .flatten()
}

fn write_manifest(path: &Path, manifest: &Manifest) -> Result<(), String> {
    let json = manifest.to_json().map_err(|e| e.to_string())?;
    std::fs::write(details_file(path), json).map_err(|e| format!("could not save edits: {e}"))
}

/// "a", "a and b", "a, b and c".
fn join(parts: &[String]) -> String {
    match parts {
        [] => String::new(),
        [one] => one.clone(),
        [rest @ .., last] => format!("{} and {last}", rest.join(", ")),
    }
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
