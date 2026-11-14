//! Scrollback search: finding text in a Session's output, its Scrollback included.
//!
//! The grid's text is copied into a [`Snapshot`] and the pattern runs over the copy, off the UI
//! thread: the terminal is locked only while rows are copied, never while a pattern runs, so
//! searching 500,000 lines holds up neither output nor typing. The copy is kept while the search
//! is open, and as output arrives only rows that can have changed are copied again ([`refresh`]):
//! a row in the Scrollback never changes; it only moves up and, once the Scrollback is full,
//! drops off the top.
//!
//! Each row of the grid is one line of the copy, except that a row which wraps runs on into the
//! next with nothing in between, so a match can span a wrapped line. Before a match is shown, the
//! row it is on is checked against the copy, so a highlight is only ever drawn over the text it
//! matched, even when output has moved the rows since the copy was made.
//!
//! [`refresh`]: Snapshot::refresh

use std::ops::Range;

use alacritty_terminal::grid::{Dimensions, Row};
use alacritty_terminal::index::{Column, Line, Point};
use alacritty_terminal::term::cell::{Cell, Flags, LineLength};
use alacritty_terminal::term::{Term, TermMode};
use regex::{Regex, RegexBuilder};

/// Most matches kept, the newest ones; all are counted.
pub const MAX_MATCHES: usize = 100_000;
/// Rows of the copy compared with the grid to find out how far the rows have moved.
const SAMPLES: usize = 8;

/// What to look for.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Query {
    pub text: String,
    pub case_sensitive: bool,
    pub regex: bool,
}

impl Query {
    /// The pattern to run: `None` for an empty query, an error for a regular expression that
    /// doesn't parse (its message's first line).
    pub fn compile(&self) -> Result<Option<Regex>, String> {
        if self.text.is_empty() {
            return Ok(None);
        }
        let pattern = if self.regex { self.text.clone() } else { regex::escape(&self.text) };
        RegexBuilder::new(&pattern)
            .case_insensitive(!self.case_sensitive)
            // Rows are joined with line breaks: `^` and `$` mean a line's start and end.
            .multi_line(true)
            .build()
            .map(Some)
            .map_err(|err| match err {
                regex::Error::Syntax(text) => {
                    let reason = text.lines().last().unwrap_or_default().trim();
                    reason.strip_prefix("error: ").unwrap_or(reason).to_string()
                }
                regex::Error::CompiledTooBig(_) => "Pattern is too large".to_string(),
                _ => "Invalid pattern".to_string(),
            })
    }
}

/// Matches as byte ranges of a [`Snapshot`]'s text, oldest first.
#[derive(Debug, Clone, Default)]
pub struct Matches {
    /// The newest [`MAX_MATCHES`] matches.
    pub ranges: Vec<Range<usize>>,
    /// The row of the copy each match starts on, for the minimap.
    pub starts: Vec<usize>,
    /// All matches, including the older ones not kept.
    pub total: usize,
}

impl Matches {
    /// Run `regex` over the copy. Empty matches (`a*` between letters) are not shown.
    pub fn find(snapshot: &Snapshot, regex: &Regex) -> Self {
        let (mut ranges, mut starts) = (Vec::new(), Vec::new());
        let (mut total, mut row) = (0, 0);
        for m in regex.find_iter(&snapshot.text) {
            if m.is_empty() {
                continue;
            }
            total += 1;
            // Matches come in order, so the row only moves forward.
            while snapshot.rows.get(row + 1).is_some_and(|r| r.start <= m.start()) {
                row += 1;
            }
            ranges.push(m.range());
            starts.push(row);
            // Drop the oldest in batches rather than one per match.
            if ranges.len() == 2 * MAX_MATCHES {
                ranges.drain(..MAX_MATCHES);
                starts.drain(..MAX_MATCHES);
            }
        }
        if ranges.len() > MAX_MATCHES {
            let excess = ranges.len() - MAX_MATCHES;
            ranges.drain(..excess);
            starts.drain(..excess);
        }
        Self { ranges, starts, total }
    }

    /// The position shown to the user of the match at `index` (1 is the oldest counted).
    pub fn ordinal(&self, index: usize) -> usize {
        self.total - self.ranges.len() + index + 1
    }

