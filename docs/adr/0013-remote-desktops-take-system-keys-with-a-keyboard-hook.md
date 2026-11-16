# Remote desktops take system keys with a keyboard hook

Windows acts on the Windows key, Alt+Tab, Ctrl+Esc, Print Screen and the media and volume keys
before any window's key events see them, so a desktop pane alone can't send them on. Komando
installs a low-level keyboard hook (`WH_KEYBOARD_LL`) on its UI thread the first time a desktop
asks for these keys, and takes them only while a desktop that asked has the keyboard and one of
Komando's windows is in front; every other key, and every key otherwise, passes through untouched.
This is what mstsc and VNC viewers do. The alternative, a "send keys" menu only, can't do Win+E
or Alt+Tab as the user types them.

## Consequences

- Which keys a desktop takes is a per-Host viewer option (Windows key and shortcuts on, media
  keys on, volume keys off by default, as other viewers do), plus "Send" items for one-off keys.
- Alt+F4 and Alt+Space are never taken, so the window can always be closed and moved; injected
  keys (from other programs and Komando itself) pass through.
- A key taken down is taken up as well, even when its modifier was let go first, so neither side
  is left with a key held.
- The hook stays installed once used; it costs a check per key. It is Windows-only; elsewhere the
  options have no effect.
