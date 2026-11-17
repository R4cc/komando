//! One terminal pane: paints the grid on the GPU and turns input into bytes for its Session.

use std::cell::{Cell, RefCell};
use std::collections::hash_map::Entry;
use std::collections::VecDeque;
use std::hash::Hash;
use std::ops::Range;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use alacritty_terminal::grid::{Dimensions as _, Scroll};
use alacritty_terminal::index::{Column, Line, Point as GridPoint, Side};
use alacritty_terminal::selection::{Selection, SelectionRange, SelectionType};
use alacritty_terminal::term::{Term, TermDamage, TermMode};
use alacritty_terminal::term::cell::Flags;
use alacritty_terminal::vte::ansi::{Color, CursorShape, NamedColor, Rgb};
use futures::StreamExt as _;
use rustc_hash::FxHashMap;
use futures::channel::{mpsc, oneshot};
use gpui::prelude::FluentBuilder as _;
use gpui::{
    App, Bounds, ClipboardItem, Context, ElementInputHandler, Entity, EntityInputHandler, EventEmitter, FocusHandle,
    Focusable, FontStyle, FontWeight, Hsla, KeyDownEvent, Modifiers, MouseButton, MouseDownEvent, MouseMoveEvent,
    MouseUpEvent, PathBuilder, Pixels, ScrollEvent, ShapedLine, SharedString, StrikethroughStyle, Subscription,
    Task, TextAlign, TextRun, TouchPhase, UTF16Selection, UnderlineStyle, Window, actions, canvas, div, fill,
    outline, point, prelude::*, px, size,
};
use gpui_component::input::{Input, InputEvent, InputState};
use gpui_component::menu::{ContextMenuExt, PopupMenu};
use gpui_component::Icon;

use crate::highlight::{self, ActiveHighlights};
use crate::keys::{self, MouseAction, MouseMode, Mods};
use crate::marks::Marks;
use crate::model::StartupCommand;
use crate::session::wait_for::WaitFor;
use crate::ui::connect_route::{self, ConnectRoute, RouteCard};
use crate::session::{
    Answer, EventProxy, GridSize, InputSink, Prompt, Remembered, SessionEvent, SharedTerm, Status, new_term,
    scrollback_text,
};
use crate::model::{LinkClick, RightClick};
use crate::theme::{Appearance, rgb_to_hsla};
use crate::ui::box_drawing::{self, Prim};
use crate::ui::widgets::{button, caps_lock_indicator, icon_text_button, password_input, ButtonKind};

mod rich_copy;
mod search;
mod selection;
#[cfg(all(test, target_os = "linux"))]
mod render_bench;

actions!(terminal, [
    Copy, Paste, ConfirmPaste, ScrollPageUp, ScrollPageDown, Find, SearchOlder, SearchNewer, SendTab, SendBackTab, SelectOutput,
]);

pub const CONTEXT: &str = "Terminal";
pub const PASTE_CONFIRM_CONTEXT: &str = "TerminalPasteConfirmation";
/// The search bar's key context, for Up and Down in its field.
pub const SEARCH_CONTEXT: &str = "TerminalSearch";
const PADDING_X: f32 = 12.0;
const PADDING_Y: f32 = 8.0;
/// Height each command takes on the rail: its line plus the gap below.
const RAIL_STEP: f32 = 8.0;
/// How long a row stays lit after jumping to it.
const FLASH: Duration = Duration::from_millis(1400);
/// Output this soon after a key press is its echo (or the prompt it brings back), not a command
/// at work.
const ECHO: Duration = Duration::from_millis(500);
/// Output that pauses longer than this has stopped. Long enough for programs that redraw
/// every few seconds (`top` every 3, `watch` every 2) to count as running throughout.
const ACTIVITY_GAP: Duration = Duration::from_secs(4);
/// Output has to keep coming this long before the Session counts as busy, so a quick `ls`
/// doesn't flash the sidebar.
const ACTIVITY_SUSTAIN: Duration = Duration::from_secs(1);
const ACTIVITY_CHECK: Duration = Duration::from_millis(250);

/// When output came in, and when the user last typed, to tell a command at work from echo.
#[derive(Default)]
struct Activity {
    last_input: Option<Instant>,
    /// When the current stretch of output began.
    since: Option<Instant>,
    last_output: Option<Instant>,
}

impl Activity {
    /// The user typed. Unless a command is already at work, what came before belongs to the
    /// last command: count afresh, so two quick commands in a row don't add up to one busy one.
    fn input(&mut self, now: Instant) {
        self.last_input = Some(now);
        if !self.busy(now) {
            self.since = None;
            self.last_output = None;
        }
    }

    fn output(&mut self, now: Instant) {
        if self.last_input.is_some_and(|t| now.duration_since(t) < ECHO) {
            return;
        }
        if self.settled(now) {
            self.since = Some(now);
        }
        self.last_output = Some(now);
    }

    /// No output for a while: nothing is going on.
    fn settled(&self, now: Instant) -> bool {
        !self.last_output.is_some_and(|t| now.duration_since(t) < ACTIVITY_GAP)
    }

    /// Output has kept coming for a while (not one burst) and hasn't stopped.
    fn busy(&self, now: Instant) -> bool {
        let lasted = match (self.since, self.last_output) {
            (Some(since), Some(last)) => last.duration_since(since),
            _ => Duration::ZERO,
        };
        !self.settled(now) && lasted >= ACTIVITY_SUSTAIN
    }
}
/// Lines per second the Scrollback moves while a selection is held just past the top or
/// bottom of the terminal; each line height further away adds as much again.
const AUTOSCROLL_SPEED: f32 = 25.;
/// Most lines per second, when the pointer is far from the terminal.
const AUTOSCROLL_MAX_SPEED: f32 = 200.;
/// Longest time counted between two frames, so a stalled frame doesn't jump the Scrollback.
const AUTOSCROLL_MAX_STEP: Duration = Duration::from_millis(100);
/// How long the shell must stay quiet before the next Startup Command is typed.
const STARTUP_QUIET: Duration = Duration::from_millis(150);
/// How long to wait for the shell's first output before typing anyway.
const STARTUP_FIRST: Duration = Duration::from_millis(1500);
/// How long a Startup Command waits for its text before it and the rest are dropped, so it
/// can't be typed into whatever the user is doing much later.
const STARTUP_WAIT_LIMIT: Duration = Duration::from_secs(60);

/// Startup Commands still to type. They go in one at a time: each once the shell has printed
/// something and then gone quiet, so each lands at a prompt like a typed command, or, for
/// one that waits for text, once that text appears.
#[derive(Default)]
struct StartupCommands {
    queue: VecDeque<StartupCommand>,
    /// Bumped whenever a new wait starts; a timer only types if no newer wait replaced it.
    generation: u64,
    /// The shell asked for a password (e.g. `sudo`): wait until the user has answered.
    paused: bool,
    /// The next command's text hasn't appeared yet: the id of its wait in the proxy.
    watching: Option<u64>,
}

impl StartupCommands {
    fn waiting(&self) -> bool {
        !self.queue.is_empty() && !self.paused
    }

    /// The next command waits for text, not for the shell to go quiet.
    fn next_waits(&self) -> bool {
        self.queue.front().is_some_and(|c| c.wait_for.is_some())
    }
}

/// Whether a terminal line is asking for a secret ("[sudo] password for ana:").
fn asks_for_secret(line: &str) -> bool {
    let line = line.trim_end().to_lowercase();
    line.ends_with(':') && (line.contains("password") || line.contains("passphrase"))
}

/// What a terminal tells the workspace about.
pub enum TerminalEvent {
    /// Status or title changed: refresh the sidebar.
    Changed,
    /// Save a typed user name or password for the Host of hop `hop` (0 is the first Jump
    /// Host, or the Host itself when there are none).
    Remember { hop: usize, what: Remembered },
    Reconnect,
    /// Open the host form for the Host of hop `hop`.
    EditHost { hop: usize },
    /// The pane's grid size is known or changed (see [`TerminalView::measured_size`]).
    Sized,
    /// A Startup Command's text didn't appear in time; it and the ones after it were dropped.
    StartupStopped { waited_for: String },
    /// A line was entered at a prompt (prompt included), for the Command History. Sent only
    /// while `command_history` is on.
    Entered { line: String, at: chrono::DateTime<chrono::Local> },
    /// Turn off the highlight rule that coloured a word (from the right-click menu).
    Unhighlight { rule: highlight::RuleHit },
    /// Turn the minimap off in Settings (from its right-click menu).
    HideMinimap,
    /// A Port Forward couldn't start; `why` says so in a sentence.
    ForwardFailed { forward: crate::model::PortForward, why: String },
}

enum ActivePrompt {
    HostKey { fingerprint: String, algorithm: String, other_types: Vec<String>, reply: Option<oneshot::Sender<bool>> },
    HostKeyChanged {
        fingerprint: String,
        previous: Option<String>,
        algorithm: String,
        openssh: bool,
        reply: Option<oneshot::Sender<bool>>,
    },
    Input {
        label: String,
        secret: bool,
        error: Option<String>,
        input: Entity<InputState>,
        /// The "Remember for this host" checkbox, when shown.
        remember: Option<bool>,
        reply: Option<oneshot::Sender<Option<Answer>>>,
    },
    ConfirmPaste { text: String, lines: usize },
}

pub struct TerminalView {
    pub title: SharedString,
    /// Output keeps coming in without anyone typing: a command at work (`tree /`, `tail -f`,
    /// a build). Shown in the sidebar.
    pub busy: bool,
    activity: Activity,
    /// A task is checking `activity` until the output settles.
    activity_watch: bool,
    /// The name the tab started with; programs that reset their title get it back.
    base_title: SharedString,
    pub status: Status,
    pub latency: Vec<crate::session::latency::HopLatency>,
    /// The Session's Port Forwards while it is connected, in order.
    pub forwards: Vec<crate::session::forward::ForwardStatus>,
    term: SharedTerm,
    proxy: EventProxy,
    sink: Option<InputSink>,
    focus: FocusHandle,
    grid: Rc<Cell<Option<GridSize>>>,
    origin: Rc<Cell<gpui::Point<Pixels>>>,
    cell: Rc<Cell<(f32, f32)>>,
    prompt: Option<ActivePrompt>,
    selecting: bool,
    /// Where the pointer is while selecting, even outside the terminal.
    drag_at: Option<gpui::Point<Pixels>>,
    /// The Scrollback is scrolling because a selection was dragged above or below the grid:
    /// when it last moved, and the part of a line it has yet to move.
    autoscroll: Option<(Instant, f32)>,
    /// A button press the program was told about; its release goes to the program too.
    mouse_pressed: Option<keys::MouseButton>,
    /// Text an IME (or a pending dead key on Linux) is composing, drawn at the cursor until
    /// it is committed.
    marked: Option<String>,
    /// The cell the mouse was last reported in (motion is reported once per cell).
    mouse_cell: Option<(usize, usize)>,
    /// Wheel movement smaller than a line, carried over to the next event.
    scroll_remainder: f32,
    /// Scrolled up past the top after a clear hid older output: the Restore scrollback button
    /// shows (while that output is still hidden and the view is still at the top).
    restore_offer: bool,
    /// Focus as last reported to the program (DECSET 1004).
    focus_reported: Option<bool>,
    /// While connecting through Jump Hosts: the route page.
    route: Option<ConnectRoute>,
    /// The backend writes why the connection ended into the output once its shell was open
    /// (an SSH Session's log), so the reconnect banner isn't needed then.
    tells_end: bool,
    /// The Session connected since `begin_connect`.
    was_connected: bool,
    startup: StartupCommands,
    /// Mouse and clipboard behaviour. Word separators take effect through `set_cursor`.
    pub pointer: crate::model::Pointer,
    /// Highlight Rules colour this terminal's output: off when its Host or Project turned
    /// highlighting off (the Settings switch empties the rules for every terminal instead).
    pub highlight: bool,
    /// A plain click began on this link (`LinkClick::Click`): it opens on release, unless
    /// the press turned into a selection.
    link_press: Option<TermLink>,
    pub windows_selection: bool,
    /// macOS: Option is Meta (ESC + key) rather than typing the layout's characters.
    pub option_as_meta: bool,
    keyboard_selection: selection::KeyboardSelection,
    cursor_epoch: Instant,
    cursor_timer: Option<Task<()>>,
    /// Show the command rail and record entered lines for it.
    pub command_rail: bool,
    /// Record entered lines for the Command History (`TerminalEvent::Entered`), with or
    /// without the rail.
    pub command_history: bool,
    /// Show the minimap (see `ui::minimap`).
    pub minimap: bool,
    /// Lay the minimap over the output instead of giving it a column of its own.
    pub minimap_overlay: bool,
    /// Over the output: how long the pointer rests at the right edge before it opens.
    pub minimap_delay: Duration,
    minimap_state: Rc<crate::ui::minimap::MinimapState>,
    grid_cache: Rc<RefCell<GridCache>>,
    marks: Marks,
    /// Where the last right-click landed, for the menu it opens.
    menu_point: Option<GridPoint>,
    /// The minimap heard this right-click before the terminal did (it sits inside it).
    pub(crate) minimap_right_press: bool,
    /// The open right-click menu is the minimap's: it offers Hide minimap.
    menu_on_minimap: bool,
    /// The link under the pointer, underlined; Ctrl+click opens it.
    hover_link: Option<TermLink>,
    hovered_mark: Option<u64>,
    /// The mark jumped to last, lit until the timer with the same number runs out.
    flash: Option<(u64, u64)>,
    /// Shrinks the Scrollback's rows once a new width has settled (see `compact_after_resize`).
    compaction: Option<Task<()>>,
    compaction_cancel: Option<Arc<AtomicBool>>,
    reflow: Rc<Cell<Reflow>>,
    width_preparation: Rc<RefCell<WidthPreparation>>,
    width_task: Option<Task<()>>,
    /// Lines of Scrollback the Session keeps.
    scrollback: usize,
    /// The search bar, while open (Ctrl+Shift+F), and what it last searched for.
    search: Option<search::SearchBar>,
    last_search: crate::search::Query,
    _subscriptions: Vec<Subscription>,
}

/// How long the width must stay the same before the rows are compacted, so dragging a window
/// edge compacts once at the end rather than every frame.
const COMPACT_DELAY: Duration = Duration::from_millis(500);

/// Below this size widening is cheap enough to do during layout.
const PREPARE_WIDTH_HISTORY: usize = 4_096;

#[derive(Default)]
struct WidthPreparation {
    pending: Option<PreparingWidth>,
}

struct PreparingWidth {
    cols: u16,
    ready: bool,
    cancelled: Arc<AtomicBool>,
}

impl Drop for PreparingWidth {
    fn drop(&mut self) {
        self.cancelled.store(true, Ordering::Relaxed);
    }
}

impl WidthPreparation {
    /// A new request cancels work for the previous width. Repeated frames share one worker.
    fn request(&mut self, cols: u16) -> Option<(u16, Arc<AtomicBool>)> {
        if self.pending.as_ref().is_some_and(|p| p.cols >= cols) {
            return None;
        }
        // A divider often moves a column per frame. Prepare a small group together so those
        // frames can share the worker instead of continually cancelling and restarting it.
        let cols = ((u32::from(cols) + 15) / 16 * 16).min(u32::from(u16::MAX)) as u16;
        let cancelled = Arc::new(AtomicBool::new(false));
        self.pending = Some(PreparingWidth { cols, ready: false, cancelled: cancelled.clone() });
        Some((cols, cancelled))
    }

    fn ready(&self, cols: u16) -> bool {
        self.pending.as_ref().is_some_and(|p| p.cols >= cols && p.ready)
    }

    fn finish(&mut self, cancelled: &Arc<AtomicBool>, prepared: bool) {
        if let Some(pending) = self.pending.as_mut()
            && Arc::ptr_eq(&pending.cancelled, cancelled)
        {
            if prepared {
                pending.ready = true;
            } else {
                self.pending = None;
            }
        }
    }
}

/// Limit repeated Scrollback reflows during a divider/window drag, while still drawing the
/// terminal at its existing column count between them. Allow one reflow's duration for other
/// work, capped at a 100 ms interval: a slow resize must not leave a long redraw cooldown.
#[derive(Clone, Copy, Default)]
struct Reflow {
    /// When the width last changed, and how long rewrapping took.
    at: Option<Instant>,
    took: Duration,
    /// A frame is due when the wait is over.
    woken: bool,
}

impl Reflow {
    /// How long a new width still waits.
    fn wait(&self, now: Instant) -> Option<Duration> {
        let ready = self.at? + (self.took * 2).min(Duration::from_millis(100));
        (ready > now).then(|| ready - now)
    }
}

impl EventEmitter<TerminalEvent> for TerminalView {}

impl Focusable for TerminalView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

/// Text input from the platform: plain keys (which `key_down` leaves unhandled), what dead
/// keys compose and what an IME commits. The "document" is only the composition in progress
/// (`marked`); committed text goes straight to the Session.
impl EntityInputHandler for TerminalView {
    fn text_for_range(
        &mut self,
        range: Range<usize>,
        adjusted_range: &mut Option<Range<usize>>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<String> {
        let utf16: Vec<u16> = self.marked.as_deref().unwrap_or_default().encode_utf16().collect();
        let range = range.start.min(utf16.len())..range.end.min(utf16.len());
        let text = String::from_utf16_lossy(&utf16[range.clone()]);
        *adjusted_range = Some(range);
        Some(text)
    }

    fn selected_text_range(&mut self, _: bool, _: &mut Window, _: &mut Context<Self>) -> Option<UTF16Selection> {
        let end = self.marked.as_deref().map_or(0, |text| text.encode_utf16().count());
        Some(UTF16Selection { range: end..end, reversed: false })
    }

    fn marked_text_range(&self, _: &mut Window, _: &mut Context<Self>) -> Option<Range<usize>> {
        self.marked.as_deref().map(|text| 0..text.encode_utf16().count())
    }

    fn unmark_text(&mut self, _: &mut Window, cx: &mut Context<Self>) {
        if self.marked.take().is_some() {
            cx.notify();
        }
    }

    fn replace_text_in_range(&mut self, _: Option<Range<usize>>, text: &str, window: &mut Window, cx: &mut Context<Self>) {
        // Committing ends the composition, and so does an empty commit (a cancelled one).
        if self.marked.take().is_some() {
            cx.notify();
        }
        if self.focus.is_focused(window) {
            self.type_text(text, cx);
        }
    }

    fn replace_and_mark_text_in_range(
        &mut self,
        _: Option<Range<usize>>,
        text: &str,
        _: Option<Range<usize>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.marked = (!text.is_empty()).then(|| text.to_string());
        window.invalidate_character_coordinates();
        cx.notify();
    }

    /// The cursor's cell, wherever in the composition: the IME puts its candidates by it.
    fn bounds_for_range(
        &mut self,
        _: Range<usize>,
        element_bounds: Bounds<Pixels>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        let (col, row) = cursor_cell(&*self.term.lock())?;
        let (cw, ch) = self.cell.get();
        let origin = point(element_bounds.left() + px(cw * col as f32), element_bounds.top() + px(ch * row as f32));
        Some(Bounds::new(origin, size(px(cw), px(ch))))
    }

    fn character_index_for_point(
        &mut self,
        _: gpui::Point<Pixels>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<usize> {
        None
    }

    fn accepts_text_input(&self, _: &mut Window, _: &mut Context<Self>) -> bool {
        !self.is_disconnected()
    }
}

/// The viewport cell (column, row) the cursor is in, when it is on screen.
fn cursor_cell<T>(term: &Term<T>) -> Option<(usize, usize)> {
    let cursor = term.grid().cursor.point;
    let row = cursor.line.0 + term.grid().display_offset() as i32;
    (0..term.screen_lines() as i32).contains(&row).then_some((cursor.column.0, row as usize))
}

/// The grid assumed before a pane is first laid out.
const DEFAULT_GRID: GridSize = GridSize { cols: 120, rows: 36, cell_width: 8, cell_height: 18 };

impl TerminalView {
    /// Create the pane and its grid. The backend is attached with [`TerminalView::attach`].
    pub fn new(title: impl Into<SharedString>, scrollback: usize, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let (tx, rx) = mpsc::unbounded();
        let proxy = EventProxy::new(tx);
        proxy.set_window_active(window.is_window_active());
        let initial = DEFAULT_GRID;
        if let Some(appearance) = cx.try_global::<Appearance>() {
            proxy.set_palette(&appearance.term);
        }
        let term = new_term(initial, scrollback, proxy.clone());
        Self::listen(rx, window, cx);
        let focus = cx.focus_handle();
        let subscriptions = vec![
            cx.on_focus_in(&focus, window, |this, window, cx| {
                this.reset_cursor();
                this.report_focus(window.is_window_active());
                cx.notify();
            }),
            cx.on_focus_out(&focus, window, |this, _, _, cx| {
                this.reset_cursor();
                this.report_focus(false);
                cx.notify();
            }),
            cx.observe_window_activation(window, |this, window, cx| {
                this.proxy.set_window_active(window.is_window_active());
                let focused = window.is_window_active() && this.focus.is_focused(window);
                this.reset_cursor();
                this.report_focus(focused);
                cx.notify();
            }),
        ];
        let title = title.into();
        Self {
            base_title: title.clone(),
            title,
            status: Status::Connecting,
            latency: Vec::new(),
            forwards: Vec::new(),
            term,
            proxy,
            sink: None,
            focus,
            grid: Rc::new(Cell::new(None)),
            origin: Rc::new(Cell::new(point(px(0.), px(0.)))),
            cell: Rc::new(Cell::new((8.0, 18.0))),
            prompt: None,
            busy: false,
            activity: Activity::default(),
            activity_watch: false,
            selecting: false,
            drag_at: None,
            autoscroll: None,
            mouse_pressed: None,
            marked: None,
            mouse_cell: None,
            scroll_remainder: 0.,
            restore_offer: false,
            focus_reported: None,
            route: None,
            tells_end: false,
            was_connected: false,
            startup: StartupCommands::default(),
            pointer: crate::model::Pointer::default(),
            link_press: None,
            highlight: true,
            windows_selection: false,
            option_as_meta: false,
            keyboard_selection: Default::default(),
            cursor_epoch: Instant::now(),
            cursor_timer: None,
            command_rail: true,
            command_history: false,
            minimap: true,
            minimap_overlay: false,
            minimap_delay: Duration::from_millis(300),
            minimap_state: Rc::new(Default::default()),
            grid_cache: Rc::new(RefCell::new(GridCache::default())),
            marks: Marks::default(),
            menu_point: None,
            minimap_right_press: false,
            menu_on_minimap: false,
            hover_link: None,
            hovered_mark: None,
            flash: None,
            compaction: None,
            compaction_cancel: None,
            reflow: Rc::default(),
            width_preparation: Rc::default(),
            width_task: None,
            scrollback,
            search: None,
            last_search: Default::default(),
            _subscriptions: subscriptions,
        }
    }

    fn listen(mut rx: mpsc::UnboundedReceiver<SessionEvent>, window: &mut Window, cx: &mut Context<Self>) {
        cx.spawn_in(window, async move |this, cx| {
            while let Some(event) = rx.next().await {
                let alive = this.update_in(cx, |this, window, cx| this.on_session_event(event, window, cx));
                if alive.is_err() {
                    break;
                }
            }
        })
        .detach();
    }

    pub fn term(&self) -> SharedTerm {
        self.term.clone()
    }

    pub fn proxy(&self) -> EventProxy {
        self.proxy.clone()
    }

    /// Paint again once a new width has waited long enough (see `Reflow`).
    fn repaint_after(&mut self, wait: Duration, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(wait).await;
            let _ = this.update(cx, |this, cx| {
                this.reflow.set(Reflow { woken: false, ..this.reflow.get() });
                cx.notify();
            });
        })
        .detach();
    }

