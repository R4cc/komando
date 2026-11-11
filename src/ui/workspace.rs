//! The main window: unlock screen, Project sidebar with its Sessions, the active terminal,
//! quick connect, and the small forms for adding Projects and Hosts.

use std::collections::HashSet;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, App, ClickEvent, Context, Entity, FocusHandle, Focusable, FontWeight, Hsla, KeyDownEvent,
    SharedString, Subscription, Window, WindowAppearance, actions, div, prelude::*, px,
};
use gpui_component::input::{Input, InputEvent, InputState};

use crate::model::{
    self, AppThemeSetting, CredentialSource, Host, Library, LocalShell, Project, Settings, TerminalThemeSetting,
};
use crate::session::{self, Status, ssh};
use crate::theme::{AppPalette, Appearance, PROJECT_COLORS, TerminalPalette, UI_FONT, alpha, project_color};
use crate::ui::terminal_view::{TerminalEvent, TerminalView};
use crate::ui::widgets::{ButtonKind, badge, button, elevation, icon_button, icon_el, section_label, status_dot, wordmark};
use crate::vault::{Credential, Secret, Vault};

actions!(
    komando,
    [QuickConnect, NewSession, CloseSession, NextSession, PrevSession, CopyOutputText, CopyOutputFile, LockVault]
);

const SIDEBAR_WIDTH: f32 = 264.;
const HOST_ICONS: [&str; 10] =
    ["server", "database", "globe", "shield", "hard-drive", "cpu", "network", "cloud", "container", "router"];

#[derive(Clone)]
enum SessionKind {
    Ssh { host_id: String },
    Local { shell: LocalShell },
}

struct SessionEntry {
    id: usize,
    project_id: String,
    kind: SessionKind,
    /// "web01" or "web01 (2)" when the same Host is open twice.
    label: SharedString,
    view: Entity<TerminalView>,
    _subscription: Subscription,
}

enum Overlay {
    Picker { query: Entity<InputState>, project_id: Option<String> },
    NewHost { project_id: String, fields: Vec<Entity<InputState>>, icon: String, error: Option<String> },
    NewProject { name: Entity<InputState>, color: String, icon: String, error: Option<String> },
}

enum Pending {
    Open { project_id: String, host_id: String },
    Save { session: usize, password: String, as_project: bool },
}

/// Things to do right after launch. Used by development tooling (screenshots, demos);
/// the Komando binary itself never passes a master password.
#[derive(Clone, Default)]
pub struct Startup {
    pub master_password: Option<String>,
    /// (Project name, Host or local shell name) pairs to open, in order.
    pub open: Vec<(String, String)>,
    pub active: Option<usize>,
}

pub struct Workspace {
    data_dir: PathBuf,
    library: Library,
    settings: Settings,
    vault: Option<Vault>,
    /// Shown full-window at launch; as a dialog when a locked Vault is needed later.
    unlock: Option<UnlockState>,
    pending: Option<Pending>,
    sessions: Vec<SessionEntry>,
    active: Option<usize>,
    next_id: usize,
    collapsed: HashSet<String>,
    overlay: Option<Overlay>,
    toast: Option<(SharedString, Instant)>,
    last_activity: Instant,
    focus: FocusHandle,
    _subscriptions: Vec<Subscription>,
}

struct UnlockState {
    creating: bool,
    password: Entity<InputState>,
    confirm: Entity<InputState>,
    error: Option<String>,
    full_screen: bool,
}

impl Focusable for Workspace {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Workspace {
    pub fn new(data_dir: PathBuf, skip_unlock: bool, startup: Startup, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let settings = Settings::load(&data_dir);
        let library = match Library::load(&data_dir) {
            Ok(lib) if !lib.projects.is_empty() => lib,
            Ok(_) => {
                let lib = model::starter_library();
                let _ = lib.save(&data_dir);
                lib
            }
            Err(err) => {
                log::error!("{err:#}");
                model::starter_library()
            }
        };
        let mut this = Self {
            data_dir,
            library,
            settings,
            vault: None,
            unlock: None,
            pending: None,
            sessions: vec![],
            active: None,
            next_id: 1,
            collapsed: HashSet::new(),
            overlay: None,
            toast: None,
            last_activity: Instant::now(),
            focus: cx.focus_handle(),
            _subscriptions: vec![],
        };
        this.apply_appearance(window, cx);
        let sub = cx.observe_window_appearance(window, |this, window, cx| this.apply_appearance(window, cx));
        this._subscriptions.push(sub);
        if let Some(password) = &startup.master_password {
            match Vault::unlock(&this.data_dir, password) {
                Ok(vault) => this.vault = Some(vault),
                Err(err) => log::error!("startup unlock: {err:#}"),
            }
        } else if !skip_unlock {
            this.show_unlock(true, window, cx);
        }
        this.start_auto_lock(window, cx);
        if !startup.open.is_empty() {
            cx.defer_in(window, move |this, window, cx| this.run_startup(&startup, window, cx));
        }
        this
    }

    fn run_startup(&mut self, startup: &Startup, window: &mut Window, cx: &mut Context<Self>) {
        for (project_name, item) in &startup.open {
            let Some(project) = self.library.projects.iter().find(|p| &p.name == project_name).cloned() else {
                continue;
            };
            let host = self.library.hosts_of(&project).find(|h| &h.name == item).cloned();
            if let Some(host) = host {
                self.open_host(&project.id, &host.id, window, cx);
            } else {
                let shells = if project.shells.is_empty() { model::default_shells() } else { project.shells.clone() };
                if let Some(shell) = shells.into_iter().find(|s| &s.name == item) {
                    self.open_local(&project.id, shell, window, cx);
                }
            }
        }
        if let Some(index) = startup.active
            && let Some(id) = self.sessions.get(index).map(|s| s.id)
        {
            self.activate(id, window, cx);
        }
    }

    // ---- appearance -------------------------------------------------------------------

    fn apply_appearance(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let system_dark = matches!(window.appearance(), WindowAppearance::Dark | WindowAppearance::VibrantDark);
        let app_dark = match self.settings.app_theme {
            AppThemeSetting::System => system_dark,
            AppThemeSetting::Light => false,
            AppThemeSetting::Dark => true,
        };
        let term_dark = match self.settings.terminal_theme {
            TerminalThemeSetting::Follow => app_dark,
            TerminalThemeSetting::Light => false,
            TerminalThemeSetting::Dark => true,
        };
        cx.set_global(Appearance {
            app: if app_dark { AppPalette::dark() } else { AppPalette::light() },
            term: if term_dark { TerminalPalette::dark() } else { TerminalPalette::light() },
            font_family: self.settings.font_family.clone().into(),
            font_size: self.settings.font_size,
        });
        gpui_component::Theme::change(
            if app_dark { gpui_component::ThemeMode::Dark } else { gpui_component::ThemeMode::Light },
            Some(window),
            cx,
        );
        let brand = cx.global::<Appearance>().app.brand;
        gpui_component::Theme::update(cx, |theme| {
            theme.colors.primary = brand;
            theme.colors.ring = brand;
            theme.colors.caret = brand;
        });
        cx.refresh_windows();
    }

