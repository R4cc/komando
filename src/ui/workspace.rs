//! The main window: unlock screen, Project sidebar with its Sessions, the active terminal,
//! quick connect, and the small forms for adding Projects and Hosts.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use gpui::prelude::FluentBuilder as _;
use gpui::{
    Animation, AnimationExt as _, AnyElement, App, anchored, deferred, ClickEvent, Context, Entity, FocusHandle, Focusable, FontWeight, Hsla, KeyDownEvent,
    ScrollHandle, SharedString, Subscription, Window, WindowAppearance, actions, div, prelude::*, px, WindowControlArea, MouseButton, ListState, ListAlignment, list,
};
use gpui_component::input::{Input, InputEvent, InputState};
use gpui_component::menu::{ContextMenuExt, PopupMenu, PopupMenuItem};
use gpui_component::slider::{Slider, SliderEvent, SliderState};
use gpui_component::tooltip::Tooltip;

use crate::highlight::{self, ActiveHighlights, HighlightSet, Highlighter};
use crate::model::{
    self, AppThemeSetting, CredentialSource, Host, Library, LocalShell, Project, Settings, TerminalThemeSetting,
};
use crate::session::{self, Status, ssh};
use crate::theme::{AppPalette, Appearance, TerminalPalette, alpha, project_color};
use crate::model::Protocol;
use crate::ui::connect_route::RouteCard;
use crate::ui::desktop_view::DesktopView;
use crate::ui::file_browser::{self, BrowserEvent, FavoriteStore, FileBrowser, MenuExtras};
use crate::ui::popup::item as menu_item;
use crate::ui::terminal_view::{TerminalEvent, TerminalView};
use crate::ui::transfers::{Activity, Transfers, TransfersChanged};
use crate::ui::widgets::{
    Busy, BusyPhase, ButtonKind, ISLAND_GAP, MenuTrigger, badge, busy_done, button, close_button, icon_button, icon_el, icon_text_button, island,
    connection_indicator, is_artwork, password_input, progress_ring, section_label, shell_icon_color, spinner, status_dot, wordmark,
};
use crate::vault::{Credential, Secret, Vault};

mod command_history;
mod desktop_window;
mod desktops;
mod forms;
mod forwards;
mod highlights;
mod import;
mod main_menu;
pub mod nav;
mod notifications;
mod onboarding;
mod open_tabs;
mod sessions;
mod settings;
mod split;
mod updates;
mod vault_page;
mod window_frame;
pub use settings::SettingsSection;
use desktops::desktop_protocol;
use nav::NavTarget;
use sessions::{EndRequest, Ending, EndingAll, Question, Side};
use split::{DraggedItem, DraggedProject, DropTarget};
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
        ShowAbout,
        ToggleSftp,
        ToggleSidebar,
        TogglePresentation,
        PickerUp,
        PickerDown
    ]
);

/// The key context of the quick connect dialog, for its Up and Down bindings.
pub const PICKER_CONTEXT: &str = "QuickConnect";

/// What fills the window: the sessions, or the full-window Settings page.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Page {
    Sessions,
    Settings(SettingsSection),
}

const SIDEBAR_WIDTH: f32 = 264.;
/// Expand sidebar on hover, timed like Zen Browser's compact mode: the strip along the
/// window's left edge that floats the sidebar out (at once), how long the pointer may be off
/// it before it slides back, and how long sliding in and out takes.
const SIDEBAR_PEEK_EDGE: f32 = 9.;
const SIDEBAR_PEEK_CLOSE_MS: u64 = 150;
const SIDEBAR_PEEK_IN_MS: u64 = 250;
const SIDEBAR_PEEK_OUT_MS: u64 = 150;
/// Leaving the window through its left edge keeps the sidebar out until the pointer is this
/// far past the edge, so overshooting the edge doesn't close it.
const SIDEBAR_PEEK_OUTSIDE: f32 = 200.;

/// Leave a usable terminal beside the navigation, including at the minimum window size.
/// Where a Host's icon sits in the sidebar, from the left of its row.
const NESTED_INDENT: f32 = 28.;
/// How far a Host row's highlight starts inside its Project row's.
const NESTED_INSET: f32 = 14.;

fn sidebar_width(preferred: f32, window_width: f32) -> f32 {
    let max = (window_width - ISLAND_GAP * 3. - 360.).clamp(200., 520.);
    let preferred = if preferred.is_finite() { preferred } else { SIDEBAR_WIDTH };
    preferred.clamp(200., max)
}

/// A divider drag has no image following the pointer.
struct SidebarDivider;

impl Render for SidebarDivider {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
    }
}

/// What is dragged while resizing a Session's file browser (drawn as nothing).
struct SftpDivider(usize);

impl Render for SftpDivider {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
    }
}
/// Height of Komando's own title bar (and of its window buttons).
const TITLE_BAR_HEIGHT: f32 = 40.;
/// macOS draws its own close, minimise and zoom buttons (the traffic lights) at the top left
/// of a window, centred in Komando's title bar. The title bar leaves this much room for them.
const TRAFFIC_LIGHTS_WIDTH: f32 = if cfg!(target_os = "macos") { 64. } else { 0. };
pub const TRAFFIC_LIGHT_POSITION: Option<gpui::Point<gpui::Pixels>> =
    if cfg!(target_os = "macos") { Some(gpui::Point { x: px(ISLAND_GAP + 8.), y: px((TITLE_BAR_HEIGHT - 14.) / 2.) }) } else { None };
/// The window buttons' highlighted shapes, sized like Windows 11's own caption buttons.
const WINDOW_BUTTON_WIDTH: f32 = 46.;
const WINDOW_BUTTON_HEIGHT: f32 = 32.;

/// Window width from which the host and project forms use two columns side by side: the
/// wide form plus a margin each side. Narrower, they stack in one.
const WIDE_FORM_MIN_WINDOW: f32 = forms::WIDE_FORM_WIDTH + 60.;

#[derive(Clone)]
enum SessionKind {
    Ssh { host_id: String },
    Local { shell: LocalShell },
    /// A Host's Remote Desktop over RDP or VNC.
    Desktop { host_id: String, protocol: Protocol },
}

/// What a Session shows: a terminal, or a Remote Desktop.
#[derive(Clone, PartialEq)]
enum SessionView {
    Terminal(Entity<TerminalView>),
    Desktop(Entity<DesktopView>),
}

impl SessionView {
    fn terminal(&self) -> Option<&Entity<TerminalView>> {
        match self {
            SessionView::Terminal(view) => Some(view),
            SessionView::Desktop(_) => None,
        }
    }

    fn desktop(&self) -> Option<&Entity<DesktopView>> {
        match self {
            SessionView::Desktop(view) => Some(view),
            SessionView::Terminal(_) => None,
        }
    }

    fn has_prompt(&self, cx: &App) -> bool {
        match self {
            SessionView::Terminal(view) => view.read(cx).has_prompt(),
            SessionView::Desktop(view) => view.read(cx).has_prompt(),
        }
    }

    /// End its connection for good: the Session is being discarded.
    fn close(&self, cx: &mut App) {
        match self {
            SessionView::Terminal(view) => view.update(cx, |v, _| v.close()),
            SessionView::Desktop(view) => view.update(cx, |v, _| v.close()),
        }
    }
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

/// A row in the sidebar. Shared, as a row's handlers each keep a copy and a Project lists
/// all its Hosts.
#[derive(Clone)]
enum SidebarItem {
    Host { project: Arc<Project>, host: Arc<Host> },
    Shell { project: Option<Arc<Project>>, shell: LocalShell },
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
            Self::Shell { shell, .. } => crate::icons::shell_icon(&shell).into(),
        }
    }

    fn icon_color(&self, icon: &str, color: Hsla, p: AppPalette) -> Hsla {
        match self {
            Self::Shell { .. } => shell_icon_color(icon, p),
            Self::Host { .. } => color,
        }
    }
}

/// A quick connect result and what its row shows, worked out when the query changes rather
/// than on every frame.
struct PickerResult {
    item: SidebarItem,
    icon: String,
    /// A Host's colour; a shell's icon is coloured by the palette when drawn.
    color: Option<Hsla>,
    name: SharedString,
    detail: SharedString,
}

/// What an item's sidebar row shows (see `Workspace::item_state`).
struct ItemState<'a> {
    sessions: Vec<&'a SessionEntry>,
    /// The Sessions with rows of their own under the item's.
    listed: Vec<&'a SessionEntry>,
    grouped: bool,
    expanded: bool,
    /// The Session last shown for it.
    shown: Option<usize>,
    /// The Session the row itself stands for, if it stands for one.
    main: Option<&'a SessionEntry>,
    beside: bool,
    /// The Session beside the row's in the remembered split, shown or not (see `split_link`).
    linked: Option<usize>,
    status: Option<Status>,
    live: bool,
    busy: bool,
    active: bool,
}

/// An open Session (see `sessions.rs`).
type SessionEntry = sessions::Entry<SessionParts>;

/// What only the UI needs of an open Session: the rest is kept by `Sessions`.
struct SessionParts {
    view: SessionView,
    /// The terminal's title (set by the shell), copied here like the status.
    title: SharedString,
    /// The SFTP file browser beside the terminal (SSH Sessions). Kept while hidden, so it
    /// opens again where it was.
    browser: Option<Entity<FileBrowser>>,
    sftp_open: bool,
    /// The browser's work under way: the title bar's SFTP button turns a loader, then shows a
    /// check. Copied from the browser when it changes, so rendering never reads the browser.
    sftp_busy: Busy,
    /// Opened before any terminal pane was laid out: the shell starts once one is, at its size.
    waiting_for_size: bool,
    /// SSH: the Host's address as it connected (see `history::address_label`). Commands go
    /// into the history with it, even if the Host is edited while the Session is open.
    history_on: Option<String>,
    /// A Remote Desktop dragged out into a window of its own: that window. Set together with
    /// `Sessions::set_own_window`.
    window: Option<gpui::AnyWindowHandle>,
    _subscription: Subscription,
    _browser_subscription: Option<Subscription>,
    _browser_observer: Option<Subscription>,
}

impl SessionParts {
    /// A Session just opened: titled with its label, without a file browser or a window of
    /// its own.
    fn new(view: SessionView, label: &SharedString, subscription: Subscription) -> Self {
        Self {
            view,
            title: label.clone(),
            browser: None,
            sftp_open: false,
            sftp_busy: Busy::default(),
            waiting_for_size: false,
            history_on: None,
            window: None,
            _subscription: subscription,
            _browser_subscription: None,
            _browser_observer: None,
        }
    }
}

enum Overlay {
    /// Quick connect. `selected` is the result Enter opens, moved with Up and Down.
    Picker { query: Entity<InputState>, results: Vec<PickerResult>, selected: usize, scroll: ScrollHandle },
    /// Add a Host to a Project, or edit one.
    HostForm(forms::HostForm),
    /// Create a Project, or edit one.
    ProjectForm(forms::ProjectForm),
    /// Add or edit a standalone Local Shell from Settings.
    ShellForm,
    /// Add or edit a Highlight Rule from Settings → Highlights.
    RuleForm(Box<highlights::RuleForm>),
    /// Create or rename a Highlight Set.
    SetForm(highlights::SetForm),
    /// Ask before something that can't be undone.
    Confirm { title: String, body: String, action: ConfirmAction },
    /// Give a Session a name of its own (or clear it).
    RenameSession { id: usize, name: Entity<InputState>, _subscription: Subscription },
    /// A Host's Command History.
    History(command_history::HistoryPanel),
    /// The welcome tour, shown on the first start (see `onboarding.rs`).
    Tour(onboarding::Tour),
}

#[derive(Clone)]
enum ConfirmAction {
    DeleteHost(String),
    DeleteProject(String),
    /// Disconnect a Session whose command is still printing output, keeping it.
    Disconnect(usize),
    /// Close a Session (see `end_session`) whose command is still printing output.
    Close(usize),
    /// Disconnect all of an item's Sessions, some still printing output.
    DisconnectAll(ItemKey),
    /// Every Host's Command History (Settings → Security).
    DeleteAllHistory,
    /// A saved password or key (Settings → Vault).
    DeleteCredential(model::CredentialId),
    /// Download and install the update, after which Komando closes (see `install_update`).
    InstallUpdate,
    /// Restart into the update already in place.
    RestartForUpdate,
}

/// What waits for the Vault: done once the master password is entered.
enum Pending {
    Open { project_id: String, host_id: String },
    /// Connect a Remote Desktop Session whose password is in the Vault.
    Desktop { session: usize },
    /// Save a Remote Desktop's password typed in with "Remember" ticked.
    SaveDesktop { host_id: String, protocol: Protocol, user: Option<String>, password: zeroize::Zeroizing<String> },
    Save { session: usize, hop: usize, password: String },
    /// Save the open host or project form, which imports a key file into the Vault.
    Form,
}

/// A saved font weight as the text system takes it; a weight edited out of range is kept in it.
fn font_weight(weight: u16) -> gpui::FontWeight {
    gpui::FontWeight(weight.clamp(*crate::model::FONT_WEIGHTS.start(), *crate::model::FONT_WEIGHTS.end()) as f32)
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
    /// Screenshot tooling: show the active Session's file browser after opening Sessions.
    pub show_files: bool,
    /// A Local Shell to open: its Project's id (`None` when standalone) and name. Passed by the
    /// Komando that asked for this one to run the shell as administrator.
    pub open_shell: Option<(Option<String>, String)>,
}

pub struct Workspace {
    data_dir: PathBuf,
    library: Library,
    settings: Settings,
    #[cfg(windows)]
    tray: Option<crate::platform::Tray>,
    #[cfg(windows)]
    _activation: Option<crate::platform::instance::WindowBinding>,
    #[cfg(windows)]
    notifications: Option<crate::platform::Notifications>,
    completion_watches: notifications::CompletionWatches,
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
    /// The sidebar is near its narrowest: rows offer fewer buttons on hover.
    sidebar_compact: bool,
    /// The sidebar's width as last drawn, for what fits in its rows.
    sidebar_width_px: f32,
    /// Keyboard use of the sidebar (see `nav.rs`).
    nav: nav::NavState,
    /// Per-window navigation visibility; the title-bar toggle stays available when hidden.
    sidebar_visible: bool,
    sidebar_resizing: bool,
    /// The hidden sidebar floated out over the Sessions (Expand sidebar on hover).
    sidebar_peek: bool,
    /// A pending open (`true`) or close of `sidebar_peek`, cancelled by dropping it.
    sidebar_peek_timer: Option<(bool, gpui::Task<()>)>,
    /// The floated-out sidebar is sliding back; drawn until this finishes.
    sidebar_peek_closing: Option<gpui::Task<()>>,
    /// Follows the pointer outside the window while the sidebar is out.
    sidebar_peek_watch: Option<gpui::Task<()>>,
    /// The highlight set whose delete button was clicked once and now asks for a second click.
    confirm_delete_set: settings::ConfirmTwice,
    blur_slider: Option<Entity<SliderState>>,
    font_slider: Option<Entity<SliderState>>,
    /// Settings → Appearance: the terminal's font size in Presentation mode.
    presentation_slider: Option<Entity<SliderState>>,
    /// The menu behind the title bar's ☰ button.
    main_menu: crate::ui::popup::Popup,
    /// The active Session's Port Forwards, out of the session strip.
    forwards_menu: crate::ui::popup::Popup,
    /// The Session whose forwards the menu shows.
    forwards_menu_session: Option<usize>,
    /// Presentation mode is on, with what to put back when it ends.
    presentation: Option<main_menu::Presentation>,
    font_input: Option<Entity<InputState>>,
    /// Settings → Appearance with Mode set to System: show the theme of the mode Windows is
    /// not in, to change it before Windows switches.
    appearance_other_mode: bool,
    /// A theme menu is open and shows the entry under the keyboard or pointer: the saved
    /// theme settings, put back when it closes without a pick.
    theme_preview: Option<settings::ThemePreview>,
    /// Settings → Mouse & clipboard: the characters that end a double-clicked word.
    word_separators_input: Option<Entity<InputState>>,
    /// Settings → Terminal: the regular and bold font weight fields.
    weight_inputs: Option<[Entity<InputState>; 2]>,
    /// Settings → Local shells: the form that adds a shell or edits one.
    shell_editor: Option<settings::ShellEditor>,
    /// A shell whose Remove button was clicked once, waiting for the second click.
    confirm_remove_shell: settings::ConfirmTwice,
    vault: Option<Vault>,
    /// Settings → Vault: the Credentials whose secret is shown. Cleared when the Vault locks
    /// or the page is left.
    vault_revealed: HashSet<model::CredentialId>,
    /// A Vault file is on this PC (looked up when its state can change, not on every frame).
    /// With it locked, the sidebar shows the vault row.
    vault_exists: bool,
    /// The master password popover, out of the sidebar's vault row.
    unlock: Option<UnlockState>,
    /// What waits for the master password, in the order it was asked for.
    pending: Vec<Pending>,
    /// Unlocked with "Stay unlocked" off: lock again once what waited has used the Vault.
    lock_after_use: bool,
    sessions: sessions::Sessions<SessionParts>,
    /// The terminal tabs last written for Restore tabs on start; `None` while they aren't
    /// kept (switched off, before the saved ones are reopened, or a window opened by tooling).
    tab_memory: Option<open_tabs::TabMemory>,
    /// Where a sidebar item being dragged would land.
    drop_target: Option<DropTarget>,
    /// A Remote Desktop's sidebar row held outside the window, to open in its own.
    tear_off: Option<desktop_window::TearOff>,
    native_drag: Option<crate::platform::DragPreview>,
    /// The terminal pane's size, as last laid out. A new Session's shell starts at this size
    /// instead of a guess, so it isn't resized (and repainted) straight after it starts.
    pane_size: Option<session::GridSize>,
    /// Sidebar items whose Sessions are folded away under the item's row.
    collapsed_items: HashSet<ItemKey>,
    overlay: Option<Overlay>,
    toast: Option<(SharedString, Instant)>,
    last_activity: SystemTime,
    /// Last activity and policy written to the Windows-protected unlock file.
    remembered_unlock: Option<(SystemTime, u32)>,
    focus: FocusHandle,
    /// Downloads and uploads from every file browser (the sidebar's Transfers entry).
    transfers: Entity<Transfers>,
    favorites: Entity<FavoriteStore>,
    /// The divider between terminal and file browser is being dragged.
    sftp_resizing: bool,
    /// The divider between the two panes of a split is being dragged.
    split_resizing: bool,
    /// The last key handed to a jump host row in a form (see `forms::JumpRow`).
    next_jump_row: usize,
    /// Settings → Import: where an import from another SSH client is.
    import: import::ImportFlow,
    _subscriptions: Vec<Subscription>,
}

/// The master password popover. It opens when something needs the Vault (connecting with a
/// saved password, saving one, importing a key) or from the sidebar's vault row, never at
/// startup. It doesn't block the window: the terminal and sidebar stay usable while it's open.
struct UnlockState {
    creating: bool,
    /// The popover's own focus, so Escape closes it only while the keyboard is in it.
    focus: FocusHandle,
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

impl Drop for Workspace {
    fn drop(&mut self) {
        // Flush activity since the last timer tick on a normal window close.
        if let Err(err) = self.sync_remembered_unlock() {
            log::warn!("Could not save remembered vault unlock on exit: {err:#}");
        }
    }
}

impl Workspace {
    pub fn new(data_dir: PathBuf, startup: Startup, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let settings = Settings::load(&data_dir);
        crate::perf::startup("settings loaded");
        // A fresh install has no Projects; the detected local shells stand alone.
        let mut library_error = None;
        let library = Library::load(&data_dir).unwrap_or_else(|err| {
            log::error!("{err:#}");
            // Move the unreadable file aside: the next save would otherwise replace it.
            let path = data_dir.join(Library::FILE);
            let aside = path.with_extension(format!("toml.broken-{}", chrono::Local::now().format("%Y%m%d-%H%M%S")));
            library_error = Some(match std::fs::rename(&path, &aside) {
                Ok(()) => format!("Couldn't read your hosts ({err:#}). The file was kept as {}.", aside.display()),
                Err(_) => format!("Couldn't read your hosts: {err:#}"),
            });
            Library::default()
        });
        crate::perf::startup("library loaded");
        let highlights = highlight::load(&data_dir);
        crate::perf::startup("highlights loaded");
        let transfers = cx.new(Transfers::new);
        let favorites = cx.new(|_| FavoriteStore::new(data_dir.clone()));
        let transfers_sub = cx.subscribe(&transfers, |_, _, _: &TransfersChanged, cx| cx.notify());
        // The ring on the Transfers entry follows the panel's progress ticks.
        let transfers_ticks = cx.observe(&transfers, |_, _, cx| cx.notify());
        let update = crate::update::take_install_error(&data_dir).map(UpdateState::Failed).unwrap_or(UpdateState::Idle);
        #[cfg(windows)]
        let auxiliary = startup.open_shell.is_some() && crate::elevate::is_elevated();
        #[cfg(windows)]
        let activation = if auxiliary { None } else {
            crate::platform::instance::WindowBinding::new(window, &data_dir)
                .map_err(|error| log::error!("Could not register the running window: {error:#}")).ok()
        };
        let sidebar_visible = !settings.sidebar_auto_hide;
        let mut this = Self {
            data_dir,
            library,
            #[cfg(windows)]
            tray: crate::platform::Tray::new(window, settings.minimize_to_tray, !auxiliary),
            #[cfg(windows)]
            _activation: activation,
            #[cfg(windows)]
            notifications: None,
            completion_watches: Default::default(),
            settings,
            highlights,
            highlight_skipped: vec![],
            page: Page::Sessions,
            highlights_expanded: None,
            confirm_delete_set: settings::ConfirmTwice::default(),
            sidebar_list: ListState::new(0, ListAlignment::Top, px(240.)),
            sidebar_compact: false,
            sidebar_width_px: SIDEBAR_WIDTH,
            nav: nav::NavState::new(cx),
            sidebar_rows: vec![],
            sidebar_visible,
            sidebar_resizing: false,
            sidebar_peek: false,
            sidebar_peek_timer: None,
            sidebar_peek_closing: None,
            sidebar_peek_watch: None,
            blur_slider: None,
            font_slider: None,
            presentation_slider: None,
            main_menu: Default::default(),
            forwards_menu: Default::default(),
            forwards_menu_session: None,
            presentation: None,
            font_input: None,
            word_separators_input: None,
            appearance_other_mode: false,
            theme_preview: None,
            weight_inputs: None,
            shell_editor: None,
            confirm_remove_shell: settings::ConfirmTwice::default(),
            vault: None,
            vault_revealed: HashSet::new(),
            vault_exists: false,
            unlock: None,
            pending: vec![],
            lock_after_use: false,
            sessions: Default::default(),
            tab_memory: None,
            drop_target: None,
            tear_off: None,
            native_drag: None,
            pane_size: None,
            collapsed_items: HashSet::new(),
            overlay: None,
            toast: None,
            last_activity: SystemTime::now(),
            remembered_unlock: None,
            focus: cx.focus_handle(),
            update,
            transfers,
            favorites,
            sftp_resizing: false,
            split_resizing: false,
            next_jump_row: 0,
            import: import::ImportFlow::default(),
            _subscriptions: vec![transfers_sub, transfers_ticks],
        };
        crate::perf::startup("workspace built");
        this.apply_appearance(window, cx);
        crate::perf::startup("appearance applied");
        this.apply_highlights(cx);
        crate::perf::startup("highlights compiled");
        let sub = cx.observe_window_appearance(window, |this, window, cx| this.apply_appearance(window, cx));
        this._subscriptions.push(sub);
        // An unexpired timed unlock can survive a restart on this Windows account.
        // Until-restart and one-use unlocks never restore a saved key.
        let minutes = if this.settings.stay_unlocked { this.settings.auto_lock_minutes } else { 0 };
        match Vault::restore_unlock(&this.data_dir, minutes, SystemTime::now()) {
            Ok(Some((vault, activity))) => {
                this.vault = Some(vault);
                this.last_activity = activity;
                this.remembered_unlock = Some((activity, minutes));
            }
            Ok(None) => {}
            Err(err) => log::warn!("Could not restore the vault unlock; master password required: {err:#}"),
        }
        if let Some(password) = &startup.master_password {
            match Vault::unlock(&this.data_dir, password) {
                Ok(vault) => this.vault = Some(vault),
                Err(err) => log::error!("startup unlock: {err:#}"),
            }
        }
        this.vault_exists = Vault::exists(&this.data_dir);
        this.start_auto_lock(window, cx);
        this.start_notifications(window, cx);
        // Desktops in windows of their own close with Komando's.
        let workspace = cx.entity().downgrade();
        window.on_window_should_close(cx, move |_, cx| {
            let _ = workspace.update(cx, |this, cx| this.close_own_windows(cx));
            true
        });
        if let Some(message) = library_error {
            this.show_toast_for(message, Duration::from_secs(20), cx);
        }
        // Look for a new version shortly after start, without getting in the way of it.
        if this.settings.check_for_updates && !matches!(this.update, UpdateState::Failed(_)) {
            cx.spawn(async move |this, cx| {
                cx.background_executor().timer(Duration::from_secs(3)).await;
                let _ = this.update(cx, |this, cx| this.check_for_updates(true, cx));
            })
            .detach();
        }
        this.start_usage_ping(cx);
        crate::perf::startup("workspace ready");
        // The welcome tour, on a fresh install. Development tooling that opens Sessions or
        // unlocks at start isn't interrupted by it.
        let tooling = startup.master_password.is_some() || !startup.open.is_empty() || startup.open_shell.is_some();
        match onboarding::should_offer(&this.settings, &this.library, this.vault_exists) {
            Some(true) if !tooling => cx.defer_in(window, |this, window, cx| this.start_tour(window, cx)),
            None => this.tour_closed(cx),
            _ => {}
        }
        if !startup.open.is_empty() || startup.open_shell.is_some() {
            cx.defer_in(window, move |this, window, cx| this.run_startup(&startup, window, cx));
        } else if this.settings.restore_tabs && !tooling {
            cx.defer_in(window, |this, window, cx| this.restore_tabs(window, cx));
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
        if let Some((project, name)) = &startup.open_shell {
            let key = ItemKey::Shell { project: project.clone(), name: name.clone() };
            match items.iter().find(|item| item.key() == key).cloned() {
                Some(item) => self.activate_item(item, false, window, cx),
                None => self.show_toast(format!("Couldn't find the shell “{name}”. It may have been renamed or removed."), cx),
            }
        }
        // `active` counts the Sessions opened, in order.
        let active = startup.active.and_then(|index| self.sessions.iter().nth(index).map(|s| s.id()));
        if let Some(id) = active {
            self.activate(id, window, cx);
        }
        if let Some(section) = startup.settings {
            self.highlights_expanded = self.highlights.first().map(|s| s.id.clone());
            self.open_settings(section, window, cx);
        }
        if startup.show_files && let Some(id) = self.sessions.active() {
            // The demo connection signs in asynchronously; the normal SFTP action remains
            // unavailable until then. Wait for it instead of changing production behavior.
            cx.spawn_in(window, async move |this, cx| {
                for _ in 0..100 {
                    let finished = this.update_in(cx, |this, window, cx| {
                        let Some(entry) = this.sessions.iter().find(|entry| entry.id() == id) else { return true };
                        match entry.status() {
                            Status::Connected => { this.toggle_sftp(id, window, cx); true }
                            Status::Disconnected(_) => true,
                            Status::Connecting => false,
                        }
                    }).unwrap_or(true);
                    if finished { break; }
                    cx.background_executor().timer(Duration::from_millis(50)).await;
                }
            }).detach();
        }
    }

    // ---- appearance -------------------------------------------------------------------

    /// Whether the app is dark in `window`'s appearance.
    fn app_dark(&self, window: &Window) -> bool {
        let system_dark = matches!(window.appearance(), WindowAppearance::Dark | WindowAppearance::VibrantDark);
        match self.settings.app_theme {
            AppThemeSetting::System => system_dark,
            AppThemeSetting::Light => false,
            AppThemeSetting::Dark => true,
        }
    }

    /// Give a window the app's backdrop (Mica, when on), light or dark frame and frame colors.
    /// Returns whether the backdrop shows.
    fn style_window(&self, window: &mut Window, app_dark: bool) -> bool {
        // Mica: Windows blurs the desktop behind the window; our surfaces become partly
        // transparent so it shows through. The slider sets how much.
        let blur = crate::platform::SUPPORTS_BACKDROP && self.settings.background_blur;
        window.set_background_appearance(if blur {
            gpui::WindowBackgroundAppearance::MicaBackdrop
        } else {
            gpui::WindowBackgroundAppearance::Opaque
        });
        crate::platform::set_window_dark_mode(window, app_dark);
        crate::platform::set_app_icon(window, self.settings.app_icon);
        let backdrop_enabled = crate::platform::set_backdrop(window, blur);
        if blur && !backdrop_enabled {
            window.set_background_appearance(gpui::WindowBackgroundAppearance::Opaque);
        }
        let opaque = AppPalette::from_theme(self.theme_for(app_dark));
        crate::platform::set_native_caption_color(
            window,
            (!backdrop_enabled).then(|| crate::theme::packed(opaque.canvas)),
        );
        backdrop_enabled
    }

    fn apply_appearance(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let app_dark = self.app_dark(window);
        let term_dark = self.terminal_dark(app_dark);
        let backdrop_enabled = self.style_window(window, app_dark);
        #[cfg(windows)]
        if let Some(tray) = &self.tray { tray.refresh_icon(); }
        for handle in self.own_windows() {
            let _ = handle.update(cx, |_, window, _| self.style_window(window, app_dark));
        }
        let t = if backdrop_enabled { self.settings.background_blur_intensity as f32 / 100. } else { 0. };
        cx.set_global(Appearance {
            app: AppPalette::from_theme(self.theme_for(app_dark)).translucent(t),
            term: TerminalPalette::for_mode(term_dark, self.terminal_colors_for(term_dark), self.theme_for(term_dark)).translucent(t, app_dark),
            font_family: if self.settings.font_family.trim().is_empty() { crate::theme::TERMINAL_FONT.into() } else { self.settings.font_family.clone().into() },
            font_size: self.terminal_font_size(),
            font_weight: font_weight(self.settings.font_weight),
            bold_font_weight: font_weight(self.settings.bold_font_weight),
        });
        gpui_component::Theme::change(
            if app_dark { gpui_component::ThemeMode::Dark } else { gpui_component::ThemeMode::Light },
            Some(window),
            cx,
        );
        let app = cx.global::<Appearance>().app;
        gpui_component::Theme::update(cx, |theme| app.apply_components(theme));
        cx.refresh_windows();
    }

    /// Whether the terminal is dark while the app is dark (`app_dark`) or light.
    fn terminal_dark(&self, app_dark: bool) -> bool {
        match self.settings.terminal_theme {
            TerminalThemeSetting::Follow => app_dark,
            TerminalThemeSetting::Light => false,
            TerminalThemeSetting::Dark => true,
        }
    }

    /// The Theme picked for dark or light mode.
    fn theme_for(&self, dark: bool) -> &'static crate::theme::ThemeSpec {
        crate::theme::theme(dark, if dark { &self.settings.dark_theme } else { &self.settings.light_theme })
    }

