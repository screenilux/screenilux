//! Microphone and system sound capture via WASAPI.
//!
//! System sound is what the computer plays, read back from the default
//! speakers in loopback mode: the same capture path as a microphone, on the
//! output device instead of the input one.
//!
//! The last thing that needed ffmpeg. The encoder takes raw PCM through
//! `send_audio_buffer`, but `windows-capture` contains no audio *capture* at
//! all, so this reads the device directly.
//!
//! Capture runs on its own thread and hands PCM over a channel. It deliberately
//! does not touch the encoder itself: the recorder decides when audio belongs
//! in the output, which matters across a pause.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;

use crossbeam_channel::{Receiver, Sender, bounded};
use windows::Win32::Media::Audio::{
    AUDCLNT_SHAREMODE_SHARED, AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM, AUDCLNT_STREAMFLAGS_LOOPBACK,
    AUDCLNT_STREAMFLAGS_SRC_DEFAULT_QUALITY, EDataFlow, IAudioCaptureClient, IAudioClient,
    IMMDevice, IMMDeviceEnumerator, MMDeviceEnumerator, WAVEFORMATEX, eCapture, eConsole, eRender,
};
use windows::Win32::System::Com::{
    CLSCTX_ALL, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx, CoUninitialize,
};
use windows::core::Interface;

/// What the encoder is configured to expect.
pub const SAMPLE_RATE: u32 = 48_000;
pub const CHANNELS: u16 = 2;
pub const BITS_PER_SAMPLE: u16 = 16;

/// Where sound is captured from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    /// The default microphone.
    Microphone,
    /// What the default speakers play.
    System,
}

impl Source {
    fn name(self) -> &'static str {
        match self {
            Source::Microphone => "the microphone",
            Source::System => "the system sound",
        }
    }

    fn flow(self) -> EDataFlow {
        match self {
            Source::Microphone => eCapture,
            Source::System => eRender,
        }
    }
}

/// A running sound capture.
pub struct AudioCapture {
    stop: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
    pcm: Receiver<Vec<u8>>,
    error: Arc<std::sync::Mutex<Option<String>>>,
}

impl AudioCapture {
    /// Start capturing the default microphone.
    ///
    /// Failures are reported through [`error`](Self::error) rather than here:
    /// a missing microphone should never stop a screen recording from running.
    pub fn start() -> Self {
        Self::start_from(Source::Microphone)
    }

    /// Start capturing what the default speakers play. Nothing arrives while
    /// nothing is playing; the recorder fills that with silence.
    pub fn start_system() -> Self {
        Self::start_from(Source::System)
    }

    fn start_from(source: Source) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let error = Arc::new(std::sync::Mutex::new(None));
        // Bounded: if the encoder stalls we would rather drop audio than grow
        // a queue without limit.
        let (tx, pcm) = bounded(64);

        let flag = Arc::clone(&stop);
        let err = Arc::clone(&error);
        let handle = std::thread::Builder::new()
            .name(format!("screenilux-audio-{source:?}").to_lowercase())
            .spawn(move || {
                if let Err(e) = capture_loop(source, &flag, &tx) {
                    tracing::warn!(error = %e, ?source, "sound capture stopped");
                    if let Ok(mut slot) = err.lock() {
                        *slot = Some(e);
                    }
                }
            })
            .ok();

        Self {
            stop,
            handle,
            pcm,
            error,
        }
    }

    /// PCM that has arrived since the last call. Never blocks.
    pub fn drain(&self) -> Vec<Vec<u8>> {
        self.pcm.try_iter().collect()
    }

    /// Throw away whatever has queued, such as audio captured while paused.
    ///
    /// The encoder places audio by counting samples rather than by timestamp,
    /// so stale audio cannot simply be skipped later: once written, it plays
    /// and shifts every later sample out of sync with the picture.
    pub fn discard(&self) {
        while self.pcm.try_recv().is_ok() {}
    }

    pub fn error(&self) -> Option<String> {
        self.error.lock().ok().and_then(|e| e.clone())
    }

    pub fn stop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

impl Drop for AudioCapture {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Is there a microphone at all?
pub fn has_microphone() -> bool {
    // SAFETY: COM is initialised and uninitialised around the probe.
    unsafe {
        if CoInitializeEx(None, COINIT_MULTITHREADED).is_err() {
            return false;
        }
        let found = default_device(Source::Microphone).is_ok();
        CoUninitialize();
        found
    }
}

/// Is there an output device whose sound could be recorded?
pub fn has_system_sound() -> bool {
    // SAFETY: as in `has_microphone`.
    unsafe {
        if CoInitializeEx(None, COINIT_MULTITHREADED).is_err() {
            return false;
        }
        let found = default_device(Source::System).is_ok();
        CoUninitialize();
        found
    }
}

/// The default device for `source`.
///
/// # Safety
/// COM must be initialised on the calling thread.
unsafe fn default_device(source: Source) -> windows::core::Result<IMMDevice> {
    unsafe {
        let enumerator: IMMDeviceEnumerator =
            CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)?;
        enumerator.GetDefaultAudioEndpoint(source.flow(), eConsole)
    }
}

fn capture_loop(source: Source, stop: &AtomicBool, tx: &Sender<Vec<u8>>) -> Result<(), String> {
    // SAFETY: every call below is a documented WASAPI sequence; COM is
    // initialised for this thread and released on every exit path.
    unsafe {
        CoInitializeEx(None, COINIT_MULTITHREADED)
            .ok()
            .map_err(|e| format!("COM init failed: {e}"))?;
        let result = capture_inner(source, stop, tx);
        CoUninitialize();
        result
    }
}

