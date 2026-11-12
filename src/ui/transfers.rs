//! Downloads and uploads across all Hosts, and the panel that lists them (opened from the
//! sidebar's Downloads entry).

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use chrono::{DateTime, Local};
use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, ClickEvent, Context, EventEmitter, FontWeight, IntoElement, MouseDownEvent, SharedString, WeakEntity, Window, div,
    prelude::*, px,
};
use gpui_component::tooltip::Tooltip;

use crate::sftp::{self, ErrorKind, OpError, Progress, Remote, format_size};
use crate::theme::{AppPalette, Appearance};
use crate::ui::file_browser::FileBrowser;
use crate::ui::popup::{self, Popup};
use crate::ui::widgets::{elevation, icon_el};

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
    /// A failure stays counted on the Downloads entry until the panel has shown it.
    seen: bool,
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

/// Tells the workspace that the counts on the Downloads entry changed.
pub struct TransfersChanged;

pub struct Transfers {
    items: Vec<Transfer>,
    next_id: usize,
    open: bool,
    /// When the panel last closed from a click outside it, so the click that closed it doesn't
    /// reopen it when it lands on the Downloads entry.
    closed_at: Option<Instant>,
    filter: Filter,
    ticking: bool,
    popup: Popup,
}

impl EventEmitter<TransfersChanged> for Transfers {}

/// Where downloads go: the user's Downloads folder.
pub fn download_dir() -> PathBuf {
    dirs::download_dir()
        .or_else(|| dirs::home_dir().map(|h| h.join("Downloads")))
        .unwrap_or_else(std::env::temp_dir)
}

impl Transfers {
    pub fn new() -> Self {
        Self { items: vec![], next_id: 1, open: false, closed_at: None, filter: Filter::All, ticking: false, popup: Popup::default() }
    }

