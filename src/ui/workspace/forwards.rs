//! The session strip's Port Forwards button and its menu: what each forward carries, whether
//! it is listening, and why one failed. Clicking a forward that listens here copies its
//! address, for pasting into the program that uses it.

use gpui::prelude::FluentBuilder as _;
use gpui::{AnyElement, ClickEvent, Context, FontWeight, Hsla, SharedString, Window, div, prelude::*, px};
use gpui_component::menu::PopupMenuItem;
use gpui_component::tooltip::Tooltip;

use super::{SessionEntry, SessionKind, Workspace};
use crate::model::{ForwardKind, PortForward};
use crate::session::forward::{ForwardState, ForwardStatus};
use crate::theme::AppPalette;
use crate::ui::popup::Popup;
use crate::ui::widgets::{icon_el, plural};

/// How the Session's forwards are doing, in a few words: for the button's tooltip and name.
fn summary(forwards: &[ForwardStatus]) -> String {
    let failed = forwards.iter().filter(|f| matches!(f.state, ForwardState::Failed(_))).count();
    let starting = forwards.iter().filter(|f| f.state == ForwardState::Starting).count();
    let troubled = forwards.iter().filter(|f| matches!(f.state, ForwardState::Open { problem: Some(_), .. })).count();
    let total = forwards.len();
    match (failed, starting, troubled) {
        (0, 0, 0) if total == 1 => "Port forward: listening".into(),
        (0, 0, 0) => format!("Port forwards: all {total} listening"),
        (0, 0, _) => "Port forwards: a connection failed".into(),
        (0, n, _) if total == 1 && n == 1 => "Port forward: starting".into(),
        (0, n, _) => format!("Port forwards: {n} of {total} starting"),
        (n, _, _) if total == 1 && n == 1 => "Port forward: didn't start".into(),
        (n, _, _) => format!("Port forwards: {n} of {total} didn't start"),
    }
}

/// The address to copy for a forward that listens here, as programs are told to use it.
fn copy_address(forward: &PortForward) -> Option<String> {
    forward.kind.listens_here().then(|| format!("localhost:{}", forward.port))
}

/// A forward's first line in the menu: where it listens, and where that goes.
fn route_line(forward: &PortForward, host: &str) -> String {
    match forward.kind {
        ForwardKind::Local => format!("localhost:{} → {}", forward.port, forward.target()),
        ForwardKind::Remote => format!("{host}:{} → {}", forward.port, forward.target()),
        ForwardKind::Socks => format!("SOCKS proxy on localhost:{}", forward.port),
    }
}

/// A forward's second line: its state, and what clicking it does.
fn state_line(status: &ForwardStatus) -> (String, bool) {
    let here = status.forward.kind.listens_here();
    let network = if status.forward.open_to_network { ", open to the network" } else { "" };
    match &status.state {
        ForwardState::Starting => ("Starting…".into(), false),
        ForwardState::Failed(why) => (why.clone(), true),
        ForwardState::Open { problem: Some(why), .. } => (why.clone(), true),
        ForwardState::Open { connections, problem: None } => {
            let listening = if here { format!("Listening{network}") } else { format!("The server is listening{network}") };
            let text = match connections {
                0 if here => format!("{listening} · Click to copy the address"),
                0 => listening,
                n => format!("{listening} · {}", plural(*n, "connection")),
            };
            (text, false)
        }
    }
}

