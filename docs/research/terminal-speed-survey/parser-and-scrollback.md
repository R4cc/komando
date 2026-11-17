# How faster terminals turn bytes into cells and Scrollback, and what Komando could adopt

Researcher A — scope: parsing, the cell/line model, and Scrollback storage.
Date: 2026-11-14. Source inspection only: nothing below was built, run or benchmarked. Every
estimate for Komando is an engineering estimate from reading code, not a measurement.

Sources (shallow clones, read-only), commits:

| Project | Commit |
|---|---|
| kitty | [`8e5487e9`](https://github.com/kovidgoyal/kitty/tree/8e5487e9d4a1dd79c3a769e0ba82326b8ad7c37f) |
| foot | [`8768826c`](https://codeberg.org/dnkl/foot/src/commit/8768826c887c27721fddbf4670e27215b201bce2) |
| contour | [`8aa53133`](https://github.com/contour-terminal/contour/tree/8aa5313325b66561b81120464aebb32a4dce6af2) |
| WezTerm / termwiz | [`e1eae81a`](https://github.com/wezterm/wezterm/tree/e1eae81a56be8b5487dc77a29616afa568fb57b7) |
| Rio (copa, rio-backend) | [`93983b78`](https://github.com/raphamorim/rio/tree/93983b78bbc9da340a92b657a063830a3ac6bf8e) |
| xterm.js | [`f70764dd`](https://github.com/xtermjs/xterm.js/tree/f70764dd3da05ebda0390605c6f9adbbea3f0e90) |
| libvterm (neovim fork) | [`934bc2fb`](https://github.com/neovim/libvterm/tree/934bc2fbf21800ac3458a499df8820ca5fb45fd3) |
| vt100-rust | [`fb0f54ec`](https://github.com/doy/vt100-rust/tree/fb0f54ec7222f4805f33cd3ca530229e0a0fe6ad) |

---

## 0. Baseline: what one log line costs Komando today

### The workload

`log_line` (`tests/support/log_line.rs:2`) emits about 100 bytes per line: 5 SGR sequences
(`ESC[2m`, `ESC[0m`, `ESC[<c>m`, `ESC[0m`, `ESC[36m`, `ESC[0m`), about 73 printable ASCII
characters in 6 runs, then `\r\n`. So per line the terminal does roughly: 6 `input_ascii` calls,
6 CSI dispatches (`terminal_attribute`), one `carriage_return`, one `linefeed` → `scroll_up(1)`.

### The cell and row model (vendored alacritty_terminal 0.26.0)

- `Cell` (`vendor/alacritty_terminal/src/term/cell.rs:133`): `c: char` (4) + `fg: Color` (4) +
  `bg: Color` (4) + `flags: u16` (2) + `extra: Option<Arc<CellExtra>>` (8) = **24 bytes**
  (asserted at `cell.rs:303`). Hyperlinks — and therefore Komando's Command Mark tags
  (`src/marks.rs:232`, a hyperlink on column 0) — live in `extra`.
- `Row<Cell>` (`grid/row.rs:17`) is a `Vec<Cell>` of the full terminal width plus `occ`, the
  high-water mark of written cells. 177 columns × 24 B = **4,248 B per row**, regardless of how
  much text the line holds (~73 cells here, so ~59% of every history row is blank padding).
- `Storage` (`grid/storage.rs`) is a ring of `Row`s; `rotate` is a modular add on `zero`.
  It grows in chunks of `MAX_CACHE_SIZE = 1000` rows (`storage.rs:14`, `initialize`).
  A 100,000-line history at 177 columns is ~425 MB of cells (matching the ~380 MB at ~1600 px
  recorded in `docs/benchmarks.md`).

### Per-line work once the Scrollback is full (read from the vendored code)

`linefeed` (`term/mod.rs:1500`) → `scroll_up(1)` (`:1562`) → `scroll_up_relative` (`:770`):

1. `self.selection.take().and_then(|s| s.rotate(..))` — cheap when there is no selection.
2. `Grid::scroll_up` (`grid/mod.rs:266`): `increase_scroll_limit` is a no-op when full;
   `raw.rotate(-1)` is one modulo; then `self.raw[last].reset(&template)` on the row that just
   fell off the top of history.
3. `Row::reset` (`grid/row.rs:89`) rewrites `occ` cells (here ~73–80) to the template:
   `*self = Cell { bg, ..Cell::default() }`, which also drops each old `extra` (a branch per cell).
   **That row was last touched 100,000 lines ago.** At 4.2 KB/row the history is ~425 MB, far
   larger than any L3 (even the 96 MB of the 9800X3D), so these ~1.8–1.9 KB (≈29 cache lines)
   are read from DRAM, modified, and later written back. It is sequential, so prefetchers help,
   but it is still ~3.5 KB of DRAM traffic per line.
4. vi-cursor adjust and `mark_fully_damaged` (a bool store).

Then `input_ascii` (`term/mod.rs:1149`) writes ~73 cells again (now in cache) with five stores
each (`c`, `fg`, `bg`, `flags`, `extra.clone()` — an `Option<Arc>` clone and the drop of the
old value, both branches since `extra` is `None`), plus the `WIDE_CHAR|WIDE_CHAR_SPACER` check
per cell. Each of the six SGRs goes through vte's CSI fast path and `terminal_attribute`.

**Estimate of where the steady-state ~0.4 µs/line goes** (Windows: 0.48 s for 1 M lines with
the first 100 k at 2.7×, solving 0.9 M·s + 0.1 M·2.7 s = 0.48 s gives s ≈ 0.41 µs/line, about
4 ns/byte). The Linux VM numbers in `docs/research/tildaz-performance-review.md` (130 / 110 /
78.8 MB/s at 0 / 10 k / 100 k Scrollback) give the same shape: solving for the steady-state
cost with a full Scrollback gives ~1.09 µs/line vs 0.775 µs with none (+40%), and ~0.90 µs
with 10 k (+16%; 10 k rows = 42 MB, close to cache-resident). So, **after the fill cost is
gone, a full 100 k Scrollback still costs ~30–40% more per line than no Scrollback**, and the
only Scrollback-dependent work in steady state is step 3 (the cold reset) plus the cold
cache lines `input_ascii` then writes. That is the "remaining per-line cost in `Term`": roughly
**25–30% of the per-line time is memory traffic caused by 24-byte, full-width history rows**;
the rest is parsing (~6 CSIs + 6 runs), SGR handling, and the cell writes themselves.

Rough split of the ~410 ns (Windows, full 100 k), by reading the code — uncertainty ±50%:

| Work | Est. ns/line |
|---|---:|
| vte: memchr for ESC, UTF-8 validate of runs, 6 CSI fast-path parses | 120–160 |
| 6 × `terminal_attribute` + `Attr` construction from params | 30–50 |
| 6 × `input_ascii` setup + ~73 cell writes (5 fields, Arc branch, wide check) | 80–120 |
| `linefeed`/`scroll_up`: cold-row `reset` (DRAM), ring rotate | 80–130 |
| `carriage_return`, damage bookkeeping, lock, `watch_output` | 10–20 |

The second big cost, **filling** the Scrollback (2.7×/line), is page faults on fresh 4.2 KB
rows; the in-progress `RowPool` (`src/session/row_pool.rs`, `RowSource` in
`vendor/alacritty_terminal/src/grid/mod.rs:27`) moves that allocation to a worker thread.
Anything that makes rows smaller attacks both costs at once.

---

## 1. kitty

kitty's model is not compact: a cell is a 12-byte `CPUCell` plus a 20-byte `GPUCell`
([line.h:38-87](https://github.com/kovidgoyal/kitty/blob/8e5487e9d4a1dd79c3a769e0ba82326b8ad7c37f/kitty/line.h#L38-L87)),
32 bytes, *more* than alacritty's 24. Its history ring
([history.c:17-48](https://github.com/kovidgoyal/kitty/blob/8e5487e9d4a1dd79c3a769e0ba82326b8ad7c37f/kitty/history.c#L17-L48))
stores full-width rows in 2048-line segments allocated with `mmap` (one allocation per 2048
lines, not per line; the comment cites fragmentation, kitty PR #10254). Lines that fall off the
end of the cell history are serialised to ANSI text into an optional "pager history" ring buffer
(`pagerhist_push`,
[history.c:299-315](https://github.com/kovidgoyal/kitty/blob/8e5487e9d4a1dd79c3a769e0ba82326b8ad7c37f/kitty/history.c#L299-L315)),
which is a second, cheap, text-only tier of Scrollback. kitty is nevertheless fast, and its
0.49 release notes
([docs/changelog.rst:43-62](https://github.com/kovidgoyal/kitty/blob/8e5487e9d4a1dd79c3a769e0ba82326b8ad7c37f/docs/changelog.rst#L43-L62))
list, with the author's own measurements (from `benchmark.py`/`kitten __benchmark__`, not
independently reproduced):

### 1a. Prefetch the next history slot before writing it — "~35%" with a large Scrollback

`prefetch_line_for_write`
([history.c:331-344](https://github.com/kovidgoyal/kitty/blob/8e5487e9d4a1dd79c3a769e0ba82326b8ad7c37f/kitty/history.c#L331-L344)),
called at the end of `historybuf_add_line` (`:353`), issues `__builtin_prefetch(p, 1, 0)`
(prefetch for write, no temporal locality) on every cache line of the slot that the **next**
scroll will overwrite. The comment says it outright: "The history buffer is typically much larger
than the CPU cache, so the line copy … stalls acquiring ownership of the destination cache
lines. Prefetching the slot that the next call will write to overlaps those misses with the
parsing/drawing of the next line, speeding up bulk text throughput by ~35%."

- **Why fast:** this is exactly Komando's cold-row problem (§0, step 3). The DRAM latency for
  ~29–70 cache lines is overlapped with one line's worth of parsing (~300 ns) instead of being
  paid inside `Row::reset`.
- **Applies to Komando:** directly, as a **small vendor patch**. In `Grid::scroll_up`
  (`grid/mod.rs:266`), after resetting the new bottom row, compute the storage index of the row
  that will be recycled next (the current topmost history row, i.e. `raw[topmost_line]` when the
  history is full, or the next not-yet-used slot while it grows) and prefetch its cell buffer
  `[0 .. occ * 24)` with `core::arch::x86_64::_mm_prefetch::<_MM_HINT_ET0>` (write intent;
  falls back to `_MM_HINT_T0` — the ET hints need PREFETCHW, available on all Zen and on Intel
  since Broadwell) — or simply touch-read one byte per cache line. Only `occ` cells need
  prefetching, since `reset` touches only those. One subtlety: `Row` holds a `Vec`, so the
  pointer to the cells is itself in the `Storage` ring (the 32-byte `Row` headers are dense,
  3.2 MB for 100 k rows, so that load is usually cheap).
  No effect on selection, search, reflow, resize, marks or highlighting: it changes no data.
- **Estimated gain for Komando:** 10–25% on steady-state lines with a full 100 k Scrollback
  (it can only hide the ~80–130 ns of §0 step 3; it does nothing for the fill phase and nothing
  with no Scrollback). Confidence: medium-high that it helps, medium on the size. It is the
  cheapest item in this report. Risk: on a 10 k Scrollback that is cache-resident it is a few
  wasted instructions per line.

### 1b. Fill a run of cells from a prebuilt template with wide stores

`draw_text_loop`'s batched path
([screen.c:1550-1608](https://github.com/kovidgoyal/kitty/blob/8e5487e9d4a1dd79c3a769e0ba82326b8ad7c37f/kitty/screen.c#L1550-L1608))
finds the printable run with SIMD (`printable_ascii_run_length`), checks the destination for
multicell cells four at a time, then `memcpy`s a 16-cell template array (inlined as wide
vector stores) and finally does one 32-bit store per cell for the character. Changelog: "~50%
for ASCII" for batching runs (which Komando already has via `input_ascii`) and "another ~10%"
for SIMD run finding + wide stores + skipping scroll bookkeeping.

- **Applies to Komando:** `Term::input_ascii` (`term/mod.rs:1185-1196`) writes five fields per
  cell and clones/drops an `Option<Arc>` per cell. When `template.extra` is `None` (always, for
  SGR-only output) and the destination run has no `extra` (checkable while scanning for
  `WIDE_CHAR`), the run can be written as `[Cell; N]` copies of a ready-made cell, then the
  characters patched in — no refcount branches. Small vendor patch, purely local; the existing
  differential test (`komando_tests.rs`) covers it.
- **Gain:** 3–8%. Confidence: medium.

### 1c. SIMD UTF-8 decoding and escape scanning (`simd-string`)

`utf8_decode_to_esc`
([simd-string.h:33](https://github.com/kovidgoyal/kitty/blob/8e5487e9d4a1dd79c3a769e0ba82326b8ad7c37f/kitty/simd-string.h#L33),
implementations in `simd-string-impl.h`, 128/256/512-bit variants) decodes UTF-8 to UTF-32 *and*
stops at ESC in one vector pass; the changelog reports 20% (ASCII) to 50% (multi-byte) gains
for that decoder, and +75% with AVX-512. kitty needs it because its whole pipeline is UTF-32.

- **Applies to Komando:** only partly. Komando's vte works on bytes and `&str`, which is
  already better for ASCII than expanding to 4-byte code points. Today the ground state makes
  **three passes** over each text byte: `memchr(ESC)` (SIMD), `str::from_utf8` (word-at-a-time
  ASCII fast path), and the scalar `is_printable_ascii` loop in `ground_dispatch`
  (`vendor/vte/src/lib.rs:644-805`). One SSE2 pass can replace all three for the common case:
  per 16 bytes, `movemask((b < 0x20) | (b >= 0x7f))` gives the first byte that is not printable
  ASCII; ESC, controls and ≥0x80 bytes all fall out of the same mask. Only when a ≥0x80 byte is
  found does the slow UTF-8 path run. Small/medium vendor patch in vte (unsafe SIMD with a
  scalar fallback; the existing `ground_dispatch_matches_upstream` test is the oracle).
- **Gain:** for this workload the runs between escapes are short (average ~12 bytes), so
  per-run setup dominates, not per-byte scanning: 3–8%. More on long plain-text lines.
  Confidence: medium-low.

### 1d. One lock per buffer, cheaper CSI arithmetic — "~30% on escape-heavy input"

Komando already locks once per `advance` call (`src/session/ssh.rs:691`) and already has a CSI
fast path, so this is done.

### 1e. Two-tier history (cells, then text) — design reference

kitty's pager history shows that a terminal can keep the *recent* Scrollback as cells and the
*older* Scrollback as serialised text (`line_as_ansi`). See §8 (recommendation) for how this
maps to Komando.

---

## 2. foot

foot's parser is a plain byte-at-a-time state machine with no SIMD
([vt.c:1101-1133](https://codeberg.org/dnkl/foot/src/commit/8768826c887c27721fddbf4670e27215b201bce2/vt.c#L1101-L1133)),
and printable ASCII goes one character at a time through a function pointer that is switched
between a "fast" and a "generic" printer whenever charset/insert-mode/sixel state changes
(`ascii_printer_fast`, `term_update_ascii_printer`,
[terminal.c:4111-4178](https://codeberg.org/dnkl/foot/src/commit/8768826c887c27721fddbf4670e27215b201bce2/terminal.c#L4111-L4178)).
Komando's `input_ascii` run path already does better than that. foot's speed comes from its
**data model**:

### 2a. A 12-byte cell; rare attributes live per row, not per cell

`struct cell { char32_t wc; struct attributes attrs; }` with `attributes` packed into 8 bytes:
8 SGR bits, `fg:24`, `bg:24`, 2-bit colour *source* for each (default / base16 / 256 / RGB), and
a few flags
([terminal.h:42-72](https://codeberg.org/dnkl/foot/src/commit/8768826c887c27721fddbf4670e27215b201bce2/terminal.h#L42-L72)).
Hyperlinks (OSC 8) and coloured/styled underlines are not in the cell at all: they are sorted
**column ranges in an optional per-row side table** (`struct row_data { uri_ranges;
underline_ranges; }`, `row->extra`,
[terminal.h:135-160](https://codeberg.org/dnkl/foot/src/commit/8768826c887c27721fddbf4670e27215b201bce2/terminal.h#L135-L160)).
The per-cell hot path only checks `if (unlikely(row->extra != NULL))`. Shell-integration prompt
marks are likewise per-row fields (`row->shell_integration`).

- **Why fast:** half the bytes of an alacritty cell, so half the memory traffic for writing,
  resetting and recycling Scrollback rows, and a cell write is two plain stores with no
  refcount. Published measurement: foot's [doc/benchmark.md](https://codeberg.org/dnkl/foot/src/commit/8768826c887c27721fddbf4670e27215b201bce2/doc/benchmark.md)
  (vtebench, 2022, i9-9900, 10 k Scrollback): **dense cells 45 ms vs Alacritty 0.10's 97 ms,
  light cells 4.4 vs 12.8 ms, scrolling ~equal (116 vs 122 ms)**. Old Alacritty, but the cell
  layout comparison still holds (alacritty's `Cell` is unchanged at 24 bytes).
- **Applies to Komando:** shrinking alacritty's `Cell` from 24 to 12 or 16 bytes is a
  **large vendor refactor**: `Cell` has `pub` fields used by Komando (`src/ui/minimap.rs:549-575`,
  `terminal_view.rs`, `search.rs`, `marks.rs`) and by all of alacritty_terminal (`extra`'s
  zero-width chars, underline colour, hyperlinks). A realistic 16-byte cell: `c: char` (4) +
  `fg`/`bg` as packed 32-bit `Color` (tag:8 + rgb/index:24, 4+4) + `flags: u16` + a `u16`
  index into a per-grid `extra` table instead of `Option<Arc<CellExtra>>` (8 → 2 bytes). Every
  `cell.extra` user (zerowidth, hyperlink, underline colour) changes; `Arc` reference counting
  becomes table GC on row reset. Risks: Command Marks are hyperlinks on column 0
  (`src/marks.rs:232`), so the table must survive Scrollback rotation exactly as `Arc` does;
  selection/search/reflow only see `c` and `flags` and are unaffected except through `extra`
  (zero-width chars in `bounds_to_string`, reflow copies cells and must copy the index).
  The upstream crate's `Color` enum layout would have to be replaced in `vendor/vte` too
  (`vte::ansi::Color` is what `Attr` carries).
- **Estimated gain:** with the full 100 k Scrollback, the memory part of §0 (~25–30% of a line)
  roughly halves at 12 B, or drops by a third at 16 B: **~10–15% steady state**, plus a similar
  share of the **fill** cost (page faults scale with bytes: 4.2 KB → 2.8 KB per 177-column row
  at 16 B) and 33–50% less Scrollback memory (425 MB → 283/212 MB for 100 k × 177). Confidence:
  medium. Effort: large (weeks), and it makes every future alacritty_terminal update a manual
  merge.

### 2b. The blank cell is all-zero bits

foot's empty cell is `wc = 0, attrs = 0`, so erasing is `memset(0)` and a fresh row is
`xcalloc` (`erase_cell_range`,
[terminal.c:2052-2074](https://codeberg.org/dnkl/foot/src/commit/8768826c887c27721fddbf4670e27215b201bce2/terminal.c#L2052-L2074);
`grid_row_alloc`,
[grid.c:438-457](https://codeberg.org/dnkl/foot/src/commit/8768826c887c27721fddbf4670e27215b201bce2/grid.c#L438-L457)).
alacritty's `Cell::default()` is `c: ' '`, `Color::Named(Foreground/Background)` — non-zero —
so `Row::new` must store every cell (`grid/row.rs:37-56`) and `reset` is a per-field loop.

- **Applies to Komando:** only together with 2a (it needs control of the bit layout:
  e.g. `c == '\0'` meaning blank, and named-colour discriminants arranged so Foreground and
  Background are 0). With it, `Storage::initialize` could allocate its 1000-row chunks with
  `alloc_zeroed`, which on Windows returns demand-zero pages from `VirtualAlloc` for large
  blocks: the OS zeroes each page on first touch and the terminal never writes a blank. The
  page faults themselves remain. Risk: every `c == ' '` check (e.g. `minimap.rs:409`, `549`,
  `is_empty`) must treat `'\0'` as blank too.
- **Gain:** a modest part of the fill cost (one fewer full write pass per row): 5–15% of fill
  time, ~0 in steady state. Confidence: medium-low. Only worth doing as part of 2a.

### 2c. Profile-guided optimisation — measured 1.5× on dense cells

The same benchmark table shows **foot built with GCC + PGO at 29.6 ms vs 45.5 ms without** on
"dense cells" (1.54×) and 10.4 vs 14.1 ms on cursor motion (1.35×); scrolling did not improve.
foot ships a PGO build script (`pgo/`) whose training input is generated escape-heavy output.

- **Applies to Komando:** directly, **no code change**: Rust supports PGO
  (`-Cprofile-generate` / `-Cprofile-use`, or `cargo pgo`), and Komando already has a ready
  training workload (`throughput_parser_only` and the render benchmarks). The parser state
  machine, `terminal_attribute`'s big `match`, and `input`/`input_ascii` are exactly the
  branchy code PGO helps. Risks: build complexity and CI time (an instrumented build plus a
  training run before every release), and the profile must be regenerated when the code
  changes; it does not change behaviour.
- **Gain:** 10–30% on the parser-only benchmark is typical for branchy Rust parsers; foot's
  1.35–1.54× is an upper reference. Confidence: medium (well established technique, but the
  vte/alacritty code is already fairly tight after Komando's patches).

### 2d. Not worth copying from foot

Separately `malloc`ed rows (`struct row` + `cells`, one allocation each) and a byte-at-a-time
parser: Komando already beats both.

Note on Komando's current build (`Cargo.toml:51-54`): `lto = "thin"`, `codegen-units = 4`,
no PGO, baseline `x86-64` target (SSE2). Trying `lto = "fat"` + `codegen-units = 1` on the
release profile is a one-line, zero-risk experiment that pairs naturally with PGO (cross-crate
inlining between vte, alacritty_terminal and Komando's `Output::feed`); expect 0–10%.

---

## 3. contour

The `TrivialLineBuffer` named in the brief (a line kept as one `string` + one rendition until a
second rendition appears) is now marked deprecated and survives only as a render-side view
([Line.hpp:62-82](https://github.com/contour-terminal/contour/blob/8aa5313325b66561b81120464aebb32a4dce6af2/src/vtbackend/grid/Line.hpp#L62-L82)).
Storage moved to a **structure-of-arrays line** (`LineSoA`,
[LineSoA.hpp:131-245](https://github.com/contour-terminal/contour/blob/8aa5313325b66561b81120464aebb32a4dce6af2/src/vtbackend/grid/LineSoA.hpp#L131-L245)):
separate 64-byte-aligned vectors for code points, widths, scales, packed SGR, hyperlink ids,
cluster info, plus optional side tables for images and soft glyphs. That is *not* compact —
about 30 bytes per cell across ~8 vectors (8 heap allocations per materialised line) — so it is
not a Scrollback-size model to copy. Three ideas in it are relevant:

### 3a. Lazily materialised blank lines

A `Line` constructed blank has **zero-length arrays** and is "logically `cols` wide, uniformly
`fillAttrs`" (`initializeBlankLineSoA`,
[LineSoA.cpp:57-68](https://github.com/contour-terminal/contour/blob/8aa5313325b66561b81120464aebb32a4dce6af2/src/vtbackend/grid/LineSoA.cpp#L57-L68));
the arrays are filled only on the first write. `recycleBlankLineSoA` (`:70-78`) clears a line
but keeps its capacity, so a recycled line re-materialises without allocating.

- **Applies to Komando:** in principle this removes Scrollback *fill* cost for rows nobody
  writes, but in a log flood every row is written, so it saves nothing there. In alacritty every
  `grid[line][col]` index assumes a full-width `Vec`; making rows lazily sized means auditing
  every indexing site (vendor and Komando). **Not recommended** for throughput.

### 3b. A per-line "shape" that gates the fast paths

`LineShape::{Uniform, Mixed, Spanning}` (`LineSoA.hpp:50-75`): a line only ever degrades;
`Spanning` (wide chars, images) disables bulk ASCII writes; `Uniform` lets the renderer emit the
whole line as one run. `resetLine` skips refilling SGR when the line was uniform in the fill pen
(`LineSoA.cpp:155-173`).

- **Applies to Komando:** as a *row flag* ("contains a wide char / spacer / extra"), it would
  let `input_ascii` skip its per-cell `WIDE_CHAR|WIDE_CHAR_SPACER` test and let `Row::reset` /
  the compact-history encoder (§8) skip per-cell `extra` handling. Small benefit (2–4%) on its
  own; useful as an enabler for 1b and §8. Maintaining the flag correctly in every alacritty
  write path is the risk (a missed site silently corrupts wide characters).

### 3c. Bulk text bounded by the room left on the line

`parseBulkText`
([Parser-impl.hpp:762-860](https://github.com/contour-terminal/contour/blob/8aa5313325b66561b81120464aebb32a4dce6af2/src/vtparser/Parser-impl.hpp#L762-L860))
asks the screen `maxBulkTextSequenceWidth()` (columns left before the margin, 0 if the line is
`Spanning`) and has libunicode's SIMD `scan_text` stop there, so a run never needs splitting
for wrap; it then executes trailing C0 controls (CR, LF, HT) inline without returning to the
state machine (the comment claims "~50x for cat-like workloads" — relative to its own
per-byte FSM, not to vte). Komando's `input_ascii` already loops per row and vte already handles
controls between runs in `ground_dispatch`, so this is **already covered**.

contour's repository has no published throughput comparison against alacritty
(`docs/internals/profiling.md` only explains how to profile).

---

## 4. WezTerm (wezterm-term, wezterm-surface, termwiz)

### 4a. History compressed to "text + attribute clusters" when a line leaves the screen

`Screen::scroll_up` calls `line.compress_for_scrollback()` on each row as it scrolls off the top
([term/src/screen.rs:690-694](https://github.com/wezterm/wezterm/blob/e1eae81a56be8b5487dc77a29616afa568fb57b7/term/src/screen.rs#L690-L694)).
That converts the row's `Vec<Cell>` (`CellStorage::V`) into a `ClusteredLine`
(`CellStorage::C`,
[wezterm-surface/src/line/line.rs:1065-1075](https://github.com/wezterm/wezterm/blob/e1eae81a56be8b5487dc77a29616afa568fb57b7/wezterm-surface/src/line/line.rs#L1065-L1075)):
one UTF-8 `String` for the whole line, a `Vec<Cluster { cell_width: u16, attrs:
CellAttributes }>` run-length list (a new run whenever attributes change), and an optional
bitset of double-width cells
([clusterline.rs:17-41, 103-158](https://github.com/wezterm/wezterm/blob/e1eae81a56be8b5487dc77a29616afa568fb57b7/wezterm-surface/src/line/clusterline.rs#L17-L158)).
Any mutation re-materialises the cells (`make_cells` / `coerce_vec_storage`, `line.rs:1047-1063`).
Rendering, search and selection read clustered lines through a `CellRef` iterator that
works on both representations (`visible_cells`), so readers never need the `Vec<Cell>`.

- **Size:** for Komando's log line: ~73 B of text + 6 clusters × (2 + 16 B `CellAttributes`,
  rounded to 24) ≈ 220 B + 3 heap headers, versus 4,248 B for an alacritty row at 177 columns.
  WezTerm's `Cell` is itself 24 bytes (`wezterm-cell/src/lib.rs:1032-1036`).
- **Why it is not a throughput win in WezTerm:** it allocates a new `String`/`Vec` per
  compressed line and frees the old ones, and recycled lines are rebuilt as `Line::new`
  (`screen.rs:707-716`); WezTerm is not known for bulk throughput, and its repository publishes
  no vtebench numbers. The lesson is the *representation*, not the implementation: done with
  per-line heap allocations it trades page faults for allocator traffic.
- **Applies to Komando:** the encoding is cheap (one pass over `occ` cells, one compare per
  cell) and could run in `Grid::scroll_up` exactly where WezTerm does it. But it changes the
  type of history rows, which alacritty's `Grid` hands out as `&Row<Cell>` from `Index<Line>`
  for every line (selection, `RegexSearch`, `display_iter`, `bounds_to_string`, reflow in
  `grid/resize.rs`) and which Komando reads directly in `marks.rs`, `search.rs`, `minimap.rs`,
  `session/mod.rs` and `terminal_view.rs`. See §8 for a design that keeps that API.

### 4b. Rows are only as wide as what was written

WezTerm's `VecStorage` lines hold only the cells written so far (`Line::new` starts empty;
`visible_cells` stops at the line's length). Same idea as contour's lazy blank lines; same
verdict for a log flood (every row is written anyway), but it does mean history costs
memory proportional to the *text*, not the window width.

---

## 5. Rio (rio-vt "crosswords": an alacritty_terminal fork with an 8-byte cell)

Rio's terminal core is a fork of alacritty_terminal (same `Grid`/`Storage`/`Row`/`occ`
structure, same `Crosswords` ≈ `Term`), which makes it **the most directly transferable
precedent in this report**: it shows the "smaller cell" refactor done on Komando's own grid code.

### 5a. `Square(u64)`: an 8-byte cell with interned styles and a side table for extras

The header of
[rio-vt/src/crosswords/square.rs:1-60](https://github.com/raphamorim/rio/blob/93983b78bbc9da340a92b657a063830a3ac6bf8e/rio-vt/src/crosswords/square.rs#L1-L60)
says it plainly: "The previous design held `c`, `fg`, `bg`, `flags`, and an
`Option<Arc<CellExtra>>` inline (24 bytes per cell + per-cell heap allocations for "extras").
All variable-sized data now lives in per-grid side tables (`StyleSet`, `ExtrasTable`); the cell
itself is 8 bytes." Bit layout: 21-bit code point, 2-bit width class, 7 cell flags (incl.
WRAPLINE), 2-bit content tag, 16-bit `style_id`, 16-bit `extras_id`. Two extra tags encode
"background-only" cells (palette or RGB bg inline) so cleared coloured areas need no style
entry. `Square(0)` is the blank default cell (asserted `size_of::<Square>() == 8`, `:635`).

`StyleSet`
([style.rs:150-387](https://github.com/raphamorim/rio/blob/93983b78bbc9da340a92b657a063830a3ac6bf8e/rio-vt/src/crosswords/style.rs#L150-L387))
interns `(fg, bg, underline_color, flags)` packed losslessly into a `u128`; a 1024-slot
direct-mapped memo in front of an `FxHashMap` makes repeat interns a multiply, a load and one
128-bit compare. Ids are reclaimed by **mark-and-sweep** over the ring (`reclaim_styles`,
`crosswords/grid/mod.rs:805`), skipping rows whose conservative `has_styles` flag is false
(`grid/row.rs:40-50`), and only once the table passes 32,768 entries with 4,096 novel interns
since the last sweep. SGR handling stays cheap because interning is **lazy**: SGR only edits the
cursor's pending style and sets `style_dirty`; the first print after it calls
`sync_template_style` (`grid/mod.rs:779-785`).

The ASCII fast path (`input_ascii_str`,
[crosswords/mod.rs:3980-4080](https://github.com/raphamorim/rio/blob/93983b78bbc9da340a92b657a063830a3ac6bf8e/rio-vt/src/crosswords/mod.rs#L3980-L4080))
is Komando's `input_ascii` with two improvements: a branch-free `fold` over the target cells
for "needs wide cleanup" (so the check vectorises) followed by a store of
`template | codepoint` per cell — one 8-byte store.

`Row::reset` (`grid/row.rs:201-236`) also improves on alacritty's: it probes the tail beyond
`occ` once and, if the whole row must be refilled, uses `fill(proto)` (a memset-like loop).
`Storage` keeps a recycle pool of freed rows (`grid/storage.rs:54-62, 143-200`) so shrink/grow
and resize-reflow allocate nothing after the first cycle. Prompt marks (OSC 133) are a per-row
field (`Row::semantic_prompt`, `row.rs:52-54`) that travels into the Scrollback — the same
choice foot makes.

- **Why fast:** 3× less memory per cell than alacritty: a 177-column row is 1,416 B instead of
  4,248 B, so the 100 k Scrollback is 142 MB instead of 425 MB, every recycled row is a third of
  the cache lines, and a cell write is one store with no refcount branch. No published
  benchmark in the repository compares the old and new Rio cell; treat the gain as unmeasured.
- **Applies to Komando:** this is **the "smaller cells" option, already designed and debugged
  on alacritty's grid**. It is still a **large vendor refactor** (rio-vt's `Square` touched
  `square.rs` 768 lines, `style.rs` 579, extras table, sweep, every renderer read): every
  `cell.fg`/`cell.bg`/`cell.flags`/`cell.extra` access in alacritty_terminal and Komando becomes
  an accessor that may need the grid's `StyleSet` (`term.grid().style(cell)`), including
  `src/ui/minimap.rs:549-575`, `terminal_view.rs`, `search.rs`, `highlight.rs` callers.
  Risks:
  - **Command Marks** are hyperlinks in `extra` on column 0 (`src/marks.rs:232`). Under this
    model they become `extras_id` entries swept by mark-and-sweep — or better, move to a
    per-row field as Rio and foot do for prompt marks, which also makes `find_tag`
    (`marks.rs:237`) a per-row check instead of a hyperlink lookup.
  - **Highlighting** (`src/highlight.rs`) colours cells at render time; it reads styles, so it
    needs the `StyleSet` lookup but no semantic change.
  - **Selection/search/reflow** only copy cells; with ids they must stay within one grid
    (alternate screen and main screen each own a `StyleSet` in Rio — copying a cell between
    grids must re-intern).
  - **Updates from upstream alacritty_terminal** become manual ports.
  - Mark-and-sweep with a 100 k × 177 ring is a 17.7 M-cell walk when it runs; Rio bounds how
    often. A log flood uses a handful of styles, so it never triggers there.
- **Estimated gain:** with a full 100 k Scrollback, steady state **15–25%** (the cold-memory
  part of §0 shrinks to a third; cell writes get cheaper), **fill phase 40–60% cheaper** (a third
  of the page faults), and ~280 MB less memory at 100 k × 177. With the prefetch (1a) in place
  the steady-state share shrinks (they attack the same misses), so do 1a first and measure.
  Confidence: medium.

### 5b. Rio's parser (copa → rio-vt `performer/parser`)

`advance_ground`
([performer/parser/mod.rs:903-1160](https://github.com/raphamorim/rio/blob/93983b78bbc9da340a92b657a063830a3ac6bf8e/rio-vt/src/performer/parser/mod.rs#L903-L1160))
is vte's: `memchr` for ESC, then printable-ASCII runs to `print_str`, with non-ASCII decoded
by the C++ `simdutf` library only for long runs ("for short runs … the out-of-line simdutf call
costs more than decoding in place", `:1077-1122`). Komando already has the ASCII part; simdutf
only helps long non-ASCII text, which is not the log workload. Not worth an FFI dependency.

---

## 6. xterm.js, libvterm, vt100-rust, Windows Terminal's row model

### 6a. xterm.js: 12 bytes per cell in one `Uint32Array`, sparse side maps

`BufferLine`
([src/common/buffer/BufferLine.ts:20-95](https://github.com/xtermjs/xterm.js/blob/f70764dd3da05ebda0390605c6f9adbbea3f0e90/src/common/buffer/BufferLine.ts#L20-L95))
stores three `u32` per cell — content (code point + width + "is combined" bit), fg (colour +
all SGR bits), bg — in one typed array per line. Combined characters and extended attributes
(underline style/colour, URL id) are sparse `{[column]: …}` objects on the line
(`_combined`, `_extendedAttrs`, `:72-74`). When the ring is full, `BufferService.scroll`
**recycles** the trimmed line and `copyFrom`s a cached blank line into it (one typed-array
copy) (`src/common/services/BufferService.ts:69-98`). Same lessons as foot: 12-byte cell, rare
data out of the cell, recycle rather than allocate. Nothing new beyond §2/§5.

### 6b. libvterm: the terminal keeps no Scrollback; the embedder decides the format

libvterm's screen layer copies a row that scrolls off into a reusable cell buffer and hands it
to the embedder's `sb_pushline` callback
([src/screen.c:209-232](https://github.com/neovim/libvterm/blob/934bc2fbf21800ac3458a499df8820ca5fb45fd3/src/screen.c#L209-L232)).
Neovim's `:terminal` stores those lines as ordinary buffer **text** with highlights kept apart —
the "history as text + attributes" model in its purest form. A clean separation, but in
Komando it would mean taking history out of alacritty's `Grid` entirely (see §8).

### 6c. vt100-rust: not a speed reference

`Cell` holds the grapheme inline (`contents: [u8; CONTENT_BYTES]`, `len`, `attrs`,
[src/cell.rs:12-16](https://github.com/doy/vt100-rust/blob/fb0f54ec7222f4805f33cd3ca530229e0a0fe6ad/src/cell.rs#L12-L16)),
and Scrollback is a `VecDeque<Row>` with a heap allocation per row (`src/grid.rs:14-19`).
Larger cells than alacritty and per-line allocation: nothing to adopt.

### 6d. Windows Terminal (storage model only; its benchmarks were covered earlier)

From the `Row.hpp`/`textBuffer.cpp` copies downloaded by the earlier attempt (path
`src/wt/`, upstream microsoft/terminal `src/buffer/out/`; commit not recorded, so line numbers
refer to those copies): a `ROW` is **UTF-16 text (2 B/column) + a `u16` char-offset per column
+ run-length-encoded attributes** (`til::small_rle<TextAttribute, uint16_t, 2>`, `Row.hpp:20`).
All rows live at a fixed stride in **one `VirtualAlloc(MEM_RESERVE)` region**, committed in
chunks as the buffer grows (`TextBuffer::_reserve` / `_commit`, `textBuffer.cpp:104-180`) — no
per-row heap allocation ever; the comment on `_commit` notes making the cold path `noinline`
was worth ~6%. Per column this is ~4 bytes plus a few runs per row, versus alacritty's 24.

- **Applies to Komando:** the **single reserved-and-committed region** is applicable to
  alacritty's `Storage` without changing the cell type: allocate each 1000-row chunk's cells as
  one block (or reserve the whole `max_scroll_limit × columns × 24` once and commit on demand)
  instead of 1000 separate `Vec`s. It does not remove the page faults of first touch, which are
  the dominant fill cost, but removes the per-row allocator calls and lets the zeroing/commit be
  done by the OS in big pieces; the in-progress `RowPool` (§0) already moves this cost off the
  parser thread, which is the bigger lever. Low priority for throughput; mentioned for
  completeness.

---

## 7. The topics asked about, answered across projects

**Remaining per-line cost in alacritty's `Term` once the Scrollback is full.** See §0. About
0.41 µs/line on the Windows machine (≈4 ns/byte). A full 100 k Scrollback makes each line
~30–40% dearer than no Scrollback (Linux VM data). The only Scrollback-dependent work in
steady state is `Row::reset` on the recycled row plus the cell writes that follow it, both on
memory last touched 100,000 lines ago. Komando does **no per-line heap allocation** once the
Scrollback is full: the ring recycles rows, and `extra` stays `None` for SGR-only output. The
per-line allocations are all in the fill phase (`Row::new` × 1000 per `initialize`), which
the `RowPool` work addresses.

**Compact or lazily materialised Scrollback rows.** Working precedents: WezTerm (text +
attribute clusters on scroll-off, §4a), kitty (cells, then ANSI text in a pager ring, §1e),
Windows Terminal (UTF-16 + RLE attributes for all rows, §6d) and libvterm (the embedder owns
text history, §6b). None of them published a throughput gain from it; the win is memory
(≈20–30× smaller history) and, as a consequence, cache behaviour and fill cost. Lazy
materialisation of *blank* rows (contour, WezTerm) does not help a log flood. Design for
Komando in §8.

**Smaller cells.** foot 12 B, xterm.js 12 B, Rio 8 B (an alacritty fork), WezTerm 24 B,
alacritty 24 B, kitty 32 B. foot's vtebench "dense cells" beats Alacritty 2×, though not all of
that comes from cell size. Rio's `Square(u64)` (§5a) is the blueprint for Komando.

**SIMD scanning beyond memchr.** Komando's vte makes three passes over text bytes (memchr,
`from_utf8`, printable scan). One fused SSE2/AVX2 "first byte not in 0x20..0x7e" scan replaces
them for ASCII (§1c). kitty and Rio show that decoding non-ASCII with SIMD only pays on long
runs; the log workload has ~12-byte runs, so expect single-digit gains. CSI parameters are 1–2
digits: nothing for SIMD there. **Software prefetch** (kitty, §1a) is the memory-side trick
that matters more than SIMD for this workload.

**Avoiding per-line allocation.** Already true in steady state. For the fill: Rio's recycle
pool for rows freed by shrink/resize (§5a), Windows Terminal's single reserved region (§6d),
kitty's `mmap` of 2048-line segments (§1) and Komando's own `RowPool`. With an 8-byte cell the
fill shrinks by two thirds regardless.

---

## 8. A compact history tier for Komando: what it would take

Goal: store Scrollback rows older than the screen plus a small "hot" margin as packed text +
attribute runs, and keep alacritty's `Row<Cell>` for the screen and the hot margin.

**Encoding** (in `Grid::scroll_up`, when a row passes the hot margin, e.g. 1,000 rows above
the screen): one pass over `occ` cells writes a record into a byte arena that is itself a ring
of large blocks (no per-line allocation, unlike WezTerm):
`[header: occ u16, WRAPLINE/mark bits, mark id][chars: UTF-8, or u32 when non-ASCII]
[runs: (start_col u16, fg u32 packed, bg u32 packed, flags u16, extra_idx u16)]`. The `extra`
`Arc`s (zero-width chars, hyperlinks, underline colour) go in a side `Vec` per arena block and
are released when the block is overwritten. For Komando's log line: ~73 B text + 6 × 14 B runs
+ 8 B header ≈ 165 B instead of 4,248 B. 100 k lines ≈ 16 MB instead of 425 MB, small enough
to stay largely cache-resident on a 96 MB-L3 part. The row freed from the hot ring is reused
for the new bottom line while still warm (touched ~1,000 lines ago, ~4 MB back), so the
cold-`reset` problem of §0 disappears without prefetching.

**Reading** is the hard part. Every `grid[Line(l)]` with `l` in the archive must change:

| Reader | Where | What it would need |
|---|---|---|
| Rendering a scrolled-back viewport | `terminal_view.rs` `layout_grid`, `display_iter` | decode ≤ screen-lines rows per frame into a scratch ring (cheap) |
| Selection → text | alacritty `selection.rs`, `bounds_to_string` | decode the selected range, or read text straight from records |
| Search | `src/search.rs`, alacritty `term/search.rs` (`RegexSearch` walks cells across all history) | a record iterator that yields cells, or search record text directly (faster than today) |
| Command Marks | `src/marks.rs:232-245` (hyperlink on column 0) | store the mark id in the record header; `find_tag` becomes a header scan |
| Minimap | `src/ui/minimap.rs:504-580` (a window of `capacity` lines) | decode that window; it already caches lines |
| Highlighting | `src/highlight.rs` | works on decoded rows; unchanged |
| Resize / reflow | alacritty `grid/resize.rs` | decode → reflow → re-encode the whole archive, or reflow records at text level |
| Copy Scrollback, `drop_blank_scrollback` | `src/session/mod.rs` | record iteration |

The least invasive way to keep alacritty's API is a `Grid::row(line) -> RowRef<'_>` (an enum
of `&Row<Cell>` or a packed view with `cell(col) -> Cell` and a cell iterator), converting the
readers above to it. `Index<Line>` stays for the screen and hot margin only, and panics in
debug builds for archived lines so missed callers show up. That is **a terminal-core refactor
of alacritty_terminal**, comparable in size to Rio's cell rewrite. It touches selection, search,
resize and every Komando reader, with a long tail of correctness work: wide characters,
zero-width characters, wrapped lines across the hot/archive boundary, the alternate screen,
`clear_history`, `CSI 3 J`.

**Expected gain:** steady state 10–25% (encoding at ~40–80 ns/line replaces the ~80–130 ns cold
reset), fill phase mostly gone (a few MB of arena instead of 425 MB of rows, so the `RowPool`
becomes unnecessary), and ≈25× less memory at 100 k lines. Confidence: medium on memory,
low-medium on throughput. Compared with §5a (8-byte cells), it saves far more memory but costs
more code and touches more of Komando; §5a keeps one representation, so every reader stays
simple. If Komando wants a big structural change, do **5a first**: it is self-contained and has
a working precedent on the same grid. Revisit §8 only if Scrollback memory, not speed, becomes
the complaint.

Rejected alternative: a *lazy reset* (marking a recycled row "to be reset" and resetting only
the cells the new line does not overwrite). Every reader of the row would need to know about
it, and the prefetch (1a) hides most of the same cost for a fraction of the risk.

---

## 9. Ranked recommendations

Gains are for `throughput_parser_only` with a 100 k Scrollback on the Windows machine,
estimated from reading the code; none was measured. They overlap (1, 5 and 6 all attack the
same memory traffic), so do not add them up. A realistic combined target for items 1–4 is
**1.3–1.6×**.

| # | Technique | Source | Effort | Expected gain | Confidence |
|---|---|---|---|---|---|
| 1 | **Prefetch the next row to be recycled** (`_mm_prefetch` with write intent over its `occ` cells, one or two rows ahead) in `Grid::scroll_up` | kitty §1a | Small vendor patch (~20 lines, no data change) | 10–25% steady state with a full Scrollback; ~0 without | Medium-high |
| 2 | **PGO** (`-Cprofile-generate/-use` trained on `throughput_parser_only` + render benches), and try `lto = "fat"`, `codegen-units = 1` | foot §2c | Build/CI only, no code | 10–25% (foot measured 1.35–1.54× on dense cells) | Medium |
| 3 | **Template fill in `input_ascii`**: write a ready-made `Cell` per position (or Rio's branch-free cleanup check + one store), skipping the per-cell `Option<Arc>` clone/drop when neither template nor run has `extra` | kitty §1b, Rio §5a | Small vendor patch, covered by `komando_tests.rs` | 3–8% | Medium |
| 4 | **Fused SIMD ground scan** in vte: one SSE2 pass for "first non-printable-ASCII byte" replacing memchr + `from_utf8` + scalar run scan; slow path unchanged | kitty §1c, contour §3c | Small-medium vendor patch (unsafe + scalar fallback; existing differential tests) | 3–8% (short runs here; more on plain text) | Medium-low |
| 5 | **8-byte cell**: `u64` with code point, flags, interned `style_id`, `extras_id`; per-grid `StyleSet` with memo + mark-and-sweep; Command Marks and prompt marks as per-row fields | Rio §5a (alacritty fork), foot §2a, xterm.js §6a | Large vendor refactor + all Komando cell readers; manual merges of future alacritty releases | 15–25% steady state, 40–60% cheaper fill, −280 MB at 100 k × 177 | Medium |
| 6 | **Compact history tier**: text + attribute runs in a block-ring arena past a hot margin | WezTerm §4a, kitty §1e, Windows Terminal §6d | Terminal-core refactor (Grid API, selection, search, resize, marks, minimap) | 10–25% steady state, fill mostly gone, ≈25× less Scrollback memory | Low-medium (throughput), medium (memory) |
| 7 | Per-row "has wide/extra" flag gating the fast paths | contour §3b, Rio `has_extras` | Small-medium, risky to keep exact | 2–4% alone; enabler for 3 and 5 | Medium-low |
| 8 | One reserved region for Scrollback rows instead of 1000 `Vec`s per chunk | Windows Terminal §6d, kitty segments | Medium vendor patch | Small; mostly superseded by `RowPool` | Low |

Not recommended for throughput: lazily materialised blank rows (contour, WezTerm: every log row
is written), simdutf FFI (Rio: long non-ASCII runs only), byte-at-a-time parsers (foot, WezTerm
vtparse: Komando's vte is already ahead), and per-line heap rows (vt100, WezTerm).

**Suggested order:** 1 → 2 → 3 → 4. Each is small and measurable on its own with the existing
benchmark and differential tests. Then decide on 5 with numbers in hand. Measure 1 with both a
100 k and a 10 k Scrollback: if it works, the gap between them should mostly close.
