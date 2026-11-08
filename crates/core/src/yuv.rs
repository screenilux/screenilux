//! NV12 to RGBA, for frames coming out of a hardware decoder.
//!
//! Asking Media Foundation for RGB costs roughly a fifth of its decode speed
//! (about 100fps at 1080p against 540 for NV12), because its colour converter
//! is slow. Converting here is one pass that also produces exactly what the UI
//! uploads: RGBA rather than BGRA, opaque alpha, and the codec's padding rows
//! (1080 is coded as 1088) cropped off.

/// Which YUV matrix the stream was encoded with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Matrix {
    /// SD content.
    Bt601,
    /// HD content, and what encoders pick from 720p up.
    Bt709,
}

impl Matrix {
    /// The conventional choice when a stream does not say.
    pub fn for_height(height: u32) -> Self {
        if height >= 720 {
            Matrix::Bt709
        } else {
            Matrix::Bt601
        }
    }

    /// `(r_v, g_u, g_v, b_u)` scaled by 1024, for limited-range input.
    const fn coefficients(self) -> (i32, i32, i32, i32) {
        match self {
            Matrix::Bt601 => (1634, 401, 832, 2066),
            Matrix::Bt709 => (1836, 218, 546, 2163),
        }
    }

    /// The other direction: rows for Y, U and V, each `(r, g, b)` scaled by
    /// 1024, producing limited range. Each chroma row sums to zero, so grey
    /// stays exactly grey.
    const fn forward(self) -> [(i32, i32, i32); 3] {
        match self {
            Matrix::Bt601 => [(263, 516, 100), (-152, -298, 450), (450, -377, -73)],
            Matrix::Bt709 => [(187, 629, 63), (-103, -347, 450), (450, -409, -41)],
        }
    }
}

/// Bytes an NV12 buffer of `stride` and `rows` (luma rows, padding included)
/// must hold.
pub const fn nv12_len(stride: usize, rows: usize) -> usize {
    stride * rows + stride * rows.div_ceil(2)
}

/// Convert one NV12 frame into tightly packed RGBA8.
///
/// `rows` is the number of luma rows in the buffer, which may exceed `height`
/// by the codec's padding; the chroma plane starts after all of them.
///
/// # Panics
/// If `src` is shorter than [`nv12_len`] or `out` is not `width * height * 4`.
pub fn nv12_to_rgba(
    src: &[u8],
    stride: usize,
    rows: usize,
    width: usize,
    height: usize,
    matrix: Matrix,
    out: &mut [u8],
) {
    assert!(
        rows >= height && stride >= width,
        "frame larger than buffer"
    );
    assert!(src.len() >= nv12_len(stride, rows), "NV12 buffer too short");
    assert_eq!(out.len(), width * height * 4, "output is the wrong size");

    let (rv, gu, gv, bu) = matrix.coefficients();
    let (luma, chroma) = src.split_at(stride * rows);

    for (y, out_row) in out.chunks_exact_mut(width * 4).enumerate() {
        let y_row = &luma[y * stride..][..width];
        let uv_row = &chroma[(y / 2) * stride..][..width.next_multiple_of(2).min(stride)];
        for (x, px) in out_row.as_chunks_mut::<4>().0.iter_mut().enumerate() {
            let c = (y_row[x] as i32 - 16).max(0) * 1192; // 1.164 * 1024
            let u = uv_row[x & !1] as i32 - 128;
            let v = uv_row[(x & !1) + 1] as i32 - 128;
            px[0] = clamp((c + rv * v) >> 10);
            px[1] = clamp((c - gu * u - gv * v) >> 10);
            px[2] = clamp((c + bu * u) >> 10);
            px[3] = 255;
        }
    }
}

/// Convert tightly packed BGRA8 into NV12 for an encoder, limited range.
///
/// The recorder's frames are BGRA on the CPU, and the hardware encoders take
/// NV12. Handing Media Foundation BGRA makes it insert its own colour
/// converter, the same slow one [`nv12_to_rgba`] exists to avoid on the way
/// out: it was most of a recording's processor time, and a recording that
/// eats a core slows down whatever is being recorded.
///
/// The luma stride is `width` rounded up to even, so an odd width still has
/// whole chroma pairs; each chroma sample averages the 2x2 block above it,
/// repeating the last row or column at an odd edge.
///
/// # Panics
/// If `src` is not `width * height * 4` or `out` is not
/// [`nv12_len`]`(stride, height)`.
pub fn bgra_to_nv12(src: &[u8], width: usize, height: usize, matrix: Matrix, out: &mut [u8]) {
    let stride = width.next_multiple_of(2);
    assert_eq!(src.len(), width * height * 4, "input is the wrong size");
    assert_eq!(
        out.len(),
        nv12_len(stride, height),
        "output is the wrong size"
    );
    if width == 0 || height == 0 {
        return;
    }
    let (luma, chroma) = out.split_at_mut(stride * height);
    for ((pair, luma), uv) in src
        .chunks(width * 4 * 2)
        .zip(luma.chunks_mut(stride * 2))
        .zip(chroma.chunks_mut(stride))
    {
        row_pair_to_nv12(pair, luma, uv, width, stride, matrix);
    }
}