impl Workspace {
    /// The strip's Port Forwards button, for a connected SSH Session that has any: their
    /// count, amber once one failed. It opens the forwards menu.
    pub(super) fn forwards_strip_button(&self, entry: &SessionEntry, p: AppPalette, cx: &mut Context<Self>) -> Option<AnyElement> {
        let view = entry.ui.view.terminal()?;
        let forwards = &view.read(cx).forwards;
        if forwards.is_empty() {
            return None;
        }
        let id = entry.id();
        let open = self.forwards_menu.is_open();
        let trouble = forwards
            .iter()
            .any(|f| matches!(f.state, ForwardState::Failed(_) | ForwardState::Open { problem: Some(_), .. }));
        let color = match (trouble, open) {
            (true, _) => p.warning,
            (false, true) => p.active_text,
            (false, false) => p.text_muted,
        };
        let tip: SharedString = summary(forwards).into();
        let label = tip.clone();
        Some(
            div()
                .id("forwards-toggle")
                .role(gpui::Role::Button)
                .aria_label(label)
                .aria_expanded(open)
                .tab_index(0)
                .focus_visible(move |s| crate::ui::widgets::focus_outline(s, p.focus))
                .h(px(24.))
                .px(px(8.))
                .mr(px(6.))
                .flex()
                .items_center()
                .gap(px(5.))
                .rounded(p.corner(7.))
                .border_1()
                .border_color(if trouble { p.warning.opacity(0.6) } else if open { p.brand.opacity(0.7) } else { p.border })
                .when(open, |el| el.bg(p.brand_soft))
                .text_size(px(12.))
                .font_weight(FontWeight::MEDIUM)
                .text_color(color)
                .cursor_pointer()
                .hover(|s| s.bg(p.hover))
                // Its hitbox hides the title bar's drag area: see `strip_action`.
                .occlude()
                .tooltip(move |window, cx| Tooltip::new(tip.clone()).build(window, cx))
                .on_click(cx.listener(move |this, event: &ClickEvent, window, cx| this.toggle_forwards_menu(id, event, window, cx)))
                .child(icon_el(if trouble { "triangle-alert" } else { "arrow-right-left" }, 13., color))
                .child(forwards.len().to_string())
                .into_any_element(),
        )
    }

