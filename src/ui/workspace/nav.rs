//! Keyboard use of the sidebar. Ctrl+Shift+H moves the keyboard into it (and back out); a
//! cursor then walks its rows: Up and Down move, Right unfolds (or steps into) a Project or a
//! Host's Sessions, Left folds them (or steps out), Enter connects or shows, Ctrl+Enter opens
//! another Session, Shift+F10 opens the row's menu, and Escape goes back to the Session.
//!
//! F2 and Delete act on the Session a row stands for (rename, disconnect), so they mean the
//! same on a Host's row as on its Sessions' rows; on an idle Host or a Project they edit or
//! delete it. Shift+F2 and Shift+Delete always edit or delete the Host.
//!
//! Also the rows' menus, shared by the ⋯ buttons and Shift+F10 (see `open_row_menu`).

use gpui::{AnyElement, Context, DismissEvent, Entity, FocusHandle, Focusable as _, SharedString, Subscription, Window, actions, div, prelude::*, px};
use gpui_component::menu::PopupMenu;
use gpui_component::popover::Popover;

use super::{ItemKey, SidebarRow, Workspace};
use crate::ui::widgets::MenuTrigger;

actions!(
    sidebar,
    [
        FocusSidebar,
        NavUp,
        NavDown,
        NavFirst,
        NavLast,
        NavExpand,
        NavCollapse,
        NavOpen,
        NavOpenAnother,
        NavEdit,
        NavEditHost,
        NavDelete,
        NavDeleteHost,
        NavMenu,
        NavLeave
    ]
);

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
    /// The keyboard moved the cursor since the last click in the list. A click focuses the
    /// list too, but shouldn't light up a cursor the mouse never used.
    pub keyboard: bool,
    /// The row whose menu is open from its ⋯ button or Shift+F10.
    pub menu: Option<RowMenu>,
    /// A dialog was opened from the sidebar's keys (F2, Delete): once it closes, the keyboard
    /// goes back to the sidebar rather than to the terminal. Cleared on the first frame
    /// without a dialog, so it never outlives that dialog.
    pub after_dialog: std::cell::Cell<bool>,
}

pub(super) struct RowMenu {
    pub target: NavTarget,
    pub menu: Entity<PopupMenu>,
    _dismissed: Subscription,
}

impl NavState {
    pub fn new(cx: &mut Context<Workspace>) -> Self {
        // A tab stop too, so Tab from Quick connect reaches the list.
        Self {
            focus: cx.focus_handle().tab_index(0).tab_stop(true),
            cursor: None,
            focused: false,
            reveal: false,
            keyboard: false,
            menu: None,
            after_dialog: std::cell::Cell::new(false),
        }
    }
}

impl Workspace {
    /// The sidebar's keys are for the list itself. With a button inside it focused (Tab), the
    /// key goes on to that button instead. With a dialog up they do nothing: a second Delete
    /// must not act behind the question the first one asked.
    pub(super) fn nav_owns(&mut self, window: &Window, cx: &mut Context<Self>) -> bool {
        if self.modal_open(cx) {
            return false;
        }
        let owns = self.nav.focus.is_focused(window);
        if owns {
            self.nav.keyboard = true;
        } else {
            cx.propagate();
        }
        owns
    }

    /// Whether the keyboard cursor is on this row and should be drawn.
    pub(super) fn nav_focus_on(&self, target: &NavTarget) -> bool {
        self.nav.focused && self.nav.keyboard && self.nav.cursor.as_ref() == Some(target)
    }

    /// A click on a row: the cursor follows it, without showing until a key is used.
    pub(super) fn nav_clicked(&mut self, target: NavTarget) {
        self.nav.cursor = Some(target);
        self.nav.keyboard = false;
    }

    /// The Session a row stands for: an item's own (its only one, or its terminal beside
    /// listed desktops), or a Session's row.
    pub(super) fn nav_session(&self, target: &NavTarget) -> Option<usize> {
        match target {
            NavTarget::Session(id) => Some(*id),
            NavTarget::Item(key) => self.item_state(key).main.map(|s| s.id),
            NavTarget::Project(_) | NavTarget::AddHost(_) => None,
        }
    }

