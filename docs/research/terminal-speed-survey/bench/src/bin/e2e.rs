//! Loopback SSH throughput: a russh server prints N coloured log lines in 64 KiB blocks (as
//! Komando's throughput test does); a russh client takes them and either discards them or
//! parses them into an alacritty Term (100k scrollback, 177x42) under a FairMutex.
//!
//! usage: e2e <cipher> <mode> [lines] [max_packet] [window]
//!   cipher: chacha | aes256gcm | aes128gcm
//!   mode:   discard | parse | parse-batch (parse, but coalesce what is already queued)
//!           parser-only (no SSH, 32 KiB chunks) | parser-only-256k
#[cfg(feature = "mimalloc")]
#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

use std::borrow::Cow;
use std::sync::Arc;
use std::time::{Duration, Instant};

use alacritty_terminal::event::VoidListener;
use alacritty_terminal::sync::FairMutex;
use alacritty_terminal::term::test::TermSize;
use alacritty_terminal::term::{Config as TermConfig, Term};
use alacritty_terminal::vte::ansi::Processor;
use russh::server::{self, Msg, Server as _, Session};
use russh::{Channel, ChannelId, ChannelMsg, Preferred, cipher};

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
        if i % 16 == 0 { 500 } else { 200 },
        i % 997,
    );
}

#[derive(Clone)]
struct LogServer {
    lines: u64,
    channels: Arc<tokio::sync::Mutex<std::collections::HashMap<ChannelId, Channel<Msg>>>>,
}

impl server::Server for LogServer {
    type Handler = Self;
    fn new_client(&mut self, _: Option<std::net::SocketAddr>) -> Self {
        self.clone()
    }
}

