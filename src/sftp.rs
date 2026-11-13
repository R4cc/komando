//! SFTP on an SSH Session's connection: listing, file operations and transfers.
//!
//! A file browser opens an SFTP channel on the connection its terminal already has, so
//! nothing signs in twice (see `ssh::Command::OpenSftp`). Everything here runs on the SSH
//! runtime; the UI awaits the handles [`spawn`] returns.
//!
//! Transfers never leave a misleading file behind. A download is written to a partial file
//! next to its destination and only renamed into place once every byte has arrived; on any
//! failure the partial file is removed. The remote file is opened before anything is created
//! locally, so a permission error never produces an empty file. Uploads work the same way on
//! the server.

use std::future::Future;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::time::Duration;

use anyhow::{Result, anyhow};
use futures::{StreamExt as _, TryStreamExt as _};
use russh::ChannelMsg;
use russh::client::Handle;
use russh_sftp::client::SftpSession;
use russh_sftp::client::error::Error as SftpError;
use russh_sftp::protocol::{FileAttributes, OpenFlags, StatusCode};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use crate::session::ssh::{self, Client};

/// Suffix of files and folders still being transferred.
pub const PARTIAL_SUFFIX: &str = ".komando-part";
const CHUNK: usize = 256 * 1024;

/// Read and write requests one file keeps in flight, of up to [`REQUEST_KIB`] each.
/// Throughput is what is in flight per round trip, so a distant server needs many; but on a
/// slow link the last ones queue behind the others and must still arrive within
/// [`REQUEST_TIMEOUT_SECS`].
const PIPELINE: usize = 16;
/// The largest read or write request: OpenSSH's limit (256 KiB packets, less headers).
const REQUEST_KIB: usize = 255;
/// How long the server may take to answer one request. Generous, because a request waits
/// behind everything in flight before it: up to [`BUDGET_KIB`], 32 s at 256 KiB/s. A dead
/// connection is noticed sooner by the SSH keepalive.
const REQUEST_TIMEOUT_SECS: u64 = 60;
/// How many files a folder transfer (or delete) works on at once, and how many files all
/// transfers on one connection have open together. Each file costs a few round trips of its
/// own (open, read, close) that one at a time would wait out back to back; but servers limit
/// open files (OpenSSH fails opens past a few hundred).
const PARALLEL: usize = 32;
/// How many bytes all transfers on one connection may have in flight together, in KiB: many
/// small files at once, but large ones two at a time, so however many transfers run, the
/// server answers every request within the timeout on a slow link too. More wait their turn.
const BUDGET_KIB: u32 = 8192;

/// Run `future` on the SSH runtime. The handle can be awaited from any executor.
pub fn spawn<F>(future: F) -> impl Future<Output = Result<F::Output, OpError>>
where
    F: Future + Send + 'static,
    F::Output: Send + 'static,
{
    let handle = ssh::runtime().spawn(future);
    async move { handle.await.map_err(|err| OpError::new(ErrorKind::Other, format!("Stopped unexpectedly: {err}"))) }
}

// ---- errors ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ErrorKind {
    PermissionDenied,
    NotFound,
    Exists,
    /// The connection dropped or the server stopped answering.
    Connection,
    Cancelled,
    /// The server doesn't support what was asked.
    Unsupported,
    /// Reading or writing on this PC failed.
    Local,
    Other,
}

/// Why an operation failed, worded for the user.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OpError {
    pub kind: ErrorKind,
    pub message: String,
}

impl OpError {
    pub fn new(kind: ErrorKind, message: impl Into<String>) -> Self {
        Self { kind, message: message.into() }
    }

    fn denied() -> Self {
        Self::new(ErrorKind::PermissionDenied, "Permission denied (EACCES)")
    }

    fn not_found() -> Self {
        Self::new(ErrorKind::NotFound, "No such file or directory (ENOENT)")
    }

    pub fn cancelled() -> Self {
        Self::new(ErrorKind::Cancelled, "Cancelled")
    }

    fn connection() -> Self {
        Self::new(ErrorKind::Connection, "Connection lost")
    }

    /// The same error, naming the path it happened on.
    fn at(mut self, path: &str) -> Self {
        if !matches!(self.kind, ErrorKind::Cancelled | ErrorKind::Connection) && !self.message.contains(path) {
            self.message = format!("{}: {path}", self.message);
        }
        self
    }

    /// From an error message only (write errors and shell commands give nothing more).
    fn from_message(message: &str) -> Self {
        let lower = message.to_lowercase();
        if lower.contains("permission denied") || lower.contains("operation not permitted") {
            Self::denied()
        } else if lower.contains("no such file") {
            Self::not_found()
        } else if lower.contains("no space") || lower.contains("quota") {
            Self::new(ErrorKind::Other, "No space left on the server")
        } else if message.trim().is_empty() || message.trim() == "Failure" {
            Self::new(ErrorKind::Other, "The server reported a failure")
        } else {
            Self::new(ErrorKind::Other, message.trim().to_string())
        }
    }

    fn local(err: &std::io::Error, path: &Path) -> Self {
        let what = match err.kind() {
            std::io::ErrorKind::PermissionDenied => "Access denied".to_string(),
            std::io::ErrorKind::NotFound => "Not found".to_string(),
            std::io::ErrorKind::StorageFull => "The disk is full".to_string(),
            _ => err.to_string(),
        };
        Self::new(ErrorKind::Local, format!("{what}: {}", path.display()))
    }

    /// An I/O error from a remote file (reads and writes surface SFTP errors this way).
    fn remote_io(err: &std::io::Error) -> Self {
        if let Some(inner) = err.get_ref().and_then(|e| e.downcast_ref::<SftpError>()) {
            return Self::from(inner.clone());
        }
        match err.kind() {
            std::io::ErrorKind::TimedOut => Self::new(ErrorKind::Connection, "The server stopped responding"),
            std::io::ErrorKind::BrokenPipe | std::io::ErrorKind::ConnectionReset | std::io::ErrorKind::UnexpectedEof => {
                Self::connection()
            }
            _ => Self::from_message(&err.to_string()),
        }
    }
}

impl std::fmt::Display for OpError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for OpError {}

impl From<SftpError> for OpError {
    fn from(err: SftpError) -> Self {
        match err {
            SftpError::Status(status) => match status.status_code {
                StatusCode::PermissionDenied => Self::denied(),
                StatusCode::NoSuchFile => Self::not_found(),
                StatusCode::NoConnection | StatusCode::ConnectionLost => Self::connection(),
                StatusCode::OpUnsupported => Self::new(ErrorKind::Unsupported, "The server doesn't support this"),
                StatusCode::Eof => Self::new(ErrorKind::Other, "Unexpected end of file"),
                _ => Self::from_message(&status.error_message),
            },
            SftpError::Timeout => Self::new(ErrorKind::Connection, "The server stopped responding"),
            SftpError::IO(_) => Self::connection(),
            SftpError::Limited(message) => Self::new(ErrorKind::Other, message),
            other => Self::new(ErrorKind::Other, other.to_string()),
        }
    }
}

// ---- who we are, and what we may do -------------------------------------------------

/// The signed-in user on the server, from `id`. Unknown when `id` couldn't run.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Identity {
    pub uid: Option<u32>,
    pub name: Option<String>,
    /// Every group the user is in, primary group first, with names where known.
    pub groups: Vec<(u32, Option<String>)>,
}

impl Identity {
    /// Parse `id`'s output: `uid=1000(codex) gid=1000(codex) groups=1000(codex),27(sudo)`.
    pub fn parse_id(output: &str) -> Self {
        fn id_and_name(s: &str) -> Option<(u32, Option<String>)> {
            let (num, rest) = match s.find('(') {
                Some(i) => (&s[..i], Some(&s[i + 1..])),
                None => (s, None),
            };
            let name = rest.and_then(|r| r.strip_suffix(')')).map(str::to_string);
            Some((num.trim().parse().ok()?, name))
        }
        let mut identity = Identity::default();
        for field in output.split_whitespace() {
            let Some((key, value)) = field.split_once('=') else { continue };
            match key {
                "uid" => {
                    if let Some((uid, name)) = id_and_name(value) {
                        identity.uid = Some(uid);
                        identity.name = name;
                    }
                }
                "gid" => {
                    if let Some(group) = id_and_name(value) {
                        identity.groups.retain(|(g, _)| *g != group.0);
                        identity.groups.insert(0, group);
                    }
                }
                "groups" => {
                    for group in value.split(',').filter_map(id_and_name) {
                        if !identity.groups.iter().any(|(g, _)| *g == group.0) {
                            identity.groups.push(group);
                        }
                    }
                }
                _ => {}
            }
        }
        identity
    }

    pub fn group_name(&self, gid: u32) -> Option<&str> {
        self.groups.iter().find(|(g, _)| *g == gid).and_then(|(_, n)| n.as_deref())
    }
}

/// Whether the user may do something: definitely yes, definitely no, or can't tell (no
/// permission bits, or `id` didn't run). Only `No` greys anything out.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Allowed {
    Yes,
    No,
    Unknown,
}

