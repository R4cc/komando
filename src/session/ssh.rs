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
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use alacritty_terminal::event::{Event, EventListener};
use alacritty_terminal::vte::ansi::Processor;
use anyhow::{Result, anyhow};
use futures::channel::oneshot;
use russh::client::{self, AuthResult, Handle};
use russh::keys::{HashAlg, PrivateKeyWithHashAlg, PublicKey, PublicKeyOrCertificate};
use russh::{ChannelMsg, Disconnect, Pty};
use tokio::sync::mpsc;

use super::{Answer, EventProxy, GridSize, HopStep, InputSink, Prompt, Remembered, SessionEvent, SharedTerm, Status};
use crate::vault::Secret;

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

/// The connection's progress, written into the terminal above where the shell will start
/// (and kept in its Scrollback): where it is connecting, the Host Key, how it signs in.
#[derive(Clone)]
pub(crate) struct ConnectLog {
    term: SharedTerm,
    proxy: EventProxy,
}

impl ConnectLog {
    pub(crate) fn new(term: SharedTerm, proxy: EventProxy) -> Self {
        Self { term, proxy }
    }

    pub(crate) fn line(&self, kind: LogKind, text: &str) {
        let mut parser: Processor = Processor::new();
        parser.advance(&mut *self.term.lock(), log_line(kind, text).as_bytes());
        self.proxy.send_event(Event::Wakeup);
    }
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

pub(crate) fn runtime() -> &'static tokio::runtime::Runtime {
    static RUNTIME: OnceLock<tokio::runtime::Runtime> = OnceLock::new();
    RUNTIME.get_or_init(|| {
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
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
        nodelay: true,
        ..Default::default()
    });
    // Every hop's connection stays open: the next one runs through it.
    let mut handles: Vec<Handle<Client>> = Vec::with_capacity(target.hops.len());
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
            None => tokio::time::timeout(CONNECT_TIMEOUT, client::connect(config.clone(), (hop.address.as_str(), hop.port), handler))
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
        let remember_password = authenticate(&mut handle, &user, hop.secret.as_ref(), &proxy, &log).await?;
        log.line(LogKind::Good, &format!("Signed in to {} as {user}", hop.name));
        if remember_user {
            proxy.send(SessionEvent::Remember { hop: index, what: Remembered::User(user.clone()) });
        }
        if let Some(password) = remember_password {
            proxy.send(SessionEvent::Remember { hop: index, what: Remembered::Password(password) });
        }
        proxy.send(SessionEvent::Hop { index, step: HopStep::Done });
        handles.push(handle);
    }
    // Shared with SFTP channels opened on the same connection.
    let handle = Arc::new(handles.pop().ok_or_else(|| anyhow!("Nothing to connect to."))?);
    let result = shell(&handle, &mut size, &term, &proxy, &mut commands).await;
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
) -> Result<String> {
    let mut channel = handle.channel_open_session().await?;
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
                Some(ChannelMsg::Data { data }) | Some(ChannelMsg::ExtendedData { data, .. }) => {
                    output.feed(term, proxy, &data);
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
                Some(Command::Input(bytes)) => channel.data_bytes(bytes).await?,
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

async fn authenticate(
    handle: &mut Handle<Client>,
    user: &str,
    secret: Option<&Secret>,
    proxy: &EventProxy,
    log: &ConnectLog,
) -> Result<Option<String>> {
    let mut last_error = None;
    match secret {
        Some(Secret::PrivateKey { key, passphrase }) => {
            log.line(LogKind::Step, &format!("Signing in as {user} with the saved key"));
            let decoded = match russh::keys::decode_secret_key(key, passphrase.as_deref()) {
                Ok(decoded) => Ok(decoded),
                Err(_) if passphrase.is_none() => {
                    let pass = ask(proxy, "Key passphrase", true, None, None).await.ok_or_else(|| anyhow!("Cancelled."))?;
                    russh::keys::decode_secret_key(key, Some(&pass.value))
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
                        return Ok(None);
                    }
                    last_error = Some("The server rejected the saved key.".to_string());
                }
                Err(err) => last_error = Some(format!("Could not read the saved key: {err}")),
            }
        }
        Some(Secret::Password { password }) => {
            log.line(LogKind::Step, &format!("Signing in as {user} with the saved password"));
            if matches!(handle.authenticate_password(user, password.as_str()).await?, AuthResult::Success) {
                return Ok(None);
            }
            last_error = Some("The saved password was rejected.".to_string());
        }
        None => {}
    }
    if let Some(error) = &last_error {
        log.line(LogKind::Warn, error);
    }
    log.line(LogKind::Step, &format!("Signing in as {user} with a password: waiting for it"));

    // Fall back to asking. A typed password that works is returned when "Remember" was
    // ticked, for saving; the tick carries over to the next try.
    let mut remember = false;
    for _ in 0..3 {
        let label = format!("Password for {user}");
        let Some(answer) = ask(proxy, &label, true, Some(remember), last_error.take()).await else {
            return Err(anyhow!("Cancelled."));
        };
        remember = answer.remember;
        if matches!(handle.authenticate_password(user, answer.value.as_str()).await?, AuthResult::Success) {
            return Ok(remember.then_some(answer.value));
        }
        log.line(LogKind::Warn, "Wrong password");
        last_error = Some("Wrong password. Try again.".to_string());
    }
    Err(anyhow!("Authentication failed."))
}

#[cfg(test)]
mod tests {
    use super::{LogKind, log_line};

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
