//! Projects, Hosts and settings, persisted as TOML under the Komando data directory.
//!
//! Secrets never live here: a Host or Project only names a Credential by id, and the
//! Credential itself lives in the Vault (see `vault.rs`).

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result};
use serde::{Deserialize, Serialize};

pub type ProjectId = String;
pub type HostId = String;
pub type CredentialId = String;

pub fn new_id() -> String {
    uuid::Uuid::new_v4().simple().to_string()[..12].to_string()
}

/// Where Komando keeps its files. `%APPDATA%\Komando` on Windows; `KOMANDO_HOME` overrides it.
pub fn data_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os("KOMANDO_HOME") {
        return PathBuf::from(dir);
    }
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("Komando")
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Project {
    pub id: ProjectId,
    pub name: String,
    /// Preset name (e.g. "red") or `#rrggbb`.
    pub color: String,
    /// Lucide icon name, or "letter" for the first letter of the name.
    #[serde(default = "default_project_icon")]
    pub icon: String,
    /// User name Hosts inherit when they set none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_user: Option<String>,
    /// The Project Credential: used by Hosts that set no Credential of their own.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub credential: Option<CredentialId>,
    /// Hosts in this Project, in sidebar order. A Host may appear in several Projects.
    #[serde(default)]
    pub hosts: Vec<HostId>,
    /// Local Shells that belong to this Project (shown in its group, in its colour).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub shells: Vec<LocalShell>,
}

