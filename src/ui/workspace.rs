//! The main window: unlock screen, Project sidebar with its Sessions, the active terminal,
//! quick connect, and the small forms for adding Projects and Hosts.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::time::{Duration, Instant};

use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, App, ClickEvent, Context, Entity, FocusHandle, Focusable, FontWeight, Hsla, KeyDownEvent,
    SharedString, Subscription, Window, WindowAppearance, actions, div, prelude::*, px, WindowControlArea, MouseButton, ListState, ListAlignment, list,
};
use gpui_component::input::{Input, InputEvent, InputState};
use gpui_component::menu::{ContextMenuExt, PopupMenu, PopupMenuItem};
use gpui_component::Icon;
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
/// Height of Komando's own title bar (and of its window buttons).
const TITLE_BAR_HEIGHT: f32 = 36.;

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

/// A row of the sidebar list: what it shows, by position in the Library. `sessions` is part of
/// it because a host with several Sessions is taller (one nested row each).
#[derive(Clone, PartialEq)]
enum SidebarRow {
    Shell { index: usize, sessions: usize },
    ProjectsHeader,
    NoProjects,
    Project { index: usize },
    FirstHost { project: usize },
    Item { project: usize, index: usize, sessions: usize },
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
    /// The terminal's status, copied here when it changes. The sidebar reads this rather than
    /// the terminal: reading a terminal while rendering makes gpui redraw the whole window on
    /// every bit of output it gets, even when it isn't on screen.
    status: Status,
    _subscription: Subscription,
}

impl SessionEntry {
    fn is_live(&self) -> bool {
        !matches!(self.status, Status::Disconnected(_))
    }
}

enum Overlay {
    Picker { query: Entity<InputState> },
    /// Add a Host to a Project, or edit one (`editing` is its id).
    NewHost {
        project_id: String,
        editing: Option<String>,
        fields: Vec<Entity<InputState>>,
        icon: String,
        /// The icon picker's search.
        icon_query: Entity<InputState>,
        auth: HostAuth,
        error: Option<String>,
    },
    /// Create a Project, or edit one (`editing` is its id).
    NewProject {
        editing: Option<String>,
        name: Entity<InputState>,
        color: String,
        icon: String,
        /// The icon picker's search.
        icon_query: Entity<InputState>,
        error: Option<String>,
    },
    /// Ask before something that can't be undone.
    Confirm { title: String, body: String, action: ConfirmAction },
}

/// How a Host signs in, as chosen in the host form.
#[derive(Clone, Copy, PartialEq, Eq)]
enum HostAuth {
    /// No credential of its own: the Project's saved credential if it has one, else a
    /// password typed on connect (which Komando offers to save).
    Inherit,
    /// Keep the Host's own saved credential (editing only).
    Saved,
    /// Import a private key file into the vault.
    KeyFile,
}