impl Allowed {
    fn from(bit: bool) -> Self {
        if bit { Allowed::Yes } else { Allowed::No }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Access {
    pub read: Allowed,
    pub write: Allowed,
    pub exec: Allowed,
}

impl Access {
    pub const UNKNOWN: Access = Access { read: Allowed::Unknown, write: Allowed::Unknown, exec: Allowed::Unknown };
}

/// What `identity` may do with a file of this mode and owner, by the Unix permission bits.
/// ACLs and similar can grant more, so a "no" here means "appears not to".
pub fn access(identity: &Identity, mode: Option<u32>, uid: Option<u32>, gid: Option<u32>, is_dir: bool) -> Access {
    let (Some(me), Some(mode)) = (identity.uid, mode) else { return Access::UNKNOWN };
    if me == 0 {
        // root reads and writes anything; it may execute a file only if some x bit is set.
        let exec = if is_dir { Allowed::Yes } else { Allowed::from(mode & 0o111 != 0) };
        return Access { read: Allowed::Yes, write: Allowed::Yes, exec };
    }
    let Some(owner) = uid else { return Access::UNKNOWN };
    let bits = if owner == me {
        (mode >> 6) & 0o7
    } else {
        match gid {
            Some(gid) if identity.groups.iter().any(|(g, _)| *g == gid) => (mode >> 3) & 0o7,
            Some(_) => mode & 0o7,
            None => return Access::UNKNOWN,
        }
    };
    Access { read: Allowed::from(bits & 0o4 != 0), write: Allowed::from(bits & 0o2 != 0), exec: Allowed::from(bits & 0o1 != 0) }
}

// ---- entries --------------------------------------------------------------------------

const S_IFMT: u32 = 0o170000;
const S_IFDIR: u32 = 0o040000;
const S_IFLNK: u32 = 0o120000;
const S_IFREG: u32 = 0o100000;

fn is_dir_mode(mode: Option<u32>) -> bool {
    mode.is_some_and(|m| m & S_IFMT == S_IFDIR)
}

fn is_link_mode(mode: Option<u32>) -> bool {
    mode.is_some_and(|m| m & S_IFMT == S_IFLNK)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Dir,
    File,
    /// Devices, sockets, pipes: listed, not transferred.
    Other,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Entry {
    pub name: String,
    pub path: String,
    /// What it is, following a symlink.
    pub kind: Kind,
    pub link: bool,
    /// A symlink whose target can't be read (missing, or no permission).
    pub broken_link: bool,
    pub size: Option<u64>,
    /// Seconds since the Unix epoch.
    pub mtime: Option<u32>,
    /// Mode bits as listed (a symlink shows as `l`).
    pub mode: Option<u32>,
    pub uid: Option<u32>,
    pub gid: Option<u32>,
    /// What the signed-in user may do with it (with the link's target).
    pub access: Access,
}

impl Entry {
    fn from_attrs(dir: &str, name: String, attrs: &FileAttributes, target: Option<&FileAttributes>, identity: &Identity) -> Self {
        let link = is_link_mode(attrs.permissions);
        let effective = target.unwrap_or(attrs);
        let kind = if link && target.is_none() {
            Kind::Other
        } else if is_dir_mode(effective.permissions) {
            Kind::Dir
        } else if effective.permissions.is_none_or(|m| m & S_IFMT == S_IFREG) {
            Kind::File
        } else {
            Kind::Other
        };
        let access = if link && target.is_none() {
            Access::UNKNOWN
        } else {
            access(identity, effective.permissions, effective.uid, effective.gid, kind == Kind::Dir)
        };
        Entry {
            path: join(dir, &name),
            name,
            kind,
            link,
            broken_link: link && target.is_none(),
            size: if kind == Kind::Dir { None } else { effective.size },
            mtime: effective.mtime,
            mode: attrs.permissions,
            uid: effective.uid,
            gid: effective.gid,
            access,
        }
    }

    pub fn is_dir(&self) -> bool {
        self.kind == Kind::Dir
    }

    pub fn is_hidden(&self) -> bool {
        self.name.starts_with('.')
    }

    /// The main action is definitely denied: listing a folder, reading a file.
    pub fn locked(&self) -> bool {
        match self.kind {
            Kind::Dir => self.access.read == Allowed::No || self.access.exec == Allowed::No,
            Kind::File => self.access.read == Allowed::No,
            Kind::Other => false,
        }
    }

    pub fn mode_string(&self) -> String {
        self.mode.map(mode_string).unwrap_or_else(|| "?".into())
    }
}

/// `drwxr-xr-x`, as `ls -l` shows it.
pub fn mode_string(mode: u32) -> String {
    let kind = match mode & S_IFMT {
        S_IFDIR => 'd',
        S_IFLNK => 'l',
        0o020000 => 'c',
        0o060000 => 'b',
        0o010000 => 'p',
        0o140000 => 's',
        _ => '-',
    };
    let mut s = String::with_capacity(10);
    s.push(kind);
    for (shift, special, special_char) in [(6, 0o4000, 's'), (3, 0o2000, 's'), (0, 0o1000, 't')] {
        let bits = (mode >> shift) & 0o7;
        s.push(if bits & 0o4 != 0 { 'r' } else { '-' });
        s.push(if bits & 0o2 != 0 { 'w' } else { '-' });
        let x = bits & 0o1 != 0;
        s.push(match (mode & special != 0, x) {
            (true, true) => special_char,
            (true, false) => special_char.to_ascii_uppercase(),
            (false, true) => 'x',
            (false, false) => '-',
        });
    }
    s
}

/// A folder's contents, and what the user may do in the folder itself.
#[derive(Clone, Debug)]
pub struct Listing {
    /// The canonical path (symlinks and `..` resolved).
    pub path: String,
    pub entries: Vec<Entry>,
    pub access: Access,
}

// ---- remote paths ---------------------------------------------------------------------

pub fn join(dir: &str, name: &str) -> String {
    if dir.is_empty() {
        name.to_string()
    } else if dir.ends_with('/') {
        format!("{dir}{name}")
    } else {
        format!("{dir}/{name}")
    }
}

/// The folder containing `path`; `None` for the root.
pub fn parent(path: &str) -> Option<String> {
    let trimmed = path.trim_end_matches('/');
    if trimmed.is_empty() {
        return None;
    }
    match trimmed.rfind('/') {
        Some(0) => Some("/".into()),
        Some(i) => Some(trimmed[..i].to_string()),
        None => None,
    }
}

pub fn file_name(path: &str) -> &str {
    let trimmed = path.trim_end_matches('/');
    trimmed.rsplit('/').next().unwrap_or(trimmed)
}

/// Breadcrumb segments: (name, full path), starting with the root ("/", "/").
pub fn segments(path: &str) -> Vec<(String, String)> {
    let mut out = vec![("/".to_string(), "/".to_string())];
    let mut full = String::new();
    for part in path.split('/').filter(|p| !p.is_empty()) {
        full.push('/');
        full.push_str(part);
        out.push((part.to_string(), full.clone()));
    }
    out
}

/// Quote for a POSIX shell.
pub fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

/// "name (2).ext": a name not in `taken`, keeping the extension.
pub fn unique_name(name: &str, taken: impl Fn(&str) -> bool) -> String {
    if !taken(name) {
        return name.to_string();
    }
    // Hidden files (".bashrc") and names without a dot have no extension.
    let (stem, ext) = match name.rfind('.') {
        Some(i) if i > 0 => (&name[..i], &name[i..]),
        _ => (name, ""),
    };
    (1..).map(|n| format!("{stem} ({n}){ext}")).find(|candidate| !taken(candidate)).expect("some name is free")
}

/// A local path in `dir` that doesn't exist yet, nor as a partial download.
pub fn unique_local_path(dir: &Path, name: &str) -> PathBuf {
    let name = unique_name(name, |n| {
        dir.join(n).exists() || dir.join(format!("{n}{PARTIAL_SUFFIX}")).exists()
    });
    dir.join(name)
}

/// Make a remote name safe as a Windows file name: no forbidden characters, no trailing dots
/// or spaces, and no device name ("con.log" becomes "con_.log").
pub fn local_file_name(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .map(|c| if matches!(c, '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*') || c.is_control() { '_' } else { c })
        .collect();
    let trimmed = cleaned.trim_end_matches(['.', ' ']);
    if trimmed.is_empty() {
        return "_".into();
    }
    // Windows reads everything before the first dot as the device name, whatever the extension.
    let (stem, ext) = trimmed.split_at(trimmed.find('.').unwrap_or(trimmed.len()));
    if is_device_name(stem) { format!("{stem}_{ext}") } else { trimmed.to_string() }
}

/// Names Windows reserves for devices: CON, PRN, AUX, NUL, COM1-9 and LPT1-9 (also with the
/// superscript digits), CONIN$ and CONOUT$. Case doesn't matter, and trailing spaces are ignored.
fn is_device_name(stem: &str) -> bool {
    let stem = stem.trim_end_matches(' ').to_ascii_uppercase();
    if matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL" | "CONIN$" | "CONOUT$") {
        return true;
    }
    match stem.strip_prefix("COM").or_else(|| stem.strip_prefix("LPT")) {
        Some(digit) => matches!(digit, "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9" | "\u{b9}" | "\u{b2}" | "\u{b3}"),
        None => false,
    }
}

/// Where a folder download puts each remote entry: local names (relative to the download's
/// root, "/" separated) for the folders and for the files (remote path, local path, size).
/// Names that Windows would treat as the same ("README" and "readme", "foo." and "foo") get
/// "name (1)" like a top-level download does, so nothing is ever written over. Entries are
/// handled in a fixed order (shallow first, then by name), so the same listing always gives
/// the same names however it was read.
fn map_download_names(dirs: &[String], files: &[(String, u64)]) -> (Vec<String>, Vec<(String, String, u64)>) {
    let mut entries: Vec<(&str, Option<u64>)> = dirs
        .iter()
        .filter(|rel| !rel.is_empty())
        .map(|rel| (rel.as_str(), None))
        .chain(files.iter().map(|(rel, size)| (rel.as_str(), Some(*size))))
        .collect();
    entries.sort_by(|a, b| depth(a.0).cmp(&depth(b.0)).then_with(|| a.0.cmp(b.0)));
    // NTFS ignores case: names are compared in upper case, as it does.
    let fold = |name: &str| name.to_uppercase();
    let mut local_dirs: std::collections::HashMap<&str, String> = std::collections::HashMap::from([("", String::new())]);
    let mut taken: std::collections::HashMap<String, HashSet<String>> = std::collections::HashMap::new();
    let (mut out_dirs, mut out_files) = (vec![], vec![]);
    for (rel, size) in entries {
        let (parent, name) = rel.rsplit_once('/').unwrap_or(("", rel));
        // Folders are listed before their contents, and sorted by depth: the parent is mapped.
        let parent_local = local_dirs.get(parent).cloned().unwrap_or_default();
        let siblings = taken.entry(parent_local.clone()).or_default();
        let chosen = unique_name(&local_file_name(name), |n| siblings.contains(&fold(n)));
        siblings.insert(fold(&chosen));
        let local = if parent_local.is_empty() { chosen } else { format!("{parent_local}/{chosen}") };
        match size {
            Some(size) => out_files.push((rel.to_string(), local, size)),
            None => {
                local_dirs.insert(rel, local.clone());
                out_dirs.push(local);
            }
        }
    }
    (out_dirs, out_files)
}

// ---- progress -------------------------------------------------------------------------

/// Shared between a running transfer and the UI that shows it.
#[derive(Default, Debug)]
pub struct Progress {
    pub done: AtomicU64,
    pub total: AtomicU64,
    pub cancel: AtomicBool,
    /// Files of this transfer waiting for other transfers on the connection to make room.
    pub waiting: AtomicUsize,
}

impl Progress {
    fn check(&self) -> Result<(), OpError> {
        if self.cancel.load(Ordering::Relaxed) { Err(OpError::cancelled()) } else { Ok(()) }
    }

    fn add(&self, n: u64) {
        self.done.fetch_add(n, Ordering::Relaxed);
    }
}

// ---- the connection -------------------------------------------------------------------

/// An SFTP channel on a Session's connection, plus who is signed in.
pub struct Remote {
    sftp: SftpSession,
    handle: Arc<Handle<Client>>,
    pub identity: Identity,
    /// The folder SFTP starts in (the user's home).
    pub home: String,
    /// Remote paths an upload on this connection is writing to right now.
    uploading: Mutex<HashSet<String>>,
    /// What the transfers on this connection may have in flight together (KiB permits).
    budget: tokio::sync::Semaphore,
    /// Files the transfers on this connection may have open together.
    slots: tokio::sync::Semaphore,
}

/// A file's turn among the connection's transfers, until dropped.
struct Room<'a> {
    _slot: tokio::sync::SemaphorePermit<'a>,
    _budget: tokio::sync::SemaphorePermit<'a>,
}

/// A remote path claimed by one upload until dropped, so a second upload to the same place
/// can't take over (and delete) the first one's partial file.
struct UploadClaim<'a> {
    set: &'a Mutex<HashSet<String>>,
    path: String,
}

