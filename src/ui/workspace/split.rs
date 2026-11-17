//! Split view and dragging sidebar items.
//!
//! A Host or Local Shell dragged from the sidebar onto the terminal area shows a blue
//! rectangle over the half it would take, following the pointer; let go and it opens there,
//! beside the Session you were in. Dragged onto another Project's group in the sidebar, a
//! Host moves to that Project; dragged up or down within its own Project, it changes place
//! there, a blue line showing where it would go.
//!
//! What a drop puts where, and how a split behaves as Sessions come and go, is decided in
//! `sessions.rs`; this file draws it and carries it out.

use std::time::Duration;

use gpui::prelude::FluentBuilder as _;
use gpui::{
    Animation, AnimationExt as _, AnyElement, Context, DragMoveEvent, Hsla, Pixels, Point, SharedString, Window, div,
    ease_out_quint, prelude::*, px,
};

use super::sessions::{Placed, Side};
use super::{ItemKey, SidebarItem, SidebarRow, Workspace};
use crate::theme::{AppPalette, Appearance, project_color};
use crate::ui::widgets::{ISLAND_GAP, ISLAND_RADIUS, elevation, icon_el};

/// What is dragged from the sidebar: an item, or one of its Sessions (a row under an item
/// that has several).
#[derive(Clone)]
pub(super) struct DraggedItem {
    pub key: ItemKey,
    pub session: Option<usize>,
    pub label: SharedString,
    pub icon: String,
    pub color: Hsla,
}

/// Separate from Hosts/Sessions so moving a Project cannot open or move its connections.
#[derive(Clone)]
pub(super) struct DraggedProject {
    pub id: String,
    pub label: SharedString,
    pub icon: String,
    pub color: Hsla,
}

/// Where the dragged item would land if it were let go now.
#[derive(Clone, PartialEq, Debug)]
pub(super) enum DropTarget {
    ProjectOrder { id: String, index: usize, row: usize, hover_row: usize, after: bool },
    /// A half of the terminal area: it opens there, beside the Session that is shown.
    Pane(Side),
    /// The whole terminal area, when no Session is shown: it just opens.
    Main,
    /// A Project's group in the sidebar: the Host moves there. `row` is the sidebar row under
    /// the pointer (each row of the group is a target of its own).
    Project { id: String, row: usize },
    /// Between two Hosts of the dragged Host's own Project: it moves there. `index` is its
    /// new place among the Project's Hosts (see `Library::reorder_host`); the line is drawn
    /// along the top of sidebar row `row`, or along its bottom when `after`.
    Reorder { project: String, index: usize, row: usize, after: bool },
}

/// The colour of every drop indicator: blue whatever the Theme's accent.
pub(super) fn drop_color() -> Hsla {
    project_color("blue")
}

