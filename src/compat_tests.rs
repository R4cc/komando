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
use crate::session::{EventProxy, GridSize, HopStep, InputSink, Prompt, SessionEvent, SharedTerm, new_term};
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
    /// Progress reported on the way to an SSH host, in order.
    hops: Vec<(usize, HopStep)>,
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
        Self { term, proxy, sink, events, size, ended: false, hops: vec![] }
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
            SessionEvent::Hop { index, step } => self.hops.push((index, step)),
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

/// A throwaway sshd on this machine that accepts one generated key.
struct LocalSshd {
    dir: PathBuf,
    port: u16,
    server: std::process::Child,
}

impl LocalSshd {
    /// `None` (with a note) when sshd or ssh-keygen is not installed.
    fn start(name: &str, port_offset: u16) -> Option<Self> {
        let (Some(sshd), Some(keygen)) = (find("sshd"), find("ssh-keygen")) else {
            eprintln!("skipped: sshd or ssh-keygen is not installed");
            return None;
        };
        let dir = std::env::temp_dir().join(format!("komando-compat-{name}-{}", std::process::id()));
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
        let port = 20000 + port_offset + (std::process::id() % 20000) as u16;
        let config = format!(
            "Port {port}\nListenAddress 127.0.0.1\nHostKey {host}\nAuthorizedKeysFile {keys}\nPidFile {pid}\n\
             UsePAM no\nStrictModes no\nPasswordAuthentication no\nPrintMotd no\nPrintLastLog no\nAllowTcpForwarding yes\nMaxStartups 100\n\
             Subsystem sftp internal-sftp\n",
            host = dir.join("host").display(),
            keys = dir.join("authorized_keys").display(),
            pid = dir.join("sshd.pid").display(),
        );
        std::fs::write(dir.join("sshd_config"), config).unwrap();
        let server = std::process::Command::new(&sshd)
            .args(["-D", "-e", "-f"])
            .arg(dir.join("sshd_config"))
            .stderr(std::process::Stdio::null())
            .spawn()
            .unwrap();
        std::thread::sleep(Duration::from_millis(500));
        Some(Self { dir, port, server })
    }

    /// Signing in to this server as the current user with the generated key.
    fn hop(&self, name: &str, address: &str) -> crate::session::ssh::Hop {
        crate::session::ssh::Hop {
            name: name.into(),
            address: address.into(),
            port: self.port,
            user: Some(std::env::var("USER").or_else(|_| std::env::var("LOGNAME")).unwrap_or_else(|_| {
                let out = std::process::Command::new("whoami").output().unwrap();
                String::from_utf8(out.stdout).unwrap().trim().to_string()
            })),
            secret: Some(crate::vault::Secret::PrivateKey {
                key: std::fs::read_to_string(self.dir.join("user")).unwrap(),
                passphrase: None,
            }),
        }
    }

    fn connect(&self, hops: Vec<crate::session::ssh::Hop>, size: GridSize) -> Running {
        let (term, proxy, events) = Running::new(size);
        let target = crate::session::ssh::Target { hops, known_hosts: self.dir.join("known_hosts") };
        let sink = crate::session::ssh::spawn(target, size, term.clone(), proxy.clone());
        Running { term, proxy, sink, events, size, ended: false, hops: vec![] }
    }
}

