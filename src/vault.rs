//! The Vault: every Credential, encrypted with a key derived from the Master Password.
//!
//! File format (`vault.json`): JSON with the Argon2id parameters, a random salt, a random
//! XChaCha20-Poly1305 nonce and the ciphertext of the JSON-encoded credentials. A fresh
//! nonce is drawn on every save. There is no recovery: losing the Master Password loses
//! the Credentials (Projects and Hosts are stored separately and survive).

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result, anyhow, bail};
use argon2::{Algorithm, Argon2, Params, Version};
use base64::{Engine as _, engine::general_purpose::STANDARD as B64};
use chacha20poly1305::aead::{Aead, KeyInit};
use chacha20poly1305::{XChaCha20Poly1305, XNonce};
use rand::RngCore;
use serde::{Deserialize, Serialize};
use zeroize::{Zeroize, ZeroizeOnDrop, Zeroizing};

use crate::model::{CredentialId, new_id, write_atomic};

#[derive(Clone, Serialize, Deserialize, Zeroize, ZeroizeOnDrop, PartialEq)]
pub struct Credential {
    pub label: String,
    pub secret: Secret,
}

impl std::fmt::Debug for Credential {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let kind = match self.secret {
            Secret::Password { .. } => "password",
            Secret::PrivateKey { .. } => "private key",
        };
        write!(f, "Credential({:?}, {kind})", self.label)
    }
}

#[derive(Clone, Serialize, Deserialize, Zeroize, ZeroizeOnDrop, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Secret {
    Password { password: String },
    /// OpenSSH or PuTTY `.ppk` private key text, with its passphrase if it has one.
    PrivateKey { key: String, passphrase: Option<String> },
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
struct KdfParams {
    memory_kib: u32,
    iterations: u32,
    parallelism: u32,
}

impl KdfParams {
    #[cfg(not(test))]
    const DEFAULT: Self = Self { memory_kib: 64 * 1024, iterations: 3, parallelism: 1 };
    // Tests use cheap parameters; the format and code path are the same.
    #[cfg(test)]
    const DEFAULT: Self = Self { memory_kib: 1024, iterations: 1, parallelism: 1 };
}

#[derive(Serialize, Deserialize)]
struct VaultFile {
    version: u32,
    kdf: KdfParams,
    salt: String,
    nonce: String,
    ciphertext: String,
}

#[derive(Default, Serialize, Deserialize)]
struct Contents {
    credentials: BTreeMap<CredentialId, Credential>,
}

/// An unlocked Vault. The derived key is wiped from memory when this is dropped (locking).
pub struct Vault {
    path: PathBuf,
    kdf: KdfParams,
    salt: [u8; 16],
    key: Zeroizing<[u8; 32]>,
    contents: Contents,
}

impl Vault {
    pub const FILE: &'static str = "vault.json";

    pub fn path_in(dir: &Path) -> PathBuf {
        dir.join(Self::FILE)
    }

    pub fn exists(dir: &Path) -> bool {
        Self::path_in(dir).exists()
    }

    /// First run: create an empty Vault protected by `master_password`.
    pub fn create(dir: &Path, master_password: &str) -> Result<Self> {
        if master_password.chars().count() < 8 {
            bail!("The master password needs at least 8 characters.");
        }
        // Another window may have created one since the dialog opened; never overwrite it.
        if Self::exists(dir) {
            bail!("A vault already exists. Unlock it with its master password instead.");
        }
        let mut salt = [0u8; 16];
        rand::thread_rng().fill_bytes(&mut salt);
        let kdf = KdfParams::DEFAULT;
        let key = derive_key(master_password, &salt, kdf)?;
        let vault = Self { path: Self::path_in(dir), kdf, salt, key, contents: Contents::default() };
        fs::create_dir_all(dir)?;
        vault.save()?;
        Ok(vault)
    }