    fn cycle_app_theme(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.settings.app_theme = match self.settings.app_theme {
            AppThemeSetting::System => AppThemeSetting::Light,
            AppThemeSetting::Light => AppThemeSetting::Dark,
            AppThemeSetting::Dark => AppThemeSetting::System,
        };
        let _ = self.settings.save(&self.data_dir);
        self.apply_appearance(window, cx);
    }

    fn cycle_terminal_theme(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.settings.terminal_theme = match self.settings.terminal_theme {
            TerminalThemeSetting::Follow => TerminalThemeSetting::Light,
            TerminalThemeSetting::Light => TerminalThemeSetting::Dark,
            TerminalThemeSetting::Dark => TerminalThemeSetting::Follow,
        };
        let _ = self.settings.save(&self.data_dir);
        self.apply_appearance(window, cx);
    }

    // ---- vault ------------------------------------------------------------------------

    fn show_unlock(&mut self, full_screen: bool, window: &mut Window, cx: &mut Context<Self>) {
        let creating = !Vault::exists(&self.data_dir);
        let password = cx.new(|cx| {
            InputState::new(window, cx)
                .masked(true)
                .placeholder(if creating { "New master password" } else { "Master password" })
        });
        let confirm = cx.new(|cx| InputState::new(window, cx).masked(true).placeholder("Repeat master password"));
        for input in [&password, &confirm] {
            let sub = cx.subscribe_in(input, window, |this, _, event: &InputEvent, window, cx| {
                if let InputEvent::PressEnter { .. } = event {
                    this.submit_unlock(window, cx);
                }
            });
            self._subscriptions.push(sub);
        }
        password.update(cx, |state, cx| state.focus(window, cx));
        self.unlock = Some(UnlockState { creating, password, confirm, error: None, full_screen });
        cx.notify();
    }

    fn submit_unlock(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(state) = self.unlock.as_mut() else { return };
        let password = state.password.read(cx).value().to_string();
        let result = if state.creating {
            let confirm = state.confirm.read(cx).value().to_string();
            if password != confirm {
                Err(anyhow::anyhow!("The two passwords don't match."))
            } else {
                Vault::create(&self.data_dir, &password)
            }
        } else {
            Vault::unlock(&self.data_dir, &password)
        };
        match result {
            Ok(vault) => {
                self.vault = Some(vault);
                self.unlock = None;
                self.last_activity = Instant::now();
                match self.pending.take() {
                    Some(Pending::Open { project_id, host_id }) => self.open_host(&project_id, &host_id, window, cx),
                    Some(Pending::Save { session, password, as_project }) => {
                        self.save_password(session, password, as_project, cx)
                    }
                    None => self.focus_active(window, cx),
                }
            }
            Err(err) => state.error = Some(err.to_string()),
        }
        cx.notify();
    }

    fn skip_unlock(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.unlock = None;
        if let Some(Pending::Open { project_id, host_id }) = self.pending.take() {
            self.open_host_with_secret(&project_id, &host_id, None, window, cx);
        }
        self.focus_active(window, cx);
        cx.notify();
    }

    fn lock_vault(&mut self, cx: &mut Context<Self>) {
        if self.vault.take().is_some() {
            self.show_toast("Vault locked. Open sessions stay connected.", cx);
        }
        cx.notify();
    }

    fn start_auto_lock(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        cx.spawn_in(window, async move |this, cx| {
            loop {
                cx.background_executor().timer(Duration::from_secs(20)).await;
                let alive = this.update(cx, |this, cx| {
                    let minutes = this.settings.auto_lock_minutes;
                    if minutes > 0
                        && this.vault.is_some()
                        && this.last_activity.elapsed() >= Duration::from_secs(minutes as u64 * 60)
                    {
                        this.lock_vault(cx);
                    }
                    if this.toast.as_ref().is_some_and(|(_, at)| at.elapsed() > Duration::from_secs(4)) {
                        this.toast = None;
                        cx.notify();
                    }
                });
                if alive.is_err() {
                    break;
                }
            }
        })
        .detach();
    }

    // ---- sessions ---------------------------------------------------------------------

    fn label_for(&self, base: &str) -> SharedString {
        let same = self.sessions.iter().filter(|s| s.label == base || s.label.starts_with(&format!("{base} ("))).count();
        if same == 0 { base.to_string().into() } else { format!("{base} ({})", same + 1).into() }
    }