impl Workspace {
    pub(super) fn project_drag_preview(
        &self,
        cx: &Context<Self>,
    ) -> impl Fn(&DraggedProject, Point<Pixels>, &mut Window, &mut gpui::App) -> gpui::Entity<DragPreview> + 'static {
        let ws = cx.entity().downgrade();
        move |dragged, offset, _, cx| {
            let _ = ws.update(cx, |this, cx| this.set_drop_target(None, cx));
            let ws = ws.clone();
            cx.new(|cx| {
                cx.on_release(move |_, cx| cx.defer(move |cx| {
                    let _ = ws.update(cx, |this, cx| this.set_drop_target(None, cx));
                })).detach();
                DragPreview { native: false, label: dragged.label.clone(), icon: dragged.icon.clone(), color: dragged.color, offset }
            })
        }
    }

    pub(super) fn drag_project_order(&mut self, project: &str, row: usize, event: &DragMoveEvent<DraggedProject>, cx: &mut Context<Self>) {
        let dragged = event.drag(cx).id.clone();
        let target = if event.bounds.contains(&event.event.position) && !self.modal_open(cx) {
            let from = self.library.projects.iter().position(|p| p.id == dragged);
            let to = self.library.projects.iter().position(|p| p.id == project);
            match (from, to) {
                (Some(from), Some(to)) => {
                    let after = !matches!(self.sidebar_rows.get(row), Some(SidebarRow::Project { .. }))
                        || event.event.position.y > event.bounds.center().y;
                    let index = to + usize::from(after);
                    let boundary = if after {
                        self.sidebar_rows.iter().rposition(|r| match r {
                            SidebarRow::Project { index } => *index == to,
                            SidebarRow::FirstHost { project } | SidebarRow::Host { project, .. }
                            | SidebarRow::ProjectShell { project, .. } => *project == to,
                            _ => false,
                        })
                    } else {
                        self.sidebar_rows.iter().position(|r| matches!(r, SidebarRow::Project { index } if *index == to))
                    };
                    boundary.filter(|_| index != from && index != from + 1)
                        .map(|boundary| DropTarget::ProjectOrder { id: project.to_owned(), index, row: boundary, hover_row: row, after })
                }
                _ => None,
            }
        } else {
            None
        };
        // Each row receives movement events; leaving an unrelated row must not erase the
        // target selected by the row currently under the pointer.
        let here = matches!(&self.drop_target, Some(DropTarget::ProjectOrder { hover_row, .. }) if *hover_row == row);
        if target.is_some() || (here && !event.bounds.contains(&event.event.position)) {
            self.set_drop_target(target, cx);
        } else if event.bounds.contains(&event.event.position) {
            self.set_drop_target(None, cx);
        }
    }

    pub(super) fn drop_project_order(&mut self, project: &str, dragged: &DraggedProject, cx: &mut Context<Self>) {
        let target = self.drop_target.take();
        cx.notify();
        if self.modal_open(cx) {
            return;
        }
        if let Some(DropTarget::ProjectOrder { id, index, .. }) = target && id == project {
            let mut library = self.library.clone();
            if library.reorder_project(&dragged.id, index) {
                if let Err(err) = self.commit_library(library) {
                    self.show_toast(format!("Could not reorder projects. {err}"), cx);
                }
            }
        }
    }

    /// The payload for dragging a sidebar row.
    pub(super) fn dragged(&self, item: &SidebarItem, session: Option<usize>, color: Hsla) -> DraggedItem {
        let label = match session.and_then(|id| self.sessions.get(id)) {
            Some(s) => s.label().clone(),
            None => item.label().to_string().into(),
        };
        DraggedItem { key: item.key(), session, label, icon: item.icon(), color }
    }

    /// Builds the chip that follows the pointer when a sidebar row is dragged. A drag let go
    /// outside the window reaches no drop handler, so a new drag starts from no target.
    pub(super) fn drag_preview(
        &self,
        cx: &Context<Self>,
    ) -> impl Fn(&DraggedItem, Point<Pixels>, &mut Window, &mut gpui::App) -> gpui::Entity<DragPreview> + 'static {
        let ws = cx.entity().downgrade();
        move |dragged, offset, window, cx| {
            let native = ws.update(cx, |this, cx| {
                this.drop_target = None;
                this.native_drag = dragged.session
                    .filter(|id| this.sessions.get(*id).is_some_and(|s| s.ui.view.desktop().is_some()))
                    .and_then(|_| crate::platform::DragPreview::new(&dragged.label, window, cx.global::<Appearance>().app.dark));
                this.native_drag.is_some()
            }).unwrap_or(false);
            let ws = ws.clone();
            cx.new(|cx| {
                cx.on_release(move |_, cx| cx.defer(move |cx| {
                    let _ = ws.update(cx, |this, _| this.native_drag = None);
                })).detach();
                let mut preview = DragPreview::new(dragged, offset);
                preview.native = native;
                preview
            })
        }
    }

    /// Show `key` (or its `session`) on `side` of the terminal area, beside what is shown.
    pub(super) fn open_in_split(
        &mut self,
        key: &ItemKey,
        session: Option<usize>,
        side: Side,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(placed) = self.sessions.place(key, session, side) else { return };
        match placed {
            Placed::Show(id) => self.activate(id, window, cx),
            Placed::Existing(id) => {
                self.activate(id, window, cx);
                if self.sessions.get(id).is_some_and(|s| !s.is_live()) {
                    self.reconnect(id, window, cx);
                }
            }
            Placed::New(pending) => {
                let Some(item) = self.item_for_key(key) else { return };
                self.sessions.wait_for_split(pending);
                self.activate_item(item, true, window, cx);
            }
        }
        cx.notify();
    }

    /// Back to one Session: the active one fills the terminal area again.
    pub(super) fn close_split(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.sessions.close_split();
        self.focus_active(window, cx);
        cx.notify();
    }

    fn set_drop_target(&mut self, target: Option<DropTarget>, cx: &mut Context<Self>) {
        if self.drop_target != target {
            self.drop_target = target;
            cx.notify();
        }
    }

    /// The pointer moved while dragging: over the terminal area, the half it is over.
    pub(super) fn drag_over_main(&mut self, event: &DragMoveEvent<DraggedItem>, cx: &mut Context<Self>) {
        let position = event.event.position;
        if !event.bounds.contains(&position) {
            if matches!(self.drop_target, Some(DropTarget::Pane(_) | DropTarget::Main)) {
                self.set_drop_target(None, cx);
            }
            return;
        }
        let dragged = event.drag(cx).clone();
        let target = if self.modal_open(cx) {
            None
        } else if self.sessions.active().is_none() {
            Some(DropTarget::Main)
        } else {
            let side = if position.x < event.bounds.center().x { Side::Left } else { Side::Right };
            self.sessions.can_place(&dragged.key, dragged.session, side).then_some(DropTarget::Pane(side))
        };
        self.set_drop_target(target, cx);
    }

    pub(super) fn drop_on_main(&mut self, dragged: &DraggedItem, window: &mut Window, cx: &mut Context<Self>) {
        let target = self.drop_target.take();
        cx.notify();
        if self.modal_open(cx) {
            return;
        }
        match target {
            Some(DropTarget::Pane(side)) => self.open_in_split(&dragged.key, dragged.session, side, window, cx),
            Some(DropTarget::Main) => match dragged.session {
                Some(id) => self.activate(id, window, cx),
                None => {
                    if let Some(item) = self.item_for_key(&dragged.key) {
                        self.activate_item(item, false, window, cx);
                    }
                }
            },
            _ => {}
        }
    }

    /// The pointer moved while dragging: over one of a Project's sidebar rows.
    pub(super) fn drag_over_project(
        &mut self,
        project: &str,
        row: usize,
        event: &DragMoveEvent<DraggedItem>,
        cx: &mut Context<Self>,
    ) {
        let over = event.bounds.contains(&event.event.position) && !self.modal_open(cx);
        let dragged = event.drag(cx);
        let (key, session) = (dragged.key.clone(), dragged.session);
        let ItemKey::Host { project: from, host } = &key else { return };
        if from == project {
            // Within its own Project: the place it would take, above or below this row.
            let after = event.event.position.y > event.bounds.center().y;
            let target = if over && session.is_none() { self.reorder_target(project, host, row, after) } else { None };
            let here = matches!(&self.drop_target, Some(DropTarget::Reorder { row: r, .. }) if *r == row);
            if target.is_some() || here {
                self.set_drop_target(target, cx);
            }
            return;
        }
        // A Session's row only opens beside another; moving it would take its whole Host,
        // with every other Session of it, into the other Project.
        if session.is_some() {
            if matches!(&self.drop_target, Some(DropTarget::Project { .. })) {
                self.set_drop_target(None, cx);
            }
            return;
        }
        let target = DropTarget::Project { id: project.to_string(), row };
        if over {
            self.set_drop_target(Some(target), cx);
        } else if self.drop_target.as_ref() == Some(&target) {
            self.set_drop_target(None, cx);
        }
    }

    /// Where `host` would go in its `project` if let go over sidebar row `row` (in its top
    /// half, or its bottom half when `after`), or `None` when it would stay where it is.
    fn reorder_target(&self, project: &str, host: &str, row: usize, after: bool) -> Option<DropTarget> {
        let hosts = &self.library.projects.iter().find(|p| p.id == project)?.hosts;
        let from = hosts.iter().position(|h| h == host)?;
        let (index, row, after) = match self.sidebar_rows.get(row)? {
            // The Project's own row: to the top.
            SidebarRow::Project { .. } => (0, row, true),
            SidebarRow::Host { host: at, .. } => {
                let id = &self.library.hosts.get(*at)?.id;
                let place = hosts.iter().position(|h| h == id)?;
                (place + usize::from(after), row, after)
            }
            // The Project's shells, listed after its Hosts: to the bottom, under the last Host.
            SidebarRow::ProjectShell { .. } => {
                let last = (0..row).rev().find(|&r| matches!(self.sidebar_rows[r], SidebarRow::Host { .. }))?;
                (hosts.len(), last, true)
            }
            _ => return None,
        };
        (index != from && index != from + 1).then(|| DropTarget::Reorder { project: project.to_string(), index, row, after })
    }

    /// The line showing where a Host dragged within its Project would go, when it is drawn
    /// along sidebar row `row`: `Some(true)` along its bottom, `Some(false)` along its top.
    pub(super) fn reorder_line(&self, row: usize, cx: &gpui::App) -> Option<bool> {
        match &self.drop_target {
            Some(DropTarget::ProjectOrder { row: r, after, .. }) if *r == row && cx.has_active_drag() => Some(*after),
            Some(DropTarget::Reorder { row: r, after, .. }) if *r == row && cx.has_active_drag() => Some(*after),
            _ => None,
        }
    }

    pub(super) fn drop_on_project(&mut self, project: &str, dragged: &DraggedItem, cx: &mut Context<Self>) {
        let target = self.drop_target.take();
        cx.notify();
        let ItemKey::Host { project: from, host } = &dragged.key else { return };
        match target {
            Some(DropTarget::Project { id, .. }) if id == project && dragged.session.is_none() => {
                self.move_host_to(host, from, project, cx)
            }
            Some(DropTarget::Reorder { project: id, index, .. }) if id == project && from == project => {
                self.reorder_host(project, host, index, cx)
            }
            _ => {}
        }
    }

    /// Move a Host to another place within its Project.
    fn reorder_host(&mut self, project: &str, host_id: &str, index: usize, cx: &mut Context<Self>) {
        let mut library = self.library.clone();
        if !library.reorder_host(project, host_id, index) {
            return;
        }
        if let Err(err) = self.commit_library(library) {
            let name = self.library.host(host_id).map(|h| h.name.clone()).unwrap_or_default();
            self.show_toast(format!("Could not move {name}. {err}"), cx);
        }
    }

    /// The Project whose sidebar group the dragged Host would move into.
    pub(super) fn drop_project(&self, cx: &gpui::App) -> Option<&str> {
        match &self.drop_target {
            Some(DropTarget::Project { id, .. }) if cx.has_active_drag() => Some(id),
            _ => None,
        }
    }

    /// Move a Host to another Project. Its open Sessions go with it: they stay connected,
    /// and reconnect with the new Project's defaults.
    fn move_host_to(&mut self, host_id: &str, from: &str, to: &str, cx: &mut Context<Self>) {
        let mut library = self.library.clone();
        if !library.move_host(host_id, from, to) {
            return;
        }
        let name = self.library.host(host_id).map(|h| h.name.clone()).unwrap_or_default();
        if let Err(err) = self.commit_library(library) {
            self.show_toast(format!("Could not move {name}. {err}"), cx);
            return;
        }
        let old = ItemKey::Host { project: from.to_string(), host: host_id.to_string() };
        let new = ItemKey::Host { project: to.to_string(), host: host_id.to_string() };
        self.sessions.move_item(&old, &new, &name, None);
        if self.collapsed_items.remove(&old) {
            self.collapsed_items.insert(new);
        }
        // Open the Project it went to, so it can be seen there.
        self.set_project_collapsed(to, false, cx);
        let project = self.library.project(to).map(|p| p.name.clone()).unwrap_or_default();
        self.show_toast(format!("Moved {name} to {project}."), cx);
    }

    /// The blue rectangle over the half of the terminal area the dragged item would take.
    pub(super) fn render_drop_overlay(&self, cx: &gpui::App) -> Option<AnyElement> {
        if !cx.has_active_drag() {
            return None;
        }
        let side = match self.drop_target.as_ref()? {
            DropTarget::Pane(side) => Some(*side),
            DropTarget::Main => None,
            DropTarget::Project { .. } | DropTarget::Reorder { .. } | DropTarget::ProjectOrder { .. } => return None,
        };
        let p = cx.global::<Appearance>().app;
        let blue = drop_color();
        let rect = div().flex_1().min_w_0().h_full().p(px(6.)).child(
            div()
                .size_full()
                .rounded(p.corner(ISLAND_RADIUS - 4.))
                .border_2()
                .border_color(blue.opacity(0.85))
                .bg(blue.opacity(if p.dark { 0.12 } else { 0.09 })),
        );
        let id = match side {
            None => "drop-main",
            Some(Side::Left) => "drop-left",
            Some(Side::Right) => "drop-right",
        };
        // The two halves line up with the panes of a split.
        let overlay = div().absolute().inset_0().flex().gap(px(ISLAND_GAP)).map(|el| match side {
            None => el.child(rect),
            Some(Side::Left) => el.child(rect).child(div().flex_1()),
            Some(Side::Right) => el.child(div().flex_1()).child(rect),
        });
        Some(
            overlay
                .with_animation(id, Animation::new(Duration::from_millis(140)).with_easing(ease_out_quint()), |el, t| {
                    el.opacity(t)
                })
                .into_any_element(),
        )
    }
}

