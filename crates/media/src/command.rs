//! ffmpeg argument construction.
//!
//! Kept pure and separate from process handling so the exact argv for every
//! case can be asserted in tests without running anything. Several of these
//! flags are load-bearing in non-obvious ways; see the notes on each.

use std::ffi::OsString;
use std::path::Path;

/// A video encoder we can ask ffmpeg for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EncoderSpec {
    /// Shown in the UI and recorded in the manifest.
    pub label: &'static str,
    pub args: &'static [&'static str],
    /// Appended to the video filter chain. Quick Sync needs an explicit
    /// conversion because our pipe feeds BGRA and it wants NV12.
    pub filter_suffix: &'static str,
}

/// Measured on real desktop content: smaller output *and* slightly higher
/// quality than x264 veryfast at crf 23, for less CPU. Leads the order.
pub const QUICK_SYNC: EncoderSpec = EncoderSpec {
    label: "Quick Sync",
    args: &["-c:v", "h264_qsv", "-global_quality", "23"],
    filter_suffix: ",format=nv12",
};

pub const NVENC: EncoderSpec = EncoderSpec {
    label: "NVENC",
    args: &["-c:v", "h264_nvenc", "-cq", "23", "-pix_fmt", "yuv420p"],
    filter_suffix: "",
};

/// The universal fallback. Never probed: if this cannot run, nothing can.
pub const CPU_X264: EncoderSpec = EncoderSpec {
    label: "CPU (x264)",
    args: &[
        "-c:v", "libx264", "-preset", "veryfast", "-crf", "23", "-pix_fmt", "yuv420p",
    ],
    filter_suffix: "",
};

/// Probed in order; the first that opens wins.
pub const PROBE_ORDER: &[EncoderSpec] = &[QUICK_SYNC, NVENC];

/// Even dimensions are required by yuv420p, and an odd-sized monitor is
/// possible, so always pad up rather than assuming.
const PAD_FILTER: &str = "pad=ceil(iw/2)*2:ceil(ih/2)*2";

/// What a segment encoder needs to know.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SegmentParams {
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    /// DirectShow device name. Only one screen in a session carries audio:
    /// Windows gives the capture device a single consumer.
    pub audio_device: Option<String>,
}

impl SegmentParams {
    /// Bytes in one BGRA frame. Writes are checked against this, because a
    /// short write silently desynchronises the whole stream.
    pub fn frame_bytes(&self) -> usize {
        self.width as usize * self.height as usize * 4
    }
}

fn push(args: &mut Vec<OsString>, items: &[&str]) {
    args.extend(items.iter().map(OsString::from));
}

/// Full argv for encoding one segment from raw BGRA on stdin.
pub fn segment_args(p: &SegmentParams, enc: &EncoderSpec, out: &Path) -> Vec<OsString> {
    let mut a: Vec<OsString> = Vec::new();
    push(&mut a, &["-hide_banner", "-loglevel", "error", "-y"]);
    push(&mut a, &["-f", "rawvideo", "-pix_fmt", "bgra"]);
    a.push("-s".into());
    a.push(format!("{}x{}", p.width, p.height).into());
    a.push("-r".into());
    a.push(p.fps.to_string().into());
    // a generous queue so a brief encoder stall does not block our writer
    push(&mut a, &["-thread_queue_size", "512", "-i", "pipe:0"]);

    if let Some(dev) = &p.audio_device {
        push(&mut a, &["-f", "dshow", "-thread_queue_size", "1024", "-i"]);
        a.push(format!("audio={dev}").into());
    }

    a.push("-vf".into());
    a.push(format!("{PAD_FILTER}{}", enc.filter_suffix).into());
    push(&mut a, enc.args);

    if p.audio_device.is_some() {
        // first_pts=0 pads the mic's late start with silence so the audio
        // stream is as long as the video and -shortest clips nothing.
        // -shortest is also what makes ffmpeg *exit*: a dshow input never ends,
        // so without it the process outlives the closed video pipe.
        push(
            &mut a,
            &[
                "-af",
                "aresample=async=1:first_pts=0",
                "-c:a",
                "aac",
                "-b:a",
                "160k",
                "-shortest",
            ],
        );
    }

    a.push(out.into());
    a
}

/// Argv for a short synthetic encode, used to find out whether a hardware
/// encoder actually opens on this machine. Probing beats assuming: an encoder
/// can be absent for reasons the GPU name does not reveal, such as a driver
/// older than the ffmpeg build expects.
pub fn probe_args(enc: &EncoderSpec) -> Vec<OsString> {
    let mut a: Vec<OsString> = Vec::new();
    push(&mut a, &["-hide_banner", "-loglevel", "error", "-y"]);
    push(
        &mut a,
        &["-f", "lavfi", "-i", "color=c=black:s=640x480:r=30"],
    );
    push(&mut a, &["-frames:v", "5"]);
    if !enc.filter_suffix.is_empty() {
        a.push("-vf".into());
        a.push(enc.filter_suffix.trim_start_matches(',').into());
    }
    push(&mut a, enc.args);
    push(&mut a, &["-f", "null", "-"]);
    a
}

/// Argv listing the DirectShow capture devices. Always exits non-zero by
/// design; the device list is on stderr.
pub fn list_devices_args() -> Vec<OsString> {
    [
        "-hide_banner",
        "-list_devices",
        "true",
        "-f",
        "dshow",
        "-i",
        "dummy",
    ]
    .iter()
    .map(OsString::from)
    .collect()
}