#[derive(Clone)]
enum ConfirmAction {
    DeleteHost(String),
    DeleteProject(String),
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
    /// The sidebar list (virtual: only rows in view are built) and the rows it was built for.
    sidebar_list: ListState,
    sidebar_rows: Vec<SidebarRow>,
    /// The highlight set whose delete button was clicked once and now asks for a second click.
    confirm_delete_set: Option<(String, Instant)>,
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
            confirm_delete_set: None,
            sidebar_list: ListState::new(0, ListAlignment::Top, px(240.)),
            sidebar_rows: vec![],
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
        let app = cx.global::<Appearance>().app;
        let brand = app.brand;
        gpui_component::Theme::update(cx, |theme| {
            // Menus (right-click on hosts and projects) use the app's own surfaces.
            theme.colors.popover = app.surface.opacity(1.);
            theme.colors.popover_foreground = app.text;
            theme.colors.accent = app.active;
            theme.colors.accent_foreground = app.text;
            theme.colors.muted_foreground = app.text_muted;
            theme.colors.primary = brand;
            theme.colors.ring = brand;
            theme.colors.caret = brand;
            // Sliders match the switches and selectors instead of the library's default.
            theme.colors.slider_bar = brand;
            theme.colors.slider_thumb = gpui::white();
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
                let dropped = self.sessions.iter().find(|s| s.id == id).is_some_and(|s| !s.is_live());
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
        let sub = cx.subscribe_in(&view, window, move |this, view, event: &TerminalEvent, window, cx| match event {
            TerminalEvent::Changed => {
                let status = view.read(cx).status.clone();
                if let Some(entry) = this.sessions.iter_mut().find(|s| s.id == id) {
                    entry.status = status;
                }
                cx.notify();
            }
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
        self.sessions.push(SessionEntry {
            id,
            item,
            project_id,
            kind,
            label,
            view: view.clone(),
            status: Status::Connecting,
            _subscription: sub,
        });
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
            v.set_status(Status::Connecting, cx);
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
        if self.sessions[index].is_live() {
            self.sessions[index].view.update(cx, |v, cx| {
                v.close();
                v.set_status(Status::Disconnected("Session closed.".into()), cx);
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
        self.open_host_form(project_id, None, window, cx);
    }

    fn open_edit_host(&mut self, project_id: String, host_id: String, window: &mut Window, cx: &mut Context<Self>) {
        self.open_host_form(project_id, Some(host_id), window, cx);
    }

    /// The host form's text fields, in order: name, address, user, port, key file.
    fn open_host_form(&mut self, project_id: String, editing: Option<String>, window: &mut Window, cx: &mut Context<Self>) {
        let host = editing.as_deref().and_then(|id| self.library.host(id)).cloned();
        let default_user = self.library.project(&project_id).and_then(|p| p.default_user.clone());
        let user_hint = match &default_user {
            Some(u) => format!("{u} (project default)"),
            None => "root".to_string(),
        };
        let placeholders: [String; 5] = [
            "e.g. web01".into(),
            "e.g. 10.0.4.21 or web01.example.com".into(),
            user_hint,
            "22".into(),
            r"C:\Users\you\.ssh\id_ed25519".into(),
        ];
        let values = match &host {
            Some(h) => [
                h.name.clone(),
                h.address.clone(),
                h.user.clone().unwrap_or_default(),
                if h.port == 22 { String::new() } else { h.port.to_string() },
                String::new(),
            ],
            None => Default::default(),
        };
        let fields: Vec<_> = placeholders
            .into_iter()
            .zip(values)
            .map(|(ph, value)| cx.new(|cx| InputState::new(window, cx).placeholder(ph).default_value(value)))
            .collect();
        // Enter in any field submits the form.
        for field in &fields {
            let sub = cx.subscribe_in(field, window, |this, _, event: &InputEvent, window, cx| {
                if let InputEvent::PressEnter { .. } = event {
                    this.submit_new_host(window, cx);
                }
            });
            self._subscriptions.push(sub);
        }
        fields[0].update(cx, |f, cx| f.focus(window, cx));
        let auth = if host.as_ref().is_some_and(|h| h.credential.is_some()) { HostAuth::Saved } else { HostAuth::Inherit };
        let icon = host.and_then(|h| h.icon).unwrap_or_else(|| "server".into());
        let icon_query = self.new_icon_query(window, cx);
        self.overlay = Some(Overlay::NewHost { project_id, editing, fields, icon, icon_query, auth, error: None });
        cx.notify();
    }

    /// The search box of a form's icon picker.
    fn new_icon_query(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Entity<InputState> {
        let query = cx.new(|cx| InputState::new(window, cx).placeholder("Search icons, e.g. database, cloud, git"));
        let sub = cx.subscribe_in(&query, window, |_, _, event: &InputEvent, _, cx| {
            if let InputEvent::Change = event {
                cx.notify();
            }
        });
        self._subscriptions.push(sub);
        query
    }

    /// Pick a private key file for the host form's key field.
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
                if let Some(Overlay::NewHost { fields, auth, error, .. }) = this.overlay.as_mut() {
                    *auth = HostAuth::KeyFile;
                    *error = None;
                    fields[4].update(cx, |f, cx| f.set_value(path.display().to_string(), window, cx));
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn submit_new_host(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        let Some(Overlay::NewHost { project_id, editing, fields, icon, auth, error, .. }) = self.overlay.as_mut() else { return };
        let value = |i: usize| fields[i].read(cx).value().trim().to_string();
        let (name, address, user, port, key_path) = (value(0), value(1), value(2), value(3), value(4));
        let fail = |error: &mut Option<String>, message: String, cx: &mut Context<Self>| {
            *error = Some(message);
            cx.notify();
        };
        if name.is_empty() || address.is_empty() {
            return fail(error, "Give the host a name and an address.".into(), cx);
        }
        let port = if port.is_empty() {
            22
        } else {
            match port.parse::<u16>() {
                Ok(p) if p > 0 => p,
                _ => return fail(error, "The port is a number from 1 to 65535.".into(), cx),
            }
        };
        // What the host's own credential becomes: `Some(new)` replaces it, `None` keeps it.
        let mut credential: Option<Option<String>> = match auth {
            HostAuth::Inherit => Some(None),
            HostAuth::Saved => None,
            HostAuth::KeyFile => None,
        };
        if *auth == HostAuth::KeyFile {
            if key_path.is_empty() {
                return fail(error, "Choose a private key file, or pick another sign-in.".into(), cx);
            }
            let Some(vault) = self.vault.as_mut() else {
                return fail(error, "Unlock the vault to import a key.".into(), cx);
            };
            let key = match std::fs::read_to_string(&key_path) {
                Ok(key) => key,
                Err(err) => return fail(error, format!("Could not read {key_path}: {err}"), cx),
            };
            match vault.add(Credential { label: format!("{name} key"), secret: Secret::PrivateKey { key, passphrase: None } }) {
                Ok(id) => credential = Some(Some(id)),
                Err(err) => return fail(error, format!("Could not store the key: {err}"), cx),
            }
        }
        let user = (!user.is_empty()).then_some(user);
        let icon = icon.clone();
        if let Some(id) = editing.clone() {
            if let Some(host) = self.library.hosts.iter_mut().find(|h| h.id == id) {
                host.name = name;
                host.address = address;
                host.port = port;
                host.user = user;
                host.icon = Some(icon);
                if let Some(credential) = credential {
                    host.credential = credential;
                }
            }
        } else {
            let host = Host {
                id: model::new_id(),
                name,
                address,
                port,
                user,
                credential: credential.flatten(),
                color: None,
                icon: Some(icon),
            };
            let project_id = project_id.clone();
            if let Some(project) = self.library.projects.iter_mut().find(|p| p.id == project_id) {
                project.hosts.push(host.id.clone());
            }
            self.library.hosts.push(host);
        }
        let _ = self.library.save(&self.data_dir);
        self.overlay = None;
        cx.notify();
    }

    fn open_new_project(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.open_project_form(None, window, cx);
    }

    fn open_edit_project(&mut self, project_id: String, window: &mut Window, cx: &mut Context<Self>) {
        self.open_project_form(Some(project_id), window, cx);
    }

    fn open_project_form(&mut self, editing: Option<String>, window: &mut Window, cx: &mut Context<Self>) {
        let project = editing.as_deref().and_then(|id| self.library.project(id)).cloned();
        let value = project.as_ref().map(|p| p.name.clone()).unwrap_or_default();
        let name = cx.new(|cx| InputState::new(window, cx).placeholder("Project name, e.g. Atlas Production").default_value(value));
        name.update(cx, |f, cx| f.focus(window, cx));
        let (color, icon) = match &project {
            Some(p) => (p.color.clone(), p.icon.clone()),
            None => ("blue".into(), "folder".into()),
        };
        let icon_query = self.new_icon_query(window, cx);
        self.overlay = Some(Overlay::NewProject { editing, name, color, icon, icon_query, error: None });
        cx.notify();
    }

    fn submit_new_project(&mut self, cx: &mut Context<Self>) {
        let Some(Overlay::NewProject { editing, name, color, icon, error, .. }) = self.overlay.as_mut() else { return };
        let name = name.read(cx).value().trim().to_string();
        if name.is_empty() {
            *error = Some("Give the project a name.".into());
            cx.notify();
            return;
        }
        if let Some(id) = editing.clone() {
            let (color, icon) = (color.clone(), icon.clone());
            if let Some(project) = self.library.projects.iter_mut().find(|p| p.id == id) {
                project.name = name;
                project.color = color;
                project.icon = icon;
            }
            let _ = self.library.save(&self.data_dir);
            self.overlay = None;
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

    fn confirm_delete_host(&mut self, host_id: String, cx: &mut Context<Self>) {
        let Some(host) = self.library.host(&host_id) else { return };
        let projects = self.library.projects.iter().filter(|p| p.hosts.contains(&host_id)).count();
        let mut body = format!("{} ({}) is removed from Komando.", host.name, host.address);
        if projects > 1 {
            body.push_str(&format!(" It is in {projects} projects and leaves all of them."));
        }
        body.push_str(" Its open sessions are closed. A saved key or password stays in the vault.");
        self.overlay = Some(Overlay::Confirm {
            title: format!("Delete {}?", host.name),
            body,
            action: ConfirmAction::DeleteHost(host_id),
        });
        cx.notify();
    }

    fn confirm_delete_project(&mut self, project_id: String, cx: &mut Context<Self>) {
        let Some(project) = self.library.project(&project_id) else { return };
        let only_here = project
            .hosts
            .iter()
            .filter(|h| !self.library.projects.iter().any(|p| p.id != project_id && p.hosts.contains(h)))
            .count();
        let body = match only_here {
            0 => "Its hosts are also in other projects and stay there. Open sessions in this project are closed.".to_string(),
            1 => "Its 1 host is deleted too. Open sessions in this project are closed.".to_string(),
            n => format!("Its {n} hosts are deleted too. Open sessions in this project are closed."),
        };
        self.overlay = Some(Overlay::Confirm { title: format!("Delete {}?", project.name), body, action: ConfirmAction::DeleteProject(project_id) });
        cx.notify();
    }

    fn run_confirm(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(Overlay::Confirm { action, .. }) = self.overlay.take() else { return };
        match action {
            ConfirmAction::DeleteHost(id) => {
                self.discard_sessions(|s| matches!(&s.kind, SessionKind::Ssh { host_id } if *host_id == id), cx);
                for project in &mut self.library.projects {
                    project.hosts.retain(|h| *h != id);
                }
                self.library.hosts.retain(|h| h.id != id);
            }
            ConfirmAction::DeleteProject(id) => {
                self.discard_sessions(|s| s.project_id.as_deref() == Some(id.as_str()), cx);
                let hosts = self.library.project(&id).map(|p| p.hosts.clone()).unwrap_or_default();
                self.library.projects.retain(|p| p.id != id);
                let still_used = |h: &String| self.library.projects.iter().any(|p| p.hosts.contains(h));
                let orphans: Vec<String> = hosts.into_iter().filter(|h| !still_used(h)).collect();
                self.library.hosts.retain(|h| !orphans.contains(&h.id));
                self.collapsed.remove(&id);
            }
        }
        let _ = self.library.save(&self.data_dir);
        self.focus_active(window, cx);
        cx.notify();
    }

    /// Close and drop every Session that matches, e.g. those of a deleted Host.
    fn discard_sessions(&mut self, matches: impl Fn(&SessionEntry) -> bool, cx: &mut Context<Self>) {
        let (gone, kept): (Vec<_>, Vec<_>) = std::mem::take(&mut self.sessions).into_iter().partition(|s| matches(s));
        self.sessions = kept;
        for entry in gone {
            entry.view.update(cx, |v, _| v.close());
            self.last_active.retain(|_, v| *v != entry.id);
            if self.active == Some(entry.id) {
                self.active = None;
            }
        }
    }

    fn copy_host_address(&mut self, project_id: &str, host_id: &str, cx: &mut Context<Self>) {
        let Some(r) = self.library.resolve(project_id, host_id) else { return };
        let user = r.user.map(|u| format!("{u}@")).unwrap_or_default();
        let text = if r.port == 22 { format!("{user}{}", r.address) } else { format!("{user}{}:{}", r.address, r.port) };
        cx.write_to_clipboard(gpui::ClipboardItem::new_string(text.clone()));
        self.show_toast(format!("Copied {text}"), cx);
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
                        button("skip", if state.creating { "Set up later" } else { "Continue without unlocking" }, ButtonKind::Ghost, p)
                            .on_click(cx.listener(|this, _, window, cx| this.skip_unlock(window, cx))),
                    ),
            );
        let note = if state.creating {
            "This password encrypts your saved passwords and keys."
        } else {
            "Unlock to connect with saved passwords and keys."
        };
        // The one sentence on this screen that must not be skimmed past.
        let warning = state.creating.then(|| {
            div()
                .text_size(px(13.))
                .line_height(px(20.))
                .font_weight(FontWeight::MEDIUM)
                .text_color(p.warning)
                .child("There is no recovery: if you forget it, saved passwords are lost. Projects and hosts are kept.")
        });
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
                        .flex()
                        .flex_col()
                        .gap(px(8.))
                        .text_center()
                        .child(div().text_size(px(13.)).line_height(px(20.)).text_color(p.text_muted).child(note))
                        .children(warning),
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
                    .children(warning)
                    .child(form)
                    .into_any_element(),
            )
        }
    }

    /// The sidebar's rows, in order. Kept small (indices, counts) so working it out on every
    /// frame is cheap; only the rows in view are built into elements.
    fn sidebar_rows(&self) -> Vec<SidebarRow> {
        let mut counts: HashMap<&ItemKey, usize> = HashMap::new();
        for s in &self.sessions {
            *counts.entry(&s.item).or_default() += 1;
        }
        let mut rows = vec![];
        for (index, shell) in self.library.standalone_shells().iter().enumerate() {
            let key = ItemKey::Shell { project: None, name: shell.name.clone() };
            rows.push(SidebarRow::Shell { index, sessions: counts.get(&key).copied().unwrap_or(0) });
        }
        rows.push(SidebarRow::ProjectsHeader);
        if self.library.projects.is_empty() {
            rows.push(SidebarRow::NoProjects);
        }
        for (pi, project) in self.library.projects.iter().enumerate() {
            rows.push(SidebarRow::Project { index: pi });
            if self.collapsed.contains(&project.id) {
                continue;
            }
            let hosts = self.library.hosts_of(project).map(|h| ItemKey::Host { project: project.id.clone(), host: h.id.clone() });
            let shells = project.shells.iter().map(|sh| ItemKey::Shell { project: Some(project.id.clone()), name: sh.name.clone() });
            let keys: Vec<ItemKey> = hosts.chain(shells).collect();
            if keys.is_empty() {
                rows.push(SidebarRow::FirstHost { project: pi });
            }
            for (index, key) in keys.iter().enumerate() {
                rows.push(SidebarRow::Item { project: pi, index, sessions: counts.get(key).copied().unwrap_or(0) });
            }
        }
        rows
    }

    /// One row of the sidebar list, built only when it's in view.
    fn render_sidebar_row(&mut self, ix: usize, cx: &mut Context<Self>) -> AnyElement {
        let p = cx.global::<Appearance>().app;
        let Some(row) = self.sidebar_rows.get(ix).cloned() else { return div().into_any_element() };
        let element = match row {
            SidebarRow::Shell { index, .. } => match self.library.standalone_shells().get(index).cloned() {
                Some(shell) => div()
                    .flex()
                    .flex_col()
                    .gap(px(2.))
                    .children(self.render_item(&SidebarItem::Shell { project: None, shell }, p.brand, 8., p, cx))
                    .into_any_element(),
                None => div().into_any_element(),
            },
            SidebarRow::ProjectsHeader => {
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
                )
            .into_any_element()
            }
            SidebarRow::NoProjects => {
            div()
                    .px(px(8.))
                    .py(px(6.))
                    .text_size(px(12.5))
                    .line_height(px(18.))
                    .text_color(p.text_faint)
                    .child("Group your servers into projects, each with its own colour.")
                .into_any_element()
            }
            SidebarRow::Project { index } => self.render_project_row(index, p, cx),
            SidebarRow::FirstHost { project } => {
                let Some(project) = self.library.projects.get(project) else { return div().into_any_element() };
                let pid_add = project.id.clone();
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
                        .child("Add a host")
                    .into_any_element()
            }
            SidebarRow::Item { project, index, .. } => {
                let Some(project) = self.library.projects.get(project).cloned() else { return div().into_any_element() };
                let color = project_color(&project.color);
                match self.project_items(&project).get(index) {
                    Some(item) => div()
                        .flex()
                        .flex_col()
                        .gap(px(2.))
                        .children(self.render_item(item, color, 20., p, cx))
                        .into_any_element(),
                    None => div().into_any_element(),
                }
            }
        };
        div().px(px(8.)).pb(px(2.)).child(element).into_any_element()
    }

    fn render_project_row(&self, index: usize, p: AppPalette, cx: &mut Context<Self>) -> AnyElement {
        let Some(project) = self.library.projects.get(index) else { return div().into_any_element() };
            let color = project_color(&project.color);
            let collapsed = self.collapsed.contains(&project.id);
            let items = self.project_items(project);
            let live = items
                .iter()
                .flat_map(|item| self.sessions_of(&item.key()).collect::<Vec<_>>())
                .filter(|s| s.is_live())
                .count();
            let pid = project.id.clone();
            let pid_host = project.id.clone();
            let ws = cx.entity().downgrade();
            let menu_pid = project.id.clone();
            let project_menu = move |menu: PopupMenu, _: &mut Window, _: &mut Context<PopupMenu>| {
                let id = menu_pid.clone();
                let (a, b, c, d) = (id.clone(), id.clone(), id.clone(), id);
                menu.item(menu_item("Add host…", "plus", &ws, move |this, window, cx| this.open_new_host(a.clone(), window, cx)))
                    .item(menu_item("Edit project…", "pencil", &ws, move |this, window, cx| this.open_edit_project(b.clone(), window, cx)))
                    .item(menu_item(if collapsed { "Expand" } else { "Collapse" }, if collapsed { "chevron-down" } else { "chevron-right" }, &ws, move |this, _, cx| {
                        if !this.collapsed.remove(&c) {
                            this.collapsed.insert(c.clone());
                        }
                        cx.notify();
                    }))
                    .separator()
                    .item(menu_item("Delete project…", "trash", &ws, move |this, _, cx| this.confirm_delete_project(d.clone(), cx)))
            };
            div()
                    .id(SharedString::from(format!("project-{}", project.id)))
                    .group("project-row")
                    .text_size(px(13.))
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
                    .child(icon_el(if collapsed { "chevron-right" } else { "chevron-down" }, 14., p.text_faint))
                    .context_menu(project_menu)
                .into_any_element()
    }

    fn render_sidebar(&mut self, p: AppPalette, cx: &mut Context<Self>) -> AnyElement {
        // Only the rows in view are built (hundreds of hosts would otherwise cost milliseconds
        // on every frame). Row heights are measured once and kept until the rows change.
        let rows = self.sidebar_rows();
        if rows != self.sidebar_rows {
            self.sidebar_list.reset(rows.len());
            self.sidebar_rows = rows;
        }
        let list = list(self.sidebar_list.clone(), cx.processor(|this, ix, _window, cx| this.render_sidebar_row(ix, cx)))
            .flex_1()
            .min_h_0();

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
                    .px(px(8.))
                    .pt(px(8.))
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
            let status = session.map(|s| s.status.clone());
            let active = session.is_some_and(|s| Some(s.id) == self.active);
            let live = matches!(status, Some(Status::Connected | Status::Connecting));
            let session_id = session.map(|s| s.id);
            let item_for_new = item.clone();
            let ws = cx.entity().downgrade();
            let menu_item_src = item.clone();
            let dropped = matches!(status, Some(Status::Disconnected(_)));
            let item_menu = move |menu: PopupMenu, _: &mut Window, _: &mut Context<PopupMenu>| {
                let item = menu_item_src.clone();
                let mut menu = menu;
                if nested {
                    // A specific Session of the item.
                    if let Some(id) = session_id {
                        if dropped {
                            menu = menu
                                .item(menu_item("Reconnect", "refresh-cw", &ws, move |this, window, cx| {
                                    this.activate(id, window, cx);
                                    this.reconnect(id, window, cx);
                                }))
                                .item(menu_item("Close", "x", &ws, move |this, window, cx| this.close_session(id, window, cx)));
                        } else {
                            menu = menu.item(menu_item("Disconnect", "unplug", &ws, move |this, window, cx| this.close_session(id, window, cx)));
                        }
                    }
                    return menu;
                }
                let (open, another) = (item.clone(), item.clone());
                menu = menu
                    .item(menu_item(if live { "Show session" } else { "Connect" }, "plug", &ws, move |this, window, cx| {
                        this.activate_item(open.clone(), false, window, cx)
                    }))
                    .item(menu_item("New session", "plus", &ws, move |this, window, cx| this.activate_item(another.clone(), true, window, cx)));
                if let Some(id) = session_id {
                    if live {
                        menu = menu.item(menu_item("Disconnect", "unplug", &ws, move |this, window, cx| this.close_session(id, window, cx)));
                    } else {
                        menu = menu.item(menu_item("Close session", "x", &ws, move |this, window, cx| this.close_session(id, window, cx)));
                    }
                }
                if let SidebarItem::Host { project, host } = &item {
                    let (pa, ha) = (project.id.clone(), host.id.clone());
                    let (pb, hb) = (project.id.clone(), host.id.clone());
                    let hc = host.id.clone();
                    menu = menu
                        .separator()
                        .item(menu_item("Edit host…", "pencil", &ws, move |this, window, cx| this.open_edit_host(pa.clone(), ha.clone(), window, cx)))
                        .item(menu_item("Copy address", "copy", &ws, move |this, _, cx| this.copy_host_address(&pb, &hb, cx)))
                        .separator()
                        .item(menu_item("Delete host…", "trash", &ws, move |this, _, cx| this.confirm_delete_host(hc.clone(), cx)));
                }
                menu
            };
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
                // Also the size of its right-click menu, which inherits it.
                .text_size(px(13.))
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
                        let dropped = this.sessions.iter().find(|s| s.id == id).is_some_and(|s| !s.is_live());
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
                .context_menu(item_menu)
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

    /// The active Session's state, name and address with its actions. Lives in the title
    /// bar, above the terminal.
    fn render_session_strip(&self, p: AppPalette, cx: &mut Context<Self>) -> Option<AnyElement> {
        let entry = self.active_entry()?;
        // Where you are typing: state, the session's name, then user@address and port. The
        // name is repeated from the sidebar, which may be scrolled away from the active host.
        let name = entry.label.clone();
        let target = match &entry.kind {
            SessionKind::Ssh { host_id } => entry
                .project_id
                .as_deref()
                .and_then(|id| self.library.resolve(id, host_id))
                .map(|r| {
                    let user = r.user.clone().map(|u| format!("{u}@")).unwrap_or_default();
                    format!("{user}{}:{}", r.address, r.port)
                })
                .unwrap_or_default(),
            SessionKind::Local { shell } => format!("Local · {}", shell.program),
        };
        let (status_text, status_color) = match &entry.status {
            Status::Connecting => ("Connecting", p.warning),
            Status::Connected => ("Connected", p.success),
            Status::Disconnected(_) => ("Disconnected", p.text_faint),
        };
        let id = entry.id;
        Some(
            div()
                .id("session-strip")
                .min_w_0()
                .flex_1()
                .h_full()
                .pl(px(6.))
                .flex()
                .items_center()
                .gap(px(7.))
                .child(status_dot(status_color))
                .child(
                    div()
                        .id("session-target")
                        .min_w_0()
                        .flex()
                        .items_center()
                        .gap(px(6.))
                        .text_size(px(12.))
                        .tooltip(move |window, cx| Tooltip::new(status_text).build(window, cx))
                        .child(
                            div()
                                .flex_shrink_0()
                                .max_w(px(240.))
                                .truncate()
                                .font_weight(FontWeight::MEDIUM)
                                .text_color(p.text)
                                .child(name),
                        )
                        .child(div().min_w_0().text_color(p.text_muted).truncate().child(target)),
                )
                // The empty middle stays draggable: only the controls above catch the mouse.
                .child(div().flex_1())
                .child(
                    strip_action("copy-text", "copy", "Copy output", false, p)
                        .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.copy_output_text(cx))),
                )
                .child(
                    strip_action("copy-file", "file-text", "Copy as .txt", false, p)
                        .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.copy_output_file(cx))),
                )
                // Set apart from the harmless copy actions: this one disconnects.
                .child(div().w(px(1.)).h(px(14.)).mx(px(4.)).bg(p.border))
                // An unplug, not an ✕: the window's own ✕ is in the same bar.
                .child(
                    strip_action("close-active", "unplug", "Close session", true, p)
                        .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| this.close_session(id, window, cx))),
                )
                // Room between the session's actions and the window buttons.
                .child(div().w(px(16.)).h(px(16.)).mr(px(8.)).border_r_1().border_color(p.border))
                .into_any_element(),
        )
    }

