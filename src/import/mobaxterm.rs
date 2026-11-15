//! Reading SSH sessions from MobaXterm.
//!
//! MobaXterm keeps its sessions in `MobaXterm.ini` (next to a portable MobaXterm.exe, in
//! `Documents\MobaXterm` or in `%APPDATA%\MobaXterm`); "Export sessions" writes the same
//! sections to a `.mxtsessions` file. The format isn't documented: what's read here is taken
//! from real exports (see `testdata/`) and other clients' importers.
//!
//! Each `[Bookmarks]`, `[Bookmarks_1]`, … section is one session folder: `SubRep` is its path
//! (`Parent\Child`, empty for sessions in no folder) and `ImgNum` its icon. Every other line is
//! a session, `name=#icon#fields#terminal#…`, where `fields` are `%`-separated: 0 is the
//! session type (0 SSH, 4 RDP, …), 1–3 host, port and user. For SSH, 7 is "Execute command",
//! 8–10 the SSH gateway's hosts, ports and users (one per hop, separated by `__PIPE__`), 14 the
//! private key, and 19–22 the proxy's type (0: none), host, port and user. Characters MobaXterm
//! uses as separators are written as `__PERCENT__`, `__DIEZE__` (#) and so on. The file is
//! written in the Windows ANSI code page (Windows-1252), not UTF-8.
//!
//! Saved passwords (`[Passwords]`, `[Credentials]`) are never read.

use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result, bail};

use super::{Found, FoundGroup, FoundHost, LeftOut};
use crate::model::{JumpHost, StartupCommand};

/// The file name MobaXterm keeps its settings and sessions in.
pub const CONFIG_FILE: &str = "MobaXterm.ini";

/// The project name for sessions that are in no folder.
pub const UNGROUPED: &str = "MobaXterm";

/// Where an installed MobaXterm keeps `MobaXterm.ini` on this PC, most likely first. A
/// portable MobaXterm keeps it next to its .exe, which can be anywhere.
pub fn config_candidates() -> Vec<PathBuf> {
    let mut dirs = vec![];
    if let Some(documents) = dirs::document_dir() {
        dirs.push(documents.join("MobaXterm"));
    }
    if let Some(home) = dirs::home_dir() {
        // Documents moved to OneDrive.
        dirs.push(home.join("OneDrive").join("Documents").join("MobaXterm"));
    }
    if let Some(config) = dirs::config_dir() {
        dirs.push(config.join("MobaXterm"));
    }
    dirs.into_iter().map(|dir| dir.join(CONFIG_FILE)).collect()
}

/// MobaXterm's settings on this PC, if they are in one of the usual places and list sessions.
pub fn find_config() -> Option<PathBuf> {
    config_candidates().into_iter().find(|path| path.is_file())
}

/// Read `MobaXterm.ini` or an exported `.mxtsessions` file.
pub fn read(path: &Path) -> Result<Found> {
    let bytes = std::fs::read(path).with_context(|| format!("Could not read {}", path.display()))?;
    parse(&decode(&bytes))
}

/// The file's text: UTF-8 when it is valid UTF-8 (a file someone edited and saved that way),
/// otherwise Windows-1252, which MobaXterm writes.
pub fn decode(bytes: &[u8]) -> String {
    let bytes = bytes.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(bytes);
    match std::str::from_utf8(bytes) {
        Ok(text) => text.to_string(),
        Err(_) => bytes.iter().map(|&b| windows_1252(b)).collect(),
    }
}

/// One Windows-1252 byte as a character. 0x80–0x9F hold the characters Latin-1 doesn't have;
/// the five bytes Windows-1252 leaves unassigned keep their Latin-1 control character.
fn windows_1252(byte: u8) -> char {
    const HIGH: [char; 32] = [
        '€', '\u{81}', '‚', 'ƒ', '„', '…', '†', '‡', 'ˆ', '‰', 'Š', '‹', 'Œ', '\u{8D}', 'Ž', '\u{8F}', //
        '\u{90}', '‘', '’', '“', '”', '•', '–', '—', '˜', '™', 'š', '›', 'œ', '\u{9D}', 'ž', 'Ÿ',
    ];
    match byte {
        0x80..=0x9F => HIGH[(byte - 0x80) as usize],
        _ => byte as char,
    }
}

