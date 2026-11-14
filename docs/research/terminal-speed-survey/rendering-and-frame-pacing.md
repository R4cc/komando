# Research B: from "the grid changed" to pixels during heavy output

Researcher B of three. Scope: renderer/parser decoupling, frame pacing during floods, GPU cell
grids, highlighting/regex at speed, per-frame extra work (minimap). Source inspection only:
nothing was built or run, and nothing in `C:\Users\Admin\komando` was changed. Komando was read
as the working tree on 2026-11-14 (HEAD `e2afb8b` plus uncommitted edits by another agent;
line numbers refer to that working tree and may drift).

Clones (shallow, read-only) are under `research/b/src/`. Commits used for links:

| Project | Commit |
|---|---|
| Windows Terminal (`microsoft/terminal`) | `f6de9e0618015def1c9dda5b82f9d3abf03eb799` |
| kitty | `8e5487e9d4a1dd79c3a769e0ba82326b8ad7c37f` |
| foot (codeberg) | `8768826c887c27721fddbf4670e27215b201bce2` |
| WezTerm | `e1eae81a56be8b5487dc77a29616afa568fb57b7` |
| Rio | `93983b78bbc9da340a92b657a063830a3ac6bf8e` |
| contour | `8aa5313325b66561b81120464aebb32a4dce6af2` |
| zutty (deepin mirror of tomscii/zutty) | `3515b63e39edd2ab808baf8cbd9216cfdc7209df` |
| refterm (mirror of cmuratori/refterm) | `f7e612b6d63e03a3562202f19f9bfbc4c9d5598f` |
| refterm-x11 (port) | `26fa719f856eba9592fc0a2ab431a45a42da64bf` |
| refterm-hash-break (m1el) | `ea68e5f7dadca1535b0e2ad723f7770f89203a8b` |

---

## 1. What Komando's UI thread does per frame during a flood

### Frame trigger and pacing (already sane)

- The SSH task parses each `ChannelMsg::Data` under `term.lock()`
  (`src/session/ssh.rs:692`, `Output::feed`), then sends `Event::Wakeup`.
- `EventProxy::send_event` (`src/session/mod.rs:294-299`) coalesces wakeups with an `AtomicBool`:
  only the first wakeup after the UI cleared the flag is queued.
- `on_session_event(Wakeup)` (`src/ui/terminal_view.rs:594-612`) clears the flag, calls
  `drop_blank_scrollback` (locks the term; returns immediately once history > screen lines,
  `src/session/mod.rs:376-381`), `search_output` (debounced, cheap) and `cx.notify()`.
- GPUI's Windows backend (`gpui-pre-windows-0.3.12/src/platform.rs:429-485`) runs a
  `VSyncProvider` thread that `RedrawWindow(RDW_INVALIDATE)`s every window on each vsync; the
  `WM_PAINT` handler (`events.rs:1353`) calls `request_frame`, which draws only if a view is
  dirty. `Present(0, 0)` (`directx_renderer.rs:251`) on a flip swap chain. So Komando already
  draws **at most one frame per refresh and skips intermediate states** (Mosh's
  "latest state only" idea is effectively already in place). Frame pacing is not the problem;
  per-frame cost is.

### Per-frame work, in order, with lock windows

| Step | Where | Holds `term.lock()`? | Cost during a flood |
|---|---|---|---|
| `render()` builds the element tree | `terminal_view.rs:2711-3070` | briefly, several times | small |
| `render_rail` (Command Marks) | `terminal_view.rs:1489-1499` | yes, `marks.rows(&term)` | small unless many marks |
| `search_view` | `terminal_view/search.rs:368` | yes (only with an open search) | small |
| password-prompt probe | `terminal_view.rs:2767-2773` | yes, builds a `String` of the cursor row | small; allocates every frame while focused |
| minimap `beside`/`over` sizing | `minimap.rs:749`, `minimap.rs:782` | yes, `content_lines` | small |
| **minimap strip prepaint** `layout` | `minimap.rs:677-680` → `layout` `minimap.rs:504-607` | **yes, for the whole layout incl. regex** | **large: see below** |
| grid prepaint `layout_grid`, locked part | `terminal_view.rs:2357-2426` | yes | damage scan + `renderable_content` + `copy_rows` of every dirty row (whole screen while scrolling) |
| grid prepaint, unlocked part | `terminal_view.rs:2427-2462` → `layout_row` | no | highlight regex per dirty row + glyph cache + vectors |
| IME preedit probe | `terminal_view.rs:2892` | yes, only while composing | tiny |
| paint: grid | `terminal_view.rs:~2947-3000` | no | one `paint_glyph` per glyph inside one `paint_layer` |
| paint: minimap | `minimap.rs:684-692` | no | one `paint_quad` per run, **not** in a `paint_layer` |

### Hotspot A (highest): the minimap re-highlights ~400–500 brand-new lines per frame while holding the terminal lock

`strip()`'s prepaint takes `term.lock()` (`minimap.rs:677`) and calls `layout()` with it held.
`layout()` walks `capacity = strip_height / LINE_H` lines (`LINE_H = 2 px`, `minimap.rs:45`), so
a 1000-px-tall terminal shows ~490 minimap lines, each up to `shown_cols` = (width − 16 px) / 1 px
= 40–94 columns (`MIN_WIDTH`/`MAX_WIDTH` 56–110 px, `minimap.rs:40-41`). For each line it hashes
the raw cells (cheap), and on a cache miss resolves colours and runs **`highlighter.styles()`**
(`line_runs`, `minimap.rs:609-614`), which allocates a `String` and a `Vec<Option<Style>>` and
runs the 62-pattern `RegexSet` plus each matching rule's `find_iter`/`captures_iter`.

