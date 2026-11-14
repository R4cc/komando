//! Launch the unchanged Komando library with disposable, already-unlocked SSH sessions.
//! See docs/benchmarks.md. Never point this at a real Komando data directory.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use komando::model::{Host, Library, Project, Settings};
use komando::ui::workspace::Startup;
use komando::vault::{Credential, Secret, Vault};
use std::{env, fs, path::PathBuf};

fn main() -> anyhow::Result<()> {
    let args: Vec<_> = env::args().collect();
    anyhow::ensure!(
        args.len() == 6,
        "benchmark_client <new-dir> <key> <host-public-key> <port> <sessions>"
    );
    let dir = PathBuf::from(&args[1]);
    anyhow::ensure!(
        !dir.exists(),
        "benchmark data directory must not already exist"
    );
    fs::create_dir_all(&dir)?;
    let mut vault = Vault::create(&dir, "local-benchmark-only")?;
    let credential = vault.add(Credential {
        label: "Loopback benchmark".into(),
        secret: Secret::PrivateKey {
            key: fs::read_to_string(&args[2])?,
            passphrase: None,
        },
    })?;
    let port: u16 = args[4].parse()?;
    let sessions: usize = args[5].parse()?;
    let mut project = Project::new("Benchmark", "blue", "server");
    project.default_user = Some("bench".into());
    project.credential = Some(credential);
    let mut library = Library {
        shells: Some(vec![]),
        ..Default::default()
    };
    let mut open = vec![];
    for i in 0..sessions {
        let mut host = Host::new(format!("Session {}", i + 1), "127.0.0.1");
        host.port = port;
        project.hosts.push(host.id.clone());
        open.push((project.name.clone(), host.name.clone()));
        library.hosts.push(host);
    }
    library.projects.push(project);
    library.save(&dir)?;
    Settings {
        check_for_updates: false,
        scrollback_lines: 100_000,
        ..Default::default()
    }
    .save(&dir)?;
    let host_key = fs::read_to_string(&args[3])?;
    fs::write(
        dir.join("known_hosts"),
        format!("[127.0.0.1]:{port} {}\n", host_key.trim()),
    )?;
    komando::run(komando::Options {
        data_dir: dir,
        startup: Startup {
            master_password: Some("local-benchmark-only".into()),
            open,
            active: Some(0),
            ..Default::default()
        },
    });
    Ok(())
}