pub fn parse(text: &str) -> Result<Found> {
    let sections = bookmark_sections(text);
    if sections.is_empty() {
        bail!("This isn't a MobaXterm sessions file: it has no [Bookmarks] sections.");
    }
    let mut groups: Vec<FoundGroup> = vec![];
    let mut ungrouped = FoundGroup { name: UNGROUPED.into(), ..Default::default() };
    let mut other_types: Vec<(&'static str, usize)> = vec![];
    let mut notes = vec![];
    for section in &sections {
        let folder = folder_name(section.sub_rep);
        for &(name, value) in &section.sessions {
            let name = unescape(name);
            let session = match Session::parse(value) {
                Some(session) => session,
                None => {
                    notes.push(format!("\"{name}\" couldn't be read and is left out."));
                    continue;
                }
            };
            if session.kind() != "0" {
                let label = type_label(session.kind());
                match other_types.iter_mut().find(|(l, _)| *l == label) {
                    Some((_, n)) => *n += 1,
                    None => other_types.push((label, 1)),
                }
                continue;
            }
            let Some(host) = ssh_host(&name, &session) else {
                notes.push(format!("\"{name}\" has no host name in MobaXterm and is left out."));
                continue;
            };
            let group = match &folder {
                None => &mut ungrouped,
                Some(folder) => match groups.iter().position(|g| g.name == *folder) {
                    Some(i) => &mut groups[i],
                    None => {
                        groups.push(FoundGroup { name: folder.clone(), ..Default::default() });
                        groups.last_mut().unwrap()
                    }
                },
            };
            group.hosts.push(host);
        }
    }
    // Sessions in no folder come last, as Tabby's ungrouped profiles do.
    groups.push(ungrouped);
    groups.retain(|g| !g.hosts.is_empty());
    if !other_types.is_empty() {
        let total: usize = other_types.iter().map(|(_, n)| n).sum();
        let kinds: Vec<String> = other_types.iter().map(|(k, n)| format!("{n} {k}")).collect();
        let sessions = if total == 1 { "session stays" } else { "sessions stay" };
        notes.insert(0, format!("Komando imports SSH sessions only: {} {sessions} in MobaXterm.", join_and(&kinds)));
    }
    Ok(Found { groups, notes })
}

/// One `[Bookmarks…]` section: its folder path and its sessions as (name, value).
struct Section<'a> {
    sub_rep: &'a str,
    sessions: Vec<(&'a str, &'a str)>,
}

/// The `[Bookmarks]` and `[Bookmarks_N]` sections, in the file's order. Other sections (settings,
/// saved passwords) are skipped.
fn bookmark_sections(text: &str) -> Vec<Section<'_>> {
    let mut sections = vec![];
    let mut current: Option<Section> = None;
    for line in text.lines() {
        let line = line.trim();
        if let Some(header) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
            sections.extend(current.take());
            let header = header.trim();
            let is_bookmarks = header.get(..9).is_some_and(|start| start.eq_ignore_ascii_case("bookmarks"))
                && (header.len() == 9 || header[9..].strip_prefix('_').is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit())));
            if is_bookmarks {
                current = Some(Section { sub_rep: "", sessions: vec![] });
            }
            continue;
        }
        let Some(section) = current.as_mut() else { continue };
        let Some((key, value)) = line.split_once('=') else { continue };
        let (key, value) = (key.trim(), value.trim());
        if key.eq_ignore_ascii_case("SubRep") {
            section.sub_rep = value;
        } else if !key.eq_ignore_ascii_case("ImgNum") && !key.is_empty() {
            section.sessions.push((key, value));
        }
    }
    sections.extend(current);
    sections
}

/// A folder path (`Parent\Child`) as a project name, "Parent / Child"; None for no folder.
fn folder_name(sub_rep: &str) -> Option<String> {
    let parts: Vec<String> = sub_rep.split('\\').map(unescape).filter(|p| !p.is_empty()).collect();
    (!parts.is_empty()).then(|| parts.join(" / "))
}

/// A session's value, split: `#icon#fields#…`.
struct Session<'a> {
    fields: Vec<&'a str>,
}

impl<'a> Session<'a> {
    fn parse(value: &'a str) -> Option<Self> {
        let mut parts = value.trim().split('#');
        let (Some(""), Some(_icon), Some(fields)) = (parts.next(), parts.next(), parts.next()) else { return None };
        let fields: Vec<&str> = fields.split('%').collect();
        (fields.len() >= 3).then_some(Self { fields })
    }

    fn kind(&self) -> &str {
        self.fields[0].trim()
    }

