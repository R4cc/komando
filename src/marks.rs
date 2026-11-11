//! Command Marks: the lines a user entered in a Session, so the command rail can list them and
//! jump back to them.
//!
//! A mark is tied to its row by a private hyperlink stamped on the row's first cell. The grid
//! carries that tag along when lines scroll into the Scrollback, when old lines drop off the top,
//! and when a resize reflows the text, so a mark always finds its row again without counting
//! lines. When the row is overwritten or cleared, the tag goes with it and the mark is dropped.

use alacritty_terminal::Term;
use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::index::{Column, Line, Point};
use alacritty_terminal::term::cell::{Flags, Hyperlink};
use chrono::{DateTime, Local};

/// Identifies Komando's tags among hyperlinks that programs print themselves (OSC 8).
const TAG_URI: &str = "komando:command-mark";
/// Most marks a Session keeps; the oldest go first.
const MAX_MARKS: usize = 1000;
/// Rows of a wrapped line that are read for its text.
const MAX_ROWS: i32 = 8;

#[derive(Debug, Clone)]
pub struct Mark {
    pub id: u64,
    /// The entered line as shown on screen, prompt included.
    pub line: String,
    pub at: DateTime<Local>,
    /// Row position as `history_size + line` when last seen, to skip rescans while nothing moved.
    abs: i64,
}

impl Mark {
    /// The line split into prompt and command, for display.
    pub fn parts(&self) -> (&str, &str) {
        split_prompt(&self.line)
    }
}

/// Enter was pressed; the line is read once the shell has moved past it, so redraws on Enter
/// (PSReadLine dropping a prediction, for example) are not captured.
struct Pending {
    id: u64,
    /// Row of the line's start as `history_size + line` at Enter, used if the tag was overwritten.
    abs: i64,
    /// Cursor row as `history_size + line` at Enter.
    cursor_abs: i64,
}

#[derive(Default)]
pub struct Marks {
    marks: Vec<Mark>,
    pending: Option<Pending>,
    next_id: u64,
}

impl Marks {
    pub fn list(&self) -> &[Mark] {
        &self.marks
    }

    /// Enter was pressed and the line has not been read yet.
    pub fn is_pending(&self) -> bool {
        self.pending.is_some()
    }

    pub fn get(&self, id: u64) -> Option<&Mark> {
        self.marks.iter().find(|m| m.id == id)
    }

    /// Enter was pressed on the line under the cursor. Call before the key reaches the shell.
    pub fn entered<T>(&mut self, term: &mut Term<T>) {
        self.finish(term);
        let grid = term.grid();
        let history = grid.history_size() as i64;
        let cursor = grid.cursor.point.line;
        let start = logical_start(term, cursor);
        self.next_id += 1;
        let id = self.next_id;
        stamp(term, start, id);
        self.pending = Some(Pending { id, abs: history + start.0 as i64, cursor_abs: history + cursor.0 as i64 });
    }

    /// New output arrived. Reads the pending line once the cursor has moved below it.
    pub fn output<T>(&mut self, term: &mut Term<T>) {
        let Some(pending) = &self.pending else { return };
        let grid = term.grid();
        let cursor = grid.cursor.point.line;
        // Once Scrollback is full the cursor's absolute row stops growing, so prefer the tag.
        let moved_on = match find_tag(term, pending.id) {
            Some(start) => cursor > logical_end(term, start),
            None => grid.history_size() as i64 + cursor.0 as i64 > pending.cursor_abs,
        };
        if moved_on {
            self.finish(term);
        }
    }

    fn finish<T>(&mut self, term: &mut Term<T>) {
        let Some(pending) = self.pending.take() else { return };
        let line = match find_tag(term, pending.id) {
            Some(line) => line,
            None => {
                // The shell redrew the line from its first column. Without overflowing Scrollback
                // the position from Enter is still exact.
                let line = Line((pending.abs - term.grid().history_size() as i64) as i32);
                if line < term.grid().topmost_line() || line > term.grid().bottommost_line() {
                    return;
                }
                stamp(term, line, pending.id);
                line
            }
        };
        let text = logical_text(term, line);
        if !worth_marking(&text) {
            term.grid_mut()[line][Column(0)].set_hyperlink(None);
            return;
        }
        let abs = term.grid().history_size() as i64 + line.0 as i64;
        self.marks.push(Mark { id: pending.id, line: text, at: Local::now(), abs });
        if self.marks.len() > MAX_MARKS {
            self.marks.remove(0);
        }
    }

