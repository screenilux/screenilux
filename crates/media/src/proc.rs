//! Running ffmpeg and not hanging on it.
//!
//! `std::process::Child` has no wait-with-timeout, and every ffmpeg call here
//! can in principle hang: a probe against a wedged driver, a device
//! enumeration that blocks on a busy camera, an encoder that will not exit.
//! Everything goes through [`run`] so none of them can stall the app forever.

use std::ffi::OsString;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// Suppress the console window ffmpeg would otherwise flash up.
#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// Apply the platform flags every ffmpeg invocation wants.
pub fn configure(cmd: &mut Command) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
}

#[derive(Debug, Clone)]
pub struct Output {
    pub status: Option<i32>,
    pub stderr: String,
    pub timed_out: bool,
}

impl Output {
    pub fn success(&self) -> bool {
        self.status == Some(0) && !self.timed_out
    }

    /// The last line of stderr, which is where ffmpeg puts the actual reason.
    pub fn reason(&self) -> String {
        self.stderr
            .lines()
            .rev()
            .find(|l| !l.trim().is_empty())
            .unwrap_or("no output")
            .to_string()
    }
}

/// Run to completion, or kill it at `timeout`.
pub fn run(ffmpeg: &Path, args: &[OsString], timeout: Duration) -> std::io::Result<Output> {
    let mut cmd = Command::new(ffmpeg);
    cmd.args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    configure(&mut cmd);

    let mut child = cmd.spawn()?;

    // Drain stderr on its own thread: a full pipe buffer would deadlock a
    // process we are simultaneously waiting on.
    let mut pipe = child.stderr.take().expect("stderr was piped");
    let reader = std::thread::spawn(move || {
        let mut buf = String::new();
        let _ = pipe.read_to_string(&mut buf);
        buf
    });

    let deadline = Instant::now() + timeout;
    let mut timed_out = false;
    let status = loop {
        match child.try_wait()? {
            Some(s) => break s.code(),
            None if Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                timed_out = true;
                break None;
            }
            None => std::thread::sleep(Duration::from_millis(10)),
        }
    };

    let stderr = reader.join().unwrap_or_default();
    Ok(Output {
        status,
        stderr,
        timed_out,
    })
}

/// Where the bundled ffmpeg lives, falling back to PATH.
///
/// Next to the executable first, so a bundled build never silently uses a
/// different ffmpeg that happens to be installed.
pub fn locate_ffmpeg() -> Option<PathBuf> {
    let name = if cfg!(windows) {
        "ffmpeg.exe"
    } else {
        "ffmpeg"
    };

    if let Ok(exe) = std::env::current_exe()
        && let Some(dir) = exe.parent()
    {
        for candidate in [dir.join(name), dir.join("ffmpeg").join(name)] {
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }

    std::env::var_os("PATH")
        .iter()
        .flat_map(std::env::split_paths)
        .map(|dir| dir.join(name))
        .find(|p| p.is_file())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ffmpeg() -> Option<PathBuf> {
        locate_ffmpeg()
    }

    #[test]
    fn locates_ffmpeg_on_this_machine() {
        // Not an assertion about every machine - just that resolution works
        // where ffmpeg is installed, which is the case here.
        if let Some(p) = ffmpeg() {
            assert!(p.is_file());
        }
    }

    #[test]
    fn a_trivial_run_succeeds_and_captures_stderr() {
        let Some(ff) = ffmpeg() else { return };
        let out = run(&ff, &["-version".into()], Duration::from_secs(10)).unwrap();
        assert!(out.success(), "stderr: {}", out.stderr);
        assert!(!out.timed_out);
    }

    #[test]
    fn a_bad_invocation_reports_a_reason_rather_than_panicking() {
        let Some(ff) = ffmpeg() else { return };
        let out = run(
            &ff,
            &["-f".into(), "definitely-not-a-format".into()],
            Duration::from_secs(10),
        )
        .unwrap();
        assert!(!out.success());
        assert!(!out.reason().is_empty());
    }

    #[test]
    fn a_hanging_process_is_killed_at_the_deadline() {
        let Some(ff) = ffmpeg() else { return };
        // an endless synthetic source with no output limit would run forever
        let args: Vec<OsString> = [
            "-f",
            "lavfi",
            "-i",
            "testsrc=size=320x240:rate=30",
            "-f",
            "null",
            "-",
        ]
        .iter()
        .map(OsString::from)
        .collect();

        let start = Instant::now();
        let out = run(&ff, &args, Duration::from_millis(400)).unwrap();
        assert!(out.timed_out, "expected the deadline to fire");
        assert!(
            start.elapsed() < Duration::from_secs(5),
            "kill must be prompt, took {:?}",
            start.elapsed()
        );
    }
}
