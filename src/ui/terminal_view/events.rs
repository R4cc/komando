//! What the Session sends the view (output, status, prompts, things to remember), the
//! questions it asks, and the Startup Commands typed once the shell is ready.

use super::*;

impl TerminalView {
    pub(super) fn on_session_event(&mut self, event: SessionEvent, window: &mut Window, cx: &mut Context<Self>) {
        match event {
            SessionEvent::Wakeup => {
                crate::perf::count("session wakeup");
                self.proxy.clear_dirty();
                // Output since the last Wakeup is in the terminal by now (it wakes after writing).
                self.minimap_state.output();
                // New output may have moved the link's text from under the pointer: the
                // underline goes unless the same link is still where it was drawn.
                if let Some(link) = &self.hover_link
                    && let Some((line, cols)) = link.spans.first()
                    && link_at(&*self.term.lock(), GridPoint::new(*line, Column(cols.start))).as_ref() != Some(link)
                {
                    self.hover_link = None;
                }
                if self.status == Status::Connected {
                    self.note_output(cx);
                }
                if self.marks.is_pending() {
                    self.marks.output(&mut *self.term.lock());
                    self.emit_entered(cx);
                } else {
                    crate::session::drop_blank_scrollback(&mut *self.term.lock());
                }
                if self.startup.waiting() && !self.startup.next_waits() && !self.is_disconnected() && self.status == Status::Connected {
                    self.schedule_startup(STARTUP_QUIET, window, cx);
                }
                self.search_output(cx);
                cx.notify();
            }
            SessionEvent::Title(title) => {
                // An empty title is a reset (vim restores the title it found on exit).
                let title = if title.is_empty() { self.base_title.clone() } else { title.into() };
                if title != self.title {
                    self.title = title;
                    cx.emit(TerminalEvent::Changed);
                }
            }
            SessionEvent::Status(status) => {
                if self.status != status {
                    if status != Status::Connected {
                        self.latency.clear();
                    }
                    // They start before the shell, while the Session still says Connecting.
                    if matches!(status, Status::Disconnected(_)) {
                        self.forwards.clear();
                    }
                    self.status = status;
                    match &self.status {
                        Status::Disconnected(reason) => {
                            // A pending question can no longer be answered.
                            if matches!(self.prompt, Some(ActivePrompt::Input { .. } | ActivePrompt::HostKey { .. } | ActivePrompt::HostKeyChanged { .. })) {
                                self.prompt = None;
                            }
                            self.startup = StartupCommands::default();
                            self.proxy.wait_for(None);
                            match self.route.as_mut() {
                                Some(route) if !route.is_finished() => route.fail(reason.clone()),
                                _ => self.route = None,
                            }
                        }
                        Status::Connected => {
                            self.was_connected = true;
                            if let Some(route) = self.route.as_mut() {
                                route.finish();
                                cx.spawn_in(window, async move |this, cx| {
                                    cx.background_executor().timer(connect_route::FADE).await;
                                    let _ = this.update(cx, |this, cx| {
                                        if this.route.as_ref().is_some_and(|r| r.is_finished()) {
                                            this.route = None;
                                            cx.notify();
                                        }
                                    });
                                })
                                .detach();
                            }
                            if self.startup.next_waits() {
                                self.limit_wait(cx);
                            } else if self.startup.waiting() {
                                self.schedule_startup(STARTUP_FIRST, window, cx);
                            }
                        }
                        Status::Connecting => {}
                    }
                    cx.emit(TerminalEvent::Changed);
                    cx.notify();
                }
            }
            SessionEvent::Latency(latency) => {
                if !self.is_disconnected() {
                    match self.latency.iter_mut().find(|hop| hop.hop == latency.hop) {
                        Some(hop) => *hop = latency,
                        None => {
                            self.latency.push(latency);
                            self.latency.sort_by_key(|hop| hop.hop);
                        }
                    }
                    cx.emit(TerminalEvent::Changed);
                    cx.notify();
                }
            }
            SessionEvent::Forward(status) => {
                if !self.is_disconnected() {
                    if let crate::session::forward::ForwardState::Failed(why) = &status.state {
                        cx.emit(TerminalEvent::ForwardFailed { forward: status.forward.clone(), why: why.clone() });
                    }
                    match self.forwards.iter_mut().find(|f| f.index == status.index) {
                        Some(known) => *known = status,
                        None => {
                            self.forwards.push(status);
                            self.forwards.sort_by_key(|f| f.index);
                        }
                    }
                    cx.notify();
                }
            }
            SessionEvent::Hop { index, step } => {
                if let Some(route) = self.route.as_mut() {
                    route.advance(index, step);
                    cx.notify();
                }
            }
            SessionEvent::Prompt(prompt) => self.show_prompt(prompt, window, cx),
            SessionEvent::Remember { hop, what } => cx.emit(TerminalEvent::Remember { hop, what }),
            SessionEvent::ClipboardStore(text) => self.clipboard_store(text, cx),
            SessionEvent::Appeared(id) => {
                if self.startup.watching == Some(id) {
                    self.startup.watching = None;
                    self.type_startup_command(cx);
                }
            }
        }
    }

