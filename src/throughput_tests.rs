//! Output throughput over SSH, end to end: an in-process SSH server prints coloured log lines as
//! fast as the client takes them, through the real Session path (`session::ssh::spawn`: russh,
//! decryption, the parser and the grid with a full Scrollback), while a stand-in for the UI
//! copies the screen once per frame the way the terminal view does.
//!
//! Ignored by default. Run it with an optimized build:
//!
//! ```text
//! cargo test --release --lib throughput_ -- --ignored --nocapture --test-threads=1
//! ```
//!
//! `KOMANDO_THROUGHPUT_LINES` sets the number of lines (default 1,000,000) and
//! `KOMANDO_THROUGHPUT_RUNS` the number of runs (default 3).

use std::sync::Arc;
use std::time::{Duration, Instant};

use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::index::{Column, Line};
use futures::StreamExt;
use russh::server::{self, Msg, Server as _, Session};
use russh::{Channel, ChannelId};
use tokio::sync::Mutex;

use crate::session::{EventProxy, GridSize, Prompt, SessionEvent, Status, new_term};
use crate::theme::TerminalPalette;

/// Log line `i`, about 96 bytes: a dim timestamp, a coloured level, a service and a message.
fn log_line(i: u64, out: &mut Vec<u8>) {
    use std::io::Write;
    let (color, level) = match i % 16 {
        0 => ("31", "ERROR"),
        1 | 2 => ("33", "WARN "),
        3 => ("35", "DEBUG"),
        _ => ("32", "INFO "),
    };
    let services = ["api", "worker", "auth", "billing", "scheduler"];
    let service = services[(i % 5) as usize];
    let _ = write!(
        out,
        "\x1b[2m2026-11-13 {:02}:{:02}:{:02}.{:03}\x1b[0m \x1b[{color}m{level}\x1b[0m \x1b[36m{service:<9}\x1b[0m GET /api/items/{} {} in {}ms\r\n",
        (i / 3_600_000) % 24,
        (i / 60_000) % 60,
        (i / 1000) % 60,
        i % 1000,
        i % 100_000,
        if i.is_multiple_of(16) { 500 } else { 200 },
        i % 997,
    );
}

/// The last line's text as the terminal shows it.
fn plain_line(i: u64) -> String {
    let mut bytes = Vec::new();
    log_line(i, &mut bytes);
    let mut text = String::new();
    let mut chars = std::str::from_utf8(&bytes).unwrap().chars();
    while let Some(c) = chars.next() {
        if c == '\x1b' {
            for c in chars.by_ref() {
                if c == 'm' {
                    break;
                }
            }
        } else if c != '\r' && c != '\n' {
            text.push(c);
        }
    }
    text.trim_end().to_string()
}

/// Prints `lines` log lines to each shell, except for the user `quiet`, whose shell echoes
/// what is typed, like a shell at its prompt. Over SFTP it offers one file, `/big.bin`, of
/// `file_size` bytes, made up as it is read.
#[derive(Clone)]
struct LogServer {
    lines: u64,
    file_size: u64,
    user: String,
    channels: Arc<Mutex<std::collections::HashMap<ChannelId, Channel<Msg>>>>,
}

impl LogServer {
    fn new(lines: u64, file_size: u64) -> Self {
        Self { lines, file_size, user: String::new(), channels: Arc::default() }
    }
}

impl server::Server for LogServer {
    type Handler = Self;
    fn new_client(&mut self, _: Option<std::net::SocketAddr>) -> Self {
        Self::new(self.lines, self.file_size)
    }
}

impl server::Handler for LogServer {
    type Error = russh::Error;

    async fn auth_password(&mut self, user: &str, _: &str) -> Result<server::Auth, Self::Error> {
        self.user = user.to_string();
        Ok(server::Auth::Accept)
    }

    async fn channel_open_session(
        &mut self,
        channel: Channel<Msg>,
        reply: server::ChannelOpenHandle,
        _: &mut Session,
    ) -> Result<(), Self::Error> {
        self.channels.lock().await.insert(channel.id(), channel);
        reply.accept().await;
        Ok(())
    }

    async fn subsystem_request(&mut self, id: ChannelId, name: &str, session: &mut Session) -> Result<(), Self::Error> {
        let channel = self.channels.lock().await.remove(&id);
        match channel {
            Some(channel) if name == "sftp" => {
                session.channel_success(id)?;
                russh_sftp::server::run(channel.into_stream(), BigFile::new(self.file_size)).await;
            }
            _ => session.channel_failure(id)?,
        }
        Ok(())
    }