    /// The window's title bar, drawn by Komando so it matches the app: the wordmark over the
    /// sidebar, the active Session's strip over the terminal, and the window buttons. Its
    /// background is the canvas, so on Windows Mica shows through it like everywhere else.
    /// `minimal` (the unlock screen) shows only the window buttons.
    fn render_title_bar(&self, minimal: bool, p: AppPalette, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let strip = match (&self.page, minimal) {
            (Page::Sessions, false) => self.render_session_strip(p, cx),
            _ => None,
        };
        div()
            .id("title-bar")
            .h(px(TITLE_BAR_HEIGHT))
            .w_full()
            .flex_shrink_0()
            .flex()
            .items_center()
            .bg(p.canvas)
            // Windows: the bar is the window's caption, so dragging, double-click to maximise
            // and snapping work as with any window. Buttons inside take their clicks.
            .window_control_area(WindowControlArea::Drag)
            .when(cfg!(not(windows)), |el| {
                el.on_mouse_down(MouseButton::Left, |_, window, _| window.start_window_move())
            })
            .child(
                div()
                    .w(px(SIDEBAR_WIDTH + ISLAND_GAP * 2.))
                    .flex_shrink_0()
                    .h_full()
                    .pl(px(ISLAND_GAP + 18.))
                    .flex()
                    .items_center()
                    .when(!minimal, |el| el.child(wordmark(15., p.text))),
            )
            .child(div().flex_1().min_w_0().h_full().flex().items_center().children(strip))
            .child(window_controls(p, window))
            .into_any_element()
    }