impl server::Handler for LogServer {
    type Error = russh::Error;
    async fn auth_password(&mut self, _: &str, _: &str) -> Result<server::Auth, Self::Error> {
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
    async fn shell_request(&mut self, id: ChannelId, session: &mut Session) -> Result<(), Self::Error> {
        session.channel_success(id)?;
        let Some(channel) = self.channels.lock().await.remove(&id) else { return Ok(()) };
        let lines = self.lines;
        let (mut read, channel) = channel.split();
        tokio::spawn(async move { while read.wait().await.is_some() {} });
        tokio::spawn(async move {
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

struct Client;
impl russh::client::Handler for Client {
    type Error = russh::Error;
    async fn check_server_key(&mut self, _: &russh::keys::PublicKeyOrCertificate) -> Result<bool, Self::Error> {
        Ok(true)
    }
}

fn new_term() -> Arc<FairMutex<Term<VoidListener>>> {
    let config = TermConfig { scrolling_history: 100_000, ..TermConfig::default() };
    Arc::new(FairMutex::new(Term::new(config, &TermSize::new(177, 42), VoidListener)))
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let cipher_name = args.get(1).map(String::as_str).unwrap_or("chacha");
    let mode = args.get(2).cloned().unwrap_or_else(|| "discard".into());
    let lines: u64 = args.get(3).and_then(|s| s.parse().ok()).unwrap_or(1_000_000);
    let max_packet: u32 = args.get(4).and_then(|s| s.parse().ok()).unwrap_or(32768);
    let window: u32 = args.get(5).and_then(|s| s.parse().ok()).unwrap_or(8 << 20);

    if mode.starts_with("parser-only") {
        let chunk = if mode.ends_with("256k") { 256 * 1024 } else { 32 * 1024 };
        let mut data = Vec::new();
        for i in 0..lines {
            log_line(i, &mut data);
        }
        let term = new_term();
        let mut parser: Processor = Processor::new();
        let t = Instant::now();
        for c in data.chunks(chunk) {
            parser.advance(&mut *term.lock(), c);
        }
        let s = t.elapsed().as_secs_f64();
        println!("{mode}: {:.1} MB in {s:.3} s = {:.0} MB/s", data.len() as f64 / 1e6, data.len() as f64 / 1e6 / s);
        return;
    }

    let cipher_list: Vec<cipher::Name> = match cipher_name {
        "chacha" => vec![cipher::CHACHA20_POLY1305],
        "aes256gcm" => vec![cipher::AES_256_GCM],
        "aes128gcm" => vec![cipher::AES_128_GCM],
        "aes256ctr" => vec![cipher::AES_256_CTR],
        other => panic!("cipher {other}"),
    };

    // Server on its own 2-thread runtime, like Komando's test.
    let srt = tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build().unwrap();
    let key = russh::keys::PrivateKey::from(russh::keys::ssh_key::private::Ed25519Keypair::from_seed(&[7; 32]));
    let sconfig = Arc::new(server::Config {
        keys: vec![key],
        auth_rejection_time: Duration::ZERO,
        auth_rejection_time_initial: Some(Duration::ZERO),
        inactivity_timeout: None,
        nodelay: true,
        ..Default::default()
    });
    let listener = srt.block_on(tokio::net::TcpListener::bind("127.0.0.1:0")).unwrap();
    let port = listener.local_addr().unwrap().port();
    let mut server = LogServer { lines, channels: Default::default() };
    srt.spawn(async move {
        let _ = server.run_on_socket(sconfig, &listener).await;
    });

    // Client on Komando's runtime shape.
    let threads = std::thread::available_parallelism().map_or(2, |n| n.get().saturating_sub(1)).clamp(2, 8);
    let crt = tokio::runtime::Builder::new_multi_thread().worker_threads(threads).enable_all().build().unwrap();
    let term = new_term();
    let mode2 = mode.clone();
    let (bytes, secs) = crt.block_on(async move {
        let mode = mode2;
        let socket = tokio::net::TcpSocket::new_v4().unwrap();
        socket.set_recv_buffer_size(8 * 1024 * 1024).unwrap();
        let stream = socket.connect(([127, 0, 0, 1], port).into()).await.unwrap();
        stream.set_nodelay(true).unwrap();
        let config = Arc::new(russh::client::Config {
            inactivity_timeout: None,
            window_size: window,
            maximum_packet_size: max_packet,
            preferred: Preferred { cipher: Cow::Owned(cipher_list), ..Preferred::DEFAULT },
            ..Default::default()
        });
        let mut handle = russh::client::connect_stream(config, stream, Client).await.unwrap();
        assert!(handle.authenticate_password("bench", "x").await.unwrap().success());
        let mut channel = handle.channel_open_session().await.unwrap();
        channel.request_pty(false, "xterm-256color", 177, 42, 0, 0, &[]).await.unwrap();
        channel.request_shell(true).await.unwrap();
        let mut parser: Processor = Processor::new();
        let mut total = 0usize;
        let mut msgs = 0usize;
        let mut batches = 0usize;
        let ui = mode.ends_with("-ui");
        let base = mode.trim_end_matches("-ui").to_string();
        let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let ui_thread = ui.then(|| {
            let term = term.clone();
            let stop = stop.clone();
            std::thread::spawn(move || {
                let mut frames = 0usize;
                while !stop.load(std::sync::atomic::Ordering::Relaxed) {
                    std::thread::sleep(Duration::from_micros(16_667));
                    let term = term.lock();
                    let mut cells = 0usize;
                    for indexed in term.renderable_content().display_iter {
                        cells += indexed.cell.c as usize & 1;
                    }
                    std::hint::black_box(cells);
                    frames += 1;
                }
                frames
            })
        });
        let t = Instant::now();
        let mut pending: Vec<bytes::Bytes> = Vec::new();
        let mut closed = false;
        while !closed {
            match channel.wait().await {
                Some(ChannelMsg::Data { data }) => pending.push(data),
                Some(ChannelMsg::Close) | None => break,
                Some(_) => continue,
            }
            if base == "batch" {
                // Take whatever is already queued, without waiting.
                loop {
                    match tokio::time::timeout(Duration::ZERO, channel.wait()).await {
                        Ok(Some(ChannelMsg::Data { data })) => pending.push(data),
                        Ok(Some(ChannelMsg::Close)) | Ok(None) => { closed = true; break }
                        Ok(Some(_)) => {}
                        Err(_) => break,
                    }
                    if pending.len() >= 64 { break }
                }
            }
            batches += 1;
            msgs += pending.len();
            match base.as_str() {
                "discard" => {
                    for d in &pending { total += d.len(); std::hint::black_box(d); }
                }
                "parse" | "batch" => {
                    let mut term = term.lock();
                    for d in &pending { total += d.len(); parser.advance(&mut *term, d); }
                }
                _ => panic!("mode {mode}"),
            }
            pending.clear();
        }
        stop.store(true, std::sync::atomic::Ordering::Relaxed);
        if let Some(h) = ui_thread { eprintln!("  ui frames {}", h.join().unwrap()); }
        eprintln!("  batches {batches}");
        let secs = t.elapsed().as_secs_f64();
        eprintln!("  {msgs} data messages, avg {} B", total / msgs.max(1));
        (total, secs)
    });
    println!(
        "{cipher_name:<10} {mode:<8} pkt {max_packet:>6} win {:>5} KiB: {:.1} MB in {secs:.3} s = {:.0} MB/s",
        window / 1024,
        bytes as f64 / 1e6,
        bytes as f64 / 1e6 / secs
    );
}
