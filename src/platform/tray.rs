//! The subclass and notification icon live exactly as long as the Workspace.
use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use std::{cell::Cell, rc::Rc};
use windows::{
    Win32::{
        Foundation::{HWND, LPARAM, LRESULT, POINT, WPARAM},
        System::LibraryLoader::GetModuleHandleW,
        UI::{Shell::*, WindowsAndMessaging::*},
    },
    core::{PCWSTR, w},
};

const SUBCLASS_ID: usize = 0x4b4f4d;
const TRAY_MESSAGE: u32 = WM_APP + 42;
const ICON_ID: u32 = 1;
// shellapi.h defines NIN_KEYSELECT as NIN_SELECT | NINF_KEY (1).
const NIN_KEYSELECT: u32 = NIN_SELECT | 1;

pub struct Tray {
    state: Rc<State>,
    allowed: bool,
}

struct State {
    hwnd: HWND,
    enabled: Cell<bool>,
    visible: Cell<bool>,
    taskbar_created: u32,
    attached: Cell<bool>,
    exiting: Cell<bool>,
}

impl Tray {
    pub fn new(window: &gpui::Window, enabled: bool, allowed: bool) -> Option<Self> {
        let handle = HasWindowHandle::window_handle(window).ok()?;
        let RawWindowHandle::Win32(handle) = handle.as_raw() else {
            return None;
        };
        let state = Rc::new(State {
            hwnd: HWND(handle.hwnd.get() as *mut _),
            enabled: Cell::new(enabled && allowed),
            attached: Cell::new(true),
            visible: Cell::new(false),
            exiting: Cell::new(false),
            taskbar_created: unsafe { RegisterWindowMessageW(w!("TaskbarCreated")) },
        });
        // Rc keeps the callback address stable even when Workspace moves. All calls and
        // destruction happen on the window's UI thread; the subclass never owns State.
        if !unsafe { SetWindowSubclass(state.hwnd, Some(window_proc), SUBCLASS_ID, &*state as *const State as usize) }.as_bool() {
            log::warn!("Could not install minimize-to-tray handler");
            return None;
        }
        Some(Self { state, allowed })
    }

    pub fn set_enabled(&self, enabled: bool) {
        let enabled = enabled && self.allowed;
        self.state.enabled.set(enabled);
        if !enabled && self.state.visible.get() {
            self.state.restore();
        }
    }

    pub fn refresh_icon(&self) {
        if self.state.visible.get() {
            let mut data = self.state.icon_data();
            data.uFlags = NIF_ICON;
            if let Some(icon) = self.state.window_icon() {
                data.hIcon = icon;
                unsafe { let _ = Shell_NotifyIconW(NIM_MODIFY, &data); }
            }
        }
    }
}

impl Drop for Tray {
    fn drop(&mut self) {
        if self.state.attached.replace(false) {
            unsafe {
                let _ = RemoveWindowSubclass(self.state.hwnd, Some(window_proc), SUBCLASS_ID);
            }
        }
        self.state.remove_icon();
    }
}

impl State {
    fn window_icon(&self) -> Option<HICON> {
        // The window owns the current choice; shared resource handles are never destroyed.
        let icon = unsafe { SendMessageW(self.hwnd, WM_GETICON, Some(WPARAM(ICON_SMALL as usize)), None) };
        if icon.0 != 0 {
            Some(HICON(icon.0 as *mut _))
        } else {
            unsafe { GetModuleHandleW(None).and_then(|module| LoadIconW(Some(module.into()), PCWSTR(1usize as *const u16))).ok() }
        }
    }

    fn icon_data(&self) -> NOTIFYICONDATAW {
        NOTIFYICONDATAW { cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32, hWnd: self.hwnd, uID: ICON_ID, ..Default::default() }
    }

    fn add_icon(&self) -> bool {
        let mut data = self.icon_data();
        data.uFlags = NIF_ICON | NIF_MESSAGE | NIF_TIP | NIF_SHOWTIP;
        data.uCallbackMessage = TRAY_MESSAGE;
        let Some(icon) = self.window_icon() else { return false };
        data.hIcon = icon;
        for (dest, ch) in data.szTip.iter_mut().zip("Komando".encode_utf16()) {
            *dest = ch;
        }
        if !unsafe { Shell_NotifyIconW(NIM_ADD, &data) }.as_bool() {
            return false;
        }
        data.Anonymous.uVersion = NOTIFYICON_VERSION_4;
        if !unsafe { Shell_NotifyIconW(NIM_SETVERSION, &data) }.as_bool() {
            unsafe {
                let _ = Shell_NotifyIconW(NIM_DELETE, &data);
            }
            return false;
        }
        true
    }

    fn remove_icon(&self) {
        if self.visible.replace(false) {
            unsafe {
                let _ = Shell_NotifyIconW(NIM_DELETE, &self.icon_data());
            }
        }
    }

    fn restore(&self) {
        unsafe {
            let _ = ShowWindow(self.hwnd, SW_RESTORE);
            let _ = SetForegroundWindow(self.hwnd);
        }
        self.remove_icon();
    }

