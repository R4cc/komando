//! The host and project forms.
//!
//! Both edit the settings a Project hands down to its Hosts in the same way: the Project sets
//! a default (user, Credential, Jump Hosts, Startup Commands), and in the host form each one
//! is a choice between taking the Project's, setting the Host's own, or switching it off.
//! Jump Hosts and Startup Commands add up instead: the Project's come first, then the Host's,
//! and the host form has a switch to leave the Project's out.

use gpui::prelude::FluentBuilder as _;
use gpui::{AnyElement, App, ClickEvent, Context, Entity, FontWeight, SharedString, Window, div, prelude::*, px};
use gpui_component::input::{Input, InputEvent, InputState, Textarea, TextareaState};

use super::{FIRST_PROJECT_COLOR, FIRST_PROJECT_NAME, Overlay, Workspace, form_field, form_section, icon_picker, segmented, switch};
use crate::model::{CredentialId, Host, Inherited, JumpHost, Library, Project};
use crate::theme::{AppPalette, project_color};
use crate::ui::widgets::{ButtonKind, badge, button, icon_button, icon_el, icon_text_button};
use crate::vault::{Credential, Secret};

/// Where a Host takes a setting from, as picked in the host form.
#[derive(Clone, Copy, PartialEq, Eq)]
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

/// How a Host or Project signs in, as picked in a form.
#[derive(Clone, Copy, PartialEq, Eq)]
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
    commands: Entity<TextareaState>,
    user_source: Source,
    sign_in: SignIn,
    /// Go through the Project's Jump Hosts first.
    use_project_jumps: bool,
    jumps: Vec<JumpRow>,
    run_project_commands: bool,
    icon: String,
    icon_query: Entity<InputState>,
    error: Option<String>,
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
    jumps: Vec<JumpRow>,
    commands: Entity<TextareaState>,
    error: Option<String>,
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

/// A Jump Host read from a form row, before its new password (if any) is in the Vault.
struct JumpEntry {
    jump: JumpHost,
    new_password: Option<String>,
}

fn lines(commands: &[String]) -> String {
    commands.join("\n")
}

/// The non-empty lines of a commands field.
fn commands_of(field: &Entity<TextareaState>, cx: &App) -> Vec<String> {
    field.read(cx).value().lines().map(str::trim).filter(|l| !l.is_empty()).map(str::to_string).collect()
}

impl Workspace {
    // ---- host form ----------------------------------------------------------------------

    pub(super) fn open_new_host(&mut self, project_id: String, window: &mut Window, cx: &mut Context<Self>) {
        self.open_host_form(project_id, None, window, cx);
    }

