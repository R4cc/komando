//! RDP through IronRDP: TLS with the certificate checked like a Host Key, then Network Level
//! Authentication (CredSSP with NTLM) or, for servers without it, TLS sign-in.
//!
//! Komando advertises only what a plain desktop needs: the display, keyboard and mouse, the
//! Display Control channel to follow the pane's size, and the clipboard for text (see
//! `clipboard`). No sound, drives, printers or files are redirected.

mod clipboard;

use anyhow::{Context as _, Result, anyhow};
use ironrdp_connector::connection_activation::ConnectionActivationState;
use ironrdp_connector::sspi::generator::NetworkRequest;
use ironrdp_connector::{
    ClientConnector, ConnectionResult, ConnectorError, ConnectorErrorKind, ConnectorResult, Credentials, DesktopSize, ServerName,
};
use ironrdp_core::WriteBuf;
use ironrdp_displaycontrol::client::DisplayControlClient;
use ironrdp_displaycontrol::pdu::MonitorLayoutEntry;
use ironrdp_dvc::DrdynvcClient;
use ironrdp_graphics::image_processing::PixelFormat;
use ironrdp_input::{Database, MouseButton, MousePosition, Operation, Scancode, WheelRotations};
use ironrdp_pdu::gcc::{ConnectionType, KeyboardType};
use ironrdp_pdu::geometry::InclusiveRectangle;
use ironrdp_pdu::rdp::capability_sets::{MajorPlatformType, RailSupportLevel};
use ironrdp_pdu::rdp::client_info::{PerformanceFlags, TimezoneInfo};
use ironrdp_session::image::DecodedImage;
use ironrdp_cliprdr::CliprdrClient;
use ironrdp_session::{ActiveStage, ActiveStageBuilder, ActiveStageOutput};
use ironrdp_tokio::{FramedWrite as _, TokioFramed, single_sequence_step_read, split_tokio_framed};
use zeroize::Zeroizing;

use crate::session::CLOSED_BY_USER;
use super::keys::{self, Key};
use super::trust::{self, Known};
use super::{
    Attempt, Button, DesktopEvent, HANDSHAKE_TIMEOUT, Input, Link, Picture, Quality, Stream, Target, handshake_timed_out,
    sign_in, split_domain,
};
use clipboard::Clipboard;

type Framed = TokioFramed<Stream>;

pub(super) async fn run(target: Target, link: &mut Link) -> Result<String> {
    let mut user = target.user.clone().filter(|u| !u.is_empty());
    let mut password = target.password.clone();
    let attempt = async |link: &Link, error: Option<String>| {
        let mut remember = false;
        if user.is_none() || password.is_none() || error.is_some() {
            let Some(answer) = link.ask_sign_in(user.clone(), true, error).await else { return Ok(Attempt::Cancelled) };
            user = answer.user.filter(|u| !u.is_empty());
            password = Some(answer.password);
            remember = answer.remember;
        }
        let (Some(name), Some(secret)) = (user.clone(), password.clone()) else { return Ok(Attempt::Retry(None)) };
        match connect(&target, link, &name, &secret).await {
            Ok(connection) => Ok(Attempt::Connected { connection, remember: remember.then_some((Some(name), secret)) }),
            Err(Failure::SignIn(message)) => Ok(Attempt::Retry(Some(message))),
            Err(Failure::Other(err)) => Err(err),
        }
    };
    let active = async |(framed, result, clipboard), link: &mut Link| active(framed, result, clipboard, link).await;
    sign_in(link, attempt, active).await
}

enum Failure {
    /// The server refused the user name or password: ask again.
    SignIn(String),
    Other(anyhow::Error),
}

impl From<anyhow::Error> for Failure {
    fn from(err: anyhow::Error) -> Self {
        Failure::Other(err)
    }
}

