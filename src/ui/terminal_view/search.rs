//! The search bar over a terminal's output (Ctrl+Shift+F): Scrollback search, highlighting
//! every match on screen and stepping through them. The searching itself is in `crate::search`.
//!
//! The bar floats over the top-right corner of the output rather than taking a row of its own,
//! so opening it never resizes the terminal (which the remote shell would have to redraw for).

use std::ops::Range;
use std::sync::Arc;
use std::time::{Duration, Instant};

use alacritty_terminal::grid::{Dimensions as _, Scroll};
use alacritty_terminal::index::{Line, Side};
use alacritty_terminal::selection::{Selection, SelectionType};
use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, Context, Entity, Focusable as _, FontWeight, KeyDownEvent, MouseButton, MouseDownEvent, Subscription,
    Window, div, prelude::*, px,
};
use gpui_component::Sizable as _;
use gpui_component::input::{Escape, Input, InputEvent, InputState};
use gpui_component::tooltip::Tooltip;

use super::{SEARCH_CONTEXT, SearchNewer, SearchOlder, TerminalView};
use crate::search::{self, Matches, Query, Refreshed, Snapshot};
use crate::theme::{AppPalette, Appearance, UI_FONT};
use crate::ui::minimap::SearchMarks;
use crate::ui::widgets::icon_el;

/// Shortest wait between copying new output for an open search. A refresh that took longer
/// waits four times as long, so a huge Scrollback receiving output isn't copied over and over.
const REFRESH_MIN: Duration = Duration::from_millis(250);

/// The open search bar and what it found.
pub(super) struct SearchBar {
    input: Entity<InputState>,
    case_sensitive: bool,
    regex: bool,
    /// Why the pattern can't be used (a regular expression that doesn't parse).
    error: Option<String>,
    /// The text searched (see `crate::search::Snapshot`).
    snapshot: Option<Arc<Snapshot>>,
    matches: Arc<Matches>,
    /// Index into `matches.ranges` of the match stepped to.
    current: Option<usize>,
    /// Output that arrived (or grid changes), counted; and the count the copy includes. They
    /// differ while there is output the copy doesn't have.
    seen: u64,
    copied: u64,
    /// The field is empty: nothing to search, nor to refresh for.
    empty: bool,
    /// A search is running off the UI thread; results from older ones are dropped.
    running: bool,
    generation: u64,
    /// A refresh is waiting for `refresh_wait` to pass.
    refresh_pending: bool,
    refresh_wait: Duration,
    _subscription: Subscription,
}

impl SearchBar {
    fn stale(&self) -> bool {
        self.seen != self.copied
    }

    fn query(&self, cx: &gpui::App) -> Query {
        Query {
            text: self.input.read(cx).value().to_string(),
            case_sensitive: self.case_sensitive,
            regex: self.regex,
        }
    }

    /// The current match's start as (row, byte in the row) of the copy.
    fn current_position(&self) -> Option<(usize, usize)> {
        let snapshot = self.snapshot.as_ref()?;
        let range = self.matches.ranges.get(self.current?)?;
        Some(snapshot.position(range.start))
    }
}

/// Why a search runs.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Reason {
    /// The query changed: show the match nearest to where you were, scrolling to it.
    Query,
    /// New output: keep the match you are on, and leave the view where it is.
    Refresh,
}

/// One highlighted match on screen: view row, columns, and whether it's the current one.
pub(super) type Highlight = (i32, Range<usize>, bool);