unsafe fn capture_inner(
    source: Source,
    stop: &AtomicBool,
    tx: &Sender<Vec<u8>>,
) -> Result<(), String> {
    let name = source.name();
    unsafe {
        let device = default_device(source).map_err(|e| format!("no device for {name}: {e}"))?;
        let client: IAudioClient = device
            .Activate(CLSCTX_ALL, None)
            .map_err(|e| format!("could not open {name}: {e}"))?;

        // Ask WASAPI to convert to the format the encoder wants, rather than
        // resampling by hand: devices vary wildly in native rate and layout.
        let format = WAVEFORMATEX {
            wFormatTag: 1, // WAVE_FORMAT_PCM
            nChannels: CHANNELS,
            nSamplesPerSec: SAMPLE_RATE,
            wBitsPerSample: BITS_PER_SAMPLE,
            nBlockAlign: CHANNELS * BITS_PER_SAMPLE / 8,
            nAvgBytesPerSec: SAMPLE_RATE * (CHANNELS * BITS_PER_SAMPLE / 8) as u32,
            cbSize: 0,
        };
        // 200ms of buffer, in 100-nanosecond units.
        let duration = 2_000_000i64;
        let mut flags =
            AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM | AUDCLNT_STREAMFLAGS_SRC_DEFAULT_QUALITY;
        if source == Source::System {
            flags |= AUDCLNT_STREAMFLAGS_LOOPBACK;
        }
        client
            .Initialize(AUDCLNT_SHAREMODE_SHARED, flags, duration, 0, &format, None)
            .map_err(|e| format!("could not configure {name}: {e}"))?;

        let capture: IAudioCaptureClient = client
            .GetService()
            .map_err(|e| format!("could not start capture: {e}"))?;
        client
            .Start()
            .map_err(|e| format!("could not start {name}: {e}"))?;

        let frame_bytes = (CHANNELS * BITS_PER_SAMPLE / 8) as usize;
        while !stop.load(Ordering::Relaxed) {
            let Ok(available) = capture.GetNextPacketSize() else {
                break;
            };
            if available == 0 {
                std::thread::sleep(std::time::Duration::from_millis(5));
                continue;
            }

            let mut data: *mut u8 = std::ptr::null_mut();
            let mut frames = 0u32;
            let mut flags = 0u32;
            if capture
                .GetBuffer(&mut data, &mut frames, &mut flags, None, None)
                .is_err()
            {
                break;
            }
            if frames > 0 && !data.is_null() {
                let len = frames as usize * frame_bytes;
                // AUDCLNT_BUFFERFLAGS_SILENT: the buffer is not meaningful and
                // should be treated as silence.
                const SILENT: u32 = 0x2;
                let chunk = if flags & SILENT != 0 {
                    vec![0u8; len]
                } else {
                    std::slice::from_raw_parts(data, len).to_vec()
                };
                // Drop rather than block: stalling here would back up the
                // device and cause glitching.
                let _ = tx.try_send(chunk);
            }
            let _ = capture.ReleaseBuffer(frames);
        }

        let _ = client.Stop();
        Ok(())
    }
}

/// Needed so `IAudioClient::GetService` can resolve the interface id.
#[allow(dead_code)]
fn _assert_interfaces() {
    fn takes<T: Interface>() {}
    takes::<IAudioCaptureClient>();
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    #[test]
    fn reports_whether_a_microphone_exists() {
        // No assertion on the answer - hardware varies - only that probing is
        // safe and terminates.
        let _ = has_microphone();
    }

    #[test]
    fn capture_starts_and_stops_promptly() {
        let mut cap = AudioCapture::start();
        std::thread::sleep(Duration::from_millis(300));
        let start = Instant::now();
        cap.stop();
        assert!(
            start.elapsed() < Duration::from_secs(3),
            "stopping took {:?}",
            start.elapsed()
        );
    }

    #[test]
    fn delivers_pcm_when_a_microphone_is_present() {
        if !has_microphone() {
            return;
        }
        let cap = AudioCapture::start();
        let deadline = Instant::now() + Duration::from_secs(4);
        let mut bytes = 0usize;
        while Instant::now() < deadline && bytes == 0 {
            bytes += cap.drain().iter().map(|c| c.len()).sum::<usize>();
            std::thread::sleep(Duration::from_millis(50));
        }
        if let Some(e) = cap.error() {
            // A microphone in use elsewhere, or blocked by privacy settings,
            // is a legitimate outcome rather than a test failure.
            eprintln!("microphone unavailable: {e}");
            return;
        }
        assert!(bytes > 0, "expected some PCM within four seconds");
        assert_eq!(
            bytes % (CHANNELS as usize * BITS_PER_SAMPLE as usize / 8),
            0,
            "PCM should arrive in whole frames"
        );
    }

    #[test]
    fn system_sound_starts_and_stops_without_error() {
        if !has_system_sound() {
            return;
        }
        let mut cap = AudioCapture::start_system();
        // Nothing may be playing, so no PCM is expected, only that the
        // device accepted loopback capture in the encoder's format.
        std::thread::sleep(Duration::from_millis(400));
        assert_eq!(cap.error(), None, "loopback capture should start");
        let bytes: usize = cap.drain().iter().map(|c| c.len()).sum();
        assert_eq!(
            bytes % (CHANNELS as usize * BITS_PER_SAMPLE as usize / 8),
            0
        );
        cap.stop();
    }

    #[test]
    fn dropping_stops_the_thread() {
        let before = std::thread::available_parallelism().ok();
        {
            let _cap = AudioCapture::start();
            std::thread::sleep(Duration::from_millis(150));
        }
        // No handle leak is observable directly; this asserts drop does not
        // hang, which is the failure that would matter.
        let _ = before;
    }
}
