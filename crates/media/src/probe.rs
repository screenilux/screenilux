//! Finding an encoder that actually works, and listing capture devices.

use crate::command::{CPU_X264, EncoderSpec, PROBE_ORDER, list_devices_args, probe_args};
use crate::proc;
use std::path::Path;
use std::time::Duration;

/// Generous: a cold GPU driver can take a while on the first probe.
const PROBE_TIMEOUT: Duration = Duration::from_secs(30);
const ENUMERATE_TIMEOUT: Duration = Duration::from_secs(20);

/// The first hardware encoder that opens, else the software fallback.
///
/// Probing rather than inferring from the GPU name matters: on the development
/// machine NVENC is present in hardware but cannot open, because the installed
/// driver predates the NVENC API version the ffmpeg build requires. Only a real
/// encode reveals that.
pub fn detect(ffmpeg: &Path) -> EncoderSpec {
    for spec in PROBE_ORDER {
        match proc::run(ffmpeg, &probe_args(spec), PROBE_TIMEOUT) {
            Ok(out) if out.success() => {
                tracing::info!(encoder = spec.label, "hardware encoder available");
                return *spec;
            }
            Ok(out) => tracing::debug!(
                encoder = spec.label,
                reason = out.reason(),
                "encoder unavailable"
            ),
            Err(e) => tracing::debug!(encoder = spec.label, error = %e, "probe failed to run"),
        }
    }
    tracing::info!(
        encoder = CPU_X264.label,
        "falling back to software encoding"
    );
    CPU_X264
}

/// Capture devices, split by kind.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Devices {
    pub audio: Vec<String>,
    pub video: Vec<String>,
}

/// Enumerate DirectShow devices.
///
/// This invocation always exits non-zero by design — asking for the device list
/// is phrased as a failed open — so the exit status is ignored and the names
/// are parsed from stderr.
pub fn list_devices(ffmpeg: &Path) -> Devices {
    match proc::run(ffmpeg, &list_devices_args(), ENUMERATE_TIMEOUT) {
        Ok(out) => parse_devices(&out.stderr),
        Err(e) => {
            tracing::warn!(error = %e, "could not enumerate capture devices");
            Devices::default()
        }
    }
}

fn parse_devices(stderr: &str) -> Devices {
    let mut d = Devices::default();
    for line in stderr.lines() {
        let kind = if line.contains("(audio)") {
            &mut d.audio
        } else if line.contains("(video)") {
            &mut d.video
        } else {
            continue;
        };
        if let Some(name) = quoted(line)
            && !kind.iter().any(|existing| existing == name)
        {
            kind.push(name.to_string());
        }
    }
    d
}

/// The first double-quoted run on a line.
fn quoted(line: &str) -> Option<&str> {
    let start = line.find('"')? + 1;
    let rest = &line[start..];
    let end = rest.find('"')?;
    Some(&rest[..end])
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Verbatim ffmpeg output from the development machine.
    const SAMPLE: &str = r#"
[dshow @ 000002d715731380] "Integrated Camera" (video)
[dshow @ 000002d715731380]   Alternative name "@device_pnp_\\?\usb#vid_04f2&pid_b7b9"
[dshow @ 000002d715731380] "Pixel 7a (Windows Virtual Camera)" (video)
[dshow @ 000002d715731380]   Alternative name "@device_pnp_\\?\swd#vcamdevapi#106f"
[dshow @ 000002d715731380] "Microphone Array (Realtek(R) Audio)" (audio)
[dshow @ 000002d715731380]   Alternative name "@device_cm_{33D9A762}"
"#;

    #[test]
    fn parses_real_device_output() {
        let d = parse_devices(SAMPLE);
        assert_eq!(
            d.video,
            vec!["Integrated Camera", "Pixel 7a (Windows Virtual Camera)"]
        );
        assert_eq!(d.audio, vec!["Microphone Array (Realtek(R) Audio)"]);
    }

    #[test]
    fn alternative_name_lines_are_not_mistaken_for_devices() {
        let d = parse_devices(SAMPLE);
        assert!(
            !d.video.iter().any(|n| n.starts_with("@device")),
            "the indented Alternative name lines must be skipped"
        );
    }

    #[test]
    fn duplicate_names_are_collapsed() {
        let dup = "[x] \"Cam\" (video)\n[x] \"Cam\" (video)\n";
        assert_eq!(parse_devices(dup).video, vec!["Cam"]);
    }

    #[test]
    fn empty_output_yields_no_devices() {
        assert_eq!(parse_devices(""), Devices::default());
    }

    #[test]
    fn names_containing_parens_survive() {
        let d = parse_devices(SAMPLE);
        assert!(d.audio[0].contains("(Realtek(R) Audio)"));
    }

    #[test]
    fn detect_picks_something_usable_on_this_machine() {
        let Some(ff) = proc::locate_ffmpeg() else {
            return;
        };
        let enc = detect(&ff);
        assert!(
            [QUICK_SYNC_LABEL, NVENC_LABEL, CPU_X264.label].contains(&enc.label),
            "unexpected encoder {}",
            enc.label
        );
        // whatever it picked must at least be able to open
        let out = proc::run(&ff, &probe_args(&enc), PROBE_TIMEOUT).unwrap();
        assert!(out.success(), "chosen encoder failed: {}", out.reason());
    }

    const QUICK_SYNC_LABEL: &str = "Quick Sync";
    const NVENC_LABEL: &str = "NVENC";

    #[test]
    fn enumerates_devices_on_this_machine() {
        let Some(ff) = proc::locate_ffmpeg() else {
            return;
        };
        let d = list_devices(&ff);
        // no assertion on contents - hardware varies - but parsing must not
        // produce the Alternative-name noise
        assert!(d.audio.iter().all(|n| !n.starts_with('@')));
        assert!(d.video.iter().all(|n| !n.starts_with('@')));
    }
}
