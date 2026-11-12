//! What the Recordings list shows for each recording beyond its file: how
//! many screens it has, how long it runs, and a small picture from its
//! opening. Worked out on a thread of its own, one recording at a time, and
//! only for the recordings the list asks for, which is the page on show.

use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, channel};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

use eframe::egui;
use screenilux_core::package;

/// The largest a picture is kept at: twice the widest card, so it stays
/// sharp on a scaled display.
const PICTURE: (u32, u32) = (480, 270);
/// How much of a packaged screen's picture data is copied out to read its
/// opening from. Plenty for the first second at any bitrate Screenilux
/// records at, and a sliver of a long recording.
const HEAD: u64 = 12 << 20;
/// The picture is taken this far in, past whatever the first frames show
/// while the recorder settles.
const INTO: f64 = 0.5;

/// What was found out about one recording.
pub struct Found {
    pub screens: usize,
    /// Seconds, or zero when the file did not say.
    pub duration: f64,
    pub picture: Option<egui::ColorImage>,
}

#[derive(Default)]
struct Queue {
    wanted: Vec<PathBuf>,
    busy: Option<PathBuf>,
}

pub struct Thumbs {
    queue: Arc<(Mutex<Queue>, Condvar)>,
    stop: Arc<AtomicBool>,
    rx: Receiver<(PathBuf, Result<Found, String>)>,
}

impl Thumbs {
    pub fn start(ctx: &egui::Context) -> Self {
        let queue: Arc<(Mutex<Queue>, Condvar)> = Arc::default();
        let stop = Arc::new(AtomicBool::new(false));
        let (tx, rx) = channel();
        let (q, quit, repaint) = (queue.clone(), stop.clone(), ctx.clone());
        std::thread::Builder::new()
            .name("screenilux-thumbs".into())
            .spawn(move || {
                let (lock, ready) = &*q;
                while !quit.load(Ordering::Relaxed) {
                    let next = {
                        let Ok(mut queue) = lock.lock() else { return };
                        while queue.wanted.is_empty() {
                            // Woken when there is work; the timeout only
                            // lets a dropped list end the thread.
                            if quit.load(Ordering::Relaxed) {
                                return;
                            }
                            queue = match ready.wait_timeout(queue, Duration::from_millis(500)) {
                                Ok((queue, _)) => queue,
                                Err(_) => return,
                            };
                        }
                        let next = queue.wanted.remove(0);
                        queue.busy = Some(next.clone());
                        next
                    };
                    let found = find(&next);
                    if tx.send((next, found)).is_err() {
                        return;
                    }
                    if let Ok(mut queue) = lock.lock() {
                        queue.busy = None;
                    }
                    repaint.request_repaint();
                }
            })
            .ok();
        Self { queue, stop, rx }
    }

    /// Ask for these recordings, in this order, in place of whatever was
    /// asked for before: turning the page drops the last page's requests.
    /// One already being read is finished either way, so it is left out.
    pub fn want(&self, paths: Vec<PathBuf>) {
        let (lock, ready) = &*self.queue;
        if let Ok(mut queue) = lock.lock() {
            let paths: Vec<PathBuf> = paths
                .into_iter()
                .filter(|p| queue.busy.as_ref() != Some(p))
                .collect();
            if queue.wanted != paths {
                queue.wanted = paths;
                ready.notify_one();
            }
        }
    }

    /// Whatever has been found since last asked.
    pub fn take(&self) -> Vec<(PathBuf, Result<Found, String>)> {
        self.rx.try_iter().collect()
    }
}

impl Drop for Thumbs {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        self.queue.1.notify_one();
    }
}

fn find(path: &Path) -> Result<Found, String> {
    if package::is_package(path) {
        let manifest =
            package::read_manifest(path).map_err(|_| "Can't read this file".to_string())?;
        // The screen the recording starts on, as the player shows it first.
        let first = manifest.screens.iter().min_by_key(|s| s.order);
        let picture = first.and_then(|s| opening_picture(path, &s.file));
        return Ok(Found {
            screens: manifest.screens.len(),
            duration: manifest.duration,
            picture,
        });
    }
    let info = screenilux_platform::probe(path).map_err(|_| "Can't play this file".to_string())?;
    Ok(Found {
        screens: 1,
        duration: info.duration,
        picture: picture_of(path),
    })
}

