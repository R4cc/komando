//! Port Forwards over an SSH Session's connection (see `model::PortForward`).
//!
//! They start once every hop has signed in and stop with the Session: `start` returns a
//! `JoinSet` whose tasks own every listener and connection, so dropping it closes them all.
//! Local and SOCKS forwards listen here and open a `direct-tcpip` channel per connection;
//! a Remote forward asks the server to listen, and the server opens a `forwarded-tcpip`
//! channel per connection, which `ssh::Client` hands to `RemoteForwards::accept`.
//!
//! The SSH side is reached through `Opener`, so the listening and SOCKS halves are tested
//! here against plain TCP.

use std::collections::HashMap;
use std::future::Future;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::pin::Pin;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use tokio::io::{AsyncRead, AsyncReadExt as _, AsyncWrite, AsyncWriteExt as _};
use tokio::net::{TcpListener, TcpStream};
use tokio::task::JoinSet;

use crate::model::{ForwardKind, PortForward};

/// Where one Port Forward is, as the session strip shows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ForwardState {
    /// Binding its port, or waiting for the server to.
    Starting,
    /// Listening. `problem` is why the latest connection through it failed, until one works.
    Open { connections: usize, problem: Option<String> },
    /// It could not listen, and stays off until the Session reconnects.
    Failed(String),
}

/// A Port Forward's state, sent to the Session's view whenever it changes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ForwardStatus {
    /// Its place in the Session's list: the Project's forwards first, then the Host's.
    pub index: usize,
    pub forward: PortForward,
    pub state: ForwardState,
}

/// Where a forward's state changes go.
pub type Report = Arc<dyn Fn(ForwardStatus) + Send + Sync>;

/// A two-way byte stream: an SSH channel, or a TCP connection in tests.
pub trait Tunnel: AsyncRead + AsyncWrite + Unpin + Send {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send> Tunnel for T {}

pub type BoxTunnel = Box<dyn Tunnel>;
type BoxFuture<T> = Pin<Box<dyn Future<Output = T> + Send>>;

/// Opens a channel to `host:port` from the server's side, for a connection that came from
/// `from` here. Its error is the sentence to show.
pub type Opener = Arc<dyn Fn(String, u16, SocketAddr) -> BoxFuture<Result<BoxTunnel, OpenError>> + Send + Sync>;

/// Why the server didn't open a channel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OpenError {
    /// The server forbids forwarding (sshd's AllowTcpForwarding, or a `permitopen` limit).
    Prohibited,
    /// The server tried and couldn't reach the address.
    Unreachable,
    /// Anything else, already worded.
    Other(String),
}

impl OpenError {
    fn sentence(&self, target: &str) -> String {
        match self {
            OpenError::Prohibited => "The server doesn't allow port forwarding (its AllowTcpForwarding setting).".into(),
            OpenError::Unreachable => format!("The server couldn't connect to {target}."),
            OpenError::Other(why) => format!("Couldn't open a channel to {target}: {why}"),
        }
    }

    /// SOCKS5's reply code for it.
    fn socks_reply(&self) -> u8 {
        match self {
            OpenError::Prohibited => 0x02,
            OpenError::Unreachable => 0x05,
            OpenError::Other(_) => 0x01,
        }
    }
}

/// One forward's state, reported whenever it changes.
struct Tracker {
    index: usize,
    forward: PortForward,
    report: Report,
    connections: AtomicUsize,
    problem: Mutex<Option<String>>,
}

impl Tracker {
    fn new(index: usize, forward: PortForward, report: Report) -> Arc<Self> {
        let tracker = Arc::new(Self { index, forward, report, connections: AtomicUsize::new(0), problem: Mutex::new(None) });
        tracker.send(ForwardState::Starting);
        tracker
    }

    fn send(&self, state: ForwardState) {
        (self.report)(ForwardStatus { index: self.index, forward: self.forward.clone(), state });
    }

    fn send_open(&self) {
        let problem = self.problem.lock().unwrap().clone();
        self.send(ForwardState::Open { connections: self.connections.load(Ordering::Acquire), problem });
    }

    fn failed(&self, why: String) {
        log::warn!("port forward {}: {why}", self.forward.label());
        self.send(ForwardState::Failed(why));
    }

    /// A connection went through: it clears the last problem.
    fn connected(&self) {
        self.connections.fetch_add(1, Ordering::AcqRel);
        *self.problem.lock().unwrap() = None;
        self.send_open();
    }

