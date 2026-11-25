//! SSH Sessions with the embedded client (russh); see ADR 0002.
//!
//! Network bytes go straight into the terminal parser on a tokio worker. No local shell and
//! no ConPTY are involved.
//!
//! A Host behind Jump Hosts is reached hop by hop: Komando signs in to the first Jump Host,
//! asks it for a tunnel (`direct-tcpip`) to the next one, runs a new SSH connection inside that
//! tunnel, and so on until the target. Each hop checks its own Host Key and signs in with its
//! own Credential, exactly as if it were opened directly.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use alacritty_terminal::event::{Event, EventListener};
use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::index::Line;
use alacritty_terminal::term::Term;
use alacritty_terminal::term::cell::Flags;
use alacritty_terminal::term::TermMode;
use alacritty_terminal::vte::ansi::Processor;
use anyhow::{Context as _, Result, anyhow};
use futures::channel::oneshot;
use russh::client::{self, AuthResult, Handle};
use russh::keys::{HashAlg, PrivateKeyWithHashAlg, PublicKey, PublicKeyOrCertificate};
use russh::client::KeyboardInteractiveAuthResponse;
use russh::{ChannelMsg, Disconnect, MethodKind, MethodSet, Pty};
use tokio::sync::mpsc;

use super::agent::Agent;
use super::pace::Pace;
use super::sudo::SudoPassword;
use super::{Answer, EventProxy, GridSize, HopStep, InputSink, Prompt, Remembered, SessionEvent, SharedTerm, Status};
use crate::model::AgentUse;
use crate::vault::Secret;
use zeroize::Zeroizing;

pub enum Command {
    Input(Vec<u8>),
    Resize(GridSize),
    /// Open an SFTP channel on this Session's connection (no second sign-in).
    OpenSftp(oneshot::Sender<Result<crate::sftp::Remote>>),
    /// Open a tunnel on this Session's connection to `port` on the Host itself, for a Remote
    /// Desktop reached through SSH.
    OpenTunnel { port: u16, reply: oneshot::Sender<Result<crate::desktop::Stream>> },
    Close,
}

/// Everything a connection needs, resolved on the UI thread before it starts.
#[derive(Default)]
pub struct Target {
    /// The Hosts to sign in to, in order: Jump Hosts first, the Host with the shell last.
    pub hops: Vec<Hop>,
    /// Komando's own trusted Host Keys.
    pub known_hosts: PathBuf,
    /// Port Forwards opened on the last hop's connection once every hop has signed in.
    pub forwards: Vec<crate::model::PortForward>,
    /// The SSH Agent every hop may sign in with; `None`: no agent.
    pub agent: Option<Agent>,
    /// Which of the agent's keys the Host itself (the last hop) signs in with. Jump Hosts
    /// offer every key, as `ssh -J` does.
    pub host_agent: AgentUse,
    /// Agent Forwarding to the Host itself. Never to its Jump Hosts.
    pub forward_agent: bool,
}

/// One Host on the way.
pub struct Hop {
    pub name: String,
    pub address: String,
    pub port: u16,
    pub user: Option<String>,
    /// The saved Credential's secret, if the Vault is unlocked and one is configured.
    pub secret: Option<Secret>,
}

/// What a connection log line reports, which sets its colour.
#[derive(Clone, Copy, PartialEq, Debug)]
pub(crate) enum LogKind {
    Step,
    Good,
    Warn,
    Fail,
}

/// The connection's progress, written into the terminal above where the shell will start:
/// where it is connecting, the Host Key, how it signs in. Once every hop has signed in it is
/// erased, so the shell starts on a clean screen. A connection that fails keeps it, to show
/// why, until an attempt after it signs in: that erases the failed attempts' lines with its own.
#[derive(Clone)]
pub(crate) struct ConnectLog {
    term: SharedTerm,
    proxy: EventProxy,
    /// Lines written since the last `clear`, by this attempt and earlier ones in the terminal.
    lines: Arc<AtomicUsize>,
    /// This attempt's shell opened: its end is told below the shell's output (see `ended`).
    shell_open: Arc<AtomicBool>,
    /// How the last open shell ended, shared by every attempt in this terminal: the attempt
    /// that signs in again writes it, with the time it reconnected, where its log was.
    last_end: Arc<std::sync::Mutex<Option<Ended>>>,
}

/// How an open shell's connection ended, and when.
pub(crate) struct Ended {
    kind: LogKind,
    reason: String,
    at: chrono::DateTime<chrono::Local>,
}

impl ConnectLog {
    pub(crate) fn new(term: SharedTerm, proxy: EventProxy) -> Self {
        Self { term, lines: proxy.connect_log_lines(), last_end: proxy.connect_ended(), proxy, shell_open: Arc::default() }
    }

    /// The shell opened: from now on the Session is connected.
    pub(crate) fn shell_opened(&self) {
        self.shell_open.store(true, Ordering::Relaxed);
    }

    pub(crate) fn is_shell_open(&self) -> bool {
        self.shell_open.load(Ordering::Relaxed)
    }

    /// The connection of an open shell ended: say so on a line of its own below the output,
    /// with how to reconnect, so the last prompt and the cursor no longer look ready for
    /// input. A program that was running can't undo what it switched on any more, so the
    /// terminal leaves an editor's alternate screen and drops mouse reporting and the like;
    /// the next shell starts from plain settings. The lines are part of the log, so the next
    /// attempt erases them once it has signed in, and leaves for good when it disconnected and
    /// reconnected in their place (see `clear`).
    pub(crate) fn ended(&self, kind: LogKind, reason: &str) {
        *self.last_end.lock().unwrap() = Some(Ended { kind, reason: reason.to_string(), at: chrono::Local::now() });
        {
            let mut term = self.term.lock();
            let mut parser: Processor = Processor::new();
            if term.mode().contains(TermMode::ALT_SCREEN) {
                parser.advance(&mut *term, b"\x1b[?1049l");
            }
            // Cursor shown, normal cursor and keypad keys, no mouse reporting, no bracketed paste.
            parser.advance(&mut *term, b"\x1b[0m\x1b[?25h\x1b[?1l\x1b>\x1b[?1000l\x1b[?1002l\x1b[?1003l\x1b[?1006l\x1b[?2004l");
            if term.grid().cursor.point.column.0 > 0 {
                parser.advance(&mut *term, b"\r\n");
            }
        }
        self.line(kind, &format!("Disconnected: {reason}"));
        self.line(LogKind::Step, "Press Enter to reconnect");
    }

    pub(crate) fn line(&self, kind: LogKind, text: &str) {
        let mut parser: Processor = Processor::new();
        parser.advance(&mut *self.term.lock(), log_line(kind, text).as_bytes());
        self.lines.fetch_add(1, Ordering::Relaxed);
        self.proxy.send_event(Event::Wakeup);
    }

    /// Signed in: erase the lines written so far, as far as they are still on screen. If an
    /// open shell's connection ended before, say for good when it dropped and when it came
    /// back, so the output shows where the reconnect happened.
    pub(crate) fn clear(&self) {
        let lines = self.lines.swap(0, Ordering::Relaxed);
        let mut term = self.term.lock();
        let rows = rows_above_cursor(&*term, lines);
        let mut parser: Processor = Processor::new();
        if rows > 0 {
            // Back to the first log row, then erase from there to the end of the screen.
            parser.advance(&mut *term, format!("\r\x1b[{rows}A\x1b[J").as_bytes());
        }
        let ended = self.last_end.lock().unwrap().take();
        if let Some(Ended { kind, reason, at }) = &ended {
            // Not counted as log lines, so no later sign-in erases them.
            let when = at.format("%H:%M:%S");
            parser.advance(&mut *term, log_line(*kind, &format!("Disconnected at {when}: {reason}")).as_bytes());
            let now = chrono::Local::now().format("%H:%M:%S");
            parser.advance(&mut *term, log_line(LogKind::Good, &format!("Reconnected at {now}")).as_bytes());
        }
        drop(term);
        if rows > 0 || ended.is_some() {
            self.proxy.send_event(Event::Wakeup);
        }
    }
}

/// How many screen rows the last `lines` lines above the cursor take up. A line too long for
/// the terminal wraps onto more than one row; the count stops at the top of the screen, as
/// rows that have scrolled into the Scrollback can't be erased.
fn rows_above_cursor<T>(term: &Term<T>, lines: usize) -> usize {
    let grid = term.grid();
    let mut left = lines;
    let mut rows = 0;
    let mut line = grid.cursor.point.line.0 - 1;
    while left > 0 && line >= 0 {
        rows += 1;
        let continues = line > 0 && grid[Line(line - 1)][grid.last_column()].flags.contains(Flags::WRAPLINE);
        if !continues {
            left -= 1;
        }
        line -= 1;
    }
    rows
}

/// One log line as terminal output: a muted "SSH" tag, a marker coloured by `kind`, and the
/// text. Plain ANSI colours, so it follows the terminal theme.
fn log_line(kind: LogKind, text: &str) -> String {
    let (color, mark) = match kind {
        LogKind::Step => ("2", "·"),
        LogKind::Good => ("32", "✓"),
        LogKind::Warn => ("33", "!"),
        LogKind::Fail => ("31", "✕"),
    };
    // Control characters from a server or a Host's name must not drive the terminal.
    let text: String = text.chars().map(|c| if c.is_control() { ' ' } else { c }).collect();
    format!("\x1b[0m\x1b[90mSSH\x1b[0m \x1b[{color}m{mark}\x1b[0m {text}\x1b[0m\r\n")
}

/// The error when the user closes a sign-in question (user name, password, key passphrase).
const CANCELLED: &str = "Cancelled.";

fn is_cancelled(err: &anyhow::Error) -> bool {
    err.root_cause().to_string() == CANCELLED
}

/// `name (address:port)`, or just `address:port` when the name is the address.
fn describe(hop: &Hop) -> String {
    let at = format!("{}:{}", hop.address, hop.port);
    if hop.name.is_empty() || hop.name == hop.address { at } else { format!("{} ({at})", hop.name) }
}

/// How long reaching one hop may take, Host Key questions aside.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);

/// How long signing in to one hop may keep waiting on the server, time spent on the user's
/// answers (a password, a code, an SSH agent's approval) aside. Longer than
/// [`CONNECT_TIMEOUT`]: servers pause after a wrong password, and some look users up slowly.
const SIGN_IN_TIMEOUT: Duration = Duration::from_secs(30);

/// How long ending a connection may take before it is dropped anyway.
const DISCONNECT_TIMEOUT: Duration = Duration::from_secs(5);

/// When reaching a hop started counting towards [`CONNECT_TIMEOUT`]: `None` while a Host Key
/// question waits for the user, who may take longer than that to compare a fingerprint. A VNC
/// Remote Desktop's handshake uses it the same way around its sign-in prompt.
#[derive(Clone)]
pub(crate) struct ConnectClock(Arc<std::sync::Mutex<Option<Instant>>>);

