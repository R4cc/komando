//! VNC through vnc-rs: no authentication or classic VNC password authentication, ZRLE,
//! CopyRect and Raw pictures.
//!
//! Classic VNC encrypts nothing after the password exchange, so a Host's VNC desktop is best
//! reached through its SSH Session (`Desktop::through_ssh`); the form says so.
//!
//! The clipboard is VNC's cut text: plain text both ways, with LF line breaks. The protocol
//! defines it as Latin-1; vnc-rs sends and reads UTF-8, which servers that follow the letter
//! show as other characters past ASCII.

use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Result, anyhow};
use vnc::{ClientKeyEvent, ClientMouseEvent, PixelFormat, VncClient, VncConnector, VncEncoding, VncError, VncEvent, X11Event};
use zeroize::Zeroizing;

use super::keys::{self, Key};
use super::{Attempt, Button, DesktopEvent, HANDSHAKE_TIMEOUT, Input, Link, Quality, Target, handshake_timed_out, sign_in};
use crate::session::ssh::ConnectClock;

/// How often Komando asks for what changed on screen. Servers answer when something has.
const REFRESH: Duration = Duration::from_millis(33);

/// The most text sent to the clipboard at once. vnc-rs drops the connection on more coming
/// back, and servers may do the same.
const MAX_CLIPBOARD: usize = 1024 * 1024;

/// The password the server asked for, typed in: kept to remember it once signed in.
type Typed = Arc<Mutex<Option<(Zeroizing<String>, bool)>>>;

/// vnc-rs's words when the server offers only sign-ins it doesn't implement (Apple, VeNCrypt,
/// TLS…).
const UNSUPPORTED_SIGN_IN: &str = "Security type apart from Vnc Auth has not been implemented";

pub(super) async fn run(target: Target, link: &mut Link) -> Result<String> {
    let mut password = target.password.clone();
    let attempt = async |link: &Link, error: Option<String>| {
        let typed = Typed::default();
        let sent = Arc::new(AtomicBool::new(false));
        match connect(&target, link, password.take(), error, typed.clone(), sent.clone(), HANDSHAKE_TIMEOUT).await {
            Ok(client) => {
                let remember = typed.lock().expect("typed").take().filter(|(_, remember)| *remember);
                Ok(Attempt::Connected { connection: client, remember: remember.map(|(password, _)| (None, password)) })
            }
            Err(VncError::WrongPassword) => Ok(Attempt::Retry(Some("The password is incorrect.".to_string()))),
            Err(VncError::General(reason)) if reason == CANCELLED => Ok(Attempt::Cancelled),
            Err(VncError::General(reason)) if reason == TIMED_OUT => Err(handshake_timed_out()),
            // RFB 3.8 servers refuse a password with a reason of their own rather than as
            // "wrong password": once one was sent, ask again, with the server's words.
            Err(VncError::General(reason)) if sent.load(Ordering::Acquire) => {
                let reason = reason.trim();
                Ok(Attempt::Retry(Some(if reason.is_empty() {
                    "The password is incorrect.".to_string()
                } else {
                    format!("The password was refused: {reason}")
                })))
            }
            Err(VncError::General(reason)) if reason == UNSUPPORTED_SIGN_IN => {
                Err(anyhow!("{}", describe(&VncError::ConnectError)))
            }
            Err(err @ VncError::General(_)) => Err(anyhow!("{}", describe(&err))),
            Err(err) => Err(anyhow!("Connecting failed: {}", describe(&err))),
        }
    };
    let active = async |client, link: &mut Link| active(client, link, pixel_format(target.quality), target.clipboard).await;
    sign_in(link, attempt, active).await
}

const CANCELLED: &str = "cancelled";
const TIMED_OUT: &str = "timed out";

fn describe(err: &VncError) -> String {
    match err {
        VncError::InvalidSecurityTyep(_) | VncError::ConnectError => {
            "the server wants a kind of sign-in Komando doesn't support (only none and VNC password are).".into()
        }
        VncError::General(reason) => reason.clone(),
        other => other.to_string(),
    }
}

