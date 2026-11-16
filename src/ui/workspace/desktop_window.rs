//! A Remote Desktop Session in a window of its own: dragged out of Komando's window from its
//! sidebar row (or moved there from the title bar or the row's menu). The Session stays in the
//! sidebar; clicking it brings its window to the front. Closing the window, or "Back to
//! Komando", returns the Session to the main window, still connected.

use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyWindowHandle, App, Bounds, ClickEvent, Context, Entity, FontWeight, MouseButton, Pixels, Point, Render, SharedString,
    WeakEntity, Window, WindowBounds, WindowControlArea, WindowOptions, div, point, prelude::*, px, size,
};

use super::{SessionView, TITLE_BAR_HEIGHT, Workspace, strip_action, window_controls};
use crate::session::Status;
use crate::theme::Appearance;
use crate::ui::desktop_view::DesktopView;
use crate::ui::widgets::{ISLAND_GAP, island, status_dot};

/// How far the pointer has to leave the window, while a desktop's sidebar row is dragged,
/// before letting go there opens the desktop in a window of its own.
const TEAR_OFF_MARGIN: f32 = 8.;

/// A sidebar row of a Remote Desktop Session held outside the window: where letting go
/// would open its window, on screen.
#[derive(Clone, Copy, PartialEq, Debug)]
pub(super) struct TearOff {
    pub session: usize,
    pub screen: Point<Pixels>,
}

/// The root of a Remote Desktop Session's own window: a title bar with the Session's state
/// and actions, and the desktop under it.
pub(super) struct DesktopWindow {
    session: usize,
    view: Entity<DesktopView>,
    workspace: WeakEntity<Workspace>,
    main: AnyWindowHandle,
}

impl DesktopWindow {
    fn new(session: usize, view: Entity<DesktopView>, workspace: WeakEntity<Workspace>, main: AnyWindowHandle, cx: &mut Context<Self>) -> Self {
        // Its title bar follows the desktop's state.
        cx.observe(&view, |_, _, cx| cx.notify()).detach();
        // However the window goes (its ✕, Alt+F4, "Back to Komando"), the Session goes back
        // to the main window. Deferred: the window is still being taken down.
        cx.on_release(move |this: &mut Self, cx: &mut App| {
            let (id, workspace, main) = (this.session, this.workspace.clone(), this.main);
            cx.defer(move |cx| {
                let Some(workspace) = workspace.upgrade() else { return };
                let _ = main.update(cx, |_, window, cx| workspace.update(cx, |ws, cx| ws.reattach_desktop(id, window, cx)));
            });
        })
        .detach();
        Self { session, view, workspace, main }
    }

    /// Run `f` on the Workspace, in the main window.
    fn on_workspace(&self, cx: &mut App, f: impl FnOnce(&mut Workspace, &mut Window, &mut Context<Workspace>) + 'static) {
        let Some(workspace) = self.workspace.upgrade() else { return };
        let main = self.main;
        // After this window's update: the Workspace may close this window.
        cx.defer(move |cx| {
            let _ = main.update(cx, |_, window, cx| workspace.update(cx, |ws, cx| f(ws, window, cx)));
        });
    }
}

