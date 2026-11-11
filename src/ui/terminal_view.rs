//! One terminal pane: paints the grid on the GPU and turns input into bytes for its Session.

use std::cell::Cell;
use std::rc::Rc;
use std::time::Duration;

use alacritty_terminal::grid::{Dimensions as _, Scroll};
use alacritty_terminal::index::{Column, Line, Point as GridPoint, Side};
use alacritty_terminal::selection::{Selection, SelectionType};
use alacritty_terminal::term::TermMode;
use alacritty_terminal::term::cell::Flags;
use alacritty_terminal::vte::ansi::Rgb;
use futures::StreamExt as _;
use futures::channel::{mpsc, oneshot};
use gpui::prelude::FluentBuilder as _;
use gpui::{
    App, Bounds, ClipboardItem, Context, Entity, EventEmitter, FocusHandle, Focusable, FontStyle, FontWeight,
    Hsla, KeyDownEvent, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, Pixels, ScrollEvent,
    ShapedLine, SharedString, Subscription, TextAlign, TextRun, UnderlineStyle, Window, actions, canvas, div,
    fill, outline, point, prelude::*, px, size,
};
use gpui_component::input::{Input, InputEvent, InputState};

use crate::highlight::{self, ActiveHighlights};
use crate::keys::{self, Mods};
use crate::marks::Marks;
use crate::session::{
    EventProxy, GridSize, InputSink, Prompt, SessionEvent, SharedTerm, Status, new_term, scrollback_text,
};
use crate::theme::{Appearance, UI_FONT, alpha, rgb_to_hsla};
use crate::ui::widgets::{button, ButtonKind};

actions!(terminal, [Copy, Paste, ScrollPageUp, ScrollPageDown]);

pub const CONTEXT: &str = "Terminal";
const PADDING_X: f32 = 12.0;
const PADDING_Y: f32 = 8.0;
/// Height each command takes on the rail: its line plus the gap below.
const RAIL_STEP: f32 = 8.0;
/// How long a row stays lit after jumping to it.
const FLASH: Duration = Duration::from_millis(1400);

/// What a terminal tells the workspace about.
pub enum TerminalEvent {
    /// Status or title changed: refresh the sidebar.
    Changed,
    SavePassword { password: String, as_project: bool },
    Reconnect,
}

enum ActivePrompt {
    HostKey { fingerprint: String, algorithm: String, reply: Option<oneshot::Sender<bool>> },
    HostKeyChanged { fingerprint: String },
    Input { label: String, error: Option<String>, input: Entity<InputState>, reply: Option<oneshot::Sender<Option<String>>> },
    OfferSave { password: String },
    ConfirmPaste { text: String, lines: usize },
}