    /// After the width changed: once it has settled, shrink the rows' allocations to the new
    /// width, off the UI thread. A newer width replaces (and so cancels) a pending pass.
    fn compact_after_resize(&mut self, cols: u16, cx: &mut Context<Self>) {
        if let Some(cancelled) = self.compaction_cancel.take() {
            cancelled.store(true, Ordering::Relaxed);
        }
        let cancelled = Arc::new(AtomicBool::new(false));
        self.compaction_cancel = Some(cancelled.clone());
        let term = self.term.clone();
        self.compaction = Some(cx.spawn(async move |_, cx| {
            cx.background_executor().timer(COMPACT_DELAY).await;
            cx.background_spawn(async move { crate::session::compact_rows_until(&term, cols as usize, &cancelled) }).await;
        }));
    }

    fn prepare_width(&mut self, from: u16, cols: u16, cancelled: Arc<AtomicBool>, cx: &mut Context<Self>) {
        if cancelled.load(Ordering::Relaxed) {
            return;
        }
        // A compaction for the old width would undo the allocations being prepared.
        if let Some(cancelled) = self.compaction_cancel.take() {
            cancelled.store(true, Ordering::Relaxed);
        }
        self.compaction = None;
        let term = self.term.clone();
        self.width_task = Some(cx.spawn(async move |this, cx| {
            let stop = cancelled.clone();
            let prepared = cx.background_spawn(async move {
                crate::session::prepare_width(&term, from as usize, cols as usize, &stop)
            }).await;
            let _ = this.update(cx, |view, cx| {
                view.width_preparation.borrow_mut().finish(&cancelled, prepared);
                cx.notify();
            });
        }));
    }

    /// The grid size from the pane's layout; `None` before it was first painted.
    pub fn measured_size(&self) -> Option<GridSize> {
        self.grid.get()
    }

    /// Take `size` (another pane's) before this pane is first painted, so a backend started
    /// now opens the shell at about the right size. The first paint corrects it if it differs.
    pub fn assume_size(&mut self, size: GridSize) {
        if self.grid.get().is_none() {
            self.grid.set(Some(size));
            crate::session::resize(&mut *self.term.lock(), size);
            self.proxy.set_size(size);
        }
    }

    /// The grid size last laid out, or a sensible default before the first paint.
    pub fn grid_size(&self) -> GridSize {
        self.grid.get().unwrap_or(DEFAULT_GRID)
    }

    /// Get ready for a new connection: the route page if it goes through Jump Hosts, and the
    /// Startup Commands to type once the shell is open. `tells_end`: the backend says in the
    /// output when the connection of an open shell ends.
    pub fn begin_connect(&mut self, route: Option<Vec<RouteCard>>, startup: Vec<StartupCommand>, tells_end: bool) {
        self.route = route.map(ConnectRoute::new);
        self.tells_end = tells_end;
        self.was_connected = false;
        self.startup = StartupCommands { queue: startup.into(), ..StartupCommands::default() };
        // Watched from before the shell opens, so text printed on sign-in isn't missed.
        self.watch_next(false);
    }

    /// Watch the output for the text the next Startup Command waits for, if it waits.
    /// `after_echo`: a command was just typed, so the line echoing it doesn't count.
    fn watch_next(&mut self, after_echo: bool) {
        let wait = self.startup.queue.front().and_then(|c| c.wait_for.as_deref()).map(|text| WaitFor::new(text, after_echo));
        self.startup.watching = wait.as_ref().map(WaitFor::id);
        self.proxy.wait_for(wait);
    }

    /// Drop the Startup Commands still to type, and stop watching for text.
    fn stop_startup(&mut self) {
        self.startup = StartupCommands::default();
        self.proxy.wait_for(None);
    }