/// What follows the pointer while a sidebar item is dragged: its icon and name in a chip.
pub(super) struct DragPreview {
    native: bool,
    label: SharedString,
    icon: String,
    color: Hsla,
    /// Where the row was grabbed, from its top left: the chip is drawn there, under the pointer.
    offset: Point<Pixels>,
}

impl DragPreview {
    pub fn new(dragged: &DraggedItem, offset: Point<Pixels>) -> Self {
        Self { native: false, label: dragged.label.clone(), icon: dragged.icon.clone(), color: dragged.color, offset }
    }
}

impl Render for DragPreview {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p: AppPalette = cx.global::<Appearance>().app;
        if self.native {
            return div();
        }
        div()
            .pl((self.offset.x - px(14.)).max(px(0.)))
            .pt((self.offset.y - px(15.)).max(px(0.)))
            .child(
                div()
                    .h(px(30.))
                    .max_w(px(240.))
                    .px(px(10.))
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .rounded(p.corner(8.))
                    .bg(p.surface.opacity(1.))
                    .border_1()
                    .border_color(p.border)
                    .shadow(elevation(p))
                    .font_family(p.font_family)
                    .text_size(px(12.5))
                    .text_color(p.text)
                    .child(div().flex_shrink_0().child(icon_el(&self.icon, 14., self.color)))
                    .child(div().min_w_0().truncate().child(self.label.clone())),
            )
    }
}
