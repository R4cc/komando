# Development

## Build from source

```powershell
cargo build --release      # target\release\komando.exe
cargo test --lib
```

## Versions and releases

The version is in `Cargo.toml` and every change is listed in [CHANGELOG.md](../CHANGELOG.md).
Patch versions (0.1.x) go up with each change; minor and major versions are bumped by the
maintainer.

Publish a GitHub release with a tag that matches `Cargo.toml`, like `v0.2.0`. The
[release workflow](../.github/workflows/release.yml) builds Komando on Windows and attaches
`Komando-0.2.0-x64.msi`, the same installer as `Komando-x64.msi` (which the README download
link points to), and the plain `.exe`. To rebuild an existing release, run the workflow
manually with its tag.

## Developing on Linux

Komando ships for Windows only, but it also builds on Linux, which is handy for tests and
screenshots. You need the X11 libraries (`libxcb`, `libxkbcommon`, `libxkbcommon-x11`). Set
`RUST_FONTCONFIG_DLOPEN=on` if fontconfig has no pkg-config file. `examples/demo.rs` seeds a
throwaway data folder and opens a few sessions; it's what produced the README screenshot.

## Design notes

- [GLOSSARY.md](../GLOSSARY.md): the terms used in code and docs (Project, Host, Session,
  Vault, …).
- [docs/adr](adr): decisions and why: Windows only, embedded SSH client, native GPU UI,
  master-password vault, a sidebar of hosts instead of tabs.