async fn connect(
    target: &Target,
    link: &Link,
    user: &str,
    password: &Zeroizing<String>,
) -> Result<(Framed, ConnectionResult, Clipboard), Failure> {
    let stream = target.route.open().await?;
    let mut framed = Framed::new(stream);
    let (username, domain) = split_domain(user, target.domain.as_deref());
    let config = config(target, username, password.to_string(), domain);
    let channels = DrdynvcClient::new().with_dynamic_channel(DisplayControlClient::new(|_| Ok(Vec::new())));
    let (cliprdr, clipboard) = Clipboard::new(target.clipboard);
    let mut connector =
        ClientConnector::new(config, target.route.local_address()).with_static_channel(channels).with_static_channel(cliprdr);

    let should_upgrade = within(ironrdp_tokio::connect_begin(&mut framed, &mut connector)).await?.map_err(failure)?;
    let (stream, leftover) = framed.into_inner();
    let (tls, der, public_key) = within(upgrade_tls(stream, &target.address)).await??;
    check_certificate(target, link, &der).await?;

    let upgraded = ironrdp_tokio::mark_as_upgraded(should_upgrade, &mut connector);
    let mut framed = Framed::new_with_leftover(Box::new(tls) as Stream, leftover);
    let result = within(ironrdp_tokio::connect_finalize(
        upgraded,
        connector,
        &mut framed,
        &mut NoKerberos,
        ServerName::new(target.address.clone()),
        public_key,
        None,
    ))
    .await?
    .map_err(failure)?
    .into_connected()
    .map_err(|_| anyhow!("The server redirected the connection to another server, which Komando doesn't follow."))?;
    Ok((framed, result, clipboard))
}

/// TLS with the server: the stream, the server's certificate (DER) and its public key.
///
/// Any certificate chain is accepted here because `check_certificate` pins the certificate
/// itself, trusted on first use like a Host Key. rustls still checks the handshake signature
/// against that certificate's key, which proves the server holds the private key. The pin
/// depends on that check: a certificate is public, so without it anyone on the path could
/// present the server's certificate and match the pin. `ironrdp_tls::upgrade` skips the
/// signature check, which is why it isn't used.
async fn upgrade_tls<S>(stream: S, address: &str) -> Result<(ironrdp_tls::TlsStream<S>, Vec<u8>, Vec<u8>)>
where
    S: Unpin + tokio::io::AsyncRead + tokio::io::AsyncWrite,
{
    let accept_any_chain: ironrdp_tls::CertificateValidationCallback = std::sync::Arc::new(|_, _, _| true);
    let (tls, certificate) = ironrdp_tls::upgrade_with_certificate_validation_callback(stream, address, accept_any_chain)
        .await
        .map_err(|err| anyhow!("TLS with the server failed: {err}"))?;
    let der = tls.get_ref().1.peer_certificates().and_then(|c| c.first()).map(|c| c.to_vec());
    let der = der.ok_or_else(|| anyhow!("The server sent no certificate."))?;
    let public_key = ironrdp_tls::extract_tls_server_public_key(&certificate)
        .ok_or_else(|| anyhow!("The server's certificate has no public key Komando can read."))?
        .to_vec();
    Ok((tls, der, public_key))
}

/// One step of reaching the desktop, given [`HANDSHAKE_TIMEOUT`].
async fn within<T>(step: impl std::future::Future<Output = T>) -> Result<T, Failure> {
    tokio::time::timeout(HANDSHAKE_TIMEOUT, step).await.map_err(|_| Failure::Other(handshake_timed_out()))
}

/// Trusted on first use, like a Host Key: most RDP servers sign their own certificate.
async fn check_certificate(target: &Target, link: &Link, der: &[u8]) -> Result<()> {
    let server = format!("{}:{}", target.address, target.port);
    let fingerprint = trust::fingerprint(der);
    let known = trust::check(&target.certificates, &server, &fingerprint)
        .with_context(|| format!("Couldn't read the trusted certificates in {}", target.certificates.display()))?;
    let previous = match known {
        Known::Trusted => return Ok(()),
        Known::New => None,
        Known::Changed { previous } => Some(previous),
    };
    if !link.ask_certificate(fingerprint.clone(), previous).await {
        return Err(anyhow!("The server's certificate was not trusted."));
    }
    trust::trust(&target.certificates, &server, &fingerprint)?;
    Ok(())
}