    fn render_main(&self, p: AppPalette, cx: &mut Context<Self>) -> AnyElement {
        let Some(entry) = self.active_entry() else {
            return self.render_empty(p, cx);
        };
        let project = entry.project_id.as_deref().and_then(|id| self.library.project(id));
        let color = project.map(|pr| project_color(&pr.color)).unwrap_or(p.text_faint);
        let term_bg = cx.global::<Appearance>().term.background_hsla();
        island(p)
            .relative()
            .flex_1()
            .min_w_0()
            .h_full()
            .bg(term_bg)
            .overflow_hidden()
            .child(entry.view.clone())
            // Project colour accent: you always see which Project you are typing into. It is
            // the top edge of the island itself, so it follows the rounded corners and tapers
            // off down their curve.
            .child(
                div()
                    .absolute()
                    .inset_0()
                    .rounded(px(ISLAND_RADIUS))
                    .border_t_2()
                    .border_color(color),
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
            Overlay::NewHost { project_id, editing, fields, icon, icon_query, auth, error } => {
                let project = self.library.project(project_id);
                let project_name = project.map(|pr| pr.name.clone()).unwrap_or_default();
                let project_icon = project.map(|pr| pr.icon.clone()).unwrap_or_else(|| "folder".into());
                let project_has_credential = project.is_some_and(|pr| pr.credential.is_some());
                let color = project.map(|pr| project_color(&pr.color)).unwrap_or(p.brand);
                let host = editing.as_deref().and_then(|id| self.library.host(id));
                let has_saved = host.is_some_and(|h| h.credential.is_some());
                let title = match host {
                    Some(h) => format!("Edit {}", h.name),
                    None => "Add host".to_string(),
                };
                let ok_label = if editing.is_some() { "Save" } else { "Add host" };
                let auth = *auth;

                // Sign-in: what the host uses to log in. The labels follow what "no credential
                // of its own" means for this Project.
                let inherit_label = if project_has_credential { "Project credential" } else { "Password" };
                let mut choices: Vec<(&'static str, HostAuth)> = vec![];
                if has_saved {
                    choices.push(("Saved key", HostAuth::Saved));
                }
                choices.push((inherit_label, HostAuth::Inherit));
                choices.push(("Key file", HostAuth::KeyFile));
                let options: Vec<(&'static str, bool)> = choices.iter().map(|(l, a)| (*l, *a == auth)).collect();
                let picks: Vec<HostAuth> = choices.iter().map(|(_, a)| *a).collect();
                let auth_choice = segmented("host-auth", &options, p, cx, move |this, i, _, cx| {
                    if let Some(Overlay::NewHost { auth, error, .. }) = this.overlay.as_mut() {
                        *auth = picks[i];
                        *error = None;
                    }
                    cx.notify();
                });
                let auth_hint = match auth {
                    HostAuth::Saved => "Keeps the key or password already saved in the vault for this host.",
                    HostAuth::Inherit if project_has_credential => {
                        "Uses the key or password saved for the project, shared by all its hosts."
                    }
                    HostAuth::Inherit => "Asked when you connect. Once it works, Komando offers to save it in the vault.",
                    HostAuth::KeyFile => "The key is copied into the vault, encrypted. The file itself isn't needed afterwards.",
                };
                let key_row = (auth == HostAuth::KeyFile).then(|| {
                    div()
                        .flex()
                        .gap(px(8.))
                        .child(div().flex_1().min_w_0().child(Input::new(&fields[4])))
                        .child(
                            button("nh-browse", "Browse…", ButtonKind::Secondary, p)
                                .h(px(32.))
                                .flex_shrink_0()
                                .on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.browse_key_file(window, cx))),
                        )
                });

                let selected = icon.clone();
                let body = div()
                    .flex()
                    .flex_col()
                    .gap(px(20.))
                    .child(
                        form_section("Connection", p)
                            .child(form_field("Name", None, Input::new(&fields[0]).into_any_element(), p))
                            .child(
                                div()
                                    .flex()
                                    .gap(px(10.))
                                    .child(div().flex_1().min_w_0().child(form_field(
                                        "Address",
                                        None,
                                        Input::new(&fields[1]).into_any_element(),
                                        p,
                                    )))
                                    .child(div().w(px(92.)).flex_shrink_0().child(form_field(
                                        "Port",
                                        None,
                                        Input::new(&fields[3]).into_any_element(),
                                        p,
                                    ))),
                            )
                            .child(form_field(
                                "User",
                                Some("Leave empty to use the project's default user."),
                                Input::new(&fields[2]).into_any_element(),
                                p,
                            )),
                    )
                    .child(
                        form_section("Sign-in", p)
                            .child(div().flex().child(auth_choice))
                            .children(key_row)
                            .child(div().text_size(px(12.)).line_height(px(17.)).text_color(p.text_muted).child(auth_hint)),
                    )
                    .child(form_section("Icon", p).child(icon_picker(icon_query, &selected, color, p, cx, |this, name| {
                        if let Some(Overlay::NewHost { icon, .. }) = this.overlay.as_mut() {
                            *icon = name;
                        }
                    })))
                    .when_some(error.clone(), |el, e| {
                        el.child(
                            div()
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
                                .child(e),
                        )
                    });

                div()
                    .w(px(520.))
                    .flex_1()
                    .min_h_0()
                    .flex()
                    .flex_col()
                    // Header: which project the host belongs to.
                    .child(
                        div()
                            .px(px(24.))
                            .pt(px(22.))
                            .pb(px(18.))
                            .flex()
                            .items_center()
                            .gap(px(12.))
                            .child(badge(&project_icon, color, 34.))
                            .child(
                                div()
                                    .flex()
                                    .flex_col()
                                    .gap(px(2.))
                                    .child(div().text_size(px(15.)).font_weight(FontWeight::SEMIBOLD).text_color(p.text).child(title))
                                    .child(
                                        div()
                                            .text_size(px(12.5))
                                            .text_color(p.text_muted)
                                            .child(format!("SSH host in {project_name}")),
                                    ),
                            ),
                    )
                    // Only the middle scrolls; header and footer (with the buttons) stay put.
                    .child(div().id("host-form-body").flex_1().min_h_0().overflow_y_scroll().px(px(24.)).pb(px(22.)).child(body))
                    // Footer.
                    .child(
                        div()
                            .px(px(24.))
                            .py(px(14.))
                            .flex()
                            .items_center()
                            .gap(px(8.))
                            .border_t_1()
                            .border_color(p.border)
                            .child(div().flex_1().text_size(px(11.5)).text_color(p.text_faint).child("Enter to save · Esc to cancel"))
                            .child(
                                button("nh-cancel", "Cancel", ButtonKind::Ghost, p)
                                    .on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.close_overlay(window, cx))),
                            )
                            .child(
                                button("nh-ok", ok_label, ButtonKind::Primary, p)
                                    .on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.submit_new_host(window, cx))),
                            ),
                    )
                    .into_any_element()
            }
            Overlay::NewProject { editing, name, color, icon, icon_query, error } => {
                let (title, ok_label) = if editing.is_some() { ("Edit project", "Save") } else { ("New project", "Create project") };
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
                    title,
                    div()
                        .flex()
                        .flex_col()
                        .gap(px(12.))
                        .child(Input::new(name))
                        .child(section_label("Color", p))
                        .child(swatches)
                        .child(section_label("Icon", p))
                        .child(icon_picker(icon_query, &selected_icon, project_color(&current), p, cx, |this, name| {
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
                        button("np-ok", ok_label, ButtonKind::Primary, p)
                            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.submit_new_project(cx)))
                            .into_any_element(),
                    ],
                )
            }
            Overlay::Confirm { title, body, .. } => form_card(
                p,
                title,
                div().text_size(px(13.)).line_height(px(20.)).text_color(p.text_muted).child(body.clone()).into_any_element(),
                vec![
                    button("confirm-cancel", "Cancel", ButtonKind::Ghost, p)
                        .on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.close_overlay(window, cx)))
                        .into_any_element(),
                    button("confirm-ok", "Delete", ButtonKind::Danger, p)
                        .on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.run_confirm(window, cx)))
                        .into_any_element(),
                ],
            ),
        };
        Some(
            div()
                .id("overlay-backdrop")
                .absolute()
                // Clicks stop here: without this the terminal underneath also gets them and
                // takes the keyboard focus away from the dialog's fields.
                .occlude()
                // Below the title bar, so the window can still be moved and closed.
                .top(px(TITLE_BAR_HEIGHT))
                .left_0()
                .right_0()
                .bottom_0()
                .flex()
                .items_start()
                .justify_center()
                .pt(px(56.))
                .pb(px(24.))
                .bg(alpha(0x000000, if p.dark { 0.5 } else { 0.2 }))
                .on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.close_overlay(window, cx)))
                .child(
                    div()
                        .id("overlay-card")
                        .h_auto()
                        // As tall as the window allows; a taller form scrolls its own middle.
                        .max_h_full()
                        .flex()
                        .flex_col()
                        .rounded(px(14.))
                        .bg(p.surface)
                        .border_1()
                        .border_color(p.border)
                        .shadow(elevation(p.dark))
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
    switch_with(id, on, false, p)
}

