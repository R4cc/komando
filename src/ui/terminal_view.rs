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
use crate::session::password_prompt::asks_for_password;
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

mod banners;
mod events;
mod grid;
mod input;
mod mouse;
mod rich_copy;
mod search;
mod selection;
#[cfg(all(test, target_os = "linux"))]
mod render_bench;

actions!(terminal, [
    Copy, Paste, ConfirmPaste, ScrollPageUp, ScrollPageDown, Find, SearchOlder, SearchNewer, SendTab, SendBackTab, SelectOutput,
]);

use grid::*;

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
    /// Back to one Session: close the split this pane is in (from the right-click menu).
    CloseSplit,
    /// A Port Forward couldn't start; `why` says so in a sentence.
    ForwardFailed { forward: crate::model::PortForward, why: String },
    /// "Always" (`allow`) or "Never" on the clipboard question: keep it for the Host.
    ClipboardChoice { allow: bool },
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
    /// Whether programs here may copy to the clipboard (OSC 52); the workspace sets it from
    /// the Host and Settings (see [`Self::set_clipboard`]).
    pub clipboard: crate::model::RemoteClipboard,
    /// What a program last asked to copy, waiting for the user to allow it.
    clipboard_ask: Option<String>,
    /// One of the two panes of Split View: its right-click menu offers to close the split.
    pub in_split: bool,
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
            clipboard: crate::model::RemoteClipboard::Ask,
            clipboard_ask: None,
            in_split: false,
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

}

/// How the clipboard question describes `text`: its size ("12 characters", "3 lines"), and its
/// start on one line, with line breaks shown as ⏎.
fn clipboard_preview(text: &str) -> (String, String) {
    let lines = text.lines().count();
    let summary = if lines > 1 {
        format!("{lines} lines")
    } else {
        crate::ui::widgets::plural(text.chars().count(), "character")
    };
    let preview: String = text.chars().take(200).map(|c| if c == '\n' { '⏎' } else if c.is_control() { ' ' } else { c }).collect();
    (summary, preview)
}

fn report_button(button: MouseButton) -> keys::MouseButton {
    match button {
        MouseButton::Middle => keys::MouseButton::Middle,
        MouseButton::Right => keys::MouseButton::Right,
        _ => keys::MouseButton::Left,
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
                asks_for_password(&line)
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
        let search_colors = search_colors(app);
        // The current match's text, laid out with the grid (see `RowContext::current_match`).
        let current_match: Vec<(i32, Range<usize>)> =
            search_marks.iter().filter(|(_, _, current)| *current).map(|(row, cols, _)| (*row, cols.clone())).collect();
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
                    current_match: &current_match,
                    match_foreground: search_colors.current_text,
                };
                layout_grid(&term, &*text_system, &look, &mut grid_cache.borrow_mut());
                // Search matches: over the cells' backgrounds and the selection, under the text.
                // The current one is solid, its text recoloured to read on it; the others are a
                // light tint with an edge, so their text keeps its own colours.
                for (row, cols, current) in &search_marks {
                    let area = Bounds::new(
                        point(bounds.origin.x + cell_w * cols.start as f32, bounds.origin.y + line_h * *row as f32),
                        size(cell_w * cols.len() as f32, line_h),
                    );
                    if *current {
                        frame.matches.push((area, search_colors.current));
                    } else {
                        frame.matches.push((area, search_colors.other));
                        frame.outlines.push((area, search_colors.other_edge));
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
            .children(self.render_clipboard_ask(cx))
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
mod tests;