impl Drop for UploadClaim<'_> {
    fn drop(&mut self) {
        self.set.lock().unwrap_or_else(|e| e.into_inner()).remove(&self.path);
    }
}

pub struct ExecOutput {
    pub code: Option<u32>,
    pub stdout: String,
    pub stderr: String,
}

impl Remote {
    pub(crate) async fn open(handle: Arc<Handle<Client>>) -> Result<Remote> {
        let channel = handle.channel_open_session().await.map_err(|e| anyhow!("Could not open a channel: {e}"))?;
        channel.request_subsystem(true, "sftp").await.map_err(|e| anyhow!("Could not start SFTP: {e}"))?;
        let config = russh_sftp::client::Config {
            request_timeout_secs: REQUEST_TIMEOUT_SECS,
            max_concurrent_reads: PIPELINE,
            max_concurrent_writes: PIPELINE,
            // As large as the server allows (OpenSSH: 255 KiB); the default 32 KiB needs 8x
            // the requests for the same data.
            max_write_packet_len: (REQUEST_KIB * 1024) as u32,
            ..Default::default()
        };
        let sftp = tokio::time::timeout(Duration::from_secs(20), SftpSession::new_with_config(channel.into_stream(), config))
            .await
            .map_err(|_| anyhow!("The server did not start SFTP. It may not offer it."))?
            .map_err(|e| anyhow!("Could not start SFTP: {e}"))?;
        let home = sftp.canonicalize(".").await.unwrap_or_else(|_| "/".into());
        let mut remote = Remote { sftp, handle, identity: Identity::default(), home, uploading: Mutex::default(), budget: tokio::sync::Semaphore::new(BUDGET_KIB as usize),
            slots: tokio::sync::Semaphore::new(PARALLEL),
        };
        // Who we are decides which files look inaccessible. Without it nothing is marked.
        if let Ok(Ok(out)) = tokio::time::timeout(Duration::from_secs(10), remote.exec("id")).await
            && out.code == Some(0)
        {
            remote.identity = Identity::parse_id(&out.stdout);
        }
        Ok(remote)
    }

    /// Run a command on the server (for what SFTP can't do: copying).
    pub async fn exec(&self, command: &str) -> Result<ExecOutput, OpError> {
        let mut channel = self.handle.channel_open_session().await.map_err(|_| OpError::connection())?;
        channel.exec(true, command).await.map_err(|_| OpError::connection())?;
        let (mut stdout, mut stderr, mut code) = (Vec::new(), Vec::new(), None);
        loop {
            match channel.wait().await {
                Some(ChannelMsg::Data { data }) => stdout.extend_from_slice(&data),
                Some(ChannelMsg::ExtendedData { data, .. }) => stderr.extend_from_slice(&data),
                Some(ChannelMsg::ExitStatus { exit_status }) => code = Some(exit_status),
                Some(ChannelMsg::Failure) => {
                    return Err(OpError::new(ErrorKind::Unsupported, "The server doesn't allow running commands"));
                }
                Some(ChannelMsg::Close) | None => break,
                Some(_) => {}
            }
        }
        Ok(ExecOutput {
            code,
            stdout: String::from_utf8_lossy(&stdout).into_owned(),
            stderr: String::from_utf8_lossy(&stderr).into_owned(),
        })
    }

    pub async fn canonicalize(&self, path: &str) -> Result<String, OpError> {
        self.sftp.canonicalize(path).await.map_err(|e| OpError::from(e).at(path))
    }

    pub async fn exists(&self, path: &str) -> Result<bool, OpError> {
        match self.sftp.symlink_metadata(path).await {
            Ok(_) => Ok(true),
            Err(SftpError::Status(s)) if s.status_code == StatusCode::NoSuchFile => Ok(false),
            Err(e) => Err(OpError::from(e).at(path)),
        }
    }

    /// List a folder. Symlinks are followed to show what they point at.
    pub async fn list(&self, path: &str) -> Result<Listing, OpError> {
        let path = self.canonicalize(path).await?;
        let attrs = self.sftp.metadata(path.as_str()).await.map_err(|e| OpError::from(e).at(&path))?;
        if !is_dir_mode(attrs.permissions) && attrs.permissions.is_some() {
            return Err(OpError::new(ErrorKind::Other, format!("Not a folder: {path}")));
        }
        let dir_access = access(&self.identity, attrs.permissions, attrs.uid, attrs.gid, true);
        let read = self.sftp.read_dir(path.as_str()).await.map_err(|e| OpError::from(e).at(&path))?;
        let raw: Vec<(String, FileAttributes)> =
            read.filter(|e| e.file_name() != "." && e.file_name() != "..").map(|e| (e.file_name(), e.metadata())).collect();
        // Look up where symlinks lead, a few at a time.
        let lookups: Vec<(String, bool)> =
            raw.iter().map(|(name, attrs)| (join(&path, name), is_link_mode(attrs.permissions))).collect();
        let session = &self.sftp;
        let targets: Vec<Option<FileAttributes>> = futures::stream::iter(lookups)
            .map(|(full, link)| async move { if link { session.metadata(full).await.ok() } else { None } })
            .buffered(32)
        .collect()
        .await;
        let entries = raw
            .into_iter()
            .zip(targets)
            .map(|((name, attrs), target)| Entry::from_attrs(&path, name, &attrs, target.as_ref(), &self.identity))
            .collect();
        Ok(Listing { path, entries, access: dir_access })
    }

