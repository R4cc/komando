//! What is drawn over the output: the command rail, the connect route, prompt cards, the
//! clipboard question and the disconnected banner.

use super::*;
use crate::ui::widgets::{dialog_card, dialog_mono, dialog_text, icon_el, keycaps};

impl TerminalView {
    /// Scroll so the entered line sits near the top of the view, and light it up briefly.
    fn jump_to_mark(&mut self, id: u64, cx: &mut Context<Self>) {
        {
            let mut term = self.term.lock();
            if term.mode().contains(TermMode::ALT_SCREEN) {
                return;
            }
            let Some(line) = self.marks.rows(&term).into_iter().find_map(|(m, line)| (m == id).then_some(line)) else {
                return;
            };
            let history = term.grid().history_size() as i32;
            let target = (2 - line.0).clamp(0, history);
            let current = term.grid().display_offset() as i32;
            term.scroll_display(Scroll::Delta(target - current));
        }
        let epoch = self.flash.map_or(0, |(_, e)| e) + 1;
        self.flash = Some((id, epoch));
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(FLASH).await;
            let _ = this.update(cx, |this, cx| {
                if this.flash.is_some_and(|(_, e)| e == epoch) {
                    this.flash = None;
                    cx.notify();
                }
            });
        })
        .detach();
        cx.notify();
    }

    /// The command rail: one short line per entered command along the left edge, inside the
    /// terminal's padding so it takes no columns. Hidden until there is output to scroll back
    /// through. Returns the view row to light up, if any.
    pub(super) fn render_rail(&mut self, cx: &mut Context<Self>) -> (Option<gpui::AnyElement>, Option<i32>) {
        if !self.command_rail || self.marks.list().is_empty() {
            return (None, None);
        }
        let (rows, offset, screen_rows) = {
            let term = self.term.lock();
            if term.mode().contains(TermMode::ALT_SCREEN) {
                return (None, None);
            }
            let grid = term.grid();
            if grid.history_size() == 0 {
                self.hovered_mark = None;
                return (None, None);
            }
            (self.marks.rows(&term), grid.display_offset() as i32, grid.screen_lines() as i32)
        };
        if rows.is_empty() {
            return (None, None);
        }
        let flash_row = self
            .flash
            .and_then(|(id, _)| rows.iter().find(|(m, _)| *m == id))
            .map(|(_, line)| line.0 + offset)
            .filter(|row| (0..screen_rows).contains(row));

        // The command being read: the newest one at the bottom, otherwise the last one that
        // starts in the top part of the view.
        let top = -offset;
        let current = if offset == 0 {
            rows.len() - 1
        } else {
            rows.iter().rposition(|(_, line)| line.0 <= top + (screen_rows / 4).max(2)).unwrap_or(0)
        };
        // Show as many as fit, keeping the current one in the window.
        let (_, cell_h) = self.cell.get();
        let fit = (((screen_rows as f32 * cell_h) * 0.85 / RAIL_STEP) as usize).max(3);
        let start = if rows.len() <= fit { 0 } else { current.saturating_sub(fit / 2).min(rows.len() - fit) };
        let end = (start + fit).min(rows.len());

        let appearance = cx.global::<Appearance>();
        let p = appearance.app;
        let mono = appearance.font_family.clone();
        let fg = rgb_to_hsla(appearance.term.foreground);
        let with_alpha = |a: f32| Hsla { a, ..fg };
        let hovered = self.hovered_mark;

        let ticks = rows[start..end].iter().enumerate().map(|(i, (id, _))| {
            let id = *id;
            let is_current = start + i == current;
            let is_hovered = hovered == Some(id);
            let card = is_hovered.then(|| self.marks.get(id)).flatten().map(|mark| {
                let (prompt, command) = mark.parts();
                let meta = if prompt.is_empty() {
                    mark.at.format("%H:%M").to_string()
                } else {
                    format!("{prompt} · {}", mark.at.format("%H:%M"))
                };
                div()
                    .absolute()
                    .left(px(PADDING_X + 4.))
                    .top(px(RAIL_STEP / 2. - 26.))
                    .min_w(px(200.))
                    .max_w(px(520.))
                    .px(px(14.))
                    .py(px(10.))
                    .rounded(p.corner(12.))
                    .bg(p.surface)
                    .border_1()
                    .border_color(p.border)
                    .flex()
                    .flex_col()
                    .gap(px(3.))
                    .child(
                        div()
                            .font_family(mono.clone())
                            .text_size(px(13.))
                            .text_color(p.text)
                            .whitespace_nowrap()
                            .overflow_hidden()
                            .text_ellipsis()
                            .child(command.to_string()),
                    )
                    .child(
                        div()
                            .font_family(p.font_family)
                            .text_size(px(12.))
                            .text_color(p.text_muted)
                            .whitespace_nowrap()
                            .overflow_hidden()
                            .text_ellipsis()
                            .child(meta),
                    )
            });
            div()
                .id(SharedString::from(format!("mark-{id}")))
                .relative()
                .w(px(PADDING_X))
                .h(px(RAIL_STEP))
                .flex()
                .items_center()
                .cursor_pointer()
                .on_hover(cx.listener(move |this, over: &bool, _, cx| {
                    if *over {
                        this.hovered_mark = Some(id);
                    } else if this.hovered_mark == Some(id) {
                        this.hovered_mark = None;
                    }
                    cx.notify();
                }))
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, _: &MouseDownEvent, window, cx| {
                        cx.stop_propagation();
                        window.focus(&this.focus, cx);
                        this.jump_to_mark(id, cx);
                    }),
                )
                .child(
                    div()
                        .ml(px(2.))
                        .h(px(2.))
                        .rounded(px(1.))
                        .w(px(if is_current || is_hovered { 9. } else { 7. }))
                        .bg(with_alpha(if is_hovered {
                            1.0
                        } else if is_current {
                            0.9
                        } else {
                            0.5
                        })),
                )
                .children(card)
        });
        let rail = div()
            .absolute()
            .left_0()
            .top_0()
            .bottom_0()
            .w(px(PADDING_X))
            .flex()
            .flex_col()
            .justify_center()
            .children(ticks)
            .into_any_element();
        (Some(rail), flash_row)
    }

    /// Whether a question is open: a connection's user name, password or host key, or a paste
    /// to confirm. The workspace shows it as a modal over the whole window (see `prompt_modal`).
    pub fn has_prompt(&self) -> bool {
        self.prompt.is_some()
    }

    /// The open question as a card, for the workspace's modal layer. It lives there rather than
    /// in the terminal so the rest of the window (sidebar, other dialogs) is blocked while it is
    /// open.
    pub fn prompt_modal(&self, cx: &mut Context<Self>) -> Option<gpui::AnyElement> {
        self.prompt_card(cx)
    }

    /// Escape on the open question: what its Cancel (or Close) button does.
    pub fn cancel_prompt(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        match self.prompt {
            Some(ActivePrompt::Input { .. }) => self.answer_input(false, window, cx),
            Some(ActivePrompt::HostKey { .. } | ActivePrompt::HostKeyChanged { .. }) => self.answer_host_key(false, window, cx),
            Some(_) => self.dismiss(window, cx),
            None => {}
        }
    }

    /// While connecting through Jump Hosts: the route page, with the current hop's question.
    pub(super) fn render_route(&self, cx: &mut Context<Self>) -> Option<gpui::AnyElement> {
        let route = self.route.as_ref()?;
        let p = cx.global::<Appearance>().app;
        let hop = route.current();
        // The same pair, named and drawn the same, as the notice when a direct connection fails.
        let actions = vec![
            button("route-edit", "Edit host", ButtonKind::Ghost, p)
                .border_1()
                .border_color(p.border.blend(p.text.opacity(0.18)))
                .text_color(p.text)
                .on_click(cx.listener(move |_, _, _, cx| cx.emit(TerminalEvent::EditHost { hop })))
                .into_any_element(),
            button("route-retry", "Reconnect", ButtonKind::Primary, p)
                .on_click(cx.listener(|_, _, _, cx| cx.emit(TerminalEvent::Reconnect)))
                .into_any_element(),
        ];
        Some(route.render(actions, p))
    }

    /// The open question as a card.
    fn prompt_card(&self, cx: &mut Context<Self>) -> Option<gpui::AnyElement> {
        let p = cx.global::<Appearance>().app;
        let prompt = self.prompt.as_ref()?;
        let hop_name = self.route.as_ref().and_then(|r| r.hop_name(r.current()).map(str::to_string));
        let card = |title: &str, body: gpui::AnyElement, actions: Vec<gpui::AnyElement>| {
            dialog_card(title.to_string(), body, actions, p).into_any_element()
        };
        let text = |s: String| dialog_text(s, p);
        let mono = |s: String| dialog_mono(s, p);
        let element = match prompt {
            ActivePrompt::HostKey { fingerprint, algorithm, other_types, .. } => card(
                &match &hop_name {
                    Some(name) => format!("Trust {name}?"),
                    None => "Trust this server?".to_string(),
                },
                div()
                    .flex()
                    .flex_col()
                    .gap(px(10.))
                    .child(text(if other_types.is_empty() {
                        format!("First connection to this host. Check that this {algorithm} key fingerprint matches the server before trusting it.")
                    } else {
                        format!(
                            "This PC already trusts a {} key for this host, but the server presented a {algorithm} key. That can happen when a server has keys of several types, but it can also mean someone is intercepting the connection. Check this fingerprint with the server before trusting it.",
                            other_types.join(" or ")
                        )
                    }))
                    .child(mono(fingerprint.clone()))
                    .into_any_element(),
                vec![
                    button("hk-cancel", "Cancel", ButtonKind::Ghost, p)
                        .on_click(cx.listener(|this, _, window, cx| this.answer_host_key(false, window, cx)))
                        .into_any_element(),
                    button("hk-trust", "Trust and connect", ButtonKind::Primary, p)
                        .on_click(cx.listener(|this, _, window, cx| this.answer_host_key(true, window, cx)))
                        .into_any_element(),
                ],
            ),
            ActivePrompt::HostKeyChanged { fingerprint, previous, algorithm, openssh, .. } => card(
                &match &hop_name {
                    Some(name) => format!("Host key of {name} changed"),
                    None => "Host key changed".to_string(),
                },
                div()
                    .flex()
                    .flex_col()
                    .gap(px(10.))
                    .child(text(format!(
                        "The server presented a different {algorithm} key than the one you trusted{}. Someone may be intercepting the connection, or the server was reinstalled. Only replace the key if you know why it changed, or after checking the new fingerprint with the server's administrator.",
                        if *openssh { " in your OpenSSH known_hosts" } else { "" }
                    )))
                    .when_some(previous.clone(), |el, previous| {
                        el.child(text("Trusted key:".into())).child(mono(previous))
                    })
                    .child(text("Key presented now:".into()))
                    .child(mono(fingerprint.clone()))
                    .when(*openssh, |el| {
                        el.child(text("Replacing trusts the new key in Komando only; your OpenSSH known_hosts is left as it is.".into()))
                    })
                    .into_any_element(),
                vec![
                    button("hkc-cancel", "Cancel", ButtonKind::Ghost, p)
                        .on_click(cx.listener(|this, _, window, cx| this.answer_host_key(false, window, cx)))
                        .into_any_element(),
                    button("hkc-replace", "Replace key and connect", ButtonKind::Danger, p)
                        .on_click(cx.listener(|this, _, window, cx| this.answer_host_key(true, window, cx)))
                        .into_any_element(),
                ],
            ),
            ActivePrompt::Input { label, secret, error, input, remember, .. } => card(
                label,
                div()
                    .flex()
                    .flex_col()
                    .gap(px(8.))
                    .child(if *secret { password_input(input, p).into_any_element() } else { Input::new(input).into_any_element() })
                    .when_some(error.clone(), |el, error| {
                        el.child(div().text_size(px(12.)).text_color(p.danger).child(error))
                    })
                    .when_some(*remember, |el, ticked| {
                        let view = cx.entity().downgrade();
                        el.child(crate::ui::widgets::labelled_checkbox("in-remember", "Remember for this host", ticked, p, move |clicked, window, cx| {
                            let _ = view.update(cx, |this, cx| this.toggle_remember(clicked, window, cx));
                        }))
                    })
                    .into_any_element(),
                vec![
                    button("in-cancel", "Cancel", ButtonKind::Ghost, p)
                        .on_click(cx.listener(|this, _, window, cx| this.answer_input(false, window, cx)))
                        .into_any_element(),
                    button("in-ok", "Continue", ButtonKind::Primary, p)
                        .on_click(cx.listener(|this, _, window, cx| this.answer_input(true, window, cx)))
                        .into_any_element(),
                ],
            ),
            ActivePrompt::ConfirmPaste { lines, .. } => card(
                &format!("Paste {lines} lines?"),
                text("Each line may run as a separate command on this machine.".into()),
                vec![
                    button("pp-cancel", "Cancel", ButtonKind::Ghost, p)
                        .on_click(cx.listener(|this, _, window, cx| this.dismiss(window, cx)))
                        .into_any_element(),
                    button("pp-ok", "Paste", ButtonKind::Primary, p)
                        .on_click(cx.listener(|this, _, window, cx| this.confirm_paste(window, cx)))
                        .into_any_element(),
                ],
            ),
        };
        Some(element)
    }

    /// A program asked to put `text` on the clipboard (OSC 52).
    pub(super) fn clipboard_store(&mut self, text: String, cx: &mut Context<Self>) {
        use crate::model::RemoteClipboard;
        match self.clipboard {
            RemoteClipboard::Allow => cx.write_to_clipboard(ClipboardItem::new_string(text)),
            RemoteClipboard::Never => log::info!("a program on the host asked to copy to the clipboard: not allowed"),
            // A later copy replaces the one still waiting: the question is about the latest.
            RemoteClipboard::Ask => {
                self.clipboard_ask = Some(text);
                cx.notify();
            }
        }
    }

    /// The Host's clipboard choice or Settings changed. A copy still waiting is dropped
    /// unless it's still to be asked about.
    pub fn set_clipboard(&mut self, policy: crate::model::RemoteClipboard, cx: &mut Context<Self>) {
        if self.clipboard != policy {
            self.clipboard = policy;
            if policy != crate::model::RemoteClipboard::Ask {
                self.clipboard_ask = None;
            }
            cx.notify();
        }
    }

    /// An answer to the clipboard question: copy it or not, and for "Always" and "Never" keep
    /// the choice for the Host (`remember`).
    fn answer_clipboard(&mut self, copy: bool, remember: bool, cx: &mut Context<Self>) {
        let Some(text) = self.clipboard_ask.take() else { return };
        if copy {
            cx.write_to_clipboard(ClipboardItem::new_string(text));
        }
        if remember {
            cx.emit(TerminalEvent::ClipboardChoice { allow: copy });
        }
        cx.notify();
    }

    pub(super) fn render_clipboard_ask(&self, cx: &mut Context<Self>) -> Option<gpui::AnyElement> {
        let text = self.clipboard_ask.as_ref()?;
        let p = cx.global::<Appearance>().app;
        let stop = |_: &MouseDownEvent, window: &mut Window, cx: &mut gpui::App| {
            // Not a click on the terminal: no selection, and the keyboard stays where it was.
            window.prevent_default();
            cx.stop_propagation();
        };
        let (summary, preview) = clipboard_preview(text);
        Some(
            div()
                .absolute()
                .bottom(px(16.))
                .left(px(16.))
                .right(px(16.))
                .flex()
                .justify_center()
                .child(
                    div()
                        .id("clipboard-ask")
                        .role(gpui::Role::AlertDialog)
                        .aria_label("A program wants to copy to your clipboard")
                        .min_w_0()
                        .max_w(px(560.))
                        .flex()
                        .flex_col()
                        .gap(px(8.))
                        .px(px(14.))
                        .py(px(10.))
                        .rounded(p.corner(10.))
                        .bg(p.surface)
                        .border_1()
                        .border_color(p.border)
                        .font_family(p.font_family)
                        .text_size(px(13.))
                        .cursor_default()
                        .on_mouse_down(MouseButton::Left, stop)
                        .on_mouse_down(MouseButton::Middle, stop)
                        .on_mouse_down(MouseButton::Right, stop)
                        .child(div().text_color(p.text).child(format!("A program on this host wants to copy {summary} to your clipboard.")))
                        .child(
                            div()
                                .min_w_0()
                                .px(px(8.))
                                .py(px(4.))
                                .rounded(p.corner(6.))
                                .bg(p.muted)
                                .font_family(p.mono_family)
                                .text_size(px(12.))
                                .text_color(p.text_muted)
                                .whitespace_nowrap()
                                .overflow_hidden()
                                .text_ellipsis()
                                .child(preview),
                        )
                        .child(
                            div()
                                .flex()
                                .flex_wrap()
                                .justify_end()
                                .gap(px(8.))
                                .child(
                                    button("clipboard-never", "Never for this host", ButtonKind::Ghost, p)
                                        .on_click(cx.listener(|this, _, _, cx| this.answer_clipboard(false, true, cx))),
                                )
                                .child(
                                    button("clipboard-deny", "Not now", ButtonKind::Ghost, p)
                                        .on_click(cx.listener(|this, _, _, cx| this.answer_clipboard(false, false, cx))),
                                )
                                .child(
                                    button("clipboard-always", "Always for this host", ButtonKind::Secondary, p)
                                        .on_click(cx.listener(|this, _, _, cx| this.answer_clipboard(true, true, cx))),
                                )
                                .child(
                                    button("clipboard-once", "Allow once", ButtonKind::Primary, p)
                                        .on_click(cx.listener(|this, _, _, cx| this.answer_clipboard(true, false, cx))),
                                ),
                        ),
                )
                .into_any_element(),
        )
    }

    pub(super) fn render_disconnected(&self, cx: &mut Context<Self>) -> Option<gpui::AnyElement> {
        let Status::Disconnected(reason) = &self.status else { return None };
        // The route page shows where and why the connection failed; the output itself says
        // that a connected SSH Session ended (see `ssh::ConnectLog::ended`).
        if self.route.is_some() || (self.tells_end && self.was_connected) {
            return None;
        }
        let p = cx.global::<Appearance>().app;
        // It never connected: a failure, marked in red like its row in the sidebar.
        let failed = !self.was_connected && reason != crate::session::CLOSED_BY_USER;
        // An SSH connection: its log above has the exact reason, and its Host can be edited.
        let said = self.tells_end;
        // Right under the last line of output, where the failure is read, unless that is
        // too far down to fit: then at the bottom.
        let (_, line_h) = self.cell.get();
        let (cursor_line, screen_lines) = {
            let term = self.term.lock();
            (term.grid().cursor.point.line.0.max(0) as f32, term.screen_lines() as f32)
        };
        let under_output = cursor_line < screen_lines - 6.;
        // What happened, in a few words, then where: the log line above keeps the exact reason.
        let heading = match (failed, self.status.timed_out(), said) {
            (false, ..) => "Disconnected",
            (true, true, true) => "SSH connection timed out",
            (true, true, false) => "Connection timed out",
            (true, false, true) => "Couldn't connect over SSH",
            (true, false, false) => "Couldn't connect",
        };
        let detail = failure_detail(reason);
        let accent = if failed { p.danger } else { p.border };
        // Outlines that show on the terminal's background in either theme.
        let outline = p.border.blend(p.text.opacity(0.18));
        let enter = keycaps("Enter", p);
        let actions = div()
            .mt(px(6.))
            .text_size(px(13.))
            .flex()
            .flex_wrap()
            .items_center()
            .gap(px(10.))
            .child(
                button("reconnect", "Reconnect", ButtonKind::Primary, p)
                    .on_click(cx.listener(|_, _, _, cx| cx.emit(TerminalEvent::Reconnect))),
            )
            .when(failed && said, |el| {
                // Outlined, in the text colour: a filled secondary button takes the theme's
                // secondary colours, an accent in some themes.
                el.child(
                    button("reconnect-edit", "Edit host", ButtonKind::Ghost, p)
                        .border_1()
                        .border_color(outline)
                        .text_color(p.text)
                        .on_click(cx.listener(|_, _, _, cx| cx.emit(TerminalEvent::EditHost { hop: THE_HOST }))),
                )
            })
            .child(div().ml(px(6.)).flex().items_center().gap(px(6.)).text_color(p.text_muted).child(enter).child("to retry"));
        // No card: a coloured bar down its left edge ties it to the output above, as a margin
        // note would.
        let notice = div()
            .min_w_0()
            .max_w(px(640.))
            .flex()
            .flex_col()
            .gap(px(4.))
            .pl(px(20.))
            .py(px(4.))
            .border_l_2()
            .border_color(accent)
            .font_family(p.font_family)
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(10.))
                    .child(icon_el(if failed { "triangle-alert" } else { "unplug" }, 17., if failed { p.danger } else { p.text_muted }))
                    .child(div().text_size(px(14.)).font_weight(FontWeight::SEMIBOLD).text_color(p.text).child(heading)),
            )
            .when_some(detail, |el, detail| {
                // A long reason (a failed key exchange lists every algorithm) ends in "…" past
                // three lines: the output above has it in full.
                el.child(
                    div()
                        .min_w_0()
                        .whitespace_normal()
                        .line_clamp(3)
                        .text_ellipsis()
                        .text_size(px(13.5))
                        .text_color(p.text_muted)
                        .child(detail),
                )
            })
            .child(actions);
        Some(
            div()
                .absolute()
                .left(px(PADDING_X))
                .right(px(PADDING_X))
                .map(|el| {
                    if under_output {
                        el.top(px(PADDING_Y + cursor_line * line_h + 14.))
                    } else {
                        el.bottom(px(16.))
                    }
                })
                .flex()
                .child(notice)
                .into_any_element(),
        )
    }
}

