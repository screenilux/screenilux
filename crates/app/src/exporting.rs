//! Handing the demo over: which shape it leaves in, where it lands, and
//! watching it happen.
//!
//! Export used to be one button that wrote one GIF to one place nobody
//! chose. That is three decisions taken on the user's behalf, and only the
//! first of them was ever right. So the button became a menu of the three
//! things a demo actually becomes — a video, a GIF, a package — every
//! export asks where it should go, and the waiting is shown rather than
//! guessed at.
//!
//! The writing happens off the interface's thread, because a video is
//! minutes of work and a frozen window looks like a crash. What crosses
//! back is deliberately small: a number for the bar, and one result at the
//! end. What crosses the other way is a flag saying stop, since the person
//! who started a ten-minute export is the one most likely to want it back.
//!
//! The popup is modal on purpose. An export reads the recording the editor
//! is holding, so letting the edits change underneath it would be writing
//! out something nobody asked for.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::mpsc::{Receiver, TryRecvError, channel};

use eframe::egui;
use screenilux_core::edit::EditList;
use screenilux_core::session::{MANIFEST_NAME, Manifest};
use screenilux_export::{Format, Mp4, Written};

use crate::theme::{self, icon};

/// The popup's width: enough for a file name and a bar under it.
const POPUP_W: f32 = 340.0;

/// What an export needs to know about the recording it is writing out.
pub struct Source {
    /// The video to read. The first screen: the others are only in a
    /// package, which copies files rather than reading them.
    pub video: PathBuf,
    /// The folder holding the tracks and the manifest, which is what a
    /// package is made of.
    pub folder: PathBuf,
    /// What the file is called, before the extension.
    pub stem: String,
    pub edits: EditList,
    pub fps: f64,
    /// Where the save dialog opens.
    pub start_in: PathBuf,
}

/// An export under way.
struct Job {
    format: Format,
    to: PathBuf,
    /// How far along, in thousandths, so the bar can be read without a lock.
    at: Arc<AtomicU32>,
    stop: Arc<AtomicBool>,
    result: Receiver<Result<Written, String>>,
}

/// What became of the last one, for the popup to report.
enum Outcome {
    Done { to: PathBuf, text: String },
    Failed(String),
}

#[derive(Default)]
pub struct Exports {
    job: Option<Job>,
    last: Option<Outcome>,
}

impl Exports {
    /// Is something being written? The export menu stands down while it is:
    /// two exports at once would read the same edits twice over and race
    /// for the same name.
    pub fn busy(&self) -> bool {
        self.job.is_some()
    }

    /// Ask where it should go, then start writing it there.
    ///
    /// The dialog is opened on this thread, which stops the app for as long
    /// as it is up. That is what the user is doing anyway, and it keeps the
    /// chosen path a plain return value rather than a third thing to poll.
    pub fn start(&mut self, format: Format, source: Source) {
        if self.busy() {
            return;
        }
        let Some(to) = ask_where(format, &source) else {
            return;
        };

        // A package is a copy of the folder, so the effects have to be in
        // the folder's own manifest before it is copied.
        if format == Format::Package
            && let Err(e) = lay_in_effects(&source.folder, &source.edits)
        {
            self.last = Some(Outcome::Failed(e));
            return;
        }

        let at = Arc::new(AtomicU32::new(0));
        let stop = Arc::new(AtomicBool::new(false));
        let (tx, result) = channel();
        let (ticker, halt, out) = (Arc::clone(&at), Arc::clone(&stop), to.clone());
        let Source {
            video,
            folder,
            edits,
            fps,
            ..
        } = source;

        std::thread::spawn(move || {
            let mut tick = |done: f32| {
                ticker.store((done * 1000.0) as u32, Ordering::Relaxed);
                !halt.load(Ordering::Relaxed)
            };
            let written = match format {
                Format::Mp4 => screenilux_export::mp4(
                    &video,
                    &out,
                    &edits,
                    Mp4 {
                        fps: fps.round().clamp(1.0, 240.0) as u32,
                    },
                    &mut tick,
                ),
                Format::Gif => screenilux_export::gif(
                    &video,
                    &out,
                    &edits,
                    screenilux_export::Gif::default(),
                    &mut tick,
                ),
                Format::Package => screenilux_export::package(&folder, &out, &mut tick),
            };
            let _ = tx.send(written.map_err(|e| e.to_string()));
        });

        self.last = None;
        self.job = Some(Job {
            format,
            to,
            at,
            stop,
            result,
        });
    }

