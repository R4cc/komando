<picture>
  <source media="(prefers-color-scheme: dark)" srcset="assets/logo-dark.svg">
  <img src="assets/logo-light.svg" width="292" height="42" alt="Komando">
</picture>

# Komando: a fast, Rust-built SSH terminal with project-based server organisation

**Komando is a fast SSH terminal for Windows 11, written in Rust.** It keeps all your Linux servers organised in colour-coded projects, one click away, with passwords and keys in an encrypted vault. Built for sysadmins, DevOps and system engineers who spend their day in SSH sessions.

**[⬇ Download Komando for Windows (.msi)](https://github.com/R4cc/komando/releases/latest/download/Komando-x64.msi)** · [All releases](https://github.com/R4cc/komando/releases) · [Changelog](CHANGELOG.md)

<img src="assets/screenshot-dark.png" alt="Komando SSH terminal: servers grouped into projects in a sidebar, an SSH session showing systemctl status and a highlighted application log">

<sub>Screenshots of the real app: SSH sessions and SFTP against an isolated test server, with example logs and files.</sub>

## Why Komando

- **Fast.** Written in Rust, drawn on the GPU. Even a flood of log output stays smooth and the window never freezes.
- **Organised.** Group servers into projects with their own colour and icon. You always know which environment you're typing into.
- **Secure.** Passwords and SSH keys live in a vault encrypted with your master password. Unknown server keys are checked before you connect.
- **Made for Windows 11.** A native Windows app with its own built-in SSH client. No WSL, no PuTTY, no extra tools.

## Features

### Manage all your servers

- **Every server one click away.** The sidebar always lists your saved SSH hosts and local shells. Click to connect, or to jump back to a server you're already on.
- **Projects with colours and icons.** A coloured strip above the terminal shows the active project at a glance, so you don't run a command on production by accident.
- **Shared settings per project.** Set a default user, key or password, jump host and startup commands once for a project and every host inherits them, unless it sets its own. A host can belong to several projects.
- **Jump hosts.** Reach servers that are only reachable from another server. While connecting, Komando shows each host on the way and where the connection is, or where it failed.
- **Startup commands.** Have `cd /srv/app`, `sudo -i` or `tmux attach` typed for you once the shell is ready.
- **Reconnect in place.** If a connection drops, you keep the output and one click reconnects in the same window.
- **Multiple sessions per host.** Ctrl+click a host to open a second session on the same server. A host or shell with several sessions folds them into a group in the sidebar, each with its own row and close button.
- **Local shells too.** PowerShell 7, Windows PowerShell and Command Prompt are detected automatically and can live inside your projects. In Settings → Local shells, hide the ones you don't use or add your own, such as WSL or Git Bash.

### Secure by default

- **Encrypted password and key vault.** Argon2id and XChaCha20-Poly1305, locked by a master password. Auto-locks after 30 minutes idle while your sessions stay connected.
- **Saves passwords for you.** When a password you typed works, Komando offers to remember it for that host or for the whole project.
- **Sudo without retyping.** When `sudo` asks for your password, press Enter and Komando types the password you signed in with.
- **Import SSH keys.** OpenSSH and PuTTY `.ppk` keys are supported.
- **Server fingerprint checks.** New servers ask for your trust on first connect, and a changed key blocks the connection. Hosts you already trust in your OpenSSH `known_hosts` are recognised.

### Files over SFTP

<img src="assets/screenshot-sftp.png" alt="Komando SFTP browser beside an SSH terminal, showing deployment folders, configuration files, sizes, modification dates and permissions">

- **A file browser beside the terminal.** Click **SFTP** above any SSH session: the server's files open on the right, over the connection you already have, with no second login. Drag the divider to size it.
- **Everything a quick file job needs.** Upload and download files and whole folders (or drop them in from Explorer), create, rename, copy, move and delete, sort by name, size or date, and see each file's permissions.
- **Favorite folders.** Star `/var/log` once and it's there on every server, or keep a favorite to one host.
- **Honest about access.** Files you can't read show a lock, and a download that fails never leaves an empty file behind. The Downloads list in the sidebar shows every transfer, with retry for the ones that failed.

### Read logs faster

<img src="assets/screenshot-highlights.png" alt="Komando settings: a rule set of 69 highlight rules, each shown in its own colour with regex and case badges and an on/off switch" width="720">

- **Syntax highlighting for terminal output.** ERROR in red, WARN in orange, `Active: active` in green. 69 ready-made rules for Linux and service logs are included.
- **Your own rules.** Import and export rule sets as JSON, compatible with Tabby's highlight format. See [Highlights](docs/highlights.md).
- **Minimap.** The whole output in miniature on the right edge, like in VS Code, with errors and warnings in colour. Click or drag to jump.
- **Command rail.** A small mark at the left edge for every command you ran. Hover to see it, click to scroll back to it.
- **Copy the whole output.** Copy a session's full scrollback as text, or as a `.txt` file you can paste straight into Explorer, Outlook or Teams.

### Looks good

- **Themes for dark and light.** Pick a theme for each mode (Ember, Midnight, Forest, Paper, …); the terminal can follow the app or keep its own mode.
- **Windows 11 Mica blur.** Your blurred wallpaper shows through the window, with the terminal kept readable. Adjustable or off in Settings.
- **Slim, distraction-free layout.** Just your sidebar, the terminal, and a thin status line.

## Keyboard shortcuts

All app shortcuts use Ctrl+Shift, so plain Ctrl keys (Ctrl+W, Ctrl+R, …) still reach the shell on your server.

| Action | Keys |
|---|---|
| Quick connect | Ctrl+Shift+K |
| Another session on the current host | Ctrl+Shift+T |
| Disconnect (keeps the output); again to discard it | Ctrl+Shift+W |
| Next / previous session | Ctrl+Tab / Ctrl+Shift+Tab |
| Copy / paste | Ctrl+Shift+C / Ctrl+Shift+V |
| Copy whole output as text / as .txt file | Ctrl+Shift+A / Ctrl+Shift+S |
| Lock vault | Ctrl+Shift+L |
| Show / hide the SFTP file browser | Ctrl+Shift+E |
| Settings | Ctrl+, |

## Install

1. **[Download the latest Komando-x64.msi](https://github.com/R4cc/komando/releases/latest/download/Komando-x64.msi)** and run it. Choose **Install for me only** (no administrator rights needed) or **Install for all users** of the PC.
2. Start Komando. The first time it saves a password or key, it asks you to set a master password for the vault.
3. Add your first project and host, and click to connect.

### Without installing (portable)

No installer and no admin rights needed. Komando is a single `.exe` with everything built in.

1. **[Download the latest Komando-x64.exe](https://github.com/R4cc/komando/releases/latest/download/Komando-x64.exe)** and put it anywhere, for example `%LOCALAPPDATA%\Programs\Komando`.
2. Right-click it → **Properties** → tick **Unblock** → **OK**. Or in PowerShell: `Unblock-File .\Komando-x64.exe`.
3. Double-click to start. Pin it to the taskbar if you like.

Unblocking removes the "downloaded from the internet" mark that makes Windows SmartScreen warn about apps without a code-signing certificate. Komando isn't signed yet. If your company blocks unknown programs entirely (AppLocker), this won't help. You can also [build it yourself](docs/development.md#build-it-yourself-on-windows); a build from source never gets a SmartScreen warning.

Komando runs on Windows 11 only ([why](docs/adr/0001-windows-11-only.md)). Your data stays on your machine in `%APPDATA%\Komando`. There is no recovery for a forgotten master password, but your projects and hosts are stored separately and survive.

## FAQ

**What is Komando?**
A Rust-based SSH client and terminal emulator for Windows 11 that organises servers into projects.

**Is it an alternative to PuTTY, Windows Terminal, Tabby or MobaXterm?**
Yes, if you mostly work on Linux servers over SSH. Komando combines a fast GPU-rendered terminal, a built-in SSH client and a server manager, and it reads PuTTY keys and Tabby highlight rules.

**Does it need WSL or OpenSSH?**
No. SSH runs inside Komando, so remote output never passes through the Windows console layer (ConPTY).

**Where are my passwords stored?**
In an encrypted vault on your computer, protected by your master password. Nothing is sent anywhere.

## Coming soon

Search in output (Ctrl+F), split view, Windows Hello unlock and changing the master password.

## For developers

Build instructions, release process and design notes are in [docs/development.md](docs/development.md). Terms used in the code are in [GLOSSARY.md](GLOSSARY.md).

---

<sub>Keywords: SSH terminal for Windows, SSH client, SSH manager, terminal emulator, Rust terminal, GPU terminal, server management, sysadmin tools, DevOps, Linux server access, PuTTY alternative, Tabby alternative.</sub>
