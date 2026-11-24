//! Importing Hosts from other remote-access clients.
//!
//! An importer (`tabby`, `mobaxterm`, `openssh`, or `crate::share` for a Project File) reads
//! the files into `Found`: groups of hosts with what Komando takes over (name, address,
//! protocol, port, user and icon, and where the file has them Jump Hosts, Startup Commands and
//! Port Forwards) and the settings
//! it leaves behind. `plan` then matches that against the Library: which Project each group goes
//! into and which hosts are already there. `apply` adds the hosts the user picked. Secrets
//! never come over (see docs/adr/0008-importing-from-tabby.md).

use crate::model::{AgentUse, Desktop, Host, Inherited, JumpHost, Library, PortForward, Project, ProjectId, Protocol, StartupCommand, resolve};
use crate::theme::PROJECT_COLORS;

pub mod mobaxterm;
pub mod openssh;
pub mod tabby;

/// What an importer read: groups of hosts (each becomes a Project) and notes about what it
/// read but can't import at all, such as other kinds of profiles.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Found {
    pub groups: Vec<FoundGroup>,
    pub notes: Vec<String>,
}

impl Found {
    pub fn host_count(&self) -> usize {
        self.groups.iter().map(|g| g.hosts.len()).sum()
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct FoundGroup {
    /// The Project's name: the group's, with its parents' in front ("Atlas / Production").
    pub name: String,
    /// Lucide icon name, if the group's icon has a match.
    pub icon: Option<String>,
    /// The user the group sets for its hosts; becomes the default user of a new Project.
    pub default_user: Option<String>,
    /// A new Project's color; the next unused preset when None.
    pub color: Option<String>,
    /// A new Project's Jump Hosts, Startup Commands and Port Forwards.
    pub jump_hosts: Vec<JumpHost>,
    pub startup_commands: Vec<StartupCommand>,
    pub port_forwards: Vec<PortForward>,
    pub hosts: Vec<FoundHost>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct FoundHost {
    pub name: String,
    pub address: String,
    /// The session's protocol, port and user. Desktop sessions become a Host that opens its
    /// Remote Desktop; these are not its SSH port or SSH user.
    pub protocol: Protocol,
    pub port: u16,
    pub user: Option<String>,
    /// Lucide icon name, if the host's icon has a match.
    pub icon: Option<String>,
    pub color: Option<String>,
    /// Every Jump Host on the host's way, in order. None: not known, so the host takes the
    /// Project's like a new Host does.
    pub jump_hosts: Option<Vec<JumpHost>>,
    /// Every Startup Command the host runs, the Project's included. None: not known.
    pub startup_commands: Option<Vec<StartupCommand>>,
    /// Every Port Forward the host opens, the Project's included. None: not known.
    pub port_forwards: Option<Vec<PortForward>>,
    /// Agent Forwarding is on for it.
    pub forward_agent: bool,
    /// It signs in without the SSH Agent (`IdentityAgent none`).
    pub agent_off: bool,
    /// Settings the user set in the other client that Komando does not take over.
    pub left_out: Vec<LeftOut>,
}

/// A setting a host has in the other client that is not imported.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LeftOut {
    /// What it is: "Jump host", "Login script".
    pub setting: String,
    /// Its value, short: "via bastion", "2 steps".
    pub detail: String,
}

impl LeftOut {
    pub fn new(setting: impl Into<String>, detail: impl Into<String>) -> Self {
        Self { setting: setting.into(), detail: detail.into() }
    }
}

/// Where a group's hosts go, and which of them Komando already has.
#[derive(Debug, Clone, PartialEq)]
pub struct Plan {
    pub groups: Vec<PlannedGroup>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct PlannedGroup {
    pub target: Target,
    /// Per host of the group, in order.
    pub hosts: Vec<Duplicate>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
    /// A Project with the group's name exists (names match ignoring case): hosts are added to it.
    Existing { id: ProjectId, name: String },
    /// A new Project is made. An earlier group with the same name makes it first; later ones
    /// add to it.
    New,
}

/// Whether a host is already in Komando, which leaves it unpicked at first.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Duplicate {
    No,
    /// A Host with the same name, protocol, address, port, user, Jump Hosts and Startup
    /// Commands is in the Library: its Host and Project.
    InKomando { host: String, project: String },
    /// A host with all the same settings was listed earlier in the same group, under this name.
    Earlier { host: String },
}

impl Duplicate {
    pub fn is_duplicate(&self) -> bool {
        !matches!(self, Duplicate::No)
    }
}

/// What tells two sessions apart for duplicates: protocol, destination and account.
#[derive(PartialEq)]
struct Login {
    protocol: Protocol,
    address: String,
    port: u16,
    user: Option<String>,
}

impl Login {
    fn new(protocol: Protocol, address: &str, port: u16, user: Option<&str>) -> Self {
        Self { protocol, address: address.trim().to_ascii_lowercase(), port, user: user.map(str::to_string) }
    }
}

/// Everything about a host the duplicate check compares. A host made by duplicating another
/// one and then changing its name, address, user, Jump Hosts or anything else is not the same
/// host. Against the Library, Jump Hosts and Startup Commands the file doesn't know (None)
/// match any; between two hosts of the same file they must be the same.
struct Fingerprint {
    name: String,
    login: Login,
    jump_hosts: Option<Vec<JumpHost>>,
    startup_commands: Option<Vec<StartupCommand>>,
}

impl Fingerprint {
    fn found(host: &FoundHost) -> Self {
        let ssh = host.protocol.is_ssh();
        Self {
            name: host.name.trim().to_lowercase(),
            login: Login::new(host.protocol, &host.address, host.port, host.user.as_deref()),
            jump_hosts: host.jump_hosts.as_ref().filter(|_| ssh).map(|route| route.iter().map(without_credential).collect()),
            startup_commands: host.startup_commands.clone().filter(|_| ssh),
        }
    }

    fn matches(&self, other: &Fingerprint, exact: bool) -> bool {
        fn same<T: PartialEq>(a: &Option<T>, b: &Option<T>, exact: bool) -> bool {
            match (a, b) {
                (Some(a), Some(b)) => a == b,
                (None, None) => true,
                _ => !exact,
            }
        }
        self.name == other.name
            && self.login == other.login
            && same(&self.jump_hosts, &other.jump_hosts, exact)
            && same(&self.startup_commands, &other.startup_commands, exact)
    }
}

fn same_name(a: &str, b: &str) -> bool {
    a.trim().to_lowercase() == b.trim().to_lowercase()
}

/// Match what was found against the Library: the Project each group goes into and which
/// hosts are already there. A host counts as already there only when a Host, in whichever
/// Project, has all the same settings: name, protocol, address, port, user, and the Jump
/// Hosts and Startup Commands it connects with. A host counts as listed twice only when an
/// earlier host of the same group has all the same settings, those left in the other client
/// included.
pub fn plan(found: &Found, library: &Library) -> Plan {
    let existing: Vec<(Fingerprint, &str, &str)> = library
        .projects
        .iter()
        .flat_map(|project| {
            library.hosts_of(project).flat_map(move |host| {
                let resolved = resolve(host, project);
                let name = host.name.trim().to_lowercase();
                let user = resolved.user.clone();
                let mut prints = vec![Fingerprint {
                    name: name.clone(),
                    login: Login::new(Protocol::Ssh, &host.address, host.port, user.as_deref()),
                    jump_hosts: Some(resolved.jump_hosts.iter().map(|r| without_credential(&r.jump)).collect()),
                    startup_commands: Some(resolved.startup_commands.clone()),
                }];
                for protocol in host.desktops() {
                    let desktop = host.desktop(protocol).unwrap();
                    let account = match protocol {
                        Protocol::Rdp => desktop.user.as_ref().or(user.as_ref()).map(|user| match &desktop.domain {
                            Some(domain) if !domain.is_empty() && !user.contains('\\') && !user.contains('@') => format!("{domain}\\{user}"),
                            _ => user.clone(),
                        }),
                        _ => None,
                    };
                    prints.push(Fingerprint {
                        name: name.clone(),
                        login: Login::new(protocol, &host.address, desktop.port, account.as_deref()),
                        jump_hosts: None,
                        startup_commands: None,
                    });
                }
                prints.into_iter().map(move |print| (print, host.name.as_str(), project.name.as_str()))
            })
        })
        .collect();
    let mut seen: Vec<(Fingerprint, &[LeftOut], &str, &str)> = vec![];
    let groups = found
        .groups
        .iter()
        .map(|group| {
            let target = match library.projects.iter().find(|p| same_name(&p.name, &group.name)) {
                Some(project) => Target::Existing { id: project.id.clone(), name: project.name.clone() },
                None => Target::New,
            };
            let hosts = group
                .hosts
                .iter()
                .map(|host| {
                    let print = Fingerprint::found(host);
                    let duplicate = if let Some((_, name, project)) = existing.iter().find(|(existing, _, _)| existing.matches(&print, false))
                    {
                        Duplicate::InKomando { host: name.to_string(), project: project.to_string() }
                    } else if let Some((_, _, _, name)) = seen.iter().find(|(earlier, left_out, in_group, _)| {
                        same_name(in_group, &group.name) && *left_out == host.left_out.as_slice() && earlier.matches(&print, true)
                    }) {
                        Duplicate::Earlier { host: name.to_string() }
                    } else {
                        Duplicate::No
                    };
                    seen.push((print, &host.left_out, &group.name, &host.name));
                    duplicate
                })
                .collect();
            PlannedGroup { target, hosts }
        })
        .collect();
    Plan { groups }
}

/// What `apply` did, for the message afterwards.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Applied {
    pub hosts: usize,
    pub new_projects: usize,
    /// Existing Projects that got hosts.
    pub extended_projects: usize,
}

/// Add the picked hosts (`picked[group][host]`) to the Library. Each group goes into the
/// Project `plan` chose, or a new one at the end of the sidebar; a new Project takes the
/// group's user, Jump Hosts, Startup Commands and Port Forwards as its defaults, and its color or the next
/// preset color not yet used. A host whose user is its Project's default user takes it from
/// the Project; otherwise it gets its own, and one without a user in a group with one is
/// asked for it. Desktop sessions instead configure a Remote Desktop with its own port and
/// account, opening that protocol by default and leaving SSH sign-in unset. A host with known Jump Hosts, Startup Commands
/// or Port Forwards
/// takes the Project's when they start its own list and gets the rest as its own; otherwise
/// it switches the Project's off and gets the whole list, so it reaches its machine the same
/// way in a Project with other defaults. No Credential is ever set, and none is inherited: in
/// a Project with a saved password or key, an imported host asks instead, so a file can't
/// point a host at its own server and receive that Project's password.
pub fn apply(found: &Found, plan: &Plan, picked: &[Vec<bool>], library: &mut Library) -> Applied {
    let mut applied = Applied::default();
    let mut extended: Vec<ProjectId> = vec![];
    for (gi, (group, planned)) in found.groups.iter().zip(&plan.groups).enumerate() {
        let chosen: Vec<&FoundHost> = group
            .hosts
            .iter()
            .enumerate()
            .filter(|(hi, _)| picked.get(gi).and_then(|g| g.get(*hi)).copied().unwrap_or(false))
            .map(|(_, h)| h)
            .collect();
        if chosen.is_empty() {
            continue;
        }
        let existing = match &planned.target {
            Target::Existing { id, .. } => library.projects.iter().position(|p| &p.id == id),
            // An earlier group of the same name made it in this import.
            Target::New => library.projects.iter().position(|p| same_name(&p.name, &group.name)),
        };
        let index = match existing {
            Some(index) => {
                let id = library.projects[index].id.clone();
                if !extended.contains(&id) {
                    extended.push(id);
                    applied.extended_projects += 1;
                }
                index
            }
            None => {
                let color = group.color.clone().unwrap_or_else(|| next_color(library).to_string());
                let mut project = Project::new(group.name.trim(), color, group.icon.clone().unwrap_or_else(|| "folder".into()));
                project.default_user = group.default_user.clone().filter(|u| !u.is_empty());
                project.jump_hosts = group.jump_hosts.iter().map(without_credential).collect();
                project.startup_commands = group.startup_commands.clone();
                project.port_forwards = group.port_forwards.clone();
                extended.push(project.id.clone());
                library.projects.push(project);
                applied.new_projects += 1;
                library.projects.len() - 1
            }
        };
        let default_user = library.projects[index].default_user.clone();
        let project_has_credential = library.projects[index].credential.is_some();
        let project_jumps: Vec<JumpHost> = library.projects[index].jump_hosts.iter().map(without_credential).collect();
        let project_commands = library.projects[index].startup_commands.clone();
        let project_forwards = library.projects[index].port_forwards.clone();
        for found in chosen {
            let mut host = Host::new(found.name.trim(), found.address.trim());
            if found.protocol.is_ssh() {
                host.port = found.port;
                host.user = match &found.user {
                    Some(user) if Some(user) != default_user.as_ref() => Inherited::Own(user.clone()),
                    // No user although its group has one: the user is asked when connecting.
                    None if group.default_user.is_some() => Inherited::Off,
                    _ => Inherited::Project,
                };
                if project_has_credential {
                    host.credential = Inherited::Off;
                }
            } else {
                let desktop = Desktop {
                    port: found.port,
                    user: if found.protocol == Protocol::Rdp { found.user.clone() } else { None },
                    ..Desktop::new(found.protocol)
                };
                match found.protocol {
                    Protocol::Rdp => host.rdp = Some(desktop),
                    Protocol::Vnc => host.vnc = Some(desktop),
                    Protocol::Ssh => unreachable!(),
                }
                host.opens = found.protocol;
                // Desktop users and passwords are independent of SSH and Project defaults.
                host.user = Inherited::Off;
                host.credential = Inherited::Off;
            }
            host.icon = found.icon.clone();
            host.color = found.color.clone();
            if let Some(route) = &found.jump_hosts {
                let route: Vec<JumpHost> = route.iter().map(without_credential).collect();
                (host.use_project_jump_hosts, host.jump_hosts) = split_off_project(&route, &project_jumps);
            }
            if let Some(commands) = &found.startup_commands {
                (host.run_project_commands, host.startup_commands) = split_off_project(commands, &project_commands);
            }
            if let Some(forwards) = found.port_forwards.as_ref().filter(|_| found.protocol.is_ssh()) {
                (host.use_project_forwards, host.port_forwards) = split_off_project(forwards, &project_forwards);
            }
            if found.protocol.is_ssh() {
                // On for the Host itself, whatever its Project says; left to the Project otherwise.
                host.forward_agent = found.forward_agent.then_some(true);
                if found.agent_off {
                    host.agent = AgentUse::Off;
                }
            }
            library.projects[index].hosts.push(host.id.clone());
            library.hosts.push(host);
            applied.hosts += 1;
        }
    }
    applied
}

/// Whether a host's whole list starts with the Project's (so it takes them from the Project),
/// and what it adds of its own.
fn split_off_project<T: PartialEq + Clone>(all: &[T], project: &[T]) -> (bool, Vec<T>) {
    match all.strip_prefix(project) {
        Some(own) => (true, own.to_vec()),
        None => (false, all.to_vec()),
    }
}

/// Imported Jump Hosts never name a Credential; one in this Library is compared without it.
fn without_credential(jump: &JumpHost) -> JumpHost {
    JumpHost { credential: None, ..jump.clone() }
}

/// The first preset color no Project uses yet, or the least used one.
fn next_color(library: &Library) -> &'static str {
    let used = |name: &str| library.projects.iter().filter(|p| p.color == name).count();
    PROJECT_COLORS.iter().map(|(name, _)| *name).min_by_key(|name| used(name)).unwrap_or("blue")
}