/// What the server said, in words for the user. A refused password asks again.
fn failure(err: ConnectorError) -> Failure {
    let text = format!("{err:#}");
    let lower = text.to_lowercase();
    // Windows names why it refused (NTSTATUS). These aren't a wrong password: asking again
    // would only count more failed sign-ins against the account.
    const REFUSALS: [(&str, &str); 6] = [
        ("STATUS_ACCOUNT_LOCKED_OUT", "This account is locked. Ask its administrator to unlock it, or wait and try again later."),
        ("STATUS_ACCOUNT_DISABLED", "This account is disabled on the server."),
        ("STATUS_PASSWORD_EXPIRED", "The password of this account has expired. Change it on the server (or with its administrator), then connect again."),
        ("STATUS_PASSWORD_MUST_CHANGE", "The password of this account must be changed before it can sign in. Change it on the server, then connect again."),
        ("STATUS_LOGON_TYPE_NOT_GRANTED", "This account isn't allowed to sign in through Remote Desktop. Add it to the server's Remote Desktop Users group."),
        ("STATUS_INVALID_LOGON_HOURS", "This account isn't allowed to sign in at this time of day."),
    ];
    if let Some((_, message)) = REFUSALS.iter().find(|(status, _)| text.contains(status)) {
        return Failure::Other(anyhow!("{message}"));
    }
    match err.kind() {
        ConnectorErrorKind::Credssp(_) if lower.contains("logon") || lower.contains("password") || lower.contains("credentials") => {
            Failure::SignIn("The user name or password is incorrect.".into())
        }
        ConnectorErrorKind::Credssp(_) => Failure::SignIn(format!("Signing in failed: {text}")),
        ConnectorErrorKind::AccessDenied => Failure::Other(anyhow!("The server denied access to this account.")),
        ConnectorErrorKind::Negotiation(_) => Failure::Other(anyhow!(
            "The server and Komando share no way to secure the connection ({text}). Komando needs TLS or Network Level Authentication."
        )),
        _ => Failure::Other(anyhow!("Connecting failed: {text}")),
    }
}

fn config(target: &Target, username: String, password: String, domain: Option<String>) -> ironrdp_connector::Config {
    let (width, height) = MonitorLayoutEntry::adjust_display_size(target.size.0, target.size.1);
    ironrdp_connector::Config {
        desktop_size: DesktopSize { width: width as u16, height: height as u16 },
        monitor_layout: None,
        desktop_scale_factor: target.scale_percent,
        desktop_physical_size: None,
        desktop_orientation: None,
        device_scale_factor: None,
        monitor_extended_layout: None,
        relative_mouse_input: false,
        // Network Level Authentication first; TLS sign-in for servers without it (xrdp).
        // Never Standard RDP Security: IronRDP has it only unencrypted.
        enable_tls: true,
        enable_credssp: true,
        enable_standard_rdp_security: false,
        credentials: Credentials::UsernamePassword { username, password },
        domain,
        client_build: 0,
        client_name: client_name(),
        keyboard_type: KeyboardType::IBM_ENHANCED,
        keyboard_subtype: 0,
        keyboard_functional_keys_count: 12,
        // 0: the server's layout, which characters (sent as Unicode) don't depend on.
        keyboard_layout: 0,
        connection_type: ConnectionType::Autodetect,
        ime_file_name: String::new(),
        bitmap: None,
        dig_product_id: String::new(),
        client_dir: "C:\\Windows\\System32\\mstscax.dll".into(),
        alternate_shell: String::new(),
        work_dir: String::new(),
        remote_application_mode: false,
        rail_support_level: RailSupportLevel::empty(),
        platform: MajorPlatformType::WINDOWS,
        hardware_id: None,
        request_data: None,
        // The password is given: sign in without the server's own sign-in screen.
        autologon: true,
        enable_audio_playback: false,
        enable_audio_capture: false,
        performance_flags: performance_flags(target.quality),
        license_cache: None,
        timezone_info: TimezoneInfo::default(),
        compression_type: None,
        // Windows' own arrow stands for the pointer: gpui can't hide it, and a second pointer
        // drawn into the picture would trail behind it.
        enable_server_pointer: false,
        pointer_software_rendering: false,
        multitransport_flags: None,
        // The graphics pipeline (H.264 and friends) needs codecs Komando doesn't ship.
        support_dyn_vc_gfx_protocol: false,
    }
}

