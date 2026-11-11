//! Real programs in a real pseudo-terminal, driven the way Komando drives them: the same
//! terminal configuration, the same replies to the program's queries, and keys, mouse and
//! wheel encoded by [`crate::keys`].
//!
//! They need the programs installed, so they are ignored by default. On a Linux development
//! host run them with:
//!
//! ```text
//! cargo test --lib compat -- --ignored --test-threads=1
//! ```
//!
//! A program that is not on the PATH is skipped with a note. `KOMANDO_COMPAT_PATH` is put in
//! front of the PATH, for programs unpacked somewhere else.

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use alacritty_terminal::event_loop::EventLoop;
use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::index::{Column, Line};
use alacritty_terminal::term::TermMode;
use alacritty_terminal::term::cell::Flags;
use alacritty_terminal::tty::{self, Shell};
use futures::channel::mpsc;

use crate::keys::{self, Mods, MouseAction, MouseButton, MouseMode};
use crate::session::{EventProxy, GridSize, InputSink, Prompt, SessionEvent, SharedTerm, new_term};
use crate::theme::TerminalPalette;

const NONE: Mods = Mods { ctrl: false, alt: false, shift: false };
const TIMEOUT: Duration = Duration::from_secs(10);

fn find(program: &str) -> Option<PathBuf> {
    let extra = std::env::var_os("KOMANDO_COMPAT_PATH");
    let path = std::env::var_os("PATH").unwrap_or_default();
    let dirs = extra.iter().flat_map(std::env::split_paths).chain(std::env::split_paths(&path));
    dirs.map(|dir| dir.join(program)).find(|p| p.is_file())
}

fn search_path() -> String {
    let mut dirs: Vec<PathBuf> = std::env::var_os("KOMANDO_COMPAT_PATH").iter().flat_map(std::env::split_paths).collect();
    dirs.extend(std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default()));
    std::env::join_paths(dirs).unwrap().into_string().unwrap()
}

macro_rules! require {
    ($program:expr) => {
        match find($program) {
            Some(path) => path,
            None => {
                eprintln!("skipped: {} is not installed", $program);
                return;
            }
        }
    };
}

/// A program running in a pseudo-terminal of its own.
struct Running {
    term: SharedTerm,
    proxy: EventProxy,
    sink: InputSink,
    events: mpsc::UnboundedReceiver<SessionEvent>,
    size: GridSize,
    /// The session reported that it ended.
    ended: bool,
}

impl Running {
    fn new(size: GridSize) -> (SharedTerm, EventProxy, mpsc::UnboundedReceiver<SessionEvent>) {
        let (tx, events) = mpsc::unbounded();
        let proxy = EventProxy::new(tx);
        proxy.set_palette(&TerminalPalette::dark());
        proxy.set_size(size);
        (new_term(size, 1000, proxy.clone()), proxy, events)
    }

    fn spawn(program: &std::path::Path, args: &[&str], cols: u16, rows: u16) -> Self {
        let size = GridSize { cols, rows, cell_width: 9, cell_height: 18 };
        let (term, proxy, events) = Self::new(size);
        let mut env = HashMap::new();
        env.insert("TERM".to_string(), "xterm-256color".to_string());
        env.insert("COLORTERM".to_string(), "truecolor".to_string());
        env.insert("LANG".to_string(), "C.UTF-8".to_string());
        env.insert("PATH".to_string(), search_path());
        // htop saves its settings on exit; keep them out of the user's own.
        let htoprc = std::env::temp_dir().join(format!("komando-compat-htoprc-{}", std::process::id()));
        env.insert("HTOPRC".to_string(), htoprc.display().to_string());
        let options = tty::Options {
            shell: Some(Shell::new(program.display().to_string(), args.iter().map(|a| a.to_string()).collect())),
            working_directory: None,
            drain_on_exit: true,
            env,
        };
        let pty = tty::new(&options, size.window_size(), 0).expect("pty");
        let event_loop = EventLoop::new(term.clone(), proxy.clone(), pty, true, false).expect("event loop");
        let sink = InputSink::Local(event_loop.channel());
        proxy.set_responder(sink.clone());
        event_loop.spawn();
        Self { term, proxy, sink, events, size, ended: false }
    }