    /// The Terminal Colors id picked for a dark or light terminal.
    fn terminal_colors_for(&self, dark: bool) -> &str {
        if dark { &self.settings.dark_terminal_colors } else { &self.settings.light_terminal_colors }
    }

    fn set_terminal_colors(&mut self, dark: bool, id: &'static str, window: &mut Window, cx: &mut Context<Self>) {
        if dark {
            self.settings.dark_terminal_colors = id.into();
        } else {
            self.settings.light_terminal_colors = id.into();
        }
        self.save_settings(cx);
        self.apply_appearance(window, cx);
    }

    fn set_theme(&mut self, dark: bool, id: &'static str, window: &mut Window, cx: &mut Context<Self>) {
        if dark {
            self.settings.dark_theme = id.into();
        } else {
            self.settings.light_theme = id.into();
        }
        self.save_settings(cx);
        self.apply_appearance(window, cx);
    }

    fn set_app_theme(&mut self, theme: AppThemeSetting, window: &mut Window, cx: &mut Context<Self>) {
        self.settings.app_theme = theme;
        self.appearance_other_mode = false;
        self.save_settings(cx);
        self.apply_appearance(window, cx);
    }

    fn set_terminal_theme(&mut self, theme: TerminalThemeSetting, window: &mut Window, cx: &mut Context<Self>) {
        self.settings.terminal_theme = theme;
        self.save_settings(cx);
        self.apply_appearance(window, cx);
    }

    fn set_background_blur(&mut self, on: bool, window: &mut Window, cx: &mut Context<Self>) {
        self.settings.background_blur = on;
        self.save_settings(cx);
        self.apply_appearance(window, cx);
    }

    // ---- highlights -------------------------------------------------------------------

    /// Whether Highlight Rules colour the Sessions of `item`: not when its Host or Project
    /// switched highlighting off.
    fn highlights_item(&self, item: &ItemKey) -> bool {
        let project_on = |id: &str| self.library.project(id).is_none_or(|p| p.highlight);
        match item {
            ItemKey::Host { project, host } => project_on(project) && self.library.host(host).is_none_or(|h| h.highlight),
            ItemKey::Shell { project, .. } => project.as_deref().is_none_or(project_on),
        }
    }

    /// After a Host or Project changed: turn each open terminal's highlighting on or off.
    fn refresh_session_highlights(&mut self, cx: &mut Context<Self>) {
        for entry in &self.sessions {
            let on = self.highlights_item(&entry.item());
            if let Some(view) = entry.ui.view.terminal() {
                view.update(cx, |v, cx| {
                    if v.highlight != on {
                        v.highlight = on;
                        cx.notify();
                    }
                });
            }
        }
    }

    fn apply_highlights(&mut self, cx: &mut Context<Self>) {
        let mut compiled = Highlighter::from_sets(&self.highlights, &self.settings.disabled_highlight_sets);
        self.highlight_skipped = compiled.skipped.clone();
        compiled.full_screen = self.settings.highlight_full_screen;
        if !self.settings.highlighting {
            compiled = Highlighter::default();
        }
        cx.set_global(ActiveHighlights(Arc::new(compiled)));
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
        self.save_settings(cx);
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

    /// Turn off the rule that coloured a word right-clicked in a terminal. It stays in its
    /// set, off, so it can be turned back on in Settings → Highlights.
    fn unhighlight(&mut self, hit: &highlight::RuleHit, cx: &mut Context<Self>) {
        let source = &hit.source;
        let Some(rule) = self.highlights.iter_mut().find(|s| s.id == source.set).and_then(|s| s.keywords.get_mut(source.index))
        else {
            return;
        };
        if !rule.enabled {
            return;
        }
        rule.enabled = false;
        // A rule written for just this word, or one that also colours other text.
        let just_this = !rule.is_regex() && rule.text.eq_ignore_ascii_case(hit.matched.trim());
        self.save_highlights(cx);
        let word = hit.matched.trim();
        let message = if just_this {
            format!("“{word}” is no longer highlighted. Turn it back on in Settings → Highlights.")
        } else {
            format!("Turned off the rule that highlighted “{word}”, along with what else it matched. Turn it back on in Settings → Highlights.")
        };
        self.show_toast_for(message, Duration::from_secs(6), cx);
        cx.notify();
    }

    fn delete_highlight_set(&mut self, id: &str, cx: &mut Context<Self>) {
        self.highlights.retain(|s| s.id != id);
        self.settings.disabled_highlight_sets.retain(|d| d != id);
        self.save_settings(cx);
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
                let plural = crate::ui::widgets::plural;
                let mut message = format!("Imported {}, {}", plural(added + replaced, "set"), plural(rules, "rule"));
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

    /// "Share project…": save the Project as a Project File to send to someone. It holds
    /// where its Hosts are and how to reach them, never a password or key.
    fn share_project(&mut self, project_id: &str, window: &mut Window, cx: &mut Context<Self>) {
        let Some(project) = self.library.project(project_id) else { return };
        let Some(text) = crate::share::write(&self.library, project_id) else {
            self.show_toast("Could not share the project.", cx);
            return;
        };
        let project_name = project.name.clone();
        let hosts = self.library.hosts_of(project).count();
        let dir = dirs::document_dir().or_else(dirs::home_dir).unwrap_or_else(std::env::temp_dir);
        let name = format!("{}.{}", sanitize(&project_name), crate::share::EXTENSION);
        let picked = cx.prompt_for_new_path(&dir, Some(&name));
        cx.spawn_in(window, async move |this, cx| {
            let Ok(Ok(Some(path))) = picked.await else { return };
            let result = std::fs::write(&path, text);
            let _ = this.update(cx, |this, cx| match result {
                Ok(()) => this.show_toast_for(
                    format!(
                        "Saved {project_name} with {} to {}. It has no passwords or keys; import it under Settings → Import.",
                        if hosts == 1 { "1 host".to_string() } else { format!("{hosts} hosts") },
                        path.display()
                    ),
                    Duration::from_secs(8),
                    cx,
                ),
                Err(err) => this.show_toast(format!("Could not save {}: {err}", path.display()), cx),
            });
        })
        .detach();
    }

    // ---- vault ------------------------------------------------------------------------

    /// Ask for the master password, saying why (`reason`). What waits for it is in `pending`.
    /// Asked again while the dialog is open (two hosts opened at once), it stays as it is.
    fn show_unlock(&mut self, reason: impl Into<String>, window: &mut Window, cx: &mut Context<Self>) {
        // The password popover is anchored to a row in the sidebar.
        self.sidebar_visible = true;
        if let Some(state) = self.unlock.as_mut() {
            if self.pending.len() > 1 {
                state.reason = "The hosts you're opening sign in with passwords or keys saved in your vault.".into();
            }
            let password = state.password.clone();
            password.update(cx, |input, cx| input.focus(window, cx));
            cx.notify();
            return;
        }
        self.vault_exists = Vault::exists(&self.data_dir);
        let creating = !self.vault_exists;
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
        let focus = cx.focus_handle();
        self.unlock = Some(UnlockState { creating, focus, password, confirm, error: None, reason: reason.into() });
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
                self.vault_exists = true;
                self.unlock = None;
                self.last_activity = SystemTime::now();
                let pending = std::mem::take(&mut self.pending);
                self.lock_after_use = !pending.is_empty() && !self.settings.stay_unlocked;
                self.sync_remembered_unlock_with_notice(cx);
                if pending.is_empty() {
                    self.focus_active(window, cx);
                }
                for job in pending {
                    match job {
                        Pending::Open { project_id, host_id } => self.open_host(&project_id, &host_id, window, cx),
                        Pending::Desktop { session } => self.connect_desktop(session, window, cx),
                        Pending::SaveDesktop { host_id, protocol, user, password } => {
                            self.save_desktop_password(&host_id, protocol, user, password, cx)
                        }
                        Pending::Save { session, hop, password } => self.save_password(session, hop, password, cx),
                        Pending::Form => self.submit_form(window, cx),
                    }
                }
                self.lock_if_used(cx);
            }
            Err(err) => {
                // A vault created elsewhere since the dialog opened: ask for its password now.
                if state.creating && Vault::exists(&self.data_dir) {
                    state.creating = false;
                }
                state.error = Some(err.to_string());
            }
        }
        cx.notify();
    }

    /// "Not now", ✕ or Escape on the master password popover, or a second click on the vault row. A connection still goes ahead
    /// and asks for the server's password instead; anything else is dropped.
    fn skip_unlock(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.unlock = None;
        for job in std::mem::take(&mut self.pending) {
            match job {
                Pending::Open { project_id, host_id } => self.open_host_session(&project_id, &host_id, window, cx),
                // Connects without the saved password: the desktop asks for one.
                Pending::Desktop { session } => self.connect_desktop_without_vault(session, window, cx),
                Pending::Form => self.show_form_error(
                    "Key files and jump host passwords go into the vault: unlock it, or leave them out.",
                    cx,
                ),
                // "Remember" was ticked: say why nothing was remembered.
                Pending::Save { .. } | Pending::SaveDesktop { .. } => {
                    self.show_toast("The password wasn't saved, as the vault stayed locked. Komando asks for it next time.", cx)
                }
            }
        }
        if self.overlay.is_none() {
            self.focus_active(window, cx);
        }
        cx.notify();
    }

    /// With "Stay unlocked" off, lock the Vault once what waited for it has read it: saving
    /// and imports do at once, a new Session when it starts (see `start_when_sized`).
    fn lock_if_used(&mut self, cx: &mut Context<Self>) {
        if self.lock_after_use && !self.sessions.iter().any(|s| s.ui.waiting_for_size) {
            self.lock_after_use = false;
            self.vault = None;
            self.vault_revealed.clear();
            self.clear_remembered_unlock(cx);
            cx.notify();
        }
    }

    fn lock_vault(&mut self, cx: &mut Context<Self>) {
        self.lock_after_use = false;
        self.vault_revealed.clear();
        if self.vault.take().is_some() {
            self.vault_exists = true;
            self.show_toast("Vault locked. Open sessions stay connected.", cx);
        }
        self.clear_remembered_unlock(cx);
        cx.notify();
    }

    fn clear_remembered_unlock(&mut self, cx: &mut Context<Self>) {
        self.remembered_unlock = None;
        if let Err(err) = Vault::forget_unlock(&self.data_dir) {
            log::warn!("{err:#}");
            self.show_toast("Vault locked in this window, but its saved unlock could not be removed. Check folder permissions.", cx);
        }
    }

    fn sync_remembered_unlock(&mut self) -> anyhow::Result<()> {
        let minutes = if self.settings.stay_unlocked && !self.lock_after_use {
            self.settings.auto_lock_minutes
        } else { 0 };
        let Some(vault) = &self.vault else { return Ok(()) };
        let current = (self.last_activity, minutes);
        if self.remembered_unlock != Some(current) {
            vault.remember_unlock(minutes, self.last_activity)?;
            self.remembered_unlock = Some(current);
        }
        Ok(())
    }

    fn sync_remembered_unlock_with_notice(&mut self, cx: &mut Context<Self>) {
        if let Err(err) = self.sync_remembered_unlock() {
            log::warn!("Could not remember vault unlock: {err:#}");
            let system = if cfg!(windows) { "Windows" } else { "Komando" };
            self.show_toast(format!("Vault unlocked for this window, but {system} could not remember it across restarts."), cx);
        }
    }

