//! Locking and unlocking the Vault, and the sidebar's vault row with its unlock popover.

use super::*;

/// Under a new master password, the one sentence that must not be skimmed past.
const NO_RECOVERY: &str = "There is no recovery: forget it and saved passwords are lost. Projects and hosts are kept.";

/// A line under a password field, with its icon: an error, or what the password is for.
pub(super) fn field_note(icon: &str, text: impl IntoElement, color: Hsla) -> gpui::Div {
    div()
        .flex()
        .items_start()
        .gap(px(6.))
        .text_size(px(12.))
        .line_height(px(17.))
        .text_color(color)
        .child(div().flex_shrink_0().pt(px(1.)).child(icon_el(icon, 13., color)))
        .child(div().flex_1().min_w_0().child(text))
}

/// The warning box with [`NO_RECOVERY`], for setting a master password.
pub(super) fn no_recovery_warning(p: AppPalette) -> gpui::Div {
    div()
        .flex()
        .items_start()
        .gap(px(8.))
        .px(px(10.))
        .py(px(8.))
        .rounded(p.corner(8.))
        .bg(p.warning.opacity(0.1))
        .border_1()
        .border_color(p.warning.opacity(0.3))
        .text_size(px(12.))
        .line_height(px(17.))
        .text_color(p.text)
        .child(div().flex_shrink_0().pt(px(1.)).child(icon_el("triangle-alert", 13., p.warning)))
        .child(div().flex_1().min_w_0().child(NO_RECOVERY))
}

impl Workspace {
    // ---- vault ------------------------------------------------------------------------

    /// Ask for the master password, saying why (`reason`). What waits for it is in `pending`.
    /// Asked again while the dialog is open (two hosts opened at once), it stays as it is.
    pub(super) fn show_unlock(&mut self, reason: impl Into<String>, window: &mut Window, cx: &mut Context<Self>) {
        // The password popover is anchored to a row in the sidebar.
        self.sidebar_visible = true;
        if let Some(state) = self.unlock.as_mut() {
            if self.pending.len() > 1 {
                state.reason = "The hosts you're opening sign in with passwords or keys saved in your vault.".into();
            }
            let password = state.password.clone();
            password.update(cx, |input, cx| input.focus(window, cx));
            cx.notify();
            return;
        }
        self.vault_exists = Vault::exists(&self.data_dir);
        let creating = !self.vault_exists;
        let password = cx.new(|cx| {
            InputState::new(window, cx)
                .masked(true)
                .placeholder(if creating { "New master password" } else { "Master password" })
        });
        let confirm = cx.new(|cx| InputState::new(window, cx).masked(true).placeholder("Repeat master password"));
        for input in [&password, &confirm] {
            let sub = cx.subscribe_in(input, window, |this, _, event: &InputEvent, window, cx| {
                match event {
                    InputEvent::PressEnter { .. } => this.submit_unlock(window, cx),
                    // Typing again answers "wrong password".
                    InputEvent::Change => {
                        if let Some(state) = this.unlock.as_mut()
                            && state.error.take().is_some()
                        {
                            cx.notify();
                        }
                    }
                    _ => {}
                }
            });
            self._subscriptions.push(sub);
        }
        password.update(cx, |state, cx| state.focus(window, cx));
        let focus = cx.focus_handle();
        self.unlock = Some(UnlockState { creating, focus, password, confirm, error: None, reason: reason.into() });
        cx.notify();
    }

    fn submit_unlock(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(state) = self.unlock.as_mut() else { return };
        let password = state.password.read(cx).value().to_string();
        let result = if state.creating {
            let confirm = state.confirm.read(cx).value().to_string();
            if password != confirm {
                Err(anyhow::anyhow!("The two passwords don't match."))
            } else {
                Vault::create(&self.data_dir, &password)
            }
        } else {
            Vault::unlock(&self.data_dir, &password)
        };
        match result {
            Ok(vault) => {
                self.vault = Some(vault);
                self.vault_exists = true;
                self.unlock = None;
                self.last_activity = SystemTime::now();
                let pending = std::mem::take(&mut self.pending);
                self.lock_after_use = !pending.is_empty() && !self.settings.stay_unlocked;
                self.sync_remembered_unlock_with_notice(cx);
                if pending.is_empty() {
                    self.focus_active(window, cx);
                }
                for job in pending {
                    match job {
                        Pending::Open { project_id, host_id } => self.open_host(&project_id, &host_id, window, cx),
                        Pending::Reconnect { session } => self.reconnect(session, window, cx),
                        Pending::Desktop { session } => self.connect_desktop(session, window, cx),
                        Pending::SaveDesktop { host_id, protocol, user, password } => {
                            self.save_desktop_password(&host_id, protocol, user, password, cx)
                        }
                        Pending::Save { session, hop, password } => self.save_password(session, hop, password, cx),
                        Pending::Form => self.submit_form(window, cx),
                    }
                }
                self.lock_if_used(cx);
            }
            Err(err) => {
                // A vault created elsewhere since the dialog opened: ask for its password now.
                if state.creating && Vault::exists(&self.data_dir) {
                    state.creating = false;
                }
                state.error = Some(err.to_string());
            }
        }
        cx.notify();
    }