impl ConnectClock {
    pub(crate) fn new() -> Self {
        Self(Arc::new(std::sync::Mutex::new(Some(Instant::now()))))
    }

    fn set(&self, started: Option<Instant>) {
        *self.0.lock().unwrap_or_else(|e| e.into_inner()) = started;
    }

    fn started(&self) -> Option<Instant> {
        *self.0.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Stop counting until [`Self::resume`]: a question is on screen.
    pub(crate) fn pause(&self) {
        self.set(None);
    }

    /// Count again from now, with the whole timeout ahead.
    pub(crate) fn resume(&self) {
        self.set(Some(Instant::now()));
    }

    /// `future`, with the clock stopped while it runs: it waits on the user.
    pub(crate) async fn paused<T>(&self, future: impl Future<Output = T>) -> T {
        self.pause();
        let out = future.await;
        self.resume();
        out
    }

    /// `future`'s output, or `None` once it took longer than [`CONNECT_TIMEOUT`] while not
    /// paused.
    pub(crate) async fn limit<T>(&self, timeout: Duration, future: impl Future<Output = T>) -> Option<T> {
        self.resume();
        let mut future = std::pin::pin!(future);
        loop {
            // While paused, look again after a whole timeout: answering resumes the clock,
            // and the next wait then ends when its timeout does.
            let wake = self.started().map_or(Instant::now() + timeout, |started| started + timeout);
            tokio::select! {
                out = &mut future => return Some(out),
                _ = tokio::time::sleep_until(wake.into()) => {
                    if self.started().is_some_and(|started| started.elapsed() >= timeout) {
                        return None;
                    }
                }
            }
        }
    }
}

/// How much of the server's output the operating system may hold for Komando: as much as the
/// SSH window of a direct connection lets the server send ahead ([`WINDOW`], and the most the
/// shell's paced window grows to there; see `pace`).
///
/// With Windows' default receive buffer, output floods stalled: a server writing faster than the
/// buffer drained waited about one timer tick (15 ms) every couple of hundred kilobytes. Against
/// an asyncssh server on the same machine, 1,000,000 log lines took 3.5 s with the default and
/// 1.1 s with this buffer, with no stalls left.
const RECEIVE_BUFFER: u32 = 8 * 1024 * 1024;

/// For benchmarks that compare with how Komando used to connect: every channel at this fixed
/// SSH window, no pacing, and the receive buffer set (`true`) or left to the operating system.
/// Only `tests/throughput.rs` sets it; the app leaves it `None`.
#[doc(hidden)]
pub static FIXED_WINDOW: std::sync::Mutex<Option<(u32, bool)>> = std::sync::Mutex::new(None);

/// How much a server may send on a channel before Komando acknowledges it: the SSH window.
/// russh tops a channel's window up only once half of it is used, so about half of it is on
/// its way per round trip: this caps a Session's output at about 80 MB/s at a round trip of
/// [`WINDOW_ROUND_TRIP`]. The default 2 MiB capped an SFTP download at 40 MB/s at 50 ms before
/// anything else did. `RECEIVE_BUFFER` matches it.
const WINDOW: u32 = 8 * 1024 * 1024;
/// The round trip [`WINDOW`] is meant for.
const WINDOW_ROUND_TRIP: Duration = Duration::from_millis(50);
/// The largest window: for the Jump Hosts' connections, and for a Host far away behind them.
///
/// A Jump Host's connection only carries the tunnel to the next hop, whose own window already
/// limits what is on its way, so a large one holds nothing more back; a smaller one would
/// throttle the connection inside.
const MAX_WINDOW: u32 = 32 * 1024 * 1024;

/// The window for a Host behind Jump Hosts whose round trip from here is `round_trip`: one
/// [`WINDOW`] for every whole [`WINDOW_ROUND_TRIP`], up to [`MAX_WINDOW`].
///
/// Every hop adds its own way there and back, and russh can't grow a window once connected: at
/// a fixed 8 MiB, a Host 25 ms beyond a Jump Host that is 25 ms away printed half as fast as the
/// Jump Host itself would. In whole steps, a Host next to its Jump Host keeps the usual window:
/// a larger one only holds more output on its way, which Ctrl+C then has to wait for.
fn window_for(round_trip: Duration) -> u32 {
    let steps = (round_trip.as_millis() / WINDOW_ROUND_TRIP.as_millis()).clamp(1, (MAX_WINDOW / WINDOW) as u128);
    WINDOW * steps as u32
}

/// The client settings for one hop's connection, with a window of `window` bytes: what its SFTP
/// channels and, on a Jump Host's connection, the tunnel onward get. The shell's channel has a
/// paced window of its own instead (see `shell`). With `certificates`, the server is asked for
/// a host certificate ahead of a plain key: only when a trusted authority covers the host,
/// since a certificate from anyone else would be refused (see `host_cert`).
fn client_config(window: u32, certificates: bool) -> Arc<client::Config> {
    let window = FIXED_WINDOW.lock().unwrap().map_or(window, |(window, _)| window);
    Arc::new(client::Config {
        inactivity_timeout: None,
        keepalive_interval: Some(Duration::from_secs(30)),
        keepalive_max: 4,
        window_size: window,
        preferred: russh::Preferred {
            kex: std::borrow::Cow::Borrowed(KEX),
            cipher: std::borrow::Cow::Borrowed(ciphers()),
            mac: std::borrow::Cow::Borrowed(MACS),
            host_key_certificates: if certificates { russh::Preferred::DEFAULT.key } else { std::borrow::Cow::Borrowed(&[]) },
            ..Default::default()
        },
        ..Default::default()
    })
}

/// The key exchanges to offer, best first: russh's defaults, then what network gear such as
/// Cisco switches offers instead (NIST-curve ECDH, and Diffie-Hellman group 14 with SHA-1 on
/// older firmware). The server picks the first of ours it also has, so a server with a modern
/// key exchange still gets it, and the exchange hash protects the choice from tampering.
/// Last, for the oldest firmware: group exchange with SHA-1, and group 1 (1024-bit, weak, but
/// all some switches, UPS cards and out-of-band controllers have).
const KEX: &[russh::kex::Name] = {
    use russh::kex::*;
    &[
        MLKEM768X25519_SHA256,
        CURVE25519,
        CURVE25519_PRE_RFC_8731,
        DH_GEX_SHA256,
        DH_G18_SHA512,
        DH_G17_SHA512,
        DH_G16_SHA512,
        DH_G15_SHA512,
        DH_G14_SHA256,
        ECDH_SHA2_NISTP256,
        ECDH_SHA2_NISTP384,
        ECDH_SHA2_NISTP521,
        DH_G14_SHA1,
        DH_GEX_SHA1,
        DH_G1_SHA1,
        EXTENSION_SUPPORT_AS_CLIENT,
        EXTENSION_OPENSSH_STRICT_KEX_AS_CLIENT,
    ]
};

/// The MACs to offer, best first: russh's defaults, then HMAC-SHA1, the only one older Cisco
/// firmware has. Only used with a cipher that isn't AEAD (CTR or CBC).
const MACS: &[russh::mac::Name] = {
    use russh::mac::*;
    &[HMAC_SHA512_ETM, HMAC_SHA256_ETM, HMAC_SHA512, HMAC_SHA256, HMAC_SHA1_ETM, HMAC_SHA1]
};

/// The ciphers to offer, best first. russh puts ChaCha20-Poly1305 first, as OpenSSH does; with
/// AES instructions in the CPU (every PC that runs Windows 11 has them), AES-GCM decrypts
/// several times faster and is just as safe, so it goes first there. The server still picks
/// ChaCha20 if it doesn't offer AES-GCM.
fn ciphers() -> &'static [russh::cipher::Name] {
    use russh::cipher::{
        AES_128_CBC, AES_128_CTR, AES_128_GCM, AES_192_CBC, AES_192_CTR, AES_256_CBC, AES_256_CTR, AES_256_GCM,
        CHACHA20_POLY1305,
    };
    #[cfg(target_arch = "x86_64")]
    if std::arch::is_x86_feature_detected!("aes") && std::arch::is_x86_feature_detected!("pclmulqdq") {
        return &[
            AES_256_GCM, AES_128_GCM, CHACHA20_POLY1305, AES_256_CTR, AES_192_CTR, AES_128_CTR,
            AES_256_CBC, AES_192_CBC, AES_128_CBC,
        ];
    }
    &[
        CHACHA20_POLY1305, AES_256_GCM, AES_128_GCM, AES_256_CTR, AES_192_CTR, AES_128_CTR,
        AES_256_CBC, AES_192_CBC, AES_128_CBC,
    ]
}

/// A TCP connection to `address:port`, trying each address it resolves to in turn, with a
/// receive buffer of [`RECEIVE_BUFFER`].
async fn open_tcp(address: &str, port: u16) -> std::io::Result<tokio::net::TcpStream> {
    let mut last_error = None;
    for addr in tokio::net::lookup_host((address, port)).await? {
        let socket = if addr.is_ipv4() { tokio::net::TcpSocket::new_v4()? } else { tokio::net::TcpSocket::new_v6()? };
        let set_buffer = FIXED_WINDOW.lock().unwrap().is_none_or(|(_, set_buffer)| set_buffer);
        // A smaller buffer only costs speed, so a refusal is not an error.
        if set_buffer && let Err(err) = socket.set_recv_buffer_size(RECEIVE_BUFFER) {
            log::warn!("setting the TCP receive buffer: {err}");
        }
        match socket.connect(addr).await {
            Ok(stream) => {
                // Keystrokes go out at once rather than waiting to be batched.
                stream.set_nodelay(true)?;
                return Ok(stream);
            }
            Err(err) => last_error = Some(err),
        }
    }
    Err(last_error.unwrap_or_else(|| std::io::Error::new(std::io::ErrorKind::NotFound, format!("{address} has no address"))))
}

/// Threads for SSH and SFTP: one per CPU but one, which is left for the UI, between 2 and 8.
///
/// Output is parsed on these threads, so they bound how much output all Sessions together take
/// in, and how soon a key typed in one Session shows while others print. With 8 Sessions each
/// printing 500,000 log lines, 2 threads took 2.3 s in all and delayed a typed key by up to 24 ms;
/// 8 threads took 0.95 s and at most 2.4 ms.
fn ssh_threads() -> usize {
    std::thread::available_parallelism().map_or(2, |n| n.get().saturating_sub(1)).clamp(2, 8)
}

pub(crate) fn runtime() -> &'static tokio::runtime::Runtime {
    static RUNTIME: OnceLock<tokio::runtime::Runtime> = OnceLock::new();
    RUNTIME.get_or_init(|| {
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(ssh_threads())
            .thread_name("komando-ssh")
            .enable_all()
            .build()
            .expect("tokio runtime")
    })
}

