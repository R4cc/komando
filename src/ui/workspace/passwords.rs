//! Saving what a Session asks to remember (user names, passwords), and its output's
//! exports.

use super::*;

impl Workspace {
    // ---- passwords & output -----------------------------------------------------------

    /// Save a user name typed for one hop of a Session: as the Host's own, or for the Jump
    /// Host where it is saved.
    pub(super) fn save_user(&mut self, session_id: usize, hop: usize, user: String, cx: &mut Context<Self>) {
        let mut library = self.library.clone();
        let saved = match self.hop_target(session_id, hop) {
            Some(HopTarget::Jump(jump)) => library.jump_host_mut(&jump.owner).map(|j| j.user = Some(user)),
            Some(HopTarget::Host { host_id, .. }) => {
                library.hosts.iter_mut().find(|h| h.id == host_id).map(|h| h.user = model::Inherited::Own(user))
            }
            None => None,
        };
        if saved.is_none() {
            return;
        }
        match self.save_library(&library) {
            Ok(_) => {
                self.library = library;
                self.show_toast("User name saved.", cx);
            }
            Err(err) => self.show_toast(format!("Could not save: {err}"), cx),
        }
    }

    /// Save a password typed for a Jump Host on a Session's way, where that Jump Host is
    /// saved. It gets a Credential of its own; the one it had goes once nothing uses it.
    pub(super) fn save_jump_password(&mut self, jump: model::RouteJump, password: String, cx: &mut Context<Self>) {
        let Some(vault) = self.vault.as_mut() else { return };
        let label = format!("{} (jump host)", jump.jump.label());
        let cred_id = match vault.add(Credential { label, secret: Secret::Password { password } }) {
            Ok(id) => id,
            Err(err) => return self.show_toast(format!("Could not save: {err}"), cx),
        };
        let mut library = self.library.clone();
        let Some(saved) = library.jump_host_mut(&jump.owner) else {
            let _ = vault.remove(&cred_id);
            return;
        };
        let before = saved.credential.replace(cred_id.clone());
        match self.save_library(&library) {
            Ok(_) => {
                self.library = library;
                self.forget_unused_credentials(before);
                self.show_toast("Password saved to the vault.", cx);
            }
            Err(err) => {
                if let Some(vault) = self.vault.as_mut() {
                    let _ = vault.remove(&cred_id);
                }
                self.show_toast(format!("Could not save the password: {err:#}"), cx);
            }
        }
    }

    /// Take Credentials out of the Vault that nothing in the Library uses any more.
    pub(super) fn forget_unused_credentials(&mut self, ids: impl IntoIterator<Item = model::CredentialId>) {
        let Some(vault) = self.vault.as_mut() else { return };
        for id in ids {
            if self.library.credential_users(&id) == 0 {
                let _ = vault.remove(&id);
            }
        }
    }

    /// Save a password typed for one hop of a Session for that hop's Host.
    pub(super) fn save_password(&mut self, session_id: usize, hop: usize, password: String, cx: &mut Context<Self>) {
        let (project_id, host_id) = match self.hop_target(session_id, hop) {
            Some(HopTarget::Host { project_id, host_id }) => (project_id, host_id),
            Some(HopTarget::Jump(jump)) => return self.save_jump_password(jump, password, cx),
            None => return,
        };
        let Some(resolved) = self.library.resolve(&project_id, &host_id) else { return };
        let Some(vault) = self.vault.as_mut() else { return };
        let label = format!("{}@{}", resolved.user.clone().unwrap_or_default(), resolved.name);
        let credential = Credential { label, secret: Secret::Password { password } };

        // Replace the secret in place when the Host already has its own Credential, so every
        // Host sharing it picks up the new password.
        let existing = match resolved.credential_source {
            CredentialSource::Host => resolved.credential.clone(),
            _ => None,
        };
        let added = existing.is_none();
        let result = match existing {
            Some(id) => vault.replace(&id, credential).map(|_| id),
            None => vault.add(credential),
        };
        let cred_id = match result {
            Ok(id) => id,
            Err(err) => return self.show_toast(format!("Could not save: {err}"), cx),
        };
        // Replacing an existing Credential changes only the Vault, which is already saved.
        // The Host still refers to the same id; no Library write or rollback is needed.
        if !added {
            self.show_toast("Password saved to the vault.", cx);
            return;
        }
        let Some(index) = self.library.hosts.iter().position(|h| h.id == host_id) else { return };
        let before = std::mem::replace(&mut self.library.hosts[index].credential, model::Inherited::Own(cred_id.clone()));
        match self.save_library(&self.library) {
            Ok(_) => self.show_toast("Password saved to the vault.", cx),
            Err(err) => {
                // The Host would lose its new Credential at the next start: undo both, so
                // what is shown is what is saved.
                self.library.hosts[index].credential = before;
                if let Some(vault) = self.vault.as_mut() {
                    let _ = vault.remove(&cred_id);
                }
                self.show_toast(format!("Could not save the password: {err:#}"), cx);
            }
        }
    }

