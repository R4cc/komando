//! Keys Windows keeps for itself, sent to a Remote Desktop instead while it has the keyboard:
//! the Windows key and what is pressed with it, Alt+Tab, Alt+Esc, Ctrl+Esc, Ctrl+Shift+Esc,
//! Print Screen, and (each by its own option) media and volume keys.
//!
//! These never reach a window's key events: Windows acts on them first. A low-level keyboard
//! hook sees them before it does. The hook is installed on the UI thread the first time a
//! desktop asks for keys, and stays; it lets every key through unless a desktop has asked and
//! one of Komando's windows is in front. Keys it takes go to that desktop over a channel.
//! Alt+F4 and Alt+Space stay with this PC, so the window can still be closed and moved.

// The hook is Windows-only; elsewhere only the tests use its decisions.
#![cfg_attr(not(windows), allow(dead_code))]

use futures::channel::mpsc::UnboundedSender;

use super::keys::Code;

/// Which keys a desktop takes from Windows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Capture {
    pub shortcuts: bool,
    pub media: bool,
    pub volume: bool,
}

impl Capture {
    pub fn any(self) -> bool {
        self.shortcuts || self.media || self.volume
    }
}

/// A key taken from Windows: down or up.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HookedKey {
    pub code: Code,
    pub down: bool,
}

// Virtual-key codes the hook decides on.
const VK_TAB: u32 = 0x09;
const VK_ESCAPE: u32 = 0x1b;
const VK_SNAPSHOT: u32 = 0x2c;
const VK_LWIN: u32 = 0x5b;
const VK_RWIN: u32 = 0x5c;
const VK_VOLUME_MUTE: u32 = 0xad;
const VK_VOLUME_UP: u32 = 0xaf;
const VK_MEDIA_NEXT_TRACK: u32 = 0xb0;
const VK_MEDIA_PLAY_PAUSE: u32 = 0xb3;

/// Which keys are being taken: decided per key press, so a key taken down is taken up too
/// (Alt may be let go before Tab).
#[derive(Debug)]
pub(crate) struct Taker {
    taken: [bool; 256],
    windows_held: bool,
}

impl Default for Taker {
    fn default() -> Self {
        Self { taken: [false; 256], windows_held: false }
    }
}

impl Taker {
    /// Whether the hook takes this key event from Windows.
    pub(crate) fn take(&mut self, capture: Capture, vk: u32, down: bool, alt: bool, ctrl: bool) -> bool {
        let Some(slot) = self.taken.get_mut(vk as usize) else { return false };
        if !down {
            let was = std::mem::take(slot);
            if matches!(vk, VK_LWIN | VK_RWIN) {
                self.windows_held = false;
            }
            return was;
        }
        let take = match vk {
            VK_LWIN | VK_RWIN => capture.shortcuts,
            VK_TAB => capture.shortcuts && alt,
            VK_ESCAPE => capture.shortcuts && (alt || ctrl),
            VK_SNAPSHOT => capture.shortcuts,
            VK_MEDIA_NEXT_TRACK..=VK_MEDIA_PLAY_PAUSE => capture.media,
            VK_VOLUME_MUTE..=VK_VOLUME_UP => capture.volume,
            // Win+E, Win+Shift+S and the like: everything pressed with the Windows key.
            _ => capture.shortcuts && self.windows_held,
        };
        if take {
            *slot = true;
            if matches!(vk, VK_LWIN | VK_RWIN) {
                self.windows_held = true;
            }
        }
        take
    }
}

