//! Projects, Hosts and settings, persisted as TOML under the Komando data directory.
//!
//! Secrets never live here: a Host or Project only names a Credential by id, and the
//! Credential itself lives in the Vault (see `vault.rs`).

use std::collections::{BTreeSet, HashMap};
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
    /// Jump Hosts its Hosts go through first, in order, unless a Host switches them off.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub jump_hosts: Vec<JumpHost>,
    /// Before typed-in Jump Hosts: the saved Host its Hosts were reached through. Read from
    /// older files and converted by `Library::load`; never written.
    #[serde(default, rename = "jump_host", skip_serializing)]
    legacy_jump_host: Option<HostId>,
    /// Startup Commands typed into every Host's shell, before the Host's own.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub startup_commands: Vec<StartupCommand>,
    /// Port Forwards every Host's SSH Session opens, before the Host's own.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub port_forwards: Vec<PortForward>,
    /// Agent Forwarding for its Hosts, unless a Host sets its own. Off unless switched on.
    #[serde(default, skip_serializing_if = "is_false")]
    pub forward_agent: bool,
    /// Hosts in this Project, in sidebar order. A Host may appear in several Projects.
    #[serde(default)]
    pub hosts: Vec<HostId>,
    /// Local Shells that belong to this Project (shown in its group, in its colour).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub shells: Vec<LocalShell>,
    /// Highlight Rules colour the output of its Hosts and Local Shells. Off overrides the Hosts.
    #[serde(default = "yes", skip_serializing_if = "is_true")]
    pub highlight: bool,
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
            jump_hosts: vec![],
            legacy_jump_host: None,
            startup_commands: vec![],
            port_forwards: vec![],
            forward_agent: false,
            hosts: vec![],
            shells: vec![],
            highlight: true,
        }
    }

    /// The Credentials this Project refers to, in sidebar order: its default, then its Jump
    /// Hosts'. With [`Host::credential_refs`], every reference to a Credential there is
    /// (`Library::forget_credential` clears the same fields).
    fn credential_refs(&self) -> impl Iterator<Item = (&CredentialId, CredentialRef<'_>)> {
        let own = self.credential.as_ref().map(|c| (c, CredentialRef::Project(&self.name)));
        own.into_iter().chain(jump_credential_refs(&self.jump_hosts, &self.name))
    }
}

/// Where a Credential reference sits, named as the Vault page shows it.
enum CredentialRef<'a> {
    /// A Project's default Credential; the Project's name.
    Project(&'a str),
    /// A Host's own SSH Credential; the Host's name.
    Host(&'a str),
    /// A Host's Remote Desktop: the Host's name and the protocol ("RDP", "VNC").
    Desktop(&'a str, &'static str),
    /// A Jump Host, and the name of the Host or Project it belongs to.
    Jump(&'a JumpHost, &'a str),
}

/// The Credentials of a Host's or Project's Jump Hosts, in order.
fn jump_credential_refs<'a>(jumps: &'a [JumpHost], owner: &'a str) -> impl Iterator<Item = (&'a CredentialId, CredentialRef<'a>)> {
    jumps.iter().filter_map(move |j| Some((j.credential.as_ref()?, CredentialRef::Jump(j, owner))))
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

/// Which of the SSH Agent's keys a Host signs in with (see `session::agent`).
///
/// In `library.toml` the default is left out, a single key is written as its fingerprint
/// (`agent = "SHA256:…"`), and no agent as `agent = false`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum AgentUse {
    /// Every key the agent offers, until the server takes one.
    #[default]
    Any,
    /// Only the key with this SHA-256 fingerprint (`SHA256:…`): a server that counts each key
    /// offered as a failed try (OpenSSH's `MaxAuthTries`) refuses after a few.
    Key(String),
    /// Not the agent at all.
    Off,
}

impl AgentUse {
    pub fn is_any(&self) -> bool {
        *self == AgentUse::Any
    }
}

impl Serialize for AgentUse {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error> {
        match self {
            AgentUse::Any => serializer.serialize_bool(true),
            AgentUse::Key(fingerprint) => serializer.serialize_str(fingerprint),
            AgentUse::Off => serializer.serialize_bool(false),
        }
    }
}

impl<'de> Deserialize<'de> for AgentUse {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> std::result::Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Repr {
            Switch(bool),
            Key(String),
        }
        Ok(match Repr::deserialize(deserializer)? {
            Repr::Switch(true) => AgentUse::Any,
            Repr::Switch(false) => AgentUse::Off,
            Repr::Key(key) if key.trim().is_empty() => AgentUse::Any,
            Repr::Key(key) => AgentUse::Key(key.trim().to_string()),
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
    /// Which of the SSH Agent's keys it signs in with, if any.
    #[serde(default, skip_serializing_if = "AgentUse::is_any")]
    pub agent: AgentUse,
    /// Agent Forwarding on or off for this Host; `None` takes the Project's.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub forward_agent: Option<bool>,
    /// Go through the Project's Jump Hosts first, before the Host's own.
    #[serde(default = "yes", skip_serializing_if = "is_true")]
    pub use_project_jump_hosts: bool,
    /// The Host's own Jump Hosts, in the order they are signed in to, after the Project's.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub jump_hosts: Vec<JumpHost>,
    /// Before typed-in Jump Hosts: the saved Host this one was reached through. Read from
    /// older files and converted by `Library::load`; never written.
    #[serde(default, rename = "jump_host", skip_serializing)]
    legacy_jump_host: Inherited<HostId>,
    /// Startup Commands typed after the Project's (see `run_project_commands`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub startup_commands: Vec<StartupCommand>,
    /// Type the Project's Startup Commands before the Host's own.
    #[serde(default = "yes", skip_serializing_if = "is_true")]
    pub run_project_commands: bool,
    /// The Host's own Port Forwards, opened after the Project's (see `use_project_forwards`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub port_forwards: Vec<PortForward>,
    /// Open the Project's Port Forwards too.
    #[serde(default = "yes", skip_serializing_if = "is_true")]
    pub use_project_forwards: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icon: Option<String>,
    /// The Host's Remote Desktop over RDP, if it offers one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rdp: Option<Desktop>,
    /// The Host's Remote Desktop over VNC, if it offers one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vnc: Option<Desktop>,
    /// Whether RDP is offered in the Host's menu, even before it is configured.
    #[serde(default = "yes", skip_serializing_if = "is_true")]
    pub rdp_enabled: bool,
    /// Whether VNC is offered in the Host's menu, even before it is configured.
    #[serde(default = "yes", skip_serializing_if = "is_true")]
    pub vnc_enabled: bool,
    /// What clicking the Host in the sidebar opens when it has no Session yet: its terminal,
    /// or one of its Remote Desktops (a Windows machine without SSH).
    #[serde(default, skip_serializing_if = "Protocol::is_ssh")]
    pub opens: Protocol,
    /// Highlight Rules colour its terminal output, when its Project's are on too.
    #[serde(default = "yes", skip_serializing_if = "is_true")]
    pub highlight: bool,
    /// Programs on it may copy to the clipboard (OSC 52): always (`Some(true)`), never, or
    /// as Settings say (`None`, see [`Settings::remote_clipboard`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub clipboard: Option<bool>,
}

/// Whether programs on a Host may copy to the clipboard (OSC 52) when the Host has no choice
/// of its own. Local shells always may: their programs can reach the clipboard anyway.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum RemoteClipboard {
    /// The first copy asks: once, always for this Host, or never for it.
    #[default]
    Ask,
    Allow,
    Never,
}

/// How Komando reaches a Host: a terminal over SSH, or a Remote Desktop over RDP or VNC.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash, Default)]
#[serde(rename_all = "lowercase")]
pub enum Protocol {
    #[default]
    Ssh,
    Rdp,
    Vnc,
}

impl Protocol {
    pub fn is_ssh(&self) -> bool {
        *self == Protocol::Ssh
    }

    /// "SSH", "RDP", "VNC": how the UI names it.
    pub fn label(self) -> &'static str {
        match self {
            Protocol::Ssh => "SSH",
            Protocol::Rdp => "RDP",
            Protocol::Vnc => "VNC",
        }
    }

    /// The port a server of this kind listens on unless told otherwise.
    pub fn default_port(self) -> u16 {
        match self {
            Protocol::Ssh => 22,
            Protocol::Rdp => 3389,
            Protocol::Vnc => 5900,
        }
    }
}

/// A Remote Desktop a Host offers: an RDP or VNC server on the Host's own address.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Desktop {
    pub port: u16,
    /// RDP: the account to sign in with. None: the Host's user, or asked when connecting.
    /// VNC servers have no user.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user: Option<String>,
    /// RDP: the account's Windows domain. None: a local account (or one typed as
    /// `DOMAIN\user`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub domain: Option<String>,
    /// Its password in the Vault. None: asked when connecting.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub credential: Option<CredentialId>,
    /// Reach it through the Host's SSH Session: a tunnel to the port on the Host itself
    /// (`localhost` as the SSH server sees it), so a VNC server need not listen on the network
    /// and nothing crosses it unencrypted. Off: connect straight to the Host's address.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub through_ssh: bool,
    /// How the desktop is shown and what input reaches it, set from its toolbar.
    #[serde(default, skip_serializing_if = "ViewerOptions::is_default")]
    pub viewer: ViewerOptions,
}

impl Desktop {
    pub fn new(protocol: Protocol) -> Self {
        Self {
            port: protocol.default_port(),
            user: None,
            domain: None,
            credential: None,
            through_ssh: false,
            viewer: ViewerOptions::default(),
        }
    }
}

/// A Remote Desktop's viewing options: chosen in its toolbar, kept for the Host.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct ViewerOptions {
    pub scaling: Scaling,
    pub quality: Quality,
    /// Show the desktop without sending it keyboard or mouse input.
    pub view_only: bool,
    /// Send the Windows key and system shortcuts (Alt+Tab, Ctrl+Esc, Print Screen) to the
    /// desktop while it has the keyboard, instead of to this PC.
    pub send_shortcuts: bool,
    /// Send media keys (play/pause, next, previous, stop) to the desktop.
    pub send_media_keys: bool,
    /// Send volume up, down and mute to the desktop.
    pub send_volume_keys: bool,
    /// Text copied on this PC can be pasted on the desktop, and text copied there lands on
    /// this PC's clipboard.
    pub share_clipboard: bool,
    /// RDP: ask the server for a desktop the size of the pane, and again when it changes.
    /// `None` follows Settings → Remote desktop.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub match_window: Option<bool>,
}

impl Default for ViewerOptions {
    fn default() -> Self {
        Self {
            scaling: Scaling::Fit,
            quality: Quality::High,
            view_only: false,
            send_shortcuts: true,
            send_media_keys: true,
            send_volume_keys: false,
            share_clipboard: true,
            match_window: None,
        }
    }
}

impl ViewerOptions {
    pub fn shares_clipboard(self) -> bool {
        self.share_clipboard && !self.view_only
    }
}

impl ViewerOptions {
    pub fn is_default(&self) -> bool {
        *self == Self::default()
    }
}

/// How a Remote Desktop's picture fills its pane.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum Scaling {
    /// As large as fits, keeping its shape.
    #[default]
    Fit,
    /// One remote pixel per screen pixel; the view pans with the pointer when it is larger.
    Actual,
    /// The whole pane, its shape changed to fill it.
    Stretch,
}

impl Scaling {
    pub fn label(self) -> &'static str {
        match self {
            Scaling::Fit => "Fit",
            Scaling::Actual => "100%",
            Scaling::Stretch => "Stretch",
        }
    }
}

/// How much picture detail a Remote Desktop trades for speed. Applied when it connects.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum Quality {
    #[default]
    High,
    Medium,
    Low,
}

impl Quality {
    pub const ALL: [Quality; 3] = [Quality::High, Quality::Medium, Quality::Low];

    pub fn label(self) -> &'static str {
        match self {
            Quality::High => "High",
            Quality::Medium => "Medium",
            Quality::Low => "Low",
        }
    }

    /// What it means for `protocol`, in a few words.
    pub fn describe(self, protocol: Protocol) -> &'static str {
        match (self, protocol) {
            (Quality::High, Protocol::Rdp) => "All visual effects",
            (Quality::Medium, Protocol::Rdp) => "No wallpaper or animations",
            (Quality::Low, Protocol::Rdp) => "Plain windows, fastest",
            (Quality::High, _) => "Full color",
            (Quality::Medium, _) => "65,536 colors",
            (Quality::Low, _) => "256 colors, fastest",
        }
    }
}

impl Host {
    /// The Credentials this Host refers to, in sidebar order: its own SSH Credential (not an
    /// inherited one), its Remote Desktops' (RDP, then VNC), then its Jump Hosts'.
    fn credential_refs(&self) -> impl Iterator<Item = (&CredentialId, CredentialRef<'_>)> {
        let own = self.credential.own().map(|c| (c, CredentialRef::Host(&self.name)));
        let desktops = [(&self.rdp, "RDP"), (&self.vnc, "VNC")]
            .into_iter()
            .filter_map(|(desktop, protocol)| Some((desktop.as_ref()?.credential.as_ref()?, CredentialRef::Desktop(&self.name, protocol))));
        own.into_iter().chain(desktops).chain(jump_credential_refs(&self.jump_hosts, &self.name))
    }

    /// Remote desktops are offered by default; only an explicit opt-out hides one.
    pub fn desktop_enabled(&self, protocol: Protocol) -> bool {
        match protocol {
            Protocol::Ssh => false,
            Protocol::Rdp => self.rdp_enabled,
            Protocol::Vnc => self.vnc_enabled,
        }
    }

    /// The Host's Remote Desktop over `protocol`, if it has one (never for SSH).
    pub fn desktop(&self, protocol: Protocol) -> Option<&Desktop> {
        match protocol {
            Protocol::Ssh => None,
            Protocol::Rdp => self.rdp.as_ref(),
            Protocol::Vnc => self.vnc.as_ref(),
        }
    }

    pub fn desktop_mut(&mut self, protocol: Protocol) -> Option<&mut Desktop> {
        match protocol {
            Protocol::Ssh => None,
            Protocol::Rdp => self.rdp.as_mut(),
            Protocol::Vnc => self.vnc.as_mut(),
        }
    }