    /// Pick up whatever the writing thread has finished saying.
    pub fn poll(&mut self) {
        let Some(job) = &self.job else { return };
        let outcome = match job.result.try_recv() {
            Ok(Ok(done)) => Some(Outcome::Done {
                to: job.to.clone(),
                text: reads_as(job.format, &done),
            }),
            // A stop is the user's own doing, so it has nothing to report.
            Ok(Err(e)) if job.stop.load(Ordering::Relaxed) => {
                let _ = e;
                None
            }
            Ok(Err(e)) => Some(Outcome::Failed(e)),
            Err(TryRecvError::Disconnected) => {
                Some(Outcome::Failed("the export stopped unexpectedly".into()))
            }
            Err(TryRecvError::Empty) => return,
        };
        self.last = outcome;
        self.job = None;
    }

    /// The popup: a bar while it writes, and what came of it after.
    pub fn show(&mut self, ctx: &egui::Context, p: &theme::Palette) {
        if self.job.is_none() && self.last.is_none() {
            return;
        }
        let mut close = false;
        let mut reveal: Option<PathBuf> = None;

        egui::Modal::new(egui::Id::unique("screenilux-export-popup"))
            .frame(
                egui::Frame::new()
                    .fill(p.base)
                    .stroke(egui::Stroke::new(1.0, p.stroke))
                    .corner_radius(16)
                    .inner_margin(egui::Margin::symmetric(20, 18)),
            )
            .show(ctx, |ui| {
                ui.set_width(POPUP_W);
                match (&self.job, &self.last) {
                    (Some(job), _) => {
                        let at = job.at.load(Ordering::Relaxed) as f32 / 1000.0;
                        heading(ui, p, &format!("Writing the {}", noun(job.format)));
                        name_line(ui, p, &job.to);
                        ui.add_space(12.0);
                        ui.add(
                            egui::ProgressBar::new(at)
                                .desired_height(10.0)
                                .corner_radius(5)
                                .fill(p.signal),
                        );
                        ui.add_space(4.0);
                        ui.horizontal(|ui| {
                            ui.label(
                                egui::RichText::new(if at <= 0.0 {
                                    "Starting…".to_string()
                                } else {
                                    format!("{:.0}%", at * 100.0)
                                })
                                .size(11.5)
                                .color(p.weak),
                            );
                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    if ui.add(theme::outline("Stop")).clicked() {
                                        job.stop.store(true, Ordering::Relaxed);
                                    }
                                },
                            );
                        });
                        // Nothing here is an input event, so the bar has to
                        // ask for the frames that show it filling.
                        ui.ctx().request_repaint();
                    }
                    (None, Some(Outcome::Done { to, text })) => {
                        heading(ui, p, "Exported");
                        name_line(ui, p, to);
                        ui.add_space(2.0);
                        ui.label(egui::RichText::new(text).size(11.5).color(p.weak));
                        ui.add_space(14.0);
                        ui.horizontal(|ui| {
                            if ui
                                .add(theme::outline(format!("{}  Show in folder", icon::FOLDER)))
                                .clicked()
                            {
                                reveal = Some(to.clone());
                                close = true;
                            }
                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    if ui.add(theme::outline("Done")).clicked() {
                                        close = true;
                                    }
                                },
                            );
                        });
                    }
                    (None, Some(Outcome::Failed(e))) => {
                        heading(ui, p, "It did not work");
                        ui.add_space(4.0);
                        theme::notice(ui, e.clone());
                        ui.add_space(14.0);
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if ui.add(theme::outline("Close")).clicked() {
                                close = true;
                            }
                        });
                    }
                    (None, None) => {}
                }
            });

        // Escape closes what is finished; it does not stop what is running,
        // since a key pressed by accident should not throw away ten minutes.
        if self.job.is_none() && ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
            close = true;
        }
        if let Some(path) = reveal {
            screenilux_platform::reveal(&path);
        }
        if close {
            self.last = None;
        }
    }
}