    fn menu(&self) {
        unsafe {
            let Ok(menu) = CreatePopupMenu() else { return };
            let mut point = POINT::default();
            let setup = AppendMenuW(menu, MF_STRING, 1, w!("Show Komando"))
                .and_then(|_| AppendMenuW(menu, MF_STRING, 2, w!("Exit")))
                .and_then(|_| GetCursorPos(&mut point));
            if setup.is_ok() {
                let _ = SetForegroundWindow(self.hwnd);
                let command = TrackPopupMenu(menu, TPM_RETURNCMD | TPM_RIGHTBUTTON, point.x, point.y, None, self.hwnd, None).0;
                // Required by TrackPopupMenu for a notification-area context menu.
                let _ = PostMessageW(Some(self.hwnd), WM_NULL, WPARAM(0), LPARAM(0));
                match command {
                    1 => self.restore(),
                    2 => {
                        self.exiting.set(true);
                        if PostMessageW(Some(self.hwnd), WM_CLOSE, WPARAM(0), LPARAM(0)).is_err() {
                            self.exiting.set(false);
                        }
                    }
                    _ => {}
                }
            }
            let _ = DestroyMenu(menu);
        }
    }
}

unsafe extern "system" fn window_proc(hwnd: HWND, message: u32, wparam: WPARAM, lparam: LPARAM, _id: usize, data: usize) -> LRESULT {
    // A nested message can close the window and drop Workspace while this callback is
    // still on the stack (notably TrackPopupMenu). Retain State until we return.
    let pointer = data as *const State;
    unsafe {
        Rc::increment_strong_count(pointer);
    }
    let state = unsafe { Rc::from_raw(pointer) };
    if message == WM_NCDESTROY {
        state.remove_icon();
        state.attached.set(false);
        unsafe {
            let _ = RemoveWindowSubclass(hwnd, Some(window_proc), SUBCLASS_ID);
        }
    } else if message == WM_CLOSE && state.enabled.get() && !state.exiting.get() {
        // X and Alt+F4 send WM_CLOSE, not the WM_SIZE notification from minimizing.
        // Register the way back first; if Explorer refuses it, retain normal close behavior.
        if state.visible.get() || state.add_icon() {
            state.visible.set(true);
            unsafe {
                // Minimize before hiding so restoring preserves the maximized state too.
                let _ = ShowWindow(hwnd, SW_MINIMIZE);
                let _ = ShowWindow(hwnd, SW_HIDE);
            }
            return LRESULT(0);
        }
    } else if message == WM_SIZE && wparam.0 == SIZE_MINIMIZED as usize && state.enabled.get() {
        // Never hide the only way back into the app unless the shell accepted the icon.
        if state.visible.get() || state.add_icon() {
            state.visible.set(true);
            unsafe {
                let _ = ShowWindow(hwnd, SW_HIDE);
            }
        }
    } else if message == WM_SIZE && wparam.0 != SIZE_MINIMIZED as usize {
        state.remove_icon();
    } else if message == TRAY_MESSAGE && ((lparam.0 as u32 >> 16) == ICON_ID) {
        match lparam.0 as u32 & 0xffff {
            NIN_SELECT | NIN_KEYSELECT => state.restore(),
            WM_CONTEXTMENU => state.menu(),
            _ => {}
        }
        return LRESULT(0);
    } else if state.taskbar_created != 0 && message == state.taskbar_created && state.visible.get() {
        // Explorer restarted: put the icon back, or restore the window if that fails.
        if !state.add_icon() {
            state.restore();
        }
    }
    unsafe { DefSubclassProc(hwnd, message, wparam, lparam) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auxiliary_windows_cannot_enable_another_tray_icon() {
        let state = Rc::new(State {
            hwnd: HWND::default(), enabled: Cell::new(false), visible: Cell::new(false),
            taskbar_created: 0, attached: Cell::new(false), exiting: Cell::new(false),
        });
        let tray = Tray { state: state.clone(), allowed: false };
        tray.set_enabled(true);
        assert!(!state.enabled.get());
        assert!(!state.visible.get());
    }

    #[test]
    fn close_keeps_the_window_alive_when_tray_is_enabled() {
        assert!(send_close(true, false), "Close must hide the window instead of destroying it when tray is enabled");
    }

    #[test]
    fn close_destroys_the_window_when_tray_is_disabled() {
        assert!(!send_close(false, false));
    }

    #[test]
    fn explicit_exit_destroys_the_window_even_when_tray_is_enabled() {
        assert!(!send_close(true, true));
    }

    fn send_close(enabled: bool, exiting: bool) -> bool {
        unsafe {
            let hwnd = CreateWindowExW(
                WINDOW_EX_STYLE::default(),
                w!("STATIC"),
                w!("Komando tray close test"),
                WS_OVERLAPPEDWINDOW,
                0,
                0,
                100,
                100,
                None,
                None,
                None,
                None,
            )
            .unwrap();
            let state = Rc::new(State {
                hwnd,
                enabled: Cell::new(enabled),
                visible: Cell::new(true),
                taskbar_created: 0,
                attached: Cell::new(true),
                exiting: Cell::new(exiting),
            });
            assert!(SetWindowSubclass(hwnd, Some(window_proc), SUBCLASS_ID, &*state as *const State as usize).as_bool());
            // Model the already-registered notification icon; exercise the real Win32 close
            // message and subclass without depending on Explorer's notification area.
            SendMessageW(hwnd, WM_CLOSE, Some(WPARAM(0)), Some(LPARAM(0)));
            let survived = IsWindow(Some(hwnd)).as_bool();
            if survived {
                DestroyWindow(hwnd).unwrap();
            }
            survived
        }
    }
}
