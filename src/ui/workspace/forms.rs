//! The host and project forms.
//!
//! Both edit the settings a Project hands down to its Hosts in the same way: the Project sets
//! a default (user, Credential, Jump Hosts, Startup Commands), and in the host form each one
//! is a choice between taking the Project's, setting the Host's own, or switching it off.
//! Jump Hosts, Port Forwards and Startup Commands add up instead: the Project's come first,
//! then the Host's, and the host form has a switch to leave the Project's out.

use gpui::prelude::FluentBuilder as _;
use gpui::{AnyElement, App, ClickEvent, Context, Entity, FontWeight, Hsla, SharedString, Window, div, prelude::*, px};
use gpui_component::input::{Input, InputEvent, InputState};
use gpui_component::tooltip::Tooltip;

use super::settings::{group, setting_row, settings_block, stacked};
use super::{FIRST_PROJECT_COLOR, FIRST_PROJECT_NAME, Overlay, Workspace, field_label, form_field, icon_picker, kbd, segmented, switch};
use crate::model::{AgentUse, CredentialId, Desktop, ForwardKind, Host, Inherited, JumpHost, Library, PortForward, Project, Protocol, StartupCommand};
use crate::session::agent::{self, Agent};
use crate::theme::{AppPalette, project_color};
use crate::ui::widgets::{ButtonKind, badge, button, icon_button, icon_el, icon_text_button};
use crate::vault::{Credential, Secret};

/// Where a Host takes a setting from, as picked in the host form.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Source {
    Project,
    Own,
    Off,
}

impl Source {
    fn of<T>(setting: &Inherited<T>) -> Self {
        match setting {
            Inherited::Project => Source::Project,
            Inherited::Own(_) => Source::Own,
            Inherited::Off => Source::Off,
        }
    }
}

/// Which of the SSH Agent's keys a Host signs in with, as picked in the host form.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum AgentChoice {
    Any,
    One,
    Off,
}

/// The agent's keys as the host form lists them: `None` while the agent is asked.
type AgentKeys = Option<Result<Vec<agent::Key>, String>>;

/// Under Agent Forwarding when it is on: what it lets the server do.
const FORWARD_AGENT_WARNING: &str = "git and ssh on the host can sign in with your agent's keys while you're connected. \
    So can anyone with root there: forward only to hosts you trust.";

/// How a Host or Project signs in, as picked in a form.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum SignIn {
    /// Host: the Project Credential (or asked, if the Project has none).
    Project,
    /// Keep the Credential already saved (editing only).
    Saved,
    /// Import a private key file into the Vault.
    KeyFile,
    /// No saved Credential: asked on connect, then offered for saving.
    Ask,
}

/// A page of the host or project form, picked in the form's side rail.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum FormTab {
    General,
    SignIn,
    JumpHosts,
    Forwards,
    Terminal,
    RemoteDesktop,
}

impl FormTab {
    const HOST: [FormTab; 6] = [Self::General, Self::SignIn, Self::JumpHosts, Self::Forwards, Self::Terminal, Self::RemoteDesktop];
    const PROJECT: [FormTab; 5] = [Self::General, Self::SignIn, Self::JumpHosts, Self::Forwards, Self::Terminal];

    fn label(self) -> &'static str {
        match self {
            Self::General => "General",
            Self::SignIn => "Sign-in",
            Self::JumpHosts => "Jump hosts",
            Self::Forwards => "Port forwards",
            Self::Terminal => "Terminal",
            Self::RemoteDesktop => "Remote desktop",
        }
    }

    fn icon(self) -> &'static str {
        match self {
            Self::General => "sliders-horizontal",
            Self::SignIn => "key-round",
            Self::JumpHosts => "waypoints",
            Self::Forwards => "arrow-right-left",
            Self::Terminal => "square-terminal",
            Self::RemoteDesktop => "monitor",
        }
    }

    /// The line under the page's title, for a project's form or a host's.
    fn about(self, project: bool) -> &'static str {
        match (self, project) {
            (Self::General, false) => "Its name in the sidebar, where to reach it, and its icon.",
            (Self::General, true) => "Its name, color and icon in the sidebar.",
            (Self::SignIn, false) => "Who Komando signs in as, and with which key or password.",
            (Self::SignIn, true) => "Defaults for its hosts. Each host can set its own instead.",
            (Self::JumpHosts, false) => "Hosts to sign in to on the way, such as a bastion.",
            (Self::JumpHosts, true) => "Every host in the project goes through these, unless it switches them off.",
            (Self::Forwards, false) => "Ports carried through its SSH connection while a session is connected.",
            (Self::Forwards, true) => "Every host in the project opens these, unless it leaves them out.",
            (Self::Terminal, false) => "What is typed when the shell opens, and how its output looks.",
            (Self::Terminal, true) => "What is typed into every host's shell, and how their output looks.",
            (Self::RemoteDesktop, _) => "RDP and VNC desktops, opened beside the host's terminal.",
        }
    }
}

/// A form's complaint, with the page to show it on (`None`: stay on the current one).
type FormError = (Option<FormTab>, String);

/// Attach a form's complaint to the page that has the field at fault.
fn on_tab(tab: FormTab) -> impl FnOnce(String) -> FormError {
    move |message| (Some(tab), message)
}

/// Add a Host to a Project, or edit one.
pub(super) struct HostForm {
    project_id: String,
    /// The Host being edited, by id; `None` adds one.
    editing: Option<String>,
    name: Entity<InputState>,
    address: Entity<InputState>,
    port: Entity<InputState>,
    user: Entity<InputState>,
    key_path: Entity<InputState>,
    commands: Vec<CommandRow>,
    user_source: Source,
    sign_in: SignIn,
    agent: AgentChoice,
    /// The key for [`AgentChoice::One`], by fingerprint.
    agent_key: Option<String>,
    agent_keys: AgentKeys,
    /// Agent Forwarding: the Project's (Project), on (Own) or off.
    forward_agent: Source,
    /// Go through the Project's Jump Hosts first.
    use_project_jumps: bool,
    jumps: Vec<JumpRow>,
    /// Open the Project's Port Forwards too.
    use_project_forwards: bool,
    forwards: Vec<ForwardRow>,
    run_project_commands: bool,
    /// Highlight Rules colour its output.
    highlight: bool,
    /// Programs on it may copy to the clipboard: always, never, or as Settings say (`None`).
    clipboard: Option<bool>,
    /// Its Remote Desktops: RDP, then VNC.
    desktops: [DesktopRow; 2],
    /// Opened from an unconfigured desktop's menu entry: show just that protocol's settings.
    desktop_setup: Option<Protocol>,
    /// What a click on the Host opens.
    opens: Protocol,
    icon: String,
    icon_query: Entity<InputState>,
    /// The page shown.
    tab: FormTab,
    error: Option<String>,
    unsaved: Unsaved,
}

/// Whether closing a host or project form throws anything away.
#[derive(Default)]
pub(super) struct Unsaved {
    /// What the form held when it opened (see [`UnsavedForm::values`]).
    opened_with: String,
    /// Closing it was asked for with changes made: "Discard your changes?" is on screen.
    pub(super) asking_discard: bool,
}

/// The host and project forms, which ask "Discard your changes?" before closing throws
/// something away.
pub(super) trait UnsavedForm {
    /// What the form's fields and choices hold (see [`field_values`]).
    fn values(&self, cx: &App) -> String;
    fn unsaved(&self) -> &Unsaved;
    fn unsaved_mut(&mut self) -> &mut Unsaved;
    fn name_field(&self) -> Entity<InputState>;

    /// Something was changed since the form opened.
    fn changed(&self, cx: &App) -> bool {
        self.values(cx) != self.unsaved().opened_with
    }

    /// Remember what the form holds as it opens, to compare against when it closes.
    fn opened(&mut self, cx: &App) {
        self.unsaved_mut().opened_with = self.values(cx);
    }
}

/// What a form's fields and choices hold, as one string: equal while nothing was changed.
/// Which page is shown, the icon search and errors aren't part of it.
fn field_values(fields: &[&Entity<InputState>], choices: String, cx: &App) -> String {
    let mut out = choices;
    for field in fields {
        out.push('\u{0}');
        out.push_str(&field.read(cx).value());
    }
    out
}

fn row_fields<'a>(commands: &'a [CommandRow], jumps: &'a [JumpRow], forwards: &'a [ForwardRow]) -> Vec<&'a Entity<InputState>> {
    let mut fields = vec![];
    for row in commands {
        fields.extend([&row.wait_for, &row.command]);
    }
    for row in jumps {
        fields.extend([&row.user, &row.address, &row.port, &row.password]);
    }
    for row in forwards {
        fields.extend([&row.port, &row.to_host, &row.to_port]);
    }
    fields
}

fn row_choices(commands: &[CommandRow], jumps: &[JumpRow], forwards: &[ForwardRow]) -> String {
    let jumps: Vec<_> = jumps.iter().map(|row| (&row.saved, row.remove_saved)).collect();
    let forwards: Vec<_> = forwards.iter().map(|row| (row.kind, row.open_to_network)).collect();
    format!("{}|{jumps:?}|{forwards:?}", commands.len())
}

impl UnsavedForm for HostForm {
    fn values(&self, cx: &App) -> String {
        let mut fields = vec![&self.name, &self.address, &self.port, &self.user, &self.key_path];
        fields.extend(row_fields(&self.commands, &self.jumps, &self.forwards));
        for desktop in &self.desktops {
            fields.extend([&desktop.port, &desktop.user, &desktop.domain, &desktop.password]);
        }
        let desktops: Vec<_> =
            self.desktops.iter().map(|d| (d.on, &d.saved, d.forget_saved, d.through_ssh)).collect();
        let choices = format!(
            "{:?}|{:?}|{:?}|{:?}|{:?}|{}|{}|{}|{}|{:?}|{desktops:?}|{:?}|{}|{}",
            self.user_source,
            self.sign_in,
            self.agent,
            self.agent_key,
            self.forward_agent,
            self.use_project_jumps,
            self.use_project_forwards,
            self.run_project_commands,
            self.highlight,
            self.clipboard,
            self.opens,
            self.icon,
            row_choices(&self.commands, &self.jumps, &self.forwards),
        );
        field_values(&fields, choices, cx)
    }

    fn unsaved(&self) -> &Unsaved {
        &self.unsaved
    }

    fn unsaved_mut(&mut self) -> &mut Unsaved {
        &mut self.unsaved
    }

    fn name_field(&self) -> Entity<InputState> {
        self.name.clone()
    }
}

impl UnsavedForm for ProjectForm {
    fn values(&self, cx: &App) -> String {
        let mut fields = vec![&self.name, &self.default_user, &self.key_path];
        fields.extend(row_fields(&self.commands, &self.jumps, &self.forwards));
        let choices = format!(
            "{}|{}|{:?}|{}|{}|{}",
            self.color,
            self.icon,
            self.sign_in,
            self.forward_agent,
            self.highlight,
            row_choices(&self.commands, &self.jumps, &self.forwards),
        );
        field_values(&fields, choices, cx)
    }

    fn unsaved(&self) -> &Unsaved {
        &self.unsaved
    }

    fn unsaved_mut(&mut self) -> &mut Unsaved {
        &mut self.unsaved
    }

    fn name_field(&self) -> Entity<InputState> {
        self.name.clone()
    }
}

/// One Remote Desktop in the host form: switched on or off, with its settings.
pub(super) struct DesktopRow {
    protocol: Protocol,
    on: bool,
    port: Entity<InputState>,
    /// RDP only.
    user: Entity<InputState>,
    domain: Entity<InputState>,
    /// A new password, saved to the Vault with the form.
    password: Entity<InputState>,
    /// The Credential it had when the form opened, kept unless replaced or forgotten.
    saved: Option<CredentialId>,
    forget_saved: bool,
    through_ssh: bool,
    /// Its toolbar's options, kept as they are through an edit.
    viewer: crate::model::ViewerOptions,
}

/// Create a Project, or edit one.
pub(super) struct ProjectForm {
    editing: Option<String>,
    name: Entity<InputState>,
    color: String,
    icon: String,
    icon_query: Entity<InputState>,
    default_user: Entity<InputState>,
    sign_in: SignIn,
    key_path: Entity<InputState>,
    /// Agent Forwarding for its hosts.
    forward_agent: bool,
    jumps: Vec<JumpRow>,
    forwards: Vec<ForwardRow>,
    commands: Vec<CommandRow>,
    /// Highlight Rules colour the output of its hosts and shells.
    highlight: bool,
    /// The page shown.
    tab: FormTab,
    error: Option<String>,
    unsaved: Unsaved,
}

/// The fields a host form and a project form both have, borrowed from the open one.
struct SharedForm<'a> {
    tab: &'a mut FormTab,
    error: &'a mut Option<String>,
    sign_in: &'a mut SignIn,
    key_path: &'a Entity<InputState>,
    jumps: &'a mut Vec<JumpRow>,
    forwards: &'a mut Vec<ForwardRow>,
    commands: &'a mut Vec<CommandRow>,
}

/// One Startup Command in a form's list: what it waits for (optional) and what it types.
pub(super) struct CommandRow {
    /// Tells rows apart in element ids while rows are added and removed.
    key: usize,
    wait_for: Entity<InputState>,
    command: Entity<InputState>,
}

/// One Jump Host in a form's list: user, address and port side by side, and its password.
pub(super) struct JumpRow {
    /// Tells rows apart in element ids while rows are added and removed.
    key: usize,
    user: Entity<InputState>,
    address: Entity<InputState>,
    port: Entity<InputState>,
    /// A new password, typed after "Set password"; saved to the Vault with the form.
    password: Entity<InputState>,
    /// The password field is shown. Closing it keeps what was typed.
    password_open: bool,
    /// The Credential it had when the form opened, kept unless replaced or removed.
    saved: Option<CredentialId>,
    remove_saved: bool,
}

/// One Port Forward in a form's list: which way, the port it listens on, where it goes.
pub(super) struct ForwardRow {
    /// Tells rows apart in element ids while rows are added and removed.
    key: usize,
    kind: ForwardKind,
    port: Entity<InputState>,
    /// Kept while the row is SOCKS, which has no address, for switching back.
    to_host: Entity<InputState>,
    to_port: Entity<InputState>,
    open_to_network: bool,
}

/// The Port Forwards of a form's rows, after `inherited` (the Project's, in the host form).
fn read_forward_rows(rows: &[ForwardRow], inherited: &[PortForward], cx: &App) -> Result<Vec<PortForward>, String> {
    let text = |field: &Entity<InputState>| field.read(cx).value().trim().to_string();
    let fields: Vec<ForwardFields> = rows
        .iter()
        .map(|row| ForwardFields {
            kind: row.kind,
            port: text(&row.port),
            to_host: text(&row.to_host),
            to_port: text(&row.to_port),
            open_to_network: row.open_to_network,
        })
        .collect();
    parse_forwards(&fields, inherited)
}

/// What one Port Forward row holds, as typed.
struct ForwardFields {
    kind: ForwardKind,
    port: String,
    to_host: String,
    to_port: String,
    open_to_network: bool,
}

