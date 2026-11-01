//! Removing a per-user install: the Start menu entry, the entry in Windows'
//! list of installed apps and the file types.
//!
//! Earlier versions installed themselves on request, by copying the program
//! to `%LOCALAPPDATA%\Programs\Screenilux`. File types now register from
//! whichever copy runs, so nothing installs any more, but what was installed
//! still has to come out cleanly from Settings, Apps.

use std::path::PathBuf;

use windows::Win32::System::Registry::{HKEY_CURRENT_USER, RegDeleteKeyValueW, RegDeleteTreeW};
use windows::Win32::UI::Shell::{SHCNE_ASSOCCHANGED, SHCNF_IDLIST, SHChangeNotify};
use windows::Win32::UI::WindowsAndMessaging::{IDYES, MB_ICONQUESTION, MB_YESNO, MessageBoxW};
use windows::core::HSTRING;

const UNINSTALL_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Uninstall\Screenilux";

/// `%LOCALAPPDATA%\Programs\Screenilux`, where per-user apps conventionally
/// live.
pub fn install_dir() -> Option<PathBuf> {
    std::env::var_os("LOCALAPPDATA").map(|d| PathBuf::from(d).join("Programs").join("Screenilux"))
}

/// The Start menu shortcut.
pub fn shortcut_path() -> Option<PathBuf> {
    std::env::var_os("APPDATA").map(|d| {
        PathBuf::from(d)
            .join(r"Microsoft\Windows\Start Menu\Programs")
            .join("Screenilux.lnk")
    })
}

/// Remove the shortcut, the installed apps entry and the file types. The
/// program files themselves are removed by the caller, once this process has
/// exited and Windows lets go of the executable.
///
/// Recordings are the user's files and are never touched.
pub fn uninstall() {
    if let Some(lnk) = shortcut_path() {
        let _ = std::fs::remove_file(lnk);
    }
    let classes = r"Software\Classes";
    // SAFETY: deleting keys and values this program created; a missing key
    // is simply an error code, ignored.
    unsafe {
        for key in [
            UNINSTALL_KEY.to_string(),
            format!(r"{classes}\Screenilux.Recording"),
            format!(r"{classes}\Screenilux.Video"),
            format!(r"{classes}\.screenilux"),
            format!(r"{classes}\Applications\screenilux.exe"),
        ] {
            let _ = RegDeleteTreeW(HKEY_CURRENT_USER, &HSTRING::from(key));
        }
        let _ = RegDeleteKeyValueW(
            HKEY_CURRENT_USER,
            &HSTRING::from(format!(r"{classes}\.mp4\OpenWithProgids")),
            &HSTRING::from("Screenilux.Video"),
        );
        SHChangeNotify(SHCNE_ASSOCCHANGED, SHCNF_IDLIST, None, None);
    }
}

/// A Yes/No question in a standard Windows dialog, for use before any window
/// of our own exists.
pub fn confirm(title: &str, text: &str) -> bool {
    // SAFETY: a modal message box with owned, terminated strings.
    unsafe {
        MessageBoxW(
            None,
            &HSTRING::from(text),
            &HSTRING::from(title),
            MB_YESNO | MB_ICONQUESTION,
        ) == IDYES
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn installs_under_the_users_own_programs_folder() {
        let dir = install_dir().unwrap();
        assert!(dir.ends_with(r"Programs\Screenilux"));
        assert!(
            !dir.to_string_lossy().contains("Program Files"),
            "no admin rights needed"
        );
    }
}