/// One or two source rows into their luma rows and the chroma row they share.
fn row_pair_to_nv12(
    src: &[u8],
    luma: &mut [u8],
    uv: &mut [u8],
    width: usize,
    stride: usize,
    matrix: Matrix,
) {
    let [ky, ku, kv] = matrix.forward();
    let to_y =
        |(r, g, b): (i32, i32, i32)| clamp(16 + ((ky.0 * r + ky.1 * g + ky.2 * b + 512) >> 10));

    let top = src[..width * 4].as_chunks::<4>().0;
    let bottom = if src.len() > width * 4 {
        src[width * 4..].as_chunks::<4>().0
    } else {
        top
    };
    let (y_top, y_bottom) = luma.split_at_mut(stride);
    let rgb = |p: &[u8; 4]| (p[2] as i32, p[1] as i32, p[0] as i32);

    for (cx, pair) in uv.as_chunks_mut::<2>().0.iter_mut().enumerate() {
        let x0 = cx * 2;
        let x1 = (x0 + 1).min(width - 1);
        let px = [
            rgb(&top[x0]),
            rgb(&top[x1]),
            rgb(&bottom[x0]),
            rgb(&bottom[x1]),
        ];

        y_top[x0] = to_y(px[0]);
        y_top[x0 + 1] = to_y(px[1]);
        if !y_bottom.is_empty() {
            y_bottom[x0] = to_y(px[2]);
            y_bottom[x0 + 1] = to_y(px[3]);
        }

        let (r, g, b) = px
            .iter()
            .fold((0, 0, 0), |s, p| (s.0 + p.0, s.1 + p.1, s.2 + p.2));
        // Four pixels summed, so two more bits of shift than for luma.
        pair[0] = clamp(128 + ((ku.0 * r + ku.1 * g + ku.2 * b + 2048) >> 12));
        pair[1] = clamp(128 + ((kv.0 * r + kv.1 * g + kv.2 * b + 2048) >> 12));
    }
}