/// A smaller, neutral switch for long lists (single highlight rules), so the brand-coloured
/// switches of the settings that matter more stand out.
fn small_switch(id: SharedString, on: bool, p: AppPalette) -> gpui::Stateful<gpui::Div> {
    switch_with(id, on, true, p)
}

fn switch_with(id: SharedString, on: bool, small: bool, p: AppPalette) -> gpui::Stateful<gpui::Div> {
    let (w, h, knob) = if small { (26., 14., 10.) } else { (32., 18., 14.) };
    // The off track has to be visible on its own, not only through the knob's position.
    let off = if p.dark { alpha(0xffffff, 0.16) } else { alpha(0x000000, 0.2) };
    let on_color = if small { p.text_muted.opacity(0.6) } else { p.brand };
    div()
        .id(id)
        .w(px(w))
        .h(px(h))
        .flex_shrink_0()
        .p(px(2.))
        .rounded_full()
        .cursor_pointer()
        .bg(if on { on_color } else { off })
        .flex()
        .when(on, |el| el.justify_end())
        .child(div().size(px(knob)).rounded_full().bg(gpui::white()).shadow(elevation(false)))
}

/// Every icon name in the bundled Lucide set ("server", "database-backup", …), sorted.
fn all_icons() -> &'static [String] {
    static ICONS: std::sync::OnceLock<Vec<String>> = std::sync::OnceLock::new();
    ICONS.get_or_init(|| {
        use gpui::AssetSource as _;
        let mut names: Vec<String> = gpui_kit_assets::AllAssets
            .list("icons/")
            .unwrap_or_default()
            .iter()
            .filter_map(|path| path.strip_prefix("icons/")?.strip_suffix(".svg").map(str::to_string))
            // Only plain Lucide icons: no sub-folders, no window chrome.
            .filter(|name| !name.contains('/') && !name.starts_with("window-"))
            .collect();
        names.sort();
        names.dedup();
        names
    })
}

