//! Sound out to the default playback device, through WASAPI.
//!
//! The mirror of [`audio`](crate::audio): the player decodes PCM and this
//! plays it. Shared mode with automatic format conversion, so the device's
//! own rate and layout do not matter.

use windows::Win32::Media::Audio::{
    AUDCLNT_SHAREMODE_SHARED, AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM,
    AUDCLNT_STREAMFLAGS_SRC_DEFAULT_QUALITY, IAudioClient, IAudioRenderClient, IMMDeviceEnumerator,
    MMDeviceEnumerator, WAVEFORMATEX, eConsole, eRender,
};
use windows::Win32::System::Com::{
    CLSCTX_ALL, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx,
};

use crate::decoder::{PLAYBACK_BLOCK, PLAYBACK_CHANNELS, PLAYBACK_RATE};

/// How much sound the device holds ahead. Pausing and seeking stop it
/// instantly by resetting the buffer, so this costs nothing in
/// responsiveness, and it is long enough that a busy UI thread cannot starve
/// it.
const BUFFER_HNS: i64 = 2_000_000; // 200ms

/// An open playback stream. Not `Send`: it stays on the thread that opened it.
pub struct Speaker {
    client: IAudioClient,
    render: IAudioRenderClient,
    capacity: u32,
    running: bool,
}

impl Speaker {
    pub fn open() -> Result<Self, String> {
        // SAFETY: the documented WASAPI shared-mode sequence. COM is
        // initialised on this thread first; a thread already in another
        // apartment reports so harmlessly.
        unsafe {
            let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
            let enumerator: IMMDeviceEnumerator =
                CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)
                    .map_err(|e| format!("no audio devices: {e}"))?;
            let device = enumerator
                .GetDefaultAudioEndpoint(eRender, eConsole)
                .map_err(|e| format!("no speakers: {e}"))?;
            let client: IAudioClient = device
                .Activate(CLSCTX_ALL, None)
                .map_err(|e| format!("could not open the speakers: {e}"))?;
            let format = WAVEFORMATEX {
                wFormatTag: 1, // WAVE_FORMAT_PCM
                nChannels: PLAYBACK_CHANNELS,
                nSamplesPerSec: PLAYBACK_RATE,
                wBitsPerSample: 16,
                nBlockAlign: PLAYBACK_BLOCK as u16,
                nAvgBytesPerSec: PLAYBACK_RATE * PLAYBACK_BLOCK as u32,
                cbSize: 0,
            };
            client
                .Initialize(
                    AUDCLNT_SHAREMODE_SHARED,
                    AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM | AUDCLNT_STREAMFLAGS_SRC_DEFAULT_QUALITY,
                    BUFFER_HNS,
                    0,
                    &format,
                    None,
                )
                .map_err(|e| format!("could not configure the speakers: {e}"))?;
            let capacity = client
                .GetBufferSize()
                .map_err(|e| format!("speakers: {e}"))?;
            let render: IAudioRenderClient =
                client.GetService().map_err(|e| format!("speakers: {e}"))?;
            Ok(Self {
                client,
                render,
                capacity,
                running: false,
            })
        }
    }

    /// Bytes the device can take right now.
    pub fn room(&self) -> usize {
        // SAFETY: a read-only query on an initialised client.
        let queued = unsafe { self.client.GetCurrentPadding() }.unwrap_or(self.capacity);
        self.capacity.saturating_sub(queued) as usize * PLAYBACK_BLOCK
    }

    /// Queue as much of `pcm` as fits. Returns the bytes taken, always whole
    /// sample frames.
    pub fn write(&mut self, pcm: &[u8]) -> usize {
        let frames = (self.room().min(pcm.len()) / PLAYBACK_BLOCK) as u32;
        if frames == 0 {
            return 0;
        }
        let bytes = frames as usize * PLAYBACK_BLOCK;
        // SAFETY: GetBuffer hands out exactly `frames` frames of writable
        // memory, which is filled completely before ReleaseBuffer.
        unsafe {
            let Ok(dst) = self.render.GetBuffer(frames) else {
                return 0;
            };
            std::ptr::copy_nonoverlapping(pcm.as_ptr(), dst, bytes);
            if self.render.ReleaseBuffer(frames, 0).is_err() {
                return 0;
            }
        }
        bytes
    }

    pub fn start(&mut self) {
        if !self.running {
            // SAFETY: starting an initialised client.
            self.running = unsafe { self.client.Start() }.is_ok();
        }
    }

    /// Stop and throw away whatever was queued, so a pause or seek is heard
    /// immediately rather than after the buffer drains.
    pub fn stop(&mut self) {
        // SAFETY: Stop then Reset is the documented way to flush a stream;
        // Reset is only valid once stopped.
        unsafe {
            let _ = self.client.Stop();
            let _ = self.client.Reset();
        }
        self.running = false;
    }
}

impl Drop for Speaker {
    fn drop(&mut self) {
        self.stop();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn opens_queues_and_flushes_when_a_device_exists() {
        // Machines without speakers (CI, servers) legitimately have none.
        let Ok(mut s) = Speaker::open() else { return };
        let room = s.room();
        assert!(room > 0, "an idle stream has room");
        assert_eq!(room % PLAYBACK_BLOCK, 0);
        let silence = vec![0u8; room + 4096];
        let taken = s.write(&silence);
        assert_eq!(taken, room, "never more than there is room for");
        assert_eq!(s.write(&silence), 0, "a full buffer takes nothing");
        s.stop();
        assert_eq!(s.room(), room, "stopping flushes the queue");
    }
}
