//! The window's look and its visibility to capture.
//!
//! X11 has no way to keep a window out of a screen grab, so everything a
//! Screenilux window shows can be recorded. The app knows this through
//! [`CAN_HIDE_FROM_CAPTURE`]: the camera preview is then recorded as it is
//! rather than drawn into the video a second time. Opacity and shapes are
//! left to the window manager, so those calls change nothing.

#[derive(Debug, thiserror::Error)]
pub enum AffinityError {
    #[error("this system cannot hide a window from screen capture")]
    Unsupported,
}

/// X11 cannot keep a window out of a screen grab.
pub const CAN_HIDE_FROM_CAPTURE: bool = false;

/// # Safety
///
/// No requirements here; `unsafe` only to match the Windows backend.
pub unsafe fn exclude_from_capture(_hwnd: isize) -> Result<(), AffinityError> {
    Err(AffinityError::Unsupported)
}

/// # Safety
///
/// No requirements here; `unsafe` only to match the Windows backend.
pub unsafe fn include_in_capture(_hwnd: isize) -> Result<(), AffinityError> {
    Ok(())
}

/// # Safety
///
/// No requirements here; `unsafe` only to match the Windows backend.
pub unsafe fn set_opacity(_hwnd: isize, _alpha: u8) {}

/// # Safety
///
/// No requirements here; `unsafe` only to match the Windows backend.
pub unsafe fn set_rounded(_hwnd: isize, _rounded: bool) {}

/// # Safety
///
/// No requirements here; `unsafe` only to match the Windows backend.
pub unsafe fn set_pill_shape(_hwnd: isize, _width: u32, _height: u32) {}

/// Windows are not looked up by title here; nothing needs a handle.
pub fn find_window(_title: &str) -> Option<isize> {
    None
}
