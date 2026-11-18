# Benchmarks

The numbers in the README, how they were measured, and their limits.

## Three-terminal overview

| Application | Startup | Memory, 1 idle SSH Session | Memory, 10 idle SSH Sessions | Processes, 10 Sessions | A million log lines | Measured |
|---|---:|---:|---:|---:|---:|---|
| Komando 0.8.4 | 203 ms | 29.7 MB | 36.5 MB | 1 | 0.37 s | 2026-11-18 |
| Windows Terminal + OpenSSH* | 351 ms | 29 MB | 127 MB | 21 | 1.4 s | 2026-11-13 |
| Tabby 1.0.238 | 687 ms | 319.6 MB | 380.8 MB | 5 | 3.73 s | 2026-11-18 |

\*The Windows Terminal row repeats the rounded historical figures below. Its version was not
recorded, its window was approximately 1600 × 1000, its Scrollback was the default 9,001 lines,
and its million-line fixture was reported as 95.6 MB. Komando and Tabby use the newer matched
104 × 40 grid, 100,000-line Scrollback and 100,778,552-byte fixture described next. The earlier
memory figures retain their originally reported MB units; original raw byte samples are not
available here. This overview is not a controlled three-way benchmark or a basis for speed-up
ratios against Windows Terminal. Its [historical summary](performance/2026-11-18/windows-terminal-historical.json)
is kept separately from the new raw trials.

## Komando 0.8.4 versus Tabby 1.0.238

Measured on 2026-11-18 on the Windows 11 machine described below, using the installed
**Komando 0.8.4** for startup and the installed **Tabby 1.0.238** for all Tabby measurements.
Komando's SSH measurements use a release-built launcher linked to the unchanged 0.8.4
library. It only seeds a disposable Project, unlocks a disposable Vault and opens the
requested Sessions through the normal application. It does not replace SSH, the parser,
rendering or the allocator. This is a whole GUI application measurement, not a headless parser
test; the SSH results are not measurements of the installed Komando executable itself.

The [raw trials and computed medians](performance/2026-11-18/results.json) and
[versions, executable hashes and source revisions](performance/2026-11-18/environment.json)
are included. The library's `src`, `vendor`, `Cargo.toml` and `Cargo.lock` matched the
`v0.8.4` source at `2f48773` when the launcher was built.

### Results

| Metric | Komando 0.8.4 | Tabby 1.0.238 | Tabby / Komando |
|---|---:|---:|---:|
| Warm launch to visible window, median | 203 ms | 687 ms | 3.4× |
| Startup range, five launches | 190–204 ms | 672–692 ms | |
| Private working set, 1 idle SSH Session | 29.7 MB | 319.6 MB | 10.7× |
| Memory range, all 15 samples | 29.5–30.2 MB | 318.8–321.2 MB | |
| Private working set, 10 idle SSH Sessions | 36.5 MB | 380.8 MB | 10.4× |
| Memory range, all 15 samples | 36.4–37.6 MB | 380.6–382.4 MB | |
| Client process count, 1 or 10 Sessions | 1 | 5 | |
| 1,000,000 coloured log lines, median | 0.367 s | 3.729 s | 10.1× |
| Throughput time range, five runs | 0.365–0.379 s | 3.699–3.815 s | |
| Throughput, median | 274.3 MB/s | 27.0 MB/s | 10.1× |

### Method

- **Startup:** five launches after one discarded warm-up launch, with no open Sessions.
  Timing starts immediately before `CreateProcess` and stops when the process has a visible,
  non-cloaked top-level window larger than 100 × 100 pixels. Both request a 1280 × 800 window.
  The same isolated data folder is reused across these launches so each application's caches
  can warm. This measures a warm launch to a visible window, not a cold boot or readiness to type.
- **SSH setup:** both use their embedded SSH clients against an AsyncSSH 2.24.1 fake shell on
  `127.0.0.1`. It generates a new Host Key and accepts the fixture connections without user
  authentication. No system `ssh.exe`, ConPTY, local shell, real remote host or network delay
  is involved. Ten Sessions use ten separate SSH connections, with Tabby's reuse disabled.
  Saved user profiles, credentials and application settings are not loaded or changed.
