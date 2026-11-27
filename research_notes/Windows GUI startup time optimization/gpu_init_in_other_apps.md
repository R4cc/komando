# GPU and window initialization at startup in other GPU-rendered Windows apps

Scope: how Chromium/Chrome/Edge, Firefox, Windows Terminal, VS Code/Electron, Zed (GPUI Windows) and Alacritty order GPU device creation, window creation, ShowWindow and the first Present, and what they paint before the GPU frame is ready. Each technique is tagged **[changes what the user sees]** or **[no visible change]**.

Context for the reader: the target app (Rust GPUI fork, D3D11 + DirectComposition, `WS_EX_NOREDIRECTIONBITMAP`) creates its D3D11 device synchronously on the main thread before app init, shows the window via `SetWindowPlacement` during creation, and presents its first frame about 35 ms after the window becomes visible.

Research method note: GitHub sources were read directly with `gh` and raw source files (zed-industries/zed, microsoft/terminal, alacritty/alacritty, microsoft/vscode). Mozilla Bugzilla, the Electron docs and one news article were fetched over the web. I could not read Chromium source through the GitHub mirror in this session (the files came back empty or the greps found nothing), so the Chromium claims rest on a secondary article that quotes the CL.

---

## Q1. Which apps create the GPU device asynchronously and overlap it with app/UI init? Measured savings?

### Takeaway
None of the sources I found measures a saving from moving D3D device creation off the critical path. Windows Terminal's AtlasEngine is the clearest case of deferred device creation: it creates its D3D11 device lazily inside the render engine on first paint, not during app or window setup. Zed's measured Windows startup wins came from elsewhere, mostly a synchronous DirectWrite font-collection refresh (about 90 ms, or 31%, off the first frame). That makes "what else runs synchronously before the first frame" at least as important as the device itself.

