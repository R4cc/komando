//! Waiting for text in an SSH Session's output, for a Startup Command that waits before it is
//! typed ("wait for `# `, then type `cd /srv`").
//!
//! The output is watched as it arrives, before it reaches the grid, so redraws, scrolling and
//! a full Scrollback can't hide or repeat it. Escape sequences (colours, cursor movement,
//! titles) are skipped, so only the text a program prints is matched.

use std::sync::atomic::{AtomicU64, Ordering};

/// Tells waits apart, so the UI ignores a match reported for a wait it has given up on.
static NEXT_ID: AtomicU64 = AtomicU64::new(1);

/// Where an escape sequence is, so its bytes are not taken for text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Escape {
    None,
    /// After ESC.
    Start,
    /// ESC followed by an intermediate byte, such as `ESC ( B`: one more byte ends it.
    Intermediate,
    /// CSI (`ESC [`): ends with a byte from `@` to `~`.
    Csi,
    /// OSC, DCS, APC and the like: end with BEL or ST (`ESC \`).
    Text,
    /// ESC inside one of those: `\` ends it.
    TextEsc,
}

#[derive(Debug)]
pub struct WaitFor {
    id: u64,
    pattern: Vec<u8>,
    /// Skip output until the first line break: the shell echoing the command just typed,
    /// which may itself contain the text.
    skip_echo: bool,
    escape: Escape,
    /// The last text seen, as long as the pattern.
    recent: Vec<u8>,
}

impl WaitFor {
    /// Wait for `text`. `after_echo`: a command was just typed, so its echoed line is skipped.
    pub fn new(text: &str, after_echo: bool) -> Self {
        Self {
            id: NEXT_ID.fetch_add(1, Ordering::Relaxed),
            pattern: text.as_bytes().to_vec(),
            skip_echo: after_echo,
            escape: Escape::None,
            recent: Vec::with_capacity(text.len()),
        }
    }

    pub fn id(&self) -> u64 {
        self.id
    }

    /// Watch more output. True once the text has appeared.
    pub fn feed(&mut self, bytes: &[u8]) -> bool {
        if self.pattern.is_empty() {
            return true;
        }
        for &byte in bytes {
            let text = match self.escape {
                Escape::None => match byte {
                    0x1b => {
                        self.escape = Escape::Start;
                        None
                    }
                    b'\n' => {
                        self.skip_echo = false;
                        Some(b'\n')
                    }
                    // Carriage returns, bells, backspaces: not text.
                    0..0x20 | 0x7f => None,
                    _ => Some(byte),
                },
                Escape::Start => {
                    self.escape = match byte {
                        b'[' => Escape::Csi,
                        b']' | b'P' | b'X' | b'^' | b'_' => Escape::Text,
                        0x20..=0x2f => Escape::Intermediate,
                        _ => Escape::None,
                    };
                    None
                }
                Escape::Intermediate => {
                    self.escape = Escape::None;
                    None
                }
                Escape::Csi => {
                    if (0x40..=0x7e).contains(&byte) {
                        self.escape = Escape::None;
                    }
                    None
                }
                Escape::Text => {
                    match byte {
                        0x07 => self.escape = Escape::None,
                        0x1b => self.escape = Escape::TextEsc,
                        _ => {}
                    }
                    None
                }
                Escape::TextEsc => {
                    self.escape = if byte == b'\\' { Escape::None } else { Escape::Text };
                    None
                }
            };
            let Some(text) = text else { continue };
            if self.skip_echo {
                continue;
            }
            if self.recent.len() == self.pattern.len() {
                self.recent.remove(0);
            }
            self.recent.push(text);
            if self.recent == self.pattern {
                return true;
            }
        }
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn appears(text: &str, after_echo: bool, chunks: &[&[u8]]) -> bool {
        let mut wait = WaitFor::new(text, after_echo);
        chunks.iter().any(|chunk| wait.feed(chunk))
    }

    #[test]
    fn text_is_found_across_chunks_and_colours() {
        assert!(appears("root@web01:~# ", false, &[b"Welcome\r\n\x1b[01;31mroot\x1b[0m@web01:~", b"# "]));
        assert!(appears("ready", false, &[b"re", b"a", b"dy"]));
        assert!(!appears("ready", false, &[b"read", b"\r\ny"]), "a line break is not skipped");
        assert!(!appears("Ready", false, &[b"ready"]), "the case matters, as in Tabby");
    }

    #[test]
    fn escape_sequences_are_not_text() {
        // A window title naming the text, cursor moves and a charset switch around it.
        assert!(!appears("done", false, &[b"\x1b]0;done\x07\x1b[2J\x1b(B"]));
        assert!(!appears("done", false, &[b"\x1b]2;done\x1b\\"]));
        assert!(appears("done", false, &[b"\x1b]0;title\x07\x1b[1;1Hdo\x1b[Kne"]));
    }

    #[test]
    fn the_typed_commands_echo_is_skipped() {
        // `sudo -i` waits for "sudo": the echo must not count, the prompt after it does.
        assert!(!appears("sudo", true, &[b"sudo -i\r"]));
        assert!(appears("sudo", true, &[b"sudo -i\r\n", b"[sudo] password for ana: "]));
        assert!(appears("sudo", false, &[b"sudo -i\r"]), "nothing typed yet: everything counts");
    }

    #[test]
    fn each_wait_has_its_own_id() {
        assert_ne!(WaitFor::new("a", false).id(), WaitFor::new("a", false).id());
    }
}