    /// The match to show for `anchor` (a byte of the copy): the last match that starts at or
    /// before it, so searching goes up from where you are, as the Scrollback is read from the
    /// bottom. Without one above, the newest.
    pub fn at_or_before(&self, anchor: usize) -> Option<usize> {
        let after = self.ranges.partition_point(|m| m.start <= anchor);
        after.checked_sub(1).or_else(|| self.ranges.len().checked_sub(1))
    }
}

/// The grid's text, copied (see the module documentation).
#[derive(Debug, Clone, Default)]
pub struct Snapshot {
    text: String,
    /// Each row's text in `text`, from the top of the Scrollback down. After a row's text comes
    /// a line break, unless the row wraps.
    rows: Vec<Range<usize>>,
    /// How many of the rows were in the Scrollback when copied.
    history: usize,
    /// The width when copied: a new width rewraps every row.
    columns: usize,
    /// Copied from the alternate screen (vim, less), a grid of its own with no Scrollback.
    alt_screen: bool,
}

/// What [`Snapshot::refresh`] did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refreshed {
    /// The rows moved up by this many: row `i` of the old copy is row `i - n` of the new one,
    /// and the old rows above `n` dropped off the top.
    Moved(usize),
    /// Copied afresh: the old copy's rows have no counterpart in the new one.
    Anew,
}

impl Snapshot {
    /// Copy the whole grid, Scrollback included.
    pub fn copy<T>(term: &Term<T>) -> Self {
        let grid = term.grid();
        let mut snapshot = Self {
            history: grid.history_size(),
            columns: grid.columns(),
            alt_screen: term.mode().contains(TermMode::ALT_SCREEN),
            ..Self::default()
        };
        snapshot.push_rows(term, grid.topmost_line(), grid.bottommost_line());
        snapshot
    }

