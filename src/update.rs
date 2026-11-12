//! Updates from GitHub releases.
//!
//! Komando asks GitHub for the latest release of the public repository, compares its tag
//! (`vX.Y.Z`) with the running version and, on request, downloads and installs it:
//!
//! - A portable `komando.exe` in a folder the user can write to is replaced in place. Windows
//!   lets a running program's file be renamed, so the old one moves aside as `komando.exe.old`
//!   (deleted at the next start) and the new one takes its name. A restart finishes it.
//! - An installed Komando is updated by running the new MSI, which replaces the old version.
//!   Installed for all users it is under Program Files (administrator rights to change);
//!   installed for one user it is in a folder the user can write to, so the installer leaves
//!   a marker file next to the program (`MSI_MARKER`) to tell it apart from a portable copy.
//!
//! Downloads are checked against the size GitHub reports and, when GitHub lists one, the
//! asset's SHA-256 digest, before anything is replaced.

use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use anyhow::{Context as _, Result, anyhow, bail};
use serde::Deserialize;
use sha2::{Digest, Sha256};

/// The public repository releases come from.
pub const REPO: &str = "R4cc/komando";
/// The running version, from Cargo.toml.
pub const CURRENT: &str = env!("CARGO_PKG_VERSION");

const USER_AGENT: &str = concat!("Komando/", env!("CARGO_PKG_VERSION"));

/// A version as `(major, minor, patch)`.
pub type Version = (u64, u64, u64);

/// `"v0.1.12"` or `"0.1.12"` → `(0, 1, 12)`.
pub fn parse_version(text: &str) -> Option<Version> {
    let mut parts = text.trim().trim_start_matches('v').split('.');
    let major = parts.next()?.parse().ok()?;
    let minor = parts.next()?.parse().ok()?;
    // Ignore anything after the patch number ("0.2.0-beta").
    let patch: String = parts.next()?.chars().take_while(|c| c.is_ascii_digit()).collect();
    Some((major, minor, patch.parse().ok()?))
}

/// The running version. `KOMANDO_PRETEND_VERSION=0.0.1` makes Komando treat itself as older,
/// to try the update flow against a real release.
pub fn current_version() -> Version {
    std::env::var("KOMANDO_PRETEND_VERSION")
        .ok()
        .and_then(|v| parse_version(&v))
        .unwrap_or_else(|| parse_version(CURRENT).expect("Cargo.toml version"))
}

/// A file attached to a release.
#[derive(Debug, Clone, Deserialize)]
pub struct Asset {
    pub name: String,
    pub browser_download_url: String,
    pub size: u64,
    /// `"sha256:<hex>"`, listed by GitHub for assets uploaded since mid-2025.
    #[serde(default)]
    pub digest: Option<String>,
}

#[derive(Debug, Clone)]
pub struct Release {
    pub version: Version,
    pub tag: String,
    /// The release page, for "What's new".
    pub url: String,
    /// Release notes as published on GitHub. Displayed as plain text in the update tooltip.
    pub notes: String,
    /// The portable program.
    pub exe: Option<Asset>,
    /// The installer.
    pub msi: Option<Asset>,
}

impl Release {
    pub fn is_newer(&self) -> bool {
        self.version > current_version()
    }

    pub fn version_text(&self) -> String {
        let (a, b, c) = self.version;
        format!("{a}.{b}.{c}")
    }

    /// A few bounded, plain-text highlights for the sidebar tooltip. Wrapped Markdown
    /// bullets are joined; headings and GitHub's comparison-link preamble are omitted.
    pub fn highlights(&self) -> Vec<String> {
        let mut bullets: Vec<String> = Vec::new();
        let mut in_bullet = false;
        for line in self.notes.lines() {
            let line = line.trim();
            if let Some(item) = line.strip_prefix("- ").or_else(|| line.strip_prefix("* ")).or_else(|| line.strip_prefix("+ ")) {
                bullets.push(item.to_string());
                in_bullet = true;
            } else if line.is_empty() || line.starts_with('#') {
                in_bullet = false;
            } else if in_bullet && let Some(last) = bullets.last_mut() {
                last.push(' ');
                last.push_str(line);
            }
        }
        // Some releases have prose instead of a list.
        if bullets.is_empty() {
            bullets.extend(self.notes.lines().map(str::trim).filter(|s| !s.is_empty() && !s.starts_with('#')).take(2).map(str::to_owned));
        }
        static LINKS: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
        let links = LINKS.get_or_init(|| regex::Regex::new(r"\[([^\]]+)\]\([^)]+\)").unwrap());
        bullets
            .into_iter()
            .take(3)
            .map(|text| {
                let text = links.replace_all(&text, "$1").replace(['`', '*'], "");
                let mut chars = text.chars().filter(|c| !c.is_control());
                let mut short: String = chars.by_ref().take(180).collect();
                if chars.next().is_some() {
                    short.push('…');
                }
                short
            })
            .collect()
    }
}

