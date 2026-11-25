//! The sidebar: its rows, Projects and their menus.

use super::*;

impl Workspace {
    /// The sidebar's rows, in order. Kept small (indices, counts) so working it out on every
    /// frame is cheap; only the rows in view are built into elements.
    pub(super) fn sidebar_rows(&self) -> Vec<SidebarRow> {
        // Each item's Sessions, found by borrowed ids: most items have none, and need no key.
        let mut counts: HashMap<(bool, Option<&str>, &str), (&ItemKey, usize)> = HashMap::new();
        for s in &self.sessions {
            let id = match &s.item() {
                ItemKey::Host { project, host } => (true, Some(project.as_str()), host.as_str()),
                ItemKey::Shell { project, name } => (false, project.as_deref(), name.as_str()),
            };
            counts.entry(id).or_insert((&s.item(), 0)).1 += 1;
        }
        let nested = |id| counts.get(&id).map_or(0, |&(key, count)| self.nested_rows(key, count));
        let hosts = self.library.host_indices();
        let mut rows = vec![];
        let shells = self.sidebar_shells();
        for (index, shell) in shells.iter().enumerate() {
            let sessions = nested((false, None, &shell.name));
            rows.push(SidebarRow::Shell { index, sessions });
        }
        rows.push(SidebarRow::ProjectsHeader);
        if self.library.projects.is_empty() {
            rows.push(SidebarRow::NoProjects);
        }
        for (pi, project) in self.library.projects.iter().enumerate() {
            rows.push(SidebarRow::Project { index: pi });
            if self.settings.collapsed_projects.contains(&project.id) {
                continue;
            }
            let start = rows.len();
            for id in &project.hosts {
                let Some(&host) = hosts.get(id.as_str()) else { continue };
                let sessions = nested((true, Some(&project.id), id));
                rows.push(SidebarRow::Host { project: pi, host, sessions });
            }
            for (shell, sh) in project.shells.iter().enumerate() {
                let sessions = nested((false, Some(&project.id), &sh.name));
                rows.push(SidebarRow::ProjectShell { project: pi, shell, sessions });
            }
            if rows.len() == start {
                rows.push(SidebarRow::FirstHost { project: pi });
            }
        }
        rows
    }