pub fn spawn(target: Target, size: GridSize, term: SharedTerm, proxy: EventProxy) -> InputSink {
    let (tx, rx) = mpsc::unbounded_channel();
    let sink = InputSink::Ssh(tx);
    proxy.set_responder(sink.clone());
    runtime().spawn(async move {
        proxy.send(SessionEvent::Status(Status::Connecting));
        let log = ConnectLog::new(term.clone(), proxy.clone());
        let (reason, kind) = match run(target, size, term, proxy.clone(), log.clone(), rx).await {
            Ok(reason) => (reason, LogKind::Step),
            Err(err) => {
                let reason = format!("{err:#}");
                if !log.is_shell_open() {
                    log.line(LogKind::Fail, &reason);
                    // Enter reconnects a disconnected Session: say so where the user is looking.
                    if is_cancelled(&err) {
                        log.line(LogKind::Step, "Press Enter to reconnect");
                    }
                }
                (reason, LogKind::Fail)
            }
        };
        if log.is_shell_open() {
            log.ended(kind, &reason);
        }
        proxy.send(SessionEvent::Status(Status::Disconnected(reason)));
    });
    sink
}

pub(crate) struct Client {
    host: String,
    port: u16,
    /// The Certificate Authorities trusted for this host (see `host_cert`): a host certificate
    /// signed by one of them is trusted without asking.
    authorities: Vec<PublicKey>,
    known_hosts: PathBuf,
    proxy: EventProxy,
    log: ConnectLog,
    /// Which hop this connection is, for progress events.
    hop: usize,
    /// Paused while the Host Key question waits for an answer.
    clock: ConnectClock,
    /// The Remote forwards this connection asked the server for. Empty on Jump Hosts.
    remote_forwards: super::forward::RemoteForwards,
    /// Agent Forwarding is on for this connection: the agent its channels go to. Only ever
    /// the Host's own connection.
    forward_agent: Option<Agent>,
    /// The connection's key exchange, for the agent to know which server forwarded requests
    /// come through. Set once connected.
    binding: Arc<OnceLock<russh::keys::agent::AgentSessionBinding>>,
}

impl client::Handler for Client {
    type Error = anyhow::Error;

    /// The server opens a channel to the SSH Agent (Agent Forwarding): carried to the agent
    /// here when forwarding is on for this Host, refused otherwise.
    async fn server_channel_open_agent_forward(
        &mut self,
        channel: russh::Channel<client::Msg>,
        reply: client::ChannelOpenHandle,
        _session: &mut client::Session,
    ) -> Result<(), Self::Error> {
        match self.forward_agent.clone() {
            Some(agent) => {
                reply.accept().await;
                let binding = self.binding.get().cloned();
                tokio::spawn(async move { agent.relay(channel, binding).await });
            }
            None => reply.reject(russh::ChannelOpenFailure::AdministrativelyProhibited).await,
        }
        Ok(())
    }

    /// A connection to a Remote forward's port on the server: carried to its address here.
    /// One for a port Komando didn't ask for is refused.
    async fn server_channel_open_forwarded_tcpip(
        &mut self,
        channel: russh::Channel<client::Msg>,
        _connected_address: &str,
        connected_port: u32,
        _originator_address: &str,
        _originator_port: u32,
        reply: client::ChannelOpenHandle,
        _session: &mut client::Session,
    ) -> Result<(), Self::Error> {
        if self.remote_forwards.accept(connected_port, Box::new(channel.into_stream())) {
            reply.accept().await;
        } else {
            reply.reject(russh::ChannelOpenFailure::AdministrativelyProhibited).await;
        }
        Ok(())
    }

    async fn check_server_key(&mut self, server_key: &PublicKeyOrCertificate) -> Result<bool, Self::Error> {
        let key: &PublicKey = match server_key {
            PublicKeyOrCertificate::PublicKey { key, .. } => key,
            PublicKeyOrCertificate::Certificate(cert) => return self.check_certificate(cert),
        };
        let fingerprint = key.fingerprint(HashAlg::Sha256).to_string();
        self.proxy.send(SessionEvent::Hop { index: self.hop, step: HopStep::CheckingKey });
        self.log.line(LogKind::Step, &format!("Host key {} {fingerprint}", key.algorithm()));

        // Komando's own list first, then the user's OpenSSH known_hosts (read-only import).
        let mut sources = vec![self.known_hosts.clone()];
        if let Some(home) = dirs::home_dir() {
            sources.push(home.join(".ssh").join("known_hosts"));
        }
        for path in &sources {
            if !path.exists() {
                continue;
            }
            match russh::keys::check_known_hosts_path(&self.host, self.port, key, path) {
                Ok(true) => {
                    self.log.line(LogKind::Good, "Host key matches the trusted one");
                    if path != &self.known_hosts {
                        let _ = learn_known_host(&self.host, self.port, key, &self.known_hosts);
                    }
                    return Ok(true);
                }
                Ok(false) => {}
                Err(russh::keys::Error::KeyChanged { .. }) => {
                    self.log.line(LogKind::Fail, "Host key changed since you trusted it: waiting for you to decide");
                    let previous = russh::keys::known_hosts::known_host_keys_path(&self.host, self.port, path)
                        .ok()
                        .and_then(|keys| keys.into_iter().find(|(_, old)| old.algorithm() == key.algorithm()))
                        .map(|(_, old)| old.fingerprint(HashAlg::Sha256).to_string());
                    let (reply, answer) = oneshot::channel();
                    self.proxy.send(SessionEvent::Prompt(Prompt::HostKeyChanged {
                        fingerprint,
                        previous,
                        algorithm: key.algorithm().to_string(),
                        openssh: path != &self.known_hosts,
                        reply,
                    }));
                    self.clock.pause();
                    let trusted = answer.await.unwrap_or(false);
                    self.clock.resume();
                    if !trusted {
                        return Err(anyhow!(
                            "The host key of {} has changed. Connection blocked: this can mean someone is intercepting the connection.",
                            self.host
                        ));
                    }
                    replace_known_host(&self.host, self.port, key, &self.known_hosts)?;
                    self.log.line(LogKind::Warn, "Old host key replaced: the new one is trusted from now on");
                    return Ok(true);
                }
                Err(err) => log::warn!("reading {}: {err}", path.display()),
            }
        }

        // Keys of other types already trusted for this host: russh reports those as unknown
        // rather than changed, but the user should not be told this is a first connection.
        let mut other_types: Vec<String> = sources
            .iter()
            .filter_map(|path| russh::keys::known_hosts::known_host_keys_path(&self.host, self.port, path).ok())
            .flatten()
            .map(|(_, known)| known.algorithm().to_string())
            .filter(|algorithm| *algorithm != key.algorithm().to_string())
            .collect();
        other_types.sort();
        other_types.dedup();
        if other_types.is_empty() {
            self.log.line(LogKind::Warn, "First connection to this host: waiting for you to trust its key");
        } else {
            self.log.line(LogKind::Warn, "This host presented a key of a type not trusted before: waiting for you to decide");
        }
        let (reply, answer) = oneshot::channel();
        self.proxy.send(SessionEvent::Prompt(Prompt::HostKey {
            fingerprint,
            algorithm: key.algorithm().to_string(),
            other_types,
            reply,
        }));
        self.clock.pause();
        let trusted = answer.await.unwrap_or(false);
        self.clock.resume();
        if trusted {
            learn_known_host(&self.host, self.port, key, &self.known_hosts)?;
            self.log.line(LogKind::Good, "Host key trusted");
            Ok(true)
        } else {
            Err(anyhow!("Host key not trusted."))
        }
    }
}

/// Add `key` for `host:port` to Komando's known_hosts at `path`, which only the user may read
/// on Linux and macOS: it lists every Host the user connects to.
fn learn_known_host(host: &str, port: u16, key: &PublicKey, path: &std::path::Path) -> Result<()> {
    if let Some(dir) = path.parent() {
        crate::model::create_private_dir(dir)?;
    }
    // Made private first: russh would create it readable by others.
    drop(crate::model::open_private_append(path).with_context(|| format!("Opening {}", path.display()))?);
    russh::keys::known_hosts::learn_known_hosts_path(host, port, key, path)?;
    Ok(())
}

/// Trust `key` as the only key of `host:port` in Komando's known_hosts at `path`: every entry
/// for that host is removed (as `ssh-keygen -R` does) and the new key added. Komando's list is
/// checked before the user's OpenSSH known_hosts, so a key that changed there is replaced too,
/// without touching that file.
fn replace_known_host(host: &str, port: u16, key: &PublicKey, path: &std::path::Path) -> Result<()> {
    let host_port = if port == 22 { host.to_string() } else { format!("[{host}]:{port}") };
    let existing = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(err) => return Err(err).with_context(|| format!("Reading {}", path.display())),
    };
    let mut kept = String::with_capacity(existing.len());
    for line in existing.lines() {
        let hosts = line.split_whitespace().next().unwrap_or("");
        if !hosts.starts_with('#') && hosts.split(',').any(|entry| entry == host_port) {
            continue;
        }
        kept.push_str(line);
        kept.push('\n');
    }
    if let Some(dir) = path.parent() {
        crate::model::create_private_dir(dir)?;
    }
    crate::model::write_atomic(path, kept.as_bytes())?;
    learn_known_host(host, port, key, path)?;
    // An entry this could not recognise (a hashed host name) would still hold the old key.
    match russh::keys::check_known_hosts_path(host, port, key, path) {
        Ok(true) => Ok(()),
        _ => Err(anyhow!(
            "Could not replace the old host key of {host}: remove its line from {} and connect again.",
            path.display()
        )),
    }
}

/// Ask the user, with `clock` stopped until they answer.
/// The `@cert-authority` lines of Komando's known_hosts and then the user's OpenSSH one.
fn known_authorities(own: &std::path::Path) -> Vec<super::host_cert::Authority> {
    let mut authorities = super::host_cert::read(own);
    if let Some(home) = dirs::home_dir() {
        authorities.extend(super::host_cert::read(&home.join(".ssh").join("known_hosts")));
    }
    authorities
}

impl Client {
    /// A host certificate: trusted when one of the host's authorities signed it for this host
    /// and it is in date. Otherwise the connection stops; there is no key to fall back on that
    /// the user could compare.
    fn check_certificate(&self, cert: &russh::keys::Certificate) -> Result<bool> {
        self.proxy.send(SessionEvent::Hop { index: self.hop, step: HopStep::CheckingKey });
        self.log.line(LogKind::Step, &format!("Host certificate {} (key ID \"{}\")", cert.algorithm(), cert.key_id()));
        let authorities: Vec<&PublicKey> = self.authorities.iter().collect();
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |since| since.as_secs());
        match super::host_cert::check(cert, &self.host, &authorities, now) {
            Ok(authority) => {
                self.log.line(LogKind::Good, &format!("Host certificate signed by the trusted authority {authority}"));
                Ok(true)
            }
            Err(why) => {
                self.log.line(LogKind::Fail, &format!("Host certificate refused: {why}"));
                Err(anyhow!(
                    "{} presented a host certificate that can't be trusted: {why}. Its authority is trusted by an @cert-authority line in known_hosts.",
                    self.host
                ))
            }
        }
    }
}

