//! Quick connect, confirmations and the other dialogs over the window, and how they open
//! and close.

use super::*;

impl Workspace {
    // ---- overlays: picker and forms ----------------------------------------------------

    pub(super) fn open_picker(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        // Replacing an open form would lose what was typed into it.
        if self.modal_open(cx) {
            return;
        }
        let query = cx.new(|cx| InputState::new(window, cx).placeholder("Search hosts and shells…"));
        let sub = cx.subscribe_in(&query, window, |this, query, event: &InputEvent, window, cx| match event {
            // A new query starts again at its best match.
            InputEvent::Change => {
                let found = this.picker_results(&query.read(cx).value());
                if let Some(Overlay::Picker { results, selected, scroll, .. }) = &mut this.overlay {
                    *results = found;
                    *selected = 0;
                    scroll.scroll_to_item(0);
                }
                cx.notify()
            }
            // Ctrl+Enter: another Session on it, like Ctrl+click in the sidebar.
            InputEvent::PressEnter { secondary, .. } => {
                let item = match &this.overlay {
                    Some(Overlay::Picker { results, selected, .. }) if !results.is_empty() => {
                        Some(results[(*selected).min(results.len() - 1)].item.clone())
                    }
                    _ => None,
                };
                if let Some(item) = item {
                    this.pick(item, *secondary, window, cx);
                }
            }
            _ => {}
        });
        self._subscriptions.push(sub);
        query.update(cx, |q, cx| q.focus(window, cx));
        let results = self.picker_results("");
        self.overlay = Some(Overlay::Picker { query, results, selected: 0, scroll: ScrollHandle::new() });
        cx.notify();
    }

    /// Move quick connect's selection one result down (or up), wrapping at the ends, and
    /// scroll it into view.
    pub(super) fn step_picker(&mut self, down: bool, cx: &mut Context<Self>) {
        let Some(Overlay::Picker { results, selected, scroll, .. }) = &mut self.overlay else { return };
        let count = results.len();
        if count == 0 {
            return;
        }
        let current = (*selected).min(count - 1);
        *selected = if down { (current + 1) % count } else { (current + count - 1) % count };
        scroll.scroll_to_item(*selected);
        cx.notify();
    }

    /// Quick connect: every sidebar item, filtered by name, address (with its port), user or
    /// Project.
    pub(super) fn picker_results(&self, query: &str) -> Vec<PickerResult> {
        let needle = query.to_lowercase();
        let found = |hay: String| needle.is_empty() || hay.to_lowercase().contains(&needle);
        self.sidebar_items()
            .into_iter()
            .filter_map(|item| {
                let (icon, color, name, detail) = match &item {
                    SidebarItem::Host { project, host } => {
                        let r = model::resolve(host, project);
                        let dest = destination(&r);
                        // `user@address:port` as shown, so a pasted destination finds its Host.
                        if !found(format!("{} {} {} {}", host.name, dest, project.name, r.port)) {
                            return None;
                        }
                        let via = match r.jump_hosts.as_slice() {
                            [] => String::new(),
                            [one] => format!(" · via {}", one.jump.address),
                            more => format!(" · via {} jump hosts", more.len()),
                        };
                        (r.icon.clone(), Some(project_color(&r.color)), host.name.clone(), format!("{dest}{via} · {}", project.name))
                    }
                    SidebarItem::Shell { project, shell } => {
                        if !found(format!("{} {} local", shell.name, project.as_ref().map(|p| p.name.as_str()).unwrap_or(""))) {
                            return None;
                        }
                        let detail = match project {
                            Some(project) => format!("Local shell · {}", project.name),
                            None => "Local shell".to_string(),
                        };
                        (crate::icons::shell_icon(shell).to_string(), None, shell.name.clone(), detail)
                    }
                };
                let open = self.sessions.of(&item.key()).next().is_some();
                Some(PickerResult { item, icon, color, name: name.into(), detail: detail.into(), open })
            })
            .collect()
    }

    /// Open what quick connect found, and show where it is in the sidebar: its Project
    /// unfolds and its row scrolls into view.
    pub(super) fn pick(&mut self, item: SidebarItem, another: bool, window: &mut Window, cx: &mut Context<Self>) {
        self.overlay = None;
        let key = item.key();
        if let ItemKey::Host { project, .. } | ItemKey::Shell { project: Some(project), .. } = &key {
            self.set_project_collapsed(project, false, cx);
        }
        self.activate_item(item, another, window, cx);
        self.nav.cursor = Some(NavTarget::Item(key));
        self.nav.reveal = true;
    }