/// Port Forwards from what was typed. Empty rows are skipped. An empty address is
/// "localhost" (the host itself, for a Local forward), and an empty port the one it listens
/// on. Two forwards can't listen on the same port in the same place, nor on one of
/// `inherited`'s.
fn parse_forwards(rows: &[ForwardFields], inherited: &[PortForward]) -> Result<Vec<PortForward>, String> {
    let mut forwards: Vec<(usize, PortForward)> = vec![];
    for (i, row) in rows.iter().enumerate() {
        let n = i + 1;
        let (port, to_host, to_port) = (row.port.trim(), row.to_host.trim(), row.to_port.trim());
        let has_target = row.kind.has_target();
        if port.is_empty() && (!has_target || (to_host.is_empty() && to_port.is_empty())) {
            continue;
        }
        let Some(port) = Some(port).filter(|p| !p.is_empty()).and_then(parse_port) else {
            return Err(format!("Type the port that port forward {n} listens on, from 1 to 65535."));
        };
        let forward = if has_target {
            let to_port = if to_port.is_empty() { Some(port) } else { parse_port(to_port) };
            let Some(to_port) = to_port else {
                return Err(format!("The port that port forward {n} goes to is a number from 1 to 65535."));
            };
            // Typed as [fd00::5] from habit: the brackets aren't part of the address.
            let to_host = to_host.trim_start_matches('[').trim_end_matches(']');
            let to_host = if to_host.is_empty() { "localhost" } else { to_host };
            PortForward { kind: row.kind, port, to_host: to_host.to_string(), to_port, open_to_network: row.open_to_network }
        } else {
            PortForward { open_to_network: row.open_to_network, ..PortForward::socks(port) }
        };
        let place = if forward.kind.listens_here() { "on this computer" } else { "on the host" };
        let same = |other: &PortForward| other.same_listener(&forward);
        if let Some(project) = inherited.iter().find(|other| same(other)) {
            return Err(format!(
                "Port forward {n} listens on port {port} {place}, as the project's \"{}\" does. Pick another port, or \
                 leave out the project's forwards.",
                project.label()
            ));
        }
        if let Some((earlier, _)) = forwards.iter().find(|(_, other)| same(other)) {
            return Err(format!("Port forwards {earlier} and {n} both listen on port {port} {place}. Pick another port for one of them."));
        }
        forwards.push((n, forward));
    }
    Ok(forwards.into_iter().map(|(_, forward)| forward).collect())
}

/// Whether one of a project's forwards listens on the same port, in the same place, as one of
/// the own forwards of `hosts` (its hosts that open the project's forwards too).
fn clash_with_hosts(project: &[PortForward], hosts: &[(&str, &[PortForward])]) -> Result<(), String> {
    for forward in project {
        let same = |other: &&PortForward| other.same_listener(forward);
        if let Some((host, own)) = hosts.iter().find_map(|(host, own)| own.iter().find(same).map(|own| (host, own))) {
            let place = if forward.kind.listens_here() { "on this computer" } else { "on the host" };
            return Err(format!(
                "The project's port forward \"{}\" listens on port {} {place}, as {host}'s own \"{}\" does. Pick another \
                 port, or have {host} leave out the project's forwards.",
                forward.label(),
                forward.port,
                own.label()
            ));
        }
    }
    Ok(())
}

/// A Jump Host read from a form row, before its new password (if any) is in the Vault.
struct JumpEntry {
    jump: JumpHost,
    new_password: Option<String>,
}

/// The Startup Commands of a form's rows. Empty rows are skipped; a row that waits for a
/// password question is refused, since its command would be a password saved unencrypted.
fn read_command_rows(rows: &[CommandRow], cx: &App) -> Result<Vec<StartupCommand>, String> {
    let mut commands = vec![];
    for (i, row) in rows.iter().enumerate() {
        let wait_for = row.wait_for.read(cx).value().to_string();
        let command = row.command.read(cx).value().trim().to_string();
        let wait_for = (!wait_for.trim().is_empty()).then_some(wait_for);
        let n = i + 1;
        let step = match (wait_for, command.is_empty()) {
            (None, true) => continue,
            (Some(_), true) => return Err(format!("Type the command to run in startup command {n}, or remove it.")),
            (wait_for, false) => StartupCommand { wait_for, command },
        };
        if step.answers_password() {
            return Err(format!(
                "Startup command {n} answers a password question. Startup commands are saved unencrypted, so they \
                 can't hold passwords: Komando stops at the question for you to type it, or save the host's password \
                 in the vault instead."
            ));
        }
        commands.push(step);
    }
    Ok(commands)
}

impl Workspace {
    // ---- host form ----------------------------------------------------------------------

    pub(super) fn open_new_host(&mut self, project_id: String, window: &mut Window, cx: &mut Context<Self>) {
        self.open_host_form(project_id, None, window, cx);
    }

    pub(super) fn open_edit_host(&mut self, project_id: String, host_id: String, window: &mut Window, cx: &mut Context<Self>) {
        self.open_host_form(project_id, Some(host_id), window, cx);
    }

    /// Configure a Remote Desktop without changing the Host until the form is saved.
    pub(super) fn set_up_desktop(&mut self, project_id: String, host_id: String, protocol: Protocol, window: &mut Window, cx: &mut Context<Self>) {
        if self.modal_open(cx) || !self.library.host(&host_id).is_some_and(|host| host.desktop_enabled(protocol)) {
            return;
        }
        self.open_edit_host(project_id, host_id, window, cx);
        if let Some(form) = self.host_form() {
            form.desktop_setup = Some(protocol);
            if let Some(row) = form.desktops.iter_mut().find(|row| row.protocol == protocol) {
                row.on = true;
                row.password.update(cx, |field, cx| field.focus(window, cx));
            }
        }
        cx.notify();
    }

    /// Copy a Host below itself in the Project it was picked from, then open the copy's form
    /// so its name and address can be changed.
    pub(super) fn duplicate_host(&mut self, project_id: String, host_id: String, window: &mut Window, cx: &mut Context<Self>) {
        if self.prompt_open(cx) {
            return;
        }
        let mut library = self.library.clone();
        let Some(copy) = library.duplicate_host(&project_id, &host_id) else { return };
        match self.commit_library(library) {
            Ok(()) => self.open_edit_host(project_id, copy, window, cx),
            Err(err) => self.show_toast(format!("Could not duplicate. {err}"), cx),
        }
        cx.notify();
    }

    fn open_host_form(&mut self, project_id: String, editing: Option<String>, window: &mut Window, cx: &mut Context<Self>) {
        // One dialog at a time: a Session's open question comes first.
        if self.prompt_open(cx) {
            return;
        }
        let host = editing.as_deref().and_then(|id| self.library.host(id)).cloned();
        let h = host.clone().unwrap_or_else(|| Host::new("", ""));
        let port = if h.port == 22 { String::new() } else { h.port.to_string() };
        let name = self.form_input("", &h.name, window, cx);
        let address = self.form_input("host name or IP", &h.address, window, cx);
        let port = self.form_input("22", &port, window, cx);
        let user = self.form_input("", h.user.own().map_or("", |u| u.as_str()), window, cx);
        let key_path = self.form_input(key_file_hint(), "", window, cx);
        let commands = h.startup_commands.iter().map(|c| self.command_row(Some(c), window, cx)).collect();
        let jumps = h.jump_hosts.iter().map(|j| self.jump_row(Some(j), window, cx)).collect();
        let forwards = h.port_forwards.iter().map(|f| self.forward_row(Some(f), window, cx)).collect();
        name.update(cx, |f, cx| f.focus(window, cx));
        let sign_in = match h.credential {
            Inherited::Project => SignIn::Project,
            Inherited::Own(_) => SignIn::Saved,
            Inherited::Off => SignIn::Ask,
        };
        let (agent, agent_key) = match &h.agent {
            AgentUse::Any => (AgentChoice::Any, None),
            AgentUse::Key(fingerprint) => (AgentChoice::One, Some(fingerprint.clone())),
            AgentUse::Off => (AgentChoice::Off, None),
        };
        let forward_agent = match h.forward_agent {
            None => Source::Project,
            Some(true) => Source::Own,
            Some(false) => Source::Off,
        };
        let icon_query = self.new_icon_query(window, cx);
        let desktops = [Protocol::Rdp, Protocol::Vnc].map(|protocol| {
            let mut row = self.desktop_row(protocol, h.desktop(protocol), window, cx);
            row.on = h.desktop_enabled(protocol);
            row
        });
        self.overlay = Some(Overlay::HostForm(HostForm {
            project_id,
            editing,
            name,
            address,
            port,
            user,
            key_path,
            commands,
            user_source: Source::of(&h.user),
            sign_in,
            agent,
            agent_key,
            agent_keys: None,
            forward_agent,
            use_project_jumps: h.use_project_jump_hosts,
            jumps,
            use_project_forwards: h.use_project_forwards,
            forwards,
            run_project_commands: h.run_project_commands,
            highlight: h.highlight,
            clipboard: h.clipboard,
            desktops,
            desktop_setup: None,
            opens: h.opens_with(),
            icon: h.icon.unwrap_or_else(|| "server".into()),
            icon_query,
            tab: FormTab::General,
            error: None,
            unsaved: Unsaved::default(),
        }));
        if let Some(Overlay::HostForm(form)) = &mut self.overlay {
            form.opened(cx);
        }
        self.list_agent_keys(cx);
        cx.notify();
    }

    /// Ask the SSH Agent for its keys, for the host form's "One key".
    fn list_agent_keys(&mut self, cx: &mut Context<Self>) {
        let Some(agent) = Agent::from_setting(&self.settings.ssh_agent) else {
            if let Some(form) = self.host_form() {
                form.agent_keys = Some(Ok(vec![]));
            }
            return;
        };
        let listing = crate::session::ssh::runtime().spawn(async move { agent.keys().await });
        cx.spawn(async move |this, cx| {
            let keys = match listing.await {
                Ok(Ok(keys)) => Ok(keys),
                Ok(Err(err)) => Err(format!("{err:#}")),
                Err(err) => Err(format!("Couldn't ask the SSH agent: {err}")),
            };
            let _ = this.update(cx, |this, cx| {
                if let Some(form) = this.host_form() {
                    form.agent_keys = Some(keys);
                    cx.notify();
                }
            });
        })
        .detach();
    }

    /// A one-line form field; Enter in it submits the open form.
    fn form_input(&mut self, placeholder: &str, value: &str, window: &mut Window, cx: &mut Context<Self>) -> Entity<InputState> {
        let (placeholder, value) = (placeholder.to_string(), value.to_string());
        let field = cx.new(|cx| InputState::new(window, cx).placeholder(placeholder).default_value(value));
        let sub = cx.subscribe_in(&field, window, |this, _, event: &InputEvent, window, cx| match event {
            InputEvent::PressEnter { .. } => this.submit_form(window, cx),
            // Typing answers the complaint: it goes until the next Save says otherwise.
            // The form shows what is typed elsewhere too (the jump host route, the rail's counts).
            InputEvent::Change => {
                let Some(form) = this.shared_form() else { return };
                form.error.take();
                cx.notify();
            }
            _ => {}
        });
        self._subscriptions.push(sub);
        field
    }

    pub(super) fn submit_form(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        // A key file or a Jump Host's password goes into the Vault: ask for the master
        // password first, then save.
        let (sign_in, key_path, jumps, desktops) = match &self.overlay {
            Some(Overlay::HostForm(form)) => (form.sign_in, form.key_path.clone(), &form.jumps, &form.desktops[..]),
            Some(Overlay::ProjectForm(form)) => (form.sign_in, form.key_path.clone(), &form.jumps, &[][..]),
            _ => return,
        };
        let new_password = jumps.iter().any(|row| !row.password.read(cx).value().is_empty());
        let desktop_password = desktops.iter().any(|row| row.on && !row.password.read(cx).value().is_empty());
        if self.vault.is_none() {
            let reason = if sign_in == SignIn::KeyFile && !key_path.read(cx).value().trim().is_empty() {
                Some("The key file is copied into your vault, encrypted.")
            } else if new_password {
                Some("Jump host passwords are saved in your vault, encrypted.")
            } else if desktop_password {
                Some("Remote desktop passwords are saved in your vault, encrypted.")
            } else {
                None
            };
            if let Some(reason) = reason {
                self.pending.push(super::Pending::Form);
                self.show_unlock(reason, window, cx);
                return;
            }
        }
        match &self.overlay {
            Some(Overlay::HostForm(_)) => self.submit_host_form(window, cx),
            Some(Overlay::ProjectForm(_)) => self.submit_project_form(window, cx),
            _ => {}
        }
    }

    fn host_form(&mut self) -> Option<&mut HostForm> {
        match self.overlay.as_mut() {
            Some(Overlay::HostForm(form)) => Some(form),
            _ => None,
        }
    }

    fn project_form(&mut self) -> Option<&mut ProjectForm> {
        match self.overlay.as_mut() {
            Some(Overlay::ProjectForm(form)) => Some(form),
            _ => None,
        }
    }

