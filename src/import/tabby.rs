//! Reading SSH profiles from Tabby (tabby.sh).
//!
//! Tabby keeps everything in one YAML file, `config.yaml`, in `%APPDATA%\tabby` (a portable
//! Tabby keeps it in the `data` folder next to Tabby.exe). Profiles are listed under
//! `profiles`, each with a `type` (`ssh`, `local`, `serial`, …), a `group` (a group's id)
//! and its own `options`. Groups are listed under `groups` and can sit in another group
//! (`parentGroupId`). A setting a profile leaves out comes from its group's `defaults.ssh`,
//! then from `profileDefaults.ssh`, then from Tabby's built-in defaults, merged in that order
//! (lists are replaced, not joined). With "Encrypt config file" on, Tabby stores the whole
//! configuration encrypted under `vault`; it can't be read without Tabby's vault passphrase.

use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result, bail};
use serde_json::{Map, Value, json};

use super::{Found, FoundGroup, FoundHost, LeftOut};

/// The file name Tabby keeps its configuration in.
pub const CONFIG_FILE: &str = "config.yaml";

/// The project name for profiles that are in no group (Tabby lists them as "Ungrouped").
pub const UNGROUPED: &str = "Tabby";

/// Where Tabby keeps its configuration on this PC, most likely first.
pub fn config_candidates() -> Vec<PathBuf> {
    let mut dirs = vec![];
    if let Some(dir) = std::env::var_os("TABBY_CONFIG_DIRECTORY") {
        dirs.push(PathBuf::from(dir));
    }
    if let Some(config) = dirs::config_dir() {
        dirs.push(config.join("tabby"));
        // Tabby's name before 2021; Tabby itself copies it over when it is newer.
        dirs.push(config.join("terminus"));
    }
    if let Some(home) = dirs::home_dir() {
        // Installed with Scoop: the portable data folder is kept here.
        dirs.push(home.join("scoop").join("persist").join("tabby").join("data"));
    }
    dirs.into_iter().map(|dir| dir.join(CONFIG_FILE)).collect()
}

/// Tabby's configuration on this PC, if it is in one of the usual places.
pub fn find_config() -> Option<PathBuf> {
    config_candidates().into_iter().find(|path| path.is_file())
}

/// The configuration file a picked path means: the file itself, or `config.yaml` in a picked
/// folder (or in its `data` folder, for a portable Tabby's install folder).
pub fn config_in(path: &Path) -> Result<PathBuf> {
    if path.is_file() {
        return Ok(path.to_path_buf());
    }
    for candidate in [path.join(CONFIG_FILE), path.join("data").join(CONFIG_FILE)] {
        if candidate.is_file() {
            return Ok(candidate);
        }
    }
    bail!("There is no {CONFIG_FILE} in {}.", path.display())
}

/// Read Tabby's configuration. `is_icon` tells whether a Lucide icon of that name exists.
pub fn read(path: &Path, is_icon: &dyn Fn(&str) -> bool) -> Result<Found> {
    let text = std::fs::read_to_string(path).with_context(|| format!("Could not read {}", path.display()))?;
    parse(&text, is_icon)
}

/// Tabby's built-in settings for SSH profiles (`SSHProfilesService.configDefaults` and
/// `ProfilesService.profileDefaults`), for the settings checked here. A value equal to these
/// was not set by the user.
fn builtin_defaults() -> Value {
    json!({
        "options": {
            "port": 22,
            "user": "root",
            "auth": null,
            "privateKeys": [],
            "keepaliveInterval": 5000,
            "keepaliveCountMax": 10,
            "x11": false,
            "jumpHost": null,
            "agentForward": false,
            "proxyCommand": null,
            "forwardedPorts": [],
            "scripts": [],
            "socksProxyHost": null,
            "socksProxyPort": null,
            "httpProxyHost": null,
            "httpProxyPort": null,
            "input": { "backspace": "backspace" },
            "cwd": null,
            "term": "xterm-256color",
        },
        "behaviorOnSessionEnd": "auto",
    })
}

