//! At a sudo prompt, offer the password the SSH Session signed in with: a hint appears after
//! the prompt, and pressing Enter types the password. Any other key dismisses the hint and goes
//! to the shell as usual.
//!
//! Only the Host's own sign-in password is offered (never a Jump Host's), only when the prompt
//! asks for the user Komando signed in as, and only after the user presses Enter: the server
//! already received this password when Komando signed in, so nothing new is given away unless
//! the user chooses to.

use std::sync::OnceLock;

use regex::Regex;
use zeroize::Zeroizing;

/// Erases the hint: the cursor sits where the hint begins, so clear to the end of the line.
pub(crate) const CLEAR: &[u8] = b"\x1b[K";

/// The hint, from widest to narrowest, with how many columns each takes. Drawn after saving
/// the cursor and followed by restoring it, so the cursor stays at the prompt.
const HINTS: [(&str, usize); 2] = [
    ("\x1b[0m \x1b[7;90m Komando \x1b[0m \x1b[90mPress Enter to use your sign-in password\x1b[0m", 51),
    ("\x1b[0m \x1b[90mEnter: sign-in password\x1b[0m", 24),
];

/// The most of the current line kept for matching; a sudo prompt is far shorter.
const TAIL_LIMIT: usize = 256;

/// sudo's password prompt in the languages it ships, with the user it asks for (sudo-rs does
/// not name one). The same list Tabby uses.
fn prompts() -> &'static [Regex] {
    static PROMPTS: OnceLock<Vec<Regex>> = OnceLock::new();
    PROMPTS.get_or_init(|| {
        [
            r"^\[sudo\] password for ([^:]+):\s*$",
            r"^\[sudo\] Passwort für ([^:]+):\s*$",
            r"^\[sudo\] Mot de passe de ([^:]+?)\s*:\s*$",
            r"^\[sudo\] [Cc]ontraseña para ([^:]+):\s*$",
            r"^\[sudo\] [Ss]enha para ([^:]+):\s*$",
            r"^\[sudo\] [Pp]assword di ([^:]+):\s*$",
            r"^\[sudo\] (\S+) 的密码[：:]\s*$",
            r"^\[sudo\] (\S+) 的密碼[：:]\s*$",
            r"^\[sudo\] (\S+) のパスワード[：:]\s*$",
            r"^\[sudo\] (\S+) 암호[：:]\s*$",
            r"^\[sudo\] пароль для ([^:]+):\s*$",
            r"^\[sudo\] hasło użytkownika ([^:]+):\s*$",
            r"^\[sudo\] (\S+) için parola:\s*$",
            r"^\[sudo\] [Hh]eslo pro ([^:]+):\s*$",
            r"^\[sudo\] lösenord för ([^:]+):\s*$",
            r"^\[sudo\] adgangskode for ([^:]+):\s*$",
            r"^\[sudo\] kata sandi untuk ([^:]+):\s*$",
            r"^\[sudo\] пароль до ([^:]+):\s*$",
            r"^\[sudo\] lozinka za ([^:]+):\s*$",
            // sudo-rs: "[sudo: authenticate] Password:"
            r"^\[sudo: authenticate\] .+?[：:]\s*$",
        ]
        .iter()
        .map(|p| Regex::new(p).expect("sudo prompt pattern"))
        .collect()
    })
}

/// Colour and cursor sequences, which a prompt may carry but the patterns don't expect.
fn escapes() -> &'static Regex {
    static ESCAPES: OnceLock<Regex> = OnceLock::new();
    ESCAPES.get_or_init(|| Regex::new(r"\x1b\[[0-?]*[ -/]*[@-~]").unwrap())
}

/// Watches an SSH Session's output for sudo prompts and its input for the answer.
pub(crate) struct SudoPassword {
    user: String,
    password: Zeroizing<String>,
    /// The end of the output's current line.
    tail: Vec<u8>,
    /// The hint is on screen: Enter types the password.
    offered: bool,
    /// The password was typed for the user and they have not pressed a key since. A sudo
    /// prompt now means it was wrong, so it is not offered again.
    just_used: bool,
}

/// What to do with a key the user pressed.
pub(crate) struct Typed {
    /// Erase the hint first.
    pub clear: bool,
    /// What to send to the shell.
    pub send: Zeroizing<Vec<u8>>,
}

impl SudoPassword {
    pub(crate) fn new(user: String, password: Zeroizing<String>) -> Self {
        Self { user, password, tail: Vec::new(), offered: false, just_used: false }
    }

    /// Output is about to be shown: take the hint away first if it is up. Returns whether it
    /// must be erased.
    pub(crate) fn output_arrives(&mut self) -> bool {
        std::mem::take(&mut self.offered)
    }

    /// Output was shown. Returns whether it ended in a sudo prompt for the signed-in user
    /// that the password should be offered for.
    pub(crate) fn output_shown(&mut self, data: &[u8]) -> bool {
        match data.iter().rposition(|&b| b == b'\n') {
            Some(newline) => {
                self.tail.clear();
                self.tail.extend_from_slice(&data[newline + 1..]);
            }
            None => self.tail.extend_from_slice(data),
        }
        if self.tail.len() > TAIL_LIMIT {
            self.tail.drain(..self.tail.len() - TAIL_LIMIT);
        }
        if self.just_used || !self.tail.windows(5).any(|w| w == b"[sudo") {
            return false;
        }
        let line = String::from_utf8_lossy(&self.tail);
        let line = escapes().replace_all(&line, "");
        // A line redrawn in place: only what follows the last carriage return is on screen.
        let line = line.rsplit('\r').next().unwrap_or_default();
        prompts().iter().find_map(|p| p.captures(line)).is_some_and(|found| match found.get(1) {
            Some(user) => user.as_str().trim() == self.user,
            None => true,
        })
    }

