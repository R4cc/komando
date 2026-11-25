//! The SFTP file browser: a panel beside an SSH Session's terminal, browsing that Host over
//! the Session's own connection.
//!
//! Each Session has its own browser, which keeps its folder, history and selection while it is
//! closed. Transfers go to the shared [`Transfers`] list (the sidebar's Transfers entry).

use std::collections::HashSet;
use std::ops::Range;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, App, ClickEvent, Context, Entity, EntityId, EventEmitter, ExternalPaths, FocusHandle, Focusable, FontWeight, Hsla,
    KeyDownEvent, MouseButton, MouseDownEvent, ScrollStrategy, SharedString, Subscription, UniformListScrollHandle, WeakEntity,
    Window, div, prelude::*, px, uniform_list,
};
use gpui_component::Sizable as _;
use gpui_component::input::{Input, InputEvent, InputState};
use gpui_component::menu::{ContextMenuExt, PopupMenu, PopupMenuItem};
use gpui_component::tooltip::Tooltip;

use crate::favorites::Favorites;
use crate::session::InputSink;
use crate::sftp::{self, Access, Allowed, Entry, ErrorKind, Identity, Kind, Listing, OpError, Remote, format_size};
use crate::theme::{AppPalette, Appearance};
use crate::ui::popup::{self, Popup};
use crate::ui::transfers::{self, Job, Transfers};
use crate::ui::widgets::{Busy, BusyPhase, ButtonKind, busy_done, after_delay, button, focus_outline, icon_el, plural, spinner, status_dot};

const ROW_HEIGHT: f32 = 30.;

/// Favorites, shared by every browser and saved on each change.
pub struct FavoriteStore {
    pub data: Favorites,
    dir: PathBuf,
    /// The favorites file when it could be neither read nor kept aside at start: it is the
    /// only copy of the user's favorites, so nothing is saved over it this run.
    in_place: Option<PathBuf>,
}

impl FavoriteStore {
    /// `in_place` is [`crate::model::Unreadable::in_place`] from loading `data`.
    pub fn new(dir: PathBuf, data: Favorites, in_place: Option<PathBuf>) -> Self {
        Self { data, dir, in_place }
    }

    pub fn change(&mut self, f: impl FnOnce(&mut Favorites), cx: &mut Context<Self>) {
        f(&mut self.data);
        if let Some(path) = &self.in_place {
            log::warn!("not saving favorites over {}, which couldn't be read", path.display());
        } else if let Err(err) = self.data.save(&self.dir) {
            log::error!("saving favorites: {err:#}");
        }
        cx.notify();
    }
}

pub enum BrowserEvent {
    /// The browser's own close button.
    Close,
    /// "Show hidden files" changed; remembered for new browsers.
    ShowHidden(bool),
    /// Try starting SFTP again (it failed).
    Retry,
    /// "Jump to here": change the terminal's directory to this folder.
    JumpTo(String),
}

/// Context-menu entries the user can switch on in Settings (all off by default).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct MenuExtras {
    pub rename: bool,
    pub copy_to: bool,
    pub move_to: bool,
}

enum Conn {
    /// The Session isn't connected yet (or any more).
    Waiting(String),
    Connecting,
    Ready(Arc<Remote>),
    Failed(String),
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum SortKey {
    Name,
    Size,
    Modified,
    Perms,
}

enum History {
    Push,
    /// Go to this position in the history (back and forward).
    To(usize),
    /// Reload in place.
    Stay,
}

struct Notice {
    text: SharedString,
    error: bool,
    seq: u64,
}

enum EditKind {
    Rename { path: String },
    NewFolder,
}

struct Edit {
    kind: EditKind,
    input: Entity<InputState>,
    _subscription: Subscription,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum CopyOp {
    Copy,
    Move,
}

enum Dialog {
    CopyMove { op: CopyOp, paths: Vec<String>, input: Entity<InputState>, _subscription: Subscription },
    Delete { paths: Vec<String> },
    /// Uploads whose names already exist in the folder.
    Conflict { uploads: Vec<PathBuf>, dir: String, existing: Vec<String> },
}

/// Rows being dragged inside the browser, to move them into a folder.
#[derive(Clone)]
pub struct DraggedFiles {
    /// The browser they were dragged from.
    source: EntityId,
    paths: Vec<String>,
    label: SharedString,
}

impl DraggedFiles {
    /// The paths to move, if they were dragged inside `browser`. In a split view another
    /// Session's browser is on screen too: its paths are on another Host (or another
    /// connection), so dropped here they would move whatever has the same path on this one.
    fn paths_for(&self, browser: EntityId) -> Option<&[String]> {
        (self.source == browser).then_some(self.paths.as_slice())
    }
}

struct DragGhost {
    label: SharedString,
}

impl Render for DragGhost {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = cx.global::<Appearance>().app;
        div()
            .px(px(10.))
            .py(px(5.))
            .rounded(p.corner(7.))
            .bg(p.surface.opacity(1.))
            .border_1()
            .border_color(p.border)
            .text_size(px(12.))
            .text_color(p.text)
            .child(self.label.clone())
    }
}

pub struct FileBrowser {
    host_id: String,
    host_label: SharedString,
    /// "user@address:port".
    target: SharedString,
    favorites: Entity<FavoriteStore>,
    transfers: Entity<Transfers>,
    conn: Conn,
    cwd: Option<String>,
    history: Vec<String>,
    history_ix: usize,
    entries: Vec<Entry>,
    /// What the user may do in the current folder (create, rename, delete need write).
    dir_access: Access,
    /// `entries` indices in display order (sorted, hidden files filtered).
    rows: Vec<usize>,
    loading: bool,
    load_seq: u64,
    /// The folder couldn't be listed; shown in place of the list.
    load_error: Option<OpError>,
    notice: Option<Notice>,
    notice_seq: u64,
    /// A running operation, shown in the footer.
    busy: Option<SharedString>,
    /// Starting SFTP, loading a folder or running an operation: the footer's spinner, then
    /// its check. The workspace copies it for the title bar's SFTP button.
    indicator: Busy,
    /// What the footer says while `indicator` works.
    indicator_label: SharedString,
    selected: HashSet<String>,
    anchor: Option<String>,
    /// The row Shift+arrows last moved to, when it isn't `anchor`: the selection runs from the
    /// anchor to it, so moving back shrinks it.
    cursor: Option<String>,
    sort: (SortKey, bool),
    show_hidden: bool,
    path_input: Entity<InputState>,
    editing_path: bool,
    /// Something changed on the server while a listing was loading, which may predate it.
    refresh_after_load: bool,
    edit: Option<Edit>,
    favorites_open: bool,
    /// The favorites list's own focus: it takes the keyboard when the list opens, so Tab
    /// goes on into its rows rather than past the rest of the browser first.
    favorites_focus: FocusHandle,
    /// When a click outside last closed the favorites list, so the click that closed it
    /// doesn't open it again when it lands on the star.
    favorites_closed_at: Option<std::time::Instant>,
    favorite_rename: Option<(String, Entity<InputState>, Subscription)>,
    dialog: Option<Dialog>,
    popup: Popup,
    scroll: UniformListScrollHandle,
    focus: FocusHandle,
    /// The panel's width, set by the workspace; narrow panels drop columns.
    pub width: f32,
    /// The path to select once the next listing arrives.
    select_after_load: Option<String>,
    /// Optional context-menu entries, set by the workspace from Settings.
    pub menu_extras: MenuExtras,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<BrowserEvent> for FileBrowser {}

impl Focusable for FileBrowser {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl FileBrowser {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        host_id: String,
        host_label: SharedString,
        target: SharedString,
        favorites: Entity<FavoriteStore>,
        transfers: Entity<Transfers>,
        show_hidden: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let path_input = cx.new(|cx| InputState::new(window, cx).placeholder("Go to folder…"));
        let subscriptions = vec![
            cx.observe(&favorites, |_, _, cx| cx.notify()),
            cx.subscribe_in(&path_input, window, |this, input, event: &InputEvent, window, cx| match event {
                InputEvent::PressEnter { .. } => {
                    let text = input.read(cx).value().trim().to_string();
                    this.editing_path = false;
                    if !text.is_empty() {
                        let path = this.expand(&text);
                        this.navigate(path, cx);
                    }
                    window.focus(&this.focus, cx);
                    cx.notify();
                }
                InputEvent::Blur => {
                    this.editing_path = false;
                    cx.notify();
                }
                _ => {}
            }),
        ];
        Self {
            host_id,
            host_label,
            target,
            favorites,
            transfers,
            conn: Conn::Waiting("Waiting for the session to connect…".into()),
            cwd: None,
            history: vec![],
            history_ix: 0,
            entries: vec![],
            dir_access: Access::UNKNOWN,
            rows: vec![],
            loading: false,
            load_seq: 0,
            load_error: None,
            notice: None,
            notice_seq: 0,
            busy: None,
            indicator: Busy::default(),
            indicator_label: SharedString::default(),
            selected: HashSet::new(),
            anchor: None,
            cursor: None,
            sort: (SortKey::Name, true),
            show_hidden,
            path_input,
            editing_path: false,
            refresh_after_load: false,
            edit: None,
            favorites_open: false,
            favorites_focus: cx.focus_handle(),
            favorites_closed_at: None,
            favorite_rename: None,
            dialog: None,
            popup: Popup::default(),
            scroll: UniformListScrollHandle::new(),
            focus: cx.focus_handle(),
            width: 520.,
            select_after_load: None,
            menu_extras: MenuExtras::default(),
            _subscriptions: subscriptions,
        }
    }

    // ---- connection ---------------------------------------------------------------

    pub fn remote(&self) -> Option<Arc<Remote>> {
        match &self.conn {
            Conn::Ready(remote) => Some(remote.clone()),
            _ => None,
        }
    }

    /// Connected or on its way: nothing to do when the Session (re)connects.
    pub fn is_connected(&self) -> bool {
        matches!(self.conn, Conn::Ready(_) | Conn::Connecting)
    }

    /// Starting SFTP, or loading a folder (the first one included).
    fn is_busy(&self) -> bool {
        match self.conn {
            Conn::Connecting => true,
            Conn::Ready(_) => self.loading || (self.cwd.is_none() && self.load_error.is_none()),
            Conn::Waiting(_) | Conn::Failed(_) => false,
        }
    }

    /// The work under way and its indicator (see `indicator`).
    pub fn indicator(&self) -> Busy {
        self.indicator
    }

    /// Keep `indicator` in step after work starts or ends. `ok`: the work that just ended
    /// succeeded, so a check follows once nothing else is under way.
    fn sync_indicator(&mut self, ok: bool, cx: &mut Context<Self>) {
        if self.busy.is_some() || self.is_busy() {
            self.indicator_label = self.busy.clone().unwrap_or_else(|| {
                if matches!(self.conn, Conn::Connecting) { "Starting SFTP…" } else { "Loading…" }.into()
            });
            self.indicator.start();
            if !ok {
                self.indicator.fail();
            }
        } else {
            self.indicator.finish(ok, cx);
        }
    }

