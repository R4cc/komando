//! Sessions: the backend half of a terminal tab.
//!
//! Speed rule: bytes are parsed into the terminal grid on the backend thread (PTY reader
//! thread for Local Sessions, tokio task for SSH Sessions), never on the UI thread. The UI
//! is told "something changed" at most once until it has rendered, so a flood of output
//! costs one repaint per frame, not one per chunk.

pub mod agent;
pub mod forward;
pub mod host_cert;
pub(crate) mod ledbat;
pub mod latency;
pub mod local;
mod pace;
pub(crate) mod password_prompt;
pub mod row_pool;
pub mod ssh;
mod sudo;
pub mod wait_for;

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
    /// A hop's smoothed SSH round trip and whether its latest reply is overdue.
    Latency(latency::HopLatency),
    /// A Port Forward started, failed, or its connections changed.
    Forward(forward::ForwardStatus),
    Prompt(Prompt),
    /// Progress on the way to the Host: `index` is the hop (Jump Hosts first, the Host last).
    Hop { index: usize, step: HopStep },
    /// Signed in to hop `hop` with something typed in and ticked "Remember": save it for
    /// that hop's Host.
    Remember { hop: usize, what: Remembered },
    /// A program asked to put text on the clipboard (OSC 52).
    ClipboardStore(String),
    /// The text of the wait with this id (see `EventProxy::wait_for`) appeared in the output.
    Appeared(u64),
}

#[derive(Debug, Clone, PartialEq)]
pub enum Status {
    Connecting,
    Connected,
    /// The connection or shell ended. The tab keeps its Scrollback.
    Disconnected(String),
}

/// The [`Status::Disconnected`] reason when the user ended the Session themselves.
pub const CLOSED_BY_USER: &str = "Session closed.";

/// How a [`Status::Disconnected`] reason begins when reaching a Host took too long.
pub const TIMED_OUT: &str = "Timed out connecting to ";
/// ... when the server stopped answering while signing in.
pub const TIMED_OUT_SIGNING_IN: &str = "Timed out signing in to ";
/// ... when the server didn't open the shell's channel.
pub const TIMED_OUT_OPENING_SHELL: &str = "Timed out opening the shell on ";

impl Status {
    /// The connection ended because a Host didn't answer in time.
    pub fn timed_out(&self) -> bool {
        matches!(self, Status::Disconnected(reason)
            if [TIMED_OUT, TIMED_OUT_SIGNING_IN, TIMED_OUT_OPENING_SHELL].iter().any(|start| reason.starts_with(start)))
    }
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
    /// First connection to this Host: trust its Host Key? `other_types` are the algorithms of
    /// keys already trusted for it: a key of a new type is not proof of the same server.
    HostKey { fingerprint: String, algorithm: String, other_types: Vec<String>, reply: oneshot::Sender<bool> },
    /// The Host Key differs from the trusted one: replace the trusted key with this one?
    /// `previous` is the trusted key's fingerprint, when it can be read back. `openssh` says it
    /// was trusted in the user's OpenSSH known_hosts, which Komando leaves as it is.
    HostKeyChanged {
        fingerprint: String,
        previous: Option<String>,
        algorithm: String,
        openssh: bool,
        reply: oneshot::Sender<bool>,
    },
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

