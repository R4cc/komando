---
status: accepted
---

# Command History is kept per Host on this PC, and each entry remembers its address

A shell's `history` is shared by everyone who signs in as that user, so after someone else has
worked on a server it no longer tells you what you did there. Komando keeps its own Command
History for each Host instead: every line entered at a prompt in the Host's SSH Sessions, with
its time. It is on by default and can be switched off (then nothing is recorded) or deleted in
Settings → Security.

It is filled from the Command Marks, which read the entered line off the screen once the shell
has moved past it. Nothing is run or read on the host for it, a password typed without echo
never shows up, and answers to password prompts are skipped as they are for the rail. A command
typed after an extra space is left out, as bash's `ignorespace` does, for secrets on a command
line. Local Sessions are not recorded: their shells keep their own history on this PC.

## A Host's history follows its id; each entry records its address

The file is named after the Host's id (`history/<id>.jsonl` in the data folder), so renaming a
Host or editing its address keeps the history. Changing the address may mean it is now another
machine, or the same machine renumbered; Komando can't tell, so it doesn't guess. Each entry
stores the address (and port, when not 22) its Session was connected to. While all entries match
the Host's current address the list looks plain; once they differ, the list and the exported
file group commands under the address they ran on and mark earlier addresses as such. Nothing is
lost or merged silently, and nothing has to be asked.

- A duplicated Host gets a new id and starts with an empty history.
- A Host in several Projects is one Host and has one history.
- Deleting a Host (or a Project that deletes it) deletes its history.
- Project Files and imports carry no history: it is a record of this PC's use.

## Consequences

- The history is plain text on this PC, like a shell's history file, not in the Vault: reading it
  must not need the Master Password. Each Host keeps its newest 10,000 commands.
- A Session started before its Host's address was edited keeps recording the address it actually
  connected to, until it reconnects.
