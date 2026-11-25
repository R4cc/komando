//! Trusted RDP certificates: the RDP counterpart of `known_hosts`.
//!
//! Most RDP servers present a self-signed certificate, so no certificate authority can vouch
//! for them. As with Host Keys, the first one seen is trusted once the user agrees, and every
//! later connection must present the same one. The check happens after the TLS handshake and
//! before any password is sent.
//!
//! One line per server: `address:port SHA256:<base64>`.

use std::fs;
use std::io;
use std::path::Path;
use std::sync::Mutex;

use base64::Engine as _;
use sha2::{Digest as _, Sha256};

pub const FILE: &str = "rdp_certificates";
// Several desktop workers can approve different servers at the same time.
static WRITER: Mutex<()> = Mutex::new(());

/// A certificate's fingerprint as shown to the user, in the form OpenSSH uses for keys.
pub fn fingerprint(der: &[u8]) -> String {
    let digest = Sha256::digest(der);
    format!("SHA256:{}", base64::engine::general_purpose::STANDARD_NO_PAD.encode(digest))
}

#[derive(Debug, PartialEq, Eq)]
pub enum Known {
    Trusted,
    /// Never connected to this server before.
    New,
    /// The server presented another certificate before: this one is not it.
    Changed {
        previous: String,
    },
}

/// Whether `server` presented `fingerprint` before. An error when the store exists but can't be
/// read: treating it as empty would show a changed certificate as a first connection.
pub fn check(path: &Path, server: &str, fingerprint: &str) -> io::Result<Known> {
    let lines = read_lines(path)?;
    Ok(match lines.iter().find_map(|line| entry(line).filter(|(name, _)| *name == server)) {
        Some((_, known)) if known == fingerprint => Known::Trusted,
        Some((_, known)) => Known::Changed { previous: known.to_string() },
        None => Known::New,
    })
}

/// Trust `fingerprint` for `server`, in place of any certificate it had before. Every other
/// line is kept byte for byte, also one that isn't valid text.
pub fn trust(path: &Path, server: &str, fingerprint: &str) -> io::Result<()> {
    let _writer = WRITER.lock().map_err(|_| io::Error::other("The certificate store is unavailable."))?;
    let mut lines: Vec<Vec<u8>> = read_lines(path)?
        .into_iter()
        .filter(|line| entry(line).is_none_or(|(name, _)| name != server))
        .collect();
    lines.push(format!("{server} {fingerprint}").into_bytes());
    if let Some(dir) = path.parent() {
        crate::model::create_private_dir(dir)?;
    }
    let mut bytes = lines.join(&b'\n');
    bytes.push(b'\n');
    crate::model::write_atomic(path, &bytes).map_err(io::Error::other)
}

/// The store's non-empty lines, as bytes; none when it doesn't exist yet.
fn read_lines(path: &Path) -> io::Result<Vec<Vec<u8>>> {
    let bytes = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(vec![]),
        Err(err) => return Err(err),
    };
    Ok(bytes
        .split(|b| *b == b'\n')
        .map(|line| line.strip_suffix(b"\r").unwrap_or(line).to_vec())
        .filter(|line| !line.iter().all(u8::is_ascii_whitespace))
        .collect())
}

/// A line's server and fingerprint, if it is valid text with both.
fn entry(line: &[u8]) -> Option<(&str, &str)> {
    let mut words = std::str::from_utf8(line).ok()?.split_whitespace();
    Some((words.next()?, words.next()?))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_server_is_new_then_trusted_then_changed() {
        let dir = std::env::temp_dir().join(format!("komando-rdp-trust-{}", crate::model::new_id()));
        let path = dir.join(FILE);
        assert_eq!(check(&path, "win01:3389", "SHA256:a").unwrap(), Known::New);
        trust(&path, "win01:3389", "SHA256:a").unwrap();
        trust(&path, "win02:3389", "SHA256:b").unwrap();
        assert_eq!(check(&path, "win01:3389", "SHA256:a").unwrap(), Known::Trusted);
        assert_eq!(check(&path, "win01:3389", "SHA256:c").unwrap(), Known::Changed { previous: "SHA256:a".into() });
        // Trusting the new one replaces the old.
        trust(&path, "win01:3389", "SHA256:c").unwrap();
        assert_eq!(check(&path, "win01:3389", "SHA256:c").unwrap(), Known::Trusted);
        assert_eq!(check(&path, "win02:3389", "SHA256:b").unwrap(), Known::Trusted);
        assert_eq!(fs::read_to_string(&path).unwrap().lines().count(), 2);
        let _ = fs::remove_dir_all(dir);
    }

    /// Regression: a store that can't be read as UTF-8 (one damaged byte, or a line written by
    /// another tool) must not make a trusted server look new, nor lose the other servers when
    /// one more is trusted.
    #[test]
    fn a_damaged_line_neither_hides_trusted_servers_nor_erases_them() {
        let dir = std::env::temp_dir().join(format!("komando-rdp-trust-{}", crate::model::new_id()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join(FILE);
        fs::write(&path, b"win01:3389 SHA256:a\nwin\xff02:3389 SHA256:b\n").unwrap();
        let known = check(&path, "win01:3389", "SHA256:evil").unwrap();
        trust(&path, "win03:3389", "SHA256:c").unwrap();
        let kept = check(&path, "win01:3389", "SHA256:a").unwrap();
        let damaged_kept = fs::read(&path).unwrap().windows(9).any(|w| w == b"win\xff02:33");
        // A store that exists but can't be read is an error, never an empty store.
        let unreadable = check(&dir, "win01:3389", "SHA256:a");
        let _ = fs::remove_dir_all(dir);
        assert!(damaged_kept, "the damaged line is left as it was");
        assert!(unreadable.is_err());
        assert_eq!(known, Known::Changed { previous: "SHA256:a".into() }, "a changed certificate must not look like a first connection");
        assert_eq!(kept, Known::Trusted, "trusting another server erased this one");
    }

    #[test]
    fn fingerprints_read_like_host_keys() {
        assert_eq!(fingerprint(b""), "SHA256:47DEQpj8HBSa+/TImW+5JCeuQeRkm5NMpJWZG3hSuFU");
    }
}