    fn disconnected(&self) {
        self.connections.fetch_sub(1, Ordering::AcqRel);
        self.send_open();
    }

    fn problem(&self, why: String) {
        log::info!("port forward {}: {why}", self.forward.label());
        *self.problem.lock().unwrap() = Some(why);
        self.send_open();
    }

    /// Carry bytes both ways until either side closes, counted as an open connection.
    async fn carry(&self, mut near: impl AsyncRead + AsyncWrite + Unpin, mut far: BoxTunnel) {
        self.connected();
        let _ = tokio::io::copy_bidirectional(&mut near, &mut far).await;
        let _ = far.shutdown().await;
        self.disconnected();
    }
}

/// Remote forwards the server listens for, by the port it listens on, so its
/// `forwarded-tcpip` channels reach the right address here. Shared with `ssh::Client`.
#[derive(Clone, Default)]
pub struct RemoteForwards(Arc<Mutex<HashMap<u32, Arc<Tracker>>>>);

impl RemoteForwards {
    /// A connection the server took on port `port`: carry it to the forward's address here.
    /// False if no forward listens there (the caller rejects the channel).
    pub fn accept(&self, port: u32, channel: BoxTunnel) -> bool {
        let Some(tracker) = self.0.lock().unwrap().get(&port).cloned() else { return false };
        tokio::spawn(async move {
            let target = tracker.forward.target();
            match TcpStream::connect((tracker.forward.to_host.as_str(), tracker.forward.to_port)).await {
                Ok(stream) => {
                    let _ = stream.set_nodelay(true);
                    tracker.carry(stream, channel).await;
                }
                Err(err) => tracker.problem(format!("Couldn't connect to {target} from this computer: {}.", describe_io(&err))),
            }
        });
        true
    }
}

/// Asks the server to listen for a Remote forward. `Ok` once it does.
pub type Listen = Arc<dyn Fn(String, u32) -> BoxFuture<Result<(), String>> + Send + Sync>;

/// Start every forward. Each reports `Starting`, then `Open` or `Failed`; the returned tasks
/// keep them running until dropped.
pub fn start(forwards: &[PortForward], open: Opener, listen: Listen, remote: &RemoteForwards, report: Report) -> JoinSet<()> {
    let mut tasks = JoinSet::new();
    for (index, forward) in forwards.iter().enumerate() {
        let tracker = Tracker::new(index, forward.clone(), report.clone());
        match forward.kind {
            ForwardKind::Local | ForwardKind::Socks => {
                tasks.spawn(serve(tracker, open.clone()));
            }
            ForwardKind::Remote => {
                let (listen, remote) = (listen.clone(), remote.clone());
                tasks.spawn(async move {
                    // OpenSSH's "localhost" listens on loopback only; "" on every interface, if
                    // the server's GatewayPorts lets clients choose.
                    let address = if tracker.forward.open_to_network { "" } else { "localhost" };
                    let port = u32::from(tracker.forward.port);
                    remote.0.lock().unwrap().insert(port, tracker.clone());
                    match listen(address.to_string(), port).await {
                        Ok(()) => tracker.send_open(),
                        Err(why) => {
                            remote.0.lock().unwrap().remove(&port);
                            tracker.failed(why);
                        }
                    }
                });
            }
        }
    }
    tasks
}

/// The sentence for a Remote forward the server refused.
pub fn remote_refused(port: u16) -> String {
    format!(
        "The server didn't listen on port {port}. The port may be in use there, or below 1024, or its sshd may not \
         allow forwarding (AllowTcpForwarding)."
    )
}