    /// Give the next Startup Command's text `STARTUP_WAIT_LIMIT` to appear.
    fn limit_wait(&mut self, cx: &mut Context<Self>) {
        let Some(id) = self.startup.watching else { return };
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(STARTUP_WAIT_LIMIT).await;
            let _ = this.update(cx, |this, cx| {
                if this.startup.watching == Some(id) {
                    let waited_for = this.startup.queue.front().and_then(|c| c.wait_for.clone()).unwrap_or_default();
                    this.stop_startup();
                    cx.emit(TerminalEvent::StartupStopped { waited_for });
                }
            });
        })
        .detach();
    }

    pub fn attach(&mut self, sink: InputSink) {
        self.sink = Some(sink);
    }

    /// Where input goes, while a backend is attached.
    pub fn sink(&self) -> Option<InputSink> {
        self.sink.clone()
    }

    pub fn close(&mut self) {
        if let Some(sink) = self.sink.take() {
            sink.close();
        }
    }

    /// Set the status from outside (connect, close) and tell the workspace, which keeps its
    /// own copy for the sidebar.
    pub fn set_status(&mut self, status: Status, cx: &mut Context<Self>) {
        if self.status != status {
            if status != Status::Connected {
                self.latency.clear();
                self.forwards.clear();
            }
            self.status = status;
            cx.emit(TerminalEvent::Changed);
        }
        cx.notify();
    }

    pub fn is_disconnected(&self) -> bool {
        matches!(self.status, Status::Disconnected(_))
    }

    /// The whole Scrollback as plain text (for "Copy output").
    pub fn output_text(&self) -> String {
        scrollback_text(&*self.term.lock())
    }

    fn on_session_event(&mut self, event: SessionEvent, window: &mut Window, cx: &mut Context<Self>) {
        match event {
            SessionEvent::Wakeup => {
                crate::perf::count("session wakeup");
                self.proxy.clear_dirty();
                // Output since the last Wakeup is in the terminal by now (it wakes after writing).
                self.minimap_state.output();
                // New output may have moved the link's text from under the pointer: the
                // underline goes unless the same link is still where it was drawn.
                if let Some(link) = &self.hover_link
                    && let Some((line, cols)) = link.spans.first()
                    && link_at(&*self.term.lock(), GridPoint::new(*line, Column(cols.start))).as_ref() != Some(link)
                {
                    self.hover_link = None;
                }
                if self.status == Status::Connected {
                    self.note_output(cx);
                }
                if self.marks.is_pending() {
                    self.marks.output(&mut *self.term.lock());
                    self.emit_entered(cx);
                } else {
                    crate::session::drop_blank_scrollback(&mut *self.term.lock());
                }
                if self.startup.waiting() && !self.startup.next_waits() && !self.is_disconnected() && self.status == Status::Connected {
                    self.schedule_startup(STARTUP_QUIET, window, cx);
                }
                self.search_output(cx);
                cx.notify();
            }
            SessionEvent::Title(title) => {
                // An empty title is a reset (vim restores the title it found on exit).
                let title = if title.is_empty() { self.base_title.clone() } else { title.into() };
                if title != self.title {
                    self.title = title;
                    cx.emit(TerminalEvent::Changed);
                }
            }
            SessionEvent::Status(status) => {
                if self.status != status {
                    if status != Status::Connected {
                        self.latency.clear();
                    }
                    // They start before the shell, while the Session still says Connecting.
                    if matches!(status, Status::Disconnected(_)) {
                        self.forwards.clear();
                    }
                    self.status = status;
                    match &self.status {
                        Status::Disconnected(reason) => {
                            // A pending question can no longer be answered.
                            if matches!(self.prompt, Some(ActivePrompt::Input { .. } | ActivePrompt::HostKey { .. } | ActivePrompt::HostKeyChanged { .. })) {
                                self.prompt = None;
                            }
                            self.startup = StartupCommands::default();
                            self.proxy.wait_for(None);
                            match self.route.as_mut() {
                                Some(route) if !route.is_finished() => route.fail(reason.clone()),
                                _ => self.route = None,
                            }
                        }
                        Status::Connected => {
                            self.was_connected = true;
                            if let Some(route) = self.route.as_mut() {
                                route.finish();
                                cx.spawn_in(window, async move |this, cx| {
                                    cx.background_executor().timer(connect_route::FADE).await;
                                    let _ = this.update(cx, |this, cx| {
                                        if this.route.as_ref().is_some_and(|r| r.is_finished()) {
                                            this.route = None;
                                            cx.notify();
                                        }
                                    });
                                })
                                .detach();
                            }
                            if self.startup.next_waits() {
                                self.limit_wait(cx);
                            } else if self.startup.waiting() {
                                self.schedule_startup(STARTUP_FIRST, window, cx);
                            }
                        }
                        Status::Connecting => {}
                    }
                    cx.emit(TerminalEvent::Changed);
                    cx.notify();
                }
            }
            SessionEvent::Latency(latency) => {
                if !self.is_disconnected() {
                    match self.latency.iter_mut().find(|hop| hop.hop == latency.hop) {
                        Some(hop) => *hop = latency,
                        None => {
                            self.latency.push(latency);
                            self.latency.sort_by_key(|hop| hop.hop);
                        }
                    }
                    cx.emit(TerminalEvent::Changed);
                    cx.notify();
                }
            }
            SessionEvent::Forward(status) => {
                if !self.is_disconnected() {
                    if let crate::session::forward::ForwardState::Failed(why) = &status.state {
                        cx.emit(TerminalEvent::ForwardFailed { forward: status.forward.clone(), why: why.clone() });
                    }
                    match self.forwards.iter_mut().find(|f| f.index == status.index) {
                        Some(known) => *known = status,
                        None => {
                            self.forwards.push(status);
                            self.forwards.sort_by_key(|f| f.index);
                        }
                    }
                    cx.notify();
                }
            }
            SessionEvent::Hop { index, step } => {
                if let Some(route) = self.route.as_mut() {
                    route.advance(index, step);
                    cx.notify();
                }
            }
            SessionEvent::Prompt(prompt) => self.show_prompt(prompt, window, cx),
            SessionEvent::Remember { hop, what } => cx.emit(TerminalEvent::Remember { hop, what }),
            SessionEvent::ClipboardStore(text) => cx.write_to_clipboard(ClipboardItem::new_string(text)),
            SessionEvent::Appeared(id) => {
                if self.startup.watching == Some(id) {
                    self.startup.watching = None;
                    self.type_startup_command(cx);
                }
            }
        }
    }

    fn show_prompt(&mut self, prompt: Prompt, window: &mut Window, cx: &mut Context<Self>) {
        self.prompt = Some(match prompt {
            Prompt::HostKey { fingerprint, algorithm, other_types, reply } => {
                ActivePrompt::HostKey { fingerprint, algorithm, other_types, reply: Some(reply) }
            }
            Prompt::HostKeyChanged { fingerprint, previous, algorithm, openssh, reply } => {
                ActivePrompt::HostKeyChanged { fingerprint, previous, algorithm, openssh, reply: Some(reply) }
            }
            Prompt::Input { label, secret, remember, error, reply } => {
                let input = cx.new(|cx| InputState::new(window, cx).masked(secret).placeholder(label.clone()));
                let sub = cx.subscribe_in(&input, window, |this, _, event: &InputEvent, window, cx| {
                    if let InputEvent::PressEnter { .. } = event {
                        this.answer_input(true, window, cx);
                    }
                });
                self._subscriptions.push(sub);
                // Take the keyboard only from the terminal itself (a session you're watching
                // connect), not from a dialog you're typing in; the question waits behind it.
                if self.focus.is_focused(window) || window.focused(cx).is_none() {
                    input.update(cx, |state, cx| state.focus(window, cx));
                }
                ActivePrompt::Input { label, secret, error, input, remember, reply: Some(reply) }
            }
        });
        cx.notify();
    }

    /// Give the keyboard to the open question: its field, or the terminal for a question
    /// answered with buttons.
    pub fn focus_prompt(&self, window: &mut Window, cx: &mut Context<Self>) {
        let handle = match self.prompt.as_ref() {
            Some(ActivePrompt::Input { input, .. }) => input.focus_handle(cx),
            _ => self.focus.clone(),
        };
        if !handle.is_focused(window) {
            window.focus(&handle, cx);
        }
    }

    fn answer_input(&mut self, submit: bool, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(ActivePrompt::Input { input, remember, reply, .. }) = self.prompt.as_mut() {
            let value = input.read(cx).value().to_string();
            let remember = remember.unwrap_or(false);
            if let Some(reply) = reply.take() {
                let _ = reply.send(submit.then_some(Answer { value, remember }));
            }
            self.prompt = None;
            window.focus(&self.focus, cx);
            cx.notify();
        }
    }

    fn toggle_remember(&mut self, clicked: bool, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(ActivePrompt::Input { input, remember: Some(remember), .. }) = self.prompt.as_mut() {
            *remember = !*remember;
            // A click returns to the field so typing carries on; Space keeps the Tab position.
            if clicked {
                input.update(cx, |state, cx| state.focus(window, cx));
            }
            cx.notify();
        }
    }

    /// Answer a host key question: trust a new host's key, or replace a changed one.
    fn answer_host_key(&mut self, trust: bool, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(ActivePrompt::HostKey { reply, .. } | ActivePrompt::HostKeyChanged { reply, .. }) = self.prompt.as_mut()
            && let Some(reply) = reply.take()
        {
            let _ = reply.send(trust);
        }
        self.prompt = None;
        window.focus(&self.focus, cx);
        cx.notify();
    }

    fn dismiss(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.prompt = None;
        window.focus(&self.focus, cx);
        cx.notify();
    }

    /// Type the next Startup Command after `delay`, unless output arrives in the meantime
    /// (which starts a new wait).
    fn schedule_startup(&mut self, delay: Duration, window: &mut Window, cx: &mut Context<Self>) {
        self.startup.generation += 1;
        let generation = self.startup.generation;
        cx.spawn_in(window, async move |this, cx| {
            cx.background_executor().timer(delay).await;
            let _ = this.update(cx, |this, cx| {
                if this.startup.generation == generation {
                    this.type_startup_command(cx);
                }
            });
        })
        .detach();
    }

    fn type_startup_command(&mut self, cx: &mut Context<Self>) {
        if !self.startup.waiting() || self.status != Status::Connected {
            return;
        }
        if self.startup.next_waits() {
            // Typed once its text appears, whatever the shell does in between.
            if self.startup.watching.is_some() {
                return;
            }
        } else {
            let cursor_line = {
                let term = self.term.lock();
                let grid = term.grid();
                let row = &grid[grid.cursor.point.line];
                (0..term.columns()).map(|c| row[Column(c)].c).collect::<String>()
            };
            if asks_for_secret(&cursor_line) {
                self.startup.paused = true;
                return;
            }
        }
        let Some(next) = self.startup.queue.pop_front() else { return };
        // Before typing, so the output that follows can't come before the watch.
        self.watch_next(true);
        self.limit_wait(cx);
        let line = next.command;
        self.scroll_to_bottom();
        {
            let mut term = self.term.lock();
            if self.records_lines() && !term.mode().contains(TermMode::ALT_SCREEN) {
                self.marks.entered(&mut term);
            }
        }
        self.emit_entered(cx);
        self.write(format!("{line}\r").into_bytes());
        cx.notify();
    }

    /// Entered lines are read for the command rail or the Command History.
    fn records_lines(&self) -> bool {
        self.command_rail || self.command_history
    }

    /// Hand the lines read since last time to the Command History.
    fn emit_entered(&mut self, cx: &mut Context<Self>) {
        let lines = self.marks.take_new();
        if !self.command_history {
            return;
        }
        for mark in lines {
            cx.emit(TerminalEvent::Entered { line: mark.line, at: mark.at });
        }
    }

    /// Type `line` into the shell and run it, replacing anything typed but not yet entered.
    /// Refuses (false) when there is no shell to type into, or a full-screen program such as
    /// an editor has the terminal and would take the keys as its own.
    pub fn run_line(&mut self, line: &str, cx: &mut Context<Self>) -> bool {
        if self.sink.is_none() || self.status != Status::Connected || self.prompt.is_some() {
            return false;
        }
        {
            let mut term = self.term.lock();
            if term.mode().contains(TermMode::ALT_SCREEN) {
                return false;
            }
            if self.records_lines() {
                self.marks.entered(&mut term);
            }
        }
        self.emit_entered(cx);
        self.scroll_to_bottom();
        // Ctrl+U clears a half-typed command so it doesn't run together with `line`.
        self.write(format!("\x15{line}\r").into_bytes());
        cx.notify();
        true
    }

    fn write(&mut self, bytes: Vec<u8>) {
        self.reset_cursor();
        if let Some(sink) = &self.sink {
            self.activity.input(Instant::now());
            sink.write(bytes);
        }
    }

    /// Output arrived: note it, and keep `busy` up to date until the output settles.
    fn note_output(&mut self, cx: &mut Context<Self>) {
        self.activity.output(Instant::now());
        if self.activity_watch {
            return;
        }
        self.activity_watch = true;
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(ACTIVITY_CHECK).await;
                let go_on = this
                    .update(cx, |this, cx| {
                        let now = Instant::now();
                        let busy = this.activity.busy(now) && this.status == Status::Connected;
                        if busy != this.busy {
                            this.busy = busy;
                            cx.emit(TerminalEvent::Changed);
                        }
                        let settled = this.activity.settled(now);
                        if settled {
                            this.activity_watch = false;
                        }
                        !settled
                    })
                    .unwrap_or(false);
                if !go_on {
                    break;
                }
            }
        })
        .detach();
    }

    fn scroll_to_bottom(&mut self) {
        self.restore_offer = false;
        let mut term = self.term.lock();
        if term.grid().display_offset() != 0 {
            term.scroll_display(Scroll::Bottom);
        }
    }

    fn key_down(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        self.key_pressed(&event.keystroke, window, cx);
    }

    fn send_tab(&mut self, backwards: bool, window: &mut Window, cx: &mut Context<Self>) {
        // A field or button over the terminal keeps the component library's Tab navigation.
        if !self.focus.is_focused(window) {
            cx.propagate();
            return;
        }
        self.key_pressed(
            &gpui::Keystroke {
                key: "tab".into(),
                key_char: None,
                modifiers: Modifiers { shift: backwards, ..Default::default() },
            },
            window,
            cx,
        );
    }

    fn key_pressed(&mut self, ks: &gpui::Keystroke, window: &mut Window, cx: &mut Context<Self>) {
        // Keys typed into a field on top of the terminal (a connection question's user name or
        // password) bubble up to here. They belong to that field: sending them to the session
        // and stopping propagation would also stop the field from receiving the text.
        if !self.focus.is_focused(window) {
            return;
        }
        // A question answered with buttons holds the keyboard: typing must not reach the
        // shell behind it. Its Enter and Escape are actions, handled elsewhere.
        if self.prompt_holds_keys() {
            return;
        }
        let m = &ks.modifiers;
        let mods = Mods { ctrl: m.control, alt: m.alt, shift: m.shift };
        if self.windows_selection && !m.alt && !m.platform && self.prompt.is_none()
            && self.keyboard_selection.key(&mut self.term.lock(), &ks.key, m.control, m.shift)
        {
            cx.stop_propagation();
            cx.notify();
            return;
        }
        // Application shortcuts are all ctrl+shift+… (and ctrl+tab); plain ctrl+letter keys
        // belong to the shell (ctrl+w deletes a word in bash, ctrl+k kills the line).
        if keys::is_app_shortcut(&ks.key, ks.key_char.as_deref(), mods) {
            return;
        }
        // Unhandled, Alt+F4 closes the window and Alt+Space opens its menu.
        if cfg!(target_os = "windows") && keys::is_window_command(&ks.key, mods) {
            return;
        }
        if self.is_disconnected() {
            if ks.key == "enter" {
                cx.emit(TerminalEvent::Reconnect);
                cx.stop_propagation();
            }
            return;
        }
        // Plain text is left to come back through the input handler (`EntityInputHandler`
        // below), which is how dead keys compose and IMEs commit: Windows only turns the key
        // into text (WM_CHAR) when it goes unhandled here, and Linux types `key_char` itself.
        // Both do so only for keys with no modifier but shift, the same test as here.
        if m.is_subset_of(&Modifiers::shift()) && keys::is_text_input(ks.key_char.as_deref()) {
            return;
        }
        let (key_char, mods) = if cfg!(target_os = "macos") {
            keys::mac_option(&ks.key, ks.key_char.as_deref(), mods, self.option_as_meta)
        } else {
            (ks.key_char.as_deref(), mods)
        };
        let mode = *self.term.lock().mode();
        let app_cursor = mode.contains(TermMode::APP_CURSOR);
        if let Some(bytes) = keys::to_bytes(&ks.key, key_char, mods, app_cursor) {
            self.scroll_to_bottom();
            {
                let mut term = self.term.lock();
                term.selection = None;
                // Full-screen programs (vim, htop) take Enter for themselves: nothing to mark.
                if ks.key == "enter" && !m.control && !m.alt && self.records_lines() && !mode.contains(TermMode::ALT_SCREEN) {
                    self.marks.entered(&mut term);
                }
            }
            self.emit_entered(cx);
            // The user answered the question that paused the Startup Commands.
            if ks.key == "enter" && self.startup.paused {
                self.startup.paused = false;
            }
            self.write(bytes);
            cx.stop_propagation();
            cx.notify();
        }
    }

    /// Type text from the platform's text input: a key, a dead-key composition or an IME's
    /// commit.
    fn type_text(&mut self, text: &str, cx: &mut Context<Self>) {
        if text.is_empty() || self.is_disconnected() || self.prompt_holds_keys() {
            return;
        }
        self.scroll_to_bottom();
        self.term.lock().selection = None;
        self.write(text.as_bytes().to_vec());
        cx.notify();
    }

    fn copy(&mut self, _: &Copy, _: &mut Window, cx: &mut Context<Self>) {
        self.copy_selection(cx);
    }

    /// Put the selection on the clipboard, trimmed and with its colors as the settings say.
    /// Returns whether there was anything to copy.
    fn copy_selection(&mut self, cx: &mut Context<Self>) -> bool {
        let (text, html) = {
            let term = self.term.lock();
            let Some(text) = term.selection_to_string() else { return false };
            let text = if self.pointer.trim_copied { text.trim().to_string() } else { text };
            if text.is_empty() {
                return false;
            }
            // Only Windows can hold the HTML beside the text; see `rich_copy::write_clipboard`.
            let html = (cfg!(windows) && self.pointer.copy_formatting).then(|| {
                let appearance = cx.global::<Appearance>();
                rich_copy::selection_html(&term, &appearance.term, &appearance.font_family, appearance.font_size, self.pointer.trim_copied)
            });
            (text, html.flatten())
        };
        rich_copy::write_clipboard(text, html, cx);
        true
    }

    fn paste(&mut self, _: &Paste, window: &mut Window, cx: &mut Context<Self>) {
        let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) else { return };
        // Typing in the search bar: Ctrl+Shift+V pastes there, not into the shell.
        if self.search_focused(window, cx) {
            self.paste_into_search(&text, window, cx);
            return;
        }
        self.paste_or_confirm(text, cx);
    }

    /// A question answered with its buttons (a Host Key, a multi-line paste) is open, and the
    /// terminal keeps the focus for its Enter and Escape.
    fn prompt_holds_keys(&self) -> bool {
        matches!(self.prompt, Some(ActivePrompt::HostKey { .. } | ActivePrompt::HostKeyChanged { .. } | ActivePrompt::ConfirmPaste { .. }))
    }

    /// Paste `text`, asking first when it has several lines and that setting is on.
    fn paste_or_confirm(&mut self, text: String, cx: &mut Context<Self>) {
        // Nothing reaches the shell, or replaces the question, while one is open.
        if self.prompt.is_some() {
            return;
        }
        let text = if self.pointer.paste_single_line { one_line(&text) } else { text };
        let lines = pasted_lines(&text);
        if self.pointer.confirm_multiline_paste && lines > 1 {
            self.prompt = Some(ActivePrompt::ConfirmPaste { text, lines });
            cx.notify();
        } else {
            self.paste_text(&text);
        }
    }

    fn paste_text(&mut self, text: &str) {
        let bracketed = self.pointer.bracketed_paste && self.term.lock().mode().contains(TermMode::BRACKETED_PASTE);
        self.scroll_to_bottom();
        self.write(keys::paste_bytes(text, bracketed));
    }

    /// The Paste button and the dialog's Enter binding share one path. Taking the pending
    /// text before writing prevents a second confirmation from pasting it again.
    fn confirm_paste(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !matches!(self.prompt, Some(ActivePrompt::ConfirmPaste { .. })) {
            return;
        }
        if let Some(ActivePrompt::ConfirmPaste { text, .. }) = self.prompt.take() {
            self.paste_text(&text);
        }
        self.dismiss(window, cx);
    }

    fn scroll_page(&mut self, up: bool, cx: &mut Context<Self>) {
        if self.windows_selection && self.prompt.is_none() {
            let key = if up { "pageup" } else { "pagedown" };
            if self.keyboard_selection.key(&mut self.term.lock(), key, false, true) {
                cx.notify();
                return;
            }
            // On the alternate screen the program keeps Shift+PageUp/PageDown too.
            let app_cursor = self.term.lock().mode().contains(TermMode::APP_CURSOR);
            if let Some(bytes) = keys::to_bytes(key, None, Mods { ctrl: false, alt: false, shift: true }, app_cursor) {
                self.write(bytes);
                cx.notify();
            }
            return;
        }
        self.scroll_view(if up { Scroll::PageUp } else { Scroll::PageDown });
        cx.notify();
    }

    /// Scroll the Scrollback, offering the output a clear hid once the view is at its top.
    fn scroll_view(&mut self, scroll: Scroll) {
        let up = matches!(scroll, Scroll::PageUp | Scroll::Top) || matches!(scroll, Scroll::Delta(n) if n > 0);
        let mut term = self.term.lock();
        term.scroll_display(scroll);
        let grid = term.grid();
        self.restore_offer = up && grid.hidden_history() > 0 && grid.display_offset() == grid.history_size();
    }

    /// Whether the Restore scrollback button shows.
    fn shows_restore(&self) -> bool {
        self.restore_offer && {
            let term = self.term.lock();
            let grid = term.grid();
            !term.mode().contains(TermMode::ALT_SCREEN)
                && grid.hidden_history() > 0
                && grid.display_offset() == grid.history_size()
        }
    }

    /// Bring back the output a clear hid, and show the end of it.
    fn restore_scrollback(&mut self, cx: &mut Context<Self>) {
        self.restore_offer = false;
        {
            let mut term = self.term.lock();
            let half = (term.screen_lines() / 2) as i32;
            term.grid_mut().restore_history();
            term.scroll_display(Scroll::Delta(half));
        }
        cx.notify();
    }

    fn render_restore(&self, cx: &mut Context<Self>) -> Option<gpui::AnyElement> {
        if !self.shows_restore() {
            return None;
        }
        let p = cx.global::<Appearance>().app;
        let stop = |_: &MouseDownEvent, window: &mut Window, cx: &mut gpui::App| {
            // Not a click on the terminal: no selection, and the keyboard stays where it was.
            window.prevent_default();
            cx.stop_propagation();
        };
        Some(
            div()
                .absolute()
                .top(px(10.))
                .left_0()
                .right_0()
                .flex()
                .justify_center()
                .child(
                    div()
                        .rounded(p.corner(8.))
                        .shadow(crate::ui::widgets::elevation(p))
                        .font_family(p.font_family)
                        .text_size(px(13.))
                        .cursor_default()
                        .on_mouse_down(MouseButton::Left, stop)
                        .on_mouse_down(MouseButton::Middle, stop)
                        .on_mouse_down(MouseButton::Right, stop)
                        .child(
                            icon_text_button("restore-scrollback", "rotate-ccw", "Restore scrollback", ButtonKind::Secondary, p)
                                .on_click(cx.listener(|this, _, _, cx| this.restore_scrollback(cx))),
                        ),
                )
                .into_any_element(),
        )
    }

    /// The viewport cell under `position`, clamped to the grid.
    fn cell_at(&self, position: gpui::Point<Pixels>) -> (usize, usize) {
        let origin = self.origin.get();
        let (cw, ch) = self.cell.get();
        let size = self.grid_size();
        let x = (f32::from(position.x - origin.x) / cw).max(0.0) as usize;
        let y = (f32::from(position.y - origin.y) / ch).max(0.0) as usize;
        (x.min(size.cols.saturating_sub(1) as usize), y.min(size.rows.saturating_sub(1) as usize))
    }

    fn grid_point(&self, position: gpui::Point<Pixels>) -> (GridPoint, Side) {
        let origin = self.origin.get();
        let (cw, _) = self.cell.get();
        let (col, row) = self.cell_at(position);
        let x = f32::from(position.x - origin.x) / cw;
        // Past the left edge the selection starts at the line's start, past the right edge it
        // takes the last column in.
        let side = if x <= 0.0 {
            Side::Left
        } else if x >= self.grid_size().cols as f32 {
            Side::Right
        } else if x.fract() < 0.5 {
            Side::Left
        } else {
            Side::Right
        };
        let offset = self.term.lock().grid().display_offset() as i32;
        (GridPoint::new(Line(row as i32 - offset), Column(col)), side)
    }

    /// The link under the pointer, if any.
    ///
    /// `grid_point` locks the terminal itself, so it has to run before the lock here is taken:
    /// `link_at(&*self.term.lock(), self.grid_point(..))` locked it twice on the UI thread, and
    /// the terminal's mutex isn't reentrant, so the window hung for good.
    /// The rules colouring this terminal's output: none while its highlighting is off. The
    /// empty set is shared, so the grid sees the same one from frame to frame.
    fn highlighter(&self, cx: &App) -> Arc<highlight::Highlighter> {
        static NONE: std::sync::LazyLock<Arc<highlight::Highlighter>> = std::sync::LazyLock::new(Default::default);
        match cx.try_global::<ActiveHighlights>() {
            Some(active) if self.highlight => active.0.clone(),
            _ => NONE.clone(),
        }
    }

    /// Whether a click with these keys opens a link at once: the chosen key, and Ctrl (Cmd on
    /// macOS) when a plain click opens links, so programs that take the mouse still allow it.
    fn link_key_held(&self, m: &gpui::Modifiers) -> bool {
        match self.pointer.link_click {
            LinkClick::Ctrl | LinkClick::Click => m.control || m.platform,
            LinkClick::Alt => m.alt,
        }
    }

    fn link_under(&self, position: gpui::Point<Pixels>) -> Option<TermLink> {
        let at = self.grid_point(position).0;
        link_at(&*self.term.lock(), at)
    }

    /// The mouse mode the program asked for, if clicks should go to it. Shift held down
    /// always means "select text" instead, as in xterm.
    fn mouse_mode(&self, shift: bool) -> Option<MouseMode> {
        if shift || self.prompt.is_some() || self.sink.is_none() || self.is_disconnected() {
            return None;
        }
        let mouse = MouseMode::of(*self.term.lock().mode());
        mouse.reporting().then_some(mouse)
    }

    fn report_mouse(
        &mut self,
        mode: MouseMode,
        button: keys::MouseButton,
        action: MouseAction,
        position: gpui::Point<Pixels>,
        modifiers: &gpui::Modifiers,
    ) {
        let cell = self.cell_at(position);
        // Motion is reported once per cell, not once per pixel.
        if action == MouseAction::Motion && self.mouse_cell == Some(cell) {
            return;
        }
        self.mouse_cell = Some(cell);
        let mods = Mods { ctrl: modifiers.control, alt: modifiers.alt, shift: false };
        if let Some(bytes) = keys::mouse_report(mode, button, action, cell.0, cell.1, mods) {
            self.write(bytes);
        }
    }

    fn mouse_down(&mut self, event: &MouseDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        // A question is open: a click anywhere (its field included, since the card sits inside
        // the terminal) keeps the keyboard on the question and selects nothing behind it.
        // A right-click that reaches the terminal's wrapper opens its menu (see `context_menu`);
        // one that means something else stops here.
        if self.prompt.is_some() {
            if event.button == MouseButton::Right {
                cx.stop_propagation();
            }
            self.focus_prompt(window, cx);
            return;
        }
        window.focus(&self.focus, cx);
        // A right-click on the minimap is never the program's or a paste: it opens the menu.
        if event.button == MouseButton::Right {
            self.menu_on_minimap = std::mem::take(&mut self.minimap_right_press);
            if self.menu_on_minimap {
                self.menu_point = None;
                return;
            }
        }
        // The link key and click (Ctrl+click by default) open a link, even in a program that
        // took the mouse.
        self.link_press = None;
        if event.button == MouseButton::Left
            && self.link_key_held(&event.modifiers)
            && let Some(link) = self.link_under(event.position)
        {
            cx.open_url(&link.url);
            return;
        }
        if let Some(mode) = self.mouse_mode(event.modifiers.shift) {
            // The program asked for the mouse, right button included; shift+right-click is
            // still the menu.
            if event.button == MouseButton::Right {
                cx.stop_propagation();
            }
            let button = report_button(event.button);
            self.mouse_pressed = Some(button);
            self.report_mouse(mode, button, MouseAction::Press, event.position, &event.modifiers);
            return;
        }
        if event.button == MouseButton::Right {
            self.menu_point = Some(self.grid_point(event.position).0);
        }
        match event.button {
            MouseButton::Left => {
                // A plain click opens a link on release, if it didn't select anything.
                if self.pointer.link_click == LinkClick::Click && event.click_count == 1 && !event.modifiers.modified() {
                    self.link_press = self.link_under(event.position);
                }
                let (pt, side) = self.grid_point(event.position);
                let kind = match event.click_count {
                    2 => SelectionType::Semantic,
                    3 => SelectionType::Lines,
                    _ => SelectionType::Simple,
                };
                self.term.lock().selection = Some(Selection::new(kind, pt, side));
                self.selecting = true;
                self.drag_at = Some(event.position);
                cx.notify();
            }
            // A right-click that pastes (or copies); with Ctrl or Shift it opens the menu, as in PuTTY.
            MouseButton::Right if self.pointer.right_click != RightClick::Menu && !event.modifiers.control && !event.modifiers.shift => {
                cx.stop_propagation();
                let copied = self.pointer.right_click == RightClick::CopyOrPaste && self.copy_selection(cx);
                if copied {
                    self.term.lock().selection = None;
                    cx.notify();
                } else {
                    self.paste(&Paste, window, cx);
                }
            }
            MouseButton::Middle if self.pointer.middle_click_paste => {
                cx.stop_propagation();
                self.paste(&Paste, window, cx);
            }
            _ => {}
        }
    }

    /// The terminal's right-click menu: copy and paste, typing the selection into the shell,
    /// selecting everything, searching, and copying the whole output.
    fn context_menu(view: gpui::WeakEntity<Self>, focus: FocusHandle, menu: PopupMenu, cx: &mut App) -> PopupMenu {
        let Some(this) = view.upgrade() else { return menu };
        let (selected, can_type, hit, link) = {
            let this = this.read(cx);
            let highlighter = this.highlight.then(|| cx.try_global::<ActiveHighlights>().map(|h| h.0.clone())).flatten();
            let selected = this.term.lock().selection_to_string().is_some_and(|t| !t.is_empty());
            let hit = highlighter.zip(this.menu_point).and_then(|(h, at)| highlighted_at(&*this.term.lock(), &h, at));
            let link = this.menu_point.and_then(|at| link_at(&*this.term.lock(), at)).map(|l| l.url);
            (selected, this.sink.is_some() && !this.is_disconnected(), hit, link)
        };
        let menu = if this.read(cx).menu_on_minimap {
            menu.item(crate::ui::popup::item("Hide minimap", "eye-off", &view, |_, _, cx| cx.emit(TerminalEvent::HideMinimap)))
                .separator()
        } else {
            menu
        };
        let icon = |name: &str| Icon::empty().path(format!("icons/{name}.svg"));
        let run = |label: &str, icon_name: &str, f: fn(&mut Self, &mut Window, &mut Context<Self>)| {
            crate::ui::popup::item(label, icon_name, &view, f)
        };
        // A highlighted word was right-clicked: its rule can be turned off from here.
        let menu = match hit {
            Some(hit) => {
                let label = format!("Unhighlight “{}”", ellipsize(&hit.matched, 32));
                let item = crate::ui::popup::item(label, "eraser", &view, move |_, _, cx| {
                    cx.emit(TerminalEvent::Unhighlight { rule: hit.clone() })
                });
                menu.item(item).separator()
            }
            None => menu,
        };
        // A link was right-clicked: open or copy it.
        let menu = match link {
            Some(url) => {
                let copy = url.clone();
                menu.item(crate::ui::popup::item("Open link", "external-link", &view, move |_, _, cx| cx.open_url(&url)))
                    .item(crate::ui::popup::item("Copy link", "link", &view, move |_, _, cx| {
                        cx.write_to_clipboard(ClipboardItem::new_string(copy.clone()))
                    }))
                    .separator()
            }
            None => menu,
        };
        menu.action_context(focus)
            .menu_with_icon_and_disabled("Copy", icon("copy"), Box::new(Copy), !selected)
            .menu_with_icon_and_disabled("Paste", icon("clipboard-paste"), Box::new(Paste), !can_type)
            .item(
                run("Paste selection", "text-cursor-input", |this, _, cx| this.paste_selection(cx))
                    .disabled(!(selected && can_type)),
            )
            .separator()
            .item(run("Select all", "scan-text", |this, _, cx| this.select_all(cx)))
            .menu_with_icon("Find in output…", icon("search"), Box::new(Find))
            .separator()
            .menu_with_icon("Copy output", icon("copy"), Box::new(crate::ui::workspace::CopyOutputText))
            .menu_with_icon("Copy as .txt", icon("file-text"), Box::new(crate::ui::workspace::CopyOutputFile))
    }

    /// Type the selected text into the shell, as if pasted.
    fn paste_selection(&mut self, cx: &mut Context<Self>) {
        let Some(text) = self.term.lock().selection_to_string().filter(|t| !t.is_empty()) else { return };
        self.paste_or_confirm(text, cx);
    }

    /// Select the whole output: the Scrollback and the screen down to its last written line
    /// (or the cursor's), not the blank rows below.
    fn select_all(&mut self, cx: &mut Context<Self>) {
        self.keyboard_selection.select_all(&mut self.term.lock());
        cx.notify();
    }

    fn select_output(&mut self, _: &SelectOutput, window: &mut Window, cx: &mut Context<Self>) {
        if self.windows_selection && self.focus.is_focused(window) && self.prompt.is_none()
            && !self.term.lock().mode().contains(TermMode::ALT_SCREEN)
        {
            self.select_all(cx);
        } else {
            // Keep the existing Copy output shortcut when keyboard selection is off.
            cx.propagate();
        }
    }

    pub fn set_cursor(&mut self, shape: crate::model::TerminalCursor, blinking: bool) {
        let mut config = crate::session::terminal_config(self.scrollback);
        config.semantic_escape_chars = format!("{}\t", self.pointer.word_separators);
        config.default_cursor_style = alacritty_terminal::vte::ansi::CursorStyle {
            shape: match shape {
                crate::model::TerminalCursor::Line => CursorShape::Beam,
                crate::model::TerminalCursor::Block => CursorShape::Block,
                crate::model::TerminalCursor::Underline => CursorShape::Underline,
            },
            blinking,
        };
        self.term.lock().set_options(config);
        self.reset_cursor();
    }

    fn reset_cursor(&mut self) {
        self.cursor_epoch = Instant::now();
        self.cursor_timer = None;
    }

    /// Wake only at blink edges, and only while the terminal has the keyboard.
    fn cursor_visible(&mut self, focused: bool, cx: &mut Context<Self>) -> bool {
        let blink = {
            let term = self.term.lock();
            focused && term.cursor_style().blinking && term.mode().contains(TermMode::SHOW_CURSOR)
        };
        if !blink {
            self.cursor_timer = None;
            return true;
        }
        let elapsed = self.cursor_epoch.elapsed();
        if self.cursor_timer.is_none() {
            let wait = Duration::from_millis(500 - (elapsed.as_millis() % 500) as u64);
            self.cursor_timer = Some(cx.spawn(async move |this, cx| {
                cx.background_executor().timer(wait).await;
                let _ = this.update(cx, |this, cx| {
                    this.cursor_timer = None;
                    cx.notify();
                });
            }));
        }
        (elapsed.as_millis() / 500) % 2 == 0
    }

    /// Pointer motion over the terminal, for programs that asked for it. A selection follows
    /// the pointer through `drag_selection` instead, which hears it anywhere.
    fn mouse_move(&mut self, event: &MouseMoveEvent, _: &mut Window, cx: &mut Context<Self>) {
        let link = if self.selecting || event.pressed_button.is_some() || self.prompt.is_some() {
            None
        } else {
            self.link_under(event.position)
        };
        if link != self.hover_link {
            self.hover_link = link;
            cx.notify();
        }
        if self.selecting {
            return;
        }
        if let Some(mode) = self.mouse_mode(event.modifiers.shift) {
            let button = match event.pressed_button {
                Some(button) if self.mouse_pressed.is_some() => report_button(button),
                _ => keys::MouseButton::None,
            };
            if mode.wants(button, MouseAction::Motion) {
                self.report_mouse(mode, button, MouseAction::Motion, event.position, &event.modifiers);
            }
        }
    }

    /// Pointer motion anywhere in the window: a held minimap slider follows it (even off the
    /// minimap), and the overlay opens and closes with it.
    fn minimap_hover(&mut self, event: &MouseMoveEvent, window: &mut Window, cx: &mut Context<Self>) {
        let state = self.minimap_state.clone();
        // So does the scrollbar's thumb, shown while the minimap is off.
        if state.bar_dragging() || state.dragging() {
            if event.pressed_button == Some(MouseButton::Left) {
                if state.bar_dragging() {
                    crate::ui::minimap::bar_drag(&self.term, &state, event.position.y);
                } else {
                    crate::ui::minimap::drag(&self.term, &state, event.position.y);
                }
                cx.stop_propagation();
                cx.notify();
                return;
            }
            // The release happened somewhere nothing reported it.
            state.bar_release();
            state.release();
            cx.notify();
        }
        self.minimap_track(event.position, event.pressed_button.is_some(), window, cx);
    }

    /// Something else has the pointer, or there is nothing for the overlay to show.
    fn minimap_blocked(&self, window: &Window) -> bool {
        !self.minimap
            || !self.minimap_overlay
            || self.prompt.is_some()
            || self.route.is_some()
            || self.selecting
            || self.mouse_pressed.is_some()
            || !window.is_window_active()
            || {
                let term = self.term.lock();
                term.mode().contains(TermMode::ALT_SCREEN) || term.grid().history_size() == 0
            }
    }

    /// Open or close the overlay for the pointer at `position`. Merely passing over output
    /// never opens it, nor may a selection; once open it stays while the pointer is on it.
    fn minimap_track(&mut self, position: gpui::Point<Pixels>, pressed: bool, window: &mut Window, cx: &mut Context<Self>) {
        use crate::ui::minimap::{HIDE_DELAY, Timer, hover_region};
        if !self.minimap_overlay {
            return;
        }
        let state = self.minimap_state.clone();
        let Some(bounds) = state.terminal() else {
            return;
        };
        let (at_edge, over_map) = hover_region(position, bounds, window.viewport_size().width, PADDING_X);
        let blocked = pressed || self.minimap_blocked(window);
        let now = Instant::now();
        let before = state.look();
        let timer = state.track(at_edge, over_map, blocked, now);
        if state.look() != before {
            cx.notify();
        }
        match timer {
            Some(Timer::Show(generation)) if self.minimap_delay.is_zero() => {
                if state.show(generation, now) {
                    cx.notify();
                }
            }
            Some(Timer::Show(generation)) => {
                let delay = self.minimap_delay;
                cx.spawn_in(window, async move |this, cx| {
                    cx.background_executor().timer(delay).await;
                    let _ = this.update_in(cx, |this, window, cx| {
                        let state = this.minimap_state.clone();
                        if !this.minimap_blocked(window) && window.is_window_hovered() {
                            if state.show(generation, Instant::now()) {
                                cx.notify();
                            }
                        } else if state.look().1 {
                            state.cancel_pending();
                            cx.notify();
                        }
                    });
                })
                .detach();
            }
            Some(Timer::Hide(generation)) => {
                cx.spawn_in(window, async move |this, cx| {
                    cx.background_executor().timer(HIDE_DELAY).await;
                    let _ = this.update_in(cx, |this, _, cx| {
                        if !this.minimap_state.dragging() && this.minimap_state.leave(generation, Instant::now()) {
                            cx.notify();
                        }
                    });
                })
                .detach();
            }
            None => {}
        }
    }

    /// A press on the minimap. Returns whether the minimap took it.
    pub(crate) fn minimap_pressed(&mut self, y: Pixels, window: &mut Window, cx: &mut Context<Self>) -> bool {
        // A question is open: the terminal's own handler keeps the keyboard on it.
        if self.prompt.is_some() || self.route.is_some() {
            return false;
        }
        window.focus(&self.focus, cx);
        crate::ui::minimap::press(&self.term, &self.minimap_state, y);
        if self.minimap_overlay {
            self.minimap_state.open_now(Instant::now());
        }
        cx.notify();
        true
    }

    /// The left button came up. Returns whether it ended a drag of the minimap's slider. The
    /// overlay stays open; if the pointer is off it, it fades after the usual pause.
    pub(crate) fn minimap_released(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        if !self.minimap_state.dragging() {
            return false;
        }
        self.minimap_state.release();
        self.minimap_track(window.mouse_position(), false, window, cx);
        cx.notify();
        true
    }

    /// A press on the scrollbar shown while the minimap is off. Returns whether it took it.
    pub(crate) fn scrollbar_pressed(&mut self, y: Pixels, window: &mut Window, cx: &mut Context<Self>) -> bool {
        // A question is open: the terminal's own handler keeps the keyboard on it.
        if self.prompt.is_some() || self.route.is_some() {
            return false;
        }
        window.focus(&self.focus, cx);
        crate::ui::minimap::bar_press(&self.term, &self.minimap_state, y);
        cx.notify();
        true
    }

    /// The left button came up. Returns whether it let go of the scrollbar's thumb.
    pub(crate) fn scrollbar_released(&mut self, cx: &mut Context<Self>) -> bool {
        if !self.minimap_state.bar_dragging() {
            return false;
        }
        self.minimap_state.bar_release();
        cx.notify();
        true
    }

    /// A click on the overlay's handle: open now, without waiting.
    pub(crate) fn minimap_open_now(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        if self.minimap_blocked(window) {
            return false;
        }
        self.minimap_state.open_now(Instant::now());
        cx.notify();
        true
    }

    /// The pointer moved while a selection is being dragged, wherever it is: over the
    /// terminal, the sidebar, or outside the window (the window keeps the mouse while a button
    /// is held). Past the top or bottom of the grid the Scrollback scrolls along.
    fn drag_selection(&mut self, event: &MouseMoveEvent, window: &mut Window, cx: &mut Context<Self>) {
        if !self.selecting {
            return;
        }
        if event.pressed_button != Some(MouseButton::Left) {
            // The release was missed (it happened somewhere nothing reported it): stop here.
            self.selecting = false;
            self.drag_at = None;
            return;
        }
        self.drag_at = Some(event.position);
        self.extend_selection(event.position);
        if self.autoscroll_speed(event.position) != 0. && self.autoscroll.is_none() {
            // Step once per displayed frame, so it moves as smoothly as the display refreshes.
            self.autoscroll = Some((Instant::now(), 0.));
            cx.on_next_frame(window, |this, window, cx| this.autoscroll_step(window, cx));
        }
        cx.notify();
    }

    fn extend_selection(&mut self, position: gpui::Point<Pixels>) {
        let (pt, side) = self.grid_point(position);
        if let Some(selection) = self.term.lock().selection.as_mut() {
            selection.update(pt, side);
        }
    }

    /// Lines per second to scroll for a selection dragged to `position`: up (positive) above
    /// the grid, down below it, faster the further away; 0 over it.
    fn autoscroll_speed(&self, position: gpui::Point<Pixels>) -> f32 {
        let top = f32::from(self.origin.get().y);
        let (_, ch) = self.cell.get();
        let bottom = top + self.grid_size().rows as f32 * ch;
        let y = f32::from(position.y);
        let speed = |distance: f32| ((1. + distance / ch) * AUTOSCROLL_SPEED).min(AUTOSCROLL_MAX_SPEED);
        if y < top {
            speed(top - y)
        } else if y >= bottom {
            -speed(y - bottom)
        } else {
            0.
        }
    }

    /// One frame of scrolling while a selection is held above or below the grid: as many
    /// lines as the time since the last frame allows, the rest carried to the next frame.
    fn autoscroll_step(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some((last, carry)) = self.autoscroll else { return };
        let (position, speed) = match self.drag_at {
            Some(position) if self.selecting => (position, self.autoscroll_speed(position)),
            _ => (Default::default(), 0.),
        };
        if speed == 0. {
            self.autoscroll = None;
            return;
        }
        let now = Instant::now();
        let carry = carry + speed * now.duration_since(last).min(AUTOSCROLL_MAX_STEP).as_secs_f32();
        let lines = carry.trunc();
        if lines != 0. {
            self.term.lock().scroll_display(Scroll::Delta(lines as i32));
            self.extend_selection(position);
            cx.notify();
        }
        self.autoscroll = Some((now, carry - lines));
        cx.on_next_frame(window, |this, window, cx| this.autoscroll_step(window, cx));
    }

    fn mouse_up(&mut self, event: &MouseUpEvent, _: &mut Window, cx: &mut Context<Self>) {
        // A press the program saw gets its release, even if shift went down in between.
        if self.mouse_pressed.take().is_some() {
            if let Some(mode) = self.mouse_mode(false) {
                let button = report_button(event.button);
                self.report_mouse(mode, button, MouseAction::Release, event.position, &event.modifiers);
            }
            return;
        }
        if event.button != MouseButton::Left || !self.selecting {
            return;
        }
        self.selecting = false;
        self.drag_at = None;
        let selected = {
            let mut term = self.term.lock();
            if term.selection.as_ref().is_some_and(|s| s.is_empty()) {
                term.selection = None;
            }
            term.selection.is_some()
        };
        if let Some(link) = self.link_press.take()
            && !selected
            && self.link_under(event.position).as_ref() == Some(&link)
        {
            cx.open_url(&link.url);
        } else if self.pointer.copy_on_select && selected {
            self.copy_selection(cx);
        }
        cx.notify();
    }

    fn scroll_wheel(&mut self, event: &ScrollEvent, _: &mut Window, cx: &mut Context<Self>) {
        let (_, ch) = self.cell.get();
        let dy = f32::from(event.delta.pixel_delta(px(ch)).y);
        // Touchpads send many small deltas: keep the remainder so they add up to lines.
        if matches!(event.touch_phase, TouchPhase::Started) || dy * self.scroll_remainder < 0. {
            self.scroll_remainder = 0.;
        }
        self.scroll_remainder += dy;
        let lines = (self.scroll_remainder / ch).trunc() as i32;
        if lines == 0 {
            return;
        }
        self.scroll_remainder -= lines as f32 * ch;

        if let Some(mode) = self.mouse_mode(event.modifiers.shift) {
            let button = if lines > 0 { keys::MouseButton::WheelUp } else { keys::MouseButton::WheelDown };
            for _ in 0..lines.unsigned_abs().min(10) {
                self.report_mouse(mode, button, MouseAction::Press, event.position, &event.modifiers);
            }
            return;
        }
        let mode = *self.term.lock().mode();
        if mode.contains(TermMode::ALT_SCREEN) {
            // Full-screen programs (less, man) that did not ask for the mouse get cursor
            // keys, unless they turned that off (DECRST 1007).
            if mode.contains(TermMode::ALTERNATE_SCROLL) && !event.modifiers.shift && self.sink.is_some() {
                let bytes = keys::alternate_scroll_bytes(lines.clamp(-10, 10), mode.contains(TermMode::APP_CURSOR));
                self.write(bytes);
            }
        } else {
            self.scroll_view(Scroll::Delta(lines));
            cx.notify();
        }
    }

    /// Tell the program whether the terminal has focus, if it asked (DECSET 1004).
    fn report_focus(&mut self, focused: bool) {
        if self.focus_reported == Some(focused) {
            return;
        }
        self.focus_reported = Some(focused);
        if self.term.lock().mode().contains(TermMode::FOCUS_IN_OUT) {
            self.write(keys::focus_bytes(focused).to_vec());
        }
    }

    /// Scroll so the entered line sits near the top of the view, and light it up briefly.
    fn jump_to_mark(&mut self, id: u64, cx: &mut Context<Self>) {
        {
            let mut term = self.term.lock();
            if term.mode().contains(TermMode::ALT_SCREEN) {
                return;
            }
            let Some(line) = self.marks.rows(&term).into_iter().find_map(|(m, line)| (m == id).then_some(line)) else {
                return;
            };
            let history = term.grid().history_size() as i32;
            let target = (2 - line.0).clamp(0, history);
            let current = term.grid().display_offset() as i32;
            term.scroll_display(Scroll::Delta(target - current));
        }
        let epoch = self.flash.map_or(0, |(_, e)| e) + 1;
        self.flash = Some((id, epoch));
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(FLASH).await;
            let _ = this.update(cx, |this, cx| {
                if this.flash.is_some_and(|(_, e)| e == epoch) {
                    this.flash = None;
                    cx.notify();
                }
            });
        })
        .detach();
        cx.notify();
    }

    /// The command rail: one short line per entered command along the left edge, inside the
    /// terminal's padding so it takes no columns. Hidden until there is output to scroll back
    /// through. Returns the view row to light up, if any.
    fn render_rail(&mut self, cx: &mut Context<Self>) -> (Option<gpui::AnyElement>, Option<i32>) {
        if !self.command_rail || self.marks.list().is_empty() {
            return (None, None);
        }
        let (rows, offset, screen_rows) = {
            let term = self.term.lock();
            if term.mode().contains(TermMode::ALT_SCREEN) {
                return (None, None);
            }
            let grid = term.grid();
            if grid.history_size() == 0 {
                self.hovered_mark = None;
                return (None, None);
            }
            (self.marks.rows(&term), grid.display_offset() as i32, grid.screen_lines() as i32)
        };
        if rows.is_empty() {
            return (None, None);
        }
        let flash_row = self
            .flash
            .and_then(|(id, _)| rows.iter().find(|(m, _)| *m == id))
            .map(|(_, line)| line.0 + offset)
            .filter(|row| (0..screen_rows).contains(row));

        // The command being read: the newest one at the bottom, otherwise the last one that
        // starts in the top part of the view.
        let top = -offset;
        let current = if offset == 0 {
            rows.len() - 1
        } else {
            rows.iter().rposition(|(_, line)| line.0 <= top + (screen_rows / 4).max(2)).unwrap_or(0)
        };
        // Show as many as fit, keeping the current one in the window.
        let (_, cell_h) = self.cell.get();
        let fit = (((screen_rows as f32 * cell_h) * 0.85 / RAIL_STEP) as usize).max(3);
        let start = if rows.len() <= fit { 0 } else { current.saturating_sub(fit / 2).min(rows.len() - fit) };
        let end = (start + fit).min(rows.len());

        let appearance = cx.global::<Appearance>();
        let p = appearance.app;
        let mono = appearance.font_family.clone();
        let fg = rgb_to_hsla(appearance.term.foreground);
        let with_alpha = |a: f32| Hsla { a, ..fg };
        let hovered = self.hovered_mark;

        let ticks = rows[start..end].iter().enumerate().map(|(i, (id, _))| {
            let id = *id;
            let is_current = start + i == current;
            let is_hovered = hovered == Some(id);
            let card = is_hovered.then(|| self.marks.get(id)).flatten().map(|mark| {
                let (prompt, command) = mark.parts();
                let meta = if prompt.is_empty() {
                    mark.at.format("%H:%M").to_string()
                } else {
                    format!("{prompt} · {}", mark.at.format("%H:%M"))
                };
                div()
                    .absolute()
                    .left(px(PADDING_X + 4.))
                    .top(px(RAIL_STEP / 2. - 26.))
                    .min_w(px(200.))
                    .max_w(px(520.))
                    .px(px(14.))
                    .py(px(10.))
                    .rounded(p.corner(12.))
                    .bg(p.surface)
                    .border_1()
                    .border_color(p.border)
                    .shadow(crate::ui::widgets::elevation(p))
                    .flex()
                    .flex_col()
                    .gap(px(3.))
                    .child(
                        div()
                            .font_family(mono.clone())
                            .text_size(px(13.))
                            .text_color(p.text)
                            .whitespace_nowrap()
                            .overflow_hidden()
                            .text_ellipsis()
                            .child(command.to_string()),
                    )
                    .child(
                        div()
                            .font_family(p.font_family)
                            .text_size(px(12.))
                            .text_color(p.text_muted)
                            .whitespace_nowrap()
                            .overflow_hidden()
                            .text_ellipsis()
                            .child(meta),
                    )
            });
            div()
                .id(SharedString::from(format!("mark-{id}")))
                .relative()
                .w(px(PADDING_X))
                .h(px(RAIL_STEP))
                .flex()
                .items_center()
                .cursor_pointer()
                .on_hover(cx.listener(move |this, over: &bool, _, cx| {
                    if *over {
                        this.hovered_mark = Some(id);
                    } else if this.hovered_mark == Some(id) {
                        this.hovered_mark = None;
                    }
                    cx.notify();
                }))
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, _: &MouseDownEvent, window, cx| {
                        cx.stop_propagation();
                        window.focus(&this.focus, cx);
                        this.jump_to_mark(id, cx);
                    }),
                )
                .child(
                    div()
                        .ml(px(2.))
                        .h(px(2.))
                        .rounded(px(1.))
                        .w(px(if is_current || is_hovered { 9. } else { 7. }))
                        .bg(with_alpha(if is_hovered {
                            1.0
                        } else if is_current {
                            0.9
                        } else {
                            0.5
                        })),
                )
                .children(card)
        });
        let rail = div()
            .absolute()
            .left_0()
            .top_0()
            .bottom_0()
            .w(px(PADDING_X))
            .flex()
            .flex_col()
            .justify_center()
            .children(ticks)
            .into_any_element();
        (Some(rail), flash_row)
    }

    /// Whether a question is open: a connection's user name, password or host key, or a paste
    /// to confirm. The workspace shows it as a modal over the whole window (see `prompt_modal`).
    pub fn has_prompt(&self) -> bool {
        self.prompt.is_some()
    }

    /// The open question as a card, for the workspace's modal layer. It lives there rather than
    /// in the terminal so the rest of the window (sidebar, other dialogs) is blocked while it is
    /// open.
    pub fn prompt_modal(&self, cx: &mut Context<Self>) -> Option<gpui::AnyElement> {
        self.prompt_card(cx)
    }

    /// Escape on the open question: what its Cancel (or Close) button does.
    pub fn cancel_prompt(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        match self.prompt {
            Some(ActivePrompt::Input { .. }) => self.answer_input(false, window, cx),
            Some(ActivePrompt::HostKey { .. } | ActivePrompt::HostKeyChanged { .. }) => self.answer_host_key(false, window, cx),
            Some(_) => self.dismiss(window, cx),
            None => {}
        }
    }

    /// While connecting through Jump Hosts: the route page, with the current hop's question.
    fn render_route(&self, cx: &mut Context<Self>) -> Option<gpui::AnyElement> {
        let route = self.route.as_ref()?;
        let p = cx.global::<Appearance>().app;
        let hop = route.current();
        let actions = vec![
            button("route-edit", "Edit host", ButtonKind::Secondary, p)
                .on_click(cx.listener(move |_, _, _, cx| cx.emit(TerminalEvent::EditHost { hop })))
                .into_any_element(),
            button("route-retry", "Retry", ButtonKind::Primary, p)
                .on_click(cx.listener(|_, _, _, cx| cx.emit(TerminalEvent::Reconnect)))
                .into_any_element(),
        ];
        Some(route.render(actions, p))
    }

    /// The open question as a card.
    fn prompt_card(&self, cx: &mut Context<Self>) -> Option<gpui::AnyElement> {
        let p = cx.global::<Appearance>().app;
        let prompt = self.prompt.as_ref()?;
        let hop_name = self.route.as_ref().and_then(|r| r.hop_name(r.current()).map(str::to_string));
        let card = |title: &str, body: gpui::AnyElement, actions: Vec<gpui::AnyElement>| {
            div()
                .w(px(420.))
                .p(px(20.))
                .rounded(p.corner(14.))
                .bg(p.surface)
                .border_1()
                .border_color(p.border)
                .shadow(crate::ui::widgets::elevation(p))
                .flex()
                .flex_col()
                .gap(px(14.))
                .font_family(p.font_family)
                .child(div().text_size(px(15.)).font_weight(FontWeight::SEMIBOLD).text_color(p.text).child(title.to_string()))
                .child(body)
                .child(div().flex().justify_end().gap(px(8.)).children(actions))
                .into_any_element()
        };
        let text = |s: String| div().text_size(px(13.)).line_height(px(20.)).text_color(p.text_muted).child(s).into_any_element();
        let mono = |s: String| {
            div()
                .px(px(10.))
                .py(px(8.))
                .rounded(p.corner(8.))
                .bg(p.hover)
                .font_family(p.mono_family)
                .text_size(px(12.))
                .text_color(p.text)
                .child(s)
                .into_any_element()
        };
        let element = match prompt {
            ActivePrompt::HostKey { fingerprint, algorithm, other_types, .. } => card(
                &match &hop_name {
                    Some(name) => format!("Trust {name}?"),
                    None => "Trust this server?".to_string(),
                },
                div()
                    .flex()
                    .flex_col()
                    .gap(px(10.))
                    .child(text(if other_types.is_empty() {
                        format!("First connection to this host. Check that this {algorithm} key fingerprint matches the server before trusting it.")
                    } else {
                        format!(
                            "This PC already trusts a {} key for this host, but the server presented a {algorithm} key. That can happen when a server has keys of several types, but it can also mean someone is intercepting the connection. Check this fingerprint with the server before trusting it.",
                            other_types.join(" or ")
                        )
                    }))
                    .child(mono(fingerprint.clone()))
                    .into_any_element(),
                vec![
                    button("hk-cancel", "Cancel", ButtonKind::Ghost, p)
                        .on_click(cx.listener(|this, _, window, cx| this.answer_host_key(false, window, cx)))
                        .into_any_element(),
                    button("hk-trust", "Trust and connect", ButtonKind::Primary, p)
                        .on_click(cx.listener(|this, _, window, cx| this.answer_host_key(true, window, cx)))
                        .into_any_element(),
                ],
            ),
            ActivePrompt::HostKeyChanged { fingerprint, previous, algorithm, openssh, .. } => card(
                &match &hop_name {
                    Some(name) => format!("Host key of {name} changed"),
                    None => "Host key changed".to_string(),
                },
                div()
                    .flex()
                    .flex_col()
                    .gap(px(10.))
                    .child(text(format!(
                        "The server presented a different {algorithm} key than the one you trusted{}. Someone may be intercepting the connection, or the server was reinstalled. Only replace the key if you know why it changed, or after checking the new fingerprint with the server's administrator.",
                        if *openssh { " in your OpenSSH known_hosts" } else { "" }
                    )))
                    .when_some(previous.clone(), |el, previous| {
                        el.child(text("Trusted key:".into())).child(mono(previous))
                    })
                    .child(text("Key presented now:".into()))
                    .child(mono(fingerprint.clone()))
                    .when(*openssh, |el| {
                        el.child(text("Replacing trusts the new key in Komando only; your OpenSSH known_hosts is left as it is.".into()))
                    })
                    .into_any_element(),
                vec![
                    button("hkc-cancel", "Cancel", ButtonKind::Ghost, p)
                        .on_click(cx.listener(|this, _, window, cx| this.answer_host_key(false, window, cx)))
                        .into_any_element(),
                    button("hkc-replace", "Replace key and connect", ButtonKind::Danger, p)
                        .on_click(cx.listener(|this, _, window, cx| this.answer_host_key(true, window, cx)))
                        .into_any_element(),
                ],
            ),
            ActivePrompt::Input { label, secret, error, input, remember, .. } => card(
                label,
                div()
                    .flex()
                    .flex_col()
                    .gap(px(8.))
                    .child(if *secret { password_input(input, p).into_any_element() } else { Input::new(input).into_any_element() })
                    .when_some(error.clone(), |el, error| {
                        el.child(div().text_size(px(12.)).text_color(p.danger).child(error))
                    })
                    .when_some(*remember, |el, ticked| {
                        let view = cx.entity().downgrade();
                        el.child(crate::ui::widgets::labelled_checkbox("in-remember", "Remember for this host", ticked, p, move |clicked, window, cx| {
                            let _ = view.update(cx, |this, cx| this.toggle_remember(clicked, window, cx));
                        }))
                    })
                    .into_any_element(),
                vec![
                    button("in-cancel", "Cancel", ButtonKind::Ghost, p)
                        .on_click(cx.listener(|this, _, window, cx| this.answer_input(false, window, cx)))
                        .into_any_element(),
                    button("in-ok", "Continue", ButtonKind::Primary, p)
                        .on_click(cx.listener(|this, _, window, cx| this.answer_input(true, window, cx)))
                        .into_any_element(),
                ],
            ),
            ActivePrompt::ConfirmPaste { lines, .. } => card(
                &format!("Paste {lines} lines?"),
                text("Each line may run as a separate command on this machine.".into()),
                vec![
                    button("pp-cancel", "Cancel", ButtonKind::Ghost, p)
                        .on_click(cx.listener(|this, _, window, cx| this.dismiss(window, cx)))
                        .into_any_element(),
                    button("pp-ok", "Paste", ButtonKind::Primary, p)
                        .on_click(cx.listener(|this, _, window, cx| this.confirm_paste(window, cx)))
                        .into_any_element(),
                ],
            ),
        };
        Some(element)
    }

    fn render_disconnected(&self, cx: &Context<Self>) -> Option<gpui::AnyElement> {
        let Status::Disconnected(reason) = &self.status else { return None };
        // The route page shows where and why the connection failed; the output itself says
        // that a connected SSH Session ended (see `ssh::ConnectLog::ended`).
        if self.route.is_some() || (self.tells_end && self.was_connected) {
            return None;
        }
        let p = cx.global::<Appearance>().app;
        Some(
            div()
                .absolute()
                .bottom(px(16.))
                .left(px(16.))
                .right(px(16.))
                .flex()
                .justify_center()
                .child(
                    // A long reason (a failed key exchange lists every algorithm) wraps within
                    // the terminal's width, and past three lines ends in "…": the output above
                    // has it in full.
                    div()
                        .min_w_0()
                        .max_w_full()
                        .flex()
                        .items_center()
                        .gap(px(10.))
                        .px(px(14.))
                        .py(px(9.))
                        .rounded(p.corner(10.))
                        .bg(p.surface)
                        .border_1()
                        .border_color(p.border)
                        .shadow(crate::ui::widgets::elevation(p))
                        .font_family(p.font_family)
                        .text_size(px(13.))
                        .child(div().flex_shrink_0().size(px(8.)).rounded_full().bg(p.text_faint))
                        .child(
                            div()
                                .min_w_0()
                                .flex_shrink(1.)
                                .whitespace_normal()
                                .line_clamp(3)
                                .text_ellipsis()
                                .text_color(p.text)
                                .child(reason.clone()),
                        )
                        .child(div().flex_shrink_0().text_color(p.text_muted).child("Press Enter to reconnect")),
                )
                .into_any_element(),
        )
    }
}

