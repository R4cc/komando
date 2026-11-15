# Shell and WSL symbols

The automatic shell icons use unmodified SVG symbols rather than resized application logos:

- PowerShell: Tabler outline `brand-powershell`.
- Command Prompt: Tabler outline `terminal-2`, a generic console symbol.
- Linux / WSL: Simple Icons distribution marks, with Linux as the fallback.

Tabler is pinned to [4581ad20d8d13e24f6c3be75caaea1c4ad6c4b35](https://github.com/tabler/tabler-icons/tree/4581ad20d8d13e24f6c3be75caaea1c4ad6c4b35),
release v3.52.0. Its [MIT license](LICENSE-tabler) is bundled, including the copyright notice.

Simple Icons is pinned to [74fa843c81f86d7fe28fe54b6f206efb3104af13](https://github.com/simple-icons/simple-icons/tree/74fa843c81f86d7fe28fe54b6f206efb3104af13),
release 16.38.0. Its package [CC0 license](LICENSE-simple-icons.md),
[disclaimer](DISCLAIMER-simple-icons.md), and the selected icons' complete upstream
[source, guideline and license metadata](simple-icons-metadata.json) are bundled.
Individual marks retain their respective licenses and brand notices; the package license
is not a blanket replacement for those records. Credits include the Debian Project,
Gentoo Foundation, NixOS contributors, Rocky Enterprise Software Foundation and Fedora Project.
For the remaining marks, see the linked upstream source/guideline records.

Each SVG stays unmodified. GPUI renders it as a monochrome mask, tinted with a theme-aware
accent. The accent pairs are UI colors, not claims of official brand colors. Shell rows use
16 logical pixels and do not apply additional opacity while idle. SVGs remain sharp at
different display scales without requiring an installed font or network access.

Run `python tools/update-shell-symbols.py` to reproduce the assets, metadata and Rust catalog.
Existing Devicon and legacy `shell:` selections remain supported, and explicit choices
continue to override automatic detection.