    fn start_auto_lock(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        cx.spawn_in(window, async move |this, cx| {
            loop {
                cx.background_executor().timer(Duration::from_secs(20)).await;
                let alive = this.update(cx, |this, cx| {
                    let minutes = this.settings.auto_lock_minutes;
                    if minutes > 0
                        && this.vault.is_some()
                        && this.last_activity.elapsed().map_or(true, |idle| idle >= Duration::from_secs(minutes as u64 * 60))
                    {
                        this.lock_vault(cx);
                    } else if let Err(err) = this.sync_remembered_unlock() {
                        log::warn!("Could not refresh remembered vault unlock: {err:#}");
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
            let project = Arc::new(project.clone());
            items.extend(project.hosts.iter().filter_map(|id| hosts.get(id.as_str())).map(|&index| {
                SidebarItem::Host { project: project.clone(), host: Arc::new(self.library.hosts[index].clone()) }
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
            if let (ItemKey::Shell { project: None, name }, SessionKind::Local { shell }) = (&s.item(), &s.kind())
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

    /// How many Session rows show under an item: its listed Sessions, unless folded away.
    fn nested_rows(&self, key: &ItemKey, sessions: usize) -> usize {
        if sessions == 0 || self.collapsed_items.contains(key) { 0 } else { self.sessions.listed(key).len() }
    }

    fn sessions_of(&self, key: &ItemKey) -> impl Iterator<Item = &SessionEntry> {
        self.sessions.of(key)
    }

    /// Clicking a sidebar item: show its Session (reconnecting it in place if it dropped,
    /// so the earlier output stays above), or connect if it has none. `another` opens an
    /// additional Session for the same item: for a Host always another terminal, even when a
    /// click on it opens a Remote Desktop (a Host has one desktop per protocol, opened from
    /// its menu).
    fn activate_item(&mut self, item: SidebarItem, another: bool, window: &mut Window, cx: &mut Context<Self>) {
        let key = item.key();
        if !another {
            // The row is the Host's terminal while its desktops have rows of their own.
            if let Some(id) = self.sessions.item_session(&key) {
                self.activate(id, window, cx);
                let dropped = self.sessions.get(id).is_some_and(|s| !s.is_live());
                if dropped {
                    self.reconnect(id, window, cx);
                }
                return;
            }
        }
        match item {
            SidebarItem::Host { project, host } => match host.opens_with() {
                _ if another => self.open_host(&project.id, &host.id, window, cx),
                Protocol::Ssh => self.open_host(&project.id, &host.id, window, cx),
                protocol => self.open_desktop(&project.id, &host.id, protocol, window, cx),
            },
            SidebarItem::Shell { project, shell } => self.open_local(project.map(|p| p.id.clone()), shell, window, cx),
        }
    }

    fn add_session(
        &mut self,
        item: ItemKey,
        kind: SessionKind,
        base_label: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Entity<TerminalView> {
        // A new Session shows under its item even if the item's Sessions were folded away.
        self.collapsed_items.remove(&item);
        let scrollback = self.settings.scrollback_lines;
        let pointer = self.settings.pointer.clone();
        let highlight = self.highlights_item(&item);
        let windows_selection = self.settings.windows_selection;
        let option_as_meta = self.settings.option_as_meta;
        let cursor_shape = self.settings.cursor_shape;
        let cursor_blink = self.settings.cursor_blink;
        let command_rail = self.settings.command_rail;
        // Kept per Host: Local Sessions have their shell's own history.
        let command_history = self.settings.command_history && matches!(kind, SessionKind::Ssh { .. });
        // Not in a split yet; `activate` below tells it if it opened into one.
        let minimap = self.minimap_shown(false);
        let minimap_overlay = self.settings.minimap_placement == crate::model::MinimapPlacement::Over;
        let minimap_delay = std::time::Duration::from_millis(self.settings.minimap_delay_ms.into());
        let project_id = match &item {
            ItemKey::Host { project, .. } => Some(project.clone()),
            ItemKey::Shell { project, .. } => project.clone(),
        };
        if let Some(project_id) = &project_id {
            self.set_project_collapsed(project_id, false, cx);
        }
        let mut made = None;
        let id = self.sessions.add(item.clone(), kind, base_label, |id, label| {
            let view = cx.new(|cx| {
                let mut view = TerminalView::new(label.clone(), scrollback, window, cx);
                view.pointer = pointer;
                view.highlight = highlight;
                view.windows_selection = windows_selection;
                view.option_as_meta = option_as_meta;
                view.set_cursor(cursor_shape, cursor_blink);
                view.command_rail = command_rail;
                view.command_history = command_history;
                view.minimap = minimap;
                view.minimap_overlay = minimap_overlay;
                view.minimap_delay = minimap_delay;
                view
            });
            let sub = cx.subscribe_in(&view, window, move |this, view, event: &TerminalEvent, window, cx| match event {
                TerminalEvent::Changed => {
                    let status = view.read(cx).status.clone();
                    // Remote Desktops waiting to go through this Session can connect now.
                    let connected = matches!(status, Status::Connected)
                        && this.sessions.get(id).is_some_and(|s| *s.status() != Status::Connected);
                    if connected {
                        cx.defer_in(window, move |this, window, cx| this.ssh_connected(id, window, cx));
                    }
                    this.sessions.set_busy(id, view.read(cx).busy);
                    let changed = this.sessions.set_status(id, status.clone());
                    if let Some(entry) = this.sessions.get_mut(id) {
                        entry.ui.title = view.read(cx).title.clone();
                        // The file browser follows its terminal's connection.
                        if changed && let Some(browser) = entry.ui.browser.clone() {
                            match status {
                                Status::Connected if entry.ui.sftp_open => {
                                    let sink = view.read(cx).sink();
                                    browser.update(cx, |b, cx| b.connect(sink, cx));
                                }
                                Status::Disconnected(_) => browser.update(cx, |b, cx| b.disconnected(cx)),
                                _ => {}
                            }
                        }
                    }
                    if let Some(entry) = this.sessions.get(id)
                        && this.completion_watches.update(id, entry.busy(), entry.status())
                    {
                        let label = entry.label().to_string();
                        this.notify_output_done(id, &label, cx);
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
                            let host = this.hop_target(id, *hop).map(|target| this.hop_name(&target));
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
                    // A new Session opens alone and without a file browser: only a pane laid out
                    // like that has its size. Half a split would start the next one at half width.
                    let alone = !this.sessions.shown_split().is_some_and(|s| s.contains(id))
                        && this.sessions.get(id).is_some_and(|s| !s.ui.sftp_open);
                    if alone {
                        this.pane_size = view.read(cx).measured_size().or(this.pane_size);
                    }
                    this.start_waiting(cx);
                }
                TerminalEvent::Entered { line, at } => this.record_command(id, line, *at, cx),
                TerminalEvent::Unhighlight { rule } => this.unhighlight(rule, cx),
                // In a split, only the split's minimap goes.
                TerminalEvent::HideMinimap if this.sessions.in_split(id) && this.settings.minimap_in_split => {
                    this.settings.minimap_in_split = false;
                    this.apply_terminal_settings(cx);
                    this.show_toast("Minimap hidden in split view. Turn it back on in Settings → Terminal.", cx);
                }
                TerminalEvent::HideMinimap => {
                    this.settings.minimap = false;
                    this.apply_terminal_settings(cx);
                    this.show_toast("Minimap hidden. Turn it back on in Settings → Terminal.", cx);
                }
                TerminalEvent::CloseSplit => this.close_split(window, cx),
                TerminalEvent::ForwardFailed { forward, why } => this.forward_failed(id, forward, why, cx),
                TerminalEvent::StartupStopped { waited_for } => this.show_toast_for(
                    format!("Startup commands stopped: \"{}\" didn't appear within a minute.", waited_for.trim()),
                    Duration::from_secs(8),
                    cx,
                ),
                TerminalEvent::EditHost { hop } => match this.hop_target(id, *hop) {
                    Some(HopTarget::Jump(model::RouteJump { owner: model::JumpOwner::Project(project_id, _), .. })) => {
                        this.open_edit_project(project_id, window, cx)
                    }
                    Some(HopTarget::Host { project_id, host_id }) => this.open_edit_host(project_id, host_id, window, cx),
                    // A Host's own Jump Host is edited in that Host's form.
                    Some(HopTarget::Jump(_)) => {
                        let entry = this.sessions.get(id);
                        if let Some((Some(project_id), SessionKind::Ssh { host_id })) = entry.map(|e| (e.project_id().map(str::to_string), e.kind().clone())) {
                            this.open_edit_host(project_id, host_id, window, cx);
                        }
                    }
                    None => {}
                },
            });
            made = Some(view.clone());
            SessionParts::new(SessionView::Terminal(view), label, sub)
        });
        // Into the split waiting for it, if any (see `Sessions::add`).
        self.activate(id, window, cx);
        made.expect("Sessions::add builds the payload")
    }

    fn open_local(&mut self, project_id: Option<String>, shell: LocalShell, window: &mut Window, cx: &mut Context<Self>) {
        let item = ItemKey::Shell { project: project_id.clone(), name: shell.name.clone() };
        let kind = SessionKind::Local { shell: shell.clone() };
        let view = self.add_session(item, kind.clone(), &shell.name, window, cx);
        self.start_when_sized(&view, cx);
    }

    /// Open a new Session for a Host. If any Host on the way has a saved Credential, the
    /// Vault is unlocked first (or skipped, and passwords are asked instead).
    fn open_host(&mut self, project_id: &str, host_id: &str, window: &mut Window, cx: &mut Context<Self>) {
        let needs_vault = self
            .library
            .resolve(project_id, host_id)
            .is_some_and(|host| host.credential.is_some() || host.jump_hosts.iter().any(|j| j.jump.credential.is_some()));
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
        let view = self.add_session(item, kind.clone(), &resolved.name, window, cx);
        self.start_when_sized(&view, cx);
    }

    /// Start a new Session's backend at the terminal pane's size: the size of the pane last
    /// laid out, or, before any was, once this one is. A shell started at a guessed size and
    /// resized as soon as the pane appears repaints, and Windows' pseudo-console leaves blank
    /// lines above the prompt when it does.
    fn start_when_sized(&mut self, view: &Entity<TerminalView>, cx: &mut Context<Self>) {
        let Some(id) = self.sessions.iter().find(|s| s.ui.view.terminal() == Some(view)).map(|s| s.id()) else { return };
        // Opened into a split: it gets half the room, which the last pane laid out may not
        // have had. Wait for its own pane.
        let in_split = self.sessions.in_split(id);
        let Some(entry) = self.sessions.get_mut(id) else { return };
        let size = view.read(cx).measured_size().or(self.pane_size.filter(|_| !in_split));
        match size {
            Some(size) => {
                view.update(cx, |v, _| v.assume_size(size));
                let (kind, project_id) = (entry.kind().clone(), entry.project_id().map(str::to_string));
                self.start_backend(view, &kind, project_id.as_deref(), cx);
            }
            None => {
                entry.ui.waiting_for_size = true;
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
            .filter_map(|s| {
                let view = s.ui.view.terminal()?.clone();
                std::mem::take(&mut s.ui.waiting_for_size).then(|| (view, s.kind().clone(), s.project_id().map(str::to_string)))
            })
            .collect();
        for (view, kind, project_id) in waiting {
            if let Some(size) = self.pane_size {
                view.update(cx, |v, _| v.assume_size(size));
            }
            self.start_backend(&view, &kind, project_id.as_deref(), cx);
        }
        self.lock_if_used(cx);
    }

    fn start_backend(&mut self, view: &Entity<TerminalView>, kind: &SessionKind, project_id: Option<&str>, cx: &mut Context<Self>) {
        let (term, proxy, size) = {
            let v = view.read(cx);
            (v.term(), v.proxy(), v.grid_size())
        };
        let sink = match kind {
            SessionKind::Local { shell } => {
                view.update(cx, |v, _| v.begin_connect(None, vec![], false));
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
                let Some(host) = project_id.and_then(|p| self.library.resolve(p, host_id)) else {
                    view.update(cx, |v, _| v.begin_connect(None, vec![], false));
                    proxy.send(session::SessionEvent::Status(Status::Disconnected("This host is no longer saved.".into())));
                    return;
                };
                let cards = (!host.jump_hosts.is_empty()).then(|| {
                    host.jump_hosts.iter().map(|j| jump_card(&j.jump, &host.color)).chain([route_card(&host)]).collect()
                });
                view.update(cx, |v, _| v.begin_connect(cards, host.startup_commands.clone(), true));
                let secret = |credential: Option<&str>| {
                    credential.and_then(|id| self.vault.as_ref()?.get(id)).map(|c| c.secret.clone())
                };
                let mut hops: Vec<ssh::Hop> = host
                    .jump_hosts
                    .iter()
                    .map(|j| ssh::Hop {
                        secret: secret(j.jump.credential.as_deref()),
                        name: j.jump.address.clone(),
                        address: j.jump.address.clone(),
                        port: j.jump.port,
                        user: j.jump.user.clone(),
                    })
                    .collect();
                let history_on = crate::history::address_label(&host.address, host.port);
                if let Some(entry) = self.sessions.iter_mut().find(|s| s.ui.view.terminal() == Some(view)) {
                    entry.ui.history_on = Some(history_on);
                }
                let secret = |credential: Option<&str>| {
                    credential.and_then(|id| self.vault.as_ref()?.get(id)).map(|c| c.secret.clone())
                };
                hops.push(ssh::Hop {
                    secret: secret(host.credential.as_deref()),
                    name: host.name,
                    address: host.address,
                    port: host.port,
                    user: host.user,
                });
                let target = ssh::Target { hops, known_hosts: self.data_dir.join("known_hosts"), forwards: host.port_forwards };
                Some(ssh::spawn(target, size, term, proxy))
            }
            SessionKind::Desktop { .. } => return,
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
        self.cancel_completion_notification(id);
        let Some(entry) = self.sessions.get(id) else { return };
        let (view, kind, project_id) = (entry.ui.view.clone(), entry.kind().clone(), entry.project_id().map(str::to_string));
        let SessionView::Terminal(view) = view else {
            self.connect_desktop(id, window, cx);
            self.focus_active(window, cx);
            return;
        };
        view.update(cx, |v, _| v.close());
        self.start_backend(&view, &kind, project_id.as_deref(), cx);
        self.focus_active(window, cx);
    }

    /// What one hop of a Session's way is: one of its Jump Hosts, or the Host at its end.
    fn hop_target(&self, session_id: usize, hop: usize) -> Option<HopTarget> {
        let entry = self.sessions.get(session_id)?;
        let SessionKind::Ssh { host_id } = entry.kind() else { return None };
        let project_id = entry.project_id()?;
        let host = self.library.resolve(project_id, host_id)?;
        match host.jump_hosts.get(hop) {
            Some(jump) => Some(HopTarget::Jump(jump.clone())),
            None if hop == host.jump_hosts.len() => Some(HopTarget::Host { project_id: project_id.into(), host_id: host_id.clone() }),
            None => None,
        }
    }

    /// A hop's name as the user knows it: the Host's name, or the Jump Host's address.
    fn hop_name(&self, target: &HopTarget) -> String {
        match target {
            HopTarget::Jump(jump) => jump.jump.address.clone(),
            HopTarget::Host { host_id, .. } => self.library.host(host_id).map(|h| h.name.clone()).unwrap_or_default(),
        }
    }

    fn activate(&mut self, id: usize, window: &mut Window, cx: &mut Context<Self>) {
        match self.sessions.activate(id) {
            sessions::Shown::Here => {}
            // A desktop in a window of its own: that window comes to the front.
            sessions::Shown::InOwnWindow => {
                if let Some(handle) = self.sessions.get(id).and_then(|s| s.ui.window) {
                    let _ = handle.update(cx, |_, window, _| window.activate_window());
                }
                cx.notify();
                return;
            }
            sessions::Shown::Gone => return,
        }
        if matches!(self.page, Page::Settings(_)) {
            self.page = Page::Sessions;
        }
        // It may have just gone into a split (see `Sessions::add`).
        self.sync_split_views(cx);
        self.focus_active(window, cx);
        cx.notify();
    }

    /// Browse files from the sidebar: show the Session with its file browser open.
    fn show_files(&mut self, id: usize, window: &mut Window, cx: &mut Context<Self>) {
        self.activate(id, window, cx);
        if self.sessions.get(id).is_some_and(|s| !s.ui.sftp_open) {
            self.toggle_sftp(id, window, cx);
        }
    }

    /// Show or hide the SFTP file browser beside an SSH Session's terminal.
    fn toggle_sftp(&mut self, id: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(entry) = self.sessions.get_mut(id) else { return };
        let SessionKind::Ssh { host_id } = entry.kind().clone() else { return };
        if entry.ui.sftp_open {
            entry.ui.sftp_open = false;
            self.focus_active(window, cx);
            cx.notify();
            return;
        }
        // Not connected (yet): nothing to browse. The title bar's button says so.
        if !matches!(entry.status(), Status::Connected) {
            return;
        }
        if entry.ui.browser.is_none() {
            let entry = &*entry;
            let (label, target) = match entry.project_id().and_then(|p| self.library.resolve(p, &host_id)) {
                Some(r) => {
                    let user = r.user.clone().map(|u| format!("{u}@")).unwrap_or_default();
                    (r.name.clone(), format!("{user}{}:{}", r.address, r.port))
                }
                None => (entry.label().to_string(), String::new()),
            };
            let (favorites, transfers, show_hidden) = (self.favorites.clone(), self.transfers.clone(), self.settings.sftp_show_hidden);
            let extras = self.sftp_menu_extras();
            let browser = cx.new(|cx| {
                let mut browser = FileBrowser::new(host_id.clone(), label.into(), target.into(), favorites, transfers, show_hidden, window, cx);
                browser.menu_extras = extras;
                browser
            });
            let sub = cx.subscribe_in(&browser, window, move |this, browser, event: &BrowserEvent, window, cx| match event {
                BrowserEvent::Close => this.toggle_sftp(id, window, cx),
                BrowserEvent::ShowHidden(show) => {
                    this.settings.sftp_show_hidden = *show;
                    this.save_settings(cx);
                }
                BrowserEvent::Retry => {
                    let sink = this.sessions.get(id).and_then(|s| s.ui.view.terminal()?.read(cx).sink());
                    browser.update(cx, |b, cx| b.connect(sink, cx));
                }
                BrowserEvent::JumpTo(dir) => {
                    let Some(view) = this.sessions.get(id).and_then(|s| s.ui.view.terminal().cloned()) else { return };
                    let line = file_browser::cd_command(dir);
                    if view.update(cx, |v, cx| v.run_line(&line, cx)) {
                        view.update(cx, |v, cx| v.focus_prompt(window, cx));
                    } else {
                        browser.update(cx, |b, cx| b.jump_refused(cx));
                    }
                }
            });
            let observer = cx.observe(&browser, move |this, browser, cx| {
                let busy = browser.read(cx).indicator();
                let Some(entry) = this.sessions.get_mut(id) else { return };
                if entry.ui.sftp_busy != busy {
                    entry.ui.sftp_busy = busy;
                    busy.schedule_redraws(cx);
                    cx.notify();
                }
            });
            let Some(entry) = self.sessions.get_mut(id) else { return };
            entry.ui.browser = Some(browser);
            entry.ui._browser_subscription = Some(sub);
            entry.ui._browser_observer = Some(observer);
        }
        let Some(entry) = self.sessions.get_mut(id) else { return };
        entry.ui.sftp_open = true;
        let browser = entry.ui.browser.clone().expect("browser");
        if matches!(entry.status(), Status::Connected) && !browser.read(cx).is_connected() {
            let sink = entry.ui.view.terminal().and_then(|v| v.read(cx).sink());
            browser.update(cx, |b, cx| b.connect(sink, cx));
        }
        let handle = browser.read(cx).focus_handle(cx);
        window.focus(&handle, cx);
        cx.notify();
    }

    /// Drag the divider between terminal and file browser: the browser takes what is right
    /// of the pointer, up to the right edge of its pane (`pane`).
    fn resize_sftp(&mut self, x: f32, pane: gpui::Bounds<gpui::Pixels>, cx: &mut Context<Self>) {
        // The pointer is on the middle of the gap left of the browser card.
        let right_edge = f32::from(pane.right()) - ISLAND_GAP / 2.;
        let max = (f32::from(pane.size.width) - 300.).max(320.);
        let width = (right_edge - x).clamp(320., max);
        if (width - self.settings.sftp_width).abs() >= 1. {
            self.settings.sftp_width = width;
            cx.notify();
        }
    }

    fn sidebar_width(&self, window: &Window) -> f32 {
        sidebar_width(self.settings.sidebar_width, f32::from(window.bounds().size.width))
    }

    fn finish_sidebar_resize(&mut self, cx: &mut Context<Self>) {
        if self.sidebar_resizing {
            self.sidebar_resizing = false;
            self.save_settings(cx);
            cx.notify();
        }
    }

    fn render_sidebar_divider(&self, p: AppPalette, cx: &mut Context<Self>) -> AnyElement {
        let workspace = cx.entity().downgrade();
        div()
            .id("sidebar-divider")
            .group("sidebar-divider")
            .w(px(ISLAND_GAP))
            .h_full()
            .flex_shrink_0()
            .flex()
            .justify_center()
            .py(px(14.))
            .cursor_col_resize()
            .tooltip(|window, cx| Tooltip::new("Drag to resize sidebar").build(window, cx))
            .on_drag(SidebarDivider, move |_, _, _, cx| {
                let _ = workspace.update(cx, |this, cx| {
                    this.sidebar_resizing = true;
                    this.drop_target = None;
                    cx.notify();
                });
                let workspace = workspace.clone();
                cx.new(|cx| {
                    // Release covers cancellation and letting go outside the window too.
                    cx.on_release(move |_, cx| {
                        let _ = workspace.update(cx, |this, cx| this.finish_sidebar_resize(cx));
                    }).detach();
                    SidebarDivider
                })
            })
            .child(
                div()
                    .w(px(2.))
                    .h_full()
                    .rounded_full()
                    .when(self.sidebar_resizing, |el| el.bg(p.brand))
                    .group_hover("sidebar-divider", |s| s.bg(p.brand.opacity(0.6))),
            )
            .into_any_element()
    }

    fn sftp_menu_extras(&self) -> MenuExtras {
        MenuExtras {
            rename: self.settings.sftp_menu_rename,
            copy_to: self.settings.sftp_menu_copy_to,
            move_to: self.settings.sftp_menu_move_to,
        }
    }

    /// Give the keyboard to the active Session. The master password popover doesn't hold on
    /// to it: switching Sessions while it's open types into the terminal, not the popover.
    fn focus_active(&self, window: &mut Window, cx: &mut Context<Self>) {
        // A dialog the sidebar's keys opened has closed: back to the sidebar.
        if self.nav.after_dialog.take() && !self.modal_open(cx) {
            window.focus(&self.nav.focus, cx);
            return;
        }
        if let Some(entry) = self.active_entry() {
            // The open question's field if it has one, else the terminal or desktop.
            match &entry.ui.view {
                SessionView::Terminal(view) => view.update(cx, |view, cx| view.focus_prompt(window, cx)),
                SessionView::Desktop(view) => view.update(cx, |view, cx| view.focus_prompt(window, cx)),
            }
        } else {
            window.focus(&self.focus, cx);
        }
    }

    fn toggle_sidebar(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        // Keep the Vault's pending question visible until it is answered or dismissed.
        if self.modal_open(cx) || self.unlock.is_some() {
            return;
        }
        self.sidebar_visible = !self.sidebar_visible;
        // Pinning a floated-out sidebar keeps it where it is.
        self.sidebar_peek = false;
        self.sidebar_peek_timer = None;
        if !self.sidebar_visible {
            self.transfers.update(cx, |transfers, cx| transfers.close(cx));
        }
        if self.page == Page::Sessions {
            self.focus_active(window, cx);
        } else {
            window.focus(&self.focus, cx);
        }
        cx.notify();
    }

    /// Expand sidebar on hover: whether the hidden sidebar may float out over the Sessions.
    fn sidebar_can_peek(&self) -> bool {
        self.settings.sidebar_auto_hide && !self.sidebar_visible && self.page == Page::Sessions && self.unlock.is_none()
    }

    /// Float the sidebar out at once (`true`), or slide it back once the pointer has been off
    /// it for a moment, so brushing past its edge doesn't close it. A pending change the
    /// other way is cancelled; one the same way keeps its timer.
    fn peek_sidebar_later(&mut self, open: bool, window: &mut Window, cx: &mut Context<Self>) {
        if self.sidebar_peek == open {
            self.sidebar_peek_timer = None;
            return;
        }
        if open {
            self.set_sidebar_peek(true, window, cx);
            return;
        }
        if self.sidebar_peek_timer.as_ref().is_some_and(|(target, _)| *target == open) {
            return;
        }
        let task = cx.spawn_in(window, async move |this, cx| {
            cx.background_executor().timer(Duration::from_millis(SIDEBAR_PEEK_CLOSE_MS)).await;
            let _ = this.update_in(cx, |this, window, cx| this.set_sidebar_peek(false, window, cx));
        });
        self.sidebar_peek_timer = Some((open, task));
    }

    /// Float the sidebar out or slide it back now.
    fn set_sidebar_peek(&mut self, open: bool, window: &mut Window, cx: &mut Context<Self>) {
        self.sidebar_peek_timer = None;
        if open == self.sidebar_peek || (open && !self.sidebar_can_peek()) {
            return;
        }
        self.sidebar_peek = open;
        if open {
            self.sidebar_peek_closing = None;
            self.watch_sidebar_peek(window, cx);
        } else {
            self.sidebar_peek_watch = None;
            self.transfers.update(cx, |transfers, cx| transfers.close(cx));
            self.sidebar_peek_closing = Some(cx.spawn(async move |this, cx| {
                cx.background_executor().timer(Duration::from_millis(SIDEBAR_PEEK_OUT_MS)).await;
                let _ = this.update(cx, |this, cx| {
                    this.sidebar_peek_closing = None;
                    cx.notify();
                });
            }));
        }
        cx.notify();
    }

    /// While the sidebar is out, follow the pointer outside the window too (gpui stops
    /// reporting it there). Out through the left edge, the sidebar stays until the pointer is
    /// `SIDEBAR_PEEK_OUTSIDE` past it; out any other way, it slides back as if left inside.
    fn watch_sidebar_peek(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if crate::platform::pointer_in_window(window).is_none() {
            return;
        }
        self.sidebar_peek_watch = Some(cx.spawn_in(window, async move |this, cx| {
            // Whether the pointer left through the left edge, once it is outside.
            let mut left_edge: Option<bool> = None;
            loop {
                cx.background_executor().timer(Duration::from_millis(16)).await;
                let alive = this.update_in(cx, |this, window, cx| {
                    if !this.sidebar_peek {
                        return false;
                    }
                    let Some((x, y)) = crate::platform::pointer_in_window(window) else { return false };
                    let size = window.viewport_size();
                    let (w, h) = (f32::from(size.width), f32::from(size.height));
                    if x >= 0. && x < w && y >= 0. && y < h {
                        left_edge = None;
                        return true;
                    }
                    let through_left = *left_edge.get_or_insert(x < 0. && y >= 0. && y < h);
                    if !through_left {
                        this.sidebar_peek_pointer(f32::MAX, window, cx);
                    } else if x < -SIDEBAR_PEEK_OUTSIDE || y < 0. || y >= h {
                        if this.sidebar_peek_may_close(cx) {
                            this.set_sidebar_peek(false, window, cx);
                        }
                    } else {
                        this.sidebar_peek_timer = None;
                    }
                    true
                });
                if !alive.unwrap_or(false) {
                    break;
                }
            }
        }));
    }

    /// Whether nothing opened from the floated-out sidebar is still in use.
    fn sidebar_peek_may_close(&self, cx: &App) -> bool {
        !cx.has_active_drag() && !self.transfers.read(cx).is_open() && !self.modal_open(cx)
    }

    /// The pointer moved over the Sessions while the sidebar is floated out: slide it back
    /// once the pointer is off it, unless something opened from it is still in use.
    fn sidebar_peek_pointer(&mut self, x: f32, window: &mut Window, cx: &mut Context<Self>) {
        if !self.sidebar_peek {
            return;
        }
        let right = self.sidebar_width(window) + ISLAND_GAP * 2.;
        if x <= right {
            self.peek_sidebar_later(true, window, cx);
        } else if self.sidebar_peek_may_close(cx) {
            self.peek_sidebar_later(false, window, cx);
        }
    }

    /// The thin strip along the left edge that floats the hidden sidebar out, and the
    /// sidebar itself while it is out. Laid over the Sessions; nothing moves underneath.
    fn render_sidebar_peek(&mut self, p: AppPalette, window: &Window, cx: &mut Context<Self>) -> Option<AnyElement> {
        if !self.sidebar_can_peek() {
            self.sidebar_peek = false;
            self.sidebar_peek_timer = None;
            self.sidebar_peek_closing = None;
            self.sidebar_peek_watch = None;
            return None;
        }
        let edge = div()
            .id("sidebar-peek-edge")
            .absolute()
            .left_0()
            .top_0()
            .bottom_0()
            .w(px(SIDEBAR_PEEK_EDGE))
            .on_hover(cx.listener(|this, hovered: &bool, window, cx| {
                if *hovered {
                    this.peek_sidebar_later(true, window, cx);
                }
            }));
        if !self.sidebar_peek && self.sidebar_peek_closing.is_none() {
            return Some(edge.into_any_element());
        }
        let width = self.sidebar_width(window);
        // Only the panels cover the terminal. An opaque canvas on the padded wrapper
        // paints a solid strip around them (black in dark themes with window blur).
        let mut overlay_palette = p;
        overlay_palette.sidebar.a = 1.;
        let panels = self.render_sidebar(overlay_palette, window, cx);
        let sidebar = div()
                .id("sidebar-peek")
                .absolute()
                .left_0()
                .top_0()
                .bottom_0()
                .w(px(width + ISLAND_GAP * 2.))
                .pl(px(ISLAND_GAP))
                .pr(px(ISLAND_GAP))
                .pb(px(ISLAND_GAP))
                .flex()
                .child(panels);
        let full = width + ISLAND_GAP * 2.;
        if !self.sidebar_peek {
            // Sliding back: the edge strip under it brings it straight out again.
            let closing = sidebar.with_animation(
                "sidebar-peek-out",
                Animation::new(Duration::from_millis(SIDEBAR_PEEK_OUT_MS)).with_easing(crate::ui::easing::ease),
                move |el, t| el.left(px(-full * t)),
            );
            return Some(div().absolute().inset_0().child(closing).child(edge).into_any_element());
        }
        Some(
            sidebar
                .occlude()
                .on_hover(cx.listener(|this, hovered: &bool, window, cx| {
                    if *hovered {
                        this.peek_sidebar_later(true, window, cx);
                    }
                }))
                .with_animation(
                    "sidebar-peek-in",
                    Animation::new(Duration::from_millis(SIDEBAR_PEEK_IN_MS)).with_easing(crate::ui::easing::spring),
                    move |el, t| el.left(px(-full * (1. - t))),
                )
                .into_any_element(),
        )
    }

    fn active_entry(&self) -> Option<&SessionEntry> {
        self.sessions.active_entry()
    }

    /// Whether the active Session has a question open (a password, a host key…). It is a
    /// modal: nothing else in the window can be used, and no other dialog opens, until it is
    /// answered. The master password popover is not one: it leaves the window usable.
    fn prompt_open(&self, cx: &App) -> bool {
        self.active_entry().is_some_and(|entry| entry.ui.view.has_prompt(cx))
    }

    /// Whether the keyboard is in the master password popover.
    fn unlock_focused(&self, window: &Window, cx: &App) -> bool {
        self.unlock.as_ref().is_some_and(|state| state.focus.contains_focused(window, cx))
    }

    /// Whether a dialog is up, so the window's shortcuts and other dialogs must wait.
    fn modal_open(&self, cx: &App) -> bool {
        self.overlay.is_some() || self.prompt_open(cx)
    }

    /// End a Session as the user asked: close or disconnect it now, or ask first (see
    /// `Sessions::ending`). With "Keep sessions after disconnecting" on, closing a live
    /// terminal only disconnects it, keeping its output until closed again; an unexpected
    /// disconnect keeps either kind of Session.
    fn end_session(&mut self, id: usize, request: EndRequest, window: &mut Window, cx: &mut Context<Self>) {
        // A dialog is up (the question, perhaps): nothing closes behind it.
        if request != EndRequest::CloseNow && self.modal_open(cx) {
            return;
        }
        let ending = self.sessions.ending(id, request, self.settings.keep_disconnected_sessions);
        let question = match ending {
            Ending::Disconnect => return self.disconnect_session(id, window, cx),
            Ending::Remove => return self.remove_session(id, window, cx),
            Ending::Nothing => return,
            Ending::Ask(question) => question,
        };
        let Some(name) = self.sessions.get(id).map(|entry| self.sessions.display_name(entry)) else { return };
        self.overlay = Some(match question {
            Question::CloseWhilePrinting => Overlay::Confirm {
                title: format!("Close {name}?"),
                body: format!("{name} is still printing output, so a command may be running. Closing the session ends it."),
                action: ConfirmAction::Close(id),
            },
            Question::DisconnectWhilePrinting => Overlay::Confirm {
                title: format!("Disconnect {name}?"),
                body: format!("{name} is still printing output, so a command may be running. Disconnecting ends it; the output so far stays until you close the session."),
                action: ConfirmAction::Disconnect(id),
            },
            Question::CloseDesktopByKey => Overlay::Confirm {
                title: format!("Close {name}?"),
                body: "Ctrl+Shift+W is Komando's shortcut for closing a session, so programs on the remote desktop don't receive it. Closing ends the remote desktop session.".into(),
                action: ConfirmAction::Close(id),
            },
        });
        cx.notify();
    }

    /// End a Session's connection but keep it, with its output, to reconnect or close later.
    fn disconnect_session(&mut self, id: usize, window: &mut Window, cx: &mut Context<Self>) {
        self.cancel_completion_notification(id);
        let Some(entry) = self.sessions.get(id) else { return };
        if !entry.is_live() {
            return;
        }
        match entry.ui.view.clone() {
            SessionView::Terminal(view) => view.update(cx, |v, cx| {
                v.close();
                v.set_status(Status::Disconnected("Session closed.".into()), cx);
            }),
            SessionView::Desktop(view) => view.update(cx, |v, cx| v.end("Session closed.", cx)),
        }
        self.focus_active(window, cx);
        cx.notify();
    }

    /// End a Session (if still connected) and take it out of the sidebar.
    fn remove_session(&mut self, id: usize, window: &mut Window, cx: &mut Context<Self>) {
        self.cancel_completion_notification(id);
        if !self.sessions.contains(id) {
            return;
        }
        // The other half of a split fills the area; otherwise stay with the item and show
        // another of its Sessions, if it has one.
        if let Some(next) = self.discard_sessions(|s| s.id() == id, cx) {
            self.activate(next, window, cx);
        }
        self.focus_active(window, cx);
        cx.notify();
    }

    /// Disconnect all of an item's Sessions (or close them once dropped), asking once first if
    /// any is still printing output, unless `confirmed`.
    fn end_item_sessions(&mut self, key: &ItemKey, confirmed: bool, window: &mut Window, cx: &mut Context<Self>) {
        if !confirmed && self.modal_open(cx) {
            return;
        }
        match self.sessions.ending_all(key, confirmed) {
            EndingAll::Disconnect(ids) => ids.into_iter().for_each(|id| self.disconnect_session(id, window, cx)),
            EndingAll::Remove(ids) => ids.into_iter().for_each(|id| self.remove_session(id, window, cx)),
            EndingAll::Ask { printing, live } => {
                let name = self.item_for_key(key).map(|item| item.label().to_string()).unwrap_or_default();
                let printing = if printing == 1 { "One of them is".to_string() } else { format!("{printing} of them are") };
                self.overlay = Some(Overlay::Confirm {
                    title: if live == 1 { format!("Disconnect the session of {name}?") } else { format!("Disconnect all {live} sessions of {name}?") },
                    body: format!("{printing} still printing output, so a command may be running. Disconnecting ends it; the output so far stays until you close the sessions."),
                    action: ConfirmAction::DisconnectAll(key.clone()),
                });
                cx.notify();
            }
        }
    }

    /// Rename session…: a name of its own for a Session, shown in the sidebar and title bar.
    fn open_rename_session(&mut self, id: usize, window: &mut Window, cx: &mut Context<Self>) {
        if self.modal_open(cx) {
            return;
        }
        let Some(entry) = self.sessions.get(id) else { return };
        let current = entry.name().unwrap_or(entry.label()).to_string();
        let name = cx.new(|cx| InputState::new(window, cx).default_value(current));
        let subscription = cx.subscribe_in(&name, window, move |this, _, event: &InputEvent, window, cx| {
            if matches!(event, InputEvent::PressEnter { .. }) {
                this.save_session_name(window, cx);
            }
        });
        name.update(cx, |f, cx| f.focus(window, cx));
        self.overlay = Some(Overlay::RenameSession { id, name, _subscription: subscription });
        cx.notify();
    }

    fn save_session_name(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(Overlay::RenameSession { id, name, .. }) = &self.overlay else { return };
        let (id, value) = (*id, name.read(cx).value().trim().to_string());
        if let Some(entry) = self.sessions.get(id) {
            // The field starts with the host's name; keeping it means no name of its own.
            let name: Option<SharedString> = (!value.is_empty() && value != entry.label().as_ref()).then(|| value.into());
            let view = entry.ui.view.clone();
            self.sessions.set_name(id, name.clone());
            // A desktop in a window of its own shows it in that window's title.
            if let SessionView::Desktop(view) = &view {
                view.update(cx, |view, cx| {
                    view.session_name = name;
                    cx.notify();
                });
            }
        }
        self.close_overlay(window, cx);
    }

    fn render_rename_session(&self, name: &Entity<InputState>, p: AppPalette, cx: &mut Context<Self>) -> AnyElement {
        form_card(
            p,
            "Rename session",
            div()
                .flex()
                .flex_col()
                .gap(px(8.))
                .child(Input::new(name))
                .child(
                    div()
                        .text_size(px(12.))
                        .line_height(px(17.))
                        .text_color(p.text_muted)
                        .child("Shown in the sidebar and the title bar until the session is closed. Leave it empty to show the shell's title again."),
                )
                .into_any_element(),
            vec![
                button("rename-cancel", "Cancel", ButtonKind::Ghost, p)
                    .on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.close_overlay(window, cx)))
                    .into_any_element(),
                button("rename-ok", "Rename", ButtonKind::Primary, p)
                    .on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.save_session_name(window, cx)))
                    .into_any_element(),
            ],
        )
    }

    fn cycle(&mut self, forward: bool, window: &mut Window, cx: &mut Context<Self>) {
        let items: Vec<ItemKey> = self.sidebar_items().iter().map(|item| item.key()).collect();
        if let Some(next) = self.sessions.cycle(&items, forward) {
            self.activate(next, window, cx);
        }
    }

    /// The item of the active Session, for "new session on this item".
    fn active_item(&self) -> Option<SidebarItem> {
        self.item_for_key(&self.active_entry()?.item().clone())
    }

    /// The sidebar item a key stands for, if it is still there.
    fn item_for_key(&self, key: &ItemKey) -> Option<SidebarItem> {
        match key.clone() {
            ItemKey::Host { project, host } => {
                let project = self.library.project(&project)?;
                if !project.hosts.contains(&host) { return None; }
                Some(SidebarItem::Host { project: Arc::new(project.clone()), host: Arc::new(self.library.host(&host)?.clone()) })
            }
            ItemKey::Shell { project: Some(id), name } => {
                let project = self.library.project(&id)?;
                let shell = project.shells.iter().find(|sh| sh.name == name)?.clone();
                Some(SidebarItem::Shell { project: Some(Arc::new(project.clone())), shell })
            }
            ItemKey::Shell { project: None, name } => self.library.standalone_shells().into_iter()
                .chain(self.orphan_shells()).find(|sh| sh.name == name)
                .map(|shell| SidebarItem::Shell { project: None, shell }),
        }
    }

    // ---- passwords & output -----------------------------------------------------------

    /// Save a user name typed for one hop of a Session: as the Host's own, or for the Jump
    /// Host where it is saved.
    fn save_user(&mut self, session_id: usize, hop: usize, user: String, cx: &mut Context<Self>) {
        let mut library = self.library.clone();
        let saved = match self.hop_target(session_id, hop) {
            Some(HopTarget::Jump(jump)) => library.jump_host_mut(&jump.owner).map(|j| j.user = Some(user)),
            Some(HopTarget::Host { host_id, .. }) => {
                library.hosts.iter_mut().find(|h| h.id == host_id).map(|h| h.user = model::Inherited::Own(user))
            }
            None => None,
        };
        if saved.is_none() {
            return;
        }
        match library.save(&self.data_dir) {
            Ok(_) => {
                self.library = library;
                self.show_toast("User name saved.", cx);
            }
            Err(err) => self.show_toast(format!("Could not save: {err}"), cx),
        }
    }

    /// Save a password typed for a Jump Host on a Session's way, where that Jump Host is
    /// saved. It gets a Credential of its own; the one it had goes once nothing uses it.
    fn save_jump_password(&mut self, jump: model::RouteJump, password: String, cx: &mut Context<Self>) {
        let Some(vault) = self.vault.as_mut() else { return };
        let label = format!("{} (jump host)", jump.jump.label());
        let cred_id = match vault.add(Credential { label, secret: Secret::Password { password } }) {
            Ok(id) => id,
            Err(err) => return self.show_toast(format!("Could not save: {err}"), cx),
        };
        let mut library = self.library.clone();
        let Some(saved) = library.jump_host_mut(&jump.owner) else {
            let _ = vault.remove(&cred_id);
            return;
        };
        let before = saved.credential.replace(cred_id.clone());
        match library.save(&self.data_dir) {
            Ok(_) => {
                self.library = library;
                self.forget_unused_credentials(before);
                self.show_toast("Password saved to the vault.", cx);
            }
            Err(err) => {
                let _ = vault.remove(&cred_id);
                self.show_toast(format!("Could not save the password: {err:#}"), cx);
            }
        }
    }

    /// Take Credentials out of the Vault that nothing in the Library uses any more.
    pub(super) fn forget_unused_credentials(&mut self, ids: impl IntoIterator<Item = model::CredentialId>) {
        let Some(vault) = self.vault.as_mut() else { return };
        for id in ids {
            if self.library.credential_users(&id) == 0 {
                let _ = vault.remove(&id);
            }
        }
    }

    /// Save a password typed for one hop of a Session for that hop's Host.
    fn save_password(&mut self, session_id: usize, hop: usize, password: String, cx: &mut Context<Self>) {
        let (project_id, host_id) = match self.hop_target(session_id, hop) {
            Some(HopTarget::Host { project_id, host_id }) => (project_id, host_id),
            Some(HopTarget::Jump(jump)) => return self.save_jump_password(jump, password, cx),
            None => return,
        };
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
        let added = existing.is_none();
        let result = match existing {
            Some(id) => vault.replace(&id, credential).map(|_| id),
            None => vault.add(credential),
        };
        let cred_id = match result {
            Ok(id) => id,
            Err(err) => return self.show_toast(format!("Could not save: {err}"), cx),
        };
        // Replacing an existing Credential changes only the Vault, which is already saved.
        // The Host still refers to the same id; no Library write or rollback is needed.
        if !added {
            self.show_toast("Password saved to the vault.", cx);
            return;
        }
        let Some(index) = self.library.hosts.iter().position(|h| h.id == host_id) else { return };
        let before = std::mem::replace(&mut self.library.hosts[index].credential, model::Inherited::Own(cred_id.clone()));
        match self.library.save(&self.data_dir) {
            Ok(_) => self.show_toast("Password saved to the vault.", cx),
            Err(err) => {
                // The Host would lose its new Credential at the next start: undo both, so
                // what is shown is what is saved.
                self.library.hosts[index].credential = before;
                if let Some(vault) = self.vault.as_mut() {
                    let _ = vault.remove(&cred_id);
                }
                self.show_toast(format!("Could not save the password: {err:#}"), cx);
            }
        }
    }

    fn copy_output_text(&mut self, cx: &mut Context<Self>) {
        let Some(view) = self.active_entry().and_then(|e| e.ui.view.terminal()) else { return };
        let text = view.read(cx).output_text();
        let lines = text.lines().count();
        cx.write_to_clipboard(gpui::ClipboardItem::new_string(text));
        self.show_toast(format!("Copied {} as text.", crate::ui::widgets::plural(lines, "line")), cx);
    }

    fn copy_output_file(&mut self, cx: &mut Context<Self>) {
        let Some(entry) = self.active_entry() else { return };
        let Some(view) = entry.ui.view.terminal() else { return };
        let text = view.read(cx).output_text();
        let name = format!(
            "{}_{}.txt",
            sanitize(entry.label()),
            chrono::Local::now().format("%Y-%m-%d_%H-%M-%S")
        );
        match crate::export::write_and_copy_file(&name, &text) {
            Ok(note) => self.show_toast(format!("Copied {name}{note}"), cx),
            Err(err) => self.show_toast(format!("Could not create file: {err:#}"), cx),
        }
        // On Linux and macOS the clipboard gets the path as text instead.
        #[cfg(not(windows))]
        cx.write_to_clipboard(gpui::ClipboardItem::new_string(
            crate::export::export_dir().join(&name).display().to_string(),
        ));
    }

    fn show_toast(&mut self, message: impl Into<SharedString>, cx: &mut Context<Self>) {
        self.show_toast_for(message, Duration::from_secs(4), cx);
    }

    fn show_toast_for(&mut self, message: impl Into<SharedString>, duration: Duration, cx: &mut Context<Self>) {
        let shown = Instant::now();
        self.toast = Some((message.into(), shown));
        cx.notify();
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(duration).await;
            let _ = this.update(cx, |this, cx| {
                // Unless a newer toast replaced it.
                if this.toast.as_ref().is_some_and(|(_, at)| *at == shown) {
                    this.toast = None;
                    cx.notify();
                }
            });
        })
        .detach();
    }

    /// Keep the sidebar and its saved expansion state in one place.
    fn set_project_collapsed(&mut self, project: &str, collapsed: bool, cx: &mut Context<Self>) {
        let changed = if collapsed {
            self.settings.collapsed_projects.insert(project.to_owned())
        } else {
            self.settings.collapsed_projects.remove(project)
        };
        if changed {
            self.save_settings(cx);
            cx.notify();
        }
    }

    /// Save the settings, and say so when that fails instead of dropping the change silently.
    fn save_settings(&mut self, cx: &mut Context<Self>) {
        if let Err(err) = self.settings.save(&self.data_dir) {
            self.show_toast(format!("Could not save settings: {err:#}"), cx);
        }
    }

    // ---- overlays: picker and forms ----------------------------------------------------

    fn open_picker(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        // Replacing an open form would lose what was typed into it.
        if self.modal_open(cx) {
            return;
        }
        let query = cx.new(|cx| InputState::new(window, cx).placeholder("Search hosts and shells…"));
        let sub = cx.subscribe_in(&query, window, |this, query, event: &InputEvent, window, cx| match event {
            // A new query starts again at its best match.
            InputEvent::Change => {
                let found = this.picker_results(&query.read(cx).value());
                if let Some(Overlay::Picker { results, selected, scroll, .. }) = &mut this.overlay {
                    *results = found;
                    *selected = 0;
                    scroll.scroll_to_item(0);
                }
                cx.notify()
            }
            // Ctrl+Enter: another Session on it, like Ctrl+click in the sidebar.
            InputEvent::PressEnter { secondary, .. } => {
                let item = match &this.overlay {
                    Some(Overlay::Picker { results, selected, .. }) if !results.is_empty() => {
                        Some(results[(*selected).min(results.len() - 1)].item.clone())
                    }
                    _ => None,
                };
                if let Some(item) = item {
                    this.pick(item, *secondary, window, cx);
                }
            }
            _ => {}
        });
        self._subscriptions.push(sub);
        query.update(cx, |q, cx| q.focus(window, cx));
        let results = self.picker_results("");
        self.overlay = Some(Overlay::Picker { query, results, selected: 0, scroll: ScrollHandle::new() });
        cx.notify();
    }

    /// Move quick connect's selection one result down (or up), wrapping at the ends, and
    /// scroll it into view.
    fn step_picker(&mut self, down: bool, cx: &mut Context<Self>) {
        let Some(Overlay::Picker { results, selected, scroll, .. }) = &mut self.overlay else { return };
        let count = results.len();
        if count == 0 {
            return;
        }
        let current = (*selected).min(count - 1);
        *selected = if down { (current + 1) % count } else { (current + count - 1) % count };
        scroll.scroll_to_item(*selected);
        cx.notify();
    }

    /// Quick connect: every sidebar item, filtered by name, address (with its port), user or
    /// Project.
    fn picker_results(&self, query: &str) -> Vec<PickerResult> {
        let needle = query.to_lowercase();
        let found = |hay: String| needle.is_empty() || hay.to_lowercase().contains(&needle);
        self.sidebar_items()
            .into_iter()
            .filter_map(|item| {
                let (icon, color, name, detail) = match &item {
                    SidebarItem::Host { project, host } => {
                        let r = model::resolve(host, project);
                        let dest = destination(&r);
                        // `user@address:port` as shown, so a pasted destination finds its Host.
                        if !found(format!("{} {} {} {}", host.name, dest, project.name, r.port)) {
                            return None;
                        }
                        let via = match r.jump_hosts.as_slice() {
                            [] => String::new(),
                            [one] => format!(" · via {}", one.jump.address),
                            more => format!(" · via {} jump hosts", more.len()),
                        };
                        (r.icon.clone(), Some(project_color(&r.color)), host.name.clone(), format!("{dest}{via} · {}", project.name))
                    }
                    SidebarItem::Shell { project, shell } => {
                        if !found(format!("{} {} local", shell.name, project.as_ref().map(|p| p.name.as_str()).unwrap_or(""))) {
                            return None;
                        }
                        let detail = match project {
                            Some(project) => format!("Local shell · {}", project.name),
                            None => "Local shell".to_string(),
                        };
                        (crate::icons::shell_icon(shell).to_string(), None, shell.name.clone(), detail)
                    }
                };
                Some(PickerResult { item, icon, color, name: name.into(), detail: detail.into() })
            })
            .collect()
    }

    /// Open what quick connect found, and show where it is in the sidebar: its Project
    /// unfolds and its row scrolls into view.
    fn pick(&mut self, item: SidebarItem, another: bool, window: &mut Window, cx: &mut Context<Self>) {
        self.overlay = None;
        let key = item.key();
        if let ItemKey::Host { project, .. } | ItemKey::Shell { project: Some(project), .. } = &key {
            self.set_project_collapsed(project, false, cx);
        }
        self.activate_item(item, another, window, cx);
        self.nav.cursor = Some(NavTarget::Item(key));
        self.nav.reveal = true;
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
            0 if project.hosts.is_empty() => "It has no hosts. Open sessions in this project are closed.".to_string(),
            0 => "Its hosts are also in other projects and stay there. Open sessions in this project are closed.".to_string(),
            1 => "Its 1 host is deleted too. Open sessions in this project are closed.".to_string(),
            n => format!("Its {n} hosts are deleted too. Open sessions in this project are closed."),
        };
        self.overlay = Some(Overlay::Confirm { title: format!("Delete {}?", project.name), body, action: ConfirmAction::DeleteProject(project_id) });
        cx.notify();
    }

    fn run_confirm(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(Overlay::Confirm { action, .. }) = self.overlay.take() else { return };
        let mut library = self.library.clone();
        match &action {
            ConfirmAction::DeleteHost(id) => library.remove_host(id),
            ConfirmAction::DeleteProject(id) => library.remove_project(id),
            ConfirmAction::Disconnect(id) => {
                self.disconnect_session(*id, window, cx);
                return;
            }
            ConfirmAction::Close(id) => {
                self.end_session(*id, EndRequest::CloseNow, window, cx);
                return;
            }
            ConfirmAction::DisconnectAll(key) => {
                self.end_item_sessions(key, true, window, cx);
                return;
            }
            ConfirmAction::DeleteAllHistory => {
                self.delete_all_history(cx);
                self.focus_active(window, cx);
                return;
            }
            ConfirmAction::DeleteCredential(id) => {
                self.delete_credential(id, cx);
                self.focus_active(window, cx);
                return;
            }
            ConfirmAction::InstallUpdate => {
                self.install_update(cx);
                self.focus_active(window, cx);
                return;
            }
            ConfirmAction::RestartForUpdate => {
                self.restart_for_update(cx);
                return;
            }
        }
        // Persist before closing Sessions or removing sidebar state: a failed deletion
        // should leave the saved library and its running Sessions intact.
        if let Err(err) = library.save(&self.data_dir) {
            self.show_toast(format!("Could not delete: {err:#}"), cx);
            self.focus_active(window, cx);
            return;
        }
        // A deleted Host's Command History goes with it.
        for gone in self.library.hosts.iter().filter(|h| library.host(&h.id).is_none()) {
            if let Err(err) = crate::history::forget(&self.data_dir, &gone.id) {
                log::warn!("deleting the history of {}: {err:#}", gone.id);
            }
        }
        self.library = library;
        match action {
            // What is shown next comes up without leaving the page the user is on.
            ConfirmAction::DeleteHost(id) => {
                let next = self.discard_sessions(
                    |s| matches!(s.kind(), SessionKind::Ssh { host_id } | SessionKind::Desktop { host_id, .. } if *host_id == id),
                    cx,
                );
                if let Some(next) = next {
                    self.sessions.activate(next);
                }
            }
            ConfirmAction::DeleteProject(id) => {
                if let Some(next) = self.discard_sessions(|s| s.project_id() == Some(id.as_str()), cx) {
                    self.sessions.activate(next);
                }
                self.set_project_collapsed(&id, false, cx);
            }
            ConfirmAction::DeleteAllHistory
            | ConfirmAction::DeleteCredential(_)
            | ConfirmAction::InstallUpdate
            | ConfirmAction::RestartForUpdate
            | ConfirmAction::Disconnect(_)
            | ConfirmAction::Close(_)
            | ConfirmAction::DisconnectAll(_) => {}
        }
        self.focus_active(window, cx);
        cx.notify();
    }

    /// Close and drop every Session that matches, e.g. those of a deleted Host. Returns what
    /// to show if the shown Session went; the caller shows it.
    fn discard_sessions(&mut self, matches: impl Fn(&SessionEntry) -> bool, cx: &mut Context<Self>) -> Option<usize> {
        let removed = self.sessions.remove_where(matches);
        for entry in &removed.entries {
            self.cancel_completion_notification(entry.id());
            if let Some(handle) = entry.ui.window {
                let _ = handle.update(cx, |_, window, _| window.remove_window());
            }
            entry.ui.view.close(cx);
        }
        // The other half of a split one of them was in has its minimap back.
        self.sync_split_views(cx);
        removed.show
    }

    fn copy_host_address(&mut self, project_id: &str, host_id: &str, cx: &mut Context<Self>) {
        let Some(r) = self.library.resolve(project_id, host_id) else { return };
        let text = destination(&r);
        cx.write_to_clipboard(gpui::ClipboardItem::new_string(text.clone()));
        self.show_toast(format!("Copied {text}"), cx);
    }

    /// A press on the backdrop beside a dialog. Quick connect, a question and the history
    /// close; a host, project, shell or highlight form stays, since a stray click would throw away what was
    /// typed into it. Escape, Cancel and the close button still close it.
    fn backdrop_pressed(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !matches!(self.overlay, Some(Overlay::HostForm(_) | Overlay::ProjectForm(_) | Overlay::ShellForm | Overlay::RuleForm(_) | Overlay::SetForm(_) | Overlay::Tour(_))) {
            self.close_overlay(window, cx);
        }
    }

    fn close_overlay(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if matches!(self.overlay, Some(Overlay::ShellForm)) {
            self.clear_shell_editor(window, cx);
        }
        if matches!(self.overlay, Some(Overlay::Tour(_))) {
            self.tour_closed(cx);
        }
        self.overlay = None;
        if matches!(self.page, Page::Settings(_)) {
            window.focus(&self.focus, cx);
        } else {
            self.focus_active(window, cx);
        }
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

/// One hop of a Session's way (see `Workspace::hop_target`).
enum HopTarget {
    Jump(model::RouteJump),
    Host { project_id: String, host_id: String },
}

/// How a Jump Host shows on the route page, in the colour of the Host it leads to.
fn jump_card(jump: &model::JumpHost, color: &str) -> RouteCard {
    RouteCard { name: jump.address.clone(), detail: jump.label(), icon: "network".into(), color: project_color(color) }
}

/// Where a Host connects to: `user@address`, with `:port` when it isn't 22.
fn destination(hop: &model::ResolvedHost) -> String {
    let user = hop.user.as_deref().map(|u| format!("{u}@")).unwrap_or_default();
    let port = if hop.port == 22 { String::new() } else { format!(":{}", hop.port) };
    format!("{user}{}{port}", hop.address)
}

/// How the Host at the end of the way shows on the route page.
fn route_card(hop: &model::ResolvedHost) -> RouteCard {
    RouteCard {
        name: hop.name.clone(),
        detail: destination(hop),
        icon: hop.icon.clone(),
        color: project_color(&hop.color),
    }
}

fn sanitize(s: &str) -> String {
    s.chars().map(|c| if c.is_alphanumeric() || c == '-' || c == '.' { c } else { '_' }).collect()
}

// ---- rendering -------------------------------------------------------------------------

impl Workspace {
    /// The sidebar's vault row, above Transfers: there while the Vault is locked, gone once
    /// it is unlocked. A click opens the master password popover out of it (a second click
    /// closes it), and anything else that needs the Vault opens the popover here too. Before a
    /// Vault exists the row shows only while the popover asks for a new master password.
    pub(super) fn render_vault_row(&self, compact: bool, p: AppPalette, cx: &mut Context<Self>) -> Option<AnyElement> {
        let open = self.unlock.as_ref();
        if open.is_none() && (self.vault.is_some() || !self.vault_exists) {
            return None;
        }
        let creating = open.is_some_and(|state| state.creating);
        let label = if creating { "Set up vault" } else { "Vault locked" };
        let row = div()
            .id("vault-row")
            .role(gpui::Role::Button)
            .aria_label(if open.is_some() { label.to_string() } else { format!("{label}, unlock") })
            .tab_index(0)
            .focus_visible(move |s| crate::ui::widgets::focus_outline(s, p.focus))
            .relative()
            .h(px(34.))
            .px(px(10.))
            .flex()
            .items_center()
            .gap(px(8.))
            .rounded(p.corner(8.))
            .border_1()
            .border_color(p.brand.opacity(if open.is_some() { 0.6 } else { 0.3 }))
            .bg(p.brand_soft)
            .cursor_pointer()
            .text_size(px(12.5))
            .font_weight(FontWeight::MEDIUM)
            .text_color(p.active_text)
            .hover(|s| s.bg(p.brand_soft))
            .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                if this.unlock.is_some() {
                    this.skip_unlock(window, cx);
                } else {
                    this.show_unlock("Unlock to connect with your saved passwords and keys.", window, cx);
                }
            }))
            .child(icon_el("lock-keyhole", 14., p.active_text))
            .child(div().flex_1().min_w_0().truncate().child(label))
            .when(open.is_none() && !compact, |el| el.child(div().flex_shrink_0().whitespace_nowrap().text_size(px(11.5)).text_color(p.active_text).child("Unlock")))
            .child(icon_el("chevron-right", 14., p.active_text))
            .children(open.map(|state| self.render_unlock(state, p, cx)));
        Some(row.into_any_element())
    }

    /// The master password popover, drawn upward from the vault row's right edge over the
    /// terminal area, its bottom level with the row. It is not a modal: there is no backdrop and
    /// the rest of the window keeps working. It must not be mistaken for a server's password
    /// question (a plain card titled "Password for …"): it carries the vault badge and a
    /// brand-coloured top edge, names Komando's vault, and says under the field that this is
    /// not the server's password.
    fn render_unlock(&self, state: &UnlockState, p: AppPalette, cx: &mut Context<Self>) -> AnyElement {
        let p = p.panel();
        let creating = state.creating;
        let waiting = !self.pending.is_empty();
        let skip_label = if self.pending.iter().any(|job| matches!(job, Pending::Open { .. })) {
            "Use server password"
        } else {
            "Not now"
        };
        let header = div()
            .flex()
            .items_start()
            .gap(px(12.))
            .child(badge("lock-keyhole", p.brand, 36.))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .gap(px(3.))
                    .child(
                        div()
                            .text_size(px(10.5))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(p.brand)
                            .child("KOMANDO VAULT"),
                    )
                    .child(
                        div()
                            .text_size(px(15.))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(p.text)
                            .child(if creating { "Create a master password" } else { "Unlock your vault" }),
                    )
                    .child(
                        div()
                            .pt(px(2.))
                            .text_size(px(12.5))
                            .line_height(px(18.))
                            .text_color(p.text_muted)
                            .child(state.reason.clone()),
                    ),
            )
            // Same as the skip button or Escape.
            .child(
                close_x("unlock-close", p)
                    .flex_shrink_0()
                    .mt(px(-4.))
                    .mr(px(-6.))
                    .on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.skip_unlock(window, cx))),
            );

        // Under the field, one line that keeps it apart from a server's password question.
        let hint = |icon: &str, text: &'static str, color: Hsla| {
            div()
                .flex()
                .items_start()
                .gap(px(6.))
                .text_size(px(12.))
                .line_height(px(17.))
                .text_color(color)
                .child(div().flex_shrink_0().pt(px(1.)).child(icon_el(icon, 13., color)))
                .child(div().flex_1().min_w_0().child(text))
        };
        let form = div()
            .flex()
            .flex_col()
            .gap(px(8.))
            // Inset by the focus ring's width, so the ring lines up with the card below
            // instead of reaching past it.
            .child(div().px(px(3.)).child(password_input(&state.password, p)))
            .when(creating, |el| el.child(div().px(px(3.)).child(password_input(&state.confirm, p))))
            .child(match &state.error {
                Some(error) => div()
                    .flex()
                    .items_start()
                    .gap(px(6.))
                    .text_size(px(12.))
                    .line_height(px(17.))
                    .text_color(p.danger)
                    .child(div().flex_shrink_0().pt(px(1.)).child(icon_el("circle-alert", 13., p.danger)))
                    .child(div().flex_1().min_w_0().child(error.clone()))
                    .into_any_element(),
                None if creating => hint(
                    "key-round",
                    "Protects the passwords and keys Komando saves. It is not a server's password.",
                    p.text_faint,
                )
                .into_any_element(),
                None => hint("key-round", "The password you chose for Komando, not the server's.", p.text_faint)
                    .into_any_element(),
            });

        // The one sentence that must not be skimmed past.
        let warning = creating.then(|| {
            div()
                .flex()
                .items_start()
                .gap(px(8.))
                .px(px(10.))
                .py(px(8.))
                .rounded(p.corner(8.))
                .bg(p.warning.opacity(0.1))
                .border_1()
                .border_color(p.warning.opacity(0.3))
                .text_size(px(12.))
                .line_height(px(17.))
                .text_color(p.text)
                .child(div().flex_shrink_0().pt(px(1.)).child(icon_el("triangle-alert", 13., p.warning)))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .child("There is no recovery: forget it and saved passwords are lost. Projects and hosts are kept."),
                )
        });

        // Stay unlocked for the auto-lock time, or lock again once what waits has used the
        // Vault. Only offered when something waits: unlocking from the row means to stay.
        let stay = self.settings.stay_unlocked;
        let stay_detail = if !stay {
            "Locks again right after this".to_string()
        } else {
            match self.settings.auto_lock_minutes {
                0 => "Until Komando closes".to_string(),
                minutes if cfg!(windows) => format!("Locks after {} without use, including across restarts", settings::auto_lock_label(minutes)),
                minutes => format!("Locks after {} without use", settings::auto_lock_label(minutes)),
            }
        };
        let remember = waiting.then(|| {
            div()
                .id("unlock-stay")
                .flex()
                .items_center()
                .gap(px(10.))
                .px(px(10.))
                .py(px(8.))
                .rounded(p.corner(8.))
                .bg(p.hover)
                .cursor_pointer()
                .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                    this.settings.stay_unlocked = !this.settings.stay_unlocked;
                    this.save_settings(cx);
                    this.sync_remembered_unlock_with_notice(cx);
                    cx.notify();
                }))
                .child(switch("unlock-stay-switch".into(), "Stay unlocked", stay, p))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .flex()
                        .flex_col()
                        .child(div().text_size(px(12.5)).font_weight(FontWeight::MEDIUM).text_color(p.text).child("Stay unlocked"))
                        .child(div().text_size(px(11.5)).text_color(p.text_faint).child(stay_detail)),
                )
                .child(
                    div()
                        .id("unlock-stay-change")
                        .flex_shrink_0()
                        .text_size(px(11.5))
                        .text_color(p.text_muted)
                        .cursor_pointer()
                        .hover(|s| s.text_color(p.brand))
                        .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                            cx.stop_propagation();
                            this.open_settings(SettingsSection::Security, window, cx);
                        }))
                        .child("Change"),
                )
        });

        let actions = div()
            .flex()
            .items_center()
            .gap(px(8.))
            .px(px(20.))
            .py(px(12.))
            .border_t_1()
            .border_color(p.border)
            .child(
                // Its text lines up with the content above; the hover background reaches out.
                button("unlock-skip", skip_label, ButtonKind::Ghost, p)
                    .ml(px(-12.))
                    .on_click(cx.listener(|this, _, window, cx| this.skip_unlock(window, cx))),
            )
            .child(div().flex_1())
            .child(
                button("unlock", if creating { "Create vault" } else { "Unlock" }, ButtonKind::Primary, p)
                    .on_click(cx.listener(|this, _, window, cx| this.submit_unlock(window, cx))),
            );

        let card = div()
            .id("unlock-popover")
            .track_focus(&state.focus)
            .role(gpui::Role::Dialog)
            .aria_label("Komando vault")
            // Clicks on it must not reach the terminal underneath (it would take the focus).
            .occlude()
            .relative()
            .w(px(380.))
            .rounded(p.corner(14.))
            .bg(p.surface)
            .border_1()
            .border_color(p.border)
            .child(
                div()
                    .rounded(p.corner(14.))
                    // A brand-coloured top edge, which the plain server questions don't have.
                    .border_t_4()
                    .border_color(p.brand)
                    .child(
                        div()
                            .px(px(20.))
                            .pt(px(16.))
                            .pb(px(16.))
                            .flex()
                            .flex_col()
                            .gap(px(14.))
                            .child(header)
                            .children(warning)
                            .child(form)
                            .children(remember),
                    )
                    .child(actions),
            )
            // Slides out of the row to the right as it fades in.
            .with_animation(
                "unlock-popover-in",
                Animation::new(Duration::from_millis(160)).with_easing(gpui::ease_out_quint()),
                |el, t| el.opacity(t).left(px(-10. * (1. - t))),
            );
        // From the row's right edge: past the sidebar's inner padding and the gap between the
        // islands, so the card starts just inside the terminal area and grows upward from
        // the footer row. It stays in the window when the window is short.
        div()
            .absolute()
            .bottom_0()
            .right_0()
            .size_0()
            .child(
                deferred(
                    anchored()
                        .anchor(gpui::Anchor::BottomLeft)
                        .offset(gpui::point(px(8. + ISLAND_GAP + 8.), px(4.)))
                        .snap_to_window_with_margin(px(ISLAND_GAP))
                        .child(card),
                )
                .with_priority(5),
            )
            .into_any_element()
    }

    /// The sidebar's rows, in order. Kept small (indices, counts) so working it out on every
    /// frame is cheap; only the rows in view are built into elements.
    fn sidebar_rows(&self) -> Vec<SidebarRow> {
        // Each item's Sessions, found by borrowed ids: most items have none, and need no key.
        let mut counts: HashMap<(bool, Option<&str>, &str), (&ItemKey, usize)> = HashMap::new();
        for s in &self.sessions {
            let id = match &s.item() {
                ItemKey::Host { project, host } => (true, Some(project.as_str()), host.as_str()),
                ItemKey::Shell { project, name } => (false, project.as_deref(), name.as_str()),
            };
            counts.entry(id).or_insert((&s.item(), 0)).1 += 1;
        }
        let nested = |id| counts.get(&id).map_or(0, |&(key, count)| self.nested_rows(key, count));
        let hosts = self.library.host_indices();
        let mut rows = vec![];
        let shells = self.sidebar_shells();
        for (index, shell) in shells.iter().enumerate() {
            let sessions = nested((false, None, &shell.name));
            rows.push(SidebarRow::Shell { index, sessions });
        }
        rows.push(SidebarRow::ProjectsHeader);
        if self.library.projects.is_empty() {
            rows.push(SidebarRow::NoProjects);
        }
        for (pi, project) in self.library.projects.iter().enumerate() {
            rows.push(SidebarRow::Project { index: pi });
            if self.settings.collapsed_projects.contains(&project.id) {
                continue;
            }
            let start = rows.len();
            for id in &project.hosts {
                let Some(&host) = hosts.get(id.as_str()) else { continue };
                let sessions = nested((true, Some(&project.id), id));
                rows.push(SidebarRow::Host { project: pi, host, sessions });
            }
            for (shell, sh) in project.shells.iter().enumerate() {
                let sessions = nested((false, Some(&project.id), &sh.name));
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
        let p = cx.global::<Appearance>().app.navigation();
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
                        .tooltip(|window, cx| Tooltip::new("New project").build(window, cx))
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
                let focused = self.nav_focus_on(&NavTarget::AddHost(project.id.clone()));
                div()
                        .id(SharedString::from(format!("first-host-{}", project.id)))
                        .relative()
                        .when(focused, |el| el.child(focus_ring(p.corner(8.), p)))
                        .h(px(32.))
                        .ml(px(NESTED_INSET))
                        .pl(px(NESTED_INDENT - NESTED_INSET))
                        .flex()
                        .items_center()
                        .gap(px(10.))
                        .rounded(p.corner(8.))
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
                let item = SidebarItem::Host { project: Arc::new(project.clone()), host: Arc::new(host.clone()) };
                div().flex().flex_col().gap(px(2.))
                    .children(self.render_item(&item, color, NESTED_INDENT, p, cx)).into_any_element()
            }
            SidebarRow::ProjectShell { project, shell, .. } => {
                let Some(project) = self.library.projects.get(project) else { return div().into_any_element() };
                let Some(shell) = project.shells.get(shell) else { return div().into_any_element() };
                let color = project_color(&project.color);
                let item = SidebarItem::Shell { project: Some(Arc::new(project.clone())), shell: shell.clone() };
                div().flex().flex_col().gap(px(2.))
                    .children(self.render_item(&item, color, NESTED_INDENT, p, cx)).into_any_element()
            }
        };
        // Every row of a Project's group takes a Host dropped on it (moved to that Project);
        // the whole group lights up while one is held over it.
        let project_index = match row {
            SidebarRow::Project { index }
            | SidebarRow::FirstHost { project: index }
            | SidebarRow::Host { project: index, .. }
            | SidebarRow::ProjectShell { project: index, .. } => Some(index),
            _ => None,
        };
        let project_id = project_index.and_then(|i| self.library.projects.get(i)).map(|p| p.id.clone());
        let wrapper = div().id(("sidebar-row", ix)).mx(px(4.)).px(px(4.)).pb(px(2.)).child(element);
        let Some(project_id) = project_id else { return wrapper.into_any_element() };
        let lit = self.drop_project(cx) == Some(project_id.as_str())
            || matches!(&self.drop_target, Some(DropTarget::ProjectOrder { id, .. }) if id == &project_id && cx.has_active_drag());
        let first = matches!(row, SidebarRow::Project { .. });
        let last = !matches!(
            self.sidebar_rows.get(ix + 1),
            Some(SidebarRow::FirstHost { project } | SidebarRow::Host { project, .. } | SidebarRow::ProjectShell { project, .. })
                if Some(*project) == project_index
        );
        let blue = split::drop_color();
        let (over, dropped) = (project_id.clone(), project_id);
        let (order_over, order_drop) = (over.clone(), dropped.clone());
        // A Host dragged within its Project: a line where it would go.
        let line = self.reorder_line(ix, cx);
        wrapper
            .relative()
            .when_some(line, |el, after| {
                el.child(
                    div()
                        .absolute()
                        .left(px(10.))
                        .right(px(6.))
                        .h(px(2.))
                        .rounded(px(1.))
                        .bg(blue)
                        .map(|el| if after { el.bottom(px(0.)) } else { el.top(px(0.)) }),
                )
            })
            .when(lit, |el| {
                el.bg(blue.opacity(if p.dark { 0.16 } else { 0.1 }))
                    .when(first, |el| el.rounded_t(p.corner(10.)))
                    .when(last, |el| el.rounded_b(p.corner(10.)))
            })
            .on_drag_move(cx.listener(move |this, event: &gpui::DragMoveEvent<DraggedItem>, _, cx| {
                this.drag_over_project(&over, ix, event, cx)
            }))
            .on_drop(cx.listener(move |this, dragged: &DraggedItem, _, cx| this.drop_on_project(&dropped, dragged, cx)))
            .on_drag_move(cx.listener(move |this, event: &gpui::DragMoveEvent<DraggedProject>, _, cx| {
                this.drag_project_order(&order_over, ix, event, cx)
            }))
            .on_drop(cx.listener(move |this, dragged: &DraggedProject, _, cx| this.drop_project_order(&order_drop, dragged, cx)))
            .into_any_element()
    }

    fn render_project_row(&self, index: usize, p: AppPalette, cx: &mut Context<Self>) -> AnyElement {
        let Some(project) = self.library.projects.get(index) else { return div().into_any_element() };
            let color = project_color(&project.color);
            let collapsed = self.settings.collapsed_projects.contains(&project.id);
            let sessions: Vec<&SessionEntry> = self.sessions.iter().filter(|s| match s.item() {
                ItemKey::Host { project: id, host } => id == &project.id && project.hosts.contains(host),
                ItemKey::Shell { project: id, name } => id.as_ref() == Some(&project.id) && project.shells.iter().any(|sh| &sh.name == name),
            }).collect();
            let live = sessions.iter().filter(|s| s.is_live()).count();
            let busy = sessions.iter().any(|s| self.sessions.busy_out_of_sight(s));
            // Folded away with the Session in use inside: the row says so.
            let holds_active = collapsed && sessions.iter().any(|s| Some(s.id()) == self.sessions.active());
            let focused = self.nav_focus_on(&NavTarget::Project(project.id.clone()));
            let pid = project.id.clone();
            let pid_host = project.id.clone();
            // A Host from another Project is held over this one's group.
            let move_here = self.drop_project(cx) == Some(project.id.as_str());
            let project_menu = self.project_menu(index, cx);
            let click_id = project.id.clone();
            div()
                    .id(SharedString::from(format!("project-{}", project.id)))
                    .group("project-row")
                    .role(gpui::Role::TreeItem)
                    .aria_label(project.name.clone())
                    .aria_level(1)
                    .aria_expanded(!collapsed)
                    .text_size(px(13.))
                    .relative()
                    .h(px(36.))
                    .px(px(8.))
                    .mt(px(4.))
                    .flex()
                    .items_center()
                    .gap(px(10.))
                    .rounded(p.corner(8.))
                    .cursor_pointer()
                    .tooltip(|window, cx| Tooltip::new("Drag to reorder projects").build(window, cx))
                    .on_drag(
                        DraggedProject { id: project.id.clone(), label: project.name.clone().into(), icon: project.icon.clone(), color },
                        self.project_drag_preview(cx),
                    )
                    .hover(|s| s.bg(p.hover))
                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                        this.nav_clicked(NavTarget::Project(click_id.clone()));
                        this.set_project_collapsed(&pid, !collapsed, cx);
                    }))
                    // Folded away with the Session in use inside: a bar in the Project's colour
                    // at its edge, where hovering can't be mistaken for it.
                    .when(holds_active, |el| {
                        el.child(div().absolute().left(px(0.)).top(px(9.)).bottom(px(9.)).w(px(3.)).rounded(px(2.)).bg(color))
                    })
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
                                .text_size(px(11.))
                                .text_color(p.text_faint)
                                .group_hover("project-row", |s| s.invisible())
                                .when(busy, |el| el.child(activity_icon(p)))
                                .child(format!("{live} live")),
                        )
                    })
                    .child(
                        // Like a host row's buttons: the hover colour over the sidebar's, so its
                        // backdrop matches the hovered row.
                        div()
                            .absolute()
                            .right(px(26.))
                            .top(px(6.))
                            .rounded(p.corner(7.))
                            .bg(p.sidebar)
                            .when(!focused, |el| el.invisible().group_hover("project-row", |s| s.visible()))
                            .child(
                                div().rounded(p.corner(7.)).bg(p.hover).child(
                                    icon_button(SharedString::from(format!("add-host-{}", project.id)), "plus", p)
                                        .aria_label("Add host")
                                        .size(px(24.))
                                        .tooltip(|window, cx| Tooltip::new("Add host").build(window, cx))
                                        .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                                            cx.stop_propagation();
                                            this.open_new_host(pid_host.clone(), window, cx);
                                        })),
                                ),
                            ),
                    )
                    .map(|el| {
                        if move_here {
                            el.child(
                                div()
                                    .flex_shrink_0()
                                    .text_size(px(11.5))
                                    .font_weight(FontWeight::MEDIUM)
                                    .text_color(split::drop_color())
                                    .child("Move here"),
                            )
                        } else {
                            el.child(icon_el(if collapsed { "chevron-right" } else { "chevron-down" }, 14., p.text_faint))
                        }
                    })
                    .children(self.row_menu_anchor(NavTarget::Project(project.id.clone()), cx))
                    .when(focused, |el| el.child(focus_ring(p.corner(8.), p)))
                    .context_menu(project_menu)
                .into_any_element()
    }

    /// A Project row's menu.
    fn project_menu(&self, index: usize, cx: &mut Context<Self>) -> impl Fn(PopupMenu, &mut Window, &mut Context<PopupMenu>) -> PopupMenu + Clone + 'static {
        let ws = cx.entity().downgrade();
        let id = self.library.projects.get(index).map(|p| p.id.clone()).unwrap_or_default();
        let collapsed = self.settings.collapsed_projects.contains(&id);
        move |menu: PopupMenu, _: &mut Window, _: &mut Context<PopupMenu>| {
            let (a, b, c, d, e) = (id.clone(), id.clone(), id.clone(), id.clone(), id.clone());
            menu.item(menu_item("Add host…", Some("plus"), &ws, move |this, window, cx| this.open_new_host(a.clone(), window, cx)))
                .item(menu_item("Edit project…", None, &ws, move |this, window, cx| this.open_edit_project(b.clone(), window, cx)))
                .item(menu_item("Share project…", None, &ws, move |this, window, cx| this.share_project(&e, window, cx)))
                .item(menu_item(if collapsed { "Expand" } else { "Collapse" }, None, &ws, move |this, _, cx| {
                    this.set_project_collapsed(&c, !collapsed, cx);
                }))
                .separator()
                .item(menu_item("Delete project…", Some("trash"), &ws, move |this, _, cx| this.confirm_delete_project(d.clone(), cx)))
        }
    }

    fn render_sidebar(&mut self, p: AppPalette, window: &Window, cx: &mut Context<Self>) -> AnyElement {
        let p = p.navigation();
        // Keep the primary labels readable near the 200px minimum; secondary hints return
        // once there is room for them. Use the clamped width, including in small windows.
        let width = self.sidebar_width(window);
        let compact = width < 240.;
        self.sidebar_compact = compact;
        self.sidebar_width_px = width;
        // Only the rows in view are built (hundreds of hosts would otherwise cost milliseconds
        // on every frame). Row heights are measured once and kept until the rows change.
        let rows = crate::perf::time("sidebar rows", || self.sidebar_rows());
        if rows != self.sidebar_rows {
            // Replace only the rows that changed. A reset would scroll the list to the top
            // whenever a Session opens or closes.
            let (old, new) = changed_rows(&self.sidebar_rows, &rows);
            self.sidebar_list.splice(old, new);
            self.sidebar_rows = rows;
        }
        // Keyboard use (see `nav.rs`): the cursor shows while the keyboard is in the list.
        self.nav.focused = self.nav.focus.is_focused(window);
        if self.nav.focused && self.nav.cursor.is_none() {
            self.nav.cursor = self.nav_targets(&self.sidebar_rows).first().map(|(t, _)| t.clone());
        }
        if let Some(ix) = self.take_nav_reveal() {
            self.sidebar_list.scroll_to_reveal_item(ix);
        }
        let list = div()
            .id("sidebar-list")
            .key_context(nav::CONTEXT)
            .track_focus(&self.nav.focus)
            .role(gpui::Role::Tree)
            .aria_label("Hosts and sessions")
            .flex_1()
            .min_h_0()
            .flex()
            .flex_col()
            .on_action(cx.listener(|this, _: &nav::NavUp, window, cx| if this.nav_owns(window, cx) { this.nav_step(-1, cx) }))
            .on_action(cx.listener(|this, _: &nav::NavDown, window, cx| if this.nav_owns(window, cx) { this.nav_step(1, cx) }))
            .on_action(cx.listener(|this, _: &nav::NavFirst, window, cx| if this.nav_owns(window, cx) { this.nav_step(isize::MIN, cx) }))
            .on_action(cx.listener(|this, _: &nav::NavLast, window, cx| if this.nav_owns(window, cx) { this.nav_step(isize::MAX, cx) }))
            .on_action(cx.listener(|this, _: &nav::NavExpand, window, cx| if this.nav_owns(window, cx) { this.nav_expand(cx) }))
            .on_action(cx.listener(|this, _: &nav::NavCollapse, window, cx| if this.nav_owns(window, cx) { this.nav_collapse(cx) }))
            .on_action(cx.listener(|this, _: &nav::NavOpen, window, cx| if this.nav_owns(window, cx) { this.nav_open(false, window, cx) }))
            .on_action(cx.listener(|this, _: &nav::NavOpenAnother, window, cx| if this.nav_owns(window, cx) { this.nav_open(true, window, cx) }))
            .on_action(cx.listener(|this, _: &nav::NavEdit, window, cx| if this.nav_owns(window, cx) { this.nav_edit(false, window, cx) }))
            .on_action(cx.listener(|this, _: &nav::NavEditHost, window, cx| if this.nav_owns(window, cx) { this.nav_edit(true, window, cx) }))
            .on_action(cx.listener(|this, _: &nav::NavDelete, window, cx| if this.nav_owns(window, cx) { this.nav_delete(false, window, cx) }))
            .on_action(cx.listener(|this, _: &nav::NavDeleteHost, window, cx| if this.nav_owns(window, cx) { this.nav_delete(true, window, cx) }))
            .on_action(cx.listener(|this, _: &nav::NavMenu, window, cx| if this.nav_owns(window, cx) { this.nav_menu(window, cx) }))
            // A click focuses the list too; the cursor shows again once a key is used.
            .on_mouse_down(MouseButton::Left, cx.listener(|this, _: &gpui::MouseDownEvent, _, _| this.nav.keyboard = false))
            .on_action(cx.listener(|this, _: &nav::NavLeave, window, cx| if this.nav_owns(window, cx) { this.nav_leave(window, cx) }))
            .child(
                list(self.sidebar_list.clone(), cx.processor(|this, ix, _window, cx| this.render_sidebar_row(ix, cx)))
                    .flex_1()
                    .min_h_0(),
            );

        let (attention, downloads_open, new_download, activity) = {
            let t = self.transfers.read(cx);
            (t.attention(), t.is_open(), t.new_download(), t.activity())
        };
        let ring_fraction = match activity {
            Activity::Idle => None,
            Activity::Unknown => Some(None),
            Activity::Progress(f) => Some(Some(f)),
        };
        let footer_row = |id: &'static str, icon: &str, label: String| {
            div()
                .id(id)
                .role(gpui::Role::Button)
                .aria_label(label.clone())
                .tab_index(0)
                .focus_visible(move |s| crate::ui::widgets::focus_outline(s, p.focus))
                .h(px(32.))
                .px(px(8.))
                .flex()
                .items_center()
                .gap(px(10.))
                .rounded(p.corner(8.))
                .cursor_pointer()
                .text_size(px(12.5))
                .text_color(p.text_muted)
                .hover(|s| s.bg(p.hover).text_color(p.hover_text))
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
                    .flex()
                    .gap(px(6.))
                    .child(
                        div()
                            .id("quick-connect")
                            .flex_1()
                            .min_w_0()
                            .role(gpui::Role::Button)
                            .aria_label("Quick connect")
                            .tab_index(0)
                            .focus_visible(move |s| crate::ui::widgets::focus_outline(s, p.focus))
                            .h(px(34.))
                            .px(px(10.))
                            .flex()
                            .items_center()
                            .gap(px(8.))
                            .rounded(p.corner(8.))
                            .bg(p.hover)
                            .border_1()
                            .border_color(p.border)
                            .cursor_pointer()
                            .text_size(px(12.5))
                            .text_color(p.hover_text)
                            .hover(|s| s.bg(p.active))
                            .on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.open_picker(window, cx)))
                            .child(icon_el("search", 14., p.hover_text))
                            .child(div().flex_1().min_w_0().truncate().child("Quick connect"))
                            .when(!compact, |el| el.child(kbd("Ctrl Shift K", p.highlighted()))),
                    )
                    .child(self.render_sidebar_hide_button(p, cx)),
            )
            .child(list.pb(px(8.)));

        div()
            .w(px(width))
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
                    .children(self.render_vault_row(compact, p, cx).map(|row| div().pb(px(6.)).child(row)))
                    // Always available; the number is what needs a look.
                    .child(
                        footer_row("downloads", "arrow-down-up", "Transfers".into())
                            .aria_expanded(downloads_open)
                            .when_some(ring_fraction, |el, fraction| {
                                // Around the 15px icon (8px in, centred in the 32px row):
                                // the combined progress of everything running.
                                let label = match fraction {
                                    Some(f) => format!("Transfers, {}% done", (f * 100.).round()),
                                    None => "Transfers, in progress".to_string(),
                                };
                                el.aria_label(label).child(
                                    div().absolute().left(px(8. + 7.5 - 12.)).top(px(16. - 12.)).child(progress_ring(
                                        "downloads-ring",
                                        fraction,
                                        24.,
                                        2.,
                                        p.border,
                                        p.brand,
                                    )),
                                )
                            })
                            // The new-download dot waits until nothing is running.
                            .when(new_download && ring_fraction.is_none(), |el| {
                                el.aria_label("Transfers, new download").child(
                                    // A dot on the icon's top-right corner (the 15px icon
                                    // sits 8px in, 8.5px down), ringed in the sidebar's
                                    // colour so it reads as cut out of the icon.
                                    div()
                                        .absolute()
                                        .left(px(17.))
                                        .top(px(5.5))
                                        .size(px(10.))
                                        .rounded_full()
                                        .bg(p.brand)
                                        .border_2()
                                        .border_color(p.sidebar),
                                )
                            })
                            .relative()
                            .when(downloads_open, |el| el.bg(p.active).text_color(p.active_text))
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
                                        .rounded(p.corner(6.))
                                        .bg(p.brand_soft)
                                        .text_size(px(11.))
                                        .font_weight(FontWeight::SEMIBOLD)
                                        .text_color(p.brand)
                                        .child(attention.to_string()),
                                )
                            })
                            .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                                this.transfers.update(cx, |t, cx| t.toggle(window, cx));
                            })),
                    )
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

    /// What an item's row shows, worked out from its Sessions.
    fn item_state(&self, key: &ItemKey) -> ItemState<'_> {
        let sessions: Vec<&SessionEntry> = self.sessions_of(key).collect();
        let listed = self.sessions.listed(key);
        let grouped = !listed.is_empty();
        let expanded = grouped && !self.collapsed_items.contains(key);
        let shown = self.sessions.last_shown(key).or_else(|| sessions.first().map(|s| s.id()));
        // The Session the row itself is: its only one, or the terminal beside listed desktops.
        let main = if grouped {
            sessions.iter().find(|s| !listed.iter().any(|l| l.id() == s.id())).copied()
        } else {
            shown.and_then(|id| sessions.iter().find(|s| s.id() == id).copied()).or(sessions.first().copied())
        };
        let holds_active = sessions.iter().any(|s| Some(s.id()) == self.sessions.active());
        // Shown in the other half of the split: lit, more faintly than the active row. A row
        // that is the Host's terminal is lit for that terminal alone.
        let split = self.sessions.shown_split();
        let in_split = |s: &SessionEntry| Some(s.id()) != self.sessions.active() && split.is_some_and(|split| split.contains(s.id()));
        let beside = match main {
            Some(main) if grouped => in_split(main),
            _ => !holds_active && sessions.iter().any(|s| in_split(s)),
        };
        // A group's state: live if any Session is, connecting if any is on the way. A row that
        // is the Host's terminal shows that terminal's.
        let status = if grouped && main.is_none() {
            let any = |f: fn(&Status) -> bool| sessions.iter().any(|s| f(s.status()));
            Some(if any(|s| matches!(s, Status::Connected)) {
                Status::Connected
            } else if any(|s| matches!(s, Status::Connecting)) {
                Status::Connecting
            } else {
                Status::Disconnected(String::new())
            })
        } else {
            main.map(|s| s.status().clone())
        };
        let live = matches!(status, Some(Status::Connected | Status::Connecting));
        let busy = !holds_active && !beside
            && if grouped { sessions.iter().any(|s| self.sessions.busy_out_of_sight(s)) } else { main.is_some_and(|s| self.sessions.busy_out_of_sight(s)) };
        // With the Sessions listed below, the active one is marked there instead.
        let active = main.is_some_and(|s| Some(s.id()) == self.sessions.active()) || (holds_active && !expanded);
        // In a split that stays while another Session is shown: linked to the other half. Expanded,
        // the listed Sessions show it on their own rows.
        let remembered = self.sessions.split();
        let partner = |s: &SessionEntry| remembered.and_then(|split| split.other(s.id()));
        let linked = if expanded { main.and_then(partner) } else { sessions.iter().find_map(|s| partner(s)) };
        ItemState { sessions, listed, grouped, expanded, shown, main, beside, linked, status, live, busy, active }
    }

    /// The menu of an item's row: its right-click menu, and the one its ⋯ button and
    /// Shift+F10 open.
    fn item_menu(
        &self,
        item: &SidebarItem,
        cx: &mut Context<Self>,
    ) -> impl Fn(PopupMenu, &mut Window, &mut Context<PopupMenu>) -> PopupMenu + Clone + 'static {
        let key = item.key();
        let ItemState { sessions, grouped, expanded, shown, main, linked, .. } = self.item_state(&key);
        let any_live = sessions.iter().any(|s| s.is_live());
        let session_id = main.map(|s| s.id());
        let sessions_live = main.is_some_and(|s| s.is_live());
        let open_session = self.sessions.item_session(&key).and_then(|id| sessions.iter().find(|s| s.id() == id));
        let open_live = open_session.is_some_and(|s| s.is_live());
        let visible = self.page == Page::Sessions && open_session.is_some_and(|s| {
            !s.own_window() && (Some(s.id()) == self.sessions.active() || self.sessions.shown_split().is_some_and(|split| split.contains(s.id())))
        });
        let sidebar_focus = self.nav.focus.clone();
        let ws = cx.entity().downgrade();
        let menu_item_src = item.clone();
        let menu_key = key.clone();
        // What "Disconnect all" / "Close all" acts on: the live Sessions while any is, else
        // the dropped ones (see `end_item_sessions`).
        let count = sessions.iter().filter(|s| s.is_live() == any_live).count();
        // Beside the Session shown, unless that is the one this would show.
        let can_split = self.sessions.can_place(&key, None, Side::Right);
        let history = self.settings.command_history;
        // × and Delete only disconnect a terminal; with the setting off they close it.
        let keep_sessions = self.settings.keep_disconnected_sessions && main.is_some_and(|s| s.ui.view.desktop().is_none());
        let completion_ids: Vec<usize> = sessions.iter().map(|s| s.id()).collect();
        let linked = linked.is_some();
        // Its connected terminal, for Browse files: the row's own, else the last shown.
        let files_session = main
            .into_iter()
            .chain(shown.and_then(|id| sessions.iter().find(|s| s.id() == id).copied()))
            .chain(sessions.iter().copied())
            .find(|s| matches!(s.kind(), SessionKind::Ssh { .. }) && matches!(s.status(), Status::Connected))
            .map(|s| s.id());
        move |menu: PopupMenu, _: &mut Window, cx: &mut Context<PopupMenu>| {
            let item = menu_item_src.clone();
            let (open, another) = (item.clone(), item.clone());
            let new_label = if matches!(item, SidebarItem::Host { .. }) { "New terminal" } else { "New session" };
            // Hint the sidebar's row actions, rather than global New/Close, which act on
            // the active Session and may belong to a different Host. Keep click handlers
            // bound to this menu's item regardless of the keyboard cursor's position.
            let mut menu = menu.action_context(sidebar_focus.clone()).item(
                menu_item(new_label, Some("plus"), &ws, move |this, window, cx| this.activate_item(another.clone(), true, window, cx))
                    .action(Box::new(nav::NavOpenAnother)),
            );
            if let Some(id) = files_session {
                menu = menu.item(menu_item("Browse files", Some("folder-open"), &ws, move |this, window, cx| this.show_files(id, window, cx)));
            }
            if can_split {
                let split_key = menu_key.clone();
                menu = menu.item(menu_item("Open in split view", None, &ws, move |this, window, cx| {
                    this.open_in_split(&split_key, None, Side::Right, window, cx)
                }));
            }
            if linked {
                menu = menu.item(menu_item("Close split view", Some("columns-2"), &ws, |this, window, cx| this.close_split(window, cx)));
            }
            // A dropped Session still needs Connect, even with its retained output visible.
            if !open_live || !visible {
                menu = menu.item(menu_item(if open_live { "Show session" } else { "Connect" }, None, &ws, move |this, window, cx| {
                    this.activate_item(open.clone(), false, window, cx)
                }));
            }
            if grouped {
                let (fold, all) = (menu_key.clone(), menu_key.clone());
                menu = menu
                    .item(menu_item(
                        if expanded { "Collapse sessions" } else { "Expand sessions" },
                        None,
                        &ws,
                        move |this, _, cx| this.toggle_item_sessions(&fold, cx),
                    ))
                    .item(menu_item(
                        &if any_live { format!("Disconnect all {count}") } else { format!("Close all {count}") },
                        None,
                        &ws,
                        move |this, window, cx| this.end_item_sessions(&all, false, window, cx),
                    ));
            }
            if let Some(id) = session_id {
                let live = sessions_live;
                menu = menu.item(
                    menu_item("Rename session…", None, &ws, move |this, window, cx| this.open_rename_session(id, window, cx))
                        .action(Box::new(nav::NavEdit)),
                );
                if live {
                    menu = live_session_items(menu, id, keep_sessions, &ws);
                } else {
                    menu = menu.item(
                        menu_item("Close session", None, &ws, move |this, window, cx| this.end_session(id, EndRequest::CloseNow, window, cx))
                            .action(Box::new(nav::NavDelete)),
                    );
                }
            }
            menu = notifications::completion_menu(menu, &ws, &completion_ids, cx);
            if let SidebarItem::Host { project, host } = &item {
                let (pa, ha) = (project.id.clone(), host.id.clone());
                let (pb, hb) = (project.id.clone(), host.id.clone());
                let (pd, hd) = (project.id.clone(), host.id.clone());
                let hc = host.id.clone();
                // Both desktops are offered by default. An explicit opt-out hides one;
                // an unconfigured one opens its settings to add its port and password.
                let desktops: Vec<Protocol> = [Protocol::Rdp, Protocol::Vnc].into_iter().filter(|protocol| host.desktop_enabled(*protocol)).collect();
                if !desktops.is_empty() {
                    menu = menu.separator();
                    // A Host that opens its desktop on a click offers its terminal here instead.
                    if !host.opens_with().is_ssh() {
                        let (pt, ht) = (project.id.clone(), host.id.clone());
                        menu = menu.item(menu_item("Open terminal", None, &ws, move |this, window, cx| {
                            this.show_terminal_of(&pt, &ht, window, cx)
                        }));
                    }
                    for protocol in desktops {
                        let (pr, hr) = (project.id.clone(), host.id.clone());
                        menu = if host.desktop(protocol).is_some() {
                            menu.item(menu_item(format!("Open {}", protocol.label()), None, &ws, move |this, window, cx| {
                                this.open_desktop(&pr, &hr, protocol, window, cx)
                            }))
                        } else {
                            menu.item(menu_item(format!("Set up {}…", protocol.label()), None, &ws, move |this, window, cx| {
                                this.set_up_desktop(pr.clone(), hr.clone(), protocol, window, cx)
                            }))
                        };
                    }
                }
                menu = menu
                    .separator()
                    .item(menu_item("Edit host…", None, &ws, move |this, window, cx| this.open_edit_host(pa.clone(), ha.clone(), window, cx)))
                    .item(menu_item("Duplicate host…", None, &ws, move |this, window, cx| {
                        this.duplicate_host(pd.clone(), hd.clone(), window, cx)
                    }))
                    .item(menu_item("Copy address", None, &ws, move |this, _, cx| this.copy_host_address(&pb, &hb, cx)));
                if history {
                    let hh = host.id.clone();
                    menu = menu.item(menu_item("Command history…", None, &ws, move |this, window, cx| {
                        this.open_history(&hh, window, cx)
                    }));
                }
                menu = menu
                    .separator()
                    .item(menu_item("Delete host…", Some("trash"), &ws, move |this, _, cx| this.confirm_delete_host(hc.clone(), cx)));
            }
            // In an elevated window every Session is elevated already.
            // Windows only: elsewhere there is no administrator mode to start a shell in.
            if let SidebarItem::Shell { project, shell } = &item
                && cfg!(windows)
                && !crate::elevate::is_elevated()
            {
                let (project, name) = (project.as_ref().map(|p| p.id.clone()), shell.name.clone());
                menu = menu.separator().item(menu_item("Run as administrator", None, &ws, move |this, window, cx| {
                    this.run_as_admin(project.clone(), name.clone(), window, cx)
                }));
            }
            menu
        }
    }

    /// One sidebar item, plus a row for each of its Sessions when it has several.
    ///
    /// Live items are bright with a status dot; idle ones are dimmed; a dropped Session
    /// that still holds its output shows a hollow ring until it is reconnected or discarded.
    /// An item with several Sessions is a group: its row shows how many and folds them away,
    /// and each Session gets a card under it, with its own close button.
    fn render_item(&self, item: &SidebarItem, color: Hsla, indent: f32, p: AppPalette, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let key = item.key();
        let ItemState { listed, grouped, expanded, main, beside, linked, status, live, busy, active, .. } = self.item_state(&key);
        let icon = item.icon();
        let icon_size = if matches!(item, SidebarItem::Shell { .. }) { 16. } else { 15. };
        let icon_color = item.icon_color(&icon, if active || live { color } else { p.text_faint }, p);
        let listed_count = listed.len();
        let item_menu = self.item_menu(item, cx);

        let item_for_click = item.clone();
        let item_for_new = item.clone();
        let toggle_key = key.clone();
        let (click_key, fold_key) = (key.clone(), key.clone());
        let is_host = matches!(item, SidebarItem::Host { .. });
        let focused = self.nav_focus_on(&NavTarget::Item(key.clone()));
        let menu_open = self.row_menu_open(&NavTarget::Item(key.clone()));
        let dragged = self.dragged(item, None, color);
        // Shell identities stay clear while idle; their labels and status carry activity.
        let dim = !(active || live) && is_artwork(&icon) && matches!(item, SidebarItem::Host { .. });
        // The row's end: its state, and for a group how many Sessions and the fold chevron.
        // Folded out, the row keeps the group's state (or its terminal's, when it is one): the
        // Sessions' cards only mark what needs a look.
        let show_status = status.is_some();
        // The Session the row itself stands for closes from the row: its status dot turns into
        // × on hover, and a middle-click closes it, as Settings → Close from the sidebar says.
        let quick_close = self.settings.quick_close;
        let row_session = main.filter(|_| show_status).map(|s| s.id());
        let hover_close = row_session.filter(|_| quick_close.on_hover());
        let middle_close = row_session.filter(|_| quick_close.on_middle_click());
        let close_tip: SharedString = match main {
            Some(s) if s.is_live() && self.settings.keep_disconnected_sessions && s.ui.view.desktop().is_none() => "Disconnect".into(),
            _ => "Close session".into(),
        };
        let show_count = grouped && !expanded;
        let trailing: f32 = [(show_status, 16.), (show_count, 14.), (grouped, 22.)]
            .iter()
            .filter(|(shown, _)| *shown)
            .map(|(_, w)| w + 4.)
            .sum::<f32>();
        let pad_right = if grouped { 6. } else { 10. };
        let new_tip: SharedString = if is_host { "New terminal (Ctrl+click)".into() } else { "New session (Ctrl+click)".into() };
        let more = self.menu_popover(
            SharedString::from(format!("more-{key:?}")),
            NavTarget::Item(key.clone()),
            MenuTrigger::new(
                icon_button(SharedString::from(format!("more-button-{key:?}")), "ellipsis", p)
                    .aria_label("More actions")
                    .size(px(24.))
                    .tooltip(|window, cx| Tooltip::new("More actions").build(window, cx)),
            ),
            cx,
        );
        // A name given to the Session the row stands for, after the Host's.
        let session_name = main.and_then(|s| s.name().clone());
        let label_color = if active { p.active_text } else if live { p.text } else { p.text_muted };
        let mut rows = vec![];
        rows.push(
            div()
                .id(SharedString::from(format!("item-{key:?}")))
                .group("item-row")
                .role(gpui::Role::TreeItem)
                .aria_label(match &session_name {
                    Some(name) => format!("{}, {name}", item.label()),
                    None => item.label().to_string(),
                })
                .aria_level(if indent > 10. { 2 } else { 1 })
                .aria_selected(active)
                .when(grouped, |el| el.aria_expanded(expanded))
                // Also the size of its right-click menu, which inherits it.
                .text_size(px(13.))
                .relative()
                .h(px(34.))
                // A nested row's highlight starts inside its Project's, so the tree reads as one.
                .when(indent > 10., |el| el.ml(px(NESTED_INSET)))
                .pl(px(if indent > 10. { indent - NESTED_INSET } else { indent }))
                .pr(px(pad_right))
                .flex()
                .items_center()
                .gap(px(10.))
                .rounded(p.corner(8.))
                .cursor_pointer()
                .when(active, |el| el.bg(p.active).text_color(p.active_text))
                .when(!active, |el| el.hover(|s| s.bg(p.hover)))
                .when(beside && !active && (!expanded || main.is_some()), |el| el.bg(p.hover))
                .on_click(cx.listener(move |this, event: &ClickEvent, window, cx| {
                    this.nav_clicked(NavTarget::Item(click_key.clone()));
                    this.activate_item(item_for_click.clone(), event.modifiers().control, window, cx)
                }))
                .when_some(middle_close, |el, id| {
                    el.on_aux_click(cx.listener(move |this, event: &ClickEvent, window, cx| {
                        if event.is_middle_click() {
                            cx.stop_propagation();
                            this.end_session(id, EndRequest::Close, window, cx);
                        }
                    }))
                })
                // Drag it onto the terminal area to open it beside, or onto another Project.
                .on_drag(dragged, self.drag_preview(cx))
                .child(
                    div()
                        .flex_shrink_0()
                        .when(dim, |el| el.opacity(0.55))
                        .child(icon_el(&icon, icon_size, icon_color)),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .flex()
                        .items_center()
                        .gap(px(5.))
                        .child(
                            div()
                                .min_w_0()
                                .flex_shrink_1()
                                .truncate()
                                .text_color(label_color)
                                .when(!active, |el| el.group_hover("item-row", |s| s.text_color(p.hover_text)))
                                .child(item.label().to_string()),
                        )
                        .when_some(session_name, |el, name| {
                            el.child(
                                div()
                                    .min_w_0()
                                    .flex_shrink_1()
                                    .truncate()
                                    .text_size(px(12.5))
                                    .text_color(if active { p.active_text.opacity(0.75) } else { p.text_faint })
                                    .child(format!("· {name}")),
                            )
                        })
                        .children(linked.and_then(|partner| {
                            self.split_link(partner, if active { p.active_text.opacity(0.75) } else { p.text_faint })
                        })),
                )
                // The state stays put while the row's actions show beside it.
                .child(
                    div()
                        .flex_shrink_0()
                        .flex()
                        .items_center()
                        .gap(px(4.))
                        .id("item-status")
                        .when(show_status, |el| {
                            el.child(
                                div()
                                    .relative()
                                    .w(px(16.))
                                    .h(px(20.))
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .child(
                                        div()
                                            .when(hover_close.is_some(), |el| el.group_hover("item-row", |s| s.invisible()))
                                            .child(status_indicator(status.as_ref(), busy, p)),
                                    )
                                    .when_some(hover_close, |el, id| {
                                        let tip = close_tip.clone();
                                        el.child(
                                            div()
                                                .absolute()
                                                .top_0()
                                                .left(px(-2.))
                                                .invisible()
                                                .group_hover("item-row", |s| s.visible())
                                                .child(
                                                    close_button(SharedString::from(format!("row-close-{id}")), 20., p)
                                                        .aria_label(tip.clone())
                                                        .tooltip(move |window, cx| Tooltip::new(tip.clone()).build(window, cx))
                                                        .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                                                            cx.stop_propagation();
                                                            this.end_session(id, EndRequest::Close, window, cx);
                                                        })),
                                                ),
                                        )
                                    }),
                            )
                        })
                        .when(show_count, |el| {
                            el.child(
                                div().w(px(14.)).text_size(px(11.)).text_color(p.text_faint).child(listed_count.to_string()),
                            )
                        })
                        // A group folds its Sessions away with the chevron, which is always there.
                        .when(grouped, |el| {
                            el.child(
                                div()
                                    .id(SharedString::from(format!("fold-{key:?}")))
                                    .role(gpui::Role::Button)
                                    .aria_label(if expanded { "Hide sessions" } else { "Show sessions" })
                                    .aria_expanded(expanded)
                                    .flex_shrink_0()
                                    .size(px(22.))
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .rounded(p.corner(7.))
                                    .hover(|s| s.bg(p.hover))
                                    .tooltip(move |window, cx| {
                                        Tooltip::new(if expanded { "Hide sessions" } else { "Show sessions" }).build(window, cx)
                                    })
                                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                                        cx.stop_propagation();
                                        this.nav_clicked(NavTarget::Item(fold_key.clone()));
                                        this.toggle_item_sessions(&toggle_key, cx);
                                    }))
                                    .child(icon_el(if expanded { "chevron-down" } else { "chevron-right" }, 14., p.text_faint)),
                            )
                        }),
                )
                .child(
                    // Just + and ⋯: disconnecting and remote desktops are in the menu (⋯ or a
                    // right-click), which keeps the row calm. Opaque sidebar colour under the
                    // row's translucent hover colour, so the buttons' backdrop matches the row
                    // and covers the end of a long label. Also shown on the keyboard cursor's
                    // row, and while its menu is open.
                    div()
                        .absolute()
                        .id("item-hover-actions")
                        .right(px(pad_right + trailing))
                        .top(px(5.))
                        .rounded(p.corner(7.))
                        .bg(p.sidebar)
                        .when(!(menu_open || focused), |el| el.invisible().group_hover("item-row", |s| s.visible()))
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .gap(px(2.))
                                .rounded(p.corner(7.))
                                .bg(if active { p.active } else { p.hover })
                                .child(
                                    icon_button(SharedString::from(format!("another-{key:?}")), "plus", p)
                                        .aria_label(new_tip.clone())
                                        .size(px(24.))
                                        .tooltip(move |window, cx| Tooltip::new(new_tip.clone()).build(window, cx))
                                        .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                                            cx.stop_propagation();
                                            this.activate_item(item_for_new.clone(), true, window, cx);
                                        })),
                                )
                                .child(more),
                        ),
                )
                .when(focused, |el| el.child(focus_ring(p.corner(8.), p)))
                .context_menu(item_menu)
                .into_any_element(),
        );
        if expanded {
            rows.push(self.render_session_cards(item, &listed, color, indent, p, cx));
        }
        rows
    }

    /// An item's listed Sessions as cards under its row, hung off a guide line so they never
    /// read as Hosts. The cards always fill the sidebar's width: as many to a line as fit (see
    /// `session_card_rows`), the lines balanced and each card stretched to share its line.
    fn render_session_cards(
        &self,
        item: &SidebarItem,
        listed: &[&SessionEntry],
        color: Hsla,
        indent: f32,
        p: AppPalette,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        // The guide hangs from the middle of the item's icon.
        let guide = indent + 7.;
        let mut sessions = listed.iter();
        let rows = session_card_rows(listed.len(), self.sidebar_width_px, indent).into_iter().map(|count| {
            div().flex().gap(px(5.)).children(sessions.by_ref().take(count).map(|session| {
                div().flex_1().flex_basis(px(0.)).min_w_0().child(self.render_session_card(item, session, color, p, cx))
            }))
        });
        div()
            .relative()
            .ml(px(guide + 7.))
            .mr(px(4.))
            .pl(px(4.))
            .py(px(3.))
            .flex()
            .flex_col()
            .gap(px(5.))
            .child(div().absolute().left(px(-7.)).top(px(-2.)).bottom(px(-2.)).w(px(1.)).bg(p.border))
            .children(rows)
            .into_any_element()
    }

    /// One Session of an item, as a card in the grid under its row: what it is (terminal or
    /// remote desktop) and a short label, its number, its protocol or the name it was given. The
    /// shown Session's card is raised; the rest lie flat. A corner mark appears only when
    /// something needs a look (connecting, dropped, output out of sight), and × on hover
    /// disconnects it, or closes it once it has dropped.
    fn render_session_card(&self, item: &SidebarItem, session: &SessionEntry, color: Hsla, p: AppPalette, cx: &mut Context<Self>) -> AnyElement {
        let id = session.id();
        let active = Some(id) == self.sessions.active();
        let focused = self.nav_focus_on(&NavTarget::Session(id));
        let beside = !active && self.sessions.shown_split().is_some_and(|s| s.contains(id));
        let linked = self.sessions.split().and_then(|s| s.other(id));
        let dragged = self.dragged(item, Some(id), color);
        let live = session.is_live();
        let busy = self.sessions.busy_out_of_sight(session);
        let own_window = session.own_window();
        let (label, title) = session_card_text(session);
        let session_menu = self.session_menu(id, cx);
        // What the Session is: a Host's terminal or desktop by kind, a shell by its own icon.
        let icon = match (session.kind(), item) {
            (SessionKind::Desktop { .. }, _) => "monitor".to_string(),
            (_, SidebarItem::Host { .. }) => "square-terminal".to_string(),
            (_, SidebarItem::Shell { .. }) => item.icon(),
        };
        let icon_color = match item {
            SidebarItem::Shell { .. } => item.icon_color(&icon, color, p),
            SidebarItem::Host { .. } if active => color,
            SidebarItem::Host { .. } => p.text_muted,
        };
        let text_color = if active { p.active_text } else if live { p.text_muted } else { p.text_faint };
        let disconnects = live && self.settings.keep_disconnected_sessions && session.ui.view.desktop().is_none();
        let tip: SharedString = if disconnects { "Disconnect".into() } else { "Close session".into() };
        let close = close_button(SharedString::from(format!("close-{id}")), 20., p)
            .aria_label(tip.clone())
            .tooltip(move |window, cx| Tooltip::new(tip.clone()).build(window, cx))
            .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                cx.stop_propagation();
                this.end_session(id, EndRequest::Close, window, cx);
            }));
        // Only a state that needs a look gets a mark: a live, quiet Session's is its plain card.
        let mark: Option<AnyElement> = match session.status() {
            Status::Connected if busy => Some(spinner(SharedString::from(format!("card-busy-{id}")), 10., p.success)),
            Status::Connected => None,
            Status::Connecting => Some(div().size(px(5.)).rounded_full().bg(p.warning).into_any_element()),
            Status::Disconnected(_) => {
                Some(div().size(px(6.)).rounded_full().border_1().border_color(p.text_faint).into_any_element())
            }
        };
        let tooltip_text = title.clone();
        div()
            .id(SharedString::from(format!("session-{id}")))
            .group("session-card")
            .role(gpui::Role::TreeItem)
            .aria_label(title.clone())
            .aria_level(3)
            .aria_selected(active)
            .relative()
            .min_w_0()
            .h(px(40.))
            .px(px(8.))
            .flex()
            .items_center()
            .justify_center()
            .gap(px(6.))
            .text_size(px(12.5))
            .font_weight(FontWeight::MEDIUM)
            .text_color(text_color)
            .rounded(p.corner(10.))
            .cursor_pointer()
            // Like a pinned tab: a filled tile with a hairline edge, and the shown one ringed in
            // the item's colour.
            .map(|el| {
                if active {
                    el.bg(p.active).border_2().border_color(color)
                } else {
                    el.bg(p.hover)
                        .border_1()
                        .border_color(if beside { color.opacity(0.5) } else { p.border })
                        .hover(|s| s.border_color(p.text_faint).text_color(p.hover_text))
                }
            })
            .tooltip(move |window, cx| Tooltip::new(tooltip_text.clone()).build(window, cx))
            .on_drag(dragged, self.drag_preview(cx))
            .when(self.settings.quick_close.on_middle_click(), |el| {
                el.on_aux_click(cx.listener(move |this, event: &ClickEvent, window, cx| {
                    if event.is_middle_click() {
                        cx.stop_propagation();
                        this.end_session(id, EndRequest::Close, window, cx);
                    }
                }))
            })
            .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                this.activate(id, window, cx);
                let dropped = this.sessions.get(id).is_some_and(|s| !s.is_live());
                if dropped && !own_window {
                    this.reconnect(id, window, cx);
                }
            }))
            .child(icon_el(&icon, 16., icon_color))
            .child(div().min_w_0().truncate().child(label))
            .children(linked.and_then(|partner| self.split_link(partner, if active { p.active_text.opacity(0.75) } else { p.text_faint })))
            .when(own_window, |el| {
                el.child(
                    div()
                        .id("session-own-window")
                        .flex_shrink_0()
                        .tooltip(|window, cx| Tooltip::new("In its own window").build(window, cx))
                        .child(icon_el("app-window", 13., p.text_faint)),
                )
            })
            .when_some(mark, |el, mark| {
                el.child(div().id("session-status").absolute().top(px(5.)).right(px(5.)).group_hover("session-card", |s| s.invisible()).child(mark))
            })
            // Over the card's end on hover, on the card's own colour so it covers a long label.
            .child(
                div()
                    .absolute()
                    .right(px(5.))
                    .top(px(10.))
                    .rounded(p.corner(6.))
                    .bg(if active { p.active } else { p.sidebar })
                    .invisible()
                    .group_hover("session-card", |s| s.visible())
                    
                    .child(div().rounded(p.corner(6.)).bg(if active { p.active } else { p.hover }).child(close)),
            )
            .children(self.row_menu_anchor(NavTarget::Session(id), cx))
            .when(focused, |el| el.child(focus_ring(p.corner(10.), p)))
            .context_menu(session_menu)
            .into_any_element()
    }

    /// The link on a sidebar row whose Session is in the remembered split, shown or not: the
    /// split stays while another Session is shown and comes back with either of its two. Its
    /// tooltip names the Session on the other side.
    fn split_link(&self, partner: usize, color: Hsla) -> Option<AnyElement> {
        let name = self.sessions.display_name(self.sessions.get(partner)?);
        let tip: SharedString = format!("In split view with {name}").into();
        Some(
            div()
                .id("split-link")
                .flex_shrink_0()
                .aria_label(tip.clone())
                .tooltip(move |window, cx| Tooltip::new(tip.clone()).build(window, cx))
                .child(icon_el("link-2", 12., color))
                .into_any_element(),
        )
    }

    /// The menu of a Session's own row.
    fn session_menu(&self, id: usize, cx: &mut Context<Self>) -> impl Fn(PopupMenu, &mut Window, &mut Context<PopupMenu>) -> PopupMenu + Clone + 'static {
        let ws = cx.entity().downgrade();
        let session = self.sessions.get(id);
        let dropped = session.is_some_and(|s| !s.is_live());
        let desktop = session.and_then(desktop_protocol);
        let own_window = session.is_some_and(|s| s.own_window());
        let keep = self.settings.keep_disconnected_sessions && desktop.is_none();
        let files = session.is_some_and(|s| matches!(s.kind(), SessionKind::Ssh { .. }) && matches!(s.status(), Status::Connected));
        let in_split = self.sessions.in_split(id);
        let sidebar_focus = self.nav.focus.clone();
        move |menu: PopupMenu, _: &mut Window, cx: &mut Context<PopupMenu>| {
            let mut menu = menu.action_context(sidebar_focus.clone());
            if files {
                menu = menu.item(menu_item("Browse files", Some("folder-open"), &ws, move |this, window, cx| this.show_files(id, window, cx)));
            }
            if in_split {
                menu = menu.item(menu_item("Close split view", Some("columns-2"), &ws, |this, window, cx| this.close_split(window, cx)));
            }
            menu = menu.item(
                menu_item("Rename session…", None, &ws, move |this, window, cx| this.open_rename_session(id, window, cx))
                    .action(Box::new(nav::NavEdit)),
            );
            menu = menu.separator();
            let mut menu = if dropped {
                menu.item(menu_item("Reconnect", Some("refresh-cw"), &ws, move |this, window, cx| {
                    this.activate(id, window, cx);
                    this.reconnect(id, window, cx);
                }))
                .item(menu_item("Close", None, &ws, move |this, window, cx| this.end_session(id, EndRequest::CloseNow, window, cx)).action(Box::new(nav::NavDelete)))
            } else {
                live_session_items(menu, id, keep, &ws)
            };
            if desktop.is_some() {
                menu = menu.separator().item(if own_window {
                    menu_item("Show its window", None, &ws, move |this, window, cx| this.activate(id, window, cx))
                } else {
                    menu_item("Open in its own window", None, &ws, move |this, window, cx| {
                        this.detach_desktop(id, None, window, cx)
                    })
                });
            }
            notifications::completion_menu(menu, &ws, &[id], cx)
        }
    }

    /// Open a Local Shell in a new, elevated Komando window. Windows asks for consent (UAC)
    /// first; saying no there leaves everything as it was. See `crate::elevate`.
    fn run_as_admin(&mut self, project: Option<String>, name: String, window: &mut Window, cx: &mut Context<Self>) {
        let parent = crate::platform::window_handle(window);
        let (tx, rx) = futures::channel::oneshot::channel();
        let label = name.clone();
        // The call blocks until the prompt is answered; this window keeps drawing meanwhile.
        std::thread::spawn(move || {
            let _ = tx.send(crate::elevate::open_shell_elevated(parent, project.as_deref(), &name));
        });
        cx.spawn(async move |this, cx| {
            let Ok(outcome) = rx.await else { return };
            let _ = this.update(cx, |this, cx| match outcome {
                Ok(crate::elevate::Outcome::Started) => {
                    this.show_toast(format!("{label} opens as administrator in a new window."), cx)
                }
                Ok(crate::elevate::Outcome::Cancelled) => {}
                Err(err) => {
                    log::error!("run as administrator: {err:#}");
                    this.show_toast(format!("Couldn't run {label} as administrator: {err:#}"), cx)
                }
            });
        })
        .detach();
    }

    /// Fold an item's Sessions away under its row, or show them again.
    fn toggle_item_sessions(&mut self, key: &ItemKey, cx: &mut Context<Self>) {
        if !self.collapsed_items.remove(key) {
            self.collapsed_items.insert(key.clone());
        }
        cx.notify();
    }

    /// The active Session's state, name and address with its actions. Lives in the title
    /// bar, above the terminal.
    fn render_session_strip(&self, p: AppPalette, cx: &mut Context<Self>) -> Option<AnyElement> {
        let entry = self.active_entry()?;
        // Where you are typing: state, the session's name, then user@address and port. The
        // name is repeated from the sidebar, which may be scrolled away from the active host.
        let name = self.sessions.display_name(entry);
        let target = match entry.kind() {
            SessionKind::Ssh { host_id } => entry
                .project_id()
                .and_then(|id| self.library.resolve(id, host_id))
                .map(|r| {
                    let user = r.user.clone().map(|u| format!("{u}@")).unwrap_or_default();
                    format!("{user}{}:{}", r.address, r.port)
                })
                .unwrap_or_default(),
            SessionKind::Local { shell } => format!("Local · {}", shell.program),
            SessionKind::Desktop { host_id, protocol } => {
                // The label already names the protocol ("win-build · RDP").
                entry.project_id().map(|p| self.desktop_target(p, host_id, *protocol)).unwrap_or_default()
            }
        };
        let desktop_buttons = self.desktop_strip_buttons(entry, p, cx);
        let forwards_button = self.forwards_strip_button(entry, p, cx);
        let (status_text, status_color) = match entry.status() {
            Status::Connecting => ("Connecting", p.warning),
            Status::Connected => ("Connected", p.success),
            Status::Disconnected(_) => ("Disconnected", p.text_faint),
        };
        let id = entry.id();
        let live = entry.is_live();
        let is_ssh = matches!(entry.kind(), SessionKind::Ssh { .. });
        let indicator = if is_ssh {
            let hops = entry.ui.view.terminal().map(|view| view.read(cx).latency.as_slice()).unwrap_or_default();
            connection_indicator(entry.status(), hops, cx.reduce_motion(), p).into_any_element()
        } else {
            status_dot(status_color).into_any_element()
        };
        let sftp_open = entry.ui.sftp_open;
        let sftp_phase = if sftp_open { entry.ui.sftp_busy.phase() } else { BusyPhase::Idle };
        // SFTP runs over the session's connection: until it is connected the button only looks
        // like one. An open browser can still be closed.
        let sftp_ready = matches!(entry.status(), Status::Connected);
        let sftp_enabled = sftp_ready || sftp_open;
        let sftp_color = if sftp_open {
            p.active_text
        } else if sftp_ready {
            p.text_muted
        } else {
            p.text_faint.opacity(0.55)
        };
        let sftp_done = p.success;
        let sftp_tip = match (entry.status(), sftp_open) {
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
                .child(indicator)
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
                .children(forwards_button)
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
                            .rounded(p.corner(7.))
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
                            // Its hitbox hides the title bar's drag area: see `strip_action`.
                            .occlude()
                            .child(match sftp_phase {
                                BusyPhase::Idle => icon_el("folder-open", 13., sftp_color),
                                BusyPhase::Working => spinner("sftp-toggle-spinner", 13., sftp_color),
                                // The check fades out as the folder fades back in.
                                BusyPhase::Done => busy_done("sftp-toggle-done", move |opacity| {
                                    let layer = |o: f32| div().absolute().top_0().left_0().size(px(13.)).opacity(o);
                                    div()
                                        .relative()
                                        .size(px(13.))
                                        .child(layer(1. - opacity).child(icon_el("folder-open", 13., sftp_color)))
                                        .child(layer(opacity).child(icon_el("check", 13., sftp_done)))
                                        .into_any_element()
                                }),
                            })
                            .child("SFTP"),
                    )
                    // End the connection, keeping the terminal and its output to reconnect later.
                    .when(live, |el| {
                        el.child(
                            strip_action("session-disconnect", "unplug", "Disconnect", false, p)
                                .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| this.end_session(id, EndRequest::Disconnect, window, cx))),
                        )
                    })
                    // A new connection in the same terminal, the earlier output kept above: for a
                    // session that dropped or hangs. A live one is closed first.
                    .child(
                        strip_action("session-reconnect", "refresh-cw", "Reconnect", false, p)
                            .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| this.reconnect(id, window, cx))),
                    )
                })
                .when(self.sessions.shown_split().is_some(), |el| {
                    el.child(
                        strip_action("close-split", "columns-2", "Close split view", false, p)
                            .on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.close_split(window, cx))),
                    )
                })
                .children(desktop_buttons)
                // Room between the session's actions and the window buttons.
                .child(div().w(px(12.)).h(px(16.)).mr(px(6.)).border_r_1().border_color(p.border))
                .into_any_element(),
        )
    }

    /// The window's title bar, drawn by Komando so it matches the app: the sidebar toggle,
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
                el.on_mouse_down(MouseButton::Left, |event, window, _| {
                    // Double-click zooms (or does what the user set in macOS' settings).
                    if event.click_count == 2 { window.titlebar_double_click() } else { window.start_window_move() }
                })
            })
            .child(
                div()
                    .when(self.sidebar_visible || minimal, |el| el.w(px(self.sidebar_width(window) + ISLAND_GAP * 2.)))
                    .flex_shrink_0()
                    .h_full()
                    .pl(px(ISLAND_GAP + 8. + TRAFFIC_LIGHTS_WIDTH))
                    .pr(px(ISLAND_GAP))
                    .flex()
                    .items_center()
                    .gap(px(6.))
                    .when(!minimal, |el| el.children(self.render_main_menu_buttons(p, cx)))
                    .when(crate::elevate::is_elevated(), |el| el.child(admin_badge(p))),
            )
            .child(div().flex_1().min_w_0().h_full().flex().items_center().children(strip))
            // macOS has its traffic lights instead.
            .when(!cfg!(target_os = "macos"), |el| el.child(window_controls(p, window)))
            .into_any_element()
    }

    /// The terminal area: the active Session, or the two of its split side by side. It is
    /// also where sidebar items are dropped to open them beside the Session shown.
    fn render_main(&self, p: AppPalette, window: &Window, cx: &mut Context<Self>) -> AnyElement {
        let content = match (self.active_entry(), self.sessions.shown_split()) {
            (None, _) => self.render_empty(p, cx),
            (Some(_), Some(split)) => {
                let ratio = self.sessions.split_ratio();
                let mut panes: Vec<AnyElement> = [(split.left, ratio), (split.right, 1. - ratio)]
                    .into_iter()
                    .filter_map(|(id, share)| self.render_pane(id, Some(share), p, window, cx))
                    .collect();
                if panes.len() == 2 {
                    panes.insert(1, self.render_split_divider(p, cx));
                }
                div().flex_1().min_w_0().h_full().flex().children(panes).into_any_element()
            }
            (Some(entry), None) => self.render_pane(entry.id(), None, p, window, cx).unwrap_or_else(|| div().into_any_element()),
        };
        div()
            .id("main-area")
            .relative()
            .flex_1()
            .min_w_0()
            .h_full()
            .flex()
            .child(content)
            .children(self.render_drop_overlay(cx))
            .on_drag_move(cx.listener(|this, event: &gpui::DragMoveEvent<DraggedItem>, _, cx| this.drag_over_main(event, cx)))
            .on_drag_move(cx.listener(|this, event: &gpui::DragMoveEvent<split::SplitDivider>, _, cx| this.drag_split_divider(event, cx)))
            .on_drop(cx.listener(|this, dragged: &DraggedItem, window, cx| this.drop_on_main(dragged, window, cx)))
            .into_any_element()
    }

    /// One Session's terminal, with its file browser when open. In a split, `share` is its
    /// part of the width, and a press anywhere in the pane activates it.
    fn render_pane(&self, id: usize, share: Option<f32>, p: AppPalette, window: &Window, cx: &mut Context<Self>) -> Option<AnyElement> {
        let entry = self.sessions.get(id)?;
        let in_split = share.is_some();
        // Both panes grow from nothing, each by its share: the divider sets how the width splits.
        let grow = move |mut el: gpui::Stateful<gpui::Div>| {
            if let Some(share) = share {
                el.style().flex_grow = Some(share);
            }
            el
        };
        if entry.own_window() {
            let placeholder = self.render_in_own_window(id, p, cx);
            return Some(grow(div().id(("pane", id)).flex_1().min_w_0().h_full().flex().child(placeholder)).into_any_element());
        }
        let focused = self.sessions.active() == Some(id);
        let term_bg = cx.global::<Appearance>().term.background_hsla();
        let browser = entry.ui.browser.clone().filter(|_| entry.ui.sftp_open);
        // The browser keeps at least 300px for the terminal when the pane is narrow.
        let window_width = f32::from(window.bounds().size.width);
        let area = window_width - ISLAND_GAP * 2.
            - if self.sidebar_visible { self.sidebar_width(window) + ISLAND_GAP } else { 0. };
        let pane_width = match share {
            Some(share) => (area - ISLAND_GAP) * share,
            None => area,
        };
        let max = (pane_width - 300.).max(280.);
        let width = self.settings.sftp_width.min(max).max(280.);
        if let Some(browser) = &browser {
            browser.update(cx, |b, _| b.width = width);
        }
        let session = island(p)
            .when(matches!(&entry.ui.view, SessionView::Terminal(_)), |el| el.border_color(p.border.blend(p.text.opacity(0.18))))
            .flex_1()
            .min_w_0()
            .h_full()
            .bg(term_bg)
            .overflow_hidden()
            .flex()
            .child(div().flex_1().min_w_0().h_full().map(|el| match &entry.ui.view {
                SessionView::Terminal(view) => el.child(view.clone()),
                SessionView::Desktop(view) => el.child(view.clone()),
            }));
        Some(
            div()
                .id(("pane", id))
                .relative()
                .flex_1()
                .map(grow)
                .min_w_0()
                .h_full()
                .flex()
                .when(in_split && !focused, |el| {
                    el.capture_any_mouse_down(cx.listener(move |this, _: &gpui::MouseDownEvent, window, cx| {
                        if this.sessions.active() != Some(id) && !this.modal_open(cx) {
                            this.activate(id, window, cx);
                        }
                    }))
                })
                .on_drag_move(cx.listener(move |this, event: &gpui::DragMoveEvent<SftpDivider>, _, cx| {
                    if event.drag(cx).0 == id {
                        this.sftp_resizing = true;
                        this.drop_target = None;
                        this.resize_sftp(f32::from(event.event.position.x), event.bounds, cx);
                    }
                }))
                .child(session)
                // The browser is a card of its own beside the session; the gap between them
                // is the handle that resizes it.
                .when_some(browser, |el, browser| {
                    el.child(
                        div()
                            .id("sftp-divider")
                            .group("sftp-divider")
                            .w(px(ISLAND_GAP))
                            .h_full()
                            .flex_shrink_0()
                            .flex()
                            .justify_center()
                            .py(px(14.))
                            .cursor_col_resize()
                            .on_drag(SftpDivider(id), |d, _, _, cx| cx.new(|_| SftpDivider(d.0)))
                            .child(
                                div()
                                    .w(px(2.))
                                    .h_full()
                                    .rounded_full()
                                    .when(self.sftp_resizing, |el| el.bg(p.brand))
                                    .group_hover("sftp-divider", |s| s.bg(p.brand.opacity(0.6))),
                            ),
                    )
                    .child(island(p).w(px(width)).flex_shrink_0().h_full().bg(p.sidebar).overflow_hidden().child(browser))
                })
                .into_any_element(),
        )
    }

    /// In place of a desktop that is in a window of its own.
    fn render_in_own_window(&self, id: usize, p: AppPalette, cx: &mut Context<Self>) -> AnyElement {
        island(p)
            .flex_1()
            .min_w_0()
            .h_full()
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap(px(12.))
            .child(icon_el("app-window", 28., p.text_faint))
            .child(div().text_size(px(13.)).text_color(p.text_muted).child("This remote desktop is in its own window."))
            .child(
                div()
                    .flex()
                    .gap(px(8.))
                    .child(button("own-window-show", "Show its window", ButtonKind::Ghost, p).on_click(cx.listener(
                        move |this, _: &ClickEvent, window, cx| this.activate(id, window, cx),
                    )))
                    .child(button("own-window-back", "Bring it back here", ButtonKind::Primary, p).on_click(cx.listener(
                        move |this, _: &ClickEvent, _, cx| this.close_own_window(id, cx),
                    ))),
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
            .child(wordmark(34., p.text))
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
            icon_text_button(id, icon, label, kind, p).h(px(44.)).px(px(20.)).gap(px(10.)).rounded(p.corner(10.)).text_size(px(14.))
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
            let icon = crate::icons::shell_icon(&shell);
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
        // A Remote Desktop asks inside its own pane.
        let view = self.active_entry()?.ui.view.terminal()?.clone();
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
            Overlay::Picker { query, results, selected, scroll } => {
                let count = results.len();
                let selected = (*selected).min(count.saturating_sub(1));
                let title = "Quick connect".to_string();
                let mut list = div()
                    .id("picker-list")
                    .role(gpui::Role::ListBox)
                    .aria_label("Hosts and local shells")
                    .flex()
                    .flex_col()
                    .gap(px(2.))
                    .max_h(px(380.))
                    .overflow_y_scroll()
                    .track_scroll(scroll)
                    .p(px(6.));
                if results.is_empty() {
                    // A typo, or nothing to find yet: say which, and how to go on.
                    let hint = if self.library.hosts.is_empty() {
                        "No match. Hosts you add show up here too: add one with the + next to a project."
                    } else {
                        "No host or local shell matches. Search by name, address, port, user or project."
                    };
                    list = list.child(div().p(px(16.)).text_size(px(13.)).text_color(p.text_faint).child(hint));
                }
                for (i, PickerResult { item, icon, color, name, detail }) in results.iter().enumerate() {
                    let color = color.unwrap_or_else(|| shell_icon_color(icon, p));
                    let item = item.clone();
                    list = list.child(
                        div()
                            .id(("pick", i))
                            .h(px(44.))
                            .px(px(10.))
                            .flex()
                            .items_center()
                            .gap(px(12.))
                            .rounded(p.corner(8.))
                            .cursor_pointer()
                            .role(gpui::Role::ListBoxOption)
                            .aria_label(format!("{name}, {detail}"))
                            .aria_selected(i == selected)
                            .aria_position_in_set(i + 1)
                            .aria_size_of_set(count)
                            .when(i == selected, |el| el.bg(p.hover))
                            .hover(|s| s.bg(p.active))
                            .on_click(cx.listener(move |this, event: &ClickEvent, window, cx| {
                                this.pick(item.clone(), event.modifiers().control, window, cx)
                            }))
                            .child(badge(icon, color, 26.))
                            .child(
                                div()
                                    .flex()
                                    .flex_col()
                                    .min_w_0()
                                    .child(div().text_size(px(13.)).font_weight(FontWeight::MEDIUM).text_color(p.text).child(name.clone()))
                                    .child(div().text_size(px(12.)).text_color(p.text_faint).truncate().child(detail.clone())),
                            ),
                    );
                }
                div()
                    .key_context(PICKER_CONTEXT)
                    .on_action(cx.listener(|this, _: &PickerUp, _, cx| this.step_picker(false, cx)))
                    .on_action(cx.listener(|this, _: &PickerDown, _, cx| this.step_picker(true, cx)))
                    .w(px(520.))
                    .flex()
                    .flex_col()
                    .child(
                        div()
                            .p(px(14.))
                            .pt(px(12.))
                            .flex()
                            .flex_col()
                            .gap(px(10.))
                            .border_b_1()
                            .border_color(p.border)
                            // As tall as the close button beside it, so the field starts below it.
                            .child(div().h(px(30.)).flex().items_center().child(section_label(&title, p)))
                            .child(Input::new(query)),
                    )
                    .child(list)
                    // How to open another Session from here, which isn't obvious.
                    .when(count > 0, |el| {
                        el.child(
                            div()
                                .px(px(14.))
                                .py(px(8.))
                                .flex()
                                .items_center()
                                .gap(px(14.))
                                .border_t_1()
                                .border_color(p.border)
                                .text_size(px(11.5))
                                .text_color(p.text_faint)
                                .child(div().flex().items_center().gap(px(6.)).child(kbd("Enter", p)).child("Open"))
                                .child(div().flex().items_center().gap(px(6.)).child(kbd("Ctrl Enter", p)).child("New session")),
                        )
                    })
                    .into_any_element()
            }
            Overlay::HostForm(form) => self.render_host_form(form, wide, p, cx),
            Overlay::ProjectForm(form) => self.render_project_form(form, wide, p, cx),
            Overlay::ShellForm => self.render_shell_form(p, cx),
            Overlay::RuleForm(form) => self.render_rule_form(form, p, cx),
            Overlay::SetForm(form) => self.render_set_form(form, p, cx),
            Overlay::History(panel) => self.render_history(panel, p, cx),
            Overlay::Tour(tour) => self.render_tour(tour, p, cx),
            Overlay::RenameSession { name, .. } => self.render_rename_session(name, p, cx),
            Overlay::Confirm { title, body, action } => form_card(
                p,
                title,
                div().text_size(px(13.)).line_height(px(20.)).text_color(p.text_muted).child(body.clone()).into_any_element(),
                vec![
                    button("confirm-cancel", "Cancel", ButtonKind::Ghost, p)
                        .on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.close_overlay(window, cx)))
                        .into_any_element(),
                    button("confirm-ok", match action {
                        ConfirmAction::Disconnect(_) | ConfirmAction::DisconnectAll(_) => "Disconnect",
                        ConfirmAction::Close(_) => "Close",
                        ConfirmAction::InstallUpdate => "Update and restart",
                        ConfirmAction::RestartForUpdate => "Restart now",
                        _ => "Delete",
                    }, if matches!(action, ConfirmAction::InstallUpdate | ConfirmAction::RestartForUpdate) {
                        // Nothing is lost for good: the restart is what was asked for.
                        ButtonKind::Primary
                    } else {
                        ButtonKind::Danger
                    }, p)
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
                // Close on a press outside the card (not a form's). Not on click: selecting text in a field by
                // dragging past the card's edge ends outside it, and that must not close it.
                .on_mouse_down(MouseButton::Left, cx.listener(|this, _: &gpui::MouseDownEvent, window, cx| this.backdrop_pressed(window, cx)))
                .child(
                    div()
                        .id("overlay-card")
                        .h_auto()
                        // As tall as the window allows; a taller form scrolls its own middle.
                        .max_h_full()
                        .flex()
                        .flex_col()
                        .rounded(p.corner(14.))
                        .bg(p.surface)
                        .border_1()
                        .border_color(p.border)
                        .relative()
                        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                        .child(body)
                        .child(
                            close_x("overlay-close", p)
                                .absolute()
                                .top(px(12.))
                                .right(px(12.))
                                .on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.close_overlay(window, cx))),
                        ),
                )
                .into_any_element(),
        )
    }
}