impl Render for DesktopWindow {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = cx.global::<Appearance>().app;
        let view = self.view.read(cx);
        let (status_text, status_color) = match &view.status {
            Status::Connecting => ("Connecting", p.warning),
            Status::Connected => ("Connected", p.success),
            Status::Disconnected(_) => ("Disconnected", p.text_faint),
        };
        let name: SharedString = match &view.session_name {
            Some(given) => format!("{} · {} · {given}", view.host, view.protocol.label()).into(),
            None => format!("{} · {}", view.host, view.protocol.label()).into(),
        };
        let target = view.target.clone();
        let live = !matches!(view.status, Status::Disconnected(_));
        let id = self.session;
        let title_bar = div()
            .id("desktop-title-bar")
            .h(px(TITLE_BAR_HEIGHT))
            .w_full()
            .flex_shrink_0()
            .flex()
            .items_center()
            .gap(px(7.))
            .pl(px(ISLAND_GAP + 10.))
            .bg(p.canvas)
            .window_control_area(WindowControlArea::Drag)
            .when(cfg!(not(windows)), |el| el.on_mouse_down(MouseButton::Left, |_, window, _| window.start_window_move()))
            .child(status_dot(status_color))
            .child(
                div()
                    .id("desktop-window-target")
                    .min_w_0()
                    .flex()
                    .items_center()
                    .gap(px(6.))
                    .text_size(px(12.))
                    .tooltip(move |window, cx| gpui_component::tooltip::Tooltip::new(status_text).build(window, cx))
                    .child(div().flex_shrink_0().max_w(px(240.)).truncate().font_weight(FontWeight::MEDIUM).text_color(p.text).child(name))
                    .child(div().min_w_0().truncate().text_color(p.text_muted).child(target)),
            )
            .child(div().flex_1())
            .when(!live, |el| {
                el.child(strip_action("desktop-reconnect", "refresh-cw", "Reconnect", false, p).on_click(cx.listener(
                    move |this, _: &ClickEvent, _, cx| this.on_workspace(cx, move |ws, window, cx| ws.connect_desktop(id, window, cx)),
                )))
            })
            .child(
                strip_action("desktop-back", "undo-2", "Back to Komando", false, p)
                    .on_click(|_: &ClickEvent, window, _| window.remove_window()),
            )
            .child(div().w(px(1.)).h(px(14.)).mx(px(4.)).bg(p.border))
            .child(
                strip_action("desktop-close", "unplug", if live { "Disconnect" } else { "Close session" }, true, p).on_click(cx.listener(
                    move |this, _: &ClickEvent, _, cx| this.on_workspace(cx, move |ws, window, cx| ws.close_session(id, window, cx)),
                )),
            )
            .child(div().w(px(12.)).h(px(16.)).mr(px(6.)).border_r_1().border_color(p.border))
            .child(window_controls(p, window));
        let fullscreen = window.is_fullscreen();
        div()
            .id("desktop-window")
            .size_full()
            .flex()
            .flex_col()
            .font_family(p.font_family)
            .text_color(p.text)
            .bg(p.canvas)
            // Full screen, from the desktop's toolbar: the desktop alone, edge to edge.
            .when(fullscreen, |el| el.child(self.view.clone()))
            .when(!fullscreen, |el| {
                el.child(title_bar).child(
                    div().flex_1().min_h_0().w_full().px(px(ISLAND_GAP)).pb(px(ISLAND_GAP)).child(
                        island(p).size_full().overflow_hidden().child(self.view.clone()),
                    ),
                )
            })
    }
}

impl Workspace {
    /// The pointer moved while a sidebar row is dragged: a Remote Desktop Session's row held
    /// outside the window is ready to open in a window of its own.
    pub(super) fn drag_outside(&mut self, session: Option<usize>, position: Point<Pixels>, window: &Window, cx: &mut Context<Self>) {
        let size = window.viewport_size();
        let margin = px(TEAR_OFF_MARGIN);
        let outside = position.x < -margin
            || position.y < -margin
            || position.x > size.width + margin
            || position.y > size.height + margin;
        let tear_off = session
            .filter(|id| outside && self.sessions.iter().any(|s| s.id == *id && s.view.desktop().is_some() && s.window.is_none()))
            .map(|session| TearOff { session, screen: window.bounds().origin + position });
        if self.tear_off != tear_off {
            if tear_off.is_some() {
                self.drop_target = None;
            }
            self.tear_off = tear_off;
            cx.notify();
        }
    }

    /// The mouse went up: a desktop's row let go outside the window opens there.
    pub(super) fn finish_tear_off(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.native_drag = None;
        if let Some(tear_off) = self.tear_off.take() {
            // Its title bar under the pointer, as if the window had been carried out.
            let origin = tear_off.screen - point(px(140.), px(TITLE_BAR_HEIGHT / 2.));
            self.detach_desktop(tear_off.session, Some(origin), window, cx);
        }
    }

