//! A Remote Desktop Session's pane: the remote screen, scaled down to fit, with keyboard and
//! mouse going to it while it has focus, and cards for connecting, signing in, trusting an RDP
//! certificate and reconnecting.

use std::cell::Cell;
use std::collections::HashSet;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use futures::StreamExt as _;
use futures::channel::oneshot;
use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, AnyWindowHandle, App, Bounds, Context, Corners, Entity, EventEmitter, FocusHandle, Focusable, FontWeight, KeyDownEvent, KeyUpEvent,
    Modifiers, ModifiersChangedEvent, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, Pixels, Point, RenderImage, ScrollDelta,
    ScrollEvent, SharedString, Subscription, Task, Window, actions, canvas, div, point, prelude::*, px, size,
};
use gpui_component::input::{Input, InputEvent, InputState};
use zeroize::Zeroizing;

use crate::desktop::{self, Button, Connection, DesktopEvent, DesktopPrompt, Input as DesktopInput, Key, Protocol, SignIn, Status, Target};
use crate::theme::{Appearance};
use crate::ui::widgets::{ButtonKind, button, elevation, password_input, spinner};

/// The key context of a desktop pane, so Tab goes to the remote desktop instead of moving focus.
pub const CONTEXT: &str = "RemoteDesktop";

actions!(remote_desktop, [SendTab, SendBackTab]);

/// How long the pane must keep a new size before an RDP server is asked to follow it, so
/// dragging the window's edge doesn't resize the remote desktop at every step.
const RESIZE_SETTLE: Duration = Duration::from_millis(400);

pub enum DesktopViewEvent {
    /// Status changed (the sidebar and title bar show it).
    Changed,
    /// Signed in with a typed password and "Remember" ticked: save it for the Host.
    Remember {
        user: Option<String>,
        password: Zeroizing<String>,
    },
    Reconnect,
    EditHost,
    /// The desktop goes through the Host's SSH Session, which isn't connected: open it.
    OpenSsh,
}

impl EventEmitter<DesktopViewEvent> for DesktopView {}

enum ActivePrompt {
    SignIn {
        user: Option<Entity<InputState>>,
        password: Entity<InputState>,
        remember: bool,
        error: Option<String>,
        reply: Option<oneshot::Sender<Option<SignIn>>>,
    },
    Certificate {
        fingerprint: String,
        previous: Option<String>,
        reply: Option<oneshot::Sender<bool>>,
    },
}

/// Where the picture was last drawn, for mapping the pointer onto the remote screen.
#[derive(Clone, Copy)]
struct Placement {
    bounds: Bounds<Pixels>,
    /// Remote pixels per logical pixel.
    remote_per_px: f32,
}

pub struct DesktopView {
    pub protocol: Protocol,
    /// The Host's name, for the cards.
    pub host: SharedString,
    /// "10.0.4.21:3389", with " through SSH" when tunnelled.
    pub target: SharedString,
    pub status: Status,
    /// Ask an RDP server for a desktop the pane's size, and again when it changes.
    pub fit_desktop: bool,
    /// Waiting for the Host's SSH Session before connecting through it.
    pub waiting_for_ssh: bool,
    focus: FocusHandle,
    connection: Option<Connection>,
    _events: Option<Task<()>>,
    image: Option<Arc<RenderImage>>,
    remote_size: (u32, u32),
    drawn_version: u64,
    /// Images replaced since the last frame, removed from the GPU atlas when it is drawn.
    stale: Vec<Arc<RenderImage>>,
    prompt: Option<ActivePrompt>,
    /// The pane's bounds and the window's scale, as last laid out.
    pane: Rc<Cell<Option<(Bounds<Pixels>, f32)>>>,
    placement: Rc<Cell<Option<Placement>>>,
    /// The size last asked of the RDP server, and the timer that asks for a new one.
    requested: Option<(u32, u32)>,
    resize: Option<Task<()>>,
    /// Keys sent down and not yet up, and the modifiers the remote side has down.
    held: HashSet<u16>,
    modifiers: Modifiers,
    /// The window the pane is shown in: Komando's own, or one of its own after it was dragged
    /// out. Its prompts and pictures belong to that window (see `bind`).
    window: Option<AnyWindowHandle>,
    /// A question that came while the pane was between windows, asked once it has one.
    pending_prompt: Option<DesktopPrompt>,
    _window_subscriptions: Vec<Subscription>,
    _subscriptions: Vec<Subscription>,
}

