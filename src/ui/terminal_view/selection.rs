//! Keyboard selection works on the grid, without sending editing keys to the shell.

use super::*;
use alacritty_terminal::event::EventListener;

#[derive(Default)]
pub(super) struct KeyboardSelection {
    /// Cell boundaries: `columns` means just after the last cell on a row.
    endpoints: Option<(GridPoint, GridPoint)>,
    /// If output or a mouse selection changed this, the endpoints no longer apply.
    previous: Option<Selection>,
}

impl KeyboardSelection {
    /// Returns whether the key was handled locally. Disabled callers leave shell input alone.
    pub fn key<T: EventListener>(&mut self, term: &mut Term<T>, key: &str, ctrl: bool, shift: bool) -> bool {
        if term.mode().contains(TermMode::ALT_SCREEN) {
            return false;
        }
        if key == "escape" && !ctrl && !shift && term.selection.is_some() {
            term.selection = None;
            *self = Self::default();
            return true;
        }
        if key == "a" && ctrl && !shift {
            // A second press expands from the command to all output.
            let all = between(term, GridPoint::new(term.grid().topmost_line(), Column(0)), output_end(term));
            if term.selection.as_ref() == Some(&all) {
                self.select_all(term);
                return true;
            }
            if let Some((start, end)) = command(term) {
                let selection = between(term, start, end);
                if term.selection.as_ref() != Some(&selection) {
                    self.set(term, start, end);
                    return true;
                }
            }
            self.select_all(term);
            return true;
        }
        if !shift || !matches!(key, "left" | "right" | "up" | "down" | "home" | "end" | "pageup" | "pagedown") {
            return false;
        }
        if self.previous != term.selection {
            self.endpoints = None;
        }
        let (anchor, end) = self.endpoints.unwrap_or_else(|| {
            match term.selection.as_ref().and_then(|s| s.to_range(term)) {
                Some(range) => (range.start, GridPoint::new(range.end.line, range.end.column + 1)),
                None => {
                    let mut cursor = term.grid().cursor.point;
                    if term.grid().cursor.input_needs_wrap {
                        cursor.column = Column(term.columns());
                    }
                    (cursor, cursor)
                }
            }
        });
        let end = moved(term, end, key, ctrl);
        self.set(term, anchor, end);
        // Keep the active end visible while selecting into the Scrollback.
        let offset = term.grid().display_offset() as i32;
        let row = end.line.0 + offset;
        let delta = if row < 0 { -row } else { (term.screen_lines() as i32 - 1 - row).min(0) };
        if delta != 0 {
            term.scroll_display(Scroll::Delta(delta));
        }
        true
    }

    pub fn select_all<T>(&mut self, term: &mut Term<T>) {
        let start = GridPoint::new(term.grid().topmost_line(), Column(0));
        self.set(term, start, output_end(term));
    }

