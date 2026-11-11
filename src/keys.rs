//! Translate key presses, mouse events and focus changes into the bytes a terminal program
//! expects, following xterm's conventions (what `TERM=xterm-256color` promises).

use alacritty_terminal::term::TermMode;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Mods {
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
}

impl Mods {
    /// xterm's modifier parameter: 1 + shift + 2·alt + 4·ctrl.
    fn param(self) -> u8 {
        1 + self.shift as u8 + 2 * self.alt as u8 + 4 * self.ctrl as u8
    }
}

/// `key` is gpui's key name ("a", "enter", "up", "f5", …), `key_char` the text the key
/// produces with the current layout, if any. `app_cursor` is DECCKM (application cursor keys).
pub fn to_bytes(key: &str, key_char: Option<&str>, mods: Mods, app_cursor: bool) -> Option<Vec<u8>> {
    let modifier_param = mods.param();
    let csi = |final_byte: char| -> Vec<u8> {
        if modifier_param > 1 {
            format!("\x1b[1;{modifier_param}{final_byte}").into_bytes()
        } else if app_cursor {
            format!("\x1bO{final_byte}").into_bytes()
        } else {
            format!("\x1b[{final_byte}").into_bytes()
        }
    };
    let tilde = |code: u8| -> Vec<u8> {
        if modifier_param > 1 {
            format!("\x1b[{code};{modifier_param}~").into_bytes()
        } else {
            format!("\x1b[{code}~").into_bytes()
        }
    };
    // Alt sends ESC before the key's usual bytes (xterm's metaSendsEscape).
    let meta = |bytes: &[u8]| -> Vec<u8> {
        let mut out = Vec::with_capacity(bytes.len() + 1);
        if mods.alt {
            out.push(0x1b);
        }
        out.extend_from_slice(bytes);
        out
    };

    let named = match key {
        "enter" => Some(meta(b"\r")),
        "backspace" => Some(meta(if mods.ctrl { &[0x08] } else { &[0x7f] })),
        "tab" => Some(if mods.shift { b"\x1b[Z".to_vec() } else { meta(b"\t") }),
        "escape" => Some(meta(b"\x1b")),
        "up" => Some(csi('A')),
        "down" => Some(csi('B')),
        "right" => Some(csi('C')),
        "left" => Some(csi('D')),
        "home" => Some(csi('H')),
        "end" => Some(csi('F')),
        "insert" => Some(tilde(2)),
        "delete" => Some(tilde(3)),
        "pageup" => Some(tilde(5)),
        "pagedown" => Some(tilde(6)),
        "f1" => Some(ss3_or_csi('P', modifier_param)),
        "f2" => Some(ss3_or_csi('Q', modifier_param)),
        "f3" => Some(ss3_or_csi('R', modifier_param)),
        "f4" => Some(ss3_or_csi('S', modifier_param)),
        "f5" => Some(tilde(15)),
        "f6" => Some(tilde(17)),
        "f7" => Some(tilde(18)),
        "f8" => Some(tilde(19)),
        "f9" => Some(tilde(20)),
        "f10" => Some(tilde(21)),
        "f11" => Some(tilde(23)),
        "f12" => Some(tilde(24)),
        "space" if mods.ctrl => Some(meta(&[0])),
        _ => None,
    };
    if named.is_some() {
        return named;
    }

    if mods.ctrl {
        // Windows reports AltGr as ctrl+alt. When that produced a character of its own (`@`
        // on a German layout), type the character; otherwise ctrl+alt+x is ESC + ctrl+x.
        let altgr_text = mods.alt && key_char.is_some_and(|text| is_altgr_text(text, key));
        if !altgr_text {
            let byte = control_byte(key)?;
            return Some(meta(&[byte]));
        }
    }

    let text = match key_char {
        Some(text) if !text.is_empty() => text.to_string(),
        _ if key == "space" => " ".to_string(),
        _ if key.chars().count() == 1 => {
            if mods.shift { key.to_uppercase() } else { key.to_string() }
        }
        _ => return None,
    };
    let mut bytes = Vec::with_capacity(text.len() + 1);
    // Alt (without AltGr, which Windows reports as ctrl+alt) prefixes ESC.
    if mods.alt && !mods.ctrl {
        bytes.push(0x1b);
    }
    bytes.extend_from_slice(text.as_bytes());
    Some(bytes)
}

/// The C0 control byte ctrl+`key` sends, as in xterm.
fn control_byte(key: &str) -> Option<u8> {
    let mut chars = key.chars();
    let (Some(base), None) = (chars.next(), chars.next()) else { return None };
    Some(match base.to_ascii_lowercase() {
        c @ 'a'..='z' => c as u8 - b'a' + 1,
        '@' | '2' | '`' => 0,
        '[' | '3' => 0x1b,
        '\\' | '4' => 0x1c,
        ']' | '5' => 0x1d,
        '^' | '6' | '~' => 0x1e,
        '_' | '-' | '7' | '/' => 0x1f,
        '8' | '?' => 0x7f,
        _ => return None,
    })
}

