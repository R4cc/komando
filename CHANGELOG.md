# Changelog

All notable changes to Komando are listed here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and versions follow
[Semantic Versioning](https://semver.org/).

## [Unreleased]

## [0.1.9] - 2026-11-11

### Changed

- The strip above the terminal names the session (for example **web01**) before its
  user@address:port, so you can always see which server you're typing into.
- Secondary text (hints in Settings, the session address, section labels) has more contrast,
  most noticeably in the light theme.
- The minimap dims plain text and keeps coloured text strong, so errors and warnings stand
  out. The box marking what's on screen now has an outline.
- Command rail marks are easier to see.
- Highlights settings: single rules get smaller, neutral switches, so the set's own switch and
  the rule colours stand out. Hover **regex** or **Aa** to see what they mean.
- Highlight colours are adjusted to read at 4.5:1 or better on the terminal: very light
  colours are darkened on a light terminal (previously only to 3:1), and very dark colours are
  lightened on a dark one. Rules that set their own background keep their colours as they are.
- Highlights settings show each pattern as a small chip around its text instead of a
  full-width bar that looked like an input field.
- Switches that are off show their track clearly, and sliders use the same purple as the
  other controls.
- First run: the "there is no recovery" warning stands out, and the skip button reads **Set
  up later**. Warning text in the light theme is a darker amber, so it reads at 5:1.
- Red buttons have more contrast in the dark theme.
- Settings → Security shows "1 saved" instead of a bare number. Settings → About shows the
  data folder in monospace on one line, and the version only once.

### Fixed

- The close button above the terminal sits apart from the copy buttons and turns red on
  hover, so closing a live session by accident is less likely.
- Deleting a highlight set takes a second click on **Delete set**; one click could delete the
  whole set without warning. A double-click on the trash icon doesn't count as both clicks.

## [0.1.8] - 2026-11-11

### Added

- Minimap: the whole output in miniature on the terminal's right edge, like in VS Code, coloured
  like the text so errors and warnings stand out. A box marks what's on screen; click or drag to
  scroll there. Turn it off in **Settings → Terminal**.
- Command rail: a short line at the left edge of the terminal for each command you entered.
  Hover one to see the command and when you ran it; click it to scroll back to that line,
  which lights up briefly. It sits in the terminal's margin, so the terminal keeps all its
  columns. Turn it off in **Settings → Terminal**.

### Removed

- The vault status row at the bottom of the sidebar; only Settings is there now. Lock and
  unlock the vault in Settings, or lock it with Ctrl+Shift+L.

## [0.1.7] - 2026-11-11

### Changed

- README screenshots retaken to match the current look: the slim session strip and the
  sidebar of hosts.

## [0.1.6] - 2026-11-11

### Added

- Portable use without the installer: README steps for the `.exe` (download, unblock, run), a
  "latest" download link for it (releases now also attach `Komando-x64.exe`), and step-by-step
  instructions for building Komando yourself on Windows.

### Changed

- `komando.exe` now includes the Visual C++ runtime, so it runs on PCs without the VC++
  Redistributable installed.

## [0.1.5] - 2026-11-11

### Changed

- README rewritten for people who want to know what Komando is: a headline, why-Komando
  summary, feature overview, install steps and FAQ. The detailed highlight rule format and
  the build and release notes moved to `docs/highlights.md` and `docs/development.md`.

## [0.1.4] - 2026-11-11

### Changed

- The bar above the terminal is gone, leaving more room for output. In its place, a slim
  line at the top left shows the connection state as a coloured dot and `user@address:port`
  (hover the line for the state in words). Copy output, Copy as .txt and close are now small
  icons at the right of that line, with tooltips.

## [0.1.3] - 2026-11-11

### Added

- README download link that always points to the installer of the latest release. Releases
  now also attach the installer as `Komando-x64.msi` for that link.

## [0.1.2] - 2026-11-11

### Changed

- The sidebar lists every saved host and local shell instead of open sessions. Clicking one
  connects, or returns to its session if it's already connected. Live items are bright with a
  status dot; idle ones are dimmed.
- When a connection drops or you disconnect (×, Ctrl+Shift+W), the host goes idle but keeps
  its output; clicking it again reconnects in the same terminal. A second × discards the output.
- Ctrl+click (or the + on a row) opens another session on the same host, shown as a nested
  row. Ctrl+Shift+T now does the same for the current host.
- Local shells are listed at the top of the sidebar on their own and can also belong to a
  project. New installs no longer get a "Local" project.

## [0.1.1] - 2026-11-11

### Fixed

- Windows build: 0.1.0 did not compile on Windows because of the code that matches the Mica
  tint to the app theme.

## [0.1.0] - 2026-11-11

First version.

### Added

- Terminal built on gpui and alacritty_terminal: output is parsed off the UI thread and drawn
  at most once per frame, with selection, scrollback and bracketed paste.
- Embedded SSH client: password and key authentication (OpenSSH and PuTTY `.ppk`), host key
  trust on first connect, connections blocked when a host key changes, and keys trusted in
  `~/.ssh/known_hosts` recognised.
- Local PowerShell 7, Windows PowerShell and Command Prompt sessions.
- Projects and hosts with colours and icons; a host can belong to several projects and takes
  each project's colour, user and password unless it sets its own.
- Vault encrypted with a master password (Argon2id + XChaCha20-Poly1305), an offer to save a
  typed password that worked (for the host or as the project password), and auto-lock.
- Output highlighting with rule sets in Tabby's format: import, export, and switching sets and
  single rules on or off. A 69-rule set for Linux and service logs is included.
- Copy a session's whole output as text, or as a `.txt` file on the clipboard.
- Settings page with Appearance, Terminal, Highlights, Security and About sections.
- Dark and light themes for the app and the terminal, set independently; dark by default.
- Background blur using Windows 11 Mica, with an intensity setting.
- Island layout, KOMANDO serif wordmark and KO app icon (window, taskbar, installer).
- MSI installer attached to GitHub releases by the release workflow.

[Unreleased]: https://github.com/R4cc/komando/compare/v0.1.8...HEAD
[0.1.8]: https://github.com/R4cc/komando/compare/v0.1.7...v0.1.8
[0.1.7]: https://github.com/R4cc/komando/compare/v0.1.6...v0.1.7
[0.1.6]: https://github.com/R4cc/komando/compare/v0.1.5...v0.1.6
[0.1.5]: https://github.com/R4cc/komando/compare/v0.1.4...v0.1.5
[0.1.4]: https://github.com/R4cc/komando/compare/v0.1.3...v0.1.4
[0.1.3]: https://github.com/R4cc/komando/compare/v0.1.2...v0.1.3
[0.1.2]: https://github.com/R4cc/komando/compare/v0.1.1...v0.1.2
[0.1.1]: https://github.com/R4cc/komando/compare/v0.1.0...v0.1.1
[0.1.0]: https://github.com/R4cc/komando/releases/tag/v0.1.0