    /// Whether the Downloads entry shows: once anything was transferred.
    pub fn any(&self) -> bool {
        !self.items.is_empty()
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

    pub fn has_active(&self) -> bool {
        self.items.iter().any(|t| matches!(t.state, State::Active))
    }

    pub fn toggle(&mut self, cx: &mut Context<Self>) {
        let just_closed = self.closed_at.is_some_and(|at| at.elapsed() < Duration::from_millis(300));
        self.open = !self.open && !just_closed;
        if self.open {
            self.mark_seen();
        }
        cx.emit(TransfersChanged);
        cx.notify();
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
            if matches!(t.state, State::Failed { .. }) {
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
            Transfer { id, job, name, host, is_dir, source, progress: Arc::default(), state: State::Active, seen: false },
        );
        self.run(id, remote, cx);
    }

    fn run(&mut self, id: usize, remote: Arc<Remote>, cx: &mut Context<Self>) {
        let Some(t) = self.items.iter_mut().find(|t| t.id == id) else { return };
        let progress = Arc::new(Progress::default());
        t.progress = progress.clone();
        t.state = State::Active;
        t.seen = false;
        let job = t.job.clone();
        let task = sftp::spawn(async move {
            match job {
                Job::Download { remote_path, dir, .. } => {
                    remote.download(&remote_path, &dir, &progress).await.map(|p| (Some(p), None))
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
                if let (Job::Download { open_after: true, .. }, Some(path)) = (&t.job, &local) {
                    cx.open_with_system(path);
                }
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
                        cx.notify();
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

    pub fn remove(&mut self, id: usize, cx: &mut Context<Self>) {
        self.items.retain(|t| t.id != id || matches!(t.state, State::Active));
        if self.items.is_empty() {
            self.open = false;
        }
        cx.emit(TransfersChanged);
        cx.notify();
    }

    /// Forget finished transfers. Downloaded files stay where they are.
    pub fn clear_finished(&mut self, cx: &mut Context<Self>) {
        self.items.retain(|t| match self.filter {
            Filter::All => matches!(t.state, State::Active),
            Filter::Successful => !matches!(t.state, State::Done { .. }),
            Filter::Failed => !matches!(t.state, State::Failed { .. }),
        });
        if self.items.is_empty() {
            self.open = false;
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
        let _ = crate::export::copy_file_to_clipboard(&path);
        // Komando ships for Windows; elsewhere the clipboard gets the path.
        #[cfg(not(windows))]
        cx.write_to_clipboard(gpui::ClipboardItem::new_string(path.display().to_string()));
        let _ = cx;
    }

    fn show_on_server(&mut self, id: usize, cx: &mut Context<Self>) {
        let Some(t) = self.items.iter().find(|t| t.id == id) else { return };
        let path = match &t.state {
            State::Done { remote: Some(remote), .. } => remote.clone(),
            _ => t.remote_path(),
        };
        if let Some(browser) = t.source.upgrade() {
            browser.update(cx, |b, cx| b.reveal(&path, cx));
        }
        self.close(cx);
    }
}

impl Default for Transfers {
    fn default() -> Self {
        Self::new()
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

fn row_button(id: impl Into<gpui::ElementId>, icon: &str, tip: &'static str, p: AppPalette) -> gpui::Stateful<gpui::Div> {
    div()
        .id(id)
        .size(px(24.))
        .flex()
        .flex_shrink_0()
        .items_center()
        .justify_center()
        .rounded(px(6.))
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
        let detail: SharedString = match &t.state {
            State::Active if total > 0 => {
                format!("{} of {} · {}", format_size(done), format_size(total), t.host).into()
            }
            State::Active => format!("{} · {}", format_size(done), t.host).into(),
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
                    move |this, _: &ClickEvent, _, cx| this.show_on_server(id, cx),
                )));
            }
            State::Failed { .. } => {
                actions = actions
                    .child(row_button(("retry", id), "refresh-cw", "Retry", p).on_click(cx.listener(
                        move |this, _: &ClickEvent, _, cx| this.retry(id, cx),
                    )))
                    .child(row_button(("remove", id), "trash", "Remove from list", p).on_click(cx.listener(
                        move |this, _: &ClickEvent, _, cx| this.remove(id, cx),
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
                        menu = menu.item(popup::item("Copy path", "clipboard-copy", &view, move |this, _, cx| {
                            this.copy_text(text.clone(), cx)
                        }));
                    }
                    let server = format!("{host}:{remote_path}");
                    menu = menu.item(popup::item("Copy server path", "server", &view, move |this, _, cx| {
                        this.copy_text(server.clone(), cx)
                    }));
                    if let Some(error) = error {
                        menu = menu.item(popup::item("Copy error", "circle-alert", &view, move |this, _, cx| {
                            this.copy_text(error.clone(), cx)
                        }));
                    }
                    if !active {
                        menu = menu.separator().item(popup::item("Remove from list", "trash", &view, move |this, _, cx| {
                            this.remove(id, cx)
                        }));
                    }
                    menu
                });
                cx.notify();
            },
        )));

        let fraction = if total > 0 { (done as f32 / total as f32).clamp(0., 1.) } else { 0. };
        div()
            .id(("transfer", id))
            .px(px(8.))
            .py(px(7.))
            .flex()
            .items_center()
            .gap(px(10.))
            .rounded(px(8.))
            .hover(|s| s.bg(p.hover))
            .child(icon_el(icon, 17., color))
            .child(
                div()
                    .id(("transfer-text", id))
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .gap(px(2.))
                    .tooltip(move |window, cx| Tooltip::new(tip.clone()).build(window, cx))
                    .child(div().truncate().text_size(px(12.5)).font_weight(FontWeight::MEDIUM).text_color(p.text).child(t.name.clone()))
                    .child(
                        div()
                            .truncate()
                            .text_size(px(11.))
                            .text_color(if failed && color == p.danger { p.danger.opacity(0.9) } else { p.text_faint })
                            .child(detail),
                    )
                    .when(active, |el| {
                        el.child(
                            div().mt(px(3.)).h(px(3.)).w_full().rounded_full().bg(p.hover).child(
                                div().h_full().rounded_full().bg(p.brand).w(gpui::relative(if total > 0 { fraction } else { 0.15 })),
                            ),
                        )
                    }),
            )
            .child(actions)
            .into_any_element()
    }
}

impl Render for Transfers {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = cx.global::<Appearance>().app;
        if !self.open {
            return div().into_any_element();
        }
        let (all, ok, failed) = self.counts();
        let chip = |id: &'static str, label: &str, count: usize, filter: Filter, cx: &mut Context<Self>| {
            let selected = self.filter == filter;
            div()
                .id(id)
                .h(px(24.))
                .px(px(9.))
                .flex()
                .items_center()
                .gap(px(6.))
                .rounded(px(7.))
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
        let finished = self.items.iter().any(|t| !matches!(t.state, State::Active));
        div()
            .id("transfers-panel")
            .occlude()
            .w_full()
            .flex()
            .flex_col()
            .rounded(px(14.))
            .bg(p.surface.opacity(1.))
            .border_1()
            .border_color(p.border)
            .shadow(elevation(p.dark))
            .on_mouse_down_out(cx.listener(|this, _: &MouseDownEvent, _, cx| {
                if !this.popup.is_open() {
                    this.close(cx);
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
                    .child(div().flex_1().text_size(px(13.5)).font_weight(FontWeight::SEMIBOLD).text_color(p.text).child("Downloads"))
                    .child(
                        row_button("transfers-close", "x", "Close", p)
                            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.close(cx))),
                    ),
            )
            .child(
                div()
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
                    .max_h(px(340.))
                    .overflow_y_scroll()
                    .p(px(4.))
                    .flex()
                    .flex_col()
                    .gap(px(1.))
                    .children(shown)
                    .when(empty, |el| {
                        el.child(div().py(px(18.)).text_center().text_size(px(12.)).text_color(p.text_faint).child("Nothing here."))
                    }),
            )
            .when(finished, |el| {
                let label = match self.filter {
                    Filter::All => "Clear finished",
                    Filter::Successful => "Clear successful",
                    Filter::Failed => "Clear failed",
                };
                el.child(div().h(px(1.)).bg(p.border)).child(
                    div().px(px(12.)).py(px(7.)).flex().items_center().child(
                        div()
                            .id("transfers-clear")
                            .text_size(px(11.5))
                            .text_color(p.text_faint)
                            .cursor_pointer()
                            .hover(|s| s.text_color(p.text))
                            .tooltip(|window, cx| Tooltip::new("Removes them from this list. Downloaded files are kept.").build(window, cx))
                            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.clear_finished(cx)))
                            .child(label),
                    ),
                )
            })
            .children(self.popup.render())
            .into_any_element()
    }
}
