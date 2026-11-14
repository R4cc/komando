//! Sharing a Project: a Project File with its Hosts and how to reach them, without secrets.
//!
//! `write` turns a Project into the file's text; `read` turns a file into the same `Found`
//! list the Tabby import makes, so Settings → Import picks, reviews and adds it the same way
//! (see docs/adr/0009-sharing-a-project.md).
//!
//! The file's types are Komando's own, not the Library's: they have no field a Credential
//! could go into, so a password or key can't end up in a shared file by mistake.

use anyhow::{Context as _, Result, bail};
use serde::{Deserialize, Serialize};

use crate::import::{Found, FoundGroup, FoundHost};
use crate::model::{Host, Inherited, JumpHost, Library, Project, StartupCommand, resolve};

/// The file name extension of a Project File.
pub const EXTENSION: &str = "komando";

/// The newest file layout this version writes and reads.
const FORMAT: u32 = 1;

const HEADER: &str = "# A Komando project, made with \"Share project…\".\n\
# Import it in Komando under Settings → Import. It holds no passwords or keys.\n\n";

#[derive(Debug, Serialize, Deserialize)]
struct File {
    /// The file's layout; what tells a Project File from other files.
    komando_project: u32,
    project: SharedProject,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    hosts: Vec<SharedHost>,
}

#[derive(Debug, Serialize, Deserialize)]
struct SharedProject {
    name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    color: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    icon: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    default_user: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    startup_commands: Vec<StartupCommand>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    jump_hosts: Vec<SharedJump>,
}

#[derive(Debug, Serialize, Deserialize)]
struct SharedHost {
    name: String,
    address: String,
    #[serde(default = "default_port")]
    port: u16,
    /// Left out: the Project's default user. `false`: asked when connecting.
    #[serde(default, skip_serializing_if = "Inherited::is_project")]
    user: Inherited<String>,
    #[serde(default = "yes", skip_serializing_if = "is_true")]
    use_project_jump_hosts: bool,
    #[serde(default = "yes", skip_serializing_if = "is_true")]
    run_project_commands: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    startup_commands: Vec<StartupCommand>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    color: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    icon: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    jump_hosts: Vec<SharedJump>,
}

/// A Jump Host without its password: asked when connecting, like any Jump Host without one.
#[derive(Debug, Serialize, Deserialize)]
struct SharedJump {
    address: String,
    #[serde(default = "default_port")]
    port: u16,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    user: Option<String>,
}

impl SharedJump {
    fn from_jump(jump: &JumpHost) -> Self {
        Self { address: jump.address.clone(), port: jump.port, user: jump.user.clone() }
    }

    fn to_jump(&self) -> JumpHost {
        JumpHost { address: self.address.trim().to_string(), port: self.port, user: self.user.clone().filter(|u| !u.is_empty()), credential: None }
    }
}

fn default_port() -> u16 {
    22
}

fn yes() -> bool {
    true
}

fn is_true(value: &bool) -> bool {
    *value
}

/// Startup Commands without any that answer a password question: their command would be a
/// password. Forms refuse those, but a hand-edited `library.toml` or file may have one.
fn shareable(commands: &[StartupCommand]) -> Vec<StartupCommand> {
    commands.iter().filter(|c| !c.answers_password()).cloned().collect()
}

/// What a Project File of `project` holds: the Project's name, look and defaults, and its
/// Hosts with their addresses, users, Jump Hosts and Startup Commands. No Credential, and no
/// Local Shells (they are this PC's programs). None if the Project isn't in the Library.
pub fn write(library: &Library, project_id: &str) -> Option<String> {
    let project = library.project(project_id)?;
    let file = File {
        komando_project: FORMAT,
        project: SharedProject {
            name: project.name.clone(),
            color: Some(project.color.clone()),
            icon: Some(project.icon.clone()),
            default_user: project.default_user.clone(),
            startup_commands: shareable(&project.startup_commands),
            jump_hosts: project.jump_hosts.iter().map(SharedJump::from_jump).collect(),
        },
        hosts: library
            .hosts_of(project)
            .map(|host| SharedHost {
                name: host.name.clone(),
                address: host.address.clone(),
                port: host.port,
                user: host.user.clone(),
                use_project_jump_hosts: host.use_project_jump_hosts,
                run_project_commands: host.run_project_commands,
                startup_commands: shareable(&host.startup_commands),
                color: host.color.clone(),
                icon: host.icon.clone(),
                jump_hosts: host.jump_hosts.iter().map(SharedJump::from_jump).collect(),
            })
            .collect(),
    };
    let body = toml::to_string_pretty(&file).ok()?;
    Some(format!("{HEADER}{body}"))
}