    /// One row of the sidebar list, built only when it's in view.
    fn render_sidebar_row(&mut self, ix: usize, cx: &mut Context<Self>) -> AnyElement {
        let p = cx.global::<Appearance>().app.navigation();
        let Some(row) = self.sidebar_rows.get(ix).cloned() else { return div().into_any_element() };
        let element = match row {
            SidebarRow::Shell { index, .. } => match self.sidebar_shells().get(index).cloned() {
                Some(shell) => div()
                    .flex()
                    .flex_col()
                    .gap(px(2.))
                    .children(self.render_item(&SidebarItem::Shell { project: None, shell }, p.brand, 8., p, cx))
                    .into_any_element(),
                None => div().into_any_element(),
            },
            SidebarRow::ProjectsHeader => {
        div()
                .px(px(8.))
                .pt(px(12.))
                .pb(px(2.))
                .flex()
                .items_center()
                .justify_between()
                .child(section_label("Projects", p))
                .child(
                    icon_button("new-project", "folder-plus", p)
                        .aria_label("New project")
                        .size(px(24.))
                        .tooltip(|window, cx| Tooltip::new("New project").build(window, cx))
                        .on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.open_new_project(window, cx))),
                )
            .into_any_element()
            }
            SidebarRow::NoProjects => {
            div()
                    .px(px(8.))
                    .py(px(6.))
                    .text_size(px(12.5))
                    .line_height(px(18.))
                    .text_color(p.text_faint)
                    .child("Group your servers into projects, each with its own colour.")
                .into_any_element()
            }
            SidebarRow::Project { index } => self.render_project_row(index, p, cx),
            SidebarRow::FirstHost { project } => {
                let Some(project) = self.library.projects.get(project) else { return div().into_any_element() };
                let pid_add = project.id.clone();
                let focused = self.nav_focus_on(&NavTarget::AddHost(project.id.clone()));
                div()
                        .id(SharedString::from(format!("first-host-{}", project.id)))
                        .relative()
                        .when(focused, |el| el.child(focus_ring(p.corner(8.), p)))
                        .h(px(32.))
                        .ml(px(NESTED_INSET))
                        .pl(px(NESTED_INDENT - NESTED_INSET))
                        .flex()
                        .items_center()
                        .gap(px(10.))
                        .rounded(p.corner(8.))
                        .cursor_pointer()
                        .text_size(px(12.5))
                        .text_color(p.text_faint)
                        .hover(|s| s.bg(p.hover).text_color(p.text_muted))
                        .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| this.open_new_host(pid_add.clone(), window, cx)))
                        .child(icon_el("plus", 14., p.text_faint))
                        .child("Add a host")
                    .into_any_element()
            }
            SidebarRow::Host { project, host, .. } => {
                let (Some(project), Some(host)) = (self.library.projects.get(project), self.library.hosts.get(host)) else {
                    return div().into_any_element();
                };
                let color = project_color(&project.color);
                let item = SidebarItem::Host { project: Arc::new(project.clone()), host: Arc::new(host.clone()) };
                div().flex().flex_col().gap(px(2.))
                    .children(self.render_item(&item, color, NESTED_INDENT, p, cx)).into_any_element()
            }
            SidebarRow::ProjectShell { project, shell, .. } => {
                let Some(project) = self.library.projects.get(project) else { return div().into_any_element() };
                let Some(shell) = project.shells.get(shell) else { return div().into_any_element() };
                let color = project_color(&project.color);
                let item = SidebarItem::Shell { project: Some(Arc::new(project.clone())), shell: shell.clone() };
                div().flex().flex_col().gap(px(2.))
                    .children(self.render_item(&item, color, NESTED_INDENT, p, cx)).into_any_element()
            }
        };
        // Every row of a Project's group takes a Host dropped on it (moved to that Project);
        // the whole group lights up while one is held over it.
        let project_index = match row {
            SidebarRow::Project { index }
            | SidebarRow::FirstHost { project: index }
            | SidebarRow::Host { project: index, .. }
            | SidebarRow::ProjectShell { project: index, .. } => Some(index),
            _ => None,
        };
        let project_id = project_index.and_then(|i| self.library.projects.get(i)).map(|p| p.id.clone());
        let wrapper = div().id(("sidebar-row", ix)).mx(px(4.)).px(px(4.)).pb(px(2.)).child(element);
        let Some(project_id) = project_id else { return wrapper.into_any_element() };
        let lit = self.drop_project(cx) == Some(project_id.as_str())
            || matches!(&self.drop_target, Some(DropTarget::ProjectOrder { id, .. }) if id == &project_id && cx.has_active_drag());
        let first = matches!(row, SidebarRow::Project { .. });
        let last = !matches!(
            self.sidebar_rows.get(ix + 1),
            Some(SidebarRow::FirstHost { project } | SidebarRow::Host { project, .. } | SidebarRow::ProjectShell { project, .. })
                if Some(*project) == project_index
        );
        let blue = split::drop_color();
        let (over, dropped) = (project_id.clone(), project_id);
        let (order_over, order_drop) = (over.clone(), dropped.clone());
        // A Host dragged within its Project: a line where it would go.
        let line = self.reorder_line(ix, cx);
        wrapper
            .relative()
            .when_some(line, |el, after| {
                el.child(
                    div()
                        .absolute()
                        .left(px(10.))
                        .right(px(6.))
                        .h(px(2.))
                        .rounded(px(1.))
                        .bg(blue)
                        .map(|el| if after { el.bottom(px(0.)) } else { el.top(px(0.)) }),
                )
            })
            .when(lit, |el| {
                el.bg(blue.opacity(if p.dark { 0.16 } else { 0.1 }))
                    .when(first, |el| el.rounded_t(p.corner(10.)))
                    .when(last, |el| el.rounded_b(p.corner(10.)))
            })
            .on_drag_move(cx.listener(move |this, event: &gpui::DragMoveEvent<DraggedItem>, _, cx| {
                this.drag_over_project(&over, ix, event, cx)
            }))
            .on_drop(cx.listener(move |this, dragged: &DraggedItem, _, cx| this.drop_on_project(&dropped, dragged, cx)))
            .on_drag_move(cx.listener(move |this, event: &gpui::DragMoveEvent<DraggedProject>, _, cx| {
                this.drag_project_order(&order_over, ix, event, cx)
            }))
            .on_drop(cx.listener(move |this, dragged: &DraggedProject, _, cx| this.drop_project_order(&order_drop, dragged, cx)))
            .into_any_element()
    }

    fn render_project_row(&self, index: usize, p: AppPalette, cx: &mut Context<Self>) -> AnyElement {
        let Some(project) = self.library.projects.get(index) else { return div().into_any_element() };
            let color = project_color(&project.color);
            let collapsed = self.settings.collapsed_projects.contains(&project.id);
            let sessions: Vec<&SessionEntry> = self.sessions.iter().filter(|s| match s.item() {
                ItemKey::Host { project: id, host } => id == &project.id && project.hosts.contains(host),
                ItemKey::Shell { project: id, name } => id.as_ref() == Some(&project.id) && project.shells.iter().any(|sh| &sh.name == name),
            }).collect();
            let live = sessions.iter().filter(|s| s.is_live()).count();
            let busy = sessions.iter().any(|s| self.sessions.busy_out_of_sight(s));
            // Folded away with the Session in use inside: the row says so.
            let holds_active = collapsed && sessions.iter().any(|s| Some(s.id()) == self.sessions.active());
            let focused = self.nav_focus_on(&NavTarget::Project(project.id.clone()));
            let pid = project.id.clone();
            let pid_host = project.id.clone();
            // A Host from another Project is held over this one's group.
            let move_here = self.drop_project(cx) == Some(project.id.as_str());
            let project_menu = self.project_menu(index, cx);
            let click_id = project.id.clone();
            div()
                    .id(SharedString::from(format!("project-{}", project.id)))
                    .group("project-row")
                    .role(gpui::Role::TreeItem)
                    .aria_label(project.name.clone())
                    .aria_level(1)
                    .aria_expanded(!collapsed)
                    .text_size(px(13.))
                    .relative()
                    .h(px(36.))
                    .px(px(8.))
                    .mt(px(4.))
                    .flex()
                    .items_center()
                    .gap(px(10.))
                    .rounded(p.corner(8.))
                    .cursor_pointer()
                    .tooltip(|window, cx| Tooltip::new("Drag to reorder projects").build(window, cx))
                    .on_drag(
                        DraggedProject { id: project.id.clone(), label: project.name.clone().into(), icon: project.icon.clone(), color },
                        self.project_drag_preview(cx),
                    )
                    .hover(|s| s.bg(p.hover))
                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                        this.nav_clicked(NavTarget::Project(click_id.clone()));
                        this.set_project_collapsed(&pid, !collapsed, cx);
                    }))
                    // Folded away with the Session in use inside: a bar in the Project's colour
                    // at its edge, where hovering can't be mistaken for it.
                    .when(holds_active, |el| {
                        el.child(div().absolute().left(px(0.)).top(px(9.)).bottom(px(9.)).w(px(3.)).rounded(px(2.)).bg(color))
                    })
                    .child(badge(&project.icon, color, 22.))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_size(px(13.))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(p.text)
                            .child(project.name.clone()),
                    )
                    // When collapsed, show how many Sessions are live inside. It gives way to the
                    // row's add button while hovered, so the two never sit on top of each other.
                    .when(collapsed && live > 0, |el| {
                        el.child(
                            div()
                                .flex_shrink_0()
                                .flex()
                                .items_center()
                                .gap(px(5.))
                                .text_size(px(11.))
                                .text_color(p.text_faint)
                                .group_hover("project-row", |s| s.invisible())
                                .when(busy, |el| el.child(activity_icon(p)))
                                .child(format!("{live} live")),
                        )
                    })
                    .child(
                        // Like a host row's buttons: the hover colour over the sidebar's, so its
                        // backdrop matches the hovered row.
                        div()
                            .absolute()
                            .right(px(26.))
                            .top(px(6.))
                            .rounded(p.corner(7.))
                            .bg(p.sidebar)
                            .when(!focused, |el| el.invisible().group_hover("project-row", |s| s.visible()))
                            .child(
                                div().rounded(p.corner(7.)).bg(p.hover).child(
                                    icon_button(SharedString::from(format!("add-host-{}", project.id)), "plus", p)
                                        .aria_label("Add host")
                                        .size(px(24.))
                                        .tooltip(|window, cx| Tooltip::new("Add host").build(window, cx))
                                        .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                                            cx.stop_propagation();
                                            this.open_new_host(pid_host.clone(), window, cx);
                                        })),
                                ),
                            ),
                    )
                    .map(|el| {
                        if move_here {
                            el.child(
                                div()
                                    .flex_shrink_0()
                                    .text_size(px(11.5))
                                    .font_weight(FontWeight::MEDIUM)
                                    .text_color(split::drop_color())
                                    .child("Move here"),
                            )
                        } else {
                            el.child(icon_el(if collapsed { "chevron-right" } else { "chevron-down" }, 14., p.text_faint))
                        }
                    })
                    .children(self.row_menu_anchor(NavTarget::Project(project.id.clone()), cx))
                    .when(focused, |el| el.child(focus_ring(p.corner(8.), p)))
                    .context_menu(project_menu)
                .into_any_element()
    }

    /// A Project row's menu.
    pub(super) fn project_menu(&self, index: usize, cx: &mut Context<Self>) -> impl Fn(PopupMenu, &mut Window, &mut Context<PopupMenu>) -> PopupMenu + Clone + 'static {
        let ws = cx.entity().downgrade();
        let id = self.library.projects.get(index).map(|p| p.id.clone()).unwrap_or_default();
        let collapsed = self.settings.collapsed_projects.contains(&id);
        move |menu: PopupMenu, _: &mut Window, _: &mut Context<PopupMenu>| {
            let (a, b, c, d, e) = (id.clone(), id.clone(), id.clone(), id.clone(), id.clone());
            menu.item(menu_item("Add host…", Some("plus"), &ws, move |this, window, cx| this.open_new_host(a.clone(), window, cx)))
                .item(menu_item("Edit project…", None, &ws, move |this, window, cx| this.open_edit_project(b.clone(), window, cx)))
                .item(menu_item("Share project…", None, &ws, move |this, window, cx| this.share_project(&e, window, cx)))
                .item(menu_item(if collapsed { "Expand" } else { "Collapse" }, None, &ws, move |this, _, cx| {
                    this.set_project_collapsed(&c, !collapsed, cx);
                }))
                .separator()
                .item(menu_item("Delete project…", Some("trash"), &ws, move |this, _, cx| this.confirm_delete_project(d.clone(), cx)))
        }
    }

    pub(super) fn render_sidebar(&mut self, p: AppPalette, window: &Window, cx: &mut Context<Self>) -> AnyElement {
        let p = p.navigation();
        // Keep the primary labels readable near the 200px minimum; secondary hints return
        // once there is room for them. Use the clamped width, including in small windows.
        let width = self.sidebar_width(window);
        let compact = width < 240.;
        self.sidebar_compact = compact;
        self.sidebar_width_px = width;
        // Only the rows in view are built (hundreds of hosts would otherwise cost milliseconds
        // on every frame). Row heights are measured once and kept until the rows change.
        let rows = crate::perf::time("sidebar rows", || self.sidebar_rows());
        if rows != self.sidebar_rows {
            // Replace only the rows that changed. A reset would scroll the list to the top
            // whenever a Session opens or closes.
            let (old, new) = changed_rows(&self.sidebar_rows, &rows);
            self.sidebar_list.splice(old, new);
            self.sidebar_rows = rows;
        }
        // Keyboard use (see `nav.rs`): the cursor shows while the keyboard is in the list.
        self.nav.focused = self.nav.focus.is_focused(window);
        if self.nav.focused && self.nav.cursor.is_none() {
            self.nav.cursor = self.nav_targets(&self.sidebar_rows).first().map(|(t, _)| t.clone());
        }
        if let Some(ix) = self.take_nav_reveal() {
            self.sidebar_list.scroll_to_reveal_item(ix);
        }
        let list = div()
            .id("sidebar-list")
            .key_context(nav::CONTEXT)
            .track_focus(&self.nav.focus)
            .role(gpui::Role::Tree)
            .aria_label("Hosts and sessions")
            .flex_1()
            .min_h_0()
            .flex()
            .flex_col()
            .on_action(cx.listener(|this, _: &nav::NavUp, window, cx| if this.nav_owns(window, cx) { this.nav_step(-1, cx) }))
            .on_action(cx.listener(|this, _: &nav::NavDown, window, cx| if this.nav_owns(window, cx) { this.nav_step(1, cx) }))
            .on_action(cx.listener(|this, _: &nav::NavFirst, window, cx| if this.nav_owns(window, cx) { this.nav_step(isize::MIN, cx) }))
            .on_action(cx.listener(|this, _: &nav::NavLast, window, cx| if this.nav_owns(window, cx) { this.nav_step(isize::MAX, cx) }))
            .on_action(cx.listener(|this, _: &nav::NavExpand, window, cx| if this.nav_owns(window, cx) { this.nav_expand(cx) }))
            .on_action(cx.listener(|this, _: &nav::NavCollapse, window, cx| if this.nav_owns(window, cx) { this.nav_collapse(cx) }))
            .on_action(cx.listener(|this, _: &nav::NavOpen, window, cx| if this.nav_owns(window, cx) { this.nav_open(false, window, cx) }))
            .on_action(cx.listener(|this, _: &nav::NavOpenAnother, window, cx| if this.nav_owns(window, cx) { this.nav_open(true, window, cx) }))
            .on_action(cx.listener(|this, _: &nav::NavEdit, window, cx| if this.nav_owns(window, cx) { this.nav_edit(false, window, cx) }))
            .on_action(cx.listener(|this, _: &nav::NavEditHost, window, cx| if this.nav_owns(window, cx) { this.nav_edit(true, window, cx) }))
            .on_action(cx.listener(|this, _: &nav::NavDelete, window, cx| if this.nav_owns(window, cx) { this.nav_delete(false, window, cx) }))
            .on_action(cx.listener(|this, _: &nav::NavDeleteHost, window, cx| if this.nav_owns(window, cx) { this.nav_delete(true, window, cx) }))
            .on_action(cx.listener(|this, _: &nav::NavMenu, window, cx| if this.nav_owns(window, cx) { this.nav_menu(window, cx) }))
            // A click focuses the list too; the cursor shows again once a key is used.
            .on_mouse_down(MouseButton::Left, cx.listener(|this, _: &gpui::MouseDownEvent, _, _| this.nav.keyboard = false))
            .on_action(cx.listener(|this, _: &nav::NavLeave, window, cx| if this.nav_owns(window, cx) { this.nav_leave(window, cx) }))
            .child(
                list(self.sidebar_list.clone(), cx.processor(|this, ix, _window, cx| this.render_sidebar_row(ix, cx)))
                    .flex_1()
                    .min_h_0(),
            );

        let (attention, downloads_open, new_download, activity) = {
            let t = self.transfers.read(cx);
            (t.attention(), t.is_open(), t.new_download(), t.activity())
        };
        let ring_fraction = match activity {
            Activity::Idle => None,
            Activity::Unknown => Some(None),
            Activity::Progress(f) => Some(Some(f)),
        };
        let footer_row = |id: &'static str, icon: &str, label: String| {
            div()
                .id(id)
                .role(gpui::Role::Button)
                .aria_label(label.clone())
                .tab_index(0)
                .focus_visible(move |s| crate::ui::widgets::focus_outline(s, p.focus))
                .h(px(32.))
                .px(px(8.))
                .flex()
                .items_center()
                .gap(px(10.))
                .rounded(p.corner(8.))
                .cursor_pointer()
                .text_size(px(12.5))
                .text_color(p.text_muted)
                .hover(|s| s.bg(p.hover).text_color(p.hover_text))
                .child(icon_el(icon, 15., p.text_faint))
                .child(label)
        };

        let nav = island(p)
            .flex_1()
            .min_h_0()
            .flex()
            .flex_col()
            .overflow_hidden()
            .child(
                div()
                    .px(px(8.))
                    .pt(px(8.))
                    .pb(px(10.))
                    .flex()
                    .gap(px(6.))
                    .child(
                        div()
                            .id("quick-connect")
                            .flex_1()
                            .min_w_0()
                            .role(gpui::Role::Button)
                            .aria_label("Quick connect")
                            .tab_index(0)
                            .focus_visible(move |s| crate::ui::widgets::focus_outline(s, p.focus))
                            .h(px(34.))
                            .px(px(10.))
                            .flex()
                            .items_center()
                            .gap(px(8.))
                            .rounded(p.corner(8.))
                            .bg(p.hover)
                            .border_1()
                            .border_color(p.border)
                            .cursor_pointer()
                            .text_size(px(12.5))
                            .text_color(p.hover_text)
                            .hover(|s| s.bg(p.active))
                            .on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.open_picker(window, cx)))
                            .child(icon_el("search", 14., p.hover_text))
                            .child(div().flex_1().min_w_0().truncate().child("Quick connect"))
                            .when(!compact, |el| el.child(kbd("Ctrl Shift K", p.highlighted()))),
                    )
                    .child(self.render_sidebar_hide_button(p, cx)),
            )
            .child(list.pb(px(8.)));

        div()
            .w(px(width))
            .h_full()
            .flex()
            .flex_col()
            .flex_shrink_0()
            .gap(px(ISLAND_GAP))
            .child(nav)
            .child(
                island(p)
                    .p(px(8.))
                    .flex()
                    .flex_col()
                    .gap(px(2.))
                    .children(self.render_vault_row(compact, p, cx).map(|row| div().pb(px(6.)).child(row)))
                    // Always available; the number is what needs a look.
                    .child(
                        footer_row("downloads", "arrow-down-up", "Transfers".into())
                            .aria_expanded(downloads_open)
                            .when_some(ring_fraction, |el, fraction| {
                                // Around the 15px icon (8px in, centred in the 32px row):
                                // the combined progress of everything running.
                                let label = match fraction {
                                    Some(f) => format!("Transfers, {}% done", (f * 100.).round()),
                                    None => "Transfers, in progress".to_string(),
                                };
                                el.aria_label(label).child(
                                    div().absolute().left(px(8. + 7.5 - 12.)).top(px(16. - 12.)).child(progress_ring(
                                        "downloads-ring",
                                        fraction,
                                        24.,
                                        2.,
                                        p.border,
                                        p.brand,
                                    )),
                                )
                            })
                            // The new-download dot waits until nothing is running.
                            .when(new_download && ring_fraction.is_none(), |el| {
                                el.aria_label("Transfers, new download").child(
                                    // A dot on the icon's top-right corner (the 15px icon
                                    // sits 8px in, 8.5px down), ringed in the sidebar's
                                    // colour so it reads as cut out of the icon.
                                    div()
                                        .absolute()
                                        .left(px(17.))
                                        .top(px(5.5))
                                        .size(px(10.))
                                        .rounded_full()
                                        .bg(p.brand)
                                        .border_2()
                                        .border_color(p.sidebar),
                                )
                            })
                            .relative()
                            .when(downloads_open, |el| el.bg(p.active).text_color(p.active_text))
                            .child(div().flex_1())
                            .when(attention > 0, |el| {
                                el.child(
                                    div()
                                        .min_w(px(18.))
                                        .h(px(18.))
                                        .px(px(5.))
                                        .flex()
                                        .items_center()
                                        .justify_center()
                                        .rounded(p.corner(6.))
                                        .bg(p.brand_soft)
                                        .text_size(px(11.))
                                        .font_weight(FontWeight::SEMIBOLD)
                                        .text_color(p.brand)
                                        .child(attention.to_string()),
                                )
                            })
                            .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                                this.transfers.update(cx, |t, cx| t.toggle(window, cx));
                            })),
                    )
                    .child(
                        div().flex().items_center().gap(px(4.))
                            .child(
                                footer_row("settings", "settings", "Settings".into())
                                    .flex_1().min_w_0()
                                    .child(div().flex_1())
                                    .child(kbd("Ctrl ,", p))
                                    .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                                        this.open_settings(SettingsSection::Appearance, window, cx)
                                    })),
                            )
                            .children(self.render_update_button(p, cx)),
                    ),
            )
            .into_any_element()
    }
}
