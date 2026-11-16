//! Remote Desktops: the backend half of a desktop Session. RDP runs on IronRDP, VNC on vnc-rs;
//! Komando draws the picture itself (see docs/adr/0012-remote-desktops-are-sessions-of-a-host.md).
//!
//! Speed rule, as for terminals: the protocol runs on its own thread, never on the
//! UI thread. It draws into a shared [`Picture`], and the view is told "the picture changed" at
//! most once until it has drawn, so a busy desktop costs one repaint per frame, not one per
//! rectangle the server sends.

pub mod capture;
pub mod keys;
mod rdp;
pub mod trust;
mod vnc;

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Context as _, Result, anyhow};
use futures::channel::{mpsc as events, oneshot};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::sync::mpsc;
use zeroize::Zeroizing;

pub use crate::model::{Protocol, Quality};
use crate::session::InputSink;
pub use crate::session::Status;
pub use keys::Key;

/// A connection a desktop engine reads and writes: a TCP socket, or a tunnel through SSH.
pub trait AsyncStream: AsyncRead + AsyncWrite + Unpin + Send + Sync {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send + Sync> AsyncStream for T {}
pub type Stream = Box<dyn AsyncStream>;

/// How long reaching the server may take before Komando gives up.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);

/// The remote screen, in BGRA (the order gpui draws), always opaque.
#[derive(Default)]
pub struct Picture {
    pub width: u32,
    pub height: u32,
    pub bgra: Vec<u8>,
    /// Counts changes, so the view can tell whether it has drawn the latest.
    pub version: u64,
}

/// Refuse a server's desktop size before anything that size is allocated: up to two 8K
/// screens' worth of pixels.
pub(crate) fn check_size(width: u32, height: u32) -> Result<()> {
    if width == 0 || height == 0 || u64::from(width) * u64::from(height) > 64 * 1024 * 1024 {
        return Err(anyhow!("The server requested an unsupported desktop size ({width} × {height})."));
    }
    Ok(())
}

impl Picture {
    /// A new size, black, as servers expect a fresh desktop to start.
    fn resize(&mut self, width: u32, height: u32) -> Result<()> {
        check_size(width, height)?;
        if (width, height) != (self.width, self.height) {
            self.width = width;
            self.height = height;
            self.bgra = [0, 0, 0, 0xff].repeat(width as usize * height as usize);
        }
        self.version += 1;
        Ok(())
    }

    /// Copy a rectangle of 4-byte pixels in. Rows of `src` are `stride` bytes apart. Parts
    /// outside the picture are left out.
    fn put(&mut self, x: u32, y: u32, width: u32, height: u32, src: &[u8], stride: usize) {
        if x >= self.width || y >= self.height {
            return;
        }
        let width = width.min(self.width.saturating_sub(x));
        let height = height.min(self.height.saturating_sub(y));
        let row_bytes = width as usize * 4;
        for row in 0..height as usize {
            let Some(from) = src.get(row * stride..row * stride + row_bytes) else { break };
            let start = ((y as usize + row) * self.width as usize + x as usize) * 4;
            let to = &mut self.bgra[start..start + row_bytes];
            to.copy_from_slice(from);
            // Servers leave the fourth byte as padding (often 0): the picture is opaque.
            for pixel in to.chunks_exact_mut(4) {
                pixel[3] = 0xff;
            }
        }
        self.version += 1;
    }