pub struct TerminalView {
    pub title: SharedString,
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
    /// Whether this session can offer "save as project password".
    pub has_project: bool,
    pub confirm_multiline_paste: bool,
    /// PuTTY-style: selecting copies, right-click pastes.
    pub putty_clipboard: bool,
    /// Show the command rail and record entered lines for it.
    pub command_rail: bool,
    /// Show the minimap on the right edge.
    pub minimap: bool,
    minimap_state: Rc<crate::ui::minimap::MinimapState>,
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
        let term = new_term(initial, scrollback, proxy.clone());
        Self::listen(rx, window, cx);
        Self {
            title: title.into(),
            status: Status::Connecting,
            term,
            proxy,
            sink: None,
            focus: cx.focus_handle(),
            grid: Rc::new(Cell::new(None)),
            origin: Rc::new(Cell::new(point(px(0.), px(0.)))),
            cell: Rc::new(Cell::new((8.0, 18.0))),
            prompt: None,
            selecting: false,
            has_project: true,
            confirm_multiline_paste: true,
            putty_clipboard: false,
            command_rail: true,
            minimap: true,
            minimap_state: Rc::new(Default::default()),
            marks: Marks::default(),
            hovered_mark: None,
            flash: None,
            _subscriptions: vec![],
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
    pub fn grid_size(&self) -> GridSize {
        self.grid.get().unwrap_or(GridSize { cols: 120, rows: 36, cell_width: 8, cell_height: 18 })
    }

    pub fn attach(&mut self, sink: InputSink) {
        self.sink = Some(sink);
    }

    pub fn close(&mut self) {
        if let Some(sink) = self.sink.take() {
            sink.close();
        }
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
                self.proxy.clear_dirty();
                if self.marks.is_pending() {
                    self.marks.output(&mut *self.term.lock());
                }
                cx.notify();
            }
            SessionEvent::Title(title) => {
                if !title.is_empty() {
                    self.title = title.into();
                    cx.emit(TerminalEvent::Changed);
                }
            }
            SessionEvent::Status(status) => {
                if self.status != status {
                    self.status = status;
                    if self.is_disconnected() {
                        // A pending question can no longer be answered.
                        if matches!(self.prompt, Some(ActivePrompt::Input { .. } | ActivePrompt::HostKey { .. })) {
                            self.prompt = None;
                        }
                    }
                    cx.emit(TerminalEvent::Changed);
                    cx.notify();
                }
            }
            SessionEvent::Prompt(prompt) => self.show_prompt(prompt, window, cx),
            SessionEvent::OfferSave { password } => {
                self.prompt = Some(ActivePrompt::OfferSave { password });
                cx.notify();
            }
            SessionEvent::ClipboardStore(text) => cx.write_to_clipboard(ClipboardItem::new_string(text)),
        }
    }

    fn show_prompt(&mut self, prompt: Prompt, window: &mut Window, cx: &mut Context<Self>) {
        self.prompt = Some(match prompt {
            Prompt::HostKey { fingerprint, algorithm, reply } => {
                ActivePrompt::HostKey { fingerprint, algorithm, reply: Some(reply) }
            }
            Prompt::HostKeyChanged { fingerprint } => ActivePrompt::HostKeyChanged { fingerprint },
            Prompt::Input { label, secret, error, reply } => {
                let input = cx.new(|cx| InputState::new(window, cx).masked(secret).placeholder(label.clone()));
                let sub = cx.subscribe_in(&input, window, |this, _, event: &InputEvent, window, cx| {
                    if let InputEvent::PressEnter { .. } = event {
                        this.answer_input(true, window, cx);
                    }
                });
                self._subscriptions.push(sub);
                input.update(cx, |state, cx| state.focus(window, cx));
                ActivePrompt::Input { label, error, input, reply: Some(reply) }
            }
        });
        cx.notify();
    }

    fn answer_input(&mut self, submit: bool, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(ActivePrompt::Input { input, reply, .. }) = self.prompt.as_mut() {
            let value = input.read(cx).value().to_string();
            if let Some(reply) = reply.take() {
                let _ = reply.send(submit.then_some(value));
            }
            self.prompt = None;
            window.focus(&self.focus, cx);
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
            self.write(bytes);
            cx.stop_propagation();
            cx.notify();
        }
        let _ = window;
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

    fn grid_point(&self, position: gpui::Point<Pixels>) -> (GridPoint, Side) {
        let origin = self.origin.get();
        let (cw, ch) = self.cell.get();
        let size = self.grid_size();
        let x = (f32::from(position.x - origin.x) / cw).max(0.0);
        let y = (f32::from(position.y - origin.y) / ch).max(0.0);
        let col = (x as usize).min(size.cols.saturating_sub(1) as usize);
        let row = (y as usize).min(size.rows.saturating_sub(1) as usize);
        let side = if x.fract() < 0.5 { Side::Left } else { Side::Right };
        let offset = self.term.lock().grid().display_offset() as i32;
        (GridPoint::new(Line(row as i32 - offset), Column(col)), side)
    }

    fn mouse_down(&mut self, event: &MouseDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        window.focus(&self.focus, cx);
        let (pt, side) = self.grid_point(event.position);
        let kind = match event.click_count {
            2 => SelectionType::Semantic,
            3 => SelectionType::Lines,
            _ => SelectionType::Simple,
        };
        self.term.lock().selection = Some(Selection::new(kind, pt, side));
        self.selecting = true;
        cx.notify();
    }

    fn mouse_move(&mut self, event: &MouseMoveEvent, _: &mut Window, cx: &mut Context<Self>) {
        if !self.selecting || event.pressed_button != Some(MouseButton::Left) {
            return;
        }
        let (pt, side) = self.grid_point(event.position);
        if let Some(selection) = self.term.lock().selection.as_mut() {
            selection.update(pt, side);
        }
        cx.notify();
    }

    fn mouse_up(&mut self, _: &MouseUpEvent, _: &mut Window, cx: &mut Context<Self>) {
        self.selecting = false;
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

    fn right_click(&mut self, _: &MouseDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        if self.putty_clipboard {
            window.focus(&self.focus, cx);
            self.paste(&Paste, window, cx);
        }
    }

    fn scroll_wheel(&mut self, event: &ScrollEvent, _: &mut Window, cx: &mut Context<Self>) {
        let (_, ch) = self.cell.get();
        let dy = f32::from(event.delta.pixel_delta(px(ch)).y);
        let lines = (dy / ch).round() as i32;
        if lines != 0 {
            let mut term = self.term.lock();
            if term.mode().contains(TermMode::ALT_SCREEN) {
                drop(term);
                // Full-screen programs (less, vim): send arrow keys instead of scrolling history.
                let key = if lines > 0 { b"\x1b[A".to_vec() } else { b"\x1b[B".to_vec() };
                for _ in 0..lines.abs().min(10) {
                    self.write(key.clone());
                }
            } else {
                term.scroll_display(Scroll::Delta(lines));
            }
            cx.notify();
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

    fn render_prompt(&self, cx: &mut Context<Self>) -> Option<gpui::AnyElement> {
        let p = cx.global::<Appearance>().app;
        let prompt = self.prompt.as_ref()?;
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
                "Trust this server?",
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
            ActivePrompt::Input { label, error, input, .. } => card(
                label,
                div()
                    .flex()
                    .flex_col()
                    .gap(px(8.))
                    .child(Input::new(input))
                    .when_some(error.clone(), |el, error| {
                        el.child(div().text_size(px(12.)).text_color(p.danger).child(error))
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
            ActivePrompt::OfferSave { .. } => {
                let mut actions = vec![
                    button("sv-no", "Not now", ButtonKind::Ghost, p)
                        .on_click(cx.listener(|this, _, window, cx| this.dismiss(window, cx)))
                        .into_any_element(),
                ];
                if self.has_project {
                    actions.push(
                        button("sv-project", "As project password", ButtonKind::Secondary, p)
                            .on_click(cx.listener(|this, _, window, cx| this.save_password(true, window, cx)))
                            .into_any_element(),
                    );
                }
                actions.push(
                    button("sv-host", "Save for this host", ButtonKind::Primary, p)
                        .on_click(cx.listener(|this, _, window, cx| this.save_password(false, window, cx)))
                        .into_any_element(),
                );
                card(
                    "Save this password?",
                    text("It is stored encrypted in your vault and filled in automatically next time.".into()),
                    actions,
                )
            }
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
        Some(
            div()
                .absolute()
                .inset_0()
                .rounded(px(crate::ui::widgets::ISLAND_RADIUS - 1.))
                .flex()
                .items_center()
                .justify_center()
                .bg(alpha(0x000000, if p.dark { 0.45 } else { 0.18 }))
                .child(element)
                .into_any_element(),
        )
    }

    fn save_password(&mut self, as_project: bool, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(ActivePrompt::OfferSave { password }) = self.prompt.take() {
            cx.emit(TerminalEvent::SavePassword { password, as_project });
        }
        self.dismiss(window, cx);
    }

    fn render_disconnected(&self, cx: &Context<Self>) -> Option<gpui::AnyElement> {
        let Status::Disconnected(reason) = &self.status else { return None };
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

struct RowCell {
    point: GridPoint,
    ch: char,
    fg: Rgb,
    bg: Rgb,
    flags: Flags,
}

struct Frame {
    rects: Vec<(Bounds<Pixels>, Hsla)>,
    lines: Vec<(gpui::Point<Pixels>, ShapedLine)>,
    cursor_outline: Option<(Bounds<Pixels>, Hsla)>,
    line_height: Pixels,
}

impl Render for TerminalView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let appearance = cx.global::<Appearance>();
        let palette = appearance.term.clone();
        let family = appearance.font_family.clone();
        let font_size = px(appearance.font_size);
        let term_bg = palette.background_hsla();
        let focused = self.focus.is_focused(window) && self.prompt.is_none();

        let term = self.term.clone();
        let sink = self.sink.clone();
        let grid = self.grid.clone();
        let origin_cell = self.origin.clone();
        let cell_cell = self.cell.clone();
        let highlighter = cx.try_global::<ActiveHighlights>().map(|h| h.0.clone()).unwrap_or_default();
        let (rail, flash_row) = self.render_rail(cx);
        let minimap = self.minimap.then(|| {
            crate::ui::minimap::minimap(
                self.term.clone(),
                self.minimap_state.clone(),
                palette.clone(),
                cx.global::<Appearance>().app,
                highlighter.clone(),
                cx,
            )
        });
        let flash_color = Hsla { a: palette.selection.a * 0.7, ..palette.selection };

        let surface = canvas(
            move |bounds, window, _cx| {
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
                    term.lock().resize(new_size.term_size());
                    if let Some(sink) = &sink {
                        sink.resize(new_size);
                    }
                }

                let term = term.lock();
                let content = term.renderable_content();
                let colors = content.colors;
                let offset = content.display_offset as i32;
                let selection = content.selection;
                let show_cursor = content.mode.contains(TermMode::SHOW_CURSOR) && offset == 0;
                let cursor = content.cursor.point;
                let default_bg = palette.background;

                // Highlight rules colour plain output; full-screen programs (vim, htop) are left alone.
                let highlight = !content.mode.contains(TermMode::ALT_SCREEN) && highlighter.rule_count() > 0;
                let mut frame = Frame { rects: vec![], lines: vec![], cursor_outline: None, line_height: line_h };
                if let Some(row) = flash_row {
                    let y = bounds.origin.y + line_h * row as f32;
                    frame.rects.push((Bounds::new(point(bounds.origin.x, y), size(bounds.size.width, line_h)), flash_color));
                }
                let mut cells: Vec<RowCell> = Vec::with_capacity(cols as usize);
                let mut current_row: i32 = i32::MIN;

                let emit = |row: i32, cells: &mut Vec<RowCell>, frame: &mut Frame| {
                    if cells.is_empty() {
                        return;
                    }
                    let text: String = cells.iter().map(|c| c.ch).collect();
                    let styles = if highlight { highlighter.styles(&text) } else { None };
                    let mut runs: Vec<TextRun> = Vec::new();
                    let mut byte = 0;
                    for cell in cells.iter() {
                        let len = cell.ch.len_utf8();
                        let rule = styles.as_ref().and_then(|s| s[byte]);
                        byte += len;
                        let (mut fg, mut bg) = (cell.fg, cell.bg);
                        let mut bold = cell.flags.contains(Flags::BOLD);
                        if let Some(rule) = rule {
                            if let Some(c) = rule.fg {
                                // A rule with its own background keeps its colours.
                                let c = if rule.bg.is_some() { c } else { highlight::readable_on(c, palette.dark) };
                                fg = Rgb { r: c.r, g: c.g, b: c.b };
                            }
                            if let Some(c) = rule.bg {
                                bg = Rgb { r: c.r, g: c.g, b: c.b };
                            }
                            bold |= rule.bold;
                        }
                        let is_cursor = show_cursor && cell.point == cursor;
                        if is_cursor && focused {
                            fg = palette.background;
                            bg = palette.cursor;
                        }
                        let col = cell.point.column.0 as f32;
                        let cell_bounds = Bounds::new(
                            point(bounds.origin.x + cell_w * col, bounds.origin.y + line_h * row as f32),
                            size(cell_w * if cell.flags.contains(Flags::WIDE_CHAR) { 2.0 } else { 1.0 }, line_h),
                        );
                        if is_cursor && !focused {
                            frame.cursor_outline = Some((cell_bounds, rgb_to_hsla(palette.cursor)));
                        }
                        if bg != default_bg || (is_cursor && focused) {
                            frame.rects.push((cell_bounds, rgb_to_hsla(bg)));
                        }
                        if selection.is_some_and(|s| s.contains(cell.point)) {
                            frame.rects.push((cell_bounds, palette.selection));
                        }

                        let mut color = rgb_to_hsla(fg);
                        if cell.flags.contains(Flags::DIM) {
                            color.a *= 0.66;
                        }
                        let mut font = regular.clone();
                        if bold {
                            font.weight = FontWeight::BOLD;
                        }
                        if cell.flags.contains(Flags::ITALIC) {
                            font.style = FontStyle::Italic;
                        }
                        let underline = cell.flags.intersects(Flags::ALL_UNDERLINES).then(|| UnderlineStyle {
                            thickness: px(1.),
                            color: Some(color),
                            wavy: cell.flags.contains(Flags::UNDERCURL),
                        });
                        match runs.last_mut() {
                            Some(last) if last.color == color && last.font == font && last.underline == underline => {
                                last.len += len
                            }
                            _ => runs.push(TextRun {
                                len,
                                font,
                                color,
                                background_color: None,
                                underline,
                                strikethrough: None,
                            }),
                        }
                    }
                    cells.clear();
                    if row >= 0 {
                        let shaped = text_system.shape_line(SharedString::from(text), font_size, &runs, None);
                        frame.lines.push((point(bounds.origin.x, bounds.origin.y + line_h * row as f32), shaped));
                    }
                };

                for indexed in content.display_iter {
                    let row = indexed.point.line.0 + offset;
                    if row != current_row {
                        emit(current_row, &mut cells, &mut frame);
                        current_row = row;
                    }
                    let cell = indexed.cell;
                    if cell.flags.contains(Flags::WIDE_CHAR_SPACER) {
                        continue;
                    }
                    let mut fg = palette.resolve(cell.fg, colors);
                    let mut bg = palette.resolve(cell.bg, colors);
                    if cell.flags.contains(Flags::INVERSE) {
                        std::mem::swap(&mut fg, &mut bg);
                    }
                    let ch = if cell.flags.contains(Flags::HIDDEN) || cell.c == '\t' { ' ' } else { cell.c };
                    cells.push(RowCell { point: indexed.point, ch, fg, bg, flags: cell.flags });
                }
                emit(current_row, &mut cells, &mut frame);
                frame
            },
            move |_bounds, frame, window, cx| {
                for (bounds, color) in &frame.rects {
                    window.paint_quad(fill(*bounds, *color));
                }
                for (origin, line) in &frame.lines {
                    let _ = line.paint(*origin, frame.line_height, TextAlign::Left, None, window, cx);
                }
                if let Some((bounds, color)) = frame.cursor_outline {
                    window.paint_quad(outline(bounds, color, gpui::BorderStyle::Solid));
                }
            },
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
            .on_mouse_move(cx.listener(Self::mouse_move))
            .on_mouse_up(MouseButton::Left, cx.listener(Self::mouse_up))
            .on_mouse_down(MouseButton::Right, cx.listener(Self::right_click))
            .on_scroll(cx.listener(Self::scroll_wheel))
            .relative()
            .size_full()
            // Inside a rounded island: round our own background too, 1px in for the border.
            .rounded(px(crate::ui::widgets::ISLAND_RADIUS - 1.))
            .bg(term_bg)
            .cursor_text()
            .child(
                div()
                    .size_full()
                    .flex()
                    .child(div().flex_1().min_w_0().h_full().px(px(PADDING_X)).py(px(PADDING_Y)).child(surface))
                    .children(minimap),
            )
            .children(rail)
            .children(self.render_disconnected(cx))
            .children(self.render_prompt(cx))
    }
}
