# Sharing a Project copies how to reach its Hosts, never a secret

"Share project…" in a Project's menu saves a Project File (`<name>.komando`, TOML) to send to
someone else. It holds the Project's name, colour, icon, default user, Jump Hosts and Startup
Commands, and each of its Hosts with its name, address, port, user, Jump Hosts, Startup
Commands, colour, icon and whether it takes the Project's Jump Hosts and Startup Commands.

It never holds a Credential: no password, key or passphrase, and not even a Credential's id.
The file is written from types of its own (`share.rs`) that have no field a Credential could
go into, so adding a setting to the Library can't leak one into shared files by accident. Ids,
Local Shells (programs on this PC) and Host Keys are left out too. Host Keys are public, but
trusting one because a file says so would skip the check made on first connection.

Settings → Import reads a Project File into the same `Found` list as the Tabby import (ADR
0008), so it is picked, reviewed and added the same way. Each Host carries the user, Jump
Hosts and Startup Commands it ends up with in the shared Project. Added to a new Project, the
Project gets the shared defaults and the Hosts take them from it. Added to an existing Project
of the same name, that Project keeps its defaults; a Host takes them when they start its own
list, otherwise it switches them off and gets the whole list, so it reaches its machine the
same way it did for the person who shared it.

## Consequences

- The person importing is asked for every password, the Jump Hosts' included, on first
  connection, and can save them in their own Vault.
- Startup Commands come over as typed, with what they wait for, except one that answers a
  password question (ADR 0010). Someone who put another secret in one, such as a token in an
  `export`, shares it; the file is plain text so it can be checked before it is sent.
- The file starts with `komando_project = 1`. A Komando that reads a higher number says the
  file needs a newer Komando instead of guessing.
