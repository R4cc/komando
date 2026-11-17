# Changelog

All notable changes to Komando are listed here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and versions follow
[Semantic Versioning](https://semver.org/).

## [Unreleased]

## [0.8.0] - 2026-11-17

### Added

- RDP and VNC desktops share plain text with this computer's clipboard. Turn it off for
  a desktop from its toolbar under Keys → Share text with this PC. View only pauses
  clipboard sharing.
- Port forwarding. A host or project form's new Port forwards page sets Local forwards (a port
  on this computer reaches an address from the server's side), Remote forwards (a port on the
  server reaches an address from this computer's side) and SOCKS proxies. They open while the
  host's SSH session is connected. A project's forwards open for each of its hosts, unless a
  host turns them off.
- The top bar shows a connected session's port forwards: whether each is listening, how many
  connections it carries and why one failed. Click a local forward or SOCKS proxy to copy its
  address.
- Forwards listen only on this computer (for a Remote forward, only on the server) unless you
  open one to the network.
- Shared project files carry port forwards, and Settings → Import brings Tabby's forwarded
  ports over instead of leaving them behind.
- Settings → Import → SSH config brings over the hosts in `~/.ssh/config` and the files it
  includes. Each comes with its address, port and user, ProxyJump as jump hosts, and
  LocalForward, RemoteForward and DynamicForward as port forwards, as `ssh` would use them,
  `Host *` defaults and Match blocks on the host's name included. Keys (a jump host's too),
  ProxyCommand, agent forwarding and Match blocks on anything else stay in the file, and the
  review lists them per host.
- The import review lists the port forwards each host brings over.

### Changed

- A thin silver frame with a soft glow surrounds the window and adapts to the app theme,
  replacing the project-colored stripe above sessions.
- While an SSH session connects and for its first five seconds, the connection bars play a
  searching animation instead of showing a reading. Quick checks during that time settle on
  the connection's real latency, so the bars no longer start low and creep up over the
  first minute.
- The connection tooltip says when it is connecting, measuring or waiting for a reply,
  instead of showing "— ms" or an outdated time.

### Fixed

- With "Keep sessions after disconnecting" on, closing a connected remote desktop with its ×,
  Close session in its menu or Delete in the sidebar closes it, instead of only disconnecting
  it and leaving it in the sidebar. Remote desktops aren't kept after disconnecting.

## [0.7.9] - 2026-11-17

### Changed

- The SSH connection tooltip shows only the target's round-trip time in milliseconds.

### Fixed

- The minimap shows highlights across the full width of terminal output, so errors later
  on a line remain visible even when an earlier log prefix says INFO.

## [0.7.8] - 2026-11-17

### Added

- The top bar shows four connection-quality bars for SSH sessions, with smoothed latency,
  measurement age and Jump Host round trips on hover. Checks run every 5 seconds while the
  window is active and every 30 seconds in the background; overdue replies turn the bars
  amber, then red, without shortening the SSH disconnect timeout.
- macOS: a menu bar with About, Settings, Hide and Quit, and the usual Cmd shortcuts beside the
  Ctrl+Shift ones: Cmd+C and Cmd+V copy and paste in the terminal, Cmd+F searches, Cmd+T and
  Cmd+W open and close a session, Cmd+, opens Settings, Cmd+Q quits, Cmd+H hides and Cmd+M
  minimizes. Double-clicking the title bar zooms the window.
- macOS: Settings → Terminal → Use Option as Meta key, for Option+B and Option+F word moves in
  the shell.

### Changed

- Expand sidebar on hover feels like Zen Browser's compact mode: the sidebar opens as soon
  as the pointer reaches the window's left edge and springs in, then slides back 150 ms
  after the pointer leaves it. On Windows, moving past the window's left edge keeps it open
  until the pointer is 200 px beyond it.
- On Linux and macOS, the update prompt says how to install the new version: unpack the Linux
  tarball and run its install.sh, or drag Komando into Applications from the macOS disk image.
- SFTP keeps a file's permissions on Linux and macOS: a downloaded script stays executable and
  a private file stays private, and an uploaded file gets the permissions it has on your
  computer.
- On Linux and macOS, downloaded files keep the names the server gave them, such as `a:b` or
  `CON`. Only Windows renames those.
- On Linux and macOS, settings and menu items that only work on Windows are no longer shown:
  background blur, minimize to the notification area, Run as administrator for local shells,
  and the remote desktop options that take the Windows key, media keys and volume keys from
  this computer. The one-off Send → Windows key item stays.
- On Linux and macOS, examples and hints show paths and shells from those systems instead of
  Windows ones, and Stay unlocked no longer says it lasts across restarts.
- The Linux menu entry lists Komando under System only, as the desktop entry rules ask.
- macOS: the download now says it needs macOS 12 or later, which its graphics already required.
- macOS: Komando quits when its window closes, as on Windows and Linux, instead of staying in
  the Dock with sessions still connected and no window to show them.
- macOS: in a remote desktop, Cmd is sent as Ctrl, so Cmd+C and Cmd+V copy and paste on the
  remote computer. Option types the symbols it gives on your keyboard (`@`, `|`, `€`) there,
  and stays Alt for letters.

### Fixed

- On Linux and macOS, Komando offers an update only once that system's own download is on the
  release page. Before, a Windows file was enough, so the prompt could appear before the Linux
  tarball or macOS disk image was ready.
- The sidebar update button on Linux and macOS says it opens the release page, rather than that
  it will update and restart.
- On Linux and macOS, "Copy output as .txt file" no longer starts the file with an invisible
  byte order mark, and keeps the file in your own cache folder where only you can read it,
  instead of the temp folder every user shares.
- On Linux and macOS, a local shell still opens when `$SHELL` is empty or names a program that
  doesn't exist: Komando starts bash, or `/bin/sh` where there is no bash.
- On Linux, the app menu entry starts Komando when your home folder's path has a space in it.
- macOS: the window's own close, minimize and zoom buttons no longer sit on top of the ☰ menu,
  and Komando no longer draws a second, Windows-style set of them at the top right.
- macOS: Option no longer sends an escape followed by a stray character in the terminal. It
  types the character your keyboard gives it, such as `@` or `|` on European layouts.
- macOS: local shells start in your region's UTF-8 locale when Komando is opened from Finder or
  the Dock, so accented file names and text show correctly. With no `$SHELL`, zsh is started.
- macOS: Komando explains why it asks to reach your local network or to use your Downloads,
  Documents and Desktop folders.

## [0.7.7] - 2026-11-17

### Added

- SSH sign-in works with servers that ask their own questions (keyboard-interactive), such
  as switches and firewalls that check passwords through RADIUS or TACACS+, and servers that
  ask for a one-time code after the password. A plain password question still uses the saved
  password and offers "Remember"; any other question is shown as the server words it.
- A server that takes no password now says which sign-in methods it does take, instead of
  asking for a password it will always reject.

### Fixed

- SSH now connects to Cisco switches and other network gear that only offers older key
  exchanges (NIST-curve ECDH, or Diffie-Hellman group 14, group exchange or group 1 with
  SHA-1), HMAC-SHA1, or AES-CBC ciphers. Komando still prefers modern algorithms whenever the server has them.
- The prompt of a Cisco switch (and other gear that starts with empty lines) now appears at
  the top of the terminal instead of below a gap.
- A long reason for a disconnect, such as a failed key exchange, now wraps inside the
  terminal instead of running off both edges.
- Moving between buttons and other controls with Tab no longer makes them shift slightly,
  for example on Cancel and Continue in the password prompt.

## [0.7.6] - 2026-11-17

### Added

- Hovering a sidebar row with one session turns its status dot into ×, which closes the
  session. A middle-click on a session's row in the sidebar closes it too. Choose which of
  these work in Settings → Appearance → Close from the sidebar.

### Changed

- Rename session… starts with the session's current name filled in, ready to edit.

### Fixed

- When an SSH session disconnects after you signed in, the terminal now says so below the
  last prompt, with "Press Enter to reconnect", and the cursor goes away. A full-screen
  program that was open, such as an editor, no longer stays on screen.
- With background blur on, a dark terminal in light mode (Terminal: Always dark) or a light
  terminal in dark mode no longer looks washed-out gray. It stays solid instead of letting
  the backdrop through.
- Ctrl+Tab no longer gets stuck on a remote desktop that is open in its own window.
- Closing the active session no longer leaves the main window empty when the host's other
  session is a remote desktop in its own window. Opening a host in split view no longer puts
  that desktop's placeholder beside your terminal.
- Disconnect in a remote desktop's own window keeps the desktop so you can reconnect it, as
  in the main window, instead of closing it.
- Pasting text that uses only carriage returns as line breaks now asks first when
  Confirm multi-line paste is on, like any other multi-line paste.
- Pasting into the terminal search bar uses the first line with text, so a copied selection
  that starts with a blank line no longer pastes nothing.
- Moving several files in the file browser says how many moved before one failed.
- Opening a socket, pipe, device or broken link in the file browser says it can't be
  downloaded, instead of doing nothing.
- If `settings.toml` can't be read, Komando keeps it as `settings.toml.broken-…` instead of
  replacing it with defaults the next time a setting changes.
- Import from MobaXterm uses the settings file that lists sessions when MobaXterm left an
  empty one in Documents.
- Settings → Mouse & clipboard no longer shows Copy colors and styles on macOS and Linux,
  where it had no effect. Copying there puts plain text on the clipboard, as before.
- If Keep sessions after disconnecting or Minimize to tray can't be saved, Komando now says
  so, as it does for other settings.

## [0.7.5] - 2026-11-17

### Added

- A Disconnect button next to Reconnect in the title bar ends the connection and keeps the
  session's output, so you can reconnect later. Remote desktops have it too.
- Settings → Appearance → Behaviour → Keep sessions after disconnecting brings back the old
  two-step ×: the first click disconnects, the second removes the session.

### Changed

- Settings → Vault groups saved passwords and keys by project, matching the project and
  host order in the sidebar. Other saved items appear below the projects.
- The × on a session (and Delete or Ctrl+Shift+W) now closes it in one click instead of
  disconnecting it first. A session whose command is still printing output still asks before
  closing. Disconnect is in the session's menu and the title bar.

### Fixed

- The sidebar no longer jumps back to the top when a session opens or closes, such as when
  you click + to open a new session on a host further down the list.

## [0.7.4] - 2026-11-17

### Added

- Komando now runs on Linux and macOS too. Each release has a Linux download (a `.tar.gz`
  with an `install.sh` that adds Komando to your app menu) and a macOS download (a `.dmg` for
  Apple Silicon Macs). One-click updates, running a shell as administrator, WSL shells, staying
  unlocked across restarts, the blurred window background and the tray icon remain
  Windows-only for now; on Linux and macOS the update prompt opens the release page.

### Fixed

- Opening a host's ⋯ menu keeps the sidebar at its current scroll position instead of
  jumping back to the top.
- Expand sidebar on hover no longer draws a solid black strip around the sidebar panels.

## [0.7.3] - 2026-11-17

### Added

- The theme and terminal color menus in Settings → Appearance show each entry as you move
  through them with the arrow keys or the pointer, so you can see it on the preview and in
  the window. Escape or clicking away puts the saved one back.
