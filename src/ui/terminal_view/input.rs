//! The keyboard, copy and paste, and scrolling the Scrollback.

use super::*;

impl TerminalView {
    pub(super) fn key_down(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        self.key_pressed(&event.keystroke, window, cx);
    }

    pub(super) fn send_tab(&mut self, backwards: bool, window: &mut Window, cx: &mut Context<Self>) {
        // A field or button over the terminal keeps the component library's Tab navigation.
        if !self.focus.is_focused(window) {
            cx.propagate();
            return;
        }
        self.key_pressed(
            &gpui::Keystroke {
                key: "tab".into(),
                key_char: None,
                modifiers: Modifiers { shift: backwards, ..Default::default() },
            },
            window,
            cx,
        );
    }

    fn key_pressed(&mut self, ks: &gpui::Keystroke, window: &mut Window, cx: &mut Context<Self>) {
        // Keys typed into a field on top of the terminal (a connection question's user name or
        // password) bubble up to here. They belong to that field: sending them to the session
        // and stopping propagation would also stop the field from receiving the text.
        if !self.focus.is_focused(window) {
            return;
        }
        // A question answered with buttons holds the keyboard: typing must not reach the
        // shell behind it. Its Enter and Escape are actions, handled elsewhere.
        if self.prompt_holds_keys() {
            return;
        }
        let m = &ks.modifiers;
        let mods = Mods { ctrl: m.control, alt: m.alt, shift: m.shift };
        if self.windows_selection && !m.alt && !m.platform && self.prompt.is_none()
            && self.keyboard_selection.key(&mut self.term.lock(), &ks.key, m.control, m.shift)
        {
            cx.stop_propagation();
            cx.notify();
            return;
        }
        // Application shortcuts are all ctrl+shift+… (and ctrl+tab); plain ctrl+letter keys
        // belong to the shell (ctrl+w deletes a word in bash, ctrl+k kills the line).
        if keys::is_app_shortcut(&ks.key, ks.key_char.as_deref(), mods) {
            return;
        }
        // Unhandled, Alt+F4 closes the window and Alt+Space opens its menu.
        if cfg!(target_os = "windows") && keys::is_window_command(&ks.key, mods) {
            return;
        }
        if self.is_disconnected() {
            if ks.key == "enter" {
                cx.emit(TerminalEvent::Reconnect);
                cx.stop_propagation();
            }
            return;
        }
        // Plain text is left to come back through the input handler (`EntityInputHandler`
        // below), which is how dead keys compose and IMEs commit: Windows only turns the key
        // into text (WM_CHAR) when it goes unhandled here, and Linux types `key_char` itself.
        // Both do so only for keys with no modifier but shift, the same test as here.
        if m.is_subset_of(&Modifiers::shift()) && keys::is_text_input(ks.key_char.as_deref()) {
            return;
        }
        let (key_char, mods) = if cfg!(target_os = "macos") {
            keys::mac_option(&ks.key, ks.key_char.as_deref(), mods, self.option_as_meta)
        } else {
            (ks.key_char.as_deref(), mods)
        };
        let mode = *self.term.lock().mode();
        let app_cursor = mode.contains(TermMode::APP_CURSOR);
        if let Some(bytes) = keys::to_bytes(&ks.key, key_char, mods, app_cursor) {
            self.scroll_to_bottom();
            {
                let mut term = self.term.lock();
                term.selection = None;
                // Full-screen programs (vim, htop) take Enter for themselves: nothing to mark.
                if ks.key == "enter" && !m.control && !m.alt && self.records_lines() && !mode.contains(TermMode::ALT_SCREEN) {
                    self.marks.entered(&mut term);
                }
            }
            self.emit_entered(cx);
            // The user answered the question that paused the Startup Commands.
            if ks.key == "enter" && self.startup.paused {
                self.startup.paused = false;
            }
            self.write(bytes);
            cx.stop_propagation();
            cx.notify();
        }
    }

    /// Type text from the platform's text input: a key, a dead-key composition or an IME's
    /// commit.
    pub(super) fn type_text(&mut self, text: &str, cx: &mut Context<Self>) {
        if text.is_empty() || self.is_disconnected() || self.prompt_holds_keys() {
            return;
        }
        self.scroll_to_bottom();
        self.term.lock().selection = None;
        self.write(text.as_bytes().to_vec());
        cx.notify();
    }

    pub(super) fn copy(&mut self, _: &Copy, _: &mut Window, cx: &mut Context<Self>) {
        self.copy_selection(cx);
    }

    /// Put the selection on the clipboard, trimmed and with its colors as the settings say.
    /// Returns whether there was anything to copy.
    pub(super) fn copy_selection(&mut self, cx: &mut Context<Self>) -> bool {
        let (text, html) = {
            let term = self.term.lock();
            let Some(text) = term.selection_to_string() else { return false };
            let text = if self.pointer.trim_copied { text.trim().to_string() } else { text };
            if text.is_empty() {
                return false;
            }
            // Only Windows can hold the HTML beside the text; see `rich_copy::write_clipboard`.
            let html = (cfg!(windows) && self.pointer.copy_formatting).then(|| {
                let appearance = cx.global::<Appearance>();
                rich_copy::selection_html(&term, &appearance.term, &appearance.font_family, appearance.font_size, self.pointer.trim_copied)
            });
            (text, html.flatten())
        };
        rich_copy::write_clipboard(text, html, cx);
        true
    }