async fn ask(
    proxy: &EventProxy,
    clock: &ConnectClock,
    label: &str,
    secret: bool,
    remember: Option<bool>,
    error: Option<String>,
) -> Option<Answer> {
    let (reply, answer) = oneshot::channel();
    proxy.send(SessionEvent::Prompt(Prompt::Input { label: label.into(), secret, remember, error, reply }));
    clock.paused(answer).await.ok().flatten()
}

async fn run(
    target: Target,
    mut size: GridSize,
    term: SharedTerm,
    proxy: EventProxy,
    log: ConnectLog,
    mut commands: mpsc::UnboundedReceiver<Command>,
) -> Result<String> {
    // Every hop's connection stays open: the next one runs through it.
    let mut handles: Vec<Handle<Client>> = Vec::with_capacity(target.hops.len());
    let authorities = known_authorities(&target.known_hosts);
    // The last hop's user and the password it signed in with, offered at sudo prompts.
    let mut sudo = None;
    // The Host's connection window: the most its shell's paced window grows to.
    let mut host_window = WINDOW;
    let remote_forwards = super::forward::RemoteForwards::default();
    for (index, hop) in target.hops.iter().enumerate() {
        proxy.send(SessionEvent::Hop { index, step: HopStep::Reaching });
        match index {
            0 => log.line(LogKind::Step, &format!("Connecting to {}", describe(hop))),
            _ => log.line(LogKind::Step, &format!("Opening a tunnel to {} through {}", describe(hop), target.hops[index - 1].name)),
        }
        let clock = ConnectClock::new();
        let is_host = index + 1 == target.hops.len();
        let binding = Arc::new(OnceLock::new());
        let host_authorities: Vec<PublicKey> =
            super::host_cert::for_host(&authorities, &hop.address, hop.port).into_iter().cloned().collect();
        let certificates = !host_authorities.is_empty();
        let handler = Client {
            host: hop.address.clone(),
            port: hop.port,
            authorities: host_authorities,
            known_hosts: target.known_hosts.clone(),
            proxy: proxy.clone(),
            log: log.clone(),
            hop: index,
            clock: clock.clone(),
            remote_forwards: if is_host { remote_forwards.clone() } else { Default::default() },
            forward_agent: if is_host && target.forward_agent { target.agent.clone() } else { None },
            binding: binding.clone(),
        };
        let timed_out = || anyhow!("{}{}:{}.", super::TIMED_OUT, hop.address, hop.port);
        let is_jump = index + 1 < target.hops.len();
        let mut handle = match handles.last() {
            None => clock
                .limit(CONNECT_TIMEOUT, async {
                    let stream = open_tcp(&hop.address, hop.port).await?;
                    let window = if is_jump { MAX_WINDOW } else { WINDOW };
                    host_window = window;
                    client::connect_stream(client_config(window, certificates), stream, handler).await
                })
                .await
                .ok_or_else(timed_out)??,
            Some(through) => {
                let via = &target.hops[index - 1].name;
                let opening = std::time::Instant::now();
                let tunnel = tokio::time::timeout(
                    CONNECT_TIMEOUT,
                    through.channel_open_direct_tcpip(hop.address.as_str(), hop.port as u32, "127.0.0.1", 0),
                )
                .await
                .map_err(|_| timed_out())?
                .map_err(|err| anyhow!("{via} could not open a tunnel to {}:{}: {err}", hop.address, hop.port))?;
                // The Jump Host answers once its own connection onwards is up: about a round
                // trip from here to this hop.
                let round_trip = opening.elapsed();
                let window = if is_jump { MAX_WINDOW } else { window_for(round_trip) };
                host_window = window;
                log::debug!("tunnel to {} opened in {round_trip:?}: window {} KiB", describe(hop), window / 1024);
                clock
                    .limit(CONNECT_TIMEOUT, client::connect_stream(client_config(window, certificates), tunnel.into_stream(), handler))
                    .await
                    .ok_or_else(timed_out)??
            }
        };
        if let Some(context) = handle.agent_session_binding() {
            let _ = binding.set(context.clone());
        }
        let (user, remember_user) = match hop.user.clone() {
            Some(user) => (user, false),
            None => {
                log.line(LogKind::Step, "No user name saved for this host: waiting for one");
                let answer = ask(&proxy, &clock, &format!("User name for {}", hop.name), false, Some(false), None)
                    .await
                    .ok_or_else(|| anyhow!(CANCELLED))?;
                (answer.value, answer.remember)
            }
        };
        proxy.send(SessionEvent::Hop { index, step: HopStep::SigningIn });
        // Saved only once it signed in, so a mistyped user name is not remembered. Before the
        // password, so the password's vault entry is labelled with it.
        let agent_use = if is_host { target.host_agent.clone() } else { AgentUse::Any };
        let agent = target.agent.as_ref().filter(|_| agent_use != AgentUse::Off);
        let signed_in = clock
            .limit(SIGN_IN_TIMEOUT, authenticate(&mut handle, &user, &hop.address, hop.secret.as_ref(), agent, &agent_use, &proxy, &log, &clock))
            .await
            .ok_or_else(|| anyhow!("{}{}:{}.", super::TIMED_OUT_SIGNING_IN, hop.address, hop.port))??;
        log.line(LogKind::Good, &format!("Signed in to {} as {user}", hop.name));
        if remember_user {
            proxy.send(SessionEvent::Remember { hop: index, what: Remembered::User(user.clone()) });
        }
        if let Some(password) = &signed_in.password
            && signed_in.remember
        {
            proxy.send(SessionEvent::Remember { hop: index, what: Remembered::Password(password.to_string()) });
        }
        sudo = signed_in.password.map(|password| SudoPassword::new(user, password));
        proxy.send(SessionEvent::Hop { index, step: HopStep::Done });
        handles.push(handle);
    }
    // Signed in everywhere: the shell starts where the log began.
    log.clear();
    // Shared with SFTP channels opened on the same connection.
    let handles: Vec<_> = handles.into_iter().map(Arc::new).collect();
    let handle = handles.last().ok_or_else(|| anyhow!("Nothing to connect to."))?;
    // Scoped to this Session: dropping the set aborts even a ping that never answers. Each
    // hop's round trip includes the Jump Hosts before it, all the way from this computer.
    let mut monitors = tokio::task::JoinSet::new();
    for (index, (hop, handle)) in target.hops.iter().zip(&handles).enumerate() {
        let handle = handle.clone();
        let proxy = proxy.clone();
        let active = proxy.window_activity();
        monitors.spawn(super::latency::monitor(index, hop.name.clone(), active, move || {
            let handle = handle.clone();
            async move {
                // russh's helper also returns Ok when its reply channel is dropped on
                // disconnect. A closed connection must never become a successful sample.
                handle.send_ping().await.is_ok() && !handle.is_closed()
            }
        }, move |latency| proxy.send(SessionEvent::Latency(latency))));
    }
    // Dropped with the Session, which closes every listener and connection.
    let forwards = start_forwards(handle, &target.forwards, &remote_forwards, &proxy);
    if target.forward_agent && target.agent.is_none() {
        log::info!("agent forwarding is on, but there is no SSH agent to forward");
    }
    let forward_agent = target.forward_agent && target.agent.is_some();
    let host = target.hops.last().map(|hop| format!("{}:{}", hop.address, hop.port)).unwrap_or_default();
    let result = shell(handle, &host, &mut size, &term, &proxy, &log, &mut commands, sudo, host_window, forward_agent).await;
    drop(forwards);
    monitors.shutdown().await;
    // Always end the connection with the terminal, so SFTP channels opened on it end too.
    // A server that stopped answering doesn't hold the tab: the connections drop with `handles`.
    let _ = tokio::time::timeout(DISCONNECT_TIMEOUT, async {
        let _ = handle.disconnect(Disconnect::ByApplication, "", "en").await;
        // The Jump Hosts' connections, innermost first.
        for jump in handles[..handles.len() - 1].iter().rev() {
            let _ = jump.disconnect(Disconnect::ByApplication, "", "en").await;
        }
    })
    .await;
    result
}