pub fn parse(text: &str, is_icon: &dyn Fn(&str) -> bool) -> Result<Found> {
    let config: Value = serde_saphyr::from_str(text).map_err(|err| anyhow::anyhow!("This isn't a Tabby configuration file: {err}"))?;
    let Some(config) = config.as_object() else { bail!("This isn't a Tabby configuration file.") };
    if config.get("encrypted").and_then(Value::as_bool) == Some(true) {
        bail!(
            "This Tabby configuration is encrypted with Tabby's vault. In Tabby, open Settings → Vault, \
             switch off \"Encrypt config file\", then import again. You can switch it back on afterwards."
        );
    }
    if !config.contains_key("profiles") && !config.contains_key("version") {
        bail!("This isn't a Tabby configuration file: it lists no profiles.");
    }
    let profiles: Vec<&Map<String, Value>> = list(config.get("profiles")).filter_map(Value::as_object).collect();
    let groups: Vec<&Map<String, Value>> = list(config.get("groups")).filter_map(Value::as_object).collect();
    let global = config.get("profileDefaults").and_then(|d| d.get("ssh")).cloned().unwrap_or(Value::Null);
    let builtin = builtin_defaults();
    let global_user = set_value(&global, &builtin, &["options", "user"]).and_then(|v| text_of(Some(v))).map(str::to_string);

    // Every group with its name path, parents before children, in the file's order.
    let ordered = group_order(&groups);
    let mut found_groups: Vec<(String, FoundGroup)> = ordered
        .iter()
        .map(|(id, name)| {
            let group = groups.iter().find(|g| text_of(g.get("id")) == Some(id.as_str()));
            let defaults = group.and_then(|g| g.get("defaults")).and_then(|d| d.get("ssh")).cloned().unwrap_or(Value::Null);
            let user_layers = merge(global.clone(), defaults);
            let default_user = set_value(&user_layers, &builtin, &["options", "user"]).and_then(|v| text_of(Some(v))).map(str::to_string);
            let icon = group.and_then(|g| text_of(g.get("icon"))).and_then(|i| lucide_icon(i, is_icon));
            (id.clone(), FoundGroup { name: name.clone(), icon, default_user, hosts: vec![] })
        })
        .collect();

    let mut other_types: Vec<(String, usize)> = vec![];
    let mut notes = vec![];
    for profile in &profiles {
        if flag(profile.get("isBuiltin")) || flag(profile.get("isTemplate")) {
            continue;
        }
        let kind = text_of(profile.get("type")).unwrap_or("");
        if kind != "ssh" {
            match other_types.iter_mut().find(|(k, _)| k == kind) {
                Some((_, n)) => *n += 1,
                None => other_types.push((kind.to_string(), 1)),
            }
            continue;
        }
        let group_id = text_of(profile.get("group")).map(str::trim).unwrap_or("");
        let group = groups.iter().find(|g| text_of(g.get("id")) == Some(group_id));
        let group_defaults = group.and_then(|g| g.get("defaults")).and_then(|d| d.get("ssh")).cloned().unwrap_or(Value::Null);
        // What the user set, from the widest to the narrowest; then with Tabby's defaults under it.
        let user_layers = merge(merge(global.clone(), group_defaults), Value::Object((*profile).clone()));
        let effective = merge(builtin.clone(), user_layers.clone());
        let options = effective.get("options");

        let address = options.and_then(|o| text_of(o.get("host"))).map(str::trim).unwrap_or("").to_string();
        let port = options.and_then(|o| number(o.get("port"))).and_then(|p| u16::try_from(p).ok()).filter(|p| *p != 0).unwrap_or(22);
        let user = options.and_then(|o| text_of(o.get("user"))).map(str::trim).filter(|u| !u.is_empty()).map(str::to_string);
        let name = text_of(profile.get("name")).map(str::trim).filter(|n| !n.is_empty()).map(str::to_string);
        if address.is_empty() {
            notes.push(format!("\"{}\" has no host name in Tabby and is left out.", name.as_deref().unwrap_or("A profile")));
            continue;
        }
        let name = name.unwrap_or_else(|| {
            let user = user.as_deref().map(|u| format!("{u}@")).unwrap_or_default();
            if port == 22 { format!("{user}{address}") } else { format!("{user}{address}:{port}") }
        });
        let host = FoundHost {
            name,
            address,
            port,
            user,
            icon: text_of(profile.get("icon")).and_then(|i| lucide_icon(i, is_icon)),
            left_out: left_out(&user_layers, &builtin, &profiles),
        };
        // A group id Tabby doesn't know is shown by Tabby as a group of that name (files from
        // before groups had ids name them that way).
        let key = if group_id.is_empty() { String::new() } else { group_id.to_string() };
        match found_groups.iter_mut().find(|(id, _)| *id == key) {
            Some((_, group)) => group.hosts.push(host),
            None => {
                let name = if key.is_empty() { UNGROUPED.to_string() } else { key.clone() };
                found_groups.push((key, FoundGroup { name, icon: None, default_user: global_user.clone(), hosts: vec![host] }));
            }
        }
    }
    // Profiles in no group come last, as in Tabby.
    if let Some(i) = found_groups.iter().position(|(id, _)| id.is_empty()) {
        let ungrouped = found_groups.remove(i);
        found_groups.push(ungrouped);
    }
    if !other_types.is_empty() {
        let total: usize = other_types.iter().map(|(_, n)| n).sum();
        let kinds: Vec<String> = other_types.iter().map(|(k, n)| format!("{n} {}", kind_label(k))).collect();
        let profile = if total == 1 { "profile stays" } else { "profiles stay" };
        notes.insert(0, format!("Komando imports SSH profiles only: {} {profile} in Tabby.", join_and(&kinds)));
    }
    Ok(Found {
        groups: found_groups.into_iter().map(|(_, g)| g).filter(|g| !g.hosts.is_empty()).collect(),
        notes,
    })
}

