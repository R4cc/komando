//! One terminal pane: paints the grid on the GPU and turns input into bytes for its Session.

use std::cell::Cell;
use std::rc::Rc;

use alacritty_terminal::grid::Scroll;
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
use crate::session::{
    EventProxy, GridSize, InputSink, Prompt, SessionEvent, SharedTerm, Status, new_term, scrollback_text,
};
use crate::theme::{Appearance, UI_FONT, alpha, rgb_to_hsla};
use crate::ui::widgets::{button, ButtonKind};

actions!(terminal, [Copy, Paste, ScrollPageUp, ScrollPageDown]);

pub const CONTEXT: &str = "Terminal";
const PADDING_X: f32 = 12.0;
const PADDING_Y: f32 = 8.0;

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
        let app_cursor = self.term.lock().mode().contains(TermMode::APP_CURSOR);
        let mods = Mods { ctrl: m.control, alt: m.alt, shift: m.shift };
        if let Some(bytes) = keys::to_bytes(&ks.key, ks.key_char.as_deref(), mods, app_cursor) {
            self.scroll_to_bottom();
            {
                let mut term = self.term.lock();
                term.selection = None;
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
        cx.notify();
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
        let term_bg = rgb_to_hsla(palette.background);
        let focused = self.focus.is_focused(window) && self.prompt.is_none();

        let term = self.term.clone();
        let sink = self.sink.clone();
        let grid = self.grid.clone();
        let origin_cell = self.origin.clone();
        let cell_cell = self.cell.clone();
        let highlighter = cx.try_global::<ActiveHighlights>().map(|h| h.0.clone()).unwrap_or_default();

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
                                let c = highlight::readable_on(c, palette.dark);
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
            .on_scroll(cx.listener(Self::scroll_wheel))
            .relative()
            .size_full()
            // Inside a rounded island: round our own background too, 1px in for the border.
            .rounded(px(crate::ui::widgets::ISLAND_RADIUS - 1.))
            .bg(term_bg)
            .cursor_text()
            .child(div().size_full().px(px(PADDING_X)).py(px(PADDING_Y)).child(surface))
            .children(self.render_disconnected(cx))
            .children(self.render_prompt(cx))
    }
}
