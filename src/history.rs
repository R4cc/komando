//! Command History: the commands entered in a Host's SSH Sessions, with when and on which
//! address, kept on this PC for each Host.
//!
//! The shell's own `history` is shared by everyone who signs in as that user, so it no longer
//! tells you what *you* did once someone else has worked on the server. This is Komando's own
//! record, filled from the Command Marks (lines read off the screen after Enter), so it only
//! holds what the terminal showed: a password typed with echo off never gets in, and nothing is
//! ever run or read on the host for it.
//!
//! One JSON-lines file per Host, named after the Host's id, so renaming the Host or changing its
//! address keeps its history. Each entry also records the address it was entered on: after an
//! address change, older commands are still shown under the address they actually ran on.

use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result};
use chrono::{DateTime, FixedOffset, Local};
use serde::{Deserialize, Serialize};

/// Entries kept per Host; the oldest go first.
pub const MAX_ENTRIES: usize = 10_000;
/// A file this large is trimmed on the next append.
const TRIM_AT_BYTES: u64 = 2 * 1024 * 1024;
/// What a trim leaves at most, well under `TRIM_AT_BYTES`: an entry is its line plus about 70
/// bytes, so `MAX_ENTRIES` of lines longer than about 140 bytes (a prompt with a long path and
/// a git branch) don't fit in `TRIM_AT_BYTES`. Trimmed by count alone, such a file stayed over
/// the limit and was rewritten on every Enter; this way the next trim is a few thousand
/// commands away.
const KEEP_BYTES: usize = 3 * 512 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Entry {
    /// When the line was entered, with the PC's UTC offset at the time.
    #[serde(with = "rfc3339")]
    pub at: DateTime<FixedOffset>,
    /// The Host's address (with `:port` when not 22) the Session was connected to.
    pub on: String,
    /// The line as shown on screen, prompt included: the prompt usually tells the user and
    /// directory it ran as.
    pub line: String,
}

impl Entry {
    pub fn new(at: DateTime<Local>, on: impl Into<String>, line: impl Into<String>) -> Self {
        Self { at: at.fixed_offset(), on: on.into(), line: line.into() }
    }

    /// The line split into prompt and command, for display.
    pub fn parts(&self) -> (&str, &str) {
        crate::marks::split_prompt(&self.line)
    }
}

/// How a Host's address is written in its history: `address`, or `address:port` off port 22.
pub fn address_label(address: &str, port: u16) -> String {
    if port == 22 { address.to_string() } else { format!("{address}:{port}") }
}

/// Whether an entered line is left out of the history: a command typed after an extra space,
/// as bash and zsh leave it out of theirs (`HISTCONTROL=ignorespace`), to keep a secret on a
/// command line out of it.
pub fn is_ignored(line: &str) -> bool {
    let (prompt, _) = crate::marks::split_prompt(line);
    if prompt.is_empty() {
        return false;
    }
    let Some(start) = line.find(prompt) else { return false };
    line[start + prompt.len()..].starts_with("  ")
}

pub fn dir(data_dir: &Path) -> PathBuf {
    data_dir.join("history")
}

/// A Host's history file. Host ids are short hex strings; anything else (an id from an import)
/// is hashed, so an id can never name a path outside the folder.
fn file(data_dir: &Path, host_id: &str) -> PathBuf {
    let plain = !host_id.is_empty() && host_id.len() <= 64 && host_id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
    let name = if plain {
        host_id.to_string()
    } else {
        use sha2::Digest as _;
        let hash = sha2::Sha256::digest(host_id.as_bytes());
        hash.iter().take(12).map(|b| format!("{b:02x}")).collect()
    };
    dir(data_dir).join(format!("{name}.jsonl"))
}

/// A Host's history, oldest first. Lines that can't be read (a crash mid-write) are skipped.
pub fn load(data_dir: &Path, host_id: &str) -> Vec<Entry> {
    newest(read(&file(data_dir, host_id)).unwrap_or_default())
}

/// The newest `MAX_ENTRIES` of `entries`.
fn newest(mut entries: Vec<Entry>) -> Vec<Entry> {
    if entries.len() > MAX_ENTRIES {
        entries.drain(..entries.len() - MAX_ENTRIES);
    }
    entries
}

