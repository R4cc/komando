//! Signing in with the SSH Agent's keys, and Agent Forwarding. Each test runs an in-process
//! SSH server and an agent on a Unix socket (russh's own), and drives the real Session path
//! (`session::ssh::spawn`).
#![cfg(unix)]

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use russh::keys::agent::client::AgentClient;
use russh::keys::ssh_key::private::Ed25519Keypair;
use russh::keys::{HashAlg, PrivateKey, PublicKey};
use russh::server::{self, Msg, Server as _, Session};
use russh::{Channel, ChannelId, MethodKind, MethodSet};

use komando::model::AgentUse;
use komando::session::agent::Agent;
use komando::session::ssh::{Hop, Target};
use komando::session::{EventProxy, GridSize, HopStep, Prompt, SessionEvent, Status, new_term};
use komando::vault::Secret;

const PASSWORD: &str = "secret";

fn key(seed: u8) -> PrivateKey {
    PrivateKey::from(Ed25519Keypair::from_seed(&[seed; 32]))
}

fn fingerprint(key: &PrivateKey) -> String {
    key.public_key().fingerprint(HashAlg::Sha256).to_string()
}

/// What the server saw.
#[derive(Default)]
struct Seen {
    /// Every key offered, in order.
    offered: Vec<PublicKey>,
    /// The client asked for Agent Forwarding.
    agent_requested: bool,
    /// How many keys the forwarded agent listed to the server.
    forwarded_keys: Option<usize>,
    /// Whether the client took the agent channel the server opened.
    agent_channel_accepted: Option<bool>,
}

#[derive(Clone)]
struct TestServer {
    /// The one key it takes.
    accepts: PublicKey,
    /// Open an agent channel once the shell starts, even if the client didn't ask for one.
    push_agent: bool,
    seen: Arc<Mutex<Seen>>,
}

/// How the test server behaves.
#[derive(Clone, Copy, Default)]
struct Opts {
    /// It takes `PASSWORD` too.
    password: bool,
    /// Tries before it hangs up (0: russh's default).
    max_tries: usize,
    push_agent: bool,
}

impl server::Server for TestServer {
    type Handler = Self;
    fn new_client(&mut self, _: Option<std::net::SocketAddr>) -> Self {
        self.clone()
    }
}

impl server::Handler for TestServer {
    type Error = russh::Error;

    async fn auth_password(&mut self, _: &str, password: &str) -> Result<server::Auth, Self::Error> {
        Ok(if password == PASSWORD { server::Auth::Accept } else { server::Auth::reject() })
    }

    async fn auth_publickey_offered(&mut self, _: &str, offer: server::PublicKeyOffer<'_>) -> Result<server::Auth, Self::Error> {
        self.seen.lock().unwrap().offered.push(offer.public_key.clone());
        Ok(if *offer.public_key == self.accepts { server::Auth::Accept } else { server::Auth::reject() })
    }

    async fn auth_publickey(&mut self, _: &str, key: server::VerifiedPublicKey<'_>) -> Result<server::Auth, Self::Error> {
        Ok(if *key.public_key == self.accepts { server::Auth::Accept } else { server::Auth::reject() })
    }

    async fn channel_open_session(&mut self, _: Channel<Msg>, reply: server::ChannelOpenHandle, _: &mut Session) -> Result<(), Self::Error> {
        reply.accept().await;
        Ok(())
    }

    async fn agent_request(&mut self, _: ChannelId, _: &mut Session) -> Result<bool, Self::Error> {
        self.seen.lock().unwrap().agent_requested = true;
        Ok(true)
    }

    /// With Agent Forwarding on, ask the forwarded agent for its keys, as `ssh-add -l` on the
    /// server would.
    async fn shell_request(&mut self, channel: ChannelId, session: &mut Session) -> Result<(), Self::Error> {
        let _ = session.channel_success(channel);
        if self.push_agent || self.seen.lock().unwrap().agent_requested {
            let handle = session.handle();
            let seen = self.seen.clone();
            tokio::spawn(async move {
                let opened = handle.channel_open_agent().await;
                seen.lock().unwrap().agent_channel_accepted = Some(opened.is_ok());
                let Ok(channel) = opened else { return };
                let mut agent = AgentClient::connect(channel.into_stream());
                if let Ok(keys) = agent.request_identities().await {
                    seen.lock().unwrap().forwarded_keys = Some(keys.len());
                }
            });
        }
        Ok(())
    }
}

struct Fixture {
    dir: std::path::PathBuf,
    socket: std::path::PathBuf,
    port: u16,
    seen: Arc<Mutex<Seen>>,
    _runtime: tokio::runtime::Runtime,
}

