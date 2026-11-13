//! Development benchmark server: an SSH server on 127.0.0.1 that floods each shell with coloured
//! log lines and reports how long the terminal took to show them, for measuring the whole app
//! (rendering included) with `examples/demo.rs`.
//!
//! Each shell waits for the window to settle, prints the lines in 64 KiB blocks as fast as SSH
//! flow control allows, then asks for the cursor position (`CSI 6 n`). The terminal answers only
//! once it has parsed everything before the question, so the time until the answer is the time
//! the terminal took to take in all of the output.
//!
//! ```text
//! cargo run --release --example logserver -- <dir> [lines] [runs]
//! ```
//!
//! It writes `client_ed25519` and `host_ed25519.pub` into `<dir>` for `examples/demo.rs`
//! (`KOMANDO_DEMO_KEY`, `KOMANDO_DEMO_HOST_KEY`) and listens on `KOMANDO_DEMO_PORT` (2222).
//! Any key and any password are accepted.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use russh::keys::ssh_key::LineEnding;
use russh::keys::ssh_key::private::Ed25519Keypair;
use russh::keys::{PrivateKey, PublicKey};
use russh::server::{self, Msg, Server as _, Session};
use russh::{Channel, ChannelId, ChannelMsg};

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

struct LogServer {
    lines: u64,
    runs: usize,
    channel: Option<Channel<Msg>>,
}

impl server::Server for LogServer {
    type Handler = Self;
    fn new_client(&mut self, _: Option<std::net::SocketAddr>) -> Self {
        Self { lines: self.lines, runs: self.runs, channel: None }
    }
}

impl server::Handler for LogServer {
    type Error = russh::Error;

    async fn auth_password(&mut self, _: &str, _: &str) -> Result<server::Auth, Self::Error> {
        Ok(server::Auth::Accept)
    }

    async fn auth_publickey(&mut self, _: &str, _: server::VerifiedPublicKey<'_>) -> Result<server::Auth, Self::Error> {
        Ok(server::Auth::Accept)
    }

    async fn channel_open_session(
        &mut self,
        channel: Channel<Msg>,
        reply: server::ChannelOpenHandle,
        _: &mut Session,
    ) -> Result<(), Self::Error> {
        self.channel = Some(channel);
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

    /// Logged, to see when a resize reaches the server.
    async fn window_change_request(
        &mut self,
        _: ChannelId,
        cols: u32,
        rows: u32,
        _: u32,
        _: u32,
        _: &mut Session,
    ) -> Result<(), Self::Error> {
        eprintln!("{:?} resized to {cols} x {rows}", std::time::SystemTime::now());
        Ok(())
    }

    async fn shell_request(&mut self, id: ChannelId, session: &mut Session) -> Result<(), Self::Error> {
        session.channel_success(id)?;
        let Some(mut channel) = self.channel.take() else { return Ok(()) };
        let (lines, runs) = (self.lines, self.runs);
        tokio::spawn(async move {
            let mut times = Vec::new();
            for run in 1..=runs {
                tokio::time::sleep(Duration::from_secs(2)).await;
                let start = Instant::now();
                let mut bytes = 0;
                let mut block = Vec::with_capacity(70 * 1024);
                for i in 0..lines {
                    log_line(i, &mut block);
                    if block.len() >= 64 * 1024 || i + 1 == lines {
                        bytes += block.len();
                        if channel.data(&block[..]).await.is_err() {
                            return;
                        }
                        block.clear();
                    }
                }
                let sent = start.elapsed();
                let _ = channel.data(&b"\x1b[6n"[..]).await;
                loop {
                    match channel.wait().await {
                        Some(ChannelMsg::Data { data }) if data.ends_with(b"R") => break,
                        Some(_) => {}
                        None => return,
                    }
                }
                let secs = start.elapsed().as_secs_f64();
                eprintln!(
                    "run {run}: {lines} lines, {:.1} MB in {secs:.3} s = {:.1} MB/s (all sent after {:.3} s)",
                    bytes as f64 / 1e6,
                    bytes as f64 / 1e6 / secs,
                    sent.as_secs_f64()
                );
                times.push(secs);
            }
            times.sort_by(f64::total_cmp);
            eprintln!("median {:.3} s", times[times.len() / 2]);
            let summary = format!("\r\n\x1b[1mlogserver: median {:.3} s over {runs} runs\x1b[0m\r\n", times[times.len() / 2]);
            let _ = channel.data(summary.as_bytes()).await;
        });
        Ok(())
    }
}

#[tokio::main]
async fn main() {
    let mut args = std::env::args().skip(1);
    let dir = PathBuf::from(args.next().expect("usage: logserver <dir> [lines] [runs]"));
    let lines = args.next().map(|v| v.parse().unwrap()).unwrap_or(1_000_000);
    let runs = args.next().map(|v| v.parse().unwrap()).unwrap_or(3);
    let port: u16 = std::env::var("KOMANDO_DEMO_PORT").map(|p| p.parse().unwrap()).unwrap_or(2222);

    std::fs::create_dir_all(&dir).unwrap();
    let host = PrivateKey::from(Ed25519Keypair::from_seed(&[7; 32]));
    let client = PrivateKey::from(Ed25519Keypair::from_seed(&[9; 32]));
    std::fs::write(dir.join("client_ed25519"), client.to_openssh(LineEnding::LF).unwrap().as_bytes()).unwrap();
    let host_pub: &PublicKey = host.public_key();
    std::fs::write(dir.join("host_ed25519.pub"), host_pub.to_openssh().unwrap()).unwrap();
    std::fs::write(dir.join("host_ed25519_private"), host.to_openssh(LineEnding::LF).unwrap().as_bytes()).unwrap();

    let config = Arc::new(server::Config {
        keys: vec![host],
        auth_rejection_time: Duration::ZERO,
        auth_rejection_time_initial: Some(Duration::ZERO),
        inactivity_timeout: None,
        nodelay: true,
        ..Default::default()
    });
    eprintln!("logserver on 127.0.0.1:{port}, {lines} lines x {runs} runs per shell");
    let mut server = LogServer { lines, runs, channel: None };
    server.run_on_address(config, ("127.0.0.1", port)).await.unwrap();
}
