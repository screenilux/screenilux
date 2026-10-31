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

    #[test]
    fn hd_heights_default_to_bt709() {
        assert_eq!(Matrix::for_height(1080), Matrix::Bt709);
        assert_eq!(Matrix::for_height(480), Matrix::Bt601);
    }
}