#[derive(Deserialize)]
struct ApiRelease {
    tag_name: String,
    html_url: String,
    #[serde(default)]
    body: Option<String>,
    #[serde(default)]
    draft: bool,
    #[serde(default)]
    prerelease: bool,
    #[serde(default)]
    assets: Vec<Asset>,
}

fn agent() -> ureq::Agent {
    ureq::AgentBuilder::new()
        .timeout_connect(Duration::from_secs(10))
        .timeout_read(Duration::from_secs(30))
        .user_agent(USER_AGENT)
        .build()
}

/// The latest published release. Blocking: call it off the UI thread.
pub fn fetch_latest() -> Result<Release> {
    let url = format!("https://api.github.com/repos/{REPO}/releases/latest");
    let response = agent()
        .get(&url)
        .set("Accept", "application/vnd.github+json")
        .call()
        .map_err(|err| match err {
            ureq::Error::Status(404, _) => anyhow!("No release has been published yet."),
            ureq::Error::Status(403 | 429, _) => {
                anyhow!("GitHub limits how often it can be asked. Try again in a few minutes.")
            }
            ureq::Error::Status(code, _) => anyhow!("GitHub answered with error {code}."),
            ureq::Error::Transport(err) => anyhow!("Couldn't reach GitHub: {err}"),
        })?;
    let release: ApiRelease = serde_json::from_reader(response.into_reader()).context("Unexpected answer from GitHub")?;
    parse_release(release)
}

fn parse_release(release: ApiRelease) -> Result<Release> {
    if release.draft || release.prerelease {
        bail!("The latest release is not final yet.");
    }
    let version =
        parse_version(&release.tag_name).ok_or_else(|| anyhow!("The latest release has an unexpected tag, {}.", release.tag_name))?;
    let (a, b, c) = version;
    // Prefer the versioned file names; the unversioned copies are the same files.
    let pick = |ext: &str| {
        let versioned = format!("Komando-{a}.{b}.{c}-x64.{ext}");
        let plain = format!("Komando-x64.{ext}");
        release
            .assets
            .iter()
            .find(|asset| asset.name == versioned)
            .or_else(|| release.assets.iter().find(|asset| asset.name == plain))
            .cloned()
    };
    Ok(Release {
        version,
        tag: release.tag_name.clone(),
        url: release.html_url.clone(),
        notes: release.body.clone().unwrap_or_default(),
        exe: pick("exe"),
        msi: pick("msi"),
    })
}

/// Download `asset` to `dest`, counting bytes into `progress`. The file is written next to
/// `dest` first and only moved there once its size and digest check out.
pub fn download(asset: &Asset, dest: &Path, progress: Arc<AtomicU64>) -> Result<()> {
    let response = agent()
        .get(&asset.browser_download_url)
        .call()
        .map_err(|err| anyhow!("Couldn't download {}: {err}", asset.name))?;
    let partial = dest.with_extension("part");
    if let Some(dir) = dest.parent() {
        fs::create_dir_all(dir).with_context(|| format!("Couldn't create {}", dir.display()))?;
    }
    let mut file = fs::File::create(&partial).with_context(|| format!("Couldn't write {}", partial.display()))?;
    let saving = || format!("Couldn't save the download to {}", partial.display());
    let mut reader = response.into_reader();
    let mut hasher = Sha256::new();
    let mut buffer = vec![0u8; 64 * 1024];
    let mut total = 0u64;
    loop {
        let read = reader.read(&mut buffer).context("The download was interrupted")?;
        if read == 0 {
            break;
        }
        file.write_all(&buffer[..read]).with_context(saving)?;
        hasher.update(&buffer[..read]);
        total += read as u64;
        progress.store(total, Ordering::Relaxed);
    }
    file.flush().with_context(saving)?;
    drop(file);
    let check = || -> Result<()> {
        if total != asset.size {
            bail!("The download is incomplete ({total} of {} bytes).", asset.size);
        }
        if let Some(expected) = asset.digest.as_deref().and_then(|d| d.strip_prefix("sha256:")) {
            let actual: String = hasher.finalize().iter().map(|b| format!("{b:02x}")).collect();
            if !actual.eq_ignore_ascii_case(expected) {
                bail!("The download doesn't match the checksum GitHub lists for it.");
            }
        }
        Ok(())
    };
    if let Err(err) = check() {
        let _ = fs::remove_file(&partial);
        return Err(err);
    }
    rename_with_retries(&partial, dest).with_context(|| format!("Couldn't put the download at {}", dest.display()))?;
    Ok(())
}

