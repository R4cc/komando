//! Keys for a Remote Desktop: what the view's key events become for RDP (scancodes, the
//! key's place on the keyboard) and VNC (X11 keysyms, the symbol on it).
//!
//! A character typed without Ctrl or Alt is sent as itself (RDP's Unicode key events, VNC's
//! keysym for it), so it arrives right whatever keyboard layout either side uses. A key with
//! Ctrl or Alt, and keys without a character (Enter, arrows, F-keys), are sent as keys, so the
//! remote side sees shortcuts as shortcuts.

/// A key for the remote desktop.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Key {
    /// A character to type: down and up in one.
    Char(char),
    Code(Code),
}

/// A key on the keyboard: its RDP scancode (set 1, `0xE0..` for extended keys) and its X11
/// keysym.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Code {
    pub scancode: u16,
    pub keysym: u32,
}

const fn code(scancode: u16, keysym: u32) -> Code {
    Code { scancode, keysym }
}

pub const CONTROL: Code = code(0x1d, 0xffe3);
pub const SHIFT: Code = code(0x2a, 0xffe1);
pub const ALT: Code = code(0x38, 0xffe9);
pub const WINDOWS: Code = code(0xe05b, 0xffeb);
pub const DELETE: Code = code(0xe053, 0xffff);

/// What a key press becomes, from gpui's `key` and `key_char` and the modifiers held. `None`
/// for keys a remote desktop has no use for.
pub fn from_keystroke(key: &str, key_char: Option<&str>, ctrl: bool, alt: bool) -> Option<Key> {
    if !ctrl
        && !alt
        && let Some(c) = key_char.and_then(single_char)
        && !c.is_control()
    {
        return Some(Key::Char(c));
    }
    // AltGr, which Windows reports as Ctrl+Alt, typing a character of its own (`@` is AltGr+Q
    // on a German keyboard): that character, whatever layout the remote side uses.
    if ctrl
        && alt
        && let Some(c) = key_char.and_then(single_char)
        && !c.is_control()
        && !c.to_string().eq_ignore_ascii_case(key)
    {
        return Some(Key::Char(c));
    }
    if let Some(named) = named(key) {
        return Some(Key::Code(named));
    }
    let c = single_char(key)?.to_ascii_lowercase();
    Some(Key::Code(Code { scancode: scancode_for_char(c)?, keysym: keysym_for_char(c) }))
}

/// macOS: Option typing a symbol of its own (`@` is Option+L on a German Mac, `€` Option+E,
/// `|` Option+7): that character, whatever layout the remote side uses. Option with a key that
/// gives a letter (`ƒ`, `ß` on a US Mac) stays Alt, so Alt shortcuts like Alt+F still work.
pub fn mac_option_char(key: &str, key_char: Option<&str>, ctrl: bool, alt: bool) -> Option<Key> {
    if !alt || ctrl {
        return None;
    }
    let c = key_char.and_then(single_char)?;
    (!c.is_control() && !c.is_alphanumeric() && !c.to_string().eq_ignore_ascii_case(key)).then_some(Key::Char(c))
}

fn single_char(text: &str) -> Option<char> {
    let mut chars = text.chars();
    let c = chars.next()?;
    chars.next().is_none().then_some(c)
}

/// The X11 keysym that types `c`: Latin-1 as itself, anything else in the Unicode block.
pub fn keysym_for_char(c: char) -> u32 {
    match c as u32 {
        code @ (0x20..=0x7e | 0xa0..=0xff) => code,
        code => 0x0100_0000 + code,
    }
}

/// Keys gpui names rather than by their character.
fn named(key: &str) -> Option<Code> {
    Some(match key {
        "escape" => code(0x01, 0xff1b),
        "backspace" => code(0x0e, 0xff08),
        "tab" => code(0x0f, 0xff09),
        "enter" => code(0x1c, 0xff0d),
        "space" => code(0x39, 0x20),
        "insert" => code(0xe052, 0xff63),
        "delete" => DELETE,
        "home" => code(0xe047, 0xff50),
        "end" => code(0xe04f, 0xff57),
        "pageup" => code(0xe049, 0xff55),
        "pagedown" => code(0xe051, 0xff56),
        "left" => code(0xe04b, 0xff51),
        "up" => code(0xe048, 0xff52),
        "right" => code(0xe04d, 0xff53),
        "down" => code(0xe050, 0xff54),
        "menu" => code(0xe05d, 0xff67),
        _ => return function_key(key),
    })
}

/// F1 to F12 (F13 and up have no scancode most servers know).
fn function_key(key: &str) -> Option<Code> {
    let n: u16 = key.strip_prefix('f')?.parse().ok()?;
    let scancode = match n {
        1..=10 => 0x3b + n - 1,
        11 => 0x57,
        12 => 0x58,
        _ => return None,
    };
    Some(code(scancode, 0xffbe + u32::from(n) - 1))
}

