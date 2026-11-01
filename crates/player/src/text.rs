//! Reading the video's text: on demand for a paused frame, and for the whole
//! recording in the background so it can be searched.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::thread::JoinHandle;

use crossbeam_channel::{Receiver, Sender, unbounded};
use screenilux_core::playback::Recording;
use screenilux_core::text::{Page, TextIndex};
use screenilux_win::TextReader;
use screenilux_win::decoder::{VideoDecoder, VideoFrame};

/// How often the indexer samples each screen. Text that is on screen for
/// less than this may be missed; anything a viewer could read is not.
pub const INDEX_INTERVAL: f64 = 2.0;

/// A request to read one frame.
struct Job {
    track: usize,
    frame: Arc<VideoFrame>,
}

/// The text on one frame.
#[derive(Debug, Clone)]
pub struct Read {
    pub track: usize,
    /// The frame's own timestamp, so a result that arrives after the user has
    /// moved on can be recognised as stale.
    pub t: f64,
    pub page: Result<Page, String>,
}

/// Reads paused frames on a background thread, so the UI never waits on OCR.
pub struct TextWorker {
    jobs: Option<Sender<Job>>,
    results: Receiver<Read>,
    handle: Option<JoinHandle<()>>,
}

impl TextWorker {
    pub fn start(repaint: Arc<dyn Fn() + Send + Sync>) -> Self {
        let (jobs, rx) = unbounded::<Job>();
        let (tx, results) = unbounded();
        let handle = std::thread::Builder::new()
            .name("screenilux-text".into())
            .spawn(move || {
                let reader = TextReader::new();
                for job in rx {
                    let page = match &reader {
                        Ok(r) => r.read(&job.frame.rgba, job.frame.width, job.frame.height),
                        Err(e) => Err(e.clone()),
                    };
                    let _ = tx.send(Read {
                        track: job.track,
                        t: job.frame.t,
                        page,
                    });
                    repaint();
                }
            })
            .ok();
        Self {
            jobs: Some(jobs),
            results,
            handle,
        }
    }

    pub fn read(&self, track: usize, frame: Arc<VideoFrame>) {
        if let Some(jobs) = &self.jobs {
            let _ = jobs.send(Job { track, frame });
        }
    }

    /// Results that have arrived since the last call.
    pub fn results(&self) -> Vec<Read> {
        self.results.try_iter().collect()
    }
}

impl Drop for TextWorker {
    fn drop(&mut self) {
        // Closing the channel ends the thread's loop.
        self.jobs = None;
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

/// A coarse greyscale thumbnail, for telling whether a frame changed.
///
/// Fine enough that one changed word moves a cell: at 1080p a cell is 15
/// pixels square, about one character.
fn thumbnail(f: &VideoFrame) -> Vec<u8> {
    const W: u32 = 128;
    const H: u32 = 72;
    let (cw, ch) = ((f.width / W).max(1), (f.height / H).max(1));
    let mut out = Vec::with_capacity((W * H) as usize);
    for gy in 0..H.min(f.height) {
        for gx in 0..W.min(f.width) {
            let mut sum = 0u32;
            let mut n = 0u32;
            // Sampling a few pixels per cell is enough and keeps this cheap.
            for y in (gy * ch..((gy + 1) * ch).min(f.height)).step_by(3) {
                for x in (gx * cw..((gx + 1) * cw).min(f.width)).step_by(3) {
                    let i = ((y * f.width + x) * 4) as usize;
                    sum += (f.rgba[i] as u32 * 77
                        + f.rgba[i + 1] as u32 * 150
                        + f.rgba[i + 2] as u32 * 29)
                        >> 8;
                    n += 1;
                }
            }
            out.push((sum / n.max(1)) as u8);
        }
    }
    out
}

/// Whether two thumbnails show the same thing, allowing for codec noise.
fn unchanged(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).all(|(x, y)| x.abs_diff(*y) <= 3)
}

/// How far an index build has got, and its result.
pub struct Indexer {
    /// Thousandths done, across every screen.
    progress: Arc<AtomicU32>,
    cancel: Arc<AtomicBool>,
    done: Receiver<Result<TextIndex, String>>,
    handle: Option<JoinHandle<()>>,
}

impl Indexer {
    /// Read every screen's text, sampling every `interval` seconds.
    ///
    /// Each screen is decoded once front to back, converting only the
    /// sampled frames, and a sample that looks the same as the last one
    /// reuses its text instead of running OCR again. A static screen
    /// therefore costs almost nothing beyond decoding.
    pub fn start(
        recording: Recording,
        interval: f64,
        repaint: Arc<dyn Fn() + Send + Sync>,
    ) -> Self {
        let progress = Arc::new(AtomicU32::new(0));
        let cancel = Arc::new(AtomicBool::new(false));
        let (tx, done) = unbounded();
        let (p, c) = (Arc::clone(&progress), Arc::clone(&cancel));
        let handle = std::thread::Builder::new()
            .name("screenilux-index".into())
            .spawn(move || {
                let result = build(&recording, interval, &p, &c, &*repaint);
                let _ = tx.send(result);
                repaint();
            })
            .ok();
        Self {
            progress,
            cancel,
            done,
            handle,
        }
    }

