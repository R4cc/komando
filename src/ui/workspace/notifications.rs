//! One-shot watches of the same output activity used by the sidebar spinner.
use super::*;

#[derive(Default)]
pub(super) struct CompletionWatches(HashSet<usize>);

impl CompletionWatches {
    fn contains(&self, id: usize) -> bool {
        self.0.contains(&id)
    }

    fn set(&mut self, id: usize, enabled: bool) {
        if enabled {
            self.0.insert(id);
        } else {
            self.0.remove(&id);
        }
    }

    /// Disconnecting cancels a watch; silence while connected completes it once.
    pub(super) fn update(&mut self, id: usize, busy: bool, status: &Status) -> bool {
        if *status != Status::Connected {
            self.set(id, false);
            return false;
        }
        !busy && self.0.remove(&id)
    }
}

impl Workspace {
    pub(super) fn start_notifications(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        #[cfg(windows)]
        {
            use futures::StreamExt as _;
            match crate::platform::Notifications::new(window) {
                Ok((notifications, mut clicks)) => {
                    self.notifications = Some(notifications);
                    cx.spawn_in(window, async move |this, cx| {
                        while let Some(id) = clicks.next().await {
                            if this
                                .update_in(cx, |this, window, cx| {
                                    // A notification must never reconnect or select a different
                                    // Session when its original Session has been discarded.
                                    if let Some(entry) = this.sessions.get(id) {
                                        let (item, project) =
                                            (entry.item().clone(), entry.project_id().map(str::to_string));
                                        this.collapsed_items.remove(&item);
                                        if let Some(project) = project {
                                            this.set_project_collapsed(&project, false, cx);
                                        }
                                        this.sidebar_visible = true;
                                        this.activate(id, window, cx);
                                    }
                                })
                                .is_err()
                            {
                                break;
                            }
                        }
                    })
                    .detach();
                }
                Err(error) => log::warn!("Could not set up completion notifications: {error:#}"),
            }
        }
        #[cfg(not(windows))]
        let _ = (window, cx);
    }

    pub(super) fn notify_output_done(&mut self, id: usize, label: &str, cx: &mut Context<Self>) {
        #[cfg(windows)]
        if let Some(notifications) = &self.notifications
            && let Err(error) = notifications.show(id, label)
        {
            log::warn!("Could not show completion notification: {error:#}");
            self.show_toast("Windows could not show the notification.", cx);
        }
        #[cfg(not(windows))]
        let _ = (id, label, cx);
    }

    pub(super) fn cancel_completion_notification(&mut self, id: usize) {
        self.completion_watches.set(id, false);
        #[cfg(windows)]
        if let Some(notifications) = &self.notifications {
            notifications.remove_session(id);
        }
    }
}

/// Read at menu opening, so an already-finished Session cannot offer a stale watch.
pub(super) fn completion_menu(
    menu: PopupMenu,
    workspace: &gpui::WeakEntity<Workspace>,
    ids: &[usize],
    cx: &App,
) -> PopupMenu {
    let Some(ws) = workspace.upgrade() else {
        return menu;
    };
    let this = ws.read(cx);
    #[cfg(windows)]
    let available = this.notifications.is_some();
    #[cfg(not(windows))]
    let available = false;
    if !available {
        return menu;
    }
    let armed = ids.iter().any(|id| this.completion_watches.contains(*id));
    let busy = ids.iter().any(|id| {
        this.sessions
            .iter()
            .any(|s| s.id() == *id && s.busy() && *s.status() == Status::Connected)
    });
    if !armed && !busy {
        return menu;
    }
    let ids = ids.to_vec();
    menu.separator().item(menu_item(
        if armed {
            "Cancel notification"
        } else {
            "Notify when done"
        },
        if armed { "bell-off" } else { "bell" },
        workspace,
        move |this, _, cx| {
            for &id in &ids {
                // Recheck when clicked: the menu can outlive the activity.
                let busy = this
                    .sessions
                    .iter()
                    .any(|s| s.id() == id && s.busy() && *s.status() == Status::Connected);
                this.completion_watches.set(id, !armed && busy);
            }
            cx.notify();
        },
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn completes_once_and_can_be_armed_for_another_command() {
        let mut watches = CompletionWatches::default();
        watches.set(7, true);
        assert!(!watches.update(7, true, &Status::Connected));
        assert!(watches.contains(7));
        assert!(watches.update(7, false, &Status::Connected));
        assert!(!watches.update(7, false, &Status::Connected));
        assert!(!watches.update(7, true, &Status::Connected));
        assert!(!watches.update(7, false, &Status::Connected));
        watches.set(7, true);
        assert!(watches.update(7, false, &Status::Connected));
    }

    #[test]
    fn cancellation_and_disconnect_do_not_report_completion() {
        let mut watches = CompletionWatches::default();
        watches.set(7, true);
        watches.set(7, false);
        assert!(!watches.update(7, false, &Status::Connected));
        watches.set(7, true);
        assert!(!watches.update(7, true, &Status::Disconnected("Connection lost".into())));
        assert!(!watches.update(7, false, &Status::Connected));
    }

    #[test]
    fn watches_are_independent_for_sessions_of_the_same_host() {
        let mut watches = CompletionWatches::default();
        watches.set(1, true);
        watches.set(2, true);
        assert!(!watches.update(3, false, &Status::Connected));
        assert!(watches.update(2, false, &Status::Connected));
        assert!(watches.contains(1));
        assert!(!watches.contains(2));
        assert!(watches.update(1, false, &Status::Connected));
    }
}