    /// "Not now", ✕ or Escape on the master password popover, or a second click on the vault row. A connection still goes ahead
    /// and asks for the server's password instead; anything else is dropped.
    pub(super) fn skip_unlock(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.unlock = None;
        for job in std::mem::take(&mut self.pending) {
            match job {
                Pending::Open { project_id, host_id } => self.open_host_session(&project_id, &host_id, window, cx),
                Pending::Reconnect { session } => self.reconnect_with(session, false, window, cx),
                // Connects without the saved password: the desktop asks for one.
                Pending::Desktop { session } => self.connect_desktop_without_vault(session, window, cx),
                Pending::Form => self.show_form_error(
                    "Key files and jump host passwords go into the vault: unlock it, or leave them out.",
                    cx,
                ),
                // "Remember" was ticked: say why nothing was remembered.
                Pending::Save { .. } | Pending::SaveDesktop { .. } => {
                    self.show_toast("The password wasn't saved, as the vault stayed locked. Komando asks for it next time.", cx)
                }
            }
        }
        if self.overlay.is_none() {
            self.focus_active(window, cx);
        }
        cx.notify();
    }

    /// With "Stay unlocked" off, lock the Vault once what waited for it has read it: saving
    /// and imports do at once, a new Session when it starts (see `start_when_sized`).
    pub(super) fn lock_if_used(&mut self, cx: &mut Context<Self>) {
        if self.lock_after_use && !self.sessions.iter().any(|s| s.ui.waiting_for_size) {
            self.lock_after_use = false;
            self.vault = None;
            self.vault_revealed.clear();
            self.clear_remembered_unlock(cx);
            cx.notify();
        }
    }

    pub(super) fn lock_vault(&mut self, cx: &mut Context<Self>) {
        self.lock_after_use = false;
        self.vault_revealed.clear();
        if self.vault.take().is_some() {
            self.vault_exists = true;
            self.show_toast("Vault locked. Open sessions stay connected.", cx);
        }
        self.clear_remembered_unlock(cx);
        cx.notify();
    }

    fn clear_remembered_unlock(&mut self, cx: &mut Context<Self>) {
        self.remembered_unlock = None;
        if let Err(err) = Vault::forget_unlock(&self.data_dir) {
            log::warn!("{err:#}");
            self.show_toast("Vault locked in this window, but its saved unlock could not be removed. Check folder permissions.", cx);
        }
    }

    pub(super) fn sync_remembered_unlock(&mut self) -> anyhow::Result<()> {
        let minutes = if self.settings.stay_unlocked && !self.lock_after_use {
            self.settings.auto_lock_minutes
        } else { 0 };
        let Some(vault) = &self.vault else { return Ok(()) };
        let current = (self.last_activity, minutes);
        if self.remembered_unlock != Some(current) {
            vault.remember_unlock(minutes, self.last_activity)?;
            self.remembered_unlock = Some(current);
        }
        Ok(())
    }

    pub(super) fn sync_remembered_unlock_with_notice(&mut self, cx: &mut Context<Self>) {
        if let Err(err) = self.sync_remembered_unlock() {
            log::warn!("Could not remember vault unlock: {err:#}");
            let system = if cfg!(windows) { "Windows" } else { "Komando" };
            self.show_toast(format!("Vault unlocked for this window, but {system} could not remember it across restarts."), cx);
        }
    }

    pub(super) fn start_auto_lock(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        cx.spawn_in(window, async move |this, cx| {
            loop {
                cx.background_executor().timer(Duration::from_secs(20)).await;
                let alive = this.update(cx, |this, cx| {
                    let minutes = this.settings.auto_lock_minutes;
                    if minutes > 0
                        && this.vault.is_some()
                        && this.last_activity.elapsed().map_or(true, |idle| idle >= Duration::from_secs(minutes as u64 * 60))
                    {
                        this.lock_vault(cx);
                    } else if let Err(err) = this.sync_remembered_unlock() {
                        log::warn!("Could not refresh remembered vault unlock: {err:#}");
                    }
                });
                if alive.is_err() {
                    break;
                }
            }
        })
        .detach();
    }
}

