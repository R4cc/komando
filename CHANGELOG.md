# Changelog

All notable changes to Komando are listed here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and versions follow
[Semantic Versioning](https://semver.org/).

## [Unreleased]

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

[Unreleased]: https://github.com/R4cc/komando/compare/v0.1.17...HEAD
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
