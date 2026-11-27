# gpui-pre-windows 0.3.12, patched for Komando

This is gpui-pre-windows 0.3.12 from crates.io (Zed's `gpui_windows` at `d2a4fb6`), used
through `[patch.crates-io]` in Komando's `Cargo.toml`. `komando.patch` holds the changes as a
diff against the published crate. Each change is marked `Komando:` in the source.

The first three are about how the window behaves while Windows moves, resizes, minimizes or
hides it. The last two make startup shorter. None changes what a frame looks like.

## 1. A hidden or minimized window isn't invalidated every vblank

The vsync thread (`begin_vsync_thread` in `platform.rs`) called
`RedrawWindow(RDW_INVALIDATE)` on every window after each vblank, including hidden and
minimized ones. Those windows draw nothing (the frame callback is parked while minimized), but
each `WM_PAINT` still went through `DefWindowProc`, at display rate.
[zed-industries/zed#66221](https://github.com/zed-industries/zed/issues/66221) measured ~3% of a
core for a minimized window, and ~0.7% with this change. Komando hides into the tray by default,
so this was the cost of every hour it sat there.

Now the loop skips a window that is hidden or iconic. Nothing is lost on the way back:

- Showing it sends `WM_SHOWWINDOW`, which already draws.
- Restoring it sends `WM_SIZE`, and the window class has `CS_HREDRAW | CS_VREDRAW`, so the
  whole window is invalidated. The next vblank invalidates it too.

## 2. Dragging the window doesn't redraw it

GPUI answers every `WM_MOVE` with `Window::bounds_changed`, which calls `refresh()`: a full
layout and paint of every view, bypassing view caches, for each step of a drag, though nothing
in the window changed. [Cyenoch/solid-gpui#2](https://github.com/Cyenoch/solid-gpui/issues/2)
measured ~4 ms of CPU per `WM_MOVE` for an application tree, and that work runs on the thread
that is also running the drag.

Between `WM_ENTERSIZEMOVE` and `WM_EXITSIZEMOVE`, `handle_move_msg` now records that the window
moved and reports it once when the loop ends. Outside the loop (Win+arrow keys, a program
calling `SetWindowPos`), moves are reported as before. Nothing else depends on the per-step
report:

- Crossing onto another monitor still goes through `set_display` at once, and a DPI change
  through `WM_DPICHANGED`.
- `Window::bounds()` reads the window's position live, and `state.origin` is still updated
  on every `WM_MOVE`.
- When a window moves under a still cursor, Windows sends a synthetic `WM_MOUSEMOVE`, which
  updates the pointer position.

Komando and gpui-component don't observe window bounds.

## 3. Resizing by an edge presents each new size right away

GPUI resizes the swap chain on `WM_SIZE` and draws on the next `WM_PAINT` or on the move/size
loop's timer. Windows delivers both only when no other input is waiting. While an edge is being
dragged, the mouse keeps input waiting, so DWM shows the old frame stretched to the new size
(DirectComposition swap chains use `DXGI_SCALING_STRETCH`) and the content trails the edge.
Microsoft's advice is to render a complete frame right after `ResizeBuffers`
([Q&A](https://learn.microsoft.com/en-us/answers/a/12751382)).

Inside the move/size loop, `handle_size_msg` now draws and presents straight after resizing.
The loop runs from `DefWindowProc`, outside any GPUI update, so the draw can borrow the app.
The draw guard still defers a draw that would nest. Windows coalesces mouse moves, so a slow
frame means fewer resize steps, not a backlog. The loop's timer still runs; after a resize
frame it finds the window clean and does nothing.

Outside the loop (maximize, Snap, restore) `WM_SIZE` is followed straight away by the
`WM_PAINT` that `CS_HREDRAW | CS_VREDRAW` causes, so the patch leaves it alone.

[zed-industries/zed#65729](https://github.com/zed-industries/zed/pull/65729), which is open,
goes further: it draws on `WM_SIZING`, before the window takes the new size. Revisit this
change if that PR is merged.

## 4. The DirectX devices are created while the app starts

Creating the Direct3D 11 device loads and initialises the GPU driver: about 120 ms on an
RTX 5070 Ti, the longest step of startup. GPUI created it on the main thread in `run`, after
DirectWrite, and only then let the app finish launching, so nothing else could happen meanwhile.
A second device costs about as much as the first, so creating a throwaway device earlier (what
Komando used to do) only pre-paid part of it.

`WindowsPlatform::new` now starts `DirectXDevices::new` on a thread of its own before anything
else. `connect_initially` decides between windowed and headless as before, but leaves the
devices for the first window: `open_window` collects them (`attach_pending_gpu`), and starts
vsync then. Until that moment the main thread sets up DirectWrite and the platform window, and
the app runs its launch callback: Komando registers its fonts and reads its settings, Hosts and
Highlight Rules there. Measured with Komando's startup trace, its first frame came 62 ms sooner
(237 → 175 ms, median of 10 warm launches).

- The device can be used from any thread; only its immediate context is single-threaded, and
  after the hand-over only the main thread uses it.
- Starting headless, or switching to headless before the first window, drops the thread's
  devices. A later switch to windowed creates new ones, as before.
- If DirectX can't be initialised, the app still stops with `failed to initialize DirectX`, now
  when its first window opens.

## 5. The system font collection isn't refreshed at startup

`DirectWriteTextSystem::new` asked for the system font collection with `checkForUpdates` set,
which makes DirectWrite check for font changes before answering: about 50 ms, on the main
thread. Without it, DirectWrite answers from the font cache service, which follows font
installs on its own. A font lookup that misses still fetches the collection again with the
check (`select_and_cache_font`), so a font installed moments before Komando started is found
when it's used. It can be missing from the font list in Settings until Komando restarts.
[zed-industries/zed#66001](https://github.com/zed-industries/zed/pull/66001) made the same
change upstream.

## Checking it

These changes only build on Windows. The crate and the Komando library type-check for
`x86_64-pc-windows-msvc`. Window behavior needs a Windows machine:

- **Tray:** with minimize-to-tray on, minimize, then watch Komando's CPU in Task Manager. It
  should stay near zero instead of a steady few percent. Restore from the tray icon; the window
  should show current terminal output at once.
- **Drag:** drag the window around with a busy terminal and the minimap open. The drag should
  stay smooth, and Komando's CPU shouldn't climb. Drag onto a monitor with another scale; the
  window should redraw at that scale.
- **Resize:** drag the right or bottom edge quickly. The content should follow the edge
  instead of stretching and catching up. Drag the left or top edge, and drag across monitors.
- **Snap:** Win+arrows, dragging to a corner, and maximize/restore should each draw the new
  size straight away.
- **Startup:** run with `KOMANDO_STARTUP_TRACE=1` and `KOMANDO_LOG=info` and read the log
  (`docs/development.md`, Profiling). "Using GPU" should be logged before `platform ready`, and
  `highlights compiled` should come before `window created`.

To update gpui: copy the new gpui-pre-windows release here and apply `komando.patch`, fixing
anything that conflicts, or drop changes upstream has made unnecessary. Then regenerate the
patch with `diff -ruN` against the published crate.