fn report_button(button: MouseButton) -> keys::MouseButton {
    match button {
        MouseButton::Middle => keys::MouseButton::Middle,
        MouseButton::Right => keys::MouseButton::Right,
        _ => keys::MouseButton::Left,
    }
}

/// One cell copied out of the grid, so text layout runs without holding the terminal lock
/// (the parser keeps reading output meanwhile).
struct SnapCell {
    /// Row in the view and column.
    row: i32,
    col: usize,
    c: char,
    /// Zero-width characters on this cell (combining accents, variation selectors, ZWJ): where
    /// they are in the `Snapshot`'s `extras`.
    extra: Range<u32>,
    fg: Rgb,
    bg: Rgb,
    underline: Option<Rgb>,
    flags: Flags,
    selected: bool,
}

#[derive(Clone, Copy)]
struct CursorSnap {
    row: i32,
    col: usize,
    shape: CursorShape,
    wide: bool,
    color: Rgb,
}

/// Vertical text positions within a terminal row, shared by glyphs and decorations.
struct RowTextMetrics {
    baseline: Pixels,
    underline_y: Pixels,
    strike_y: Pixels,
}

impl RowTextMetrics {
    fn new(line_h: Pixels, ascent: Pixels, descent: Pixels) -> Self {
        // GPUI's Windows backend returns a signed descent below the baseline.
        // Row layout needs its magnitude, otherwise text shifts down relative
        // to the cell backgrounds and cursor, and underlines move above it.
        let descent = descent.abs();
        let padding_top = (line_h - ascent - descent) / 2.;
        Self {
            baseline: padding_top + ascent,
            underline_y: padding_top + ascent + descent * 0.618,
            strike_y: padding_top + ascent * 0.65,
        }
    }
}

/// Everything a row needs to be laid out, shared by all rows of a frame.
struct RowContext<'a> {
    shaper: &'a dyn Shaper,
    /// Regular, bold, italic, bold italic.
    fonts: [gpui::Font; 4],
    font_size: Pixels,
    origin: gpui::Point<Pixels>,
    cell_w: Pixels,
    line_h: Pixels,
    scale: f32,
    /// From the top of a row: the text baseline, underline and strikethrough.
    baseline: Pixels,
    underline_y: Pixels,
    strike_y: Pixels,
    highlighter: Option<&'a highlight::Highlighter>,
    dark: bool,
    default_bg: Rgb,
    selection: Hsla,
    selection_foreground: Rgb,
    cursor: Option<CursorSnap>,
    focused: bool,
}

/// Buffers `layout_row` reuses from row to row, so a frame does not allocate per row.
#[derive(Default)]
struct RowScratch {
    /// The row's text and where each cell starts in it, for highlight rules.
    text: String,
    starts: Vec<usize>,
    /// The style highlight rules give each byte of `text`.
    styles: Vec<Option<highlight::Style>>,
    /// (first column, end column, colour)
    backgrounds: Vec<(usize, usize, Rgb)>,
    selected: Vec<(usize, usize)>,
    prims: Vec<Prim>,
}