    pub(super) fn open_edit_host(&mut self, project_id: String, host_id: String, window: &mut Window, cx: &mut Context<Self>) {
        self.open_host_form(project_id, Some(host_id), window, cx);
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
        let address = self.form_input("", &h.address, window, cx);
        let port = self.form_input("22", &port, window, cx);
        let user = self.form_input("", h.user.own().map_or("", |u| u.as_str()), window, cx);
        let key_path = self.form_input(r"C:\Users\you\.ssh\id_ed25519", "", window, cx);
        let commands = commands_field(&lines(&h.startup_commands), window, cx);
        let jumps = h.jump_hosts.iter().map(|j| self.jump_row(Some(j), window, cx)).collect();
        name.update(cx, |f, cx| f.focus(window, cx));
        let sign_in = match h.credential {
            Inherited::Project => SignIn::Project,
            Inherited::Own(_) => SignIn::Saved,
            Inherited::Off => SignIn::Ask,
        };
        let icon_query = self.new_icon_query(window, cx);
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
            use_project_jumps: h.use_project_jump_hosts,
            jumps,
            run_project_commands: h.run_project_commands,
            icon: h.icon.unwrap_or_else(|| "server".into()),
            icon_query,
            error: None,
        }));
        cx.notify();
    }

    /// A one-line form field; Enter in it submits the open form.
    fn form_input(&mut self, placeholder: &str, value: &str, window: &mut Window, cx: &mut Context<Self>) -> Entity<InputState> {
        let (placeholder, value) = (placeholder.to_string(), value.to_string());
        let field = cx.new(|cx| InputState::new(window, cx).placeholder(placeholder).default_value(value));
        let sub = cx.subscribe_in(&field, window, |this, _, event: &InputEvent, window, cx| {
            if let InputEvent::PressEnter { .. } = event {
                this.submit_form(window, cx);
            }
        });
        self._subscriptions.push(sub);
        field
    }

    pub(super) fn submit_form(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        // A key file or a Jump Host's password goes into the Vault: ask for the master
        // password first, then save.
        let (sign_in, key_path, jumps) = match &self.overlay {
            Some(Overlay::HostForm(form)) => (form.sign_in, form.key_path.clone(), &form.jumps),
            Some(Overlay::ProjectForm(form)) => (form.sign_in, form.key_path.clone(), &form.jumps),
            _ => return,
        };
        let new_password = jumps.iter().any(|row| !row.password.read(cx).value().is_empty());
        if self.vault.is_none() {
            let reason = if sign_in == SignIn::KeyFile && !key_path.read(cx).value().trim().is_empty() {
                Some("The key file is copied into your vault, encrypted.")
            } else if new_password {
                Some("Jump host passwords are saved in your vault, encrypted.")
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
                let field = match this.overlay.as_mut() {
                    Some(Overlay::HostForm(form)) => {
                        form.sign_in = SignIn::KeyFile;
                        form.error = None;
                        form.key_path.clone()
                    }
                    Some(Overlay::ProjectForm(form)) => {
                        form.sign_in = SignIn::KeyFile;
                        form.error = None;
                        form.key_path.clone()
                    }
                    _ => return,
                };
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
        let key = std::fs::read_to_string(path).map_err(|err| format!("Could not read {path}: {err}"))?;
        vault
            .add(Credential { label, secret: Secret::PrivateKey { key, passphrase: None } })
            .map_err(|err| format!("Could not store the key: {err}"))
    }

    /// Save a changed copy of the Library. It is taken only once it is on disk, so a failed
    /// write changes nothing.
    pub(super) fn commit_library(&mut self, library: Library) -> Result<(), String> {
        library.save(&self.data_dir).map_err(|err| format!("Could not save: {err:#}"))?;
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
                let Some(vault) = self.vault.as_mut() else {
                    self.forget_added(added);
                    return Err("Unlock the vault to save the jump host's password.".into());
                };
                let credential = Credential { label: format!("{} (jump host)", jump.label()), secret: Secret::Password { password } };
                match vault.add(credential) {
                    Ok(id) => {
                        added.push(id.clone());
                        jump.credential = Some(id);
                    }
                    Err(err) => {
                        self.forget_added(added);
                        return Err(format!("Could not store the password: {err}"));
                    }
                }
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
        self.finish_form(result, window, cx);
    }

    /// Show `message` on the open host or project form.
    pub(super) fn show_form_error(&mut self, message: &str, cx: &mut Context<Self>) {
        match self.overlay.as_mut() {
            Some(Overlay::HostForm(form)) => form.error = Some(message.to_string()),
            Some(Overlay::ProjectForm(form)) => form.error = Some(message.to_string()),
            _ => {}
        }
        cx.notify();
    }

    fn finish_form(&mut self, result: Result<(), String>, window: &mut Window, cx: &mut Context<Self>) {
        match result {
            Ok(()) => self.close_overlay(window, cx),
            Err(message) => self.show_form_error(&message, cx),
        }
    }

    fn save_host_form(&mut self, cx: &mut Context<Self>) -> Result<(), String> {
        let Some(Overlay::HostForm(form)) = self.overlay.as_ref() else { return Ok(()) };
        let text = |field: &Entity<InputState>| field.read(cx).value().trim().to_string();
        let (name, address, port, user, key_path) =
            (text(&form.name), text(&form.address), text(&form.port), text(&form.user), text(&form.key_path));
        if name.is_empty() || address.is_empty() {
            return Err("Give the host a name and an address.".into());
        }
        let Some(port) = parse_port(&port) else { return Err("The port is a number from 1 to 65535.".into()) };
        let user = match form.user_source {
            Source::Project => Inherited::Project,
            Source::Off => Inherited::Off,
            Source::Own if user.is_empty() => return Err("Type a user name, or take the project's.".into()),
            Source::Own => Inherited::Own(user),
        };
        let jumps = read_jump_rows(&form.jumps, cx)?;
        let use_project_jumps = form.use_project_jumps;
        let startup_commands = commands_of(&form.commands, cx);
        let (project_id, editing, sign_in, run_project_commands, icon) =
            (form.project_id.clone(), form.editing.clone(), form.sign_in, form.run_project_commands, form.icon.clone());

        let mut host = editing.as_deref().and_then(|id| self.library.host(id)).cloned().unwrap_or_else(|| Host::new("", ""));
        let mut imported = None;
        host.credential = match sign_in {
            SignIn::Project => Inherited::Project,
            SignIn::Saved => host.credential.clone(),
            SignIn::Ask => Inherited::Off,
            SignIn::KeyFile => {
                let id = self.import_key(format!("{name} key"), &key_path)?;
                imported = Some(id.clone());
                Inherited::Own(id)
            }
        };
        host.name = name;
        host.address = address;
        host.port = port;
        host.user = user;
        host.use_project_jump_hosts = use_project_jumps;
        host.startup_commands = startup_commands;
        host.run_project_commands = run_project_commands;
        host.icon = Some(icon);
        let before = std::mem::take(&mut host.jump_hosts);

        self.commit_form(jumps, imported.into_iter().collect(), before, move |library, jumps| {
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
        })
    }

    pub(super) fn render_host_form(&self, form: &HostForm, wide: bool, p: AppPalette, cx: &mut Context<Self>) -> AnyElement {
        let project = self.library.project(&form.project_id);
        // No project yet (the first connection, from the start screen): the host goes into a
        // new one, created when it is saved.
        let project_name = project.map(|pr| pr.name.clone()).unwrap_or_else(|| FIRST_PROJECT_NAME.into());
        let project_icon = project.map(|pr| pr.icon.clone()).unwrap_or_else(|| "folder".into());
        let color = project_color(project.map(|pr| pr.color.as_str()).unwrap_or(FIRST_PROJECT_COLOR));
        let host = form.editing.as_deref().and_then(|id| self.library.host(id));
        let title = match host {
            Some(h) => format!("Edit {}", h.name),
            None => "Add host".to_string(),
        };

        let connection = form_section("Connection", p)
            .child(form_field("Name", None, Input::new(&form.name).into_any_element(), p))
            .child(
                div()
                    .flex()
                    .gap(px(10.))
                    .child(div().flex_1().min_w_0().child(form_field("Address", None, Input::new(&form.address).into_any_element(), p)))
                    .child(div().w(px(92.)).flex_shrink_0().child(form_field("Port", None, Input::new(&form.port).into_any_element(), p))),
            );

        let sign_in = form_section("Sign-in", p)
            .child(self.host_user_field(form, project, p, cx))
            .child(self.host_credential_field(form, host, project, p, cx));

        let route = form_section("Jump hosts", p).child(self.host_jump_field(form, project, wide, p, cx));

        let project_commands = project.map(|pr| pr.startup_commands.clone()).unwrap_or_default();
        let commands = form_section("Startup commands", p)
            .when(!project_commands.is_empty(), |el| {
                el.child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(10.))
                        .child(
                            switch("hf-project-commands".into(), "Run the project's commands first", form.run_project_commands, p).on_click(cx.listener(
                                |this, _: &ClickEvent, _, cx| {
                                    if let Some(form) = this.host_form() {
                                        form.run_project_commands = !form.run_project_commands;
                                    }
                                    cx.notify();
                                },
                            )),
                        )
                        .child(div().text_size(px(12.5)).text_color(p.text).child("Run the project's commands first")),
                )
                .when(form.run_project_commands, |el| el.child(commands_preview(&project_commands, p)))
            })
            .child(form_field(
                "This host's commands",
                Some("One per line. Typed into the shell once it is ready, as if you typed them."),
                Textarea::new(&form.commands).into_any_element(),
                p,
            ));

        let selected = form.icon.clone();
        let icon = form_section("Icon", p).child(icon_picker(&form.icon_query, &selected, color, p, cx, |this, name| {
            if let Some(form) = this.host_form() {
                form.icon = name;
            }
        }));
        // Two columns: who it is and how you sign in on the left, what runs on it and how it
        // looks on the right. The jump hosts, a row of fields each, get the full width below.
        let body = form_body(
            wide,
            vec![connection.into_any_element(), sign_in.into_any_element()],
            vec![commands.into_any_element(), icon.into_any_element()],
            vec![route.into_any_element()],
        );

        let ok_label = if form.editing.is_some() { "Save" } else { "Add host" };
        form_shell(
            "host-form-body",
            wide,
            form_header(badge(&project_icon, color, 34.).into_any_element(), title, format!("SSH host in {project_name}"), p),
            body,
            form_footer("hf", ok_label, form.error.as_deref(), p, cx),
        )
    }

    fn host_user_field(&self, form: &HostForm, project: Option<&Project>, p: AppPalette, cx: &mut Context<Self>) -> AnyElement {
        let options = [("From project", Source::Project), ("Custom", Source::Own), ("Ask", Source::Off)];
        let choice = source_choice("hf-user", &options, form.user_source, p, cx, |form, source| form.user_source = source);
        let hint = match (form.user_source, project.and_then(|pr| pr.default_user.as_deref())) {
            (Source::Project, Some(user)) => format!("Signs in as {user}, the project's default user."),
            (Source::Project, None) => "The project sets no user, so it is asked when you connect.".to_string(),
            (Source::Own, _) => String::new(),
            (Source::Off, _) => "Asked each time you connect.".to_string(),
        };
        div()
            .flex()
            .flex_col()
            .gap(px(8.))
            .child(field_label("User", p))
            .child(div().flex().child(choice))
            .when(form.user_source == Source::Own, |el| el.child(Input::new(&form.user)))
            .when(!hint.is_empty(), |el| el.child(hint_text(hint, p)))
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
        let labels: Vec<(&'static str, bool)> = options.iter().map(|(l, s)| (*l, *s == form.sign_in)).collect();
        let picks: Vec<SignIn> = options.iter().map(|(_, s)| *s).collect();
        let choice = segmented("hf-sign-in", &labels, p, cx, move |this, i, _, cx| {
            if let Some(form) = this.host_form() {
                form.sign_in = picks[i];
                form.error = None;
            }
            cx.notify();
        });
        let hint = match form.sign_in {
            SignIn::Project if project_credential.is_some() => {
                "Uses the key or password saved for the project, shared by all its hosts."
            }
            SignIn::Project => "The project has no saved key or password, so a password is asked when you connect.",
            SignIn::Saved => "Keeps the key or password already saved in the vault for this host.",
            SignIn::KeyFile => "The key is copied into the vault, encrypted. The file itself isn't needed afterwards.",
            SignIn::Ask => "A password is asked each time, even if the project has one saved. Komando offers to save it for this host.",
        };
        div()
            .flex()
            .flex_col()
            .gap(px(8.))
            .child(field_label("Key or password", p))
            .child(div().flex().child(choice))
            .when(form.sign_in == SignIn::KeyFile, |el| el.child(key_file_row(&form.key_path, "hf", p, cx)))
            .child(hint_text(hint.to_string(), p))
            .into_any_element()
    }

    fn host_jump_field(&self, form: &HostForm, project: Option<&Project>, wide: bool, p: AppPalette, cx: &mut Context<Self>) -> AnyElement {
        let project_jumps = project.map(|pr| pr.jump_hosts.clone()).unwrap_or_default();
        let through_project = form.use_project_jumps && !project_jumps.is_empty();
        let own = form.jumps.len();
        let hint = match (through_project, own) {
            (false, 0) => "Connects directly from this PC.",
            (true, 0) => "Komando signs in to the project's jump hosts in turn, then reaches this host from the last.",
            (false, _) => "Komando signs in to each jump host in turn, top first, then reaches this host from the last.",
            (true, _) => "Komando signs in to the project's jump hosts, then to these, then reaches this host from the last.",
        };
        div()
            .flex()
            .flex_col()
            .gap(px(10.))
            .when(!project_jumps.is_empty(), |el| {
                el.child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(10.))
                        .child(switch("hf-project-jumps".into(), "Go through the project's jump hosts first", form.use_project_jumps, p).on_click(
                            cx.listener(|this, _: &ClickEvent, _, cx| {
                                if let Some(form) = this.host_form() {
                                    form.use_project_jumps = !form.use_project_jumps;
                                }
                                cx.notify();
                            }),
                        ))
                        .child(div().text_size(px(12.5)).text_color(p.text).child("Go through the project's jump hosts first")),
                )
                .when(form.use_project_jumps, |el| {
                    el.child(commands_preview(&project_jumps.iter().map(JumpHost::label).collect::<Vec<_>>(), p))
                })
            })
            .child(self.jump_list("hf", &form.jumps, wide, p, cx))
            .child(hint_text(hint.into(), p))
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

    /// The open form's Jump Host rows.
    fn jump_rows(&mut self) -> Option<&mut Vec<JumpRow>> {
        match self.overlay.as_mut() {
            Some(Overlay::HostForm(form)) => Some(&mut form.jumps),
            Some(Overlay::ProjectForm(form)) => Some(&mut form.jumps),
            _ => None,
        }
    }

    fn add_jump_row(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let row = self.jump_row(None, window, cx);
        let address = row.address.clone();
        if let Some(rows) = self.jump_rows() {
            rows.push(row);
            address.update(cx, |f, cx| f.focus(window, cx));
        }
        cx.notify();
    }

    /// Change the row with `key`, if the form still has it.
    fn update_jump_row(&mut self, key: usize, change: impl FnOnce(&mut JumpRow)) {
        if let Some(row) = self.jump_rows().and_then(|rows| rows.iter_mut().find(|r| r.key == key)) {
            change(row);
        }
    }

    /// The Jump Hosts of a form, one row each, with a button to add another.
    /// In a narrow form the user and port fields shrink and the password button shows only
    /// its icon, so the address keeps room.
    fn jump_list(&self, prefix: &'static str, rows: &[JumpRow], wide: bool, p: AppPalette, cx: &mut Context<Self>) -> AnyElement {
        let (user_width, port_width, actions_width) = if wide { (140., 72., 170.) } else { (96., 60., 70.) };
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
                            icon_text_button((prefix, 100 + key * 4), password_icon, if wide { password_label } else { "" }, ButtonKind::Secondary, p)
                                .flex_1()
                                .px(px(10.))
                                .when(!wide, |el| el.gap(px(0.)))
                                .aria_label(format!("{password_label} for jump host {}", i + 1))
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
                                    if let Some(rows) = this.jump_rows() {
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
                            .child(div().flex_1().min_w_0().child(Input::new(&row.password)))
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
        let key_path = self.form_input(r"C:\Users\you\.ssh\id_ed25519", "", window, cx);
        let commands = commands_field(&lines(&pr.startup_commands), window, cx);
        let jumps = pr.jump_hosts.iter().map(|j| self.jump_row(Some(j), window, cx)).collect();
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
            jumps,
            commands,
            error: None,
        }));
        cx.notify();
    }

    fn submit_project_form(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let result = self.save_project_form(cx);
        self.finish_form(result, window, cx);
    }

    fn save_project_form(&mut self, cx: &mut Context<Self>) -> Result<(), String> {
        let Some(Overlay::ProjectForm(form)) = self.overlay.as_ref() else { return Ok(()) };
        let text = |field: &Entity<InputState>| field.read(cx).value().trim().to_string();
        let (name, default_user, key_path) = (text(&form.name), text(&form.default_user), text(&form.key_path));
        if name.is_empty() {
            return Err("Give the project a name.".into());
        }
        let jumps = read_jump_rows(&form.jumps, cx)?;
        let startup_commands = commands_of(&form.commands, cx);
        let (editing, color, icon, sign_in) = (form.editing.clone(), form.color.clone(), form.icon.clone(), form.sign_in);

        let mut project = editing
            .as_deref()
            .and_then(|id| self.library.project(id))
            .cloned()
            .unwrap_or_else(|| Project::new("", "", ""));
        let mut imported = None;
        project.credential = match sign_in {
            SignIn::Saved => project.credential.clone(),
            SignIn::KeyFile => {
                let id = self.import_key(format!("{name} project key"), &key_path)?;
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
        let before = std::mem::take(&mut project.jump_hosts);

        self.commit_form(jumps, imported.into_iter().collect(), before, move |library, jumps| {
            project.jump_hosts = jumps;
            match library.projects.iter_mut().find(|pr| pr.id == project.id) {
                Some(existing) => *existing = project,
                None => library.projects.push(project),
            }
        })
    }

    pub(super) fn render_project_form(&self, form: &ProjectForm, wide: bool, p: AppPalette, cx: &mut Context<Self>) -> AnyElement {
        let (title, ok_label) = if form.editing.is_some() { ("Edit project", "Save") } else { ("New project", "Create project") };
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
                .focus_visible(move |s| s.border_2().border_color(p.brand))
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
        let color = project_color(&current);

        // Sign-in for the Project's Hosts: the Project Credential.
        let has_saved = form.editing.as_deref().and_then(|id| self.library.project(id)).is_some_and(|pr| pr.credential.is_some());
        let mut options = vec![("None", SignIn::Ask)];
        if has_saved {
            options.push(("Saved", SignIn::Saved));
        }
        options.push(("Key file", SignIn::KeyFile));
        let labels: Vec<(&'static str, bool)> = options.iter().map(|(l, s)| (*l, *s == form.sign_in)).collect();
        let picks: Vec<SignIn> = options.iter().map(|(_, s)| *s).collect();
        let sign_in_choice = segmented("pf-sign-in", &labels, p, cx, move |this, i, _, cx| {
            if let Some(form) = this.project_form() {
                form.sign_in = picks[i];
                form.error = None;
            }
            cx.notify();
        });
        let sign_in_hint = match form.sign_in {
            SignIn::Saved => "Its hosts use the key or password saved for the project.",
            SignIn::KeyFile => "The key is copied into the vault, encrypted, and shared by the project's hosts.",
            _ => "Hosts ask for a password when you connect. Komando can save a typed one for the whole project.",
        };

        let defaults = form_section("Defaults for its hosts", p)
            .child(hint_text("Each host takes these unless it sets its own.".into(), p))
            .child(form_field("User", Some("Empty: asked when you connect."), Input::new(&form.default_user).into_any_element(), p))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(8.))
                    .child(field_label("Key or password", p))
                    .child(div().flex().child(sign_in_choice))
                    .when(form.sign_in == SignIn::KeyFile, |el| el.child(key_file_row(&form.key_path, "pf", p, cx)))
                    .child(hint_text(sign_in_hint.into(), p)),
            );
        let jump_hint = if form.jumps.is_empty() {
            "Its hosts connect directly from this PC, unless they add jump hosts of their own."
        } else {
            "Its hosts are reached through these, top first, unless a host switches them off. A host can add its own after them."
        };
        let jumps = form_section("Jump hosts", p).child(self.jump_list("pf", &form.jumps, wide, p, cx)).child(hint_text(jump_hint.into(), p));
        let commands = form_section("Startup commands", p).child(form_field(
            "For every host",
            Some("One per line. Typed into every host's shell once it is ready, before the host's own."),
            Textarea::new(&form.commands).into_any_element(),
            p,
        ));

        let selected_icon = form.icon.clone();
        let project = form_section("Project", p)
            .child(form_field("Name", None, Input::new(&form.name).into_any_element(), p))
            .child(field_label("Color", p))
            .child(swatches);
        let icon = form_section("Icon", p).child(icon_picker(&form.icon_query, &selected_icon, color, p, cx, |this, name| {
            if let Some(form) = this.project_form() {
                form.icon = name;
            }
        }));
        // Two columns: the project itself on the left, what its hosts inherit on the right. The
        // jump hosts, a row of fields each, get the full width below.
        let (left, right) = if wide {
            (vec![project.into_any_element(), icon.into_any_element()], vec![defaults.into_any_element(), commands.into_any_element()])
        } else {
            (vec![project.into_any_element(), defaults.into_any_element(), commands.into_any_element(), icon.into_any_element()], vec![])
        };
        let body = form_body(wide, left, right, vec![jumps.into_any_element()]);

        let subtitle = if form.editing.is_some() { "Its settings apply to all its hosts" } else { "A group of hosts that belong together" };
        form_shell(
            "project-form-body",
            wide,
            form_header(badge(&form.icon, color, 34.).into_any_element(), title.into(), subtitle.into(), p),
            body,
            form_footer("pf", ok_label, form.error.as_deref(), p, cx),
        )
    }
}