    pub fn text(&self) -> &str {
        &self.text
    }

    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }

    fn push_rows<T>(&mut self, term: &Term<T>, from: Line, to: Line) {
        let grid = term.grid();
        let mut line = from;
        while line <= to {
            let start = self.text.len();
            let wraps = push_row(&grid[line], &mut self.text);
            self.rows.push(start..self.text.len());
            if !wraps {
                self.text.push('\n');
            }
            line += 1;
        }
    }

    /// Bring the copy up to date with `term`, copying only the rows that can have changed:
    /// what was the screen when the copy was made, and the output since. `max_history` is the
    /// Session's Scrollback limit: until the Scrollback is full, no row drops off the top.
    ///
    /// The terminal is locked meanwhile, so this only copies rows; [`Snapshot::assemble`]
    /// builds the new copy from them without the lock.
    pub fn refresh<T>(&self, term: &Term<T>, max_history: usize) -> Fresh {
        let grid = term.grid();
        let same_grid = !self.rows.is_empty()
            && self.columns == grid.columns()
            && self.alt_screen == term.mode().contains(TermMode::ALT_SCREEN);
        let moved = if same_grid { self.shift(term, max_history) } else { None };
        // Rows still in the Scrollback that were there before are kept; the rest is copied.
        let keep = match moved {
            Some(n) if n < self.history && self.history - n <= grid.history_size() => n..self.history,
            _ => 0..0,
        };
        let first = if keep.is_empty() {
            grid.topmost_line()
        } else {
            Line((keep.end - keep.start) as i32 - grid.history_size() as i32)
        };
        let mut tail = Self {
            history: grid.history_size(),
            columns: grid.columns(),
            alt_screen: term.mode().contains(TermMode::ALT_SCREEN),
            ..Self::default()
        };
        tail.push_rows(term, first, grid.bottommost_line());
        let refreshed = match moved {
            Some(n) if !keep.is_empty() => Refreshed::Moved(n),
            // Nothing in the Scrollback to keep, yet the screen's rows are where they were.
            Some(0) if self.history == 0 && same_grid => Refreshed::Moved(0),
            _ => Refreshed::Anew,
        };
        Fresh { keep, tail, refreshed }
    }

    /// The new copy: the rows kept from this one, then the rows `refresh` copied.
    pub fn assemble(&self, fresh: Fresh) -> (Self, Refreshed) {
        let Fresh { keep, mut tail, refreshed } = fresh;
        if keep.is_empty() {
            return (tail, refreshed);
        }
        let (start, end) = (self.rows[keep.start].start, self.rows[keep.end].start);
        let mut text = String::with_capacity(end - start + tail.text.len());
        text.push_str(&self.text[start..end]);
        let mut rows: Vec<Range<usize>> = Vec::with_capacity(keep.len() + tail.rows.len());
        rows.extend(self.rows[keep].iter().map(|r| r.start - start..r.end - start));
        let base = text.len();
        text.push_str(&tail.text);
        rows.extend(tail.rows.iter().map(|r| r.start + base..r.end + base));
        tail.text = text;
        tail.rows = rows;
        (tail, refreshed)
    }

    /// How many rows the grid has moved up since the copy was made: rows that dropped off the
    /// top of a full Scrollback. `None` when the copy's rows can't be found in the grid (it was
    /// cleared, resized or rewrapped).
    ///
    /// Rows found in the Scrollback where the copy expects them confirm a guess; most guesses
    /// fail at the first character compared, so trying every distance stays cheap.
    pub fn shift<T>(&self, term: &Term<T>, max_history: usize) -> Option<usize> {
        let grid = term.grid();
        if self.columns != grid.columns() || self.alt_screen != term.mode().contains(TermMode::ALT_SCREEN) {
            return None;
        }
        let history = grid.history_size();
        if self.history == 0 {
            return Some(0);
        }
        // Rows only drop off the top of a full Scrollback.
        let most = if history >= max_history { self.history } else { 0 };
        (0..=most).find(|&moved| self.history_matches(term, moved))
    }

    /// Whether the copy's Scrollback rows, moved up by `moved`, are in the grid's Scrollback.
    fn history_matches<T>(&self, term: &Term<T>, moved: usize) -> bool {
        let grid = term.grid();
        let history = grid.history_size();
        if moved >= self.history {
            // All of them dropped off: nothing to compare, nothing to keep.
            return moved == self.history;
        }
        if self.history - moved > history {
            return false;
        }
        let line = |row: usize| Line(row as i32 - moved as i32 - history as i32);
        // The newest rows first (where a wrong guess shows soonest), then a few further up.
        let newest = (moved.max(self.history.saturating_sub(SAMPLES))..self.history).rev();
        let spread = [moved, moved + (self.history - moved) / 2];
        newest.chain(spread).all(|row| row_equals(&grid[line(row)], self.row_text(row)))
    }

    /// The text of the copy's row `row`.
    pub fn row_text(&self, row: usize) -> &str {
        &self.text[self.rows[row].clone()]
    }

    pub fn row_count(&self) -> usize {
        self.rows.len()
    }

    /// The row a byte of the text is in, and where in that row.
    pub fn position(&self, offset: usize) -> (usize, usize) {
        let row = self.rows.partition_point(|r| r.start <= offset).saturating_sub(1);
        (row, offset.saturating_sub(self.rows[row].start))
    }

    /// The byte at `byte` into row `row`, kept within the row.
    pub fn offset(&self, row: usize, byte: usize) -> usize {
        match self.rows.get(row.min(self.rows.len().saturating_sub(1))) {
            Some(range) => range.start.saturating_add(byte).min(range.end),
            None => 0,
        }
    }

    /// The part of each row that `range` covers: (row, byte range within the row).
    pub fn spans(&self, range: Range<usize>) -> impl Iterator<Item = (usize, Range<usize>)> + '_ {
        let first = self.position(range.start).0;
        self.rows[first..].iter().enumerate().take_while(move |(_, r)| r.start < range.end).filter_map(move |(i, r)| {
            let start = range.start.max(r.start) - r.start;
            let end = range.end.min(r.end) - r.start;
            (start < end).then(|| (first + i, start..end))
        })
    }
}

/// The rows [`Snapshot::refresh`] copied and what to keep of the old copy.
pub struct Fresh {
    keep: Range<usize>,
    tail: Snapshot,
    refreshed: Refreshed,
}