/// Shown before anything is typed: icons for servers and the things that run on them.
const SUGGESTED_ICONS: &[&str] = &[
    "folder", "server", "database", "globe", "shield", "hard-drive", "cpu", "network", "cloud", "box", "router",
    "container", "monitor", "laptop", "terminal", "code", "git-branch", "key-round", "lock", "mail", "cloud-cog",
    "server-cog", "database-backup", "rocket", "flame", "activity", "bug", "building-2", "factory", "house",
    "flask-conical", "wrench", "zap", "radio-tower", "satellite", "warehouse",
];

/// Most results shown at once; a more specific search narrows them down.
const ICON_RESULTS: usize = 60;

/// Icons matching `query`: exact name first, then names starting with it, then names with a
/// word starting with it ("backup" finds "database-backup"), then any other match.
fn search_icons(query: &str) -> (Vec<&'static str>, usize) {
    let q = query.trim().to_lowercase().replace(' ', "-");
    if q.is_empty() {
        let all = all_icons();
        let shown = SUGGESTED_ICONS.iter().filter_map(|s| all.iter().find(|n| n.as_str() == *s)).map(|n| n.as_str()).collect();
        return (shown, 0);
    }
    let rank = |name: &str| {
        if name == q {
            Some(0)
        } else if name.starts_with(&q) {
            Some(1)
        } else if name.split('-').any(|word| word.starts_with(&q)) {
            Some(2)
        } else if name.contains(&q) {
            Some(3)
        } else {
            None
        }
    };
    let mut hits: Vec<(u8, &'static str)> =
        all_icons().iter().filter_map(|n| rank(n).map(|r| (r, n.as_str()))).collect();
    hits.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.len().cmp(&b.1.len())).then(a.1.cmp(b.1)));
    let total = hits.len();
    (hits.into_iter().take(ICON_RESULTS).map(|(_, n)| n).collect(), total)
}

