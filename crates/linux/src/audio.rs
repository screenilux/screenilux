//! Microphone and system sound capture through GStreamer.
//!
//! Both come from the PulseAudio interface, which PipeWire also serves: the
//! microphone is the default source, and the system's own sound is the
//! monitor of the default output. Capture runs on its own thread and hands
//! PCM over a channel; the recorder decides when it belongs in the output.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

use crossbeam_channel::{Receiver, Sender, bounded};
use gstreamer as gst;
use gstreamer::prelude::*;
use gstreamer_app as gst_app;

use crate::gst as g;

/// What the encoder is configured to expect.
pub const SAMPLE_RATE: u32 = 48_000;
pub const CHANNELS: u16 = 2;
pub const BITS_PER_SAMPLE: u16 = 16;

/// The PCM format above, as GStreamer caps.
pub(crate) const PCM_CAPS: &str =
    "audio/x-raw,format=S16LE,rate=48000,channels=2,layout=interleaved";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Source {
    Microphone,
    System,
}

/// A running sound capture.
pub struct AudioCapture {
    stop: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
    pcm: Receiver<Vec<u8>>,
    error: Arc<Mutex<Option<String>>>,
}

impl AudioCapture {
    /// Start capturing the default microphone. Failures are reported through
    /// [`error`](Self::error): a missing microphone never stops a recording.
    pub fn start() -> Self {
        Self::start_from(Source::Microphone)
    }

    /// Start capturing what the default output plays. Silence while nothing
    /// plays; the recorder paces the track either way.
    pub fn start_system() -> Self {
        Self::start_from(Source::System)
    }

    fn start_from(source: Source) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let error = Arc::new(Mutex::new(None));
        let (tx, pcm) = bounded(64);
        let (flag, err) = (Arc::clone(&stop), Arc::clone(&error));
        let handle = std::thread::Builder::new()
            .name(format!("screenilux-audio-{source:?}").to_lowercase())
            .spawn(move || {
                if let Err(e) = capture(source, &flag, &tx) {
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

    /// Throw away whatever has queued, such as sound heard while paused.
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

fn description(source: Source) -> Result<String, String> {
    let src = match (source, g::has("pulsesrc")) {
        (Source::Microphone, true) => "pulsesrc".to_string(),
        (Source::Microphone, false) => "autoaudiosrc".to_string(),
        (Source::System, true) => "pulsesrc device=@DEFAULT_MONITOR@".to_string(),
        (Source::System, false) => {
            return Err("recording the system sound needs GStreamer's PulseAudio plugin".into());
        }
    };
    Ok(format!(
        "{src} ! audioconvert ! audioresample ! {PCM_CAPS} ! \
         appsink name=sink sync=false max-buffers=50 drop=true"
    ))
}

fn capture(source: Source, stop: &AtomicBool, tx: &Sender<Vec<u8>>) -> Result<(), String> {
    let pipeline = g::pipeline(&description(source)?)?;
    let sink: gst_app::AppSink = g::element(&pipeline, "sink")?;
    let result = (|| {
        // A live source starts in its own time; waiting for it here would
        // also keep a stop request waiting. Failures arrive on the bus.
        pipeline
            .set_state(gst::State::Playing)
            .map_err(|_| g::error_so_far(&pipeline).unwrap_or_else(|| "could not start".into()))?;
        while !stop.load(Ordering::Relaxed) {
            if let Some(e) = g::error_so_far(&pipeline) {
                return Err(e);
            }
            let Some(sample) = sink.try_pull_sample(gst::ClockTime::from_mseconds(50)) else {
                continue;
            };
            if let Some(buffer) = sample.buffer()
                && let Ok(map) = buffer.map_readable()
            {
                // Drop rather than block: stalling here backs up the device.
                let _ = tx.try_send(map.as_slice().to_vec());
            }
        }
        Ok(())
    })();
    let _ = pipeline.set_state(gst::State::Null);
    result
}

/// Devices of a class, such as `Audio/Source`, as GStreamer sees them.
pub(crate) fn devices(class: &str) -> Vec<gst::Device> {
    g::init();
    let monitor = gst::DeviceMonitor::new();
    monitor.add_filter(Some(class), None);
    if monitor.start().is_err() {
        return Vec::new();
    }
    let found = monitor.devices().into_iter().collect();
    monitor.stop();
    found
}

/// Whether a device is the monitor of an output, which also lists as a
/// source but is not a microphone.
fn is_monitor(device: &gst::Device) -> bool {
    device
        .properties()
        .and_then(|p| p.get::<String>("device.class").ok())
        .is_some_and(|c| c == "monitor")
}

/// Is there a microphone at all?
pub fn has_microphone() -> bool {
    devices("Audio/Source").iter().any(|d| !is_monitor(d))
}

/// Is there an output whose sound could be recorded?
pub fn has_system_sound() -> bool {
    g::has("pulsesrc") && !devices("Audio/Sink").is_empty()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    #[test]
    fn probing_is_safe_without_sound_hardware() {
        let _ = has_microphone();
        let _ = has_system_sound();
    }

    #[test]
    fn capture_starts_and_stops_promptly() {
        let mut cap = AudioCapture::start();
        std::thread::sleep(Duration::from_millis(200));
        let start = Instant::now();
        cap.stop();
        assert!(start.elapsed() < Duration::from_secs(3));
    }

    #[test]
    fn the_system_sound_is_the_default_outputs_monitor() {
        if g::has("pulsesrc") {
            let d = description(Source::System).unwrap();
            assert!(d.starts_with("pulsesrc device=@DEFAULT_MONITOR@"));
            assert!(d.contains("rate=48000"));
        }
    }
}