    pub async fn mkdir(&self, path: &str) -> Result<(), OpError> {
        if self.exists(path).await? {
            return Err(OpError::new(ErrorKind::Exists, format!("\"{}\" already exists", file_name(path))));
        }
        self.sftp.create_dir(path).await.map_err(|e| OpError::from(e).at(path))
    }

    /// Rename or move. Fails rather than replace an existing file.
    pub async fn rename(&self, from: &str, to: &str) -> Result<(), OpError> {
        if from == to {
            return Ok(());
        }
        if self.exists(to).await? {
            return Err(OpError::new(ErrorKind::Exists, format!("\"{}\" already exists in {}", file_name(to), parent(to).unwrap_or_default())));
        }
        match self.sftp.rename(from, to).await {
            Ok(()) => Ok(()),
            Err(err) => {
                let err = OpError::from(err);
                // Across file systems the server can't rename; `mv` copies instead.
                if err.kind == ErrorKind::Other {
                    let out = self.exec(&format!("mv -- {} {}", shell_quote(from), shell_quote(to))).await?;
                    if out.code == Some(0) {
                        return Ok(());
                    }
                    return Err(OpError::from_message(&out.stderr).at(from));
                }
                Err(err.at(from))
            }
        }
    }

    /// Delete a file, or a folder with everything in it. Symlinks are removed, never followed.
    pub async fn remove(&self, path: &str) -> Result<(), OpError> {
        let attrs = self.sftp.symlink_metadata(path).await.map_err(|e| OpError::from(e).at(path))?;
        if !is_dir_mode(attrs.permissions) {
            return self.sftp.remove_file(path).await.map_err(|e| OpError::from(e).at(path));
        }
        // A level of folders at a time: list them, delete their files as they are found. Then
        // the folders themselves, deepest level first.
        let mut levels = vec![vec![path.to_string()]];
        loop {
            let level = levels.last().expect("starts with one");
            let below: Vec<Vec<String>> = futures::stream::iter(level.clone())
                .map(|dir| async move {
                    let read = self.read_dir_in_turn(&dir).await?;
                    let (mut folders, mut files) = (vec![], vec![]);
                    for entry in read {
                        let name = entry.file_name();
                        if name == "." || name == ".." {
                            continue;
                        }
                        let child = join(&dir, &name);
                        if is_dir_mode(entry.metadata().permissions) { folders.push(child) } else { files.push(child) }
                    }
                    futures::stream::iter(files)
                        .map(|file| async move { self.sftp.remove_file(file.as_str()).await.map_err(|e| OpError::from(e).at(&file)) })
                        .buffer_unordered(PARALLEL)
                        .try_collect::<Vec<()>>()
                        .await?;
                    Ok::<_, OpError>(folders)
                })
                .buffer_unordered(8)
                .try_collect()
                .await?;
            let below: Vec<String> = below.into_iter().flatten().collect();
            if below.is_empty() {
                break;
            }
            levels.push(below);
        }
        for level in levels.into_iter().rev() {
            futures::stream::iter(level)
                .map(|dir| async move { self.sftp.remove_dir(dir.as_str()).await.map_err(|e| OpError::from(e).at(&dir)) })
                .buffer_unordered(PARALLEL)
                .try_collect::<Vec<()>>()
                .await?;
        }
        Ok(())
    }

    /// Copy a file or folder to `to` on the server. Uses `cp` there; without a shell, the data
    /// goes through this connection instead.
    pub async fn copy(&self, from: &str, to: &str) -> Result<(), OpError> {
        if self.exists(to).await? {
            return Err(OpError::new(ErrorKind::Exists, format!("\"{}\" already exists in {}", file_name(to), parent(to).unwrap_or_default())));
        }
        match self.exec(&format!("cp -Rp -- {} {}", shell_quote(from), shell_quote(to))).await {
            Ok(out) if out.code == Some(0) => Ok(()),
            // No cp on the server.
            Ok(out) if out.code == Some(127) => self.copy_through(from, to).await,
            Ok(out) => Err(OpError::from_message(out.stderr.lines().next().unwrap_or_default()).at(from)),
            Err(err) if err.kind == ErrorKind::Unsupported => self.copy_through(from, to).await,
            Err(err) => Err(err),
        }
    }

    async fn copy_through(&self, from: &str, to: &str) -> Result<(), OpError> {
        let mut stack = vec![(from.to_string(), to.to_string())];
        while let Some((src, dst)) = stack.pop() {
            let attrs = self.sftp.metadata(src.as_str()).await.map_err(|e| OpError::from(e).at(&src))?;
            if is_dir_mode(attrs.permissions) {
                self.sftp.create_dir(dst.as_str()).await.map_err(|e| OpError::from(e).at(&dst))?;
                let read = self.sftp.read_dir(src.as_str()).await.map_err(|e| OpError::from(e).at(&src))?;
                for entry in read {
                    let name = entry.file_name();
                    if name == "." || name == ".." {
                        continue;
                    }
                    let child = join(&src, &name);
                    // Links to folders are skipped (they can loop), as in `walk_dir`.
                    if is_link_mode(entry.metadata().permissions)
                        && self.sftp.metadata(child.as_str()).await.is_ok_and(|t| is_dir_mode(t.permissions))
                    {
                        continue;
                    }
                    stack.push((child, join(&dst, &name)));
                }
            } else {
                let mut input = self.sftp.open(src.as_str()).await.map_err(|e| OpError::from(e).at(&src))?;
                let mut output = self.create_file(&dst).await?;
                let mut buf = vec![0u8; CHUNK];
                loop {
                    let n = input.read(&mut buf).await.map_err(|e| OpError::remote_io(&e).at(&src))?;
                    if n == 0 {
                        break;
                    }
                    output.write_all(&buf[..n]).await.map_err(|e| OpError::remote_io(&e).at(&dst))?;
                }
                output.shutdown().await.map_err(|e| OpError::remote_io(&e).at(&dst))?;
                if let Some(mode) = attrs.permissions {
                    let _ = self.set_mode(&dst, mode).await;
                }
            }
        }
        Ok(())
    }

    async fn create_file(&self, path: &str) -> Result<russh_sftp::client::fs::File, OpError> {
        self.sftp
            .open_with_flags(path, OpenFlags::CREATE | OpenFlags::TRUNCATE | OpenFlags::WRITE)
            .await
            .map_err(|e| OpError::from(e).at(path))
    }

