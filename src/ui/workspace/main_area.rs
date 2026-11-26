//! The window's main area: the session strip, title bar, panes, and the empty and
//! welcome screens.

use super::*;

impl Workspace {
    /// The active Session's state, name and address with its actions. Lives in the title
    /// bar, above the terminal.
    fn render_session_strip(&self, p: AppPalette, cx: &mut Context<Self>) -> Option<AnyElement> {
        let entry = self.active_entry()?;
        // Where you are typing: state, the session's name, then user@address and port. The
        // name is repeated from the sidebar, which may be scrolled away from the active host.
        let name = self.session_title(entry);
        let target = match entry.kind() {
            SessionKind::Ssh { host_id } => entry
                .project_id()
                .and_then(|id| self.library.resolve(id, host_id))
                .map(|r| {
                    let user = r.user.clone().map(|u| format!("{u}@")).unwrap_or_default();
                    format!("{user}{}:{}", r.address, r.port)
                })
                .unwrap_or_default(),
            SessionKind::Local { shell } => format!("Local · {}", shell.program),
            SessionKind::Desktop { host_id, protocol } => {
                // The label already names the protocol ("win-build · RDP").
                entry.project_id().map(|p| self.desktop_target(p, host_id, *protocol)).unwrap_or_default()
            }
        };
        let desktop_buttons = self.desktop_strip_buttons(entry, p, cx);
        let forwards_button = self.forwards_strip_button(entry, p, cx);
        let (status_text, status_color) = match entry.status() {
            Status::Connecting => ("Connecting", p.warning),
            Status::Connected => ("Connected", p.success),
            Status::Disconnected(_) => ("Disconnected", p.text_faint),
        };
        let id = entry.id();
        let live = entry.is_live();
        let is_ssh = matches!(entry.kind(), SessionKind::Ssh { .. });
        let indicator = if is_ssh {
            let hops = entry.ui.view.terminal().map(|view| view.read(cx).latency.as_slice()).unwrap_or_default();
            connection_indicator(entry.status(), hops, cx.reduce_motion(), p).into_any_element()
        } else {
            status_dot(status_color).into_any_element()
        };
        let sftp_open = entry.ui.sftp_open;
        let sftp_phase = if sftp_open { entry.ui.sftp_busy.phase() } else { BusyPhase::Idle };
        // SFTP runs over the session's connection: until it is connected the button only looks
        // like one. An open browser can still be closed.
        let sftp_ready = matches!(entry.status(), Status::Connected);
        let sftp_enabled = sftp_ready || sftp_open;
        let sftp_color = if sftp_open {
            p.active_text
        } else if sftp_ready {
            p.text_muted
        } else {
            p.text_faint.opacity(0.55)
        };
        let sftp_done = p.success;
        let sftp_tip = match (entry.status(), sftp_open) {
            (_, true) => "Close file browser (Ctrl+Shift+E)",
            (Status::Connected, false) => "Browse files over SFTP (Ctrl+Shift+E)",
            (Status::Connecting, false) => "File browser: available once the session is connected",
            (Status::Disconnected(_), false) => "File browser: reconnect the session to browse its files",
        };
        Some(
            div()
                .id("session-strip")
                .min_w_0()
                .flex_1()
                .h_full()
                .pl(px(6.))
                .flex()
                .items_center()
                .gap(px(7.))
                .child(indicator)
                .child(
                    div()
                        .id("session-target")
                        .min_w_0()
                        .flex()
                        .items_center()
                        .gap(px(6.))
                        .text_size(px(12.))
                        .tooltip(move |window, cx| Tooltip::new(status_text).build(window, cx))
                        .child(
                            div()
                                .flex_shrink_0()
                                .max_w(px(240.))
                                .truncate()
                                .font_weight(FontWeight::MEDIUM)
                                .text_color(p.text)
                                .child(name),
                        )
                        .child(div().min_w_0().text_color(p.text_muted).truncate().child(target)),
                )
                // The empty middle stays draggable: only the controls above catch the mouse.
                .child(div().flex_1())
                .children(forwards_button)
                .when(is_ssh, |el| {
                    el.child(
                        div()
                            .id("sftp-toggle")
                            .h(px(24.))
                            .px(px(9.))
                            .mr(px(6.))
                            .flex()
                            .items_center()
                            .gap(px(6.))
                            .rounded(p.corner(7.))
                            .border_1()
                            .border_color(if sftp_open {
                                p.brand.opacity(0.7)
                            } else if sftp_enabled {
                                p.border
                            } else {
                                p.border.opacity(0.5)
                            })
                            .when(sftp_open, |el| el.bg(p.brand_soft))
                            .text_size(px(12.))
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(sftp_color)
                            .tooltip(move |window, cx| Tooltip::new(sftp_tip).build(window, cx))
                            .when(sftp_enabled, |el| {
                                el.cursor_pointer()
                                    .hover(|s| s.bg(p.hover))
                                    .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| this.toggle_sftp(id, window, cx)))
                            })
                            .when(!sftp_enabled, |el| el.cursor_default())
                            // Its hitbox hides the title bar's drag area: see `strip_action`.
                            .occlude()
                            .child(match sftp_phase {
                                BusyPhase::Idle => icon_el("folder-open", 13., sftp_color),
                                BusyPhase::Working => spinner("sftp-toggle-spinner", 13., sftp_color),
                                // The check fades out as the folder fades back in.
                                BusyPhase::Done => busy_done("sftp-toggle-done", move |opacity| {
                                    let layer = |o: f32| div().absolute().top_0().left_0().size(px(13.)).opacity(o);
                                    div()
                                        .relative()
                                        .size(px(13.))
                                        .child(layer(1. - opacity).child(icon_el("folder-open", 13., sftp_color)))
                                        .child(layer(opacity).child(icon_el("check", 13., sftp_done)))
                                        .into_any_element()
                                }),
                            })
                            .child("SFTP"),
                    )
                    // End the connection, keeping the terminal and its output to reconnect later.
                    .when(live, |el| {
                        el.child(
                            strip_action("session-disconnect", "unplug", "Disconnect", false, p)
                                .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| this.end_session(id, EndRequest::Disconnect, window, cx))),
                        )
                    })
                    // A new connection in the same terminal, the earlier output kept above: for a
                    // session that dropped or hangs. A live one is closed first.
                    .child(
                        strip_action("session-reconnect", "refresh-cw", "Reconnect", false, p)
                            .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| this.reconnect(id, window, cx))),
                    )
                })
                .when(self.sessions.shown_split().is_some(), |el| {
                    el.child(
                        strip_action("close-split", "columns-2", "Close split view", false, p)
                            .on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.close_split(window, cx))),
                    )
                })
                .children(desktop_buttons)
                // Room between the session's actions and the window buttons.
                .child(div().w(px(12.)).h(px(16.)).mr(px(6.)).border_r_1().border_color(p.border))
                .into_any_element(),
        )
    }

    /// The window's title bar, drawn by Komando so it matches the app: the sidebar toggle,
    /// sidebar, the active Session's strip over the terminal, and the window buttons. Its
    /// background is the canvas, so on Windows Mica shows through it like everywhere else.
    /// `minimal` (the unlock screen) shows only the window buttons.
    pub(super) fn render_title_bar(&self, minimal: bool, p: AppPalette, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let strip = match (&self.page, minimal) {
            (Page::Sessions, false) => self.render_session_strip(p, cx),
            _ => None,
        };
        div()
            .id("title-bar")
            .h(px(TITLE_BAR_HEIGHT))
            .w_full()
            .flex_shrink_0()
            .flex()
            .items_center()
            // No cover for DWM's own caption buttons is needed: with Mica on, the window has
            // no native caption for DWM to draw them in (see `platform::set_backdrop`).
            .bg(p.canvas)
            // Windows: the bar is the window's caption, so dragging, double-click to maximise
            // and snapping work as with any window. Buttons inside take their clicks.
            .window_control_area(WindowControlArea::Drag)
            .when(cfg!(not(windows)), |el| {
                el.on_mouse_down(MouseButton::Left, |event, window, _| {
                    // Double-click zooms (or does what the user set in macOS' settings).
                    if event.click_count == 2 { window.titlebar_double_click() } else { window.start_window_move() }
                })
            })
            .child(
                div()
                    .when(self.sidebar_visible || minimal, |el| el.w(px(self.sidebar_width(window) + ISLAND_GAP * 2.)))
                    .flex_shrink_0()
                    .h_full()
                    .pl(px(ISLAND_GAP + 8. + TRAFFIC_LIGHTS_WIDTH))
                    .pr(px(ISLAND_GAP))
                    .flex()
                    .items_center()
                    .gap(px(6.))
                    .when(!minimal, |el| el.children(self.render_main_menu_buttons(p, cx)))
                    .when(crate::elevate::is_elevated(), |el| el.child(admin_badge(p))),
            )
            .child(div().flex_1().min_w_0().h_full().flex().items_center().children(strip))
            // macOS has its traffic lights instead.
            .when(!cfg!(target_os = "macos"), |el| el.child(window_controls(p, window)))
            .into_any_element()
    }

    /// The terminal area: the active Session, or the two of its split side by side. It is
    /// also where sidebar items are dropped to open them beside the Session shown.
    pub(super) fn render_main(&self, p: AppPalette, window: &Window, cx: &mut Context<Self>) -> AnyElement {
        let content = match (self.active_entry(), self.sessions.shown_split()) {
            (None, _) => self.render_empty(p, cx),
            (Some(_), Some(split)) => {
                let ratio = self.sessions.split_ratio();
                let mut panes: Vec<AnyElement> = [(split.left, ratio), (split.right, 1. - ratio)]
                    .into_iter()
                    .filter_map(|(id, share)| self.render_pane(id, Some(share), p, window, cx))
                    .collect();
                if panes.len() == 2 {
                    panes.insert(1, self.render_split_divider(p, cx));
                }
                div().flex_1().min_w_0().h_full().flex().children(panes).into_any_element()
            }
            (Some(entry), None) => self.render_pane(entry.id(), None, p, window, cx).unwrap_or_else(|| div().into_any_element()),
        };
        div()
            .id("main-area")
            .relative()
            .flex_1()
            .min_w_0()
            .h_full()
            .flex()
            .child(content)
            .children(self.render_drop_overlay(cx))
            .on_drag_move(cx.listener(|this, event: &gpui::DragMoveEvent<DraggedItem>, _, cx| this.drag_over_main(event, cx)))
            .on_drag_move(cx.listener(|this, event: &gpui::DragMoveEvent<split::SplitDivider>, _, cx| this.drag_split_divider(event, cx)))
            .on_drop(cx.listener(|this, dragged: &DraggedItem, window, cx| this.drop_on_main(dragged, window, cx)))
            .into_any_element()
    }

    /// One Session's terminal, with its file browser when open. In a split, `share` is its
    /// part of the width, and a press anywhere in the pane activates it.
    pub(super) fn render_pane(&self, id: usize, share: Option<f32>, p: AppPalette, window: &Window, cx: &mut Context<Self>) -> Option<AnyElement> {
        let entry = self.sessions.get(id)?;
        let in_split = share.is_some();
        // Both panes grow from nothing, each by its share: the divider sets how the width splits.
        let grow = move |mut el: gpui::Stateful<gpui::Div>| {
            if let Some(share) = share {
                el.style().flex_grow = Some(share);
            }
            el
        };
        if entry.own_window() {
            let placeholder = self.render_in_own_window(id, p, cx);
            return Some(grow(div().id(("pane", id)).flex_1().min_w_0().h_full().flex().child(placeholder)).into_any_element());
        }
        let focused = self.sessions.active() == Some(id);
        let term_bg = cx.global::<Appearance>().term.background_hsla();
        let browser = entry.ui.browser.clone().filter(|_| entry.ui.sftp_open);
        // The browser keeps at least 300px for the terminal when the pane is narrow.
        let window_width = f32::from(window.bounds().size.width);
        let area = window_width - ISLAND_GAP * 2.
            - if self.sidebar_visible { self.sidebar_width(window) + ISLAND_GAP } else { 0. };
        let pane_width = match share {
            Some(share) => (area - ISLAND_GAP) * share,
            None => area,
        };
        let max = (pane_width - 300.).max(280.);
        let width = self.settings.sftp_width.min(max).max(280.);
        if let Some(browser) = &browser {
            browser.update(cx, |b, _| b.width = width);
        }
        let session = island(p)
            .when(matches!(&entry.ui.view, SessionView::Terminal(_)), |el| el.border_color(p.border.blend(p.text.opacity(0.18))))
            .flex_1()
            .min_w_0()
            .h_full()
            .bg(term_bg)
            .overflow_hidden()
            .flex()
            .child(div().flex_1().min_w_0().h_full().map(|el| match &entry.ui.view {
                SessionView::Terminal(view) => el.child(view.clone()),
                SessionView::Desktop(view) => el.child(view.clone()),
            }));
        Some(
            div()
                .id(("pane", id))
                .relative()
                .flex_1()
                .map(grow)
                .min_w_0()
                .h_full()
                .flex()
                .when(in_split && !focused, |el| {
                    el.capture_any_mouse_down(cx.listener(move |this, _: &gpui::MouseDownEvent, window, cx| {
                        if this.sessions.active() != Some(id) && !this.modal_open(cx) {
                            this.activate(id, window, cx);
                        }
                    }))
                })
                .on_drag_move(cx.listener(move |this, event: &gpui::DragMoveEvent<SftpDivider>, _, cx| {
                    if event.drag(cx).0 == id {
                        this.sftp_resizing = true;
                        this.drop_target = None;
                        this.resize_sftp(f32::from(event.event.position.x), event.bounds, cx);
                    }
                }))
                .child(session)
                // The browser is a card of its own beside the session; the gap between them
                // is the handle that resizes it.
                .when_some(browser, |el, browser| {
                    el.child(
                        div()
                            .id("sftp-divider")
                            .group("sftp-divider")
                            .w(px(ISLAND_GAP))
                            .h_full()
                            .flex_shrink_0()
                            .flex()
                            .justify_center()
                            .py(px(14.))
                            .cursor_col_resize()
                            .on_drag(SftpDivider(id), |d, _, _, cx| cx.new(|_| SftpDivider(d.0)))
                            .child(
                                div()
                                    .w(px(2.))
                                    .h_full()
                                    .rounded_full()
                                    .when(self.sftp_resizing, |el| el.bg(p.brand))
                                    .group_hover("sftp-divider", |s| s.bg(p.brand.opacity(0.6))),
                            ),
                    )
                    .child(island(p).w(px(width)).flex_shrink_0().h_full().bg(p.sidebar).overflow_hidden().child(browser))
                })
                .into_any_element(),
        )
    }

    /// In place of a desktop that is in a window of its own.
    fn render_in_own_window(&self, id: usize, p: AppPalette, cx: &mut Context<Self>) -> AnyElement {
        island(p)
            .flex_1()
            .min_w_0()
            .h_full()
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap(px(12.))
            .child(icon_el("app-window", 28., p.text_faint))
            .child(div().text_size(px(13.)).text_color(p.text_muted).child("This remote desktop is in its own window."))
            .child(
                div()
                    .flex()
                    .gap(px(8.))
                    .child(button("own-window-show", "Show its window", ButtonKind::Ghost, p).on_click(cx.listener(
                        move |this, _: &ClickEvent, window, cx| this.activate(id, window, cx),
                    )))
                    .child(button("own-window-back", "Bring it back here", ButtonKind::Primary, p).on_click(cx.listener(
                        move |this, _: &ClickEvent, _, cx| this.close_own_window(id, cx),
                    ))),
            )
            .into_any_element()
    }

    fn render_empty(&self, p: AppPalette, cx: &mut Context<Self>) -> AnyElement {
        if self.library.projects.is_empty() && self.library.hosts.is_empty() {
            return self.render_welcome(p, cx);
        }
        island(p)
            .flex_1()
            .h_full()
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap(px(18.))
            .bg(p.background)
            .child(wordmark(34., p.text))
            .child(div().text_size(px(13.)).text_color(p.text_muted).child("No open sessions."))
            .child(
                div()
                    .flex()
                    .gap(px(8.))
                    .child(
                        button("empty-connect", "Quick connect", ButtonKind::Primary, p)
                            .on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.open_picker(window, cx))),
                    )
                    .child(
                        button("empty-project", "New project", ButtonKind::Secondary, p)
                            .on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.open_new_project(window, cx))),
                    ),
            )
            .into_any_element()
    }

    /// The start screen, shown until the first project or connection exists: what Komando is,
    /// and one click to add a server or open a local shell.
    fn render_welcome(&self, p: AppPalette, cx: &mut Context<Self>) -> AnyElement {
        let shells = welcome_shells(self.library.standalone_shells().into_iter().filter(|s| !s.hidden).collect());
        let hero = |id: SharedString, icon: &str, label: String, kind: ButtonKind| {
            icon_text_button(id, icon, label, kind, p).h(px(44.)).px(px(20.)).gap(px(10.)).rounded(p.corner(10.)).text_size(px(14.))
        };
        let mut actions = vec![
            hero("welcome-add".into(), "plus", "Add connection".into(), ButtonKind::Primary)
                .on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.open_new_host(String::new(), window, cx)))
                .into_any_element(),
            hero("welcome-project".into(), "folder-plus", "New project".into(), ButtonKind::Secondary)
                .on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.open_new_project(window, cx)))
                .into_any_element(),
        ];
        for (label, shell) in shells {
            let icon = crate::icons::shell_icon(&shell);
            actions.push(
                hero(SharedString::from(format!("welcome-shell-{label}")), icon, label, ButtonKind::Secondary)
                    .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                        this.activate_item(SidebarItem::Shell { project: None, shell: shell.clone() }, false, window, cx)
                    }))
                    .into_any_element(),
            );
        }
        island(p)
            .flex_1()
            .h_full()
            .px(px(32.))
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .bg(p.background)
            .child(wordmark(60., p.text))
            .child(div().mt(px(40.)).flex().flex_wrap().justify_center().gap(px(12.)).children(actions))
            .into_any_element()
    }

    /// The active Session's open question (a password, a host key…) as a modal over the whole
    /// window: the sidebar and the rest are blocked until it is answered. A press on the
    /// backdrop gives the keyboard back to the question. A dialog you opened yourself comes
    /// first; the question waits behind it.
    pub(super) fn render_prompt_modal(&self, p: AppPalette, cx: &mut Context<Self>) -> Option<AnyElement> {
        if self.overlay.is_some() {
            return None;
        }
        // A Remote Desktop asks inside its own pane.
        let view = self.active_entry()?.ui.view.terminal()?.clone();
        let card = view.update(cx, |view, cx| view.prompt_modal(cx))?;
        Some(
            div()
                .id("prompt-backdrop")
                .absolute()
                .occlude()
                // Below the title bar, as for the other dialogs.
                .top(px(TITLE_BAR_HEIGHT))
                .left_0()
                .right_0()
                .bottom_0()
                .flex()
                .items_center()
                .justify_center()
                .p(px(24.))
                .bg(alpha(0x000000, if p.dark { 0.5 } else { 0.2 }))
                .on_mouse_down(MouseButton::Left, move |_, window, cx| {
                    view.update(cx, |view, cx| view.focus_prompt(window, cx));
                })
                .child(card)
                .into_any_element(),
        )
    }
}