/// What the server may leave out to send less: nothing at High, decoration at Medium, all
/// visual effects at Low.
fn performance_flags(quality: Quality) -> PerformanceFlags {
    match quality {
        Quality::High => PerformanceFlags::ENABLE_FONT_SMOOTHING | PerformanceFlags::ENABLE_DESKTOP_COMPOSITION,
        Quality::Medium => {
            PerformanceFlags::DISABLE_WALLPAPER
                | PerformanceFlags::DISABLE_MENUANIMATIONS
                | PerformanceFlags::DISABLE_CURSOR_SHADOW
                | PerformanceFlags::ENABLE_FONT_SMOOTHING
        }
        Quality::Low => {
            PerformanceFlags::DISABLE_WALLPAPER
                | PerformanceFlags::DISABLE_FULLWINDOWDRAG
                | PerformanceFlags::DISABLE_MENUANIMATIONS
                | PerformanceFlags::DISABLE_THEMING
                | PerformanceFlags::DISABLE_CURSOR_SHADOW
                | PerformanceFlags::DISABLE_CURSORSETTINGS
        }
    }
}

/// This PC's name, as the server lists the client (at most 15 characters).
fn client_name() -> String {
    std::env::var("COMPUTERNAME").ok().filter(|n| !n.is_empty()).unwrap_or_else(|| "Komando".into()).chars().take(15).collect()
}

/// Kerberos would need to reach a domain controller; NTLM covers local and domain accounts.
struct NoKerberos;

impl ironrdp_tokio::NetworkClient for NoKerberos {
    async fn send(&mut self, _: &NetworkRequest) -> ConnectorResult<Vec<u8>> {
        Err(ConnectorError::new("Kerberos", ConnectorErrorKind::Reason("Kerberos sign-in is not supported".into())))
    }
}