    /// Fraction done, 0 to 1.
    pub fn progress(&self) -> f32 {
        self.progress.load(Ordering::Relaxed) as f32 / 1000.0
    }

    /// The finished index, once.
    pub fn finished(&self) -> Option<Result<TextIndex, String>> {
        self.done.try_recv().ok()
    }
}

impl Drop for Indexer {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

fn build(
    recording: &Recording,
    interval: f64,
    progress: &AtomicU32,
    cancel: &AtomicBool,
    repaint: &(dyn Fn() + Send + Sync),
) -> Result<TextIndex, String> {
    let reader = TextReader::new()?;
    let mut index = TextIndex::new(interval);
    let tracks = recording.tracks.len().max(1) as f64;
    let duration = recording.duration.max(0.001);

    for (i, track) in recording.tracks.iter().enumerate() {
        let mut dec = VideoDecoder::open(&track.path).map_err(|e| e.to_string())?;
        let mut next_sample = 0.0f64;
        let mut last: Option<(Vec<u8>, Page)> = None;
        let mut last_report = 0u32;

        while let Some((t, frame)) = dec
            .next_frame_if(|t| t + 1e-3 >= next_sample)
            .map_err(|e| e.to_string())?
        {
            if cancel.load(Ordering::Relaxed) {
                return Err("cancelled".into());
            }
            let Some(frame) = frame else { continue };
            next_sample = t + interval;

            let thumb = thumbnail(&frame);
            let page = match &last {
                Some((prev, page)) if unchanged(prev, &thumb) => page.clone(),
                _ => reader.read(&frame.rgba, frame.width, frame.height)?,
            };
            index.record(&track.id, t, page.clone());
            last = Some((thumb, page));

            let done = ((i as f64 + (t / duration).min(1.0)) / tracks * 1000.0) as u32;
            progress.store(done, Ordering::Relaxed);
            if done >= last_report + 10 {
                last_report = done;
                repaint();
            }
        }
    }
    progress.store(1000, Ordering::Relaxed);
    Ok(index)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(w: u32, h: u32, fill: u8) -> VideoFrame {
        VideoFrame {
            t: 0.0,
            width: w,
            height: h,
            rgba: vec![fill; (w * h * 4) as usize],
        }
    }

    #[test]
    fn identical_frames_are_unchanged() {
        let a = thumbnail(&frame(1920, 1080, 40));
        let b = thumbnail(&frame(1920, 1080, 41));
        assert!(unchanged(&a, &b), "codec noise is not a change");
    }

    #[test]
    fn one_changed_word_counts_as_a_change() {
        let a = frame(1920, 1080, 255);
        let mut b = a.clone();
        // A dark 12x16 glyph-sized patch, roughly one character.
        for y in 500..516 {
            for x in 900..912 {
                let i = ((y * 1920 + x) * 4) as usize;
                b.rgba[i..i + 3].copy_from_slice(&[0, 0, 0]);
            }
        }
        assert!(!unchanged(&thumbnail(&a), &thumbnail(&b)));
    }

    #[test]
    fn tiny_frames_do_not_divide_by_zero() {
        let t = thumbnail(&frame(4, 4, 9));
        assert!(!t.is_empty());
    }
}
