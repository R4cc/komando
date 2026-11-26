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
    self, AppThemeSetting, CredentialSource, Host, Library, LocalShell, Project, Settings, TerminalThemeSetting, Unreadable,
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

use controls::*;

mod appearance;
mod command_history;
mod controls;
mod desktop_window;
mod desktops;
mod forms;
mod forwards;
mod highlighting;
mod highlights;
mod import;
mod main_area;
mod main_menu;
pub mod nav;
mod notifications;
mod onboarding;
mod open_tabs;
mod overlays;
mod passwords;
mod session_actions;
mod sessions;
mod settings;
mod sidebar;
mod sidebar_items;
mod split;
mod updates;
mod vault;
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
/// How tall a Session's row under its Host is in the sidebar.
const SESSION_ROW_HEIGHT: f32 = 30.;
/// How long the tree line's branch to a Session's row is.
const SESSION_BRANCH: f32 = 10.;
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
    /// It has an open Session, so picking it switches to that rather than connecting again.
    open: bool,
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
    /// The connection it stands for failed to connect: ringed in red.
    failed: bool,
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
    /// Close all of an item's Sessions, some still printing output.
    CloseAll(ItemKey),
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
    /// Reconnect a dropped SSH Session that signs in with a saved Credential.
    Reconnect { session: usize },
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
    /// The library file when it could be neither read nor kept aside at start: it is the
    /// only copy of the user's Hosts and Projects, so nothing is saved over it this run.
    library_read_only: Option<PathBuf>,
    settings: Settings,
    /// The settings file when it could be neither read nor kept aside at start, so it isn't
    /// saved over this run.
    settings_read_only: Option<PathBuf>,
    #[cfg(windows)]
    tray: Option<crate::platform::Tray>,
    #[cfg(windows)]
    _activation: Option<crate::platform::instance::WindowBinding>,
    #[cfg(windows)]
    notifications: Option<crate::platform::Notifications>,
    completion_watches: notifications::CompletionWatches,
    highlights: Vec<HighlightSet>,
    /// The highlights file couldn't be read nor kept aside, so it isn't saved over this run.
    highlights_file_locked: bool,
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
    /// Settings → Security's "SSH agent" path.
    ssh_agent_input: Option<Entity<InputState>>,
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
        // Data files that couldn't be read, each kept aside so the next save doesn't replace it.
        let (settings, settings_unreadable) = Settings::load(&data_dir);
        crate::perf::startup("settings loaded");
        // A fresh install has no Projects; the detected local shells stand alone.
        let mut library_unreadable = None;
        let library = Library::load(&data_dir).unwrap_or_else(|err| {
            library_unreadable =
                Some(Unreadable::keep(&data_dir.join(Library::FILE), &err, "your hosts", "Hosts and Projects"));
            Library::default()
        });
        crate::perf::startup("library loaded");
        let (highlights, highlights_unreadable) = highlight::load(&data_dir);
        crate::perf::startup("highlights loaded");
        let (favorite_folders, favorites_unreadable) = crate::favorites::Favorites::load(&data_dir);
        let transfers = cx.new(Transfers::new);
        let favorites = cx.new(|_| {
            let in_place = favorites_unreadable.as_ref().and_then(|u| u.in_place.clone());
            FavoriteStore::new(data_dir.clone(), favorite_folders, in_place)
        });
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
            library_read_only: library_unreadable.as_ref().and_then(|u| u.in_place.clone()),
            settings_read_only: settings_unreadable.as_ref().and_then(|u| u.in_place.clone()),
            #[cfg(windows)]
            tray: crate::platform::Tray::new(window, settings.minimize_to_tray, !auxiliary),
            #[cfg(windows)]
            _activation: activation,
            #[cfg(windows)]
            notifications: None,
            completion_watches: Default::default(),
            settings,
            highlights,
            highlights_file_locked: highlights_unreadable.as_ref().is_some_and(|u| u.in_place.is_some()),
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
            ssh_agent_input: None,
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
        this.watch_close_to_tray(window, cx);
        // Desktops in windows of their own close with Komando's.
        let workspace = cx.entity().downgrade();
        window.on_window_should_close(cx, move |_, cx| {
            let _ = workspace.update(cx, |this, cx| this.close_own_windows(cx));
            true
        });
        // One toast holds them all, should several files be unreadable.
        let unreadable: Vec<String> = [library_unreadable, settings_unreadable, highlights_unreadable, favorites_unreadable]
            .into_iter()
            .flatten()
            .map(|u| u.message)
            .collect();
        if !unreadable.is_empty() {
            this.show_toast_for(unreadable.join(" "), Duration::from_secs(20), cx);
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
                    this.cancel_overlay(window, cx);
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
mod session_row_label_tests {
    use super::session_row_label;
    use crate::model::Protocol;

    #[test]
    fn says_the_protocol_and_which_session() {
        assert_eq!(session_row_label(None, true, None, 2, None), "SSH — Session 2");
        assert_eq!(session_row_label(Some(Protocol::Vnc), false, None, 1, Some(5901)), "VNC — Display 1");
        assert_eq!(session_row_label(Some(Protocol::Vnc), false, None, 1, Some(5900)), "VNC — Display 0");
        assert_eq!(session_row_label(Some(Protocol::Vnc), false, None, 1, Some(15900)), "VNC — Desktop");
        assert_eq!(session_row_label(Some(Protocol::Rdp), false, None, 1, Some(3389)), "RDP — Desktop");
    }

    #[test]
    fn a_given_name_replaces_which_session() {
        assert_eq!(session_row_label(Some(Protocol::Vnc), false, Some("Monitoring"), 1, Some(5902)), "VNC — Monitoring");
        assert_eq!(session_row_label(None, true, Some("logs"), 3, None), "SSH — logs");
    }

    #[test]
    fn a_local_shell_has_no_protocol() {
        assert_eq!(session_row_label(None, false, None, 2, None), "Session 2");
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

#[cfg(all(test, target_os = "linux"))]
mod library_read_only_tests {
    use super::*;
    use gpui::WindowOptions;

    #[test]
    fn a_library_that_could_not_be_kept_is_never_saved_over() {
        let dir = std::env::temp_dir().join(format!("komando-library-read-only-{}", uuid::Uuid::new_v4()));
        Settings { onboarded: true, check_for_updates: false, usage_ping: false, ..Settings::default() }.save(&dir).unwrap();
        let path = dir.join(Library::FILE);
        let data_dir = dir.clone();
        let file = path.clone();
        gpui_platform::headless().run(move |cx| {
            crate::init(cx);
            cx.open_window(WindowOptions::default(), move |window, cx| {
                let workspace = cx.new(|cx| Workspace::new(data_dir, Startup::default(), window, cx));
                workspace.update(cx, |this, _| {
                    // As if the file couldn't be read, moved or copied at start.
                    std::fs::write(&file, "the user's only copy").unwrap();
                    this.library_read_only = Some(file.clone());
                    let mut library = this.library.clone();
                    library.hosts.push(Host::new("web01", "10.0.0.1"));
                    let err = this.commit_library(library).unwrap_err();
                    assert!(err.contains(&file.display().to_string()), "{err}");
                    assert!(this.library.hosts.is_empty(), "a refused save changes nothing");
                    assert_eq!(std::fs::read_to_string(&file).unwrap(), "the user's only copy");
                });
                workspace
            }).unwrap();
            // Quit after the headless event loop starts; an earlier quit is reset on entry.
            cx.spawn(async move |cx| {
                cx.background_executor().timer(std::time::Duration::from_millis(10)).await;
                cx.update(|cx| cx.quit());
            }).detach();
        });
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "the user's only copy");
        std::fs::remove_dir_all(dir).unwrap();
    }
}
