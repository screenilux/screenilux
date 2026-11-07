//! The sound of an exported video: the recording's sound, put through the
//! same edits as the picture.
//!
//! A cut stretch is left out, a muted one is silent, and a held frame holds
//! only the picture, so the sound carries on under it. A sped-up stretch is
//! time-stretched rather than played faster: played faster, a voice goes
//! high and squeaky; stretched, it is quicker but still sounds like the
//! person who said it.
//!
//! The sound is made as the video is written, a frame's worth at a time, so
//! a long recording never has to be held in memory whole.

use std::collections::VecDeque;
use std::path::Path;

use screenilux_core::edit::{EditList, Tool};
use screenilux_platform::decoder::AudioDecoder;

use crate::ExportError;

/// What the encoder takes: 48 kHz, two channels, 16-bit.
pub const RATE: u32 = 48_000;
const CHANNELS: usize = 2;

/// What happens to the sound over a stretch of the recording.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Change {
    Cut,
    Speed(f32),
    Mute,
}

#[derive(Debug, Clone, Copy)]
struct Span {
    start: f64,
    end: f64,
    change: Change,
}

/// The finished video's sound, made as it is asked for.
pub struct Soundtrack {
    decoder: AudioDecoder,
    spans: Vec<Span>,
    /// Sound made but not yet handed over, as interleaved samples.
    ready: VecDeque<i16>,
    /// Frames handed over so far.
    given: u64,
    /// The sped-up stretch being gathered, and which one it is.
    gathering: Option<(usize, Vec<i16>)>,
    done: bool,
}

impl Soundtrack {
    /// The sound of `video` through `edits`, or `None` when it has none.
    pub fn open(video: &Path, edits: &EditList) -> Result<Option<Self>, ExportError> {
        let Ok(decoder) = AudioDecoder::open(video) else {
            return Ok(None);
        };
        let mut spans: Vec<Span> = edits
            .effects
            .iter()
            .filter_map(|e| {
                let change = match &e.tool {
                    Tool::Cut | Tool::Trim => Change::Cut,
                    Tool::Speed { rate } => Change::Speed(rate.max(0.05)),
                    Tool::Mute => Change::Mute,
                    _ => return None,
                };
                Some(Span {
                    start: e.start,
                    end: e.end,
                    change,
                })
            })
            .collect();
        spans.sort_by(|a, b| a.start.total_cmp(&b.start));
        Ok(Some(Self {
            decoder,
            spans,
            ready: VecDeque::new(),
            given: 0,
            gathering: None,
            done: false,
        }))
    }

    /// Sound up to `until` on the finished video's clock, as bytes for the
    /// encoder. At the end of the sound, silence keeps it as long as the
    /// picture.
    pub fn take_until(&mut self, until: f64) -> Result<Vec<u8>, ExportError> {
        let want = (until.max(0.0) * f64::from(RATE)).round() as u64;
        let need = want.saturating_sub(self.given) as usize * CHANNELS;
        while self.ready.len() < need && !self.done {
            self.read()?;
        }
        let mut out = Vec::with_capacity(need * 2);
        for _ in 0..need {
            let s = self.ready.pop_front().unwrap_or(0);
            out.extend_from_slice(&s.to_le_bytes());
        }
        self.given = want;
        Ok(out)
    }

    /// One more piece of the recording's sound, put through the edits.
    fn read(&mut self) -> Result<(), ExportError> {
        let Some((t0, bytes)) = self
            .decoder
            .next_chunk()
            .map_err(|e| ExportError::Read(e.to_string()))?
        else {
            self.done = true;
            self.flush();
            return Ok(());
        };
        let samples: Vec<i16> = bytes
            .as_chunks::<2>()
            .0
            .iter()
            .map(|b| i16::from_le_bytes(*b))
            .collect();
        const SILENT: [i16; CHANNELS] = [0; CHANNELS];
        for (j, frame) in samples.as_chunks::<CHANNELS>().0.iter().enumerate() {
            let t = t0 + j as f64 / f64::from(RATE);
            let within = |change: fn(Change) -> bool| {
                self.spans
                    .iter()
                    .position(|s| t >= s.start && t < s.end && change(s.change))
            };
            let cut = within(|c| c == Change::Cut).is_some();
            let muted = within(|c| c == Change::Mute).is_some();
            let sped = within(|c| matches!(c, Change::Speed(_)));
            // A sped-up stretch ends: stretch what was gathered and move on.
            if self
                .gathering
                .as_ref()
                .is_some_and(|(i, _)| cut || Some(*i) != sped)
            {
                self.flush();
            }
            if cut {
                continue;
            }
            let frame: &[i16] = if muted { &SILENT } else { frame };
            match sped {
                Some(i) => self
                    .gathering
                    .get_or_insert_with(|| (i, Vec::new()))
                    .1
                    .extend_from_slice(frame),
                None => self.ready.extend(frame.iter().copied()),
            }
        }
        Ok(())
    }