#[allow(clippy::too_many_arguments)]
async fn shell(
    handle: &Arc<Handle<Client>>,
    host: &str,
    size: &mut GridSize,
    term: &SharedTerm,
    proxy: &EventProxy,
    log: &ConnectLog,
    commands: &mut mpsc::UnboundedReceiver<Command>,
    mut sudo: Option<SudoPassword>,
    max_window: u32,
    forward_agent: bool,
) -> Result<String> {
    // The shell's window follows the link (see `pace`), so output doesn't pile up in a queue
    // that still scrolls past after Ctrl+C. Opening the channel with it keeps the server from
    // sending the connection's large window's worth first.
    let mut pace = Pace::up_to(max_window);
    let paced = FIXED_WINDOW.lock().unwrap().is_none();
    // The requests below don't wait for replies; opening the channel does.
    let opening = async {
        if paced { handle.channel_open_session_with_window(pace.window()).await } else { handle.channel_open_session().await }
    };
    let mut channel = tokio::time::timeout(CONNECT_TIMEOUT, opening)
        .await
        .map_err(|_| anyhow!("{}{host}.", super::TIMED_OUT_OPENING_SHELL))??;
    let window = paced.then(|| set_window(handle, channel.id(), pace.window()));
    // Signing in took a while: the pane has been laid out since the connection started, so
    // open the shell at its size rather than resize it straight after (a shell that starts at
    // one size and is resized at once can leave blank lines behind).
    if let Some(latest) = proxy.size() {
        *size = latest;
    }
    channel
        .request_pty(
            false,
            "xterm-256color",
            size.cols as u32,
            size.rows as u32,
            size.pixel_width(),
            size.pixel_height(),
            TERMINAL_MODES,
        )
        .await?;
    // Most servers only accept LANG and LC_* (sshd's AcceptEnv); the rest are dropped
    // silently, so asking costs nothing.
    channel.set_env(false, "COLORTERM", "truecolor").await?;
    if forward_agent {
        // Without a reply, as OpenSSH's client asks: a server that doesn't allow it just never
        // opens an agent channel.
        channel.agent_forward(false).await?;
    }
    channel.request_shell(true).await?;
    log.shell_opened();
    proxy.send(SessionEvent::Status(Status::Connected));

    let mut output = Output::default();
    // Output that arrived in several messages, parsed in one go (see `BATCH`), and the message
    // that ended it, handled next.
    let mut batch = Vec::new();
    let mut held: Option<ChannelMsg> = None;
    // Until the server prints something: blank lines before it are dropped.
    let mut at_start = true;
    let mut exit_status = None;
    // The answer to the ping on its way, if any.
    let mut ping = (paced && pace.started(Instant::now())).then(|| send_ping(handle));
    let reason = loop {
        let sync_deadline = output.sync_deadline();
        tokio::select! {
            msg = async { match held.take() { Some(msg) => Some(msg), None => channel.wait().await } } => match msg {
                Some(ChannelMsg::Data { data }) | Some(ChannelMsg::ExtendedData { data, .. }) => {
                    // Output that is already waiting is parsed with this message, under one lock
                    // and with one Wakeup: a server sends output in packets of a few KiB, and
                    // each paid for those on its own.
                    let mut bytes: &[u8] = &data;
                    if sudo.is_none() {
                        batch.clear();
                        while batch.len().max(bytes.len()) < BATCH {
                            match channel.try_wait() {
                                Some(ChannelMsg::Data { data: more }) | Some(ChannelMsg::ExtendedData { data: more, .. }) => {
                                    if batch.is_empty() {
                                        batch.extend_from_slice(bytes);
                                    }
                                    batch.extend_from_slice(&more);
                                }
                                other => {
                                    held = other;
                                    break;
                                }
                            }
                        }
                        if !batch.is_empty() {
                            bytes = &batch;
                        }
                    }
                    if paced && pace.output(bytes.len(), Instant::now()) {
                        ping = Some(send_ping(handle));
                    }
                    let bytes = skip_leading_blank_lines(&mut at_start, bytes);
                    match &mut sudo {
                        Some(sudo) => show_watching_sudo(&mut output, term, proxy, sudo, bytes),
                        None => output.feed(term, proxy, bytes),
                    }
                }
                Some(ChannelMsg::ExitStatus { exit_status: code }) => exit_status = Some(code),
                // The exit status may still follow the end of output; wait for Close.
                Some(ChannelMsg::Eof) => {}
                Some(ChannelMsg::Close) | None => {
                    output.flush(term, proxy);
                    break match exit_status {
                        Some(0) | None => "Connection closed.".to_string(),
                        Some(code) => format!("Remote shell exited with code {code}."),
                    };
                }
                Some(_) => {}
            },
            command = commands.recv() => match command {
                Some(Command::Input(bytes)) => {
                    match &mut sudo {
                        Some(sudo) => {
                            let typed = sudo.typed(bytes);
                            if typed.clear {
                                output.feed(term, proxy, super::sudo::CLEAR);
                            }
                            channel.data(&typed.send[..]).await?;
                        }
                        None => channel.data_bytes(bytes).await?,
                    }
                    // After the key, so the ping doesn't hold it up.
                    if paced && pace.typed(Instant::now()) {
                        ping = Some(send_ping(handle));
                    }
                }
                Some(Command::Resize(new_size)) => {
                    if new_size != *size {
                        *size = new_size;
                        channel
                            .window_change(size.cols as u32, size.rows as u32, size.pixel_width(), size.pixel_height())
                            .await?;
                    }
                }
                Some(Command::OpenSftp(reply)) => {
                    let handle = handle.clone();
                    tokio::spawn(async move {
                        let _ = reply.send(crate::sftp::Remote::open(handle).await);
                    });
                }
                Some(Command::OpenTunnel { port, reply }) => {
                    let handle = handle.clone();
                    tokio::spawn(async move {
                        let _ = reply.send(open_tunnel(&handle, port).await);
                    });
                }
                Some(Command::Close) | None => break "Session closed.".to_string(),
            },
            // A program began a synchronized update (DECSET 2026) and did not finish it in
            // time: show what it drew so far rather than freezing the screen.
            _ = sleep_until(sync_deadline), if sync_deadline.is_some() => output.flush(term, proxy),
            answer = async { ping.as_mut().expect("a ping").await }, if ping.is_some() => {
                ping = None;
                pace.answered(answer.is_ok(), Instant::now());
                if let Some(window) = &window {
                    window.send_if_modified(|current| std::mem::replace(current, pace.window()) != pace.window());
                }
            }
        }
    };
    Ok(reason)
}

/// Start the Session's Port Forwards on the Host's connection (see `forward`).
fn start_forwards(
    handle: &Arc<Handle<Client>>,
    forwards: &[crate::model::PortForward],
    remote: &super::forward::RemoteForwards,
    proxy: &EventProxy,
) -> tokio::task::JoinSet<()> {
    use super::forward::{BoxTunnel, Listen, OpenError, Opener};
    let open: Opener = {
        let handle = handle.clone();
        Arc::new(move |host: String, port: u16, from: std::net::SocketAddr| {
            let handle = handle.clone();
            Box::pin(async move {
                match handle.channel_open_direct_tcpip(host, u32::from(port), from.ip().to_string(), u32::from(from.port())).await {
                    Ok(channel) => Ok(Box::new(channel.into_stream()) as BoxTunnel),
                    Err(russh::Error::ChannelOpenFailure(russh::ChannelOpenFailure::AdministrativelyProhibited)) => {
                        Err(OpenError::Prohibited)
                    }
                    Err(russh::Error::ChannelOpenFailure(russh::ChannelOpenFailure::ConnectFailed)) => Err(OpenError::Unreachable),
                    Err(err) => Err(OpenError::Other(err.to_string())),
                }
            })
        })
    };
    let listen: Listen = {
        let handle = handle.clone();
        Arc::new(move |address: String, port: u32| {
            let handle = handle.clone();
            Box::pin(async move {
                match handle.tcpip_forward(address, port).await {
                    Ok(_) => Ok(()),
                    Err(russh::Error::RequestDenied) => Err(super::forward::remote_refused(port as u16)),
                    Err(err) => Err(format!("Couldn't ask the server to listen on port {port}: {err}")),
                }
            })
        })
    };
    let proxy = proxy.clone();
    super::forward::start(forwards, open, listen, remote, Arc::new(move |status| proxy.send(SessionEvent::Forward(status))))
}

/// A tunnel to `port` on the Host itself, over the Session's connection. The channel is
/// copied to one end of an in-memory pipe, whose other end is returned: the desktop engines
/// want a stream they may share between threads, which a channel is not.
async fn open_tunnel(handle: &Arc<Handle<Client>>, port: u16) -> Result<crate::desktop::Stream> {
    let channel = handle
        .channel_open_direct_tcpip("localhost", u32::from(port), "127.0.0.1", 0)
        .await
        .with_context(|| format!("The SSH server refused a tunnel to port {port} on this host"))?;
    let (near, mut far) = tokio::io::duplex(256 * 1024);
    tokio::spawn(async move {
        let mut channel = channel.into_stream();
        let _ = tokio::io::copy_bidirectional(&mut channel, &mut far).await;
    });
    Ok(Box::new(near))
}

/// Keeps the shell channel `id` at the window sent here, from a task of its own: the shell loop
/// must never wait for the connection, which may itself be waiting for the shell loop to take
/// output. Only the latest window is sent. Ends with the sender.
fn set_window(handle: &Arc<Handle<Client>>, id: russh::ChannelId, window: u32) -> tokio::sync::watch::Sender<u32> {
    let (sender, mut latest) = tokio::sync::watch::channel(window);
    let handle = handle.clone();
    tokio::spawn(async move {
        while latest.changed().await.is_ok() {
            let window = *latest.borrow_and_update();
            if handle.set_channel_window(id, window).await.is_err() {
                return;
            }
        }
    });
    sender
}

/// Ping the server; the answer comes once it has arrived, after all output sent before it.
fn send_ping(handle: &Arc<Handle<Client>>) -> oneshot::Receiver<()> {
    let (answered, answer) = oneshot::channel();
    let handle = handle.clone();
    tokio::spawn(async move {
        if handle.send_ping().await.is_ok() {
            let _ = answered.send(());
        }
    });
    answer
}

/// Shows output, and offers the sign-in password after a sudo prompt for the signed-in user:
/// a hint after the prompt, where there is room for it on the line and the screen is the
/// shell's (not a full-screen program's, which would draw over it).
fn show_watching_sudo(output: &mut Output, term: &SharedTerm, proxy: &EventProxy, sudo: &mut SudoPassword, data: &[u8]) {
    if sudo.output_arrives() {
        output.feed(term, proxy, super::sudo::CLEAR);
    }
    output.feed(term, proxy, data);
    if sudo.output_shown(data) {
        let room = {
            let term = term.lock();
            if term.mode().contains(TermMode::ALT_SCREEN) {
                0
            } else {
                term.columns().saturating_sub(term.grid().cursor.point.column.0 + 1)
            }
        };
        if let Some(hint) = sudo.offer(room) {
            output.feed(term, proxy, hint.as_bytes());
        }
    }
}

/// POSIX terminal modes for the remote PTY, as OpenSSH sends them from a local terminal.
/// Without them the server falls back to kernel defaults, which on Linux leave out IUTF8:
/// then backspace in cooked mode (`read`, `cat`, password prompts) deletes one byte of a
/// multi-byte character instead of the whole character.
const TERMINAL_MODES: &[(Pty, u32)] = &[
    (Pty::VINTR, 0x03),
    (Pty::VQUIT, 0x1c),
    (Pty::VERASE, 0x7f),
    (Pty::VKILL, 0x15),
    (Pty::VEOF, 0x04),
    (Pty::VSUSP, 0x1a),
    (Pty::VWERASE, 0x17),
    (Pty::VLNEXT, 0x16),
    (Pty::ICRNL, 1),
    (Pty::IUTF8, 1),
    (Pty::ISIG, 1),
    (Pty::ICANON, 1),
    (Pty::IEXTEN, 1),
    (Pty::ECHO, 1),
    (Pty::ECHOE, 1),
    (Pty::ECHOK, 1),
    (Pty::ECHOCTL, 1),
    (Pty::ECHOKE, 1),
    (Pty::OPOST, 1),
    (Pty::ONLCR, 1),
    (Pty::CS8, 1),
    (Pty::TTY_OP_ISPEED, 38400),
    (Pty::TTY_OP_OSPEED, 38400),
];

/// The most output collected when several messages are already waiting. Parsing releases
/// the terminal lock between smaller pieces (see `PARSE_CHUNK`).
const BATCH: usize = 256 * 1024;

/// Bound ordinary parsing's lock hold without giving up network batching. The parser keeps
/// UTF-8, escape sequence and synchronized-update state across pieces.
const PARSE_CHUNK: usize = 32 * 1024;

/// The output with line breaks before the server's first visible output left out, while
/// `at_start`. Network gear (Cisco IOS, for one) starts a session with empty lines before its
/// prompt, which would leave the prompt below a gap at the top of the terminal.
fn skip_leading_blank_lines<'a>(at_start: &mut bool, bytes: &'a [u8]) -> &'a [u8] {
    if !*at_start {
        return bytes;
    }
    let first = bytes.iter().position(|b| !matches!(b, b'\r' | b'\n')).unwrap_or(bytes.len());
    if first < bytes.len() {
        *at_start = false;
    }
    &bytes[first..]
}

async fn sleep_until(deadline: Option<std::time::Instant>) {
    match deadline {
        Some(deadline) => tokio::time::sleep_until(deadline.into()).await,
        None => std::future::pending().await,
    }
}

