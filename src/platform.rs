//! Small Windows-only touches gpui doesn't cover.

/// Tell Windows whether the window is dark. This picks the dark or light Mica tint and title
/// bar. gpui sets it from the *system* theme; Komando's App Theme can differ from that.
#[cfg(windows)]
pub fn set_window_dark_mode(window: &gpui::Window, dark: bool) {
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    use windows::Win32::Foundation::HWND;
    use windows::Win32::Graphics::Dwm::{DWMWA_USE_IMMERSIVE_DARK_MODE, DwmSetWindowAttribute};

    let Ok(handle) = window.window_handle() else { return };
    if let RawWindowHandle::Win32(win32) = handle.as_raw() {
        let hwnd = HWND(win32.hwnd.get() as *mut core::ffi::c_void);
        let value: i32 = dark as i32; // Win32 BOOL
        unsafe {
            let _ = DwmSetWindowAttribute(
                hwnd,
                DWMWA_USE_IMMERSIVE_DARK_MODE,
                &value as *const i32 as *const core::ffi::c_void,
                std::mem::size_of::<i32>() as u32,
            );
        }
    }
}

#[cfg(not(windows))]
pub fn set_window_dark_mode(_window: &gpui::Window, _dark: bool) {}

/// Mica is a Windows 11 feature; elsewhere (Linux dev builds) the window stays opaque.
pub const SUPPORTS_BACKDROP: bool = cfg!(windows);
