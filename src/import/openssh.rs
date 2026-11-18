//! Reading Hosts from OpenSSH's client configuration, `~/.ssh/config`.
//!
//! Each `Host` block that names a host (not only patterns such as `*.lan`) becomes one host,
//! named after its first such name. Its settings are what `ssh` would use for that name: every
//! block whose patterns match it, in file order, and for each keyword the first value found
//! (see ssh_config(5)), so `Host *` defaults at the end fill in what a host leaves out. `Include`
//! is followed, with `*` and `?` in file names, relative to `~/.ssh`.
//!
//! Komando takes over the name, `HostName` (with `%h`), `Port`, `User`, `ProxyJump` (as Jump
//! Hosts, each looked up in the same file, as `ssh` does) and `LocalForward`, `RemoteForward`
//! and `DynamicForward` (as Port Forwards). Settings that change how `ssh` connects but don't
//! come over are listed per host, such as `IdentityFile` (keys stay where they are, see
//! docs/adr/0016-importing-from-the-ssh-config.md), `ProxyCommand` and `ForwardAgent`.
//! `Match` blocks count when they depend only on the name (`all`, `host`, `originalhost`,
//! `final`); one that depends on the user, a command or the network is left out with a note.

use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result};

use super::{Found, FoundGroup, FoundHost, LeftOut, join_and, key_file_name};
use crate::model::{ForwardKind, JumpHost, PortForward};

/// The project the hosts go into.
pub const GROUP: &str = "SSH config";

/// How deep `Include` and `ProxyJump` are followed, as a guard against loops.
const MAX_DEPTH: usize = 16;

/// `~/.ssh`, where the configuration lives and relative `Include`s start.
pub fn ssh_dir() -> Option<PathBuf> {
    dirs::home_dir().map(|home| home.join(".ssh"))
}

/// The user's SSH configuration, if there is one.
pub fn find_config() -> Option<PathBuf> {
    ssh_dir().map(|dir| dir.join("config")).filter(|path| path.is_file())
}

/// Read the configuration at `path` and the files it includes. Relative `Include`s start in
/// `~/.ssh`, as they do for `ssh`, or next to `path` when there is no home folder.
pub fn read(path: &Path) -> Result<Found> {
    let text = std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    let base = ssh_dir().unwrap_or_else(|| path.parent().map(Path::to_path_buf).unwrap_or_default());
    let home = dirs::home_dir();
    let mut lines = vec![];
    expand(&text, &base, home.as_deref(), 0, None, &mut lines);
    Ok(found(&lines))
}

/// A keyword and its arguments, keyword in lower case.
#[derive(Debug, Clone, PartialEq)]
struct Line {
    key: String,
    args: Vec<String>,
}

/// Split one line into keyword and arguments: `Key value`, `Key=value` or `Key = value`, with
/// double quotes around arguments that have spaces. None for blank lines and comments.
fn parse_line(raw: &str) -> Option<Line> {
    let line = raw.trim();
    if line.is_empty() || line.starts_with('#') {
        return None;
    }
    let end = line.find(|c: char| c.is_whitespace() || c == '=').unwrap_or(line.len());
    let key = line[..end].to_ascii_lowercase();
    let rest = line[end..].trim_start();
    // One `=` may stand between keyword and value.
    let rest = rest.strip_prefix('=').unwrap_or(rest).trim_start();
    let mut args = vec![];
    let mut current = String::new();
    let (mut quoted, mut started) = (false, false);
    for c in rest.chars() {
        match c {
            '"' => {
                quoted = !quoted;
                started = true;
            }
            c if c.is_whitespace() && !quoted => {
                if started {
                    args.push(std::mem::take(&mut current));
                    started = false;
                }
            }
            // A comment after the arguments.
            '#' if !quoted && !started => break,
            c => {
                current.push(c);
                started = true;
            }
        }
    }
    if started {
        args.push(current);
    }
    Some(Line { key, args })
}

/// The lines of `text` with every `Include` replaced by the lines of the files it names. A
/// file starts in the block its `Include` sits in (`outer`; `None` before any `Host` line, where
/// lines apply to every host), and a `Host` or `Match` line in it changes the block only until
/// the file ends, as in `ssh`: the block around the `Include` goes on after it.
fn expand(text: &str, base: &Path, home: Option<&Path>, depth: usize, outer: Option<&Line>, out: &mut Vec<Line>) {
    let mut block: Option<Line> = outer.cloned();
    for line in text.lines().filter_map(parse_line) {
        match line.key.as_str() {
            "host" | "match" => {
                block = Some(line.clone());
                out.push(line);
            }
            "include" if depth < MAX_DEPTH => {
                let start = out.len();
                for pattern in &line.args {
                    for file in include_files(pattern, base, home) {
                        if let Ok(text) = std::fs::read_to_string(&file) {
                            expand(&text, base, home, depth + 1, block.as_ref(), out);
                        }
                    }
                }
                // Back to the block the Include sat in, if the included files left it.
                let changed = out[start..].iter().any(|l| l.key == "host" || l.key == "match");
                match &block {
                    Some(block) if changed => out.push(block.clone()),
                    // Before any Host line, settings apply to every host.
                    None if changed => out.push(Line { key: "host".into(), args: vec!["*".into()] }),
                    _ => {}
                }
            }
            _ => out.push(line),
        }
    }
}