    /// Ask an SSH Session for a tunnel to `port` on its Host. `None` for Local Sessions and
    /// for a Session whose connection has already ended.
    pub fn open_tunnel(&self, port: u16) -> Option<oneshot::Receiver<anyhow::Result<crate::desktop::Stream>>> {
        match self {
            InputSink::Local(_) => None,
            InputSink::Ssh(sender) => {
                let (reply, answer) = oneshot::channel();
                sender.send(ssh::Command::OpenTunnel { port, reply }).ok()?;
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
    /// How the last open shell in this terminal ended, kept until a later attempt signs in
    /// and records it with the reconnect (see `ssh::ConnectLog::ended`).
    connect_ended: Arc<Mutex<Option<ssh::Ended>>>,
    /// The text a Startup Command waits for, watched in the output as it arrives.
    wait: Arc<Mutex<Option<wait_for::WaitFor>>>,
    /// Whether `wait` holds one, so output is only looked at while something waits.
    waiting: Arc<AtomicBool>,
    /// Window activation, independent of terminal focus (e.g. while browsing files).
    window_active: tokio::sync::watch::Sender<bool>,
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
            connect_ended: Arc::default(),
            wait: Arc::default(),
            waiting: Arc::default(),
            window_active: tokio::sync::watch::channel(false).0,
        }
    }

    pub fn set_window_active(&self, active: bool) {
        self.window_active.send_if_modified(|current| std::mem::replace(current, active) != active);
    }

    pub(crate) fn window_activity(&self) -> tokio::sync::watch::Receiver<bool> {
        self.window_active.subscribe()
    }

    /// Watch the output for this text from now on, replacing any earlier wait (`None`: stop
    /// watching). `SessionEvent::Appeared` with its id says when it appears.
    pub fn wait_for(&self, wait: Option<wait_for::WaitFor>) {
        let mut slot = self.wait.lock().unwrap();
        self.waiting.store(wait.is_some(), Ordering::Release);
        *slot = wait;
    }

    /// Output arrived from the program: look for the awaited text in it.
    pub(crate) fn watch_output(&self, bytes: &[u8]) {
        if !self.waiting.load(Ordering::Acquire) {
            return;
        }
        let mut slot = self.wait.lock().unwrap();
        if let Some(wait) = slot.as_mut()
            && wait.feed(bytes)
        {
            let id = wait.id();
            *slot = None;
            self.waiting.store(false, Ordering::Release);
            self.send(SessionEvent::Appeared(id));
        }
    }

    pub(crate) fn connect_log_lines(&self) -> Arc<std::sync::atomic::AtomicUsize> {
        self.connect_log.clone()
    }

    pub(crate) fn connect_ended(&self) -> Arc<Mutex<Option<ssh::Ended>>> {
        self.connect_ended.clone()
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
            Event::CursorBlinkingChange => self.send_event(Event::Wakeup),
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
    let config = terminal_config(scrollback_lines);
    let mut term = Term::new(config, &size.term_size(), proxy);
    if scrollback_lines > 0 {
        // New Scrollback rows are built ahead, off the parser's thread, while it fills.
        term.grid_mut().set_row_source(Some(Arc::new(row_pool::RowPool::default())));
    }
    Arc::new(FairMutex::new(term))
}

pub(crate) fn terminal_config(scrollback_lines: usize) -> TermConfig {
    TermConfig {
        scrolling_history: scrollback_lines,
        default_cursor_style: alacritty_terminal::vte::ansi::CursorStyle {
            shape: alacritty_terminal::vte::ansi::CursorShape::Beam,
            blinking: true,
        },
        // Programs may copy to the clipboard (OSC 52) but never read it.
        osc52: Osc52::OnlyCopy,
        ..TermConfig::default()
    }
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
/// are recycled rather than reallocated.
///
/// Only rows that are bigger than their length get a new allocation: reallocating rows already
/// the right size (after [`resize`] widened them, say) would only hold a second copy of the
/// Scrollback until the heap gives the old one back.
///
/// Works through the rows a stretch at a time, so the parser and the UI never wait long for the
/// terminal. Stops when the width changes again: the resize that changed it starts another pass.
pub fn compact_rows<T>(term: &FairMutex<Term<T>>, cols: usize) {
    compact_rows_until(term, cols, &AtomicBool::new(false));
}

pub(crate) fn compact_rows_until<T>(term: &FairMutex<Term<T>>, cols: usize, cancelled: &AtomicBool) {
    const STRETCH: usize = 2048;
    // Rows done, counted from the oldest. Output that arrives in between shifts the rows a
    // little; a row missed that way stays as big as it was, which is harmless.
    let mut done = 0;
    loop {
        let mut term = term.lock();
        let grid = term.grid_mut();
        let total = grid.total_lines();
        if cancelled.load(Ordering::Relaxed) || grid.columns() != cols {
            return;
        }
        if done >= total {
            // The rows' old allocations, from before the resize and from here, are free now.
            drop(term);
            crate::memory::release_soon();
            return;
        }
        let top = -(grid.history_size() as i32);
        let end = (done + STRETCH).min(total);
        for i in done..end {
            let row = &mut grid[Line(top + i as i32)];
            let len = row.len();
            // Taking the cells hands over the row's allocation, but not its count of cells a
            // reset has to clear (see `occupied`).
            let mut cells = row.front_split_off(len);
            let occupied = occupied(&cells, len);
            if cells.capacity() > len {
                cells = cells.to_vec();
            }
            *row = alacritty_terminal::grid::Row::from_vec(cells, occupied);
        }
        drop(term);
        done = end;
        std::thread::yield_now();
    }
}

/// Resize the terminal. Getting wider, every row first gets an allocation of exactly the new
/// width, which the terminal then fills in place.
///
/// Left to itself, the terminal grows each row as a `Vec` does, to at least twice its size: with
/// a full Scrollback, one column wider briefly took twice the memory (three times, as Windows
/// counts it, until the heap released the old rows), and copying every row into its doubled
/// allocation took longer than reallocating it at the right size. `compact_rows` still trims
/// what reflowing leaves oversized.
///
/// Only the grid on screen is reached: while a full-screen program has the alternate screen up,
/// the Scrollback behind it grows the old way.
pub fn resize<T: EventListener>(term: &mut Term<T>, size: GridSize) {
    reserve_width(term, size.cols as usize);
    term.resize(size.term_size());
}

/// Prepare a wider Scrollback off the UI thread without changing its size or contents.
/// Each batch releases the lock so drawing and output can continue. Output may rotate new
/// rows into an already visited part of the grid; `resize` handles those remaining rows.
pub fn prepare_width<T>(term: &FairMutex<Term<T>>, from: usize, cols: usize, cancelled: &AtomicBool) -> bool {
    const BATCH: usize = 128;
    let mut done = 0;
    loop {
        if cancelled.load(Ordering::Relaxed) {
            return false;
        }
        let mut term = term.lock();
        let grid = term.grid_mut();
        if grid.columns() != from {
            return false;
        }
        let end = (done + BATCH).min(grid.total_lines());
        let top = -(grid.history_size() as i32);
        for i in done..end {
            reserve_row(&mut grid[Line(top + i as i32)], cols);
        }
        let finished = end >= grid.total_lines();
        drop(term);
        if finished {
            return true;
        }
        done = end;
        std::thread::yield_now();
    }
}

fn reserve_width<T>(term: &mut Term<T>, cols: usize) {
    let grid = term.grid_mut();
    if cols <= grid.columns() {
        return;
    }
    let top = -(grid.history_size() as i32);
    for line in top..grid.screen_lines() as i32 {
        reserve_row(&mut grid[Line(line)], cols);
    }
}

fn reserve_row(row: &mut alacritty_terminal::grid::Row<alacritty_terminal::term::cell::Cell>, cols: usize) {
    use alacritty_terminal::grid::Row;
    let len = row.len();
    // Taking all cells hands over their allocation without copying. Preserve the reset
    // accounting used by the synchronous resize, including rows with coloured blank tails.
    let mut cells = row.front_split_off(len);
    let occupied = if cells.capacity() >= cols {
        len
    } else {
        let occupied = occupied(&cells, cols);
        cells.reserve_exact(cols - len);
        occupied
    };
    *row = Row::from_vec(cells, occupied);
}

/// How many cells from the start of a row a reset has to clear once the row is `cols` wide.
///
/// A reset compares the background of the row's last cell with the new one: when they differ it
/// clears the whole row, otherwise only the cells counted. Past them, every cell must already be
/// what a reset makes it: blank, in that background. Growing to `cols` adds blank cells in the
/// default background, which is then the last cell's.
fn occupied(cells: &[alacritty_terminal::term::cell::Cell], cols: usize) -> usize {
    use alacritty_terminal::term::cell::Cell;
    let Some(last) = cells.last() else { return 0 };
    let bg = if cells.len() < cols { Cell::default().bg } else { last.bg };
    let blank = Cell { bg, ..Cell::default() };
    cells.iter().rposition(|cell| *cell != blank).map_or(0, |i| i + 1)
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
            // A wide character that wrapped onto the next row is written with that row: stop
            // before its spacer here, or the terminal adds it to this row too.
            let last = grid.last_column();
            let wraps_wide = row[last].flags.contains(Flags::LEADING_WIDE_CHAR_SPACER);
            let end_column = if wraps_wide { Column(last.0 - 1) } else { last };
            let (start, end) = (Point::new(Line(line), Column(0)), Point::new(Line(line), end_column));
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
    #[test]
    fn only_a_connection_that_ran_out_of_time_counts_as_timed_out() {
        use super::{Status, TIMED_OUT, TIMED_OUT_OPENING_SHELL, TIMED_OUT_SIGNING_IN};
        assert!(Status::Disconnected(format!("{TIMED_OUT}example.com:22.")).timed_out());
        assert!(Status::Disconnected(format!("{TIMED_OUT_SIGNING_IN}example.com:22.")).timed_out());
        assert!(Status::Disconnected(format!("{TIMED_OUT_OPENING_SHELL}example.com:22.")).timed_out());
        assert!(!Status::Disconnected("Connection refused (os error 111)".into()).timed_out());
        assert!(!Status::Disconnected(String::new()).timed_out());
        assert!(!Status::Connecting.timed_out());
    }

    #[test]
    fn cursor_defaults_blink_as_a_line_and_respect_program_requests() {
        use alacritty_terminal::vte::ansi::{CursorShape, Processor};
        let (tx, _rx) = futures::channel::mpsc::unbounded();
        let term = super::new_term(super::GridSize { cols: 20, rows: 3, cell_width: 8, cell_height: 16 }, 100, super::EventProxy::new(tx));
        let mut term = term.lock();
        assert_eq!(term.cursor_style().shape, CursorShape::Beam);
        assert!(term.cursor_style().blinking);
        let mut parser: Processor = Processor::new();
        parser.advance(&mut *term, b"\x1b[2 q");
        assert_eq!(term.cursor_style().shape, CursorShape::Block);
        assert!(!term.cursor_style().blinking);
        parser.advance(&mut *term, b"\x1bc");
        assert_eq!(term.cursor_style().shape, CursorShape::Beam);
        assert!(term.cursor_style().blinking);
    }

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

    /// Output that leaves rows with every kind of tail: coloured backgrounds to the edge, underlined
    /// spaces, wide characters, wrapped lines, links, combining characters, tabs, blank lines.
    /// Links have ids: a link without one gets a new id each time it is parsed.
    fn varied_output(i: usize) -> String {
        match i % 9 {
            0 => format!("\x1b[3{}mline {i}\x1b[0m {}\r\n", i % 8, "x".repeat(i % 70)),
            1 => format!("\x1b[44mblue to the edge {i}\x1b[K\x1b[0m\r\n"),
            2 => format!("under\x1b[4m      \x1b[0m{i}\r\n"),
            3 => format!("wide \u{4e16}\u{754c}\u{4e16}\u{754c}\u{4e16}\u{754c}\u{4e16}\u{754c}\u{4e16}\u{754c} {i}\r\n"),
            4 => format!("\x1b]8;id={i};https://example.com/{i}\x07link\x1b]8;;\x07 e\u{301} tab\there\r\n"),
            5 => "\r\n".into(),
            // Scrolls while the background is set: those rows are reset to green.
            6 => format!("\x1b[42mgreen {i}\r\n\x1b[0m"),
            7 => format!("{}\r\n", "long ".repeat(15)),
            _ => format!("\x1b[7m inverse {i} \x1b[0m\r\n"),
        }
    }

    #[test]
    fn resizing_keeps_every_cell_the_terminal_would_keep() {
        let size = GridSize { cols: 40, rows: 8, cell_width: 8, cell_height: 18 };
        let ours = new_term(size, 300, EventProxy::new(mpsc::unbounded().0));
        let plain = new_term(size, 300, EventProxy::new(mpsc::unbounded().0));
        let mut parsers: [Processor; 2] = [Processor::new(), Processor::new()];
        let mut feed = |from: usize, to: usize| {
            for i in from..to {
                let text = varied_output(i);
                parsers[0].advance(&mut *ours.lock(), text.as_bytes());
                parsers[1].advance(&mut *plain.lock(), text.as_bytes());
            }
        };
        let same = |when: &str| {
            let (a, b) = (ours.lock(), plain.lock());
            assert_eq!(a.grid().history_size(), b.grid().history_size(), "{when}");
            assert_eq!(a.grid().cursor.point, b.grid().cursor.point, "{when}");
            assert!(rows(&*a) == rows(&*b), "{when}: the rows differ");
            assert_eq!(scrollback_text(&*a), scrollback_text(&*b), "{when}");
        };
        // A full Scrollback, so new output recycles the oldest rows.
        feed(0, 500);
        same("before resizing");
        // One column wider, much wider, narrower, wider within what the rows had room for, and
        // wider than ever; after each, enough output to recycle every row, which resets them
        // (a resize leaves the terminal room for 1,000 more rows before it reuses old ones).
        // The rows are compacted after some steps, as when the width has settled.
        let steps = [(41u16, true), (97, true), (30, false), (60, false), (50, true), (120, true)];
        for (step, (cols, compact)) in steps.into_iter().enumerate() {
            let size = GridSize { cols, ..size };
            let from = ours.lock().columns();
            if cols > from as u16 {
                let before = rows(&*ours.lock());
                assert!(prepare_width(&ours, from, cols as usize, &AtomicBool::new(false)));
                assert_eq!(ours.lock().columns(), from, "preparation must not change layout");
                assert!(rows(&*ours.lock()) == before, "preparation changed cells");
            }
            resize(&mut *ours.lock(), size);
            plain.lock().resize(size.term_size());
            same(&format!("resized to {cols}"));
            feed(500 + step * 2_000, 2_500 + step * 2_000);
            same(&format!("output after resizing to {cols}"));
            if compact {
                compact_rows(&ours, cols as usize);
                same(&format!("compacted at {cols}"));
                feed(10_000 + step * 2_000, 12_000 + step * 2_000);
                same(&format!("output after compacting at {cols}"));
            }
        }
    }

    #[test]
    fn obsolete_width_preparation_and_compaction_stop_without_changing_cells() {
        let size = GridSize { cols: 40, rows: 8, cell_width: 8, cell_height: 18 };
        let term = new_term(size, 300, EventProxy::new(mpsc::unbounded().0));
        let mut parser: Processor = Processor::new();
        for i in 0..500 {
            parser.advance(&mut *term.lock(), varied_output(i).as_bytes());
        }
        let before = rows(&*term.lock());
        let cancelled = AtomicBool::new(true);
        assert!(!prepare_width(&term, 40, 80, &cancelled));
        // The layout changed before the worker acquired its first batch.
        assert!(!prepare_width(&term, 39, 80, &AtomicBool::new(false)));
        compact_rows_until(&term, 40, &cancelled);
        assert!(rows(&*term.lock()) == before);
    }

    #[test]
    fn narrowing_rows_matches_the_original_tail_split() {
        use alacritty_terminal::grid::{GridCell, Row};
        use alacritty_terminal::term::cell::Cell;
        let size = GridSize { cols: 40, rows: 8, cell_width: 8, cell_height: 18 };
        let term = new_term(size, 100, EventProxy::new(mpsc::unbounded().0));
        let mut parser: Processor = Processor::new();
        for i in 0..200 {
            parser.advance(&mut *term.lock(), varied_output(i).as_bytes());
        }
        // Differential check against the former split-then-trim operation, including blank
        // tails, wrapped text, links, styled spaces, combining characters and wide cells.
        for cells in rows(&*term.lock()) {
            for cols in 2..=41 {
                let mut original = cells.clone();
                let expected = if cols < original.len() {
                    let mut tail = original.split_off(cols);
                    tail.truncate(tail.iter().rposition(|cell| !cell.is_empty()).map_or(0, |i| i + 1));
                    (!tail.is_empty()).then_some(tail)
                } else {
                    None
                };
                let mut row: Row<Cell> = Row::from_vec(cells.clone(), cells.len());
                assert_eq!(row.shrink(cols), expected);
                assert_eq!(&row[..], &original);
                row.reset(&Cell::default());
                assert!(row.into_iter().all(|cell| *cell == Cell::default()));
            }
        }
    }

    #[test]
    fn occupied_cells_are_those_a_reset_has_to_clear() {
        use alacritty_terminal::grid::Row;
        use alacritty_terminal::term::cell::{Cell, Flags};
        use alacritty_terminal::vte::ansi::{Color, NamedColor};
        let mut row: Row<Cell> = Row::new(10);
        assert_eq!(occupied(&row[..], 10), 0);
        row[Column(3)].c = 'a';
        assert_eq!(occupied(&row[..], 10), 4);
        // A space with an attribute is not blank.
        row[Column(6)].flags.insert(Flags::UNDERLINE);
        assert_eq!(occupied(&row[..], 10), 7);
        row[Column(6)].flags = Flags::empty();
        // Spaces in the last cell's background are blank: a reset to that background leaves them
        // as they are, and one to any other background clears the whole row. Spaces in another
        // background before them are not.
        let blue = Color::Named(NamedColor::Blue);
        for col in 7..10 {
            row[Column(col)].bg = blue;
        }
        assert_eq!(occupied(&row[..], 10), 7);
        for col in 4..7 {
            row[Column(col)].bg = blue;
        }
        assert_eq!(occupied(&row[..], 10), 4);
        // Not once the row grows: its last cell will be in the default background.
        assert_eq!(occupied(&row[..], 11), 10);
        row[Column(9)].c = 'z';
        assert_eq!(occupied(&row[..], 10), 10);
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
    fn chunked_ssh_output_preserves_utf8_escapes_sync_and_replies() {
        // Move every part of the sequence across the actual production chunk boundary.
        for shift in 0..40 {
            let (term, proxy, mut replies_rx) = replying_term();
            let (reference, _reference_proxy, mut reference_rx) = replying_term();
            let mut bytes = vec![b'x'; 32 * 1024 - shift];
            bytes.extend_from_slice("日本 e\u{301}\x1b[31mERROR\x1b[0m\x1b[6n\x1b[?2026h".as_bytes());
            bytes.extend_from_slice(&b"buffered\r\n".repeat(4000));
            let mut output = ssh::Output::default();
            let mut parser: Processor = Processor::new();
            output.feed(&term, &proxy, &bytes);
            parser.advance(&mut *reference.lock(), &bytes);
            assert!(output.sync_deadline().is_some());
            assert_eq!(rows(&*term.lock()), rows(&*reference.lock()), "inside sync, shift {shift}");
            let end = b"\x1b[?2026l\x1b[6n";
            output.feed(&term, &proxy, end);
            parser.advance(&mut *reference.lock(), end);
            assert!(output.sync_deadline().is_none());
            assert_eq!(rows(&*term.lock()), rows(&*reference.lock()), "after sync, shift {shift}");
            assert_eq!(term.lock().grid().cursor.point, reference.lock().grid().cursor.point);
            assert_eq!(replies(&mut replies_rx), replies(&mut reference_rx), "shift {shift}");
        }
    }

    #[test]
    fn awaited_text_in_ssh_output_is_reported_once() {
        let (term, proxy, _replies) = replying_term();
        let (tx, mut rx) = mpsc::unbounded();
        let proxy = EventProxy { tx, ..proxy };
        let appeared = |rx: &mut mpsc::UnboundedReceiver<SessionEvent>| {
            std::iter::from_fn(|| rx.try_recv().ok()).filter_map(|e| if let SessionEvent::Appeared(id) = e { Some(id) } else { None }).collect::<Vec<_>>()
        };
        let mut output = ssh::Output::default();
        let wait = wait_for::WaitFor::new("# ", true);
        let id = wait.id();
        proxy.wait_for(Some(wait));
        output.feed(&term, &proxy, b"sudo -i # really
");
        assert!(appeared(&mut rx).is_empty(), "the echoed command line doesn't count");
        output.feed(&term, &proxy, b"[1;31mroot@web01[0m:~");
        output.feed(&term, &proxy, b"# ");
        assert_eq!(appeared(&mut rx), [id]);
        output.feed(&term, &proxy, b"
root@web01:~# ");
        assert!(appeared(&mut rx).is_empty(), "reported once");

        proxy.wait_for(Some(wait_for::WaitFor::new("done", false)));
        proxy.wait_for(None);
        output.feed(&term, &proxy, b"done");
        assert!(appeared(&mut rx).is_empty(), "no longer watched");
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

    /// Found by fuzzing (`fuzz_terminal_output`): copying a row that wraps a wide character onto
    /// the next row. alacritty's `line_to_string` took that character from the row *above*:
    /// on the top row of the Scrollback that panicked, elsewhere the copy got another row's
    /// first character. Komando copies through it for a selection (`selection_to_string`) and,
    /// for rows with a tab, for Copy output (`scrollback_text`), which must not then write the
    /// character twice.
    #[test]
    fn copying_a_row_that_wraps_a_wide_character() {
        // `fill` x's after `prefix` reach the last column, where the emoji doesn't fit.
        let term_with = |prefix: &[u8], fill: usize| {
            let (term, _proxy, _rx) = replying_term();
            let mut parser: Processor = Processor::new();
            let mut bytes = prefix.to_vec();
            bytes.extend(std::iter::repeat_n(b'x', fill));
            bytes.extend("\u{1F600} done".as_bytes());
            parser.advance(&mut *term.lock(), &bytes);
            term
        };
        let copy = |term: &SharedTerm, line: i32| {
            let t = term.lock();
            let (start, end) = (Point::new(Line(line), Column(0)), Point::new(Line(line), t.grid().last_column()));
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| t.bounds_to_string(start, end)))
        };
        let wrapped = format!("{}\u{1F600}", "x".repeat(79));
        assert_eq!(copy(&term_with(b"A\r\n", 79), 1).expect("copying a lower row"), wrapped);
        assert_eq!(copy(&term_with(b"", 79), 0).expect("copying the top row"), wrapped);

        // Copy output: once, whether the row has a tab or not, at the top or lower down.
        for (prefix, fill) in [(&b""[..], 79), (b"A\r\n", 79), (b"\t", 71), (b"A\r\n\t", 71)] {
            let term = term_with(prefix, fill);
            let t = term.lock();
            let row = if prefix.starts_with(b"A") { 1 } else { 0 };
            assert!(t.grid()[Line(row)][t.grid().last_column()].flags.contains(alacritty_terminal::term::cell::Flags::LEADING_WIDE_CHAR_SPACER));
            let exported = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| scrollback_text(&*t))).expect("Copy output");
            let line = format!("{}{}\u{1F600} done\r\n", if prefix.ends_with(b"\t") { "\t" } else { "" }, "x".repeat(fill));
            let want = if prefix.starts_with(b"A") { format!("A\r\n{line}") } else { line };
            assert_eq!(exported, want, "{prefix:?}");
        }
    }

    /// Fuzzed: terminal output as a server could send it, through the SSH output path, with
    /// resizes in between (0xF8 'R' cols rows), then export and compaction. Widths start at 2
    /// columns, as the terminal view's do: alacritty can't place a wide character in one. Nothing panics,
    /// the cursor stays on the grid, and the grid keeps the size it was given.
    #[test]
    fn fuzz_terminal_output() {
        let seed = b"\x1b[?1049h\x1b[2J\x1b[H\x1b[31mERROR\x1b[0m e\xcc\x81 \xe6\x97\xa5\xe6\x9c\xac\r\n\x1b]8;;https://example.com\x1b\\link\x1b]8;;\x1b\\\x1b[?2026h\xf8R\x28\x10\x1b[10;5r\x1b[5S\x1b[?2026l\x1b]133;A\x07$ \x1b]133;B\x07ls\r\n\x1b]133;C\x07out\r\n\x1b]133;D;0\x07\x1b[?1049l\x1b[6n\x1b]11;?\x07";
        let dictionary: &[&[u8]] = &[
            b"\xf8R\x01\x01", b"\xf8R\xff\xff", b"\xf8R\x00\x05", b"\xf8", b"\x1b[", b"\x1b]", b"\x07", b"\x1b\\", b"\x1bP", b"\x1b[?2026h", b"\x1b[?2026l",
            b"\x1b[?1049h", b"\x1b[?1049l", b"\x1b[?6h", b"\x1b[r", b"\x1b[999;999H", b"\x1b[4294967295C", b"\x1b[0;0r", b"\x1b[3J", b"\x1b[@", b"\x1b[999P", b"\x1b[999L",
            b"\x1b[999M", b"\x1b[999X", b"\x1b[999b", b"\x1b#8", b"\x1b[?3h", b"\x1b[?7l", b"\x1b[g", b"\x1b[3g", b"\x1bH", b"\t", b"\x1b[999I", b"\x1b[999Z", b"\x1b]8;;x\x1b\\",
            b"\x1b]52;c;aGVsbG8=\x07", b"\x1b]4;1;?\x07", b"\x1b]133;A\x07", b"\x1b]133;D;1\x07", b"\x1b]7;file:///tmp\x07", b"\x1b[38;2;1;2;3m", b"\x1b[38:5:9m", b"\x1b[>c",
            b"\x1b[14t", b"\x1b[18t", b"\x1b[22;0t", b"\x1b[23;0t", b"\x1b%G", b"\x1b(0", b"\x0e", b"\x0f", "\u{1F600}".as_bytes(), "\u{300}".as_bytes(), "\u{fe0f}".as_bytes(),
            "\u{200d}".as_bytes(), "\u{3000}".as_bytes(), b"\r\n", b"\x08", b"\x7f",
        ];
        crate::fuzz::run("terminal output", &[seed], dictionary, |input| {
            let (term, proxy, _rx) = replying_term();
            let mut output = ssh::Output::default();
            for part in input.split(|b| *b == 0xf8) {
                match part {
                    [b'R', cols, rows, rest @ ..] => {
                        let size = GridSize { cols: 2 + u16::from(*cols) % 299, rows: 1 + u16::from(*rows) % 100, cell_width: 9, cell_height: 18 };
                        let cancelled = AtomicBool::new(false);
                        let from = term.lock().columns();
                        prepare_width(&term, from, usize::from(size.cols), &cancelled);
                        proxy.set_size(size);
                        resize(&mut *term.lock(), size);
                        compact_rows(&term, usize::from(size.cols));
                        let t = term.lock();
                        assert_eq!((t.columns(), t.screen_lines()), (usize::from(size.cols), usize::from(size.rows)), "the grid didn't take its size");
                        drop(t);
                        output.feed(&term, &proxy, rest);
                    }
                    _ => output.feed(&term, &proxy, part),
                }
            }
            output.flush(&term, &proxy);
            let t = term.lock();
            let cursor = t.grid().cursor.point;
            assert!(cursor.column.0 < t.columns() && cursor.line.0 >= 0 && (cursor.line.0 as usize) < t.screen_lines(), "the cursor left the grid: {cursor:?}");
            let text = scrollback_text(&*t);
            assert!(!text.contains('\0'));
            let _ = tidy_export(&text);
            drop(t);
            drop_blank_scrollback(&mut *term.lock());
        });
    }
}