    /// Open SFTP on the Session's connection.
    pub fn connect(&mut self, sink: Option<InputSink>, cx: &mut Context<Self>) {
        let Some(answer) = sink.and_then(|s| s.open_sftp()) else {
            self.conn = Conn::Waiting("The session isn't connected. Reconnect it to browse files.".into());
            self.indicator.clear();
            cx.notify();
            return;
        };
        self.conn = Conn::Connecting;
        self.sync_indicator(true, cx);
        cx.spawn(async move |this, cx| {
            let result = answer.await;
            let _ = this.update(cx, |this, cx| {
                let ok = matches!(result, Ok(Ok(_)));
                match result {
                    Ok(Ok(remote)) => {
                        let start = this.cwd.clone().unwrap_or_else(|| remote.home.clone());
                        this.conn = Conn::Ready(Arc::new(remote));
                        this.load(start, History::Stay, cx);
                    }
                    Ok(Err(err)) => this.conn = Conn::Failed(format!("{err:#}")),
                    Err(_) => this.conn = Conn::Waiting("The session ended. Reconnect it to browse files.".into()),
                }
                this.sync_indicator(ok, cx);
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    /// The Session's connection ended.
    pub fn disconnected(&mut self, cx: &mut Context<Self>) {
        self.conn = Conn::Waiting("The session is disconnected. Reconnect it to browse files.".into());
        self.loading = false;
        self.busy = None;
        self.indicator.clear();
        cx.notify();
    }

    // ---- navigation ---------------------------------------------------------------

    fn identity(&self) -> Identity {
        self.remote().map(|r| r.identity.clone()).unwrap_or_default()
    }

    /// `~` and relative paths are taken from the home folder and the current folder. The
    /// result has no `.` or trailing slash, so it compares equal to the folder it names.
    /// Leave `..` for the server: a preceding component might be a symlink.
    fn expand(&self, text: &str) -> String {
        let home = self.remote().map(|r| r.home.clone()).unwrap_or_else(|| "/".into());
        let path = if text == "~" {
            home
        } else if let Some(rest) = text.strip_prefix("~/") {
            sftp::join(&home, rest)
        } else if text.starts_with('/') {
            text.to_string()
        } else {
            sftp::join(self.cwd.as_deref().unwrap_or(&home), text)
        };
        clean_path(&path)
    }

    pub fn navigate(&mut self, path: String, cx: &mut Context<Self>) {
        self.load(path, History::Push, cx);
    }

    fn refresh(&mut self, cx: &mut Context<Self>) {
        if let Some(cwd) = self.cwd.clone() {
            self.load(cwd, History::Stay, cx);
        } else if let Some(remote) = self.remote() {
            self.load(remote.home.clone(), History::Stay, cx);
        }
    }

    fn go_back(&mut self, cx: &mut Context<Self>) {
        if self.history_ix > 0 {
            let ix = self.history_ix - 1;
            self.load(self.history[ix].clone(), History::To(ix), cx);
        }
    }

    fn go_forward(&mut self, cx: &mut Context<Self>) {
        if self.history_ix + 1 < self.history.len() {
            let ix = self.history_ix + 1;
            self.load(self.history[ix].clone(), History::To(ix), cx);
        }
    }

    fn go_up(&mut self, cx: &mut Context<Self>) {
        if let Some(cwd) = self.cwd.clone()
            && let Some(parent) = sftp::parent(&cwd)
        {
            self.select_after_load = Some(cwd);
            self.navigate(parent, cx);
        }
    }

    /// Something changed in `dir` on the server (an upload finished): show it.
    pub fn changed_on_server(&mut self, dir: &str, cx: &mut Context<Self>) {
        if self.loading {
            // The listing on its way may have been read before the change: list again after.
            self.refresh_after_load = true;
        } else if self.cwd.as_deref() == Some(dir) {
            self.refresh(cx);
        }
    }

    /// Go to the folder holding `path` and select it.
    pub fn reveal(&mut self, path: &str, cx: &mut Context<Self>) {
        if let Some(parent) = sftp::parent(path) {
            self.select_after_load = Some(path.to_string());
            self.navigate(parent, cx);
        }
    }

    fn load(&mut self, path: String, history: History, cx: &mut Context<Self>) {
        let Some(remote) = self.remote() else { return };
        self.load_seq += 1;
        let seq = self.load_seq;
        self.loading = true;
        self.sync_indicator(true, cx);
        let task = sftp::spawn(async move { remote.list(&path).await });
        cx.spawn(async move |this, cx| {
            let result = task.await.and_then(|r| r);
            let _ = this.update(cx, |this, cx| {
                if seq == this.load_seq {
                    this.loaded(result, history, cx);
                }
            });
        })
        .detach();
        cx.notify();
    }

    fn loaded(&mut self, result: Result<Listing, OpError>, history: History, cx: &mut Context<Self>) {
        self.loading = false;
        let ok = result.is_ok();
        match result {
            Ok(listing) => {
                let same_folder = self.cwd.as_deref() == Some(listing.path.as_str());
                match history {
                    History::Push if !same_folder => {
                        self.history.truncate(self.history_ix + 1);
                        self.history.push(listing.path.clone());
                        self.history_ix = self.history.len() - 1;
                    }
                    History::To(ix) => self.history_ix = ix,
                    _ if self.history.is_empty() => {
                        self.history.push(listing.path.clone());
                        self.history_ix = 0;
                    }
                    _ => {}
                }
                if !same_folder {
                    self.selected.clear();
                    self.anchor = None;
                    self.cursor = None;
                    self.edit = None;
                    // The path bar shows where the browser is now, not a path typed earlier.
                    self.editing_path = false;
                    self.scroll.scroll_to_item(0, ScrollStrategy::Top);
                }
                self.cwd = Some(listing.path);
                self.entries = listing.entries;
                self.dir_access = listing.access;
                self.load_error = None;
                // Keep only selections that still exist.
                let present: HashSet<&str> = self.entries.iter().map(|e| e.path.as_str()).collect();
                self.selected.retain(|p| present.contains(p.as_str()));
                if let Some(path) = self.select_after_load.take()
                    && present.contains(path.as_str())
                {
                    self.selected = HashSet::from([path.clone()]);
                    self.anchor = Some(path);
                    self.cursor = None;
                }
                self.update_rows();
                self.scroll_to_selection();
                if std::mem::take(&mut self.refresh_after_load) {
                    self.refresh(cx);
                }
            }
            Err(err) => {
                self.refresh_after_load = false;
                self.select_after_load = None;
                match (&self.cwd, history) {
                    // Reloading the folder on screen failed: it's gone or no longer readable.
                    (Some(_), History::Stay) | (None, _) => {
                        self.load_error = Some(err);
                        self.entries.clear();
                        self.update_rows();
                    }
                    // Going somewhere else failed: stay here and say why.
                    _ => {
                        let message = match err.kind {
                            ErrorKind::PermissionDenied => format!("Can't open that folder. {}", err.message),
                            ErrorKind::NotFound => format!("That folder doesn't exist on {}. {}", self.host_label, err.message),
                            _ => format!("Can't open that folder: {}", err.message),
                        };
                        self.notify_error(message, cx);
                    }
                }
            }
        }
        self.sync_indicator(ok, cx);
        cx.notify();
    }

    // ---- rows and selection ---------------------------------------------------------

    fn update_rows(&mut self) {
        let (key, asc) = self.sort;
        let mut rows: Vec<usize> =
            (0..self.entries.len()).filter(|&i| self.show_hidden || !self.entries[i].is_hidden()).collect();
        let entries = &self.entries;
        rows.sort_by(|&a, &b| {
            let (a, b) = (&entries[a], &entries[b]);
            // Folders first, whichever way the list is sorted.
            let dirs = b.is_dir().cmp(&a.is_dir());
            let by_name = || natural_cmp(&a.name, &b.name);
            let order = match key {
                SortKey::Name => by_name(),
                SortKey::Size => a.size.cmp(&b.size).then_with(by_name),
                SortKey::Modified => a.mtime.cmp(&b.mtime).then_with(by_name),
                SortKey::Perms => a.mode_string().cmp(&b.mode_string()).then_with(by_name),
            };
            dirs.then(if asc { order } else { order.reverse() })
        });
        self.rows = rows;
    }

    fn set_sort(&mut self, key: SortKey, cx: &mut Context<Self>) {
        self.sort = if self.sort.0 == key { (key, !self.sort.1) } else { (key, key == SortKey::Name) };
        self.update_rows();
        cx.notify();
    }

    pub fn set_show_hidden(&mut self, show: bool, cx: &mut Context<Self>) {
        self.show_hidden = show;
        if !show {
            let visible: HashSet<&str> =
                self.entries.iter().filter(|e| !e.is_hidden()).map(|e| e.path.as_str()).collect();
            self.selected.retain(|p| visible.contains(p.as_str()));
        }
        self.update_rows();
        cx.emit(BrowserEvent::ShowHidden(show));
        cx.notify();
    }

    fn has_parent(&self) -> bool {
        self.cwd.as_deref().is_some_and(|c| c != "/")
    }

    fn entry(&self, path: &str) -> Option<&Entry> {
        self.entries.iter().find(|e| e.path == path)
    }

    /// Selected entries, in display order.
    fn selection(&self) -> Vec<Entry> {
        self.rows.iter().map(|&i| &self.entries[i]).filter(|e| self.selected.contains(&e.path)).cloned().collect()
    }

    fn visible_index(&self, path: &str) -> Option<usize> {
        self.rows.iter().position(|&i| self.entries[i].path == path)
    }

    fn scroll_to_selection(&self) {
        if let Some(path) = self.cursor.as_ref().or(self.anchor.as_ref())
            && let Some(ix) = self.visible_index(path)
        {
            self.scroll.scroll_to_item(ix + self.has_parent() as usize, ScrollStrategy::Center);
        }
    }

    fn click_row(&mut self, path: String, event: &ClickEvent, window: &mut Window, cx: &mut Context<Self>) {
        window.focus(&self.focus, cx);
        let mods = event.modifiers();
        if event.click_count() >= 2 && !mods.control && !mods.shift {
            if let Some(entry) = self.entry(&path).cloned() {
                self.open_entry(&entry, cx);
            }
            return;
        }
        if mods.shift
            && let Some(anchor) = self.anchor.clone()
            && let (Some(a), Some(b)) = (self.visible_index(&anchor), self.visible_index(&path))
        {
            let (lo, hi) = if a <= b { (a, b) } else { (b, a) };
            if !mods.control {
                self.selected.clear();
            }
            for &i in &self.rows[lo..=hi] {
                self.selected.insert(self.entries[i].path.clone());
            }
            self.cursor = Some(path);
        } else if mods.control || mods.platform {
            if !self.selected.remove(&path) {
                self.selected.insert(path.clone());
            }
            self.anchor = Some(path);
            self.cursor = None;
        } else {
            self.selected = HashSet::from([path.clone()]);
            self.anchor = Some(path);
            self.cursor = None;
        }
        cx.notify();
    }

    /// Right-click: a row outside the selection becomes the selection.
    fn right_click_row(&mut self, path: String, window: &mut Window, cx: &mut Context<Self>) {
        window.focus(&self.focus, cx);
        if !self.selected.contains(&path) {
            self.selected = HashSet::from([path.clone()]);
            self.anchor = Some(path);
            self.cursor = None;
            cx.notify();
        }
    }

    fn move_cursor(&mut self, delta: isize, extend: bool, cx: &mut Context<Self>) {
        if self.rows.is_empty() {
            return;
        }
        let current = self.cursor.as_ref().or(self.anchor.as_ref()).and_then(|p| self.visible_index(p));
        let next = match current {
            Some(i) => (i as isize + delta).clamp(0, self.rows.len() as isize - 1) as usize,
            None if delta < 0 => self.rows.len() - 1,
            None => 0,
        };
        let path = self.entries[self.rows[next]].path.clone();
        // Shift: the selection runs from the anchor to the new row, as with Shift+click.
        if extend && let Some(anchor) = self.anchor.as_deref().and_then(|p| self.visible_index(p)) {
            let (lo, hi) = if anchor <= next { (anchor, next) } else { (next, anchor) };
            self.selected = self.rows[lo..=hi].iter().map(|&i| self.entries[i].path.clone()).collect();
            self.cursor = Some(path);
        } else {
            self.selected = HashSet::from([path.clone()]);
            self.anchor = Some(path);
            self.cursor = None;
        }
        self.scroll_to_selection();
        cx.notify();
    }

    fn select_all(&mut self, cx: &mut Context<Self>) {
        self.selected = self.rows.iter().map(|&i| self.entries[i].path.clone()).collect();
        cx.notify();
    }

    // ---- notices ------------------------------------------------------------------

    fn notify_error(&mut self, text: impl Into<SharedString>, cx: &mut Context<Self>) {
        // Work that reported an error ends without a check, even if it partly succeeded.
        self.indicator.fail();
        self.show_notice(text.into(), true, cx);
    }

    fn notify_info(&mut self, text: impl Into<SharedString>, cx: &mut Context<Self>) {
        self.show_notice(text.into(), false, cx);
    }

    fn show_notice(&mut self, text: SharedString, error: bool, cx: &mut Context<Self>) {
        self.notice_seq += 1;
        let seq = self.notice_seq;
        self.notice = Some(Notice { text, error, seq });
        let secs = if error { 10 } else { 4 };
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(Duration::from_secs(secs)).await;
            let _ = this.update(cx, |this, cx| {
                if this.notice.as_ref().is_some_and(|n| n.seq == seq) {
                    this.notice = None;
                    cx.notify();
                }
            });
        })
        .detach();
        cx.notify();
    }

    // ---- operations ---------------------------------------------------------------

    /// Run an operation on the server, show it in the footer, then reload the folder.
    fn run_op<T: Send + 'static>(
        &mut self,
        busy: impl Into<SharedString>,
        op: impl FnOnce(Arc<Remote>) -> futures::future::BoxFuture<'static, Result<T, OpError>>,
        cx: &mut Context<Self>,
        done: impl FnOnce(&mut Self, Result<T, OpError>, &mut Context<Self>) + 'static,
    ) {
        let Some(remote) = self.remote() else {
            self.notify_error("Not connected.", cx);
            return;
        };
        self.busy = Some(busy.into());
        self.sync_indicator(true, cx);
        let task = sftp::spawn(op(remote));
        cx.spawn(async move |this, cx| {
            let result = task.await.and_then(|r| r);
            let _ = this.update(cx, |this, cx| {
                this.busy = None;
                let ok = result.is_ok();
                done(this, result, cx);
                this.refresh(cx);
                this.sync_indicator(ok, cx);
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn open_entry(&mut self, entry: &Entry, cx: &mut Context<Self>) {
        match entry.kind {
            Kind::Dir => self.navigate(entry.path.clone(), cx),
            _ => self.download(vec![entry.clone()], true, cx),
        }
    }

    fn open_selection(&mut self, cx: &mut Context<Self>) {
        let selection = self.selection();
        match selection.as_slice() {
            [entry] => {
                let entry = entry.clone();
                self.open_entry(&entry, cx);
            }
            [] => {}
            many => {
                let files: Vec<Entry> = many.iter().filter(|e| !e.is_dir()).cloned().collect();
                if files.is_empty() {
                    self.notify_info("Enter downloads the selected files. Use Download for folders.", cx);
                    return;
                }
                self.download(files, false, cx);
            }
        }
    }

    /// Download files and folders to the Downloads folder. `open_after` opens a single file
    /// with its default program once it has arrived.
    fn download(&mut self, entries: Vec<Entry>, open_after: bool, cx: &mut Context<Self>) {
        let Some(remote) = self.remote() else { return };
        let locked: Vec<&str> = entries.iter().filter(|e| e.locked()).map(|e| e.name.as_str()).collect();
        if !locked.is_empty() {
            self.notify_error(
                format!("You don't have permission to read {}. Nothing was downloaded for it.", list_names(&locked)),
                cx,
            );
        }
        let source = cx.entity().downgrade();
        let host = self.host_label.clone();
        let dir = transfers::download_dir();
        // Sockets, pipes, devices and broken links have nothing to download.
        let special: Vec<&str> = entries.iter().filter(|e| !e.locked() && e.kind == Kind::Other).map(|e| e.name.as_str()).collect();
        if !special.is_empty() && locked.is_empty() {
            self.notify_error(format!("{} isn't a file or folder, so it can't be downloaded.", list_names(&special)), cx);
        }
        let wanted: Vec<Entry> = entries.into_iter().filter(|e| !e.locked() && e.kind != Kind::Other).collect();
        if wanted.is_empty() {
            return;
        }
        self.transfers.update(cx, |t, cx| {
            for entry in wanted {
                let job = Job::Download { remote_path: entry.path.clone(), dir: dir.clone(), open_after: open_after && !entry.is_dir() };
                t.start(job, entry.name.clone(), entry.is_dir(), host.clone(), source.clone(), remote.clone(), cx);
            }
        });
    }

    fn pick_uploads(&mut self, folders: bool, window: &mut Window, cx: &mut Context<Self>) {
        let Some(dir) = self.cwd.clone() else { return };
        let picked = cx.prompt_for_paths(gpui::PathPromptOptions {
            files: !folders,
            directories: folders,
            multiple: true,
            prompt: Some("Upload".into()),
        });
        cx.spawn_in(window, async move |this, cx| {
            let Ok(Ok(Some(paths))) = picked.await else { return };
            let _ = this.update(cx, |this, cx| this.upload(paths, dir, cx));
        })
        .detach();
    }

    /// Upload into `dir`, asking first when names are taken there.
    fn upload(&mut self, paths: Vec<PathBuf>, dir: String, cx: &mut Context<Self>) {
        if paths.is_empty() {
            return;
        }
        if self.cwd.as_deref() == Some(dir.as_str()) && self.dir_access.write == Allowed::No {
            self.notify_error(format!("You don't have permission to write to {dir}. Nothing was uploaded."), cx);
            return;
        }
        let Some(remote) = self.remote() else { return };
        let names: Vec<String> = paths.iter().map(|p| upload_name(p)).collect();
        let check_dir = dir.clone();
        let task = sftp::spawn(async move {
            use futures::{StreamExt as _, TryStreamExt as _};
            // Asked together: one at a time, a thousand dropped files would wait a thousand
            // round trips before the first upload starts.
            let checks: Vec<_> = names
                .into_iter()
                .map(|name| {
                    let (remote, path) = (remote.clone(), sftp::join(&check_dir, &name));
                    async move { remote.exists(&path).await.map(|exists| exists.then_some(name)) }
                })
                .collect();
            let taken: Vec<Option<String>> = futures::stream::iter(checks).buffered(32).try_collect().await?;
            Ok::<_, OpError>(taken.into_iter().flatten().collect::<Vec<_>>())
        });
        cx.spawn(async move |this, cx| {
            let result = task.await.and_then(|r| r);
            let _ = this.update(cx, |this, cx| match result {
                Ok(existing) if existing.is_empty() => this.start_uploads(paths, dir, false, None, cx),
                Ok(existing) => {
                    this.dialog = Some(Dialog::Conflict { uploads: paths, dir, existing });
                    cx.notify();
                }
                Err(err) => this.notify_error(format!("Can't upload to {dir}: {}", err.message), cx),
            });
        })
        .detach();
    }

    /// "Keep both" on the conflict dialog: upload under free names. `existing` are the names
    /// found taken in `dir`; when `dir` isn't the folder on screen (the files were dropped on
    /// a folder row), what else is in it is listed first, so a "name (1)" is free too.
    fn keep_both(&mut self, paths: Vec<PathBuf>, dir: String, existing: Vec<String>, cx: &mut Context<Self>) {
        let mut taken = names_in(&dir, self.cwd.as_deref(), &self.entries, &existing);
        if self.cwd.as_deref() == Some(dir.as_str()) {
            self.start_uploads(paths, dir, false, Some(taken), cx);
            return;
        }
        let Some(remote) = self.remote() else { return };
        let list_dir = dir.clone();
        let task = sftp::spawn(async move { remote.list(&list_dir).await });
        cx.spawn(async move |this, cx| {
            // Not listed: the names known to be taken still get free ones.
            if let Ok(Ok(listing)) = task.await {
                taken.extend(listing.entries.into_iter().map(|e| e.name));
            }
            let _ = this.update(cx, |this, cx| this.start_uploads(paths, dir, false, Some(taken), cx));
        })
        .detach();
    }

    /// `replace` replaces files of the same name; with `taken` (Keep both), a new one whose
    /// name is in it gets a free name instead.
    fn start_uploads(&mut self, paths: Vec<PathBuf>, dir: String, replace: bool, taken: Option<HashSet<String>>, cx: &mut Context<Self>) {
        let Some(remote) = self.remote() else { return };
        let source = cx.entity().downgrade();
        let host = self.host_label.clone();
        let names = upload_names(&paths, taken);
        self.transfers.update(cx, |t, cx| {
            for (path, name) in paths.into_iter().zip(names) {
                let is_dir = path.is_dir();
                let job = Job::Upload { local: path, remote_dir: dir.clone(), name: name.clone(), replace };
                t.start(job, name, is_dir, host.clone(), source.clone(), remote.clone(), cx);
            }
        });
    }

    fn start_new_folder(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.cwd.is_none() {
            return;
        }
        if self.dir_access.write == Allowed::No {
            self.notify_error("You don't have permission to create folders here.", cx);
            return;
        }
        let taken: HashSet<String> = self.entries.iter().map(|e| e.name.clone()).collect();
        let name = sftp::unique_name("New folder", |n| taken.contains(n));
        self.begin_edit(EditKind::NewFolder, &name, window, cx);
        self.scroll.scroll_to_item(0, ScrollStrategy::Top);
    }

    fn start_rename(&mut self, path: String, window: &mut Window, cx: &mut Context<Self>) {
        let Some(entry) = self.entry(&path) else { return };
        // F2 as the menu does: not where nothing can be renamed.
        if self.dir_access.write == Allowed::No {
            self.notify_error("You don't have permission to rename items here.", cx);
            return;
        }
        let name = entry.name.clone();
        self.selected = HashSet::from([path.clone()]);
        self.anchor = Some(path.clone());
        self.cursor = None;
        self.begin_edit(EditKind::Rename { path }, &name, window, cx);
    }

    fn begin_edit(&mut self, kind: EditKind, value: &str, window: &mut Window, cx: &mut Context<Self>) {
        let input = cx.new(|cx| InputState::new(window, cx).default_value(value.to_string()));
        let subscription = cx.subscribe_in(&input, window, |this, _, event: &InputEvent, window, cx| match event {
            InputEvent::PressEnter { .. } => {
                this.commit_edit(cx);
                window.focus(&this.focus, cx);
            }
            InputEvent::Blur => this.commit_edit(cx),
            _ => {}
        });
        input.update(cx, |state, cx| {
            state.focus(window, cx);
            state.select_all(window, cx);
        });
        self.edit = Some(Edit { kind, input, _subscription: subscription });
        cx.notify();
    }

    fn cancel_edit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.edit = None;
        window.focus(&self.focus, cx);
        cx.notify();
    }

    fn commit_edit(&mut self, cx: &mut Context<Self>) {
        let Some(edit) = self.edit.take() else { return };
        let Some(cwd) = self.cwd.clone() else { return };
        let typed = edit.input.read(cx).value().to_string();
        let name = typed.trim().to_string();
        cx.notify();
        // Left as it was: a name with spaces at its ends must not lose them.
        if let EditKind::Rename { path } = &edit.kind
            && sftp::file_name(path) == typed
        {
            return;
        }
        if name.is_empty() {
            return;
        }
        if name.contains('/') || name == "." || name == ".." {
            self.notify_error(format!("\"{name}\" can't be used as a name."), cx);
            return;
        }
        match edit.kind {
            EditKind::NewFolder => {
                let path = sftp::join(&cwd, &name);
                self.select_after_load = Some(path.clone());
                self.run_op(
                    "Creating folder…",
                    move |r| Box::pin(async move { r.mkdir(&path).await }),
                    cx,
                    move |this, result, cx| {
                        if let Err(err) = result {
                            this.select_after_load = None;
                            this.notify_error(explain("create the folder", &err), cx);
                        }
                    },
                );
            }
            EditKind::Rename { path } => {
                let Some(dir) = sftp::parent(&path) else { return };
                let to = sftp::join(&dir, &name);
                if to == path {
                    return;
                }
                self.select_after_load = Some(to.clone());
                let old = sftp::file_name(&path).to_string();
                self.run_op(
                    "Renaming…",
                    move |r| Box::pin(async move { r.rename(&path, &to).await }),
                    cx,
                    move |this, result, cx| {
                        if let Err(err) = result {
                            this.select_after_load = None;
                            this.notify_error(explain(&format!("rename {old}"), &err), cx);
                        }
                    },
                );
            }
        }
    }

    fn ask_copy_move(&mut self, op: CopyOp, paths: Vec<String>, window: &mut Window, cx: &mut Context<Self>) {
        if paths.is_empty() {
            return;
        }
        let start = self.cwd.clone().unwrap_or_default();
        let input = cx.new(|cx| InputState::new(window, cx).default_value(start).placeholder("/path/to/folder"));
        let subscription = cx.subscribe_in(&input, window, |this, _, event: &InputEvent, window, cx| {
            if let InputEvent::PressEnter { .. } = event {
                this.submit_copy_move(window, cx);
            }
        });
        input.update(cx, |state, cx| {
            state.focus(window, cx);
            state.select_all(window, cx);
        });
        self.dialog = Some(Dialog::CopyMove { op, paths, input, _subscription: subscription });
        cx.notify();
    }

    fn submit_copy_move(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(Dialog::CopyMove { op, paths, input, .. }) = self.dialog.take() else { return };
        window.focus(&self.focus, cx);
        let text = input.read(cx).value().trim().to_string();
        if text.is_empty() {
            return;
        }
        let dest = self.expand(&text);
        let taken: HashSet<String> = self.entries.iter().map(|e| e.name.clone()).collect();
        let in_cwd = self.cwd.as_deref() == Some(dest.trim_end_matches('/')) || self.cwd.as_deref() == Some(dest.as_str());
        let pairs = copy_targets(op, paths, &dest, &taken);
        if pairs.is_empty() {
            // Moving to where it already is: say so rather than close without a word.
            self.notify_info("Already in that folder.", cx);
            return;
        }
        let total = pairs.len();
        let (verb, busy) = match op {
            CopyOp::Copy => ("copy", "Copying…"),
            CopyOp::Move => ("move", "Moving…"),
        };
        let shown_dest = dest.clone();
        self.run_op(
            busy,
            move |r| {
                Box::pin(async move {
                    let mut done = 0;
                    for (from, to) in pairs {
                        let result = match op {
                            CopyOp::Copy => r.copy(&from, &to).await,
                            CopyOp::Move => r.rename(&from, &to).await,
                        };
                        if let Err(err) = result {
                            return Ok((done, Some((sftp::file_name(&from).to_string(), err))));
                        }
                        done += 1;
                    }
                    Ok((done, None))
                })
            },
            cx,
            move |this, result: Result<(usize, Option<(String, OpError)>), OpError>, cx| match result {
                Ok((_, None)) if !in_cwd => {
                    let past = if op == CopyOp::Copy { "Copied" } else { "Moved" };
                    this.notify_info(format!("{past} {} to {shown_dest}", plural(total, "item")), cx);
                }
                Ok((_, None)) => {}
                Ok((done, Some((name, err)))) => {
                    let prefix = if done > 0 { format!("{done} of {total} done. ") } else { String::new() };
                    this.notify_error(format!("{prefix}{}", explain(&format!("{verb} {name}"), &err)), cx);
                }
                Err(err) => this.notify_error(explain(verb, &err), cx),
            },
        );
    }

    /// Rows dropped on a folder: move them there, if they are this browser's own.
    fn drop_files(&mut self, dragged: &DraggedFiles, dest: String, cx: &mut Context<Self>) {
        if let Some(paths) = dragged.paths_for(cx.entity_id()) {
            self.move_into(paths.to_vec(), dest, cx);
        }
    }

    /// Move entries dragged onto a folder.
    fn move_into(&mut self, paths: Vec<String>, dest: String, cx: &mut Context<Self>) {
        let pairs: Vec<(String, String)> = paths
            .into_iter()
            .filter(|p| p != &dest && sftp::parent(p).as_deref() != Some(dest.as_str()) && !dest.starts_with(&format!("{p}/")))
            .map(|p| {
                let to = sftp::join(&dest, sftp::file_name(&p));
                (p, to)
            })
            .collect();
        if pairs.is_empty() {
            return;
        }
        let total = pairs.len();
        self.run_op(
            "Moving…",
            move |r| {
                Box::pin(async move {
                    let mut done = 0;
                    for (from, to) in pairs {
                        if let Err(err) = r.rename(&from, &to).await {
                            return Ok((done, Some((sftp::file_name(&from).to_string(), err))));
                        }
                        done += 1;
                    }
                    Ok((done, None))
                })
            },
            cx,
            // As with delete: say how many moved before one failed.
            move |this, result: Result<(usize, Option<(String, OpError)>), OpError>, cx| match result {
                Ok((_, None)) => {}
                Ok((done, Some((name, err)))) => {
                    let prefix = if done > 0 { format!("Moved {done} of {total}. ") } else { String::new() };
                    this.notify_error(format!("{prefix}{}", explain(&format!("move {name}"), &err)), cx);
                }
                Err(err) => this.notify_error(explain("move", &err), cx),
            },
        );
    }

    fn ask_delete(&mut self, paths: Vec<String>, cx: &mut Context<Self>) {
        if paths.is_empty() {
            return;
        }
        if self.dir_access.write == Allowed::No {
            self.notify_error("You don't have permission to delete here: the folder isn't writable for you.", cx);
            return;
        }
        self.dialog = Some(Dialog::Delete { paths });
        cx.notify();
    }

    fn delete(&mut self, paths: Vec<String>, cx: &mut Context<Self>) {
        let total = paths.len();
        self.run_op(
            "Deleting…",
            move |r| {
                Box::pin(async move {
                    let mut done = 0;
                    for path in paths {
                        if let Err(err) = r.remove(&path).await {
                            return Ok((done, Some((sftp::file_name(&path).to_string(), err))));
                        }
                        done += 1;
                    }
                    Ok((done, None))
                })
            },
            cx,
            move |this, result: Result<(usize, Option<(String, OpError)>), OpError>, cx| match result {
                Ok((_, None)) => {}
                Ok((done, Some((name, err)))) => {
                    let prefix = if done > 0 { format!("Deleted {done} of {total}. ") } else { String::new() };
                    this.notify_error(format!("{prefix}{}", explain(&format!("delete {name}"), &err)), cx);
                }
                Err(err) => this.notify_error(explain("delete", &err), cx),
            },
        );
    }

    fn copy_paths(&mut self, paths: Vec<String>, cx: &mut Context<Self>) {
        let n = paths.len();
        cx.write_to_clipboard(gpui::ClipboardItem::new_string(paths.join("\n")));
        self.notify_info(if n == 1 { "Path copied.".to_string() } else { format!("{n} paths copied.") }, cx);
    }

    /// Take the terminal to `entry`: into it if it is a folder, else into the folder it is in.
    /// The browser follows to the same folder.
    fn jump_to(&mut self, entry: &Entry, cx: &mut Context<Self>) {
        let dir = if entry.is_dir() {
            self.navigate(entry.path.clone(), cx);
            entry.path.clone()
        } else {
            match sftp::parent(&entry.path) {
                Some(parent) => parent,
                None => return,
            }
        };
        cx.emit(BrowserEvent::JumpTo(dir));
    }

    /// The terminal couldn't take the `cd` (see `TerminalView::run_line`).
    pub fn jump_refused(&mut self, cx: &mut Context<Self>) {
        self.notify_error("The terminal isn't at a shell prompt (a program is running or the session ended).", cx);
    }

    fn add_favorite(&mut self, path: String, host_only: bool, cx: &mut Context<Self>) {
        let host = host_only.then(|| self.host_id.clone());
        self.favorites.update(cx, |store, cx| {
            store.change(|f| {
                f.add(&path, host.as_deref());
            }, cx)
        });
        let scope = if host_only { format!("for {}", self.host_label) } else { "for all hosts".to_string() };
        let name = match sftp::file_name(&path) {
            "" => "/",
            name => name,
        };
        self.notify_info(format!("Added {name} to favorites {scope}."), cx);
    }

    fn open_favorite(&mut self, path: String, window: &mut Window, cx: &mut Context<Self>) {
        self.close_favorites(window, cx);
        self.navigate(path, cx);
    }

    fn start_favorite_rename(&mut self, id: String, name: String, window: &mut Window, cx: &mut Context<Self>) {
        let input = cx.new(|cx| InputState::new(window, cx).default_value(name));
        let fav_id = id.clone();
        let subscription = cx.subscribe_in(&input, window, move |this, input, event: &InputEvent, window, cx| match event {
            InputEvent::PressEnter { .. } | InputEvent::Blur => {
                let name = input.read(cx).value().to_string();
                let id = fav_id.clone();
                this.favorite_rename = None;
                this.favorites.update(cx, |store, cx| store.change(|f| f.rename(&id, &name), cx));
                // Renamed with Enter: the keyboard stays in the favorites list.
                if matches!(event, InputEvent::PressEnter { .. }) {
                    window.focus(&this.favorites_focus, cx);
                }
                cx.notify();
            }
            _ => {}
        });
        input.update(cx, |state, cx| {
            state.focus(window, cx);
            state.select_all(window, cx);
        });
        self.favorite_rename = Some((id, input, subscription));
        cx.notify();
    }

    fn on_key_down(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let key = event.keystroke.key.as_str();
        let mods = event.keystroke.modifiers;
        if key == "escape" {
            if self.popup.is_open() {
                return;
            }
            if self.edit.is_some() {
                self.cancel_edit(window, cx);
            } else if self.dialog.is_some() {
                self.dialog = None;
                window.focus(&self.focus, cx);
            } else if self.favorite_rename.is_some() {
                // Renaming a favorite: Escape ends the rename, not the whole list.
                self.favorite_rename = None;
                window.focus(&self.favorites_focus, cx);
            } else if self.favorites_open {
                self.close_favorites(window, cx);
            } else if self.editing_path {
                self.editing_path = false;
                window.focus(&self.focus, cx);
            } else {
                self.selected.clear();
            }
            cx.stop_propagation();
            cx.notify();
            return;
        }
        // Everything else only when the list itself has the focus (not a text field).
        if !self.focus.is_focused(window) {
            return;
        }
        // The delete question keeps the focus on the list: Enter answers it, as its button does.
        if key == "enter" && !mods.modified()
            && let Some(Dialog::Delete { paths }) = &self.dialog
        {
            let paths = paths.clone();
            self.dialog = None;
            self.delete(paths, cx);
            cx.stop_propagation();
            cx.notify();
            return;
        }
        if self.dialog.is_some() {
            return;
        }
        let handled = match (key, mods.control || mods.platform, mods.alt, mods.shift) {
            ("up", false, false, extend) => {
                self.move_cursor(-1, extend, cx);
                true
            }
            ("down", false, false, extend) => {
                self.move_cursor(1, extend, cx);
                true
            }
            ("home", false, false, _) => {
                self.move_cursor(-(self.rows.len() as isize), false, cx);
                true
            }
            ("end", false, false, _) => {
                self.move_cursor(self.rows.len() as isize, false, cx);
                true
            }
            ("enter", false, false, false) => {
                self.open_selection(cx);
                true
            }
            ("backspace", false, false, false) | ("up", false, true, false) => {
                self.go_up(cx);
                true
            }
            ("left", false, true, false) => {
                self.go_back(cx);
                true
            }
            ("right", false, true, false) => {
                self.go_forward(cx);
                true
            }
            ("f5", ..) | ("r", true, false, false) => {
                self.refresh(cx);
                true
            }
            ("f2", false, false, false) => {
                if let [entry] = self.selection().as_slice() {
                    self.start_rename(entry.path.clone(), window, cx);
                }
                true
            }
            ("delete", false, false, false) => {
                let paths = self.selection().into_iter().map(|e| e.path).collect();
                self.ask_delete(paths, cx);
                true
            }
            ("a", true, false, false) => {
                self.select_all(cx);
                true
            }
            ("l", true, false, false) => {
                self.edit_path(window, cx);
                true
            }
            ("n", true, false, true) => {
                self.start_new_folder(window, cx);
                true
            }
            ("h", true, false, false) => {
                self.set_show_hidden(!self.show_hidden, cx);
                true
            }
            ("f10", false, false, true) | ("menu", false, false, false) => {
                self.open_menu_from_keyboard(window, cx);
                true
            }
            _ => false,
        };
        if handled {
            cx.stop_propagation();
        }
    }

    /// The keyboard is in the list and was last used: the row it is on wears the focus ring,
    /// or the list does until the keyboard is on a row.
    fn keyboard_in_list(&self, window: &Window) -> bool {
        self.focus.is_focused(window) && window.last_input_was_keyboard()
    }

    /// A click on a toolbar button leaves the keyboard in the list, as before the buttons
    /// could take focus: arrows still move through the files afterwards. The path field
    /// keeps it while it is being typed in.
    fn keep_list_focus(&mut self, _: &MouseDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        if !self.editing_path {
            window.focus(&self.focus, cx);
        }
    }

    /// Close the favorites list; the keyboard goes back to the files if it was in the list.
    fn close_favorites(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.favorites_open = false;
        self.favorite_rename = None;
        if self.favorites_focus.contains_focused(window, cx) {
            window.focus(&self.focus, cx);
        }
        cx.notify();
    }

    /// The menu of the selected rows (or of the folder, with none), from the keyboard: beside
    /// the row the keyboard is on, or at the top of the list.
    fn open_menu_from_keyboard(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let (bounds, offset) = {
            let state = self.scroll.0.borrow();
            (state.base_handle.bounds(), state.base_handle.offset())
        };
        let row = self.cursor.as_ref().or(self.anchor.as_ref()).filter(|p| self.selected.contains(*p)).and_then(|p| self.visible_index(p));
        let y = match row {
            Some(ix) => bounds.top() + offset.y + px(4. + ROW_HEIGHT * (ix + self.has_parent() as usize + 1) as f32),
            None => bounds.top() + px(4.),
        };
        let position = gpui::point(bounds.left() + px(36.), y.clamp(bounds.top(), bounds.bottom().max(bounds.top())));
        if self.selected.is_empty() {
            let view = cx.entity().downgrade();
            let (cwd, show_hidden, connected) = (self.cwd.clone(), self.show_hidden, self.remote().is_some());
            self.popup = Popup::open(position, |this: &mut Self| &mut this.popup, window, cx, move |menu, _, _| {
                folder_menu(menu, &view, cwd, show_hidden, connected)
            });
        } else {
            self.open_entry_menu(position, window, cx);
        }
        cx.notify();
    }

    /// The menu of the selected rows at `position`.
    fn open_entry_menu(&mut self, position: gpui::Point<gpui::Pixels>, window: &mut Window, cx: &mut Context<Self>) {
        let view = cx.entity().downgrade();
        let targets = self.selection();
        let dir_write = self.dir_access.write;
        let favorites = self.favorites.read(cx).data.clone();
        let host = self.host_id.clone();
        let extras = self.menu_extras;
        self.popup = Popup::open(position, |this: &mut Self| &mut this.popup, window, cx, move |menu, _, _| {
            entry_menu(menu, &view, targets, dir_write, &favorites, &host, extras)
        });
    }

    fn edit_path(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let cwd = self.cwd.clone().unwrap_or_default();
        self.editing_path = true;
        self.path_input.update(cx, |state, cx| {
            state.set_value(cwd, window, cx);
            state.focus(window, cx);
            state.select_all(window, cx);
        });
        cx.notify();
    }
}

/// "Couldn't delete app.log: Permission denied (EACCES)", plus what that means here.
fn explain(action: &str, err: &OpError) -> String {
    let hint = match err.kind {
        ErrorKind::PermissionDenied => " You don't have permission for this on the server.",
        ErrorKind::Connection => " Check the session's connection and try again.",
        _ => "",
    };
    format!("Couldn't {action}: {}.{hint}", err.message.trim_end_matches('.'))
}

fn list_names(names: &[&str]) -> String {
    match names {
        [one] => format!("\"{one}\""),
        [first, rest @ ..] => format!("\"{first}\" and {} more", rest.len()),
        [] => String::new(),
    }
}

/// The name a local file or folder gets on the server.
fn upload_name(path: &std::path::Path) -> String {
    path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "upload".into())
}

/// The names uploads get on the server. With `taken` (Keep both), a name in it, or given to
/// an earlier upload of the same batch, becomes a free one: "app (1).conf".
fn upload_names(paths: &[PathBuf], taken: Option<HashSet<String>>) -> Vec<String> {
    let Some(mut taken) = taken else { return paths.iter().map(|p| upload_name(p)).collect() };
    paths
        .iter()
        .map(|path| {
            let name = sftp::unique_name(&upload_name(path), |n| taken.contains(n));
            taken.insert(name.clone());
            name
        })
        .collect()
}

/// The names known to be taken in `dir`: those found there (`existing`), and everything in
/// it when it is the folder on screen.
fn names_in(dir: &str, cwd: Option<&str>, entries: &[Entry], existing: &[String]) -> HashSet<String> {
    let mut taken: HashSet<String> = existing.iter().cloned().collect();
    if cwd == Some(dir) {
        taken.extend(entries.iter().map(|e| e.name.clone()));
    }
    taken
}

/// Case-insensitive, with numbers compared by value ("file2" before "file10").
fn natural_cmp(a: &str, b: &str) -> std::cmp::Ordering {
    let (mut a, mut b) = (a.chars().peekable(), b.chars().peekable());
    loop {
        match (a.peek().copied(), b.peek().copied()) {
            (None, None) => return std::cmp::Ordering::Equal,
            (None, Some(_)) => return std::cmp::Ordering::Less,
            (Some(_), None) => return std::cmp::Ordering::Greater,
            (Some(x), Some(y)) if x.is_ascii_digit() && y.is_ascii_digit() => {
                let mut na = String::new();
                while let Some(c) = a.peek().copied().filter(char::is_ascii_digit) {
                    na.push(c);
                    a.next();
                }
                let mut nb = String::new();
                while let Some(c) = b.peek().copied().filter(char::is_ascii_digit) {
                    nb.push(c);
                    b.next();
                }
                let (ta, tb) = (na.trim_start_matches('0'), nb.trim_start_matches('0'));
                let order = ta.len().cmp(&tb.len()).then_with(|| ta.cmp(tb));
                if order != std::cmp::Ordering::Equal {
                    return order;
                }
            }
            (Some(x), Some(y)) => {
                let order = x.to_lowercase().cmp(y.to_lowercase());
                if order != std::cmp::Ordering::Equal {
                    return order;
                }
                a.next();
                b.next();
            }
        }
    }
}

fn format_time(mtime: Option<u32>) -> String {
    mtime
        .and_then(|t| chrono::DateTime::from_timestamp(t as i64, 0))
        .map(|t| t.with_timezone(&chrono::Local).format("%Y-%m-%d %H:%M").to_string())
        .unwrap_or_else(|| "—".into())
}

/// Who owns it and what the signed-in user may do, for the lock and permission tooltips.
fn permission_text(entry: &Entry, me: &Identity) -> String {
    let who = |uid: Option<u32>| match (uid, me.uid) {
        (Some(u), Some(mine)) if u == mine => format!("{} (you)", me.name.clone().unwrap_or_else(|| u.to_string())),
        (Some(0), _) => "root".into(),
        (Some(u), _) => format!("uid {u}"),
        (None, _) => "unknown".into(),
    };
    let group = |gid: Option<u32>| match gid {
        Some(g) => me.group_name(g).map(|n| format!("{n} (yours)")).unwrap_or_else(|| format!("gid {g}")),
        None => "unknown".into(),
    };
    let mark = |a: Allowed| match a {
        Allowed::Yes => "yes",
        Allowed::No => "no",
        Allowed::Unknown => "?",
    };
    let mut text = format!("{} · owner {} · group {}", entry.mode_string(), who(entry.uid), group(entry.gid));
    if entry.access == Access::UNKNOWN {
        text.push_str("\nWhat you may do here couldn't be worked out.");
    } else {
        let a = entry.access;
        text.push_str(&format!("\nYou: read {} · write {} · {} {}", mark(a.read), mark(a.write), if entry.is_dir() { "enter" } else { "execute" }, mark(a.exec)));
        if entry.locked() {
            text.push_str(if entry.is_dir() {
                "\nYou can't open this folder."
            } else {
                "\nYou can't read this file, so it can't be downloaded."
            });
        }
    }
    text
}

fn file_icon(entry: &Entry) -> &'static str {
    if entry.is_dir() {
        return if entry.link { "folder-symlink" } else { "folder" };
    }
    if entry.link {
        return "file-symlink";
    }
    let ext = entry.name.rsplit_once('.').map(|(_, e)| e.to_lowercase()).unwrap_or_default();
    match ext.as_str() {
        "conf" | "cfg" | "ini" | "toml" | "yaml" | "yml" | "env" | "service" | "properties" => "file-cog",
        "sh" | "bash" | "zsh" | "ps1" | "py" | "rb" | "pl" => "file-terminal",
        "rs" | "js" | "ts" | "go" | "c" | "h" | "cpp" | "java" | "php" | "html" | "css" | "json" | "xml" | "sql" => "file-code",
        "png" | "jpg" | "jpeg" | "gif" | "svg" | "webp" | "ico" => "file-image",
        "zip" | "gz" | "tgz" | "tar" | "bz2" | "xz" | "7z" | "rar" | "zst" => "file-archive",
        "csv" | "tsv" | "xlsx" => "sheet",
        _ => "file-text",
    }
}

// ---- rendering --------------------------------------------------------------------

/// The focus ring of the list's keyboard row, inside its edge so the columns don't move.
fn keyboard_ring(p: AppPalette) -> Vec<gpui::BoxShadow> {
    vec![gpui::BoxShadow {
        color: p.focus,
        offset: gpui::point(px(0.), px(0.)),
        blur_radius: px(0.),
        spread_radius: px(1.5),
        inset: true,
    }]
}

/// How a control that can't be used right now draws its icon and label.
fn disabled_color(p: AppPalette) -> Hsla {
    p.text_faint.opacity(0.55)
}

/// What every control in the browser shares: a button for screen readers, named `label`, and
/// a Tab stop with the focus ring while it can be used. One that can't is skipped by Tab.
fn control(id: impl Into<gpui::ElementId>, label: impl Into<SharedString>, enabled: bool, p: AppPalette) -> gpui::Stateful<gpui::Div> {
    div()
        .id(id)
        .role(gpui::Role::Button)
        .aria_label(label)
        .when(enabled, |el| el.tab_index(0).focus_visible(move |s| focus_outline(s, p.focus)))
}

/// A tooltip's name and the shortcut in its brackets: "Refresh (F5)" is ("Refresh", Some("F5")).
/// Screen readers announce the shortcut on its own, after the name.
fn split_shortcut(tip: &str) -> (&str, Option<&str>) {
    match tip.strip_suffix(')').and_then(|t| t.rsplit_once(" (")) {
        Some((name, keys)) => (name, Some(keys)),
        None => (tip, None),
    }
}

fn tool_button(id: impl Into<gpui::ElementId>, icon: &str, tip: &'static str, enabled: bool, p: AppPalette) -> gpui::Stateful<gpui::Div> {
    let (label, keys) = split_shortcut(tip);
    control(id, label, enabled, p)
        .when_some(keys, |el, keys| el.aria_keyshortcuts(keys))
        .size(px(28.))
        .flex()
        .flex_shrink_0()
        .items_center()
        .justify_center()
        .rounded(p.corner(7.))
        .tooltip(move |window, cx| Tooltip::new(tip).build(window, cx))
        .when(enabled, |el| el.cursor_pointer().hover(move |s| s.bg(p.hover)))
        .child(icon_el(icon, 15., if enabled { p.text_muted } else { disabled_color(p) }))
}

/// A bordered toolbar button with an icon and, when there is room, a label.
fn labeled_button(
    id: &'static str,
    icon: &str,
    label: &'static str,
    show_label: bool,
    enabled: bool,
    p: AppPalette,
) -> gpui::Stateful<gpui::Div> {
    let fg = if enabled { p.text } else { disabled_color(p) };
    control(id, label, enabled, p)
        .h(px(30.))
        .px(px(if show_label { 10. } else { 7. }))
        .flex()
        .flex_shrink_0()
        .items_center()
        .gap(px(6.))
        .rounded(p.corner(8.))
        .border_1()
        .border_color(p.border)
        .bg(p.hover)
        .text_size(px(12.5))
        .font_weight(FontWeight::MEDIUM)
        .text_color(fg)
        .when(enabled, |el| el.cursor_pointer().hover(move |s| s.bg(p.active)))
        .when(!show_label, |el| el.tooltip(move |window, cx| Tooltip::new(label).build(window, cx)))
        .child(icon_el(icon, 14., if enabled { p.text_muted } else { disabled_color(p) }))
        .when(show_label, |el| el.child(label))
}

impl FileBrowser {
    fn render_header(&self, p: AppPalette, cx: &mut Context<Self>) -> AnyElement {
        let (dot, state) = match &self.conn {
            Conn::Ready(_) => (p.success, "Connected"),
            Conn::Connecting => (p.warning, "Connecting"),
            Conn::Waiting(_) => (p.text_faint, "Not connected"),
            Conn::Failed(_) => (p.danger, "SFTP unavailable"),
        };
        let view = cx.entity().downgrade();
        let can_create = self.cwd.is_some() && self.dir_access.write != Allowed::No && self.remote().is_some();
        div()
            .pl(px(14.))
            .pr(px(8.))
            .pt(px(10.))
            .pb(px(8.))
            .flex()
            .items_center()
            .gap(px(10.))
            .on_mouse_down(MouseButton::Left, cx.listener(Self::keep_list_focus))
            .child(
                div()
                    .size(px(30.))
                    .flex()
                    .flex_shrink_0()
                    .items_center()
                    .justify_center()
                    .rounded(p.corner(8.))
                    .border_1()
                    .border_color(p.border)
                    .child(icon_el("folder-open", 15., p.text_muted)),
            )
            .child(
                div()
                    .id("sftp-title")
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .tooltip(move |window, cx| Tooltip::new(state).build(window, cx))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(8.))
                            .child(
                                div()
                                    .min_w_0()
                                    .truncate()
                                    .text_size(px(13.5))
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .text_color(p.text)
                                    .child(format!("SFTP — {}", self.host_label)),
                            )
                            .child(
                                div()
                                    .id("sftp-state")
                                    .role(gpui::Role::Status)
                                    .aria_label(format!("SFTP: {}", state.to_lowercase()))
                                    .child(status_dot(dot)),
                            ),
                    )
                    .child(div().truncate().text_size(px(11.5)).text_color(p.text_faint).child(self.target.clone())),
            )
            .child(
                tool_button("sftp-new-folder", "folder-plus", "New folder (Ctrl+Shift+N)", can_create, p).on_click(cx.listener(
                    move |this, _: &ClickEvent, window, cx| {
                        if can_create {
                            this.start_new_folder(window, cx)
                        }
                    },
                )),
            )
            .child(tool_button("sftp-more", "ellipsis", "More", true, p).on_click(cx.listener(move |this, event: &ClickEvent, window, cx| {
                let view = view.clone();
                let show_hidden = this.show_hidden;
                let cwd = this.cwd.clone();
                let connected = this.remote().is_some();
                this.popup = Popup::open(event.position(), |this: &mut Self| &mut this.popup, window, cx, move |menu, _, _| {
                    folder_menu(menu, &view, cwd, show_hidden, connected)
                });
                cx.notify();
            })))
            .child(
                tool_button("sftp-close", "x", "Close file browser (Ctrl+Shift+E)", true, p)
                    .on_click(cx.listener(|_, _: &ClickEvent, _, cx| cx.emit(BrowserEvent::Close))),
            )
            .into_any_element()
    }

    fn render_toolbar(&self, p: AppPalette, cx: &mut Context<Self>) -> AnyElement {
        let connected = self.remote().is_some();
        let me = cx.entity_id();
        let can_back = self.history_ix > 0;
        let can_forward = self.history_ix + 1 < self.history.len();
        let can_up = self.has_parent();
        let wide = self.width >= 560.;
        let is_favorite = self.cwd.as_deref().is_some_and(|c| self.favorites.read(cx).data.contains(&self.host_id, c));
        let selection = self.selection();
        let can_download = !selection.is_empty() && selection.iter().any(|e| !e.locked() && e.kind != Kind::Other);
        let can_upload = connected && self.cwd.is_some() && self.dir_access.write != Allowed::No;
        let view = cx.entity().downgrade();

        let path_field: AnyElement = if self.editing_path {
            div()
                .flex_1()
                .min_w_0()
                .child(Input::new(&self.path_input).small().prefix(icon_el("hard-drive", 13., p.text_faint)))
                .into_any_element()
        } else {
            let segments = self.cwd.as_deref().map(sftp::segments).unwrap_or_default();
            // Long paths keep their last folders; the rest folds into "…".
            let room = ((self.width - if wide { 420. } else { 300. }) / 64.).max(1.) as usize;
            let skip = segments.len().saturating_sub(room + 1);
            let mut crumbs: Vec<AnyElement> = vec![];
            for (i, (name, full)) in segments.iter().enumerate() {
                if i > 0 && i <= skip {
                    if i == 1 {
                        crumbs.push(div().px(px(2.)).text_color(p.text_faint).child("…").into_any_element());
                    }
                    continue;
                }
                if i > 1 {
                    crumbs.push(div().text_color(p.text_faint).child("/").into_any_element());
                }
                let target = full.clone();
                let last = i + 1 == segments.len();
                crumbs.push(
                    control(("crumb", i), if name == "/" { "Go to /".to_string() } else { format!("Go to {full}") }, true, p)
                        .px(px(4.))
                        .h(px(22.))
                        .flex()
                        .flex_shrink_0()
                        .items_center()
                        .rounded(p.corner(5.))
                        .cursor_pointer()
                        .text_color(if last { p.text } else { p.text_muted })
                        .hover(|s| s.bg(p.active).text_color(p.active_text))
                        .drag_over::<DraggedFiles>(move |s, d, _, _| if d.source == me { s.bg(p.brand_soft) } else { s })
                        .on_drop(cx.listener({
                            let target = target.clone();
                            move |this, dragged: &DraggedFiles, _, cx| this.drop_files(dragged, target.clone(), cx)
                        }))
                        .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                            cx.stop_propagation();
                            this.navigate(target.clone(), cx)
                        }))
                        .child(if name == "/" { icon_el("hard-drive", 13., p.text_faint) } else { div().child(name.clone()).into_any_element() })
                        .into_any_element(),
                );
            }
            div()
                .id("sftp-path")
                .flex_1()
                .min_w_0()
                .h(px(30.))
                .px(px(6.))
                .flex()
                .items_center()
                .gap(px(1.))
                .overflow_hidden()
                .rounded(p.corner(8.))
                .border_1()
                .border_color(p.border)
                .bg(p.hover)
                .text_size(px(12.5))
                .cursor_text()
                .tooltip(|window, cx| Tooltip::new("Click to type a path (Ctrl+L)").build(window, cx))
                .when(connected, |el| el.on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.edit_path(window, cx))))
                .children(crumbs)
                .into_any_element()
        };

