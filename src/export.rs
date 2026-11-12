//! "Copy output as .txt file": write the Scrollback to a temporary file and put the *file*
//! on the clipboard, so Ctrl+V in Explorer, Outlook or Teams pastes it as an attachment.

use std::path::PathBuf;

use anyhow::Result;

pub fn export_dir() -> PathBuf {
    std::env::temp_dir().join("Komando")
}

/// Returns a short note for the confirmation toast.
pub fn write_and_copy_file(name: &str, text: &str) -> Result<String> {
    let dir = export_dir();
    std::fs::create_dir_all(&dir)?;
    let path = dir.join(name);
    // UTF-8 with BOM so Notepad and Outlook previews pick the right encoding.
    let mut bytes = Vec::with_capacity(text.len() + 3);
    bytes.extend_from_slice(b"\xEF\xBB\xBF");
    bytes.extend_from_slice(text.as_bytes());
    std::fs::write(&path, bytes)?;
    copy_file_to_clipboard(&path)
}

#[cfg(windows)]
pub fn copy_file_to_clipboard(path: &std::path::Path) -> Result<String> {
    let path = path.to_string_lossy().to_string();
    let _clipboard = clipboard_win::Clipboard::new_attempts(10)
        .map_err(|e| anyhow::anyhow!("clipboard busy: {e}"))?;
    clipboard_win::raw::empty().map_err(|e| anyhow::anyhow!("clipboard: {e}"))?;
    clipboard_win::raw::set_file_list(&[path.as_str()]).map_err(|e| anyhow::anyhow!("clipboard: {e}"))?;
    Ok(" to the clipboard. Paste it into Explorer, Outlook or Teams.".into())
}

#[cfg(not(windows))]
pub fn copy_file_to_clipboard(_path: &std::path::Path) -> Result<String> {
    // Komando ships for Windows; on dev hosts the caller puts the path on the clipboard.
    Ok(" (path copied).".into())
}
