//! A timed unlock may survive a restart on Windows. Only the derived key is kept,
//! protected by current-user DPAPI; the encrypted Vault remains the source of truth.
use super::*;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const FILE: &str = "vault-unlock.bin";
const MAX_MINUTES: u32 = 7 * 24 * 60;

#[derive(Serialize, Deserialize, Zeroize, ZeroizeOnDrop)]
struct Remembered {
    version: u32,
    key: [u8; 32],
    salt: [u8; 16],
    activity: u64,
    minutes: u32,
}

impl Vault {
    /// Clear restart unlock access, including when the Vault is already locked.
    pub fn forget_unlock(dir: &Path) -> Result<()> {
        crate::model::ignore_not_found(fs::remove_file(dir.join(FILE))).context("Could not clear the remembered vault unlock")
    }

    /// Save the original last-use time, never the time this function was called.
    pub fn remember_unlock(&self, minutes: u32, activity: SystemTime) -> Result<()> {
        let dir = self.path.parent().context("Vault has no directory")?;
        if !cfg!(windows) || minutes == 0 || minutes > MAX_MINUTES {
            return Self::forget_unlock(dir);
        }
        let remembered = Remembered {
            version: 1,
            key: *self.key,
            salt: self.salt,
            activity: activity.duration_since(UNIX_EPOCH)?.as_secs(),
            minutes,
        };
        let plaintext = Zeroizing::new(serde_json::to_vec(&remembered)?);
        let protected = protect(&plaintext)?;
        write_atomic(&dir.join(FILE), &protected)
    }

    /// Restore only an unexpired unlock for this Vault and this exact timeout.
    /// Bad, expired or disabled caches are removed; callers stay locked on any error.
    pub fn restore_unlock(dir: &Path, minutes: u32, now: SystemTime) -> Result<Option<(Self, SystemTime)>> {
        if !cfg!(windows) || minutes == 0 || minutes > MAX_MINUTES {
            Self::forget_unlock(dir)?;
            return Ok(None);
        }
        let path = dir.join(FILE);
        if !path.exists() {
            return Ok(None);
        }
        let restored = (|| -> Result<Option<(Self, SystemTime)>> {
            anyhow::ensure!(fs::metadata(&path)?.len() <= 64 * 1024, "Remembered unlock is too large");
            let plaintext = unprotect(&fs::read(&path)?)?;
            let remembered: Remembered = serde_json::from_slice(&plaintext)?;
            let activity = UNIX_EPOCH.checked_add(Duration::from_secs(remembered.activity)).context("Invalid unlock time")?;
            let elapsed = now.duration_since(activity).ok();
            if remembered.version != 1 || remembered.minutes != minutes
                || !elapsed.is_some_and(|d| d < Duration::from_secs(u64::from(minutes) * 60))
            {
                return Ok(None);
            }
            let vault_path = Self::path_in(dir);
            let file: VaultFile = serde_json::from_slice(&fs::read(&vault_path)?)?;
            anyhow::ensure!(file.version == 1 && B64.decode(&file.salt)? == remembered.salt, "Remembered unlock belongs to another vault");
            let vault = Self::decrypt(vault_path, file, remembered.salt, Zeroizing::new(remembered.key))?;
            Ok(Some((vault, activity)))
        })();
        if !matches!(restored, Ok(Some(_))) {
            Self::forget_unlock(dir)?;
        }
        restored
    }
}

#[cfg(windows)]
fn dpapi(bytes: &[u8], encrypt: bool) -> Result<Zeroizing<Vec<u8>>> {
    use windows::Win32::Foundation::{HLOCAL, LocalFree};
    use windows::Win32::Security::Cryptography::{
        CRYPT_INTEGER_BLOB, CRYPTPROTECT_UI_FORBIDDEN, CryptProtectData, CryptUnprotectData,
    };
    use windows::core::PCWSTR;
    let input = CRYPT_INTEGER_BLOB { cbData: bytes.len().try_into()?, pbData: bytes.as_ptr() as *mut u8 };
    let mut output = CRYPT_INTEGER_BLOB::default();
    // No LOCAL_MACHINE flag: only the Windows account that saved it can restore it.
    // DPAPI allocates the output; copy into zeroizing memory and wipe before LocalFree.
    unsafe {
        if encrypt {
            CryptProtectData(&input, PCWSTR::null(), None, None, None, CRYPTPROTECT_UI_FORBIDDEN, &mut output)?;
        } else {
            CryptUnprotectData(&input, None, None, None, None, CRYPTPROTECT_UI_FORBIDDEN, &mut output)?;
        }
        let result = if output.cbData == 0 {
            Zeroizing::new(Vec::new())
        } else {
            anyhow::ensure!(!output.pbData.is_null(), "DPAPI returned no data");
            let data = std::slice::from_raw_parts_mut(output.pbData, output.cbData as usize);
            let result = Zeroizing::new(data.to_vec());
            data.zeroize();
            result
        };
        let _ = LocalFree(Some(HLOCAL(output.pbData.cast())));
        Ok(result)
    }
}