/// Bytes `start..end` of a row's text, coloured by a highlight rule.
#[derive(Clone, Copy, Debug, PartialEq)]
struct StyledSpan {
    start: u32,
    end: u32,
    style: highlight::Style,
}

/// Row texts a generation of the highlight cache grows to before it ages (see `Generations`):
/// a few screens.
const HIGHLIGHT_GENERATION: usize = 512;

/// What the highlight rules make of a row's text, worked out once per text. Output scrolling
/// the screen damages every row, so each frame lays out every row again; without this, every
/// rule ran on every row each frame although only the rows that came in were new.
#[derive(Default)]
struct HighlightCache {
    /// The rules and the background (dark or not) the entries were worked out for.
    key: Option<(ByAddress<highlight::Highlighter>, bool)>,
    rows: Generations<Box<str>, Box<[StyledSpan]>>,
    #[cfg(test)]
    scans: usize,
}

impl HighlightCache {
    /// Start a frame highlighted by `highlighter` on a dark or light background: forget
    /// everything if either changed, and keep the cache bounded.
    fn prepare(&mut self, highlighter: &Arc<highlight::Highlighter>, dark: bool) {
        if !self.key.as_ref().is_some_and(|(h, d)| Arc::ptr_eq(&h.0, highlighter) && *d == dark) {
            self.key = Some((ByAddress(highlighter.clone()), dark));
            self.rows.clear();
        }
        self.rows.age(HIGHLIGHT_GENERATION);
    }

    /// The coloured spans of `text`, from `work_out` if it isn't cached.
    fn spans(&mut self, text: &str, work_out: impl FnOnce() -> Box<[StyledSpan]>) -> &[StyledSpan] {
        let Generations { current, previous } = &mut self.rows;
        if !current.contains_key(text) {
            let spans = previous.remove(text).unwrap_or_else(|| {
                #[cfg(test)]
                { self.scans += 1; }
                work_out()
            });
            current.insert(text.into(), spans);
        }
        &current[text]
    }
}

/// Per-byte styles (see `Highlighter::styles_for_background_into`) as spans.
fn styled_spans(styles: &[Option<highlight::Style>]) -> Box<[StyledSpan]> {
    let mut spans: Vec<StyledSpan> = Vec::new();
    for (i, style) in styles.iter().enumerate() {
        let Some(style) = *style else { continue };
        match spans.last_mut() {
            Some(span) if span.end as usize == i && span.style == style => span.end += 1,
            _ => spans.push(StyledSpan { start: i as u32, end: i as u32 + 1, style }),
        }
    }
    spans.into()
}

#[derive(Debug, PartialEq)]
struct GlyphPaint {
    origin: gpui::Point<Pixels>,
    font_id: gpui::FontId,
    id: gpui::GlyphId,
    color: Hsla,
    emoji: bool,
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum DecorationKind {
    Underline,
    Double,
    Curly,
    Strike,
}

#[derive(Debug, PartialEq)]
struct Decoration {
    kind: DecorationKind,
    x0: Pixels,
    x1: Pixels,
    y: Pixels,
    color: Hsla,
}

/// One row of the grid laid out, ready to paint. Rows are kept from frame to frame in
/// `GridCache`, their buffers reused.
#[derive(Default, Debug, PartialEq)]
struct RowPaint {
    /// Backgrounds, selection and the block cursor, under the text.
    rects: Vec<(Bounds<Pixels>, Hsla)>,
    /// Box drawing and block elements.
    prims: Vec<(Prim, Hsla)>,
    glyphs: Vec<GlyphPaint>,
    decorations: Vec<Decoration>,
    /// Bar and underline cursors, over the text.
    cursor_bars: Vec<(Bounds<Pixels>, Hsla)>,
    cursor_outline: Option<(Bounds<Pixels>, Hsla)>,
}

impl RowPaint {
    fn clear(&mut self) {
        self.rects.clear();
        self.prims.clear();
        self.glyphs.clear();
        self.decorations.clear();
        self.cursor_bars.clear();
        self.cursor_outline = None;
    }
}

/// What a frame draws besides the rows (which `GridCache` holds).
#[derive(Default)]
struct Frame {
    /// The row of the Command Mark jumped to, lit under everything.
    flash: Option<(Bounds<Pixels>, Hsla)>,
    /// Search matches: over the cells' backgrounds and the selection, under the text.
    matches: Vec<(Bounds<Pixels>, Hsla)>,
    /// The current search match's edge, over the text.
    outlines: Vec<(Bounds<Pixels>, Hsla)>,
    /// What the IME is composing, over the cursor: its area, text and background.
    preedit: Option<(Bounds<Pixels>, ShapedLine, Hsla)>,
    /// The hovered link's underline: where each row of it starts, and how wide it is.
    links: Vec<(gpui::Point<Pixels>, Pixels)>,
    link_color: Hsla,
    font_size: Pixels,
}

/// Lay out one row: merged background runs, glyphs placed on their cells, procedurally
/// drawn line art, underlines and the cursor.
///
/// Glyphs are put on the grid one cell at a time instead of trusting the shaper's advances:
/// a character from a fallback font (CJK, emoji, symbols, Nerd Font icons) has its own
/// width, and would shift everything after it on the line out of its column.
#[allow(clippy::too_many_arguments)]
fn layout_row(
    cells: &[SnapCell],
    extras: &[char],
    row: i32,
    input: Option<InputRow>,
    ctx: &RowContext,
    glyphs: &mut GlyphCache,
    highlights: &mut HighlightCache,
    frame: &mut RowPaint,
    scratch: &mut RowScratch,
) {
    let y = ctx.origin.y + ctx.line_h * row as f32;
    let col_x = |col: usize| ctx.origin.x + ctx.cell_w * col as f32;
    let cell_width = |cell: &SnapCell| if cell.flags.contains(Flags::WIDE_CHAR) { 2 } else { 1 };
    let extra = |cell: &SnapCell| &extras[cell.extra.start as usize..cell.extra.end as usize];

    let RowScratch { text, starts, styles, backgrounds, selected, prims } = scratch;
    backgrounds.clear();
    selected.clear();
    // Highlight rules match on the row's text; build it only when there are rules. What the
    // user typed is left as the shell shows it.
    let styles = ctx
        .highlighter
        .filter(|_| input != Some(InputRow::All))
        .is_some_and(|h| {
            text.clear();
            starts.clear();
            for cell in cells {
                starts.push(text.len());
                text.push(cell.c);
                text.extend(extra(cell).iter());
            }
            // On a command line, rules see only the prompt, so the command can't colour it.
            let end = match input {
                Some(InputRow::AfterPrompt) => crate::marks::prompt_end(text).unwrap_or(text.len()),
                _ => text.len(),
            };
            let spans = highlights.spans(&text[..end], || {
                if h.styles_for_background_into(&text[..end], ctx.dark, styles) { styled_spans(styles) } else { Box::default() }
            });
            if spans.is_empty() {
                return false;
            }
            styles.clear();
            styles.resize(text.len(), None);
            for span in spans {
                styles[span.start as usize..span.end as usize].fill(Some(span.style));
            }
            true
        })
        .then_some(&*styles);

    let cursor_here = ctx.cursor.filter(|k| k.row == row);
    let block_cursor = ctx.focused && cursor_here.is_some_and(|k| k.shape == CursorShape::Block);

    for (i, cell) in cells.iter().enumerate() {
        let (mut fg, mut bg) = (cell.fg, cell.bg);
        let mut bold = cell.flags.contains(Flags::BOLD);
        if let Some(rule) = styles.and_then(|s| s[starts[i]]) {
            if let Some(c) = rule.fg {
                fg = Rgb { r: c.r, g: c.g, b: c.b };
            }
            if let Some(c) = rule.bg {
                bg = Rgb { r: c.r, g: c.g, b: c.b };
            }
            bold |= rule.bold;
        }
        if cell.selected { fg = ctx.selection_foreground; }
        let width = cell_width(cell);
        if block_cursor && cursor_here.is_some_and(|k| k.col == cell.col) {
            fg = bg;
            bg = cursor_here.map_or(bg, |k| k.color);
        }
        if bg != ctx.default_bg {
            match backgrounds.last_mut() {
                Some((_, end, color)) if *end == cell.col && *color == bg => *end += width,
                _ => backgrounds.push((cell.col, cell.col + width, bg)),
            }
        }
        if cell.selected {
            match selected.last_mut() {
                Some((_, end)) if *end == cell.col => *end += width,
                _ => selected.push((cell.col, cell.col + width)),
            }
        }

        let mut color = rgb_to_hsla(fg);
        if cell.flags.contains(Flags::DIM) && !cell.selected {
            color.a *= 0.66;
        }

        let x = col_x(cell.col);
        let mut drawn = false;
        if matches!(cell.c, '\u{2500}'..='\u{259f}' | '\u{e0b0}'..='\u{e0b3}') {
            let geometry = box_drawing::Cell {
                x: f32::from(x),
                y: f32::from(y),
                w: f32::from(ctx.cell_w) * width as f32,
                h: f32::from(ctx.line_h),
                scale: ctx.scale,
            };
            drawn = box_drawing::draw(cell.c, geometry, prims);
            frame.prims.extend(prims.drain(..).map(|p| (p, color)));
        }
        if !drawn && (cell.c != ' ' || !cell.extra.is_empty()) {
            let style = bold as usize + 2 * cell.flags.contains(Flags::ITALIC) as usize;
            for g in glyphs.get(ctx, style, cell.c, extra(cell)) {
                frame.glyphs.push(GlyphPaint {
                    origin: point(x + g.x, y + ctx.baseline + g.y),
                    font_id: g.font_id,
                    id: g.id,
                    color,
                    emoji: g.emoji,
                });
            }
        }

        let underline = if cell.flags.contains(Flags::UNDERCURL) {
            Some(DecorationKind::Curly)
        } else if cell.flags.contains(Flags::DOUBLE_UNDERLINE) {
            Some(DecorationKind::Double)
        } else if cell.flags.intersects(Flags::ALL_UNDERLINES) {
            Some(DecorationKind::Underline)
        } else {
            None
        };
        let strike = cell.flags.contains(Flags::STRIKEOUT).then_some(DecorationKind::Strike);
        for (kind, color) in [
            (underline, cell.underline.map_or(color, rgb_to_hsla)),
            (strike, color),
        ] {
            let Some(kind) = kind else { continue };
            let (x0, x1) = (x, col_x(cell.col + width));
            match frame.decorations.last_mut() {
                Some(d) if d.kind == kind && d.color == color && d.y == y && d.x1 == x0 => d.x1 = x1,
                _ => frame.decorations.push(Decoration { kind, x0, x1, y, color }),
            }
        }
    }

    for &(start, end, color) in backgrounds.iter() {
        let bounds = Bounds::new(point(col_x(start), y), size(ctx.cell_w * (end - start) as f32, ctx.line_h));
        frame.rects.push((bounds, rgb_to_hsla(color)));
    }
    for &(start, end) in selected.iter() {
        let bounds = Bounds::new(point(col_x(start), y), size(ctx.cell_w * (end - start) as f32, ctx.line_h));
        frame.rects.push((bounds, ctx.selection));
    }

    if let Some(k) = cursor_here {
        let width = ctx.cell_w * if k.wide { 2. } else { 1. };
        let bounds = Bounds::new(point(col_x(k.col), y), size(width, ctx.line_h));
        let color = rgb_to_hsla(k.color);
        let bar = px((2. * ctx.scale).round().max(1.) / ctx.scale);
        match k.shape {
            CursorShape::Block if !ctx.focused => frame.cursor_outline = Some((bounds, color)),
            CursorShape::Block => {} // drawn as the cell's background above
            CursorShape::HollowBlock => frame.cursor_outline = Some((bounds, color)),
            CursorShape::Beam => frame.cursor_bars.push((Bounds::new(bounds.origin, size(bar, ctx.line_h)), color)),
            CursorShape::Underline => frame.cursor_bars.push((
                Bounds::new(point(bounds.origin.x, y + ctx.line_h - bar), size(width, bar)),
                color,
            )),
            CursorShape::Hidden => {}
        }
    }
}

/// The cells on screen, copied out of the terminal.
#[derive(Default)]
struct Snapshot {
    /// A row at a time, from the left.
    cells: Vec<SnapCell>,
    /// The zero-width characters of all cells (see `SnapCell::extra`).
    extras: Vec<char>,
    /// The rows copied that are part of a command line, by row of the view.
    input: Vec<(i32, InputRow)>,
}

/// How much of a row is a command the user typed, which highlight rules leave alone: the
/// line at the cursor and lines entered earlier (Command Marks), after their prompt.
#[derive(Clone, Copy, PartialEq, Debug)]
enum InputRow {
    /// The first row of the line: what follows the prompt.
    AfterPrompt,
    /// A row the line wrapped onto: all of it.
    All,
}

impl Snapshot {
    /// Copy the cells of `rows` (rows of the view, top to bottom), colours resolved with
    /// `palette`, after what is copied already.
    fn copy_rows<T: alacritty_terminal::event::EventListener>(
        &mut self,
        term: &Term<T>,
        content: &alacritty_terminal::term::RenderableContent,
        palette: &crate::theme::TerminalPalette,
        rows: impl IntoIterator<Item = usize>,
    ) {
        let grid = term.grid();
        let colors = content.colors;
        let offset = view_offset(term) as i32;
        let selection = content.selection;
        let typing = typing_start(term);
        for row in rows {
            let line = Line(row as i32 - offset);
            if let Some(input) = input_row(term, line, typing) {
                self.input.push((row as i32, input));
            }
            let cells = &grid[line];
            for col in 0..grid.columns() {
                let cell = &cells[Column(col)];
                if cell.flags.contains(Flags::WIDE_CHAR_SPACER) {
                    continue;
                }
                let mut fg = palette.resolve(cell.fg, colors);
                let mut bg = palette.resolve(cell.bg, colors);
                if cell.flags.contains(Flags::INVERSE) {
                    std::mem::swap(&mut fg, &mut bg);
                }
                let hidden = cell.flags.contains(Flags::HIDDEN);
                let extra_start = self.extras.len() as u32;
                if let Some(extra) = cell.zerowidth().filter(|_| !hidden) {
                    self.extras.extend_from_slice(extra);
                }
                self.cells.push(SnapCell {
                    row: row as i32,
                    col,
                    c: if hidden || cell.c == '\t' { ' ' } else { cell.c },
                    extra: extra_start..self.extras.len() as u32,
                    fg,
                    bg,
                    underline: cell.underline_color().map(|c| palette.resolve(c, colors)),
                    flags: cell.flags,
                    selected: selection.is_some_and(|s| s.contains(GridPoint::new(line, Column(col)))),
                });
            }
        }
    }

    fn clear(&mut self) {
        self.cells.clear();
        self.extras.clear();
        self.input.clear();
    }

    fn input(&self, row: i32) -> Option<InputRow> {
        self.input.iter().find(|(r, _)| *r == row).map(|&(_, input)| input)
    }
}

/// The first row of the line at the cursor, when it is being typed at a prompt.
fn typing_start<T>(term: &Term<T>) -> Option<Line> {
    let grid = term.grid();
    let cursor = grid.cursor.point.line;
    (!term.mode().contains(TermMode::ALT_SCREEN) && cursor <= grid.bottommost_line())
        .then(|| crate::marks::logical_start(term, cursor))
        .filter(|&start| {
            let row = &grid[start];
            let text: String = (0..grid.columns()).map(|col| row[Column(col)].c).collect();
            crate::marks::prompt_end(&text).is_some()
        })
}

/// Whether row `line` is part of a command line (see `InputRow`): the one being typed, which
/// starts at `typing`, or one entered earlier.
fn input_row<T>(term: &Term<T>, line: Line, typing: Option<Line>) -> Option<InputRow> {
    let start = crate::marks::logical_start(term, line);
    (Some(start) == typing || crate::marks::is_marked(&term.grid()[start][Column(0)]))
        .then_some(if start == line { InputRow::AfterPrompt } else { InputRow::All })
}

/// A link in the output: the address it opens and the cells it covers, row by row (a long
/// one wraps onto several).
#[derive(Clone, Debug, PartialEq)]
struct TermLink {
    url: String,
    spans: Vec<(Line, Range<usize>)>,
}

/// Rows of a wrapped line read to find a link in it, either way from the pointer.
const LINK_ROWS: i32 = 16;

/// The link at `at`: one a program marked (OSC 8) with a web address, or an address with its
/// scheme written in the text (see `crate::links`), read across rows a long line wrapped onto.
fn link_at<T>(term: &Term<T>, at: GridPoint) -> Option<TermLink> {
    let grid = term.grid();
    let cols = grid.columns();
    if cols == 0 || at.line < grid.topmost_line() || at.line > grid.bottommost_line() || at.column.0 >= cols {
        return None;
    }
    let row = &grid[at.line];
    if let Some(link) = row[at.column].hyperlink() {
        if !crate::links::is_openable(link.uri()) {
            return None;
        }
        let same = |col: usize| row[Column(col)].hyperlink().as_ref() == Some(&link);
        let (mut start, mut end) = (at.column.0, at.column.0 + 1);
        while start > 0 && same(start - 1) {
            start -= 1;
        }
        while end < cols && same(end) {
            end += 1;
        }
        return Some(TermLink { url: link.uri().trim().to_string(), spans: vec![(at.line, start..end)] });
    }
    let wraps = |line: Line| grid[line][Column(cols - 1)].flags.contains(Flags::WRAPLINE);
    let mut first = at.line;
    while first > grid.topmost_line() && at.line.0 - first.0 < LINK_ROWS && wraps(Line(first.0 - 1)) {
        first = Line(first.0 - 1);
    }
    let mut last = at.line;
    while last < grid.bottommost_line() && last.0 - at.line.0 < LINK_ROWS && wraps(last) {
        last = Line(last.0 + 1);
    }
    // The line's text, and where each cell's character starts in it.
    let mut text = String::new();
    let mut cells: Vec<(usize, Line, usize, usize)> = vec![];
    let mut byte = None;
    for line in first.0..=last.0 {
        let line = Line(line);
        for col in 0..cols {
            let cell = &grid[line][Column(col)];
            if cell.flags.contains(Flags::WIDE_CHAR_SPACER) {
                continue;
            }
            if line == at.line && col <= at.column.0 {
                byte = Some(text.len());
            }
            let width = if cell.flags.contains(Flags::WIDE_CHAR) { 2 } else { 1 };
            cells.push((text.len(), line, col, width));
            let hidden = cell.flags.contains(Flags::HIDDEN);
            text.push(if hidden || cell.c == '\t' { ' ' } else { cell.c });
            if let Some(extra) = cell.zerowidth().filter(|_| !hidden) {
                text.extend(extra.iter());
            }
        }
    }
    let range = crate::links::link_at(&text, byte?)?;
    let mut spans: Vec<(Line, Range<usize>)> = vec![];
    for &(_, line, col, width) in cells.iter().filter(|(b, ..)| range.contains(b)) {
        match spans.last_mut() {
            Some((l, cols)) if *l == line => cols.end = col + width,
            _ => spans.push((line, col..col + width)),
        }
    }
    Some(TermLink { url: text[range].to_string(), spans })
}

/// The highlight rule colouring the cell at `at`, read the way rows are drawn (`layout_row`).
fn highlighted_at<T>(term: &Term<T>, highlighter: &highlight::Highlighter, at: GridPoint) -> Option<highlight::RuleHit> {
    let grid = term.grid();
    if term.mode().contains(TermMode::ALT_SCREEN) || at.line < grid.topmost_line() || at.line > grid.bottommost_line() {
        return None;
    }
    let row = &grid[at.line];
    let mut text = String::new();
    let mut byte = None;
    for col in 0..grid.columns() {
        let cell = &row[Column(col)];
        if cell.flags.contains(Flags::WIDE_CHAR_SPACER) {
            continue;
        }
        if col <= at.column.0 {
            byte = Some(text.len());
        }
        let hidden = cell.flags.contains(Flags::HIDDEN);
        text.push(if hidden || cell.c == '\t' { ' ' } else { cell.c });
        if let Some(extra) = cell.zerowidth().filter(|_| !hidden) {
            text.extend(extra.iter());
        }
    }
    let byte = byte?;
    let end = match input_row(term, at.line, typing_start(term)) {
        Some(InputRow::All) => return None,
        Some(InputRow::AfterPrompt) => crate::marks::prompt_end(&text).unwrap_or(text.len()),
        None => text.len(),
    };
    (byte < end).then(|| highlighter.rule_at(&text[..end], byte)).flatten()
}

/// `text` cut to `max` characters, with an ellipsis when it was longer.
fn ellipsize(text: &str, max: usize) -> String {
    let text = text.trim();
    match text.char_indices().nth(max) {
        Some((i, _)) => format!("{}…", &text[..i]),
        None => text.to_string(),
    }
}

/// How far the view is scrolled up into the Scrollback.
///
/// Not always the terminal's display offset: while the view is scrolled up, a program scrolling
/// a region that doesn't start at the top raises the offset without adding to the Scrollback
/// (alacritty_terminal 0.26), and the view would start above the oldest line.
fn view_offset<T>(term: &Term<T>) -> usize {
    term.grid().display_offset().min(term.grid().history_size())
}

/// The cursor, if it is shown in the view.
fn cursor_snap<T: alacritty_terminal::event::EventListener>(
    term: &Term<T>,
    content: &alacritty_terminal::term::RenderableContent,
    palette: &crate::theme::TerminalPalette,
) -> Option<CursorSnap> {
    let point = content.cursor.point;
    let row = point.line.0 + view_offset(term) as i32;
    (content.cursor.shape != CursorShape::Hidden && (0..term.screen_lines() as i32).contains(&row)).then(|| CursorSnap {
        row,
        col: point.column.0,
        shape: content.cursor.shape,
        wide: term.grid()[point].flags.contains(Flags::WIDE_CHAR),
        color: palette.resolve(Color::Named(NamedColor::Cursor), content.colors),
    })
}

/// The terminal font at one size: what the grid is measured and laid out with.
struct GridFont {
    family: SharedString,
    size: Pixels,
    /// Regular, bold, italic, bold italic.
    fonts: [gpui::Font; 4],
    cell_w: Pixels,
    line_h: Pixels,
    metrics: RowTextMetrics,
}

impl GridFont {
    /// `weights`: of regular and of bold text.
    fn new(text_system: &gpui::WindowTextSystem, family: SharedString, size: Pixels, weights: [FontWeight; 2]) -> Self {
        let mut regular = gpui::font(family);
        regular.weight = weights[0];
        regular.features = gpui::FontFeatures::disable_ligatures();
        let font_id = text_system.resolve_font(&regular);
        let cell_w = text_system.advance(font_id, size, 'm').map(|s| s.width).unwrap_or(px(8.));
        let line_h = (size * 1.32).round();
        let metrics = RowTextMetrics::new(line_h, text_system.ascent(font_id, size), text_system.descent(font_id, size));
        Self::with_metrics(regular, weights[1], size, cell_w, line_h, metrics)
    }

    fn with_metrics(regular: gpui::Font, bold_weight: FontWeight, size: Pixels, cell_w: Pixels, line_h: Pixels, metrics: RowTextMetrics) -> Self {
        let bold = |mut f: gpui::Font| {
            f.weight = bold_weight;
            f
        };
        let italic = |mut f: gpui::Font| {
            f.style = FontStyle::Italic;
            f
        };
        let family = regular.family.clone();
        let fonts = [regular.clone(), bold(regular.clone()), italic(regular.clone()), bold(italic(regular))];
        Self { family, size, fonts, cell_w, line_h, metrics }
    }
}

/// How the grid is drawn this frame, apart from what is in the terminal.
struct GridLook<'a> {
    font: &'a GridFont,
    /// The top left corner of the grid's first cell.
    origin: gpui::Point<Pixels>,
    scale: f32,
    palette: &'a crate::theme::TerminalPalette,
    highlighter: &'a Arc<highlight::Highlighter>,
    focused: bool,
    cursor_visible: bool,
}

