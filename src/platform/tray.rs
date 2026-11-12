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
}

struct State {
    hwnd: HWND,
    enabled: Cell<bool>,
    visible: Cell<bool>,
    taskbar_created: u32,
    attached: Cell<bool>,
}

impl Tray {
    pub fn new(window: &gpui::Window, enabled: bool) -> Option<Self> {
        let handle = HasWindowHandle::window_handle(window).ok()?;
        let RawWindowHandle::Win32(handle) = handle.as_raw() else {
            return None;
        };
        let state = Rc::new(State {
            hwnd: HWND(handle.hwnd.get() as *mut _),
            enabled: Cell::new(enabled),
            attached: Cell::new(true),
            visible: Cell::new(false),
            taskbar_created: unsafe { RegisterWindowMessageW(w!("TaskbarCreated")) },
        });
        // Rc keeps the callback address stable even when Workspace moves. All calls and
        // destruction happen on the window's UI thread; the subclass never owns State.
        if !unsafe {
            SetWindowSubclass(
                state.hwnd,
                Some(window_proc),
                SUBCLASS_ID,
                &*state as *const State as usize,
            )
        }
        .as_bool()
        {
            log::warn!("Could not install minimize-to-tray handler");
            return None;
        }
        Some(Self { state })
    }

    pub fn set_enabled(&self, enabled: bool) {
        self.state.enabled.set(enabled);
        if !enabled && self.state.visible.get() {
            self.state.restore();
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
    fn icon_data(&self) -> NOTIFYICONDATAW {
        NOTIFYICONDATAW {
            cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32,
            hWnd: self.hwnd,
            uID: ICON_ID,
            ..Default::default()
        }
    }

    fn add_icon(&self) -> bool {
        let mut data = self.icon_data();
        data.uFlags = NIF_ICON | NIF_MESSAGE | NIF_TIP | NIF_SHOWTIP;
        data.uCallbackMessage = TRAY_MESSAGE;
        let icon = unsafe {
            GetModuleHandleW(None)
                .and_then(|module| LoadIconW(Some(module.into()), PCWSTR(1usize as *const u16)))
        };
        let Ok(icon) = icon else { return false };
        data.hIcon = icon; // LoadIcon returns a shared resource; it must not be destroyed.
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
                let command = TrackPopupMenu(
                    menu,
                    TPM_RETURNCMD | TPM_RIGHTBUTTON,
                    point.x,
                    point.y,
                    None,
                    self.hwnd,
                    None,
                )
                .0;
                // Required by TrackPopupMenu for a notification-area context menu.
                let _ = PostMessageW(Some(self.hwnd), WM_NULL, WPARAM(0), LPARAM(0));
                match command {
                    1 => self.restore(),
                    2 => {
                        let _ = PostMessageW(Some(self.hwnd), WM_CLOSE, WPARAM(0), LPARAM(0));
                    }
                    _ => {}
                }
            }
            let _ = DestroyMenu(menu);
        }
    }
}

unsafe extern "system" fn window_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
    _id: usize,
    data: usize,
) -> LRESULT {
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
    } else if state.taskbar_created != 0 && message == state.taskbar_created && state.visible.get()
    {
        // Explorer restarted: put the icon back, or restore the window if that fails.
        if !state.add_icon() {
            state.restore();
        }
    }
    unsafe { DefSubclassProc(hwnd, message, wparam, lparam) }
}