/// The Project in a Project File, as one group to import. Each Host carries the user, Jump
/// Hosts and Startup Commands it ends up with in that Project, so it reaches its machine the
/// same way whichever Project it is added to. `is_icon` says which icon names are bundled;
/// others are dropped.
pub fn read(text: &str, is_icon: &dyn Fn(&str) -> bool) -> Result<Found> {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    /// Just the layout, so a newer file is told apart before the rest is read.
    #[derive(Deserialize)]
    struct Probe {
        komando_project: Option<u32>,
    }
    match toml::from_str::<Probe>(text) {
        Ok(Probe { komando_project: Some(format) }) if format > FORMAT => {
            bail!("This project was shared from a newer Komando. Update Komando, then import it again.")
        }
        Ok(Probe { komando_project: Some(_) }) => {}
        Err(err) if text.contains("komando_project") => {
            return Err(anyhow::Error::new(err).context("This Komando project file is damaged"));
        }
        _ => bail!("This isn't a Komando project file."),
    }
    let file: File = toml::from_str(text).context("This Komando project file is damaged")?;
    let shared = &file.project;
    let name = shared.name.trim();
    if name.is_empty() {
        bail!("This Komando project file has no project name.");
    }
    let icon = |icon: &Option<String>| icon.clone().filter(|i| is_icon(i));
    let mut project = Project::new(name, shared.color.clone().unwrap_or_default(), "folder");
    project.default_user = shared.default_user.clone().filter(|u| !u.trim().is_empty());
    project.jump_hosts = shared.jump_hosts.iter().map(SharedJump::to_jump).filter(|j| !j.address.is_empty()).collect();
    project.startup_commands = shareable(&shared.startup_commands);

    let mut notes = vec![];
    let mut hosts = vec![];
    for shared_host in &file.hosts {
        let address = shared_host.address.trim();
        let host_name = shared_host.name.trim();
        if address.is_empty() {
            let name = if host_name.is_empty() { "A host" } else { host_name };
            notes.push(format!("\"{name}\" has no host name in this file and is left out."));
            continue;
        }
        let mut host = Host::new(if host_name.is_empty() { address } else { host_name }, address);
        host.port = if shared_host.port == 0 { default_port() } else { shared_host.port };
        host.user = shared_host.user.clone();
        host.use_project_jump_hosts = shared_host.use_project_jump_hosts;
        host.run_project_commands = shared_host.run_project_commands;
        host.startup_commands = shareable(&shared_host.startup_commands);
        host.jump_hosts = shared_host.jump_hosts.iter().map(SharedJump::to_jump).filter(|j| !j.address.is_empty()).collect();
        let resolved = resolve(&host, &project);
        hosts.push(FoundHost {
            name: host.name,
            address: host.address,
            port: host.port,
            user: resolved.user,
            icon: icon(&shared_host.icon),
            color: shared_host.color.clone(),
            jump_hosts: Some(resolved.jump_hosts.into_iter().map(|r| r.jump).collect()),
            startup_commands: Some(resolved.startup_commands),
            left_out: vec![],
        });
    }
    if hosts.is_empty() {
        bail!("The project \"{name}\" in this file has no hosts.");
    }
    Ok(Found {
        groups: vec![FoundGroup {
            name: name.to_string(),
            // "letter" draws the name's first letter instead of an icon.
            icon: shared.icon.clone().filter(|i| i == "letter" || is_icon(i)),
            color: shared.color.clone().filter(|c| !c.is_empty()),
            default_user: project.default_user,
            jump_hosts: project.jump_hosts,
            startup_commands: project.startup_commands,
            hosts,
        }],
        notes,
    })
}