    /// Komando asks `id` when SFTP opens.
    async fn exec_request(&mut self, id: ChannelId, _: &[u8], session: &mut Session) -> Result<(), Self::Error> {
        session.channel_success(id)?;
        if let Some(channel) = self.channels.lock().await.remove(&id) {
            tokio::spawn(async move {
                let _ = channel.data(&b"uid=1000(bench) gid=1000(bench) groups=1000(bench)\n"[..]).await;
                let _ = channel.exit_status(0).await;
                let _ = channel.eof().await;
                let _ = channel.close().await;
            });
        }
        Ok(())
    }

    async fn pty_request(
        &mut self,
        channel: ChannelId,
        _: &str,
        _: u32,
        _: u32,
        _: u32,
        _: u32,
        _: &[(russh::Pty, u32)],
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        session.channel_success(channel)?;
        Ok(())
    }

    async fn shell_request(&mut self, channel: ChannelId, session: &mut Session) -> Result<(), Self::Error> {
        session.channel_success(channel)?;
        let Some(mut channel) = self.channels.lock().await.remove(&channel) else { return Ok(()) };
        if self.user == "quiet" {
            tokio::spawn(async move {
                while let Some(msg) = channel.wait().await {
                    if let russh::ChannelMsg::Data { data } = msg
                        && channel.data(&data[..]).await.is_err()
                    {
                        return;
                    }
                }
            });
            return Ok(());
        }
        let lines = self.lines;
        tokio::spawn(async move {
            // Blocks of about 64 KiB, each sent once the client's window has room for it.
            let mut block = Vec::with_capacity(70 * 1024);
            for i in 0..lines {
                log_line(i, &mut block);
                if block.len() >= 64 * 1024 || i + 1 == lines {
                    if channel.data(&block[..]).await.is_err() {
                        return;
                    }
                    block.clear();
                }
            }
            let _ = channel.exit_status(0).await;
            let _ = channel.eof().await;
            let _ = channel.close().await;
        });
        Ok(())
    }
}

/// Start the server on its own runtime (a separate program in real use) and return its port.
fn start_server(lines: u64, threads: usize) -> (u16, tokio::runtime::Runtime) {
    start_server_with_file(lines, 0, threads)
}

fn start_server_with_file(lines: u64, file_size: u64, threads: usize) -> (u16, tokio::runtime::Runtime) {
    let runtime = tokio::runtime::Builder::new_multi_thread().worker_threads(threads).enable_all().build().unwrap();
    let key = russh::keys::PrivateKey::from(russh::keys::ssh_key::private::Ed25519Keypair::from_seed(&[7; 32]));
    let config = Arc::new(server::Config {
        keys: vec![key],
        auth_rejection_time: Duration::ZERO,
        auth_rejection_time_initial: Some(Duration::ZERO),
        inactivity_timeout: None,
        nodelay: true,
        ..Default::default()
    });
    let listener = runtime.block_on(tokio::net::TcpListener::bind("127.0.0.1:0")).unwrap();
    let port = listener.local_addr().unwrap().port();
    runtime.spawn(async move {
        let mut server = LogServer::new(lines, file_size);
        let _ = server.run_on_socket(config, &listener).await;
    });
    (port, runtime)
}

/// The SFTP side of [`LogServer`]: `/big.bin`, and `/many`, a folder of [`MANY_FILES`] files of
/// [`MANY_SIZE`] bytes. Contents are made up as they are read.
struct BigFile {
    size: u64,
    /// Folder listings already sent, by handle.
    listed: std::collections::HashSet<String>,
}

const MANY_FILES: usize = 300;
const MANY_SIZE: u64 = 64 * 1024;

impl BigFile {
    fn new(size: u64) -> Self {
        Self { size, listed: Default::default() }
    }

    fn attrs(&self, path: &str) -> Result<russh_sftp::protocol::FileAttributes, russh_sftp::protocol::StatusCode> {
        let (size, permissions) = match path {
            "/big.bin" => (self.size, 0o100644),
            "/many" => (0, 0o040755),
            _ if path.starts_with("/many/") => (MANY_SIZE, 0o100644),
            _ => return Err(russh_sftp::protocol::StatusCode::NoSuchFile),
        };
        Ok(russh_sftp::protocol::FileAttributes { size: Some(size), permissions: Some(permissions), ..Default::default() })
    }

