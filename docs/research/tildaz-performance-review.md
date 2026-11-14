# TildaZ and Komando: recorded performance and transferable techniques

Reviewed 2026-11-14. No benchmarks, builds, or application tests were run. This is a comparison of existing measurements and a source inspection, not a measured optimization result.

## What the existing numbers establish

Komando's measurements are for **0.4.0**, not the current 0.4.1 checkout. Sources: [method](../benchmarks.md), [raw results](../performance/2026-11-14/results.json), and [environment](../performance/2026-11-14/environment.json). TildaZ's published Windows results are for **0.10.9**: [benchmark page](https://tildaz.dev/benchmarks/#windows).

| Metric | Komando, existing measurement | TildaZ, published Windows measurement | Interpretation |
|---|---:|---:|---|
| Coloured/ANSI output | 118.94 MiB/s | 137.8 MiB/s | TildaZ's number is 15.86% higher; workloads and endpoints differ |
| Warm startup | 187.26 ms to visible window | 341.4 ms to first program frame | Komando's number is lower, but it stops earlier |
| Memory | 22.45 MiB, one idle SSH Session | 33.1 MiB, empty terminal | Komando's number is lower; session/process workloads differ |
| Idle key-to-screen latency | No equivalent recorded GUI measurement | 9.43 ms median; 13.56 ms p95 | Cannot rank Komando |
| Plain ASCII output | No equivalent recorded GUI measurement | 322.3 MiB/s | Cannot compare to our coloured logs |
| SSH/SFTP/Jump Host performance | Existing SSH-specific measurements | No equivalent on the published page | No SSH ranking follows |

Conversion: `100,778,552 / 0.8080555 / 1,048,576 = 118.9397469 MiB/s`. Our documentation uses decimal MB/s; comparing its 124.7 MB/s directly to TildaZ's MiB/s understates the numerical difference. Never compare TildaZ's plain-text number to our ANSI workload as a speedup ratio.

The two experiments use different PCs, grids, Scrollback sizes, payloads, and statistics. Ours includes encrypted loopback SSH and a parser-completion reply. TildaZ's Windows path uses a local producer through ConPTY. **Neither result proves that one SSH client is faster.**

### A measurement detail that changes the conclusion

Although the website describes throughput as parsed and drawn end to end, the tagged [producer source](https://github.com/ensky0/tildaz/blob/v0.10.9/src/stress.zig#L334-L382) starts a timer, generates and writes the payload, then reads the timer immediately after the final write. It does not wait for the final parsed or presented frame. The [harness documentation](https://github.com/ensky0/tildaz/blob/v0.10.9/tool/stress/README.md#L574-L587) explicitly discusses unconsumed buffered output. The optional pane marker is sent after the timer has stopped.

Our timer waits for a cursor-position reply after the final line has been parsed, but also does not wait for final GPU presentation. These endpoints differ. The 16% numerical gap is a reason to inspect the code, not a demonstrated performance deficit or a prediction of an achievable gain.

## Source findings and implementation options

Inspected TildaZ tag `v0.10.9`, tree `6ae7a744844bf01e1ff0b54a442138cac9d25a09`. Its [dependency pin](https://github.com/ensky0/tildaz/blob/v0.10.9/build.zig.zon) uses Ghostty commit `c7f68a7f48c6a5b357689eb8c2328bc2dab5d8ed`. Suggestions below are engineering hypotheses; none has a measured Komando speedup.

### 1. Process printable text in runs: strongest throughput direction

Ghostty's [stream](https://github.com/ghostty-org/ghostty/blob/c7f68a7f48c6a5b357689eb8c2328bc2dab5d8ed/src/terminal/stream.zig#L899-L940) decodes/scans input in batches, keeps ASCII as bytes, and passes printable runs to its handler. This reduces repeated work per codepoint.

Komando currently feeds `alacritty_terminal::vte::ansi::Processor` in `src/session/ssh.rs:691`. The locked VTE 0.15.0 already uses SIMD-assisted `memchr` to find escapes, but its `ground_dispatch` still calls `print` per character; Alacritty's `Term::input` calculates width and handles modes per character. "Add SIMD" alone is therefore an incomplete proposal.

Two possible routes:

- **Keep Alacritty:** introduce a printable ASCII run interface across the parser/handler boundary. In the common case, process spans with a single style and width, update cursor/damage in batches, and retain the scalar path for Unicode, wrapping, insert mode, character-set translation, and controls. This requires dependency work, not a small caller-side shortcut.
- **Alternative terminal core:** explore libghostty-vt behind a terminal interface while retaining Komando's embedded SSH and UI. Its batching, Unicode handling and storage become available together, but renderer snapshots, selection, search, Command Marks, resize, replies and synchronized output must be adapted. This is the largest architectural option, with no demonstrated gain for Komando yet.

Do not bypass escape parsing with an application-side ASCII detector that assumes every chunk begins in ground state: an escape or UTF-8 sequence can cross chunk boundaries.

### 2. Reuse snapshots and row layout: useful for typing and TUIs

TildaZ calls `RenderState.update` before drawing in [its Windows renderer](https://github.com/ensky0/tildaz/blob/v0.10.9/src/renderer/windows.zig#L1325-L1349). Ghostty's [render state](https://github.com/ghostty-org/ghostty/blob/c7f68a7f48c6a5b357689eb8c2328bc2dab5d8ed/src/terminal/render.zig) retains row data, tracks damage and supports stable row identities.

Komando's `src/ui/terminal_view.rs:2380` copies every visible cell into a fresh vector on each prepaint; the following layout creates a fresh `Frame` and `RowScratch` and processes all visible rows. Our Alacritty dependency exposes `Term::damage` and `reset_damage`, which this rendering path does not consume.

An incremental renderer could retain row snapshots, highlight results, scratch capacity and row geometry, updating only damaged rows. Cursor, selection, search, palette, font, resizing, viewport movement and alternate-screen changes must invalidate the right data. Consume damage under the terminal lock with a clear owner. This should target less work per sparse update; a full-screen log flood can invalidate every row, so it is not automatically a bulk-throughput win. TildaZ still traverses visible cells to draw: its renderer is not evidence of universal partial GPU redraw.

### 3. Make shaping caches robust: concrete small change

TildaZ's [cluster cache](https://github.com/ensky0/tildaz/blob/v0.10.9/src/font/cluster_cache.zig) uses an inline fixed-size key, caches failed resolutions, and sizes its capacity to cover large visible screens. Its comments describe repeated whole-cache eviction causing extensive reshaping.

Komando already caches single characters and combining sequences. However, `GlyphCache::prepare` clears both maps when the cluster map exceeds 5,000 entries. A sufficiently diverse visible screen can therefore refill it and clear it again on the next frame. `GlyphCache::get` also allocates a new `String` for every combining-sequence lookup, including hits.

Adopt a bounded cache sized with viewport and style diversity in mind, retain useful entries rather than clearing everything, and use an allocation-free short key or borrowed lookup. Preserve an unrestricted fallback for long combining sequences. This targets Unicode-heavy frame stalls and allocations; it is unlikely to explain a lead on ordinary coloured ASCII logs. Existing caching means TildaZ's historical before/after shaping improvements cannot be applied as a multiplier to Komando.

### 4. Control Windows presentation queues: latency direction

TildaZ explicitly creates a [frame-latency waitable swap chain](https://github.com/ensky0/tildaz/blob/v0.10.9/src/renderer/windows.zig#L469-L557), bounds queued frames, and checks readiness before presenting. Its queue depth is chosen for its own loop timing; copying that constant is not a general latency optimization.

The installed `gpui-pre-windows` 0.3.12 source already uses instanced drawing and a flip-model swap chain. Its `directx_renderer.rs` creates the swap chain with `Flags: 0` and presents with interval zero; this file has no corresponding frame-latency waitable setup. Investigate presentation scheduling at the GPUI platform layer before proposing a fork or upstream change. Komando's existing GPU renderer already supplies batching, so a custom D3D renderer is not justified merely because TildaZ has one. There is no equivalent recorded Komando key-to-present distribution to establish a deficit.

### 5. Allocation recycling: inspect the principle, not the patch

TildaZ's [Ghostty patch](https://github.com/ensky0/tildaz/blob/v0.10.9/vendor/ghostty/patches/0001-reuse-pruned-heap-page.patch) reuses suitable pruned heap pages instead of freeing and reallocating them. This is specific to Ghostty's page allocator. Alacritty already recycles grid storage, and Komando already addresses oversized row allocations after resizing. The patch does not port directly to our current terminal core. Reusing our per-frame vectors is the nearer application of the same principle.

## What to preserve and avoid

- Preserve background parsing, embedded SSH, per-channel pacing, and SFTP flow control. TildaZ's local ConPTY throughput does not justify replacing these.
- TildaZ parses bounded chunks with a shared time budget on its event loop ([source](https://github.com/ensky0/tildaz/blob/v0.10.9/src/session_core.zig#L622-L646)). Copying that design would move work onto our UI thread; it is not an obvious improvement over our existing workers.
- Both applications suppress unnecessary redraws, cache shaped glyphs, and batch GPU drawing. These are existing strengths, not missing features.
- TildaZ documents removing Windows/Linux cluster-run shaping because it made font choice depend on neighbouring text ([architecture](https://github.com/ensky0/tildaz/blob/v0.10.9/ARCHITECTURE.md)). Parser batching is a separate proposal and must not be confused with that rejected renderer optimization.

## Recommended order

1. **Smallest concrete work:** robust cluster-cache eviction and allocation-free lookup, plus reuse of frame/snapshot scratch capacity.
2. **Everyday responsiveness:** damage-aware snapshots and cached row layout.
3. **Bulk throughput:** parser printable-run support; consider a libghostty-vt experiment only if the compatibility and maintenance costs are acceptable.
4. **Input latency:** GPUI/DXGI queue scheduling investigation, informed by equivalent latency measurements when future benchmarking is authorized.

This review itself changed no application code and ran no benchmarks.

## Outcome in Komando 0.4.2

Items 1–3 were implemented and kept; item 4 was not implemented. Old and new builds alternated on the
Linux development VM (4 cores), with nothing else running. These are not Windows measurements.

| Measurement | 0.4.1 code | 0.4.2 code |
|---|---:|---:|
| `throughput_parser_only`, Scrollback 100,000 / 10,000 / 0 (median of 3) | 55.9 / 70.0 / 74.5 MB/s | 78.8 / 110.3 / 130.0 MB/s |
| `throughput_coloured_log_lines_over_ssh`, 1,000,000 lines (median) | 1.77 s | 1.30 s |
| `render_bench_grid`, per-frame median: static / typing / TUI (3 rows) | 437 / 438 / 367 µs | 8.3 / 8.7 / 29 µs |
| `render_bench_grid`, per-frame median: full-screen flood | 445 µs | 385 µs |
| `render_bench_grid`, per-frame median: 7,434 distinct clusters | 20.5 ms (all reshaped every frame) | 0.37 ms (none reshaped) |

1. **Printable runs:** vendored vte 0.15.0 and alacritty_terminal 0.26.0 (`vendor/*/KOMANDO.md`).
   Printable ASCII goes to `Term::input_ascii` as a run. A differential test against the
   per-character path compares grids, cursors, modes and damage.
2. **Row reuse:** `layout_grid` in `src/ui/terminal_view.rs` reuses buffers and lays out again
   only rows that are damaged or affected by selection or cursor changes. This depends on
   alacritty's damage being complete; `layout_grid` is the only reader of the damage.
3. **Glyph cache:** two bounded generations, a separate ASCII table and allocation-free lookups
   for clusters of up to 8 characters.
4. **Frame-latency waitable swap chain:** not implemented. GPUI's Windows backend draws on
   `WM_PAINT` after a vsync thread invalidates the window, so at most about one frame is
   produced per refresh and no queue builds up for a waitable object to drain. Any change would
   be a GPUI fork that couldn't be compiled or measured on the Linux development host.

The largest remaining cost found is highlighting rows that contain non-ASCII text: about 6 ms per
frame on a 177 × 42 screen when such output scrolls. It is probably caused by the word boundaries
in the default rules, but this is unconfirmed.
