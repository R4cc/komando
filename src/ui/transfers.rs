//! Downloads and uploads across all Hosts, and the panel that lists them (opened from the
//! sidebar's Transfers entry).

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use chrono::{DateTime, Local};
use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, ClickEvent, Context, EventEmitter, FocusHandle, FontWeight, IntoElement, MouseDownEvent, SharedString, WeakEntity,
    Window, div, prelude::*, px,
};
use gpui_component::tooltip::Tooltip;

use crate::sftp::{self, ErrorKind, OpError, Progress, Remote, format_size};
use crate::theme::{AppPalette, Appearance};
use crate::ui::file_browser::FileBrowser;
use crate::ui::popup::{self, Popup};
use crate::ui::widgets::{focus_outline, icon_el, progress_bar};

#[derive(Clone, Debug)]
pub enum Job {
    Download { remote_path: String, dir: PathBuf, open_after: bool },
    Upload { local: PathBuf, remote_dir: String, name: String, replace: bool },
}

#[derive(Clone, Debug)]
pub enum State {
    Active,
    /// Finished. Downloads say where the file was saved; uploads where it went on the server.
    Done { at: DateTime<Local>, local: Option<PathBuf>, remote: Option<String> },
    Failed { at: DateTime<Local>, error: OpError },
}

pub struct Transfer {
    pub id: usize,
    pub job: Job,
    /// The file or folder's name.
    pub name: String,
    pub host: SharedString,
    pub is_dir: bool,
    /// The browser it was started from; asked again for its connection on retry.
    source: WeakEntity<FileBrowser>,
    progress: Arc<Progress>,
    pub state: State,
    /// The panel has shown how it ended. Until then a failure stays counted on the Transfers
    /// entry, and a finished download marks it as new.
    seen: bool,
    /// Part of the current burst of transfers, whose combined progress the Transfers entry
    /// shows. A burst starts when a transfer starts while none is running.
    in_burst: bool,
    /// How fast it is going while it runs.
    rate: Rate,
}

/// A running transfer's speed, smoothed so the speed and time left shown don't jump about.
/// Counting starts with the first bytes: time spent queued or listing a folder isn't speed.
#[derive(Default)]
struct Rate {
    /// When the first bytes moved.
    first: Option<Instant>,
    /// The latest sample: when, and the bytes done by then.
    last: Option<(Instant, u64)>,
    /// When the bytes done last went up.
    moved: Option<Instant>,
    /// Bytes per second.
    smoothed: Option<f64>,
}

impl Rate {
    /// How many seconds of samples the smoothed speed mostly reflects.
    const SMOOTHING: f64 = 3.;
    /// Bytes have moved for this long before a speed is shown: a first reading is mostly noise.
    const WARM_UP: Duration = Duration::from_secs(1);
    /// No bytes for this long, and the transfer is stalled rather than slow.
    const STALLED: Duration = Duration::from_secs(5);

    fn sample(&mut self, done: u64, now: Instant) {
        let Some((at, before)) = self.last else {
            if done > 0 {
                (self.first, self.last, self.moved) = (Some(now), Some((now, done)), Some(now));
            }
            return;
        };
        let elapsed = now.saturating_duration_since(at).as_secs_f64();
        if elapsed < 0.05 {
            return;
        }
        let speed = done.saturating_sub(before) as f64 / elapsed;
        self.smoothed = Some(match self.smoothed {
            None => speed,
            Some(smoothed) => smoothed + (speed - smoothed) * (1. - (-elapsed / Self::SMOOTHING).exp()),
        });
        if done > before {
            self.moved = Some(now);
        }
        self.last = Some((now, done));
    }

    fn stalled(&self, now: Instant) -> bool {
        self.moved.is_some_and(|moved| now.saturating_duration_since(moved) >= Self::STALLED)
    }

    /// Bytes per second, once it has been measured for long enough.
    fn speed(&self, now: Instant) -> Option<f64> {
        let warm = self.first.is_some_and(|first| now.saturating_duration_since(first) >= Self::WARM_UP);
        self.smoothed.filter(|speed| warm && *speed >= 1. && !self.stalled(now))
    }
}

/// "5 s left", "about 3 min left", "about 1 h 20 min left": rounder the further off it is, so
/// it doesn't flicker between close values.
fn time_left(remaining: u64, speed: f64) -> String {
    let secs = (remaining as f64 / speed).ceil() as u64;
    match secs {
        0..=9 => "a few seconds left".into(),
        10..=59 => format!("{} s left", secs.div_ceil(5) * 5),
        60..=3599 => format!("about {} min left", secs.div_ceil(60)),
        _ => {
            let mins = secs.div_ceil(60);
            match mins % 60 {
                0 => format!("about {} h left", mins / 60),
                m => format!("about {} h {m} min left", mins / 60),
            }
        }
    }
}