    pub fn unlock(dir: &Path, master_password: &str) -> Result<Self> {
        let path = Self::path_in(dir);
        let text = fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
        let file: VaultFile = serde_json::from_str(&text).context("the vault file is damaged")?;
        if file.version != 1 {
            bail!("unsupported vault version {}", file.version);
        }
        let salt: [u8; 16] = B64
            .decode(&file.salt)?
            .try_into()
            .map_err(|_| anyhow!("the vault file is damaged (salt)"))?;
        let nonce = B64.decode(&file.nonce)?;
        if nonce.len() != 24 {
            bail!("the vault file is damaged (nonce)");
        }
        let ciphertext = B64.decode(&file.ciphertext)?;
        let key = derive_key(master_password, &salt, file.kdf)?;
        let cipher = XChaCha20Poly1305::new(key.as_ref().into());
        let plaintext = Zeroizing::new(
            cipher
                .decrypt(XNonce::from_slice(&nonce), ciphertext.as_ref())
                .map_err(|_| anyhow!("Wrong master password."))?,
        );
        let contents: Contents = serde_json::from_slice(&plaintext).context("the vault contents are damaged")?;
        Ok(Self { path, kdf: file.kdf, salt, key, contents })
    }

    fn save(&self) -> Result<()> {
        let mut nonce = [0u8; 24];
        rand::thread_rng().fill_bytes(&mut nonce);
        let plaintext = Zeroizing::new(serde_json::to_vec(&self.contents)?);
        let cipher = XChaCha20Poly1305::new(self.key.as_ref().into());
        let ciphertext = cipher
            .encrypt(XNonce::from_slice(&nonce), plaintext.as_ref())
            .map_err(|_| anyhow!("encryption failed"))?;
        let file = VaultFile {
            version: 1,
            kdf: self.kdf,
            salt: B64.encode(self.salt),
            nonce: B64.encode(nonce),
            ciphertext: B64.encode(ciphertext),
        };
        write_atomic(&self.path, serde_json::to_string_pretty(&file)?.as_bytes())
    }

    pub fn get(&self, id: &str) -> Option<&Credential> {
        self.contents.credentials.get(id)
    }

    pub fn list(&self) -> impl Iterator<Item = (&CredentialId, &Credential)> {
        self.contents.credentials.iter()
    }

    /// Store a new Credential and return its id.
    pub fn add(&mut self, credential: Credential) -> Result<CredentialId> {
        let id = new_id();
        self.contents.credentials.insert(id.clone(), credential);
        if let Err(err) = self.save() {
            self.contents.credentials.remove(&id);
            return Err(err);
        }
        Ok(id)
    }

    /// Replace the secret of an existing Credential (e.g. after a password change).
    /// Every Host that shares it picks up the new secret.
    pub fn replace(&mut self, id: &str, credential: Credential) -> Result<()> {
        let Some(previous) = self.contents.credentials.get_mut(id) else {
            bail!("unknown credential");
        };
        let previous = std::mem::replace(previous, credential);
        if let Err(err) = self.save() {
            self.contents.credentials.insert(id.to_string(), previous);
            return Err(err);
        }
        Ok(())
    }

    pub fn remove(&mut self, id: &str) -> Result<()> {
        let previous = self.contents.credentials.remove(id);
        if let Err(err) = self.save() {
            if let Some(previous) = previous {
                self.contents.credentials.insert(id.to_string(), previous);
            }
            return Err(err);
        }
        Ok(())
    }
}