/// The sign that output is streaming in.
fn activity_icon(p: AppPalette) -> AnyElement {
    div()
        .id("output-activity-status")
        .role(gpui::Role::Status)
        .aria_label("Receiving output")
        .flex_shrink_0()
        .child(spinner("output-activity", 12., p.success))
        .into_any_element()
}

/// A Session's state in the sidebar: a dot while live (green) or connecting (amber), a hollow
/// ring once it dropped, and a small turning green ring for output streaming in the background.
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

/// The keyboard cursor's outline on a sidebar row. Drawn over the row rather than as its
/// border, so the row's content doesn't move when the cursor arrives.
fn focus_ring(radius: gpui::Pixels, p: AppPalette) -> gpui::Div {
    div().absolute().top_0().left_0().right_0().bottom_0().rounded(radius).border_2().border_color(p.focus)
}

/// What a Session's card in the sidebar says, and its tooltip. The card: the name it was given,
/// else a desktop's protocol or a terminal's number. The tooltip: the name, else the title its
/// shell set unless that only repeats the item's name or is the program's path (as Windows
/// shells set it), with what the Session is.
/// How many Session cards go on each line of the sidebar, for `count` cards under an item
/// indented by `indent`. A card stays at least `MIN` wide, so a wide sidebar fits more to a line
/// and a narrow one two, never more than six. The lines are balanced (five cards three to a line
/// go 3 + 2, not 3 + 1 + 1) and each line's cards share its whole width.
fn session_card_rows(count: usize, sidebar_width: f32, indent: f32) -> Vec<usize> {
    const MIN: f32 = 60.;
    const GAP: f32 = 5.;
    if count == 0 {
        return Vec::new();
    }
    // The list's own padding, the guide line's inset and the margins.
    let room = sidebar_width - (16. + indent + 14. + 8.);
    let fit = (((room + GAP) / (MIN + GAP)).floor() as usize).clamp(2, 6);
    let lines = count.div_ceil(fit);
    let (each, extra) = (count / lines, count % lines);
    (0..lines).map(|line| each + usize::from(line < extra)).collect()
}

