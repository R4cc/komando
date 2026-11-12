# Embedded SSH client instead of the system ssh.exe

SSH Sessions use an SSH library inside the Komando process (russh), not the Windows OpenSSH `ssh.exe`. Running `ssh.exe` would send all output through ConPTY, which is a known throughput bottleneck and rewrites the escape-sequence stream. Speed under heavy output is the top priority. With the embedded client, network bytes go straight into the terminal parser, and Komando knows for certain whether authentication succeeded, which "save password" depends on.

## Consequences

- `~/.ssh/config` is not read automatically. Hosts are configured in Komando.
- Only password and private-key authentication are supported. Kerberos/GSSAPI and ssh-agent forwarding are out of scope for v1. (Jump hosts were too; see ADR 0006.)
- Local Sessions (PowerShell, cmd) still go through ConPTY. That is unavoidable for local Windows shells.