async fn connect(
    target: &Target,
    link: &Link,
    password: Option<Zeroizing<String>>,
    error: Option<String>,
    typed: Typed,
    sent: Arc<AtomicBool>,
    timeout: Duration,
) -> Result<VncClient, VncError> {
    let stream = target.route.open().await.map_err(|err| VncError::General(format!("{err:#}")))?;
    // Paused while the sign-in prompt is open: the user may take longer than the timeout.
    let clock = ConnectClock::new();
    // Asked only if the server wants a password: a saved one first, else the user.
    let events = link.events.clone();
    let asking = clock.clone();
    let ask = async move {
        if let Some(password) = password {
            sent.store(true, Ordering::Release);
            return Ok(password.to_string());
        }
        let (reply, answer) = futures::channel::oneshot::channel();
        let prompt = super::DesktopPrompt::SignIn { user: None, needs_user: false, error, reply };
        asking.pause();
        let _ = events.unbounded_send(DesktopEvent::Prompt(prompt));
        let answer = answer.await.ok().flatten();
        asking.resume();
        match answer {
            Some(answer) => {
                let password = answer.password.to_string();
                *typed.lock().expect("typed") = Some((answer.password, answer.remember));
                sent.store(true, Ordering::Release);
                Ok(password)
            }
            None => Err(VncError::General(CANCELLED.into())),
        }
    };
    let handshake = async {
        VncConnector::new(stream)
            .set_auth_method(ask)
            .add_encoding(VncEncoding::Zrle)
            .add_encoding(VncEncoding::CopyRect)
            .add_encoding(VncEncoding::Raw)
            .add_encoding(VncEncoding::DesktopSizePseudo)
            .allow_shared(target.shared)
            .set_pixel_format(pixel_format(target.quality))
            .build()?
            .try_start()
            .await?
            .finish()
    };
    clock.limit(timeout, handshake).await.unwrap_or_else(|| Err(VncError::General(TIMED_OUT.into())))
}

/// The pixels asked of the server: full color, or fewer bits per pixel to send less.
fn pixel_format(quality: Quality) -> PixelFormat {
    let mut format = PixelFormat::bgra();
    match quality {
        Quality::High => {}
        // RGB565.
        Quality::Medium => {
            format.bits_per_pixel = 16;
            format.depth = 16;
            (format.red_max, format.green_max, format.blue_max) = (31, 63, 31);
            (format.red_shift, format.green_shift, format.blue_shift) = (11, 5, 0);
        }
        // BGR233.
        Quality::Low => {
            format.bits_per_pixel = 8;
            format.depth = 8;
            (format.red_max, format.green_max, format.blue_max) = (7, 7, 3);
            (format.red_shift, format.green_shift, format.blue_shift) = (0, 3, 6);
        }
    }
    format
}

/// Pixels in `format` (little endian, true color) as the picture's BGRA. Fewer bits per pixel
/// are widened into `scratch`, kept from one rectangle to the next so a busy screen doesn't
/// allocate for each, and converted before the picture is locked.
fn to_bgra<'a>(data: &'a [u8], format: &PixelFormat, scratch: &'a mut Vec<u8>) -> &'a [u8] {
    let bytes = usize::from(format.bits_per_pixel / 8);
    if bytes == 4 {
        return data;
    }
    // A channel of `max` scaled to 0..=255.
    let scale = |value: u32, max: u16| (value * 255 / u32::from(max.max(1))) as u8;
    scratch.clear();
    scratch.reserve(data.len() / bytes * 4);
    for pixel in data.chunks_exact(bytes) {
        let value = match bytes {
            2 => u32::from(u16::from_le_bytes([pixel[0], pixel[1]])),
            _ => u32::from(pixel[0]),
        };
        let channel = |shift: u8, max: u16| scale((value >> shift) & u32::from(max), max);
        scratch.extend_from_slice(&[
            channel(format.blue_shift, format.blue_max),
            channel(format.green_shift, format.green_max),
            channel(format.red_shift, format.red_max),
            0xff,
        ]);
    }
    scratch
}

