//! Mixing sound sources that keep their own time into one track.
//!
//! The microphone and the computer's own sound come from different devices,
//! each with its own clock, and the computer's sound only arrives while
//! something is playing. So neither can set the pace of the track. Each
//! source queues what it delivers, and the recording pulls whole stretches
//! from all of them at once, padding a source that is short with silence.

use std::collections::VecDeque;

/// 16-bit little-endian PCM from several sources, mixed on demand.
pub struct Mixer {
    queues: Vec<VecDeque<u8>>,
    /// Bytes per sample frame: every channel, two bytes each.
    frame: usize,
    /// The most a source may queue. A source whose clock runs fast fills its
    /// queue; past this the oldest sound goes, so it never drifts further
    /// behind the picture than this.
    limit: usize,
}

impl Mixer {
    /// `sources` inputs of `channels`-channel PCM, each holding at most
    /// `max_frames` sample frames.
    pub fn new(sources: usize, channels: u16, max_frames: usize) -> Self {
        let frame = 2 * channels.max(1) as usize;
        Self {
            queues: (0..sources).map(|_| VecDeque::new()).collect(),
            frame,
            limit: max_frames * frame,
        }
    }

    /// Queue PCM from `source`. A trailing partial frame is dropped.
    pub fn push(&mut self, source: usize, pcm: &[u8]) {
        let Some(q) = self.queues.get_mut(source) else {
            return;
        };
        let whole = pcm.len() - pcm.len() % self.frame;
        q.extend(&pcm[..whole]);
        if q.len() > self.limit {
            let excess = q.len() - self.limit;
            q.drain(..excess);
        }
    }

    /// The next `frames` sample frames: every source added together and
    /// clipped, silence where a source has nothing queued.
    pub fn pull(&mut self, frames: usize) -> Vec<u8> {
        let len = frames * self.frame;
        let mut acc = vec![0i32; len / 2];
        for q in &mut self.queues {
            let take = q.len().min(len);
            for (i, pair) in q
                .drain(..take)
                .collect::<Vec<u8>>()
                .as_chunks::<2>()
                .0
                .iter()
                .enumerate()
            {
                acc[i] += i16::from_le_bytes(*pair) as i32;
            }
        }
        let mut out = Vec::with_capacity(len);
        for s in acc {
            let s = s.clamp(i16::MIN as i32, i16::MAX as i32) as i16;
            out.extend_from_slice(&s.to_le_bytes());
        }
        out
    }

    /// Forget everything queued, such as sound heard during a pause.
    pub fn clear(&mut self) {
        for q in &mut self.queues {
            q.clear();
        }
    }

    /// Sample frames queued for `source`.
    pub fn queued(&self, source: usize) -> usize {
        self.queues.get(source).map_or(0, |q| q.len() / self.frame)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pcm(samples: &[i16]) -> Vec<u8> {
        samples.iter().flat_map(|s| s.to_le_bytes()).collect()
    }

    fn samples(pcm: &[u8]) -> Vec<i16> {
        pcm.as_chunks::<2>()
            .0
            .iter()
            .map(|b| i16::from_le_bytes(*b))
            .collect()
    }

    #[test]
    fn two_sources_are_added_together() {
        let mut m = Mixer::new(2, 2, 100);
        m.push(0, &pcm(&[100, 200, 300, 400]));
        m.push(1, &pcm(&[10, 20, 30, 40]));
        assert_eq!(samples(&m.pull(2)), vec![110, 220, 330, 440]);
    }

    #[test]
    fn a_short_source_is_padded_with_silence() {
        let mut m = Mixer::new(2, 1, 100);
        m.push(0, &pcm(&[5, 5, 5]));
        assert_eq!(samples(&m.pull(5)), vec![5, 5, 5, 0, 0]);
        assert_eq!(samples(&m.pull(2)), vec![0, 0], "nothing left over");
    }

    #[test]
    fn what_is_not_pulled_waits_for_next_time() {
        let mut m = Mixer::new(1, 1, 100);
        m.push(0, &pcm(&[1, 2, 3, 4]));
        assert_eq!(samples(&m.pull(2)), vec![1, 2]);
        assert_eq!(m.queued(0), 2);
        assert_eq!(samples(&m.pull(2)), vec![3, 4]);
    }

    #[test]
    fn loud_sources_clip_rather_than_wrap() {
        let mut m = Mixer::new(2, 1, 100);
        m.push(0, &pcm(&[30_000, -30_000]));
        m.push(1, &pcm(&[30_000, -30_000]));
        assert_eq!(samples(&m.pull(2)), vec![i16::MAX, i16::MIN]);
    }

    #[test]
    fn a_fast_source_keeps_only_the_newest_sound() {
        let mut m = Mixer::new(1, 1, 3);
        m.push(0, &pcm(&[1, 2, 3, 4, 5]));
        assert_eq!(m.queued(0), 3);
        assert_eq!(samples(&m.pull(3)), vec![3, 4, 5]);
    }

    #[test]
    fn partial_frames_and_unknown_sources_are_ignored() {
        let mut m = Mixer::new(1, 2, 100);
        // One whole stereo frame and half of another.
        m.push(0, &pcm(&[7, 8, 9]));
        assert_eq!(m.queued(0), 1);
        m.push(5, &pcm(&[1, 1]));
        m.clear();
        assert_eq!(m.queued(0), 0);
        assert_eq!(samples(&m.pull(1)), vec![0, 0]);
    }
}