        div()
            .px(px(10.))
            .pb(px(8.))
            .flex()
            .items_center()
            .gap(px(6.))
            .on_mouse_down(MouseButton::Left, cx.listener(Self::keep_list_focus))
            .child(
                div()
                    .flex()
                    .flex_shrink_0()
                    .rounded(p.corner(8.))
                    .border_1()
                    .border_color(p.border)
                    .child(tool_button("sftp-back", "chevron-left", "Back (Alt+Left)", can_back, p).on_click(cx.listener(
                        |this, _: &ClickEvent, _, cx| this.go_back(cx),
                    )))
                    .child(tool_button("sftp-forward", "chevron-right", "Forward (Alt+Right)", can_forward, p).on_click(
                        cx.listener(|this, _: &ClickEvent, _, cx| this.go_forward(cx)),
                    ))
                    .child(tool_button("sftp-up", "arrow-up", "Parent folder (Backspace)", can_up, p).on_click(cx.listener(
                        |this, _: &ClickEvent, _, cx| this.go_up(cx),
                    ))),
            )
            .child(path_field)
            .child(
                control("sftp-favorites", "Favorites", connected, p)
                    .aria_expanded(self.favorites_open)
                    .size(px(30.))
                    .flex()
                    .flex_shrink_0()
                    .items_center()
                    .justify_center()
                    .rounded(p.corner(8.))
                    .border_1()
                    .border_color(if self.favorites_open { p.brand.opacity(0.6) } else { p.border })
                    .when(self.favorites_open, |el| el.bg(p.active))
                    .when(connected, |el| el.cursor_pointer().hover(|s| s.bg(p.active)))
                    .tooltip(move |window, cx| {
                        Tooltip::new(if is_favorite { "Favorites (this folder is one)" } else { "Favorites" }).build(window, cx)
                    })
                    .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                        let just_closed = this.favorites_closed_at.is_some_and(|at| at.elapsed() < Duration::from_millis(300));
                        if connected && !just_closed {
                            if this.favorites_open {
                                this.close_favorites(window, cx);
                            } else {
                                this.favorites_open = true;
                                this.favorite_rename = None;
                                window.focus(&this.favorites_focus, cx);
                            }
                            cx.notify();
                        }
                    }))
                    .child(icon_el(
                        "star",
                        15.,
                        match (connected, is_favorite) {
                            (false, _) => disabled_color(p),
                            (true, true) => p.warning,
                            (true, false) => p.text_muted,
                        },
                    )),
            )
            .child(tool_button("sftp-refresh", "refresh-cw", "Refresh (F5)", connected, p).on_click(cx.listener(
                |this, _: &ClickEvent, _, cx| this.refresh(cx),
            )))
            .child(
                labeled_button("sftp-upload", "upload", "Upload", wide, can_upload, p).on_click(cx.listener(
                    move |this, event: &ClickEvent, window, cx| {
                        if !can_upload {
                            if this.dir_access.write == Allowed::No {
                                this.notify_error("You don't have permission to upload to this folder.", cx);
                            }
                            return;
                        }
                        let view = view.clone();
                        this.popup = Popup::open(event.position(), |this: &mut Self| &mut this.popup, window, cx, move |menu, _, _| {
                            menu.item(popup::item("Upload files…", None, &view, |this, window, cx| this.pick_uploads(false, window, cx)))
                                .item(popup::item("Upload folder…", None, &view, |this, window, cx| {
                                    this.pick_uploads(true, window, cx)
                                }))
                        });
                        cx.notify();
                    },
                )),
            )
            .child(labeled_button("sftp-download", "download", "Download", wide, can_download, p).on_click(cx.listener(
                move |this, _: &ClickEvent, _, cx| {
                    let selection = this.selection();
                    this.download(selection, false, cx);
                },
            )))
            .into_any_element()
    }

    fn render_columns(&self, p: AppPalette, cx: &mut Context<Self>) -> AnyElement {
        let (key, asc) = self.sort;
        let column = |id: &'static str, label: &'static str, k: SortKey, cx: &mut Context<Self>| {
            let sorted = match (key == k, asc) {
                (false, _) => String::new(),
                (true, true) => ", sorted ascending".into(),
                (true, false) => ", sorted descending".into(),
            };
            control(id, format!("Sort by {}{sorted}", label.to_lowercase()), true, p)
                .px(px(3.))
                .mx(px(-3.))
                .rounded(p.corner(4.))
                .flex()
                .items_center()
                .gap(px(4.))
                .cursor_pointer()
                .hover(|s| s.text_color(p.text))
                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| this.set_sort(k, cx)))
                .child(label)
                .when(key == k, |el| el.child(icon_el(if asc { "chevron-up" } else { "chevron-down" }, 11., p.text_muted)))
        };
        let (show_modified, show_perms) = self.columns();
        div()
            .mx(px(4.))
            .px(px(10.))
            .h(px(28.))
            .flex()
            .flex_shrink_0()
            .items_center()
            .gap(px(8.))
            .border_b_1()
            .border_color(p.border)
            .text_size(px(11.5))
            .font_weight(FontWeight::MEDIUM)
            .text_color(p.text_faint)
            .child(div().flex_1().min_w_0().pl(px(24.)).child(column("col-name", "Name", SortKey::Name, cx)))
            .child(div().w(px(70.)).flex().justify_end().child(column("col-size", "Size", SortKey::Size, cx)))
            .when(show_modified, |el| el.child(div().w(px(118.)).child(column("col-modified", "Modified", SortKey::Modified, cx))))
            .when(show_perms, |el| el.child(div().w(px(92.)).child(column("col-perms", "Perms", SortKey::Perms, cx))))
            .child(div().w(px(20.)))
            .into_any_element()
    }

    /// Which optional columns fit: (modified, permissions).
    fn columns(&self) -> (bool, bool) {
        (self.width >= 430., self.width >= 540.)
    }

    fn render_rows(&mut self, range: Range<usize>, window: &mut Window, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let p = cx.global::<Appearance>().app;
        let mono: SharedString = cx.global::<Appearance>().font_family.clone();
        let (show_modified, show_perms) = self.columns();
        let has_parent = self.has_parent();
        let identity = self.identity();
        let view = cx.entity().downgrade();
        let me = cx.entity_id();
        // What a drag from a selected row carries; worked out once, not once per row.
        let mut selection: Option<Vec<String>> = None;
        // The row the keyboard is on, which a screen reader reads out as the arrows move.
        let keyboard_row = self.cursor.clone().or_else(|| self.anchor.clone());
        let ringed = self.keyboard_in_list(window);
        let set_size = self.rows.len() + has_parent as usize;
        let mut out = vec![];
        for ix in range {
            if has_parent && ix == 0 {
                out.push(self.render_parent_row(p, show_modified, show_perms, cx));
                continue;
            }
            let Some(&entry_ix) = self.rows.get(ix - has_parent as usize) else { continue };
            let entry = self.entries[entry_ix].clone();
            let selected = self.selected.contains(&entry.path);
            let p = if selected { p.highlighted() } else { p };
            let locked = entry.locked();
            let renaming = matches!(&self.edit, Some(Edit { kind: EditKind::Rename { path }, .. }) if *path == entry.path);
            let perm_tip: SharedString = permission_text(&entry, &identity).into();
            let icon_color = if entry.is_dir() { p.warning } else { p.text_muted };
            let path = entry.path.clone();
            let name_color = if locked { p.text_muted } else { p.text };

            let name_cell: AnyElement = if renaming {
                let input = self.edit.as_ref().map(|e| e.input.clone()).expect("editing");
                div()
                    .flex_1()
                    .min_w_0()
                    .capture_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                        if event.keystroke.key == "escape" {
                            this.cancel_edit(window, cx);
                            cx.stop_propagation();
                        }
                    }))
                    .child(Input::new(&input).xsmall())
                    .into_any_element()
            } else {
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .items_center()
                    .gap(px(6.))
                    .child(div().min_w_0().truncate().text_color(name_color).group_hover("sftp-row", |s| s.text_color(p.hover_text)).child(entry.name.clone()))
                    .when(locked, |el| {
                        let tip = perm_tip.clone();
                        el.child(
                            div()
                                .id(("lock", ix))
                                .flex_shrink_0()
                                .tooltip(move |window, cx| Tooltip::new(tip.clone()).build(window, cx))
                                .child(icon_el("lock", 12., p.text_faint)),
                        )
                    })
                    .when(entry.broken_link, |el| el.child(div().text_size(px(11.)).text_color(p.text_faint).child("broken link")))
                    .into_any_element()
            };

            let dragged = if selected {
                selection.get_or_insert_with(|| self.selection().into_iter().map(|e| e.path).collect()).clone()
            } else {
                vec![entry.path.clone()]
            };
            let label: SharedString =
                if dragged.len() == 1 { entry.name.clone().into() } else { format!("{} items", dragged.len()).into() };
            let drag = DraggedFiles { source: cx.entity_id(), paths: dragged, label };
            let click_path = path.clone();
            let right_path = path.clone();
            let more_path = path.clone();
            let is_dir = entry.is_dir();
            let drop_dir = entry.path.clone();
            let drop_dir_ext = entry.path.clone();
            let description = {
                let mut text = match (entry.kind, entry.size) {
                    (Kind::Dir, _) => format!("{}, folder", entry.name),
                    (_, Some(size)) => format!("{}, {}", entry.name, format_size(size)),
                    (_, None) => entry.name.clone(),
                };
                if entry.mtime.is_some() {
                    text.push_str(&format!(", modified {}", format_time(entry.mtime)));
                }
                if locked {
                    text.push_str(", no access");
                }
                if entry.broken_link {
                    text.push_str(", broken link");
                }
                text
            };
            let row = div()
                .id(("sftp-row", ix))
                .role(gpui::Role::ListBoxOption)
                .aria_label(description)
                .aria_selected(selected)
                .aria_position_in_set(ix + 1)
                .aria_size_of_set(set_size)
                .when(keyboard_row.as_deref() == Some(entry.path.as_str()), |el| {
                    el.aria_active_descendant().when(ringed, |el| el.shadow(keyboard_ring(p)))
                })
                .group("sftp-row")
                .h(px(ROW_HEIGHT))
                // A list item is as wide as its content unless told otherwise; full width
                // keeps the Size, Modified and Perms columns under their headers.
                .w_full()
                .px(px(10.))
                .flex()
                .items_center()
                .gap(px(8.))
                .rounded(p.corner(6.))
                .text_size(px(13.))
                .when(selected, |el| el.bg(p.active))
                .when(!selected, |el| el.hover(|s| s.bg(p.hover)))
                .on_click(cx.listener(move |this, event: &ClickEvent, window, cx| this.click_row(click_path.clone(), event, window, cx)))
                .on_mouse_down(MouseButton::Right, cx.listener(move |this, _: &MouseDownEvent, window, cx| {
                    this.right_click_row(right_path.clone(), window, cx)
                }))
                .when(!renaming, |el| {
                    el.on_drag(drag, |drag, _, _, cx| cx.new(|_| DragGhost { label: drag.label.clone() }))
                })
                .when(is_dir && !locked, |el| {
                    el.drag_over::<DraggedFiles>(move |s, d, _, _| if d.source == me { s.bg(p.brand_soft) } else { s })
                        .drag_over::<ExternalPaths>(move |s, _, _, _| s.bg(p.brand_soft))
                        .on_drop(cx.listener(move |this, dragged: &DraggedFiles, _, cx| {
                            this.drop_files(dragged, drop_dir.clone(), cx)
                        }))
                        .on_drop(cx.listener(move |this, paths: &ExternalPaths, _, cx| {
                            cx.stop_propagation();
                            this.upload(paths.paths().to_vec(), drop_dir_ext.clone(), cx)
                        }))
                })
                .child(icon_el(file_icon(&entry), 16., if locked { icon_color.opacity(0.55) } else { icon_color }))
                .child(name_cell)
                .child(
                    div()
                        .w(px(70.))
                        .flex_shrink_0()
                        .text_right()
                        .text_size(px(12.))
                        .text_color(p.text_muted).group_hover("sftp-row", |s| s.text_color(p.hover_text))
                        .child(match (entry.kind, entry.size) {
                            (Kind::Dir, _) | (_, None) => "—".to_string(),
                            (_, Some(size)) => format_size(size),
                        }),
                )
                .when(show_modified, |el| {
                    el.child(
                        div()
                            .w(px(118.))
                            .flex_shrink_0()
                            .truncate()
                            .text_size(px(12.))
                            .text_color(p.text_muted).group_hover("sftp-row", |s| s.text_color(p.hover_text))
                            .child(format_time(entry.mtime)),
                    )
                })
                .when(show_perms, |el| {
                    let tip = perm_tip.clone();
                    el.child(
                        div()
                            .id(("perms", ix))
                            .w(px(92.))
                            .flex_shrink_0()
                            .font_family(mono.clone())
                            .text_size(px(11.5))
                            .text_color(if locked { p.text_faint } else { p.text_muted }).group_hover("sftp-row", |s| s.text_color(p.hover_text))
                            .tooltip(move |window, cx| Tooltip::new(tip.clone()).build(window, cx))
                            .child(entry.mode_string()),
                    )
                })
                .child(
                    div()
                        .id(("row-more", ix))
                        .size(px(20.))
                        .flex()
                        .flex_shrink_0()
                        .items_center()
                        .justify_center()
                        .rounded(p.corner(5.))
                        .cursor_pointer()
                        .when(!selected, |el| el.invisible().group_hover("sftp-row", |s| s.visible()))
                        .hover(|s| s.bg(p.active))
                        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                        .on_click(cx.listener(move |this, event: &ClickEvent, window, cx| {
                            cx.stop_propagation();
                            this.right_click_row(more_path.clone(), window, cx);
                            this.open_entry_menu(event.position(), window, cx);
                            cx.notify();
                        }))
                        .child(icon_el("ellipsis-vertical", 14., p.text_muted)),
                );
            let ctx_view = view.clone();
            out.push(
                row.context_menu(move |menu, _, cx| {
                    let Some(browser) = ctx_view.upgrade() else { return menu };
                    let this = browser.read(cx);
                    let targets = this.selection();
                    let favorites = this.favorites.read(cx).data.clone();
                    entry_menu(menu, &ctx_view, targets, this.dir_access.write, &favorites, &this.host_id, this.menu_extras)
                })
                .into_any_element(),
            );
        }
        out
    }

    fn render_parent_row(&self, p: AppPalette, show_modified: bool, show_perms: bool, cx: &mut Context<Self>) -> AnyElement {
        let parent = self.cwd.as_deref().and_then(sftp::parent).unwrap_or_else(|| "/".into());
        let drop_parent = parent.clone();
        let drop_parent_ext = parent.clone();
        let me = cx.entity_id();
        div()
            .id("sftp-parent")
            .role(gpui::Role::ListBoxOption)
            .aria_label("Parent folder")
            .aria_selected(false)
            .aria_position_in_set(1)
            .aria_size_of_set(self.rows.len() + 1)
            .h(px(ROW_HEIGHT))
            .w_full()
            .px(px(10.))
            .flex()
            .items_center()
            .gap(px(8.))
            .rounded(p.corner(6.))
            .text_size(px(13.))
            .text_color(p.text_muted)
            .hover(|s| s.bg(p.hover))
            .drag_over::<DraggedFiles>(move |s, d, _, _| if d.source == me { s.bg(p.brand_soft) } else { s })
            .on_drop(cx.listener(move |this, dragged: &DraggedFiles, _, cx| this.drop_files(dragged, drop_parent.clone(), cx)))
            .on_drop(cx.listener(move |this, paths: &ExternalPaths, _, cx| {
                cx.stop_propagation();
                this.upload(paths.paths().to_vec(), drop_parent_ext.clone(), cx)
            }))
            .on_click(cx.listener(|this, event: &ClickEvent, window, cx| {
                window.focus(&this.focus, cx);
                if event.click_count() >= 2 {
                    this.go_up(cx);
                } else {
                    this.selected.clear();
                    cx.notify();
                }
            }))
            .child(icon_el("folder", 16., p.warning))
            .child(div().flex_1().min_w_0().child(".."))
            .child(div().w(px(70.)).text_right().text_size(px(12.)).text_color(p.text_faint).child("—"))
            .when(show_modified, |el| el.child(div().w(px(118.)).text_size(px(12.)).text_color(p.text_faint).child("—")))
            .when(show_perms, |el| el.child(div().w(px(92.)).text_size(px(12.)).text_color(p.text_faint).child("—")))
            .child(div().w(px(20.)))
            .into_any_element()
    }

    fn render_new_folder_row(&self, p: AppPalette, cx: &mut Context<Self>) -> Option<AnyElement> {
        let edit = self.edit.as_ref().filter(|e| matches!(e.kind, EditKind::NewFolder))?;
        Some(
            div()
                .h(px(ROW_HEIGHT))
                .mx(px(4.))
                .px(px(10.))
                .flex()
                .flex_shrink_0()
                .items_center()
                .gap(px(8.))
                .rounded(p.corner(6.))
                .bg(p.active)
                .capture_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                    if event.keystroke.key == "escape" {
                        this.cancel_edit(window, cx);
                        cx.stop_propagation();
                    }
                }))
                .child(icon_el("folder-plus", 16., p.warning))
                .child(div().flex_1().min_w_0().child(Input::new(&edit.input).xsmall()))
                .into_any_element(),
        )
    }

    /// Instead of the list: not connected, failed, loading, empty.
    fn render_state(&self, p: AppPalette, cx: &mut Context<Self>) -> Option<AnyElement> {
        let message = |icon: &str, color: Hsla, title: String, detail: Option<String>| {
            div()
                .flex()
                .flex_col()
                .items_center()
                .gap(px(8.))
                .max_w(px(360.))
                .px(px(20.))
                // Connecting and loading turn their loader, so the panel visibly works.
                .child(if icon == "loader-circle" { spinner("sftp-state-spinner", 22., color) } else { icon_el(icon, 22., color) })
                .child(div().text_size(px(13.)).font_weight(FontWeight::MEDIUM).text_color(p.text).text_center().child(title))
                .when_some(detail, |el, d| {
                    el.child(div().text_size(px(12.)).line_height(px(17.)).text_color(p.text_muted).text_center().child(d))
                })
        };
        let body = match &self.conn {
            Conn::Waiting(reason) => message("unplug", p.text_faint, "Not connected".into(), Some(reason.clone())).into_any_element(),
            // Both waits share an id, so "Loading…" carries on from "Starting SFTP…".
            Conn::Connecting => after_delay("sftp-state-wait", message("loader-circle", p.text_faint, "Starting SFTP…".into(), None)),
            Conn::Failed(reason) => message("circle-alert", p.danger, "SFTP isn't available".into(), Some(reason.clone()))
                .child(
                    button("sftp-reconnect", "Try again", ButtonKind::Secondary, p)
                        .mt(px(6.))
                        .on_click(cx.listener(|_, _: &ClickEvent, _, cx| cx.emit(BrowserEvent::Retry))),
                )
                .into_any_element(),
            Conn::Ready(_) => {
                if let Some(err) = &self.load_error {
                    let denied = err.kind == ErrorKind::PermissionDenied;
                    let title = if denied { "You can't open this folder" } else { "This folder can't be shown" };
                    let has_parent = self.has_parent();
                    message(if denied { "lock" } else { "circle-alert" }, if denied { p.text_faint } else { p.danger }, title.into(), Some(err.message.clone()))
                        .child(
                            div()
                                .mt(px(6.))
                                .flex()
                                .gap(px(8.))
                                .child(
                                    button("sftp-retry", "Retry", ButtonKind::Secondary, p)
                                        .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.refresh(cx))),
                                )
                                .when(has_parent, |el| {
                                    el.child(
                                        button("sftp-to-parent", "Parent folder", ButtonKind::Ghost, p)
                                            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.go_up(cx))),
                                    )
                                })
                                .child(button("sftp-home", "Home", ButtonKind::Ghost, p).on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                                    if let Some(home) = this.remote().map(|r| r.home.clone()) {
                                        this.navigate(home, cx);
                                    }
                                }))),
                        )
                        .into_any_element()
                } else if self.cwd.is_none() {
                    after_delay("sftp-state-wait", message("loader-circle", p.text_faint, "Loading…".into(), None))
                } else if self.rows.is_empty() && self.edit.is_none() && !self.loading {
                    if self.entries.is_empty() {
                        let hint = (self.dir_access.write != Allowed::No).then(|| "Drop files here to upload them.".into());
                        message("folder-open", p.text_faint, "This folder is empty".into(), hint).into_any_element()
                    } else {
                        message("eye-off", p.text_faint, "Only hidden files here".into(), None)
                            .child(
                                button("sftp-show-hidden", "Show hidden files", ButtonKind::Secondary, p)
                                    .mt(px(6.))
                                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.set_show_hidden(true, cx))),
                            )
                            .into_any_element()
                    }
                } else {
                    return None;
                }
            }
        };
        Some(div().flex_1().min_h_0().flex().items_center().justify_center().pb(px(40.)).child(body).into_any_element())
    }

    fn render_footer(&self, p: AppPalette, cx: &mut Context<Self>) -> AnyElement {
        let total = self.rows.len();
        let hidden = self.entries.len() - total;
        let selected = self.selected.len();
        let mut text = plural(total, "item");
        if hidden > 0 {
            text.push_str(&format!(" ({hidden} hidden)"));
        }
        if selected > 0 {
            let bytes: u64 = self.selection().iter().filter_map(|e| if e.is_dir() { None } else { e.size }).sum();
            text.push_str(&format!(" · {selected} selected"));
            if bytes > 0 {
                text.push_str(&format!(", {}", format_size(bytes)));
            }
        }
        let status = match self.indicator.phase() {
            BusyPhase::Idle => None,
            BusyPhase::Working => Some(
                div()
                    .flex()
                    .items_center()
                    .gap(px(5.))
                    .child(spinner("sftp-footer-spinner", 12., p.brand))
                    .child(self.indicator_label.clone())
                    .into_any_element(),
            ),
            BusyPhase::Done => {
                let color = p.success;
                Some(busy_done("sftp-footer-done", move |opacity| {
                    div().flex().items_center().gap(px(5.)).opacity(opacity).child(icon_el("check", 12., color)).child("Done").into_any_element()
                }))
            }
        };
        let show = self.show_hidden;
        div()
            .h(px(28.))
            .px(px(14.))
            .flex()
            .flex_shrink_0()
            .items_center()
            .gap(px(10.))
            .border_t_1()
            .border_color(p.border)
            .text_size(px(11.5))
            .text_color(p.text_faint)
            .child(
                div()
                    .id("sftp-summary")
                    .role(gpui::Role::Status)
                    .aria_label(text.clone())
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .child(if self.cwd.is_some() { text } else { String::new() }),
            )
            .children(status)
            .child(
                div()
                    .id("sftp-hidden-toggle")
                    .role(gpui::Role::Switch)
                    .aria_label("Show hidden files")
                    .aria_toggled(if show { gpui::Toggled::True } else { gpui::Toggled::False })
                    .aria_keyshortcuts("Ctrl+H")
                    .tab_index(0)
                    .focus_visible(move |s| focus_outline(s, p.focus))
                    .h(px(20.))
                    .px(px(4.))
                    .mr(px(-4.))
                    .rounded(p.corner(5.))
                    .flex()
                    .items_center()
                    .gap(px(5.))
                    .cursor_pointer()
                    .hover(|s| s.text_color(p.text))
                    .tooltip(|window, cx| Tooltip::new("Show or hide files starting with a dot (Ctrl+H)").build(window, cx))
                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| this.set_show_hidden(!show, cx)))
                    .child(icon_el(if show { "eye" } else { "eye-off" }, 12., p.text_faint))
                    .child(if show { "Hidden shown" } else { "Hidden off" }),
            )
            .into_any_element()
    }

    fn render_notice(&self, p: AppPalette, cx: &mut Context<Self>) -> Option<AnyElement> {
        let notice = self.notice.as_ref()?;
        let (fg, bg, icon) = if notice.error {
            (p.danger, p.danger.opacity(0.1), "circle-alert")
        } else {
            (p.success, p.success.opacity(0.1), "circle-check")
        };
        Some(
            div()
                .id("sftp-notice")
                .role(if notice.error { gpui::Role::Alert } else { gpui::Role::Status })
                .aria_label(notice.text.clone())
                .mx(px(10.))
                .mb(px(6.))
                .pl(px(10.))
                .pr(px(4.))
                .py(px(4.))
                .flex()
                .items_start()
                .gap(px(8.))
                .rounded(p.corner(8.))
                .bg(bg)
                .text_size(px(12.))
                .line_height(px(17.))
                .text_color(if notice.error { fg } else { p.text })
                .child(div().pt(px(3.)).child(icon_el(icon, 13., fg)))
                .child(div().flex_1().min_w_0().py(px(2.)).child(notice.text.clone()))
                .child(
                    control("notice-close", "Dismiss", true, p)
                        .size(px(21.))
                        .flex()
                        .flex_shrink_0()
                        .items_center()
                        .justify_center()
                        .rounded(p.corner(5.))
                        .cursor_pointer()
                        .hover(|s| s.bg(p.hover))
                        .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                            this.notice = None;
                            cx.notify();
                        }))
                        .child(icon_el("x", 13., p.text_faint)),
                )
                .into_any_element(),
        )
    }

    fn render_favorites(&self, p: AppPalette, cx: &mut Context<Self>) -> Option<AnyElement> {
        if !self.favorites_open {
            return None;
        }
        let store = &self.favorites.read(cx).data;
        let cwd = self.cwd.clone();
        let host_id = self.host_id.clone();
        let current_shared = cwd.as_deref().is_some_and(|c| store.items.iter().any(|f| f.path == c && f.host.is_none()));
        let current_own = cwd.as_deref().is_some_and(|c| store.items.iter().any(|f| f.path == c && f.host.as_deref() == Some(&host_id)));
        let own: Vec<_> = store.items.iter().filter(|f| f.host.as_deref() == Some(host_id.as_str())).cloned().collect();
        let shared: Vec<_> = store.items.iter().filter(|f| f.host.is_none()).cloned().collect();
        let empty = own.is_empty() && shared.is_empty();

        let fav_row = |f: crate::favorites::Favorite, first: bool, last: bool, cx: &mut Context<Self>| {
            let renaming = self.favorite_rename.as_ref().filter(|(id, _, _)| *id == f.id).map(|(_, input, _)| input.clone());
            let (id1, id2, id3, id4, id5) = (f.id.clone(), f.id.clone(), f.id.clone(), f.id.clone(), f.id.clone());
            let host = host_id.clone();
            let host2 = host_id.clone();
            let host3 = host_id.clone();
            let shared = f.host.is_none();
            let name = f.name.clone();
            let path = f.path.clone();
            let current = cwd.as_deref() == Some(f.path.as_str());
            // For the pointer; the keyboard has F2, Delete and Alt+arrows on the row instead.
            let small = |id: (&'static str, usize), icon: &str, tip: &'static str| {
                div()
                    .id(id)
                    .role(gpui::Role::Button)
                    .aria_label(tip)
                    .size(px(22.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded(p.corner(5.))
                    .cursor_pointer()
                    .hover(|s| s.bg(p.active))
                    .tooltip(move |window, cx| Tooltip::new(tip).build(window, cx))
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .child(icon_el(icon, 12., p.text_muted))
            };
            let key = f.id.bytes().fold(0usize, |a, b| a.wrapping_mul(31).wrapping_add(b as usize));
            let editing = renaming.is_some();
            let (key_id, key_host, key_name) = (f.id.clone(), host_id.clone(), f.name.clone());
            div()
                .id(("fav", key))
                .role(gpui::Role::Button)
                .aria_label(format!("{}, {}{}", f.name, f.path, if current { ", current folder" } else { "" }))
                .aria_description("F2 renames, Delete removes, Alt+Up and Alt+Down move it")
                .when(!editing, |el| {
                    el.tab_index(0).focus_visible(move |s| focus_outline(s, p.focus)).on_key_down(cx.listener(
                        move |this, event: &KeyDownEvent, window, cx| {
                            let (key, mods) = (event.keystroke.key.as_str(), event.keystroke.modifiers);
                            let (id, host) = (key_id.clone(), key_host.clone());
                            match (key, mods.alt, mods.modified()) {
                                ("f2", _, false) => this.start_favorite_rename(id, key_name.clone(), window, cx),
                                ("delete", _, false) => {
                                    this.favorites.update(cx, |s, cx| s.change(|f| f.remove(&id), cx));
                                    // The row is gone: the keyboard stays in the list.
                                    window.focus(&this.favorites_focus, cx);
                                }
                                ("up" | "down", true, _) => {
                                    this.favorites.update(cx, |s, cx| s.change(|f| f.shift(&id, &host, key == "up"), cx));
                                }
                                _ => return,
                            }
                            cx.stop_propagation();
                        },
                    ))
                })
                .group("fav-row")
                .relative()
                .h(px(40.))
                .px(px(8.))
                .flex()
                .items_center()
                .gap(px(9.))
                .rounded(p.corner(7.))
                .cursor_pointer()
                .when(current, |el| el.bg(p.hover))
                .hover(|s| s.bg(p.hover))
                .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| this.open_favorite(path.clone(), window, cx)))
                .child(icon_el(if shared { "globe" } else { "server" }, 13., if current { p.brand } else { p.text_faint }))
                .child(match renaming {
                    Some(input) => div()
                        .id("fav-rename-input")
                        .flex_1()
                        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                        .on_click(|_, _, cx| cx.stop_propagation())
                        .child(Input::new(&input).xsmall())
                        .into_any_element(),
                    None => div()
                        .flex_1()
                        .min_w_0()
                        .flex()
                        .flex_col()
                        .child(div().truncate().text_size(px(12.5)).font_weight(FontWeight::MEDIUM).text_color(p.text).child(f.name.clone()))
                        .child(div().truncate().text_size(px(11.)).text_color(p.text_faint).child(f.path.clone()))
                        .into_any_element(),
                })
                .child(
                    div()
                        .absolute()
                        .right(px(4.))
                        .top(px(9.))
                        .flex()
                        .rounded(p.corner(6.))
                        .bg(p.surface.opacity(1.))
                        .invisible()
                        .group_hover("fav-row", |s| s.visible())
                        .when(!first, |el| {
                            el.child(small(("fav-up", key), "arrow-up", "Move up").on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                                cx.stop_propagation();
                                let (id, host) = (id1.clone(), host.clone());
                                this.favorites.update(cx, |s, cx| s.change(|f| f.shift(&id, &host, true), cx));
                            })))
                        })
                        .when(!last, |el| {
                            el.child(small(("fav-down", key), "arrow-down", "Move down").on_click(cx.listener(
                                move |this, _: &ClickEvent, _, cx| {
                                    cx.stop_propagation();
                                    let (id, host) = (id2.clone(), host2.clone());
                                    this.favorites.update(cx, |s, cx| s.change(|f| f.shift(&id, &host, false), cx));
                                },
                            )))
                        })
                        .child(
                            small(("fav-scope", key), if shared { "server" } else { "globe" }, if shared { "Only for this host" } else { "Share with all hosts" })
                                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                                    cx.stop_propagation();
                                    let id = id3.clone();
                                    let host = if shared { Some(host3.clone()) } else { None };
                                    this.favorites.update(cx, |s, cx| s.change(|f| f.set_host(&id, host.as_deref()), cx));
                                })),
                        )
                        .child(small(("fav-rename", key), "pencil", "Rename").on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                            cx.stop_propagation();
                            this.start_favorite_rename(id4.clone(), name.clone(), window, cx);
                        })))
                        .child(small(("fav-remove", key), "x", "Remove").on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                            cx.stop_propagation();
                            let id = id5.clone();
                            this.favorites.update(cx, |s, cx| s.change(|f| f.remove(&id), cx));
                        }))),
                )
                .into_any_element()
        };

        let mut list: Vec<AnyElement> = vec![];
        let section = |label: String| {
            div().px(px(8.)).pt(px(6.)).pb(px(2.)).text_size(px(10.5)).font_weight(FontWeight::SEMIBOLD).text_color(p.text_faint).child(label.to_uppercase())
        };
        if !own.is_empty() {
            list.push(section(format!("{} only", self.host_label)).into_any_element());
            let n = own.len();
            for (i, f) in own.into_iter().enumerate() {
                list.push(fav_row(f, i == 0, i + 1 == n, cx));
            }
        }
        if !shared.is_empty() {
            list.push(section("All hosts".into()).into_any_element());
            let n = shared.len();
            for (i, f) in shared.into_iter().enumerate() {
                list.push(fav_row(f, i == 0, i + 1 == n, cx));
            }
        }
        let add = |id: &'static str, label: &'static str, icon: &str, host_only: bool, done: bool, cx: &mut Context<Self>| {
            let cwd = cwd.clone();
            let spoken = match (done, host_only) {
                (true, true) => "Already a favorite for this host",
                (true, false) => "Already a favorite for all hosts",
                (false, true) => "Add to favorites for this host",
                (false, false) => "Add to favorites for all hosts",
            };
            control(id, spoken, !done && cwd.is_some(), p)
                .flex_1()
                .h(px(28.))
                .px(px(8.))
                .flex()
                .items_center()
                .justify_center()
                .gap(px(6.))
                .rounded(p.corner(7.))
                .border_1()
                .border_color(p.border)
                .text_size(px(11.5))
                .font_weight(FontWeight::MEDIUM)
                .text_color(if done { p.text_faint } else { p.text })
                .when(!done && cwd.is_some(), |el| {
                    el.cursor_pointer().hover(|s| s.bg(p.hover)).on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                        if let Some(path) = cwd.clone() {
                            this.add_favorite(path, host_only, cx);
                        }
                    }))
                })
                .child(icon_el(if done { "check" } else { icon }, 12., p.text_faint))
                .child(label)
        };
        let width = (self.width - 32.).clamp(240., 340.);
        Some(
            div()
                .id("sftp-favorites-dropdown")
                .track_focus(&self.favorites_focus)
                .role(gpui::Role::Dialog)
                .aria_label("Favorites")
                .absolute()
                .top(px(96.))
                .right(px(16.))
                .w(px(width))
                .occlude()
                .flex()
                .flex_col()
                .rounded(p.corner(10.))
                .bg(p.surface.opacity(1.))
                .border_1()
                .border_color(p.border)
                .on_mouse_down_out(cx.listener(|this, _: &MouseDownEvent, window, cx| {
                    this.favorites_open = false;
                    this.favorite_rename = None;
                    this.favorites_closed_at = Some(std::time::Instant::now());
                    // The click lands where it was aimed; when that takes no focus, the
                    // keyboard goes back to the files rather than staying in the closed list.
                    cx.defer_in(window, |this, window, cx| {
                        if this.favorites_focus.contains_focused(window, cx) {
                            window.focus(&this.focus, cx);
                        }
                    });
                    cx.notify();
                }))
                .child(
                    div()
                        .px(px(12.))
                        .pt(px(10.))
                        .pb(px(4.))
                        .flex()
                        .items_center()
                        .gap(px(6.))
                        .child(icon_el("star", 13., p.warning))
                        .child(div().text_size(px(12.5)).font_weight(FontWeight::SEMIBOLD).text_color(p.text).child("Favorites")),
                )
                .child(
                    div()
                        .id("fav-list")
                        .max_h(px(300.))
                        .overflow_y_scroll()
                        .px(px(4.))
                        .pb(px(4.))
                        .children(list)
                        .when(empty, |el| {
                            el.child(
                                div()
                                    .px(px(8.))
                                    .py(px(10.))
                                    .text_size(px(12.))
                                    .line_height(px(17.))
                                    .text_color(p.text_faint)
                                    .child("No favorites yet. Add folders you visit often, here or from a folder's menu."),
                            )
                        }),
                )
                .child(div().h(px(1.)).bg(p.border))
                .child(
                    div()
                        .p(px(8.))
                        .flex()
                        .flex_col()
                        .gap(px(6.))
                        .child(div().px(px(2.)).text_size(px(11.)).text_color(p.text_faint).truncate().child(format!(
                            "Add {}",
                            cwd.as_deref().unwrap_or("this folder")
                        )))
                        .child(
                            div()
                                .flex()
                                .gap(px(6.))
                                .child(add("fav-add-host", "This host", "server", true, current_own, cx))
                                .child(add("fav-add-all", "All hosts", "globe", false, current_shared, cx)),
                        ),
                )
                .into_any_element(),
        )
    }

    fn render_dialog(&self, p: AppPalette, cx: &mut Context<Self>) -> Option<AnyElement> {
        let dialog = self.dialog.as_ref()?;
        let cancel = button("sftp-dialog-cancel", "Cancel", ButtonKind::Ghost, p).on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
            this.dialog = None;
            window.focus(&this.focus, cx);
            cx.notify();
        }));
        let (title, body, actions): (String, AnyElement, Vec<AnyElement>) = match dialog {
            Dialog::CopyMove { op, paths, input, .. } => {
                let (title, ok) = match op {
                    CopyOp::Copy => (format!("Copy {}", describe(paths)), "Copy"),
                    CopyOp::Move => (format!("Move {}", describe(paths)), "Move"),
                };
                (
                    title,
                    div()
                        .flex()
                        .flex_col()
                        .gap(px(6.))
                        .child(div().text_size(px(12.)).text_color(p.text_muted).child("To folder on this host:"))
                        .child(Input::new(input).small())
                        .into_any_element(),
                    vec![
                        cancel.into_any_element(),
                        button("sftp-dialog-ok", ok, ButtonKind::Primary, p)
                            .on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.submit_copy_move(window, cx)))
                            .into_any_element(),
                    ],
                )
            }
            Dialog::Delete { paths } => {
                let names: Vec<String> = paths.iter().take(4).map(|p| sftp::file_name(p).to_string()).collect();
                let more = paths.len().saturating_sub(4);
                let has_dir = paths.iter().any(|p| self.entry(p).is_some_and(|e| e.is_dir()));
                let paths = paths.clone();
                (
                    format!("Delete {}?", describe(&paths)),
                    div()
                        .flex()
                        .flex_col()
                        .gap(px(6.))
                        .text_size(px(12.5))
                        .line_height(px(18.))
                        .text_color(p.text_muted)
                        .children(names.into_iter().map(|n| div().truncate().text_color(p.text).child(n)))
                        .when(more > 0, |el| el.child(format!("and {more} more")))
                        .child(if has_dir {
                            "Folders are deleted with everything in them. This can't be undone."
                        } else {
                            "This can't be undone."
                        })
                        .into_any_element(),
                    vec![
                        cancel.into_any_element(),
                        button("sftp-dialog-delete", "Delete", ButtonKind::Danger, p)
                            .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                                this.dialog = None;
                                window.focus(&this.focus, cx);
                                this.delete(paths.clone(), cx);
                            }))
                            .into_any_element(),
                    ],
                )
            }
            Dialog::Conflict { uploads, dir, existing } => {
                let (u1, u2, d1, d2, e1) = (uploads.clone(), uploads.clone(), dir.clone(), dir.clone(), existing.clone());
                let names = existing.iter().take(3).map(|n| format!("\"{n}\"")).collect::<Vec<_>>().join(", ");
                let more = existing.len().saturating_sub(3);
                (
                    if existing.len() == 1 { "Replace the existing file?".into() } else { format!("Replace {} existing items?", existing.len()) },
                    div()
                        .text_size(px(12.5))
                        .line_height(px(18.))
                        .text_color(p.text_muted)
                        .child(format!(
                            "{names}{} already exist{} in {dir}. Replacing overwrites {} once the upload is complete.",
                            if more > 0 { format!(" and {more} more") } else { String::new() },
                            if existing.len() == 1 { "s" } else { "" },
                            if existing.len() == 1 { "it" } else { "them" },
                        ))
                        .into_any_element(),
                    vec![
                        cancel.into_any_element(),
                        button("sftp-keep-both", "Keep both", ButtonKind::Secondary, p)
                            .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                                this.dialog = None;
                                window.focus(&this.focus, cx);
                                this.keep_both(u1.clone(), d1.clone(), e1.clone(), cx);
                            }))
                            .into_any_element(),
                        button("sftp-replace", "Replace", ButtonKind::Danger, p)
                            .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                                this.dialog = None;
                                window.focus(&this.focus, cx);
                                this.start_uploads(u2.clone(), d2.clone(), true, None, cx);
                            }))
                            .into_any_element(),
                    ],
                )
            }
        };
        Some(
            div()
                .id("sftp-dialog")
                .absolute()
                .inset_0()
                .occlude()
                .flex()
                .items_start()
                .justify_center()
                .pt(px(110.))
                .px(px(16.))
                .rounded(p.corner(12.))
                .bg(crate::theme::alpha(0x000000, if p.dark { 0.4 } else { 0.15 }))
                .child(
                    div()
                        .id("sftp-dialog-card")
                        .role(gpui::Role::Dialog)
                        .aria_label(title.clone())
                        .w_full()
                        .max_w(px(380.))
                        .p(px(16.))
                        .flex()
                        .flex_col()
                        .gap(px(12.))
                        .rounded(p.corner(12.))
                        .bg(p.surface.opacity(1.))
                        .border_1()
                        .border_color(p.border)
                        .child(div().text_size(px(14.)).font_weight(FontWeight::SEMIBOLD).text_color(p.text).child(title))
                        .child(body)
                        .child(div().flex().justify_end().gap(px(8.)).children(actions)),
                )
                .into_any_element(),
        )
    }
}

