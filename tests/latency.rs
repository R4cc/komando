//! Exercise the indicator's events through real SSH, including a tunnel through a Jump
//! Host. The fixture replies REQUEST_FAILURE to keepalives, as network appliances can;
//! that is still a valid round trip. No external server or credentials are used.

use std::time::{Duration, Instant};

use komando::session::latency::{HopLatency, ReplyState};
use komando::session::ssh::{self, Hop, Target};
use komando::session::{EventProxy, GridSize, Prompt, SessionEvent, Status, new_term};
use russh::server::{self, Msg, Session};
use russh::{Channel, ChannelId, ChannelMsg};

#[path = "support/ssh_server.rs"]
mod ssh_server;

#[derive(Clone)]
struct Loopback;

impl server::Server for Loopback {
    type Handler = Self;

    fn new_client(&mut self, _: Option<std::net::SocketAddr>) -> Self {
        Self
    }
}

impl server::Handler for Loopback {
    type Error = russh::Error;

    async fn auth_none(&mut self, _: &str) -> Result<server::Auth, Self::Error> {
        Ok(server::Auth::Accept)
    }

    async fn channel_open_session(
        &mut self,
        mut channel: Channel<Msg>,
        reply: server::ChannelOpenHandle,
        _: &mut Session,
    ) -> Result<(), Self::Error> {
        reply.accept().await;
        tokio::spawn(async move {
            while let Some(message) = channel.wait().await {
                match message {
                    ChannelMsg::Data { data } => {
                        let _ = channel.data(&data[..]).await;
                    }
                    ChannelMsg::Close => break,
                    _ => {}
                }
            }
        });
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
        assert_eq!(host, "127.0.0.1");
        let mut stream = tokio::net::TcpStream::connect((host, port as u16)).await?;
        reply.accept().await;
        tokio::spawn(async move {
            let _ = tokio::io::copy_bidirectional(&mut channel.into_stream(), &mut stream).await;
        });
        Ok(())
    }
}

fn next(events: &mut futures::channel::mpsc::UnboundedReceiver<SessionEvent>) -> SessionEvent {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        assert!(Instant::now() < deadline, "timed out waiting for an SSH event");
        match events.try_recv() {
            Ok(event) => return event,
            Err(_) => std::thread::sleep(Duration::from_millis(5)),
        }
    }
}

#[test]
fn ssh_latency_is_reported_directly_and_through_a_jump_host() {
    let (port, _runtime) = ssh_server::start(Loopback, ssh_server::config(17), 2);

    for names in [&["target"][..], &["jump", "target"][..]] {
        let dir = std::env::temp_dir().join(format!("komando-latency-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&dir).unwrap();
        let size = GridSize { cols: 80, rows: 24, cell_width: 9, cell_height: 18 };
        let (tx, mut events) = futures::channel::mpsc::unbounded();
        let proxy = EventProxy::new(tx);
        proxy.set_window_active(true);
        let term = new_term(size, 100, proxy.clone());
        let hops = names
            .iter()
            .map(|name| Hop {
                name: (*name).into(),
                address: "127.0.0.1".into(),
                port,
                user: Some("fixture".into()),
                secret: None,
            })
            .collect();
        let sink = ssh::spawn(Target { hops, known_hosts: dir.join("known_hosts"), forwards: vec![], ..Default::default() }, size, term.clone(), proxy);
        let mut samples: Vec<Option<HopLatency>> = vec![None; names.len()];
        let mut connected = false;
        while !connected || samples.iter().any(Option::is_none) {
            match next(&mut events) {
                SessionEvent::Prompt(Prompt::HostKey { reply, .. }) => {
                    reply.send(true).unwrap();
                }
                SessionEvent::Prompt(_) => panic!("fixture unexpectedly asked for credentials"),
                SessionEvent::Status(Status::Connected) => connected = true,
                SessionEvent::Status(Status::Disconnected(reason)) => panic!("SSH ended before sampling: {reason}"),
                SessionEvent::Latency(sample) if sample.state == ReplyState::Responsive => {
                    let index = sample.hop;
                    assert_eq!(sample.name, names[index]);
                    assert!(sample.round_trip.is_some());
                    assert!(sample.measured_at.is_some());
                    samples[index] = Some(sample);
                }
                _ => {}
            }
        }

        // Sampling must coexist with the shell and its independent output-pacing pings.
        sink.write(b"still connected\r\n".to_vec());
        while !komando::session::scrollback_text(&*term.lock()).contains("still connected") {
            if let SessionEvent::Status(Status::Disconnected(reason)) = next(&mut events) {
                panic!("sampling interrupted the shell: {reason}");
            }
        }
        sink.close();
        while !matches!(next(&mut events), SessionEvent::Status(Status::Disconnected(_))) {}
        std::fs::remove_dir_all(dir).unwrap();
    }
}