async fn active(client: VncClient, link: &mut Link, format: PixelFormat, mut sharing: bool) -> Result<String> {
    let mut refresh = tokio::time::interval(REFRESH);
    let mut pointer = Pointer::default();
    let mut scratch = Vec::new();
    let _ = client.input(X11Event::FullRefresh).await;
    let reason = loop {
        tokio::select! {
            event = client.recv_event() => match event {
                Ok(event) => {
                    if let Some(reason) = show(event, link, &format, &mut scratch, sharing)? {
                        break reason;
                    }
                }
                Err(err) => break format!("The connection ended: {err}"),
            },
            input = link.input.recv() => {
                let Some(input) = input else { break "Session closed.".into() };
                let input = match input {
                    Input::Close => break "Session closed.".into(),
                    Input::ShareClipboard(on) => {
                        sharing = on;
                        continue;
                    }
                    Input::Clipboard(text) => {
                        let text = super::with_lf(&text);
                        if !sharing || text.len() > MAX_CLIPBOARD {
                            continue;
                        }
                        if let Err(err) = client.input(X11Event::CopyText(text)).await {
                            break format!("The connection ended: {err}");
                        }
                        continue;
                    }
                    input => input,
                };
                let mut failed = None;
                for event in pointer.events(input) {
                    if let Err(err) = client.input(event).await {
                        failed = Some(format!("The connection ended: {err}"));
                        break;
                    }
                }
                if let Some(reason) = failed {
                    break reason;
                }
            }
            _ = refresh.tick() => {
                if client.input(X11Event::Refresh).await.is_err() {
                    break "The connection ended.".into();
                }
            }
        }
    };
    let _ = client.close().await;
    Ok(reason)
}

/// Draw what the server sent. `Some` ends the session, with why.
fn show(event: VncEvent, link: &Link, format: &PixelFormat, scratch: &mut Vec<u8>, sharing: bool) -> Result<Option<String>> {
    match event {
        VncEvent::SetResolution(screen) => link.draw(|p| p.resize(screen.width.into(), screen.height.into()))?,
        // A server that changes its layout on its own (wayvnc): the picture follows.
        VncEvent::DesktopUpdate(vnc::DesktopUpdate { layout: Some(layout), .. }) => {
            link.draw(|p| p.resize(layout.width.into(), layout.height.into()))?
        }
        VncEvent::RawImage(rect, data) => {
            let data = to_bgra(&data, format, scratch);
            link.draw(|p| p.put(rect.x.into(), rect.y.into(), rect.width.into(), rect.height.into(), data, usize::from(rect.width) * 4))
        }
        VncEvent::Copy(to, from) => {
            link.draw(|p| p.copy((to.x.into(), to.y.into()), (from.x.into(), from.y.into()), to.width.into(), to.height.into()))
        }
        VncEvent::Text(text) if sharing => link.send(DesktopEvent::Clipboard(super::for_this_pc(&text))),
        VncEvent::Error(reason) => return Ok(Some(format!("The connection ended: {reason}"))),
        // Not asked for (Tight, cursor shapes), or not shared (the server's clipboard).
        _ => {}
    }
    Ok(None)
}

/// The VNC pointer: one message carries the position and every button held.
#[derive(Default)]
struct Pointer {
    x: u16,
    y: u16,
    buttons: u8,
    held: HashSet<u32>,
    /// Wheel movement short of a notch (120), down and across: a touchpad sends many small
    /// steps that add up.
    wheel: [i32; 2],
}

impl Pointer {
    fn message(&self, buttons: u8) -> X11Event {
        X11Event::PointerEvent(ClientMouseEvent { position_x: self.x, position_y: self.y, bottons: buttons })
    }