    fn ok(id: u32) -> russh_sftp::protocol::Status {
        russh_sftp::protocol::Status {
            id,
            status_code: russh_sftp::protocol::StatusCode::Ok,
            error_message: "Ok".into(),
            language_tag: "en-US".into(),
        }
    }
}

impl russh_sftp::server::Handler for BigFile {
    type Error = russh_sftp::protocol::StatusCode;

    fn unimplemented(&self) -> Self::Error {
        russh_sftp::protocol::StatusCode::OpUnsupported
    }

    async fn realpath(&mut self, id: u32, _: String) -> Result<russh_sftp::protocol::Name, Self::Error> {
        Ok(russh_sftp::protocol::Name { id, files: vec![russh_sftp::protocol::File::dummy("/")] })
    }

    async fn stat(&mut self, id: u32, path: String) -> Result<russh_sftp::protocol::Attrs, Self::Error> {
        Ok(russh_sftp::protocol::Attrs { id, attrs: self.attrs(&path)? })
    }

    async fn lstat(&mut self, id: u32, path: String) -> Result<russh_sftp::protocol::Attrs, Self::Error> {
        Ok(russh_sftp::protocol::Attrs { id, attrs: self.attrs(&path)? })
    }

    async fn fstat(&mut self, id: u32, handle: String) -> Result<russh_sftp::protocol::Attrs, Self::Error> {
        Ok(russh_sftp::protocol::Attrs { id, attrs: self.attrs(&handle)? })
    }

    async fn open(
        &mut self,
        id: u32,
        path: String,
        _: russh_sftp::protocol::OpenFlags,
        _: russh_sftp::protocol::FileAttributes,
    ) -> Result<russh_sftp::protocol::Handle, Self::Error> {
        self.attrs(&path)?;
        Ok(russh_sftp::protocol::Handle { id, handle: path })
    }

    async fn read(&mut self, id: u32, handle: String, offset: u64, len: u32) -> Result<russh_sftp::protocol::Data, Self::Error> {
        let size = self.attrs(&handle)?.size.unwrap_or(0);
        if offset >= size {
            return Err(russh_sftp::protocol::StatusCode::Eof);
        }
        let len = (len as u64).min(size - offset) as usize;
        Ok(russh_sftp::protocol::Data { id, data: (0..len).map(|i| (offset as usize + i) as u8).collect() })
    }

    async fn opendir(&mut self, id: u32, path: String) -> Result<russh_sftp::protocol::Handle, Self::Error> {
        if path.trim_end_matches('/') != "/many" {
            return Err(russh_sftp::protocol::StatusCode::NoSuchFile);
        }
        let handle = format!("dir {id}");
        self.listed.remove(&handle);
        Ok(russh_sftp::protocol::Handle { id, handle })
    }

    async fn readdir(&mut self, id: u32, handle: String) -> Result<russh_sftp::protocol::Name, Self::Error> {
        if !self.listed.insert(handle) {
            return Err(russh_sftp::protocol::StatusCode::Eof);
        }
        let files = (0..MANY_FILES)
            .map(|i| {
                let name = format!("file{i:03}.log");
                let attrs = self.attrs(&format!("/many/{name}")).expect("a file of the folder");
                russh_sftp::protocol::File::new(name, attrs)
            })
            .collect();
        Ok(russh_sftp::protocol::Name { id, files })
    }

    async fn close(&mut self, id: u32, handle: String) -> Result<russh_sftp::protocol::Status, Self::Error> {
        self.listed.remove(&handle);
        Ok(Self::ok(id))
    }
}

struct Run {
    elapsed: Duration,
    bytes: u64,
    frames: u32,
}

