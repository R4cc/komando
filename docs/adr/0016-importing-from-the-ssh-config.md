---
status: accepted
---

# Importing from the SSH config resolves each host as ssh would

Settings → Import reads `~/.ssh/config` (OpenSSH's client configuration, on every platform) and
the files it `Include`s, and adds one Host for each `Host` block that names a host. A block
with only patterns (`Host *`, `Host *.lan`) is not a host. A block with several names
(`Host web web.prod`) is one host, named after its first name. All hosts go into a Project
called "SSH config", or into an existing Project of that name.

A host's settings are what `ssh` would use for its name. Every block that matches it counts,
in file order, and for each keyword the first value found wins, so `Host *` defaults further
down fill in what a host leaves out. Komando does not keep the patterns themselves. A Project
Default would apply to every Host in the Project, while a pattern applies only to the names it
matches. The resolved values are what the user gets from `ssh` today.

What comes over: `HostName` (with `%h`), `Port`, `User`, `ProxyJump` as typed-in Jump Hosts
(ADR 0007), and `LocalForward`, `RemoteForward` and `DynamicForward` as Port Forwards
(ADR 0015). `ProxyJump` is resolved like `ssh -J`: each jump's name is looked up in the same
file. The first jump's own `ProxyJump` comes before it. Later jumps are reached through the
one before them, which overrides theirs.

What stays in the file, listed per host in the review: `IdentityFile` and `CertificateFile`
(keys stay where they are; importing them into the Vault means asking for each passphrase,
which belongs in the host form), `ProxyCommand`, `ForwardAgent` (until agent support), X11
forwarding, `RemoteCommand`, `LocalCommand`, `SetEnv`, keep-alives, and forwards Komando can't
represent (Unix sockets, a reverse SOCKS proxy, a specific bind address). `Match` blocks are
left out with a note, because they depend on things only known while connecting.

## Consequences

- The import is a copy. Later edits to `~/.ssh/config` don't reach Komando. Importing again
  starts hosts that are already in Komando unticked (ADR 0008's duplicate check).
- `Include` follows `~`, absolute paths and paths relative to `~/.ssh`, with `*` and `?` in
  the file name only. Missing files are skipped, as `ssh` does.
- Tokens other than `%h` and `%%` in `HostName`, and a `User` with tokens, are not expanded.
  Such a user is listed as left out.