/// A scanner can temporarily hold a downloaded file open without allowing a rename on
/// Windows. Retry access-denied, sharing and lock violations for up to three seconds.
fn rename_with_retries(from: &Path, to: &Path) -> std::io::Result<()> {
    let mut tries = 0;
    loop {
        match fs::rename(from, to) {
            Ok(()) => return Ok(()),
            Err(err) if cfg!(windows) && matches!(err.raw_os_error(), Some(5 | 32 | 33)) && tries < 10 => {
                tries += 1;
                std::thread::sleep(Duration::from_millis(300));
            }
            Err(err) => return Err(err),
        }
    }
}

/// How this copy of Komando gets updated.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InstallKind {
    /// Replace the program file in place, then restart.
    Portable { exe: PathBuf },
    /// Run the new installer. `all_users`: installed for all users, so Windows asks for
    /// administrator rights.
    Installer { all_users: bool },
    /// Not on Windows: Komando ships for Windows only, so there is nothing to install.
    Unsupported,
}

/// The file the MSI puts next to `komando.exe` (see packaging/installed-with-msi.txt).
pub const MSI_MARKER: &str = "installed-with-msi.txt";

pub fn install_kind() -> InstallKind {
    if !cfg!(windows) {
        return InstallKind::Unsupported;
    }
    let Ok(exe) = std::env::current_exe() else { return InstallKind::Installer { all_users: true } };
    let Some(dir) = exe.parent() else { return InstallKind::Installer { all_users: true } };
    // A folder we can't write to is Program Files: the MSI, for all users. One we can write
    // to is the MSI for one user if it left its marker, else a portable copy.
    let probe = dir.join(".komando-update-probe");
    let writable = match fs::File::create(&probe) {
        Ok(_) => {
            let _ = fs::remove_file(&probe);
            true
        }
        Err(_) => false,
    };
    kind_for(exe.clone(), writable, dir.join(MSI_MARKER).is_file())
}

fn kind_for(exe: PathBuf, writable: bool, has_msi_marker: bool) -> InstallKind {
    match (writable, has_msi_marker) {
        (false, _) => InstallKind::Installer { all_users: true },
        (true, true) => InstallKind::Installer { all_users: false },
        (true, false) => InstallKind::Portable { exe },
    }
}

fn old_exe(exe: &Path) -> PathBuf {
    exe.with_extension("exe.old")
}

/// Put the downloaded program in place of the running one. The running file is renamed
/// aside (Windows allows that) and removed at the next start.
pub fn replace_exe(exe: &Path, new_exe: &Path) -> Result<()> {
    let old = old_exe(exe);
    let _ = fs::remove_file(&old);
    fs::rename(exe, &old).with_context(|| format!("Couldn't move {} aside", exe.display()))?;
    if let Err(err) = fs::rename(new_exe, exe) {
        // Put the old one back so Komando still starts.
        let _ = fs::rename(&old, exe);
        return Err(err).with_context(|| format!("Couldn't put the new version at {}", exe.display()));
    }
    Ok(())
}

/// Remove what an earlier update left behind. Call at startup.
pub fn clean_up() {
    if let Ok(exe) = std::env::current_exe() {
        let _ = fs::remove_file(old_exe(&exe));
        let _ = fs::remove_file(exe.with_extension("download"));
        let _ = fs::remove_file(exe.with_extension("part"));
    }
    remove_installers(&installer_dir());
}