    /// The Remote Desktops it offers, RDP first.
    pub fn desktops(&self) -> impl Iterator<Item = Protocol> + '_ {
        [Protocol::Rdp, Protocol::Vnc].into_iter().filter(|p| self.desktop(*p).is_some())
    }

    /// What a click opens: what `opens` says, unless that Remote Desktop was removed since.
    pub fn opens_with(&self) -> Protocol {
        if self.desktop_enabled(self.opens) && self.desktop(self.opens).is_some() { self.opens } else { Protocol::Ssh }
    }

    /// A Host with a fresh id that takes everything it can from its Project.
    pub fn new(name: impl Into<String>, address: impl Into<String>) -> Self {
        Self {
            id: new_id(),
            name: name.into(),
            address: address.into(),
            port: default_port(),
            user: Inherited::Project,
            credential: Inherited::Project,
            agent: AgentUse::Any,
            forward_agent: None,
            use_project_jump_hosts: true,
            jump_hosts: vec![],
            legacy_jump_host: Inherited::Project,
            startup_commands: vec![],
            run_project_commands: true,
            port_forwards: vec![],
            use_project_forwards: true,
            color: None,
            icon: None,
            rdp: None,
            vnc: None,
            rdp_enabled: true,
            vnc_enabled: true,
            opens: Protocol::Ssh,
            highlight: true,
            clipboard: None,
        }
    }
}

/// A Jump Host: a machine signed in to on the way to a Host, typed in where it is used.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct JumpHost {
    pub address: String,
    #[serde(default = "default_port")]
    pub port: u16,
    /// None: asked when connecting.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user: Option<String>,
    /// Its password in the Vault (a key, for Jump Hosts converted from saved Hosts).
    /// None: asked when connecting.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub credential: Option<CredentialId>,
}

impl JumpHost {
    pub fn new(address: impl Into<String>) -> Self {
        Self { address: address.into(), port: default_port(), user: None, credential: None }
    }

    /// `user@address:port`, leaving out what is not set or is the default.
    pub fn label(&self) -> String {
        let user = self.user.as_deref().map(|u| format!("{u}@")).unwrap_or_default();
        let port = if self.port == default_port() { String::new() } else { format!(":{}", self.port) };
        format!("{user}{}{port}", self.address)
    }
}

/// A Startup Command: a line typed into an SSH Session's shell, as if the user typed it.
///
/// Without `wait_for` it is typed once the shell is ready and has gone quiet. With it, it is
/// typed once that text appears in the output, after the previous command's line (or, for
/// the first, once signed in), such as a root prompt after `sudo -i`.
///
/// In `library.toml` one without `wait_for` is written as plain text, as all were before
/// waiting existed; one with it as `{ wait_for = "...", command = "..." }`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StartupCommand {
    pub wait_for: Option<String>,
    pub command: String,
}

impl StartupCommand {
    pub fn new(command: impl Into<String>) -> Self {
        Self { wait_for: None, command: command.into() }
    }

    pub fn after(wait_for: impl Into<String>, command: impl Into<String>) -> Self {
        Self { wait_for: Some(wait_for.into()), command: command.into() }
    }

    /// The command, with what it waits for in front: `[# ] cd /srv`.
    pub fn label(&self) -> String {
        match &self.wait_for {
            Some(text) => format!("[{text}] {}", self.command),
            None => self.command.clone(),
        }
    }

    /// Whether it waits for a password question, so its command would be a password saved
    /// unencrypted (and shared with a Project). Komando never saves one: forms refuse it,
    /// imports and shared files leave it out.
    pub fn answers_password(&self) -> bool {
        // Any sudo prompt at all, or text naming a password in any language.
        self.wait_for.as_deref().is_some_and(|text| {
            text.to_lowercase().contains("[sudo") || crate::session::password_prompt::mentions_password(text)
        })
    }
}

impl From<&str> for StartupCommand {
    fn from(command: &str) -> Self {
        Self::new(command)
    }
}

impl PartialEq<&str> for StartupCommand {
    fn eq(&self, other: &&str) -> bool {
        self.wait_for.is_none() && self.command == *other
    }
}

impl Serialize for StartupCommand {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error> {
        #[derive(Serialize)]
        struct Step<'a> {
            wait_for: &'a str,
            command: &'a str,
        }
        match &self.wait_for {
            None => serializer.serialize_str(&self.command),
            Some(wait_for) => Step { wait_for, command: &self.command }.serialize(serializer),
        }
    }
}

impl<'de> Deserialize<'de> for StartupCommand {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> std::result::Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Repr {
            Line(String),
            Step {
                #[serde(default)]
                wait_for: Option<String>,
                command: String,
            },
        }
        Ok(match Repr::deserialize(deserializer)? {
            Repr::Line(command) => Self::new(command),
            Repr::Step { wait_for, command } => Self { wait_for: wait_for.filter(|w| !w.is_empty()), command },
        })
    }
}

/// What a Port Forward carries, and which way.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash, Default)]
#[serde(rename_all = "lowercase")]
pub enum ForwardKind {
    /// A port on this computer reaches an address from the Host's side (`ssh -L`).
    #[default]
    Local,
    /// A port on the Host reaches an address from this computer's side (`ssh -R`).
    Remote,
    /// A SOCKS5 proxy on this computer whose connections leave from the Host (`ssh -D`).
    Socks,
}

impl ForwardKind {
    pub const ALL: [ForwardKind; 3] = [ForwardKind::Local, ForwardKind::Remote, ForwardKind::Socks];

    pub fn label(self) -> &'static str {
        match self {
            ForwardKind::Local => "Local",
            ForwardKind::Remote => "Remote",
            ForwardKind::Socks => "SOCKS",
        }
    }

    /// Whether it sends its connections to an address of its own (SOCKS asks each program).
    pub fn has_target(self) -> bool {
        self != ForwardKind::Socks
    }

    /// Whether it listens on this computer, rather than on the Host.
    pub fn listens_here(self) -> bool {
        self != ForwardKind::Remote
    }
}

/// A Port Forward: connections to a port carried through a Host's SSH connection, for as long
/// as its SSH Session is connected.
///
/// It listens on loopback only (this computer, or the Host for a Remote forward), unless
/// `open_to_network` lets other machines on that network connect to it too.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PortForward {
    #[serde(default)]
    pub kind: ForwardKind,
    /// The port it listens on: on this computer, or on the Host for a Remote forward.
    pub port: u16,
    /// Where its connections go: resolved on the Host for a Local forward, on this computer
    /// for a Remote one. Empty for SOCKS, whose programs say where.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub to_host: String,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub to_port: u16,
    /// Listen on every network interface, not only loopback.
    #[serde(default, skip_serializing_if = "is_false")]
    pub open_to_network: bool,
}

impl PortForward {
    /// Whether both listen on the same port in the same place (this computer, or the Host),
    /// so they can't both open in one Session.
    pub fn same_listener(&self, other: &PortForward) -> bool {
        self.port == other.port && self.kind.listens_here() == other.kind.listens_here()
    }

    pub fn local(port: u16, to_host: impl Into<String>, to_port: u16) -> Self {
        Self { kind: ForwardKind::Local, port, to_host: to_host.into(), to_port, open_to_network: false }
    }

    pub fn remote(port: u16, to_host: impl Into<String>, to_port: u16) -> Self {
        Self { kind: ForwardKind::Remote, port, to_host: to_host.into(), to_port, open_to_network: false }
    }

    pub fn socks(port: u16) -> Self {
        Self { kind: ForwardKind::Socks, port, to_host: String::new(), to_port: 0, open_to_network: false }
    }

    /// Where it sends connections, `host:port` (IPv6 addresses in brackets).
    pub fn target(&self) -> String {
        if self.to_host.contains(':') {
            format!("[{}]:{}", self.to_host, self.to_port)
        } else {
            format!("{}:{}", self.to_host, self.to_port)
        }
    }

    /// What it does, short: "5432 → db.internal:5432", "Remote 8080 → localhost:3000",
    /// "SOCKS on 1080".
    pub fn label(&self) -> String {
        match self.kind {
            ForwardKind::Local => format!("{} → {}", self.port, self.target()),
            ForwardKind::Remote => format!("Remote {} → {}", self.port, self.target()),
            ForwardKind::Socks => format!("SOCKS on {}", self.port),
        }
    }
}

fn is_zero(value: &u16) -> bool {
    *value == 0
}

pub(crate) fn is_false(value: &bool) -> bool {
    !*value
}

/// Where a Jump Host on a Host's way is saved: the n-th of a Project's or of a Host's own.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JumpOwner {
    Project(ProjectId, usize),
    Host(HostId, usize),
}

/// A Jump Host on a Host's way, with where it is saved, so what is typed for it while
/// connecting can be saved there.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RouteJump {
    pub owner: JumpOwner,
    pub jump: JumpHost,
}

pub(crate) fn yes() -> bool {
    true
}

pub(crate) fn is_true(value: &bool) -> bool {
    *value
}