fn session_card_text(session: &SessionEntry) -> (SharedString, SharedString) {
    let kind = match desktop_protocol(session) {
        Some(protocol) => format!("Remote desktop ({})", protocol.label()),
        None => format!("Terminal #{}", session.number()),
    };
    if let Some(name) = session.name() {
        return (name.clone(), format!("{name} · {kind}").into());
    }
    let label: SharedString = match desktop_protocol(session) {
        Some(protocol) => protocol.label().into(),
        None => session.number().to_string().into(),
    };
    let title = session.ui.title.trim();
    let useless = title.is_empty() || title == session.label().as_ref() || title.to_ascii_lowercase().ends_with(".exe");
    let tip = if useless || desktop_protocol(session).is_some() { kind } else { format!("{title} · {kind}") };
    (label, tip.into())
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
                .focus_visible(move |s| crate::ui::widgets::focus_outline(s, p.focus))
                .h(px(26.))
                .px(px(10.))
                .flex()
                .items_center()
                .rounded(p.corner(6.))
                .cursor_pointer()
                .text_size(px(12.5))
                .font_weight(FontWeight::MEDIUM)
                .text_color(if *selected { p.active_text } else { p.muted_text })
                .when(*selected, |el| el.bg(p.brand_soft).font_weight(FontWeight::SEMIBOLD))
                .when(!*selected, |el| el.hover(|s| s.bg(p.hover).text_color(p.hover_text)))
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
        .rounded(p.corner(9.))
        .bg(p.muted)
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
    let off = p.border;
    let on_color = if small { p.text_muted.opacity(0.6) } else { p.brand };
    div()
        .id(id)
        .role(gpui::Role::Switch)
        .aria_label(label)
        .aria_toggled(if on { gpui::Toggled::True } else { gpui::Toggled::False })
        .tab_index(0)
        .focus_visible(move |s| crate::ui::widgets::focus_outline(s, p.focus))
        .w(px(w))
        .h(px(h))
        .flex_shrink_0()
        .p(px(2.))
        .rounded_full()
        .cursor_pointer()
        .bg(if on { on_color } else { off })
        .flex()
        .when(on, |el| el.justify_end())
        // Keep the knob readable on every supplied primary color.
        .child(
            div()
                .size(px(knob))
                .rounded_full()
                .bg(if on && !small { p.on_brand } else { p.text }),
        )
}

