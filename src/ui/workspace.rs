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
use crate::theme::{AppPalette, Appearance, TerminalPalette, UI_FONT, alpha, project_color};
use crate::ui::connect_route::RouteCard;
use crate::ui::file_browser::{BrowserEvent, FavoriteStore, FileBrowser};
use crate::ui::terminal_view::{TerminalEvent, TerminalView};
use crate::ui::transfers::{Transfers, TransfersChanged};
use crate::ui::widgets::{
    ButtonKind, ISLAND_GAP, ISLAND_RADIUS, badge, button, elevation, icon_button, icon_el, icon_text_button, island,
    is_artwork, section_label, shell_icon, status_dot, wordmark,
};
use crate::vault::{Credential, Secret, Vault};

mod forms;
mod settings;
mod updates;
pub use settings::SettingsSection;
use updates::UpdateState;

actions!(
    komando,
    [
        QuickConnect,
        NewSession,
        CloseSession,
        NextSession,
        PrevSession,
        CopyOutputText,
        CopyOutputFile,
        LockVault,
        OpenSettings,
        ToggleSftp
    ]
);

/// What fills the window: the sessions, or the full-window Settings page.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Page {
    Sessions,
    Settings(SettingsSection),
}

const SIDEBAR_WIDTH: f32 = 264.;

/// What is dragged while resizing the file browser (drawn as nothing).
struct SftpDivider;

impl Render for SftpDivider {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
    }
}
/// Height of Komando's own title bar (and of its window buttons).
const TITLE_BAR_HEIGHT: f32 = 40.;
/// The window buttons' rounded shapes, and the room between them.
const WINDOW_BUTTON_WIDTH: f32 = 36.;
const WINDOW_BUTTON_HEIGHT: f32 = 28.;
const WINDOW_BUTTON_GAP: f32 = 2.;

/// Window width from which the host and project forms use two columns side by side: the
/// wide form plus a margin each side. Narrower, they stack in one.
const WIDE_FORM_MIN_WINDOW: f32 = forms::WIDE_FORM_WIDTH + 60.;

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

/// A row of the sidebar list: what it shows, by position in the Library. `sessions` (the
/// Session rows shown under the item) is part of it because they make the row taller.
#[derive(Clone, PartialEq)]
enum SidebarRow {
    Shell { index: usize, sessions: usize },
    ProjectsHeader,
    NoProjects,
    Project { index: usize },
    FirstHost { project: usize },
    Host { project: usize, host: usize, sessions: usize },
    ProjectShell { project: usize, shell: usize, sessions: usize },
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
            Self::Shell { shell, .. } => shell_icon(&shell.program).unwrap_or("square-terminal").into(),
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
    /// Its number among the item's Sessions: the lowest one free when it was opened.
    number: usize,
    /// The terminal's title (set by the shell), copied here like `status`.
    title: SharedString,
    /// Output is streaming in (a command at work), copied here like `status`.
    busy: bool,
    view: Entity<TerminalView>,
    /// The terminal's status, copied here when it changes. The sidebar reads this rather than
    /// the terminal: reading a terminal while rendering makes gpui redraw the whole window on
    /// every bit of output it gets, even when it isn't on screen.
    status: Status,
    /// The SFTP file browser beside the terminal (SSH Sessions). Kept while hidden, so it
    /// opens again where it was.
    browser: Option<Entity<FileBrowser>>,
    sftp_open: bool,
    /// Opened before any terminal pane was laid out: the shell starts once one is, at its size.
    waiting_for_size: bool,
    _subscription: Subscription,
    _browser_subscription: Option<Subscription>,
}

impl SessionEntry {
    fn is_live(&self) -> bool {
        !matches!(self.status, Status::Disconnected(_))
    }
}

enum Overlay {
    Picker { query: Entity<InputState> },
    /// Add a Host to a Project, or edit one.
    HostForm(forms::HostForm),
    /// Create a Project, or edit one.
    ProjectForm(forms::ProjectForm),
    /// Ask before something that can't be undone.
    Confirm { title: String, body: String, action: ConfirmAction },
}

#[derive(Clone)]
enum ConfirmAction {
    DeleteHost(String),
    DeleteProject(String),
}

/// What waits for the Vault: done once the master password is entered.
enum Pending {
    Open { project_id: String, host_id: String },
    Save { session: usize, hop: usize, password: String },
    /// Save the open host or project form, which imports a key file into the Vault.
    Form,
}

/// Things to do right after launch. Used by development tooling (screenshots, demos);
/// the Komando binary itself never passes a master password.
#[derive(Clone, Default)]
pub struct Startup {
    pub master_password: Option<String>,
    /// (Project name, Host or local shell name) pairs to open, in order. A pair listed again
    /// opens another Session on it.
    pub open: Vec<(String, String)>,
    pub active: Option<usize>,
    /// Open the Settings page at this section (used for screenshots).
    pub settings: Option<SettingsSection>,
}

pub struct Workspace {
    data_dir: PathBuf,
    library: Library,
    settings: Settings,
    #[cfg(windows)]
    tray: Option<crate::platform::Tray>,
    highlights: Vec<HighlightSet>,
    /// Rules that could not be compiled, with the reason.
    highlight_skipped: Vec<(String, String)>,
    page: Page,
    highlights_expanded: Option<String>,
    /// Updates from GitHub (sidebar footer and Settings → About).
    update: UpdateState,
    /// The sidebar list (virtual: only rows in view are built) and the rows it was built for.
    sidebar_list: ListState,
    sidebar_rows: Vec<SidebarRow>,
    /// The highlight set whose delete button was clicked once and now asks for a second click.
    confirm_delete_set: Option<(String, Instant)>,
    blur_slider: Option<Entity<SliderState>>,
    font_slider: Option<Entity<SliderState>>,
    font_input: Option<Entity<InputState>>,
    /// Settings → Local shells: the form that adds a shell or edits one.
    shell_editor: Option<settings::ShellEditor>,
    /// A shell whose Remove button was clicked once, waiting for the second click.
    confirm_remove_shell: Option<(String, Instant)>,
    vault: Option<Vault>,
    /// Shown full-window at launch; as a dialog when a locked Vault is needed later.
    unlock: Option<UnlockState>,
    /// What waits for the master password, in the order it was asked for.
    pending: Vec<Pending>,
    sessions: Vec<SessionEntry>,
    active: Option<usize>,
    /// The Session last shown for each item, so a click returns to it.
    last_active: std::collections::HashMap<ItemKey, usize>,
    next_id: usize,
    /// The terminal pane's size, as last laid out. A new Session's shell starts at this size
    /// instead of a guess, so it isn't resized (and repainted) straight after it starts.
    pane_size: Option<session::GridSize>,
    collapsed: HashSet<String>,
    /// Sidebar items whose Sessions are folded away under the item's row.
    collapsed_items: HashSet<ItemKey>,
    overlay: Option<Overlay>,
    toast: Option<(SharedString, Instant)>,
    last_activity: Instant,
    focus: FocusHandle,
    /// Downloads and uploads from every file browser (the sidebar's Downloads entry).
    transfers: Entity<Transfers>,
    favorites: Entity<FavoriteStore>,
    /// The divider between terminal and file browser is being dragged.
    sftp_resizing: bool,
    _subscriptions: Vec<Subscription>,
}

/// The master password dialog. It opens only when something needs the Vault (connecting
/// with a saved password, saving one, importing a key), never at startup.
struct UnlockState {
    creating: bool,
    password: Entity<InputState>,
    confirm: Entity<InputState>,
    error: Option<String>,
    /// Why it's asked, in a sentence: "web01 signs in with a password saved in your vault."
    reason: String,
}

impl Focusable for Workspace {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Workspace {
    pub fn new(data_dir: PathBuf, startup: Startup, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let settings = Settings::load(&data_dir);
        // A fresh install has no Projects; the detected local shells stand alone.
        let library = Library::load(&data_dir).unwrap_or_else(|err| {
            log::error!("{err:#}");
            Library::default()
        });
        let highlights = highlight::load(&data_dir);
        let transfers = cx.new(|_| Transfers::new());
        let favorites = cx.new(|_| FavoriteStore::new(data_dir.clone()));
        let transfers_sub = cx.subscribe(&transfers, |_, _, _: &TransfersChanged, cx| cx.notify());
        let update = crate::update::take_install_error(&data_dir).map(UpdateState::Failed).unwrap_or(UpdateState::Idle);
        let mut this = Self {
            data_dir,
            library,
            #[cfg(windows)]
            tray: crate::platform::Tray::new(window, settings.minimize_to_tray),
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
            shell_editor: None,
            confirm_remove_shell: None,
            vault: None,
            unlock: None,
            pending: vec![],
            sessions: vec![],
            active: None,
            last_active: Default::default(),
            next_id: 1,
            pane_size: None,
            collapsed: HashSet::new(),
            collapsed_items: HashSet::new(),
            overlay: None,
            toast: None,
            last_activity: Instant::now(),
            focus: cx.focus_handle(),
            update,
            transfers,
            favorites,
            sftp_resizing: false,
            _subscriptions: vec![transfers_sub],
        };
        this.apply_appearance(window, cx);
        this.apply_highlights(cx);
        let sub = cx.observe_window_appearance(window, |this, window, cx| this.apply_appearance(window, cx));
        this._subscriptions.push(sub);
        // The Vault stays locked until something needs it; then the master password is asked.
        if let Some(password) = &startup.master_password {
            match Vault::unlock(&this.data_dir, password) {
                Ok(vault) => this.vault = Some(vault),
                Err(err) => log::error!("startup unlock: {err:#}"),
            }
        }
        this.start_auto_lock(window, cx);
        // Look for a new version shortly after start, without getting in the way of it.
        if this.settings.check_for_updates && !matches!(this.update, UpdateState::Failed(_)) {
            cx.spawn(async move |this, cx| {
                cx.background_executor().timer(Duration::from_secs(3)).await;
                let _ = this.update(cx, |this, cx| this.check_for_updates(true, cx));
            })
            .detach();
        }
        if !startup.open.is_empty() {
            cx.defer_in(window, move |this, window, cx| this.run_startup(&startup, window, cx));
        }
        this
    }