/// The icon picker: a search over the whole icon set and a grid of results. The selected icon
/// is always in the grid, first if the search doesn't include it.
fn icon_picker(
    query: &Entity<InputState>,
    selected: &str,
    color: Hsla,
    p: AppPalette,
    cx: &mut Context<Workspace>,
    set: impl Fn(&mut Workspace, String) + Clone + 'static,
) -> impl IntoElement {
    let text = query.read(cx).value().to_string();
    let (mut icons, total) = search_icons(&text);
    let searching = !text.trim().is_empty();
    if !selected.is_empty() && !icons.contains(&selected) && !(searching && total == 0) {
        if let Some(name) = all_icons().iter().find(|n| n.as_str() == selected) {
            icons.insert(0, name.as_str());
        }
    }
    let status = if !searching {
        format!("Suggested · {} icons in total, search to find more", all_icons().len())
    } else if total == 0 {
        format!("No icon matches \"{}\".", text.trim())
    } else if total > icons.len() {
        format!("{} matches, showing the first {}. Keep typing to narrow it down.", total, icons.len())
    } else if total == 1 {
        "1 match".to_string()
    } else {
        format!("{total} matches")
    };
    let grid = div().flex().flex_wrap().gap(px(6.)).children(icons.into_iter().map(|name| {
        let is_selected = name == selected;
        let set = set.clone();
        let owned = name.to_string();
        let tip: SharedString = name.into();
        div()
            .id(SharedString::from(format!("icon-{name}")))
            .size(px(36.))
            .flex()
            .items_center()
            .justify_center()
            .rounded(px(8.))
            .cursor_pointer()
            .border_1()
            .border_color(if is_selected { color } else { p.border })
            .bg(if is_selected { color.opacity(0.14) } else { gpui::transparent_black() })
            .hover(|s| s.bg(p.hover))
            .tooltip(move |window, cx| Tooltip::new(tip.clone()).build(window, cx))
            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                set(this, owned.clone());
                cx.notify();
            }))
            .child(icon_el(name, 16., if is_selected { color } else { p.text_muted }))
    }));
    div()
        .flex()
        .flex_col()
        .gap(px(8.))
        .child(Input::new(query).prefix(icon_el("search", 14., p.text_faint)).cleanable(true))
        // Three rows; more results scroll.
        .child(div().id("icon-grid").max_h(px(126.)).overflow_y_scroll().child(grid))
        .child(div().text_size(px(11.5)).text_color(p.text_faint).child(status))
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

/// A titled group of fields in a form.
fn form_section(title: &str, p: AppPalette) -> gpui::Div {
    div().flex().flex_col().gap(px(10.)).child(section_label(title, p))
}

