# Komando

## Versioning

The version lives in `Cargo.toml` (`version = "X.Y.Z"`) and is shown in the app (Settings → About).

- **Every change you ship bumps the patch number** (0.1.0 → 0.1.1). Do it in the same commit
  as the change: update `Cargo.toml` (and `Cargo.lock` via a build), and add an entry to
  `CHANGELOG.md`.
- **Never bump the minor or major number yourself.** Those are the maintainer's call. When a
  change looks like it deserves one, say so in your reply and explain why. A minor bump fits a
  notable new capability or a change in behaviour users will notice; a major bump fits breaking
  changes to stored data (`library.toml`, `vault.json`, `highlights.json`) or the end of 0.x.
- `CHANGELOG.md` follows Keep a Changelog: newest version first, sections `Added`, `Changed`,
  `Fixed`, `Removed`, `Security`. Write entries for users, not as commit messages.
- A release is a GitHub release tagged `vX.Y.Z`, which must match `Cargo.toml`; the release
  workflow refuses mismatches.

## Agent skills

### Issue tracker

Issues live in GitHub Issues for R4cc/komando, managed with the gh CLI. See `docs/agents/issue-tracker.md`.

### Triage labels

The five default triage labels: needs-triage, needs-info, ready-for-agent, ready-for-human, wontfix. See `docs/agents/triage-labels.md`.

### Domain docs

Single-context: `GLOSSARY.md` and `docs/adr/` at the repo root. See `docs/agents/domain.md`.
