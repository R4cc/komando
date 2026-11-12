# Performance investigation: 0.3.2

## Scope and selection

Inspected the terminal parser and rendering paths, highlighting, command marks, minimap,
Project/sidebar resolution, SSH session loop, SFTP file operations and transfers, Vault,
and update downloads. Selected three expensive, avoidable application costs in the requested
large-library, streaming-output and concurrent-download workloads:

1. Project resolution and sidebar construction: repeated linear searches through every
   Host, then rebuilding and cloning every item of a Project for each visible row. A
   500-Project library made the original headless window's initial sidebar/main build take
   about 1.10 seconds. Session startup also rebuilt the entire library for every Session.
2. Highlighted terminal repainting: every matched character recalculated its foreground
   contrast, including repeated floating-point exponentiation. Colours depend on the rule
   and light/dark mode, not on each individual character or frame.
3. Download destination reservation: every colliding filename retried filesystem metadata
   queries from the first candidate, synchronously on the shared SSH runtime. Concurrent
   downloads compounded this work and delayed other sessions' processing.

The terminal parser runs off the UI thread and already coalesces wake-ups. The minimap and
command rail already cache work: explicit 100,000-line tests confirmed these existing paths
remain bounded (see `minimap.log` and `marks.log`). SFTP already pipelines requests and
limits aggregate in-flight data; increasing concurrency blindly would consume more memory
and worsen slow-link cancellation. Vault key derivation was left unchanged.

## Changes

- Build one borrowed Host-ID index for each sidebar/library traversal, retaining Project
  order and the first Host for a duplicate ID. Visible rows carry direct Host/Local Shell
  indices. Project live-session counts avoid constructing its items. Startup resolves the
  library once, and a new Session looks up its current item directly.
- Prepare readable Highlight Rule colours for both modes when compiling rules. Preserve
  raw import/export colours, regex/capture matching, precedence, bold and custom backgrounds.
- Reserve download names on Tokio's blocking pool. Unused names keep a direct fast path;
  collisions use a directory snapshot and in-memory name checks. Exclusive creation still
  protects simultaneous downloads. A reservation abandoned before handoff cleans its partial.

## Measurements

Timing and test output are saved beside this report. All local measurements use optimized
Rust release builds on a four-CPU x86-64 Linux administration container. These are development
measurements, not Windows GPU or WAN guarantees. Scheduling and filesystem caches affect
wall-clock timings. Comparative microbenchmarks retain the former algorithm under test-only
code and run it against the optimized path using the same inputs and process.

With other local build work paused during the comparisons:

| Workload | Former algorithm | Optimized | Speedup |
| --- | ---: | ---: | ---: |
| 10 Projects / 200 Hosts, resolve all | 83.082 µs | 10.072 µs | 8.2× |
| 100 Projects / 2,000 Hosts, resolve all | 11.492 ms | 0.111 ms | 104× |
| 500 Projects / 10,000 Hosts, resolve all | 304.384 ms | 0.628 ms | 485× |
| Highlight 40 rows, dark mode | 165.191 µs | 79.853 µs | 2.1× |
| Highlight 40 rows, light mode | 346.464 µs | 79.599 µs | 4.4× |
| Reserve 1,000 identical download names, 32 active | 2.205 s | 1.221 s | 1.8× |

These are costs of the specific operations, not whole-app frame times or total download
speed. Project timings include rebuilding the index each pass. Highlight timings include
matching and consuming each character's style, with the old path adjusting contrast at
that point. Download reservation timings include verifying uniqueness and cleaning all
1,000 partial files. See `comparisons.log`.

In the mixed workload, 32 handshakes took 309 ms, concurrent downloads took 418 ms, and all
terminal output, files and disconnections were verified in 1.95 seconds (excluding fixture
creation). A repeat alongside compilation took 3.20 seconds in total; timings are reported
as observations, not latency guarantees. See `mixed-load.log` and `compatibility.log`.

The SFTP test process reported a peak resident size of 63,116 KiB before and 57,544 KiB
after; this is the transfer harness only, not a bound on the GUI or all open Sessions.
Both full suites passed. Delayed-link and bandwidth-limited transfer rates remain subject
to network limits and scheduling.

Native GUI samples with 100 Projects / 2,000 Hosts / 24 streaming Sessions reduced
`sidebar + main` build time from 31.113 ms to 0.644 ms in the recorded five-second windows.
The optimized GUI authenticated all 8/24/32 requested SSH Sessions in its three scenarios;
see `gui-summary.log` and `gui-*.log`. The largest scenario held an idle Local Session in
front of streaming background Sessions; its empty later profiling windows confirm no
continuous workspace repaint. These are observational software-rendering samples, distinct
from the controlled operation benchmarks above.

## Validation

- Unit suite: 146 passed. Includes prepared-vs-original highlight styles in both modes,
  Host lookup order/missing IDs/duplicates/deletion, and concurrent file/folder reservations
  that preserve existing final and partial files.
- Mixed load: 500 Projects / 10,000 Hosts serialized and reloaded, 32 real simultaneous SSH
  handshakes, 320,000 Unicode log lines, 32 simultaneous byte-verified 1 MiB downloads with
  matching names, filled bounded Scrollback, verified final lines and clean disconnection.
- SFTP stress before and after: 128 MiB downloads and uploads; 1,001-file nested folders;
  two large and two folder downloads together; 1,000 independent downloads started together;
  cancellation cleanup, duplicate uploads and connection reuse. Every file is byte-checked.
  Runs cover direct, 25 ms one-way latency, and 25 ms one-way plus 1 MiB/s bandwidth limit.
  Transfer throughput on delayed/limited links is still dominated by the network; individual
  timings vary, and this change does not claim a universal SFTP throughput multiplier.
- Terminal compatibility: real bash, less, direct SSH, Jump Host SSH, SFTP and mixed-load
  checks passed. Five checks for vim, htop and tmux explicitly skipped because those programs
  are absent. See `compatibility.log`; its test-runner success count includes those skips.
- Existing 100,000-line minimap and command-mark benchmarks passed.
- Headless native GUI loads use 10/100/500 Projects, 200/2,000/10,000 Hosts and 8/24/32 SSH
  Sessions, with an idle foreground Local Session in the largest configuration. Software
  rendering under Xvfb cannot establish Windows hardware frame rates or input latency.

## Reproduce

Install Rust and the Linux native UI dependencies described in `docs/development.md`.
`sshd` and `ssh-keygen` must be on PATH for SSH tests. Use a disk-backed TMPDIR for SFTP:

```sh
cargo test --lib --locked
cargo test --release --lib stress_ -- --ignored --nocapture --test-threads=1
cargo test --release --lib compat_many_projects_connections_and_downloads -- --ignored --nocapture
cargo test --release --lib compat_sftp_stress -- --ignored --nocapture
cargo test --release --lib scrolling_a_long_log -- --ignored --nocapture
cargo test --release --lib finding_marks_in_a_full_long_scrollback -- --ignored --nocapture
```

`KOMANDO_STRESS_MB`, `KOMANDO_STRESS_LATENCY_MS` and `KOMANDO_STRESS_SLOW_KBPS` control
transfer sizes and simulated links. All SSH tests use temporary local server fixtures.
No saved Hosts, operational credentials, homelab services or real user files are modified.