/// What the grid keeps from one frame to the next: the rows laid out, and buffers reused.
///
/// A row is laid out again only when something it shows has changed: its cells (the terminal's
/// damage), the selection on it, or the cursor on it or leaving it. Anything else that changes
/// how rows look (font, theme, highlight rules, size, position, scrolling) lays out every row.
///
/// The view paints its rows from here, so each view draws one grid per frame (as its `origin`
/// and `cell` already assume).
#[derive(Default)]
struct GridCache {
    glyphs: GlyphCache,
    highlights: HighlightCache,
    snapshot: Snapshot,
    scratch: RowScratch,
    /// The rows of the view, laid out.
    rows: Vec<RowPaint>,
    /// What `rows` were laid out with, besides the cells (`None` until the first frame).
    looked: Option<LaidOutWith>,
    /// The selection and the row of the cursor that `rows` show.
    selection: Option<SelectionRange>,
    cursor_row: Option<usize>,
    /// The rows to lay out this frame.
    dirty: Vec<bool>,
}

/// What every row's layout depends on besides its cells, the selection and the cursor.
#[derive(PartialEq)]
struct LaidOutWith {
    family: SharedString,
    font_size: Pixels,
    cell_w: Pixels,
    line_h: Pixels,
    origin: gpui::Point<Pixels>,
    scale: f32,
    palette: crate::theme::TerminalPalette,
    /// The rules colouring output, `None` while highlighting is off; compared by identity
    /// (and held, so the address cannot come back with other rules).
    highlighter: Option<ByAddress<highlight::Highlighter>>,
    columns: usize,
    lines: usize,
    /// How far the view is scrolled up (see `view_offset`).
    offset: usize,
}

/// An `Arc` compared by what it points to, not by value.
struct ByAddress<T>(Arc<T>);

impl<T> PartialEq for ByAddress<T> {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}

/// Lay out the grid for one frame into `cache.rows`: copy what changed on screen and let go of
/// the terminal, then lay out those rows (the parser keeps reading output meanwhile).
///
/// This is the only reader of the terminal's damage, and resets it: the rows of a `GridCache`
/// are only right if nothing else does.
fn layout_grid(term: &SharedTerm, shaper: &dyn Shaper, look: &GridLook, cache: &mut GridCache) {
    let GridCache { glyphs, highlights, snapshot, scratch, rows, looked, selection, cursor_row, dirty } = cache;
    let font = look.font;
    snapshot.clear();
    let (cursor, highlight) = {
        let mut term = term.lock();
        let lines = term.screen_lines();
        dirty.clear();
        dirty.resize(lines, false);
        // Rows the terminal changed since the last frame (it covers the cursor's moves too).
        let mut all = match term.damage() {
            TermDamage::Full => true,
            TermDamage::Partial(damaged) => {
                for line in damaged {
                    if let Some(row) = dirty.get_mut(line.line) {
                        *row = true;
                    }
                }
                false
            }
        };
        term.reset_damage();
        let content = term.renderable_content();
        let offset = view_offset(&*term);
        // The damage is for the terminal's own offset (see `view_offset`).
        all |= offset != content.display_offset;
        // Highlight rules colour plain output; full-screen programs (vim, htop) only when asked.
        let full_screen_ok = look.highlighter.full_screen || !content.mode.contains(TermMode::ALT_SCREEN);
        let highlight = (full_screen_ok && look.highlighter.rule_count() > 0)
            .then(|| look.highlighter.clone());
        let now = LaidOutWith {
            family: font.family.clone(),
            font_size: font.size,
            cell_w: font.cell_w,
            line_h: font.line_h,
            origin: look.origin,
            scale: look.scale,
            palette: look.palette.clone(),
            highlighter: highlight.clone().map(ByAddress),
            columns: term.columns(),
            lines,
            offset,
        };
        if looked.as_ref() != Some(&now) {
            *looked = Some(now);
            all = true;
        }
        let cursor = cursor_snap(&term, &content, look.palette).filter(|_| look.cursor_visible);
        if all {
            dirty.fill(true);
        } else {
            // The selection isn't part of the damage: the rows it covered and covers.
            if content.selection != *selection {
                for range in [*selection, content.selection].into_iter().flatten() {
                    let first = (range.start.line.0 + offset as i32).max(0);
                    let last = (range.end.line.0 + offset as i32).min(lines as i32 - 1);
                    for row in first..=last {
                        dirty[row as usize] = true;
                    }
                }
            }
            // Nor is the cursor's look (shape, colour, focus): the row it was drawn on, and its row.
            for row in [*cursor_row, cursor.map(|k| k.row as usize)].into_iter().flatten() {
                if let Some(row) = dirty.get_mut(row) {
                    *row = true;
                }
            }
        }
        *selection = content.selection;
        *cursor_row = cursor.map(|k| k.row as usize);
        rows.resize_with(lines, RowPaint::default);
        let changed = dirty.iter().enumerate().filter_map(|(row, dirty)| dirty.then_some(row));
        snapshot.copy_rows(&term, &content, look.palette, changed);
        (cursor, highlight)
    };
    let ctx = RowContext {
        shaper,
        fonts: font.fonts.clone(),
        font_size: font.size,
        origin: look.origin,
        cell_w: font.cell_w,
        line_h: font.line_h,
        scale: look.scale,
        baseline: font.metrics.baseline,
        underline_y: font.metrics.underline_y,
        strike_y: font.metrics.strike_y,
        highlighter: highlight.as_deref(),
        dark: look.palette.dark,
        default_bg: look.palette.background,
        selection: look.palette.selection,
        selection_foreground: look.palette.selection_foreground,
        cursor,
        focused: look.focused,
    };
    glyphs.prepare(&font.family, font.size, [font.fonts[0].weight, font.fonts[1].weight]);
    if let Some(highlighter) = &highlight {
        highlights.prepare(highlighter, look.palette.dark);
    }
    let cells = &snapshot.cells;
    let mut start = 0;
    while start < cells.len() {
        let row = cells[start].row;
        let end = cells[start..].iter().position(|c| c.row != row).map_or(cells.len(), |n| start + n);
        let paint = &mut rows[row as usize];
        paint.clear();
        let input = snapshot.input(row);
        layout_row(&cells[start..end], &snapshot.extras, row, input, &ctx, glyphs, highlights, paint, scratch);
        for d in &mut paint.decorations {
            d.y += if d.kind == DecorationKind::Strike { ctx.strike_y } else { ctx.underline_y };
        }
        start = end;
    }
}

/// One glyph of a shaped cell, relative to the cell's left edge and the baseline.
#[derive(Clone, Copy)]
struct CachedGlyph {
    font_id: gpui::FontId,
    id: gpui::GlyphId,
    x: Pixels,
    y: Pixels,
    emoji: bool,
}

/// Clusters of up to this many characters (a base character and the zero-width ones on its cell)
/// are looked up without allocating. Longer ones, such as family emoji joined with ZWJ, are rare.
const INLINE_CLUSTER: usize = 8;

/// Entries a generation of the glyph cache grows to before it ages (see `Generations`). Two
/// generations are kept, so this many entries or more are always there: a screen in four styles
/// with thousands of different characters, CJK or combining marks fits many times over.
const GLYPH_GENERATION: usize = 16_384;

/// A cluster and its style, inline.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
struct ClusterKey {
    style: u8,
    len: u8,
    chars: [char; INLINE_CLUSTER],
}

/// Cache entries in two generations: those used since the cache last aged, and those before.
/// Aging drops the older generation and starts a new one, so whatever was used in the last
/// frame is still there in the next, however many it was: a screen with more different
/// characters than a generation holds is never shaped again frame after frame.
struct Generations<K, V = Box<[CachedGlyph]>> {
    current: FxHashMap<K, V>,
    previous: FxHashMap<K, V>,
}

impl<K, V> Default for Generations<K, V> {
    fn default() -> Self {
        Self { current: FxHashMap::default(), previous: FxHashMap::default() }
    }
}

impl<K: Hash + Eq, V> Generations<K, V> {
    /// Start a new generation once the current one has more than `limit` entries.
    fn age(&mut self, limit: usize) {
        if self.current.len() > limit {
            // The old generation's table is cleared and reused for the new one.
            std::mem::swap(&mut self.current, &mut self.previous);
            self.current.clear();
        }
    }

    fn clear(&mut self) {
        self.current.clear();
        self.previous.clear();
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        self.current.len() + self.previous.len()
    }
}

/// The glyphs for each character (or base character with its combining marks) in each style,
/// shaped once and reused. Text is placed cell by cell, so shaping whole rows every frame
/// would be thrown away; during heavy output, when every row is new each frame, that
/// shaping was most of the time spent drawing.
///
/// Whatever shaping gives is kept, including a character no font has (drawn as the missing
/// glyph): it is not looked for in the fonts again.
struct GlyphCache {
    /// The font family, size, and regular and bold weights the entries were shaped with.
    font: Option<(SharedString, Pixels, [FontWeight; 2])>,
    /// ASCII characters in each style, by style and code: most cells of most screens.
    ascii: Box<[Option<Box<[CachedGlyph]>>]>,
    clusters: Generations<ClusterKey>,
    /// Clusters longer than `INLINE_CLUSTER`, the style first (as a character).
    long: Generations<Box<[char]>>,
    /// Where a long cluster's key is put together for a lookup.
    long_key: Vec<char>,
    /// Entries a generation grows to before it ages.
    limit: usize,
}

impl Default for GlyphCache {
    fn default() -> Self {
        Self {
            font: None,
            ascii: std::iter::repeat_with(|| None).take(4 * 128).collect(),
            clusters: Generations::default(),
            long: Generations::default(),
            long_key: Vec::new(),
            limit: GLYPH_GENERATION,
        }
    }
}

impl GlyphCache {
    /// Start a frame: forget everything if the font changed, and keep the cache bounded.
    fn prepare(&mut self, family: &SharedString, size: Pixels, weights: [FontWeight; 2]) {
        if self.font.as_ref().is_none_or(|(f, s, w)| f != family || *s != size || *w != weights) {
            self.font = Some((family.clone(), size, weights));
            self.ascii.iter_mut().for_each(|slot| *slot = None);
            self.clusters.clear();
            self.long.clear();
        }
        self.clusters.age(self.limit);
        self.long.age(self.limit / 16);
    }

    /// The glyphs of a cell's text (`c` and the zero-width characters on its cell) in a style,
    /// shaped if they aren't cached yet.
    fn get(&mut self, ctx: &RowContext, style: usize, c: char, extra: &[char]) -> &[CachedGlyph] {
        self.get_with(style, c, extra, |text| shape_cluster(ctx, style, text))
    }

    fn get_with(
        &mut self,
        style: usize,
        c: char,
        extra: &[char],
        shape: impl FnOnce(&str) -> Box<[CachedGlyph]>,
    ) -> &[CachedGlyph] {
        let text = || std::iter::once(c).chain(extra.iter().copied()).collect::<String>();
        if extra.is_empty() && c.is_ascii() {
            return self.ascii[style * 128 + c as usize].get_or_insert_with(|| shape(c.encode_utf8(&mut [0; 4])));
        }
        let len = 1 + extra.len();
        if len <= INLINE_CLUSTER {
            let mut chars = ['\0'; INLINE_CLUSTER];
            chars[0] = c;
            chars[1..len].copy_from_slice(extra);
            let key = ClusterKey { style: style as u8, len: len as u8, chars };
            let Generations { current, previous } = &mut self.clusters;
            return match current.entry(key) {
                Entry::Occupied(entry) => entry.into_mut(),
                Entry::Vacant(entry) => entry.insert(previous.remove(&key).unwrap_or_else(|| shape(&text()))),
            };
        }
        self.long_key.clear();
        self.long_key.push(char::from(style as u8));
        self.long_key.push(c);
        self.long_key.extend_from_slice(extra);
        let Generations { current, previous } = &mut self.long;
        if !current.contains_key(self.long_key.as_slice()) {
            let glyphs = previous.remove(self.long_key.as_slice()).unwrap_or_else(|| shape(&text()));
            current.insert(self.long_key.as_slice().into(), glyphs);
        }
        &current[self.long_key.as_slice()]
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        self.ascii.iter().flatten().count() + self.clusters.len() + self.long.len()
    }
}

/// Shapes the text of a cell: GPUI's text system, or a stand-in in tests.
trait Shaper {
    /// The glyphs of `text`, placed relative to the first one.
    fn shape(&self, font: &gpui::Font, size: Pixels, text: &str) -> Box<[CachedGlyph]>;
}

impl Shaper for gpui::WindowTextSystem {
    fn shape(&self, font: &gpui::Font, size: Pixels, text: &str) -> Box<[CachedGlyph]> {
        let run = TextRun {
            len: text.len(),
            font: font.clone(),
            color: Hsla::black(),
            background_color: None,
            underline: None,
            strikethrough: None,
        };
        let layout = self.layout_line(text, size, &[run], None);
        let base = layout.runs.iter().flat_map(|r| r.glyphs.first()).next().map_or(px(0.), |g| g.position.x);
        layout
            .runs
            .iter()
            .flat_map(|r| {
                r.glyphs.iter().map(move |g| CachedGlyph {
                    font_id: r.font_id,
                    id: g.id,
                    x: g.position.x - base,
                    y: g.position.y,
                    emoji: g.is_emoji,
                })
            })
            .collect()
    }
}

/// Shape one cell's text in a style.
fn shape_cluster(ctx: &RowContext, style: usize, text: &str) -> Box<[CachedGlyph]> {
    #[cfg(test)]
    SHAPED.with(|n| n.set(n.get() + 1));
    ctx.shaper.shape(&ctx.fonts[style], ctx.font_size, text)
}

#[cfg(test)]
thread_local! {
    /// Clusters shaped on this thread, for tests and benchmarks of the glyph cache.
    static SHAPED: Cell<usize> = const { Cell::new(0) };
}

#[cfg(test)]
fn shaped_count() -> usize {
    SHAPED.with(Cell::get)
}

fn paint_prim(window: &mut Window, prim: &Prim, color: Hsla) {
    let p = |(x, y): (f32, f32)| point(px(x), px(y));
    let path = match *prim {
        Prim::Rect { x, y, w, h, alpha } => {
            window.paint_quad(fill(Bounds::new(p((x, y)), size(px(w), px(h))), Hsla { a: color.a * alpha, ..color }));
            return;
        }
        Prim::Triangle([a, b, c]) => {
            let mut path = PathBuilder::fill();
            path.move_to(p(a));
            path.line_to(p(b));
            path.line_to(p(c));
            path.close();
            path
        }
        Prim::Chevron { points: [a, b, c], width } => {
            let mut path = PathBuilder::stroke(px(width));
            path.move_to(p(a));
            path.line_to(p(b));
            path.line_to(p(c));
            path
        }
        Prim::Arc { from, start, corner, end, to, width } => {
            // A cubic Bézier with control points 55% of the way to the corner is a close
            // match for a quarter circle.
            const K: f32 = 0.5523;
            let toward = |a: (f32, f32)| (a.0 + (corner.0 - a.0) * K, a.1 + (corner.1 - a.1) * K);
            let mut path = PathBuilder::stroke(px(width));
            path.move_to(p(from));
            path.line_to(p(start));
            path.cubic_bezier_to(p(end), p(toward(start)), p(toward(end)));
            path.line_to(p(to));
            path
        }
    };
    if let Ok(path) = path.build() {
        window.paint_path(path, color);
    }
}

