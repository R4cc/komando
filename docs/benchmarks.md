# Benchmarks

The numbers in the README, how they were measured, and their limits.

## Komando, Windows Terminal and Tabby (2026-11-27)

All three were measured on 2026-11-27 on the same machine, with the same SSH server, the same
terminal grid and the same Scrollback length:

| Application | Build measured |
|---|---|
| Komando | `main` at `79e2b82`: release 0.9.1 plus the changes listed under *Unreleased* in the [changelog](../CHANGELOG.md), built with `cargo build --release --locked` |
| Windows Terminal + OpenSSH | Windows Terminal 1.25.3131.0 (official portable build) running the `ssh.exe` that ships with Windows (OpenSSH for Windows 9.5p2) |
| Tabby | 1.0.238, installed |

The [raw trials and computed medians](performance/2026-11-27/results.json),
[versions, source revision and executable hashes](performance/2026-11-27/environment.json) and
[scripts](performance/2026-11-27/compare.py) are included. Lower is better in every row except
throughput in MB/s. Ranges are the fastest and slowest single sample.

### Machine

| | |
|---|---|
| OS | Windows 11 Pro, build 26300 |
| CPU | AMD Ryzen 7 9800X3D (8 cores) |
| GPU | NVIDIA GeForce RTX 5070 Ti |
| RAM | 32 GB |
| Display | 2560 × 1440 at 100 % scale |

### Results

| Metric | Komando | Windows Terminal + OpenSSH | Tabby |
|---|---:|---:|---:|
| Warm launch to visible window, median of 10 | **209 ms** | 233 ms | 705 ms |
| Range | 201–293 ms | 217–246 ms | 680–715 ms |
| Memory, 1 idle SSH Session | **29.7 MB** | 31.5 MB | 319.7 MB |
| Range, 15 samples | 29.4–30.3 MB | 31.4–31.8 MB | 318.8–320.8 MB |
| Memory, 10 idle SSH Sessions | **36.5 MB** | 131.6 MB | 383.8 MB |
| Range, 15 samples | 36.2–37.1 MB | 130.3–132.4 MB | 378.3–386.0 MB |
| Processes, 1 / 10 Sessions | **1 / 1** | 3 / 21 | 5 / 5 |
| 1,000,000 coloured log lines, median of 10 | **0.317 s** | 1.410 s | 3.669 s |
| Range | 0.294–0.411 s | 1.253–1.883 s | 3.641–3.900 s |
| Throughput, median | **318 MB/s** | 71.5 MB/s | 27.5 MB/s |

