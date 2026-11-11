//! Development demo: seeds a throwaway data directory and opens a few sessions.
//! Used to take the README screenshot against a local test SSH server.
//!
//!   KOMANDO_DEMO_DIR=/tmp/komando-demo \
//!   KOMANDO_DEMO_KEY=./client_ed25519 KOMANDO_DEMO_HOST_KEY=./host_ed25519.pub \
//!   KOMANDO_DEMO_PORT=2222 KOMANDO_DEMO_USER=$USER cargo run --example demo

use std::env;
use std::fs;
use std::path::PathBuf;

use komando::model::{AppThemeSetting, Host, Library, Project, Settings, new_id};
use komando::ui::workspace::Startup;
use komando::vault::{Credential, Secret, Vault};

const MASTER: &str = "demo-master-password";

fn main() {
    let dir = PathBuf::from(env::var("KOMANDO_DEMO_DIR").expect("KOMANDO_DEMO_DIR"));
    let key = fs::read_to_string(env::var("KOMANDO_DEMO_KEY").expect("KOMANDO_DEMO_KEY")).unwrap();
    let host_key = fs::read_to_string(env::var("KOMANDO_DEMO_HOST_KEY").expect("KOMANDO_DEMO_HOST_KEY")).unwrap();
    let port: u16 = env::var("KOMANDO_DEMO_PORT").map(|p| p.parse().unwrap()).unwrap_or(2222);
    let user = env::var("KOMANDO_DEMO_USER").unwrap_or_else(|_| "ops".into());
    let theme = env::var("KOMANDO_DEMO_THEME").unwrap_or_else(|_| "dark".into());

    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();

    let mut vault = Vault::create(&dir, MASTER).unwrap();
    let key_id = vault
        .add(Credential { label: "Customer A deploy key".into(), secret: Secret::PrivateKey { key, passphrase: None } })
        .unwrap();

    let host = |name: &str, address: &str, port: u16, icon: &str| Host {
        id: new_id(),
        name: name.into(),
        address: address.into(),
        port,
        user: None,
        credential: None,
        color: None,
        icon: Some(icon.into()),
    };
    let web01 = host("web01", "127.0.0.1", port, "server");
    let web02 = host("web02", "10.20.1.12", 22, "server");
    let db01 = host("db01", "127.0.0.1", port, "database");
    let lb01 = host("lb01", "10.20.1.2", 22, "network");
    let stg = host("web-stg01", "10.30.1.11", 22, "server");
    let mon = host("monitoring", "10.0.0.20", 22, "cpu");
    let backup = host("backup", "10.0.0.30", 22, "hard-drive");

    let project = |name: &str, color: &str, icon: &str, hosts: &[&Host], credential: Option<String>| Project {
        id: new_id(),
        name: name.into(),
        color: color.into(),
        icon: icon.into(),
        default_user: Some(user.clone()),
        credential,
        hosts: hosts.iter().map(|h| h.id.clone()).collect(),
        shells: vec![],
    };
    let library = Library {
        projects: vec![
            project("Customer A – Prod", "red", "shield", &[&web01, &web02, &db01, &lb01], Some(key_id)),
            project("Customer A – Staging", "amber", "box", &[&stg], None),
            project("Internal", "blue", "globe", &[&mon, &backup], None),
            project("Local", "slate", "square-terminal", &[], None),
        ],
        hosts: vec![web01, web02, db01, lb01, stg, mon, backup],
    };
    library.save(&dir).unwrap();

    let mut settings = Settings::default();
    settings.app_theme = if theme == "light" { AppThemeSetting::Light } else { AppThemeSetting::Dark };
    settings.save(&dir).unwrap();

    let mut parts = host_key.split_whitespace();
    let line = format!("[127.0.0.1]:{port} {} {}\n", parts.next().unwrap(), parts.next().unwrap());
    fs::write(dir.join("known_hosts"), line).unwrap();

    let local_shell = komando::model::default_shells().remove(0).name;
    komando::run(komando::Options {
        data_dir: dir,
        skip_unlock: true,
        startup: Startup {
            master_password: Some(MASTER.into()),
            open: vec![
                ("Customer A – Prod".into(), "web01".into()),
                ("Customer A – Prod".into(), "db01".into()),
                ("Customer A – Staging".into(), "web-stg01".into()),
                ("Local".into(), local_shell),
            ],
            active: Some(0),
            show_highlights: env::var("KOMANDO_DEMO_HIGHLIGHTS").is_ok(),
        },
    });
}
