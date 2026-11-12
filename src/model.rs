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
    /// The Jump Host its Hosts are reached through, unless a Host sets its own or connects
    /// directly. The Jump Host itself is never reached through itself.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub jump_host: Option<HostId>,
    /// Startup Commands typed into every Host's shell, before the Host's own.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub startup_commands: Vec<String>,
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

impl Project {
    /// An empty Project with a fresh id and no defaults for its Hosts.
    pub fn new(name: impl Into<String>, color: impl Into<String>, icon: impl Into<String>) -> Self {
        Self {
            id: new_id(),
            name: name.into(),
            color: color.into(),
            icon: icon.into(),
            default_user: None,
            credential: None,
            jump_host: None,
            startup_commands: vec![],
            hosts: vec![],
            shells: vec![],
        }
    }
}

/// A Host setting that can come from the Project the Host is opened from.
///
/// In `library.toml` an inherited setting is left out, the Host's own value is written as is,
/// and a switched-off setting is written as `false`. Files from before this type existed only
/// have plain values and missing keys, so they read the same as they always did.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum Inherited<T> {
    /// Use the Project's value, if it has one.
    #[default]
    Project,
    /// The Host's own value.
    Own(T),
    /// Neither: no Jump Host (connect directly), no saved Credential or user (ask).
    Off,
}

impl<T> Inherited<T> {
    pub fn is_project(&self) -> bool {
        matches!(self, Inherited::Project)
    }

    pub fn own(&self) -> Option<&T> {
        match self {
            Inherited::Own(value) => Some(value),
            _ => None,
        }
    }

    /// The value in effect, given what the Project offers.
    pub fn or_project<'a>(&'a self, project: Option<&'a T>) -> Option<&'a T> {
        match self {
            Inherited::Project => project,
            Inherited::Own(value) => Some(value),
            Inherited::Off => None,
        }
    }
}

impl<T: Serialize> Serialize for Inherited<T> {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error> {
        match self {
            // Left out by `skip_serializing_if`; written as "no value" if it ever is not.
            Inherited::Project => serializer.serialize_none(),
            Inherited::Own(value) => value.serialize(serializer),
            Inherited::Off => serializer.serialize_bool(false),
        }
    }
}

impl<'de, T: Deserialize<'de>> Deserialize<'de> for Inherited<T> {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> std::result::Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Repr<T> {
            Switch(bool),
            Value(T),
        }
        Ok(match Repr::deserialize(deserializer)? {
            Repr::Switch(false) => Inherited::Off,
            Repr::Switch(true) => Inherited::Project,
            Repr::Value(value) => Inherited::Own(value),
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Host {
    pub id: HostId,
    pub name: String,
    pub address: String,
    #[serde(default = "default_port")]
    pub port: u16,
    #[serde(default, skip_serializing_if = "Inherited::is_project")]
    pub user: Inherited<String>,
    #[serde(default, skip_serializing_if = "Inherited::is_project")]
    pub credential: Inherited<CredentialId>,
    /// The Host this one is reached through. Off: connect directly.
    #[serde(default, skip_serializing_if = "Inherited::is_project")]
    pub jump_host: Inherited<HostId>,
    /// Startup Commands typed after the Project's (see `run_project_commands`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub startup_commands: Vec<String>,
    /// Type the Project's Startup Commands before the Host's own.
    #[serde(default = "yes", skip_serializing_if = "is_true")]
    pub run_project_commands: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icon: Option<String>,
}

impl Host {
    /// A Host with a fresh id that takes everything it can from its Project.
    pub fn new(name: impl Into<String>, address: impl Into<String>) -> Self {
        Self {
            id: new_id(),
            name: name.into(),
            address: address.into(),
            port: default_port(),
            user: Inherited::Project,
            credential: Inherited::Project,
            jump_host: Inherited::Project,
            startup_commands: vec![],
            run_project_commands: true,
            color: None,
            icon: None,
        }
    }
}

fn yes() -> bool {
    true
}

fn is_true(value: &bool) -> bool {
    *value
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
/// The Host's own values win; anything it leaves to the Project comes from the Project it was
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
    /// The Jump Host this Host is reached through, if any.
    pub jump_host: Option<HostId>,
    /// The Project's Startup Commands (unless the Host skips them), then the Host's own.
    pub startup_commands: Vec<String>,
    pub color: String,
    pub icon: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CredentialSource {
    Host,
    Project,
    /// Neither has one: asked on connect, and may be saved for the Host or the Project.
    None,
    /// The Host ignores the Project Credential: asked on connect, saved for the Host only.
    Off,
}

pub fn resolve(host: &Host, project: &Project) -> ResolvedHost {
    let (credential, credential_source) = match (&host.credential, &project.credential) {
        (Inherited::Own(c), _) => (Some(c.clone()), CredentialSource::Host),
        (Inherited::Off, _) => (None, CredentialSource::Off),
        (Inherited::Project, Some(c)) => (Some(c.clone()), CredentialSource::Project),
        (Inherited::Project, None) => (None, CredentialSource::None),
    };
    // The Project's Jump Host connects directly itself.
    let project_jump = project.jump_host.as_ref().filter(|id| **id != host.id);
    let mut startup_commands = if host.run_project_commands { project.startup_commands.clone() } else { vec![] };
    startup_commands.extend(host.startup_commands.iter().cloned());
    ResolvedHost {
        host_id: host.id.clone(),
        project_id: project.id.clone(),
        name: host.name.clone(),
        address: host.address.clone(),
        port: host.port,
        user: host.user.or_project(project.default_user.as_ref()).cloned(),
        credential,
        credential_source,
        jump_host: host.jump_host.or_project(project_jump).cloned(),
        startup_commands,
        color: host.color.clone().unwrap_or_else(|| project.color.clone()),
        icon: host.icon.clone().unwrap_or_else(|| "server".into()),
    }
}

/// Why a Host can't be reached.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RouteError {
    /// The named Host is reached through itself, directly or through other Jump Hosts.
    Cycle(String),
    /// The named Host's Jump Host no longer exists.
    Missing(String),
}

impl std::fmt::Display for RouteError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RouteError::Cycle(name) => write!(f, "{name} would be reached through itself. Pick another jump host."),
            RouteError::Missing(name) => write!(f, "The jump host of {name} no longer exists."),
        }
    }
}