#[cfg(test)]
thread_local! {
    /// Makes `read` fail on this thread, so tests can check what a failed read leaves.
    static FAIL_READS: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Every entry in a history file, oldest first. Read as bytes and line by line, so a line cut
/// in the middle of a character costs only that line, not the whole history.
fn read(path: &Path) -> std::io::Result<Vec<Entry>> {
    #[cfg(test)]
    if FAIL_READS.with(std::cell::Cell::get) {
        return Err(std::io::Error::other("reading failed for a test"));
    }
    let bytes = fs::read(path)?;
    let entries = bytes
        .split(|&b| b == b'\n')
        .filter_map(|line| serde_json::from_slice(line.strip_suffix(b"\r").unwrap_or(line)).ok())
        .collect();
    Ok(entries)
}

/// Add an entry to a Host's history.
pub fn append(data_dir: &Path, host_id: &str, entry: &Entry) -> Result<()> {
    let path = file(data_dir, host_id);
    crate::model::create_private_dir(&dir(data_dir))?;
    let mut line = serde_json::to_string(entry)?;
    line.push('\n');
    // A write cut short (a crash) leaves a line without its end: start on a line of our own.
    if !ends_with_newline(&path) {
        line.insert(0, '\n');
    }
    let mut out = crate::model::open_private_append(&path).with_context(|| format!("opening {}", path.display()))?;
    out.write_all(line.as_bytes()).with_context(|| format!("writing {}", path.display()))?;
    drop(out);
    if fs::metadata(&path).is_ok_and(|m| m.len() > TRIM_AT_BYTES) {
        // Trimmed only from what was read: a file that can't be read now is left as it is,
        // never replaced by an empty one.
        let Ok(entries) = read(&path) else { return Ok(()) };
        crate::model::write_atomic(&path, trimmed(&entries)?.as_bytes())?;
    }
    Ok(())
}

/// The file's text after a trim: the newest entries, at most `MAX_ENTRIES` of them and at most
/// `KEEP_BYTES`. The newest one is kept whatever its size.
fn trimmed(entries: &[Entry]) -> Result<String> {
    let mut lines = vec![];
    let mut size = 0;
    for entry in entries.iter().rev().take(MAX_ENTRIES) {
        let line = serde_json::to_string(entry)?;
        size += line.len() + 1;
        if size > KEEP_BYTES && !lines.is_empty() {
            break;
        }
        lines.push(line);
    }
    let mut text = String::with_capacity(size);
    for line in lines.iter().rev() {
        text.push_str(line);
        text.push('\n');
    }
    Ok(text)
}

/// Whether the file ends with a line break, or is empty or missing.
fn ends_with_newline(path: &Path) -> bool {
    use std::io::{Read as _, Seek as _, SeekFrom};
    let Ok(mut file) = fs::File::open(path) else { return true };
    if file.seek(SeekFrom::End(-1)).is_err() {
        return true;
    }
    let mut last = [0u8];
    file.read_exact(&mut last).is_err() || last[0] == b'\n'
}

/// Delete a Host's history.
pub fn forget(data_dir: &Path, host_id: &str) -> Result<()> {
    match fs::remove_file(file(data_dir, host_id)) {
        Err(err) if err.kind() != std::io::ErrorKind::NotFound => Err(err.into()),
        _ => Ok(()),
    }
}

/// Delete every Host's history.
pub fn forget_all(data_dir: &Path) -> Result<()> {
    match fs::remove_dir_all(dir(data_dir)) {
        Err(err) if err.kind() != std::io::ErrorKind::NotFound => Err(err.into()),
        _ => Ok(()),
    }
}

/// How many Hosts have a history saved.
pub fn hosts_with_history(data_dir: &Path) -> usize {
    let Ok(entries) = fs::read_dir(dir(data_dir)) else { return 0 };
    entries.flatten().filter(|e| e.path().extension().is_some_and(|ext| ext == "jsonl")).count()
}

/// A row of the history as shown: a command, or where the commands below it were entered when
/// that differs between them (the Host's address was changed).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Row {
    /// The address the commands below were entered on; `current` when it is the Host's address now.
    On { address: String, current: bool },
    /// An entry, by index into the entries.
    Entry(usize),
}

/// The rows to show, newest first, for the entries whose line contains `filter` (ignoring
/// case). While every entry was entered on the Host's current address there are no `On` rows.
pub fn rows(entries: &[Entry], current: &str, filter: &str) -> Vec<Row> {
    let needle = filter.trim().to_lowercase();
    let mixed = entries.iter().any(|e| e.on != current);
    let mut rows = vec![];
    let mut last_on: Option<&str> = None;
    for (i, entry) in entries.iter().enumerate().rev() {
        if !matches(entry, &needle) {
            continue;
        }
        if mixed && last_on != Some(entry.on.as_str()) {
            rows.push(Row::On { address: entry.on.clone(), current: entry.on == current });
            last_on = Some(&entry.on);
        }
        rows.push(Row::Entry(i));
    }
    rows
}

