//! Keep the custom title bar flush with the work area when maximized with Mica.

use windows::Win32::{
    Foundation::{HWND, LPARAM, LRESULT, WPARAM},
    Graphics::Gdi::{GetMonitorInfoW, MONITOR_DEFAULTTONEAREST, MONITORINFO, MonitorFromRect},
    UI::{
        Shell::{DefSubclassProc, RemoveWindowSubclass, SetWindowSubclass},
        WindowsAndMessaging::{GWL_STYLE, GetWindowLongPtrW, IsZoomed, NCCALCSIZE_PARAMS, WM_NCCALCSIZE, WM_NCDESTROY, WS_CAPTION, WS_THICKFRAME},
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
            let style = GetWindowLongPtrW(hwnd, GWL_STYLE) as u32;
            if IsZoomed(hwnd).as_bool() && style & WS_CAPTION.0 == 0 && style & WS_THICKFRAME.0 != 0 {
                // GPUI's top inset assumes WS_CAPTION. Removing it for Mica changes
                // Windows' maximized frame thickness, leaving a non-client dead strip.
                // Use the proposed window's monitor so moves between displays also work.
                let monitor = MonitorFromRect(&proposed, MONITOR_DEFAULTTONEAREST);
                let mut info = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
                if GetMonitorInfoW(monitor, &mut info).as_bool() {
                    (*params).rgrc[0].top = info.rcWork.top;
                }
            }
            return result;
        }
        DefSubclassProc(hwnd, message, wparam, lparam)
    }
}
