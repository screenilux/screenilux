//! Telling Explorer that Screenilux opens recordings.
//!
//! Without this, a `.screenilux` file shows as an unknown type and Screenilux
//! is missing from "Open with". Everything is written per user, under
//! `HKCU\Software\Classes`, so it needs no administrator rights and no
//! installer.
//!
//! It deliberately does not make Screenilux the *default* for MP4: it only
//! adds itself to the list of apps that can open one. Windows also keeps the
//! user's own choice of default separately, and nothing here touches that.

use std::path::Path;

use windows::Win32::Foundation::ERROR_SUCCESS;
use windows::Win32::System::Registry::{
    HKEY, HKEY_CURRENT_USER, KEY_WRITE, REG_OPTION_NON_VOLATILE, REG_SZ, RRF_RT_REG_SZ,
    RegCloseKey, RegCreateKeyExW, RegGetValueW, RegSetValueExW,
};
use windows::Win32::UI::Shell::{SHCNE_ASSOCCHANGED, SHCNF_IDLIST, SHChangeNotify};
use windows::core::{HSTRING, PCWSTR};

const CLASSES: &str = r"Software\Classes";
const RECORDING_PROGID: &str = "Screenilux.Recording";
const VIDEO_PROGID: &str = "Screenilux.Video";

/// One registry value: `key\name = value`, `name` `None` for the default.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub key: String,
    pub name: Option<&'static str>,
    pub value: String,
}

fn entry(key: impl Into<String>, name: Option<&'static str>, value: impl Into<String>) -> Entry {
    Entry {
        key: key.into(),
        name,
        value: value.into(),
    }
}

/// What registering `exe` writes, relative to `HKCU\Software\Classes`.
pub fn association_entries(exe: &Path) -> Vec<Entry> {
    let exe_str = exe.display().to_string();
    let exe_name = exe
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "screenilux.exe".into());
    let command = format!("\"{exe_str}\" \"%1\"");
    let icon = format!("\"{exe_str}\",0");
    let app = format!(r"Applications\{exe_name}");

    vec![
        // Our own extension: Screenilux is its handler.
        entry(".screenilux", None, RECORDING_PROGID),
        entry(r".screenilux\OpenWithProgids", Some(RECORDING_PROGID), ""),
        entry(RECORDING_PROGID, None, "Screenilux recording"),
        entry(
            format!(r"{RECORDING_PROGID}\DefaultIcon"),
            None,
            icon.clone(),
        ),
        entry(
            format!(r"{RECORDING_PROGID}\shell\open\command"),
            None,
            command.clone(),
        ),
        // MP4: offered under "Open with", never made the default.
        entry(r".mp4\OpenWithProgids", Some(VIDEO_PROGID), ""),
        entry(VIDEO_PROGID, None, "MP4 video"),
        entry(format!(r"{VIDEO_PROGID}\DefaultIcon"), None, icon),
        entry(
            format!(r"{VIDEO_PROGID}\shell\open\command"),
            None,
            command.clone(),
        ),
        // The name shown in "Open with", and what the app says it opens.
        entry(app.clone(), Some("FriendlyAppName"), "Screenilux"),
        entry(format!(r"{app}\shell\open\command"), None, command),
        entry(format!(r"{app}\SupportedTypes"), Some(".screenilux"), ""),
        entry(format!(r"{app}\SupportedTypes"), Some(".mp4"), ""),
    ]
}

fn read(root: HKEY, key: &str, name: Option<&str>) -> Option<String> {
    let key = HSTRING::from(key);
    let name = name.map(HSTRING::from);
    let name_ptr = name
        .as_ref()
        .map(|n| PCWSTR(n.as_ptr()))
        .unwrap_or(PCWSTR::null());
    let mut buf = [0u16; 1024];
    let mut len = std::mem::size_of_val(&buf) as u32;
    // SAFETY: the buffer and its byte length are passed together, and
    // RRF_RT_REG_SZ guarantees a terminated string on success.
    let status = unsafe {
        RegGetValueW(
            root,
            &key,
            name_ptr,
            RRF_RT_REG_SZ,
            None,
            Some(buf.as_mut_ptr().cast()),
            Some(&mut len),
        )
    };
    if status != ERROR_SUCCESS {
        return None;
    }
    let chars = (len as usize / 2).saturating_sub(1);
    Some(String::from_utf16_lossy(&buf[..chars]))
}