    /// A field, unescaped; None when empty.
    fn field(&self, i: usize) -> Option<String> {
        self.fields.get(i).map(|f| unescape(f)).filter(|f| !f.is_empty())
    }

    /// A field holding one value per gateway hop.
    fn hops(&self, i: usize) -> Vec<String> {
        self.fields.get(i).map(|f| f.split("__PIPE__").map(unescape).collect()).unwrap_or_default()
    }
}

/// An SSH session as a host; None when it names no host.
fn ssh_host(name: &str, session: &Session) -> Option<FoundHost> {
    let address = session.field(1)?;
    let port = port(session.field(2).as_deref()).unwrap_or(22);
    let user = session.field(3).and_then(user);
    let command = session.field(7);
    let mut left_out = vec![];
    if let Some(key) = session.field(14) {
        left_out.push(LeftOut::new("Private key", key_label(&key)));
    }
    let proxy = session.field(19).filter(|kind| kind != "0");
    if let (Some(_), Some(host)) = (proxy, session.field(20)) {
        let detail = match session.field(21) {
            Some(port) => format!("{host}:{port}"),
            None => host,
        };
        left_out.push(LeftOut::new("Proxy", detail));
    }
    Some(FoundHost {
        name: if name.is_empty() { address.clone() } else { name.to_string() },
        address,
        port,
        user,
        jump_hosts: Some(gateway(session)),
        startup_commands: command.map(|c| vec![StartupCommand::new(c)]),
        left_out,
        ..Default::default()
    })
}

/// The SSH gateway's hops, in the order they're passed through. Hops without a host are skipped.
fn gateway(session: &Session) -> Vec<JumpHost> {
    let ports = session.hops(9);
    let users = session.hops(10);
    session
        .hops(8)
        .into_iter()
        .enumerate()
        .filter(|(_, host)| !host.is_empty())
        .map(|(i, host)| JumpHost {
            port: port(ports.get(i).map(String::as_str)).unwrap_or(22),
            user: users.get(i).cloned().and_then(user),
            ..JumpHost::new(host)
        })
        .collect()
}

fn port(text: Option<&str>) -> Option<u16> {
    text?.trim().parse::<u16>().ok().filter(|p| *p != 0)
}

/// A user name; MobaXterm writes placeholders like `<default>` for none.
fn user(name: String) -> Option<String> {
    let name = name.trim();
    (!name.is_empty() && !(name.starts_with('<') && name.ends_with('>'))).then(|| name.to_string())
}

/// A key's file name. MobaXterm writes `_CurrentDrive_:\…` and `_ProfileDir_\…` for folders it
/// moves along with a portable install.
fn key_label(path: &str) -> String {
    path.rsplit(['/', '\\']).find(|s| !s.is_empty()).unwrap_or(path).to_string()
}

/// MobaXterm's escapes for the characters it uses as separators.
fn unescape(text: &str) -> String {
    text.replace("__PIPE__", "|")
        .replace("__PTVIRG__", ";")
        .replace("__DBLQUO__", "\"")
        .replace("__DIEZE__", "#")
        .replace("__PERCENT__", "%")
        .trim()
        .to_string()
}

/// What a MobaXterm session type is called.
fn type_label(kind: &str) -> &'static str {
    match kind {
        "1" => "Telnet",
        "2" => "Rsh",
        "3" => "XDMCP",
        "4" => "RDP",
        "5" => "VNC",
        "6" => "FTP",
        "7" => "SFTP",
        "8" => "serial",
        "9" => "browser",
        "10" => "Mosh",
        _ => "other",
    }
}

