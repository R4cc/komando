//! Sessions: the backend half of a terminal tab.
//!
//! Speed rule: bytes are parsed into the terminal grid on the backend thread (PTY reader
//! thread for Local Sessions, tokio task for SSH Sessions), never on the UI thread. The UI
//! is told "something changed" at most once until it has rendered, so a flood of output
//! costs one repaint per frame, not one per chunk.

pub mod local;
mod pace;
pub mod ssh;
mod sudo;

use std::borrow::Cow;
use std::sync::{Arc, Mutex};
use std::sync::atomic::{AtomicBool, Ordering};

use alacritty_terminal::Term;
use alacritty_terminal::event::{Event, EventListener};
use alacritty_terminal::event_loop::{EventLoopSender, Msg};
use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::index::{Column, Line, Point};
use alacritty_terminal::sync::FairMutex;
use alacritty_terminal::term::{Config as TermConfig, Osc52};
use alacritty_terminal::term::test::TermSize;
use futures::channel::{mpsc, oneshot};

use crate::theme::TerminalPalette;

pub type SharedTerm = Arc<FairMutex<Term<EventProxy>>>;

/// Messages from a backend to its tab.
pub enum SessionEvent {
    /// New output was parsed into the grid.
    Wakeup,
    Title(String),
    Status(Status),
    Prompt(Prompt),
    /// Progress on the way to the Host: `index` is the hop (Jump Hosts first, the Host last).
    Hop { index: usize, step: HopStep },
    /// Signed in to hop `hop` with something typed in and ticked "Remember": save it for
    /// that hop's Host.
    Remember { hop: usize, what: Remembered },
    /// A program asked to put text on the clipboard (OSC 52).
    ClipboardStore(String),
}

#[derive(Debug, Clone, PartialEq)]
pub enum Status {
    Connecting,
    Connected,
    /// The connection or shell ended. The tab keeps its Scrollback.
    Disconnected(String),
}

/// Where one hop of a connection is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HopStep {
    /// Opening the network connection, or the tunnel through the previous hop.
    Reaching,
    CheckingKey,
    SigningIn,
    /// Signed in: the next hop goes through this one, or the shell opens.
    Done,
}

pub enum Prompt {
    /// First connection to this Host: trust its Host Key?
    HostKey { fingerprint: String, algorithm: String, reply: oneshot::Sender<bool> },
    /// The Host Key differs from the trusted one. Shown as a blocking warning.
    HostKeyChanged { fingerprint: String },
    /// Ask for text (user name) or a secret (password, key passphrase). `None` cancels.
    /// `remember` shows a "Remember for this host" checkbox, ticked or not; `None` hides it.
    Input {
        label: String,
        secret: bool,
        remember: Option<bool>,
        error: Option<String>,
        reply: oneshot::Sender<Option<Answer>>,
    },
}

/// What was typed into an Input prompt, and whether to remember it for the Host.
pub struct Answer {
    pub value: String,
    pub remember: bool,
}

/// A typed-in sign-in detail to save for a Host.
#[derive(Clone)]
pub enum Remembered {
    User(String),
    Password(String),
}

/// Where keyboard input for a Session goes.
#[derive(Clone)]
pub enum InputSink {
    Local(EventLoopSender),
    Ssh(tokio::sync::mpsc::UnboundedSender<ssh::Command>),
}

impl InputSink {
    pub fn write(&self, bytes: Vec<u8>) {
        match self {
            InputSink::Local(sender) => {
                let _ = sender.send(Msg::Input(Cow::Owned(bytes)));
            }
            InputSink::Ssh(sender) => {
                let _ = sender.send(ssh::Command::Input(bytes));
            }
        }
    }

    pub fn resize(&self, size: GridSize) {
        match self {
            InputSink::Local(sender) => {
                let _ = sender.send(Msg::Resize(size.window_size()));
            }
            InputSink::Ssh(sender) => {
                let _ = sender.send(ssh::Command::Resize(size));
            }
        }
    }

    /// Ask an SSH Session for an SFTP channel on its connection. `None` for Local Sessions
    /// and for a Session whose connection has already ended.
    pub fn open_sftp(&self) -> Option<oneshot::Receiver<anyhow::Result<crate::sftp::Remote>>> {
        match self {
            InputSink::Local(_) => None,
            InputSink::Ssh(sender) => {
                let (reply, answer) = oneshot::channel();
                sender.send(ssh::Command::OpenSftp(reply)).ok()?;
                Some(answer)
            }
        }
    }