/// Parses program output into the grid the way alacritty's PTY loop does for Local
/// Sessions, including synchronized updates: while a program draws inside one, the grid is
/// left alone and the UI is not woken, so it never shows a half-drawn screen.
#[derive(Default)]
pub struct Output {
    parser: Processor,
}

impl Output {
    pub fn feed(&mut self, term: &SharedTerm, proxy: &EventProxy, bytes: &[u8]) {
        for chunk in bytes.chunks(PARSE_CHUNK) {
            self.parser.advance(&mut *term.lock(), chunk);
            if self.parser.sync_bytes_count() < chunk.len() {
                proxy.send_event(Event::Wakeup);
            }
        }
        // After the grid has it, so whatever is typed next lands after this output.
        proxy.watch_output(bytes);
    }

    /// When a pending synchronized update must be shown even if it never ends.
    pub(crate) fn sync_deadline(&self) -> Option<std::time::Instant> {
        self.parser.sync_timeout().sync_timeout()
    }

    /// Apply a pending synchronized update now.
    pub(crate) fn flush(&mut self, term: &SharedTerm, proxy: &EventProxy) {
        if self.parser.sync_bytes_count() > 0 {
            self.parser.stop_sync(&mut *term.lock());
            proxy.send_event(Event::Wakeup);
        }
    }
}

/// How a hop signed in.
struct SignedIn {
    /// The password that worked, saved or typed; `None` for a key.
    password: Option<Zeroizing<String>>,
    /// It was typed with "Remember" ticked: save it for the Host.
    remember: bool,
}

/// Where signing in with a password stands, shared by the password and keyboard-interactive
/// methods and across a server's several steps (a key, then a one-time code, say).
struct PasswordState<'a> {
    /// The saved password, until it has been tried once.
    saved: Option<&'a str>,
    /// The tick on "Remember", carried over from one question to the next.
    remember: bool,
    /// Why the last try failed, shown with the next question.
    last_error: Option<String>,
    /// A rejected saved password ends the method rather than asking for one: the SSH Agent's
    /// keys come before a typed password.
    agent_next: bool,
    signed_in: SignedIn,
    /// Stopped while a question waits for the user.
    clock: ConnectClock,
}

/// What one sign-in method came to.
enum Step {
    /// Signed in.
    Done,
    /// Accepted, but the server asks for more, by one of these methods.
    More(MethodSet),
    /// Turned down; the server still accepts these methods.
    Refused(MethodSet),
}

/// Tries of a method the user typed answers for before Komando gives up, as OpenSSH does.
const TRIES: usize = 3;

/// `address` names the machine in the password question, so a Jump Host's (perhaps from a
/// shared Project) can't pass for another's.
///
/// The order: a saved key, then the SSH Agent's keys (`agent_use` of them), then passwords.
/// A saved password goes before the agent's keys, though: a server counts every key offered
/// as a failed try, and an agent with many keys would use them up before the password that
/// was saved for it. The agent comes after it only if the server asks for a key as well, or
/// takes nothing but keys.
///
/// After a key, the server says which methods it takes next. Of those, keyboard-interactive
/// goes first, as in OpenSSH: network gear and servers with one-time codes (RADIUS, TACACS+,
/// PAM) often take passwords only that way. A plain "Password:" question looks the same as
/// with the password method, saved password and "Remember" included.
#[allow(clippy::too_many_arguments)]
async fn authenticate(
    handle: &mut Handle<Client>,
    user: &str,
    address: &str,
    secret: Option<&Secret>,
    agent: Option<&Agent>,
    agent_use: &AgentUse,
    proxy: &EventProxy,
    log: &ConnectLog,
    clock: &ConnectClock,
) -> Result<SignedIn> {
    let saved = match secret {
        Some(Secret::Password { password }) => Some(password.as_str()),
        _ => None,
    };
    let mut state = PasswordState {
        saved,
        remember: false,
        last_error: None,
        agent_next: false,
        signed_in: SignedIn { password: None, remember: false },
        clock: clock.clone(),
    };
    let mut methods = None;
    if let Some(Secret::PrivateKey { key, passphrase }) = secret {
        log.line(LogKind::Step, &format!("Signing in as {user} with the saved key"));
        let decoded = match russh::keys::decode_secret_key(key, passphrase.as_deref()) {
            Ok(decoded) => Ok(decoded),
            Err(_) if passphrase.is_none() => {
                let pass = ask(proxy, clock, "Key passphrase", true, None, None).await.ok_or_else(|| anyhow!(CANCELLED))?;
                let pass = Zeroizing::new(pass.value);
                russh::keys::decode_secret_key(key, Some(pass.as_str()))
            }
            Err(err) => Err(err),
        };
        match decoded {
            Ok(private_key) => {
                let hash = handle.best_supported_rsa_hash().await?.flatten();
                let result = handle
                    .authenticate_publickey(user, PrivateKeyWithHashAlg::new(Arc::new(private_key), hash))
                    .await?;
                match result {
                    AuthResult::Success => return Ok(state.signed_in),
                    AuthResult::Failure { remaining_methods, partial_success: true } => {
                        log.line(LogKind::Step, "The key was accepted; the server asks for more");
                        methods = Some(remaining_methods);
                    }
                    AuthResult::Failure { remaining_methods, partial_success: false } => {
                        state.last_error = Some("The server rejected the saved key.".to_string());
                        methods = Some(remaining_methods);
                    }
                }
            }
            Err(err) => state.last_error = Some(format!("Could not read the saved key: {err}")),
        }
        if let Some(error) = &state.last_error {
            log.line(LogKind::Warn, error);
        }
    }
    // Without a key, asking to sign in with no method at all is how the server says which it takes.
    let mut methods = match methods {
        Some(methods) => methods,
        None => match handle.authenticate_none(user).await? {
            AuthResult::Success => return Ok(state.signed_in),
            AuthResult::Failure { remaining_methods, .. } => remaining_methods,
        },
    };
    let mut agent = agent;
    // A server asks for at most a few things in turn (a key, a password, a code), and the
    // agent's keys take a turn of their own.
    for _ in 0..5 {
        let interactive = methods.contains(&MethodKind::KeyboardInteractive);
        // An empty list is a server that didn't say: it may take either.
        let takes_password = interactive || methods.contains(&MethodKind::Password) || methods.is_empty();
        let takes_key = methods.contains(&MethodKind::PublicKey) || methods.is_empty();
        if takes_key
            && (state.saved.is_none() || !takes_password)
            && let Some(agent) = agent.take()
        {
            match agent_keys(handle, user, address, agent, agent_use, log, clock).await? {
                // Nothing to offer: on to the rest.
                None => {}
                Some(Step::Done) => return Ok(state.signed_in),
                Some(Step::More(next)) => {
                    log.line(LogKind::Step, "The key was accepted; the server asks for more");
                    methods = next;
                    continue;
                }
                Some(Step::Refused(next)) => methods = next,
            }
            continue;
        }
        state.agent_next = agent.is_some();
        let step = if interactive {
            keyboard_interactive(handle, user, address, &mut state, proxy, log).await?
        } else if methods.contains(&MethodKind::Password) || methods.is_empty() {
            // An empty list is a server that didn't say: try a password, as before.
            password(handle, user, address, &mut state, proxy, log).await?
        } else {
            let names: Vec<&str> = methods.iter().map(<&str>::from).collect();
            let hint = if methods.contains(&MethodKind::PublicKey) {
                " Save a key for this host, or add one to your SSH agent."
            } else {
                ""
            };
            return Err(anyhow!("{address} takes no password, only: {}.{hint}", names.join(", ")));
        };
        match step {
            Step::Done => return Ok(state.signed_in),
            Step::More(next) => {
                log.line(LogKind::Step, "Accepted; the server asks for more");
                methods = next;
            }
            // The saved password was rejected: the agent's keys before asking for one.
            Step::Refused(next) if agent.is_some() && next.contains(&MethodKind::PublicKey) => methods = next,
            // Keyboard-interactive turned down: the password method, if the server takes it too.
            Step::Refused(mut next) if interactive && next.contains(&MethodKind::Password) => {
                next.remove(MethodKind::KeyboardInteractive);
                methods = next;
            }
            Step::Refused(_) => break,
        }
    }
    Err(anyhow!("Authentication failed."))
}

/// Sign in with the SSH Agent's keys, one after another, until the server takes one. `None`:
/// there was no key to offer (no agent running, or not the key chosen for this Host).
async fn agent_keys(
    handle: &mut Handle<Client>,
    user: &str,
    address: &str,
    agent: &Agent,
    agent_use: &AgentUse,
    log: &ConnectLog,
    clock: &ConnectClock,
) -> Result<Option<Step>> {
    // Its own time limit: listing may also wait for the user's approval.
    let opened = match clock.paused(agent.open(handle.agent_session_binding())).await {
        Ok(Some(opened)) => opened,
        Ok(None) => return Ok(None),
        Err(err) => {
            log.line(LogKind::Warn, &format!("{err:#}"));
            return Ok(None);
        }
    };
    let super::agent::Opened { mut client, keys, agent } = opened;
    let keys: Vec<_> = match agent_use {
        AgentUse::Key(fingerprint) => {
            let chosen: Vec<_> = keys.into_iter().filter(|key| super::agent::fingerprint(key) == *fingerprint).collect();
            if chosen.is_empty() {
                log.line(LogKind::Warn, &format!("{} doesn't hold the key chosen for this host ({fingerprint})", agent.label()));
            }
            chosen
        }
        _ => keys,
    };
    if keys.is_empty() {
        return Ok(None);
    }
    match keys.as_slice() {
        [key] => {
            let key = super::agent::Key::of(key);
            log.line(LogKind::Step, &format!("Signing in as {user} with the SSH agent's key {}", key.label()));
        }
        _ => log.line(LogKind::Step, &format!("Signing in as {user} with the SSH agent's keys ({})", keys.len())),
    }
    let hash = handle.best_supported_rsa_hash().await?.flatten();
    let mut refused = None;
    let mut offered = 0;
    for key in &keys {
        if handle.is_closed() {
            break;
        }
        offered += 1;
        // Not timed: the agent may wait for the user to approve or touch a security key.
        let result = clock
            .paused(async {
                match key {
                    russh::keys::agent::AgentIdentity::PublicKey { key, .. } => {
                        handle.authenticate_publickey_with(user, key.clone(), hash, &mut client).await
                    }
                    russh::keys::agent::AgentIdentity::Certificate { certificate, .. } => {
                        handle.authenticate_certificate_with(user, certificate.clone(), hash, &mut client).await
                    }
                }
            })
            .await;
        match result {
            Ok(AuthResult::Success) => return Ok(Some(Step::Done)),
            Ok(AuthResult::Failure { remaining_methods, partial_success: true }) => return Ok(Some(Step::More(remaining_methods))),
            Ok(AuthResult::Failure { remaining_methods, partial_success: false }) => refused = Some(remaining_methods),
            // Declined on the agent's side (a 1Password approval, a YubiKey not touched):
            // the next key may still do.
            // The server hung up (too many keys): not the agent's doing.
            Err(_) if handle.is_closed() => break,
            Err(err) => {
                let key = super::agent::Key::of(key);
                log.line(LogKind::Warn, &format!("The SSH agent didn't sign with {}: {err}", key.label()));
            }
        }
    }
    if handle.is_closed() {
        return Err(anyhow!(
            "{address} closed the connection after {offered} of the SSH agent's keys. Choose the key for this host in its sign-in settings, or turn the agent off there."
        ));
    }
    log.line(LogKind::Warn, if keys.len() == 1 { "The server rejected the agent's key." } else { "The server rejected the agent's keys." });
    Ok(refused.map(Step::Refused))
}