    /// Room for a file of `size` among the connection's transfers: a slot to open it and its
    /// share of the budget. Waits while others use them (shown as waiting), and gives up when
    /// the transfer is cancelled meanwhile.
    async fn take_room(&self, size: u64, progress: &Progress) -> Result<Room<'_>, OpError> {
        let kib = budget_kib(size);
        if let Ok(slot) = self.slots.try_acquire()
            && let Ok(budget) = self.budget.try_acquire_many(kib)
        {
            return Ok(Room { _slot: slot, _budget: budget });
        }
        progress.waiting.fetch_add(1, Ordering::Relaxed);
        let acquire = async {
            let slot = self.slots.acquire().await.expect("never closed");
            let budget = self.budget.acquire_many(kib).await.expect("never closed");
            Room { _slot: slot, _budget: budget }
        };
        tokio::pin!(acquire);
        let result = loop {
            tokio::select! {
                room = &mut acquire => break Ok(room),
                _ = tokio::time::sleep(Duration::from_millis(100)) => {
                    if let Err(err) = progress.check() {
                        break Err(err);
                    }
                }
            }
        };
        progress.waiting.fetch_sub(1, Ordering::Relaxed);
        result
    }

    /// List a folder for a transfer or delete, taking one of the connection's open-file slots
    /// for it (a listing holds the folder open on the server).
    async fn read_dir_in_turn(&self, dir: &str) -> Result<russh_sftp::client::fs::ReadDir, OpError> {
        let _slot = self.slots.acquire().await.expect("never closed");
        self.sftp.read_dir(dir).await.map_err(|e| OpError::from(e).at(dir))
    }

    fn claim_upload(&self, target: &str, name: &str, remote_dir: &str) -> Result<UploadClaim<'_>, OpError> {
        let mut set = self.uploading.lock().unwrap_or_else(|e| e.into_inner());
        if !set.insert(target.to_string()) {
            return Err(OpError::new(ErrorKind::Exists, format!("\"{name}\" is already being uploaded to {remote_dir}")));
        }
        Ok(UploadClaim { set: &self.uploading, path: target.to_string() })
    }

    async fn set_mode(&self, path: &str, mode: u32) -> Result<(), OpError> {
        let mut attrs = FileAttributes::empty();
        attrs.permissions = Some(mode & 0o7777);
        self.sftp.set_metadata(path, attrs).await.map_err(OpError::from)
    }

    // ---- downloads ----------------------------------------------------------------

    /// Download a file or folder into `local_dir`. Returns where it was saved: under its own
    /// name, or "name (1)" when that is taken, so nothing local is overwritten.
    pub async fn download(&self, remote_path: &str, local_dir: &Path, progress: &Progress) -> Result<PathBuf, OpError> {
        let attrs = self.sftp.metadata(remote_path).await.map_err(|e| OpError::from(e).at(remote_path))?;
        let name = local_file_name(file_name(remote_path));
        if is_dir_mode(attrs.permissions) {
            return self.download_dir(remote_path, local_dir, &name, progress).await;
        }
        // Wait for room among the connection's other transfers before opening anything.
        let _room = self.take_room(attrs.size.unwrap_or(0), progress).await?;
        // Open the remote file before creating anything here: no access, no file.
        let mut input = self.sftp.open(remote_path).await.map_err(|e| OpError::from(e).at(remote_path))?;
        progress.total.store(attrs.size.unwrap_or(0), Ordering::Relaxed);
        tokio::fs::create_dir_all(local_dir).await.map_err(|e| OpError::local(&e, local_dir))?;
        let (target, partial, output) = reserve_local(local_dir, &name, false).await?;
        let result = async {
            let mut output = output.expect("a file was reserved");
            copy_down(&mut input, &mut output, remote_path, &partial, progress).await?;
            output.sync_all().await.map_err(|e| OpError::local(&e, &partial))?;
            drop(output);
            finish_local(&partial, &target).await
        }
        .await;
        if result.is_err() {
            let _ = tokio::fs::remove_file(&partial).await;
        }
        result
    }

    async fn download_dir(&self, remote_path: &str, local_dir: &Path, name: &str, progress: &Progress) -> Result<PathBuf, OpError> {
        // Walk first: an unreadable folder fails before anything is written, and the
        // progress bar knows the total. A level of folders at a time, listed together.
        let mut files: Vec<(String, u64)> = vec![];
        let mut dirs = vec![String::new()];
        let mut level = vec![String::new()];
        while !level.is_empty() {
            progress.check()?;
            let listed: Vec<Vec<(String, FileAttributes)>> = futures::stream::iter(level)
                .map(|rel| self.walk_dir(remote_path, rel))
                .buffer_unordered(PARALLEL)
                .try_collect()
                .await?;
            level = vec![];
            for (child_rel, attrs) in listed.into_iter().flatten() {
                if is_dir_mode(attrs.permissions) {
                    dirs.push(child_rel.clone());
                    level.push(child_rel);
                } else if attrs.permissions.is_none_or(|m| m & S_IFMT == S_IFREG) {
                    files.push((child_rel, attrs.size.unwrap_or(0)));
                }
            }
        }
        progress.total.store(files.iter().map(|(_, s)| s).sum(), Ordering::Relaxed);
        tokio::fs::create_dir_all(local_dir).await.map_err(|e| OpError::local(&e, local_dir))?;
        // Named up front, not as the files arrive: they download side by side.
        let (local_dirs, local_files) = map_download_names(&dirs, &files);
        let (target, partial, _) = reserve_local(local_dir, name, true).await?;
        let result = async {
            create_download_dirs(&partial, &local_dirs).await?;
            let partial = &partial;
            let jobs: Vec<_> = local_files
                .iter()
                .map(|(rel, local_rel, size)| async move {
                    let _room = self.take_room(*size, progress).await?;
                    progress.check()?;
                    let remote = join(remote_path, rel);
                    let local = join_rel(partial, local_rel);
                    let mut input = self.sftp.open(remote.as_str()).await.map_err(|e| OpError::from(e).at(&remote))?;
                    let mut output = create_download_file(&local).await?;
                    copy_down(&mut input, &mut output, &remote, &local, progress).await?;
                    // Written and closed before the folder is renamed (Windows can't rename a
                    // folder with an open file in it). Not synced to disk one by one: with
                    // thousands of small files that costs more than the transfer.
                    output.flush().await.map_err(|e| OpError::local(&e, &local))?;
                    Ok(())
                })
                .collect();
            all(jobs, PARALLEL).await?;
            finish_local(partial, &target).await
        }
        .await;
        if result.is_err() {
            let _ = tokio::fs::remove_dir_all(&partial).await;
        }
        result
    }

    /// One folder of a download: its entries as (path relative to `root`, attributes), links
    /// to files followed, links to folders skipped (they can loop).
    async fn walk_dir(&self, root: &str, rel: String) -> Result<Vec<(String, FileAttributes)>, OpError> {
        let dir = if rel.is_empty() { root.to_string() } else { join(root, &rel) };
        let read = self.read_dir_in_turn(&dir).await?;
        let mut out = vec![];
        for entry in read {
            let child_name = entry.file_name();
            if child_name == "." || child_name == ".." {
                continue;
            }
            let child_rel = if rel.is_empty() { child_name.clone() } else { format!("{rel}/{child_name}") };
            let mut attrs = entry.metadata();
            if is_link_mode(attrs.permissions) {
                match self.sftp.metadata(join(&dir, &child_name)).await {
                    Ok(target) if !is_dir_mode(target.permissions) => attrs = target,
                    _ => continue,
                }
            }
            out.push((child_rel, attrs));
        }
        Ok(out)
    }

    // ---- uploads ------------------------------------------------------------------

    /// Upload a local file or folder into `remote_dir` as `name`. With `replace`, an existing
    /// file or folder of that name is replaced once the upload is complete; otherwise an
    /// existing one fails the upload.
    pub async fn upload(&self, local: &Path, remote_dir: &str, name: &str, replace: bool, progress: &Progress) -> Result<String, OpError> {
        // Claimed before anything else, so of two uploads to the same place the first wins.
        let target = join(remote_dir, name);
        let _claim = self.claim_upload(&target, name, remote_dir)?;
        // Off the SSH runtime's few threads: walking a large folder blocks for a while, and
        // every terminal on the runtime would freeze meanwhile.
        let walk = local.to_path_buf();
        let plan = tokio::task::spawn_blocking(move || plan_local(&walk))
            .await
            .map_err(|err| OpError::new(ErrorKind::Other, format!("Stopped unexpectedly: {err}")))??;
        progress.total.store(plan.iter().map(|(_, size)| size.unwrap_or(0)).sum(), Ordering::Relaxed);
        let existing = match self.sftp.symlink_metadata(target.as_str()).await {
            Ok(attrs) => Some(attrs),
            Err(SftpError::Status(s)) if s.status_code == StatusCode::NoSuchFile => None,
            Err(e) => return Err(OpError::from(e).at(&target)),
        };
        if existing.is_some() && !replace {
            return Err(OpError::new(ErrorKind::Exists, format!("\"{name}\" already exists in {remote_dir}")));
        }
        // Left over from an upload that was interrupted (no upload here is using it: claimed).
        let partial = join(remote_dir, &format!(".{name}{PARTIAL_SUFFIX}"));
        if self.exists(&partial).await? {
            self.remove(&partial).await?;
        }
        let remote_of = |rel: &str| if rel.is_empty() { partial.clone() } else { join(&partial, rel) };
        let result = async {
            // Folders first, a level at a time (each needs its parent), then the files.
            let mut folders: Vec<&str> = plan.iter().filter(|(_, size)| size.is_none()).map(|(rel, _)| rel.as_str()).collect();
            folders.sort_by_key(|rel| depth(rel));
            for level in folders.chunk_by(|a, b| depth(a) == depth(b)) {
                progress.check()?;
                let jobs: Vec<_> = level
                    .iter()
                    .map(|rel| {
                        let remote = remote_of(rel);
                        async move { self.sftp.create_dir(remote.as_str()).await.map_err(|e| OpError::from(e).at(&remote)) }
                    })
                    .collect();
                all(jobs, PARALLEL).await?;
            }
            let jobs: Vec<_> = plan
                .iter()
                .filter_map(|(rel, size)| size.map(|size| (rel, size)))
                .map(|(rel, size)| {
                    let remote = remote_of(rel);
                    async move {
                        let _room = self.take_room(size, progress).await?;
                        progress.check()?;
                        // The real local name: only download targets get Windows-safe names.
                        let local_path = join_rel(local, rel);
                        let mut input = tokio::fs::File::open(&local_path).await.map_err(|e| OpError::local(&e, &local_path))?;
                        let mut output = self.create_file(&remote).await.map_err(|e| e.at(remote_dir))?;
                        copy_up(&mut input, &mut output, &local_path, progress).await?;
                        output.shutdown().await.map_err(|e| OpError::remote_io(&e))?;
                        Ok(())
                    }
                })
                .collect();
            all(jobs, PARALLEL).await?;
            progress.check()?;
            // A replaced file keeps its permissions (an executable script stays executable).
            if let Some(attrs) = &existing {
                if !is_dir_mode(attrs.permissions) && !is_link_mode(attrs.permissions) && plan.len() == 1 {
                    if let Some(mode) = attrs.permissions {
                        let _ = self.set_mode(&partial, mode).await;
                    }
                }
                self.remove(&target).await?;
            }
            self.sftp.rename(partial.as_str(), target.as_str()).await.map_err(|e| OpError::from(e).at(&target))?;
            Ok(target.clone())
        }
        .await;
        if result.is_err() {
            let _ = self.remove(&partial).await;
        }
        result
    }
}

/// The partial file a download is written to: "name.ext.komando-part" next to `target`.
fn partial_path(target: &Path) -> PathBuf {
    let mut name = target.file_name().map(|n| n.to_os_string()).unwrap_or_default();
    name.push(PARTIAL_SUFFIX);
    target.with_file_name(name)
}

// If a reservation future is dropped while the blocking job runs, its result still
// cleans up the claimed partial. Completed reservations are handed to the transfer.
struct LocalReservation {
    target: PathBuf,
    partial: PathBuf,
    file: Option<std::fs::File>,
    folder: bool,
    handed_off: bool,
}