/// Where each of `paths` goes when copied or moved into `dest`. A copy in its own folder gets a
/// free name ("app (1).conf", `taken` holds the names there); a move there is nothing to do.
fn copy_targets(op: CopyOp, paths: Vec<String>, dest: &str, taken: &HashSet<String>) -> Vec<(String, String)> {
    let mut pairs = vec![];
    for path in paths {
        let name = sftp::file_name(&path).to_string();
        let same_dir = sftp::parent(&path).as_deref() == Some(dest);
        match (op, same_dir) {
            (CopyOp::Move, true) => continue,
            (CopyOp::Copy, true) => {
                let new = sftp::unique_name(&name, |n| taken.contains(n));
                pairs.push((path, sftp::join(dest, &new)));
            }
            _ => pairs.push((path, sftp::join(dest, &name))),
        }
    }
    pairs
}

/// The shell command that changes to `dir`, quoted for POSIX shells (and fish) when needed.
pub fn cd_command(dir: &str) -> String {
    let plain = !dir.is_empty() && dir.chars().all(|c| c.is_ascii_alphanumeric() || "/._-+@%:,=".contains(c));
    let quoted = if plain { dir.to_string() } else { format!("'{}'", dir.replace('\'', "'\\''")) };
    if dir.starts_with('-') { format!("cd -- {quoted}") } else { format!("cd {quoted}") }
}