/// The history as a text file, oldest first like the shell's `history`: one line per command
/// with its date and time, and a note wherever the address it was entered on changes. With a
/// `filter`, only the commands that match it, as the panel shows them.
pub fn export_text(host_name: &str, current: &str, entries: &[Entry], filter: &str) -> String {
    let mut text = format!(
        "# Command history of {host_name} ({current}), kept by Komando on this PC\n# Exported {}. Oldest first.\n",
        Local::now().format("%Y-%m-%d %H:%M:%S %:z")
    );
    if !filter.trim().is_empty() {
        text.push_str(&format!("# Only commands containing \"{}\".\n", filter.trim()));
    }
    let needle = filter.trim().to_lowercase();
    let mixed = entries.iter().any(|e| e.on != current);
    let mut last_on: Option<&str> = None;
    if !mixed {
        text.push('\n');
    }
    for entry in entries.iter().filter(|e| matches(e, &needle)) {
        if mixed && last_on != Some(entry.on.as_str()) {
            let note = if entry.on == current { "" } else { " (an earlier address of this host)" };
            text.push_str(&format!("\n# On {}{note}\n", entry.on));
            last_on = Some(&entry.on);
        }
        text.push_str(&format!("{}  {}\n", entry.at.format("%Y-%m-%d %H:%M:%S"), entry.line));
    }
    text
}

/// Whether an entry's line contains `needle`, which is lowercase (empty matches all).
fn matches(entry: &Entry, needle: &str) -> bool {
    needle.is_empty() || entry.line.to_lowercase().contains(needle)
}

