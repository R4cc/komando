# Socket → parser: how fast terminals and SSH clients move bytes, and what Komando should take

Researcher C. Scope: the path from the socket to `parser.advance`, plus process-wide settings
(allocator, release profile, socket options). Komando was read at the working tree of
2026-11-14 (HEAD `e2afb8b`, with uncommitted edits by another agent; nothing there was built or
run). Machine for all measurements here: the Windows 11 box from `docs/benchmarks.md`
(AMD Ryzen 7 9800X3D, Zen 5, AVX-512 + VAES).

Clones studied (shallow, read only), under `research/c/src/`:

| Project | Commit |
|---|---|
| russh (upstream main) | `b541663a88381aab9f6af1af7bbadb02e155058a` |
| openssh-portable | `268bdcb0c80876d97abdc74a1ca8992a1fdf7652` |
| hpn-ssh | `81efae0837575e1ae8e7022237f8bf4f985ec8d9` |
| libssh2 | `e75fa9a539d21e82d4920c126662e36984e97d61` |
| PuTTY | `29dd4b1755e22399602eb95c8167682a234349a0` |
| Eternal Terminal | `730db79df7de7e63c04539a7ec5b39ceed34fd56` |
| Mosh | `decd9b705eb81626f694335b8d5940538beb06da` |
| kitty | `8e5487e9d4a1dd79c3a769e0ba82326b8ad7c37f` |
| foot | `8768826c887c27721fddbf4670e27215b201bce2` |
| WezTerm | `e1eae81a56be8b5487dc77a29616afa568fb57b7` |
| Rio | `93983b78bbc9da340a92b657a063830a3ac6bf8e` |
| Windows Terminal | `f6de9e0618015def1c9dda5b82f9d3abf03eb799` |

---

## 1. Komando's SSH path today, traced

### 1.1 The stages and threads

```
socket (WSARecv, 8 MiB SO_RCVBUF)
  └─ russh session task (tokio worker A)
       cipher::read            read_exact(len probe) + read_exact(rest) into SSHBuffer.buffer
       OpeningKey::open        decrypt + verify in place
       maybe_decompress        buffer[5..].into()  → new Vec, copy #1
       Bytes::decode           Vec::<u8>::decode   → new Vec, copy #2
       adjust_window_size      maybe queue WINDOW_ADJUST
       chan.send(Data{data}).await   tokio bounded mpsc, 100 messages (Config::channel_buffer_size)
  └─ Komando shell task (tokio worker B; `ssh::shell`, tokio::select! loop)
       pace.output(...)        bookkeeping, maybe spawn a ping task
       Output::feed            FairMutex lock (two mutexes) → parser.advance(&mut term, &data)
                               → send Wakeup (AtomicBool gate) → watch_output (atomic check)
UI thread                      locks the same FairMutex to snapshot the grid
```

Sources (Komando, read-only): `src/session/ssh.rs:171` (`RECEIVE_BUFFER`), `:208`
(`client_config`, everything else `Default`), `:222` (`open_tcp`, `set_recv_buffer_size`,
`set_nodelay`), `:520–589` (the shell loop), `:691` (`Output::feed`);
`vendor/russh/src/cipher/mod.rs:295` (`read`), `src/client/mod.rs:1334` (`maybe_decompress`),
`:1473` (session `select!` loop), `src/client/encrypted.rs:590` (CHANNEL_DATA),
`ssh-encoding-0.3.0/src/decode.rs:214` (`impl Decode for Bytes` = `Vec::decode` + `into`).

### 1.2 What gets negotiated

`client::Config { ..Default::default() }` keeps `Preferred::DEFAULT`
(`vendor/russh/src/negotiation.rs:188–209`). As the client, russh's list order wins
(RFC 4253 §7.1; `negotiation.rs:493` iterates the client list first):

| Kind | russh default order (client) | Result against OpenSSH ≥ 8.x / russh server / asyncssh |
|---|---|---|
| Cipher | **chacha20-poly1305@openssh.com**, aes256-gcm@openssh.com, aes256-ctr, aes192-ctr, aes128-ctr | **chacha20-poly1305** (all three offer it) |
| MAC | hmac-sha512-etm, hmac-sha256-etm, hmac-sha512, hmac-sha256 | unused (AEAD) |
| Compression | **none**, zlib, zlib@openssh.com (flate2 feature) | **none** |

`aes128-gcm@openssh.com` is not in russh's default list at all.

So zlib is compiled in but never chosen, because `none` comes first. It should stay that way
(see §2.6).

### 1.3 Per-packet work and copies

The server sends channel data in packets of at most the client's `maximum_packet_size`
(russh default 32 768; `client/mod.rs:2812`). 100.8 MB of log output = ~3 100 packets. For each:

| Step | Cost per 32 KiB packet | For 100.8 MB |
|---|---|---|
| `vec![0; 4 or 16]` length probe (`cipher/mod.rs:299`) | 1 small alloc | ~3 100 allocs, negligible |
| 2 × `read_exact` on the raw `ReadHalf<TcpStream>` (no BufReader) | 2 recv calls via tokio's poll path | ~6 200 recv calls, ~5–10 ms |
| chacha20-poly1305 open (ring, 2 passes) | ~21 µs | **~64 ms** (measured, §1.4) |
| `maybe_decompress` → `buffer[5..].into()` | 32 KiB alloc + memcpy | ~3 ms copy + alloc |
| `Bytes::decode` → `Vec::decode` | 32 KiB alloc + memcpy | ~3 ms copy + alloc |
| bounded mpsc send + wake of the shell task | < 1 µs when busy, a few µs when it sleeps | ~1–10 ms |
| `FairMutex` lock (two `parking_lot` mutexes) | ~50 ns uncontended | negligible unless the UI holds it |
| `parser.advance` | ~150 µs (0.48 s / 3 100) | **~480 ms** |

Two memcpys and two 32 KiB heap allocations per packet are pure overhead but small
(≈ 6 ms + allocation; 32 KiB blocks are above Windows' LFH limit of 16 KiB, so they hit the
NT heap's back end, and the block is freed on another thread than it was allocated on).

The chunk reaching `advance` is exactly one SSH packet's payload: ≤ 32 759 bytes. Komando
never coalesces several queued `ChannelMsg::Data` into one `advance` call, and it takes the
`FairMutex` once per packet.

### 1.4 Measured: decrypt speed of the ciphers russh can use (ring 0.17.14)

`research/c/bench/src/bin/crypto.rs`, release, thin LTO, this machine, 512 MiB per cipher:

| Cipher (ring) | 32 KiB packets | 256 KiB packets |
|---|---:|---:|
| chacha20-poly1305@openssh.com open | **1 575 MB/s** | 1 580 MB/s |
| chacha20-poly1305@openssh.com seal | 1 588 MB/s | 1 601 MB/s |
| aes128-gcm open | 13 061 MB/s | 13 305 MB/s |
| aes256-gcm open | **12 681 MB/s** | 12 875 MB/s |
| memcpy alone (reference) | 121 549 MB/s | 148 792 MB/s |

ring's `chacha20_poly1305_openssh` is a two-pass construction (Poly1305 over the packet, then
ChaCha20; the OpenSSH variant cannot use ring's fused AVX2 ChaCha20-Poly1305 because the length
is encrypted with a second key), so it runs at ~1.6 GB/s. AES-GCM uses AES-NI/VAES + carry-less
multiply and runs eight times faster. **For 100.8 MB, chacha costs ~64 ms of client CPU on the
russh task; AES-256-GCM costs ~8 ms.** The same factor applies on the sending side
(in the throughput test the server is russh too, on the same machine).

### 1.5 Measured: the whole SSH path over loopback, without Komando's UI

