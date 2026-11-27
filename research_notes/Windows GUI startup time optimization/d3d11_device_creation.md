# D3D11 / DXGI device creation cost at startup on Windows 10/11, and how to reduce or hide it

Source quality note: Microsoft Learn pages are primary and authoritative for API semantics. Almost all **measured timings** found come from small open-source projects' GitHub issues and PRs (nene-nib, Lodestone, expo/exact, amigo-blinzel), dated 2025–2026. Each was measured on a single machine with few runs (about 5). They are consistent with each other, but none is a vendor or Microsoft measurement. I found no published NVIDIA, AMD, Intel or Microsoft timing of D3D11CreateDevice, and no public ETW breakdown of what the UMD does inside it.

## 1. What happens inside D3D11CreateDevice / CreateDXGIFactory / EnumAdapters, and what does it cost?

### Takeaway
On real hardware, almost all of the cost is GPU user-mode driver (UMD) initialization inside D3D11CreateDevice. Measurements: about 150–160 ms on NVIDIA (RTX 3090, RTX 5070), about 55–150 ms on AMD (Radeon 680M iGPU, a desktop AMD GPU), against 3.6–9.5 ms for WARP. On NVIDIA the cost is paid again on every device creation in the same process, not just the first. It is not mainly a one-time DLL-load cost.