fn derive_key(password: &str, salt: &[u8], kdf: KdfParams) -> Result<Zeroizing<[u8; 32]>> {
    let params = Params::new(kdf.memory_kib, kdf.iterations, kdf.parallelism, Some(32))
        .map_err(|e| anyhow!("invalid KDF parameters: {e}"))?;
    let mut key = Zeroizing::new([0u8; 32]);
    Argon2::new(Algorithm::Argon2id, Version::V0x13, params)
        .hash_password_into(password.as_bytes(), salt, key.as_mut())
        .map_err(|e| anyhow!("key derivation failed: {e}"))?;
    Ok(key)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir() -> PathBuf {
        let dir = std::env::temp_dir().join(format!("komando-vault-{}", new_id()));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn password(p: &str) -> Credential {
        Credential { label: "ops".into(), secret: Secret::Password { password: p.into() } }
    }

    #[test]
    fn roundtrip() {
        let dir = temp_dir();
        let mut v = Vault::create(&dir, "correct horse").unwrap();
        let id = v.add(password("hunter2")).unwrap();
        drop(v);
        let v = Vault::unlock(&dir, "correct horse").unwrap();
        assert_eq!(v.get(&id), Some(&password("hunter2")));
        fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn wrong_password_is_rejected() {
        let dir = temp_dir();
        Vault::create(&dir, "correct horse").unwrap();
        let err = Vault::unlock(&dir, "wrong horse!").err().unwrap();
        assert_eq!(err.to_string(), "Wrong master password.");
        fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn tampering_is_detected() {
        let dir = temp_dir();
        let mut v = Vault::create(&dir, "correct horse").unwrap();
        v.add(password("hunter2")).unwrap();
        let path = Vault::path_in(&dir);
        let mut file: VaultFile = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        let mut ct = B64.decode(&file.ciphertext).unwrap();
        ct[0] ^= 1;
        file.ciphertext = B64.encode(ct);
        fs::write(&path, serde_json::to_string(&file).unwrap()).unwrap();
        assert!(Vault::unlock(&dir, "correct horse").is_err());
        fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn secrets_are_not_stored_in_plain_text() {
        let dir = temp_dir();
        let mut v = Vault::create(&dir, "correct horse").unwrap();
        v.add(password("very-secret-value")).unwrap();
        let raw = fs::read_to_string(Vault::path_in(&dir)).unwrap();
        assert!(!raw.contains("very-secret-value"));
        fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn replacing_a_shared_credential_keeps_its_id() {
        let dir = temp_dir();
        let mut v = Vault::create(&dir, "correct horse").unwrap();
        let id = v.add(password("old")).unwrap();
        v.replace(&id, password("new")).unwrap();
        let v = Vault::unlock(&dir, "correct horse").unwrap();
        assert_eq!(v.get(&id), Some(&password("new")));
        fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn failed_add_does_not_leave_an_unsaved_credential() {
        let dir = temp_dir();
        let mut v = Vault::create(&dir, "correct horse").unwrap();
        crate::model::FAIL_WRITES.set(true);
        assert!(v.add(password("not saved")).is_err());
        assert_eq!(v.list().count(), 0);
        crate::model::FAIL_WRITES.set(false);
        v.add(password("saved")).unwrap();
        let reopened = Vault::unlock(&dir, "correct horse").unwrap();
        assert_eq!(reopened.list().count(), 1);
        fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn failed_replace_keeps_the_saved_credential() {
        let dir = temp_dir();
        let mut v = Vault::create(&dir, "correct horse").unwrap();
        let id = v.add(password("old")).unwrap();
        crate::model::FAIL_WRITES.set(true);
        assert!(v.replace(&id, password("new")).is_err());
        assert_eq!(v.get(&id), Some(&password("old")));
        crate::model::FAIL_WRITES.set(false);
        v.add(password("another")).unwrap();
        let reopened = Vault::unlock(&dir, "correct horse").unwrap();
        assert_eq!(reopened.get(&id), Some(&password("old")));
        fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn failed_remove_keeps_the_saved_credential() {
        let dir = temp_dir();
        let mut v = Vault::create(&dir, "correct horse").unwrap();
        let id = v.add(password("saved")).unwrap();
        crate::model::FAIL_WRITES.set(true);
        assert!(v.remove(&id).is_err());
        assert_eq!(v.get(&id), Some(&password("saved")));
        crate::model::FAIL_WRITES.set(false);
        v.add(password("another")).unwrap();
        let reopened = Vault::unlock(&dir, "correct horse").unwrap();
        assert_eq!(reopened.get(&id), Some(&password("saved")));
        fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn short_master_password_is_refused() {
        let dir = temp_dir();
        assert!(Vault::create(&dir, "short").is_err());
        fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn an_existing_vault_is_never_overwritten() {
        let dir = temp_dir();
        let mut v = Vault::create(&dir, "correct horse").unwrap();
        let id = v.add(password("hunter2")).unwrap();
        drop(v);
        assert!(Vault::create(&dir, "another horse").is_err());
        let v = Vault::unlock(&dir, "correct horse").unwrap();
        assert_eq!(v.get(&id), Some(&password("hunter2")));
        fs::remove_dir_all(dir).ok();
    }
}