/// Whether ctrl+alt+`key` produced real text (AltGr) rather than the key itself.
fn is_altgr_text(text: &str, key: &str) -> bool {
    !text.is_empty() && !text.chars().any(|c| c.is_control()) && !text.eq_ignore_ascii_case(key)
}

fn ss3_or_csi(final_byte: char, modifier_param: u8) -> Vec<u8> {
    if modifier_param > 1 {
        format!("\x1b[1;{modifier_param}{final_byte}").into_bytes()
    } else {
        format!("\x1bO{final_byte}").into_bytes()
    }
}

/// Wrap pasted text for bracketed-paste mode and normalise newlines to carriage returns.
pub fn paste_bytes(text: &str, bracketed: bool) -> Vec<u8> {
    let normalized = text.replace("\r\n", "\r").replace('\n', "\r");
    if bracketed {
        // Strip any embedded end marker so pasted text cannot break out of the bracket.
        let safe = normalized.replace("\x1b[201~", "");
        format!("\x1b[200~{safe}\x1b[201~").into_bytes()
    } else {
        normalized.into_bytes()
    }
}

/// Focus reports (DECSET 1004): vim, tmux and ConPTY ask for them.
pub fn focus_bytes(focused: bool) -> &'static [u8] {
    if focused { b"\x1b[I" } else { b"\x1b[O" }
}

/// The wheel in a full-screen program that did not ask for the mouse (alternate scroll,
/// DECSET 1007): one cursor key per line. `lines` > 0 scrolls up.
pub fn alternate_scroll_bytes(lines: i32, app_cursor: bool) -> Vec<u8> {
    let key: &[u8] = match (lines > 0, app_cursor) {
        (true, false) => b"\x1b[A",
        (true, true) => b"\x1bOA",
        (false, false) => b"\x1b[B",
        (false, true) => b"\x1bOB",
    };
    key.repeat(lines.unsigned_abs() as usize)
}

/// Mouse buttons as xterm numbers them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MouseButton {
    Left,
    Middle,
    Right,
    /// Motion with no button held (any-event tracking).
    None,
    WheelUp,
    WheelDown,
}