    fn run_startup(&mut self, startup: &Startup, window: &mut Window, cx: &mut Context<Self>) {
        // Resolve the library once, even when opening many Sessions together.
        let items = self.sidebar_items();
        // An empty project name means a standalone Local Shell.
        for (project_name, name) in &startup.open {
            let item = items.iter().find(|item| {
                let project = match item {
                    SidebarItem::Host { project, .. } => Some(project.name.as_str()),
                    SidebarItem::Shell { project, .. } => project.as_ref().map(|p| p.name.as_str()),
                };
                project.unwrap_or("") == project_name && item.label() == name
            }).cloned();
            // Listed again: another Session on the same item.
            if let Some(item) = item {
                let another = self.sessions_of(&item.key()).next().is_some();
                self.activate_item(item, another, window, cx);
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
        window.set_background_appearance(if blur {
            gpui::WindowBackgroundAppearance::MicaBackdrop
        } else {
            gpui::WindowBackgroundAppearance::Opaque
        });
        crate::platform::set_window_dark_mode(window, app_dark);
        let backdrop_enabled = crate::platform::set_backdrop(window, blur);
        if blur && !backdrop_enabled {
            window.set_background_appearance(gpui::WindowBackgroundAppearance::Opaque);
        }
        let t = if backdrop_enabled { self.settings.background_blur_intensity as f32 / 100. } else { 0. };
        cx.set_global(Appearance {
            app: AppPalette::from_theme(self.theme_for(app_dark)).translucent(t),
            term: TerminalPalette::from_theme(self.theme_for(term_dark)).translucent(t),
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
            // gpui-component's Root paints this under the whole window. Komando paints its
            // own canvas (translucent with the background blur on); an opaque colour here
            // would hide the blurred backdrop behind it.
            theme.colors.background = gpui::transparent_black();
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

    /// The Theme picked for dark or light mode.
    fn theme_for(&self, dark: bool) -> &'static crate::theme::ThemeSpec {
        crate::theme::theme(dark, if dark { &self.settings.dark_theme } else { &self.settings.light_theme })
    }

    fn set_theme(&mut self, dark: bool, id: &'static str, window: &mut Window, cx: &mut Context<Self>) {
        if dark {
            self.settings.dark_theme = id.into();
        } else {
            self.settings.light_theme = id.into();
        }
        let _ = self.settings.save(&self.data_dir);
        self.apply_appearance(window, cx);
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

    /// Ask for the master password, saying why (`reason`). What waits for it is in `pending`.
    /// Asked again while the dialog is open (two hosts opened at once), it stays as it is.
    fn show_unlock(&mut self, reason: impl Into<String>, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(state) = self.unlock.as_mut() {
            if self.pending.len() > 1 {
                state.reason = "The hosts you're opening sign in with passwords or keys saved in your vault.".into();
            }
            let password = state.password.clone();
            password.update(cx, |input, cx| input.focus(window, cx));
            cx.notify();
            return;
        }
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
        self.unlock = Some(UnlockState { creating, password, confirm, error: None, reason: reason.into() });
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
                let pending = std::mem::take(&mut self.pending);
                if pending.is_empty() {
                    self.focus_active(window, cx);
                }
                for job in pending {
                    match job {
                        Pending::Open { project_id, host_id } => self.open_host(&project_id, &host_id, window, cx),
                        Pending::Save { session, hop, password } => self.save_password(session, hop, password, cx),
                        Pending::Form => self.submit_form(window, cx),
                    }
                }
            }
            Err(err) => state.error = Some(err.to_string()),
        }
        cx.notify();
    }

    /// "Not now" (or Escape) on the master password dialog. A connection still goes ahead
    /// and asks for the server's password instead; anything else is dropped.
    fn skip_unlock(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.unlock = None;
        for job in std::mem::take(&mut self.pending) {
            match job {
                Pending::Open { project_id, host_id } => self.open_host_session(&project_id, &host_id, window, cx),
                Pending::Form => self.show_form_error("The key file goes into the vault: unlock it, or pick another sign-in.", cx),
                Pending::Save { .. } => {}
            }
        }
        if self.overlay.is_none() {
            self.focus_active(window, cx);
        }
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
            .chain(self.orphan_shells())
            .map(|shell| SidebarItem::Shell { project: None, shell })
            .collect();
        let hosts = self.library.host_indices();
        for project in &self.library.projects {
            items.extend(project.hosts.iter().filter_map(|id| hosts.get(id.as_str())).map(|&index| {
                SidebarItem::Host { project: project.clone(), host: self.library.hosts[index].clone() }
            }));
            items.extend(project.shells.iter().map(|shell| SidebarItem::Shell { project: Some(project.clone()), shell: shell.clone() }));
        }
        items
    }

    /// Standalone shells removed in Settings whose Sessions are still open, so those Sessions
    /// stay reachable until they are closed.
    fn orphan_shells(&self) -> Vec<LocalShell> {
        let saved = self.library.standalone_shells();
        let mut orphans: Vec<LocalShell> = vec![];
        for s in &self.sessions {
            if let (ItemKey::Shell { project: None, name }, SessionKind::Local { shell }) = (&s.item, &s.kind)
                && !saved.iter().any(|saved| saved.name == *name)
                && !orphans.iter().any(|o| o.name == *name)
            {
                orphans.push(LocalShell { name: name.clone(), ..shell.clone() });
            }
        }
        orphans
    }

    /// The standalone shells the sidebar lists: those not hidden in Settings, plus any with
    /// Sessions open.
    fn sidebar_shells(&self) -> Vec<LocalShell> {
        self.library
            .standalone_shells()
            .into_iter()
            .chain(self.orphan_shells())
            .filter(|shell| {
                !shell.hidden || self.sessions_of(&ItemKey::Shell { project: None, name: shell.name.clone() }).next().is_some()
            })
            .collect()
    }

    /// How many Session rows show under an item: one each when it has several and they are
    /// not folded away.
    fn nested_rows(&self, key: &ItemKey, sessions: usize) -> usize {
        if sessions > 1 && !self.collapsed_items.contains(key) { sessions } else { 0 }
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
        let taken: HashSet<usize> = self.sessions_of(&item).map(|s| s.number).collect();
        let number = (1..).find(|n| !taken.contains(n)).unwrap_or(1);
        let label: SharedString = if number == 1 { base_label.to_string().into() } else { format!("{base_label} ({number})").into() };
        // A new Session shows under its item even if the item's Sessions were folded away.
        self.collapsed_items.remove(&item);
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
                    entry.title = view.read(cx).title.clone();
                    entry.busy = view.read(cx).busy;
                    let changed = entry.status != status;
                    entry.status = status.clone();
                    // The file browser follows its terminal's connection.
                    if changed && let Some(browser) = entry.browser.clone() {
                        match status {
                            Status::Connected if entry.sftp_open => {
                                let sink = view.read(cx).sink();
                                browser.update(cx, |b, cx| b.connect(sink, cx));
                            }
                            Status::Disconnected(_) => browser.update(cx, |b, cx| b.disconnected(cx)),
                            _ => {}
                        }
                    }
                }
                cx.notify();
            }
            TerminalEvent::Remember { hop, what } => match what {
                session::Remembered::User(user) => this.save_user(id, *hop, user.clone(), cx),
                session::Remembered::Password(password) => {
                    if this.vault.is_some() {
                        this.save_password(id, *hop, password.clone(), cx);
                    } else {
                        this.pending.push(Pending::Save { session: id, hop: *hop, password: password.clone() });
                        let host = this.hop_host(id, *hop).and_then(|(_, host_id)| this.library.host(&host_id).map(|h| h.name.clone()));
                        let reason = match host {
                            Some(name) => format!("To remember the password for {name}, Komando saves it in your vault."),
                            None => "To remember this password, Komando saves it in your vault.".to_string(),
                        };
                        this.show_unlock(reason, window, cx);
                    }
                }
            },
            TerminalEvent::Reconnect => this.reconnect(id, window, cx),
            TerminalEvent::Sized => {
                this.pane_size = view.read(cx).measured_size().or(this.pane_size);
                this.start_waiting(cx);
            }
            TerminalEvent::EditHost { hop } => {
                if let Some((project_id, host_id)) = this.hop_host(id, *hop) {
                    this.open_edit_host(project_id, host_id, window, cx);
                }
            }
        });
        if let Some(project_id) = &project_id {
            self.collapsed.remove(project_id);
        }
        self.sessions.push(SessionEntry {
            id,
            item,
            project_id,
            kind,
            label: label.clone(),
            number,
            title: label,
            busy: false,
            view: view.clone(),
            status: Status::Connecting,
            browser: None,
            sftp_open: false,
            waiting_for_size: false,
            _subscription: sub,
            _browser_subscription: None,
        });
        self.activate(id, window, cx);
        view
    }

    fn open_local(&mut self, project_id: Option<String>, shell: LocalShell, window: &mut Window, cx: &mut Context<Self>) {
        let item = ItemKey::Shell { project: project_id.clone(), name: shell.name.clone() };
        let kind = SessionKind::Local { shell: shell.clone() };
        let view = self.add_session(item, project_id.clone(), kind.clone(), &shell.name, window, cx);
        self.start_when_sized(&view, cx);
    }

    /// Open a new Session for a Host. If any Host on the way has a saved Credential, the
    /// Vault is unlocked first (or skipped, and passwords are asked instead).
    fn open_host(&mut self, project_id: &str, host_id: &str, window: &mut Window, cx: &mut Context<Self>) {
        let needs_vault = self
            .library
            .route(project_id, host_id)
            .is_ok_and(|route| route.iter().any(|hop| hop.credential.is_some()));
        if needs_vault && self.vault.is_none() {
            self.pending.push(Pending::Open { project_id: project_id.into(), host_id: host_id.into() });
            let name = self.library.host(host_id).map(|h| h.name.clone()).unwrap_or_else(|| "This host".into());
            self.show_unlock(format!("{name} signs in with a password or key saved in your vault."), window, cx);
            return;
        }
        self.open_host_session(project_id, host_id, window, cx);
    }