/// The connected desktop: frames in, input out, until either side ends it.
async fn active(framed: Framed, result: ConnectionResult, mut clipboard: Clipboard, link: &mut Link) -> Result<String> {
    let (mut reader, mut writer) = split_tokio_framed(framed);
    super::check_size(result.desktop_size.width.into(), result.desktop_size.height.into())?;
    let mut image = DecodedImage::new(PixelFormat::BgrA32, result.desktop_size.width, result.desktop_size.height);
    link.draw(|p| p.resize(image.width().into(), image.height().into()))?;
    let activation = result.activation_factory;
    let mut stage = ActiveStageBuilder {
        static_channels: result.static_channels,
        user_channel_id: result.user_channel_id,
        io_channel_id: result.io_channel_id,
        message_channel_id: result.message_channel_id,
        share_id: result.share_id,
        compression_type: result.compression_type,
        enable_server_pointer: result.enable_server_pointer,
        pointer_software_rendering: result.pointer_software_rendering,
    }
    .build();
    let mut input = Database::new();
    let mut closing = false;
    // Once closing, how long the server has to end the session before it is ended here.
    let mut deadline = None;

    loop {
        let outputs = tokio::select! {
            _ = async { tokio::time::sleep_until(deadline.expect("a deadline")).await }, if deadline.is_some() => {
                return Ok(CLOSED_BY_USER.into());
            }
            frame = reader.read_pdu() => {
                let (action, payload) = match frame {
                    Ok(frame) => frame,
                    Err(_) if closing => return Ok(CLOSED_BY_USER.into()),
                    // The server hung up (its own disconnect, a restart, the network).
                    Err(err) if err.kind() == std::io::ErrorKind::UnexpectedEof => return Ok("The server closed the connection.".into()),
                    Err(err) => return Ok(format!("The connection ended: {err}")),
                };
                let mut outputs = stage.process(&mut image, action, &payload, reader.last_read_at()).map_err(|e| anyhow!("{e}"))?;
                // What the clipboard channel noted meanwhile is answered after the server's
                // own replies, so a paste request follows the format list's acknowledgement.
                outputs.extend(on_clipboard(&mut stage, |c| clipboard.drain(c, |text| link.send(DesktopEvent::Clipboard(text)))));
                outputs
            }
            event = link.input.recv() => match event {
                None => return Ok(CLOSED_BY_USER.into()),
                Some(Input::Close) if closing => vec![],
                Some(Input::Close) => {
                    // Let the server end it; if it doesn't answer soon, end it here.
                    closing = true;
                    deadline = Some(tokio::time::Instant::now() + std::time::Duration::from_secs(2));
                    stage.graceful_shutdown().map_err(|e| anyhow!("{e}"))?
                }
                Some(Input::Clipboard(text)) => on_clipboard(&mut stage, |c| clipboard.offer(c, text)),
                Some(Input::ShareClipboard(on)) => on_clipboard(&mut stage, |c| clipboard.share(c, on)),
                Some(Input::ReleaseAll) => {
                    let events = input.release_all();
                    stage.process_fastpath_input(&mut image, &events).map_err(|e| anyhow!("{e}"))?
                }
                Some(Input::Resize { width, height, scale_percent }) => {
                    let (width, height) = MonitorLayoutEntry::adjust_display_size(width, height);
                    match stage.encode_resize(width, height, Some(scale_percent), None) {
                        Some(frame) => vec![ActiveStageOutput::ResponseFrame(frame.map_err(|e| anyhow!("{e}"))?)],
                        // The server can't change size while connected: keep scaling.
                        None => vec![],
                    }
                }
                Some(event) => {
                    let events = input.apply(operations(event));
                    stage.process_fastpath_input(&mut image, &events).map_err(|e| anyhow!("{e}"))?
                }
            },
        };

        for output in outputs {
            match output {
                ActiveStageOutput::ResponseFrame(frame) => {
                    if let Err(err) = writer.write_all(&frame).await {
                        return Ok(if closing { CLOSED_BY_USER.into() } else { format!("The connection ended: {err}") });
                    }
                }
                ActiveStageOutput::GraphicsUpdate(rect) => link.draw(|p| copy_rect(p, &image, &rect))?,
                ActiveStageOutput::DeactivateAll => {
                    // The server changes the desktop (a new size): run the activation again.
                    let mut sequence = activation.create();
                    let mut buf = WriteBuf::new();
                    loop {
                        let written = single_sequence_step_read(&mut reader, &mut sequence, &mut buf).await.map_err(|e| anyhow!("{e}"))?;
                        if written.size().is_some() {
                            writer.write_all(buf.filled()).await?;
                        }
                        if let ConnectionActivationState::Finalized {
                            desktop_size,
                            share_id,
                            static_channel_chunk_size,
                            enable_server_pointer,
                            pointer_software_rendering,
                            ..
                        } = sequence.connection_activation_state()
                        {
                            super::check_size(desktop_size.width.into(), desktop_size.height.into())?;
                            image = DecodedImage::new(PixelFormat::BgrA32, desktop_size.width, desktop_size.height);
                            link.draw(|p| p.resize(desktop_size.width.into(), desktop_size.height.into()))?;
                            let (io, user) = (sequence.io_channel_id(), sequence.user_channel_id());
                            if !stage.reactivate(
                                io,
                                user,
                                share_id,
                                enable_server_pointer,
                                pointer_software_rendering,
                                static_channel_chunk_size,
                            ) {
                                return Err(anyhow!("The server sent settings Komando can't use after changing the desktop."));
                            }
                            break;
                        }
                    }
                }
                ActiveStageOutput::Terminate(reason) => {
                    return Ok(match reason {
                        ironrdp_session::GracefulDisconnectReason::UserInitiated => CLOSED_BY_USER.into(),
                        ironrdp_session::GracefulDisconnectReason::ServerInitiated => "The server ended the session.".into(),
                        other => format!("The server ended the session: {}.", other.description()),
                    });
                }
                // Pointer shapes: Windows' own arrow is shown instead.
                _ => {}
            }
        }
    }
}

