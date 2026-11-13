//! Split view and dragging sidebar items.
//!
//! A Host or Local Shell dragged from the sidebar onto the terminal area shows a blue
//! rectangle over the half it would take, following the pointer; let go and it opens there,
//! beside the Session you were in. Dragged onto another Project's group in the sidebar, a
//! Host moves to that Project.

use std::collections::HashSet;
use std::time::Duration;

use gpui::prelude::FluentBuilder as _;
use gpui::{
    Animation, AnimationExt as _, AnyElement, Context, DragMoveEvent, Hsla, Pixels, Point, SharedString, Window, div,
    ease_out_quint, prelude::*, px,
};

use super::{ItemKey, SidebarItem, Workspace};
use crate::theme::{AppPalette, Appearance, project_color};
use crate::ui::widgets::{ISLAND_GAP, ISLAND_RADIUS, elevation, icon_el};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Side {
    Left,
    Right,
}

impl Side {
    fn opposite(self) -> Self {
        match self {
            Side::Left => Side::Right,
            Side::Right => Side::Left,
        }
    }
}

/// Two Sessions side by side. It is remembered while another Session is shown and comes
/// back when either of its two is activated again.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) struct Split {
    pub left: usize,
    pub right: usize,
}

impl Split {
    /// `placed` on `side`, `other` on the other one.
    fn new(side: Side, placed: usize, other: usize) -> Self {
        match side {
            Side::Left => Split { left: placed, right: other },
            Side::Right => Split { left: other, right: placed },
        }
    }

    pub fn contains(&self, id: usize) -> bool {
        self.left == id || self.right == id
    }

    fn at(&self, side: Side) -> usize {
        match side {
            Side::Left => self.left,
            Side::Right => self.right,
        }
    }

    pub fn other(&self, id: usize) -> Option<usize> {
        if self.left == id {
            Some(self.right)
        } else if self.right == id {
            Some(self.left)
        } else {
            None
        }
    }
}

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

/// Where the dragged item would land if it were let go now.
#[derive(Clone, PartialEq, Debug)]
pub(super) enum DropTarget {
    /// A half of the terminal area: it opens there, beside the Session that is shown.
    Pane(Side),
    /// The whole terminal area, when no Session is shown: it just opens.
    Main,
    /// A Project's group in the sidebar: the Host moves there. `row` is the sidebar row under
    /// the pointer (each row of the group is a target of its own).
    Project { id: String, row: usize },
}

/// A split waiting for its new Session, which may first need the Vault unlocked.
pub(super) struct PendingSplit {
    item: ItemKey,
    partner: usize,
    side: Side,
}

/// How a drop on one half plays out.
enum Placement {
    /// The two Sessions of the split trade places.
    Swap,
    /// This Session goes there.
    Existing(usize),
    /// A new Session of the item goes there.
    New,
}

/// The colour of every drop indicator: blue whatever the Theme's accent.
pub(super) fn drop_color() -> Hsla {
    project_color("blue")
}

impl Workspace {
    /// The split on screen: the remembered one, while one of its Sessions is active.
    pub(super) fn shown_split(&self) -> Option<Split> {
        let split = self.split?;
        self.active.filter(|&id| split.contains(id)).map(|_| split)
    }

