//! A small mutation fuzzer for the tests: no extra crates and no nightly toolchain.
//!
//! Each target starts from seed inputs and a dictionary of tokens that mean something to it
//! (escape sequences, keywords, separators), then runs many inputs made by mutating them:
//! flipped bits, interesting bytes, inserted tokens, cut, repeated and spliced pieces, and
//! now and then a very long run. Every input runs on a worker thread, so a panic is caught
//! and a hang or a very slow input is reported too. A failing input is shrunk before it is
//! reported, with the seed and the iteration that found it.
//!
//! The run is deterministic. `KOMANDO_FUZZ_ITERS` sets how many inputs each target runs
//! (default 1500, quick enough for every `cargo test`), and `KOMANDO_FUZZ_SEED` picks another
//! sequence, for longer hunts:
//!
//! ```sh
//! KOMANDO_FUZZ_ITERS=200000 KOMANDO_FUZZ_SEED=7 cargo test --lib fuzz_ -- --test-threads=4
//! ```

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::Arc;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use rand::rngs::StdRng;
use rand::{Rng, SeedableRng};

/// The most one input may take before it counts as a hang.
const SLOW: Duration = Duration::from_secs(5);
/// The longest input a mutation makes.
const MAX_LEN: usize = 64 * 1024;

/// Bytes that often take parsers down an edge.
const INTERESTING: &[u8] = &[0, 1, 0x7f, 0x80, 0xff, 0xfe, 0xc0, 0xe2, 0xf0, b'\n', b'\r', b'\t', 0x1b, b' ', b'"', b'\'', b'\\', b'%', b'[', b']', b'{', b'}', b'=', b':', b'/', b'.', b'#', b'*', b'?', b'@', b'-'];

/// Text that is unusual for any parser: invalid and multi-byte UTF-8, a BOM, huge numbers.
const COMMON: &[&[u8]] = &[
    b"\xef\xbb\xbf",
    b"\xff\xfe",
    b"\xe2\x80\xae",
    "\u{1F600}".as_bytes(),
    "e\u{301}".as_bytes(),
    "\u{130}".as_bytes(),
    "\u{2028}".as_bytes(),
    b"\xed\xa0\x80",
    b"\r\n",
    b"\x00",
    b"18446744073709551616",
    b"-1",
    b"65536",
    b"0",
    b"..",
    b"../",
    b"NaN",
    b"1e309",
];

fn iterations() -> usize {
    std::env::var("KOMANDO_FUZZ_ITERS").ok().and_then(|n| n.parse().ok()).unwrap_or(1500)
}

fn base_seed() -> u64 {
    std::env::var("KOMANDO_FUZZ_SEED").ok().and_then(|n| n.parse().ok()).unwrap_or(0x006b_6f6d_616e_646f)
}

/// Run `check` on `iterations()` inputs grown from `seeds` with `dictionary`'s tokens. `check`
/// panics on anything unexpected: a crash in the code under test, or a broken invariant.
pub(crate) fn run(name: &str, seeds: &[&[u8]], dictionary: &[&[u8]], check: impl Fn(&[u8]) + Send + Sync + 'static) {
    let seed = base_seed() ^ name.bytes().fold(0xcbf2_9ce4_8422_2325u64, |h, b| (h ^ u64::from(b)).wrapping_mul(0x100_0000_01b3));
    let mut rng = StdRng::seed_from_u64(seed);
    let mut corpus: Vec<Vec<u8>> = seeds.iter().map(|s| s.to_vec()).collect();
    if corpus.is_empty() {
        corpus.push(vec![]);
    }
    let tokens: Vec<&[u8]> = dictionary.iter().chain(COMMON).copied().collect();
    let worker = Worker::new(check, SLOW);

    // The seeds as they are first: they must pass, or the target is wrong.
    for (i, input) in corpus.clone().iter().enumerate() {
        if let Err(why) = worker.try_input(input) {
            panic!("fuzz {name}: seed {i} fails ({why}): {}", show(input));
        }
    }
    for iteration in 0..iterations() {
        let parent = &corpus[rng.gen_range(0..corpus.len())];
        let mut input = parent.clone();
        for _ in 0..rng.gen_range(1..=6) {
            mutate(&mut input, &corpus, &tokens, &mut rng);
        }
        if let Err(why) = worker.try_input(&input) {
            let (small, why) = shrink(&worker, input, why);
            panic!(
                "fuzz {name}: {why}\n  input ({} bytes): {}\n  found at iteration {iteration} with KOMANDO_FUZZ_SEED={}",
                small.len(),
                show(&small),
                base_seed()
            );
        }
        // Keep some of what was made, so later inputs build on deeper ones.
        if corpus.len() < 256 && rng.gen_ratio(1, 8) {
            corpus.push(input);
        } else if rng.gen_ratio(1, 16) {
            let at = rng.gen_range(seeds.len().min(corpus.len() - 1)..corpus.len());
            corpus[at] = input;
        }
    }
}

