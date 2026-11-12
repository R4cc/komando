//! One terminal pane: paints the grid on the GPU and turns input into bytes for its Session.

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, VecDeque};
use std::rc::Rc;
use std::time::Duration;

use alacritty_terminal::grid::{Dimensions as _, Scroll};
use alacritty_terminal::index::{Column, Line, Point as GridPoint, Side};
use alacritty_terminal::selection::{Selection, SelectionType};
use alacritty_terminal::term::TermMode;
use alacritty_terminal::term::cell::Flags;
use alacritty_terminal::vte::ansi::{Color, CursorShape, NamedColor, Rgb};
use futures::StreamExt as _;
use futures::channel::{mpsc, oneshot};
use gpui::prelude::FluentBuilder as _;
use gpui::{
    App, Bounds, ClipboardItem, Context, Entity, EventEmitter, FocusHandle, Focusable, FontStyle, FontWeight,
    Hsla, KeyDownEvent, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, PathBuilder, Pixels,
    ScrollEvent, SharedString, StrikethroughStyle, Subscription, TextRun, TouchPhase, UnderlineStyle, Window,
    actions, canvas, div, fill, outline, point, prelude::*, px, size,
};
use gpui_component::input::{Input, InputEvent, InputState};

use crate::highlight::{self, ActiveHighlights};
use crate::keys::{self, MouseAction, MouseMode, Mods};
use crate::marks::Marks;
use crate::ui::connect_route::{self, ConnectRoute, RouteCard};
use crate::session::{
    Answer, EventProxy, GridSize, InputSink, Prompt, Remembered, SessionEvent, SharedTerm, Status, new_term,
    scrollback_text,
};
use crate::theme::{Appearance, UI_FONT, rgb_to_hsla};
use crate::ui::box_drawing::{self, Prim};
use crate::ui::widgets::{button, icon_el, ButtonKind};

actions!(terminal, [Copy, Paste, ScrollPageUp, ScrollPageDown]);

pub const CONTEXT: &str = "Terminal";
const PADDING_X: f32 = 12.0;
const PADDING_Y: f32 = 8.0;
/// Height each command takes on the rail: its line plus the gap below.
const RAIL_STEP: f32 = 8.0;
/// How long a row stays lit after jumping to it.
const FLASH: Duration = Duration::from_millis(1400);
/// How often the Scrollback moves while a selection is held above or below the terminal.
const AUTOSCROLL_TICK: Duration = Duration::from_millis(40);
/// Most lines it moves per tick, when the pointer is far from the terminal.
const AUTOSCROLL_MAX_LINES: f32 = 8.;
/// How long the shell must stay quiet before the next Startup Command is typed.
const STARTUP_QUIET: Duration = Duration::from_millis(150);
/// How long to wait for the shell's first output before typing anyway.
const STARTUP_FIRST: Duration = Duration::from_millis(1500);

/// Startup Commands still to type. They go in one at a time, each once the shell has printed
/// something and then gone quiet, so each lands at a prompt like a typed command.
#[derive(Default)]
struct StartupCommands {
    queue: VecDeque<String>,
    /// Bumped whenever a new wait starts; a timer only types if no newer wait replaced it.
    generation: u64,
    /// The shell asked for a password (e.g. `sudo`): wait until the user has answered.
    paused: bool,
}