- A ☰ menu at the top left: Quick connect, New project, Import, switches for the sidebar,
  Expand sidebar on hover, the minimap and the command rail, Presentation mode, Lock vault,
  Settings, the welcome tour and About.
- Presentation mode (Ctrl+Shift+P or the ☰ menu) makes the terminal text larger for people
  watching your screen, fills the screen and hides the sidebar. A "Presenting" button in
  the title bar ends it. Its font size and what it hides are in Settings → Appearance.
- With the minimap off, the terminal has a plain scrollbar on the right: drag it, or click
  above or below it to jump there. Search matches are marked beside it.

### Changed

- The button that hides the sidebar is now beside Quick connect at the top of the sidebar.
  While the sidebar is hidden, a button next to the ☰ menu brings it back.
- `clear` (and Ctrl+L) now clears the screen and its scrollback: scrolling up no longer
  shows the old output. It isn't lost: scroll up after a clear and a Restore scrollback
  button at the top brings it back. Search, the minimap and Copy output leave it out while
  it's hidden. A terminal reset (`reset`) still erases it.

## [0.7.2] - 2026-11-17

### Added

- Right-click the minimap to hide it. Settings → Terminal turns it back on.
- A Reconnect button next to SFTP above the terminal starts a new connection for the
  session in the same terminal, keeping the earlier output above it.

### Changed

- The bar above the terminal no longer has Command history, Copy output and Copy as .txt
  buttons. Right-click the terminal to copy its output, or right-click a host for its
  command history.
- Hosts sit further in under their project in the sidebar, and the highlight on the open
  host starts inside the project's, so it's clearer which project a host belongs to.
- The host and project forms are split into pages: General, Sign-in, Jump hosts, Terminal
  and, for hosts, Remote desktop. The list on the left shows how many jump hosts and
  startup commands are set, and the Jump hosts page shows the way from this PC to the host.
  When something can't be saved, the form opens the page with the field to fix.
- The project form says, under its default user, key or password, jump hosts and startup
  commands, how many of its hosts use them and which hosts set their own.

### Fixed

- The theme settings in Settings → Appearance line up with the rest of the page.

## [0.7.1] - 2026-11-17

### Added

- Highlighting can be switched off: for everything under Settings → Highlights, or for a
  single host or project with the new Highlight output switch in its form. Full-screen
  programs such as vim and htop can now be highlighted too; that stays off by default.
- A Vault page in Settings lists every saved password and key once the vault is unlocked,
  with what signs in with each. Show reveals a password or key, Copy puts it on the
  clipboard for 30 seconds, and Delete removes it; hosts that used it ask for a password
  instead.
- A Mouse & clipboard page in Settings, with the copy and paste switches that were under
  Terminal and new ones. Your PuTTY-style clipboard choice carries over. Copied text now
  leaves out spaces and blank lines at both ends; a switch turns this off. New settings:
  - What a right-click does: open the menu, paste, or copy a selection and otherwise paste.
  - Paste with a middle-click.
  - Which characters end a word when you double-click.
  - Open links with Ctrl+click, Alt+click or a plain click.
  - Copy text with its colors, so it keeps the terminal's look when pasted into documents,
    mail or chat.
  - Paste text as one line.
  - Turn bracketed paste off for programs that show stray characters around pasted text.
- Restore tabs on start (Settings → Appearance → Behaviour): Komando reopens the terminal
  tabs that were open when it closed, with the names you gave them. Hosts that sign in with
  a saved password wait for the vault to be unlocked. Remote desktops aren't reopened.
- Font weight and Bold font weight (Settings → Terminal) set how thick terminal text and
  bold text are drawn, from 100 to 900. Reset puts each back to the regular 400 or bold 700.

### Changed

- While files transfer, the window redraws only when the transfer ring's percentage
  changes or the transfers panel is open, rather than four times a second.
- The sidebar, quick connect and icon search do less work each time the window is drawn,
  which keeps Komando responsive with many hosts. Quick connect works out its matches when
  you type instead of on every redraw.
- Dragging the blur or font size slider no longer restyles the window when the value
  hasn't changed.
- Remote desktops copy only the parts of the screen that changed, and the connection no
  longer waits while the whole picture is copied, so busy desktops keep up better. VNC at
  reduced colors no longer allocates a buffer for every update.

## [0.7.0] - 2026-11-16

### Added

- Drag Project headings up or down in the sidebar to change their order. A drop indicator
  shows the new position, and the order is remembered after restarting Komando.

### Changed

- Scrolling terminal output reuses Highlight Rule results for rows that only moved, with a
  bounded cache that refreshes when the rules or dark/light background change.
- The minimap reuses its picture while output is unchanged, avoiding repeated scans of its
  terminal cells when the cursor blinks, the pointer moves or a selection changes.
- Heavy SSH output releases the terminal lock between smaller pieces so drawing and input
  can take their turn, while keeping UTF-8, escape sequences and synchronized updates intact.
- VNC moves rectangles within its picture without allocating and copying a temporary image.
- Settings → Appearance is reorganised. It shows the Theme and Terminal Colors of the mode
  in use, beside a live preview of the window and terminal; following Windows, the other
  mode's can be edited too. Mode and App icon are picked from pictures, the theme and
  terminal color menus show a miniature of each choice, and the window and sidebar
  settings have their own sections.
- Settings pages are easier to scan: explanations that only repeated a setting's name are
  gone, and the rest are shortened to what you need to know.

### Fixed

- With background blur on, the terminal now lets the blurred desktop show through as the
  intensity slider says. Its background was drawn twice, which kept it almost opaque even
  at 100%. The minimap opened over the output stays opaque so both remain readable.
- Minimap colors refresh when switching Terminal Colors within the same mode or when a
  program changes the terminal palette.
- Enter confirms the multi-line paste question, just like its Paste button, without sending
  an extra Enter to the terminal underneath.
- Pasted text can no longer end bracketed paste early by hiding a nested end marker, which
  could make the shell run the rest of a paste as typed commands.
- Taking longer than 15 seconds to check a new or changed Host Key no longer makes the
  connection time out after you click Trust.
- Clicking in the empty bars beside a fitted remote desktop no longer clicks the edge of the
  remote screen, and moving over them no longer drags the remote pointer to that edge.
- Sideways touchpad scrolling on a remote desktop is no longer lost when the gesture also
  moves slightly up or down.
- A Windows key released after switching away from Komando no longer turns later letters on
  a remote desktop into Windows shortcuts.
- Replacing a remote file or folder by upload keeps the old one until the new one is in its
  place, so a dropped connection at the last moment can't lose both.
- Replacing a remote file with an empty folder no longer gives the folder the file's
  permissions, which could leave it impossible to open.
- Copying a remote folder into one of its own subfolders is refused instead of copying
  itself over and over on servers without `cp`.
- In the File Browser, Shift+Up and Shift+Down now shrink the selection again when moving
  back towards where it started.
- Renaming a remote file without changing its name no longer removes spaces at the start or
  end of the name.
- Switching off RDP or VNC in Edit host only hides it from the host's menus, as described.
  Its port, account and saved password are kept for switching it back on.
- A saved key or password replaced in Edit host or Edit project, or no longer used, is
  removed from the vault instead of lingering there.
- Typing, pasting and Ctrl+Shift+V no longer reach the shell, or replace the question, while
  a Host Key or multi-line paste question is open.
- A link's underline goes away when the pointer leaves the terminal or output moves the link.
- The File Browser's path bar shows the current folder after Back, Forward or Up, F2 says
  when a folder can't be renamed in instead of failing afterwards, and an upload that
  finishes while a folder loads appears without refreshing by hand.
- Sizes just under 1 MB, 1 GB and so on show as "1.0 MB" rather than "1024 KB".
- Moving files to the folder they are already in says so instead of doing nothing.
- "Pick all" in the import list leaves out hosts already in Komando, as the first selection
  does.
- "Click again to delete" in Command History resets after a few seconds, so a much later
  click no longer deletes the history without asking again.
- A form's error message goes away as soon as you start correcting it.
- Clearer wording: singular counts ("1 line", "1 rule", "1 host goes"), a matching count in
  Disconnect all, the delete question for a Project without hosts, the project password
  hint, and a note that the RDP fit setting applies from the next connection.
- Ctrl+Shift with Enter, Backspace, Delete, Insert, Space or an F-key reaches the program in
  the terminal instead of being dropped; only Ctrl+Shift+ a letter or symbol is kept for
  Komando's shortcuts.
- Ctrl+Shift+W on a connected remote desktop asks before ending it, since programs on the
  remote side often use the same keys.
- AltGr characters (such as `@` on a German keyboard) type correctly on remote desktops,
  including over VNC and with a different keyboard layout on the remote side.
- With "Windows key and shortcuts" off for a remote desktop, the Windows key no longer opens
  the Start menu on the remote side as well as on this PC.
- A Ctrl, Alt or Shift held down before clicking into a remote desktop now applies to the
  next key, so Ctrl+C no longer arrives as a plain C.
- After F2 or Delete in the sidebar opens a dialog, closing it returns the keyboard to the
  sidebar instead of the terminal.
- In the File Browser, Enter confirms the delete question, Escape while renaming a favorite
  ends only the rename, and Enter on a selection of only folders explains that Download
  handles folders.
- A wrong VNC password on current VNC servers asks again, with the server's reason, instead
  of ending the session. Servers that offer only sign-ins Komando doesn't support (Apple,
  VeNCrypt) say so plainly.
- Touchpad scrolling on a VNC desktop scrolls as far as the gesture, instead of a whole
  notch for every small step.
- A VNC server that changes its screen size on its own (such as wayvnc) keeps showing the
  whole desktop.
- RDP says why Windows refused a sign-in when it isn't the password: a locked or disabled
  account, an expired password, or an account not allowed to use Remote Desktop. It no
  longer asks again for a password in those cases.
- An RDP server that accepts the connection but never answers stops trying after 30
  seconds instead of showing Connecting forever.
- A user name such as `ada@corp.example` signs in to RDP even when the host has a domain set.
- "The server closed the connection" replaces "not enough bytes" when an RDP server hangs up,
  and a slow SSH tunnel is no longer reported as an ended SSH session.
- A remote desktop that goes through SSH connects when the host's SSH session starts from
  another Project the host is in.
- A remote desktop connecting behind another session no longer takes the keyboard for its
  password question; the question gets the keyboard once the desktop is shown.
- Escape cancels a remote desktop's sign-in or certificate question in its own window too.
- Clicking beside a fitted remote desktop no longer moves the remote pointer to its edge
  when the button is released.
- Reconnecting a remote desktop in its own window on another monitor asks for the size and
  scale of that window.
- Choosing "Not now" at the vault's unlock after ticking Remember says the password wasn't
  saved.

### Security

- A host imported into one of your Projects that has a saved password or key no longer uses
  it: Komando asks for that host's password instead. A shared or imported file could
  otherwise point a host at its own server and receive the Project's password.
- The import review shows a new Project's own Jump Hosts and Startup Commands, and every
  Startup Command in full, so nothing that runs on connect can hide behind an ellipsis.
- Password questions name the machine they are for (`user@address`), so a Jump Host can't
  pass for another one.
- A server presenting a key of a new type for a host that already has a trusted key is no
  longer shown as a first connection: the question says which key type is already trusted.
- Startup Commands that answer a sudo password prompt in any language sudo uses are kept out
  of the library and shared Project Files, not only English and German ones.