    fn line(&self, row: usize) -> String {
        let term = self.term.lock();
        let grid = term.grid();
        let row = &grid[Line(row as i32)];
        let mut text = String::new();
        for col in 0..grid.columns() {
            let cell = &row[Column(col)];
            if !cell.flags.contains(Flags::WIDE_CHAR_SPACER) {
                text.push(cell.c);
                text.extend(cell.zerowidth().into_iter().flatten());
            }
        }
        text.trim_end().to_string()
    }

    fn screen(&self) -> String {
        (0..self.size.rows as usize).map(|r| self.line(r)).collect::<Vec<_>>().join("\n")
    }

    fn mode(&self) -> TermMode {
        *self.term.lock().mode()
    }

    fn cursor(&self) -> (usize, i32) {
        let term = self.term.lock();
        let point = term.grid().cursor.point;
        (point.column.0, point.line.0)
    }

    fn wait(&mut self, what: &str, done: impl Fn(&Self) -> bool) {
        let start = Instant::now();
        while start.elapsed() < TIMEOUT {
            while let Ok(event) = self.events.try_recv() {
                self.handle(event);
            }
            if done(self) {
                return;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        panic!("timed out waiting for {what}. Screen:\n{}", self.screen());
    }

    fn wait_text(&mut self, text: &str) {
        let text = text.to_string();
        self.wait(&format!("{text:?}"), move |r| r.screen().contains(&text));
    }

    /// Answer what a session asks the user, as a person would.
    fn handle(&mut self, event: SessionEvent) {
        match event {
            SessionEvent::Prompt(Prompt::HostKey { reply, .. }) => {
                let _ = reply.send(true);
            }
            SessionEvent::Status(crate::session::Status::Disconnected(_)) => self.ended = true,
            _ => {}
        }
    }

    fn settle(&mut self) {
        std::thread::sleep(Duration::from_millis(300));
        while let Ok(event) = self.events.try_recv() {
            self.handle(event);
        }
    }

    fn write(&self, bytes: impl Into<Vec<u8>>) {
        self.sink.write(bytes.into());
    }

    fn key(&self, key: &str, mods: Mods) {
        let app_cursor = self.mode().contains(TermMode::APP_CURSOR);
        let bytes = keys::to_bytes(key, None, mods, app_cursor).unwrap_or_else(|| panic!("no bytes for {key}"));
        self.write(bytes);
    }

    fn text(&self, text: &str) {
        for c in text.chars() {
            let s = c.to_string();
            self.write(keys::to_bytes(&s, Some(&s), NONE, false).unwrap());
        }
    }

    fn mouse(&self, button: MouseButton, action: MouseAction, col: usize, row: usize) {
        let mode = MouseMode::of(self.mode());
        assert!(mode.reporting(), "the program did not ask for the mouse");
        if let Some(bytes) = keys::mouse_report(mode, button, action, col, row, NONE) {
            self.write(bytes);
        }
    }

    fn click(&self, col: usize, row: usize) {
        self.mouse(MouseButton::Left, MouseAction::Press, col, row);
        self.mouse(MouseButton::Left, MouseAction::Release, col, row);
    }

    fn resize(&mut self, cols: u16, rows: u16) {
        self.size = GridSize { cols, rows, ..self.size };
        self.term.lock().resize(self.size.term_size());
        self.proxy.set_size(self.size);
        self.sink.resize(self.size);
    }
}

impl Drop for Running {
    fn drop(&mut self) {
        self.sink.close();
    }
}

#[test]
#[ignore]
fn compat_vim_editing_mouse_resize_and_unicode() {
    let vim = require!("vim").to_owned();
    let mut r = Running::spawn(&vim, &["-u", "NONE", "-i", "NONE", "-N", "-n", "--cmd", "set mouse=a"], 80, 24);
    r.wait("the alternate screen", |r| r.mode().contains(TermMode::ALT_SCREEN) && r.line(1).starts_with('~'));
    r.settle();

    // Typing, with UTF-8, a wide character and a combining accent.
    r.key("i", NONE);
    r.text("hello 日本 e\u{301}!");
    r.key("escape", NONE);
    r.wait_text("hello 日本 e\u{301}!");
    {
        let term = r.term.lock();
        let row = &term.grid()[Line(0)];
        assert!(row[Column(6)].flags.contains(Flags::WIDE_CHAR), "日 takes two columns");
        assert!(row[Column(7)].flags.contains(Flags::WIDE_CHAR_SPACER));
        assert_eq!(row[Column(11)].c, 'e');
        assert_eq!(row[Column(11)].zerowidth(), Some(&['\u{301}'][..]), "the accent stays on its letter");
    }

    // Cursor keys in vim's application mode: from the "!" at the end of the line (column
    // 12, after two double-width characters), two to the left lands on the space.
    r.text("$");
    r.wait("the cursor at the end", |r| r.cursor() == (12, 0));
    r.key("left", NONE);
    r.key("left", NONE);
    r.wait("the cursor to move left", |r| r.cursor() == (10, 0));

    // The mouse: vim asked for it, and a click moves the cursor there.
    r.wait("mouse reporting", |r| MouseMode::of(r.mode()).reporting());
    r.click(2, 0);
    r.wait("the cursor at the click", |r| r.cursor() == (2, 0));

    // The wheel goes to vim as wheel buttons; it does not scroll Komando's history.
    r.key("escape", NONE);
    r.text(":for i in range(200) | call append(line('$'), 'line ' . i) | endfor\r");
    r.wait_text("line 20");
    r.mouse(MouseButton::WheelDown, MouseAction::Press, 10, 10);
    r.wait("vim to scroll", |r| !r.line(0).starts_with("hello"));

    // A resize reaches vim, which redraws for the new size.
    r.resize(100, 30);
    r.wait("vim to redraw for 30 rows", |r| r.line(28).starts_with("line"));
    r.text(":echo &columns . 'x' . &lines\r");
    r.wait_text("100x30");

    // Leaving restores the shell's screen.
    r.text(":qa!\r");
    r.wait("the primary screen", |r| !r.mode().contains(TermMode::ALT_SCREEN));
}

#[test]
#[ignore]
fn compat_vim_reads_the_background_colour() {
    let vim = require!("vim").to_owned();
    let mut r = Running::spawn(&vim, &["-u", "NONE", "-i", "NONE", "-N", "-n"], 80, 24);
    r.wait("vim", |r| r.line(1).starts_with('~'));
    // vim asks with OSC 11 at startup (t_RB) and sets 'background' from the answer.
    r.settle();
    r.text(":echo 'bg=' . &background\r");
    r.wait_text("bg=dark");
    r.text(":qa!\r");
}

#[test]
#[ignore]
fn compat_htop_draws_and_quits_with_a_mouse_click() {
    let htop = require!("htop").to_owned();
    let mut r = Running::spawn(&htop, &["--no-color"], 120, 30);
    r.wait_text("PID");
    r.wait("htop to ask for the mouse", |r| MouseMode::of(r.mode()).reporting());
    // The function bar at the bottom: clicking "Quit" ends htop.
    let bottom = r.size.rows as usize - 1;
    let col = r.line(bottom).find("Quit").expect("a Quit button");
    r.click(col, bottom);
    r.wait("htop to exit", |r| !r.mode().contains(TermMode::ALT_SCREEN));
}

#[test]
#[ignore]
fn compat_htop_function_keys_and_resize() {
    let htop = require!("htop").to_owned();
    let mut r = Running::spawn(&htop, &[], 120, 30);
    r.wait_text("PID");
    // F1: help screen; any key back; resizing redraws the meters at the new width.
    r.key("f1", NONE);
    r.wait_text("htop 3");
    r.text("q");
    r.wait_text("PID");
    r.resize(90, 25);
    r.wait("htop's function bar on the new last row", |r| r.line(24).contains("Quit"));
    r.key("f10", NONE);
    r.wait("htop to exit", |r| !r.mode().contains(TermMode::ALT_SCREEN));
}

#[test]
#[ignore]
fn compat_tmux_status_resize_and_focus() {
    let tmux = require!("tmux").to_owned();
    let socket = format!("komando-compat-{}", std::process::id());
    let args = [
        "-L", &socket, "-f", "/dev/null", "new-session", ";", "set", "-g", "mouse", "on", ";", "set", "-g",
        "focus-events", "on",
    ];
    let mut r = Running::spawn(&tmux, &args, 100, 30);
    r.wait("tmux's status line", |r| r.line(29).contains("[komando") || r.line(29).contains("0:"));
    r.wait("focus reporting", |r| r.mode().contains(TermMode::FOCUS_IN_OUT));
    r.write(keys::focus_bytes(true).to_vec());
    r.settle();

    // Splits are drawn with box-drawing lines.
    r.key("b", Mods { ctrl: true, ..NONE });
    r.text("%");
    r.wait("a vertical split", |r| r.line(5).contains('│'));

    // Resize: tmux moves its status line to the new last row.
    r.resize(80, 20);
    r.wait("the status line on row 20", |r| r.line(19).contains("0:"));

    // Mouse: tmux asked for it. The new right pane (%1) is active; clicking the left one
    // (%0) makes it active, so the command runs there.
    r.wait("mouse reporting", |r| MouseMode::of(r.mode()).reporting());
    r.click(2, 2);
    r.settle();
    r.text("echo \"pane=$TMUX_PANE\"\r");
    r.wait("the command in the left pane", |r| (0..19).any(|i| r.line(i).starts_with("pane=%0")));

    let _ = std::process::Command::new(&tmux).args(["-L", &socket, "kill-server"]).status();
}

#[test]
#[ignore]
fn compat_less_scrolls_with_the_wheel() {
    let less = require!("less").to_owned();
    let file = std::env::temp_dir().join(format!("komando-compat-{}.txt", std::process::id()));
    std::fs::write(&file, (1..=500).map(|i| format!("row {i}\n")).collect::<String>()).unwrap();
    let mut r = Running::spawn(&less, &[file.to_str().unwrap()], 80, 24);
    r.wait_text("row 1");
    assert!(r.mode().contains(TermMode::ALT_SCREEN));
    // less did not ask for the mouse: the wheel becomes cursor keys (alternate scroll).
    assert!(!MouseMode::of(r.mode()).reporting());
    assert!(r.mode().contains(TermMode::ALTERNATE_SCROLL));
    r.write(keys::alternate_scroll_bytes(-3, r.mode().contains(TermMode::APP_CURSOR)));
    r.wait("three rows down", |r| r.line(0) == "row 4");
    r.write(keys::alternate_scroll_bytes(2, r.mode().contains(TermMode::APP_CURSOR)));
    r.wait("two rows back up", |r| r.line(0) == "row 2");
    r.text("q");
    r.wait("less to exit", |r| !r.mode().contains(TermMode::ALT_SCREEN));
    let _ = std::fs::remove_file(file);
}

#[test]
#[ignore]
fn compat_shell_unicode_colours_and_queries() {
    let sh = require!("bash").to_owned();
    let mut r = Running::spawn(&sh, &["--norc", "--noprofile", "-i"], 80, 24);
    r.settle();
    // Wide, combining, emoji, box drawing, 24-bit colour.
    r.text("printf '\\e[38;2;255;100;0m日本語 é ✓ 🙂 ┌─┐\\e[0m\\n'\r");
    r.wait_text("日本語 é ✓ 🙂 ┌─┐");
    // The size a program sees (stty asks the kernel; tput asks terminfo + ioctl).
    r.text("stty size\r");
    r.wait_text("24 80");
    r.resize(91, 31);
    r.text("stty size\r");
    r.wait_text("31 91");
}

/// The SSH path end to end against a throwaway sshd on this machine: host key prompt,
/// key authentication, the PTY modes and size the server sees, UTF-8 and resizing.
#[test]
#[ignore]
fn compat_ssh_session_against_a_local_sshd() {
    let sshd = require!("sshd").to_owned();
    let keygen = require!("ssh-keygen").to_owned();
    let dir = std::env::temp_dir().join(format!("komando-compat-ssh-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    for name in ["host", "user"] {
        let ok = std::process::Command::new(&keygen)
            .args(["-q", "-t", "ed25519", "-N", "", "-f"])
            .arg(dir.join(name))
            .status()
            .unwrap()
            .success();
        assert!(ok);
    }
    std::fs::copy(dir.join("user.pub"), dir.join("authorized_keys")).unwrap();
    let port = 20000 + (std::process::id() % 20000) as u16;
    let config = format!(
        "Port {port}\nListenAddress 127.0.0.1\nHostKey {host}\nAuthorizedKeysFile {keys}\nPidFile {pid}\n\
         UsePAM no\nStrictModes no\nPasswordAuthentication no\nPrintMotd no\nPrintLastLog no\n",
        host = dir.join("host").display(),
        keys = dir.join("authorized_keys").display(),
        pid = dir.join("sshd.pid").display(),
    );
    std::fs::write(dir.join("sshd_config"), config).unwrap();
    let mut server = std::process::Command::new(&sshd)
        .args(["-D", "-e", "-f"])
        .arg(dir.join("sshd_config"))
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();
    std::thread::sleep(Duration::from_millis(500));

    let size = GridSize { cols: 100, rows: 30, cell_width: 9, cell_height: 18 };
    let (term, proxy, events) = Running::new(size);
    let target = crate::session::ssh::Target {
        address: "127.0.0.1".into(),
        port,
        user: Some(std::env::var("USER").or_else(|_| std::env::var("LOGNAME")).unwrap_or_else(|_| {
            let out = std::process::Command::new("whoami").output().unwrap();
            String::from_utf8(out.stdout).unwrap().trim().to_string()
        })),
        secret: Some(crate::vault::Secret::PrivateKey {
            key: std::fs::read_to_string(dir.join("user")).unwrap(),
            passphrase: None,
        }),
        known_hosts: dir.join("known_hosts"),
    };
    let sink = crate::session::ssh::spawn(target, size, term.clone(), proxy.clone());
    let mut r = Running { term, proxy, sink, events, size, ended: false };
    r.settle();
    r.text("stty -a | tr ' ;' '\\n\\n' | grep -xE -- '-?iutf8' ; stty size; echo \"T=$TERM\"\r");
    r.wait_text("T=xterm-256color");
    let screen = r.screen();
    assert!(screen.lines().any(|l| l == "iutf8"), "IUTF8 is on:\n{screen}");
    assert!(screen.lines().any(|l| l == "30 100"), "the PTY has our size:\n{screen}");
    r.text("clear; printf '日本 é\\n'\r");
    r.wait_text("日本 é");
    r.resize(120, 40);
    r.settle();
    r.text("stty size\r");
    r.wait_text("40 120");
    r.text("exit\r");
    r.wait("the session to end", |r| r.ended);

    let _ = server.kill();
    let _ = std::fs::remove_dir_all(&dir);
}
