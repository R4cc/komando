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

pub fn check(path: &Path, server: &str, fingerprint: &str) -> Known {
    let text = fs::read_to_string(path).unwrap_or_default();
    match entries(&text).find(|(name, _)| *name == server) {
        Some((_, known)) if known == fingerprint => Known::Trusted,
        Some((_, known)) => Known::Changed { previous: known.to_string() },
        None => Known::New,
    }
}

/// Trust `fingerprint` for `server`, in place of any certificate it had before.
pub fn trust(path: &Path, server: &str, fingerprint: &str) -> io::Result<()> {
    let _writer = WRITER.lock().map_err(|_| io::Error::other("The certificate store is unavailable."))?;
    let text = fs::read_to_string(path).unwrap_or_default();
    let mut lines: Vec<String> = text.lines().filter(|line| line.split_whitespace().next() != Some(server)).map(str::to_string).collect();
    lines.push(format!("{server} {fingerprint}"));
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    crate::model::write_atomic(path, format!("{}\n", lines.join("\n")).as_bytes()).map_err(io::Error::other)
}

fn entries(text: &str) -> impl Iterator<Item = (&str, &str)> {
    text.lines().filter_map(|line| {
        let mut words = line.split_whitespace();
        Some((words.next()?, words.next()?))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_server_is_new_then_trusted_then_changed() {
        let dir = std::env::temp_dir().join(format!("komando-rdp-trust-{}", crate::model::new_id()));
        let path = dir.join(FILE);
        assert_eq!(check(&path, "win01:3389", "SHA256:a"), Known::New);
        trust(&path, "win01:3389", "SHA256:a").unwrap();
        trust(&path, "win02:3389", "SHA256:b").unwrap();
        assert_eq!(check(&path, "win01:3389", "SHA256:a"), Known::Trusted);
        assert_eq!(check(&path, "win01:3389", "SHA256:c"), Known::Changed { previous: "SHA256:a".into() });
        // Trusting the new one replaces the old.
        trust(&path, "win01:3389", "SHA256:c").unwrap();
        assert_eq!(check(&path, "win01:3389", "SHA256:c"), Known::Trusted);
        assert_eq!(check(&path, "win02:3389", "SHA256:b"), Known::Trusted);
        assert_eq!(fs::read_to_string(&path).unwrap().lines().count(), 2);
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn fingerprints_read_like_host_keys() {
        assert_eq!(fingerprint(b""), "SHA256:47DEQpj8HBSa+/TImW+5JCeuQeRkm5NMpJWZG3hSuFU");
    }
}