    fn open_host_session(&mut self, project_id: &str, host_id: &str, window: &mut Window, cx: &mut Context<Self>) {
        let Some(resolved) = self.library.resolve(project_id, host_id) else { return };
        let item = ItemKey::Host { project: project_id.to_string(), host: host_id.to_string() };
        let kind = SessionKind::Ssh { host_id: host_id.to_string() };
        let view = self.add_session(item, Some(project_id.to_string()), kind.clone(), &resolved.name, window, cx);
        self.start_when_sized(&view, cx);
    }

    /// Start a new Session's backend at the terminal pane's size: the size of the pane last
    /// laid out, or, before any was, once this one is. A shell started at a guessed size and
    /// resized as soon as the pane appears repaints, and Windows' pseudo-console leaves blank
    /// lines above the prompt when it does.
    fn start_when_sized(&mut self, view: &Entity<TerminalView>, cx: &mut Context<Self>) {
        let Some(entry) = self.sessions.iter_mut().find(|s| &s.view == view) else { return };
        let size = view.read(cx).measured_size().or(self.pane_size);
        match size {
            Some(size) => {
                view.update(cx, |v, _| v.assume_size(size));
                let (kind, project_id) = (entry.kind.clone(), entry.project_id.clone());
                self.start_backend(view, &kind, project_id.as_deref(), cx);
            }
            None => {
                entry.waiting_for_size = true;
                // In case no pane gets laid out (the window is hidden): start at a guess.
                cx.spawn(async move |this, cx| {
                    cx.background_executor().timer(Duration::from_millis(500)).await;
                    let _ = this.update(cx, |this, cx| this.start_waiting(cx));
                })
                .detach();
            }
        }
    }

    /// Start the Sessions that were waiting for a terminal pane to be laid out.
    fn start_waiting(&mut self, cx: &mut Context<Self>) {
        let waiting: Vec<_> = self
            .sessions
            .iter_mut()
            .filter_map(|s| std::mem::take(&mut s.waiting_for_size).then(|| (s.view.clone(), s.kind.clone(), s.project_id.clone())))
            .collect();
        for (view, kind, project_id) in waiting {
            if let Some(size) = self.pane_size {
                view.update(cx, |v, _| v.assume_size(size));
            }
            self.start_backend(&view, &kind, project_id.as_deref(), cx);
        }
    }

