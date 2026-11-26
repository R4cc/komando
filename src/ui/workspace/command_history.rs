//! The Command History panel: a Host's entered commands with their time, newest first, to
//! filter, copy one from, copy as a .txt file, or delete. See `crate::history`.

use std::ops::Range;

use gpui::{UniformListScrollHandle, uniform_list};

use super::*;
use crate::history::{self, Row};

const ROW_HEIGHT: f32 = 28.;
/// Tallest the list grows before it scrolls.
const LIST_MAX_HEIGHT: f32 = 448.;

pub(super) struct HistoryPanel {
    host_id: String,
    filter: Entity<InputState>,
    /// Oldest first, as saved.
    entries: Vec<history::Entry>,
    /// What the list shows, for the filter (see `history::rows`).
    rows: Vec<Row>,
    /// The Host's address now, to tell commands entered on an earlier one.
    current: String,
    scroll: UniformListScrollHandle,
    /// "Delete history" was clicked once and asks for a second click.
    confirm_clear: Option<Instant>,
}

impl HistoryPanel {
    fn refresh(&mut self, cx: &App) {
        let filter = self.filter.read(cx).value().to_string();
        self.rows = history::rows(&self.entries, &self.current, &filter);
    }
}

impl Workspace {
    /// Show a Host's Command History. Over another History it switches Host; over a form or a
    /// question it waits, so nothing typed there is lost.
    pub(super) fn open_history(&mut self, host_id: &str, window: &mut Window, cx: &mut Context<Self>) {
        let replacing = matches!(self.overlay, Some(Overlay::History(_)));
        if (self.modal_open(cx) && !replacing) || self.prompt_open(cx) {
            return;
        }
        let Some(host) = self.library.host(host_id) else { return };
        let current = history::address_label(&host.address, host.port);
        let filter = cx.new(|cx| InputState::new(window, cx).placeholder("Filter commands"));
        let sub = cx.subscribe_in(&filter, window, |this, _, event: &InputEvent, _, cx| {
            if let InputEvent::Change = event
                && let Some(Overlay::History(panel)) = &mut this.overlay
            {
                panel.refresh(cx);
                panel.confirm_clear = None;
                cx.notify();
            }
        });
        self._subscriptions.push(sub);
        filter.update(cx, |f, cx| f.focus(window, cx));
        let mut panel = HistoryPanel {
            host_id: host_id.to_string(),
            filter,
            entries: history::load(&self.data_dir, host_id),
            rows: vec![],
            current,
            scroll: UniformListScrollHandle::new(),
            confirm_clear: None,
        };
        panel.refresh(cx);
        self.overlay = Some(Overlay::History(panel));
        cx.notify();
    }

    /// A line was entered in Session `id`: add it to its Host's history, and to the panel if
    /// that Host's is open.
    pub(super) fn record_command(&mut self, id: usize, line: &str, at: chrono::DateTime<chrono::Local>, cx: &mut Context<Self>) {
        if !self.settings.command_history || history::is_ignored(line) {
            return;
        }
        let Some(entry) = self.sessions.get(id) else { return };
        let (SessionKind::Ssh { host_id }, Some(on)) = (entry.kind(), entry.ui.history_on.clone()) else { return };
        let host_id = host_id.clone();
        let item = history::Entry::new(at, on, line);
        if let Err(err) = history::append(&self.data_dir, &host_id, &item) {
            log::warn!("saving to the command history: {err:#}");
            // Once per run: a full or read-only disk would otherwise say so for every command.
            static WARNED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
            if !WARNED.swap(true, std::sync::atomic::Ordering::Relaxed) {
                self.show_toast_for(format!("Could not save the command history: {err:#}"), Duration::from_secs(8), cx);
            }
            return;
        }
        if let Some(Overlay::History(panel)) = &mut self.overlay
            && panel.host_id == host_id
        {
            panel.entries.push(item);
            if panel.entries.len() > history::MAX_ENTRIES {
                panel.entries.remove(0);
            }
            panel.refresh(cx);
            cx.notify();
        }
    }

    pub(super) fn delete_all_history(&mut self, cx: &mut Context<Self>) {
        match history::forget_all(&self.data_dir) {
            Ok(()) => self.show_toast("Deleted the command history of every host.", cx),
            Err(err) => self.show_toast(format!("Could not delete the command history: {err:#}"), cx),
        }
        cx.notify();
    }

    /// Settings → Security: ask before deleting every Host's history.
    pub(super) fn confirm_delete_all_history(&mut self, cx: &mut Context<Self>) {
        if self.prompt_open(cx) {
            return;
        }
        let hosts = history::hosts_with_history(&self.data_dir);
        let body = match hosts {
            1 => "The commands kept for 1 host are deleted from this PC. This can't be undone.".to_string(),
            n => format!("The commands kept for {n} hosts are deleted from this PC. This can't be undone."),
        };
        self.overlay = Some(Overlay::Confirm { title: "Delete all command history?".into(), body, action: ConfirmAction::DeleteAllHistory });
        cx.notify();
    }