/// The files an `Include` names: `~` is the home folder, a relative path starts in `base`,
/// and `*` or `?` in the file name match files in that folder, in name order.
fn include_files(pattern: &str, base: &Path, home: Option<&Path>) -> Vec<PathBuf> {
    let path = match (pattern.strip_prefix("~/").or_else(|| pattern.strip_prefix("~\\")), home) {
        (Some(rest), Some(home)) => home.join(rest),
        _ if Path::new(pattern).is_absolute() => PathBuf::from(pattern),
        _ => base.join(pattern),
    };
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    if !name.contains(['*', '?']) {
        return vec![path];
    }
    let Some(dir) = path.parent() else { return vec![] };
    let mut files: Vec<PathBuf> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|entry| entry.file_type().is_ok_and(|t| t.is_file()))
        // As glob(3) does, a wildcard doesn't match a leading dot: editors' `.config.swp`.
        .filter(|entry| {
            let file = entry.file_name().to_string_lossy().into_owned();
            (!file.starts_with('.') || name.starts_with('.')) && wildcard(&name, &file, false)
        })
        .map(|entry| entry.path())
        .collect();
    files.sort();
    files
}

/// Whether `text` matches `pattern`, where `*` is any run of characters and `?` any one.
fn wildcard(pattern: &str, text: &str, ignore_case: bool) -> bool {
    let (p, t): (Vec<char>, Vec<char>) = if ignore_case {
        (pattern.to_lowercase().chars().collect(), text.to_lowercase().chars().collect())
    } else {
        (pattern.chars().collect(), text.chars().collect())
    };
    let (mut pi, mut ti) = (0, 0);
    let mut star: Option<(usize, usize)> = None;
    while ti < t.len() {
        if pi < p.len() && (p[pi] == '?' || p[pi] == t[ti]) {
            pi += 1;
            ti += 1;
        } else if pi < p.len() && p[pi] == '*' {
            star = Some((pi, ti));
            pi += 1;
        } else if let Some((sp, st)) = star {
            pi = sp + 1;
            ti = st + 1;
            star = Some((sp, st + 1));
        } else {
            return false;
        }
    }
    p[pi..].iter().all(|c| *c == '*')
}

/// Whether a `Host` line's patterns take `name`: one of them matches and none of its negated
/// (`!`) ones does.
fn host_matches(patterns: &[String], name: &str) -> bool {
    let mut matched = false;
    for pattern in patterns {
        match pattern.strip_prefix('!') {
            Some(negated) if wildcard(negated, name, true) => return false,
            Some(_) => {}
            None => matched |= wildcard(pattern, name, true),
        }
    }
    matched
}

/// One criterion of a `Match` line Komando can check before connecting.
#[derive(Debug, PartialEq)]
struct Criterion {
    negated: bool,
    kind: CriterionKind,
}

#[derive(Debug, PartialEq)]
enum CriterionKind {
    /// `all`, and `final` (the last pass, which reading the whole file is).
    Always,
    /// `canonical`: Komando doesn't canonicalize host names, so never.
    Never,
    /// `host`: the patterns, against the host name found so far.
    Host(Vec<String>),
    /// `originalhost`: the patterns, against the name as typed.
    OriginalHost(Vec<String>),
}

impl Criterion {
    fn holds(&self, name: &str, host_name: &str) -> bool {
        let held = match &self.kind {
            CriterionKind::Always => true,
            CriterionKind::Never => false,
            CriterionKind::Host(patterns) => host_matches(patterns, host_name),
            CriterionKind::OriginalHost(patterns) => host_matches(patterns, name),
        };
        held != self.negated
    }
}

/// A `Match` line's criteria, all of which must hold. None when one depends on what only
/// connecting tells (`user`, `localuser`, `exec`, `localnetwork`, `tagged` and the like).
fn match_criteria(args: &[String]) -> Option<Vec<Criterion>> {
    let mut criteria = vec![];
    let mut args = args.iter();
    while let Some(arg) = args.next() {
        let (negated, keyword) = match arg.strip_prefix('!') {
            Some(keyword) => (true, keyword.to_ascii_lowercase()),
            None => (false, arg.to_ascii_lowercase()),
        };
        let patterns = |list: &String| list.split(',').map(str::to_string).collect::<Vec<_>>();
        let kind = match keyword.as_str() {
            "all" | "final" => CriterionKind::Always,
            "canonical" => CriterionKind::Never,
            "host" => CriterionKind::Host(patterns(args.next()?)),
            "originalhost" => CriterionKind::OriginalHost(patterns(args.next()?)),
            _ => return None,
        };
        criteria.push(Criterion { negated, kind });
    }
    (!criteria.is_empty()).then_some(criteria)
}

fn is_pattern(name: &str) -> bool {
    name.starts_with('!') || name.contains(['*', '?'])
}

/// One host's settings: for each keyword the first value, except those `ssh` collects from
/// every matching block.
#[derive(Default)]
struct Settings {
    values: Vec<(String, Vec<String>)>,
}

/// Keywords whose every occurrence counts, not only the first.
const MULTIPLE: [&str; 6] = ["localforward", "remoteforward", "dynamicforward", "identityfile", "certificatefile", "sendenv"];