/// A multi-line field for Startup Commands.
fn commands_field(value: &str, window: &mut Window, cx: &mut Context<Workspace>) -> Entity<TextareaState> {
    let value = value.to_string();
    cx.new(|cx| {
        TextareaState::new(window, cx)
            .auto_grow(2, 8)
            .default_value(value)
    })
}

/// A Source picked with a segmented control, written back into the host form.
fn source_choice(
    id: &'static str,
    options: &[(&'static str, Source)],
    current: Source,
    p: AppPalette,
    cx: &mut Context<Workspace>,
    set: impl Fn(&mut HostForm, Source) + Clone + 'static,
) -> AnyElement {
    let labels: Vec<(&'static str, bool)> = options.iter().map(|(l, s)| (*l, *s == current)).collect();
    let picks: Vec<Source> = options.iter().map(|(_, s)| *s).collect();
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
        .rounded(px(8.))
        .bg(p.hover)
        .flex()
        .flex_col()
        .gap(px(2.))
        .font_family("Cascadia Mono")
        .text_size(px(12.))
        .text_color(p.text_muted)
        .children(commands.iter().map(|c| div().truncate().child(c.clone())))
        .into_any_element()
}

fn field_label(label: &str, p: AppPalette) -> AnyElement {
    div().text_size(px(12.5)).font_weight(FontWeight::MEDIUM).text_color(p.text).child(label.to_string()).into_any_element()
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
        .rounded(px(8.))
        .bg(p.danger.opacity(0.1))
        .text_size(px(12.5))
        .text_color(p.danger)
        .child(icon_el("circle-alert", 14., p.danger))
        .child(message)
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
        .gap(px(8.))
        .child(div().flex_1().text_size(px(11.5)).text_color(p.text_faint).child("Enter to save · Esc to cancel"))
        .child(
            button((id, 1usize), "Cancel", ButtonKind::Ghost, p)
                .on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.close_overlay(window, cx))),
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
        .gap(px(8.))
        .border_t_1()
        .border_color(p.border)
        .children(error.map(|message| form_error(message.to_string(), p)))
        .child(actions)
        .into_any_element()
}

/// Width of a form laid out in two columns; one column is `NARROW_FORM_WIDTH`.
pub(super) const WIDE_FORM_WIDTH: f32 = 880.;
const NARROW_FORM_WIDTH: f32 = 520.;

/// A form's sections: in two columns side by side when `wide`, else `left` then `right` in
/// one; `full` after them at full width. Validation feedback lives in the fixed footer.
fn form_body(wide: bool, left: Vec<AnyElement>, right: Vec<AnyElement>, full: Vec<AnyElement>) -> AnyElement {
    let column = || div().flex().flex_col().gap(px(20.));
    let sections = if wide {
        div()
            .flex()
            .items_start()
            .gap(px(28.))
            .child(column().flex_1().min_w_0().children(left))
            .child(column().flex_1().min_w_0().children(right))
    } else {
        column().children(left).children(right)
    };
    div().flex().flex_col().gap(px(20.)).child(sections).children(full).into_any_element()
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

/// A port field: empty is 22.
fn parse_port(text: &str) -> Option<u16> {
    match text {
        "" => Some(22),
        port => port.parse::<u16>().ok().filter(|p| *p > 0),
    }
}

/// A form's layout: header and footer stay put, the middle scrolls.
fn form_shell(id: &'static str, wide: bool, header: AnyElement, body: AnyElement, footer: AnyElement) -> AnyElement {
    div()
        .id(SharedString::from(format!("{id}-dialog")))
        .role(gpui::Role::Dialog)
        .aria_label(if id == "host-form-body" { "Host" } else { "Project" })
        .tab_group()
        .w(px(if wide { WIDE_FORM_WIDTH } else { NARROW_FORM_WIDTH }))
        .flex_1()
        .min_h_0()
        .flex()
        .flex_col()
        .child(header)
        .child(div().id(id).flex_1().min_h_0().overflow_y_scroll().px(px(24.)).pb(px(22.)).child(body))
        .child(footer)
        .into_any_element()
}