/// The cells of a row that hold text, as the copy holds them: up to the last one written, and
/// without the spacer after a wide character or before one that didn't fit at the line's end.
fn text_cells(row: &Row<Cell>) -> impl Iterator<Item = (usize, &Cell)> {
    let len = row.line_length().0;
    row[..][..len]
        .iter()
        .enumerate()
        .filter(|(_, cell)| !cell.flags.intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER))
}

/// The character a cell shows. A tab is shown as blank space.
fn shown(cell: &Cell) -> char {
    if cell.c == '\t' { ' ' } else { cell.c }
}

/// Append a row's text. Returns whether the row wraps into the next.
fn push_row(row: &Row<Cell>, out: &mut String) -> bool {
    for (_, cell) in text_cells(row) {
        out.push(shown(cell));
        out.extend(cell.zerowidth().into_iter().flatten());
    }
    wraps(row)
}

fn wraps(row: &Row<Cell>) -> bool {
    let len = row.line_length().0;
    len > 0 && row[Column(len - 1)].flags.contains(Flags::WRAPLINE)
}

/// Whether a row of the grid holds `text`, stopping at the first difference.
pub fn row_equals(row: &Row<Cell>, text: &str) -> bool {
    let mut rest = text.chars();
    for (_, cell) in text_cells(row) {
        if rest.next() != Some(shown(cell)) {
            return false;
        }
        for c in cell.zerowidth().into_iter().flatten() {
            if rest.next() != Some(*c) {
                return false;
            }
        }
    }
    rest.next().is_none()
}

/// The columns a byte range of a row's text covers.
pub fn columns(row: &Row<Cell>, bytes: Range<usize>) -> Option<Range<usize>> {
    let (mut at, mut first, mut end) = (0, None, None);
    for (col, cell) in text_cells(row) {
        let len = shown(cell).len_utf8() + cell.zerowidth().map_or(0, |z| z.iter().map(|c| c.len_utf8()).sum::<usize>());
        if at >= bytes.end {
            break;
        }
        if at + len > bytes.start {
            first.get_or_insert(col);
            end = Some(col + if cell.flags.contains(Flags::WIDE_CHAR) { 2 } else { 1 });
        }
        at += len;
    }
    Some(first?..end?)
}

/// A match on the grid: its first and last cell, for selecting it.
pub fn grid_span<T>(term: &Term<T>, snapshot: &Snapshot, range: Range<usize>, moved: usize) -> Option<(Point, Point)> {
    let grid = term.grid();
    let line = |row: usize| Line(row as i32 - moved as i32 - grid.history_size() as i32);
    let mut cells = snapshot.spans(range).map(|(row, bytes)| {
        let line = line(row);
        if line < grid.topmost_line() || line > grid.bottommost_line() || !row_equals(&grid[line], snapshot.row_text(row)) {
            return None;
        }
        columns(&grid[line], bytes).map(|cols| (line, cols))
    });
    let (first_line, first_cols) = cells.next()??;
    let mut last = (first_line, first_cols.clone());
    for cell in cells {
        last = cell?;
    }
    Some((Point::new(first_line, Column(first_cols.start)), Point::new(last.0, Column(last.1.end - 1))))
}

#[cfg(test)]
mod tests {
    use super::*;
    use alacritty_terminal::event::VoidListener;
    use alacritty_terminal::term::Config;
    use alacritty_terminal::term::test::TermSize;
    use alacritty_terminal::vte::ansi::Processor;

    fn term(cols: usize, rows: usize, history: usize) -> (Term<VoidListener>, Processor) {
        let config = Config { scrolling_history: history, ..Config::default() };
        (Term::new(config, &TermSize::new(cols, rows), VoidListener), Processor::new())
    }

    fn print(term: &mut Term<VoidListener>, parser: &mut Processor, text: &str) {
        parser.advance(term, text.replace('\n', "\r\n").as_bytes());
    }

    fn query(text: &str) -> Regex {
        Query { text: text.into(), ..Query::default() }.compile().unwrap().unwrap()
    }

    /// Where each match is on the grid now: (line, first column, last column).
    fn found(term: &Term<VoidListener>, snapshot: &Snapshot, matches: &Matches) -> Vec<(i32, usize, usize)> {
        matches
            .ranges
            .iter()
            .map(|m| {
                let (start, end) = grid_span(term, snapshot, m.clone(), 0).expect("match on the grid");
                assert_eq!(start.line, end.line, "single-line match");
                (start.line.0, start.column.0, end.column.0)
            })
            .collect()
    }

