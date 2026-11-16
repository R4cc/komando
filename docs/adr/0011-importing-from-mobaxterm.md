# Importing from MobaXterm copies the way to the host, not the secrets

Settings → Import reads MobaXterm's `MobaXterm.ini` (found in `Documents\MobaXterm`, the same
under OneDrive, or `%APPDATA%\MobaXterm`) or a `.mxtsessions` file exported from MobaXterm, and
adds its SSH, RDP and VNC sessions as Hosts through the same `Found` list, `import::plan` and
`import::apply` as Tabby (ADR 0008). A session folder becomes a Project (nested folders are
flattened to "Parent / Child"; sessions in no folder go into a Project called "MobaXterm").

Unlike Tabby, MobaXterm's SSH gateway is typed into each session as host, port and user, one per
hop, which is what a Komando Jump Host is (ADR 0007). For SSH sessions, the gateway comes over
as the Host's own Jump Hosts, in order, and "Execute command" comes over as one Startup Command.
Every imported Host gets its gateway list even when it is empty, so a direct session added to a Project with
Jump Hosts still connects directly, as it did in MobaXterm.

Direct RDP and VNC sessions become Hosts with a Remote Desktop and open that protocol by
default (ADR 0012). The session's port belongs to the desktop, not SSH. RDP keeps its account,
including `DOMAIN\user` or a UPN; VNC has no user. The Host's SSH user and Credential are
switched off so an unspecified desktop account never takes an unrelated Project's SSH user.
Each session remains a separate Host; importing does not merge sessions by address or modify
an existing Host. Duplicate checks compare the name, protocol, address, port and account (and
for SSH the Jump Hosts and Startup Commands), including Remote Desktops on existing Hosts
regardless of which protocol the Host opens first.

Passwords stay in MobaXterm: `[Passwords]` and `[Credentials]` are never read. A private key
and an SSH proxy are listed in the review as settings that don't come over. Desktop preferences
use Komando's global settings and are listed as left behind. Telnet and other unsupported
session types are counted in a note.

A desktop using a MobaXterm SSH gateway is left out with an explanation. That gateway may
reach a different machine; Komando's desktop tunnel goes through SSH on the destination
itself. Converting it would change the route or assume SSH access that the export never
specified. Direct desktops keep a direct route, including VNC, rather than inventing SSH
settings. No Project Jump Hosts or Startup Commands are inherited by desktop imports.

The format isn't documented. The field positions used (`#icon#type%host%port%user%…`, the
command at 7, the gateway at 8–10, the key at 14, the proxy at 19–22) come from real exports,
kept as test fixtures in `src/import/testdata/`. The RDP fixture covers its host, port and
account. Desktop SSH gateway offsets (RDP 13–15, VNC 5–7) are cross-checked against
[Muxus's parser](https://github.com/flosch62/muxus/blob/main/client/src/mobaxterm-import.ts);
VNC and desktop gateway tests are synthetic, not real exports. The file is written in
Windows-1252; it is read as UTF-8 only when it is valid UTF-8.

## Consequences

- The MobaXterm import is marked experimental in Settings → Import until it has been tried on
  more MobaXterm versions.

- An installed MobaXterm that keeps its sessions in the registry instead of the ini isn't read;
  the user exports the sessions to a file and picks it.
- If a MobaXterm version moves fields, a session may come over with a wrong value; it is shown
  for review before anything is saved, and the fixtures are where to add a new version's file.