impl Workspace {
    /// The sidebar's vault row, above Transfers: there while the Vault is locked, gone once
    /// it is unlocked. A click opens the master password popover out of it (a second click
    /// closes it), and anything else that needs the Vault opens the popover here too. Before a
    /// Vault exists the row shows only while the popover asks for a new master password.
    pub(super) fn render_vault_row(&self, compact: bool, p: AppPalette, cx: &mut Context<Self>) -> Option<AnyElement> {
        let open = self.unlock.as_ref();
        if open.is_none() && (self.vault.is_some() || !self.vault_exists) {
            return None;
        }
        let creating = open.is_some_and(|state| state.creating);
        let label = if creating { "Set up vault" } else { "Vault locked" };
        let row = div()
            .id("vault-row")
            .role(gpui::Role::Button)
            .aria_label(if open.is_some() { label.to_string() } else { format!("{label}, unlock") })
            .tab_index(0)
            .focus_visible(move |s| crate::ui::widgets::focus_outline(s, p.focus))
            .relative()
            .h(px(34.))
            .px(px(10.))
            .flex()
            .items_center()
            .gap(px(8.))
            .rounded(p.corner(8.))
            .border_1()
            .border_color(p.brand.opacity(if open.is_some() { 0.6 } else { 0.3 }))
            .bg(p.brand_soft)
            .cursor_pointer()
            .text_size(px(12.5))
            .font_weight(FontWeight::MEDIUM)
            .text_color(p.active_text)
            .hover(|s| s.bg(p.brand_soft))
            .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                if this.unlock.is_some() {
                    this.skip_unlock(window, cx);
                } else {
                    this.show_unlock("Unlock to connect with your saved passwords and keys.", window, cx);
                }
            }))
            .child(icon_el("lock-keyhole", 14., p.active_text))
            .child(div().flex_1().min_w_0().truncate().child(label))
            .when(open.is_none() && !compact, |el| el.child(div().flex_shrink_0().whitespace_nowrap().text_size(px(11.5)).text_color(p.active_text).child("Unlock")))
            .child(icon_el("chevron-right", 14., p.active_text))
            .children(open.map(|state| self.render_unlock(state, p, cx)));
        Some(row.into_any_element())
    }

    /// The master password popover, drawn upward from the vault row's right edge over the
    /// terminal area, its bottom level with the row. It is not a modal: there is no backdrop and
    /// the rest of the window keeps working. It must not be mistaken for a server's password
    /// question (a plain card titled "Password for …"): it carries the vault badge and a
    /// brand-coloured top edge, names Komando's vault, and says under the field that this is
    /// not the server's password.
    fn render_unlock(&self, state: &UnlockState, p: AppPalette, cx: &mut Context<Self>) -> AnyElement {
        let p = p.panel();
        let creating = state.creating;
        let waiting = !self.pending.is_empty();
        let skip_label = if self.pending.iter().any(|job| matches!(job, Pending::Open { .. } | Pending::Reconnect { .. })) {
            "Use server password"
        } else {
            "Not now"
        };
        let header = div()
            .flex()
            .items_start()
            .gap(px(12.))
            .child(badge("lock-keyhole", p.brand, 36.))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .gap(px(3.))
                    .child(
                        div()
                            .text_size(px(10.5))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(p.brand)
                            .child("KOMANDO VAULT"),
                    )
                    .child(
                        div()
                            .text_size(px(15.))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(p.text)
                            .child(if creating { "Create a master password" } else { "Unlock your vault" }),
                    )
                    .child(
                        div()
                            .pt(px(2.))
                            .text_size(px(12.5))
                            .line_height(px(18.))
                            .text_color(p.text_muted)
                            .child(state.reason.clone()),
                    ),
            )
            // Same as the skip button or Escape.
            .child(
                close_x("unlock-close", p)
                    .flex_shrink_0()
                    .mt(px(-4.))
                    .mr(px(-6.))
                    .on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.skip_unlock(window, cx))),
            );

        let form = div()
            .flex()
            .flex_col()
            .gap(px(8.))
            // Inset by the focus ring's width, so the ring lines up with the card below
            // instead of reaching past it.
            .child(div().px(px(3.)).child(password_input(&state.password, p)))
            .when(creating, |el| el.child(div().px(px(3.)).child(password_input(&state.confirm, p))))
            // Under the field, one line that keeps it apart from a server's password question.
            .child(match &state.error {
                Some(error) => field_note("circle-alert", error.clone(), p.danger),
                None if creating => field_note(
                    "key-round",
                    "Protects the passwords and keys Komando saves. It is not a server's password.",
                    p.text_faint,
                ),
                None => field_note("key-round", "The password you chose for Komando, not the server's.", p.text_faint),
            });

        // The one sentence that must not be skimmed past.
        let warning = creating.then(|| no_recovery_warning(p));

        // Stay unlocked for the auto-lock time, or lock again once what waits has used the
        // Vault. Only offered when something waits: unlocking from the row means to stay.
        let stay = self.settings.stay_unlocked;
        let stay_detail = if !stay {
            "Locks again right after this".to_string()
        } else {
            match self.settings.auto_lock_minutes {
                0 => "Until Komando closes".to_string(),
                minutes if cfg!(windows) => format!("Locks after {} without use, including across restarts", settings::auto_lock_label(minutes)),
                minutes => format!("Locks after {} without use", settings::auto_lock_label(minutes)),
            }
        };
        let remember = waiting.then(|| {
            div()
                .id("unlock-stay")
                .flex()
                .items_center()
                .gap(px(10.))
                .px(px(10.))
                .py(px(8.))
                .rounded(p.corner(8.))
                .bg(p.hover)
                .cursor_pointer()
                .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                    this.settings.stay_unlocked = !this.settings.stay_unlocked;
                    this.save_settings(cx);
                    this.sync_remembered_unlock_with_notice(cx);
                    cx.notify();
                }))
                .child(switch("unlock-stay-switch".into(), "Stay unlocked", stay, p))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .flex()
                        .flex_col()
                        .child(div().text_size(px(12.5)).font_weight(FontWeight::MEDIUM).text_color(p.text).child("Stay unlocked"))
                        .child(div().text_size(px(11.5)).text_color(p.text_faint).child(stay_detail)),
                )
                .child(
                    div()
                        .id("unlock-stay-change")
                        .flex_shrink_0()
                        .text_size(px(11.5))
                        .text_color(p.text_muted)
                        .cursor_pointer()
                        .hover(|s| s.text_color(p.brand))
                        .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                            cx.stop_propagation();
                            this.open_settings(SettingsSection::Security, window, cx);
                        }))
                        .child("Change"),
                )
        });

        let actions = div()
            .flex()
            .items_center()
            .gap(px(8.))
            .px(px(20.))
            .py(px(12.))
            .border_t_1()
            .border_color(p.border)
            .child(
                // Its text lines up with the content above; the hover background reaches out.
                button("unlock-skip", skip_label, ButtonKind::Ghost, p)
                    .ml(px(-12.))
                    .on_click(cx.listener(|this, _, window, cx| this.skip_unlock(window, cx))),
            )
            .child(div().flex_1())
            .child(
                button("unlock", if creating { "Create vault" } else { "Unlock" }, ButtonKind::Primary, p)
                    .on_click(cx.listener(|this, _, window, cx| this.submit_unlock(window, cx))),
            );

        let card = div()
            .id("unlock-popover")
            .track_focus(&state.focus)
            .role(gpui::Role::Dialog)
            .aria_label("Komando vault")
            // Clicks on it must not reach the terminal underneath (it would take the focus).
            .occlude()
            .relative()
            .w(px(380.))
            .rounded(p.corner(14.))
            .bg(p.surface)
            .border_1()
            .border_color(p.border)
            .child(
                div()
                    .rounded(p.corner(14.))
                    // A brand-coloured top edge, which the plain server questions don't have.
                    .border_t_4()
                    .border_color(p.brand)
                    .child(
                        div()
                            .px(px(20.))
                            .pt(px(16.))
                            .pb(px(16.))
                            .flex()
                            .flex_col()
                            .gap(px(14.))
                            .child(header)
                            .children(warning)
                            .child(form)
                            .children(remember),
                    )
                    .child(actions),
            )
            // Slides out of the row to the right as it fades in.
            .with_animation(
                "unlock-popover-in",
                Animation::new(Duration::from_millis(160)).with_easing(gpui::ease_out_quint()),
                |el, t| el.opacity(t).left(px(-10. * (1. - t))),
            );
        // From the row's right edge: past the sidebar's inner padding and the gap between the
        // islands, so the card starts just inside the terminal area and grows upward from
        // the footer row. It stays in the window when the window is short.
        div()
            .absolute()
            .bottom_0()
            .right_0()
            .size_0()
            .child(
                deferred(
                    anchored()
                        .anchor(gpui::Anchor::BottomLeft)
                        .offset(gpui::point(px(8. + ISLAND_GAP + 8.), px(4.)))
                        .snap_to_window_with_margin(px(ISLAND_GAP))
                        .child(card),
                )
                .with_priority(5),
            )
            .into_any_element()
    }
}