/// A packaged screen's picture, read from a copy of its opening.
fn opening_picture(package_path: &Path, file: &str) -> Option<egui::ColorImage> {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let n = NEXT.fetch_add(1, Ordering::Relaxed);
    let name = format!("screenilux-thumb-{}-{n}.mp4", std::process::id());
    let temp = std::env::temp_dir().join(name);
    let written = (|| -> Result<(), String> {
        let mut out = BufWriter::new(File::create(&temp).map_err(|e| e.to_string())?);
        package::video_opening(package_path, file, HEAD, &mut out).map_err(|e| e.to_string())?;
        out.flush().map_err(|e| e.to_string())
    })();
    let picture = match written {
        Ok(()) => picture_of(&temp),
        Err(e) => {
            tracing::debug!(path = %package_path.display(), error = %e, "no opening to read");
            None
        }
    };
    let _ = std::fs::remove_file(&temp);
    picture
}

/// A frame from just into the video, made small.
fn picture_of(path: &Path) -> Option<egui::ColorImage> {
    let mut decoder = screenilux_platform::VideoDecoder::open(path).ok()?;
    let first = decoder.next_frame().ok()??;
    // Frames on the way are decoded but not converted, and a short video
    // or a cut-short copy that ends first leaves the first frame.
    let mut chosen = None;
    if first.t < INTO {
        for _ in 0..90 {
            match decoder.next_frame_if(|t| t >= INTO) {
                Ok(Some((_, Some(frame)))) => {
                    chosen = Some(frame);
                    break;
                }
                Ok(Some((_, None))) => continue,
                _ => break,
            }
        }
    }
    let frame = chosen.unwrap_or(first);
    let (w, h, rgba) = shrink(
        &frame.rgba,
        frame.width as usize,
        frame.height as usize,
        PICTURE,
    )?;
    Some(egui::ColorImage::from_rgba_unmultiplied([w, h], &rgba))
}

/// Scale an RGBA picture down to fit `fit`, keeping its shape, by
/// averaging each block of pixels that becomes one. Never scales up.
pub fn shrink(rgba: &[u8], w: usize, h: usize, fit: (u32, u32)) -> Option<(usize, usize, Vec<u8>)> {
    if w == 0 || h == 0 || rgba.len() < w * h * 4 {
        return None;
    }
    let scale = (fit.0 as f64 / w as f64)
        .min(fit.1 as f64 / h as f64)
        .min(1.0);
    let ow = ((w as f64 * scale).round() as usize).max(1);
    let oh = ((h as f64 * scale).round() as usize).max(1);
    let mut out = Vec::with_capacity(ow * oh * 4);
    for oy in 0..oh {
        let (y0, y1) = (oy * h / oh, ((oy + 1) * h / oh).max(oy * h / oh + 1));
        for ox in 0..ow {
            let (x0, x1) = (ox * w / ow, ((ox + 1) * w / ow).max(ox * w / ow + 1));
            let mut sum = [0u64; 4];
            for y in y0..y1 {
                let row = &rgba[(y * w + x0) * 4..(y * w + x1) * 4];
                for px in row.as_chunks::<4>().0 {
                    for (s, v) in sum.iter_mut().zip(px) {
                        *s += *v as u64;
                    }
                }
            }
            let n = ((y1 - y0) * (x1 - x0)) as u64;
            out.extend(sum.iter().map(|s| ((s + n / 2) / n) as u8));
        }
    }
    Some((ow, oh, out))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn solid(w: usize, h: usize, px: [u8; 4]) -> Vec<u8> {
        px.iter().copied().cycle().take(w * h * 4).collect()
    }

    #[test]
    fn a_picture_shrinks_to_fit_and_keeps_its_shape() {
        let picture = solid(1920, 1080, [10, 20, 30, 255]);
        let (w, h, out) = shrink(&picture, 1920, 1080, PICTURE).unwrap();
        assert_eq!((w, h), (480, 270));
        assert_eq!(out.len(), 480 * 270 * 4);
        assert!(
            out.as_chunks::<4>()
                .0
                .iter()
                .all(|p| *p == [10, 20, 30, 255])
        );

        // A tall screen fits by its height.
        let (w, h, _) = shrink(&solid(1080, 1920, [0; 4]), 1080, 1920, PICTURE).unwrap();
        assert_eq!((w, h), (152, 270));
    }

    #[test]
    fn shrinking_averages_and_never_enlarges() {
        let mut two = solid(1, 1, [0, 0, 0, 255]);
        two.extend(solid(1, 1, [255, 255, 255, 255]));
        let (w, h, out) = shrink(&two, 2, 1, (1, 1)).unwrap();
        assert_eq!((w, h), (1, 1));
        assert_eq!(out, vec![128, 128, 128, 255]);

        let (w, h, _) = shrink(&solid(100, 50, [1; 4]), 100, 50, PICTURE).unwrap();
        assert_eq!((w, h), (100, 50));
    }

    #[test]
    fn an_empty_or_short_picture_is_refused() {
        assert!(shrink(&[], 0, 0, PICTURE).is_none());
        assert!(shrink(&[0; 8], 4, 4, PICTURE).is_none());
    }
}