### Cited Findings
- **Windows Terminal (AtlasEngine) creates the D3D11 device lazily.** In `AtlasEngine.r.cpp` the paint path checks `if (!_b) { _recreateBackend(); }`. `_recreateBackend()` calls `D3D11CreateDevice(...)` and then creates the swap chain (`CreateSwapChainForHwnd`, or a DComp surface handle via `DCompositionCreateSurfaceHandle`). Device creation is therefore tied to the first render, not done up front. — [AtlasEngine.r.cpp (microsoft/terminal main)](https://raw.githubusercontent.com/microsoft/terminal/main/src/renderer/atlas/AtlasEngine.r.cpp)
- **Windows Terminal startup breakdown (lhecker, 2023, Nsight trace):** "Of the remaining ~320ms launch cost of Windows Terminal, about 240ms (75%) are due to WinUI and XAML." He also wrote: "HWND creation costs 20ms and we need at least 2"; Monarch COM server setup costs "another 5-10ms"; the app extension catalog LPC takes "10ms for returning an empty list". An earlier 80 ms (20%) workaround for loading nearby fonts (#11648) was "Fixed". — [microsoft/terminal#5907 comments](https://github.com/microsoft/terminal/issues/5907)
- **A community trace on WT (ssylvan, 2021)** suggested the render thread "could be initialized at the very beginning of the process launch rather than waiting for XAML", and that "most things [be] initialized (esp. DirectX, DirectWrite, etc.), and then resize at the end and wire it up to XAML." This was a suggestion, not a landed change. — [microsoft/terminal#5907](https://github.com/microsoft/terminal/issues/5907)
- **The WT team's ideas for skipping font work at startup:** caching the initial window size and font cell metrics in `state.json` (zadjii-msft), or a compiled-in cache of cell sizes for popular fonts (DHowett: "That will stop us from creating a font collection on every startup"). — [microsoft/terminal#5907](https://github.com/microsoft/terminal/issues/5907)
- **WT jump-list and cross-process COM startup costs were fixed** in #13692 (version 1.16), according to lhecker's WPR trace comment. — [microsoft/terminal#5907](https://github.com/microsoft/terminal/issues/5907)
- **Zed / GPUI Windows, PR #66001 (merged 2026-11-24):** `DirectWriteTextSystem::new` called `GetSystemFontCollection` with `checkForUpdates=true`, "forcing a synchronous font collection update before the first window can appear." Changing it to `false` gave:

  | Measurement | Before | After |
  |---|---:|---:|
  | DirectWrite font collection | 72.5 ms | 1.34 ms |
  | GPUI initialization | 143.8 ms | 57.8 ms |
  | First painted frame | 285.3 ms | 195.6 ms |

  These were measured on a Ryzen 5 PRO 5650U with 137 font families, 50 fresh processes per variant, using GPUI 0.3.9 standalone, "not verified against a full current Zed build". The trade-off: after fonts are installed or removed, the font collection can be briefly stale. — [zed-industries/zed#66001](https://github.com/zed-industries/zed/pull/66001); repro: [JeremyEspresso/gpui-windows-startup-repro](https://github.com/JeremyEspresso/gpui-windows-startup-repro)
- **Zed's startup log order on Windows** (issue #66163) shows `gpui_windows::direct_write` "Use Segoe UI as UI font", then `gpui_windows::directx_devices` "Using GPU: …", then "Created device with Direct3D 11.1 feature level." These all happen in the same millisecond bucket early in startup. That fits synchronous device creation during platform init. (Inference from the log; I did not read the code.) — [zed-industries/zed#66163](https://github.com/zed-industries/zed/issues/66163)
- **Zed's GPUI wgpu backend (Linux), PR #66002:** configuring the surface once at creation, instead of once during adapter selection (64x64) and again in the renderer, cut exec-to-first-frame from a median of 946 ms to 528 ms on NVIDIA/X11. The removed configure took 218–534 ms. It had no measurable effect on AMD/Mesa (2–5 ms per configure). This is Linux-only, but it shows that redundant swapchain or surface creation at startup can cost a lot. — [zed-industries/zed#66002](https://github.com/zed-industries/zed/pull/66002)
- **Zed's overall Windows startup** is tracked in the open issue #49442. User timings from Enter to the first visible window frame: Zed 2.07–5.67 s, Sublime Text 0.553 s, a tiny Rust app 0.159 s. Later comments trace multi-second (8–15 s) delays on corporate laptops to EDR or antivirus scanning before Zed's first log line. — [zed-industries/zed#49442](https://github.com/zed-industries/zed/issues/49442), [#66163](https://github.com/zed-industries/zed/issues/66163), [#29731](https://github.com/zed-industries/zed/issues/29731)
- **Alacritty does GPU setup synchronously before showing the window:** create the window hidden, create the GL surface and context, create the renderer, preload common glyphs, then clear, swap and show (see Q2). — [alacritty display/mod.rs](https://raw.githubusercontent.com/alacritty/alacritty/master/alacritty/src/display/mod.rs)

### Inferences
- Windows Terminal ties device creation to first paint, and it renders on a renderer thread separate from the XAML UI thread (ssylvan's trace refers to "render thread initialization"). So its D3D device creation probably already overlaps with XAML layout. I have not verified which thread runs `_recreateBackend` at startup in current builds.
- For the target app, the closest precedent is Zed #66001: the measured win came from removing synchronous DirectWrite work that blocked the first window, not from threading D3D. Creating the D3D11 device (and the DirectWrite factory and font collection) on a worker thread while the main thread does app init is technically sound. D3D11 devices are free-threaded for creation, and the immediate context can be handed over afterwards. But I found no published measurement of this in any of the surveyed apps. **[no visible change]** if the main thread just waits on the device before the first draw.
- Chromium's architecture already puts GPU initialization in a separate GPU process that runs in parallel with browser-process UI startup (general architectural knowledge; no primary source fetched in this session).

### Gaps
- I found no published numbers for D3D11CreateDevice / DXGI factory / DComp device creation time on a typical machine, and no numbers for moving them to a background thread, in any of these projects.
- Chromium: I could not fetch the primary source (crbug, the CL, or `content/browser/browser_main_loop.cc`) showing when the GPU process is launched relative to the first browser window, so I can't quote its early-GPU-channel logic.
- Firefox WebRender: I found nothing specific on whether the GPU process or compositor is started in parallel with skeleton UI or XUL loading, or on measured gains from doing so.

---

## Q2. Who cloaks the window (DWMWA_CLOAK) or delays ShowWindow until the first frame, and why? Trade-offs?

### Takeaway
Hiding the window until the first real frame is ready is common: Windows Terminal, Alacritty, Firefox (cloak and clear), Chromium/Edge (cloak plus a GDI fill), Electron's `ready-to-show` pattern, and an open Zed PR. The trade-offs that come up repeatedly are:
1. The window *seems* to appear later even though time-to-usable is unchanged.
2. Show-then-uncloak breaks the OS window-open animation.
3. Placement and DPI bugs when the window is positioned while hidden.

Electron's docs warn that waiting for the first frame "could be emitted too late, making the app feel slow". For complex apps they recommend showing immediately with a matching `backgroundColor`.

### Cited Findings
**Windows Terminal: hide until init is done, then show** **[changes what the user sees: no frame-only/transparent window, window appears later]**
- PR #12979 (merged 2022-05-06, zadjii-msft): the window was "initially just a frame with a transparent content area … a few frames where the frame is visible, before the page paints". The fix waits for the UI thread to finish initial init (`CoreDispatcherPriority::Low`, so tabs and panes exist), and the window layer "[is] gonna ~~cloak~~ just not show the window till this event is fired." The author's own trade-off: "**This will result in the window seemingly not loading as fast**, but it will actually take exactly the same amount of time before it's usable." He also tried "drawing a solid BG color before the initialization is finished. However, there are still a few frames after the frame is displayed before the XAML content first draws, so that didn't actually resolve any issues." Note that the final mechanism was delaying ShowWindow, not DWMWA_CLOAK. — [microsoft/terminal#12979](https://github.com/microsoft/terminal/pull/12979)
- Reverted in #13098 (2022-05-13) because "persisted windows on a secondary monitor [would] shrink a little each time." — [microsoft/terminal#13098](https://github.com/microsoft/terminal/pull/13098)
- Relanded in #13811 (merged 2023-04-06). The root cause of the shrinking was that window creation had been moved into `_AppInitializedHandler`. The fix creates the window in `_HandleCreateWindow`, "_hidden_, and then should only _show_ it in `_AppInitializedHandler`." This is a lesson about window placement and DPI when deferring the show. — [microsoft/terminal#13811](https://github.com/microsoft/terminal/pull/13811)
- WT also switched to a dark title bar when a dark theme is requested, to reduce flash (#14536). — [microsoft/terminal#14536](https://github.com/microsoft/terminal/pull/14536)

**Alacritty: hidden window, clear-and-present the background color, then show** **[minimal visible change: first visible frame is the terminal background color]**
- The window is created with `.with_visible(false)` (and `.with_transparent(true)`). — [alacritty display/window.rs](https://raw.githubusercontent.com/alacritty/alacritty/master/alacritty/src/display/window.rs)
- Then it creates the GL surface, makes the context current, creates the renderer, preloads common glyphs, calls `renderer.clear(background_color, …)`, calls `surface.swap_buffers(...)` (skipped on Wayland, where "the window isn't visible until you actually draw something into it and commit those changes"), and finally `window.set_visible(true)`. The first composited content is a presented frame in the user's background color. — [alacritty display/mod.rs](https://raw.githubusercontent.com/alacritty/alacritty/master/alacritty/src/display/mod.rs)

**Firefox: cloak, clear to the theme color, then uncloak (Firefox 129)** **[changes what the user sees: no white flash, but no window-open animation]**
- Bug 1824053 found the flash "appears to be a (mis)feature of DWM itself". DWM composites a new window as soon as it is shown, before the app can paint. Fix (Firefox 129): remove the per-window Win32 `SetBackgroundColor()` (which only set the class brush), make the window-class background dark grey (on its own this "does not quite" fix it), then "cloak and clear new windows before showing them", because "DWM will draw our window's surface before we get a chance to clear it." The same approach was applied to the skeleton UI window. — [Bugzilla 1824053](https://bugzilla.mozilla.org/show_bug.cgi?id=1824053)
- Regression, Bug 1906358 (open, backlog): "Show-then-uncloak breaks the window open animation." Ray Kraesig (author of the fix) concluded "simply skipping the window-open animation seems the least of all available evils". He rejected a cloak/show/clear-to-black/hide sequence because of its extra latency. Other follow-ups: modal and sub-windows lost their animations (1934440), border glitches with the Classic theme (1950145), and a white rectangle at the bottom at startup with the title bar enabled (1903452, fixed). — [Bugzilla 1906358](https://bugzilla.mozilla.org/show_bug.cgi?id=1906358), [Bugzilla 1824053](https://bugzilla.mozilla.org/show_bug.cgi?id=1824053)
- Earlier bug 1647161 added handling for cloaking events, describing cloaking as a Windows 8+ DWM and shell feature. — [Bugzilla 1647161](https://bugzilla.mozilla.org/show_bug.cgi?id=1647161) (search-result summary only)

**Chromium / Chrome / Edge: cloak, GDI-fill a dark color, uncloak (late 2024 / early 2025)** **[changes what the user sees: dark instead of white first frame]**
- CL [chromium-review 6092335](https://chromium-review.googlesource.com/c/chromium/src/+/6092335) in `ui/views/win/hwnd_message_handler.cc`, attributed to Microsoft's Chris Davis. Newly created windows are cloaked (`DWMWA_CLOAK` TRUE), the client area is filled with a dark color using GDI while cloaked, and then the window is uncloaked. Commit text: "When a window is first created, DWM will use a default white background colour for the window." Reviewers worried about a race where the GPU process finishes drawing page content just as the background is filled, so the dark fill overwrites it. Microsoft's tests did not reproduce it. The article calls the change a workaround and mentions `WS_EX_NOREDIRECTIONBITMAP` and `DWM_SYSTEMBACKDROP_TYPE` as possible future alternatives. — [Windows Latest, 2025-01-08](https://www.windowslatest.com/2025/01/08/microsoft-finally-fixing-chrome-edges-white-flash-in-dark-mode-on-windows-11-windows-10/) (secondary source; I did not read the CL directly)

**Electron / VS Code** **[the Electron pattern changes what the user sees: later appearance; VS Code shows early with a cached color]**
- Electron docs: with `show: false`, "the `ready-to-show` event will be emitted when the renderer process has rendered the page for the first time … Showing the window after this event will have no visual flash." But "For a complex app, the `ready-to-show` event could be emitted too late, making the app feel slow. In this case, it is recommended to show the window immediately, and use a `backgroundColor` close to your app's background." `ready-to-show` "will never fire if you use `paintWhenInitiallyHidden: false`". — [Electron BrowserWindow docs](https://www.electronjs.org/docs/latest/api/browser-window)
- VS Code takes the "show early with a matching color" route. Window options set `backgroundColor: themeMainService.getBackgroundColor()`, a cached color from the last theme. `IThemeMainService` also has `saveWindowSplash` / `getWindowSplash` (`IPartsSplash`), which stores the last window's part layout and colors for a splash. The title-bar overlay color comes from `getWindowSplash(undefined)?.colorInfo.titleBarBackground`. — [vscode windows.ts](https://raw.githubusercontent.com/microsoft/vscode/main/src/vs/platform/windows/electron-main/windows.ts), [vscode themeMainService.ts](https://raw.githubusercontent.com/microsoft/vscode/main/src/vs/platform/theme/electron-main/themeMainService.ts)
- VS Code delays only `show()` around maximize and fullscreen: "to reduce flicker from the default window size to maximize or fullscreen, we only show after" calling `maximize()`, because `maximize()` "may or may not show the window, depends on the platform: currently on Windows and Linux will show the window as active." — [vscode windowImpl.ts](https://raw.githubusercontent.com/microsoft/vscode/main/src/vs/platform/windows/electron-main/windowImpl.ts)

**Zed (GPUI Windows): open PR to show only after the first Present** **[changes what the user sees only by removing the empty-window state; keeps the open animation]**
- PR #63134 (open since 2026-08-24): on Windows, "windows created with `show: true` are no longer shown at creation. Their placement is applied while they are hidden, the first frame is presented, and the window is shown from `PlatformWindow::draw`. This keeps the OS window-open animation, lays the first frame out at the final position, size and DPI (a freshly created window otherwise keeps its default size), and never shows a frame without content. An activation requested before the window is shown is performed right after showing it." It also applies the DWM accent/backdrop at creation, because "DWM composites asynchronously, so a window that becomes visible before its accent is applied can be composited without it." "Windows created with `show: false` (the main window) are unaffected." — [zed-industries/zed#63134](https://github.com/zed-industries/zed/pull/63134)

### Inferences
- Ordering present-then-ShowWindow (WT, Alacritty, Zed #63134) keeps the OS open animation. Ordering show-then-uncloak (Firefox, Chromium) loses it, as Firefox's Bug 1906358 shows. For an app that wants to look exactly the same as today but without an empty frame, the Zed #63134 / Alacritty pattern fits better than DWMWA_CLOAK: place hidden at the final DPI, Present the first frame, then ShowWindow.
- With `WS_EX_NOREDIRECTIONBITMAP` there is no redirection surface (no GDI content), so between ShowWindow and the first DComp commit the client area has nothing to composite. Firefox's rkraesig notes "Windows without a compositor surface become invisible." In the target app, the ~35 ms between "visible" and first frame is probably a visible frame/shadow (or nothing) over a see-through client area rather than a white flash. Showing after the first Present removes that state and changes nothing the user would deliberately notice. It also means a GDI class brush or GDI fill (the Chromium and Firefox tricks) *cannot* work as a placeholder with this flag.
- On perceived startup, moving ShowWindow after the first Present makes the "window appears" moment arrive up to ~35 ms later. The total time to a fully painted window does not get longer, and gets shorter if layout happens once at the final size and DPI (Zed #63134's point). WT's author made the same point: it looks slower but "take[s] exactly the same amount of time before it's usable."

### Gaps
- None of these sources measures time-to-first-frame before and after the delayed-show change, for WT, Alacritty or Zed #63134. Only qualitative descriptions exist.
- I didn't verify whether Chromium's cloak CL (6092335) finally landed as described, or whether it was later replaced by a `WS_EX_NOREDIRECTIONBITMAP` / backdrop approach.
- I didn't re-fetch the Microsoft Learn `DWMWINDOWATTRIBUTE` / DirectComposition pages in this session. Get the exact `DWMWA_CLOAK` semantics (the window stays composed by DWM while hidden) from [DWMWINDOWATTRIBUTE (Microsoft Learn)](https://learn.microsoft.com/en-us/windows/win32/api/dwmapi/ne-dwmapi-dwmwindowattribute) before relying on them.

---

## Q3. Zed's Windows GPUI: issues and PRs on startup time, DirectX/DirectWrite init and window flash

### Takeaway
Zed's slow Windows startup is a known open issue (#49442). The concrete fixes so far are removing a synchronous DirectWrite font-collection refresh (#66001, about 90 ms off the first frame) and an open PR to show windows only after their first frame is presented (#63134). Much of the multi-second startup users report happens before Zed's own code runs (EDR/AV) or comes from app-level work, not DirectX.

### Cited Findings
- #66001 (merged 2026-11-24): `GetSystemFontCollection(checkForUpdates=false)`. First painted frame went from 285.3 to 195.6 ms, and GPUI init from 143.8 to 57.8 ms. Missing fonts still trigger the existing forced refresh in `select_and_cache_font`. — [zed#66001](https://github.com/zed-industries/zed/pull/66001)
- #63134 (open): show from `PlatformWindow::draw` after the first Present, and apply the backdrop before showing. — [zed#63134](https://github.com/zed-industries/zed/pull/63134)
- #66002 (merged, Linux wgpu): one surface configure instead of two; median first frame from 946 to 528 ms on NVIDIA/X11. — [zed#66002](https://github.com/zed-industries/zed/pull/66002)
- #49442 (open): "Slow startup on Windows". Users time Enter-to-first-frame at 2–5.7 s, against 0.55 s for Sublime. The window is "slightly off on start and then jumps to the right place" (a placement jump after showing). Comments point to EDR on corporate laptops and to auto-update work at startup. — [zed#49442](https://github.com/zed-industries/zed/issues/49442)
- #66163 (closed as duplicate): 14.5 s before Zed's first log line, attributed to EDR. — [zed#66163](https://github.com/zed-industries/zed/issues/66163)
- #29731 (closed): a "startup slower" report that turned out to be Scoop packaging; later comments add the timing tables. — [zed#29731](https://github.com/zed-industries/zed/issues/29731)
- Upstream `crates/gpui_windows/src/window.rs` references `DWMWA_CLOAKED`, the *query* attribute used to detect cloaked windows, not `DWMWA_CLOAK`. — `gh search code --repo zed-industries/zed DWMWA_CLOAK` (I didn't read the usage context)

### Inferences
- The "position jumps after start" symptom in #49442 is the same class of problem #63134 fixes: the window is shown before its final placement and DPI are applied. The target app's `SetWindowPlacement`-inside-creation pattern may share this risk if layout happens at a provisional size.

### Gaps
- I found no Zed PR that moves `DirectXDevices` creation to a background thread or overlaps it with app init, and no measurement of Zed's D3D device creation cost.

---

## Q4. Windows Terminal startup work and measured numbers

### Takeaway
WT's startup cost is dominated by WinUI/XAML: about 240 of about 320 ms (75%) in lhecker's 2023 trace. The team cut font loading (about 80 ms), jump-list and COM work, and other overhead. They hid the window until UI init completes (delaying ShowWindow rather than cloaking). AtlasEngine creates its D3D device lazily at first render.

### Cited Findings
- About 320 ms total launch cost, about 240 ms (75%) in WinUI/XAML. An 80 ms nearby-fonts workaround was fixed. HWND creation is about 20 ms each (at least 2 needed), and COM Monarch setup 5–10 ms. — [terminal#5907](https://github.com/microsoft/terminal/issues/5907)
- "Avoid loading nearby fonts unless necessary" (#15239) is the commit behind the 80 ms font fix. — [commit mirror](https://source-hub.etos-pro.ru/microsoft/terminal/commits/commit/48eee4d75aef934e97f7831cbcc3335de35c5279/src) (search snippet only); see [terminal#5907](https://github.com/microsoft/terminal/issues/5907) for the "Fixed" note
- A 2022 trace by zadjii-msft: `TryLoadSettings` 44/284 of the AppHost ctor; XAML resources 62/284; command palette about 30/284; opening the first tab about 53 (color picker flyout 15 of that). Looking for fragments was "wholly 50% of the settings load cost". — [terminal#5907](https://github.com/microsoft/terminal/issues/5907)
- Jump list and cross-process COM costs were fixed in #13692 (1.16). — [terminal#5907](https://github.com/microsoft/terminal/issues/5907)
- Hide until initialized: #12979, then the revert #13098, then the reland #13811. — [#12979](https://github.com/microsoft/terminal/pull/12979), [#13098](https://github.com/microsoft/terminal/pull/13098), [#13811](https://github.com/microsoft/terminal/pull/13811)
- #20536 "ConPTY: Do not wait for DA1 on startup" (lhecker, merged) removes a startup wait in ConPTY. — `gh search prs --repo microsoft/terminal --author lhecker startup`
- AtlasEngine lazy `D3D11CreateDevice` in `_recreateBackend()` on first paint. — [AtlasEngine.r.cpp](https://raw.githubusercontent.com/microsoft/terminal/main/src/renderer/atlas/AtlasEngine.r.cpp)

### Inferences
- WT's experience suggests the GPU device is not the dominant cost for a native D3D11 renderer. UI framework init, fonts, settings and process/COM plumbing dominate. For a GPUI app with no XAML, D3D11 device creation plus DirectWrite could be a larger share. Profile before assuming.

### Gaps
- I found no WT number for AtlasEngine's own init (device plus swap chain plus glyph atlas) or for the time from first Present to visible.

---

## Q5. Chromium early window and background color; Firefox early blank window / skeleton UI and measured wins

### Takeaway
Firefox's skeleton UI is the most aggressive "show early" technique: a GDI-painted placeholder window before `xul.dll` loads, later handed over to the real `nsWindow`. It improves *perceived* startup, by up to "15 seconds sooner" on slow cold starts, but it **adds a visible placeholder state** and has caused handoff and flash bugs. Chromium paints a dark GDI fill under a cloak and relies on the GPU process for content. Neither approach fits a goal of "no new placeholder states".

### Cited Findings
- **Firefox skeleton UI** **[changes what the user sees: placeholder UI]**: Bug 1656526 (fixed in Firefox 82, behind a registry value). Motivation: on reference hardware, 19 s of cold start before any visual sign, then a blank window, about 3 s to browser chrome and about 8 s to about:home. The proposal was to create the window before prefetching and loading `xul.dll`, which "can take a very long time." It is painted with GDI. User32 and GDI32 had to be lazy-loaded because of the DLL blocklist. — [Bugzilla 1656526](https://bugzilla.mozilla.org/show_bug.cgi?id=1656526)
- Enabled on Nightly (Windows 10) on 2021-01-05. Visual feedback can come "as much as 15 seconds sooner than normal" on some systems. The HWND is "handed off to nsWindow" when the first real browser window is needed, and the code tries to make its state match a normally created window even though it has already been shown. Size and position are read from `HKCU\SOFTWARE\Mozilla\Firefox\PreXULSkeletonUISettings`, saved from the last run. Feedback was qualitative ("very perceivable improvement", Mike Conley), with no telemetry numbers posted. One known handoff bug: a missed initial focus event meant no caret (bug 1665456). — [mozilla.dev.platform post](https://groups.google.com/g/mozilla.dev.platform/c/Vr9RTMMbaEI)
- Users report the blank or skeleton window as an unwanted extra state, for example "A blank window briefly appears before the Firefox window comes up". — [SUMO 1348229](https://support.mozilla.org/en-US/questions/1348229)
- Firefox's `WS_EX_NOREDIRECTIONBITMAP` analysis (Bug 1906358, rkraesig): the flag can't be toggled on an existing HWND, which blocks falling back mid-session to non-accelerated rendering. Windows without a compositor surface are invisible, so compositor availability must be checked before the first window. The GDI skeleton UI would not render. — [Bugzilla 1906358](https://bugzilla.mozilla.org/show_bug.cgi?id=1906358)
- **Chromium**: cloak plus a dark GDI fill plus uncloak (CL 6092335), with the GPU-process race concern noted above. — [Windows Latest](https://www.windowslatest.com/2025/01/08/microsoft-finally-fixing-chrome-edges-white-flash-in-dark-mode-on-windows-11-windows-10/)

### Inferences
- The skeleton-UI approach and the Electron/VS Code "show immediately with backgroundColor" approach both trade a new visible intermediate state for earlier feedback. They fall outside the target constraint. The parts that fit the constraint are:
  - (a) overlap or defer the expensive synchronous init (Zed's font fix, WT's font, jump-list and COM fixes, possibly background D3D/DWrite creation);
  - (b) present the first frame before showing (Alacritty, WT, Zed #63134), so the 35 ms visible-but-empty window disappears without adding any state.
- Firefox's placement-from-registry trick (stored last size and position so the first window can be created at its final rect without loading config) is a no-visible-change idea worth noting. WT discussed the same for font cell metrics.

### Gaps
- I found no Chromium primary source on GPU process launch timing relative to the browser window's first show, or on "initial background color" handling beyond the 2025 cloak CL.
- I found no telemetry-based measurement of the Firefox skeleton UI's effect on perceived or actual startup. Only anecdotal figures exist (the 19 s baseline and "up to 15 seconds sooner").
- I didn't research WezTerm or Warp Windows startup specifics (gh repo search for wez/wezterm failed). Warp's Windows client is closed source.
