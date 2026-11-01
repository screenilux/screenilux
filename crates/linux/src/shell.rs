//! Telling the desktop that Screenilux opens recordings, the freedesktop way:
//! a `.desktop` entry for the app and a MIME type for `.screenilux` files,
//! both in the user's own data folder, so no administrator rights are needed.
//!
//! Like on Windows it offers itself for MP4 without becoming the default.

use std::path::{Path, PathBuf};

const DESKTOP_FILE: &str = "screenilux.desktop";
const MIME_FILE: &str = "screenilux.xml";
pub const RECORDING_TYPE: &str = "application/x-screenilux";

/// `$XDG_DATA_HOME`, or `~/.local/share`.
fn data_home() -> Option<PathBuf> {
    std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share")))
}

/// The desktop entry that runs `exe` on a file.
pub fn desktop_entry(exe: &Path) -> String {
    format!(
        "[Desktop Entry]\n\
         Type=Application\n\
         Name=Screenilux\n\
         Comment=Record your screens, then play them back and copy the text in them\n\
         Exec=\"{}\" %f\n\
         Terminal=false\n\
         Categories=AudioVideo;Video;Recorder;\n\
         MimeType={RECORDING_TYPE};video/mp4;\n",
        exe.display()
    )
}

/// The MIME type for `.screenilux` files, which are ZIP archives.
pub fn mime_package() -> String {
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
         <mime-info xmlns=\"http://www.freedesktop.org/standards/shared-mime-info\">\n\
         \x20 <mime-type type=\"{RECORDING_TYPE}\">\n\
         \x20   <comment>Screenilux recording</comment>\n\
         \x20   <sub-class-of type=\"application/zip\"/>\n\
         \x20   <glob pattern=\"*.screenilux\"/>\n\
         \x20 </mime-type>\n\
         </mime-info>\n"
    )
}

/// Write `contents` to `path` unless it already holds exactly that. Returns
/// whether anything changed.
fn write_if_changed(path: &Path, contents: &str) -> Result<bool, String> {
    if std::fs::read_to_string(path).is_ok_and(|now| now == contents) {
        return Ok(false);
    }
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    std::fs::write(path, contents).map_err(|e| e.to_string())?;
    Ok(true)
}

/// Point `.screenilux` files and "Open with" for MP4 at `exe`. Only changed
/// files are written, and the desktop's caches are only refreshed then.
pub fn register_file_types(exe: &Path) -> Result<(), String> {
    let home = data_home().ok_or("no home folder")?;
    let apps = home.join("applications");
    let mime = home.join("mime");
    let app_changed = write_if_changed(&apps.join(DESKTOP_FILE), &desktop_entry(exe))?;
    let mime_changed = write_if_changed(&mime.join("packages").join(MIME_FILE), &mime_package())?;
    // Both tools are optional: a desktop without them picks the files up on
    // its next scan.
    if mime_changed {
        let _ = std::process::Command::new("update-mime-database")
            .arg(&mime)
            .status();
    }
    if app_changed {
        let _ = std::process::Command::new("update-desktop-database")
            .arg(&apps)
            .status();
    }
    Ok(())
}

/// Open the folder holding `path` (or `path` itself, if it is a folder) in
/// the file manager.
pub fn reveal(path: &Path) {
    let folder = if path.is_dir() {
        path
    } else {
        path.parent().unwrap_or(path)
    };
    let _ = std::process::Command::new("xdg-open").arg(folder).spawn();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_desktop_entry_runs_the_program_on_a_file() {
        let entry = desktop_entry(Path::new("/opt/Screenilux App/screenilux"));
        assert!(entry.contains("Exec=\"/opt/Screenilux App/screenilux\" %f"));
        assert!(entry.contains("MimeType=application/x-screenilux;video/mp4;"));
    }

    #[test]
    fn recordings_get_their_own_type() {
        let xml = mime_package();
        assert!(xml.contains("*.screenilux"));
        assert!(xml.contains("application/zip"));
    }

    #[test]
    fn registering_writes_once_and_then_leaves_things_alone() {
        let dir = std::env::temp_dir().join(format!("screenilux-xdg-{}", std::process::id()));
        let file = dir.join("a/b.txt");
        assert!(write_if_changed(&file, "x").unwrap());
        assert!(!write_if_changed(&file, "x").unwrap());
        assert!(write_if_changed(&file, "y").unwrap());
        let _ = std::fs::remove_dir_all(dir);
    }
}
