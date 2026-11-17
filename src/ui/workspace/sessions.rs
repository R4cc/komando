//! The open Sessions: which exist for which sidebar item, their numbers and labels, which one
//! is shown and what is shown once it goes, and the state the sidebar and these rules read
//! (status, busy, a window of their own).
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
        Self { entries: Vec::new(), next_id: 1, numbers: HashMap::new(), active: None, last_shown: HashMap::new() }
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

    /// Take out every Session that `matches`, keeping the others in order. If the shown one
    /// goes, nothing is shown, and `show` says what to show next: the other half of its split
    /// (`split_partner`), or else another Session of its item in the main window.
    pub fn remove_where(&mut self, matches: impl Fn(&Entry<P>) -> bool, split_partner: impl Fn(usize) -> Option<usize>) -> Removed<P> {
        let (entries, kept): (Vec<_>, Vec<_>) = std::mem::take(&mut self.entries).into_iter().partition(|s| matches(s));
        self.entries = kept;
        self.last_shown.retain(|_, id| !entries.iter().any(|s| s.id == *id));
        let shown = self.active.and_then(|active| entries.iter().find(|s| s.id == active));
        let show = shown.and_then(|shown| self.next_to_show(shown.id, &shown.item, split_partner(shown.id)));
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

    /// A Remote Desktop moved into a window of its own. It is no longer what its item's row
    /// returns to, and if the main window showed it, nothing is shown there; the result is
    /// what to show instead (see `next_to_show`).
    pub fn detach(&mut self, id: usize, split_partner: Option<usize>) -> Option<usize> {
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
    fn close(sessions: &mut Sessions<()>, id: usize, split_partner: impl Fn(usize) -> Option<usize>) -> Removed<()> {
        sessions.remove_where(|s| s.id() == id, split_partner)
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
        close(&mut sessions, second, |_| None);
        let third = open(&mut sessions, &web, ssh("web01"));
        assert_eq!(label(&sessions, third), "web01 #3");
    }

    #[test]
    fn numbering_starts_over_once_an_item_has_no_terminals() {
        let mut sessions = Sessions::default();
        let web = host("web01");
        let first = open(&mut sessions, &web, ssh("web01"));
        let second = open(&mut sessions, &web, ssh("web01"));
        close(&mut sessions, first, |_| None);
        close(&mut sessions, second, |_| None);
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
        sessions.detach(id, None);
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
        sessions.detach(desktop, None);
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
        sessions.activate(web);
        let removed = close(&mut sessions, web, |id| (id == web).then_some(db));
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
        sessions.detach(desktop, None);
        sessions.activate(first);
        assert_eq!(close(&mut sessions, first, |_| None).show, Some(second));
    }

    #[test]
    fn closing_the_last_session_of_an_item_shows_nothing() {
        let mut sessions = Sessions::default();
        let web = open(&mut sessions, &host("web01"), ssh("web01"));
        open(&mut sessions, &host("db01"), ssh("db01"));
        sessions.activate(web);
        assert_eq!(close(&mut sessions, web, |_| None).show, None);
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
        let removed = close(&mut sessions, second, |_| None);
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
        close(&mut sessions, first, |_| None);
        assert_eq!(sessions.last_shown(&item), None);
    }

    #[test]
    fn deleting_both_halves_of_a_split_shows_nothing() {
        let mut sessions = Sessions::default();
        let a = open(&mut sessions, &host("a"), ssh("a"));
        let b = open(&mut sessions, &host("b"), ssh("b"));
        let kept = open(&mut sessions, &ItemKey::Host { project: "q".into(), host: "c".into() }, ssh("c"));
        sessions.activate(a);
        let partner = |id: usize| match id {
            id if id == a => Some(b),
            id if id == b => Some(a),
            _ => None,
        };
        let removed = sessions.remove_where(|s| s.project_id() == Some("p"), partner);
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
        sessions.activate(desktop);
        assert_eq!(sessions.detach(desktop, Some(db)), Some(db));
        assert_eq!(sessions.active(), None);
        assert_eq!(sessions.last_shown(&item), None);
        sessions.reattach(desktop);
        sessions.activate(desktop);
        assert_eq!(sessions.detach(desktop, None), Some(terminal));
    }

    #[test]
    fn detaching_a_desktop_that_is_not_shown_leaves_the_shown_session() {
        let mut sessions = Sessions::default();
        let item = host("web01");
        let terminal = open(&mut sessions, &item, ssh("web01"));
        let desktop = open(&mut sessions, &item, rdp("web01"));
        sessions.activate(desktop);
        sessions.activate(terminal);
        assert_eq!(sessions.detach(desktop, None), None);
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
    fn removing_where_keeps_the_others_in_order() {
        let mut sessions = Sessions::default();
        let a = open(&mut sessions, &host("a"), ssh("a"));
        let b = open(&mut sessions, &host("b"), ssh("b"));
        let c = open(&mut sessions, &host("a"), ssh("a"));
        let d = open(&mut sessions, &host("c"), ssh("c"));
        let gone: Vec<usize> = sessions.remove_where(|s| s.item() == &host("a"), |_| None).entries.iter().map(|s| s.id()).collect();
        assert_eq!(gone, vec![a, c]);
        assert_eq!(sessions.iter().map(|s| s.id()).collect::<Vec<_>>(), vec![b, d]);
    }
}
