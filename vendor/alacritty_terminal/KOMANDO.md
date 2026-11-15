# alacritty_terminal 0.26.0, patched for Komando

This is alacritty_terminal 0.26.0 from crates.io with three changes, used through
`[patch.crates-io]` in Komando's `Cargo.toml`, against the patched vte in `vendor/vte` (see
its `KOMANDO.md`). `komando.patch` holds the changes as a diff against the published crate.

## The change: printable ASCII written in runs

The patched vte hands `Handler::input_ascii` runs of printable ASCII (`0x20..=0x7e`).
`Term::input_ascii` writes them the way calling `Term::input` for each character would, but
row by row: for each row it reads the cursor's template once and fills the cells from the
cursor to the end of the run or the row in one loop, then moves the cursor, wrapping
(`wrapline`) where `input` would. Printable ASCII is always one cell wide, so there is no width
lookup. It leaves to `input`:

- the whole run while insert mode is on, or while the active charset is not ASCII (DEC line
  drawing, `ESC ( 0`);
- any cell that holds part of a wide character (`WIDE_CHAR` or `WIDE_CHAR_SPACER`): the run
  stops there, `input` writes that one character (clearing the wide character's other half and
  any leading spacer on the line above), and the run carries on after it.

With line wrapping off (DECAWM, `CSI ? 7 l`) and the cursor already waiting to wrap, every
character left lands on the last column, so only the last one is written. Damage is recorded as
`input` records it: `input` itself only damages through `wrapline` (and the `linefeed` that can
call), the rest coming from the cursor's movement when the damage is read, and `input_ascii`
calls `wrapline` at the same points.

The change touches `src/term/mod.rs` (`Term::input_ascii` in the `Handler` impl, and the test
module declaration) and adds `src/term/komando_tests.rs`. The second change (below) touches
`src/grid/mod.rs`, `src/grid/storage.rs` and `src/grid/resize.rs`. New items are marked
`Komando patch`. The third change covers Windows PTY resource ownership (below).
Everything else here is the published crate as it is, with `Cargo.lock`, the registry's
bookkeeping files and the reference test recordings (`tests/ref/`, 46 MB) left out.

## The change: the next recycled row is loaded ahead

Once the Scrollback is full, every new line reuses its oldest row: `Grid::scroll_up` rotates
the ring and `Row::reset` clears the row. That row was last written a whole Scrollback ago
(about 425 MB earlier with 100,000 lines of 177 columns), so it is in no CPU cache, and the
reset and the text written after it waited for main memory. After each rotation,
`Storage::prefetch_next_recycled` asks the CPU to start loading the row the next rotation
reuses (`Row::prefetch`: its occupied cells and the last one, which `reset` reads), so the
memory arrives while the current line is parsed. kitty does the same for its history buffer.

It only issues prefetch instructions, which never fault and change no state, and only with
at least 4,096 rows in the ring: smaller ones stay in the caches anyway. `Row::prefetch` is
kept out of line: inlined into `scroll_up` it made output without Scrollback about 6% slower,
although it never ran there. On other architectures than x86-64 it does nothing.

The change touches `src/grid/mod.rs` (one call in `scroll_up`), `src/grid/storage.rs` and
`src/grid/row.rs` (the two methods).

Measured with `throughput_parser_only` on Windows 11 (Ryzen 7 9800X3D), medians of 3 runs
alternating with the build without it:

| Scrollback | Before | After |
|---|---|---|
| 100,000 lines | 0.481 s | 0.341 s (1.41x) |
| 10,000 lines | 0.283 s | 0.269 s |
| 0 | 0.249 s | 0.250 s |

Over SSH (`throughput_coloured_log_lines_over_ssh`, 100,000 lines of Scrollback): 0.548 s
before, 0.443 s after.

## Tests

`src/term/komando_tests.rs` checks that `input_ascii` leaves a `Term` exactly as `input` does:
it feeds the same bytes, in the same pieces, to one `Term` directly and to another through a
handler that forwards everything but `input_ascii` (so runs arrive one character at a time),
and compares, after every piece, the cells of every line (scrollback, alternate screen, row
occupancy), both cursors and saved cursors, modes, charset, scroll region and damage. It does
this for a list of tricky cases (runs wrapping at the last column, a wrap pending when a run
starts, DECAWM off, insert mode, DEC line drawing, REP, overwriting wide characters and their
spacers, leading spacers, pieces split inside UTF-8 and escapes, scroll regions, origin mode,
cells with extra storage, the alternate screen) and for 4000 random streams of such output at
sizes from 2 to 80 columns. One-column terminals are left out: `input` panics on a wide
character there, patched or not.

To run alacritty_terminal's own tests, including the reference tests that replay recorded
sessions and compare the grid, with the patched vte (`.gitignore` keeps the recordings out):

```sh
cp -r ~/.cargo/registry/src/index.crates.io-*/alacritty_terminal-0.26.0/tests/ref vendor/alacritty_terminal/tests/
cargo test --manifest-path vendor/alacritty_terminal/Cargo.toml --config "patch.crates-io.vte.path='$PWD/vendor/vte'"
rm vendor/alacritty_terminal/Cargo.lock
```

## Why runs of ASCII

