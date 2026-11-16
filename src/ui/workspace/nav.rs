//! Keyboard use of the sidebar. Ctrl+Shift+H moves the keyboard into it (and back out); a
//! cursor then walks its rows: Up and Down move, Right unfolds (or steps into) a Project or a
//! Host's Sessions, Left folds them (or steps out), Enter connects or shows, Ctrl+Enter opens
//! another Session, F2 edits a Host or Project or renames a Session, Delete asks to delete a
//! Host or Project or closes a Session, and Escape goes back to the Session.

use gpui::{Context, FocusHandle, Window, actions};

use super::{ItemKey, SidebarRow, Workspace};

actions!(sidebar, [FocusSidebar, NavUp, NavDown, NavFirst, NavLast, NavExpand, NavCollapse, NavOpen, NavOpenAnother, NavEdit, NavDelete, NavLeave]);

/// The key context of the sidebar's list, for its arrow, Enter, F2 and Delete bindings.
pub const CONTEXT: &str = "Sidebar";

/// A row the keyboard cursor can be on.
#[derive(Clone, PartialEq, Eq, Debug)]
pub(super) enum NavTarget {
    Project(String),
    Item(ItemKey),
    Session(usize),
    /// The "Add a host" row of an empty Project.
    AddHost(String),
}

pub(super) struct NavState {
    pub focus: FocusHandle,
    pub cursor: Option<NavTarget>,
    /// Whether the keyboard is in the sidebar, as of this frame: the cursor shows only then.
    pub focused: bool,
    /// Scroll the cursor's row into view on the next frame, once its row exists.
    pub reveal: bool,
}

impl NavState {
    pub fn new(cx: &mut Context<Workspace>) -> Self {
        // A tab stop too, so Tab from Quick connect reaches the list.
        Self { focus: cx.focus_handle().tab_index(0).tab_stop(true), cursor: None, focused: false, reveal: false }
    }
}

impl Workspace {
    /// The sidebar's keys are for the list itself. With a button inside it focused (Tab), the
    /// key goes on to that button instead.
    pub(super) fn nav_owns(&self, window: &Window, cx: &mut Context<Self>) -> bool {
        let owns = self.nav.focus.is_focused(window);
        if !owns {
            cx.propagate();
        }
        owns
    }

    /// Whether the keyboard cursor is on this row and should be drawn.
    pub(super) fn nav_focus_on(&self, target: &NavTarget) -> bool {
        self.nav.focused && self.nav.cursor.as_ref() == Some(target)
    }

    /// The rows the cursor can be on, in order, each with the sidebar list row it is drawn in.
    pub(super) fn nav_targets(&self, rows: &[SidebarRow]) -> Vec<(NavTarget, usize)> {
        let shells = self.sidebar_shells();
        let mut out = vec![];
        let item = |out: &mut Vec<(NavTarget, usize)>, key: ItemKey, ix: usize| {
            let expanded = !self.collapsed_items.contains(&key);
            let listed: Vec<usize> = if expanded { self.listed_sessions(&key).iter().map(|s| s.id).collect() } else { vec![] };
            out.push((NavTarget::Item(key), ix));
            out.extend(listed.into_iter().map(|id| (NavTarget::Session(id), ix)));
        };
        for (ix, row) in rows.iter().enumerate() {
            match *row {
                SidebarRow::Shell { index, .. } => {
                    if let Some(shell) = shells.get(index) {
                        item(&mut out, ItemKey::Shell { project: None, name: shell.name.clone() }, ix);
                    }
                }
                SidebarRow::Project { index } => {
                    if let Some(project) = self.library.projects.get(index) {
                        out.push((NavTarget::Project(project.id.clone()), ix));
                    }
                }
                SidebarRow::FirstHost { project } => {
                    if let Some(project) = self.library.projects.get(project) {
                        out.push((NavTarget::AddHost(project.id.clone()), ix));
                    }
                }
                SidebarRow::Host { project, host, .. } => {
                    if let (Some(project), Some(host)) = (self.library.projects.get(project), self.library.hosts.get(host)) {
                        item(&mut out, ItemKey::Host { project: project.id.clone(), host: host.id.clone() }, ix);
                    }
                }
                SidebarRow::ProjectShell { project, shell, .. } => {
                    if let Some(project) = self.library.projects.get(project)
                        && let Some(shell) = project.shells.get(shell)
                    {
                        item(&mut out, ItemKey::Shell { project: Some(project.id.clone()), name: shell.name.clone() }, ix);
                    }
                }
                SidebarRow::ProjectsHeader | SidebarRow::NoProjects => {}
            }
        }
        out
    }