/// Remove only week-old downloads from our own directory. A second Komando window may
/// still be downloading or using a recent installer when this window starts.
fn remove_installers(dir: &Path) {
    let Some(cutoff) = std::time::SystemTime::now().checked_sub(Duration::from_secs(7 * 24 * 60 * 60)) else { return };
    let Ok(entries) = fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        let stale_file =
            entry.metadata().ok().is_some_and(|meta| meta.is_file() && meta.modified().ok().is_some_and(|modified| modified < cutoff));
        if stale_file && name.starts_with("Komando-") && (name.ends_with(".msi") || name.ends_with(".part") || name.ends_with(".ready")) {
            let _ = fs::remove_file(entry.path());
        }
    }
}

/// Where downloaded installers go.
fn installer_dir() -> PathBuf {
    std::env::temp_dir().join("Komando updates")
}

/// Where a download for `kind` goes.
///
/// Each attempt gets a fresh filename so it never has to overwrite an installer still
/// open from an earlier attempt.
pub fn download_path(kind: &InstallKind, release: &Release) -> PathBuf {
    match kind {
        InstallKind::Portable { exe } => exe.with_extension("download"),
        _ => {
            let unique = uuid::Uuid::new_v4().simple().to_string();
            installer_dir().join(format!("Komando-{}-x64-{unique}.msi", release.version_text()))
        }
    }
}