    /// The payload for dragging a sidebar row.
    pub(super) fn dragged(&self, item: &SidebarItem, session: Option<usize>, color: Hsla) -> DraggedItem {
        let label = match session.and_then(|id| self.sessions.iter().find(|s| s.id == id)) {
            Some(s) => s.label.clone(),
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
        move |dragged, offset, _, cx| {
            let _ = ws.update(cx, |this, _| this.drop_target = None);
            cx.new(|_| DragPreview::new(dragged, offset))
        }
    }

    /// What dropping `key` (or its `session`) on `side` would do, or `None` when it would
    /// change nothing (it is already there).
    fn placement(&self, key: &ItemKey, session: Option<usize>, side: Side) -> Option<(Placement, usize)> {
        let active = self.active?;
        let shown = self.shown_split();
        // The Session that stays on the other side.
        let other = shown.map_or(active, |s| s.at(side.opposite()));
        let here = shown.map(|s| s.at(side));
        let item_of = |id: usize| self.sessions.iter().find(|s| s.id == id).map(|s| &s.item);
        // Dragging the other half's Session (or its item) across swaps the two.
        if shown.is_some() && (session == Some(other) || (session.is_none() && item_of(other) == Some(key))) {
            return Some((Placement::Swap, other));
        }
        let placed = match session {
            Some(id) if id == other => return None,
            Some(id) => Placement::Existing(id),
            // The item's Session last shown, unless that one stays on the other side: then
            // another of its Sessions, or a new one (two shells on one server).
            None => {
                let last = self.last_active.get(key).copied().filter(|&id| id != other && item_of(id) == Some(key));
                match last.or_else(|| self.sessions_of(key).map(|s| s.id).find(|&id| id != other)) {
                    Some(id) => Placement::Existing(id),
                    None => Placement::New,
                }
            }
        };
        if let Placement::Existing(id) = placed
            && Some(id) == here
        {
            return None;
        }
        Some((placed, other))
    }

    /// Whether "Open in split view" would do anything for an item.
    pub(super) fn placement_possible(&self, key: &ItemKey) -> bool {
        self.placement(key, None, Side::Right).is_some()
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
        let Some((placement, other)) = self.placement(key, session, side) else { return };
        match placement {
            Placement::Swap => {
                let Some(split) = self.shown_split() else { return };
                self.split = Some(Split { left: split.right, right: split.left });
                self.activate(other, window, cx);
            }
            Placement::Existing(id) => {
                self.split = Some(Split::new(side, id, other));
                self.activate(id, window, cx);
                if self.sessions.iter().find(|s| s.id == id).is_some_and(|s| !s.is_live()) {
                    self.reconnect(id, window, cx);
                }
            }
            Placement::New => {
                let Some(item) = self.item_for_key(key) else { return };
                // Taken up by `add_session` once the Session exists (after the Vault, if
                // the Host needs it).
                self.pending_split = Some(PendingSplit { item: key.clone(), partner: other, side });
                self.activate_item(item, true, window, cx);
            }
        }
        cx.notify();
    }

    /// A Session was just added for `item`: put it in the split it was opened for.
    pub(super) fn take_pending_split(&mut self, item: &ItemKey, id: usize) {
        let Some(pending) = self.pending_split.take_if(|p| p.item == *item) else { return };
        if self.sessions.iter().any(|s| s.id == pending.partner) {
            self.split = Some(Split::new(pending.side, id, pending.partner));
        }
    }

    /// Forget the split once one of its Sessions is gone. Returns the one left, so it can
    /// stay on screen.
    pub(super) fn leave_split(&mut self, id: usize) -> Option<usize> {
        let partner = self.split?.other(id)?;
        self.split = None;
        Some(partner)
    }

    /// Back to one Session: the active one fills the terminal area again.
    pub(super) fn close_split(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.split = None;
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
        } else if self.active.is_none() {
            Some(DropTarget::Main)
        } else {
            let side = if position.x < event.bounds.center().x { Side::Left } else { Side::Right };
            self.placement(&dragged.key, dragged.session, side).map(|_| DropTarget::Pane(side))
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
        let target = DropTarget::Project { id: project.to_string(), row };
        let movable = matches!(&event.drag(cx).key, ItemKey::Host { project: from, .. } if from != project);
        if event.bounds.contains(&event.event.position) && movable && !self.modal_open(cx) {
            self.set_drop_target(Some(target), cx);
        } else if self.drop_target.as_ref() == Some(&target) {
            self.set_drop_target(None, cx);
        }
    }

    pub(super) fn drop_on_project(&mut self, project: &str, dragged: &DraggedItem, cx: &mut Context<Self>) {
        let target = self.drop_target.take();
        cx.notify();
        let aimed = matches!(&target, Some(DropTarget::Project { id, .. }) if id == project);
        if let (true, ItemKey::Host { project: from, host }) = (aimed, &dragged.key) {
            self.move_host_to(host, from, project, cx);
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
        // Already open in the new Project too (a Host in both): the moved Sessions take
        // numbers that are still free there.
        let mut taken: HashSet<usize> = self.sessions_of(&new).map(|s| s.number).collect();
        for s in self.sessions.iter_mut().filter(|s| s.item == old) {
            s.item = new.clone();
            s.project_id = Some(to.to_string());
            if !taken.insert(s.number) {
                s.number = (1..).find(|n| !taken.contains(n)).unwrap_or(1);
                taken.insert(s.number);
                s.label = if s.number == 1 { name.clone().into() } else { format!("{name} ({})", s.number).into() };
            }
        }
        if let Some(id) = self.last_active.remove(&old) {
            self.last_active.entry(new.clone()).or_insert(id);
        }
        if self.collapsed_items.remove(&old) {
            self.collapsed_items.insert(new);
        }
        // Open the Project it went to, so it can be seen there.
        self.collapsed.remove(to);
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
            DropTarget::Project { .. } => return None,
        };
        let p = cx.global::<Appearance>().app;
        let blue = drop_color();
        let rect = div().flex_1().min_w_0().h_full().p(px(6.)).child(
            div()
                .size_full()
                .rounded(px(ISLAND_RADIUS - 4.))
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
    label: SharedString,
    icon: String,
    color: Hsla,
    /// Where the row was grabbed, from its top left: the chip is drawn there, under the pointer.
    offset: Point<Pixels>,
}

impl DragPreview {
    pub fn new(dragged: &DraggedItem, offset: Point<Pixels>) -> Self {
        Self { label: dragged.label.clone(), icon: dragged.icon.clone(), color: dragged.color, offset }
    }
}

impl Render for DragPreview {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p: AppPalette = cx.global::<Appearance>().app;
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
                    .rounded(px(8.))
                    .bg(p.surface.opacity(1.))
                    .border_1()
                    .border_color(p.border)
                    .shadow(elevation(p.dark))
                    .font_family(crate::theme::UI_FONT)
                    .text_size(px(12.5))
                    .text_color(p.text)
                    .child(div().flex_shrink_0().child(icon_el(&self.icon, 14., self.color)))
                    .child(div().min_w_0().truncate().child(self.label.clone())),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_split_knows_its_sides() {
        let split = Split::new(Side::Left, 7, 3);
        assert_eq!(split, Split { left: 7, right: 3 });
        assert_eq!(Split::new(Side::Right, 7, 3), Split { left: 3, right: 7 });
        assert_eq!(split.at(Side::Right), 3);
        assert_eq!(split.other(7), Some(3));
        assert_eq!(split.other(3), Some(7));
        assert_eq!(split.other(9), None);
        assert!(split.contains(3) && !split.contains(9));
    }
}
