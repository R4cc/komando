//! The host and project forms.
//!
//! Both edit the settings a Project hands down to its Hosts in the same way: the Project sets
//! a default (user, Credential, Jump Host, Startup Commands), and in the host form each one
//! is a choice between taking the Project's, setting the Host's own, or switching it off.
//! Startup Commands add up instead: the Project's run first, then the Host's.

use gpui::prelude::FluentBuilder as _;
use gpui::{AnyElement, App, ClickEvent, Context, Entity, FontWeight, SharedString, Window, div, prelude::*, px};
use gpui_component::input::{Input, InputEvent, InputState, Textarea, TextareaState};

use super::{FIRST_PROJECT_COLOR, FIRST_PROJECT_NAME, Overlay, Workspace, form_field, form_section, icon_picker, segmented, switch};
use crate::model::{CredentialId, Host, HostId, Inherited, Library, Project};
use crate::theme::{AppPalette, project_color};
use crate::ui::widgets::{ButtonKind, badge, button, icon_el};
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
    jump_source: Source,
    jump_host: Option<HostId>,
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
    jump_host: Option<HostId>,
    /// "Reached through a jump host" is picked, maybe before a host is.
    uses_jump_host: bool,
    commands: Entity<TextareaState>,
    error: Option<String>,
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

    fn open_host_form(&mut self, project_id: String, editing: Option<String>, window: &mut Window, cx: &mut Context<Self>) {
        // One dialog at a time: a Session's open question comes first.
        if self.prompt_open(cx) {
            return;
        }
        let host = editing.as_deref().and_then(|id| self.library.host(id)).cloned();
        let h = host.clone().unwrap_or_else(|| Host::new("", ""));
        let port = if h.port == 22 { String::new() } else { h.port.to_string() };
        let name = self.form_input("", &h.name, true, window, cx);
        let address = self.form_input("", &h.address, true, window, cx);
        let port = self.form_input("22", &port, true, window, cx);
        let user = self.form_input("", h.user.own().map_or("", |u| u.as_str()), true, window, cx);
        let key_path = self.form_input(r"C:\Users\you\.ssh\id_ed25519", "", true, window, cx);
        let commands = commands_field(&lines(&h.startup_commands), window, cx);
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
            jump_source: Source::of(&h.jump_host),
            jump_host: h.jump_host.own().cloned(),
            run_project_commands: h.run_project_commands,
            icon: h.icon.unwrap_or_else(|| "server".into()),
            icon_query,
            error: None,
        }));
        cx.notify();
    }

    /// A one-line form field; Enter in it submits the open form.
    fn form_input(&mut self, placeholder: &str, value: &str, submit: bool, window: &mut Window, cx: &mut Context<Self>) -> Entity<InputState> {
        let (placeholder, value) = (placeholder.to_string(), value.to_string());
        let field = cx.new(|cx| InputState::new(window, cx).placeholder(placeholder).default_value(value));
        if submit {
            let sub = cx.subscribe_in(&field, window, |this, _, event: &InputEvent, window, cx| {
                if let InputEvent::PressEnter { .. } = event {
                    this.submit_form(window, cx);
                }
            });
            self._subscriptions.push(sub);
        }
        field
    }

    fn submit_form(&mut self, window: &mut Window, cx: &mut Context<Self>) {
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

    /// Save a changed copy of the Library, unless a Host could no longer be reached.
    fn commit_library(&mut self, library: Library) -> Result<(), String> {
        library.check_routes().map_err(|err| err.to_string())?;
        self.library = library;
        let _ = self.library.save(&self.data_dir);
        Ok(())
    }

    fn submit_host_form(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let result = self.save_host_form(cx);
        self.finish_form(result, window, cx);
    }

    fn finish_form(&mut self, result: Result<(), String>, window: &mut Window, cx: &mut Context<Self>) {
        match result {
            Ok(()) => self.close_overlay(window, cx),
            Err(message) => {
                match self.overlay.as_mut() {
                    Some(Overlay::HostForm(form)) => form.error = Some(message),
                    Some(Overlay::ProjectForm(form)) => form.error = Some(message),
                    _ => {}
                }
                cx.notify();
            }
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
        let port = match port.as_str() {
            "" => 22,
            port => match port.parse::<u16>() {
                Ok(p) if p > 0 => p,
                _ => return Err("The port is a number from 1 to 65535.".into()),
            },
        };
        let user = match form.user_source {
            Source::Project => Inherited::Project,
            Source::Off => Inherited::Off,
            Source::Own if user.is_empty() => return Err("Type a user name, or take the project's.".into()),
            Source::Own => Inherited::Own(user),
        };
        let jump_host = match (form.jump_source, &form.jump_host) {
            (Source::Project, _) => Inherited::Project,
            (Source::Off, _) => Inherited::Off,
            (Source::Own, Some(id)) => Inherited::Own(id.clone()),
            (Source::Own, None) => return Err("Pick the host it is reached through.".into()),
        };
        let startup_commands = commands_of(&form.commands, cx);
        let (project_id, editing, sign_in, run_project_commands, icon) =
            (form.project_id.clone(), form.editing.clone(), form.sign_in, form.run_project_commands, form.icon.clone());

        let mut host = editing.as_deref().and_then(|id| self.library.host(id)).cloned().unwrap_or_else(|| Host::new("", ""));
        host.credential = match sign_in {
            SignIn::Project => Inherited::Project,
            SignIn::Saved => host.credential.clone(),
            SignIn::Ask => Inherited::Off,
            SignIn::KeyFile => Inherited::Own(self.import_key(format!("{name} key"), &key_path)?),
        };
        host.name = name;
        host.address = address;
        host.port = port;
        host.user = user;
        host.jump_host = jump_host;
        host.startup_commands = startup_commands;
        host.run_project_commands = run_project_commands;
        host.icon = Some(icon);

        let mut library = self.library.clone();
        match library.hosts.iter_mut().find(|h| h.id == host.id) {
            Some(existing) => *existing = host,
            None => {
                match library.projects.iter_mut().find(|p| p.id == project_id) {
                    Some(project) => project.hosts.push(host.id.clone()),
                    // The first connection, added from the start screen: give it a project.
                    None => library.projects.push(Project {
                        hosts: vec![host.id.clone()],
                        ..Project::new(FIRST_PROJECT_NAME, FIRST_PROJECT_COLOR, "folder")
                    }),
                }
                library.hosts.push(host);
            }
        }
        self.commit_library(library)
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

        let route = form_section("Reached through", p).child(self.host_jump_field(form, project, p, cx));

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
        // Two columns: who it is and how you sign in on the left, how it is reached and what
        // runs on it on the right.
        let body = form_body(
            wide,
            vec![connection.into_any_element(), sign_in.into_any_element(), icon.into_any_element()],
            vec![route.into_any_element(), commands.into_any_element()],
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

    fn host_jump_field(&self, form: &HostForm, project: Option<&Project>, p: AppPalette, cx: &mut Context<Self>) -> AnyElement {
        let options = [("From project", Source::Project), ("Jump host", Source::Own), ("Direct", Source::Off)];
        let choice = source_choice("hf-jump", &options, form.jump_source, p, cx, |form, source| form.jump_source = source);
        let project_jump = project.and_then(|pr| pr.jump_host.as_deref());
        let is_project_jump = form.editing.is_some() && project_jump == form.editing.as_deref();
        let hint = match form.jump_source {
            Source::Project if is_project_jump => "This is the project's jump host: it connects directly.".to_string(),
            Source::Project => match project_jump.and_then(|id| self.library.host(id)) {
                Some(jump) => format!("Reached through {}, the project's jump host.", jump.name),
                None => "The project sets no jump host, so it connects directly.".to_string(),
            },
            Source::Own => "Komando signs in to the jump host first and reaches this host from there.".to_string(),
            Source::Off => "Connects directly from this PC.".to_string(),
        };
        let exclude = form.editing.clone();
        div()
            .flex()
            .flex_col()
            .gap(px(8.))
            .child(div().flex().child(choice))
            .child(hint_text(hint, p))
            .when(form.jump_source == Source::Own, |el| {
                el.child(self.host_list("hf-jump-list", form.jump_host.as_deref(), exclude.as_deref(), p, cx, |this, id| {
                    if let Some(form) = this.host_form() {
                        form.jump_host = Some(id);
                        form.error = None;
                    }
                }))
            })
            .into_any_element()
    }

    /// A list of saved Hosts to pick a Jump Host from.
    fn host_list(
        &self,
        id: &'static str,
        selected: Option<&str>,
        exclude: Option<&str>,
        p: AppPalette,
        cx: &mut Context<Self>,
        pick: impl Fn(&mut Workspace, String) + Clone + 'static,
    ) -> AnyElement {
        let mut hosts: Vec<&Host> = self.library.hosts.iter().filter(|h| Some(h.id.as_str()) != exclude).collect();
        hosts.sort_by_key(|h| h.name.to_lowercase());
        if hosts.is_empty() {
            return hint_text("Add the jump host as a host first, then pick it here.".into(), p);
        }
        let rows = hosts.into_iter().enumerate().map(|(i, host)| {
            let project = self.library.projects.iter().find(|pr| pr.hosts.contains(&host.id));
            let color = host
                .color
                .as_deref()
                .or(project.map(|pr| pr.color.as_str()))
                .map(project_color)
                .unwrap_or(p.text_muted);
            let detail = match project {
                Some(pr) => format!("{} · {}", host.address, pr.name),
                None => host.address.clone(),
            };
            let is_selected = selected == Some(host.id.as_str());
            let (pick, host_id) = (pick.clone(), host.id.clone());
            div()
                .id((id, i))
                .h(px(40.))
                .px(px(10.))
                .flex()
                .items_center()
                .gap(px(10.))
                .rounded(px(8.))
                .cursor_pointer()
                .when(is_selected, |el| el.bg(p.brand_soft))
                .when(!is_selected, |el| el.hover(|s| s.bg(p.hover)))
                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                    pick(this, host_id.clone());
                    cx.notify();
                }))
                .child(badge(host.icon.as_deref().unwrap_or("server"), color, 24.))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .flex()
                        .flex_col()
                        .child(div().text_size(px(13.)).font_weight(FontWeight::MEDIUM).text_color(p.text).child(host.name.clone()))
                        .child(div().text_size(px(11.5)).text_color(p.text_faint).truncate().child(detail)),
                )
                .when(is_selected, |el| el.child(icon_el("check", 15., p.brand)))
        });
        div()
            .id(id)
            .max_h(px(176.))
            .overflow_y_scroll()
            .p(px(4.))
            .flex()
            .flex_col()
            .gap(px(2.))
            .rounded(px(10.))
            .border_1()
            .border_color(p.border)
            .children(rows)
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
        let name = self.form_input("", &pr.name, true, window, cx);
        let default_user = self.form_input("", pr.default_user.as_deref().unwrap_or(""), true, window, cx);
        let key_path = self.form_input(r"C:\Users\you\.ssh\id_ed25519", "", true, window, cx);
        let commands = commands_field(&lines(&pr.startup_commands), window, cx);
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
            uses_jump_host: pr.jump_host.is_some(),
            jump_host: pr.jump_host.clone(),
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
        let jump_host = match (form.uses_jump_host, &form.jump_host) {
            (false, _) => None,
            (true, Some(id)) => Some(id.clone()),
            (true, None) => return Err("Pick the jump host its hosts are reached through.".into()),
        };
        let startup_commands = commands_of(&form.commands, cx);
        let (editing, color, icon, sign_in) = (form.editing.clone(), form.color.clone(), form.icon.clone(), form.sign_in);

        let mut project = editing
            .as_deref()
            .and_then(|id| self.library.project(id))
            .cloned()
            .unwrap_or_else(|| Project::new("", "", ""));
        project.credential = match sign_in {
            SignIn::Saved => project.credential.clone(),
            SignIn::KeyFile => Some(self.import_key(format!("{name} project key"), &key_path)?),
            SignIn::Ask | SignIn::Project => None,
        };
        project.name = name;
        project.color = color;
        project.icon = icon;
        project.default_user = (!default_user.is_empty()).then_some(default_user);
        project.jump_host = jump_host;
        project.startup_commands = startup_commands;

        let mut library = self.library.clone();
        match library.projects.iter_mut().find(|pr| pr.id == project.id) {
            Some(existing) => *existing = project,
            None => library.projects.push(project),
        }
        self.commit_library(library)
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

        let jump_labels = [("Direct", !form.uses_jump_host), ("Jump host", form.uses_jump_host)];
        let jump_choice = segmented("pf-jump", &jump_labels, p, cx, |this, i, _, cx| {
            if let Some(form) = this.project_form() {
                form.uses_jump_host = i == 1;
                form.error = None;
            }
            cx.notify();
        });
        let jump_hint = if form.uses_jump_host {
            "Its hosts are reached through this host unless they choose otherwise. The jump host itself connects directly."
        } else {
            "Its hosts connect directly from this PC unless they choose a jump host."
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
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(8.))
                    .child(field_label("Reached through", p))
                    .child(div().flex().child(jump_choice))
                    .child(hint_text(jump_hint.into(), p))
                    .when(form.uses_jump_host, |el| {
                        el.child(self.host_list("pf-jump-list", form.jump_host.as_deref(), None, p, cx, |this, id| {
                            if let Some(form) = this.project_form() {
                                form.jump_host = Some(id);
                                form.error = None;
                            }
                        }))
                    }),
            );
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
        // Two columns: the project itself on the left, what its hosts inherit on the right.
        let (left, right) = if wide {
            (vec![project.into_any_element(), icon.into_any_element(), commands.into_any_element()], vec![defaults.into_any_element()])
        } else {
            (vec![project.into_any_element(), defaults.into_any_element(), commands.into_any_element(), icon.into_any_element()], vec![])
        };
        let body = form_body(wide, left, right);

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
        .px(px(24.))
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
/// one. Validation feedback lives in the fixed footer.
fn form_body(wide: bool, left: Vec<AnyElement>, right: Vec<AnyElement>) -> AnyElement {
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
    div().flex().flex_col().gap(px(20.)).child(sections).into_any_element()
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