/// Runs inputs one at a time on a thread of its own, so a hang can be reported. After a hang
/// the thread is abandoned and a new one started.
type Check = Arc<dyn Fn(&[u8]) + Send + Sync>;
type Channel = (mpsc::Sender<Vec<u8>>, mpsc::Receiver<Result<(), String>>);

struct Worker {
    check: Check,
    slow: Duration,
    channel: std::cell::RefCell<Option<Channel>>,
}

impl Worker {
    fn new(check: impl Fn(&[u8]) + Send + Sync + 'static, slow: Duration) -> Self {
        quiet_workers();
        Self { check: Arc::new(check), slow, channel: std::cell::RefCell::new(None) }
    }

    fn start(&self) -> Channel {
        let (input_tx, input_rx) = mpsc::channel::<Vec<u8>>();
        let (result_tx, result_rx) = mpsc::channel();
        let check = self.check.clone();
        std::thread::Builder::new()
            .name("fuzz-worker".into())
            .stack_size(16 * 1024 * 1024)
            .spawn(move || {
                for input in input_rx {
                    let outcome = catch_unwind(AssertUnwindSafe(|| check(&input))).map_err(|payload| panic_message(&*payload));
                    if result_tx.send(outcome).is_err() {
                        return;
                    }
                }
            })
            .expect("a fuzz worker thread");
        (input_tx, result_rx)
    }

    fn try_input(&self, input: &[u8]) -> Result<(), String> {
        let mut channel = self.channel.borrow_mut();
        let (tx, rx) = channel.get_or_insert_with(|| self.start());
        tx.send(input.to_vec()).map_err(|_| "the worker stopped".to_string())?;
        match rx.recv_timeout(self.slow) {
            Ok(Ok(())) => Ok(()),
            Ok(Err(message)) => Err(format!("panicked: {message}")),
            Err(_) => {
                *channel = None;
                Err(format!("hung for over {:?}", self.slow))
            }
        }
    }
}

/// Panics on fuzz workers are expected and reported with their input: keep their messages
/// (thousands while shrinking) out of the test output. Other threads report as before.
fn quiet_workers() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        let previous = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            if std::thread::current().name() != Some("fuzz-worker") {
                previous(info);
            }
        }));
    });
}

fn panic_message(payload: &(dyn std::any::Any + Send)) -> String {
    payload
        .downcast_ref::<String>()
        .cloned()
        .or_else(|| payload.downcast_ref::<&str>().map(|s| s.to_string()))
        .unwrap_or_else(|| "a panic without a message".into())
}