    fn set<T>(&mut self, term: &mut Term<T>, start: GridPoint, end: GridPoint) {
        let selection = between(term, start, end);
        self.previous = Some(selection.clone());
        self.endpoints = Some((start, end));
        term.selection = Some(selection);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alacritty_terminal::vte::ansi::Processor;

    struct Shell {
        term: SharedTerm,
        parser: Processor,
        selection: KeyboardSelection,
    }

    impl Shell {
        fn new(cols: u16, rows: u16) -> Self {
            let (tx, _rx) = mpsc::unbounded();
            let term = new_term(GridSize { cols, rows, cell_width: 8, cell_height: 16 }, 100, EventProxy::new(tx));
            Self { term, parser: Processor::new(), selection: Default::default() }
        }
        fn feed(&mut self, bytes: &str) {
            self.parser.advance(&mut *self.term.lock(), bytes.as_bytes());
        }
        fn key(&mut self, key: &str, ctrl: bool, shift: bool) -> bool {
            self.selection.key(&mut self.term.lock(), key, ctrl, shift)
        }
        fn text(&self) -> String {
            self.term.lock().selection_to_string().unwrap_or_default()
        }
    }

    #[test]
    fn ctrl_a_selects_the_wrapped_unicode_command_then_all_output() {
        let mut s = Shell::new(18, 6);
        s.feed("earlier output\r\n界 ❯ echo café 世界\u{301} and more");
        assert!(s.key("a", true, false));
        assert_eq!(s.text(), "echo café 世界\u{301} and more");
        s.key("a", true, false);
        assert_eq!(s.text(), "earlier output\n界 ❯ echo café 世界\u{301} and more");
    }

    #[test]
    fn ctrl_a_reads_the_whole_command_when_cursor_is_in_the_middle() {
        let mut s = Shell::new(40, 4);
        s.feed("ops@host:~$ echo one two\x1b[5D");
        s.key("a", true, false);
        assert_eq!(s.text(), "echo one two");
    }

    #[test]
    fn unrecognised_and_empty_prompts_fall_back_to_all_output() {
        for line in ["custom prompt: text", "ops@host:~$ "] {
            let mut s = Shell::new(40, 4);
            s.feed(&format!("old output\r\n{line}"));
            s.key("a", true, false);
            assert!(s.text().starts_with("old output\n"));
        }
    }

    #[test]
    fn selecting_words_can_shrink_and_reverse_without_moving_the_shell_cursor() {
        let mut s = Shell::new(40, 4);
        s.feed("$ echo one two");
        let cursor = s.term.lock().grid().cursor.point;
        s.key("left", true, true);
        assert_eq!(s.text(), "two");
        s.key("left", true, true);
        assert_eq!(s.text(), "one two");
        s.key("right", true, true);
        assert_eq!(s.text(), " two");
        assert_eq!(s.term.lock().grid().cursor.point, cursor);
        s.key("end", false, true);
        assert_eq!(s.text(), "");
        s.key("left", false, true);
        assert_eq!(s.text(), "o");
    }

    #[test]
    fn wide_characters_are_selected_as_whole_cells() {
        let mut s = Shell::new(40, 4);
        s.feed("$ 界");
        s.key("left", false, true);
        assert_eq!(s.text(), "界");
        s.key("right", false, true);
        assert_eq!(s.text(), "");
    }

    #[test]
    fn buffer_navigation_reaches_scrollback_and_escape_clears_locally() {
        let mut s = Shell::new(20, 3);
        s.feed("first\r\nsecond\r\nthird\r\n$ echo hi");
        s.key("home", true, true);
        assert_eq!(s.text(), "first\nsecond\nthird\n$ echo hi");
        assert!(s.term.lock().grid().display_offset() > 0);
        assert!(s.key("escape", false, false));
        assert!(s.term.lock().selection.is_none());
        assert!(!s.key("escape", false, false));
    }

    #[test]
    fn new_output_invalidates_keyboard_endpoints_and_mouse_selections_can_expand() {
        let mut s = Shell::new(20, 3);
        s.feed("$ echo hi");
        s.key("left", true, true);
        s.term.lock().selection = None;
        s.feed("!x");
        s.key("left", false, true);
        assert_eq!(s.text(), "x");
        let mut mouse = Selection::new(SelectionType::Simple, GridPoint::new(Line(0), Column(2)), Side::Left);
        mouse.update(GridPoint::new(Line(0), Column(5)), Side::Right);
        s.term.lock().selection = Some(mouse);
        s.key("right", false, true);
        assert_eq!(s.text(), "echo ");
    }

    #[test]
    fn full_screen_programs_and_plain_keys_keep_their_input() {
        let mut s = Shell::new(20, 3);
        s.feed("$ echo hi");
        assert!(!s.key("a", false, false));
        assert!(!s.key("left", true, false));
        s.feed("\x1b[?1049h");
        for (key, ctrl, shift) in [("a", true, false), ("left", true, true), ("pageup", false, true)] {
            assert!(!s.key(key, ctrl, shift));
            assert!(s.term.lock().selection.is_none());
        }
    }

    #[test]
    fn word_selection_crosses_wrapped_rows_and_handles_a_pending_wrap() {
        let mut s = Shell::new(8, 4);
        s.feed("$ abcdefghijkl");
        s.key("left", true, true);
        assert_eq!(s.text(), "abcdefghijkl");
        s.key("right", true, true);
        assert_eq!(s.text(), "");

        let mut s = Shell::new(8, 4);
        s.feed("$ abcdef");
        s.key("left", false, true);
        assert_eq!(s.text(), "f");
    }
}

fn between<T>(term: &Term<T>, start: GridPoint, end: GridPoint) -> Selection {
    let edge = |p: GridPoint| {
        if p.column.0 >= term.columns() {
            (GridPoint::new(p.line, term.last_column()), Side::Right)
        } else {
            (p, Side::Left)
        }
    };
    let (start, side) = edge(start);
    let mut selection = Selection::new(SelectionType::Simple, start, side);
    let (end, side) = edge(end);
    selection.update(end, side);
    selection
}

fn output_end<T>(term: &Term<T>) -> GridPoint {
    let grid = term.grid();
    let last = (0..grid.screen_lines() as i32)
        .rev()
        .find(|&row| (0..grid.columns()).any(|col| grid[Line(row)][Column(col)].c != ' '))
        .unwrap_or(0)
        .max(grid.cursor.point.line.0);
    GridPoint::new(Line(last), Column(grid.columns()))
}

/// Recognise the same prompt syntax as Command Marks. Map bytes back to grid cells so
/// Unicode prompts, wide characters and wrapped commands select the right cells.
fn command<T>(term: &Term<T>) -> Option<(GridPoint, GridPoint)> {
    let grid = term.grid();
    let mut first = grid.cursor.point.line;
    while first > grid.topmost_line() && grid[first - 1i32][term.last_column()].flags.contains(Flags::WRAPLINE) {
        first -= 1;
    }
    let mut last = first;
    while last < grid.bottommost_line() && grid[last][term.last_column()].flags.contains(Flags::WRAPLINE) {
        last += 1;
    }
    let mut text = String::new();
    let mut cells = Vec::new();
    for row in first.0..=last.0 {
        for col in 0..term.columns() {
            let point = GridPoint::new(Line(row), Column(col));
            let cell = &grid[point];
            if cell.flags.intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER) {
                continue;
            }
            cells.push((text.len(), point));
            text.push(cell.c);
            if let Some(chars) = cell.zerowidth() {
                text.extend(chars);
            }
        }
    }
    let prompt = crate::marks::prompt_end(&text)?;
    let start_byte = prompt + text[prompt..].len() - text[prompt..].trim_start().len();
    let end_byte = text.trim_end().len();
    if start_byte >= end_byte {
        return None;
    }
    let start = cells.iter().find(|(byte, _)| *byte >= start_byte)?.1;
    let end = cells.iter().rev().find(|(byte, _)| *byte < end_byte)?.1;
    let width = if grid[end].flags.contains(Flags::WIDE_CHAR) { 2 } else { 1 };
    Some((start, GridPoint::new(end.line, end.column + width)))
}