    #[test]
    fn finds_text_in_the_scrollback_and_on_screen() {
        let (mut term, mut p) = term(40, 5, 1000);
        for i in 0..20 {
            print(&mut term, &mut p, &format!("line {i} ok\n"));
        }
        print(&mut term, &mut p, "boom: ERROR here\n$ ");
        let snapshot = Snapshot::copy(&term);
        assert_eq!(snapshot.history, term.grid().history_size());
        let matches = Matches::find(&snapshot, &query("error"));
        assert_eq!(matches.total, 1);
        // Case-insensitive by default. The line is the last but one: the prompt is below it.
        assert_eq!(found(&term, &snapshot, &matches), vec![(3, 6, 10)]);
        let matches = Matches::find(&snapshot, &query("line 1"));
        // line 1, line 10..19
        assert_eq!(matches.total, 11);
        assert_eq!(matches.starts, [1, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19]);
        assert!(matches.ranges.iter().zip(&matches.starts).all(|(m, &row)| snapshot.position(m.start).0 == row));
        assert_eq!(found(&term, &snapshot, &matches)[0].0, -(term.grid().history_size() as i32) + 1);
    }

    #[test]
    fn case_and_regular_expressions() {
        let (mut term, mut p) = term(40, 5, 100);
        print(&mut term, &mut p, "Error error ERROR\nid=42 id=7\n");
        let snapshot = Snapshot::copy(&term);
        let count = |q: Query| Matches::find(&snapshot, &q.compile().unwrap().unwrap()).total;
        assert_eq!(count(Query { text: "error".into(), ..Default::default() }), 3);
        assert_eq!(count(Query { text: "Error".into(), case_sensitive: true, ..Default::default() }), 1);
        assert_eq!(count(Query { text: r"id=\d+".into(), regex: true, ..Default::default() }), 2);
        // Literal text is taken as it is.
        assert_eq!(count(Query { text: r"id=\d+".into(), ..Default::default() }), 0);
        assert_eq!(count(Query { text: "^id".into(), regex: true, ..Default::default() }), 1);
        assert!(Query { text: "(".into(), regex: true, ..Default::default() }.compile().is_err());
        assert_eq!(Query::default().compile().unwrap().map(|r| r.to_string()), None);
    }

    #[test]
    fn a_match_spans_a_wrapped_line_and_wide_characters() {
        let (mut term, mut p) = term(10, 5, 100);
        print(&mut term, &mut p, "abcdefgh-NEEDLE-xyz\n漢字x\n");
        let snapshot = Snapshot::copy(&term);
        let matches = Matches::find(&snapshot, &query("needle"));
        let (start, end) = grid_span(&term, &snapshot, matches.ranges[0].clone(), 0).unwrap();
        assert_eq!((start.line.0, start.column.0), (0, 9));
        assert_eq!((end.line.0, end.column.0), (1, 4));
        // Each wide character covers two columns.
        let matches = Matches::find(&snapshot, &query("字x"));
        let (start, end) = grid_span(&term, &snapshot, matches.ranges[0].clone(), 0).unwrap();
        assert_eq!((start.line.0, start.column.0, end.column.0), (2, 2, 4));
    }

    #[test]
    fn refreshing_copies_only_new_rows_until_the_scrollback_is_full() {
        let (mut term, mut p) = term(30, 4, 1000);
        for i in 0..50 {
            print(&mut term, &mut p, &format!("first {i}\n"));
        }
        let old = Snapshot::copy(&term);
        for i in 0..30 {
            print(&mut term, &mut p, &format!("second {i}\n"));
        }
        let fresh = old.refresh(&term, 1000);
        // Rows that were in the Scrollback are kept; the old screen and what followed is copied.
        assert_eq!(fresh.keep, 0..old.history);
        let (new, refreshed) = old.assemble(fresh);
        assert_eq!(refreshed, Refreshed::Moved(0));
        let whole = Snapshot::copy(&term);
        assert_eq!(new.text, whole.text);
        assert_eq!(new.rows, whole.rows);
        assert_eq!(new.history, whole.history);
    }