    pub(super) fn show_prompt(&mut self, prompt: Prompt, window: &mut Window, cx: &mut Context<Self>) {
        self.prompt = Some(match prompt {
            Prompt::HostKey { fingerprint, algorithm, other_types, reply } => {
                ActivePrompt::HostKey { fingerprint, algorithm, other_types, reply: Some(reply) }
            }
            Prompt::HostKeyChanged { fingerprint, previous, algorithm, openssh, reply } => {
                ActivePrompt::HostKeyChanged { fingerprint, previous, algorithm, openssh, reply: Some(reply) }
            }
            Prompt::Input { label, secret, remember, error, reply } => {
                let input = cx.new(|cx| InputState::new(window, cx).masked(secret).placeholder(label.clone()));
                let sub = cx.subscribe_in(&input, window, |this, _, event: &InputEvent, window, cx| {
                    if let InputEvent::PressEnter { .. } = event {
                        this.answer_input(true, window, cx);
                    }
                });
                self._subscriptions.push(sub);
                // Take the keyboard only from the terminal itself (a session you're watching
                // connect), not from a dialog you're typing in; the question waits behind it.
                if self.focus.is_focused(window) || window.focused(cx).is_none() {
                    input.update(cx, |state, cx| state.focus(window, cx));
                }
                ActivePrompt::Input { label, secret, error, input, remember, reply: Some(reply) }
            }
        });
        cx.notify();
    }

    /// Give the keyboard to the open question: its field, or the terminal for a question
    /// answered with buttons.
    pub fn focus_prompt(&self, window: &mut Window, cx: &mut Context<Self>) {
        let handle = match self.prompt.as_ref() {
            Some(ActivePrompt::Input { input, .. }) => input.focus_handle(cx),
            _ => self.focus.clone(),
        };
        if !handle.is_focused(window) {
            window.focus(&handle, cx);
        }
    }

    pub(super) fn answer_input(&mut self, submit: bool, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(ActivePrompt::Input { input, remember, reply, .. }) = self.prompt.as_mut() {
            let value = input.read(cx).value().to_string();
            let remember = remember.unwrap_or(false);
            if let Some(reply) = reply.take() {
                let _ = reply.send(submit.then_some(Answer { value, remember }));
            }
            self.prompt = None;
            window.focus(&self.focus, cx);
            cx.notify();
        }
    }

    pub(super) fn toggle_remember(&mut self, clicked: bool, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(ActivePrompt::Input { input, remember: Some(remember), .. }) = self.prompt.as_mut() {
            *remember = !*remember;
            // A click returns to the field so typing carries on; Space keeps the Tab position.
            if clicked {
                input.update(cx, |state, cx| state.focus(window, cx));
            }
            cx.notify();
        }
    }

    /// Answer a host key question: trust a new host's key, or replace a changed one.
    pub(super) fn answer_host_key(&mut self, trust: bool, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(ActivePrompt::HostKey { reply, .. } | ActivePrompt::HostKeyChanged { reply, .. }) = self.prompt.as_mut()
            && let Some(reply) = reply.take()
        {
            let _ = reply.send(trust);
        }
        self.prompt = None;
        window.focus(&self.focus, cx);
        cx.notify();
    }

    pub(super) fn dismiss(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.prompt = None;
        window.focus(&self.focus, cx);
        cx.notify();
    }

