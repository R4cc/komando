//! Opening, connecting, reconnecting, splitting and closing Sessions, and the events
//! their views send.

use super::*;

impl Workspace {
    // ---- sessions ---------------------------------------------------------------------

    /// Items in sidebar order: standalone shells, then each Project's Hosts and shells.
    pub(super) fn sidebar_items(&self) -> Vec<SidebarItem> {
        let mut items: Vec<SidebarItem> = self
            .library
            .standalone_shells()
            .into_iter()
            .chain(self.orphan_shells())
            .map(|shell| SidebarItem::Shell { project: None, shell })
            .collect();
        let hosts = self.library.host_indices();
        for project in &self.library.projects {
            let project = Arc::new(project.clone());
            items.extend(project.hosts.iter().filter_map(|id| hosts.get(id.as_str())).map(|&index| {
                SidebarItem::Host { project: project.clone(), host: Arc::new(self.library.hosts[index].clone()) }
            }));
            items.extend(project.shells.iter().map(|shell| SidebarItem::Shell { project: Some(project.clone()), shell: shell.clone() }));
        }
        items
    }

    /// Standalone shells removed in Settings whose Sessions are still open, so those Sessions
    /// stay reachable until they are closed.
    pub(super) fn orphan_shells(&self) -> Vec<LocalShell> {
        let saved = self.library.standalone_shells();
        let mut orphans: Vec<LocalShell> = vec![];
        for s in &self.sessions {
            if let (ItemKey::Shell { project: None, name }, SessionKind::Local { shell }) = (&s.item(), &s.kind())
                && !saved.iter().any(|saved| saved.name == *name)
                && !orphans.iter().any(|o| o.name == *name)
            {
                orphans.push(LocalShell { name: name.clone(), ..shell.clone() });
            }
        }
        orphans
    }

    /// The standalone shells the sidebar lists: those not hidden in Settings, plus any with
    /// Sessions open.
    pub(super) fn sidebar_shells(&self) -> Vec<LocalShell> {
        self.library
            .standalone_shells()
            .into_iter()
            .chain(self.orphan_shells())
            .filter(|shell| {
                !shell.hidden || self.sessions_of(&ItemKey::Shell { project: None, name: shell.name.clone() }).next().is_some()
            })
            .collect()
    }

    /// How many Session rows show under an item: its listed Sessions, unless folded away.
    pub(super) fn nested_rows(&self, key: &ItemKey, sessions: usize) -> usize {
        if sessions == 0 || self.collapsed_items.contains(key) { 0 } else { self.sessions.listed(key).len() }
    }

    pub(super) fn sessions_of(&self, key: &ItemKey) -> impl Iterator<Item = &SessionEntry> {
        self.sessions.of(key)
    }

    /// Clicking a sidebar item: show its Session (reconnecting it in place if it dropped,
    /// so the earlier output stays above), or connect if it has none. `another` opens an
    /// additional Session for the same item: for a Host always another terminal, even when a
    /// click on it opens a Remote Desktop (a Host has one desktop per protocol, opened from
    /// its menu).
    pub(super) fn activate_item(&mut self, item: SidebarItem, another: bool, window: &mut Window, cx: &mut Context<Self>) {
        let key = item.key();
        if !another {
            // The row is the Host's terminal while its desktops have rows of their own.
            if let Some(id) = self.sessions.item_session(&key) {
                self.activate(id, window, cx);
                let dropped = self.sessions.get(id).is_some_and(|s| !s.is_live());
                if dropped {
                    self.reconnect(id, window, cx);
                }
                return;
            }
        }
        match item {
            SidebarItem::Host { project, host } => match host.opens_with() {
                _ if another => self.open_host(&project.id, &host.id, window, cx),
                Protocol::Ssh => self.open_host(&project.id, &host.id, window, cx),
                protocol => self.open_desktop(&project.id, &host.id, protocol, window, cx),
            },
            SidebarItem::Shell { project, shell } => self.open_local(project.map(|p| p.id.clone()), shell, window, cx),
        }
    }

