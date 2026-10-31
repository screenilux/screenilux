//! The sound of a mouse click, mixed into a recording's audio.
//!
//! A real click is two small mechanical events: the switch closing on press
//! and, a few tens of milliseconds later, opening again on release, quieter.
//! Each is a sharp tick with a little body under it. Synthesised here rather
//! than shipped as a sample so there is no asset to load and the level is
//! set in one place: quiet, the way a click sounds through headphones, under
//! any voice rather than over it.

/// Seconds between the press and the release.
const RELEASE_AFTER: f64 = 0.075;
/// The release is softer than the press.
const RELEASE_GAIN: f64 = 0.45;
/// Peak level, as a fraction of full scale: about -17 dBFS.
const PEAK: f64 = 0.14;

/// One tick: a burst of noise for the edge, and two decaying tones for the
/// body, which is what makes it sound like plastic rather than static.
fn tick(t: f64, noise: f64) -> f64 {
    let edge = noise * (-t / 0.0006).exp();
    let high = (std::f64::consts::TAU * 1800.0 * t).sin() * (-t / 0.0015).exp();
    let low = (std::f64::consts::TAU * 420.0 * t).sin() * (-t / 0.004).exp();
    0.5 * edge + 0.35 * high + 0.25 * low
}

/// The click at `rate` samples per second, mono, 16-bit.
pub fn click_sound(rate: u32) -> Vec<i16> {
    let rate = rate.max(1) as f64;
    // Long enough for the release tick to die away.
    let len = ((RELEASE_AFTER + 0.02) * rate) as usize;
    // A fixed linear congruential sequence: the same click every time.
    let mut seed: u32 = 0x1234_5678;
    let mut raw: Vec<f64> = (0..len)
        .map(|i| {
            seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            let noise = (seed >> 8) as f64 / (1u32 << 24) as f64 * 2.0 - 1.0;
            let t = i as f64 / rate;
            let mut s = tick(t, noise);
            if t >= RELEASE_AFTER {
                s += RELEASE_GAIN * tick(t - RELEASE_AFTER, noise);
            }
            s
        })
        .collect();
    let peak = raw.iter().fold(0.0f64, |m, s| m.max(s.abs())).max(1e-9);
    for s in &mut raw {
        *s *= PEAK / peak;
    }
    raw.iter()
        .map(|s| (s * i16::MAX as f64).round() as i16)
        .collect()
}

/// Mixes clicks into 16-bit interleaved PCM as it passes through.
pub struct ClickMixer {
    sound: Vec<i16>,
    channels: usize,
    /// How far into the sound each click still sounding has got.
    voices: Vec<usize>,
}

impl ClickMixer {
    pub fn new(rate: u32, channels: u16) -> Self {
        Self {
            sound: click_sound(rate),
            channels: channels.max(1) as usize,
            voices: Vec::new(),
        }
    }

    /// Start a click at the next sample mixed.
    pub fn trigger(&mut self) {
        self.voices.push(0);
    }

    /// Whether any click is still sounding.
    pub fn is_sounding(&self) -> bool {
        !self.voices.is_empty()
    }

    /// Add the sounding clicks into `pcm`: little-endian 16-bit samples,
    /// channels interleaved. Clips rather than wraps.
    pub fn mix(&mut self, pcm: &mut [u8]) {
        if self.voices.is_empty() {
            return;
        }
        let frame_bytes = 2 * self.channels;
        for frame in pcm.chunks_exact_mut(frame_bytes) {
            let mut add = 0i32;
            for pos in &mut self.voices {
                if let Some(&s) = self.sound.get(*pos) {
                    add += s as i32;
                }
                *pos += 1;
            }
            if add != 0 {
                for sample in frame.as_chunks_mut::<2>().0 {
                    let v = i16::from_le_bytes([sample[0], sample[1]]) as i32 + add;
                    let v = v.clamp(i16::MIN as i32, i16::MAX as i32) as i16;
                    sample.copy_from_slice(&v.to_le_bytes());
                }
            }
        }
        let len = self.sound.len();
        self.voices.retain(|&pos| pos < len);
    }