    fn start_backend(&mut self, view: &Entity<TerminalView>, kind: &SessionKind, project_id: Option<&str>, cx: &mut Context<Self>) {
        let (term, proxy, size) = {
            let v = view.read(cx);
            (v.term(), v.proxy(), v.grid_size())
        };
        let sink = match kind {
            SessionKind::Local { shell } => {
                view.update(cx, |v, _| v.begin_connect(None, vec![]));
                match session::local::spawn(shell, size, term, proxy.clone()) {
                    Ok(sink) => {
                        proxy.send(session::SessionEvent::Status(Status::Connected));
                        Some(sink)
                    }
                    Err(err) => {
                        proxy.send(session::SessionEvent::Status(Status::Disconnected(format!("{err:#}"))));
                        None
                    }
                }
            }
            SessionKind::Ssh { host_id } => {
                let route = match project_id.map(|p| self.library.route(p, host_id)) {
                    Some(Ok(route)) => route,
                    Some(Err(err)) => {
                        proxy.send(session::SessionEvent::Status(Status::Disconnected(err.to_string())));
                        return;
                    }
                    None => return,
                };
                let cards = (route.len() > 1).then(|| route.iter().map(route_card).collect());
                let startup = route.last().map(|target| target.startup_commands.clone()).unwrap_or_default();
                view.update(cx, |v, _| v.begin_connect(cards, startup));
                let hops = route
                    .into_iter()
                    .map(|hop| ssh::Hop {
                        secret: hop.credential.as_deref().and_then(|id| self.vault.as_ref()?.get(id)).map(|c| c.secret.clone()),
                        name: hop.name,
                        address: hop.address,
                        port: hop.port,
                        user: hop.user,
                    })
                    .collect();
                let target = ssh::Target { hops, known_hosts: self.data_dir.join("known_hosts") };
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
        view.update(cx, |v, _| v.close());
        self.start_backend(&view, &kind, project_id.as_deref(), cx);
        self.focus_active(window, cx);
    }

    /// The Host of one hop of a Session's route: (Project id, Host id).
    fn hop_host(&self, session_id: usize, hop: usize) -> Option<(String, String)> {
        let entry = self.sessions.iter().find(|s| s.id == session_id)?;
        let SessionKind::Ssh { host_id } = &entry.kind else { return None };
        let route = self.library.route(entry.project_id.as_deref()?, host_id).ok()?;
        let resolved = route.get(hop)?;
        Some((resolved.project_id.clone(), resolved.host_id.clone()))
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

    /// Show or hide the SFTP file browser beside an SSH Session's terminal.
    fn toggle_sftp(&mut self, id: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(index) = self.sessions.iter().position(|s| s.id == id) else { return };
        let SessionKind::Ssh { host_id } = self.sessions[index].kind.clone() else { return };
        if self.sessions[index].sftp_open {
            self.sessions[index].sftp_open = false;
            self.focus_active(window, cx);
            cx.notify();
            return;
        }
        // Not connected (yet): nothing to browse. The title bar's button says so.
        if !matches!(self.sessions[index].status, Status::Connected) {
            return;
        }
        if self.sessions[index].browser.is_none() {
            let entry = &self.sessions[index];
            let (label, target) = match entry.project_id.as_deref().and_then(|p| self.library.resolve(p, &host_id)) {
                Some(r) => {
                    let user = r.user.clone().map(|u| format!("{u}@")).unwrap_or_default();
                    (r.name.clone(), format!("{user}{}:{}", r.address, r.port))
                }
                None => (entry.label.to_string(), String::new()),
            };
            let (favorites, transfers, show_hidden) = (self.favorites.clone(), self.transfers.clone(), self.settings.sftp_show_hidden);
            let browser = cx.new(|cx| {
                FileBrowser::new(host_id.clone(), label.into(), target.into(), favorites, transfers, show_hidden, window, cx)
            });
            let sub = cx.subscribe_in(&browser, window, move |this, browser, event: &BrowserEvent, window, cx| match event {
                BrowserEvent::Close => this.toggle_sftp(id, window, cx),
                BrowserEvent::ShowHidden(show) => {
                    this.settings.sftp_show_hidden = *show;
                    let _ = this.settings.save(&this.data_dir);
                }
                BrowserEvent::Retry => {
                    let sink = this.sessions.iter().find(|s| s.id == id).and_then(|s| s.view.read(cx).sink());
                    browser.update(cx, |b, cx| b.connect(sink, cx));
                }
            });
            self.sessions[index].browser = Some(browser);
            self.sessions[index]._browser_subscription = Some(sub);
        }
        let entry = &mut self.sessions[index];
        entry.sftp_open = true;
        let browser = entry.browser.clone().expect("browser");
        if matches!(entry.status, Status::Connected) && !browser.read(cx).is_connected() {
            let sink = entry.view.read(cx).sink();
            browser.update(cx, |b, cx| b.connect(sink, cx));
        }
        let handle = browser.read(cx).focus_handle(cx);
        window.focus(&handle, cx);
        cx.notify();
    }

    /// Drag the divider between terminal and file browser: the browser takes what is right
    /// of the pointer.
    fn resize_sftp(&mut self, x: f32, window: &mut Window, cx: &mut Context<Self>) {
        let window_width = f32::from(window.bounds().size.width);
        let right_edge = window_width - ISLAND_GAP - 8.;
        let max = (window_width - SIDEBAR_WIDTH - ISLAND_GAP * 3. - 300.).max(320.);
        let width = (right_edge - x).clamp(320., max);
        if (width - self.settings.sftp_width).abs() >= 1. {
            self.settings.sftp_width = width;
            cx.notify();
        }
    }

    fn focus_active(&self, window: &mut Window, cx: &mut Context<Self>) {
        // The master password dialog sits over everything: it keeps the keyboard.
        if let Some(unlock) = &self.unlock {
            unlock.password.update(cx, |input, cx| input.focus(window, cx));
        } else if let Some(entry) = self.active_entry() {
            // The open question's field if it has one, else the terminal.
            entry.view.update(cx, |view, cx| view.focus_prompt(window, cx));
        } else {
            window.focus(&self.focus, cx);
        }
    }

    fn active_entry(&self) -> Option<&SessionEntry> {
        self.sessions.iter().find(|s| Some(s.id) == self.active)
    }

    /// Whether a question is open: the active Session's (a password, a host key…) or the
    /// master password. It is a modal: nothing else in the window can be used, and no other
    /// dialog opens, until it is answered.
    fn prompt_open(&self, cx: &App) -> bool {
        self.unlock.is_some() || self.active_entry().is_some_and(|entry| entry.view.read(cx).has_prompt())
    }

    /// Whether a dialog is up, so the window's shortcuts and other dialogs must wait.
    fn modal_open(&self, cx: &App) -> bool {
        self.overlay.is_some() || self.prompt_open(cx)
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
                // Stay with the item: show another of its Sessions, if it has one.
                let next = self.sessions_of(&entry.item).next().map(|s| s.id);
                if let Some(next) = next {
                    self.activate(next, window, cx);
                }
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
        match key {
            ItemKey::Host { project, host } => {
                let project = self.library.project(&project)?;
                if !project.hosts.contains(&host) { return None; }
                Some(SidebarItem::Host { project: project.clone(), host: self.library.host(&host)?.clone() })
            }
            ItemKey::Shell { project: Some(id), name } => {
                let project = self.library.project(&id)?;
                let shell = project.shells.iter().find(|sh| sh.name == name)?.clone();
                Some(SidebarItem::Shell { project: Some(project.clone()), shell })
            }
            ItemKey::Shell { project: None, name } => self.library.standalone_shells().into_iter()
                .chain(self.orphan_shells()).find(|sh| sh.name == name)
                .map(|shell| SidebarItem::Shell { project: None, shell }),
        }
    }

    // ---- passwords & output -----------------------------------------------------------

    /// Save a user name typed for one hop of a Session as that hop's Host's own.
    fn save_user(&mut self, session_id: usize, hop: usize, user: String, cx: &mut Context<Self>) {
        let Some((_, host_id)) = self.hop_host(session_id, hop) else { return };
        let Some(host) = self.library.hosts.iter_mut().find(|h| h.id == host_id) else { return };
        host.user = model::Inherited::Own(user);
        match self.library.save(&self.data_dir) {
            Ok(_) => self.show_toast("User name saved for this host.", cx),
            Err(err) => self.show_toast(format!("Could not save: {err}"), cx),
        }
    }

    /// Save a password typed for one hop of a Session for that hop's Host.
    fn save_password(&mut self, session_id: usize, hop: usize, password: String, cx: &mut Context<Self>) {
        let Some((project_id, host_id)) = self.hop_host(session_id, hop) else { return };
        let Some(resolved) = self.library.resolve(&project_id, &host_id) else { return };
        let Some(vault) = self.vault.as_mut() else { return };
        let label = format!("{}@{}", resolved.user.clone().unwrap_or_default(), resolved.name);
        let credential = Credential { label, secret: Secret::Password { password } };

        // Replace the secret in place when the Host already has its own Credential, so every
        // Host sharing it picks up the new password.
        let existing = match resolved.credential_source {
            CredentialSource::Host => resolved.credential.clone(),
            _ => None,
        };
        let result = match existing {
            Some(id) => vault.replace(&id, credential).map(|_| id),
            None => vault.add(credential),
        };
        match result {
            Ok(cred_id) => {
                if let Some(h) = self.library.hosts.iter_mut().find(|h| h.id == host_id) {
                    h.credential = model::Inherited::Own(cred_id);
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
        if self.prompt_open(cx) {
            return;
        }
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

    /// The search box of a form's icon picker.
    fn new_icon_query(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Entity<InputState> {
        let query = cx.new(|cx| InputState::new(window, cx).placeholder("Search icons"));
        let sub = cx.subscribe_in(&query, window, |_, _, event: &InputEvent, _, cx| {
            if let InputEvent::Change = event {
                cx.notify();
            }
        });
        self._subscriptions.push(sub);
        query
    }

    fn confirm_delete_host(&mut self, host_id: String, cx: &mut Context<Self>) {
        if self.prompt_open(cx) {
            return;
        }
        let Some(host) = self.library.host(&host_id) else { return };
        let projects = self.library.projects.iter().filter(|p| p.hosts.contains(&host_id)).count();
        let mut body = format!("{} ({}) is removed from Komando.", host.name, host.address);
        if projects > 1 {
            body.push_str(&format!(" It is in {projects} projects and leaves all of them."));
        }
        body.push_str(" Its open sessions are closed. A saved key or password stays in the vault.");
        let mut reached: Vec<&str> = self.library.reached_through(&host_id).iter().map(|h| h.name.as_str()).collect();
        reached.sort_unstable();
        match reached.as_slice() {
            [] => {}
            [one] => body.push_str(&format!(" {one} is reached through it and will connect directly.")),
            [rest @ .., last] => {
                body.push_str(&format!(" {} and {last} are reached through it and will connect directly.", rest.join(", ")))
            }
        }
        self.overlay = Some(Overlay::Confirm {
            title: format!("Delete {}?", host.name),
            body,
            action: ConfirmAction::DeleteHost(host_id),
        });
        cx.notify();
    }

    fn confirm_delete_project(&mut self, project_id: String, cx: &mut Context<Self>) {
        if self.prompt_open(cx) {
            return;
        }
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
                self.library.remove_host(&id);
            }
            ConfirmAction::DeleteProject(id) => {
                self.discard_sessions(|s| s.project_id.as_deref() == Some(id.as_str()), cx);
                self.library.remove_project(&id);
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


/// The project the first connection goes into when it is added before any project exists.
pub(super) const FIRST_PROJECT_NAME: &str = "My servers";
pub(super) const FIRST_PROJECT_COLOR: &str = "blue";

/// The local shells offered on the start screen, with short labels: one per kind
/// (PowerShell 7 wins over Windows PowerShell), at most three.
fn welcome_shells(shells: Vec<LocalShell>) -> Vec<(String, LocalShell)> {
    let mut picked: Vec<(String, LocalShell)> = vec![];
    for shell in shells {
        let label = if shell.name.contains("PowerShell") {
            "PowerShell".to_string()
        } else if shell.name == "Command Prompt" {
            "CMD".to_string()
        } else {
            shell.name.clone()
        };
        if picked.len() < 3 && !picked.iter().any(|(l, _)| *l == label) {
            picked.push((label, shell));
        }
    }
    picked
}

/// How a Host on the way shows on the route page.
fn route_card(hop: &model::ResolvedHost) -> RouteCard {
    let user = hop.user.as_deref().map(|u| format!("{u}@")).unwrap_or_default();
    let port = if hop.port == 22 { String::new() } else { format!(":{}", hop.port) };
    RouteCard {
        name: hop.name.clone(),
        detail: format!("{user}{}{port}", hop.address),
        icon: hop.icon.clone(),
        color: project_color(&hop.color),
    }
}

fn sanitize(s: &str) -> String {
    s.chars().map(|c| if c.is_alphanumeric() || c == '-' || c == '.' { c } else { '_' }).collect()
}

// ---- rendering -------------------------------------------------------------------------

impl Workspace {
    /// The master password dialog. It must not be mistaken for a server's password question
    /// (a plain card titled "Password for …"): it carries the vault badge, names Komando's
    /// vault, and says in a highlighted note that this is not the server's password.
    fn render_unlock(&self, state: &UnlockState, p: AppPalette, cx: &mut Context<Self>) -> AnyElement {
        let creating = state.creating;
        let skip_label = if self.pending.iter().any(|job| matches!(job, Pending::Open { .. })) {
            "Type the server password instead"
        } else {
            "Not now"
        };
        let form = div()
            .flex()
            .flex_col()
            .gap(px(10.))
            .child(Input::new(&state.password))
            .when(creating, |el| el.child(Input::new(&state.confirm)))
            .when_some(state.error.clone(), |el, e| el.child(div().text_size(px(12.)).text_color(p.danger).child(e)));
        let header = div()
            .flex()
            .items_center()
            .gap(px(12.))
            .child(badge("lock-keyhole", p.brand, 38.))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(2.))
                    .child(div().text_size(px(11.5)).font_weight(FontWeight::MEDIUM).text_color(p.brand).child("KOMANDO VAULT"))
                    .child(
                        div()
                            .text_size(px(16.))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(p.text)
                            .child(if creating { "Create a master password" } else { "Enter your master password" }),
                    ),
            );
        let note = div()
            .flex()
            .items_start()
            .gap(px(10.))
            .px(px(12.))
            .py(px(10.))
            .rounded(px(8.))
            .bg(p.brand_soft)
            .border_1()
            .border_color(p.brand.opacity(0.35))
            .child(div().flex_shrink_0().pt(px(2.)).child(icon_el("key-round", 15., p.brand)))
            .child(
                div().flex_1().min_w_0().text_size(px(12.5)).line_height(px(18.)).text_color(p.text).child(if creating {
                    "This password protects the passwords and keys Komando saves for you. It is not a server's password, and Komando asks for it only when it needs the vault."
                } else {
                    "This is the password you chose for Komando's vault, not the password of the server."
                }),
            );
        // The one sentence that must not be skimmed past.
        let warning = creating.then(|| {
            div()
                .text_size(px(12.5))
                .line_height(px(18.))
                .font_weight(FontWeight::MEDIUM)
                .text_color(p.warning)
                .child("There is no recovery: if you forget it, saved passwords are lost. Projects and hosts are kept.")
        });
        let actions = div()
            .flex()
            .items_center()
            .justify_end()
            .gap(px(8.))
            .child(
                button("unlock-skip", skip_label, ButtonKind::Ghost, p)
                    .on_click(cx.listener(|this, _, window, cx| this.skip_unlock(window, cx))),
            )
            .child(
                button("unlock", if creating { "Create vault" } else { "Unlock" }, ButtonKind::Primary, p)
                    .on_click(cx.listener(|this, _, window, cx| this.submit_unlock(window, cx))),
            );
        modal(
            p,
            div()
                .w(px(440.))
                .p(px(24.))
                .flex()
                .flex_col()
                .gap(px(16.))
                // A brand-coloured top edge, which the plain server questions don't have.
                .border_t_4()
                .border_color(p.brand)
                .rounded(px(14.))
                .child(header)
                .child(div().text_size(px(13.)).line_height(px(20.)).text_color(p.text_muted).child(state.reason.clone()))
                .child(note)
                .children(warning)
                .child(form)
                .child(actions)
                .into_any_element(),
        )
    }

    /// The sidebar's rows, in order. Kept small (indices, counts) so working it out on every
    /// frame is cheap; only the rows in view are built into elements.
    fn sidebar_rows(&self) -> Vec<SidebarRow> {
        let mut counts: HashMap<&ItemKey, usize> = HashMap::new();
        for s in &self.sessions {
            *counts.entry(&s.item).or_default() += 1;
        }
        let hosts = self.library.host_indices();
        let mut rows = vec![];
        for (index, shell) in self.sidebar_shells().iter().enumerate() {
            let key = ItemKey::Shell { project: None, name: shell.name.clone() };
            let sessions = self.nested_rows(&key, counts.get(&key).copied().unwrap_or(0));
            rows.push(SidebarRow::Shell { index, sessions });
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
            let start = rows.len();
            for id in &project.hosts {
                let Some(&host) = hosts.get(id.as_str()) else { continue };
                let key = ItemKey::Host { project: project.id.clone(), host: id.clone() };
                let sessions = self.nested_rows(&key, counts.get(&key).copied().unwrap_or(0));
                rows.push(SidebarRow::Host { project: pi, host, sessions });
            }
            for (shell, sh) in project.shells.iter().enumerate() {
                let key = ItemKey::Shell { project: Some(project.id.clone()), name: sh.name.clone() };
                let sessions = self.nested_rows(&key, counts.get(&key).copied().unwrap_or(0));
                rows.push(SidebarRow::ProjectShell { project: pi, shell, sessions });
            }
            if rows.len() == start {
                rows.push(SidebarRow::FirstHost { project: pi });
            }
        }
        rows
    }

    /// One row of the sidebar list, built only when it's in view.
    fn render_sidebar_row(&mut self, ix: usize, cx: &mut Context<Self>) -> AnyElement {
        let p = cx.global::<Appearance>().app;
        let Some(row) = self.sidebar_rows.get(ix).cloned() else { return div().into_any_element() };
        let element = match row {
            SidebarRow::Shell { index, .. } => match self.sidebar_shells().get(index).cloned() {
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
                        .aria_label("New project")
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
            SidebarRow::Host { project, host, .. } => {
                let (Some(project), Some(host)) = (self.library.projects.get(project), self.library.hosts.get(host)) else {
                    return div().into_any_element();
                };
                let color = project_color(&project.color);
                let item = SidebarItem::Host { project: project.clone(), host: host.clone() };
                div().flex().flex_col().gap(px(2.))
                    .children(self.render_item(&item, color, 20., p, cx)).into_any_element()
            }
            SidebarRow::ProjectShell { project, shell, .. } => {
                let Some(project) = self.library.projects.get(project) else { return div().into_any_element() };
                let Some(shell) = project.shells.get(shell) else { return div().into_any_element() };
                let color = project_color(&project.color);
                let item = SidebarItem::Shell { project: Some(project.clone()), shell: shell.clone() };
                div().flex().flex_col().gap(px(2.))
                    .children(self.render_item(&item, color, 20., p, cx)).into_any_element()
            }
        };
        div().px(px(8.)).pb(px(2.)).child(element).into_any_element()
    }

    fn render_project_row(&self, index: usize, p: AppPalette, cx: &mut Context<Self>) -> AnyElement {
        let Some(project) = self.library.projects.get(index) else { return div().into_any_element() };
            let color = project_color(&project.color);
            let collapsed = self.collapsed.contains(&project.id);
            let sessions: Vec<&SessionEntry> = self.sessions.iter().filter(|s| match &s.item {
                ItemKey::Host { project: id, host } => id == &project.id && project.hosts.contains(host),
                ItemKey::Shell { project: id, name } => id.as_ref() == Some(&project.id) && project.shells.iter().any(|sh| &sh.name == name),
            }).collect();
            let live = sessions.iter().filter(|s| s.is_live()).count();
            let busy = sessions.iter().any(|s| s.busy);
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
                    // When collapsed, show how many Sessions are live inside. It gives way to the
                    // row's add button while hovered, so the two never sit on top of each other.
                    .when(collapsed && live > 0, |el| {
                        el.child(
                            div()
                                .flex_shrink_0()
                                .flex()
                                .items_center()
                                .gap(px(5.))
                                .text_size(px(11.5))
                                .text_color(p.text_faint)
                                .group_hover("project-row", |s| s.invisible())
                                .when(busy, |el| el.child(activity_icon(p)))
                                .child(format!("{live} live")),
                        )
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
        let rows = crate::perf::time("sidebar rows", || self.sidebar_rows());
        if rows != self.sidebar_rows {
            self.sidebar_list.reset(rows.len());
            self.sidebar_rows = rows;
        }
        let list = list(self.sidebar_list.clone(), cx.processor(|this, ix, _window, cx| this.render_sidebar_row(ix, cx)))
            .flex_1()
            .min_h_0();

        let (show_downloads, attention, downloads_open) = {
            let t = self.transfers.read(cx);
            (t.any(), t.attention(), t.is_open())
        };
        let footer_row = |id: &'static str, icon: &str, label: String| {
            div()
                .id(id)
                .role(gpui::Role::Button)
                .aria_label(label.clone())
                .tab_index(0)
                .focus_visible(move |s| s.border_2().border_color(p.brand))
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
                            .role(gpui::Role::Button)
                            .aria_label("Quick connect")
                            .tab_index(0)
                            .focus_visible(move |s| s.border_2().border_color(p.brand))
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
                    .gap(px(2.))
                    // Appears with the first transfer; the number is what needs a look.
                    .when(show_downloads, |el| {
                        el.child(
                            footer_row("downloads", "download", "Downloads".into())
                                .when(downloads_open, |el| el.bg(p.active).text_color(p.text))
                                .child(div().flex_1())
                                .when(attention > 0, |el| {
                                    el.child(
                                        div()
                                            .min_w(px(18.))
                                            .h(px(18.))
                                            .px(px(5.))
                                            .flex()
                                            .items_center()
                                            .justify_center()
                                            .rounded_full()
                                            .bg(p.brand_soft)
                                            .text_size(px(11.))
                                            .font_weight(FontWeight::SEMIBOLD)
                                            .text_color(p.brand)
                                            .child(attention.to_string()),
                                    )
                                })
                                .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                                    this.transfers.update(cx, |t, cx| t.toggle(cx));
                                })),
                        )
                    })
                    .child(
                        div().flex().items_center().gap(px(4.))
                            .child(
                                footer_row("settings", "settings", "Settings".into())
                                    .flex_1().min_w_0()
                                    .child(div().flex_1())
                                    .child(kbd("Ctrl ,", p))
                                    .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                                        this.open_settings(SettingsSection::Appearance, window, cx)
                                    })),
                            )
                            .children(self.render_update_button(p, cx)),
                    ),
            )
            .into_any_element()
    }

    /// One sidebar item, plus a row for each of its Sessions when it has several.
    ///
    /// Live items are bright with a status dot; idle ones are dimmed; a dropped Session
    /// that still holds its output shows a hollow ring until it is reconnected or discarded.
    /// An item with several Sessions is a group: its row shows how many and folds them away,
    /// and each Session has its own row with its own close button.
    fn render_item(&self, item: &SidebarItem, color: Hsla, indent: f32, p: AppPalette, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let key = item.key();
        let sessions: Vec<&SessionEntry> = self.sessions_of(&key).collect();
        let grouped = sessions.len() > 1;
        let expanded = grouped && !self.collapsed_items.contains(&key);
        let shown = self.last_active.get(&key).copied().or_else(|| sessions.first().map(|s| s.id));
        let main = shown.and_then(|id| sessions.iter().find(|s| s.id == id).copied()).or(sessions.first().copied());
        let holds_active = sessions.iter().any(|s| Some(s.id) == self.active);
        // A group's state: live if any Session is, connecting if any is on the way.
        let status = if grouped {
            let any = |f: fn(&Status) -> bool| sessions.iter().any(|s| f(&s.status));
            Some(if any(|s| matches!(s, Status::Connected)) {
                Status::Connected
            } else if any(|s| matches!(s, Status::Connecting)) {
                Status::Connecting
            } else {
                Status::Disconnected(String::new())
            })
        } else {
            main.map(|s| s.status.clone())
        };
        let live = matches!(status, Some(Status::Connected | Status::Connecting));
        let busy = if grouped { sessions.iter().any(|s| s.busy) } else { main.is_some_and(|s| s.busy) };
        // With the Sessions listed below, the active one is marked there instead.
        let active = holds_active && !expanded;
        let any_live = sessions.iter().any(|s| s.is_live());
        let session_id = main.map(|s| s.id);
        let icon = item.icon();

        let ws = cx.entity().downgrade();
        let menu_item_src = item.clone();
        let menu_key = key.clone();
        let count = sessions.len();
        let item_menu = move |menu: PopupMenu, _: &mut Window, _: &mut Context<PopupMenu>| {
            let item = menu_item_src.clone();
            let (open, another) = (item.clone(), item.clone());
            let mut menu = menu
                .item(menu_item(if live { "Show session" } else { "Connect" }, "plug", &ws, move |this, window, cx| {
                    this.activate_item(open.clone(), false, window, cx)
                }))
                .item(menu_item("New session", "plus", &ws, move |this, window, cx| this.activate_item(another.clone(), true, window, cx)));
            if grouped {
                let (fold, all) = (menu_key.clone(), menu_key.clone());
                menu = menu
                    .item(menu_item(
                        if expanded { "Collapse sessions" } else { "Expand sessions" },
                        if expanded { "chevron-up" } else { "chevron-down" },
                        &ws,
                        move |this, _, cx| this.toggle_item_sessions(&fold, cx),
                    ))
                    .item(menu_item(
                        &if any_live { format!("Disconnect all {count}") } else { format!("Close all {count}") },
                        if any_live { "unplug" } else { "x" },
                        &ws,
                        move |this, window, cx| this.close_item_sessions(&all, window, cx),
                    ));
            } else if let Some(id) = session_id {
                if live {
                    menu = menu.item(menu_item("Disconnect", "unplug", &ws, move |this, window, cx| this.close_session(id, window, cx)));
                } else {
                    menu = menu.item(menu_item("Close session", "x", &ws, move |this, window, cx| this.close_session(id, window, cx)));
                }
            }
            if let SidebarItem::Host { project, host } = &item {
                let (pa, ha) = (project.id.clone(), host.id.clone());
                let (pb, hb) = (project.id.clone(), host.id.clone());
                let (pd, hd) = (project.id.clone(), host.id.clone());
                let hc = host.id.clone();
                menu = menu
                    .separator()
                    .item(menu_item("Edit host…", "pencil", &ws, move |this, window, cx| this.open_edit_host(pa.clone(), ha.clone(), window, cx)))
                    .item(menu_item("Duplicate host…", "copy-plus", &ws, move |this, window, cx| {
                        this.duplicate_host(pd.clone(), hd.clone(), window, cx)
                    }))
                    .item(menu_item("Copy address", "copy", &ws, move |this, _, cx| this.copy_host_address(&pb, &hb, cx)))
                    .separator()
                    .item(menu_item("Delete host…", "trash", &ws, move |this, _, cx| this.confirm_delete_host(hc.clone(), cx)));
            }
            menu
        };

        let item_for_click = item.clone();
        let item_for_new = item.clone();
        let toggle_key = key.clone();
        // A shell's coloured icon is dimmed like the tinted ones when it's idle.
        let dim = !(active || live) && is_artwork(&icon);
        let mut rows = vec![];
        rows.push(
            div()
                .id(SharedString::from(format!("item-{key:?}")))
                .group("item-row")
                // Also the size of its right-click menu, which inherits it.
                .text_size(px(13.))
                .relative()
                .h(px(34.))
                .pl(px(indent))
                .pr(px(if grouped { 6. } else { 10. }))
                .flex()
                .items_center()
                .gap(px(10.))
                .rounded(px(8.))
                .cursor_pointer()
                .when(active, |el| el.bg(p.active))
                .when(!active, |el| el.hover(|s| s.bg(p.hover)))
                .on_click(cx.listener(move |this, event: &ClickEvent, window, cx| {
                    this.activate_item(item_for_click.clone(), event.modifiers().control, window, cx)
                }))
                .child(
                    div()
                        .flex_shrink_0()
                        .when(dim, |el| el.opacity(0.55))
                        .child(icon_el(&icon, 15., if active || live { color } else { p.text_faint })),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .text_color(if active || live { p.text } else { p.text_muted })
                        .child(item.label().to_string()),
                )
                // The status (and a group's count) gives way to the row's buttons while
                // hovered, so the two never sit on top of each other.
                .child(
                    div()
                        .flex_shrink_0()
                        .flex()
                        .items_center()
                        .gap(px(6.))
                        .group_hover("item-row", |s| s.invisible())
                        .child(status_indicator(status.as_ref(), busy, p))
                        .when(grouped, |el| {
                            el.child(div().text_size(px(11.5)).text_color(p.text_faint).child(count.to_string()))
                        }),
                )
                // A group folds its Sessions away with the chevron, which is always there.
                .when(grouped, |el| {
                    el.child(
                        div()
                            .id(SharedString::from(format!("fold-{key:?}")))
                            .role(gpui::Role::Button)
                            .aria_label(if expanded { "Collapse sessions" } else { "Expand sessions" })
                            .aria_expanded(expanded)
                            .flex_shrink_0()
                            .size(px(22.))
                            .flex()
                            .items_center()
                            .justify_center()
                            .rounded(px(6.))
                            .hover(|s| s.bg(p.hover))
                            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                                cx.stop_propagation();
                                this.toggle_item_sessions(&toggle_key, cx);
                            }))
                            .child(icon_el(if expanded { "chevron-down" } else { "chevron-right" }, 14., p.text_faint)),
                    )
                })
                .child(
                    // Opaque sidebar colour under the row's translucent hover colour, so the
                    // buttons' backdrop matches the row and covers the end of a long label.
                    div()
                        .absolute()
                        .right(px(if grouped { 30. } else { 4. }))
                        .top(px(5.))
                        .rounded(px(7.))
                        .bg(p.sidebar)
                        .invisible()
                        .group_hover("item-row", |s| s.visible())
                        .child(
                            div()
                                .flex()
                                .gap(px(2.))
                                .rounded(px(7.))
                                .bg(if active { p.active } else { p.hover })
                                .child(
                                    icon_button(SharedString::from(format!("another-{key:?}")), "plus", p)
                                        .aria_label("New session")
                                        .size(px(24.))
                                        .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                                            cx.stop_propagation();
                                            this.activate_item(item_for_new.clone(), true, window, cx);
                                        })),
                                )
                                // A group has no close button of its own: which Session would it
                                // close? Each Session's row has one.
                                .when_some(session_id.filter(|_| !grouped), |el, id| {
                                    el.child(
                                        icon_button(SharedString::from(format!("close-{id}")), "x", p)
                                            .aria_label(if live { "Disconnect" } else { "Close session" })
                                            .size(px(24.))
                                            .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                                                cx.stop_propagation();
                                                this.close_session(id, window, cx);
                                            })),
                                    )
                                }),
                        ),
                )
                .context_menu(item_menu)
                .into_any_element(),
        );
        if expanded {
            for session in &sessions {
                rows.push(self.render_session_row(session, indent, p, cx));
            }
        }
        rows
    }

    /// One Session of an item with several: its state, its title (or "Session 2"), and a
    /// button that disconnects it, or closes it once it has dropped.
    fn render_session_row(&self, session: &SessionEntry, indent: f32, p: AppPalette, cx: &mut Context<Self>) -> AnyElement {
        let id = session.id;
        let active = Some(id) == self.active;
        let live = session.is_live();
        let dropped = !live;
        let title = session_title(session);
        let ws = cx.entity().downgrade();
        let session_menu = move |menu: PopupMenu, _: &mut Window, _: &mut Context<PopupMenu>| {
            if dropped {
                menu.item(menu_item("Reconnect", "refresh-cw", &ws, move |this, window, cx| {
                    this.activate(id, window, cx);
                    this.reconnect(id, window, cx);
                }))
                .item(menu_item("Close", "x", &ws, move |this, window, cx| this.close_session(id, window, cx)))
            } else {
                menu.item(menu_item("Disconnect", "unplug", &ws, move |this, window, cx| this.close_session(id, window, cx)))
            }
        };
        div()
            .id(SharedString::from(format!("session-{id}")))
            .group("session-row")
            .text_size(px(12.5))
            .relative()
            .h(px(30.))
            // The status sits under the item's icon, the title under its name.
            .pl(px(indent + 2.))
            .pr(px(10.))
            .flex()
            .items_center()
            .gap(px(11.))
            .rounded(px(8.))
            .cursor_pointer()
            .when(active, |el| el.bg(p.active))
            .when(!active, |el| el.hover(|s| s.bg(p.hover)))
            .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                this.activate(id, window, cx);
                let dropped = this.sessions.iter().find(|s| s.id == id).is_some_and(|s| !s.is_live());
                if dropped {
                    this.reconnect(id, window, cx);
                }
            }))
            .child(
                div()
                    .w(px(12.))
                    .flex_shrink_0()
                    .flex()
                    .justify_center()
                    .child(status_indicator(Some(&session.status), session.busy, p)),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_color(if active || live { p.text } else { p.text_muted })
                    .child(title.clone()),
            )
            // Titles can repeat (two shells in the same folder); the number tells them apart.
            .when(title.as_ref() != format!("Session {}", session.number), |el| {
                el.child(
                    div()
                        .flex_shrink_0()
                        .text_size(px(11.))
                        .text_color(p.text_faint)
                        .group_hover("session-row", |s| s.invisible())
                        .child(format!("#{}", session.number)),
                )
            })
            .child(
                div()
                    .absolute()
                    .right(px(4.))
                    .top(px(3.))
                    .rounded(px(7.))
                    .bg(p.sidebar)
                    .invisible()
                    .group_hover("session-row", |s| s.visible())
                    .child(
                        div().rounded(px(7.)).bg(if active { p.active } else { p.hover }).child(
                            icon_button(SharedString::from(format!("close-{id}")), "x", p)
                                .aria_label(if live { "Disconnect" } else { "Close session" })
                                .size(px(24.))
                                .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                                    cx.stop_propagation();
                                    this.close_session(id, window, cx);
                                })),
                        ),
                    ),
            )
            .context_menu(session_menu)
            .into_any_element()
    }

    /// Fold an item's Sessions away under its row, or show them again.
    fn toggle_item_sessions(&mut self, key: &ItemKey, cx: &mut Context<Self>) {
        if !self.collapsed_items.remove(key) {
            self.collapsed_items.insert(key.clone());
        }
        cx.notify();
    }

    /// Disconnect every live Session of an item; when none is live, close them all.
    fn close_item_sessions(&mut self, key: &ItemKey, window: &mut Window, cx: &mut Context<Self>) {
        let any_live = self.sessions_of(key).any(|s| s.is_live());
        let ids: Vec<usize> = self.sessions_of(key).filter(|s| s.is_live() == any_live).map(|s| s.id).collect();
        for id in ids {
            self.close_session(id, window, cx);
        }
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
        let is_ssh = matches!(entry.kind, SessionKind::Ssh { .. });
        let sftp_open = entry.sftp_open;
        // SFTP runs over the session's connection: until it is connected the button only looks
        // like one. An open browser can still be closed.
        let sftp_ready = matches!(entry.status, Status::Connected);
        let sftp_enabled = sftp_ready || sftp_open;
        let sftp_color = if sftp_open {
            p.brand
        } else if sftp_ready {
            p.text_muted
        } else {
            p.text_faint.opacity(0.55)
        };
        let sftp_tip = match (&entry.status, sftp_open) {
            (_, true) => "Close file browser (Ctrl+Shift+E)",
            (Status::Connected, false) => "Browse files over SFTP (Ctrl+Shift+E)",
            (Status::Connecting, false) => "File browser: available once the session is connected",
            (Status::Disconnected(_), false) => "File browser: reconnect the session to browse its files",
        };
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
                .when(is_ssh, |el| {
                    el.child(
                        div()
                            .id("sftp-toggle")
                            .h(px(24.))
                            .px(px(9.))
                            .mr(px(6.))
                            .flex()
                            .items_center()
                            .gap(px(6.))
                            .rounded(px(7.))
                            .border_1()
                            .border_color(if sftp_open {
                                p.brand.opacity(0.7)
                            } else if sftp_enabled {
                                p.border
                            } else {
                                p.border.opacity(0.5)
                            })
                            .when(sftp_open, |el| el.bg(p.brand_soft))
                            .text_size(px(12.))
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(sftp_color)
                            .tooltip(move |window, cx| Tooltip::new(sftp_tip).build(window, cx))
                            .when(sftp_enabled, |el| {
                                el.cursor_pointer()
                                    .hover(|s| s.bg(p.hover))
                                    .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| this.toggle_sftp(id, window, cx)))
                            })
                            .when(!sftp_enabled, |el| el.cursor_default())
                            .child(icon_el("folder-open", 13., sftp_color))
                            .child("SFTP"),
                    )
                })
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
                .child(div().w(px(12.)).h(px(16.)).mr(px(6.)).border_r_1().border_color(p.border))
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
            // No cover for DWM's own caption buttons is needed: with Mica on, the window has
            // no native caption for DWM to draw them in (see `platform::set_backdrop`).
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
                    .when(!minimal, |el| el.child(wordmark(16., p.text))),
            )
            .child(div().flex_1().min_w_0().h_full().flex().items_center().children(strip))
            .child(window_controls(p, window))
            .into_any_element()
    }

    fn render_main(&self, p: AppPalette, window: &Window, cx: &mut Context<Self>) -> AnyElement {
        let Some(entry) = self.active_entry() else {
            return self.render_empty(p, cx);
        };
        let project = entry.project_id.as_deref().and_then(|id| self.library.project(id));
        let color = project.map(|pr| project_color(&pr.color)).unwrap_or(p.text_faint);
        let term_bg = cx.global::<Appearance>().term.background_hsla();
        let browser = entry.browser.clone().filter(|_| entry.sftp_open);
        // The browser keeps at least 300px for the terminal when the window is narrow.
        let window_width = f32::from(window.bounds().size.width);
        let max = (window_width - SIDEBAR_WIDTH - ISLAND_GAP * 3. - 300.).max(280.);
        let width = self.settings.sftp_width.min(max).max(280.);
        if let Some(browser) = &browser {
            browser.update(cx, |b, _| b.width = width);
        }
        island(p)
            .relative()
            .flex_1()
            .min_w_0()
            .h_full()
            .bg(term_bg)
            .overflow_hidden()
            .flex()
            .child(div().flex_1().min_w_0().h_full().child(entry.view.clone()))
            .when_some(browser, |el, browser| {
                el.child(
                    div()
                        .id("sftp-divider")
                        .group("sftp-divider")
                        .w(px(9.))
                        .h_full()
                        .flex_shrink_0()
                        .flex()
                        .justify_center()
                        .py(px(14.))
                        .cursor_col_resize()
                        .on_drag(SftpDivider, |_, _, _, cx| cx.new(|_| SftpDivider))
                        .child(
                            div()
                                .w(px(2.))
                                .h_full()
                                .rounded_full()
                                .when(self.sftp_resizing, |el| el.bg(p.brand))
                                .group_hover("sftp-divider", |s| s.bg(p.brand.opacity(0.6))),
                        ),
                )
                .child(
                    div().w(px(width)).flex_shrink_0().h_full().pt(px(10.)).pr(px(8.)).pb(px(8.)).child(
                        div()
                            .size_full()
                            .rounded(px(12.))
                            .border_1()
                            .border_color(p.border)
                            .bg(p.sidebar)
                            .overflow_hidden()
                            .child(browser),
                    ),
                )
            })
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
        if self.library.projects.is_empty() && self.library.hosts.is_empty() {
            return self.render_welcome(p, cx);
        }
        island(p)
            .flex_1()
            .h_full()
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap(px(18.))
            .bg(p.background)
            .child(wordmark(34., p.text.opacity(0.12)))
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

    /// The start screen, shown until the first project or connection exists: what Komando is,
    /// and one click to add a server or open a local shell.
    fn render_welcome(&self, p: AppPalette, cx: &mut Context<Self>) -> AnyElement {
        let shells = welcome_shells(self.library.standalone_shells().into_iter().filter(|s| !s.hidden).collect());
        let hero = |id: SharedString, icon: &str, label: String, kind: ButtonKind| {
            icon_text_button(id, icon, label, kind, p).h(px(44.)).px(px(20.)).gap(px(10.)).rounded(px(10.)).text_size(px(14.))
        };
        let mut actions = vec![
            hero("welcome-add".into(), "plus", "Add connection".into(), ButtonKind::Primary)
                .on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.open_new_host(String::new(), window, cx)))
                .into_any_element(),
            hero("welcome-project".into(), "folder-plus", "New project".into(), ButtonKind::Secondary)
                .on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.open_new_project(window, cx)))
                .into_any_element(),
        ];
        for (label, shell) in shells {
            let icon = shell_icon(&shell.program).unwrap_or("square-terminal");
            actions.push(
                hero(SharedString::from(format!("welcome-shell-{label}")), icon, label, ButtonKind::Secondary)
                    .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                        this.activate_item(SidebarItem::Shell { project: None, shell: shell.clone() }, false, window, cx)
                    }))
                    .into_any_element(),
            );
        }
        island(p)
            .flex_1()
            .h_full()
            .px(px(32.))
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .bg(p.background)
            .child(wordmark(60., p.text))
            .child(
                div()
                    .mt(px(14.))
                    .text_size(px(16.))
                    .text_color(p.text_muted)
                    .text_center()
                    .child("A fast terminal for local and remote work."),
            )
            .child(div().mt(px(40.)).flex().flex_wrap().justify_center().gap(px(12.)).children(actions))
            .into_any_element()
    }

    /// The active Session's open question (a password, a host key…) as a modal over the whole
    /// window: the sidebar and the rest are blocked until it is answered. A press on the
    /// backdrop gives the keyboard back to the question. A dialog you opened yourself comes
    /// first; the question waits behind it.
    fn render_prompt_modal(&self, p: AppPalette, cx: &mut Context<Self>) -> Option<AnyElement> {
        if self.overlay.is_some() {
            return None;
        }
        let view = self.active_entry()?.view.clone();
        let card = view.update(cx, |view, cx| view.prompt_modal(cx))?;
        Some(
            div()
                .id("prompt-backdrop")
                .absolute()
                .occlude()
                // Below the title bar, as for the other dialogs.
                .top(px(TITLE_BAR_HEIGHT))
                .left_0()
                .right_0()
                .bottom_0()
                .flex()
                .items_center()
                .justify_center()
                .p(px(24.))
                .bg(alpha(0x000000, if p.dark { 0.5 } else { 0.2 }))
                .on_mouse_down(MouseButton::Left, move |_, window, cx| {
                    view.update(cx, |view, cx| view.focus_prompt(window, cx));
                })
                .child(card)
                .into_any_element(),
        )
    }

    /// `wide`: the window has room for forms laid out in two columns.
    fn render_overlay(&self, wide: bool, p: AppPalette, cx: &mut Context<Self>) -> Option<AnyElement> {
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
                            let via = r
                                .jump_host
                                .as_deref()
                                .and_then(|id| self.library.host(id))
                                .map(|jump| format!(" · via {}", jump.name))
                                .unwrap_or_default();
                            (r.icon.clone(), project_color(&r.color), host.name.clone(), format!("{user}{}{via} · {}", host.address, project.name))
                        }
                        SidebarItem::Shell { project, shell } => (
                            shell_icon(&shell.program).unwrap_or("square-terminal").to_string(),
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
            Overlay::HostForm(form) => self.render_host_form(form, wide, p, cx),
            Overlay::ProjectForm(form) => self.render_project_form(form, wide, p, cx),
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
                // Close on a press outside the card. Not on click: selecting text in a field by
                // dragging past the card's edge ends outside it, and that must not close it.
                .on_mouse_down(MouseButton::Left, cx.listener(|this, _: &gpui::MouseDownEvent, window, cx| this.close_overlay(window, cx)))
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
                        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                        .child(body),
                )
                .into_any_element(),
        )
    }
}