impl Transfer {
    fn is_upload(&self) -> bool {
        matches!(self.job, Job::Upload { .. })
    }

    fn remote_path(&self) -> String {
        match &self.job {
            Job::Download { remote_path, .. } => remote_path.clone(),
            Job::Upload { remote_dir, name, .. } => sftp::join(remote_dir, name),
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Filter {
    All,
    Successful,
    Failed,
}

/// What the Transfers entry shows around its icon.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Activity {
    /// Nothing running.
    Idle,
    /// Running, but no sizes known yet (a folder still being listed).
    Unknown,
    /// Running, this far (0 to 1) through the bytes of the current burst.
    Progress(f32),
}

impl Activity {
    /// Shows the same on the Transfers entry, whose ring and label move in whole percents.
    fn looks_like(self, other: Activity) -> bool {
        match (self, other) {
            (Activity::Progress(a), Activity::Progress(b)) => (a * 100.).round() == (b * 100.).round(),
            (a, b) => a == b,
        }
    }
}

/// Bytes done over bytes to transfer, as (done, total) per transfer. Transfers whose size
/// isn't known yet are left out; `None` if no size is known at all.
fn combined_progress(parts: impl Iterator<Item = (u64, u64)>) -> Option<f32> {
    let (done, total) =
        parts.filter(|(_, total)| *total > 0).fold((0u64, 0u64), |(d, t), (done, total)| (d + done.min(total), t + total));
    (total > 0).then(|| done as f32 / total as f32)
}

/// Tells the workspace that the counts on the Transfers entry changed.
pub struct TransfersChanged;

pub struct Transfers {
    items: Vec<Transfer>,
    next_id: usize,
    open: bool,
    /// When the panel last closed from a click outside it, so the click that closed it doesn't
    /// reopen it when it lands on the Transfers entry.
    closed_at: Option<Instant>,
    filter: Filter,
    ticking: bool,
    /// What the Transfers entry showed at the last repaint from a tick.
    ticked: Activity,
    popup: Popup,
    /// The panel's own focus: it takes the keyboard when it opens, so Tab reaches its controls
    /// and Escape closes it.
    focus: FocusHandle,
    /// What had the keyboard before the panel opened, given it back when the panel closes.
    return_focus: Option<FocusHandle>,
}

impl EventEmitter<TransfersChanged> for Transfers {}

/// Where downloads go: the user's Downloads folder.
pub fn download_dir() -> PathBuf {
    dirs::download_dir()
        .or_else(|| dirs::home_dir().map(|h| h.join("Downloads")))
        .unwrap_or_else(std::env::temp_dir)
}

/// Tag a file as coming from the Internet zone (Mark of the Web) so SmartScreen and Office
/// treat it like a browser download. Used only for files fetched to be opened directly.
/// Failing (FAT32, network shares) is not fatal: the file still opens.
#[cfg(windows)]
fn mark_from_internet(path: &std::path::Path) {
    let mut stream = path.as_os_str().to_owned();
    stream.push(":Zone.Identifier");
    if let Err(e) = std::fs::write(&stream, "[ZoneTransfer]\r\nZoneId=3\r\n") {
        log::warn!("could not mark {} as downloaded from the internet: {e}", path.display());
    }
}

#[cfg(not(windows))]
fn mark_from_internet(_path: &std::path::Path) {}

impl Transfers {
    pub fn new(cx: &mut Context<Self>) -> Self {
        Self {
            items: vec![],
            next_id: 1,
            open: false,
            closed_at: None,
            filter: Filter::All,
            ticking: false,
            ticked: Activity::Idle,
            popup: Popup::default(),
            focus: cx.focus_handle(),
            return_focus: None,
        }
    }

    pub fn is_open(&self) -> bool {
        self.open
    }

    /// Transfers that want attention: running, or failed and not yet looked at.
    pub fn attention(&self) -> usize {
        self.items
            .iter()
            .filter(|t| match t.state {
                State::Active => true,
                State::Failed { .. } => !t.seen,
                State::Done { .. } => false,
            })
            .count()
    }

    /// A download finished while the panel was closed, and the panel hasn't been opened since.
    pub fn new_download(&self) -> bool {
        self.items.iter().any(|t| !t.seen && !t.is_upload() && matches!(t.state, State::Done { .. }))
    }

    /// How far the current burst of transfers is, for the ring on the Transfers entry.
    pub fn activity(&self) -> Activity {
        if !self.has_active() {
            return Activity::Idle;
        }
        let parts = self.items.iter().filter(|t| t.in_burst).map(|t| {
            let total = t.progress.total.load(Ordering::Relaxed);
            let done = match t.state {
                State::Active => t.progress.done.load(Ordering::Relaxed),
                // Ended (even failed): no longer moving, so it counts as complete.
                _ => total,
            };
            (done, total)
        });
        match combined_progress(parts) {
            Some(fraction) => Activity::Progress(fraction),
            None => Activity::Unknown,
        }
    }

    pub fn has_active(&self) -> bool {
        self.items.iter().any(|t| matches!(t.state, State::Active))
    }

    pub fn toggle(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.open {
            self.close_in(window, cx);
            return;
        }
        if self.closed_at.is_some_and(|at| at.elapsed() < Duration::from_millis(300)) {
            return;
        }
        self.open = true;
        self.mark_seen();
        self.return_focus = window.focused(cx);
        window.focus(&self.focus, cx);
        cx.emit(TransfersChanged);
        cx.notify();
    }

    /// Escape is the panel's: it is open, has the keyboard, and no menu of its own is open.
    pub fn takes_escape(&self, window: &Window, cx: &gpui::App) -> bool {
        self.open && !self.popup.is_open() && self.focus.contains_focused(window, cx)
    }

    /// Close the panel and give the keyboard back to what had it before, unless something
    /// else took it meanwhile (a click outside the panel lands where it was aimed).
    pub fn close_in(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.close(cx);
        cx.defer_in(window, |this, window, cx| {
            if this.focus.contains_focused(window, cx)
                && let Some(handle) = this.return_focus.take()
            {
                window.focus(&handle, cx);
            }
        });
    }

    pub fn close(&mut self, cx: &mut Context<Self>) {
        if self.open {
            self.open = false;
            self.closed_at = Some(Instant::now());
            cx.emit(TransfersChanged);
            cx.notify();
        }
    }

    fn mark_seen(&mut self) {
        for t in &mut self.items {
            if !matches!(t.state, State::Active) {
                t.seen = true;
            }
        }
    }

    pub fn start(
        &mut self,
        job: Job,
        name: String,
        is_dir: bool,
        host: SharedString,
        source: WeakEntity<FileBrowser>,
        remote: Arc<Remote>,
        cx: &mut Context<Self>,
    ) {
        let id = self.next_id;
        self.next_id += 1;
        // Newest first.
        self.items.insert(
            0,
            Transfer {
                id,
                job,
                name,
                host,
                is_dir,
                source,
                progress: Arc::default(),
                state: State::Active,
                seen: false,
                in_burst: false,
                rate: Rate::default(),
            },
        );
        self.run(id, remote, cx);
    }

    fn run(&mut self, id: usize, remote: Arc<Remote>, cx: &mut Context<Self>) {
        // Nothing else running: this one starts a new burst.
        if !self.items.iter().any(|t| t.id != id && matches!(t.state, State::Active)) {
            for t in &mut self.items {
                t.in_burst = false;
            }
        }
        let Some(t) = self.items.iter_mut().find(|t| t.id == id) else { return };
        t.in_burst = true;
        let progress = Arc::new(Progress::default());
        t.progress = progress.clone();
        t.rate = Rate::default();
        t.state = State::Active;
        t.seen = false;
        let job = t.job.clone();
        let task = sftp::spawn(async move {
            match job {
                Job::Download { remote_path, dir, open_after } => {
                    let path = remote.download(&remote_path, &dir, &progress).await?;
                    if open_after {
                        // Before the file is handed to the system, so Windows warns like a browser download.
                        mark_from_internet(&path);
                    }
                    Ok((Some(path), None))
                }
                Job::Upload { local, remote_dir, name, replace } => {
                    remote.upload(&local, &remote_dir, &name, replace, &progress).await.map(|p| (None, Some(p)))
                }
            }
        });
        cx.spawn(async move |this, cx| {
            let result = task.await.and_then(|r| r);
            let _ = this.update(cx, |this, cx| this.finish(id, result, cx));
        })
        .detach();
        self.tick(cx);
        cx.emit(TransfersChanged);
        cx.notify();
    }

    fn finish(&mut self, id: usize, result: Result<(Option<PathBuf>, Option<String>), OpError>, cx: &mut Context<Self>) {
        let Some(t) = self.items.iter_mut().find(|t| t.id == id) else { return };
        let at = Local::now();
        match result {
            Ok((local, remote)) => {
                let opened = matches!(t.job, Job::Download { open_after: true, .. });
                if let (true, Some(path)) = (opened, &local) {
                    cx.open_with_system(path);
                }
                // New, unless the panel shows it finishing or it opened right away.
                t.seen = self.open || opened;
                t.state = State::Done { at, local, remote };
            }
            Err(error) => {
                // Seen already if the panel is open while it fails.
                t.seen = self.open || error.kind == ErrorKind::Cancelled;
                t.state = State::Failed { at, error };
            }
        }
        // The browser shows the new file when it is in the folder that changed.
        if let Job::Upload { remote_dir, .. } = &t.job
            && let Some(browser) = t.source.upgrade()
        {
            let dir = remote_dir.clone();
            browser.update(cx, |b, cx| b.changed_on_server(&dir, cx));
        }
        cx.emit(TransfersChanged);
        cx.notify();
    }

    /// Repaint progress a few times a second while anything is running.
    fn tick(&mut self, cx: &mut Context<Self>) {
        if self.ticking {
            return;
        }
        self.ticking = true;
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(Duration::from_millis(250)).await;
                let active = this
                    .update(cx, |this, cx| {
                        let now = Instant::now();
                        for t in this.items.iter_mut().filter(|t| matches!(t.state, State::Active)) {
                            t.rate.sample(t.progress.done.load(Ordering::Relaxed), now);
                        }
                        // Each repaint redraws the whole window, so only repaint when something
                        // shown moved: the open panel's byte counts, or the ring by a percent.
                        let activity = this.activity();
                        if this.open || !activity.looks_like(this.ticked) {
                            this.ticked = activity;
                            cx.notify();
                        }
                        let active = this.has_active();
                        if !active {
                            this.ticking = false;
                        }
                        active
                    })
                    .unwrap_or(false);
                if !active {
                    break;
                }
            }
        })
        .detach();
    }

    pub fn cancel(&mut self, id: usize) {
        if let Some(t) = self.items.iter().find(|t| t.id == id) {
            t.progress.cancel.store(true, Ordering::Relaxed);
        }
    }

    pub fn retry(&mut self, id: usize, cx: &mut Context<Self>) {
        let Some(t) = self.items.iter_mut().find(|t| t.id == id) else { return };
        let remote = t.source.upgrade().and_then(|b| b.read(cx).remote());
        match remote {
            Some(remote) => self.run(id, remote, cx),
            None => {
                t.state = State::Failed {
                    at: Local::now(),
                    error: OpError::new(
                        ErrorKind::Connection,
                        format!("Not connected. Open the file browser on {} to retry.", t.host),
                    ),
                };
                cx.notify();
            }
        }
    }

    pub fn remove(&mut self, id: usize, window: &mut Window, cx: &mut Context<Self>) {
        self.items.retain(|t| t.id != id || matches!(t.state, State::Active));
        if self.items.is_empty() {
            self.close_in(window, cx);
        }
        cx.emit(TransfersChanged);
        cx.notify();
    }

    /// Forget finished transfers. Downloaded files stay where they are.
    pub fn clear_finished(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.items.retain(|t| match self.filter {
            Filter::All => matches!(t.state, State::Active),
            Filter::Successful => !matches!(t.state, State::Done { .. }),
            Filter::Failed => !matches!(t.state, State::Failed { .. }),
        });
        if self.items.is_empty() {
            self.close_in(window, cx);
        }
        cx.emit(TransfersChanged);
        cx.notify();
    }

    fn counts(&self) -> (usize, usize, usize) {
        let ok = self.items.iter().filter(|t| matches!(t.state, State::Done { .. })).count();
        let failed = self.items.iter().filter(|t| matches!(t.state, State::Failed { .. })).count();
        (self.items.len(), ok, failed)
    }

    fn copy_text(&mut self, text: String, cx: &mut Context<Self>) {
        cx.write_to_clipboard(gpui::ClipboardItem::new_string(text));
    }

    fn copy_file(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        let _ = crate::export::copy_file_to_clipboard(&path, cx);
    }

    fn show_on_server(&mut self, id: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(t) = self.items.iter().find(|t| t.id == id) else { return };
        let path = match &t.state {
            State::Done { remote: Some(remote), .. } => remote.clone(),
            _ => t.remote_path(),
        };
        if let Some(browser) = t.source.upgrade() {
            browser.update(cx, |b, cx| b.reveal(&path, cx));
        }
        self.close_in(window, cx);
    }
}