/// Read a Project File from disk.
pub fn read_file(path: &std::path::Path, is_icon: &dyn Fn(&str) -> bool) -> Result<Found> {
    let text = std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    read(&text, is_icon)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn any_icon(_: &str) -> bool {
        true
    }

    fn jump(address: &str, user: Option<&str>, credential: Option<&str>) -> JumpHost {
        JumpHost { address: address.into(), port: 22, user: user.map(Into::into), credential: credential.map(Into::into) }
    }

    /// "Atlas": default user deploy, Project Credential, a Jump Host with a saved password and
    /// a Startup Command; web (inherits everything), db (own user and key, own Jump Host,
    /// skips the Project's), and a Host in another Project.
    fn library() -> Library {
        let mut atlas = Project::new("Atlas", "red", "database");
        atlas.default_user = Some("deploy".into());
        atlas.credential = Some("cred-project".into());
        atlas.jump_hosts = vec![jump("bastion.example.com", Some("jump"), Some("cred-bastion"))];
        atlas.startup_commands = vec!["cd /srv".into(), StartupCommand::after("Password:", "hunter2")];
        let web = Host::new("web", "web.lan");
        let mut db = Host::new("db", "10.0.0.5");
        db.port = 2222;
        db.user = Inherited::Own("postgres".into());
        db.credential = Inherited::Own("cred-db-key".into());
        db.use_project_jump_hosts = false;
        db.jump_hosts = vec![jump("db-gw.lan", None, Some("cred-gw"))];
        db.run_project_commands = false;
        db.startup_commands = vec!["sudo -iu postgres".into(), StartupCommand::after("$ ", "psql")];
        db.color = Some("green".into());
        db.icon = Some("database".into());
        atlas.hosts = vec![web.id.clone(), db.id.clone()];
        let mut other = Project::new("Other", "blue", "folder");
        let elsewhere = Host::new("elsewhere", "elsewhere.lan");
        other.hosts = vec![elsewhere.id.clone()];
        Library { shells: None, projects: vec![atlas, other], hosts: vec![web, db, elsewhere] }
    }

    #[test]
    fn a_shared_project_holds_no_credentials() {
        let lib = library();
        let text = write(&lib, &lib.projects[0].id).unwrap();
        assert!(text.starts_with("# A Komando project"));
        assert!(!text.contains("cred-"), "{text}");
        assert!(!text.contains("hunter2"), "a command answering a password question is a password: {text}");
        assert!(!text.contains("credential"), "{text}");
        assert!(!text.contains("elsewhere"), "only the Project's own Hosts");
        assert!(!text.contains(&lib.projects[0].id), "ids are this PC's");
    }

    #[test]
    fn reading_a_shared_project_gives_each_host_its_way_there() {
        let lib = library();
        let found = read(&write(&lib, &lib.projects[0].id).unwrap(), &any_icon).unwrap();
        assert!(found.notes.is_empty());
        let [group] = found.groups.as_slice() else { panic!("one group") };
        assert_eq!(group.name, "Atlas");
        assert_eq!((group.icon.as_deref(), group.color.as_deref(), group.default_user.as_deref()), (Some("database"), Some("red"), Some("deploy")));
        assert_eq!(group.jump_hosts, [jump("bastion.example.com", Some("jump"), None)]);
        assert_eq!(group.startup_commands, ["cd /srv"]);

        let [web, db] = group.hosts.as_slice() else { panic!("two hosts") };
        assert_eq!((web.address.as_str(), web.port, web.user.as_deref()), ("web.lan", 22, Some("deploy")));
        assert_eq!(web.jump_hosts.as_deref(), Some(&[jump("bastion.example.com", Some("jump"), None)][..]));
        assert_eq!(web.startup_commands.as_deref(), Some(&[StartupCommand::new("cd /srv")][..]));

        assert_eq!((db.address.as_str(), db.port, db.user.as_deref()), ("10.0.0.5", 2222, Some("postgres")));
        assert_eq!(db.jump_hosts.as_deref(), Some(&[jump("db-gw.lan", None, None)][..]));
        assert_eq!(db.startup_commands.as_deref(), Some(&["sudo -iu postgres".into(), StartupCommand::after("$ ", "psql")][..]));
        assert_eq!((db.color.as_deref(), db.icon.as_deref()), (Some("green"), Some("database")));
    }

    #[test]
    fn files_that_are_not_project_files_are_refused() {
        for text in ["", "name = 1", "[project]\nname = \"x\"\n", "{\"profiles\": []}"] {
            let err = read(text, &any_icon).unwrap_err();
            assert_eq!(err.to_string(), "This isn't a Komando project file.", "{text:?}");
        }
        let newer = "komando_project = 2\n[project]\nname = \"x\"\n[[hosts]]\nname = \"a\"\naddress = \"a\"\n";
        assert!(read(newer, &any_icon).unwrap_err().to_string().contains("newer Komando"));
        let no_hosts = "komando_project = 1\n[project]\nname = \"Lab\"\n";
        assert!(read(no_hosts, &any_icon).unwrap_err().to_string().contains("no hosts"));
    }

    #[test]
    fn a_hand_written_file_reads_with_defaults_and_drops_what_it_cant_use() {
        let text = "\u{feff}komando_project = 1\n\
            future_setting = true\n\
            [project]\nname = \" Lab \"\nicon = \"no-such-icon\"\n\
            [[hosts]]\nname = \"\"\naddress = \"pi.lan\"\nuser = false\n\
            [[hosts]]\nname = \"empty\"\naddress = \" \"\n";
        let found = read(text, &|i| i == "server").unwrap();
        let group = &found.groups[0];
        assert_eq!((group.name.as_str(), group.icon.as_deref(), group.color.as_deref()), ("Lab", None, None));
        let pi = &group.hosts[0];
        assert_eq!((pi.name.as_str(), pi.port, pi.user.as_deref()), ("pi.lan", 22, None));
        assert_eq!(pi.jump_hosts.as_deref(), Some(&[][..]));
        assert_eq!(found.notes, ["\"empty\" has no host name in this file and is left out."]);
    }
}