    /// Forget clicks still sounding, such as one cut off by a pause.
    pub fn silence(&mut self) {
        self.voices.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn samples(pcm: &[u8]) -> Vec<i16> {
        pcm.as_chunks::<2>()
            .0
            .iter()
            .map(|b| i16::from_le_bytes(*b))
            .collect()
    }

    #[test]
    fn a_click_is_short_and_quiet() {
        let s = click_sound(48_000);
        let secs = s.len() as f64 / 48_000.0;
        assert!(secs < 0.12, "{secs}s is not a click");
        let peak = s.iter().map(|v| (*v as i32).abs()).max().unwrap();
        assert!(
            peak <= (0.15 * i16::MAX as f64) as i32,
            "peak {peak} is too loud to be subtle"
        );
        assert!(peak > 1000, "peak {peak} would be inaudible");
    }

    #[test]
    fn a_click_has_a_press_and_a_softer_release() {
        let s = click_sound(48_000);
        let at = (RELEASE_AFTER * 48_000.0) as usize;
        let loudest =
            |r: std::ops::Range<usize>| s[r].iter().map(|v| (*v as i32).abs()).max().unwrap();
        let press = loudest(0..at);
        let release = loudest(at..s.len());
        assert!(release > press / 5, "the release should be heard");
        assert!(release < press, "the release should be softer");
        // Silent between the two, so they read as two events.
        assert!(loudest(at - 200..at) < press / 50);
    }

    #[test]
    fn a_click_ends_in_silence() {
        let s = click_sound(48_000);
        let tail = &s[s.len() - 48..];
        assert!(tail.iter().all(|v| v.abs() < 50), "it would end on a pop");
    }

    #[test]
    fn mixing_without_a_click_leaves_audio_untouched() {
        let mut m = ClickMixer::new(48_000, 2);
        let mut pcm: Vec<u8> = (0..400u16).flat_map(|v| v.to_le_bytes()).collect();
        let before = pcm.clone();
        m.mix(&mut pcm);
        assert_eq!(pcm, before);
    }

    #[test]
    fn a_triggered_click_plays_once_in_both_channels() {
        let mut m = ClickMixer::new(48_000, 2);
        m.trigger();
        let mut pcm = vec![0u8; 48_000 * 4 / 5]; // 200 ms of stereo
        m.mix(&mut pcm);
        let s = samples(&pcm);
        assert!(s.iter().any(|v| *v != 0), "the click should be heard");
        for frame in s.as_chunks::<2>().0 {
            assert_eq!(frame[0], frame[1], "the same in both ears");
        }
        assert!(!m.is_sounding(), "it should have finished");
        let mut more = vec![0u8; 4000];
        m.mix(&mut more);
        assert!(more.iter().all(|b| *b == 0), "and not play again");
    }

    #[test]
    fn a_click_split_across_chunks_is_the_same_click() {
        let mut whole = ClickMixer::new(48_000, 2);
        whole.trigger();
        let mut a = vec![0u8; 24_000];
        whole.mix(&mut a);

        let mut split = ClickMixer::new(48_000, 2);
        split.trigger();
        let mut b = vec![0u8; 24_000];
        for chunk in b.chunks_mut(1_000) {
            split.mix(chunk);
        }
        assert_eq!(a, b);
    }

    #[test]
    fn loud_audio_clips_rather_than_wrapping() {
        let mut m = ClickMixer::new(48_000, 2);
        m.trigger();
        let mut pcm: Vec<u8> = std::iter::repeat_n(i16::MAX.to_le_bytes(), 4000)
            .flatten()
            .collect();
        m.mix(&mut pcm);
        // Wrapping would turn a loud positive sample hugely negative.
        assert!(samples(&pcm).iter().all(|v| *v > 0));
    }
}
