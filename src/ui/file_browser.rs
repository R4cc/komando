//! The SFTP file browser: a panel beside an SSH Session's terminal, browsing that Host over
//! the Session's own connection.
//!
//! Each Session has its own browser, which keeps its folder, history and selection while it is
//! closed. Transfers go to the shared [`Transfers`] list (the sidebar's Downloads entry).

use std::collections::HashSet;
use std::ops::Range;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, App, ClickEvent, Context, Entity, EventEmitter, ExternalPaths, FocusHandle, Focusable, FontWeight, Hsla,
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
use crate::ui::widgets::{ButtonKind, button, icon_el};

const ROW_HEIGHT: f32 = 30.;

/// Favorites, shared by every browser and saved on each change.
pub struct FavoriteStore {
    pub data: Favorites,
    dir: PathBuf,
}

impl FavoriteStore {
    pub fn new(dir: PathBuf) -> Self {
        Self { data: Favorites::load(&dir), dir }
    }

    pub fn change(&mut self, f: impl FnOnce(&mut Favorites), cx: &mut Context<Self>) {
        f(&mut self.data);
        if let Err(err) = self.data.save(&self.dir) {
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
    paths: Vec<String>,
    label: SharedString,
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
            .rounded(px(7.))
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
    selected: HashSet<String>,
    anchor: Option<String>,
    sort: (SortKey, bool),
    show_hidden: bool,
    path_input: Entity<InputState>,
    editing_path: bool,
    edit: Option<Edit>,
    favorites_open: bool,
    favorite_rename: Option<(String, Entity<InputState>, Subscription)>,
    dialog: Option<Dialog>,
    popup: Popup,
    scroll: UniformListScrollHandle,
    focus: FocusHandle,
    /// The panel's width, set by the workspace; narrow panels drop columns.
    pub width: f32,
    /// The path to select once the next listing arrives.
    select_after_load: Option<String>,
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
            selected: HashSet::new(),
            anchor: None,
            sort: (SortKey::Name, true),
            show_hidden,
            path_input,
            editing_path: false,
            edit: None,
            favorites_open: false,
            favorite_rename: None,
            dialog: None,
            popup: Popup::default(),
            scroll: UniformListScrollHandle::new(),
            focus: cx.focus_handle(),
            width: 520.,
            select_after_load: None,
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

    /// Open SFTP on the Session's connection.
    pub fn connect(&mut self, sink: Option<InputSink>, cx: &mut Context<Self>) {
        let Some(answer) = sink.and_then(|s| s.open_sftp()) else {
            self.conn = Conn::Waiting("The session isn't connected. Reconnect it to browse files.".into());
            cx.notify();
            return;
        };
        self.conn = Conn::Connecting;
        cx.spawn(async move |this, cx| {
            let result = answer.await;
            let _ = this.update(cx, |this, cx| {
                match result {
                    Ok(Ok(remote)) => {
                        let start = this.cwd.clone().unwrap_or_else(|| remote.home.clone());
                        this.conn = Conn::Ready(Arc::new(remote));
                        this.load(start, History::Stay, cx);
                    }
                    Ok(Err(err)) => this.conn = Conn::Failed(format!("{err:#}")),
                    Err(_) => this.conn = Conn::Waiting("The session ended. Reconnect it to browse files.".into()),
                }
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
        cx.notify();
    }

    // ---- navigation ---------------------------------------------------------------

    fn identity(&self) -> Identity {
        self.remote().map(|r| r.identity.clone()).unwrap_or_default()
    }

    /// `~` and relative paths are taken from the home folder and the current folder.
    fn expand(&self, text: &str) -> String {
        let home = self.remote().map(|r| r.home.clone()).unwrap_or_else(|| "/".into());
        if text == "~" {
            home
        } else if let Some(rest) = text.strip_prefix("~/") {
            sftp::join(&home, rest)
        } else if text.starts_with('/') {
            text.to_string()
        } else {
            sftp::join(self.cwd.as_deref().unwrap_or(&home), text)
        }
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
        if self.cwd.as_deref() == Some(dir) && !self.loading {
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
                    self.edit = None;
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
                }
                self.update_rows();
                self.scroll_to_selection();
            }
            Err(err) => {
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
            let entries = &self.entries;
            self.selected.retain(|p| entries.iter().any(|e| &e.path == p && !e.is_hidden()));
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
        if let Some(path) = &self.anchor
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
        } else if mods.control || mods.platform {
            if !self.selected.remove(&path) {
                self.selected.insert(path.clone());
            }
            self.anchor = Some(path);
        } else {
            self.selected = HashSet::from([path.clone()]);
            self.anchor = Some(path);
        }
        cx.notify();
    }

    /// Right-click: a row outside the selection becomes the selection.
    fn right_click_row(&mut self, path: String, window: &mut Window, cx: &mut Context<Self>) {
        window.focus(&self.focus, cx);
        if !self.selected.contains(&path) {
            self.selected = HashSet::from([path.clone()]);
            self.anchor = Some(path);
            cx.notify();
        }
    }

    fn move_cursor(&mut self, delta: isize, extend: bool, cx: &mut Context<Self>) {
        if self.rows.is_empty() {
            return;
        }
        let current = self.anchor.as_deref().and_then(|p| self.visible_index(p));
        let next = match current {
            Some(i) => (i as isize + delta).clamp(0, self.rows.len() as isize - 1) as usize,
            None if delta < 0 => self.rows.len() - 1,
            None => 0,
        };
        let path = self.entries[self.rows[next]].path.clone();
        if !extend {
            self.selected.clear();
        }
        self.selected.insert(path.clone());
        self.anchor = Some(path);
        self.scroll_to_selection();
        cx.notify();
    }

    fn select_all(&mut self, cx: &mut Context<Self>) {
        self.selected = self.rows.iter().map(|&i| self.entries[i].path.clone()).collect();
        cx.notify();
    }

    // ---- notices ------------------------------------------------------------------

    fn notify_error(&mut self, text: impl Into<SharedString>, cx: &mut Context<Self>) {
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
        let task = sftp::spawn(op(remote));
        cx.spawn(async move |this, cx| {
            let result = task.await.and_then(|r| r);
            let _ = this.update(cx, |this, cx| {
                this.busy = None;
                done(this, result, cx);
                this.refresh(cx);
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
            let mut taken = vec![];
            for name in names {
                if remote.exists(&sftp::join(&check_dir, &name)).await? {
                    taken.push(name);
                }
            }
            Ok::<_, OpError>(taken)
        });
        cx.spawn(async move |this, cx| {
            let result = task.await.and_then(|r| r);
            let _ = this.update(cx, |this, cx| match result {
                Ok(existing) if existing.is_empty() => this.start_uploads(paths, dir, false, false, cx),
                Ok(existing) => {
                    this.dialog = Some(Dialog::Conflict { uploads: paths, dir, existing });
                    cx.notify();
                }
                Err(err) => this.notify_error(format!("Can't upload to {dir}: {}", err.message), cx),
            });
        })
        .detach();
    }

    /// `replace` replaces files of the same name; `keep_both` gives the new ones free names.
    fn start_uploads(&mut self, paths: Vec<PathBuf>, dir: String, replace: bool, keep_both: bool, cx: &mut Context<Self>) {
        let Some(remote) = self.remote() else { return };
        let source = cx.entity().downgrade();
        let host = self.host_label.clone();
        let mut taken: HashSet<String> =
            if self.cwd.as_deref() == Some(dir.as_str()) { self.entries.iter().map(|e| e.name.clone()).collect() } else { HashSet::new() };
        self.transfers.update(cx, |t, cx| {
            for path in paths {
                let mut name = upload_name(&path);
                if keep_both {
                    name = sftp::unique_name(&name, |n| taken.contains(n));
                }
                taken.insert(name.clone());
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
        let name = entry.name.clone();
        self.selected = HashSet::from([path.clone()]);
        self.anchor = Some(path.clone());
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
            // Select the name without its extension, as file managers do.
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
        let name = edit.input.read(cx).value().trim().to_string();
        cx.notify();
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
        let mut pairs = vec![];
        for path in paths {
            let name = sftp::file_name(&path).to_string();
            let same_dir = sftp::parent(&path).as_deref() == Some(dest.as_str());
            match (op, same_dir) {
                (CopyOp::Move, true) => continue,
                // A copy in the same folder gets a free name: "app (1).conf".
                (CopyOp::Copy, true) => {
                    let new = sftp::unique_name(&name, |n| taken.contains(n));
                    pairs.push((path, sftp::join(&dest, &new)));
                }
                _ => pairs.push((path, sftp::join(&dest, &name))),
            }
        }
        if pairs.is_empty() {
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
        self.run_op(
            "Moving…",
            move |r| {
                Box::pin(async move {
                    for (from, to) in pairs {
                        r.rename(&from, &to).await.map_err(|e| (sftp::file_name(&from).to_string(), e)).map_err(|(n, e)| {
                            OpError::new(e.kind, explain(&format!("move {n}"), &e))
                        })?;
                    }
                    Ok(())
                })
            },
            cx,
            |this, result, cx| {
                if let Err(err) = result {
                    this.notify_error(err.message, cx);
                }
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

    fn jump_to(&mut self, entry: &Entry, cx: &mut Context<Self>) {
        if entry.is_dir() {
            self.navigate(entry.path.clone(), cx);
        } else {
            self.reveal(&entry.path, cx);
        }
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

    fn open_favorite(&mut self, path: String, cx: &mut Context<Self>) {
        self.favorites_open = false;
        self.favorite_rename = None;
        self.navigate(path, cx);
    }

    fn start_favorite_rename(&mut self, id: String, name: String, window: &mut Window, cx: &mut Context<Self>) {
        let input = cx.new(|cx| InputState::new(window, cx).default_value(name));
        let fav_id = id.clone();
        let subscription = cx.subscribe_in(&input, window, move |this, input, event: &InputEvent, _, cx| match event {
            InputEvent::PressEnter { .. } | InputEvent::Blur => {
                let name = input.read(cx).value().to_string();
                let id = fav_id.clone();
                this.favorite_rename = None;
                this.favorites.update(cx, |store, cx| store.change(|f| f.rename(&id, &name), cx));
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
            } else if self.favorites_open {
                self.favorites_open = false;
                self.favorite_rename = None;
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
        if !self.focus.is_focused(window) || self.dialog.is_some() {
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
            _ => false,
        };
        if handled {
            cx.stop_propagation();
        }
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

fn plural(n: usize, what: &str) -> String {
    if n == 1 { format!("1 {what}") } else { format!("{n} {what}s") }
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

fn tool_button(id: impl Into<gpui::ElementId>, icon: &str, tip: &'static str, enabled: bool, p: AppPalette) -> gpui::Stateful<gpui::Div> {
    div()
        .id(id)
        .size(px(28.))
        .flex()
        .flex_shrink_0()
        .items_center()
        .justify_center()
        .rounded(px(7.))
        .tooltip(move |window, cx| Tooltip::new(tip).build(window, cx))
        .when(enabled, |el| el.cursor_pointer().hover(move |s| s.bg(p.hover)))
        .child(icon_el(icon, 15., if enabled { p.text_muted } else { p.text_faint.opacity(0.5) }))
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
    let fg = if enabled { p.text } else { p.text_faint };
    div()
        .id(id)
        .h(px(30.))
        .px(px(if show_label { 10. } else { 7. }))
        .flex()
        .flex_shrink_0()
        .items_center()
        .gap(px(6.))
        .rounded(px(8.))
        .border_1()
        .border_color(p.border)
        .bg(p.hover)
        .text_size(px(12.5))
        .font_weight(FontWeight::MEDIUM)
        .text_color(fg)
        .when(enabled, |el| el.cursor_pointer().hover(move |s| s.bg(p.active)))
        .when(!show_label, |el| el.tooltip(move |window, cx| Tooltip::new(label).build(window, cx)))
        .child(icon_el(icon, 14., if enabled { p.text_muted } else { p.text_faint }))
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
            .child(
                div()
                    .size(px(30.))
                    .flex()
                    .flex_shrink_0()
                    .items_center()
                    .justify_center()
                    .rounded(px(8.))
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
                            .child(div().size(px(7.)).flex_shrink_0().rounded_full().bg(dot)),
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
                    div()
                        .id(("crumb", i))
                        .px(px(4.))
                        .h(px(22.))
                        .flex()
                        .flex_shrink_0()
                        .items_center()
                        .rounded(px(5.))
                        .cursor_pointer()
                        .text_color(if last { p.text } else { p.text_muted })
                        .hover(|s| s.bg(p.active).text_color(p.text))
                        .drag_over::<DraggedFiles>(move |s, _, _, _| s.bg(p.brand_soft))
                        .on_drop(cx.listener({
                            let target = target.clone();
                            move |this, dragged: &DraggedFiles, _, cx| this.move_into(dragged.paths.clone(), target.clone(), cx)
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
                .rounded(px(8.))
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
            .child(
                div()
                    .flex()
                    .flex_shrink_0()
                    .rounded(px(8.))
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
                div()
                    .id("sftp-favorites")
                    .size(px(30.))
                    .flex()
                    .flex_shrink_0()
                    .items_center()
                    .justify_center()
                    .rounded(px(8.))
                    .border_1()
                    .border_color(if self.favorites_open { p.brand.opacity(0.6) } else { p.border })
                    .when(self.favorites_open, |el| el.bg(p.active))
                    .when(connected, |el| el.cursor_pointer().hover(|s| s.bg(p.active)))
                    .tooltip(move |window, cx| {
                        Tooltip::new(if is_favorite { "Favorites (this folder is one)" } else { "Favorites" }).build(window, cx)
                    })
                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                        if connected {
                            this.favorites_open = !this.favorites_open;
                            this.favorite_rename = None;
                            cx.notify();
                        }
                    }))
                    .child(icon_el("star", 15., if is_favorite { p.warning } else { p.text_muted })),
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
                            menu.item(popup::item("Upload files…", "file-up", &view, |this, window, cx| this.pick_uploads(false, window, cx)))
                                .item(popup::item("Upload folder…", "folder-up", &view, |this, window, cx| {
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
            div()
                .id(id)
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

    fn render_rows(&mut self, range: Range<usize>, _window: &mut Window, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let p = cx.global::<Appearance>().app;
        let mono: SharedString = cx.global::<Appearance>().font_family.clone();
        let (show_modified, show_perms) = self.columns();
        let has_parent = self.has_parent();
        let identity = self.identity();
        let view = cx.entity().downgrade();
        let mut out = vec![];
        for ix in range {
            if has_parent && ix == 0 {
                out.push(self.render_parent_row(p, show_modified, show_perms, cx));
                continue;
            }
            let Some(&entry_ix) = self.rows.get(ix - has_parent as usize) else { continue };
            let entry = self.entries[entry_ix].clone();
            let selected = self.selected.contains(&entry.path);
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
                    .child(div().min_w_0().truncate().text_color(name_color).child(entry.name.clone()))
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
                self.selection().into_iter().map(|e| e.path).collect::<Vec<_>>()
            } else {
                vec![entry.path.clone()]
            };
            let label: SharedString =
                if dragged.len() == 1 { entry.name.clone().into() } else { format!("{} items", dragged.len()).into() };
            let drag = DraggedFiles { paths: dragged, label };
            let menu_view = view.clone();
            let click_path = path.clone();
            let right_path = path.clone();
            let more_path = path.clone();
            let is_dir = entry.is_dir();
            let drop_dir = entry.path.clone();
            let drop_dir_ext = entry.path.clone();
            let row = div()
                .id(("sftp-row", ix))
                .group("sftp-row")
                .h(px(ROW_HEIGHT))
                // A list item is as wide as its content unless told otherwise; full width
                // keeps the Size, Modified and Perms columns under their headers.
                .w_full()
                .px(px(10.))
                .flex()
                .items_center()
                .gap(px(8.))
                .rounded(px(6.))
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
                    el.drag_over::<DraggedFiles>(move |s, _, _, _| s.bg(p.brand_soft))
                        .drag_over::<ExternalPaths>(move |s, _, _, _| s.bg(p.brand_soft))
                        .on_drop(cx.listener(move |this, dragged: &DraggedFiles, _, cx| {
                            this.move_into(dragged.paths.clone(), drop_dir.clone(), cx)
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
                        .text_color(p.text_muted)
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
                            .text_color(p.text_muted)
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
                            .text_color(if locked { p.text_faint } else { p.text_muted })
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
                        .rounded(px(5.))
                        .cursor_pointer()
                        .when(!selected, |el| el.invisible().group_hover("sftp-row", |s| s.visible()))
                        .hover(|s| s.bg(p.active))
                        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                        .on_click(cx.listener(move |this, event: &ClickEvent, window, cx| {
                            cx.stop_propagation();
                            this.right_click_row(more_path.clone(), window, cx);
                            let view = menu_view.clone();
                            let targets = this.selection();
                            let dir_write = this.dir_access.write;
                            let favorites = this.favorites.read(cx).data.clone();
                            let host = this.host_id.clone();
                            this.popup = Popup::open(event.position(), |this: &mut Self| &mut this.popup, window, cx, move |menu, _, _| {
                                entry_menu(menu, &view, targets, dir_write, &favorites, &host)
                            });
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
                    entry_menu(menu, &ctx_view, targets, this.dir_access.write, &favorites, &this.host_id)
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
        div()
            .id("sftp-parent")
            .h(px(ROW_HEIGHT))
            .w_full()
            .px(px(10.))
            .flex()
            .items_center()
            .gap(px(8.))
            .rounded(px(6.))
            .text_size(px(13.))
            .text_color(p.text_muted)
            .hover(|s| s.bg(p.hover))
            .drag_over::<DraggedFiles>(move |s, _, _, _| s.bg(p.brand_soft))
            .on_drop(cx.listener(move |this, dragged: &DraggedFiles, _, cx| this.move_into(dragged.paths.clone(), drop_parent.clone(), cx)))
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
                .rounded(px(6.))
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
                .child(icon_el(icon, 22., color))
                .child(div().text_size(px(13.)).font_weight(FontWeight::MEDIUM).text_color(p.text).text_center().child(title))
                .when_some(detail, |el, d| {
                    el.child(div().text_size(px(12.)).line_height(px(17.)).text_color(p.text_muted).text_center().child(d))
                })
        };
        let body = match &self.conn {
            Conn::Waiting(reason) => message("unplug", p.text_faint, "Not connected".into(), Some(reason.clone())).into_any_element(),
            Conn::Connecting => message("loader-circle", p.text_faint, "Starting SFTP…".into(), None).into_any_element(),
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
                    message("loader-circle", p.text_faint, "Loading…".into(), None).into_any_element()
                } else if self.rows.is_empty() && self.edit.is_none() && !self.loading {
                    if self.entries.is_empty() {
                        message("folder-open", p.text_faint, "This folder is empty".into(), Some("Drop files here to upload them.".into()))
                            .into_any_element()
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
        let status: Option<SharedString> = self.busy.clone().or_else(|| self.loading.then(|| "Loading…".into()));
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
            .child(div().flex_1().min_w_0().truncate().child(if self.cwd.is_some() { text } else { String::new() }))
            .when_some(status, |el, status| {
                el.child(div().flex().items_center().gap(px(5.)).child(icon_el("loader-circle", 12., p.brand)).child(status))
            })
            .child(
                div()
                    .id("sftp-hidden-toggle")
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
                .mx(px(10.))
                .mb(px(6.))
                .px(px(10.))
                .py(px(6.))
                .flex()
                .items_start()
                .gap(px(8.))
                .rounded(px(8.))
                .bg(bg)
                .text_size(px(12.))
                .line_height(px(17.))
                .text_color(if notice.error { fg } else { p.text })
                .child(div().pt(px(1.)).child(icon_el(icon, 13., fg)))
                .child(div().flex_1().min_w_0().child(notice.text.clone()))
                .child(
                    div()
                        .id("notice-close")
                        .flex_shrink_0()
                        .cursor_pointer()
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
            let small = |id: (&'static str, usize), icon: &str, tip: &'static str| {
                div()
                    .id(id)
                    .size(px(22.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded(px(5.))
                    .cursor_pointer()
                    .hover(|s| s.bg(p.active))
                    .tooltip(move |window, cx| Tooltip::new(tip).build(window, cx))
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .child(icon_el(icon, 12., p.text_muted))
            };
            let key = f.id.bytes().fold(0usize, |a, b| a.wrapping_mul(31).wrapping_add(b as usize));
            div()
                .id(("fav", key))
                .group("fav-row")
                .relative()
                .h(px(40.))
                .px(px(8.))
                .flex()
                .items_center()
                .gap(px(9.))
                .rounded(px(7.))
                .cursor_pointer()
                .when(current, |el| el.bg(p.hover))
                .hover(|s| s.bg(p.hover))
                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| this.open_favorite(path.clone(), cx)))
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
                        .rounded(px(6.))
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
            div()
                .id(id)
                .flex_1()
                .h(px(28.))
                .px(px(8.))
                .flex()
                .items_center()
                .justify_center()
                .gap(px(6.))
                .rounded(px(7.))
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
                .absolute()
                .top(px(96.))
                .right(px(16.))
                .w(px(width))
                .occlude()
                .flex()
                .flex_col()
                .rounded(px(10.))
                .bg(p.surface.opacity(1.))
                .border_1()
                .border_color(p.border)
                .shadow(crate::ui::widgets::elevation(p.dark))
                .on_mouse_down_out(cx.listener(|this, _: &MouseDownEvent, _, cx| {
                    this.favorites_open = false;
                    this.favorite_rename = None;
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
                let (u1, u2, d1, d2) = (uploads.clone(), uploads.clone(), dir.clone(), dir.clone());
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
                                this.start_uploads(u1.clone(), d1.clone(), false, true, cx);
                            }))
                            .into_any_element(),
                        button("sftp-replace", "Replace", ButtonKind::Danger, p)
                            .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                                this.dialog = None;
                                window.focus(&this.focus, cx);
                                this.start_uploads(u2.clone(), d2.clone(), true, false, cx);
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
                .rounded(px(12.))
                .bg(crate::theme::alpha(0x000000, if p.dark { 0.4 } else { 0.15 }))
                .child(
                    div()
                        .w_full()
                        .max_w(px(380.))
                        .p(px(16.))
                        .flex()
                        .flex_col()
                        .gap(px(12.))
                        .rounded(px(12.))
                        .bg(p.surface.opacity(1.))
                        .border_1()
                        .border_color(p.border)
                        .shadow(crate::ui::widgets::elevation(p.dark))
                        .child(div().text_size(px(14.)).font_weight(FontWeight::SEMIBOLD).text_color(p.text).child(title))
                        .child(body)
                        .child(div().flex().justify_end().gap(px(8.)).children(actions)),
                )
                .into_any_element(),
        )
    }
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
) -> PopupMenu {
    let Some(first) = targets.first().cloned() else { return menu };
    let single = targets.len() == 1;
    let paths: Vec<String> = targets.iter().map(|e| e.path.clone()).collect();
    let readable = targets.iter().any(|e| !e.locked() && e.kind != Kind::Other);
    let can_change = dir_write != Allowed::No;
    let item = |label: String, icon: &str, enabled: bool, run: Box<dyn Fn(&mut FileBrowser, &mut Window, &mut Context<FileBrowser>)>| {
        popup::item(label, icon, view, move |this, window, cx| run(this, window, cx)).disabled(!enabled)
    };

    if single {
        let e = first.clone();
        if first.is_dir() {
            menu = menu.item(item("Open".into(), "folder-open", !first.locked(), Box::new(move |this, _, cx| this.open_entry(&e, cx))));
        } else {
            menu = menu.item(item("Open".into(), "external-link", !first.locked(), Box::new(move |this, _, cx| this.open_entry(&e, cx))));
        }
    }
    let download_label = if single { "Download".to_string() } else { format!("Download {} items", targets.len()) };
    let t = targets.clone();
    menu = menu.item(item(download_label, "download", readable, Box::new(move |this, _, cx| this.download(t.clone(), false, cx))));
    menu = menu.separator();
    if single {
        let path = first.path.clone();
        menu = menu.item(item("Rename".into(), "pencil", can_change, Box::new(move |this, window, cx| this.start_rename(path.clone(), window, cx))));
    }
    let (p1, p2, p3, p4) = (paths.clone(), paths.clone(), paths.clone(), paths.clone());
    menu = menu
        .item(item("Copy to…".into(), "copy-plus", readable, Box::new(move |this, window, cx| this.ask_copy_move(CopyOp::Copy, p1.clone(), window, cx))))
        .item(item("Move to…".into(), "folder-input", can_change, Box::new(move |this, window, cx| this.ask_copy_move(CopyOp::Move, p2.clone(), window, cx))))
        .item(item(if single { "Delete".into() } else { format!("Delete {} items", targets.len()) }, "trash", can_change, Box::new(move |this, _, cx| this.ask_delete(p3.clone(), cx))))
        .separator()
        .item(item(if single { "Copy full path".into() } else { "Copy full paths".into() }, "clipboard-copy", true, Box::new(move |this, _, cx| this.copy_paths(p4.clone(), cx))));
    if single {
        let e = first.clone();
        menu = menu.item(item("Jump to here".into(), "corner-down-right", !(first.is_dir() && first.locked()), Box::new(move |this, _, cx| this.jump_to(&e, cx))));
        if first.is_dir() {
            let (a, b) = (first.path.clone(), first.path.clone());
            let has_shared = favorites.items.iter().any(|f| f.path == first.path && f.host.is_none());
            let has_own = favorites.items.iter().any(|f| f.path == first.path && f.host.as_deref() == Some(host));
            menu = menu
                .item(item("Add to favorites".into(), "star", !has_shared, Box::new(move |this, _, cx| this.add_favorite(a.clone(), false, cx))))
                .item(item("Add to favorites for this host".into(), "server", !has_own, Box::new(move |this, _, cx| this.add_favorite(b.clone(), true, cx))));
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
        .item(popup::item("New folder", "folder-plus", view, |this, window, cx| this.start_new_folder(window, cx)).disabled(!connected))
        .item(popup::item("Upload files…", "file-up", view, |this, window, cx| this.pick_uploads(false, window, cx)).disabled(!connected))
        .item(popup::item("Upload folder…", "folder-up", view, |this, window, cx| this.pick_uploads(true, window, cx)).disabled(!connected))
        .separator()
        .item(popup::item("Refresh", "refresh-cw", view, |this, _, cx| this.refresh(cx)).disabled(!connected))
        .item(popup::item("Show hidden files", "eye", view, move |this, _, cx| this.set_show_hidden(!show_hidden, cx)).checked(show_hidden));
    if let Some(cwd) = cwd {
        let (a, b, c) = (cwd.clone(), cwd.clone(), cwd);
        menu = menu
            .separator()
            .item(popup::item("Copy current path", "clipboard-copy", view, move |this, _, cx| this.copy_paths(vec![a.clone()], cx)))
            .item(popup::item("Add to favorites", "star", view, move |this, _, cx| this.add_favorite(b.clone(), false, cx)))
            .item(popup::item("Add to favorites for this host", "server", view, move |this, _, cx| this.add_favorite(c.clone(), true, cx)));
    }
    menu
}

impl Render for FileBrowser {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
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
        let cwd = self.cwd.clone();
        let show_hidden = self.show_hidden;
        let connected = self.remote().is_some();
        div()
            .id("file-browser")
            .key_context("FileBrowser")
            .track_focus(&self.focus)
            .size_full()
            .relative()
            .flex()
            .flex_col()
            .on_key_down(cx.listener(Self::on_key_down))
            .drag_over::<ExternalPaths>(move |s, _, _, _| s.border_color(p.brand).bg(p.brand_soft.opacity(0.4)))
            .border_1()
            .border_color(gpui::transparent_black())
            .rounded(px(12.))
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
    fn errors_explain_themselves() {
        let err = OpError::new(ErrorKind::PermissionDenied, "Permission denied (EACCES): /etc/shadow");
        assert_eq!(
            explain("delete shadow", &err),
            "Couldn't delete shadow: Permission denied (EACCES): /etc/shadow. You don't have permission for this on the server."
        );
    }
}
