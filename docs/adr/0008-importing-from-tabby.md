# Importing from Tabby copies where to connect, not how

Settings → Import reads Tabby's `config.yaml` and adds its SSH profiles as Hosts. A profile
group becomes a Project (nested groups are flattened to "Parent / Child"; ungrouped profiles go
into a Project called "Tabby"). A Host gets the profile's name, address, port, user and an icon
mapped from Tabby's Font Awesome icon to the bundled Lucide set. A new Project takes the user
its group gives its profiles as its default user.

Everything else stays in Tabby: passwords and keys (Tabby keeps passwords in its own vault or
the Windows credential store, and importing secrets silently into the Vault is not something we
want), Jump Hosts (Tabby points at another profile; Komando types them in, see ADR 0007), login
scripts, port forwarding, proxies and colours. Instead of dropping them silently, the review
before importing lists, per Host, each such setting the user changed from Tabby's defaults.
Settings that only change Tabby's own tabs and look are not listed.

Reading (`import::tabby`) is kept apart from matching and adding (`import::plan`,
`import::apply`), so another client's importer only has to produce the same `Found` list.

## Consequences

- A group whose name matches a Project (ignoring case) adds to that Project; its defaults are
  left alone.
- A profile counts as already imported when any Host connects to the same address and port as
  the same user. It is shown but left unticked; the user can still tick it.
- An encrypted Tabby configuration can't be read; the user is told to switch off "Encrypt
  config file" in Tabby for the import.