/// `path` without empty or `.` segments or a trailing slash: "/srv/app/" is "/srv/app".
/// Keep `..`: only the server knows the parent of a symlink's target.
fn clean_path(path: &str) -> String {
    let mut parts: Vec<&str> = vec![];
    for part in path.split('/') {
        match part {
            "" | "." => {}
            part => parts.push(part),
        }
    }
    let joined = parts.join("/");
    if path.starts_with('/') { format!("/{joined}") } else { joined }
}

/// "app.conf" or "3 items".
fn describe(paths: &[String]) -> String {
    match paths {
        [one] => format!("\"{}\"", sftp::file_name(one)),
        many => format!("{} items", many.len()),
    }
}

/// The menu for files and folders (right-click or ⋯).
fn entry_menu(
    mut menu: PopupMenu,
    view: &WeakEntity<FileBrowser>,
    targets: Vec<Entry>,
    dir_write: Allowed,
    favorites: &Favorites,
    host: &str,
    extras: MenuExtras,
) -> PopupMenu {
    let Some(first) = targets.first().cloned() else { return menu };
    let single = targets.len() == 1;
    let paths: Vec<String> = targets.iter().map(|e| e.path.clone()).collect();
    let readable = targets.iter().any(|e| !e.locked() && e.kind != Kind::Other);
    let can_change = dir_write != Allowed::No;
    let item = |label: String, icon: Option<&str>, enabled: bool, run: Box<dyn Fn(&mut FileBrowser, &mut Window, &mut Context<FileBrowser>)>| {
        popup::item(label, icon, view, move |this, window, cx| run(this, window, cx)).disabled(!enabled)
    };

    if single {
        let e = first.clone();
        menu = menu.item(item("Open".into(), None, !first.locked(), Box::new(move |this, _, cx| this.open_entry(&e, cx))));
    }
    let download_label = if single { "Download".to_string() } else { format!("Download {} items", targets.len()) };
    let t = targets.clone();
    menu = menu.item(item(download_label, Some("download"), readable, Box::new(move |this, _, cx| this.download(t.clone(), false, cx))));
    menu = menu.separator();
    if single && extras.rename {
        let path = first.path.clone();
        menu = menu.item(item("Rename".into(), None, can_change, Box::new(move |this, window, cx| this.start_rename(path.clone(), window, cx))));
    }
    if extras.copy_to {
        let p = paths.clone();
        menu = menu.item(item("Copy to…".into(), None, readable, Box::new(move |this, window, cx| this.ask_copy_move(CopyOp::Copy, p.clone(), window, cx))));
    }
    if extras.move_to {
        let p = paths.clone();
        menu = menu.item(item("Move to…".into(), None, can_change, Box::new(move |this, window, cx| this.ask_copy_move(CopyOp::Move, p.clone(), window, cx))));
    }
    let (p3, p4) = (paths.clone(), paths.clone());
    menu = menu
        .item(item(if single { "Delete".into() } else { format!("Delete {} items", targets.len()) }, Some("trash"), can_change, Box::new(move |this, _, cx| this.ask_delete(p3.clone(), cx))))
        .separator()
        .item(item(if single { "Copy as path".into() } else { "Copy as paths".into() }, None, true, Box::new(move |this, _, cx| this.copy_paths(p4.clone(), cx))));
    if single {
        let e = first.clone();
        menu = menu.item(item("Jump to here".into(), None, !(first.is_dir() && first.locked()), Box::new(move |this, _, cx| this.jump_to(&e, cx))));
        if first.is_dir() {
            let (a, b) = (first.path.clone(), first.path.clone());
            let has_shared = favorites.items.iter().any(|f| f.path == first.path && f.host.is_none());
            let has_own = favorites.items.iter().any(|f| f.path == first.path && f.host.as_deref() == Some(host));
            menu = menu
                .item(item("Add to favorites".into(), Some("star"), !has_shared, Box::new(move |this, _, cx| this.add_favorite(a.clone(), false, cx))))
                .item(item("Add to favorites for this host".into(), None, !has_own, Box::new(move |this, _, cx| this.add_favorite(b.clone(), true, cx))));
        }
        if first.locked() {
            menu = menu.separator().item(PopupMenuItem::label(if first.is_dir() {
                "You don't have permission to open this folder"
            } else {
                "You don't have permission to read this file"
            }));
        }
    }
    if dir_write == Allowed::No {
        menu = menu.item(PopupMenuItem::label("This folder isn't writable for you"));
    }
    menu
}