impl Render for TerminalView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        crate::perf::count("terminal render");
        let appearance = cx.global::<Appearance>();
        let palette = appearance.term.clone();
        let family = appearance.font_family.clone();
        let font_size = px(appearance.font_size);
        let weights = [appearance.font_weight, appearance.bold_font_weight];
        let now = Instant::now();
        let minimap_state = self.minimap_state.clone();
        if !self.minimap {
            minimap_state.hide();
            minimap_state.release();
        } else if !self.minimap_overlay {
            minimap_state.hide();
        } else {
            if !window.is_window_active() || (!window.is_window_hovered() && !minimap_state.dragging()) {
                minimap_state.close(now);
            }
            minimap_state.set_still(cx.reduce_motion());
            if minimap_state.animating(now) {
                window.request_animation_frame();
            }
        }
        let focused = self.focus.is_focused(window) && self.prompt.is_none() && window.is_window_active();
        // A dropped Session takes no input: no cursor waits for any.
        let cursor_visible = !self.is_disconnected() && self.cursor_visible(focused, cx);
        // Colour queries (OSC 10/11/4) are answered with the colours on screen.
        self.proxy.set_palette(&palette);
        let mouse_reporting = self.mouse_mode(false).is_some();
        // The hovered link's cells, as rows of the view.
        let link_marks: Vec<(i32, Range<usize>)> = match &self.hover_link {
            Some(link) => {
                let offset = self.term.lock().grid().display_offset() as i32;
                link.spans.iter().map(|(line, cols)| (line.0 + offset, cols.clone())).collect()
            }
            None => vec![],
        };
        let link_pointer = self.hover_link.is_some() && self.link_key_held(&window.modifiers());
        let watch_ctrl = self.hover_link.is_some();

        let term = self.term.clone();
        let sink = self.sink.clone();
        let proxy = self.proxy.clone();
        let grid = self.grid.clone();
        let reflow = self.reflow.clone();
        let width_preparation = self.width_preparation.clone();
        let this = cx.entity().downgrade();
        let this_drag = this.clone();
        let this_hover = this.clone();
        let hover_entity = this.entity_id();
        let hover_state = self.minimap_state.clone();
        let exit_state = self.minimap_state.clone();
        let bounds_state = self.minimap_state.clone();
        let origin_cell = self.origin.clone();
        let cell_cell = self.cell.clone();
        let grid_cache = self.grid_cache.clone();
        let paint_cache = self.grid_cache.clone();
        let marked = self.marked.clone();
        let input_view = cx.entity();
        let input_focus = self.focus.clone();
        let highlighter = self.highlighter(cx);
        let (rail, flash_row) = crate::perf::time("command rail", || self.render_rail(cx));
        let (search_marks, minimap_marks) = crate::perf::time("search highlights", || self.search_view());
        // Beside the output it takes a column (and the right padding: it has its own); over the
        // output it is laid on top, or is just the handle in the right padding.
        let beside = self.minimap && !self.minimap_overlay;
        let pad_right = if beside { 0. } else { PADDING_X };
        let app = cx.global::<Appearance>().app;
        let caps_lock = window.capslock().on;
        let password_prompt = focused && self.status == Status::Connected && {
            let term = self.term.lock();
            !term.mode().contains(TermMode::ALT_SCREEN) && {
                let row = &term.grid()[term.grid().cursor.point.line];
                let line: String = (0..term.columns()).map(|c| row[Column(c)].c).collect();
                asks_for_secret(&line)
            }
        };
        let (minimap_column, minimap_overlay) = if !self.minimap {
            // No minimap: a plain scrollbar in the right padding instead.
            let bar = crate::ui::minimap::scrollbar(self.term.clone(), minimap_state, app, (PADDING_X, PADDING_Y), minimap_marks, cx);
            (None, bar)
        } else if beside {
            let column = crate::ui::minimap::beside(
                self.term.clone(),
                minimap_state,
                palette.clone(),
                app,
                highlighter.clone(),
                minimap_marks,
                cx,
            );
            (Some(column), None)
        } else {
            let overlay = crate::ui::minimap::over(
                self.term.clone(),
                minimap_state,
                palette.clone(),
                app,
                highlighter.clone(),
                (PADDING_X, PADDING_Y),
                minimap_marks,
                cx,
            );
            (None, overlay)
        };
        let flash_color = Hsla { a: palette.selection.a * 0.7, ..palette.selection };
        let search_colors = (Hsla { a: 0.3, ..app.brand }, Hsla { a: 0.6, ..app.brand }, app.brand);
        let search_bar = self.render_search_bar(cx);

        let surface = canvas(
            move |bounds, window, cx| crate::perf::time("terminal grid prepaint", || {
                let text_system = window.text_system().clone();
                let font = GridFont::new(&text_system, family.clone(), font_size, weights);
                let (cell_w, line_h) = (font.cell_w, font.line_h);
                let cw = f32::from(cell_w);
                let lh = f32::from(line_h);
                origin_cell.set(bounds.origin);
                cell_cell.set((cw, lh));

                let cols = ((f32::from(bounds.size.width) / cw).floor() as u16).max(2);
                let rows = ((f32::from(bounds.size.height) / lh).floor() as u16).max(1);
                let mut new_size = GridSize { cols, rows, cell_width: cw.round() as u16, cell_height: lh as u16 };
                let old_size = grid.get();
                // Collapsing the sidebar widens every Scrollback row. Prepare their allocations
                // in short batches off the UI thread and paint at the old column count meanwhile.
                let preparing = if let Some(old) = old_size.filter(|old| old.cols < cols)
                    && term.lock().grid().history_size() >= PREPARE_WIDTH_HISTORY
                {
                    let mut preparation = width_preparation.borrow_mut();
                    if let Some((prepared_cols, cancelled)) = preparation.request(cols) {
                        let this = this.clone();
                        cx.defer(move |cx| {
                            let _ = this.update(cx, |view, cx| view.prepare_width(old.cols, prepared_cols, cancelled, cx));
                        });
                    }
                    !preparation.ready(cols)
                } else {
                    let abandoned = width_preparation.borrow_mut().pending.take().is_some_and(|p| {
                        !p.ready || old_size.is_some_and(|old| old.cols != p.cols)
                    });
                    if abandoned && let Some(old) = old_size {
                        // A rapid toggle can cancel preparation without changing the grid's
                        // width. Reclaim its unused capacity once the layout settles anyway.
                        let this = this.clone();
                        cx.defer(move |cx| {
                            let _ = this.update(cx, |view, cx| view.compact_after_resize(old.cols, cx));
                        });
                    }
                    false
                };
                // A new width may have to wait (see `Reflow`); a new height never does.
                let now = Instant::now();
                let wait = old_size.filter(|old| old.cols != new_size.cols).and_then(|_| reflow.get().wait(now));
                if let (Some(_), Some(old)) = (wait, old_size) {
                    new_size.cols = old.cols;
                }
                if preparing && let Some(old) = old_size {
                    new_size.cols = old.cols;
                }
                if let Some(wait) = wait
                    && !reflow.get().woken
                {
                    reflow.set(Reflow { woken: true, ..reflow.get() });
                    let this = this.clone();
                    cx.defer(move |cx| {
                        let _ = this.update(cx, |view, cx| view.repaint_after(wait, cx));
                    });
                }
                if old_size != Some(new_size) {
                    // A new width leaves the Scrollback's rows oversized; the first layout has none.
                    let width_changed = old_size.is_some_and(|old| old.cols != new_size.cols);
                    grid.set(Some(new_size));
                    {
                        let mut term = term.lock();
                        let start = Instant::now();
                        crate::session::resize(&mut *term, new_size);
                        if width_changed {
                            reflow.set(Reflow { at: Some(start), took: start.elapsed(), ..reflow.get() });
                        }
                        // Fewer rows push lines into the Scrollback; blank ones are dead space.
                        crate::session::drop_blank_scrollback(&mut *term);
                    }
                    proxy.set_size(new_size);
                    if let Some(sink) = &sink {
                        sink.resize(new_size);
                    }
                    let this = this.clone();
                    cx.defer(move |cx| {
                        let _ = this.update(cx, |view, cx| {
                            if width_changed {
                                view.compact_after_resize(new_size.cols, cx);
                            }
                            // Rows moved or rewrapped: an open search looks again.
                            view.search_output(cx);
                            cx.emit(TerminalEvent::Sized)
                        });
                    });
                }

                let mut frame = Frame { font_size, ..Frame::default() };
                if let Some(row) = flash_row {
                    let y = bounds.origin.y + line_h * row as f32;
                    frame.flash = Some((Bounds::new(point(bounds.origin.x, y), size(bounds.size.width, line_h)), flash_color));
                }
                let look = GridLook {
                    font: &font,
                    origin: bounds.origin,
                    scale: window.scale_factor(),
                    palette: &palette,
                    highlighter: &highlighter,
                    focused,
                    cursor_visible,
                };
                layout_grid(&term, &*text_system, &look, &mut grid_cache.borrow_mut());
                // Search matches: over the cells' backgrounds and the selection, under the text.
                let (match_bg, current_bg, current_edge) = search_colors;
                for (row, cols, current) in &search_marks {
                    let area = Bounds::new(
                        point(bounds.origin.x + cell_w * cols.start as f32, bounds.origin.y + line_h * *row as f32),
                        size(cell_w * cols.len() as f32, line_h),
                    );
                    frame.matches.push((area, if *current { current_bg } else { match_bg }));
                    if *current {
                        frame.outlines.push((area, current_edge));
                    }
                }
                // The IME's composition sits at the cursor, underlined, until it is committed.
                if let Some(text) = marked.filter(|text| !text.contains('\n'))
                    && let Some((col, row)) = cursor_cell(&*term.lock())
                {
                    let fg = rgb_to_hsla(palette.foreground);
                    let run = TextRun {
                        len: text.len(),
                        font: font.fonts[0].clone(),
                        color: fg,
                        background_color: None,
                        underline: Some(UnderlineStyle { thickness: px(1.), color: Some(fg), wavy: false }),
                        strikethrough: None,
                    };
                    let line = text_system.shape_line(text.into(), font_size, &[run], None);
                    let origin = point(bounds.origin.x + cell_w * col as f32, bounds.origin.y + line_h * row as f32);
                    let area = Bounds::new(origin, size(line.width, line_h));
                    frame.preedit = Some((area, line, rgb_to_hsla(palette.background)));
                }
                for (row, cols) in &link_marks {
                    let x = bounds.origin.x + cell_w * cols.start as f32;
                    let y = bounds.origin.y + line_h * *row as f32 + font.metrics.underline_y;
                    frame.links.push((point(x, y), cell_w * cols.len() as f32));
                }
                frame.link_color = rgb_to_hsla(palette.foreground);
                frame
            }),
            move |bounds, frame, window, cx| crate::perf::time("terminal grid paint", || {
                // Text typed, composed with dead keys or committed by an IME comes in here.
                window.handle_input(&input_focus, ElementInputHandler::new(bounds, input_view), cx);
                let terminal_bounds = Bounds::new(
                    point(bounds.left() - px(PADDING_X), bounds.top() - px(PADDING_Y)),
                    size(bounds.size.width + px(PADDING_X + pad_right), bounds.size.height + px(PADDING_Y * 2.)),
                );
                bounds_state.set_terminal(terminal_bounds);
                window.on_mouse_event(move |event: &MouseMoveEvent, phase, window, cx| {
                    if phase == gpui::DispatchPhase::Capture {
                        let _ = this_hover.update(cx, |this, cx| this.minimap_hover(event, window, cx));
                    }
                });
                window.on_mouse_event(move |_: &MouseDownEvent, phase, _, _| {
                    if phase == gpui::DispatchPhase::Capture {
                        hover_state.cancel_pending();
                    }
                });
                // Leaving the window: the overlay fades (a held slider keeps it).
                window.on_mouse_event(move |_: &gpui::MouseExitEvent, phase, _, cx| {
                    if phase == gpui::DispatchPhase::Capture && !exit_state.dragging() {
                        let before = exit_state.look();
                        exit_state.close(Instant::now());
                        if exit_state.look() != before || exit_state.animating(Instant::now()) {
                            cx.notify(hover_entity);
                        }
                    }
                });
                // A selection keeps following the pointer once it leaves the terminal.
                window.on_mouse_event(move |event: &MouseMoveEvent, phase, window, cx| {
                    if phase == gpui::DispatchPhase::Bubble {
                        let _ = this_drag.update(cx, |this, cx| this.drag_selection(event, window, cx));
                    }
                });
                // A new width may wait a moment (see `Reflow`): meanwhile the grid can be wider
                // than the pane, and must not draw over its neighbours.
                window.with_content_mask(Some(gpui::ContentMask { bounds: terminal_bounds }), |window| {
                    // One layer for the whole grid: everything in it shares one draw order and
                    // is drawn by kind (backgrounds, line art, underlines, then text), instead of
                    // each of thousands of glyphs being placed in the scene on its own.
                    let grid = paint_cache.borrow();
                    let rows = &grid.rows;
                    window.paint_layer(bounds, |window| {
                        let rects = frame.flash.iter().chain(rows.iter().flat_map(|r| &r.rects)).chain(&frame.matches);
                        for (bounds, color) in rects {
                            window.paint_quad(fill(*bounds, *color));
                        }
                        for (prim, color) in rows.iter().flat_map(|r| &r.prims) {
                            paint_prim(window, prim, *color);
                        }
                        for d in rows.iter().flat_map(|r| &r.decorations) {
                            let origin = point(d.x0, d.y);
                            let width = d.x1 - d.x0;
                            let line = UnderlineStyle { thickness: px(1.), color: Some(d.color), wavy: false };
                            match d.kind {
                                DecorationKind::Underline => window.paint_underline(origin, width, &line),
                                DecorationKind::Curly => {
                                    window.paint_underline(origin, width, &UnderlineStyle { wavy: true, ..line })
                                }
                                DecorationKind::Double => {
                                    window.paint_underline(point(d.x0, d.y - px(1.)), width, &line);
                                    window.paint_underline(point(d.x0, d.y + px(1.)), width, &line);
                                }
                                DecorationKind::Strike => window.paint_strikethrough(
                                    origin,
                                    width,
                                    &StrikethroughStyle { thickness: px(1.), color: Some(d.color) },
                                ),
                            }
                        }
                        for g in rows.iter().flat_map(|r| &r.glyphs) {
                            let _ = if g.emoji {
                                window.paint_emoji(g.origin, g.font_id, g.id, frame.font_size)
                            } else {
                                window.paint_glyph(g.origin, g.font_id, g.id, frame.font_size, g.color)
                            };
                        }
                    });
                    // The cursor goes on top of the text.
                    for (bounds, color) in rows.iter().flat_map(|r| &r.cursor_bars) {
                        window.paint_quad(fill(*bounds, *color));
                    }
                    if let Some((bounds, color)) = rows.iter().find_map(|r| r.cursor_outline) {
                        window.paint_quad(outline(bounds, color, gpui::BorderStyle::Solid));
                    }
                    for (bounds, color) in &frame.outlines {
                        window.paint_quad(outline(*bounds, *color, gpui::BorderStyle::Solid));
                    }
                    let link_line = UnderlineStyle { thickness: px(1.), color: Some(frame.link_color), wavy: false };
                    for (origin, width) in &frame.links {
                        window.paint_underline(*origin, *width, &link_line);
                    }
                    if let Some((bounds, line, background)) = &frame.preedit {
                        window.paint_quad(fill(*bounds, *background));
                        let _ = line.paint(bounds.origin, bounds.size.height, TextAlign::Left, None, window, cx);
                    }
                });
            }),
        )
        .size_full();

        div()
            .id("terminal")
            .on_modifiers_changed(move |event, window, _| {
                // Ctrl turns the pointer into a hand over a link.
                if (password_prompt && event.capslock.on != caps_lock) || watch_ctrl {
                    window.refresh();
                }
            })
            .key_context(CONTEXT)
            .when(matches!(self.prompt, Some(ActivePrompt::ConfirmPaste { .. })), |el| {
                el.key_context(format!("{CONTEXT} {PASTE_CONFIRM_CONTEXT}").as_str())
            })
            .track_focus(&self.focus)
            .on_key_down(cx.listener(Self::key_down))
            .on_action(cx.listener(|this, _: &SendTab, window, cx| this.send_tab(false, window, cx)))
            .on_action(cx.listener(|this, _: &SendBackTab, window, cx| this.send_tab(true, window, cx)))
            .on_action(cx.listener(Self::copy))
            .on_action(cx.listener(Self::paste))
            .on_action(cx.listener(|this, _: &ConfirmPaste, window, cx| {
                if this.focus.is_focused(window) && matches!(this.prompt, Some(ActivePrompt::ConfirmPaste { .. })) {
                    this.confirm_paste(window, cx);
                    cx.stop_propagation();
                } else {
                    cx.propagate();
                }
            }))
            .on_action(cx.listener(Self::select_output))
            .on_action(cx.listener(|this, _: &ScrollPageUp, window, cx| {
                if this.focus.is_focused(window) { this.scroll_page(true, cx); } else { cx.propagate(); }
            }))
            .on_action(cx.listener(|this, _: &ScrollPageDown, window, cx| {
                if this.focus.is_focused(window) { this.scroll_page(false, cx); } else { cx.propagate(); }
            }))
            .on_action(cx.listener(|this, _: &Find, window, cx| this.open_search(window, cx)))
            .on_mouse_down(MouseButton::Left, cx.listener(Self::mouse_down))
            .on_mouse_down(MouseButton::Middle, cx.listener(Self::mouse_down))
            .on_mouse_down(MouseButton::Right, cx.listener(Self::mouse_down))
            .on_mouse_move(cx.listener(Self::mouse_move))
            // Moves are heard only over the terminal: leaving it takes the link underline away.
            .on_hover(cx.listener(|this, hovered: &bool, _, cx| {
                if !*hovered && this.hover_link.take().is_some() {
                    cx.notify();
                }
            }))
            .on_mouse_up(MouseButton::Left, cx.listener(Self::mouse_up))
            .on_mouse_up(MouseButton::Middle, cx.listener(Self::mouse_up))
            .on_mouse_up(MouseButton::Right, cx.listener(Self::mouse_up))
            // A drag that ends outside the terminal still ends: the program gets its release.
            .on_mouse_up_out(MouseButton::Left, cx.listener(Self::mouse_up))
            .on_mouse_up_out(MouseButton::Middle, cx.listener(Self::mouse_up))
            .on_mouse_up_out(MouseButton::Right, cx.listener(Self::mouse_up))
            .on_scroll(cx.listener(Self::scroll_wheel))
            .relative()
            .size_full()
            .flex()
            // No background of its own: the pane behind it paints the terminal's, and a
            // second translucent layer would hide the backdrop.
            // A program that takes the mouse gets an arrow, as in xterm; shift still selects.
            .map(|el| if link_pointer { el.cursor_pointer() } else if mouse_reporting { el.cursor_default() } else { el.cursor_text() })
            .child(
                div()
                    .relative()
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .pl(px(PADDING_X))
                    .pr(px(pad_right))
                    .py(px(PADDING_Y))
                    .child(surface)
                    .when(password_prompt && caps_lock, |el| el.child(
                        div()
                            .absolute()
                            .bottom(px(12.))
                            .right(px(16.))
                            .px(px(8.))
                            .py(px(5.))
                            .rounded(app.corner(6.))
                            .bg(app.surface)
                            .border_1()
                            .border_color(app.border)
                            .child(caps_lock_indicator("terminal-caps-lock", app)),
                    ))
                    .children(search_bar),
            )
            .children(minimap_column)
            .children(minimap_overlay)
            .children(rail)
            .children(self.render_restore(cx))
            .children(self.render_disconnected(cx))
            .children(self.render_route(cx))
            .context_menu({
                let (view, focus) = (cx.entity().downgrade(), self.focus.clone());
                move |menu, _, cx| Self::context_menu(view.clone(), focus.clone(), menu, cx)
            })
    }
}

/// Pasted text as one line: each line break, of any kind, becomes a space.
/// How many lines a paste sends, counted as the shell will see them: `paste_bytes` sends a
/// bare CR as Enter too, not only LF and CRLF.
fn pasted_lines(text: &str) -> usize {
    text.trim_end_matches(['\r', '\n']).replace("\r\n", "\n").split(['\r', '\n']).count()
}

fn one_line(text: &str) -> String {
    text.trim_end_matches(['\r', '\n']).replace("\r\n", " ").replace(['\r', '\n'], " ")
}

#[cfg(test)]
mod tests {
    use super::{
        Activity, CachedGlyph, GlyphCache, GridCache, GridFont, GridLook, INLINE_CLUSTER, RowTextMetrics, Shaper,
        asks_for_secret, layout_grid,
    };

    #[test]
    fn pasting_as_one_line_turns_every_line_break_into_a_space() {
        use super::one_line;
        assert_eq!(one_line("cd /srv\r\nls -la\nexit\r\n"), "cd /srv ls -la exit");
        assert_eq!(one_line("a\rb"), "a b");
        assert_eq!(one_line("single"), "single");
    }

    #[test]
    fn a_paste_counts_every_line_break_the_shell_will_see() {
        use super::pasted_lines;
        assert_eq!(pasted_lines("ls\n"), 1);
        assert_eq!(pasted_lines("ls\r\npwd\r\n"), 2);
        // A bare CR is Enter to the shell, so it must trigger the multi-line question.
        assert_eq!(pasted_lines("ls\rrm -rf build"), 2);
        assert_eq!(pasted_lines(""), 1);
    }
    use crate::highlight::Highlighter;
    use crate::session::ssh::Output;
    use crate::session::{EventProxy, GridSize, SharedTerm, new_term};
    use crate::theme::TerminalPalette;
    use alacritty_terminal::grid::{Dimensions as _, Scroll};
    use alacritty_terminal::index::{Column, Line, Point as GridPoint, Side};
    use alacritty_terminal::selection::{Selection, SelectionType};
    use gpui::{Pixels, px};
    use std::cell::Cell;
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    #[test]
    fn width_preparation_coalesces_frames_and_rejects_obsolete_completions() {
        use super::{Ordering, WidthPreparation};
        let mut preparation = WidthPreparation::default();
        let (_, first) = preparation.request(120).unwrap();
        assert!(preparation.request(120).is_none());
        assert!(!preparation.ready(120));
        let (_, second) = preparation.request(150).unwrap();
        assert!(first.load(Ordering::Relaxed));
        preparation.finish(&first, true);
        assert!(!preparation.ready(150));
        preparation.finish(&second, true);
        assert!(preparation.ready(150));
        // Collapse, expand, then collapse to the same width before an old worker finishes.
        preparation.pending = None;
        assert!(second.load(Ordering::Relaxed));
        let (_, third) = preparation.request(150).unwrap();
        preparation.finish(&second, true);
        assert!(!preparation.ready(150));
        preparation.finish(&third, false);
        assert!(preparation.request(150).is_some(), "a failed preparation must be retryable");
    }

    #[test]
    fn divider_movements_share_preparation_for_nearby_widths() {
        use super::{Ordering, WidthPreparation};
        let mut preparation = WidthPreparation::default();
        let (target, work) = preparation.request(121).unwrap();
        assert_eq!(target, 128);
        for cols in 122..=128 {
            assert!(preparation.request(cols).is_none(), "column {cols} restarted preparation");
        }
        assert!(!work.load(Ordering::Relaxed));
        preparation.finish(&work, true);
        for cols in 121..=128 {
            assert!(preparation.ready(cols));
        }
        assert!(preparation.request(129).is_some());
        assert!(work.load(Ordering::Relaxed));
        let (target, work) = preparation.request(u16::MAX).unwrap();
        assert_eq!(target, u16::MAX);
        preparation.finish(&work, true);
        assert!(preparation.ready(u16::MAX));
    }

    #[test]
    fn slow_reflow_does_not_leave_a_long_redraw_cooldown() {
        use super::Reflow;
        let start = Instant::now();
        let slow = Reflow { at: Some(start), took: Duration::from_millis(250), woken: false };
        assert!(slow.wait(start + Duration::from_millis(250)).is_none());
        let normal = Reflow { at: Some(start), took: Duration::from_millis(20), woken: false };
        assert!(normal.wait(start + Duration::from_millis(40)).is_none());
        let tiny = Reflow { at: Some(start), took: Duration::from_millis(1), woken: false };
        assert!(tiny.wait(start + Duration::from_millis(16)).is_none());
    }

    /// Output every `every` ms from `from` to `to` (ms after `t0`).
    fn stream(a: &mut Activity, t0: Instant, from: u64, to: u64, every: u64) {
        for ms in (from..=to).step_by(every as usize) {
            a.output(t0 + Duration::from_millis(ms));
        }
    }

    #[test]
    fn steady_output_is_busy_after_a_second_and_stops_after_a_pause() {
        let t0 = Instant::now();
        let at = |ms| t0 + Duration::from_millis(ms);
        let mut a = Activity::default();
        a.input(t0); // Enter on `tree /`
        stream(&mut a, t0, 20, 1400, 20);
        // Output in the first half second is the command's echo; the stretch starts after it.
        assert!(!a.busy(at(1400)));
        stream(&mut a, t0, 1420, 3000, 20);
        assert!(a.busy(at(1600)));
        assert!(a.busy(at(3000)));
        // The output ends: still busy across a short pause, not after a long one.
        assert!(a.busy(at(6500)));
        assert!(!a.busy(at(7100)));
        assert!(a.settled(at(7100)));
    }

    #[test]
    fn a_quick_command_and_typing_are_not_activity() {
        let t0 = Instant::now();
        let at = |ms| t0 + Duration::from_millis(ms);
        let mut a = Activity::default();
        // `ls`: Enter, then a burst of output within a fraction of a second.
        a.input(t0);
        stream(&mut a, t0, 600, 700, 10);
        assert!(!a.busy(at(800)) && !a.busy(at(1500)) && !a.busy(at(3000)));
        // Typing: each key's echo comes right after it.
        let mut a = Activity::default();
        for ms in (0..5000).step_by(150) {
            a.input(at(ms));
            a.output(at(ms + 40));
        }
        assert!(!a.busy(at(5000)));
        assert!(a.settled(at(5000)));
    }

    #[test]
    fn a_followed_log_stays_busy_while_lines_keep_coming() {
        let t0 = Instant::now();
        // Two quick commands, each output arriving a little after its Enter (a slow server).
        let mut a = Activity::default();
        a.input(t0);
        stream(&mut a, t0, 600, 700, 50);
        a.input(t0 + Duration::from_millis(2000));
        stream(&mut a, t0, 2600, 2700, 50);
        assert!(!a.busy(t0 + Duration::from_millis(2800)));
        // A busy session stays busy while the user types.
        let mut a = Activity::default();
        stream(&mut a, t0, 0, 3000, 100);
        a.input(t0 + Duration::from_millis(3000));
        assert!(a.busy(t0 + Duration::from_millis(3100)));

        let mut a = Activity::default();
        // `top`: a redraw every 3 s.
        stream(&mut a, t0, 0, 9000, 3000);
        assert!(a.busy(t0 + Duration::from_millis(9500)));
        assert!(a.busy(t0 + Duration::from_millis(11500)));
    }

    #[test]
    fn terminal_text_stays_centered_in_cursor_row_with_signed_descent() {
        // Windows DirectWrite reports a negative descent. Other backends may
        // report a positive distance. Both must place text in the same row.
        for line_h in [18., 24., 36.] {
            let ascent = px(line_h * 0.7);
            let depth = px(line_h * 0.2);
            for descent in [depth, -depth] {
                let metrics = RowTextMetrics::new(px(line_h), ascent, descent);
                let top_gap = metrics.baseline - ascent;
                let bottom_gap = px(line_h) - metrics.baseline - depth;
                assert!(
                    f32::from(top_gap - bottom_gap).abs() < 0.001,
                    "text must be centered within the cursor row: top={top_gap:?}, bottom={bottom_gap:?}, descent={descent:?}"
                );
                assert!(metrics.underline_y > metrics.baseline);
                assert!(metrics.underline_y < px(line_h));
                assert!(metrics.strike_y > top_gap);
                assert!(metrics.strike_y < metrics.baseline);
            }
        }
    }

    #[test]
    fn startup_commands_wait_at_password_questions() {
        assert!(asks_for_secret("[sudo] password for ana:   "));
        assert!(asks_for_secret("Enter passphrase for key '/root/.ssh/id_ed25519':"));
        assert!(asks_for_secret("Password:"));
        assert!(!asks_for_secret("ana@web01:~$ "));
        assert!(!asks_for_secret("export PASSWORD_FILE=/tmp/x"));
    }