impl MouseButton {
    fn code(self) -> u8 {
        match self {
            MouseButton::Left => 0,
            MouseButton::Middle => 1,
            MouseButton::Right => 2,
            MouseButton::None => 3,
            MouseButton::WheelUp => 64,
            MouseButton::WheelDown => 65,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MouseAction {
    Press,
    Release,
    Motion,
}

/// What the program asked for with DECSET 1000/1002/1003 and 1005/1006.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct MouseMode {
    /// 1000: presses and releases (and the wheel).
    pub click: bool,
    /// 1002: also motion while a button is held.
    pub drag: bool,
    /// 1003: all motion.
    pub motion: bool,
    /// 1006: `CSI < b ; x ; y M/m`, no coordinate limit.
    pub sgr: bool,
    /// 1005: coordinates as UTF-8.
    pub utf8: bool,
}

impl MouseMode {
    /// The mouse mode a terminal is in.
    pub fn of(mode: TermMode) -> Self {
        Self {
            click: mode.contains(TermMode::MOUSE_REPORT_CLICK),
            drag: mode.contains(TermMode::MOUSE_DRAG),
            motion: mode.contains(TermMode::MOUSE_MOTION),
            sgr: mode.contains(TermMode::SGR_MOUSE),
            utf8: mode.contains(TermMode::UTF8_MOUSE),
        }
    }

    pub fn reporting(self) -> bool {
        self.click || self.drag || self.motion
    }

    /// Whether this kind of event is reported at all in this mode.
    pub fn wants(self, button: MouseButton, action: MouseAction) -> bool {
        match action {
            MouseAction::Press | MouseAction::Release => self.reporting(),
            MouseAction::Motion if button == MouseButton::None => self.motion,
            MouseAction::Motion => self.drag || self.motion,
        }
    }
}

/// Encode one mouse event at the zero-based cell (`col`, `row`). `None` when the mode does
/// not report it or the position cannot be encoded (beyond column 223 without SGR or UTF-8).
pub fn mouse_report(
    mode: MouseMode,
    button: MouseButton,
    action: MouseAction,
    col: usize,
    row: usize,
    mods: Mods,
) -> Option<Vec<u8>> {
    if !mode.wants(button, action) {
        return None;
    }
    let wheel = matches!(button, MouseButton::WheelUp | MouseButton::WheelDown);
    if wheel && action == MouseAction::Release {
        return None;
    }
    let mut code = button.code();
    if action == MouseAction::Motion {
        code += 32;
    }
    code += 4 * mods.shift as u8 + 8 * mods.alt as u8 + 16 * mods.ctrl as u8;

    if mode.sgr {
        let final_byte = if action == MouseAction::Release { 'm' } else { 'M' };
        return Some(format!("\x1b[<{code};{};{}{final_byte}", col + 1, row + 1).into_bytes());
    }

    // X10-style encoding: every value is offset by 32; releases do not say which button.
    if action == MouseAction::Release {
        code = (code & !0b11) | 3;
    }
    let limit = if mode.utf8 { 2015 } else { 223 };
    if col >= limit || row >= limit {
        return None;
    }
    let mut out = b"\x1b[M".to_vec();
    out.push(32 + code);
    for pos in [col, row] {
        let value = 32 + 1 + pos as u32;
        if mode.utf8 && value >= 0x80 {
            let mut buf = [0u8; 4];
            out.extend_from_slice(char::from_u32(value)?.encode_utf8(&mut buf).as_bytes());
        } else {
            out.push(value as u8);
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    const NONE: Mods = Mods { ctrl: false, alt: false, shift: false };
    const CTRL: Mods = Mods { ctrl: true, alt: false, shift: false };
    const ALT: Mods = Mods { ctrl: false, alt: true, shift: false };
    const CTRL_ALT: Mods = Mods { ctrl: true, alt: true, shift: false };

    #[test]
    fn printable_text_uses_key_char() {
        assert_eq!(to_bytes("a", Some("a"), NONE, false), Some(b"a".to_vec()));
        assert_eq!(to_bytes("2", Some("\""), Mods { shift: true, ..NONE }, false), Some(b"\"".to_vec()));
        assert_eq!(to_bytes("e", Some("€"), CTRL_ALT, false), Some("€".as_bytes().to_vec()));
        assert_eq!(to_bytes("q", Some("@"), CTRL_ALT, false), Some(b"@".to_vec()));
    }

    #[test]
    fn control_keys() {
        assert_eq!(to_bytes("c", None, CTRL, false), Some(vec![3]));
        assert_eq!(to_bytes("d", None, CTRL, false), Some(vec![4]));
        assert_eq!(to_bytes("[", None, CTRL, false), Some(vec![0x1b]));
        assert_eq!(to_bytes("space", None, CTRL, false), Some(vec![0]));
        assert_eq!(to_bytes("/", Some("/"), CTRL, false), Some(vec![0x1f]));
        assert_eq!(to_bytes(";", Some(";"), CTRL, false), None);
    }

    #[test]
    fn ctrl_alt_without_altgr_text_is_escape_plus_control() {
        assert_eq!(to_bytes("a", None, CTRL_ALT, false), Some(vec![0x1b, 1]));
        assert_eq!(to_bytes("a", Some("a"), CTRL_ALT, false), Some(vec![0x1b, 1]));
        assert_eq!(to_bytes("a", Some("\u{1}"), CTRL_ALT, false), Some(vec![0x1b, 1]));
    }

    #[test]
    fn alt_prefixes_escape() {
        assert_eq!(to_bytes("b", Some("b"), ALT, false), Some(b"\x1bb".to_vec()));
        assert_eq!(to_bytes("enter", None, ALT, false), Some(b"\x1b\r".to_vec()));
        assert_eq!(to_bytes("backspace", None, ALT, false), Some(b"\x1b\x7f".to_vec()));
        assert_eq!(to_bytes("escape", None, ALT, false), Some(b"\x1b\x1b".to_vec()));
    }

    #[test]
    fn cursor_keys_respect_application_mode() {
        assert_eq!(to_bytes("up", None, NONE, false), Some(b"\x1b[A".to_vec()));
        assert_eq!(to_bytes("up", None, NONE, true), Some(b"\x1bOA".to_vec()));
        assert_eq!(to_bytes("home", None, NONE, true), Some(b"\x1bOH".to_vec()));
        assert_eq!(to_bytes("right", None, CTRL, false), Some(b"\x1b[1;5C".to_vec()));
        // Modified keys always use the CSI form, even in application mode.
        assert_eq!(to_bytes("right", None, CTRL, true), Some(b"\x1b[1;5C".to_vec()));
        assert_eq!(to_bytes("up", None, Mods { shift: true, alt: true, ..NONE }, false), Some(b"\x1b[1;4A".to_vec()));
    }

    #[test]
    fn editing_and_function_keys() {
        assert_eq!(to_bytes("enter", None, NONE, false), Some(b"\r".to_vec()));
        assert_eq!(to_bytes("backspace", None, NONE, false), Some(vec![0x7f]));
        assert_eq!(to_bytes("backspace", None, CTRL, false), Some(vec![0x08]));
        assert_eq!(to_bytes("delete", None, NONE, false), Some(b"\x1b[3~".to_vec()));
        assert_eq!(to_bytes("delete", None, CTRL, false), Some(b"\x1b[3;5~".to_vec()));
        assert_eq!(to_bytes("f1", None, NONE, false), Some(b"\x1bOP".to_vec()));
        assert_eq!(to_bytes("f1", None, Mods { shift: true, ..NONE }, false), Some(b"\x1b[1;2P".to_vec()));
        assert_eq!(to_bytes("f5", None, NONE, false), Some(b"\x1b[15~".to_vec()));
        assert_eq!(to_bytes("f12", None, CTRL, false), Some(b"\x1b[24;5~".to_vec()));
        assert_eq!(to_bytes("tab", None, Mods { shift: true, ..NONE }, false), Some(b"\x1b[Z".to_vec()));
    }

    #[test]
    fn paste_is_bracketed_and_sanitised() {
        assert_eq!(paste_bytes("a\nb", false), b"a\rb".to_vec());
        assert_eq!(paste_bytes("x\x1b[201~y", true), b"\x1b[200~xy\x1b[201~".to_vec());
    }

    #[test]
    fn focus_and_alternate_scroll() {
        assert_eq!(focus_bytes(true), b"\x1b[I");
        assert_eq!(focus_bytes(false), b"\x1b[O");
        assert_eq!(alternate_scroll_bytes(2, false), b"\x1b[A\x1b[A".to_vec());
        assert_eq!(alternate_scroll_bytes(-1, true), b"\x1bOB".to_vec());
    }

    const CLICK: MouseMode = MouseMode { click: true, drag: false, motion: false, sgr: false, utf8: false };

    #[test]
    fn sgr_mouse_reports() {
        let sgr = MouseMode { sgr: true, ..CLICK };
        assert_eq!(
            mouse_report(sgr, MouseButton::Left, MouseAction::Press, 0, 0, NONE),
            Some(b"\x1b[<0;1;1M".to_vec())
        );
        assert_eq!(
            mouse_report(sgr, MouseButton::Right, MouseAction::Release, 299, 99, NONE),
            Some(b"\x1b[<2;300;100m".to_vec())
        );
        assert_eq!(
            mouse_report(sgr, MouseButton::WheelDown, MouseAction::Press, 4, 2, CTRL),
            Some(b"\x1b[<81;5;3M".to_vec())
        );
        assert_eq!(mouse_report(sgr, MouseButton::WheelUp, MouseAction::Release, 0, 0, NONE), None);
    }

    #[test]
    fn x10_mouse_reports() {
        assert_eq!(
            mouse_report(CLICK, MouseButton::Left, MouseAction::Press, 2, 3, NONE),
            Some(vec![0x1b, b'[', b'M', 32, 35, 36])
        );
        // Releases say "some button" (3).
        assert_eq!(
            mouse_report(CLICK, MouseButton::Middle, MouseAction::Release, 0, 0, Mods { shift: true, ..NONE }),
            Some(vec![0x1b, b'[', b'M', 32 + 3 + 4, 33, 33])
        );
        // Beyond column 223 the classic encoding cannot say where; UTF-8 can.
        assert_eq!(mouse_report(CLICK, MouseButton::Left, MouseAction::Press, 300, 0, NONE), None);
        let utf8 = MouseMode { utf8: true, ..CLICK };
        let mut want = vec![0x1b, b'[', b'M', 32];
        want.extend_from_slice("\u{14d}".as_bytes()); // 32 + 1 + 300
        want.push(33);
        assert_eq!(mouse_report(utf8, MouseButton::Left, MouseAction::Press, 300, 0, NONE), Some(want));
    }

    #[test]
    fn motion_is_reported_only_when_asked_for() {
        let drag = MouseMode { drag: true, sgr: true, ..Default::default() };
        let any = MouseMode { motion: true, sgr: true, ..Default::default() };
        assert_eq!(mouse_report(CLICK, MouseButton::Left, MouseAction::Motion, 1, 1, NONE), None);
        assert_eq!(
            mouse_report(drag, MouseButton::Left, MouseAction::Motion, 1, 1, NONE),
            Some(b"\x1b[<32;2;2M".to_vec())
        );
        assert_eq!(mouse_report(drag, MouseButton::None, MouseAction::Motion, 1, 1, NONE), None);
        assert_eq!(
            mouse_report(any, MouseButton::None, MouseAction::Motion, 1, 1, NONE),
            Some(b"\x1b[<35;2;2M".to_vec())
        );
    }
}