mod rfc3339 {
    use chrono::{DateTime, FixedOffset};
    use serde::{Deserialize as _, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(at: &DateTime<FixedOffset>, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&at.to_rfc3339_opts(chrono::SecondsFormat::Secs, false))
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<DateTime<FixedOffset>, D::Error> {
        let text = String::deserialize(d)?;
        DateTime::parse_from_rfc3339(&text).map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir() -> PathBuf {
        let dir = std::env::temp_dir().join(format!("komando-history-{}", crate::model::new_id()));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn entry(minute: u32, on: &str, line: &str) -> Entry {
        let at = DateTime::parse_from_rfc3339(&format!("2026-11-14T10:{minute:02}:00+01:00")).unwrap();
        Entry { at, on: on.into(), line: line.into() }
    }

    #[test]
    fn entries_round_trip_per_host() {
        let dir = temp_dir();
        let first = entry(1, "10.0.0.5", "[ops@web01 ~]$ uptime");
        let second = entry(2, "10.0.0.5", "[ops@web01 ~]$ df -h");
        append(&dir, "web", &first).unwrap();
        append(&dir, "web", &second).unwrap();
        append(&dir, "db", &entry(3, "10.0.0.6", "$ ls")).unwrap();
        assert_eq!(load(&dir, "web"), [first, second]);
        assert_eq!(load(&dir, "db").len(), 1);
        assert_eq!(hosts_with_history(&dir), 2);
        forget(&dir, "web").unwrap();
        assert!(load(&dir, "web").is_empty());
        forget(&dir, "web").unwrap();
        forget_all(&dir).unwrap();
        assert_eq!(hosts_with_history(&dir), 0);
        fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn a_broken_line_is_skipped() {
        let dir = temp_dir();
        append(&dir, "web", &entry(1, "a", "$ one")).unwrap();
        // A write cut short: the next entry still starts on a line of its own.
        fs::OpenOptions::new().append(true).open(file(&dir, "web")).unwrap().write_all(b"{\"at\":\"2026-11").unwrap();
        append(&dir, "web", &entry(2, "a", "$ two")).unwrap();
        let lines: Vec<String> = load(&dir, "web").into_iter().map(|e| e.line).collect();
        assert_eq!(lines, ["$ one", "$ two"]);
        fs::remove_dir_all(dir).ok();
    }

    /// A saved line whose `é` lost its second byte, as a crash can leave it: not UTF-8, which
    /// once made the whole file unreadable.
    fn cut_character_line() -> Vec<u8> {
        let mut bad = serde_json::to_string(&entry(2, "a", "$ echo é")).unwrap().into_bytes();
        let at = bad.iter().position(|&b| b == 0xc3).unwrap();
        bad.remove(at + 1);
        bad.push(b'\n');
        bad
    }

    #[test]
    fn a_line_with_a_bad_byte_costs_only_that_line() {
        let dir = temp_dir();
        append(&dir, "web", &entry(1, "a", "$ one")).unwrap();
        fs::OpenOptions::new().append(true).open(file(&dir, "web")).unwrap().write_all(&cut_character_line()).unwrap();
        append(&dir, "web", &entry(3, "a", "$ three")).unwrap();
        let lines: Vec<String> = load(&dir, "web").into_iter().map(|e| e.line).collect();
        assert_eq!(lines, ["$ one", "$ three"]);
        fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn a_trim_keeps_the_lines_around_a_bad_byte() {
        let dir = temp_dir();
        let long = "x".repeat(300);
        let mut bytes = vec![];
        for i in 0..MAX_ENTRIES + 50 {
            bytes.extend(serde_json::to_string(&entry(0, "a", &format!("$ {i} {long}"))).unwrap().into_bytes());
            bytes.push(b'\n');
        }
        // A cut character among the lines the trim keeps: before, the trim emptied the file.
        bytes.extend(cut_character_line());
        fs::create_dir_all(super::dir(&dir)).unwrap();
        fs::write(file(&dir, "web"), bytes).unwrap();
        append(&dir, "web", &entry(1, "a", "$ newest")).unwrap();
        let text = fs::read_to_string(file(&dir, "web")).expect("the trim rewrote the file");
        let kept: Vec<Entry> = text.lines().map(|l| serde_json::from_str(l).unwrap()).collect();
        assert!(kept.len() > 1000, "kept {}", kept.len());
        assert_eq!(kept[kept.len() - 2].line, format!("$ {} {long}", MAX_ENTRIES + 49));
        assert_eq!(kept.last().unwrap().line, "$ newest");
        fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn a_file_that_cant_be_read_is_not_trimmed() {
        let dir = temp_dir();
        fs::create_dir_all(super::dir(&dir)).unwrap();
        let path = file(&dir, "web");
        let line = serde_json::to_string(&entry(0, "a", &"x".repeat(300))).unwrap() + "\n";
        fs::write(&path, line.repeat(MAX_ENTRIES + 50)).unwrap();
        let before = fs::metadata(&path).unwrap().len();
        // Appending works, reading it back for the trim does not.
        super::FAIL_READS.with(|fail| fail.set(true));
        let appended = append(&dir, "web", &entry(1, "a", "$ newest"));
        super::FAIL_READS.with(|fail| fail.set(false));
        appended.unwrap();
        assert!(fs::metadata(&path).unwrap().len() > before);
        assert_eq!(load(&dir, "web").len(), MAX_ENTRIES);
        fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn odd_host_ids_stay_inside_the_folder() {
        let dir = Path::new("data");
        assert_eq!(file(dir, "a1b2c3"), dir.join("history").join("a1b2c3.jsonl"));
        let odd = file(dir, "../../evil");
        assert_eq!(odd.parent().unwrap(), dir.join("history"));
        assert!(!odd.to_string_lossy().contains(".."));
    }

    #[test]
    fn a_large_file_is_trimmed_to_the_newest_entries() {
        let dir = temp_dir();
        let long = "x".repeat(300);
        let mut text = String::new();
        for i in 0..MAX_ENTRIES + 50 {
            text.push_str(&serde_json::to_string(&entry(0, "a", &format!("$ {i} {long}"))).unwrap());
            text.push('\n');
        }
        fs::create_dir_all(super::dir(&dir)).unwrap();
        fs::write(file(&dir, "web"), text).unwrap();
        append(&dir, "web", &entry(1, "a", "$ newest")).unwrap();
        let kept = load(&dir, "web");
        // Long lines: fewer than `MAX_ENTRIES` fit, the newest ones, in order.
        assert!(kept.len() > 1000 && kept.len() < MAX_ENTRIES, "kept {}", kept.len());
        assert_eq!(kept[kept.len() - 2].line, format!("$ {} {long}", MAX_ENTRIES + 49));
        assert_eq!(kept.last().unwrap().line, "$ newest");
        let size = fs::metadata(file(&dir, "web")).unwrap().len();
        assert!(size <= KEEP_BYTES as u64, "{size} bytes");
        // The next command is only added: the trim isn't repeated on every Enter.
        let next = entry(2, "a", "$ next");
        append(&dir, "web", &next).unwrap();
        let added = serde_json::to_string(&next).unwrap().len() as u64 + 1;
        assert_eq!(fs::metadata(file(&dir, "web")).unwrap().len(), size + added);
        fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn a_trim_of_short_lines_keeps_the_newest_max_entries() {
        let dir = temp_dir();
        let mut text = String::new();
        let mut i = 0;
        while text.len() as u64 <= TRIM_AT_BYTES {
            text.push_str(&serde_json::to_string(&entry(0, "a", &format!("$ ls {i}"))).unwrap());
            text.push('\n');
            i += 1;
        }
        fs::create_dir_all(super::dir(&dir)).unwrap();
        fs::write(file(&dir, "web"), text).unwrap();
        append(&dir, "web", &entry(1, "a", "$ newest")).unwrap();
        let kept = load(&dir, "web");
        assert_eq!(kept.len(), MAX_ENTRIES);
        assert_eq!(kept[MAX_ENTRIES - 2].line, format!("$ ls {}", i - 1));
        assert_eq!(kept.last().unwrap().line, "$ newest");
        fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn a_trim_keeps_the_newest_entry_however_long() {
        let huge = entry(0, "a", &"x".repeat(KEEP_BYTES + 10));
        let text = trimmed(&[entry(0, "a", "$ old"), huge.clone()]).unwrap();
        let kept: Vec<Entry> = text.lines().map(|l| serde_json::from_str(l).unwrap()).collect();
        assert_eq!(kept, vec![huge]);
    }

    #[test]
    fn commands_after_an_extra_space_are_ignored() {
        assert!(is_ignored("[ops@web01 ~]$  export TOKEN=secret"));
        assert!(!is_ignored("[ops@web01 ~]$ export PATH=$PATH:/opt"));
        assert!(!is_ignored("plain input"));
    }

    #[test]
    fn rows_are_newest_first_and_name_the_address_only_once_it_changed() {
        let same = [entry(1, "10.0.0.5", "$ uptime"), entry(2, "10.0.0.5", "$ df -h")];
        assert_eq!(rows(&same, "10.0.0.5", ""), [Row::Entry(1), Row::Entry(0)]);

        let moved = [entry(1, "10.0.0.5", "$ uptime"), entry(2, "10.0.0.5", "$ df"), entry(3, "10.0.9.9", "$ ls")];
        assert_eq!(
            rows(&moved, "10.0.9.9", ""),
            [
                Row::On { address: "10.0.9.9".into(), current: true },
                Row::Entry(2),
                Row::On { address: "10.0.0.5".into(), current: false },
                Row::Entry(1),
                Row::Entry(0),
            ]
        );
        assert_eq!(rows(&moved, "10.0.9.9", "UPT"), [Row::On { address: "10.0.0.5".into(), current: false }, Row::Entry(0)]);
    }

    #[test]
    fn the_export_lists_commands_oldest_first_with_their_time() {
        let entries = [entry(1, "10.0.0.5", "[ops@web01 ~]$ uptime"), entry(2, "10.0.9.9", "[ops@web01 ~]$ ls")];
        let text = export_text("web01", "10.0.9.9", &entries, "");
        let body: Vec<&str> = text.lines().filter(|l| !l.starts_with("# Command") && !l.starts_with("# Exported")).collect();
        assert_eq!(
            body,
            [
                "",
                "# On 10.0.0.5 (an earlier address of this host)",
                "2026-11-14 10:01:00  [ops@web01 ~]$ uptime",
                "",
                "# On 10.0.9.9",
                "2026-11-14 10:02:00  [ops@web01 ~]$ ls",
            ]
        );
        let plain = export_text("web01", "10.0.0.5", &entries[..1], "");
        assert!(plain.ends_with("\n\n2026-11-14 10:01:00  [ops@web01 ~]$ uptime\n"), "{plain}");
        let filtered = export_text("web01", "10.0.9.9", &entries, " LS ");
        assert!(filtered.contains("# Only commands containing \"LS\".\n"), "{filtered}");
        assert!(!filtered.contains("uptime") && filtered.contains("$ ls"), "{filtered}");
    }

    #[test]
    fn address_labels_leave_out_the_usual_port() {
        assert_eq!(address_label("10.0.0.5", 22), "10.0.0.5");
        assert_eq!(address_label("db.lan", 2222), "db.lan:2222");
    }
}