/// The password method: the saved password, then up to [`TRIES`] typed ones.
async fn password(
    handle: &mut Handle<Client>,
    user: &str,
    address: &str,
    state: &mut PasswordState<'_>,
    proxy: &EventProxy,
    log: &ConnectLog,
) -> Result<Step> {
    let mut refused = MethodSet::empty();
    if let Some(saved) = state.saved.take() {
        log.line(LogKind::Step, &format!("Signing in as {user} with the saved password"));
        match handle.authenticate_password(user, saved).await? {
            AuthResult::Success => {
                state.signed_in.password = Some(Zeroizing::new(saved.to_string()));
                return Ok(Step::Done);
            }
            AuthResult::Failure { remaining_methods, partial_success: true } => {
                state.signed_in.password = Some(Zeroizing::new(saved.to_string()));
                return Ok(Step::More(remaining_methods));
            }
            AuthResult::Failure { remaining_methods, partial_success: false } => {
                log.line(LogKind::Warn, "The saved password was rejected.");
                state.last_error = Some("The saved password was rejected.".to_string());
                if state.agent_next {
                    return Ok(Step::Refused(remaining_methods));
                }
                refused = remaining_methods;
            }
        }
    }
    log.line(LogKind::Step, &format!("Signing in as {user} with a password: waiting for it"));
    for _ in 0..TRIES {
        let password = ask_password(user, address, state, proxy).await?;
        match handle.authenticate_password(user, password.as_str()).await? {
            AuthResult::Success => {
                state.signed_in.password = Some(password);
                state.signed_in.remember = state.remember;
                return Ok(Step::Done);
            }
            AuthResult::Failure { remaining_methods, partial_success: true } => {
                state.signed_in.password = Some(password);
                state.signed_in.remember = state.remember;
                return Ok(Step::More(remaining_methods));
            }
            AuthResult::Failure { remaining_methods, partial_success: false } => {
                log.line(LogKind::Warn, "Wrong password");
                state.last_error = Some("Wrong password. Try again.".to_string());
                refused = remaining_methods;
            }
        }
    }
    Ok(Step::Refused(refused))
}

/// Keyboard-interactive: the server asks questions, any number of them in any number of
/// rounds. A password question gets the saved password once, then asks as the password method
/// does; any other question (a one-time code, a challenge) is asked as the server words it.
async fn keyboard_interactive(
    handle: &mut Handle<Client>,
    user: &str,
    address: &str,
    state: &mut PasswordState<'_>,
    proxy: &EventProxy,
    log: &ConnectLog,
) -> Result<Step> {
    log.line(LogKind::Step, &format!("Signing in as {user} (keyboard-interactive)"));
    let mut failures = 0;
    // Rounds without a question don't count as tries, but a server can't send them forever.
    let mut rounds = 0;
    let mut reply = handle.authenticate_keyboard_interactive_start(user, None).await?;
    // Whether this try answered with something the user typed (not just the saved password).
    let mut typed = false;
    // The password given in this try, kept if it signs in.
    let mut given: Option<(Zeroizing<String>, bool)> = None;
    loop {
        match reply {
            KeyboardInteractiveAuthResponse::Success => {
                keep_password(state, given);
                return Ok(Step::Done);
            }
            KeyboardInteractiveAuthResponse::Failure { remaining_methods, partial_success: true } => {
                keep_password(state, given);
                return Ok(Step::More(remaining_methods));
            }
            KeyboardInteractiveAuthResponse::Failure { remaining_methods, partial_success: false } => {
                let (line, error) = match &given {
                    Some((_, false)) => ("The saved password was rejected.", "The saved password was rejected."),
                    Some((_, true)) => ("Wrong password", "Wrong password. Try again."),
                    None => ("Not accepted", "Not accepted. Try again."),
                };
                log.line(LogKind::Warn, line);
                state.last_error = Some(error.to_string());
                if !typed && matches!(given, Some((_, false))) && state.agent_next {
                    return Ok(Step::Refused(remaining_methods));
                }
                if typed {
                    failures += 1;
                }
                if failures >= TRIES || !remaining_methods.contains(&MethodKind::KeyboardInteractive) {
                    return Ok(Step::Refused(remaining_methods));
                }
                (typed, given) = (false, None);
                reply = handle.authenticate_keyboard_interactive_start(user, None).await?;
            }
            KeyboardInteractiveAuthResponse::InfoRequest { name, instructions, prompts } => {
                rounds += 1;
                if rounds > 20 {
                    return Err(anyhow!("The server kept asking without signing in."));
                }
                for text in [name.trim(), instructions.trim()] {
                    if !text.is_empty() {
                        log.line(LogKind::Step, text);
                    }
                }
                let mut answers = Vec::with_capacity(prompts.len());
                for prompt in &prompts {
                    if is_password_question(&prompt.prompt, prompt.echo) {
                        let (password, was_typed) = match state.saved.take() {
                            Some(saved) => (Zeroizing::new(saved.to_string()), false),
                            None => (ask_password(user, address, state, proxy).await?, true),
                        };
                        typed |= was_typed;
                        answers.push(password.to_string());
                        given = Some((password, was_typed));
                    } else {
                        typed = true;
                        let label = question_label(&prompt.prompt, user, address);
                        let answer = ask(proxy, &state.clock, &label, !prompt.echo, None, state.last_error.take())
                            .await
                            .ok_or_else(|| anyhow!(CANCELLED))?;
                        answers.push(answer.value);
                    }
                }
                reply = handle.authenticate_keyboard_interactive_respond(answers).await?;
            }
        }
    }
}

/// Keeps the password that signed in, for sudo and, if typed with "Remember" ticked, the vault.
fn keep_password(state: &mut PasswordState<'_>, given: Option<(Zeroizing<String>, bool)>) {
    if let Some((password, typed)) = given {
        state.signed_in.remember = typed && state.remember;
        state.signed_in.password = Some(password);
    }
}

async fn ask_password(user: &str, address: &str, state: &mut PasswordState<'_>, proxy: &EventProxy) -> Result<Zeroizing<String>> {
    let label = format!("Password for {user}@{address}");
    let answer = ask(proxy, &state.clock, &label, true, Some(state.remember), state.last_error.take())
        .await
        .ok_or_else(|| anyhow!(CANCELLED))?;
    state.remember = answer.remember;
    Ok(Zeroizing::new(answer.value))
}

/// A hidden question that asks for the password, which a saved password may answer. Servers
/// word it "Password:", "Password for alice:", "alice@host's password:" and the like; a
/// question for a code or a "passcode" is not one.
fn is_password_question(prompt: &str, echo: bool) -> bool {
    !echo && prompt.to_lowercase().contains("password")
}

/// The server's question as the prompt's label, with who it is for.
fn question_label(prompt: &str, user: &str, address: &str) -> String {
    let question = prompt.trim().trim_end_matches(':').trim();
    if question.is_empty() { format!("Answer for {user}@{address}") } else { format!("{question} ({user}@{address})") }
}

#[cfg(test)]
mod tests {
    use super::{CANCELLED, ConnectLog, LogKind, Output, SudoPassword, is_cancelled, log_line, show_watching_sudo};
    use super::{MAX_WINDOW, WINDOW, window_for};
    use std::time::Duration;
    use anyhow::anyhow;
    use crate::session::{EventProxy, GridSize, new_term, scrollback_text};

    #[test]
    fn blank_lines_before_the_first_output_are_left_out() {
        let mut at_start = true;
        assert_eq!(super::skip_leading_blank_lines(&mut at_start, b"\r\n"), b"");
        assert!(at_start, "nothing shown yet");
        assert_eq!(super::skip_leading_blank_lines(&mut at_start, b"\r\n\r\nswitch#"), b"switch#");
        assert!(!at_start);
        assert_eq!(super::skip_leading_blank_lines(&mut at_start, b"\r\n\r\nswitch#"), b"\r\n\r\nswitch#", "only at the start");
        let mut at_start = true;
        assert_eq!(super::skip_leading_blank_lines(&mut at_start, b"\x1b[0m\r\n$ "), b"\x1b[0m\r\n$ ", "escape sequences count as output");
    }

    #[test]
    fn the_connect_timeout_does_not_count_a_host_key_question() {
        let runtime = tokio::runtime::Builder::new_current_thread().enable_time().build().unwrap();
        let timeout = Duration::from_millis(80);
        let clock = super::ConnectClock::new();
        // The user takes several timeouts to answer; the key exchange then finishes promptly.
        let slow_answer = runtime.block_on(clock.limit(timeout, {
            let clock = clock.clone();
            async move {
                clock.pause();
                tokio::time::sleep(timeout * 4).await;
                clock.resume();
                tokio::time::sleep(timeout / 4).await;
                "connected"
            }
        }));
        assert_eq!(slow_answer, Some("connected"));
        // Without a question, a server that never answers still times out.
        let stalled = runtime.block_on(clock.limit(timeout, std::future::pending::<()>()));
        assert_eq!(stalled, None);
    }

    #[test]
    fn signing_in_times_out_only_while_waiting_on_the_server() {
        let runtime = tokio::runtime::Builder::new_current_thread().enable_time().build().unwrap();
        let timeout = Duration::from_millis(80);
        let clock = super::ConnectClock::new();
        // A slow password prompt, then two server replies each well within the limit.
        let signed_in = runtime.block_on(clock.limit(timeout, {
            let clock = clock.clone();
            async move {
                clock.paused(tokio::time::sleep(timeout * 4)).await;
                tokio::time::sleep(timeout / 3).await;
                clock.paused(tokio::time::sleep(timeout * 2)).await;
                tokio::time::sleep(timeout / 3).await;
                "signed in"
            }
        }));
        assert_eq!(signed_in, Some("signed in"));
        // A server that stops answering after the prompt still times out.
        let stalled = runtime.block_on(clock.limit(timeout, {
            let clock = clock.clone();
            async move {
                clock.paused(tokio::time::sleep(timeout * 2)).await;
                std::future::pending::<()>().await
            }
        }));
        assert_eq!(stalled, None);
    }