/// The X11 keysym for a virtual key (VNC sends symbols, RDP the scancode). 0 when unknown.
pub(crate) fn keysym_for_vk(vk: u32, char_of: impl Fn(u32) -> Option<char>) -> u32 {
    match vk {
        0x08 => 0xff08,
        VK_TAB => 0xff09,
        0x0d => 0xff0d,
        0x10 | 0xa0 => 0xffe1,
        0xa1 => 0xffe2,
        0x11 | 0xa2 => 0xffe3,
        0xa3 => 0xffe4,
        0x12 | 0xa4 => 0xffe9,
        0xa5 => 0xffea,
        VK_ESCAPE => 0xff1b,
        0x20 => 0x20,
        0x21 => 0xff55,
        0x22 => 0xff56,
        0x23 => 0xff57,
        0x24 => 0xff50,
        0x25 => 0xff51,
        0x26 => 0xff52,
        0x27 => 0xff53,
        0x28 => 0xff54,
        VK_SNAPSHOT => 0xff61,
        0x2d => 0xff63,
        0x2e => 0xffff,
        VK_LWIN => 0xffeb,
        VK_RWIN => 0xffec,
        0x5d => 0xff67,
        0x70..=0x7b => 0xffbe + (vk - 0x70),
        // XF86 media and volume keys.
        VK_VOLUME_MUTE => 0x1008_ff12,
        0xae => 0x1008_ff11,
        VK_VOLUME_UP => 0x1008_ff13,
        VK_MEDIA_NEXT_TRACK => 0x1008_ff17,
        0xb1 => 0x1008_ff16,
        0xb2 => 0x1008_ff15,
        VK_MEDIA_PLAY_PAUSE => 0x1008_ff14,
        0x30..=0x39 => vk,
        0x41..=0x5a => vk + 0x20,
        _ => char_of(vk).map(|c| super::keys::keysym_for_char(c.to_ascii_lowercase())).unwrap_or(0),
    }
}

/// Send keys `capture` takes to `sender` from now on, in place of any desktop before.
pub fn start(capture: Capture, sender: UnboundedSender<HookedKey>) {
    #[cfg(windows)]
    windows_hook::start(capture, sender);
    #[cfg(not(windows))]
    let _ = (capture, sender);
}

/// The desktop sending to `sender` no longer takes keys: Windows has them all again. Another
/// desktop's capture that started since is left alone.
pub fn stop(sender: &UnboundedSender<HookedKey>) {
    #[cfg(windows)]
    windows_hook::stop(sender);
    #[cfg(not(windows))]
    let _ = sender;
}

