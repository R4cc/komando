# DirectWrite startup cost on Windows 10/11 and how to reduce it

Scope note: modern (2017-2026), Windows 10/11-specific *measured* numbers for DirectWrite init are scarce in public sources. Most hard numbers come from Mozilla bugs from the Windows 7 era (2010-2011) and a 2022 Firefox bug. Microsoft documents API semantics but publishes no timings. Every number below is labelled with its era and with whether it was measured or estimated.

## 1. What does DWriteCreateFactory (shared vs isolated) cost; role of the Windows Font Cache Service, cold vs warm?

### Takeaway
Microsoft publishes no cost figure for DWriteCreateFactory. A shared factory is documented to use the cross-process font cache (the FontCache service), and an isolated one is not. Windows 7-era measurements showed factory creation of about 550-700 ms on cold boot, which was mostly DLL and service startup. A 2022 Windows 10 measurement put a warm GetSystemFontCollection at about 2 ms. Cold-boot and "cache rebuild" cases are the outliers that cost hundreds of ms to seconds.

### Cited Findings
- **Shared factory and the cache (Microsoft docs):** DWRITE_FACTORY_TYPE_SHARED "allows for the reuse of cached font data across multiple in-process components. Such factories also take advantage of cross process font caching components for better performance." An ISOLATED factory's objects "do not interact with internal DirectWrite state from other components." — [DWRITE_FACTORY_TYPE (Microsoft Learn)](https://learn.microsoft.com/en-us/windows/win32/api/dwrite/ne-dwrite-dwrite_factory_type)
- **Docs recommendation:** The factory "contains internal state data such as font loader registration and cached font data". The docs recommend shared "in most cases" (memory sharing) and isolated only for sandboxing untrusted components. They say nothing about speed. — [DWriteCreateFactory (Microsoft Learn)](https://learn.microsoft.com/en-us/windows/win32/api/dwrite/nf-dwrite-dwritecreatefactory)
- **DWriteCore restricted factory:** DWriteCore (Windows App SDK) adds DWRITE_FACTORY_TYPE_ISOLATED2, a "restricted factory" that "doesn't interact with a cross-process nor persistent font cache in any way". Its system font collection "includes only well-known fonts". System DirectWrite returns E_INVALIDARG for this value if it doesn't support it. — [DWriteCore overview (Microsoft Learn)](https://learn.microsoft.com/en-us/windows/win32/directwrite/dwritecore-overview)
- **DWriteCore loading claim (not measured):** Microsoft claims DWriteCore's "cleanup of the font cache, and the in-memory font loader, allow for faster loading of fonts, and memory improvements". It gives no numbers. — [DWriteCore overview](https://learn.microsoft.com/en-us/windows/win32/directwrite/dwritecore-overview)
- **Measured, Windows 7, 2010, reboot between runs:** Standalone test app, Mozilla bug 602792 comment 5. Factory creation took 551-704 ms and GetSystemFontCollection took 155-6260 ms. Example results:
  - static build: 781 ms total (625 factory / 155 collection)
  - dynamic build: 6934 ms total (673 / 6260)

  — [Mozilla bug 602792](https://bugzilla.mozilla.org/show_bug.cgi?id=602792)
- **Cold-start mechanism (Mozilla, after talking with a DirectWrite developer, Windows 7 era):** Creating the factory starts the FontCache service. If the service is still starting, client calls may time out and fall back to reading font files directly. This makes cold-boot I/O much heavier (about 158 MB DirectWrite vs 84 MB GDI in one trace). — [Mozilla bug 602792](https://bugzilla.mozilla.org/show_bug.cgi?id=602792)
- **Windows 7 fixes:**
  - Microsoft's KB2454826 changed FontCache to "Automatic (Delayed start)".
  - KB2505438 fixed a delay in which, per a search snippet of the now-404 Microsoft KB, "when the computer restarts, the DirectWrite API waits for two seconds while the FontCache service loads the system cache".

  Both are Windows 7-specific. — [Mozilla bug 602792](https://bugzilla.mozilla.org/show_bug.cgi?id=602792); [Mozilla bug 600713](https://bugzilla.mozilla.org/show_bug.cgi?id=600713); [Microsoft KB2505438 (now 404)](https://support.microsoft.com/en-us/help/2505438/slow-performance-in-applications-that-use-the-directwrite-api-on-a-com)
- **Measured, Windows 10, 2022 (Mozilla bug 1766908):** Jonathan Kew measured GetSystemFontCollection at 2 ms on his Windows 10 machine. A user's profile showed the same call taking more than 1 s in Firefox's privileged content process on an i9-9900K with NVMe storage. Kew speculated that DirectWrite was "failing to use its cache and rebuilding it" on each launch. Deleting the font cache in system32 and restarting the FontCache service "did seem to alleviate" it, with no measured before/after. QA could not reproduce it, and the bug closed INCOMPLETE. — [Mozilla bug 1766908](https://bugzilla.mozilla.org/show_bug.cgi?id=1766908)
- **Antivirus as an amplifier (Windows 7 era):** A file-level virus scanner made GetSystemFontCollection take "25-40 seconds" on a netbook, against about 5 s with the scanner off. Chromium also suspected antivirus per-file checks inflate font load latency. — [Mozilla bug 602792](https://bugzilla.mozilla.org/show_bug.cgi?id=602792); [Chromium DirectWrite Font Cache (obsolete)](https://www.chromium.org/developers/design-documents/directwrite-font-cache/)
- **Sandboxed processes:** The font cache service is reached over an "undocumented" ALPC IPC. Sandboxed renderers cannot reach it, which is why Chromium built a browser-side font cache and later the DWriteFontProxy. — [Chromium DirectWrite Font Cache (obsolete)](https://www.chromium.org/developers/design-documents/directwrite-font-cache/)

### Inferences
- In a non-sandboxed desktop app on Windows 10/11 with FontCache running and warm, DWriteCreateFactory(SHARED) plus GetSystemFontCollection should be in the low milliseconds (consistent with Kew's 2 ms). The ~45 ms observed in this app is therefore more likely dominated by:
  - dwrite.dll load and first-touch page faults,
  - the checkForUpdates=TRUE check (section 2),
  - custom font set / collection building and first font-face creation (section 3),
  - or a cold/rebuilding font cache.

  An ETW trace with dwrite.dll stacks would separate these.
- An isolated factory forgoes the cross-process cache. It is therefore unlikely to be faster on a warm system and may be slower, since it must build state in-process. No source measures this.
- Cold-boot first launch, while FontCache is still starting (it is delayed-start on Windows 7, and the Windows 10/11 setting is unverified), can cost far more than warm launch. Benchmarks should report warm and post-reboot runs separately.

### Gaps
- No Microsoft-published or recent (2017+) measurement of DWriteCreateFactory alone on Windows 10/11.
- No measured shared-vs-isolated factory timing comparison found.
- The FontCache service start type and cache file details for Windows 10/11 (e.g. FNTCACHE.DAT vs `%WinDir%\ServiceProfiles\LocalService\AppData\Local\FontCache\*.dat`) were not confirmed from a primary source in this pass.

## 2. Cost of GetSystemFontCollection and the checkForUpdates flag; GetSystemFontSet vs collection; any way to make it cheaper or lazy?

### Takeaway
Microsoft documents checkForUpdates=TRUE as forcing "an immediate check for changes to the set of system fonts". With FALSE, changes are still detected via the font cache service, with some latency. A Zed fork removed checkForUpdates=TRUE from GPUI's DirectWriteTextSystem::new for exactly this startup reason, estimating "tens of milliseconds" (not measured). Historically, GetSystemFontCollection is the dominant DirectWrite init cost when it is slow.

### Cited Findings
- **Microsoft's definition of the flag:** "If this parameter is TRUE, the function performs an immediate check for changes to the set of system fonts. If this parameter is FALSE, the function will still detect changes if the font cache service is running, but there may be some latency." The docs' example use of TRUE is an app that "has just installed a font". The IDWriteFactory3 overload also has includeDownloadableFonts (cloud fonts vs only locally installed). — [IDWriteFactory3::GetSystemFontCollection (Microsoft Learn)](https://learn.microsoft.com/en-us/windows/win32/api/dwrite_3/nf-dwrite_3-idwritefactory3-getsystemfontcollection); [IDWriteFactory::GetSystemFontCollection](https://learn.microsoft.com/en-us/windows/win32/api/dwrite/nf-dwrite-idwritefactory-getsystemfontcollection)
- **Zed fork PR #81 (Finityno/zed), the GPUI change:**
  - It changed `DirectWriteTextSystem::new` to `GetSystemFontCollection(false, &mut result, false)`.
  - The rationale: the old code "asked DirectWrite to check the system font collection for updates", which "rebuilds it synchronously before the first window can draw".
  - A code comment estimates "collection rebuild (tens of milliseconds)". This is an estimate, not a measurement.
  - Safety net: "a lookup that misses rebuilds the collection in `select_and_cache_font`", so newly installed fonts are still found.
  - Known trade-off: `all_font_names` (font picker/settings schema) may omit just-installed fonts.
  - The author notes that adding a check on the first `all_font_names` call would bring the synchronous rebuild back at startup, because the app lists fonts while resolving its theme.
  - The PR also moved DXGI output enumeration off the main thread.

  — [Finityno/zed PR #81](https://github.com/Finityno/zed/pull/81)
- **Historic telemetry (Windows 7 era, Mozilla bug 600713, 2011):**
  - About 4.16% of 39,429 DirectWrite font-list inits took more than 180 ms, "all of that time ... spent fetching the system font collection via the GetSystemFontCollection API call".
  - GetSystemFontCollection alone: 35,960 samples under 1 ms, 1.64% over 2.3 s, and tails at 8 s and 30 s buckets.
  - Opt-in data, roughly 2% of users, a single day.

  — [Mozilla bug 600713](https://bugzilla.mozilla.org/show_bug.cgi?id=600713)
- **Share of a slow startup (Windows 7, 2010):** One xperf trace of a 13.9 s cold start attributed 6.7 s to font-list init, "99% of this time was spent in the GetSystemFontCollection method". — [Mozilla bug 602792](https://bugzilla.mozilla.org/show_bug.cgi?id=602792)
- **Firefox mitigations:**
  - Mozilla moved font-list initialization to a thread (`gfx.font-list-omt.enabled`, default false outside macOS in 2022). A user reported that forcing the font-list thread on made session restores "instantaneous".
  - Mozilla also defers cmap and name loading until after startup and uses hard-coded fallbacks to avoid scanning.

  — [Mozilla bug 1766908](https://bugzilla.mozilla.org/show_bug.cgi?id=1766908); [Mozilla bug 600713](https://bugzilla.mozilla.org/show_bug.cgi?id=600713)
- **Chromium:** The browser process builds a font lookup table from GetSystemFontCollection, with UMA histograms `DirectWrite.Fonts.Proxy.LookupTableBuildTime`, `LookupTableReadyTime`, and `NumFontFiles`, and serves renderers via DWriteFontProxy. No published values were found. — [Chromium dwrite_font_lookup_table_builder_win.cc](https://chromium.googlesource.com/chromium/src/+/fb5c1b013f7932c56bd99c575f9088a8d7d26fc9/content/browser/renderer_host/dwrite_font_lookup_table_builder_win.cc)

### Inferences
- **Lowest-risk change for this app:** pass `checkForUpdates = false` (and `includeDownloadableFonts = false`) at startup. Re-query with TRUE only on a lookup miss or a user action like opening a font picker. This is the same design as the Zed fork PR.
- **Deferring the system collection:** if the first frame only needs the embedded fonts (Cascadia Mono, Geist), the system collection is needed only for fallback or for user-chosen system fonts. It could then be fetched lazily, or on a background thread, after the first window is shown.
- **GetSystemFontSet:** IDWriteFactory3::GetSystemFontSet returns the flat font set, while the collection adds the family/WWS grouping on top. No source gives a timing comparison. Both come from the same underlying system font cache, so the difference is likely small next to the update check, but that is unverified.

### Gaps
- No measured Windows 10/11 cost of checkForUpdates=TRUE vs FALSE; the "tens of ms" figure is an unmeasured estimate.
- No timing comparison of GetSystemFontSet vs GetSystemFontCollection found.
- No Microsoft documentation of a "lazy" system collection mode.

## 3. In-memory vs local file loaders vs AddFontMemResourceEx; cost of building font sets/collections; off-main-thread registration and thread safety

### Takeaway
Microsoft documents how the in-memory loader works, including that it avoids a copy if you give it an owner object, and that AddFontFile reads the font data to derive properties. It publishes no timings and nothing comparing it with AddFontMemResourceEx. Supplying custom properties via AddFontFaceReference stops DirectWrite deriving properties from the font data, which may avoid some parsing. No explicit Microsoft thread-safety guarantee for IDWriteFactory or IDWriteFontSetBuilder was found.

### Cited Findings
- **AddFontFile:** IDWriteFontSetBuilder1::AddFontFile (Windows 10 Creators Update, build 15021+) handles all faces in a collection file and all named instances of a variable font in one call, and "is recommended whenever available". — [Custom Font Sets (Microsoft Learn)](https://learn.microsoft.com/en-us/windows/win32/DirectWrite/custom-font-sets-win10)
- **When properties are read:** "When custom fonts are added to a font set builder using the API calls listed above, these informational properties are obtained directly from the actual font data, which is read as the font is added."
- **Custom properties:** "if any custom property values are set by the app, then only those values that are specified will be used ... DirectWrite will not derive any values directly from the font". These are set via AddFontFaceReference(ref, props, count).

  — [Custom Font Sets](https://learn.microsoft.com/en-us/windows/win32/DirectWrite/custom-font-sets-win10)
- **In-memory data copy:** If a buffer pointer is passed "without a clear indication that there is an owner, then DirectWrite will make a copy of the data into a new memory buffer". To avoid the copy, pass an IUnknown data-owner object. The in-memory loader must stay registered while its fonts are in use and be unregistered before the factory is destroyed. The data must be raw OpenType, not WOFF/WOFF2. — [Custom Font Sets](https://learn.microsoft.com/en-us/windows/win32/DirectWrite/custom-font-sets-win10); [CreateInMemoryFontFileReference](https://learn.microsoft.com/en-us/windows/win32/api/dwrite_3/nf-dwrite_3-idwriteinmemoryfontfileloader-createinmemoryfontfilereference)
- **Combining sets:** IDWriteFontSetBuilder::AddFontSet adds all fonts of an existing set in one call. GetMatchingFonts filters a set. These can combine embedded fonts with system fonts without enumerating them individually. — [Custom Font Sets](https://learn.microsoft.com/en-us/windows/win32/DirectWrite/custom-font-sets-win10)
- **Bundled font, Firefox (Windows 10 era):** Loading a single bundled TTF (Twemoji) took "about 1 second" in one cold-start profile. Jonathan Kew called that "a fairly extreme outlier", "generally seeing only a few milliseconds". The most he saw for all gfxPlatformFontList init was 30 ms, mostly in other DirectWrite calls (GetDefaultFontForPlatform). Florian Quèze attributed the slow cases to cold disk I/O on HDDs plus antivirus.
- **Why Firefox didn't move it off the main thread:** Kew said moving the bundled-font load off the main thread or making it lazy would invalidate font references and force a full reflow. — [Mozilla bug 1686274](https://bugzilla.mozilla.org/show_bug.cgi?id=1686274)
- **Thread safety (unofficial sources):**
  - Skia's 2013 DirectWrite port noted that its global IDWriteFactory *initialization* was not thread-safe and fixed it with one-time init. — [Chromium code review 107963003](https://codereview.chromium.org/107963003/)
  - The piet-direct2d Rust crate marks its DirectWrite factory wrapper `Send`. It says it "couldn't find any documentation about using IDWriteFactory in a multi-threaded context". — [piet-direct2d dwrite.rs](https://docs.rs/piet-direct2d/0.6.1/src/piet_direct2d/dwrite.rs.html)
- **Direct2D is different:** Direct2D's multithreaded factory documents serialization of resource access. That guidance covers Direct2D, not DirectWrite. — [Multithreaded Direct2D apps](https://learn.microsoft.com/en-us/windows/win32/Direct2D/multi-threaded-direct2d-apps)

### Inferences
- For ~6 embedded fonts, the measured ~6 ms is plausible and consistent with Kew's "a few milliseconds" for one bundled font. Ways to cut it:
  - Pass a data-owner IUnknown, or keep `&'static [u8]` from `include_bytes!` behind an owner object, so DirectWrite doesn't copy each buffer.
  - Build the custom set **once** with all embedded fonts, rather than creating an empty set or collection at init and rebuilding later. The empty CreateFontSet and CreateFontCollectionFromFontSet at startup is redundant work if the embedded fonts are always added afterwards.
  - Optionally supply custom properties via AddFontFaceReference so DirectWrite skips deriving them from font data. Whether this saves measurable time is unverified.
- **Moving font registration to a background thread:** building a font set on a worker thread with its own builder object, then handing the resulting IDWriteFontSet/IDWriteFontCollection to the main thread, is a pattern apps use. It is not covered by an explicit Microsoft guarantee. The safer pattern:
  - Create the factory once (one-time init).
  - Do all builder work on one thread.
  - Publish the finished immutable set/collection with a happens-before edge, such as a channel or join.
- **AddFontMemResourceEx** is GDI-only, so the fonts would not show up in DirectWrite's custom collection. GDI-registered private fonts reach DirectWrite only via GDI interop, so it is not a substitute for this app's path. This is not verified against a primary source.

### Gaps
- No measured comparison of in-memory loader vs local file loader vs AddFontMemResourceEx.
- No official Microsoft statement on thread safety of IDWriteFactory, IDWriteFontSetBuilder, or IDWriteInMemoryFontFileLoader was found in this pass. Treat this as an open item and verify empirically, or in the Windows SDK headers and samples.
- No timing for CreateFontSet / CreateFontCollectionFromFontSet on small sets.

## 4. Is it safe/useful to init DirectWrite on a background thread concurrently with D3D11 device creation? Apps that do this or measured it?

### Takeaway
No source documents a hazard between DirectWrite init and D3D11 device creation: they are separate DLLs and services, and DirectWrite factory creation does not need a D3D device. Firefox has a font-list-off-main-thread mode, and a Zed fork moved DXGI enumeration off the main thread. No public measurement of overlapping DirectWrite init with D3D11 device creation was found.

### Cited Findings
- **Firefox:** Firefox has `gfx.font-list-omt.enabled` to initialize the platform font list off the main thread. It was default-on only on macOS as of 2022. A user reported that forcing the font-list thread made restores "instantaneous", while Kew's suggestion of the pref "didnt seem to do much" for that reporter. Results were mixed and anecdotal. — [Mozilla bug 1766908](https://bugzilla.mozilla.org/show_bug.cgi?id=1766908)
- **Firefox's constraint:** Loading fonts late or asynchronously means font references may be invalidated, forcing a reflow when the set changes. That is a design cost for apps that lay out text before the fonts are ready. — [Mozilla bug 1686274](https://bugzilla.mozilla.org/show_bug.cgi?id=1686274)
- **Zed fork (GPUI):** It moved DXGI output enumeration "off the main thread so startup does not wait for them", and removed the synchronous system-collection update check. It did not move DirectWrite to a thread, and reports no measured timings. — [Finityno/zed PR #81](https://github.com/Finityno/zed/pull/81)
- **Chromium:** Chromium does system font enumeration in the browser process, a separate process from renderers, and builds its lookup table asynchronously. Renderers query via DWriteFontProxy IPC. — [Chromium DirectWrite Font Cache (obsolete)](https://www.chromium.org/developers/design-documents/directwrite-font-cache/); [Chromium dwrite_font_lookup_table_builder_win.cc](https://chromium.googlesource.com/chromium/src/+/fb5c1b013f7932c56bd99c575f9088a8d7d26fc9/content/browser/renderer_host/dwrite_font_lookup_table_builder_win.cc)
- **Mozilla 2010 suggestion:** Create the DirectWrite factory early so the FontCache service starts sooner. Another commenter reported that starting the service manually did not help. — [Mozilla bug 602792](https://bugzilla.mozilla.org/show_bug.cgi?id=602792)
- **Windows Terminal:** Searches found no Windows Terminal (AtlasEngine) issue or PR that measures DirectWrite startup cost or moves its init to a background thread. The closest is a 2021 issue noting that @lhecker contacted the DirectWrite team about per-call rendering overhead, not startup. — [microsoft/terminal #10623](https://github.com/microsoft/terminal/issues/10623)

### Inferences
- **Overlap with D3D11:** starting a thread at process entry that runs DWriteCreateFactory, GetSystemFontCollection(checkForUpdates=false), and the embedded font-set build, while the main thread creates the window and the D3D11 device, should hide most of the ~45 ms, if the main thread doesn't need fonts before the device is ready. Both workloads are largely I/O, DLL loading, and IPC, so they may parallelize well on multi-core machines. Join before the first text layout.
- **Safety:** keep the factory, collection and loader handoff one-way. Build on the worker and send the finished COM pointers to the main thread. GPUI's `DirectWriteTextSystem` must be Send-able, or the raw interfaces wrapped, to allow this. COM apartment: DirectWrite is not a COM-apartment-bound API, since it is created via a flat export, not CoCreateInstance. Apartment issues are therefore unlikely, but no source confirmed this.
- Measure it in this app with ETW. No external number predicts the gain.

### Gaps
- No public measurement of overlapping DirectWrite init with D3D11 device creation.
- No Windows Terminal or Zed (upstream) primary source measuring DirectWrite startup cost was found.
- Microsoft has no explicit statement on DirectWrite-on-a-worker-thread for startup.

## 5. Font fallback (IDWriteFontFallback) and first text layout cost on first frame

### Takeaway
Microsoft's docs say nothing about first-call cost for IDWriteFactory2::GetSystemFontFallback or MapCharacters. Firefox measured that "other DirectWrite calls" such as default-font lookup dominated its 30 ms font-list init. Third-party reports put the first fallback that loads CJK or emoji fonts at around 100+ ms. Those reports are low-authority.

### Cited Findings
- **MapCharacters docs:** IDWriteFontFallback::MapCharacters "determines an appropriate font to use to render the beginning range of text". The docs don't mention performance. — [IDWriteFontFallback::MapCharacters (Microsoft Learn)](https://learn.microsoft.com/en-us/windows/win32/api/dwrite_2/nf-dwrite_2-idwritefontfallback-mapcharacters)
- **Firefox (Windows 10 era):** Of a maximum ~30 ms gfxPlatformFontList init, "most of that was in other DirectWrite calls", particularly under GetDefaultFontForPlatform. — [Mozilla bug 1686274](https://bugzilla.mozilla.org/show_bug.cgi?id=1686274)
- **Cold-path face creation (Windows 7 era):** In cold conditions DirectWrite read entire font files when creating IDWriteFontFace. Warm startups were served from the system font cache. Mozilla's mitigation was lazy IDWriteFontFace creation and lazy metrics. — [Mozilla bug 602792](https://bugzilla.mozilla.org/show_bug.cgi?id=602792)
- **Low-authority third-party reports (unverified):**
  - A 116-121 ms stall at the first text needing fallback (CJK or emoji) when the whole fallback stack was loaded synchronously. — [Carnival-Science/Alicorn issue #233](https://github.com/Carnival-Science/Alicorn/issues/233)
  - A warm first MapCharacters call answering in about 109-125 ms. — [115dkk/mactype_tauri PR #119](https://github.com/115dkk/mactype_tauri/pull/119)
  - MapCharacters "is not free" and its results should be cached. — [deblasis/wintty issue #78](https://github.com/deblasis/wintty/issues/78)

### Inferences
- **What to warm up:** for a terminal whose first frame shows only ASCII in Cascadia Mono, the first frame should not need system fallback at all. The first-frame cost should be one IDWriteFontFace creation from in-memory data, plus glyph rasterization for the visible glyphs. To keep fallback off the critical path:
  - create the system font fallback object lazily, or on the init worker thread;
  - cache MapCharacters results per code-point range.
- **Optional warm-up:** a background warm-up after first paint can pre-pay first-fallback latency before the user hits non-Latin text. For example, call MapCharacters once for a CJK or emoji sample, or create the fallback faces.

### Gaps
- No authoritative measurement of the cost of GetSystemFontFallback creation, the first MapCharacters call, or the first IDWriteTextLayout/glyph-run analysis on Windows 10/11.
- The third-party figures above come from small GitHub projects with unclear methodology.