/// The menu for the folder itself (⋯ in the header, right-click on empty space).
fn folder_menu(menu: PopupMenu, view: &WeakEntity<FileBrowser>, cwd: Option<String>, show_hidden: bool, connected: bool) -> PopupMenu {
    let mut menu = menu
        .item(popup::item("New folder", Some("folder-plus"), view, |this, window, cx| this.start_new_folder(window, cx)).disabled(!connected))
        .item(popup::item("Upload files…", None, view, |this, window, cx| this.pick_uploads(false, window, cx)).disabled(!connected))
        .item(popup::item("Upload folder…", None, view, |this, window, cx| this.pick_uploads(true, window, cx)).disabled(!connected))
        .separator()
        .item(popup::item("Refresh", Some("refresh-cw"), view, |this, _, cx| this.refresh(cx)).disabled(!connected))
        .item(popup::item("Show hidden files", None, view, move |this, _, cx| this.set_show_hidden(!show_hidden, cx)).checked(show_hidden));
    if let Some(cwd) = cwd {
        let (a, b, c) = (cwd.clone(), cwd.clone(), cwd);
        menu = menu
            .separator()
            .item(popup::item("Copy current path", None, view, move |this, _, cx| this.copy_paths(vec![a.clone()], cx)))
            .item(popup::item("Add to favorites", Some("star"), view, move |this, _, cx| this.add_favorite(b.clone(), false, cx)))
            .item(popup::item("Add to favorites for this host", None, view, move |this, _, cx| this.add_favorite(c.clone(), true, cx)));
    }
    menu
}