    fn add_session(
        &mut self,
        project_id: &str,
        kind: SessionKind,
        base_label: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Entity<TerminalView> {
        let label = self.label_for(base_label);
        let scrollback = self.settings.scrollback_lines;
        let confirm_paste = self.settings.confirm_multiline_paste;
        let view = cx.new(|cx| {
            let mut view = TerminalView::new(label.clone(), scrollback, window, cx);
            view.confirm_multiline_paste = confirm_paste;
            view
        });
        let id = self.next_id;
        self.next_id += 1;
        let sub = cx.subscribe_in(&view, window, move |this, _, event: &TerminalEvent, window, cx| match event {
            TerminalEvent::Changed => cx.notify(),
            TerminalEvent::SavePassword { password, as_project } => {
                if this.vault.is_some() {
                    this.save_password(id, password.clone(), *as_project, cx);
                } else {
                    this.pending = Some(Pending::Save { session: id, password: password.clone(), as_project: *as_project });
                    this.show_unlock(false, window, cx);
                }
            }
            TerminalEvent::Reconnect => this.reconnect(id, window, cx),
        });
        self.sessions.push(SessionEntry {
            id,
            project_id: project_id.to_string(),
            kind,
            label,
            view: view.clone(),
            _subscription: sub,
        });
        self.collapsed.remove(project_id);
        self.activate(id, window, cx);
        view
    }

    fn open_local(&mut self, project_id: &str, shell: LocalShell, window: &mut Window, cx: &mut Context<Self>) {
        let view = self.add_session(project_id, SessionKind::Local { shell: shell.clone() }, &shell.name, window, cx);
        self.start_backend(&view, &SessionKind::Local { shell }, project_id, None, cx);
    }

    fn open_host(&mut self, project_id: &str, host_id: &str, window: &mut Window, cx: &mut Context<Self>) {
        let Some(resolved) = self.library.resolve(project_id, host_id) else { return };
        if resolved.credential.is_some() && self.vault.is_none() {
            self.pending = Some(Pending::Open { project_id: project_id.into(), host_id: host_id.into() });
            self.show_unlock(false, window, cx);
            return;
        }
        let secret = resolved
            .credential
            .as_deref()
            .and_then(|id| self.vault.as_ref()?.get(id))
            .map(|c| c.secret.clone());
        self.open_host_with_secret(project_id, host_id, secret, window, cx);
    }

    fn open_host_with_secret(
        &mut self,
        project_id: &str,
        host_id: &str,
        secret: Option<Secret>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(resolved) = self.library.resolve(project_id, host_id) else { return };
        let kind = SessionKind::Ssh { host_id: host_id.to_string() };
        let view = self.add_session(project_id, kind.clone(), &resolved.name, window, cx);
        self.start_backend(&view, &kind, project_id, secret, cx);
    }

    fn start_backend(
        &mut self,
        view: &Entity<TerminalView>,
        kind: &SessionKind,
        project_id: &str,
        secret: Option<Secret>,
        cx: &mut Context<Self>,
    ) {
        let (term, proxy, size) = {
            let v = view.read(cx);
            (v.term(), v.proxy(), v.grid_size())
        };
        let sink = match kind {
            SessionKind::Local { shell } => match session::local::spawn(shell, size, term, proxy.clone()) {
                Ok(sink) => {
                    proxy.send(session::SessionEvent::Status(Status::Connected));
                    Some(sink)
                }
                Err(err) => {
                    proxy.send(session::SessionEvent::Status(Status::Disconnected(format!("{err:#}"))));
                    None
                }
            },
            SessionKind::Ssh { host_id } => {
                let Some(resolved) = self.library.resolve(project_id, host_id) else { return };
                let target = ssh::Target {
                    address: resolved.address,
                    port: resolved.port,
                    user: resolved.user,
                    secret,
                    known_hosts: self.data_dir.join("known_hosts"),
                };
                Some(ssh::spawn(target, size, term, proxy))
            }
        };
        view.update(cx, |v, cx| {
            if let Some(sink) = sink {
                v.attach(sink);
            }
            v.status = Status::Connecting;
            cx.notify();
        });
    }

    fn reconnect(&mut self, id: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(entry) = self.sessions.iter().find(|s| s.id == id) else { return };
        let (view, kind, project_id) = (entry.view.clone(), entry.kind.clone(), entry.project_id.clone());
        let secret = match &kind {
            SessionKind::Ssh { host_id } => {
                let resolved = self.library.resolve(&project_id, host_id);
                resolved
                    .and_then(|r| r.credential)
                    .and_then(|cred| self.vault.as_ref()?.get(&cred))
                    .map(|c| c.secret.clone())
            }
            SessionKind::Local { .. } => None,
        };
        view.update(cx, |v, _| v.close());
        self.start_backend(&view, &kind, &project_id, secret, cx);
        self.focus_active(window, cx);
    }

    fn activate(&mut self, id: usize, window: &mut Window, cx: &mut Context<Self>) {
        self.active = Some(id);
        self.focus_active(window, cx);
        cx.notify();
    }

    fn focus_active(&self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(entry) = self.active_entry() {
            let handle = entry.view.read(cx).focus_handle(cx);
            window.focus(&handle, cx);
        } else {
            window.focus(&self.focus, cx);
        }
    }

    fn active_entry(&self) -> Option<&SessionEntry> {
        self.sessions.iter().find(|s| Some(s.id) == self.active)
    }

    fn close_session(&mut self, id: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(index) = self.sessions.iter().position(|s| s.id == id) else { return };
        let entry = self.sessions.remove(index);
        entry.view.update(cx, |v, _| v.close());
        if self.active == Some(id) {
            self.active = self
                .sessions
                .get(index.min(self.sessions.len().saturating_sub(1)))
                .map(|s| s.id);
        }
        self.focus_active(window, cx);
        cx.notify();
    }

    fn cycle(&mut self, forward: bool, window: &mut Window, cx: &mut Context<Self>) {
        // Follow sidebar order: Projects top to bottom, Sessions within each.
        let order: Vec<usize> = self
            .library
            .projects
            .iter()
            .flat_map(|p| self.sessions.iter().filter(move |s| s.project_id == p.id).map(|s| s.id))
            .collect();
        if order.is_empty() {
            return;
        }
        let pos = self.active.and_then(|a| order.iter().position(|&id| id == a)).unwrap_or(0);
        let next = if forward { (pos + 1) % order.len() } else { (pos + order.len() - 1) % order.len() };
        self.activate(order[next], window, cx);
    }

    // ---- passwords & output -----------------------------------------------------------

    fn save_password(&mut self, session_id: usize, password: String, as_project: bool, cx: &mut Context<Self>) {
        let Some(entry) = self.sessions.iter().find(|s| s.id == session_id) else { return };
        let SessionKind::Ssh { host_id } = entry.kind.clone() else { return };
        let project_id = entry.project_id.clone();
        let Some(resolved) = self.library.resolve(&project_id, &host_id) else { return };
        let Some(vault) = self.vault.as_mut() else { return };
        let project_name = self.library.project(&project_id).map(|p| p.name.clone()).unwrap_or_default();
        let label = if as_project {
            format!("{project_name} project password")
        } else {
            format!("{}@{}", resolved.user.clone().unwrap_or_default(), resolved.name)
        };
        let credential = Credential { label, secret: Secret::Password { password } };

        // Replace the secret in place when the target already has a Credential, so every
        // Host sharing it picks up the new password.
        let existing = if as_project {
            self.library.project(&project_id).and_then(|p| p.credential.clone())
        } else {
            match resolved.credential_source {
                CredentialSource::Host => resolved.credential.clone(),
                _ => None,
            }
        };
        let result = match existing {
            Some(id) => vault.replace(&id, credential).map(|_| id),
            None => vault.add(credential),
        };
        match result {
            Ok(cred_id) => {
                if as_project {
                    if let Some(p) = self.library.projects.iter_mut().find(|p| p.id == project_id) {
                        p.credential = Some(cred_id);
                    }
                } else if let Some(h) = self.library.hosts.iter_mut().find(|h| h.id == host_id) {
                    h.credential = Some(cred_id);
                }
                let _ = self.library.save(&self.data_dir);
                self.show_toast("Password saved to the vault.", cx);
            }
            Err(err) => self.show_toast(format!("Could not save: {err}"), cx),
        }
    }

    fn copy_output_text(&mut self, cx: &mut Context<Self>) {
        let Some(entry) = self.active_entry() else { return };
        let text = entry.view.read(cx).output_text();
        let lines = text.lines().count();
        cx.write_to_clipboard(gpui::ClipboardItem::new_string(text));
        self.show_toast(format!("Copied {lines} lines as text."), cx);
    }

    fn copy_output_file(&mut self, cx: &mut Context<Self>) {
        let Some(entry) = self.active_entry() else { return };
        let text = entry.view.read(cx).output_text();
        let name = format!(
            "{}_{}.txt",
            sanitize(&entry.label),
            chrono::Local::now().format("%Y-%m-%d_%H-%M")
        );
        match crate::export::write_and_copy_file(&name, &text) {
            Ok(note) => self.show_toast(format!("Copied {name}{note}"), cx),
            Err(err) => self.show_toast(format!("Could not create file: {err:#}"), cx),
        }
        // On non-Windows dev hosts the clipboard gets the path as text instead.
        #[cfg(not(windows))]
        cx.write_to_clipboard(gpui::ClipboardItem::new_string(
            crate::export::export_dir().join(&name).display().to_string(),
        ));
    }

    fn show_toast(&mut self, message: impl Into<SharedString>, cx: &mut Context<Self>) {
        self.toast = Some((message.into(), Instant::now()));
        cx.notify();
    }

    // ---- overlays: picker and forms ----------------------------------------------------

    fn open_picker(&mut self, project_id: Option<String>, window: &mut Window, cx: &mut Context<Self>) {
        let query = cx.new(|cx| InputState::new(window, cx).placeholder("Search hosts and shells…"));
        let sub = cx.subscribe_in(&query, window, |this, _, event: &InputEvent, window, cx| match event {
            InputEvent::Change => cx.notify(),
            InputEvent::PressEnter { .. } => {
                if let Some(first) = this.picker_items(cx).into_iter().next() {
                    this.pick(first, window, cx);
                }
            }
            _ => {}
        });
        self._subscriptions.push(sub);
        query.update(cx, |q, cx| q.focus(window, cx));
        self.overlay = Some(Overlay::Picker { query, project_id });
        cx.notify();
    }

    fn picker_items(&self, cx: &App) -> Vec<PickerItem> {
        let Some(Overlay::Picker { query, project_id }) = &self.overlay else { return vec![] };
        let needle = query.read(cx).value().to_lowercase();
        let mut items = vec![];
        for project in &self.library.projects {
            if project_id.as_ref().is_some_and(|id| id != &project.id) {
                continue;
            }
            for host in self.library.hosts_of(project) {
                let r = model::resolve(host, project);
                let hay = format!("{} {} {} {}", host.name, host.address, project.name, r.user.clone().unwrap_or_default())
                    .to_lowercase();
                if needle.is_empty() || hay.contains(&needle) {
                    items.push(PickerItem::Host { project: project.clone(), host: host.clone() });
                }
            }
            let shells = if project.shells.is_empty() { model::default_shells() } else { project.shells.clone() };
            for shell in shells {
                let hay = format!("{} {} local", shell.name, project.name).to_lowercase();
                if needle.is_empty() || hay.contains(&needle) {
                    items.push(PickerItem::Shell { project: project.clone(), shell });
                }
            }
        }
        items
    }

    fn pick(&mut self, item: PickerItem, window: &mut Window, cx: &mut Context<Self>) {
        self.overlay = None;
        match item {
            PickerItem::Host { project, host } => self.open_host(&project.id, &host.id, window, cx),
            PickerItem::Shell { project, shell } => self.open_local(&project.id, shell, window, cx),
        }
    }

    fn open_new_host(&mut self, project_id: String, window: &mut Window, cx: &mut Context<Self>) {
        let placeholders = ["Name, e.g. web01", "Address, e.g. 10.0.4.21", "User (optional, else project default)", "Port (22)", "Private key file (optional)"];
        let fields: Vec<_> = placeholders
            .iter()
            .map(|ph| cx.new(|cx| InputState::new(window, cx).placeholder(*ph)))
            .collect();
        fields[0].update(cx, |f, cx| f.focus(window, cx));
        self.overlay = Some(Overlay::NewHost { project_id, fields, icon: "server".into(), error: None });
        cx.notify();
    }

    fn submit_new_host(&mut self, cx: &mut Context<Self>) {
        let Some(Overlay::NewHost { project_id, fields, icon, error }) = self.overlay.as_mut() else { return };
        let value = |i: usize| fields[i].read(cx).value().trim().to_string();
        let (name, address, user, port, key_path) = (value(0), value(1), value(2), value(3), value(4));
        if name.is_empty() || address.is_empty() {
            *error = Some("Name and address are required.".into());
            cx.notify();
            return;
        }
        let port = if port.is_empty() { 22 } else {
            match port.parse() {
                Ok(p) => p,
                Err(_) => {
                    *error = Some("The port must be a number.".into());
                    cx.notify();
                    return;
                }
            }
        };
        let mut credential = None;
        if !key_path.is_empty() {
            let Some(vault) = self.vault.as_mut() else {
                *error = Some("Unlock the vault to import a key.".into());
                cx.notify();
                return;
            };
            match std::fs::read_to_string(&key_path) {
                Ok(key) => match vault.add(Credential {
                    label: format!("{name} key"),
                    secret: Secret::PrivateKey { key, passphrase: None },
                }) {
                    Ok(id) => credential = Some(id),
                    Err(err) => {
                        *error = Some(format!("Could not store the key: {err}"));
                        cx.notify();
                        return;
                    }
                },
                Err(err) => {
                    *error = Some(format!("Could not read {key_path}: {err}"));
                    cx.notify();
                    return;
                }
            }
        }
        let host = Host {
            id: model::new_id(),
            name,
            address,
            port,
            user: (!user.is_empty()).then_some(user),
            credential,
            color: None,
            icon: Some(icon.clone()),
        };
        let project_id = project_id.clone();
        if let Some(project) = self.library.projects.iter_mut().find(|p| p.id == project_id) {
            project.hosts.push(host.id.clone());
        }
        self.library.hosts.push(host);
        let _ = self.library.save(&self.data_dir);
        self.overlay = None;
        cx.notify();
    }

    fn open_new_project(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let name = cx.new(|cx| InputState::new(window, cx).placeholder("Project name, e.g. Customer A – Prod"));
        name.update(cx, |f, cx| f.focus(window, cx));
        self.overlay = Some(Overlay::NewProject { name, color: "blue".into(), icon: "folder".into(), error: None });
        cx.notify();
    }

    fn submit_new_project(&mut self, cx: &mut Context<Self>) {
        let Some(Overlay::NewProject { name, color, icon, error }) = self.overlay.as_mut() else { return };
        let name = name.read(cx).value().trim().to_string();
        if name.is_empty() {
            *error = Some("Give the project a name.".into());
            cx.notify();
            return;
        }
        self.library.projects.push(Project {
            id: model::new_id(),
            name,
            color: color.clone(),
            icon: icon.clone(),
            default_user: None,
            credential: None,
            hosts: vec![],
            shells: vec![],
        });
        let _ = self.library.save(&self.data_dir);
        self.overlay = None;
        cx.notify();
    }

    fn close_overlay(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.overlay = None;
        self.focus_active(window, cx);
        cx.notify();
    }
}

#[derive(Clone)]
enum PickerItem {
    Host { project: Project, host: Host },
    Shell { project: Project, shell: LocalShell },
}

fn sanitize(s: &str) -> String {
    s.chars().map(|c| if c.is_alphanumeric() || c == '-' || c == '.' { c } else { '_' }).collect()
}

// ---- rendering -------------------------------------------------------------------------

impl Workspace {
    fn render_unlock(&self, state: &UnlockState, p: AppPalette, cx: &mut Context<Self>) -> AnyElement {
        let form = div()
            .w(px(360.))
            .flex()
            .flex_col()
            .gap(px(12.))
            .child(Input::new(&state.password))
            .when(state.creating, |el| el.child(Input::new(&state.confirm)))
            .when_some(state.error.clone(), |el, e| el.child(div().text_size(px(12.)).text_color(p.danger).child(e)))
            .child(
                button("unlock", if state.creating { "Create vault" } else { "Unlock" }, ButtonKind::Primary, p)
                    .w_full()
                    .h(px(36.))
                    .on_click(cx.listener(|this, _, window, cx| this.submit_unlock(window, cx))),
            )
            .child(
                div()
                    .flex()
                    .justify_center()
                    .child(
                        button("skip", "Continue without unlocking", ButtonKind::Ghost, p)
                            .on_click(cx.listener(|this, _, window, cx| this.skip_unlock(window, cx))),
                    ),
            );
        let note = if state.creating {
            "Your passwords and keys are encrypted with this master password. There is no recovery: if you forget it, saved passwords are lost. Projects and hosts are kept."
        } else {
            "Unlock to connect with saved passwords and keys."
        };
        if state.full_screen {
            div()
                .size_full()
                .flex()
                .flex_col()
                .items_center()
                .justify_center()
                .gap(px(28.))
                .bg(p.background)
                .child(wordmark(64., p.text))
                .child(div().w(px(56.)).h(px(3.)).rounded_full().bg(p.brand))
                .child(
                    div()
                        .w(px(360.))
                        .text_center()
                        .text_size(px(13.))
                        .line_height(px(20.))
                        .text_color(p.text_muted)
                        .child(note),
                )
                .child(form)
                .into_any_element()
        } else {
            modal(
                p,
                div()
                    .w(px(400.))
                    .p(px(24.))
                    .flex()
                    .flex_col()
                    .gap(px(16.))
                    .child(div().text_size(px(15.)).font_weight(FontWeight::SEMIBOLD).text_color(p.text).child("Unlock vault"))
                    .child(div().text_size(px(13.)).text_color(p.text_muted).child(note))
                    .child(form)
                    .into_any_element(),
            )
        }
    }