/// Groups in tree order (parents first, children right after, otherwise as listed), each with
/// its name and the names of the groups it sits in: "Atlas / Production".
fn group_order(groups: &[&Map<String, Value>]) -> Vec<(String, String)> {
    let id_of = |g: &Map<String, Value>| text_of(g.get("id")).unwrap_or("").to_string();
    let parent_of = |g: &Map<String, Value>| {
        text_of(g.get("parentGroupId")).filter(|p| groups.iter().any(|o| text_of(o.get("id")) == Some(*p))).map(str::to_string)
    };
    let mut ordered: Vec<(String, String)> = vec![];
    fn visit(
        id: &str,
        path: &str,
        groups: &[&Map<String, Value>],
        parent_of: &dyn Fn(&Map<String, Value>) -> Option<String>,
        ordered: &mut Vec<(String, String)>,
    ) {
        // A group listed twice, or a loop of parents, is visited once.
        if ordered.iter().any(|(seen, _)| seen == id) || ordered.len() > groups.len() {
            return;
        }
        let Some(group) = groups.iter().find(|g| text_of(g.get("id")) == Some(id)) else { return };
        let name = text_of(group.get("name")).map(str::trim).filter(|n| !n.is_empty()).unwrap_or(id);
        let path = if path.is_empty() { name.to_string() } else { format!("{path} / {name}") };
        ordered.push((id.to_string(), path.clone()));
        for child in groups.iter().filter(|g| parent_of(g).as_deref() == Some(id)) {
            visit(text_of(child.get("id")).unwrap_or(""), &path, groups, parent_of, ordered);
        }
    }
    for group in groups.iter().filter(|g| parent_of(g).is_none()) {
        visit(&id_of(group), "", groups, &parent_of, &mut ordered);
    }
    // Groups only reachable through a loop of parents.
    for group in groups {
        visit(&id_of(group), "", groups, &parent_of, &mut ordered);
    }
    ordered.retain(|(id, _)| !id.is_empty());
    ordered
}

