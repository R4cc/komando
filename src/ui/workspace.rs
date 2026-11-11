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
use gpui_component::slider::{Slider, SliderEvent, SliderState};
use gpui_component::tooltip::Tooltip;

use crate::highlight::{self, ActiveHighlights, HighlightSet, Highlighter};
use crate::model::{
    self, AppThemeSetting, CredentialSource, Host, Library, LocalShell, Project, Settings, TerminalThemeSetting,
};
use crate::session::{self, Status, ssh};
use crate::theme::{AppPalette, Appearance, PROJECT_COLORS, TerminalPalette, UI_FONT, alpha, project_color};
use crate::ui::terminal_view::{TerminalEvent, TerminalView};
use crate::ui::widgets::{
    ButtonKind, ISLAND_GAP, ISLAND_RADIUS, badge, button, elevation, icon_button, icon_el, island, section_label, status_dot,
    wordmark,
};
use crate::vault::{Credential, Secret, Vault};

mod settings;
pub use settings::SettingsSection;

actions!(
    komando,
    [QuickConnect, NewSession, CloseSession, NextSession, PrevSession, CopyOutputText, CopyOutputFile, LockVault, OpenSettings]
);

/// What fills the window: the sessions, or the full-window Settings page.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Page {
    Sessions,
    Settings(SettingsSection),
}

const SIDEBAR_WIDTH: f32 = 264.;
const HOST_ICONS: [&str; 10] =
    ["server", "database", "globe", "shield", "hard-drive", "cpu", "network", "cloud", "container", "router"];

#[derive(Clone)]
enum SessionKind {
    Ssh { host_id: String },
    Local { shell: LocalShell },
}

/// Identifies a sidebar item: a saved Host in a Project, or a Local Shell (in a Project or
/// standalone). Sessions belong to items; an item without a Session is just idle.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
enum ItemKey {
    Host { project: String, host: String },
    Shell { project: Option<String>, name: String },
}

/// A row in the sidebar.
#[derive(Clone)]
enum SidebarItem {
    Host { project: Project, host: Host },
    Shell { project: Option<Project>, shell: LocalShell },
}

impl SidebarItem {
    fn key(&self) -> ItemKey {
        match self {
            Self::Host { project, host } => ItemKey::Host { project: project.id.clone(), host: host.id.clone() },
            Self::Shell { project, shell } => {
                ItemKey::Shell { project: project.as_ref().map(|p| p.id.clone()), name: shell.name.clone() }
            }
        }
    }

    fn label(&self) -> &str {
        match self {
            Self::Host { host, .. } => &host.name,
            Self::Shell { shell, .. } => &shell.name,
        }
    }

    fn icon(&self) -> String {
        match self {
            Self::Host { host, .. } => host.icon.clone().unwrap_or_else(|| "server".into()),
            Self::Shell { .. } => "square-terminal".into(),
        }
    }
}

struct SessionEntry {
    id: usize,
    item: ItemKey,
    /// `None` for a standalone Local Shell.
    project_id: Option<String>,
    kind: SessionKind,
    /// "web01" or "web01 (2)" when the same Host is open twice.
    label: SharedString,
    view: Entity<TerminalView>,
    _subscription: Subscription,
}

enum Overlay {
    Picker { query: Entity<InputState> },
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
    /// Open the Settings page at this section (used for screenshots).
    pub settings: Option<SettingsSection>,
}