/// An agent holding `keys` and a server taking `accepts` (and the password, if `password`).
fn start(name: &str, keys: &[PrivateKey], accepts: &PrivateKey, opts: Opts) -> Fixture {
    let dir = std::env::temp_dir().join(format!("komando-agent-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let runtime = tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build().unwrap();

    let socket = dir.join("agent.sock");
    let listener = runtime.block_on(async { tokio::net::UnixListener::bind(&socket) }).unwrap();
    let connections = futures::stream::unfold(listener, |listener| async move {
        let accepted = listener.accept().await.map(|(stream, _)| stream);
        Some((accepted, listener))
    });
    runtime.spawn(async move {
        let _ = russh::keys::agent::server::serve(Box::pin(connections), ()).await;
    });
    runtime.block_on(async {
        let mut client = AgentClient::connect_uds(&socket).await.unwrap();
        for key in keys {
            client.add_identity(key, &[]).await.unwrap();
        }
    });

    let methods: &[MethodKind] = if opts.password { &[MethodKind::Password, MethodKind::PublicKey] } else { &[MethodKind::PublicKey] };
    let config = Arc::new(server::Config {
        keys: vec![key(9)],
        methods: MethodSet::from(methods),
        auth_rejection_time: Duration::ZERO,
        auth_rejection_time_initial: Some(Duration::ZERO),
        max_auth_attempts: if opts.max_tries > 0 { opts.max_tries } else { server::Config::default().max_auth_attempts },
        ..Default::default()
    });
    let listener = runtime.block_on(tokio::net::TcpListener::bind("127.0.0.1:0")).unwrap();
    let port = listener.local_addr().unwrap().port();
    let seen = Arc::new(Mutex::new(Seen::default()));
    let mut server = TestServer { accepts: accepts.public_key().clone(), push_agent: opts.push_agent, seen: seen.clone() };
    runtime.spawn(async move {
        let _ = server.run_on_socket(config, &listener).await;
    });
    Fixture { dir, socket, port, seen, _runtime: runtime }
}

/// How connecting went.
struct Outcome {
    signed_in: bool,
    /// Questions asked, by label.
    asked: Vec<String>,
    ended: Option<String>,
}

/// Connect as alice to the fixture's server, signing in with `secret` and the agent's keys
/// (`agent_use` of them), then wait for `until` or 10 s.
fn connect(fixture: &Fixture, secret: Option<Secret>, agent_use: AgentUse, forward_agent: bool, until: impl Fn(&Seen) -> bool) -> Outcome {
    let size = GridSize { cols: 80, rows: 24, cell_width: 9, cell_height: 18 };
    let (tx, mut events) = futures::channel::mpsc::unbounded();
    let proxy = EventProxy::new(tx);
    let term = new_term(size, 1000, proxy.clone());
    let hop = Hop { name: "server".into(), address: "127.0.0.1".into(), port: fixture.port, user: Some("alice".into()), secret };
    let target = Target {
        hops: vec![hop],
        known_hosts: fixture.dir.join("known_hosts"),
        agent: Some(Agent::At(fixture.socket.to_string_lossy().into_owned())),
        host_agent: agent_use,
        forward_agent,
        ..Default::default()
    };
    let sink = komando::session::ssh::spawn(target, size, term, proxy);
    let mut outcome = Outcome { signed_in: false, asked: vec![], ended: None };
    let deadline = Instant::now() + Duration::from_secs(10);
    while outcome.ended.is_none() && !(outcome.signed_in && until(&fixture.seen.lock().unwrap())) {
        assert!(Instant::now() < deadline, "timed out; asked so far: {:?}", outcome.asked);
        let Ok(event) = events.try_recv() else {
            std::thread::sleep(Duration::from_millis(10));
            continue;
        };
        match event {
            SessionEvent::Prompt(Prompt::HostKey { reply, .. }) => {
                let _ = reply.send(true);
            }
            SessionEvent::Prompt(Prompt::Input { label, reply, .. }) => {
                outcome.asked.push(label);
                let _ = reply.send(None);
            }
            SessionEvent::Hop { step: HopStep::Done, .. } => outcome.signed_in = true,
            SessionEvent::Status(Status::Disconnected(reason)) => outcome.ended = Some(reason),
            _ => {}
        }
    }
    sink.close();
    outcome
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

#[test]
fn signs_in_with_a_key_the_agent_holds() {
    let (other, right) = (key(1), key(2));
    let fixture = start("any", &[other.clone(), right.clone()], &right, Opts::default());
    let outcome = connect(&fixture, None, AgentUse::Any, false, |_| true);
    assert!(outcome.signed_in, "ended: {:?}", outcome.ended);
    assert_eq!(outcome.asked, Vec::<String>::new());
}

#[test]
fn only_the_chosen_agent_key_is_offered() {
    let (other, right) = (key(1), key(2));
    let fixture = start("one", &[other.clone(), right.clone()], &right, Opts::default());
    let outcome = connect(&fixture, None, AgentUse::Key(fingerprint(&right)), false, |_| true);
    assert!(outcome.signed_in, "ended: {:?}", outcome.ended);
    assert_eq!(fixture.seen.lock().unwrap().offered, [right.public_key().clone()]);
}

#[test]
fn a_saved_key_goes_before_the_agents_keys() {
    let (agent_key, saved) = (key(1), key(2));
    let fixture = start("saved-key", &[agent_key], &saved, Opts::default());
    let pem = saved.to_openssh(russh::keys::ssh_key::LineEnding::LF).unwrap().to_string();
    let outcome = connect(&fixture, Some(Secret::PrivateKey { key: pem, passphrase: None }), AgentUse::Any, false, |_| true);
    assert!(outcome.signed_in, "ended: {:?}", outcome.ended);
    assert_eq!(fixture.seen.lock().unwrap().offered, [saved.public_key().clone()]);
}

#[test]
fn a_saved_password_goes_before_the_agents_keys() {
    // A server that counts every key as a failed try would refuse before the password.
    let keys: Vec<PrivateKey> = (1..=8).map(key).collect();
    let fixture = start("password", &keys, &key(20), Opts { password: true, ..Default::default() });
    let outcome = connect(&fixture, Some(Secret::Password { password: PASSWORD.into() }), AgentUse::Any, false, |_| true);
    assert!(outcome.signed_in, "ended: {:?}", outcome.ended);
    assert!(fixture.seen.lock().unwrap().offered.is_empty(), "no agent key before the saved password");
}

#[test]
fn a_host_without_the_agent_asks_for_a_password() {
    let right = key(2);
    let fixture = start("off", &[right.clone()], &right, Opts { password: true, ..Default::default() });
    let outcome = connect(&fixture, None, AgentUse::Off, false, |_| true);
    assert!(!outcome.signed_in);
    assert_eq!(outcome.asked, ["Password for alice@127.0.0.1"]);
    assert!(fixture.seen.lock().unwrap().offered.is_empty());
}

#[test]
fn a_key_the_agent_no_longer_holds_falls_back_to_a_password() {
    let right = key(2);
    let fixture = start("missing", &[right.clone()], &right, Opts { password: true, ..Default::default() });
    let outcome = connect(&fixture, None, AgentUse::Key(fingerprint(&key(3))), false, |_| true);
    assert!(!outcome.signed_in);
    assert_eq!(outcome.asked, ["Password for alice@127.0.0.1"]);
    assert!(fixture.seen.lock().unwrap().offered.is_empty());
}

#[test]
fn the_agent_is_forwarded_to_the_host_when_switched_on() {
    let (other, right) = (key(1), key(2));
    let fixture = start("forward", &[other, right.clone()], &right, Opts::default());
    let outcome = connect(&fixture, None, AgentUse::Any, true, |seen| seen.forwarded_keys.is_some());
    assert!(outcome.signed_in, "ended: {:?}", outcome.ended);
    let seen = fixture.seen.lock().unwrap();
    assert!(seen.agent_requested);
    assert_eq!(seen.forwarded_keys, Some(2), "the server lists the agent's keys through the forward");
}

#[test]
fn the_agent_is_not_forwarded_unless_switched_on() {
    let right = key(2);
    let fixture = start("no-forward", &[right.clone()], &right, Opts::default());
    let outcome = connect(&fixture, None, AgentUse::Any, false, |_| true);
    assert!(outcome.signed_in, "ended: {:?}", outcome.ended);
    // Give a forward request time to arrive, if one were sent.
    std::thread::sleep(Duration::from_millis(300));
    assert!(!fixture.seen.lock().unwrap().agent_requested);
}

#[test]
fn a_rejected_saved_password_lets_the_agent_try_before_asking() {
    let right = key(2);
    let fixture = start("stale", &[right.clone()], &right, Opts { password: true, ..Default::default() });
    let outcome = connect(&fixture, Some(Secret::Password { password: "old".into() }), AgentUse::Any, false, |_| true);
    assert!(outcome.signed_in, "ended: {:?}", outcome.ended);
    assert_eq!(outcome.asked, Vec::<String>::new());
}

#[test]
fn an_agent_channel_the_client_didnt_ask_for_is_refused() {
    let right = key(2);
    let fixture = start("pushed", &[right.clone()], &right, Opts { push_agent: true, ..Default::default() });
    let outcome = connect(&fixture, None, AgentUse::Any, false, |seen| seen.agent_channel_accepted.is_some());
    assert!(outcome.signed_in, "ended: {:?}", outcome.ended);
    let seen = fixture.seen.lock().unwrap();
    assert!(!seen.agent_requested);
    assert_eq!(seen.agent_channel_accepted, Some(false), "the server must not reach the agent");
}

#[test]
fn a_server_that_hangs_up_after_too_many_keys_says_to_choose_one() {
    // The right key joins the agent only after the first try: the agent lists its keys in no
    // set order, so holding it from the start would sometimes offer it within the server's limit.
    let keys: Vec<PrivateKey> = (1..=5).map(key).collect();
    let fixture = start("too-many", &keys, &key(6), Opts { max_tries: 3, ..Default::default() });
    let outcome = connect(&fixture, None, AgentUse::Any, false, |_| true);
    assert!(!outcome.signed_in);
    let reason = outcome.ended.unwrap();
    assert!(reason.contains("closed the connection after"), "{reason}");
    tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap().block_on(async {
        let mut client = AgentClient::connect_uds(&fixture.socket).await.unwrap();
        client.add_identity(&key(6), &[]).await.unwrap();
    });
    // Choosing the key lets it in.
    let outcome = connect(&fixture, None, AgentUse::Key(fingerprint(&key(6))), false, |_| true);
    assert!(outcome.signed_in, "ended: {:?}", outcome.ended);
}
