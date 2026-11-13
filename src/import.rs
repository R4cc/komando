//! Importing Hosts from other SSH clients.
//!
//! An importer (only `tabby` so far) reads the other client's files into `Found`: groups of
//! hosts with what Komando takes over (name, address, port, user, icon) and the settings it
//! leaves behind. `plan` then matches that against the Library: which Project each group goes
//! into and which hosts are already there. `apply` adds the hosts the user picked. Secrets
//! never come over (see docs/adr/0008-importing-from-tabby.md).

use crate::model::{Host, Inherited, Library, Project, ProjectId, resolve};
use crate::theme::PROJECT_COLORS;

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

#[derive(Debug, Clone, PartialEq)]
pub struct FoundGroup {
    /// The Project's name: the group's, with its parents' in front ("Atlas / Production").
    pub name: String,
    /// Lucide icon name, if the group's icon has a match.
    pub icon: Option<String>,
    /// The user the group sets for its hosts; becomes the default user of a new Project.
    pub default_user: Option<String>,
    pub hosts: Vec<FoundHost>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct FoundHost {
    pub name: String,
    pub address: String,
    pub port: u16,
    pub user: Option<String>,
    /// Lucide icon name, if the host's icon has a match.
    pub icon: Option<String>,
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
    /// A Host with the same address, port and user is in the Library: its name and Project.
    InKomando { host: String, project: String },
    /// The same address, port and user is listed earlier in this import, under this name.
    Earlier { host: String },
}

impl Duplicate {
    pub fn is_duplicate(&self) -> bool {
        !matches!(self, Duplicate::No)
    }
}

/// What tells two hosts apart for duplicates: where they connect and as whom.
fn same_login(a: (&str, u16, Option<&str>), b: (&str, u16, Option<&str>)) -> bool {
    a.0.trim().eq_ignore_ascii_case(b.0.trim()) && a.1 == b.1 && a.2 == b.2
}

fn same_name(a: &str, b: &str) -> bool {
    a.trim().to_lowercase() == b.trim().to_lowercase()
}

/// Match what was found against the Library: the Project each group goes into and which
/// hosts are already there. A host counts as already there when any Host connects to the
/// same address and port as the same user, in whichever Project it is.
pub fn plan(found: &Found, library: &Library) -> Plan {
    let existing: Vec<(&str, u16, Option<String>, &str, &str)> = library
        .projects
        .iter()
        .flat_map(|project| {
            library.hosts_of(project).map(move |host| {
                let user = resolve(host, project).user;
                (host.address.as_str(), host.port, user, host.name.as_str(), project.name.as_str())
            })
        })
        .collect();
    let mut seen: Vec<(&FoundHost, &str)> = vec![];
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
                    let login = (host.address.as_str(), host.port, host.user.as_deref());
                    let duplicate = if let Some((_, _, _, name, project)) =
                        existing.iter().find(|(a, p, u, _, _)| same_login((a, *p, u.as_deref()), login))
                    {
                        Duplicate::InKomando { host: name.to_string(), project: project.to_string() }
                    } else if let Some((earlier, _)) =
                        seen.iter().find(|(h, _)| same_login((&h.address, h.port, h.user.as_deref()), login))
                    {
                        Duplicate::Earlier { host: earlier.name.clone() }
                    } else {
                        Duplicate::No
                    };
                    seen.push((host, &group.name));
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
/// group's user as its default user and the next preset color not yet used. A host whose
/// user is its Project's default user takes it from the Project; otherwise it gets its own.
/// Nothing else is set: no Credential, no Jump Hosts, no Startup Commands, no color.
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
                let color = next_color(library);
                let mut project = Project::new(group.name.trim(), color, group.icon.clone().unwrap_or_else(|| "folder".into()));
                project.default_user = group.default_user.clone().filter(|u| !u.is_empty());
                extended.push(project.id.clone());
                library.projects.push(project);
                applied.new_projects += 1;
                library.projects.len() - 1
            }
        };
        let default_user = library.projects[index].default_user.clone();
        for found in chosen {
            let mut host = Host::new(found.name.trim(), found.address.trim());
            host.port = found.port;
            host.user = match &found.user {
                Some(user) if Some(user) != default_user.as_ref() => Inherited::Own(user.clone()),
                _ => Inherited::Project,
            };
            host.icon = found.icon.clone();
            library.projects[index].hosts.push(host.id.clone());
            library.hosts.push(host);
            applied.hosts += 1;
        }
    }
    applied
}

/// The first preset color no Project uses yet, or the least used one.
fn next_color(library: &Library) -> &'static str {
    let used = |name: &str| library.projects.iter().filter(|p| p.color == name).count();
    PROJECT_COLORS.iter().map(|(name, _)| *name).min_by_key(|name| used(name)).unwrap_or("blue")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn found_host(name: &str, address: &str, user: Option<&str>) -> FoundHost {
        FoundHost { name: name.into(), address: address.into(), port: 22, user: user.map(Into::into), icon: None, left_out: vec![] }
    }

    fn group(name: &str, default_user: Option<&str>, hosts: Vec<FoundHost>) -> FoundGroup {
        FoundGroup { name: name.into(), icon: Some("database".into()), default_user: default_user.map(Into::into), hosts }
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
    fn hosts_komando_has_are_duplicates_by_address_port_and_user() {
        let found = Found {
            groups: vec![group(
                "Hermes",
                None,
                vec![
                    found_host("web", "WEB01.lan", Some("deploy")),
                    found_host("web as root", "web01.lan", Some("root")),
                    FoundHost { port: 2222, ..found_host("web ssh2", "web01.lan", Some("deploy")) },
                    found_host("db", "db.lan", Some("root")),
                    found_host("db again", "db.lan", Some("root")),
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
                Duplicate::Earlier { host: "db".into() },
            ]
        );
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