- Updates are downloaded over HTTPS only, even through redirects, and the installer is
  checked again just before Windows Installer runs it. Only GitHub release pages are opened
  from the update notice.
- An RDP server can no longer make Komando reserve memory for an impossibly large desktop
  and close it, and SFTP refuses to download devices such as `/dev/zero` that never end.

## [0.6.6] - 2026-11-16

### Changed

- Disconnecting a remote desktop yourself removes its sidebar entry immediately. If the
  host or network disconnects it, the entry remains for reconnecting, with its last picture
  strongly blurred beneath the disconnected message.
- Remote desktop toolbars now have a window button: open the desktop in its own window,
  or bring it back to Komando, without reconnecting.
- On Windows, a dragged remote desktop row stays visible beside the pointer outside
  Komando's window, until it is dropped or the drag is cancelled.
- Sidebar menus put New terminal, Browse files and Open in split view first, omit Show
  session when it is already visible, and use shorter desktop labels (Open VNC, Set up RDP…).
- Sidebar menus show keyboard shortcuts beside New terminal (Ctrl+Enter), Rename session
  (F2), and Disconnect or Close session (Delete).

### Fixed

- Collapsing the sidebar with a large Scrollback no longer waits for every terminal row's
  wider allocation on the UI thread. The sidebar responds while the terminal prepares its
  new width in the background.
- Resizing the SFTP File Browser updates a terminal with a large Scrollback more promptly:
  nearby widths share preparation, blank line endings avoid temporary allocations, and
  slow resizes no longer cause a long extra redraw delay.

## [0.6.5] - 2026-11-16

### Changed

- Host and local shell rows now show only + and ⋯ on hover. Disconnect and Close session
  remain in their right-click and ⋯ menus; individual session rows keep their × button.
- RDP and VNC are offered in every host's right-click and ⋯ menus by default, including
  existing hosts. Choosing an unconfigured protocol opens its port and password settings.
  Switch a protocol off in Edit host → Remote desktop to hide it from those menus.

### Fixed

- Komando closed when a sidebar row's ⋯ button (or Shift+F10) opened its menu while Windows
  notifications were available.

## [0.6.4] - 2026-11-16

### Added

- When Komando crashes, it now says why in its log (`logs\komando.log` in its data folder) and
  saves a crash report beside it (`komando-crash-….dmp`, the last three are kept). Send these
  along when reporting a crash.
- Shift+F10 (or the Menu key) opens the menu of the sidebar row you're on, so everything in
  it, such as opening a remote desktop or duplicating a host, works from the keyboard.
- Shift+F2 and Shift+Delete in the sidebar edit or delete the host.

### Changed

- Dragging the window around no longer redraws everything in it at every step, so the drag
  stays smooth even with busy terminals open.
- Resizing the window by an edge redraws at the new size right away. The content no longer
  stretches and then catches up with the edge.
- Komando no longer uses CPU redrawing its window while it is minimized or in the tray.
- In the sidebar, F2 and Delete act on the session a row stands for: they rename and
  disconnect it on a host's row as on its sessions' rows. On a host without sessions, or a
  project, they still edit or delete it.
- A host row offers only the buttons that leave its name readable: in a narrow sidebar the
  remote desktop button goes first (it is also in the ⋯ menu), then ×. A host whose click
  opens its desktop no longer repeats that in a button until it has a session.
- The ⋯, +, × and remote desktop buttons now share one size and style. The remote desktop
  button shows the same RDP or VNC tag as the desktop's session row, and × turns red under
  the pointer.
- Session rows always show the session's number, which no longer gets cut off in a narrow
  sidebar, and drop the SSH tag: the terminal icon already says it. In a narrow sidebar ×
  floats over the row's end instead of taking room from the title.
- A disconnected session's title is dimmer than a connected one's.
- The keyboard cursor in the sidebar shows only once you use the keyboard, no longer moves
  the row's content, and shows the row's buttons. A folded project holding the session
  you're in has a bar in its colour, instead of a highlight that looked like hovering it.

### Fixed

- Komando could stop responding ("Komando is not responding") as soon as the pointer moved over
  a terminal, for example right after connecting to a host or after closing the host editor
  with Esc. Ctrl+clicking in a terminal could do the same. This came with clickable links in
  0.6.3.
- In the sidebar, a second Delete while "Disconnect …?" was asking disconnected the session
  without waiting for the answer. Sidebar keys now do nothing while a question is open.
- Disconnect and Disconnect all in the menus now also ask first while a command is still
  printing output, like × and Ctrl+Shift+W.
- A renamed session now shows its name in the sidebar wherever it is listed: after the host's
  name on the host's row, and on a remote desktop's row. A remote desktop in its own window
  shows it in that window's title.
- Session numbers are no longer given out again after closing the highest one; they start
  at 1 again only once all of a host's sessions are closed.
- A host's row is lit for its terminal shown beside another session in split view, also
  while its remote desktops are listed under it.
- The project row's + button sat on a patch of a different colour.

## [0.6.3] - 2026-11-16

### Added

- Web links in terminal output are clickable. Hovering one that starts with `http://`,
  `https://` or `ftp://` underlines it; Ctrl+click opens it in your browser, and the
  right-click menu can open or copy it. Links programs mark themselves work too. Plain IP
  addresses and host names in logs stay plain text.
- The sidebar works from the keyboard. Ctrl+Shift+H moves into it (and back out). Use the
  arrows to move, fold and unfold, Enter to connect, Ctrl+Enter to open another session, F2 to
  edit a host or project or rename a session, Delete to delete or close, and Esc to return.
- A ⋯ button on each host and shell row opens the same menu as a right-click, including
  **Edit host…**.
- **Rename session…** in a session's menu gives it a name of its own, shown in the sidebar and
  the title bar.
- **Browse files** in a connected host's or session's menu opens its file browser.
- Ctrl+Enter in quick connect opens another session on the chosen host or shell.
- Disconnecting a session with × or Ctrl+Shift+W asks first while a command is still printing
  output.
- **Restart** in the tray icon's right-click menu closes Komando and opens it again.

### Changed

- With **Minimize to tray** on, the tray icon now shows the whole time Komando is open, not
  only while its window is hidden.
- In split view, a thin line now runs down the gap between the two sessions, so they stay
  clearly apart on dark themes.
- When a server's host key no longer matches the one you trusted, the warning now shows the
  trusted and the new fingerprint and offers **Replace key and connect**, instead of only
  blocking the connection. Replacing trusts the new key from then on; a key trusted in your
  OpenSSH known_hosts is replaced in Komando only, and that file is left unchanged.
- Opening a host's remote desktop no longer moves its terminal into a row of its own. The
  host's row stays its SSH session, and the remote desktop is the only row listed under it.
- A host row shows fewer buttons on hover: a labelled RDP or VNC button for the desktop the
  host opens first, + for a new terminal, ⋯ for the menu, and ×. The connection dot stays where
  it is, and every button has a tooltip. A narrow sidebar shows only + and ⋯.
- Session numbers count up and are no longer reused while a host has sessions open. The title
  bar names a second session "web01 #2", as the sidebar does, instead of "web01 (2)".
- With a host's sessions unfolded, the host's row no longer repeats their state and count.
- Quick connect unfolds the project of the host you open and scrolls the sidebar to it. A
  folded project holding the session you're in is highlighted.
- The sidebar's Downloads entry is now called Transfers, since it lists uploads too.
- The host form's hints now point to the sidebar for opening remote desktops.

### Fixed

- With Mica on, a maximized Komando could stay behind the Windows taskbar, for example after
  switching the taskbar from auto-hide to always shown. It now fits above the taskbar, and
  refits when the taskbar is shown, hidden or moved.
- The remote desktop toolbar's keyboard button is now labelled "Keys" and its menu arrow no
  longer spills out of the button.
- On a host whose click opens a remote desktop, +, Ctrl+click, New session and Ctrl+Shift+T
  only showed the desktop again. They now open another terminal.
- Dragging a session's row onto another project moved its whole host, with all its sessions,
  into that project. Session rows now only open in split view.
- The welcome tour pointed to RDP and VNC buttons in the title bar, which no longer exist.

### Removed

- The Close session button in the title bar. Close a session from its sidebar row, its
  right-click menu, or with Ctrl+Shift+W.

## [0.6.2] - 2026-11-16

### Added

- A welcome tour on the first start. It sets the options worth choosing straight away (theme,
  command history, copy on select, multi-line paste, the notification area and update checks),
  creates your master password, and shows how to import hosts from Tabby, MobaXterm or a
  project file, what the minimap does, how to set up RDP and VNC desktops, and a few more
  features. Skip any step or the whole tour; replay it from Settings → About.

### Fixed

- Restored the supplied serif “Komando.” wordmark in the app and README, without the
  terminal symbol on its left.
- README performance charts follow GitHub's selected theme and keep their labels readable
  on an opaque background, including when the system theme differs from GitHub's theme.
- Projects remember whether they were expanded or collapsed when Komando relaunches.
- Importing no longer marks a host as a duplicate just because another host has the same
  address and user. A host is only flagged when its name, address, port, user, jump hosts and
  startup commands all match, so profiles you duplicated and then changed are picked as new.
- An encrypted Tabby configuration now gets a clear explanation with the steps to switch
  encryption off in Tabby and a Try again button, instead of a generic error.
- The Review and Import buttons stay visible below the list while you scroll through a long
  import.
- Long host names on the import page are cut short instead of running into the duplicate
  labels.

## [0.6.1] - 2026-11-16

### Added

- New dark and light app icons. Choose either in Settings → Appearance, independently of
  the app theme; the choice is remembered and updates the taskbar and notification area.
- Remote desktops have a toolbar above the picture. It shows the protocol, the remote
  screen's size and the zoom, and has:
  - **Scaling**: fit to window, actual size (100%, panning as the pointer nears an edge) or
    stretch to fill. RDP can also resize the remote desktop to the window.
  - **Picture quality**: High, Medium or Low. VNC sends fewer colors; RDP leaves out wallpaper,
    animations and other visual effects. Changing it reconnects the desktop.
  - **Keyboard**: send Ctrl+Alt+Del, the Windows key, Ctrl+Shift+Esc or Print Screen, and
    choose whether the Windows key and system shortcuts (Alt+Tab, Ctrl+Esc), media keys
    and volume keys go to the remote desktop instead of this PC.
  - **View only**: watch without sending keyboard or mouse input.
  - **Full screen**: the desktop fills the screen; the toolbar slides in from the top edge.
  Each host remembers its choices.
- A thin outline marks where the remote picture starts and ends.
- Highlight rules can be edited in Settings → Highlights. Click a rule to change what it
  matches (text or a regular expression, case, a capture group), its text colour, background
  and bold, with a live preview on a line of your own output. Add rules, move them up or down
  (the first match wins), delete them, and create or rename sets, without editing JSON.

### Changed

- Refreshed the Komando wordmark and README branding and screenshots.
- Ctrl+Alt+Del moved from the title bar into the desktop's toolbar, under Keyboard.
- Session rows under a host keep their SSH, VNC or RDP tag and state visible on hover; the
  close button has its own place at the end of the row.

### Fixed

- Hovering a menu item highlights it clearly in every theme. In Komando Default's dark mode
  the highlight was darker than the menu itself, so it was hard to see which item was under
  the pointer.