fn write(root: HKEY, e: &Entry) -> Result<(), String> {
    let mut key = HKEY::default();
    // SAFETY: the created key is closed on every path below; the value is a
    // NUL-terminated UTF-16 string, passed as bytes as REG_SZ requires.
    unsafe {
        let status = RegCreateKeyExW(
            root,
            &HSTRING::from(e.key.as_str()),
            None,
            PCWSTR::null(),
            REG_OPTION_NON_VOLATILE,
            KEY_WRITE,
            None,
            &mut key,
            None,
        );
        if status != ERROR_SUCCESS {
            return Err(format!("could not create {}: {status:?}", e.key));
        }
        let wide: Vec<u16> = e.value.encode_utf16().chain(Some(0)).collect();
        let bytes = std::slice::from_raw_parts(wide.as_ptr().cast::<u8>(), wide.len() * 2);
        let name = e.name.map(HSTRING::from);
        let name_ptr = name
            .as_ref()
            .map(|n| PCWSTR(n.as_ptr()))
            .unwrap_or(PCWSTR::null());
        let status = RegSetValueExW(key, name_ptr, None, REG_SZ, Some(bytes));
        let _ = RegCloseKey(key);
        if status != ERROR_SUCCESS {
            return Err(format!("could not set {}: {status:?}", e.key));
        }
    }
    Ok(())
}

/// Write `entries` under `HKCU\<base>`. Returns how many values changed;
/// values already right are left alone.
fn apply(base: &str, entries: &[Entry]) -> Result<usize, String> {
    let mut changed = 0;
    for e in entries {
        let full = Entry {
            key: format!(r"{base}\{}", e.key),
            ..e.clone()
        };
        if read(HKEY_CURRENT_USER, &full.key, full.name).as_deref() == Some(e.value.as_str()) {
            continue;
        }
        write(HKEY_CURRENT_USER, &full)?;
        changed += 1;
    }
    Ok(changed)
}

/// Register `exe` as the handler for `.screenilux` files and as an "Open
/// with" choice for MP4.
///
/// Cheap to call on every start: values that are already right are not
/// rewritten, and Explorer is only told to refresh when something changed,
/// such as the first run or the program having moved.
pub fn register_file_types(exe: &Path) -> Result<(), String> {
    let changed = apply(CLASSES, &association_entries(exe))?;
    if changed > 0 {
        // SAFETY: the documented notification for association changes; it
        // takes no item pointers.
        unsafe { SHChangeNotify(SHCNE_ASSOCCHANGED, SHCNF_IDLIST, None, None) };
        tracing::info!(changed, "registered Screenilux to open recordings");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows::Win32::System::Registry::RegDeleteTreeW;

    const EXE: &str = r"C:\Program Files\Screenilux\screenilux.exe";

    #[test]
    fn screenilux_files_open_with_the_exe() {
        let entries = association_entries(Path::new(EXE));
        let command = entries
            .iter()
            .find(|e| e.key == r"Screenilux.Recording\shell\open\command")
            .unwrap();
        assert_eq!(
            command.value, r#""C:\Program Files\Screenilux\screenilux.exe" "%1""#,
            "both the program and the file are quoted, for paths with spaces"
        );
        assert!(entries.contains(&entry(".screenilux", None, "Screenilux.Recording")));
    }

    #[test]
    fn mp4_is_offered_but_never_taken_over() {
        let entries = association_entries(Path::new(EXE));
        assert!(
            !entries.iter().any(|e| e.key == ".mp4" && e.name.is_none()),
            "the .mp4 default must be left to the user"
        );
        assert!(
            entries
                .iter()
                .any(|e| e.key == r".mp4\OpenWithProgids" && e.name == Some("Screenilux.Video"))
        );
    }

    #[test]
    fn the_app_lists_what_it_opens_under_its_own_name() {
        let entries = association_entries(Path::new(EXE));
        assert!(entries.contains(&entry(
            r"Applications\screenilux.exe",
            Some("FriendlyAppName"),
            "Screenilux"
        )));
    }

    /// Writes to a scratch key under HKCU rather than the real classes, so
    /// the test changes nothing Explorer reads.
    #[test]
    fn values_are_written_once_and_read_back() {
        let base = format!(r"Software\ScreeniluxTest-{}", std::process::id());
        let entries = association_entries(Path::new(EXE));
        let first = apply(&base, &entries).unwrap();
        assert_eq!(first, entries.len());
        assert_eq!(
            read(HKEY_CURRENT_USER, &format!(r"{base}\.screenilux"), None).as_deref(),
            Some("Screenilux.Recording")
        );
        assert_eq!(apply(&base, &entries).unwrap(), 0, "nothing to redo");

        let moved = association_entries(Path::new(r"D:\elsewhere\screenilux.exe"));
        assert!(
            apply(&base, &moved).unwrap() > 0,
            "a moved exe is re-registered"
        );

        // SAFETY: deleting the scratch tree this test created.
        unsafe {
            let _ = RegDeleteTreeW(HKEY_CURRENT_USER, &HSTRING::from(base.as_str()));
        }
    }
}