### Cited Findings
- nene-nib (RTX 3090, Release build, driver 32.0.16.1088): the `device_created` span (`D3D11CreateDevice(D3D_DRIVER_TYPE_HARDWARE)` + QI to IDXGIDevice) measured **159.6 ms of a 184.2 ms startup (87%)**. The same span on CI using WARP was **3.6 ms**. The author attributes the ~156 ms gap to NVIDIA UMD loading (attribution, not traced). — [nene-nib #24](https://github.com/hideyukiMORI/nene-nib/issues/24)
- nene-nib follow-up experiments, same machine and driver: control **159.0 ms**; feature levels restricted to {11_1, 11_0} **161.8 ms**; explicit adapter via CreateDXGIFactory2 + EnumAdapters1(0) with D3D_DRIVER_TYPE_UNKNOWN **164.6 ms**; D3D11_CREATE_DEVICE_SINGLETHREADED **158.2 ms**; **second consecutive D3D11CreateDevice in the same process 159.5 ms**; forced WARP **9.5 ms**. Author's conclusion: about 160 ms is a fixed driver cost paid on every call, and the way the device is requested makes no difference (all within about ±6 ms). — [nene-nib PR #29](https://github.com/hideyukiMORI/nene-nib/pull/29)
- Another nene-nib issue on an NVIDIA GPU: D3D11CreateDevice took about 150 ms of a ~190 ms first frame. On a worker thread it took **151–156 ms, 4–5 ms longer than on the UI thread**. — [nene-nib #481](https://github.com/hideyukiMORI/nene-nib/issues/481)
- expo/exact (Windows 11 build 26300, RTX 5070, nvwgf2umx.dll): first D3D11CreateDevice after launch took **96.6 ms**. Later re-creations in the same process, after the previous device was released on hide, took **74.8–76.5 ms**. With the first window frame included, the reopen was 151.4 ms the first time and 83.6–86.2 ms later. The reporter concluded that "nearly all of the reopen is creating the device itself", not the swap chain, surfaces or painting. — [expo/exact #1195](https://github.com/expo/exact/issues/1195)
- Lodestone (Ryzen 9 9950X3D, AMD GPU, GPUI fork): "the first D3D11CreateDevice in a process takes about 70 ms because it loads and initializes the GPU driver". A temporary trace showed device creation at **55–71 ms** on a background thread. — [Lodestone PR #761](https://github.com/Lodestone-Team/lodestone/pull/761)
- amigo-blinzel (AMD Radeon 680M): D3D11CreateDevice took **115–150 ms**, which is longer than reading and decoding a 24 MP JPEG. — [amigo-blinzel PR #8](https://github.com/amigo-labs/amigo-blinzel/pull/8)
- Resources the NVIDIA UMD brings in (expo/exact, RTX 5070, Win11 26300): an app that creates a D3D11 device and never shows a window costs **125 MB private bytes, 56 threads (37 of them NVIDIA driver `NVDEV_Thunk` threads), 47 MB dedicated GPU memory, and 18 extra modules**. The modules include `nvwgf2umx.dll`, `nvldumdx.dll`, `nvgpucomp64.dll`, `nvmemmapstoragex.dll`, `nvspcap64.dll` and `nvppex.dll`. Without the device: 6–7 MB private, 18–19 threads. A WARP device instead: 75 MB private, 29 threads, 0 MB GPU. — [expo/exact #987](https://github.com/expo/exact/issues/987)
- Older Mozilla finding (pre-2018, flagged as old): part of a Firefox startup slowdown came from querying a D3D10.0 device and then a 10.1 device, which is a cost of multiple device creations. — search-result summary of [Mozilla bug 722225](https://bugzilla.mozilla.org/show_bug.cgi?id=722225) (not fetched in full)
- Mozilla also found D3D11 device creation, plus probing DXVA decoder creation, to be the slow part of video initialization on the main thread. — [Mozilla bug 1678684](https://bugzilla.mozilla.org/show_bug.cgi?id=1678684) (search snippet)

### Inferences
- NVIDIA: the second same-process call costs the same as the first (159.5 vs 159.0 ms in nene-nib; 75–77 vs 97 ms in expo/exact). So the dominant cost is **per-device driver initialization** (contexts, driver threads, allocations, KMD calls), not DLL mapping or first-time global init. A **throwaway warm-up device probably saves little** (at most the ~20 ms first-vs-later difference expo/exact saw). The real device still pays nearly full price. The komando setup (throwaway device on a background thread, then the real device on the main thread) fits this: about 160 ms is still spent in platform setup.
- The figures roughly match komando's ~160 ms on an RTX 5070 Ti. That suggests the cost is inherent to current NVIDIA drivers on Windows 11, not a bug in komando or GPUI.
- CreateDXGIFactory and EnumAdapters were never measured separately in any source. nene-nib's explicit-adapter experiment (factory + EnumAdapters1(0) + create) cost about 5 ms more than the null-adapter path. That suggests factory creation and enumeration are single-digit ms; D3D11CreateDevice is the expensive part.

### Gaps
- No public ETW/WPA breakdown of the ~150 ms inside nvwgf2umx.dll (DLL load vs application-profile lookup vs shader-cache init vs thread spawn vs KMD calls). Getting one needs a local WPR trace with stack sampling and Kernel image-load events.
- No Intel iGPU measurements found.
- No vendor documentation (NVIDIA, AMD, Intel) on device-creation cost.
- In the nene-nib "second consecutive call" result, it is not stated whether the first device was still alive.

## 2. Do creation flags, feature levels, the debug layer or DXGI factory flags change the cost?

### Takeaway
No flag has been measured to meaningfully reduce D3D11CreateDevice latency on NVIDIA. SINGLETHREADED and restricted feature levels were within noise. PREVENT_INTERNAL_THREADING_OPTIMIZATIONS cuts driver threads and memory but not latency. The debug layer only adds cost (it loads SDKLayers).

### Cited Findings
- SINGLETHREADED: 158.2 ms vs a 159.0 ms control. Feature levels {11_1, 11_0}: 161.8 ms. Both on an RTX 3090. — [nene-nib PR #29](https://github.com/hideyukiMORI/nene-nib/pull/29)
- PREVENT_INTERNAL_THREADING_OPTIMIZATIONS (RTX 5070): driver threads dropped from **54 to 20** and private bytes fell about 20 MB with the window shown. D3D11CreateDevice still took **74.2–77.7 ms** (unchanged). Effect on draw performance was not measured. — [expo/exact #1195](https://github.com/expo/exact/issues/1195)
- Microsoft: SINGLETHREADED ("By default, the ID3D11Device object is thread-safe. By using this flag, you can increase performance. However, if you ... call methods ... from multiple threads, undefined behavior might result."). PREVENT_INTERNAL_THREADING_OPTIMIZATIONS "Prevents multiple threads from being created ... This flag is not recommended for general use". Its remarks say it asks "the runtime and video driver not create any additional threads", for debugging, profiling and tools. — [D3D11_CREATE_DEVICE_FLAG](https://learn.microsoft.com/en-us/windows/win32/api/d3d11/ne-d3d11-d3d11_create_device_flag)
- BGRA_SUPPORT: "Required for Direct2D interoperability". All 10level9+ hardware with WDDM 1.1+ drivers supports BGRA. No performance statement. — [same page](https://learn.microsoft.com/en-us/windows/win32/api/d3d11/ne-d3d11-d3d11_create_device_flag)
- VIDEO_SUPPORT: Microsoft warns that creating the device twice (once with the flag, then again without it after finding the Basic Render Device) "you actually degrade performance". This is Microsoft's own acknowledgement that a second device creation is a real cost. — [same page](https://learn.microsoft.com/en-us/windows/win32/api/d3d11/ne-d3d11-d3d11_create_device_flag)
- DEBUG requires D3D11*SDKLayers.dll. Device creation fails without it. PREVENT_ALTERING_LAYER_SETTINGS_FROM_REGISTRY makes the runtime ignore registry settings that enable the debug layer. Recommended for release builds. — [same page](https://learn.microsoft.com/en-us/windows/win32/api/d3d11/ne-d3d11-d3d11_create_device_flag)
- Old (about 2014–2016, anecdotal) NVIDIA forum report: high kernel CPU time from Sleep(0) calls inside nvwgf2umx.dll, reduced by a device-creation flag. This is about runtime CPU, not startup. — [NVIDIA dev forum](https://forums.developer.nvidia.com/t/directx11-performance-bug-in-nvwgf2umx-dll/159035) (search snippet only)

### Inferences
- PREVENT_INTERNAL_THREADING_OPTIMIZATIONS is worth considering for idle memory and thread count in a lightweight GUI app (about 34 fewer threads, about 20 MB). It does not help startup latency. Microsoft says it is "not recommended for general use", and it may lower draw throughput with NVIDIA's threaded driver path. Measure before adopting.
- SINGLETHREADED does not affect startup. Since the device is thread-safe by default, setting it also rules out the background-creation-and-handoff pattern unless every later use happens on one thread.

### Gaps
- No measurements found for BGRA_SUPPORT, VIDEO_SUPPORT or the debug layer's added cost, or for DXGI factory flags (DXGI_CREATE_FACTORY_DEBUG).

## 3. Does adapter selection (EnumAdapters loop, EnumAdapterByGpuPreference, null adapter) or a hybrid-GPU setup change the cost?

### Takeaway
On a single-dGPU desktop, an explicit adapter was no cheaper than the null adapter (164.6 vs 159.0 ms). Enumerating adapters only gets IDXGIAdapter objects. The UMD cost comes from creating a device on an adapter. No measurement was found for hybrid-GPU laptops.

### Cited Findings
- Explicit adapter (CreateDXGIFactory2 + EnumAdapters1(0) + D3D_DRIVER_TYPE_UNKNOWN): 164.6 ms vs 159.0 ms for the null adapter. — [nene-nib PR #29](https://github.com/hideyukiMORI/nene-nib/pull/29)
- EnumAdapterByGpuPreference (Windows 10 1803+) reorders enumeration. HIGH_PERFORMANCE order: xGPU, dGPU, iGPU. MINIMUM_POWER order: iGPU, dGPU, xGPU. UNSPECIFIED is equivalent to EnumAdapters1. It is meant for picking the adapter to pass to D3D*CreateDevice. — [Microsoft Learn: EnumAdapterByGpuPreference](https://learn.microsoft.com/en-us/windows/win32/api/dxgi1_6/nf-dxgi1_6-idxgifactory6-enumadapterbygpupreference)

### Inferences
- On a hybrid laptop, creating the device on the iGPU (MINIMUM_POWER) would avoid waking the dGPU and probably cut startup time and power. The 680M result (115–150 ms) shows iGPU UMDs are not free either. This is not measured; treat it as a hypothesis.
- Creating devices on several adapters (for example, probing each) would multiply the per-device cost (see the VIDEO_SUPPORT "create twice degrades performance" note above).

### Gaps
- No timings found for EnumAdapters loops or for Optimus/hybrid systems. No source says whether enumerating adapters alone loads a UMD.

## 4. Is a throwaway warm-up device wasteful compared with creating the real device on a background thread and handing it over? Is that thread-safe?

### Takeaway
Create the real device (and DXGI factory) on a background thread at the start of main and hand it over. Lodestone did this for GPUI and saved about 30–37 ms of time to first frame. A throwaway device on NVIDIA likely buys little, because a second device costs about as much as the first. ID3D11Device is documented as thread-safe (free-threaded), so a device created on one thread can be used on another. The immediate context is not thread-safe and should be used from one thread at a time, preferably the thread that does DXGI Present.

### Cited Findings
- Lodestone PR #761 (GPUI fork, AMD GPU): `WindowsPlatform::new` now starts DirectX device creation on a background thread before OLE, DirectWrite and platform-window setup. `attach_gpu` waits for it and uses those devices. The device is not created twice; the main thread recreates it only if the background attempt fails or panics. Median first frame **244.5 → 213.1 ms** (window opened 239.6 → 207.8 ms). On top of another PR: **160.2 → 123.8 ms**. `attach_gpu` still waits about 20 ms because the main thread has only about 40 ms of other work. Device-lost recovery is unchanged. — [Lodestone PR #761](https://github.com/Lodestone-Team/lodestone/pull/761); fork commit [Lodestone-Team/zed@39a9cfcf](https://github.com/Lodestone-Team/zed/commit/39a9cfcfd2d65b31b28915bc543d3fdf76491e80)
- nene-nib worker-thread handoff (NVIDIA): device creation moved to a worker at the start of `run()`; the renderer takes it from a handoff box once and falls back to local creation if the box is empty. First frame **194.9 → 179.9 ms (−15 ms)**. Window-shown time unchanged (35.6 vs 36.2 ms). On the worker, creation took 151–156 ms, 4–5 ms slower than on the UI thread. Only the UI thread's otherwise-idle wait is saved. Side effect: other worker tasks queued right after startup wait about 150 ms. — [nene-nib #481](https://github.com/hideyukiMORI/nene-nib/issues/481)
- nene-nib estimate before implementing: overlapping can hide at most about 60 ms (the amount of other main-thread work). The 160 ms itself does not go away. — [nene-nib #24](https://github.com/hideyukiMORI/nene-nib/issues/24)
- Same-process second device on NVIDIA: 159.5 ms vs 159.0 ms for the first. — [nene-nib PR #29](https://github.com/hideyukiMORI/nene-nib/pull/29). Re-creation 75–77 ms vs 96.6 ms for the first. — [expo/exact #1195](https://github.com/expo/exact/issues/1195)
- Microsoft: "the use of a device context (ID3D11DeviceContext) is not thread-safe, the use of a Direct3D 11 device (ID3D11Device) is thread-safe ... multiple threads are not required to use critical sections ... to access a single ID3D11Device." The immediate-context thread "should also use that same thread for ... DXGI operations, especially ... IDXGISwapChain::Present". It is invalid to use an immediate context concurrently with most DXGI functions (only AddRef/Release/QI are safe). — [Microsoft Learn: Introduction to Multithreading in Direct3D 11](https://learn.microsoft.com/en-us/windows/win32/direct3d11/overviews-direct3d-11-render-multi-thread-intro)
- SINGLETHREADED removes that thread-safety guarantee ("undefined behavior" if called from multiple threads). — [D3D11_CREATE_DEVICE_FLAG](https://learn.microsoft.com/en-us/windows/win32/api/d3d11/ne-d3d11-d3d11_create_device_flag)
- Old D3D9-era Mozilla note: the device is "owned" by a thread for fundamental operations such as reset. That does not apply to D3D11's free-threaded device, but it explains historical caution. — [Mozilla bug 595277](https://bugzilla.mozilla.org/show_bug.cgi?id=595277) (search snippet)
- Mozilla proposal: make device creation asynchronous and block only when a device is first needed. — [Mozilla bug 1360171](https://bugzilla.mozilla.org/show_bug.cgi?id=1360171) (search snippet)

### Inferences
- For komando: the throwaway device runs in parallel but does not shorten the real device's ~160 ms on the main thread. Creating the real device (and the factory, and possibly the DirectComposition device) on the background thread, as Lodestone does in GPUI, should take nearly all of it off the main thread. The saving is bounded by how much other main-thread work runs in parallel (Lodestone still waited about 20 ms).
- Keep the immediate context and swap-chain/Present work on one thread after handoff. Passing the device across threads once is within the documented contract, as long as SINGLETHREADED is not set.
- The throwaway device also costs about 125 MB, about 37 threads and 47 MB of GPU memory while alive (per expo/exact #987 for one device). Two devices alive at once roughly double transient cost. Removing it saves memory and CPU contention.

### Gaps
- No source measured "throwaway warm-up + real" against "single background real device" head to head on one machine. The conclusion is drawn from the same-process second-device timings.

## 5. D3D11 vs D3D12 vs D3D11On12 vs WARP creation cost; do DirectComposition and CreateSwapChainForComposition add notable cost?

### Takeaway
WARP device creation is about 3.6–9.5 ms against about 160 ms for an NVIDIA hardware device, but it renders on the CPU. Swap-chain, surface and DComp setup were measured as small next to device creation (expo/exact). No reliable D3D12 or D3D11On12 creation timings were found.

### Cited Findings
- WARP: 3.6 ms (CI) — [nene-nib #24](https://github.com/hideyukiMORI/nene-nib/issues/24); 9.5 ms forced on the RTX 3090 machine — [nene-nib PR #29](https://github.com/hideyukiMORI/nene-nib/pull/29).
- WARP device resource cost: 75 MB private, 29 threads, 0 MB GPU (vs 125 MB / 56 threads / 47 MB GPU for NVIDIA hardware). — [expo/exact #987](https://github.com/expo/exact/issues/987)
- expo/exact's reopen path (device + swap chain + surfaces + first paint) was "nearly all" device creation. Present was 1.1 ms per frame once shown. DComp and swap-chain creation were not timed separately. — [expo/exact #1195](https://github.com/expo/exact/issues/1195)
- Microsoft's VIDEO_SUPPORT note says device creation on WARP succeeds for software fallback. — [D3D11_CREATE_DEVICE_FLAG](https://learn.microsoft.com/en-us/windows/win32/api/d3d11/ne-d3d11-d3d11_create_device_flag)

### Inferences
- A "WARP first, hardware later" bootstrap could get first pixels on screen in about 10 ms of device time. But it means creating two devices, re-creating GPU resources on the new device, CPU rasterization costs, and about 75 MB of memory. A CPU/GDI first frame (section 7) is simpler.
- D3D11On12 adds a D3D12 device under D3D11, so it should cost at least as much as D3D12 creation. Not measured; inference only.

### Gaps
- No measured D3D12CreateDevice vs D3D11CreateDevice comparison on NVIDIA was found.
- No isolated timing of DCompositionCreateDevice or CreateSwapChainForComposition was found.

## 6. Shader object creation from precompiled DXBC: driver compile cost, driver shader caches, and hiding it

### Takeaway
Drivers compile DXBC to GPU ISA in CreateVertexShader/CreatePixelShader or lazily at first draw. Drivers have historically also recompiled shaders on background threads in D3D11. No timings were found for small GUI shader sets. The documented app-side control (SetBackgroundProcessingMode) exists only in D3D12.

### Cited Findings
- Microsoft's D3D12 Background Processing spec: in D3D9/D3D11, "IHVs have done background re-compilation/optimization of shaders" on low-priority threads. That work "must not disrupt critical path work" ("2-7% improvements in the average case is not worth even momentary stutter elsewhere"). D3D12 moves this to runtime-owned threads (at most two tasks at idle priority by default) and adds `SetBackgroundProcessingMode` (ALLOWED, ALLOW_INTRUSIVE_MEASUREMENTS, DISABLE_BACKGROUND_WORK, DISABLE_PROFILING_BY_SYSTEM; COMMIT_RESULTS[_HIGH_PRIORITY]). Disable modes work only in developer mode. Support is queried via D3D12_OPTIONS6. The runtime emits ETW events for each task. — [DirectX-Specs: Background Processing](https://microsoft.github.io/DirectX-Specs/d3d/BackgroundProcessing.html)
- D3D11's device is free-threaded, so shader and resource creation can happen on any thread without app synchronization. — [Microsoft Learn: Multithreading intro](https://learn.microsoft.com/en-us/windows/win32/direct3d11/overviews-direct3d-11-render-multi-thread-intro)

### Inferences
- Shaders can be created on the same background thread right after the device, before handoff, so any driver compile cost is hidden too. Because the device is thread-safe, this is legal.
- `nvgpucomp64.dll` (NVIDIA's compiler module) loads with the device (expo/exact #987), which suggests compiler initialization is part of the device-creation cost. This is not confirmed.

### Gaps
- No measured CreatePixelShader/CreateVertexShader cost for small DXBC on NVIDIA/AMD/Intel, and no documentation on whether the NVIDIA DX shader cache (on-disk) applies to D3D11 desktop apps or how it affects launch-to-launch timing.

## 7. NVIDIA "Threaded Optimization" and driver settings; other ways to hide the cost

### Takeaway
No NVIDIA documentation was found on how "Threaded Optimization" affects D3D11 startup. The closest measured lever, PREVENT_INTERNAL_THREADING_OPTIMIZATIONS, does not change latency. The documented ways to hide the cost are architectural: start creation as early as possible off the UI thread, show the window (or a CPU-drawn first frame) before the device exists, or defer the device until first paint.

### Cited Findings
- Only anecdotal community reports about the NVIDIA "Threaded optimization" control-panel setting were found (OpenGL/ioquake3 FPS issues). Nothing covers D3D11 startup. — [gamedev.net thread](https://gamedev.net/forums/topic/580647-threaded-optimization/580647/), [Khronos forum](https://community.khronos.org/t/nvidia-threaded-optimization/55795) (old, anecdotal)
- Show the window first (nene-nib ADR 0013): after the flag and adapter experiments failed, the window is shown before device creation. It appears in about **35 ms (median 34.9 ms)**, the content still arrives after about 160 ms, and the first frame got 5–10 ms slower (accepted). — [nene-nib PR #29](https://github.com/hideyukiMORI/nene-nib/pull/29)
- CPU/GDI first frame while D3D starts (amigo-blinzel, Radeon 680M): the first image is drawn with `SetDIBitsToDevice` into a cloaked window shown via `SW_SHOWNA`, on a redirection surface (no `WS_EX_NOREDIRECTIONBITMAP`), then D3D takes over. First image **230–330 → 54–61 ms**; D3D frame 127–137 ms. Unexplained observation: **with the UI thread pumping messages instead of blocking, D3D11CreateDevice dropped from ~150 ms to ~95 ms**. — [amigo-blinzel PR #8](https://github.com/amigo-labs/amigo-blinzel/pull/8)
- Defer the device until first visible draw (expo/exact): for hidden or background launches, this removed 125 MB, about 38 threads and 47 MB of GPU memory at idle. Device creation was started on the show request so it overlaps other work, with the window kept cloaked until its first Present. Later captures went 207.7–236.8 → 152.8–164.4 ms. — [expo/exact #987](https://github.com/expo/exact/issues/987), [expo/exact #1195](https://github.com/expo/exact/issues/1195)
- Delay-loading shell32/ole32/oleaut32/shlwapi gave no startup benefit (amigo-blinzel). — [amigo-blinzel PR #8](https://github.com/amigo-labs/amigo-blinzel/pull/8)

### Inferences
- The ~95 vs ~150 ms observation in amigo-blinzel suggests the UMD (or DXGI) may wait on window messages from the creating process's UI thread during device creation, for example through a hidden DXGI/driver window or cross-thread SendMessage. If so, a blocked main thread waiting on a background device could inflate its creation time. This is a single anecdote. It is worth checking in komando by having the main thread keep pumping messages, or doing non-blocking work, while the background device is created.
- komando's DirectComposition + flip-model setup cannot use the GDI-first-frame trick directly, because GPUI windows likely use WS_EX_NOREDIRECTIONBITMAP. A cloaked-window + early-show approach, or a CPU-drawn placeholder, would need GPUI changes.

### Gaps
- No NVIDIA, AMD or Intel developer-guide text on device-creation cost or driver profile lookup.
- No Windows Terminal, Zed upstream, or Chromium measurements of D3D11CreateDevice time were found. Chromium's closest metric is `GPU.GPUProcessLaunchTime` ([gpu_process_host.cc](https://chromium.googlesource.com/chromium/src/+/master/content/browser/gpu/gpu_process_host.cc)), which does not isolate device creation.
