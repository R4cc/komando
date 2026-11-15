//! Read registered WSL distributions without starting Linux or a console process.
use std::sync::{OnceLock, RwLock};

fn cache() -> &'static RwLock<Option<Vec<String>>> {
    static DISTROS: OnceLock<RwLock<Option<Vec<String>>>> = OnceLock::new();
    DISTROS.get_or_init(|| RwLock::new(None))
}

pub fn distributions() -> Vec<String> {
    if let Some(names) = cache().read().unwrap().as_ref() { return names.clone(); }
    cache().write().unwrap().get_or_insert_with(detect).clone()
}

pub fn refresh() {
    *cache().write().unwrap() = Some(detect());
}

#[cfg(not(windows))]
fn detect() -> Vec<String> { Vec::new() }

#[cfg(windows)]
fn detect() -> Vec<String> {
    use windows::Win32::System::Registry::*;
    use windows::core::{PCWSTR, PWSTR, w};
    let mut root = HKEY::default();
    let mut names = Vec::new();
    // This per-user registration covers both WSL 1 and WSL 2. Reading it cannot boot a distro.
    unsafe {
        if RegOpenKeyExW(HKEY_CURRENT_USER, w!("Software\\Microsoft\\Windows\\CurrentVersion\\Lxss"), None, KEY_READ, &mut root).is_err() {
            return names;
        }
        for index in 0..1024 {
            let mut key = [0u16; 256];
            let mut len = key.len() as u32;
            if RegEnumKeyExW(root, index, Some(PWSTR(key.as_mut_ptr())), &mut len, None, None, None, None).is_err() {
                break;
            }
            let mut value = [0u16; 4096];
            let mut bytes = std::mem::size_of_val(&value) as u32;
            if RegGetValueW(root, PCWSTR(key.as_ptr()), w!("DistributionName"), RRF_RT_REG_SZ, None, Some(value.as_mut_ptr().cast()), Some(&mut bytes)).is_ok() {
                let end = value.iter().position(|&c| c == 0).unwrap_or(value.len());
                let name = String::from_utf16_lossy(&value[..end]);
                let lower = name.to_ascii_lowercase();
                if !name.trim().is_empty() && !lower.starts_with("docker-desktop") && !lower.starts_with("rancher-desktop") {
                    names.push(name);
                }
            }
        }
        let _ = RegCloseKey(root);
    }
    names.sort();
    names.dedup();
    names
}