fn mutate(input: &mut Vec<u8>, corpus: &[Vec<u8>], tokens: &[&[u8]], rng: &mut StdRng) {
    let len = input.len();
    let at = |rng: &mut StdRng| rng.gen_range(0..=input.len());
    match rng.gen_range(0..12) {
        0 if len > 0 => {
            let i = rng.gen_range(0..len);
            input[i] ^= 1 << rng.gen_range(0..8);
        }
        1 if len > 0 => {
            let i = rng.gen_range(0..len);
            input[i] = INTERESTING[rng.gen_range(0..INTERESTING.len())];
        }
        2 | 3 => {
            let i = at(rng);
            let token = tokens[rng.gen_range(0..tokens.len())];
            input.splice(i..i, token.iter().copied());
        }
        4 if len > 0 => {
            let start = rng.gen_range(0..len);
            let end = rng.gen_range(start..=len.min(start + 64));
            input.drain(start..end);
        }
        5 if len > 0 => {
            // Repeat a piece: deep nesting, long lines, many entries.
            let start = rng.gen_range(0..len);
            let end = rng.gen_range(start..=len.min(start + 32));
            let piece = input[start..end].to_vec();
            let times = if rng.gen_ratio(1, 8) { rng.gen_range(100..2000) } else { rng.gen_range(1..8) };
            let i = at(rng);
            input.splice(i..i, piece.iter().copied().cycle().take(piece.len() * times));
        }
        6 => {
            let other = &corpus[rng.gen_range(0..corpus.len())];
            if !other.is_empty() {
                let start = rng.gen_range(0..other.len());
                let end = rng.gen_range(start..=other.len());
                let i = at(rng);
                input.splice(i..i, other[start..end].iter().copied());
            }
        }
        7 => {
            let i = at(rng);
            let n = rng.gen_range(1..16);
            let bytes: Vec<u8> = (0..n).map(|_| rng.r#gen()).collect();
            input.splice(i..i, bytes);
        }
        8 if len > 0 => input.truncate(rng.gen_range(0..len)),
        9 if rng.gen_ratio(1, 4) => {
            let i = at(rng);
            let byte = INTERESTING[rng.gen_range(0..INTERESTING.len())];
            input.splice(i..i, std::iter::repeat_n(byte, rng.gen_range(1000..20_000)));
        }
        10 => {
            // A number where one was: past the edges of the integer types.
            let numbers: [&[u8]; 8] = [b"0", b"-1", b"255", b"256", b"65535", b"65536", b"4294967296", b"99999999999999999999"];
            let i = at(rng);
            input.splice(i..i, numbers[rng.gen_range(0..numbers.len())].iter().copied());
        }
        _ => {
            if len > 1 {
                let (a, b) = (rng.gen_range(0..len), rng.gen_range(0..len));
                input.swap(a, b);
            }
        }
    }
    input.truncate(MAX_LEN);
}

/// The smallest input found that still fails, removing pieces while it does.
fn shrink(worker: &Worker, mut input: Vec<u8>, mut why: String) -> (Vec<u8>, String) {
    let deadline = Instant::now() + Duration::from_secs(30);
    let mut chunk = input.len().div_ceil(2).max(1);
    while chunk > 0 && Instant::now() < deadline {
        let mut start = 0;
        let mut shrunk = false;
        while start < input.len() && Instant::now() < deadline {
            let end = (start + chunk).min(input.len());
            let mut candidate = input.clone();
            candidate.drain(start..end);
            match worker.try_input(&candidate) {
                Err(still) => {
                    input = candidate;
                    why = still;
                    shrunk = true;
                }
                Ok(()) => start += chunk,
            }
        }
        if !shrunk {
            chunk /= 2;
        }
    }
    (input, why)
}

/// An input as a Rust byte string literal, to paste into a regression test.
fn show(input: &[u8]) -> String {
    const LIMIT: usize = 2000;
    let mut out = String::from("b\"");
    for &b in input.iter().take(LIMIT) {
        out.extend(std::ascii::escape_default(b).map(char::from));
    }
    out.push('"');
    if input.len() > LIMIT {
        out.push_str(&format!(" … ({} more bytes)", input.len() - LIMIT));
    }
    out
}

/// The input as text, the way a file read with `from_utf8_lossy` would be.
pub(crate) fn text(input: &[u8]) -> std::borrow::Cow<'_, str> {
    String::from_utf8_lossy(input)
}