    /// VNC's CopyRect: move a rectangle already on screen. Source and destination may overlap.
    fn copy(&mut self, to: (u32, u32), from: (u32, u32), width: u32, height: u32) {
        let fits = |(x, y): (u32, u32)| x + width <= self.width && y + height <= self.height;
        if !fits(to) || !fits(from) {
            return;
        }
        let stride = self.width as usize * 4;
        let rect: Vec<u8> = (0..height as usize)
            .flat_map(|row| {
                let start = (from.1 as usize + row) * stride + from.0 as usize * 4;
                self.bgra[start..start + width as usize * 4].to_vec()
            })
            .collect();
        self.put(to.0, to.1, width, height, &rect, width as usize * 4);
    }
}

pub type SharedPicture = Arc<Mutex<Picture>>;

/// How the desktop is reached.
#[derive(Clone)]
pub enum Route {
    /// Straight to the Host's address.
    Direct { address: String, port: u16 },
    /// Through a tunnel on the Host's SSH Session, to the port on the Host itself.
    Ssh { sink: InputSink, port: u16 },
}

impl Route {
    async fn open(&self) -> Result<Stream> {
        match self {
            Route::Direct { address, port } => {
                let tcp = tokio::time::timeout(CONNECT_TIMEOUT, tokio::net::TcpStream::connect((address.as_str(), *port)))
                    .await
                    .map_err(|_| anyhow!("{address}:{port} didn't answer within {} seconds.", CONNECT_TIMEOUT.as_secs()))?
                    .with_context(|| format!("Couldn't reach {address}:{port}"))?;
                tcp.set_nodelay(true)?;
                Ok(Box::new(tcp))
            }
            Route::Ssh { sink, port } => {
                let ended = || anyhow!("The host's SSH session has ended: connect it again, then reconnect.");
                let answer = sink.open_tunnel(*port).ok_or_else(ended)?;
                tokio::time::timeout(CONNECT_TIMEOUT, answer).await.map_err(|_| ended())?.map_err(|_| ended())?
            }
        }
    }

    /// The local address the engine reports to the server.
    fn local_address(&self) -> std::net::SocketAddr {
        std::net::SocketAddr::from(([127, 0, 0, 1], 0))
    }
}

/// Everything a desktop connection needs, resolved on the UI thread before it starts.
pub struct Target {
    pub protocol: Protocol,
    /// The Host's address: where a direct connection goes, and the name an RDP server's
    /// certificate is filed under.
    pub address: String,
    pub port: u16,
    pub route: Route,
    /// RDP: the account. None: asked.
    pub user: Option<String>,
    pub domain: Option<String>,
    /// From the Vault. None: asked.
    pub password: Option<Zeroizing<String>>,
    /// The size to ask an RDP server for, in device pixels, and the display's scale (100 = 100%).
    pub size: (u32, u32),
    pub scale_percent: u32,
    /// VNC: leave other viewers connected.
    pub shared: bool,
    /// Picture detail traded for speed: colors (VNC), visual effects (RDP).
    pub quality: Quality,
    /// Where trusted RDP certificates are kept.
    pub certificates: PathBuf,
}

/// What the view sends the engine.
#[derive(Debug, Clone)]
pub enum Input {
    Key {
        key: Key,
        down: bool,
    },
    /// The pointer moved to a point on the remote screen.
    Pointer {
        x: u16,
        y: u16,
    },
    Button {
        button: Button,
        down: bool,
    },
    /// Wheel turned, in 120ths of a notch (Windows' unit). Negative is down or left.
    Wheel {
        vertical: bool,
        amount: i16,
    },
    /// The pane changed size (device pixels): an RDP server set to follow it is asked for a
    /// desktop that size.
    Resize {
        width: u32,
        height: u32,
        scale_percent: u32,
    },
    CtrlAltDel,
    /// Let go of every key and button held: the view lost the keyboard.
    ReleaseAll,
    Close,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Button {
    Left,
    Middle,
    Right,
}

/// What the engine tells the view.
pub enum DesktopEvent {
    Status(Status),
    /// The picture changed. Sent once until the view has drawn (see `Connection::drawn`).
    Picture,
    Prompt(DesktopPrompt),
    /// Signed in with a password typed in and "Remember" ticked: save it for the Host.
    Remember {
        user: Option<String>,
        password: Zeroizing<String>,
    },
}

pub enum DesktopPrompt {
    /// A password, and for RDP the account when none is set. `None` cancels.
    SignIn { user: Option<String>, needs_user: bool, error: Option<String>, reply: oneshot::Sender<Option<SignIn>> },
    /// First connection to this RDP server, or its certificate changed (`previous` is the one
    /// trusted before): trust this one?
    Certificate { fingerprint: String, previous: Option<String>, reply: oneshot::Sender<bool> },
}

pub struct SignIn {
    pub user: Option<String>,
    pub password: Zeroizing<String>,
    pub remember: bool,
}

/// The engine's side of a connection.
struct Link {
    events: events::UnboundedSender<DesktopEvent>,
    input: mpsc::UnboundedReceiver<Input>,
    picture: SharedPicture,
    /// A `Picture` event is on its way and the view hasn't drawn since.
    pending: Arc<AtomicBool>,
}

impl Link {
    fn send(&self, event: DesktopEvent) {
        let _ = self.events.unbounded_send(event);
    }