Parsing is most of what a fast stream of output costs. `throughput_parser_only` (one million
coloured log lines, 130 MB, fed straight into a 177 x 42 terminal) on a 4-core Linux VM, median
of 6 interleaved runs each, with another build running alongside:

| Scrollback | Before | After |
|---|---|---|
| 100,000 lines | 56.9 MB/s | 77.4 MB/s (1.36x) |
| 10,000 lines | 72.2 MB/s | 107.2 MB/s (1.49x) |
| 0 | 81.7 MB/s | 132.5 MB/s (1.62x) |

Other output, through alacritty_terminal and vte alone (177 x 42, 10,000 lines of scrollback,
best of 5 passes, median of 3 interleaved runs), before and after:

| Output | Before | After |
|---|---|---|
| Plain ASCII lines, 100 characters | 68.7 MB/s | 135.2 MB/s |
| `ls --color`-like lines | 68.5 MB/s | 81.8 MB/s |
| Full-screen redraws (cursor moves, colours) | 97.7 MB/s | 202.6 MB/s |
| Lines mostly of other UTF-8 (box drawing, Cyrillic, accents) | 83.2 MB/s | 92.9 MB/s |
| CJK lines | 84.3 MB/s | 86.3 MB/s |

Text that is not ASCII is no slower.

## The second change: new history rows from a row source

Every history row is a full-width `Row<Cell>` (24-byte cells: about 4.2 KB at 177 columns).
While the history grows, `Storage::initialize` allocates and default-fills new rows 1,000 at a
time, on the thread that parses output and with the terminal locked; once the history is full,
rows are only rotated and reused. The allocations, and the fresh memory pages they touch, made
a filling history much slower than a full one.

A grid can now be given a `RowSource` (`Grid::set_row_source`), which hands over fresh rows
built elsewhere; Komando's is `src/session/row_pool.rs`, which builds them on a worker thread.
`Storage::initialize` asks the source for the rows it is about to add, only in the branch where
it grows its buffer (in an out-of-line, cold function, so the per-line path is unchanged), and
builds whatever it doesn't get with `Row::new` as before. It throws everything it was handed
away, and builds every row itself, if the source handed over more rows than asked for or any
row of another width. The source is told how many more rows the grid may still ask for
(`rows_left`; zero once the history is full), and is told again, asking for nothing, after
`Grid::resize` and `Grid::update_history` (a change of the history's size). Without a source,
nothing changes.

The grid holds the source as `Arc<dyn RowSource<T>>`, which must be `Send + Sync`; it is left
out of serialization, and `Storage`'s own tests set it to `None`.

Komando's tests (`src/session/row_pool.rs`) check that a terminal taking its rows from the pool
has the same grid as one building them itself, through a history that fills, is resized while
the pool holds rows, overflows, is cleared (`ESC [3J`), shrinks and grows with the Scrollback
setting, and is reset (`ESC c`); that the grid refuses rows of another width or too many; and
that the pool empties and its thread stops once the history is full, the terminal is dropped,
or output stops.

Measured on Windows 11 (bare metal), `throughput_parser_only` and the time for the first
100,000 lines (while the history fills) and the next 900,000 lines (once it's full), 177 x 42,
median of 8 interleaved runs each:

| Scrollback | Before | After |
|---|---|---|
| 100,000 lines, whole run | 0.478 s | 0.434 s |
| 100,000 lines, first 100,000 lines | 0.070 s | 0.037 s |
| 100,000 lines, next 900,000 lines | 0.399 s | 0.395 s |
| 10,000 lines, whole run | 0.290 s | 0.282 s |
| 0 (no source is set) | 0.255 s | 0.256 s |

## The change: Windows PTY resource ownership

`src/tty/windows/conpty.rs` now owns the pseudo-console immediately after creation,
keeps the pipe ends in RAII wrappers, deletes initialized process attribute lists, and
closes both handles returned by `CreateProcessW`. A failed pseudo-console creation returns
an error instead of panicking. Failed shell launches previously leaked five native handles
per attempt, and successful launches also retained pipe, process and thread handles.

`src/tty/windows/child.rs` duplicates the borrowed process handle for the exit watcher's
lifetime. The watcher owns the callback context and cancels its wait synchronously before
freeing that context and closing its handle, including when the shell is closed before it
exits. This prevents callback races and releases contexts whose callbacks never run.

Komando's Windows tests in `src/session/local.rs` check handle counts in an isolated process
after repeated failed and successful launches, and run cmd and PowerShell through the
actual session event loop to check final output and exit notification.

## Updating alacritty_terminal

Copy the new release's crate source over this folder (leaving out `Cargo.lock`, `.cargo-ok`,
`.cargo_vcs_info.json` and `tests/ref/`), apply `komando.patch` with `patch -p1` (or redo the
change by hand where it no longer applies), regenerate the patch with
`diff -ruN a/src b/src > vendor/alacritty_terminal/komando.patch` (`a` the crate source, `b`
this folder), and update `vendor/vte` to the vte version it depends on. Then check
`Term::input` and `write_at_cursor` for changes `input_ascii` must follow (anything new that
`input` does for a one-cell character outside insert mode and the DEC charset), check that
`Storage::initialize` is still the only place new history rows are built, run the tests
above, and compare `throughput_parser_only` before and after. If alacritty_terminal gains a
batched input path of its own, use that and drop the patch.