/// The bundled Lucide names, legacy artwork, Devicon IDs and shell symbols, sorted.
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
        names.push("shell:cmd".into());
        names.extend(crate::icons::ICONS.iter().map(|icon| icon.id.to_string()));
        names.extend(crate::icons::SYMBOLS.iter().map(|icon| icon.id.to_string()));
        names.sort();
        names.dedup();
        names
    })
}

/// Shown before anything is typed: icons for servers and the things that run on them.
const SUGGESTED_ICONS: &[&str] = &[
    "folder", "server", "database", "globe", "shield", "hard-drive", "cpu", "network", "cloud", "box", "router",
    "tabler:brand-powershell", "tabler:terminal-2", "simpleicons:linux", "simpleicons:ubuntu", "simpleicons:debian", "devicon:docker", "devicon:kubernetes",
    "container", "monitor", "laptop", "terminal", "code", "git-branch", "key-round", "lock", "mail", "cloud-cog",
    "server-cog", "database-backup", "rocket", "flame", "activity", "bug", "building-2", "factory", "house",
    "flask-conical", "wrench", "zap", "radio-tower", "satellite", "warehouse",
];

/// Most results shown at once; a more specific search narrows them down.
const ICON_RESULTS: usize = 60;

/// What an icon is found by, prepared once: the search runs on every frame of the icon picker.
struct IconTerms {
    id: &'static str,
    /// The id without its set's prefix.
    name: &'static str,
    /// A Devicon's name without dashes, so "nodejs" finds "node-js".
    undashed: Option<String>,
    /// Its catalog's search words, lower case and joined by dashes like the query.
    words: Option<String>,
}

