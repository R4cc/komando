# Windows audit, 2026-11-15

Audited the Windows-only integrations on Windows, starting from `6f52ca5` (0.5.0).
Linux remains a development host; Windows 11 is the shipping platform.

## Fixed in 0.5.1

- **Local Shell resource ownership.** Eight failed ConPTY shell launches increased the
  process's handle count from 164 to 204. The vendored Windows backend lost ownership of
  console-side pipes, did not close the console on startup errors, and retained process and
  thread handles after successful launches. It also omitted native attribute-list cleanup.
  These resources now have explicit owners on success and failure. Console creation errors
  return an error instead of panicking.
- **Shell exit callbacks.** The exit watcher now owns a duplicate of the borrowed process
  handle and its callback context. Synchronous wait cancellation finishes callbacks before
  freeing their context; closing a shell before its callback runs also releases the context.
  This follows Microsoft's [wait cancellation contract](https://learn.microsoft.com/en-us/windows/win32/sync/unregisterwaitex).
- **Portable updates with several windows.** All windows previously wrote to the same
  `.download` and `.part` paths, and startup removed those files immediately. Separate
  attempts now have unique paths beside the executable, and startup removes only downloads
  older than a week. Regression tests reproduced both the shared-path problem and deletion
  of an active update before the fix.

The complete vendored patch was applied to the original crates.io source and checked to
reproduce every patched source file, including the Windows changes.

## Verification and coverage

- `cargo test --lib --locked -- --quiet`: **346 passed, 20 opt-in tests ignored**.
  This includes native Windows argument parsing for administrator launches, tray close/exit
  window messages, Windows-safe download names and junction handling, Mark of the Web, file
  sharing violations during update renames, and the new isolated ConPTY handle-count checks.
- New Local Session integration coverage runs **cmd and Windows PowerShell** through the
  actual ConPTY session event loop and checks final output and exit notification.
- `powershell.exe -NoProfile -ExecutionPolicy Bypass -File packaging/test-update.ps1`:
  **all six update-helper scenarios passed** under Windows PowerShell 5.1. Process calls are
  substituted: these tests do not actually install, elevate, or restart Komando.
- `cargo test --lib --locked live_github_request -- --ignored --nocapture`: a real update
  check succeeded on Windows and returned `v0.5.0` before publishing 0.5.1.
- A development-profile Windows executable built successfully in about 20 seconds, with
  product version **0.5.1**. The shipping release profile is left to the release workflow;
  its full LTO build was not run locally under the repository's two-minute build limit.

Also inspected backdrop/caption setup, tray lifetime and Explorer-restart handling, file
clipboard export, administrator launch, Windows proxy handling, and MSI packaging/update
wiring. No additional concrete failure was reproduced in those areas. Native UI automation
could not connect: `Computer Use native pipe is unavailable` (Windows error 2). Consequently,
visual backdrop/tray behavior, UAC interaction and a real MSI installation remain unverified.
CI and release packaging were not monitored.
