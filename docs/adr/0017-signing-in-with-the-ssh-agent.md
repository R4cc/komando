---
status: accepted
---

# Signing in with the SSH Agent, and Agent Forwarding off unless switched on

Many people keep their keys in an SSH Agent (1Password, Bitwarden, KeePassXC, Secretive, a
YubiKey, plain `ssh-agent`) and won't, or can't, copy them into the Vault: a hardware key never
leaves its device. The embedded client (ADR 0002) talks to the agent itself, with russh's agent
client. The agent keeps the private keys; Komando asks it for public keys and signatures only.

## Where the agent is

Settings → Security → "SSH agent". Empty finds the usual one: `SSH_AUTH_SOCK` on Linux and
macOS; on Windows `SSH_AUTH_SOCK` if it is set, else the OpenSSH agent's pipe
(`\\.\pipe\openssh-ssh-agent`, where 1Password answers too), and Pageant if the pipe isn't
there or holds no keys. A path names another agent (1Password's and Secretive's sockets on
macOS aren't in `SSH_AUTH_SOCK` for apps started from the Dock), `pageant` picks Pageant, and
`none` switches the agent off, like `IdentityAgent none`. Komando doesn't read `IdentityAgent`
from `~/.ssh/config` (ADR 0002: the config isn't read).

## When the agent is used

Automatically, whenever an agent answers, as `ssh` does: nothing to switch on, and a user
without an agent never notices. The order on each hop:

1. A key saved in the Vault for it.
2. The agent's keys.
3. Passwords and keyboard-interactive, saved or asked.

A **saved password goes before the agent's keys**, though. Servers count every key offered as a
failed try (OpenSSH's `MaxAuthTries`, 6 by default), so an agent with many keys would use them
up before the password saved for the Host, and a Host that signed in before agent support would
stop working. With a saved password the agent is tried only if the server rejects it (then
before a password is asked for), asks for a key as well (partial success), or takes keys only.

Each key is offered before it is signed with (russh waits for the server's `PK_OK`), so an
agent that asks for approval (1Password, a YubiKey touch) asks only for a key the server takes.
A key the agent declines to sign with is skipped. If the server closes the connection after
too many keys, the error says to choose the Host's key.

A Host chooses **Any key** (the default), **One key** (by its SHA-256 fingerprint, picked from
the agent's list in the host form) or **Off**. Jump Hosts are typed in (ADR 0007) and offer every
key, as `ssh -J` does. A key the agent no longer holds is skipped with a warning in the
connection log.

The agent connection is bound to the server's key exchange (OpenSSH's
`session-bind@openssh.com`) when the agent supports it, so keys limited to certain servers work.

## Agent Forwarding

Off by default. A Project Default, which each Host takes, switches on or off for itself. The
form warns that root on the server can use the forwarded keys while the Session is connected.
It is forwarded to the Host only, never to its Jump Hosts. Komando asks for it on the shell's
channel (`auth-agent-req@openssh.com`), carries each agent channel the server opens to the local
agent, and refuses agent channels on any connection it didn't ask for.

## Consequences

- `library.toml` gains `agent` on a Host (left out for any key, a fingerprint for one key,
  `false` for off), and `forward_agent` on a Host and a Project. An older Komando ignores them.
- Project Files carry Agent Forwarding and "agent off", not a chosen key: whoever imports the
  file has keys of their own.
- Settings → Import takes `ForwardAgent yes` and `IdentityAgent none` from `~/.ssh/config`, and
  Tabby's agent forwarding. Another `IdentityAgent` path is listed as left out. The review shows
  a host that comes over with Agent Forwarding on, with the same warning as the forms.
- On Windows, forwarded requests go to whichever of the OpenSSH agent and Pageant has keys, as
  signing in does. If both have keys, Pageant's aren't used.
- The tests run an in-process agent on a Unix socket. Pageant and the Windows pipe are only
  type-checked off Windows.