    pub(super) fn paste(&mut self, _: &Paste, window: &mut Window, cx: &mut Context<Self>) {
        let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) else { return };
        // Typing in the search bar: Ctrl+Shift+V pastes there, not into the shell.
        if self.search_focused(window, cx) {
            self.paste_into_search(&text, window, cx);
            return;
        }
        self.paste_or_confirm(text, cx);
    }

    /// A question answered with its buttons (a Host Key, a multi-line paste) is open, and the
    /// terminal keeps the focus for its Enter and Escape.
    fn prompt_holds_keys(&self) -> bool {
        matches!(self.prompt, Some(ActivePrompt::HostKey { .. } | ActivePrompt::HostKeyChanged { .. } | ActivePrompt::ConfirmPaste { .. }))
    }

    /// Paste `text`, asking first when it has several lines and that setting is on.
    pub(super) fn paste_or_confirm(&mut self, text: String, cx: &mut Context<Self>) {
        // Nothing reaches the shell, or replaces the question, while one is open.
        if self.prompt.is_some() {
            return;
        }
        let text = if self.pointer.paste_single_line { one_line(&text) } else { text };
        let lines = pasted_lines(&text);
        if self.pointer.confirm_multiline_paste && lines > 1 {
            self.prompt = Some(ActivePrompt::ConfirmPaste { text, lines });
            cx.notify();
        } else {
            self.paste_text(&text);
        }
    }

    fn paste_text(&mut self, text: &str) {
        let bracketed = self.pointer.bracketed_paste && self.term.lock().mode().contains(TermMode::BRACKETED_PASTE);
        self.scroll_to_bottom();
        self.write(keys::paste_bytes(text, bracketed));
    }

    /// The Paste button and the dialog's Enter binding share one path. Taking the pending
    /// text before writing prevents a second confirmation from pasting it again.
    pub(super) fn confirm_paste(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !matches!(self.prompt, Some(ActivePrompt::ConfirmPaste { .. })) {
            return;
        }
        if let Some(ActivePrompt::ConfirmPaste { text, .. }) = self.prompt.take() {
            self.paste_text(&text);
        }
        self.dismiss(window, cx);
    }

    pub(super) fn scroll_page(&mut self, up: bool, cx: &mut Context<Self>) {
        if self.windows_selection && self.prompt.is_none() {
            let key = if up { "pageup" } else { "pagedown" };
            if self.keyboard_selection.key(&mut self.term.lock(), key, false, true) {
                cx.notify();
                return;
            }
            // On the alternate screen the program keeps Shift+PageUp/PageDown too.
            let app_cursor = self.term.lock().mode().contains(TermMode::APP_CURSOR);
            if let Some(bytes) = keys::to_bytes(key, None, Mods { ctrl: false, alt: false, shift: true }, app_cursor) {
                self.write(bytes);
                cx.notify();
            }
            return;
        }
        self.scroll_view(if up { Scroll::PageUp } else { Scroll::PageDown });
        cx.notify();
    }

    /// Scroll the Scrollback, offering the output a clear hid once the view is at its top.
    pub(super) fn scroll_view(&mut self, scroll: Scroll) {
        let up = matches!(scroll, Scroll::PageUp | Scroll::Top) || matches!(scroll, Scroll::Delta(n) if n > 0);
        let mut term = self.term.lock();
        term.scroll_display(scroll);
        let grid = term.grid();
        self.restore_offer = up && grid.hidden_history() > 0 && grid.display_offset() == grid.history_size();
    }

    /// Whether the Restore scrollback button shows.
    fn shows_restore(&self) -> bool {
        self.restore_offer && {
            let term = self.term.lock();
            let grid = term.grid();
            !term.mode().contains(TermMode::ALT_SCREEN)
                && grid.hidden_history() > 0
                && grid.display_offset() == grid.history_size()
        }
    }

    /// Bring back the output a clear hid, and show the end of it.
    fn restore_scrollback(&mut self, cx: &mut Context<Self>) {
        self.restore_offer = false;
        {
            let mut term = self.term.lock();
            let half = (term.screen_lines() / 2) as i32;
            term.grid_mut().restore_history();
            term.scroll_display(Scroll::Delta(half));
        }
        cx.notify();
    }

    pub(super) fn render_restore(&self, cx: &mut Context<Self>) -> Option<gpui::AnyElement> {
        if !self.shows_restore() {
            return None;
        }
        let p = cx.global::<Appearance>().app;
        let stop = |_: &MouseDownEvent, window: &mut Window, cx: &mut gpui::App| {
            // Not a click on the terminal: no selection, and the keyboard stays where it was.
            window.prevent_default();
            cx.stop_propagation();
        };
        Some(
            div()
                .absolute()
                .top(px(10.))
                .left_0()
                .right_0()
                .flex()
                .justify_center()
                .child(
                    div()
                        .rounded(p.corner(8.))
                        .font_family(p.font_family)
                        .text_size(px(13.))
                        .cursor_default()
                        .on_mouse_down(MouseButton::Left, stop)
                        .on_mouse_down(MouseButton::Middle, stop)
                        .on_mouse_down(MouseButton::Right, stop)
                        .child(
                            icon_text_button("restore-scrollback", "rotate-ccw", "Restore scrollback", ButtonKind::Secondary, p)
                                .on_click(cx.listener(|this, _, _, cx| this.restore_scrollback(cx))),
                        ),
                )
                .into_any_element(),
        )
    }
}
