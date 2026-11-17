//! The open Sessions: which exist for which sidebar item, their numbers and labels, which one
//! is shown and what is shown once it goes, Split View, and the state the sidebar and these
//! rules read (status, busy, a window of their own).
//!
//! Plain data with no gpui context, so its rules are tested without a window. Each Session
//! carries a payload `P` with what only the UI needs (its view, file browser, subscriptions);
//! the rest of an entry changes only through `Sessions`.

use std::collections::{HashMap, HashSet};

use gpui::SharedString;

use super::{ItemKey, SessionKind};
use crate::session::Status;

pub(super) struct Sessions<P> {
    /// In the order they were opened.
    entries: Vec<Entry<P>>,
    next_id: usize,
    /// The highest number given to each item's terminals (`true`) or desktops since it last
    /// had none of them open (see `next_number`).
    numbers: HashMap<(ItemKey, bool), usize>,
    /// The Session shown in the main window.
    active: Option<usize>,
    /// The Session last shown for each item, so a click returns to it.
    last_shown: HashMap<ItemKey, usize>,
    /// Two Sessions side by side, shown while either is active.
    split: Option<Split>,
    /// A split waiting for the new Session dropped into it.
    pending_split: Option<PendingSplit>,
}

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

/// What putting an item in the split did, for the caller to finish.
pub(super) enum Placed {
    /// The split is set: show this Session.
    Show(usize),
    /// The split is set with this Session in it: show it, and reconnect it if it dropped.
    Existing(usize),
    /// A new Session of the item goes there: open one, after `wait_for_split`.
    New(PendingSplit),
}

/// What showing a Session comes to.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum Shown {
    /// It is the main window's Session now.
    Here,
    /// A desktop in a window of its own: that window is to come to the front instead.
    InOwnWindow,
    /// No such Session.
    Gone,
}

/// Sessions taken out of the list, and what the main window shows next if the shown one was
/// among them.
pub(super) struct Removed<P> {
    pub entries: Vec<Entry<P>>,
    pub show: Option<usize>,
}

/// One open Session.
pub(super) struct Entry<P> {
    id: usize,
    item: ItemKey,
    kind: SessionKind,
    /// "web01", "web01 #2" when the same Host is open twice, or "win-build · RDP".
    label: SharedString,
    /// Its number among the item's terminals (or desktops): one past the highest open when it
    /// was opened.
    number: usize,
    /// A name given with Rename session…, shown instead of the shell's title.
    name: Option<SharedString>,
    /// Its view's status, copied here when it changes. The sidebar reads this rather than the
    /// view: reading a terminal while rendering makes gpui redraw the whole window on every bit
    /// of output it gets, even when it isn't on screen.
    status: Status,
    /// Output is streaming in (a command at work), copied here like `status`.
    busy: bool,
    /// A Remote Desktop dragged out into a window of its own.
    own_window: bool,
    pub ui: P,
}

impl<P> Entry<P> {
    pub fn id(&self) -> usize {
        self.id
    }

    pub fn item(&self) -> &ItemKey {
        &self.item
    }

    /// The Project it was opened from; `None` for a standalone Local Shell.
    pub fn project_id(&self) -> Option<&str> {
        match &self.item {
            ItemKey::Host { project, .. } => Some(project),
            ItemKey::Shell { project, .. } => project.as_deref(),
        }
    }

    pub fn kind(&self) -> &SessionKind {
        &self.kind
    }

    pub fn is_desktop(&self) -> bool {
        matches!(self.kind, SessionKind::Desktop { .. })
    }

    pub fn label(&self) -> &SharedString {
        &self.label
    }

    pub fn number(&self) -> usize {
        self.number
    }

    pub fn name(&self) -> Option<&SharedString> {
        self.name.as_ref()
    }

    pub fn status(&self) -> &Status {
        &self.status
    }

    pub fn busy(&self) -> bool {
        self.busy
    }

    pub fn own_window(&self) -> bool {
        self.own_window
    }

    pub fn is_live(&self) -> bool {
        !matches!(self.status, Status::Disconnected(_))
    }
}

impl<P> Default for Sessions<P> {
    fn default() -> Self {
        Self {
            entries: Vec::new(),
            next_id: 1,
            numbers: HashMap::new(),
            active: None,
            last_shown: HashMap::new(),
            split: None,
            pending_split: None,
        }
    }
}