    /// Each mark's current row (negative rows are in the Scrollback), oldest first. Marks whose
    /// row is gone are dropped. Call only while the normal screen is showing: tags live there.
    pub fn rows<T>(&mut self, term: &Term<T>) -> Vec<(u64, Line)> {
        let grid = term.grid();
        let history = grid.history_size() as i64;
        let (top, bottom) = (grid.topmost_line(), grid.bottommost_line());
        let at = |abs: i64| Line((abs - history) as i32);
        let still_there = self.marks.iter().all(|m| {
            let line = at(m.abs);
            line >= top && line <= bottom && tag_id(&grid[line][Column(0)]) == Some(m.id)
        });
        if !still_there {
            self.rescan(term);
        }
        self.marks.iter().map(|m| (m.id, at(m.abs))).collect()
    }

    fn rescan<T>(&mut self, term: &Term<T>) {
        let grid = term.grid();
        let history = grid.history_size() as i64;
        let mut found = std::collections::HashMap::new();
        let mut line = grid.topmost_line();
        while line <= grid.bottommost_line() {
            if let Some(id) = tag_id(&grid[line][Column(0)]) {
                found.insert(id, history + line.0 as i64);
            }
            line += 1;
        }
        self.marks.retain_mut(|m| match found.get(&m.id) {
            Some(&abs) => {
                m.abs = abs;
                true
            }
            None => false,
        });
    }
}

fn stamp<T>(term: &mut Term<T>, line: Line, id: u64) {
    term.grid_mut()[line][Column(0)].set_hyperlink(Some(Hyperlink::new(Some(id), TAG_URI.into())));
}

fn tag_id(cell: &alacritty_terminal::term::cell::Cell) -> Option<u64> {
    let link = cell.hyperlink()?;
    (link.uri() == TAG_URI).then(|| link.id().parse().ok()).flatten()
}

fn find_tag<T>(term: &Term<T>, id: u64) -> Option<Line> {
    let grid = term.grid();
    let mut line = grid.bottommost_line();
    while line >= grid.topmost_line() {
        if tag_id(&grid[line][Column(0)]) == Some(id) {
            return Some(line);
        }
        line -= 1;
    }
    None
}

fn wraps<T>(term: &Term<T>, line: Line) -> bool {
    let grid = term.grid();
    grid[line][grid.last_column()].flags.contains(Flags::WRAPLINE)
}

/// First row of the (possibly wrapped) line that `line` belongs to.
fn logical_start<T>(term: &Term<T>, mut line: Line) -> Line {
    let top = term.grid().topmost_line();
    let mut steps = 0;
    while line > top && steps < MAX_ROWS && wraps(term, line - 1) {
        line -= 1;
        steps += 1;
    }
    line
}

/// Last row of the (possibly wrapped) line starting at `start`.
fn logical_end<T>(term: &Term<T>, start: Line) -> Line {
    let bottom = term.grid().bottommost_line();
    let mut end = start;
    while end < bottom && end.0 - start.0 < MAX_ROWS - 1 && wraps(term, end) {
        end += 1;
    }
    end
}

fn logical_text<T>(term: &Term<T>, start: Line) -> String {
    let end = logical_end(term, start);
    let text = term.bounds_to_string(Point::new(start, Column(0)), Point::new(end, term.grid().last_column()));
    text.trim().to_string()
}

/// Skips Enter on an empty prompt and answers to password prompts.
fn worth_marking(line: &str) -> bool {
    let (_, command) = split_prompt(line);
    if command.is_empty() {
        return false;
    }
    let lower = line.to_lowercase();
    let asks_secret = ["password", "passphrase", "passwort", "pin"].iter().any(|w| lower.contains(w));
    !(asks_secret && line.trim_end().ends_with(':'))
}