impl StartupCommands {
    fn waiting(&self) -> bool {
        !self.queue.is_empty() && !self.paused
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
}

enum ActivePrompt {
    HostKey { fingerprint: String, algorithm: String, reply: Option<oneshot::Sender<bool>> },
    HostKeyChanged { fingerprint: String },
    Input {
        label: String,
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
    /// The name the tab started with; programs that reset their title get it back.
    base_title: SharedString,
    pub status: Status,
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
    /// The Scrollback is scrolling because a selection was dragged above or below the grid.
    autoscrolling: bool,
    /// A button press the program was told about; its release goes to the program too.
    mouse_pressed: Option<keys::MouseButton>,
    /// The cell the mouse was last reported in (motion is reported once per cell).
    mouse_cell: Option<(usize, usize)>,
    /// Wheel movement smaller than a line, carried over to the next event.
    scroll_remainder: f32,
    /// Focus as last reported to the program (DECSET 1004).
    focus_reported: Option<bool>,
    /// While connecting through Jump Hosts: the route page.
    route: Option<ConnectRoute>,
    startup: StartupCommands,
    pub confirm_multiline_paste: bool,
    /// PuTTY-style: selecting copies, right-click pastes.
    pub putty_clipboard: bool,
    /// Show the command rail and record entered lines for it.
    pub command_rail: bool,
    /// Show the minimap on the right edge.
    pub minimap: bool,
    minimap_state: Rc<crate::ui::minimap::MinimapState>,
    glyph_cache: Rc<RefCell<GlyphCache>>,
    marks: Marks,
    hovered_mark: Option<u64>,
    /// The mark jumped to last, lit until the timer with the same number runs out.
    flash: Option<(u64, u64)>,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<TerminalEvent> for TerminalView {}

impl Focusable for TerminalView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl TerminalView {
    /// Create the pane and its grid. The backend is attached with [`TerminalView::attach`].
    pub fn new(title: impl Into<SharedString>, scrollback: usize, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let (tx, rx) = mpsc::unbounded();
        let proxy = EventProxy::new(tx);
        let initial = GridSize { cols: 120, rows: 36, cell_width: 8, cell_height: 18 };
        if let Some(appearance) = cx.try_global::<Appearance>() {
            proxy.set_palette(&appearance.term);
        }
        let term = new_term(initial, scrollback, proxy.clone());
        Self::listen(rx, window, cx);
        let focus = cx.focus_handle();
        let subscriptions = vec![
            cx.on_focus_in(&focus, window, |this, window, _| this.report_focus(window.is_window_active())),
            cx.on_focus_out(&focus, window, |this, _, _, _| this.report_focus(false)),
            cx.observe_window_activation(window, |this, window, _| {
                let focused = window.is_window_active() && this.focus.is_focused(window);
                this.report_focus(focused);
            }),
        ];
        let title = title.into();
        Self {
            base_title: title.clone(),
            title,
            status: Status::Connecting,
            term,
            proxy,
            sink: None,
            focus,
            grid: Rc::new(Cell::new(None)),
            origin: Rc::new(Cell::new(point(px(0.), px(0.)))),
            cell: Rc::new(Cell::new((8.0, 18.0))),
            prompt: None,
            selecting: false,
            drag_at: None,
            autoscrolling: false,
            mouse_pressed: None,
            mouse_cell: None,
            scroll_remainder: 0.,
            focus_reported: None,
            route: None,
            startup: StartupCommands::default(),
            confirm_multiline_paste: true,
            putty_clipboard: false,
            command_rail: true,
            minimap: true,
            minimap_state: Rc::new(Default::default()),
            glyph_cache: Rc::new(RefCell::new(GlyphCache::default())),
            marks: Marks::default(),
            hovered_mark: None,
            flash: None,
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

    /// The grid size last laid out, or a sensible default before the first paint.
    /// The grid size from the pane's layout; `None` before it was first painted.
    pub fn measured_size(&self) -> Option<GridSize> {
        self.grid.get()
    }

    /// Take `size` (another pane's) before this pane is first painted, so a backend started
    /// now opens the shell at about the right size. The first paint corrects it if it differs.
    pub fn assume_size(&mut self, size: GridSize) {
        if self.grid.get().is_none() {
            self.grid.set(Some(size));
            self.term.lock().resize(size.term_size());
            self.proxy.set_size(size);
        }
    }

    pub fn grid_size(&self) -> GridSize {
        self.grid.get().unwrap_or(GridSize { cols: 120, rows: 36, cell_width: 8, cell_height: 18 })
    }

    /// Get ready for a new connection: the route page if it goes through Jump Hosts, and the
    /// Startup Commands to type once the shell is open.
    pub fn begin_connect(&mut self, route: Option<Vec<RouteCard>>, startup: Vec<String>) {
        self.route = route.map(ConnectRoute::new);
        self.startup = StartupCommands { queue: startup.into(), ..StartupCommands::default() };
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
                if self.marks.is_pending() {
                    self.marks.output(&mut *self.term.lock());
                } else {
                    crate::session::drop_blank_scrollback(&mut *self.term.lock());
                }
                if self.startup.waiting() && !self.is_disconnected() && self.status == Status::Connected {
                    self.schedule_startup(STARTUP_QUIET, window, cx);
                }
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
                    self.status = status;
                    match &self.status {
                        Status::Disconnected(reason) => {
                            // A pending question can no longer be answered.
                            if matches!(self.prompt, Some(ActivePrompt::Input { .. } | ActivePrompt::HostKey { .. })) {
                                self.prompt = None;
                            }
                            self.startup = StartupCommands::default();
                            match self.route.as_mut() {
                                Some(route) if !route.is_finished() => route.fail(reason.clone()),
                                _ => self.route = None,
                            }
                        }
                        Status::Connected => {
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
                            if self.startup.waiting() {
                                self.schedule_startup(STARTUP_FIRST, window, cx);
                            }
                        }
                        Status::Connecting => {}
                    }
                    cx.emit(TerminalEvent::Changed);
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
        }
    }

    fn show_prompt(&mut self, prompt: Prompt, window: &mut Window, cx: &mut Context<Self>) {
        self.prompt = Some(match prompt {
            Prompt::HostKey { fingerprint, algorithm, reply } => {
                ActivePrompt::HostKey { fingerprint, algorithm, reply: Some(reply) }
            }
            Prompt::HostKeyChanged { fingerprint } => ActivePrompt::HostKeyChanged { fingerprint },
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
                ActivePrompt::Input { label, error, input, remember, reply: Some(reply) }
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

    fn toggle_remember(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(ActivePrompt::Input { input, remember: Some(remember), .. }) = self.prompt.as_mut() {
            *remember = !*remember;
            // Keep typing in the field after ticking the box.
            input.update(cx, |state, cx| state.focus(window, cx));
            cx.notify();
        }
    }

    fn answer_host_key(&mut self, trust: bool, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(ActivePrompt::HostKey { reply, .. }) = self.prompt.as_mut() {
            if let Some(reply) = reply.take() {
                let _ = reply.send(trust);
            }
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
        let Some(line) = self.startup.queue.pop_front() else { return };
        self.scroll_to_bottom();
        {
            let mut term = self.term.lock();
            if self.command_rail && !term.mode().contains(TermMode::ALT_SCREEN) {
                self.marks.entered(&mut term);
            }
        }
        self.write(format!("{line}\r").into_bytes());
        cx.notify();
    }

    fn write(&mut self, bytes: Vec<u8>) {
        if let Some(sink) = &self.sink {
            sink.write(bytes);
        }
    }

    fn scroll_to_bottom(&self) {
        let mut term = self.term.lock();
        if term.grid().display_offset() != 0 {
            term.scroll_display(Scroll::Bottom);
        }
    }

    fn key_down(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        // Keys typed into a field on top of the terminal (a connection question's user name or
        // password) bubble up to here. They belong to that field: sending them to the session
        // and stopping propagation would also stop the field from receiving the text.
        if !self.focus.is_focused(window) {
            return;
        }
        let ks = &event.keystroke;
        let m = &ks.modifiers;
        // Application shortcuts are all ctrl+shift+… (and ctrl+tab); plain ctrl+letter keys
        // belong to the shell (ctrl+w deletes a word in bash, ctrl+k kills the line).
        if (m.control && m.shift && ks.key != "-" && ks.key != "2" && ks.key != "6") || (m.control && ks.key == "tab") {
            return;
        }
        if self.is_disconnected() {
            if ks.key == "enter" {
                cx.emit(TerminalEvent::Reconnect);
                cx.stop_propagation();
            }
            return;
        }
        let mode = *self.term.lock().mode();
        let app_cursor = mode.contains(TermMode::APP_CURSOR);
        let mods = Mods { ctrl: m.control, alt: m.alt, shift: m.shift };
        if let Some(bytes) = keys::to_bytes(&ks.key, ks.key_char.as_deref(), mods, app_cursor) {
            self.scroll_to_bottom();
            {
                let mut term = self.term.lock();
                term.selection = None;
                // Full-screen programs (vim, htop) take Enter for themselves: nothing to mark.
                if ks.key == "enter" && !m.control && !m.alt && self.command_rail && !mode.contains(TermMode::ALT_SCREEN) {
                    self.marks.entered(&mut term);
                }
            }
            // The user answered the question that paused the Startup Commands.
            if ks.key == "enter" && self.startup.paused {
                self.startup.paused = false;
            }
            self.write(bytes);
            cx.stop_propagation();
            cx.notify();
        }
    }

    fn copy(&mut self, _: &Copy, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(text) = self.term.lock().selection_to_string() {
            cx.write_to_clipboard(ClipboardItem::new_string(text));
        }
    }

    fn paste(&mut self, _: &Paste, _: &mut Window, cx: &mut Context<Self>) {
        let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) else { return };
        let lines = text.trim_end_matches(['\r', '\n']).lines().count();
        if self.confirm_multiline_paste && lines > 1 {
            self.prompt = Some(ActivePrompt::ConfirmPaste { text, lines });
            cx.notify();
        } else {
            self.paste_text(&text);
        }
    }

    fn paste_text(&mut self, text: &str) {
        let bracketed = self.term.lock().mode().contains(TermMode::BRACKETED_PASTE);
        self.scroll_to_bottom();
        self.write(keys::paste_bytes(text, bracketed));
    }

    fn scroll_page(&mut self, up: bool, cx: &mut Context<Self>) {
        self.term.lock().scroll_display(if up { Scroll::PageUp } else { Scroll::PageDown });
        cx.notify();
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
        if self.prompt.is_some() {
            self.focus_prompt(window, cx);
            return;
        }
        window.focus(&self.focus, cx);
        if let Some(mode) = self.mouse_mode(event.modifiers.shift) {
            let button = report_button(event.button);
            self.mouse_pressed = Some(button);
            self.report_mouse(mode, button, MouseAction::Press, event.position, &event.modifiers);
            return;
        }
        match event.button {
            MouseButton::Left => {
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
            MouseButton::Right if self.putty_clipboard => self.paste(&Paste, window, cx),
            _ => {}
        }
    }

    /// Pointer motion over the terminal, for programs that asked for it. A selection follows
    /// the pointer through `drag_selection` instead, which hears it anywhere.
    fn mouse_move(&mut self, event: &MouseMoveEvent, _: &mut Window, _: &mut Context<Self>) {
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

    /// The pointer moved while a selection is being dragged, wherever it is: over the
    /// terminal, the sidebar, or outside the window (the window keeps the mouse while a button
    /// is held). Past the top or bottom of the grid the Scrollback scrolls along.
    fn drag_selection(&mut self, event: &MouseMoveEvent, cx: &mut Context<Self>) {
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
        if self.autoscroll_lines(event.position) != 0 && !self.autoscrolling {
            self.autoscrolling = true;
            cx.spawn(async move |this, cx| {
                loop {
                    cx.background_executor().timer(AUTOSCROLL_TICK).await;
                    let go_on = this.update(cx, |this, cx| this.autoscroll_step(cx)).unwrap_or(false);
                    if !go_on {
                        break;
                    }
                }
            })
            .detach();
        }
        cx.notify();
    }

    fn extend_selection(&mut self, position: gpui::Point<Pixels>) {
        let (pt, side) = self.grid_point(position);
        if let Some(selection) = self.term.lock().selection.as_mut() {
            selection.update(pt, side);
        }
    }

    /// Lines to scroll for a selection dragged to `position`: up (positive) above the grid,
    /// down below it, faster the further away; 0 over it.
    fn autoscroll_lines(&self, position: gpui::Point<Pixels>) -> i32 {
        let top = f32::from(self.origin.get().y);
        let (_, ch) = self.cell.get();
        let bottom = top + self.grid_size().rows as f32 * ch;
        let y = f32::from(position.y);
        let lines = |distance: f32| (1. + distance / ch).min(AUTOSCROLL_MAX_LINES) as i32;
        if y < top {
            lines(top - y)
        } else if y >= bottom {
            -lines(y - bottom)
        } else {
            0
        }
    }

    /// One tick of scrolling while a selection is held above or below the grid. Returns
    /// whether to keep going.
    fn autoscroll_step(&mut self, cx: &mut Context<Self>) -> bool {
        let position = match self.drag_at {
            Some(position) if self.selecting => position,
            _ => {
                self.autoscrolling = false;
                return false;
            }
        };
        let lines = self.autoscroll_lines(position);
        if lines == 0 {
            self.autoscrolling = false;
            return false;
        }
        self.term.lock().scroll_display(Scroll::Delta(lines));
        self.extend_selection(position);
        cx.notify();
        true
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
        let mut term = self.term.lock();
        if term.selection.as_ref().is_some_and(|s| s.is_empty()) {
            term.selection = None;
        }
        if self.putty_clipboard
            && let Some(text) = term.selection_to_string().filter(|t| !t.is_empty())
        {
            cx.write_to_clipboard(ClipboardItem::new_string(text));
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
        let mut term = self.term.lock();
        let mode = *term.mode();
        if mode.contains(TermMode::ALT_SCREEN) {
            drop(term);
            // Full-screen programs (less, man) that did not ask for the mouse get cursor
            // keys, unless they turned that off (DECRST 1007).
            if mode.contains(TermMode::ALTERNATE_SCROLL) && !event.modifiers.shift && self.sink.is_some() {
                let bytes = keys::alternate_scroll_bytes(lines.clamp(-10, 10), mode.contains(TermMode::APP_CURSOR));
                self.write(bytes);
            }
        } else {
            term.scroll_display(Scroll::Delta(lines));
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
    /// terminal's padding so it takes no columns. Returns the view row to light up, if any.
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
                    .rounded(px(12.))
                    .bg(p.surface)
                    .border_1()
                    .border_color(p.border)
                    .shadow(crate::ui::widgets::elevation(p.dark))
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
                            .font_family(UI_FONT)
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
            Some(ActivePrompt::HostKey { .. }) => self.answer_host_key(false, window, cx),
            Some(_) => self.dismiss(window, cx),
            None => {}
        }
    }

    /// While connecting through Jump Hosts: the route page, with the current hop's question.
    fn render_route(&self, cx: &mut Context<Self>) -> Option<gpui::AnyElement> {
        let route = self.route.as_ref()?;
        let p = cx.global::<Appearance>().app;
        // The current hop's question shows in the workspace's modal layer, over this page.
        let prompt = None;
        let hop = route.current();
        let actions = vec![
            button("route-edit", "Edit host", ButtonKind::Secondary, p)
                .on_click(cx.listener(move |_, _, _, cx| cx.emit(TerminalEvent::EditHost { hop })))
                .into_any_element(),
            button("route-retry", "Retry", ButtonKind::Primary, p)
                .on_click(cx.listener(|_, _, _, cx| cx.emit(TerminalEvent::Reconnect)))
                .into_any_element(),
        ];
        Some(route.render(prompt, actions, p))
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
                .rounded(px(14.))
                .bg(p.surface)
                .border_1()
                .border_color(p.border)
                .shadow(crate::ui::widgets::elevation(p.dark))
                .flex()
                .flex_col()
                .gap(px(14.))
                .font_family(UI_FONT)
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
                .rounded(px(8.))
                .bg(p.hover)
                .font_family("Cascadia Mono")
                .text_size(px(12.))
                .text_color(p.text)
                .child(s)
                .into_any_element()
        };
        let element = match prompt {
            ActivePrompt::HostKey { fingerprint, algorithm, .. } => card(
                &match &hop_name {
                    Some(name) => format!("Trust {name}?"),
                    None => "Trust this server?".to_string(),
                },
                div()
                    .flex()
                    .flex_col()
                    .gap(px(10.))
                    .child(text(format!(
                        "First connection to this host. Check that this {algorithm} key fingerprint matches the server before trusting it."
                    )))
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
            ActivePrompt::HostKeyChanged { fingerprint } => card(
                "Host key changed: connection blocked",
                div()
                    .flex()
                    .flex_col()
                    .gap(px(10.))
                    .child(text("The server presented a different key than the one you trusted. Someone may be intercepting the connection, or the server was reinstalled. Confirm with the server's administrator before removing the old key from Komando's known_hosts.".into()))
                    .child(mono(fingerprint.clone()))
                    .into_any_element(),
                vec![button("hkc-ok", "Close", ButtonKind::Secondary, p)
                    .on_click(cx.listener(|this, _, window, cx| this.dismiss(window, cx)))
                    .into_any_element()],
            ),
            ActivePrompt::Input { label, error, input, remember, .. } => card(
                label,
                div()
                    .flex()
                    .flex_col()
                    .gap(px(8.))
                    .child(Input::new(input))
                    .when_some(error.clone(), |el, error| {
                        el.child(div().text_size(px(12.)).text_color(p.danger).child(error))
                    })
                    .when_some(*remember, |el, ticked| {
                        el.child(
                            div()
                                .id("in-remember")
                                .flex()
                                .items_center()
                                .gap(px(8.))
                                .mt(px(2.))
                                .cursor_pointer()
                                .on_click(cx.listener(|this, _, window, cx| this.toggle_remember(window, cx)))
                                .child(
                                    div()
                                        .size(px(16.))
                                        .rounded(px(4.))
                                        .border_1()
                                        .border_color(if ticked { p.brand } else { p.border })
                                        .when(ticked, |el| el.bg(p.brand))
                                        .flex()
                                        .items_center()
                                        .justify_center()
                                        .when(ticked, |el| el.child(icon_el("check", 11., p.on_brand))),
                                )
                                .child(div().text_size(px(13.)).text_color(p.text).child("Remember for this host")),
                        )
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
                        .on_click(cx.listener(|this, _, window, cx| {
                            if let Some(ActivePrompt::ConfirmPaste { text, .. }) = this.prompt.take() {
                                this.paste_text(&text);
                            }
                            this.dismiss(window, cx);
                        }))
                        .into_any_element(),
                ],
            ),
        };
        Some(element)
    }

    fn render_disconnected(&self, cx: &Context<Self>) -> Option<gpui::AnyElement> {
        let Status::Disconnected(reason) = &self.status else { return None };
        // The route page shows where and why the connection failed.
        if self.route.is_some() {
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
                    div()
                        .flex()
                        .items_center()
                        .gap(px(10.))
                        .px(px(14.))
                        .py(px(9.))
                        .rounded(px(10.))
                        .bg(p.surface)
                        .border_1()
                        .border_color(p.border)
                        .shadow(crate::ui::widgets::elevation(p.dark))
                        .font_family(UI_FONT)
                        .text_size(px(13.))
                        .child(div().size(px(8.)).rounded_full().bg(p.text_faint))
                        .child(div().text_color(p.text).child(reason.clone()))
                        .child(div().text_color(p.text_muted).child("Press Enter to reconnect")),
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
    /// Zero-width characters on this cell: combining accents, variation selectors, ZWJ.
    extra: Option<Box<[char]>>,
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
    text_system: &'a gpui::WindowTextSystem,
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
    cursor: Option<CursorSnap>,
    focused: bool,
}

/// Buffers `layout_row` reuses from row to row, so a frame does not allocate per row.
#[derive(Default)]
struct RowScratch {
    /// The row's text and where each cell starts in it, for highlight rules.
    text: String,
    starts: Vec<usize>,
    /// (first column, end column, colour)
    backgrounds: Vec<(usize, usize, Rgb)>,
    selected: Vec<(usize, usize)>,
    prims: Vec<Prim>,
}

struct GlyphPaint {
    origin: gpui::Point<Pixels>,
    font_id: gpui::FontId,
    id: gpui::GlyphId,
    color: Hsla,
    emoji: bool,
}

#[derive(Clone, Copy, PartialEq)]
enum DecorationKind {
    Underline,
    Double,
    Curly,
    Strike,
}

struct Decoration {
    kind: DecorationKind,
    x0: Pixels,
    x1: Pixels,
    y: Pixels,
    color: Hsla,
}

#[derive(Default)]
struct Frame {
    /// Backgrounds, selection and the block cursor, under the text.
    rects: Vec<(Bounds<Pixels>, Hsla)>,
    /// Box drawing and block elements.
    prims: Vec<(Prim, Hsla)>,
    glyphs: Vec<GlyphPaint>,
    decorations: Vec<Decoration>,
    /// Bar and underline cursors, over the text.
    cursor_bars: Vec<(Bounds<Pixels>, Hsla)>,
    cursor_outline: Option<(Bounds<Pixels>, Hsla)>,
    font_size: Pixels,
}

/// Lay out one row: merged background runs, glyphs placed on their cells, procedurally
/// drawn line art, underlines and the cursor.
///
/// Glyphs are put on the grid one cell at a time instead of trusting the shaper's advances:
/// a character from a fallback font (CJK, emoji, symbols, Nerd Font icons) has its own
/// width, and would shift everything after it on the line out of its column.
fn layout_row(
    cells: &[SnapCell],
    row: i32,
    ctx: &RowContext,
    glyphs: &mut GlyphCache,
    frame: &mut Frame,
    scratch: &mut RowScratch,
) {
    let y = ctx.origin.y + ctx.line_h * row as f32;
    let col_x = |col: usize| ctx.origin.x + ctx.cell_w * col as f32;
    let cell_width = |cell: &SnapCell| if cell.flags.contains(Flags::WIDE_CHAR) { 2 } else { 1 };

    let RowScratch { text, starts, backgrounds, selected, prims } = scratch;
    backgrounds.clear();
    selected.clear();
    // Highlight rules match on the row's text; build it only when there are rules.
    let styles = ctx.highlighter.and_then(|h| {
        text.clear();
        starts.clear();
        for cell in cells {
            starts.push(text.len());
            text.push(cell.c);
            if let Some(extra) = &cell.extra {
                text.extend(extra.iter());
            }
        }
        h.styles(text)
    });

    let cursor_here = ctx.cursor.filter(|k| k.row == row);
    let block_cursor = ctx.focused && cursor_here.is_some_and(|k| k.shape == CursorShape::Block);

    for (i, cell) in cells.iter().enumerate() {
        let (mut fg, mut bg) = (cell.fg, cell.bg);
        let mut bold = cell.flags.contains(Flags::BOLD);
        if let Some(rule) = styles.as_ref().and_then(|s| s[starts[i]]) {
            if let Some(c) = rule.fg {
                // A rule with its own background keeps its colours.
                let c = if rule.bg.is_some() { c } else { highlight::readable_on(c, ctx.dark) };
                fg = Rgb { r: c.r, g: c.g, b: c.b };
            }
            if let Some(c) = rule.bg {
                bg = Rgb { r: c.r, g: c.g, b: c.b };
            }
            bold |= rule.bold;
        }
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
        if cell.flags.contains(Flags::DIM) {
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
        if !drawn && (cell.c != ' ' || cell.extra.is_some()) {
            let style = bold as usize + 2 * cell.flags.contains(Flags::ITALIC) as usize;
            for g in glyphs.get(ctx, style, cell).iter() {
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
            _ if !ctx.focused => frame.cursor_outline = Some((bounds, color)),
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

/// One glyph of a shaped cell, relative to the cell's left edge and the baseline.
#[derive(Clone, Copy)]
struct CachedGlyph {
    font_id: gpui::FontId,
    id: gpui::GlyphId,
    x: Pixels,
    y: Pixels,
    emoji: bool,
}

/// The glyphs for each character (or base character with its combining marks) in each style,
/// shaped once and reused. Text is placed cell by cell, so shaping whole rows every frame
/// would be thrown away; during heavy output, when every row is new each frame, that
/// shaping was most of the time spent drawing.
#[derive(Default)]
struct GlyphCache {
    /// The font family and size the entries were shaped with.
    font: Option<(SharedString, Pixels)>,
    chars: HashMap<(usize, char), Box<[CachedGlyph]>>,
    clusters: HashMap<(usize, String), Box<[CachedGlyph]>>,
}

impl GlyphCache {
    /// Start a frame: forget everything if the font changed, and keep the cache bounded.
    fn prepare(&mut self, family: &SharedString, size: Pixels) {
        let font = Some((family.clone(), size));
        if self.font != font || self.chars.len() > 20_000 || self.clusters.len() > 5_000 {
            self.font = font;
            self.chars.clear();
            self.clusters.clear();
        }
    }

    fn get(&mut self, ctx: &RowContext, style: usize, cell: &SnapCell) -> &[CachedGlyph] {
        let shape = |text: &str| -> Box<[CachedGlyph]> {
            let run = TextRun {
                len: text.len(),
                font: ctx.fonts[style].clone(),
                color: Hsla::black(),
                background_color: None,
                underline: None,
                strikethrough: None,
            };
            let layout = ctx.text_system.layout_line(text, ctx.font_size, &[run], None);
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
        };
        match &cell.extra {
            None => self.chars.entry((style, cell.c)).or_insert_with(|| shape(cell.c.encode_utf8(&mut [0; 4]))),
            Some(extra) => {
                let mut text = String::with_capacity(16);
                text.push(cell.c);
                text.extend(extra.iter());
                self.clusters.entry((style, text)).or_insert_with_key(|(_, text)| shape(text))
            }
        }
    }
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
        let term_bg = palette.background_hsla();
        let focused = self.focus.is_focused(window) && self.prompt.is_none() && window.is_window_active();
        // Colour queries (OSC 10/11/4) are answered with the colours on screen.
        self.proxy.set_palette(&palette);
        let mouse_reporting = self.mouse_mode(false).is_some();

        let term = self.term.clone();
        let sink = self.sink.clone();
        let proxy = self.proxy.clone();
        let grid = self.grid.clone();
        let this = cx.entity().downgrade();
        let this_drag = this.clone();
        let origin_cell = self.origin.clone();
        let cell_cell = self.cell.clone();
        let glyph_cache = self.glyph_cache.clone();
        let highlighter = cx.try_global::<ActiveHighlights>().map(|h| h.0.clone()).unwrap_or_default();
        let (rail, flash_row) = crate::perf::time("command rail", || self.render_rail(cx));
        // Laid over the terminal's right edge, sized from the last layout's width.
        let text_width = self.grid.get().map_or(800., |g| g.cols as f32 * g.cell_width as f32) + PADDING_X * 2.;
        let minimap = self
            .minimap
            .then(|| {
                crate::ui::minimap::minimap(
                    self.term.clone(),
                    self.minimap_state.clone(),
                    palette.clone(),
                    cx.global::<Appearance>().app,
                    highlighter.clone(),
                    crate::ui::minimap::width_for(text_width),
                    cx,
                )
            })
            .flatten();
        let flash_color = Hsla { a: palette.selection.a * 0.7, ..palette.selection };

        let surface = canvas(
            move |bounds, window, cx| crate::perf::time("terminal grid prepaint", || {
                let text_system = window.text_system().clone();
                let mut regular = gpui::font(family.clone());
                regular.features = gpui::FontFeatures::disable_ligatures();
                let font_id = text_system.resolve_font(&regular);
                let cell_w = text_system
                    .advance(font_id, font_size, 'm')
                    .map(|s| s.width)
                    .unwrap_or(px(8.));
                let line_h = (font_size * 1.32).round();
                let cw = f32::from(cell_w);
                let lh = f32::from(line_h);
                origin_cell.set(bounds.origin);
                cell_cell.set((cw, lh));

                let cols = ((f32::from(bounds.size.width) / cw).floor() as u16).max(2);
                let rows = ((f32::from(bounds.size.height) / lh).floor() as u16).max(1);
                let new_size = GridSize { cols, rows, cell_width: cw.round() as u16, cell_height: lh as u16 };
                if grid.get() != Some(new_size) {
                    grid.set(Some(new_size));
                    {
                        let mut term = term.lock();
                        term.resize(new_size.term_size());
                        // Fewer rows push lines into the Scrollback; blank ones are dead space.
                        crate::session::drop_blank_scrollback(&mut *term);
                    }
                    proxy.set_size(new_size);
                    if let Some(sink) = &sink {
                        sink.resize(new_size);
                    }
                    let this = this.clone();
                    cx.defer(move |cx| {
                        let _ = this.update(cx, |_, cx| cx.emit(TerminalEvent::Sized));
                    });
                }

                // Copy what is on screen and let go of the terminal before laying out text.
                let (cells, cursor, mode) = {
                    let term = term.lock();
                    let content = term.renderable_content();
                    let colors = content.colors;
                    let offset = content.display_offset as i32;
                    let selection = content.selection;
                    let cursor_point = content.cursor.point;
                    let mut cursor_wide = false;
                    let mut cells = Vec::with_capacity(cols as usize * rows as usize);
                    for indexed in content.display_iter {
                        let cell = indexed.cell;
                        if cell.flags.contains(Flags::WIDE_CHAR_SPACER) {
                            continue;
                        }
                        if indexed.point == cursor_point && cell.flags.contains(Flags::WIDE_CHAR) {
                            cursor_wide = true;
                        }
                        let mut fg = palette.resolve(cell.fg, colors);
                        let mut bg = palette.resolve(cell.bg, colors);
                        if cell.flags.contains(Flags::INVERSE) {
                            std::mem::swap(&mut fg, &mut bg);
                        }
                        let hidden = cell.flags.contains(Flags::HIDDEN);
                        cells.push(SnapCell {
                            row: indexed.point.line.0 + offset,
                            col: indexed.point.column.0,
                            c: if hidden || cell.c == '\t' { ' ' } else { cell.c },
                            extra: cell.zerowidth().filter(|_| !hidden).map(Into::into),
                            fg,
                            bg,
                            underline: cell.underline_color().map(|c| palette.resolve(c, colors)),
                            flags: cell.flags,
                            selected: selection.is_some_and(|s| s.contains(indexed.point)),
                        });
                    }
                    let cursor_row = cursor_point.line.0 + offset;
                    let cursor = (content.cursor.shape != CursorShape::Hidden && (0..rows as i32).contains(&cursor_row))
                        .then(|| CursorSnap {
                            row: cursor_row,
                            col: cursor_point.column.0,
                            shape: content.cursor.shape,
                            wide: cursor_wide,
                            color: palette.resolve(Color::Named(NamedColor::Cursor), colors),
                        });
                    (cells, cursor, content.mode)
                };

                let ascent = text_system.ascent(font_id, font_size);
                let descent = text_system.descent(font_id, font_size);
                let metrics = RowTextMetrics::new(line_h, ascent, descent);
                let bold = |mut f: gpui::Font| {
                    f.weight = FontWeight::BOLD;
                    f
                };
                let italic = |mut f: gpui::Font| {
                    f.style = FontStyle::Italic;
                    f
                };
                // Highlight rules colour plain output; full-screen programs (vim, htop) are left alone.
                let highlight = !mode.contains(TermMode::ALT_SCREEN) && highlighter.rule_count() > 0;
                let ctx = RowContext {
                    text_system: &text_system,
                    fonts: [regular.clone(), bold(regular.clone()), italic(regular.clone()), bold(italic(regular))],
                    font_size,
                    origin: bounds.origin,
                    cell_w,
                    line_h,
                    scale: window.scale_factor(),
                    baseline: metrics.baseline,
                    underline_y: metrics.underline_y,
                    strike_y: metrics.strike_y,
                    highlighter: highlight.then_some(&*highlighter),
                    dark: palette.dark,
                    default_bg: palette.background,
                    selection: palette.selection,
                    cursor,
                    focused,
                };

                let mut frame = Frame { font_size, ..Frame::default() };
                if let Some(row) = flash_row {
                    let y = bounds.origin.y + line_h * row as f32;
                    frame.rects.push((Bounds::new(point(bounds.origin.x, y), size(bounds.size.width, line_h)), flash_color));
                }
                let mut scratch = RowScratch::default();
                let mut glyphs = glyph_cache.borrow_mut();
                glyphs.prepare(&family, font_size);
                let mut start = 0;
                while start < cells.len() {
                    let row = cells[start].row;
                    let end = cells[start..].iter().position(|c| c.row != row).map_or(cells.len(), |n| start + n);
                    if row >= 0 {
                        layout_row(&cells[start..end], row, &ctx, &mut glyphs, &mut frame, &mut scratch);
                    }
                    start = end;
                }
                for d in &mut frame.decorations {
                    d.y += if d.kind == DecorationKind::Strike { ctx.strike_y } else { ctx.underline_y };
                }
                frame
            }),
            move |bounds, frame, window, _cx| crate::perf::time("terminal grid paint", || {
                // A selection keeps following the pointer once it leaves the terminal.
                window.on_mouse_event(move |event: &MouseMoveEvent, phase, _window, cx| {
                    if phase == gpui::DispatchPhase::Bubble {
                        let _ = this_drag.update(cx, |this, cx| this.drag_selection(event, cx));
                    }
                });
                // One layer for the whole grid: everything in it shares one draw order and
                // is drawn by kind (backgrounds, line art, underlines, then text), instead of
                // each of thousands of glyphs being placed in the scene on its own.
                window.paint_layer(bounds, |window| {
                    for (bounds, color) in &frame.rects {
                        window.paint_quad(fill(*bounds, *color));
                    }
                    for (prim, color) in &frame.prims {
                        paint_prim(window, prim, *color);
                    }
                    for d in &frame.decorations {
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
                    for g in &frame.glyphs {
                        let _ = if g.emoji {
                            window.paint_emoji(g.origin, g.font_id, g.id, frame.font_size)
                        } else {
                            window.paint_glyph(g.origin, g.font_id, g.id, frame.font_size, g.color)
                        };
                    }
                });
                // The cursor goes on top of the text.
                for (bounds, color) in &frame.cursor_bars {
                    window.paint_quad(fill(*bounds, *color));
                }
                if let Some((bounds, color)) = frame.cursor_outline {
                    window.paint_quad(outline(bounds, color, gpui::BorderStyle::Solid));
                }
            }),
        )
        .size_full();

        div()
            .id("terminal")
            .key_context(CONTEXT)
            .track_focus(&self.focus)
            .on_key_down(cx.listener(Self::key_down))
            .on_action(cx.listener(Self::copy))
            .on_action(cx.listener(Self::paste))
            .on_action(cx.listener(|this, _: &ScrollPageUp, _, cx| this.scroll_page(true, cx)))
            .on_action(cx.listener(|this, _: &ScrollPageDown, _, cx| this.scroll_page(false, cx)))
            .on_mouse_down(MouseButton::Left, cx.listener(Self::mouse_down))
            .on_mouse_down(MouseButton::Middle, cx.listener(Self::mouse_down))
            .on_mouse_down(MouseButton::Right, cx.listener(Self::mouse_down))
            .on_mouse_move(cx.listener(Self::mouse_move))
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
            // Inside a rounded island: round our own background too, 1px in for the border.
            .rounded(px(crate::ui::widgets::ISLAND_RADIUS - 1.))
            .bg(term_bg)
            // A program that takes the mouse gets an arrow, as in xterm; shift still selects.
            .map(|el| if mouse_reporting { el.cursor_default() } else { el.cursor_text() })
            .child(div().size_full().px(px(PADDING_X)).py(px(PADDING_Y)).child(surface))
            .children(minimap)
            .children(rail)
            .children(self.render_disconnected(cx))
            .children(self.render_route(cx))
    }
}

#[cfg(test)]
mod tests {
    use super::{RowTextMetrics, asks_for_secret};
    use gpui::px;

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
}