/// The settings the user set in Tabby (`user`: their layers merged) that change how they
/// connect or work in the terminal but aren't imported. Settings left at Tabby's defaults
/// aren't listed, nor ones that only change Tabby's own look and tabs.
fn left_out(user: &Value, builtin: &Value, profiles: &[&Map<String, Value>]) -> Vec<LeftOut> {
    let set = |path: &[&str]| set_value(user, builtin, path);
    let opt = |key: &str| set(&["options", key]);
    let mut out = vec![];

    if let Some(jump) = opt("jumpHost").and_then(|v| text_of(Some(v))) {
        let name = profiles
            .iter()
            .find(|p| text_of(p.get("id")) == Some(jump))
            .and_then(|p| text_of(p.get("name")))
            .unwrap_or(jump);
        out.push(LeftOut::new("Jump host", format!("via {name}")));
    }
    if let Some(steps) = opt("scripts").and_then(Value::as_array) {
        out.push(LeftOut::new("Login script", plural(steps.len(), "step")));
    }
    if let Some(ports) = opt("forwardedPorts").and_then(Value::as_array) {
        let shown: Vec<String> = ports.iter().take(2).filter_map(forward_label).collect();
        let more = ports.len().saturating_sub(shown.len());
        let detail = match (shown.is_empty(), more) {
            (true, _) => plural(ports.len(), "port"),
            (false, 0) => shown.join(", "),
            (false, n) => format!("{} and {n} more", shown.join(", ")),
        };
        out.push(LeftOut::new("Port forwarding", detail));
    }
    let keys: Vec<String> =
        opt("privateKeys").and_then(Value::as_array).into_iter().flatten().filter_map(|k| text_of(Some(k))).map(key_label).collect();
    if !keys.is_empty() {
        out.push(LeftOut::new(if keys.len() == 1 { "Private key" } else { "Private keys" }, keys.join(", ")));
    } else if opt("auth").and_then(|v| text_of(Some(v))) == Some("agent") {
        out.push(LeftOut::new("Sign-in", "SSH agent"));
    }
    if opt("password").and_then(|v| text_of(Some(v))).is_some() {
        out.push(LeftOut::new("Password", "saved in Tabby"));
    }
    if let Some(command) = opt("proxyCommand").and_then(|v| text_of(Some(v))) {
        out.push(LeftOut::new("Proxy command", command));
    }
    for (setting, host, port) in [("SOCKS proxy", "socksProxyHost", "socksProxyPort"), ("HTTP proxy", "httpProxyHost", "httpProxyPort")] {
        if let Some(host) = opt(host).and_then(|v| text_of(Some(v))) {
            let detail = match number(user.get("options").and_then(|o| o.get(port))) {
                Some(port) => format!("{host}:{port}"),
                None => host.to_string(),
            };
            out.push(LeftOut::new(setting, detail));
        }
    }
    if opt("x11").is_some_and(|v| v.as_bool() == Some(true)) {
        out.push(LeftOut::new("X11 forwarding", "on"));
    }
    if opt("agentForward").is_some_and(|v| v.as_bool() == Some(true)) {
        out.push(LeftOut::new("Agent forwarding", "on"));
    }
    if let Some(cwd) = opt("cwd").and_then(|v| text_of(Some(v))) {
        out.push(LeftOut::new("Starting folder", cwd));
    }
    if let Some(term) = opt("term").and_then(|v| text_of(Some(v))) {
        out.push(LeftOut::new("Terminal type", term));
    }
    if let Some(backspace) = set(&["options", "input", "backspace"]).and_then(|v| text_of(Some(v))) {
        let detail = match backspace {
            "ctrl-h" => "sends Ctrl+H",
            "ctrl-?" => "sends Ctrl+?",
            "delete" => "sends Delete",
            other => other,
        };
        out.push(LeftOut::new("Backspace key", detail));
    }
    let interval = opt("keepaliveInterval").and_then(|v| number(Some(v)));
    let count = opt("keepaliveCountMax").and_then(|v| number(Some(v)));
    if interval.is_some() || count.is_some() {
        let options = merge(builtin.clone(), user.clone());
        let options = options.get("options");
        let interval = number(options.and_then(|o| o.get("keepaliveInterval"))).unwrap_or(5000);
        let count = number(options.and_then(|o| o.get("keepaliveCountMax"))).unwrap_or(10);
        let every = if interval == 0 { "off".to_string() } else { format!("every {} s, {}", interval / 1000, plural(count as usize, "try")) };
        out.push(LeftOut::new("Keep-alive", every));
    }
    if set(&["behaviorOnSessionEnd"]).and_then(|v| text_of(Some(v))) == Some("reconnect") {
        out.push(LeftOut::new("When the session ends", "reconnect"));
    }
    out
}