#[inline]
fn clamp(v: i32) -> u8 {
    v.clamp(0, 255) as u8
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Encode one RGB colour to limited-range YUV with the same matrix, for
    /// round-trip checks.
    fn yuv(r: f64, g: f64, b: f64, m: Matrix) -> (u8, u8, u8) {
        let (kr, kb) = match m {
            Matrix::Bt601 => (0.299, 0.114),
            Matrix::Bt709 => (0.2126, 0.0722),
        };
        let kg = 1.0 - kr - kb;
        let y = kr * r + kg * g + kb * b;
        let u = (b - y) / (2.0 * (1.0 - kb));
        let v = (r - y) / (2.0 * (1.0 - kr));
        (
            (16.0 + 219.0 * y / 255.0).round() as u8,
            (128.0 + 224.0 * u / 255.0).round() as u8,
            (128.0 + 224.0 * v / 255.0).round() as u8,
        )
    }

    fn flat(w: usize, rows: usize, stride: usize, (y, u, v): (u8, u8, u8)) -> Vec<u8> {
        let mut buf = vec![0u8; nv12_len(stride, rows)];
        buf[..stride * rows].fill(y);
        for pair in buf[stride * rows..].as_chunks_mut::<2>().0 {
            pair[0] = u;
            pair[1] = v;
        }
        let _ = w;
        buf
    }

    #[test]
    fn colours_survive_a_round_trip() {
        for m in [Matrix::Bt601, Matrix::Bt709] {
            for rgb in [
                (220.0, 0.0, 0.0),
                (0.0, 200.0, 0.0),
                (0.0, 0.0, 220.0),
                (255.0, 255.0, 255.0),
                (0.0, 0.0, 0.0),
                (128.0, 64.0, 32.0),
            ] {
                let src = flat(4, 2, 4, yuv(rgb.0, rgb.1, rgb.2, m));
                let mut out = vec![0u8; 4 * 2 * 4];
                nv12_to_rgba(&src, 4, 2, 4, 2, m, &mut out);
                let got = (out[0] as f64, out[1] as f64, out[2] as f64);
                for (a, b) in [(got.0, rgb.0), (got.1, rgb.1), (got.2, rgb.2)] {
                    assert!((a - b).abs() <= 3.0, "{m:?} {rgb:?} came back {got:?}");
                }
                assert_eq!(out[3], 255, "alpha is opaque");
            }
        }
    }

    #[test]
    fn padding_rows_are_cropped_and_chroma_found_after_them() {
        // 4x2 visible inside a 6-wide, 4-row coded buffer.
        let (stride, rows) = (6, 4);
        let mut src = flat(4, rows, stride, (16, 128, 128)); // black
        // Make the padding rows white; they must not show up.
        src[2 * stride..4 * stride].fill(235);
        let mut out = vec![0u8; 4 * 2 * 4];
        nv12_to_rgba(&src, stride, rows, 4, 2, Matrix::Bt709, &mut out);
        assert!(out.as_chunks::<4>().0.iter().all(|p| p[..3] == [0, 0, 0]));
    }

    #[test]
    fn odd_sizes_do_not_read_past_the_row() {
        let (stride, rows) = (6, 4);
        let src = flat(5, rows, stride, (126, 128, 128));
        let mut out = vec![0u8; 5 * 3 * 4];
        nv12_to_rgba(&src, stride, rows, 5, 3, Matrix::Bt601, &mut out);
        assert!(
            out.as_chunks::<4>()
                .0
                .iter()
                .all(|p| p[0] == p[1] && p[1] == p[2])
        );
    }

    fn bgra(w: usize, h: usize, (r, g, b): (u8, u8, u8)) -> Vec<u8> {
        [b, g, r, 255].repeat(w * h)
    }

    #[test]
    fn colours_survive_encoding_and_decoding() {
        for m in [Matrix::Bt601, Matrix::Bt709] {
            for rgb in [
                (220, 0, 0),
                (0, 200, 0),
                (0, 0, 220),
                (255, 255, 255),
                (0, 0, 0),
                (128, 64, 32),
                (37, 37, 37),
            ] {
                let mut nv12 = vec![0u8; nv12_len(4, 2)];
                bgra_to_nv12(&bgra(4, 2, rgb), 4, 2, m, &mut nv12);
                let want = yuv(rgb.0 as f64, rgb.1 as f64, rgb.2 as f64, m);
                let got = (nv12[0], nv12[8], nv12[9]);
                // Integer coefficients round a level differently at a half.
                assert!(
                    got.0.abs_diff(want.0) <= 1
                        && got.1.abs_diff(want.1) <= 1
                        && got.2.abs_diff(want.2) <= 1,
                    "{m:?} {rgb:?} encoded as {got:?}, the reference says {want:?}"
                );
                let mut back = vec![0u8; 4 * 2 * 4];
                nv12_to_rgba(&nv12, 4, 2, 4, 2, m, &mut back);
                for (a, b) in [(back[0], rgb.0), (back[1], rgb.1), (back[2], rgb.2)] {
                    assert!(a.abs_diff(b) <= 3, "{m:?} {rgb:?} came back {back:?}");
                }
            }
        }
    }

    #[test]
    fn grey_has_no_colour() {
        let mut nv12 = vec![0u8; nv12_len(2, 2)];
        bgra_to_nv12(&bgra(2, 2, (90, 90, 90)), 2, 2, Matrix::Bt709, &mut nv12);
        assert_eq!((nv12[4], nv12[5]), (128, 128));
    }

    #[test]
    fn rows_land_top_first_and_chroma_averages_its_block() {
        // Left column red, right column blue, top row bright, bottom dark.
        let (w, h) = (2, 2);
        let mut src = Vec::new();
        src.extend_from_slice(&[0, 0, 255, 255, 255, 0, 0, 255]);
        src.extend_from_slice(&[0, 0, 60, 255, 60, 0, 0, 255]);
        let mut nv12 = vec![0u8; nv12_len(w, h)];
        bgra_to_nv12(&src, w, h, Matrix::Bt709, &mut nv12);
        assert!(nv12[0] > nv12[2], "the top row is the brighter one");
        assert!(
            nv12[0] > nv12[1],
            "red is brighter than blue at the same level"
        );
        let (u, v) = (nv12[4], nv12[5]);
        assert!(u > 128 && v > 128, "half red, half blue is purple: {u} {v}");
    }

    #[test]
    fn odd_sizes_fill_whole_chroma_pairs() {
        let (w, h) = (5, 3);
        let stride = 6;
        let mut nv12 = vec![7u8; nv12_len(stride, h)];
        bgra_to_nv12(&bgra(w, h, (200, 30, 30)), w, h, Matrix::Bt601, &mut nv12);
        let (luma, chroma) = nv12.split_at(stride * h);
        for row in luma.chunks(stride) {
            assert!(row[..w].iter().all(|&y| y == luma[0]), "{row:?}");
        }
        assert!(
            chroma
                .as_chunks::<2>()
                .0
                .iter()
                .all(|p| *p == [chroma[0], chroma[1]])
        );
    }

    #[test]
    fn hd_heights_default_to_bt709() {
        assert_eq!(Matrix::for_height(1080), Matrix::Bt709);
        assert_eq!(Matrix::for_height(480), Matrix::Bt601);
    }
}
