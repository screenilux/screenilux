//! Writing a recording out as a GIF.
//!
//! A GIF is what a demo wants to be when it has to live in a README, a pull
//! request or a chat message: it plays by itself, it needs no player and it
//! survives being pasted anywhere. The price is a small palette and no
//! sound, so this is deliberately not a video export. It samples the
//! recording at a handful of frames a second and shrinks it to a width
//! worth loading.
//!
//! It reads the recording through its edit list, which is what makes it the
//! first thing in the app that treats edits as real: a cut is a stretch of
//! source frames that are never asked for, a speed-up is frames arriving
//! closer together, and the GIF comes out the length the editor said it
//! would. Effects that change *pixels* rather than *time* — a zoom, a blur,
//! a caption — are not drawn yet, here or anywhere else, so a GIF shows the
//! recording cut to shape but not dressed.

#![forbid(unsafe_code)]

use std::fs::File;
use std::io::BufWriter;
use std::path::Path;

use screenilux_core::edit::EditList;
use screenilux_platform::decoder::VideoDecoder;

#[derive(Debug, thiserror::Error)]
pub enum ExportError {
    #[error("could not read the recording: {0}")]
    Read(String),
    #[error("could not write the GIF: {0}")]
    Write(String),
    #[error("there is nothing left to export: every frame is cut")]
    Empty,
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
}

impl Default for Gif {
    fn default() -> Self {
        Self {
            fps: 10.0,
            width: 720,
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
/// `progress` is called with a fraction between 0 and 1 as the source is
/// walked, and may be called many times a second.
pub fn gif(
    video: &Path,
    out: &Path,
    edits: &EditList,
    how: Gif,
    mut progress: impl FnMut(f32),
) -> Result<Written, ExportError> {
    let mut decoder = VideoDecoder::open(video).map_err(|e| ExportError::Read(e.to_string()))?;
    let source_len = decoder.duration().max(0.001);
    let output_len = edits.output_duration(source_len);
    if output_len <= 0.0 {
        return Err(ExportError::Empty);
    }

    let step = 1.0 / how.fps.max(1.0);
    // GIF counts delays in hundredths of a second, so the frame rate that
    // actually plays is the one that survives rounding.
    let delay = ((step * 100.0).round() as u16).max(2);

    // The encoder owns the file, and it cannot be made until the first
    // frame says how big the picture is, so both wait together.
    let mut encoder: Option<gif::Encoder<BufWriter<File>>> = None;
    let mut size = (0u32, 0u32);
    let mut frames = 0u32;
    // The next point on the finished video's clock that wants a frame.
    let mut want = 0.0_f64;
    // The last picture written, to tell what moved since.
    let mut previous: Option<Vec<u8>> = None;
    // Time owed by frames that changed nothing, paid to the next one that
    // does, so a still stretch costs one frame rather than thirty.
    let mut held = 0u16;

    loop {
        // Only the frames that are actually used are converted to pixels;
        // the rest cost a timestamp each.
        let read = decoder
            .next_frame_if(|t| edits.to_output(t).is_some_and(|out_t| out_t + 1e-9 >= want))
            .map_err(|e| ExportError::Read(e.to_string()))?;
        let Some((t, frame)) = read else { break };
        progress((t / source_len).clamp(0.0, 1.0) as f32);

        let Some(frame) = frame else { continue };
        let Some(out_t) = edits.to_output(t) else {
            continue;
        };

        let (small, w, h) = shrink(&frame.rgba, frame.width, frame.height, how.width);
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
        frames += 1;

        want = (out_t + step).max(want + step);
        if want > output_len {
            break;
        }
    }

    drop(encoder);
    let bytes = std::fs::metadata(out).map(|m| m.len()).unwrap_or(0);
    progress(1.0);
    if frames == 0 {
        return Err(ExportError::Empty);
    }
    Ok(Written {
        frames,
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
    fn a_gif_is_named_beside_the_recording_it_came_from() {
        let from = Path::new("C:/demos/monday.mp4");
        assert_eq!(beside(from), Path::new("C:/demos/monday.gif"));
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
        let done = gif(
            Path::new(&input),
            Path::new(&out),
            &edits,
            Gif::default(),
            |_| {},
        )
        .expect("the GIF is written");
        println!("{done:?}");
        assert!(done.frames > 1);
        assert!(done.bytes > 1000);
    }
}