    /// What the open host or project form has in common with the other.
    fn shared_form(&mut self) -> Option<SharedForm<'_>> {
        match self.overlay.as_mut()? {
            Overlay::HostForm(f) => Some(SharedForm {
                tab: &mut f.tab,
                error: &mut f.error,
                sign_in: &mut f.sign_in,
                key_path: &f.key_path,
                jumps: &mut f.jumps,
                forwards: &mut f.forwards,
                commands: &mut f.commands,
            }),
            Overlay::ProjectForm(f) => Some(SharedForm {
                tab: &mut f.tab,
                error: &mut f.error,
                sign_in: &mut f.sign_in,
                key_path: &f.key_path,
                jumps: &mut f.jumps,
                forwards: &mut f.forwards,
                commands: &mut f.commands,
            }),
            _ => None,
        }
    }

    /// Pick a private key file for the open form's key field.
    fn browse_key_file(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let picked = cx.prompt_for_paths(gpui::PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some("Use key".into()),
        });
        cx.spawn_in(window, async move |this, cx| {
            let Ok(Ok(Some(paths))) = picked.await else { return };
            let Some(path) = paths.into_iter().next() else { return };
            let _ = this.update_in(cx, |this, window, cx| {
                let Some(form) = this.shared_form() else { return };
                *form.sign_in = SignIn::KeyFile;
                *form.error = None;
                let field = form.key_path.clone();
                field.update(cx, |f, cx| f.set_value(path.display().to_string(), window, cx));
                cx.notify();
            });
        })
        .detach();
    }

    /// Copy a private key file into the Vault as a new Credential.
    fn import_key(&mut self, label: String, path: &str) -> Result<CredentialId, String> {
        if path.is_empty() {
            return Err("Choose a private key file, or pick another sign-in.".into());
        }
        let Some(vault) = self.vault.as_mut() else {
            return Err("Unlock the vault to import a key.".into());
        };
        let path = expand_home(path, dirs::home_dir().as_deref());
        let bytes = std::fs::read(&path).map_err(|err| format!("Could not read {}: {err}", path.display()))?;
        let key = decode_key_text(&bytes)
            .map_err(|why| format!("Could not read {}: {why}. Save the key as plain text and try again.", path.display()))?;
        vault
            .add(Credential { label, secret: Secret::PrivateKey { key, passphrase: None } })
            .map_err(|err| format!("Could not store the key: {err}"))
    }

    /// Save a changed copy of the Library. It is taken only once it is on disk, so a failed
    /// write changes nothing.
    pub(super) fn commit_library(&mut self, library: Library) -> Result<(), String> {
        self.save_library(&library).map_err(|err| format!("Could not save: {err:#}"))?;
        self.library = library;
        Ok(())
    }

    /// Credentials added for a form that then could not be saved: take them out of the Vault
    /// again, or every retry would leave another copy nothing uses.
    fn forget_added(&mut self, ids: Vec<CredentialId>) {
        if let Some(vault) = self.vault.as_mut() {
            for id in ids {
                let _ = vault.remove(&id);
            }
        }
    }

    /// Put a password typed into a form in the Vault as a new Credential, and note it in
    /// `added`. On failure (`locked` says why when the Vault is locked), all of `added` leaves
    /// the Vault again: the form is not saved.
    fn add_password(&mut self, label: String, password: String, locked: &str, added: &mut Vec<CredentialId>) -> Result<CredentialId, String> {
        let result = match self.vault.as_mut() {
            Some(vault) => {
                vault.add(Credential { label, secret: Secret::Password { password } }).map_err(|err| format!("Could not store the password: {err}"))
            }
            None => Err(locked.to_string()),
        };
        match result {
            Ok(id) => {
                added.push(id.clone());
                Ok(id)
            }
            Err(err) => {
                self.forget_added(std::mem::take(added));
                Err(err)
            }
        }
    }

    /// Save a form: put the Jump Hosts' new passwords in the Vault, change a copy of the
    /// Library with them, and commit it. On failure the new passwords leave the Vault again;
    /// on success, passwords no Jump Host uses any more do.
    fn commit_form(
        &mut self,
        jumps: Vec<JumpEntry>,
        mut added: Vec<CredentialId>,
        before: Vec<JumpHost>,
        change: impl FnOnce(&mut Library, Vec<JumpHost>),
    ) -> Result<(), String> {
        let mut saved = Vec::with_capacity(jumps.len());
        for JumpEntry { mut jump, new_password } in jumps {
            if let Some(password) = new_password {
                let label = format!("{} (jump host)", jump.label());
                jump.credential = Some(self.add_password(label, password, "Unlock the vault to save the jump host's password.", &mut added)?);
            }
            saved.push(jump);
        }
        let mut library = self.library.clone();
        change(&mut library, saved);
        match self.commit_library(library) {
            Ok(()) => {
                self.forget_unused_credentials(before.into_iter().filter_map(|j| j.credential));
                Ok(())
            }
            Err(err) => {
                self.forget_added(added);
                Err(err)
            }
        }
    }

    fn submit_host_form(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let result = self.save_host_form(cx);
        self.refresh_session_highlights(cx);
        let result = self.show_failed_tab(result);
        self.finish_form(result, window, cx);
    }

    /// Turn to the page with the field a complaint is about, so it is in view beside it.
    fn show_failed_tab(&mut self, result: Result<(), FormError>) -> Result<(), String> {
        result.map_err(|(tab, message)| {
            if let (Some(tab), Some(form)) = (tab, self.shared_form()) {
                *form.tab = tab;
            }
            message
        })
    }

    /// Show `message` on the open host or project form.
    pub(super) fn show_form_error(&mut self, message: &str, cx: &mut Context<Self>) {
        if let Some(form) = self.shared_form() {
            *form.error = Some(message.to_string());
        }
        cx.notify();
    }

    fn finish_form(&mut self, result: Result<(), String>, window: &mut Window, cx: &mut Context<Self>) {
        match result {
            Ok(()) => self.close_overlay(window, cx),
            Err(message) => self.show_form_error(&message, cx),
        }
    }

    fn save_host_form(&mut self, cx: &mut Context<Self>) -> Result<(), FormError> {
        let Some(Overlay::HostForm(form)) = self.overlay.as_ref() else { return Ok(()) };
        let text = |field: &Entity<InputState>| field.read(cx).value().trim().to_string();
        let (name, address, port, user, key_path) =
            (text(&form.name), text(&form.address), text(&form.port), text(&form.user), text(&form.key_path));
        if name.is_empty() || address.is_empty() {
            return Err(on_tab(FormTab::General)("Give the host a name and an address.".into()));
        }
        let Some(port) = parse_port(&port) else { return Err(on_tab(FormTab::General)("The port is a number from 1 to 65535.".into())) };
        let user = match form.user_source {
            Source::Project => Inherited::Project,
            Source::Off => Inherited::Off,
            Source::Own if user.is_empty() => return Err(on_tab(FormTab::SignIn)("Type a user name, or take the project's.".into())),
            Source::Own => Inherited::Own(user),
        };
        let agent = match (form.agent, &form.agent_key) {
            (AgentChoice::Any, _) => AgentUse::Any,
            (AgentChoice::Off, _) => AgentUse::Off,
            (AgentChoice::One, Some(fingerprint)) => AgentUse::Key(fingerprint.clone()),
            (AgentChoice::One, None) => {
                return Err(on_tab(FormTab::SignIn)("Pick one of the SSH agent's keys, or let the host offer any of them.".into()));
            }
        };
        let forward_agent = match form.forward_agent {
            Source::Project => None,
            Source::Own => Some(true),
            Source::Off => Some(false),
        };
        let jumps = read_jump_rows(&form.jumps, cx).map_err(on_tab(FormTab::JumpHosts))?;
        let use_project_jumps = form.use_project_jumps;
        let use_project_forwards = form.use_project_forwards;
        let project_forwards = match self.library.project(&form.project_id) {
            Some(project) if use_project_forwards => project.port_forwards.clone(),
            _ => vec![],
        };
        let port_forwards = read_forward_rows(&form.forwards, &project_forwards, cx).map_err(on_tab(FormTab::Forwards))?;
        let startup_commands = read_command_rows(&form.commands, cx).map_err(on_tab(FormTab::Terminal))?;
        let (project_id, editing, sign_in, run_project_commands, icon, highlight, clipboard) = (
            form.project_id.clone(),
            form.editing.clone(),
            form.sign_in,
            form.run_project_commands,
            form.icon.clone(),
            form.highlight,
            form.clipboard,
        );
        let desktop_entries =
            form.desktops.iter().map(|row| read_desktop_row(row, cx)).collect::<Result<Vec<_>, _>>().map_err(on_tab(FormTab::RemoteDesktop))?;
        let opens = form.opens;
        let desktop_setup = form.desktop_setup;
        let desktop_enabled = form.desktops.each_ref().map(|row| row.on);

        let mut host = editing.as_deref().and_then(|id| self.library.host(id)).cloned().unwrap_or_else(|| Host::new("", ""));
        let mut imported = None;
        // A sign-in key or password replaced here leaves the Vault once nothing uses it.
        let old_credential = host.credential.own().cloned();
        host.credential = match sign_in {
            SignIn::Project => Inherited::Project,
            SignIn::Saved => host.credential.clone(),
            SignIn::Ask => Inherited::Off,
            SignIn::KeyFile => {
                let id = self.import_key(format!("{name} key"), &key_path).map_err(on_tab(FormTab::SignIn))?;
                imported = Some(id.clone());
                Inherited::Own(id)
            }
        };
        host.name = name;
        host.address = address;
        host.port = port;
        host.user = user;
        host.agent = agent;
        host.forward_agent = forward_agent;
        host.use_project_jump_hosts = use_project_jumps;
        host.use_project_forwards = use_project_forwards;
        host.port_forwards = port_forwards;
        host.startup_commands = startup_commands;
        host.run_project_commands = run_project_commands;
        host.highlight = highlight;
        host.clipboard = clipboard;
        host.icon = Some(icon);
        let mut imported: Vec<CredentialId> = imported.into_iter().collect();
        // Desktop passwords that are replaced or forgotten leave the Vault once nothing uses them.
        let old_desktop_credentials: Vec<CredentialId> =
            [&host.rdp, &host.vnc].into_iter().flatten().filter_map(|d| d.credential.clone()).collect();
        for (protocol, entry) in [Protocol::Rdp, Protocol::Vnc].into_iter().zip(desktop_entries) {
            // A protocol's setup dialog must not configure or disable the other desktop.
            if desktop_setup.is_some_and(|selected| selected != protocol) {
                continue;
            }
            match protocol {
                Protocol::Rdp => host.rdp_enabled = desktop_enabled[0],
                _ => host.vnc_enabled = desktop_enabled[1],
            }
            let desktop = match entry {
                // Switched off: hidden from the menus, as the menu's own opt-out does. Its port,
                // account and saved password stay for switching it back on.
                None => continue,
                Some((mut desktop, new_password)) => {
                    if let Some(password) = new_password {
                        let label = format!("{} ({})", host.name, protocol.label());
                        let locked = "Unlock the vault to save the remote desktop's password.";
                        let id = self.add_password(label, password, locked, &mut imported).map_err(on_tab(FormTab::RemoteDesktop))?;
                        desktop.credential = Some(id);
                    }
                    Some(desktop)
                }
            };
            match protocol {
                Protocol::Rdp => host.rdp = desktop,
                _ => host.vnc = desktop,
            }
        }
        // Take the form's choice, then check it against the desktops just saved: `opens_with`
        // falls back to SSH when that Remote Desktop is turned off or was removed.
        host.opens = opens;
        host.opens = host.opens_with();
        let before = std::mem::take(&mut host.jump_hosts);

        let result = self.commit_form(jumps, imported, before, move |library, jumps| {
            host.jump_hosts = jumps;
            match library.hosts.iter_mut().find(|h| h.id == host.id) {
                Some(existing) => *existing = host,
                None => {
                    match library.projects.iter_mut().find(|p| p.id == project_id) {
                        Some(project) => project.hosts.push(host.id.clone()),
                        // The first connection, added from the start screen: give it a project.
                        None => {
                            let mut project = Project::new(FIRST_PROJECT_NAME, FIRST_PROJECT_COLOR, "folder");
                            project.hosts.push(host.id.clone());
                            library.projects.push(project);
                        }
                    }
                    library.hosts.push(host);
                }
            }
        });
        if result.is_ok() {
            self.forget_unused_credentials(old_desktop_credentials.into_iter().chain(old_credential));
        }
        result.map_err(|message| (None, message))
    }

    pub(super) fn render_host_form(&self, form: &HostForm, wide: bool, p: AppPalette, cx: &mut Context<Self>) -> AnyElement {
        let project = self.library.project(&form.project_id);
        // No project yet (the first connection, from the start screen): the host goes into a
        // new one, created when it is saved.
        let project_name = project.map(|pr| pr.name.clone()).unwrap_or_else(|| FIRST_PROJECT_NAME.into());
        let color = project_color(project.map(|pr| pr.color.as_str()).unwrap_or(FIRST_PROJECT_COLOR));
        let host = form.editing.as_deref().and_then(|id| self.library.host(id));
        if let Some((index, row)) = form.desktop_setup.and_then(|protocol| form.desktops.iter().enumerate().find(|(_, row)| row.protocol == protocol)) {
            let host_name = host.map(|h| h.name.as_str()).unwrap_or("");
            return single_dialog(
                "host-setup",
                "Remote desktop",
                form_header(
                    badge("monitor", color, 34.).into_any_element(),
                    format!("Set up {}", row.protocol.label()),
                    format!("{host_name} · {project_name}"),
                    p,
                ),
                self.desktop_card(index, row, p, cx),
                form_footer("hf", "Save", form.error.as_deref(), p, cx),
            );
        }
        let title = match host {
            Some(h) => format!("Edit {}", h.name),
            None => "Add host".to_string(),
        };
        let pane = match form.tab {
            FormTab::General => self.host_general_pane(form, color, p, cx),
            FormTab::SignIn => self.host_sign_in_pane(form, host, project, p, cx),
            FormTab::JumpHosts => self.host_jump_pane(form, project, color, p, cx),
            FormTab::Forwards => self.host_forward_pane(form, project, p, cx),
            FormTab::Terminal => self.host_terminal_pane(form, project, p, cx),
            FormTab::RemoteDesktop => self.host_desktop_pane(form, p, cx),
        };
        // The rail says what each page holds, so it is found without opening every page.
        let project_jumps = if form.use_project_jumps { project.map_or(0, |pr| pr.jump_hosts.len()) } else { 0 };
        let project_commands = if form.run_project_commands { project.map_or(0, |pr| pr.startup_commands.len()) } else { 0 };
        let project_forwards = if form.use_project_forwards { project.map_or(0, |pr| pr.port_forwards.len()) } else { 0 };
        let desktops: Vec<&str> = form.desktops.iter().filter(|row| row.on).map(|row| row.protocol.label()).collect();
        let tabs = FormTab::HOST.map(|tab| {
            let note = match tab {
                FormTab::JumpHosts => count_note(project_jumps + form.jumps.len()),
                FormTab::Forwards => count_note(project_forwards + form.forwards.len()),
                FormTab::Terminal => count_note(project_commands + form.commands.len()),
                FormTab::RemoteDesktop if desktops.len() == 1 => Some(desktops[0].to_string()),
                FormTab::RemoteDesktop => count_note(desktops.len()),
                _ => None,
            };
            (tab, note)
        });
        let frame = FormFrame {
            id: "hf",
            icon: &form.icon,
            color,
            title,
            subtitle: format!("Host in {project_name}"),
            tabs: &tabs,
            current: form.tab,
            project: false,
        };
        let ok_label = if form.editing.is_some() { "Save" } else { "Add host" };
        let footer = form_footer("hf", ok_label, form.error.as_deref(), p, cx);
        form_dialog(frame, wide, pane, footer, p, cx, |this, tab| {
            if let Some(form) = this.host_form() {
                form.tab = tab;
            }
        })
    }

    fn host_general_pane(&self, form: &HostForm, color: Hsla, p: AppPalette, cx: &mut Context<Self>) -> AnyElement {
        let icons = icon_picker(&form.icon_query, &form.icon, color, p, cx, |this, name| {
            if let Some(form) = this.host_form() {
                form.icon = name;
            }
        });
        div()
            .flex()
            .flex_col()
            .gap(px(18.))
            .child(form_field("Name", Some("As the sidebar shows it."), Input::new(&form.name).into_any_element(), p))
            .child(
                div()
                    .flex()
                    .gap(px(10.))
                    .child(div().flex_1().min_w_0().child(form_field("Address", None, Input::new(&form.address).into_any_element(), p)))
                    .child(div().w(px(92.)).flex_shrink_0().child(form_field("Port", None, Input::new(&form.port).into_any_element(), p))),
            )
            .child(div().pt(px(6.)).flex().flex_col().gap(px(8.)).child(field_label("Icon", p)).child(icons))
            .into_any_element()
    }

    fn host_sign_in_pane(&self, form: &HostForm, host: Option<&Host>, project: Option<&Project>, p: AppPalette, cx: &mut Context<Self>) -> AnyElement {
        group(
            p,
            vec![
                self.host_user_field(form, project, p, cx),
                self.host_credential_field(form, host, project, p, cx),
                self.host_agent_field(form, p, cx),
                self.host_forward_agent_field(form, project, p, cx),
            ],
        )
    }

    fn host_agent_field(&self, form: &HostForm, p: AppPalette, cx: &mut Context<Self>) -> AnyElement {
        const OPTIONS: [(&str, AgentChoice); 3] = [("Any key", AgentChoice::Any), ("One key", AgentChoice::One), ("Off", AgentChoice::Off)];
        let labels = OPTIONS.map(|(label, choice)| (label, choice == form.agent));
        let choice = segmented("hf-agent", &labels, p, cx, |this, i, _, cx| {
            if let Some(form) = this.host_form() {
                form.agent = OPTIONS[i].1;
                // The only key there is needs no picking.
                if form.agent == AgentChoice::One
                    && form.agent_key.is_none()
                    && let Some(Ok([key])) = form.agent_keys.as_ref().map(|keys| keys.as_ref().map(Vec::as_slice))
                {
                    form.agent_key = Some(key.fingerprint.clone());
                }
                form.error = None;
            }
            cx.notify();
        });
        let no_agent = Agent::from_setting(&self.settings.ssh_agent).is_none();
        let hint = match form.agent {
            AgentChoice::Off => "The SSH agent isn't used for this host.",
            _ if no_agent => "There is no SSH agent to use: Settings → Security says where Komando looks for one.",
            AgentChoice::Any => {
                "Signs in with the keys your SSH agent holds, such as 1Password's or a YubiKey's. A key or password saved for the host goes first."
            }
            AgentChoice::One => {
                "Only this key is offered, so a server that counts each key as a failed try doesn't turn you away first."
            }
        };
        stacked("SSH agent", hint, p)
            .child(div().flex().child(choice))
            .when(form.agent == AgentChoice::One && !no_agent, |el| el.child(self.agent_key_list(form, p, cx)))
            .into_any_element()
    }

    /// The agent's keys to pick the Host's one from, and the one it has if the agent doesn't
    /// hold it now.
    fn agent_key_list(&self, form: &HostForm, p: AppPalette, cx: &mut Context<Self>) -> AnyElement {
        let keys = match &form.agent_keys {
            None => return hint_text("Asking the SSH agent for its keys…".into(), p),
            Some(Err(err)) => return hint_text(err.clone(), p),
            Some(Ok(keys)) => keys,
        };
        let mut rows: Vec<(agent::Key, bool)> = keys.iter().map(|key| (key.clone(), true)).collect();
        if let Some(chosen) = &form.agent_key
            && !keys.iter().any(|key| &key.fingerprint == chosen)
        {
            let missing = agent::Key { fingerprint: chosen.clone(), algorithm: String::new(), comment: String::new(), certificate: false };
            rows.insert(0, (missing, false));
        }
        if rows.is_empty() {
            return hint_text("The SSH agent holds no keys. Add one to it, then open this form again.".into(), p);
        }
        let items = rows.into_iter().enumerate().map(|(i, (key, held))| {
            let selected = form.agent_key.as_ref() == Some(&key.fingerprint);
            let label = if held { key.label().to_string() } else { "Not in the agent now".to_string() };
            let mut detail = key.algorithm.clone();
            if key.certificate {
                detail.push_str(" certificate");
            }
            // Without a comment the label is the fingerprint already.
            if !held || !key.comment.trim().is_empty() {
                if !detail.is_empty() {
                    detail.push_str(" · ");
                }
                detail.push_str(&key.fingerprint);
            }
            let fingerprint = key.fingerprint.clone();
            div()
                .id(("hf-agent-key", i))
                .role(gpui::Role::RadioButton)
                .aria_label(SharedString::from(label.clone()))
                .aria_toggled(if selected { gpui::Toggled::True } else { gpui::Toggled::False })
                .tab_index(0)
                .focus_visible(move |s| crate::ui::widgets::focus_outline(s, p.focus))
                .flex()
                .items_center()
                .gap(px(10.))
                .px(px(10.))
                .py(px(7.))
                .rounded(p.corner(8.))
                .border_1()
                .border_color(if selected { p.brand } else { p.border })
                .cursor_pointer()
                .when(selected, |el| el.bg(p.brand_soft))
                .when(!selected, |el| el.hover(|s| s.bg(p.hover)))
                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                    if let Some(form) = this.host_form() {
                        form.agent_key = Some(fingerprint.clone());
                        form.error = None;
                    }
                    cx.notify();
                }))
                .child(icon_el("key-round", 14., if held { p.text_muted } else { p.text_faint }))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .flex()
                        .flex_col()
                        .gap(px(2.))
                        .child(div().truncate().text_size(px(13.)).text_color(if held { p.text } else { p.text_muted }).child(label))
                        .when(!detail.is_empty(), |el| {
                            el.child(div().truncate().text_size(px(11.5)).font_family(p.mono_family).text_color(p.text_muted).child(detail))
                        }),
                )
                .when(selected, |el| el.child(icon_el("check", 14., p.brand)))
        });
        div().flex().flex_col().gap(px(6.)).children(items).into_any_element()
    }

    fn host_forward_agent_field(&self, form: &HostForm, project: Option<&Project>, p: AppPalette, cx: &mut Context<Self>) -> AnyElement {
        let options = [("From project", Source::Project), ("On", Source::Own), ("Off", Source::Off)];
        let choice = host_choice("hf-forward-agent", &options, form.forward_agent, p, cx, |form, source| form.forward_agent = source);
        let hint = match (form.forward_agent, project.is_some_and(|pr| pr.forward_agent)) {
            (Source::Project, false) => "The project doesn't forward the SSH agent.".to_string(),
            (Source::Project, true) => format!("The project forwards the SSH agent. {FORWARD_AGENT_WARNING}"),
            (Source::Own, _) => FORWARD_AGENT_WARNING.to_string(),
            (Source::Off, _) => "Not forwarded, even if the project forwards it.".to_string(),
        };
        stacked("Agent forwarding", &hint, p).child(div().flex().child(choice)).into_any_element()
    }

    fn host_user_field(&self, form: &HostForm, project: Option<&Project>, p: AppPalette, cx: &mut Context<Self>) -> AnyElement {
        let options = [("From project", Source::Project), ("Custom", Source::Own), ("Ask", Source::Off)];
        let choice = host_choice("hf-user", &options, form.user_source, p, cx, |form, source| form.user_source = source);
        let hint = match (form.user_source, project.and_then(|pr| pr.default_user.as_deref())) {
            (Source::Project, Some(user)) => format!("Signs in as {user}, the project's default user."),
            (Source::Project, None) => "The project sets no user, so it is asked when you connect.".to_string(),
            (Source::Own, _) => "Signs in as the user typed here.".to_string(),
            (Source::Off, _) => "Asked each time you connect.".to_string(),
        };
        stacked("User", &hint, p)
            .child(div().flex().child(choice))
            .when(form.user_source == Source::Own, |el| el.child(Input::new(&form.user)))
            .into_any_element()
    }

    fn host_credential_field(
        &self,
        form: &HostForm,
        host: Option<&Host>,
        project: Option<&Project>,
        p: AppPalette,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let has_saved = host.is_some_and(|h| h.credential.own().is_some());
        let project_credential = project.and_then(|pr| pr.credential.as_deref());
        let mut options = vec![("From project", SignIn::Project)];
        if has_saved {
            options.push(("Saved", SignIn::Saved));
        }
        options.push(("Key file", SignIn::KeyFile));
        options.push(("Ask", SignIn::Ask));
        let choice = host_choice("hf-sign-in", &options, form.sign_in, p, cx, |form, sign_in| form.sign_in = sign_in);
        let hint = match form.sign_in {
            SignIn::Project if project_credential.is_some() => {
                "Uses the key or password saved for the project, shared by all its hosts."
            }
            SignIn::Project => "The project has no saved key or password, so a password is asked when you connect.",
            SignIn::Saved => "Keeps the key or password already saved in the vault for this host.",
            SignIn::KeyFile => "The key is copied into the vault, encrypted. The file itself isn't needed afterwards.",
            SignIn::Ask => "A password is asked each time, even if the project has one saved. Komando offers to save it for this host.",
        };
        stacked("Key or password", hint, p)
            .child(div().flex().child(choice))
            .when(form.sign_in == SignIn::KeyFile, |el| el.child(key_file_row(&form.key_path, "hf", p, cx)))
            .into_any_element()
    }

    fn host_jump_pane(&self, form: &HostForm, project: Option<&Project>, color: Hsla, p: AppPalette, cx: &mut Context<Self>) -> AnyElement {
        let project_jumps = project.map(|pr| pr.jump_hosts.clone()).unwrap_or_default();
        let through_project = form.use_project_jumps && !project_jumps.is_empty();
        let mut hops: Vec<Hop> = vec![];
        if through_project {
            hops.extend(project_jumps.iter().map(|jump| Hop { icon: "waypoints".into(), label: jump.label(), note: Some("project") }));
        }
        hops.extend(typed_hops(&form.jumps, cx));
        let name = form.name.read(cx).value().trim().to_string();
        let end = Hop { icon: form.icon.clone(), label: if name.is_empty() { "This host".into() } else { name }, note: None };
        let hint = if hops.is_empty() {
            "Connects directly from this PC."
        } else {
            "Komando signs in to each jump host in turn, left to right, then reaches the host from the last."
        };
        let project_hint = format!("Set in the project: {}.", project_jumps.iter().map(JumpHost::label).collect::<Vec<_>>().join(", "));
        div()
            .flex()
            .flex_col()
            .gap(px(20.))
            .child(route_preview(hops, end, color, hint, p))
            .when(!project_jumps.is_empty(), |el| {
                el.child(group(
                    p,
                    vec![setting_row(
                        p,
                        "Start with the project's jump hosts",
                        &project_hint,
                        switch("hf-project-jumps".into(), "Start with the project's jump hosts", form.use_project_jumps, p)
                            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                                if let Some(form) = this.host_form() {
                                    form.use_project_jumps = !form.use_project_jumps;
                                }
                                cx.notify();
                            }))
                            .into_any_element(),
                    )],
                ))
            })
            .child(settings_block(
                if project_jumps.is_empty() { "Jump hosts" } else { "Then its own" },
                p,
                self.jump_list("hf", &form.jumps, p, cx),
            ))
            .into_any_element()
    }

    fn host_terminal_pane(&self, form: &HostForm, project: Option<&Project>, p: AppPalette, cx: &mut Context<Self>) -> AnyElement {
        let project_commands = project.map(|pr| pr.startup_commands.clone()).unwrap_or_default();
        let inherited = (!project_commands.is_empty()).then(|| {
            let hint = format!("{}, typed before this host's own.", crate::ui::widgets::plural(project_commands.len(), "command"));
            group(
                p,
                vec![
                    div()
                        .flex()
                        .flex_col()
                        .gap(px(12.))
                        .child(setting_row(
                            p,
                            "Run the project's commands first",
                            &hint,
                            switch("hf-project-commands".into(), "Run the project's commands first", form.run_project_commands, p)
                                .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                                    if let Some(form) = this.host_form() {
                                        form.run_project_commands = !form.run_project_commands;
                                    }
                                    cx.notify();
                                }))
                                .into_any_element(),
                        ))
                        .when(form.run_project_commands, |el| {
                            el.child(commands_preview(&project_commands.iter().map(StartupCommand::label).collect::<Vec<_>>(), p))
                        })
                        .into_any_element(),
                ],
            )
        });
        let commands = div()
            .flex()
            .flex_col()
            .gap(px(12.))
            .children(inherited)
            .child(self.command_list("hf-cmd", &form.commands, p, cx))
            .child(hint_text(COMMANDS_HINT.into(), p));
        let project_highlights = project.is_none_or(|pr| pr.highlight);
        let highlight = highlight_row(
            "hf-highlight",
            form.highlight,
            if project_highlights { HIGHLIGHT_HINT } else { "Off for every host in this project. Turn it on in the project first." },
            p,
            cx,
            |this| {
                if let Some(form) = this.host_form() {
                    form.highlight = !form.highlight;
                }
            },
        );
        const CLIPBOARD: [(&str, Option<bool>); 3] = [("As in Settings", None), ("Allow", Some(true)), ("Never", Some(false))];
        let labels = CLIPBOARD.map(|(label, choice)| (label, choice == form.clipboard));
        let clipboard = setting_row(
            p,
            "Programs may copy to the clipboard",
            "When a program on the host, such as tmux or vim, copies text to your clipboard (OSC 52). Settings → Security has the default.",
            segmented("hf-clipboard", &labels, p, cx, |this, i, _, cx| {
                if let Some(form) = this.host_form() {
                    form.clipboard = CLIPBOARD[i].1;
                }
                cx.notify();
            }),
        );
        div()
            .flex()
            .flex_col()
            .gap(px(28.))
            .child(settings_block("Startup commands", p, commands.into_any_element()))
            .child(settings_block("Output", p, group(p, vec![highlight, clipboard])))
            .into_any_element()
    }

    // ---- jump host list (both forms) ----------------------------------------------------

    /// A form row for a Jump Host, empty or filled from a saved one.
    fn jump_row(&mut self, jump: Option<&JumpHost>, window: &mut Window, cx: &mut Context<Self>) -> JumpRow {
        let port = jump.filter(|j| j.port != 22).map(|j| j.port.to_string()).unwrap_or_default();
        let user = self.form_input("user", jump.and_then(|j| j.user.as_deref()).unwrap_or(""), window, cx);
        let address = self.form_input("host name or IP", jump.map_or("", |j| j.address.as_str()), window, cx);
        let port = self.form_input("22", &port, window, cx);
        let password = cx.new(|cx| InputState::new(window, cx).masked(true).placeholder("Password"));
        let sub = cx.subscribe_in(&password, window, |this, _, event: &InputEvent, window, cx| {
            if let InputEvent::PressEnter { .. } = event {
                this.submit_form(window, cx);
            }
        });
        self._subscriptions.push(sub);
        self.next_jump_row += 1;
        JumpRow {
            key: self.next_jump_row,
            user,
            address,
            port,
            password,
            password_open: false,
            saved: jump.and_then(|j| j.credential.clone()),
            remove_saved: false,
        }
    }

    fn add_jump_row(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let row = self.jump_row(None, window, cx);
        let address = row.address.clone();
        if let Some(rows) = self.shared_form().map(|f| f.jumps) {
            rows.push(row);
            address.update(cx, |f, cx| f.focus(window, cx));
        }
        cx.notify();
    }

    /// Change the row with `key`, if the form still has it.
    fn update_jump_row(&mut self, key: usize, change: impl FnOnce(&mut JumpRow)) {
        if let Some(row) = self.shared_form().map(|f| f.jumps).and_then(|rows| rows.iter_mut().find(|r| r.key == key)) {
            change(row);
        }
    }

    /// The Jump Hosts of a form, one row each, with a button to add another. The password
    /// button shows only its icon (its name is in its tooltip), so the address keeps room.
    fn jump_list(&self, prefix: &'static str, rows: &[JumpRow], p: AppPalette, cx: &mut Context<Self>) -> AnyElement {
        let (user_width, port_width, actions_width) = (110., 64., 70.);
        let column_label = |text: &'static str| div().text_size(px(11.5)).text_color(p.text_faint).child(text);
        let header = div()
            .flex()
            .gap(px(8.))
            .pl(px(22.))
            .child(column_label("User").w(px(user_width)).flex_shrink_0())
            .child(column_label("Host name or IP").flex_1().min_w_0())
            .child(column_label("Port").w(px(port_width)).flex_shrink_0())
            .child(div().w(px(actions_width)).flex_shrink_0());
        let any = !rows.is_empty();
        let rows = rows.iter().enumerate().map(|(i, row)| {
            let key = row.key;
            let has_saved = row.saved.is_some() && !row.remove_saved;
            let typed = !row.password.read(cx).value().is_empty();
            let (password_icon, password_label) =
                if has_saved || typed { ("check", "Password set") } else { ("key-round", "Set password") };
            let fields = div()
                .flex()
                .items_center()
                .gap(px(8.))
                .child(div().w(px(14.)).flex_shrink_0().text_size(px(12.)).text_color(p.text_faint).child(format!("{}", i + 1)))
                .child(div().w(px(user_width)).flex_shrink_0().child(Input::new(&row.user)))
                .child(div().flex_1().min_w_0().child(Input::new(&row.address)))
                .child(div().w(px(port_width)).flex_shrink_0().child(Input::new(&row.port)))
                .child(
                    div()
                        .w(px(actions_width))
                        .flex_shrink_0()
                        .flex()
                        .items_center()
                        .gap(px(4.))
                        .child(
                            icon_text_button((prefix, 100 + key * 4), password_icon, "", ButtonKind::Secondary, p)
                                .flex_1()
                                .px(px(10.))
                                .gap(px(0.))
                                .aria_label(format!("{password_label} for jump host {}", i + 1))
                                .tooltip(move |window, cx| Tooltip::new(password_label).build(window, cx))
                                .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                                    let mut focus = None;
                                    this.update_jump_row(key, |row| {
                                        row.password_open = !row.password_open;
                                        focus = row.password_open.then(|| row.password.clone());
                                    });
                                    if let Some(field) = focus {
                                        field.update(cx, |f, cx| f.focus(window, cx));
                                    }
                                    cx.notify();
                                })),
                        )
                        .child(
                            icon_button((prefix, 101 + key * 4), "x", p)
                                .aria_label(format!("Remove jump host {}", i + 1))
                                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                                    if let Some(rows) = this.shared_form().map(|f| f.jumps) {
                                        rows.retain(|r| r.key != key);
                                    }
                                    cx.notify();
                                })),
                        ),
                );
            let password = row.password_open.then(|| {
                let hint = if has_saved {
                    "A password is saved in the vault. Type a new one to replace it when you save."
                } else {
                    "Saved in the vault, encrypted, when you save. Leave empty to be asked when you connect."
                };
                div()
                    .pl(px(22.))
                    .flex()
                    .flex_col()
                    .gap(px(6.))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(8.))
                            .child(div().flex_1().min_w_0().child(super::password_input(&row.password, p)))
                            .when(has_saved, |el| {
                                el.child(
                                    button((prefix, 102 + key * 4), "Forget saved password", ButtonKind::Ghost, p).on_click(cx.listener(
                                        move |this, _: &ClickEvent, window, cx| {
                                            let mut field = None;
                                            this.update_jump_row(key, |row| {
                                                row.remove_saved = true;
                                                field = Some(row.password.clone());
                                            });
                                            if let Some(field) = field {
                                                field.update(cx, |f, cx| f.set_value("", window, cx));
                                            }
                                            cx.notify();
                                        },
                                    )),
                                )
                            }),
                    )
                    .child(hint_text(hint.into(), p))
            });
            div().flex().flex_col().gap(px(6.)).child(fields).children(password)
        });
        div()
            .flex()
            .flex_col()
            .gap(px(8.))
            .when(any, |el| el.child(header).children(rows))
            .child(
                div().flex().child(
                    icon_text_button((prefix, 99usize), "plus", "Add jump host", ButtonKind::Ghost, p)
                        .px(px(10.))
                        .on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.add_jump_row(window, cx))),
                ),
            )
            .into_any_element()
    }

    // ---- port forwards (both forms) -----------------------------------------------------

    /// Open the host form on its Port Forwards page (from the session strip's forwards menu).
    pub(super) fn open_host_forwards(&mut self, project_id: String, host_id: String, window: &mut Window, cx: &mut Context<Self>) {
        self.open_edit_host(project_id, host_id, window, cx);
        if let Some(form) = self.host_form() {
            form.tab = FormTab::Forwards;
        }
    }

    fn host_forward_pane(&self, form: &HostForm, project: Option<&Project>, p: AppPalette, cx: &mut Context<Self>) -> AnyElement {
        let project_forwards = project.map(|pr| pr.port_forwards.clone()).unwrap_or_default();
        let inherited = (!project_forwards.is_empty()).then(|| {
            let hint = format!("Set in the project: {}.", project_forwards.iter().map(PortForward::label).collect::<Vec<_>>().join(", "));
            group(
                p,
                vec![setting_row(
                    p,
                    "Open the project's forwards too",
                    &hint,
                    switch("hf-project-forwards".into(), "Open the project's forwards too", form.use_project_forwards, p)
                        .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                            if let Some(form) = this.host_form() {
                                form.use_project_forwards = !form.use_project_forwards;
                                form.error = None;
                            }
                            cx.notify();
                        }))
                        .into_any_element(),
                )],
            )
        });
        let title = if project_forwards.is_empty() { "Port forwards" } else { "Its own" };
        div()
            .flex()
            .flex_col()
            .gap(px(20.))
            .children(inherited)
            .child(settings_block(title, p, self.forward_list("hf-fw", &form.forwards, p, cx)))
            .into_any_element()
    }

    fn project_forward_pane(&self, form: &ProjectForm, hosts: &[&Host], p: AppPalette, cx: &mut Context<Self>) -> AnyElement {
        let list = div()
            .flex()
            .flex_col()
            .gap(px(12.))
            .child(self.forward_list("pf-fw", &form.forwards, p, cx))
            .when(!form.forwards.is_empty(), |el| el.children(reach_note(hosts, |h| h.use_project_forwards, p)));
        settings_block("Port forwards", p, list.into_any_element())
    }

    /// A form row for a Port Forward, empty (a Local one) or filled from a saved one.
    fn forward_row(&mut self, forward: Option<&PortForward>, window: &mut Window, cx: &mut Context<Self>) -> ForwardRow {
        let number = |n: u16| if n == 0 { String::new() } else { n.to_string() };
        let port = self.form_input("5432", &forward.map_or(String::new(), |f| number(f.port)), window, cx);
        let to_host = self.form_input("localhost", forward.map_or("", |f| f.to_host.as_str()), window, cx);
        // A port that is the same as the one listened on stays empty: "same" says so.
        let to_port = forward.filter(|f| f.to_port != f.port).map_or(String::new(), |f| number(f.to_port));
        let to_port = self.form_input("same", &to_port, window, cx);
        self.next_jump_row += 1;
        ForwardRow {
            key: self.next_jump_row,
            kind: forward.map_or(ForwardKind::Local, |f| f.kind),
            port,
            to_host,
            to_port,
            open_to_network: forward.is_some_and(|f| f.open_to_network),
        }
    }

    fn add_forward_row(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let row = self.forward_row(None, window, cx);
        let port = row.port.clone();
        if let Some(rows) = self.shared_form().map(|f| f.forwards) {
            rows.push(row);
            port.update(cx, |f, cx| f.focus(window, cx));
        }
        cx.notify();
    }

    /// Change the row with `key`, if the form still has it.
    fn update_forward_row(&mut self, key: usize, change: impl FnOnce(&mut ForwardRow)) {
        let Some(form) = self.shared_form() else { return };
        if let Some(row) = form.forwards.iter_mut().find(|r| r.key == key) {
            change(row);
        }
        *form.error = None;
    }

    /// The Port Forwards of a form, one row each: which way, the port it listens on, where it
    /// goes, and whether other computers may connect. A SOCKS row has no address: the program
    /// using it says where to go.
    fn forward_list(&self, prefix: &'static str, rows: &[ForwardRow], p: AppPalette, cx: &mut Context<Self>) -> AnyElement {
        use gpui_component::button::Button;
        use gpui_component::menu::{DropdownMenu as _, PopupMenuItem};
        let (kind_width, port_width, actions_width) = (104., 72., 64.);
        let column_label = |text: &'static str| div().text_size(px(11.5)).text_color(p.text_faint).child(text);
        let header = div()
            .flex()
            .gap(px(8.))
            .pl(px(22.))
            .child(column_label("Type").w(px(kind_width)).flex_shrink_0())
            .child(column_label("Listens on").w(px(port_width)).flex_shrink_0())
            .child(div().w(px(14.)).flex_shrink_0())
            .child(column_label("Goes to").flex_1().min_w_0())
            .child(column_label("Port").w(px(port_width)).flex_shrink_0())
            .child(div().w(px(actions_width)).flex_shrink_0());
        let any = !rows.is_empty();
        let ws = cx.entity().downgrade();
        let rows = rows.iter().enumerate().map(|(i, row)| {
            let (key, kind, n) = (row.key, row.kind, i + 1);
            let ws = ws.clone();
            let kind_picker = Button::new((prefix, 200 + key * 4))
                .w_full()
                .accessibility_label(format!("Type of port forward {n}: {}", kind.label()))
                .bg(p.popover)
                .text_color(p.popover_text)
                .border_color(p.input)
                .label(kind.label())
                .dropdown_caret(true)
                .dropdown_menu(move |mut menu, _, _| {
                    for choice in ForwardKind::ALL {
                        let ws = ws.clone();
                        menu = menu.item(PopupMenuItem::new(choice.label()).checked(choice == kind).on_click(move |_, _, cx| {
                            let _ = ws.update(cx, |this, cx| {
                                this.update_forward_row(key, |row| row.kind = choice);
                                cx.notify();
                            });
                        }));
                    }
                    menu
                });
            let target = if kind.has_target() {
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .child(div().flex_1().min_w_0().child(Input::new(&row.to_host)))
                    .child(div().w(px(port_width)).flex_shrink_0().child(Input::new(&row.to_port)))
            } else {
                div()
                    .flex_1()
                    .min_w_0()
                    .h(px(32.))
                    .px(px(10.))
                    .flex()
                    .items_center()
                    .rounded(p.corner(7.))
                    .bg(p.hover)
                    .text_size(px(12.))
                    .text_color(p.text_muted)
                    .child(div().truncate().child("Wherever each program asks"))
            };
            let open = row.open_to_network;
            let (network_tip, network_label) = match (kind.listens_here(), open) {
                (true, false) => ("Only this computer can connect. Click to open it to your network.", "Open to the network"),
                (true, true) => ("Other computers on your network can connect. Click to allow only this computer.", "Open to the network"),
                (false, false) => ("Only the host itself can connect. Click to open it to the host's network.", "Open to the network"),
                (false, true) => (
                    "Other computers on the host's network can connect, if its sshd allows it (GatewayPorts). Click to allow only the host.",
                    "Open to the network",
                ),
            };
            // Like `icon_button`, with the icon in the active colour while it is on.
            let network = div()
                .id((prefix, 201 + key * 4))
                .role(gpui::Role::Button)
                .tab_index(0)
                .focus_visible(move |s| crate::ui::widgets::focus_outline(s, p.focus))
                .size(px(30.))
                .flex()
                .items_center()
                .justify_center()
                .rounded(p.corner(7.))
                .cursor_pointer()
                .when(open, |el| el.bg(p.brand_soft))
                .when(!open, |el| el.hover(move |s| s.bg(p.hover)))
                .child(icon_el("globe", 16., if open { p.active_text } else { p.text_muted }))
                .aria_label(format!("{network_label}: port forward {n}"))
                .aria_toggled(if open { gpui::Toggled::True } else { gpui::Toggled::False })
                .tooltip(move |window, cx| Tooltip::new(network_tip).build(window, cx))
                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                    this.update_forward_row(key, |row| row.open_to_network = !row.open_to_network);
                    cx.notify();
                }));
            div()
                .flex()
                .items_center()
                .gap(px(8.))
                .child(div().w(px(14.)).flex_shrink_0().text_size(px(12.)).text_color(p.text_faint).child(format!("{n}")))
                .child(div().w(px(kind_width)).flex_shrink_0().child(kind_picker))
                .child(div().w(px(port_width)).flex_shrink_0().child(Input::new(&row.port)))
                .child(div().w(px(14.)).flex_shrink_0().flex().justify_center().child(icon_el("arrow-right", 13., p.text_faint)))
                .child(target)
                .child(
                    div()
                        .w(px(actions_width))
                        .flex_shrink_0()
                        .flex()
                        .items_center()
                        .gap(px(4.))
                        .child(network)
                        .child(
                            icon_button((prefix, 202 + key * 4), "x", p)
                                .aria_label(format!("Remove port forward {n}"))
                                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                                    if let Some(rows) = this.shared_form().map(|f| f.forwards) {
                                        rows.retain(|r| r.key != key);
                                    }
                                    cx.notify();
                                })),
                        ),
                )
        });
        div()
            .flex()
            .flex_col()
            .gap(px(8.))
            .when(any, |el| el.child(header).children(rows))
            .child(
                div().flex().child(
                    icon_text_button((prefix, 99usize), "plus", "Add port forward", ButtonKind::Ghost, p)
                        .px(px(10.))
                        .on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.add_forward_row(window, cx))),
                ),
            )
            .child(div().pt(px(4.)).flex().flex_col().gap(px(6.)).children(FORWARDS_HINT.iter().map(|line| hint_text((*line).into(), p))))
            .into_any_element()
    }

    // ---- startup command list (both forms) -----------------------------------------------

    /// A form row for a Startup Command, empty or filled from a saved one.
    fn command_row(&mut self, command: Option<&StartupCommand>, window: &mut Window, cx: &mut Context<Self>) -> CommandRow {
        let wait_for = self.form_input("shell ready", command.and_then(|c| c.wait_for.as_deref()).unwrap_or(""), window, cx);
        let command = self.form_input("command", command.map_or("", |c| c.command.as_str()), window, cx);
        self.next_jump_row += 1;
        CommandRow { key: self.next_jump_row, wait_for, command }
    }

    fn add_command_row(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let row = self.command_row(None, window, cx);
        let command = row.command.clone();
        if let Some(rows) = self.shared_form().map(|f| f.commands) {
            rows.push(row);
            command.update(cx, |f, cx| f.focus(window, cx));
        }
        cx.notify();
    }

    /// Move the row with `key` one up (or down), if there is room.
    fn move_command_row(&mut self, key: usize, up: bool, cx: &mut Context<Self>) {
        if let Some(rows) = self.shared_form().map(|f| f.commands)
            && let Some(i) = rows.iter().position(|r| r.key == key)
        {
            let j = if up { i.checked_sub(1) } else { Some(i + 1).filter(|j| *j < rows.len()) };
            if let Some(j) = j {
                rows.swap(i, j);
            }
        }
        cx.notify();
    }

    /// The Startup Commands of a form, one row each (what it waits for, then the command),
    /// with a button to add another.
    fn command_list(&self, prefix: &'static str, rows: &[CommandRow], p: AppPalette, cx: &mut Context<Self>) -> AnyElement {
        let wait_width = 140.;
        let column_label = |text: &'static str| div().text_size(px(11.5)).text_color(p.text_faint).child(text);
        let actions_width = 84.;
        let header = div()
            .flex()
            .gap(px(8.))
            .pl(px(22.))
            .child(column_label("Wait for").w(px(wait_width)).flex_shrink_0())
            .child(column_label("Command").flex_1().min_w_0())
            .child(div().w(px(actions_width)).flex_shrink_0());
        let count = rows.len();
        let rows = rows.iter().enumerate().map(|(i, row)| {
            let key = row.key;
            div()
                .flex()
                .items_center()
                .gap(px(8.))
                .child(div().w(px(14.)).flex_shrink_0().text_size(px(12.)).text_color(p.text_faint).child(format!("{}", i + 1)))
                .child(div().w(px(wait_width)).flex_shrink_0().child(Input::new(&row.wait_for)))
                .child(div().flex_1().min_w_0().child(Input::new(&row.command)))
                .child(
                    div()
                        .w(px(actions_width))
                        .flex_shrink_0()
                        .flex()
                        .items_center()
                        .gap(px(2.))
                        .child(
                            icon_button((prefix, key * 3), "arrow-up", p)
                                .aria_label(format!("Move startup command {} up", i + 1))
                                .when(i == 0, |el| el.opacity(0.35))
                                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| this.move_command_row(key, true, cx))),
                        )
                        .child(
                            icon_button((prefix, key * 3 + 1), "arrow-down", p)
                                .aria_label(format!("Move startup command {} down", i + 1))
                                .when(i + 1 == count, |el| el.opacity(0.35))
                                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| this.move_command_row(key, false, cx))),
                        )
                        .child(
                            icon_button((prefix, key * 3 + 2), "x", p)
                                .aria_label(format!("Remove startup command {}", i + 1))
                                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                                    if let Some(rows) = this.shared_form().map(|f| f.commands) {
                                        rows.retain(|r| r.key != key);
                                    }
                                    cx.notify();
                                })),
                        ),
                )
        });
        div()
            .flex()
            .flex_col()
            .gap(px(8.))
            .when(count > 0, |el| el.child(header).children(rows))
            .child(
                div().flex().child(
                    icon_text_button((prefix, 0usize), "plus", "Add command", ButtonKind::Ghost, p)
                        .px(px(10.))
                        .on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.add_command_row(window, cx))),
                ),
            )
            .into_any_element()
    }

    // ---- remote desktops in the host form ----------------------------------------------

    fn desktop_row(&mut self, protocol: Protocol, desktop: Option<&Desktop>, window: &mut Window, cx: &mut Context<Self>) -> DesktopRow {
        let default = protocol.default_port().to_string();
        let port = desktop.map(|d| d.port).filter(|p| *p != protocol.default_port()).map(|p| p.to_string()).unwrap_or_default();
        let user = desktop.and_then(|d| d.user.clone()).unwrap_or_default();
        let domain = desktop.and_then(|d| d.domain.clone()).unwrap_or_default();
        DesktopRow {
            protocol,
            on: desktop.is_some(),
            port: self.form_input(&default, &port, window, cx),
            user: self.form_input("The host's user", &user, window, cx),
            domain: self.form_input("None (local account)", &domain, window, cx),
            password: {
                let field = cx.new(|cx| InputState::new(window, cx).masked(true));
                let sub = cx.subscribe_in(&field, window, |this, _, event: &InputEvent, window, cx| {
                    if let InputEvent::PressEnter { .. } = event {
                        this.submit_form(window, cx);
                    }
                });
                self._subscriptions.push(sub);
                field
            },
            saved: desktop.and_then(|d| d.credential.clone()),
            forget_saved: false,
            // A new desktop connects straight to the Host, like other viewers do: many SSH
            // servers refuse tunnels. The hint suggests the SSH session for VNC.
            through_ssh: desktop.is_some_and(|d| d.through_ssh),
            viewer: desktop.map(|d| d.viewer).unwrap_or_default(),
        }
    }

    fn update_desktop_row(&mut self, index: usize, change: impl FnOnce(&mut DesktopRow)) {
        if let Some(form) = self.host_form() {
            change(&mut form.desktops[index]);
            form.error = None;
            // A Host can't open with a desktop it no longer has.
            if form.desktops.iter().all(|row| row.protocol != form.opens || !row.on) {
                form.opens = Protocol::Ssh;
            }
        }
    }

    fn host_desktop_pane(&self, form: &HostForm, p: AppPalette, cx: &mut Context<Self>) -> AnyElement {
        let cards = form.desktops.iter().enumerate().map(|(i, row)| self.desktop_card(i, row, p, cx)).collect::<Vec<_>>();
        let offered: Vec<Protocol> = form.desktops.iter().filter(|row| row.on).map(|row| row.protocol).collect();
        let opens = if offered.is_empty() {
            hint_text("Switch on RDP or VNC to open the host's desktop from its right-click or ⋯ menu.".into(), p)
        } else {
            let choices: Vec<Protocol> = std::iter::once(Protocol::Ssh).chain(offered).collect();
            let labels: Vec<(&'static str, bool)> = choices
                .iter()
                .map(|protocol| (if protocol.is_ssh() { "Terminal" } else { protocol.label() }, *protocol == form.opens))
                .collect();
            let choice = segmented("hf-opens", &labels, p, cx, move |this, i, _, cx| {
                if let Some(form) = this.host_form() {
                    form.opens = choices[i];
                }
                cx.notify();
            });
            group(p, vec![setting_row(p, "A click on the host opens", "The others are in its right-click and ⋯ menu.", choice)])
        };
        div().flex().flex_col().gap(px(14.)).children(cards).child(div().pt(px(6.)).child(opens)).into_any_element()
    }

    /// A Remote Desktop's card: switched on or off in its first row, its settings below.
    fn desktop_card(&self, index: usize, row: &DesktopRow, p: AppPalette, cx: &mut Context<Self>) -> AnyElement {
        let protocol = row.protocol;
        let (name, about) = match protocol {
            Protocol::Rdp => ("RDP", "Windows Remote Desktop, xrdp"),
            _ => ("VNC", "Linux desktops, macOS screen sharing"),
        };
        let toggle = switch(format!("hf-desktop-{index}").into(), format!("Remote desktop over {name}"), row.on, p).on_click(cx.listener(
            move |this, _: &ClickEvent, _, cx| {
                this.update_desktop_row(index, |row| row.on = !row.on);
                cx.notify();
            },
        ));
        let mut rows = vec![setting_row(p, name, about, toggle.into_any_element())];
        if !row.on {
            return group(p, rows);
        }
        let password_hint = match (&row.saved, row.forget_saved) {
            (Some(_), false) => "Saved in your vault. Type a new one to replace it.",
            _ => "Asked when connecting, unless you type it here to save it in your vault.",
        };
        let ssh_hint = match (protocol, row.through_ssh) {
            (_, true) => "A tunnel on the host's SSH session to this port on the host itself. The SSH session connects first.",
            (Protocol::Vnc, false) => "Straight to the host's address. VNC sends the desktop unencrypted: prefer the SSH session.",
            (_, false) => "Straight to the host's address.",
        };
        let account = div()
            .flex()
            .gap(px(10.))
            .when(protocol == Protocol::Rdp, |el| {
                el.child(div().flex_1().min_w_0().child(form_field("User", None, Input::new(&row.user).into_any_element(), p)))
                    .child(div().flex_1().min_w_0().child(form_field("Domain", None, Input::new(&row.domain).into_any_element(), p)))
            })
            .child(div().w(px(92.)).flex_shrink_0().child(form_field("Port", None, Input::new(&row.port).into_any_element(), p)));
        let password = div()
            .flex()
            .flex_col()
            .gap(px(6.))
            .child(field_label("Password", p))
            .child(
                div()
                    .flex()
                    .gap(px(8.))
                    .child(div().flex_1().min_w_0().child(crate::ui::widgets::password_input(&row.password, p)))
                    .when(row.saved.is_some() && !row.forget_saved, |el| {
                        el.child(button(("hf-desktop-forget", index), "Forget", ButtonKind::Ghost, p).on_click(cx.listener(
                            move |this, _: &ClickEvent, _, cx| {
                                this.update_desktop_row(index, |row| row.forget_saved = true);
                                cx.notify();
                            },
                        )))
                    }),
            )
            .child(hint_text(password_hint.into(), p));
        let through_ssh = switch(format!("hf-desktop-ssh-{index}").into(), "Through the host's SSH session", row.through_ssh, p).on_click(
            cx.listener(move |this, _: &ClickEvent, _, cx| {
                this.update_desktop_row(index, |row| row.through_ssh = !row.through_ssh);
                cx.notify();
            }),
        );
        rows.push(div().flex().flex_col().gap(px(14.)).child(account).child(password).into_any_element());
        rows.push(setting_row(p, "Through the host's SSH session", ssh_hint, through_ssh.into_any_element()));
        group(p, rows)
    }

    // ---- project form -------------------------------------------------------------------

    pub(super) fn open_new_project(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.open_project_form(None, window, cx);
    }

    pub(super) fn open_edit_project(&mut self, project_id: String, window: &mut Window, cx: &mut Context<Self>) {
        self.open_project_form(Some(project_id), window, cx);
    }

    fn open_project_form(&mut self, editing: Option<String>, window: &mut Window, cx: &mut Context<Self>) {
        // One dialog at a time: a Session's open question comes first.
        if self.prompt_open(cx) {
            return;
        }
        let project = editing.as_deref().and_then(|id| self.library.project(id)).cloned();
        let pr = project.clone().unwrap_or_else(|| Project::new("", "blue", "folder"));
        let name = self.form_input("", &pr.name, window, cx);
        let default_user = self.form_input("", pr.default_user.as_deref().unwrap_or(""), window, cx);
        let key_path = self.form_input(key_file_hint(), "", window, cx);
        let commands = pr.startup_commands.iter().map(|c| self.command_row(Some(c), window, cx)).collect();
        let jumps = pr.jump_hosts.iter().map(|j| self.jump_row(Some(j), window, cx)).collect();
        let forwards = pr.port_forwards.iter().map(|f| self.forward_row(Some(f), window, cx)).collect();
        name.update(cx, |f, cx| f.focus(window, cx));
        let icon_query = self.new_icon_query(window, cx);
        self.overlay = Some(Overlay::ProjectForm(ProjectForm {
            editing,
            name,
            color: pr.color.clone(),
            icon: pr.icon.clone(),
            icon_query,
            default_user,
            sign_in: if pr.credential.is_some() { SignIn::Saved } else { SignIn::Ask },
            key_path,
            forward_agent: pr.forward_agent,
            jumps,
            forwards,
            commands,
            highlight: pr.highlight,
            tab: FormTab::General,
            error: None,
            unsaved: Unsaved::default(),
        }));
        if let Some(Overlay::ProjectForm(form)) = &mut self.overlay {
            form.opened(cx);
        }
        cx.notify();
    }

    fn submit_project_form(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let result = self.save_project_form(cx);
        self.refresh_session_highlights(cx);
        let result = self.show_failed_tab(result);
        self.finish_form(result, window, cx);
    }

    fn save_project_form(&mut self, cx: &mut Context<Self>) -> Result<(), FormError> {
        let Some(Overlay::ProjectForm(form)) = self.overlay.as_ref() else { return Ok(()) };
        let text = |field: &Entity<InputState>| field.read(cx).value().trim().to_string();
        let (name, default_user, key_path) = (text(&form.name), text(&form.default_user), text(&form.key_path));
        if name.is_empty() {
            return Err(on_tab(FormTab::General)("Give the project a name.".into()));
        }
        let jumps = read_jump_rows(&form.jumps, cx).map_err(on_tab(FormTab::JumpHosts))?;
        let port_forwards = read_forward_rows(&form.forwards, &[], cx).map_err(on_tab(FormTab::Forwards))?;
        // Its hosts that open the project's forwards open their own beside them: none may listen
        // on the same port.
        if let Some(project) = form.editing.as_deref().and_then(|id| self.library.project(id)) {
            let hosts: Vec<(&str, &[PortForward])> = self
                .library
                .hosts_of(project)
                .filter(|h| h.use_project_forwards)
                .map(|h| (h.name.as_str(), h.port_forwards.as_slice()))
                .collect();
            clash_with_hosts(&port_forwards, &hosts).map_err(on_tab(FormTab::Forwards))?;
        }
        let startup_commands = read_command_rows(&form.commands, cx).map_err(on_tab(FormTab::Terminal))?;
        let (editing, color, icon, sign_in, highlight, forward_agent) =
            (form.editing.clone(), form.color.clone(), form.icon.clone(), form.sign_in, form.highlight, form.forward_agent);

        let mut project = editing
            .as_deref()
            .and_then(|id| self.library.project(id))
            .cloned()
            .unwrap_or_else(|| Project::new("", "", ""));
        let mut imported = None;
        let old_credential = project.credential.clone();
        project.credential = match sign_in {
            SignIn::Saved => project.credential.clone(),
            SignIn::KeyFile => {
                let id = self.import_key(format!("{name} project key"), &key_path).map_err(on_tab(FormTab::SignIn))?;
                imported = Some(id.clone());
                Some(id)
            }
            SignIn::Ask | SignIn::Project => None,
        };
        project.name = name;
        project.color = color;
        project.icon = icon;
        project.default_user = (!default_user.is_empty()).then_some(default_user);
        project.startup_commands = startup_commands;
        project.port_forwards = port_forwards;
        project.forward_agent = forward_agent;
        project.highlight = highlight;
        let before = std::mem::take(&mut project.jump_hosts);

        let result = self.commit_form(jumps, imported.into_iter().collect(), before, move |library, jumps| {
            project.jump_hosts = jumps;
            match library.projects.iter_mut().find(|pr| pr.id == project.id) {
                Some(existing) => *existing = project,
                None => library.projects.push(project),
            }
        });
        // A project key or password replaced here leaves the Vault once nothing uses it.
        if result.is_ok() {
            self.forget_unused_credentials(old_credential);
        }
        result.map_err(|message| (None, message))
    }

    pub(super) fn render_project_form(&self, form: &ProjectForm, wide: bool, p: AppPalette, cx: &mut Context<Self>) -> AnyElement {
        let editing = form.editing.as_deref().and_then(|id| self.library.project(id));
        let (title, ok_label) = match editing {
            Some(pr) => (format!("Edit {}", pr.name), "Save"),
            None => ("New project".to_string(), "Create project"),
        };
        let color = project_color(&form.color);
        // Its Hosts, to say on each page which of them take the setting and which set their own.
        let hosts: Vec<&Host> = editing.map(|pr| self.library.hosts_of(pr).collect()).unwrap_or_default();
        let pane = match form.tab {
            FormTab::SignIn => self.project_sign_in_pane(form, editing.is_some_and(|pr| pr.credential.is_some()), &hosts, p, cx),
            FormTab::JumpHosts => self.project_jump_pane(form, color, &hosts, p, cx),
            FormTab::Forwards => self.project_forward_pane(form, &hosts, p, cx),
            FormTab::Terminal => self.project_terminal_pane(form, &hosts, p, cx),
            _ => self.project_general_pane(form, color, p, cx),
        };
        let tabs = FormTab::PROJECT.map(|tab| {
            let note = match tab {
                FormTab::JumpHosts => count_note(form.jumps.len()),
                FormTab::Forwards => count_note(form.forwards.len()),
                FormTab::Terminal => count_note(form.commands.len()),
                _ => None,
            };
            (tab, note)
        });
        let subtitle = match (editing, hosts.len()) {
            (None, _) => "A group of hosts that belong together".to_string(),
            (Some(_), 0) => "Project with no hosts yet".to_string(),
            (Some(_), n) => format!("Project with {}", crate::ui::widgets::plural(n, "host")),
        };
        let frame = FormFrame {
            id: "pf",
            icon: &form.icon,
            color,
            title,
            subtitle,
            tabs: &tabs,
            current: form.tab,
            project: true,
        };
        let footer = form_footer("pf", ok_label, form.error.as_deref(), p, cx);
        form_dialog(frame, wide, pane, footer, p, cx, |this, tab| {
            if let Some(form) = this.project_form() {
                form.tab = tab;
            }
        })
    }

    fn project_general_pane(&self, form: &ProjectForm, color: Hsla, p: AppPalette, cx: &mut Context<Self>) -> AnyElement {
        let current = form.color.clone();
        let swatches = div().flex().flex_wrap().gap(px(8.)).children(crate::theme::PROJECT_COLORS.iter().map(|(cname, _)| {
            let cname = cname.to_string();
            let selected = cname == current;
            let c = project_color(&cname);
            div()
                .id(SharedString::from(format!("swatch-{cname}")))
                .role(gpui::Role::RadioButton)
                .aria_label(format!("Project color: {cname}"))
                .aria_toggled(if selected { gpui::Toggled::True } else { gpui::Toggled::False })
                .tab_index(0)
                .focus_visible(move |s| crate::ui::widgets::focus_outline(s, p.focus))
                .size(px(26.))
                .rounded_full()
                .p(px(3.))
                .cursor_pointer()
                .border_2()
                .border_color(if selected { c } else { gpui::transparent_black() })
                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                    if let Some(form) = this.project_form() {
                        form.color = cname.clone();
                    }
                    cx.notify();
                }))
                .child(div().size_full().rounded_full().bg(c))
        }));
        let icons = icon_picker(&form.icon_query, &form.icon, color, p, cx, |this, name| {
            if let Some(form) = this.project_form() {
                form.icon = name;
            }
        });
        div()
            .flex()
            .flex_col()
            .gap(px(18.))
            .child(form_field("Name", Some("As the sidebar shows it."), Input::new(&form.name).into_any_element(), p))
            .child(div().flex().flex_col().gap(px(8.)).child(field_label("Color", p)).child(swatches))
            .child(div().pt(px(6.)).flex().flex_col().gap(px(8.)).child(field_label("Icon", p)).child(icons))
            .into_any_element()
    }

    /// Sign-in for the Project's Hosts: its default user and the Project Credential.
    fn project_sign_in_pane(&self, form: &ProjectForm, has_saved: bool, hosts: &[&Host], p: AppPalette, cx: &mut Context<Self>) -> AnyElement {
        // "Ask", as the host form calls a sign-in without a saved key or password.
        let mut options = vec![("Ask", SignIn::Ask)];
        if has_saved {
            options.push(("Saved", SignIn::Saved));
        }
        options.push(("Key file", SignIn::KeyFile));
        let labels: Vec<(&'static str, bool)> = options.iter().map(|(l, s)| (*l, *s == form.sign_in)).collect();
        let picks: Vec<SignIn> = options.iter().map(|(_, s)| *s).collect();
        let choice = segmented("pf-sign-in", &labels, p, cx, move |this, i, _, cx| {
            if let Some(form) = this.project_form() {
                form.sign_in = picks[i];
                form.error = None;
            }
            cx.notify();
        });
        let hint = match form.sign_in {
            SignIn::Saved => "Its hosts use the key or password saved for the project.",
            SignIn::KeyFile => "The key is copied into the vault, encrypted, and shared by the project's hosts.",
            _ => "Hosts ask for a password when you connect. Komando can save a typed one for each host.",
        };
        group(
            p,
            vec![
                stacked("User", "Empty: asked when you connect.", p)
                    .child(Input::new(&form.default_user))
                    .children(reach_note(hosts, |h| matches!(h.user, Inherited::Project), p))
                    .into_any_element(),
                stacked("Key or password", hint, p)
                    .child(div().flex().child(choice))
                    .when(form.sign_in == SignIn::KeyFile, |el| el.child(key_file_row(&form.key_path, "pf", p, cx)))
                    .children(reach_note(hosts, |h| matches!(h.credential, Inherited::Project), p))
                    .into_any_element(),
                div()
                    .flex()
                    .flex_col()
                    .gap(px(12.))
                    .child(setting_row(
                        p,
                        "Agent forwarding",
                        if form.forward_agent { FORWARD_AGENT_WARNING } else { "Off: the SSH agent stays on this PC." },
                        switch("pf-forward-agent".into(), "Agent forwarding", form.forward_agent, p)
                            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                                if let Some(form) = this.project_form() {
                                    form.forward_agent = !form.forward_agent;
                                }
                                cx.notify();
                            }))
                            .into_any_element(),
                    ))
                    .children(reach_note(hosts, |h| h.forward_agent.is_none(), p))
                    .into_any_element(),
            ],
        )
    }

    fn project_jump_pane(&self, form: &ProjectForm, color: Hsla, hosts: &[&Host], p: AppPalette, cx: &mut Context<Self>) -> AnyElement {
        let hops = typed_hops(&form.jumps, cx);
        let hint = if hops.is_empty() {
            "Its hosts connect directly from this PC, unless they add jump hosts of their own."
        } else {
            "Its hosts are reached through these, left to right, unless a host switches them off. A host can add its own after them."
        };
        let label = if hosts.is_empty() { "Its hosts".to_string() } else { crate::ui::widgets::plural(hosts.len(), "host") };
        let end = Hop { icon: form.icon.clone(), label, note: None };
        let list = div()
            .flex()
            .flex_col()
            .gap(px(12.))
            .child(self.jump_list("pf", &form.jumps, p, cx))
            .when(!form.jumps.is_empty(), |el| el.children(reach_note(hosts, |h| h.use_project_jump_hosts, p)));
        div()
            .flex()
            .flex_col()
            .gap(px(20.))
            .child(route_preview(hops, end, color, hint, p))
            .child(settings_block("Jump hosts", p, list.into_any_element()))
            .into_any_element()
    }

    fn project_terminal_pane(&self, form: &ProjectForm, hosts: &[&Host], p: AppPalette, cx: &mut Context<Self>) -> AnyElement {
        let commands = div()
            .flex()
            .flex_col()
            .gap(px(12.))
            .child(self.command_list("pf-cmd", &form.commands, p, cx))
            .when(!form.commands.is_empty(), |el| el.children(reach_note(hosts, |h| h.run_project_commands, p)))
            .child(hint_text(COMMANDS_HINT.into(), p));
        let highlight = highlight_row("pf-highlight", form.highlight, HIGHLIGHT_HINT, p, cx, |this| {
            if let Some(form) = this.project_form() {
                form.highlight = !form.highlight;
            }
        });
        div()
            .flex()
            .flex_col()
            .gap(px(28.))
            .child(settings_block("Startup commands", p, commands.into_any_element()))
            .child(settings_block("Output", p, group(p, vec![highlight])))
            .into_any_element()
    }
}