fn default_project_icon() -> String {
    "folder".into()
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Host {
    pub id: HostId,
    pub name: String,
    pub address: String,
    #[serde(default = "default_port")]
    pub port: u16,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub credential: Option<CredentialId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icon: Option<String>,
}

fn default_port() -> u16 {
    22
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct LocalShell {
    pub name: String,
    pub program: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub args: Vec<String>,
}

/// A Host's effective settings when opened from a specific Project.
///
/// The Host's own values win; anything it leaves unset comes from the Project it was
/// opened from. The same Host can therefore look and authenticate differently per Project.
#[derive(Debug, Clone, PartialEq)]
pub struct ResolvedHost {
    pub host_id: HostId,
    pub project_id: ProjectId,
    pub name: String,
    pub address: String,
    pub port: u16,
    pub user: Option<String>,
    pub credential: Option<CredentialId>,
    /// Where the credential came from, so "save password" can offer the right target.
    pub credential_source: CredentialSource,
    pub color: String,
    pub icon: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CredentialSource {
    Host,
    Project,
    None,
}

pub fn resolve(host: &Host, project: &Project) -> ResolvedHost {
    let (credential, credential_source) = match (&host.credential, &project.credential) {
        (Some(c), _) => (Some(c.clone()), CredentialSource::Host),
        (None, Some(c)) => (Some(c.clone()), CredentialSource::Project),
        (None, None) => (None, CredentialSource::None),
    };
    ResolvedHost {
        host_id: host.id.clone(),
        project_id: project.id.clone(),
        name: host.name.clone(),
        address: host.address.clone(),
        port: host.port,
        user: host.user.clone().or_else(|| project.default_user.clone()),
        credential,
        credential_source,
        color: host.color.clone().unwrap_or_else(|| project.color.clone()),
        icon: host.icon.clone().unwrap_or_else(|| "server".into()),
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct Library {
    /// Local Shells that belong to no Project; shown at the top of the sidebar.
    /// `None` (not set in the file) means the shells detected on this PC.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shells: Option<Vec<LocalShell>>,
    #[serde(default)]
    pub projects: Vec<Project>,
    #[serde(default)]
    pub hosts: Vec<Host>,
}

impl Library {
    pub const FILE: &'static str = "library.toml";

    pub fn load(dir: &Path) -> Result<Self> {
        let path = dir.join(Self::FILE);
        if !path.exists() {
            return Ok(Self::default());
        }
        let text = fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
        toml::from_str(&text).with_context(|| format!("parsing {}", path.display()))
    }

    pub fn save(&self, dir: &Path) -> Result<()> {
        fs::create_dir_all(dir)?;
        write_atomic(&dir.join(Self::FILE), toml::to_string_pretty(self)?.as_bytes())
    }

    /// Local Shells outside any Project.
    pub fn standalone_shells(&self) -> Vec<LocalShell> {
        self.shells.clone().unwrap_or_else(default_shells)
    }

    pub fn project(&self, id: &str) -> Option<&Project> {
        self.projects.iter().find(|p| p.id == id)
    }

    pub fn host(&self, id: &str) -> Option<&Host> {
        self.hosts.iter().find(|h| h.id == id)
    }

    pub fn hosts_of<'a>(&'a self, project: &'a Project) -> impl Iterator<Item = &'a Host> + 'a {
        project.hosts.iter().filter_map(|id| self.host(id))
    }

    pub fn resolve(&self, project_id: &str, host_id: &str) -> Option<ResolvedHost> {
        Some(resolve(self.host(host_id)?, self.project(project_id)?))
    }

    /// Number of Hosts and Projects that reference a Credential, for "used by N" hints.
    pub fn credential_users(&self, credential: &str) -> usize {
        self.hosts
            .iter()
            .filter(|h| h.credential.as_deref() == Some(credential))
            .count()
            + self
                .projects
                .iter()
                .filter(|p| p.credential.as_deref() == Some(credential))
                .count()
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum AppThemeSetting {
    System,
    Light,
    #[default]
    Dark,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum TerminalThemeSetting {
    #[default]
    Follow,
    Light,
    Dark,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Settings {
    /// Dark, light, or follow Windows.
    pub app_theme: AppThemeSetting,
    pub terminal_theme: TerminalThemeSetting,
    /// The Theme (by id, see `theme::DARK_THEMES`) used in dark mode.
    pub dark_theme: String,
    /// The Theme (by id, see `theme::LIGHT_THEMES`) used in light mode.
    pub light_theme: String,
    pub font_family: String,
    pub font_size: f32,
    /// Lines of Scrollback each Session keeps.
    pub scrollback_lines: usize,
    /// Minutes of inactivity before the Vault locks. 0 disables auto-lock.
    pub auto_lock_minutes: u32,
    /// PuTTY-style: selecting copies, right-click pastes.
    pub putty_clipboard: bool,
    /// Ask before pasting more than one line.
    pub confirm_multiline_paste: bool,
    /// Show the command rail: a mark at the terminal's left edge for each line entered.
    pub command_rail: bool,
    /// Show the minimap: the Scrollback in miniature on the terminal's right edge.
    pub minimap: bool,
    /// Highlight sets (by id) that are switched off.
    pub disabled_highlight_sets: Vec<String>,
    /// Windows 11 Mica: the blurred desktop shows through Komando's background.
    pub background_blur: bool,
    /// 0–100: how much of the blurred backdrop shows through.
    pub background_blur_intensity: u8,
    /// Look for a newer release on GitHub when Komando starts.
    pub check_for_updates: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            app_theme: AppThemeSetting::Dark,
            terminal_theme: TerminalThemeSetting::Follow,
            dark_theme: "ember".into(),
            light_theme: "ember".into(),
            font_family: "Cascadia Mono".into(),
            font_size: 14.0,
            scrollback_lines: 100_000,
            auto_lock_minutes: 30,
            putty_clipboard: false,
            confirm_multiline_paste: true,
            command_rail: true,
            minimap: true,
            disabled_highlight_sets: vec![],
            background_blur: true,
            background_blur_intensity: 60,
            check_for_updates: true,
        }
    }
}

impl Settings {
    pub const FILE: &'static str = "settings.toml";

    pub fn load(dir: &Path) -> Self {
        fs::read_to_string(dir.join(Self::FILE))
            .ok()
            .and_then(|text| toml::from_str(&text).ok())
            .unwrap_or_default()
    }

    pub fn save(&self, dir: &Path) -> Result<()> {
        fs::create_dir_all(dir)?;
        write_atomic(&dir.join(Self::FILE), toml::to_string_pretty(self)?.as_bytes())
    }
}

/// Shells offered when a Project defines none of its own.
pub fn default_shells() -> Vec<LocalShell> {
    #[cfg(windows)]
    {
        let mut shells = Vec::new();
        if which("pwsh.exe") {
            shells.push(LocalShell { name: "PowerShell 7".into(), program: "pwsh.exe".into(), args: vec!["-NoLogo".into()] });
        }
        shells.push(LocalShell { name: "Windows PowerShell".into(), program: "powershell.exe".into(), args: vec!["-NoLogo".into()] });
        shells.push(LocalShell { name: "Command Prompt".into(), program: "cmd.exe".into(), args: vec![] });
        shells
    }
    #[cfg(not(windows))]
    {
        let program = std::env::var("SHELL").unwrap_or_else(|_| "/bin/bash".into());
        let name = Path::new(&program)
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| "shell".into());
        vec![LocalShell { name, program, args: vec![] }]
    }
}

#[cfg(windows)]
fn which(exe: &str) -> bool {
    std::env::var_os("PATH")
        .map(|paths| std::env::split_paths(&paths).any(|dir| dir.join(exe).is_file()))
        .unwrap_or(false)
}

/// Write via a temporary file and rename, so a crash never leaves a half-written file.
pub fn write_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    let tmp = path.with_extension("tmp");
    fs::write(&tmp, bytes).with_context(|| format!("writing {}", tmp.display()))?;
    fs::rename(&tmp, path).with_context(|| format!("replacing {}", path.display()))?;
    Ok(())
}

pub type CredentialNames = BTreeMap<CredentialId, String>;

#[cfg(test)]
mod tests {
    use super::*;

    fn project(credential: Option<&str>, user: Option<&str>) -> Project {
        Project {
            id: "p1".into(),
            name: "Atlas".into(),
            color: "red".into(),
            icon: "folder".into(),
            default_user: user.map(Into::into),
            credential: credential.map(Into::into),
            hosts: vec!["h1".into()],
            shells: vec![],
        }
    }

    fn host(credential: Option<&str>, user: Option<&str>, color: Option<&str>) -> Host {
        Host {
            id: "h1".into(),
            name: "db01".into(),
            address: "10.0.0.5".into(),
            port: 22,
            user: user.map(Into::into),
            credential: credential.map(Into::into),
            color: color.map(Into::into),
            icon: None,
        }
    }

    #[test]
    fn host_settings_win_over_project() {
        let r = resolve(&host(Some("own"), Some("admin"), Some("blue")), &project(Some("shared"), Some("root")));
        assert_eq!(r.credential.as_deref(), Some("own"));
        assert_eq!(r.credential_source, CredentialSource::Host);
        assert_eq!(r.user.as_deref(), Some("admin"));
        assert_eq!(r.color, "blue");
    }

    #[test]
    fn unset_host_settings_come_from_the_opening_project() {
        let r = resolve(&host(None, None, None), &project(Some("shared"), Some("root")));
        assert_eq!(r.credential.as_deref(), Some("shared"));
        assert_eq!(r.credential_source, CredentialSource::Project);
        assert_eq!(r.user.as_deref(), Some("root"));
        assert_eq!(r.color, "red");
    }

    #[test]
    fn same_host_resolves_differently_per_project() {
        let h = host(None, None, None);
        let mut a = project(Some("x"), None);
        a.color = "red".into();
        let mut b = project(Some("y"), None);
        b.id = "p2".into();
        b.color = "blue".into();
        let (ra, rb) = (resolve(&h, &a), resolve(&h, &b));
        assert_eq!((ra.color.as_str(), ra.credential.as_deref()), ("red", Some("x")));
        assert_eq!((rb.color.as_str(), rb.credential.as_deref()), ("blue", Some("y")));
    }

    #[test]
    fn no_credential_anywhere() {
        let r = resolve(&host(None, None, None), &project(None, None));
        assert_eq!(r.credential, None);
        assert_eq!(r.credential_source, CredentialSource::None);
    }

    #[test]
    fn standalone_shells_default_to_detected_ones() {
        let lib = Library::default();
        assert_eq!(lib.standalone_shells(), default_shells());
        let custom = vec![LocalShell { name: "WSL".into(), program: "wsl.exe".into(), args: vec![] }];
        let lib = Library { shells: Some(custom.clone()), ..Library::default() };
        assert_eq!(lib.standalone_shells(), custom);
        // An explicit empty list means "no standalone shells", not "the defaults".
        let lib = Library { shells: Some(vec![]), ..Library::default() };
        assert!(lib.standalone_shells().is_empty());
    }

    #[test]
    fn library_roundtrips_through_toml() {
        let dir = std::env::temp_dir().join(format!("komando-test-{}", new_id()));
        let lib = Library { shells: None, projects: vec![project(Some("c"), None)], hosts: vec![host(None, Some("u"), None)] };
        lib.save(&dir).unwrap();
        assert_eq!(Library::load(&dir).unwrap(), lib);
        assert_eq!(lib.credential_users("c"), 1);
        fs::remove_dir_all(dir).ok();
    }
}
