//! Port Forwards through a real SSH connection: an in-process SSH server that opens
//! `direct-tcpip` channels and listens for `tcpip-forward` requests, and the real Session path
//! (`session::ssh::spawn`) with Local, SOCKS and Remote forwards.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use russh::server::{self, Msg, Server as _, Session};
use russh::{Channel, ChannelId, MethodKind, MethodSet};
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

use komando::model::PortForward;
use komando::session::forward::{ForwardState, ForwardStatus};
use komando::session::{EventProxy, GridSize, Prompt, SessionEvent, new_term};
use komando::vault::Secret;

const PASSWORD: &str = "secret";

/// What the test server allows, and where its Remote forwards really listen.
#[derive(Clone, Default)]
struct TestServer {
    /// `direct-tcpip` channels to this port are refused as forbidden.
    forbidden_port: Option<u32>,
    /// Refuse every `tcpip-forward` request.
    refuse_listen: bool,
    /// For each port a Remote forward asked for, the port the server really listens on (the
    /// one asked for may be taken on the test machine).
    listening: Arc<Mutex<HashMap<u32, u16>>>,
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

    async fn channel_open_session(&mut self, _: Channel<Msg>, reply: server::ChannelOpenHandle, _: &mut Session) -> Result<(), Self::Error> {
        reply.accept().await;
        Ok(())
    }

    async fn shell_request(&mut self, channel: ChannelId, session: &mut Session) -> Result<(), Self::Error> {
        session.channel_success(channel)?;
        Ok(())
    }

    async fn channel_open_direct_tcpip(
        &mut self,
        channel: Channel<Msg>,
        host: &str,
        port: u32,
        _: &str,
        _: u32,
        reply: server::ChannelOpenHandle,
        _: &mut Session,
    ) -> Result<(), Self::Error> {
        if Some(port) == self.forbidden_port {
            reply.reject(russh::ChannelOpenFailure::AdministrativelyProhibited).await;
            return Ok(());
        }
        match tokio::net::TcpStream::connect((host, port as u16)).await {
            Ok(mut stream) => {
                reply.accept().await;
                tokio::spawn(async move {
                    let mut channel = channel.into_stream();
                    let _ = tokio::io::copy_bidirectional(&mut channel, &mut stream).await;
                });
            }
            Err(_) => reply.reject(russh::ChannelOpenFailure::ConnectFailed).await,
        }
        Ok(())
    }

    async fn tcpip_forward(&mut self, address: &str, port: &mut u32, session: &mut Session) -> Result<bool, Self::Error> {
        if self.refuse_listen {
            return Ok(false);
        }
        assert_eq!(address, "localhost", "loopback only unless opened to the network");
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        self.listening.lock().unwrap().insert(*port, listener.local_addr().unwrap().port());
        let (handle, asked) = (session.handle(), *port);
        tokio::spawn(async move {
            while let Ok((mut stream, from)) = listener.accept().await {
                let handle = handle.clone();
                tokio::spawn(async move {
                    let channel = handle
                        .channel_open_forwarded_tcpip("localhost", asked, from.ip().to_string(), u32::from(from.port()))
                        .await
                        .unwrap();
                    let mut channel = channel.into_stream();
                    let _ = tokio::io::copy_bidirectional(&mut channel, &mut stream).await;
                });
            }
        });
        Ok(true)
    }
}

/// Starts a server on its own runtime and returns its port.
fn start(server: TestServer) -> (u16, tokio::runtime::Runtime) {
    let runtime = tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build().unwrap();
    let key = russh::keys::PrivateKey::from(russh::keys::ssh_key::private::Ed25519Keypair::from_seed(&[7; 32]));
    let config = Arc::new(server::Config {
        keys: vec![key],
        methods: MethodSet::from(&[MethodKind::Password][..]),
        auth_rejection_time: Duration::ZERO,
        auth_rejection_time_initial: Some(Duration::ZERO),
        ..Default::default()
    });
    let listener = runtime.block_on(tokio::net::TcpListener::bind("127.0.0.1:0")).unwrap();
    let port = listener.local_addr().unwrap().port();
    let mut server = server;
    runtime.spawn(async move {
        let _ = server.run_on_socket(config, &listener).await;
    });
    (port, runtime)
}

/// An echo server on its own runtime, and its port.
fn echo(runtime: &tokio::runtime::Runtime) -> u16 {
    let listener = runtime.block_on(tokio::net::TcpListener::bind("127.0.0.1:0")).unwrap();
    let port = listener.local_addr().unwrap().port();
    runtime.spawn(async move {
        while let Ok((mut stream, _)) = listener.accept().await {
            tokio::spawn(async move {
                let (mut read, mut write) = stream.split();
                let _ = tokio::io::copy(&mut read, &mut write).await;
            });
        }
    });
    port
}

/// A port nothing listens on right now.
fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port()
}

/// A connected Session with `forwards`, and its forwards' reports as they come.
struct Connected {
    sink: komando::session::InputSink,
    events: futures::channel::mpsc::UnboundedReceiver<SessionEvent>,
    seen: Vec<ForwardStatus>,
    dir: std::path::PathBuf,
}