/// Below a form's Port Forwards: what each type does, and who can use them.
const FORWARDS_HINT: [&str; 4] = [
    "Local: a port on this computer reaches an address from the host's side, such as a database only the host can see.",
    "Remote: a port on the host reaches an address from this computer's side.",
    "SOCKS: a proxy on this computer. Programs set to use it connect from the host.",
    "Forwards open while a session of the host is connected. Only this computer (for Remote, the host) can use them, \
     unless you open one to the network.",
];

/// Below a form's Startup Commands: when each is typed.
const COMMANDS_HINT: &str = "Typed into the shell in turn, as if you typed them: each once the shell is quiet, or once \
    the text in \"Wait for\" appears after the previous command. Komando stops at password questions for you to answer.";

/// A choice picked with a segmented control, written back into the host form with `set`.
/// Picking one answers the form's complaint, as typing does.
fn host_choice<T: Copy + PartialEq + 'static>(
    id: &'static str,
    options: &[(&'static str, T)],
    current: T,
    p: AppPalette,
    cx: &mut Context<Workspace>,
    set: impl Fn(&mut HostForm, T) + Clone + 'static,
) -> AnyElement {
    let labels: Vec<(&'static str, bool)> = options.iter().map(|(l, s)| (*l, *s == current)).collect();
    let picks: Vec<T> = options.iter().map(|(_, s)| *s).collect();
    segmented(id, &labels, p, cx, move |this, i, _, cx| {
        if let Some(form) = this.host_form() {
            set(form, picks[i]);
            form.error = None;
        }
        cx.notify();
    })
}

fn key_file_row(field: &Entity<InputState>, id: &'static str, p: AppPalette, cx: &mut Context<Workspace>) -> AnyElement {
    div()
        .flex()
        .gap(px(8.))
        .child(div().flex_1().min_w_0().child(Input::new(field)))
        .child(
            button((id, 0usize), "Browse…", ButtonKind::Secondary, p)
                .h(px(32.))
                .flex_shrink_0()
                .on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.browse_key_file(window, cx))),
        )
        .into_any_element()
}