/// "Local 8080 → db.lan:5432".
fn forward_label(port: &Value) -> Option<String> {
    let kind = text_of(port.get("type")).unwrap_or("Local");
    let listen = number(port.get("port"))?;
    match kind {
        "Dynamic" => Some(format!("SOCKS on {listen}")),
        _ => {
            let target = text_of(port.get("targetAddress")).unwrap_or("?");
            let target_port = number(port.get("targetPort")).map(|p| format!(":{p}")).unwrap_or_default();
            Some(format!("{kind} {listen} → {target}{target_port}"))
        }
    }
}

/// A private key's file name; Tabby lists them as `file://` paths or vault references.
fn key_label(key: &str) -> String {
    if key.starts_with("vault://") {
        return "one in Tabby's vault".into();
    }
    let path = key.strip_prefix("file://").unwrap_or(key);
    path.rsplit(['/', '\\']).find(|s| !s.is_empty()).unwrap_or(path).to_string()
}

fn kind_label(kind: &str) -> String {
    match kind {
        "local" => "local shell".into(),
        "serial" => "serial".into(),
        "telnet" => "Telnet".into(),
        "split-layout" => "split layout".into(),
        "" => "unknown".into(),
        other => other.into(),
    }
}

/// A Tabby icon ("fas fa-server", "fab fa-ubuntu") as a Lucide icon name, if it has one.
pub fn lucide_icon(icon: &str, is_icon: &dyn Fn(&str) -> bool) -> Option<String> {
    // Custom SVG icons and anything else that isn't a Font Awesome class.
    if icon.trim_start().starts_with('<') {
        return None;
    }
    const MODIFIERS: &[&str] = &["fw", "lg", "xs", "sm", "xl", "2x", "3x", "4x", "5x", "spin", "pulse", "inverse", "border"];
    let name = icon
        .split_whitespace()
        .filter_map(|class| class.strip_prefix("fa-"))
        .find(|name| !MODIFIERS.contains(name))?
        .to_lowercase();
    let mapped = ICON_MAP.iter().find(|(fa, _)| *fa == name).map(|(_, lucide)| *lucide);
    match mapped {
        Some(lucide) => Some(lucide.to_string()),
        None if is_icon(&name) => Some(name),
        None => None,
    }
}