impl std::error::Error for RouteError {}

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

    /// The Hosts to connect to, in order, to reach a Host opened from a Project: its Jump
    /// Hosts first (outermost first), the Host itself last.
    ///
    /// A Jump Host takes its settings from the same Project if it is in it, otherwise from the
    /// first Project that holds it.
    pub fn route(&self, project_id: &str, host_id: &str) -> Result<Vec<ResolvedHost>, RouteError> {
        let mut hops = vec![self.resolve(project_id, host_id).ok_or_else(|| RouteError::Missing(host_id.into()))?];
        while let Some(jump) = hops.last().and_then(|h| h.jump_host.clone()) {
            let from = hops.last().expect("not empty");
            if hops.iter().any(|h| h.host_id == jump) {
                return Err(RouteError::Cycle(hops[0].name.clone()));
            }
            let project = match self.project(&from.project_id) {
                Some(p) if p.hosts.contains(&jump) => Some(p),
                _ => self.projects.iter().find(|p| p.hosts.contains(&jump)),
            };
            let hop = project
                .and_then(|p| self.resolve(&p.id, &jump))
                .ok_or_else(|| RouteError::Missing(from.name.clone()))?;
            hops.push(hop);
        }
        hops.reverse();
        Ok(hops)
    }

    /// Whether every Host in every Project can be reached: no Jump Host cycles, no missing
    /// Jump Hosts. Forms check a changed copy of the Library with this before saving it.
    pub fn check_routes(&self) -> Result<(), RouteError> {
        for project in &self.projects {
            for host in &project.hosts {
                self.route(&project.id, host)?;
            }
        }
        Ok(())
    }

    /// Hosts that are reached through a Host, on their own or by their Project's default.
    pub fn reached_through(&self, jump: &str) -> Vec<&Host> {
        self.hosts
            .iter()
            .filter(|h| h.id != jump)
            .filter(|h| match &h.jump_host {
                Inherited::Own(id) => id == jump,
                Inherited::Off => false,
                Inherited::Project => self
                    .projects
                    .iter()
                    .any(|p| p.hosts.contains(&h.id) && p.jump_host.as_deref() == Some(jump)),
            })
            .collect()
    }

    /// Delete a Host. Hosts that were reached through it connect directly from now on.
    pub fn remove_host(&mut self, id: &str) {
        self.hosts.retain(|h| h.id != id);
        for host in &mut self.hosts {
            if host.jump_host.own().is_some_and(|j| j == id) {
                host.jump_host = Inherited::Off;
            }
        }
        for project in &mut self.projects {
            project.hosts.retain(|h| h != id);
            if project.jump_host.as_deref() == Some(id) {
                project.jump_host = None;
            }
        }
    }

    /// Delete a Project, and the Hosts that are in no other Project.
    pub fn remove_project(&mut self, id: &str) {
        let Some(index) = self.projects.iter().position(|p| p.id == id) else { return };
        let project = self.projects.remove(index);
        for host in project.hosts {
            if !self.projects.iter().any(|p| p.hosts.contains(&host)) {
                self.remove_host(&host);
            }
        }
    }

    /// Number of Hosts and Projects that reference a Credential, for "used by N" hints.
    pub fn credential_users(&self, credential: &str) -> usize {
        self.hosts
            .iter()
            .filter(|h| h.credential.own().is_some_and(|c| c == credential))
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
            jump_host: None,
            startup_commands: vec![],
            hosts: vec!["h1".into()],
            shells: vec![],
        }
    }

    fn own(value: Option<&str>) -> Inherited<String> {
        value.map_or(Inherited::Project, |v| Inherited::Own(v.into()))
    }

    fn host(credential: Option<&str>, user: Option<&str>, color: Option<&str>) -> Host {
        Host {
            id: "h1".into(),
            name: "db01".into(),
            address: "10.0.0.5".into(),
            port: 22,
            user: own(user),
            credential: own(credential),
            jump_host: Inherited::Project,
            startup_commands: vec![],
            run_project_commands: true,
            color: color.map(Into::into),
            icon: None,
        }
    }

    /// A Host with only an id and a name, for routing tests.
    fn named(id: &str) -> Host {
        Host { id: id.into(), name: id.into(), address: format!("{id}.lan"), ..host(None, None, None) }
    }

    fn reached_through(mut host: Host, jump: &str) -> Host {
        host.jump_host = Inherited::Own(jump.into());
        host
    }

    /// A Library with one Project "p1" holding the given Hosts.
    fn library(hosts: Vec<Host>) -> Library {
        let mut p = project(None, None);
        p.hosts = hosts.iter().map(|h| h.id.clone()).collect();
        Library { shells: None, projects: vec![p], hosts }
    }

    fn route_names(lib: &Library, host: &str) -> Vec<String> {
        lib.route("p1", host).unwrap().into_iter().map(|h| h.name).collect()
    }

    #[test]
    fn a_host_without_jump_host_is_its_own_route() {
        let lib = library(vec![named("web")]);
        assert_eq!(route_names(&lib, "web"), ["web"]);
    }

    #[test]
    fn jump_hosts_chain_in_connection_order() {
        let lib = library(vec![reached_through(named("web"), "inner"), reached_through(named("inner"), "outer"), named("outer")]);
        assert_eq!(route_names(&lib, "web"), ["outer", "inner", "web"]);
    }

    #[test]
    fn the_project_jump_host_applies_to_all_but_itself() {
        let mut lib = library(vec![named("web"), named("bastion")]);
        lib.projects[0].jump_host = Some("bastion".into());
        assert_eq!(route_names(&lib, "web"), ["bastion", "web"]);
        assert_eq!(route_names(&lib, "bastion"), ["bastion"]);
    }

    #[test]
    fn a_host_can_connect_directly_despite_the_project_jump_host() {
        let mut web = named("web");
        web.jump_host = Inherited::Off;
        let mut lib = library(vec![web, named("bastion")]);
        lib.projects[0].jump_host = Some("bastion".into());
        assert_eq!(route_names(&lib, "web"), ["web"]);
    }

    #[test]
    fn a_jump_host_outside_the_project_resolves_in_its_own_project() {
        let mut lib = library(vec![reached_through(named("web"), "bastion")]);
        let mut infra = project(Some("infra-key"), Some("ops"));
        infra.id = "p2".into();
        infra.color = "green".into();
        infra.hosts = vec!["bastion".into()];
        lib.projects.push(infra);
        lib.hosts.push(named("bastion"));
        lib.projects[0].credential = Some("atlas-key".into());

        let route = lib.route("p1", "web").unwrap();
        assert_eq!((route[0].project_id.as_str(), route[0].credential.as_deref()), ("p2", Some("infra-key")));
        assert_eq!(route[0].user.as_deref(), Some("ops"));
        assert_eq!((route[1].project_id.as_str(), route[1].credential.as_deref()), ("p1", Some("atlas-key")));
    }

    #[test]
    fn a_jump_host_in_the_same_project_resolves_there() {
        let mut lib = library(vec![reached_through(named("web"), "bastion"), named("bastion")]);
        let mut other = project(Some("other-key"), None);
        other.id = "p0".into();
        other.hosts = vec!["bastion".into()];
        lib.projects.insert(0, other);
        lib.projects[1].credential = Some("atlas-key".into());
        let route = lib.route("p1", "web").unwrap();
        assert_eq!(route[0].credential.as_deref(), Some("atlas-key"));
    }

    #[test]
    fn cycles_are_refused() {
        let lib = library(vec![reached_through(named("a"), "b"), reached_through(named("b"), "a")]);
        assert_eq!(lib.route("p1", "a").unwrap_err(), RouteError::Cycle("a".into()));
        assert!(lib.check_routes().is_err());
        assert!(library(vec![reached_through(named("a"), "b"), named("b")]).check_routes().is_ok());
    }

    #[test]
    fn a_missing_jump_host_is_reported() {
        let lib = library(vec![reached_through(named("web"), "gone")]);
        assert_eq!(lib.route("p1", "web").unwrap_err(), RouteError::Missing("web".into()));
    }

    #[test]
    fn startup_commands_run_project_first_unless_skipped() {
        let mut web = named("web");
        web.startup_commands = vec!["tail -f app.log".into()];
        let mut lib = library(vec![web]);
        lib.projects[0].startup_commands = vec!["cd /srv/atlas".into()];
        assert_eq!(lib.resolve("p1", "web").unwrap().startup_commands, ["cd /srv/atlas", "tail -f app.log"]);
        lib.hosts[0].run_project_commands = false;
        assert_eq!(lib.resolve("p1", "web").unwrap().startup_commands, ["tail -f app.log"]);
    }

    #[test]
    fn switched_off_sign_in_ignores_the_project_credential() {
        let mut h = host(None, None, None);
        h.credential = Inherited::Off;
        h.user = Inherited::Off;
        let r = resolve(&h, &project(Some("shared"), Some("root")));
        assert_eq!((r.credential, r.credential_source), (None, CredentialSource::Off));
        assert_eq!(r.user, None);
    }

    #[test]
    fn removing_a_jump_host_makes_its_hosts_connect_directly() {
        let mut lib = library(vec![reached_through(named("web"), "bastion"), named("db"), named("bastion")]);
        lib.projects[0].jump_host = Some("bastion".into());
        let mut names: Vec<_> = lib.reached_through("bastion").into_iter().map(|h| h.name.clone()).collect();
        names.sort();
        assert_eq!(names, ["db", "web"]);

        lib.remove_host("bastion");
        assert!(lib.host("bastion").is_none());
        assert_eq!(lib.projects[0].jump_host, None);
        assert_eq!(lib.host("web").unwrap().jump_host, Inherited::Off);
        assert_eq!(route_names(&lib, "db"), ["db"]);
    }

    #[test]
    fn removing_a_project_removes_hosts_that_are_in_no_other_project() {
        let mut lib = library(vec![named("web"), named("shared")]);
        let mut other = project(None, None);
        other.id = "p2".into();
        other.hosts = vec!["shared".into()];
        lib.projects.push(other);
        lib.remove_project("p1");
        assert!(lib.host("web").is_none());
        assert!(lib.host("shared").is_some());
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
    fn older_library_files_still_load() {
        // A Host as Komando 0.1.15 wrote it: plain values mean "the Host's own".
        let text = r#"
            [[hosts]]
            id = "h1"
            name = "db01"
            address = "10.0.0.5"
            user = "admin"
            credential = "c1"

            [[hosts]]
            id = "h2"
            name = "db02"
            address = "10.0.0.6"
        "#;
        let lib: Library = toml::from_str(text).unwrap();
        assert_eq!(lib.hosts[0].user, Inherited::Own("admin".into()));
        assert_eq!(lib.hosts[0].credential, Inherited::Own("c1".into()));
        assert_eq!(lib.hosts[1].user, Inherited::Project);
        assert_eq!(lib.hosts[1].jump_host, Inherited::Project);
        assert!(lib.hosts[1].run_project_commands);
    }

    #[test]
    fn switched_off_settings_are_written_as_false() {
        let mut h = host(None, None, None);
        h.jump_host = Inherited::Off;
        h.credential = Inherited::Own("c1".into());
        let text = toml::to_string(&h).unwrap();
        assert!(text.contains("jump_host = false"), "{text}");
        assert!(text.contains("credential = \"c1\""), "{text}");
        assert!(!text.contains("user"), "inherited settings are left out: {text}");
        assert_eq!(toml::from_str::<Host>(&text).unwrap(), h);
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