/// The Project's Startup Commands, shown read-only in the host form.
fn commands_preview(commands: &[String], p: AppPalette) -> AnyElement {
    div()
        .px(px(10.))
        .py(px(8.))
        .rounded(p.corner(8.))
        .bg(p.hover)
        .flex()
        .flex_col()
        .gap(px(2.))
        .font_family(p.mono_family)
        .text_size(px(12.))
        .text_color(p.text_muted)
        .children(commands.iter().map(|c| div().truncate().child(c.clone())))
        .into_any_element()
}

/// Under "Highlight output", while the Project lets its Hosts highlight.
const HIGHLIGHT_HINT: &str = "Marks what your highlight rules match. The rules are in Settings → Highlights.";

/// A count in the form's rail; nothing for none.
fn count_note(n: usize) -> Option<String> {
    (n > 0).then(|| n.to_string())
}

/// The "Highlight output" row of a host or project form.
fn highlight_row(
    id: &'static str,
    on: bool,
    hint: &'static str,
    p: AppPalette,
    cx: &mut Context<Workspace>,
    flip: impl Fn(&mut Workspace) + 'static,
) -> AnyElement {
    let toggle = switch(id.into(), "Highlight output", on, p).on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
        flip(this);
        cx.notify();
    }));
    setting_row(p, "Highlight output", hint, toggle.into_any_element())
}

