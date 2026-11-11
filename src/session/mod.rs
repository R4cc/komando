//! Sessions: the backend half of a terminal tab.
//!
//! Speed rule: bytes are parsed into the terminal grid on the backend thread (PTY reader
//! thread for Local Sessions, tokio task for SSH Sessions), never on the UI thread. The UI
//! is told "something changed" at most once until it has rendered, so a flood of output
//! costs one repaint per frame, not one per chunk.

pub mod local;
pub mod ssh;

use std::borrow::Cow;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use alacritty_terminal::Term;
use alacritty_terminal::event::{Event, EventListener};
use alacritty_terminal::event_loop::{EventLoopSender, Msg};
use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::index::{Column, Line, Point};
use alacritty_terminal::sync::FairMutex;
use alacritty_terminal::term::Config as TermConfig;
use alacritty_terminal::term::test::TermSize;
use futures::channel::{mpsc, oneshot};

pub type SharedTerm = Arc<FairMutex<Term<EventProxy>>>;

/// Messages from a backend to its tab.
pub enum SessionEvent {
    /// New output was parsed into the grid.
    Wakeup,
    Title(String),
    Status(Status),
    Prompt(Prompt),
    /// Authentication with a typed-in password worked; offer to save it.
    OfferSave { password: String },
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
}

impl EventProxy {
    pub fn new(tx: mpsc::UnboundedSender<SessionEvent>) -> Self {
        Self { tx, dirty: Arc::new(AtomicBool::new(false)), responder: Arc::new(Default::default()) }
    }

    pub fn set_responder(&self, sink: InputSink) {
        self.responder.set(sink);
    }

    /// Called by the UI once it has picked up a Wakeup, re-arming the next one.
    pub fn clear_dirty(&self) {
        self.dirty.store(false, Ordering::Release);
    }

    pub fn send(&self, event: SessionEvent) {
        let _ = self.tx.unbounded_send(event);
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
            Event::PtyWrite(text) => {
                if let Some(sink) = self.responder.get() {
                    sink.write(text.into_bytes());
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
    let config = TermConfig { scrolling_history: scrollback_lines, ..TermConfig::default() };
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
