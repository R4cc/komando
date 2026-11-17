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

- Windows-only for now: in-place updates (Linux and macOS open the release page), running a
  Local Shell as administrator, WSL shells, remembering the vault unlock (DPAPI), Mica, the tray
  icon, the single-instance handoff and the remote desktop keyboard hook (ADR 0013).
- Code keeps the existing pattern: a `#[cfg(windows)]` implementation with a plain
  `#[cfg(not(windows))]` fallback, rather than a platform abstraction layer.
- Linux builds on the oldest supported Ubuntu runner (glibc 2.35) with X11 and Wayland; only
  libxcb and libxkbcommon are linked, and Wayland, Vulkan and fontconfig load at startup. The
  tarball's `install.sh` installs per user into `~/.local`.
- macOS builds for Apple Silicon only, needs macOS 11 or later, and is ad-hoc signed but not
  notarized, so Gatekeeper asks before the first start. GPUI's Mac backend needs its `font-kit`
  feature, or it draws no text. Shortcuts still use Ctrl rather than Cmd, and there is no
  menu bar yet.
- CI builds and tests on Linux for every push. macOS is built only by releases, as macOS runner
  minutes cost ten times as much.