    fn flush(&mut self) {
        if let Some((i, gathered)) = self.gathering.take()
            && let Change::Speed(rate) = self.spans[i].change
        {
            self.ready.extend(stretch(&gathered, rate));
        }
    }
}

/// Interleaved stereo `input` made `rate` times shorter without changing its
/// pitch, by WSOLA: overlapping windows of the input laid down closer
/// together, each one shifted a little to where it best continues the one
/// before, so the waves line up rather than cancelling.
pub fn stretch(input: &[i16], rate: f32) -> Vec<i16> {
    const N: usize = 1024;
    const HOP: usize = N / 2;
    const TOLERANCE: isize = 256;
    let frames = input.len() / CHANNELS;
    let out_frames = (frames as f64 / f64::from(rate)).round() as usize;
    if out_frames == 0 {
        return Vec::new();
    }
    if frames < N * 2 {
        // Too short to stretch: keep evenly spaced frames instead.
        return (0..out_frames)
            .flat_map(|k| {
                let i = (k as f64 * f64::from(rate)) as usize;
                let i = i.min(frames - 1) * CHANNELS;
                [input[i], input[i + 1]]
            })
            .collect();
    }

    let mono: Vec<f32> = input
        .as_chunks::<CHANNELS>()
        .0
        .iter()
        .map(|f| (f32::from(f[0]) + f32::from(f[1])) / 2.0)
        .collect();
    let window: Vec<f32> = (0..N)
        .map(|i| 0.5 - 0.5 * (2.0 * std::f32::consts::PI * i as f32 / N as f32).cos())
        .collect();
    let mut out = vec![0f32; (out_frames + N) * CHANNELS];
    let mut weight = vec![0f32; out_frames + N];
    let last_start = (frames - N) as isize;
    let mut prev: isize = 0;

    let mut k = 0usize;
    while k * HOP < out_frames {
        let nominal = (k as f64 * HOP as f64 * f64::from(rate)) as isize;
        let at = if k == 0 {
            0
        } else {
            // The input that would naturally follow the window before, and
            // the place near the nominal one that matches it best.
            let natural = (prev + HOP as isize).min(last_start);
            let mut best = nominal.clamp(0, last_start);
            let mut best_score = f32::MIN;
            let mut d = -TOLERANCE;
            while d <= TOLERANCE {
                let c = (nominal + d).clamp(0, last_start);
                let mut score = 0.0;
                let mut i = 0;
                while i < N / 2 {
                    score += mono[(natural as usize) + i] * mono[(c as usize) + i];
                    i += 4;
                }
                if score > best_score {
                    best_score = score;
                    best = c;
                }
                d += 4;
            }
            best
        };
        let at = at.clamp(0, last_start) as usize;
        let o = k * HOP;
        for i in 0..N {
            let w = window[i];
            for c in 0..CHANNELS {
                out[(o + i) * CHANNELS + c] += f32::from(input[(at + i) * CHANNELS + c]) * w;
            }
            weight[o + i] += w;
        }
        prev = at as isize;
        k += 1;
    }

    (0..out_frames * CHANNELS)
        .map(|i| {
            let w = weight[i / CHANNELS];
            let v = if w > 1e-3 { out[i] / w } else { 0.0 };
            v.round().clamp(f32::from(i16::MIN), f32::from(i16::MAX)) as i16
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A sine at `hz` for `secs`, in both channels.
    fn tone(hz: f32, secs: f32) -> Vec<i16> {
        let n = (secs * RATE as f32) as usize;
        (0..n)
            .flat_map(|i| {
                let v = (2.0 * std::f32::consts::PI * hz * i as f32 / RATE as f32).sin() * 8000.0;
                [v as i16, v as i16]
            })
            .collect()
    }

    /// The tone's pitch, by counting rising zero crossings.
    fn pitch(pcm: &[i16]) -> f32 {
        let mono: Vec<i16> = pcm.as_chunks::<2>().0.iter().map(|f| f[0]).collect();
        let rises = mono.windows(2).filter(|w| w[0] < 0 && w[1] >= 0).count();
        rises as f32 / (mono.len() as f32 / RATE as f32)
    }

    #[test]
    fn stretching_shortens_without_changing_pitch() {
        let input = tone(440.0, 2.0);
        let out = stretch(&input, 2.0);
        assert_eq!(out.len() / 2, input.len() / 2 / 2, "half as long");
        let p = pitch(&out);
        assert!((p - 440.0).abs() < 15.0, "pitch moved to {p}");
    }

    #[test]
    fn slowing_down_works_too() {
        let input = tone(300.0, 1.0);
        let out = stretch(&input, 0.5);
        assert_eq!(out.len() / 2, input.len() / 2 * 2);
        assert!((pitch(&out) - 300.0).abs() < 15.0);
    }

    #[test]
    fn a_tiny_stretch_still_comes_out_the_right_length() {
        let input = tone(440.0, 0.01);
        let out = stretch(&input, 3.0);
        assert_eq!(out.len() / 2, ((input.len() / 2) as f64 / 3.0).round() as usize);
    }
}