    #[test]
    fn refreshing_a_full_scrollback_finds_how_far_rows_moved() {
        let (mut term, mut p) = term(30, 4, 40);
        for i in 0..100 {
            print(&mut term, &mut p, &format!("log {i}\n"));
        }
        assert_eq!(term.grid().history_size(), 40);
        let old = Snapshot::copy(&term);
        for i in 100..113 {
            print(&mut term, &mut p, &format!("log {i}\n"));
        }
        assert_eq!(old.shift(&term, 40), Some(13));
        let (new, refreshed) = old.assemble(old.refresh(&term, 40));
        assert_eq!(refreshed, Refreshed::Moved(13));
        let whole = Snapshot::copy(&term);
        assert_eq!(new.text, whole.text);
        assert_eq!(new.rows, whole.rows);
        // The matches line up with the grid again.
        let matches = Matches::find(&new, &query("log 99"));
        assert_eq!(found(&term, &new, &matches).len(), 1);
    }

    #[test]
    fn a_cleared_or_rewrapped_grid_is_copied_anew() {
        let (mut term, mut p) = term(30, 4, 1000);
        for i in 0..20 {
            print(&mut term, &mut p, &format!("row {i}\n"));
        }
        let old = Snapshot::copy(&term);
        // `clear`: the screen and the Scrollback (ED 3) are erased.
        print(&mut term, &mut p, "\x1b[H\x1b[2J\x1b[3Jfresh\n");
        let (new, refreshed) = old.assemble(old.refresh(&term, 1000));
        assert_eq!(refreshed, Refreshed::Anew);
        assert_eq!(new.text, Snapshot::copy(&term).text);

        let old = Snapshot::copy(&term);
        term.resize(TermSize::new(20, 4));
        assert_eq!(old.shift(&term, 1000), None);
        let (_, refreshed) = old.assemble(old.refresh(&term, 1000));
        assert_eq!(refreshed, Refreshed::Anew);
    }

    #[test]
    fn a_changed_screen_row_is_not_shown_as_a_match() {
        let (mut term, mut p) = term(30, 4, 100);
        print(&mut term, &mut p, "$ find me\n");
        let snapshot = Snapshot::copy(&term);
        let matches = Matches::find(&snapshot, &query("find"));
        assert!(grid_span(&term, &snapshot, matches.ranges[0].clone(), 0).is_some());
        // The program redraws the screen.
        print(&mut term, &mut p, "\x1b[Hsomething else");
        assert!(grid_span(&term, &snapshot, matches.ranges[0].clone(), 0).is_none());
    }

    #[test]
    fn the_match_shown_first_is_the_nearest_one_above() {
        let matches = Matches { ranges: vec![0..2, 10..12, 20..22], total: 3, ..Matches::default() };
        assert_eq!(matches.at_or_before(15), Some(1));
        assert_eq!(matches.at_or_before(10), Some(1));
        assert_eq!(matches.at_or_before(100), Some(2));
        // Nothing above: the newest.
        let matches = Matches { ranges: vec![10..12, 20..22], total: 2, ..Matches::default() };
        assert_eq!(matches.at_or_before(5), Some(1));
        assert_eq!(Matches::default().at_or_before(5), None);
    }

    #[test]
    fn only_the_newest_matches_are_kept() {
        let (mut term, mut p) = term(20, 4, 300_000);
        print(&mut term, &mut p, &"x\n".repeat(MAX_MATCHES + 7));
        let snapshot = Snapshot::copy(&term);
        let matches = Matches::find(&snapshot, &query("x"));
        assert_eq!(matches.total, MAX_MATCHES + 7);
        assert_eq!(matches.ranges.len(), MAX_MATCHES);
        assert_eq!(matches.ordinal(0), 8);
        assert_eq!(matches.ordinal(MAX_MATCHES - 1), MAX_MATCHES + 7);
        assert_eq!(matches.ranges.last().unwrap().start, snapshot.text.rfind('x').unwrap());
        assert_eq!(matches.starts.len(), MAX_MATCHES);
        assert_eq!(*matches.starts.last().unwrap(), snapshot.position(snapshot.text.rfind('x').unwrap()).0);
    }
}
