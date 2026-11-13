---
status: accepted; the "saved Host" part is superseded by ADR 0007
---

# Jump Hosts run inside the embedded client

ADR 0002 left jump hosts out of v1. They are now supported, and they stay inside the embedded
SSH client: Komando signs in to each Jump Host itself and opens the next connection through
an SSH tunnel (`direct-tcpip`) on the previous one, rather than using OpenSSH's `ProxyJump`
or an agent on the Jump Host. Each hop checks its own Host Key against Komando's known hosts
and signs in with its own Credential from the Vault, so no key or password ever reaches a
Jump Host, and Komando knows which hop failed and why. That is what the route page shows.

A Jump Host is a saved Host, not an address typed into another Host, so its Credential and
Host Key are stored once and shared by every Host behind it.

## Consequences

- A Host reached through a Jump Host takes its settings from the Project it was opened from.
  The Jump Host uses the same Project if it is in it, otherwise the first Project that holds it.
- Cycles (A through B through A) are refused when a form is saved.
- Deleting a Jump Host makes the Hosts behind it connect directly.