    fn history_panel(&self) -> Option<&HistoryPanel> {
        match &self.overlay {
            Some(Overlay::History(panel)) => Some(panel),
            _ => None,
        }
    }

    fn history_host_name(&self, panel: &HistoryPanel) -> String {
        self.library.host(&panel.host_id).map(|h| h.name.clone()).unwrap_or_else(|| "host".into())
    }

    /// The shown commands as text, oldest first, with their time.
    fn history_text(&self, cx: &App) -> Option<(String, String, usize)> {
        let panel = self.history_panel()?;
        let name = self.history_host_name(panel);
        let filter = panel.filter.read(cx).value().to_string();
        let shown = panel.rows.iter().filter(|r| matches!(r, Row::Entry(_))).count();
        Some((name.clone(), history::export_text(&name, &panel.current, &panel.entries, &filter), shown))
    }

    fn copy_history_text(&mut self, cx: &mut Context<Self>) {
        let Some((_, text, shown)) = self.history_text(cx) else { return };
        self.copy_as_text(text, shown, "command", cx);
    }

    fn copy_history_file(&mut self, cx: &mut Context<Self>) {
        let Some((host, text, _)) = self.history_text(cx) else { return };
        self.copy_as_file(&format!("{host}_history"), &text, cx);
    }

    /// First click asks, the second (not a double click) deletes the open Host's history. The
    /// question lapses after a few seconds, so a click much later asks again.
    fn clear_history(&mut self, cx: &mut Context<Self>) {
        const ARMED: Duration = Duration::from_secs(5);
        let data_dir = self.data_dir.clone();
        let Some(Overlay::History(panel)) = &mut self.overlay else { return };
        let failed = match panel.confirm_clear {
            None => {
                let armed = Instant::now();
                panel.confirm_clear = Some(armed);
                cx.spawn(async move |this, cx| {
                    cx.background_executor().timer(ARMED).await;
                    let _ = this.update(cx, |this, cx| {
                        if let Some(Overlay::History(panel)) = &mut this.overlay
                            && panel.confirm_clear == Some(armed)
                        {
                            panel.confirm_clear = None;
                            cx.notify();
                        }
                    });
                })
                .detach();
                None
            }
            Some(at) if at.elapsed() < Duration::from_millis(500) => return,
            Some(_) => {
                panel.confirm_clear = None;
                let result = history::forget(&data_dir, &panel.host_id);
                if result.is_ok() {
                    panel.entries.clear();
                    panel.refresh(cx);
                }
                result.err()
            }
        };
        if let Some(err) = failed {
            self.show_toast(format!("Could not delete the history: {err:#}"), cx);
        }
        cx.notify();
    }

    fn copy_history_command(&mut self, index: usize, cx: &mut Context<Self>) {
        let Some(entry) = self.history_panel().and_then(|p| p.entries.get(index)) else { return };
        let command = entry.parts().1.to_string();
        cx.write_to_clipboard(gpui::ClipboardItem::new_string(command.clone()));
        // A long or multi-line command is named by its start, so the note stays one short line.
        let mut shown: String = command.lines().next().unwrap_or_default().chars().take(60).collect();
        if shown.len() < command.trim_end().len() {
            shown.push('…');
        }
        self.show_toast(format!("Copied {shown}"), cx);
    }

