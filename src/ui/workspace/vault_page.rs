//! Settings → Vault: every saved password and key, what signs in with it, and ways to see,
//! copy or delete it. Shown only while the Vault is unlocked.

use super::settings::group;
use super::*;
use crate::model::CredentialId;
use crate::vault::Secret;

/// A copied secret is taken off the clipboard after this, if it is still there.
const CLIPBOARD_CLEAR: Duration = Duration::from_secs(30);

impl Workspace {
    pub(super) fn settings_vault(&self, p: AppPalette, cx: &mut Context<Self>) -> AnyElement {
        let Some(vault) = &self.vault else {
            return if self.vault_exists {
                vault_message(
                    "lock",
                    "The vault is locked",
                    "Unlock it with your master password to see what's saved.",
                    Some(
                        button("vault-page-unlock", "Unlock", ButtonKind::Primary, p)
                            .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                                this.show_unlock("Unlock to see your saved passwords and keys.", window, cx)
                            }))
                            .into_any_element(),
                    ),
                    p,
                )
            } else {
                nothing_saved(p)
            };
        };
        let mut items: Vec<_> = vault.list().collect();
        if items.is_empty() {
            return nothing_saved(p);
        }
        items.sort_by_cached_key(|(_, c)| c.label.to_lowercase());

        let rows = items
            .into_iter()
            .map(|(id, credential)| {
                let shown = self.vault_revealed.contains(id);
                let uses = self.library.credential_uses(id);
                credential_row(id, &credential.label, &credential.secret, &uses, shown, p, cx)
            })
            .collect();

        div()
            .flex()
            .flex_col()
            .gap(px(14.))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(12.))
                    .child(
                        div()
                            .flex_1()
                            .text_size(px(13.))
                            .text_color(p.text_muted)
                            .child(crate::ui::widgets::plural(vault.list().count(), "saved item")),
                    )
                    .when(!self.vault_revealed.is_empty(), |el| {
                        el.child(
                            button("vault-hide-all", "Hide all", ButtonKind::Ghost, p).on_click(cx.listener(
                                |this, _: &ClickEvent, _, cx| {
                                    this.vault_revealed.clear();
                                    cx.notify();
                                },
                            )),
                        )
                    })
                    .child(
                        button("vault-page-lock", "Lock now", ButtonKind::Secondary, p)
                            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.lock_vault(cx))),
                    ),
            )
            .child(group(p, rows))
            .into_any_element()
    }

    fn toggle_revealed(&mut self, id: &str, cx: &mut Context<Self>) {
        if !self.vault_revealed.remove(id) {
            self.vault_revealed.insert(id.to_string());
        }
        cx.notify();
    }

    /// Copy a password, or a key's text, and take it off the clipboard again shortly after
    /// unless something else was copied since.
    fn copy_secret(&mut self, id: &str, cx: &mut Context<Self>) {
        let Some(credential) = self.vault.as_ref().and_then(|v| v.get(id)) else { return };
        let (text, what) = match &credential.secret {
            Secret::Password { password } => (password.clone(), "Password"),
            Secret::PrivateKey { key, .. } => (key.clone(), "Key"),
        };
        cx.write_to_clipboard(gpui::ClipboardItem::new_string(text.clone()));
        self.show_toast(format!("{what} copied. It's cleared from the clipboard in 30 seconds."), cx);
        let text = zeroize::Zeroizing::new(text);
        cx.spawn(async move |_, cx| {
            cx.background_executor().timer(CLIPBOARD_CLEAR).await;
            let _ = cx.update(|cx| {
                if cx.read_from_clipboard().and_then(|item| item.text()).is_some_and(|now| now == *text) {
                    cx.write_to_clipboard(gpui::ClipboardItem::new_string(String::new()));
                }
            });
        })
        .detach();
    }

    fn confirm_delete_credential(&mut self, id: CredentialId, cx: &mut Context<Self>) {
        if self.modal_open(cx) {
            return;
        }
        let Some(credential) = self.vault.as_ref().and_then(|v| v.get(&id)) else { return };
        let what = match credential.secret {
            Secret::Password { .. } => "password",
            Secret::PrivateKey { .. } => "key",
        };
        let uses = self.library.credential_uses(&id);
        let body = match uses.as_slice() {
            [] => format!("Nothing signs in with this {what}. This can't be undone."),
            [one] => format!("{one} will ask for a password when connecting instead. This can't be undone."),
            [first, rest @ ..] => format!(
                "{first} and {} will ask for a password when connecting instead. This can't be undone.",
                crate::ui::widgets::plural(rest.len(), "other")
            ),
        };
        self.overlay = Some(Overlay::Confirm {
            title: format!("Delete “{}”?", credential.label),
            body,
            action: ConfirmAction::DeleteCredential(id),
        });
        cx.notify();
    }

    /// Delete a Credential from the Vault, then everything that pointed at it.
    pub(super) fn delete_credential(&mut self, id: &str, cx: &mut Context<Self>) {
        let Some(vault) = self.vault.as_mut() else {
            self.show_toast("The vault was locked, so nothing was deleted.", cx);
            return;
        };
        if let Err(err) = vault.remove(id) {
            self.show_toast(format!("Could not delete: {err:#}"), cx);
            return;
        }
        self.vault_revealed.remove(id);
        let mut library = self.library.clone();
        if library.forget_credential(id) {
            // The Credential is gone either way; a host still pointing at it just asks.
            if let Err(err) = library.save(&self.data_dir) {
                self.show_toast(format!("Deleted, but your hosts could not be updated: {err:#}"), cx);
                return;
            }
            self.library = library;
        }
        self.show_toast("Deleted from the vault.", cx);
        cx.notify();
    }
}

