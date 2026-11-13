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
/// what is typed, like a shell at its prompt.
#[derive(Clone)]
struct LogServer {
    lines: u64,
    user: String,
    channel: Arc<Mutex<Option<Channel<Msg>>>>,
}

impl server::Server for LogServer {
    type Handler = Self;
    fn new_client(&mut self, _: Option<std::net::SocketAddr>) -> Self {
        Self { lines: self.lines, user: String::new(), channel: Arc::default() }
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
        *self.channel.lock().await = Some(channel);
        reply.accept().await;
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
        let Some(mut channel) = self.channel.lock().await.take() else { return Ok(()) };
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
        let mut server = LogServer { lines, user: String::new(), channel: Arc::default() };
        let _ = server.run_on_socket(config, &listener).await;
    });
    (port, runtime)
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