pub struct Workspace {
    data_dir: PathBuf,
    library: Library,
    settings: Settings,
    highlights: Vec<HighlightSet>,
    /// Rules that could not be compiled, with the reason.
    highlight_skipped: Vec<(String, String)>,
    page: Page,
    highlights_expanded: Option<String>,
    blur_slider: Option<Entity<SliderState>>,
    font_slider: Option<Entity<SliderState>>,
    font_input: Option<Entity<InputState>>,
    vault: Option<Vault>,
    /// Shown full-window at launch; as a dialog when a locked Vault is needed later.
    unlock: Option<UnlockState>,
    pending: Option<Pending>,
    sessions: Vec<SessionEntry>,
    active: Option<usize>,
    /// The Session last shown for each item, so a click returns to it.
    last_active: std::collections::HashMap<ItemKey, usize>,
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
        // A fresh install has no Projects; the detected local shells stand alone.
        let library = Library::load(&data_dir).unwrap_or_else(|err| {
            log::error!("{err:#}");
            Library::default()
        });
        let highlights = highlight::load(&data_dir);
        let mut this = Self {
            data_dir,
            library,
            settings,
            highlights,
            highlight_skipped: vec![],
            page: Page::Sessions,
            highlights_expanded: None,
            blur_slider: None,
            font_slider: None,
            font_input: None,
            vault: None,
            unlock: None,
            pending: None,
            sessions: vec![],
            active: None,
            last_active: Default::default(),
            next_id: 1,
            collapsed: HashSet::new(),
            overlay: None,
            toast: None,
            last_activity: Instant::now(),
            focus: cx.focus_handle(),
            _subscriptions: vec![],
        };
        this.apply_appearance(window, cx);
        this.apply_highlights(cx);
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
        // An empty project name means a standalone Local Shell.
        for (project_name, name) in &startup.open {
            let item = self
                .sidebar_items()
                .into_iter()
                .find(|item| {
                    let project = match item {
                        SidebarItem::Host { project, .. } => Some(project.name.as_str()),
                        SidebarItem::Shell { project, .. } => project.as_ref().map(|p| p.name.as_str()),
                    };
                    project.unwrap_or("") == project_name && item.label() == name
                });
            if let Some(item) = item {
                self.activate_item(item, false, window, cx);
            }
        }
        if let Some(index) = startup.active
            && let Some(id) = self.sessions.get(index).map(|s| s.id)
        {
            self.activate(id, window, cx);
        }
        if let Some(section) = startup.settings {
            self.highlights_expanded = self.highlights.first().map(|s| s.id.clone());
            self.open_settings(section, window, cx);
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
        // Mica: Windows blurs the desktop behind the window; our surfaces become partly
        // transparent so it shows through. The slider sets how much.
        let blur = crate::platform::SUPPORTS_BACKDROP && self.settings.background_blur;
        let t = if blur { self.settings.background_blur_intensity as f32 / 100. } else { 0. };
        window.set_background_appearance(if blur {
            gpui::WindowBackgroundAppearance::MicaBackdrop
        } else {
            gpui::WindowBackgroundAppearance::Opaque
        });
        crate::platform::set_window_dark_mode(window, app_dark);
        cx.set_global(Appearance {
            app: (if app_dark { AppPalette::dark() } else { AppPalette::light() }).translucent(t),
            term: (if term_dark { TerminalPalette::dark() } else { TerminalPalette::light() }).translucent(t),
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

    fn set_app_theme(&mut self, theme: AppThemeSetting, window: &mut Window, cx: &mut Context<Self>) {
        self.settings.app_theme = theme;
        let _ = self.settings.save(&self.data_dir);
        self.apply_appearance(window, cx);
    }

    fn set_terminal_theme(&mut self, theme: TerminalThemeSetting, window: &mut Window, cx: &mut Context<Self>) {
        self.settings.terminal_theme = theme;
        let _ = self.settings.save(&self.data_dir);
        self.apply_appearance(window, cx);
    }

    fn set_background_blur(&mut self, on: bool, window: &mut Window, cx: &mut Context<Self>) {
        self.settings.background_blur = on;
        let _ = self.settings.save(&self.data_dir);
        self.apply_appearance(window, cx);
    }

    // ---- highlights -------------------------------------------------------------------

    fn apply_highlights(&mut self, cx: &mut Context<Self>) {
        let compiled = Highlighter::from_sets(&self.highlights, &self.settings.disabled_highlight_sets);
        self.highlight_skipped = compiled.skipped.clone();
        cx.set_global(ActiveHighlights(std::sync::Arc::new(compiled)));
        cx.refresh_windows();
    }

    fn save_highlights(&mut self, cx: &mut Context<Self>) {
        if let Err(err) = highlight::save(&self.data_dir, &self.highlights) {
            self.show_toast(format!("Could not save highlights: {err:#}"), cx);
        }
        self.apply_highlights(cx);
    }

    fn active_rule_count(&self) -> usize {
        self.highlights
            .iter()
            .filter(|s| !self.settings.disabled_highlight_sets.contains(&s.id))
            .flat_map(|s| s.keywords.iter())
            .filter(|k| k.enabled)
            .count()
    }

    fn toggle_highlight_set(&mut self, id: &str, cx: &mut Context<Self>) {
        let disabled = &mut self.settings.disabled_highlight_sets;
        if let Some(pos) = disabled.iter().position(|d| d == id) {
            disabled.remove(pos);
        } else {
            disabled.push(id.to_string());
        }
        let _ = self.settings.save(&self.data_dir);
        self.apply_highlights(cx);
        cx.notify();
    }

    fn toggle_highlight_rule(&mut self, set_id: &str, index: usize, cx: &mut Context<Self>) {
        if let Some(rule) = self.highlights.iter_mut().find(|s| s.id == set_id).and_then(|s| s.keywords.get_mut(index)) {
            rule.enabled = !rule.enabled;
        }
        self.save_highlights(cx);
        cx.notify();
    }

    fn delete_highlight_set(&mut self, id: &str, cx: &mut Context<Self>) {
        self.highlights.retain(|s| s.id != id);
        self.settings.disabled_highlight_sets.retain(|d| d != id);
        let _ = self.settings.save(&self.data_dir);
        self.save_highlights(cx);
        cx.notify();
    }

    fn import_highlights(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let picked = cx.prompt_for_paths(gpui::PathPromptOptions {
            files: true,
            directories: false,
            multiple: true,
            prompt: Some("Import".into()),
        });
        cx.spawn_in(window, async move |this, cx| {
            let Ok(Ok(Some(paths))) = picked.await else { return };
            let _ = this.update(cx, |this, cx| {
                let mut imported = vec![];
                for path in &paths {
                    match std::fs::read_to_string(path).map_err(anyhow::Error::from).and_then(|t| highlight::parse(&t)) {
                        Ok(sets) => imported.extend(sets),
                        Err(err) => {
                            this.show_toast(format!("{}: {err:#}", path.display()), cx);
                            return;
                        }
                    }
                }
                let rules: usize = imported.iter().map(|s| s.keywords.len()).sum();
                let (added, replaced) = highlight::merge(&mut this.highlights, imported);
                this.save_highlights(cx);
                let mut message = format!("Imported {} set(s), {rules} rules", added + replaced);
                if replaced > 0 {
                    message.push_str(&format!(" ({replaced} replaced)"));
                }
                if !this.highlight_skipped.is_empty() {
                    message.push_str(&format!(". {} rule(s) can't be used", this.highlight_skipped.len()));
                }
                this.show_toast(message, cx);
            });
        })
        .detach();
    }

    fn export_highlight_set(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) {
        let Some(set) = self.highlights.iter().find(|s| s.id == id).cloned() else { return };
        let dir = dirs::document_dir().or_else(dirs::home_dir).unwrap_or_else(std::env::temp_dir);
        let name = format!("{}.json", sanitize(&set.name));
        let picked = cx.prompt_for_new_path(&dir, Some(&name));
        cx.spawn_in(window, async move |this, cx| {
            let Ok(Ok(Some(path))) = picked.await else { return };
            let result = highlight::export(&set).and_then(|json| Ok(std::fs::write(&path, json)?));
            let _ = this.update(cx, |this, cx| match result {
                Ok(()) => this.show_toast(format!("Exported {} to {}", set.name, path.display()), cx),
                Err(err) => this.show_toast(format!("Could not export: {err:#}"), cx),
            });
        })
        .detach();
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

    /// Items in sidebar order: standalone shells, then each Project's Hosts and shells.
    fn sidebar_items(&self) -> Vec<SidebarItem> {
        let mut items: Vec<SidebarItem> = self
            .library
            .standalone_shells()
            .into_iter()
            .map(|shell| SidebarItem::Shell { project: None, shell })
            .collect();
        for project in &self.library.projects {
            items.extend(self.project_items(project));
        }
        items
    }

    fn project_items(&self, project: &Project) -> Vec<SidebarItem> {
        let mut items: Vec<SidebarItem> = self
            .library
            .hosts_of(project)
            .map(|host| SidebarItem::Host { project: project.clone(), host: host.clone() })
            .collect();
        items.extend(project.shells.iter().map(|shell| SidebarItem::Shell { project: Some(project.clone()), shell: shell.clone() }));
        items
    }

    fn sessions_of(&self, key: &ItemKey) -> impl Iterator<Item = &SessionEntry> {
        let key = key.clone();
        self.sessions.iter().filter(move |s| s.item == key)
    }

    fn is_live(&self, entry: &SessionEntry, cx: &App) -> bool {
        !matches!(entry.view.read(cx).status, Status::Disconnected(_))
    }

    /// Clicking a sidebar item: show its Session (reconnecting it in place if it dropped,
    /// so the earlier output stays above), or connect if it has none. `another` opens an
    /// additional Session for the same item.
    fn activate_item(&mut self, item: SidebarItem, another: bool, window: &mut Window, cx: &mut Context<Self>) {
        let key = item.key();
        if !another {
            let existing = self
                .last_active
                .get(&key)
                .copied()
                .filter(|id| self.sessions.iter().any(|s| s.id == *id))
                .or_else(|| self.sessions_of(&key).next().map(|s| s.id));
            if let Some(id) = existing {
                self.activate(id, window, cx);
                let dropped = self.sessions.iter().find(|s| s.id == id).is_some_and(|s| !self.is_live(s, cx));
                if dropped {
                    self.reconnect(id, window, cx);
                }
                return;
            }
        }
        match item {
            SidebarItem::Host { project, host } => self.open_host(&project.id, &host.id, window, cx),
            SidebarItem::Shell { project, shell } => self.open_local(project.map(|p| p.id), shell, window, cx),
        }
    }

    fn add_session(
        &mut self,
        item: ItemKey,
        project_id: Option<String>,
        kind: SessionKind,
        base_label: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Entity<TerminalView> {
        let existing = self.sessions_of(&item).count();
        let label: SharedString =
            if existing == 0 { base_label.to_string().into() } else { format!("{base_label} ({})", existing + 1).into() };
        let scrollback = self.settings.scrollback_lines;
        let confirm_paste = self.settings.confirm_multiline_paste;
        let putty = self.settings.putty_clipboard;
        let command_rail = self.settings.command_rail;
        let minimap = self.settings.minimap;
        let view = cx.new(|cx| {
            let mut view = TerminalView::new(label.clone(), scrollback, window, cx);
            view.confirm_multiline_paste = confirm_paste;
            view.putty_clipboard = putty;
            view.command_rail = command_rail;
            view.minimap = minimap;
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
        if let Some(project_id) = &project_id {
            self.collapsed.remove(project_id);
        }
        self.sessions.push(SessionEntry { id, item, project_id, kind, label, view: view.clone(), _subscription: sub });
        self.activate(id, window, cx);
        view
    }

    fn open_local(&mut self, project_id: Option<String>, shell: LocalShell, window: &mut Window, cx: &mut Context<Self>) {
        let item = ItemKey::Shell { project: project_id.clone(), name: shell.name.clone() };
        let kind = SessionKind::Local { shell: shell.clone() };
        let view = self.add_session(item, project_id.clone(), kind.clone(), &shell.name, window, cx);
        self.start_backend(&view, &kind, project_id.as_deref(), None, cx);
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
        let item = ItemKey::Host { project: project_id.to_string(), host: host_id.to_string() };
        let kind = SessionKind::Ssh { host_id: host_id.to_string() };
        let view = self.add_session(item, Some(project_id.to_string()), kind.clone(), &resolved.name, window, cx);
        self.start_backend(&view, &kind, Some(project_id), secret, cx);
    }

    fn start_backend(
        &mut self,
        view: &Entity<TerminalView>,
        kind: &SessionKind,
        project_id: Option<&str>,
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
                let Some(resolved) = project_id.and_then(|p| self.library.resolve(p, host_id)) else { return };
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

    /// Start a new connection in the same terminal: the earlier output stays above.
    fn reconnect(&mut self, id: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(entry) = self.sessions.iter().find(|s| s.id == id) else { return };
        let (view, kind, project_id) = (entry.view.clone(), entry.kind.clone(), entry.project_id.clone());
        let secret = match &kind {
            SessionKind::Ssh { host_id } => project_id
                .as_deref()
                .and_then(|p| self.library.resolve(p, host_id))
                .and_then(|r| r.credential)
                .and_then(|cred| self.vault.as_ref()?.get(&cred))
                .map(|c| c.secret.clone()),
            SessionKind::Local { .. } => None,
        };
        view.update(cx, |v, _| v.close());
        self.start_backend(&view, &kind, project_id.as_deref(), secret, cx);
        self.focus_active(window, cx);
    }

    fn activate(&mut self, id: usize, window: &mut Window, cx: &mut Context<Self>) {
        self.active = Some(id);
        if let Some(entry) = self.sessions.iter().find(|s| s.id == id) {
            self.last_active.insert(entry.item.clone(), id);
        }
        if matches!(self.page, Page::Settings(_)) {
            self.page = Page::Sessions;
        }
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

    /// × on a live Session disconnects it but keeps its output (the item goes idle and a
    /// click reconnects). × on a Session that is already disconnected discards it.
    fn close_session(&mut self, id: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(index) = self.sessions.iter().position(|s| s.id == id) else { return };
        if self.is_live(&self.sessions[index], cx) {
            self.sessions[index].view.update(cx, |v, cx| {
                v.close();
                v.status = Status::Disconnected("Session closed.".into());
                cx.notify();
            });
        } else {
            let entry = self.sessions.remove(index);
            self.last_active.retain(|_, v| *v != id);
            entry.view.update(cx, |v, _| v.close());
            if self.active == Some(id) {
                self.active = None;
            }
        }
        self.focus_active(window, cx);
        cx.notify();
    }

    fn cycle(&mut self, forward: bool, window: &mut Window, cx: &mut Context<Self>) {
        // Sessions in sidebar order.
        let order: Vec<usize> = self
            .sidebar_items()
            .iter()
            .flat_map(|item| self.sessions_of(&item.key()).map(|s| s.id).collect::<Vec<_>>())
            .collect();
        if order.is_empty() {
            return;
        }
        let pos = self.active.and_then(|a| order.iter().position(|&id| id == a)).unwrap_or(0);
        let next = if forward { (pos + 1) % order.len() } else { (pos + order.len() - 1) % order.len() };
        self.activate(order[next], window, cx);
    }

    /// The item of the active Session, for "new session on this item".
    fn active_item(&self) -> Option<SidebarItem> {
        let key = self.active_entry()?.item.clone();
        self.sidebar_items().into_iter().find(|item| item.key() == key)
    }

    // ---- passwords & output -----------------------------------------------------------

    fn save_password(&mut self, session_id: usize, password: String, as_project: bool, cx: &mut Context<Self>) {
        let Some(entry) = self.sessions.iter().find(|s| s.id == session_id) else { return };
        let SessionKind::Ssh { host_id } = entry.kind.clone() else { return };
        let Some(project_id) = entry.project_id.clone() else { return };
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

    fn open_picker(&mut self, window: &mut Window, cx: &mut Context<Self>) {
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
        self.overlay = Some(Overlay::Picker { query });
        cx.notify();
    }

    /// Quick connect: every sidebar item, filtered by name, address, user or Project.
    fn picker_items(&self, cx: &App) -> Vec<SidebarItem> {
        let Some(Overlay::Picker { query }) = &self.overlay else { return vec![] };
        let needle = query.read(cx).value().to_lowercase();
        self.sidebar_items()
            .into_iter()
            .filter(|item| {
                let hay = match item {
                    SidebarItem::Host { project, host } => {
                        let r = model::resolve(host, project);
                        format!("{} {} {} {}", host.name, host.address, project.name, r.user.unwrap_or_default())
                    }
                    SidebarItem::Shell { project, shell } => {
                        format!("{} {} local", shell.name, project.as_ref().map(|p| p.name.as_str()).unwrap_or(""))
                    }
                };
                needle.is_empty() || hay.to_lowercase().contains(&needle)
            })
            .collect()
    }

    fn pick(&mut self, item: SidebarItem, window: &mut Window, cx: &mut Context<Self>) {
        self.overlay = None;
        self.activate_item(item, false, window, cx);
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
        let name = cx.new(|cx| InputState::new(window, cx).placeholder("Project name, e.g. Atlas Production"));
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
        // Standalone Local Shells: no header, at the top.
        for item in self.library.standalone_shells().into_iter().map(|shell| SidebarItem::Shell { project: None, shell }) {
            list = list.children(self.render_item(&item, p.brand, 8., p, cx));
        }
        list = list.child(
            div()
                .px(px(8.))
                .pt(px(12.))
                .pb(px(2.))
                .flex()
                .items_center()
                .justify_between()
                .child(section_label("Projects", p))
                .child(
                    icon_button("new-project", "folder-plus", p)
                        .size(px(24.))
                        .on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.open_new_project(window, cx))),
                ),
        );
        if self.library.projects.is_empty() {
            list = list.child(
                div()
                    .px(px(8.))
                    .py(px(6.))
                    .text_size(px(12.5))
                    .line_height(px(18.))
                    .text_color(p.text_faint)
                    .child("Group your servers into projects, each with its own colour."),
            );
        }
        for project in &self.library.projects {
            let color = project_color(&project.color);
            let collapsed = self.collapsed.contains(&project.id);
            let items = self.project_items(project);
            let live = items
                .iter()
                .flat_map(|item| self.sessions_of(&item.key()).collect::<Vec<_>>())
                .filter(|s| self.is_live(s, cx))
                .count();
            let pid = project.id.clone();
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
                    // When collapsed, show how many Sessions are live inside.
                    .when(collapsed && live > 0, |el| {
                        el.child(div().text_size(px(11.5)).text_color(p.text_faint).child(format!("{live} live")))
                    })
                    .child(
                        div()
                            .absolute()
                            .right(px(26.))
                            .top(px(3.))
                            .rounded(px(7.))
                            .bg(p.sidebar)
                            .invisible()
                            .group_hover("project-row", |s| s.visible())
                            .child(
                                icon_button(SharedString::from(format!("add-host-{}", project.id)), "plus", p).on_click(
                                    cx.listener(move |this, _: &ClickEvent, window, cx| {
                                        cx.stop_propagation();
                                        this.open_new_host(pid_host.clone(), window, cx);
                                    }),
                                ),
                            ),
                    )
                    .child(icon_el(if collapsed { "chevron-right" } else { "chevron-down" }, 14., p.text_faint)),
            );
            if collapsed {
                continue;
            }
            if items.is_empty() {
                let pid_add = project.id.clone();
                list = list.child(
                    div()
                        .id(SharedString::from(format!("first-host-{}", project.id)))
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
                        .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| this.open_new_host(pid_add.clone(), window, cx)))
                        .child(icon_el("plus", 14., p.text_faint))
                        .child("Add a host"),
                );
            }
            for item in &items {
                list = list.children(self.render_item(item, color, 20., p, cx));
            }
        }

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

        let nav = island(p)
            .flex_1()
            .min_h_0()
            .flex()
            .flex_col()
            .overflow_hidden()
            .child(
                div()
                    .h(px(60.))
                    .px(px(18.))
                    .flex()
                    .items_center()
                    .justify_between()
                    .child(wordmark(21., p.text)),
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
                            .on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.open_picker(window, cx)))
                            .child(icon_el("search", 14., p.text_faint))
                            .child(div().flex_1().child("Quick connect"))
                            .child(kbd("Ctrl Shift K", p)),
                    ),
            )
            .child(list.pb(px(8.)));

        div()
            .w(px(SIDEBAR_WIDTH))
            .h_full()
            .flex()
            .flex_col()
            .flex_shrink_0()
            .gap(px(ISLAND_GAP))
            .child(nav)
            .child(
                island(p)
                    .p(px(8.))
                    .flex()
                    .flex_col()
                    .child(
                        footer_row("settings", "settings", "Settings".into())
                            .child(div().flex_1())
                            .child(kbd("Ctrl ,", p))
                            .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                                this.open_settings(SettingsSection::Appearance, window, cx)
                            })),
                    ),
            )
            .into_any_element()
    }

    /// One sidebar item, plus a nested row for each additional Session on it.
    ///
    /// Live items are bright with a status dot; idle ones are dimmed; a dropped Session
    /// that still holds its output shows a hollow ring until it is reconnected or discarded.
    fn render_item(&self, item: &SidebarItem, color: Hsla, indent: f32, p: AppPalette, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let key = item.key();
        let sessions: Vec<&SessionEntry> = self.sessions_of(&key).collect();
        let shown = self.last_active.get(&key).copied().or_else(|| sessions.first().map(|s| s.id));
        let mut rows = vec![];
        let row = |id: SharedString,
                   label: SharedString,
                   icon: String,
                   session: Option<&SessionEntry>,
                   nested: bool,
                   item: SidebarItem,
                   cx: &mut Context<Self>| {
            let status = session.map(|s| s.view.read(cx).status.clone());
            let active = session.is_some_and(|s| Some(s.id) == self.active);
            let live = matches!(status, Some(Status::Connected | Status::Connecting));
            let session_id = session.map(|s| s.id);
            let item_for_new = item.clone();
            let indicator = match &status {
                Some(Status::Connected) => status_dot(p.success).into_any_element(),
                Some(Status::Connecting) => status_dot(p.warning).into_any_element(),
                Some(Status::Disconnected(_)) => div()
                    .size(px(7.))
                    .flex_shrink_0()
                    .rounded_full()
                    .border_1()
                    .border_color(p.text_faint)
                    .into_any_element(),
                None => div().into_any_element(),
            };
            div()
                .id(id)
                .group("item-row")
                .relative()
                .h(px(if nested { 30. } else { 34. }))
                .pl(px(if nested { indent + 18. } else { indent }))
                .pr(px(10.))
                .flex()
                .items_center()
                .gap(px(10.))
                .rounded(px(8.))
                .cursor_pointer()
                .when(active, |el| el.bg(p.active))
                .when(!active, |el| el.hover(|s| s.bg(p.hover)))
                .on_click(cx.listener(move |this, event: &ClickEvent, window, cx| match session_id {
                    // Nested rows are specific Sessions.
                    Some(id) if nested => {
                        this.activate(id, window, cx);
                        let dropped = this.sessions.iter().find(|s| s.id == id).is_some_and(|s| !this.is_live(s, cx));
                        if dropped {
                            this.reconnect(id, window, cx);
                        }
                    }
                    _ => this.activate_item(item.clone(), event.modifiers().control, window, cx),
                }))
                .when(!nested, |el| {
                    el.child(icon_el(&icon, 15., if active || live { color } else { p.text_faint }))
                })
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .text_size(px(if nested { 12.5 } else { 13. }))
                        .text_color(if active || live { p.text } else { p.text_muted })
                        .child(label),
                )
                .child(indicator)
                .child(
                    div()
                        .absolute()
                        .right(px(4.))
                        .top(px(if nested { 3. } else { 5. }))
                        .flex()
                        .gap(px(2.))
                        .rounded(px(7.))
                        .bg(if active { p.sidebar } else { p.sidebar })
                        .invisible()
                        .group_hover("item-row", |s| s.visible())
                        .when(!nested, |el| {
                            el.child(
                                icon_button(SharedString::from(format!("another-{key:?}")), "plus", p).size(px(24.)).on_click(
                                    cx.listener(move |this, _: &ClickEvent, window, cx| {
                                        cx.stop_propagation();
                                        this.activate_item(item_for_new.clone(), true, window, cx);
                                    }),
                                ),
                            )
                        })
                        .when_some(session_id, |el, id| {
                            el.child(icon_button(SharedString::from(format!("close-{id}")), "x", p).size(px(24.)).on_click(
                                cx.listener(move |this, _: &ClickEvent, window, cx| {
                                    cx.stop_propagation();
                                    this.close_session(id, window, cx);
                                }),
                            ))
                        }),
                )
                .into_any_element()
        };
        let main = shown.and_then(|id| sessions.iter().find(|s| s.id == id).copied()).or(sessions.first().copied());
        rows.push(row(
            SharedString::from(format!("item-{key:?}")),
            item.label().to_string().into(),
            item.icon(),
            main,
            false,
            item.clone(),
            cx,
        ));
        if sessions.len() > 1 {
            for session in &sessions {
                rows.push(row(
                    SharedString::from(format!("session-{}", session.id)),
                    session.label.clone(),
                    item.icon(),
                    Some(session),
                    true,
                    item.clone(),
                    cx,
                ));
            }
        }
        rows
    }

    fn render_main(&self, p: AppPalette, cx: &mut Context<Self>) -> AnyElement {
        let Some(entry) = self.active_entry() else {
            return self.render_empty(p, cx);
        };
        let project = entry.project_id.as_deref().and_then(|id| self.library.project(id));
        let color = project.map(|pr| project_color(&pr.color)).unwrap_or(p.text_faint);
        let view = entry.view.read(cx);
        // Just enough to know where you are typing: state, user@address and port.
        // The Host name, icon and Project are already in the sidebar.
        let target = match &entry.kind {
            SessionKind::Ssh { host_id } => entry
                .project_id
                .as_deref()
                .and_then(|id| self.library.resolve(id, host_id))
                .map(|r| {
                    let user = r.user.clone().map(|u| format!("{u}@")).unwrap_or_default();
                    format!("{user}{}:{}", r.address, r.port)
                })
                .unwrap_or_else(|| entry.label.clone().to_string()),
            SessionKind::Local { shell } => format!("Local · {}", shell.program),
        };
        let (status_text, status_color) = match &view.status {
            Status::Connecting => ("Connecting", p.warning),
            Status::Connected => ("Connected", p.success),
            Status::Disconnected(_) => ("Disconnected", p.text_faint),
        };
        let id = entry.id;
        let term_bg = cx.global::<Appearance>().term.background_hsla();
        div()
            .flex_1()
            .min_w_0()
            .h_full()
            .flex()
            .flex_col()
            .gap(px(4.))
            .child(
                div()
                    .id("session-strip")
                    .h(px(22.))
                    .flex_shrink_0()
                    .pl(px(6.))
                    .flex()
                    .items_center()
                    .gap(px(7.))
                    .child(status_dot(status_color))
                    .child(
                        div()
                            .id("session-target")
                            .min_w_0()
                            .text_size(px(12.))
                            .text_color(p.text_faint)
                            .truncate()
                            .tooltip(move |window, cx| Tooltip::new(status_text).build(window, cx))
                            .child(target),
                    )
                    .child(div().flex_1())
                    .child(
                        strip_action("copy-text", "copy", "Copy output", p)
                            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.copy_output_text(cx))),
                    )
                    .child(
                        strip_action("copy-file", "file-text", "Copy as .txt", p)
                            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.copy_output_file(cx))),
                    )
                    .child(
                        strip_action("close-active", "x", "Close session", p)
                            .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| this.close_session(id, window, cx))),
                    ),
            )
            .child(
                island(p)
                    .relative()
                    .flex_1()
                    .min_h_0()
                    .bg(term_bg)
                    .overflow_hidden()
                    .child(entry.view.clone())
                    // Project colour accent: you always see which Project you are typing into.
                    .child(
                        div()
                            .absolute()
                            .top_0()
                            .left(px(ISLAND_RADIUS))
                            .right(px(ISLAND_RADIUS))
                            .h(px(2.))
                            .rounded_b(px(2.))
                            .bg(color),
                    ),
            )
            .into_any_element()
    }

    fn render_empty(&self, p: AppPalette, cx: &mut Context<Self>) -> AnyElement {
        island(p)
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
                            .on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.open_picker(window, cx))),
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
            Overlay::Picker { query } => {
                let items = self.picker_items(cx);
                let title = "Quick connect".to_string();
                let mut list = div().id("picker-list").flex().flex_col().gap(px(2.)).max_h(px(380.)).overflow_y_scroll().p(px(6.));
                if items.is_empty() {
                    list = list.child(
                        div().p(px(16.)).text_size(px(13.)).text_color(p.text_faint).child("Nothing matches. Add hosts with the + next to a project."),
                    );
                }
                for (i, item) in items.into_iter().enumerate() {
                    let (icon, color, name, detail) = match &item {
                        SidebarItem::Host { project, host } => {
                            let r = model::resolve(host, project);
                            let user = r.user.clone().map(|u| format!("{u}@")).unwrap_or_default();
                            (r.icon.clone(), project_color(&r.color), host.name.clone(), format!("{user}{} · {}", host.address, project.name))
                        }
                        SidebarItem::Shell { project, shell } => (
                            "square-terminal".to_string(),
                            project.as_ref().map(|p| project_color(&p.color)).unwrap_or(p.brand),
                            shell.name.clone(),
                            match project {
                                Some(project) => format!("Local shell · {}", project.name),
                                None => "Local shell".to_string(),
                            },
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
                .items_start()
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

/// A segmented control: one choice of several.
fn segmented(
    id: &'static str,
    options: &[(&'static str, bool)],
    p: AppPalette,
    cx: &mut Context<Workspace>,
    pick: impl Fn(&mut Workspace, usize, &mut Window, &mut Context<Workspace>) + Clone + 'static,
) -> AnyElement {
    let items: Vec<_> = options.iter().enumerate().map(|(i, (label, selected))| {
            let pick = pick.clone();
            div()
                .id((id, i))
                .h(px(26.))
                .px(px(10.))
                .flex()
                .items_center()
                .rounded(px(6.))
                .cursor_pointer()
                .text_size(px(12.5))
                .font_weight(FontWeight::MEDIUM)
                .text_color(if *selected { p.text } else { p.text_muted })
                .when(*selected, |el| el.bg(p.brand_soft).text_color(p.brand))
                .when(!*selected, |el| el.hover(|s| s.text_color(p.text)))
                .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| pick(this, i, window, cx)))
                .child(*label)
        })
        .collect();
    div()
        .flex()
        .flex_shrink_0()
        .p(px(3.))
        .gap(px(2.))
        .rounded(px(9.))
        .bg(p.hover)
        .border_1()
        .border_color(p.border)
        .children(items)
        .into_any_element()
}

/// An on/off switch, 32×18.
fn switch(id: SharedString, on: bool, p: AppPalette) -> gpui::Stateful<gpui::Div> {
    div()
        .id(id)
        .w(px(32.))
        .h(px(18.))
        .flex_shrink_0()
        .p(px(2.))
        .rounded_full()
        .cursor_pointer()
        .bg(if on { p.brand } else { p.active })
        .flex()
        .when(on, |el| el.justify_end())
        .child(div().size(px(14.)).rounded_full().bg(gpui::white()).shadow(elevation(false)))
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

/// A small icon-only action for the session strip above the terminal.
fn strip_action(id: &'static str, icon: &str, label: &'static str, p: AppPalette) -> gpui::Stateful<gpui::Div> {
    div()
        .id(id)
        .size(px(22.))
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(6.))
        .cursor_pointer()
        .hover(|s| s.bg(p.hover))
        .tooltip(move |window, cx| Tooltip::new(label).build(window, cx))
        .child(icon_el(icon, 13., p.text_faint))
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
                } else if event.keystroke.key == "escape" && matches!(this.page, Page::Settings(_)) && this.unlock.is_none() {
                    this.close_settings(window, cx);
                    cx.stop_propagation();
                }
            }))
            .on_action(cx.listener(|this, _: &QuickConnect, window, cx| this.open_picker(window, cx)))
            // Another Session on the item you're in; without one, quick connect.
            .on_action(cx.listener(|this, _: &NewSession, window, cx| match this.active_item() {
                Some(item) => this.activate_item(item, true, window, cx),
                None => this.open_picker(window, cx),
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
            .on_action(cx.listener(|this, _: &LockVault, _, cx| this.lock_vault(cx)))
            .on_action(cx.listener(|this, _: &OpenSettings, window, cx| {
                if matches!(this.page, Page::Settings(_)) {
                    this.close_settings(window, cx);
                } else {
                    this.open_settings(SettingsSection::Appearance, window, cx);
                }
            }));

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
        let page = match self.page {
            Page::Settings(section) => self.render_settings(section, p, cx),
            Page::Sessions => div()
                .size_full()
                .flex()
                .gap(px(ISLAND_GAP))
                .p(px(ISLAND_GAP))
                .bg(p.canvas)
                .child(self.render_sidebar(p, cx))
                .child(self.render_main(p, cx))
                .into_any_element(),
        };
        root.child(page)
            .children(self.render_overlay(p, cx))
            .children(self.unlock.as_ref().map(|s| self.render_unlock(s, p, cx)))
            .children(toast)
            .into_any_element()
    }
}