The `LineCache` (`minimap.rs:269-283`) keys by content hash, so it helps when scrolling through
old output, but during a flood of distinct log lines (timestamps differ) *every visible minimap
line is new every frame*: at ~120 MB/s and ~100 bytes/line, ~20,000 lines arrive per 16.7 ms
frame, far more than the ~490 lines the strip shows. Result: per frame the minimap runs the
highlighter on ~490 × ~94 ≈ 46,000 characters — about **6× the text of the main grid**
(42 × 177 = 7,434 cells) — **with the terminal locked**, so the parser stalls for that entire
time. The cache also grows past `CACHE_LINES = 8192` within ~17 frames and is then cleared
wholesale (`minimap.rs:529-532`), which adds rehash/realloc churn.

If the main grid's highlight costs ~6 ms/frame on non-ASCII rows (the known number), the
minimap's identical work is plausibly several ms/frame even for ASCII and tens of ms for non-ASCII
rows, all of it inside the lock. This is the single most likely reason UI work slows the parser.
(Unmeasured; inferred from code. A `perf::time("minimap layout")` counter already exists at
`minimap.rs:680` and would confirm it in one run.)

### Hotspot B: highlight regexes fall back to the PikeVM on any non-ASCII row (cause confirmed in source)

The bundled set has 62 rules, all regexes; **53 use `\b`**, 60 are case-insensitive, 27 use
capture groups (counted from `assets/highlights/default.json` in the working tree).
`match_styles` (`src/highlight.rs:~245`) first runs `RegexSet::matches(line)` (an *overlapping*
search across all 62 patterns), then `find_iter`/`captures_iter` per matching rule.

