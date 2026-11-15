//! VNC through vnc-rs: no authentication or classic VNC password authentication, ZRLE,
//! CopyRect and Raw pictures.
//!
//! Classic VNC encrypts nothing after the password exchange, so a Host's VNC desktop is best
//! reached through its SSH Session (`Desktop::through_ssh`); the form says so.

use std::collections::HashSet;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Result, anyhow};
use vnc::{ClientKeyEvent, ClientMouseEvent, PixelFormat, VncClient, VncConnector, VncEncoding, VncError, VncEvent, X11Event};
use zeroize::Zeroizing;

use super::keys::{self, Key};
use super::{Button, DesktopEvent, Input, Link, Status, Target};

/// How often Komando asks for what changed on screen. Servers answer when something has.
const REFRESH: Duration = Duration::from_millis(33);

const ATTEMPTS: usize = 5;

/// The password the server asked for, typed in: kept to remember it once signed in.
type Typed = Arc<Mutex<Option<(Zeroizing<String>, bool)>>>;

pub(super) async fn run(target: Target, link: &mut Link) -> Result<String> {
    let mut password = target.password.clone();
    let mut error = None;
    for _ in 0..ATTEMPTS {
        let typed = Typed::default();
        match connect(&target, link, password.take(), error.take(), typed.clone()).await {
            Ok(client) => {
                if let Some((password, true)) = typed.lock().expect("typed").take() {
                    link.send(DesktopEvent::Remember { user: None, password });
                }
                link.status(Status::Connected);
                return active(client, link).await;
            }
            Err(VncError::WrongPassword) => error = Some("The password is incorrect.".to_string()),
            Err(VncError::General(reason)) if reason == CANCELLED => return Ok("Sign-in cancelled.".into()),
            Err(err @ VncError::General(_)) => return Err(anyhow!("{}", describe(&err))),
            Err(err) => return Err(anyhow!("Connecting failed: {}", describe(&err))),
        }
    }
    Ok("Signing in failed too many times.".into())
}

const CANCELLED: &str = "cancelled";

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
) -> Result<VncClient, VncError> {
    let stream = target.route.open().await.map_err(|err| VncError::General(format!("{err:#}")))?;
    // Asked only if the server wants a password: a saved one first, else the user.
    let events = link.events.clone();
    let ask = async move {
        if let Some(password) = password {
            return Ok(password.to_string());
        }
        let (reply, answer) = futures::channel::oneshot::channel();
        let prompt = super::DesktopPrompt::SignIn { user: None, needs_user: false, error, reply };
        let _ = events.unbounded_send(DesktopEvent::Prompt(prompt));
        match answer.await.ok().flatten() {
            Some(answer) => {
                let password = answer.password.to_string();
                *typed.lock().expect("typed") = Some((answer.password, answer.remember));
                Ok(password)
            }
            None => Err(VncError::General(CANCELLED.into())),
        }
    };
    VncConnector::new(stream)
        .set_auth_method(ask)
        .add_encoding(VncEncoding::Zrle)
        .add_encoding(VncEncoding::CopyRect)
        .add_encoding(VncEncoding::Raw)
        .add_encoding(VncEncoding::DesktopSizePseudo)
        .allow_shared(target.shared)
        .set_pixel_format(PixelFormat::bgra())
        .build()?
        .try_start()
        .await?
        .finish()
}

async fn active(client: VncClient, link: &mut Link) -> Result<String> {
    let mut refresh = tokio::time::interval(REFRESH);
    let mut pointer = Pointer::default();
    let _ = client.input(X11Event::FullRefresh).await;
    let reason = loop {
        tokio::select! {
            event = client.recv_event() => match event {
                Ok(event) => {
                    if let Some(reason) = show(event, link)? {
                        break reason;
                    }
                }
                Err(err) => break format!("The connection ended: {err}"),
            },
            input = link.input.recv() => {
                let Some(input) = input else { break "Session closed.".into() };
                if matches!(input, Input::Close) {
                    break "Session closed.".into();
                }
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
fn show(event: VncEvent, link: &Link) -> Result<Option<String>> {
    match event {
        VncEvent::SetResolution(screen) => link.draw(|p| p.resize(screen.width.into(), screen.height.into()))?,
        VncEvent::RawImage(rect, data) => {
            link.draw(|p| p.put(rect.x.into(), rect.y.into(), rect.width.into(), rect.height.into(), &data, usize::from(rect.width) * 4))
        }
        VncEvent::Copy(to, from) => {
            link.draw(|p| p.copy((to.x.into(), to.y.into()), (from.x.into(), from.y.into()), to.width.into(), to.height.into()))
        }
        VncEvent::Error(reason) => return Ok(Some(format!("The connection ended: {reason}"))),
        // Not asked for (Tight, cursor shapes), or not used yet (the server's clipboard).
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
                let notches = (i32::from(amount).abs() / 120).max(1);
                (0..notches).flat_map(|_| [self.message(self.buttons | bit), self.message(self.buttons)]).collect()
            }
            Input::CtrlAltDel => {
                let combo = [keys::CONTROL, keys::ALT, keys::DELETE];
                combo.iter().map(|k| key(k.keysym, true)).chain(combo.iter().rev().map(|k| key(k.keysym, false))).collect()
            }
            Input::ReleaseAll => {
                let mut events: Vec<X11Event> = self.held.drain().map(|keysym| key(keysym, false)).collect();
                if self.buttons != 0 {
                    self.buttons = 0;
                    events.push(self.message(0));
                }
                events
            }
            Input::Resize { .. } | Input::Close => vec![],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