impl Focusable for DesktopView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl DesktopView {
    pub fn new(protocol: Protocol, host: SharedString, target: SharedString, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let focus = cx.focus_handle();
        let mut this = Self {
            protocol,
            host,
            target,
            status: Status::Connecting,
            fit_desktop: true,
            waiting_for_ssh: false,
            focus,
            connection: None,
            _events: None,
            image: None,
            remote_size: (0, 0),
            drawn_version: 0,
            stale: vec![],
            prompt: None,
            pane: Rc::default(),
            placement: Rc::default(),
            requested: None,
            resize: None,
            held: HashSet::new(),
            modifiers: Modifiers::default(),
            window: None,
            pending_prompt: None,
            _window_subscriptions: vec![],
            _subscriptions: vec![],
        };
        this.bind(window, cx);
        this
    }

    /// Show the pane in `window` from now on: when it is created, and when it moves into a
    /// window of its own or back. Call `leave` on the window it was in first, while that one
    /// is still open.
    pub fn bind(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.window = Some(window.window_handle());
        // Keys held when the keyboard leaves would stay down on the remote side.
        let blur = cx.on_blur(&self.focus, window, |this, _, _| this.release_all());
        let release = cx.on_release_in(window, |this, window, _| {
            for image in this.image.take().into_iter().chain(this.stale.drain(..)) {
                let _ = window.drop_image(image);
            }
        });
        self._window_subscriptions = vec![blur, release];
        // A resize waiting to settle was measured in the other window.
        self.resize = None;
        if let Some(prompt) = self.pending_prompt.take() {
            self.show_prompt(prompt, window, cx);
        }
        cx.notify();
    }

    /// The pane is no longer shown in `window`: free the picture's copy on its GPU. The
    /// picture itself stays, and is drawn in the next window.
    pub fn leave(&mut self, window: &mut Window) {
        for image in self.image.clone().into_iter().chain(self.stale.drain(..)) {
            let _ = window.drop_image(image);
        }
        self.release_all();
    }

    /// The pane's size as last laid out.
    pub fn pane_size(&self) -> Option<gpui::Size<Pixels>> {
        self.pane.get().map(|(bounds, _)| bounds.size)
    }

    pub fn focus(&self, window: &mut Window, cx: &mut App) {
        window.focus(&self.focus, cx);
    }

    /// The size to ask a server for: the pane in device pixels, and the display's scale in
    /// percent. Before the pane was laid out, the window less the sidebar.
    pub fn desired_size(&self, window: &Window) -> (u32, u32, u32) {
        let scale = window.scale_factor();
        let logical = match self.pane.get() {
            Some((bounds, _)) => bounds.size,
            None => {
                let window = window.bounds().size;
                size(window.width - px(300.), window.height - px(60.))
            }
        };
        let device = |v: Pixels| (f32::from(v) * scale).round().max(200.) as u32;
        (device(logical.width), device(logical.height), (scale * 100.).round() as u32)
    }

