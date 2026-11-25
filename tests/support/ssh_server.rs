//! An in-process SSH server on 127.0.0.1 for the tests, on a runtime of its own like the
//! separate program a real server is.
#![allow(dead_code)]

use std::sync::Arc;
use std::time::Duration;

use russh::keys::PrivateKey;
use russh::keys::ssh_key::private::Ed25519Keypair;
use russh::server::{self, Server};

/// An Ed25519 key made from `seed`, so a test's keys are the same on every run.
pub fn key(seed: u8) -> PrivateKey {
    PrivateKey::from(Ed25519Keypair::from_seed(&[seed; 32]))
}

/// A server config with the host key made from `seed`, which turns down a failed sign-in at once
/// rather than after russh's delay, so tests that try wrong answers don't wait.
pub fn config(seed: u8) -> server::Config {
    server::Config {
        keys: vec![key(seed)],
        auth_rejection_time: Duration::ZERO,
        auth_rejection_time_initial: Some(Duration::ZERO),
        ..Default::default()
    }
}

/// [`config`] for the benchmark servers, which send a lot: an idle connection never times out,
/// and small writes such as a typed key's echo go out at once.
pub fn benchmark_config() -> server::Config {
    server::Config { inactivity_timeout: None, nodelay: true, ..config(7) }
}

/// A multi-thread runtime with `threads` workers.
pub fn runtime(threads: usize) -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_multi_thread().worker_threads(threads).enable_all().build().unwrap()
}

/// Serves `server` with `config` on a free port on `runtime`, and returns the port.
pub fn serve<S: Server + Send + 'static>(runtime: &tokio::runtime::Runtime, mut server: S, config: server::Config) -> u16 {
    let config = Arc::new(config);
    let listener = runtime.block_on(tokio::net::TcpListener::bind("127.0.0.1:0")).unwrap();
    let port = listener.local_addr().unwrap().port();
    runtime.spawn(async move {
        let _ = server.run_on_socket(config, &listener).await;
    });
    port
}

/// Serves `server` with `config` on a runtime of its own with `threads` workers, and returns the
/// port and the runtime, which stops the server when dropped.
pub fn start<S: Server + Send + 'static>(server: S, config: server::Config, threads: usize) -> (u16, tokio::runtime::Runtime) {
    let runtime = runtime(threads);
    let port = serve(&runtime, server, config);
    (port, runtime)
}