/// The whole import of `found`: plan it against a Library that already has a Project with a
/// saved Credential, add every host the plan doesn't call a duplicate, and check what came of
/// it. Panics on a broken invariant.
pub(crate) fn check_import(found: &crate::import::Found) {
    use crate::import::{apply, plan};
    use crate::model::{Host, Inherited, Library, Project};

    let mut library = Library::default();
    let mut servers = Project::new("Servers", "#3b82f6", "server");
    servers.credential = Some("saved-key".into());
    let mut existing = Host::new("web01", "web01.atlas.lan");
    existing.credential = Inherited::Own("saved-password".into());
    servers.hosts.push(existing.id.clone());
    library.projects.push(servers);
    library.hosts.push(existing);
    let users_before = (library.credential_users("saved-key"), library.credential_users("saved-password"));

    let planned = plan(found, &library);
    assert_eq!(planned.groups.len(), found.groups.len(), "a plan for every group");
    for (group, planned) in found.groups.iter().zip(&planned.groups) {
        assert_eq!(planned.hosts.len(), group.hosts.len(), "a plan for every host of {:?}", group.name);
    }
    let picked: Vec<Vec<bool>> = planned.groups.iter().map(|g| g.hosts.iter().map(|d| !d.is_duplicate()).collect()).collect();
    let wanted: usize = picked.iter().flatten().filter(|p| **p).count();
    let applied = apply(found, &planned, &picked, &mut library);
    assert_eq!(applied.hosts, wanted, "every picked host is added");

    // A sound Library: every Project's Hosts exist, once, and Host ids are unique.
    let mut ids = std::collections::HashSet::new();
    for host in &library.hosts {
        assert!(ids.insert(host.id.as_str()), "two Hosts with id {}", host.id);
    }
    for project in &library.projects {
        for id in &project.hosts {
            assert!(ids.contains(id.as_str()), "Project {:?} lists a Host that doesn't exist", project.name);
        }
    }
    // A file can never give a host a saved Credential.
    assert_eq!(
        (library.credential_users("saved-key"), library.credential_users("saved-password")),
        users_before,
        "an imported host took a saved Credential"
    );
    // What was added is saved and read back as it is. A string with 256 quotes in a row
    // overflows `toml` 0.8's writer in builds with overflow checks: a known bug with a test
    // of its own (`model::tests::names_with_long_runs_of_quotes_save`), left out here so the
    // fuzzer looks further.
    if cfg!(debug_assertions) && has_long_quote_run(&library) {
        return;
    }
    let text = toml::to_string_pretty(&library).expect("the Library is written as TOML");
    let read: Library = toml::from_str(&text).unwrap_or_else(|err| panic!("the saved Library can't be read back: {err}\n{text}"));
    assert_eq!(read, library, "the Library changed on its way through library.toml");

    // Importing the same again adds nothing: every host is now already there.
    let again = plan(found, &library);
    for (group, planned) in found.groups.iter().zip(&again.groups) {
        for ((host, duplicate), was_picked) in group.hosts.iter().zip(&planned.hosts).zip(picked.iter().flatten()) {
            if *was_picked {
                assert!(duplicate.is_duplicate(), "{:?} ({}) imported twice would be added twice", host.name, host.address);
            }
        }
    }
}

/// Whether any string in `value` has 256 or more of the same quote in a row.
fn has_long_quote_run(value: &impl serde::Serialize) -> bool {
    fn walk(value: &serde_json::Value) -> bool {
        match value {
            serde_json::Value::String(s) => ['\'', '"'].iter().any(|q| s.contains(&q.to_string().repeat(256))),
            serde_json::Value::Array(items) => items.iter().any(walk),
            serde_json::Value::Object(map) => map.iter().any(|(k, v)| walk(&serde_json::Value::String(k.clone())) || walk(v)),
            _ => false,
        }
    }
    serde_json::to_value(value).is_ok_and(|v| walk(&v))
}