    fn render_sidebar(&self, p: AppPalette, cx: &mut Context<Self>) -> AnyElement {
        let mut list = div().id("projects").flex().flex_col().gap(px(2.)).px(px(8.)).flex_1().overflow_y_scroll();
        for project in &self.library.projects {
            let color = project_color(&project.color);
            let collapsed = self.collapsed.contains(&project.id);
            let sessions: Vec<&SessionEntry> = self.sessions.iter().filter(|s| s.project_id == project.id).collect();
            let pid = project.id.clone();
            let pid_add = project.id.clone();
            let pid_host = project.id.clone();
            list = list.child(
                div()
                    .id(SharedString::from(format!("project-{}", project.id)))
                    .group("project-row")
                    .relative()
                    .h(px(36.))
                    .px(px(8.))
                    .mt(px(4.))
                    .flex()
                    .items_center()
                    .gap(px(10.))
                    .rounded(px(8.))
                    .cursor_pointer()
                    .hover(|s| s.bg(p.hover))
                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                        if !this.collapsed.remove(&pid) {
                            this.collapsed.insert(pid.clone());
                        }
                        cx.notify();
                    }))
                    .child(badge(&project.icon, color, 22.))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_size(px(13.))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(p.text)
                            .child(project.name.clone()),
                    )
                    .child(
                        div()
                            .absolute()
                            .right(px(4.))
                            .top(px(3.))
                            .flex()
                            .gap(px(2.))
                            .p(px(0.))
                            .rounded(px(7.))
                            .bg(p.sidebar)
                            .invisible()
                            .group_hover("project-row", |s| s.visible())
                            .child(icon_button(SharedString::from(format!("add-host-{}", project.id)), "server-cog", p)
                                .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                                    cx.stop_propagation();
                                    this.open_new_host(pid_host.clone(), window, cx);
                                })))
                            .child(icon_button(SharedString::from(format!("new-{}", project.id)), "plus", p)
                                .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                                    cx.stop_propagation();
                                    this.open_picker(Some(pid_add.clone()), window, cx);
                                }))),
                    )
                    .child(icon_el(if collapsed { "chevron-right" } else { "chevron-down" }, 14., p.text_faint)),
            );
            if collapsed {
                continue;
            }
            for entry in sessions {
                let active = self.active == Some(entry.id);
                let dot = match &entry.view.read(cx).status {
                    Status::Connected => p.success,
                    Status::Connecting => p.warning,
                    Status::Disconnected(_) => p.text_faint,
                };
                let kind_icon = match &entry.kind {
                    SessionKind::Ssh { host_id } => self
                        .library
                        .host(host_id)
                        .and_then(|h| h.icon.clone())
                        .unwrap_or_else(|| "server".into()),
                    SessionKind::Local { .. } => "square-terminal".into(),
                };
                let id = entry.id;
                list = list.child(
                    div()
                        .id(SharedString::from(format!("session-{}", entry.id)))
                        .group("session-row")
                        .h(px(34.))
                        .pl(px(20.))
                        .pr(px(6.))
                        .flex()
                        .items_center()
                        .gap(px(10.))
                        .rounded(px(8.))
                        .cursor_pointer()
                        .when(active, |el| el.bg(p.active))
                        .when(!active, |el| el.hover(|s| s.bg(p.hover)))
                        .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| this.activate(id, window, cx)))
                        .child(icon_el(&kind_icon, 15., if active { color } else { p.text_faint }))
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .truncate()
                                .text_size(px(13.))
                                .text_color(if active { p.text } else { p.text_muted })
                                .child(entry.label.clone()),
                        )
                        .child(status_dot(dot))
                        .child(
                            icon_button(SharedString::from(format!("close-{id}")), "x", p)
                                .size(px(24.))
                                .invisible()
                                .group_hover("session-row", |s| s.visible())
                                .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                                    cx.stop_propagation();
                                    this.close_session(id, window, cx);
                                })),
                        ),
                );
            }
            let pid_new = project.id.clone();
            list = list.child(
                div()
                    .id(SharedString::from(format!("new-session-{}", project.id)))
                    .h(px(32.))
                    .pl(px(20.))
                    .flex()
                    .items_center()
                    .gap(px(10.))
                    .rounded(px(8.))
                    .cursor_pointer()
                    .text_size(px(12.5))
                    .text_color(p.text_faint)
                    .hover(|s| s.bg(p.hover).text_color(p.text_muted))
                    .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| this.open_picker(Some(pid_new.clone()), window, cx)))
                    .child(icon_el("plus", 14., p.text_faint))
                    .child("New session"),
            );
        }

        let app_theme = match self.settings.app_theme {
            AppThemeSetting::System => ("monitor", "App: system"),
            AppThemeSetting::Light => ("sun", "App: light"),
            AppThemeSetting::Dark => ("moon", "App: dark"),
        };
        let term_theme = match self.settings.terminal_theme {
            TerminalThemeSetting::Follow => ("square-terminal", "Terminal: follow app"),
            TerminalThemeSetting::Light => ("sun", "Terminal: light"),
            TerminalThemeSetting::Dark => ("moon", "Terminal: dark"),
        };
        let footer_row = |id: &'static str, icon: &str, label: String| {
            div()
                .id(id)
                .h(px(32.))
                .px(px(8.))
                .flex()
                .items_center()
                .gap(px(10.))
                .rounded(px(8.))
                .cursor_pointer()
                .text_size(px(12.5))
                .text_color(p.text_muted)
                .hover(|s| s.bg(p.hover).text_color(p.text))
                .child(icon_el(icon, 15., p.text_faint))
                .child(label)
        };
        let vault_row = if self.vault.is_some() {
            footer_row("vault", "lock-open", "Vault unlocked".into())
                .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.lock_vault(cx)))
        } else {
            footer_row("vault", "lock", "Vault locked · Unlock".into())
                .on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.show_unlock(false, window, cx)))
        };

        div()
            .w(px(SIDEBAR_WIDTH))
            .h_full()
            .flex()
            .flex_col()
            .flex_shrink_0()
            .bg(p.sidebar)
            .border_r_1()
            .border_color(p.border)
            .child(
                div()
                    .h(px(64.))
                    .px(px(18.))
                    .flex()
                    .items_center()
                    .justify_between()
                    .child(wordmark(17., p.text))
                    .child(div().size(px(8.)).rounded(px(2.)).bg(p.brand)),
            )
            .child(
                div()
                    .px(px(8.))
                    .pb(px(10.))
                    .child(
                        div()
                            .id("quick-connect")
                            .h(px(34.))
                            .px(px(10.))
                            .flex()
                            .items_center()
                            .gap(px(8.))
                            .rounded(px(8.))
                            .bg(p.hover)
                            .border_1()
                            .border_color(p.border)
                            .cursor_pointer()
                            .text_size(px(12.5))
                            .text_color(p.text_faint)
                            .hover(|s| s.bg(p.active))
                            .on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.open_picker(None, window, cx)))
                            .child(icon_el("search", 14., p.text_faint))
                            .child(div().flex_1().child("Quick connect"))
                            .child(kbd("Ctrl Shift K", p)),
                    ),
            )
            .child(
                div()
                    .px(px(16.))
                    .pt(px(6.))
                    .pb(px(4.))
                    .flex()
                    .items_center()
                    .justify_between()
                    .child(section_label("Projects", p))
                    .child(
                        icon_button("new-project", "folder-plus", p)
                            .size(px(24.))
                            .on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.open_new_project(window, cx))),
                    ),
            )
            .child(list)
            .child(
                div()
                    .p(px(8.))
                    .border_t_1()
                    .border_color(p.border)
                    .flex()
                    .flex_col()
                    .gap(px(2.))
                    .child(vault_row)
                    .child(
                        footer_row("app-theme", app_theme.0, app_theme.1.into())
                            .on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.cycle_app_theme(window, cx))),
                    )
                    .child(
                        footer_row("term-theme", term_theme.0, term_theme.1.into())
                            .on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.cycle_terminal_theme(window, cx))),
                    ),
            )
            .into_any_element()
    }

    fn render_main(&self, p: AppPalette, cx: &mut Context<Self>) -> AnyElement {
        let Some(entry) = self.active_entry() else {
            return self.render_empty(p, cx);
        };
        let project = self.library.project(&entry.project_id);
        let color = project.map(|pr| project_color(&pr.color)).unwrap_or(p.text_faint);
        let view = entry.view.read(cx);
        let (subtitle, icon) = match &entry.kind {
            SessionKind::Ssh { host_id } => {
                let r = self.library.resolve(&entry.project_id, host_id);
                let sub = r
                    .as_ref()
                    .map(|r| {
                        let user = r.user.clone().map(|u| format!("{u}@")).unwrap_or_default();
                        let port = if r.port == 22 { String::new() } else { format!(":{}", r.port) };
                        format!("{user}{}{port}", r.address)
                    })
                    .unwrap_or_default();
                (sub, r.map(|r| r.icon).unwrap_or_else(|| "server".into()))
            }
            SessionKind::Local { shell } => (format!("Local · {}", shell.program), "square-terminal".to_string()),
        };
        let (status_text, status_color) = match &view.status {
            Status::Connecting => ("Connecting", p.warning),
            Status::Connected => ("Connected", p.success),
            Status::Disconnected(_) => ("Disconnected", p.text_faint),
        };
        let project_name = project.map(|pr| pr.name.clone()).unwrap_or_default();
        let id = entry.id;
        div()
            .flex_1()
            .min_w_0()
            .h_full()
            .flex()
            .flex_col()
            .child(
                div()
                    .h(px(52.))
                    .flex_shrink_0()
                    .px(px(16.))
                    .flex()
                    .items_center()
                    .gap(px(12.))
                    .bg(p.background)
                    .border_b_1()
                    .border_color(p.border)
                    .child(badge(&icon, color, 28.))
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .min_w_0()
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap(px(8.))
                                    .child(div().text_size(px(13.5)).font_weight(FontWeight::SEMIBOLD).text_color(p.text).truncate().child(entry.label.clone()))
                                    .child(
                                        div()
                                            .flex()
                                            .items_center()
                                            .gap(px(6.))
                                            .px(px(8.))
                                            .h(px(20.))
                                            .rounded_full()
                                            .bg(status_color.opacity(0.12))
                                            .text_size(px(11.))
                                            .font_weight(FontWeight::MEDIUM)
                                            .text_color(status_color)
                                            .child(status_dot(status_color))
                                            .child(status_text),
                                    ),
                            )
                            .child(
                                div()
                                    .text_size(px(12.))
                                    .text_color(p.text_faint)
                                    .truncate()
                                    .child(format!("{project_name} · {subtitle}")),
                            ),
                    )
                    .child(div().flex_1())
                    .child(
                        header_action("copy-text", "copy", "Copy output", p)
                            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.copy_output_text(cx))),
                    )
                    .child(
                        header_action("copy-file", "file-text", "Copy as .txt", p)
                            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.copy_output_file(cx))),
                    )
                    .child(
                        icon_button("close-active", "x", p)
                            .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| this.close_session(id, window, cx))),
                    ),
            )
            // Project color strip: you always see which Project you are typing into.
            .child(div().h(px(2.)).flex_shrink_0().bg(color))
            .child(div().flex_1().min_h_0().child(entry.view.clone()))
            .into_any_element()
    }

    fn render_empty(&self, p: AppPalette, cx: &mut Context<Self>) -> AnyElement {
        div()
            .flex_1()
            .h_full()
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap(px(18.))
            .bg(p.background)
            .child(wordmark(40., p.text.opacity(0.12)))
            .child(div().text_size(px(13.)).text_color(p.text_muted).child("No open sessions."))
            .child(
                div()
                    .flex()
                    .gap(px(8.))
                    .child(
                        button("empty-connect", "Quick connect", ButtonKind::Primary, p)
                            .on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.open_picker(None, window, cx))),
                    )
                    .child(
                        button("empty-project", "New project", ButtonKind::Secondary, p)
                            .on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.open_new_project(window, cx))),
                    ),
            )
            .into_any_element()
    }

    fn render_overlay(&self, p: AppPalette, cx: &mut Context<Self>) -> Option<AnyElement> {
        let overlay = self.overlay.as_ref()?;
        let body = match overlay {
            Overlay::Picker { query, project_id } => {
                let items = self.picker_items(cx);
                let title = project_id
                    .as_ref()
                    .and_then(|id| self.library.project(id))
                    .map(|pr| format!("New session in {}", pr.name))
                    .unwrap_or_else(|| "Quick connect".into());
                let mut list = div().id("picker-list").flex().flex_col().gap(px(2.)).max_h(px(380.)).overflow_y_scroll().p(px(6.));
                if items.is_empty() {
                    list = list.child(
                        div().p(px(16.)).text_size(px(13.)).text_color(p.text_faint).child("Nothing matches. Add hosts with the server icon next to a project."),
                    );
                }
                for (i, item) in items.into_iter().enumerate() {
                    let (icon, color, name, detail) = match &item {
                        PickerItem::Host { project, host } => {
                            let r = model::resolve(host, project);
                            let user = r.user.clone().map(|u| format!("{u}@")).unwrap_or_default();
                            (r.icon.clone(), project_color(&r.color), host.name.clone(), format!("{user}{} · {}", host.address, project.name))
                        }
                        PickerItem::Shell { project, shell } => (
                            "square-terminal".to_string(),
                            project_color(&project.color),
                            shell.name.clone(),
                            format!("Local shell · {}", project.name),
                        ),
                    };
                    list = list.child(
                        div()
                            .id(("pick", i))
                            .h(px(44.))
                            .px(px(10.))
                            .flex()
                            .items_center()
                            .gap(px(12.))
                            .rounded(px(8.))
                            .cursor_pointer()
                            .when(i == 0, |el| el.bg(p.hover))
                            .hover(|s| s.bg(p.active))
                            .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| this.pick(item.clone(), window, cx)))
                            .child(badge(&icon, color, 26.))
                            .child(
                                div()
                                    .flex()
                                    .flex_col()
                                    .min_w_0()
                                    .child(div().text_size(px(13.)).font_weight(FontWeight::MEDIUM).text_color(p.text).child(name))
                                    .child(div().text_size(px(12.)).text_color(p.text_faint).truncate().child(detail)),
                            ),
                    );
                }
                div()
                    .w(px(520.))
                    .flex()
                    .flex_col()
                    .child(
                        div()
                            .p(px(14.))
                            .flex()
                            .flex_col()
                            .gap(px(10.))
                            .border_b_1()
                            .border_color(p.border)
                            .child(section_label(&title, p))
                            .child(Input::new(query)),
                    )
                    .child(list)
                    .into_any_element()
            }
            Overlay::NewHost { project_id, fields, icon, error } => {
                let project_name = self.library.project(project_id).map(|pr| pr.name.clone()).unwrap_or_default();
                let color = self.library.project(project_id).map(|pr| project_color(&pr.color)).unwrap_or(p.brand);
                let selected = icon.clone();
                form_card(
                    p,
                    &format!("Add host to {project_name}"),
                    div()
                        .flex()
                        .flex_col()
                        .gap(px(10.))
                        .children(fields.iter().map(Input::new))
                        .child(section_label("Icon", p))
                        .child(icon_choices(&selected, color, p, cx, |this, name| {
                            if let Some(Overlay::NewHost { icon, .. }) = this.overlay.as_mut() {
                                *icon = name;
                            }
                        }))
                        .child(div().text_size(px(12.)).text_color(p.text_faint).child(
                            "Password hosts: connect once and choose \"Save\". Keys are imported into the vault.",
                        ))
                        .when_some(error.clone(), |el, e| el.child(div().text_size(px(12.)).text_color(p.danger).child(e)))
                        .into_any_element(),
                    vec![
                        button("nh-cancel", "Cancel", ButtonKind::Ghost, p)
                            .on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.close_overlay(window, cx)))
                            .into_any_element(),
                        button("nh-ok", "Add host", ButtonKind::Primary, p)
                            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.submit_new_host(cx)))
                            .into_any_element(),
                    ],
                )
            }
            Overlay::NewProject { name, color, icon, error } => {
                let current = color.clone();
                let selected_icon = icon.clone();
                let swatches = div().flex().flex_wrap().gap(px(8.)).children(PROJECT_COLORS.iter().map(|(cname, _)| {
                    let cname = cname.to_string();
                    let selected = cname == current;
                    let c = project_color(&cname);
                    div()
                        .id(SharedString::from(format!("swatch-{cname}")))
                        .size(px(26.))
                        .rounded_full()
                        .p(px(3.))
                        .cursor_pointer()
                        .border_2()
                        .border_color(if selected { c } else { gpui::transparent_black() })
                        .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                            if let Some(Overlay::NewProject { color, .. }) = this.overlay.as_mut() {
                                *color = cname.clone();
                            }
                            cx.notify();
                        }))
                        .child(div().size_full().rounded_full().bg(c))
                }));
                form_card(
                    p,
                    "New project",
                    div()
                        .flex()
                        .flex_col()
                        .gap(px(12.))
                        .child(Input::new(name))
                        .child(section_label("Color", p))
                        .child(swatches)
                        .child(section_label("Icon", p))
                        .child(icon_choices(&selected_icon, project_color(&current), p, cx, |this, name| {
                            if let Some(Overlay::NewProject { icon, .. }) = this.overlay.as_mut() {
                                *icon = name;
                            }
                        }))
                        .when_some(error.clone(), |el, e| el.child(div().text_size(px(12.)).text_color(p.danger).child(e)))
                        .into_any_element(),
                    vec![
                        button("np-cancel", "Cancel", ButtonKind::Ghost, p)
                            .on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.close_overlay(window, cx)))
                            .into_any_element(),
                        button("np-ok", "Create project", ButtonKind::Primary, p)
                            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.submit_new_project(cx)))
                            .into_any_element(),
                    ],
                )
            }
        };
        Some(
            div()
                .id("overlay-backdrop")
                .absolute()
                .inset_0()
                .flex()
                .justify_center()
                .pt(px(96.))
                .bg(alpha(0x000000, if p.dark { 0.5 } else { 0.2 }))
                .on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.close_overlay(window, cx)))
                .child(
                    div()
                        .id("overlay-card")
                        .h_auto()
                        .max_h(px(560.))
                        .rounded(px(14.))
                        .bg(p.surface)
                        .border_1()
                        .border_color(p.border)
                        .shadow(elevation(p.dark))
                        .overflow_hidden()
                        .on_click(|_, _, cx| cx.stop_propagation())
                        .child(body),
                )
                .into_any_element(),
        )
    }
}