impl TerminalView {
    /// Open the search bar, or give it the keyboard again with its text selected.
    pub(super) fn open_search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.prompt.is_some() || self.route.is_some() {
            return;
        }
        let input = match &self.search {
            Some(bar) => bar.input.clone(),
            None => {
                // The last search comes back, as in other terminals and editors.
                let last = self.last_search.clone();
                let input =
                    cx.new(|cx| InputState::new(window, cx).placeholder("Find in output").default_value(last.text.clone()));
                let subscription = cx.subscribe_in(&input, window, |this, _, event: &InputEvent, _, cx| match event {
                    InputEvent::Change => this.start_search(Reason::Query, cx),
                    // Enter goes up the Scrollback, to older output; Shift+Enter comes back down.
                    InputEvent::PressEnter { shift, .. } => this.step_search(!*shift, cx),
                    _ => {}
                });
                self.search = Some(SearchBar {
                    input: input.clone(),
                    case_sensitive: last.case_sensitive,
                    regex: last.regex,
                    error: None,
                    snapshot: None,
                    matches: Arc::default(),
                    current: None,
                    seen: 1,
                    copied: 0,
                    empty: last.text.is_empty(),
                    running: false,
                    generation: 0,
                    refresh_pending: false,
                    refresh_wait: REFRESH_MIN,
                    _subscription: subscription,
                });
                if !last.text.is_empty() {
                    self.start_search(Reason::Query, cx);
                }
                input
            }
        };
        input.update(cx, |state, cx| {
            state.focus(window, cx);
            state.select_all(window, cx);
        });
        cx.notify();
    }

    /// Close the bar and give the keyboard back to the terminal. The match you were on stays
    /// selected, ready to copy.
    pub(super) fn close_search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(bar) = self.search.take() else { return };
        self.last_search = bar.query(cx);
        if let (Some(snapshot), Some(range)) = (&bar.snapshot, bar.current.and_then(|i| bar.matches.ranges.get(i))) {
            let mut term = self.term.lock();
            if let Some(moved) = snapshot.shift(&term, self.scrollback)
                && let Some((start, end)) = search::grid_span(&term, snapshot, range.clone(), moved)
            {
                let mut selection = Selection::new(SelectionType::Simple, start, Side::Left);
                selection.update(end, Side::Right);
                term.selection = Some(selection);
            }
        }
        window.focus(&self.focus, cx);
        cx.notify();
    }

    /// Whether the search bar's field has the keyboard.
    pub(super) fn search_focused(&self, window: &Window, cx: &gpui::App) -> bool {
        self.search.as_ref().is_some_and(|bar| bar.input.read(cx).focus_handle(cx).is_focused(window))
    }

    /// Paste into the search field (Ctrl+Shift+V there), its first line only.
    pub(super) fn paste_into_search(&mut self, text: &str, window: &mut Window, cx: &mut Context<Self>) {
        let Some(bar) = &self.search else { return };
        let line = text.lines().next().unwrap_or_default().to_string();
        bar.input.update(cx, |state, cx| state.insert(line, window, cx));
    }

    fn toggle_search_option(&mut self, regex: bool, window: &mut Window, cx: &mut Context<Self>) {
        let Some(bar) = self.search.as_mut() else { return };
        if regex {
            bar.regex = !bar.regex;
        } else {
            bar.case_sensitive = !bar.case_sensitive;
        }
        let input = bar.input.clone();
        input.update(cx, |state, cx| state.focus(window, cx));
        self.start_search(Reason::Query, cx);
    }

    /// Output arrived or the grid changed: search the new text once `refresh_wait` has passed.
    pub(super) fn search_output(&mut self, cx: &mut Context<Self>) {
        let Some(bar) = self.search.as_mut() else { return };
        bar.seen += 1;
        if bar.refresh_pending || bar.running || bar.empty {
            return;
        }
        bar.refresh_pending = true;
        let wait = bar.refresh_wait;
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(wait).await;
            let _ = this.update(cx, |this, cx| {
                let Some(bar) = this.search.as_mut() else { return };
                bar.refresh_pending = false;
                if bar.stale() && !bar.running {
                    this.start_search(Reason::Refresh, cx);
                }
            });
        })
        .detach();
    }

    /// Search off the UI thread: bring the copy up to date if output arrived, then run the
    /// pattern over it. A newer search replaces one still running.
    fn start_search(&mut self, reason: Reason, cx: &mut Context<Self>) {
        let Some(bar) = self.search.as_mut() else { return };
        let query = bar.query(cx);
        bar.empty = query.text.is_empty();
        let regex = match query.compile() {
            Ok(Some(regex)) => regex,
            result => {
                bar.error = result.err();
                bar.matches = Arc::default();
                bar.current = None;
                bar.generation += 1;
                bar.running = false;
                cx.notify();
                return;
            }
        };
        bar.error = None;
        bar.generation += 1;
        bar.running = true;
        let generation = bar.generation;
        let anchor = bar.current_position();
        let old = bar.snapshot.clone();
        // A search that replaces one still copying copies again: that copy is thrown away.
        let copy = bar.stale() || old.is_none();
        let seen = bar.seen;
        let term = self.term.clone();
        let max_history = self.scrollback;
        let started = Instant::now();
        let work = cx.background_spawn(async move {
            let (snapshot, refreshed) = match old {
                Some(old) if !copy => (old, Refreshed::Moved(0)),
                old => {
                    let old = old.unwrap_or_default();
                    // Only the copying holds the terminal; building and searching don't.
                    let fresh = crate::perf::time("search copy", || old.refresh(&*term.lock(), max_history));
                    let (snapshot, refreshed) = old.assemble(fresh);
                    (Arc::new(snapshot), refreshed)
                }
            };
            let matches = crate::perf::time("search match", || Matches::find(&snapshot, &regex));
            (snapshot, refreshed, matches)
        });
        cx.spawn(async move |this, cx| {
            let (snapshot, refreshed, matches) = work.await;
            let _ = this.update(cx, |this, cx| {
                let copied = copy.then_some(seen);
                this.search_done(generation, reason, anchor, copied, snapshot, refreshed, matches, started.elapsed(), cx);
            });
        })
        .detach();
    }

    #[allow(clippy::too_many_arguments)]
    fn search_done(
        &mut self,
        generation: u64,
        reason: Reason,
        anchor: Option<(usize, usize)>,
        copied: Option<u64>,
        snapshot: Arc<Snapshot>,
        refreshed: Refreshed,
        matches: Matches,
        took: Duration,
        cx: &mut Context<Self>,
    ) {
        if !self.search.as_ref().is_some_and(|bar| bar.generation == generation) {
            return;
        }
        // Where the match you were on is in the new copy, if its rows are still there.
        let anchor = match (anchor, refreshed) {
            (Some((row, byte)), Refreshed::Moved(moved)) if row >= moved => Some(snapshot.offset(row - moved, byte)),
            _ => None,
        };
        let anchor = anchor.or_else(|| self.view_bottom(&snapshot));
        let current = anchor.and_then(|at| matches.at_or_before(at));
        let Some(bar) = self.search.as_mut() else { return };
        bar.running = false;
        bar.refresh_wait = (took * 4).max(REFRESH_MIN);
        bar.snapshot = Some(snapshot);
        bar.matches = Arc::new(matches);
        bar.current = current;
        if let Some(copied) = copied {
            bar.copied = copied;
        }
        let stale = bar.stale();
        if reason == Reason::Query {
            self.reveal_search_match();
        }
        if stale {
            self.search_output(cx);
        }
        cx.notify();
    }

    /// The end of the bottom row on screen, as a byte of the copy: searching starts there.
    fn view_bottom(&self, snapshot: &Snapshot) -> Option<usize> {
        if snapshot.is_empty() {
            return None;
        }
        let term = self.term.lock();
        let moved = snapshot.shift(&term, self.scrollback)?;
        let grid = term.grid();
        let bottom = grid.screen_lines() as i64 - 1 - grid.display_offset() as i64;
        let row = (bottom + grid.history_size() as i64 + moved as i64).max(0) as usize;
        Some(snapshot.offset(row, usize::MAX))
    }

    /// Step to the next match up (`older`) or down the Scrollback, wrapping at the ends.
    fn step_search(&mut self, older: bool, cx: &mut Context<Self>) {
        let Some(bar) = self.search.as_ref() else { return };
        let count = bar.matches.ranges.len();
        if count == 0 {
            return;
        }
        let current = match bar.current {
            Some(i) if older => (i + count - 1) % count,
            Some(i) => (i + 1) % count,
            None => {
                let snapshot = bar.snapshot.clone();
                let matches = bar.matches.clone();
                let at = snapshot.and_then(|s| self.view_bottom(&s));
                at.and_then(|at| matches.at_or_before(at)).unwrap_or(count - 1)
            }
        };
        if let Some(bar) = self.search.as_mut() {
            bar.current = Some(current);
        }
        self.reveal_search_match();
        cx.notify();
    }

    /// Scroll the current match into view, about halfway down, unless it's already on screen.
    fn reveal_search_match(&mut self) {
        let Some(bar) = &self.search else { return };
        let (Some(snapshot), Some(range)) = (&bar.snapshot, bar.current.and_then(|i| bar.matches.ranges.get(i))) else {
            return;
        };
        let mut term = self.term.lock();
        let Some(moved) = snapshot.shift(&term, self.scrollback) else { return };
        let Some((start, end)) = search::grid_span(&term, snapshot, range.clone(), moved) else { return };
        let grid = term.grid();
        let rows = grid.screen_lines() as i32;
        let offset = grid.display_offset() as i32;
        if start.line.0 + offset >= 0 && end.line.0 + offset < rows {
            return;
        }
        let target = (rows / 2 - start.line.0).clamp(0, grid.history_size() as i32);
        term.scroll_display(Scroll::Delta(target - offset));
    }

    /// The matches on screen, for the grid to paint under their text, and all of them for the
    /// minimap to mark. A row whose text is no longer what was searched (output moved it, or a
    /// program redrew it) shows none until the next refresh catches up.
    pub(super) fn search_view(&self) -> (Vec<Highlight>, Option<SearchMarks>) {
        let Some(bar) = &self.search else { return (Vec::new(), None) };
        let Some(snapshot) = &bar.snapshot else { return (Vec::new(), None) };
        if bar.matches.ranges.is_empty() || snapshot.is_empty() {
            return (Vec::new(), None);
        }
        let term = self.term.lock();
        let Some(moved) = snapshot.shift(&term, self.scrollback) else { return (Vec::new(), None) };
        let marks = SearchMarks { snapshot: snapshot.clone(), matches: bar.matches.clone(), moved, current: bar.current };
        (Self::on_screen(&term, bar, snapshot, moved), Some(marks))
    }

    fn on_screen<T>(
        term: &alacritty_terminal::Term<T>,
        bar: &SearchBar,
        snapshot: &Snapshot,
        moved: usize,
    ) -> Vec<Highlight> {
        let grid = term.grid();
        let rows = grid.screen_lines();
        let offset = grid.display_offset() as i64;
        // The copy's row shown at the top of the view.
        let first = grid.history_size() as i64 + moved as i64 - offset;
        if first < 0 || first as usize >= snapshot.row_count() {
            return Vec::new();
        }
        let first = first as usize;
        let last = (first + rows - 1).min(snapshot.row_count() - 1);
        let (from, to) = (snapshot.offset(first, 0), snapshot.offset(last, usize::MAX));
        let line_of = |row: usize| Line(row as i32 - moved as i32 - grid.history_size() as i32);
        let mut same: Vec<Option<bool>> = vec![None; last - first + 1];
        let mut out = Vec::new();
        let start = bar.matches.ranges.partition_point(|m| m.end <= from);
        for (i, range) in bar.matches.ranges.iter().enumerate().skip(start) {
            if range.start > to {
                break;
            }
            let current = bar.current == Some(i);
            for (row, bytes) in snapshot.spans(range.clone()) {
                if row < first || row > last {
                    continue;
                }
                let line = line_of(row);
                let unchanged = *same[row - first]
                    .get_or_insert_with(|| search::row_equals(&grid[line], snapshot.row_text(row)));
                if !unchanged {
                    continue;
                }
                if let Some(cols) = search::columns(&grid[line], bytes) {
                    out.push(((line.0 as i64 + offset) as i32, cols, current));
                }
            }
        }
        out
    }

    /// The bar, over the top-right corner of the output.
    pub(super) fn render_search_bar(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let bar = self.search.as_ref()?;
        let p = cx.global::<Appearance>().app;
        let has_query = !bar.input.read(cx).value().is_empty();
        let count = bar.matches.ranges.len();
        let (status, status_color) = match (&bar.error, bar.current) {
            (Some(error), _) => (error.clone(), p.danger),
            _ if !has_query => (String::new(), p.text_faint),
            _ if bar.snapshot.is_none() || (bar.running && count == 0) => (String::new(), p.text_faint),
            _ if bar.matches.total == 0 => ("No results".into(), p.text_muted),
            (None, Some(i)) => (format!("{} of {}", thousands(bar.matches.ordinal(i)), thousands(bar.matches.total)), p.text_muted),
            (None, None) => (format!("{} found", thousands(bar.matches.total)), p.text_muted),
        };
        let can_step = count > 0;
        let status = div()
            .max_w(px(150.))
            .overflow_hidden()
            .text_ellipsis()
            .whitespace_nowrap()
            .text_size(px(12.))
            .text_color(status_color)
            .child(status);
        let stop = |_: &MouseDownEvent, window: &mut Window, cx: &mut gpui::App| {
            // Clicks on the bar are not clicks on the terminal: no selection, and the keyboard
            // stays where it was.
            window.prevent_default();
            cx.stop_propagation();
        };
        Some(
            div()
                .id("search-bar")
                .key_context(SEARCH_CONTEXT)
                .absolute()
                .top(px(6.))
                .right(px(6.))
                .w(px(400.))
                .max_w(gpui::relative(0.92))
                .flex()
                .items_center()
                .gap(px(2.))
                .p(px(4.))
                .rounded(px(10.))
                .bg(p.surface)
                .border_1()
                .border_color(p.border)
                .shadow(crate::ui::widgets::elevation(p.dark))
                .font_family(UI_FONT)
                .cursor_default()
                .on_mouse_down(MouseButton::Left, stop)
                .on_mouse_down(MouseButton::Middle, stop)
                .on_mouse_down(MouseButton::Right, stop)
                .on_action(cx.listener(|this, _: &Escape, window, cx| this.close_search(window, cx)))
                .on_action(cx.listener(|this, _: &SearchOlder, _, cx| this.step_search(true, cx)))
                .on_action(cx.listener(|this, _: &SearchNewer, _, cx| this.step_search(false, cx)))
                .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                    let m = &event.keystroke.modifiers;
                    if m.alt && !m.control && !m.shift {
                        match event.keystroke.key.as_str() {
                            "c" => this.toggle_search_option(false, window, cx),
                            "r" => this.toggle_search_option(true, window, cx),
                            _ => return,
                        }
                        cx.stop_propagation();
                    }
                }))
                .child(
                    div().flex_1().min_w_0().child(
                        Input::new(&bar.input)
                            .small()
                            .appearance(false)
                            .prefix(icon_el("search", 13., p.text_faint))
                            .suffix(status),
                    ),
                )
                .child(
                    toggle("search-case", "case-sensitive", "Match case (Alt+C)", bar.case_sensitive, p)
                        .on_click(cx.listener(|this, _, window, cx| this.toggle_search_option(false, window, cx))),
                )
                .child(
                    toggle("search-regex", "regex", "Use regular expression (Alt+R)", bar.regex, p)
                        .on_click(cx.listener(|this, _, window, cx| this.toggle_search_option(true, window, cx))),
                )
                .child(div().w(px(1.)).h(px(18.)).mx(px(3.)).bg(p.border))
                .child(
                    tool("search-older", "arrow-up", "Older match (Enter or Up)", can_step, p)
                        .on_click(cx.listener(|this, _, _, cx| this.step_search(true, cx))),
                )
                .child(
                    tool("search-newer", "arrow-down", "Newer match (Shift+Enter or Down)", can_step, p)
                        .on_click(cx.listener(|this, _, _, cx| this.step_search(false, cx))),
                )
                .child(
                    tool("search-close", "x", "Close (Escape)", true, p)
                        .on_click(cx.listener(|this, _, window, cx| this.close_search(window, cx))),
                )
                .into_any_element(),
        )
    }
}