/// "2m ago", "3h ago", or the date for older ones.
fn ago(at: DateTime<Local>) -> String {
    let secs = (Local::now() - at).num_seconds().max(0);
    match secs {
        0..=59 => "just now".into(),
        60..=3599 => format!("{}m ago", secs / 60),
        3600..=86399 => format!("{}h ago", secs / 3600),
        _ => at.format("%Y-%m-%d %H:%M").to_string(),
    }
}

/// A compact icon button for the panel's rows and header: `icon_button`'s keyboard focus and
/// label at the rows' smaller size.
fn row_button(id: impl Into<gpui::ElementId>, icon: &str, tip: &'static str, p: AppPalette) -> gpui::Stateful<gpui::Div> {
    div()
        .id(id)
        .role(gpui::Role::Button)
        .aria_label(tip)
        .tab_index(0)
        .focus_visible(move |s| focus_outline(s, p.focus))
        .size(px(24.))
        .flex()
        .flex_shrink_0()
        .items_center()
        .justify_center()
        .rounded(p.corner(6.))
        .cursor_pointer()
        .hover(move |s| s.bg(p.hover))
        .tooltip(move |window, cx| Tooltip::new(tip).build(window, cx))
        .child(icon_el(icon, 14., p.text_muted))
}

impl Transfers {
    fn render_row(&self, t: &Transfer, p: AppPalette, cx: &mut Context<Self>) -> AnyElement {
        let id = t.id;
        let done = t.progress.done.load(Ordering::Relaxed);
        let total = t.progress.total.load(Ordering::Relaxed);
        let verb = if t.is_upload() { "Uploaded" } else { "Downloaded" };
        let (icon, color) = match &t.state {
            State::Active => (if t.is_upload() { "upload" } else { "download" }, p.brand),
            State::Done { .. } => ("circle-check", p.success),
            State::Failed { error, .. } if error.kind == ErrorKind::Cancelled => ("circle-x", p.text_faint),
            State::Failed { .. } => ("circle-alert", p.danger),
        };
        let local_exists = match &t.state {
            State::Done { local: Some(path), .. } => Some(path.exists()),
            _ => None,
        };
        let waiting = done == 0 && t.progress.waiting.load(Ordering::Relaxed) > 0;
        let now = Instant::now();
        // While it runs: how fast, and how long until it's done once the size is known.
        let pace = match (t.rate.speed(now), t.rate.stalled(now)) {
            (_, true) => " · stalled".to_string(),
            (Some(speed), _) if total > done => {
                format!(" · {}/s · {}", format_size(speed.round() as u64), time_left(total - done, speed))
            }
            (Some(speed), _) => format!(" · {}/s", format_size(speed.round() as u64)),
            (None, _) => String::new(),
        };
        let detail: SharedString = match &t.state {
            State::Active if waiting => format!("Waiting for other transfers · {}", t.host).into(),
            State::Active if total > 0 => {
                format!("{} of {}{pace} · {}", format_size(done), format_size(total), t.host).into()
            }
            State::Active => format!("{}{pace} · {}", format_size(done), t.host).into(),
            State::Done { at, .. } if local_exists == Some(false) => {
                format!("Moved or deleted · {verb} {}", ago(*at)).into()
            }
            State::Done { at, .. } => format!("{} · {verb} {} · {}", format_size(total), ago(*at), t.host).into(),
            State::Failed { error, .. } => error.message.clone().into(),
        };
        let failed = matches!(t.state, State::Failed { .. });
        let tip: SharedString = format!("{}:{}", t.host, t.remote_path()).into();
        let view = cx.entity().downgrade();

        let mut actions = div().flex().flex_shrink_0().items_center().gap(px(1.));
        match &t.state {
            State::Active => {
                actions = actions.child(
                    row_button(("cancel", id), "x", "Cancel", p).on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                        this.cancel(id);
                        cx.notify();
                    })),
                );
            }
            State::Done { local: Some(path), .. } if local_exists == Some(true) => {
                let (a, b, c) = (path.clone(), path.clone(), path.clone());
                actions = actions
                    .child(row_button(("reveal", id), "folder-open", "Show in folder", p).on_click(cx.listener(
                        move |_, _: &ClickEvent, _, cx| cx.reveal_path(&a),
                    )))
                    .when(!t.is_dir, |el| {
                        el.child(row_button(("open", id), "external-link", "Open file", p).on_click(cx.listener(
                            move |_, _: &ClickEvent, _, cx| cx.open_with_system(&b),
                        )))
                    })
                    .child(row_button(("copy-file", id), "copy", "Copy file", p).on_click(cx.listener(
                        move |this, _: &ClickEvent, _, cx| this.copy_file(c.clone(), cx),
                    )));
            }
            State::Done { remote: Some(_), .. } => {
                actions = actions.child(row_button(("show", id), "folder-open", "Show on server", p).on_click(cx.listener(
                    move |this, _: &ClickEvent, window, cx| this.show_on_server(id, window, cx),
                )));
            }
            State::Failed { .. } => {
                actions = actions
                    .child(row_button(("retry", id), "refresh-cw", "Retry", p).on_click(cx.listener(
                        move |this, _: &ClickEvent, _, cx| this.retry(id, cx),
                    )))
                    .child(row_button(("remove", id), "trash", "Remove from list", p).on_click(cx.listener(
                        move |this, _: &ClickEvent, window, cx| this.remove(id, window, cx),
                    )));
            }
            _ => {}
        }
        // More: copy paths, remove.
        let local = match &t.state {
            State::Done { local, .. } => local.clone(),
            _ => None,
        };
        let remote_path = t.remote_path();
        let host = t.host.clone();
        let error = match &t.state {
            State::Failed { error, .. } => Some(error.message.clone()),
            _ => None,
        };
        let active = matches!(t.state, State::Active);
        actions = actions.child(row_button(("more", id), "ellipsis", "More", p).on_click(cx.listener(
            move |this, event: &ClickEvent, window, cx| {
                let (view, local, remote_path, host, error) =
                    (view.clone(), local.clone(), remote_path.clone(), host.clone(), error.clone());
                this.popup = Popup::open(event.position(), |this: &mut Self| &mut this.popup, window, cx, move |mut menu, _, _| {
                    if let Some(local) = local {
                        let text = local.display().to_string();
                        menu = menu.item(popup::item("Copy path", None, &view, move |this, _, cx| {
                            this.copy_text(text.clone(), cx)
                        }));
                    }
                    let server = format!("{host}:{remote_path}");
                    menu = menu.item(popup::item("Copy server path", None, &view, move |this, _, cx| {
                        this.copy_text(server.clone(), cx)
                    }));
                    if let Some(error) = error {
                        menu = menu.item(popup::item("Copy error", None, &view, move |this, _, cx| {
                            this.copy_text(error.clone(), cx)
                        }));
                    }
                    if !active {
                        menu = menu.separator().item(popup::item("Remove from list", None, &view, move |this, window, cx| {
                            this.remove(id, window, cx)
                        }));
                    }
                    menu
                });
                cx.notify();
            },
        )));

        let fraction = (total > 0).then(|| (done as f32 / total as f32).clamp(0., 1.));
        // Read out as one item: its name and how it is doing; its buttons follow.
        let label = format!("{}, {detail}", t.name);
        let reduce_motion = cx.reduce_motion();
        div()
            .id(("transfer", id))
            .role(gpui::Role::ListItem)
            .aria_label(label)
            .flex_shrink_0()
            .px(px(8.))
            .py(px(10.))
            .flex()
            .items_center()
            .gap(px(10.))
            .rounded(p.corner(8.))
            .hover(|s| s.bg(p.hover))
            .child(icon_el(icon, 17., color))
            .child(
                div()
                    .id(("transfer-text", id))
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .gap(px(4.))
                    .tooltip(move |window, cx| Tooltip::new(tip.clone()).build(window, cx))
                    .child(
                        div()
                            .whitespace_normal()
                            .text_size(px(13.))
                            .line_height(px(18.))
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(p.text)
                            .child(t.name.clone()),
                    )
                    .child(
                        div()
                            .whitespace_normal()
                            .text_size(px(12.))
                            .line_height(px(17.))
                            .text_color(if failed && color == p.danger { p.danger.opacity(0.9) } else { p.text_faint })
                            .child(detail),
                    )
                    .when(active, |el| {
                        // Queued behind other transfers, nothing moves: the bar sits empty.
                        let shown = if waiting { Some(0.) } else { fraction };
                        el.child(
                            div()
                                .id(("transfer-progress", id))
                                .role(gpui::Role::ProgressIndicator)
                                .aria_label(if t.is_upload() { "Upload progress" } else { "Download progress" })
                                .aria_min_numeric_value(0.)
                                .aria_max_numeric_value(100.)
                                .when_some(shown, |el, f| el.aria_numeric_value((f64::from(f) * 100.).round()))
                                .mt(px(3.))
                                .child(progress_bar(("transfer-bar", id), shown, 3., p.hover, p.brand, reduce_motion)),
                        )
                    }),
            )
            .child(actions)
            .into_any_element()
    }
}