In `regex-automata 0.4.18` (the locked version), a Unicode `\b` makes the lazy DFA treat every
non-ASCII byte as a *quit* byte (`hybrid/dfa.rs:3220-3240`, `Config::unicode_word_boundary`):
on the first non-ASCII byte in the haystack the DFA gives up and the meta engine falls back to
the PikeVM (`meta/strategy.rs:870-906`, `which_overlapping_imp`: "using PikeVM for overlapping
search"). A PikeVM over a 62-pattern union NFA steps every live NFA state for every byte; that is
typically 10–100× slower than the DFA. Capture searches (`captures_iter`, used by 27 rules) need
the PikeVM/backtracker for the group slots anyway, but only after the DFA found the match span,
which also fails on non-ASCII rows. So the "regex word boundaries" hypothesis in
`tildaz-performance-review.md` has a concrete mechanism: **any row with one non-ASCII character
(●, ×, é, box drawing, CJK) runs the whole set on the PikeVM.** Fixes are in §9 (technique T2).

### Hotspot C: `copy_rows` under the lock is per-cell work that could be per-row memcpy

`Snapshot::copy_rows` (`terminal_view.rs:2172-2214`) runs under the lock for every dirty row and,
per cell: palette resolution of fg and bg (`palette.resolve`, a match on `Color`), an INVERSE swap,
`cell.zerowidth()`, `underline_color()` (both touch the cell's `extra` Arc), and
`selection.contains(point)` per cell. During a flood all 42 rows are dirty every frame (scrolling
damages everything), so ~7,400 cells × maybe 20–40 ns ≈ 0.15–0.3 ms locked. That is small next to
Hotspot A, but it is pure lock time. Copying the raw `Cell`s (alacritty `Cell` is 24 bytes; a row
is a contiguous `Vec<Cell>`, so `clone_from_slice` per row) and resolving colours/selection after
unlocking would cut the locked portion to a memcpy (~7,400 × 24 B ≈ 180 KB ≈ 20–40 µs).
Cells with `extra` (zero-width chars, underline colour, hyperlinks) clone an `Arc`, still cheap.

### Hotspot D: paint is GPUI per-glyph work, unmeasured by `render_bench_grid`

`render_bench_grid` measures `layout_grid`. The paint closure then calls `window.paint_glyph` per
glyph (`gpui-pre-0.3.12/src/window.rs:5071-5145`). Each call clones `RenderGlyphParams`, does an
`RwLock` + `HashMap` lookup in `raster_bounds`, a lock + lookup in the sprite atlas, computes the
subpixel variant, and pushes a sprite. Inside Komando's `paint_layer` (already used,
`terminal_view.rs:~2953`) primitives skip the `BoundsTree` insert, which is the expensive part
elsewhere. Estimated 100–250 ns per glyph → 0.75–1.9 ms for a full 177 × 42 screen, every frame,
off-lock. Then GPUI sorts the scene's primitive vectors and uploads them. This cost is fixed per
frame in GPUI; it can only be reduced by fewer glyphs (skip spaces, already done) or by a GPUI
fork (see §5, refterm/zutty).

The minimap's paint does **not** use `paint_layer` (`minimap.rs:684-692`), so each of its
thousands of run quads pays a `BoundsTree` insert (`gpui-pre-0.3.12/src/bounds_tree.rs:120`).

### Hotspot E: small per-frame allocations

- `render()` password probe: `String` of the cursor row every frame while focused
  (`terminal_view.rs:2770`).
- `minimap::layout` allocates `rects: Quads` and `cells` per frame (`minimap.rs:539-541`), plus
  `String`/`Vec` per highlighted line (`minimap.rs:612-613`); `layout_grid` already reuses buffers.
- `GridFont::new` per prepaint (`terminal_view.rs:2817`) resolves the font and measures `'m'`
  each frame (hash lookups in GPUI's text system; cheap but avoidable).

### How long the UI holds `term.lock()` per frame during a flood (estimate)

`FairMutex` (alacritty) hands the lock over fairly, and the parser re-takes it per SSH packet
(≤ 32 KB), so the parser waits at most one UI critical section at a time. Per frame:

| Critical section | Estimated time held |
|---|---|
| render-time probes (rail, password, minimap sizing) | ≲ 50 µs total |
| minimap `layout` (miss-heavy flood, ASCII rows) | **~1–5 ms** (unmeasured) |
| minimap `layout` (non-ASCII rows, PikeVM) | **~5–30 ms** (unmeasured; extrapolated from the 6 ms grid number × ~6 text volume, minus caching) |
| `layout_grid` locked part (all rows dirty) | ~0.15–0.4 ms |

At 60 Hz, a 5 ms locked section is ~30 % of wall time in which the parser cannot advance.
With the minimap disabled the locked time per frame is well under 0.5 ms (~3 %).

---

## 2. Windows Terminal (AtlasEngine + Renderer)

Base: `https://github.com/microsoft/terminal/blob/f6de9e0618015def1c9dda5b82f9d3abf03eb799/`

**Lock discipline.** `Renderer::_PaintFrame` (`src/renderer/base/renderer.cpp#L402-L467`) takes
the console lock, ticks timers, computes invalidation, and calls `_PaintFrameForEngine` for each
engine *under the lock*. For AtlasEngine that is `StartPaint`/`PaintBufferLine`
(`src/renderer/atlas/AtlasEngine.cpp#L486-L538`), which copies each dirty row's text and colours
into the engine's own `ShapedRow`s and does the DirectWrite font fallback and glyph mapping
there. Only `Present()` runs unlocked: the file header says "Present() is called without the
console buffer lock being held. --> Put as much in here as possible"
(`src/renderer/atlas/AtlasEngine.r.cpp#L10-L53`), and it does the D3D work (`_b->Render(_p)`).
So WT's lock window is *larger* than Komando's (Komando shapes after unlocking). Komando's
snapshot-then-unlock design is already ahead here; nothing to copy.

**Frame pacing.** Waitable swap chain with `SetMaximumFrameLatency(1)`
(`AtlasEngine.r.cpp#L381-L386`); before each frame `_waitUntilCanRender` blocks on the
frame-latency object, or sleeps 8 ms when nothing was presented
(`AtlasEngine.r.cpp#L448-L476`); `Present1(1, 0, &dirtyRects)` with vsync. The render thread is
woken by an atomic `_redraw` flag + `WaitOnAddress` (`renderer.cpp#L262-L291`), i.e. coalesced
"latest state" rendering, like Komando's `AtomicBool` wakeup + GPUI's vsync invalidation.

**Scroll = rotate cached rows, not re-lay-out.** When the viewport scrolled by N rows, AtlasEngine
rotates its `ShapedRow` pointers and `memmove`s its per-cell colour bitmaps by N rows, then only
re-fills the N new rows (`AtlasEngine.cpp#L156-L229`). It also uses `Present1` scroll rects.
Komando treats a scroll as full damage (alacritty marks everything damaged on scroll), so every
row is laid out again even when one line arrived (e.g. `tail -f` at a few lines per frame). See
technique T6.

**GPU cell data.** Backgrounds and foreground colours are kept as a per-cell `u32` bitmap
(`colorBitmap`, one texel per cell) that is uploaded only when its generation changes and drawn
with one full-screen quad (`BackendD3D.cpp#L1072-L1105`). Glyphs are still CPU-generated instanced
quads (`QuadInstance`, `BackendD3D.cpp#L77-L82`, `#L958-L1012`), deduplicated through a
glyph-atlas hash map. Background-as-texture would save Komando one quad per background run, but
GPUI exposes no way to draw a custom texture with a custom shader (see §5).

**Regex/URL patterns: only when output goes idle.** `ControlCore` runs
`UpdatePatternsUnderLock()` from a `throttled_func` with `delay = 100 ms, debounce = true,
trailing = true` named `outputIdle` (`src/cascadia/TerminalControl/ControlCore.cpp#L205-L226`).
During a flood the debounce keeps resetting, so no URL scanning happens at all until output
pauses. Directly relevant to Komando's minimap highlighting (T1) and, optionally, to grid
highlighting during floods (T4).

**Applies to Komando:** debounced/idle-only regex work (yes, for the minimap; optional for the
grid); scroll row rotation (yes, for moderate-rate output; useless during a full flood).
**Does not apply:** the lock structure (Komando's is already better); waitable swap chain
(GPUI-internal, already analysed in `tildaz-performance-review.md` item 4).

---

## 3. kitty

Base: `https://github.com/kovidgoyal/kitty/blob/8e5487e9d4a1dd79c3a769e0ba82326b8ad7c37f/`

**Parser/renderer decoupling: same thread, separate I/O thread, tiny lock.** The I/O thread only
`read()`s into the parser's 1 MB buffer; the buffer lock covers bookkeeping of
`read.sz`/`write.pending`, not parsing (`kitty/vt-parser.c#L1598-L1700`, `run_worker`: "consume
all already read input without lock cycling, the writer thread only modifies write.pending and
buffer space beyond read.sz + write.pending"). Parsing and rendering both run on the main thread,
so there is no parser/renderer lock at all; instead they take turns.

**Frame pacing in floods: `input_delay` + `repaint_delay`.**
- `input_delay` (default 3 ms, `kitty/options/definition.py#L1543-L1556`): the I/O thread wakes
  the main loop for a large chunk only after it has aged 3 ms, so parsing happens in big batches;
  small pending input (< 1 KB, typed echo) wakes immediately but at most once per period
  (`kitty/child-monitor.c#L2097-L2137`, `classify_pending_input` `vt-parser.c#L1602-L1616`).
  Near-full buffers bypass the delay so the producer never blocks.
- `repaint_delay` (default 10 ms, `definition.py#L1528-L1540`): `render()` returns early if the
  last render was < 10 ms ago and no input was just read (`child-monitor.c#L1228-L1235`).
  Windows whose new bytes are still held are not repainted (`#L1243-L1251`), avoiding a frame of
  stale content.

**Per-line render caching that survives scrolling.** `screen_update_cell_data`
(`kitty/screen.c#L4404-L4463`) walks the visible lines; only lines whose own
`attrs.has_dirty_text` flag is set get `render_line` (shaping → sprite indices written *into the
line's `GPUCell`s*) and `mark_text_in_line` (regex markers, stored as mark bits in the cells).
Every visible line is then `memcpy`'d into the GPU cell buffer (`update_line_data`,
`screen.c#L4236-L4239`). Because the dirty flag and the shaped/marked results live on the line,
a line that scrolls up costs a memcpy, not a reshape or a regex run; history lines are re-rendered
only when dirty.

**GPU cell buffer.** The whole screen is one instanced draw over a `GPUCell` array (sprite
indices, colours, attrs), with the colour table in a uniform block (`kitty/shaders.c#L1351-L1403`,
`cell_prepare_to_render`). The CPU never computes per-glyph pixel positions.

**Regex highlighting ("markers").** `marker_from_regex` / `marker_from_multiple_regex`
(`kitty/marks.py#L15-L50`) — one combined alternation with named groups, Python `re`. Fast only
because it runs once per dirty line at render time (`screen.c#L4450`, `#L4457`), never per
frame per line.

**Applies to Komando:**
- *Highlight results cached per line content and reused across frames and scroll* (T3): the
  direct analogue of kitty's marks-in-cells. Komando cannot add bits to alacritty's `Cell`
  cheaply, but can cache by row content hash (the minimap already does this).
- *A minimum repaint interval during floods* (T5): GPUI already caps at vsync; an explicit
  "while busy, render at most every N ms" (e.g. 16–33 ms) would halve UI lock time at 120/144 Hz.
- *GPU cell buffer:* not reachable through GPUI 0.3.12 (see §5).

---

## 4. foot

Base: `https://codeberg.org/dnkl/foot/src/commit/8768826c887c27721fddbf4670e27215b201bce2/`

**Single thread, bounded read batches.** The PTY handler reads at most 10 × 24 KB per event-loop
turn before going back to the loop (`terminal.c#L273-L298`), so a flood can never starve
rendering for more than ~240 KB of parsing. Parsing and rendering share the thread, so there is no
lock; the bound is what keeps frames coming.

**Two-timer delayed render** (`terminal.c#L300-L366`, documented in `doc/foot.ini.5.scd#L1909-L1970`):
every read re-arms a short *lower* timer (default 0.5 ms); a second *upper* timer (default
~8.3 ms) is armed only once and reset only by a render. So bursts are coalesced into one frame,
but a steady trickle cannot postpone rendering beyond the upper bound. When the renderer is busy,
"we're rate limited by the wayland compositor anyway".

**Frame-callback gating = skip intermediate frames.** `fdm_hook_refresh_pending_terminals`
(`render.c#L5146-L5207`) renders only if no frame callback is outstanding; otherwise it just ORs
the request into `render.pending` and the frame callback renders the *latest* grid once the
compositor is ready. Exactly one frame per display refresh, always showing the newest state.
(GPUI's vsync-driven `WM_PAINT` gives Komando the same property.)

**Damage that travels with rows + pixel scrolling.** `row->dirty` lives on the grid row, so it
moves with the row on scroll; scroll damage is applied as a `memmove` of the previous frame's
pixels (`grid_render_scroll`, `render.c#L1318-L1430`), and if the scroll exceeds the region it is
skipped entirely ("The entire scroll region will be scrolled out", `#L1327-L1331`). Rows are then
rendered by a pool of worker threads (`render_worker_thread`, `render.c#L2184`).

**Applies to Komando:**
- The *upper-bound* idea is the right shape for any "defer expensive work while output streams"
  rule (T1/T4): defer, but never longer than N ms.
- Pixel scrolling/`Present1` scroll rects: not reachable through GPUI.
- Worker threads for row layout: possible in Komando (rows are independent after the snapshot),
  but GPUI's text system calls (`shape_cluster` on glyph-cache misses) must stay on the UI thread;
  only the highlight pass is trivially parallel. Low priority: once T1–T3 land, per-frame row
  layout is a few hundred µs.

---

## 5. GPU cell grids (refterm, zutty, kitty) and what GPUI 0.3.12 allows

**refterm** (mirror of Casey Muratori's prototype; base
`https://github.com/deepankarsharma/refterm/blob/f7e612b6d63e03a3562202f19f9bfbc4c9d5598f/`):
the whole screen is a `StructuredBuffer<TerminalCell>` of 12-byte cells `{GlyphIndex, Foreground,
Background}` (`refterm.hlsl#L3-L8`). One compute dispatch over the window's pixels; each thread
finds its cell, looks up the glyph's atlas position via `GlyphMapping[Cell.GlyphIndex]`, samples
the atlas and lerps fg/bg (`refterm.hlsl#L34-L56`). The CPU writes W × H × 12 bytes per frame
with `Map(WRITE_DISCARD)` (`refterm.c#L349-L372`), then `Dispatch` + `Present`
(`refterm.c#L375-L383`), pacing on a frame-latency waitable object (`refterm.c#L140`,
`#L475-L502`). Per-frame CPU cost is a ~90 KB fill for 177 × 42, no per-glyph geometry. The full
refterm adds an LRU glyph cache keyed by a hash of the glyph run (`refterm-x11` port:
`refterm_glyph_cache.h`). **Caution:** m1el's `refterm-hash-break` (commit `ea68e5f`) shows the
original AES-based run hash could be inverted to build collisions. Any cache keyed only by a
64-bit hash *without comparing the key* (Komando's minimap `LineCache` keys by a custom 64-bit
FNV-style hash, `minimap.rs:546-560`) can in principle show the wrong cached content; for a
minimap that is harmless, for grid highlighting it would be a visible bug, so T3 should keep the
full key or verify on hit.

**zutty** (base `https://github.com/deepin-community/zutty/blob/3515b63e39edd2ab808baf8cbd9216cfdc7209df/`):
render thread separate from the vterm thread. `Renderer::update` swaps a lightweight `Frame`
(a pointer to the cell storage plus damage range, cursor, selection) into `nextFrame` under a
mutex and bumps `seqNo` (`src/renderer.cc#L36-L43`); the render thread copies the damaged cell
range into a mapped SSBO (`deltaCopyCells`, `src/frame.cc#L128-L135`, `#L267-L292`) and draws
with `glDispatchCompute(nCols, nRows, 1)` (`src/charvdev.cc#L595`). **If a newer frame arrived
while it was copying, it skips drawing the outdated one** and forces a full copy next time
(`renderer.cc#L93-L104`). The lock is held only for the `Frame` assignment, O(1).

**kitty**: instanced draw over a per-cell `GPUCell` buffer (§3).

**What GPUI 0.3.12 exposes on Windows.** The scene has fixed primitive kinds
(`gpui-pre-0.3.12/src/scene.rs#L222-L231`: shadow, quad, path, underline, mono/subpixel/polychrome
sprite, surface). `Window::paint_surface` exists but the DirectX renderer's `draw_surfaces` is an
empty stub (`gpui-pre-windows-0.3.12/src/directx_renderer.rs#L816-L821`), and there is no public
custom-shader or raw-D3D hook. Shaders are compiled into the crate (`shaders.hlsl`). So a
refterm/zutty-style cell buffer needs a fork of `gpui-pre-windows`: add a primitive kind (e.g.
`CellGrid { cells: Arc<[GpuCell]>, atlas tiles }`) to `gpui-pre`'s `Scene` and a pass to
`DirectXRenderer`, sharing GPUI's glyph atlas (`directx_atlas.rs`). Effort: large (two forked
crates, HLSL, atlas integration, DPI/subpixel, emoji). Gain: removes ~0.75–1.9 ms/frame of
`paint_glyph` + sprite sorting + upload, *off-lock*. Since GPUI already caps the frame rate and
this work does not hold the terminal lock, it does **not** speed up parsing much; it lowers UI CPU
and latency. **Not recommended now.** Cheaper GPUI-internal win if ever forked: skip the
`RenderGlyphParams` clone + two locked hash lookups per glyph by caching `(tile, raster_bounds)`
in Komando's own `CachedGlyph` (needs a public API to insert a pre-resolved sprite).

---

## 6. WezTerm

Base: `https://github.com/wezterm/wezterm/blob/e1eae81a56be8b5487dc77a29616afa568fb57b7/`

**Regex runs once per line lifetime.** `Line::scan_and_create_hyperlinks`
(`wezterm-surface/src/line/line.rs#L531-L557`) returns immediately if the line's
`SCANNED_IMPLICIT_HYPERLINKS` bit is set; any mutation clears that bit
(`line.rs#L489-L519`). Results are stored on the cells (hyperlink attribute). The GUI calls
`apply_hyperlinks` only for the visible stable-row range (`mux/src/pane.rs#L217-L234`). So during
a flood, only lines actually on screen at a render get scanned, once each; scrolling or
repainting an unchanged line costs a bit test.

**Shaping cached per line by sequence number.** `shape_hash_for_line`
(`wezterm-gui/src/termwindow/render/mod.rs#L892-L923`) stores a `CachedLineState { seqno,
shape_hash }` in the line's `appdata` (a `Weak` pointer stored *in the Line*,
`wezterm-surface/src/line/line.rs#L249-L275`); if the line's `current_seqno()` is unchanged the
hash is reused, and the shaped line comes from `line_to_ele_shape_cache` keyed by it
(`render/screen_line.rs#L120`, `#L914`). This is kitty's "cache travels with the line" again.

**Applies to Komando:** the cache-per-line idea is T3. alacritty's `Row` has no spare field, so
Komando would key by content (hash + verify) instead of by identity; or add a generation counter
to the vendored `Row` (a patch to `vendor/alacritty_terminal/src/grid/row.rs`, bumped in
`IndexMut`), which is invasive because rows are mutated through many paths.

---

## 7. Rio (rioterm + sugarloaf)

Base: `https://github.com/raphamorim/rio/blob/93983b78bbc9da340a92b657a063830a3ac6bf8e/`

Rio uses an alacritty-derived `Crosswords` grid. Per frame and panel it takes **one** terminal
lock, merges PTY + UI damage, calls `terminal.reset_damage()` and copies the visible rows with
`snapshot_visible` into a `Vec<Row<Square>>` that is **reused across frames** ("reuses the existing
`Row<Square>` allocations across frames ... only one terminal lock + one materialize pass per
frame per panel", `frontends/rioterm/src/context/renderable.rs#L70-L77`, `#L185-L210`). The
renderer then `mem::take`s those buffers and works without the lock
(`frontends/rioterm/src/screen/mod.rs#L4120-L4140`). Per-row style resolution is stored as values
next to the rows (`renderable.rs#L78-L80`); grid CPU bg/fg buffers stay resident, and
UI-only damage maps to `RowsToRebuild::None` (zero row work, `renderable.rs#L185-L194`).

This is the same design as Komando's `layout_grid` (damage → copy → unlock → lay out), with one
difference worth copying: **Rio copies raw rows (`Row<Square>` clone into a reused buffer) under
the lock and resolves styles separately**, whereas Komando resolves palette colours and selection
per cell while locked (Hotspot C, technique T7).

---

## 8. contour

Base: `https://github.com/contour-terminal/contour/blob/8aa5313325b66561b81120464aebb32a4dce6af2/`

**Double-buffered render buffer, filled at most once per refresh interval.**
`RenderDoubleBuffer` (`src/vtbackend/render/RenderBuffer.hpp#L452-L500`) holds two resolved
`RenderBuffer`s, an atomic back-buffer index, a state machine and `lastUpdate`. The terminal side
fills the back buffer only if `elapsed >= _refreshInterval` (`avoidRefresh`), then swaps; the
render thread reads the front buffer under a separate `readerLock`
(`src/vtbackend/screen/Terminal.cpp#L1096-L1145`). Rendering never touches the terminal state
lock, and filling is rate-limited to the display refresh.

**Read-ahead instead of blocking.** When the PTY thread wants the state lock and the render thread
holds it, it does `std::try_to_lock`; on failure it keeps reading what the child writes ("Read
what the child writes meanwhile instead of waiting idle ... the parse below covers all of it in
this one hold of the lock"), then parses the whole batch in one hold
(`Terminal.cpp#L724-L737`).

**Applies to Komando:** the read-ahead pattern maps directly onto `Output::feed`
(`src/session/ssh.rs:691-698`), which calls the blocking `term.lock()` from inside a tokio task:
while the UI holds the lock, that tokio worker thread is parked and cannot receive, decrypt or
window-adjust (shared with up to 7 other sessions, `ssh_threads`, `ssh.rs:252-254`). With
`FairMutex::try_lock_unfair` (`vendor/alacritty_terminal/src/sync.rs:46`) the task could append
to a pending buffer and keep draining the channel, parsing everything in one hold once the lock
is free (technique T8; overlaps researcher C's SSH/I/O scope).

---

## 9. Mosh (concept only, not cloned)

Mosh's State Synchronization Protocol sends the *latest* screen state and lets the client skip
intermediate states entirely (Winstein & Balakrishnan, "Mosh: An Interactive Remote Shell for
Mobile Clients", USENIX ATC 2012). The local equivalent is "draw only the newest grid, never queue
frames": Komando already has it (coalesced `Wakeup` flag + GPUI drawing at most once per vsync).
There is nothing further to adopt; the remaining question is only how *expensive* each frame is
and how long it locks the terminal.

---

## 10. Answers to the five questions

1. **Keeping the renderer from blocking the parser.** Three patterns: (a) *same thread, take
   turns* with bounded parse batches (kitty, foot); (b) *short lock + snapshot* (Rio, zutty,
   contour's double buffer, Komando's `layout_grid`); (c) *long lock* (Windows Terminal shapes
   under the console lock). Komando's grid path is already (b). Its **minimap is (c)**: it runs
   regex highlighting while holding the lock (Hotspot A). The fix is to make the minimap (b) as
   well, and to make the parser side non-blocking (contour's read-ahead).
2. **Pacing during floods.** Everyone caps at the display refresh rate or lower and draws the
   latest state (WT frame-latency waitable, foot frame callbacks, zutty skipping outdated frames,
   GPUI vsync invalidation). kitty (`repaint_delay` 10 ms, `input_delay` 3 ms) and contour
   (`refreshInterval`) additionally decouple from the monitor and rate-limit buffer filling; foot
   uses a lower/upper timer pair to coalesce bursts with a bounded delay. Komando could add a
   "busy" frame cap (T5), which helps most on high-refresh displays.
3. **GPU cell buffers.** refterm, zutty and kitty upload a per-cell array and let a shader place
   glyphs; WT uploads per-cell colour bitmaps but still builds glyph quads on the CPU. GPUI 0.3.12
   has no custom primitive, no custom shader hook, and `draw_surfaces` is a no-op on Windows, so
   this needs a fork of `gpui-pre` + `gpui-pre-windows`. The saving (~1–2 ms/frame of
   `paint_glyph`) is off-lock, so it would not raise parse throughput much. Not now.
4. **Highlighting/regex at speed.** Nobody runs regexes over every visible line every frame.
   WezTerm marks lines "scanned" and only re-scans mutated lines; kitty marks text once per dirty
   line and stores marks in the cells; WT runs URL patterns only 100 ms after output goes idle.
   Komando reruns the full 62-rule set on every dirty row each frame (fine for 42 rows if the DFA
   works), and on ~490 minimap lines per frame under the lock (not fine), and on non-ASCII rows
   the DFA bails to the PikeVM (T2).
5. **Minimap/per-frame extras.** No studied terminal has a minimap; the analogous extras (URL
   scanning, search highlighting, scrollbar marks) are always debounced, cached per line, or
   computed only on idle. Komando's minimap is the most expensive per-frame extra by a wide margin
   during floods (Hotspot A).


---

## 11. Techniques for Komando, in detail

Gains are estimates from code reading. Nothing here was measured. Each technique says how to
check it cheaply. Komando's profiler is enabled with `KOMANDO_PERF` (`src/perf.rs:2-18`) and
already has the probes `minimap layout`, `terminal grid prepaint`, `terminal grid paint`,
`command rail` and `search highlights`.

**Measure first (an hour, decides T1's priority):** run the standard 1,000,000-line benchmark
(`docs/benchmarks.md` says it runs "with its normal window, sidebar and minimap") with
`KOMANDO_PERF=1`, once with the minimap on and once with it off. The difference is an upper bound
on T1's gain. Read the `minimap layout` timings, because they are lock-held time.

### T1. Take the minimap off the terminal lock and stop re-highlighting it during floods

*Why it's fast elsewhere:* WT scans patterns only once output has been idle for 100 ms
(`ControlCore.cpp#L205-L226`); kitty and WezTerm run regexes once per changed line, never per
frame (§3, §6); zutty, Rio and contour never do heavy work under the lock.

*What to change* (`src/ui/minimap.rs`):

1. Under the lock, do only the per-line hash (already cheap) and copy the raw cells of the lines
   whose hash missed into a reused buffer. Release the lock, then resolve colours, run
   `line_runs` and insert into the cache. This moves the regex work out of the critical section.
2. While output streams, build missed minimap lines **without highlight rules** (raw fg colours
   only, a cheap pass) and mark them, or rebuild the strip at most every ~100–250 ms. "Streaming"
   can come from the view's `Activity` tracking (`terminal_view.rs:77-118`) or from "every grid
   row was damaged for the last N frames". When output has been idle for 100 ms, re-run the rules
   for the visible lines. This is WT's debounce plus foot's upper bound, so a steady trickle
   can't postpone it forever. A 2-px-per-line strip of a flood can't be read at 60 Hz anyway.
3. Replace "clear all at 8,192 lines" (`minimap.rs:529-532`) with the two-generation scheme the
   glyph cache already uses (`Generations`, `terminal_view.rs:~2530`), and reuse the
   `rects`/`cells`/`String` buffers between frames (`minimap.rs:539-541`, `612-613`).
4. Wrap the strip's `paint_quad` loop in `window.paint_layer(bounds, …)` (`minimap.rs:684-692`)
   so each run quad skips GPUI's `BoundsTree` insert (`gpui-pre-0.3.12/src/bounds_tree.rs:120`).
   The quads don't overlap, so a single layer is correct.

*Risk:* while output streams, the minimap shows uncoloured or slightly stale lines, which is a
visible but minor change. Highlight rules must still win once output is idle.
*Effort:* 0.5–1.5 days. *Gain:* removes an estimated 1–5 ms (ASCII) to 5–30 ms (non-ASCII) of
lock hold per frame. Under GUI load that could mean 10–40 % higher flood throughput with the
minimap visible, and a lower UI frame time. *Confidence:* medium. The mechanism is certain from
the code; the size is unmeasured.

### T2. Make highlight word boundaries ASCII so the lazy DFA never quits

*Mechanism (verified in source):* a Unicode `\b` makes regex-automata's lazy DFA quit on any
non-ASCII byte (`regex-automata-0.4.18/src/hybrid/dfa.rs:3220-3240`). The meta engine then
runs the PikeVM over the whole 62-pattern set (`meta/strategy.rs:870-906`) and over each
matching rule. 53 of the 62 bundled rules contain `\b`.

*What to change* (`src/highlight.rs`, `Highlighter::new`): before compiling each pattern, parse
it with `regex_syntax::ast::parse::Parser`, collect the spans of word-boundary assertions
(`AssertionKind::WordBoundary`, `NotWordBoundary` and the `\b{start}`/`\b{end}`/`\<`/`\>`
forms), and splice each into `(?-u:…)`, working from the end of the string. Use the rewritten
pattern for both the `RegexSet` and the per-rule `Regex`. Leave Unicode on for everything else
(literals such as `●`, case folding). Optionally also raise `RegexSetBuilder::dfa_size_limit`
(default 2 MB) and check that the 62-pattern overlapping set doesn't make the lazy DFA give up by
thrashing its cache, which is a second, less likely PikeVM fallback.

*Compatibility:* the rule format is Tabby's, and Tabby evaluates rules as JavaScript regexes,
where `\b` is ASCII-only. ASCII word boundaries are therefore *more* faithful to imported sets,
not less. The only difference from today is a word boundary next to a non-ASCII letter (`Überror`
would now match `\berror`). That is rare in logs and matches Tabby.

*Check:* extend `stress_highlight_repaints` (`src/highlight.rs:~638`) with rows containing `●`,
`é` or box drawing, and compare before and after. A unit test should assert that rewritten and
original patterns agree on ASCII rows.

*Effort:* about half a day with tests. *Gain:* grid highlighting on non-ASCII rows could drop
from about 6 ms to well under 1 ms per frame (the DFA is typically 10–100× faster than the
PikeVM), and the minimap benefits by the same factor. *Confidence:* medium-high on the cause,
medium on the size.

### T3. Cache highlight results per row content (kitty marks, WezTerm's scanned bit)

In `layout_row` (`terminal_view.rs:2026-2040`) the row's `text` is already built. Look the text
up in a bounded two-generation map `text → Rc<[Option<Style>]>`, storing the full text as the key
(or a strong hash *plus* a comparison on hit, see the refterm hash-collision note in §5). Do this
before calling `styles_for_background_into`. Invalidate the map when the highlighter `Arc` or
`dark` changes. The minimap could share the same map if it highlighted full rows and then
truncated them, which also keeps its colours consistent with the grid.

*Gain:* highlighting cost drops to about zero for rows that were seen before: scrolling the
Scrollback, slow `tail -f` (alacritty marks the whole screen damaged on every scroll), and
programs that redraw the same lines. There is **no gain in a full flood**, because every row is
new. *Effort:* half a day to a day. *Risk:* memory, since the map is bounded. *Confidence:* high
for those cases.

### T4. Deferred or budgeted grid highlighting (fallback if T2 is not enough)

Give highlighting a per-frame time budget (for example 1–2 ms). Rows not processed within it are
drawn plain, marked, and highlighted in later frames, or once output is idle (WT's
`outputIdle`). *Risk:* highlights visibly lag or flicker during floods. *Effort:* 1 day.
*Adopt only if* T2 plus T3 still leave highlighting above about 2 ms per frame.

### T5. Cap repaints while output streams (kitty `repaint_delay`, contour `refreshInterval`)

In `on_session_event(Wakeup)` (`terminal_view.rs:594-612`), if the last paint was less than
`BUSY_FRAME` (for example 16 ms) ago and the user hasn't typed within `ECHO`, schedule one
`cx.notify()` for `last_paint + BUSY_FRAME` instead of notifying now. Exempt the user's own input,
as kitty exempts small pending input. *Gain:* none on a 60 Hz display. On 120/144 Hz displays it
cuts UI CPU and lock time by 2–2.4× during floods. *Risk:* almost none, if typed echo is exempt.
*Effort:* about 2 hours. *Confidence:* medium. The test machine's refresh rate isn't recorded in
`docs/benchmarks.md`.

### T6. Reuse rows that scrolled (Windows Terminal's row rotation)

When damage is Full but the new viewport equals the old one shifted by k rows (compare per-row
content hashes from the previous frame), rotate `GridCache.rows` by k and lay out only the k new
rows. This needs `RowPaint` to store y relative to the row, with the origin added at paint time
(y is currently absolute, `terminal_view.rs:2022`). Mirrors `AtlasEngine.cpp#L156-L229`.
*Gain:* for slow tails, rows laid out per frame drop from 42 to k. That is about 0.2–0.4 ms of
CPU per frame, with no gain in floods. *Effort:* 1 day. *Confidence:* medium. *Priority:* low.

### T7. Copy raw cells under the lock and resolve them afterwards (Rio, zutty)

In `Snapshot::copy_rows` (`terminal_view.rs:2172-2214`), clone the dirty rows' `Cell` slices into
a reused `Vec<Cell>` (24 bytes per cell; the `Option<Arc>` extra makes it a clone rather than a
memcpy, but it is still cheap), plus the selection range and the colour table. Then unlock and do
palette resolution, INVERSE/HIDDEN handling, zero-width extraction and the per-cell
`selection.contains` outside the lock. *Gain:* the lock-held part of `layout_grid` drops from an
estimated 0.15–0.4 ms to about 0.05 ms per frame during floods. *Risk:* low, and the existing
damage logic is unchanged. *Effort:* half a day. *Confidence:* medium-high (small but certain).

### T8. Don't park a tokio worker on the terminal lock (contour's read-ahead)

`Output::feed` (`src/session/ssh.rs:691-698`) calls the blocking `term.lock()` in an async task.
Instead, use `try_lock_unfair()`. If it fails, append the bytes to a pending buffer (up to about
1–4 MB, then block) and go back to `channel.wait()`. Parse the pending bytes together with the
next chunk in one hold. Keep the order of `proxy.watch_output`, replies and synchronized-update
handling. *Gain:* the SSH pipeline keeps decrypting and the window stays open while the UI holds
the lock, and bigger parse batches amortise per-call overhead. After T1 the remaining UI hold is
small, so the gain is probably ≤5 %. *Risk:* medium, because ordering and sync-update deadlines
must be preserved. *Effort:* 1 day. *Confidence:* low-medium. This overlaps researcher C's scope.

### T9. Small per-frame allocations

These are the password-prompt probe `String` (`terminal_view.rs:2770`), `GridFont::new` on every
prepaint (`terminal_view.rs:2817`, cacheable by family, size and scale), and the minimap's
per-frame `Vec`s. *Effort:* about an hour. *Gain:* tens of µs per frame. *Confidence:* high.

### T10. GPU cell grid (refterm, zutty, kitty) through a GPUI fork

This is not reachable through GPUI 0.3.12's public API (§5). It would need forks of `gpui-pre`
and `gpui-pre-windows`, a new scene primitive, HLSL and atlas integration. *Gain:* about 1–2 ms
per frame of UI CPU off the lock, plus lower latency, but little parse throughput. *Effort:* 1–3
weeks, plus ongoing upkeep of the fork. *Not recommended* until T1–T5 are done and paint
measurably dominates the frame time.

---

## 12. Ranked list

| Rank | Technique | Effort | Expected gain | Confidence |
|---|---|---|---|---|
| 1 | **T1** Minimap off the lock; plain colours or rate-limited while output streams; LRU-style cache; `paint_layer` | 0.5–1.5 days | Removes an est. 1–30 ms of lock hold per frame; possibly +10–40 % flood throughput with the minimap visible | Medium (measure the minimap on/off first) |
| 2 | **T2** Rewrite `\b` to `(?-u:\b)` at compile time so the lazy DFA handles non-ASCII rows | ~0.5 day | Non-ASCII highlighting about 6 ms → <1 ms per frame (grid), same factor in the minimap | Medium-high (cause verified) |
| 3 | **T5** Repaint at most every ~16 ms while output streams (typing exempt) | ~2 h | 0 on 60 Hz; −50–60 % UI CPU and lock time at 120/144 Hz | Medium |
| 4 | **T7** Copy raw cells under the lock, resolve after unlocking | ~0.5 day | Locked `layout_grid` time drops from an est. 0.15–0.4 ms to ~0.05 ms per frame | Medium-high (small) |
| 5 | **T3** Per-row highlight cache keyed by text | 0.5–1 day | Highlighting ≈ 0 for unchanged rows (scrollback, slow tails); 0 in floods | High (for those cases) |
| 6 | **T8** Non-blocking `Output::feed` with read-ahead (coordinate with researcher C) | ~1 day | ≤5 % after T1; keeps the SSH pipeline draining | Low-medium |
| 7 | **T9** Remove small per-frame allocations | ~1 h | Tens of µs per frame | High (tiny) |
| 8 | **T6** Rotate cached rows on scroll | ~1 day | ~0.2–0.4 ms CPU per frame on slow tails; 0 in floods | Medium |
| 9 | **T4** Budgeted or deferred grid highlighting | ~1 day | Only needed if T2 and T3 fall short | Medium; UX risk |
| 10 | **T10** GPU cell grid via a GPUI fork | 1–3 weeks + upkeep | ~1–2 ms UI CPU per frame, off the lock | Medium; not recommended now |

Already in place and confirmed: one frame per vsync showing only the newest state (GPUI's vsync
thread plus the coalesced `Wakeup`), snapshot-then-unlock for the grid, damage-aware row layout,
buffer reuse, the glyph cache, and the grid painted inside one `paint_layer`.