    /// Where the cursor starts: on the active Session's row, else the first row.
    fn nav_home(&self, targets: &[(NavTarget, usize)]) -> Option<NavTarget> {
        let active = self.active_entry().map(|e| (e.id, e.item.clone()));
        let on = |t: &NavTarget| match (t, &active) {
            (NavTarget::Session(id), Some((active, _))) => id == active,
            _ => false,
        };
        let on_item = |t: &NavTarget| matches!((t, &active), (NavTarget::Item(key), Some((_, item))) if key == item);
        targets
            .iter()
            .find(|(t, _)| on(t))
            .or_else(|| targets.iter().find(|(t, _)| on_item(t)))
            .or_else(|| targets.first())
            .map(|(t, _)| t.clone())
    }

    /// Ctrl+Shift+H: the keyboard into the sidebar (showing it if hidden), or back to the
    /// Session when it is already there.
    pub(super) fn focus_sidebar(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.modal_open(cx) || self.unlock.is_some() {
            return;
        }
        if self.nav.focus.is_focused(window) {
            self.leave_sidebar(window, cx);
            return;
        }
        if matches!(self.page, super::Page::Settings(_)) {
            self.page = super::Page::Sessions;
        }
        if !self.sidebar_visible {
            self.sidebar_visible = true;
            self.sidebar_peek = false;
            self.sidebar_peek_timer = None;
        }
        let targets = self.nav_targets(&self.sidebar_rows());
        if !self.nav.cursor.as_ref().is_some_and(|c| targets.iter().any(|(t, _)| t == c)) {
            self.nav.cursor = self.nav_home(&targets);
        }
        // Back on the row of the Session in use, which may have changed since.
        if let Some(home) = self.nav_home(&targets).filter(|_| self.active.is_some()) {
            self.nav.cursor = Some(home);
        }
        self.nav.reveal = true;
        window.focus(&self.nav.focus, cx);
        cx.notify();
    }

    fn leave_sidebar(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.focus_active(window, cx);
        cx.notify();
    }

    /// The cursor's row has to be in the list: put it back at the start if it went away
    /// (its Host deleted, its Session closed).
    fn nav_cursor(&mut self) -> Option<(NavTarget, Vec<(NavTarget, usize)>)> {
        let targets = self.nav_targets(&self.sidebar_rows());
        if !self.nav.cursor.as_ref().is_some_and(|c| targets.iter().any(|(t, _)| t == c)) {
            self.nav.cursor = self.nav_home(&targets);
        }
        Some((self.nav.cursor.clone()?, targets))
    }

    fn nav_move_to(&mut self, target: NavTarget, cx: &mut Context<Self>) {
        self.nav.cursor = Some(target);
        self.nav.reveal = true;
        cx.notify();
    }

    pub(super) fn nav_step(&mut self, step: isize, cx: &mut Context<Self>) {
        let Some((cursor, targets)) = self.nav_cursor() else { return };
        let Some(pos) = targets.iter().position(|(t, _)| *t == cursor) else { return };
        let next = match step {
            isize::MIN => 0,
            isize::MAX => targets.len() - 1,
            step => pos.saturating_add_signed(step).min(targets.len() - 1),
        };
        self.nav_move_to(targets[next].0.clone(), cx);
    }

    /// Right: unfold a Project or a Host's Sessions, or step into them once unfolded.
    pub(super) fn nav_expand(&mut self, cx: &mut Context<Self>) {
        let Some((cursor, targets)) = self.nav_cursor() else { return };
        match &cursor {
            NavTarget::Project(id) if self.settings.collapsed_projects.contains(id) => self.set_project_collapsed(id, false, cx),
            NavTarget::Item(key) if !self.listed_sessions(key).is_empty() && self.collapsed_items.contains(key) => {
                self.toggle_item_sessions(key, cx)
            }
            NavTarget::Project(_) | NavTarget::Item(_) => {
                let pos = targets.iter().position(|(t, _)| *t == cursor);
                let child = pos.and_then(|pos| targets.get(pos + 1)).map(|(t, _)| t.clone()).filter(|next| {
                    matches!(
                        (&cursor, next),
                        (NavTarget::Project(_), NavTarget::Item(_) | NavTarget::AddHost(_)) | (NavTarget::Item(_), NavTarget::Session(_))
                    )
                });
                if let Some(child) = child {
                    self.nav_move_to(child, cx);
                }
            }
            NavTarget::Session(_) | NavTarget::AddHost(_) => {}
        }
    }