/// Which of a Project's Hosts take one of its settings and which set their own, as a line
/// under the setting. Nothing for a Project without Hosts (a new one).
fn reach_note(hosts: &[&Host], uses: impl Fn(&Host) -> bool, p: AppPalette) -> Option<AnyElement> {
    let total = hosts.len();
    let own: Vec<&str> = hosts.iter().filter(|h| !uses(h)).map(|h| h.name.as_str()).collect();
    let text = match (total, own.len()) {
        (0, _) => return None,
        (1, 0) => "Its host uses this.".to_string(),
        (1, _) => format!("{} sets its own instead.", own[0]),
        (_, 0) => format!("All {total} of its hosts use this."),
        (_, n) if n == total => format!("None of its {total} hosts use this: each sets its own."),
        (_, n) => format!("{} of {total} hosts use this. Set their own: {}.", total - n, name_list(&own)),
    };
    Some(
        div()
            .flex()
            .items_start()
            .gap(px(8.))
            .child(div().pt(px(2.)).flex_shrink_0().child(icon_el("users", 13., p.text_faint)))
            .child(div().flex_1().min_w_0().text_size(px(12.)).line_height(px(17.)).text_color(p.text_muted).child(text))
            .into_any_element(),
    )
}

/// Up to three names, then how many more.
fn name_list(names: &[&str]) -> String {
    match names.len() {
        0..=3 => names.join(", "),
        n => format!("{} and {} more", names[..3].join(", "), n - 3),
    }
}

