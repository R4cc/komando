# Komando

A Windows 11 terminal for system engineers who work on remote Linux machines over SSH, organised by Project and with saved credentials.

## Organisation

**Project**:
A named group of Hosts that belong together, such as one customer or one environment. It sets defaults that its Hosts inherit.
_Avoid_: Workspace, folder, group

**Host**:
A saved SSH destination: address, port, user, Credential and how it looks (icon, color). A Host can belong to more than one Project.
_Avoid_: Profile, server, connection, bookmark

**Host Key**:
The public key a Host's server presents to prove its identity. It is trusted on first connection and checked on every connection after that.
_Avoid_: Fingerprint (that is only its display form), known host

## Running terminals

**Session**:
One live terminal, either an SSH Session or a Local Session. Opening the same Host twice creates two Sessions.
_Avoid_: Connection, shell, instance

**SSH Session**:
A Session connected to a Host. The shell runs on the remote machine; no local shell is involved.

**Local Session**:
A Session running a shell on the Windows machine itself, such as PowerShell or cmd. Like every Session, it belongs to a Project.
_Avoid_: Local tab, local profile

**Scrollback**:
All output a Session has received and still holds, from its first line to its current screen.
_Avoid_: History, buffer, log

**Tab**:
An entry in the vertical tab sidebar that shows one Session.
_Avoid_: Pane, window

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
Light or dark mode for Komando's chrome: sidebar, dialogs and settings.

**Terminal Theme**:
Light or dark color scheme for terminal content, set independently of the App Theme.
_Avoid_: Color scheme (ambiguous)

## Highlighting

**Highlight Rule**:
A pattern (plain text or regex) and the colours or bold style given to output that matches it.
_Avoid_: Keyword (the file format's field name), trigger, filter

**Highlight Set**:
A named, ordered list of Highlight Rules that is imported, exported and switched on or off as a whole. Earlier rules take precedence.
_Avoid_: Profile, theme
