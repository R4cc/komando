//! Remote Desktop Sessions: opening a Host's RDP or VNC desktop, connecting it (straight or
//! through the Host's SSH Session), and the title bar's actions on a desktop.

use gpui::{AnyElement, ClickEvent, Context, SharedString, Window, prelude::*};
use zeroize::Zeroizing;

use super::{ItemKey, Pending, SessionEntry, SessionKind, SessionView, Workspace, strip_action};
use crate::desktop::{self, Route, Target};
use crate::model::Protocol;
use crate::session::Status;
use crate::theme::AppPalette;
use crate::ui::desktop_view::{DesktopView, DesktopViewEvent};
use crate::ui::widgets::Busy;
use crate::vault::{Credential, Secret};

impl Workspace {
    /// Show a Host's Remote Desktop: its Session if it has one (reconnecting it if it ended),
    /// or a new one.
    pub(super) fn open_desktop(
        &mut self,
        project_id: &str,
        host_id: &str,
        protocol: Protocol,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let item = ItemKey::Host { project: project_id.to_string(), host: host_id.to_string() };
        let existing = self.sessions.iter().find(|s| s.item == item && desktop_protocol(s) == Some(protocol));
        if let Some((id, live)) = existing.map(|s| (s.id, s.is_live())) {
            self.activate(id, window, cx);
            if !live {
                self.connect_desktop(id, window, cx);
            }
            return;
        }
        let Some(host) = self.library.host(host_id) else { return };
        if host.desktop(protocol).is_none() {
            return;
        }
        let id = self.add_desktop_session(item, project_id, host_id, protocol, window, cx);
        self.connect_desktop(id, window, cx);
    }