- **Terminal settings:** Cascadia Mono at size 14 and 100,000 lines of Scrollback in both.
  The final PTY size is checked as **104 columns × 40 rows**, the grid Komando's default
  1280 × 800 window gives with its normal sidebar and minimap. Tabby's window (867 × 748) is
  sized to give the same terminal grid. A matching outer window would give Tabby a larger grid
  because Komando also shows the sidebar.
  Other rendering features retain each application's defaults. Automatic update checks and
  Tabby's analytics are disabled in the disposable configurations.
- **Memory:** private resident working set, summed over the full client process tree, using
  `psutil.Process.memory_full_info().uss` on Windows. Three fresh application launches for each
  app and Session count; after every SSH shell is open, wait ten seconds, then take five
  samples one second apart. Report the median of the three launch medians. Sessions are idle
  with just the fixture prompt, not filled Scrollbacks. MB means **1,000,000 bytes**; this
  excludes shared resident pages, GPU allocations and the benchmark server.
- **Throughput:** five consecutive runs in one visible SSH Session, with a three-second pause
  before each run. Each prints the same **1,000,000 coloured log lines, 100,778,552 bytes**,
  pre-rendered into blocks of 600 lines outside the timer. The server writes each block and
  waits for SSH flow control, then sends `CSI 6 n`. Timing stops when the terminal returns the
  cursor-position reply after parsing the final line. This includes SSH transport, application
  output handling and terminal parsing under GUI load, but does not wait for GPU presentation
  of the final frame. The median is not an Internet-link, real-shell or general workload claim.

### Reproduce the Tabby comparison

From the repository root, on Windows with both applications installed in `C:\Program Files`:

```powershell
python -m venv target/tabby-benchmark/venv
& target/tabby-benchmark/venv/Scripts/python.exe -m pip install asyncssh==2.24.1 psutil==7.2.2 PyYAML==6.0.3
cargo build --release --locked --example benchmark_client
```

Run [compare.py](performance/2026-11-18/compare.py) sequentially, with only one benchmark
application open at a time. Startup and throughput each produce five runs by default:

```powershell
$benchmarkPython = 'target/tabby-benchmark/venv/Scripts/python.exe'
$benchmarkScript = 'docs/performance/2026-11-18/compare.py'
foreach ($benchmarkApp in @('komando', 'tabby')) {
    & $benchmarkPython -B $benchmarkScript --app $benchmarkApp --mode startup
    & $benchmarkPython -B $benchmarkScript --app $benchmarkApp --mode throughput
}
foreach ($benchmarkRepeat in 1..3) {
    foreach ($benchmarkSessions in @(1, 10)) {
        foreach ($benchmarkApp in @('komando', 'tabby')) {
            & $benchmarkPython -B $benchmarkScript --app $benchmarkApp --mode idle --sessions $benchmarkSessions
        }
    }
}
```

Output and disposable configurations go under the ignored `target/tabby-benchmark` directory.
Move earlier trials out of it first, because the summary takes the newest complete trials it
finds. The launcher refuses an existing data directory. The script terminates only the
processes it started. If font metrics, display scaling or Komando's layout differ, the run
stops and reports the grid it found. Set `GRID` in compare.py to Komando's grid and adjust
`TABBY_WINDOW` until Tabby reports the same grid before comparing results.
[summarize.py](performance/2026-11-18/summarize.py) can collect the latest complete trials and
regenerate the light and dark three-terminal graphics. Pass `--charts-only` instead of a results
directory to redraw them from the checked-in results and historical summary without collecting
new trials. The checked-in environment file records this
run; record the new machine and binary versions separately when repeating it.

## Earlier Tabby comparison: Komando 0.4.0

Measured on 2026-11-14 with the same method, except that Komando 0.4.0's default window gave a
**107 × 40** grid and Tabby's window was 892 × 748 to match it. Because the grids differ, these
figures are not directly comparable with the 0.8.4 run above.

