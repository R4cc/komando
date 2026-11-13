# Komando

## Builds and CI

- Monitor CI only when the user explicitly asks. Pushing changes does not authorize CI
  monitoring or waiting for CI to finish.
- Run local builds only when expected to finish in under two minutes. Skip builds known
  to take longer unless the user explicitly requests that build, and report what was skipped.

## Versioning

The version lives in `Cargo.toml` (`version = "X.Y.Z"`) and is shown in the app (Settings → About).

- **Change the version only when the user explicitly asks**, including patch, minor and major
  bumps. You may recommend a bump and explain why; leave the version unchanged until requested.
- When a bump is requested, update `Cargo.toml` and the matching package version in
  `Cargo.lock` in the same commit, and add the version's entry to `CHANGELOG.md`.
- Keep user-facing changes under `Unreleased` in `CHANGELOG.md` until a version bump or
  release is explicitly requested.
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