    fn events(&mut self, input: Input) -> Vec<X11Event> {
        let key = |keysym: u32, down: bool| X11Event::KeyEvent(ClientKeyEvent { keycode: keysym, down });
        match input {
            Input::Key { key: Key::Char(c), down: true } => {
                let keysym = keys::keysym_for_char(c);
                vec![key(keysym, true), key(keysym, false)]
            }
            Input::Key { key: Key::Char(_), down: false } => vec![],
            Input::Key { key: Key::Code(code), down } => {
                if down {
                    self.held.insert(code.keysym);
                } else {
                    self.held.remove(&code.keysym);
                }
                vec![key(code.keysym, down)]
            }
            Input::Pointer { x, y } => {
                (self.x, self.y) = (x, y);
                vec![self.message(self.buttons)]
            }
            Input::Button { button, down } => {
                let bit = match button {
                    Button::Left => 1,
                    Button::Middle => 2,
                    Button::Right => 4,
                };
                self.buttons = if down { self.buttons | bit } else { self.buttons & !bit };
                vec![self.message(self.buttons)]
            }
            // Each notch is a press and release of button 4 (up), 5 (down), 6 (left) or 7 (right).
            Input::Wheel { vertical, amount } => {
                let bit = match (vertical, amount > 0) {
                    (true, true) => 8,
                    (true, false) => 16,
                    (false, true) => 64,
                    (false, false) => 32,
                };
                let rest = &mut self.wheel[usize::from(!vertical)];
                // Turning back starts over, so the first step the other way isn't lost.
                if (*rest > 0) != (amount > 0) {
                    *rest = 0;
                }
                *rest += i32::from(amount);
                let notches = rest.abs() / 120;
                *rest %= 120;
                (0..notches).flat_map(|_| [self.message(self.buttons | bit), self.message(self.buttons)]).collect()
            }
            Input::ReleaseAll => {
                let mut events: Vec<X11Event> = self.held.drain().map(|keysym| key(keysym, false)).collect();
                if self.buttons != 0 {
                    self.buttons = 0;
                    events.push(self.message(0));
                }
                events
            }
            // Handled by the session loop.
            Input::Resize { .. } | Input::Clipboard(_) | Input::ShareClipboard(_) | Input::Close => vec![],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::desktop::Protocol;
    use crate::desktop::tests::{rfb_accept, target};

    const TEST_TIMEOUT: Duration = Duration::from_millis(200);

    /// A link whose events the test reads; the receiver must outlive the connection.
    fn link() -> (Link, futures::channel::mpsc::UnboundedReceiver<DesktopEvent>) {
        let (events, receiver) = futures::channel::mpsc::unbounded();
        let (_input, input) = tokio::sync::mpsc::unbounded_channel();
        (Link { events, input, picture: Default::default(), pending: Default::default() }, receiver)
    }

    async fn connect_within(target: &Target, link: &Link) -> Result<VncClient, VncError> {
        connect(target, link, target.password.clone(), None, Typed::default(), Arc::default(), TEST_TIMEOUT).await
    }

    #[tokio::test]
    async fn a_server_that_never_speaks_times_out() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let target = target(Protocol::Vnc, listener.local_addr().unwrap().port());
        // Accepted, then silent: the socket stays open until the test ends.
        let server = tokio::spawn(async move { listener.accept().await.unwrap() });
        let (link, _events) = link();
        let result = tokio::time::timeout(Duration::from_secs(3), connect_within(&target, &link))
            .await
            .expect("the handshake must give up on its own");
        assert!(matches!(result, Err(VncError::General(reason)) if reason == TIMED_OUT));
        drop(server);
    }

    #[tokio::test]
    async fn the_sign_in_prompt_does_not_count_towards_the_timeout() {
        use futures::StreamExt as _;

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let mut target = target(Protocol::Vnc, listener.local_addr().unwrap().port());
        target.password = None;
        let server = tokio::spawn(async move { rfb_accept(&listener, true).await });
        let (link, mut events) = link();
        // The user takes several timeouts to type the password.
        let user = async {
            while let Some(event) = events.next().await {
                if let DesktopEvent::Prompt(super::super::DesktopPrompt::SignIn { reply, .. }) = event {
                    tokio::time::sleep(TEST_TIMEOUT * 4).await;
                    let answer = super::super::SignIn { user: None, password: Zeroizing::new("test".into()), remember: false };
                    reply.send(Some(answer)).ok().unwrap();
                    return;
                }
            }
        };
        let (result, ()) = tokio::time::timeout(Duration::from_secs(5), futures::future::join(connect_within(&target, &link), user))
            .await
            .expect("sign-in round trip");
        assert!(result.is_ok(), "signed in after a slow answer: {:?}", result.err());
        let _socket = server.await.unwrap();
    }

    #[test]
    fn fewer_colors_become_full_bgra() {
        // RGB565 white, pure red, and black.
        let medium = pixel_format(Quality::Medium);
        let pixels = [0xffff_u16, 0xf800, 0x0000].iter().flat_map(|p| p.to_le_bytes()).collect::<Vec<u8>>();
        let mut scratch = Vec::new();
        assert_eq!(to_bgra(&pixels, &medium, &mut scratch), &[255, 255, 255, 255, 0, 0, 255, 255, 0, 0, 0, 255]);
        // BGR233: blue in the top two bits, green in the middle three, red in the low three.
        let low = pixel_format(Quality::Low);
        // The buffer is reused: nothing of the longer rectangle before is left over.
        assert_eq!(to_bgra(&[0b1100_0000, 0b0000_0111], &low, &mut scratch), &[255, 0, 0, 255, 0, 0, 255, 255]);
        // Full color is passed through untouched.
        let high = pixel_format(Quality::High);
        let data = [1, 2, 3, 4];
        assert_eq!(to_bgra(&data, &high, &mut scratch).as_ptr(), data.as_ptr());
    }

    #[test]
    fn small_wheel_steps_add_up_to_notches() {
        let mut pointer = Pointer::default();
        let notches = |events: Vec<X11Event>| events.len() / 2;
        // A touchpad's small steps: nothing until they make a notch.
        assert_eq!(notches(pointer.events(Input::Wheel { vertical: true, amount: 60 })), 0);
        assert_eq!(notches(pointer.events(Input::Wheel { vertical: true, amount: 60 })), 1);
        // A mouse notch is one notch; three at once are three.
        assert_eq!(notches(pointer.events(Input::Wheel { vertical: true, amount: 120 })), 1);
        assert_eq!(notches(pointer.events(Input::Wheel { vertical: true, amount: 360 })), 3);
        // Turning back isn't held up by what was left the other way.
        pointer.events(Input::Wheel { vertical: true, amount: 100 });
        assert_eq!(notches(pointer.events(Input::Wheel { vertical: true, amount: -120 })), 1);
    }

    #[test]
    fn focus_loss_releases_shortcut_keys_and_mouse_buttons() {
        let mut pointer = Pointer::default();
        let a = keys::from_keystroke("a", None, true, false).unwrap();
        pointer.events(Input::Key { key: Key::Code(keys::CONTROL), down: true });
        pointer.events(Input::Key { key: a, down: true });
        pointer.events(Input::Button { button: Button::Left, down: true });
        let released = pointer.events(Input::ReleaseAll);
        let keys: HashSet<u32> = released
            .iter()
            .filter_map(|e| match e {
                X11Event::KeyEvent(e) if !e.down => Some(e.keycode),
                _ => None,
            })
            .collect();
        assert_eq!(keys, [keys::CONTROL.keysym, 'a' as u32].into_iter().collect());
        assert!(released.iter().any(|e| matches!(e, X11Event::PointerEvent(e) if e.bottons == 0)));
        assert!(pointer.events(Input::ReleaseAll).is_empty());
    }
}