fn protect(bytes: &[u8]) -> Result<Zeroizing<Vec<u8>>> {
    #[cfg(windows)]
    { dpapi(bytes, true) }
    #[cfg(not(windows))]
    { let _ = bytes; bail!("Remembered unlocks require Windows") }
}

fn unprotect(bytes: &[u8]) -> Result<Zeroizing<Vec<u8>>> {
    #[cfg(windows)]
    { dpapi(bytes, false) }
    #[cfg(not(windows))]
    { let _ = bytes; bail!("Remembered unlocks require Windows") }
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;
    use crate::vault::tests::{password, temp_dir};

    fn at(seconds: u64) -> SystemTime { UNIX_EPOCH + Duration::from_secs(seconds) }

    #[test]
    fn timed_unlock_survives_restart_without_resetting_inactivity() {
        let dir = temp_dir();
        let mut vault = Vault::create(&dir, "correct horse").unwrap();
        let id = vault.add(password("hunter2")).unwrap();
        vault.remember_unlock(MAX_MINUTES, at(100)).unwrap();
        let bytes = fs::read(dir.join(FILE)).unwrap();
        assert!(!bytes.windows(32).any(|b| b == vault.key.as_ref()));
        assert!(!String::from_utf8_lossy(&bytes).contains("correct horse"));
        drop(vault);
        let (mut vault, activity) = Vault::restore_unlock(&dir, MAX_MINUTES, at(200)).unwrap().unwrap();
        assert_eq!(activity, at(100));
        assert_eq!(vault.get(&id), Some(&password("hunter2")));
        vault.replace(&id, password("updated secret")).unwrap();
        drop(vault);
        let (vault, activity) = Vault::restore_unlock(&dir, MAX_MINUTES, at(300)).unwrap().unwrap();
        assert_eq!(vault.get(&id), Some(&password("updated secret")), "restore must read current credentials");
        assert_eq!(activity, at(100), "restarts must not extend the deadline");
        assert!(Vault::restore_unlock(&dir, MAX_MINUTES, at(100 + u64::from(MAX_MINUTES) * 60)).unwrap().is_none());
        assert!(!dir.join(FILE).exists());
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn lock_now_and_until_restart_remove_saved_access() {
        let dir = temp_dir();
        let vault = Vault::create(&dir, "correct horse").unwrap();
        vault.remember_unlock(60, at(100)).unwrap();
        Vault::forget_unlock(&dir).unwrap();
        assert!(Vault::restore_unlock(&dir, 60, at(101)).unwrap().is_none());
        vault.remember_unlock(60, at(100)).unwrap();
        assert!(Vault::restore_unlock(&dir, 0, at(101)).unwrap().is_none());
        assert!(!dir.join(FILE).exists());
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn changed_timeout_clock_rollback_and_corruption_stay_locked() {
        let dir = temp_dir();
        let vault = Vault::create(&dir, "correct horse").unwrap();
        for (minutes, now) in [(15, at(101)), (60, at(99))] {
            vault.remember_unlock(60, at(100)).unwrap();
            assert!(Vault::restore_unlock(&dir, minutes, now).unwrap().is_none());
        }
        for blob in [b"broken DPAPI blob".to_vec(), protect(b"").unwrap().to_vec()] {
            fs::write(dir.join(FILE), blob).unwrap();
            assert!(Vault::restore_unlock(&dir, 60, at(101)).is_err());
            assert!(!dir.join(FILE).exists());
        }
        assert!(Vault::unlock(&dir, "correct horse").is_ok());
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn saved_unlock_cannot_open_a_replacement_vault_or_tampered_contents() {
        let dir = temp_dir();
        let vault = Vault::create(&dir, "correct horse").unwrap();
        vault.remember_unlock(60, at(100)).unwrap();
        let mut file: VaultFile = serde_json::from_slice(&fs::read(Vault::path_in(&dir)).unwrap()).unwrap();
        file.ciphertext = B64.encode(b"tampered");
        fs::write(Vault::path_in(&dir), serde_json::to_vec(&file).unwrap()).unwrap();
        assert!(Vault::restore_unlock(&dir, 60, at(101)).is_err());
        vault.remember_unlock(60, at(100)).unwrap();
        fs::remove_file(Vault::path_in(&dir)).unwrap();
        Vault::create(&dir, "different password").unwrap();
        assert!(Vault::restore_unlock(&dir, 60, at(101)).is_err());
        assert!(!dir.join(FILE).exists());
        fs::remove_dir_all(dir).unwrap();
    }
}