    /// Left: fold a Host's Sessions or a Project, or step out to the row they hang from.
    pub(super) fn nav_collapse(&mut self, cx: &mut Context<Self>) {
        let Some((cursor, _)) = self.nav_cursor() else { return };
        match cursor {
            NavTarget::Session(id) => {
                if let Some(item) = self.sessions.iter().find(|s| s.id == id).map(|s| s.item.clone()) {
                    self.nav_move_to(NavTarget::Item(item), cx);
                }
            }
            NavTarget::Item(key) if !self.listed_sessions(&key).is_empty() && !self.collapsed_items.contains(&key) => {
                self.toggle_item_sessions(&key, cx)
            }
            NavTarget::Item(ItemKey::Host { project, .. } | ItemKey::Shell { project: Some(project), .. })
            | NavTarget::AddHost(project) => self.nav_move_to(NavTarget::Project(project), cx),
            NavTarget::Project(id) if !self.settings.collapsed_projects.contains(&id) => self.set_project_collapsed(&id, true, cx),
            NavTarget::Item(_) | NavTarget::Project(_) => {}
        }
    }

    /// Enter (`another`: Ctrl+Enter): what a click (or Ctrl+click) on the row does.
    pub(super) fn nav_open(&mut self, another: bool, window: &mut Window, cx: &mut Context<Self>) {
        let Some((cursor, _)) = self.nav_cursor() else { return };
        match cursor {
            NavTarget::Project(id) if another => self.open_new_host(id, window, cx),
            NavTarget::Project(id) => {
                let collapsed = self.settings.collapsed_projects.contains(&id);
                self.set_project_collapsed(&id, !collapsed, cx);
            }
            NavTarget::AddHost(id) => self.open_new_host(id, window, cx),
            NavTarget::Item(key) => {
                if let Some(item) = self.item_for_key(&key) {
                    self.activate_item(item, another, window, cx);
                }
            }
            NavTarget::Session(id) => {
                let Some(entry) = self.sessions.iter().find(|s| s.id == id) else { return };
                if another {
                    if let Some(item) = self.item_for_key(&entry.item.clone()) {
                        self.activate_item(item, true, window, cx);
                    }
                    return;
                }
                let reconnect = !entry.is_live() && entry.window.is_none();
                self.activate(id, window, cx);
                if reconnect {
                    self.reconnect(id, window, cx);
                }
            }
        }
    }

    /// F2: edit the Host or Project, or rename the Session.
    pub(super) fn nav_edit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some((cursor, _)) = self.nav_cursor() else { return };
        match cursor {
            NavTarget::Project(id) | NavTarget::AddHost(id) => self.open_edit_project(id, window, cx),
            NavTarget::Item(ItemKey::Host { project, host }) => self.open_edit_host(project, host, window, cx),
            NavTarget::Session(id) => self.open_rename_session(id, window, cx),
            NavTarget::Item(ItemKey::Shell { .. }) => {}
        }
    }

    /// Delete: ask to delete the Host or Project, or close the Session (asking first if a
    /// command is still printing output).
    pub(super) fn nav_delete(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some((cursor, _)) = self.nav_cursor() else { return };
        match cursor {
            NavTarget::Project(id) => self.confirm_delete_project(id, cx),
            NavTarget::Item(ItemKey::Host { host, .. }) => self.confirm_delete_host(host, cx),
            NavTarget::Session(id) => {
                self.request_close(id, window, cx);
                // Stay in the sidebar, which closing would have left.
                if !self.modal_open(cx) {
                    window.focus(&self.nav.focus, cx);
                }
            }
            NavTarget::Item(ItemKey::Shell { .. }) | NavTarget::AddHost(_) => {}
        }
    }

    /// Escape: back to the Session.
    pub(super) fn nav_leave(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.leave_sidebar(window, cx);
    }

    /// The list row to scroll into view for the cursor, once per move.
    pub(super) fn take_nav_reveal(&mut self) -> Option<usize> {
        if !std::mem::take(&mut self.nav.reveal) {
            return None;
        }
        let cursor = self.nav.cursor.clone()?;
        self.nav_targets(&self.sidebar_rows).into_iter().find(|(t, _)| *t == cursor).map(|(_, ix)| ix)
    }
}
