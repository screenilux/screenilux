//! Keeping our own windows out of the recording.

use windows::Win32::Foundation::HWND;
use windows::Win32::UI::WindowsAndMessaging::{
    GA_ROOT, GetAncestor, SetWindowDisplayAffinity, WDA_EXCLUDEFROMCAPTURE, WDA_NONE,
};

#[derive(Debug, thiserror::Error)]
pub enum AffinityError {
    #[error("the window handle was null")]
    NullHandle,
    #[error(
        "this Windows build does not support excluding a window from capture \
         (needs Windows 10 2004 or newer): {0}"
    )]
    Unsupported(String),
}

/// Make a window visible on screen but invisible to screen capture.
///
/// The camera preview is a real always-on-top window so you can see yourself
/// while recording, but the recorder also composites the camera into the video
/// itself. Without this the grab would catch the preview too and the recording
/// would carry two copies of your face, one of them scaled and lagging.
///
/// Returns an error rather than a bool. The Python build discarded the result,
/// so on a Windows build without `WDA_EXCLUDEFROMCAPTURE` it produced exactly
/// the double image its own docs promised to prevent, silently.
///
/// # Safety
///
/// `hwnd` must be a live top-level window handle, or null.
pub unsafe fn exclude_from_capture(hwnd: isize) -> Result<(), AffinityError> {
    unsafe { set_affinity(hwnd, true) }
}

/// Undo [`exclude_from_capture`], making the window recordable again.
///
/// # Safety
///
/// `hwnd` must be a live top-level window handle, or null.
pub unsafe fn include_in_capture(hwnd: isize) -> Result<(), AffinityError> {
    unsafe { set_affinity(hwnd, false) }
}

unsafe fn set_affinity(hwnd: isize, exclude: bool) -> Result<(), AffinityError> {
    if hwnd == 0 {
        return Err(AffinityError::NullHandle);
    }
    let handle = HWND(hwnd as *mut std::ffi::c_void);

    // A UI toolkit may hand back a child handle; display affinity only applies
    // to a top-level window, and setting it on a child silently does nothing.
    // SAFETY: the caller guarantees a live handle.
    let root = unsafe { GetAncestor(handle, GA_ROOT) };
    let target = if root.is_invalid() { handle } else { root };

    let affinity = if exclude {
        WDA_EXCLUDEFROMCAPTURE
    } else {
        WDA_NONE
    };

    // SAFETY: a live top-level handle and a valid affinity constant.
    unsafe { SetWindowDisplayAffinity(target, affinity) }
        .map_err(|e| AffinityError::Unsupported(e.to_string()))
}

/// Whether this Windows build can exclude a window from capture at all.
///
/// `WDA_EXCLUDEFROMCAPTURE` arrived in Windows 10 2004; older builds reject it.
/// Probing with a throwaway window lets the UI warn up front rather than
/// producing a doubled camera halfway through a recording.
///
/// # Safety
///
/// `hwnd` must be a live top-level window handle.
pub unsafe fn supports_exclusion(hwnd: isize) -> bool {
    // SAFETY: delegated to the caller's guarantee.
    unsafe {
        let ok = exclude_from_capture(hwnd).is_ok();
        if ok {
            let _ = include_in_capture(hwnd);
        }
        ok
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_null_handle_is_rejected_rather_than_passed_to_win32() {
        // SAFETY: null is explicitly allowed and handled before any call.
        let err = unsafe { exclude_from_capture(0) }.unwrap_err();
        assert!(matches!(err, AffinityError::NullHandle));
    }

    #[test]
    fn an_invalid_handle_reports_an_error_rather_than_succeeding_quietly() {
        // A handle that is not a window: Win32 must refuse, and we must notice.
        // SAFETY: passing a bogus handle is the point; Win32 validates it.
        let result = unsafe { exclude_from_capture(0x7FFF_FFFF) };
        assert!(
            result.is_err(),
            "a bad handle must surface, not be swallowed like the Python build did"
        );
    }
}
