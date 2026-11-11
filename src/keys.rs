//! Translate key presses into the bytes a terminal program expects.

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Mods {
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
}

/// `key` is gpui's key name ("a", "enter", "up", "f5", …), `key_char` the text the key
/// produces with the current layout, if any. `app_cursor` is DECCKM (application cursor keys).
pub fn to_bytes(key: &str, key_char: Option<&str>, mods: Mods, app_cursor: bool) -> Option<Vec<u8>> {
    let modifier_param = 1 + mods.shift as u8 + 2 * mods.alt as u8 + 4 * mods.ctrl as u8;
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

    let named = match key {
        "enter" => Some(if mods.alt { b"\x1b\r".to_vec() } else { b"\r".to_vec() }),
        "backspace" => Some(match (mods.ctrl, mods.alt) {
            (true, _) => vec![0x08],
            (_, true) => b"\x1b\x7f".to_vec(),
            _ => vec![0x7f],
        }),
        "tab" => Some(if mods.shift { b"\x1b[Z".to_vec() } else { b"\t".to_vec() }),
        "escape" => Some(vec![0x1b]),
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
        "space" if mods.ctrl => Some(vec![0]),
        _ => None,
    };
    if named.is_some() {
        return named;
    }

    if mods.ctrl && !mods.alt {
        let base = if key.chars().count() == 1 { key.chars().next() } else { None }?;
        let byte = match base.to_ascii_lowercase() {
            c @ 'a'..='z' => c as u8 - b'a' + 1,
            '@' | '2' => 0,
            '[' | '3' => 0x1b,
            '\\' | '4' => 0x1c,
            ']' | '5' => 0x1d,
            '^' | '6' => 0x1e,
            '_' | '-' | '7' => 0x1f,
            '8' => 0x7f,
            _ => return None,
        };
        return Some(vec![byte]);
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

#[cfg(test)]
mod tests {
    use super::*;

    const NONE: Mods = Mods { ctrl: false, alt: false, shift: false };
    const CTRL: Mods = Mods { ctrl: true, alt: false, shift: false };
    const ALT: Mods = Mods { ctrl: false, alt: true, shift: false };

    #[test]
    fn printable_text_uses_key_char() {
        assert_eq!(to_bytes("a", Some("a"), NONE, false), Some(b"a".to_vec()));
        assert_eq!(to_bytes("2", Some("\""), Mods { shift: true, ..NONE }, false), Some(b"\"".to_vec()));
        assert_eq!(to_bytes("e", Some("€"), Mods { ctrl: true, alt: true, shift: false }, false), Some("€".as_bytes().to_vec()));
    }

    #[test]
    fn control_keys() {
        assert_eq!(to_bytes("c", None, CTRL, false), Some(vec![3]));
        assert_eq!(to_bytes("d", None, CTRL, false), Some(vec![4]));
        assert_eq!(to_bytes("[", None, CTRL, false), Some(vec![0x1b]));
        assert_eq!(to_bytes("space", None, CTRL, false), Some(vec![0]));
    }

    #[test]
    fn alt_prefixes_escape() {
        assert_eq!(to_bytes("b", Some("b"), ALT, false), Some(b"\x1bb".to_vec()));
    }

    #[test]
    fn cursor_keys_respect_application_mode() {
        assert_eq!(to_bytes("up", None, NONE, false), Some(b"\x1b[A".to_vec()));
        assert_eq!(to_bytes("up", None, NONE, true), Some(b"\x1bOA".to_vec()));
        assert_eq!(to_bytes("right", None, CTRL, false), Some(b"\x1b[1;5C".to_vec()));
    }

    #[test]
    fn editing_and_function_keys() {
        assert_eq!(to_bytes("enter", None, NONE, false), Some(b"\r".to_vec()));
        assert_eq!(to_bytes("backspace", None, NONE, false), Some(vec![0x7f]));
        assert_eq!(to_bytes("delete", None, NONE, false), Some(b"\x1b[3~".to_vec()));
        assert_eq!(to_bytes("f1", None, NONE, false), Some(b"\x1bOP".to_vec()));
        assert_eq!(to_bytes("f5", None, NONE, false), Some(b"\x1b[15~".to_vec()));
        assert_eq!(to_bytes("tab", None, Mods { shift: true, ..NONE }, false), Some(b"\x1b[Z".to_vec()));
    }

    #[test]
    fn paste_is_bracketed_and_sanitised() {
        assert_eq!(paste_bytes("a\nb", false), b"a\rb".to_vec());
        assert_eq!(paste_bytes("x\x1b[201~y", true), b"\x1b[200~xy\x1b[201~".to_vec());
    }
}
