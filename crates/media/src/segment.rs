//! The ffmpeg-backed [`VideoSink`]: one child process per segment.
//!
//! A recording is cut into segments at every pause. Closing the encoder at a
//! pause and opening a fresh one on resume is what keeps microphone audio in
//! sync: a single long-lived ffmpeg would keep pulling mic input while the
//! video was paused, pushing the two further apart with every pause.

use crate::command::{
    CPU_X264, EncoderSpec, JoinMode, SegmentParams, concat_args, concat_line, segment_args,
};
use crate::proc;
use crate::util::TempDir;
use screenilux_core::ports::{SinkError, VideoSink, WriteStatus};
use std::fs::File;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::time::{Duration, Instant};

/// How long to let an encoder finish writing its container after we close the
/// pipe. Generous, because this is where the file gets its moov atom.
const FINALISE_TIMEOUT: Duration = Duration::from_secs(90);
const CONCAT_TIMEOUT: Duration = Duration::from_secs(300);

struct Segment {
    path: PathBuf,
    encoder: &'static str,
}

struct Running {
    child: Child,
    stdin: ChildStdin,
    log: File,
}

pub struct FfmpegSink {
    ffmpeg: PathBuf,
    params: SegmentParams,
    encoder: EncoderSpec,
    tmp: TempDir,
    segments: Vec<Segment>,
    running: Option<Running>,
    /// Kept so a segment can be topped up before it closes.
    last_frame: Option<Vec<u8>>,
}

impl FfmpegSink {
    pub fn new(
        ffmpeg: impl Into<PathBuf>,
        params: SegmentParams,
        encoder: EncoderSpec,
    ) -> Result<Self, SinkError> {
        Ok(Self {
            ffmpeg: ffmpeg.into(),
            params,
            encoder,
            tmp: TempDir::new("screenilux")?,
            segments: Vec::new(),
            running: None,
            last_frame: None,
        })
    }

    fn next_path(&self) -> PathBuf {
        self.tmp
            .path()
            .join(format!("seg{:03}.mp4", self.segments.len()))
    }

    fn spawn(&mut self) -> Result<(), SinkError> {
        let path = self.next_path();
        let args = segment_args(&self.params, &self.encoder, &path);

        let log_path = path.with_extension("log");
        let log = File::options()
            .create(true)
            .read(true)
            .write(true)
            .truncate(true)
            .open(&log_path)?;

        let mut cmd = Command::new(&self.ffmpeg);
        cmd.args(&args)
            .stdin(Stdio::piped())
            .stdout(Stdio::from(log.try_clone()?))
            .stderr(Stdio::from(log.try_clone()?));
        proc::configure(&mut cmd);

        let mut child = cmd
            .spawn()
            .map_err(|e| SinkError::StartFailed(format!("{}: {e}", self.ffmpeg.display())))?;
        let stdin = child.stdin.take().expect("stdin was piped");

        tracing::debug!(encoder = self.encoder.label, path = %path.display(), "segment opened");
        self.segments.push(Segment {
            path,
            encoder: self.encoder.label,
        });
        self.running = Some(Running { child, stdin, log });
        Ok(())
    }

    /// Drop to software encoding after a hardware encoder died mid-stream.
    ///
    /// ffmpeg reading from a pipe does not open its encoder until frames
    /// arrive, so an encoder that probed fine can still fail on the real
    /// stream — the first write is where we find out. Returns false when
    /// already on the fallback, meaning the failure is real.
    fn fall_back(&mut self) -> Result<bool, SinkError> {
        if self.encoder.label == CPU_X264.label {
            return Ok(false);
        }
        let failed = self.encoder.label;
        let reason = self.take_log_tail();

        if let Some(mut r) = self.running.take() {
            let _ = r.child.kill();
            let _ = r.child.wait();
        }
        // the dead segment holds nothing playable
        if let Some(dead) = self.segments.pop() {
            let _ = std::fs::remove_file(&dead.path);
        }

        tracing::warn!(
            failed_encoder = failed,
            reason = %reason,
            "encoder failed mid-stream, switching to CPU encoding"
        );
        self.encoder = CPU_X264;
        self.last_frame = None;
        self.spawn()?;
        Ok(true)
    }