Komando and Windows Terminal open their window in about the same time: the difference is
smaller than Windows Terminal's own spread. With one Session their memory is also close; each
further Windows Terminal tab adds an `OpenConsole.exe` and an `ssh.exe`, about 11 MB, where a
further Komando Session adds under 1 MB. Windows Terminal's throughput varies more from run to
run than the others': an extra five-run trial that is not part of these results took 1.15–1.25 s
(see [Trials not used](#trials-not-used)).

### Method

- **Startup:** ten launches after one discarded warm-up launch, timed from immediately before
  `CreateProcess` until the process has a visible, non-cloaked top-level window larger than
  100 × 100 pixels. The same disposable data folder is reused across the launches so caches can
  warm. Komando and Tabby request a 1280 × 800 window and open no Session; Windows Terminal
  always opens a tab, here a Command Prompt, in a window sized for the 107 × 40 grid. This
  measures a warm launch to a visible window, not a cold boot or readiness to type.
- **SSH setup:** Komando and Tabby use their built-in SSH clients. Windows Terminal runs
  `ssh.exe` in each tab through ConPTY, with a disposable `ssh_config` (`-F`) that loads no user
  configuration, agent or keys. The server is an AsyncSSH 2.24.1 fake shell on `127.0.0.1`. It
  generates a new Host Key per trial and accepts connections without user authentication. There
  is no local shell, real remote host or network delay. Ten Sessions are ten separate SSH
  connections, with Tabby's connection reuse disabled. Saved user profiles, credentials and
  settings are neither loaded nor changed: Windows Terminal runs from its portable folder, whose
  `settings` folder the script rewrites for each trial. Komando's SSH trials use the
  `benchmark_client` example, which seeds a disposable Project and unlocks a disposable Vault,
  then runs the unchanged library from the same build.
- **Terminal settings:** Cascadia Mono at 14 px (10.5 pt in Windows Terminal) and 100,000 lines
  of Scrollback in all three. The server checks that every terminal reports **107 columns ×
  40 rows**, the grid Komando's default 1280 × 800 window gives next to its sidebar. Tabby's
  window (892 × 748) is sized to give that grid; Windows Terminal is given it with `initialCols`
  and `initialRows`. Other settings keep each application's defaults. Update checks and Tabby's
  analytics are off.
- **Memory:** private resident working set (`psutil` USS), summed over the client's whole process
  tree: for Windows Terminal, `WindowsTerminal.exe` plus each tab's `OpenConsole.exe` and
  `ssh.exe`. Three fresh launches for each application and Session count; ten seconds after
  every shell is open, five samples one second apart. The figure is the median of the three
  launch medians. Sessions are idle with just the fixture prompt, not full Scrollbacks. MB means
  **1,000,000 bytes**. Shared pages, GPU memory and the server are excluded.
- **Throughput:** ten consecutive runs in one visible SSH Session, with a three-second pause
  before each. Each prints the same **1,000,000 coloured log lines, 100,778,552 bytes**,
  pre-rendered in blocks of 600 lines outside the timer. The server writes each block, waits for
  SSH flow control, then sends `CSI 6 n`; timing stops when the cursor-position reply arrives,
  which the terminal sends after reading the last line. In Windows Terminal the reply comes back
  through ConPTY. The time includes SSH transport, output handling and parsing under GUI load,
  but not the GPU presenting the final frame. It is not a claim about Internet links, real
  shells or other workloads.
- **Order:** all startups, then all throughput runs, then the idle launches, alternating
  applications, with only one benchmark application open at a time.

### Trials not used

[superseded.json](performance/2026-11-27/superseded.json) keeps the complete trials taken the
same day that are not in the results above. A first pass measured Komando's **v0.9.1 release**
instead of `main`: 202 ms to a visible window, 30.9 and 36.8 MB with 1 and 10 Sessions, and
0.402 s (0.395–0.413 s) for the million log lines, against 0.317 s on `main`. That pass used five
throughput runs. Windows Terminal's took 1.29–2.48 s (median 1.68 s), so throughput was repeated
for all three applications with ten runs. An extra five-run Windows Terminal trial in between
took 1.15–1.25 s (median 1.16 s).

### Reproduce

From the repository root, on Windows with Tabby installed in `C:\Program Files\Tabby`:

```powershell
python -m venv target/tabby-benchmark/venv
& target/tabby-benchmark/venv/Scripts/python.exe -m pip install asyncssh==2.24.1 psutil==7.2.2 PyYAML==6.0.3
cargo build --release --locked --bin komando --example benchmark_client
gh release download v1.25.3131.0 -R microsoft/terminal -p 'Microsoft.WindowsTerminal_1.25.3131.0_x64.zip' -D target/bench-downloads/wt
Expand-Archive target/bench-downloads/wt/Microsoft.WindowsTerminal_1.25.3131.0_x64.zip target/wt-portable
```

Then run [compare.py](performance/2026-11-27/compare.py) with one application at a time, and
summarize:

```powershell
$benchmarkPython = 'target/tabby-benchmark/venv/Scripts/python.exe'
$benchmarkScript = 'docs/performance/2026-11-27/compare.py'
foreach ($benchmarkApp in @('komando', 'wt', 'tabby')) {
    & $benchmarkPython -B $benchmarkScript --app $benchmarkApp --mode startup --runs 10
}
foreach ($benchmarkApp in @('komando', 'wt', 'tabby')) {
    & $benchmarkPython -B $benchmarkScript --app $benchmarkApp --mode throughput --runs 10
}
foreach ($benchmarkRepeat in 1..3) {
    foreach ($benchmarkSessions in @(1, 10)) {
        foreach ($benchmarkApp in @('komando', 'wt', 'tabby')) {
            & $benchmarkPython -B $benchmarkScript --app $benchmarkApp --mode idle --sessions $benchmarkSessions
        }
    }
}
& $benchmarkPython -B docs/performance/2026-11-27/summarize.py target/benchmark-2026-11-27
```

Output and disposable configurations go under the ignored `target/benchmark-2026-11-27`
directory. The summary takes the newest complete trials it finds there, so move earlier trials
out first. The script stops only the processes it started. If font metrics, display scaling or
Komando's layout differ, a run stops and reports the grid it found: set `GRID` in compare.py to
Komando's grid and adjust `TABBY_WINDOW` until Tabby reports the same grid.
[summarize.py](performance/2026-11-27/summarize.py) writes `results.json` and the README's light
and dark charts, `assets/benchmark-light.svg` and `assets/benchmark-dark.svg`; with
`--charts-only` it redraws them from the checked-in results. When repeating the run elsewhere,
record the machine and binary versions in `environment.json`.

## Earlier Tabby comparison: Komando 0.8.4

Measured on 2026-11-18 with the method above, except: five startup launches and five throughput
runs, Komando's installed 0.8.4 release for startup, and a **104 × 40** grid (Tabby window
867 × 748), the grid Komando 0.8.4's default window gave. Windows Terminal was not measured
again then; the README showed its 2026-11-13 figures from the last section below.

| Metric | Komando 0.8.4 | Tabby 1.0.238 |
|---|---:|---:|
| Warm launch to visible window, median | 203 ms | 687 ms |
| Private working set, 1 idle SSH Session | 29.7 MB | 319.6 MB |
| Private working set, 10 idle SSH Sessions | 36.5 MB | 380.8 MB |
| 1,000,000 coloured log lines, median | 0.367 s | 3.729 s |

The [raw trials](performance/2026-11-18/results.json),
[environment](performance/2026-11-18/environment.json) and
[scripts](performance/2026-11-18/compare.py) for that run are kept unchanged.

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