/// The line under a dropped Session's heading: where it couldn't connect, in plain words for a
/// timeout ("Could not connect to 10.0.0.1:22"), otherwise the reason itself. `None` when
/// there is nothing to say.
fn failure_detail(reason: &str) -> Option<String> {
    use crate::session::{TIMED_OUT, TIMED_OUT_OPENING_SHELL, TIMED_OUT_SIGNING_IN};
    let reason = reason.trim();
    if let Some(at) = reason.strip_prefix(TIMED_OUT) {
        return Some(format!("Could not connect to {at}"));
    }
    if let Some(at) = reason.strip_prefix(TIMED_OUT_SIGNING_IN) {
        return Some(format!("{at} stopped answering while signing in"));
    }
    if let Some(at) = reason.strip_prefix(TIMED_OUT_OPENING_SHELL) {
        return Some(format!("{at} didn't open the shell"));
    }
    (!reason.is_empty()).then(|| reason.to_string())
}

#[cfg(test)]
mod failure_detail_tests {
    use super::failure_detail;

    #[test]
    fn says_where_a_timeout_happened_in_plain_words() {
        assert_eq!(failure_detail("Timed out connecting to 10.30.1.11:22").as_deref(), Some("Could not connect to 10.30.1.11:22"));
        assert_eq!(failure_detail("Connection refused (os error 111)").as_deref(), Some("Connection refused (os error 111)"));
        assert_eq!(failure_detail("  "), None);
    }
}
