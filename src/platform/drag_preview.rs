//! A non-activating, click-through drag chip. GPUI's drag layer is clipped to its window;
//! this small native window follows the captured pointer across the desktop instead.

#[cfg(windows)]
use windows::{
    core::w,
    Win32::{
        Foundation::{COLORREF, HWND, LPARAM, POINT, RECT, WPARAM},
        Graphics::Gdi::*,
        UI::WindowsAndMessaging::*,
    },
};

pub struct DragPreview {
    #[cfg(windows)]
    hwnd: HWND,
    #[cfg(windows)]
    bitmap: HBITMAP,
}

impl DragPreview {
    #[cfg(windows)]
    pub fn new(label: &str, window: &gpui::Window, dark: bool) -> Option<Self> {
        let scale = window.scale_factor();
        let pixels = |n: f32| (n * scale).round() as i32;
        let (width, height) = (pixels(240.), pixels(32.));
        let rgb = |color: u32| COLORREF(((color & 0xff) << 16) | (color & 0xff00) | ((color >> 16) & 0xff));
        unsafe {
            let screen = GetDC(None);
            if screen.is_invalid() {
                return None;
            }
            let dc = CreateCompatibleDC(Some(screen));
            let bitmap = CreateCompatibleBitmap(screen, width, height);
            ReleaseDC(None, screen);
            if dc.is_invalid() || bitmap.is_invalid() {
                if !dc.is_invalid() { let _ = DeleteDC(dc); }
                if !bitmap.is_invalid() { let _ = DeleteObject(bitmap.into()); }
                return None;
            }
            let previous = SelectObject(dc, bitmap.into());
            let bg = rgb(if dark { 0x25272b } else { 0xfafafa });
            let fg = rgb(if dark { 0xf0f0f0 } else { 0x202124 });
            let brush = CreateSolidBrush(bg);
            FillRect(dc, &RECT { left: 0, top: 0, right: width, bottom: height }, brush);
            let _ = DeleteObject(brush.into());
            SetBkColor(dc, bg);
            SetTextColor(dc, fg);
            let font = CreateFontW(-pixels(12.5), 0, 0, 0, 400, 0, 0, 0, DEFAULT_CHARSET,
                OUT_DEFAULT_PRECIS, CLIP_DEFAULT_PRECIS, CLEARTYPE_QUALITY, 0, w!("Segoe UI"));
            let previous_font = SelectObject(dc, font.into());
            // A monitor outline keeps the chip recognisable as a desktop Session.
            let brush = CreateSolidBrush(rgb(0x4b91ff));
            let rect = RECT { left: pixels(10.), top: pixels(8.), right: pixels(26.), bottom: pixels(21.) };
            FrameRect(dc, &rect, brush);
            FillRect(dc, &RECT { left: pixels(16.), top: pixels(21.), right: pixels(20.), bottom: pixels(24.) }, brush);
            let _ = DeleteObject(brush.into());
            let mut text: Vec<u16> = label.encode_utf16().collect();
            DrawTextW(dc, &mut text, &mut RECT { left: pixels(34.), top: 0, right: width - pixels(10.), bottom: height },
                DT_SINGLELINE | DT_VCENTER | DT_END_ELLIPSIS | DT_NOPREFIX);
            SelectObject(dc, previous_font);
            let _ = DeleteObject(font.into());
            SelectObject(dc, previous);
            let _ = DeleteDC(dc);

            // STATIC with SS_BITMAP (0x0e) paints the owned bitmap without a window procedure.
            let hwnd = match CreateWindowExW(
                WS_EX_LAYERED | WS_EX_NOACTIVATE | WS_EX_TOOLWINDOW | WS_EX_TRANSPARENT,
                w!("STATIC"), w!(""), WS_POPUP | WINDOW_STYLE(0x0e),
                0, 0, width, height, None, None, None, None,
            ) {
                Ok(hwnd) => hwnd,
                Err(err) => {
                    let _ = DeleteObject(bitmap.into());
                    log::warn!("Could not create drag preview: {err}");
                    return None;
                }
            };
            SendMessageW(hwnd, STM_SETIMAGE, Some(WPARAM(IMAGE_BITMAP.0 as usize)), Some(LPARAM(bitmap.0 as isize)));
            if SetLayeredWindowAttributes(hwnd, COLORREF(0), 245, LWA_ALPHA).is_err() {
                let _ = DestroyWindow(hwnd);
                let _ = DeleteObject(bitmap.into());
                return None;
            }
            let preview = Self { hwnd, bitmap };
            preview.follow_cursor();
            Some(preview)
        }
    }

    #[cfg(not(windows))]
    pub fn new(_label: &str, _window: &gpui::Window, _dark: bool) -> Option<Self> { None }

    pub fn follow_cursor(&self) {
        #[cfg(windows)]
        unsafe {
            // Native physical coordinates handle negative monitor origins and mixed DPI.
            let mut point = POINT::default();
            if GetCursorPos(&mut point).is_ok() {
                let _ = SetWindowPos(self.hwnd, Some(HWND_TOPMOST), point.x + 12, point.y + 12, 0, 0,
                    SWP_NOACTIVATE | SWP_NOSIZE | SWP_SHOWWINDOW);
            }
        }
    }
}

impl Drop for DragPreview {
    fn drop(&mut self) {
        #[cfg(windows)]
        unsafe {
            let _ = DestroyWindow(self.hwnd);
            let _ = DeleteObject(self.bitmap.into());
        }
    }
}
