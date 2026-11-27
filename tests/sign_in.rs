//! Signing in to servers that don't take a plain password: keyboard-interactive only, as
//! network gear with RADIUS or TACACS+ often is, and a password followed by a one-time code.
//! Each test runs an in-process SSH server and drives the real Session path
//! (`session::ssh::spawn`), answering its questions the way a person would.

use std::borrow::Cow;
use std::time::{Duration, Instant};

use russh::server::{self, Msg, Response, Session};
use russh::{Channel, MethodKind, MethodSet};

use komando::session::{Answer, EventProxy, GridSize, HopStep, Prompt, Remembered, SessionEvent, Status, new_term};
use komando::vault::Secret;

#[path = "support/ssh_server.rs"]
mod ssh_server;

const PASSWORD: &str = "secret";
const CODE: &str = "123456";

#[derive(Clone, Copy)]
enum Mode {
    /// Takes only keyboard-interactive, asking "Password:", like a switch signing in through
    /// RADIUS.
    Interactive,
    /// A password, then a one-time code by keyboard-interactive.
    PasswordThenCode,
    /// Keys only.
    KeysOnly,
    /// Offers keyboard-interactive before the password but turns it down without a question,
    /// like sshd with it switched on and no PAM behind it.
    EmptyInteractiveThenPassword,
}

#[derive(Clone)]
struct TestServer {
    mode: Mode,
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
        Ok(match self.mode {
            Mode::EmptyInteractiveThenPassword if password == PASSWORD => server::Auth::Accept,
            Mode::PasswordThenCode if password == PASSWORD => server::Auth::Reject {
                proceed_with_methods: Some(MethodSet::from(&[MethodKind::KeyboardInteractive][..])),
                partial_success: true,
            },
            _ => server::Auth::reject(),
        })
    }

    async fn auth_keyboard_interactive<'a>(
        &'a mut self,
        _: &str,
        _: &str,
        response: Option<Response<'a>>,
    ) -> Result<server::Auth, Self::Error> {
        let (question, instructions, expected) = match self.mode {
            Mode::Interactive => ("Password: ", "", PASSWORD),
            Mode::PasswordThenCode => ("Verification code: ", "Enter the code from your app.", CODE),
            Mode::KeysOnly | Mode::EmptyInteractiveThenPassword => return Ok(server::Auth::reject()),
        };
        Ok(match response {
            None => server::Auth::Partial {
                name: Cow::Borrowed(""),
                instructions: Cow::Borrowed(instructions),
                prompts: Cow::Owned(vec![(Cow::Borrowed(question), false)]),
            },
            Some(mut answers) => {
                let answer = answers.next();
                if answer.as_deref() == Some(expected.as_bytes()) { server::Auth::Accept } else { server::Auth::reject() }
            }
        })
    }

    async fn channel_open_session(
        &mut self,
        _: Channel<Msg>,
        reply: server::ChannelOpenHandle,
        _: &mut Session,
    ) -> Result<(), Self::Error> {
        reply.accept().await;
        Ok(())
    }
}

/// The seed of the server's host key.
const HOST_KEY_SEED: u8 = 9;

/// The server's host key.
fn host_key() -> russh::keys::PrivateKey {
    ssh_server::key(HOST_KEY_SEED)
}

/// Starts a server on its own runtime and returns its port.
fn start(mode: Mode) -> (u16, tokio::runtime::Runtime) {
    start_with(mode, vec![])
}

/// A server that also has the host `certificates`.
fn start_with(mode: Mode, certificates: Vec<russh::keys::Certificate>) -> (u16, tokio::runtime::Runtime) {
    let methods: &[MethodKind] = match mode {
        Mode::Interactive => &[MethodKind::KeyboardInteractive],
        Mode::PasswordThenCode => &[MethodKind::Password],
        Mode::KeysOnly => &[MethodKind::PublicKey],
        Mode::EmptyInteractiveThenPassword => &[MethodKind::KeyboardInteractive, MethodKind::Password],
    };
    let config = server::Config { certificates, methods: MethodSet::from(methods), ..ssh_server::config(HOST_KEY_SEED) };
    ssh_server::start(TestServer { mode }, config, 1)
}

/// A question the Session asked: its label, whether it was hidden, the "Remember" box, and
/// the error shown with it.
#[derive(Debug, Clone, PartialEq)]
struct Asked {
    label: String,
    secret: bool,
    remember: Option<bool>,
    error: Option<String>,
}

/// How signing in went.
struct Outcome {
    asked: Vec<Asked>,
    signed_in: bool,
    remembered: Option<String>,
    /// Why the session ended, if it did before signing in.
    ended: Option<String>,
    /// The Host Key question came up (and was answered with Trust).
    host_key_asked: bool,
}

/// Signs in to the server on `port` as alice, answering its questions in turn with `answers`
/// (the text, and whether "Remember" is ticked).
fn sign_in(name: &str, port: u16, secret: Option<Secret>, answers: &[(&str, bool)]) -> Outcome {
    sign_in_trusting(name, port, secret, answers, "")
}

