//! Small Windows-only touches gpui doesn't cover.

/// Tell Windows whether the window is dark. This picks the dark or light Mica tint and title
/// bar. gpui sets it from the *system* theme; Komando's App Theme can differ from that.
#[cfg(windows)]
pub fn set_window_dark_mode(window: &gpui::Window, dark: bool) {
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    use windows::Win32::Foundation::HWND;
    use windows::Win32::Graphics::Dwm::{DWMWA_USE_IMMERSIVE_DARK_MODE, DwmSetWindowAttribute};

    // gpui's Window has its own `window_handle()` (returning AnyWindowHandle); call the
    // raw-window-handle trait method explicitly.
    let Ok(handle) = HasWindowHandle::window_handle(window) else { return };
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

/// Let the Mica backdrop show behind the whole window, or only behind its (invisible) frame.
/// gpui asks DWM for Mica, but DWM paints it in the frame alone; Komando draws its own title
/// bar, so the client area is the whole visible window and Mica stayed hidden. Extending the
/// frame over the client area ("sheet of glass") puts the backdrop under everything we draw.
#[cfg(windows)]
pub fn extend_backdrop(window: &gpui::Window, on: bool) {
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    use windows::Win32::Foundation::HWND;
    use windows::Win32::Graphics::Dwm::DwmExtendFrameIntoClientArea;
    use windows::Win32::UI::Controls::MARGINS;

    let Ok(handle) = HasWindowHandle::window_handle(window) else { return };
    if let RawWindowHandle::Win32(win32) = handle.as_raw() {
        let hwnd = HWND(win32.hwnd.get() as *mut core::ffi::c_void);
        let m = if on { -1 } else { 0 };
        let margins = MARGINS { cxLeftWidth: m, cxRightWidth: m, cyTopHeight: m, cyBottomHeight: m };
        unsafe {
            let _ = DwmExtendFrameIntoClientArea(hwnd, &margins);
        }
    }
}

#[cfg(not(windows))]
pub fn extend_backdrop(_window: &gpui::Window, _on: bool) {}

/// Mica is a Windows 11 feature; elsewhere (Linux dev builds) the window stays opaque.
pub const SUPPORTS_BACKDROP: bool = cfg!(windows);