impl Render for FileBrowser {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        crate::perf::count("file browser render");
        let p = cx.global::<Appearance>().app;
        let state = self.render_state(p, cx);
        let count = self.rows.len() + self.has_parent() as usize;
        let body_view = cx.entity().downgrade();
        let list = state.is_none().then(|| {
            uniform_list("sftp-rows", count, cx.processor(|this, range: Range<usize>, window, cx| this.render_rows(range, window, cx)))
                .track_scroll(&self.scroll)
                .flex_1()
                .min_h_0()
                // The rows' side margin, matching the column header's.
                .px(px(4.))
                .pt(px(4.))
                .pb(px(8.))
                .into_any_element()
        });
        let ready = self.remote().is_some() && self.cwd.is_some();
        // Tabbed into the list before the keyboard is on any row: the list itself wears the ring.
        let on_row = self.cursor.as_ref().or(self.anchor.as_ref()).and_then(|path| self.visible_index(path)).is_some();
        let ring_list = list.is_some() && !on_row && self.keyboard_in_list(window);
        let cwd = self.cwd.clone();
        let show_hidden = self.show_hidden;
        let connected = self.remote().is_some();
        div()
            .id("file-browser")
            .key_context("FileBrowser")
            .track_focus(&self.focus)
            // Tab comes back to the files from the buttons (see `keyboard_ring`).
            .tab_index(0)
            .size_full()
            .relative()
            .flex()
            .flex_col()
            .on_key_down(cx.listener(Self::on_key_down))
            .drag_over::<ExternalPaths>(move |s, _, _, _| s.border_color(p.brand).bg(p.brand_soft.opacity(0.4)))
            .border_1()
            .border_color(gpui::transparent_black())
            .rounded(p.corner(12.))
            .when(ready, |el| {
                el.on_drop(cx.listener(|this, paths: &ExternalPaths, _, cx| {
                    if let Some(cwd) = this.cwd.clone() {
                        this.upload(paths.paths().to_vec(), cwd, cx);
                    }
                }))
            })
            .child(self.render_header(p, cx))
            .child(self.render_toolbar(p, cx))
            .children(self.render_notice(p, cx))
            .when(state.is_none(), |el| el.child(self.render_columns(p, cx)))
            .children(self.render_new_folder_row(p, cx).map(|row| div().pt(px(4.)).child(row)))
            .child(
                div()
                    .id("sftp-body")
                    .when(list.is_some(), |el| {
                        el.role(gpui::Role::ListBox)
                            .aria_label(format!("Files in {}", cwd.as_deref().unwrap_or("this folder")))
                    })
                    .when(ring_list, |el| el.rounded(p.corner(8.)).shadow(keyboard_ring(p)))
                    .flex_1()
                    .min_h_0()
                    .flex()
                    .flex_col()
                    .on_mouse_down(MouseButton::Left, cx.listener(|this, _: &MouseDownEvent, window, cx| {
                        if this.edit.is_none() {
                            window.focus(&this.focus, cx);
                        }
                    }))
                    .children(state)
                    .children(list)
                    .context_menu(move |menu, _, _| folder_menu(menu, &body_view, cwd.clone(), show_hidden, connected)),
            )
            .child(self.render_footer(p, cx))
            .children(self.render_favorites(p, cx))
            .children(self.render_dialog(p, cx))
            .children(self.popup.render())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cd_command_quotes_only_when_needed() {
        assert_eq!(cd_command("/var/log"), "cd /var/log");
        assert_eq!(cd_command("/home/me/my files"), "cd '/home/me/my files'");
        assert_eq!(cd_command("/tmp/it's"), "cd '/tmp/it'\\''s'");
        assert_eq!(cd_command("/tmp/$(rm -rf x)"), "cd '/tmp/$(rm -rf x)'");
        assert_eq!(cd_command("-odd"), "cd -- -odd");
    }