/// The sign that output is streaming in.
fn activity_icon(p: AppPalette) -> AnyElement {
    div().flex_shrink_0().child(icon_el("activity", 12., p.success)).into_any_element()
}

/// A Session's state in the sidebar: a dot while live (green) or connecting (amber), a hollow
/// ring once it dropped, and a pulse line while output is streaming in (a command at work).
/// The pulse line doesn't move: an animation would redraw the whole window every frame for as
/// long as `tail -f` runs.
fn status_indicator(status: Option<&Status>, busy: bool, p: AppPalette) -> AnyElement {
    match status {
        Some(Status::Connected) if busy => activity_icon(p),
        Some(Status::Connected) => status_dot(p.success).into_any_element(),
        Some(Status::Connecting) => status_dot(p.warning).into_any_element(),
        Some(Status::Disconnected(_)) => {
            div().size(px(7.)).flex_shrink_0().rounded_full().border_1().border_color(p.text_faint).into_any_element()
        }
        None => div().into_any_element(),
    }
}

/// What a Session's own row in the sidebar says: the title its shell set, unless that only
/// repeats the item's name or is the program's path (as Windows shells set it); otherwise
/// "Session 2".
fn session_title(session: &SessionEntry) -> SharedString {
    let title = session.title.trim();
    let useless = title.is_empty() || title == session.label.as_ref() || title.to_ascii_lowercase().ends_with(".exe");
    if useless { format!("Session {}", session.number).into() } else { title.to_string().into() }
}