    pub(super) fn copy_output_text(&mut self, cx: &mut Context<Self>) {
        let Some(view) = self.active_entry().and_then(|e| e.ui.view.terminal()) else { return };
        let text = view.read(cx).output_text();
        let lines = text.lines().count();
        cx.write_to_clipboard(gpui::ClipboardItem::new_string(text));
        self.show_toast(format!("Copied {} as text.", crate::ui::widgets::plural(lines, "line")), cx);
    }

    pub(super) fn copy_output_file(&mut self, cx: &mut Context<Self>) {
        let Some(entry) = self.active_entry() else { return };
        let Some(view) = entry.ui.view.terminal() else { return };
        let text = view.read(cx).output_text();
        let name = format!(
            "{}_{}.txt",
            sanitize(entry.label()),
            chrono::Local::now().format("%Y-%m-%d_%H-%M-%S")
        );
        match crate::export::write_and_copy_file(&name, &text, cx) {
            Ok(note) => self.show_toast(format!("Copied {name}{note}"), cx),
            Err(err) => self.show_toast(format!("Could not create file: {err:#}"), cx),
        }
    }

    pub(super) fn show_toast(&mut self, message: impl Into<SharedString>, cx: &mut Context<Self>) {
        self.show_toast_for(message, Duration::from_secs(4), cx);
    }

    /// Save `library` as the user's Hosts and Projects, unless their file couldn't be read or
    /// kept aside at start: saving would replace their only copy.
    pub(super) fn save_library(&self, library: &Library) -> anyhow::Result<()> {
        if let Some(path) = &self.library_read_only {
            anyhow::bail!("{} couldn't be read at start, so Komando won't save over it. Fix or move it, then restart Komando", path.display());
        }
        library.save(&self.data_dir)
    }

    pub(super) fn show_toast_for(&mut self, message: impl Into<SharedString>, duration: Duration, cx: &mut Context<Self>) {
        let shown = Instant::now();
        self.toast = Some((message.into(), shown));
        cx.notify();
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(duration).await;
            let _ = this.update(cx, |this, cx| {
                // Unless a newer toast replaced it.
                if this.toast.as_ref().is_some_and(|(_, at)| *at == shown) {
                    this.toast = None;
                    cx.notify();
                }
            });
        })
        .detach();
    }

    /// Keep the sidebar and its saved expansion state in one place.
    pub(super) fn set_project_collapsed(&mut self, project: &str, collapsed: bool, cx: &mut Context<Self>) {
        let changed = if collapsed {
            self.settings.collapsed_projects.insert(project.to_owned())
        } else {
            self.settings.collapsed_projects.remove(project)
        };
        if changed {
            self.save_settings(cx);
            cx.notify();
        }
    }

    /// Save the settings, and say so when that fails instead of dropping the change silently.
    pub(super) fn save_settings(&mut self, cx: &mut Context<Self>) {
        if let Some(path) = &self.settings_read_only {
            let message = format!("Settings aren't saved until Komando restarts: {} couldn't be read.", path.display());
            self.show_toast(message, cx);
        } else if let Err(err) = self.settings.save(&self.data_dir) {
            self.show_toast(format!("Could not save settings: {err:#}"), cx);
        }
    }
}