fn credential_row(
    id: &CredentialId,
    label: &str,
    secret: &Secret,
    uses: &[String],
    shown: bool,
    p: AppPalette,
    cx: &mut Context<Workspace>,
) -> AnyElement {
    let (icon, kind, noun) = match secret {
        Secret::Password { .. } => ("key-round", "Password", "password"),
        Secret::PrivateKey { .. } => ("file-key", "Private key", "key"),
    };
    let detail = if uses.is_empty() { format!("{kind} · Not used") } else { format!("{kind} · {}", uses.join(", ")) };
    let (show_id, copy_id, delete_id) = (id.clone(), id.clone(), id.clone());
    let tile = div()
        .size(px(32.))
        .flex_shrink_0()
        .flex()
        .items_center()
        .justify_center()
        .rounded(p.corner(8.))
        .bg(p.muted)
        .child(icon_el(icon, 16., p.text_muted));
    let text = div()
        .flex_1()
        .min_w_0()
        .flex()
        .flex_col()
        .gap(px(2.))
        .child(div().text_size(px(13.5)).font_weight(FontWeight::MEDIUM).text_color(p.text).truncate().child(label.to_string()))
        .child(div().text_size(px(12.)).text_color(p.text_faint).truncate().child(detail));
    let actions = div()
        .flex()
        .flex_shrink_0()
        .items_center()
        .gap(px(2.))
        .child(
            icon_button(SharedString::from(format!("vault-show-{id}")), if shown { "eye-off" } else { "eye" }, p)
                .aria_label(if shown { format!("Hide {noun}: {label}") } else { format!("Show {noun}: {label}") })
                .tooltip(move |window, cx| Tooltip::new(if shown { "Hide" } else { "Show" }).build(window, cx))
                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| this.toggle_revealed(&show_id, cx))),
        )
        .child(
            icon_button(SharedString::from(format!("vault-copy-{id}")), "copy", p)
                .aria_label(format!("Copy {noun}: {label}"))
                .tooltip(|window, cx| Tooltip::new("Copy").build(window, cx))
                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| this.copy_secret(&copy_id, cx))),
        )
        .child(
            icon_button(SharedString::from(format!("vault-delete-{id}")), "trash", p)
                .aria_label(format!("Delete {noun}: {label}"))
                .hover(move |s| s.bg(p.danger.opacity(0.12)))
                .tooltip(|window, cx| Tooltip::new("Delete").build(window, cx))
                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| this.confirm_delete_credential(delete_id.clone(), cx))),
        );
    div()
        .flex()
        .flex_col()
        .gap(px(10.))
        .child(div().flex().items_center().gap(px(12.)).child(tile).child(text).child(actions))
        .when(shown, |el| el.child(div().pl(px(44.)).child(revealed(id, secret, p))))
        .into_any_element()
}

/// The secret itself, in the terminal's monospace font, under its row.
fn revealed(id: &CredentialId, secret: &Secret, p: AppPalette) -> AnyElement {
    let well = |content: String| {
        div()
            .px(px(10.))
            .py(px(8.))
            .rounded(p.corner(8.))
            .bg(p.input)
            .border_1()
            .border_color(p.border)
            .font_family(p.mono_family)
            .text_size(px(12.5))
            .line_height(px(18.))
            .text_color(p.text)
            .child(content)
    };
    match secret {
        Secret::Password { password } if password.is_empty() => {
            div().text_size(px(12.)).text_color(p.text_faint).child("An empty password.").into_any_element()
        }
        Secret::Password { password } => well(password.clone()).into_any_element(),
        Secret::PrivateKey { key, passphrase } => div()
            .flex()
            .flex_col()
            .gap(px(6.))
            .child(
                well(key.trim_end().to_string())
                    .id(SharedString::from(format!("vault-key-{id}")))
                    .max_h(px(180.))
                    .overflow_y_scroll(),
            )
            .child(
                div().text_size(px(12.)).text_color(p.text_muted).child(match passphrase {
                    Some(phrase) if !phrase.is_empty() => format!("Passphrase: {phrase}"),
                    _ => "No passphrase.".to_string(),
                }),
            )
            .into_any_element(),
    }
}

fn nothing_saved(p: AppPalette) -> AnyElement {
    vault_message(
        "key-round",
        "Nothing saved yet",
        "Tick Remember when you type a password, or choose a key file in a host's form. What you save is kept here, encrypted with your master password.",
        None,
        p,
    )
}

/// The page's empty and locked states: an icon over a short explanation.
fn vault_message(icon: &str, title: &str, hint: &str, action: Option<AnyElement>, p: AppPalette) -> AnyElement {
    group(
        p,
        vec![
            div()
                .py(px(24.))
                .flex()
                .flex_col()
                .items_center()
                .gap(px(10.))
                .child(
                    div()
                        .size(px(40.))
                        .flex()
                        .items_center()
                        .justify_center()
                        .rounded(p.corner(10.))
                        .bg(p.muted)
                        .child(icon_el(icon, 18., p.text_muted)),
                )
                .child(div().text_size(px(14.)).font_weight(FontWeight::MEDIUM).text_color(p.text).child(title.to_string()))
                .child(
                    div()
                        .max_w(px(380.))
                        .text_center()
                        .text_size(px(12.5))
                        .line_height(px(18.))
                        .text_color(p.text_muted)
                        .child(hint.to_string()),
                )
                .children(action.map(|a| div().pt(px(4.)).child(a)))
                .into_any_element(),
        ],
    )
}
