//! Native Windows completion notifications. Each pending notification owns an icon ID,
//! so two Sessions finishing together cannot overwrite each other's click target.
use futures::channel::mpsc;
use std::{
    cell::{Cell, RefCell},
    collections::HashMap,
    rc::Rc,
};
use windows::{
    Win32::{
        Foundation::{HWND, LPARAM, LRESULT, WPARAM},
        System::LibraryLoader::GetModuleHandleW,
        UI::{Shell::*, WindowsAndMessaging::*},
    },
    core::{PCWSTR, w},
};

const SUBCLASS_ID: usize = 0x4b4f4e;
const NOTIFICATION_MESSAGE: u32 = WM_APP + 43;

pub struct Notifications(Rc<State>);

struct State {
    hwnd: HWND,
    attached: Cell<bool>,
    // Version 4 callbacks carry a 16-bit icon ID. Do not reuse IDs: a delayed callback
    // for a dismissed notification must never activate a newer notification's Session.
    next_icon: Cell<u32>,
    sessions: RefCell<HashMap<u32, usize>>,
    clicks: mpsc::UnboundedSender<usize>,
    taskbar_created: u32,
}

impl Notifications {
    pub fn new(window: &gpui::Window) -> anyhow::Result<(Self, mpsc::UnboundedReceiver<usize>)> {
        let hwnd = crate::platform::window_handle(window)
            .ok_or_else(|| anyhow::anyhow!("No native window handle"))?;
        Self::attach(HWND(hwnd as *mut _))
    }

    fn attach(hwnd: HWND) -> anyhow::Result<(Self, mpsc::UnboundedReceiver<usize>)> {
        let (clicks, receiver) = mpsc::unbounded();
        let state = Rc::new(State {
            hwnd,
            attached: Cell::new(true),
            next_icon: Cell::new(2), // 1 belongs to the minimize-to-tray icon.
            sessions: RefCell::new(HashMap::new()),
            clicks,
            taskbar_created: unsafe { RegisterWindowMessageW(w!("TaskbarCreated")) },
        });
        anyhow::ensure!(
            unsafe {
                SetWindowSubclass(
                    hwnd,
                    Some(window_proc),
                    SUBCLASS_ID,
                    Rc::as_ptr(&state) as usize,
                )
            }
            .as_bool(),
            "Could not install notification activation handler"
        );
        Ok((Self(state), receiver))
    }

    /// Works whether minimize-to-tray is on or off, without changing that preference.
    pub fn show(&self, session: usize, label: &str) -> anyhow::Result<()> {
        let state = &self.0;
        anyhow::ensure!(state.attached.get(), "Notification window has closed");
        let id = state.next_icon.get();
        anyhow::ensure!(
            id <= u16::MAX as u32,
            "Windows notification IDs exhausted; restart Komando"
        );
        state.next_icon.set(id + 1);
        let mut data = state.icon_data(id);
        data.uFlags = NIF_ICON | NIF_MESSAGE | NIF_TIP | NIF_SHOWTIP;
        data.uCallbackMessage = NOTIFICATION_MESSAGE;
        data.hIcon = unsafe {
            GetModuleHandleW(None)
                .and_then(|module| LoadIconW(Some(module.into()), PCWSTR(1usize as *const u16)))
        }?;
        write_text(&mut data.szTip, &format!("Komando — {label}"));
        anyhow::ensure!(
            unsafe { Shell_NotifyIconW(NIM_ADD, &data) }.as_bool(),
            "Windows refused the notification icon"
        );
        state.sessions.borrow_mut().insert(id, session);
        data.Anonymous.uVersion = NOTIFYICON_VERSION_4;
        if !unsafe { Shell_NotifyIconW(NIM_SETVERSION, &data) }.as_bool() {
            state.remove(id);
            anyhow::bail!("Windows refused notification callbacks");
        }
        data.uFlags = NIF_INFO;
        data.dwInfoFlags = NIIF_INFO | NIIF_RESPECT_QUIET_TIME;
        write_text(&mut data.szInfoTitle, "Output stopped");
        write_text(
            &mut data.szInfo,
            &format!("{label} is no longer receiving output. Click to open this session."),
        );
        if !unsafe { Shell_NotifyIconW(NIM_MODIFY, &data) }.as_bool() {
            state.remove(id);
            anyhow::bail!("Windows refused the completion notification");
        }
        Ok(())
    }

    pub fn remove_session(&self, session: usize) {
        let ids: Vec<u32> = self
            .0
            .sessions
            .borrow()
            .iter()
            .filter_map(|(&id, &s)| (s == session).then_some(id))
            .collect();
        for id in ids {
            self.0.remove(id);
        }
    }
}

impl Drop for Notifications {
    fn drop(&mut self) {
        if self.0.attached.replace(false) {
            unsafe {
                let _ = RemoveWindowSubclass(self.0.hwnd, Some(window_proc), SUBCLASS_ID);
            }
        }
        self.0.clear();
    }
}

impl State {
    fn icon_data(&self, id: u32) -> NOTIFYICONDATAW {
        NOTIFYICONDATAW {
            cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32,
            hWnd: self.hwnd,
            uID: id,
            ..Default::default()
        }
    }