/// Split `"[ops@web01 ~]$ systemctl status nginx"` into the prompt and the command after it.
/// The prompt ends at the first `$`, `#`, `>`, `%`, `❯` or `»` followed by a space. A line without
/// one is all command.
pub fn split_prompt(line: &str) -> (&str, &str) {
    let mut chars = line.char_indices().peekable();
    while let Some((i, c)) = chars.next() {
        if matches!(c, '$' | '#' | '>' | '%' | '❯' | '»') && chars.peek().is_none_or(|&(_, next)| next == ' ') {
            let end = i + c.len_utf8();
            return (line[..end].trim(), line[end..].trim());
        }
    }
    ("", line.trim())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::{EventProxy, GridSize, new_term};
    use alacritty_terminal::vte::ansi::Processor;
    use futures::channel::mpsc;

    struct Shell {
        term: crate::session::SharedTerm,
        parser: Processor,
        marks: Marks,
    }

    impl Shell {
        fn new(cols: u16, rows: u16, scrollback: usize) -> Self {
            let (tx, _rx) = mpsc::unbounded();
            let size = GridSize { cols, rows, cell_width: 8, cell_height: 16 };
            Self { term: new_term(size, scrollback, EventProxy::new(tx)), parser: Processor::new(), marks: Marks::default() }
        }

        fn print(&mut self, s: &str) {
            let mut term = self.term.lock();
            self.parser.advance(&mut *term, s.as_bytes());
            self.marks.output(&mut term);
        }

        /// Type a command at a prompt and press Enter; the "shell" echoes it and prints output.
        fn run(&mut self, command: &str, output: &[&str]) {
            self.print(&format!("$ {command}"));
            self.marks.entered(&mut *self.term.lock());
            self.print("\r\n");
            for line in output {
                self.print(&format!("{line}\r\n"));
            }
        }

        fn commands(&mut self) -> Vec<(String, i32)> {
            let term = self.term.lock();
            let rows = self.marks.rows(&term);
            rows.iter().map(|(id, line)| (self.marks.get(*id).unwrap().parts().1.to_string(), line.0)).collect()
        }
    }

    #[test]
    fn splits_common_prompts() {
        assert_eq!(split_prompt("[ops@web01 ~]$ systemctl status nginx"), ("[ops@web01 ~]$", "systemctl status nginx"));
        assert_eq!(split_prompt("root@db01:~# ls -la > out"), ("root@db01:~#", "ls -la > out"));
        assert_eq!(split_prompt(r"PS C:\Users\ops> Get-Process"), (r"PS C:\Users\ops>", "Get-Process"));
        assert_eq!(split_prompt(">>> print(1)"), (">>>", "print(1)"));
        assert_eq!(split_prompt("no prompt here"), ("", "no prompt here"));
    }

    #[test]
    fn skips_empty_lines_and_password_prompts() {
        assert!(worth_marking("$ ls"));
        assert!(!worth_marking("[ops@web01 ~]$"));
        assert!(!worth_marking("[sudo] password for ops:"));
        assert!(worth_marking("plain input"));
    }

    #[test]
    fn marks_follow_their_rows_into_the_scrollback() {
        let mut shell = Shell::new(40, 5, 100);
        shell.run("uptime", &["up 3 days"]);
        shell.run("df -h", &["a", "b", "c", "d", "e", "f"]);
        shell.print("$ ");
        // Ten rows on a 5-row screen: the first five are in the Scrollback.
        assert_eq!(shell.commands(), vec![("uptime".into(), -5), ("df -h".into(), -3)]);
    }

    #[test]
    fn marks_survive_lines_dropping_off_a_full_scrollback() {
        let mut shell = Shell::new(40, 4, 10);
        for i in 0..20 {
            shell.print(&format!("filler {i}\r\n"));
        }
        shell.run("tail log", &["x"; 5]);
        shell.run("echo hi", &["hi"]);
        // History is full, so every line from here on pushes the oldest one out.
        for i in 0..4 {
            shell.print(&format!("more {i}\r\n"));
        }
        shell.print("$ ");
        let commands = shell.commands();
        let term = shell.term.lock();
        for (command, line) in &commands {
            let text = term.bounds_to_string(Point::new(Line(*line), Column(0)), Point::new(Line(*line), Column(39)));
            assert_eq!(text.trim(), format!("$ {command}"));
        }
        assert_eq!(commands.len(), 2);
    }

    #[test]
    fn marks_go_when_their_rows_are_cleared() {
        let mut shell = Shell::new(40, 5, 100);
        shell.run("ls", &["a"]);
        // `clear` in bash: home, erase screen, erase Scrollback.
        shell.print("\x1b[H\x1b[2J\x1b[3J$ ");
        assert!(shell.commands().is_empty());
    }

    #[test]
    fn enter_on_an_empty_prompt_is_not_marked() {
        let mut shell = Shell::new(40, 5, 100);
        shell.run("", &[]);
        assert!(shell.commands().is_empty());
    }

    #[test]
    fn redrawn_lines_are_read_after_enter() {
        let mut shell = Shell::new(40, 5, 100);
        // A shell that shows a grey suggestion, then redraws the line from column 0 on Enter.
        shell.print("$ git st\x1b[2matus --short\x1b[0m");
        shell.marks.entered(&mut *shell.term.lock());
        shell.print("\r$ git st\x1b[K\r\n");
        shell.print("$ ");
        assert_eq!(shell.commands(), vec![("git st".into(), 0)]);
    }

    #[test]
    fn wrapped_lines_are_read_whole() {
        let mut shell = Shell::new(10, 5, 100);
        shell.run("echo abcdefghijkl", &[]);
        shell.print("$ ");
        assert_eq!(shell.commands(), vec![("echo abcdefghijkl".into(), 0)]);
    }
}