`research/c/bench/src/bin/e2e.rs`: a russh 0.65.1 (Komando's patched copy) server on a
2-thread runtime prints the same 1 M coloured log lines (100.8 MB) in 64 KiB blocks, exactly
as `LogServer` in `src/throughput_tests.rs` does; a russh client on Komando's runtime shape
(7 workers, 8 MiB SO_RCVBUF, NODELAY) receives them and either discards them or parses them
into an alacritty 0.26 `Term` (177×42, 100 000 lines of Scrollback) under a `FairMutex`.
Fixed window (no pacing). Machine quiet, three runs each, medians:

| Configuration | Time | MB/s |
|---|---:|---:|
| chacha20-poly1305, discard | 0.190 s | 530 |
| aes256-gcm, discard | 0.153 s | 660 |
| chacha20-poly1305, parse | 0.564 s | 179 |
| aes256-gcm, parse | 0.557 s | 181 |
| parser alone, 32 KiB chunks (no SSH) | 0.545 s | 185 |
| parser alone, 256 KiB chunks | 0.539 s | 187 |

Window sensitivity (chacha, quiet machine): discard is 0.33 s at a 64 or 256 KiB window and
0.19–0.21 s from 1 MiB up; **parse is 0.54–0.58 s for every window from 64 KiB to 32 MiB.**
`maximum_packet_size` 16 384 / 32 768 / 65 535 made no difference to discard.

What this says:

1. **With the parser as fast as it is today, SSH is fully hidden behind it on loopback.** The
   russh read/decrypt task runs on another worker, in parallel with parsing; parse-over-SSH
   is within 2–4 % of parsing alone. Komando's extra time over the parser in
   `throughput_coloured_log_lines_over_ssh` (1.30 s vs 0.48 s on Linux; 0.81 s in the GUI on
   Windows) is therefore not transport cost. It must come from what Komando adds around
   the parser: the UI's lock hold time, Wakeups and the per-packet overheads below, and
   (only in the app) pacing. §1.6 isolates the first two.
2. **The SSH stage has ~3× headroom today (530–660 MB/s vs 180 MB/s).** It becomes the limit
   once the parser gets ~3× faster (other researchers' scope), and on a real network, where
   decrypt is on the per-connection critical path anyway. Chacha → AES-GCM raises that
   ceiling by ~25 % (0.19 → 0.15 s), both ends together.
3. **Komando's own test server sends ~8 KiB SSH packets, not 32 KiB.** `ChannelWriteHalf::data`
   (`vendor/russh/src/channels/mod.rs:352`) goes through `tokio::io::copy`, whose buffer is
   8 KiB, so a 64 KiB block becomes eight packets: 13 855 data messages for 100.8 MB, average
   7 273 bytes. Every per-packet cost in §1.3 is therefore paid ~4.5× more often than the table
   assumed. A real sshd behaves similarly or worse (see §2.1: it reads a Linux pty, which
   returns ≤ 4 KiB per read, and sends what it has).

### 1.6 Measured: per-message overhead, batching, and a UI thread holding the lock

Same bench, quiet machine, chacha, 8 MiB window, three runs each (all within ±1 %):

| Mode | 8 KiB packets (13 855 msgs) | 4 KiB packets (26 153 msgs) |
|---|---:|---:|
| parser alone, 32 KiB chunks | 0.510 s | — |
| `parse`: lock + `advance` per `ChannelMsg::Data` (what Komando does) | 0.539 s | 0.555 s |
| `batch`: take every message already queued (up to 64), lock once, `advance` each | **0.515 s** | **0.520 s** |
| `parse` + a UI thread that every 16.7 ms locks the Term and walks `display_iter` (as `run_once` in the throughput test does) | 0.547 s | — |
| `batch` + the same UI thread | 0.519 s | — |
| discard, chacha / aes256-gcm | 0.19 / 0.15 s | 0.21 / 0.16 s |

(4 KiB packets were forced with `maximum_packet_size = 4096`; that is roughly what a Linux
sshd sends for pty output, see §2.1.) Batching removes **4–6 %** and brings parse-over-SSH to
the parser-alone time. Per message, the bench's loop costs ~1.8 µs (wake-up of the receiving
task, `FairMutex` lock/unlock, the `advance` call's set-up). Komando's real loop does more per
message: a four-armed `tokio::select!` re-armed each time, `pace.output` with `Instant::now()`,
`sync_deadline`, `watch_output`, the Wakeup atomic. So the saving there is at least this
large. The bench's "UI" holds the lock only briefly, so its effect is small here; the real
GPUI frame holds it longer (renderer researcher's scope).

Under CPU load (another agent's build, 100 % CPU) the same runs took 0.9–1.2 s, and batching
saved 10–15 % there. The likely reason: fewer lock hand-offs and task wake-ups when workers
are contended.

**The gap in Komando's own benchmark (0.81 s in the GUI against a 0.48 s parser) is not explained
by transport cost.** Suggested split, which only the main agent can run (I may not build in
the Komando tree): run `throughput_coloured_log_lines_over_ssh` (a) as it is, (b) with
`FIXED_WINDOW = Some((8 MiB, true))` (no pacing), and (c) with the test's UI loop not locking
the Term. (a)–(b) is what pacing costs on loopback; (b)–(c) is lock contention.

---

## 2. How fast SSH clients and libraries do it

### 2.1 OpenSSH (portable `268bdcb`)

- **Reads straight into the channel buffer.** `channel_handle_rfd`
  (`channels.c:2274–2318`) uses `sshbuf_read(c->rfd, c->input, maxlen)` for plain channels,
  with `maxlen = CHANNEL_MAX_READ = 32 KiB` (`channels.h:263`), limited by the peer's
  window. There's no stack copy.
- **Packetizes whatever is buffered.** `channel_output_poll_input_open`
  (`channels.c:3184–3199`) sends `min(buffered, remote_window, remote_maxpacket)` per packet.
  sshd reads the pty once per poll round. A Linux pty master returns at most one
  `N_TTY_BUF_SIZE` (4 KiB) per read, so pty output usually reaches the client as ~4 KiB
  packets. Packets get bigger only when sshd is held up and the input buffer fills between
  rounds. So Komando should expect many small packets from real servers, and per-packet costs
  matter more than with its own test server.
- **Window:** session channels have `CHAN_SES_WINDOW_DEFAULT = 64 × 32 KiB = 2 MiB`
  (`channels.h:225`). `channel_check_window` (`channels.c:2526–2560`) sends a WINDOW_ADJUST
  once more than 3 packets are consumed or less than half the window is left. The window is
  credited when data is *consumed* (written to the local fd), not when it is received. That
  is the behaviour russh#730 asked for (§2.6).
- **Ciphers:** the client default order is `chacha20-poly1305, aes128-ctr, aes192-ctr,
  aes256-ctr, aes128-gcm, aes256-gcm`, so OpenSSH also negotiates chacha. With OpenSSL
  libcrypto, though, its chacha is OpenSSL's AVX-512/AVX2 code, much faster than ring's
  two-pass construction.

### 2.2 HPN-SSH (`81efae0`)

- **Dynamic window = the TCP receive buffer** (`channels.c:1425–1460`, `channel_tcpwinsz`;
  growth at `channels.c:2627–2636`). Each window adjust looks up `getsockopt(SO_RCVBUF)` and
  grows the SSH window to match, up to 128 MB. Against non-HPN OpenSSH ≥ 8.8 it caps at
  `NON_HPN_WINDOW_MAX` (~15 MB). This is what lets HPN fill long, fat links. Komando's `pace`
  already sizes the shell window from measured delay, and SFTP has 8 MiB, so the idea is
  already covered. The useful part is the coupling: never let the TCP buffer be smaller than
  the SSH window (§4.3).
- **`CHAN_RBUF` raised to 32 KiB** (`channels.h:262`; OpenSSH has 16 KiB).
- **Threaded ciphers:** `cipher-chachapoly-libcrypto-mt.c` has worker threads generate ChaCha20
  keystream ahead (`NUMTHREADS 1`, `NUMSTREAMS 64`). The main thread only XORs and runs
  Poly1305. HPN-README: "approximately 90 % gain in throughput" over serial chacha.
  `cipher-ctr-mt.c` does the same for AES-CTR. **For Komando the cheap answer is the same
  problem solved differently:** AES-GCM on AES-NI is already 8× faster than ring's chacha on
  one thread (§1.4), with no threads and no new code.
- **NONE cipher/MAC** after authentication is for bulk transfers only. It is refused for
  sessions with a tty (HPN-README, "NONE CIPHER"), so it doesn't apply.

### 2.3 libssh2 (`e75fa9a`), and WezTerm's SSH

- libssh2 reads from the socket into a per-session buffer of `PACKETBUFSIZE =
  MAX_SSH_PACKET_LEN = 35 000` bytes (`src/libssh2_priv.h:219,612`; `src/transport.c:512`), so
  one `recv` can bring in several small packets. russh issues two `read_exact` calls per
  packet instead (§4.5).
- Defaults: window 2 MiB, max packet 32 KiB (`include/libssh2.h:786–787`).
- **WezTerm's SSH path is an example of what not to do** (`wezterm-ssh/src/sessioninner.rs`): a
  session thread polls the socket and copies channel data into 8 KiB `VecDeque`s
  (`:897–1002`, `read_into_buf` at `:1090`). Those are written into a pipe that the pane's
  reader thread reads with a 1 MiB buffer (`mux/src/lib.rs:118,288`). The reader writes into a
  socketpair with 1 MiB SO_SNDBUF/SO_RCVBUF (`:268–273`), which the parser thread reads again
  (`:142`). That's three copies and two kernel round trips per byte.

### 2.4 PuTTY (`29dd4b1`)

- Default window is **16 KiB** (`ssh.h:47`, `OUR_V2_WINSIZE`), grown by 16 KiB each time the
  server runs it out (`ssh/connection2.c:608–611`), up to 1 GiB. It is credited back only for
  what the front end has consumed (`:619–620`): `bufsize` is the backlog the terminal hasn't
  taken yet. If the backlog exceeds the window, PuTTY throttles the whole connection (stops
  reading the socket, `:627–631`). "Simple" sessions use a 2 GiB window (`OUR_V2_BIGWIN`). Max
  packet is 16 KiB (`OUR_V2_MAXPKT`). This is conservative flow control aimed at Ctrl+C
  latency, close in spirit to Komando's `pace`, and slower on fat links.

### 2.5 Eternal Terminal and Mosh

- **Eternal Terminal** (`730db79`): plain TCP with its own reconnect journal (`BackedWriter`,
  64 MiB backup, `src/base/BackedWriter.hpp:32`). The server reads the pty with 16 KiB buffers
  (`src/terminal/TerminalServer.cpp:15`, `UserTerminalHandlerUnix.cpp:295`). There's no
  throughput technique Komando lacks.
- **Mosh** (`decd9b7`): it doesn't ship the byte stream at all. The server runs the terminal
  emulator and sends screen-state diffs at most every 20–250 ms (`SEND_INTERVAL_MIN/MAX`,
  `src/network/transportsender.h:49–50`). Floods look instant because intermediate output is
  dropped and the Scrollback isn't kept. That's incompatible with Komando's 100 000-line
  Scrollback and full-fidelity log output. Not applicable.

### 2.6 Rust SSH: russh upstream and its history

- Upstream main (`b541663`, still 0.65.1) has the same receive path as the vendored copy: the
  same `maybe_decompress` copy (`russh/src/client/mod.rs:1294–1310`), the same cipher order,
  and no buffered reader. **There's no upstream fix to pick up.**
- Relevant merged PRs (github.com/Eugeny/russh): #653 (2026-03) replaced mlocked `CryptoVec`
  with `Vec`/`Bytes` for non-secret buffers ("~21 % throughput"). Komando already has it in
  0.65. #695 removed write-path copies (`data_bytes`; Komando's input path already uses
  `channel.data_bytes`). #627 made `CryptoVec` faster. #532 (closed 2025-06) found that the
  ciphers were russh's main bottleneck and moved chacha to a faster implementation; that's
  the ring one measured here.
- Relevant design issues: #730 (closed unmerged) and #789 (closed). russh credits the window
  **on receipt**, and `chan.send().await` into the bounded 100-message queue parks the whole
  session loop when a consumer is slow. For Komando this means up to `channel_buffer_size ×
  packet` (100 × 4–32 KiB = 0.4–3.2 MB) sits in russh's queue on top of the SSH window. That
  affects Ctrl+C latency, not throughput; `pace.rs` already notes it. #770 is the issue
  Komando's window patch works around.
- `thrussh` (the predecessor) and `async-ssh2-lite` (a tokio wrapper over libssh2) bring
  nothing faster. async-ssh2-lite inherits libssh2's model plus a blocking-to-async bridge.

---

## 3. How fast terminals feed their parser

| Terminal | Read buffer | Batching | Locking while parsing | Source |
|---|---|---|---|---|
| **kitty** (`8e5487e`) | 1 MiB inside the parser (`kitty/vt-parser.c:18`, `BUF_SZ`) | The I/O thread `read()`s **directly into the parser's buffer** (`vt_parser_create_write_buffer`) and keeps reading until a short read. The main thread parses everything pending in one pass, and only after `input_delay` (3 ms), so wake-ups are rare | The parser runs on the main (render) thread, so parse and render never contend; the I/O thread only takes a short lock to append | `kitty/child-monitor.c:1803–1831`, `kitty/vt-parser.c:1611–1650` |
| **foot** (`8768826`) | 24 KiB on the stack | Up to 10 reads (~240 KiB) per event-loop round, then render, with a short render delay (`delayed_render_lower/upper_ns`) | Single-threaded; no locks | `terminal.c:262–297` |
| **WezTerm** (`e1eae81`) | 1 MiB reader, 128 KiB parser | Parses **without the terminal lock** into a `Vec<Action>`. It keeps reading while more data arrives within 3 ms (`mux_output_parser_coalesce_delay_ms`), then applies the actions in one batch | Applies under the pane lock, once per batch | `mux/src/lib.rs:118–240`, `config/src/config.rs:1675–1681` |
| **Rio** (`93983b7`) | 1 MiB (`READ_BUFFER_SIZE`), 64 KiB per `read` | Reads until `WouldBlock` **or 256 KiB parsed under one lock lease** (`MAX_LOCKED_READ`; its comment says "~1 ms of parsing … measured ~3 % more drain throughput than yielding per chunk") | `try_lock_unfair`, falls back to `lock_unfair` when the buffer is full | `rio-vt/src/performer/mod.rs:66–81, 260–330` |
| **Windows Terminal** (`f6de9e0`) | 128 KiB | **Overlapped `ReadFile`:** the next read is queued *before* the previous chunk is converted and parsed, so I/O and parsing overlap | Locks the buffer per chunk | `src/cascadia/TerminalConnection/ConptyConnection.cpp:745–800` |

What carries over to Komando:

- **Komando already overlaps I/O with parsing,** as Windows Terminal does: russh decrypts on
  one tokio worker while the shell task parses on another (§1.5). Keep that; don't fold the
  two into one task.
- **What Komando lacks is the lease** used by Rio, kitty and WezTerm: one lock and one wake-up
  per *burst*, not per SSH packet. With the chunks Komando gets (4–32 KiB), each packet pays
  the full lock, wake-up and `select!` cost. A bounded lease (Rio: 256 KiB ≈ 1 ms) keeps the
  renderer's wait short. This is the batching measured in §1.6.
- Copying straight into the parser's buffer (kitty) isn't needed: alacritty's `advance` takes
  a borrowed slice, and russh's `Bytes` is already the end of the copy chain.
- Parsing without the lock into an action list (WezTerm) allocates per action, and it's the
  parser researcher's territory. Not recommended from this scope.

---

## 4. Process-wide settings

All measured with the e2e bench on this machine, quiet, three interleaved runs per binary
(each variant built into its own target dir and deleted afterwards; the disk was nearly full).

### 4.1 Global allocator: mimalloc 0.1.52

| | System (HeapAlloc) | mimalloc |
|---|---:|---:|
| parser alone (alacritty 0.26, 100 k Scrollback) | 0.508 s | **0.480 s (−5.5 %)** |
| SSH + parse, batched | 0.514 s | 0.511 s (≈ 0) |
| SSH discard | 0.19 s | 0.19–0.21 s (≈ 0) |

mimalloc helps the parser alone, where Scrollback growth and row allocation dominate.
Komando's `RowPool` (uncommitted, `src/session/row_pool.rs`) already takes much of that
allocation off the parser thread, so the real gain is probably smaller. Over SSH the difference
vanished in this bench. Published evidence is anecdotal: a rust-analyzer maintainer measured
~8 s saved on Windows in analysis-stats, but noisy (rust-lang/rust-analyzer PR #19603);
informal reports put mimalloc's small-allocation malloc+free at 5–6× faster than the Windows
heap. **Risks:** a C dependency (cc build), and a higher idle working set, typically 1–4 MB
because mimalloc keeps segments per thread. That works against the advertised 23.5 MB for one
Session. Verdict: measure in Komando's own `throughput_parser_only` and idle-memory runs;
adopt only if it's ≥ 3 % faster and adds ≤ 2 MB idle.

### 4.2 Release profile and target CPU

| Build | parser alone | SSH + parse, batched |
|---|---:|---:|
| Komando's profile (thin LTO, codegen-units 4) | 0.507–0.515 s | 0.511–0.527 s |
| fat LTO, codegen-units 1 | 0.511–0.540 s | 0.516–0.593 s |
| thin LTO + `-C target-cpu=x86-64-v3` | 0.508–0.520 s | 0.507–0.529 s |

**There's no gain from either.** Fat LTO with one codegen unit was the same or slightly slower,
and makes linking much slower. x86-64-v3 brought nothing: ring dispatches AES-NI/AVX2 at run
time anyway, and the parser's hot loops aren't auto-vectorised. It would also stop the app
starting on Windows 11 machines without AVX2 (Windows 11 only needs x86-64-v2).
`panic = "abort"` wasn't measured, and it's risky for Komando: today a panic in one Session's
tokio task ends that task, but with abort it would kill the whole app and every Session in it.
Don't adopt any of these.

### 4.3 Socket options on Windows

- `TCP_NODELAY` is set (`ssh.rs:237`). That's correct for keystrokes, and it costs bulk
  receive nothing.
- `SO_RCVBUF = 8 MiB` (`ssh.rs:171,231`). On Windows, **setting SO_RCVBUF switches off
  receive-window auto-tuning for that socket.** Microsoft's networking blog ("Windows network
  performance suffering from bad buffering", TechCommunity, 2017) advises letting auto-tuning
  manage buffers, and Cygwin measured 12.6 → 151 Mbit/s after dropping a fixed ~280 KB
  buffer. Komando sets it on purpose (the comment records 3.5 s → 1.1 s on loopback), and
  that's fine for direct connections, whose SSH window is ≤ 8 MiB too. **But the first hop of
  a Jump Host chain gets an SSH window of `MAX_WINDOW = 32 MiB` (`ssh.rs:191, 405`) over a TCP
  socket fixed at 8 MiB.** TCP then caps the tunnel at 8 MiB per round trip (40 MB/s at
  200 ms), below what the 32 MiB window allows. Fix: pass the window into `open_tcp` and set
  `SO_RCVBUF = max(window, 8 MiB)`.
- Packet size: `maximum_packet_size` 16/32/64 KiB made no measurable difference (§1.5).
  Keep 32 KiB; russh logs an error above 65 535 (`client/mod.rs:1271`).

### 4.4 Compression

zlib is compiled in (`flate2` feature) but `none` comes first in russh's list, so it is never
negotiated: RFC 4253 makes `none` mandatory, so every server offers it. Keep it that way.
Inflate at a few hundred MB/s plus the server's deflate would cap throughput far below today's
180 MB/s, and compression only pays below ~10–20 Mbit/s. Compressing interactive traffic that
mixes typed secrets with attacker-influenced output also opens a CRIME-style length side
channel. Dropping the `flate2` feature would only save binary size.

### 4.5 Remaining copies and syscalls inside russh

Per packet, russh does two `read_exact` calls on the raw socket half, one tiny `vec!` for the
length probe, and two extra 4–32 KiB allocations and copies: `maybe_decompress` →
`buffer[5..].into()`, then `Bytes::decode` → `Vec::decode`. Upper bound for their cost, from
the discard runs: 4 KiB packets take 0.21 s against 0.19 s at 8 KiB, and that includes the
server's per-packet cost. So everything per-packet on the receive side is worth ≲ 20 ms per
100 MB, or ~3 % of the end-to-end time today.

A patch would make `SSHBuffer` a `BytesMut` filled by a `read_buf` of up to 64–256 KiB. Packets
would be decrypted in place, and channel data handed out as `split_to(..).freeze()` slices of
it, which removes both copies and most syscalls. It would be worthwhile once the parser is
2–3× faster, but it's a sizeable patch to maintain against upstream (`cipher::read`,
`maybe_decompress`, `IncomingSshPacket`, every `Bytes::decode` site), so better offered
upstream first.

---

## 5. Concrete changes for Komando

### 5.1 Parse every queued packet under one lease (`src/session/ssh.rs`, the shell loop)

After `channel.wait()` yields `Data`, keep taking messages that are *already queued* without
waiting, using `futures::FutureExt::now_or_never` on `channel.wait()` (russh's `wait` is a tokio
mpsc `recv`, which is cancel-safe). Stop at a byte budget (Rio uses 256 KiB, about 1 ms of
parsing) or at the first non-`Data` message, which must then be handled after the batch, in
order. Then:

- call `pace.output(total_len, now)` once;
- take `term.lock()` once and call `parser.advance` for each chunk;
- check `sync_bytes_count` once and send one Wakeup;
- call `watch_output` per chunk (it is a single atomic load when nothing waits).

Keep the per-chunk path while `sudo` is `Some`, since `show_watching_sudo` looks at each
chunk. Sketch:

```rust
Some(ChannelMsg::Data { data }) | Some(ChannelMsg::ExtendedData { data, .. }) => {
    let mut batch = vec![data];          // Bytes clones: no copy
    let mut size = batch[0].len();
    let mut after = None;                // a non-data message that ended the batch
    while size < LEASE {
        match channel.wait().now_or_never() {
            Some(Some(ChannelMsg::Data { data } | ChannelMsg::ExtendedData { data, .. })) => {
                size += data.len(); batch.push(data);
            }
            Some(other) => { after = Some(other); break }   // handle after the batch
            None => break,                                   // nothing queued
        }
    }
    if paced && pace.output(size, Instant::now()) { ping = Some(send_ping(handle)); }
    output.feed_all(term, proxy, &batch);  // one lock, one Wakeup
    // ...then process `after` exactly as the outer match would
}
```

A reusable `SmallVec<[Bytes; 16]>`, or a `Vec` kept in the loop, avoids the allocation.
Measured in the bench: −4 to −6 % end to end, up to −10 to −15 % with the CPU contended.

### 5.2 Prefer AES-GCM when the CPU has AES-NI (`client_config`)

```rust
use russh::cipher::{AES_128_CTR, AES_128_GCM, AES_192_CTR, AES_256_CTR, AES_256_GCM, CHACHA20_POLY1305};
let aes = std::arch::is_x86_feature_detected!("aes") && std::arch::is_x86_feature_detected!("pclmulqdq");
let cipher: &'static [russh::cipher::Name] = if aes {
    &[AES_256_GCM, AES_128_GCM, CHACHA20_POLY1305, AES_256_CTR, AES_192_CTR, AES_128_CTR]
} else {
    &[CHACHA20_POLY1305, AES_256_GCM, AES_256_CTR, AES_192_CTR, AES_128_CTR]
};
client::Config { preferred: Preferred { cipher: Cow::Borrowed(cipher), ..Preferred::DEFAULT }, .. }
```

- **Why it is fast:** ring's AES-GCM uses AES-NI/VAES and carry-less multiply, 12.7 GB/s. ring's
  `chacha20-poly1305@openssh.com` is two passes without the fused AVX2 kernel, 1.6 GB/s
  (§1.4). Every CPU that runs Windows 11 has AES-NI and PCLMULQDQ; the check is only for
  correctness elsewhere (Linux development hosts, ARM64).
- **Security:** both are AEADs that OpenSSH enables by default. AES-GCM was never affected by
  Terrapin (CVE-2023-48795); chacha20-poly1305 was, and is safe only with strict-kex, which
  russh negotiates. With AES-NI, AES-GCM is constant time; without it ring falls back to a
  constant-time bit-sliced AES, so even a wrong guess is slow, not unsafe. OpenSSH puts chacha
  first only for the sake of machines without AES hardware. A server that lacks GCM falls
  back to chacha automatically.
- **Gain:** 0 % for one Session on loopback today, because decrypting overlaps with parsing.
  It saves ~56 ms of client CPU per 100 MB, and as much again on a russh or OpenSSH server.
  The SSH stage's ceiling rises from 530 to 660 MB/s on loopback (+25 %). It helps whenever
  workers are shared: several Sessions flooding at once (`ssh_threads` notes 8 Sessions are
  CPU-bound), SFTP next to a flood, and any future where the parser is 2–3× faster.

### 5.3 Size the socket buffer to the window (`open_tcp`)

Pass `window` into `open_tcp` and call `set_recv_buffer_size(window.max(RECEIVE_BUFFER))`.
This matters only for a Jump Host's first hop (32 MiB window, 8 MiB fixed TCP buffer). The
8 MiB fixed buffer disables Windows auto-tuning, so TCP, not SSH, caps throughput there
at 8 MiB per round trip.

---

## 6. Ranked recommendations

| # | Technique | Effort | Expected gain (1 M coloured lines unless stated) | Confidence |
|---|---|---|---|---|
| 1 | **Split the existing gap before optimising.** Run `throughput_coloured_log_lines_over_ssh` paced, with `FIXED_WINDOW = (8 MiB, true)`, and with the test's UI loop not locking. The SSH transport alone adds only 2–4 % to parsing (§1.5); the other ~0.3 s Komando shows is pacing or lock contention, and this says which | XS (3 runs, no code) | Directs everything below; parsing over SSH in the bench matches parser-alone time | High |
| 2 | **Parse all queued packets under one lock lease** (≤ 256 KiB; one lock, one Wakeup, one `pace.output`), as Rio, kitty and WezTerm do (§5.1) | S (~40 lines in `ssh.rs`) | −4 to −6 % measured in the bench (per-packet overhead ~1.8 µs × 14–26 k packets); more in Komando, whose per-message loop is heavier; −10 to −15 % with the CPU contended | High on direction, medium on size |
| 3 | **Prefer AES-256/128-GCM over chacha20-poly1305 when AES-NI is present** (§5.2) | XS (cipher list in `client_config`) | ≈ 0 % for one Session today; −56 ms of client CPU per 100 MB (8× cheaper decrypt), +25 % SSH-stage ceiling; an estimated 5–10 % with several Sessions flooding at once, and 10–20 % once the parser is 2–3× faster | High (cipher speeds measured); medium (end-to-end) |
| 4 | **Socket receive buffer ≥ the connection's SSH window** (Jump Host first hop: 32 MiB) (§5.3) | XS | Only for long, fast links through Jump Hosts: up to 4× there (8 → 32 MiB per round trip); none on direct connections | Medium |
| 5 | **Try mimalloc**, keeping it only if Komando's own parser test gains ≥ 3 % and idle memory grows ≤ 2 MB (§4.1) | XS to try | 0–5 % (−5.5 % parser-alone in the bench, ≈ 0 over SSH); +1–4 MB idle working set | Low–medium |
| 6 | **Zero-copy, buffered receive in russh** (`BytesMut` read buffer, decrypt in place, `Bytes` slices for channel data; one `recv` per 64–256 KiB instead of 2 per packet) (§4.5); better as an upstream PR than a bigger vendored patch | M–L | ≲ 3 % today; ~10 % of the SSH stage later | Medium |

**Don't adopt** (measured or reasoned above):

- fat LTO + codegen-units 1 and `target-cpu=x86-64-v3`: no gain, measured (§4.2).
- `panic = "abort"`: one Session's panic would kill every Session.
- zlib compression: it would cap throughput and opens a length side channel (§4.4).
- larger `maximum_packet_size`: no difference, measured.
- HPN-style multithreaded ciphers: AES-GCM is simpler and 8× faster.
- the NONE cipher: never for ttys.
- WezTerm's multi-hop pipe design and Mosh's state sync: both incompatible.
- switching ring for aws-lc-rs: not measured; it adds a C/CMake build, and its AES-GCM is in
  the same class as ring's.

Bench sources: `research/c/bench/src/bin/crypto.rs`, `research/c/bench/src/bin/e2e.rs`
(russh, vte and alacritty_terminal are Komando's vendored copies, copied into
`research/c/bench/`). Disk caveat: the C: volume was at 0–1.7 GB free during this work, and an
external build loaded the CPU for part of the time. Only runs on a quiet machine are quoted,
except where marked "under load".