pub(crate) fn default_port() -> u16 {
    22
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct LocalShell {
    pub name: String,
    pub program: String,
    /// None selects artwork from the program or WSL distribution.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icon: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub args: Vec<String>,
    /// Left out of the sidebar (standalone shells only); still found by search, and shown
    /// while it has Sessions open.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub hidden: bool,
}

impl LocalShell {
    /// The arguments as one line, quoting those with spaces, for editing.
    pub fn args_line(&self) -> String {
        self.args
            .iter()
            .map(|a| if a.is_empty() || a.contains(char::is_whitespace) { format!("\"{a}\"") } else { a.clone() })
            .collect::<Vec<_>>()
            .join(" ")
    }
}

/// Split a line of arguments at spaces; double quotes keep spaces inside one argument.
pub fn split_args(line: &str) -> Vec<String> {
    let mut args = vec![];
    let mut current = String::new();
    let (mut quoted, mut started) = (false, false);
    for c in line.chars() {
        match c {
            '"' => {
                quoted = !quoted;
                started = true;
            }
            c if c.is_whitespace() && !quoted => {
                if started {
                    args.push(std::mem::take(&mut current));
                    started = false;
                }
            }
            c => {
                current.push(c);
                started = true;
            }
        }
    }
    if started {
        args.push(current);
    }
    args
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
    /// Which of the SSH Agent's keys it signs in with.
    pub agent: AgentUse,
    /// Agent Forwarding: the Host's own choice, else the Project's.
    pub forward_agent: bool,
    /// The Jump Hosts to sign in to first, in order: the Project's (unless the Host
    /// switches them off), then the Host's own. Empty: connect directly.
    pub jump_hosts: Vec<RouteJump>,
    /// The Project's Startup Commands (unless the Host skips them), then the Host's own.
    pub startup_commands: Vec<StartupCommand>,
    /// The Project's Port Forwards (unless the Host leaves them out), then the Host's own.
    pub port_forwards: Vec<PortForward>,
    pub color: String,
    pub icon: String,
}

impl ResolvedHost {
    /// Whether connecting reads the Vault: the Host or one of its Jump Hosts signs in with a
    /// saved Credential. Opening and reconnecting both unlock it first when it's locked.
    pub fn signs_in_with_vault(&self) -> bool {
        self.credential.is_some() || self.jump_hosts.iter().any(|j| j.jump.credential.is_some())
    }
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
    let project_jumps = project
        .jump_hosts
        .iter()
        .enumerate()
        .filter(|_| host.use_project_jump_hosts)
        .map(|(i, jump)| RouteJump { owner: JumpOwner::Project(project.id.clone(), i), jump: jump.clone() });
    let own_jumps =
        host.jump_hosts.iter().enumerate().map(|(i, jump)| RouteJump { owner: JumpOwner::Host(host.id.clone(), i), jump: jump.clone() });
    let mut startup_commands = if host.run_project_commands { project.startup_commands.clone() } else { vec![] };
    startup_commands.extend(host.startup_commands.iter().cloned());
    let mut port_forwards = if host.use_project_forwards { project.port_forwards.clone() } else { vec![] };
    port_forwards.extend(host.port_forwards.iter().cloned());
    ResolvedHost {
        host_id: host.id.clone(),
        project_id: project.id.clone(),
        name: host.name.clone(),
        address: host.address.clone(),
        port: host.port,
        user: host.user.or_project(project.default_user.as_ref()).cloned(),
        credential,
        credential_source,
        agent: host.agent.clone(),
        forward_agent: host.forward_agent.unwrap_or(project.forward_agent),
        jump_hosts: project_jumps.chain(own_jumps).collect(),
        startup_commands,
        port_forwards,
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
        // Only a file that is surely missing means a fresh start; one that can't be checked
        // is unreadable, so it isn't saved over.
        if !path.try_exists().with_context(|| format!("checking {}", path.display()))? {
            return Ok(Self::default());
        }
        let text = fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
        let mut library: Self = toml::from_str(&text).with_context(|| format!("parsing {}", path.display()))?;
        library.convert_legacy_jump_hosts();
        Ok(library)
    }

    pub fn save(&self, dir: &Path) -> Result<()> {
        save_toml(dir, Self::FILE, self)
    }

    /// Local Shells outside any Project, hidden ones included.
    pub fn standalone_shells(&self) -> Vec<LocalShell> {
        self.shells.clone().unwrap_or_else(default_shells)
    }

    /// The standalone shells to change: the detected ones become a saved list on first change.
    pub fn standalone_shells_mut(&mut self) -> &mut Vec<LocalShell> {
        self.shells.get_or_insert_with(default_shells)
    }

    /// Bring back the shells detected on this PC: shown again if hidden, added if removed.
    /// Shells added by hand stay.
    pub fn restore_detected_shells(&mut self) {
        crate::wsl::refresh();
        let shells = self.standalone_shells_mut();
        for detected in default_shells() {
            match shells.iter_mut().find(|s| s.name == detected.name) {
                Some(existing) => existing.hidden = false,
                None => shells.push(detected),
            }
        }
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

    /// Build once for a batch of Project lookups. Indices stay valid while the Library is
    /// unchanged; do not persist them across edits. Like `host`, the first duplicate wins.
    pub fn host_indices(&self) -> HashMap<&str, usize> {
        let mut index = HashMap::with_capacity(self.hosts.len());
        for (i, host) in self.hosts.iter().enumerate() {
            index.entry(host.id.as_str()).or_insert(i);
        }
        index
    }

    pub fn resolve(&self, project_id: &str, host_id: &str) -> Option<ResolvedHost> {
        Some(resolve(self.host(host_id)?, self.project(project_id)?))
    }

    /// A saved Jump Host, to change it.
    pub fn jump_host_mut(&mut self, owner: &JumpOwner) -> Option<&mut JumpHost> {
        match owner {
            JumpOwner::Project(id, i) => self.projects.iter_mut().find(|p| &p.id == id)?.jump_hosts.get_mut(*i),
            JumpOwner::Host(id, i) => self.hosts.iter_mut().find(|h| &h.id == id)?.jump_hosts.get_mut(*i),
        }
    }

    /// Turn Jump Hosts saved as Hosts (older files) into typed-in ones: each becomes the
    /// address, port, user and Credential it had, after the Jump Hosts it was reached through
    /// itself. The Hosts stay. A Host that set its own Jump Host, or connected directly,
    /// leaves the Project's Jump Hosts off, as does a Project's Jump Host in that Project.
    pub(crate) fn convert_legacy_jump_hosts(&mut self) {
        let legacy = self.projects.iter().any(|p| p.legacy_jump_host.is_some())
            || self.hosts.iter().any(|h| !h.legacy_jump_host.is_project());
        if !legacy {
            return;
        }
        let project_jumps: Vec<Vec<JumpHost>> = self
            .projects
            .iter()
            .map(|p| p.legacy_jump_host.as_deref().map(|jump| self.legacy_chain(&p.id, jump)).unwrap_or_default())
            .collect();
        let host_jumps: Vec<(bool, Vec<JumpHost>)> = self
            .hosts
            .iter()
            .map(|h| match &h.legacy_jump_host {
                Inherited::Own(jump) => {
                    let project = self.projects.iter().find(|p| p.hosts.contains(&h.id)).map(|p| p.id.as_str()).unwrap_or("");
                    (false, self.legacy_chain(project, jump))
                }
                Inherited::Off => (false, vec![]),
                Inherited::Project => {
                    let is_project_jump = self
                        .projects
                        .iter()
                        .any(|p| p.hosts.contains(&h.id) && p.legacy_jump_host.as_deref() == Some(h.id.as_str()));
                    (!is_project_jump, vec![])
                }
            })
            .collect();
        for (project, jumps) in self.projects.iter_mut().zip(project_jumps) {
            project.legacy_jump_host = None;
            if !jumps.is_empty() {
                project.jump_hosts = jumps;
            }
        }
        for (host, (use_project, jumps)) in self.hosts.iter_mut().zip(host_jumps) {
            host.legacy_jump_host = Inherited::Project;
            host.use_project_jump_hosts &= use_project;
            if !jumps.is_empty() {
                host.jump_hosts = jumps;
            }
        }
    }

    /// The legacy Jump Host `jump`, reached from a Host in Project `from`, as typed-in Jump
    /// Hosts: those it was reached through first, then itself. It took its settings from
    /// `from` if it was in it, otherwise from the first Project holding it. A missing Host
    /// or a cycle ends the chain.
    fn legacy_chain(&self, from: &str, jump: &str) -> Vec<JumpHost> {
        let mut chain = vec![];
        let mut seen: Vec<HostId> = vec![];
        let (mut from, mut next) = (from.to_string(), Some(jump.to_string()));
        while let Some(id) = next.take() {
            if seen.contains(&id) {
                break;
            }
            let project = match self.project(&from) {
                Some(p) if p.hosts.contains(&id) => Some(p),
                _ => self.projects.iter().find(|p| p.hosts.contains(&id)),
            };
            let (Some(host), Some(project)) = (self.host(&id), project) else { break };
            let credential = match (&host.credential, &project.credential) {
                (Inherited::Own(c), _) => Some(c.clone()),
                (Inherited::Project, c) => c.clone(),
                (Inherited::Off, _) => None,
            };
            chain.push(JumpHost {
                address: host.address.clone(),
                port: host.port,
                user: host.user.or_project(project.default_user.as_ref()).cloned(),
                credential,
            });
            // A Project's Jump Host connected directly itself.
            let project_jump = project.legacy_jump_host.as_ref().filter(|j| **j != id);
            next = host.legacy_jump_host.or_project(project_jump).cloned();
            from = project.id.clone();
            seen.push(id);
        }
        chain.reverse();
        chain
    }

    /// Copy a Host into the Project it was picked from, right below the original. The copy
    /// keeps every setting, including its Credential (shared, not copied in the Vault), and
    /// is named "<name> (copy)", numbered when that name is taken. Returns the copy's id.
    pub fn duplicate_host(&mut self, project_id: &str, host_id: &str) -> Option<HostId> {
        let original = self.host(host_id)?.clone();
        let project = self.projects.iter().position(|p| p.id == project_id)?;
        let taken = |name: &str| self.hosts.iter().any(|h| h.name == name);
        let mut name = format!("{} (copy)", original.name);
        let mut n = 2;
        while taken(&name) {
            name = format!("{} (copy {n})", original.name);
            n += 1;
        }
        let copy = Host { id: new_id(), name, ..original };
        let id = copy.id.clone();
        let hosts = &mut self.projects[project].hosts;
        let at = hosts.iter().position(|h| h == host_id).map_or(hosts.len(), |i| i + 1);
        hosts.insert(at, id.clone());
        let after = self.hosts.iter().position(|h| h.id == host_id).map_or(self.hosts.len(), |i| i + 1);
        self.hosts.insert(after, copy);
        Some(id)
    }

    /// Move a Host from one Project to the end of another. A Host that is already in the
    /// other Project only leaves this one. Its own settings stay; what it took from the
    /// Project (user, Credential, Jump Hosts, Startup Commands) now comes from the new one.
    /// Returns false when nothing changed, also when the move would give the Host two Port
    /// Forwards on one port (see `forward_clash`).
    pub fn move_host(&mut self, host_id: &str, from: &str, to: &str) -> bool {
        let position = |id: &str| self.projects.iter().position(|p| p.id == id);
        let (Some(from), Some(to)) = (position(from), position(to)) else { return false };
        if from == to || self.host(host_id).is_none() || !self.projects[from].hosts.iter().any(|h| h == host_id) {
            return false;
        }
        if self.forward_clash(host_id, &self.projects[to].id).is_some() {
            return false;
        }
        self.projects[from].hosts.retain(|h| h != host_id);
        if !self.projects[to].hosts.iter().any(|h| h == host_id) {
            self.projects[to].hosts.push(host_id.to_string());
        }
        true
    }

    /// The first of `project_id`'s Port Forwards that listens where one of `host_id`'s own does,
    /// with that one of the Host's, when the Host opens its Project's forwards: as part of that
    /// Project it couldn't open both. The host and project forms refuse the same.
    pub fn forward_clash(&self, host_id: &str, project_id: &str) -> Option<(&PortForward, &PortForward)> {
        let host = self.host(host_id).filter(|h| h.use_project_forwards)?;
        let project = self.project(project_id)?;
        project
            .port_forwards
            .iter()
            .find_map(|theirs| host.port_forwards.iter().find(|own| own.same_listener(theirs)).map(|own| (theirs, own)))
    }

    /// Move a Host to another place in its Project's sidebar order. `index` is where it goes
    /// among the Project's Hosts as they are now (0 is the top, the number of Hosts the
    /// bottom). Returns false when nothing changed.
    pub fn reorder_host(&mut self, project_id: &str, host_id: &str, index: usize) -> bool {
        let Some(project) = self.projects.iter_mut().find(|p| p.id == project_id) else { return false };
        let Some(from) = project.hosts.iter().position(|h| h == host_id) else { return false };
        let index = index.min(project.hosts.len());
        if index == from || index == from + 1 {
            return false;
        }
        let host = project.hosts.remove(from);
        project.hosts.insert(if index > from { index - 1 } else { index }, host);
        true
    }

    /// Move an entire Project to an insertion slot in the current sidebar order.
    pub fn reorder_project(&mut self, id: &str, index: usize) -> bool {
        let Some(from) = self.projects.iter().position(|p| p.id == id) else { return false };
        let index = index.min(self.projects.len());
        if index == from || index == from + 1 {
            return false;
        }
        let project = self.projects.remove(from);
        self.projects.insert(if index > from { index - 1 } else { index }, project);
        true
    }

    /// Delete a Host.
    pub fn remove_host(&mut self, id: &str) {
        self.hosts.retain(|h| h.id != id);
        for project in &mut self.projects {
            project.hosts.retain(|h| h != id);
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

    /// Number of Hosts, Projects and Jump Hosts that reference a Credential, for "used by N"
    /// hints and to tell when one is no longer needed.
    pub fn credential_users(&self, credential: &str) -> usize {
        self.hosts
            .iter()
            .flat_map(Host::credential_refs)
            .chain(self.projects.iter().flat_map(Project::credential_refs))
            .filter(|(c, _)| *c == credential)
            .count()
    }

    /// What signs in with a Credential, for the Vault page: "web01", "web01 (RDP)",
    /// "bastion (jump host for web01)", "Production (project)". Each once, in library order.
    pub fn credential_uses(&self, credential: &str) -> Vec<String> {
        let mut uses = Vec::new();
        let refs = self.projects.iter().flat_map(Project::credential_refs).chain(self.hosts.iter().flat_map(Host::credential_refs));
        for (_, at) in refs.filter(|(c, _)| *c == credential) {
            let text = match at {
                CredentialRef::Project(name) => format!("{name} (project)"),
                CredentialRef::Host(name) => name.to_string(),
                CredentialRef::Desktop(name, protocol) => format!("{name} ({protocol})"),
                CredentialRef::Jump(jump, name) => format!("{} (jump host for {name})", jump.label()),
            };
            if !uses.contains(&text) {
                uses.push(text);
            }
        }
        uses
    }

    /// A Project's Credentials in sidebar order: its defaults, then each Host's SSH,
    /// Remote Desktop and Jump Host Credentials. Shared Credentials appear once per Project.
    pub fn project_credentials<'a>(&'a self, project: &'a Project) -> Vec<&'a CredentialId> {
        let mut credentials = Vec::new();
        let refs = project.credential_refs().chain(self.hosts_of(project).flat_map(Host::credential_refs));
        for (id, _) in refs {
            if !credentials.contains(&id) {
                credentials.push(id);
            }
        }
        credentials
    }

    /// Drop every reference to a deleted Credential. A Host that had it as its own asks
    /// instead of falling back to its Project's: that would send another password unasked.
    /// Returns whether anything changed.
    pub fn forget_credential(&mut self, credential: &str) -> bool {
        let mut changed = false;
        let mut clear = |c: &mut Option<CredentialId>| {
            if c.as_deref() == Some(credential) {
                *c = None;
                changed = true;
            }
        };
        for project in &mut self.projects {
            clear(&mut project.credential);
            project.jump_hosts.iter_mut().for_each(|j| clear(&mut j.credential));
        }
        for host in &mut self.hosts {
            [&mut host.rdp, &mut host.vnc].into_iter().flatten().for_each(|d| clear(&mut d.credential));
            host.jump_hosts.iter_mut().for_each(|j| clear(&mut j.credential));
        }
        for host in self.hosts.iter_mut().filter(|h| h.credential.own().is_some_and(|c| c == credential)) {
            host.credential = Inherited::Off;
            changed = true;
        }
        changed
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

/// The app artwork, chosen independently of the app's colour theme.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum AppIcon {
    #[default]
    Dark,
    Light,
}

/// Where the minimap goes.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum MinimapPlacement {
    /// A column of its own, always shown, as in VS Code: output never runs underneath it.
    #[default]
    Beside,
    /// Laid over the output's right edge, shown after resting the pointer at that edge.
    Over,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Settings {
    /// Dark, light, or follow Windows.
    pub app_theme: AppThemeSetting,
    pub app_icon: AppIcon,
    pub terminal_theme: TerminalThemeSetting,
    /// The Theme (by id, see `theme::DARK_THEMES`) used in dark mode.
    pub dark_theme: String,
    /// The Theme (by id, see `theme::LIGHT_THEMES`) used in light mode.
    pub light_theme: String,
    /// The Terminal Colors (by id, see `theme::DARK_TERMINAL_COLORS`) of a dark terminal.
    /// `theme::MATCH_THEME` derives them from the dark Theme instead.
    pub dark_terminal_colors: String,
    /// The Terminal Colors (by id, see `theme::LIGHT_TERMINAL_COLORS`) of a light terminal.
    pub light_terminal_colors: String,
    /// Empty follows the selected Terminal Theme's bundled font; a name overrides it.
    pub font_family: String,
    pub font_size: f32,
    /// The weight of terminal text, 100 (thin) to 900 (black); 400 is regular.
    pub font_weight: u16,
    /// The weight of bold terminal text, 100 to 900; 700 is bold.
    pub bold_font_weight: u16,
    /// Lines of Scrollback each Session keeps.
    pub scrollback_lines: usize,
    /// Minutes of inactivity before the Vault locks, up to 7 days. 0 means it stays unlocked
    /// until Komando closes. Timed unlocks can be restored on the same Windows account.
    pub auto_lock_minutes: u32,
    /// The master password popover's "Stay unlocked": on, the Vault stays unlocked for
    /// `auto_lock_minutes`; off, it locks again once what asked for it has used it.
    pub stay_unlocked: bool,
    /// The old single PuTTY-style clipboard switch, now `copy_on_select` with a right-click
    /// that pastes. Read once by `load` and never written.
    #[serde(skip_serializing)]
    pub putty_clipboard: bool,
    /// What the mouse and the clipboard do in the terminal.
    #[serde(flatten)]
    pub pointer: Pointer,
    /// Local keyboard selection instead of the shell's Ctrl+A and Shift navigation.
    pub windows_selection: bool,
    /// macOS: Option is the terminal's Meta key instead of typing characters.
    pub option_as_meta: bool,
    pub cursor_shape: TerminalCursor,
    pub cursor_blink: bool,
    /// Show the command rail: a mark at the terminal's left edge for each line entered.
    pub command_rail: bool,
    /// Keep the Command History: each Host's entered commands, with their time, on this PC.
    /// Off, nothing is recorded.
    pub command_history: bool,
    /// Show the minimap: the Scrollback in miniature on the terminal's right edge.
    pub minimap: bool,
    pub minimap_placement: MinimapPlacement,
    /// Over the output: how long the pointer rests at the right edge before the minimap opens.
    pub minimap_delay_ms: u32,
    /// Also show the minimap in Split View. Off, both panes give its room to output.
    pub minimap_in_split: bool,
    /// Highlight sets (by id) that are switched off.
    pub disabled_highlight_sets: Vec<String>,
    /// Highlight Rules colour terminal output at all. Hosts and Projects can turn it off too.
    pub highlighting: bool,
    /// Highlight Rules also colour full-screen programs (the alternate screen: vim, htop).
    pub highlight_full_screen: bool,
    /// Windows 11 Mica: the blurred desktop shows through Komando's background.
    pub background_blur: bool,
    /// Hide minimized windows in the Windows notification area.
    pub minimize_to_tray: bool,
    /// Closing the window into the notification area (see `minimize_to_tray`) disconnects
    /// every Session. On unless switched off; minimizing leaves them running either way.
    pub disconnect_on_close_to_tray: bool,
    /// 0–100: how much of the blurred backdrop shows through.
    pub background_blur_intensity: u8,
    /// Look for a newer release on GitHub when Komando starts.
    pub check_for_updates: bool,
    /// Send the usage ping: once a day, an anonymous event so the developers can estimate how
    /// many people use Komando (see `crate::usage`). On unless switched off.
    pub usage_ping: bool,
    /// Where the SSH Agent is (see `session::agent::Agent::from_setting`): empty finds the
    /// usual one, `none` switches it off, `pageant` picks Pageant, anything else is a socket or
    /// pipe path.
    pub ssh_agent: String,
    /// Width of the SFTP file browser beside the terminal, in pixels.
    pub sftp_width: f32,
    /// Preferred sidebar width in logical pixels, adjusted by dragging its divider.
    pub sidebar_width: f32,
    /// Start with the sidebar hidden, and float it out over the Sessions while the pointer
    /// rests at the window's left edge, like a browser's vertical tabs.
    pub sidebar_auto_hide: bool,
    /// How a sidebar row closes its Session without opening its menu.
    pub quick_close: QuickClose,
    /// Projects folded away in the sidebar, remembered across app launches.
    pub collapsed_projects: BTreeSet<ProjectId>,
    /// The SFTP browser lists files starting with a dot.
    pub sftp_show_hidden: bool,
    /// "Rename" in the SFTP browser's context menu (F2 renames either way).
    pub sftp_menu_rename: bool,
    /// "Copy to…" in the SFTP browser's context menu.
    pub sftp_menu_copy_to: bool,
    /// "Move to…" in the SFTP browser's context menu.
    pub sftp_menu_move_to: bool,
    /// RDP: ask the server for a desktop the size of the pane, and again when the pane changes
    /// size. Off, the server picks the size and the picture is scaled.
    pub rdp_fit_desktop: bool,
    /// VNC: leave other viewers of the desktop connected. Off asks the server for it alone.
    pub vnc_shared: bool,
    /// The welcome tour was shown (or skipped) and isn't offered again on start.
    pub onboarded: bool,
    /// Reopen the terminal tabs that were open when Komando last closed (see `OpenTabs`).
    pub restore_tabs: bool,
    /// Presentation mode: the terminal's font size while presenting.
    pub presentation_font_size: f32,
    /// Presentation mode fills the screen.
    pub presentation_full_screen: bool,
    /// Presentation mode hides the sidebar (Ctrl+Shift+B still shows it).
    pub presentation_hide_sidebar: bool,
    /// Whether programs on a Host may copy to the clipboard, for Hosts without a choice of
    /// their own.
    pub remote_clipboard: RemoteClipboard,
}

/// How a sidebar row closes the Session it stands for, as × does (Settings → Appearance).
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "kebab-case")]
pub enum QuickClose {
    /// The status dot of a row with one Session turns into × on hover, and a middle-click on
    /// a row closes its Session.
    #[default]
    HoverAndMiddleClick,
    HoverOnly,
    MiddleClickOnly,
    /// Only × on a Session's own row, Ctrl+Shift+W and the menus close.
    Off,
}

impl QuickClose {
    pub const ALL: [QuickClose; 4] = [Self::HoverAndMiddleClick, Self::HoverOnly, Self::MiddleClickOnly, Self::Off];

    pub fn label(self) -> &'static str {
        match self {
            Self::HoverAndMiddleClick => "Hover × and middle-click",
            Self::HoverOnly => "Hover × only",
            Self::MiddleClickOnly => "Middle-click only",
            Self::Off => "Off",
        }
    }

    /// The status dot turns into × while the pointer is over the row.
    pub fn on_hover(self) -> bool {
        matches!(self, Self::HoverAndMiddleClick | Self::HoverOnly)
    }

    pub fn on_middle_click(self) -> bool {
        matches!(self, Self::HoverAndMiddleClick | Self::MiddleClickOnly)
    }
}

/// What a right-click in the terminal does. With Ctrl or Shift held it always opens the menu.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "kebab-case")]
pub enum RightClick {
    #[default]
    Menu,
    Paste,
    /// Copies the selection when there is one, otherwise pastes (as in Windows Terminal).
    CopyOrPaste,
}

/// What opens a link in the terminal's output.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "kebab-case")]
pub enum LinkClick {
    #[default]
    Ctrl,
    Alt,
    /// A plain click that doesn't drag. Ctrl+click still opens one, also in programs that
    /// take the mouse.
    Click,
}

/// Characters that end a word when double-clicking, besides tabs (see `Pointer::word_separators`).
pub const DEFAULT_WORD_SEPARATORS: &str = ",│`|:\"' ()[]{}<>";

/// The terminal's mouse and clipboard behaviour (Settings → Mouse & clipboard).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Pointer {
    pub right_click: RightClick,
    /// A middle-click pastes the clipboard, unless a program took the mouse.
    pub middle_click_paste: bool,
    /// Double-click selection stops at these characters. A tab always ends a word too.
    pub word_separators: String,
    pub link_click: LinkClick,
    /// Selecting text copies it.
    pub copy_on_select: bool,
    /// Copies of a selection also carry its colors and styles as HTML, for documents and mail.
    pub copy_formatting: bool,
    /// Copies of a selection leave out whitespace and line breaks at both ends.
    pub trim_copied: bool,
    /// Ask before pasting more than one line.
    pub confirm_multiline_paste: bool,
    /// Pasted line breaks become spaces, so a paste is one line.
    pub paste_single_line: bool,
    /// Mark pastes for programs that ask (bracketed paste), so the shell doesn't run them.
    pub bracketed_paste: bool,
}

impl Default for Pointer {
    fn default() -> Self {
        Self {
            right_click: RightClick::Menu,
            middle_click_paste: false,
            word_separators: DEFAULT_WORD_SEPARATORS.into(),
            link_click: LinkClick::Ctrl,
            copy_on_select: false,
            copy_formatting: false,
            trim_copied: true,
            confirm_multiline_paste: true,
            paste_single_line: false,
            bracketed_paste: true,
        }
    }
}

impl Pointer {
    /// PuTTY's habits: selecting copies and a right-click pastes.
    pub fn putty_style(&self) -> bool {
        self.copy_on_select && self.right_click == RightClick::Paste
    }

    pub fn set_putty_style(&mut self, on: bool) {
        self.copy_on_select = on;
        self.right_click = if on { RightClick::Paste } else { RightClick::Menu };
    }
}

/// The cursor used unless a terminal program requests its own style.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum TerminalCursor {
    #[default]
    Line,
    Block,
    Underline,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            app_theme: AppThemeSetting::Dark,
            app_icon: AppIcon::Dark,
            terminal_theme: TerminalThemeSetting::Follow,
            dark_theme: crate::theme::DEFAULT_THEME.into(),
            light_theme: crate::theme::DEFAULT_THEME.into(),
            dark_terminal_colors: crate::theme::DEFAULT_TERMINAL_COLORS.into(),
            light_terminal_colors: crate::theme::DEFAULT_TERMINAL_COLORS.into(),
            font_family: String::new(),
            font_size: 14.0,
            font_weight: 400,
            bold_font_weight: 700,
            scrollback_lines: 100_000,
            auto_lock_minutes: 0,
            stay_unlocked: true,
            putty_clipboard: false,
            pointer: Pointer::default(),
            windows_selection: false,
            option_as_meta: false,
            cursor_shape: TerminalCursor::Line,
            cursor_blink: true,
            command_rail: true,
            command_history: true,
            minimap: true,
            minimap_placement: MinimapPlacement::Beside,
            minimap_delay_ms: 300,
            minimap_in_split: false,
            disabled_highlight_sets: vec![],
            highlighting: true,
            highlight_full_screen: false,
            background_blur: false,
            minimize_to_tray: true,
            disconnect_on_close_to_tray: true,
            background_blur_intensity: 60,
            check_for_updates: true,
            usage_ping: true,
            ssh_agent: String::new(),
            sftp_width: 540.,
            sidebar_width: 264.,
            sidebar_auto_hide: false,
            quick_close: QuickClose::default(),
            collapsed_projects: BTreeSet::new(),
            sftp_show_hidden: true,
            sftp_menu_rename: false,
            sftp_menu_copy_to: false,
            sftp_menu_move_to: false,
            rdp_fit_desktop: true,
            vnc_shared: true,
            onboarded: false,
            restore_tabs: false,
            presentation_font_size: 24.0,
            presentation_full_screen: true,
            presentation_hide_sidebar: true,
            remote_clipboard: RemoteClipboard::Ask,
        }
    }
}