impl<P> Sessions<P> {
    /// Open a Session for `item`: it is numbered and labelled, and `make` builds its payload
    /// from its id and label (the view and its subscriptions need both). It starts connecting.
    pub fn add(&mut self, item: ItemKey, kind: SessionKind, base_label: &str, make: impl FnOnce(usize, &SharedString) -> P) -> usize {
        let desktop = matches!(kind, SessionKind::Desktop { .. });
        let number = self.next_number(&item, !desktop);
        let label: SharedString = match &kind {
            SessionKind::Desktop { protocol, .. } => format!("{base_label} · {}", protocol.label()).into(),
            _ if number == 1 => base_label.to_string().into(),
            _ => format!("{base_label} #{number}").into(),
        };
        let id = self.next_id;
        self.next_id += 1;
        let ui = make(id, &label);
        self.entries.push(Entry {
            id,
            item,
            kind,
            label,
            number,
            name: None,
            status: Status::Connecting,
            busy: false,
            own_window: false,
            ui,
        });
        self.take_pending_split(id);
        id
    }

    /// The number a new terminal (or Remote Desktop) of an item gets: one past the highest
    /// given to its kind since the item last had none open, so a number is never given to
    /// another Session while the item has Sessions of that kind, and a desktop never makes the
    /// second terminal "#3". Back to 1 once all of them are closed.
    fn next_number(&mut self, item: &ItemKey, terminal: bool) -> usize {
        let open = self.of(item).filter(|s| s.is_desktop() != terminal).map(|s| s.number).max();
        let counter = (item.clone(), terminal);
        let number = match open {
            None => 1,
            Some(highest) => highest.max(self.numbers.get(&counter).copied().unwrap_or(0)) + 1,
        };
        self.numbers.insert(counter, number);
        number
    }

    pub fn get(&self, id: usize) -> Option<&Entry<P>> {
        self.entries.iter().find(|s| s.id == id)
    }

    pub fn get_mut(&mut self, id: usize) -> Option<&mut Entry<P>> {
        self.entries.iter_mut().find(|s| s.id == id)
    }

    pub fn contains(&self, id: usize) -> bool {
        self.get(id).is_some()
    }

    /// Every Session, in the order they were opened.
    pub fn iter(&self) -> impl Iterator<Item = &Entry<P>> {
        self.entries.iter()
    }

    pub fn iter_mut(&mut self) -> impl Iterator<Item = &mut Entry<P>> {
        self.entries.iter_mut()
    }