    fn add_desktop_session(
        &mut self,
        item: ItemKey,
        project_id: &str,
        host_id: &str,
        protocol: Protocol,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> usize {
        let name = self.library.host(host_id).map(|h| h.name.clone()).unwrap_or_default();
        let taken: std::collections::HashSet<usize> = self.sessions_of(&item).map(|s| s.number).collect();
        let number = (1..).find(|n| !taken.contains(n)).unwrap_or(1);
        let label: SharedString = format!("{name} · {}", protocol.label()).into();
        let target = self.desktop_target(project_id, host_id, protocol);
        let view = cx.new(|cx| DesktopView::new(protocol, name.into(), target.into(), window, cx));
        let id = self.next_id;
        self.next_id += 1;
        let (project, host) = (project_id.to_string(), host_id.to_string());
        let sub = cx.subscribe_in(&view, window, move |this, view, event: &DesktopViewEvent, window, cx| match event {
            DesktopViewEvent::Changed => {
                let status = view.read(cx).status.clone();
                if let Some(entry) = this.sessions.iter_mut().find(|s| s.id == id) {
                    entry.status = status;
                }
                cx.notify();
            }
            DesktopViewEvent::Remember { user, password } => {
                this.remember_desktop_password(&host, protocol, user.clone(), password.clone(), window, cx)
            }
            DesktopViewEvent::Reconnect => this.connect_desktop(id, window, cx),
            DesktopViewEvent::EditHost => this.open_edit_host(project.clone(), host.clone(), window, cx),
            DesktopViewEvent::OpenSsh => this.show_terminal_of(&project, &host, window, cx),
            DesktopViewEvent::Options { reconnect } => {
                let options = view.read(cx).options;
                this.save_viewer_options(&host, protocol, options, cx);
                let live = this.sessions.iter().find(|s| s.id == id).is_some_and(|s| s.is_live());
                if *reconnect && live {
                    this.connect_desktop(id, window, cx);
                }
            }
        });
        self.collapsed_items.remove(&item);
        self.collapsed.remove(project_id);
        let key = item.clone();
        self.sessions.push(SessionEntry {
            id,
            item,
            project_id: Some(project_id.to_string()),
            kind: SessionKind::Desktop { host_id: host_id.to_string(), protocol },
            label: label.clone(),
            number,
            title: label,
            busy: false,
            view: SessionView::Desktop(view),
            status: Status::Connecting,
            browser: None,
            sftp_open: false,
            sftp_busy: Busy::default(),
            waiting_for_size: false,
            history_on: None,
            window: None,
            _subscription: sub,
            _browser_subscription: None,
            _browser_observer: None,
        });
        self.take_pending_split(&key, id);
        self.activate(id, window, cx);
        id
    }

    /// "10.0.4.21:3389", or "port 5900 through SSH".
    pub(super) fn desktop_target(&self, project_id: &str, host_id: &str, protocol: Protocol) -> String {
        let Some(host) = self.library.host(host_id) else { return String::new() };
        let Some(desktop) = host.desktop(protocol) else { return String::new() };
        let user = match protocol {
            Protocol::Rdp => desktop
                .user
                .clone()
                .or_else(|| self.library.resolve(project_id, host_id).and_then(|r| r.user))
                .map(|u| format!("{u}@"))
                .unwrap_or_default(),
            _ => String::new(),
        };
        if desktop.through_ssh {
            format!("{user}{}:{} through SSH", host.address, desktop.port)
        } else {
            format!("{user}{}:{}", host.address, desktop.port)
        }
    }

    /// Connect (or reconnect) a Remote Desktop Session, unlocking the Vault first when its
    /// password is saved there.
    pub(super) fn connect_desktop(&mut self, id: usize, window: &mut Window, cx: &mut Context<Self>) {
        self.connect_desktop_with(id, true, window, cx);
    }

    /// The Vault stayed locked: connect anyway, and the desktop asks for the password.
    pub(super) fn connect_desktop_without_vault(&mut self, id: usize, window: &mut Window, cx: &mut Context<Self>) {
        self.connect_desktop_with(id, false, window, cx);
    }

    fn connect_desktop_with(&mut self, id: usize, use_vault: bool, window: &mut Window, cx: &mut Context<Self>) {
        let Some(entry) = self.sessions.iter().find(|s| s.id == id) else { return };
        let (SessionKind::Desktop { host_id, protocol }, Some(view)) = (entry.kind.clone(), entry.view.desktop().cloned()) else {
            return;
        };
        let (item, project_id) = (entry.item.clone(), entry.project_id.clone().unwrap_or_default());
        let Some(host) = self.library.host(&host_id).cloned() else {
            return view.update(cx, |v, cx| v.end("This host is no longer saved.", cx));
        };
        let Some(remote) = host.desktop(protocol).cloned() else {
            let reason = format!("{} no longer has a {} remote desktop.", host.name, protocol.label());
            return view.update(cx, |v, cx| v.end(&reason, cx));
        };
        if use_vault && remote.credential.is_some() && self.vault.is_none() {
            self.pending.push(Pending::Desktop { session: id });
            self.show_unlock(format!("{}'s remote desktop signs in with a password saved in your vault.", host.name), window, cx);
            return;
        }
        let password = remote.credential.as_deref().and_then(|c| self.vault.as_ref()?.get(c)).and_then(|c| match &c.secret {
            Secret::Password { password } => Some(Zeroizing::new(password.clone())),
            Secret::PrivateKey { .. } => None,
        });
        let resolved = self.library.resolve(&project_id, &host_id);
        let user = match protocol {
            Protocol::Rdp => remote.user.clone().or_else(|| resolved.and_then(|r| r.user)),
            _ => None,
        };
        let route = if remote.through_ssh {
            let sink = self
                .sessions
                .iter()
                .filter(|s| s.item == item && matches!(s.kind, SessionKind::Ssh { .. }) && matches!(s.status, Status::Connected))
                .find_map(|s| s.view.terminal()?.read(cx).sink());
            match sink {
                Some(sink) => Route::Ssh { sink, port: remote.port },
                None => {
                    let reason =
                        format!("This remote desktop goes through {}'s SSH session. Connect it, and the desktop follows.", host.name);
                    return view.update(cx, |v, cx| v.wait_for_ssh(&reason, cx));
                }
            }
        } else {
            Route::Direct { address: host.address.clone(), port: remote.port }
        };
        let (width, height, scale_percent) = view.read(cx).desired_size(window);
        let target = Target {
            protocol,
            address: host.address.clone(),
            port: remote.port,
            route,
            user,
            domain: remote.domain.clone(),
            password,
            size: (width, height),
            scale_percent,
            shared: self.settings.vnc_shared,
            quality: remote.viewer.quality,
            certificates: self.data_dir.join(desktop::trust::FILE),
        };
        let fit = remote.viewer.match_window.unwrap_or(self.settings.rdp_fit_desktop);
        let options = remote.viewer;
        let label = self.desktop_target(&project_id, &host_id, protocol);
        view.update(cx, |v, cx| {
            v.fit_desktop = fit;
            v.set_options(options, cx);
            v.target = label.into();
            v.connect(target, cx);
        });
        self.lock_if_used(cx);
    }

    /// A Host's SSH Session just connected: its Remote Desktops waiting to go through it
    /// connect now. If one was asked for from its own pane, it comes back into view.
    pub(super) fn ssh_connected(&mut self, ssh_id: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(item) = self.sessions.iter().find(|s| s.id == ssh_id).map(|s| s.item.clone()) else { return };
        let waiting: Vec<usize> = self
            .sessions
            .iter()
            .filter(|s| s.item == item && s.view.desktop().is_some_and(|v| v.read(cx).waiting_for_ssh))
            .map(|s| s.id)
            .collect();
        for &id in &waiting {
            self.connect_desktop(id, window, cx);
        }
        if let [id] = waiting[..]
            && self.active == Some(ssh_id)
        {
            self.activate(id, window, cx);
        }
    }

    /// Show a Host's terminal: its SSH Session, reconnecting it if it ended, or a new one.
    pub(super) fn show_terminal_of(&mut self, project_id: &str, host_id: &str, window: &mut Window, cx: &mut Context<Self>) {
        let item = ItemKey::Host { project: project_id.to_string(), host: host_id.to_string() };
        let ssh = self
            .last_active
            .get(&item)
            .copied()
            .filter(|id| self.sessions.iter().any(|s| s.id == *id && matches!(s.kind, SessionKind::Ssh { .. })))
            .or_else(|| self.sessions_of(&item).find(|s| matches!(s.kind, SessionKind::Ssh { .. })).map(|s| s.id));
        match ssh {
            Some(id) => {
                self.activate(id, window, cx);
                if self.sessions.iter().find(|s| s.id == id).is_some_and(|s| !s.is_live()) {
                    self.reconnect(id, window, cx);
                }
            }
            None => self.open_host(project_id, host_id, window, cx),
        }
    }

    /// Keep a desktop's toolbar options for its Host.
    fn save_viewer_options(&mut self, host_id: &str, protocol: Protocol, options: crate::model::ViewerOptions, cx: &mut Context<Self>) {
        let Some(index) = self.library.hosts.iter().position(|h| h.id == host_id) else { return };
        let Some(remote) = self.library.hosts[index].desktop_mut(protocol) else { return };
        if remote.viewer == options {
            return;
        }
        let before = remote.viewer;
        remote.viewer = options;
        if let Err(err) = self.library.save(&self.data_dir) {
            if let Some(remote) = self.library.hosts[index].desktop_mut(protocol) {
                remote.viewer = before;
            }
            self.show_toast(format!("Could not save the desktop's options: {err:#}"), cx);
        }
    }

    fn remember_desktop_password(
        &mut self,
        host_id: &str,
        protocol: Protocol,
        user: Option<String>,
        password: Zeroizing<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.vault.is_some() {
            return self.save_desktop_password(host_id, protocol, user, password, cx);
        }
        let name = self.library.host(host_id).map(|h| h.name.clone()).unwrap_or_default();
        self.pending.push(Pending::SaveDesktop { host_id: host_id.to_string(), protocol, user, password });
        self.show_unlock(format!("To remember the password for {name}'s remote desktop, Komando saves it in your vault."), window, cx);
    }

    /// Save a Remote Desktop's password in the Vault (in place of the one it had), and for RDP
    /// the account it signed in with.
    pub(super) fn save_desktop_password(
        &mut self,
        host_id: &str,
        protocol: Protocol,
        user: Option<String>,
        password: Zeroizing<String>,
        cx: &mut Context<Self>,
    ) {
        let Some(index) = self.library.hosts.iter().position(|h| h.id == host_id) else { return };
        let Some(remote) = self.library.hosts[index].desktop(protocol).cloned() else { return };
        let Some(vault) = self.vault.as_mut() else { return };
        let host = &self.library.hosts[index];
        let label = match &user {
            Some(user) => format!("{user}@{} ({})", host.name, protocol.label()),
            None => format!("{} ({})", host.name, protocol.label()),
        };
        let credential = Credential { label, secret: Secret::Password { password: password.to_string() } };
        let (result, added) = match &remote.credential {
            Some(id) => (vault.replace(id, credential).map(|_| id.clone()), false),
            None => (vault.add(credential), true),
        };
        let credential_id = match result {
            Ok(id) => id,
            Err(err) => return self.show_toast(format!("Could not save: {err}"), cx),
        };
        let before = self.library.clone();
        if let Some(remote) = self.library.hosts[index].desktop_mut(protocol) {
            remote.credential = Some(credential_id.clone());
            // A successful retry may use a different account from the saved one.
            if protocol == Protocol::Rdp && user.is_some() {
                remote.user = user;
            }
        }
        if !added && self.library == before {
            return self.show_toast("Password saved to the vault.", cx);
        }
        match self.library.save(&self.data_dir) {
            Ok(()) => self.show_toast("Password saved to the vault.", cx),
            Err(err) => {
                self.library = before;
                if added && let Some(vault) = self.vault.as_mut() {
                    let _ = vault.remove(&credential_id);
                }
                self.show_toast(format!("Could not save the password: {err:#}"), cx);
            }
        }
    }

    /// The title bar's actions on a Remote Desktop: Reconnect once it has dropped, and its own
    /// window. Keys, scaling and quality are in the desktop's own toolbar. Moving between a Host's terminal and its desktops is done
    /// from the sidebar, so the bar keeps its layout.
    pub(super) fn desktop_strip_buttons(&self, entry: &SessionEntry, p: AppPalette, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let Some(protocol) = desktop_protocol(entry) else { return vec![] };
        let id = entry.id;
        let _ = protocol;
        let mut buttons = vec![];
        if !entry.is_live() {
            buttons.push(
                strip_action("desktop-reconnect", "refresh-cw", "Reconnect", false, p)
                    .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| this.connect_desktop(id, window, cx)))
                    .into_any_element(),
            );
        }
        buttons.push(
            strip_action("desktop-own-window", "app-window", "Open in its own window", false, p)
                .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| this.detach_desktop(id, None, window, cx)))
                .into_any_element(),
        );
        buttons
    }
}

/// The Remote Desktop a Session shows, if it is one.
pub(super) fn desktop_protocol(entry: &SessionEntry) -> Option<Protocol> {
    match &entry.kind {
        SessionKind::Desktop { protocol, .. } => Some(*protocol),
        _ => None,
    }
}