impl Settings {
    /// What `ssh` uses for `name`: the lines before the first `Host` or `Match` and those of
    /// every `Host` block that matches it. `Match` blocks are skipped.
    fn for_host(lines: &[Line], name: &str) -> Self {
        let mut settings = Settings::default();
        let mut applies = true;
        for line in lines {
            match line.key.as_str() {
                "host" => applies = host_matches(&line.args, name),
                "match" => {
                    let host_name = settings.first("hostname").and_then(|h| expand_host_name(h, name)).unwrap_or_else(|| name.to_string());
                    applies = match_criteria(&line.args).is_some_and(|criteria| criteria.iter().all(|c| c.holds(name, &host_name)));
                }
                key if applies && (MULTIPLE.contains(&key) || !settings.values.iter().any(|(k, _)| k == key)) => {
                    settings.values.push((key.to_string(), line.args.clone()));
                }
                _ => {}
            }
        }
        settings
    }

    fn first(&self, key: &str) -> Option<&str> {
        self.values.iter().find(|(k, _)| k == key).and_then(|(_, args)| args.first()).map(String::as_str)
    }

    fn all<'a>(&'a self, key: &'a str) -> impl Iterator<Item = &'a [String]> + 'a {
        self.values.iter().filter(move |(k, _)| k == key).map(|(_, args)| args.as_slice())
    }

    fn is_yes(&self, key: &str) -> bool {
        self.first(key).is_some_and(|v| v.eq_ignore_ascii_case("yes"))
    }
}

/// Where a name leads: its `HostName` (with `%h` for the name itself), port and user.
fn address_of(settings: &Settings, name: &str) -> (String, u16, Option<String>) {
    let address = settings.first("hostname").and_then(|h| expand_host_name(h, name)).unwrap_or_else(|| name.to_string());
    let port = settings.first("port").and_then(|p| p.parse::<u16>().ok()).filter(|p| *p != 0).unwrap_or(22);
    let user = settings.first("user").filter(|u| !u.contains('%')).map(str::to_string);
    (address, port, user)
}

/// A `HostName` with its tokens replaced: `%h` is the name it is looked up by and `%%` a
/// percent sign, read left to right. None if it has another token, which `ssh` doesn't take
/// there either.
fn expand_host_name(value: &str, name: &str) -> Option<String> {
    let mut out = String::new();
    let mut chars = value.chars();
    while let Some(c) = chars.next() {
        match c {
            '%' => match chars.next()? {
                'h' => out.push_str(name),
                '%' => out.push('%'),
                _ => return None,
            },
            c => out.push(c),
        }
    }
    Some(out)
}

/// `[user@]host[:port]` or `ssh://[user@]host[:port]`, IPv6 addresses in brackets.
fn parse_jump(text: &str) -> Option<(Option<String>, String, Option<u16>)> {
    let text = text.strip_prefix("ssh://").unwrap_or(text);
    let (user, rest) = match text.rsplit_once('@') {
        Some((user, rest)) => (Some(user.to_string()), rest),
        None => (None, text),
    };
    let (host, port) = match rest.strip_prefix('[') {
        Some(bracketed) => {
            let (host, after) = bracketed.split_once(']')?;
            (host, after.strip_prefix(':'))
        }
        None if rest.matches(':').count() == 1 => {
            let (host, port) = rest.split_once(':')?;
            (host, Some(port))
        }
        None => (rest, None),
    };
    let port = match port {
        Some(port) => Some(port.parse::<u16>().ok().filter(|p| *p != 0)?),
        None => None,
    };
    (!host.is_empty()).then(|| (user, host.to_string(), port))
}

/// The Jump Hosts on the way to a host whose `ProxyJump` is `jumps`, each looked up in the
/// file as `ssh -J` does. The first one's own `ProxyJump` comes before it; the later ones are
/// reached through the one before them, which overrides theirs. None if one can't be read.
fn route(lines: &[Line], jumps: &str, depth: usize, left: &mut Vec<LeftOut>) -> Option<Vec<JumpHost>> {
    if jumps.eq_ignore_ascii_case("none") {
        return Some(vec![]);
    }
    if depth >= MAX_DEPTH {
        return None;
    }
    let mut route = vec![];
    for (i, jump) in jumps.split(',').enumerate() {
        let (user, name, port) = parse_jump(jump.trim())?;
        let settings = Settings::for_host(lines, &name);
        if let Some(before) = settings.first("proxyjump").filter(|_| i == 0) {
            route.extend(self::route(lines, before, depth + 1, left)?);
        }
        let (address, config_port, config_user) = address_of(&settings, &name);
        // What signs in to the hop, or reaches it, but doesn't come over with it.
        for hop in hop_left_out(&settings) {
            let entry = LeftOut::new(format!("{} of jump host {name}", hop.setting), hop.detail);
            if !left.contains(&entry) {
                left.push(entry);
            }
        }
        route.push(JumpHost { address, port: port.unwrap_or(config_port), user: user.or(config_user), credential: None });
    }
    Some(route)
}

/// What of a Jump Host's own settings matters for signing in to it or reaching it, and
/// doesn't come over: its keys, certificate, proxy command, and a host name or user with
/// tokens.
fn hop_left_out(settings: &Settings) -> Vec<LeftOut> {
    const HOP: [&str; 6] = ["Private key", "Private keys", "Certificate", "Proxy command", "Host name", "User"];
    left_out(settings).into_iter().filter(|l| HOP.contains(&l.setting.as_str())).collect()
}