    /// An item's Sessions, in the order they were opened.
    pub fn of(&self, item: &ItemKey) -> impl Iterator<Item = &Entry<P>> {
        let item = item.clone();
        self.entries.iter().filter(move |s| s.item == item)
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// The Session shown in the main window.
    pub fn active(&self) -> Option<usize> {
        self.active
    }

    pub fn active_entry(&self) -> Option<&Entry<P>> {
        self.get(self.active?)
    }

    /// The Session last shown for an item, while it is open.
    pub fn last_shown(&self, item: &ItemKey) -> Option<usize> {
        self.last_shown.get(item).copied()
    }

    /// Show a Session in the main window, unless it is a desktop in a window of its own.
    pub fn activate(&mut self, id: usize) -> Shown {
        let Some(entry) = self.get(id) else { return Shown::Gone };
        if entry.own_window {
            return Shown::InOwnWindow;
        }
        let item = entry.item.clone();
        self.active = Some(id);
        self.last_shown.insert(item, id);
        Shown::Here
    }

    /// Take out every Session that `matches`, keeping the others in order. A split one of
    /// them was in is forgotten. If the shown one goes, nothing is shown, and `show` says what
    /// to show next: the other half of its split, or else another Session of its item in the
    /// main window.
    pub fn remove_where(&mut self, matches: impl Fn(&Entry<P>) -> bool) -> Removed<P> {
        let (entries, kept): (Vec<_>, Vec<_>) = std::mem::take(&mut self.entries).into_iter().partition(|s| matches(s));
        self.entries = kept;
        self.last_shown.retain(|_, id| !entries.iter().any(|s| s.id == *id));
        let split = self.split;
        if split.is_some_and(|split| entries.iter().any(|s| split.contains(s.id))) {
            self.split = None;
        }
        let shown = self.active.and_then(|active| entries.iter().find(|s| s.id == active));
        let show = shown.and_then(|shown| self.next_to_show(shown.id, &shown.item, split.and_then(|s| s.other(shown.id))));
        if shown.is_some() {
            self.active = None;
        }
        Removed { entries, show }
    }

    /// What the main window shows once `id` leaves it: the other half of its split if that is
    /// still open, or else another of `item`'s Sessions that isn't in a window of its own.
    fn next_to_show(&self, id: usize, item: &ItemKey, split_partner: Option<usize>) -> Option<usize> {
        split_partner
            .filter(|&partner| partner != id && self.contains(partner))
            .or_else(|| self.of(item).find(|s| s.id != id && !s.own_window).map(|s| s.id))
    }

    /// Copy a view's status in. Returns whether it changed.
    pub fn set_status(&mut self, id: usize, status: Status) -> bool {
        let Some(entry) = self.get_mut(id) else { return false };
        let changed = entry.status != status;
        entry.status = status;
        changed
    }

    pub fn set_busy(&mut self, id: usize, busy: bool) {
        if let Some(entry) = self.get_mut(id) {
            entry.busy = busy;
        }
    }

    /// Rename session…; `None` goes back to the shell's title.
    pub fn set_name(&mut self, id: usize, name: Option<SharedString>) {
        if let Some(entry) = self.get_mut(id) {
            entry.name = name;
        }
    }

    /// The split on screen: the remembered one, while one of its Sessions is shown.
    pub fn shown_split(&self) -> Option<Split> {
        let split = self.split?;
        self.active.filter(|&id| split.contains(id)).map(|_| split)
    }

    /// Whether a Session is in the remembered split, shown or not.
    pub fn in_split(&self, id: usize) -> bool {
        self.split.is_some_and(|split| split.contains(id))
    }

    /// Back to one Session: the shown one fills the terminal area again.
    pub fn close_split(&mut self) {
        self.split = None;
    }

    /// Forget the split once one of its Sessions leaves it. Returns the one left.
    fn leave_split(&mut self, id: usize) -> Option<usize> {
        let partner = self.split?.other(id)?;
        self.split = None;
        Some(partner)
    }

    /// What dropping `item` (or its `session`) on `side` would do, and the Session that stays
    /// on the other side, or `None` when it would change nothing (it is already there).
    fn placement(&self, item: &ItemKey, session: Option<usize>, side: Side) -> Option<(Placement, usize)> {
        let active = self.active?;
        let shown = self.shown_split();
        // The Session that stays on the other side.
        let other = shown.map_or(active, |s| s.at(side.opposite()));
        let here = shown.map(|s| s.at(side));
        let item_of = |id: usize| self.get(id).map(|s| &s.item);
        // Dragging the other half's Session (or its item) across swaps the two.
        if shown.is_some() && (session == Some(other) || (session.is_none() && item_of(other) == Some(item))) {
            return Some((Placement::Swap, other));
        }
        let placed = match session {
            Some(id) if id == other => return None,
            Some(id) => Placement::Existing(id),
            // The item's Session last shown, unless that one stays on the other side: then
            // another of its Sessions, or a new one (two shells on one server).
            None => {
                // Not a desktop in its own window: the split would show its placeholder.
                let in_main = |id: usize| self.get(id).is_some_and(|s| !s.own_window);
                let last = self.last_shown(item).filter(|&id| id != other && item_of(id) == Some(item) && in_main(id));
                match last.or_else(|| self.of(item).map(|s| s.id).find(|&id| id != other && in_main(id))) {
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

    /// Whether dropping `item` (or its `session`) on `side` would change anything.
    pub fn can_place(&self, item: &ItemKey, session: Option<usize>, side: Side) -> bool {
        self.placement(item, session, side).is_some()
    }

    /// Put `item` (or its `session`) on `side` of the terminal area, beside what is shown.
    /// `None` when that changes nothing.
    pub fn place(&mut self, item: &ItemKey, session: Option<usize>, side: Side) -> Option<Placed> {
        let (placement, other) = self.placement(item, session, side)?;
        Some(match placement {
            Placement::Swap => {
                let split = self.shown_split()?;
                self.split = Some(Split { left: split.right, right: split.left });
                Placed::Show(other)
            }
            Placement::Existing(id) => {
                self.split = Some(Split::new(side, id, other));
                Placed::Existing(id)
            }
            Placement::New => Placed::New(PendingSplit { item: item.clone(), partner: other, side }),
        })
    }

    /// The new Session about to be opened for a `Placed::New` goes into its split once it is
    /// added (after the Vault, if the Host needs it).
    pub fn wait_for_split(&mut self, pending: PendingSplit) {
        self.pending_split = Some(pending);
    }

    /// A Session was just added: put it in the split it was opened for.
    fn take_pending_split(&mut self, id: usize) {
        let Some(item) = self.get(id).map(|s| s.item.clone()) else { return };
        let Some(pending) = self.pending_split.take_if(|p| p.item == item) else { return };
        if self.contains(pending.partner) {
            self.split = Some(Split::new(pending.side, id, pending.partner));
        }
    }

    /// An item's Sessions now belong to another: a Host moved to another Project, or a Local
    /// Shell renamed (`kind` is then the shell as now saved, to reconnect with). They stay
    /// open. Where `to` already has Sessions (a Host in both Projects), a moved one whose
    /// number is taken there gets the next free one, terminals and desktops each counted on
    /// their own, and a terminal's label shows it.
    pub fn move_item(&mut self, from: &ItemKey, to: &ItemKey, base_label: &str, kind: Option<SessionKind>) {
        let taken_of = |terminal: bool| -> HashSet<usize> {
            self.of(to).filter(|s| s.is_desktop() != terminal).map(|s| s.number).collect()
        };
        let mut taken = [taken_of(false), taken_of(true)];
        if let Some(id) = self.last_shown.remove(from) {
            self.last_shown.entry(to.clone()).or_insert(id);
        }
        for entry in self.entries.iter_mut().filter(|s| s.item == *from) {
            entry.item = to.clone();
            if let Some(kind) = &kind {
                entry.kind = kind.clone();
            }
            let terminal = !entry.is_desktop();
            let taken = &mut taken[usize::from(terminal)];
            if !taken.insert(entry.number) {
                entry.number = taken.iter().max().map_or(1, |n| n + 1);
                taken.insert(entry.number);
                // A desktop's label names its protocol, not a number.
                if terminal {
                    entry.label = if entry.number == 1 { base_label.to_string().into() } else { format!("{base_label} #{}", entry.number).into() };
                }
            }
        }
    }

    /// A Remote Desktop moved into a window of its own. It leaves its split, it is no longer
    /// what its item's row returns to, and if the main window showed it, nothing is shown
    /// there; the result is what to show instead (see `next_to_show`).
    pub fn detach(&mut self, id: usize) -> Option<usize> {
        let split_partner = self.leave_split(id);
        let entry = self.get_mut(id)?;
        entry.own_window = true;
        let item = entry.item.clone();
        if self.last_shown.get(&item) == Some(&id) {
            self.last_shown.remove(&item);
        }
        if self.active != Some(id) {
            return None;
        }
        self.active = None;
        self.next_to_show(id, &item, split_partner)
    }

    /// A desktop's own window closed: it belongs to the main window again (not shown yet).
    pub fn reattach(&mut self, id: usize) {
        if let Some(entry) = self.get_mut(id) {
            entry.own_window = false;
        }
    }
}

impl<'a, P> IntoIterator for &'a Sessions<P> {
    type Item = &'a Entry<P>;
    type IntoIter = std::slice::Iter<'a, Entry<P>>;

    fn into_iter(self) -> Self::IntoIter {
        self.entries.iter()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Protocol;

    fn host(name: &str) -> ItemKey {
        ItemKey::Host { project: "p".into(), host: name.into() }
    }

    fn ssh(host: &str) -> SessionKind {
        SessionKind::Ssh { host_id: host.into() }
    }

    fn rdp(host: &str) -> SessionKind {
        SessionKind::Desktop { host_id: host.into(), protocol: Protocol::Rdp }
    }

    fn open(sessions: &mut Sessions<()>, item: &ItemKey, kind: SessionKind) -> usize {
        sessions.add(item.clone(), kind, "web01", |_, _| ())
    }

    /// Close one Session, as × does.
    fn close(sessions: &mut Sessions<()>, id: usize) -> Removed<()> {
        sessions.remove_where(|s| s.id() == id)
    }

    /// Show `left` and `right` side by side, `left` active.
    fn split(sessions: &mut Sessions<()>, left: usize, right: usize) {
        sessions.activate(right);
        assert!(matches!(sessions.place(&sessions.get(left).unwrap().item().clone(), Some(left), Side::Left), Some(Placed::Existing(_))));
        sessions.activate(left);
        assert_eq!(sessions.shown_split(), Some(Split { left, right }));
    }

    fn label(sessions: &Sessions<()>, id: usize) -> String {
        sessions.get(id).unwrap().label().to_string()
    }

    #[test]
    fn a_second_terminal_of_an_item_is_numbered_in_its_label() {
        let mut sessions = Sessions::default();
        let web = host("web01");
        let first = open(&mut sessions, &web, ssh("web01"));
        let second = open(&mut sessions, &web, ssh("web01"));
        assert_eq!(label(&sessions, first), "web01");
        assert_eq!(label(&sessions, second), "web01 #2");
        assert_eq!(sessions.get(second).unwrap().number(), 2);
    }

    #[test]
    fn a_desktop_takes_no_terminal_number() {
        let mut sessions = Sessions::default();
        let web = host("web01");
        open(&mut sessions, &web, ssh("web01"));
        let desktop = open(&mut sessions, &web, rdp("web01"));
        let terminal = open(&mut sessions, &web, ssh("web01"));
        assert_eq!(label(&sessions, desktop), "web01 · RDP");
        assert_eq!(sessions.get(desktop).unwrap().number(), 1);
        assert_eq!(label(&sessions, terminal), "web01 #2");
    }

    #[test]
    fn a_number_is_not_given_again_while_the_item_has_terminals_open() {
        let mut sessions = Sessions::default();
        let web = host("web01");
        open(&mut sessions, &web, ssh("web01"));
        let second = open(&mut sessions, &web, ssh("web01"));
        close(&mut sessions, second);
        let third = open(&mut sessions, &web, ssh("web01"));
        assert_eq!(label(&sessions, third), "web01 #3");
    }

    #[test]
    fn numbering_starts_over_once_an_item_has_no_terminals() {
        let mut sessions = Sessions::default();
        let web = host("web01");
        let first = open(&mut sessions, &web, ssh("web01"));
        let second = open(&mut sessions, &web, ssh("web01"));
        close(&mut sessions, first);
        close(&mut sessions, second);
        let again = open(&mut sessions, &web, ssh("web01"));
        assert_eq!(label(&sessions, again), "web01");
    }

    #[test]
    fn items_are_numbered_separately() {
        let mut sessions = Sessions::default();
        open(&mut sessions, &host("web01"), ssh("web01"));
        let other = open(&mut sessions, &host("db01"), ssh("db01"));
        assert_eq!(sessions.get(other).unwrap().number(), 1);
    }

    #[test]
    fn the_payload_is_built_with_the_sessions_id_and_label() {
        let mut sessions = Sessions::default();
        let web = host("web01");
        sessions.add(web.clone(), ssh("web01"), "web01", |id, label| (id, label.to_string()));
        let id = sessions.add(web, ssh("web01"), "web01", |id, label| (id, label.to_string()));
        assert_eq!(sessions.get(id).unwrap().ui, (id, "web01 #2".to_string()));
    }

    #[test]
    fn a_session_knows_its_project_from_its_item() {
        let mut sessions = Sessions::default();
        let id = open(&mut sessions, &host("web01"), ssh("web01"));
        let shell = ItemKey::Shell { project: None, name: "pwsh".into() };
        let standalone = sessions.add(shell, SessionKind::Local { shell: Default::default() }, "pwsh", |_, _| ());
        assert_eq!(sessions.get(id).unwrap().project_id(), Some("p"));
        assert_eq!(sessions.get(standalone).unwrap().project_id(), None);
    }

    #[test]
    fn a_new_session_is_connecting_and_live() {
        let mut sessions = Sessions::default();
        let id = open(&mut sessions, &host("web01"), ssh("web01"));
        let entry = sessions.get(id).unwrap();
        assert_eq!(entry.status(), &Status::Connecting);
        assert!(entry.is_live() && !entry.busy() && !entry.own_window() && entry.name().is_none());
    }

    #[test]
    fn set_status_says_whether_it_changed() {
        let mut sessions = Sessions::default();
        let id = open(&mut sessions, &host("web01"), ssh("web01"));
        assert!(sessions.set_status(id, Status::Connected));
        assert!(!sessions.set_status(id, Status::Connected));
        assert!(sessions.set_status(id, Status::Disconnected("gone".into())));
        assert!(!sessions.get(id).unwrap().is_live());
        assert!(!sessions.set_status(99, Status::Connected));
    }

    #[test]
    fn a_moved_host_keeps_its_numbers_where_they_are_free() {
        let mut sessions = Sessions::default();
        let (from, to) = (host("web01"), ItemKey::Host { project: "q".into(), host: "web01".into() });
        let first = open(&mut sessions, &from, ssh("web01"));
        let second = open(&mut sessions, &from, ssh("web01"));
        sessions.move_item(&from, &to, "web01", None);
        assert_eq!(sessions.of(&to).map(|s| s.id()).collect::<Vec<_>>(), vec![first, second]);
        assert_eq!(sessions.get(first).unwrap().project_id(), Some("q"));
        assert_eq!(label(&sessions, second), "web01 #2");
        assert_eq!(sessions.of(&from).count(), 0);
    }

    #[test]
    fn a_host_moved_where_it_is_open_takes_the_next_free_numbers() {
        let mut sessions = Sessions::default();
        let (from, to) = (host("web01"), ItemKey::Host { project: "q".into(), host: "web01".into() });
        open(&mut sessions, &to, ssh("web01"));
        open(&mut sessions, &to, rdp("web01"));
        let terminal = open(&mut sessions, &from, ssh("web01"));
        let desktop = open(&mut sessions, &from, rdp("web01"));
        sessions.move_item(&from, &to, "web01", None);
        assert_eq!(sessions.get(terminal).unwrap().number(), 2);
        assert_eq!(label(&sessions, terminal), "web01 #2");
        // A desktop's label names its protocol, not a number.
        assert_eq!(sessions.get(desktop).unwrap().number(), 2);
        assert_eq!(label(&sessions, desktop), "web01 · RDP");
    }

    #[test]
    fn a_renamed_shells_sessions_follow_it_with_the_shell_as_saved() {
        let mut sessions = Sessions::default();
        let (old, new) = (ItemKey::Shell { project: None, name: "pwsh".into() }, ItemKey::Shell { project: None, name: "PowerShell".into() });
        let id = sessions.add(old.clone(), SessionKind::Local { shell: Default::default() }, "pwsh", |_, _| ());
        let renamed = crate::model::LocalShell { name: "PowerShell".into(), ..Default::default() };
        sessions.move_item(&old, &new, "PowerShell", Some(SessionKind::Local { shell: renamed }));
        let entry = sessions.get(id).unwrap();
        assert_eq!(entry.item(), &new);
        assert!(matches!(entry.kind(), SessionKind::Local { shell } if shell.name == "PowerShell"));
    }

    #[test]
    fn a_session_in_its_own_window_says_so() {
        let mut sessions = Sessions::default();
        let id = open(&mut sessions, &host("web01"), rdp("web01"));
        sessions.detach(id);
        assert!(sessions.get(id).unwrap().own_window());
        sessions.reattach(id);
        assert!(!sessions.get(id).unwrap().own_window());
    }

    #[test]
    fn showing_a_session_remembers_it_for_its_item() {
        let mut sessions = Sessions::default();
        let web = host("web01");
        let first = open(&mut sessions, &web, ssh("web01"));
        let second = open(&mut sessions, &web, ssh("web01"));
        assert_eq!(sessions.activate(second), Shown::Here);
        assert_eq!(sessions.activate(first), Shown::Here);
        assert_eq!(sessions.active(), Some(first));
        assert_eq!(sessions.active_entry().map(|s| s.id()), Some(first));
        assert_eq!(sessions.last_shown(&web), Some(first));
    }

    #[test]
    fn a_desktop_in_its_own_window_is_raised_not_shown() {
        let mut sessions = Sessions::default();
        let web = host("web01");
        let terminal = open(&mut sessions, &web, ssh("web01"));
        let desktop = open(&mut sessions, &web, rdp("web01"));
        sessions.activate(terminal);
        sessions.detach(desktop);
        assert_eq!(sessions.activate(desktop), Shown::InOwnWindow);
        assert_eq!(sessions.active(), Some(terminal));
        assert_eq!(sessions.activate(99), Shown::Gone);
        assert_eq!(sessions.active(), Some(terminal));
    }

    #[test]
    fn closing_the_shown_session_shows_its_split_partner() {
        let mut sessions = Sessions::default();
        let web = open(&mut sessions, &host("web01"), ssh("web01"));
        open(&mut sessions, &host("web01"), ssh("web01"));
        let db = open(&mut sessions, &host("db01"), ssh("db01"));
        split(&mut sessions, web, db);
        let removed = close(&mut sessions, web);
        assert_eq!(removed.entries.iter().map(|s| s.id()).collect::<Vec<_>>(), vec![web]);
        assert_eq!(removed.show, Some(db));
        assert_eq!(sessions.active(), None);
    }

    #[test]
    fn closing_the_shown_session_outside_a_split_shows_another_of_its_item_in_this_window() {
        let mut sessions = Sessions::default();
        let item = host("web01");
        let desktop = open(&mut sessions, &item, rdp("web01"));
        let first = open(&mut sessions, &item, ssh("web01"));
        let second = open(&mut sessions, &item, ssh("web01"));
        open(&mut sessions, &host("db01"), ssh("db01"));
        sessions.detach(desktop);
        sessions.activate(first);
        assert_eq!(close(&mut sessions, first).show, Some(second));
    }

    #[test]
    fn closing_the_last_session_of_an_item_shows_nothing() {
        let mut sessions = Sessions::default();
        let web = open(&mut sessions, &host("web01"), ssh("web01"));
        open(&mut sessions, &host("db01"), ssh("db01"));
        sessions.activate(web);
        assert_eq!(close(&mut sessions, web).show, None);
        assert_eq!(sessions.active(), None);
    }

    #[test]
    fn closing_a_session_that_is_not_shown_leaves_the_shown_one() {
        let mut sessions = Sessions::default();
        let item = host("web01");
        let first = open(&mut sessions, &item, ssh("web01"));
        let second = open(&mut sessions, &item, ssh("web01"));
        sessions.activate(second);
        sessions.activate(first);
        let removed = close(&mut sessions, second);
        assert_eq!(removed.show, None);
        assert_eq!(sessions.active(), Some(first));
        assert_eq!(sessions.last_shown(&item), Some(first));
    }

    #[test]
    fn a_closed_session_is_no_longer_what_its_item_returns_to() {
        let mut sessions = Sessions::default();
        let item = host("web01");
        let first = open(&mut sessions, &item, ssh("web01"));
        let other = open(&mut sessions, &host("db01"), ssh("db01"));
        sessions.activate(first);
        sessions.activate(other);
        close(&mut sessions, first);
        assert_eq!(sessions.last_shown(&item), None);
    }

    #[test]
    fn deleting_both_halves_of_a_split_shows_nothing() {
        let mut sessions = Sessions::default();
        let a = open(&mut sessions, &host("a"), ssh("a"));
        let b = open(&mut sessions, &host("b"), ssh("b"));
        let kept = open(&mut sessions, &ItemKey::Host { project: "q".into(), host: "c".into() }, ssh("c"));
        split(&mut sessions, a, b);
        let removed = sessions.remove_where(|s| s.project_id() == Some("p"));
        assert_eq!(removed.entries.len(), 2);
        assert_eq!(removed.show, None);
        assert_eq!(sessions.iter().map(|s| s.id()).collect::<Vec<_>>(), vec![kept]);
    }

    #[test]
    fn detaching_the_shown_desktop_shows_its_split_partner_or_its_items_terminal() {
        let mut sessions = Sessions::default();
        let item = host("web01");
        let terminal = open(&mut sessions, &item, ssh("web01"));
        let desktop = open(&mut sessions, &item, rdp("web01"));
        let db = open(&mut sessions, &host("db01"), ssh("db01"));
        split(&mut sessions, desktop, db);
        assert_eq!(sessions.detach(desktop), Some(db));
        assert_eq!(sessions.shown_split(), None);
        assert_eq!(sessions.active(), None);
        assert_eq!(sessions.last_shown(&item), None);
        sessions.reattach(desktop);
        sessions.activate(desktop);
        assert_eq!(sessions.detach(desktop), Some(terminal));
    }

    #[test]
    fn detaching_a_desktop_that_is_not_shown_leaves_the_shown_session() {
        let mut sessions = Sessions::default();
        let item = host("web01");
        let terminal = open(&mut sessions, &item, ssh("web01"));
        let desktop = open(&mut sessions, &item, rdp("web01"));
        sessions.activate(desktop);
        sessions.activate(terminal);
        assert_eq!(sessions.detach(desktop), None);
        assert_eq!(sessions.active(), Some(terminal));
        assert_eq!(sessions.last_shown(&item), Some(terminal));
    }

    #[test]
    fn a_moved_item_keeps_what_it_returns_to_unless_the_target_has_its_own() {
        let mut sessions = Sessions::default();
        let (from, to) = (host("web01"), ItemKey::Host { project: "q".into(), host: "web01".into() });
        let moved = open(&mut sessions, &from, ssh("web01"));
        sessions.activate(moved);
        sessions.move_item(&from, &to, "web01", None);
        assert_eq!(sessions.last_shown(&to), Some(moved));
        assert_eq!(sessions.last_shown(&from), None);

        let there = open(&mut sessions, &from, ssh("web01"));
        sessions.activate(there);
        sessions.activate(moved);
        sessions.move_item(&from, &to, "web01", None);
        assert_eq!(sessions.last_shown(&to), Some(moved));
    }

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

    #[test]
    fn nothing_is_placed_in_a_split_while_nothing_is_shown() {
        let mut sessions = Sessions::default();
        let web = host("web01");
        open(&mut sessions, &web, ssh("web01"));
        assert!(!sessions.can_place(&web, None, Side::Right));
        assert!(sessions.place(&web, None, Side::Right).is_none());
    }

    #[test]
    fn an_item_dropped_beside_the_shown_session_brings_its_last_shown_session() {
        let mut sessions = Sessions::default();
        let (web, db) = (host("web01"), host("db01"));
        let first = open(&mut sessions, &web, ssh("web01"));
        let second = open(&mut sessions, &web, ssh("web01"));
        let shown = open(&mut sessions, &db, ssh("db01"));
        sessions.activate(second);
        sessions.activate(first);
        sessions.activate(shown);
        assert!(matches!(sessions.place(&web, None, Side::Right), Some(Placed::Existing(id)) if id == first));
        assert_eq!(sessions.split, Some(Split { left: shown, right: first }));
    }

    #[test]
    fn the_shown_item_dropped_beside_itself_opens_another_session_into_the_split() {
        let mut sessions = Sessions::default();
        let web = host("web01");
        let shown = open(&mut sessions, &web, ssh("web01"));
        sessions.activate(shown);
        let Some(Placed::New(pending)) = sessions.place(&web, None, Side::Left) else { panic!("expected a new Session") };
        assert_eq!(sessions.shown_split(), None);
        sessions.wait_for_split(pending);
        let new = open(&mut sessions, &web, ssh("web01"));
        sessions.activate(new);
        assert_eq!(sessions.shown_split(), Some(Split { left: new, right: shown }));
    }

    #[test]
    fn a_desktop_in_its_own_window_is_not_brought_into_a_split() {
        let mut sessions = Sessions::default();
        let (web, db) = (host("web01"), host("db01"));
        let desktop = open(&mut sessions, &web, rdp("web01"));
        let shown = open(&mut sessions, &db, ssh("db01"));
        sessions.detach(desktop);
        sessions.activate(shown);
        assert!(matches!(sessions.place(&web, None, Side::Right), Some(Placed::New(_))));
    }

    #[test]
    fn a_pending_split_whose_partner_closed_is_dropped() {
        let mut sessions = Sessions::default();
        let (web, db) = (host("web01"), host("db01"));
        let shown = open(&mut sessions, &db, ssh("db01"));
        sessions.activate(shown);
        let Some(Placed::New(pending)) = sessions.place(&web, None, Side::Right) else { panic!("expected a new Session") };
        sessions.wait_for_split(pending);
        close(&mut sessions, shown);
        let new = open(&mut sessions, &web, ssh("web01"));
        assert!(!sessions.in_split(new));
        assert!(sessions.pending_split.is_none());
    }

    #[test]
    fn the_other_halfs_item_dropped_across_swaps_the_sides() {
        let mut sessions = Sessions::default();
        let (web, db) = (host("web01"), host("db01"));
        let left = open(&mut sessions, &web, ssh("web01"));
        let right = open(&mut sessions, &db, ssh("db01"));
        split(&mut sessions, left, right);
        assert!(matches!(sessions.place(&db, None, Side::Left), Some(Placed::Show(id)) if id == right));
        assert_eq!(sessions.split, Some(Split { left: right, right: left }));
    }

    #[test]
    fn a_session_dropped_where_it_already_is_changes_nothing() {
        let mut sessions = Sessions::default();
        let left = open(&mut sessions, &host("web01"), ssh("web01"));
        let right = open(&mut sessions, &host("db01"), ssh("db01"));
        split(&mut sessions, left, right);
        assert!(!sessions.can_place(&host("db01"), Some(right), Side::Right));
        assert!(!sessions.can_place(&host("web01"), Some(left), Side::Left));
        // Across, it trades places with the other half instead.
        assert!(sessions.can_place(&host("web01"), Some(left), Side::Right));
    }

    #[test]
    fn a_split_comes_back_when_either_half_is_shown_again() {
        let mut sessions = Sessions::default();
        let left = open(&mut sessions, &host("web01"), ssh("web01"));
        let right = open(&mut sessions, &host("db01"), ssh("db01"));
        let other = open(&mut sessions, &host("app01"), ssh("app01"));
        split(&mut sessions, left, right);
        sessions.activate(other);
        assert_eq!(sessions.shown_split(), None);
        assert!(sessions.in_split(left));
        sessions.activate(right);
        assert_eq!(sessions.shown_split(), Some(Split { left, right }));
    }

    #[test]
    fn closing_half_of_a_split_that_is_not_shown_forgets_the_split() {
        let mut sessions = Sessions::default();
        let left = open(&mut sessions, &host("web01"), ssh("web01"));
        let right = open(&mut sessions, &host("db01"), ssh("db01"));
        let other = open(&mut sessions, &host("app01"), ssh("app01"));
        split(&mut sessions, left, right);
        sessions.activate(other);
        assert_eq!(close(&mut sessions, left).show, None);
        assert!(!sessions.in_split(right));
        assert_eq!(sessions.active(), Some(other));
    }

    #[test]
    fn closing_the_split_keeps_the_shown_session() {
        let mut sessions = Sessions::default();
        let left = open(&mut sessions, &host("web01"), ssh("web01"));
        let right = open(&mut sessions, &host("db01"), ssh("db01"));
        split(&mut sessions, left, right);
        sessions.close_split();
        assert_eq!(sessions.shown_split(), None);
        assert_eq!(sessions.active(), Some(left));
    }

    #[test]
    fn removing_where_keeps_the_others_in_order() {
        let mut sessions = Sessions::default();
        let a = open(&mut sessions, &host("a"), ssh("a"));
        let b = open(&mut sessions, &host("b"), ssh("b"));
        let c = open(&mut sessions, &host("a"), ssh("a"));
        let d = open(&mut sessions, &host("c"), ssh("c"));
        let gone: Vec<usize> = sessions.remove_where(|s| s.item() == &host("a")).entries.iter().map(|s| s.id()).collect();
        assert_eq!(gone, vec![a, c]);
        assert_eq!(sessions.iter().map(|s| s.id()).collect::<Vec<_>>(), vec![b, d]);
    }
}
