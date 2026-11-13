---
status: accepted (supersedes the "saved Host" part of ADR 0006)
---

# Jump Hosts are typed in where they are used, not saved Hosts

ADR 0006 made a Jump Host a saved Host that other Hosts point at, so its Credential and Host
Key were stored once. In practice that forced every bastion into a Project's sidebar, and the
host form's "From project / Jump host / Direct" choice was hard to read. A Jump Host is now an
address, port, user and optional password typed into the Host (or Project) that uses it, with
the password in the Vault like any other Credential. A Host lists any number of them, in the
order they are signed in to, and adds them after the Project's, which it can switch off.

How a connection runs is unchanged: Komando still signs in to each Jump Host itself and tunnels
through it (ADR 0006), and Host Keys are still trusted per address, so two Hosts typing the same
Jump Host share its Host Key.

## Consequences

- A Jump Host's password is saved once per place it is typed. The same bastion typed into two
  Hosts has two entries; Duplicate copies them, sharing the Credential.
- No cycles or missing Jump Hosts can occur, so forms no longer check routes.
- Libraries from before this change are converted on load: each Jump Host Host becomes typed-in
  entries (its own Jump Hosts first) with its address, port, user and Credential. The Hosts
  themselves stay.