fn run_once(port: u16, lines: u64, known_hosts: &std::path::Path, external: bool) -> Run {
    // A 1600 x 1000 pixel window with Cascadia Mono at 13 px: about 177 x 42 cells.
    let size = GridSize { cols: 177, rows: 42, cell_width: 9, cell_height: 17 };
    let (tx, mut events) = futures::channel::mpsc::unbounded();
    let proxy = EventProxy::new(tx);
    proxy.set_palette(&TerminalPalette::dark());
    proxy.set_size(size);
    let term = new_term(size, 100_000, proxy.clone());
    let target = crate::session::ssh::Target {
        hops: vec![crate::session::ssh::Hop {
            name: "bench".into(),
            address: "127.0.0.1".into(),
            port,
            user: Some("bench".into()),
            secret: Some(crate::vault::Secret::Password { password: "bench".into() }),
        }],
        known_hosts: known_hosts.to_path_buf(),
    };
    let sink = crate::session::ssh::spawn(target, size, term.clone(), proxy.clone());

    // The UI: answer prompts, and per Wakeup, wait for the next frame and copy the screen.
    let frame = Duration::from_micros(16_667);
    let mut started = None;
    let mut frames = 0;
    let elapsed = futures::executor::block_on(async {
        while let Some(event) = events.next().await {
            match event {
                SessionEvent::Prompt(Prompt::HostKey { reply, .. }) => {
                    let _ = reply.send(true);
                }
                SessionEvent::Status(Status::Connected) => started = Some(Instant::now()),
                SessionEvent::Status(Status::Disconnected(reason)) => {
                    let started = started.unwrap_or_else(|| panic!("never connected: {reason}"));
                    return started.elapsed();
                }
                SessionEvent::Wakeup => {
                    std::thread::sleep(frame);
                    proxy.clear_dirty();
                    frames += 1;
                    let term = term.lock();
                    let content = term.renderable_content();
                    let mut cells = 0usize;
                    for indexed in content.display_iter {
                        cells += indexed.cell.c as usize & 1;
                    }
                    std::hint::black_box(cells);
                }
                _ => {}
            }
        }
        panic!("the session ended without a status");
    });
    sink.close();

    if external {
        return Run { elapsed, bytes: 0, frames };
    }
    // Every line arrived, in order, ending with the last one.
    let term = term.lock();
    let grid = term.grid();
    let cursor = grid.cursor.point.line.0;
    let row = |line: i32| {
        let row = &grid[Line(line)];
        (0..grid.columns()).map(|col| row[Column(col)].c).collect::<String>().trim_end().to_string()
    };
    assert_eq!(row(cursor - 1), plain_line(lines - 1));
    assert_eq!(row(cursor - 2), plain_line(lines - 2));
    assert_eq!(grid.history_size(), (lines as usize + 1).saturating_sub(size.rows as usize).min(100_000));

    let mut bytes = Vec::new();
    let mut total = 0;
    for i in 0..lines {
        bytes.clear();
        log_line(i, &mut bytes);
        total += bytes.len() as u64;
    }
    Run { elapsed, bytes: total, frames }
}

