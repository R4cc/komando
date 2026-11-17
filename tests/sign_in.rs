//! Signing in to servers that don't take a plain password: keyboard-interactive only, as
//! network gear with RADIUS or TACACS+ often is, and a password followed by a one-time code.
//! Each test runs an in-process SSH server and drives the real Session path
//! (`session::ssh::spawn`), answering its questions the way a person would.

use std::borrow::Cow;
use std::sync::Arc;
use std::time::{Duration, Instant};

use russh::server::{self, Msg, Response, Server as _, Session};
use russh::{Channel, MethodKind, MethodSet};

use komando::session::{Answer, EventProxy, GridSize, HopStep, Prompt, Remembered, SessionEvent, Status, new_term};
use komando::vault::Secret;

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
            Mode::KeysOnly => return Ok(server::Auth::reject()),
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

/// Starts a server on its own runtime and returns its port.
fn start(mode: Mode) -> (u16, tokio::runtime::Runtime) {
    let runtime = tokio::runtime::Builder::new_multi_thread().worker_threads(1).enable_all().build().unwrap();
    let key = russh::keys::PrivateKey::from(russh::keys::ssh_key::private::Ed25519Keypair::from_seed(&[9; 32]));
    let methods: &[MethodKind] = match mode {
        Mode::Interactive => &[MethodKind::KeyboardInteractive],
        Mode::PasswordThenCode => &[MethodKind::Password],
        Mode::KeysOnly => &[MethodKind::PublicKey],
    };
    let config = Arc::new(server::Config {
        keys: vec![key],
        methods: MethodSet::from(methods),
        auth_rejection_time: Duration::ZERO,
        auth_rejection_time_initial: Some(Duration::ZERO),
        ..Default::default()
    });
    let listener = runtime.block_on(tokio::net::TcpListener::bind("127.0.0.1:0")).unwrap();
    let port = listener.local_addr().unwrap().port();
    let mut server = TestServer { mode };
    runtime.spawn(async move {
        let _ = server.run_on_socket(config, &listener).await;
    });
    (port, runtime)
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
}

/// Signs in to the server on `port` as alice, answering its questions in turn with `answers`
/// (the text, and whether "Remember" is ticked).
fn sign_in(name: &str, port: u16, secret: Option<Secret>, answers: &[(&str, bool)]) -> Outcome {
    let dir = std::env::temp_dir().join(format!("komando-sign-in-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
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
    let target = komando::session::ssh::Target { hops: vec![hop], known_hosts: dir.join("known_hosts"), forwards: vec![] };
    let sink = komando::session::ssh::spawn(target, size, term, proxy);
    let mut outcome = Outcome { asked: vec![], signed_in: false, remembered: None, ended: None };
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
