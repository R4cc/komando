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

## Build it yourself on Linux or macOS

Install Rust with [rustup](https://rustup.rs), then:

- **Linux** (Debian/Ubuntu package names): `sudo apt install build-essential libxcb1-dev
  libxcb-xkb-dev libxkbcommon-dev libxkbcommon-x11-dev`. Build with
  `RUST_FONTCONFIG_DLOPEN=on cargo build --release`, as releases do, so fontconfig is loaded
  at startup instead of linked. Wayland and Vulkan are always loaded at startup, and the build
  supports both X11 and Wayland. The program links libxcb, libxkbcommon and libxkbcommon-x11, so
  it needs `libxcb1`, `libxkbcommon0` and `libxkbcommon-x11-0` at run time.
- **macOS**: install Xcode (GPUI compiles its Metal shaders with it; on Xcode 26 and later also
  run `xcodebuild -downloadComponent MetalToolchain`), then
  `MACOSX_DEPLOYMENT_TARGET=12.0 cargo build --release`, as releases do.

The program is `target/release/komando`. `packaging/build-linux.sh` and
`packaging/build-macos.sh` wrap it the way releases do. The first makes a tarball with
`install.sh`, for example `packaging/build-linux.sh 0.7.7 target/release/komando
Komando-0.7.7-linux-x86_64.tar.gz`. The second runs on macOS with the same arguments and makes
a `.dmg` holding `Komando.app`.

## Versions and releases

The version is in `Cargo.toml` and every change is listed in [CHANGELOG.md](../CHANGELOG.md).
Follow the [versioning policy in AGENTS.md](../AGENTS.md#versioning).

Publish a GitHub release with a tag that matches `Cargo.toml`, like `v0.2.0`. The
[release workflow](../.github/workflows/release.yml) checks the version once, then builds
Komando on Windows, Linux and macOS in parallel and attaches `Komando-0.2.0-x64.msi`,
`Komando-0.2.0-x64.exe`, `Komando-0.2.0-linux-x86_64.tar.gz` and
`Komando-0.2.0-macos-arm64.dmg`. Every download filename includes the release version; the
README links to the latest release's assets. Each platform's job can be rerun on its own. To rebuild an existing release, run the workflow
manually with its tag.

Publishing a release queues a second Release workflow run on the default branch (`main`).
That run checks out `refs/tags/<tag>` and verifies its Cargo version before building. Running
the workflow on `main` lets successive releases update and reuse the same Rust dependency
cache: GitHub prevents one tag from restoring another tag's cache. Manual runs on other refs
also queue the build on the default branch. Look for the **Build release** jobs in the queued
run; the initial **Queue build on default branch** job only dispatches it.

The first release with a new compiler or dependency set can still be slow. Shipping builds
keep thin LTO and the release profile's optimisation settings. CI uses `cargo build --profile ci`
and `cargo test --profile ci`: Komando is unoptimised, dependencies retain the dev profile's
optimisations, and assertions and overflow checks remain enabled. Local `cargo build` and
`cargo test --lib` keep their existing profiles. Besides the unit tests, CI runs the integration
suites that bring their own SSH server: `sign_in`, `forwards` and `latency`, and on Linux also
`agent`. The benchmark and compatibility suites (`throughput`, `compat`) and the native window
tests (`window_frame`, `theme_picker`) are ignored tests that CI leaves out. CI builds and
tests on Windows and Linux (and packs the Linux tarball); macOS is only built for a release, as
its runners cost ten times as much. Branch pushes and pull requests run CI; pushing a tag does
not repeat those checks.

Both workflows upload Cargo's HTML timing reports as run artifacts for seven days. Download
`ci-build-timings` or `release-build-timings-<tag>` from the run's summary to see which crates
were rebuilt and how long compilation took. A cache hit saves dependency compilation; the
application and its final release LTO/link still need to build.

The installer is built from [packaging/komando.wxs](../packaging/komando.wxs) by
[packaging/build-msi.ps1](../packaging/build-msi.ps1) (WiX v5). CI builds it on every push, from
the CI program in `target\ci`, so a mistake in the installer shows up before a release. It asks whether to
install for the current user only (no administrator rights) or for all users; for scripted
installs, `msiexec /i Komando-0.2.0-x64.msi /qn` installs for the current user and adding `ALLUSERS=1`
installs for all users.

## Icon artwork and local shells

The icon picker combines Lucide with the embedded Devicon catalog. Devicon SVGs retain their
colors and work offline; [the asset notice](../assets/devicon/README.md) records the pinned
upstream revision, license and update command. Command Prompt keeps its native artwork.

Local Shells have an optional saved icon override. Automatic selection recognizes PowerShell
and WSL distribution names from `-d`, `--distribution` or `--distribution=...`. Unknown custom
distributions use the generic Linux icon; users can select another in Settings → Local shells.
Registered WSL distributions are read from the current user's Lxss registration, without
starting Linux. Discovery is cached; Restore detected shells refreshes it and retains manual
icons. Docker/Rancher internal utility distributions are excluded.

## Remote desktop checks

`cargo test --lib desktop --locked` covers framebuffer rectangles and overlapping CopyRect,
input mapping and focus-loss releases, certificate pins, old-library compatibility and
credential references. Loopback RFB servers exercise no-auth and password sign-in, the
remember-password event, a raw framebuffer update and keyboard input through the real VNC
adapter. Silent TCP peers check cancellation during both protocols' handshakes.

These tests do not establish compatibility with real Windows RDP, xrdp or VNC servers.
Before extending protocol coverage, verify real sign-in, certificate rejection and change,
SSH routes with Jump Hosts, disconnect/reconnect, desktop resizing, mixed DPI, keyboard
layouts and focus loss during a drag. Use disposable host data and credentials. RDP currently
requires TLS or NLA/NTLM; VNC supports none or classic password authentication. Unsupported
security modes must fail explicitly rather than weaken a server's configuration.

## Developing on Linux

Komando ships for Windows, Linux and macOS (see [ADR 0014](adr/0014-linux-and-macos-builds.md)).
Linux is handy for tests and screenshots. You need the X11 libraries (`libxcb`, `libxkbcommon`,
`libxkbcommon-x11`). Set `RUST_FONTCONFIG_DLOPEN=on` if fontconfig has no pkg-config file.
`KOMANDO_HOME` replaces the data folder (`~/.config/Komando` on Linux), so a test can keep its
hosts and vault separate. `examples/demo.rs` seeds a throwaway data folder and opens a few
sessions; it's what produced the README screenshot.
Set `KOMANDO_DEMO_THEME=dark` for README captures. `KOMANDO_DEMO_DESKTOPS=1` configures RDP
and VNC on the demo's web01 so their menu entries open configured desktops, without opening one.
`KOMANDO_DEMO_SFTP=1` opens its file browser once SSH connects; use a loopback server that
offers SFTP for that capture (`logserver` supplies benchmark output only). Capture the real
demo window with isolated fixture data, and check the resulting PNG before replacing an asset.
`KOMANDO_DEMO_ICON=light` selects the light app icon for captures; the default is dark.

### Output throughput

`throughput_sidebar_width_preparation` compares the terminal resize caused by collapsing
the sidebar (100 to 134 columns), with and without background allocation preparation:

```sh
cargo test --test throughput throughput_sidebar_width_preparation -- --ignored --nocapture --test-threads=1
```

With at least 4,096 Scrollback rows, a wider layout prepares row allocations off the UI
thread in batches of 128, while drawing at the old column count. Once ready, the usual
resize reflows the text and reports the new size to the shell. New requests cancel obsolete
preparation and compaction; abandoned allocations are reclaimed after the layout settles.
Short Scrollbacks still resize immediately. This also applies to widening the window and
closing the File Browser. It does not move the final reflow off the UI thread.

On the Linux development host, three runs with 100,000 log rows in the ordinary dev/test
profile gave a median blocking resize of 197.99 ms synchronously and 27.05 ms after
preparation. The longest sampled terminal-lock wait during preparation was 3.35 ms across
those runs. Total preparation plus resize was about 230 ms: this improves responsiveness
by moving allocations off the UI thread, rather than reducing total work. These are
headless resize measurements, not Windows sidebar click-to-paint measurements. The benchmark
also reports a 1,000-row case for comparison; the UI does not start a worker for that case.

`throughput_sftp_divider_resize` exercises a sequence of widths matching an SFTP divider
drag with 56,000 log rows:

```sh
cargo test --test throughput throughput_sftp_divider_resize -- --ignored --nocapture --test-threads=1
```

Narrowing a row with an empty tail now truncates it without allocating a temporary vector.
The median step while narrowing from 177 to 169 columns was 7.65 ms in the baseline run;
the medians after the change were 3.79, 3.97 and 3.94 ms. Steps that wrap real text still
need to move cells and can be much slower: the largest observed step remained over 100 ms.
These headless tests time synchronous reflow, not GPUI rendering or Windows SFTP interaction.

During widening, preparation groups up to 16 adjacent column widths so small drag movements
share work. The extra capacity is reclaimed when the layout settles. Reflows are spaced by
twice the previous reflow duration with a maximum interval of 100 ms, measured from the
previous reflow's start. Previously the interval was five times that duration, with no cap:
a 250 ms reflow could leave another second before the next requested width. Regression tests
cover both the cooldown and preparation coalescing. The current grid continues repainting
while a new column count waits; reflow itself is still synchronous.

`tests/throughput.rs` measures how fast SSH output gets into the terminal: an SSH server in
the test process prints 1,000,000 coloured log lines through the real Session path, while a
stand-in for the UI copies the screen every frame. A second test feeds the same output straight
into the parser, without SSH, and then the same lines with accented, Cyrillic and CJK messages. A third runs several busy Sessions at once while it types in a
quiet one, and reports how long the output took and how long each typed key took to show
(`KOMANDO_THROUGHPUT_SESSIONS`, default 8). A fourth types in a Session while a large file and
a folder of small files download, then upload, over SFTP on its connection, directly and through simulated
links with latency and limited bandwidth (`KOMANDO_THROUGHPUT_SFTP_MB` sets the direct file's
size, default 512). A fifth fills Scrollbacks of 100,000 and 500,000 lines and times what
holds the terminal locked: Copy output, resizes, and finding Command Marks. A sixth,
`throughput_scrollback_memory`, fills Scrollbacks of 100,000 and 500,000 lines at 177 and 350
columns with a mix of log lines, prompts, blank lines and long lines, and reports the memory they
take: live heap bytes (counted by an allocator that only counts during this test) and the
process's private bytes, per row and in total, how many cells hold anything, and what widening by
a column, halving the width and compacting the rows cost. Each case runs in a process of its own
(`KOMANDO_THROUGHPUT_MEMORY=500000x350` runs one), since Windows' heap keeps freed memory for
about a second before giving it back. A seventh floods a shell with coloured logs until Ctrl+C
arrives, directly and through the same simulated links, and reports how long the prompt took to show after Ctrl+C, how much output still arrived after
it, and how fast the flood ran; it compares the shell's paced SSH window (`src/session/pace.rs`)
with the fixed windows Komando used before (2 MiB, and 8 MiB). The simulated links queue
without limit, so how much output is still on its way when Ctrl+C is pressed is up to the SSH
window alone (`KOMANDO_THROUGHPUT_LINK` picks links by name). The
`throughput_through_jump_hosts_*` tests measure output through one and two Jump Hosts, next to the
same without one: the benchmark server also forwards `direct-tcpip` channels, so a chain of
them stands in for Jump Hosts, with simulated links (25 ms each way, 1 Gbit/s or 100 Mbit/s)
to the first hop or between every hop. `_log_lines` times the 1,000,000 log lines, `_typing`
types in one Session while four others print through the same Jump Host and then times how
long Ctrl+C takes to stop a Session that prints, and `_sftp` downloads a large file while it
types (`KOMANDO_THROUGHPUT_JUMPS` sets the most Jump Hosts, default 2). All of these servers
run on this machine, so its processors are shared and each simulated link limits a single
connection. They run on Windows too:

```sh
cargo test --release --test throughput -- --ignored --nocapture --test-threads=1
```

`KOMANDO_THROUGHPUT_LINES` and `KOMANDO_THROUGHPUT_RUNS` change the size and the number of runs.
`KOMANDO_THROUGHPUT_PORT` uses an SSH server already listening on that port instead.
`KOMANDO_PACE_DEBUG=1` prints each ping the shell's window pacing takes into account.

Komando builds against a patched russh in `vendor/russh` that lets a channel keep a receive
window of its own; `vendor/russh/KOMANDO.md` says what changed and how to carry it over to a
new russh release. It also builds against patched vte and alacritty_terminal in `vendor/vte` and
`vendor/alacritty_terminal`, which parse printable ASCII and write it to the grid in runs rather
than one character at a time, and read simple CSI sequences such as colour changes in one go;
their `KOMANDO.md` files say what changed, how to run their tests and how to carry the changes
over to new releases.

gpui's Windows backend is patched in `vendor/gpui-pre-windows`. While the window is dragged it
isn't redrawn at every step. Each new size is presented as soon as an edge is resized, and a
hidden or minimized window isn't invalidated at every vblank. Its `KOMANDO.md` lists what to
check on Windows after changing it. Keep its version in step with `gpui-pre`, which
`Cargo.toml` pins with `=`.

`src/ui/terminal_view/render_bench.rs` times the terminal's per-frame work without a window: copying
the screen out of the terminal and laying out its rows with GPUI's real text system, for a
static screen, typing, a log flood, a TUI redrawing a few rows, Unicode-heavy screens and a
screen of thousands of distinct glyph clusters. It reports the median and p95 per frame and how
many clusters were shaped (`KOMANDO_RENDER_FRAMES` sets the frame count, default 400). It runs on
Linux only:

```sh
cargo test --release --lib render_bench_ -- --ignored --nocapture --test-threads=1
```

To include rendering, run the whole app against `examples/logserver.rs`. It writes a key pair
for `examples/demo.rs`, floods each shell it opens and prints how long the terminal took to take
in the output. It also logs the size each terminal opens at and each size the app sends, with
the time it arrived. `KOMANDO_DEMO_PORT` sets its port (default 2222) for both. `KOMANDO_DEMO_SCROLLBACK`
sets the demo's Scrollback length, and `KOMANDO_DEMO_MEM=1` makes the demo print every 5 seconds
its live heap, peak heap and, on Windows, private bytes and working set (elsewhere the resident
set). `KOMANDO_DEMO_STRESS=1,8,8` opens 8 Sessions:

```sh
cargo run --release --example logserver -- target/bench 1000000 3
KOMANDO_DEMO_DIR=target/bench/data KOMANDO_DEMO_KEY=target/bench/client_ed25519   KOMANDO_DEMO_HOST_KEY=target/bench/host_ed25519.pub KOMANDO_DEMO_STRESS=1,1,1   cargo run --release --example demo
```

### Terminal compatibility tests

`tests/compat.rs` runs real programs (vim, htop, tmux, less, bash, and an SSH session against
a throwaway local `sshd`) in a real pseudo-terminal with Komando's terminal settings, and drives
them with Komando's own key, mouse and wheel encoding. They need those programs installed, so
they are ignored by default:

```sh
cargo test --test compat -- --ignored --test-threads=1
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
TMPDIR=~/.cache/komando-stress cargo test --test compat compat_sftp_stress -- --ignored --nocapture
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
MSI puts next to `komando.exe`. Downloads are checked against the size and SHA-256 digest GitHub
lists.

On Linux and macOS the check is the same, but the update button only opens the release page, and
nothing is installed. An update is offered only once that platform's own file
(`Komando-<version>-linux-x86_64.tar.gz` or `Komando-<version>-macos-arm64.dmg`) is attached, so a
release whose Linux build is still running isn't offered yet. Windows files never count for them.

To try the flow against the current release, start Komando with `KOMANDO_PRETEND_VERSION=0.0.1`
(on macOS from Terminal: `KOMANDO_PRETEND_VERSION=0.0.1 /Applications/Komando.app/Contents/MacOS/komando`).
On Linux and macOS the check uses a proxy only from `HTTPS_PROXY`/`ALL_PROXY` and trusts only
the bundled web roots; the system's proxy settings and certificate store are Windows-only.

### Profiling

Set `KOMANDO_PERF=1` and Komando prints, every 5 seconds, how often the hot paths ran and how
long they took (window renders, terminal grid, minimap, command rail, output wake-ups). Off by
default; each probe is then a single atomic load.

Set `KOMANDO_STARTUP_TRACE=1` and Komando prints each startup step with the time since the
process was created, so the time Windows spends loading the executable is included:

```
startup      9.6 ms  (+   9.6)  main
startup    188.9 ms  (+ 178.2)  platform ready
startup    229.3 ms  (+  36.6)  window created
startup    246.0 ms  (+   1.6)  first frame drawn
```

Release builds have no console, so redirect stderr to a file
(`Start-Process komando.exe -RedirectStandardError trace.txt`), or read the same lines from the log
file. On a fast PC most of startup is gpui's platform setup: DirectWrite (~45 ms) and creating
the Direct3D device, which loads the GPU driver (~140 ms). `platform::warm_up_gpu` starts that
driver load on a background thread first thing, so it overlaps with DirectWrite. To see how
startup feels on a slow PC, pin Komando to one or two CPUs (`SetProcessAffinityMask`) and run
busy loops on the same CPUs; a Job object's CPU rate cap is enforced too coarsely to slow a
startup this short.

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
cargo test --release --test compat compat_many_projects_connections_and_downloads -- --ignored --nocapture
```

Three smaller workloads compare the former algorithm with the optimized production path
in the same process. They cover 40-row highlighted repaints in both Themes, 10/100/500
Projects, and 1,000 queued download reservations (32 active) with identical filenames:

```sh
cargo test --release --lib stress_ -- --ignored --nocapture --test-threads=1
```

These print timings without machine-dependent pass/fail thresholds. SSH tests run entirely against temporary
fixtures; they do not connect to saved Hosts or change a user's library.

## Design notes

- [GLOSSARY.md](../GLOSSARY.md): the terms used in code and docs (Project, Host, Session,
  Vault, …).
- [docs/adr](adr): decisions and why: Linux and macOS builds beside Windows, embedded SSH client,
  native GPU UI, master-password vault, a sidebar of hosts instead of tabs.