    // ---- row menus ---------------------------------------------------------------------

    /// Open a row's menu: from its ⋯ button, or Shift+F10 on the keyboard cursor's row. It
    /// is the row's right-click menu.
    pub(super) fn open_row_menu(&mut self, target: NavTarget, window: &mut Window, cx: &mut Context<Self>) {
        if self.modal_open(cx) {
            return;
        }
        type Build = Box<dyn Fn(PopupMenu, &mut Window, &mut Context<PopupMenu>) -> PopupMenu>;
        let build: Build = match &target {
            NavTarget::Item(key) => {
                let Some(item) = self.item_for_key(key) else { return };
                Box::new(self.item_menu(&item, cx))
            }
            NavTarget::Session(id) => {
                if !self.sessions.iter().any(|s| s.id == *id) {
                    return;
                }
                Box::new(self.session_menu(*id, cx))
            }
            NavTarget::Project(id) => {
                let Some(index) = self.library.projects.iter().position(|p| p.id == *id) else { return };
                Box::new(self.project_menu(index, cx))
            }
            NavTarget::AddHost(_) => return,
        };
        // Opened from the keyboard: Escape returns there. A chosen action keeps the focus it
        // gave (a terminal it showed, a dialog it opened).
        let from_sidebar = self.nav.focus.is_focused(window);
        // Build the menu once this update is over. A builder reads the Workspace (the
        // "Notify when done" entry does, on Windows), and reading it while it is being updated,
        // as it is here, panics; inside a click from the window procedure that aborts Komando.
        let ws = cx.entity().downgrade();
        window.defer(cx, move |window, cx| {
            let menu = PopupMenu::build(window, cx, move |menu, window, cx| build(menu, window, cx));
            let _ = ws.update(cx, |this, cx| this.show_row_menu(target, menu, from_sidebar, window, cx));
        });
    }

    fn show_row_menu(&mut self, target: NavTarget, menu: Entity<PopupMenu>, from_sidebar: bool, window: &mut Window, cx: &mut Context<Self>) {
        // Something else may have opened in the meantime.
        if self.modal_open(cx) {
            return;
        }
        let menu_focus = menu.focus_handle(cx);
        let dismissed = cx.subscribe_in(&menu, window, move |this, _, _: &DismissEvent, window, cx| {
            this.nav.menu = None;
            if menu_focus.is_focused(window) {
                if from_sidebar {
                    window.focus(&this.nav.focus, cx);
                } else {
                    this.focus_active(window, cx);
                }
            }
            cx.notify();
        });
        menu.focus_handle(cx).focus(window, cx);
        // A ⋯ click needn't reach the row's click handler. Keep the cursor on the row
        // whose menu opened, without scrolling away from the pointer's position.
        self.nav.cursor = Some(target.clone());
        self.nav.menu = Some(RowMenu { target, menu, _dismissed: dismissed });
        cx.notify();
    }

    pub(super) fn close_row_menu(&mut self, cx: &mut Context<Self>) {
        if self.nav.menu.take().is_some() {
            cx.notify();
        }
    }

    pub(super) fn row_menu_open(&self, target: &NavTarget) -> bool {
        self.nav.menu.as_ref().is_some_and(|m| m.target == *target)
    }

    /// The row's menu hung from `trigger` (a ⋯ button), open while `open_row_menu` says so.
    pub(super) fn menu_popover(&self, id: SharedString, target: NavTarget, trigger: MenuTrigger, cx: &mut Context<Self>) -> AnyElement {
        let menu = self.nav.menu.as_ref().filter(|m| m.target == target).map(|m| m.menu.clone());
        let ws = cx.entity().downgrade();
        Popover::new(id)
            .appearance(false)
            .anchor(gpui::Anchor::TopRight)
            .trigger(trigger)
            .open(menu.is_some())
            .when_some(menu.as_ref(), |el, menu| el.track_focus(&menu.focus_handle(cx)))
            .on_open_change(move |open, window, cx| {
                let (open, target) = (*open, target.clone());
                let _ = ws.update(cx, |this, cx| {
                    if open {
                        this.open_row_menu(target, window, cx);
                    } else {
                        this.close_row_menu(cx);
                    }
                });
            })
            .content(move |_, _, _| menu.clone().map(|m| m.into_any_element()).unwrap_or_else(|| div().into_any_element()))
            .into_any_element()
    }