impl Drop for LocalReservation {
    fn drop(&mut self) {
        if !self.handed_off {
            drop(self.file.take());
            if self.folder {
                let _ = std::fs::remove_dir(&self.partial);
            } else {
                let _ = std::fs::remove_file(&self.partial);
            }
        }
    }
}

/// Claim a free name in `dir` for a download: create its partial file (or folder) so that no
/// other download can take the same name, even one started at the same moment. Returns the
/// final path, the partial path, and the partial file when it is one.
async fn reserve_local(dir: &Path, name: &str, folder: bool) -> Result<(PathBuf, PathBuf, Option<tokio::fs::File>), OpError> {
    let (dir, name) = (dir.to_path_buf(), name.to_string());
    // Filesystem enumeration and exclusive creation must not block the SSH executor.
    let mut reserved = tokio::task::spawn_blocking(move || {
        let claim = |candidate: &str| -> std::io::Result<LocalReservation> {
            let target = dir.join(candidate);
            if target.try_exists()? {
                return Err(std::io::ErrorKind::AlreadyExists.into());
            }
            let partial = partial_path(&target);
            let file = if folder {
                std::fs::create_dir(&partial).map(|()| None)
            } else {
                std::fs::OpenOptions::new().write(true).create_new(true).open(&partial).map(Some)
            }.map_err(|error| {
                // Windows reports AccessDenied when CREATE_NEW races with a directory
                // claiming the partial path. Confirm the collision before choosing a new
                // name; genuine permission errors must still reach the user.
                if error.kind() == std::io::ErrorKind::PermissionDenied && partial.try_exists().unwrap_or(false) {
                    std::io::Error::from(std::io::ErrorKind::AlreadyExists)
                } else {
                    error
                }
            })?;
            Ok(LocalReservation { target, partial, file, folder, handed_off: false })
        };
        // Most names are unused: don't enumerate a potentially huge folder for those.
        match claim(&name) {
            Ok(reserved) => return Ok(reserved),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(e) => return Err(OpError::local(&e, &dir.join(&name))),
        }
        // One directory snapshot instead of two metadata calls per existing name on
        // every retry. Exclusive creation below still handles simultaneous downloads.
        let mut taken: HashSet<_> = std::fs::read_dir(&dir)
            .map_err(|e| OpError::local(&e, &dir))?
            .map(|entry| entry.map(|entry| entry.file_name()))
            .collect::<std::io::Result<_>>()
            .map_err(|e| OpError::local(&e, &dir))?;
        loop {
            let candidate = unique_name(&name, |n| {
                taken.contains(std::ffi::OsStr::new(n)) || taken.contains(std::ffi::OsStr::new(&format!("{n}{PARTIAL_SUFFIX}")))
            });
            match claim(&candidate) {
                Ok(reserved) => return Ok(reserved),
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                    taken.insert(std::ffi::OsString::from(candidate));
                }
                Err(e) => return Err(OpError::local(&e, &dir.join(candidate))),
            }
        }
    }).await.map_err(|e| OpError::new(ErrorKind::Other, e.to_string()))??;
    reserved.handed_off = true;
    Ok((reserved.target.clone(), reserved.partial.clone(), reserved.file.take().map(tokio::fs::File::from_std)))
}

/// Run `jobs`, up to `at_once` at a time; the first error stops the rest.
async fn all<F: Future<Output = Result<(), OpError>>>(jobs: Vec<F>, at_once: usize) -> Result<(), OpError> {
    futures::stream::iter(jobs).buffer_unordered(at_once).try_collect::<Vec<()>>().await.map(drop)
}

/// How deep a relative path is: 0 for the top ("").
fn depth(rel: &str) -> usize {
    if rel.is_empty() { 0 } else { rel.matches('/').count() + 1 }
}

/// A file's share of the connection's budget: its size, up to what one file can have in
/// flight at once, and at least 1 KiB.
fn budget_kib(size: u64) -> u32 {
    let in_flight = (PIPELINE * REQUEST_KIB) as u64;
    (size / 1024).clamp(1, in_flight.min(BUDGET_KIB as u64)) as u32
}

/// `rel` ("a/b", or "" for `base` itself) below `base`, with its names as they are: a local
/// source for an upload, or a download target already named by `map_download_names`.
fn join_rel(base: &Path, rel: &str) -> PathBuf {
    let mut path = base.to_path_buf();
    path.extend(rel.split('/').filter(|p| !p.is_empty()));
    path
}

async fn create_download_dirs(base: &Path, dirs: &[String]) -> Result<(), OpError> {
    // The root is already reserved. Every other directory must be new. The names are made
    // distinct by `map_download_names`; this is the backstop should that ever miss one.
    for rel in dirs.iter().filter(|rel| !rel.is_empty()) {
        let dir = join_rel(base, rel);
        tokio::fs::create_dir(&dir).await.map_err(|e| download_local_error(&e, &dir))?;
    }
    Ok(())
}

async fn create_download_file(path: &Path) -> Result<tokio::fs::File, OpError> {
    // Exclusive creation is the backstop behind `map_download_names`: fail the whole
    // transfer (whose partial is removed) instead of silently truncating a sibling file.
    tokio::fs::OpenOptions::new().write(true).create_new(true).open(path).await.map_err(|e| download_local_error(&e, path))
}

fn download_local_error(err: &std::io::Error, path: &Path) -> OpError {
    if err.kind() == std::io::ErrorKind::AlreadyExists {
        OpError::new(ErrorKind::Local, format!("Remote names map to the same Windows path; download cancelled: {}", path.display()))
    } else {
        OpError::local(err, path)
    }
}

/// Move a finished partial file or folder into place, under a free name if `target` was
/// taken meanwhile.
async fn finish_local(partial: &Path, target: &Path) -> Result<PathBuf, OpError> {
    let (partial, mut target) = (partial.to_path_buf(), target.to_path_buf());
    // Blocking: a scanner holding the new file can make the rename retry for a few seconds,
    // and the download must not be thrown away for that.
    tokio::task::spawn_blocking(move || {
        if target.exists() {
            let dir = target.parent().map(Path::to_path_buf).unwrap_or_default();
            let name = target.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            target = dir.join(unique_name(&name, |n| dir.join(n).exists()));
        }
        crate::model::rename_with_retries(&partial, &target).map_err(|e| OpError::local(&e, &target))?;
        Ok(target)
    })
    .await
    .map_err(|e| OpError::new(ErrorKind::Other, e.to_string()))?
}

async fn copy_down(
    input: &mut russh_sftp::client::fs::File,
    output: &mut tokio::fs::File,
    remote: &str,
    local: &Path,
    progress: &Progress,
) -> Result<(), OpError> {
    let mut buf = vec![0u8; CHUNK];
    loop {
        progress.check()?;
        let n = input.read(&mut buf).await.map_err(|e| OpError::remote_io(&e).at(remote))?;
        if n == 0 {
            return Ok(());
        }
        output.write_all(&buf[..n]).await.map_err(|e| OpError::local(&e, local))?;
        progress.add(n as u64);
    }
}

async fn copy_up(
    input: &mut tokio::fs::File,
    output: &mut russh_sftp::client::fs::File,
    local: &Path,
    progress: &Progress,
) -> Result<(), OpError> {
    let mut buf = vec![0u8; CHUNK];
    loop {
        progress.check()?;
        let n = input.read(&mut buf).await.map_err(|e| OpError::local(&e, local))?;
        if n == 0 {
            return Ok(());
        }
        output.write_all(&buf[..n]).await.map_err(|e| OpError::remote_io(&e))?;
        progress.add(n as u64);
    }
}

/// What an upload of `local` consists of: ("" for the top, relative paths below it), with
/// `None` for folders and the size for files. Folders come before their contents.
fn plan_local(local: &Path) -> Result<Vec<(String, Option<u64>)>, OpError> {
    let meta = std::fs::metadata(local).map_err(|e| OpError::local(&e, local))?;
    if !meta.is_dir() {
        return Ok(vec![(String::new(), Some(meta.len()))]);
    }
    let mut plan = vec![(String::new(), None)];
    let mut queue = vec![String::new()];
    while let Some(rel) = queue.pop() {
        let dir = join_rel(local, &rel);
        let read = match std::fs::read_dir(&dir) {
            Ok(read) => read,
            // Only the folder itself has to be readable: one that isn't is uploaded empty.
            Err(e) if !rel.is_empty() => {
                log::warn!("Not uploading the contents of {}: {e}", dir.display());
                continue;
            }
            Err(e) => return Err(OpError::local(&e, &dir)),
        };
        let mut children: Vec<_> = read
            .filter_map(|child| child.map_err(|e| log::warn!("Skipping an entry of {}: {e}", dir.display())).ok())
            .collect();
        children.sort_by_key(|e| e.file_name());
        for child in children {
            let path = child.path();
            let Ok(name) = child.file_name().into_string() else {
                log::warn!("Skipping {}: its name isn't Unicode, which SFTP needs", path.display());
                continue;
            };
            let child_rel = if rel.is_empty() { name } else { format!("{rel}/{name}") };
            // Windows junctions (mount points) are reported as links here, like symlinks: std
            // counts every reparse tag with the name-surrogate bit, which both have. Other
            // reparse points (OneDrive placeholders, deduplicated files) are real files.
            let is_link = match child.file_type() {
                Ok(kind) => kind.is_symlink(),
                Err(e) => {
                    log::warn!("Skipping {}: {e}", path.display());
                    continue;
                }
            };
            // Followed, so a link to a file uploads the file. A dangling link or an entry
            // that vanished or can't be read doesn't fail the folder.
            let meta = match std::fs::metadata(&path) {
                Ok(meta) => meta,
                Err(e) => {
                    log::warn!("Skipping {}: {e}", path.display());
                    continue;
                }
            };
            // Links to folders are skipped (they can loop), as in `download_dir`.
            if meta.is_dir() && !is_link {
                plan.push((child_rel.clone(), None));
                queue.push(child_rel);
            } else if meta.is_file() {
                plan.push((child_rel, Some(meta.len())));
            }
        }
    }
    Ok(plan)
}