    fn take_log_tail(&mut self) -> String {
        let Some(r) = self.running.as_mut() else {
            return String::new();
        };
        let mut buf = String::new();
        if r.log.seek(SeekFrom::Start(0)).is_ok() {
            let _ = r.log.read_to_string(&mut buf);
        }
        buf.lines()
            .rev()
            .find(|l| !l.trim().is_empty())
            .unwrap_or("")
            .to_string()
    }

    /// Segments that actually hold data.
    fn usable(&self) -> Vec<&Segment> {
        self.segments
            .iter()
            .filter(|s| s.path.metadata().map(|m| m.len() > 0).unwrap_or(false))
            .collect()
    }

    fn join_mode(&self) -> JoinMode {
        let usable = self.usable();
        let first = usable.first().map(|s| s.encoder);
        let mixed = usable.iter().any(|s| Some(s.encoder) != first);
        if mixed {
            // A stream copy across segments from different encoders can yield a
            // file that only plays the first segment's parameters, because the
            // SPS/PPS differ. Re-encode instead; this only happens after a
            // mid-recording fallback.
            tracing::warn!("segments came from different encoders; re-encoding the join");
            JoinMode::Reencode(CPU_X264)
        } else {
            JoinMode::Copy
        }
    }
}

impl VideoSink for FfmpegSink {
    fn open_segment(&mut self) -> Result<(), SinkError> {
        if self.running.is_some() {
            return Ok(());
        }
        self.last_frame = None;
        self.spawn()
    }

    fn write(&mut self, frame: &[u8]) -> Result<WriteStatus, SinkError> {
        let expected = self.params.frame_bytes();
        if frame.len() != expected {
            return Err(SinkError::EncoderDied(format!(
                "frame is {} bytes, expected {expected}",
                frame.len()
            )));
        }

        let Some(r) = self.running.as_mut() else {
            return Err(SinkError::EncoderDied("no segment is open".into()));
        };

        match r.stdin.write_all(frame) {
            Ok(()) => {
                if self.last_frame.as_deref() != Some(frame) {
                    self.last_frame = Some(frame.to_vec());
                }
                Ok(WriteStatus::Ok)
            }
            Err(_) => {
                // the encoder went away; this is where a bad hardware encoder
                // finally reveals itself
                if self.fall_back()? {
                    if let Some(r) = self.running.as_mut() {
                        r.stdin.write_all(frame)?;
                        self.last_frame = Some(frame.to_vec());
                    }
                    Ok(WriteStatus::SegmentRestarted {
                        encoder: self.encoder.label,
                    })
                } else {
                    Err(SinkError::EncoderDied(self.take_log_tail()))
                }
            }
        }
    }

    fn close_segment(&mut self, pad: u64) -> Result<(), SinkError> {
        let Some(mut r) = self.running.take() else {
            return Ok(());
        };

        // Top the segment up before closing. Without this a stall right before
        // a pause or stop silently shortens the clip, and with audio it
        // desyncs the tail.
        if pad > 0
            && let Some(frame) = self.last_frame.clone()
        {
            for _ in 0..pad {
                if r.stdin.write_all(&frame).is_err() {
                    tracing::warn!("encoder closed early while padding the segment");
                    break;
                }
            }
        }

        drop(r.stdin); // EOF is what tells ffmpeg to finalise

        let deadline = Instant::now() + FINALISE_TIMEOUT;
        let status = loop {
            match r.child.try_wait()? {
                Some(s) => break Some(s),
                None if Instant::now() >= deadline => {
                    tracing::error!("encoder did not finish in time; killing it");
                    let _ = r.child.kill();
                    let _ = r.child.wait();
                    break None;
                }
                None => std::thread::sleep(Duration::from_millis(10)),
            }
        };

        if status.map(|s| !s.success()).unwrap_or(true) {
            let mut buf = String::new();
            let _ = r.log.seek(SeekFrom::Start(0));
            let _ = r.log.read_to_string(&mut buf);
            let tail = buf
                .lines()
                .rev()
                .find(|l| !l.trim().is_empty())
                .unwrap_or("");
            tracing::warn!(reason = tail, "encoder exited with an error");
        }
        Ok(())
    }