#[cfg(windows)]
mod windows_hook {
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicBool, Ordering};

    use futures::channel::mpsc::UnboundedSender;
    use windows::Win32::Foundation::{LPARAM, LRESULT, WPARAM};
    use windows::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows::Win32::System::Threading::GetCurrentProcessId;
    use windows::Win32::UI::Input::KeyboardAndMouse::{GetAsyncKeyState, MAPVK_VK_TO_CHAR, MapVirtualKeyW, VK_CONTROL};
    use windows::Win32::UI::WindowsAndMessaging::{
        CallNextHookEx, GetForegroundWindow, GetWindowThreadProcessId, HC_ACTION, KBDLLHOOKSTRUCT, LLKHF_ALTDOWN,
        LLKHF_EXTENDED, LLKHF_INJECTED, LLKHF_UP, SetWindowsHookExW, WH_KEYBOARD_LL,
    };

    use super::{Capture, Code, HookedKey, Taker, keysym_for_vk};

    struct Active {
        capture: Capture,
        sender: UnboundedSender<HookedKey>,
        taker: Taker,
    }

    static ACTIVE: Mutex<Option<Active>> = Mutex::new(None);
    static INSTALLED: AtomicBool = AtomicBool::new(false);

    pub fn start(capture: Capture, sender: UnboundedSender<HookedKey>) {
        install();
        *ACTIVE.lock().unwrap_or_else(|e| e.into_inner()) = Some(Active { capture, sender, taker: Taker::default() });
    }

    pub fn stop(sender: &UnboundedSender<HookedKey>) {
        let mut active = ACTIVE.lock().unwrap_or_else(|e| e.into_inner());
        if active.as_ref().is_some_and(|a| a.sender.same_receiver(sender)) {
            *active = None;
        }
    }

    /// On the UI thread, whose message loop runs the hook.
    fn install() {
        if INSTALLED.swap(true, Ordering::AcqRel) {
            return;
        }
        // SAFETY: `hook` matches HOOKPROC and lives as long as the process.
        let installed = unsafe {
            let module = GetModuleHandleW(None).ok();
            SetWindowsHookExW(WH_KEYBOARD_LL, Some(hook), module.map(Into::into), 0)
        };
        if let Err(err) = installed {
            log::warn!("keyboard hook for remote desktops: {err}");
            INSTALLED.store(false, Ordering::Release);
        }
    }

    unsafe extern "system" fn hook(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
        if code == HC_ACTION as i32 {
            // SAFETY: for HC_ACTION, lparam points at the event's KBDLLHOOKSTRUCT.
            let info = unsafe { &*(lparam.0 as *const KBDLLHOOKSTRUCT) };
            if take(info) {
                return LRESULT(1);
            }
        }
        // SAFETY: passing the event on, as every hook must for what it doesn't take.
        unsafe { CallNextHookEx(None, code, wparam, lparam) }
    }

    fn take(info: &KBDLLHOOKSTRUCT) -> bool {
        // Keys sent by programs (and by Komando itself) go their way.
        if info.flags.contains(LLKHF_INJECTED) {
            return false;
        }
        let Ok(mut active) = ACTIVE.try_lock() else { return false };
        let Some(active) = active.as_mut() else { return false };
        if !komando_in_front() {
            return false;
        }
        let down = !info.flags.contains(LLKHF_UP);
        let alt = info.flags.contains(LLKHF_ALTDOWN);
        // SAFETY: reads the key's state; no pointers.
        let ctrl = unsafe { GetAsyncKeyState(VK_CONTROL.0.into()) } < 0;
        if !active.taker.take(active.capture, info.vkCode, down, alt, ctrl) {
            return false;
        }
        let extended = if info.flags.contains(LLKHF_EXTENDED) { 0xe000 } else { 0 };
        let char_of = |vk: u32| {
            // SAFETY: a lookup in the keyboard layout; no pointers.
            let c = unsafe { MapVirtualKeyW(vk, MAPVK_VK_TO_CHAR) } & 0xffff;
            char::from_u32(c).filter(|c| *c != '\0')
        };
        let code = Code { scancode: (info.scanCode as u16 & 0xff) | extended, keysym: keysym_for_vk(info.vkCode, char_of) };
        active.sender.unbounded_send(HookedKey { code, down }).is_ok()
    }

    /// Whether the window in front is one of Komando's.
    fn komando_in_front() -> bool {
        let mut pid = 0u32;
        // SAFETY: plain window queries; `pid` outlives the call.
        unsafe {
            let window = GetForegroundWindow();
            GetWindowThreadProcessId(window, Some(&mut pid));
            pid == GetCurrentProcessId()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ALL: Capture = Capture { shortcuts: true, media: true, volume: true };

    #[test]
    fn system_shortcuts_are_taken_down_and_up() {
        let mut taker = Taker::default();
        // Alt+Tab, with Alt let go before Tab: Tab's release is taken too.
        assert!(taker.take(ALL, VK_TAB, true, true, false));
        assert!(taker.take(ALL, VK_TAB, false, false, false));
        // Tab alone stays with the window, which sends it as a key event.
        assert!(!taker.take(ALL, VK_TAB, true, false, false));
        assert!(!taker.take(ALL, VK_TAB, false, false, false));
        assert!(taker.take(ALL, VK_ESCAPE, true, false, true));
        assert!(!taker.take(ALL, VK_ESCAPE, true, false, false));
        assert!(taker.take(ALL, VK_SNAPSHOT, true, false, false));
    }

    #[test]
    fn everything_pressed_with_the_windows_key_is_taken() {
        let mut taker = Taker::default();
        assert!(taker.take(ALL, VK_LWIN, true, false, false));
        assert!(taker.take(ALL, 0x45, true, false, false)); // E
        assert!(taker.take(ALL, 0x45, false, false, false));
        assert!(taker.take(ALL, VK_LWIN, false, false, false));
        assert!(!taker.take(ALL, 0x45, true, false, false));
    }

    #[test]
    fn each_option_takes_only_its_keys() {
        let none = Capture::default();
        let media = Capture { media: true, ..none };
        let mut taker = Taker::default();
        assert!(!taker.take(none, VK_LWIN, true, false, false));
        assert!(!taker.take(media, VK_VOLUME_UP, true, false, false));
        assert!(taker.take(media, VK_MEDIA_PLAY_PAUSE, true, false, false));
        assert!(!taker.take(media, VK_TAB, true, true, false));
    }

    #[test]
    fn virtual_keys_have_keysyms() {
        let none = |_| None;
        assert_eq!(keysym_for_vk(VK_LWIN, none), 0xffeb);
        assert_eq!(keysym_for_vk(0x45, none), 'e' as u32);
        assert_eq!(keysym_for_vk(0x73, none), 0xffc1); // F4
        assert_eq!(keysym_for_vk(VK_MEDIA_PLAY_PAUSE, none), 0x1008_ff14);
        assert_eq!(keysym_for_vk(0xbd, |_| Some('-')), '-' as u32);
    }
}