/// Font Awesome names (as Tabby offers them) with a different Lucide name. Names that are the
/// same in both ("server", "database") need no entry. Brand logos have no Lucide icon and
/// become a server or the closest thing.
pub const ICON_MAP: &[(&str, &str)] = &[
    ("desktop", "monitor"),
    ("tv", "tv"),
    ("home", "house"),
    ("network-wired", "network"),
    ("sitemap", "network"),
    ("project-diagram", "workflow"),
    ("ethernet", "ethernet-port"),
    ("key", "key-round"),
    ("shield-alt", "shield"),
    ("hdd", "hard-drive"),
    ("microchip", "cpu"),
    ("memory", "memory-stick"),
    ("code-branch", "git-branch"),
    ("fire", "flame"),
    ("fire-alt", "flame"),
    ("building", "building-2"),
    ("industry", "factory"),
    ("flask", "flask-conical"),
    ("tools", "wrench"),
    ("bolt", "zap"),
    ("broadcast-tower", "radio-tower"),
    ("cube", "box"),
    ("cubes", "boxes"),
    ("envelope", "mail"),
    ("heartbeat", "heart-pulse"),
    ("stream", "activity"),
    ("mobile", "smartphone"),
    ("mobile-alt", "smartphone"),
    ("gamepad", "gamepad-2"),
    ("print", "printer"),
    ("cogs", "cog"),
    ("robot", "bot"),
    ("globe-americas", "globe"),
    ("globe-europe", "globe"),
    ("globe-asia", "globe"),
    ("cloud-upload-alt", "cloud-upload"),
    ("cloud-download-alt", "cloud-download"),
    ("terminal", "terminal"),
    ("docker", "container"),
    ("raspberry-pi", "cpu"),
    ("aws", "cloud"),
    ("google", "cloud"),
    ("microsoft", "monitor"),
    ("windows", "monitor"),
    ("apple", "laptop"),
    ("linux", "server"),
    ("ubuntu", "server"),
    ("debian", "server"),
    ("redhat", "server"),
    ("centos", "server"),
    ("fedora", "server"),
    ("suse", "server"),
    ("freebsd", "server"),
];

// ---- small helpers on parsed YAML -----------------------------------------------------------

fn list(value: Option<&Value>) -> impl Iterator<Item = &Value> {
    value.and_then(Value::as_array).into_iter().flatten()
}

fn text_of(value: Option<&Value>) -> Option<&str> {
    value.and_then(Value::as_str).filter(|s| !s.trim().is_empty())
}

fn number(value: Option<&Value>) -> Option<u64> {
    match value? {
        Value::Number(n) => n.as_u64(),
        Value::String(s) => s.trim().parse().ok(),
        _ => None,
    }
}

fn flag(value: Option<&Value>) -> bool {
    value.and_then(Value::as_bool).unwrap_or(false)
}

/// Tabby's `configMerge`: objects merge key by key, anything else (lists too) is replaced.
fn merge(base: Value, over: Value) -> Value {
    match (base, over) {
        (Value::Object(mut base), Value::Object(over)) => {
            for (key, value) in over {
                let merged = match base.remove(&key) {
                    Some(old) => merge(old, value),
                    None => value,
                };
                base.insert(key, merged);
            }
            Value::Object(base)
        }
        (base, Value::Null) => base,
        (_, over) => over,
    }
}

fn at<'a>(value: &'a Value, path: &[&str]) -> Option<&'a Value> {
    path.iter().try_fold(value, |v, key| v.get(key))
}

/// The value at `path` if the user set it to something other than Tabby's default. Empty
/// text and empty lists count as not set.
fn set_value<'a>(user: &'a Value, builtin: &Value, path: &[&str]) -> Option<&'a Value> {
    let value = at(user, path)?;
    let empty = match value {
        Value::Null => true,
        Value::String(s) => s.trim().is_empty(),
        Value::Array(a) => a.is_empty(),
        _ => false,
    };
    if empty || at(builtin, path) == Some(value) {
        return None;
    }
    Some(value)
}

fn plural(n: usize, word: &str) -> String {
    match (n, word) {
        (1, _) => format!("1 {word}"),
        (_, "try") => format!("{n} tries"),
        _ => format!("{n} {word}s"),
    }
}