    pub(super) fn add_session(
        &mut self,
        item: ItemKey,
        kind: SessionKind,
        base_label: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Entity<TerminalView> {
        // A new Session shows under its item even if the item's Sessions were folded away.
        self.collapsed_items.remove(&item);
        let scrollback = self.settings.scrollback_lines;
        let pointer = self.settings.pointer.clone();
        let highlight = self.highlights_item(&item);
        let windows_selection = self.settings.windows_selection;
        let option_as_meta = self.settings.option_as_meta;
        let cursor_shape = self.settings.cursor_shape;
        let cursor_blink = self.settings.cursor_blink;
        let command_rail = self.settings.command_rail;
        // Kept per Host: Local Sessions have their shell's own history.
        let command_history = self.settings.command_history && matches!(kind, SessionKind::Ssh { .. });
        // Not in a split yet; `activate` below tells it if it opened into one.
        let minimap = self.minimap_shown(false);
        let minimap_overlay = self.settings.minimap_placement == crate::model::MinimapPlacement::Over;
        let minimap_delay = std::time::Duration::from_millis(self.settings.minimap_delay_ms.into());
        let clipboard = self.clipboard_for(&item);
        let project_id = match &item {
            ItemKey::Host { project, .. } => Some(project.clone()),
            ItemKey::Shell { project, .. } => project.clone(),
        };
        if let Some(project_id) = &project_id {
            self.set_project_collapsed(project_id, false, cx);
        }
        let mut made = None;
        let id = self.sessions.add(item.clone(), kind, base_label, |id, label| {
            let view = cx.new(|cx| {
                let mut view = TerminalView::new(label.clone(), scrollback, window, cx);
                view.pointer = pointer;
                view.highlight = highlight;
                view.windows_selection = windows_selection;
                view.option_as_meta = option_as_meta;
                view.set_cursor(cursor_shape, cursor_blink);
                view.command_rail = command_rail;
                view.command_history = command_history;
                view.minimap = minimap;
                view.minimap_overlay = minimap_overlay;
                view.minimap_delay = minimap_delay;
                view.clipboard = clipboard;
                view
            });
            let sub = cx.subscribe_in(&view, window, move |this, view, event: &TerminalEvent, window, cx| match event {
                TerminalEvent::Changed => {
                    let status = view.read(cx).status.clone();
                    // Remote Desktops waiting to go through this Session can connect now.
                    let connected = matches!(status, Status::Connected)
                        && this.sessions.get(id).is_some_and(|s| *s.status() != Status::Connected);
                    if connected {
                        cx.defer_in(window, move |this, window, cx| this.ssh_connected(id, window, cx));
                    }
                    this.sessions.set_busy(id, view.read(cx).busy);
                    let changed = this.sessions.set_status(id, status.clone());
                    if let Some(entry) = this.sessions.get_mut(id) {
                        entry.ui.title = view.read(cx).title.clone();
                        // The file browser follows its terminal's connection.
                        if changed && let Some(browser) = entry.ui.browser.clone() {
                            match status {
                                Status::Connected if entry.ui.sftp_open => {
                                    let sink = view.read(cx).sink();
                                    browser.update(cx, |b, cx| b.connect(sink, cx));
                                }
                                Status::Disconnected(_) => browser.update(cx, |b, cx| b.disconnected(cx)),
                                _ => {}
                            }
                        }
                    }
                    if let Some(entry) = this.sessions.get(id)
                        && this.completion_watches.update(id, entry.busy(), entry.status())
                    {
                        let label = entry.label().to_string();
                        this.notify_output_done(id, &label, cx);
                    }
                    cx.notify();
                }
                TerminalEvent::Remember { hop, what } => match what {
                    session::Remembered::User(user) => this.save_user(id, *hop, user.clone(), cx),
                    session::Remembered::Password(password) => {
                        if this.vault.is_some() {
                            this.save_password(id, *hop, password.clone(), cx);
                        } else {
                            this.pending.push(Pending::Save { session: id, hop: *hop, password: password.clone() });
                            let host = this.hop_target(id, *hop).map(|target| this.hop_name(&target));
                            let reason = match host {
                                Some(name) => format!("To remember the password for {name}, Komando saves it in your vault."),
                                None => "To remember this password, Komando saves it in your vault.".to_string(),
                            };
                            this.show_unlock(reason, window, cx);
                        }
                    }
                },
                TerminalEvent::Reconnect => this.reconnect(id, window, cx),
                TerminalEvent::Sized => {
                    // A new Session opens alone and without a file browser: only a pane laid out
                    // like that has its size. Half a split would start the next one at half width.
                    let alone = !this.sessions.shown_split().is_some_and(|s| s.contains(id))
                        && this.sessions.get(id).is_some_and(|s| !s.ui.sftp_open);
                    if alone {
                        this.pane_size = view.read(cx).measured_size().or(this.pane_size);
                    }
                    this.start_waiting(cx);
                }
                TerminalEvent::Entered { line, at } => this.record_command(id, line, *at, cx),
                TerminalEvent::Unhighlight { rule } => this.unhighlight(rule, cx),
                // In a split, only the split's minimap goes.
                TerminalEvent::HideMinimap if this.sessions.in_split(id) && this.settings.minimap_in_split => {
                    this.settings.minimap_in_split = false;
                    this.apply_terminal_settings(cx);
                    this.show_toast("Minimap hidden in split view. Turn it back on in Settings → Terminal.", cx);
                }
                TerminalEvent::HideMinimap => {
                    this.settings.minimap = false;
                    this.apply_terminal_settings(cx);
                    this.show_toast("Minimap hidden. Turn it back on in Settings → Terminal.", cx);
                }
                TerminalEvent::CloseSplit => this.close_split(window, cx),
                TerminalEvent::ClipboardChoice { allow } => this.choose_clipboard(id, *allow, cx),
                TerminalEvent::ForwardFailed { forward, why } => this.forward_failed(id, forward, why, cx),
                TerminalEvent::StartupStopped { waited_for } => this.show_toast_for(
                    format!("Startup commands stopped: \"{}\" didn't appear within a minute.", waited_for.trim()),
                    Duration::from_secs(8),
                    cx,
                ),
                TerminalEvent::EditHost { hop } => match this.hop_target(id, *hop) {
                    Some(HopTarget::Jump(model::RouteJump { owner: model::JumpOwner::Project(project_id, _), .. })) => {
                        this.open_edit_project(project_id, window, cx)
                    }
                    Some(HopTarget::Host { project_id, host_id }) => this.open_edit_host(project_id, host_id, window, cx),
                    // A Host's own Jump Host is edited in that Host's form.
                    Some(HopTarget::Jump(_)) => {
                        let entry = this.sessions.get(id);
                        if let Some((Some(project_id), SessionKind::Ssh { host_id })) = entry.map(|e| (e.project_id().map(str::to_string), e.kind().clone())) {
                            this.open_edit_host(project_id, host_id, window, cx);
                        }
                    }
                    None => {}
                },
            });
            made = Some(view.clone());
            SessionParts::new(SessionView::Terminal(view), label, sub)
        });
        // Into the split waiting for it, if any (see `Sessions::add`).
        self.activate(id, window, cx);
        made.expect("Sessions::add builds the payload")
    }

    pub(super) fn open_local(&mut self, project_id: Option<String>, shell: LocalShell, window: &mut Window, cx: &mut Context<Self>) {
        let item = ItemKey::Shell { project: project_id.clone(), name: shell.name.clone() };
        let kind = SessionKind::Local { shell: shell.clone() };
        let view = self.add_session(item, kind.clone(), &shell.name, window, cx);
        self.start_when_sized(&view, cx);
    }

    /// Open a new Session for a Host. If any Host on the way has a saved Credential, the
    /// Vault is unlocked first (or skipped, and passwords are asked instead).
    pub(super) fn open_host(&mut self, project_id: &str, host_id: &str, window: &mut Window, cx: &mut Context<Self>) {
        if self.host_needs_unlock(project_id, host_id) {
            self.pending.push(Pending::Open { project_id: project_id.into(), host_id: host_id.into() });
            self.show_host_unlock(host_id, window, cx);
            return;
        }
        self.open_host_session(project_id, host_id, window, cx);
    }

    /// Connecting to the Host from this Project would read the locked Vault (see
    /// `ResolvedHost::signs_in_with_vault`). Opening and reconnecting both ask first.
    pub(super) fn host_needs_unlock(&self, project_id: &str, host_id: &str) -> bool {
        self.vault.is_none() && self.library.resolve(project_id, host_id).is_some_and(|host| host.signs_in_with_vault())
    }

    /// Ask for the Master Password to connect to a Host, once its job is in `pending`.
    pub(super) fn show_host_unlock(&mut self, host_id: &str, window: &mut Window, cx: &mut Context<Self>) {
        let name = self.library.host(host_id).map(|h| h.name.clone()).unwrap_or_else(|| "This host".into());
        self.show_unlock(format!("{name} signs in with a password or key saved in your vault."), window, cx);
    }

    pub(super) fn open_host_session(&mut self, project_id: &str, host_id: &str, window: &mut Window, cx: &mut Context<Self>) {
        let Some(resolved) = self.library.resolve(project_id, host_id) else { return };
        let item = ItemKey::Host { project: project_id.to_string(), host: host_id.to_string() };
        let kind = SessionKind::Ssh { host_id: host_id.to_string() };
        let view = self.add_session(item, kind.clone(), &resolved.name, window, cx);
        self.start_when_sized(&view, cx);
    }

    /// Start a new Session's backend at the terminal pane's size: the size of the pane last
    /// laid out, or, before any was, once this one is. A shell started at a guessed size and
    /// resized as soon as the pane appears repaints, and Windows' pseudo-console leaves blank
    /// lines above the prompt when it does.
    pub(super) fn start_when_sized(&mut self, view: &Entity<TerminalView>, cx: &mut Context<Self>) {
        let Some(id) = self.sessions.iter().find(|s| s.ui.view.terminal() == Some(view)).map(|s| s.id()) else { return };
        // Opened into a split: it gets half the room, which the last pane laid out may not
        // have had. Wait for its own pane.
        let in_split = self.sessions.in_split(id);
        let Some(entry) = self.sessions.get_mut(id) else { return };
        let size = view.read(cx).measured_size().or(self.pane_size.filter(|_| !in_split));
        match size {
            Some(size) => {
                view.update(cx, |v, _| v.assume_size(size));
                let (kind, project_id) = (entry.kind().clone(), entry.project_id().map(str::to_string));
                self.start_backend(view, &kind, project_id.as_deref(), cx);
            }
            None => {
                entry.ui.waiting_for_size = true;
                // In case no pane gets laid out (the window is hidden): start at a guess.
                cx.spawn(async move |this, cx| {
                    cx.background_executor().timer(Duration::from_millis(500)).await;
                    let _ = this.update(cx, |this, cx| this.start_waiting(cx));
                })
                .detach();
            }
        }
    }

    /// Start the Sessions that were waiting for a terminal pane to be laid out.
    pub(super) fn start_waiting(&mut self, cx: &mut Context<Self>) {
        let waiting: Vec<_> = self
            .sessions
            .iter_mut()
            .filter_map(|s| {
                let view = s.ui.view.terminal()?.clone();
                std::mem::take(&mut s.ui.waiting_for_size).then(|| (view, s.kind().clone(), s.project_id().map(str::to_string)))
            })
            .collect();
        for (view, kind, project_id) in waiting {
            if let Some(size) = self.pane_size {
                view.update(cx, |v, _| v.assume_size(size));
            }
            self.start_backend(&view, &kind, project_id.as_deref(), cx);
        }
        self.lock_if_used(cx);
    }

    pub(super) fn start_backend(&mut self, view: &Entity<TerminalView>, kind: &SessionKind, project_id: Option<&str>, cx: &mut Context<Self>) {
        let (term, proxy, size) = {
            let v = view.read(cx);
            (v.term(), v.proxy(), v.grid_size())
        };
        let sink = match kind {
            SessionKind::Local { shell } => {
                view.update(cx, |v, _| v.begin_connect(None, vec![], false));
                match session::local::spawn(shell, size, term, proxy.clone()) {
                    Ok(sink) => {
                        proxy.send(session::SessionEvent::Status(Status::Connected));
                        Some(sink)
                    }
                    Err(err) => {
                        proxy.send(session::SessionEvent::Status(Status::Disconnected(format!("{err:#}"))));
                        None
                    }
                }
            }
            SessionKind::Ssh { host_id } => {
                let Some(host) = project_id.and_then(|p| self.library.resolve(p, host_id)) else {
                    view.update(cx, |v, _| v.begin_connect(None, vec![], false));
                    proxy.send(session::SessionEvent::Status(Status::Disconnected("This host is no longer saved.".into())));
                    return;
                };
                let cards = (!host.jump_hosts.is_empty()).then(|| {
                    host.jump_hosts.iter().map(|j| jump_card(&j.jump, &host.color)).chain([route_card(&host)]).collect()
                });
                view.update(cx, |v, _| v.begin_connect(cards, host.startup_commands.clone(), true));
                let secret = |credential: Option<&str>| {
                    credential.and_then(|id| self.vault.as_ref()?.get(id)).map(|c| c.secret.clone())
                };
                let mut hops: Vec<ssh::Hop> = host
                    .jump_hosts
                    .iter()
                    .map(|j| ssh::Hop {
                        secret: secret(j.jump.credential.as_deref()),
                        name: j.jump.address.clone(),
                        address: j.jump.address.clone(),
                        port: j.jump.port,
                        user: j.jump.user.clone(),
                    })
                    .collect();
                let history_on = crate::history::address_label(&host.address, host.port);
                if let Some(entry) = self.sessions.iter_mut().find(|s| s.ui.view.terminal() == Some(view)) {
                    entry.ui.history_on = Some(history_on);
                }
                let secret = |credential: Option<&str>| {
                    credential.and_then(|id| self.vault.as_ref()?.get(id)).map(|c| c.secret.clone())
                };
                hops.push(ssh::Hop {
                    secret: secret(host.credential.as_deref()),
                    name: host.name,
                    address: host.address,
                    port: host.port,
                    user: host.user,
                });
                let target = ssh::Target {
                    hops,
                    known_hosts: self.data_dir.join("known_hosts"),
                    forwards: host.port_forwards,
                    agent: session::agent::Agent::from_setting(&self.settings.ssh_agent),
                    host_agent: host.agent,
                    forward_agent: host.forward_agent,
                };
                Some(ssh::spawn(target, size, term, proxy))
            }
            SessionKind::Desktop { .. } => return,
        };
        view.update(cx, |v, cx| {
            if let Some(sink) = sink {
                v.attach(sink);
            }
            v.set_status(Status::Connecting, cx);
        });
    }

    /// Start a new connection in the same terminal: the earlier output stays above. Like
    /// opening, it unlocks the Vault first when the Host signs in with a saved Credential.
    pub(super) fn reconnect(&mut self, id: usize, window: &mut Window, cx: &mut Context<Self>) {
        self.reconnect_with(id, true, window, cx);
    }

    /// `use_vault` false: the Vault stayed locked, so reconnect and ask for passwords instead.
    pub(super) fn reconnect_with(&mut self, id: usize, use_vault: bool, window: &mut Window, cx: &mut Context<Self>) {
        self.cancel_completion_notification(id);
        let Some(entry) = self.sessions.get(id) else { return };
        let (view, kind, project_id) = (entry.ui.view.clone(), entry.kind().clone(), entry.project_id().map(str::to_string));
        let SessionView::Terminal(view) = view else {
            // Remote Desktops ask for the Vault themselves.
            self.connect_desktop(id, window, cx);
            self.focus_active(window, cx);
            return;
        };
        if let (true, SessionKind::Ssh { host_id }, Some(project_id)) = (use_vault, &kind, project_id.as_deref()) {
            if self.host_needs_unlock(project_id, host_id) {
                // Clicked again while the prompt is up: reconnect once, not twice.
                if !self.pending.iter().any(|job| matches!(job, Pending::Reconnect { session } if *session == id)) {
                    self.pending.push(Pending::Reconnect { session: id });
                }
                self.show_host_unlock(host_id, window, cx);
                return;
            }
        }
        view.update(cx, |v, _| v.close());
        self.start_backend(&view, &kind, project_id.as_deref(), cx);
        self.focus_active(window, cx);
    }

    /// What one hop of a Session's way is: one of its Jump Hosts, or the Host at its end.
    pub(super) fn hop_target(&self, session_id: usize, hop: usize) -> Option<HopTarget> {
        let entry = self.sessions.get(session_id)?;
        let SessionKind::Ssh { host_id } = entry.kind() else { return None };
        let project_id = entry.project_id()?;
        let host = self.library.resolve(project_id, host_id)?;
        match host.jump_hosts.get(hop) {
            Some(jump) => Some(HopTarget::Jump(jump.clone())),
            None if hop == host.jump_hosts.len() => Some(HopTarget::Host { project_id: project_id.into(), host_id: host_id.clone() }),
            None => None,
        }
    }

    /// A hop's name as the user knows it: the Host's name, or the Jump Host's address.
    pub(super) fn hop_name(&self, target: &HopTarget) -> String {
        match target {
            HopTarget::Jump(jump) => jump.jump.address.clone(),
            HopTarget::Host { host_id, .. } => self.library.host(host_id).map(|h| h.name.clone()).unwrap_or_default(),
        }
    }

    pub(super) fn activate(&mut self, id: usize, window: &mut Window, cx: &mut Context<Self>) {
        match self.sessions.activate(id) {
            sessions::Shown::Here => {}
            // A desktop in a window of its own: that window comes to the front.
            sessions::Shown::InOwnWindow => {
                if let Some(handle) = self.sessions.get(id).and_then(|s| s.ui.window) {
                    let _ = handle.update(cx, |_, window, _| window.activate_window());
                }
                cx.notify();
                return;
            }
            sessions::Shown::Gone => return,
        }
        if matches!(self.page, Page::Settings(_)) {
            self.page = Page::Sessions;
        }
        // It may have just gone into a split (see `Sessions::add`).
        self.sync_split_views(cx);
        self.focus_active(window, cx);
        cx.notify();
    }

    /// Browse files from the sidebar: show the Session with its file browser open.
    pub(super) fn show_files(&mut self, id: usize, window: &mut Window, cx: &mut Context<Self>) {
        self.activate(id, window, cx);
        if self.sessions.get(id).is_some_and(|s| !s.ui.sftp_open) {
            self.toggle_sftp(id, window, cx);
        }
    }

    /// Show or hide the SFTP file browser beside an SSH Session's terminal.
    pub(super) fn toggle_sftp(&mut self, id: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(entry) = self.sessions.get_mut(id) else { return };
        let SessionKind::Ssh { host_id } = entry.kind().clone() else { return };
        if entry.ui.sftp_open {
            entry.ui.sftp_open = false;
            self.focus_active(window, cx);
            cx.notify();
            return;
        }
        // Not connected (yet): nothing to browse. The title bar's button says so.
        if !matches!(entry.status(), Status::Connected) {
            return;
        }
        if entry.ui.browser.is_none() {
            let entry = &*entry;
            let (label, target) = match entry.project_id().and_then(|p| self.library.resolve(p, &host_id)) {
                Some(r) => {
                    let user = r.user.clone().map(|u| format!("{u}@")).unwrap_or_default();
                    (r.name.clone(), format!("{user}{}:{}", r.address, r.port))
                }
                None => (entry.label().to_string(), String::new()),
            };
            let (favorites, transfers, show_hidden) = (self.favorites.clone(), self.transfers.clone(), self.settings.sftp_show_hidden);
            let extras = self.sftp_menu_extras();
            let browser = cx.new(|cx| {
                let mut browser = FileBrowser::new(host_id.clone(), label.into(), target.into(), favorites, transfers, show_hidden, window, cx);
                browser.menu_extras = extras;
                browser
            });
            let sub = cx.subscribe_in(&browser, window, move |this, browser, event: &BrowserEvent, window, cx| match event {
                BrowserEvent::Close => this.toggle_sftp(id, window, cx),
                BrowserEvent::ShowHidden(show) => {
                    this.settings.sftp_show_hidden = *show;
                    this.save_settings(cx);
                }
                BrowserEvent::Retry => {
                    let sink = this.sessions.get(id).and_then(|s| s.ui.view.terminal()?.read(cx).sink());
                    browser.update(cx, |b, cx| b.connect(sink, cx));
                }
                BrowserEvent::JumpTo(dir) => {
                    let Some(view) = this.sessions.get(id).and_then(|s| s.ui.view.terminal().cloned()) else { return };
                    let line = file_browser::cd_command(dir);
                    if view.update(cx, |v, cx| v.run_line(&line, cx)) {
                        view.update(cx, |v, cx| v.focus_prompt(window, cx));
                    } else {
                        browser.update(cx, |b, cx| b.jump_refused(cx));
                    }
                }
            });
            let observer = cx.observe(&browser, move |this, browser, cx| {
                let busy = browser.read(cx).indicator();
                let Some(entry) = this.sessions.get_mut(id) else { return };
                if entry.ui.sftp_busy != busy {
                    entry.ui.sftp_busy = busy;
                    busy.schedule_redraws(cx);
                    cx.notify();
                }
            });
            let Some(entry) = self.sessions.get_mut(id) else { return };
            entry.ui.browser = Some(browser);
            entry.ui._browser_subscription = Some(sub);
            entry.ui._browser_observer = Some(observer);
        }
        let Some(entry) = self.sessions.get_mut(id) else { return };
        entry.ui.sftp_open = true;
        let browser = entry.ui.browser.clone().expect("browser");
        if matches!(entry.status(), Status::Connected) && !browser.read(cx).is_connected() {
            let sink = entry.ui.view.terminal().and_then(|v| v.read(cx).sink());
            browser.update(cx, |b, cx| b.connect(sink, cx));
        }
        let handle = browser.read(cx).focus_handle(cx);
        window.focus(&handle, cx);
        cx.notify();
    }

    /// Drag the divider between terminal and file browser: the browser takes what is right
    /// of the pointer, up to the right edge of its pane (`pane`).
    pub(super) fn resize_sftp(&mut self, x: f32, pane: gpui::Bounds<gpui::Pixels>, cx: &mut Context<Self>) {
        // The pointer is on the middle of the gap left of the browser card.
        let right_edge = f32::from(pane.right()) - ISLAND_GAP / 2.;
        let max = (f32::from(pane.size.width) - 300.).max(320.);
        let width = (right_edge - x).clamp(320., max);
        if (width - self.settings.sftp_width).abs() >= 1. {
            self.settings.sftp_width = width;
            cx.notify();
        }
    }

    pub(super) fn sidebar_width(&self, window: &Window) -> f32 {
        sidebar_width(self.settings.sidebar_width, f32::from(window.bounds().size.width))
    }

    pub(super) fn finish_sidebar_resize(&mut self, cx: &mut Context<Self>) {
        if self.sidebar_resizing {
            self.sidebar_resizing = false;
            self.save_settings(cx);
            cx.notify();
        }
    }

    pub(super) fn render_sidebar_divider(&self, p: AppPalette, cx: &mut Context<Self>) -> AnyElement {
        let workspace = cx.entity().downgrade();
        div()
            .id("sidebar-divider")
            .group("sidebar-divider")
            .w(px(ISLAND_GAP))
            .h_full()
            .flex_shrink_0()
            .flex()
            .justify_center()
            .py(px(14.))
            .cursor_col_resize()
            .tooltip(|window, cx| Tooltip::new("Drag to resize sidebar").build(window, cx))
            .on_drag(SidebarDivider, move |_, _, _, cx| {
                let _ = workspace.update(cx, |this, cx| {
                    this.sidebar_resizing = true;
                    this.drop_target = None;
                    cx.notify();
                });
                let workspace = workspace.clone();
                cx.new(|cx| {
                    // Release covers cancellation and letting go outside the window too.
                    cx.on_release(move |_, cx| {
                        let _ = workspace.update(cx, |this, cx| this.finish_sidebar_resize(cx));
                    }).detach();
                    SidebarDivider
                })
            })
            .child(
                div()
                    .w(px(2.))
                    .h_full()
                    .rounded_full()
                    .when(self.sidebar_resizing, |el| el.bg(p.brand))
                    .group_hover("sidebar-divider", |s| s.bg(p.brand.opacity(0.6))),
            )
            .into_any_element()
    }

    pub(super) fn sftp_menu_extras(&self) -> MenuExtras {
        MenuExtras {
            rename: self.settings.sftp_menu_rename,
            copy_to: self.settings.sftp_menu_copy_to,
            move_to: self.settings.sftp_menu_move_to,
        }
    }

    /// Give the keyboard to the active Session. The master password popover doesn't hold on
    /// to it: switching Sessions while it's open types into the terminal, not the popover.
    pub(super) fn focus_active(&self, window: &mut Window, cx: &mut Context<Self>) {
        // A dialog the sidebar's keys opened has closed: back to the sidebar.
        if self.nav.after_dialog.take() && !self.modal_open(cx) {
            window.focus(&self.nav.focus, cx);
            return;
        }
        if let Some(entry) = self.active_entry() {
            // The open question's field if it has one, else the terminal or desktop.
            match &entry.ui.view {
                SessionView::Terminal(view) => view.update(cx, |view, cx| view.focus_prompt(window, cx)),
                SessionView::Desktop(view) => view.update(cx, |view, cx| view.focus_prompt(window, cx)),
            }
        } else {
            window.focus(&self.focus, cx);
        }
    }

    pub(super) fn toggle_sidebar(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        // Keep the Vault's pending question visible until it is answered or dismissed.
        if self.modal_open(cx) || self.unlock.is_some() {
            return;
        }
        self.sidebar_visible = !self.sidebar_visible;
        // Pinning a floated-out sidebar keeps it where it is.
        self.sidebar_peek = false;
        self.sidebar_peek_timer = None;
        if !self.sidebar_visible {
            self.transfers.update(cx, |transfers, cx| transfers.close(cx));
        }
        if self.page == Page::Sessions {
            self.focus_active(window, cx);
        } else {
            window.focus(&self.focus, cx);
        }
        cx.notify();
    }

    /// Expand sidebar on hover: whether the hidden sidebar may float out over the Sessions.
    pub(super) fn sidebar_can_peek(&self) -> bool {
        self.settings.sidebar_auto_hide && !self.sidebar_visible && self.page == Page::Sessions && self.unlock.is_none()
    }

    /// Float the sidebar out at once (`true`), or slide it back once the pointer has been off
    /// it for a moment, so brushing past its edge doesn't close it. A pending change the
    /// other way is cancelled; one the same way keeps its timer.
    pub(super) fn peek_sidebar_later(&mut self, open: bool, window: &mut Window, cx: &mut Context<Self>) {
        if self.sidebar_peek == open {
            self.sidebar_peek_timer = None;
            return;
        }
        if open {
            self.set_sidebar_peek(true, window, cx);
            return;
        }
        if self.sidebar_peek_timer.as_ref().is_some_and(|(target, _)| *target == open) {
            return;
        }
        let task = cx.spawn_in(window, async move |this, cx| {
            cx.background_executor().timer(Duration::from_millis(SIDEBAR_PEEK_CLOSE_MS)).await;
            let _ = this.update_in(cx, |this, window, cx| this.set_sidebar_peek(false, window, cx));
        });
        self.sidebar_peek_timer = Some((open, task));
    }

    /// Float the sidebar out or slide it back now.
    pub(super) fn set_sidebar_peek(&mut self, open: bool, window: &mut Window, cx: &mut Context<Self>) {
        self.sidebar_peek_timer = None;
        if open == self.sidebar_peek || (open && !self.sidebar_can_peek()) {
            return;
        }
        self.sidebar_peek = open;
        if open {
            self.sidebar_peek_closing = None;
            self.watch_sidebar_peek(window, cx);
        } else {
            self.sidebar_peek_watch = None;
            self.transfers.update(cx, |transfers, cx| transfers.close(cx));
            self.sidebar_peek_closing = Some(cx.spawn(async move |this, cx| {
                cx.background_executor().timer(Duration::from_millis(SIDEBAR_PEEK_OUT_MS)).await;
                let _ = this.update(cx, |this, cx| {
                    this.sidebar_peek_closing = None;
                    cx.notify();
                });
            }));
        }
        cx.notify();
    }

    /// While the sidebar is out, follow the pointer outside the window too (gpui stops
    /// reporting it there). Out through the left edge, the sidebar stays until the pointer is
    /// `SIDEBAR_PEEK_OUTSIDE` past it; out any other way, it slides back as if left inside.
    pub(super) fn watch_sidebar_peek(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if crate::platform::pointer_in_window(window).is_none() {
            return;
        }
        self.sidebar_peek_watch = Some(cx.spawn_in(window, async move |this, cx| {
            // Whether the pointer left through the left edge, once it is outside.
            let mut left_edge: Option<bool> = None;
            loop {
                cx.background_executor().timer(Duration::from_millis(16)).await;
                let alive = this.update_in(cx, |this, window, cx| {
                    if !this.sidebar_peek {
                        return false;
                    }
                    let Some((x, y)) = crate::platform::pointer_in_window(window) else { return false };
                    let size = window.viewport_size();
                    let (w, h) = (f32::from(size.width), f32::from(size.height));
                    if x >= 0. && x < w && y >= 0. && y < h {
                        left_edge = None;
                        return true;
                    }
                    let through_left = *left_edge.get_or_insert(x < 0. && y >= 0. && y < h);
                    if !through_left {
                        this.sidebar_peek_pointer(f32::MAX, window, cx);
                    } else if x < -SIDEBAR_PEEK_OUTSIDE || y < 0. || y >= h {
                        if this.sidebar_peek_may_close(cx) {
                            this.set_sidebar_peek(false, window, cx);
                        }
                    } else {
                        this.sidebar_peek_timer = None;
                    }
                    true
                });
                if !alive.unwrap_or(false) {
                    break;
                }
            }
        }));
    }

    /// Whether nothing opened from the floated-out sidebar is still in use.
    pub(super) fn sidebar_peek_may_close(&self, cx: &App) -> bool {
        !cx.has_active_drag() && !self.transfers.read(cx).is_open() && !self.modal_open(cx)
    }

    /// The pointer moved over the Sessions while the sidebar is floated out: slide it back
    /// once the pointer is off it, unless something opened from it is still in use.
    pub(super) fn sidebar_peek_pointer(&mut self, x: f32, window: &mut Window, cx: &mut Context<Self>) {
        if !self.sidebar_peek {
            return;
        }
        let right = self.sidebar_width(window) + ISLAND_GAP * 2.;
        if x <= right {
            self.peek_sidebar_later(true, window, cx);
        } else if self.sidebar_peek_may_close(cx) {
            self.peek_sidebar_later(false, window, cx);
        }
    }

    /// The thin strip along the left edge that floats the hidden sidebar out, and the
    /// sidebar itself while it is out. Laid over the Sessions; nothing moves underneath.
    pub(super) fn render_sidebar_peek(&mut self, p: AppPalette, window: &Window, cx: &mut Context<Self>) -> Option<AnyElement> {
        if !self.sidebar_can_peek() {
            self.sidebar_peek = false;
            self.sidebar_peek_timer = None;
            self.sidebar_peek_closing = None;
            self.sidebar_peek_watch = None;
            return None;
        }
        let edge = div()
            .id("sidebar-peek-edge")
            .absolute()
            .left_0()
            .top_0()
            .bottom_0()
            .w(px(SIDEBAR_PEEK_EDGE))
            .on_hover(cx.listener(|this, hovered: &bool, window, cx| {
                if *hovered {
                    this.peek_sidebar_later(true, window, cx);
                }
            }));
        if !self.sidebar_peek && self.sidebar_peek_closing.is_none() {
            return Some(edge.into_any_element());
        }
        let width = self.sidebar_width(window);
        // Only the panels cover the terminal. An opaque canvas on the padded wrapper
        // paints a solid strip around them (black in dark themes with window blur).
        let mut overlay_palette = p;
        overlay_palette.sidebar.a = 1.;
        let panels = self.render_sidebar(overlay_palette, window, cx);
        let sidebar = div()
                .id("sidebar-peek")
                .absolute()
                .left_0()
                .top_0()
                .bottom_0()
                .w(px(width + ISLAND_GAP * 2.))
                .pl(px(ISLAND_GAP))
                .pr(px(ISLAND_GAP))
                .pb(px(ISLAND_GAP))
                .flex()
                .child(panels);
        let full = width + ISLAND_GAP * 2.;
        if !self.sidebar_peek {
            // Sliding back: the edge strip under it brings it straight out again.
            let closing = sidebar.with_animation(
                "sidebar-peek-out",
                Animation::new(Duration::from_millis(SIDEBAR_PEEK_OUT_MS)).with_easing(crate::ui::easing::ease),
                move |el, t| el.left(px(-full * t)),
            );
            return Some(div().absolute().inset_0().child(closing).child(edge).into_any_element());
        }
        Some(
            sidebar
                .occlude()
                .on_hover(cx.listener(|this, hovered: &bool, window, cx| {
                    if *hovered {
                        this.peek_sidebar_later(true, window, cx);
                    }
                }))
                .with_animation(
                    "sidebar-peek-in",
                    Animation::new(Duration::from_millis(SIDEBAR_PEEK_IN_MS)).with_easing(crate::ui::easing::spring),
                    move |el, t| el.left(px(-full * (1. - t))),
                )
                .into_any_element(),
        )
    }

    pub(super) fn active_entry(&self) -> Option<&SessionEntry> {
        self.sessions.active_entry()
    }

    /// Whether the active Session has a question open (a password, a host key…). It is a
    /// modal: nothing else in the window can be used, and no other dialog opens, until it is
    /// answered. The master password popover is not one: it leaves the window usable.
    pub(super) fn prompt_open(&self, cx: &App) -> bool {
        self.active_entry().is_some_and(|entry| entry.ui.view.has_prompt(cx))
    }

    /// Whether the keyboard is in the master password popover.
    pub(super) fn unlock_focused(&self, window: &Window, cx: &App) -> bool {
        self.unlock.as_ref().is_some_and(|state| state.focus.contains_focused(window, cx))
    }

    /// Whether a dialog is up, so the window's shortcuts and other dialogs must wait.
    pub(super) fn modal_open(&self, cx: &App) -> bool {
        self.overlay.is_some() || self.prompt_open(cx)
    }

    /// End a Session as the user asked: close or disconnect it now, or ask first (see
    /// `Sessions::ending`). With "Keep sessions after disconnecting" on, closing a live
    /// terminal only disconnects it, keeping its output until closed again; an unexpected
    /// disconnect keeps either kind of Session.
    pub(super) fn end_session(&mut self, id: usize, request: EndRequest, window: &mut Window, cx: &mut Context<Self>) {
        // A dialog is up (the question, perhaps): nothing closes behind it.
        if request != EndRequest::CloseNow && self.modal_open(cx) {
            return;
        }
        let ending = self.sessions.ending(id, request, self.settings.keep_disconnected_sessions);
        let question = match ending {
            Ending::Disconnect => return self.disconnect_session(id, window, cx),
            Ending::Remove => return self.remove_session(id, window, cx),
            Ending::Nothing => return,
            Ending::Ask(question) => question,
        };
        let Some(name) = self.sessions.get(id).map(|entry| self.sessions.display_name(entry)) else { return };
        self.overlay = Some(match question {
            Question::CloseWhilePrinting => Overlay::Confirm {
                title: format!("Close {name}?"),
                body: format!("{name} is still printing output, so a command may be running. Closing the session ends it."),
                action: ConfirmAction::Close(id),
            },
            Question::DisconnectWhilePrinting => Overlay::Confirm {
                title: format!("Disconnect {name}?"),
                body: format!("{name} is still printing output, so a command may be running. Disconnecting ends it; the output so far stays until you close the session."),
                action: ConfirmAction::Disconnect(id),
            },
            Question::CloseDesktopByKey => Overlay::Confirm {
                title: format!("Close {name}?"),
                body: "Ctrl+Shift+W is Komando's shortcut for closing a session, so programs on the remote desktop don't receive it. Closing ends the remote desktop session.".into(),
                action: ConfirmAction::Close(id),
            },
        });
        cx.notify();
    }

    /// Closing the window into the notification area disconnects every Session, unless
    /// that's switched off in Settings. Each keeps its output, to reconnect on return.
    pub(super) fn watch_close_to_tray(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        #[cfg(windows)]
        if let Some(mut closes) = self.tray.as_ref().and_then(|tray| tray.closes()) {
            use futures::StreamExt as _;
            cx.spawn_in(window, async move |this, cx| {
                while closes.next().await.is_some() {
                    let closed = this.update_in(cx, |this, window, cx| {
                        if this.settings.disconnect_on_close_to_tray {
                            this.disconnect_all_sessions(window, cx);
                        }
                    });
                    if closed.is_err() {
                        break;
                    }
                }
            })
            .detach();
        }
        #[cfg(not(windows))]
        let _ = (window, cx);
    }

    /// Disconnect every live Session, without asking about ones still printing output.
    #[cfg_attr(not(windows), allow(dead_code))]
    pub(super) fn disconnect_all_sessions(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let live: Vec<usize> = self.sessions.iter().filter(|s| s.is_live()).map(|s| s.id()).collect();
        for id in live {
            self.disconnect_session(id, window, cx);
        }
    }

    /// End a Session's connection but keep it, with its output, to reconnect or close later.
    pub(super) fn disconnect_session(&mut self, id: usize, window: &mut Window, cx: &mut Context<Self>) {
        self.cancel_completion_notification(id);
        let Some(entry) = self.sessions.get(id) else { return };
        if !entry.is_live() {
            return;
        }
        match entry.ui.view.clone() {
            SessionView::Terminal(view) => view.update(cx, |v, cx| {
                v.close();
                v.set_status(Status::Disconnected("Session closed.".into()), cx);
            }),
            SessionView::Desktop(view) => view.update(cx, |v, cx| v.end("Session closed.", cx)),
        }
        self.focus_active(window, cx);
        cx.notify();
    }

    /// End a Session (if still connected) and take it out of the sidebar.
    pub(super) fn remove_session(&mut self, id: usize, window: &mut Window, cx: &mut Context<Self>) {
        self.cancel_completion_notification(id);
        if !self.sessions.contains(id) {
            return;
        }
        // The other half of a split fills the area; otherwise stay with the item and show
        // another of its Sessions, if it has one.
        if let Some(next) = self.discard_sessions(|s| s.id() == id, cx) {
            self.activate(next, window, cx);
        }
        self.focus_active(window, cx);
        cx.notify();
    }

    /// Close all of an item's Sessions, live or dropped, asking once first if any is still
    /// printing output, unless `confirmed`.
    pub(super) fn end_item_sessions(&mut self, key: &ItemKey, confirmed: bool, window: &mut Window, cx: &mut Context<Self>) {
        if !confirmed && self.modal_open(cx) {
            return;
        }
        match self.sessions.ending_all(key, confirmed) {
            EndingAll::Remove(ids) => ids.into_iter().for_each(|id| self.remove_session(id, window, cx)),
            EndingAll::Ask { printing, count } => {
                let name = self.item_for_key(key).map(|item| item.label().to_string()).unwrap_or_default();
                let printing = if printing == 1 { "One of them is".to_string() } else { format!("{printing} of them are") };
                self.overlay = Some(Overlay::Confirm {
                    title: if count == 1 { format!("Close the session of {name}?") } else { format!("Close all {count} sessions of {name}?") },
                    body: format!("{printing} still printing output, so a command may be running. Closing the sessions ends it."),
                    action: ConfirmAction::CloseAll(key.clone()),
                });
                cx.notify();
            }
        }
    }

    /// Rename session…: a name of its own for a Session, shown in the sidebar and title bar.
    pub(super) fn open_rename_session(&mut self, id: usize, window: &mut Window, cx: &mut Context<Self>) {
        if self.modal_open(cx) {
            return;
        }
        let Some(entry) = self.sessions.get(id) else { return };
        let current = entry.name().unwrap_or(entry.label()).to_string();
        let name = cx.new(|cx| InputState::new(window, cx).default_value(current));
        let subscription = cx.subscribe_in(&name, window, move |this, _, event: &InputEvent, window, cx| {
            if matches!(event, InputEvent::PressEnter { .. }) {
                this.save_session_name(window, cx);
            }
        });
        name.update(cx, |f, cx| f.focus(window, cx));
        self.overlay = Some(Overlay::RenameSession { id, name, _subscription: subscription });
        cx.notify();
    }

    pub(super) fn save_session_name(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(Overlay::RenameSession { id, name, .. }) = &self.overlay else { return };
        let (id, value) = (*id, name.read(cx).value().trim().to_string());
        if let Some(entry) = self.sessions.get(id) {
            // The field starts with the host's name; keeping it means no name of its own.
            let name: Option<SharedString> = (!value.is_empty() && value != entry.label().as_ref()).then(|| value.into());
            let view = entry.ui.view.clone();
            self.sessions.set_name(id, name.clone());
            // A desktop in a window of its own shows it in that window's title.
            if let SessionView::Desktop(view) = &view {
                view.update(cx, |view, cx| {
                    view.session_name = name;
                    cx.notify();
                });
            }
        }
        self.close_overlay(window, cx);
    }

    pub(super) fn render_rename_session(&self, name: &Entity<InputState>, p: AppPalette, cx: &mut Context<Self>) -> AnyElement {
        form_card(
            p,
            "Rename session",
            div()
                .flex()
                .flex_col()
                .gap(px(8.))
                .child(Input::new(name))
                .child(
                    div()
                        .text_size(px(12.))
                        .line_height(px(17.))
                        .text_color(p.text_muted)
                        .child("Shown in the sidebar and the title bar until the session is closed. Leave it empty to show the shell's title again."),
                )
                .into_any_element(),
            vec![
                button("rename-cancel", "Cancel", ButtonKind::Ghost, p)
                    .on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.close_overlay(window, cx)))
                    .into_any_element(),
                button("rename-ok", "Rename", ButtonKind::Primary, p)
                    .on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.save_session_name(window, cx)))
                    .into_any_element(),
            ],
        )
    }

    pub(super) fn cycle(&mut self, forward: bool, window: &mut Window, cx: &mut Context<Self>) {
        let items: Vec<ItemKey> = self.sidebar_items().iter().map(|item| item.key()).collect();
        if let Some(next) = self.sessions.cycle(&items, forward) {
            self.activate(next, window, cx);
        }
    }

    /// The item of the active Session, for "new session on this item".
    pub(super) fn active_item(&self) -> Option<SidebarItem> {
        self.item_for_key(&self.active_entry()?.item().clone())
    }

    /// The sidebar item a key stands for, if it is still there.
    pub(super) fn item_for_key(&self, key: &ItemKey) -> Option<SidebarItem> {
        match key.clone() {
            ItemKey::Host { project, host } => {
                let project = self.library.project(&project)?;
                if !project.hosts.contains(&host) { return None; }
                Some(SidebarItem::Host { project: Arc::new(project.clone()), host: Arc::new(self.library.host(&host)?.clone()) })
            }
            ItemKey::Shell { project: Some(id), name } => {
                let project = self.library.project(&id)?;
                let shell = project.shells.iter().find(|sh| sh.name == name)?.clone();
                Some(SidebarItem::Shell { project: Some(Arc::new(project.clone())), shell })
            }
            ItemKey::Shell { project: None, name } => self.library.standalone_shells().into_iter()
                .chain(self.orphan_shells()).find(|sh| sh.name == name)
                .map(|shell| SidebarItem::Shell { project: None, shell }),
        }
    }
}