/// Targets reached through public functions. Those that need a module's private parts are
/// fuzzed in that module's tests.
#[cfg(test)]
mod targets {
    use super::*;

    fn any_icon(name: &str) -> bool {
        !name.is_empty() && name.len() < 40
    }

    #[test]
    fn fuzz_shared_project_files() {
        let seed = b"komando_project = 1\n[project]\nname = \"Lab\"\ncolor = \"#22c55e\"\nicon = \"server\"\ndefault_user = \"ops\"\n\
            [[project.jump_hosts]]\naddress = \"bastion.lan\"\nport = 2222\nuser = \"jump\"\n\
            [[project.startup_commands]]\ncommand = \"cd /srv\"\n\
            [[project.port_forwards]]\nkind = \"socks\"\nport = 1080\n\
            [[hosts]]\nname = \"web\"\naddress = \"web.lan\"\nport = 22\nuser = \"root\"\n\
            [[hosts]]\nname = \"db\"\naddress = \"10.0.0.5\"\nuser = false\nuse_project_jump_hosts = false\n\
            [[hosts.port_forwards]]\nkind = \"local\"\nport = 5432\nto_host = \"localhost\"\nto_port = 5432\nopen_to_network = true\n";
        let dictionary: &[&[u8]] = &[
            b"komando_project", b"[project]", b"[[hosts]]", b"[[hosts.jump_hosts]]", b"[[project.port_forwards]]",
            b"[[hosts.startup_commands]]", b"wait_for = \"password:\"\n", b"name = ", b"address = ", b"port = ", b"user = false\n",
            b"user = true\n", b"kind = \"remote\"\n", b"\"\"", b"'''", b"\"\"\"", b"= [", b"= {", b"\\u0000", b"\\U0010ffff",
        ];
        run("shared project files", &[seed, b"komando_project = 1\n"], dictionary, |input| {
            if let Ok(found) = crate::share::read(&text(input), &any_icon) {
                check_import(&found);
            }
        });
    }

    #[test]
    fn fuzz_tabby_configs() {
        let seed = b"version: 8\nprofiles:\n  - type: ssh\n    name: web01\n    icon: fas fa-desktop\n    color: '#ff0000'\n    options:\n      host: web01.atlas.lan\n      jumpHost: ssh:custom:bastion\n      scripts:\n        - expect: 'login:'\n          send: ls\n      forwardedPorts:\n        - type: Local\n          host: 127.0.0.1\n          port: 8080\n          targetAddress: db.lan\n          targetPort: 5432\n    group: g-prod\n    id: ssh:custom:web01\n  - type: ssh\n    name: bastion\n    options:\n      host: bastion.example.com\n      port: 2222\n      user: ops\n    group: g-atlas\n    id: ssh:custom:bastion\ngroups:\n  - id: g-atlas\n    name: Atlas\n  - id: g-prod\n    name: Production\n    parentGroupId: g-atlas\n    defaults:\n      options:\n        user: deploy\n";
        let dictionary: &[&[u8]] = &[
            b"\n  - type: ssh\n", b"\n  - type: rdp\n", b"    options:\n", b"      host: ", b"      port: ", b"      user: ", b"jumpHost: ",
            b"parentGroupId: ", b"group: ", b"id: ", b"&anchor ", b"*anchor", b"<<: *anchor\n", b"!!binary ", b"~", b"- ", b": ", b"{", b"[",
            b"'", b"\"", b"|\n", b">-\n", b"\t", b"---\n", b"...\n",
        ];
        run("tabby configs", &[seed], dictionary, |input| {
            if let Ok(found) = crate::import::tabby::parse(&text(input), &any_icon) {
                check_import(&found);
            }
        });
    }

