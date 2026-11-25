//! The sidebar's Host and shell rows, their menus and their Sessions' cards.

use super::*;

impl Workspace {
    /// What an item's row shows, worked out from its Sessions.
    pub(super) fn item_state(&self, key: &ItemKey) -> ItemState<'_> {
        let sessions: Vec<&SessionEntry> = self.sessions_of(key).collect();
        let listed = self.sessions.listed(key);
        let grouped = !listed.is_empty();
        let expanded = grouped && !self.collapsed_items.contains(key);
        let shown = self.sessions.last_shown(key).or_else(|| sessions.first().map(|s| s.id()));
        // The Session the row itself is: its only one, or the terminal beside listed desktops.
        let main = if grouped {
            sessions.iter().find(|s| !listed.iter().any(|l| l.id() == s.id())).copied()
        } else {
            shown.and_then(|id| sessions.iter().find(|s| s.id() == id).copied()).or(sessions.first().copied())
        };
        let holds_active = sessions.iter().any(|s| Some(s.id()) == self.sessions.active());
        // Shown in the other half of the split: lit, more faintly than the active row. A row
        // that is the Host's terminal is lit for that terminal alone.
        let split = self.sessions.shown_split();
        let in_split = |s: &SessionEntry| Some(s.id()) != self.sessions.active() && split.is_some_and(|split| split.contains(s.id()));
        let beside = match main {
            Some(main) if grouped => in_split(main),
            _ => !holds_active && sessions.iter().any(|s| in_split(s)),
        };
        // A group's state: live if any Session is, connecting if any is on the way. A row that
        // is the Host's terminal shows that terminal's.
        let status = if grouped && main.is_none() {
            let any = |f: fn(&Status) -> bool| sessions.iter().any(|s| f(s.status()));
            Some(if any(|s| matches!(s, Status::Connected)) {
                Status::Connected
            } else if any(|s| matches!(s, Status::Connecting)) {
                Status::Connecting
            } else {
                Status::Disconnected(String::new())
            })
        } else {
            main.map(|s| s.status().clone())
        };
        let live = matches!(status, Some(Status::Connected | Status::Connecting));
        let busy = !holds_active && !beside
            && if grouped { sessions.iter().any(|s| self.sessions.busy_out_of_sight(s)) } else { main.is_some_and(|s| self.sessions.busy_out_of_sight(s)) };
        // With the Sessions listed below, the active one is marked there instead.
        let active = main.is_some_and(|s| Some(s.id()) == self.sessions.active()) || (holds_active && !expanded);
        // In a split that stays while another Session is shown: linked to the other half. Expanded,
        // the listed Sessions show it on their own rows.
        let remembered = self.sessions.split();
        let partner = |s: &SessionEntry| remembered.and_then(|split| split.other(s.id()));
        let linked = if expanded { main.and_then(partner) } else { sessions.iter().find_map(|s| partner(s)) };
        ItemState { sessions, listed, grouped, expanded, shown, main, beside, linked, status, live, busy, active }
    }

    /// The menu of an item's row: its right-click menu, and the one its ⋯ button and
    /// Shift+F10 open.
    pub(super) fn item_menu(
        &self,
        item: &SidebarItem,
        cx: &mut Context<Self>,
    ) -> impl Fn(PopupMenu, &mut Window, &mut Context<PopupMenu>) -> PopupMenu + Clone + 'static {
        let key = item.key();
        let ItemState { sessions, grouped, expanded, shown, main, linked, .. } = self.item_state(&key);
        let session_id = main.map(|s| s.id());
        let sessions_live = main.is_some_and(|s| s.is_live());
        let open_session = self.sessions.item_session(&key).and_then(|id| sessions.iter().find(|s| s.id() == id));
        let open_live = open_session.is_some_and(|s| s.is_live());
        let visible = self.page == Page::Sessions && open_session.is_some_and(|s| {
            !s.own_window() && (Some(s.id()) == self.sessions.active() || self.sessions.shown_split().is_some_and(|split| split.contains(s.id())))
        });
        let sidebar_focus = self.nav.focus.clone();
        let ws = cx.entity().downgrade();
        let menu_item_src = item.clone();
        let menu_key = key.clone();
        // What "Close all" acts on: every Session, live or dropped (see `end_item_sessions`).
        let count = sessions.len();
        // Beside the Session shown, unless that is the one this would show.
        let can_split = self.sessions.can_place(&key, None, Side::Right);
        let history = self.settings.command_history;
        // × and Delete only disconnect a terminal; with the setting off they close it.
        let keep_sessions = self.settings.keep_disconnected_sessions && main.is_some_and(|s| s.ui.view.desktop().is_none());
        let completion_ids: Vec<usize> = sessions.iter().map(|s| s.id()).collect();
        let linked = linked.is_some();
        // Its connected terminal, for Browse files: the row's own, else the last shown.
        let files_session = main
            .into_iter()
            .chain(shown.and_then(|id| sessions.iter().find(|s| s.id() == id).copied()))
            .chain(sessions.iter().copied())
            .find(|s| matches!(s.kind(), SessionKind::Ssh { .. }) && matches!(s.status(), Status::Connected))
            .map(|s| s.id());
        move |menu: PopupMenu, _: &mut Window, cx: &mut Context<PopupMenu>| {
            let item = menu_item_src.clone();
            let (open, another) = (item.clone(), item.clone());
            let new_label = if matches!(item, SidebarItem::Host { .. }) { "New terminal" } else { "New session" };
            // Hint the sidebar's row actions, rather than global New/Close, which act on
            // the active Session and may belong to a different Host. Keep click handlers
            // bound to this menu's item regardless of the keyboard cursor's position.
            let mut menu = menu.action_context(sidebar_focus.clone()).item(
                menu_item(new_label, Some("plus"), &ws, move |this, window, cx| this.activate_item(another.clone(), true, window, cx))
                    .action(Box::new(nav::NavOpenAnother)),
            );
            if let Some(id) = files_session {
                menu = menu.item(menu_item("Browse files", Some("folder-open"), &ws, move |this, window, cx| this.show_files(id, window, cx)));
            }
            if can_split {
                let split_key = menu_key.clone();
                menu = menu.item(menu_item("Open in split view", None, &ws, move |this, window, cx| {
                    this.open_in_split(&split_key, None, Side::Right, window, cx)
                }));
            }
            if linked {
                menu = menu.item(menu_item("Close split view", Some("columns-2"), &ws, |this, window, cx| this.close_split(window, cx)));
            }
            // A dropped Session still needs Connect, even with its retained output visible.
            if !open_live || !visible {
                menu = menu.item(menu_item(if open_live { "Show session" } else { "Connect" }, None, &ws, move |this, window, cx| {
                    this.activate_item(open.clone(), false, window, cx)
                }));
            }
            if grouped {
                let (fold, all) = (menu_key.clone(), menu_key.clone());
                menu = menu
                    .item(menu_item(
                        if expanded { "Collapse sessions" } else { "Expand sessions" },
                        None,
                        &ws,
                        move |this, _, cx| this.toggle_item_sessions(&fold, cx),
                    ))
                    .item(menu_item(
                        &format!("Close all {count}"),
                        None,
                        &ws,
                        move |this, window, cx| this.end_item_sessions(&all, false, window, cx),
                    ));
            }
            if let Some(id) = session_id {
                let live = sessions_live;
                menu = menu.item(
                    menu_item("Rename session…", None, &ws, move |this, window, cx| this.open_rename_session(id, window, cx))
                        .action(Box::new(nav::NavEdit)),
                );
                if live {
                    menu = live_session_items(menu, id, keep_sessions, &ws);
                } else {
                    menu = menu.item(
                        menu_item("Close session", None, &ws, move |this, window, cx| this.end_session(id, EndRequest::CloseNow, window, cx))
                            .action(Box::new(nav::NavDelete)),
                    );
                }
            }
            menu = notifications::completion_menu(menu, &ws, &completion_ids, cx);
            if let SidebarItem::Host { project, host } = &item {
                let (pa, ha) = (project.id.clone(), host.id.clone());
                let (pb, hb) = (project.id.clone(), host.id.clone());
                let (pd, hd) = (project.id.clone(), host.id.clone());
                let hc = host.id.clone();
                // Both desktops are offered by default. An explicit opt-out hides one;
                // an unconfigured one opens its settings to add its port and password.
                let desktops: Vec<Protocol> = [Protocol::Rdp, Protocol::Vnc].into_iter().filter(|protocol| host.desktop_enabled(*protocol)).collect();
                if !desktops.is_empty() {
                    menu = menu.separator();
                    // A Host that opens its desktop on a click offers its terminal here instead.
                    if !host.opens_with().is_ssh() {
                        let (pt, ht) = (project.id.clone(), host.id.clone());
                        menu = menu.item(menu_item("Open terminal", None, &ws, move |this, window, cx| {
                            this.show_terminal_of(&pt, &ht, window, cx)
                        }));
                    }
                    for protocol in desktops {
                        let (pr, hr) = (project.id.clone(), host.id.clone());
                        menu = if host.desktop(protocol).is_some() {
                            menu.item(menu_item(format!("Open {}", protocol.label()), None, &ws, move |this, window, cx| {
                                this.open_desktop(&pr, &hr, protocol, window, cx)
                            }))
                        } else {
                            menu.item(menu_item(format!("Set up {}…", protocol.label()), None, &ws, move |this, window, cx| {
                                this.set_up_desktop(pr.clone(), hr.clone(), protocol, window, cx)
                            }))
                        };
                    }
                }
                menu = menu
                    .separator()
                    .item(menu_item("Edit host…", None, &ws, move |this, window, cx| this.open_edit_host(pa.clone(), ha.clone(), window, cx)))
                    .item(menu_item("Duplicate host…", None, &ws, move |this, window, cx| {
                        this.duplicate_host(pd.clone(), hd.clone(), window, cx)
                    }))
                    .item(menu_item("Copy address", None, &ws, move |this, _, cx| this.copy_host_address(&pb, &hb, cx)));
                if history {
                    let hh = host.id.clone();
                    menu = menu.item(menu_item("Command history…", None, &ws, move |this, window, cx| {
                        this.open_history(&hh, window, cx)
                    }));
                }
                menu = menu
                    .separator()
                    .item(menu_item("Delete host…", Some("trash"), &ws, move |this, _, cx| this.confirm_delete_host(hc.clone(), cx)));
            }
            // In an elevated window every Session is elevated already.
            // Windows only: elsewhere there is no administrator mode to start a shell in.
            if let SidebarItem::Shell { project, shell } = &item
                && cfg!(windows)
                && !crate::elevate::is_elevated()
            {
                let (project, name) = (project.as_ref().map(|p| p.id.clone()), shell.name.clone());
                menu = menu.separator().item(menu_item("Run as administrator", None, &ws, move |this, window, cx| {
                    this.run_as_admin(project.clone(), name.clone(), window, cx)
                }));
            }
            menu
        }
    }

    /// One sidebar item, plus a row for each of its Sessions when it has several.
    ///
    /// Live items are bright with a status dot; idle ones are dimmed; a dropped Session
    /// that still holds its output shows a hollow ring until it is reconnected or discarded.
    /// An item with several Sessions is a group: its row shows how many and folds them away,
    /// and each Session gets a card under it, with its own close button.
    pub(super) fn render_item(&self, item: &SidebarItem, color: Hsla, indent: f32, p: AppPalette, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let key = item.key();
        let ItemState { listed, grouped, expanded, main, beside, linked, status, live, busy, active, .. } = self.item_state(&key);
        let icon = item.icon();
        let icon_size = if matches!(item, SidebarItem::Shell { .. }) { 16. } else { 15. };
        let icon_color = item.icon_color(&icon, if active || live { color } else { p.text_faint }, p);
        let listed_count = listed.len();
        let item_menu = self.item_menu(item, cx);

        let item_for_click = item.clone();
        let item_for_new = item.clone();
        let toggle_key = key.clone();
        let (click_key, fold_key) = (key.clone(), key.clone());
        let is_host = matches!(item, SidebarItem::Host { .. });
        let focused = self.nav_focus_on(&NavTarget::Item(key.clone()));
        let menu_open = self.row_menu_open(&NavTarget::Item(key.clone()));
        let dragged = self.dragged(item, None, color);
        // Shell identities stay clear while idle; their labels and status carry activity.
        let dim = !(active || live) && is_artwork(&icon) && matches!(item, SidebarItem::Host { .. });
        // The row's end: its state, and for a group how many Sessions and the fold chevron.
        // Folded out, the row keeps the group's state (or its terminal's, when it is one): the
        // Sessions' cards only mark what needs a look.
        let show_status = status.is_some();
        // The Session the row itself stands for closes from the row: its status dot turns into
        // × on hover, and a middle-click closes it, as Settings → Close from the sidebar says.
        let quick_close = self.settings.quick_close;
        let row_session = main.filter(|_| show_status).map(|s| s.id());
        let hover_close = row_session.filter(|_| quick_close.on_hover());
        let middle_close = row_session.filter(|_| quick_close.on_middle_click());
        let close_tip: SharedString = match main {
            Some(s) if s.is_live() && self.settings.keep_disconnected_sessions && s.ui.view.desktop().is_none() => "Disconnect".into(),
            _ => "Close session".into(),
        };
        let show_count = grouped && !expanded;
        // Room for every digit of the count, so 50 doesn't wrap onto two lines.
        let count_width = (listed_count.to_string().len() as f32 * 7.).max(14.);
        let trailing: f32 = [(show_status, 16.), (show_count, count_width), (grouped, 22.)]
            .iter()
            .filter(|(shown, _)| *shown)
            .map(|(_, w)| w + 4.)
            .sum::<f32>();
        let pad_right = if grouped { 6. } else { 10. };
        let new_tip: SharedString = if is_host { "New terminal (Ctrl+click)".into() } else { "New session (Ctrl+click)".into() };
        let more = self.menu_popover(
            SharedString::from(format!("more-{key:?}")),
            NavTarget::Item(key.clone()),
            MenuTrigger::new(
                icon_button(SharedString::from(format!("more-button-{key:?}")), "ellipsis", p)
                    .aria_label("More actions")
                    .size(px(24.))
                    .tooltip(|window, cx| Tooltip::new("More actions").build(window, cx)),
            ),
            cx,
        );
        // A name given to the Session the row stands for, after the Host's.
        let session_name = main.and_then(|s| s.name().clone());
        let label_color = if active { p.active_text } else if live { p.text } else { p.text_muted };
        let mut rows = vec![];
        rows.push(
            div()
                .id(SharedString::from(format!("item-{key:?}")))
                .group("item-row")
                .role(gpui::Role::TreeItem)
                .aria_label(match &session_name {
                    Some(name) => format!("{}, {name}", item.label()),
                    None => item.label().to_string(),
                })
                .aria_level(if indent > 10. { 2 } else { 1 })
                .aria_selected(active)
                .when(grouped, |el| el.aria_expanded(expanded))
                // Also the size of its right-click menu, which inherits it.
                .text_size(px(13.))
                .relative()
                .h(px(34.))
                // A nested row's highlight starts inside its Project's, so the tree reads as one.
                .when(indent > 10., |el| el.ml(px(NESTED_INSET)))
                .pl(px(if indent > 10. { indent - NESTED_INSET } else { indent }))
                .pr(px(pad_right))
                .flex()
                .items_center()
                .gap(px(10.))
                .rounded(p.corner(8.))
                .cursor_pointer()
                .when(active, |el| el.bg(p.active).text_color(p.active_text))
                .when(!active, |el| el.hover(|s| s.bg(p.hover)))
                .when(beside && !active && (!expanded || main.is_some()), |el| el.bg(p.hover))
                .on_click(cx.listener(move |this, event: &ClickEvent, window, cx| {
                    this.nav_clicked(NavTarget::Item(click_key.clone()));
                    this.activate_item(item_for_click.clone(), event.modifiers().control, window, cx)
                }))
                .when_some(middle_close, |el, id| {
                    el.on_aux_click(cx.listener(move |this, event: &ClickEvent, window, cx| {
                        if event.is_middle_click() {
                            cx.stop_propagation();
                            this.end_session(id, EndRequest::Close, window, cx);
                        }
                    }))
                })
                // Drag it onto the terminal area to open it beside, or onto another Project.
                .on_drag(dragged, self.drag_preview(cx))
                .child(
                    div()
                        .flex_shrink_0()
                        .when(dim, |el| el.opacity(0.55))
                        .child(icon_el(&icon, icon_size, icon_color)),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .flex()
                        .items_center()
                        .gap(px(5.))
                        .child(
                            div()
                                .min_w_0()
                                .flex_shrink_1()
                                .truncate()
                                .text_color(label_color)
                                .when(!active, |el| el.group_hover("item-row", |s| s.text_color(p.hover_text)))
                                .child(item.label().to_string()),
                        )
                        .when_some(session_name, |el, name| {
                            el.child(
                                div()
                                    .min_w_0()
                                    .flex_shrink_1()
                                    .truncate()
                                    .text_size(px(12.5))
                                    .text_color(if active { p.active_text.opacity(0.75) } else { p.text_faint })
                                    .child(format!("· {name}")),
                            )
                        })
                        .children(linked.and_then(|partner| {
                            self.split_link(partner, if active { p.active_text.opacity(0.75) } else { p.text_faint })
                        })),
                )
                // The state stays put while the row's actions show beside it.
                .child(
                    div()
                        .flex_shrink_0()
                        .flex()
                        .items_center()
                        .gap(px(4.))
                        .id("item-status")
                        .when(show_status, |el| {
                            el.child(
                                div()
                                    .relative()
                                    .w(px(16.))
                                    .h(px(20.))
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .child(
                                        div()
                                            .when(hover_close.is_some(), |el| el.group_hover("item-row", |s| s.invisible()))
                                            .child(status_indicator(status.as_ref(), busy, p)),
                                    )
                                    .when_some(hover_close, |el, id| {
                                        let tip = close_tip.clone();
                                        el.child(
                                            div()
                                                .absolute()
                                                .top_0()
                                                .left(px(-2.))
                                                .invisible()
                                                .group_hover("item-row", |s| s.visible())
                                                .child(
                                                    close_button(SharedString::from(format!("row-close-{id}")), 20., p)
                                                        .aria_label(tip.clone())
                                                        .tooltip(move |window, cx| Tooltip::new(tip.clone()).build(window, cx))
                                                        .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                                                            cx.stop_propagation();
                                                            this.end_session(id, EndRequest::Close, window, cx);
                                                        })),
                                                ),
                                        )
                                    }),
                            )
                        })
                        .when(show_count, |el| {
                            el.child(
                                div()
                                    .w(px(count_width))
                                    .flex_shrink_0()
                                    .flex()
                                    .justify_center()
                                    .whitespace_nowrap()
                                    .text_size(px(11.))
                                    .text_color(p.text_faint)
                                    .child(listed_count.to_string()),
                            )
                        })
                        // A group folds its Sessions away with the chevron, which is always there.
                        .when(grouped, |el| {
                            el.child(
                                div()
                                    .id(SharedString::from(format!("fold-{key:?}")))
                                    .role(gpui::Role::Button)
                                    .aria_label(if expanded { "Hide sessions" } else { "Show sessions" })
                                    .aria_expanded(expanded)
                                    .flex_shrink_0()
                                    .size(px(22.))
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .rounded(p.corner(7.))
                                    .hover(|s| s.bg(p.hover))
                                    .tooltip(move |window, cx| {
                                        Tooltip::new(if expanded { "Hide sessions" } else { "Show sessions" }).build(window, cx)
                                    })
                                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                                        cx.stop_propagation();
                                        this.nav_clicked(NavTarget::Item(fold_key.clone()));
                                        this.toggle_item_sessions(&toggle_key, cx);
                                    }))
                                    .child(icon_el(if expanded { "chevron-down" } else { "chevron-right" }, 14., p.text_faint)),
                            )
                        }),
                )
                .child(
                    // Just + and ⋯: disconnecting and remote desktops are in the menu (⋯ or a
                    // right-click), which keeps the row calm. Opaque sidebar colour under the
                    // row's translucent hover colour, so the buttons' backdrop matches the row
                    // and covers the end of a long label. Also shown on the keyboard cursor's
                    // row, and while its menu is open.
                    div()
                        .absolute()
                        .id("item-hover-actions")
                        .right(px(pad_right + trailing))
                        .top(px(5.))
                        .rounded(p.corner(7.))
                        .bg(p.sidebar)
                        .when(!(menu_open || focused), |el| el.invisible().group_hover("item-row", |s| s.visible()))
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .gap(px(2.))
                                .rounded(p.corner(7.))
                                .bg(if active { p.active } else { p.hover })
                                .child(
                                    icon_button(SharedString::from(format!("another-{key:?}")), "plus", p)
                                        .aria_label(new_tip.clone())
                                        .size(px(24.))
                                        .tooltip(move |window, cx| Tooltip::new(new_tip.clone()).build(window, cx))
                                        .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                                            cx.stop_propagation();
                                            this.activate_item(item_for_new.clone(), true, window, cx);
                                        })),
                                )
                                .child(more),
                        ),
                )
                .when(focused, |el| el.child(focus_ring(p.corner(8.), p)))
                .context_menu(item_menu)
                .into_any_element(),
        );
        if expanded {
            rows.push(self.render_session_cards(item, &listed, color, indent, p, cx));
        }
        rows
    }

    /// An item's listed Sessions as cards under its row, hung off a guide line so they never
    /// read as Hosts. As many cards to a line as fit (see `session_card_rows`), the lines
    /// balanced and each card stretched to share its line, up to `SESSION_CARD_MAX_WIDTH`.
    pub(super) fn render_session_cards(
        &self,
        item: &SidebarItem,
        listed: &[&SessionEntry],
        color: Hsla,
        indent: f32,
        p: AppPalette,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        // The guide hangs from the middle of the item's icon.
        let guide = indent + 7.;
        let mut sessions = listed.iter();
        let rows = session_card_rows(listed.len(), self.sidebar_width_px, indent).into_iter().map(|count| {
            div().flex().gap(px(5.)).children(sessions.by_ref().take(count).map(|session| {
                // No wider than `SESSION_CARD_MAX_WIDTH`, so a lone card doesn't stretch into a bar.
                div().flex_1().flex_basis(px(0.)).min_w_0().max_w(px(SESSION_CARD_MAX_WIDTH)).child(self.render_session_card(item, session, color, p, cx))
            }))
        });
        div()
            .relative()
            .ml(px(guide + 7.))
            .mr(px(4.))
            .pl(px(4.))
            .py(px(3.))
            .flex()
            .flex_col()
            .gap(px(5.))
            .child(div().absolute().left(px(-7.)).top(px(-2.)).bottom(px(-2.)).w(px(1.)).bg(p.border))
            .children(rows)
            .into_any_element()
    }

    /// One Session of an item, as a card in the grid under its row: what it is (terminal or
    /// remote desktop) and a short label, its number, its protocol or the name it was given. The
    /// shown Session's card is raised; the rest lie flat. A corner mark appears only when
    /// something needs a look (connecting, dropped, output out of sight), and × on hover
    /// disconnects it, or closes it once it has dropped.
    pub(super) fn render_session_card(&self, item: &SidebarItem, session: &SessionEntry, color: Hsla, p: AppPalette, cx: &mut Context<Self>) -> AnyElement {
        let id = session.id();
        let active = Some(id) == self.sessions.active();
        let focused = self.nav_focus_on(&NavTarget::Session(id));
        let beside = !active && self.sessions.shown_split().is_some_and(|s| s.contains(id));
        let linked = self.sessions.split().and_then(|s| s.other(id));
        let dragged = self.dragged(item, Some(id), color);
        let live = session.is_live();
        let busy = self.sessions.busy_out_of_sight(session);
        let own_window = session.own_window();
        let (label, title) = session_card_text(session);
        let session_menu = self.session_menu(id, cx);
        // What the Session is: a Host's terminal or desktop by kind, a shell by its own icon.
        let icon = match (session.kind(), item) {
            (SessionKind::Desktop { .. }, _) => "monitor".to_string(),
            (_, SidebarItem::Host { .. }) => "square-terminal".to_string(),
            (_, SidebarItem::Shell { .. }) => item.icon(),
        };
        let icon_color = match item {
            SidebarItem::Shell { .. } => item.icon_color(&icon, color, p),
            SidebarItem::Host { .. } if active => color,
            SidebarItem::Host { .. } => p.text_muted,
        };
        let text_color = if active { p.active_text } else if live { p.text_muted } else { p.text_faint };
        let disconnects = live && self.settings.keep_disconnected_sessions && session.ui.view.desktop().is_none();
        let tip: SharedString = if disconnects { "Disconnect".into() } else { "Close session".into() };
        let close = close_button(SharedString::from(format!("close-{id}")), 20., p)
            .aria_label(tip.clone())
            .tooltip(move |window, cx| Tooltip::new(tip.clone()).build(window, cx))
            .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                cx.stop_propagation();
                this.end_session(id, EndRequest::Close, window, cx);
            }));
        // Only a state that needs a look gets a mark: a live, quiet Session's is its plain card.
        let mark: Option<AnyElement> = match session.status() {
            Status::Connected if busy => Some(spinner(SharedString::from(format!("card-busy-{id}")), 10., p.success)),
            Status::Connected => None,
            Status::Connecting => Some(div().size(px(5.)).rounded_full().bg(p.warning).into_any_element()),
            Status::Disconnected(_) => {
                Some(div().size(px(6.)).rounded_full().border_1().border_color(p.text_faint).into_any_element())
            }
        };
        let tooltip_text = title.clone();
        div()
            .id(SharedString::from(format!("session-{id}")))
            .group("session-card")
            .role(gpui::Role::TreeItem)
            .aria_label(title.clone())
            .aria_level(3)
            .aria_selected(active)
            .relative()
            .min_w_0()
            .h(px(40.))
            .px(px(8.))
            .flex()
            .items_center()
            .justify_center()
            .gap(px(6.))
            .text_size(px(12.5))
            .font_weight(FontWeight::MEDIUM)
            .text_color(text_color)
            .rounded(p.corner(10.))
            .cursor_pointer()
            // Like a pinned tab: a filled tile with a hairline edge, and the shown one ringed in
            // the item's colour.
            .map(|el| {
                if active {
                    el.bg(p.active).border_2().border_color(color)
                } else {
                    el.bg(p.hover)
                        .border_1()
                        .border_color(if beside { color.opacity(0.5) } else { p.border })
                        .hover(|s| s.border_color(p.text_faint).text_color(p.hover_text))
                }
            })
            .tooltip(move |window, cx| Tooltip::new(tooltip_text.clone()).build(window, cx))
            .on_drag(dragged, self.drag_preview(cx))
            .when(self.settings.quick_close.on_middle_click(), |el| {
                el.on_aux_click(cx.listener(move |this, event: &ClickEvent, window, cx| {
                    if event.is_middle_click() {
                        cx.stop_propagation();
                        this.end_session(id, EndRequest::Close, window, cx);
                    }
                }))
            })
            .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                this.activate(id, window, cx);
                let dropped = this.sessions.get(id).is_some_and(|s| !s.is_live());
                if dropped && !own_window {
                    this.reconnect(id, window, cx);
                }
            }))
            .child(icon_el(&icon, 16., icon_color))
            .child(div().min_w_0().truncate().child(label))
            .children(linked.and_then(|partner| self.split_link(partner, if active { p.active_text.opacity(0.75) } else { p.text_faint })))
            .when(own_window, |el| {
                el.child(
                    div()
                        .id("session-own-window")
                        .flex_shrink_0()
                        .tooltip(|window, cx| Tooltip::new("In its own window").build(window, cx))
                        .child(icon_el("app-window", 13., p.text_faint)),
                )
            })
            .when_some(mark, |el, mark| {
                el.child(div().id("session-status").absolute().top(px(5.)).right(px(5.)).group_hover("session-card", |s| s.invisible()).child(mark))
            })
            // Over the card's end on hover, on the card's own colour so it covers a long label.
            .child(
                div()
                    .absolute()
                    .right(px(5.))
                    .top(px(10.))
                    .rounded(p.corner(6.))
                    .bg(if active { p.active } else { p.sidebar })
                    .invisible()
                    .group_hover("session-card", |s| s.visible())
                    .child(div().rounded(p.corner(6.)).bg(if active { p.active } else { p.hover }).child(close)),
            )
            .children(self.row_menu_anchor(NavTarget::Session(id), cx))
            .when(focused, |el| el.child(focus_ring(p.corner(10.), p)))
            .context_menu(session_menu)
            .into_any_element()
    }

    /// The link on a sidebar row whose Session is in the remembered split, shown or not: the
    /// split stays while another Session is shown and comes back with either of its two. Its
    /// tooltip names the Session on the other side.
    pub(super) fn split_link(&self, partner: usize, color: Hsla) -> Option<AnyElement> {
        let name = self.sessions.display_name(self.sessions.get(partner)?);
        let tip: SharedString = format!("In split view with {name}").into();
        Some(
            div()
                .id("split-link")
                .flex_shrink_0()
                .aria_label(tip.clone())
                .tooltip(move |window, cx| Tooltip::new(tip.clone()).build(window, cx))
                .child(icon_el("link-2", 12., color))
                .into_any_element(),
        )
    }

    /// The menu of a Session's own row.
    pub(super) fn session_menu(&self, id: usize, cx: &mut Context<Self>) -> impl Fn(PopupMenu, &mut Window, &mut Context<PopupMenu>) -> PopupMenu + Clone + 'static {
        let ws = cx.entity().downgrade();
        let session = self.sessions.get(id);
        let dropped = session.is_some_and(|s| !s.is_live());
        let desktop = session.and_then(desktop_protocol);
        let own_window = session.is_some_and(|s| s.own_window());
        let keep = self.settings.keep_disconnected_sessions && desktop.is_none();
        let files = session.is_some_and(|s| matches!(s.kind(), SessionKind::Ssh { .. }) && matches!(s.status(), Status::Connected));
        let in_split = self.sessions.in_split(id);
        let sidebar_focus = self.nav.focus.clone();
        move |menu: PopupMenu, _: &mut Window, cx: &mut Context<PopupMenu>| {
            let mut menu = menu.action_context(sidebar_focus.clone());
            if files {
                menu = menu.item(menu_item("Browse files", Some("folder-open"), &ws, move |this, window, cx| this.show_files(id, window, cx)));
            }
            if in_split {
                menu = menu.item(menu_item("Close split view", Some("columns-2"), &ws, |this, window, cx| this.close_split(window, cx)));
            }
            menu = menu.item(
                menu_item("Rename session…", None, &ws, move |this, window, cx| this.open_rename_session(id, window, cx))
                    .action(Box::new(nav::NavEdit)),
            );
            menu = menu.separator();
            let mut menu = if dropped {
                menu.item(menu_item("Reconnect", Some("refresh-cw"), &ws, move |this, window, cx| {
                    this.activate(id, window, cx);
                    this.reconnect(id, window, cx);
                }))
                .item(menu_item("Close", None, &ws, move |this, window, cx| this.end_session(id, EndRequest::CloseNow, window, cx)).action(Box::new(nav::NavDelete)))
            } else {
                live_session_items(menu, id, keep, &ws)
            };
            if desktop.is_some() {
                menu = menu.separator().item(if own_window {
                    menu_item("Show its window", None, &ws, move |this, window, cx| this.activate(id, window, cx))
                } else {
                    menu_item("Open in its own window", None, &ws, move |this, window, cx| {
                        this.detach_desktop(id, None, window, cx)
                    })
                });
            }
            notifications::completion_menu(menu, &ws, &[id], cx)
        }
    }

    /// Open a Local Shell in a new, elevated Komando window. Windows asks for consent (UAC)
    /// first; saying no there leaves everything as it was. See `crate::elevate`.
    pub(super) fn run_as_admin(&mut self, project: Option<String>, name: String, window: &mut Window, cx: &mut Context<Self>) {
        let parent = crate::platform::window_handle(window);
        let (tx, rx) = futures::channel::oneshot::channel();
        let label = name.clone();
        // The call blocks until the prompt is answered; this window keeps drawing meanwhile.
        std::thread::spawn(move || {
            let _ = tx.send(crate::elevate::open_shell_elevated(parent, project.as_deref(), &name));
        });
        cx.spawn(async move |this, cx| {
            let Ok(outcome) = rx.await else { return };
            let _ = this.update(cx, |this, cx| match outcome {
                Ok(crate::elevate::Outcome::Started) => {
                    this.show_toast(format!("{label} opens as administrator in a new window."), cx)
                }
                Ok(crate::elevate::Outcome::Cancelled) => {}
                Err(err) => {
                    log::error!("run as administrator: {err:#}");
                    this.show_toast(format!("Couldn't run {label} as administrator: {err:#}"), cx)
                }
            });
        })
        .detach();
    }

    /// Fold an item's Sessions away under its row, or show them again.
    pub(super) fn toggle_item_sessions(&mut self, key: &ItemKey, cx: &mut Context<Self>) {
        if !self.collapsed_items.remove(key) {
            self.collapsed_items.insert(key.clone());
        }
        cx.notify();
    }
}
