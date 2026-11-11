//! SSH Sessions with the embedded client (russh); see ADR 0002.
//!
//! Network bytes go straight into the terminal parser on a tokio worker. No local shell and
//! no ConPTY are involved.

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

use super::{EventProxy, GridSize, InputSink, Prompt, SessionEvent, SharedTerm, Status};
use crate::vault::Secret;

pub enum Command {
    Input(Vec<u8>),
    Resize(GridSize),
    Close,
}

/// Everything a connection needs, resolved on the UI thread before it starts.
pub struct Target {
    pub address: String,
    pub port: u16,
    pub user: Option<String>,
    /// The saved Credential's secret, if the Vault is unlocked and one is configured.
    pub secret: Option<Secret>,
    /// Komando's own trusted Host Keys.
    pub known_hosts: PathBuf,
}

fn runtime() -> &'static tokio::runtime::Runtime {
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
        let reason = match run(target, size, term, proxy.clone(), rx).await {
            Ok(reason) => reason,
            Err(err) => format!("{err:#}"),
        };
        proxy.send(SessionEvent::Status(Status::Disconnected(reason)));
    });
    sink
}

struct Client {
    host: String,
    port: u16,
    known_hosts: PathBuf,
    proxy: EventProxy,
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
                    if path != &self.known_hosts {
                        let _ = russh::keys::known_hosts::learn_known_hosts_path(&self.host, self.port, key, &self.known_hosts);
                    }
                    return Ok(true);
                }
                Ok(false) => {}
                Err(russh::keys::Error::KeyChanged { .. }) => {
                    self.proxy.send(SessionEvent::Prompt(Prompt::HostKeyChanged { fingerprint }));
                    return Err(anyhow!(
                        "The host key of {} has changed. Connection blocked: this can mean someone is intercepting the connection.",
                        self.host
                    ));
                }
                Err(err) => log::warn!("reading {}: {err}", path.display()),
            }
        }

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
            Ok(true)
        } else {
            Err(anyhow!("Host key not trusted."))
        }
    }
}

async fn ask(proxy: &EventProxy, label: &str, secret: bool, error: Option<String>) -> Option<String> {
    let (reply, answer) = oneshot::channel();
    proxy.send(SessionEvent::Prompt(Prompt::Input { label: label.into(), secret, error, reply }));
    answer.await.ok().flatten()
}

async fn run(
    target: Target,
    mut size: GridSize,
    term: SharedTerm,
    proxy: EventProxy,
    mut commands: mpsc::UnboundedReceiver<Command>,
) -> Result<String> {
    let config = Arc::new(client::Config {
        inactivity_timeout: None,
        keepalive_interval: Some(Duration::from_secs(30)),
        keepalive_max: 4,
        nodelay: true,
        ..Default::default()
    });
    let handler = Client {
        host: target.address.clone(),
        port: target.port,
        known_hosts: target.known_hosts.clone(),
        proxy: proxy.clone(),
    };
    let mut handle = tokio::time::timeout(
        Duration::from_secs(15),
        client::connect(config, (target.address.as_str(), target.port), handler),
    )
    .await
    .map_err(|_| anyhow!("Timed out connecting to {}:{}.", target.address, target.port))??;

    let user = match target.user.clone() {
        Some(user) => user,
        None => ask(&proxy, "User name", false, None).await.ok_or_else(|| anyhow!("Cancelled."))?,
    };
    authenticate(&mut handle, &user, target.secret.as_ref(), &proxy).await?;

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
                    output.feed(&term, &proxy, &data);
                }
                Some(ChannelMsg::ExitStatus { exit_status: code }) => exit_status = Some(code),
                // The exit status may still follow the end of output; wait for Close.
                Some(ChannelMsg::Eof) => {}
                Some(ChannelMsg::Close) | None => {
                    output.flush(&term, &proxy);
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
                    if new_size != size {
                        size = new_size;
                        channel
                            .window_change(size.cols as u32, size.rows as u32, size.pixel_width(), size.pixel_height())
                            .await?;
                    }
                }
                Some(Command::Close) | None => {
                    let _ = handle.disconnect(Disconnect::ByApplication, "", "en").await;
                    break "Session closed.".to_string();
                }
            },
            // A program began a synchronized update (DECSET 2026) and did not finish it in
            // time: show what it drew so far rather than freezing the screen.
            _ = sleep_until(sync_deadline), if sync_deadline.is_some() => output.flush(&term, &proxy),
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

async fn authenticate(handle: &mut Handle<Client>, user: &str, secret: Option<&Secret>, proxy: &EventProxy) -> Result<()> {
    let mut last_error = None;
    match secret {
        Some(Secret::PrivateKey { key, passphrase }) => {
            let decoded = match russh::keys::decode_secret_key(key, passphrase.as_deref()) {
                Ok(decoded) => Ok(decoded),
                Err(_) if passphrase.is_none() => {
                    let pass = ask(proxy, "Key passphrase", true, None).await.ok_or_else(|| anyhow!("Cancelled."))?;
                    russh::keys::decode_secret_key(key, Some(&pass))
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
                        return Ok(());
                    }
                    last_error = Some("The server rejected the saved key.".to_string());
                }
                Err(err) => last_error = Some(format!("Could not read the saved key: {err}")),
            }
        }
        Some(Secret::Password { password }) => {
            if matches!(handle.authenticate_password(user, password.as_str()).await?, AuthResult::Success) {
                return Ok(());
            }
            last_error = Some("The saved password was rejected.".to_string());
        }
        None => {}
    }

    // Fall back to asking. A typed password that works is offered for saving.
    for _ in 0..3 {
        let label = format!("Password for {user}");
        let Some(password) = ask(proxy, &label, true, last_error.take()).await else {
            return Err(anyhow!("Cancelled."));
        };
        if matches!(handle.authenticate_password(user, password.as_str()).await?, AuthResult::Success) {
            proxy.send(SessionEvent::OfferSave { password });
            return Ok(());
        }
        last_error = Some("Wrong password. Try again.".to_string());
    }
    Err(anyhow!("Authentication failed."))
}
