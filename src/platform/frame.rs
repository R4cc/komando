//! Keep a maximized window inside the work area when Mica takes away its caption.
//!
//! Windows only fits captioned windows to the work area when maximizing. Without
//! `WS_CAPTION` the window can cover the whole monitor (seen after switching the taskbar
//! from auto-hide to always shown), putting the taskbar over Komando's content. Clamping
//! the client area leaves any overlap as hidden non-client area beneath the taskbar.

use windows::Win32::{
    Foundation::{HWND, LPARAM, LRESULT, WPARAM},
    Graphics::Gdi::{GetMonitorInfoW, MONITOR_DEFAULTTONEAREST, MONITORINFO, MonitorFromRect},
    UI::{
        Shell::{DefSubclassProc, RemoveWindowSubclass, SetWindowSubclass},
        WindowsAndMessaging::{
            GWL_STYLE, GetWindowLongPtrW, IsZoomed, NCCALCSIZE_PARAMS, SPI_SETWORKAREA, SWP_FRAMECHANGED, SWP_NOACTIVATE,
            SWP_NOMOVE, SWP_NOOWNERZORDER, SWP_NOSIZE, SWP_NOZORDER, SetWindowPos, WM_NCCALCSIZE, WM_NCDESTROY,
            WM_SETTINGCHANGE, WS_CAPTION, WS_THICKFRAME,
        },
    },
};

const SUBCLASS_ID: usize = 0x4B4F4652;

pub(super) fn install(hwnd: HWND) {
    // Reinstalling the same procedure/id updates the existing subclass, so changing
    // themes or toggling Mica cannot stack handlers. No per-window state is needed.
    if !unsafe { SetWindowSubclass(hwnd, Some(window_proc), SUBCLASS_ID, 0) }.as_bool() {
        log::warn!("Could not install the custom window frame handler");
    }
}

unsafe extern "system" fn window_proc(hwnd: HWND, message: u32, wparam: WPARAM, lparam: LPARAM, _id: usize, _data: usize) -> LRESULT {
    unsafe {
        if message == WM_NCDESTROY {
            let _ = RemoveWindowSubclass(hwnd, Some(window_proc), SUBCLASS_ID);
        }
        if message == WM_NCCALCSIZE && wparam.0 != 0 {
            let params = lparam.0 as *mut NCCALCSIZE_PARAMS;
            let proposed = (*params).rgrc[0];
            let result = DefSubclassProc(hwnd, message, wparam, lparam);
            if IsZoomed(hwnd).as_bool() && captionless(hwnd) {
                // GPUI's top inset assumes WS_CAPTION. Removing it for Mica changes
                // Windows' maximized frame thickness, leaving a non-client dead strip,
                // and the other edges can reach under an always-shown taskbar.
                // Use the proposed window's monitor so moves between displays also work.
                let monitor = MonitorFromRect(&proposed, MONITOR_DEFAULTTONEAREST);
                let mut info = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
                if GetMonitorInfoW(monitor, &mut info).as_bool() {
                    let work = info.rcWork;
                    let client = &mut (*params).rgrc[0];
                    client.top = work.top;
                    client.left = client.left.max(work.left);
                    client.right = client.right.min(work.right);
                    client.bottom = client.bottom.min(work.bottom);
                }
            }
            return result;
        }
        let result = DefSubclassProc(hwnd, message, wparam, lparam);
        if message == WM_SETTINGCHANGE && wparam.0 == SPI_SETWORKAREA.0 as usize && IsZoomed(hwnd).as_bool() && captionless(hwnd) {
            // The taskbar was shown, hidden or moved: Windows may leave a captionless window's
            // rectangle alone, so ask for the client area again against the new work area.
            let flags = SWP_FRAMECHANGED | SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | SWP_NOOWNERZORDER | SWP_NOACTIVATE;
            let _ = SetWindowPos(hwnd, None, 0, 0, 0, 0, flags);
        }
        result
    }
}

/// Mica's frame: resizable, but without the caption Windows uses to respect the work area.
unsafe fn captionless(hwnd: HWND) -> bool {
    let style = unsafe { GetWindowLongPtrW(hwnd, GWL_STYLE) } as u32;
    style & WS_CAPTION.0 == 0 && style & WS_THICKFRAME.0 != 0
}