    /// For a row without a ⋯ button (a Session's, a Project's): an invisible point at its end
    /// for Shift+F10's menu to hang from, there only while that menu is open.
    pub(super) fn row_menu_anchor(&self, target: NavTarget, cx: &mut Context<Self>) -> Option<AnyElement> {
        if !self.row_menu_open(&target) {
            return None;
        }
        let trigger = MenuTrigger::new(div().id("row-menu-anchor").size(px(1.)));
        let popover = self.menu_popover(SharedString::from(format!("row-menu-{target:?}")), target, trigger, cx);
        Some(div().absolute().right(px(8.)).bottom(px(4.)).child(popover).into_any_element())
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
        // In a folded Project: its row.
        let on_project = |t: &NavTarget| match (t, &active) {
            (NavTarget::Project(id), Some((_, ItemKey::Host { project, .. } | ItemKey::Shell { project: Some(project), .. }))) => {
                id == project
            }
            _ => false,
        };
        targets
            .iter()
            .find(|(t, _)| on(t))
            .or_else(|| targets.iter().find(|(t, _)| on_item(t)))
            .or_else(|| targets.iter().find(|(t, _)| on_project(t)))
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
        self.nav.keyboard = true;
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

    /// F2: rename the Session the row stands for; on an idle Host or a Project, edit it.
    /// `host` (Shift+F2): always edit the Host.
    pub(super) fn nav_edit(&mut self, host: bool, window: &mut Window, cx: &mut Context<Self>) {
        self.nav_edit_inner(host, window, cx);
        self.nav.after_dialog.set(self.modal_open(cx));
    }

    fn nav_edit_inner(&mut self, host: bool, window: &mut Window, cx: &mut Context<Self>) {
        let Some((cursor, _)) = self.nav_cursor() else { return };
        if !host && let Some(id) = self.nav_session(&cursor) {
            self.open_rename_session(id, window, cx);
            return;
        }
        match cursor {
            NavTarget::Project(id) | NavTarget::AddHost(id) => self.open_edit_project(id, window, cx),
            NavTarget::Item(ItemKey::Host { project, host }) => self.open_edit_host(project, host, window, cx),
            NavTarget::Session(id) => {
                if let Some(super::SessionKind::Ssh { host_id } | super::SessionKind::Desktop { host_id, .. }) =
                    self.sessions.iter().find(|s| s.id == id).map(|s| s.kind.clone())
                    && let Some(project) = self.sessions.iter().find(|s| s.id == id).and_then(|s| s.project_id.clone())
                {
                    self.open_edit_host(project, host_id, window, cx);
                }
            }
            NavTarget::Item(ItemKey::Shell { .. }) => {}
        }
    }

    /// Delete: disconnect (or close) the Session the row stands for, asking first if a
    /// command is still printing output; on an idle Host or a Project, ask to delete it.
    /// `host` (Shift+Delete): always ask to delete the Host.
    pub(super) fn nav_delete(&mut self, host: bool, window: &mut Window, cx: &mut Context<Self>) {
        self.nav_delete_inner(host, window, cx);
        self.nav.after_dialog.set(self.modal_open(cx));
    }

    fn nav_delete_inner(&mut self, host: bool, window: &mut Window, cx: &mut Context<Self>) {
        let Some((cursor, _)) = self.nav_cursor() else { return };
        if !host && let Some(id) = self.nav_session(&cursor) {
            self.request_close(id, window, cx);
            // Stay in the sidebar, which closing would have left.
            if !self.modal_open(cx) {
                window.focus(&self.nav.focus, cx);
            }
            return;
        }
        match cursor {
            NavTarget::Project(id) if !host => self.confirm_delete_project(id, cx),
            NavTarget::Item(ItemKey::Host { host, .. }) => self.confirm_delete_host(host, cx),
            NavTarget::Session(id) => {
                let host = self.sessions.iter().find(|s| s.id == id).and_then(|s| match &s.kind {
                    super::SessionKind::Ssh { host_id } | super::SessionKind::Desktop { host_id, .. } => Some(host_id.clone()),
                    super::SessionKind::Local { .. } => None,
                });
                if let Some(host) = host {
                    self.confirm_delete_host(host, cx);
                }
            }
            NavTarget::Project(_) | NavTarget::Item(ItemKey::Shell { .. }) | NavTarget::AddHost(_) => {}
        }
    }

    /// Shift+F10 (or the Menu key): the cursor's row's menu.
    pub(super) fn nav_menu(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some((cursor, _)) = self.nav_cursor() else { return };
        // The keyboard cursor may be off screen after scrolling with the pointer.
        self.nav.reveal = true;
        self.open_row_menu(cursor, window, cx);
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

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;
    use crate::model::{Host, Library, Project, Settings};
    use crate::ui::workspace::Startup;
    use gpui::{ListAlignment, ListOffset, ListState, WindowOptions};

    #[test]
    fn opening_row_menu_preserves_sidebar_scroll() {
        let dir = std::env::temp_dir().join(format!("komando-row-menu-{}", uuid::Uuid::new_v4()));
        Settings { onboarded: true, check_for_updates: false, ..Settings::default() }.save(&dir).unwrap();
        let data_dir = dir.clone();
        gpui_platform::headless().run(move |cx| {
            crate::init(cx);
            cx.open_window(WindowOptions::default(), move |window, cx| {
                let workspace = cx.new(|cx| Workspace::new(data_dir, Startup::default(), window, cx));
                workspace.update(cx, |this, cx| {
                    let mut project = Project::new("Lab", "blue", "folder");
                    let hosts: Vec<_> = (0..100).map(|i| Host::new(&format!("host-{i}"), "localhost")).collect();
                    project.hosts = hosts.iter().map(|host| host.id.clone()).collect();
                    let first = NavTarget::Project(project.id.clone());
                    let target = NavTarget::Item(ItemKey::Host {
                        project: project.id.clone(), host: hosts[60].id.clone(),
                    });
                    this.library = Library { shells: Some(vec![]), projects: vec![project], hosts };
                    this.sidebar_rows = this.sidebar_rows();
                    this.sidebar_list = ListState::new(this.sidebar_rows.len(), ListAlignment::Top, px(0.));
                    this.sidebar_list.reset_with_uniform_height(this.sidebar_rows.len(), px(36.));
                    let top = ListOffset { item_ix: 60, offset_in_item: px(10.) };

                    // The pointer has scrolled past an old keyboard cursor, or the keyboard
                    // hasn't been used at all. Neither should move the list when ⋯ opens.
                    for cursor in [Some(first), None] {
                        this.nav.cursor = cursor;
                        this.nav.reveal = false;
                        this.sidebar_list.scroll_to(top);
                        let menu = PopupMenu::build(window, cx, |menu, _, _| menu);
                        this.show_row_menu(target.clone(), menu, false, window, cx);
                        let _ = this.render_sidebar(crate::theme::AppPalette::dark(), window, cx);
                        assert_eq!(this.sidebar_list.logical_scroll_top(), top, "opening ⋯ must keep the sidebar in place");
                        assert_eq!(this.nav.cursor.as_ref(), Some(&target));
                        this.close_row_menu(cx);
                    }

                    // A keyboard menu still reveals its row if it was scrolled off screen.
                    this.nav.cursor = Some(target);
                    this.nav_menu(window, cx);
                    assert_eq!(this.take_nav_reveal(), Some(62));
                    assert_eq!(this.take_nav_reveal(), None);
                });
                workspace
            }).unwrap();
            cx.quit();
        });
        std::fs::remove_dir_all(dir).unwrap();
    }
}