#[test]
#[ignore]
fn throughput_coloured_log_lines_over_ssh() {
    let lines: u64 = std::env::var("KOMANDO_THROUGHPUT_LINES").ok().and_then(|v| v.parse().ok()).unwrap_or(1_000_000);
    let runs: usize = std::env::var("KOMANDO_THROUGHPUT_RUNS").ok().and_then(|v| v.parse().ok()).unwrap_or(3);
    // `KOMANDO_THROUGHPUT_PORT`: use a server that is already running there instead, which
    // reports its own timings (the run here lasts until it closes the shell).
    let external: Option<u16> = std::env::var("KOMANDO_THROUGHPUT_PORT").ok().and_then(|v| v.parse().ok());
    let (port, _server) = match external {
        Some(port) => (port, None),
        None => {
            let (port, server) = start_server(lines, 2);
            (port, Some(server))
        }
    };
    let dir = std::env::temp_dir().join(format!("komando-throughput-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let known_hosts = dir.join("known_hosts");

    let mut times = Vec::new();
    for run in 1..=runs {
        let result = run_once(port, lines, &known_hosts, external.is_some());
        let secs = result.elapsed.as_secs_f64();
        eprintln!(
            "run {run}: {lines} lines, {:.1} MB in {secs:.3} s = {:.1} MB/s, {} frames",
            result.bytes as f64 / 1e6,
            result.bytes as f64 / 1e6 / secs,
            result.frames,
        );
        times.push(secs);
    }
    times.sort_by(f64::total_cmp);
    eprintln!("median {:.3} s", times[times.len() / 2]);
    let _ = std::fs::remove_dir_all(&dir);
}

/// Where parsing time goes: the same output fed straight into a terminal, no SSH.
#[test]
#[ignore]
fn throughput_parser_only() {
    let lines: u64 = std::env::var("KOMANDO_THROUGHPUT_LINES").ok().and_then(|v| v.parse().ok()).unwrap_or(1_000_000);
    let mut data = Vec::new();
    for i in 0..lines {
        log_line(i, &mut data);
    }
    let chunks: Vec<&[u8]> = data.chunks(32 * 1024).collect();
    for scrollback in [100_000, 10_000, 0] {
        let (tx, _rx) = futures::channel::mpsc::unbounded();
        let proxy = EventProxy::new(tx);
        let size = GridSize { cols: 177, rows: 42, cell_width: 9, cell_height: 17 };
        let term = new_term(size, scrollback, proxy.clone());
        let mut output = crate::session::ssh::Output::default();
        let start = Instant::now();
        for chunk in &chunks {
            output.feed(&term, &proxy, chunk);
            proxy.clear_dirty();
        }
        let secs = start.elapsed().as_secs_f64();
        eprintln!("scrollback {scrollback:>6}: {:.3} s = {:.1} MB/s", secs, data.len() as f64 / 1e6 / secs);
    }
}

/// A Session to the benchmark server as `user`, at the size of a 1600 x 1000 pixel window.
struct Bench {
    term: crate::session::SharedTerm,
    proxy: EventProxy,
    sink: crate::session::InputSink,
    events: futures::channel::mpsc::UnboundedReceiver<SessionEvent>,
}

fn connect(port: u16, user: &str, known_hosts: &std::path::Path) -> Bench {
    let size = GridSize { cols: 177, rows: 42, cell_width: 9, cell_height: 17 };
    let (tx, events) = futures::channel::mpsc::unbounded();
    let proxy = EventProxy::new(tx);
    proxy.set_palette(&TerminalPalette::dark());
    proxy.set_size(size);
    let term = new_term(size, 100_000, proxy.clone());
    let target = crate::session::ssh::Target {
        hops: vec![crate::session::ssh::Hop {
            name: "bench".into(),
            address: "127.0.0.1".into(),
            port,
            user: Some(user.into()),
            secret: Some(crate::vault::Secret::Password { password: "bench".into() }),
        }],
        known_hosts: known_hosts.to_path_buf(),
    };
    let sink = crate::session::ssh::spawn(target, size, term.clone(), proxy.clone());
    Bench { term, proxy, sink, events }
}

impl Bench {
    /// The next event, if one is waiting. Host Key prompts are answered on the way.
    fn next(&mut self) -> Option<SessionEvent> {
        loop {
            match self.events.try_recv() {
                Ok(SessionEvent::Prompt(Prompt::HostKey { reply, .. })) => {
                    let _ = reply.send(true);
                }
                Ok(event) => return Some(event),
                Err(futures::channel::mpsc::TryRecvError::Closed) => panic!("the session ended without a status"),
                Err(futures::channel::mpsc::TryRecvError::Empty) => return None,
            }
        }
    }

    fn wait_connected(&mut self) {
        let start = Instant::now();
        loop {
            match self.next() {
                Some(SessionEvent::Status(Status::Connected)) => return,
                Some(SessionEvent::Status(Status::Disconnected(reason))) => panic!("{reason}"),
                Some(SessionEvent::Wakeup) => self.proxy.clear_dirty(),
                Some(_) => {}
                None => std::thread::yield_now(),
            }
            assert!(start.elapsed() < Duration::from_secs(10), "connecting timed out");
        }
    }

    /// Type a key and wait until its echo has been parsed into the grid.
    fn echo(&mut self) -> Duration {
        let start = Instant::now();
        self.sink.write(b"a".to_vec());
        loop {
            match self.next() {
                Some(SessionEvent::Wakeup) => {
                    self.proxy.clear_dirty();
                    return start.elapsed();
                }
                Some(SessionEvent::Status(Status::Disconnected(reason))) => panic!("{reason}"),
                _ => std::thread::yield_now(),
            }
        }
    }
}

fn percentiles(mut samples: Vec<Duration>) -> String {
    samples.sort();
    let at = |p: f64| samples[((samples.len() - 1) as f64 * p) as usize].as_secs_f64() * 1000.0;
    format!("median {:.2} ms, p95 {:.2} ms, max {:.2} ms ({} keys)", at(0.5), at(0.95), at(1.0), samples.len())
}

/// Several Sessions printing logs at once, the way an engineer watches a few deploys, while
/// another Session sits at a prompt: how long all the output takes, and how long a key typed in
/// the quiet Session takes to show.
///
/// `KOMANDO_THROUGHPUT_SESSIONS` sets the number of busy Sessions (default 8) and
/// `KOMANDO_THROUGHPUT_LINES` the lines each prints (default 500,000).
#[test]
#[ignore]
fn throughput_many_sessions_and_typing() {
    let sessions: usize = std::env::var("KOMANDO_THROUGHPUT_SESSIONS").ok().and_then(|v| v.parse().ok()).unwrap_or(8);
    let lines: u64 = std::env::var("KOMANDO_THROUGHPUT_LINES").ok().and_then(|v| v.parse().ok()).unwrap_or(500_000);
    // Enough server threads that the server is not what limits the busy Sessions.
    let (port, _server) = start_server(lines, 8);
    let dir = std::env::temp_dir().join(format!("komando-throughput-many-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let known_hosts = dir.join("known_hosts");

    // The quiet Session first, which also trusts the server's key for the others.
    let mut quiet = connect(port, "quiet", &known_hosts);
    quiet.wait_connected();
    let idle: Vec<Duration> = (0..50)
        .map(|_| {
            let latency = quiet.echo();
            std::thread::sleep(Duration::from_millis(5));
            latency
        })
        .collect();

    let mut bytes_each = 0u64;
    let mut block = Vec::new();
    for i in 0..lines {
        log_line(i, &mut block);
        bytes_each += block.len() as u64;
        block.clear();
    }

    // The busy Sessions, each with a UI stand-in that re-arms Wakeups once per frame. Only the
    // first copies its screen, as only the Session in front is drawn.
    let remaining = Arc::new(std::sync::atomic::AtomicUsize::new(sessions));
    let start = Instant::now();
    let workers: Vec<_> = (0..sessions)
        .map(|index| {
            let remaining = remaining.clone();
            let mut bench = connect(port, "busy", &known_hosts);
            std::thread::spawn(move || {
                let mut connected_at = None;
                loop {
                    match bench.next() {
                        Some(SessionEvent::Status(Status::Connected)) => connected_at = Some(Instant::now()),
                        Some(SessionEvent::Status(Status::Disconnected(reason))) => {
                            remaining.fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
                            let connected_at = connected_at.unwrap_or_else(|| panic!("never connected: {reason}"));
                            bench.sink.close();
                            return (connected_at, Instant::now());
                        }
                        Some(SessionEvent::Wakeup) => {
                            std::thread::sleep(Duration::from_millis(16));
                            bench.proxy.clear_dirty();
                            if index == 0 {
                                let term = bench.term.lock();
                                let mut cells = 0usize;
                                for indexed in term.renderable_content().display_iter {
                                    cells += indexed.cell.c as usize & 1;
                                }
                                std::hint::black_box(cells);
                            }
                        }
                        Some(_) => {}
                        None => std::thread::sleep(Duration::from_millis(1)),
                    }
                }
            })
        })
        .collect();

    // Type in the quiet Session while the others print, about 30 keys a second.
    let mut busy = Vec::new();
    while remaining.load(std::sync::atomic::Ordering::Relaxed) > 0 {
        if start.elapsed() > Duration::from_millis(300) {
            busy.push(quiet.echo());
        }
        std::thread::sleep(Duration::from_millis(30));
    }
    let ends: Vec<_> = workers.into_iter().map(|w| w.join().unwrap()).collect();
    let first_start = ends.iter().map(|e| e.0).min().unwrap();
    let last_end = ends.iter().map(|e| e.1).max().unwrap();
    let total = (last_end - first_start).as_secs_f64();
    let mut each: Vec<f64> = ends.iter().map(|e| (e.1 - e.0).as_secs_f64()).collect();
    each.sort_by(f64::total_cmp);
    eprintln!(
        "{sessions} busy Sessions x {lines} lines ({:.1} MB each): all done in {total:.3} s = {:.1} MB/s together; each took {:.3}-{:.3} s",
        bytes_each as f64 / 1e6,
        (bytes_each * sessions as u64) as f64 / 1e6 / total,
        each[0],
        each[each.len() - 1],
    );
    eprintln!("typing, idle:        {}", percentiles(idle));
    eprintln!("typing, while busy:  {}", percentiles(busy));
    quiet.sink.close();
    let _ = std::fs::remove_dir_all(&dir);
}

/// A TCP forwarder that delays every byte by `one_way` in each direction, like a distant
/// server, and with `bytes_per_sec` caps each direction's bandwidth, like a slow link. What
/// the link can't send yet waits in its queue, as in a router with plenty of buffer.
/// (The same as the one in `compat_tests.rs`, which only builds on Linux.)
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

/// Typing in a Session while a large file downloads over SFTP on the same connection: how long
/// a key takes to show, and how fast the download goes. Directly, and through a link with
/// latency and limited bandwidth, where the download's data queues up ahead of the key's echo.
///
/// `KOMANDO_THROUGHPUT_SFTP_MB` sets the file size for the direct run (default 512 MB).
#[test]
#[ignore]
fn throughput_typing_during_sftp_download() {
    let mb: u64 = std::env::var("KOMANDO_THROUGHPUT_SFTP_MB").ok().and_then(|v| v.parse().ok()).unwrap_or(512);
    let dir = std::env::temp_dir().join(format!("komando-throughput-sftp-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let known_hosts = dir.join("known_hosts");

    // (label, one-way delay, bandwidth in bytes/s, file size): the slower links get about
    // four seconds' worth.
    let links: [(&str, Duration, Option<u64>, u64); 4] = [
        ("direct", Duration::ZERO, None, mb * 1_000_000),
        ("25 ms each way, 1 Gbit/s", Duration::from_millis(25), Some(125_000_000), 500_000_000),
        ("25 ms each way, 100 Mbit/s", Duration::from_millis(25), Some(12_500_000), 50_000_000),
        ("15 ms each way, 10 Mbit/s", Duration::from_millis(15), Some(1_250_000), 5_000_000),
    ];
    for (label, one_way, rate, size) in links {
        let (server_port, _server) = start_server_with_file(0, size, 4);
        let proxy = (one_way > Duration::ZERO || rate.is_some()).then(|| DelayProxy::start(server_port, one_way, rate));
        let port = proxy.as_ref().map_or(server_port, |p| p.port);

        let mut quiet = connect(port, "quiet", &known_hosts);
        quiet.wait_connected();
        let idle: Vec<Duration> = (0..20)
            .map(|_| {
                let latency = quiet.echo();
                std::thread::sleep(Duration::from_millis(10));
                latency
            })
            .collect();

        let answer = quiet.sink.open_sftp().expect("an SSH Session opens SFTP");
        let remote = Arc::new(futures::executor::block_on(answer).expect("a reply").expect("SFTP starts"));
        let local = dir.join(format!("downloads-{}", label.len()));
        let done = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let download = {
            let (remote, local, done) = (remote.clone(), local.clone(), done.clone());
            std::thread::spawn(move || {
                let start = Instant::now();
                let progress = crate::sftp::Progress::default();
                let path = futures::executor::block_on(crate::sftp::spawn(async move {
                    remote.download("/big.bin", &local, &progress).await
                }))
                .expect("the download task ran")
                .expect("the download succeeded");
                let elapsed = start.elapsed();
                done.store(true, std::sync::atomic::Ordering::Relaxed);
                (path, elapsed)
            })
        };

        // Type about 20 keys a second while it downloads, from a moment after it starts.
        let start = Instant::now();
        let mut busy = Vec::new();
        while !done.load(std::sync::atomic::Ordering::Relaxed) {
            if start.elapsed() > Duration::from_millis(300) {
                busy.push(quiet.echo());
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        let (path, elapsed) = download.join().unwrap();
        assert_eq!(std::fs::metadata(&path).unwrap().len(), size, "the whole file arrived");
        let _ = std::fs::remove_dir_all(&local);

        // A folder of many small files, which download side by side.
        let start = Instant::now();
        let folder = futures::executor::block_on(crate::sftp::spawn({
            let (remote, local) = (remote.clone(), local.clone());
            async move { remote.download("/many", &local, &crate::sftp::Progress::default()).await }
        }))
        .expect("the download task ran")
        .expect("the folder downloaded");
        let folder_time = start.elapsed();
        let files: Vec<_> = std::fs::read_dir(&folder).unwrap().map(|e| e.unwrap().metadata().unwrap().len()).collect();
        assert_eq!(files.len(), MANY_FILES);
        assert!(files.iter().all(|&len| len == MANY_SIZE));
        let _ = std::fs::remove_dir_all(&local);
        eprintln!(
            "[{label}] download {:.0} MB in {:.2} s = {:.1} MB/s",
            size as f64 / 1e6,
            elapsed.as_secs_f64(),
            size as f64 / 1e6 / elapsed.as_secs_f64()
        );
        eprintln!(
            "[{label}] folder of {MANY_FILES} x {} KB in {:.2} s",
            MANY_SIZE / 1024,
            folder_time.as_secs_f64()
        );
        eprintln!("[{label}] typing, idle:               {}", percentiles(idle));
        if busy.is_empty() {
            eprintln!("[{label}] typing, while downloading: the download ended too soon to type");
        } else {
            eprintln!("[{label}] typing, while downloading: {}", percentiles(busy));
        }
        quiet.sink.close();
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// How long the operations that hold the terminal's lock take with a full Scrollback: while one
/// runs, output stops being parsed and the screen stops being drawn. Command Marks every
/// thousand lines, as if commands were typed between the logs.
#[test]
#[ignore]
fn throughput_lock_holders_with_full_scrollback() {
    fn time<R>(label: &str, f: impl FnOnce() -> R) -> R {
        let start = Instant::now();
        let result = f();
        eprintln!("  {label:<44} {:>9.2} ms", start.elapsed().as_secs_f64() * 1e3);
        result
    }
    for scrollback in [100_000usize, 500_000] {
        let (tx, _rx) = futures::channel::mpsc::unbounded();
        let proxy = EventProxy::new(tx);
        let size = GridSize { cols: 177, rows: 42, cell_width: 9, cell_height: 17 };
        let term = new_term(size, scrollback, proxy.clone());
        let mut output = crate::session::ssh::Output::default();
        let mut marks = crate::marks::Marks::default();
        let mut data = Vec::new();
        for i in 0..scrollback as u64 + 1_000 {
            if i % 1_000 == 0 {
                output.feed(&term, &proxy, &data);
                data.clear();
                output.feed(&term, &proxy, b"[ops@web01 ~]$ tail -n 1000 app.log");
                marks.entered(&mut *term.lock());
                output.feed(&term, &proxy, b"\r\n");
                marks.output(&mut *term.lock());
            }
            log_line(i, &mut data);
        }
        output.feed(&term, &proxy, &data);
        marks.output(&mut *term.lock());
        eprintln!(
            "scrollback {scrollback}: {} rows, {} marks",
            term.lock().grid().history_size(),
            marks.list().len()
        );

        // Marks, once per frame while the command rail shows.
        time("marks: first look", || marks.rows(&*term.lock()).len());
        time("marks: nothing moved", || marks.rows(&*term.lock()).len());
        let mut line = Vec::new();
        log_line(0, &mut line);
        output.feed(&term, &proxy, &line);
        time("marks: after one more line", || marks.rows(&*term.lock()).len());
        let newest = *marks.rows(&*term.lock()).last().expect("marks");
        term.lock().grid_mut()[newest.1][Column(0)].set_hyperlink(None);
        time("marks: newest one gone (rescan)", || marks.rows(&*term.lock()).len());

        time("Copy output", || crate::session::scrollback_text(&*term.lock()).len());
        let raw = time("  of which the text", || {
            let term = term.lock();
            let grid = term.grid();
            let top = alacritty_terminal::index::Point::new(Line(-(grid.history_size() as i32)), Column(0));
            let bottom = alacritty_terminal::index::Point::new(Line(grid.screen_lines() as i32 - 1), grid.last_column());
            term.bounds_to_string(top, bottom)
        });
        time("  of which tidying it", || crate::session::tidy_export(&raw).len());

        let resize = |label: &str, cols: u16, rows: u16| {
            time(label, || {
                let mut term = term.lock();
                term.resize(GridSize { cols, rows, ..size }.term_size());
                crate::session::drop_blank_scrollback(&mut *term);
            })
        };
        resize("resize: one row less", 177, 41);
        resize("resize: one row more", 177, 42);
        resize("resize: one column narrower", 176, 42);
        resize("resize: one column wider", 177, 42);
        resize("resize: half as wide", 88, 42);
        resize("resize: back to full width", 177, 42);
        time("compact rows (in stretches, off the UI)", || crate::session::compact_rows(&term, 177));
    }
}
