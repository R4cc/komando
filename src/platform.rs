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

/// Configure DWM directly: gpui 0.3.12's Mica path has a version probe with an
/// uninitialised structure size, and its Opaque path never clears SYSTEMBACKDROP_TYPE.
/// Return whether the backdrop was enabled, so failed native calls keep our UI opaque.
#[cfg(windows)]
pub fn set_backdrop(window: &gpui::Window, on: bool) -> bool {
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    use windows::Win32::Foundation::HWND;
    use windows::Win32::Graphics::Dwm::{
        DWMWA_SYSTEMBACKDROP_TYPE, DWMSBT_MAINWINDOW, DWMSBT_NONE,
        DwmExtendFrameIntoClientArea, DwmSetWindowAttribute,
    };
    use windows::Win32::UI::Controls::MARGINS;

    let Ok(handle) = HasWindowHandle::window_handle(window) else { return false };
    let RawWindowHandle::Win32(win32) = handle.as_raw() else { return false };
    let hwnd = HWND(win32.hwnd.get() as *mut core::ffi::c_void);
    let configure = |enabled| -> windows::core::Result<()> {
        let backdrop = if enabled { DWMSBT_MAINWINDOW } else { DWMSBT_NONE };
        let m = if enabled { -1 } else { 0 };
        let margins = MARGINS { cxLeftWidth: m, cxRightWidth: m, cyTopHeight: m, cyBottomHeight: m };
        unsafe {
            DwmSetWindowAttribute(
                hwnd,
                DWMWA_SYSTEMBACKDROP_TYPE,
                &backdrop as *const _ as *const core::ffi::c_void,
                std::mem::size_of_val(&backdrop) as u32,
            )?;
            DwmExtendFrameIntoClientArea(hwnd, &margins)
        }
    };
    let enabled = match configure(on) {
        Ok(()) => on,
        Err(error) => {
            log::warn!("Could not configure Windows backdrop: {error}");
            // Best effort rollback if extending the frame failed after enabling Mica.
            let _ = configure(false);
            false
        }
    };
    set_native_caption(hwnd, !enabled);
    enabled
}

/// Give the window a native caption, or take it away.
///
/// Windows adds `WS_CAPTION` to every overlapped window, so DWM lays out its own minimise,
/// maximise and close buttons at the top right even though gpui hides the title bar. With
/// the frame extended over the whole window for Mica, DWM paints them there, beneath
/// Komando's translucent title bar, and they show through next to our own buttons. Without
/// a caption there is nothing for DWM to draw. Hit testing (dragging, snap layouts on the
/// maximise button, closing) comes from `WM_NCHITTEST` and the system menu, which stay.
#[cfg(windows)]
fn set_native_caption(hwnd: windows::Win32::Foundation::HWND, caption: bool) {
    use windows::Win32::UI::WindowsAndMessaging::{
        GWL_STYLE, GetWindowLongPtrW, SWP_FRAMECHANGED, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOOWNERZORDER, SWP_NOSIZE,
        SWP_NOZORDER, SetWindowLongPtrW, SetWindowPos, WS_CAPTION,
    };

    unsafe {
        let style = GetWindowLongPtrW(hwnd, GWL_STYLE);
        let wanted = if caption { style | WS_CAPTION.0 as isize } else { style & !(WS_CAPTION.0 as isize) };
        if wanted == style {
            return;
        }
        SetWindowLongPtrW(hwnd, GWL_STYLE, wanted);
        // The frame is cached until it is told to recalculate.
        let flags = SWP_FRAMECHANGED | SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | SWP_NOOWNERZORDER | SWP_NOACTIVATE;
        if let Err(error) = SetWindowPos(hwnd, None, 0, 0, 0, 0, flags) {
            log::warn!("Could not update the window frame: {error}");
        }
    }
}

#[cfg(not(windows))]
pub fn set_backdrop(_window: &gpui::Window, _on: bool) -> bool { false }

/// Mica is a Windows 11 feature; elsewhere (Linux dev builds) the window stays opaque.
pub const SUPPORTS_BACKDROP: bool = cfg!(windows);

#[cfg(windows)]
mod tray;
#[cfg(windows)]
pub use tray::Tray;
