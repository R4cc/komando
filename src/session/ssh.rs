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
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use alacritty_terminal::event::{Event, EventListener};
use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::index::Line;
use alacritty_terminal::term::Term;
use alacritty_terminal::term::cell::Flags;
use alacritty_terminal::term::TermMode;
use alacritty_terminal::vte::ansi::Processor;
use anyhow::{Result, anyhow};
use futures::channel::oneshot;
use russh::client::{self, AuthResult, Handle};
use russh::keys::{HashAlg, PrivateKeyWithHashAlg, PublicKey, PublicKeyOrCertificate};
use russh::{ChannelMsg, Disconnect, Pty};
use tokio::sync::mpsc;

use super::sudo::SudoPassword;
use super::{Answer, EventProxy, GridSize, HopStep, InputSink, Prompt, Remembered, SessionEvent, SharedTerm, Status};
use crate::vault::Secret;
use zeroize::Zeroizing;

pub enum Command {
    Input(Vec<u8>),
    Resize(GridSize),
    /// Open an SFTP channel on this Session's connection (no second sign-in).
    OpenSftp(oneshot::Sender<Result<crate::sftp::Remote>>),
    Close,
}

/// Everything a connection needs, resolved on the UI thread before it starts.
pub struct Target {
    /// The Hosts to sign in to, in order: Jump Hosts first, the Host with the shell last.
    pub hops: Vec<Hop>,
    /// Komando's own trusted Host Keys.
    pub known_hosts: PathBuf,
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
/// erased, so the shell starts on a clean screen; a connection that fails keeps it, to show why.
#[derive(Clone)]
pub(crate) struct ConnectLog {
    term: SharedTerm,
    proxy: EventProxy,
    /// Lines written since the last `clear`, shared by the clones every hop holds.
    lines: Arc<AtomicUsize>,
}

impl ConnectLog {
    pub(crate) fn new(term: SharedTerm, proxy: EventProxy) -> Self {
        Self { term, proxy, lines: Arc::default() }
    }

    pub(crate) fn line(&self, kind: LogKind, text: &str) {
        let mut parser: Processor = Processor::new();
        parser.advance(&mut *self.term.lock(), log_line(kind, text).as_bytes());
        self.lines.fetch_add(1, Ordering::Relaxed);
        self.proxy.send_event(Event::Wakeup);
    }

    /// Erase the lines written so far, as far as they are still on screen.
    pub(crate) fn clear(&self) {
        let lines = self.lines.swap(0, Ordering::Relaxed);
        if lines == 0 {
            return;
        }
        let mut term = self.term.lock();
        let rows = rows_above_cursor(&*term, lines);
        if rows == 0 {
            return;
        }
        // Back to the first log row, then erase from there to the end of the screen.
        let mut parser: Processor = Processor::new();
        parser.advance(&mut *term, format!("\r\x1b[{rows}A\x1b[J").as_bytes());
        drop(term);
        self.proxy.send_event(Event::Wakeup);
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

/// `name (address:port)`, or just `address:port` when the name is the address.
fn describe(hop: &Hop) -> String {
    let at = format!("{}:{}", hop.address, hop.port);
    if hop.name.is_empty() || hop.name == hop.address { at } else { format!("{} ({at})", hop.name) }
}

/// How long reaching one hop may take, Host Key questions aside.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);

/// How much of the server's output the operating system may hold for Komando: as much as the
/// SSH window lets the server send ahead (`window_size` in `run`).
///
/// With Windows' default receive buffer, output floods stalled: a server writing faster than the
/// buffer drained waited about one timer tick (15 ms) every couple of hundred kilobytes. Against
/// an asyncssh server on the same machine, 1,000,000 log lines took 3.5 s with the default and
/// 1.1 s with this buffer, with no stalls left.
const RECEIVE_BUFFER: u32 = 8 * 1024 * 1024;

/// A TCP connection to `address:port`, trying each address it resolves to in turn, with a
/// receive buffer of [`RECEIVE_BUFFER`].
async fn open_tcp(address: &str, port: u16) -> std::io::Result<tokio::net::TcpStream> {
    let mut last_error = None;
    for addr in tokio::net::lookup_host((address, port)).await? {
        let socket = if addr.is_ipv4() { tokio::net::TcpSocket::new_v4()? } else { tokio::net::TcpSocket::new_v6()? };
        // A smaller buffer only costs speed, so a refusal is not an error.
        if let Err(err) = socket.set_recv_buffer_size(RECEIVE_BUFFER) {
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
        let reason = match run(target, size, term, proxy.clone(), log.clone(), rx).await {
            Ok(reason) => reason,
            Err(err) => {
                let reason = format!("{err:#}");
                log.line(LogKind::Fail, &reason);
                reason
            }
        };
        proxy.send(SessionEvent::Status(Status::Disconnected(reason)));
    });
    sink
}

pub(crate) struct Client {
    host: String,
    port: u16,
    known_hosts: PathBuf,
    proxy: EventProxy,
    log: ConnectLog,
    /// Which hop this connection is, for progress events.
    hop: usize,
}

impl client::Handler for Client {
    type Error = anyhow::Error;

    async fn check_server_key(&mut self, server_key: &PublicKeyOrCertificate) -> Result<bool, Self::Error> {
        let key: &PublicKey = match server_key {
            PublicKeyOrCertificate::PublicKey { key, .. } => key,
            PublicKeyOrCertificate::Certificate(_) => {
                return Err(anyhow!("Host certificates are not supported yet."));
            }
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
                        let _ = russh::keys::known_hosts::learn_known_hosts_path(&self.host, self.port, key, &self.known_hosts);
                    }
                    return Ok(true);
                }
                Ok(false) => {}
                Err(russh::keys::Error::KeyChanged { .. }) => {
                    self.log.line(LogKind::Fail, "Host key changed since you trusted it");
                    self.proxy.send(SessionEvent::Prompt(Prompt::HostKeyChanged { fingerprint }));
                    return Err(anyhow!(
                        "The host key of {} has changed. Connection blocked: this can mean someone is intercepting the connection.",
                        self.host
                    ));
                }
                Err(err) => log::warn!("reading {}: {err}", path.display()),
            }
        }