impl Settings {
    pub const FILE: &'static str = "settings.toml";

    /// The saved settings, or the defaults on first run. A file that can't be read is kept
    /// aside (see [`Unreadable`]): saving the defaults over it would lose every setting made.
    pub fn load(dir: &Path) -> (Self, Option<Unreadable>) {
        match load_toml::<Self>(dir, Self::FILE, "your settings", "settings") {
            Ok(settings) => (settings.map_or_else(Self::default, Self::migrated), None),
            Err(mut unreadable) => {
                unreadable.message.push_str(" Settings are back to their defaults, with the anonymous daily ping off.");
                (Self::recovered(), Some(unreadable))
            }
        }
    }

    /// The defaults for someone whose settings file couldn't be read: not a first run, so no
    /// welcome tour, and the usage ping off, as they may have switched it off.
    fn recovered() -> Self {
        Self { onboarded: true, usage_ping: false, ..Self::default() }
    }

    /// Settings saved by older versions, in today's terms.
    fn migrated(mut self) -> Self {
        if std::mem::take(&mut self.putty_clipboard) {
            self.pointer.set_putty_style(true);
        }
        self
    }

    pub fn save(&self, dir: &Path) -> Result<()> {
        save_toml(dir, Self::FILE, self)
    }
}

/// A terminal tab remembered for Restore tabs on start: what it opened, and the name it was
/// given with Rename session….
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum SavedTab {
    /// A Host's SSH terminal.
    Host {
        project: ProjectId,
        host: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        name: Option<String>,
    },
    /// A Local Shell, by name; `project` is `None` for a standalone one.
    Shell {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        project: Option<ProjectId>,
        shell: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        name: Option<String>,
    },
}

/// The terminal tabs open in the main window, kept up to date while Restore tabs on start
/// is on, so a crash or a closed window leaves the last set.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct OpenTabs {
    pub tabs: Vec<SavedTab>,
    /// Index into `tabs` of the tab that was shown.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub active: Option<usize>,
}

impl OpenTabs {
    pub const FILE: &'static str = "tabs.toml";

    /// The tabs last saved; none when the file is missing or unreadable.
    pub fn load(dir: &Path) -> Self {
        fs::read_to_string(dir.join(Self::FILE))
            .ok()
            .and_then(|text| toml::from_str(&text).ok())
            .unwrap_or_default()
    }

    pub fn save(&self, dir: &Path) -> Result<()> {
        save_toml(dir, Self::FILE, self)
    }

    /// Forget the tabs, when Restore tabs on start is switched off.
    pub fn clear(dir: &Path) -> Result<()> {
        Ok(ignore_not_found(fs::remove_file(dir.join(Self::FILE)))?)
    }
}

/// The font weights a setting accepts, as CSS and OpenType number them.
pub const FONT_WEIGHTS: std::ops::RangeInclusive<u16> = 100..=900;

/// A font weight typed into Settings: a whole number from 100 to 900.
pub fn parse_font_weight(text: &str) -> Option<u16> {
    text.trim().parse::<u16>().ok().filter(|w| FONT_WEIGHTS.contains(w))
}

/// Shells offered when a Project defines none of its own.
pub fn default_shells() -> Vec<LocalShell> {
    #[cfg(windows)]
    {
        let mut shells = Vec::new();
        if which("pwsh.exe") {
            shells.push(LocalShell { name: "PowerShell 7".into(), program: "pwsh.exe".into(), args: vec!["-NoLogo".into()], icon: None, hidden: false });
        }
        shells.push(LocalShell { name: "Windows PowerShell".into(), program: "powershell.exe".into(), args: vec!["-NoLogo".into()], icon: None, hidden: false });
        shells.push(LocalShell { name: "Command Prompt".into(), program: "cmd.exe".into(), args: vec![], icon: None, hidden: false });
        for name in crate::wsl::distributions() {
            shells.push(LocalShell { name: format!("{name} (WSL)"), program: "wsl.exe".into(), args: vec!["--distribution".into(), name.clone()], ..LocalShell::default() });
        }
        shells
    }
    #[cfg(not(windows))]
    {
        let program = unix_shell(std::env::var_os("SHELL"));
        let name = Path::new(&program)
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| "shell".into());
        // macOS terminals start a login shell, which reads ~/.zprofile (Homebrew's PATH is
        // set there); apps opened from Finder don't inherit a terminal's environment.
        let args = if cfg!(target_os = "macos") { vec!["-l".into()] } else { Vec::new() };
        vec![LocalShell { name, program, args, ..LocalShell::default() }]
    }
}

/// The login shell offered on Linux and macOS. A `$SHELL` that is unset, empty, relative or
/// missing would fail every new Local Session, so the system's usual shell stands in for it
/// (zsh on macOS, bash elsewhere), or `/bin/sh`, which every Unix has.
#[cfg(not(windows))]
fn unix_shell(from_env: Option<std::ffi::OsString>) -> String {
    let usable = |path: &Path| path.is_absolute() && path.is_file();
    let usual = if cfg!(target_os = "macos") { "/bin/zsh" } else { "/bin/bash" };
    match from_env {
        Some(shell) if usable(Path::new(&shell)) => shell.to_string_lossy().into_owned(),
        _ if usable(Path::new(usual)) => usual.into(),
        _ => "/bin/sh".into(),
    }
}

#[cfg(windows)]
fn which(exe: &str) -> bool {
    std::env::var_os("PATH")
        .map(|paths| std::env::split_paths(&paths).any(|dir| dir.join(exe).is_file()))
        .unwrap_or(false)
}

/// A scanner can temporarily hold a downloaded file open without allowing a rename on
/// Windows. Retry access-denied, sharing and lock violations for up to three seconds.
pub fn rename_with_retries(from: &Path, to: &Path) -> std::io::Result<()> {
    let mut tries = 0;
    loop {
        match fs::rename(from, to) {
            Ok(()) => return Ok(()),
            Err(err) if cfg!(windows) && matches!(err.raw_os_error(), Some(5 | 32 | 33)) && tries < 10 => {
                tries += 1;
                std::thread::sleep(std::time::Duration::from_millis(300));
            }
            Err(err) => return Err(err),
        }
    }
}

/// The outcome of removing something, where one that was already gone counts as removed.
pub fn ignore_not_found(removed: std::io::Result<()>) -> std::io::Result<()> {
    match removed {
        Err(err) if err.kind() != std::io::ErrorKind::NotFound => Err(err),
        _ => Ok(()),
    }
}