/// The scancode of the key that types `c` on this PC's keyboard layout, so a shortcut lands on
/// the same key on a remote desktop with the same layout (Ctrl+Z is Ctrl+Z on a German
/// keyboard too). Falls back to the US layout.
fn scancode_for_char(c: char) -> Option<u16> {
    #[cfg(windows)]
    if let Some(scancode) = layout_scancode(c) {
        return Some(scancode);
    }
    us_scancode(c)
}

#[cfg(windows)]
fn layout_scancode(c: char) -> Option<u16> {
    use windows::Win32::UI::Input::KeyboardAndMouse::{MAPVK_VK_TO_VSC_EX, MapVirtualKeyW, VkKeyScanW};
    let mut unit = [0u16; 2];
    let [unit] = c.encode_utf16(&mut unit) else { return None };
    // SAFETY: plain lookups in the current keyboard layout, with no pointers involved.
    let scan = unsafe {
        let vk = VkKeyScanW(*unit);
        if vk == -1 {
            return None;
        }
        MapVirtualKeyW((vk as u16 & 0xff).into(), MAPVK_VK_TO_VSC_EX)
    };
    // VSC_EX gives 0xE0nn for extended keys, which is how `Code` writes them.
    (scan != 0).then_some(scan as u16)
}

fn us_scancode(c: char) -> Option<u16> {
    const ROWS: [(&str, u16); 4] = [("1234567890-=", 0x02), ("qwertyuiop[]", 0x10), ("asdfghjkl;'`", 0x1e), ("zxcvbnm,./", 0x2c)];
    if c == '\\' {
        return Some(0x2b);
    }
    ROWS.iter().find_map(|(row, first)| row.chars().position(|k| k == c).map(|i| first + i as u16))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn typed_characters_go_as_themselves() {
        assert_eq!(from_keystroke("a", Some("a"), false, false), Some(Key::Char('a')));
        assert_eq!(from_keystroke("!", Some("!"), false, false), Some(Key::Char('!')));
        assert_eq!(from_keystroke("e", Some("é"), false, false), Some(Key::Char('é')));
    }

    #[test]
    fn altgr_characters_go_as_themselves_and_ctrl_alt_shortcuts_as_keys() {
        assert_eq!(from_keystroke("q", Some("@"), true, true), Some(Key::Char('@')));
        assert_eq!(from_keystroke("e", Some("€"), true, true), Some(Key::Char('€')));
        assert!(matches!(from_keystroke("a", Some("a"), true, true), Some(Key::Code(_))));
        assert!(matches!(from_keystroke("a", Some("\u{1}"), true, true), Some(Key::Code(_))));
        assert!(matches!(from_keystroke("t", None, true, true), Some(Key::Code(_))));
    }

    #[test]
    fn shortcuts_and_named_keys_go_as_keys() {
        let Some(Key::Code(ctrl_c)) = from_keystroke("c", None, true, false) else { panic!() };
        assert_eq!(ctrl_c.keysym, 'c' as u32);
        assert_eq!(from_keystroke("enter", Some("\r"), false, false), Some(Key::Code(code(0x1c, 0xff0d))));
        assert_eq!(from_keystroke("f12", None, false, false), Some(Key::Code(code(0x58, 0xffc9))));
        assert_eq!(from_keystroke("left", None, true, false), Some(Key::Code(code(0xe04b, 0xff51))));
        assert_eq!(from_keystroke("f20", None, false, false), None);
    }

    #[test]
    fn the_us_layout_covers_every_row() {
        assert_eq!(us_scancode('1'), Some(0x02));
        assert_eq!(us_scancode('q'), Some(0x10));
        assert_eq!(us_scancode('`'), Some(0x29));
        assert_eq!(us_scancode('/'), Some(0x35));
        assert_eq!(us_scancode('\\'), Some(0x2b));
    }

    #[test]
    fn keysyms_outside_latin_1_use_the_unicode_block() {
        assert_eq!(keysym_for_char('a'), 0x61);
        assert_eq!(keysym_for_char('ä'), 0xe4);
        assert_eq!(keysym_for_char('€'), 0x0100_20ac);
    }

    #[test]
    fn mac_option_types_symbols_and_keeps_letters_as_alt() {
        // German Mac: Option+L is @, Option+E is €, Option+7 is |.
        assert_eq!(mac_option_char("l", Some("@"), false, true), Some(Key::Char('@')));
        assert_eq!(mac_option_char("e", Some("\u{20ac}"), false, true), Some(Key::Char('\u{20ac}')));
        assert_eq!(mac_option_char("7", Some("|"), false, true), Some(Key::Char('|')));
        // US Mac: Option+F is ƒ, a letter, so it stays Alt+F for the remote menu.
        assert_eq!(mac_option_char("f", Some("\u{192}"), false, true), None);
        // Not Option alone, or nothing typed (a dead key): the usual translation.
        assert_eq!(mac_option_char("l", Some("@"), true, true), None);
        assert_eq!(mac_option_char("l", Some("l"), false, false), None);
        assert_eq!(mac_option_char("e", None, false, true), None);
    }
}