        self.log.line(LogKind::Warn, "First connection to this host: waiting for you to trust its key");
        let (reply, answer) = oneshot::channel();
        self.proxy.send(SessionEvent::Prompt(Prompt::HostKey {
            fingerprint,
            algorithm: key.algorithm().to_string(),
            reply,
        }));
        if answer.await.unwrap_or(false) {
            if let Some(dir) = self.known_hosts.parent() {
                std::fs::create_dir_all(dir)?;
            }
            russh::keys::known_hosts::learn_known_hosts_path(&self.host, self.port, key, &self.known_hosts)?;
            self.log.line(LogKind::Good, "Host key trusted");
            Ok(true)
        } else {
            Err(anyhow!("Host key not trusted."))
        }
    }
}

async fn ask(proxy: &EventProxy, label: &str, secret: bool, remember: Option<bool>, error: Option<String>) -> Option<Answer> {
    let (reply, answer) = oneshot::channel();
    proxy.send(SessionEvent::Prompt(Prompt::Input { label: label.into(), secret, remember, error, reply }));
    answer.await.ok().flatten()
}

async fn run(
    target: Target,
    mut size: GridSize,
    term: SharedTerm,
    proxy: EventProxy,
    log: ConnectLog,
    mut commands: mpsc::UnboundedReceiver<Command>,
) -> Result<String> {
    let config = Arc::new(client::Config {
        inactivity_timeout: None,
        keepalive_interval: Some(Duration::from_secs(30)),
        keepalive_max: 4,
        // How much the server may send before we acknowledge it. The default 2 MiB caps an
        // SFTP download at 2 MiB per round trip (40 MB/s at 50 ms) before anything else does.
        // `RECEIVE_BUFFER` matches it.
        window_size: 8 * 1024 * 1024,
        ..Default::default()
    });
    // Every hop's connection stays open: the next one runs through it.
    let mut handles: Vec<Handle<Client>> = Vec::with_capacity(target.hops.len());
    // The last hop's user and the password it signed in with, offered at sudo prompts.
    let mut sudo = None;
    for (index, hop) in target.hops.iter().enumerate() {
        proxy.send(SessionEvent::Hop { index, step: HopStep::Reaching });
        match index {
            0 => log.line(LogKind::Step, &format!("Connecting to {}", describe(hop))),
            _ => log.line(LogKind::Step, &format!("Opening a tunnel to {} through {}", describe(hop), target.hops[index - 1].name)),
        }
        let handler = Client {
            host: hop.address.clone(),
            port: hop.port,
            known_hosts: target.known_hosts.clone(),
            proxy: proxy.clone(),
            log: log.clone(),
            hop: index,
        };
        let timed_out = || anyhow!("Timed out connecting to {}:{}.", hop.address, hop.port);
        let mut handle = match handles.last() {
            None => tokio::time::timeout(CONNECT_TIMEOUT, async {
                let stream = open_tcp(&hop.address, hop.port).await?;
                client::connect_stream(config.clone(), stream, handler).await
            })
            .await
            .map_err(|_| timed_out())??,
            Some(through) => {
                let via = &target.hops[index - 1].name;
                let tunnel = tokio::time::timeout(
                    CONNECT_TIMEOUT,
                    through.channel_open_direct_tcpip(hop.address.as_str(), hop.port as u32, "127.0.0.1", 0),
                )
                .await
                .map_err(|_| timed_out())?
                .map_err(|err| anyhow!("{via} could not open a tunnel to {}:{}: {err}", hop.address, hop.port))?;
                tokio::time::timeout(CONNECT_TIMEOUT, client::connect_stream(config.clone(), tunnel.into_stream(), handler))
                    .await
                    .map_err(|_| timed_out())??
            }
        };
        let (user, remember_user) = match hop.user.clone() {
            Some(user) => (user, false),
            None => {
                log.line(LogKind::Step, "No user name saved for this host: waiting for one");
                let answer = ask(&proxy, &format!("User name for {}", hop.name), false, Some(false), None)
                    .await
                    .ok_or_else(|| anyhow!("Cancelled."))?;
                (answer.value, answer.remember)
            }
        };
        proxy.send(SessionEvent::Hop { index, step: HopStep::SigningIn });
        // Saved only once it signed in, so a mistyped user name is not remembered. Before the
        // password, so the password's vault entry is labelled with it.
        let signed_in = authenticate(&mut handle, &user, hop.secret.as_ref(), &proxy, &log).await?;
        log.line(LogKind::Good, &format!("Signed in to {} as {user}", hop.name));
        if remember_user {
            proxy.send(SessionEvent::Remember { hop: index, what: Remembered::User(user.clone()) });
        }
        if let Some(password) = &signed_in.password {
            if signed_in.remember {
                proxy.send(SessionEvent::Remember { hop: index, what: Remembered::Password(password.to_string()) });
            }
        }
        sudo = signed_in.password.map(|password| SudoPassword::new(user, password));
        proxy.send(SessionEvent::Hop { index, step: HopStep::Done });
        handles.push(handle);
    }
    // Signed in everywhere: the shell starts where the log began.
    log.clear();
    // Shared with SFTP channels opened on the same connection.
    let handle = Arc::new(handles.pop().ok_or_else(|| anyhow!("Nothing to connect to."))?);
    let result = shell(&handle, &mut size, &term, &proxy, &mut commands, sudo).await;
    // Always end the connection with the terminal, so SFTP channels opened on it end too.
    let _ = handle.disconnect(Disconnect::ByApplication, "", "en").await;
    // The Jump Hosts' connections, innermost first.
    for jump in handles.iter().rev() {
        let _ = jump.disconnect(Disconnect::ByApplication, "", "en").await;
    }
    result
}