    fn remove(&self, id: u32) -> Option<usize> {
        let session = self.sessions.borrow_mut().remove(&id);
        if session.is_some() {
            unsafe {
                let _ = Shell_NotifyIconW(NIM_DELETE, &self.icon_data(id));
            }
        }
        session
    }

    fn clear(&self) {
        let ids: Vec<u32> = self.sessions.borrow().keys().copied().collect();
        for id in ids {
            self.remove(id);
        }
    }

    fn clicked(&self, id: u32) {
        if let Some(session) = self.remove(id) {
            // This native callback owns Windows' foreground permission. Restore here,
            // then let the asynchronous receiver select the Session on GPUI's next turn.
            unsafe {
                let show = if IsIconic(self.hwnd).as_bool() {
                    SW_RESTORE
                } else {
                    SW_SHOW
                };
                let _ = ShowWindow(self.hwnd, show);
                let _ = SetForegroundWindow(self.hwnd);
            }
            let _ = self.clicks.unbounded_send(session);
        }
    }
}

/// Preserve UTF-16 pairs and a trailing NUL even with long, non-ASCII Host names.
fn write_text(buffer: &mut [u16], text: &str) {
    buffer.fill(0);
    let mut pos = 0;
    for ch in text.chars().take_while(|ch| *ch != '\0') {
        let mut units = [0; 2];
        let units = ch.encode_utf16(&mut units);
        if pos + units.len() >= buffer.len() {
            break;
        }
        buffer[pos..pos + units.len()].copy_from_slice(units);
        pos += units.len();
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
    // Restoring can dispatch nested messages and drop the Workspace. Retain State until
    // the callback returns, just like the minimize-to-tray subclass.
    let pointer = data as *const State;
    unsafe {
        Rc::increment_strong_count(pointer);
    }
    let state = unsafe { Rc::from_raw(pointer) };
    if message == WM_NCDESTROY {
        state.attached.set(false);
        state.clear();
        unsafe {
            let _ = RemoveWindowSubclass(hwnd, Some(window_proc), SUBCLASS_ID);
        }
    } else if message == NOTIFICATION_MESSAGE {
        let id = (lparam.0 as u32) >> 16;
        match (lparam.0 as u32) & 0xffff {
            NIN_BALLOONUSERCLICK | NIN_SELECT => state.clicked(id),
            // NIN_KEYSELECT is the SDK macro NIN_SELECT | 1.
            event if event == (NIN_SELECT | 1) => state.clicked(id),
            NIN_BALLOONHIDE | NIN_BALLOONTIMEOUT => {
                state.remove(id);
            }
            _ => {}
        }
        return LRESULT(0);
    } else if state.taskbar_created != 0 && message == state.taskbar_created {
        // Explorer has discarded these icons; forget their click targets too.
        state.clear();
    }
    unsafe { DefSubclassProc(hwnd, message, wparam, lparam) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::{FutureExt as _, StreamExt as _};

    #[test]
    fn notification_clicks_keep_their_session_and_restore_the_window() {
        unsafe {
            let hwnd = CreateWindowExW(
                WINDOW_EX_STYLE::default(),
                w!("STATIC"),
                w!("Komando notification test"),
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
            let (notifications, mut clicks) = Notifications::attach(hwnd).unwrap();
            // Model pending icons without emitting actual notifications during tests.
            notifications
                .0
                .sessions
                .borrow_mut()
                .extend([(2, 101), (3, 202), (4, 303), (5, 404)]);
            let _ = ShowWindow(hwnd, SW_MINIMIZE);
            let _ = ShowWindow(hwnd, SW_HIDE);
            assert!(!IsWindowVisible(hwnd).as_bool());
            assert!(IsIconic(hwnd).as_bool());
            let send = |id: u32, event: u32| {
                SendMessageW(
                    hwnd,
                    NOTIFICATION_MESSAGE,
                    Some(WPARAM(0)),
                    Some(LPARAM(((id << 16) | event) as isize)),
                );
            };
            send(3, NIN_BALLOONUSERCLICK);
            assert_eq!(clicks.try_recv().unwrap(), 202);
            assert!(IsWindowVisible(hwnd).as_bool());
            assert!(!IsIconic(hwnd).as_bool());
            send(2, NIN_BALLOONTIMEOUT);
            send(2, NIN_BALLOONUSERCLICK); // Dismissed notifications cannot activate.
            assert!(clicks.try_recv().is_err());
            notifications.remove_session(404);
            send(5, NIN_BALLOONUSERCLICK); // Closing a Session invalidates its click target.
            assert!(clicks.try_recv().is_err());
            send(4, NIN_SELECT | 1); // Keyboard activation of the notification icon.
            assert_eq!(clicks.try_recv().unwrap(), 303);
            assert!(notifications.0.sessions.borrow().is_empty());
            drop(notifications);
            assert!(clicks.next().now_or_never().unwrap().is_none());
            DestroyWindow(hwnd).unwrap();
        }
    }

    #[test]
    fn text_is_terminated_without_splitting_surrogate_pairs() {
        let mut buffer = [99; 5];
        write_text(&mut buffer, "A😀BC");
        assert_eq!(String::from_utf16(&buffer[..4]).unwrap(), "A😀B");
        assert_eq!(buffer[4], 0);
        write_text(&mut buffer[..3], "A😀");
        assert_eq!(&buffer[..3], &[65, 0, 0]);
    }
}
