//! Writing a recording out: the three things a finished demo becomes.
//!
//! A demo leaves the app in one of three shapes, and which one is right
//! depends entirely on where it is going. An **MP4** is the demo itself:
//! full colour, full frame rate, the thing to upload or attach. A **GIF**
//! is the demo made small enough to live inline. A **package** is the
//! recording as working material — every screen, the text read off them and
//! the edit list — so it can be opened and changed again, here or on
//! another machine.
//!
//! All three read the recording through its edit list, which is what makes
//! edits real rather than a preview: a cut is a stretch of source frames
//! that are never asked for, a speed-up is frames arriving closer together,
//! and what comes out is the length the editor said it would be.
//!
//! Every export reports how far along it is and can be told to stop, since
//! a long recording takes long enough that somebody will want to.
//!
//! The MP4 and the GIF are drawn frame by frame through
//! [`frames`]: every screen in step, the one on show dressed with the
//! effects live on it (see `screenilux_core::dress`), in the shape asked
//! for. The package copies files and changes nothing.

#![forbid(unsafe_code)]

mod audio;
mod frames;

use std::fs::File;
use std::io::BufWriter;
use std::path::{Path, PathBuf};

use screenilux_core::edit::EditList;
use screenilux_core::package;
use screenilux_core::ports::VideoSink;
use screenilux_platform::NativeSink;

use frames::Frames;
pub use frames::{PointerStyle, Recording, Screen, Shape, Shortcut};

#[derive(Debug, thiserror::Error)]
pub enum ExportError {
    #[error("could not read the recording: {0}")]
    Read(String),
    #[error("could not write it out: {0}")]
    Write(String),
    #[error("there is nothing left to export: every frame is cut")]
    Empty,
    #[error("the export was stopped")]
    Stopped,
}

/// What a recording can be written out as.
///
/// These are three different answers to "what is this demo for", not three
/// settings of one thing, which is why each says what it is good for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    /// The demo itself: full colour and frame rate, to upload or attach.
    Mp4,
    /// The demo made small: plays by itself anywhere a picture goes.
    Gif,
    /// The recording as working material, edits and all.
    Package,
}

impl Format {
    /// In the order the menu offers them: the one most people want first.
    pub const ALL: [Self; 3] = [Self::Mp4, Self::Gif, Self::Package];

    pub fn label(self) -> &'static str {
        match self {
            Self::Mp4 => "Video (MP4)",
            Self::Gif => "Animated GIF",
            Self::Package => "Screenilux package",
        }
    }

    /// What it is for, in one line, for the menu row under the name.
    pub fn hint(self) -> &'static str {
        match self {
            Self::Mp4 => "Full quality with sound, to upload or attach.",
            Self::Gif => "Small and silent: for a README, a PR or a chat.",
            Self::Package => "Every screen and your effects, to open again later.",
        }
    }

    pub fn ext(self) -> &'static str {
        match self {
            Self::Mp4 => "mp4",
            Self::Gif => "gif",
            Self::Package => package::PACKAGE_EXT,
        }
    }
}

/// Where an export of `stem` lands in `dir`.
///
/// The extension is appended rather than replaced: a recording called
/// "v1.2 walkthrough" has no extension to replace, and `with_extension`
/// would take ".2 walkthrough" for one.
pub fn into_folder(dir: &Path, stem: &str, format: Format) -> PathBuf {
    dir.join(format!("{stem}.{}", format.ext()))
}

/// How a GIF is to be written.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Gif {
    /// Frames a second. Ten reads as motion, divides evenly into the
    /// hundredths of a second a GIF counts in, and keeps the file down.
    pub fps: f64,
    /// The widest the picture may be. A demo in a README is read at about
    /// this, and anything wider is paid for on every view.
    pub width: u32,
    pub shape: Shape,
}

impl Default for Gif {
    fn default() -> Self {
        Self {
            fps: 10.0,
            width: 720,
            shape: Shape::Wide,
        }
    }
}

/// What came of a GIF, for telling someone what they got.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Written {
    pub frames: u32,
    pub bytes: u64,
    pub width: u32,
    pub height: u32,
}