    /// Start connecting, ending any connection this pane had.
    pub fn connect(&mut self, target: Target, cx: &mut Context<Self>) {
        self.close();
        self.waiting_for_ssh = false;
        self.drawn_version = 0;
        self.requested = Some(target.size);
        self.status = Status::Connecting;
        let mut connection = desktop::spawn(target);
        let mut events = std::mem::replace(&mut connection.events, futures::channel::mpsc::unbounded().1);
        self.connection = Some(connection);
        // Each event goes to the window the pane is in at the time: it may move between windows.
        self._events = Some(cx.spawn(async move |this, cx| {
            while let Some(event) = events.next().await {
                let Ok(window) = this.read_with(cx, |this, _| this.window) else { break };
                let mut event = Some(event);
                let shown = window.and_then(|window| {
                    window
                        .update(cx, |_, window, cx| {
                            let event = event.take().expect("an event");
                            this.update(cx, |this, cx| this.on_event(event, window, cx))
                        })
                        .ok()
                });
                let handled = match (shown, event) {
                    (Some(result), _) => result.is_ok(),
                    // Its window just closed: the pane is on its way back to Komando's.
                    (None, Some(event)) => this.update(cx, |this, cx| this.on_event_between_windows(event, cx)).is_ok(),
                    (None, None) => false,
                };
                if !handled {
                    break;
                }
            }
        }));
        cx.emit(DesktopViewEvent::Changed);
        cx.notify();
    }

    /// Show that the desktop waits for the Host's SSH Session.
    pub fn wait_for_ssh(&mut self, reason: &str, cx: &mut Context<Self>) {
        self.close();
        self.waiting_for_ssh = true;
        self.status = Status::Disconnected(reason.into());
        cx.emit(DesktopViewEvent::Changed);
        cx.notify();
    }

    /// End the connection and say why (the picture stays, dimmed under the reason).
    pub fn end(&mut self, reason: &str, cx: &mut Context<Self>) {
        self.close();
        self.waiting_for_ssh = false;
        self.status = Status::Disconnected(reason.into());
        cx.emit(DesktopViewEvent::Changed);
        cx.notify();
    }

    /// End the connection (the picture stays, as a terminal keeps its Scrollback).
    pub fn close(&mut self) {
        if let Some(connection) = self.connection.take() {
            connection.send(DesktopInput::Close);
        }
        self.prompt = None;
        self.pending_prompt = None;
        self._events = None;
        self.resize = None;
        self.held.clear();
        self.modifiers = Modifiers::default();
    }

    pub fn is_connected(&self) -> bool {
        matches!(self.status, Status::Connected)
    }

    pub fn send_ctrl_alt_del(&self) {
        self.send(DesktopInput::CtrlAltDel);
    }

    pub fn has_prompt(&self) -> bool {
        self.prompt.is_some()
    }

    /// Give the keyboard to the open question's field, or to the desktop.
    pub fn focus_prompt(&self, window: &mut Window, cx: &mut Context<Self>) {
        let handle = match &self.prompt {
            Some(ActivePrompt::SignIn { user: Some(user), .. }) if user.read(cx).value().is_empty() => user.focus_handle(cx),
            Some(ActivePrompt::SignIn { password, .. }) => password.focus_handle(cx),
            _ => self.focus.clone(),
        };
        if !handle.is_focused(window) {
            window.focus(&handle, cx);
        }
    }

    /// Escape on the open question: its Cancel button.
    pub fn cancel_prompt(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        match self.prompt {
            Some(ActivePrompt::SignIn { .. }) => self.answer_sign_in(false, window, cx),
            Some(ActivePrompt::Certificate { .. }) => self.answer_certificate(false, window, cx),
            None => {}
        }
    }

    fn send(&self, input: DesktopInput) {
        if let Some(connection) = &self.connection {
            connection.send(input);
        }
    }

    fn release_all(&mut self) {
        self.held.clear();
        self.modifiers = Modifiers::default();
        // A mouse drag can be held even when no key is down.
        self.send(DesktopInput::ReleaseAll);
    }

    fn on_event(&mut self, event: DesktopEvent, window: &mut Window, cx: &mut Context<Self>) {
        match event {
            DesktopEvent::Status(status) => {
                if matches!(status, Status::Disconnected(_)) {
                    self.prompt = None;
                    self.connection = None;
                }
                self.status = status;
                cx.emit(DesktopViewEvent::Changed);
            }
            DesktopEvent::Picture => {}
            DesktopEvent::Prompt(prompt) => self.show_prompt(prompt, window, cx),
            DesktopEvent::Remember { user, password } => cx.emit(DesktopViewEvent::Remember { user, password }),
        }
        cx.notify();
    }