/// Listen for a Local or SOCKS forward and carry each connection through the server.
async fn serve(tracker: Arc<Tracker>, open: Opener) {
    let (main, extra) = match bind(tracker.forward.port, tracker.forward.open_to_network).await {
        Ok(listeners) => listeners,
        Err(why) => return tracker.failed(why),
    };
    tracker.send_open();
    let mut connections = JoinSet::new();
    loop {
        let accepted = tokio::select! {
            accepted = main.accept() => accepted,
            accepted = async { extra.as_ref().expect("a second listener").accept().await }, if extra.is_some() => accepted,
            // Finished connections leave the set, so it doesn't grow for as long as the Session runs.
            Some(_) = connections.join_next(), if !connections.is_empty() => continue,
        };
        let (stream, from) = match accepted {
            Ok(accepted) => accepted,
            Err(err) => {
                // Out of file handles, or a connection reset before it was taken: try again shortly.
                log::warn!("port forward {}: accept failed: {err}", tracker.forward.label());
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                continue;
            }
        };
        let _ = stream.set_nodelay(true);
        let (tracker, open) = (tracker.clone(), open.clone());
        connections.spawn(async move {
            match tracker.forward.kind {
                ForwardKind::Socks => socks(&tracker, stream, from, &open).await,
                _ => {
                    let forward = &tracker.forward;
                    match open(forward.to_host.clone(), forward.to_port, from).await {
                        Ok(channel) => tracker.carry(stream, channel).await,
                        Err(err) => tracker.problem(err.sentence(&forward.target())),
                    }
                }
            }
        });
    }
}

/// The listeners for `port` here: IPv4 must work; IPv6 is added when it does too, so programs
/// that try `localhost` as `::1` first still connect.
async fn bind(port: u16, open_to_network: bool) -> Result<(TcpListener, Option<TcpListener>), String> {
    let (v4, v6): (IpAddr, IpAddr) = if open_to_network {
        (Ipv4Addr::UNSPECIFIED.into(), Ipv6Addr::UNSPECIFIED.into())
    } else {
        (Ipv4Addr::LOCALHOST.into(), Ipv6Addr::LOCALHOST.into())
    };
    let main = TcpListener::bind((v4, port)).await.map_err(|err| listen_error(port, &err))?;
    // On a system whose IPv6 sockets also take IPv4, `::` clashes with `0.0.0.0`, which
    // already covers both; there and without IPv6 this one is left out.
    let extra = TcpListener::bind((v6, port)).await.ok();
    Ok((main, extra))
}

fn listen_error(port: u16, err: &std::io::Error) -> String {
    match err.kind() {
        std::io::ErrorKind::AddrInUse => {
            format!("Port {port} on this computer is already in use, by another program or another session.")
        }
        std::io::ErrorKind::PermissionDenied => format!(
            "This computer doesn't let Komando listen on port {port}. Ports below 1024 need administrator rights, \
             and Windows reserves some others. Pick another port."
        ),
        _ => format!("Couldn't listen on port {port} on this computer: {}.", describe_io(err)),
    }
}

fn describe_io(err: &std::io::Error) -> String {
    match err.kind() {
        std::io::ErrorKind::ConnectionRefused => "nothing is listening there".into(),
        std::io::ErrorKind::TimedOut => "it didn't answer".into(),
        _ => err.to_string(),
    }
}

// ---- SOCKS5 (RFC 1928), CONNECT without authentication -------------------------------------

/// Where a SOCKS client asked to connect.
#[derive(Debug, PartialEq, Eq)]
struct SocksRequest {
    host: String,
    port: u16,
}

/// Why a SOCKS client's request is refused, with the reply to send (if any).
#[derive(Debug, PartialEq, Eq)]
enum SocksRefusal {
    /// It spoke SOCKS4, or not SOCKS at all: close without a reply.
    NotSocks5 { version: u8 },
    /// It offers only authentication methods Komando doesn't take.
    NoMethod,
    /// A command other than CONNECT (BIND, UDP ASSOCIATE): reply `0x07`.
    Command(u8),
    /// An address type that doesn't exist: reply `0x08`.
    AddressType(u8),
}

/// Read a SOCKS5 greeting and request, answering the greeting. `Err(None)` when the client
/// went away; `Err(Some(_))` for a request to refuse.
async fn socks_handshake(stream: &mut (impl AsyncRead + AsyncWrite + Unpin)) -> Result<SocksRequest, Option<SocksRefusal>> {
    let gone = |_| None;
    let [version, methods] = read_array::<2>(stream).await.map_err(gone)?;
    if version != 5 {
        return Err(Some(SocksRefusal::NotSocks5 { version }));
    }
    let mut offered = vec![0; methods as usize];
    stream.read_exact(&mut offered).await.map_err(gone)?;
    if !offered.contains(&0) {
        let _ = stream.write_all(&[5, 0xff]).await;
        return Err(Some(SocksRefusal::NoMethod));
    }
    stream.write_all(&[5, 0]).await.map_err(gone)?;
    let [_, command, _, kind] = read_array::<4>(stream).await.map_err(gone)?;
    let host = match kind {
        1 => Ipv4Addr::from(read_array::<4>(stream).await.map_err(gone)?).to_string(),
        4 => Ipv6Addr::from(read_array::<16>(stream).await.map_err(gone)?).to_string(),
        3 => {
            let [len] = read_array::<1>(stream).await.map_err(gone)?;
            let mut name = vec![0; len as usize];
            stream.read_exact(&mut name).await.map_err(gone)?;
            String::from_utf8_lossy(&name).into_owned()
        }
        other => return Err(Some(SocksRefusal::AddressType(other))),
    };
    let port = u16::from_be_bytes(read_array::<2>(stream).await.map_err(gone)?);
    if command != 1 {
        return Err(Some(SocksRefusal::Command(command)));
    }
    Ok(SocksRequest { host, port })
}