    fn finish(&mut self, dest: &Path) -> Result<(), SinkError> {
        self.close_segment(0)?;

        let usable: Vec<PathBuf> = self.usable().iter().map(|s| s.path.clone()).collect();
        if usable.is_empty() {
            return Err(SinkError::EncoderDied("nothing was recorded".into()));
        }

        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent)?;
        }

        if usable.len() == 1 {
            // a rename across volumes fails, so fall back to a copy
            if std::fs::rename(&usable[0], dest).is_err() {
                std::fs::copy(&usable[0], dest)?;
            }
            return Ok(());
        }

        let mode = self.join_mode();
        let list = self.tmp.path().join("segments.txt");
        let mut f = File::create(&list)?;
        for p in &usable {
            f.write_all(concat_line(p).as_bytes())?;
        }
        f.sync_all()?;
        drop(f);

        let out = proc::run(
            &self.ffmpeg,
            &concat_args(&list, dest, mode),
            CONCAT_TIMEOUT,
        )?;
        if !out.success() {
            return Err(SinkError::EncoderDied(format!(
                "joining segments failed: {}",
                out.reason()
            )));
        }
        Ok(())
    }

    fn encoder_label(&self) -> &'static str {
        self.encoder.label
    }
}

impl Drop for FfmpegSink {
    fn drop(&mut self) {
        if let Some(mut r) = self.running.take() {
            let _ = r.child.kill();
            let _ = r.child.wait();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::command::QUICK_SYNC;

    fn ffmpeg() -> Option<PathBuf> {
        proc::locate_ffmpeg()
    }

    fn params(w: u32, h: u32, fps: u32) -> SegmentParams {
        SegmentParams {
            width: w,
            height: h,
            fps,
            audio_device: None,
        }
    }

    /// A frame of a single flat colour, so output is tiny and fast to encode.
    fn frame(p: &SegmentParams, shade: u8) -> Vec<u8> {
        vec![shade; p.frame_bytes()]
    }

    fn duration_of(ffmpeg: &Path, file: &Path) -> f64 {
        let probe = ffmpeg.with_file_name(if cfg!(windows) {
            "ffprobe.exe"
        } else {
            "ffprobe"
        });
        let out = Command::new(probe)
            .args([
                "-v",
                "error",
                "-show_entries",
                "format=duration",
                "-of",
                "csv=p=0",
            ])
            .arg(file)
            .output()
            .expect("ffprobe");
        String::from_utf8_lossy(&out.stdout)
            .trim()
            .parse()
            .unwrap_or(0.0)
    }

    #[test]
    fn encodes_a_segment_of_synthetic_frames() {
        let Some(ff) = ffmpeg() else { return };
        let p = params(320, 240, 30);
        let dir = TempDir::new("sink-test").unwrap();
        let dest = dir.path().join("out.mp4");

        let mut sink = FfmpegSink::new(&ff, p.clone(), CPU_X264).unwrap();
        sink.open_segment().unwrap();
        for i in 0..30 {
            assert_eq!(sink.write(&frame(&p, i as u8)).unwrap(), WriteStatus::Ok);
        }
        sink.close_segment(0).unwrap();
        sink.finish(&dest).unwrap();

        assert!(dest.is_file());
        assert!(dest.metadata().unwrap().len() > 0);
        let d = duration_of(&ff, &dest);
        assert!((d - 1.0).abs() < 0.2, "expected ~1s, got {d}");
    }

    #[test]
    fn padding_lengthens_a_short_segment() {
        let Some(ff) = ffmpeg() else { return };
        let p = params(320, 240, 30);
        let dir = TempDir::new("sink-pad").unwrap();
        let dest = dir.path().join("out.mp4");

        let mut sink = FfmpegSink::new(&ff, p.clone(), CPU_X264).unwrap();
        sink.open_segment().unwrap();
        for _ in 0..15 {
            sink.write(&frame(&p, 7)).unwrap();
        }
        // owed another 15 frames at the moment of closing
        sink.close_segment(15).unwrap();
        sink.finish(&dest).unwrap();

        let d = duration_of(&ff, &dest);
        assert!(
            (d - 1.0).abs() < 0.2,
            "padding should have produced ~1s, got {d}"
        );
    }

    #[test]
    fn segments_are_joined_across_a_pause() {
        let Some(ff) = ffmpeg() else { return };
        let p = params(320, 240, 30);
        let dir = TempDir::new("sink-join").unwrap();
        let dest = dir.path().join("out.mp4");

        let mut sink = FfmpegSink::new(&ff, p.clone(), CPU_X264).unwrap();
        for _ in 0..2 {
            sink.open_segment().unwrap();
            for _ in 0..30 {
                sink.write(&frame(&p, 3)).unwrap();
            }
            sink.close_segment(0).unwrap();
        }
        sink.finish(&dest).unwrap();

        let d = duration_of(&ff, &dest);
        assert!(
            (d - 2.0).abs() < 0.3,
            "expected ~2s from two segments, got {d}"
        );
    }

    #[test]
    fn a_wrong_sized_frame_is_rejected_rather_than_desyncing_the_stream() {
        let Some(ff) = ffmpeg() else { return };
        let p = params(320, 240, 30);
        let mut sink = FfmpegSink::new(&ff, p.clone(), CPU_X264).unwrap();
        sink.open_segment().unwrap();
        let err = sink.write(&[0u8; 10]).unwrap_err();
        assert!(matches!(err, SinkError::EncoderDied(_)));
    }

    #[test]
    fn finishing_with_no_data_is_an_error_not_an_empty_file() {
        let Some(ff) = ffmpeg() else { return };
        let dir = TempDir::new("sink-empty").unwrap();
        let mut sink = FfmpegSink::new(&ff, params(320, 240, 30), CPU_X264).unwrap();
        assert!(sink.finish(&dir.path().join("out.mp4")).is_err());
    }

    /// The join must not stream-copy across encoders. We cannot force a real
    /// mid-stream failure deterministically, so assert the decision directly.
    #[test]
    fn mixed_encoder_segments_are_rejoined_by_re_encoding() {
        let Some(ff) = ffmpeg() else { return };
        let mut sink = FfmpegSink::new(&ff, params(320, 240, 30), CPU_X264).unwrap();
        let dir = sink.tmp.path().to_path_buf();

        for (i, label) in [QUICK_SYNC.label, CPU_X264.label].iter().enumerate() {
            let path = dir.join(format!("seg{i:03}.mp4"));
            std::fs::write(&path, b"not empty").unwrap();
            sink.segments.push(Segment {
                path,
                encoder: label,
            });
        }
        assert!(matches!(sink.join_mode(), JoinMode::Reencode(_)));
    }

    #[test]
    fn homogeneous_segments_are_stream_copied() {
        let Some(ff) = ffmpeg() else { return };
        let mut sink = FfmpegSink::new(&ff, params(320, 240, 30), CPU_X264).unwrap();
        let dir = sink.tmp.path().to_path_buf();
        for i in 0..2 {
            let path = dir.join(format!("seg{i:03}.mp4"));
            std::fs::write(&path, b"not empty").unwrap();
            sink.segments.push(Segment {
                path,
                encoder: CPU_X264.label,
            });
        }
        assert_eq!(sink.join_mode(), JoinMode::Copy);
    }

    #[test]
    fn the_working_directory_is_removed_on_drop() {
        let Some(ff) = ffmpeg() else { return };
        let kept;
        {
            let sink = FfmpegSink::new(&ff, params(320, 240, 30), CPU_X264).unwrap();
            kept = sink.tmp.path().to_path_buf();
            assert!(kept.is_dir());
        }
        assert!(!kept.exists(), "temp dir must not leak");
    }
}
