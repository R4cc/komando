---
status: accepted (supersedes the login script part of ADR 0008)
---

# Startup Commands can wait for text, but never answer a password question

Startup Commands were lines typed one after another, each once the shell had printed something
and gone quiet. That can't follow a command whose output takes a while, such as `sudo -i`
followed by commands that must run as root. A Startup Command now has an optional **Wait
for** text: it is typed once that text appears in the output, after the previous command's
line. Without one it is typed once the shell is quiet, as before. Tabby's login scripts work
the same way (`expect`, then `send`), so Settings → Import brings them over.

The text is matched in the SSH output as it arrives (`session::wait_for`), not in the grid,
so redraws, scrolling and a full Scrollback can't hide or repeat it. Escape sequences are
skipped, and so is the line echoing the command just typed, which may contain the text itself.
The match is case-sensitive and plain text, as in Tabby. Regular expressions and optional steps
are not supported; Tabby scripts using them stay in Tabby and are listed in the review.

A command that waits is typed once its text appears and not later than a minute after the
wait starts: after that, it and the commands after it are dropped and the user is told, so a
command can't be typed into whatever the user is doing much later.

## Startup Commands never answer password questions

A step that waits for a password question ("password", "passphrase") would make its command
a password, saved unencrypted in `library.toml` and copied into every Project File (ADR 0009).
The forms refuse such a step, the Tabby import leaves such a script out ("types a password"),
and Project Files leave such commands out. Komando still stops at a password question while
typing Startup Commands so the user answers it, and a Host's own password belongs in the Vault.

## Consequences

- In `library.toml` a command without Wait for is still plain text, so files without waits
  read the same in older versions. A command with one is `{ wait_for = "...", command = "..." }`,
  which an older Komando can't read.
- A Tabby script added to a Project with Startup Commands of its own switches the Project's
  off for that Host, so it runs exactly the script it ran in Tabby.