async fn read_array<const N: usize>(stream: &mut (impl AsyncRead + Unpin)) -> std::io::Result<[u8; N]> {
    let mut bytes = [0; N];
    stream.read_exact(&mut bytes).await?;
    Ok(bytes)
}

/// A SOCKS5 reply with `code` and an empty IPv4 address (clients don't use it).
fn socks_reply(code: u8) -> [u8; 10] {
    [5, code, 0, 1, 0, 0, 0, 0, 0, 0]
}

/// One SOCKS client: read where it wants to go, open a channel there, then carry it.
async fn socks(tracker: &Tracker, mut stream: TcpStream, from: SocketAddr, open: &Opener) {
    let request = match socks_handshake(&mut stream).await {
        Ok(request) => request,
        Err(None) => return,
        Err(Some(refusal)) => {
            let code = match refusal {
                SocksRefusal::NotSocks5 { version } => {
                    let spoke = if version == 4 { "SOCKS4" } else { "something other than SOCKS" };
                    tracker.problem(format!("A program spoke {spoke} to port {}. Set it to use SOCKS5.", tracker.forward.port));
                    None
                }
                SocksRefusal::NoMethod => {
                    tracker.problem("A program asked for a SOCKS password. Komando's SOCKS proxy takes none: turn it off there.".into());
                    None
                }
                SocksRefusal::Command(_) => Some(0x07),
                SocksRefusal::AddressType(_) => Some(0x08),
            };
            if let Some(code) = code {
                let _ = stream.write_all(&socks_reply(code)).await;
            }
            return;
        }
    };
    let target = if request.host.contains(':') { format!("[{}]:{}", request.host, request.port) } else { format!("{}:{}", request.host, request.port) };
    match open(request.host, request.port, from).await {
        Ok(channel) => {
            if stream.write_all(&socks_reply(0)).await.is_ok() {
                tracker.carry(stream, channel).await;
            }
        }
        Err(err) => {
            let _ = stream.write_all(&socks_reply(err.socks_reply())).await;
            tracker.problem(err.sentence(&target));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    /// An Opener that connects straight from here, as a server next to this computer would.
    fn direct() -> Opener {
        Arc::new(|host: String, port: u16, _from| {
            Box::pin(async move {
                match TcpStream::connect((host.as_str(), port)).await {
                    Ok(stream) => Ok(Box::new(stream) as BoxTunnel),
                    Err(_) => Err(OpenError::Unreachable),
                }
            })
        })
    }

    fn no_listen() -> Listen {
        Arc::new(|_, _| Box::pin(async { Err("no server".to_string()) }))
    }

    /// Collects every report, for the tests to wait on.
    fn reports() -> (Report, Arc<Mutex<Vec<ForwardStatus>>>) {
        let seen = Arc::new(Mutex::new(vec![]));
        let sink = seen.clone();
        (Arc::new(move |status| sink.lock().unwrap().push(status)), seen)
    }

    async fn until(seen: &Mutex<Vec<ForwardStatus>>, check: impl Fn(&ForwardStatus) -> bool) -> ForwardStatus {
        for _ in 0..200 {
            if let Some(found) = seen.lock().unwrap().iter().rev().find(|s| check(s)) {
                return found.clone();
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("no such report in {:?}", seen.lock().unwrap());
    }

    /// An echo server on loopback, and its port.
    async fn echo() -> u16 {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            loop {
                let (mut stream, _) = listener.accept().await.unwrap();
                tokio::spawn(async move {
                    let (mut read, mut write) = stream.split();
                    let _ = tokio::io::copy(&mut read, &mut write).await;
                });
            }
        });
        port
    }

    /// A port nothing listens on right now.
    async fn free_port() -> u16 {
        TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap().local_addr().unwrap().port()
    }

    async fn round_trip(stream: &mut TcpStream, text: &[u8]) -> Vec<u8> {
        stream.write_all(text).await.unwrap();
        let mut back = vec![0; text.len()];
        tokio::time::timeout(Duration::from_secs(5), stream.read_exact(&mut back)).await.unwrap().unwrap();
        back
    }

    #[tokio::test]
    async fn a_local_forward_carries_a_connection_and_counts_it() {
        let target = echo().await;
        let port = free_port().await;
        let (report, seen) = reports();
        let _tasks = start(&[PortForward::local(port, "127.0.0.1", target)], direct(), no_listen(), &RemoteForwards::default(), report);
        until(&seen, |s| matches!(s.state, ForwardState::Open { .. })).await;
        let mut client = TcpStream::connect((Ipv4Addr::LOCALHOST, port)).await.unwrap();
        assert_eq!(round_trip(&mut client, b"hello").await, b"hello");
        until(&seen, |s| s.state == ForwardState::Open { connections: 1, problem: None }).await;
        drop(client);
        until(&seen, |s| s.state == ForwardState::Open { connections: 0, problem: None }).await;
    }

    #[tokio::test]
    async fn a_taken_port_fails_the_forward_and_says_why() {
        let taken = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
        let port = taken.local_addr().unwrap().port();
        let (report, seen) = reports();
        let _tasks = start(&[PortForward::local(port, "127.0.0.1", 1)], direct(), no_listen(), &RemoteForwards::default(), report);
        let failed = until(&seen, |s| matches!(s.state, ForwardState::Failed(_))).await;
        assert_eq!(failed.state, ForwardState::Failed(format!("Port {port} on this computer is already in use, by another program or another session.")));
    }

    #[tokio::test]
    async fn an_unreachable_target_is_a_problem_until_a_connection_works() {
        let port = free_port().await;
        let closed = free_port().await;
        let (report, seen) = reports();
        let _tasks = start(&[PortForward::local(port, "127.0.0.1", closed)], direct(), no_listen(), &RemoteForwards::default(), report);
        until(&seen, |s| matches!(s.state, ForwardState::Open { .. })).await;
        let _client = TcpStream::connect((Ipv4Addr::LOCALHOST, port)).await.unwrap();
        let status = until(&seen, |s| matches!(&s.state, ForwardState::Open { problem: Some(_), .. })).await;
        assert_eq!(
            status.state,
            ForwardState::Open { connections: 0, problem: Some(format!("The server couldn't connect to 127.0.0.1:{closed}.")) }
        );
    }

    /// Offer SOCKS5 without authentication and read the proxy's choice.
    async fn greet(stream: &mut TcpStream) -> [u8; 2] {
        stream.write_all(&[5, 1, 0]).await.unwrap();
        let mut choice = [0; 2];
        tokio::time::timeout(Duration::from_secs(5), stream.read_exact(&mut choice)).await.unwrap().unwrap();
        choice
    }

    #[tokio::test]
    async fn socks_connects_where_the_program_asks() {
        let target = echo().await;
        let port = free_port().await;
        let (report, seen) = reports();
        let _tasks = start(&[PortForward::socks(port)], direct(), no_listen(), &RemoteForwards::default(), report);
        until(&seen, |s| matches!(s.state, ForwardState::Open { .. })).await;

        // By name, as browsers set to resolve through the proxy ask.
        let mut client = TcpStream::connect((Ipv4Addr::LOCALHOST, port)).await.unwrap();
        assert_eq!(greet(&mut client).await, [5, 0]);
        let mut request = vec![5, 1, 0, 3, 9];
        request.extend_from_slice(b"127.0.0.1");
        request.extend_from_slice(&target.to_be_bytes());
        client.write_all(&request).await.unwrap();
        let mut reply = [0; 10];
        client.read_exact(&mut reply).await.unwrap();
        assert_eq!(reply, socks_reply(0));
        assert_eq!(round_trip(&mut client, b"through socks").await, b"through socks");

        // By IPv4 address, to a closed port: refused with "connection refused".
        let closed = free_port().await;
        let mut client = TcpStream::connect((Ipv4Addr::LOCALHOST, port)).await.unwrap();
        assert_eq!(greet(&mut client).await, [5, 0]);
        let mut request = vec![5, 1, 0, 1, 127, 0, 0, 1];
        request.extend_from_slice(&closed.to_be_bytes());
        client.write_all(&request).await.unwrap();
        client.read_exact(&mut reply).await.unwrap();
        assert_eq!(reply, socks_reply(5));
    }

    #[tokio::test]
    async fn socks_refuses_what_it_does_not_speak() {
        async fn handshake(bytes: &[u8]) -> (Result<SocksRequest, Option<SocksRefusal>>, Vec<u8>) {
            let (mut near, mut far) = tokio::io::duplex(256);
            far.write_all(bytes).await.unwrap();
            // The client has said all it will: reading past it is the end, not a wait.
            far.shutdown().await.unwrap();
            let result = socks_handshake(&mut near).await;
            drop(near);
            let mut written = vec![];
            far.read_to_end(&mut written).await.unwrap();
            (result, written)
        }
        assert_eq!(handshake(&[4, 1, 0, 80, 1, 2, 3, 4, 0]).await.0, Err(Some(SocksRefusal::NotSocks5 { version: 4 })));
        assert_eq!(handshake(&[5, 1, 2]).await, (Err(Some(SocksRefusal::NoMethod)), vec![5, 0xff]));
        // BIND to [::1]:80.
        let mut bind = vec![5, 1, 0, 5, 2, 0, 4];
        bind.extend_from_slice(&Ipv6Addr::LOCALHOST.octets());
        bind.extend_from_slice(&80u16.to_be_bytes());
        assert_eq!(handshake(&bind).await.0, Err(Some(SocksRefusal::Command(2))));
        assert_eq!(handshake(&[5, 1, 0, 5, 1, 0, 9]).await.0, Err(Some(SocksRefusal::AddressType(9))));
        // A client that leaves halfway is no refusal.
        assert_eq!(handshake(&[5, 1]).await.0, Err(None));
        let mut ipv6 = vec![5, 1, 0, 5, 1, 0, 4];
        ipv6.extend_from_slice(&Ipv6Addr::LOCALHOST.octets());
        ipv6.extend_from_slice(&443u16.to_be_bytes());
        assert_eq!(handshake(&ipv6).await.0, Ok(SocksRequest { host: "::1".into(), port: 443 }));
    }

    #[tokio::test]
    async fn a_remote_forward_carries_the_servers_connections_here() {
        let target = echo().await;
        let (report, seen) = reports();
        let remote = RemoteForwards::default();
        let asked = Arc::new(Mutex::new(vec![]));
        let listen: Listen = {
            let asked = asked.clone();
            Arc::new(move |address, port| {
                asked.lock().unwrap().push((address, port));
                Box::pin(async { Ok(()) })
            })
        };
        let _tasks = start(&[PortForward::remote(8080, "127.0.0.1", target)], direct(), listen, &remote, report);
        until(&seen, |s| matches!(s.state, ForwardState::Open { .. })).await;
        assert_eq!(*asked.lock().unwrap(), [("localhost".to_string(), 8080)]);

        // The server's channel, as the near end of an in-memory pipe.
        let (channel, mut server) = tokio::io::duplex(1024);
        assert!(remote.accept(8080, Box::new(channel)));
        server.write_all(b"from the server").await.unwrap();
        let mut back = [0; 15];
        tokio::time::timeout(Duration::from_secs(5), server.read_exact(&mut back)).await.unwrap().unwrap();
        assert_eq!(&back, b"from the server");
        let (other, _) = tokio::io::duplex(16);
        assert!(!remote.accept(9090, Box::new(other)), "nothing listens on 9090");
    }

    #[tokio::test]
    async fn a_refused_remote_forward_fails_and_lets_go_of_its_port() {
        let (report, seen) = reports();
        let remote = RemoteForwards::default();
        let listen: Listen = Arc::new(|_, _| Box::pin(async { Err(remote_refused(80)) }));
        let _tasks = start(&[PortForward { open_to_network: true, ..PortForward::remote(80, "localhost", 3000) }], direct(), listen, &remote, report);
        let failed = until(&seen, |s| matches!(s.state, ForwardState::Failed(_))).await;
        assert_eq!(failed.state, ForwardState::Failed(remote_refused(80)));
        let (channel, _) = tokio::io::duplex(16);
        assert!(!remote.accept(80, Box::new(channel)));
    }
}