/// Write `video` out to `out` as a GIF, read through `edits`.
///
/// A GIF is what a demo wants to be when it has to live in a README, a pull
/// request or a chat message: it plays by itself, it needs no player and it
/// survives being pasted anywhere. The price is a small palette and no
/// sound. It samples the recording at a handful of frames a second and
/// shrinks it to a width worth loading.
///
/// `progress` is called with a fraction between 0 and 1 as the source is
/// walked, and may be called many times a second. Returning `false` from it
/// stops the export and leaves nothing behind.
pub fn gif(
    recording: &Recording,
    out: &Path,
    edits: &EditList,
    how: Gif,
    mut progress: impl FnMut(f32) -> bool,
) -> Result<Written, ExportError> {
    let step = 1.0 / how.fps.max(1.0);
    let mut frames = Frames::open(recording, edits, how.shape, how.fps)?;
    // GIF counts delays in hundredths of a second, so the frame rate that
    // actually plays is the one that survives rounding.
    let delay = ((step * 100.0).round() as u16).max(2);

    // The encoder owns the file, and it cannot be made until the first
    // frame says how big the picture is, so both wait together.
    let mut encoder: Option<gif::Encoder<BufWriter<File>>> = None;
    let mut size = (0u32, 0u32);
    let mut written = 0u32;
    // The last picture written, to tell what moved since.
    let mut previous: Option<Vec<u8>> = None;
    // Time owed by frames that changed nothing, paid to the next one that
    // does, so a still stretch costs one frame rather than thirty.
    let mut held = 0u16;
    let (fw, fh) = frames.size();

    loop {
        if !progress(frames.progress()) {
            drop(encoder);
            let _ = std::fs::remove_file(out);
            return Err(ExportError::Stopped);
        }
        let Some((_, frame)) = frames.next()? else {
            break;
        };
        let (small, w, h) = shrink(frame, fw, fh, how.width);
        // Only what moved. A screen recording is mostly still, so writing
        // the whole picture every tenth of a second spends almost all of
        // the file on pixels that did not change. Everything outside the
        // part that moved is left transparent and the frame before it is
        // kept underneath.
        let (patch, area) = match &previous {
            Some(before) => match changed(before, &small, w, h) {
                Some(area) => (cut_out(&small, w, area), area),
                // Nothing moved: let the frame before it hold the screen
                // for this long as well.
                None => {
                    held += delay;
                    continue;
                }
            },
            None => (small.clone(), Area { x: 0, y: 0, w, h }),
        };
        previous = Some(small);

        let enc = match &mut encoder {
            Some(e) => e,
            None => {
                size = (w, h);
                let file = BufWriter::new(
                    File::create(out).map_err(|e| ExportError::Write(e.to_string()))?,
                );
                let mut e = gif::Encoder::new(file, w as u16, h as u16, &[])
                    .map_err(|e| ExportError::Write(e.to_string()))?;
                e.set_repeat(gif::Repeat::Infinite)
                    .map_err(|e| ExportError::Write(e.to_string()))?;
                encoder.insert(e)
            }
        };

        let mut pixels = patch;
        let mut gf = gif::Frame::from_rgba_speed(area.w as u16, area.h as u16, &mut pixels, 10);
        gf.left = area.x as u16;
        gf.top = area.y as u16;
        gf.delay = delay + std::mem::take(&mut held);
        gf.dispose = gif::DisposalMethod::Keep;
        enc.write_frame(&gf)
            .map_err(|e| ExportError::Write(e.to_string()))?;
        written += 1;
    }

    drop(encoder);
    let bytes = std::fs::metadata(out).map(|m| m.len()).unwrap_or(0);
    progress(1.0);
    if written == 0 {
        return Err(ExportError::Empty);
    }
    Ok(Written {
        frames: written,
        bytes,
        width: size.0,
        height: size.1,
    })
}

/// The part of a picture that changed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Area {
    x: u32,
    y: u32,
    w: u32,
    h: u32,
}

/// The smallest rectangle covering every pixel that differs between two
/// pictures of the same size, or `None` when they are the same.
///
/// Compared with a little slack, because the encoder quantises anyway and a
/// pixel that moved by one step of brightness is not something anybody sees
/// — but it is something that would cost a whole frame.
fn changed(before: &[u8], after: &[u8], w: u32, h: u32) -> Option<Area> {
    const SLACK: i16 = 6;
    let (mut x0, mut y0, mut x1, mut y1) = (w, h, 0u32, 0u32);
    for y in 0..h {
        let row = (y as usize) * (w as usize) * 4;
        for x in 0..w {
            let i = row + (x as usize) * 4;
            let moved = (0..3).any(|c| (before[i + c] as i16 - after[i + c] as i16).abs() > SLACK);
            if moved {
                x0 = x0.min(x);
                y0 = y0.min(y);
                x1 = x1.max(x);
                y1 = y1.max(y);
            }
        }
    }
    if x1 < x0 || y1 < y0 {
        return None;
    }
    Some(Area {
        x: x0,
        y: y0,
        w: x1 - x0 + 1,
        h: y1 - y0 + 1,
    })
}