impl Render for Transfers {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = cx.global::<Appearance>().app;
        if !self.open {
            return div().into_any_element();
        }
        let (all, ok, failed) = self.counts();
        let chip = |id: &'static str, label: &str, count: usize, filter: Filter, cx: &mut Context<Self>| {
            let selected = self.filter == filter;
            div()
                .id(id)
                .role(gpui::Role::RadioButton)
                .aria_label(format!("{label}, {count}"))
                .aria_toggled(if selected { gpui::Toggled::True } else { gpui::Toggled::False })
                .tab_index(0)
                .focus_visible(move |s| focus_outline(s, p.focus))
                .h(px(24.))
                .px(px(9.))
                .flex()
                .items_center()
                .gap(px(6.))
                .rounded(p.corner(7.))
                .border_1()
                .cursor_pointer()
                .text_size(px(11.5))
                .font_weight(FontWeight::MEDIUM)
                .when(selected, |el| el.bg(p.brand_soft).border_color(p.brand.opacity(0.6)).text_color(p.brand))
                .when(!selected, |el| el.border_color(p.border).text_color(p.text_muted).hover(|s| s.bg(p.hover)))
                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                    this.filter = filter;
                    cx.notify();
                }))
                .child(label.to_string())
                .child(div().text_color(if selected { p.brand } else { p.text_faint }).child(count.to_string()))
        };
        let shown: Vec<AnyElement> = self
            .items
            .iter()
            .filter(|t| match self.filter {
                Filter::All => true,
                Filter::Successful => matches!(t.state, State::Done { .. }),
                Filter::Failed => matches!(t.state, State::Failed { .. }),
            })
            .map(|t| self.render_row(t, p, cx))
            .collect();
        let empty = shown.is_empty();
        let empty_label = match self.filter {
            Filter::All => "No transfers yet.",
            Filter::Successful => "No successful transfers.",
            Filter::Failed => "No failed transfers.",
        };
        // Reserve space for the title bar, sidebar footer, panel heading, filters and actions
        // on shorter windows; a full-size window shows up to 440px of transfer rows.
        let list_height = (f32::from(window.bounds().size.height) - 260.).clamp(80., 440.);
        // Whether "Clear" has anything to remove under the current filter.
        let finished = self.items.iter().any(|t| match self.filter {
            Filter::All => !matches!(t.state, State::Active),
            Filter::Successful => matches!(t.state, State::Done { .. }),
            Filter::Failed => matches!(t.state, State::Failed { .. }),
        });
        div()
            .id("transfers-panel")
            .track_focus(&self.focus)
            .role(gpui::Role::Dialog)
            .aria_label("Transfers")
            .occlude()
            .w_full()
            .flex()
            .flex_col()
            .rounded(p.corner(14.))
            .bg(p.surface.opacity(1.))
            .border_1()
            .border_color(p.border)
            .on_mouse_down_out(cx.listener(|this, _: &MouseDownEvent, window, cx| {
                if !this.popup.is_open() {
                    this.close_in(window, cx);
                }
            }))
            .child(
                div()
                    .pl(px(14.))
                    .pr(px(8.))
                    .pt(px(10.))
                    .pb(px(8.))
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .child(icon_el("download", 15., p.text_muted))
                    .child(div().flex_1().text_size(px(13.5)).font_weight(FontWeight::SEMIBOLD).text_color(p.text).child("Transfers"))
                    .child(
                        row_button("transfers-close", "x", "Close", p)
                            .on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.close_in(window, cx))),
                    ),
            )
            .child(
                div()
                    .id("transfers-filter")
                    .role(gpui::Role::RadioGroup)
                    .aria_label("Show")
                    .px(px(12.))
                    .pb(px(8.))
                    .flex()
                    .gap(px(6.))
                    .child(chip("filter-all", "All", all, Filter::All, cx))
                    .child(chip("filter-ok", "Successful", ok, Filter::Successful, cx))
                    .child(chip("filter-failed", "Failed", failed, Filter::Failed, cx)),
            )
            .child(div().h(px(1.)).bg(p.border))
            .child(
                div()
                    .id("transfers-list")
                    .role(gpui::Role::List)
                    .aria_label("Transfers")
                    .max_h(px(list_height))
                    .overflow_y_scroll()
                    .p(px(4.))
                    .flex()
                    .flex_col()
                    .gap(px(1.))
                    .children(shown)
                    .when(empty, |el| {
                        el.child(div().py(px(28.)).text_center().text_size(px(12.)).text_color(p.text_faint).child(empty_label))
                    }),
            )
            .when(finished, |el| {
                let label = match self.filter {
                    Filter::All => "Clear finished",
                    Filter::Successful => "Clear successful",
                    Filter::Failed => "Clear failed",
                };
                el.child(div().h(px(1.)).bg(p.border)).child(
                    div().px(px(8.)).py(px(7.)).flex().items_center().child(
                        div()
                            .id("transfers-clear")
                            .role(gpui::Role::Button)
                            .aria_label(label)
                            .tab_index(0)
                            .focus_visible(move |s| focus_outline(s, p.focus))
                            .px(px(4.))
                            .rounded(p.corner(5.))
                            .text_size(px(11.5))
                            .text_color(p.text_faint)
                            .cursor_pointer()
                            .hover(|s| s.text_color(p.text))
                            .tooltip(|window, cx| Tooltip::new("Removes them from this list. Downloaded files are kept.").build(window, cx))
                            .on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.clear_finished(window, cx)))
                            .child(label),
                    ),
                )
            })
            .children(self.popup.render())
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(windows)]
    #[test]
    fn opened_files_get_a_mark_of_the_web() {
        let dir = std::env::temp_dir().join(format!("komando-motw-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("tool.exe");
        std::fs::write(&file, b"MZ").unwrap();
        mark_from_internet(&file);
        let mut stream = file.as_os_str().to_owned();
        stream.push(":Zone.Identifier");
        let text = std::fs::read_to_string(&stream).expect("zone stream is readable");
        assert!(text.contains("[ZoneTransfer]") && text.contains("ZoneId=3"), "{text}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn speed_shows_once_bytes_have_moved_for_a_second_and_smooths_out_bursts() {
        let start = Instant::now();
        let at = |ms: u64| start + Duration::from_millis(ms);
        let mut rate = Rate::default();
        // Queued or listing: no bytes, no speed, and not stalled either.
        rate.sample(0, at(0));
        rate.sample(0, at(3000));
        assert_eq!(rate.speed(at(3000)), None);
        assert!(!rate.stalled(at(3000)));
        // 1 MB every 250 ms is 4 MB/s, shown only after a second of it.
        let mb = 1_000_000;
        for i in 0..=3 {
            rate.sample(mb * (i + 1), at(3000 + 250 * i));
        }
        assert_eq!(rate.speed(at(3750)), None);
        rate.sample(5 * mb, at(4000));
        let steady = rate.speed(at(4000)).unwrap();
        assert!((steady - 4e6).abs() < 1., "{steady}");
        // One burst of ten times as much moves it only part of the way there.
        rate.sample(15 * mb, at(4250));
        let after = rate.speed(at(4250)).unwrap();
        assert!(after > steady && after < 1e7, "{after}");
    }

    #[test]
    fn a_transfer_with_no_bytes_for_five_seconds_is_stalled() {
        let start = Instant::now();
        let mut rate = Rate::default();
        rate.sample(100, start);
        rate.sample(200, start + Duration::from_secs(1));
        assert!(!rate.stalled(start + Duration::from_secs(5)));
        rate.sample(200, start + Duration::from_secs(6));
        assert!(rate.stalled(start + Duration::from_secs(6)));
        assert_eq!(rate.speed(start + Duration::from_secs(6)), None);
        // Bytes again: going once more.
        rate.sample(300, start + Duration::from_secs(7));
        assert!(!rate.stalled(start + Duration::from_secs(7)));
    }

    #[test]
    fn time_left_gets_rounder_the_further_off_it_is() {
        assert_eq!(time_left(5, 1.), "a few seconds left");
        assert_eq!(time_left(11, 1.), "15 s left");
        assert_eq!(time_left(60, 1.), "about 1 min left");
        assert_eq!(time_left(61, 1.), "about 2 min left");
        assert_eq!(time_left(3600, 1.), "about 1 h left");
        assert_eq!(time_left(4800, 1.), "about 1 h 20 min left");
    }

    #[test]
    fn progress_combines_the_bytes_of_every_transfer() {
        // A 100-byte file done, a 300-byte file a third through: 200 of 400.
        assert_eq!(combined_progress([(100, 100), (100, 300)].into_iter()), Some(0.5));
    }

    #[test]
    fn transfers_of_unknown_size_are_left_out_until_known() {
        assert_eq!(combined_progress([(0, 0), (50, 200)].into_iter()), Some(0.25));
        assert_eq!(combined_progress([(0, 0)].into_iter()), None);
        assert_eq!(combined_progress(std::iter::empty()), None);
    }

    #[test]
    fn the_ring_repaints_only_when_its_percent_changes() {
        assert!(Activity::Progress(0.501).looks_like(Activity::Progress(0.504)));
        assert!(!Activity::Progress(0.50).looks_like(Activity::Progress(0.51)));
        assert!(Activity::Unknown.looks_like(Activity::Unknown));
        assert!(!Activity::Unknown.looks_like(Activity::Progress(0.)));
        assert!(!Activity::Idle.looks_like(Activity::Unknown));
    }

    #[test]
    fn progress_never_passes_complete() {
        // A file that grew on the server while it downloaded.
        assert_eq!(combined_progress([(150, 100)].into_iter()), Some(1.0));
    }
}