/// `[bind_address:]port`, the bind address `None` when not given. IPv6 addresses are in
/// brackets; `bind/port` is OpenSSH's other spelling.
fn parse_listen(text: &str) -> Option<(Option<String>, u16)> {
    if is_socket_path(text) {
        return None;
    }
    let (bind, port) = if let Some(rest) = text.strip_prefix('[') {
        let (bind, port) = rest.split_once("]:")?;
        (Some(bind), port)
    } else if let Some((bind, port)) = text.rsplit_once(['/', ':']) {
        (Some(bind), port)
    } else {
        (None, text)
    };
    let port = port.parse::<u16>().ok().filter(|p| *p != 0)?;
    Some((bind.map(str::to_string), port))
}

/// A Unix socket's path, which `ssh` tells from an address by its leading `/` or `~`.
fn is_socket_path(text: &str) -> bool {
    text.starts_with('/') || text.starts_with('~')
}

/// `host:port`, `[v6]:port` or `host/port`.
fn parse_target(text: &str) -> Option<(String, u16)> {
    if is_socket_path(text) {
        return None;
    }
    let (host, port) = if let Some(rest) = text.strip_prefix('[') {
        rest.split_once("]:")?
    } else {
        text.rsplit_once(['/', ':'])?
    };
    let port = port.parse::<u16>().ok().filter(|p| *p != 0)?;
    (!host.is_empty()).then(|| (host.to_string(), port))
}

/// The host's Port Forwards, and what of them doesn't come over: Unix sockets, a `RemoteForward`
/// without an address (a SOCKS proxy on the server), or a bind address other than loopback or
/// every interface (it comes over on loopback).
fn forwards(settings: &Settings) -> (Vec<PortForward>, Vec<LeftOut>) {
    let (mut forwards, mut left) = (vec![], vec![]);
    let gateway = settings.is_yes("gatewayports");
    let kinds = [("localforward", ForwardKind::Local), ("remoteforward", ForwardKind::Remote), ("dynamicforward", ForwardKind::Socks)];
    for (key, kind) in kinds {
        for args in settings.all(key) {
            let spec = args.join(" ");
            let listen = args.first().and_then(|a| parse_listen(a));
            let target = args.get(1).and_then(|a| parse_target(a));
            let forward = match (kind, listen, target) {
                (ForwardKind::Socks, Some((bind, port)), _) => (bind, PortForward::socks(port)),
                (ForwardKind::Socks, None, _) => {
                    left.push(LeftOut::new("Port forwarding", format!("DynamicForward {spec}")));
                    continue;
                }
                (_, Some((bind, port)), Some((to_host, to_port))) => {
                    (bind, PortForward { kind, port, to_host, to_port, open_to_network: false })
                }
                (ForwardKind::Remote, Some(_), None) if args.len() == 1 => {
                    left.push(LeftOut::new("Port forwarding", format!("RemoteForward {spec} (a SOCKS proxy on the server)")));
                    continue;
                }
                _ => {
                    let name = if kind == ForwardKind::Local { "LocalForward" } else { "RemoteForward" };
                    left.push(LeftOut::new("Port forwarding", format!("{name} {spec}")));
                    continue;
                }
            };
            let (bind, forward) = forward;
            let forward = match bind.as_deref() {
                // `ssh` binds a Local forward without an address to every interface when
                // GatewayPorts is on; a Remote one is up to the server.
                None => PortForward { open_to_network: gateway && kind.listens_here(), ..forward },
                Some("localhost" | "127.0.0.1" | "::1") => forward,
                Some("" | "*" | "0.0.0.0" | "::") => PortForward { open_to_network: true, ..forward },
                Some(address) => {
                    left.push(LeftOut::new("Listen address", format!("{address} for {}: only loopback for now", forward.label())));
                    forward
                }
            };
            forwards.push(forward);
        }
    }
    (forwards, left)
}

/// The settings that change how `ssh` connects or behaves once connected but don't come over.
fn left_out(settings: &Settings) -> Vec<LeftOut> {
    let mut out = vec![];
    let names = |key: &str| -> Vec<String> { settings.all(key).flatten().map(|path| key_file_name(path)).collect() };
    let keys = names("identityfile");
    if !keys.is_empty() {
        out.push(LeftOut::new(if keys.len() == 1 { "Private key" } else { "Private keys" }, join_and(&keys)));
    }
    let certificates = names("certificatefile");
    if !certificates.is_empty() {
        out.push(LeftOut::new("Certificate", join_and(&certificates)));
    }
    if let Some(command) = settings.first("proxycommand").filter(|c| !c.eq_ignore_ascii_case("none")) {
        let full = settings.values.iter().find(|(k, _)| k == "proxycommand").map(|(_, a)| a.join(" ")).unwrap_or_else(|| command.to_string());
        out.push(LeftOut::new("Proxy command", full));
    }
    if settings.is_yes("forwardagent") {
        out.push(LeftOut::new("Agent forwarding", "on"));
    }
    if settings.is_yes("forwardx11") || settings.is_yes("forwardx11trusted") {
        out.push(LeftOut::new("X11 forwarding", "on"));
    }
    for (key, setting) in [("remotecommand", "Remote command"), ("localcommand", "Local command"), ("setenv", "Environment")] {
        if let Some((_, args)) = settings.values.iter().find(|(k, _)| k == key).filter(|(_, a)| !a.is_empty()) {
            out.push(LeftOut::new(setting, args.join(" ")));
        }
    }
    if let Some(interval) = settings.first("serveraliveinterval").filter(|v| *v != "0") {
        out.push(LeftOut::new("Keep-alive", format!("every {interval} s")));
    }
    if let Some(host_name) = settings.first("hostname").filter(|h| expand_host_name(h, "").is_none()) {
        out.push(LeftOut::new("Host name", host_name));
    }
    if settings.first("user").is_some_and(|u| u.contains('%')) {
        out.push(LeftOut::new("User", settings.first("user").unwrap_or_default()));
    }
    out
}

