# Faster terminals and SSH clients: what Komando could adopt

Surveyed 2026-11-14 by three parallel source reviews. They skipped projects already studied
(Alacritty, vte, Ghostty, TildaZ, Tabby, Windows Terminal's benchmarks).

- [Parsing, cells and Scrollback](parser-and-scrollback.md): kitty, foot, contour, Rio, WezTerm,
  Windows Terminal, zutty, libvterm, xterm.js, vt100.
- [Rendering and frame pacing](rendering-and-frame-pacing.md): refterm, Windows Terminal's
  AtlasEngine, kitty, WezTerm, foot, zutty, Rio, contour, Mosh, and GPUI 0.3.12.
- [SSH and I/O](ssh-and-io.md): HPN-SSH, OpenSSH, libssh/libssh2, PuTTY, Eternal Terminal, Mosh,
  russh and its issue tracker, kitty, foot, WezTerm, Rio, Windows Terminal ConPTY.
  Its standalone benchmark is in [bench/](bench/), built against `vendor/`
  (`cargo run --release --manifest-path docs/research/terminal-speed-survey/bench/Cargo.toml --bin e2e`).

Only the SSH review measured anything (cipher speed, SSH versus parser-only, window and packet
size, LTO, `target-cpu`, mimalloc), on the Windows 11 Ryzen 9800X3D machine. The other gains
below are estimates from reading code. Re-measure before keeping any change.

## Where the time goes today

1. **The parser itself.** It runs at about 180–210 MB/s. On loopback, SSH and plumbing add almost
   nothing: 0.54–0.56 s over SSH against 0.51–0.55 s for the parser alone in the standalone
   benchmark. On its own, the SSH stage runs at about 530 MB/s with chacha20 and 660 MB/s with
   AES-GCM.
2. **Scrollback memory.** While the Scrollback fills, each new line page-faults about 4.2 KB of
   fresh rows; the row-pool prototype addresses this. Once it is full, the row being reused was
   last touched 100,000 lines earlier and comes from main memory. That reset costs an estimated
   25–30% of a line.
3. **The minimap holds the terminal lock while it highlights** (`src/ui/minimap.rs:677-680`).
   During a flood it runs every highlight rule over about 490 new lines per frame, about 6× the
   main grid's text, while the parser waits for the lock.
4. **Unicode `\b` in highlight rules.** In regex-automata 0.4, a Unicode word boundary makes the
   lazy DFA give up on the first non-ASCII byte, and the whole set falls back to the PikeVM. This
   is the confirmed cause of the ~6 ms per frame on non-ASCII rows.
5. **One lock and one wakeup per SSH packet.** A real sshd sends terminal output in packets of
   about 4 KiB.

Frame pacing is already right: wakeups are merged, GPUI draws at most once per vsync, and the
grid lays out after unlocking. Compression is correctly never negotiated.

## Recommended order

The gains overlap, so don't add them up. Every step has an existing benchmark to check it:
`throughput_parser_only`, `throughput_coloured_log_lines_over_ssh`, `render_bench_grid`, and
`KOMANDO_PERF=1` probes.

| # | Change | From | Effort | Expected gain | Confidence |
|---|---|---|---|---|---|
| 0 | Measure first: run the 1M-line SSH benchmark with the minimap on and off, with `FIXED_WINDOW` at 8 MiB, and without the test's UI loop locking the terminal | — | An hour | Shows the ceiling for 1–3 | High |
| 1 | **Minimap off the lock:** copy rows under the lock and highlight after unlocking. While output streams, use plain colours or update it 4–10 times a second, then highlight fully after 100 ms of quiet. Add a two-generation cache instead of clearing all of it | Windows Terminal (URL regex after 100 ms idle), kitty | 0.5–1.5 days | 1–30 ms less lock time per frame; perhaps +10–40% flood throughput with the minimap visible | Medium |
| 2 | **ASCII word boundaries:** compile `\b` as `(?-u:\b)`, keeping the rest of Unicode | regex-automata internals; Tabby's JavaScript `\b` is ASCII-only too | 0.5 day | Non-ASCII highlighting from ~6 ms to under 1 ms per frame, and the same in the minimap | Medium-high |
| 3 | **Prefetch the next row to be reused** in `Grid::scroll_up`, with write intent (vendor patch) | kitty (changelog claims ~35% with large Scrollback) | About 20 lines | 10–25% with a full Scrollback | Medium-high |
| 4 | **Parse every queued SSH packet under one lock** (up to ~256 KiB), with one Wakeup and one `pace.output` per batch | Rio, kitty, WezTerm | About 40 lines in `ssh.rs` | 4–6% measured; 10–15% when the CPU is busy | High on direction |
| 5 | **Prefer AES-256-GCM / AES-128-GCM over chacha20-poly1305** when the CPU has AES hardware (every Windows 11 CPU) | OpenSSH on AES-NI hardware, HPN-SSH | Config change | 8× cheaper decryption (64 → 8 ms CPU per 100 MB); matters with several flooding Sessions and once the parser is faster | High on cipher cost |
| 6 | **PGO** trained on the throughput and render benchmarks | foot (1.35–1.54× on vtebench) | Build/CI only | 10–25% (unmeasured for Komando) | Medium |
| 7 | **Fill text runs from a ready-made cell** in `input_ascii`, without cloning and dropping `Option<Arc>` for each cell | kitty, Rio | Small vendor patch | 3–8% | Medium |
| 8 | **Socket receive buffer at least as large as the SSH window** (a fixed SO_RCVBUF turns off Windows TCP auto-tuning) | Microsoft networking guidance | Very small | Up to 4× on long, fast links through Jump Hosts; none on direct connections | Medium |
| 9 | Copy raw cells under the lock, resolve colours and selection after unlocking | Rio, contour | 0.5 day | Lock time ~0.3 → ~0.05 ms per frame | Medium-high |
| 10 | Cache highlight results per row, keyed by its text | kitty, WezTerm | 0.5–1 day | Near zero for unchanged rows; nothing in floods | High |
| 11 | One fused SSE2 scan for escapes, UTF-8 validity and printable runs in vte | kitty `simd-string` | Small to medium | 3–8% (text runs average ~12 bytes) | Medium-low |
| 12 | Repaint at most every ~16 ms while output streams; typed echo exempt | kitty `repaint_delay`, foot timers | ~2 h | Only on 120/144 Hz screens: −50–60% UI CPU and lock time | Medium |
| 13 | Try mimalloc; keep it only for ≥3% and ≤2 MB more idle memory | rust-analyzer | Very small | 5.5% parser-only, ~0% over SSH; 1–4 MB more idle memory | Low-medium |

### Larger options, for after 1–7 are measured

- **8-byte cells with styles stored once** (Rio forked alacritty's grid this way). An estimated
  15–25% steady state, a 40–60% cheaper fill, and about 280 MB less at a 100,000-line
  Scrollback. Every cell reader in Komando changes, and Command Marks become per-row.
- **A compact history tier**: rows stored as text plus attribute runs once they are past a hot
  margin (WezTerm, Windows Terminal, libvterm). About 25× less Scrollback memory, but
  selection, search, reflow, marks and the minimap all have to read it.
- **Fewer copies in russh's receive path.** Offer it upstream rather than growing the vendored
  patch. About 3% today, more once the parser is faster.

### Measured or reasoned out: don't adopt

- **Fat LTO with `codegen-units = 1`, and `target-cpu=x86-64-v3`:** measured, no faster; fat LTO
  was sometimes slower.
- **`panic = "abort"`:** one Session's panic would close them all.
- **zlib compression:** it caps throughput and leaks length information.
- **Bigger packets or windows on loopback:** measured from 16 to 64 KiB and 64 KiB to 32 MiB, no
  difference.
- **HPN-SSH's multithreaded or "none" ciphers:** AES-GCM is simpler, and "none" isn't acceptable.
- **Mosh's approach:** it drops output and keeps no Scrollback.
- **A GPU cell grid like refterm or zutty:** GPUI 0.3.12 has no custom shader hook, and
  `draw_surfaces` does nothing on Windows. It would mean a fork for 1–2 ms of UI time that isn't
  under the lock.
- **contour's `TrivialLineBuffer`:** deprecated upstream.

## Outcome of the first round

Measured on Windows 11 (Ryzen 7 9800X3D), each build alternated with the one before it,
medians of 3. Another session was building and benchmarking on the same machine at times.

| Change (survey #) | Benchmark | Before | After |
|---|---|---|---|
| Prefetch the next recycled row (3) | `throughput_parser_only`, Scrollback 100,000 | 0.481 s | 0.341 s |
| | same, Scrollback 10,000 / 0 | 0.283 / 0.249 s | 0.269 / 0.250 s |
| | `throughput_coloured_log_lines_over_ssh` | 0.548 s | 0.443 s |
| Parse every queued SSH message at once (4) | same | 0.442 s | 0.410 s |
| AES-GCM preferred (5) | same | 0.410 s | 0.392 s |
| ASCII word boundaries (2) | 42 non-ASCII rows, bundled rules (62, most with `\b`) being written on `main` | 5.02 ms | 18 µs |
| | same, the 69 rules released in 0.4.2 | 20 µs | 16 µs |
| Minimap highlights after unlocking (1) | `minimap_during_a_flood`: terminal lock held per repaint, 300 new lines per frame | ~0.45 ms | ~0.16 ms |

Together: 1,000,000 coloured log lines over SSH with a 100,000-line Scrollback went from
0.548 s to 0.392 s (184 to 257 MB/s).

- The minimap was cheaper than estimated in the rendering review (1–30 ms) once word
  boundaries were ASCII: about 0.4–0.5 ms per repaint during a flood. Splitting it still takes
  two thirds of that out from under the lock; the "plain colours while streaming" part of item 1
  wasn't needed.
- The 6 ms per frame on non-ASCII rows came with the rewritten rule set on `main` (53 of 62
  rules with `\b`); the rules released in 0.4.2 have only two, and were already fast.
- Item 7 (skip cloning `extra` per cell in `input_ascii`) measured within noise (1–2%) and was
  dropped.
- Not tried yet: PGO (6), mimalloc (13), the fused SIMD scan (11), copying raw cells under the
  lock (9), per-row highlight caching (10), and the larger options.

## Interactions with work in progress

- The row-pool prototype (`src/session/row_pool.rs`, `vendor/alacritty_terminal/src/grid/storage.rs`)
  covers the fill-phase allocation; item 3 targets the steady state that remains after it.
- The bundled highlight rules are being rewritten in the working tree. Item 2 should apply when
  the rules compile, to user rules too. It should be tested on the new rule set, so it changes no
  match on ASCII text.