    /// Type the next Startup Command after `delay`, unless output arrives in the meantime
    /// (which starts a new wait).
    pub(super) fn schedule_startup(&mut self, delay: Duration, window: &mut Window, cx: &mut Context<Self>) {
        self.startup.generation += 1;
        let generation = self.startup.generation;
        cx.spawn_in(window, async move |this, cx| {
            cx.background_executor().timer(delay).await;
            let _ = this.update(cx, |this, cx| {
                if this.startup.generation == generation {
                    this.type_startup_command(cx);
                }
            });
        })
        .detach();
    }

    pub(super) fn type_startup_command(&mut self, cx: &mut Context<Self>) {
        if !self.startup.waiting() || self.status != Status::Connected {
            return;
        }
        if self.startup.next_waits() {
            // Typed once its text appears, whatever the shell does in between.
            if self.startup.watching.is_some() {
                return;
            }
        } else {
            let cursor_line = {
                let term = self.term.lock();
                let grid = term.grid();
                let row = &grid[grid.cursor.point.line];
                (0..term.columns()).map(|c| row[Column(c)].c).collect::<String>()
            };
            if asks_for_password(&cursor_line) {
                self.startup.paused = true;
                return;
            }
        }
        let Some(next) = self.startup.queue.pop_front() else { return };
        // Before typing, so the output that follows can't come before the watch.
        self.watch_next(true);
        self.limit_wait(cx);
        let line = next.command;
        self.scroll_to_bottom();
        {
            let mut term = self.term.lock();
            if self.records_lines() && !term.mode().contains(TermMode::ALT_SCREEN) {
                self.marks.entered(&mut term);
            }
        }
        self.emit_entered(cx);
        self.write(format!("{line}\r").into_bytes());
        cx.notify();
    }

    /// Entered lines are read for the command rail or the Command History.
    pub(super) fn records_lines(&self) -> bool {
        self.command_rail || self.command_history
    }

    /// Hand the lines read since last time to the Command History.
    pub(super) fn emit_entered(&mut self, cx: &mut Context<Self>) {
        let lines = self.marks.take_new();
        if !self.command_history {
            return;
        }
        for mark in lines {
            cx.emit(TerminalEvent::Entered { line: mark.line, at: mark.at });
        }
    }

    /// Type `line` into the shell and run it, replacing anything typed but not yet entered.
    /// Refuses (false) when there is no shell to type into, or a full-screen program such as
    /// an editor has the terminal and would take the keys as its own.
    pub fn run_line(&mut self, line: &str, cx: &mut Context<Self>) -> bool {
        if self.sink.is_none() || self.status != Status::Connected || self.prompt.is_some() {
            return false;
        }
        {
            let mut term = self.term.lock();
            if term.mode().contains(TermMode::ALT_SCREEN) {
                return false;
            }
            if self.records_lines() {
                self.marks.entered(&mut term);
            }
        }
        self.emit_entered(cx);
        self.scroll_to_bottom();
        // Ctrl+U clears a half-typed command so it doesn't run together with `line`.
        self.write(format!("\x15{line}\r").into_bytes());
        cx.notify();
        true
    }

    pub(super) fn write(&mut self, bytes: Vec<u8>) {
        self.reset_cursor();
        if let Some(sink) = &self.sink {
            self.activity.input(Instant::now());
            sink.write(bytes);
        }
    }

    /// Output arrived: note it, and keep `busy` up to date until the output settles.
    pub(super) fn note_output(&mut self, cx: &mut Context<Self>) {
        self.activity.output(Instant::now());
        if self.activity_watch {
            return;
        }
        self.activity_watch = true;
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(ACTIVITY_CHECK).await;
                let go_on = this
                    .update(cx, |this, cx| {
                        let now = Instant::now();
                        let busy = this.activity.busy(now) && this.status == Status::Connected;
                        if busy != this.busy {
                            this.busy = busy;
                            cx.emit(TerminalEvent::Changed);
                        }
                        let settled = this.activity.settled(now);
                        if settled {
                            this.activity_watch = false;
                        }
                        !settled
                    })
                    .unwrap_or(false);
                if !go_on {
                    break;
                }
            }
        })
        .detach();
    }

    pub(super) fn scroll_to_bottom(&mut self) {
        self.restore_offer = false;
        let mut term = self.term.lock();
        if term.grid().display_offset() != 0 {
            term.scroll_display(Scroll::Bottom);
        }
    }
}