    /// Open the forwards menu of Session `id` below the button, or close it.
    fn toggle_forwards_menu(&mut self, id: usize, event: &ClickEvent, window: &mut Window, cx: &mut Context<Self>) {
        if self.forwards_menu.is_open() {
            self.forwards_menu = Popup::default();
            cx.notify();
            return;
        }
        let Some(entry) = self.sessions.get(id) else { return };
        let Some(view) = entry.ui.view.terminal() else { return };
        let forwards = view.read(cx).forwards.clone();
        let host = match (entry.project_id(), entry.kind()) {
            (Some(project), SessionKind::Ssh { host_id }) => Some((project.to_string(), host_id.clone())),
            _ => None,
        };
        let host_name = host.as_ref().and_then(|(_, h)| self.library.host(h)).map(|h| h.name.clone()).unwrap_or_else(|| "server".into());
        let p = cx.global::<crate::theme::Appearance>().app;
        let ws = cx.entity().downgrade();
        // Read now: the menu's builder must not read the Workspace (see `nav::open_row_menu`).
        // Below the button: from the keyboard, its bottom-left corner; from the mouse, just
        // under the pointer, which is on the button.
        let at = event.position();
        let position = match event {
            ClickEvent::Keyboard(_) => gpui::point(at.x, at.y + px(4.)),
            _ => gpui::point(at.x - px(12.), at.y + px(14.)),
        };
        self.forwards_menu = Popup::open(position, |this: &mut Workspace| &mut this.forwards_menu, window, cx, move |mut menu, _, _| {
            menu = menu.min_w(px(300.)).max_w(px(400.)).label(if forwards.len() == 1 { "Port forward" } else { "Port forwards" });
            for status in &forwards {
                let route = route_line(&status.forward, &host_name);
                let (state, bad) = state_line(status);
                let icon = match &status.state {
                    ForwardState::Starting => "loader-circle",
                    ForwardState::Failed(_) => "circle-alert",
                    ForwardState::Open { problem: Some(_), .. } => "triangle-alert",
                    ForwardState::Open { .. } => "arrow-right-left",
                };
                let icon_color: Hsla = match &status.state {
                    ForwardState::Failed(_) => p.danger,
                    ForwardState::Open { problem: Some(_), .. } => p.warning,
                    ForwardState::Open { .. } => p.success,
                    ForwardState::Starting => p.text_faint,
                };
                let state_color = if bad { icon_color } else { p.text_muted };
                let item = PopupMenuItem::element(move |_, _| {
                    div()
                        .py(px(3.))
                        .flex()
                        .items_start()
                        .gap(px(9.))
                        .child(div().pt(px(1.)).child(icon_el(icon, 14., icon_color)))
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .flex()
                                .flex_col()
                                .gap(px(2.))
                                .child(div().text_size(px(13.)).text_color(p.text).child(route.clone()))
                                .child(div().text_size(px(12.)).line_height(px(16.)).text_color(state_color).child(state.clone())),
                        )
                });
                let copy = copy_address(&status.forward).filter(|_| matches!(status.state, ForwardState::Open { .. }));
                menu = menu.item(match copy {
                    Some(address) => {
                        let ws = ws.clone();
                        item.on_click(move |_, _, cx| {
                            cx.write_to_clipboard(gpui::ClipboardItem::new_string(address.clone()));
                            let _ = ws.update(cx, |this, cx| this.show_toast(format!("Copied {address}"), cx));
                        })
                    }
                    None => item,
                });
            }
            if let Some((project_id, host_id)) = host.clone() {
                let ws = ws.clone();
                menu = menu.separator().item(super::menu_item("Edit port forwards…", "pencil", &ws, move |this, window, cx| {
                    this.open_host_forwards(project_id.clone(), host_id.clone(), window, cx)
                }));
            }
            menu
        });
        cx.notify();
    }

    /// A Port Forward of Session `id` didn't start: say so where the user is looking.
    pub(super) fn forward_failed(&mut self, id: usize, forward: &PortForward, why: &str, cx: &mut Context<Self>) {
        let host = self.sessions.get(id).map(|entry| self.sessions.display_name(entry)).unwrap_or_default();
        self.show_toast_for(format!("{host}: port forward {} didn't start. {why}", forward.label()), std::time::Duration::from_secs(8), cx);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn status(forward: PortForward, state: ForwardState) -> ForwardStatus {
        ForwardStatus { index: 0, forward, state }
    }

    #[test]
    fn the_summary_says_what_needs_attention_first() {
        let open = ForwardState::Open { connections: 0, problem: None };
        let one = [status(PortForward::local(5432, "db", 5432), open.clone())];
        assert_eq!(summary(&one), "Port forward: listening");
        let two = [one[0].clone(), status(PortForward::socks(1080), open.clone())];
        assert_eq!(summary(&two), "Port forwards: all 2 listening");
        let failed = [one[0].clone(), status(PortForward::socks(1080), ForwardState::Failed("taken".into()))];
        assert_eq!(summary(&failed), "Port forwards: 1 of 2 didn't start");
        let starting = [one[0].clone(), status(PortForward::socks(1080), ForwardState::Starting)];
        assert_eq!(summary(&starting), "Port forwards: 1 of 2 starting");
    }

    #[test]
    fn each_forward_says_where_it_listens_and_how_it_is_doing() {
        let local = PortForward::local(5432, "db.internal", 5432);
        assert_eq!(route_line(&local, "web01"), "localhost:5432 → db.internal:5432");
        assert_eq!(route_line(&PortForward::remote(8080, "localhost", 3000), "web01"), "web01:8080 → localhost:3000");
        assert_eq!(route_line(&PortForward::socks(1080), "web01"), "SOCKS proxy on localhost:1080");
        let idle = status(local.clone(), ForwardState::Open { connections: 0, problem: None });
        assert_eq!(state_line(&idle), ("Listening · Click to copy the address".into(), false));
        let busy = status(local.clone(), ForwardState::Open { connections: 2, problem: None });
        assert_eq!(state_line(&busy), ("Listening · 2 connections".into(), false));
        let remote = status(PortForward { open_to_network: true, ..PortForward::remote(8080, "localhost", 3000) }, ForwardState::Open { connections: 0, problem: None });
        assert_eq!(state_line(&remote), ("The server is listening, open to the network".into(), false));
        assert_eq!(copy_address(&local).as_deref(), Some("localhost:5432"));
        assert_eq!(copy_address(&PortForward::remote(8080, "localhost", 3000)), None);
    }
}