/// Hand installation to an unelevated helper which waits for us to exit, runs the MSI
/// (elevating only that process when necessary), then reopens Komando as the original user.
pub fn run_installer(msi: &Path, all_users: bool, data_dir: &Path) -> Result<()> {
    #[cfg(windows)]
    {
        use base64::Engine as _;
        use std::os::windows::process::CommandExt;
        use std::time::Instant;

        let exe = std::env::current_exe().context("Couldn't find Komando to restart it")?;
        let system_root = std::env::var_os("SystemRoot").context("Couldn't locate Windows")?;
        let powershell = PathBuf::from(system_root).join(r"System32\WindowsPowerShell\v1.0\powershell.exe");
        let script = include_str!("update-install.ps1");
        let utf16: Vec<u8> = script.encode_utf16().flat_map(u16::to_le_bytes).collect();
        let encoded = base64::engine::general_purpose::STANDARD.encode(utf16);
        let ready = msi.with_extension("ready");
        let _ = fs::remove_file(&ready);
        fs::create_dir_all(data_dir).context("Couldn't prepare the update status file")?;
        let mut child = std::process::Command::new(powershell)
            .args(["-NoProfile", "-NonInteractive", "-WindowStyle", "Hidden", "-EncodedCommand", &encoded])
            .env("KOMANDO_UPDATE_MSI", msi)
            .env("KOMANDO_UPDATE_EXE", exe)
            .env("KOMANDO_UPDATE_PARENT", std::process::id().to_string())
            .env("KOMANDO_UPDATE_ALL_USERS", if all_users { "1" } else { "0" })
            .env("KOMANDO_UPDATE_READY", &ready)
            .env("KOMANDO_UPDATE_ERROR", data_dir.join("update-error.txt"))
            .creation_flags(0x08000000) // CREATE_NO_WINDOW
            .spawn()
            .context("Couldn't start the update helper")?;
        let started = Instant::now();
        loop {
            if ready.is_file() {
                return Ok(());
            }
            if let Some(status) = child.try_wait()? {
                bail!("The update helper stopped before installation could start ({status}). Komando is still open.");
            }
            if started.elapsed() >= Duration::from_secs(15) {
                let _ = child.kill();
                let _ = child.wait();
                bail!("The update helper did not start in time. Komando is still open; please try again.");
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }
    #[cfg(not(windows))]
    {
        let _ = (msi, all_users, data_dir);
        bail!("Updates install on Windows only.")
    }
}

/// An unsuccessful installer handoff reopens the app and leaves its explanation here.
pub fn take_install_error(data_dir: &Path) -> Option<String> {
    let path = data_dir.join("update-error.txt");
    let message = fs::read_to_string(&path).ok()?;
    let _ = fs::remove_file(path);
    Some(message)
}

/// Start the (new) program again. The caller quits.
pub fn relaunch(exe: &Path) -> Result<()> {
    std::process::Command::new(exe).spawn().context("Couldn't start the new version")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    struct TestDir(PathBuf);

    impl TestDir {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!("komando-update-test-{}", uuid::Uuid::new_v4()));
            fs::create_dir_all(&path).unwrap();
            Self(path)
        }
    }

    impl Drop for TestDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    /// Serve one download without depending on a release or an internet connection.
    fn download_fixture(body: &'static [u8]) -> (Asset, std::thread::JoinHandle<()>) {
        use std::io::BufRead;
        use std::net::TcpListener;

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/installer.msi", listener.local_addr().unwrap());
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
            stream.set_write_timeout(Some(Duration::from_secs(5))).unwrap();
            let mut request = std::io::BufReader::new(&stream);
            loop {
                let mut line = String::new();
                assert_ne!(request.read_line(&mut line).unwrap(), 0);
                if line == "\r\n" {
                    break;
                }
            }
            write!(stream, "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len()).unwrap();
            stream.write_all(body).unwrap();
        });
        let digest = format!("sha256:{:x}", Sha256::digest(body));
        (Asset { name: "installer.msi".into(), browser_download_url: url, size: body.len() as u64, digest: Some(digest) }, server)
    }

    #[test]
    fn retrying_a_download_leaves_an_open_installer_alone() {
        let release = parse_release(api("v9.0.1", &["Komando-9.0.1-x64.msi"])).unwrap();
        // Keep the fixture out of the real updater's directory.
        let dir = TestDir::new();
        let kind = InstallKind::Installer { all_users: true };
        let first = dir.0.join(download_path(&kind, &release).file_name().unwrap());
        fs::write(&first, b"previous installer").unwrap();
        let mut options = fs::OpenOptions::new();
        options.read(true);
        // Windows Installer can keep an MSI open without allowing replacement or deletion.
        #[cfg(windows)]
        {
            use std::os::windows::fs::OpenOptionsExt;
            options.share_mode(1); // FILE_SHARE_READ
        }
        let open_installer = options.open(&first).unwrap();

        let next = dir.0.join(download_path(&kind, &release).file_name().unwrap());
        let (asset, server) = download_fixture(b"new installer");
        let progress = Arc::new(AtomicU64::new(0));
        let result = download(&asset, &next, progress.clone());
        server.join().unwrap();
        result.unwrap();
        assert_eq!(fs::read(&first).unwrap(), b"previous installer");
        assert_eq!(fs::read(&next).unwrap(), b"new installer");
        assert_eq!(progress.load(Ordering::Relaxed), asset.size);
        drop(open_installer);
    }

    #[test]
    fn install_kind_tells_msi_installs_from_portable_copies() {
        let exe = PathBuf::from(r"C:\Komando\komando.exe");
        assert_eq!(kind_for(exe.clone(), false, false), InstallKind::Installer { all_users: true });
        assert_eq!(kind_for(exe.clone(), false, true), InstallKind::Installer { all_users: true });
        assert_eq!(kind_for(exe.clone(), true, true), InstallKind::Installer { all_users: false });
        assert_eq!(kind_for(exe.clone(), true, false), InstallKind::Portable { exe });
    }

    #[test]
    fn versions_parse_and_compare() {
        assert_eq!(parse_version("v0.1.12"), Some((0, 1, 12)));
        assert_eq!(parse_version("0.2.0"), Some((0, 2, 0)));
        assert_eq!(parse_version("v1.0.0-beta.2"), Some((1, 0, 0)));
        assert_eq!(parse_version("v1.0"), None);
        assert_eq!(parse_version("latest"), None);
        assert!((0, 1, 10) > (0, 1, 9));
        assert!((0, 2, 0) > (0, 1, 99));
    }

    #[test]
    fn release_notes_preview_joins_wrapped_bullets_and_removes_inline_markdown() {
        let mut api = api("v9.0.1", &[]);
        api.body = Some("Changes since 9.0.0.\n\n### Added\n\n- A **sidebar update button**\n  with `progress`.\n- Read the [release notes](https://example.invalid).\n- Third change.\n- Fourth change.\n".into());
        let release = parse_release(api).unwrap();
        assert_eq!(release.highlights(), ["A sidebar update button with progress.", "Read the release notes.", "Third change."]);
    }

    #[test]
    fn release_notes_preview_handles_null_prose_and_long_unicode() {
        let mut release = parse_release(api("v9.0.1", &[])).unwrap();
        assert!(release.highlights().is_empty());
        release.notes = "## Release\n\nA useful update.".into();
        assert_eq!(release.highlights(), ["A useful update."]);
        release.notes = format!("- {}", "界".repeat(300));
        let preview = release.highlights();
        assert_eq!(preview[0].chars().count(), 181);
        assert!(preview[0].ends_with('…'));
        let api: ApiRelease = serde_json::from_str(r#"{"tag_name":"v9.0.1","html_url":"https://example.invalid","body":null}"#).unwrap();
        assert!(parse_release(api).unwrap().highlights().is_empty());
    }

    #[test]
    fn installer_failures_are_reported_once_on_restart() {
        let dir = TestDir::new();
        assert!(take_install_error(&dir.0).is_none());
        fs::write(dir.0.join("update-error.txt"), "The update was cancelled.").unwrap();
        assert_eq!(take_install_error(&dir.0).as_deref(), Some("The update was cancelled."));
        assert!(take_install_error(&dir.0).is_none());
    }

    fn api(tag: &str, assets: &[&str]) -> ApiRelease {
        ApiRelease {
            tag_name: tag.into(),
            html_url: format!("https://github.com/{REPO}/releases/tag/{tag}"),
            body: None,
            draft: false,
            prerelease: false,
            assets: assets
                .iter()
                .map(|name| Asset {
                    name: name.to_string(),
                    browser_download_url: format!("https://example.invalid/{name}"),
                    size: 1,
                    digest: None,
                })
                .collect(),
        }
    }

    #[test]
    fn releases_pick_their_files() {
        let release =
            parse_release(api("v9.0.1", &["Komando-x64.msi", "Komando-9.0.1-x64.exe", "Komando-9.0.1-x64.msi", "Komando-x64.exe"]))
                .unwrap();
        assert_eq!(release.version, (9, 0, 1));
        assert!(release.is_newer());
        assert_eq!(release.exe.unwrap().name, "Komando-9.0.1-x64.exe");
        assert_eq!(release.msi.unwrap().name, "Komando-9.0.1-x64.msi");
        // Older releases only had the unversioned copies.
        let release = parse_release(api("v9.0.2", &["Komando-x64.msi"])).unwrap();
        assert!(release.exe.is_none());
        assert_eq!(release.msi.unwrap().name, "Komando-x64.msi");
        assert!(!parse_release(api("v0.0.1", &[])).unwrap().is_newer());
        assert!(parse_release(api("nightly", &[])).is_err());
    }

    #[test]
    fn each_installer_download_gets_a_file_of_its_own() {
        let release = parse_release(api("v9.0.1", &["Komando-9.0.1-x64.msi"])).unwrap();
        let kind = InstallKind::Installer { all_users: true };
        let (a, b) = (download_path(&kind, &release), download_path(&kind, &release));
        assert_ne!(a, b);
        assert_eq!(a.parent(), Some(installer_dir().as_path()));
        let name = a.file_name().unwrap().to_string_lossy().to_string();
        assert!(name.starts_with("Komando-9.0.1-x64-") && name.ends_with(".msi"), "{name}");
    }

    #[test]
    fn cleanup_removes_only_stale_installers_and_keeps_active_downloads() {
        let dir = TestDir::new();
        let old = std::time::SystemTime::now() - Duration::from_secs(8 * 24 * 60 * 60);
        for name in [
            "Komando-0.3.0-x64.msi", "Komando-0.3.1-x64-1a2b3c4d.msi", "Komando-0.3.1-x64-1a2b3c4d.part",
            "Komando-0.3.1-x64-1a2b3c4d.ready", "other.msi", "Komando.txt",
        ] {
            fs::File::create(dir.0.join(name)).unwrap().set_modified(old).unwrap();
        }
        // A recently downloaded MSI may be waiting on its setup window, and another
        // running Komando can be writing a .part file when cleanup runs.
        fs::write(dir.0.join("Komando-recent.msi"), "x").unwrap();
        fs::write(dir.0.join("Komando-active.part"), "x").unwrap();
        fs::write(dir.0.join("Komando-active.ready"), "ready").unwrap();
        remove_installers(&dir.0);
        let mut left: Vec<String> = fs::read_dir(&dir.0).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().to_string()).collect();
        left.sort();
        assert_eq!(left, ["Komando-active.part", "Komando-active.ready", "Komando-recent.msi", "Komando.txt", "other.msi"]);
    }

    #[test]
    fn a_download_lands_in_a_folder_that_does_not_exist_yet() {
        let dir = TestDir::new();
        let dest = dir.0.join("new folder").join("installer.msi");
        let (asset, server) = download_fixture(b"installer");
        let result = download(&asset, &dest, Arc::new(AtomicU64::new(0)));
        server.join().unwrap();
        result.unwrap();
        assert_eq!(fs::read(&dest).unwrap(), b"installer");
        assert!(!dest.with_extension("part").exists());
    }

    #[test]
    fn incomplete_or_tampered_downloads_never_become_installers() {
        let dir = TestDir::new();
        for wrong_size in [true, false] {
            let dest = dir.0.join("installer.msi");
            let (mut asset, server) = download_fixture(b"installer");
            if wrong_size {
                asset.size += 1;
            } else {
                asset.digest = Some(format!("sha256:{}", "0".repeat(64)));
            }
            let result = download(&asset, &dest, Arc::new(AtomicU64::new(0)));
            server.join().unwrap();
            let error = result.unwrap_err().to_string();
            assert!(error.contains(if wrong_size { "incomplete" } else { "checksum" }), "{error}");
            assert!(!dest.exists());
            assert!(!dest.with_extension("part").exists());
        }
    }

    #[test]
    fn download_errors_identify_the_failed_file_operation() {
        let dir = TestDir::new();
        let dest = dir.0.join("installer.msi");
        // A directory cannot be replaced by the downloaded file.
        fs::create_dir(&dest).unwrap();
        let (asset, server) = download_fixture(b"installer");
        let result = download(&asset, &dest, Arc::new(AtomicU64::new(0)));
        server.join().unwrap();
        let error = format!("{:#}", result.unwrap_err());
        assert!(error.contains(&format!("Couldn't put the download at {}:", dest.display())), "{error}");
    }

    #[cfg(windows)]
    #[test]
    fn a_temporary_windows_file_lock_does_not_fail_the_download_rename() {
        use std::os::windows::fs::OpenOptionsExt;

        let dir = TestDir::new();
        let from = dir.0.join("installer.part");
        let dest = dir.0.join("installer.msi");
        fs::write(&from, b"installer").unwrap();
        let scanner = fs::OpenOptions::new().read(true).share_mode(1).open(&from).unwrap();
        // Establish that the held handle actually prevents the rename.
        assert!(fs::rename(&from, &dest).is_err());
        let release = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(100));
            drop(scanner);
        });
        let result = rename_with_retries(&from, &dest);
        release.join().unwrap();
        result.unwrap();
        assert_eq!(fs::read(&dest).unwrap(), b"installer");
    }

    #[test]
    fn replacing_the_program_keeps_the_old_one_aside() {
        let dir = std::env::temp_dir().join(format!("komando-update-test-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let exe = dir.join("komando.exe");
        let new = dir.join("komando.download");
        fs::write(&exe, "old").unwrap();
        fs::write(&new, "new").unwrap();
        replace_exe(&exe, &new).unwrap();
        assert_eq!(fs::read_to_string(&exe).unwrap(), "new");
        assert_eq!(fs::read_to_string(old_exe(&exe)).unwrap(), "old");
        assert!(!new.exists());
        fs::remove_dir_all(&dir).unwrap();
    }

    /// Downloads the latest release's program from GitHub and checks it (size and SHA-256).
    #[test]
    #[ignore]
    fn the_latest_release_downloads_and_checks_out() {
        let release = fetch_latest().unwrap();
        let asset = release.exe.or(release.msi).unwrap();
        assert!(asset.digest.is_some(), "GitHub lists no digest for {}", asset.name);
        let dest = std::env::temp_dir().join(format!("komando-update-download-{}", std::process::id()));
        let progress = Arc::new(AtomicU64::new(0));
        download(&asset, &dest, progress.clone()).unwrap();
        assert_eq!(fs::metadata(&dest).unwrap().len(), asset.size);
        assert_eq!(progress.load(Ordering::Relaxed), asset.size);
        // A wrong digest is refused and leaves nothing behind.
        let mut tampered = asset.clone();
        tampered.digest = Some(format!("sha256:{}", "0".repeat(64)));
        let other = dest.with_extension("tampered");
        assert!(download(&tampered, &other, Arc::new(AtomicU64::new(0))).is_err());
        assert!(!other.exists() && !other.with_extension("part").exists());
        fs::remove_file(&dest).unwrap();
    }

    /// Talks to GitHub. `cargo test --lib update -- --ignored`.
    #[test]
    #[ignore]
    fn the_latest_release_can_be_fetched() {
        let release = fetch_latest().unwrap();
        println!("latest: {} {:?} {:?}", release.tag, release.exe.map(|a| a.name), release.msi.map(|a| a.name));
    }
}
