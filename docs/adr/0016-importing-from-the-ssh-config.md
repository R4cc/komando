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
(ADR 0007), `LocalForward`, `RemoteForward` and `DynamicForward` as Port Forwards
(ADR 0015), and `ForwardAgent yes` and `IdentityAgent none` (ADR 0017). `ProxyJump` is resolved like `ssh -J`: each jump's name is looked up in the same
file. The first jump's own `ProxyJump` comes before it. Later jumps are reached through the
one before them, which overrides theirs.

What stays in the file, listed per host in the review: `IdentityFile` and `CertificateFile`
(keys stay where they are; importing them into the Vault means asking for each passphrase,
which belongs in the host form), `ProxyCommand`, an `IdentityAgent` path or a `ForwardAgent`
socket (Komando uses the agent set in its Settings), X11 forwarding, `RemoteCommand`, `LocalCommand`, `SetEnv`, keep-alives, and forwards Komando can't
represent (Unix sockets, a reverse SOCKS proxy, a specific bind address). A Jump Host's own
key, certificate or proxy command is listed with the host that goes through it ("Private key
of jump host bastion"), since a Jump Host's sign-in is typed in, not imported.

`Match` blocks count when every criterion depends only on the name: `all`, `final`, `host`
(against the host name found so far, as `ssh` checks it), `originalhost`, and `canonical`,
which never holds because Komando doesn't canonicalize. A block that depends on the user, a
command, the network or a tag is left out, with a note saying so.

## Consequences

- The import is a copy. Later edits to `~/.ssh/config` don't reach Komando. Importing again
  starts hosts that are already in Komando unticked (ADR 0008's duplicate check).
- `Include` follows `~`, absolute paths and paths relative to `~/.ssh`, with `*` and `?` in
  the file name only. Missing files are skipped, as `ssh` does. An `Include` that loops back
  to a file it is included from, or names something that isn't a file (a folder, a device), is
  left out with a note: `ssh` refuses a loop, and following one read the files again at every
  level, freezing the import.
- `HostName` takes `%h` and `%%`, as in `ssh`. Any other token is an error for `ssh`: the host
  keeps its name as its address and the `HostName` is listed as left out. A `User` with tokens
  is listed as left out too.
