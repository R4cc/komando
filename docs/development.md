# Development

## Build it yourself on Windows

A build from source never carries the internet download mark, so Windows SmartScreen doesn't
warn. You need about 10 GB of free disk space; the first build takes 15 to 30 minutes, later
builds a minute or two.

1. Install the tools (PowerShell, once):

   ```powershell
   winget install Microsoft.VisualStudio.2022.BuildTools --override "--quiet --wait --add Microsoft.VisualStudio.Workload.VCTools --includeRecommended"
   winget install Rustlang.Rustup
   winget install Git.Git
   ```

   The Build Tools bring the Microsoft C++ compiler and the Windows SDK, which Rust needs.
   Open a **new** PowerShell window afterwards so the tools are on the PATH.

2. Get the code. The repository is private, so Git asks you to sign in to GitHub the first
   time:

   ```powershell
   git clone https://github.com/R4cc/komando.git
   cd komando
   ```

3. Build:

   ```powershell
   cargo build --release
   ```

   The result is `target\release\komando.exe`, a single file with the C++ runtime, fonts and
   icons built in. Copy it wherever you like, for example `%LOCALAPPDATA%\Programs\Komando`.

4. To update later: `git pull`, then `cargo build --release` again.

Run the tests with `cargo test --lib`.

## Versions and releases

The version is in `Cargo.toml` and every change is listed in [CHANGELOG.md](../CHANGELOG.md).
Follow the [versioning policy in AGENTS.md](../AGENTS.md#versioning).

Publish a GitHub release with a tag that matches `Cargo.toml`, like `v0.2.0`. The
[release workflow](../.github/workflows/release.yml) builds Komando on Windows and attaches
`Komando-0.2.0-x64.msi` and `Komando-0.2.0-x64.exe`. Both download filenames include
the release version; the README links to the latest release's assets. To rebuild an existing release, run the workflow
manually with its tag.

The installer is built from [packaging/komando.wxs](../packaging/komando.wxs) by
[packaging/build-msi.ps1](../packaging/build-msi.ps1) (WiX v5). CI builds it on every push, from
the debug program, so a mistake in the installer shows up before a release. It asks whether to
install for the current user only (no administrator rights) or for all users; for scripted
installs, `msiexec /i Komando-0.2.0-x64.msi /qn` installs for the current user and adding `ALLUSERS=1`
installs for all users.

## Developing on Linux

Komando ships for Windows only, but it also builds on Linux, which is handy for tests and
screenshots. You need the X11 libraries (`libxcb`, `libxkbcommon`, `libxkbcommon-x11`). Set
`RUST_FONTCONFIG_DLOPEN=on` if fontconfig has no pkg-config file. `examples/demo.rs` seeds a
throwaway data folder and opens a few sessions; it's what produced the README screenshot.

### Terminal compatibility tests

`src/compat_tests.rs` runs real programs (vim, htop, tmux, less, bash, and an SSH session against
a throwaway local `sshd`) in a real pseudo-terminal with Komando's terminal settings, and drives
them with Komando's own key, mouse and wheel encoding. They need those programs installed, so
they are ignored by default:

```sh
cargo test --lib compat -- --ignored --test-threads=1
```

A missing program is skipped with a note. Put programs unpacked elsewhere on
`KOMANDO_COMPAT_PATH` (for example a `.deb` extracted with `dpkg-deb -x`, along with
`LD_LIBRARY_PATH` for its libraries).

### SFTP stress test

`compat_sftp_stress` puts the SFTP transfers under load against the same throwaway `sshd`:
a large file each way, a folder of 1000 small files each way, deleting it, several transfers at
once, 1000 separate downloads started together, and cancelling. It runs each three times:
directly, through a proxy that adds latency (25 ms each way), and through one that also limits
the speed (1 MiB/s), and prints the timings. Every transferred byte is checked.

```sh
TMPDIR=~/.cache/komando-stress cargo test --lib compat_sftp_stress -- --ignored --nocapture
```

`KOMANDO_STRESS_MB` sets the large file's size (default 128), `KOMANDO_STRESS_LATENCY_MS` the
delay and `KOMANDO_STRESS_SLOW_KBPS` the slow link's speed in KiB/s (0 skips it). Point
`TMPDIR` at a disk: on a RAM-backed `/tmp` large files run the machine out of memory.

### Updates

Settings → About checks the latest GitHub release (`/repos/R4cc/komando/releases/latest`) and
installs it: a portable `komando.exe` in a writable folder is swapped in place (the old file is
kept as `komando.exe.old` until the next start), an MSI install runs the new installer. An install
for all users is the one under Program Files; one for a single user is in a writable folder too
(`%LOCALAPPDATA%\Programs\Komando`), and is recognised by the `installed-with-msi.txt` file the
MSI puts next to `komando.exe`. Downloads are checked against the size and SHA-256 digest GitHub lists. To try the
flow against the current release, start Komando with `KOMANDO_PRETEND_VERSION=0.0.1`.

### Profiling

Set `KOMANDO_PERF=1` and Komando prints, every 5 seconds, how often the hot paths ran and how
long they took (window renders, terminal grid, minimap, command rail, output wake-ups). Off by
default; each probe is then a single atomic load.

For load tests the demo has a stress mode: `KOMANDO_DEMO_STRESS=30,10,24` creates 30 projects of
10 hosts and opens 24 sessions, all against the demo's test SSH server. Point the server's
`ForceCommand` at a script that streams logs (a `while` loop of `printf`s will do) to make every
session busy. Add `,idle` (`30,10,24,idle`) to show a quiet local shell while the rest stream in
the background; nothing should be redrawn then. Measure a `--release` build: debug builds are
several times slower in different places.

### Mixed load and performance comparisons

The mixed-load test creates a temporary library with 500 Projects and 10,000 Hosts, opens
32 real SSH connections to a throwaway local `sshd`, streams 320,000 Unicode log lines, and
runs 32 simultaneous downloads into one folder. It checks every downloaded byte, unique
filenames, bounded Scrollback, final output, and clean disconnection:

```sh
cargo test --release --lib compat_many_projects_connections_and_downloads -- --ignored --nocapture
```

Three smaller workloads compare the former algorithm with the optimized production path
in the same process. They cover 40-row highlighted repaints in both Themes, 10/100/500
Projects, and 1,000 queued download reservations (32 active) with identical filenames:

```sh
cargo test --release --lib stress_ -- --ignored --nocapture --test-threads=1
```

These print timings without machine-dependent pass/fail thresholds. See
[the recorded performance investigation](performance/2026-11-12/REPORT.md) for measured
results, raw test output, scope, and limitations. SSH tests run entirely against temporary
fixtures; they do not connect to saved Hosts or change a user's library.

## Design notes

- [GLOSSARY.md](../GLOSSARY.md): the terms used in code and docs (Project, Host, Session,
  Vault, …).
- [docs/adr](adr): decisions and why: Windows only, embedded SSH client, native GPU UI,
  master-password vault, a sidebar of hosts instead of tabs.