impl Drop for LocalSshd {
    fn drop(&mut self) {
        let _ = self.server.kill();
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// The SSH path end to end against a throwaway sshd on this machine: host key prompt,
/// key authentication, the PTY modes and size the server sees, UTF-8 and resizing.
#[test]
#[ignore]
fn compat_ssh_session_against_a_local_sshd() {
    let Some(sshd) = LocalSshd::start("ssh", 0) else { return };
    let size = GridSize { cols: 100, rows: 30, cell_width: 9, cell_height: 18 };
    let mut r = sshd.connect(vec![sshd.hop("local", "127.0.0.1")], size);
    r.settle();
    // The connection log sits above the shell.
    r.wait_text("Connecting to local (127.0.0.1:");
    r.wait_text("Signed in to local as");
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
}

/// A Host reached through a Jump Host: Komando signs in to the Jump Host, tunnels to the
/// Host through it, and signs in again there. Both hops are the same local sshd, the second
/// reached through a tunnel opened by the first.
#[test]
#[ignore]
fn compat_ssh_session_through_a_jump_host() {
    let Some(sshd) = LocalSshd::start("jump", 1) else { return };
    let size = GridSize { cols: 100, rows: 30, cell_width: 9, cell_height: 18 };
    let mut r = sshd.connect(vec![sshd.hop("bastion", "127.0.0.1"), sshd.hop("web01", "127.0.0.1")], size);
    r.settle();
    r.text("echo \"via=$SSH_CONNECTION\"\r");
    r.wait_text("via=");
    let done: Vec<usize> = r.hops.iter().filter(|(_, step)| *step == HopStep::Done).map(|(i, _)| *i).collect();
    assert_eq!(done, [0, 1], "both hops signed in, in order: {:?}", r.hops);
    assert_eq!(r.hops.first(), Some(&(0, HopStep::Reaching)));
    r.text("exit\r");
    r.wait("the session to end", |r| r.ended);
}

/// The SFTP browser's backend against a throwaway sshd: SFTP opens on the terminal's own
/// connection, permissions are read for the signed-in user, and transfers never leave a
/// misleading file behind.
#[test]
#[ignore]
fn compat_sftp_on_the_sessions_connection() {
    use crate::sftp::{self, ErrorKind, PARTIAL_SUFFIX, Progress};
    use std::os::unix::fs::PermissionsExt;
    use std::sync::Arc;

    let Some(sshd) = LocalSshd::start("sftp", 2) else { return };
    let size = GridSize { cols: 100, rows: 30, cell_width: 9, cell_height: 18 };
    let mut r = sshd.connect(vec![sshd.hop("local", "127.0.0.1")], size);
    r.settle();
    r.text("echo sftp-ready\r");
    r.wait_text("sftp-ready");
    let answer = r.sink.open_sftp().expect("an SSH session opens SFTP");
    let remote = Arc::new(futures::executor::block_on(answer).expect("reply").expect("SFTP starts"));
    let root = remote.identity.uid == Some(0);
    assert!(remote.identity.uid.is_some(), "id ran on the server");
    let run = |f: futures::future::BoxFuture<'static, ()>| futures::executor::block_on(sftp::spawn(f)).unwrap();

    // Files on "the server" (this machine).
    let base = sshd.dir.join("files");
    std::fs::create_dir_all(base.join("logs")).unwrap();
    std::fs::create_dir_all(base.join("storage")).unwrap();
    std::fs::write(base.join("app.conf"), b"listen 80;\n").unwrap();
    std::fs::write(base.join("empty.txt"), b"").unwrap();
    std::fs::write(base.join("secret.log"), b"top secret").unwrap();
    std::fs::write(base.join("logs/a.log"), vec![b'x'; 300_000]).unwrap();
    std::fs::write(base.join("storage/app.log"), b"hidden").unwrap();
    std::fs::set_permissions(base.join("secret.log"), std::fs::Permissions::from_mode(0o000)).unwrap();
    std::fs::set_permissions(base.join("storage"), std::fs::Permissions::from_mode(0o000)).unwrap();
    let base_s = base.display().to_string();
    let local = sshd.dir.join("downloads");
    std::fs::create_dir_all(&local).unwrap();

    let (rm, b, l) = (remote.clone(), base_s.clone(), local.clone());
    run(Box::pin(async move {
        let listing = rm.list(&b).await.unwrap();
        let get = |n: &str| listing.entries.iter().find(|e| e.name == n).unwrap().clone();
        assert!(get("logs").is_dir() && !get("logs").locked());
        assert!(!get("app.conf").locked());
        if !root {
            assert!(get("secret.log").locked(), "{:?}", get("secret.log"));
            assert!(get("storage").locked());
        }
        let p = Progress::default();

        // A file the user can't read: an error, and nothing on disk.
        if !root {
            let err = rm.download(&format!("{b}/secret.log"), &l, &p).await.unwrap_err();
            assert_eq!(err.kind, ErrorKind::PermissionDenied, "{err}");
            let err = rm.download(&format!("{b}/storage/app.log"), &l, &p).await.unwrap_err();
            assert_eq!(err.kind, ErrorKind::PermissionDenied, "{err}");
            assert_eq!(std::fs::read_dir(&l).unwrap().count(), 0, "no file left behind");
        }

        // A genuinely empty file downloads, as an empty file.
        let empty = rm.download(&format!("{b}/empty.txt"), &l, &p).await.unwrap();
        assert_eq!(std::fs::metadata(&empty).unwrap().len(), 0);

        // A second download doesn't overwrite the first.
        let first = rm.download(&format!("{b}/app.conf"), &l, &p).await.unwrap();
        let second = rm.download(&format!("{b}/app.conf"), &l, &p).await.unwrap();
        assert_eq!(first.file_name().unwrap(), "app.conf");
        assert_eq!(second.file_name().unwrap(), "app (1).conf");
        assert_eq!(std::fs::read(&second).unwrap(), b"listen 80;\n");

        // Folders, with progress.
        let p = Progress::default();
        let logs = rm.download(&format!("{b}/logs"), &l, &p).await.unwrap();
        assert_eq!(std::fs::metadata(logs.join("a.log")).unwrap().len(), 300_000);
        assert_eq!(p.done.load(std::sync::atomic::Ordering::Relaxed), 300_000);

        // Uploads: a name that's taken fails unless replacing.
        let up = l.join("app (1).conf");
        std::fs::write(&up, b"listen 8080;\n").unwrap();
        let err = rm.upload(&up, &b, "app.conf", false, &p).await.unwrap_err();
        assert_eq!(err.kind, ErrorKind::Exists);
        rm.upload(&up, &b, "app.conf", true, &p).await.unwrap();
        assert_eq!(std::fs::read(format!("{b}/app.conf")).unwrap(), b"listen 8080;\n");
        rm.upload(&logs, &b, "logs-copy", false, &p).await.unwrap();
        assert!(std::path::Path::new(&format!("{b}/logs-copy/a.log")).exists());

        // Folder operations.
        rm.mkdir(&format!("{b}/new")).await.unwrap();
        assert_eq!(rm.mkdir(&format!("{b}/new")).await.unwrap_err().kind, ErrorKind::Exists);
        rm.rename(&format!("{b}/new"), &format!("{b}/renamed")).await.unwrap();
        rm.copy(&format!("{b}/logs"), &format!("{b}/renamed/logs")).await.unwrap();
        assert!(std::path::Path::new(&format!("{b}/renamed/logs/a.log")).exists());
        rm.remove(&format!("{b}/renamed")).await.unwrap();
        assert!(!rm.exists(&format!("{b}/renamed")).await.unwrap());
        assert!(rm.list(&format!("{b}/nope")).await.unwrap_err().kind == ErrorKind::NotFound);
    }));

    // Nothing half-transferred anywhere.
    for dir in [&base, &local] {
        for entry in std::fs::read_dir(dir).unwrap() {
            let name = entry.unwrap().file_name().to_string_lossy().into_owned();
            assert!(!name.ends_with(PARTIAL_SUFFIX), "left behind: {name}");
        }
    }
    std::fs::set_permissions(base.join("storage"), std::fs::Permissions::from_mode(0o755)).unwrap();
    r.text("exit\r");
    r.wait("the session to end", |r| r.ended);
}

/// A TCP forwarder that delays every byte by `one_way` in each direction, like a distant
/// server, and with `bytes_per_sec` caps each direction's bandwidth, like a slow link.
/// Loopback has neither, which hides costs paid per round trip and queues that build up.
struct DelayProxy {
    port: u16,
}

impl DelayProxy {
    fn start(target_port: u16, one_way: Duration, bytes_per_sec: Option<u64>) -> Self {
        use std::net::{TcpListener, TcpStream};
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        fn pump(mut from: TcpStream, mut to: TcpStream, delay: Duration, rate: Option<u64>) {
            use std::io::{Read, Write};
            let (tx, rx) = std::sync::mpsc::channel::<(Instant, Vec<u8>)>();
            std::thread::spawn(move || {
                let mut buf = vec![0u8; 64 * 1024];
                loop {
                    match from.read(&mut buf) {
                        Ok(0) | Err(_) => break,
                        Ok(n) => {
                            if tx.send((Instant::now() + delay, buf[..n].to_vec())).is_err() {
                                break;
                            }
                        }
                    }
                }
            });
            std::thread::spawn(move || {
                let mut free_at = Instant::now();
                while let Ok((mut at, bytes)) = rx.recv() {
                    if let Some(rate) = rate {
                        // The link sends one thing at a time, at `rate`.
                        free_at = at.max(free_at) + Duration::from_secs_f64(bytes.len() as f64 / rate as f64);
                        at = free_at;
                    }
                    if let Some(wait) = at.checked_duration_since(Instant::now()) {
                        std::thread::sleep(wait);
                    }
                    if to.write_all(&bytes).is_err() {
                        break;
                    }
                }
                let _ = to.shutdown(std::net::Shutdown::Write);
            });
        }
        std::thread::spawn(move || {
            for client in listener.incoming().flatten() {
                let server = TcpStream::connect(("127.0.0.1", target_port)).unwrap();
                client.set_nodelay(true).unwrap();
                server.set_nodelay(true).unwrap();
                pump(client.try_clone().unwrap(), server.try_clone().unwrap(), one_way, bytes_per_sec);
                pump(server, client, one_way, bytes_per_sec);
            }
        });
        Self { port }
    }
}

/// Deterministic bytes that don't compress, so SSH compression can't flatter the numbers.
fn noise(len: usize, seed: u64) -> Vec<u8> {
    let mut x = seed | 1;
    let mut out = Vec::with_capacity(len + 8);
    while out.len() < len {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        out.extend_from_slice(&x.to_le_bytes());
    }
    out.truncate(len);
    out
}

/// Whether two files hold the same bytes, compared a piece at a time (they can be large).
fn same_file(a: &std::path::Path, b: &std::path::Path) -> bool {
    use std::io::Read;
    let (mut x, mut y) = (std::fs::File::open(a).unwrap(), std::fs::File::open(b).unwrap());
    if x.metadata().unwrap().len() != y.metadata().unwrap().len() {
        return false;
    }
    let (mut bx, mut by) = (vec![0u8; 1 << 20], vec![0u8; 1 << 20]);
    loop {
        let n = x.read(&mut bx).unwrap();
        if n == 0 {
            return true;
        }
        y.read_exact(&mut by[..n]).unwrap();
        if bx[..n] != by[..n] {
            return false;
        }
    }
}

/// Every file under `a` exists under `b` with the same bytes, and nothing else is there.
fn assert_same_tree(a: &std::path::Path, b: &std::path::Path) {
    fn walk(root: &std::path::Path, rel: &std::path::Path, out: &mut Vec<PathBuf>) {
        for entry in std::fs::read_dir(root.join(rel)).unwrap() {
            let entry = entry.unwrap();
            let child = rel.join(entry.file_name());
            out.push(child.clone());
            if entry.file_type().unwrap().is_dir() {
                walk(root, &child, out);
            }
        }
    }
    let (mut left, mut right) = (vec![], vec![]);
    walk(a, std::path::Path::new(""), &mut left);
    walk(b, std::path::Path::new(""), &mut right);
    left.sort();
    right.sort();
    assert_eq!(left, right, "the same entries under {} and {}", a.display(), b.display());
    for rel in left {
        let (x, y) = (a.join(&rel), b.join(&rel));
        if x.is_file() {
            assert!(same_file(&x, &y), "same bytes: {}", rel.display());
        }
    }
}

/// SFTP transfers under load against a throwaway sshd, directly, through a link with latency,
/// and through a slow link with latency: a large file each way, a folder of many small files
/// each way, deleting a large folder, several transfers at once on one connection, and
/// cancelling. Prints timings; checks every byte.
///
/// `KOMANDO_STRESS_MB` sets the large file's size (default 128; 8 on the slow link),
/// `KOMANDO_STRESS_LATENCY_MS` the one-way delay (default 25) and `KOMANDO_STRESS_SLOW_KBPS`
/// the slow link's speed in KiB/s (default 1024; 0 skips it).
#[test]
#[ignore]
fn compat_sftp_stress() {
    use crate::sftp::{self, ErrorKind, PARTIAL_SUFFIX, Progress, Remote};
    use std::sync::Arc;
    use std::sync::atomic::Ordering;

    let Some(sshd) = LocalSshd::start("sftp-stress", 3) else { return };
    let mb: usize = std::env::var("KOMANDO_STRESS_MB").ok().and_then(|v| v.parse().ok()).unwrap_or(128);
    let latency_ms: u64 = std::env::var("KOMANDO_STRESS_LATENCY_MS").ok().and_then(|v| v.parse().ok()).unwrap_or(25);
    let slow_kbps: u64 = std::env::var("KOMANDO_STRESS_SLOW_KBPS").ok().and_then(|v| v.parse().ok()).unwrap_or(1024);

    // The server side: a large file (a smaller one for the slow link) and a tree of small ones.
    let base = sshd.dir.join("server");
    std::fs::create_dir_all(&base).unwrap();
    {
        use std::io::Write;
        let mut big = std::io::BufWriter::new(std::fs::File::create(base.join("big.bin")).unwrap());
        for i in 0..mb {
            big.write_all(&noise(1024 * 1024, 7 + i as u64)).unwrap();
        }
    }
    std::fs::write(base.join("big-slow.bin"), noise(8 * 1024 * 1024, 9)).unwrap();
    let small = base.join("small");
    let mut small_bytes = 0u64;
    for d in 0..20 {
        let dir = small.join(format!("dir{d:02}")).join("nested");
        std::fs::create_dir_all(&dir).unwrap();
        for f in 0..50 {
            let len = (d * 50 + f) * 37 % 8192;
            small_bytes += len as u64;
            std::fs::write(dir.join(format!("file{f:03}.txt")), noise(len, (d * 50 + f) as u64 + 1)).unwrap();
        }
    }
    std::fs::create_dir_all(small.join("empty-dir")).unwrap();
    std::fs::write(small.join("empty.txt"), b"").unwrap();

    let run = |f: futures::future::BoxFuture<'static, ()>| futures::executor::block_on(sftp::spawn(f)).unwrap();
    let rate = |bytes: u64, d: Duration| format!("{:.1} MB/s", bytes as f64 / 1048576. / d.as_secs_f64());

    let latency = Duration::from_millis(latency_ms);
    let mut links = vec![(Duration::ZERO, None), (latency, None)];
    if slow_kbps > 0 {
        links.push((latency, Some(slow_kbps * 1024)));
    }
    for (round, (one_way, bandwidth)) in links.into_iter().enumerate() {
        let label = match bandwidth {
            Some(bps) => format!("{}ms one-way, {} KiB/s", one_way.as_millis(), bps / 1024),
            None => format!("{}ms one-way", one_way.as_millis()),
        };
        let mb = if bandwidth.is_some() { 8 } else { mb };
        let big = base.join(if bandwidth.is_some() { "big-slow.bin" } else { "big.bin" });
        let proxy = (!one_way.is_zero()).then(|| DelayProxy::start(sshd.port, one_way, bandwidth));
        let size = GridSize { cols: 100, rows: 30, cell_width: 9, cell_height: 18 };
        let mut hop = sshd.hop("local", "127.0.0.1");
        if let Some(proxy) = &proxy {
            hop.port = proxy.port;
        }
        let mut r = sshd.connect(vec![hop], size);
        r.settle();
        r.text("echo stress-ready\r");
        r.wait_text("stress-ready");
        let answer = r.sink.open_sftp().expect("an SSH session opens SFTP");
        let remote: Arc<Remote> = Arc::new(futures::executor::block_on(answer).expect("reply").expect("SFTP starts"));

        let local = sshd.dir.join(format!("local-{round}"));
        std::fs::create_dir_all(&local).unwrap();
        let upload_dir = base.join(format!("up-{round}"));
        std::fs::create_dir_all(&upload_dir).unwrap();
        let (big_s, small_s, up_s) =
            (big.display().to_string(), small.display().to_string(), upload_dir.display().to_string());
        let big_len = (mb * 1024 * 1024) as u64;

        // One large file down and back up.
        let (rm, l, b, u, lb) = (remote.clone(), local.clone(), big_s.clone(), up_s.clone(), label.clone());
        let big_copy = big.clone();
        run(Box::pin(async move {
            let p = Progress::default();
            let t = Instant::now();
            let got = rm.download(&b, &l, &p).await.unwrap();
            let d = t.elapsed();
            assert_eq!(got.file_name().unwrap(), big_copy.file_name().unwrap());
            eprintln!("[{lb}] download {mb} MB: {:?} ({})", d, rate(big_len, d));
            assert_eq!(p.done.load(Ordering::Relaxed), big_len);
            assert!(same_file(&got, &big_copy), "download is byte-identical");
            let p = Progress::default();
            let t = Instant::now();
            let sent = rm.upload(&got, &u, "big.bin", false, &p).await.unwrap();
            assert_eq!(p.done.load(Ordering::Relaxed), big_len);
            let d = t.elapsed();
            eprintln!("[{lb}] upload {mb} MB: {:?} ({})", d, rate(big_len, d));
            assert!(same_file(std::path::Path::new(&sent), &big_copy), "upload is byte-identical");
        }));

        // Many small files down and back up.
        let (rm, l, s, u, lb) = (remote.clone(), local.clone(), small_s.clone(), up_s.clone(), label.clone());
        let small_copy = small.clone();
        let up_small = upload_dir.join("small");
        run(Box::pin(async move {
            let p = Progress::default();
            let t = Instant::now();
            let got = rm.download(&s, &l, &p).await.unwrap();
            let d = t.elapsed();
            eprintln!("[{lb}] download 1001 small files ({} KB): {:?} ({:.0} files/s)", small_bytes / 1024, d, 1001. / d.as_secs_f64());
            assert_eq!(p.done.load(Ordering::Relaxed), small_bytes);
            assert_same_tree(&small_copy, &got);
            let p = Progress::default();
            let t = Instant::now();
            rm.upload(&got, &u, "small", false, &p).await.unwrap();
            let d = t.elapsed();
            eprintln!("[{lb}] upload 1001 small files: {:?} ({:.0} files/s)", d, 1001. / d.as_secs_f64());
            assert_eq!(p.done.load(Ordering::Relaxed), small_bytes);
            assert_same_tree(&small_copy, &up_small);
            let t = Instant::now();
            let up_small_s = up_small.display().to_string();
            rm.remove(&up_small_s).await.unwrap();
            eprintln!("[{lb}] delete the uploaded folder (1001 files, 41 folders): {:?}", t.elapsed());
            assert!(!up_small.exists());

            // A second upload to where one is running is refused, and doesn't touch the first.
            let (first, second) = (Progress::default(), Progress::default());
            let (a, b) = futures::future::join(
                rm.upload(&got, &u, "twice", false, &first),
                rm.upload(&got, &u, "twice", false, &second),
            )
            .await;
            let (won, lost) = if a.is_ok() { (a, b) } else { (b, a) };
            assert!(won.is_ok(), "{won:?}");
            assert_eq!(lost.unwrap_err().kind, ErrorKind::Exists);
            assert_same_tree(&small_copy, &std::path::Path::new(&u).join("twice"));
        }));

        // Several transfers at once on the one connection, as the Downloads panel runs them.
        let (rm, l, b, s, lb) = (remote.clone(), local.join("parallel"), big_s.clone(), small_s.clone(), label.clone());
        let (big_copy, small_copy) = (big.clone(), small.clone());
        run(Box::pin(async move {
            let t = Instant::now();
            let jobs = (0..4).map(|i| {
                let (rm, l, path) = (rm.clone(), l.clone(), if i % 2 == 0 { b.clone() } else { s.clone() });
                async move { rm.download(&path, &l, &Progress::default()).await }
            });
            let results = futures::future::join_all(jobs).await;
            eprintln!("[{lb}] 2 large + 2 folder downloads at once: {:?}", t.elapsed());
            for (i, result) in results.into_iter().enumerate() {
                let got = result.unwrap();
                if i % 2 == 0 {
                    assert!(same_file(&got, &big_copy));
                } else {
                    assert_same_tree(&small_copy, &got);
                }
            }
        }));

        // Every small file as a download of its own, all started at once: what selecting a
        // thousand files and pressing Download does.
        let (rm, l, s, lb) = (remote.clone(), local.join("each"), small_s.clone(), label.clone());
        let small_copy = small.clone();
        run(Box::pin(async move {
            let mut paths = vec![];
            for d in 0..20 {
                for f in 0..50 {
                    paths.push(format!("{s}/dir{d:02}/nested/file{f:03}.txt"));
                }
            }
            let t = Instant::now();
            let jobs = paths.iter().map(|path| {
                let (rm, l, path) = (rm.clone(), l.clone(), path.clone());
                async move { rm.download(&path, &l, &Progress::default()).await }
            });
            let results = futures::future::join_all(jobs).await;
            eprintln!("[{lb}] 1000 separate downloads started at once: {:?}", t.elapsed());
            let mut names = std::collections::HashSet::new();
            for (path, result) in paths.iter().zip(results) {
                let got = result.unwrap_or_else(|err| panic!("{path}: {err}"));
                let rel = path.strip_prefix(&format!("{s}/")).unwrap();
                assert!(same_file(&got, &small_copy.join(rel)), "{path}");
                assert!(names.insert(got), "each got a file of its own");
            }
        }));

        // Cancelling partway leaves nothing behind, and the connection keeps working.
        let (rm, l, b) = (remote.clone(), local.join("cancel"), big_s.clone());
        run(Box::pin(async move {
            let p = Arc::new(Progress::default());
            let watcher = p.clone();
            let cancel = tokio::spawn(async move {
                while watcher.done.load(Ordering::Relaxed) < 4 * 1024 * 1024 {
                    tokio::time::sleep(Duration::from_millis(2)).await;
                }
                watcher.cancel.store(true, Ordering::Relaxed);
                Instant::now()
            });
            let err = rm.download(&b, &l, &p).await.unwrap_err();
            let cancelled_at = cancel.await.unwrap();
            assert_eq!(err.kind, ErrorKind::Cancelled, "{err}");
            assert!(cancelled_at.elapsed() < Duration::from_secs(2), "cancelling is prompt: {:?}", cancelled_at.elapsed());
            let left: Vec<_> = std::fs::read_dir(&l).map(|d| d.flatten().map(|e| e.file_name()).collect()).unwrap_or_default();
            assert!(left.is_empty(), "nothing left after cancelling: {left:?}");
            assert!(rm.list(&b.rsplit_once('/').unwrap().0.to_string()).await.is_ok(), "still connected");
        }));

        for dir in [&local, &upload_dir] {
            for entry in std::fs::read_dir(dir).unwrap().flatten() {
                let name = entry.file_name().to_string_lossy().into_owned();
                assert!(!name.ends_with(PARTIAL_SUFFIX), "left behind: {name}");
            }
        }
        std::fs::remove_dir_all(&local).ok();
        std::fs::remove_dir_all(&upload_dir).ok();
        r.text("exit\r");
        r.wait("the session to end", |r| r.ended);
    }
    // Transfers stream: memory stays flat however large the files.
    let status = std::fs::read_to_string("/proc/self/status").unwrap_or_default();
    if let Some(peak) = status.lines().find(|l| l.starts_with("VmHWM")) {
        eprintln!("peak memory of the test process: {}", peak.trim_start_matches("VmHWM:").trim());
    }
}


/// Real embedded SSH connections, busy terminal parsers and downloads together. All
/// fixtures live under a throwaway server; no saved Projects or homelab Hosts are used.
#[test]
#[ignore]
fn compat_many_projects_connections_and_downloads() {
    use crate::model::{Host, Library, Project};
    use crate::sftp::{self, Progress};
    use std::sync::Arc;

    let sshd = LocalSshd::start("mixed-load", 7).expect("mixed load requires sshd and ssh-keygen");
    let mut library = Library::default();
    for pi in 0..500 {
        let mut project = Project::new(format!("Project {pi}"), "blue", "server");
        for hi in 0..20 {
            let host = Host::new(format!("p{pi}-node{hi}"), "127.0.0.1");
            project.hosts.push(host.id.clone());
            library.hosts.push(host);
        }
        library.projects.push(project);
    }
    library.save(&sshd.dir).unwrap();
    assert_eq!(Library::load(&sshd.dir).unwrap(), library);
    let log: String = (0..10_000).map(|i| format!("INFO row-{i:05} café 日本語 ERROR timeout 192.168.0.20\n")).collect();
    let log_path = sshd.dir.join("output.log");
    std::fs::write(&log_path, &log).unwrap();
    let source = sshd.dir.join("payload.bin");
    std::fs::write(&source, noise(1024 * 1024, 42)).unwrap();
    let local = sshd.dir.join("downloads");
    let size = GridSize { cols: 120, rows: 40, cell_width: 9, cell_height: 18 };
    let start = Instant::now();
    let mut sessions: Vec<_> = (0..32).map(|i| {
        let project = &library.projects[i * 15];
        let resolved = library.resolve(&project.id, &project.hosts[0]).unwrap();
        sshd.connect(vec![sshd.hop(&resolved.name, &resolved.address)], size)
    }).collect();
    // Drive every host-key prompt fairly while all handshakes proceed in parallel.
    loop {
        for r in &mut sessions {
            while let Ok(event) = r.events.try_recv() { r.handle(event); }
        }
        if sessions.iter().all(|r| r.hops.last().is_some_and(|(_, step)| *step == HopStep::Done)) { break; }
        assert!(start.elapsed() < Duration::from_secs(20), "32 concurrent handshakes timed out");
        std::thread::sleep(Duration::from_millis(5));
    }
    eprintln!("500 Projects, 10,000 Hosts: 32 concurrent SSH handshakes {:?}", start.elapsed());
    for r in &sessions {
        r.text(&format!("stty -echo; cat {}; printf 'mixed-load-%s\\n' done\r", sftp::shell_quote(&log_path.to_string_lossy())));
    }
    let answers: Vec<_> = sessions.iter().map(|r| r.sink.open_sftp().unwrap()).collect();
    let remotes: Vec<_> = futures::executor::block_on(futures::future::join_all(answers)).into_iter()
        .map(|answer| Arc::new(answer.unwrap().unwrap())).collect();
    let downloads_start = Instant::now();
    let results = futures::executor::block_on(sftp::spawn(async move {
        let jobs = remotes.iter().map(|remote| {
            let (source, local) = (source.clone(), local.clone());
            async move {
                let progress = Progress::default();
                let got = remote.download(&source.to_string_lossy(), &local, &progress).await.unwrap();
                assert!(same_file(&got, &source), "download is byte-identical during log flood");
                got
            }
        });
        futures::future::join_all(jobs).await
    })).unwrap();
    assert_eq!(results.into_iter().collect::<std::collections::HashSet<_>>().len(), 32);
    eprintln!("32 simultaneous 1 MiB downloads during 320,000 log lines: {:?}", downloads_start.elapsed());
    for r in &mut sessions {
        r.wait_text("mixed-load-done");
        let term = r.term.lock();
        let text = crate::session::scrollback_text(&*term);
        assert!(text.contains("row-09999 café 日本語 ERROR"));
        assert_eq!(term.grid().history_size(), 1000, "bounded Scrollback filled under load");
        drop(term);
        r.text("exit\r");
        r.wait("connection to close", |r| r.ended);
    }
    eprintln!("mixed load complete, all 32 terminals and downloads verified: {:?}", start.elapsed());
}