impl Connected {
    fn new(name: &str, port: u16, forwards: Vec<PortForward>) -> Self {
        let dir = std::env::temp_dir().join(format!("komando-forwards-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let size = GridSize { cols: 80, rows: 24, cell_width: 9, cell_height: 18 };
        let (tx, events) = futures::channel::mpsc::unbounded();
        let proxy = EventProxy::new(tx);
        let term = new_term(size, 1000, proxy.clone());
        let hop = komando::session::ssh::Hop {
            name: "server".into(),
            address: "127.0.0.1".into(),
            port,
            user: Some("alice".into()),
            secret: Some(Secret::Password { password: PASSWORD.into() }),
        };
        let target = komando::session::ssh::Target { hops: vec![hop], known_hosts: dir.join("known_hosts"), forwards, ..Default::default() };
        let sink = komando::session::ssh::spawn(target, size, term, proxy);
        Self { sink, events, seen: vec![], dir }
    }

    /// Wait for forward `index` to report a state that passes `check`, answering the Host Key
    /// question on the way.
    fn wait(&mut self, index: usize, check: impl Fn(&ForwardState) -> bool) -> ForwardState {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if let Some(found) = self.seen.iter().rev().find(|s| s.index == index && check(&s.state)) {
                return found.state.clone();
            }
            assert!(Instant::now() < deadline, "timed out; forwards so far: {:?}", self.seen);
            match self.events.try_recv() {
                Ok(SessionEvent::Prompt(Prompt::HostKey { reply, .. })) => {
                    let _ = reply.send(true);
                }
                Ok(SessionEvent::Forward(status)) => self.seen.push(status),
                Ok(SessionEvent::Status(komando::session::Status::Disconnected(why))) => panic!("disconnected: {why}"),
                Ok(_) => {}
                Err(_) => std::thread::sleep(Duration::from_millis(10)),
            }
        }
    }
}

impl Drop for Connected {
    fn drop(&mut self) {
        self.sink.close();
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn is_open(state: &ForwardState) -> bool {
    matches!(state, ForwardState::Open { .. })
}

/// Write `text` to `127.0.0.1:port` and read as much back.
fn round_trip(runtime: &tokio::runtime::Runtime, port: u16, text: &[u8]) -> Vec<u8> {
    runtime.block_on(async {
        let mut stream = tokio::net::TcpStream::connect(("127.0.0.1", port)).await.unwrap();
        stream.write_all(text).await.unwrap();
        let mut back = vec![0; text.len()];
        tokio::time::timeout(Duration::from_secs(5), stream.read_exact(&mut back)).await.expect("an answer").unwrap();
        back
    })
}

#[test]
fn a_local_forward_reaches_an_address_from_the_servers_side() {
    let (port, server) = start(TestServer::default());
    let target = echo(&server);
    let listen = free_port();
    let mut session = Connected::new("local", port, vec![PortForward::local(listen, "127.0.0.1", target)]);
    session.wait(0, is_open);
    assert_eq!(round_trip(&server, listen, b"through ssh"), b"through ssh");
}

#[test]
fn a_socks_forward_connects_where_the_program_asks() {
    let (port, server) = start(TestServer::default());
    let target = echo(&server);
    let listen = free_port();
    let mut session = Connected::new("socks", port, vec![PortForward::socks(listen)]);
    session.wait(0, is_open);
    server.block_on(async {
        let mut stream = tokio::net::TcpStream::connect(("127.0.0.1", listen)).await.unwrap();
        stream.write_all(&[5, 1, 0]).await.unwrap();
        let mut greeting = [0; 2];
        stream.read_exact(&mut greeting).await.unwrap();
        assert_eq!(greeting, [5, 0]);
        let mut request = vec![5, 1, 0, 3, 9];
        request.extend_from_slice(b"127.0.0.1");
        request.extend_from_slice(&target.to_be_bytes());
        stream.write_all(&request).await.unwrap();
        let mut reply = [0; 10];
        stream.read_exact(&mut reply).await.unwrap();
        assert_eq!(reply[..2], [5, 0], "succeeded");
        stream.write_all(b"proxied").await.unwrap();
        let mut back = [0; 7];
        stream.read_exact(&mut back).await.unwrap();
        assert_eq!(&back, b"proxied");
    });
}

#[test]
fn a_remote_forward_reaches_an_address_from_this_computers_side() {
    let server = TestServer::default();
    let listening = server.listening.clone();
    let (port, runtime) = start(server);
    let target = echo(&runtime);
    let mut session = Connected::new("remote", port, vec![PortForward::remote(8022, "127.0.0.1", target)]);
    session.wait(0, is_open);
    let on_server = *listening.lock().unwrap().get(&8022).expect("the server listens for port 8022");
    assert_eq!(round_trip(&runtime, on_server, b"from the server"), b"from the server");
    session.wait(0, |state| matches!(state, ForwardState::Open { connections: 1, .. }));
}

#[test]
fn a_forbidden_channel_is_reported_and_the_forward_keeps_listening() {
    let (port, server) = start(TestServer { forbidden_port: Some(5432), ..Default::default() });
    let listen = free_port();
    let mut session = Connected::new("forbidden", port, vec![PortForward::local(listen, "db.internal", 5432)]);
    session.wait(0, is_open);
    server.block_on(async {
        let mut stream = tokio::net::TcpStream::connect(("127.0.0.1", listen)).await.unwrap();
        // The forward closes the connection it couldn't carry.
        let mut byte = [0; 1];
        let read = tokio::time::timeout(Duration::from_secs(5), stream.read(&mut byte)).await.unwrap();
        assert!(matches!(read, Ok(0) | Err(_)));
    });
    let state = session.wait(0, |state| matches!(state, ForwardState::Open { problem: Some(_), .. }));
    assert_eq!(
        state,
        ForwardState::Open {
            connections: 0,
            problem: Some("The server doesn't allow port forwarding (its AllowTcpForwarding setting).".into())
        }
    );
}

#[test]
fn a_remote_forward_the_server_refuses_fails() {
    let (port, _server) = start(TestServer { refuse_listen: true, ..Default::default() });
    let mut session = Connected::new("refused", port, vec![PortForward::remote(8080, "localhost", 3000)]);
    let state = session.wait(0, |state| matches!(state, ForwardState::Failed(_)));
    assert_eq!(state, ForwardState::Failed(komando::session::forward::remote_refused(8080)));
}