| Metric | Komando 0.4.0 | Tabby 1.0.238 |
|---|---:|---:|
| Warm launch to visible window, median | 187 ms | 725 ms |
| Private working set, 1 idle SSH Session | 23.5 MB | 318.7 MB |
| Private working set, 10 idle SSH Sessions | 28.3 MB | 382.8 MB |
| 1,000,000 coloured log lines, median | 0.808 s | 4.189 s |

The [raw trials](performance/2026-11-14/results.json),
[environment](performance/2026-11-14/environment.json) and
[scripts](performance/2026-11-14/compare.py) for that run are kept unchanged.

## Earlier Windows Terminal comparison: Komando 0.3.6

Measured on 2026-11-13 with the released **Komando 0.3.6** portable EXE, against
**Windows Terminal** running the **OpenSSH client that ships with Windows** (`ssh.exe`), the
usual way to reach a Linux server from Windows 11 without extra tools.

### Machine

| | |
|---|---|
| OS | Windows 11 Pro, build 26300 |
| CPU | AMD Ryzen 7 9800X3D (8 cores) |
| GPU | NVIDIA GeForce RTX 5070 Ti |
| RAM | 32 GB |
| Display | 2560 × 1440 at 100 % scale |

Both terminals connected to the same SSH server on `127.0.0.1`, with the same key. The window was
about 1600 × 1000 pixels in both.

### Results

#### Startup

Time from starting the process until its window is on screen, five launches for Komando (with an
empty data folder and with a library of projects and hosts, the same either way) and three for
Windows Terminal.

| | Median | Range |
|---|---:|---:|
| Komando | **235 ms** | 224–242 ms |
| Windows Terminal | 351 ms | 350–358 ms |

Komando's window is on screen **1.5× sooner**.

#### Memory

Private working set: the "Memory" column in Task Manager, summed over every process involved.
Komando is a single process. Windows Terminal runs `WindowsTerminal.exe`, plus one
`OpenConsole.exe` and one `ssh.exe` per tab.

| | Komando | Windows Terminal + OpenSSH |
|---|---:|---:|
| Open, no sessions | **21 MB** | — |
| 1 SSH session | **22 MB** | 29 MB (3 processes) |
| 10 SSH sessions | **29 MB** (1 process) | 127 MB (21 processes) |

With ten sessions Komando uses **4.3× less memory**. Each extra session costs Komando well under
1 MB. CPU use with ten idle sessions was 0 % for both.

Memory grows with scrollback. Komando keeps 100,000 lines per session by default (Windows Terminal
keeps 9,001), so after a session has printed millions of lines Komando holds far more history,
and more memory, than Windows Terminal does. With a 1600-pixel-wide window, a full 100,000-line
scrollback took about 380 MB. Lower it in Settings → Terminal if you don't need that much history.

#### Download size

| | Size |
|---|---:|
| Installer (`Komando-0.3.6-x64.msi`) | 13.3 MB |
| Portable (`Komando-0.3.6-x64.exe`) | 41.7 MB |

Everything is in that one file: the SSH and SFTP client, the fonts and the icons.

#### Throughput: where Windows Terminal is ahead

The server printed 1,000,000 coloured log lines (95.6 MB) as fast as the terminal would take them,
three runs each:

| | Median | MB/s |
|---|---:|---:|
| Komando | 2.9 s | ~33 |
| Windows Terminal + OpenSSH | **1.4 s** | ~67 |

Windows Terminal finishes a flood like this about twice as fast. Komando is still well beyond what a
real log tail produces, and the window stays responsive while it streams, but on raw throughput it
is not ahead yet.

### Reproducing

The server was a small Python [asyncssh](https://asyncssh.readthedocs.io/) server with an
interactive fake shell. A `loggen <lines>` command wrote the log lines in blocks, waiting for the SSH
flow control after each one, and recorded how long the whole output took. Memory was read from the
`Win32_PerfFormattedData_PerfProc_Process` counters (`WorkingSetPrivate`), a few seconds after
the sessions had settled. Startup was timed from `CreateProcess` until a visible top-level window
belonging to the process appeared.