    /// An event while the pane has no open window: a question waits for the next one.
    fn on_event_between_windows(&mut self, event: DesktopEvent, cx: &mut Context<Self>) {
        match event {
            DesktopEvent::Prompt(prompt) => {
                self.pending_prompt = Some(prompt);
                cx.notify();
            }
            DesktopEvent::Status(status) => {
                if matches!(status, Status::Disconnected(_)) {
                    self.pending_prompt = None;
                    self.connection = None;
                }
                self.status = status;
                cx.emit(DesktopViewEvent::Changed);
                cx.notify();
            }
            DesktopEvent::Picture => cx.notify(),
            DesktopEvent::Remember { user, password } => cx.emit(DesktopViewEvent::Remember { user, password }),
        }
    }

    fn show_prompt(&mut self, prompt: DesktopPrompt, window: &mut Window, cx: &mut Context<Self>) {
        match prompt {
            DesktopPrompt::SignIn { user, needs_user, error, reply } => {
                let submit = |this: &mut Self, _: &Entity<InputState>, event: &InputEvent, window: &mut Window, cx: &mut Context<Self>| {
                    if let InputEvent::PressEnter { .. } = event {
                        this.answer_sign_in(true, window, cx);
                    }
                };
                let user = needs_user.then(|| {
                    let input = cx
                        .new(|cx| InputState::new(window, cx).placeholder("User, or DOMAIN\\user").default_value(user.unwrap_or_default()));
                    self._subscriptions.push(cx.subscribe_in(&input, window, submit));
                    input
                });
                let password = cx.new(|cx| InputState::new(window, cx).masked(true).placeholder("Password"));
                self._subscriptions.push(cx.subscribe_in(&password, window, submit));
                self.prompt = Some(ActivePrompt::SignIn { user, password, remember: false, error, reply: Some(reply) });
                self.focus_prompt(window, cx);
            }
            DesktopPrompt::Certificate { fingerprint, previous, reply } => {
                self.prompt = Some(ActivePrompt::Certificate { fingerprint, previous, reply: Some(reply) });
                window.focus(&self.focus, cx);
            }
        }
    }

    fn answer_sign_in(&mut self, submit: bool, window: &mut Window, cx: &mut Context<Self>) {
        let Some(ActivePrompt::SignIn { user, password, remember, reply, .. }) = self.prompt.as_mut() else { return };
        let answer = SignIn {
            user: user.as_ref().map(|u| u.read(cx).value().trim().to_string()),
            password: Zeroizing::new(password.read(cx).value().to_string()),
            remember: *remember,
        };
        if submit && user.is_some() && answer.user.as_deref().is_none_or(str::is_empty) {
            if let Some(user) = user {
                user.update(cx, |state, cx| state.focus(window, cx));
            }
            return;
        }
        if let Some(reply) = reply.take() {
            let _ = reply.send(submit.then_some(answer));
        }
        self.prompt = None;
        window.focus(&self.focus, cx);
        cx.notify();
    }

    fn answer_certificate(&mut self, trust: bool, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(ActivePrompt::Certificate { reply, .. }) = self.prompt.as_mut()
            && let Some(reply) = reply.take()
        {
            let _ = reply.send(trust);
        }
        self.prompt = None;
        window.focus(&self.focus, cx);
        cx.notify();
    }

    // Input

    fn key_down(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        if !self.focus.is_focused(window) || !self.is_connected() {
            return;
        }
        let ks = &event.keystroke;
        let m = &ks.modifiers;
        // Alt+F4 closes Komando and Alt+Space opens its window menu, as everywhere in Windows.
        if m.alt && !m.control && (ks.key == "f4" || ks.key == "space") {
            return;
        }
        let Some(key) = desktop::keys::from_keystroke(&ks.key, ks.key_char.as_deref(), m.control, m.alt) else { return };
        if let Key::Code(code) = key {
            self.held.insert(code.scancode);
        }
        self.send(DesktopInput::Key { key, down: true });
        cx.stop_propagation();
    }

