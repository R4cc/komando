//! Development demo: seeds a throwaway data directory and opens a few sessions.
//! Used to take the README screenshot against a local test SSH server.
//!
//!   KOMANDO_DEMO_DIR=/tmp/komando-demo \
//!   KOMANDO_DEMO_KEY=./client_ed25519 KOMANDO_DEMO_HOST_KEY=./host_ed25519.pub \
//!   KOMANDO_DEMO_PORT=2222 KOMANDO_DEMO_USER=$USER cargo run --example demo

use std::env;
use std::fs;
use std::path::PathBuf;

use komando::model::{AppThemeSetting, Host, Library, Project, Settings};
use komando::ui::workspace::{SettingsSection, Startup};
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
        .add(Credential { label: "Atlas deploy key".into(), secret: Secret::PrivateKey { key, passphrase: None } })
        .unwrap();
    let key_id_for_stress = key_id.clone();

    let host = |name: &str, address: &str, port: u16, icon: &str| Host {
        port,
        icon: Some(icon.into()),
        ..Host::new(name, address)
    };
    let web01 = host("web01", "127.0.0.1", port, "server");
    let web02 = host("web02", "10.20.1.12", 22, "server");
    let db01 = host("db01", "127.0.0.1", port, "database");
    let lb01 = host("lb01", "10.20.1.2", 22, "network");
    let stg = host("web-stg01", "10.30.1.11", 22, "server");
    let mon = host("monitoring", "10.0.0.20", 22, "cpu");
    let backup = host("backup", "10.0.0.30", 22, "hard-drive");

    let project = |name: &str, color: &str, icon: &str, hosts: &[&Host], credential: Option<String>| Project {
        default_user: Some(user.clone()),
        credential,
        hosts: hosts.iter().map(|h| h.id.clone()).collect(),
        ..Project::new(name, color, icon)
    };
    let library = Library {
        shells: None,
        projects: vec![
            project("Atlas Production", "red", "shield", &[&web01, &web02, &db01, &lb01], Some(key_id)),
            project("Atlas Staging", "amber", "box", &[&stg], None),
            project("Nova", "blue", "globe", &[&mon, &backup], None),
        ],
        hosts: vec![web01, web02, db01, lb01, stg, mon, backup],
    };
    library.save(&dir).unwrap();

    let mut settings = Settings::default();
    // Screenshots and load tests shouldn't depend on GitHub.
    settings.check_for_updates = false;
    settings.app_theme = if theme == "light" { AppThemeSetting::Light } else { AppThemeSetting::Dark };
    // KOMANDO_DEMO_THEMES="dark-id,light-id", e.g. "midnight,paper".
    if let Ok(themes) = std::env::var("KOMANDO_DEMO_THEMES")
        && let Some((dark, light)) = themes.split_once(',')
    {
        settings.dark_theme = dark.into();
        settings.light_theme = light.into();
    }
    settings.save(&dir).unwrap();

    let mut parts = host_key.split_whitespace();
    let line = format!("[127.0.0.1]:{port} {} {}\n", parts.next().unwrap(), parts.next().unwrap());
    fs::write(dir.join("known_hosts"), line).unwrap();

    let local_shell = komando::model::default_shells().remove(0).name;
    let mut open = vec![
        ("Atlas Production".into(), "web01".into()),
        ("Atlas Production".into(), "db01".into()),
        ("Atlas Staging".into(), "web-stg01".into()),
        (String::new(), local_shell),
    ];

    let mut stress_active = None;
    // Stress mode for profiling: KOMANDO_DEMO_STRESS="projects,hosts per project,open sessions".
    // Every host points at the local test server; with its log generator on, each open session
    // streams logs.
    if let Ok(spec) = env::var("KOMANDO_DEMO_STRESS") {
        // A fourth value "idle" shows a quiet local shell, so only background sessions stream.
        let idle = spec.ends_with(",idle");
        let spec = spec.trim_end_matches(",idle");
        let n: Vec<usize> = spec.split(',').map(|v| v.trim().parse().unwrap()).collect();
        let (projects, per_project, sessions) = (n[0], n[1], n[2]);
        let colors = ["red", "amber", "blue", "green", "violet", "cyan", "pink", "orange"];
        let icons = ["server", "database", "globe", "cpu", "network", "cloud", "router", "hard-drive"];
        let mut library = Library::load(&dir).unwrap();
        library.projects.clear();
        library.hosts.clear();
        open.clear();
        for pi in 0..projects {
            let mut hosts = vec![];
            for hi in 0..per_project {
                let h = host(&format!("p{pi:02}-node{hi:02}"), "127.0.0.1", port, icons[hi % icons.len()]);
                hosts.push(h.id.clone());
                if open.len() < sessions && hi < sessions.div_ceil(projects).max(1) {
                    open.push((format!("Project {pi:02}"), h.name.clone()));
                }
                library.hosts.push(h);
            }
            library.projects.push(Project {
                default_user: Some(user.clone()),
                credential: Some(key_id_for_stress.clone()),
                hosts,
                ..Project::new(format!("Project {pi:02}"), colors[pi % colors.len()], icons[pi % icons.len()])
            });
        }
        library.save(&dir).unwrap();
        if idle {
            open.push((String::new(), komando::model::default_shells().remove(0).name));
            stress_active = Some(open.len() - 1);
        }
    }

    komando::run(komando::Options {
        data_dir: dir,
        skip_unlock: true,
        startup: Startup {
            master_password: Some(MASTER.into()),
            open,
            active: stress_active.or(Some(0)),
            settings: env::var("KOMANDO_DEMO_SETTINGS").ok().and_then(|s| {
                SettingsSection::ALL.into_iter().find(|section| format!("{section:?}").eq_ignore_ascii_case(&s))
            }),
        },
    });
}