/// Remove the files in `dir` that `ours` accepts and that were last changed more than `age`
/// ago. Best effort: a file that can't be checked or removed is left for a later try.
pub fn remove_older_than(dir: &Path, age: std::time::Duration, ours: impl Fn(&Path) -> bool) {
    let Some(cutoff) = std::time::SystemTime::now().checked_sub(age) else { return };
    let Ok(entries) = fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let path = entry.path();
        let stale = || entry.metadata().is_ok_and(|meta| meta.is_file() && meta.modified().is_ok_and(|at| at < cutoff));
        if ours(&path) && stale() {
            let _ = fs::remove_file(path);
        }
    }
}

/// Create `dir`, and any folder above it that is missing, for Komando's private data. On
/// Linux and macOS only the user may open it, as Hosts, Credentials and Command History are
/// inside; one an earlier version made is closed too. Windows keeps the folder's inherited
/// permissions.
pub fn create_private_dir(dir: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
        fs::DirBuilder::new().recursive(true).mode(0o700).create(dir)?;
        // Best effort: a `KOMANDO_HOME` the user doesn't own still works as before.
        let _ = fs::set_permissions(dir, fs::Permissions::from_mode(0o700));
        Ok(())
    }
    #[cfg(not(unix))]
    {
        fs::create_dir_all(dir)
    }
}

/// Options that create a file only the user may read on Linux and macOS, for Komando's
/// private data. The mode applies to a new file only. Windows keeps the folder's permissions.
pub fn private_file_options() -> fs::OpenOptions {
    #[allow(unused_mut)]
    let mut options = fs::OpenOptions::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options
}

/// Open `path` to add to its end, creating it as a private file (see
/// [`private_file_options`]). One an earlier version left readable by others is closed too.
pub fn open_private_append(path: &Path) -> std::io::Result<fs::File> {
    let file = private_file_options().create(true).append(true).open(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        // Best effort, as for the folder: a file the user doesn't own can still be added to.
        let _ = file.set_permissions(fs::Permissions::from_mode(0o600));
    }
    Ok(file)
}

/// Rename `path` to a dated `.broken-…` name beside it, or copy it there when it can't be
/// renamed (another program has it open, or the folder refuses renames).
fn set_aside(path: &Path, rename: impl FnOnce(&Path, &Path) -> std::io::Result<()>) -> Result<PathBuf> {
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(format!(".broken-{}", chrono::Local::now().format("%Y%m%d-%H%M%S")));
    let aside = path.with_file_name(name);
    let Err(rename_err) = rename(path, &aside) else { return Ok(aside) };
    match fs::copy(path, &aside) {
        Ok(_) => Ok(aside),
        Err(copy_err) => {
            // A half-written copy isn't one.
            let _ = fs::remove_file(&aside);
            Err(anyhow::anyhow!("moving it failed ({rename_err}), and so did copying it ({copy_err})"))
                .with_context(|| format!("keeping {} as {}", path.display(), aside.display()))
        }
    }
}

/// Save `value` as the TOML file `file` in Komando's private folder `dir`, creating the folder
/// when it is missing.
pub(crate) fn save_toml(dir: &Path, file: &str, value: &impl Serialize) -> Result<()> {
    create_private_dir(dir)?;
    write_atomic(&dir.join(file), toml::to_string_pretty(value)?.as_bytes())
}

/// Read the TOML file `file` in `dir`: `None` when it is missing, as on first run. One that
/// can't be read or parsed is kept aside (see [`Unreadable`]), as saving over it would lose
/// what it holds; `what` and `changes` are as for [`Unreadable::keep`].
pub(crate) fn load_toml<T: serde::de::DeserializeOwned>(
    dir: &Path,
    file: &str,
    what: &str,
    changes: &str,
) -> std::result::Result<Option<T>, Unreadable> {
    let path = dir.join(file);
    let err = match fs::read_to_string(&path) {
        Ok(text) => match toml::from_str(&text) {
            Ok(value) => return Ok(Some(value)),
            Err(err) => anyhow::Error::from(err),
        },
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(err) => anyhow::Error::from(err),
    };
    Err(Unreadable::keep(&path, &err, what, changes))
}

/// A data file Komando couldn't read at start, so the user can be told and their file isn't
/// replaced by the next save of what stands in for it.
#[derive(Debug, Clone, PartialEq)]
pub struct Unreadable {
    /// For the user: what went wrong, and where the file was kept.
    pub message: String,
    /// The file when it could be neither moved nor copied aside: it is still the only copy of
    /// what it holds, so nothing may be saved over it this run.
    pub in_place: Option<PathBuf>,
}

impl Unreadable {
    /// Keep the file at `path`, which couldn't be read (`err`), out of the next save's way: move
    /// it to a dated `.broken-…` name beside it, or copy it there. `what` names its contents for
    /// the user ("your settings"), `changes` what won't be saved when it can't be kept.
    pub fn keep(path: &Path, err: &anyhow::Error, what: &str, changes: &str) -> Self {
        Self::keep_with(path, err, what, changes, rename_with_retries)
    }

    fn keep_with(
        path: &Path,
        err: &anyhow::Error,
        what: &str,
        changes: &str,
        rename: impl FnOnce(&Path, &Path) -> std::io::Result<()>,
    ) -> Self {
        log::error!("{}: {err:#}", path.display());
        match set_aside(path, rename) {
            Ok(aside) => Self {
                message: format!("Couldn't read {what} ({err:#}). The file was kept as {}.", aside.display()),
                in_place: None,
            },
            Err(aside_err) => {
                log::error!("{aside_err:#}");
                Self {
                    message: format!(
                        "Couldn't read {what} ({err:#}). Changes to {changes} won't be saved, so {} isn't replaced. \
                         Fix or move it, then restart Komando.",
                        path.display()
                    ),
                    in_place: Some(path.to_path_buf()),
                }
            }
        }
    }
}

#[cfg(test)]
thread_local! {
    /// Makes `write_atomic` fail on this thread, so tests can check what a failed save leaves.
    pub(crate) static FAIL_WRITES: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Write via a temporary file and rename, so a crash never leaves a half-written file.
/// The file is private (see [`private_file_options`]), so one an earlier version left
/// readable by others is closed on its next save.
pub fn write_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    use std::io::Write;
    #[cfg(test)]
    if FAIL_WRITES.with(std::cell::Cell::get) {
        anyhow::bail!("writing {} failed for a test", path.display());
    }
    static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    // Unique per write and per process, so two windows saving the same file never share a temp.
    let unique = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let mut tmp_name = path.file_name().unwrap_or_default().to_os_string();
    tmp_name.push(format!(".{}.{unique}.tmp", std::process::id()));
    let tmp = path.with_file_name(tmp_name);
    let written = (|| {
        let mut file = private_file_options().write(true).create(true).truncate(true).open(&tmp)?;
        file.write_all(bytes)?;
        // On disk before the rename, so a power cut can't leave an empty file in its place.
        file.sync_all()
    })()
    .with_context(|| format!("writing {}", tmp.display()))
    .and_then(|()| rename_with_retries(&tmp, path).with_context(|| format!("replacing {}", path.display())));
    if written.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    written
}

#[cfg(test)]
mod tests {
    #[cfg(unix)]
    fn mode(path: &std::path::Path) -> u32 {
        use std::os::unix::fs::PermissionsExt;
        std::fs::metadata(path).unwrap().permissions().mode() & 0o777
    }