    fn key_up(&mut self, event: &KeyUpEvent, _: &mut Window, _: &mut Context<Self>) {
        let ks = &event.keystroke;
        let m = &ks.modifiers;
        // Released as the key it went down as (a shortcut), whatever the modifiers are now.
        let as_code = desktop::keys::from_keystroke(&ks.key, None, true, m.alt);
        if let Some(Key::Code(code)) = as_code
            && self.held.remove(&code.scancode)
        {
            self.send(DesktopInput::Key { key: Key::Code(code), down: false });
        }
    }

    fn modifiers_changed(&mut self, event: &ModifiersChangedEvent, window: &mut Window, _: &mut Context<Self>) {
        if !self.focus.is_focused(window) || !self.is_connected() {
            return;
        }
        let (old, new) = (self.modifiers, event.modifiers);
        use desktop::keys::{ALT, CONTROL, SHIFT, WINDOWS};
        for (code, was, is) in [
            (CONTROL, old.control, new.control),
            (SHIFT, old.shift, new.shift),
            (ALT, old.alt, new.alt),
            (WINDOWS, old.platform, new.platform),
        ] {
            if was != is {
                self.send(DesktopInput::Key { key: Key::Code(code), down: is });
            }
        }
        self.modifiers = new;
    }

    fn send_tab(&mut self, backwards: bool, window: &mut Window, cx: &mut Context<Self>) {
        if !self.focus.is_focused(window) {
            cx.propagate();
            return;
        }
        let tab = desktop::keys::from_keystroke("tab", None, false, false).expect("tab");
        // Shift is already down on the remote side for Shift+Tab.
        let _ = backwards;
        self.send(DesktopInput::Key { key: tab, down: true });
        self.send(DesktopInput::Key { key: tab, down: false });
    }

    /// The point on the remote screen under `position`, if the picture is there.
    fn remote_point(&self, position: Point<Pixels>) -> Option<(u16, u16)> {
        let placement = self.placement.get()?;
        let local = position - placement.bounds.origin;
        let (x, y) = (f32::from(local.x) * placement.remote_per_px, f32::from(local.y) * placement.remote_per_px);
        let (w, h) = self.remote_size;
        let clamp = |v: f32, max: u32| v.clamp(0., max.saturating_sub(1) as f32) as u16;
        Some((clamp(x, w), clamp(y, h)))
    }

    fn mouse_down(&mut self, event: &MouseDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        if self.prompt.is_some() {
            return;
        }
        window.focus(&self.focus, cx);
        if let (Some((x, y)), Some(button)) = (self.remote_point(event.position), button_of(event.button)) {
            self.send(DesktopInput::Pointer { x, y });
            self.send(DesktopInput::Button { button, down: true });
        }
    }

    fn mouse_up(&mut self, event: &MouseUpEvent, _: &mut Window, _: &mut Context<Self>) {
        if let (Some((x, y)), Some(button)) = (self.remote_point(event.position), button_of(event.button)) {
            self.send(DesktopInput::Pointer { x, y });
            self.send(DesktopInput::Button { button, down: false });
        }
    }

    fn mouse_move(&mut self, event: &MouseMoveEvent, _: &mut Window, _: &mut Context<Self>) {
        if self.prompt.is_none()
            && let Some((x, y)) = self.remote_point(event.position)
        {
            self.send(DesktopInput::Pointer { x, y });
        }
    }