    #[test]
    fn the_sudo_hint_sits_after_the_prompt_and_goes_away() {
        let (tx, _rx) = futures::channel::mpsc::unbounded();
        let proxy = EventProxy::new(tx);
        let size = GridSize { cols: 80, rows: 5, cell_width: 9, cell_height: 18 };
        let term = new_term(size, 100, proxy.clone());
        let mut output = Output::default();
        let mut sudo = SudoPassword::new("ana".into(), zeroize::Zeroizing::new("hunter2".into()));
        let cursor = |term: &crate::session::SharedTerm| term.lock().grid().cursor.point;

        show_watching_sudo(&mut output, &term, &proxy, &mut sudo, b"$ sudo ls\r\n[sudo] password for ana: ");
        let text = scrollback_text(&*term.lock());
        assert!(text.contains("[sudo] password for ana:   Komando  Press Enter to use your sign-in password"), "{text:?}");
        assert_eq!(cursor(&term).column.0, "[sudo] password for ana: ".len(), "the cursor stays at the prompt");

        let typed = sudo.typed(b"\r".to_vec());
        assert!(typed.clear);
        assert_eq!(&*typed.send, b"hunter2\r");
        output.feed(&term, &proxy, crate::session::sudo::CLEAR);
        assert_eq!(scrollback_text(&*term.lock()), "$ sudo ls\r\n[sudo] password for ana:\r\n");

        // Too little room left on the line: nothing is drawn or offered.
        let narrow = new_term(GridSize { cols: 30, ..size }, 100, proxy.clone());
        let mut sudo = SudoPassword::new("ana".into(), zeroize::Zeroizing::new("hunter2".into()));
        show_watching_sudo(&mut output, &narrow, &proxy, &mut sudo, b"[sudo] password for ana: ");
        assert_eq!(scrollback_text(&*narrow.lock()), "[sudo] password for ana:\r\n");
        assert_eq!(&*sudo.typed(b"\r".to_vec()).send, b"\r");
    }

    #[test]
    fn clearing_the_log_erases_its_lines_and_keeps_earlier_output() {
        let (tx, _rx) = futures::channel::mpsc::unbounded();
        let proxy = EventProxy::new(tx);
        let term = new_term(GridSize { cols: 30, rows: 10, cell_width: 9, cell_height: 18 }, 100, proxy.clone());
        let mut parser: alacritty_terminal::vte::ansi::Processor = alacritty_terminal::vte::ansi::Processor::new();
        parser.advance(&mut *term.lock(), b"earlier output\r\n");

        let log = ConnectLog::new(term.clone(), proxy.clone());
        log.line(LogKind::Step, "Connecting to web01");
        // Wider than the terminal: it wraps onto a second row.
        log.line(LogKind::Step, "Host key ssh-ed25519 SHA256:abcdefghijklmnop");
        log.line(LogKind::Good, "Signed in to web01 as ana");
        log.clear();

        assert_eq!(scrollback_text(&*term.lock()), "earlier output\r\n");
        assert_eq!(term.lock().grid().cursor.point.line.0, 1, "the shell starts where the log began");
        // Nothing left to erase: clearing again leaves earlier output alone.
        log.clear();
        assert_eq!(scrollback_text(&*term.lock()), "earlier output\r\n");
    }

    #[test]
    fn signing_in_erases_the_logs_of_earlier_attempts_that_failed() {
        let (tx, _rx) = futures::channel::mpsc::unbounded();
        let proxy = EventProxy::new(tx);
        let term = new_term(GridSize { cols: 40, rows: 20, cell_width: 9, cell_height: 18 }, 100, proxy.clone());
        let mut parser: alacritty_terminal::vte::ansi::Processor = alacritty_terminal::vte::ansi::Processor::new();
        parser.advance(&mut *term.lock(), b"earlier output\r\n");

        // Two attempts cancelled at the password, each with a log of its own.
        for _ in 0..2 {
            let log = ConnectLog::new(term.clone(), proxy.clone());
            log.line(LogKind::Step, "Connecting to rhel-01");
            log.line(LogKind::Step, "Signing in as lxadm with a password: waiting for it");
            log.line(LogKind::Fail, CANCELLED);
        }
        assert!(scrollback_text(&*term.lock()).contains(CANCELLED), "a failed attempt keeps its log");

        let log = ConnectLog::new(term.clone(), proxy.clone());
        log.line(LogKind::Step, "Connecting to rhel-01");
        log.line(LogKind::Good, "Signed in to rhel-01 as lxadm");
        log.clear();
        assert_eq!(scrollback_text(&*term.lock()), "earlier output\r\n");
        assert_eq!(term.lock().grid().cursor.point.line.0, 1, "the shell starts where the first log began");
    }

    #[test]
    fn a_host_further_behind_jump_hosts_gets_a_larger_window() {
        assert_eq!(window_for(Duration::from_millis(1)), WINDOW, "a Host next to its Jump Host");
        assert_eq!(window_for(Duration::from_millis(56)), WINDOW, "a Host next to a Jump Host 25 ms away");
        assert_eq!(window_for(Duration::from_millis(102)), 2 * WINDOW);
        assert_eq!(window_for(Duration::from_millis(160)), 3 * WINDOW);
        assert_eq!(window_for(Duration::from_secs(2)), MAX_WINDOW, "but not without end");
    }

    #[test]
    fn an_ended_shell_says_so_below_its_prompt_and_leaves_full_screen_programs() {
        let (tx, _rx) = futures::channel::mpsc::unbounded();
        let proxy = EventProxy::new(tx);
        let term = new_term(GridSize { cols: 60, rows: 10, cell_width: 9, cell_height: 18 }, 100, proxy.clone());
        let mut parser: alacritty_terminal::vte::ansi::Processor = alacritty_terminal::vte::ansi::Processor::new();
        // A prompt waiting for input, then an editor on the alternate screen that hid the cursor.
        parser.advance(&mut *term.lock(), b"ana@web01:~$ vim\r\n\x1b[?1049h\x1b[?25l\x1b[?1000hediting");

        let log = ConnectLog::new(term.clone(), proxy.clone());
        log.shell_opened();
        log.ended(LogKind::Fail, "Connection reset by peer");

        let term = term.lock();
        assert!(!term.mode().contains(alacritty_terminal::term::TermMode::ALT_SCREEN), "back on the shell's screen");
        assert!(term.mode().contains(alacritty_terminal::term::TermMode::SHOW_CURSOR));
        assert!(!term.mode().intersects(alacritty_terminal::term::TermMode::MOUSE_MODE));
        let text = scrollback_text(&*term);
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines[0], "ana@web01:~$ vim");
        assert_eq!(lines[1], "SSH ✕ Disconnected: Connection reset by peer");
        assert_eq!(lines[2], "SSH · Press Enter to reconnect");
        assert_eq!(term.grid().cursor.point.column.0, 0);
    }

    #[test]
    fn an_ended_shell_starts_its_notice_on_a_line_of_its_own() {
        let (tx, _rx) = futures::channel::mpsc::unbounded();
        let proxy = EventProxy::new(tx);
        let term = new_term(GridSize { cols: 60, rows: 10, cell_width: 9, cell_height: 18 }, 100, proxy.clone());
        let mut parser: alacritty_terminal::vte::ansi::Processor = alacritty_terminal::vte::ansi::Processor::new();
        parser.advance(&mut *term.lock(), b"ana@web01:~$ ");

        let log = ConnectLog::new(term.clone(), proxy.clone());
        log.ended(LogKind::Step, "Session closed.");
        // The next attempt's sign-in erases the notice along with its own log, and records
        // for good when it dropped and when it came back.
        let next = ConnectLog::new(term.clone(), proxy.clone());
        next.line(LogKind::Good, "Signed in to web01 as ana");
        assert!(scrollback_text(&*term.lock()).contains("SSH · Disconnected: Session closed.\r\n"));
        next.clear();
        let text = scrollback_text(&*term.lock());
        let lines: Vec<&str> = text.lines().map(str::trim_end).collect();
        assert_eq!(lines[0], "ana@web01:~$");
        assert!(lines[1].starts_with("SSH · Disconnected at ") && lines[1].ends_with(": Session closed."), "{lines:?}");
        assert!(lines[2].starts_with("SSH ✓ Reconnected at "), "{lines:?}");
        assert!(!text.contains("Press Enter"));

        // A later sign-in in the same terminal leaves them be and adds nothing.
        let again = ConnectLog::new(term.clone(), proxy.clone());
        again.line(LogKind::Good, "Signed in to web01 as ana");
        again.clear();
        assert_eq!(scrollback_text(&*term.lock()), text);
    }

    #[test]
    fn log_lines_are_tagged_and_end_the_line() {
        let line = log_line(LogKind::Good, "Signed in to web01 as ana");
        assert!(line.starts_with("\x1b[0m\x1b[90mSSH\x1b[0m "));
        assert!(line.contains("✓"));
        assert!(line.ends_with("Signed in to web01 as ana\x1b[0m\r\n"));
    }

    #[test]
    fn only_a_closed_question_counts_as_cancelled() {
        assert!(is_cancelled(&anyhow!(CANCELLED)));
        assert!(is_cancelled(&anyhow!(CANCELLED).context("Signing in to web01")));
        assert!(!is_cancelled(&anyhow!("Connection refused")));
    }

    #[test]
    fn replacing_a_changed_host_key_keeps_other_hosts() {
        use russh::keys::{PrivateKey, ssh_key::private::Ed25519Keypair};
        let key = |seed| PrivateKey::from(Ed25519Keypair::from_seed(&[seed; 32])).public_key().clone();
        let (old, new, other) = (key(1), key(2), key(3));
        let dir = std::env::temp_dir().join(format!("komando-known-hosts-{}", uuid::Uuid::new_v4()));
        let path = dir.join("known_hosts");
        std::fs::create_dir_all(&dir).unwrap();
        let line = |host: &str, key: &russh::keys::PublicKey| format!("{host} {}\n", key.to_openssh().unwrap());
        std::fs::write(
            &path,
            format!("# kept\n{}{}{}", line("web01", &old), line("[web01]:2222", &other), line("db01,web01", &old)),
        )
        .unwrap();
        assert!(matches!(
            russh::keys::check_known_hosts_path("web01", 22, &new, &path),
            Err(russh::keys::Error::KeyChanged { .. })
        ));

        super::replace_known_host("web01", 22, &new, &path).unwrap();

        assert!(russh::keys::check_known_hosts_path("web01", 22, &new, &path).unwrap(), "the new key is trusted");
        assert!(russh::keys::check_known_hosts_path("web01", 2222, &other, &path).unwrap(), "another port is left alone");
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.starts_with("# kept\n"), "comments are kept: {text}");
        assert!(!text.contains("db01"), "a line also naming the host goes, as with ssh-keygen -R: {text}");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600, "only the user may read it");
        }
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn log_lines_cannot_carry_escape_sequences() {
        // A Host named with an escape sequence must not clear the screen or retitle the tab.
        let line = log_line(LogKind::Step, "Connecting to \x1b[2Jevil\x07");
        assert!(line.contains("Connecting to  [2Jevil "));
        assert_eq!(line.matches('\x1b').count(), 6, "only the log's own colour codes: {line:?}");
    }
}