/// `area` lifted out of a picture `w` wide.
fn cut_out(pixels: &[u8], w: u32, area: Area) -> Vec<u8> {
    let mut out = Vec::with_capacity((area.w * area.h * 4) as usize);
    for y in area.y..area.y + area.h {
        let from = ((y as usize) * (w as usize) + area.x as usize) * 4;
        out.extend_from_slice(&pixels[from..from + (area.w as usize) * 4]);
    }
    out
}

/// `rgba` scaled down to at most `limit` wide, by averaging the pixels that
/// fall into each new one. Averaging rather than picking the nearest keeps
/// small text legible, which is most of what a demo is made of.
fn shrink(rgba: &[u8], width: u32, height: u32, limit: u32) -> (Vec<u8>, u32, u32) {
    if width == 0 || height == 0 {
        return (Vec::new(), 0, 0);
    }
    if width <= limit {
        return (rgba.to_vec(), width, height);
    }
    // Even dimensions: a GIF of an odd width is fine, but the halving looks
    // better and encoders are happier.
    let w = (limit & !1).max(2);
    let h = ((height as u64 * w as u64 / width as u64) as u32 & !1).max(2);
    let mut out = vec![0u8; (w as usize) * (h as usize) * 4];

    for y in 0..h {
        let y0 = (y as u64 * height as u64 / h as u64) as u32;
        let y1 = (((y + 1) as u64 * height as u64 / h as u64) as u32).max(y0 + 1);
        for x in 0..w {
            let x0 = (x as u64 * width as u64 / w as u64) as u32;
            let x1 = (((x + 1) as u64 * width as u64 / w as u64) as u32).max(x0 + 1);
            let (mut r, mut g, mut b, mut a, mut n) = (0u32, 0u32, 0u32, 0u32, 0u32);
            for sy in y0..y1.min(height) {
                let row = (sy as usize) * (width as usize) * 4;
                for sx in x0..x1.min(width) {
                    let i = row + (sx as usize) * 4;
                    r += rgba[i] as u32;
                    g += rgba[i + 1] as u32;
                    b += rgba[i + 2] as u32;
                    a += rgba[i + 3] as u32;
                    n += 1;
                }
            }
            let n = n.max(1);
            let i = ((y as usize) * (w as usize) + x as usize) * 4;
            out[i] = (r / n) as u8;
            out[i + 1] = (g / n) as u8;
            out[i + 2] = (b / n) as u8;
            out[i + 3] = (a / n) as u8;
        }
    }
    (out, w, h)
}

/// How an MP4 is to be written.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Mp4 {
    /// Frames a second in the finished video. The recording's own rate is
    /// the right answer: anything lower throws motion away, and anything
    /// higher invents frames that were never captured.
    pub fps: u32,
    pub shape: Shape,
}

impl Default for Mp4 {
    fn default() -> Self {
        Self {
            fps: 30,
            shape: Shape::Wide,
        }
    }
}

