# Windows process-level startup costs for a large native executable, and how to reduce them

Context for the reader: the app (Rust, 60 MB statically linked exe, thin LTO, codegen-units=4, stripped, unsigned) reaches `main` about 10 ms after process creation and shows a window about 205 ms after. That first number caps everything that happens before `main`: the image loader, static DLL imports, Defender's check at process start, and Rust std init all fit inside about 10 ms on a warm launch. Most of the findings below should be read against that budget.

Research budget note: about 18 search/fetch calls. Several primary sources (Chromium bug trackers, Mozilla perf alerts) returned no content or no numbers, and those are listed as gaps.

## DLL loading: static imports vs delay-load, the parallel loader, and ETW measurement

### Takeaway
Delay-loading exists to cut startup cost by deferring a DLL's load until its first call. However, no source I found gives a measured saving for a modern Windows 10/11 app, and with `main` reached in about 10 ms there is little pre-main loader time left to recover. Delay-loading d3d11/dxgi/dcomp gains nothing if they are called on the path to the first window anyway; it only helps for DLLs that are not needed before first paint.

### Cited Findings
- Raymond Chen: the linker's delay-load feature "is not to make it easier to call functions which might not be there, but rather to assist in application startup performance by avoiding the cost of loading the target DLL until a function from it is called." Delay-loading works per DLL, not per function, and kernel32 cannot be delay-loaded. — [The Old New Thing, 2010-02-01](https://devblogs.microsoft.com/oldnewthing/20100201-00/?p=15123)
- Pitfall: a customer delay-loaded a DLL to speed up startup and then saw occasional crashes at shutdown, because without the static dependency "the loader isn't around to help any more" with ordering. Chen's suggested fix is to skip cleanup during process termination. — [The Old New Thing, 2019-07-18](https://devblogs.microsoft.com/oldnewthing/20190718-00/?p=102719)
- Older tradeoff list (Microsoft blog, Junfeng Zhang): a late-loaded DLL can find its preferred base address taken, which forces a rebase. A commenter says rebasing and init-order issues cause no problems in most cases. This predates ASLR becoming universal, so the rebase point is largely historical. — [Advantages and disadvantages of delay load](https://learn.microsoft.com/en-us/archive/blogs/junfeng/advantages-and-disadvantages-of-delay-load-loadlibrary)
- Windows via C/C++ (O'Reilly excerpt): if an application uses several DLLs, "its initialization time might be slow because the loader maps all the required DLLs into the process' address space." This is a qualitative claim with no number. — [Delay-Loading a DLL](https://oreilly.com/library/view/windows-via-cc/9780735639904/ch20s03.html)
- The Windows 10 parallel loader lives in ntdll and loads libraries on multiple threads to cut process startup time. The thread count can be capped per executable with the IFEO `MaxLoaderThreads` registry value, and chrome.exe is set to 1 (parallel loading disabled). The analysis gives no timing numbers. — [BlackBerry: Windows 10 parallel loading breakdown, 2017](https://blogs.blackberry.com/en/2017/10/windows-10-parallel-loading-breakdown)
- Historical measured example: removing a redundant DLL load cut Chrome startup by 40 ms on Vista. The author also notes that such measurements are usually taken with warm disk caches. — [Evan Martin, "40 milliseconds", 2009](https://neugierig.org/software/chromium/notes/2009/01/startup.html)
- Measurement tooling: Microsoft documents Windows Performance Recorder/Analyzer (WPR/WPA) for startup and responsiveness analysis. — [Optimizing performance and responsiveness (WPT)](https://learn.microsoft.com/en-in/windows-hardware/test/wpt/optimizing-performance-and-responsiveness)

### Inferences
- With `main` at about 10 ms, all static-import loading (d3d11, dxgi, dwrite, dcomp, ole32, user32 and their dependencies) already fits within about 10 ms warm. The ceiling for delay-load savings before `main` is therefore a few milliseconds at most.
- DLLs on the critical path to first present (d3d11, dxgi, dcomp, dwrite, user32) gain nothing from delay-loading, because their load cost just moves to the first call. The useful targets are DLLs that are not needed for the first frame, such as shell, clipboard/OLE or networking DLLs if any.
- The way to verify is a WPR trace with the Loader provider (or the "loader snaps" `sls` flag via gflags, viewed in a debugger). Compare the DLL-load spans with the 10 ms pre-main window. Much of the "DLL cost" may actually be the DLLs' own init work (for example D3D device creation) that happens after `main`.

### Gaps
- I found no published 2016–2026 before/after measurement of /DELAYLOAD savings on Windows 10/11.
- I found no published measurement of the parallel loader's speedup, or of setting `MaxLoaderThreads=1`.
- I did not retrieve Bruce Dawson (randomascii) posts specific to DLL load cost. The search surfaced only his unrelated "No Start Menu for You" (2023) ETW investigation.

## Binary size, page faults, and code-layout optimization (opt-level, LTO, codegen-units, PGO, orderfiles, BOLT)

### Takeaway
On a warm launch the 60 MB image is already in the file cache, so it costs mainly soft page faults for the pages actually touched, not a 60 MB read. Code layout (orderfiles or PGO-driven function ordering) mostly reduces faults and matters most for cold start and slow disks. PGO's measured gains for real apps are in the 10–17% range for startup (Chrome on Windows, 2016). BOLT does not support Windows PE binaries.

### Cited Findings
- Chrome on Windows with MSVC PGO (Chrome 53 x64 / 54 x86): startup time 16.8% faster, new-tab-page load 14.8% faster, time to first paint 5.9% faster. These are measured by Google. — [Chromium Blog: Making Chrome on Windows faster with PGO, 2016-10-31](https://blog.chromium.org/2016/10/making-chrome-on-windows-faster-with-pgo.html)
- Chromium has retired Windows `.orderfile`s (crrev.com/824529). The linker now orders binary contents from PGO data using LLD's call-graph-profile sort, which "gives similar benefits" without maintaining orderfiles. No numbers are given. — [Chromium docs: win_order_files.md](https://chromium.googlesource.com/codesearch/chromium/src/+show/HEAD/docs/win_order_files.md)
- The older orderfile workflow ran an instrumented build to collect a per-process profile, and the linker then laid out symbols from `chrome.x64.orderfile`. — [Chromium docs: Updating the Windows .order files](https://chromium.googlesource.com/chromium/src/+/ab9fc65b771f/docs/win_order_files.md)
- The same idea applied to resources: grouping the .pak resources used at startup means "less page faults to load from disk and a faster start up time and less memory used". No numbers. — [Chromium code review 2690263004, 2017](https://codereview.chromium.org/2690263004)
- Chromium pre-reads its main DLL at startup (`file_pre_reader_win`), touching the module's pages to page it in and checking hard-fault counts. — [chromium file_pre_reader_win.cc](https://chromium.googlesource.com/chromium/src/+/510e13d15/chrome/app/file_pre_reader_win.cc)
- Firefox experiment (2011, HDD era): preloading xul.dll "seems to consistently shave up 2 seconds of startup on my profile (40%)" (a single profile, anecdotal). A test on a slow laptop disk went from 1.6 s without prefetch to 2.6+ s with it. Results are mixed and disk-dependent. — [Mozilla bug 627591](https://bugzilla.mozilla.org/show_bug.cgi?id=627591)
- A later Mozilla bug (closed in 2020) found that better xul.dll readahead gave no startup gain on Windows 7. A 2019 bug notes that prefetching also loads about half of xul.dll that may never be used. — [Mozilla bug 1548048](https://bugzilla.mozilla.org/show_bug.cgi?id=1548048); [Mozilla bug 1548033](https://bugzilla.mozilla.org/show_bug.cgi?id=1548033)
- rustc itself: PGO's potential gains are cited as "roughly 10-20%" for the compiler, and PGO for Windows x64 rustc dist builds was merged in July 2022 (PR #96978). This is compiler throughput, not GUI startup. — [Kobzol, Speeding up the Rust compiler without changing its code, 2022](https://kobzol.github.io/rust/rustc/2022/10/27/speeding-rustc-without-changing-its-code.html); [rust-lang/rust#103595](https://github.com/rust-lang/rust/issues/103595)
- BOLT "only works on ELF binaries, and thus is Linux-only". The rust-lang tracking issue marks BOLT as N/A for Win64. — [rust-lang/rust#103595](https://github.com/rust-lang/rust/issues/103595)
- cargo-pgo is the Rust tool for PGO (and BOLT on Linux) workflows. — [Kobzol: cargo-pgo, 2023](https://kobzol.github.io/rust/cargo/2023/07/28/rust-cargo-pgo.html)
- LLVM's own Windows x64 release binaries built with PGO are reported as about 22% faster at compiling. This is Phoronix reporting on throughput, not startup. — [Phoronix](https://mail.phoronix.com/news/LLVM-PGO-Windows-Build)

### Inferences
- A warm launch with `main` at 10 ms already shows that mapping the 60 MB image is not a pre-main bottleneck. Image pages are demand-faulted, mostly as soft faults from the standby list or file cache on a warm launch, so the cost scales with pages touched during startup, not with file size.
- Function ordering (PGO with call-graph-profile sort via lld-link, or MSVC link `/ORDER`) clusters startup code into fewer pages. Its main payoff is cold start and HDD/slow storage. On a warm SSD launch the expected gain is probably single-digit milliseconds. This is extrapolated, not measured.
- Of the requested knobs, PGO is the one with documented double-digit percentage gains, because it speeds up the CPU-bound post-`main` work (layout, font shaping, GPU setup code) and not just page faults. Chrome's 16.8% startup gain is the best measured analogue.
- codegen-units=1, fat LTO and panic=abort mainly shrink the binary and improve inlining. I found no measured startup numbers for them. Their gains are likely small next to the 195 ms spent after `main`.
- Rust on Windows uses LLVM, and linking with lld-link allows call-graph-profile sort from PGO data, the approach Chromium uses. Confirm with your toolchain whether rustc emits `.llvm.call-graph-profile` for MSVC targets.

### Gaps
- I found no published measurement of Windows startup page-fault reductions from Chromium orderfiles or PGO ordering. crbug.com/1077279 returned no content.
- I found no measured numbers for opt-level, thin vs fat LTO, codegen-units=1 or panic=abort effects on GUI startup time.
- I found no Rust GUI app on Windows with a published PGO startup measurement.

## Windows Prefetcher / SysMain for app launches

### Takeaway
The Application Launch Prefetcher is automatic and per-executable-path. It traces the first 10 seconds of a launch and replays those I/Os asynchronously on later launches. It mainly helps cold or I/O-loaded launches; Microsoft itself says the gains are smallest on an idle SSD. Apps need to do nothing. The optional `/prefetch:N` command-line switch only matters for multi-process apps that relaunch the same exe in different roles.

### Cited Findings
- The prefetcher (part of SysMain, sysmain.dll, since Windows XP) "monitors up to the first 10 seconds of application startup with heuristics to stop sooner if the launch is completed sooner (when the application stops faulting pages)". Traces are written to `%systemroot%\Prefetch\<EXE>-<pathhash>.pf`. On the next launch, "the kernel component of the prefetcher issues asynchronous IOs to prefetch metadata, data, and image pages described in the trace file." — [Microsoft archive blog (Chad Duffey): What is Application Launch Prefetching?, 2014-03-14](https://learn.microsoft.com/en-us/archive/blogs/chadduffey/what-is-application-launch-prefetching)
- Microsoft's claimed impact: "between 10% and 80% improvement in application launch times". "The biggest improvements are when devices are under a heavy IO load. The smallest gains are on SSD devices with no load at all." These are internal tests with no published methodology. — [same source](https://learn.microsoft.com/en-us/archive/blogs/chadduffey/what-is-application-launch-prefetching)
- The .pf file name includes a hash of the file path, so a portable exe in a different folder gets its own trace. Prefetch keeps traces for up to the 128 most recently launched programs. It is on by default on workstations, off on servers, and controlled by `EnablePrefetcher`. SuperFetch was renamed SysMain in later Windows 10 and in Windows 11. — [Wikipedia: Prefetcher](https://en.wikipedia.org/wiki/Prefetcher) (secondary source)
- Mozilla (2024) adopted the `/prefetch:N` switch so that different Firefox process types get separate prefetch-cache buckets. Windows 11 reportedly has an expanded range of slots. Mozilla measured "no particular change to our Windows startup metrics, positive or negative". — [Mozilla bug 1878993](https://bugzilla.mozilla.org/show_bug.cgi?id=1878993)

### Inferences
- For a single-process app that is launched the same way every time, prefetch needs no app changes and works automatically. Its effect on a warm launch, where pages are already in memory, should be near zero. Its value is after reboot or memory pressure.
- Because the trace key includes the path, MSI-installed and portable copies each warm up separately. Changing the exe (a new version) also changes which pages the trace refers to, so the first launch after an update may be partly cold.
- A deliberate `/prefetch:N` is only worth adding if the app spawns helper processes from the same exe.

### Gaps
- I found no 2016–2026 independent measurement of prefetcher benefit on Windows 10/11 with NVMe SSDs.
- I found no official Microsoft documentation of the `/prefetch:N` slot semantics on Windows 11. Only Mozilla and Chromium discussions describe it.

## Microsoft Defender and SmartScreen: unsigned vs signed executables

### Takeaway
Microsoft's own Defender docs state that launching an unsigned .exe or .dll triggers a real-time protection scan, and they recommend signing as the fix. For a 60 MB unsigned exe that scan can be noticeable whenever Defender has no cached clean verdict (first run, after a binary update, after a signature update). SmartScreen is a reputation check on downloaded or Mark-of-the-Web files, keyed per file hash, and is not a per-launch scan. I found no authoritative published millisecond figure for the steady-state warm-launch cost.

### Cited Findings
- Microsoft Learn: "Anytime that a binary (such as .exe, .dll, and so on) is launched/started, if it's not digitally signed, Microsoft Defender Antivirus starts a real-time protection scan." The recommended fix is to sign the binaries. Enterprise workarounds are "Indicators – File hash – allow", certificate allow indicators, or process+path exclusions. — [Microsoft Learn: Troubleshoot performance issues (Defender for Endpoint), updated 2025/2026](https://learn.microsoft.com/defender-endpoint/troubleshoot-performance-issues)
- The same page warns that path exclusions do not cover Behavior Monitoring or Network Real-time Inspection, which can still cause performance issues. — [same](https://learn.microsoft.com/defender-endpoint/troubleshoot-performance-issues)
- Defender keeps a cache: Microsoft warns VDI builders to let "cache maintenance" complete before sealing an image, or performance suffers. A CIS benchmark item describes file hashes as speeding scans by skipping unchanged files already judged safe. — [Microsoft Learn](https://learn.microsoft.com/defender-endpoint/troubleshoot-performance-issues); [Tenable/CIS audit item](https://tenable.com/audits/items/CIS_MS_InTune_for_Windows_11_Level_2_v1.0.0.audit:5c64d9914330fe5b0ee496ce92abe27e)
- Defender Performance Analyzer (`New-MpPerformanceRecording` / `Get-MpPerformanceReport`) is the documented tool for measuring per-file scan time. Since Defender 4.18.2206.X it shows a "SkipReason" column. — [Microsoft Learn: Performance analyzer for Microsoft Defender Antivirus](https://learn.microsoft.com/defender-endpoint/tune-performance-defender-antivirus)
- Anecdotal measured case: a self-contained WPF app had more than 170 files scanned by Defender at launch, adding "up to 9s" to launch time. The author noted that the fast path holds only while a cache persists, and that the cache "is often released". The post has no replies and does not say whether the app was signed. — [Microsoft Tech Community thread 4245582](https://techcommunity.microsoft.com/discussions/microsoftdefenderatp/microsoft-defender-it-seriously-affected-my-app-launch/4245582)
- Anecdotal: a developer saw an unsigned installer scanned for a long time, while "once I produced a signed installer, the scan completes in moments." — [Xojo forum](https://forum.xojo.com/t/windows-defender-scans-my-installer-for-a-very-long-time/49167)
- SmartScreen reputation is per file hash, so even tiny changes to the executable generate a new identity with no history. Unsigned files must build reputation for each new version from zero, while signing lets reputation carry over through the certificate. — [Microsoft Learn: SmartScreen reputation for Windows app developers](https://learn.microsoft.com/windows/apps/package-and-deploy/smartscreen-reputation); [Eric Lawrence: Best Practices for SmartScreen AppRep, 2024](https://textslashplain.com/2024/11/15/best-practices-for-smartscreen-apprep/)
- EV certificates no longer grant immediate SmartScreen reputation. — [ToDesktop blog](https://www.todesktop.com/blog/posts/windows-apps-psa-ev-certs-do-not-grant-immediate-reputation-anymore)
- Smart App Control (Windows 11) blocks unsigned files unless they have positive reputation, and applies signature checks to all executables, not only downloaded ones. — [Eric Lawrence: Smart App Control, 2026-04-28](https://textslashplain.com/2026/04/28/smart-app-control/)
- Historical (2006, XP x64/Defender Beta 2): a Defender ShellExecute hook (MpShHook.dll) caused multi-second process-launch delays through an RPC/domain-SID timeout, cached for 30 s. Shown only as an example of AV hooks in the launch path. It is not relevant to Windows 10/11 and Vista+ did not use that hook. — [Mark Russinovich: The Case of the Process Startup Delays, 2006](https://learn.microsoft.com/en-us/archive/blogs/markrussinovich/the-case-of-the-process-startup-delays)

### Inferences
- The measured 10 ms to `main` suggests that on the developer's machine, warm launches are already hitting a cached Defender verdict. Otherwise hashing or scanning a 60 MB image would likely show up before `main`. The scan can return after each new build, each Defender signature update, or on a user's first launch, which can make "first launch after update" much slower than steady state.
- Authenticode signing is the one documented lever that changes Defender's per-launch behaviour for unsigned binaries. It also stabilises SmartScreen reputation across versions and avoids Smart App Control blocks. Of the techniques in this section it is likely the highest-value one for real users, mainly for first launch after install or update. Its effect on the steady-state warm launch is likely small.
- To quantify: run `New-MpPerformanceRecording` during launches of the unsigned build, then of a signed build (even a test certificate added to the local trust store), and compare per-file scan durations.

### Gaps
- I found no Microsoft-published figure for how long Defender caches a clean verdict, or for which events invalidate it.
- I found no controlled 2016–2026 measurement comparing per-launch Defender overhead for signed vs unsigned large executables.
- Whether a signed binary skips the process-creation scan entirely, or only gets a faster or cached path, is not stated precisely in the docs.

## Rust-specific pre-main costs: std runtime init, static initializers, allocator choice

### Takeaway
Rust's std pre-main runtime work is tiny. An empty Rust `main` starts as fast as a trivial C program, and Rust has no static constructors unless crates like `ctor` add them. The measured 10 ms to `main` is OS loader time, not Rust runtime time. I found no measured data on mimalloc's startup effect. Its relevance is allocation-heavy work after `main`, not process startup.

### Cited Findings
- Rust forum discussion: Rust's small runtime (rt.rs), which runs before and after `main`, plus a few early allocations has startup impact that is "basically zero". This is an opinion from experienced users, not a benchmark. — [users.rust-lang.org: Why does the rust startup routine bloat...](https://users.rust-lang.org/t/why-does-the-rust-startup-routine-bloat-both-stack-and-heap-for-apparently-no-reason/141260)
- A measured CLI case (Linux): an empty `fn main(){}` "spawns just as fast as mine". The only change that moved wall-clock time was static linking with musl, because of fewer startup relocations. — [DEV: Rust binary that spawns in 0.34 ms](https://dev.to/xorio42/from-a-bash-status-line-to-a-rust-binary-that-spawns-in-034-ms-and-why-99-of-it-didnt-matter-5eb1)
- LWN comment (Linux): a trivial Rust `exit(0)` binary using the `origin` crate ran 2000 invocations in about 2.7 s vs about 4.3 s for `/bin/true`. The difference was attributed to dynamic-linking cost, not to the language. — [LWN](https://lwn.net/Articles/1014744)
- Pre-main code in Rust exists only when explicitly added (for example with the `ctor`/`startup` crates). Lazy init (`OnceLock`, `LazyLock`, `lazy_static`) runs on first access, not before `main`. — [grack.com: Life before main, 2026](https://grack.com/blog/2026/06/11/life-before-main/) (`ctor` author's write-up; fetched only via search summary)

### Inferences
- With `main` at 10 ms there is nothing meaningful left to gain from Rust runtime or allocator changes before `main`. Lazily initialised statics that are first touched during window setup move their cost into the 10→205 ms window, and that is where to profile (for example font enumeration, shader compilation, config parsing).
- Allocator swaps (mimalloc) can change the CPU time of allocation-heavy setup after `main`. Any startup gain must be measured, and none was found published.

### Gaps
- I found no published measurement of mimalloc vs the Windows default heap (the Rust System allocator uses HeapAlloc) on GUI startup time.
- I found no published Windows-specific measurement of Rust std init cost, and no Rust GUI startup study on Windows (Zed, WezTerm, Alacritty discussions found only anecdotes: for example a Hacker News commenter saying Zed on Windows was slow on first launch but quick after, and the WezTerm issues relate to PTY/FancyZones, not process startup).
  — [HN discussion of Zed on Windows](https://hn.nuxt.dev/item/45594920); [wezterm#5392](https://www.github.com/wez/wezterm/issues/5392); [wezterm#740](https://github.com/wez/wezterm/issues/740)

## Other: CoInitialize/OleInitialize, single-instance checks, config file reads

### Takeaway
I found no measured numbers for CoInitializeEx/OleInitialize cost, named-mutex or named-pipe single-instance checks, or small config file reads on Windows 10/11 in this research pass. They are generally assumed negligible next to D3D/DirectComposition device creation and font loading, but that assumption is unverified here.

### Cited Findings
- No sourced measurements were found for these items in this research pass.

### Inferences
- Engineering expectation, not measured: a named mutex or named-pipe open is a kernel object lookup in the session namespace and should take microseconds. A small local config read is a few file-system calls. OleInitialize loads ole32/combase and sets up an STA, typically low single-digit milliseconds, mostly DLL load if not already loaded. Each should be confirmed with a WPR trace or by wrapping the calls in `QueryPerformanceCounter` timestamps.
- Watch for the exceptions that can be expensive: a config path on a network share or redirected (OneDrive) folder, or a single-instance handoff that waits on another process's message loop.

### Gaps
- I found no source quantifying CoInitializeEx/OleInitialize, CreateMutex/CreateNamedPipe or config-read latency on Windows 10/11. Direct measurement in the app is recommended.