fn join_and(items: &[String]) -> String {
    match items {
        [] => String::new(),
        [one] => one.clone(),
        [rest @ .., last] => format!("{} and {last}", rest.join(", ")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(bytes: &[u8]) -> Found {
        parse(&decode(bytes)).unwrap()
    }

    fn names(found: &Found) -> Vec<(&str, Vec<&str>)> {
        found.groups.iter().map(|g| (g.name.as_str(), g.hosts.iter().map(|h| h.name.as_str()).collect())).collect()
    }

    #[test]
    fn a_plain_ssh_session_in_a_folder() {
        let found = fixture(include_bytes!("testdata/mobaxterm-plain.mxtsessions"));
        assert_eq!(names(&found), [("Folder 1", vec!["192.168.0.1 (user)"])]);
        let host = &found.groups[0].hosts[0];
        assert_eq!((host.address.as_str(), host.port, host.user.as_deref()), ("192.168.0.1", 22, Some("user")));
        assert_eq!(host.jump_hosts, Some(vec![]), "no gateway: the host connects directly");
        assert_eq!(host.startup_commands, None);
        assert!(host.left_out.is_empty(), "{:?}", host.left_out);
        assert!(found.notes.is_empty());
    }

    #[test]
    fn execute_command_proxy_nested_folders_and_other_types() {
        let found = fixture(include_bytes!("testdata/mobaxterm-proxy.mxtsessions"));
        assert_eq!(
            names(&found),
            [("Folder 1", vec!["192.168.0.1 (user)"]), ("Folder 2 / Folder 3", vec!["192.168.0.1 Pönorama"])],
            "decoded as Windows-1252; the empty Folder 2 adds no project"
        );
        let host = &found.groups[1].hosts[0];
        assert_eq!(host.startup_commands, Some(vec![StartupCommand::new("echo 'hello world'")]));
        assert_eq!(host.jump_hosts, Some(vec![]));
        assert_eq!(host.left_out, [LeftOut::new("Proxy", "192.168.1.1:22")]);
        assert_eq!(found.notes, ["Komando imports SSH sessions only: 1 RDP session stays in MobaXterm."]);
    }

    #[test]
    fn the_ssh_gateway_becomes_a_jump_host() {
        let found = fixture(include_bytes!("testdata/mobaxterm-gateway.mxtsessions"));
        let host = &found.groups[0].hosts[0];
        assert_eq!(host.jump_hosts, Some(vec![JumpHost { user: Some("jump_host_admin".into()), ..JumpHost::new("192.168.4.4") }]));
        assert_eq!(found.groups[1].hosts[0].jump_hosts, Some(vec![]), "the other session has no gateway");
    }

    #[test]
    fn a_gateway_with_several_hops_keeps_them_in_order() {
        let text = "[Bookmarks]\nSubRep=\nImgNum=42\n\
                    db=#109#0%db.lan%2222%<default>%%-1%0%%a.lan__PIPE__b.lan%22__PIPE__2200%ops__PIPE__#MobaFont#0# #-1\n";
        let found = parse(text).unwrap();
        assert_eq!(names(&found), [("MobaXterm", vec!["db"])], "sessions in no folder");
        let host = &found.groups[0].hosts[0];
        assert_eq!((host.port, host.user.as_deref()), (2222, None));
        assert_eq!(
            host.jump_hosts,
            Some(vec![JumpHost { user: Some("ops".into()), ..JumpHost::new("a.lan") }, JumpHost { port: 2200, ..JumpHost::new("b.lan") }])
        );
    }

    #[test]
    fn escaped_characters_keys_and_unreadable_sessions() {
        let text = "[Bookmarks_1]\nSubRep=Ops__PERCENT__\\Lab\n\
                    web__DIEZE__1= #109#0%web.lan%22%root%%-1%0%echo 50__PERCENT__ __DIEZE__ok%%%%0%-1%0%_CurrentDrive_:\\keys\\id_ed25519%%-1%-1%0%0%%1080%%0#MobaFont\n\
                    broken=not a session\n\
                    nohost=#109#0%%22%root%\n\
                    [Passwords]\nroot@web.lan=secret\n";
        let found = parse(text).unwrap();
        assert_eq!(names(&found), [("Ops% / Lab", vec!["web#1"])]);
        let host = &found.groups[0].hosts[0];
        assert_eq!(host.startup_commands, Some(vec![StartupCommand::new("echo 50% #ok")]));
        assert_eq!(host.left_out, [LeftOut::new("Private key", "id_ed25519")], "a proxy of type 0 is none");
        assert_eq!(
            found.notes,
            ["\"broken\" couldn't be read and is left out.", "\"nohost\" has no host name in MobaXterm and is left out."]
        );
    }

    #[test]
    fn a_file_without_bookmarks_is_not_a_sessions_file() {
        assert!(parse("[Misc]\nFoo=1\n").is_err());
        assert!(parse("").is_err());
        assert!(parse("[Bookmarks]\nSubRep=\n").unwrap().groups.is_empty());
    }

    #[test]
    fn decoding_prefers_utf8_and_falls_back_to_windows_1252() {
        assert_eq!(decode("Pönorama €".as_bytes()), "Pönorama €");
        assert_eq!(decode(b"\xEF\xBB\xBFok"), "ok");
        assert_eq!(decode(b"P\xF6norama \x80"), "Pönorama €");
    }
}