    /// The search box of a form's icon picker.
    pub(super) fn new_icon_query(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Entity<InputState> {
        let query = cx.new(|cx| InputState::new(window, cx).placeholder("Search icons"));
        let sub = cx.subscribe_in(&query, window, |_, _, event: &InputEvent, _, cx| {
            if let InputEvent::Change = event {
                cx.notify();
            }
        });
        self._subscriptions.push(sub);
        query
    }

    pub(super) fn confirm_delete_host(&mut self, host_id: String, cx: &mut Context<Self>) {
        if self.prompt_open(cx) {
            return;
        }
        let Some(host) = self.library.host(&host_id) else { return };
        let projects = self.library.projects.iter().filter(|p| p.hosts.contains(&host_id)).count();
        let mut body = format!("{} ({}) is removed from Komando.", host.name, host.address);
        if projects > 1 {
            body.push_str(&format!(" It is in {projects} projects and leaves all of them."));
        }
        body.push_str(" Its open sessions are closed. A saved key or password stays in the vault.");
        self.overlay = Some(Overlay::Confirm {
            title: format!("Delete {}?", host.name),
            body,
            action: ConfirmAction::DeleteHost(host_id),
        });
        cx.notify();
    }

    pub(super) fn confirm_delete_project(&mut self, project_id: String, cx: &mut Context<Self>) {
        if self.prompt_open(cx) {
            return;
        }
        let Some(project) = self.library.project(&project_id) else { return };
        let only_here = project
            .hosts
            .iter()
            .filter(|h| !self.library.projects.iter().any(|p| p.id != project_id && p.hosts.contains(h)))
            .count();
        let body = match only_here {
            0 if project.hosts.is_empty() => "It has no hosts. Open sessions in this project are closed.".to_string(),
            0 => "Its hosts are also in other projects and stay there. Open sessions in this project are closed.".to_string(),
            1 => "Its 1 host is deleted too. Open sessions in this project are closed.".to_string(),
            n => format!("Its {n} hosts are deleted too. Open sessions in this project are closed."),
        };
        self.overlay = Some(Overlay::Confirm { title: format!("Delete {}?", project.name), body, action: ConfirmAction::DeleteProject(project_id) });
        cx.notify();
    }

    pub(super) fn run_confirm(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(Overlay::Confirm { action, .. }) = self.overlay.take() else { return };
        let mut library = self.library.clone();
        match &action {
            ConfirmAction::DeleteHost(id) => library.remove_host(id),
            ConfirmAction::DeleteProject(id) => library.remove_project(id),
            ConfirmAction::Disconnect(id) => {
                self.disconnect_session(*id, window, cx);
                return;
            }
            ConfirmAction::Close(id) => {
                self.end_session(*id, EndRequest::CloseNow, window, cx);
                return;
            }
            ConfirmAction::CloseAll(key) => {
                self.end_item_sessions(key, true, window, cx);
                return;
            }
            ConfirmAction::DeleteAllHistory => {
                self.delete_all_history(cx);
                self.focus_active(window, cx);
                return;
            }
            ConfirmAction::DeleteCredential(id) => {
                self.delete_credential(id, cx);
                self.focus_active(window, cx);
                return;
            }
            ConfirmAction::InstallUpdate => {
                self.install_update(cx);
                self.focus_active(window, cx);
                return;
            }
            ConfirmAction::RestartForUpdate => {
                self.restart_for_update(cx);
                return;
            }
        }
        // Persist before closing Sessions or removing sidebar state: a failed deletion
        // should leave the saved library and its running Sessions intact.
        if let Err(err) = self.save_library(&library) {
            self.show_toast(format!("Could not delete: {err:#}"), cx);
            self.focus_active(window, cx);
            return;
        }
        // A deleted Host's Command History goes with it.
        for gone in self.library.hosts.iter().filter(|h| library.host(&h.id).is_none()) {
            if let Err(err) = crate::history::forget(&self.data_dir, &gone.id) {
                log::warn!("deleting the history of {}: {err:#}", gone.id);
            }
        }
        self.library = library;
        match action {
            // What is shown next comes up without leaving the page the user is on.
            ConfirmAction::DeleteHost(id) => {
                let next = self.discard_sessions(
                    |s| matches!(s.kind(), SessionKind::Ssh { host_id } | SessionKind::Desktop { host_id, .. } if *host_id == id),
                    cx,
                );
                if let Some(next) = next {
                    self.sessions.activate(next);
                }
            }
            ConfirmAction::DeleteProject(id) => {
                if let Some(next) = self.discard_sessions(|s| s.project_id() == Some(id.as_str()), cx) {
                    self.sessions.activate(next);
                }
                self.set_project_collapsed(&id, false, cx);
            }
            ConfirmAction::DeleteAllHistory
            | ConfirmAction::DeleteCredential(_)
            | ConfirmAction::InstallUpdate
            | ConfirmAction::RestartForUpdate
            | ConfirmAction::Disconnect(_)
            | ConfirmAction::Close(_)
            | ConfirmAction::CloseAll(_) => {}
        }
        self.focus_active(window, cx);
        cx.notify();
    }

    /// Close and drop every Session that matches, e.g. those of a deleted Host. Returns what
    /// to show if the shown Session went; the caller shows it.
    pub(super) fn discard_sessions(&mut self, matches: impl Fn(&SessionEntry) -> bool, cx: &mut Context<Self>) -> Option<usize> {
        let removed = self.sessions.remove_where(matches);
        for entry in &removed.entries {
            self.cancel_completion_notification(entry.id());
            if let Some(handle) = entry.ui.window {
                let _ = handle.update(cx, |_, window, _| window.remove_window());
            }
            entry.ui.view.close(cx);
        }
        // The other half of a split one of them was in has its minimap back.
        self.sync_split_views(cx);
        removed.show
    }

    pub(super) fn copy_host_address(&mut self, project_id: &str, host_id: &str, cx: &mut Context<Self>) {
        let Some(r) = self.library.resolve(project_id, host_id) else { return };
        let text = destination(&r);
        cx.write_to_clipboard(gpui::ClipboardItem::new_string(text.clone()));
        self.show_toast(format!("Copied {text}"), cx);
    }

    /// A press on the backdrop beside a dialog. Quick connect, a question and the history
    /// close; a host, project, shell or highlight form stays, since a stray click would throw away what was
    /// typed into it. Escape, Cancel and the close button still close it.
    pub(super) fn backdrop_pressed(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !matches!(self.overlay, Some(Overlay::HostForm(_) | Overlay::ProjectForm(_) | Overlay::ShellForm | Overlay::RuleForm(_) | Overlay::SetForm(_) | Overlay::Tour(_))) {
            self.close_overlay(window, cx);
        }
    }

    /// Escape, Cancel or the close button. A host or project form with changes asks first, and
    /// closes when asked again (Escape twice).
    pub(super) fn cancel_overlay(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let changed = match &self.overlay {
            Some(Overlay::HostForm(form)) => form.changed(cx),
            Some(Overlay::ProjectForm(form)) => form.changed(cx),
            _ => false,
        };
        if let Some(asking) = self.asking_discard()
            && changed
            && !*asking
        {
            *asking = true;
            // Off the form's fields, so typing doesn't change what the question is about.
            window.focus(&self.focus, cx);
            cx.notify();
            return;
        }
        self.close_overlay(window, cx);
    }

    /// "Keep editing": back to the form, as it was.
    pub(super) fn keep_editing(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(asking) = self.asking_discard() {
            *asking = false;
        }
        let name = match &self.overlay {
            Some(Overlay::HostForm(form)) => Some(form.name_field()),
            Some(Overlay::ProjectForm(form)) => Some(form.name_field()),
            _ => None,
        };
        if let Some(name) = name {
            name.update(cx, |field, cx| field.focus(window, cx));
        }
        cx.notify();
    }

    /// Whether the open host or project form asks "Discard your changes?".
    pub(super) fn asking_discard(&mut self) -> Option<&mut bool> {
        match &mut self.overlay {
            Some(Overlay::HostForm(form)) => Some(form.asking_discard_mut()),
            Some(Overlay::ProjectForm(form)) => Some(form.asking_discard_mut()),
            _ => None,
        }
    }

    pub(super) fn close_overlay(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if matches!(self.overlay, Some(Overlay::ShellForm)) {
            self.clear_shell_editor(window, cx);
        }
        if matches!(self.overlay, Some(Overlay::Tour(_))) {
            self.tour_closed(cx);
        }
        self.overlay = None;
        if matches!(self.page, Page::Settings(_)) {
            window.focus(&self.focus, cx);
        } else {
            self.focus_active(window, cx);
        }
        cx.notify();
    }
}

impl Workspace {
    /// `wide`: the window has room for forms laid out in two columns.
    pub(super) fn render_overlay(&self, wide: bool, p: AppPalette, cx: &mut Context<Self>) -> Option<AnyElement> {
        let overlay = self.overlay.as_ref()?;
        let asking_discard = match overlay {
            Overlay::HostForm(form) => form.asking_discard(),
            Overlay::ProjectForm(form) => form.asking_discard(),
            _ => false,
        };
        let body = match overlay {
            Overlay::Picker { query, results, selected, scroll } => {
                let count = results.len();
                let selected = (*selected).min(count.saturating_sub(1));
                let title = "Quick connect".to_string();
                let enter_label = if results.get(selected).is_some_and(|r| r.open) { "Switch to tab" } else { "Open" };
                let mut list = div()
                    .id("picker-list")
                    .role(gpui::Role::ListBox)
                    .aria_label("Hosts and local shells")
                    .flex()
                    .flex_col()
                    .gap(px(2.))
                    .max_h(px(380.))
                    .overflow_y_scroll()
                    .track_scroll(scroll)
                    .p(px(6.));
                if results.is_empty() {
                    // A typo, or nothing to find yet: say which, and how to go on.
                    let hint = if self.library.hosts.is_empty() {
                        "No match. Hosts you add show up here too: add one with the + next to a project."
                    } else {
                        "No host or local shell matches. Search by name, address, port, user or project."
                    };
                    list = list.child(div().p(px(16.)).text_size(px(13.)).text_color(p.text_faint).child(hint));
                }
                for (i, PickerResult { item, icon, color, name, detail, open }) in results.iter().enumerate() {
                    let color = color.unwrap_or_else(|| shell_icon_color(icon, p));
                    let item = item.clone();
                    list = list.child(
                        div()
                            .id(("pick", i))
                            .h(px(44.))
                            .px(px(10.))
                            .flex()
                            .items_center()
                            .gap(px(12.))
                            .rounded(p.corner(8.))
                            .cursor_pointer()
                            .role(gpui::Role::ListBoxOption)
                            .aria_label(if *open { format!("{name}, {detail}, switch to tab") } else { format!("{name}, {detail}") })
                            .aria_selected(i == selected)
                            .aria_position_in_set(i + 1)
                            .aria_size_of_set(count)
                            .when(i == selected, |el| el.bg(p.hover))
                            .hover(|s| s.bg(p.active))
                            .on_click(cx.listener(move |this, event: &ClickEvent, window, cx| {
                                this.pick(item.clone(), event.modifiers().control, window, cx)
                            }))
                            .child(badge(icon, color, 26.))
                            .child(
                                div()
                                    .flex()
                                    .flex_col()
                                    .flex_1()
                                    .min_w_0()
                                    .child(div().text_size(px(13.)).font_weight(FontWeight::MEDIUM).text_color(p.text).child(name.clone()))
                                    .child(div().text_size(px(12.)).text_color(p.text_faint).truncate().child(detail.clone())),
                            )
                            // Like a browser's address bar offering an open tab: picking it shows
                            // the open Session instead of connecting again.
                            .when(*open, |el| el.child(switch_to_tab_badge(i == selected, p))),
                    );
                }
                div()
                    .key_context(PICKER_CONTEXT)
                    .on_action(cx.listener(|this, _: &PickerUp, _, cx| this.step_picker(false, cx)))
                    .on_action(cx.listener(|this, _: &PickerDown, _, cx| this.step_picker(true, cx)))
                    .w(px(520.))
                    .flex()
                    .flex_col()
                    .child(
                        div()
                            .p(px(14.))
                            .pt(px(12.))
                            .flex()
                            .flex_col()
                            .gap(px(10.))
                            .border_b_1()
                            .border_color(p.border)
                            // As tall as the close button beside it, so the field starts below it.
                            .child(div().h(px(30.)).flex().items_center().child(section_label(&title, p)))
                            .child(Input::new(query)),
                    )
                    .child(list)
                    // How to open another Session from here, which isn't obvious.
                    .when(count > 0, |el| {
                        el.child(
                            div()
                                .px(px(14.))
                                .py(px(8.))
                                .flex()
                                .items_center()
                                .gap(px(14.))
                                .border_t_1()
                                .border_color(p.border)
                                .text_size(px(11.5))
                                .text_color(p.text_faint)
                                .child(div().flex().items_center().gap(px(6.)).child(kbd("Enter", p)).child(enter_label))
                                .child(div().flex().items_center().gap(px(6.)).child(kbd("Ctrl Enter", p)).child("New session")),
                        )
                    })
                    .into_any_element()
            }
            Overlay::HostForm(form) => self.render_host_form(form, wide, p, cx),
            Overlay::ProjectForm(form) => self.render_project_form(form, wide, p, cx),
            Overlay::ShellForm => self.render_shell_form(p, cx),
            Overlay::RuleForm(form) => self.render_rule_form(form, p, cx),
            Overlay::SetForm(form) => self.render_set_form(form, p, cx),
            Overlay::History(panel) => self.render_history(panel, p, cx),
            Overlay::Tour(tour) => self.render_tour(tour, p, cx),
            Overlay::RenameSession { name, .. } => self.render_rename_session(name, p, cx),
            Overlay::Confirm { title, body, action } => form_card(
                p,
                title,
                div().text_size(px(13.)).line_height(px(20.)).text_color(p.text_muted).child(body.clone()).into_any_element(),
                vec![
                    button("confirm-cancel", "Cancel", ButtonKind::Ghost, p)
                        .on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.close_overlay(window, cx)))
                        .into_any_element(),
                    button("confirm-ok", match action {
                        ConfirmAction::Disconnect(_) => "Disconnect",
                        ConfirmAction::Close(_) | ConfirmAction::CloseAll(_) => "Close",
                        ConfirmAction::InstallUpdate => "Update and restart",
                        ConfirmAction::RestartForUpdate => "Restart now",
                        _ => "Delete",
                    }, if matches!(action, ConfirmAction::InstallUpdate | ConfirmAction::RestartForUpdate) {
                        // Nothing is lost for good: the restart is what was asked for.
                        ButtonKind::Primary
                    } else {
                        ButtonKind::Danger
                    }, p)
                        .on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.run_confirm(window, cx)))
                        .into_any_element(),
                ],
            ),
        };
        Some(
            div()
                .id("overlay-backdrop")
                .absolute()
                // Clicks stop here: without this the terminal underneath also gets them and
                // takes the keyboard focus away from the dialog's fields.
                .occlude()
                // Below the title bar, so the window can still be moved and closed.
                .top(px(TITLE_BAR_HEIGHT))
                .left_0()
                .right_0()
                .bottom_0()
                .flex()
                .items_start()
                .justify_center()
                .pt(px(56.))
                .pb(px(24.))
                .bg(alpha(0x000000, if p.dark { 0.5 } else { 0.2 }))
                // Close on a press outside the card (not a form's). Not on click: selecting text in a field by
                // dragging past the card's edge ends outside it, and that must not close it.
                .on_mouse_down(MouseButton::Left, cx.listener(|this, _: &gpui::MouseDownEvent, window, cx| this.backdrop_pressed(window, cx)))
                .child(
                    div()
                        .id("overlay-card")
                        .h_auto()
                        // As tall as the window allows; a taller form scrolls its own middle.
                        .max_h_full()
                        .flex()
                        .flex_col()
                        .rounded(p.corner(14.))
                        .bg(p.surface)
                        .border_1()
                        .border_color(p.border)
                        .relative()
                        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                        .child(body)
                        .child(
                            close_x("overlay-close", p)
                                .absolute()
                                .top(px(12.))
                                .right(px(12.))
                                .on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.cancel_overlay(window, cx))),
                        ),
                )
                .when(asking_discard, |el| el.child(discard_question(p, cx)))
                .into_any_element(),
        )
    }
}