fn icon_terms() -> &'static [IconTerms] {
    static TERMS: std::sync::OnceLock<Vec<IconTerms>> = std::sync::OnceLock::new();
    TERMS.get_or_init(|| {
        all_icons()
            .iter()
            .map(|id| {
                let id = id.as_str();
                let name = if id == "shell:cmd" { "command-prompt-cmd-windows-terminal" } else { id.split_once(':').map_or(id, |(_, name)| name) };
                let search = crate::icons::find(id).map(|icon| icon.search).or_else(|| crate::icons::find_symbol(id).map(|icon| icon.search));
                IconTerms {
                    id,
                    name,
                    undashed: id.starts_with("devicon:").then(|| name.replace('-', "")),
                    words: search.map(|search| search.to_lowercase().replace(' ', "-")),
                }
            })
            .collect()
    })
}

/// Icons matching `query`: exact name first, then names starting with it, then names with a
/// word starting with it ("backup" finds "database-backup"), then any other match.
fn search_icons(query: &str) -> (Vec<&'static str>, usize) {
    let q = query.trim().to_lowercase().replace(' ', "-");
    if q.is_empty() {
        let all = all_icons();
        let shown = SUGGESTED_ICONS.iter().filter_map(|s| all.iter().find(|n| n.as_str() == *s)).map(|n| n.as_str()).collect();
        return (shown, 0);
    }
    let q_undashed = q.replace('-', "");
    let rank = |icon: &IconTerms| {
        let name = icon.name;
        if name == q {
            Some(0)
        } else if name.starts_with(&q) {
            Some(1)
        } else if name.split('-').any(|word| word.starts_with(&q)) {
            Some(2)
        } else if name.contains(&q) || icon.undashed.as_ref().is_some_and(|n| n.contains(&q_undashed)) {
            Some(3)
        } else if icon.words.as_ref().is_some_and(|words| words.contains(&q)) || (q == "devicon" && icon.id.starts_with("devicon:")) {
            Some(4)
        } else {
            None
        }
    };
    let mut hits: Vec<(u8, &'static str)> = icon_terms().iter().filter_map(|icon| rank(icon).map(|r| (r, icon.id))).collect();
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
        let tip: SharedString = crate::icons::label(name).into();
        div()
            .id(SharedString::from(format!("icon-{name}")))
            .role(gpui::Role::RadioButton)
            .aria_label(format!("Icon: {}", crate::icons::label(name)))
            .aria_toggled(if is_selected { gpui::Toggled::True } else { gpui::Toggled::False })
            .tab_index(0)
            .focus_visible(move |s| crate::ui::widgets::focus_outline(s, p.focus))
            .size(px(36.))
            .flex()
            .items_center()
            .justify_center()
            .rounded(p.corner(8.))
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
        // Room on the right for the dialog's close button.
        .child(div().pr(px(28.)).text_size(px(15.)).font_weight(FontWeight::SEMIBOLD).text_color(p.text).child(title.to_string()))
        .child(body)
        .child(div().flex().justify_end().gap(px(8.)).children(actions))
        .into_any_element()
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

/// A connected Session's Disconnect and Close entries. Delete does what × does: it closes
/// the Session, or with `keep` (a terminal, "Keep sessions after disconnecting" on) only
/// disconnects it, so then there is no separate Close.
fn live_session_items(menu: PopupMenu, id: usize, keep: bool, ws: &gpui::WeakEntity<Workspace>) -> PopupMenu {
    let disconnect = menu_item("Disconnect", Some("unplug"), ws, move |this, window, cx| this.end_session(id, EndRequest::Disconnect, window, cx));
    if keep {
        return menu.item(disconnect.action(Box::new(nav::NavDelete)));
    }
    menu.item(disconnect)
        .item(menu_item("Close session", None, ws, move |this, window, cx| this.end_session(id, EndRequest::Close, window, cx)).action(Box::new(nav::NavDelete)))
}

/// The rows of `old` that differ from `new`, past their shared start and end, and how many
/// rows of `new` take their place.
fn changed_rows<T: PartialEq>(old: &[T], new: &[T]) -> (std::ops::Range<usize>, usize) {
    let start = old.iter().zip(new).take_while(|(a, b)| a == b).count();
    let end = old[start..].iter().rev().zip(new[start..].iter().rev()).take_while(|(a, b)| a == b).count();
    (start..old.len() - end, new.len() - start - end)
}

/// The ✕ in a dialog's top-right corner. It does what Escape does there.
fn close_x(id: &'static str, p: AppPalette) -> gpui::Stateful<gpui::Div> {
    icon_button(id, "x", p).aria_label("Close")
}

/// Minimise, maximise/restore and close, drawn like Windows 11's own caption buttons: flush
/// rectangles along the window's top edge with no gap between them, the close button in the
/// window's corner (DWM rounds it with the window). The highlight is the system's 32 px tall,
/// so it stays clear of the terminal island's coloured edge below the bar.
///
/// Each button's hit area is the full height of the bar, and the close button's reaches the
/// window's corner, so flicking the pointer to the top-right corner of a maximised window
/// hits it. On Windows the buttons are hit-tested by the system (snap layouts on
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
            .items_start()
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

/// Marks a window running with administrator rights, where every shell is elevated. Amber,
/// like a caution sign, and always in view in the title bar.
fn admin_badge(p: AppPalette) -> impl IntoElement {
    div()
        .id("admin-badge")
        .h(px(22.))
        .px(px(8.))
        .flex()
        .flex_shrink_0()
        .items_center()
        .gap(px(5.))
        .rounded(p.corner(6.))
        .bg(p.warning.opacity(0.14))
        .border_1()
        .border_color(p.warning.opacity(0.3))
        .text_size(px(12.))
        .font_weight(FontWeight::MEDIUM)
        .text_color(p.warning)
        .child(icon_el("shield", 13., p.warning))
        .child("Administrator")
        .tooltip(|window, cx| Tooltip::new("Shells in this window run with administrator rights").build(window, cx))
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
        .focus_visible(move |s| crate::ui::widgets::focus_outline(s, p.focus))
        .size(px(26.))
        .flex()
        .items_center()
        .justify_center()
        .rounded(p.corner(7.))
        .cursor_pointer()
        .hover(move |s| s.bg(hover_bg))
        // The strip sits in the title bar, whose drag area is everywhere behind it. Without
        // this, Windows is told the button is caption: a click starts a window move and is
        // lost, and a double click maximises the window. Blocking the area keeps it a button.
        .occlude()
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

/// A keyboard shortcut ("Ctrl ,") in a small outlined chip, the only one in the app.
fn kbd(keys: &str, p: AppPalette) -> impl IntoElement {
    div()
        .flex_shrink_0()
        .px(px(5.))
        .h(px(18.))
        // Centred on its default line, the text rendered about a pixel above the middle of
        // the chip on Windows, so the capitals touched the top border. Pin the line to the
        // space inside the border and start it a pixel lower.
        .pt(px(2.))
        .line_height(px(14.))
        .flex()
        .items_center()
        .rounded(p.corner(4.))
        .border_1()
        .border_color(p.border)
        .text_size(px(10.5))
        .text_color(p.text_faint)
        .child(keys.to_string())
}

impl Render for Workspace {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        crate::perf::count("workspace render (frame)");
        self.remember_tabs();
        self.close_stale_forwards_menu(window, cx);
        if !self.modal_open(cx) {
            self.nav.after_dialog.set(false);
        }
        let p = cx.global::<Appearance>().app;
        let root = div()
            .id("workspace")
            .key_context("Workspace")
            .track_focus(&self.focus)
            .size_full()
            .relative()
            .flex()
            .flex_col()
            .font_family(p.font_family)
            .bg(p.background)
            .text_color(p.text)
            .capture_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                this.last_activity = SystemTime::now();
                if event.keystroke.key == "escape" && this.unlock_focused(window, cx) {
                    this.skip_unlock(window, cx);
                    cx.stop_propagation();
                } else if event.keystroke.key == "escape" && this.transfers.read(cx).takes_escape(window, cx) {
                    this.transfers.update(cx, |transfers, cx| transfers.close_in(window, cx));
                    cx.stop_propagation();
                } else if event.keystroke.key == "escape" && this.overlay.is_some() {
                    this.close_overlay(window, cx);
                    cx.stop_propagation();
                } else if event.keystroke.key == "escape" && this.prompt_open(cx) {
                    match this.active_entry().map(|e| e.ui.view.clone()) {
                        Some(SessionView::Terminal(view)) => view.update(cx, |view, cx| view.cancel_prompt(window, cx)),
                        Some(SessionView::Desktop(view)) => view.update(cx, |view, cx| view.cancel_prompt(window, cx)),
                        None => {}
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
                if let Some(id) = this.sessions.active().filter(|_| !this.modal_open(cx)) {
                    this.end_session(id, EndRequest::CloseByKey, window, cx);
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
                if let Some(id) = this.sessions.active().filter(|_| !this.modal_open(cx)) {
                    this.toggle_sftp(id, window, cx);
                }
            }))
            .on_action(cx.listener(|this, _: &ToggleSidebar, window, cx| this.toggle_sidebar(window, cx)))
            .on_action(cx.listener(|this, _: &TogglePresentation, window, cx| this.toggle_presentation(window, cx)))
            .on_action(cx.listener(|this, _: &nav::FocusSidebar, window, cx| this.focus_sidebar(window, cx)))
            // A sidebar item let go anywhere that takes no drop: nothing happens, and the
            // drop indicators go.
            .on_drop(cx.listener(|this, _: &DraggedItem, _, cx| {
                this.drop_target = None;
                this.tear_off = None;
                this.native_drag = None;
                cx.notify();
            }))
            .on_drop(cx.listener(|this, _: &DraggedProject, _, cx| {
                this.drop_target = None;
                cx.notify();
            }))
            // A Remote Desktop's row dragged out of the window opens in a window of its own
            // where it is let go.
            .on_drag_move(cx.listener(|this, event: &gpui::DragMoveEvent<DraggedItem>, window, cx| {
                let session = event.drag(cx).session;
                if let Some(preview) = &this.native_drag {
                    preview.follow_cursor();
                }
                this.drag_outside(session, event.event.position, window, cx);
            }))
            .on_mouse_up_out(MouseButton::Left, cx.listener(|this, _: &gpui::MouseUpEvent, window, cx| {
                this.finish_tear_off(window, cx);
            }))
            .on_drop(cx.listener(|this, _: &SftpDivider, _, cx| {
                this.sftp_resizing = false;
                this.save_settings(cx);
                cx.notify();
            }))
            .on_drag_move(cx.listener(|this, event: &gpui::DragMoveEvent<SidebarDivider>, window, cx| {
                if this.sidebar_resizing {
                    let preferred = f32::from(event.event.position.x - event.bounds.left()) - ISLAND_GAP * 1.5;
                    let width = sidebar_width(preferred, f32::from(window.bounds().size.width));
                    if !this.settings.sidebar_width.is_finite() || (this.settings.sidebar_width - width).abs() >= 1. {
                        this.settings.sidebar_width = width;
                        cx.notify();
                    }
                }
            }))
            .on_drop(cx.listener(|this, _: &SidebarDivider, _, cx| this.finish_sidebar_resize(cx)))
            .on_mouse_move(cx.listener(|this, event: &gpui::MouseMoveEvent, window, cx| {
                this.sidebar_peek_pointer(f32::from(event.position.x), window, cx);
            }))
            .on_action(cx.listener(|this, _: &ShowAbout, window, cx| {
                if !this.modal_open(cx) {
                    this.open_settings(SettingsSection::About, window, cx);
                }
            }))
            .on_action(cx.listener(|this, _: &OpenSettings, window, cx| {
                if this.modal_open(cx) {
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
                .rounded(p.corner(10.))
                .bg(p.surface)
                .border_1()
                .border_color(p.border)
                .text_size(px(13.))
                .text_color(p.text)
                .child(msg.clone())
        });
        let frame = window_frame::render(p, window, cx);
        // Full screen, from a desktop's toolbar: the desktop alone, edge to edge. Presentation
        // mode's full screen keeps the title bar, with its way out.
        if window.is_fullscreen()
            && self.presentation.is_none()
            && let Some(SessionView::Desktop(view)) =
                self.active_entry().filter(|e| !e.own_window() && matches!(self.page, Page::Sessions)).map(|e| e.ui.view.clone())
        {
            return root.bg(p.background).child(view).children(self.render_prompt_modal(p, cx)).children(toast).into_any_element();
        }
        let title_bar = self.render_title_bar(false, p, window, cx);
        let page = match self.page {
            Page::Settings(section) => self.render_settings(section, p, cx),
            Page::Sessions => crate::perf::time("sidebar + main", || div()
                .flex_1()
                .min_h_0()
                .w_full()
                .flex()
                // The title bar above is the top margin.
                .px(px(ISLAND_GAP))
                .pb(px(ISLAND_GAP))
                .relative()
                .when(self.sidebar_visible, |el| {
                    el.child(self.render_sidebar(p, window, cx)).child(self.render_sidebar_divider(p, cx))
                })
                .child(self.render_main(p, window, cx))
                .children(self.render_sidebar_peek(p, window, cx))
                .into_any_element()),
        };
        // Transfers: floats above the sidebar's footer, from any page.
        let downloads = self.transfers.read(cx).is_open().then(|| {
            div()
                .absolute()
                .left(px(ISLAND_GAP))
                .bottom(px(ISLAND_GAP + 16. + 2. * 34. + 6.))
                .w(px(480.))
                .max_w(window.bounds().size.width - px(ISLAND_GAP * 2.))
                .child(self.transfers.clone())
        });
        root.bg(p.canvas)
            .child(title_bar)
            .child(page)
            .children(downloads)
            .children(self.render_overlay(f32::from(window.bounds().size.width) >= WIDE_FORM_MIN_WINDOW, p, cx))
            .children(self.render_prompt_modal(p, cx))
            .children(self.main_menu.render())
            .children(self.forwards_menu.render())
            .children(toast)
            .children(frame)
            .into_any_element()
    }
}

#[cfg(test)]
mod session_card_rows_tests {
    use super::{NESTED_INDENT, session_card_rows};

    #[test]
    fn fits_more_cards_as_the_sidebar_widens() {
        assert_eq!(session_card_rows(6, 200., NESTED_INDENT), [2, 2, 2]);
        assert_eq!(session_card_rows(6, 264., NESTED_INDENT), [3, 3]);
        assert_eq!(session_card_rows(6, 400., NESTED_INDENT), [3, 3]);
        assert_eq!(session_card_rows(6, 520., NESTED_INDENT), [6]);
        assert_eq!(session_card_rows(3, f32::NAN, NESTED_INDENT), [2, 1]);
    }

    #[test]
    fn cards_fill_every_line() {
        assert!(session_card_rows(0, 264., NESTED_INDENT).is_empty());
        assert_eq!(session_card_rows(1, 264., NESTED_INDENT), [1]);
        assert_eq!(session_card_rows(2, 264., NESTED_INDENT), [2]);
        assert_eq!(session_card_rows(4, 264., NESTED_INDENT), [2, 2]);
        assert_eq!(session_card_rows(5, 264., NESTED_INDENT), [3, 2]);
        assert_eq!(session_card_rows(7, 400., NESTED_INDENT), [4, 3]);
    }
}

#[cfg(test)]
mod sidebar_width_tests {
    use super::sidebar_width;

    #[test]
    fn resizing_keeps_navigation_and_terminal_usable() {
        assert_eq!(sidebar_width(264., 1280.), 264.);
        assert_eq!(sidebar_width(0., 1280.), 200.);
        assert_eq!(sidebar_width(900., 1280.), 520.);
        assert_eq!(sidebar_width(520., 720.), 336.);
        assert_eq!(sidebar_width(f32::NAN, 1280.), 264.);
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
    fn brand_names_and_aliases_are_searchable_alongside_lucide() {
        for (query, expected) in [("ubuntu", "devicon:ubuntu"), ("powershell", "devicon:powershell"), ("tabler", "tabler:brand-powershell"), ("cmd", "tabler:terminal-2"), ("simple icons", "simpleicons:ubuntu"), ("k8s", "devicon:kubernetes"), ("arm64", "devicon:aarch64"), ("Rocky Linux", "devicon:rockylinux"), ("Linux Mint", "devicon:linuxmint")] {
            let (hits, _) = search_icons(query);
            assert!(hits.contains(&expected), "{query}: {hits:?}");
        }
        let (hits, _) = search_icons("devicon");
        assert!(hits.iter().all(|id| id.starts_with("devicon:")));
    }

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

#[cfg(test)]
mod destination_tests {
    use super::*;

    #[test]
    fn a_destination_names_its_port_unless_it_is_22() {
        let project = Project::new("Lab", "blue", "folder");
        let mut host = Host::new("web", "10.0.0.5");
        host.user = model::Inherited::Own("deploy".into());
        assert_eq!(destination(&model::resolve(&host, &project)), "deploy@10.0.0.5");
        host.port = 2222;
        assert_eq!(destination(&model::resolve(&host, &project)), "deploy@10.0.0.5:2222");
        host.user = model::Inherited::Off;
        assert_eq!(destination(&model::resolve(&host, &project)), "10.0.0.5:2222");
    }
}

#[cfg(test)]
mod changed_rows_tests {
    use super::changed_rows;

    #[test]
    fn only_the_rows_that_differ_are_replaced() {
        assert_eq!(changed_rows(&[1, 2, 3, 4], &[1, 2, 3, 4]), (4..4, 0));
        // A Host's row grows a Session.
        assert_eq!(changed_rows(&[1, 2, 3, 4], &[1, 9, 3, 4]), (1..2, 1));
        // A row added, and one removed.
        assert_eq!(changed_rows(&[1, 2, 4], &[1, 2, 3, 4]), (2..2, 1));
        assert_eq!(changed_rows(&[1, 2, 3, 4], &[1, 3, 4]), (1..2, 0));
        // Repeated rows never count twice.
        assert_eq!(changed_rows(&[1, 1], &[1, 1, 1]), (2..2, 1));
        assert_eq!(changed_rows(&[], &[1, 2]), (0..0, 2));
        assert_eq!(changed_rows(&[1, 2], &[]), (0..2, 0));
    }
}