/// One stop on the way to a Host, as the jump host page draws it.
struct Hop {
    icon: String,
    label: String,
    /// Where it comes from, when not from this form ("project").
    note: Option<&'static str>,
}

/// The Jump Hosts typed into a form, as stops: the address typed, else its number.
fn typed_hops(rows: &[JumpRow], cx: &App) -> Vec<Hop> {
    rows.iter()
        .enumerate()
        .map(|(i, row)| {
            let address = row.address.read(cx).value().trim().to_string();
            let user = row.user.read(cx).value().trim().to_string();
            let label = match (user.is_empty(), address.is_empty()) {
                (_, true) => format!("Jump host {}", i + 1),
                (true, false) => address,
                (false, false) => format!("{user}@{address}"),
            };
            Hop { icon: "waypoints".into(), label, note: None }
        })
        .collect()
}

/// The way from this PC to the Host, one chip per stop, so the order of the jump hosts reads
/// at a glance. `end` is the Host itself, in its Project's color.
fn route_preview(hops: Vec<Hop>, end: Hop, end_color: Hsla, hint: &str, p: AppPalette) -> AnyElement {
    let chip = |hop: &Hop, color: Hsla| {
        div()
            .h(px(30.))
            .max_w(px(220.))
            .pl(px(5.))
            .pr(px(10.))
            .flex()
            .items_center()
            .gap(px(8.))
            .rounded(p.corner(9.))
            .bg(p.surface)
            .border_1()
            .border_color(p.border)
            .child(badge(&hop.icon, color, 20.))
            .child(div().min_w_0().truncate().text_size(px(12.5)).text_color(p.text).child(hop.label.clone()))
            .when_some(hop.note, |el, note| el.child(div().flex_shrink_0().text_size(px(11.)).text_color(p.text_faint).child(note)))
    };
    let start = Hop { icon: "laptop".into(), label: "This PC".into(), note: None };
    let mut stops = vec![chip(&start, p.text_muted)];
    stops.extend(hops.iter().map(|hop| chip(hop, p.text_muted)));
    stops.push(chip(&end, end_color));
    let count = stops.len();
    let path = div().flex().flex_wrap().items_center().gap(px(6.)).children(stops.into_iter().enumerate().flat_map(|(i, stop)| {
        let arrow = (i + 1 < count).then(|| icon_el("chevron-right", 14., p.text_faint));
        std::iter::once(stop.into_any_element()).chain(arrow)
    }));
    div()
        .p(px(14.))
        .flex()
        .flex_col()
        .gap(px(10.))
        .rounded(p.corner(12.))
        .bg(p.muted)
        .border_1()
        .border_color(p.border)
        .child(path)
        .child(hint_text(hint.into(), p))
        .into_any_element()
}

fn hint_text(text: String, p: AppPalette) -> AnyElement {
    div().text_size(px(12.)).line_height(px(17.)).text_color(p.text_muted).child(text).into_any_element()
}

fn form_error(message: String, p: AppPalette) -> AnyElement {
    div()
        .id("form-error")
        .role(gpui::Role::Alert)
        .aria_label(message.clone())
        .flex()
        .items_center()
        .gap(px(8.))
        .px(px(12.))
        .py(px(9.))
        .rounded(p.corner(8.))
        .bg(p.danger.opacity(0.1))
        .text_size(px(12.5))
        .text_color(p.danger)
        .child(icon_el("circle-alert", 14., p.danger))
        .child(div().flex_1().min_w_0().child(message))
        .into_any_element()
}

fn form_header(badge: AnyElement, title: String, subtitle: String, p: AppPalette) -> AnyElement {
    div()
        .flex_shrink_0()
        .pl(px(24.))
        // Room for the dialog's close button in the top-right corner.
        .pr(px(56.))
        .pt(px(22.))
        .pb(px(18.))
        .flex()
        .items_center()
        .gap(px(12.))
        .child(badge)
        .child(
            div()
                .flex()
                .flex_col()
                .gap(px(2.))
                .child(div().text_size(px(15.)).font_weight(FontWeight::SEMIBOLD).text_color(p.text).child(title))
                .child(div().text_size(px(12.5)).text_color(p.text_muted).child(subtitle)),
        )
        .into_any_element()
}

fn form_footer(id: &'static str, ok_label: &'static str, error: Option<&str>, p: AppPalette, cx: &mut Context<Workspace>) -> AnyElement {
    let actions = div()
        .flex()
        .items_center()
        .justify_end()
        .gap(px(8.))
        .child(
            button((id, 1usize), "Cancel", ButtonKind::Ghost, p)
                .on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.cancel_overlay(window, cx))),
        )
        .child(
            button((id, 2usize), ok_label, ButtonKind::Primary, p)
                .on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.submit_form(window, cx))),
        );
    div()
        .flex_shrink_0()
        .px(px(24.))
        .py(px(14.))
        .flex()
        .flex_col()
        .gap(px(10.))
        .border_t_1()
        .border_color(p.border)
        .children(error.map(|message| form_error(message.to_string(), p)))
        .child(actions)
        .into_any_element()
}

/// Width of the tabbed forms with their rail labelled; narrower windows get
/// `NARROW_FORM_WIDTH` and a rail of icons.
pub(super) const WIDE_FORM_WIDTH: f32 = 820.;
const NARROW_FORM_WIDTH: f32 = 560.;
/// The tabbed forms keep one height on every page, so switching pages doesn't make the
/// dialog jump; a shorter window shrinks it and the page scrolls.
const FORM_HEIGHT: f32 = 600.;
const RAIL_WIDTH: f32 = 220.;
const NARROW_RAIL_WIDTH: f32 = 60.;

/// What a tabbed form's rail shows: what is being edited, and its pages.
struct FormFrame<'a> {
    /// Prefix of its element ids.
    id: &'static str,
    icon: &'a str,
    color: Hsla,
    title: String,
    subtitle: String,
    /// Each page with a short note (a count) beside its name.
    tabs: &'a [(FormTab, Option<String>)],
    current: FormTab,
    /// A project's form rather than a host's.
    project: bool,
}