    fn scroll(&mut self, event: &ScrollEvent, _: &mut Window, cx: &mut Context<Self>) {
        // gpui gives lines (Windows' "lines per notch", 3 by default) or pixels (touchpads).
        let (dx, dy) = match event.delta {
            ScrollDelta::Lines(d) => (d.x * 40., d.y * 40.),
            ScrollDelta::Pixels(d) => (f32::from(d.x) * 3., f32::from(d.y) * 3.),
        };
        let (vertical, amount) = if dy != 0. { (true, dy) } else { (false, -dx) };
        let amount = amount.round().clamp(-32_000., 32_000.) as i16;
        if amount != 0 {
            self.send(DesktopInput::Wheel { vertical, amount });
            cx.stop_propagation();
        }
    }

    // Drawing

    /// Take a changed picture from the engine as an image gpui can draw.
    fn refresh_image(&mut self, window: &mut Window) {
        for old in self.stale.drain(..) {
            let _ = window.drop_image(old);
        }
        let Some(connection) = &self.connection else { return };
        // Before reading: a change made while reading tells the view again.
        connection.drawn();
        let picture = connection.picture.lock().expect("picture");
        if picture.version == self.drawn_version || picture.width == 0 || picture.height == 0 {
            return;
        }
        self.drawn_version = picture.version;
        self.remote_size = (picture.width, picture.height);
        let Some(buffer) = image::RgbaImage::from_raw(picture.width, picture.height, picture.bgra.clone()) else { return };
        drop(picture);
        let image = Arc::new(RenderImage::new(smallvec::smallvec![image::Frame::new(buffer)]));
        if let Some(old) = self.image.replace(image) {
            self.stale.push(old);
        }
    }