/// A small icon button with a tooltip.
fn tool(id: &'static str, icon: &str, tip: &'static str, enabled: bool, p: AppPalette) -> gpui::Stateful<gpui::Div> {
    div()
        .id(id)
        .size(px(26.))
        .flex()
        .flex_shrink_0()
        .items_center()
        .justify_center()
        .rounded(px(6.))
        .tooltip(move |window, cx| Tooltip::new(tip).build(window, cx))
        .when(enabled, |el| el.cursor_pointer().hover(move |s| s.bg(p.hover)))
        .child(icon_el(icon, 15., if enabled { p.text_muted } else { p.text_faint.opacity(0.5) }))
}

/// An option that is on or off, lit in the brand colour while on.
fn toggle(id: &'static str, icon: &str, tip: &'static str, on: bool, p: AppPalette) -> gpui::Stateful<gpui::Div> {
    div()
        .id(id)
        .size(px(26.))
        .flex()
        .flex_shrink_0()
        .items_center()
        .justify_center()
        .rounded(px(6.))
        .cursor_pointer()
        .tooltip(move |window, cx| Tooltip::new(tip).build(window, cx))
        .when(on, |el| el.bg(p.brand_soft).border_1().border_color(p.brand))
        .when(!on, |el| el.hover(move |s| s.bg(p.hover)))
        .font_weight(FontWeight::MEDIUM)
        .child(icon_el(icon, 15., if on { p.brand } else { p.text_muted }))
}

/// "12,345"
fn thousands(n: usize) -> String {
    let digits = n.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(c);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::thousands;

    #[test]
    fn counts_have_thousands_separators() {
        assert_eq!(thousands(7), "7");
        assert_eq!(thousands(999), "999");
        assert_eq!(thousands(1000), "1,000");
        assert_eq!(thousands(100_000), "100,000");
        assert_eq!(thousands(1_234_567), "1,234,567");
    }
}