/// Write `video` out to `out` as an MP4, read through `edits`.
///
/// This is the demo itself rather than a picture of it: the recording's own
/// resolution, its own frame rate, encoded by whatever the operating system
/// encodes with. The sound goes through the same edits as the picture (see
/// [`audio`]): cut where it is cut, silent where it is muted, and quicker
/// where it is sped up, at the voice's own pitch.
///
/// The video comes out at a constant frame rate, so a source frame that
/// covers several output frames is written once for each of them. That is
/// what makes a held frame hold and a speed-up actually run faster, rather
/// than the file merely claiming to.
pub fn mp4(
    recording: &Recording,
    out: &Path,
    edits: &EditList,
    how: Mp4,
    mut progress: impl FnMut(f32) -> bool,
) -> Result<Written, ExportError> {
    let fps = how.fps.max(1);
    let mut frames = Frames::open(recording, edits, how.shape, f64::from(fps))?;
    let (width, height) = frames.size();
    let mut sound = match &recording.sound {
        Some(path) => audio::Soundtrack::open(path, edits)?,
        None => None,
    };
    let mut sink = NativeSink::new(width, height, fps, sound.is_some(), scratch_mp4())
        .map_err(|e| ExportError::Write(e.to_string()))?;
    sink.open_segment()
        .map_err(|e| ExportError::Write(e.to_string()))?;

    let mut written = 0u32;
    let mut bgra = vec![0u8; width as usize * height as usize * 4];
    loop {
        if !progress(frames.progress()) {
            return Err(ExportError::Stopped);
        }
        let Some((_, frame)) = frames.next()? else {
            break;
        };
        into_bgra(frame, width, width, height, &mut bgra);
        sink.write(&bgra)
            .map_err(|e| ExportError::Write(e.to_string()))?;
        written += 1;
        // The sound up to the end of this frame, so the two stay together.
        if let Some(sound) = sound.as_mut() {
            let pcm = sound.take_until(f64::from(written) / f64::from(fps))?;
            if !pcm.is_empty() {
                sink.write_audio(&pcm)
                    .map_err(|e| ExportError::Write(e.to_string()))?;
            }
        }
    }

    if written == 0 {
        return Err(ExportError::Empty);
    }
    sink.close_segment(0)
        .map_err(|e| ExportError::Write(e.to_string()))?;
    sink.finish(out)
        .map_err(|e| ExportError::Write(e.to_string()))?;
    progress(1.0);

    Ok(Written {
        frames: written,
        bytes: std::fs::metadata(out).map(|m| m.len()).unwrap_or(0),
        width,
        height,
    })
}

/// Copy a decoded picture into the encoder's buffer: red and blue swapped,
/// and cropped to the even size the encoder was opened at.
///
/// Rows go in the order they came out of the decoder, top first, which is
/// what the sink is told to expect.
fn into_bgra(rgba: &[u8], source_width: u32, width: u32, height: u32, out: &mut [u8]) {
    let src_stride = source_width as usize * 4;
    let dst_stride = width as usize * 4;
    for y in 0..height as usize {
        let (src, dst) = (y * src_stride, y * dst_stride);
        for x in 0..width as usize {
            let (i, o) = (src + x * 4, dst + x * 4);
            out[o] = rgba[i + 2];
            out[o + 1] = rgba[i + 1];
            out[o + 2] = rgba[i];
            out[o + 3] = rgba[i + 3];
        }
    }
}

/// Where the encoder writes before the file is moved into place. Named for
/// the moment it started, so two exports at once cannot collide.
fn scratch_mp4() -> PathBuf {
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    std::env::temp_dir().join(format!("screenilux-export-{stamp}.mp4"))
}

/// Write the session folder `from` out as a `.screenilux` package at `out`.
///
/// Nothing is re-encoded: the videos are already H.264 and the edit list is
/// a few lines of JSON, so this is a copy into one file. It is the only
/// export that keeps the sound, the other screens and the effects as
/// effects rather than as pixels, which makes it the one to send to
/// somebody who will keep working on the demo.
pub fn package(
    from: &Path,
    out: &Path,
    mut progress: impl FnMut(f32) -> bool,
) -> Result<Written, ExportError> {
    if !progress(0.0) {
        return Err(ExportError::Stopped);
    }
    // Packing walks the files itself and has nothing to report along the
    // way, so the bar simply says it is working.
    package::pack(from, out).map_err(|e| ExportError::Write(e.to_string()))?;
    progress(1.0);
    Ok(Written {
        frames: 0,
        bytes: std::fs::metadata(out).map(|m| m.len()).unwrap_or(0),
        width: 0,
        height: 0,
    })
}

