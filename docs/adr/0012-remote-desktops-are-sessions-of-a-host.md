# Remote Desktops are Sessions of a Host

A Host may offer RDP and VNC alongside SSH, each with its own port and Credential, on the
Host's existing address. A Remote Desktop is a Session under that Host rather than an SFTP-like
panel: it needs the pane's space and its own keyboard focus, lifecycle and reconnect behavior.
Both protocols appear by default in the Host's right-click and ⋯ menus: configured protocols
open their desktop, and unconfigured ones open their settings to enter a port and Credential.
Switching a protocol off in the host form hides it from those menus. This choice is stored
separately from its configuration, so older Hosts also offer both protocols until opted out.
The Host chooses which opens first on a click. A Remote Desktop Session always has a row of its own under
the Host, marked with its protocol, and switching between a Host's terminal and desktops
happens there rather than in the title bar, which keeps its layout.
Global viewing defaults live in Settings → Remote desktop.

Komando embeds IronRDP and vnc-rs and draws their framebuffer in GPUI. Launching external
viewers would split focus, credential storage and session ownership between apps; Windows'
RDP ActiveX control would need native child-window hosting that VNC cannot share. Embedded
engines give both protocols the same pane and input behavior at the cost of implementing and
testing the protocol adapters and maintaining their dependencies.

## Consequences

- Desktop passwords belong to the Vault and are independent of the SSH Credential. Old
  libraries have no desktops and continue opening SSH Sessions.
- Through SSH means a `direct-tcpip` channel on an already connected SSH Session, to
  `localhost` on the Host itself. No local listener is exposed; existing Jump Hosts still
  apply to that SSH Session. Ending it ends the desktop's tunnel too.
- RDP requires TLS or NLA with NTLM. Its certificate is pinned by Host address and desktop
  port, after explicit first-use or changed-certificate approval and before credentials are
  sent. Standard unencrypted RDP Security and server redirections are refused.
- VNC initially supports no authentication and classic VNC passwords, with Raw, CopyRect and
  ZRLE updates. New VNC configurations connect directly, as other viewers do: many SSH
  servers refuse `direct-tcpip` channels (`AllowTcpForwarding no`), which made the first
  connection fail. The form recommends SSH, since classic VNC has no transport encryption.
  TLS/VeNCrypt and proprietary authentication are not silently downgraded.
- A Remote Desktop Session can move into a window of its own (dragged out of the main
  window by its row) and back. The Session stays one Session, listed in the main sidebar;
  its connection is untouched by the move. Closing that window returns it to the main
  window rather than disconnecting it, so a desktop is never lost by closing a window.
- The initial scope excludes clipboard, audio, drives, printers, RD Gateway, Kerberos and
  multi-monitor sessions. Project files and the Tabby import remain SSH-only. The experimental
  MobaXterm import also supports direct RDP and VNC sessions (ADR 0011); desktop SSH gateways
  cannot be represented by the tunnel to the Host itself and are left out explicitly.
- Komando now shares plain text through the RDP or VNC clipboard. The toolbar saves the
  sharing preference per desktop, and View only pauses it.
- Protocol work runs outside the UI thread. Picture notifications coalesce while rectangles
  are applied in order; replaced images are released from GPUI's atlas. Dropping a connection
  cancels handshakes and prompts as well as active sessions.
- Each Remote Desktop keeps viewer options for its Host (scaling, quality, view only, which
  system keys it takes), changed from the desktop's own toolbar rather than the host form:
  they are about watching the desktop, not about reaching it. Quality applies when
  connecting (VNC's pixel format, RDP's performance flags), so changing it reconnects.
  Windows' own shortcuts reach the desktop through a keyboard hook (ADR 0013).