/// How to join the per-pause segments.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JoinMode {
    /// Stream copy. Near-instant, and correct only when every segment came out
    /// of the same encoder.
    Copy,
    /// Re-encode. Needed when a mid-recording encoder fallback left segments
    /// with different SPS/PPS, which a stream copy can turn into a file that
    /// only plays the first segment's parameters.
    Reencode(EncoderSpec),
}

pub fn concat_args(list_file: &Path, dest: &Path, mode: JoinMode) -> Vec<OsString> {
    let mut a: Vec<OsString> = Vec::new();
    push(&mut a, &["-hide_banner", "-loglevel", "error", "-y"]);
    // -safe 0 because the list holds absolute temp paths
    push(&mut a, &["-f", "concat", "-safe", "0", "-i"]);
    a.push(list_file.into());
    match mode {
        JoinMode::Copy => push(&mut a, &["-c", "copy"]),
        JoinMode::Reencode(enc) => {
            push(&mut a, enc.args);
            push(&mut a, &["-c:a", "aac", "-b:a", "160k"]);
        }
    }
    a.push(dest.into());
    a
}

/// One line of a concat list file.
///
/// The demuxer takes a single-quoted path, so a literal quote has to be closed,
/// escaped and reopened. Temp paths rarely contain one, but a user profile name
/// can, and the failure mode is a baffling parse error at save time.
pub fn concat_line(path: &Path) -> String {
    let p = path.to_string_lossy().replace('\\', "/");
    format!("file '{}'\n", p.replace('\'', r"'\''"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn strs(args: &[OsString]) -> Vec<String> {
        args.iter().map(|a| a.to_string_lossy().into()).collect()
    }

    fn params() -> SegmentParams {
        SegmentParams {
            width: 1920,
            height: 1080,
            fps: 30,
            audio_device: None,
        }
    }

    #[test]
    fn frame_bytes_matches_bgra() {
        assert_eq!(params().frame_bytes(), 1920 * 1080 * 4);
    }

    #[test]
    fn video_only_segment_matches_the_python_build() {
        let a = strs(&segment_args(
            &params(),
            &CPU_X264,
            &PathBuf::from("out.mp4"),
        ));
        assert_eq!(
            a.join(" "),
            "-hide_banner -loglevel error -y \
             -f rawvideo -pix_fmt bgra -s 1920x1080 -r 30 \
             -thread_queue_size 512 -i pipe:0 \
             -vf pad=ceil(iw/2)*2:ceil(ih/2)*2 \
             -c:v libx264 -preset veryfast -crf 23 -pix_fmt yuv420p out.mp4"
        );
    }

    #[test]
    fn quick_sync_appends_the_nv12_conversion_to_the_filter_chain() {
        let a = strs(&segment_args(
            &params(),
            &QUICK_SYNC,
            &PathBuf::from("out.mp4"),
        ));
        let vf = &a[a.iter().position(|x| x == "-vf").unwrap() + 1];
        assert_eq!(vf, "pad=ceil(iw/2)*2:ceil(ih/2)*2,format=nv12");
        assert!(a.contains(&"h264_qsv".to_string()));
    }

    #[test]
    fn audio_brings_the_resample_and_shortest_pair() {
        let p = SegmentParams {
            audio_device: Some("Microphone Array (Realtek(R) Audio)".into()),
            ..params()
        };
        let a = strs(&segment_args(&p, &CPU_X264, &PathBuf::from("out.mp4")));
        assert!(a.contains(&"audio=Microphone Array (Realtek(R) Audio)".to_string()));
        assert!(a.contains(&"aresample=async=1:first_pts=0".to_string()));
        // -shortest is what makes ffmpeg exit, since dshow never ends
        assert!(a.contains(&"-shortest".to_string()));
        assert!(a.contains(&"aac".to_string()));
    }

    #[test]
    fn no_audio_means_no_shortest() {
        let a = strs(&segment_args(&params(), &CPU_X264, &PathBuf::from("o.mp4")));
        assert!(!a.contains(&"-shortest".to_string()));
        assert!(!a.contains(&"-c:a".to_string()));
    }

    #[test]
    fn probe_strips_the_leading_comma_from_the_filter() {
        let a = strs(&probe_args(&QUICK_SYNC));
        let vf = &a[a.iter().position(|x| x == "-vf").unwrap() + 1];
        assert_eq!(vf, "format=nv12", "a leading comma would be a parse error");
        assert!(a.ends_with(&["-f".into(), "null".into(), "-".into()]));
    }

    #[test]
    fn probe_without_a_filter_omits_vf_entirely() {
        let a = strs(&probe_args(&NVENC));
        assert!(!a.contains(&"-vf".to_string()));
    }

    #[test]
    fn concat_copy_versus_reencode() {
        let list = PathBuf::from("list.txt");
        let dest = PathBuf::from("out.mp4");
        let copy = strs(&concat_args(&list, &dest, JoinMode::Copy));
        assert!(copy.windows(2).any(|w| w == ["-c", "copy"]));

        let re = strs(&concat_args(&list, &dest, JoinMode::Reencode(CPU_X264)));
        assert!(!re.windows(2).any(|w| w == ["-c", "copy"]));
        assert!(re.contains(&"libx264".to_string()));
    }

    #[test]
    fn concat_lines_use_forward_slashes() {
        let line = concat_line(&PathBuf::from(r"C:\Users\x\AppData\Local\Temp\seg000.mp4"));
        assert_eq!(line, "file 'C:/Users/x/AppData/Local/Temp/seg000.mp4'\n");
    }

    #[test]
    fn concat_lines_escape_an_apostrophe() {
        let line = concat_line(&PathBuf::from(r"C:\Users\O'Brien\seg000.mp4"));
        assert_eq!(line, "file 'C:/Users/O'\\''Brien/seg000.mp4'\n");
    }
}