/// A name for the GIF beside the recording it came from.
pub fn beside(recording: &Path) -> std::path::PathBuf {
    recording.with_extension("gif")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn flat(width: u32, height: u32, shade: u8) -> Vec<u8> {
        let mut v = vec![255u8; (width * height * 4) as usize];
        for px in v.as_chunks_mut::<4>().0 {
            px[0] = shade;
            px[1] = shade;
            px[2] = shade;
        }
        v
    }

    #[test]
    fn a_picture_already_small_enough_is_left_alone() {
        let pixels = flat(40, 30, 7);
        let (out, w, h) = shrink(&pixels, 40, 30, 900);
        assert_eq!((w, h), (40, 30));
        assert_eq!(out, pixels);
    }

    #[test]
    fn shrinking_keeps_the_shape_and_the_colour() {
        let pixels = flat(1920, 1080, 120);
        let (out, w, h) = shrink(&pixels, 1920, 1080, 900);
        assert_eq!(w, 900, "the limit is the width");
        assert_eq!(h, 506, "16:9 kept, rounded to an even number");
        assert_eq!(out.len(), (w * h * 4) as usize);
        // Averaging a flat picture gives back the same flat picture.
        assert!(
            out.as_chunks::<4>()
                .0
                .iter()
                .all(|px| px[0] == 120 && px[3] == 255)
        );
    }

    #[test]
    fn a_half_and_half_picture_averages_at_the_seam() {
        // Two columns, black then white, shrunk to one pixel wide: the one
        // pixel has to be the average rather than one side or the other.
        let mut pixels = vec![255u8; 8];
        pixels[0..3].copy_from_slice(&[0, 0, 0]);
        pixels[4..7].copy_from_slice(&[255, 255, 255]);
        let (out, w, _) = shrink(&pixels, 2, 1, 2);
        assert_eq!(w, 2, "a picture at the limit is not touched");
        assert_eq!(out.len(), 8);
    }

    #[test]
    fn the_defaults_are_the_ones_a_readme_wants() {
        let how = Gif::default();
        assert!(how.fps > 8.0 && how.fps <= 15.0);
        assert!(how.width <= 1000);
        // The frame rate has to survive being counted in hundredths.
        let delay = ((1.0 / how.fps) * 100.0).round();
        assert!(delay >= 2.0, "a delay of one is played back unpredictably");
    }

    #[test]
    fn a_still_picture_is_not_worth_a_frame() {
        let a = flat(8, 8, 100);
        let b = flat(8, 8, 100);
        assert_eq!(changed(&a, &b, 8, 8), None);
        // Below the slack: the encoder would not show it either.
        let c = flat(8, 8, 103);
        assert_eq!(changed(&a, &c, 8, 8), None, "three steps is not a change");
    }

    #[test]
    fn only_the_part_that_moved_is_written() {
        let a = flat(8, 8, 0);
        let mut b = a.clone();
        // One pixel, at (5, 3), turned white.
        let i = ((3 * 8) + 5) * 4;
        b[i..i + 3].copy_from_slice(&[255, 255, 255]);
        let area = changed(&a, &b, 8, 8).expect("something moved");
        assert_eq!(
            area,
            Area {
                x: 5,
                y: 3,
                w: 1,
                h: 1
            }
        );
        let patch = cut_out(&b, 8, area);
        assert_eq!(patch, vec![255, 255, 255, 255]);
    }

    #[test]
    fn every_format_lands_in_the_folder_it_was_given() {
        let dir = Path::new("C:/demos");
        assert!(into_folder(dir, "walkthrough", Format::Mp4).ends_with("walkthrough.mp4"));
        assert!(into_folder(dir, "walkthrough", Format::Gif).ends_with("walkthrough.gif"));
        assert!(
            into_folder(dir, "walkthrough", Format::Package).ends_with("walkthrough.screenilux")
        );
    }

    #[test]
    fn a_name_with_a_dot_in_it_keeps_the_whole_name() {
        // `with_extension` would take ".2 walkthrough" for an extension and
        // hand back "v1.mp4", losing most of what the file was called.
        let out = into_folder(Path::new("C:/demos"), "v1.2 walkthrough", Format::Mp4);
        assert_eq!(
            out.file_name().unwrap().to_string_lossy(),
            "v1.2 walkthrough.mp4"
        );
    }

    #[test]
    fn the_formats_are_offered_in_the_order_people_want_them() {
        assert_eq!(Format::ALL[0], Format::Mp4);
        // No two look alike in the menu, or the menu is no help.
        for a in Format::ALL {
            for b in Format::ALL {
                assert_eq!(a == b, a.label() == b.label());
                assert_eq!(a == b, a.ext() == b.ext());
            }
        }
    }

    #[test]
    fn red_and_blue_swap_on_the_way_to_the_encoder() {
        // Two pixels of a three-wide picture, cropped to the even width the
        // encoder was opened at.
        let rgba = vec![
            1, 2, 3, 4, 5, 6, 7, 8, 9, 9, 9, 9, // row 0: px0, px1, px2 (dropped)
            10, 11, 12, 13, 14, 15, 16, 17, 9, 9, 9, 9, // row 1
        ];
        let mut out = vec![0u8; 2 * 2 * 4];
        into_bgra(&rgba, 3, 2, 2, &mut out);
        assert_eq!(&out[0..4], &[3, 2, 1, 4]);
        assert_eq!(&out[4..8], &[7, 6, 5, 8]);
        assert_eq!(&out[8..12], &[12, 11, 10, 13]);
    }

    #[test]
    fn a_gif_is_named_beside_the_recording_it_came_from() {
        let from = Path::new("C:/demos/monday.mp4");
        assert_eq!(beside(from), Path::new("C:/demos/monday.gif"));
    }

    /// An MP4 export decodes as fast as the file allows and writes through
    /// the same sink the recorder uses, which makes it the other place a
    /// slow encoder could be outrun. Round-trip a short clip so the whole
    /// path is exercised rather than only its halves.
    #[test]
    fn an_mp4_export_round_trips_a_clip() {
        let dir = std::env::temp_dir().join(format!(
            "screenilux-export-test-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        std::fs::create_dir_all(&dir).expect("a working directory");
        let (w, h, fps) = (640u32, 480u32, 30u32);

        let source = dir.join("source.mp4");
        let mut sink =
            NativeSink::new(w, h, fps, false, dir.join("source-scratch.mp4")).expect("a sink");
        if sink.open_segment().is_err() {
            // No H.264 encoder on this machine, which is a legitimate
            // outcome rather than a test failure.
            let _ = std::fs::remove_dir_all(&dir);
            return;
        }
        for n in 0..60u32 {
            sink.write(&flat(w, h, (n * 4) as u8))
                .expect("a frame goes in");
        }
        sink.close_segment(0).expect("the segment closes");
        sink.finish(&source).expect("the clip is written");

        let recording = Recording::single(source.clone(), w, h);
        let out = dir.join("export.mp4");
        let done = mp4(
            &recording,
            &out,
            &EditList::default(),
            Mp4 {
                fps,
                ..Mp4::default()
            },
            |_| true,
        )
        .expect("the export runs");
        assert_eq!((done.width, done.height), (w, h));
        assert!(
            (50..=62).contains(&done.frames),
            "two seconds at {fps}fps, got {} frames",
            done.frames
        );
        assert!(out.is_file() && done.bytes > 1_000);

        // Effects are drawn into the file: a Hide over the whole picture
        // comes back as the dark it is filled with, not the grey under it.
        let mut edits = EditList::default();
        edits.add(
            0.0,
            2.0,
            screenilux_core::edit::Tool::Hide {
                area: screenilux_core::edit::Area::new(0.0, 0.0, 1.0, 1.0),
            },
        );
        let hidden = dir.join("hidden.mp4");
        mp4(&recording, &hidden, &edits, Mp4::default(), |_| true).expect("the export runs");
        let mut back = screenilux_platform::decoder::VideoDecoder::open(&hidden).expect("it reads");
        let mut middle = None;
        while let Ok(Some((t, frame))) = back.next_frame_if(|t| t > 0.9) {
            if t > 0.9 {
                middle = frame;
                break;
            }
        }
        let frame = middle.expect("a frame from the middle");
        let i = ((frame.height / 2 * frame.width + frame.width / 2) * 4) as usize;
        assert!(
            frame.rgba[i] < 40 && frame.rgba[i + 1] < 70,
            "the middle should be hidden: {:?}",
            &frame.rgba[i..i + 3]
        );

        // A vertical export is phone-shaped whatever the recording was.
        let tall = dir.join("tall.mp4");
        let done = mp4(
            &recording,
            &tall,
            &EditList::default(),
            Mp4 {
                fps,
                shape: Shape::Vertical,
            },
            |_| true,
        )
        .expect("the export runs");
        assert_eq!((done.width, done.height), (1080, 1920));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The sound goes through the edits with the picture: muted where it is
    /// muted, gone where it is cut, and quicker but not higher where it is
    /// sped up.
    #[test]
    fn an_mp4_export_carries_the_sound_through_the_edits() {
        use screenilux_core::edit::Tool;
        let dir = std::env::temp_dir().join(format!("screenilux-sound-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("a working directory");
        let (w, h, fps) = (320u32, 240u32, 30u32);
        let rate = audio::RATE as usize;

        // Four seconds of a 440 Hz tone under a grey picture.
        let source = dir.join("source.mp4");
        let mut sink =
            NativeSink::new(w, h, fps, true, dir.join("source-scratch.mp4")).expect("a sink");
        if sink.open_segment().is_err() {
            let _ = std::fs::remove_dir_all(&dir);
            return;
        }
        let per_frame = rate / fps as usize;
        for n in 0..(4 * fps) as usize {
            sink.write(&flat(w, h, 90)).expect("a frame goes in");
            let pcm: Vec<u8> = (0..per_frame)
                .flat_map(|i| {
                    let t = (n * per_frame + i) as f64 / rate as f64;
                    let v = ((t * 440.0 * std::f64::consts::TAU).sin() * 8000.0) as i16;
                    let b = v.to_le_bytes();
                    [b[0], b[1], b[0], b[1]]
                })
                .collect();
            sink.write_audio(&pcm).expect("sound goes in");
        }
        sink.close_segment(0).expect("the segment closes");
        sink.finish(&source).expect("the clip is written");

        // Muted for the first half second, the second second cut, and the
        // last two played twice as fast: two seconds in all.
        let mut edits = EditList::default();
        edits.add(0.0, 0.5, Tool::Mute);
        edits.add(1.0, 2.0, Tool::Cut);
        edits.add(2.0, 4.0, Tool::Speed { rate: 2.0 });
        let out = dir.join("export.mp4");
        mp4(
            &Recording::single(source, w, h),
            &out,
            &edits,
            Mp4 {
                fps,
                ..Mp4::default()
            },
            |_| true,
        )
        .expect("the export runs");

        let mut back =
            screenilux_platform::decoder::AudioDecoder::open(&out).expect("the export has sound");
        let mut left = Vec::new();
        while let Ok(Some((_, bytes))) = back.next_chunk() {
            left.extend(
                bytes
                    .as_chunks::<4>()
                    .0
                    .iter()
                    .map(|f| i16::from_le_bytes([f[0], f[1]])),
            );
        }
        let secs = left.len() as f64 / rate as f64;
        assert!((secs - 2.0).abs() < 0.15, "two seconds of sound, got {secs:.2}");

        let loudness = |from: f64, to: f64| {
            let part = &left[(from * rate as f64) as usize..(to * rate as f64) as usize];
            (part.iter().map(|&v| f64::from(v).powi(2)).sum::<f64>() / part.len() as f64).sqrt()
        };
        assert!(loudness(0.05, 0.4) < 300.0, "muted at the start");
        assert!(loudness(0.6, 0.95) > 3000.0, "heard after the mute");

        let sped = &left[(1.1 * rate as f64) as usize..(1.9 * rate as f64) as usize];
        let rises = sped.windows(2).filter(|p| p[0] < 0 && p[1] >= 0).count();
        let pitch = rises as f64 / 0.8;
        assert!((pitch - 440.0).abs() < 25.0, "sped up at its own pitch, got {pitch:.0} Hz");
        let _ = std::fs::remove_dir_all(&dir);
    }
}

/// A hand-run check against a real recording, since a GIF is judged by
/// looking at it. Give it `SCREENILUX_GIF_IN` and `SCREENILUX_GIF_OUT`.
#[cfg(test)]
mod against_a_real_recording {
    use super::*;

    #[test]
    #[ignore = "needs a recording on disk; set SCREENILUX_GIF_IN and _OUT"]
    fn writes_a_gif_of_it() {
        let Ok(input) = std::env::var("SCREENILUX_GIF_IN") else {
            return;
        };
        let out = std::env::var("SCREENILUX_GIF_OUT").expect("SCREENILUX_GIF_OUT");
        let edits = EditList::default();
        let path = std::path::PathBuf::from(&input);
        let info = screenilux_platform::probe(&path).expect("a video");
        let recording = Recording::single(path, info.width, info.height);
        let done = gif(&recording, Path::new(&out), &edits, Gif::default(), |_| {
            true
        })
        .expect("the GIF is written");
        println!("{done:?}");
        assert!(done.frames > 1);
        assert!(done.bytes > 1000);
    }
}