fn icon_choices(
    selected: &str,
    color: Hsla,
    p: AppPalette,
    cx: &mut Context<Workspace>,
    set: impl Fn(&mut Workspace, String) + Clone + 'static,
) -> impl IntoElement {
    let icons: Vec<&str> = std::iter::once("folder").chain(HOST_ICONS).collect();
    div().flex().flex_wrap().gap(px(6.)).children(icons.into_iter().map(|name| {
        let is_selected = name == selected;
        let set = set.clone();
        let owned = name.to_string();
        div()
            .id(SharedString::from(format!("icon-{name}")))
            .size(px(34.))
            .flex()
            .items_center()
            .justify_center()
            .rounded(px(8.))
            .cursor_pointer()
            .border_1()
            .border_color(if is_selected { color } else { p.border })
            .bg(if is_selected { color.opacity(0.14) } else { gpui::transparent_black() })
            .hover(|s| s.bg(p.hover))
            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                set(this, owned.clone());
                cx.notify();
            }))
            .child(icon_el(name, 16., if is_selected { color } else { p.text_muted }))
    }))
}

fn form_card(p: AppPalette, title: &str, body: AnyElement, actions: Vec<AnyElement>) -> AnyElement {
    div()
        .w(px(440.))
        .p(px(22.))
        .flex()
        .flex_col()
        .gap(px(16.))
        .child(div().text_size(px(15.)).font_weight(FontWeight::SEMIBOLD).text_color(p.text).child(title.to_string()))
        .child(body)
        .child(div().flex().justify_end().gap(px(8.)).children(actions))
        .into_any_element()
}

