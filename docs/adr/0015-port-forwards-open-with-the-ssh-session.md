---
status: accepted
---

# Port Forwards open with the SSH Session, on loopback unless opened to the network

A Host or Project lists Port Forwards (Local, Remote, SOCKS), and they add up like Jump Hosts
and Startup Commands. A Project's open first, then the Host's own, and a Host can leave the
Project's out. They start once every hop has signed in, on the last hop's connection, and stop
when the Session disconnects. There is no separate "start forwarding" step and no forward
without a Session. Forwards are something a Host needs while you work on it, and a Session is
how Komando shows a connection that is up.

## Listening

- Local and SOCKS forwards listen on `127.0.0.1` (plus `::1` when the system has it). Remote
  forwards ask the server for `localhost`. `open_to_network` switches to every interface
  (`0.0.0.0`, and `""` for the server, which needs `GatewayPorts clientspecified` there). The
  default is loopback because a forward reachable from the network also exposes whatever it
  reaches, with the signed-in user's access.
- SOCKS is SOCKS5 CONNECT without authentication (RFC 1928), names resolved on the server.
  SOCKS4, BIND and UDP are refused, and a program speaking SOCKS4 is told to use SOCKS5.
- A forward that can't listen (the port is taken, or reserved, or the server refuses) fails on
  its own and the Session stays connected. A connection the server won't open (forbidden, or
  unreachable) is reported on the forward until a later connection works, and the forward keeps
  listening.

## Consequences

- A second Session of the same Host can't open the same Local ports while the first is
  connected, so its forwards fail with "already in use". This was accepted rather than having
  Sessions share forwards. Sharing would make forwards outlive the Session that shows them.
- The session strip shows the forwards of the active Session only. There is no global list of
  forwards across Sessions.
- `library.toml` and Project Files gain `port_forwards` lists and `use_project_forwards`. An
  older Komando ignores them, so editing a Host there drops its forwards on save.
- Settings → Import brings Tabby's forwarded ports over. One that listens on a specific
  non-loopback address comes over on loopback and is listed as left out.
