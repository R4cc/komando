# russh 0.65.1, patched for Komando

This is russh 0.65.1 from crates.io with one change, used through `[patch.crates-io]` in
Komando's `Cargo.toml`. `komando.patch` holds the change as a diff against the published crate.

## The change: a receive window per channel

russh keeps every channel's receive window topped up to one size for the whole connection,
`Config::window_size`. Komando needs two sizes on one connection:

- SFTP channels want a large window (8 MiB): a download can't go faster than one window per
  round trip, and SFTP paces its own requests (`src/sftp/flow.rs`).
- The shell channel wants a window that fits the link: whatever the server has sent but Komando
  has not shown still scrolls past after Ctrl+C, and a key's echo waits behind it. Komando sizes
  it from the delay SSH pings see (`src/session/pace.rs`).

The patch adds, on the client side only:

- `ChannelParams::window_target`: a window of the channel's own, used instead of
  `Config::window_size` when the channel's window is topped up. Such a window is topped up once
  an eighth of it (at least one packet's worth) is used, rather than half, so the server keeps
  close to all of it in flight: it is sized to what the link needs, and swinging between half
  and all of it would leave a slow link idle.
- `Handle::channel_open_session_with_window(window)`: opens a session channel with that window
  from the start, so the server never gets the connection's large window for it.
- `Handle::set_channel_window(id, window)`: changes it later. A larger window is granted at
  once; a smaller one takes effect as the server uses up what it was already allowed, since an
  SSH window can't be taken back.

Channels opened any other way, and the server side, behave exactly as in russh 0.65.1.

The change touches six files under `src/` (`lib_inner.rs`, `session.rs`, `client/mod.rs`,
`client/session.rs`, `client/encrypted.rs`, `server/encrypted.rs`; about 100 lines, most of
them two new methods and the plumbing to reach them, plus comments; the new
items are marked `Komando patch`, the other changed lines only add `window_target: None`). Everything else here is the published crate as it is, with `Cargo.lock` and
the registry's bookkeeping files left out.

## Why not without a patch

russh 0.65.1 (and its main branch as of November 2026) gives a client no say over window
adjustments: the target is always `Config::window_size`, applied to every channel, and the
`Handler::adjust_window` hook's answer is stored but never used on the client side. Without the
patch the only choice is one fixed window for shell and SFTP alike. Measured with
`throughput_ctrl_c_during_a_flood` (time from Ctrl+C to the prompt; flood speed):

| Link | Paced (patch) | Fixed 2 MiB | Fixed 8 MiB |
|---|---|---|---|
| 10 Mbit/s, 30 ms | 0.1 s; 1.2 MB/s | 1.3 s; 1.2 MB/s | 3.8 s; 1.2 MB/s |
| 100 Mbit/s, 50 ms | 0.1 s; 12.2 MB/s | 0.1 s; 12.4 MB/s | 0.4 s; 12.4 MB/s |
| 1 Gbit/s, 50 ms | 0.09 s; 110 MB/s | 0.08 s; 20 MB/s | 0.09 s; 81 MB/s |

A fixed 2 MiB also caps SFTP downloads at about 40 MB/s at 50 ms (2 MiB per round trip; 18.7
MB/s measured before the 8 MiB window, against 67 MB/s with it). A smaller fixed window shortens
the wait after Ctrl+C on slow links only in proportion (about 0.6 s at 10 Mbit/s for 1 MiB) and
costs fast distant links more.

## Updating russh

Copy the new release's crate source over this folder, apply `komando.patch` (or redo the change
by hand where it no longer applies), regenerate the patch with
`diff -ruN <crate source>/src vendor/russh/src > vendor/russh/komando.patch`, and run
`throughput_ctrl_c_during_a_flood` and `throughput_typing_during_sftp_transfers` (see
`docs/development.md`). If russh gains per-channel windows of its own, drop the patch.