    /// The hint that fits in `room` columns, if one does; showing it offers the password.
    pub(crate) fn offer(&mut self, room: usize) -> Option<String> {
        let (hint, _) = HINTS.iter().find(|(_, width)| *width <= room)?;
        self.offered = true;
        Some(format!("\x1b7{hint}\x1b8"))
    }

    /// A key (or paste) from the user.
    pub(crate) fn typed(&mut self, bytes: Vec<u8>) -> Typed {
        // Focus reports are not keys: the hint stays.
        if bytes == b"\x1b[I" || bytes == b"\x1b[O" {
            return Typed { clear: false, send: Zeroizing::new(bytes) };
        }
        let clear = std::mem::take(&mut self.offered);
        self.just_used = false;
        if clear && bytes == b"\r" {
            self.just_used = true;
            let mut send = Zeroizing::new(Vec::with_capacity(self.password.len() + 1));
            send.extend_from_slice(self.password.as_bytes());
            send.push(b'\r');
            return Typed { clear, send };
        }
        Typed { clear, send: Zeroizing::new(bytes) }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sudo() -> SudoPassword {
        SudoPassword::new("ana".into(), Zeroizing::new("hunter2".into()))
    }

    #[test]
    fn recognises_sudo_prompts_for_the_signed_in_user() {
        for prompt in [
            "[sudo] password for ana: ",
            "[sudo] Passwort für ana: ",
            "[sudo] Mot de passe de ana : ",
            "[sudo] ana 的密码：",
            "[sudo: authenticate] Password: ",
        ] {
            assert!(sudo().output_shown(format!("$ sudo ls\r\n{prompt}").as_bytes()), "{prompt}");
        }
    }

    #[test]
    fn ignores_other_users_and_other_text() {
        assert!(!sudo().output_shown(b"[sudo] password for bob: "));
        assert!(!sudo().output_shown(b"Password: "));
        assert!(!sudo().output_shown(b"echo '[sudo] password for ana: '\r\nnext line"));
        // The prompt was already answered: the line moved on.
        assert!(!sudo().output_shown(b"[sudo] password for ana: \r\n"));
    }

    #[test]
    fn finds_a_prompt_split_across_chunks_and_with_colours() {
        let mut s = sudo();
        assert!(!s.output_shown(b"output\r\n[sudo] pass"));
        assert!(s.output_shown(b"word for ana: "));
        assert!(sudo().output_shown(b"\x1b[1m[sudo] password for ana:\x1b[0m "));
    }

    #[test]
    fn enter_types_the_password_once_offered() {
        let mut s = sudo();
        // Not offered yet: Enter is just Enter.
        let typed = s.typed(b"\r".to_vec());
        assert!(!typed.clear);
        assert_eq!(&*typed.send, b"\r");

        assert!(s.output_shown(b"[sudo] password for ana: "));
        assert!(s.offer(80).is_some());
        let typed = s.typed(b"\r".to_vec());
        assert!(typed.clear);
        assert_eq!(&*typed.send, b"hunter2\r");
        // The hint is gone; the next Enter is an Enter again.
        assert_eq!(&*s.typed(b"\r".to_vec()).send, b"\r");
    }

    #[test]
    fn another_key_dismisses_the_hint() {
        let mut s = sudo();
        assert!(s.output_shown(b"[sudo] password for ana: "));
        s.offer(80);
        assert!(!s.typed(b"\x1b[I".to_vec()).clear, "focus reports leave the hint");
        let typed = s.typed(b"x".to_vec());
        assert!(typed.clear);
        assert_eq!(&*typed.send, b"x");
        assert!(!s.typed(b"\r".to_vec()).clear);
    }

    #[test]
    fn output_takes_the_hint_away() {
        let mut s = sudo();
        assert!(s.output_shown(b"[sudo] password for ana: "));
        s.offer(80);
        assert!(s.output_arrives());
        assert!(!s.output_arrives());
        assert_eq!(&*s.typed(b"\r".to_vec()).send, b"\r");
    }

    #[test]
    fn a_rejected_password_is_not_offered_again() {
        let mut s = sudo();
        assert!(s.output_shown(b"[sudo] password for ana: "));
        s.offer(80);
        s.typed(b"\r".to_vec());
        assert!(!s.output_shown(b"\r\nSorry, try again.\r\n[sudo] password for ana: "));
        // Once the user types something, later prompts are offered again.
        s.typed(b"x".to_vec());
        assert!(s.output_shown(b"\r\n[sudo] password for ana: "));
    }

    #[test]
    fn hint_widths_are_right() {
        for (hint, width) in HINTS {
            assert_eq!(escapes().replace_all(hint, "").chars().count(), width, "{hint:?}");
        }
    }

    #[test]
    fn the_hint_fits_the_room_left_on_the_line() {
        let mut s = sudo();
        assert!(s.offer(80).unwrap().contains("Press Enter"));
        assert!(s.offer(30).unwrap().contains("Enter: sign-in password"));
        let mut s = sudo();
        assert!(s.offer(10).is_none());
        assert!(!s.output_arrives(), "nothing offered, nothing to erase");
    }
}