    /// After a new pane size has settled, ask an RDP server to follow it.
    fn follow_pane(&mut self, window: &Window, cx: &mut Context<Self>) {
        if !self.fit_desktop || self.protocol != Protocol::Rdp || !self.is_connected() || self.pane.get().is_none() {
            return;
        }
        let (width, height, scale) = self.desired_size(window);
        if self.requested == Some((width, height)) || self.resize.is_some() {
            return;
        }
        self.resize = Some(cx.spawn_in(window, async move |this, cx| {
            cx.background_executor().timer(RESIZE_SETTLE).await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.resize = None;
                let now = this.desired_size(window);
                if (now.0, now.1) == (width, height) {
                    this.requested = Some((width, height));
                    this.send(DesktopInput::Resize { width, height, scale_percent: scale });
                } else {
                    this.follow_pane(window, cx);
                }
            });
        }));
    }

    fn render_picture(&self) -> AnyElement {
        let image = self.image.clone();
        let (w, h) = self.remote_size;
        let pane = self.pane.clone();
        let placement = self.placement.clone();
        canvas(
            move |bounds, window, _| {
                pane.set(Some((bounds, window.scale_factor())));
            },
            move |bounds, _, window, _| {
                let Some(image) = image else { return };
                if w == 0 || h == 0 {
                    return;
                }
                // One remote pixel per device pixel, scaled down (never up) to fit.
                let scale = window.scale_factor();
                let natural = size(px(w as f32 / scale), px(h as f32 / scale));
                let fit = (f32::from(bounds.size.width) / f32::from(natural.width))
                    .min(f32::from(bounds.size.height) / f32::from(natural.height))
                    .min(1.);
                let shown = size(natural.width * fit, natural.height * fit);
                let origin = point(
                    bounds.origin.x + (bounds.size.width - shown.width) / 2.,
                    bounds.origin.y + (bounds.size.height - shown.height) / 2.,
                );
                let rect = Bounds { origin: point(origin.x.round(), origin.y.round()), size: shown };
                placement.set(Some(Placement { bounds: rect, remote_per_px: w as f32 / f32::from(shown.width) }));
                let _ = window.paint_image(rect, rect, Corners::default(), image, 0, false);
            },
        )
        .size_full()
        .into_any_element()
    }

    fn render_card(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let p = cx.global::<Appearance>().app;
        let card = |title: String, body: AnyElement, actions: Vec<AnyElement>| {
            div()
                .w(px(420.))
                .max_w_full()
                .p(px(20.))
                .rounded(p.corner(14.))
                .bg(p.surface)
                .border_1()
                .border_color(p.border)
                .shadow(elevation(p))
                .flex()
                .flex_col()
                .gap(px(14.))
                .font_family(p.font_family)
                .child(div().text_size(px(15.)).font_weight(FontWeight::SEMIBOLD).text_color(p.text).child(title))
                .child(body)
                .when(!actions.is_empty(), |el| el.child(div().flex().justify_end().gap(px(8.)).children(actions)))
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
        let protocol = self.protocol.label();
        let host = self.host.clone();
        if let Some(prompt) = &self.prompt {
            return Some(match prompt {
                ActivePrompt::SignIn { user, password, remember, error, .. } => {
                    let ticked = *remember;
                    card(
                        format!("Sign in to {host}"),
                        div()
                            .flex()
                            .flex_col()
                            .gap(px(8.))
                            .child(text(format!("Remote desktop ({protocol}) · {}", self.target)))
                            .when_some(user.clone(), |el, user| el.child(Input::new(&user)))
                            .child(password_input(password, p))
                            .when_some(error.clone(), |el, error| el.child(div().text_size(px(12.)).text_color(p.danger).child(error)))
                            .child({
                                let view = cx.entity().downgrade();
                                crate::ui::widgets::labelled_checkbox("rd-remember", "Remember in vault", ticked, p, move |clicked, window, cx| {
                                    let _ = view.update(cx, |this, cx| {
                                        if let Some(ActivePrompt::SignIn { remember, .. }) = this.prompt.as_mut() {
                                            *remember = !*remember;
                                        }
                                        // A click returns to the field; Space keeps the Tab position.
                                        if clicked {
                                            this.focus_prompt(window, cx);
                                        }
                                        cx.notify();
                                    });
                                })
                            })
                            .into_any_element(),
                        vec![
                            button("rd-cancel", "Cancel", ButtonKind::Ghost, p)
                                .on_click(cx.listener(|this, _, window, cx| this.answer_sign_in(false, window, cx)))
                                .into_any_element(),
                            button("rd-sign-in", "Sign in", ButtonKind::Primary, p)
                                .on_click(cx.listener(|this, _, window, cx| this.answer_sign_in(true, window, cx)))
                                .into_any_element(),
                        ],
                    )
                }
                ActivePrompt::Certificate { fingerprint, previous, .. } => {
                    let changed = previous.is_some();
                    let body = if changed {
                        "This remote desktop presented a different certificate than the one you trusted. Windows renews its own certificate now and then, but this can also mean someone is intercepting the connection. Check with the server's administrator before trusting the new one."
                    } else {
                        "First connection to this remote desktop. Check that this certificate fingerprint matches the server's before trusting it."
                    };
                    card(
                        if changed { format!("{host}'s certificate changed") } else { format!("Trust {host}'s certificate?") },
                        div()
                            .flex()
                            .flex_col()
                            .gap(px(10.))
                            .child(text(body.into()))
                            .when_some(previous.clone(), |el, previous| {
                                el.child(text("Trusted until now:".into())).child(mono(previous)).child(text("Presented now:".into()))
                            })
                            .child(mono(fingerprint.clone()))
                            .into_any_element(),
                        vec![
                            button("rd-distrust", "Don't connect", ButtonKind::Ghost, p)
                                .on_click(cx.listener(|this, _, window, cx| this.answer_certificate(false, window, cx)))
                                .into_any_element(),
                            button(
                                "rd-trust",
                                if changed { "Trust the new certificate" } else { "Trust and connect" },
                                if changed { ButtonKind::Danger } else { ButtonKind::Primary },
                                p,
                            )
                            .on_click(cx.listener(|this, _, window, cx| this.answer_certificate(true, window, cx)))
                            .into_any_element(),
                        ],
                    )
                }
            });
        }
        match &self.status {
            Status::Connected => None,
            Status::Connecting => Some(card(
                format!("Connecting to {host}…"),
                div()
                    .flex()
                    .items_center()
                    .gap(px(10.))
                    .child(spinner("rd-connecting", 16., p.brand))
                    .child(text(format!("Remote desktop ({protocol}) · {}", self.target)))
                    .into_any_element(),
                vec![],
            )),
            Status::Disconnected(reason) if self.waiting_for_ssh => Some(card(
                format!("{host}'s SSH session isn't connected"),
                text(reason.clone()),
                vec![
                    button("rd-edit", "Edit host", ButtonKind::Ghost, p)
                        .on_click(cx.listener(|_, _, _, cx| cx.emit(DesktopViewEvent::EditHost)))
                        .into_any_element(),
                    button("rd-open-ssh", "Connect SSH", ButtonKind::Primary, p)
                        .on_click(cx.listener(|_, _, _, cx| cx.emit(DesktopViewEvent::OpenSsh)))
                        .into_any_element(),
                ],
            )),
            Status::Disconnected(reason) => Some(card(
                "Disconnected".into(),
                text(reason.clone()),
                vec![
                    button("rd-edit", "Edit host", ButtonKind::Ghost, p)
                        .on_click(cx.listener(|_, _, _, cx| cx.emit(DesktopViewEvent::EditHost)))
                        .into_any_element(),
                    button("rd-reconnect", "Reconnect", ButtonKind::Primary, p)
                        .on_click(cx.listener(|_, _, _, cx| cx.emit(DesktopViewEvent::Reconnect)))
                        .into_any_element(),
                ],
            )),
        }
    }
}