fn modal(p: AppPalette, body: AnyElement) -> AnyElement {
    div()
        .absolute()
        .inset_0()
        .flex()
        .items_center()
        .justify_center()
        .bg(alpha(0x000000, if p.dark { 0.5 } else { 0.2 }))
        .child(
            div()
                .rounded(px(14.))
                .bg(p.surface)
                .border_1()
                .border_color(p.border)
                .shadow(elevation(p.dark))
                .child(body),
        )
        .into_any_element()
}

fn header_action(id: &'static str, icon: &str, label: &'static str, p: AppPalette) -> gpui::Stateful<gpui::Div> {
    div()
        .id(id)
        .h(px(30.))
        .px(px(10.))
        .flex()
        .items_center()
        .gap(px(7.))
        .rounded(px(7.))
        .cursor_pointer()
        .text_size(px(12.5))
        .font_weight(FontWeight::MEDIUM)
        .text_color(p.text_muted)
        .hover(|s| s.bg(p.hover).text_color(p.text))
        .child(icon_el(icon, 14., p.text_muted))
        .child(label)
}

fn kbd(keys: &str, p: AppPalette) -> impl IntoElement {
    div()
        .px(px(5.))
        .h(px(18.))
        .flex()
        .items_center()
        .rounded(px(4.))
        .border_1()
        .border_color(p.border)
        .text_size(px(10.5))
        .text_color(p.text_faint)
        .child(keys.to_string())
}