/// As `sign_in`, with Komando's known_hosts holding `known_hosts` first.
fn sign_in_trusting(name: &str, port: u16, secret: Option<Secret>, answers: &[(&str, bool)], known_hosts: &str) -> Outcome {
    let dir = std::env::temp_dir().join(format!("komando-sign-in-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("known_hosts"), known_hosts).unwrap();
    let size = GridSize { cols: 80, rows: 24, cell_width: 9, cell_height: 18 };
    let (tx, mut events) = futures::channel::mpsc::unbounded();
    let proxy = EventProxy::new(tx);
    let term = new_term(size, 1000, proxy.clone());
    let hop = komando::session::ssh::Hop {
        name: "switch".into(),
        address: "127.0.0.1".into(),
        port,
        user: Some("alice".into()),
        secret,
    };
    let target = komando::session::ssh::Target { hops: vec![hop], known_hosts: dir.join("known_hosts"), forwards: vec![], ..Default::default() };
    let sink = komando::session::ssh::spawn(target, size, term, proxy);
    let mut outcome = Outcome { asked: vec![], signed_in: false, remembered: None, ended: None, host_key_asked: false };
    let mut answers = answers.iter();
    let deadline = Instant::now() + Duration::from_secs(10);
    while !outcome.signed_in && outcome.ended.is_none() {
        assert!(Instant::now() < deadline, "timed out; asked so far: {:?}", outcome.asked);
        let Ok(event) = events.try_recv() else {
            std::thread::sleep(Duration::from_millis(10));
            continue;
        };
        match event {
            SessionEvent::Prompt(Prompt::HostKey { reply, .. }) => {
                outcome.host_key_asked = true;
                let _ = reply.send(true);
            }
            SessionEvent::Prompt(Prompt::Input { label, secret, remember, error, reply }) => {
                outcome.asked.push(Asked { label, secret, remember, error });
                let answer = answers.next().map(|(value, remember)| Answer { value: value.to_string(), remember: *remember });
                let _ = reply.send(answer);
            }
            SessionEvent::Remember { what: Remembered::Password(password), .. } => outcome.remembered = Some(password),
            SessionEvent::Hop { step: HopStep::Done, .. } => outcome.signed_in = true,
            SessionEvent::Status(Status::Disconnected(reason)) => outcome.ended = Some(reason),
            _ => {}
        }
    }
    sink.close();
    let _ = std::fs::remove_dir_all(&dir);
    outcome
}

fn password_question(error: Option<&str>, remember: bool) -> Asked {
    Asked {
        label: "Password for alice@127.0.0.1".into(),
        secret: true,
        remember: Some(remember),
        error: error.map(str::to_string),
    }
}

#[test]
fn keyboard_interactive_takes_the_saved_password() {
    let (port, _server) = start(Mode::Interactive);
    let outcome = sign_in("saved", port, Some(Secret::Password { password: PASSWORD.into() }), &[]);
    assert!(outcome.signed_in, "ended: {:?}", outcome.ended);
    assert_eq!(outcome.asked, []);
}

#[test]
fn keyboard_interactive_asks_for_the_password_like_the_password_method() {
    let (port, _server) = start(Mode::Interactive);
    let outcome = sign_in("typed", port, None, &[("wrong", false), (PASSWORD, true)]);
    assert!(outcome.signed_in, "ended: {:?}", outcome.ended);
    assert_eq!(outcome.asked, [password_question(None, false), password_question(Some("Wrong password. Try again."), false)]);
    assert_eq!(outcome.remembered.as_deref(), Some(PASSWORD), "the ticked password is saved");
}

#[test]
fn keyboard_interactive_asks_again_after_a_rejected_saved_password() {
    let (port, _server) = start(Mode::Interactive);
    let outcome = sign_in("stale", port, Some(Secret::Password { password: "old".into() }), &[(PASSWORD, false)]);
    assert!(outcome.signed_in, "ended: {:?}", outcome.ended);
    assert_eq!(outcome.asked, [password_question(Some("The saved password was rejected."), false)]);
    assert_eq!(outcome.remembered, None);
}

#[test]
fn keyboard_interactive_gives_up_after_three_wrong_passwords() {
    let (port, _server) = start(Mode::Interactive);
    let outcome = sign_in("wrong", port, None, &[("a", false), ("b", false), ("c", false), ("d", false)]);
    assert!(!outcome.signed_in);
    assert_eq!(outcome.asked.len(), 3);
    assert!(outcome.ended.is_some_and(|reason| reason.contains("Authentication failed")));
}

#[test]
fn keyboard_interactive_without_a_question_falls_back_to_the_password() {
    let (port, _server) = start(Mode::EmptyInteractiveThenPassword);
    let outcome = sign_in("empty", port, Some(Secret::Password { password: PASSWORD.into() }), &[]);
    assert!(outcome.signed_in, "ended: {:?}", outcome.ended);
    assert_eq!(outcome.asked, []);
}

#[test]
fn a_password_then_a_one_time_code() {
    let (port, _server) = start(Mode::PasswordThenCode);
    let outcome = sign_in("code", port, Some(Secret::Password { password: PASSWORD.into() }), &[(CODE, false)]);
    assert!(outcome.signed_in, "ended: {:?}", outcome.ended);
    let code = Asked { label: "Verification code (alice@127.0.0.1)".into(), secret: true, remember: None, error: None };
    assert_eq!(outcome.asked, [code]);
}

#[test]
fn a_server_without_passwords_says_what_it_takes() {
    let (port, _server) = start(Mode::KeysOnly);
    let outcome = sign_in("keys", port, None, &[]);
    assert!(!outcome.signed_in);
    assert_eq!(outcome.asked, []);
    let reason = outcome.ended.unwrap();
    assert!(reason.contains("takes no password, only: publickey"), "{reason}");
}

/// A host certificate for the test server's key, signed by the authority with `ca_seed`.
fn host_certificate(ca_seed: u8, principal: &str) -> (russh::keys::Certificate, String) {
    use russh::keys::ssh_key::certificate::{Builder, CertType};
    let ca = ssh_server::key(ca_seed);
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs();
    let mut builder = Builder::new([1u8; 16], host_key().public_key().key_data().clone(), now - 60, now + 3600).unwrap();
    builder.cert_type(CertType::Host).unwrap();
    builder.valid_principal(principal).unwrap();
    (builder.sign(&ca).unwrap(), ca.public_key().to_openssh().unwrap())
}

#[test]
fn a_host_certificate_from_a_trusted_authority_is_trusted_without_asking() {
    let (cert, ca) = host_certificate(21, "127.0.0.1");
    let (port, _server) = start_with(Mode::Interactive, vec![cert]);
    let trusted = format!("@cert-authority [127.0.0.1]:{port} {ca}\n");
    let outcome = sign_in_trusting("cert", port, None, &[(PASSWORD, false)], &trusted);
    assert!(outcome.signed_in, "ended: {:?}", outcome.ended);
    assert!(!outcome.host_key_asked);
}

#[test]
fn a_host_certificate_for_another_host_is_refused() {
    let (cert, ca) = host_certificate(21, "db.example.com");
    let (port, _server) = start_with(Mode::Interactive, vec![cert]);
    let trusted = format!("@cert-authority [127.0.0.1]:{port} {ca}\n");
    let outcome = sign_in_trusting("cert-other", port, None, &[(PASSWORD, false)], &trusted);
    assert!(!outcome.signed_in);
    let reason = outcome.ended.unwrap();
    assert!(reason.contains("can't be trusted: it is for db.example.com, not 127.0.0.1"), "{reason}");
}

#[test]
fn without_an_authority_a_host_with_a_certificate_is_asked_about_as_before() {
    let (cert, _) = host_certificate(21, "127.0.0.1");
    let (port, _server) = start_with(Mode::Interactive, vec![cert]);
    let outcome = sign_in("cert-none", port, None, &[(PASSWORD, false)]);
    assert!(outcome.signed_in, "ended: {:?}", outcome.ended);
    assert!(outcome.host_key_asked);
}

/// Security regression: a host key listed on an `@revoked` line is refused outright, without
/// the Host Key question a user could answer with Trust.
#[test]
fn a_revoked_host_key_is_refused_without_asking() {
    let (port, _server) = start(Mode::Interactive);
    let revoked = format!("@revoked * {}\n", host_key().public_key().to_openssh().unwrap());
    let outcome = sign_in_trusting("revoked", port, None, &[(PASSWORD, false)], &revoked);
    assert!(!outcome.signed_in);
    assert!(!outcome.host_key_asked, "a revoked key must not be offered for trust");
    let reason = outcome.ended.unwrap();
    assert!(reason.contains("marked @revoked"), "{reason}");
}

/// Security regression: a revoked key isn't trusted through a host certificate either, and a
/// revoked authority vouches for nothing.
#[test]
fn revoked_keys_and_authorities_are_not_trusted_through_certificates() {
    let (cert, ca) = host_certificate(21, "127.0.0.1");
    let (port, _server) = start_with(Mode::Interactive, vec![cert]);
    let host = host_key().public_key().to_openssh().unwrap();
    let trusted = format!("@cert-authority [127.0.0.1]:{port} {ca}\n@revoked * {host}\n");
    let outcome = sign_in_trusting("revoked-cert-key", port, None, &[(PASSWORD, false)], &trusted);
    assert!(!outcome.signed_in);
    assert!(!outcome.host_key_asked);
    let reason = outcome.ended.unwrap();
    assert!(reason.contains("marked @revoked"), "{reason}");

    let (cert, ca) = host_certificate(22, "127.0.0.1");
    let (port, _server) = start_with(Mode::Interactive, vec![cert]);
    let trusted = format!("@cert-authority [127.0.0.1]:{port} {ca}\n@revoked * {ca}\n");
    let outcome = sign_in_trusting("revoked-ca", port, None, &[(PASSWORD, false)], &trusted);
    // Only the plain host key is left, asked about as on a first connection.
    assert!(outcome.host_key_asked, "a revoked authority must not vouch for the host");
}