    /// Move a Remote Desktop Session into a window of its own, with its top left corner at
    /// `origin` on screen (beside the main window's when `None`). One already in its own
    /// window comes to the front.
    pub(super) fn detach_desktop(&mut self, id: usize, origin: Option<Point<Pixels>>, window: &mut Window, cx: &mut Context<Self>) {
        let Some(index) = self.sessions.iter().position(|s| s.id == id) else { return };
        if let Some(handle) = self.sessions[index].window {
            let _ = handle.update(cx, |_, window, _| window.activate_window());
            return;
        }
        let SessionView::Desktop(view) = self.sessions[index].view.clone() else { return };
        let item = self.sessions[index].item.clone();
        let title: SharedString = format!("{} — Komando", self.sessions[index].label).into();
        let pane = view.read(cx).pane_size().unwrap_or(size(px(1024.), px(640.)));
        let window_size = size(
            pane.width.max(px(560.)) + px(ISLAND_GAP * 2.),
            pane.height.max(px(360.)) + px(TITLE_BAR_HEIGHT + ISLAND_GAP),
        );
        let origin = origin.unwrap_or_else(|| window.bounds().origin + point(px(48.), px(48.)));
        // Its picture leaves this window's GPU memory; the new window draws it again.
        view.update(cx, |v, _| v.leave(window));
        let workspace = cx.entity().downgrade();
        let main = window.window_handle();
        let options = WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(Bounds { origin, size: window_size })),
            window_min_size: Some(size(px(420.), px(280.))),
            titlebar: Some(gpui::TitlebarOptions { title: Some(title), appears_transparent: true, ..Default::default() }),
            app_id: Some("komando".into()),
            ..Default::default()
        };
        let dark = self.app_dark(window);
        let opened = cx.open_window(options, |new_window, cx| {
            view.update(cx, |v, cx| {
                v.detached = true;
                v.bind(new_window, cx);
                v.focus(new_window, cx);
            });
            let root = cx.new(|cx| DesktopWindow::new(id, view.clone(), workspace.clone(), main, cx));
            cx.new(|cx| gpui_component::Root::new(root, new_window, cx))
        });
        let handle: AnyWindowHandle = match opened {
            Ok(handle) => handle.into(),
            Err(err) => {
                log::error!("open a desktop window: {err:#}");
                view.update(cx, |v, cx| v.bind(window, cx));
                return self.show_toast(format!("Couldn't open a new window: {err:#}"), cx);
            }
        };
        let _ = handle.update(cx, |_, new_window, _| self.style_window(new_window, dark));
        self.sessions[index].window = Some(handle);
        // The main window shows something else: the other half of a split, or the Host's
        // terminal.
        let partner = self.leave_split(id);
        if self.last_active.get(&item) == Some(&id) {
            self.last_active.remove(&item);
        }
        if self.active == Some(id) {
            self.active = None;
            let next = partner.or_else(|| self.sessions_of(&item).find(|s| s.window.is_none()).map(|s| s.id));
            if let Some(next) = next {
                self.activate(next, window, cx);
            }
        }
        cx.notify();
    }

    /// A desktop's own window closed: show the Session in the main window again.
    pub(super) fn reattach_desktop(&mut self, id: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(entry) = self.sessions.iter_mut().find(|s| s.id == id) else { return };
        if entry.window.take().is_none() {
            return;
        }
        let Some(view) = entry.view.desktop().cloned() else { return };
        view.update(cx, |v, cx| {
            v.detached = false;
            v.bind(window, cx);
            cx.notify();
        });
        self.activate(id, window, cx);
        window.activate_window();
    }

    /// Close a Session's own window, if it has one (the Session is being discarded).
    pub(super) fn close_own_window(&self, id: usize, cx: &mut App) {
        if let Some(handle) = self.sessions.iter().find(|s| s.id == id).and_then(|s| s.window) {
            let _ = handle.update(cx, |_, window, _| window.remove_window());
        }
    }

    /// Close every desktop's own window: the main window is closing.
    pub(super) fn close_own_windows(&mut self, cx: &mut App) {
        for handle in self.sessions.iter_mut().filter_map(|s| s.window.take()) {
            let _ = handle.update(cx, |_, window, _| window.remove_window());
        }
    }

    /// The desktops' own windows, to restyle them with the main one.
    pub(super) fn own_windows(&self) -> Vec<AnyWindowHandle> {
        self.sessions.iter().filter_map(|s| s.window).collect()
    }
}