/// Run a step on the connection's clipboard channel, and what it sends as a frame to write.
fn on_clipboard(stage: &mut ActiveStage, step: impl FnOnce(&mut CliprdrClient) -> Vec<ironrdp_svc::SvcMessage>) -> Vec<ActiveStageOutput> {
    let Some(channel) = stage.get_svc_processor_mut::<CliprdrClient>() else { return vec![] };
    let messages = step(channel);
    if messages.is_empty() {
        return vec![];
    }
    match stage.process_svc_processor_messages(ironrdp_svc::SvcProcessorMessages::<CliprdrClient>::new(messages)) {
        Ok(frame) => vec![ActiveStageOutput::ResponseFrame(frame)],
        // The clipboard is a convenience: a failure there doesn't end the desktop.
        Err(err) => {
            log::warn!("remote desktop clipboard: {err}");
            vec![]
        }
    }
}

/// Copy what the server redrew into the shared picture.
fn copy_rect(picture: &mut Picture, image: &DecodedImage, rect: &InclusiveRectangle) -> Result<()> {
    let (width, height) = (u32::from(image.width()), u32::from(image.height()));
    if (picture.width, picture.height) != (width, height) {
        picture.resize(width, height)?;
        picture.put(0, 0, width, height, image.data(), image.stride());
        return Ok(());
    }
    let (left, top) = (u32::from(rect.left), u32::from(rect.top));
    let right = u32::from(rect.right).min(width.saturating_sub(1));
    let bottom = u32::from(rect.bottom).min(height.saturating_sub(1));
    if left > right || top > bottom {
        return Ok(());
    }
    let stride = image.stride();
    let start = top as usize * stride + left as usize * 4;
    picture.put(left, top, right - left + 1, bottom - top + 1, &image.data()[start..], stride);
    Ok(())
}