/// The hosts of an expanded configuration, in file order.
fn found(lines: &[Line]) -> Found {
    let mut hosts: Vec<FoundHost> = vec![];
    let mut notes = vec![];
    for line in lines.iter().filter(|l| l.key == "host") {
        let Some(name) = line.args.iter().find(|n| !is_pattern(n)) else { continue };
        if hosts.iter().any(|h| h.name.eq_ignore_ascii_case(name)) {
            continue;
        }
        let settings = Settings::for_host(lines, name);
        let (address, port, user) = address_of(&settings, name);
        let mut left = left_out(&settings);
        let jump_hosts = match settings.first("proxyjump") {
            None => vec![],
            Some(jumps) => {
                let mut hops_left = vec![];
                match route(lines, jumps, 0, &mut hops_left) {
                    Some(route) => {
                        left.splice(0..0, hops_left);
                        route
                    }
                    None => {
                        left.insert(0, LeftOut::new("Jump host", format!("ProxyJump {jumps}")));
                        vec![]
                    }
                }
            }
        };
        let (port_forwards, forwards_left) = forwards(&settings);
        left.extend(forwards_left);
        hosts.push(FoundHost {
            name: name.clone(),
            address,
            port,
            user,
            jump_hosts: Some(jump_hosts),
            port_forwards: Some(port_forwards),
            left_out: left,
            ..Default::default()
        });
    }
    let unchecked = lines.iter().filter(|l| l.key == "match" && match_criteria(&l.args).is_none()).count();
    if unchecked > 0 {
        let blocks = if unchecked == 1 { "a Match block".to_string() } else { format!("{unchecked} Match blocks") };
        notes.push(format!(
            "The settings in {blocks} aren't imported: they depend on more than the host's name, such as the user, \
             a command or the network, which Komando can't check before connecting."
        ));
    }
    let groups = if hosts.is_empty() {
        vec![]
    } else {
        vec![FoundGroup { name: GROUP.into(), icon: Some("square-terminal".into()), hosts, ..Default::default() }]
    };
    Found { groups, notes }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(text: &str) -> Found {
        let mut lines = vec![];
        expand(text, Path::new("/nonexistent"), None, 0, None, &mut lines);
        found(&lines)
    }

    fn host<'a>(found: &'a Found, name: &str) -> &'a FoundHost {
        found.groups[0].hosts.iter().find(|h| h.name == name).unwrap_or_else(|| panic!("no host {name}"))
    }

    fn jump(user: Option<&str>, address: &str, port: u16) -> JumpHost {
        JumpHost { address: address.into(), port, user: user.map(Into::into), credential: None }
    }

    #[test]
    fn lines_split_like_ssh_splits_them() {
        let line = |text: &str| parse_line(text).unwrap();
        assert_eq!(line("  HostName web.lan").args, ["web.lan"]);
        assert_eq!(line("Port=2222"), Line { key: "port".into(), args: vec!["2222".into()] });
        assert_eq!(line("User = deploy").args, ["deploy"]);
        assert_eq!(line("IdentityFile \"~/My Keys/id\"  # work").args, ["~/My Keys/id"]);
        assert_eq!(line("LocalForward 5432 db:5432").args, ["5432", "db:5432"]);
        assert!(parse_line("   # a comment").is_none() && parse_line("").is_none());
    }

    #[test]
    fn host_patterns_match_like_ssh() {
        let patterns = |list: &[&str]| list.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert!(host_matches(&patterns(&["*.lan"]), "web.lan"));
        assert!(host_matches(&patterns(&["web?"]), "WEB1"));
        assert!(!host_matches(&patterns(&["*", "!bastion"]), "bastion"));
        assert!(host_matches(&patterns(&["*", "!bastion"]), "web"));
        assert!(!host_matches(&patterns(&["!bastion"]), "web"), "a negation alone matches nothing");
    }

    #[test]
    fn each_named_host_takes_the_first_value_from_every_block_that_matches() {
        let found = parse(
            "User global\n\
             Host web web.prod\n  HostName %h.atlas.lan\n  Port 2222\n\
             Host *.lan *.prod db\n  User deploy\n\
             Host db\n  HostName 10.0.0.5\n  User postgres\n\
             Host *\n  User fallback\n  Port 22\n",
        );
        let names: Vec<&str> = found.groups[0].hosts.iter().map(|h| h.name.as_str()).collect();
        assert_eq!(names, ["web", "db"], "patterns aren't hosts, and a block is one host");
        let web = host(&found, "web");
        assert_eq!((web.address.as_str(), web.port, web.user.as_deref()), ("web.atlas.lan", 2222, Some("global")));
        let db = host(&found, "db");
        assert_eq!((db.address.as_str(), db.port, db.user.as_deref()), ("10.0.0.5", 22, Some("global")));
        assert_eq!(found.groups[0].name, GROUP);
    }

    #[test]
    fn a_host_named_twice_comes_over_once_with_both_blocks() {
        let found = parse("Host web\n  HostName web.lan\nHost web\n  User ops\n");
        assert_eq!(found.groups[0].hosts.len(), 1);
        assert_eq!(host(&found, "web").user.as_deref(), Some("ops"));
    }

    #[test]
    fn proxy_jumps_become_jump_hosts_looked_up_in_the_file() {
        let found = parse(
            "Host bastion\n  HostName bastion.example.com\n  User jump\n  Port 2200\n\
             Host inner\n  HostName 10.1.0.1\n  ProxyJump bastion\n\
             Host db\n  HostName 10.2.0.5\n  ProxyJump inner,ops@gw.lan:2022\n\
             Host direct\n  ProxyJump none\n\
             Host * !bastion\n  ProxyJump bastion\n",
        );
        assert_eq!(host(&found, "bastion").jump_hosts, Some(vec![]));
        assert_eq!(host(&found, "inner").jump_hosts, Some(vec![jump(Some("jump"), "bastion.example.com", 2200)]));
        assert_eq!(
            host(&found, "db").jump_hosts,
            Some(vec![
                jump(Some("jump"), "bastion.example.com", 2200),
                jump(None, "10.1.0.1", 22),
                jump(Some("ops"), "gw.lan", 2022),
            ])
        );
        assert_eq!(host(&found, "direct").jump_hosts, Some(vec![]));
    }

    #[test]
    fn a_proxy_jump_loop_is_left_out_instead_of_followed() {
        let found = parse("Host a\n  ProxyJump b\nHost b\n  ProxyJump a\n");
        let a = host(&found, "a");
        assert_eq!(a.jump_hosts, Some(vec![]));
        assert_eq!(a.left_out[0], LeftOut::new("Jump host", "ProxyJump b"));
    }

    #[test]
    fn forwards_come_over_and_what_cannot_is_listed() {
        let found = parse(
            "Host web\n\
             LocalForward 5432 db.internal:5432\n\
             LocalForward *:8080 [fd00::5]:80\n\
             LocalForward 10.0.0.2:9000 localhost:9000\n\
             RemoteForward 8022 localhost:22\n\
             RemoteForward 1080\n\
             DynamicForward localhost:1081\n\
             LocalForward /tmp/sock db:5432\n\
             LocalForward /run/5432 db:5432\n\
             LocalForward 6000 /var/run/x.sock\n",
        );
        let web = host(&found, "web");
        assert_eq!(
            web.port_forwards,
            Some(vec![
                PortForward::local(5432, "db.internal", 5432),
                PortForward { open_to_network: true, ..PortForward::local(8080, "fd00::5", 80) },
                PortForward::local(9000, "localhost", 9000),
                PortForward::remote(8022, "localhost", 22),
                PortForward::socks(1081),
            ])
        );
        assert_eq!(
            web.left_out,
            [
                LeftOut::new("Listen address", "10.0.0.2 for 9000 → localhost:9000: only loopback for now"),
                LeftOut::new("Port forwarding", "LocalForward /tmp/sock db:5432"),
                LeftOut::new("Port forwarding", "LocalForward /run/5432 db:5432"),
                LeftOut::new("Port forwarding", "LocalForward 6000 /var/run/x.sock"),
                LeftOut::new("Port forwarding", "RemoteForward 1080 (a SOCKS proxy on the server)"),
            ]
        );
        let gateway = parse("Host web\n  GatewayPorts yes\n  LocalForward 3000 localhost:3000\n  RemoteForward 3001 localhost:3001\n");
        let forwards = host(&gateway, "web").port_forwards.clone().unwrap();
        assert!(forwards[0].open_to_network && !forwards[1].open_to_network);
    }

    #[test]
    fn keys_and_other_settings_stay_in_the_file_and_are_listed() {
        let found = parse(
            "Host web\n  IdentityFile ~/.ssh/id_work\n  ForwardAgent yes\n  ProxyCommand nc -X 5 -x proxy:1080 %h %p\n  ServerAliveInterval 30\n\
             Host *\n  IdentityFile ~/.ssh/id_ed25519\n  ForwardX11 no\n",
        );
        assert_eq!(
            host(&found, "web").left_out,
            [
                LeftOut::new("Private keys", "id_work and id_ed25519"),
                LeftOut::new("Proxy command", "nc -X 5 -x proxy:1080 %h %p"),
                LeftOut::new("Agent forwarding", "on"),
                LeftOut::new("Keep-alive", "every 30 s"),
            ]
        );
    }

    #[test]
    fn match_blocks_on_the_name_apply_and_others_are_noted() {
        let found = parse(
            "Host web\n  HostName %h.lan\nHost db\n  HostName %h.lan\n\
             Match host *.lan !originalhost db\n  User deploy\n\
             Match originalhost db\n  Port 5433\n\
             Match user root\n  Port 2222\n\
             Match exec \"test -f /tmp/x\" host *.lan\n  User exec\n\
             Match all\n  User fallback\n",
        );
        let web = host(&found, "web");
        assert_eq!((web.user.as_deref(), web.port), (Some("deploy"), 22), "host matches the host name found so far");
        let db = host(&found, "db");
        assert_eq!((db.user.as_deref(), db.port), (Some("fallback"), 5433));
        assert_eq!(
            found.notes,
            ["The settings in 2 Match blocks aren't imported: they depend on more than the host's name, such as the user, a \
              command or the network, which Komando can't check before connecting."]
        );
    }

    #[test]
    fn host_names_expand_their_tokens_like_ssh() {
        assert_eq!(expand_host_name("%h.atlas.lan", "web").as_deref(), Some("web.atlas.lan"));
        assert_eq!(expand_host_name("%%h", "web").as_deref(), Some("%h"));
        assert_eq!(expand_host_name("100%%", "web").as_deref(), Some("100%"));
        assert_eq!(expand_host_name("%r.lan", "web"), None);
        let found = parse("Host web\n  HostName %r.lan\n");
        let web = host(&found, "web");
        assert_eq!(web.address, "web", "not a made-up address");
        assert_eq!(web.left_out, [LeftOut::new("Host name", "%r.lan")]);
    }

    #[test]
    fn a_jump_hosts_own_key_and_proxy_are_listed_with_the_host_that_uses_it() {
        let found = parse(
            "Host bastion\n  HostName b.example.com\n  IdentityFile ~/.ssh/bastion_key\n  ProxyCommand corkscrew proxy 8080 %h %p\n\
             Host db\n  ProxyJump bastion\n  IdentityFile ~/.ssh/db_key\n",
        );
        assert_eq!(
            host(&found, "db").left_out,
            [
                LeftOut::new("Private key of jump host bastion", "bastion_key"),
                LeftOut::new("Proxy command of jump host bastion", "corkscrew proxy 8080 %h %p"),
                LeftOut::new("Private key", "db_key"),
            ]
        );
    }

    #[test]
    fn includes_are_followed_with_wildcards_and_hand_the_block_back() {
        let dir = std::env::temp_dir().join(format!("komando-ssh-config-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("config.d")).unwrap();
        std::fs::write(dir.join("config.d/20-db"), "Host db\n  HostName 10.0.0.5\n").unwrap();
        std::fs::write(dir.join("config.d/10-web"), "Host web\n  HostName web.lan\n").unwrap();
        std::fs::write(dir.join("config.d/notes.txt.bak"), "Host ignored\n").unwrap();
        std::fs::write(dir.join("config.d/.10-web.swp"), "Host swap\n").unwrap();
        std::fs::write(dir.join("extra"), "User from-extra\n").unwrap();
        let config = "Host bastion\n  Include extra\n  Port 2200\nInclude config.d/??-* config.d/*web*\nHost *\n  User everyone\n";
        let mut lines = vec![];
        expand(config, &dir, None, 0, None, &mut lines);
        let found = found(&lines);
        let names: Vec<&str> = found.groups[0].hosts.iter().map(|h| h.name.as_str()).collect();
        assert_eq!(names, ["bastion", "web", "db"], "included files in name order");
        let bastion = host(&found, "bastion");
        assert_eq!((bastion.port, bastion.user.as_deref()), (2200, Some("from-extra")), "the block goes on after its Include");
        assert_eq!(host(&found, "db").user.as_deref(), Some("everyone"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn imported_hosts_connect_the_way_ssh_would() {
        let found = parse(
            "Host bastion\n  HostName bastion.example.com\n  User jump\n\
             Host db\n  HostName 10.0.0.5\n  User postgres\n  ProxyJump bastion\n  LocalForward 5432 localhost:5432\n",
        );
        let mut library = crate::model::Library::default();
        let plan = super::super::plan(&found, &library);
        let picked: Vec<Vec<bool>> = found.groups.iter().map(|g| vec![true; g.hosts.len()]).collect();
        super::super::apply(&found, &plan, &picked, &mut library);
        let project = &library.projects[0];
        assert_eq!(project.name, GROUP);
        let db = library.hosts.iter().find(|h| h.name == "db").unwrap();
        let resolved = library.resolve(&project.id, &db.id).unwrap();
        assert_eq!((resolved.address.as_str(), resolved.user.as_deref()), ("10.0.0.5", Some("postgres")));
        assert_eq!(resolved.jump_hosts.into_iter().map(|r| r.jump).collect::<Vec<_>>(), [jump(Some("jump"), "bastion.example.com", 22)]);
        assert_eq!(resolved.port_forwards, [PortForward::local(5432, "localhost", 5432)]);
        // Importing again finds both already there.
        let again = super::super::plan(&found, &library);
        assert!(again.groups[0].hosts.iter().all(|d| d.is_duplicate()));
    }

    #[test]
    fn a_nested_include_keeps_its_settings_in_the_block_it_sits_in() {
        let dir = std::env::temp_dir().join(format!("komando-ssh-nested-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        // inc1 sits in web's block and includes inc2 before its own line: that line is still web's.
        std::fs::write(dir.join("inc1"), "Include inc2\nUser from-inc1\n").unwrap();
        std::fs::write(dir.join("inc2"), "Port 2200\n").unwrap();
        // A file that opens a block of its own hands web's block back when it ends.
        std::fs::write(dir.join("inc3"), "Host other\n  HostName other.lan\n").unwrap();
        let config = "Host web\n  Include inc1\n  Include inc3\n  HostName web.lan\nHost db\n  HostName 10.0.0.5\n";
        let mut lines = vec![];
        expand(config, &dir, None, 0, None, &mut lines);
        let found = found(&lines);
        let web = host(&found, "web");
        assert_eq!((web.address.as_str(), web.port, web.user.as_deref()), ("web.lan", 2200, Some("from-inc1")));
        let db = host(&found, "db");
        assert_eq!((db.port, db.user.as_deref()), (22, None), "web's included settings don't reach db");
        assert_eq!(host(&found, "other").address, "other.lan");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_file_without_named_hosts_has_nothing_to_import() {
        assert!(parse("Host *\n  User me\n").groups.is_empty());
        assert!(parse("").groups.is_empty());
    }

    /// Found by fuzzing (`fuzz_ssh_configs`): `HostName ""` gives the host an empty address. It
    /// would be added and could never connect; the other importers leave such a host out with a
    /// note.
    #[test]
    fn an_empty_host_name_is_not_imported_as_an_address() {
        let found = parse("Host b\n  HostName \"\"\n");
        for host in found.groups.iter().flat_map(|g| &g.hosts) {
            assert!(!host.address.is_empty(), "{} was imported without an address", host.name);
        }
    }

    /// Found by fuzzing (`fuzz_ssh_configs`): files that include each other more than once are
    /// read again at every level up to `MAX_DEPTH`, so the lines grow as (includes)^16. Two
    /// Includes make 65,536 copies; three freeze Komando (the import runs on the UI thread)
    /// and fill its memory. `ssh` stops such a loop with an error instead.
    #[test]
    fn includes_that_loop_stop_growing() {
        let dir = std::env::temp_dir().join(format!("komando-ssh-loop-{}", crate::model::new_id()));
        std::fs::create_dir_all(&dir).unwrap();
        let config = "Host web\n  HostName web.lan\nInclude config\nInclude config\n";
        std::fs::write(dir.join("config"), config).unwrap();
        let mut lines = vec![];
        expand(config, &dir, None, 0, None, &mut lines);
        let _ = std::fs::remove_dir_all(&dir);
        assert!(lines.len() < 1000, "{} lines from a 4-line file", lines.len());
    }

    /// Fuzzed: SSH configs with their Includes, in a folder of their own. Paths stay relative
    /// to it, so no other file on this computer is read.
    #[test]
    fn fuzz_ssh_configs() {
        let seed = b"Host bastion\n  HostName bastion.example.com\n  Port 2222\n  User ops\n  IdentityFile ~/.ssh/id_ed25519\n\
            Host web db *.lan !old.lan\n  ProxyJump ops@bastion:2222,[fe80::1]:22\n  LocalForward 8080 db.lan:5432\n  RemoteForward [::1]:9000 localhost:3000\n  DynamicForward *:1080\n  GatewayPorts yes\n\
            Match host web exec true\n  User web\n\
            Host %h-alias\n  HostName %h.example.com\n\
            Include extra\nHost *\n  User everyone\n";
        let dictionary: &[&[u8]] = &[
            b"\nHost ", b"\nMatch ", b"\n  HostName ", b"\n  ProxyJump ", b"\n  ProxyCommand ", b"\n  LocalForward ", b"\n  RemoteForward ",
            b"\n  DynamicForward ", b"\nInclude extra\n", b"\nInclude c*\n", b"\nInclude e* e?tra\n", b"=", b"\"", b"%h", b"%%", b"%",
            b"!", b"*", b"?", b"[", b"]", b":", b"@", b",", b"none", b"ssh://", b"\t", b"#",
        ];
        let dir = std::env::temp_dir().join(format!("komando-fuzz-ssh-{}", crate::model::new_id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("extra"), "Host extra\n  HostName extra.lan\n  ProxyJump web\n").unwrap();
        let base = dir.clone();
        crate::fuzz::run("ssh configs", &[seed], dictionary, move |input| {
            let text = crate::fuzz::text(input);
            // A file that includes itself more than once grows exponentially: a known bug with
            // a test of its own (`includes_that_loop_stop_growing`), left out here.
            if text.contains(['/', '\\', '~']) || text.to_lowercase().matches("include").count() > 1 {
                return;
            }
            std::fs::write(base.join("config"), text.as_bytes()).unwrap();
            let mut lines = vec![];
            expand(&text, &base, None, 0, None, &mut lines);
            let found = found(&lines);
            for host in found.groups.iter().flat_map(|g| &g.hosts) {
                assert!(host.port != 0, "{} has port 0", host.name);
                // `HostName ""` is a known bug with a test of its own
                // (`an_empty_host_name_is_not_imported_as_an_address`), left out here.
                if host.address.is_empty() {
                    return;
                }
            }
            crate::fuzz::check_import(&found);
        });
        let _ = std::fs::remove_dir_all(dir);
    }
}