    #[test]
    fn fuzz_mobaxterm_files() {
        let seed = "[Bookmarks]\nSubRep=\nImgNum=42\n\
            db=#109#0%db.lan%2222%<default>%%-1%0%%a.lan__PIPE__b.lan%22__PIPE__2200%ops__PIPE__#MobaFont#0# #-1\n\
            [Bookmarks_1]\nSubRep=Ops__PERCENT__\\Lab\n\
            web__DIEZE__1= #109#0%web.lan%22%root%%-1%0%echo 50__PERCENT__ __DIEZE__ok%%%%0%-1%0%_CurrentDrive_:\\keys\\id_ed25519%%-1%-1%0%0%%1080%%0#MobaFont\n\
            Windows=#42#4%win.lan%3390%ACME\\alice%0%-1%-1%-1%-1%0%0%-1%%%%%0%0%%-1#MobaFont\n\
            Console=#128#5%vnc.lan%5902%-1%0%%22%%%-1#MobaFont\n";
        let dictionary: &[&[u8]] = &[
            b"[Bookmarks_9]\n", b"SubRep=", b"\\", b"#109#0%", b"#91#4%", b"#128#5%", b"%", b"#", b"__PIPE__", b"__PERCENT__",
            b"__DIEZE__", b"__EQUAL__", b"__PTVIRG__", b"=", b"<default>", b"_CurrentDrive_:", b"[Passwords]\n", b"\xe9", b"\x80",
        ];
        run("mobaxterm files", &[seed.as_bytes()], dictionary, |input| {
            // The file is read as MobaXterm writes it (Windows code page or UTF-8).
            let decoded = crate::import::mobaxterm::decode(input);
            if let Ok(found) = crate::import::mobaxterm::parse(&decoded) {
                check_import(&found);
            }
        });
    }

    #[test]
    fn fuzz_highlight_rules_on_output() {
        // A rule set and a line of output, split by the first NUL. The bundled rules first.
        let mut bundled = include_bytes!("../assets/highlights/muted-default.json").to_vec();
        bundled.extend_from_slice(b"\x002026-11-18 12:00:01 ERROR GET /api 200 12ms from 10.0.0.5 warn failed");
        let small = br##"{"id":"s","name":"Ops","keywords":[{"text":"ERROR","foregroundColor":"#ff0000","foreground":true},{"text":"(\\d+)ms","isRegExp":true,"isMatchGroup":true,"matchGroup":"1","background":true,"backgroundColor":"#030"}]}"##;
        let mut small = small.to_vec();
        small.extend_from_slice(b"\x00took 12ms ERROR");
        let seed: &[u8] = &bundled;
        let small: &[u8] = &small;
        let dictionary: &[&[u8]] = &[
            b"\"isRegExp\":true,", b"\"isCaseSensitive\":true,", b"\"isMatchGroup\":true,\"matchGroup\":\"2\",", b"\"matchGroup\":\"99999999999\",",
            b"(a+)+", b"\\\\b", b"\\\\w*", b".*", b"^", b"$", b"(?i)", b"(?P<n>x)", b"[^]", b"\\\\p{L}", b"{1000}", b"\"text\":\"\",",
            b"\"foregroundColor\":\"#abc\",", b"\x00", "\u{e9}".as_bytes(), "\u{130}".as_bytes(),
        ];
        run("highlight rules", &[seed, small], dictionary, |input| {
            let (rules, line) = match input.iter().position(|b| *b == 0) {
                Some(i) => (&input[..i], &input[i + 1..]),
                None => (input, &b"ERROR 12ms warn"[..]),
            };
            let Ok(sets) = crate::highlight::parse(&text(rules)) else { return };
            let highlighter = crate::highlight::Highlighter::from_sets(&sets, &[]);
            let line = text(line);
            for dark in [true, false] {
                if let Some(styles) = highlighter.styles_for_background(&line, dark) {
                    assert!(styles.len() <= line.len().max(line.chars().count()) + 1, "{} styles for a {}-byte line", styles.len(), line.len());
                }
            }
            for at in [0, line.len() / 2, line.len()] {
                let _ = highlighter.rule_at(&line, at);
            }
        });
    }