    fn status(&self, status: Status) {
        self.send(DesktopEvent::Status(status));
    }

    /// Change the picture, and tell the view unless it was already told.
    fn draw<T>(&self, change: impl FnOnce(&mut Picture) -> T) -> T {
        let result = change(&mut self.picture.lock().expect("picture"));
        if !self.pending.swap(true, Ordering::AcqRel) {
            self.send(DesktopEvent::Picture);
        }
        result
    }

    async fn ask_sign_in(&self, user: Option<String>, needs_user: bool, error: Option<String>) -> Option<SignIn> {
        let (reply, answer) = oneshot::channel();
        self.send(DesktopEvent::Prompt(DesktopPrompt::SignIn { user, needs_user, error, reply }));
        answer.await.ok().flatten()
    }

    async fn ask_certificate(&self, fingerprint: String, previous: Option<String>) -> bool {
        let (reply, answer) = oneshot::channel();
        self.send(DesktopEvent::Prompt(DesktopPrompt::Certificate { fingerprint, previous, reply }));
        answer.await.unwrap_or(false)
    }
}

/// The view's side of a connection.
pub struct Connection {
    pub input: mpsc::UnboundedSender<Input>,
    pub events: events::UnboundedReceiver<DesktopEvent>,
    pub picture: SharedPicture,
    pending: Arc<AtomicBool>,
    /// Dropping the connection cancels even a stalled handshake or an unanswered prompt.
    _cancel: oneshot::Sender<()>,
}

impl Connection {
    /// The view is about to draw the picture: the next change must tell it again. Called
    /// before reading the picture, so a change made while it reads is not missed.
    pub fn drawn(&self) {
        self.pending.store(false, Ordering::Release);
    }