fn button_of(button: MouseButton) -> Option<Button> {
    match button {
        MouseButton::Left => Some(Button::Left),
        MouseButton::Middle => Some(Button::Middle),
        MouseButton::Right => Some(Button::Right),
        _ => None,
    }
}

impl Render for DesktopView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.refresh_image(window);
        self.follow_pane(window, cx);
        let card = self.render_card(cx);
        let app = cx.global::<Appearance>().app;
        let connected = self.is_connected() && self.prompt.is_none();
        let dimmed = card.is_some() && self.image.is_some();
        div()
            .id("remote-desktop")
            .key_context(CONTEXT)
            .track_focus(&self.focus)
            .on_key_down(cx.listener(Self::key_down))
            .on_key_up(cx.listener(Self::key_up))
            .on_modifiers_changed(cx.listener(Self::modifiers_changed))
            .on_action(cx.listener(|this, _: &SendTab, window, cx| this.send_tab(false, window, cx)))
            .on_action(cx.listener(|this, _: &SendBackTab, window, cx| this.send_tab(true, window, cx)))
            .when(connected, |el| {
                el.on_mouse_down(MouseButton::Left, cx.listener(Self::mouse_down))
                    .on_mouse_down(MouseButton::Middle, cx.listener(Self::mouse_down))
                    .on_mouse_down(MouseButton::Right, cx.listener(Self::mouse_down))
                    .on_mouse_up(MouseButton::Left, cx.listener(Self::mouse_up))
                    .on_mouse_up(MouseButton::Middle, cx.listener(Self::mouse_up))
                    .on_mouse_up(MouseButton::Right, cx.listener(Self::mouse_up))
                    .on_mouse_up_out(MouseButton::Left, cx.listener(Self::mouse_up))
                    .on_mouse_up_out(MouseButton::Middle, cx.listener(Self::mouse_up))
                    .on_mouse_up_out(MouseButton::Right, cx.listener(Self::mouse_up))
                    .on_mouse_move(cx.listener(Self::mouse_move))
                    .on_scroll(cx.listener(Self::scroll))
            })
            .relative()
            .size_full()
            .rounded(app.corner(crate::ui::widgets::ISLAND_RADIUS - 1.))
            .overflow_hidden()
            .bg(app.background)
            .child(div().absolute().inset_0().when(dimmed, |el| el.opacity(0.35)).child(self.render_picture()))
            .when_some(card, |el, card| el.child(div().absolute().inset_0().p(px(16.)).flex().items_center().justify_center().child(card)))
    }
}