fn step<T>(term: &Term<T>, mut p: GridPoint, right: bool) -> GridPoint {
    let grid = term.grid();
    if right {
        if p.column.0 < term.columns() {
            p.column += 1;
            if p.column.0 < term.columns() && grid[p].flags.contains(Flags::WIDE_CHAR_SPACER) {
                p.column += 1;
            }
            if p.column.0 == term.columns() && p.line < grid.bottommost_line()
                && grid[p.line][term.last_column()].flags.contains(Flags::WRAPLINE)
            {
                p = GridPoint::new(p.line + 1, Column(0));
            }
        } else if p.line < grid.bottommost_line() {
            p = GridPoint::new(p.line + 1, Column(0));
        }
    } else if p.column.0 > 0 {
        p.column -= 1;
        if grid[p].flags.contains(Flags::WIDE_CHAR_SPACER) {
            p.column = Column(p.column.0.saturating_sub(1));
        }
    } else if p.line > grid.topmost_line() {
        let wrapped = grid[p.line - 1i32][term.last_column()].flags.contains(Flags::WRAPLINE);
        p = GridPoint::new(p.line - 1, Column(term.columns()));
        if wrapped {
            return step(term, p, false);
        }
    }
    p
}

fn moved<T>(term: &Term<T>, mut p: GridPoint, key: &str, ctrl: bool) -> GridPoint {
    let grid = term.grid();
    match key {
        "home" if ctrl => return GridPoint::new(grid.topmost_line(), Column(0)),
        "end" if ctrl => return output_end(term),
        "home" => p.column = Column(0),
        "end" => {
            use alacritty_terminal::term::cell::LineLength;
            p.column = grid[p.line].line_length();
        }
        "up" | "down" | "pageup" | "pagedown" => {
            let distance = if key.starts_with("page") { term.screen_lines() as i32 } else { 1 };
            let delta = if matches!(key, "up" | "pageup") { -distance } else { distance };
            p.line = Line((p.line.0 + delta).clamp(grid.topmost_line().0, grid.bottommost_line().0));
            if p.column.0 < term.columns() && grid[p].flags.contains(Flags::WIDE_CHAR_SPACER) {
                p.column = Column(p.column.0.saturating_sub(1));
            }
        }
        "left" | "right" => {
            let right = key == "right";
            if !ctrl {
                return step(term, p, right);
            }
            let kind = |at: GridPoint| {
                if at.column.0 == term.columns() { return 0; }
                let c = grid[at].c;
                if c.is_whitespace() { 0 } else if term.semantic_escape_chars().contains(c) { 1 } else { 2 }
            };
            // Skip whitespace, then select one word or delimiter group. At a hard line
            // boundary the first step crosses the newline; wrapped text remains continuous.
            let mut group = None;
            loop {
                let next = step(term, p, right);
                if next == p { break; }
                let at = if right { p } else { next };
                let k = kind(at);
                if group.is_some_and(|g| k != g) { break; }
                if k != 0 { group = Some(k); }
                let crosses = next.line != p.line;
                let row = if right { p.line } else { next.line };
                if crosses && !grid[row][term.last_column()].flags.contains(Flags::WRAPLINE) {
                    if group.is_none() { p = next; }
                    break;
                }
                p = next;
            }
        }
        _ => {}
    }
    p
}
