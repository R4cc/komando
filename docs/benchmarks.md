# Benchmarks

The numbers in the README, how they were measured, and where Komando is not ahead.

Measured on 2026-11-13 with the released **Komando 0.3.6** portable EXE, against
**Windows Terminal** running the **OpenSSH client that ships with Windows** (`ssh.exe`), the
usual way to reach a Linux server from Windows 11 without extra tools.

## Machine

| | |
|---|---|
| OS | Windows 11 Pro, build 26300 |
| CPU | AMD Ryzen 7 9800X3D (8 cores) |
| GPU | NVIDIA GeForce RTX 5070 Ti |
| RAM | 32 GB |
| Display | 2560 × 1440 at 100 % scale |

Both terminals connected to the same SSH server on `127.0.0.1`, with the same key. The window was
about 1600 × 1000 pixels in both.

## Results

### Startup

Time from starting the process until its window is on screen, five launches for Komando (with an
empty data folder and with a library of projects and hosts, the same either way) and three for
Windows Terminal.

| | Median | Range |
|---|---:|---:|
| Komando | **235 ms** | 224–242 ms |
| Windows Terminal | 351 ms | 350–358 ms |

Komando's window is on screen **1.5× sooner**.

### Memory

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

### Download size

| | Size |
|---|---:|
| Installer (`Komando-0.3.6-x64.msi`) | 13.3 MB |
| Portable (`Komando-0.3.6-x64.exe`) | 41.7 MB |

Everything is in that one file: the SSH and SFTP client, the fonts and the icons.

### Throughput: where Windows Terminal is ahead

The server printed 1,000,000 coloured log lines (95.6 MB) as fast as the terminal would take them,
three runs each:

| | Median | MB/s |
|---|---:|---:|
| Komando | 2.9 s | ~33 |
| Windows Terminal + OpenSSH | **1.4 s** | ~67 |

Windows Terminal finishes a flood like this about twice as fast. Komando is still well beyond what a
real log tail produces, and the window stays responsive while it streams, but on raw throughput it
is not ahead yet.

## Reproducing

The server was a small Python [asyncssh](https://asyncssh.readthedocs.io/) server with an
interactive fake shell. A `loggen <lines>` command wrote the log lines in blocks, waiting for the SSH
flow control after each one, and recorded how long the whole output took. Memory was read from the
`Win32_PerfFormattedData_PerfProc_Process` counters (`WorkingSetPrivate`), a few seconds after
the sessions had settled. Startup was timed from `CreateProcess` until a visible top-level window
belonging to the process appeared.

Earlier, Linux-based measurements of specific code paths (sidebar with 10,000 hosts, highlighting,
download name reservation) are in [performance/2026-11-12](performance/2026-11-12/REPORT.md).