impl Render for Workspace {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = cx.global::<Appearance>().app;
        let root = div()
            .id("workspace")
            .key_context("Workspace")
            .track_focus(&self.focus)
            .size_full()
            .relative()
            .flex()
            .font_family(UI_FONT)
            .bg(p.background)
            .text_color(p.text)
            .capture_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                this.last_activity = Instant::now();
                if event.keystroke.key == "escape" && this.overlay.is_some() {
                    this.close_overlay(window, cx);
                    cx.stop_propagation();
                }
            }))
            .on_action(cx.listener(|this, _: &QuickConnect, window, cx| this.open_picker(None, window, cx)))
            .on_action(cx.listener(|this, _: &NewSession, window, cx| {
                let project = this.active_entry().map(|e| e.project_id.clone());
                this.open_picker(project, window, cx)
            }))
            .on_action(cx.listener(|this, _: &CloseSession, window, cx| {
                if let Some(id) = this.active {
                    this.close_session(id, window, cx);
                }
            }))
            .on_action(cx.listener(|this, _: &NextSession, window, cx| this.cycle(true, window, cx)))
            .on_action(cx.listener(|this, _: &PrevSession, window, cx| this.cycle(false, window, cx)))
            .on_action(cx.listener(|this, _: &CopyOutputText, _, cx| this.copy_output_text(cx)))
            .on_action(cx.listener(|this, _: &CopyOutputFile, _, cx| this.copy_output_file(cx)))
            .on_action(cx.listener(|this, _: &LockVault, _, cx| this.lock_vault(cx)));

        if let Some(state) = self.unlock.as_ref().filter(|s| s.full_screen) {
            return root.child(self.render_unlock(state, p, cx)).into_any_element();
        }

        let toast = self.toast.as_ref().map(|(msg, _)| {
            div()
                .absolute()
                .bottom(px(20.))
                .right(px(20.))
                .px(px(14.))
                .py(px(10.))
                .rounded(px(10.))
                .bg(p.surface)
                .border_1()
                .border_color(p.border)
                .shadow(elevation(p.dark))
                .text_size(px(13.))
                .text_color(p.text)
                .child(msg.clone())
        });
        let _ = window;
        root.child(self.render_sidebar(p, cx))
            .child(self.render_main(p, cx))
            .children(self.render_overlay(p, cx))
            .children(self.unlock.as_ref().map(|s| self.render_unlock(s, p, cx)))
            .children(toast)
            .into_any_element()
    }
}