    #[cfg(unix)]
    #[test]
    fn saved_files_and_their_folder_are_private() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("komando-private-{}", uuid::Uuid::new_v4()));
        let nested = dir.join("history");
        super::create_private_dir(&nested).unwrap();
        assert_eq!(mode(&dir), 0o700, "a new data folder");
        assert_eq!(mode(&nested), 0o700, "a new folder inside it");

        // As an earlier version left them: readable by everyone.
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).unwrap();
        let saved = dir.join("library.toml");
        std::fs::write(&saved, "old").unwrap();
        std::fs::set_permissions(&saved, std::fs::Permissions::from_mode(0o644)).unwrap();
        let appended = nested.join("web.jsonl");
        std::fs::write(&appended, "old\n").unwrap();
        std::fs::set_permissions(&appended, std::fs::Permissions::from_mode(0o644)).unwrap();

        super::create_private_dir(&dir).unwrap();
        assert_eq!(mode(&dir), 0o700, "an existing folder is closed");
        super::write_atomic(&saved, b"new").unwrap();
        assert_eq!(mode(&saved), 0o600, "a saved file replaces the open one");
        assert_eq!(std::fs::read_to_string(&saved).unwrap(), "new");
        drop(super::open_private_append(&appended).unwrap());
        assert_eq!(mode(&appended), 0o600, "a file added to is closed");
        let fresh = nested.join("new.jsonl");
        drop(super::open_private_append(&fresh).unwrap());
        assert_eq!(mode(&fresh), 0o600, "a new file added to");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn open_tabs_survive_a_relaunch() {
        let dir = std::env::temp_dir().join(format!("komando-open-tabs-{}", uuid::Uuid::new_v4()));
        assert_eq!(OpenTabs::load(&dir), OpenTabs::default());
        let tabs = OpenTabs {
            tabs: vec![
                SavedTab::Shell { project: None, shell: "PowerShell 7".into(), name: None },
                SavedTab::Host { project: "prod".into(), host: "web01".into(), name: Some("logs".into()) },
                SavedTab::Shell { project: Some("prod".into()), shell: "bash".into(), name: None },
            ],
            active: Some(1),
        };
        tabs.save(&dir).unwrap();
        assert_eq!(OpenTabs::load(&dir), tabs);
        OpenTabs::clear(&dir).unwrap();
        OpenTabs::clear(&dir).unwrap();
        assert_eq!(OpenTabs::load(&dir), OpenTabs::default());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn font_weights_are_whole_numbers_from_100_to_900() {
        assert_eq!(parse_font_weight(" 350 "), Some(350));
        assert_eq!(parse_font_weight("100"), Some(100));
        assert_eq!(parse_font_weight("900"), Some(900));
        for text in ["99", "901", "bold", "", "450.5", "-400"] {
            assert_eq!(parse_font_weight(text), None, "{text}");
        }
        let settings: Settings = toml::from_str("font_size = 16.0").unwrap();
        assert_eq!((settings.font_weight, settings.bold_font_weight), (400, 700));
    }

    #[test]
    fn project_expansion_state_survives_relaunch_and_reopening() {
        let mut settings: Settings = toml::from_str("font_size = 16.0").unwrap();
        assert!(settings.collapsed_projects.is_empty());
        let dir = std::env::temp_dir().join(format!("komando-project-state-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        settings.collapsed_projects.extend(["production".into(), "staging".into()]);
        settings.save(&dir).unwrap();
        let mut reopened = Settings::load(&dir).0;
        assert_eq!(reopened.collapsed_projects, settings.collapsed_projects);
        assert!(!reopened.collapsed_projects.contains("development"));
        reopened.collapsed_projects.remove("production");
        reopened.save(&dir).unwrap();
        let relaunched = Settings::load(&dir).0;
        assert!(!relaunched.collapsed_projects.contains("production"));
        assert!(relaunched.collapsed_projects.contains("staging"));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn highlight_switches_default_on_and_are_only_written_when_off() {
        let mut project = Project::new("Web", "blue", "folder");
        let mut host = Host::new("web-01", "10.0.0.1");
        assert!(project.highlight && host.highlight);
        assert!(!toml::to_string(&project).unwrap().contains("highlight"));
        assert!(!toml::to_string(&host).unwrap().contains("highlight"));
        project.highlight = false;
        host.highlight = false;
        let project: Project = toml::from_str(&toml::to_string(&project).unwrap()).unwrap();
        let host: Host = toml::from_str(&toml::to_string(&host).unwrap()).unwrap();
        assert!(!project.highlight && !host.highlight);
        let settings: Settings = toml::from_str("").unwrap();
        assert!(settings.highlighting && !settings.highlight_full_screen);
    }

    #[test]
    fn pointer_settings_round_trip_flat_and_putty_clipboard_migrates() {
        let old: Settings = toml::from_str("putty_clipboard = true\nconfirm_multiline_paste = false").unwrap();
        let old = old.migrated();
        assert!(old.pointer.putty_style());
        assert!(!old.pointer.confirm_multiline_paste);
        let text = toml::to_string_pretty(&old).unwrap();
        assert!(!text.contains("putty_clipboard"), "{text}");
        assert!(text.contains("right_click = \"paste\""), "{text}");
        let mut settings = Settings::default();
        settings.pointer.link_click = LinkClick::Click;
        settings.pointer.word_separators = " ,".into();
        settings.pointer.right_click = RightClick::CopyOrPaste;
        let dir = std::env::temp_dir().join(format!("komando-pointer-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        settings.save(&dir).unwrap();
        assert_eq!(Settings::load(&dir).0, settings);
        std::fs::remove_dir_all(dir).unwrap();
        let fresh: Settings = toml::from_str("").unwrap();
        assert_eq!(fresh.pointer, Pointer::default());
    }

    #[test]
    fn app_icon_defaults_for_old_settings_and_persists_independently_of_theme() {
        let mut settings: Settings = toml::from_str("app_theme = 'light'").unwrap();
        assert_eq!(settings.app_icon, AppIcon::Dark);
        settings.app_icon = AppIcon::Light;
        settings.app_theme = AppThemeSetting::Dark;
        let dir = std::env::temp_dir().join(format!("komando-icon-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        settings.save(&dir).unwrap();
        assert_eq!(Settings::load(&dir).0, settings);
        settings.app_icon = AppIcon::Dark;
        settings.save(&dir).unwrap();
        assert_eq!(Settings::load(&dir).0.app_icon, AppIcon::Dark);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn viewer_options_default_away_and_keep_what_is_set() {
        let plain = Desktop::new(Protocol::Vnc);
        assert!(!toml::to_string(&plain).unwrap().contains("viewer"));
        let old: Desktop = toml::from_str("port = 5900").unwrap();
        assert_eq!(old.viewer, ViewerOptions::default());
        let mut set = Desktop::new(Protocol::Rdp);
        set.viewer.quality = Quality::Low;
        set.viewer.scaling = Scaling::Actual;
        let back: Desktop = toml::from_str(&toml::to_string(&set).unwrap()).unwrap();
        assert_eq!(back.viewer, set.viewer);
        // A partial set of options fills in the rest.
        let partial: ViewerOptions = toml::from_str("view_only = true").unwrap();
        assert!(partial.view_only && partial.send_shortcuts && partial.scaling == Scaling::Fit);
    }

    #[test]
    fn sidebar_width_defaults_for_old_settings_and_round_trips() {
        let mut settings: Settings = toml::from_str("font_size = 16.0").unwrap();
        assert_eq!(settings.sidebar_width, 264.);
        settings.sidebar_width = 410.;
        let saved = toml::to_string(&settings).unwrap();
        assert_eq!(toml::from_str::<Settings>(&saved).unwrap().sidebar_width, 410.);
    }

    #[test]
    fn unreadable_settings_are_kept_aside_rather_than_overwritten() {
        let dir = std::env::temp_dir().join(format!("komando-settings-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join(Settings::FILE), "font_size = \"huge\"").unwrap();
        let (settings, unreadable) = Settings::load(&dir);
        // Not a first run: no tour, and no ping the user may have switched off.
        assert_eq!(settings, Settings { onboarded: true, usage_ping: false, ..Settings::default() });
        let unreadable = unreadable.expect("the user is told");
        assert_eq!(unreadable.in_place, None);
        assert!(!dir.join(Settings::FILE).exists(), "the next save must not overwrite it");
        let kept: Vec<_> = fs::read_dir(&dir).unwrap().map(|e| e.unwrap().path()).collect();
        assert_eq!(kept.len(), 1, "{kept:?}");
        assert!(kept[0].file_name().unwrap().to_str().unwrap().starts_with("settings.toml.broken-"), "{kept:?}");
        assert!(unreadable.message.contains(&kept[0].display().to_string()), "{}", unreadable.message);
        assert_eq!(fs::read_to_string(&kept[0]).unwrap(), "font_size = \"huge\"");
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn missing_settings_are_a_first_run() {
        let dir = std::env::temp_dir().join(format!("komando-settings-{}", uuid::Uuid::new_v4()));
        assert_eq!(Settings::load(&dir), (Settings::default(), None));
    }

    #[test]
    fn a_hosts_clipboard_choice_round_trips_and_older_files_ask() {
        let mut host = Host::new("build", "10.0.4.22");
        let saved = toml::to_string(&host).unwrap();
        assert!(!saved.contains("clipboard"), "no choice, nothing written: {saved}");
        assert_eq!(toml::from_str::<Host>(&saved).unwrap().clipboard, None);
        host.clipboard = Some(false);
        assert_eq!(toml::from_str::<Host>(&toml::to_string(&host).unwrap()).unwrap().clipboard, Some(false));
        // Settings saved before the setting existed ask.
        let older: Settings = toml::from_str("usage_ping = false").unwrap();
        assert_eq!(older.remote_clipboard, RemoteClipboard::Ask);
        let never = Settings { remote_clipboard: RemoteClipboard::Never, ..Settings::default() };
        let back: Settings = toml::from_str(&toml::to_string(&never).unwrap()).unwrap();
        assert_eq!(back.remote_clipboard, RemoteClipboard::Never);
    }

    #[test]
    fn remote_desktops_round_trip_and_older_files_have_none() {
        let mut host = Host::new("win-build", "10.0.4.21");
        let old: Host = toml::from_str(&toml::to_string(&host).unwrap()).unwrap();
        assert_eq!((old.rdp, old.vnc, old.opens), (None, None, Protocol::Ssh));
        host.rdp = Some(Desktop { user: Some("ada".into()), credential: Some("c1".into()), ..Desktop::new(Protocol::Rdp) });
        host.vnc = Some(Desktop { through_ssh: true, ..Desktop::new(Protocol::Vnc) });
        host.opens = Protocol::Rdp;
        let text = toml::to_string(&host).unwrap();
        assert_eq!(toml::from_str::<Host>(&text).unwrap(), host);
        assert_eq!(host.desktops().collect::<Vec<_>>(), [Protocol::Rdp, Protocol::Vnc]);
        assert_eq!(host.opens_with(), Protocol::Rdp);
        // A Host that opened its RDP desktop opens its terminal once that desktop is removed.
        host.rdp = None;
        assert_eq!(host.opens_with(), Protocol::Ssh);
    }

    #[test]
    fn a_deleted_credential_is_named_by_its_uses_and_then_forgotten_everywhere() {
        let mut library = Library::default();
        let mut project = Project::new("Production", "red", "letter");
        project.credential = Some("c1".into());
        project.jump_hosts.push(JumpHost { credential: Some("c1".into()), ..JumpHost::new("bastion") });
        let mut web = Host::new("web01", "10.0.0.5");
        web.credential = Inherited::Own("c1".into());
        web.rdp = Some(Desktop { credential: Some("c1".into()), ..Desktop::new(Protocol::Rdp) });
        let mut db = Host::new("db", "10.0.0.6");
        db.credential = Inherited::Own("c2".into());
        db.jump_hosts.push(JumpHost { credential: Some("c1".into()), ..JumpHost::new("edge") });
        project.hosts = vec![web.id.clone(), db.id.clone()];
        library.projects.push(project);
        library.hosts.extend([web, db]);

        assert_eq!(
            library.credential_uses("c1"),
            ["Production (project)", "bastion (jump host for Production)", "web01", "web01 (RDP)", "edge (jump host for db)"]
        );
        assert!(library.credential_uses("unused").is_empty());

        assert!(library.forget_credential("c1"));
        assert_eq!(library.credential_users("c1"), 0);
        assert_eq!(library.hosts[0].credential, Inherited::Off, "web01 asks rather than using the project's");
        assert_eq!(library.hosts[1].credential, Inherited::Own("c2".into()));
        assert!(!library.forget_credential("c1"));
    }

    #[test]
    fn a_remote_desktop_password_counts_as_a_use_of_its_credential() {
        let mut library = Library::default();
        let mut host = Host::new("win-build", "10.0.4.21");
        host.vnc = Some(Desktop { credential: Some("c1".into()), ..Desktop::new(Protocol::Vnc) });
        library.hosts.push(host);
        assert_eq!(library.credential_users("c1"), 1);
    }

    #[test]
    fn desktop_menu_defaults_to_both_for_new_and_old_hosts() {
        let new = Host::new("web01", "web01.lan");
        let old: Host = toml::from_str("id = 'old'\nname = 'web01'\naddress = 'web01.lan'").unwrap();
        for host in [new, old] {
            assert!(host.desktop_enabled(Protocol::Rdp));
            assert!(host.desktop_enabled(Protocol::Vnc));
            // Offered in the menu, but each still needs its settings before it can open.
            assert!(host.desktop(Protocol::Rdp).is_none());
            assert!(host.desktop(Protocol::Vnc).is_none());
            assert_eq!(host.opens_with(), Protocol::Ssh);
        }
    }

    #[test]
    fn desktop_menu_opt_out_persists_per_protocol() {
        for protocol in [Protocol::Rdp, Protocol::Vnc] {
            let mut host = Host::new("web01", "web01.lan");
            host.rdp = Some(Desktop::new(Protocol::Rdp));
            host.vnc = Some(Desktop::new(Protocol::Vnc));
            host.opens = protocol;
            match protocol {
                Protocol::Rdp => host.rdp_enabled = false,
                _ => host.vnc_enabled = false,
            }
            let loaded: Host = toml::from_str(&toml::to_string(&host).unwrap()).unwrap();
            assert!(!loaded.desktop_enabled(protocol));
            let other = if protocol == Protocol::Rdp { Protocol::Vnc } else { Protocol::Rdp };
            assert!(loaded.desktop_enabled(other));
            assert_eq!(loaded.opens_with(), Protocol::Ssh);
            assert_eq!(loaded, host);
        }
    }

    #[test]
    fn terminal_preferences_migrate_and_round_trip() {
        use super::{Settings, TerminalCursor};
        let mut settings: Settings = toml::from_str("font_size = 16.0").unwrap();
        assert_eq!(settings.cursor_shape, TerminalCursor::Line);
        assert!(settings.cursor_blink);
        assert!(!settings.windows_selection);
        settings.cursor_shape = TerminalCursor::Underline;
        settings.cursor_blink = false;
        settings.windows_selection = true;
        let saved = toml::to_string(&settings).unwrap();
        assert_eq!(toml::from_str::<Settings>(&saved).unwrap(), settings);
    }

    use super::*;

    fn project(credential: Option<&str>, user: Option<&str>) -> Project {
        Project {
            id: "p1".into(),
            name: "Atlas".into(),
            color: "red".into(),
            icon: "folder".into(),
            default_user: user.map(Into::into),
            credential: credential.map(Into::into),
            jump_hosts: vec![],
            legacy_jump_host: None,
            startup_commands: vec![],
            port_forwards: vec![],
            forward_agent: false,
            hosts: vec!["h1".into()],
            shells: vec![],
            highlight: true,
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
            agent: AgentUse::Any,
            forward_agent: None,
            use_project_jump_hosts: true,
            jump_hosts: vec![],
            legacy_jump_host: Inherited::Project,
            startup_commands: vec![],
            run_project_commands: true,
            port_forwards: vec![],
            use_project_forwards: true,
            color: color.map(Into::into),
            icon: None,
            rdp: None,
            vnc: None,
            rdp_enabled: true,
            vnc_enabled: true,
            opens: Protocol::Ssh,
            highlight: true,
            clipboard: None,
        }
    }

    #[test]
    fn a_hosts_agent_choice_is_written_only_when_it_isnt_the_default() {
        let mut library = Library { projects: vec![project(None, None)], hosts: vec![host(None, None, None)], ..Default::default() };
        let text = toml::to_string(&library).unwrap();
        assert!(!text.contains("agent"), "{text}");
        library.hosts[0].agent = AgentUse::Key("SHA256:abc".into());
        library.hosts[0].forward_agent = Some(false);
        library.projects[0].forward_agent = true;
        let text = toml::to_string(&library).unwrap();
        assert!(text.contains("agent = \"SHA256:abc\""), "{text}");
        assert!(text.contains("forward_agent = false"), "{text}");
        assert_eq!(toml::from_str::<Library>(&text).unwrap(), library);
        library.hosts[0].agent = AgentUse::Off;
        let text = toml::to_string(&library).unwrap();
        assert!(text.contains("agent = false"), "{text}");
        assert_eq!(toml::from_str::<Library>(&text).unwrap(), library);
        // Komando 0.8 and older read the file too, and skip what they don't know.
        assert!(toml_0_8::from_str::<toml_0_8::Value>(&text).is_ok());
    }

    #[test]
    fn agent_forwarding_comes_from_the_project_unless_the_host_says() {
        let mut project = project(None, None);
        let mut host = host(None, None, None);
        assert!(!resolve(&host, &project).forward_agent, "off unless switched on");
        project.forward_agent = true;
        assert!(resolve(&host, &project).forward_agent);
        host.forward_agent = Some(false);
        assert!(!resolve(&host, &project).forward_agent);
        project.forward_agent = false;
        host.forward_agent = Some(true);
        assert!(resolve(&host, &project).forward_agent);
    }

    /// A Host with only an id and a name, for routing tests.
    fn named(id: &str) -> Host {
        Host { id: id.into(), name: id.into(), address: format!("{id}.lan"), ..host(None, None, None) }
    }

    fn jump(address: &str) -> JumpHost {
        JumpHost::new(address)
    }

    /// The addresses signed in to before the Host, in order.
    fn jump_addresses(lib: &Library, host: &str) -> Vec<String> {
        lib.resolve("p1", host).unwrap().jump_hosts.into_iter().map(|j| j.jump.address).collect()
    }

    /// A Library with one Project "p1" holding the given Hosts.
    fn library(hosts: Vec<Host>) -> Library {
        let mut p = project(None, None);
        p.hosts = hosts.iter().map(|h| h.id.clone()).collect();
        Library { shells: None, projects: vec![p], hosts }
    }

    /// `library` plus an empty second Project "p2" ("Hermes", user "deploy").
    fn two_projects(hosts: Vec<Host>) -> Library {
        let mut lib = library(hosts);
        lib.projects.push(Project { id: "p2".into(), name: "Hermes".into(), hosts: vec![], ..project(None, Some("deploy")) });
        lib
    }

    #[test]
    fn moving_a_host_takes_it_to_the_end_of_the_other_project() {
        let mut lib = two_projects(vec![named("web"), named("db")]);
        lib.projects[1].hosts.push("other".into());
        assert!(lib.move_host("web", "p1", "p2"));
        assert_eq!(lib.projects[0].hosts, ["db"]);
        assert_eq!(lib.projects[1].hosts, ["other", "web"]);
        // It now takes the new Project's defaults.
        assert_eq!(lib.resolve("p2", "web").unwrap().user.as_deref(), Some("deploy"));
    }

    #[test]
    fn moving_a_host_already_in_the_other_project_only_leaves_this_one() {
        let mut lib = two_projects(vec![named("web")]);
        lib.projects[1].hosts.push("web".into());
        assert!(lib.move_host("web", "p1", "p2"));
        assert!(lib.projects[0].hosts.is_empty());
        assert_eq!(lib.projects[1].hosts, ["web"]);
    }

    #[test]
    fn reordering_a_host_moves_it_within_its_project() {
        let mut lib = library(vec![named("a"), named("b"), named("c")]);
        lib.projects[0].hosts = vec!["a".into(), "b".into(), "c".into()];
        assert!(lib.reorder_host("p1", "a", 3));
        assert_eq!(lib.projects[0].hosts, ["b", "c", "a"]);
        assert!(lib.reorder_host("p1", "a", 0));
        assert_eq!(lib.projects[0].hosts, ["a", "b", "c"]);
        assert!(lib.reorder_host("p1", "c", 1));
        assert_eq!(lib.projects[0].hosts, ["a", "c", "b"]);
        assert!(lib.reorder_host("p1", "a", 99), "past the end goes to the bottom");
        assert_eq!(lib.projects[0].hosts, ["c", "b", "a"]);
    }

    #[test]
    fn reordering_a_host_onto_its_own_place_changes_nothing() {
        let mut lib = library(vec![named("a"), named("b")]);
        lib.projects[0].hosts = vec!["a".into(), "b".into()];
        let before = lib.clone();
        assert!(!lib.reorder_host("p1", "a", 0));
        assert!(!lib.reorder_host("p1", "a", 1));
        assert!(!lib.reorder_host("p1", "gone", 0));
        assert!(!lib.reorder_host("nowhere", "a", 2));
        assert_eq!(lib, before);
    }

    #[test]
    fn projects_reorder_as_whole_groups_and_persist() {
        let mut lib = two_projects(vec![named("web"), named("db")]);
        let mut third = project(None, None);
        third.id = "p3".into();
        lib.projects.push(third);
        let original = lib.clone();
        assert!(lib.reorder_project("p1", 3));
        assert_eq!(lib.projects.iter().map(|p| p.id.as_str()).collect::<Vec<_>>(), ["p2", "p3", "p1"]);
        assert_eq!(lib.projects[2], original.projects[0]);
        assert_eq!(lib.hosts, original.hosts);
        assert!(lib.reorder_project("p1", 0));
        assert_eq!(lib, original);
        assert!(lib.reorder_project("p1", usize::MAX));
        let dir = std::env::temp_dir().join(format!("komando-project-order-{}", new_id()));
        lib.save(&dir).unwrap();
        assert_eq!(Library::load(&dir).unwrap(), lib);
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// The files beside `path` whose names start with its own, other than itself.
    fn kept_beside(path: &Path) -> Vec<PathBuf> {
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        let mut kept: Vec<_> = fs::read_dir(path.parent().unwrap())
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .filter(|p| p.file_name().unwrap().to_string_lossy().starts_with(&format!("{name}.broken-")))
            .collect();
        kept.sort();
        kept
    }

    #[test]
    fn an_unreadable_library_is_moved_aside() {
        let dir = std::env::temp_dir().join(format!("komando-library-aside-{}", new_id()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join(Library::FILE);
        fs::write(&path, "hosts = [oops").unwrap();
        let err = Library::load(&dir).unwrap_err();
        let unreadable = Unreadable::keep(&path, &err, "your hosts", "Hosts and Projects");
        assert_eq!(unreadable.in_place, None);
        let [aside] = kept_beside(&path).try_into().unwrap();
        assert!(unreadable.message.contains(&aside.display().to_string()), "{}", unreadable.message);
        assert_eq!(fs::read_to_string(&aside).unwrap(), "hosts = [oops");
        assert!(!path.exists());
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn an_unreadable_library_that_cannot_be_moved_is_copied_aside() {
        let dir = std::env::temp_dir().join(format!("komando-library-copy-{}", new_id()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join(Library::FILE);
        fs::write(&path, "hosts = [oops").unwrap();
        let aside = set_aside(&path, |_, _| Err(std::io::Error::other("held open"))).unwrap();
        assert_eq!(kept_beside(&path), [aside.clone()]);
        assert_eq!(fs::read_to_string(&aside).unwrap(), "hosts = [oops");
        assert_eq!(fs::read_to_string(&path).unwrap(), "hosts = [oops");
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_file_that_cannot_be_kept_aside_is_not_to_be_saved_over() {
        let dir = std::env::temp_dir().join(format!("komando-unreadable-stuck-{}", new_id()));
        // A folder where the file should be: it can't be copied, like a file that can't be read.
        let path = dir.join(Settings::FILE);
        fs::create_dir_all(&path).unwrap();
        let err = anyhow::anyhow!("permission denied");
        let held = |_: &Path, _: &Path| Err(std::io::Error::other("held open"));
        let unreadable = Unreadable::keep_with(&path, &err, "your settings", "settings", held);
        assert_eq!(unreadable.in_place, Some(path.clone()));
        assert!(unreadable.message.contains("Changes to settings won't be saved"), "{}", unreadable.message);
        assert!(unreadable.message.contains(&path.display().to_string()), "{}", unreadable.message);
        assert!(kept_beside(&path).is_empty());
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_library_that_cannot_be_moved_or_copied_is_reported() {
        let dir = std::env::temp_dir().join(format!("komando-library-stuck-{}", new_id()));
        // A folder where the file should be: it can't be copied, like a file that can't be read.
        let path = dir.join(Library::FILE);
        fs::create_dir_all(&path).unwrap();
        let err = set_aside(&path, |_, _| Err(std::io::Error::other("held open"))).unwrap_err();
        assert!(format!("{err:#}").contains("held open"), "{err:#}");
        assert!(kept_beside(&path).is_empty(), "no half copy is left behind");
        assert!(path.is_dir());
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn project_reorder_ignores_unchanged_and_missing_targets() {
        let mut lib = two_projects(vec![named("web")]);
        let before = lib.clone();
        assert!(!lib.reorder_project("p1", 0));
        assert!(!lib.reorder_project("p1", 1));
        assert!(!lib.reorder_project("p2", 2));
        assert!(!lib.reorder_project("missing", 0));
        assert_eq!(lib, before);
        lib.projects.clear();
        assert!(!lib.reorder_project("p1", 0));
    }

    #[test]
    fn moving_a_host_nowhere_changes_nothing() {
        let mut lib = two_projects(vec![named("web")]);
        let before = lib.clone();
        assert!(!lib.move_host("web", "p1", "p1"));
        assert!(!lib.move_host("web", "p2", "p1"), "not in p2");
        assert!(!lib.move_host("web", "p1", "gone"));
        assert!(!lib.move_host("gone", "p1", "p2"));
        assert_eq!(lib, before);
    }

    #[test]
    fn minimize_to_tray_defaults_on_and_remembers_opt_out() {
        let mut settings: Settings = toml::from_str("font_size = 16.0").unwrap();
        assert!(settings.minimize_to_tray);
        settings.minimize_to_tray = false;
        let loaded: Settings = toml::from_str(&toml::to_string(&settings).unwrap()).unwrap();
        assert!(!loaded.minimize_to_tray);
    }

    #[test]
    fn disconnect_on_close_to_tray_defaults_on_and_remembers_opt_out() {
        let mut settings: Settings = toml::from_str("font_size = 16.0").unwrap();
        assert!(settings.disconnect_on_close_to_tray, "older settings files disconnect too");
        settings.disconnect_on_close_to_tray = false;
        let loaded: Settings = toml::from_str(&toml::to_string(&settings).unwrap()).unwrap();
        assert!(!loaded.disconnect_on_close_to_tray);
    }

    #[test]
    fn command_history_defaults_on_and_remembers_opt_out() {
        let mut settings: Settings = toml::from_str("font_size = 16.0").unwrap();
        assert!(settings.command_history, "older settings files keep a history");
        settings.command_history = false;
        let loaded: Settings = toml::from_str(&toml::to_string(&settings).unwrap()).unwrap();
        assert!(!loaded.command_history);
    }

    #[test]
    fn a_host_without_jump_hosts_connects_directly() {
        let lib = library(vec![named("web")]);
        assert!(jump_addresses(&lib, "web").is_empty());
    }

    #[test]
    fn project_jump_hosts_come_first_then_the_hosts_own() {
        let mut web = named("web");
        web.jump_hosts = vec![jump("inner.lan")];
        let mut lib = library(vec![web]);
        lib.projects[0].jump_hosts = vec![jump("edge.example.com"), jump("bastion.lan")];
        assert_eq!(jump_addresses(&lib, "web"), ["edge.example.com", "bastion.lan", "inner.lan"]);
        let owners: Vec<_> = lib.resolve("p1", "web").unwrap().jump_hosts.into_iter().map(|j| j.owner).collect();
        assert_eq!(
            owners,
            [JumpOwner::Project("p1".into(), 0), JumpOwner::Project("p1".into(), 1), JumpOwner::Host("web".into(), 0)]
        );
    }

    #[test]
    fn a_host_can_switch_the_project_jump_hosts_off() {
        let mut web = named("web");
        web.use_project_jump_hosts = false;
        web.jump_hosts = vec![jump("inner.lan")];
        let mut lib = library(vec![web, named("db")]);
        lib.projects[0].jump_hosts = vec![jump("bastion.lan")];
        assert_eq!(jump_addresses(&lib, "web"), ["inner.lan"]);
        assert_eq!(jump_addresses(&lib, "db"), ["bastion.lan"]);
    }

    /// Regression: the host and project forms refuse a Host whose own forward listens on the
    /// same port, in the same place, as its Project's. Moving a Host between Projects must not
    /// produce that combination either.
    #[test]
    fn moving_a_host_never_gives_it_two_forwards_on_one_port() {
        let mut web = named("web");
        web.port_forwards = vec![PortForward::remote(9000, "localhost", 4000)];
        let mut lib = two_projects(vec![web]);
        lib.projects[1].port_forwards = vec![PortForward::remote(9000, "localhost", 3000)];
        assert!(lib.forward_clash("web", "p2").is_some());
        assert!(!lib.move_host("web", "p1", "p2"), "moved into a clash");
        assert_eq!(lib.projects[0].hosts, ["web"]);
        // The same port on the other side (this computer, not the host) is no clash.
        lib.hosts[0].port_forwards = vec![PortForward::local(9000, "localhost", 4000)];
        assert!(lib.move_host("web", "p1", "p2"));
        // Nor is one the Host doesn't open: it leaves out its Project's forwards.
        lib.hosts[0].port_forwards = vec![PortForward::remote(9000, "localhost", 4000)];
        lib.hosts[0].use_project_forwards = false;
        lib.projects[0].port_forwards = vec![PortForward::remote(9000, "localhost", 3000)];
        assert!(lib.move_host("web", "p2", "p1"));
    }

    /// Found by fuzzing (`fuzz::targets::fuzz_mobaxterm_files`): `toml` 0.8's writer counted
    /// quotes in a row in a `u8`, so a name, address or Startup Command with 256 of them
    /// panicked on save in builds with overflow checks (dev, `ci`, tests). Any import or form
    /// can bring such a string in. Komando now writes with `toml` 1.
    #[test]
    fn names_with_long_runs_of_quotes_save() {
        for quote in ["'", "\"", "\\"] {
            let mut host = named("web");
            host.name = quote.repeat(256);
            host.address = format!("{}\"'\\", quote.repeat(300));
            host.startup_commands = vec![StartupCommand::new(format!("echo {}", quote.repeat(1000)))];
            let lib = library(vec![host]);
            let text = toml::to_string_pretty(&lib).unwrap();
            assert_eq!(toml::from_str::<Library>(&text).unwrap(), lib);
            assert_eq!(toml_0_8::from_str::<Library>(&text).unwrap(), lib, "older Komando can't read it");
        }
    }

    /// Files written now open in Komando 0.8 and older, which read them with `toml` 0.8 (TOML
    /// 1.0): `toml` 1 follows TOML 1.1, whose new syntax those can't read.
    #[test]
    fn saved_files_open_in_older_versions() {
        let mut web = named("web");
        web.address = "fe80::1%eth0".into();
        web.user = Inherited::Own("ops\u{e9}".into());
        web.port_forwards = vec![PortForward::socks(1080), PortForward { open_to_network: true, ..PortForward::remote(9000, "::1", 3000) }];
        web.startup_commands = vec![StartupCommand::new("printf 'a\tb\u{1b}[31m\u{7f}\\n' \"$HOME\"")];
        web.jump_hosts = vec![JumpHost { user: Some("j".into()), ..JumpHost::new("bastion.lan") }];
        let mut lib = two_projects(vec![web, named("db")]);
        lib.projects[0].default_user = Some("multi\nline".into());
        lib.shells = Some(vec![LocalShell { name: "bash \"login\"".into(), program: "C:\\x\\bash.exe".into(), args: vec!["-l".into(), String::new()], icon: None, hidden: true }]);
        let text = toml::to_string_pretty(&lib).unwrap();
        assert_eq!(toml_0_8::from_str::<Library>(&text).unwrap(), lib, "{text}");
        let settings = Settings { font_family: "Cascadia \"Code\"".into(), ..Settings::default() };
        let text = toml::to_string_pretty(&settings).unwrap();
        assert_eq!(toml_0_8::from_str::<Settings>(&text).unwrap(), settings, "{text}");
        let tabs = OpenTabs { tabs: vec![SavedTab::Shell { project: None, shell: "pwsh".into(), name: Some("\u{1F600}".into()) }], active: Some(0) };
        assert_eq!(toml_0_8::from_str::<OpenTabs>(&toml::to_string_pretty(&tabs).unwrap()).unwrap(), tabs);
    }

    #[test]
    fn port_forwards_open_the_projects_first_unless_a_host_leaves_them_out() {
        let mut web = named("web");
        web.port_forwards = vec![PortForward::socks(1080)];
        let mut db = named("db");
        db.use_project_forwards = false;
        db.port_forwards = vec![PortForward::local(5432, "localhost", 5432)];
        let mut lib = library(vec![web, db]);
        lib.projects[0].port_forwards = vec![PortForward::local(8080, "intranet.lan", 80)];
        let forwards = |host: &str| lib.resolve("p1", host).unwrap().port_forwards;
        assert_eq!(forwards("web"), [PortForward::local(8080, "intranet.lan", 80), PortForward::socks(1080)]);
        assert_eq!(forwards("db"), [PortForward::local(5432, "localhost", 5432)]);
    }

    #[test]
    fn port_forwards_round_trip_and_leave_defaults_out() {
        let mut web = named("web");
        web.port_forwards = vec![
            PortForward::local(5432, "db.internal", 5432),
            PortForward { open_to_network: true, ..PortForward::remote(8080, "localhost", 3000) },
            PortForward::socks(1080),
        ];
        web.use_project_forwards = false;
        let lib = library(vec![web]);
        let text = toml::to_string(&lib).unwrap();
        assert!(text.contains("use_project_forwards = false"), "{text}");
        let socks = text.split("[[hosts.port_forwards]]").nth(3).unwrap();
        assert!(!socks.contains("to_host") && !socks.contains("to_port") && !socks.contains("open_to_network"), "{socks}");
        let back: Library = toml::from_str(&text).unwrap();
        assert_eq!(back, lib);
        // Files from before Port Forwards read as a Host without any that takes the Project's.
        let old: Library = toml::from_str("[[hosts]]\nid = \"h\"\nname = \"h\"\naddress = \"h.lan\"\n").unwrap();
        assert!(old.hosts[0].port_forwards.is_empty() && old.hosts[0].use_project_forwards);
    }

    #[test]
    fn port_forward_labels_say_which_way_they_go() {
        assert_eq!(PortForward::local(5432, "db.internal", 5432).label(), "5432 → db.internal:5432");
        assert_eq!(PortForward::remote(8080, "localhost", 3000).label(), "Remote 8080 → localhost:3000");
        assert_eq!(PortForward::socks(1080).label(), "SOCKS on 1080");
        assert_eq!(PortForward::local(8443, "fd00::5", 443).target(), "[fd00::5]:443");
    }

    #[test]
    fn a_saved_jump_host_can_be_changed_where_it_is_saved() {
        let mut web = named("web");
        web.jump_hosts = vec![jump("inner.lan")];
        let mut lib = library(vec![web]);
        lib.projects[0].jump_hosts = vec![jump("bastion.lan")];
        lib.jump_host_mut(&JumpOwner::Project("p1".into(), 0)).unwrap().user = Some("ops".into());
        lib.jump_host_mut(&JumpOwner::Host("web".into(), 0)).unwrap().credential = Some("c1".into());
        assert!(lib.jump_host_mut(&JumpOwner::Host("web".into(), 1)).is_none());
        let jumps = lib.resolve("p1", "web").unwrap().jump_hosts;
        assert_eq!(jumps[0].jump.user.as_deref(), Some("ops"));
        assert_eq!(jumps[1].jump.credential.as_deref(), Some("c1"));
        assert_eq!(lib.credential_users("c1"), 1);
    }

    #[test]
    fn jump_hosts_round_trip_and_defaults_are_left_out() {
        let mut web = named("web");
        web.jump_hosts = vec![JumpHost { address: "bastion".into(), port: 2222, user: Some("ops".into()), credential: Some("c1".into()) }, jump("inner")];
        let text = toml::to_string_pretty(&web).unwrap();
        assert!(!text.contains("use_project_jump_hosts"), "on by default: {text}");
        assert!(!text.contains("jump_host ="), "the old key is never written: {text}");
        assert_eq!(toml::from_str::<Host>(&text).unwrap(), web);
        web.use_project_jump_hosts = false;
        let text = toml::to_string_pretty(&web).unwrap();
        assert!(text.contains("use_project_jump_hosts = false"), "{text}");
        assert_eq!(web.jump_hosts[0].label(), "ops@bastion:2222");
        assert_eq!(web.jump_hosts[1].label(), "inner");
    }

    /// A Library as Komando 0.3.4 wrote it, with Jump Hosts saved as Hosts, after loading.
    fn converted(text: &str) -> Library {
        let mut lib: Library = toml::from_str(text).unwrap();
        lib.convert_legacy_jump_hosts();
        lib
    }

    const LEGACY: &str = r#"
        [[projects]]
        id = "p1"
        name = "Atlas"
        color = "red"
        default_user = "deploy"
        credential = "atlas-key"
        jump_host = "bastion"
        hosts = ["web", "db", "bastion", "direct", "deep"]

        [[projects]]
        id = "p2"
        name = "Infra"
        color = "green"
        default_user = "ops"
        hosts = ["edge", "inner"]

        [[hosts]]
        id = "web"
        name = "web"
        address = "web.lan"

        [[hosts]]
        id = "db"
        name = "db"
        address = "db.lan"

        [[hosts]]
        id = "bastion"
        name = "bastion"
        address = "bastion.example.com"
        port = 2222
        credential = "bastion-pw"

        [[hosts]]
        id = "direct"
        name = "direct"
        address = "direct.lan"
        jump_host = false

        [[hosts]]
        id = "deep"
        name = "deep"
        address = "deep.lan"
        jump_host = "inner"

        [[hosts]]
        id = "edge"
        name = "edge"
        address = "edge.example.com"
        user = "root"

        [[hosts]]
        id = "inner"
        name = "inner"
        address = "inner.lan"
        jump_host = "edge"
    "#;

    #[test]
    fn saved_host_jump_hosts_become_typed_in_ones() {
        let lib = converted(LEGACY);
        let bastion = JumpHost { address: "bastion.example.com".into(), port: 2222, user: Some("deploy".into()), credential: Some("bastion-pw".into()) };
        assert_eq!(lib.projects[0].jump_hosts, [bastion]);
        // Hosts that took the Project's still do; the Project's Jump Host itself connects directly.
        assert_eq!(jump_addresses(&lib, "web"), ["bastion.example.com"]);
        assert!(lib.host("db").unwrap().use_project_jump_hosts);
        assert!(jump_addresses(&lib, "bastion").is_empty());
        assert!(jump_addresses(&lib, "direct").is_empty());
        // A chain through another Project: each takes that Project's settings.
        let deep = lib.host("deep").unwrap();
        assert!(!deep.use_project_jump_hosts);
        let edge = JumpHost { address: "edge.example.com".into(), port: 22, user: Some("root".into()), credential: None };
        let inner = JumpHost { address: "inner.lan".into(), port: 22, user: Some("ops".into()), credential: None };
        assert_eq!(deep.jump_hosts, [edge.clone(), inner]);
        assert_eq!(lib.host("inner").unwrap().jump_hosts, [edge]);
        // The Hosts stay.
        assert_eq!(lib.hosts.len(), 7);
    }

    #[test]
    fn converted_libraries_save_without_the_old_keys() {
        let lib = converted(LEGACY);
        let text = toml::to_string_pretty(&lib).unwrap();
        assert!(!text.contains("jump_host ="), "{text}");
        assert_eq!(converted(&text), lib, "converting again changes nothing");
    }

    #[test]
    fn a_legacy_jump_host_cycle_or_missing_host_ends_the_chain() {
        let text = r#"
            [[projects]]
            id = "p1"
            name = "Atlas"
            color = "red"
            hosts = ["a", "b", "c"]

            [[hosts]]
            id = "a"
            name = "a"
            address = "a.lan"
            jump_host = "b"

            [[hosts]]
            id = "b"
            name = "b"
            address = "b.lan"
            jump_host = "a"

            [[hosts]]
            id = "c"
            name = "c"
            address = "c.lan"
            jump_host = "gone"
        "#;
        let lib = converted(text);
        assert_eq!(jump_addresses(&lib, "a"), ["a.lan", "b.lan"]);
        assert!(jump_addresses(&lib, "c").is_empty());
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
    fn startup_commands_that_wait_round_trip_and_plain_ones_stay_plain() {
        let mut web = named("web");
        web.startup_commands = vec!["sudo -i".into(), StartupCommand::after("# ", "cd /srv")];
        let mut lib = library(vec![web]);
        lib.projects[0].startup_commands = vec![StartupCommand::after("$ ", "uptime")];
        let text = toml::to_string_pretty(&lib).unwrap();
        assert!(text.contains("\"sudo -i\""), "{text}");
        let back: Library = toml::from_str(&text).unwrap();
        assert_eq!(back, lib);

        // Files from before waiting existed.
        let old: Library = toml::from_str("[[hosts]]
id = \"a\"
name = \"a\"
address = \"a\"
startup_commands = [\"uptime\"]
").unwrap();
        assert_eq!(old.hosts[0].startup_commands, ["uptime"]);
    }

    #[test]
    fn a_wait_for_a_password_question_is_a_saved_password() {
        assert!(StartupCommand::after("[sudo] Password for ana:", "hunter2").answers_password());
        assert!(StartupCommand::after("Enter passphrase", "x").answers_password());
        assert!(StartupCommand::after("[sudo] Contraseña para ana:", "hunter2").answers_password());
        assert!(StartupCommand::after("[sudo] пароль для ana:", "hunter2").answers_password());
        assert!(StartupCommand::after("[sudo: authenticate] Password:", "hunter2").answers_password());
        assert!(!StartupCommand::after("# ", "cd /srv").answers_password());
        assert!(StartupCommand::after("[sudo] wachtwoord voor ana:", "hunter2").answers_password());
        assert!(StartupCommand::after("Enter PIN", "1234").answers_password());
        assert!(!StartupCommand::after("Spinning up", "ls").answers_password());
        assert!(!StartupCommand::new("passwd").answers_password());
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
    fn duplicating_a_host_copies_it_below_the_original_in_that_project() {
        let mut web = named("web");
        web.jump_hosts = vec![jump("bastion.lan")];
        web.credential = Inherited::Own("web-key".into());
        web.startup_commands = vec!["uptime".into()];
        let mut lib = library(vec![web, named("db"), named("bastion")]);

        let id = lib.duplicate_host("p1", "web").unwrap();
        let copy = lib.host(&id).unwrap();
        assert_ne!(id, "web");
        assert_eq!(copy.name, "web (copy)");
        assert_eq!(copy.address, "web.lan");
        assert_eq!(copy.credential, Inherited::Own("web-key".into()));
        assert_eq!(copy.jump_hosts, [jump("bastion.lan")]);
        assert_eq!(copy.startup_commands, ["uptime"]);
        assert_eq!(lib.projects[0].hosts, ["web", id.as_str(), "db", "bastion"]);

        let again = lib.duplicate_host("p1", "web").unwrap();
        assert_eq!(lib.host(&again).unwrap().name, "web (copy 2)");
        assert!(lib.duplicate_host("p1", "missing").is_none());
        assert!(lib.duplicate_host("nope", "web").is_none());
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
        assert!(!r.signs_in_with_vault());
    }

    #[test]
    fn a_saved_credential_anywhere_on_the_way_needs_the_vault() {
        assert!(resolve(&host(Some("own"), None, None), &project(None, None)).signs_in_with_vault());
        assert!(resolve(&host(None, None, None), &project(Some("shared"), None)).signs_in_with_vault());

        let mut h = host(None, None, None);
        h.jump_hosts = vec![jump("bastion.lan")];
        assert!(!resolve(&h, &project(None, None)).signs_in_with_vault(), "jump host asks for its password");
        h.jump_hosts[0].credential = Some("bastion-key".into());
        assert!(resolve(&h, &project(None, None)).signs_in_with_vault());

        // A Project's Jump Host counts too, unless the Host switches them off.
        let mut p = project(None, None);
        p.jump_hosts = vec![JumpHost { credential: Some("gate".into()), ..jump("gate.lan") }];
        let mut h = host(None, None, None);
        assert!(resolve(&h, &p).signs_in_with_vault());
        h.use_project_jump_hosts = false;
        assert!(!resolve(&h, &p).signs_in_with_vault());
    }

    #[test]
    fn standalone_shells_default_to_detected_ones() {
        let lib = Library::default();
        assert_eq!(lib.standalone_shells(), default_shells());
        let custom = vec![LocalShell { name: "WSL".into(), program: "wsl.exe".into(), ..LocalShell::default() }];
        let lib = Library { shells: Some(custom.clone()), ..Library::default() };
        assert_eq!(lib.standalone_shells(), custom);
        // An explicit empty list means "no standalone shells", not "the defaults".
        let lib = Library { shells: Some(vec![]), ..Library::default() };
        assert!(lib.standalone_shells().is_empty());
    }

    #[test]
    fn restoring_detected_shells_keeps_your_own() {
        let mut lib = Library::default();
        let detected = default_shells();
        let first = detected[0].name.clone();
        lib.standalone_shells_mut().retain(|s| s.name != first);
        lib.standalone_shells_mut().push(LocalShell { name: "WSL".into(), program: "wsl.exe".into(), ..LocalShell::default() });
        if let Some(other) = lib.standalone_shells_mut().first_mut() {
            other.hidden = true;
        }
        lib.restore_detected_shells();
        let shells = lib.standalone_shells();
        assert!(shells.iter().any(|s| s.name == "WSL"));
        for d in &detected {
            assert!(shells.iter().any(|s| s.name == d.name && !s.hidden), "{} is back and shown", d.name);
        }
    }

    #[test]
    fn hidden_shells_round_trip_and_old_files_still_load() {
        let lib: Library = toml::from_str("[[shells]]\nname = \"cmd\"\nprogram = \"cmd.exe\"\n").unwrap();
        assert!(!lib.standalone_shells()[0].hidden);
        assert_eq!(lib.standalone_shells()[0].icon, None);
        let text = toml::to_string_pretty(&lib).unwrap();
        assert!(!text.contains("hidden"), "only written when set: {text}");
        let mut lib = lib;
        lib.standalone_shells_mut()[0].hidden = true;
        lib.standalone_shells_mut()[0].icon = Some("devicon:ubuntu".into());
        let back: Library = toml::from_str(&toml::to_string_pretty(&lib).unwrap()).unwrap();
        assert!(back.standalone_shells()[0].hidden);
        assert_eq!(back.standalone_shells()[0].icon.as_deref(), Some("devicon:ubuntu"));
    }

    #[test]
    fn arguments_split_and_join_with_quotes() {
        assert_eq!(split_args(r#"-NoLogo  -d "Ubuntu 22.04" x"#), ["-NoLogo", "-d", "Ubuntu 22.04", "x"]);
        assert_eq!(split_args("  "), Vec::<String>::new());
        assert_eq!(split_args(r#"a "" b"#), ["a", "", "b"]);
        let shell = LocalShell { args: split_args(r#"-d "Ubuntu 22.04""#), ..LocalShell::default() };
        assert_eq!(shell.args_line(), r#"-d "Ubuntu 22.04""#);
        assert_eq!(split_args(&shell.args_line()), shell.args);
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
        assert!(lib.hosts[1].use_project_jump_hosts);
        assert!(lib.hosts[1].run_project_commands);
    }

    #[test]
    fn switched_off_settings_are_written_as_false() {
        let mut h = host(None, None, None);
        h.user = Inherited::Off;
        h.credential = Inherited::Own("c1".into());
        let text = toml::to_string(&h).unwrap();
        assert!(text.contains("user = false"), "{text}");
        assert!(text.contains("credential = \"c1\""), "{text}");
        assert!(!text.contains("jump"), "defaults are left out: {text}");
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
    #[test]
    #[ignore]
    fn stress_project_host_resolution() {
        for projects in [10, 100, 500] {
            let mut library = Library::default();
            for pi in 0..projects {
                let mut project = Project::new(format!("Project {pi}"), "blue", "server");
                for hi in 0..20 {
                    let host = Host::new(format!("p{pi}-host{hi}"), "127.0.0.1");
                    project.hosts.push(host.id.clone());
                    library.hosts.push(host);
                }
                library.projects.push(project);
            }
            for indexed in [false, true] {
                let start = std::time::Instant::now();
                for _ in 0..30 {
                    let resolved: usize = if indexed {
                        let index = library.host_indices();
                        library.projects.iter().map(|p| p.hosts.iter().filter_map(|id| index.get(id.as_str())).count()).sum()
                    } else {
                        library.projects.iter().map(|p| library.hosts_of(p).count()).sum()
                    };
                    assert_eq!(std::hint::black_box(resolved), projects * 20);
                }
                eprintln!("project resolution {projects} projects / {} hosts indexed={indexed}: {:?}/frame", projects * 20, start.elapsed() / 30);
            }
        }
    }

    #[test]
    fn indexed_hosts_match_ordered_lookup_after_edits() {
        let mut library = Library::default();
        let first = Host::new("first", "localhost");
        let second = Host::new("second", "localhost");
        let mut duplicate = first.clone();
        duplicate.name = "duplicate".into();
        let mut project = Project::new("shared", "blue", "server");
        project.hosts = vec![second.id.clone(), "missing".into(), first.id.clone(), second.id.clone()];
        library.hosts = vec![first, second, duplicate];
        for _ in 0..2 {
            let index = library.host_indices();
            let indexed: Vec<_> = project.hosts.iter().filter_map(|id| index.get(id.as_str())).map(|i| &library.hosts[*i]).collect();
            assert_eq!(indexed, library.hosts_of(&project).collect::<Vec<_>>());
            library.hosts.remove(0);
        }
    }

    #[cfg(not(windows))]
    #[test]
    fn unix_shell_falls_back_when_shell_is_unusable() {
        use std::ffi::OsString;
        let usual = if cfg!(target_os = "macos") { "/bin/zsh" } else { "/bin/bash" };
        let fallback = if Path::new(usual).is_file() { usual } else { "/bin/sh" };
        assert_eq!(unix_shell(Some(OsString::from("/bin/sh"))), "/bin/sh");
        for unusable in [None, Some(""), Some("/nonexistent/komando-shell"), Some("zsh")] {
            assert_eq!(unix_shell(unusable.map(OsString::from)), fallback);
        }
    }
}