    /// A stand-in for shaping: one glyph per character, numbered by the character, counted.
    fn fake_shape(count: &Cell<usize>) -> impl Fn(&str) -> Box<[CachedGlyph]> + '_ {
        move |text| {
            count.set(count.get() + 1);
            text.chars()
                .map(|c| CachedGlyph { font_id: gpui::FontId(0), id: gpui::GlyphId(c as u32), x: px(0.), y: px(0.), emoji: false })
                .collect()
        }
    }

    fn ids(glyphs: &[CachedGlyph]) -> Vec<u32> {
        glyphs.iter().map(|g| g.id.0).collect()
    }

    #[test]
    fn glyph_cache_shapes_each_cell_text_once_per_style() {
        let shaped = Cell::new(0);
        let shape = fake_shape(&shaped);
        let mut cache = GlyphCache::default();
        let family = ['\u{1f468}', '\u{200d}', '\u{1f469}', '\u{200d}', '\u{1f467}', '\u{200d}', '\u{1f466}'];
        let long: Vec<char> = std::iter::repeat_n('\u{0301}', INLINE_CLUSTER + 3).collect();
        let cells: [(char, &[char]); 6] = [
            ('a', &[]),
            ('\u{65e5}', &[]),
            ('e', &['\u{0301}']),
            ('\u{2764}', &['\u{fe0f}']),
            ('\u{1f468}', &family[1..]),
            ('x', &long),
        ];
        for _ in 0..3 {
            cache.prepare(&"Mono".into(), px(14.), [gpui::FontWeight::NORMAL, gpui::FontWeight::BOLD]);
            for style in 0..4 {
                for (c, extra) in cells {
                    let glyphs = cache.get_with(style, c, extra, &shape);
                    let want: Vec<u32> = std::iter::once(c).chain(extra.iter().copied()).map(|c| c as u32).collect();
                    assert_eq!(ids(glyphs), want);
                }
            }
        }
        assert_eq!(shaped.get(), 6 * 4, "every text in every style is shaped once, then found");
        assert_eq!(cache.len(), 6 * 4);
        // A base character alone and with a mark are different entries.
        assert_eq!(ids(cache.get_with(0, 'e', &[], &shape)), vec!['e' as u32]);
        assert_eq!(shaped.get(), 6 * 4 + 1);

        // Another font or size starts over.
        cache.prepare(&"Mono".into(), px(15.), [gpui::FontWeight::NORMAL, gpui::FontWeight::BOLD]);
        assert_eq!(cache.len(), 0);
        cache.get_with(0, 'a', &[], &shape);
        cache.get_with(0, 'x', &long, &shape);
        assert_eq!(shaped.get(), 6 * 4 + 3);
    }

    #[test]
    fn glyph_cache_keeps_a_frame_bigger_than_its_limit_and_stays_bounded() {
        let shaped = Cell::new(0);
        let shape = fake_shape(&shaped);
        let mut cache = GlyphCache { limit: 100, ..GlyphCache::default() };
        let marks: Vec<char> = ('\u{0300}'..='\u{036f}').collect();
        let marks = &marks;
        let screen = |seed: usize| (seed..seed + 1000).map(move |i| (char::from(b'a' + (i % 26) as u8), marks[i / 26 % marks.len()]));

        // The same 1,000 clusters on screen frame after frame, ten times the limit: shaped once.
        for _ in 0..5 {
            cache.prepare(&"Mono".into(), px(14.), [gpui::FontWeight::NORMAL, gpui::FontWeight::BOLD]);
            for (c, mark) in screen(0) {
                cache.get_with(0, c, &[mark], &shape);
            }
        }
        assert_eq!(shaped.get(), 1000);

        // New clusters every frame: those no longer on screen go two generations later.
        for frame in 1..=20 {
            cache.prepare(&"Mono".into(), px(14.), [gpui::FontWeight::NORMAL, gpui::FontWeight::BOLD]);
            for (c, mark) in screen(frame * 1000 % 2912) {
                cache.get_with(1, c, &[mark], &shape);
            }
            assert!(cache.len() <= 2 * 1000 + 100 + 1, "{} entries", cache.len());
        }
        // Long clusters age the same way.
        let long: Vec<char> = std::iter::repeat_n('\u{0301}', INLINE_CLUSTER).collect();
        for frame in 0..20u32 {
            cache.prepare(&"Mono".into(), px(14.), [gpui::FontWeight::NORMAL, gpui::FontWeight::BOLD]);
            for i in 0..50u32 {
                cache.get_with(2, char::from_u32(0x4e00 + frame * 50 + i).unwrap(), &long, &shape);
            }
            assert!(cache.long.len() <= 2 * 50 + 100 / 16 + 1, "{} entries", cache.long.len());
        }
    }

    /// Shapes every character as a glyph numbered by it, in a font numbered by the style.
    struct FakeShaper;

    impl Shaper for FakeShaper {
        fn shape(&self, font: &gpui::Font, _: Pixels, text: &str) -> Box<[CachedGlyph]> {
            let style = (font.weight == gpui::FontWeight::BOLD) as usize + 2 * (font.style == gpui::FontStyle::Italic) as usize;
            text.chars()
                .enumerate()
                .map(|(i, c)| CachedGlyph {
                    font_id: gpui::FontId(style),
                    id: gpui::GlyphId(c as u32),
                    x: px(i as f32),
                    y: px(0.),
                    emoji: false,
                })
                .collect()
        }
    }

    fn grid_font(size: f32) -> GridFont {
        let metrics = RowTextMetrics::new(px(size * 1.32).round(), px(size * 0.9), px(-size * 0.25));
        GridFont::with_metrics(gpui::font("Mono"), gpui::FontWeight::BOLD, px(size), px(size * 0.6), px(size * 1.32).round(), metrics)
    }

    /// A terminal and its grid, laid out frame after frame. Every frame is checked against
    /// laying out every row afresh.
    struct Grid {
        term: SharedTerm,
        proxy: EventProxy,
        output: Output,
        font: GridFont,
        origin: gpui::Point<Pixels>,
        scale: f32,
        palette: TerminalPalette,
        highlighter: Arc<Highlighter>,
        focused: bool,
        cursor_visible: bool,
        cache: GridCache,
    }

    impl Grid {
        fn new(cols: u16, rows: u16) -> Self {
            let (tx, _rx) = futures::channel::mpsc::unbounded();
            let proxy = EventProxy::new(tx);
            let term = new_term(GridSize { cols, rows, cell_width: 8, cell_height: 18 }, 1000, proxy.clone());
            let sets = crate::highlight::load(std::path::Path::new("/nonexistent/komando-grid-test"));
            Self {
                term,
                proxy,
                output: Output::default(),
                font: grid_font(14.),
                origin: gpui::point(px(12.), px(8.)),
                scale: 1.5,
                palette: TerminalPalette::dark(),
                highlighter: Arc::new(Highlighter::from_sets(&sets, &[])),
                focused: true,
                cursor_visible: true,
                cache: GridCache::default(),
            }
        }

        fn feed(&mut self, bytes: impl AsRef<[u8]>) {
            self.output.feed(&self.term, &self.proxy, bytes.as_ref());
        }

        /// Lay out a frame and check it against a fresh layout. Returns how many rows it laid out.
        fn frame(&mut self, what: &str) -> usize {
            let Grid { term, font, origin, scale, palette, highlighter, focused, cursor_visible, cache, .. } = self;
            let look = GridLook { font, origin: *origin, scale: *scale, palette, highlighter, focused: *focused, cursor_visible: *cursor_visible };
            layout_grid(term, &FakeShaper, &look, cache);
            let mut laid_out: Vec<i32> = cache.snapshot.cells.iter().map(|c| c.row).collect();
            laid_out.dedup();
            let mut fresh = GridCache::default();
            layout_grid(term, &FakeShaper, &look, &mut fresh);
            assert_eq!(cache.rows.len(), fresh.rows.len(), "{what}: rows");
            assert_eq!(fresh.rows.len(), term.lock().screen_lines(), "{what}: rows");
            for (row, (kept, new)) in cache.rows.iter().zip(&fresh.rows).enumerate() {
                assert!(kept == new, "{what}: row {row} is not what it shows now\nkept: {kept:?}\nnow: {new:?}");
            }
            laid_out.len()
        }

        fn select(&mut self, ty: SelectionType, from: (i32, usize), to: (i32, usize)) {
            let mut selection = Selection::new(ty, GridPoint::new(Line(from.0), Column(from.1)), Side::Left);
            selection.update(GridPoint::new(Line(to.0), Column(to.1)), Side::Right);
            self.term.lock().selection = Some(selection);
        }
    }

    #[test]
    fn a_right_click_finds_the_rule_colouring_the_word() {
        let mut g = Grid::new(60, 5);
        g.feed("got OOM\r\n$ cat OOM");
        let term = g.term.lock();
        let at = |line: i32, col: usize| super::highlighted_at(&*term, &g.highlighter, GridPoint::new(Line(line), Column(col)));
        let hit = at(0, 5).expect("OOM is highlighted");
        assert_eq!(hit.matched, "OOM");
        assert!(g.highlighter.rule_at("OOM", 0).is_some_and(|h| h.source == hit.source));
        assert_eq!(at(0, 1), None, "plain text");
        assert_eq!(at(1, 7), None, "the command being typed isn't highlighted");
        assert_eq!(at(0, 50), None, "past the end of the line");
    }

    #[test]
    fn links_are_found_across_wrapped_rows_and_from_programs() {
        let mut g = Grid::new(20, 6);
        // 33 characters: wraps after 20.
        g.feed("see https://example.com/a/long/path. ok\r\n");
        g.feed("\x1b]8;;https://docs.lan/x\x07docs\x1b]8;;\x07 and \x1b]8;;file:///etc\x07file\x1b]8;;\x07\r\n");
        g.feed("from 10.0.0.5 port 22");
        let term = g.term.lock();
        let at = |line: i32, col: usize| super::link_at(&*term, GridPoint::new(Line(line), Column(col)));
        let wrapped = at(1, 3).expect("the second row of the link");
        assert_eq!(wrapped.url, "https://example.com/a/long/path");
        assert_eq!(wrapped.spans, [(Line(0), 4..20), (Line(1), 0..15)]);
        assert_eq!(at(0, 5), Some(wrapped));
        assert_eq!(at(0, 1), None, "plain text");
        assert_eq!(at(1, 17), None, "after the link");
        let program = at(2, 1).expect("an OSC 8 link");
        assert_eq!((program.url.as_str(), program.spans.clone()), ("https://docs.lan/x", vec![(Line(2), 0..4)]));
        assert_eq!(at(2, 11), None, "only web addresses open");
        assert_eq!(at(3, 7), None, "an IP address alone isn't a link");
    }

    #[test]
    fn terminal_selection_uses_the_theme_foreground_even_for_dim_text() {
        for theme in crate::theme::themes(false).iter().chain(crate::theme::themes(true)) {
            let mut grid = Grid::new(30, 3);
            grid.palette = TerminalPalette::from_theme(theme);
            grid.cursor_visible = false;
            grid.feed("\x1b[2mSelected\x1b[0m");
            grid.select(SelectionType::Simple, (0, 0), (0, 7));
            grid.frame("selected dim text");
            let expected = crate::theme::rgb_to_hsla(grid.palette.selection_foreground);
            let glyph = grid.cache.rows[0].glyphs.iter().find(|g| g.id == gpui::GlyphId('S' as u32)).unwrap();
            assert_eq!(glyph.color, expected, "{} selection text", theme.name);
        }
    }

    #[test]
    fn highlight_rules_leave_the_command_being_typed_alone() {
        fn color(g: &Grid, row: usize, c: char) -> gpui::Hsla {
            let glyphs = &g.cache.rows[row].glyphs;
            glyphs.iter().find(|gl| gl.id == gpui::GlyphId(c as u32)).map(|gl| gl.color).unwrap()
        }
        let mut g = Grid::new(60, 5);
        // Output that looks like a prompt, plain output, then the line being typed.
        g.feed("$ OOM
OOM
$ cat OOM");
        g.frame("typing");
        let plain = color(&g, 2, 'c');
        assert_ne!(color(&g, 1, 'O'), plain, "output is highlighted");
        assert_ne!(color(&g, 0, 'O'), plain, "output after something like a prompt is highlighted");
        assert_eq!(color(&g, 2, 'O'), plain, "the command being typed is not");
        // Entered (a Command Mark), it stays as typed once the cursor moves on.
        crate::marks::Marks::default().entered(&mut *g.term.lock());
        g.feed("
OOM
$ ");
        g.frame("entered");
        assert_eq!(color(&g, 2, 'O'), plain, "an entered command is not highlighted");
        assert_ne!(color(&g, 3, 'O'), plain, "its output is");
    }


    #[test]
    fn grid_lays_out_only_the_rows_that_change() {
        let mut g = Grid::new(40, 10);
        for i in 0..30 {
            g.feed(format!("line {i} INFO \x1b[32mok\x1b[0m ERROR healthy\r\n"));
        }
        g.feed("\x1b[1;32muser@host\x1b[0m:~$ ");
        assert_eq!(g.frame("first frame"), 10);
        assert!(g.frame("nothing changed") <= 1);
        for c in "ls -la".chars() {
            g.feed(c.to_string());
            assert!(g.frame("typing") <= 1);
        }
        // A program redraws one line elsewhere and puts the cursor back.
        g.feed("\x1b7\x1b[3;5Hstatus: \x1b[1;31mERROR\x1b[0m\x1b8");
        assert!(g.frame("redraw") <= 2);
        g.feed("\x1b[5;1H");
        assert!(g.frame("cursor moved") <= 2);
        g.feed("\x1b[7mreverse\x1b[0m \x1b[4munder\x1b[9mstrike\x1b[0m e\u{301} \u{65e5}\u{672c} \u{2764}\u{fe0f} \u{2500}\u{2524}");
        assert!(g.frame("attributes, marks, wide characters") <= 1);
        g.feed("\x1b[5;3H");
        assert!(g.frame("cursor on a wide character") <= 1);

        g.focused = false;
        assert!(g.frame("unfocused") <= 1);
        g.focused = true;
        assert!(g.frame("focused") <= 1);
        g.cursor_visible = false;
        assert!(g.frame("blink off") <= 1);
        assert!(g.cache.rows.iter().all(|row| row.cursor_bars.is_empty() && row.cursor_outline.is_none()));
        g.cursor_visible = true;
        assert!(g.frame("blink on") <= 1);
        assert!(g.cache.rows.iter().any(|row| !row.cursor_bars.is_empty()));
        g.feed("\x1b[6 q");
        assert!(g.frame("bar cursor") <= 1);
        g.feed("\x1b[?25l");
        assert!(g.frame("cursor hidden") <= 1);
        g.feed("\x1b[?25h\x1b[2 q");
        assert!(g.frame("cursor shown") <= 1);

        g.select(SelectionType::Simple, (2, 3), (4, 7));
        assert!(g.frame("selection") <= 4);
        g.select(SelectionType::Block, (6, 1), (7, 9));
        assert!(g.frame("other selection") <= 6);
        g.select(SelectionType::Lines, (-3, 0), (0, 0));
        g.frame("selection into the Scrollback");
        g.term.lock().selection = None;
        assert!(g.frame("no selection") <= 2);

        // Output at the bottom scrolls every row.
        g.feed("\x1b[10;1H\r\nnext");
        assert_eq!(g.frame("scrolled by output"), 10);

        g.term.lock().scroll_display(Scroll::Delta(5));
        assert_eq!(g.frame("view scrolled up"), 10);
        assert!(g.frame("still scrolled up") <= 1);
        g.feed("x");
        g.frame("typing below the view");
        g.feed("\r\nmore output\r\n");
        g.frame("output while scrolled up");
        g.term.lock().scroll_display(Scroll::Delta(2));
        g.feed("\x1b[1;1Hy");
        g.frame("a change on screen while scrolled up");
        g.term.lock().grid_mut().clear_history();
        g.frame("Scrollback dropped while scrolled up");
        g.term.lock().scroll_display(Scroll::Bottom);
        g.frame("back at the bottom");
        // Scrolled up, a region below the top scrolling raises the terminal's offset past its
        // Scrollback (see `view_offset`).
        g.feed("\r\none\r\ntwo\r\n");
        g.term.lock().scroll_display(Scroll::Top);
        g.frame("scrolled to the top");
        g.feed("\x1b7\x1b[3;8r\x1b[8;1H\n\n\n\n\n\n\x1b[r\x1b8");
        g.frame("a region scrolled while scrolled up");
        g.feed("z");
        g.frame("typing with the offset past the Scrollback");
        g.term.lock().scroll_display(Scroll::Bottom);
        g.frame("back at the bottom again");

        crate::session::resize(&mut *g.term.lock(), GridSize { cols: 30, rows: 8, cell_width: 8, cell_height: 18 });
        assert_eq!(g.frame("narrower and shorter"), 8);
        crate::session::resize(&mut *g.term.lock(), GridSize { cols: 50, rows: 12, cell_width: 8, cell_height: 18 });
        assert_eq!(g.frame("wider and taller"), 12);

        g.palette = TerminalPalette::light();
        assert_eq!(g.frame("light theme"), 12);
        g.highlighter = Arc::new(Highlighter::default());
        assert_eq!(g.frame("no highlight rules"), 12);
        let sets = crate::highlight::load(std::path::Path::new("/nonexistent/komando-grid-test"));
        g.highlighter = Arc::new(Highlighter::from_sets(&sets, &[]));
        assert_eq!(g.frame("highlight rules back"), 12);
        g.font = grid_font(16.);
        assert_eq!(g.frame("bigger font"), 12);
        g.origin = gpui::point(px(40.), px(8.));
        assert_eq!(g.frame("moved"), 12);
        g.scale = 2.;
        assert_eq!(g.frame("other display"), 12);
        assert!(g.frame("nothing changed") <= 1);

        g.feed("\x1b]4;1;rgb:12/34/56\x07\x1b[31mred\x1b[0m");
        g.frame("palette colour changed by the program");
        g.feed("\x1b]12;rgb:ff/00/00\x07");
        g.frame("cursor colour changed by the program");

        g.feed("\x1b[?1049h\x1b[2J\x1b[1;1Htop - 10:00 ERROR\x1b[3;1HPID USER");
        assert_eq!(g.frame("alternate screen"), 12);
        g.feed("\x1b[3;5H99");
        assert!(g.frame("full-screen program updates a cell") <= 2);
        g.feed("\x1b[?1049l");
        assert_eq!(g.frame("back from the alternate screen"), 12);

        g.feed("\x1b[?2026h\x1b[2;1Hhalf drawn");
        g.frame("inside a synchronized update");
        g.feed("\x1b[?2026l");
        g.frame("synchronized update done");
        g.feed("\x1b[4hins\x1b[4l");
        g.frame("insert mode");
        g.feed("\x1b[2;5r\x1b[5;1H\n\n\x1b[r");
        g.frame("scroll region");
        g.feed("\x1b[3;1H\x1b[K\x1b[1J");
        g.frame("erased");
        g.feed("\x1bc");
        g.frame("reset");
    }

    #[test]
    fn grid_rows_from_damage_match_a_full_layout_under_random_output() {
        let mut g = Grid::new(24, 8);
        let mut seed = 0x2545_f491_4f6c_dd1d_u64;
        let mut next = move || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed
        };
        let pieces: &[&str] = &[
            "hello", " ", "\r\n", "\n", "\r", "\t", "\x08", "ERROR", "INFO healthy", "e\u{301}", "\u{65e5}\u{672c}",
            "\u{2764}\u{fe0f}", "\u{2500}\u{2502}", "\x1b[K", "\x1b[1K", "\x1b[2K", "\x1b[J", "\x1b[1J", "\x1b[2J",
            "\x1b[3L", "\x1b[2M", "\x1b[4@", "\x1b[3P", "\x1b[5X", "\x1bM", "\x1bD", "\x1bE", "\x1b[2S", "\x1b[T",
            "\x1b[3;6r", "\x1b[r", "\x1b[1;31m", "\x1b[0m", "\x1b[7m", "\x1b[4:3m", "\x1b[8m", "\x1b[2m", "\x1b[3m",
            "\x1b[48;5;22m", "\x1b[?25l", "\x1b[?25h", "\x1b[4h", "\x1b[4l", "\x1b[?1049h", "\x1b[?1049l", "\x1b[2 q",
            "\x1b[5 q", "\x1b#8", "\x1b[?2026hsync\x1b[?2026l", "\x1b]4;2;rgb:aa/bb/cc\x07", "\x1b]104\x07",
            "\x1b]11;rgb:10/20/30\x07", "\x1b[3G", "\x1b[10C", "\x1b[2A", "\x1b[3B", "\x1b[2D", "\x1b7", "\x1b8",
            "\x1b[?6h", "\x1b[?6l", "\x1b[?7l", "\x1b[?7h", "\x1b[3g", "\x1b[3J", "\x1bc",
        ];
        let mut partial = 0;
        for step in 0..3000 {
            for _ in 0..next() % 4 + 1 {
                let n = next();
                match n % 40 {
                    0 => g.feed(format!("\x1b[{};{}H", n / 40 % 10, n / 400 % 30)),
                    1 => {
                        let delta = (n / 40 % 7) as i32 - 3;
                        g.term.lock().scroll_display(Scroll::Delta(delta));
                    }
                    2 => {
                        let row = |n: u64| (n % 12) as i32 - 4;
                        let ty = [SelectionType::Simple, SelectionType::Block, SelectionType::Lines][(n / 40 % 3) as usize];
                        g.select(ty, (row(n / 120), (n / 1440 % 24) as usize), (row(n / 17280), (n / 207360 % 24) as usize));
                    }
                    3 => g.term.lock().selection = None,
                    4 => g.focused = !g.focused,
                    5 if n / 40 % 8 == 0 => {
                        let size = GridSize { cols: 10 + (n / 320 % 20) as u16, rows: 3 + (n / 6400 % 8) as u16, cell_width: 8, cell_height: 18 };
                        crate::session::resize(&mut *g.term.lock(), size);
                    }
                    6 if n / 40 % 8 == 0 => g.palette = if g.palette.dark { TerminalPalette::light() } else { TerminalPalette::dark() },
                    _ => g.feed(pieces[(n / 40) as usize % pieces.len()]),
                }
            }
            let lines = g.term.lock().screen_lines();
            if g.frame(&format!("step {step}")) < lines {
                partial += 1;
            }
        }
        // Many frames kept rows: the test is about those.
        assert!(partial > 3000 / 4, "{partial} of 3000 frames kept rows");
    }

    #[test]
    fn scrolling_reuses_highlights_and_keeps_the_cache_bounded() {
        let mut g = Grid::new(60, 10);
        for i in 0..30 {
            g.feed(format!("line {i} ERROR 日本 e\u{301}\r\n"));
        }
        g.frame("first paint");
        let scans = g.cache.highlights.scans;
        for i in 30..1230 {
            let before = g.cache.highlights.scans;
            g.feed(format!("line {i} ERROR 日本 e\u{301}\r\n"));
            assert_eq!(g.frame("one new line, other rows moved"), 10);
            assert!(g.cache.highlights.scans - before <= 2, "moved rows ran the rules again");
            assert!(g.cache.highlights.rows.len() <= 2 * (super::HIGHLIGHT_GENERATION + 10));
        }
        assert!(g.cache.highlights.scans > scans, "new output still runs the rules");
    }

    #[test]
    fn highlight_cache_uses_exact_text_and_invalidates_rules_and_background() {
        let mut cache = super::HighlightCache::default();
        let rules = Arc::new(Highlighter::default());
        let calls = Cell::new(0);
        let compute = || { calls.set(calls.get() + 1); Box::default() };
        cache.prepare(&rules, true);
        cache.spans("ERROR 日本", compute);
        cache.spans("ERROR 日本", compute);
        assert_eq!(calls.get(), 1);
        cache.spans("ERROR 日木", compute);
        assert_eq!(calls.get(), 2, "different text must not reuse a result");
        cache.prepare(&rules, false);
        cache.spans("ERROR 日本", compute);
        assert_eq!(calls.get(), 3, "background-dependent colors");
        cache.prepare(&Arc::new(Highlighter::default()), false);
        cache.spans("ERROR 日本", compute);
        assert_eq!(calls.get(), 4, "new rules");
    }

    /// Frames of a log scrolling a few lines at a time: the terminal damages every row when it
    /// scrolls, so every row is laid out again each frame. Times them without highlight rules,
    /// with them, and with them but each row's highlights worked out afresh (as before
    /// `HighlightCache`). Reports timings without thresholds: `cargo test --lib
    /// bench_grid_frames -- --ignored --nocapture`, in release mode for real numbers.
    #[test]
    #[ignore]
    fn bench_grid_frames_while_output_scrolls() {
        let line = |i: u64| {
            let level = ["\x1b[31mERROR", "\x1b[33mWARN ", "\x1b[32mINFO ", "\x1b[32mINFO "][(i % 4) as usize];
            format!(
                "\x1b[2m2026-11-13 10:{:02}:{:02}.{:03}\x1b[0m {level}\x1b[0m api GET /api/items/{i} from 10.0.{}.{} {} in {}ms healthy\r\n",
                i / 60 % 60,
                i % 60,
                i % 1000,
                i % 256,
                i * 7 % 256,
                if i % 16 == 0 { 500 } else { 200 },
                i % 997,
            )
        };
        let sets = crate::highlight::load(std::path::Path::new("/nonexistent/komando-grid-bench"));
        let rules = Arc::new(Highlighter::from_sets(&sets, &[]));
        let cases = [
            ("no rules", Arc::new(Highlighter::default()), true),
            ("default rules", rules.clone(), true),
            ("default rules, uncached", rules, false),
        ];
        for (name, highlighter, cached) in cases {
            let mut g = Grid::new(160, 45);
            g.highlighter = highlighter;
            let mut i = 0;
            for _ in 0..200 {
                g.feed(line(i));
                i += 1;
            }
            let frames = 1000;
            let mut took = Duration::ZERO;
            for _ in 0..frames {
                for _ in 0..3 {
                    g.feed(line(i));
                    i += 1;
                }
                if !cached {
                    g.cache.highlights = Default::default();
                }
                let Grid { term, font, origin, scale, palette, highlighter, focused, cursor_visible, cache, .. } = &mut g;
                let look = GridLook { font, origin: *origin, scale: *scale, palette, highlighter, focused: *focused, cursor_visible: *cursor_visible };
                let start = Instant::now();
                layout_grid(term, &FakeShaper, &look, cache);
                took += start.elapsed();
            }
            eprintln!("{name}: {:?} per 160x45 frame scrolled by output", took / frames);
        }
    }
}
