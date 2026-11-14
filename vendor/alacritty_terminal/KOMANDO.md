# alacritty_terminal 0.26.0, patched for Komando

This is alacritty_terminal 0.26.0 from crates.io with one change, used through
`[patch.crates-io]` in Komando's `Cargo.toml`, against the patched vte in `vendor/vte` (see
its `KOMANDO.md`). `komando.patch` holds the change as a diff against the published crate.

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
module declaration) and adds `src/term/komando_tests.rs`. New items are marked `Komando patch`.
Everything else here is the published crate as it is, with `Cargo.lock`, the registry's
bookkeeping files and the reference test recordings (`tests/ref/`, 46 MB) left out.

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

## Why

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

## Updating alacritty_terminal

Copy the new release's crate source over this folder (leaving out `Cargo.lock`, `.cargo-ok`,
`.cargo_vcs_info.json` and `tests/ref/`), apply `komando.patch` with `patch -p1` (or redo the
change by hand where it no longer applies), regenerate the patch with
`diff -ruN a/src b/src > vendor/alacritty_terminal/komando.patch` (`a` the crate source, `b`
this folder), and update `vendor/vte` to the vte version it depends on. Then check
`Term::input` and `write_at_cursor` for changes `input_ascii` must follow (anything new that
`input` does for a one-cell character outside insert mode and the DEC charset), run the tests
above, and compare `throughput_parser_only` before and after. If alacritty_terminal gains a
batched input path of its own, use that and drop the patch.