/// The view's input as IronRDP operations.
fn operations(event: Input) -> Vec<Operation> {
    let scancode = |code: keys::Code| Scancode::from_u16(code.scancode);
    match event {
        Input::Key { key: Key::Char(c), down: true } => vec![Operation::UnicodeKeyPressed(c), Operation::UnicodeKeyReleased(c)],
        Input::Key { key: Key::Char(_), down: false } => vec![],
        Input::Key { key: Key::Code(code), down } => {
            vec![if down { Operation::KeyPressed(scancode(code)) } else { Operation::KeyReleased(scancode(code)) }]
        }
        Input::Pointer { x, y } => vec![Operation::MouseMove(MousePosition { x, y })],
        Input::Button { button, down } => {
            let button = match button {
                Button::Left => MouseButton::Left,
                Button::Middle => MouseButton::Middle,
                Button::Right => MouseButton::Right,
            };
            vec![if down { Operation::MouseButtonPressed(button) } else { Operation::MouseButtonReleased(button) }]
        }
        Input::Wheel { vertical, amount } => {
            vec![Operation::WheelRotations(WheelRotations { is_vertical: vertical, rotation_units: amount })]
        }
        // Handled by the caller.
        Input::ReleaseAll | Input::Resize { .. } | Input::Clipboard(_) | Input::ShareClipboard(_) | Input::Close => vec![],
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use base64::Engine as _;
    use tokio_rustls::rustls;
    use tokio_rustls::rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
    use tokio_rustls::rustls::server::{ClientHello, ResolvesServerCert};
    use tokio_rustls::rustls::sign::CertifiedKey;

    /// A self-signed P-256 certificate for 127.0.0.1 ("komando-test-rdp") and its key, made with
    /// openssl for these tests only.
    const CERT: &str = "MIIBnjCCAUSgAwIBAgIURVnQBMbkgMugkTEVEHPlAItOPRQwCgYIKoZIzj0EAwIwGzEZMBcGA1UEAwwQa29tYW5kby10ZXN0LXJkcDAgFw0yNjExMjcwOTI1MzNaGA8yMTI2MTEwMzA5MjUzM1owGzEZMBcGA1UEAwwQa29tYW5kby10ZXN0LXJkcDBZMBMGByqGSM49AgEGCCqGSM49AwEHA0IABG9GqVvoXfmGC3l6Qye09r1dfBWLzlFLK8IRFcz9soD+dyrHSLdhB4vTojtaSYvyTKPlCK8BWwNGiolKaIJyZi6jZDBiMB0GA1UdDgQWBBT1T9DYhXsaVtAMNfMcyCViR8McgDAfBgNVHSMEGDAWgBT1T9DYhXsaVtAMNfMcyCViR8McgDAPBgNVHRMBAf8EBTADAQH/MA8GA1UdEQQIMAaHBH8AAAEwCgYIKoZIzj0EAwIDSAAwRQIgPtA74a+fapc1YxAHp6QY9JnMj/yeeJrC35w6cScOIvsCIQC8MDz0SbPnhs0qC/PuJbIJdJ1zDnIOwzqD5YQcj9jjgg==";
    const CERT_KEY: &str = "MIGHAgEAMBMGByqGSM49AgEGCCqGSM49AwEHBG0wawIBAQQgIQ02IxN9tpZ11rfaySj0Ve8a1zziW+H8jD1sTHkO9SOhRANCAARvRqlb6F35hgt5ekMntPa9XXwVi85RSyvCERXM/bKA/ncqx0i3YQeL06I7WkmL8kyj5QivAVsDRoqJSmiCcmYu";
    /// Another P-256 key, which the certificate is not for.
    const OTHER_KEY: &str = "MIGHAgEAMBMGByqGSM49AgEGCCqGSM49AwEHBG0wawIBAQQgXPd+z6/0DnFeJHovESe6x56v4USmmksdv8nyS56JxJ+hRANCAAQOT1bL5/u5LRwCTaN5CkEL4flI+MDkgKkVxIFvlhsWKNUhUBeaGW19kADLS+LGvpZ6hnf+/DnSp2vzGebTjleQ";

    fn decode(b64: &str) -> Vec<u8> {
        base64::engine::general_purpose::STANDARD.decode(b64).unwrap()
    }

    /// Presents `CERT` whatever key it signs the handshake with: an impostor that copied the
    /// real server's certificate. (rustls's own setup would refuse a key that doesn't match.)
    #[derive(Debug)]
    struct Presents(Arc<CertifiedKey>);

    impl ResolvesServerCert for Presents {
        fn resolve(&self, _: ClientHello<'_>) -> Option<Arc<CertifiedKey>> {
            Some(self.0.clone())
        }
    }

    /// Run one handshake against a local server that presents `CERT` and signs with `key`, using
    /// only `version`. Returns the certificate Komando saw, or why the handshake failed.
    async fn handshake(key: &str, version: &'static rustls::SupportedProtocolVersion) -> anyhow::Result<Vec<u8>> {
        let provider = Arc::new(rustls::crypto::ring::default_provider());
        let key = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(decode(key)));
        let signer = provider.key_provider.load_private_key(key).unwrap();
        let certified = CertifiedKey::new(vec![CertificateDer::from(decode(CERT))], signer);
        let config = rustls::ServerConfig::builder_with_provider(provider)
            .with_protocol_versions(&[version])
            .unwrap()
            .with_no_client_auth()
            .with_cert_resolver(Arc::new(Presents(Arc::new(certified))));
        let acceptor = tokio_rustls::TlsAcceptor::from(Arc::new(config));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let _ = acceptor.accept(stream).await;
        });
        let stream = tokio::net::TcpStream::connect(address).await.unwrap();
        let result = super::upgrade_tls(stream, "127.0.0.1").await.map(|(_, der, _)| der);
        server.abort();
        result
    }

    /// A self-signed certificate is accepted here (the pin is checked after) when the server
    /// holds its key.
    #[tokio::test]
    async fn tls_accepts_a_self_signed_certificate_from_its_owner() {
        for version in [&rustls::version::TLS13, &rustls::version::TLS12] {
            let der = handshake(CERT_KEY, version).await.unwrap_or_else(|err| panic!("{version:?}: {err:#}"));
            assert_eq!(der, decode(CERT));
        }
    }

    /// Security regression: a server that presents the pinned certificate without its private
    /// key (a man in the middle who copied it) fails the handshake, before the certificate
    /// pin or any credentials are reached.
    #[tokio::test]
    async fn tls_refuses_a_copied_certificate_without_its_key() {
        for version in [&rustls::version::TLS13, &rustls::version::TLS12] {
            let err = handshake(OTHER_KEY, version).await.expect_err("an impostor's handshake must fail");
            assert!(format!("{err:#}").contains("TLS with the server failed"), "{version:?}: {err:#}");
        }
    }
}