/// "a, b and c", for the importers' notes.
fn join_and(items: &[String]) -> String {
    match items {
        [] => String::new(),
        [one] => one.clone(),
        [rest @ .., last] => format!("{} and {last}", rest.join(", ")),
    }
}

/// A private key's file name, from a Windows or Unix path, for what an import leaves out.
/// MobaXterm also writes `_CurrentDrive_:\…` and `_ProfileDir_\…` for folders it moves along
/// with a portable install.
fn key_file_name(path: &str) -> String {
    path.rsplit(['/', '\\']).find(|s| !s.is_empty()).unwrap_or(path).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn found_host(name: &str, address: &str, user: Option<&str>) -> FoundHost {
        FoundHost { name: name.into(), address: address.into(), port: 22, user: user.map(Into::into), ..Default::default() }
    }

    fn group(name: &str, default_user: Option<&str>, hosts: Vec<FoundHost>) -> FoundGroup {
        FoundGroup { name: name.into(), icon: Some("database".into()), default_user: default_user.map(Into::into), hosts, ..Default::default() }
    }

    /// One Project "Atlas" (default user "deploy") with web01 at web01.lan.
    fn library() -> Library {
        let mut project = Project::new("Atlas", "red", "folder");
        project.default_user = Some("deploy".into());
        let host = Host::new("web01", "web01.lan");
        project.hosts.push(host.id.clone());
        Library { shells: None, projects: vec![project], hosts: vec![host] }
    }

    fn all(found: &Found) -> Vec<Vec<bool>> {
        found.groups.iter().map(|g| vec![true; g.hosts.len()]).collect()
    }

    #[test]
    fn a_group_with_a_projects_name_goes_into_that_project() {
        let found = Found { groups: vec![group("atlas ", None, vec![]), group("Hermes", None, vec![])], notes: vec![] };
        let lib = library();
        let plan = plan(&found, &lib);
        assert_eq!(plan.groups[0].target, Target::Existing { id: lib.projects[0].id.clone(), name: "Atlas".into() });
        assert_eq!(plan.groups[1].target, Target::New);
    }

    #[test]
    fn hosts_komando_has_are_duplicates_only_when_every_setting_matches() {
        let found = Found {
            groups: vec![group(
                "Hermes",
                None,
                vec![
                    found_host(" WEB01 ", "WEB01.lan", Some("deploy")),
                    found_host("web01", "web01.lan", Some("root")),
                    FoundHost { port: 2222, ..found_host("web01", "web01.lan", Some("deploy")) },
                    found_host("web01", "web02.lan", Some("deploy")),
                    found_host("web01 (copy)", "web01.lan", Some("deploy")),
                    FoundHost { jump_hosts: Some(vec![jump("bastion")]), ..found_host("web01", "web01.lan", Some("deploy")) },
                    FoundHost { startup_commands: Some(vec!["htop".into()]), ..found_host("web01", "web01.lan", Some("deploy")) },
                    found_host("db", "db.lan", Some("root")),
                    found_host("db", "db.lan", Some("root")),
                ],
            )],
            notes: vec![],
        };
        let plan = plan(&found, &library());
        assert_eq!(
            plan.groups[0].hosts,
            [
                Duplicate::InKomando { host: "web01".into(), project: "Atlas".into() },
                Duplicate::No,
                Duplicate::No,
                Duplicate::No,
                Duplicate::No,
                Duplicate::No,
                Duplicate::No,
                Duplicate::No,
                Duplicate::Earlier { host: "db".into() },
            ]
        );
    }

    #[test]
    fn a_duplicated_profile_with_a_changed_setting_is_not_listed_twice() {
        let original = FoundHost { left_out: vec![LeftOut::new("Port forwarding", "Local 8080 → db:5432")], ..found_host("app", "10.0.0.5", Some("ops")) };
        let found = Found {
            groups: vec![
                group(
                    "Hermes",
                    None,
                    vec![
                        original.clone(),
                        FoundHost { address: "10.0.0.6".into(), ..original.clone() },
                        FoundHost { name: "app 2".into(), ..original.clone() },
                        FoundHost { left_out: vec![], ..original.clone() },
                        FoundHost { port: 2200, ..original.clone() },
                        original.clone(),
                    ],
                ),
                group("Other", None, vec![original.clone()]),
            ],
            notes: vec![],
        };
        let plan = plan(&found, &Library::default());
        assert_eq!(
            plan.groups[0].hosts,
            [Duplicate::No, Duplicate::No, Duplicate::No, Duplicate::No, Duplicate::No, Duplicate::Earlier { host: "app".into() }]
        );
        assert_eq!(plan.groups[1].hosts, [Duplicate::No], "another group makes another project");
    }

    #[test]
    fn duplicate_checks_distinguish_protocols_and_find_desktops_on_existing_hosts() {
        let mut lib = library();
        lib.hosts[0].rdp = Some(Desktop {
            port: 3390,
            user: Some("alice".into()),
            domain: Some("ACME".into()),
            ..Desktop::new(Protocol::Rdp)
        });
        lib.hosts[0].vnc = Some(Desktop::new(Protocol::Vnc));
        let session = |name, protocol, port, user| FoundHost { protocol, port, ..found_host(name, "WEB01.lan", user) };
        let found = Found {
            groups: vec![group("Desktops", None, vec![
                session("web01", Protocol::Rdp, 3390, Some("ACME\\alice")),
                session("web01", Protocol::Rdp, 3390, Some("ACME\\bob")),
                session("web01", Protocol::Rdp, 3389, Some("ACME\\alice")),
                session("web01", Protocol::Vnc, 5900, None),
                session("web01", Protocol::Ssh, 3390, Some("ACME\\alice")),
                session("other name", Protocol::Vnc, 5900, None),
                session("new", Protocol::Rdp, 2222, Some("ops")),
                session("new", Protocol::Vnc, 2222, None),
                session("new", Protocol::Ssh, 2222, Some("ops")),
                session("new", Protocol::Rdp, 2222, Some("ops")),
            ])],
            notes: vec![],
        };
        let existing = Duplicate::InKomando { host: "web01".into(), project: "Atlas".into() };
        assert_eq!(plan(&found, &lib).groups[0].hosts, [
            existing.clone(), Duplicate::No, Duplicate::No, existing,
            Duplicate::No, Duplicate::No, Duplicate::No, Duplicate::No, Duplicate::No,
            Duplicate::Earlier { host: "new".into() },
        ]);
    }

    #[test]
    fn mobaxterm_ssh_sessions_without_a_command_do_not_inherit_project_commands() {
        let found = mobaxterm::parse("[Bookmarks]\nSubRep=Atlas\nDirect=#109#0%new.lan%22%ops%%-1%0%\n").unwrap();
        let mut lib = library();
        lib.projects[0].startup_commands = vec![StartupCommand::new("do-not-run")];
        let planned = plan(&found, &lib);
        assert_eq!(apply(&found, &planned, &all(&found), &mut lib).hosts, 1);
        let host = lib.hosts.last().unwrap();
        assert!(!host.run_project_commands);
        assert!(host.startup_commands.is_empty());
    }

    #[test]
    fn mobaxterm_desktops_open_their_protocol_and_keep_accounts_separate_from_ssh() {
        let found = mobaxterm::parse(concat!(
            "[Bookmarks]\nSubRep=Atlas\n",
            "Windows=#91#4%win.lan%3390%ACME\\alice%0%\n",
            "Ask user=#91#4%win2.lan%3389%<default>%0%\n",
            "Console=#128#5%vnc.lan%5901%-1%0%%22%%%-1\n",
        )).unwrap();
        let mut lib = library();
        lib.projects[0].credential = Some("project-secret".into());
        lib.projects[0].jump_hosts = vec![jump("project-gateway")];
        lib.projects[0].startup_commands = vec![StartupCommand::new("do-not-run")];
        let planned = plan(&found, &lib);
        assert_eq!(apply(&found, &planned, &all(&found), &mut lib).hosts, 3);
        for (name, protocol, port, user) in [
            ("Windows", Protocol::Rdp, 3390, Some("ACME\\alice")),
            ("Ask user", Protocol::Rdp, 3389, None),
            ("Console", Protocol::Vnc, 5901, None),
        ] {
            let host = lib.hosts.iter().find(|h| h.name == name).unwrap();
            assert_eq!(host.opens_with(), protocol);
            assert_eq!(host.port, 22, "the desktop port never becomes an SSH port");
            assert_eq!((host.user.clone(), host.credential.clone()), (Inherited::Off, Inherited::Off));
            let desktop = host.desktop(protocol).unwrap();
            assert_eq!((desktop.port, desktop.user.as_deref()), (port, user));
            assert!(desktop.credential.is_none() && !desktop.through_ssh);
            assert!(host.jump_hosts.is_empty() && host.startup_commands.is_empty());
            assert!(!host.use_project_jump_hosts && !host.run_project_commands);
            let saved = toml::to_string(host).unwrap();
            assert_eq!(toml::from_str::<Host>(&saved).unwrap(), *host);
        }
        let repeated = plan(&found, &lib);
        assert!(repeated.groups[0].hosts.iter().all(Duplicate::is_duplicate), "re-importing finds the saved desktops");
    }

    #[test]
    fn applying_adds_picked_hosts_to_new_and_existing_projects() {
        let found = Found {
            groups: vec![
                group("Atlas", Some("ignored"), vec![found_host("db01", "db01.lan", Some("deploy")), found_host("db02", "db02.lan", Some("root"))]),
                group("Hermes", Some("ops"), vec![found_host("api", "api.lan", Some("ops")), found_host("skip", "skip.lan", None)]),
                group("Empty pick", None, vec![found_host("x", "x.lan", None)]),
            ],
            notes: vec![],
        };
        let mut lib = library();
        let plan = plan(&found, &lib);
        let picked = vec![vec![true, true], vec![true, false], vec![false]];
        let applied = apply(&found, &plan, &picked, &mut lib);
        assert_eq!(applied, Applied { hosts: 3, new_projects: 1, extended_projects: 1 });

        let atlas = &lib.projects[0];
        assert_eq!(atlas.default_user.as_deref(), Some("deploy"), "an existing Project keeps its defaults");
        let db01 = lib.hosts.iter().find(|h| h.name == "db01").unwrap();
        assert_eq!(db01.user, Inherited::Project, "same as the Project's default user");
        let db02 = lib.hosts.iter().find(|h| h.name == "db02").unwrap();
        assert_eq!(db02.user, Inherited::Own("root".into()));
        assert_eq!(atlas.hosts.len(), 3);

        let hermes = lib.projects.iter().find(|p| p.name == "Hermes").unwrap();
        assert_eq!(hermes.default_user.as_deref(), Some("ops"));
        assert_eq!(hermes.icon, "database");
        assert_ne!(hermes.color, "red", "a color no Project uses yet");
        assert_eq!(hermes.hosts.len(), 1);
        let api = lib.host(&hermes.hosts[0]).unwrap();
        assert_eq!((api.user.clone(), api.credential.clone()), (Inherited::Project, Inherited::Project));
        assert!(api.jump_hosts.is_empty() && api.startup_commands.is_empty() && api.color.is_none());
        assert!(!lib.projects.iter().any(|p| p.name == "Empty pick"), "no Project without hosts");
    }

    #[test]
    fn a_host_imported_into_a_project_with_a_password_does_not_get_it() {
        let found = Found { groups: vec![group("Atlas", None, vec![found_host("db09", "attacker.example", Some("deploy"))])], notes: vec![] };
        let mut lib = library();
        lib.projects[0].credential = Some("atlas-password".into());
        let plan = plan(&found, &lib);
        apply(&found, &plan, &[vec![true]], &mut lib);
        let host = lib.hosts.iter().find(|h| h.name == "db09").unwrap();
        assert_eq!(host.credential, Inherited::Off, "asks instead of sending the Project's password");
        let resolved = crate::model::resolve(host, &lib.projects[0]);
        assert_eq!(resolved.credential, None);
    }

    fn jump(address: &str) -> JumpHost {
        JumpHost { user: Some("jump".into()), ..JumpHost::new(address) }
    }

    /// A shared project's group: bastion and `cd /srv` for every host; db adds its own
    /// gateway and command, lab skips the Project's.
    fn shared() -> Found {
        let routed = |name: &str, jumps: &[&str], commands: &[&str]| FoundHost {
            jump_hosts: Some(jumps.iter().map(|a| jump(a)).collect()),
            startup_commands: Some(commands.iter().map(|c| StartupCommand::new(*c)).collect()),
            ..found_host(name, &format!("{name}.lan"), Some("ops"))
        };
        Found {
            groups: vec![FoundGroup {
                color: Some("green".into()),
                jump_hosts: vec![jump("bastion")],
                startup_commands: vec!["cd /srv".into()],
                ..group(
                    "Hermes",
                    Some("ops"),
                    vec![
                        routed("api", &["bastion"], &["cd /srv"]),
                        routed("db", &["bastion", "db-gw"], &["cd /srv", "psql"]),
                        routed("lab", &[], &[]),
                    ],
                )
            }],
            notes: vec![],
        }
    }

    #[test]
    fn a_shared_project_comes_over_with_its_jump_hosts_and_startup_commands() {
        let found = shared();
        let mut lib = Library::default();
        let plan = plan(&found, &lib);
        apply(&found, &plan, &all(&found), &mut lib);
        let hermes = &lib.projects[0];
        assert_eq!((hermes.color.as_str(), hermes.jump_hosts.clone(), hermes.startup_commands.clone()), ("green", vec![jump("bastion")], vec![StartupCommand::new("cd /srv")]));
        let host = |name: &str| lib.hosts.iter().find(|h| h.name == name).unwrap();
        assert!(host("api").use_project_jump_hosts && host("api").jump_hosts.is_empty() && host("api").run_project_commands);
        assert!(host("db").use_project_jump_hosts && host("db").run_project_commands);
        assert_eq!((host("db").jump_hosts.clone(), host("db").startup_commands.clone()), (vec![jump("db-gw")], vec![StartupCommand::new("psql")]));
        assert!(!host("lab").use_project_jump_hosts && !host("lab").run_project_commands);
        assert_eq!(host("api").user, Inherited::Project);
        for name in ["api", "db", "lab"] {
            let resolved = lib.resolve(&hermes.id, &host(name).id).unwrap();
            let route: Vec<JumpHost> = resolved.jump_hosts.into_iter().map(|r| r.jump).collect();
            assert_eq!(Some(route), found.groups[0].hosts.iter().find(|h| h.name == name).unwrap().jump_hosts, "{name}");
        }
    }

    #[test]
    fn imported_port_forwards_take_the_projects_when_they_start_with_them() {
        let socks = PortForward::socks(1080);
        let db = PortForward::local(5432, "localhost", 5432);
        let mut first = found_host("web", "web.lan", None);
        first.port_forwards = Some(vec![socks.clone(), db.clone()]);
        let mut second = found_host("lab", "lab.lan", None);
        second.port_forwards = Some(vec![db.clone()]);
        let mut hermes = group("Hermes", None, vec![first, second]);
        hermes.port_forwards = vec![socks.clone()];
        let found = Found { groups: vec![hermes], notes: vec![] };
        let mut lib = Library::default();
        apply(&found, &super::plan(&found, &lib), &all(&found), &mut lib);
        assert_eq!(lib.projects[0].port_forwards, [socks]);
        let host = |name: &str| lib.hosts.iter().find(|h| h.name == name).unwrap();
        assert!(host("web").use_project_forwards);
        assert_eq!(host("web").port_forwards, std::slice::from_ref(&db));
        assert!(!host("lab").use_project_forwards, "it has no SOCKS proxy");
        assert_eq!(host("lab").port_forwards, [db]);
    }

    #[test]
    fn a_host_without_a_user_in_a_group_with_one_is_asked_for_it() {
        let found = Found { groups: vec![group("Hermes", Some("ops"), vec![found_host("ask", "ask.lan", None)])], notes: vec![] };
        let mut lib = Library::default();
        apply(&found, &super::plan(&found, &lib), &all(&found), &mut lib);
        assert_eq!(lib.hosts[0].user, Inherited::Off);
        assert_eq!(lib.resolve(&lib.projects[0].id, &lib.hosts[0].id).unwrap().user, None);
    }

    #[test]
    fn shared_hosts_added_to_a_project_with_other_jump_hosts_keep_their_own_way() {
        let found = shared();
        let mut lib = library();
        lib.projects[0].name = "Hermes".into();
        lib.projects[0].jump_hosts = vec![JumpHost { credential: Some("saved".into()), ..jump("other-bastion") }];
        apply(&found, &super::plan(&found, &lib), &all(&found), &mut lib);
        let api = lib.hosts.iter().find(|h| h.name == "api").unwrap();
        assert!(!api.use_project_jump_hosts, "the Project's Jump Host isn't on its way");
        assert!(api.run_project_commands, "the Project has no Startup Commands to skip");
        assert_eq!((api.jump_hosts.clone(), api.startup_commands.clone()), (vec![jump("bastion")], vec![StartupCommand::new("cd /srv")]));
        assert_eq!(lib.projects[0].jump_hosts[0].address, "other-bastion", "an existing Project keeps its defaults");

        // The same Jump Host, with a password saved here: the host takes it from the Project.
        let mut lib = library();
        lib.projects[0].name = "Hermes".into();
        lib.projects[0].jump_hosts = vec![JumpHost { credential: Some("saved".into()), ..jump("bastion") }];
        lib.projects[0].startup_commands = vec!["cd /srv".into()];
        apply(&found, &super::plan(&found, &lib), &all(&found), &mut lib);
        let api = lib.hosts.iter().find(|h| h.name == "api").unwrap();
        assert!(api.use_project_jump_hosts && api.jump_hosts.is_empty() && api.run_project_commands);
    }

    #[test]
    fn groups_with_the_same_new_name_share_one_project() {
        let found = Found {
            groups: vec![group("Lab", None, vec![found_host("a", "a.lan", None)]), group("lab", None, vec![found_host("b", "b.lan", None)])],
            notes: vec![],
        };
        let mut lib = Library::default();
        let plan = plan(&found, &lib);
        let applied = apply(&found, &plan, &all(&found), &mut lib);
        assert_eq!(applied, Applied { hosts: 2, new_projects: 1, extended_projects: 0 });
        assert_eq!(lib.projects.len(), 1);
        assert_eq!(lib.projects[0].hosts.len(), 2);
    }

    #[test]
    fn importing_the_same_file_again_finds_everything_already_there() {
        let found = Found {
            groups: vec![group("Hermes", Some("ops"), vec![found_host("api", "api.lan", Some("ops")), found_host("db", "db.lan", Some("root"))])],
            notes: vec![],
        };
        let mut lib = library();
        let first = plan(&found, &lib);
        apply(&found, &first, &all(&found), &mut lib);
        let again = plan(&found, &lib);
        assert!(matches!(again.groups[0].target, Target::Existing { .. }));
        assert!(again.groups[0].hosts.iter().all(Duplicate::is_duplicate));
    }
}
