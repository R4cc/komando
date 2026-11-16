//! RDP through IronRDP: TLS with the certificate checked like a Host Key, then Network Level
//! Authentication (CredSSP with NTLM) or, for servers without it, TLS sign-in.
//!
//! Komando advertises only what a plain desktop needs: the display, keyboard and mouse, and
//! the Display Control channel to follow the pane's size. No sound, drives, printers or
//! clipboard are redirected.

use anyhow::{Result, anyhow};
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
use ironrdp_session::{ActiveStageBuilder, ActiveStageOutput};
use ironrdp_tokio::{FramedWrite as _, TokioFramed, single_sequence_step_read, split_tokio_framed};
use zeroize::Zeroizing;

use super::keys::{self, Key};
use super::trust::{self, Known};
use super::{Button, Input, Link, Picture, Quality, Status, Stream, Target, split_domain};

type Framed = TokioFramed<Stream>;

/// Sign-in attempts before giving up, so a server that keeps refusing doesn't ask forever.
const ATTEMPTS: usize = 5;

pub(super) async fn run(target: Target, link: &mut Link) -> Result<String> {
    let mut user = target.user.clone().filter(|u| !u.is_empty());
    let mut password = target.password.clone();
    let mut error = None;
    for _ in 0..ATTEMPTS {
        let mut remember = false;
        if user.is_none() || password.is_none() || error.is_some() {
            let Some(answer) = link.ask_sign_in(user.clone(), true, error.take()).await else {
                return Ok("Sign-in cancelled.".into());
            };
            user = answer.user.filter(|u| !u.is_empty());
            password = Some(answer.password);
            remember = answer.remember;
        }
        let (Some(name), Some(secret)) = (user.clone(), password.clone()) else { continue };
        match connect(&target, link, &name, &secret).await {
            Ok((framed, result)) => {
                if remember {
                    link.send(super::DesktopEvent::Remember { user: Some(name), password: secret });
                }
                link.status(Status::Connected);
                return active(framed, result, link).await;
            }
            Err(Failure::SignIn(message)) => error = Some(message),
            Err(Failure::Other(err)) => return Err(err),
        }
    }
    Ok("Signing in failed too many times.".into())
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

async fn connect(target: &Target, link: &Link, user: &str, password: &Zeroizing<String>) -> Result<(Framed, ConnectionResult), Failure> {
    let stream = target.route.open().await?;
    let mut framed = Framed::new(stream);
    let (username, domain) = split_domain(user, target.domain.as_deref());
    let config = config(target, username, password.to_string(), domain);
    let channels = DrdynvcClient::new().with_dynamic_channel(DisplayControlClient::new(|_| Ok(Vec::new())));
    let mut connector = ClientConnector::new(config, target.route.local_address()).with_static_channel(channels);

    let should_upgrade = ironrdp_tokio::connect_begin(&mut framed, &mut connector).await.map_err(failure)?;
    let (stream, leftover) = framed.into_inner();
    let (tls, certificate) =
        ironrdp_tls::upgrade(stream, &target.address).await.map_err(|err| anyhow!("TLS with the server failed: {err}"))?;
    let der = tls.get_ref().1.peer_certificates().and_then(|c| c.first()).map(|c| c.to_vec());
    let der = der.ok_or_else(|| anyhow!("The server sent no certificate."))?;
    check_certificate(target, link, &der).await?;
    let public_key = ironrdp_tls::extract_tls_server_public_key(&certificate)
        .ok_or_else(|| anyhow!("The server's certificate has no public key Komando can read."))?
        .to_vec();

    let upgraded = ironrdp_tokio::mark_as_upgraded(should_upgrade, &mut connector);
    let mut framed = Framed::new_with_leftover(Box::new(tls) as Stream, leftover);
    let result = ironrdp_tokio::connect_finalize(
        upgraded,
        connector,
        &mut framed,
        &mut NoKerberos,
        ServerName::new(target.address.clone()),
        public_key,
        None,
    )
    .await
    .map_err(failure)?
    .into_connected()
    .map_err(|_| anyhow!("The server redirected the connection to another server, which Komando doesn't follow."))?;
    Ok((framed, result))
}

/// Trusted on first use, like a Host Key: most RDP servers sign their own certificate.
async fn check_certificate(target: &Target, link: &Link, der: &[u8]) -> Result<()> {
    let server = format!("{}:{}", target.address, target.port);
    let fingerprint = trust::fingerprint(der);
    let previous = match trust::check(&target.certificates, &server, &fingerprint) {
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
async fn active(framed: Framed, result: ConnectionResult, link: &mut Link) -> Result<String> {
    let (mut reader, mut writer) = split_tokio_framed(framed);
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
                return Ok("Session closed.".into());
            }
            frame = reader.read_pdu() => {
                let (action, payload) = match frame {
                    Ok(frame) => frame,
                    Err(_) if closing => return Ok("Session closed.".into()),
                    Err(err) => return Ok(format!("The connection ended: {err}")),
                };
                stage.process(&mut image, action, &payload, reader.last_read_at()).map_err(|e| anyhow!("{e}"))?
            }
            event = link.input.recv() => match event {
                None => return Ok("Session closed.".into()),
                Some(Input::Close) if closing => vec![],
                Some(Input::Close) => {
                    // Let the server end it; if it doesn't answer soon, end it here.
                    closing = true;
                    deadline = Some(tokio::time::Instant::now() + std::time::Duration::from_secs(2));
                    stage.graceful_shutdown().map_err(|e| anyhow!("{e}"))?
                }
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
                        return Ok(if closing { "Session closed.".into() } else { format!("The connection ended: {err}") });
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
                        ironrdp_session::GracefulDisconnectReason::UserInitiated => "Session closed.".into(),
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
        Input::CtrlAltDel => {
            let keys = [keys::CONTROL, keys::ALT, keys::DELETE].map(scancode);
            keys.iter().map(|k| Operation::KeyPressed(*k)).chain(keys.iter().rev().map(|k| Operation::KeyReleased(*k))).collect()
        }
        // Handled by the caller.
        Input::ReleaseAll | Input::Resize { .. } | Input::Close => vec![],
    }
}