/// A form field: label above the control, an optional hint below.
fn form_field(label: &str, hint: Option<&str>, control: AnyElement, p: AppPalette) -> AnyElement {
    div()
        .flex()
        .flex_col()
        .gap(px(6.))
        .child(div().text_size(px(12.5)).font_weight(FontWeight::MEDIUM).text_color(p.text).child(label.to_string()))
        .child(control)
        .when_some(hint, |el, hint| el.child(div().text_size(px(12.)).text_color(p.text_muted).child(hint.to_string())))
        .into_any_element()
}

fn modal(p: AppPalette, body: AnyElement) -> AnyElement {
    div()
        .absolute()
        // Clicks must not reach the terminal underneath (it would take the focus).
        .occlude()
        // Below the title bar, so the window can still be moved and closed.
        .top(px(TITLE_BAR_HEIGHT))
        .left_0()
        .right_0()
        .bottom_0()
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

/// Minimise, maximise/restore and close, in the size and order Windows uses. On Windows the
/// buttons are hit-tested by the system (snap layouts on maximise work); elsewhere (Linux dev
/// builds) they act on click.
fn window_controls(p: AppPalette, window: &Window) -> impl IntoElement {
    let maximized = window.is_maximized();
    let button = |id: &'static str, icon: &'static str, area: WindowControlArea, close: bool| {
        let (hover_bg, hover_fg) = if close { (crate::theme::hex(0xc42b1c), gpui::white()) } else { (p.hover, p.text) };
        div()
            .id(id)
            .group(id)
            .w(px(46.))
            .h_full()
            .flex()
            .items_center()
            .justify_center()
            .hover(move |s| s.bg(hover_bg))
            .window_control_area(area)
            .when(cfg!(not(windows)), |el| {
                el.on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation()).on_click(move |_, window, _| match area {
                    WindowControlArea::Min => window.minimize_window(),
                    WindowControlArea::Max => window.zoom_window(),
                    _ => window.remove_window(),
                })
            })
            .child(
                gpui::svg()
                    .path(SharedString::from(format!("icons/{icon}.svg")))
                    .size(px(14.))
                    .text_color(p.text_muted)
                    .group_hover(id, move |s| s.text_color(hover_fg)),
            )
    };
    div()
        .flex()
        .h_full()
        .flex_shrink_0()
        .child(button("window-minimize", "window-minimize", WindowControlArea::Min, false))
        .child(if maximized {
            button("window-restore", "window-restore", WindowControlArea::Max, false)
        } else {
            button("window-maximize", "window-maximize", WindowControlArea::Max, false)
        })
        .child(button("window-close", "window-close", WindowControlArea::Close, true))
}

/// A context-menu entry that runs `run` on the Workspace.
fn menu_item(
    label: &str,
    icon: &str,
    ws: &gpui::WeakEntity<Workspace>,
    run: impl Fn(&mut Workspace, &mut Window, &mut Context<Workspace>) + 'static,
) -> PopupMenuItem {
    let ws = ws.clone();
    PopupMenuItem::new(label.to_string()).icon(Icon::empty().path(format!("icons/{icon}.svg"))).on_click(move |_, window, cx| {
        let _ = ws.update(cx, |this, cx| run(this, window, cx));
    })
}

/// A small icon-only action for the session strip above the terminal.
/// `danger` marks an action that ends something: it turns red on hover.
fn strip_action(id: &'static str, icon: &str, label: &'static str, danger: bool, p: AppPalette) -> gpui::Stateful<gpui::Div> {
    let (hover_bg, hover_fg) = if danger { (p.danger.opacity(0.12), p.danger) } else { (p.hover, p.text) };
    div()
        .id(id)
        .group(id)
        .size(px(22.))
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(6.))
        .cursor_pointer()
        .hover(move |s| s.bg(hover_bg))
        .tooltip(move |window, cx| Tooltip::new(label).build(window, cx))
        .child(
            gpui::svg()
                .path(SharedString::from(format!("icons/{icon}.svg")))
                .size(px(13.))
                .flex_shrink_0()
                .text_color(p.text_faint)
                .group_hover(id, move |s| s.text_color(hover_fg)),
        )
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
        crate::perf::count("workspace render (frame)");
        let p = cx.global::<Appearance>().app;
        let root = div()
            .id("workspace")
            .key_context("Workspace")
            .track_focus(&self.focus)
            .size_full()
            .relative()
            .flex()
            .flex_col()
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
            return root
                .bg(p.canvas)
                .child(self.render_title_bar(true, p, window, cx))
                .child(div().flex_1().min_h_0().child(self.render_unlock(state, p, cx)))
                .into_any_element();
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
        let title_bar = self.render_title_bar(false, p, window, cx);
        let page = match self.page {
            Page::Settings(section) => self.render_settings(section, p, cx),
            Page::Sessions => crate::perf::time("sidebar + main", || div()
                .flex_1()
                .min_h_0()
                .w_full()
                .flex()
                .gap(px(ISLAND_GAP))
                // The title bar above is the top margin.
                .px(px(ISLAND_GAP))
                .pb(px(ISLAND_GAP))
                .bg(p.canvas)
                .child(self.render_sidebar(p, cx))
                .child(self.render_main(p, cx))
                .into_any_element()),
        };
        root.bg(p.canvas)
            .child(title_bar)
            .child(page)
            .children(self.render_overlay(p, cx))
            .children(self.unlock.as_ref().map(|s| self.render_unlock(s, p, cx)))
            .children(toast)
            .into_any_element()
    }
}

#[cfg(test)]
mod icon_search_tests {
    use super::*;

    #[test]
    fn the_full_icon_set_is_searchable() {
        assert!(all_icons().len() > 1000, "{}", all_icons().len());
        assert!(all_icons().iter().all(|n| !n.starts_with("window-")));
        // Every suggested icon exists.
        let (suggested, _) = search_icons("");
        assert_eq!(suggested.len(), SUGGESTED_ICONS.len());
    }

    #[test]
    fn exact_and_prefix_matches_come_first() {
        let (hits, total) = search_icons("database");
        assert_eq!(hits[0], "database");
        assert!(total >= hits.len());
        assert!(hits.iter().take(5).all(|n| n.starts_with("database")));
        // A word inside a name matches too.
        let (hits, _) = search_icons("backup");
        assert!(hits.contains(&"database-backup"));
        // Spaces act like the dashes in icon names.
        let (hits, _) = search_icons("hard drive");
        assert_eq!(hits[0], "hard-drive");
        let (hits, total) = search_icons("zzzz-no-such-icon");
        assert!(hits.is_empty() && total == 0);
    }
}
