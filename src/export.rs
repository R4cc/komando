//! "Copy output as .txt file": write the Scrollback to a temporary file and put the *file*
//! on the clipboard, so Ctrl+V in Explorer, Outlook or Teams pastes it as an attachment. On
//! Linux and macOS the clipboard gets the file's path as text.

use std::path::PathBuf;

use anyhow::Result;

/// Windows' temp folder is the user's own. On Linux and macOS it is `/tmp`, shared by every
/// user, where someone else could create the folder first; the user's cache folder is theirs.
fn export_dir() -> PathBuf {
    if cfg!(windows) {
        std::env::temp_dir().join("Komando")
    } else {
        dirs::cache_dir().unwrap_or_else(std::env::temp_dir).join("Komando").join("exports")
    }
}

/// Remove exports older than a day. They hold terminal output, which can include secrets,
/// and only need to live long enough to be pasted somewhere.
pub fn clean_up() {
    let day = std::time::Duration::from_secs(24 * 60 * 60);
    crate::model::remove_older_than(&export_dir(), day, |path| path.extension().is_some_and(|ext| ext == "txt"));
}

/// Returns a short note for the confirmation toast. When the file can't be written the
/// clipboard is left alone, so it doesn't lose what the user had copied to a missing file.
pub fn write_and_copy_file(name: &str, text: &str, cx: &mut gpui::App) -> Result<String> {
    write_then_copy(&export_dir(), name, text, |path| copy_file_to_clipboard(path, cx))
}

fn write_then_copy(
    dir: &std::path::Path,
    name: &str,
    text: &str,
    copy: impl FnOnce(&std::path::Path) -> Result<String>,
) -> Result<String> {
    create_private_dir(dir)?;
    let path = dir.join(name);
    write_private(&path, &export_bytes(text))?;
    copy(&path)
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
/// again when the folder already exists, so one an earlier version made is closed too. Unlike
/// Komando's data folder, where that is best effort, a folder that can't be closed is an error:
/// no export is written where others could read it.
fn create_private_dir(dir: &std::path::Path) -> std::io::Result<()> {
    crate::model::create_private_dir(dir)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

fn write_private(path: &std::path::Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    let mut file = crate::model::private_file_options().write(true).create(true).truncate(true).open(path)?;
    // The private mode only applies to a new file; an export of the same name may be older.
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

    #[test]
    fn clipboard_is_left_alone_when_the_file_cannot_be_written() {
        use crate::model::new_id;
        // A file where the export folder should be: the folder can't be created.
        let blocker = std::env::temp_dir().join(format!("komando-export-{}", new_id()));
        std::fs::write(&blocker, b"").unwrap();
        let mut copied = false;
        let result = write_then_copy(&blocker.join("exports"), "output.txt", "hi", |_| {
            copied = true;
            Ok(String::new())
        });
        std::fs::remove_file(&blocker).unwrap();
        assert!(result.is_err());
        assert!(!copied);
    }

    #[test]
    fn written_file_is_what_gets_copied() {
        use crate::model::new_id;
        let dir = std::env::temp_dir().join(format!("komando-export-{}", new_id()));
        let mut copied = None;
        let note = write_then_copy(&dir, "output.txt", "hi", |path| {
            copied = Some(std::fs::read(path).unwrap());
            Ok(" note".into())
        })
        .unwrap();
        std::fs::remove_dir_all(&dir).unwrap();
        assert_eq!(note, " note");
        assert_eq!(copied, Some(export_bytes("hi")));
    }
}

#[cfg(windows)]
pub fn copy_file_to_clipboard(path: &std::path::Path, _cx: &mut gpui::App) -> Result<String> {
    let path = path.to_string_lossy().to_string();
    let _clipboard = clipboard_win::Clipboard::new_attempts(10)
        .map_err(|e| anyhow::anyhow!("clipboard busy: {e}"))?;
    clipboard_win::raw::empty().map_err(|e| anyhow::anyhow!("clipboard: {e}"))?;
    clipboard_win::raw::set_file_list(&[path.as_str()]).map_err(|e| anyhow::anyhow!("clipboard: {e}"))?;
    Ok(" to the clipboard.".into())
}

/// Putting a file on the clipboard is Windows-only so far; on Linux and macOS its path goes
/// there as text instead.
#[cfg(not(windows))]
pub fn copy_file_to_clipboard(path: &std::path::Path, cx: &mut gpui::App) -> Result<String> {
    cx.write_to_clipboard(gpui::ClipboardItem::new_string(path.display().to_string()));
    Ok(" (path copied).".into())
}
