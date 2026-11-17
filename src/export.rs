//! "Copy output as .txt file": write the Scrollback to a temporary file and put the *file*
//! on the clipboard, so Ctrl+V in Explorer, Outlook or Teams pastes it as an attachment.

use std::path::PathBuf;

use anyhow::Result;

/// Windows' temp folder is the user's own. On Linux and macOS it is `/tmp`, shared by every
/// user, where someone else could create the folder first; the user's cache folder is theirs.
pub fn export_dir() -> PathBuf {
    if cfg!(windows) {
        std::env::temp_dir().join("Komando")
    } else {
        dirs::cache_dir().unwrap_or_else(std::env::temp_dir).join("Komando").join("exports")
    }
}

/// Remove exports older than a day. They hold terminal output, which can include secrets,
/// and only need to live long enough to be pasted somewhere.
pub fn clean_up() {
    let Some(cutoff) = std::time::SystemTime::now().checked_sub(std::time::Duration::from_secs(24 * 60 * 60)) else {
        return;
    };
    let Ok(entries) = std::fs::read_dir(export_dir()) else { return };
    for entry in entries.flatten() {
        let stale = entry.metadata().is_ok_and(|meta| meta.is_file() && meta.modified().is_ok_and(|at| at < cutoff));
        if stale && entry.path().extension().is_some_and(|ext| ext == "txt") {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

/// Returns a short note for the confirmation toast.
pub fn write_and_copy_file(name: &str, text: &str) -> Result<String> {
    let dir = export_dir();
    create_private_dir(&dir)?;
    let path = dir.join(name);
    write_private(&path, &export_bytes(text))?;
    copy_file_to_clipboard(&path)
}

/// The file's bytes. Windows gets a UTF-8 byte order mark so Notepad and Outlook previews
/// pick the right encoding; other systems don't expect one and would show it as a stray
/// character.
fn export_bytes(text: &str) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(text.len() + 3);
    if cfg!(windows) {
        bytes.extend_from_slice(b"\xEF\xBB\xBF");
    }
    bytes.extend_from_slice(text.as_bytes());
    bytes
}

/// Only the user may read the export folder: the output can hold secrets. The mode is set
/// again when the folder already exists, so one an earlier version made is closed too.
fn create_private_dir(dir: &std::path::Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
        std::fs::DirBuilder::new().recursive(true).mode(0o700).create(dir)?;
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))
    }
    #[cfg(not(unix))]
    {
        std::fs::create_dir_all(dir)
    }
}

fn write_private(path: &std::path::Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(path)?;
    // The mode above only applies to a new file; an export of the same name may be older.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
    }
    file.write_all(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn byte_order_mark_only_on_windows() {
        let bytes = export_bytes("hi");
        assert_eq!(bytes.starts_with(b"\xEF\xBB\xBF"), cfg!(windows));
        assert!(bytes.ends_with(b"hi"));
    }

    #[cfg(unix)]
    #[test]
    fn exports_are_readable_by_their_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        use crate::model::new_id;
        let dir = std::env::temp_dir().join(format!("komando-export-{}", new_id()));
        create_private_dir(&dir).unwrap();
        let path = dir.join("output.txt");
        write_private(&path, b"secret").unwrap();
        assert_eq!(std::fs::metadata(&dir).unwrap().permissions().mode() & 0o777, 0o700);
        assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
        std::fs::remove_dir_all(dir).unwrap();
    }
}

#[cfg(windows)]
pub fn copy_file_to_clipboard(path: &std::path::Path) -> Result<String> {
    let path = path.to_string_lossy().to_string();
    let _clipboard = clipboard_win::Clipboard::new_attempts(10)
        .map_err(|e| anyhow::anyhow!("clipboard busy: {e}"))?;
    clipboard_win::raw::empty().map_err(|e| anyhow::anyhow!("clipboard: {e}"))?;
    clipboard_win::raw::set_file_list(&[path.as_str()]).map_err(|e| anyhow::anyhow!("clipboard: {e}"))?;
    Ok(" to the clipboard.".into())
}

#[cfg(not(windows))]
pub fn copy_file_to_clipboard(_path: &std::path::Path) -> Result<String> {
    // Putting a file on the clipboard is Windows-only so far; on Linux and macOS the caller
    // puts its path there instead.
    Ok(" (path copied).".into())
}
