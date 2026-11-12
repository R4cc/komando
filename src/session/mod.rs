//! Sessions: the backend half of a terminal tab.
//!
//! Speed rule: bytes are parsed into the terminal grid on the backend thread (PTY reader
//! thread for Local Sessions, tokio task for SSH Sessions), never on the UI thread. The UI
//! is told "something changed" at most once until it has rendered, so a flood of output
//! costs one repaint per frame, not one per chunk.

pub mod local;
pub mod ssh;

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
    /// Authentication with a typed-in password worked; offer to save it for that hop's Host.
    OfferSave { password: String, hop: usize },
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
    Input { label: String, secret: bool, error: Option<String>, reply: oneshot::Sender<Option<String>> },
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
        }
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

/// The whole Scrollback plus screen as plain text: colours dropped, trailing blanks trimmed.
pub fn scrollback_text<T>(term: &Term<T>) -> String {
    let grid = term.grid();
    let top = Point::new(Line(-(grid.history_size() as i32)), Column(0));
    let bottom = Point::new(Line(grid.screen_lines() as i32 - 1), grid.last_column());
    tidy_export(&term.bounds_to_string(top, bottom))
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
        assert!(matches!(rx.try_next(), Ok(Some(SessionEvent::Wakeup))));
        assert!(rx.try_next().is_err());
        proxy.clear_dirty();
        proxy.send_event(Event::Wakeup);
        assert!(matches!(rx.try_next(), Ok(Some(SessionEvent::Wakeup))));
    }
}