    pub fn send(&self, input: Input) {
        let _ = self.input.send(input);
    }
}

/// Start connecting. The connection ends when the view sends `Input::Close` or drops its
/// `Connection`.
pub fn spawn(target: Target) -> Connection {
    let (events_tx, events_rx) = events::unbounded();
    let (input_tx, input_rx) = mpsc::unbounded_channel();
    let (cancel, cancelled) = oneshot::channel::<()>();
    let picture = SharedPicture::default();
    let pending = Arc::new(AtomicBool::new(false));
    let mut link = Link { events: events_tx, input: input_rx, picture: picture.clone(), pending: pending.clone() };
    // A thread of its own with a single-threaded runtime: IronRDP's connection futures can't
    // move between threads (rustc can't prove them `Send`), and a desktop's work is one stream.
    let errors = link.events.clone();
    let spawned = std::thread::Builder::new().name("komando-desktop".into()).spawn(move || {
        link.status(Status::Connecting);
        let reason = match tokio::runtime::Builder::new_current_thread().enable_all().build() {
            Ok(runtime) => runtime.block_on(async {
                tokio::select! {
                    _ = cancelled => Ok("Session closed.".into()),
                    result = async {
                        match target.protocol {
                            Protocol::Rdp => rdp::run(target, &mut link).await,
                            Protocol::Vnc => vnc::run(target, &mut link).await,
                            Protocol::Ssh => Err(anyhow!("Not a remote desktop.")),
                        }
                    } => result,
                }
            }),
            Err(err) => Err(err.into()),
        };
        let reason = reason.unwrap_or_else(|err| format!("{err:#}"));
        link.status(Status::Disconnected(reason));
    });
    if let Err(err) = spawned {
        log::error!("starting a remote desktop: {err}");
        let _ = errors.unbounded_send(DesktopEvent::Status(Status::Disconnected(format!("Couldn't start the remote desktop: {err}"))));
    }
    Connection { input: input_tx, events: events_rx, picture, pending, _cancel: cancel }
}

/// "DOMAIN\user" as a domain and a user. A user with no backslash keeps the domain given.
fn split_domain(user: &str, domain: Option<&str>) -> (String, Option<String>) {
    match user.split_once('\\') {
        Some((domain, user)) if !domain.is_empty() => (user.to_string(), Some(domain.to_string())),
        _ => (user.to_string(), domain.filter(|d| !d.is_empty()).map(str::to_string)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn picture(width: u32, height: u32) -> Picture {
        let mut picture = Picture::default();
        picture.resize(width, height).unwrap();
        picture
    }

    #[test]
    fn a_rectangle_lands_where_it_is_put_and_is_opaque() {
        let mut p = picture(4, 3);
        p.put(1, 1, 2, 1, &[1, 2, 3, 0, 4, 5, 6, 0], 8);
        assert_eq!(&p.bgra[(4 + 1) * 4..(4 + 3) * 4], &[1, 2, 3, 0xff, 4, 5, 6, 0xff]);
        assert_eq!(&p.bgra[0..4], &[0, 0, 0, 0xff]);
    }

    #[test]
    fn a_rectangle_past_the_edge_is_cut_off() {
        let mut p = picture(2, 2);
        p.put(1, 1, 3, 3, &[9; 3 * 3 * 4], 12);
        assert_eq!(&p.bgra[(2 + 1) * 4..], &[9, 9, 9, 0xff]);
        assert_eq!(p.bgra.len(), 2 * 2 * 4);
    }

    #[test]
    fn invalid_rectangles_and_dimensions_do_not_allocate_or_panic() {
        let mut p = picture(2, 2);
        let before = p.bgra.clone();
        p.put(100, 0, 1, 1, &[1; 4], 4);
        p.put(0, 100, 1, 1, &[1; 4], 4);
        assert!(p.resize(u16::MAX.into(), u16::MAX.into()).is_err());
        assert!(p.resize(0, 1).is_err());
        assert_eq!(p.bgra, before);
    }

    fn target(protocol: Protocol, port: u16) -> Target {
        Target {
            protocol,
            address: "127.0.0.1".into(),
            port,
            route: Route::Direct { address: "127.0.0.1".into(), port },
            user: Some("test".into()),
            domain: None,
            password: Some(Zeroizing::new("test".into())),
            size: (800, 600),
            scale_percent: 100,
            shared: true,
            quality: Quality::High,
            certificates: std::env::temp_dir().join(format!("komando-test-{}", crate::model::new_id())),
        }
    }

    #[tokio::test]
    async fn dropping_a_connection_cancels_a_stalled_handshake() {
        use futures::StreamExt as _;
        use tokio::io::AsyncReadExt as _;

        for protocol in [Protocol::Rdp, Protocol::Vnc] {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let mut connection = spawn(target(protocol, listener.local_addr().unwrap().port()));
            let mut events = std::mem::replace(&mut connection.events, events::unbounded().1);
            let (mut socket, _) = tokio::time::timeout(Duration::from_secs(3), listener.accept()).await.unwrap().unwrap();
            drop(connection);
            tokio::time::timeout(Duration::from_secs(3), async {
                while let Some(event) = events.next().await {
                    if let DesktopEvent::Status(Status::Disconnected(reason)) = event {
                        assert_eq!(reason, "Session closed.");
                        let mut remaining = Vec::new();
                        socket.read_to_end(&mut remaining).await.unwrap();
                        return;
                    }
                }
                panic!("the worker ended without a disconnected status");
            })
            .await
            .expect("closing a desktop must cancel its handshake");
        }
    }

    /// A loopback RFB server exercises the real adapter, not just the framebuffer helpers.
    async fn vnc_round_trip(password_auth: bool) {
        use futures::StreamExt as _;
        use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let mut target = target(Protocol::Vnc, listener.local_addr().unwrap().port());
        target.password = None;
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            socket.write_all(b"RFB 003.008\n").await.unwrap();
            let mut version = [0; 12];
            socket.read_exact(&mut version).await.unwrap();
            assert_eq!(&version, b"RFB 003.008\n");
            let security = if password_auth { 2 } else { 1 };
            socket.write_all(&[1, security]).await.unwrap();
            assert_eq!(socket.read_u8().await.unwrap(), security);
            if password_auth {
                socket.write_all(&[0x42; 16]).await.unwrap();
                let mut response = [0; 16];
                socket.read_exact(&mut response).await.unwrap();
                assert_ne!(response, [0; 16]);
            }
            socket.write_all(&[0; 4]).await.unwrap();
            assert_eq!(socket.read_u8().await.unwrap(), 1, "the viewer shares the desktop");
            // ServerInit: 2×1 screen, 32-bit true colour, and the desktop name.
            socket.write_all(&[0, 2, 0, 1, 32, 24, 0, 1, 0, 255, 0, 255, 0, 255, 16, 8, 0, 0, 0, 0]).await.unwrap();
            socket.write_all(&4u32.to_be_bytes()).await.unwrap();
            socket.write_all(b"test").await.unwrap();
            let mut sent = false;
            loop {
                match socket.read_u8().await.unwrap() {
                    0 => {
                        let mut format = [0; 19];
                        socket.read_exact(&mut format).await.unwrap();
                    }
                    2 => {
                        socket.read_u8().await.unwrap();
                        let count = socket.read_u16().await.unwrap();
                        let mut encodings = vec![0; usize::from(count) * 4];
                        socket.read_exact(&mut encodings).await.unwrap();
                    }
                    3 => {
                        let mut request = [0; 9];
                        socket.read_exact(&mut request).await.unwrap();
                        if !sent {
                            // FramebufferUpdate, one raw rectangle in the requested BGRA format.
                            socket.write_all(&[0, 0, 0, 1, 0, 0, 0, 0, 0, 2, 0, 1, 0, 0, 0, 0, 1, 2, 3, 0, 4, 5, 6, 0]).await.unwrap();
                            sent = true;
                        }
                    }
                    4 => {
                        assert_eq!(socket.read_u8().await.unwrap(), 1);
                        socket.read_u16().await.unwrap();
                        assert_eq!(socket.read_u32().await.unwrap(), 0xff09, "Tab reaches the server");
                        return;
                    }
                    other => panic!("unexpected RFB message {other}"),
                }
            }
        });
        let mut connection = spawn(target);
        let mut remembered = false;
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                match connection.events.next().await.expect("desktop event") {
                    DesktopEvent::Prompt(DesktopPrompt::SignIn { needs_user, reply, .. }) => {
                        assert!(password_auth);
                        assert!(!needs_user);
                        reply.send(Some(SignIn { user: None, password: Zeroizing::new("test".into()), remember: true })).ok().unwrap();
                    }
                    DesktopEvent::Remember { .. } => remembered = true,
                    DesktopEvent::Status(Status::Disconnected(reason)) => panic!("connection ended: {reason}"),
                    DesktopEvent::Picture => {
                        connection.drawn();
                        let picture = connection.picture.lock().unwrap();
                        if picture.bgra == [1, 2, 3, 255, 4, 5, 6, 255] {
                            break;
                        }
                    }
                    _ => {}
                }
            }
            assert_eq!(remembered, password_auth);
            let key = keys::from_keystroke("tab", None, false, false).unwrap();
            connection.send(Input::Key { key, down: true });
            server.await.unwrap();
        })
        .await
        .expect("RFB sign-in, frame and input round trip");
    }

    #[tokio::test]
    async fn vnc_receives_a_frame_and_sends_input() {
        vnc_round_trip(false).await;
    }

    #[tokio::test]
    async fn vnc_prompts_for_a_password_and_remembers_it_after_sign_in() {
        vnc_round_trip(true).await;
    }

    #[test]
    fn copying_overlapping_rectangles_moves_what_was_there() {
        let mut p = picture(3, 1);
        p.put(0, 0, 3, 1, &[1, 1, 1, 0, 2, 2, 2, 0, 3, 3, 3, 0], 12);
        p.copy((1, 0), (0, 0), 2, 1);
        let firsts: Vec<u8> = p.bgra.chunks(4).map(|px| px[0]).collect();
        assert_eq!(firsts, [1, 1, 2]);
    }

    #[test]
    fn a_domain_can_be_typed_in_front_of_the_user() {
        assert_eq!(split_domain("CORP\\ada", None), ("ada".into(), Some("CORP".into())));
        assert_eq!(split_domain("ada", Some("CORP")), ("ada".into(), Some("CORP".into())));
        assert_eq!(split_domain("ada@corp.example", Some("")), ("ada@corp.example".into(), None));
    }
}
