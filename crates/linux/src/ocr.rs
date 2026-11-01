//! Reading text off frames with Tesseract.
//!
//! Tesseract is the open source OCR engine most Linux systems package. It
//! runs offline and gives each word's position, which is what lets a user
//! drag across text on the video. It is run as a program rather than linked,
//! so Screenilux still starts without it and says what to install.

use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};

use screenilux_core::text::{Line, Page, Word};

pub struct TextReader {
    program: PathBuf,
}

impl TextReader {
    /// Find Tesseract, or say how to get it.
    pub fn new() -> Result<Self, String> {
        let program = PathBuf::from("tesseract");
        let found = Command::new(&program)
            .arg("--version")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|s| s.success());
        if !found {
            return Err(
                "Reading text needs Tesseract. Install it with your package manager, \
                        for example: sudo apt install tesseract-ocr"
                    .into(),
            );
        }
        Ok(Self { program })
    }

    /// Every word on an RGBA frame, positions in that frame's pixels.
    pub fn read(&self, rgba: &[u8], width: u32, height: u32) -> Result<Page, String> {
        if rgba.len() != width as usize * height as usize * 4 {
            return Err("the frame is the wrong size".into());
        }
        // Greyscale is all the engine needs, in the simplest image format
        // it reads.
        let mut image = format!("P5\n{width} {height}\n255\n").into_bytes();
        image.extend(
            rgba.as_chunks::<4>()
                .0
                .iter()
                .map(|p| ((p[0] as u32 * 77 + p[1] as u32 * 150 + p[2] as u32 * 29) >> 8) as u8),
        );
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let file = std::env::temp_dir().join(format!(
            "screenilux-ocr-{}-{}.pgm",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::File::create(&file)
            .and_then(|mut f| f.write_all(&image))
            .map_err(|e| e.to_string())?;
        let out = Command::new(&self.program)
            .arg(&file)
            .args(["stdout", "tsv"])
            .stderr(Stdio::null())
            .output();
        let _ = std::fs::remove_file(&file);
        let out = out.map_err(|e| e.to_string())?;
        if !out.status.success() {
            return Err("Tesseract could not read the frame".into());
        }
        Ok(parse_tsv(&String::from_utf8_lossy(&out.stdout)))
    }
}

/// Tesseract's TSV output as lines of words. Word rows are level 5, and a
/// line is everything sharing a block, paragraph and line number.
pub fn parse_tsv(tsv: &str) -> Page {
    let mut page = Page::default();
    let mut current: Option<(u32, u32, u32)> = None;
    for row in tsv.lines().skip(1) {
        let cols: Vec<&str> = row.split('\t').collect();
        if cols.len() < 12 || cols[0] != "5" {
            continue;
        }
        let text = cols[11].trim();
        let confident = cols[10].parse::<f32>().is_ok_and(|c| c >= 0.0);
        if text.is_empty() || !confident {
            continue;
        }
        let num = |i: usize| cols[i].parse::<u32>().unwrap_or(0);
        let key = (num(2), num(3), num(4));
        if current != Some(key) {
            page.lines.push(Line { words: Vec::new() });
            current = Some(key);
        }
        let f = |i: usize| cols[i].parse::<f32>().unwrap_or(0.0);
        if let Some(line) = page.lines.last_mut() {
            line.words.push(Word {
                text: text.to_string(),
                x: f(6),
                y: f(7),
                w: f(8),
                h: f(9),
            });
        }
    }
    page
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "level\tpage_num\tblock_num\tpar_num\tline_num\tword_num\tleft\ttop\twidth\theight\tconf\ttext
1\t1\t0\t0\t0\t0\t0\t0\t640\t480\t-1\t
4\t1\t1\t1\t1\t0\t10\t10\t200\t20\t-1\t
5\t1\t1\t1\t1\t1\t10\t10\t80\t20\t96.5\tLaunch
5\t1\t1\t1\t1\t2\t95\t10\t40\t20\t95.1\tplan
5\t1\t1\t1\t2\t1\t10\t40\t60\t20\t91.0\tOwner:
5\t1\t1\t1\t2\t2\t75\t40\t5\t20\t-1\t
";

    #[test]
    fn words_are_grouped_into_lines_with_their_boxes() {
        let page = parse_tsv(SAMPLE);
        assert_eq!(page.lines.len(), 2);
        let first: Vec<&str> = page.lines[0]
            .words
            .iter()
            .map(|w| w.text.as_str())
            .collect();
        assert_eq!(first, ["Launch", "plan"]);
        let w = &page.lines[0].words[1];
        assert_eq!((w.x, w.y, w.w, w.h), (95.0, 10.0, 40.0, 20.0));
        assert_eq!(
            page.lines[1].words.len(),
            1,
            "empty and unconfident words are dropped"
        );
    }

    #[test]
    fn reads_real_text_when_tesseract_is_installed() {
        let Ok(reader) = TextReader::new() else {
            return;
        };
        // A blank frame has no words, and reading it must not fail.
        let page = reader.read(&vec![255u8; 64 * 32 * 4], 64, 32).unwrap();
        assert!(page.lines.iter().all(|l| l.words.is_empty()) || page.lines.is_empty());
    }
}