/// "2.1 KB": sizes as file managers show them (1 KB = 1024 bytes).
pub fn format_size(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    if bytes < 1024 {
        return format!("{bytes} B");
    }
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024. && unit < UNITS.len() - 1 {
        value /= 1024.;
        unit += 1;
    }
    if value < 10. { format!("{value:.1} {}", UNITS[unit]) } else { format!("{value:.0} {}", UNITS[unit]) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn id_output_is_parsed() {
        let id = Identity::parse_id("uid=1000(codex) gid=1000(codex) groups=1000(codex),27(sudo),998(docker)\n");
        assert_eq!(id.uid, Some(1000));
        assert_eq!(id.name.as_deref(), Some("codex"));
        assert_eq!(id.groups.iter().map(|(g, _)| *g).collect::<Vec<_>>(), [1000, 27, 998]);
        assert_eq!(id.group_name(27), Some("sudo"));
        // busybox and some systems print no names.
        let id = Identity::parse_id("uid=0 gid=0 groups=0,10");
        assert_eq!(id.uid, Some(0));
        assert_eq!(id.groups.len(), 2);
        assert_eq!(Identity::parse_id("garbage").uid, None);
    }

    fn user() -> Identity {
        Identity::parse_id("uid=1000(codex) gid=1000(codex) groups=1000(codex),27(sudo)")
    }

    #[test]
    fn access_follows_the_class_that_applies() {
        let me = user();
        // Owner: owner bits only.
        let a = access(&me, Some(0o100600), Some(1000), Some(0), false);
        assert_eq!((a.read, a.write, a.exec), (Allowed::Yes, Allowed::Yes, Allowed::No));
        // Group member: group bits.
        let a = access(&me, Some(0o100640), Some(0), Some(27), false);
        assert_eq!((a.read, a.write), (Allowed::Yes, Allowed::No));
        // Others: other bits, even when the group bits would allow it.
        let a = access(&me, Some(0o040770), Some(0), Some(0), true);
        assert_eq!((a.read, a.exec), (Allowed::No, Allowed::No));
        // root may do anything but execute a file without x bits.
        let root = Identity::parse_id("uid=0(root) gid=0(root) groups=0(root)");
        let a = access(&root, Some(0o100600), Some(1000), Some(1000), false);
        assert_eq!((a.read, a.write, a.exec), (Allowed::Yes, Allowed::Yes, Allowed::No));
    }

    #[test]
    fn unknown_permissions_never_lock() {
        assert_eq!(access(&Identity::default(), Some(0o100000), Some(0), Some(0), false), Access::UNKNOWN);
        assert_eq!(access(&user(), None, Some(0), Some(0), false), Access::UNKNOWN);
        assert_eq!(access(&user(), Some(0o100000), None, None, false), Access::UNKNOWN);
        let mut attrs = FileAttributes::empty();
        attrs.permissions = Some(0o100000);
        attrs.uid = Some(0);
        attrs.gid = Some(0);
        let entry = Entry::from_attrs("/", "x".into(), &attrs, None, &Identity::default());
        assert!(!entry.locked());
        let entry = Entry::from_attrs("/", "x".into(), &attrs, None, &user());
        assert!(entry.locked());
    }

    #[test]
    fn folders_need_read_and_search() {
        let mut attrs = FileAttributes::empty();
        attrs.permissions = Some(0o040751);
        attrs.uid = Some(0);
        attrs.gid = Some(0);
        // Others get --x: can pass through, not list.
        let entry = Entry::from_attrs("/srv", "storage".into(), &attrs, None, &user());
        assert!(entry.is_dir());
        assert!(entry.locked());
        assert_eq!(entry.path, "/srv/storage");
        attrs.permissions = Some(0o040755);
        assert!(!Entry::from_attrs("/srv", "storage".into(), &attrs, None, &user()).locked());
    }

    #[test]
    fn symlinks_take_their_targets_kind() {
        let mut link = FileAttributes::empty();
        link.permissions = Some(0o120777);
        let mut target = FileAttributes::empty();
        target.permissions = Some(0o040755);
        target.uid = Some(0);
        target.gid = Some(0);
        let entry = Entry::from_attrs("/", "logs".into(), &link, Some(&target), &user());
        assert!(entry.is_dir() && entry.link && !entry.broken_link);
        assert_eq!(entry.mode_string(), "lrwxrwxrwx");
        let entry = Entry::from_attrs("/", "gone".into(), &link, None, &user());
        assert!(entry.broken_link && !entry.locked());
    }

    #[test]
    fn modes_print_like_ls() {
        assert_eq!(mode_string(0o040755), "drwxr-xr-x");
        assert_eq!(mode_string(0o100644), "-rw-r--r--");
        assert_eq!(mode_string(0o104755), "-rwsr-xr-x");
        assert_eq!(mode_string(0o041777), "drwxrwxrwt");
        assert_eq!(mode_string(0o100600), "-rw-------");
    }

    #[test]
    fn remote_paths() {
        assert_eq!(join("/", "etc"), "/etc");
        assert_eq!(join("/var/www", "app"), "/var/www/app");
        assert_eq!(parent("/var/www/app").as_deref(), Some("/var/www"));
        assert_eq!(parent("/var").as_deref(), Some("/"));
        assert_eq!(parent("/"), None);
        assert_eq!(file_name("/var/log/"), "log");
        let segs = segments("/var/www");
        assert_eq!(segs, vec![("/".into(), "/".into()), ("var".into(), "/var".into()), ("www".into(), "/var/www".into())]);
        assert_eq!(shell_quote("it's"), "'it'\\''s'");
    }

    #[test]
    fn unique_names_keep_the_extension() {
        let taken = ["app.conf", "app (1).conf", "logs", ".env"];
        let is_taken = |n: &str| taken.contains(&n);
        assert_eq!(unique_name("new.txt", is_taken), "new.txt");
        assert_eq!(unique_name("app.conf", is_taken), "app (2).conf");
        assert_eq!(unique_name("logs", is_taken), "logs (1)");
        assert_eq!(unique_name(".env", is_taken), ".env (1)");
        assert_eq!(local_file_name("a:b?.txt"), "a_b_.txt");
        assert_eq!(local_file_name("trailing. "), "trailing");
    }

    #[test]
    fn large_files_share_the_connection_two_at_a_time() {
        // Small files take what they are; a large one what it can have in flight, which
        // leaves room for exactly one more.
        assert_eq!(budget_kib(0), 1);
        assert_eq!(budget_kib(10 * 1024), 10);
        let large = budget_kib(1 << 30);
        assert_eq!(large as usize, PIPELINE * REQUEST_KIB);
        assert!(2 * large <= BUDGET_KIB && 3 * large > BUDGET_KIB);
        assert_eq!(depth(""), 0);
        assert_eq!(depth("a/b"), 2);
    }

    #[test]
    fn sizes() {
        assert_eq!(format_size(0), "0 B");
        assert_eq!(format_size(836), "836 B");
        assert_eq!(format_size(2150), "2.1 KB");
        assert_eq!(format_size(5 * 1024 * 1024), "5.0 MB");
        assert_eq!(format_size(300 * 1024 * 1024), "300 MB");
    }

    #[test]
    fn local_names_avoid_partial_downloads() {
        let dir = std::env::temp_dir().join(format!("komando-sftp-{}", crate::model::new_id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(format!("a.txt{PARTIAL_SUFFIX}")), b"").unwrap();
        assert_eq!(unique_local_path(&dir, "a.txt"), dir.join("a (1).txt"));
        assert_eq!(partial_path(&dir.join("a.txt")), dir.join(format!("a.txt{PARTIAL_SUFFIX}")));
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn windows_device_names_are_made_safe() {
        assert_eq!(local_file_name("con"), "con_");
        assert_eq!(local_file_name("aux.log"), "aux_.log");
        assert_eq!(local_file_name("CON.tar.gz"), "CON_.tar.gz");
        assert_eq!(local_file_name("Nul."), "Nul_");
        assert_eq!(local_file_name("com1"), "com1_");
        assert_eq!(local_file_name("LPT9.txt"), "LPT9_.txt");
        assert_eq!(local_file_name("com\u{b2}.txt"), "com\u{b2}_.txt");
        assert_eq!(local_file_name("con .txt"), "con _.txt");
        assert_eq!(local_file_name("conin$"), "conin$_");
        // Only a whole device name before the first dot is reserved.
        for fine in ["console", "com", "com0", "com10", "lpt", "a.con", "my.aux", ".con", "nulls.txt", "con_"] {
            assert_eq!(local_file_name(fine), fine);
        }
    }

    #[test]
    fn folder_downloads_name_colliding_entries_apart() {
        let dirs: Vec<String> = ["", "a:b", "a?b", "Docs", "docs", "a:b/nested", "docs/x"].map(String::from).into();
        let files: Vec<(String, u64)> =
            [("a:b.txt", 1), ("a?b.txt", 2), ("README", 3), ("readme", 4), ("foo.", 5), ("foo", 6), ("docs/con.log", 7), ("Docs/Y", 8)]
                .map(|(rel, size)| (rel.to_string(), size))
                .into();
        let (local_dirs, local_files) = map_download_names(&dirs, &files);
        assert_eq!(local_dirs, ["Docs", "a_b", "a_b (1)", "docs (1)", "a_b/nested", "docs (1)/x"]);
        let local_of = |rel: &str| local_files.iter().find(|(r, _, _)| r == rel).map(|(_, local, _)| local.as_str()).unwrap();
        assert_eq!(local_of("a:b.txt"), "a_b.txt");
        assert_eq!(local_of("a?b.txt"), "a_b (1).txt");
        assert_eq!(local_of("README"), "README");
        assert_eq!(local_of("readme"), "readme (1)");
        assert_eq!(local_of("foo"), "foo");
        assert_eq!(local_of("foo."), "foo (1)");
        assert_eq!(local_of("docs/con.log"), "docs (1)/con_.log");
        assert_eq!(local_of("Docs/Y"), "Docs/Y");
        // Nothing shares a path, ignoring case.
        let all: Vec<String> = local_dirs.iter().cloned().chain(local_files.iter().map(|(_, local, _)| local.clone())).collect();
        let distinct: HashSet<_> = all.iter().map(|path| path.to_uppercase()).collect();
        assert_eq!(distinct.len(), all.len());
        // However the walk happened to list them.
        let (mut dirs, mut files) = (dirs, files);
        dirs.reverse();
        files.reverse();
        assert_eq!(map_download_names(&dirs, &files), (local_dirs, local_files));
    }

    #[test]
    fn uploads_use_the_real_local_names() {
        let base = Path::new("base");
        // A download target is made safe; an upload source is looked up as it is.
        assert_eq!(join_rel(base, "con.log/a. "), base.join("con.log").join("a. "));
        assert_eq!(join_rel(base, ""), base);
        assert_eq!(local_file_name("con.log"), "con_.log");
        // The plan holds the names the local files really have.
        let dir = std::env::temp_dir().join(format!("komando-sftp-{}", crate::model::new_id()));
        std::fs::create_dir_all(dir.join("my docs")).unwrap();
        std::fs::write(dir.join("my docs").join("aux_.txt"), b"xy").unwrap();
        let plan = plan_local(&dir);
        std::fs::remove_dir_all(&dir).ok();
        let rels: Vec<_> = plan.unwrap().into_iter().map(|(rel, _)| rel).collect();
        assert!(rels.contains(&"my docs".to_string()) && rels.contains(&"my docs/aux_.txt".to_string()));
    }

    #[cfg(windows)]
    #[test]
    fn upload_plans_skip_junctions_and_unreadable_entries() {
        let dir = std::env::temp_dir().join(format!("komando-sftp-{}", crate::model::new_id()));
        let (real, junction) = (dir.join("real"), dir.join("junction"));
        std::fs::create_dir_all(&real).unwrap();
        std::fs::write(real.join("a.txt"), b"abc").unwrap();
        std::fs::write(dir.join("b.txt"), b"de").unwrap();
        let made = std::process::Command::new("cmd").arg("/C").arg("mklink").arg("/J").arg(&junction).arg(&real).output();
        let linked = made.is_ok_and(|out| out.status.success());
        let plan = plan_local(&dir);
        // The junction's own type, as std reports it: a link, so a loop through it can't happen.
        let reported_as_link = linked && std::fs::read_dir(&dir).unwrap().flatten().any(|e| {
            e.file_name() == "junction" && e.file_type().is_ok_and(|t| t.is_symlink())
        });
        if linked {
            std::fs::remove_dir(&junction).unwrap();
        }
        std::fs::remove_dir_all(&dir).unwrap();
        let rels: Vec<_> = plan.unwrap().into_iter().map(|(rel, _)| rel).collect();
        assert_eq!(rels, ["", "b.txt", "real", "real/a.txt"]);
        assert!(!linked || reported_as_link);
    }

    #[tokio::test]
    async fn folder_download_never_overwrites_a_sibling_file() {
        // The backstop behind `map_download_names`: even a name it missed isn't truncated.
        let dir = std::env::temp_dir().join(format!("komando-sftp-{}", crate::model::new_id()));
        std::fs::create_dir(&dir).unwrap();
        let path = join_rel(&dir, "a_b.txt");
        let mut file = create_download_file(&path).await.unwrap();
        file.write_all(b"first file").await.unwrap();
        file.flush().await.unwrap();
        drop(file);
        assert!(create_download_file(&path).await.is_err());
        assert_eq!(std::fs::read(&path).unwrap(), b"first file");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[tokio::test]
    async fn folder_download_does_not_merge_directories() {
        let dir = std::env::temp_dir().join(format!("komando-sftp-{}", crate::model::new_id()));
        std::fs::create_dir(&dir).unwrap();
        // The root has already been reserved, and parents precede their children.
        create_download_dirs(&dir, &[String::new(), "a_b".into(), "a_b/nested".into()]).await.unwrap();
        assert!(dir.join("a_b/nested").is_dir());
        assert!(create_download_dirs(&dir, &["a_b".into()]).await.is_err());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn errors_are_worded_for_people() {
        let denied = SftpError::Status(russh_sftp::protocol::Status {
            id: 1,
            status_code: StatusCode::PermissionDenied,
            error_message: "Permission denied".into(),
            language_tag: "en".into(),
        });
        let err = OpError::from(denied).at("/srv/storage/app.log");
        assert_eq!(err.kind, ErrorKind::PermissionDenied);
        assert_eq!(err.message, "Permission denied (EACCES): /srv/storage/app.log");
        assert_eq!(OpError::from(SftpError::Timeout).kind, ErrorKind::Connection);
        assert_eq!(OpError::from_message("cp: cannot open 'x': Permission denied").kind, ErrorKind::PermissionDenied);
    }
    #[test]
    #[ignore]
    fn stress_download_name_collisions() {
        for optimized in [false, true] {
            let dir = std::env::temp_dir().join(format!("komando-name-stress-{}-{optimized}", std::process::id()));
            std::fs::create_dir_all(&dir).unwrap();
            let start = std::time::Instant::now();
            futures::executor::block_on(spawn(async move {
                let mut jobs = Vec::new();
                for _ in 0..1000 {
                    let dir = dir.clone();
                    jobs.push(async move {
                        if optimized { return reserve_local(&dir, "shared.log", false).await; }
                        // The former production path, retained here for a reproducible
                        // comparison with the same filesystem, concurrency and input.
                        loop {
                            let target = unique_local_path(&dir, "shared.log");
                            let partial = partial_path(&target);
                            match tokio::fs::OpenOptions::new().write(true).create_new(true).open(&partial).await {
                                Ok(file) => return Ok((target, partial, Some(file))),
                                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
                                Err(e) => return Err(OpError::local(&e, &partial)),
                            }
                        }
                    });
                }
                let paths: Vec<_> = futures::stream::iter(jobs).buffer_unordered(32).try_collect().await.unwrap();
                let mut unique = HashSet::new();
                for (target, partial, file) in paths {
                    assert!(unique.insert(target));
                    drop(file);
                    std::fs::remove_file(partial).unwrap();
                }
                assert_eq!(unique.len(), 1000);
                std::fs::remove_dir(&dir).unwrap();
            })).unwrap();
            eprintln!("1000 colliding download reservations optimized={optimized}: {:?}", start.elapsed());
        }
    }

    #[test]
    fn concurrent_reservations_preserve_existing_files_and_partials() {
        let dir = std::env::temp_dir().join(format!("komando-reserve-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("shared.log"), b"keep final").unwrap();
        std::fs::write(dir.join("shared (1).log.komando-part"), b"keep partial").unwrap();
        futures::executor::block_on(spawn(async move {
            let jobs = (0..32).map(|i| {
                let dir = dir.clone();
                async move { reserve_local(&dir, "shared.log", i % 2 == 0).await.unwrap() }
            });
            let reservations = futures::future::join_all(jobs).await;
            let mut names = HashSet::new();
            for (target, partial, file) in reservations {
                assert!(names.insert(target.clone()));
                assert_ne!(target, dir.join("shared.log"));
                assert_ne!(target, dir.join("shared (1).log"));
                assert!(partial.exists());
                drop(file);
                if partial.is_dir() { std::fs::remove_dir(partial).unwrap(); }
                else { std::fs::remove_file(partial).unwrap(); }
            }
            assert_eq!(std::fs::read(dir.join("shared.log")).unwrap(), b"keep final");
            assert_eq!(std::fs::read(dir.join("shared (1).log.komando-part")).unwrap(), b"keep partial");
            std::fs::remove_dir_all(&dir).unwrap();
        })).unwrap();
    }

}