async fn shell(
    handle: &Arc<Handle<Client>>,
    size: &mut GridSize,
    term: &SharedTerm,
    proxy: &EventProxy,
    commands: &mut mpsc::UnboundedReceiver<Command>,
    mut sudo: Option<SudoPassword>,
) -> Result<String> {
    let mut channel = handle.channel_open_session().await?;
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
    channel.request_shell(true).await?;
    proxy.send(SessionEvent::Status(Status::Connected));

    let mut output = Output::default();
    let mut exit_status = None;
    let reason = loop {
        let sync_deadline = output.sync_deadline();
        tokio::select! {
            msg = channel.wait() => match msg {
                Some(ChannelMsg::Data { data }) | Some(ChannelMsg::ExtendedData { data, .. }) => match &mut sudo {
                    Some(sudo) => show_watching_sudo(&mut output, term, proxy, sudo, &data),
                    None => output.feed(term, proxy, &data),
                },
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
                Some(Command::Input(bytes)) => match &mut sudo {
                    Some(sudo) => {
                        let typed = sudo.typed(bytes);
                        if typed.clear {
                            output.feed(term, proxy, super::sudo::CLEAR);
                        }
                        channel.data(&typed.send[..]).await?;
                    }
                    None => channel.data_bytes(bytes).await?,
                },
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
                Some(Command::Close) | None => break "Session closed.".to_string(),
            },
            // A program began a synchronized update (DECSET 2026) and did not finish it in
            // time: show what it drew so far rather than freezing the screen.
            _ = sleep_until(sync_deadline), if sync_deadline.is_some() => output.flush(term, proxy),
        }
    };
    Ok(reason)
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
pub(crate) struct Output {
    parser: Processor,
}

impl Output {
    pub(crate) fn feed(&mut self, term: &SharedTerm, proxy: &EventProxy, bytes: &[u8]) {
        self.parser.advance(&mut *term.lock(), bytes);
        if self.parser.sync_bytes_count() < bytes.len() {
            proxy.send_event(Event::Wakeup);
        }
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

async fn authenticate(
    handle: &mut Handle<Client>,
    user: &str,
    secret: Option<&Secret>,
    proxy: &EventProxy,
    log: &ConnectLog,
) -> Result<SignedIn> {
    let mut last_error = None;
    match secret {
        Some(Secret::PrivateKey { key, passphrase }) => {
            log.line(LogKind::Step, &format!("Signing in as {user} with the saved key"));
            let decoded = match russh::keys::decode_secret_key(key, passphrase.as_deref()) {
                Ok(decoded) => Ok(decoded),
                Err(_) if passphrase.is_none() => {
                    let pass = ask(proxy, "Key passphrase", true, None, None).await.ok_or_else(|| anyhow!("Cancelled."))?;
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
                    if matches!(result, AuthResult::Success) {
                        return Ok(SignedIn { password: None, remember: false });
                    }
                    last_error = Some("The server rejected the saved key.".to_string());
                }
                Err(err) => last_error = Some(format!("Could not read the saved key: {err}")),
            }
        }
        Some(Secret::Password { password }) => {
            log.line(LogKind::Step, &format!("Signing in as {user} with the saved password"));
            if matches!(handle.authenticate_password(user, password.as_str()).await?, AuthResult::Success) {
                return Ok(SignedIn { password: Some(Zeroizing::new(password.clone())), remember: false });
            }
            last_error = Some("The saved password was rejected.".to_string());
        }
        None => {}
    }
    if let Some(error) = &last_error {
        log.line(LogKind::Warn, error);
    }
    log.line(LogKind::Step, &format!("Signing in as {user} with a password: waiting for it"));

    // Fall back to asking. The tick on "Remember" carries over to the next try.
    let mut remember = false;
    for _ in 0..3 {
        let label = format!("Password for {user}");
        let Some(answer) = ask(proxy, &label, true, Some(remember), last_error.take()).await else {
            return Err(anyhow!("Cancelled."));
        };
        remember = answer.remember;
        let password = Zeroizing::new(answer.value);
        if matches!(handle.authenticate_password(user, password.as_str()).await?, AuthResult::Success) {
            return Ok(SignedIn { password: Some(password), remember });
        }
        log.line(LogKind::Warn, "Wrong password");
        last_error = Some("Wrong password. Try again.".to_string());
    }
    Err(anyhow!("Authentication failed."))
}

#[cfg(test)]
mod tests {
    use super::{ConnectLog, LogKind, Output, SudoPassword, log_line, show_watching_sudo};
    use crate::session::{EventProxy, GridSize, new_term, scrollback_text};

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
    fn log_lines_are_tagged_and_end_the_line() {
        let line = log_line(LogKind::Good, "Signed in to web01 as ana");
        assert!(line.starts_with("\x1b[0m\x1b[90mSSH\x1b[0m "));
        assert!(line.contains("✓"));
        assert!(line.ends_with("Signed in to web01 as ana\x1b[0m\r\n"));
    }

    #[test]
    fn log_lines_cannot_carry_escape_sequences() {
        // A Host named with an escape sequence must not clear the screen or retitle the tab.
        let line = log_line(LogKind::Step, "Connecting to \x1b[2Jevil\x07");
        assert!(line.contains("Connecting to  [2Jevil "));
        assert_eq!(line.matches('\x1b').count(), 6, "only the log's own colour codes: {line:?}");
    }
}
