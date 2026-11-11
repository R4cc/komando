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
Patch versions (0.1.x) go up with each change; minor and major versions are bumped by the
maintainer.

Publish a GitHub release with a tag that matches `Cargo.toml`, like `v0.2.0`. The
[release workflow](../.github/workflows/release.yml) builds Komando on Windows and attaches
`Komando-0.2.0-x64.msi` and `Komando-0.2.0-x64.exe`, plus the same two files as
`Komando-x64.msi` and `Komando-x64.exe`, which the README's "latest" download links point to. To rebuild an existing release, run the workflow
manually with its tag.

## Developing on Linux

Komando ships for Windows only, but it also builds on Linux, which is handy for tests and
screenshots. You need the X11 libraries (`libxcb`, `libxkbcommon`, `libxkbcommon-x11`). Set
`RUST_FONTCONFIG_DLOPEN=on` if fontconfig has no pkg-config file. `examples/demo.rs` seeds a
throwaway data folder and opens a few sessions; it's what produced the README screenshot.

### Updates

Settings → About checks the latest GitHub release (`/repos/R4cc/komando/releases/latest`) and
installs it: a portable `komando.exe` in a writable folder is swapped in place (the old file is
kept as `komando.exe.old` until the next start), an MSI install under Program Files runs the new
installer. Downloads are checked against the size and SHA-256 digest GitHub lists. To try the
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

## Design notes

- [GLOSSARY.md](../GLOSSARY.md): the terms used in code and docs (Project, Host, Session,
  Vault, …).
- [docs/adr](adr): decisions and why: Windows only, embedded SSH client, native GPU UI,
  master-password vault, a sidebar of hosts instead of tabs.