    #[test]
    fn fuzz_links_in_output() {
        let seed = b"see https://example.com/docs?a=1&b=2#top, (http://[fe80::1]:80/) and ftp://files.lan/pub.";
        let dictionary: &[&[u8]] = &[b"https://", b"HTTP://", b"ftp://", b"(", b")", b"[", b"]", b".", b",", b"'", b"\"", "\u{fffd}".as_bytes(), "\u{200b}".as_bytes()];
        run("links", &[seed], dictionary, |input| {
            let line = text(input);
            // Long runs of closing brackets are slow: a known bug with a test of its own
            // (`links::tests::closing_brackets_after_an_address_take_linear_time`).
            if line.contains(&"]".repeat(500)) || line.contains(&")".repeat(500)) {
                return;
            }
            let found = crate::links::links(&line);
            let mut end = 0;
            for range in &found {
                assert!(range.start >= end && range.start < range.end && range.end <= line.len(), "bad range {range:?} in {line:?}");
                assert!(line.is_char_boundary(range.start) && line.is_char_boundary(range.end), "range {range:?} splits a character");
                assert!(crate::links::is_openable(&line[range.clone()]), "{:?} is found but would not open", &line[range.clone()]);
                end = range.end;
            }
            for at in [0, line.len() / 3, line.len() / 2, line.len().saturating_sub(1)] {
                if let Some(range) = crate::links::link_at(&line, at) {
                    assert!(range.contains(&at));
                }
            }
        });
    }

    #[test]
    fn fuzz_small_parsers() {
        let seed = b"0.8.3\x00ssh -p 2222 \"a b\" 'c d' e\\ f\x00700\x00uid=1000(ana) gid=1000(ana) groups=1000(ana),27(sudo)\x00ana@web:~$ ls -la";
        let dictionary: &[&[u8]] = &[b"\x00", b"v", b".", b"-", b"\"", b"'", b"\\", b"uid=", b"gid=", b"groups=", b"(", b")", b",", b"$ ", b"# ", b"> ", b"\xe2\x9d\xaf "];
        run("small parsers", &[seed], dictionary, |input| {
            let mut parts = input.split(|b| *b == 0);
            let mut next = || text(parts.next().unwrap_or_default()).into_owned();
            let _ = crate::update::parse_version(&next());
            // Arguments survive being shown in the shell form and read back.
            let args = crate::model::split_args(&next());
            let shell = crate::model::LocalShell { args: args.clone(), ..Default::default() };
            assert_eq!(crate::model::split_args(&shell.args_line()), args, "changed on the way through {:?}", shell.args_line());
            if let Some(weight) = crate::model::parse_font_weight(&next()) {
                assert!(crate::model::FONT_WEIGHTS.contains(&weight));
            }
            let _ = crate::sftp::Identity::parse_id(&next());
            let line = next();
            let (prompt, command) = crate::marks::split_prompt(&line);
            assert!(line.len() >= prompt.len() + command.len(), "{prompt:?} + {command:?} is longer than {line:?}");
            if let Some(end) = crate::marks::prompt_end(&line) {
                assert!(line.is_char_boundary(end) && end <= line.len());
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_panic_is_found_and_shrunk() {
        let result = catch_unwind(|| {
            run("self-test", &[b"hello"], &[b"BOOM"], |input| {
                assert!(!input.windows(4).any(|w| w == b"BOOM"), "boom");
            })
        });
        let message = panic_message(&*result.expect_err("the fuzzer finds BOOM"));
        assert!(message.contains("input (4 bytes): b\"BOOM\""), "{message}");
    }

    #[test]
    fn a_hang_is_reported() {
        let worker = Worker::new(
            |input| {
                if input == b"hang" {
                    std::thread::sleep(Duration::from_millis(500));
                }
            },
            Duration::from_millis(100),
        );
        assert!(worker.try_input(b"ok").is_ok());
        assert!(worker.try_input(b"hang").unwrap_err().contains("hung"));
        // A fresh worker takes over.
        assert!(worker.try_input(b"ok").is_ok());
    }
}