- Pressing Space on a password prompt's "Remember" checkbox ticks it and keeps the focus
  there. Before, it ticked and immediately unticked the box and jumped back to the
  password field.

## [0.6.0] - 2026-11-16

### Added

- Nine themes with light and dark variants: Komando Default, Sage Green, Noctua,
  AstroVista, QraftHive 1.0, Gold?, Mint Theme, FL-Theme and Salesforce. Each includes
  matching app and terminal colors, UI and terminal fonts, corner rounding and shadows.
- Terminal colors are chosen separately from the app theme, with their own dropdowns for
  dark and light terminals: Default (VS Code's standard terminal palette), Campbell,
  One Half, Solarized, Tango, Gruvbox, Dracula and Nord, or Match theme to use the app
  theme's terminal colors.
- Expand sidebar on hover (Settings → Appearance): the sidebar stays hidden and slides out
  over the terminal when you point at the window's left edge, like a browser's vertical
  tabs. It slides back once the pointer moves away; Ctrl+Shift+B still pins it open.
- Remote desktops can run in a window of their own: drag a VNC or RDP session's sidebar row
  out of Komando's window, or use "Open in its own window" in the title bar or the row's
  menu. Closing that window, or "Back to the Komando window", returns the desktop to the
  main window, still connected.
- A host's sidebar row shows a monitor button on hover for each remote desktop it offers.

### Changed

- Remote desktops appear as their own row under the host, marked VNC or RDP. Session rows
  under a host are smaller, hang from a guide line and show whether they are SSH, VNC or
  RDP, so they no longer look like hosts.
- The title bar no longer has SSH, VNC and RDP buttons, so it keeps its layout when you
  switch sessions. Switch between a host's terminal and desktop in the sidebar.
- New VNC desktops connect directly to the host by default, like other VNC viewers.
  Turn on "Through the host's SSH session" to tunnel VNC over SSH when the server
  allows port forwarding.
- Komando Default now uses flat black and white surfaces in dark mode and neutral
  light surfaces with a mint-green primary color in light mode. It uses Inter,
  JetBrains Mono, an 8 px base radius and no shadows.
- Theme previews show four adjacent color blocks in a narrower selector.
- Refined each theme's light and dark palettes with clearer surface hierarchy, readable
  secondary text and consistent accents. FL-Theme now has a true dark variant; Mint and
  Gold retain their signature colors in both modes.
- Buttons and controls use restrained rounding, with a 6–8 px base radius, and clearer
  hover, pressed and selected states.
- Theme selectors are dropdowns with names and palette swatches, hover highlighting
  and keyboard navigation.
- The terminal follows the theme's font unless a custom font is set. New settings use
  solid backgrounds, and removed theme selections fall back to Komando Default.
- The terminal uses the Default terminal colors unless you choose others, including after
  updating. Choose Match theme to keep the colors derived from the app theme.

### Removed

- The previous theme collection (Ember, Midnight, Ivory and the others). Saved selections
  switch to Komando Default.
- The tagline on the welcome screen and in Settings → About.

### Fixed

- In password prompts, Tab now stops on the "Remember" checkbox and Space ticks it, in
  terminal and remote desktop sign-ins alike.
- The window border and top edge use the theme's colors instead of the Windows accent
  color when "Show accent color on title bars and window borders" is turned on.
- Maximized windows can be dragged from the very top edge when background blur is enabled.
- The About icon has smoother rounded edges at different display
  scales. Its outer rim is trimmed and transparent edges use higher-quality resizing
  without dark or bright fringes.

## [0.5.5] - 2026-11-15

### Changed

- Restored the borderless, theme-colored text logo inside the app, including the welcome
  screen, empty session screen and About page. The sunrise app icon is retained.
- Default PowerShell and Command Prompt icons use Tabler outlines. WSL distributions use
  matching monochrome Simple Icons marks with theme-aware accents. Shell icons are clearer
  at 16 px and stay visible while idle; the new symbols are also available in icon search.
- The locked Vault indicator sits above Downloads, with its unlock popover opening upward
  beside it.

### Fixed

- The locked Vault label stays on one line in narrow sidebars, and secondary hints hide
  when needed so the Quick connect shortcut and Unlock text do not crowd the controls.

## [0.5.4] - 2026-11-15

### Changed

- New sunrise terminal icon throughout Windows, including the taskbar, tray,
  notifications, Start menu, installer and Settings → About, with matching favicon assets.
- New full-color Komando logo on the welcome screen, empty session screen, About page
  and README. Excess background is trimmed and exterior corners are transparent.

## [0.5.3] - 2026-11-15

### Added

- A PolyForm Noncommercial 1.0.0 license for Komando-authored code, permitting copying,
  modification and redistribution for noncommercial purposes. Third-party licenses remain
  applicable to their respective components.
- Full-color Devicon artwork for technology and Linux distribution icons, searchable alongside
  the existing icons in Project and Host forms and the Local Shell icon selector.
- Registered WSL 1 and WSL 2 distributions appear among detected Local Shells with matching
  distribution artwork. Local Shell icons can be selected manually or left on Automatic.
- Right-click a Host or terminal Session receiving output and choose **Notify when done**
  for a one-time Windows notification after four seconds of quiet. Click the notification
  to restore Komando and show that exact Session. Choose **Cancel notification** to stop
  waiting; disconnecting or closing the Session also cancels it.

### Changed

- Clipboard confirmations are concise and no longer suggest apps to paste into.
- Add and edit Local Shells in a focused dialog, keeping the Settings list compact and
  Save and Cancel visible while choosing an icon.
- Background hosts, sessions and collapsed projects show a small rotating green ring while
  receiving terminal output, replacing the static pulse icon. Visible and quiet sessions
  keep the normal connected dot. Session icons remain on the left; status indicators stay
  on the right and remain visible just before the close button on hover.
- The minimap emphasizes highlighted words at their original positions, with slightly wider
  failure matches and equally thick informational matches. Dense errors stay within their
  own rows so other log levels remain visible.
- Failure highlights use vivid red and warnings use bright amber so they are easier to spot
  in the terminal and minimap. Untouched default rules update automatically; customized
  highlight sets keep their colors.
- The README now focuses on host navigation, SSH logs, SFTP and host-linked remote desktops,
  with fresh dark-mode app captures. Benchmarks live with the performance details.

### Fixed

- The minimap's visible-area selection has rounded, inset corners that fit the terminal
  panel, including at the top and bottom of a full-height minimap.
- Ordinary text stays faint in the minimap on tinted themes, so error and warning
  highlights stand out from surrounding output.
- Launching Komando again restores the running window, including from the tray, instead
  of starting another copy with another tray icon. Administrator shell windows remain
  separate and close normally without adding notification icons.

## [0.5.2] - 2026-11-15

### Added

- Resize the sidebar by dragging the gap beside the terminal. The divider highlights on
  hover, and Komando remembers your preferred width between launches.
- The experimental MobaXterm importer now brings over direct RDP and VNC sessions, including
  their ports and RDP accounts, and opens each with its original protocol. The review identifies
  desktop sessions and lists preferences that stay behind. Desktops using an SSH gateway are
  left out with an explanation; passwords are never imported.

### Fixed

- MobaXterm imports preserve equals signs in startup commands, such as shell variable
  assignments. Sessions without a startup command no longer run an existing Project's commands.
  Mixed-case escape markers also work in commands and Jump Host lists.
- Timed vault unlocks now survive Komando restarts on your Windows account. Time with
  Komando closed counts toward the selected inactivity period; Lock now clears the saved
  unlock, and Until restart still locks when Komando closes.
- Short-lived local shells reliably report that they have exited, even when they finish
  before Komando's session event loop starts listening.

## [0.5.1] - 2026-11-15

### Changed

- Downloads stays visible in the sidebar, even before the first transfer or after the list
  is cleared. Its corner popup is wider and taller, with larger details and wrapping file
  names and messages.

### Fixed

- Starting and closing Local Shells releases their Windows console, pipe and process
  resources, including when the shell cannot start. A Windows console creation failure
  is reported as an error instead of crashing Komando.
- Portable updates use a separate download for each attempt. Opening another Komando
  window no longer removes an update being downloaded or waiting to be installed.
- With Minimize to tray enabled, the window's X button and Alt+F4 now send Komando to the
  notification area and keep sessions running. Exit in the tray menu still closes Komando.

## [0.5.0] - 2026-11-15

### Added

- RDP and VNC remote desktops inside Komando. Enable either in a host's Remote desktop
  section, then open it from the toolbar beside SFTP or the host's right-click menu. Each
  desktop has its own session, port and vault password; a host can open its desktop by default.
  Connect directly or through the host's SSH session, with VNC set to use SSH by default.
- Settings → Remote desktop controls RDP resizing and VNC sharing. Desktops scale down to
  fit the pane, forward keyboard and mouse input, and reconnect in place. RDP also has a
  Send Ctrl+Alt+Del action and checks server certificates before sending credentials.
- Initial desktop support covers RDP with TLS or NLA/NTLM, and VNC with no authentication
  or classic VNC passwords. Audio, clipboard and drive redirection, RD Gateway, Kerberos and
  VNC TLS/VeNCrypt are not supported yet; use SSH to protect classic VNC traffic.
- Optional Windows-style keyboard selection in Settings → Terminal: Ctrl+A selects the current
  recognised command, then all output on a second press; Ctrl+Shift+A selects all output.
  Shift+navigation selects text, Ctrl+Shift+Left/Right selects words, and Esc clears the selection.
  Off by default to preserve shell shortcuts; full-screen programs keep their keys.
- Cursor shape and blinking controls in Settings → Terminal. The default is a blinking line;
  terminal programs can still request their own cursor style.
- Experimental: import your SSH sessions from MobaXterm in Settings → Import. Komando reads MobaXterm.ini
  from Documents\MobaXterm or %APPDATA%\MobaXterm, or a file you exported from MobaXterm's
  sessions. Session folders become projects, an SSH gateway becomes the host's jump hosts and
  an execute command becomes a startup command. Passwords and keys stay in MobaXterm; the
  review lists, per host, a private key or proxy that doesn't come over, and how many RDP,
  VNC or other non-SSH sessions were left out. MobaXterm's file format isn't documented, so
  check the review before importing.
- Right-click a local shell, such as PowerShell or cmd, and choose Run as administrator. After
  you approve the Windows prompt, the shell opens in a new Komando window with administrator
  rights, and your current window keeps running as it was. That window shows an amber
  Administrator badge in its title bar, and every shell you open there runs as administrator.
- A "Stay unlocked" switch in the master password box. On, the vault stays unlocked for the
  auto-lock time set in Settings → Security; off, it locks again as soon as the host has signed
  in or the password is saved. Komando remembers your choice.

### Changed

- The logo above the sidebar is now a toggle: hide or show navigation with the button or
  Ctrl+Shift+B, giving the active session more room when it is hidden.
- The minimize, maximize and close buttons now look like those of other Windows 11 apps: flush
  with the top edge of the window, with the close button in the corner, instead of rounded
  buttons set in from the edge.
- The master password no longer covers the window. While the vault is locked, a "Vault locked"
  row sits at the top of the sidebar; click it, or open a host that needs the vault, and the
  password box opens beside it. The rest of the window keeps working while it's open, and the
  row goes away once the vault is unlocked.

## [0.4.4] - 2026-11-14

### Added

- Drag a host up or down in the sidebar to change its place within its project. A blue line
  shows where it will go; drop it on the project's name to put it at the top.
- Right-click a highlighted word in a terminal and choose Unhighlight to turn off the rule that
  colours it. The rule is switched off, not deleted, so you can turn it back on in
  Settings → Highlights.

### Changed

- Highlight rules no longer colour the command you are typing, or commands you entered
  earlier. The prompt and the output are still highlighted.

## [0.4.3] - 2026-11-14

### Added

- Command history for each host: every command you enter in a host's SSH sessions is kept on
  this PC with its date and time, so you can see what *you* did there even after others have
  used the server. Open it from the clock button above the terminal or by right-clicking the
  host. Filter it, click a command to copy it, or copy the history as text or as a .txt file.
  If you change a host's address, older commands stay listed under the address they ran on.
  Commands typed after an extra space are left out, as in bash. Turn it off, or delete it,
  in Settings → Security; switched off, nothing is recorded. Deleting a host deletes its history.
- A small Caps Lock warning appears in focused password and passphrase fields, including
  the vault and jump hosts, and beside recognized password prompts in the terminal.
- Quick connect: Up and Down move through the results and Enter opens the selected one, so a
  host no longer has to be the first match to open it from the keyboard.
- Hovering a highlight rule in Settings shows its whole pattern when it is cut short.

### Changed

- The Highlights page uses Komando's own terminology without naming another application.
- The default highlight set now uses a calmer, consistent palette and more precise matches,
  with added RHEL coverage for SELinux, DNF, subscriptions, NetworkManager and Red Hat
  advisories. Normal service exits and zero-error counters no longer look like failures.
  Untouched saved defaults update automatically; customized sets are preserved.
- Coloured output is taken in faster: Komando reads a whole colour code at once instead of one
  character at a time, up to a tenth less time for a million coloured log lines.
- Fast output fills a large Scrollback sooner: the memory for new Scrollback lines is prepared
  in the background while output arrives, instead of in between reading it. With a
  100,000-line Scrollback, the first 100,000 lines of a flood are taken in about twice as fast.
  Nothing is held in reserve once the Scrollback is full or the output stops.
- Quick connect shows a host's port when it isn't 22, and finds hosts by port or by a pasted
  `user@address:port`.
- Clicking beside an open host or project form no longer closes it and loses what you typed.
  Close it with Cancel, the close button or Escape.
- Quick connect's message for no results says what can be searched, instead of always
  pointing to adding a host.
- Fast output with a large Scrollback is taken in faster. Komando loads the oldest Scrollback
  line it is about to reuse ahead of time, and takes in all output that has already arrived at
  once rather than a few kilobytes at a time. A million coloured log lines over SSH with a
  100,000-line Scrollback arrive in about 30% less time.
- Connections now prefer AES-GCM encryption when the server offers it: on current PCs it takes
  several times less work to decrypt than ChaCha20-Poly1305, which stays the choice for servers
  without it.
- Highlight Rules with word boundaries (`\b`) no longer slow every frame down while lines with
  accented, Cyrillic or CJK text are on screen. A word boundary now works as in JavaScript and
  Tabby: only ASCII letters, digits and `_` count as word characters.
- The minimap no longer holds up output while it works out the colours of new lines.

### Fixed

- The command rail stays hidden while all output fits in the terminal window.
- PowerShell and Command Prompt icons use artwork made for small sizes, selected for the
  display's scaling, so they remain clearer at the same sidebar size.
- The command rail no longer mistakes log output for a command when Enter is pressed in a
  pager or a pending command's line has been overwritten.
- A character could go missing from output when it came right after a non-ASCII character that
  arrived split across two network packets.

## [0.4.2] - 2026-11-14

### Changed

- Output reaches the screen faster. Komando now takes in plain text a whole run at a time
  instead of one character at a time: a million coloured log lines over SSH arrive about a
  quarter sooner, and plain or full-screen output can take in up to twice as much per second.
- Less work for every frame. Komando lays out again only the lines that changed, so typing,
  a still screen and programs such as vim or htop that redraw a few lines take a small fraction
  of the time they used to per frame.
- Screens full of different characters (many scripts, emoji, accents) no longer slow every frame
  down: Komando keeps the shapes of the characters on screen instead of working them out again
  each time.

### Fixed

- While scrolled up, output from a program that scrolls only part of the screen could make the
  view show the wrong lines.
- Update checks wait until the new release has a download for your install type, so releases
  that are still being built no longer offer an update that cannot be installed.

## [0.4.1] - 2026-11-14

### Fixed

- Tab completes commands and paths in the terminal again instead of moving keyboard focus
  through the app. Shift+Tab also reaches the shell; Ctrl+Tab and Ctrl+Shift+Tab still switch
  Sessions.

## [0.4.0] - 2026-11-14

### Added

- Search the output: press Ctrl+Shift+F, or right-click and choose "Find in output…". Komando
  searches the whole scrollback, highlights every match and shows where you are ("3 of 24").
  The minimap marks the lines with matches too, and over the output its handle shows where
  matches are in the whole scrollback. Enter or Up steps to older matches, Shift+Enter or Down
  back to newer ones; Escape closes the search and leaves the match selected, ready to copy. Searches ignore case unless you turn on
  **Match case** (Alt+C), and **Regular expression** (Alt+R) takes the text as a pattern.
  Results keep up with new output, and searching never holds up typing, even in a scrollback
  of 500,000 lines. Ctrl+F still goes to the shell.
- Share a project with someone: right-click it in the sidebar and choose "Share project…" to
  save it as a `.komando` file. It holds the project's hosts with their host names, ports,
  users, jump hosts, startup commands, colours and icons, and never a password or key. The
  person you send it to adds it under Settings → Import → Komando project, where they pick
  the hosts and review them first; Komando asks them for each password the first time they
  connect.
- Startup commands can wait for text before they're typed. In the host and project forms each
  command is now a row with an optional "Wait for" field: type `sudo -i`, then wait for `# `
  and type `cd /srv`. A command without it is typed once the shell goes quiet, as before. If
  the text doesn't appear within a minute, the remaining commands are dropped and Komando
  tells you. Startup commands can't answer password questions, since they're saved
  unencrypted.
- Importing from Tabby brings login scripts over as startup commands. Scripts that use
  regular expressions or optional steps, or that type a password, stay in Tabby and are
  listed in the review.

### Changed

- The minimap now has a column of its own beside the output, as in VS Code, so it never
  covers output and is always there. The terminal is a little narrower to make room. To get
  the full width back, choose **Over output** in **Settings → Terminal → Minimap placement**.
- Over the output, the minimap is easier to find and keep open. A thin bar at the terminal's
  right edge shows where you are in the scrollback and lights up when you rest the pointer on
  it; the minimap then opens, or opens at once if you click the bar. It stays open as long as
  the pointer is on it, including after you drag and let go. When the pointer leaves, it waits
  a moment and then fades out. You can set how long to rest on the bar, from no delay to 1 s.
  Split panes that don't touch the window's edge get a minimap too.
- Dragging the minimap's slider now moves it from where you grabbed it instead of jumping its
  middle to the pointer. Clicking elsewhere on the minimap still jumps there. The slider
  brightens under the pointer.
- PowerShell and Command Prompt icons now use higher-resolution artwork for sharper
  rendering on high-DPI displays.
- Loading indicators no longer flash when work finishes almost at once. In the file browser,
  the footer and the title bar's SFTP button keep their spinner for at least half a second,
  then show a check that fades out after a moment. The "Loading…" message in place of the
  file list appears only if loading takes longer than about a third of a second, and
  "Checking…" in Settings → About stays long enough to read.
- When you cancel signing in to an SSH Session, the connection log now says "Press Enter to
  reconnect" under "Cancelled."
- Dragging the window's edge stays smooth with a long Scrollback. Rewrapping 500,000 lines
  to a new width used to take most of every frame; now the text rewraps a few times a second
  while you drag and catches up when you let go. Shorter Scrollbacks still rewrap every frame.
- Copy output takes about 40% less time with a full Scrollback.
- Making a window or pane wider uses much less memory while the text rewraps. With a full
  Scrollback, a Session briefly needed up to three times its usual memory after becoming
  wider: with 500,000 lines at 350 columns, about 12.8 GB instead of 4.2 GB for a moment.
  Now it needs little more than usual. Becoming wider also takes up to a third less time with
  a long Scrollback, and tidying up afterwards takes up to two thirds less.
- Typing stays responsive while files upload in the file browser, as it already did for
  downloads. On a 10 Mbit/s link with 30 ms latency, keys showed over half a second late
  during an upload and now show about 25 ms late; on a 100 Mbit/s link with 50 ms latency,
  about 65 ms late before and 15 ms now. Uploads are about as fast as before.
- Hosts far behind Jump Hosts print output up to twice as fast. With 25 ms each way to a
  Jump Host and 25 ms more from there to the Host, 1,000,000 log lines took 2.5 s and now
  take 1.5 s; through two such Jump Hosts, 3.7 s before and 2.0 s now. Hosts reached
  directly, and Hosts next to their Jump Host, are as fast as before.

### Fixed

- Output stops soon after you press Ctrl+C during a flood of output, such as `cat` of a large
  file, also on slower connections. Since 0.3.7, everything the server had already sent kept
  scrolling past: on a 10 Mbit/s link with 30 ms latency for almost 4 seconds (1.3 seconds
  before 0.3.7), and on a 100 Mbit/s link with 50 ms latency for almost half a second. SSH
  Sessions now let the server send ahead only as much output as the connection needs, so the
  prompt comes back after about a tenth of a second on both. Floods are as fast as before, and
  faster on fast distant links: about 110 MB/s instead of 80 MB/s at 1 Gbit/s with 50 ms
  latency. Through Jump Hosts, output stops within about a fifth of a second too.
- When an SSH Session signs in after earlier attempts failed or were cancelled, the
  connection log of those attempts is now cleared too, so the shell starts on a clean screen.

## [0.3.7] - 2026-11-13

### Changed

- Appearance now shows fourteen rounded theme thumbnails with matching app and terminal
  palettes: Ember, Midnight, Forest, Twilight, Rosé, Glacier and Obsidian for dark mode;
  Ivory, Sky, Sage, Lavender, Blossom, Sand and Slate for light mode. An orange outline and
  checkmark mark the selected theme, and existing theme preferences carry over.
- Komando opens faster, most noticeably on slower PCs: the graphics driver now loads while the
  rest of startup runs instead of after it. On a slow dual-core the window is ready about 0.2 s
  sooner.
- Large amounts of SSH output, such as a flood of log lines, come through up to four times
  faster. A million coloured log lines that took about 4 seconds now take under 1 second.
- Several Sessions printing at once no longer hold each other up, and typing in a quiet
  Session stays quick while others print. Eight Sessions each printing half a million log lines
  finish more than twice as fast as before on a PC with eight or more CPU threads.
- Typing stays responsive while files download in the file browser. Downloads now keep only
  as much data on its way as the connection can carry, instead of a fixed amount that queued
  up ahead of your keystrokes: on a 100 Mbit/s link with 50 ms latency, keys showed about a
  third of a second late and now show about 15 ms late, and downloads are as fast as before.

## [0.3.6] - 2026-11-13

### Added

- Komando now keeps a log in `%APPDATA%\Komando\logs\komando.log`, including the details of
  any crash, to help track down problems. Set `KOMANDO_LOG=debug` for more detail.

### Changed

- The interface now uses the bundled Geist Sans font, including menus and overlays.

### Fixed

- The logo on the empty-session screen now uses the full theme text color, staying readable
  on light and dark backgrounds and updating when the theme changes.
- Accented letters typed with dead keys (´ then e for é on French, Spanish or German
  keyboards) and text from input methods for Chinese, Japanese or Korean now reach the
  terminal.
- Alt+F4 closes Komando and Alt+Space opens the window menu while a terminal has focus,
  instead of being sent to the shell.
- Characters typed with AltGr+Shift, such as Ą on a Polish keyboard, are no longer dropped.
- Downloads, saved settings and updates no longer fail when antivirus briefly holds a newly
  written file. Komando waits and tries again.
- Two Komando windows saving at the same time no longer interfere with each other.
- Folder downloads no longer stop when two remote names differ only in case ("README" and
  "readme") or by a trailing dot. The second one is saved as "readme (1)". Remote files named
  like Windows devices, such as `con` or `aux.log`, are saved as `con_` and `aux_.log`.
- Folder uploads read files under their real names, and skip broken links and unreadable
  items instead of failing the whole upload.
- Importing a private key saved by Windows tools as UTF-16 or with a byte order mark now
  works, and a key path starting with `~\` points to your home folder.
- A custom local shell whose path contains spaces, such as
  `C:\Program Files\PowerShell\7\pwsh.exe`, now starts reliably.
- Checking for and downloading updates now works behind company proxies. Komando uses the
  Windows proxy settings (or `HTTPS_PROXY`) and trusts certificates installed in Windows, such
  as those of a proxy that inspects HTTPS. Automatic proxy scripts (PAC) aren't supported yet.

### Security

- A file opened from the file browser by double-clicking it is now marked as downloaded from
  the internet, so Windows shows the same SmartScreen and security warnings as for a browser
  download. Files you only download are not marked.

## [0.3.5] - 2026-11-13

### Added

- Dialogs such as the host and project forms, delete confirmations, quick connect and the
  vault password now have a close button (✕) in the top-right corner. It does what Escape does.
- While files download or upload, a ring around the Downloads icon fills up with their
  progress. Several at once share one ring that adds up all their bytes, and the number
  beside it says how many are running. Until sizes are known, the ring turns instead.
- The update button beside Settings now has its own icon, an arrow rising out of a circle, so
  it no longer looks like the Downloads button above it. While the update downloads, a ring
  around it fills up, like the one on Downloads.
- A dot on the Downloads button shows that a download finished while the downloads panel
  was closed. It goes away when you open the panel.
- Import your SSH profiles from Tabby in Settings → Import. Komando finds Tabby's settings
  on its own (or you choose its `config.yaml`), lets you tick the profile groups and profiles
  to bring over, and shows a review before anything is saved. Each group becomes a project;
  each profile becomes a host with its name, host name, port, user and a matching icon.
  Profiles you already have in Komando are recognised and left unticked. Passwords, keys,
  jump hosts, login scripts and colours are not imported; the review lists, per host, each
  setting you changed in Tabby that doesn't come over.

### Changed

- Release downloads use filenames containing the release version, such as
  `Komando-0.3.5-x64.msi` and `Komando-0.3.5-x64.exe`, without unversioned copies.
  README download links open the latest release so you can choose the installer or
  portable EXE.
- The SFTP browser's right-click menu is shorter: Rename, Copy to… and Move to… are hidden
  unless you switch them on in the new Settings → File browser page. F2 still renames.
- "Copy full path" in the SFTP browser's right-click menu is now called "Copy as path".
- Jump hosts are now typed in where you use them instead of being saved hosts. In the host and
  project forms, add as many as you need, each with user, host name or IP, port and a "Set
  password" button that saves the password in the vault. Komando signs in to them top first.
  A host goes through its project's jump hosts first, then its own; a switch turns the
  project's off. Your existing jump hosts are converted automatically and the hosts stay.

### Fixed

- Resizing the window, opening the SFTP browser or a split view no longer leaves a session
  holding much more memory than its output needs. With a long scrollback, making the window
  even one column wider used to double that session's memory until Komando closed.
- "Jump to here" in the SFTP browser now changes the terminal's directory: to the folder,
  or for a file to the folder it is in. If a program such as an editor has the terminal,
  the browser says so instead of typing into it.

## [0.3.4] - 2026-11-13

### Added

- Split view: drag a host or local shell from the sidebar onto the terminal area to open
  it beside the session you're in. A blue outline shows which half it takes as you move
  the pointer, and letting go opens it there. Click either side to type in it; drag a side
  onto the other half to swap them. "Open in split view" in an item's right-click menu does
  the same, and the split view button in the title bar goes back to one session. Dragging
  the host you're in opens a second session of it beside the first.
- Drag a host onto another project in the sidebar to move it there. The project lights up
  while the host is over it. Open sessions go with the host and stay connected; from then
  on it takes the new project's defaults.
- "Duplicate host…" in a host's right-click menu. It adds a copy with the same settings
  and sign-in below the original, then opens the copy's form so you can rename it or
  change its address.
- The SFTP button and the file browser show a turning loader while SFTP starts and while a
  folder loads.

### Changed

- Once an SSH connection has signed in, its connection messages (host key check, signing
  in) are erased, so the shell starts on a clean screen. If connecting fails, they stay so
  you can see why.
- Dragging a selection past the top or bottom of the terminal now scrolls once per screen
  refresh instead of in jumps 25 times a second, so it looks smooth on 120 Hz and faster
  displays.

### Fixed

- Failed Vault saves leave Credentials as they were before the attempted edit, so an
  unsuccessful addition, password change or removal cannot take effect in memory or be
  written by a later save.
- Folder downloads stop safely when different remote names become the same Windows file
  or folder name, instead of overwriting a file or merging folders silently.
- In split view, dragging files from one File Browser into the other no longer moves
  files with matching paths on the wrong Host.
- If your list of hosts can't be read at start, Komando keeps the file under a new name
  and tells you, instead of replacing it with an empty list the next time it saves.
- Creating a vault never replaces one that already exists, for example one created in
  another Komando window while the master password dialog was open.
- Settings, hosts and the vault are written to disk completely before they replace the
  old file, so a crash or power cut can't leave an empty file behind.
- A setting that can't be saved now shows a message instead of being lost silently.
- Messages in the corner disappear after about 4 seconds, instead of staying for up to
  24 seconds.
- Ctrl+Shift+K and Ctrl+, no longer close an open host or project form and lose what
  you typed.
- "Copy as .txt" twice in the same minute no longer overwrites the first file.
- Copying a folder on the server and uploading a folder skip links to folders, as
  downloads already did, instead of following a link that loops.
- When creating a folder fails during an upload, the message names the folder that
  failed instead of the upload's destination.
- A failed update download no longer leaves its partial file in the temporary folder.
- When Highlight Rules can't be combined, the Highlights page says so instead of
  highlighting nothing without explanation.
- The minimap's viewport box now gets brighter while you drag it, as intended.
- "Clear successful" and "Clear failed" in Downloads only appear when there is something
  to clear.
- An empty folder you can't write to no longer suggests dropping files into it.
- Highlights in the minimap no longer use stale results after Highlight Rules change.
- "Keep both" works when uploading into a folder other than the one shown, including
  files dropped onto a folder row, and avoids numbered filenames already in that folder.
- Copying or moving files to their own folder works when its path has a trailing slash
  or `.`. Copies get a numbered name; moves leave the files where they are.
- Password, Host and Project saves report write failures. Failed Host or Project edits
  leave the library unchanged, and failed deletions keep the item and its Sessions open.
- Keys imported by a Host or Project form are cleaned up if the form cannot be saved,
  so retrying does not leave unused copies in the Vault.
- Starting a Session uses the last full-width terminal size, avoiding a half-width start
  after split view or the File Browser has narrowed another terminal.
- Keyboard shortcut labels (Quick connect, Settings, Back to sessions) sit in the middle
  of their outline instead of touching its top edge.
- The SFTP, copy and close-session buttons in the title bar respond to every click on
  Windows. Before, a click could be taken as grabbing the title bar and do nothing, and a
  double click on one of them maximized or restored the window.
- With background blur on, Windows no longer draws its own minimize, maximize and close
  icons behind Komando's window buttons, so each button shows one icon again.

### Security

- "Copy as .txt" files, which can contain anything printed in the terminal, are deleted
  from the temporary folder after a day.
- A key passphrase typed at sign-in is wiped from memory after use.

## [0.3.3] - 2026-11-12

### Fixed

- Simultaneous file and folder downloads with matching names choose separate destinations
  on Windows, including when Windows reports a filename collision as "Access denied".

## [0.3.2] - 2026-11-12

### Added

- A download icon beside Settings at the bottom of the sidebar appears when an update is
  available. Hover for the new version and release highlights; click to download, install
  and restart Komando. The icon shows download progress and keeps update errors in reach.

### Changed

- In-app updates reopen Komando automatically after installation, for portable copies and
  MSI installs. Installed copies keep their per-user or all-users scope; Windows asks for
  administrator approval when needed. If installation fails or is cancelled, Komando reopens
  with an explanation. Updating closes open sessions.

### Fixed

- Large Project libraries stay responsive while terminals stream output. The sidebar resolves
  Hosts once per frame and builds only the visible Host or Local Shell, avoiding repeated
  searches and copies of entire Projects.
- Highlighted terminal output uses precomputed readable colours for both dark and light
  Themes, reducing repaint work while preserving Highlight Rule precedence and custom backgrounds.
- Concurrent downloads with matching filenames reserve their destinations without blocking
  SSH processing. Existing files and partial downloads keep their names and contents.
- The Windows title bar uses one consistent background with Mica enabled, removing the
  dark rectangle behind the window buttons and covering stray native caption pixels beside
  them. The rounded hover effects and window actions are unchanged.
- Update downloads use a fresh installer filename for each attempt, avoiding "Access is
  denied" when Windows still has an earlier installer open. Brief Windows file locks are
  retried, and download errors now name the file operation that failed. Old downloads are
  cleaned up after a week without removing recent downloads another Komando window may need.

## [0.3.1] - 2026-11-12

### Changed

- The minimap stays hidden until you hover at the window's far-right edge for a third of a
  second. Click or drag it to scroll, then move away to hide it. Selecting text does not
  open it. Its background hides the output underneath while it is open; terminal columns
  and line wrapping stay unchanged, even while new output arrives.

## [0.3.0] - 2026-11-12

### Added

- When `sudo` asks for your password in an SSH session, Komando shows "Press Enter to use your
  sign-in password" after the prompt. Press Enter and it types the password you signed in to
  the host with, saved or typed; press any other key to type your own. It is offered only when
  sudo asks for the user you signed in as, and not again right after it was rejected. Sessions
  that sign in with a key have no password to offer.
- Settings → Local shells lists the shells at the top of the sidebar. Switch off "Show in
  sidebar" for the ones you don't use (Command Prompt, say), remove them, or add your own with a
  name, program and arguments, such as WSL or Git Bash. Hidden shells are still found by quick
  connect. "Restore detected shells" brings back the ones found on this PC.
- The sidebar shows which sessions have a command at work: while output keeps coming in, such
  as from `tree /`, `tail -f`, a build or `top`, the session's dot turns into a green pulse
  line, on its group and on a folded project too. It goes back to a dot a few seconds after the
  output stops. Typing and quick commands like `ls` don't trigger it.
- Right-click in the terminal for a menu: Copy, Paste, Paste selection (types the selected
  text at the prompt), Select all, and Copy output or Copy as .txt for the whole output. With
  the PuTTY-style clipboard on, right-click still pastes and Ctrl+right-click opens the menu.
  When a program such as vim or htop uses the mouse, right-click goes to it and
  Shift+right-click opens the menu.

### Changed

- A host or shell with several sessions is now a group in the sidebar: its row shows how many
  sessions it has and folds them away with the chevron. Each session's row shows the title its
  shell set (such as `user@web01: ~`) or "Session 2", with its number, and has its own close
  button. The group's row no longer has one, since it closed whichever session you used last;
  right-click it for "Disconnect all". Closing the session you are on shows another session of
  the same host or shell.
- A hidden or removed shell stays in the sidebar while it has sessions open.
- The vault's auto-lock goes up to 7 days: 5, 15 or 30 minutes, 1 or 8 hours, 1 day, 7 days,
  or "Until restart", which keeps it unlocked until you close Komando. "Until restart" is the
  default for new installs; existing installs keep their current setting (30 minutes unless you
  changed it), which Settings → Security can change.
- The window buttons (minimize, maximize, close) are rounded like the rest of Komando and sit
  inside a slightly taller title bar, with room between them and the terminal's coloured top
  edge; the close button's red no longer runs into it. The close button lines up with the
  terminal's right edge, and the window's top-right corner still closes a maximized window.

### Fixed

- Selecting text keeps following the mouse when you drag past the terminal, onto the sidebar
  or out of the window. Dragging above or below the terminal scrolls the output along, faster
  the further you go, so you can select more than fits on screen.
- Hovering a folded project no longer puts its add button on top of the "1 live" count; the
  count makes way while the pointer is over the row.
- A new terminal no longer lets you scroll up into a few empty lines above the first output,
  and the minimap stays hidden until there is output to scroll back to. The shell now starts
  at the terminal's real size instead of being resized right after it starts, and empty lines
  a shell leaves above the prompt when the window is resized are dropped.

## [0.2.8] - 2026-11-12

### Changed

- Komando no longer asks for the master password when it starts. It asks only when something
  needs the vault: connecting to a host that signs in with a saved password or key, saving a
  password, importing a key file, or unlocking from Settings → Security. Several hosts opened
  at once share one prompt.
- The master password dialog is easy to tell apart from a server's password question: it
  carries the vault badge and an orange top edge, says why it is asking, and notes that this is
  your Komando password, not the server's. When connecting, "Type the server password instead"
  skips the vault for that connection; Escape does the same.
- File transfers are much faster to servers that are far away. A folder of many small files
  transfers several files at once instead of one after another (about 30 times faster with
  50 ms between you and the server), large downloads are about 3.5 times faster, and large
  uploads about 4 times faster. Deleting a large folder on the server is faster too.
- Many transfers on one connection take turns instead of all running at once: a transfer
  waiting for others says "Waiting for other transfers" in Downloads. Selecting a thousand
  files and downloading them, or starting several large downloads on a slow connection, no
  longer fails.
- Uploading many dropped files starts right away. Before, Komando checked the names on the
  server one at a time first.

### Fixed

- Downloading the same file twice at the same moment broke both downloads; each now gets a file
  of its own ("name (1)"). An upload started while another upload to the same place is still
  running is refused instead of breaking the first one.
- Selecting a large number of files and downloading them no longer fails with "The server
  reported a failure" (the server's limit on open files).
- Uploading a folder with very many files no longer freezes the terminals while Komando looks
  through the folder.

### Removed

- The `--locked` start option, which skipped the startup unlock screen. There is no such screen
  any more.

## [0.2.7] - 2026-11-12

### Fixed

- A connection question (user name, password, host key) is a proper dialog: the window behind
  it is dimmed and can't be used until you answer or cancel it, and Escape cancels it. Before,
  the sidebar stayed clickable, so a second dialog such as Edit project could open while the
  question was still waiting behind it.
- Only one dialog opens at a time: Quick connect, Edit project, Add host and delete
  confirmations wait until an open connection question is answered.
- Selecting text in a dialog's field by dragging past the dialog's edge no longer closes the
  dialog. It closes when you press outside it.

## [0.2.6] - 2026-11-12

### Fixed

- Typing into a connection question (the user name or password asked while connecting) works
  again. The keys went to the terminal behind it, so the field stayed empty.

## [0.2.5] - 2026-11-12

### Added

- Windows minimizes to the notification area by default, keeping Sessions running. Click the
  Komando tray icon to restore, or right-click for Show Komando and Exit. Turn this off in
  Settings → Appearance → Minimize to tray to use normal taskbar minimization.

### Changed

- When connecting asks for a user name or password, the question has a "Remember for this host"
  checkbox. Tick it and, once you are signed in, Komando saves the user name with the host and
  the password in your vault. This replaces the "Save the password?" question that came after
  signing in.
- Connection questions (user name, password, host key) appear in the middle of the terminal
  instead of at the bottom.
- The "SSH" tag in front of each connection-progress line is muted grey text instead of a
  bright inverted block.
- The host and project forms no longer fill empty fields with examples ("e.g. deploy",
  "e.g. web01", …); the field labels say what goes where. The icon search reads "Search icons".

### Fixed

- Terminal text aligns vertically with the cursor on Windows, including when starting a new console.
- Window buttons no longer show ghosted icons behind them when the Mica background is enabled.
- The SFTP file list keeps its Size, Modified and Perms columns lined up under their headings,
  however long the file names are.

## [0.2.4] - 2026-11-12

### Fixed

- Windows background blur now configures the wallpaper backdrop directly and clears it when
  switched off. If Windows cannot enable it, the window keeps an opaque background.
- Removed duplicate background layers that obscured the wallpaper blur in Sessions and Settings.

## [0.2.3] - 2026-11-12

### Added

- The installer asks who Komando is for. **Install for me only** puts it in your user folder
  and needs no administrator rights; **Install for all users** puts it in Program Files for
  everyone who signs in to the PC, and Windows asks for administrator rights. A new install
  starts on "for me only"; if Komando is already installed for all users (as every version up
  to 0.2.2 was), that is preselected, so the update replaces it instead of adding a second copy.
- The installer ends on a page that says Komando is installed, with a ticked "Open Komando now"
  box. Repairing or removing Komando from the installer says what happened too.
- Updating from Settings → About runs the installer for an install for one user as well, and
  says whether Windows will ask for administrator rights.

### Fixed

- Validation errors in the Project and Host dialogs stay visible above the action buttons,
  even when the form needs scrolling.
- Settings navigation, themes, switches and shared buttons expose their names and states to
  Windows accessibility tools and can be reached and activated with the keyboard.

## [0.2.2] - 2026-11-12

### Changed

- The add/edit host and new/edit project dialogs are wider and lay their sections out in two
  columns side by side, so most of the form fits on screen without scrolling. On a window
  narrower than about 940 pixels they keep the single column.
- Local shells show their own icons, as in Windows Terminal: PowerShell 7, Windows PowerShell
  and Command Prompt each have theirs, and WSL and Linux shells show Tux. In the sidebar, quick
  connect and on the start screen.
- The new Komando logo (">_Komando.") in the title bar, on the start, unlock and empty
  screens and in Settings → About, drawn in dark or light to match the theme. The app icon
  stays the same.
- The minimap lies over the terminal's right edge, like VS Code's, instead of taking a column of
  its own: the terminal gets its full width back. It is as tall as the output (up to the
  terminal's height) and its width follows the terminal's.

### Fixed

- The SFTP button above a session looks unavailable (faded, no pointer) while the session is
  connecting or disconnected, and says why when you hover it, instead of looking clickable.
  An open file browser can still be closed with it.
- The minimap no longer shows a box you can drag when there is nothing to scroll, such as a new
  session with just its prompt. It appears once output has scrolled off the screen, and stays
  out of the way of full-screen programs such as vim and htop.
- The minimise, maximise and close buttons answer a single click again after you resize the
  window with a dialog open. Before, only a double click did anything.
- Clicking beside the username or password question shown while connecting no longer takes the
  keyboard away from it, and clicking its field puts the cursor back there. The terminal behind
  the question no longer starts a text selection either.

## [0.2.1] - 2026-11-12

### Changed

- Refreshed the README screenshots to show the current terminal, SFTP browser and highlight settings.

## [0.2.0] - 2026-11-12

### Added

- An SFTP file browser beside the terminal. Click **SFTP** above an SSH session (or press
  Ctrl+Shift+E) and the server's files open on the right, over the session's own connection:
  no second login. Drag the divider to give it more or less room; each session keeps its own
  folder and history.
- Browse with back, forward and parent folder, click a folder in the path to go there, or click
  the path to type one. Sort by name, size, date or permissions; hidden files can be shown or
  hidden.
- Upload and download files and whole folders, from the toolbar or by dropping files from
  Explorer onto the list (or onto a folder in it). Create folders, rename, copy, move and delete,
  for one item or a whole selection, from the right-click menu or each row's ⋯ button. Drag
  files onto a folder to move them there.
- Favorite folders behind the star next to the path. A favorite can be for all hosts (handy for
  `/var/log` or `/etc/nginx`) or for one host only, and can be renamed and reordered.
- Files and folders you can't open show a lock; hover it, or the permissions, to see who owns
  them and what you may do. Actions you aren't allowed to take are greyed out, and permission
  errors say so plainly.
- A Downloads entry in the sidebar, once you have transferred something. It lists every
  download and upload from every host, with progress, and a number for transfers that are
  running or failed. Open a downloaded file, show it in its folder, copy the file itself to
  paste elsewhere, or copy its path. Failed transfers say why and can be retried.

### Fixed

- Downloads never leave a misleading file behind: a file you aren't allowed to read is not
  saved as an empty file, and a download that fails or is cancelled midway is removed instead
  of being left half-written. Existing files in Downloads are never overwritten; a second copy
  gets a name like "app (1).conf". Genuinely empty files still download.

## [0.1.17] - 2026-11-12

### Added

- Jump hosts. A host that can only be reached from another host now says so under
  "Reached through": pick the host it goes through, which can itself go through another.
  While connecting, Komando shows the way there as a row of cards, one per host, starting from
  this PC. Green shows how far the connection has got, and a red card with an ✕ shows where it
  failed and why, with Retry and Edit host right there. Questions about trusting a host key or
  typing a password show on the page too, and name the host they are for.
- Startup commands. Lines you give a host or a project are typed into the shell once it is
  ready, as if you typed them: `cd /srv/app`, `sudo -i`, `tmux attach`. A project's commands
  run first, then the host's own; a host can skip the project's. If a command asks for a
  password, Komando waits until you have answered before typing the next one.

### Changed

- Projects now set defaults for their hosts in one place: user, key or password, jump host and
  startup commands. Before, a project's default user and password could only be set by
  saving a typed password. In the host form, each of these says where it comes from: the
  project, the host itself, or neither (ask each time, or connect directly).
- Deleting a host that others are reached through says which ones; they connect directly
  afterwards.

## [0.1.16] - 2026-11-12

### Changed

- A new start screen for when you haven't added anything yet: what Komando is, and one click
  to add a connection, create a project or open PowerShell, Command Prompt or your local shell.
  A connection added there goes into a new project, "My servers", which you can rename later.

## [0.1.15] - 2026-11-11

### Added

- Mouse support in terminal programs. vim, htop, tmux, mc and others that ask for the mouse
  now get clicks, drags and the wheel, so you can click to place the cursor in vim, pick a pane
  in tmux or press htop's buttons. Hold Shift to select text as before.
- Programs are told when Komando gains or loses focus, if they ask (vim and tmux use it, for
  example to reload files changed elsewhere).
- Programs can ask for the terminal's colours and size. vim and neovim now pick their dark or
  light colours to match your theme.
- The cursor takes the shape programs ask for: a bar in vim's insert mode, an underline in
  replace mode.
- Strikethrough text, and underlines in their own colour (as spell checkers and language
  servers draw them).

### Changed

- Box-drawing lines, block characters and Powerline separators are drawn by Komando instead of
  the font. Borders in tmux, vim splits, htop, mc and prompt themes join up without gaps
  between rows.
- The terminal draws much faster under heavy output: each character is laid out once and
  reused, instead of every line being laid out again on every frame.

### Fixed

- Wide characters (Chinese, Japanese, Korean), emoji and icon fonts no longer push the rest of
  the line out of its columns.
- Accents typed as separate characters (combining marks) are shown instead of dropped.
- The mouse wheel in less, man and other full-screen programs scrolls by line in the mode the
  program set, and stops when the program turns that off. Small touchpad scrolls add up
  instead of being lost.
- Ctrl+Alt+letter, Ctrl+/ and Alt with Enter, Tab, Backspace or Escape send what terminal
  programs expect.
- The tab title goes back to the host's name when a program such as vim resets it on exit.
- SSH sessions: backspace over accented and other non-ASCII characters at a shell prompt
  deletes the whole character, and the remote side knows the terminal's size in pixels.
- SSH sessions no longer stall when a program starts a synchronized screen update and never
  finishes it, and the remote shell's exit code is kept when a server ends the output before
  sending it.

## [0.1.14] - 2026-11-11

### Added

- Themes. **Settings → Appearance** offers seven dark themes (Ember, Midnight, Forest, Dusk,
  Rosé, Glacier, Graphite) and seven light ones (Ember, Sky, Sage, Lavender, Blossom, Paper,
  Graphite). Each colours the app and the terminal together: the accent, greys tinted to
  match, text and the terminal background. You pick a dark and a light theme separately, so
  each mode keeps its own colours when you switch or when Windows switches for you.
- Themes are shown as bubbles with the theme's background, text and accent; click one to use
  it. The mode in use is marked **In use**.

### Changed

- **App theme** is now called **Mode** (Dark, Light, System).
- Primary buttons pick dark or white text, whichever reads better on the theme's accent.

## [0.1.13] - 2026-11-11

### Fixed

- In the sidebar, hovering a host showed the + and × buttons on top of its status dot. The dot
  now hides while the buttons are shown, and the buttons' backdrop matches the row.

## [0.1.12] - 2026-11-11

### Added

- Updates from inside Komando: **Settings → About** checks GitHub for a newer release and installs
  it with one click.
  - A portable `komando.exe` is swapped in place, and **Restart now** finishes the update.
  - An installed Komando downloads and starts the new installer, which asks for administrator
    rights and replaces the old version.
  - Downloads are checked against the size and SHA-256 checksum GitHub lists before anything is
    replaced.
  - Komando looks for a new version when it starts; turn that off with **Check automatically**.
    Nothing is downloaded without asking. When a new version is found, the sidebar's Settings
    button shows **Update**.

## [0.1.11] - 2026-11-11

### Fixed

- Sessions you aren't looking at no longer make the window redraw. Any output in a background
  session redrew the whole window, sidebar included, so with many busy hosts Komando kept the
  CPU and GPU busy even when the visible terminal was quiet. In a test with 24 hosts each
  streaming 400 log lines a second and the visible session idle, CPU use dropped from more
  than three cores to under a tenth of one.
- The sidebar builds only the rows in view, so hundreds of hosts no longer slow every redraw.
  It took about 5 ms per frame with 300 hosts and now takes about 0.3 ms.

## [0.1.10] - 2026-11-11

### Added

- Right-click menus in the sidebar. On a host: connect or show its session, open another
  session, disconnect, **Edit host…**, **Copy address** and **Delete host…**. On a project:
  **Add host…**, **Edit project…**, collapse or expand, and **Delete project…**. On a nested
  session: reconnect, disconnect or close it.
- Hosts and projects can be edited (name, address, user, port, key, colour, icon) and deleted.
  Deleting asks first; it closes their open sessions, and saved keys and passwords stay in the
  vault. Deleting a project also deletes the hosts that belong to no other project.

- Komando draws its own title bar, so the top of the window matches the app instead of the
  standard Windows bar. The KOMANDO wordmark sits over the sidebar, the active session's name,
  address and actions sit over the terminal, and minimise, maximise and close are at the right.
  Dragging, double-click to maximise, snap layouts and resizing from the top edge work as
  before.

- Icon picker with search for hosts and projects. You can choose any of the roughly 1,800
  bundled Lucide icons instead of eleven. Without a search it suggests icons for servers and
  infrastructure. A search matches names and words in names, so "backup" finds
  "database-backup". Hover an icon to see its name.

### Fixed

- Long logs no longer slow the terminal down:
  - The minimap remembers how it drew each line and only works out lines that changed, so
    repainting it while you scroll or while output streams is about 10× cheaper.
  - With a full scrollback, the command rail no longer rescans every scrollback line each
    time a new line arrives; it follows the lines as they move up (about 100× cheaper with
    100,000 lines of scrollback).
- Clicking a field in a dialog (Add host, New project, Quick connect) while a session was open
  moved the keyboard focus to the terminal behind the dialog, so typing went into the session.
  Clicks now stay in the dialog.

### Changed

- Komando's accent colour is orange instead of purple: buttons, switches, selections,
  sliders, the active settings page and the terminal's text selection. Primary buttons have
  dark text, which reads better on orange.
- The Add host and Edit host form is laid out properly:
  - the project's badge in the header;
  - labelled fields, with address and port side by side, and the project's default user shown
    in the empty User field;
  - a **Sign-in** choice (password or the project's credential, a key file with **Browse…**,
    or the host's saved key) that explains what each option does;
  - the icon picker and a clear error banner.

  Enter saves from any field.
- Dialogs use the window's height, so taller forms fit. In small windows the host form's
  fields scroll, and its buttons stay in view.
- The sidebar starts with Quick connect; the wordmark moved up into the title bar.
- The project-coloured line on top of the terminal follows the terminal's rounded corners and
  tapers off down their curve, instead of a straight bar that stopped short of them.
- The session's close button shows an unplug icon, so it can't be mistaken for the window's
  close button in the same bar.
- Dialogs leave the title bar uncovered, so the window can be moved or closed while one is
  open.
- The dark theme is a soft deep grey instead of near-black, which is easier on the eyes in
  long sessions. This includes the terminal background (#1a1a1e). The terminal's black and
  bright-black colours are lighter so dimmed output stays visible on it.

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

[Unreleased]: https://github.com/R4cc/komando/compare/v0.8.0...HEAD
[0.8.0]: https://github.com/R4cc/komando/compare/v0.7.9...v0.8.0
[0.7.9]: https://github.com/R4cc/komando/compare/v0.7.8...v0.7.9
[0.7.8]: https://github.com/R4cc/komando/compare/v0.7.7...v0.7.8
[0.7.7]: https://github.com/R4cc/komando/compare/v0.7.6...v0.7.7
[0.7.6]: https://github.com/R4cc/komando/compare/v0.7.5...v0.7.6
[0.7.5]: https://github.com/R4cc/komando/compare/v0.7.4...v0.7.5
[0.7.4]: https://github.com/R4cc/komando/compare/v0.7.3...v0.7.4
[0.7.3]: https://github.com/R4cc/komando/compare/v0.7.2...v0.7.3
[0.7.2]: https://github.com/R4cc/komando/compare/v0.7.1...v0.7.2
[0.7.1]: https://github.com/R4cc/komando/compare/v0.7.0...v0.7.1
[0.7.0]: https://github.com/R4cc/komando/compare/v0.6.6...v0.7.0
[0.6.6]: https://github.com/R4cc/komando/compare/v0.6.5...v0.6.6
[0.6.5]: https://github.com/R4cc/komando/compare/v0.6.4...v0.6.5
[0.6.4]: https://github.com/R4cc/komando/compare/v0.6.3...v0.6.4
[0.6.3]: https://github.com/R4cc/komando/compare/v0.6.2...v0.6.3
[0.6.2]: https://github.com/R4cc/komando/compare/v0.6.1...v0.6.2
[0.6.1]: https://github.com/R4cc/komando/compare/v0.6.0...v0.6.1
[0.6.0]: https://github.com/R4cc/komando/compare/v0.5.2...v0.6.0
[0.5.2]: https://github.com/R4cc/komando/compare/v0.5.1...v0.5.2
[0.5.1]: https://github.com/R4cc/komando/compare/v0.5.0...v0.5.1
[0.5.0]: https://github.com/R4cc/komando/compare/v0.4.4...v0.5.0
[0.4.4]: https://github.com/R4cc/komando/compare/v0.4.3...v0.4.4
[0.4.3]: https://github.com/R4cc/komando/compare/v0.4.2...v0.4.3
[0.4.2]: https://github.com/R4cc/komando/compare/v0.4.1...v0.4.2
[0.4.1]: https://github.com/R4cc/komando/compare/v0.4.0...v0.4.1
[0.4.0]: https://github.com/R4cc/komando/compare/v0.3.7...v0.4.0
[0.3.4]: https://github.com/R4cc/komando/compare/v0.3.3...v0.3.4
[0.3.0]: https://github.com/R4cc/komando/compare/v0.2.8...v0.3.0
[0.1.17]: https://github.com/R4cc/komando/compare/v0.1.16...v0.1.17
[0.1.16]: https://github.com/R4cc/komando/compare/v0.1.9...v0.1.16
[0.1.9]: https://github.com/R4cc/komando/compare/v0.1.8...v0.1.9
[0.1.8]: https://github.com/R4cc/komando/compare/v0.1.7...v0.1.8
[0.1.7]: https://github.com/R4cc/komando/compare/v0.1.6...v0.1.7
[0.1.6]: https://github.com/R4cc/komando/compare/v0.1.5...v0.1.6
[0.1.5]: https://github.com/R4cc/komando/compare/v0.1.4...v0.1.5
[0.1.4]: https://github.com/R4cc/komando/compare/v0.1.3...v0.1.4
[0.1.3]: https://github.com/R4cc/komando/compare/v0.1.2...v0.1.3
[0.1.2]: https://github.com/R4cc/komando/compare/v0.1.1...v0.1.2
[0.1.1]: https://github.com/R4cc/komando/compare/v0.1.0...v0.1.1
[0.1.0]: https://github.com/R4cc/komando/releases/tag/v0.1.0