fn join_and(items: &[String]) -> String {
    match items {
        [] => String::new(),
        [one] => one.clone(),
        [rest @ .., last] => format!("{} and {last}", rest.join(", ")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn any_icon(name: &str) -> bool {
        ["server", "database", "globe", "monitor"].contains(&name)
    }

    fn parse_ok(text: &str) -> Found {
        parse(text, &any_icon).unwrap()
    }

    /// A configuration as Tabby 1.0 writes it: nested groups, group and global defaults,
    /// and profiles of several kinds.
    const CONFIG: &str = r#"
version: 8
profiles:
  - type: ssh
    name: web01
    icon: fas fa-desktop
    color: '#ff0000'
    options:
      host: web01.atlas.lan
      jumpHost: ssh:custom:bastion
      scripts:
        - expect: 'password:'
          send: hunter2
      forwardedPorts:
        - type: Local
          host: 127.0.0.1
          port: 8080
          targetAddress: db.lan
          targetPort: 5432
          description: ''
    group: g-prod
    id: ssh:custom:web01
  - type: ssh
    name: bastion
    options:
      host: bastion.example.com
      port: 2222
      user: ops
      privateKeys:
        - file://C:\Users\me\.ssh\id_ed25519
      x11: false
      term: xterm-256color
    group: g-atlas
    id: ssh:custom:bastion
    clearServiceMessagesOnConnect: false
    disableDynamicTitle: true
  - type: ssh
    name: ''
    options:
      host: 10.0.0.9
      port: 2200
      input:
        backspace: ctrl-h
      keepaliveInterval: 30000
    id: ssh:custom:anon
  - type: ssh
    name: nowhere
    options:
      host: ''
    id: ssh:custom:nowhere
  - type: ssh
    name: legacy
    group: Old servers
    icon: fab fa-ubuntu
    options:
      host: legacy.lan
    id: ssh:custom:legacy
  - type: local
    name: PowerShell
    options: {}
  - type: local
    name: cmd
    options: {}
  - type: serial
    name: console
    options: {}
groups:
  - id: g-prod
    name: Production
    parentGroupId: g-atlas
    icon: fas fa-database
    defaults:
      ssh:
        options:
          user: deploy
  - id: g-atlas
    name: Atlas
  - id: g-empty
    name: Nothing here
profileDefaults:
  ssh:
    disableDynamicTitle: true
    options:
      user: admin
"#;

    #[test]
    fn groups_become_projects_with_their_parents_names_and_ungrouped_last() {
        let found = parse_ok(CONFIG);
        let names: Vec<&str> = found.groups.iter().map(|g| g.name.as_str()).collect();
        assert_eq!(names, ["Atlas", "Atlas / Production", "Old servers", UNGROUPED]);
        let prod = &found.groups[1];
        assert_eq!(prod.icon.as_deref(), Some("database"));
        assert_eq!(prod.default_user.as_deref(), Some("deploy"));
        assert_eq!(found.groups[0].default_user.as_deref(), Some("admin"), "the global default");
        assert_eq!(found.groups[3].default_user.as_deref(), Some("admin"), "ungrouped too");
    }

    #[test]
    fn hosts_take_name_address_port_user_and_icon_through_the_defaults() {
        let found = parse_ok(CONFIG);
        let web = &found.groups[1].hosts[0];
        assert_eq!((web.name.as_str(), web.address.as_str(), web.port), ("web01", "web01.atlas.lan", 22));
        assert_eq!(web.user.as_deref(), Some("deploy"), "from its group");
        assert_eq!(web.icon.as_deref(), Some("monitor"));
        let bastion = &found.groups[0].hosts[0];
        assert_eq!((bastion.port, bastion.user.as_deref()), (2222, Some("ops")));
        assert_eq!(bastion.icon, None);
        let anon = &found.groups[3].hosts[0];
        assert_eq!(anon.name, "admin@10.0.0.9:2200", "named like Tabby names it");
        assert_eq!(found.groups[2].hosts[0].icon.as_deref(), Some("server"));
    }

    #[test]
    fn settings_the_user_set_are_listed_as_left_out() {
        let found = parse_ok(CONFIG);
        let web = &found.groups[1].hosts[0];
        assert_eq!(
            web.left_out,
            [
                LeftOut::new("Jump host", "via bastion"),
                LeftOut::new("Login script", "1 step"),
                LeftOut::new("Port forwarding", "Local 8080 → db.lan:5432"),
            ]
        );
        // Defaults written out (x11: false, the default term) and Tabby-only settings
        // (clearServiceMessagesOnConnect, disableDynamicTitle) are not listed.
        let bastion = &found.groups[0].hosts[0];
        assert_eq!(bastion.left_out, [LeftOut::new("Private key", "id_ed25519")]);
        let anon = &found.groups[3].hosts[0];
        assert_eq!(
            anon.left_out,
            [LeftOut::new("Backspace key", "sends Ctrl+H"), LeftOut::new("Keep-alive", "every 30 s, 10 tries")]
        );
        assert!(found.groups[2].hosts[0].left_out.is_empty());
    }

    #[test]
    fn other_profiles_and_ones_without_an_address_are_noted() {
        let found = parse_ok(CONFIG);
        assert_eq!(found.host_count(), 4);
        assert_eq!(
            found.notes,
            [
                "Komando imports SSH profiles only: 2 local shell and 1 serial profiles stay in Tabby.",
                "\"nowhere\" has no host name in Tabby and is left out.",
            ]
        );
    }

    #[test]
    fn an_encrypted_configuration_says_how_to_read_it() {
        let err = parse("encrypted: true\nvault:\n  contents: abc\n", &any_icon).unwrap_err().to_string();
        assert!(err.contains("Encrypt config file"), "{err}");
    }

    #[test]
    fn other_files_are_refused() {
        assert!(parse("just: text\n", &any_icon).is_err());
        assert!(parse("- a\n- b\n", &any_icon).is_err());
        assert!(parse("key: [unclosed\n", &any_icon).is_err());
        let empty = parse("version: 8\nprofiles: []\n", &any_icon).unwrap();
        assert!(empty.groups.is_empty() && empty.notes.is_empty());
    }

    #[test]
    fn a_loop_of_parent_groups_still_lists_each_group_once() {
        let text = r#"
profiles:
  - { type: ssh, name: a, group: a, options: { host: a.lan } }
  - { type: ssh, name: b, group: b, options: { host: b.lan } }
groups:
  - { id: a, name: A, parentGroupId: b }
  - { id: b, name: B, parentGroupId: a }
"#;
        let found = parse_ok(text);
        assert_eq!(found.groups.len(), 2);
        assert_eq!(found.host_count(), 2);
    }

    #[test]
    fn font_awesome_icons_map_to_lucide() {
        assert_eq!(lucide_icon("fas fa-desktop", &any_icon).as_deref(), Some("monitor"));
        assert_eq!(lucide_icon("fas fa-fw fa-server", &any_icon).as_deref(), Some("server"));
        assert_eq!(lucide_icon("fab fa-docker", &any_icon).as_deref(), Some("container"));
        assert_eq!(lucide_icon("fas fa-unknown-thing", &any_icon), None);
        assert_eq!(lucide_icon("<svg></svg>", &any_icon), None);
        assert_eq!(lucide_icon("", &any_icon), None);
    }

    #[test]
    fn a_picked_folder_means_the_config_inside_it() {
        let dir = std::env::temp_dir().join(format!("komando-tabby-{}", crate::model::new_id()));
        std::fs::create_dir_all(dir.join("data")).unwrap();
        assert!(config_in(&dir).is_err());
        std::fs::write(dir.join("data").join(CONFIG_FILE), "version: 8\n").unwrap();
        assert_eq!(config_in(&dir).unwrap(), dir.join("data").join(CONFIG_FILE));
        std::fs::write(dir.join(CONFIG_FILE), "version: 8\n").unwrap();
        assert_eq!(config_in(&dir).unwrap(), dir.join(CONFIG_FILE));
        assert_eq!(config_in(&dir.join(CONFIG_FILE)).unwrap(), dir.join(CONFIG_FILE));
        std::fs::remove_dir_all(dir).ok();
    }
}
