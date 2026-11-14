# vte 0.15.0, patched for Komando

This is vte 0.15.0 from crates.io with one change, used through `[patch.crates-io]` in
Komando's `Cargo.toml` (alacritty_terminal's dependency on vte resolves to it too).
`komando.patch` holds the change as a diff against the published crate. It goes with the change
to `vendor/alacritty_terminal` (see its `KOMANDO.md`): neither does much without the other.

## The change: printable ASCII in runs

vte already finds the next ESC with `memchr` and checks the text before it as UTF-8 in one go,
but then hands it to the terminal one character at a time: `Perform::print(c)` per character,
which the `ansi` module turns into `Handler::input(c)`, and alacritty_terminal's `Term::input`
(not inlined) looks up the character's width, checks wrapping, insert mode and the charset,
and copies the cursor's template into the cell, for every character.

The patch adds, the way Ghostty's stream hands its handler printable spans:

- `Perform::print_ascii(&str)`: a run of printable ASCII (`0x20..=0x7e`). The parser's ground
  state (`ground_dispatch` in `lib.rs`) now splits text into maximal runs of printable ASCII,
  passed to `print_ascii` in one call, and everything else, dispatched one character at a time
  exactly as before (controls to `execute`, other characters to `print`). The default
  `print_ascii` calls `print` for each character, so any other `Perform` behaves as before.
- `ansi::Handler::input_ascii(&str)`: the same for the `ansi` handler. The `ansi` performer
  passes the run on and sets the character REP (`CSI b`) repeats to the run's last character,
  as `print` would have. The default calls `input` for each character.

Only bytes that are already in the ground state are split differently. Escape sequences,
UTF-8 split across `advance` calls and invalid UTF-8 go through the same code as before, and
`advance_until_terminated` still stops at the same places (it checks after each escape or
ground chunk, never inside one). A run can be cut short by the end of an `advance` call; that
only means two calls instead of one.

The change touches `src/lib.rs` (`ground_dispatch`, a helper `is_printable_ascii`, the new
`Perform` method, tests) and `src/ansi.rs` (the new `Handler` method, the performer's
`print_ascii`, tests). New items and tests are marked `Komando patch`. Everything else here is
the published crate as it is, with `Cargo.lock` and the registry's bookkeeping files left out.

## Tests

`cargo test --manifest-path vendor/vte/Cargo.toml --all-features` runs vte's own tests and the
patch's (it writes a `Cargo.lock` there, which vte's `.gitignore` leaves out):

- `print_ascii_runs`, `input_ascii_runs_and_repeat`, `input_ascii_default_calls_input`: what
  reaches the performer and handler, REP after a run, and the defaults.
- `ground_dispatch_matches_upstream`: the patched `ground_dispatch` against a copy of vte
  0.15.0's on random text (escapes, controls, C1, valid and invalid UTF-8).
- `print_ascii_matches_print_however_split`: random bytes fed whole, in random pieces and one
  byte at a time give the same events.

What this does to the grid is tested in `vendor/alacritty_terminal`.

## Why

See `vendor/alacritty_terminal/KOMANDO.md` for the measurements: together the two changes
parse coloured log output about 1.4 to 1.6 times as fast.

## Updating vte

Copy the new release's crate source over this folder (leaving out `Cargo.lock`,
`.cargo-ok` and `.cargo_vcs_info.json`), apply `komando.patch` with `patch -p1` (or redo the
change by hand where it no longer applies: it is small), regenerate the patch with
`diff -ruN a/src b/src > vendor/vte/komando.patch` (`a` the crate source, `b` this folder), run
the tests above and those in `vendor/alacritty_terminal`, and compare `throughput_parser_only`
before and after (see `docs/development.md`). alacritty_terminal must still depend on the same
vte version, or the patch for it won't apply; `Cargo.lock` should list one `vte` package
without a `source`. If vte gains a way to hand over runs of text of its own, use that and drop
the patch.
