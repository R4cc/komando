//! Restore tabs on start: the main window's terminal tabs are written to `tabs.toml` as they
//! change, and reopened when Komando next starts.

use super::*;
use crate::model::{OpenTabs, SavedTab};

impl Workspace {
    /// The terminal tabs open now, in tab order. Remote Desktops aren't kept.
    pub(super) fn current_tabs(&self) -> OpenTabs {
        let mut tabs = OpenTabs::default();
        for entry in &self.sessions {
            if entry.ui.view.terminal().is_none() {
                continue;
            }
            let name = entry.name().as_ref().map(|n| n.to_string());
            let tab = match (&entry.kind(), &entry.item()) {
                (SessionKind::Ssh { .. }, ItemKey::Host { project, host }) => {
                    SavedTab::Host { project: project.clone(), host: host.clone(), name }
                }
                (SessionKind::Local { .. }, ItemKey::Shell { project, name: shell }) => {
                    SavedTab::Shell { project: project.clone(), shell: shell.clone(), name }
                }
                _ => continue,
            };
            if self.active == Some(entry.id()) {
                tabs.active = Some(tabs.tabs.len());
            }
            tabs.tabs.push(tab);
        }
        tabs
    }

    /// Write the tabs when they changed since last written. Called on every frame while
    /// Restore tabs on start is on, so every way a tab opens, closes or is renamed counts.
    pub(super) fn remember_tabs(&mut self) {
        let Some(saved) = &self.tab_memory else { return };
        let now = self.current_tabs();
        if *saved == now {
            return;
        }
        if let Err(err) = now.save(&self.data_dir) {
            log::warn!("Could not remember the open tabs: {err:#}");
        }
        self.tab_memory = Some(now);
    }

    /// The Settings switch: on starts remembering from the tabs open now; off forgets them.
    pub(super) fn set_restore_tabs(&mut self, on: bool, cx: &mut Context<Self>) {
        self.settings.restore_tabs = on;
        self.save_settings(cx);
        if on {
            // Differs from any real set of tabs, so the next frame writes them.
            self.tab_memory = Some(OpenTabs { tabs: vec![], active: Some(usize::MAX) });
        } else {
            self.tab_memory = None;
            if let Err(err) = OpenTabs::clear(&self.data_dir) {
                log::warn!("Could not forget the open tabs: {err:#}");
            }
        }
        cx.notify();
    }

    /// Reopen the tabs saved when Komando last closed. Hosts that sign in from the Vault
    /// wait for it to be unlocked, as when opened from the sidebar.
    pub(super) fn restore_tabs(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let saved = OpenTabs::load(&self.data_dir);
        let items = self.sidebar_items();
        let mut missing = 0;
        let mut active = None;
        for (index, tab) in saved.tabs.iter().enumerate() {
            let (key, name) = match tab {
                SavedTab::Host { project, host, name } => {
                    (ItemKey::Host { project: project.clone(), host: host.clone() }, name)
                }
                SavedTab::Shell { project, shell, name } => {
                    (ItemKey::Shell { project: project.clone(), name: shell.clone() }, name)
                }
            };
            let Some(item) = items.iter().find(|item| item.key() == key).cloned() else {
                missing += 1;
                continue;
            };
            let before = self.sessions.len();
            match item {
                // The SSH terminal even for a Host that opens with a Remote Desktop.
                SidebarItem::Host { project, host } => self.open_host(&project.id, &host.id, window, cx),
                SidebarItem::Shell { project, shell } => self.open_local(project.map(|p| p.id.clone()), shell, window, cx),
            }
            // Not yet open when waiting for the Vault: that tab comes back without its name.
            if self.sessions.len() > before
                && let Some(id) = self.sessions.iter().last().map(|s| s.id())
            {
                self.sessions.set_name(id, name.clone().map(SharedString::from));
                if saved.active == Some(index) {
                    active = Some(id);
                }
            }
        }
        if let Some(id) = active.or_else(|| self.sessions.iter().next().map(|s| s.id())) {
            self.activate(id, window, cx);
        }
        if missing > 0 {
            let tabs = crate::ui::widgets::plural(missing, "tab");
            self.show_toast(format!("{tabs} couldn't be reopened: the host or shell is no longer saved."), cx);
        }
        self.tab_memory = Some(self.current_tabs());
        cx.notify();
    }
}