    pub fn close(&self) {
        match self {
            InputSink::Local(sender) => {
                let _ = sender.send(Msg::Shutdown);
            }
            InputSink::Ssh(sender) => {
                let _ = sender.send(ssh::Command::Close);
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GridSize {
    pub cols: u16,
    pub rows: u16,
    pub cell_width: u16,
    pub cell_height: u16,
}

impl GridSize {
    pub fn window_size(self) -> alacritty_terminal::event::WindowSize {
        alacritty_terminal::event::WindowSize {
            num_lines: self.rows,
            num_cols: self.cols,
            cell_width: self.cell_width,
            cell_height: self.cell_height,
        }
    }

    /// The text area in pixels, for programs that size images or layouts by it.
    pub fn pixel_width(self) -> u32 {
        self.cols as u32 * self.cell_width as u32
    }

    pub fn pixel_height(self) -> u32 {
        self.rows as u32 * self.cell_height as u32
    }

    pub fn term_size(self) -> TermSize {
        TermSize::new(self.cols as usize, self.rows as usize)
    }
}

/// Receives events from the terminal parser. Clones share the same flag and channels.
#[derive(Clone)]
pub struct EventProxy {
    tx: mpsc::UnboundedSender<SessionEvent>,
    dirty: Arc<AtomicBool>,
    /// Replies the terminal must send back to the program (e.g. cursor position reports).
    responder: Arc<parking_lot_lite::Slot<InputSink>>,
    /// What programs may ask about (colours, window size), kept current by the view.
    ///
    /// Queries are answered here, on the parser's thread and in order with the other
    /// replies: programs such as neovim send a colour query followed by a device-attributes
    /// query, and take the second answer arriving first as "not supported".
    reports: Arc<Mutex<Reports>>,
    /// Connection log lines on screen and not yet erased (see `ssh::ConnectLog`), over every
    /// attempt in this terminal: signing in erases the lines of earlier attempts that failed too.
    connect_log: Arc<std::sync::atomic::AtomicUsize>,
}

#[derive(Default)]
struct Reports {
    palette: Option<TerminalPalette>,
    size: Option<GridSize>,
}

impl EventProxy {
    pub fn new(tx: mpsc::UnboundedSender<SessionEvent>) -> Self {
        Self {
            tx,
            dirty: Arc::new(AtomicBool::new(false)),
            responder: Arc::new(Default::default()),
            reports: Arc::new(Mutex::new(Reports::default())),
            connect_log: Arc::default(),
        }
    }

    pub(crate) fn connect_log_lines(&self) -> Arc<std::sync::atomic::AtomicUsize> {
        self.connect_log.clone()
    }

    pub fn set_responder(&self, sink: InputSink) {
        self.responder.set(sink);
    }

    /// The colours OSC 4/10/11/12 queries are answered with.
    pub fn set_palette(&self, palette: &TerminalPalette) {
        let mut reports = self.reports.lock().unwrap();
        if reports.palette.as_ref() != Some(palette) {
            reports.palette = Some(palette.clone());
        }
    }

    /// The size window-size queries (CSI 14 t) are answered with.
    pub fn set_size(&self, size: GridSize) {
        self.reports.lock().unwrap().size = Some(size);
    }

    /// The last size the view laid the grid out at, if it has been laid out.
    pub fn size(&self) -> Option<GridSize> {
        self.reports.lock().unwrap().size
    }

    /// Called by the UI once it has picked up a Wakeup, re-arming the next one.
    pub fn clear_dirty(&self) {
        self.dirty.store(false, Ordering::Release);
    }

    pub fn send(&self, event: SessionEvent) {
        let _ = self.tx.unbounded_send(event);
    }

    fn reply(&self, text: String) {
        if let Some(sink) = self.responder.get() {
            sink.write(text.into_bytes());
        }
    }
}

impl EventListener for EventProxy {
    fn send_event(&self, event: Event) {
        match event {
            Event::Wakeup => {
                if !self.dirty.swap(true, Ordering::AcqRel) {
                    self.send(SessionEvent::Wakeup);
                }
            }
            Event::Title(title) => self.send(SessionEvent::Title(title)),
            Event::ResetTitle => self.send(SessionEvent::Title(String::new())),
            Event::PtyWrite(text) => self.reply(text),
            Event::ColorRequest(index, format) => {
                let color = self.reports.lock().unwrap().palette.as_ref().map(|p| p.color_at(index));
                if let Some(color) = color {
                    self.reply(format(color));
                }
            }
            Event::TextAreaSizeRequest(format) => {
                let size = self.reports.lock().unwrap().size;
                if let Some(size) = size {
                    self.reply(format(size.window_size()));
                }
            }
            Event::ClipboardStore(_, text) => self.send(SessionEvent::ClipboardStore(text)),
            Event::ChildExit(status) => {
                let reason = match status.code() {
                    Some(0) | None => "The shell exited.".to_string(),
                    Some(code) => format!("The shell exited with code {code}."),
                };
                self.send(SessionEvent::Status(Status::Disconnected(reason)));
            }
            Event::Exit => self.send(SessionEvent::Status(Status::Disconnected("The shell exited.".into()))),
            // Reading the clipboard (OSC 52 query) is never allowed: any program, including
            // one on a remote host, could read what was copied. The terminal is configured
            // not to ask (see `new_term`).
            _ => {}
        }
    }
}

/// A tiny mutex-protected optional value (avoids pulling in another dependency).
mod parking_lot_lite {
    use std::sync::Mutex;

    pub struct Slot<T>(Mutex<Option<T>>);

    impl<T> Default for Slot<T> {
        fn default() -> Self {
            Self(Mutex::new(None))
        }
    }

    impl<T: Clone> Slot<T> {
        pub fn set(&self, value: T) {
            *self.0.lock().unwrap() = Some(value);
        }

        pub fn get(&self) -> Option<T> {
            self.0.lock().unwrap().clone()
        }
    }
}

pub fn new_term(size: GridSize, scrollback_lines: usize, proxy: EventProxy) -> SharedTerm {
    let config = TermConfig {
        scrolling_history: scrollback_lines,
        // Programs may copy to the clipboard (OSC 52) but never read it.
        osc52: Osc52::OnlyCopy,
        ..TermConfig::default()
    };
    Arc::new(FairMutex::new(Term::new(config, &size.term_size(), proxy)))
}

/// Drop the Scrollback when it holds nothing but blank lines, so there is no empty space to
/// scroll up into (and no minimap) before real output scrolls off. Such lines come from a shell
/// repainting after a resize: Windows' pseudo-console scrolls blank rows up when it does.
/// Only looked at while the Scrollback is at most a screen tall, which keeps it cheap. Returns
/// whether it dropped anything.
pub fn drop_blank_scrollback<T>(term: &mut Term<T>) -> bool {
    let grid = term.grid();
    let history = grid.history_size();
    if history == 0 || history > grid.screen_lines() {
        return false;
    }
    let cols = grid.columns();
    let blank = (1..=history as i32).all(|row| {
        let line = &grid[Line(-row)];
        (0..cols).all(|col| matches!(line[Column(col)].c, ' ' | '\t'))
    });
    if blank {
        term.grid_mut().clear_history();
    }
    blank
}

/// Give every row of the Scrollback an allocation the size of the terminal's width.
///
/// Resizing leaves rows bigger than they need to be: alacritty keeps a row's allocation when the
/// terminal gets narrower, and widening a row at least doubles it (`Vec` growth). With a full
/// Scrollback, widening the window by one column doubled the Session's memory, for good, as rows
/// are recycled rather than reallocated. A clone of a row is allocated at exactly its length.
///
/// Works through the rows a stretch at a time, so the parser and the UI never wait long for the
/// terminal. Stops when the width changes again: the resize that changed it starts another pass.
pub fn compact_rows<T>(term: &FairMutex<Term<T>>, cols: usize) {
    const STRETCH: usize = 2048;
    // Rows done, counted from the oldest. Output that arrives in between shifts the rows a
    // little; a row missed that way stays as big as it was, which is harmless.
    let mut done = 0;
    loop {
        let mut term = term.lock();
        let grid = term.grid_mut();
        let total = grid.total_lines();
        if grid.columns() != cols || done >= total {
            return;
        }
        let top = -(grid.history_size() as i32);
        let end = (done + STRETCH).min(total);
        for i in done..end {
            let row = &mut grid[Line(top + i as i32)];
            *row = row.clone();
        }
        drop(term);
        done = end;
        std::thread::yield_now();
    }
}

/// The whole Scrollback plus screen as plain text: colours dropped, trailing blanks trimmed,
/// Windows line endings. The same as [`tidy_export`] of the terminal's own `bounds_to_string`,
/// built in one pass into one buffer, several times faster: the terminal is locked meanwhile,
/// and a full Scrollback of 100,000 lines took 60 ms the other way.
pub fn scrollback_text<T>(term: &Term<T>) -> String {
    use alacritty_terminal::term::cell::{Flags, LineLength};

    let grid = term.grid();
    let mut out = String::new();
    // Where the line being written starts, and where the text ends without blank lines after.
    let (mut line_start, mut content_end) = (0, 0);
    for line in -(grid.history_size() as i32)..grid.screen_lines() as i32 {
        let row = &grid[Line(line)];
        let len = row.line_length().0;
        let cells = &row[..][..len];
        if cells.iter().any(|cell| cell.c == '\t') {
            // A tab skips the cells up to the next tab stop, and only the terminal knows those.
            let (start, end) = (Point::new(Line(line), Column(0)), Point::new(Line(line), grid.last_column()));
            out.push_str(term.bounds_to_string(start, end).trim_end_matches('\n'));
        } else {
            for cell in cells {
                if !cell.flags.intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER) {
                    out.push(cell.c);
                    out.extend(cell.zerowidth().into_iter().flatten());
                }
            }
        }
        // A row that wraps goes on in the next one.
        if len == 0 || !cells[len - 1].flags.contains(Flags::WRAPLINE) {
            end_line(&mut out, &mut line_start, &mut content_end);
        }
    }
    if line_start < out.len() {
        end_line(&mut out, &mut line_start, &mut content_end);
    }
    out.truncate(content_end);
    out
}

/// Trim the line from `line_start` and end it.
fn end_line(out: &mut String, line_start: &mut usize, content_end: &mut usize) {
    let kept = out[*line_start..].trim_end().len();
    out.truncate(*line_start + kept);
    out.push_str("\r\n");
    *line_start = out.len();
    if kept > 0 {
        *content_end = out.len();
    }
}

/// Trim trailing spaces on each line and trailing empty lines, and use Windows line endings.
pub fn tidy_export(raw: &str) -> String {
    let mut lines: Vec<&str> = raw.lines().map(|l| l.trim_end()).collect();
    while lines.last().is_some_and(|l| l.is_empty()) {
        lines.pop();
    }
    let mut out = lines.join("\r\n");
    if !out.is_empty() {
        out.push_str("\r\n");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use alacritty_terminal::vte::ansi::Processor;

    #[test]
    fn export_trims_and_uses_crlf() {
        assert_eq!(tidy_export("a  \nb\n\n\n"), "a\r\nb\r\n");
        assert_eq!(tidy_export("\n\n"), "");
    }

    /// Every row, oldest first, cell by cell (characters, colours, flags such as wrapping).
    fn rows<T>(term: &Term<T>) -> Vec<Vec<alacritty_terminal::term::cell::Cell>> {
        let grid = term.grid();
        let top = -(grid.history_size() as i32);
        (top..grid.screen_lines() as i32)
            .map(|line| (0..grid.columns()).map(|col| grid[Line(line)][Column(col)].clone()).collect())
            .collect()
    }

    #[test]
    fn compacting_rows_after_a_resize_keeps_every_cell() {
        let (tx, _rx) = mpsc::unbounded();
        let size = GridSize { cols: 60, rows: 10, cell_width: 8, cell_height: 18 };
        let term = new_term(size, 5_000, EventProxy::new(tx));
        let mut parser: Processor = Processor::new();
        {
            let mut t = term.lock();
            // More than the Scrollback holds, in colour, some lines long enough to wrap.
            for i in 0..6_000 {
                let tail = "x".repeat(i % 90);
                parser.advance(&mut *t, format!("\x1b[3{}mline {i}\x1b[0m {tail}\x1b[1m!\x1b[0m\r\n", i % 8).as_bytes());
            }
            t.resize(GridSize { cols: 61, ..size }.term_size());
        }
        let before = rows(&*term.lock());
        let text = scrollback_text(&*term.lock());
        // Widening rejoined some wrapped lines; the Scrollback is still full of output.
        let history = term.lock().grid().history_size();
        assert!(history > 4_900);

        // A pass for a width the terminal no longer has changes nothing.
        compact_rows(&term, 60);
        assert_eq!(rows(&*term.lock()), before);

        compact_rows(&term, 61);
        let t = term.lock();
        assert_eq!(rows(&*t), before);
        assert_eq!(scrollback_text(&*t), text);
        assert_eq!(t.grid().history_size(), history);
        let grid = t.grid();
        let top = -(grid.history_size() as i32);
        assert!((top..grid.screen_lines() as i32).all(|line| grid[Line(line)].len() == 61));
    }

    #[test]
    fn blank_scrollback_is_dropped_but_output_is_kept() {
        let (tx, _rx) = mpsc::unbounded();
        let size = GridSize { cols: 20, rows: 4, cell_width: 8, cell_height: 18 };
        let term = new_term(size, 100, EventProxy::new(tx));
        let mut parser: Processor = Processor::new();
        let mut t = term.lock();
        assert!(!drop_blank_scrollback(&mut *t), "nothing to drop");
        // A repaint that scrolls blank rows up, then the prompt.
        parser.advance(&mut *t, b"\x1b[4;1H\n\n\n\x1b[Hprompt> ");
        assert_eq!(t.grid().history_size(), 3);
        assert!(drop_blank_scrollback(&mut *t));
        assert_eq!(t.grid().history_size(), 0);
        assert_eq!(scrollback_text(&*t), "prompt>\r\n");

        // Real output in the Scrollback stays, blank lines and all.
        parser.advance(&mut *t, b"\r\n\r\none\r\ntwo\r\nthree\r\n");
        assert!(t.grid().history_size() > 0);
        assert!(!drop_blank_scrollback(&mut *t));
        assert!(scrollback_text(&*t).starts_with("prompt>\r\n"));
    }

    #[test]
    fn scrollback_text_matches_the_terminals_own_text() {
        let (tx, _rx) = mpsc::unbounded();
        let size = GridSize { cols: 12, rows: 6, cell_width: 8, cell_height: 16 };
        let term = new_term(size, 100, EventProxy::new(tx));
        let mut parser: Processor = Processor::new();
        let mut t = term.lock();
        let output = [
            "plain\r\n",
            "\r\n",
            "trailing   \r\n",
            "\x1b[41m  coloured bg  \x1b[0m\r\n",
            "a line long enough to wrap twice, with spaces at the wrap   x\r\n",
            "wide \u{4e16}\u{754c}\u{4e16}\u{754c}\u{4e16}\u{754c} wraps\r\n",
            "e\u{301}\u{301} combining, zero\u{200d}width\r\n",
            "tab\there\tand\tthere\r\n",
            "\x1b]8;;https://example.com\x07link\x1b]8;;\x07\r\n",
            "\u{a0}nbsp\u{a0}\r\n",
            "\r\n\r\nlast\r\n\r\n",
        ];
        for chunk in output {
            parser.advance(&mut *t, chunk.as_bytes());
            let grid = t.grid();
            let top = Point::new(Line(-(grid.history_size() as i32)), Column(0));
            let bottom = Point::new(Line(grid.screen_lines() as i32 - 1), grid.last_column());
            assert_eq!(scrollback_text(&*t), tidy_export(&t.bounds_to_string(top, bottom)), "after {chunk:?}");
        }
        assert!(scrollback_text(&*t).starts_with("plain\r\n\r\ntrailing\r\n"));
        drop(t);
        let blank = new_term(size, 100, EventProxy::new(mpsc::unbounded().0));
        assert_eq!(scrollback_text(&*blank.lock()), "");
    }

    #[test]
    fn scrollback_text_includes_history_and_drops_colors() {
        let (tx, _rx) = mpsc::unbounded();
        let size = GridSize { cols: 20, rows: 3, cell_width: 8, cell_height: 16 };
        let term = new_term(size, 100, EventProxy::new(tx));
        let mut parser: Processor = Processor::new();
        let mut t = term.lock();
        for i in 1..=6 {
            parser.advance(&mut *t, format!("\x1b[31mline {i}\x1b[0m\r\n").as_bytes());
        }
        let text = scrollback_text(&*t);
        assert_eq!(text, "line 1\r\nline 2\r\nline 3\r\nline 4\r\nline 5\r\nline 6\r\n");
    }

    /// A terminal whose replies to the program land in the returned channel.
    fn replying_term() -> (SharedTerm, EventProxy, tokio::sync::mpsc::UnboundedReceiver<ssh::Command>) {
        let (tx, _rx) = mpsc::unbounded();
        let proxy = EventProxy::new(tx);
        let (input_tx, input_rx) = tokio::sync::mpsc::unbounded_channel();
        proxy.set_responder(InputSink::Ssh(input_tx));
        let size = GridSize { cols: 80, rows: 24, cell_width: 9, cell_height: 18 };
        proxy.set_size(size);
        proxy.set_palette(&crate::theme::TerminalPalette::dark());
        (new_term(size, 100, proxy.clone()), proxy, input_rx)
    }

    fn replies(rx: &mut tokio::sync::mpsc::UnboundedReceiver<ssh::Command>) -> String {
        let mut out = Vec::new();
        while let Ok(command) = rx.try_recv() {
            if let ssh::Command::Input(bytes) = command {
                out.extend(bytes);
            }
        }
        String::from_utf8(out).unwrap()
    }

    #[test]
    fn color_and_size_queries_are_answered_in_order() {
        let (term, _proxy, mut rx) = replying_term();
        let mut parser: Processor = Processor::new();
        // What neovim sends at startup: background colour, then primary device attributes.
        parser.advance(&mut *term.lock(), b"\x1b]11;?\x1b\\\x1b[c");
        let bg = crate::theme::TerminalPalette::dark().background;
        let want_bg = format!("\x1b]11;rgb:{0:02x}{0:02x}/{1:02x}{1:02x}/{2:02x}{2:02x}\x1b\\", bg.r, bg.g, bg.b);
        assert_eq!(replies(&mut rx), format!("{want_bg}\x1b[?6c"));

        // Text area in pixels (CSI 14 t) and in cells (CSI 18 t).
        parser.advance(&mut *term.lock(), b"\x1b[14t\x1b[18t");
        assert_eq!(replies(&mut rx), "\x1b[4;432;720t\x1b[8;24;80t");

        // Palette entry (OSC 4) and cursor position (DSR 6).
        parser.advance(&mut *term.lock(), b"\x1b]4;196;?\x07\x1b[3;5H\x1b[6n");
        assert_eq!(replies(&mut rx), "\x1b]4;196;rgb:ffff/0000/0000\x07\x1b[3;5R");
    }

    #[test]
    fn unfinished_synchronized_updates_are_shown_after_the_timeout() {
        let (term, proxy, _rx) = replying_term();
        let mut output = ssh::Output::default();
        output.feed(&term, &proxy, b"before\x1b[?2026hinside");
        assert!(output.sync_deadline().is_some());
        let text = || scrollback_text(&*term.lock());
        assert_eq!(text(), "before\r\n");
        output.flush(&term, &proxy);
        assert_eq!(text(), "beforeinside\r\n");
        assert!(output.sync_deadline().is_none());

        // A finished update is applied as soon as it ends.
        output.feed(&term, &proxy, b"\x1b[?2026h\r\nnext");
        output.feed(&term, &proxy, b"\x1b[?2026l");
        assert_eq!(text(), "beforeinside\r\nnext\r\n");
    }

    #[test]
    fn wakeups_are_coalesced_until_the_ui_catches_up() {
        let (tx, mut rx) = mpsc::unbounded();
        let proxy = EventProxy::new(tx);
        for _ in 0..1000 {
            proxy.send_event(Event::Wakeup);
        }
        assert!(matches!(rx.try_recv(), Ok(SessionEvent::Wakeup)));
        assert!(rx.try_recv().is_err());
        proxy.clear_dirty();
        proxy.send_event(Event::Wakeup);
        assert!(matches!(rx.try_recv(), Ok(SessionEvent::Wakeup)));
    }
}