/// A segmented control: one choice of several.
fn segmented<L: Into<SharedString> + Clone>(
    id: &'static str,
    options: &[(L, bool)],
    p: AppPalette,
    cx: &mut Context<Workspace>,
    pick: impl Fn(&mut Workspace, usize, &mut Window, &mut Context<Workspace>) + Clone + 'static,
) -> AnyElement {
    let items: Vec<_> = options.iter().enumerate().map(|(i, (label, selected))| {
            let pick = pick.clone();
            let label: SharedString = label.clone().into();
            div()
                .id((id, i))
                .role(gpui::Role::RadioButton)
                .aria_label(label.clone())
                .aria_toggled(if *selected { gpui::Toggled::True } else { gpui::Toggled::False })
                .tab_index(0)
                .focus_visible(move |s| s.border_2().border_color(p.brand))
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
                .child(label)
        })
        .collect();
    div()
        .id(id)
        .role(gpui::Role::RadioGroup)
        .aria_label(id.replace('-', " "))
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
fn switch(id: SharedString, label: impl Into<SharedString>, on: bool, p: AppPalette) -> gpui::Stateful<gpui::Div> {
    switch_with(id, label.into(), on, false, p)
}

/// A smaller, neutral switch for long lists (single highlight rules), so the brand-coloured
/// switches of the settings that matter more stand out.
fn small_switch(id: SharedString, label: impl Into<SharedString>, on: bool, p: AppPalette) -> gpui::Stateful<gpui::Div> {
    switch_with(id, label.into(), on, true, p)
}

fn switch_with(id: SharedString, label: SharedString, on: bool, small: bool, p: AppPalette) -> gpui::Stateful<gpui::Div> {
    let (w, h, knob) = if small { (26., 14., 10.) } else { (32., 18., 14.) };
    // The off track has to be visible on its own, not only through the knob's position.
    let off = if p.dark { alpha(0xffffff, 0.16) } else { alpha(0x000000, 0.2) };
    let on_color = if small { p.text_muted.opacity(0.6) } else { p.brand };
    div()
        .id(id)
        .role(gpui::Role::Switch)
        .aria_label(label)
        .aria_toggled(if on { gpui::Toggled::True } else { gpui::Toggled::False })
        .tab_index(0)
        .focus_visible(move |s| s.border_2().border_color(p.brand))
        .w(px(w))
        .h(px(h))
        .flex_shrink_0()
        .p(px(2.))
        .rounded_full()
        .cursor_pointer()
        .bg(if on { on_color } else { off })
        .flex()
        .when(on, |el| el.justify_end())
        // A white knob disappears on a very light accent (Graphite's), so it takes the
        // accent's own text colour there.
        .child(
            div()
                .size(px(knob))
                .rounded_full()
                .bg(if on && !small && p.brand.l > 0.82 { p.on_brand } else { gpui::white() })
                .shadow(elevation(false)),
        )
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
            .role(gpui::Role::RadioButton)
            .aria_label(format!("Icon: {name}"))
            .aria_toggled(if is_selected { gpui::Toggled::True } else { gpui::Toggled::False })
            .tab_index(0)
            .focus_visible(move |s| s.border_2().border_color(p.brand))
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

/// Minimise, maximise/restore and close, in the order Windows uses, drawn like the rest of
/// Komando: rounded buttons that sit inside the title bar with room around them, instead of
/// squares that fill it and run into the terminal's coloured edge below. The close button's
/// right edge lines up with the terminal island's.
///
/// Each button's hit area is still the full height of the bar, and the close button's reaches
/// the window's corner, so flicking the pointer to the top-right corner of a maximised window
/// still hits it. On Windows the buttons are hit-tested by the system (snap layouts on
/// maximise work); elsewhere (Linux dev builds) they act on click.
fn window_controls(p: AppPalette, window: &Window) -> impl IntoElement {
    let maximized = window.is_maximized();
    let button = |id: &'static str, icon: &'static str, area: WindowControlArea, label: &'static str| {
        let close = area == WindowControlArea::Close;
        let (hover_bg, hover_fg, press_bg) = if close {
            (crate::theme::hex(0xc42b1c), gpui::white(), crate::theme::hex(0xa82518))
        } else {
            (p.hover, p.text, p.active)
        };
        div()
            .id(id)
            .group(id)
            .role(gpui::Role::Button)
            .aria_label(label)
            .h_full()
            .flex()
            .items_center()
            .pl(px(WINDOW_BUTTON_GAP))
            // The last button's hit area runs on to the window's edge.
            .when(close, |el| el.pr(px(ISLAND_GAP)))
            // The button's hitbox hides the title bar's drag area behind it. Without this the
            // system hit test can answer "caption" for a button (seen after maximising with a
            // dialog open): a click then does nothing and only a double click acts.
            .occlude()
            .window_control_area(area)
            .when(cfg!(not(windows)), |el| {
                el.on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation()).on_click(move |_, window, _| match area {
                    WindowControlArea::Min => window.minimize_window(),
                    WindowControlArea::Max => window.zoom_window(),
                    _ => window.remove_window(),
                })
            })
            .child(
                div()
                    .id("shape")
                    .w(px(WINDOW_BUTTON_WIDTH))
                    .h(px(WINDOW_BUTTON_HEIGHT))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded(px(8.))
                    .group_hover(id, move |s| s.bg(hover_bg))
                    .group_active(id, move |s| s.bg(press_bg))
                    .child(
                        gpui::svg()
                            .path(SharedString::from(format!("icons/{icon}.svg")))
                            .size(px(13.))
                            .text_color(p.text_muted)
                            .group_hover(id, move |s| s.text_color(hover_fg)),
                    ),
            )
    };
    div()
        .flex()
        .h_full()
        .flex_shrink_0()
        .child(button("window-minimize", "window-minimize", WindowControlArea::Min, "Minimize"))
        .child(if maximized {
            button("window-restore", "window-restore", WindowControlArea::Max, "Restore")
        } else {
            button("window-maximize", "window-maximize", WindowControlArea::Max, "Maximize")
        })
        .child(button("window-close", "window-close", WindowControlArea::Close, "Close"))
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
        .role(gpui::Role::Button)
        .aria_label(label)
        .tab_index(0)
        .focus_visible(move |s| s.border_2().border_color(p.brand))
        .size(px(26.))
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(7.))
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
                if event.keystroke.key == "escape" && this.unlock.is_some() {
                    this.skip_unlock(window, cx);
                    cx.stop_propagation();
                } else if event.keystroke.key == "escape" && this.overlay.is_some() {
                    this.close_overlay(window, cx);
                    cx.stop_propagation();
                } else if event.keystroke.key == "escape" && this.prompt_open(cx) {
                    if let Some(entry) = this.active_entry() {
                        entry.view.clone().update(cx, |view, cx| view.cancel_prompt(window, cx));
                    }
                    cx.stop_propagation();
                } else if event.keystroke.key == "escape" && matches!(this.page, Page::Settings(_)) {
                    this.close_settings(window, cx);
                    cx.stop_propagation();
                }
            }))
            .on_action(cx.listener(|this, _: &QuickConnect, window, cx| this.open_picker(window, cx)))
            // Another Session on the item you're in; without one, quick connect.
            .on_action(cx.listener(|this, _: &NewSession, window, cx| {
                if this.modal_open(cx) {
                    return;
                }
                match this.active_item() {
                    Some(item) => this.activate_item(item, true, window, cx),
                    None => this.open_picker(window, cx),
                }
            }))
            .on_action(cx.listener(|this, _: &CloseSession, window, cx| {
                if let Some(id) = this.active.filter(|_| !this.modal_open(cx)) {
                    this.close_session(id, window, cx);
                }
            }))
            .on_action(cx.listener(|this, _: &NextSession, window, cx| {
                if !this.modal_open(cx) {
                    this.cycle(true, window, cx)
                }
            }))
            .on_action(cx.listener(|this, _: &PrevSession, window, cx| {
                if !this.modal_open(cx) {
                    this.cycle(false, window, cx)
                }
            }))
            .on_action(cx.listener(|this, _: &CopyOutputText, _, cx| this.copy_output_text(cx)))
            .on_action(cx.listener(|this, _: &CopyOutputFile, _, cx| this.copy_output_file(cx)))
            .on_action(cx.listener(|this, _: &LockVault, _, cx| this.lock_vault(cx)))
            .on_action(cx.listener(|this, _: &ToggleSftp, window, cx| {
                if let Some(id) = this.active.filter(|_| !this.modal_open(cx)) {
                    this.toggle_sftp(id, window, cx);
                }
            }))
            .on_drag_move(cx.listener(|this, event: &gpui::DragMoveEvent<SftpDivider>, window, cx| {
                this.sftp_resizing = true;
                this.resize_sftp(f32::from(event.event.position.x), window, cx);
            }))
            .on_drop(cx.listener(|this, _: &SftpDivider, _, cx| {
                this.sftp_resizing = false;
                let _ = this.settings.save(&this.data_dir);
                cx.notify();
            }))
            .on_action(cx.listener(|this, _: &OpenSettings, window, cx| {
                if this.prompt_open(cx) {
                    return;
                }
                if matches!(this.page, Page::Settings(_)) {
                    this.close_settings(window, cx);
                } else {
                    this.open_settings(SettingsSection::Appearance, window, cx);
                }
            }));

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
                .child(self.render_sidebar(p, cx))
                .child(self.render_main(p, window, cx))
                .into_any_element()),
        };
        // Downloads: floats above the sidebar's footer, from any page.
        let downloads = self.transfers.read(cx).is_open().then(|| {
            let footer_rows = 1. + self.transfers.read(cx).any() as u8 as f32;
            div()
                .absolute()
                .left(px(ISLAND_GAP))
                .bottom(px(ISLAND_GAP + 16. + footer_rows * 34. + 6.))
                .w(px(SIDEBAR_WIDTH))
                .child(self.transfers.clone())
        });
        root.bg(p.canvas)
            .child(title_bar)
            .child(page)
            .children(downloads)
            .children(self.render_overlay(f32::from(window.bounds().size.width) >= WIDE_FORM_MIN_WINDOW, p, cx))
            .children(self.render_prompt_modal(p, cx))
            .children(self.unlock.as_ref().map(|s| self.render_unlock(s, p, cx)))
            .children(toast)
            .into_any_element()
    }
}

#[cfg(test)]
mod welcome_tests {
    use super::*;

    fn shell(name: &str) -> LocalShell {
        LocalShell { name: name.into(), ..LocalShell::default() }
    }

    #[test]
    fn start_screen_offers_one_shell_of_each_kind() {
        let shells = vec![shell("PowerShell 7"), shell("Windows PowerShell"), shell("Command Prompt")];
        let picked = welcome_shells(shells);
        let labels: Vec<&str> = picked.iter().map(|(l, _)| l.as_str()).collect();
        assert_eq!(labels, ["PowerShell", "CMD"]);
        assert_eq!(picked[0].1.name, "PowerShell 7");
    }

    #[test]
    fn start_screen_keeps_other_shell_names() {
        let picked = welcome_shells(vec![shell("bash")]);
        assert_eq!(picked[0].0, "bash");
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