    #[test]
    fn tool_buttons_are_named_without_their_shortcut() {
        assert_eq!(split_shortcut("Refresh (F5)"), ("Refresh", Some("F5")));
        assert_eq!(split_shortcut("Close file browser (Ctrl+Shift+E)"), ("Close file browser", Some("Ctrl+Shift+E")));
        assert_eq!(split_shortcut("More"), ("More", None));
        assert_eq!(split_shortcut("Size (bytes) of it"), ("Size (bytes) of it", None));
    }

    #[test]
    fn natural_order() {
        let mut names = vec!["file10", "File2", "file1", "alpha", "Beta"];
        names.sort_by(|a, b| natural_cmp(a, b));
        assert_eq!(names, ["alpha", "Beta", "file1", "File2", "file10"]);
    }

    #[test]
    fn permission_text_names_the_user() {
        let me = Identity::parse_id("uid=1000(codex) gid=1000(codex) groups=1000(codex),33(www-data)");
        let mut attrs = russh_sftp::protocol::FileAttributes::empty();
        attrs.permissions = Some(0o040750);
        attrs.uid = Some(0);
        attrs.gid = Some(4);
        let entry = sftp_entry("storage", &attrs, &me);
        let text = permission_text(&entry, &me);
        assert!(text.starts_with("drwxr-x--- · owner root · group gid 4"), "{text}");
        assert!(text.contains("You can't open this folder."), "{text}");
        attrs.gid = Some(33);
        let entry = sftp_entry("storage", &attrs, &me);
        let text = permission_text(&entry, &me);
        assert!(text.contains("group www-data (yours)") && !text.contains("can't"), "{text}");
    }

    fn sftp_entry(name: &str, attrs: &russh_sftp::protocol::FileAttributes, me: &Identity) -> Entry {
        // Entry::from_attrs is private to sftp; build the same through a listing-like path.
        let access = sftp::access(me, attrs.permissions, attrs.uid, attrs.gid, true);
        Entry {
            name: name.into(),
            path: format!("/{name}"),
            kind: Kind::Dir,
            link: false,
            broken_link: false,
            size: None,
            mtime: None,
            mode: attrs.permissions,
            uid: attrs.uid,
            gid: attrs.gid,
            access,
        }
    }

    #[test]
    fn rows_dragged_from_another_browser_are_not_moved() {
        let (here, other) = (EntityId::from(1u64), EntityId::from(2u64));
        let dragged = DraggedFiles { source: other, paths: vec!["/etc/nginx/nginx.conf".into()], label: "nginx.conf".into() };
        assert_eq!(dragged.paths_for(here), None);
        assert_eq!(dragged.paths_for(other), Some(&["/etc/nginx/nginx.conf".to_string()][..]));
    }

    #[test]
    fn keep_both_into_another_folder_avoids_the_names_there() {
        // Dropped on the folder "logs" while /srv is on screen; "app.log" is already in logs.
        let shown = [sftp_entry("logs", &russh_sftp::protocol::FileAttributes::empty(), &Identity::default())];
        let taken = names_in("/srv/logs", Some("/srv"), &shown, &["app.log".to_string()]);
        let uploads = [PathBuf::from("C:/temp/app.log"), PathBuf::from("C:/temp/other.log")];
        assert_eq!(upload_names(&uploads, Some(taken)), ["app (1).log", "other.log"]);
        // In the folder on screen, everything listed there counts.
        let taken = names_in("/logs", Some("/logs"), &shown, &[]);
        assert_eq!(upload_names(&[PathBuf::from("logs")], Some(taken)), ["logs (1)"]);
        // Replace or no conflict: names stay as they are.
        assert_eq!(upload_names(&uploads, None), ["app.log", "other.log"]);
    }

    #[test]
    fn copy_or_move_to_its_own_folder_typed_with_a_slash() {
        let taken = HashSet::from(["app.conf".to_string()]);
        let paths = || vec!["/srv/app/app.conf".to_string()];
        for typed in ["/srv/app/", "/srv/app/.", "/srv//app"] {
            let dest = clean_path(typed);
            assert_eq!(dest, "/srv/app", "{typed}");
            assert_eq!(copy_targets(CopyOp::Copy, paths(), &dest, &taken), [("/srv/app/app.conf".to_string(), "/srv/app/app (1).conf".to_string())]);
            assert!(copy_targets(CopyOp::Move, paths(), &dest, &taken).is_empty(), "{typed}");
        }
        assert_eq!(clean_path("/"), "/");
        assert_eq!(clean_path("/.."), "/..");
        assert_eq!(clean_path("/var/log/"), "/var/log");
    }

    #[test]
    fn remote_parent_paths_are_left_for_the_server_to_resolve() {
        // /srv/link may point to /data/app: its parent is /data, not /srv.
        assert_eq!(clean_path("/srv/link/../backup/"), "/srv/link/../backup");
        assert_eq!(clean_path("/srv/link/./../backup"), "/srv/link/../backup");
    }

    #[test]
    fn errors_explain_themselves() {
        let err = OpError::new(ErrorKind::PermissionDenied, "Permission denied (EACCES): /etc/shadow");
        assert_eq!(
            explain("delete shadow", &err),
            "Couldn't delete shadow: Permission denied (EACCES): /etc/shadow. You don't have permission for this on the server."
        );
    }
}