/// A host or project form: a rail on the left with what is edited and its pages, the open
/// page on the right with Save and Cancel below it.
fn form_dialog(
    frame: FormFrame,
    wide: bool,
    page: AnyElement,
    footer: AnyElement,
    p: AppPalette,
    cx: &mut Context<Workspace>,
    pick: impl Fn(&mut Workspace, FormTab) + Clone + 'static,
) -> AnyElement {
    // The rail is drawn like the sidebar and the Settings page's list.
    let np = p.navigation();
    let identity = if wide {
        div()
            .px(px(6.))
            .pt(px(6.))
            .pb(px(18.))
            .flex()
            .flex_col()
            .gap(px(12.))
            .child(badge(frame.icon, frame.color, 40.))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(2.))
                    .child(
                        div()
                            .text_size(px(15.))
                            .line_height(px(20.))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(np.text)
                            .child(frame.title.clone()),
                    )
                    .child(div().text_size(px(12.)).line_height(px(16.)).text_color(np.text_muted).child(frame.subtitle.clone())),
            )
    } else {
        div().pt(px(4.)).pb(px(14.)).flex().justify_center().child(badge(frame.icon, frame.color, 32.))
    };
    let tabs = frame.tabs.iter().map(|(tab, note)| {
        let (tab, note) = (*tab, note.clone());
        let active = tab == frame.current;
        let label = tab.label();
        let pick = pick.clone();
        div()
            .id(SharedString::from(format!("{}-tab-{label}", frame.id)))
            .role(gpui::Role::Tab)
            .aria_label(label)
            .aria_selected(active)
            .tab_index(0)
            .focus_visible(move |s| crate::ui::widgets::focus_outline(s, np.focus))
            .h(px(36.))
            .px(px(10.))
            .flex()
            .items_center()
            .gap(px(10.))
            .rounded(np.corner(8.))
            .cursor_pointer()
            .text_size(px(13.))
            .text_color(if active { np.active_text } else { np.text_muted })
            .when(active, |el| el.bg(np.active).font_weight(FontWeight::MEDIUM))
            .when(!active, |el| el.hover(|s| s.bg(np.hover).text_color(np.hover_text)))
            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                pick(this, tab);
                cx.notify();
            }))
            .child(icon_el(tab.icon(), 16., if active { np.active_text } else { np.text_faint }))
            .when(wide, |el| {
                el.child(div().flex_1().min_w_0().truncate().child(label)).children(note.map(|note| {
                    div().flex_shrink_0().text_size(px(11.5)).text_color(if active { np.active_text } else { np.text_faint }).child(note)
                }))
            })
            .when(!wide, |el| el.justify_center().tooltip(move |window, cx| Tooltip::new(label).build(window, cx)))
    });
    let keys = div()
        .px(px(6.))
        .pb(px(4.))
        .flex()
        .flex_col()
        .gap(px(8.))
        .text_size(px(11.5))
        .text_color(np.text_faint)
        .child(div().flex().items_center().gap(px(8.)).child(kbd("Enter", np)).child("Save"))
        .child(div().flex().items_center().gap(px(8.)).child(kbd("Esc", np)).child("Cancel"));
    let rail = div()
        .id(SharedString::from(format!("{}-pages", frame.id)))
        .role(gpui::Role::TabList)
        .aria_label(frame.title.clone())
        .w(px(if wide { RAIL_WIDTH } else { NARROW_RAIL_WIDTH }))
        .h_full()
        .flex_shrink_0()
        .flex()
        .flex_col()
        .gap(px(2.))
        .p(px(if wide { 12. } else { 8. }))
        .bg(np.background)
        // Inside the dialog's rounded border.
        .rounded_tl(p.corner(13.))
        .rounded_bl(p.corner(13.))
        .border_r_1()
        .border_color(p.border)
        .child(identity)
        .children(tabs)
        .child(div().flex_1())
        .when(wide, |el| el.child(keys));

    let current = frame.current;
    let header = div()
        .flex_shrink_0()
        .pl(px(24.))
        // Room for the dialog's close button in the top-right corner.
        .pr(px(56.))
        .pt(px(22.))
        .pb(px(18.))
        .flex()
        .flex_col()
        .gap(px(4.))
        // Without the rail's labels, say here what is edited.
        .when(!wide, |el| el.child(div().text_size(px(12.)).text_color(p.text_faint).child(frame.title.clone())))
        .child(div().text_size(px(17.)).font_weight(FontWeight::SEMIBOLD).text_color(p.text).child(current.label()))
        .child(div().text_size(px(12.5)).line_height(px(18.)).text_color(p.text_muted).child(current.about(frame.project)));
    let page = div()
        .flex_1()
        .min_w_0()
        .h_full()
        .flex()
        .flex_col()
        .child(header)
        .child(
            div()
                // One per page, so each page opens scrolled to its top.
                .id(SharedString::from(format!("{}-page-{}", frame.id, current.label())))
                .role(gpui::Role::TabPanel)
                .aria_label(current.label())
                .flex_1()
                .min_h_0()
                .overflow_y_scroll()
                .px(px(24.))
                .pt(px(2.))
                .pb(px(28.))
                .child(page),
        )
        .child(footer);
    div()
        .id(SharedString::from(format!("{}-dialog", frame.id)))
        .role(gpui::Role::Dialog)
        .aria_label(frame.title)
        .tab_group()
        .w(px(if wide { WIDE_FORM_WIDTH } else { NARROW_FORM_WIDTH }))
        .h(px(FORM_HEIGHT))
        .flex_shrink(1.)
        .min_h_0()
        .flex()
        .child(rail)
        .child(page)
        .into_any_element()
}

/// The Jump Hosts typed into a form, in order. Rows left completely empty are skipped.
fn read_jump_rows(rows: &[JumpRow], cx: &App) -> Result<Vec<JumpEntry>, String> {
    let text = |field: &Entity<InputState>| field.read(cx).value().trim().to_string();
    let mut jumps = vec![];
    for (i, row) in rows.iter().enumerate() {
        let (user, address, port) = (text(&row.user), text(&row.address), text(&row.port));
        let password = row.password.read(cx).value().to_string();
        let saved = row.saved.clone().filter(|_| !row.remove_saved);
        if user.is_empty() && address.is_empty() && port.is_empty() && password.is_empty() && saved.is_none() {
            continue;
        }
        let n = i + 1;
        if address.is_empty() {
            return Err(format!("Type the host name or IP of jump host {n}, or remove it."));
        }
        let port = parse_port(&port).ok_or_else(|| format!("The port of jump host {n} is a number from 1 to 65535."))?;
        let new_password = (!password.is_empty()).then_some(password);
        let user = (!user.is_empty()).then_some(user);
        jumps.push(JumpEntry { jump: JumpHost { address, port, user, credential: saved }, new_password });
    }
    Ok(jumps)
}

/// A Remote Desktop read from the host form, with a new password to put in the Vault. `None`
/// when it is switched off.
fn read_desktop_row(row: &DesktopRow, cx: &App) -> Result<Option<(Desktop, Option<String>)>, String> {
    if !row.on {
        return Ok(None);
    }
    let text = |field: &Entity<InputState>| field.read(cx).value().trim().to_string();
    let name = row.protocol.label();
    let port = match text(&row.port) {
        port if port.is_empty() => row.protocol.default_port(),
        port => port.parse::<u16>().ok().filter(|p| *p > 0).ok_or_else(|| format!("The {name} port is a number from 1 to 65535."))?,
    };
    let (user, domain) = match row.protocol {
        Protocol::Rdp => (Some(text(&row.user)).filter(|u| !u.is_empty()), Some(text(&row.domain)).filter(|d| !d.is_empty())),
        _ => (None, None),
    };
    let password = row.password.read(cx).value().to_string();
    let desktop = Desktop {
        port,
        user,
        domain,
        credential: row.saved.clone().filter(|_| !row.forget_saved),
        through_ssh: row.through_ssh,
        viewer: row.viewer,
    };
    Ok(Some((desktop, (!password.is_empty()).then_some(password))))
}

/// A port field: empty is 22.
fn parse_port(text: &str) -> Option<u16> {
    match text {
        "" => Some(22),
        port => port.parse::<u16>().ok().filter(|p| *p > 0),
    }
}

/// A one-page form: header and footer stay put, the middle scrolls.
fn single_dialog(id: &'static str, aria: &'static str, header: AnyElement, body: AnyElement, footer: AnyElement) -> AnyElement {
    div()
        .id(SharedString::from(format!("{id}-dialog")))
        .role(gpui::Role::Dialog)
        .aria_label(aria)
        .tab_group()
        .w(px(NARROW_FORM_WIDTH))
        .flex_1()
        .min_h_0()
        .flex()
        .flex_col()
        .child(header)
        .child(div().id(id).flex_1().min_h_0().overflow_y_scroll().px(px(24.)).pb(px(22.)).child(body))
        .child(footer)
        .into_any_element()
}

/// The example key file path shown in an empty key field.
fn key_file_hint() -> &'static str {
    if cfg!(windows) { r"C:\Users\you\.ssh\id_ed25519" } else { "~/.ssh/id_ed25519" }
}

/// A leading `~\` or `~/` means the home folder. Other paths are used as typed.
fn expand_home(path: &str, home: Option<&std::path::Path>) -> std::path::PathBuf {
    match (path.strip_prefix("~\\").or_else(|| path.strip_prefix("~/")), home) {
        (Some(rest), Some(home)) => home.join(rest),
        _ => std::path::PathBuf::from(path),
    }
}

/// Turn the bytes of a key file into text. Windows tools write UTF-16 with a byte order mark
/// (PowerShell's `>` redirection, old Notepad) and UTF-8 with one (PowerShell 5.1); the
/// marks are dropped so the key parser sees only the key.
fn decode_key_text(bytes: &[u8]) -> Result<String, &'static str> {
    const NOT_TEXT: &str = "it is not a text file";
    if let Some(rest) = bytes.strip_prefix(&[0xFF, 0xFE]) {
        return decode_utf16(rest, u16::from_le_bytes);
    }
    if let Some(rest) = bytes.strip_prefix(&[0xFE, 0xFF]) {
        return decode_utf16(rest, u16::from_be_bytes);
    }
    let bytes = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(bytes);
    String::from_utf8(bytes.to_vec()).map_err(|_| NOT_TEXT)
}

fn decode_utf16(bytes: &[u8], unit: fn([u8; 2]) -> u16) -> Result<String, &'static str> {
    if bytes.len() % 2 != 0 {
        return Err("it is not valid UTF-16 text");
    }
    let units: Vec<u16> = bytes.chunks_exact(2).map(|pair| unit([pair[0], pair[1]])).collect();
    String::from_utf16(&units).map_err(|_| "it is not valid UTF-16 text")
}

#[cfg(test)]
mod tests {
    use super::{ForwardFields, ForwardKind, PortForward, clash_with_hosts, decode_key_text, expand_home, name_list, parse_forwards};
    use std::path::{Path, PathBuf};

    const KEY: &str = "-----BEGIN OPENSSH PRIVATE KEY-----\nabc\n-----END OPENSSH PRIVATE KEY-----\n";

    fn utf16le(text: &str) -> Vec<u8> {
        std::iter::once(0xFF).chain(std::iter::once(0xFE)).chain(text.encode_utf16().flat_map(|u| u.to_le_bytes())).collect()
    }

    fn utf16be(text: &str) -> Vec<u8> {
        std::iter::once(0xFE).chain(std::iter::once(0xFF)).chain(text.encode_utf16().flat_map(|u| u.to_be_bytes())).collect()
    }

    #[test]
    fn decodes_plain_utf8() {
        assert_eq!(decode_key_text(KEY.as_bytes()).unwrap(), KEY);
    }

    #[test]
    fn strips_utf8_bom() {
        let mut bytes = vec![0xEF, 0xBB, 0xBF];
        bytes.extend_from_slice(KEY.as_bytes());
        assert_eq!(decode_key_text(&bytes).unwrap(), KEY);
    }

    #[test]
    fn decodes_utf16_le_with_bom() {
        assert_eq!(decode_key_text(&utf16le(KEY)).unwrap(), KEY);
    }

    #[test]
    fn decodes_utf16_be_with_bom() {
        assert_eq!(decode_key_text(&utf16be(KEY)).unwrap(), KEY);
    }

    #[test]
    fn rejects_invalid_utf8() {
        assert!(decode_key_text(&[0x2D, 0xFF, 0xFE, 0x41]).is_err());
    }

    #[test]
    fn rejects_odd_length_utf16() {
        assert!(decode_key_text(&[0xFF, 0xFE, 0x41]).is_err());
    }

    #[test]
    fn lists_up_to_three_names() {
        assert_eq!(name_list(&["web-01"]), "web-01");
        assert_eq!(name_list(&["a", "b", "c"]), "a, b, c");
        assert_eq!(name_list(&["a", "b", "c", "d", "e"]), "a, b, c and 2 more");
    }

    #[test]
    fn expands_home_prefixes_only() {
        let home = Path::new(r"C:\Users\me");
        assert_eq!(expand_home(r"~\.ssh\id_ed25519", Some(home)), home.join(r".ssh\id_ed25519"));
        assert_eq!(expand_home("~/.ssh/id", Some(home)), home.join(".ssh/id"));
        assert_eq!(expand_home(r"C:\keys\id", Some(home)), PathBuf::from(r"C:\keys\id"));
        assert_eq!(expand_home("~/id", None), PathBuf::from("~/id"));
    }

    fn row(kind: ForwardKind, port: &str, to_host: &str, to_port: &str) -> ForwardFields {
        ForwardFields { kind, port: port.into(), to_host: to_host.into(), to_port: to_port.into(), open_to_network: false }
    }

    #[test]
    fn port_forward_rows_fill_in_what_was_left_empty() {
        let rows = [
            row(ForwardKind::Local, "5432", "", ""),
            row(ForwardKind::Remote, "8080", "[::1]", "3000"),
            // A SOCKS row keeps an address typed before switching to it, and ignores it.
            row(ForwardKind::Socks, "1080", "old.lan", "22"),
            row(ForwardKind::Local, "", "", ""),
        ];
        assert_eq!(
            parse_forwards(&rows, &[]).unwrap(),
            [PortForward::local(5432, "localhost", 5432), PortForward::remote(8080, "::1", 3000), PortForward::socks(1080)]
        );
    }

    #[test]
    fn port_forward_rows_say_what_is_wrong() {
        let err = |rows: &[ForwardFields], inherited: &[PortForward]| parse_forwards(rows, inherited).unwrap_err();
        assert_eq!(err(&[row(ForwardKind::Local, "", "db", "5432")], &[]), "Type the port that port forward 1 listens on, from 1 to 65535.");
        assert_eq!(err(&[row(ForwardKind::Local, "5432", "db", "x")], &[]), "The port that port forward 1 goes to is a number from 1 to 65535.");
        assert_eq!(
            err(&[row(ForwardKind::Local, "8080", "a", ""), row(ForwardKind::Socks, "8080", "", "")], &[]),
            "Port forwards 1 and 2 both listen on port 8080 on this computer. Pick another port for one of them."
        );
        assert_eq!(
            err(&[row(ForwardKind::Remote, "5432", "", "")], &[PortForward::remote(5432, "localhost", 5432)]),
            "Port forward 1 listens on port 5432 on the host, as the project's \"Remote 5432 → localhost:5432\" does. Pick \
             another port, or leave out the project's forwards."
        );
        // The same port on this computer and on the host is no clash.
        assert!(parse_forwards(&[row(ForwardKind::Local, "8080", "", ""), row(ForwardKind::Remote, "8080", "", "")], &[]).is_ok());
    }

    #[test]
    fn a_projects_forwards_may_not_clash_with_its_hosts_own() {
        let db_own = [PortForward::local(5432, "localhost", 5432)];
        let web_own = [PortForward::remote(5432, "localhost", 5432)];
        let hosts: [(&str, &[PortForward]); 2] = [("web", &web_own), ("db", &db_own)];
        assert!(clash_with_hosts(&[PortForward::socks(1080)], &hosts).is_ok());
        assert_eq!(
            clash_with_hosts(&[PortForward::socks(1080), PortForward::socks(5432)], &hosts).unwrap_err(),
            "The project's port forward \"SOCKS on 5432\" listens on port 5432 on this computer, as db's own \"5432 → \
             localhost:5432\" does. Pick another port, or have db leave out the project's forwards."
        );
    }
}