/// Put the effects as they now stand into the folder's manifest, leaving
/// everything else in it alone.
///
/// Read first and written back, rather than written from what the editor
/// is holding: the editor's own copy of the manifest can be a stand-in it
/// made when it could not find one, and packing that would hand somebody a
/// file that names no screens.
fn lay_in_effects(folder: &Path, edits: &EditList) -> Result<(), String> {
    let path = folder.join(MANIFEST_NAME);
    let json = std::fs::read_to_string(&path)
        .map_err(|e| format!("could not read this recording's details: {e}"))?;
    let mut manifest =
        Manifest::from_json(&json).map_err(|e| format!("could not read this recording: {e}"))?;
    manifest.edits = (!edits.is_empty()).then(|| edits.clone());
    let json = manifest
        .to_json()
        .map_err(|e| format!("could not write this recording's details: {e}"))?;
    std::fs::write(&path, json).map_err(|e| format!("could not save the effects: {e}"))
}

/// Where should this go? Opened beside the recording, named after it, and
/// filtered to the one extension that makes sense.
fn ask_where(format: Format, source: &Source) -> Option<PathBuf> {
    rfd::FileDialog::new()
        .set_title(format!("Export as {}", format.label()))
        .set_directory(&source.start_in)
        .set_file_name(format!("{}.{}", source.stem, format.ext()))
        .add_filter(format.label(), &[format.ext()])
        .save_file()
}

/// What the file turned out to be, in the terms that format is judged in.
fn reads_as(format: Format, done: &Written) -> String {
    match format {
        Format::Package => weight(done.bytes),
        _ => format!(
            "{}×{} · {} frames · {}",
            done.width,
            done.height,
            done.frames,
            weight(done.bytes)
        ),
    }
}

/// What the thing being written is called, in a sentence.
fn noun(format: Format) -> &'static str {
    match format {
        Format::Mp4 => "video",
        Format::Gif => "GIF",
        Format::Package => "package",
    }
}

fn heading(ui: &mut egui::Ui, p: &theme::Palette, text: &str) {
    ui.label(
        egui::RichText::new(text)
            .font(egui::FontId::new(16.0, theme::bold()))
            .color(p.text),
    );
}

/// The file's own name, which is the thing the user is actually waiting on.
fn name_line(ui: &mut egui::Ui, p: &theme::Palette, to: &Path) {
    ui.add_space(3.0);
    ui.label(
        egui::RichText::new(to.file_name().unwrap_or_default().to_string_lossy())
            .family(theme::strong())
            .size(12.0)
            .color(p.weak),
    );
}

fn weight(bytes: u64) -> String {
    const MB: f64 = 1024.0 * 1024.0;
    let mb = bytes as f64 / MB;
    if mb >= 1.0 {
        format!("{mb:.1} MB")
    } else {
        format!("{:.0} KB", bytes as f64 / 1024.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes_read_the_way_a_download_does() {
        assert_eq!(weight(900), "1 KB");
        assert_eq!(weight(1024 * 1024 * 3 / 2), "1.5 MB");
    }

    #[test]
    fn a_package_is_judged_by_its_size_and_a_video_by_its_picture() {
        let done = Written {
            frames: 120,
            bytes: 2 * 1024 * 1024,
            width: 1280,
            height: 720,
        };
        assert_eq!(reads_as(Format::Package, &done), "2.0 MB");
        assert_eq!(
            reads_as(Format::Mp4, &done),
            "1280×720 · 120 frames · 2.0 MB"
        );
    }

    #[test]
    fn every_format_says_what_it_is_for() {
        for format in Format::ALL {
            assert!(!format.hint().is_empty());
            assert!(!noun(format).is_empty());
            assert!(!format.ext().is_empty());
        }
    }
}