    pub(super) fn render_history(&self, panel: &HistoryPanel, p: AppPalette, cx: &mut Context<Self>) -> AnyElement {
        let name = self.history_host_name(panel);
        let total = panel.entries.len();
        let filtered = !panel.filter.read(cx).value().trim().is_empty();
        let shown = panel.rows.iter().filter(|r| matches!(r, Row::Entry(_))).count();
        let summary = match (total, filtered) {
            (0, _) => "Nothing kept yet".to_string(),
            (1, false) => "1 command".to_string(),
            (n, false) => format!("{n} commands"),
            (1, true) => format!("{shown} of 1 command"),
            (n, true) => format!("{shown} of {n} commands"),
        };
        let height = (panel.rows.len() as f32 * ROW_HEIGHT).clamp(ROW_HEIGHT * 3., LIST_MAX_HEIGHT);
        let list = if panel.rows.is_empty() {
            div()
                .h(px(height))
                .px(px(24.))
                .flex()
                .items_center()
                .justify_center()
                .text_center()
                .text_size(px(13.))
                .line_height(px(20.))
                .text_color(p.text_faint)
                .child(if total == 0 {
                    format!("Commands you enter in {name}'s sessions show up here with their time. They stay on this PC, whoever else uses the server.")
                } else {
                    "No command matches.".to_string()
                })
                .into_any_element()
        } else {
            uniform_list("history-rows", panel.rows.len(), cx.processor(|this, range: Range<usize>, _, cx| this.render_history_rows(range, cx)))
                .track_scroll(&panel.scroll)
                .h(px(height))
                .px(px(6.))
                .into_any_element()
        };
        let confirming = panel.confirm_clear.is_some();
        let has_shown = shown > 0;
        div()
            .w(px(760.))
            .max_w_full()
            .flex()
            .flex_col()
            .child(
                div()
                    .p(px(14.))
                    .pt(px(12.))
                    .flex()
                    .flex_col()
                    .gap(px(10.))
                    .border_b_1()
                    .border_color(p.border)
                    // As tall as the close button beside it, so the field starts below it.
                    .child(div().h(px(30.)).flex().items_center().child(section_label("Command history", p)))
                    .child(
                        div()
                            .flex()
                            .items_baseline()
                            .gap(px(8.))
                            .child(div().text_size(px(15.)).font_weight(FontWeight::SEMIBOLD).text_color(p.text).child(name))
                            .child(div().text_size(px(12.5)).text_color(p.text_muted).child(panel.current.clone()))
                            .child(div().flex_1())
                            .child(div().text_size(px(12.)).text_color(p.text_faint).child(summary)),
                    )
                    .child(Input::new(&panel.filter)),
            )
            .child(div().py(px(6.)).child(list))
            .child(
                div()
                    .p(px(12.))
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .border_t_1()
                    .border_color(p.border)
                    .when(total > 0, |el| {
                        el.child(
                            button("history-clear", if confirming { "Click again to delete" } else { "Delete history" }, if confirming { ButtonKind::Danger } else { ButtonKind::Ghost }, p)
                                .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.clear_history(cx))),
                        )
                    })
                    .child(div().flex_1())
                    .when(has_shown, |el| {
                        el.child(
                            icon_text_button("history-copy-text", "copy", "Copy", ButtonKind::Secondary, p)
                                .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.copy_history_text(cx))),
                        )
                        .child(
                            icon_text_button("history-copy-file", "file-text", "Copy as .txt", ButtonKind::Primary, p)
                                .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.copy_history_file(cx))),
                        )
                    }),
            )
            .into_any_element()
    }

    fn render_history_rows(&mut self, range: Range<usize>, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let p = cx.global::<Appearance>().app;
        let Some(panel) = self.history_panel() else { return vec![] };
        range
            .filter_map(|i| {
                let row = panel.rows.get(i)?;
                Some(match row {
                    Row::On { address, current } => div()
                        .id(("history-on", i))
                        .h(px(ROW_HEIGHT))
                        .px(px(10.))
                        .flex()
                        .items_end()
                        .pb(px(4.))
                        .gap(px(6.))
                        .text_size(px(11.5))
                        .text_color(p.text_faint)
                        .child(icon_el("server", 12., p.text_faint))
                        .child(div().font_weight(FontWeight::MEDIUM).text_color(p.text_muted).child(format!("On {address}")))
                        .when(!current, |el| el.child("· an earlier address of this host"))
                        .into_any_element(),
                    Row::Entry(index) => {
                        let index = *index;
                        let entry = panel.entries.get(index)?;
                        let (prompt, command) = entry.parts();
                        div()
                            .id(("history-entry", i))
                            .h(px(ROW_HEIGHT))
                            .px(px(10.))
                            .flex()
                            .items_center()
                            .gap(px(12.))
                            .rounded(p.corner(6.))
                            .cursor_pointer()
                            .hover(|s| s.bg(p.hover))
                            .tooltip(|window, cx| Tooltip::new("Click to copy the command").build(window, cx))
                            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| this.copy_history_command(index, cx)))
                            .child(
                                div()
                                    .flex_shrink_0()
                                    .w(px(132.))
                                    .font_family(p.mono_family)
                                    .text_size(px(11.5))
                                    .text_color(p.text_faint)
                                    .child(entry.at.format("%Y-%m-%d %H:%M:%S").to_string()),
                            )
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .flex()
                                    .gap(px(6.))
                                    .font_family(p.mono_family)
                                    .text_size(px(12.5))
                                    .when(!prompt.is_empty(), |el| {
                                        el.child(div().flex_shrink_0().max_w(px(220.)).truncate().text_color(p.text_faint).child(prompt.to_string()))
                                    })
                                    .child(div().flex_1().min_w_0().truncate().text_color(p.text).child(command.to_string())),
                            )
                            .into_any_element()
                    }
                })
            })
            .collect()
    }
}
