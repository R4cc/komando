# Linux and macOS builds ship alongside Windows

Supersedes ADR 0001. Komando is no longer Windows-only. Nothing in its core needs Windows:
GPUI, russh, alacritty_terminal, IronRDP and vnc-rs all run on Linux and macOS, and the
Windows-specific parts already have plain fallbacks for the Linux development builds. Every
release attaches a Linux build (`Komando-<version>-linux-x86_64.tar.gz`) and a macOS build
(`Komando-<version>-macos-arm64.dmg`) next to the Windows MSI and portable EXE. Windows 11 stays
the platform Komando is designed and tested on first; the others get the same SSH, SFTP and
remote desktop core, and features built on a Windows API stay Windows-only until someone builds
the equivalent.

## Consequences

- Windows-only for now: in-place updates (Linux and macOS offer an update once their own file is
  attached, and open the release page), running a Local Shell as administrator, WSL shells,
  remembering the vault unlock (DPAPI), Mica and background blur, the tray icon and minimize to
  tray, completion notifications, the app icon switch, copying text with its colours and styles,
  copying a file itself to the clipboard (elsewhere its path is copied), crash minidumps, the
  single-instance handoff and the remote desktop keyboard hook (ADR 0013).
- Code keeps the existing pattern: a `#[cfg(windows)]` implementation with a plain
  `#[cfg(not(windows))]` fallback, rather than a platform abstraction layer.
- Linux builds on the oldest supported Ubuntu runner (glibc 2.35) with X11 and Wayland. Only
  libxcb, libxkbcommon and libxkbcommon-x11 are linked; Wayland, Vulkan and fontconfig load at
  startup. The tarball's `install.sh` installs for one user: the program goes in `~/.local/bin`,
  and the menu entry and icon under `$XDG_DATA_HOME` (`~/.local/share`).
- macOS builds for Apple Silicon only, needs macOS 12 or later (GPUI compiles its Metal
  shaders for 12), and is ad-hoc signed but not notarized, so Gatekeeper asks before the first
  start of each download. GPUI's Mac backend needs its `font-kit` feature, or it draws no text.
- On macOS, Komando follows the platform where Windows conventions would get in the way
  (`src/app_menu.rs`): a menu bar, the common Cmd shortcuts beside the Ctrl+Shift ones, the
  native traffic lights instead of drawn caption buttons, quitting when its window closes,
  Option typing characters (or Meta, a setting), and Cmd as Ctrl in remote desktops.
- CI builds and tests on Windows and Linux for every push, and packs the Linux tarball. macOS is
  built only by releases, as macOS runner minutes cost ten times as much.
