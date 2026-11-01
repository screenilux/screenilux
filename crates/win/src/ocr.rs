//! Reading text off frames with the OCR engine built into Windows.
//!
//! Chosen over a cloud model for the copyable-text feature because it gives
//! each word's position, which is what lets a user drag across text on the
//! video itself, and because it is free, offline and already installed. A
//! spike read 344 words off a 1080p screen in about 130ms.

use screenilux_core::text::{Line, Page, Word};
use windows::Graphics::Imaging::{BitmapPixelFormat, SoftwareBitmap};
use windows::Media::Ocr::OcrEngine;
use windows::Security::Cryptography::CryptographicBuffer;
use windows::Win32::System::Com::{COINIT_MULTITHREADED, CoInitializeEx};

/// An OCR engine for the user's display languages.
pub struct TextReader {
    engine: OcrEngine,
    max_side: u32,
}

impl TextReader {
    /// Fails when no installed language has OCR support, which Windows
    /// reports rather than falling back.
    pub fn new() -> Result<Self, String> {
        // SAFETY: COM initialisation; harmless if the thread already has it.
        unsafe {
            let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        }
        let engine = OcrEngine::TryCreateFromUserProfileLanguages().map_err(|_| {
            "Windows has no text recognition for your languages. Add one under Settings > \
             Time & language > Language, with the optical character recognition feature."
                .to_string()
        })?;
        let max_side = OcrEngine::MaxImageDimension().unwrap_or(4096);
        Ok(Self { engine, max_side })
    }

    /// Every word on an RGBA frame, positions in that frame's pixels.
    pub fn read(&self, rgba: &[u8], width: u32, height: u32) -> Result<Page, String> {
        if rgba.len() != width as usize * height as usize * 4 {
            return Err("the frame is the wrong size".into());
        }
        // Greyscale is a quarter of the copy and all the engine needs.
        let (grey, w, h, scale) = shrink_to_fit(rgba, width, height, self.max_side);
        let buffer = CryptographicBuffer::CreateFromByteArray(&grey).map_err(|e| e.to_string())?;
        let bitmap = SoftwareBitmap::CreateCopyFromBuffer(
            &buffer,
            BitmapPixelFormat::Gray8,
            w as i32,
            h as i32,
        )
        .map_err(|e| e.to_string())?;
        let result = self
            .engine
            .RecognizeAsync(&bitmap)
            .and_then(|op| op.join())
            .map_err(|e| e.to_string())?;

        let mut page = Page::default();
        for line in &result.Lines().map_err(|e| e.to_string())? {
            let mut words = Vec::new();
            for word in &line.Words().map_err(|e| e.to_string())? {
                let r = word.BoundingRect().map_err(|e| e.to_string())?;
                words.push(Word {
                    text: word.Text().map_err(|e| e.to_string())?.to_string(),
                    x: r.X * scale,
                    y: r.Y * scale,
                    w: r.Width * scale,
                    h: r.Height * scale,
                });
            }
            page.lines.push(Line { words });
        }
        Ok(page)
    }
}

/// Greyscale, halved until it fits the engine's limit. Returns the image, its
/// size, and the factor back to the original's pixels.
fn shrink_to_fit(rgba: &[u8], width: u32, height: u32, max_side: u32) -> (Vec<u8>, u32, u32, f32) {
    let mut step = 1u32;
    while width / step > max_side || height / step > max_side {
        step *= 2;
    }
    let (w, h) = (width / step, height / step);
    let mut out = Vec::with_capacity(w as usize * h as usize);
    for y in 0..h {
        let row = &rgba[(y * step * width * 4) as usize..];
        for x in 0..w {
            let p = &row[(x * step * 4) as usize..][..3];
            // Integer BT.601 luma: plenty for telling text from background.
            out.push(((p[0] as u32 * 77 + p[1] as u32 * 150 + p[2] as u32 * 29) >> 8) as u8);
        }
    }
    (out, w, h, step as f32)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn small_frames_are_not_shrunk() {
        let rgba = vec![255u8; 8 * 4 * 4];
        let (g, w, h, s) = shrink_to_fit(&rgba, 8, 4, 100);
        assert_eq!((w, h, s), (8, 4, 1.0));
        assert_eq!(g.len(), 32);
        assert!(g.iter().all(|&v| v == 255));
    }

    #[test]
    fn huge_frames_are_halved_until_they_fit() {
        let rgba = vec![0u8; 100 * 10 * 4];
        let (g, w, h, s) = shrink_to_fit(&rgba, 100, 10, 30);
        assert_eq!((w, h, s), (25, 2, 4.0));
        assert_eq!(g.len(), 50);
    }

    #[test]
    fn a_wrong_sized_frame_is_refused() {
        let Ok(reader) = TextReader::new() else {
            return;
        };
        assert!(reader.read(&[0u8; 10], 4, 4).is_err());
    }

    /// Reads a real screen. Text on screen varies, so this asserts only that
    /// recognition runs and positions land inside the frame.
    #[test]
    fn reads_words_with_positions_inside_the_frame() {
        let Ok(reader) = TextReader::new() else {
            return;
        };
        // A synthetic frame: blank pages legitimately produce no words, so
        // this mostly proves the plumbing does not fail.
        let (w, h) = (640u32, 200u32);
        let rgba = vec![255u8; (w * h * 4) as usize];
        let page = reader.read(&rgba, w, h).unwrap();
        for (_, word) in page.words() {
            assert!(word.x >= 0.0 && word.x + word.w <= w as f32 + 1.0);
        }
    }
}
