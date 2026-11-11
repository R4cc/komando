# Komando

A Windows 11 terminal for system engineers who work on remote Linux machines over SSH, organised by Project and with saved credentials.

## Organisation

**Project**:
A named group of Hosts that belong together, such as one application or one environment ("Atlas Production"). It sets defaults that its Hosts inherit.
_Avoid_: Workspace, folder, group

**Host**:
A saved SSH destination: address, port, user, Credential and how it looks (icon, color). A Host can belong to more than one Project.
_Avoid_: Profile, server, connection, bookmark

**Host Key**:
The public key a Host's server presents to prove its identity. It is trusted on first connection and checked on every connection after that.
_Avoid_: Fingerprint (that is only its display form), known host

## Running terminals

**Local Shell**:
A saved local shell, such as PowerShell or cmd, that Komando can start. It belongs to a Project or stands alone.
_Avoid_: Local profile

**Session**:
A terminal running for a Host or Local Shell. A Host or Local Shell can have several Sessions. When a Session ends (closed or dropped), its Scrollback stays until it is discarded or Komando closes, and activating it again reconnects in the same terminal.
_Avoid_: Connection, shell, instance, tab

**SSH Session**:
A Session connected to a Host. The shell runs on the remote machine; no local shell is involved.

**Local Session**:
A Session of a Local Shell, running on the Windows machine itself.

**Scrollback**:
All output a Session has received and still holds, from its first line to its current screen.
_Avoid_: History, buffer, log

**Command Mark**:
A line the user entered in a Session (prompt and command), remembered so the command rail can list it and jump back to it. It lasts as long as the line is in the Scrollback.
_Avoid_: Bookmark, history entry

## Secrets

**Vault**:
The encrypted store that holds every Credential. It is locked until the Master Password is entered.
_Avoid_: Keychain, keystore, password manager

**Master Password**:
The single password that unlocks the Vault when Komando starts.

**Credential**:
A secret in the Vault used to authenticate: a password, or a private key with its optional passphrase. One Credential can be shared by many Hosts.
_Avoid_: Secret, login, identity

**Project Credential**:
The Credential a Project's Hosts use by default unless a Host sets its own.
_Avoid_: Project password, default login

## Appearance

**App Theme**:
Light or dark mode for Komando's chrome: sidebar, dialogs and settings. (Setting name
`app_theme`; the UI calls it **Mode**.)

**Theme**:
A named set of colours for one mode, covering the chrome and the terminal: accent, tinted
greys, text and terminal background (Ember, Midnight, Paper, …). Dark and light Themes are
separate lists; the user picks one of each, and the mode decides which one shows.

**Terminal Theme**:
Whether the terminal follows the App Theme or always uses the dark or light Theme's terminal
colours.
_Avoid_: Color scheme (ambiguous)

## Highlighting

**Highlight Rule**:
A pattern (plain text or regex) and the colours or bold style given to output that matches it.
_Avoid_: Keyword (the file format's field name), trigger, filter

**Highlight Set**:
A named, ordered list of Highlight Rules that is imported, exported and switched on or off as a whole. Earlier rules take precedence.
_Avoid_: Profile, theme
