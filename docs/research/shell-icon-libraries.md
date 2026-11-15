# Local-shell sidebar icon-library research

Researched 2026-11-15. Research only: application code and existing uncommitted icon edits were left untouched; no builds, commits, pushes or CI monitoring. Coverage below comes from upstream repository trees and SVG source, not search-result thumbnails. **The follow-up comparison below inspected real SVGs in browser-rendered sidebar rows at 15, 16 and 18 CSS px. Native GPUI and physical display scaling have not been validated.**

## Recommendation

**Recommendation after the small-size comparison:** **Microsoft Codicons for PowerShell and Command Prompt; Simple Icons for all Linux distributions**, with a shared 16 px icon box and consistent, theme-aware coloring. Codicons has symbols explicitly intended for both shells and an intrinsic 16 × 16 canvas. Simple Icons supplies all 14 requested Linux marks. This is a two-library solution; matching sizing and coloring improves visual consistency but does not make their drawing styles identical.

For the closest single-library option, Codicons also supplies Ubuntu, Debian and generic Linux. It does not supply dedicated marks for the other 11 requested Linux entries, so an all-Codicons implementation would lose distro-specific icons for those entries. Keeping **every distro in Simple Icons** avoids mixing distro drawing families.

MDI shells plus Simple Icons is a useful alternative if a solid PowerShell silhouette is preferred. Tabler shells plus Simple Icons is the lighter outline alternative. None of the audited libraries provides all required shells and distros in one complete, uniform set.

**CMD distinction:** Codicons' `terminal-cmd` is explicitly intended for Command Prompt. MDI console and Tabler terminal/terminal-2 are generic terminal glyphs; they are not dedicated CMD symbols. MS-DOS identifies a different product and was a poor match here. Likewise GNOME Terminal and Windows Terminal identify different applications.

### Small-size comparison

A self-contained interactive comparison is saved locally at `target/shell-icon-comparison/comparison.html`. It displays Codicons, MDI and Tabler shell pairs alongside the **same** 14 Simple Icons distro marks, with 15/16/18/24 px controls, light/dark backgrounds, and optional color accents. Default: 16 px. All SVGs are embedded; there are no runtime network requests in the comparison.

Inspected browser previews: dark at 15 px and 16 px, light at 16 px and 18 px. These are visual observations, not automated recognition measurements:

- Codicons gives PowerShell and CMD distinct slanted and rectangular outlines without the glossy PNG treatment or an MS-DOS wordmark. Its dedicated 16 px art is the strongest fit for compact native rows in this comparison.
- MDI's filled PowerShell mark has a heavier silhouette; its CMD console mark is generic. Tabler uses lighter 24-unit outlines scaled down to the sidebar size.
- Simple Icons removes the current mixture of badges, gradients and multicolor logos. It gives the distro set one monochrome treatment, optionally with restrained accents. Brand shapes still vary in complexity.
- Kali and CentOS remain fine-detail marks at 15–16 px. Larger 18 px slots help, but vector resolution alone cannot remove their geometric detail. Retain text labels; do not promise that every distro can be recognized from the small mark alone.
- Do not additionally dim idle artwork by 45%: current `render_item` applies `opacity(0.55)` to idle icons in `src/ui/workspace.rs`. This compounds the small-size contrast problem. Inactive labels can remain subdued while icon contrast stays useful.

The preview does not prove GPUI's exact antialiasing, focus behavior or appearance on the user's monitors. Adopt SVG paths/assets with retained attributions; no icon font dependency is needed. Application code has not been changed by this research.

### Codicons source check

Pinned [Microsoft Codicons source](https://github.com/microsoft/vscode-codicons/tree/8d1826a9bce0a52283ecaeb63499c149e97a4f14/src/icons), revision `8d1826a9bce0a52283ecaeb63499c149e97a4f14`. Recursive source tree was not truncated. Fetched and inspected all five SVGs below; each has a 16 × 16 viewBox and `fill="currentColor"`:

- [terminal-powershell.svg](https://raw.githubusercontent.com/microsoft/vscode-codicons/8d1826a9bce0a52283ecaeb63499c149e97a4f14/src/icons/terminal-powershell.svg)
- [terminal-cmd.svg](https://raw.githubusercontent.com/microsoft/vscode-codicons/8d1826a9bce0a52283ecaeb63499c149e97a4f14/src/icons/terminal-cmd.svg)
- [terminal-ubuntu.svg](https://raw.githubusercontent.com/microsoft/vscode-codicons/8d1826a9bce0a52283ecaeb63499c149e97a4f14/src/icons/terminal-ubuntu.svg)
- [terminal-debian.svg](https://raw.githubusercontent.com/microsoft/vscode-codicons/8d1826a9bce0a52283ecaeb63499c149e97a4f14/src/icons/terminal-debian.svg)
- [terminal-linux.svg](https://raw.githubusercontent.com/microsoft/vscode-codicons/8d1826a9bce0a52283ecaeb63499c149e97a4f14/src/icons/terminal-linux.svg)

The [upstream terminal-icon request](https://github.com/microsoft/vscode-codicons/issues/51) specifically describes Command Prompt, PowerShell and Linux/WSL fallback. The [icon lookup](https://microsoft.github.io/vscode-codicons/dist/codicon.html) names them accordingly. The pinned [LICENSE](https://github.com/microsoft/vscode-codicons/blob/8d1826a9bce0a52283ecaeb63499c149e97a4f14/LICENSE) is CC-BY-4.0; retain attribution if adopted. Current [VS Code sizing guidance](https://github.com/microsoft/vscode/blob/main/.github/instructions/design-tokens.instructions.md) uses 16 px as the base Codicon size, with separate compact variants where available.

### Papirus source check

[Papirus](https://github.com/PapirusDevelopmentTeam/papirus-icon-theme) is a relevant alternative because it has size-specific SVG artwork, including [16 px apps](https://github.com/PapirusDevelopmentTeam/papirus-icon-theme/tree/33d971eb2d0c8454bac58246687de78e58f960c8/Papirus/16x16/apps). Checked revision `33d971eb2d0c8454bac58246687de78e58f960c8`. The root recursive tree was truncated, so the 16 × 16 and 48 × 48 subtrees were fetched separately and were **not** truncated. Neither subtree contains a PowerShell/pwsh filename or AlmaLinux/Rocky mark. Its `utilities-terminal` is generic; it does not solve the complete single-library requirement. The upstream theme is GPL-3.0. No Papirus artwork is used in the final comparison.

## Audited revisions

| Library | Pinned catalog/source | Release context |
|---|---|---|
| Pictogrammers / Templarian MaterialDesign (MDI) | [Complete SVG tree](https://github.com/Templarian/MaterialDesign/tree/2424e748e0cc63ab7b9c095a099b9fe239b737c0/svg), revision `2424e748e0cc63ab7b9c095a099b9fe239b737c0`; [metadata](https://raw.githubusercontent.com/Templarian/MaterialDesign/2424e748e0cc63ab7b9c095a099b9fe239b737c0/meta.json) | Official [catalog](https://pictogrammers.com/library/mdi/) currently labels itself **7.4.47**. The inspected source revision is a catalog snapshot; do not equate it with a separately verified release tag. |
| Tabler | [Complete outline SVG tree](https://github.com/tabler/tabler-icons/tree/4581ad20d8d13e24f6c3be75caaea1c4ad6c4b35/icons/outline), revision `4581ad20d8d13e24f6c3be75caaea1c4ad6c4b35` | [v3.52.0](https://github.com/tabler/tabler-icons/releases/tag/v3.52.0), released 2026-11-14; the release target matches this revision. |
| Simple Icons | [Complete SVG tree](https://github.com/simple-icons/simple-icons/tree/74fa843c81f86d7fe28fe54b6f206efb3104af13/icons), revision `74fa843c81f86d7fe28fe54b6f206efb3104af13`; [metadata](https://raw.githubusercontent.com/simple-icons/simple-icons/74fa843c81f86d7fe28fe54b6f206efb3104af13/data/simple-icons.json) | [16.38.0](https://github.com/simple-icons/simple-icons/releases/tag/16.38.0), released 2026-11-15. Coverage was rechecked against the release commit, not just the development branch. |

GitHub recursive tree responses for these three pinned catalogs were **not truncated**. All 14 linked Simple Icons Linux SVGs were fetched and parsed; their titles match the table and their viewBoxes are 24 × 24.

## Shell candidates: exact SVGs

| Intended use | MDI | Tabler outline | Simple Icons |
|---|---|---|---|
| PowerShell | [powershell.svg][mdi-powershell]: dedicated PowerShell mark, solid slanted panel with cutout prompt | [brand-powershell.svg][tabler-brand-powershell]: dedicated PowerShell mark, outlined slanted panel | No PowerShell SVG in the pinned current catalog |
| Command Prompt | [console.svg][mdi-console]: generic framed console, **not CMD branding** | [terminal-2.svg][tabler-terminal-2]: generic framed terminal, **not CMD branding** | No dedicated Command Prompt SVG |
| Simpler CMD alternative | [console-line.svg][mdi-console-line]: unframed generic prompt | [terminal.svg][tabler-terminal]: unframed generic prompt | No generic UI terminal glyph; `gnometerminal.svg` is an application brand |

MDI metadata records `powershell`, `console` and `console-line` as **not deprecated**. Tabler's terminal SVG comments classify them as System/Development icons and include CMD among search tags; that does not turn them into CMD logos. Sources: linked SVGs and MDI metadata above.

## Linux coverage: exact SVGs

A filename link means the dedicated distribution mark exists. **— means absent from the inspected catalog**, not “substitute a related distro.” Generic Linux is explicitly its own fallback row; it does not count as dedicated coverage for missing distros.

| Distribution | MDI | Tabler outline | Simple Icons |
|---|---|---|---|
| Ubuntu | [ubuntu.svg][mdi-ubuntu] | [brand-ubuntu.svg][tabler-brand-ubuntu] | [ubuntu.svg][si-ubuntu] |
| Debian | [debian.svg][mdi-debian] | [brand-debian.svg][tabler-brand-debian] | [debian.svg][si-debian] |
| Kali Linux | — | — | [kalilinux.svg][si-kalilinux] |
| openSUSE | — | — | [opensuse.svg][si-opensuse] |
| Fedora | [fedora.svg][mdi-fedora] | — | [fedora.svg][si-fedora] |
| AlmaLinux | — | — | [almalinux.svg][si-almalinux] |
| Rocky Linux | — | — | [rockylinux.svg][si-rockylinux] |
| CentOS | [centos.svg][mdi-centos] | — | [centos.svg][si-centos] |
| Red Hat | [redhat.svg][mdi-redhat] | [brand-redhat.svg][tabler-brand-redhat] | [redhat.svg][si-redhat] |
| Arch Linux | — | — | [archlinux.svg][si-archlinux] |
| Gentoo | [gentoo.svg][mdi-gentoo] | — | [gentoo.svg][si-gentoo] |
| NixOS | — | — | [nixos.svg][si-nixos] |
| Linux Mint | [linux-mint.svg][mdi-linux-mint] | — | [linuxmint.svg][si-linuxmint] |
| Generic Linux / Tux | [linux.svg][mdi-linux] | — | [linux.svg][si-linux] |
| **Dedicated entries, including generic Linux** | **8 / 14** | **3 / 14** | **14 / 14** |

**Every one of the eight MDI entries above is marked `deprecated: true` and tagged Brand / Logo in its pinned metadata.** Existing SVG availability is real, but a new complete distro family should not depend on their continued upstream maintenance. Tabler has no generic Linux/Tux mark in the inspected catalog. Simple Icons has the required missing brands without substituting SUSE for openSUSE, CentOS for Alma/Rocky, or Red Hat for Fedora.

Autodetection should map distro identity to these marks rather than expect a catalog icon for each versioned WSL registration name. Ubuntu releases can share Ubuntu; openSUSE variants can share openSUSE. Unknown/imported distributions need a generic Linux fallback and their text label. This is a mapping proposal, not a change to detection code.

## Style and the actual 15 CSS px constraint

- **MDI:** the selected files use filled paths on a 24-unit canvas. PowerShell's panel occupies y=4…20, so it is only **10 CSS px tall inside a 15 px box**. Its cutout underline is about 1.69 units, or **1.06 CSS px**, thick. This is why a large SVG preview cannot settle its sidebar legibility. The framed console gives CMD a distinct rectangular silhouette.
- **Tabler:** [upstream design description](https://github.com/tabler/tabler-icons/blob/4581ad20d8d13e24f6c3be75caaea1c4ad6c4b35/README.md) specifies a 24 × 24 grid and 2 px strokes. The linked candidates confirm rounded caps/joins, no fill and `stroke-width="2"`: at 15 px that becomes **1.25 CSS px**, before device scaling and antialiasing. The framed PowerShell/terminal pair is stylistically coherent; the unframed prompt spends less area on a border.
- **Simple Icons:** all linked marks are monochrome filled brand paths on 24 × 24 viewBoxes. They normalize branding, not stroke weight or optical size. Kali's thin, wide dragon, detailed Tux, AlmaLinux's detail and openSUSE's internal shapes deserve particular scrutiny; the shared viewBox does not prove equal recognition at 15 px. Sources: exact SVGs above.

**Main-agent comparison:** render 15 × 15 CSS px in realistic sidebar rows with labels, at 100% viewing size, on light/dark backgrounds and at 1× and 2× device scale. Judge PowerShell versus CMD differentiation, distro recognition, internal gaps and optical balance. Include enlarged companions only as explanatory aids. None of these candidates has passed that evaluation in this research task.

## Licenses and practical attribution facts

- **MDI:** the pinned [LICENSE](https://github.com/Templarian/MaterialDesign/blob/2424e748e0cc63ab7b9c095a099b9fe239b737c0/LICENSE) and current [Pictogrammers license page](https://pictogrammers.com/docs/general/license/) identify icons as Apache-2.0, with an explicit **Brand / Logos category exception**. The selected PowerShell and console records are outside that category; the eight Linux marks are inside it. Preserve license/provenance notices for adopted assets; do not label every MDI distro mark Apache-2.0 solely from the package heading. Their deprecation is also a maintenance tradeoff, not evidence that embedding any SVG is categorically prohibited.
- **Tabler:** pinned [MIT license](https://github.com/tabler/tabler-icons/blob/4581ad20d8d13e24f6c3be75caaea1c4ad6c4b35/LICENSE), copyright 2020–2026 Paweł Kuna. Retain its copyright and permission notice with redistributed assets.
- **Simple Icons:** pinned [package license](https://github.com/simple-icons/simple-icons/blob/74fa843c81f86d7fe28fe54b6f206efb3104af13/LICENSE.md) is CC0-1.0, but its [disclaimer](https://github.com/simple-icons/simple-icons/blob/74fa843c81f86d7fe28fe54b6f206efb3104af13/DISCLAIMER.md) explicitly distinguishes the package from individual icon licensing. Current release metadata records Debian **CC-BY-SA-3.0**, Gentoo **CC-BY-SA-2.5**, NixOS **CC-BY-4.0**, Rocky Linux **CC-BY-SA-4.0**, and Fedora a **custom** license linked to its official brand page. These are verified catalog attribution records, not an independent legal determination or a reason to reject the library. Keep per-icon source/license links and relevant credits in the asset attribution file; a missing license field is not proof of CC0.

The coverage audit above remains useful; the later Codicons and browser-comparison findings at the top refine the final recommendation. No production icon replacement was made in this research task.

[mdi-ubuntu]: https://raw.githubusercontent.com/Templarian/MaterialDesign/2424e748e0cc63ab7b9c095a099b9fe239b737c0/svg/ubuntu.svg
[tabler-brand-ubuntu]: https://raw.githubusercontent.com/tabler/tabler-icons/4581ad20d8d13e24f6c3be75caaea1c4ad6c4b35/icons/outline/brand-ubuntu.svg
[si-ubuntu]: https://raw.githubusercontent.com/simple-icons/simple-icons/74fa843c81f86d7fe28fe54b6f206efb3104af13/icons/ubuntu.svg
[mdi-debian]: https://raw.githubusercontent.com/Templarian/MaterialDesign/2424e748e0cc63ab7b9c095a099b9fe239b737c0/svg/debian.svg
[tabler-brand-debian]: https://raw.githubusercontent.com/tabler/tabler-icons/4581ad20d8d13e24f6c3be75caaea1c4ad6c4b35/icons/outline/brand-debian.svg
[si-debian]: https://raw.githubusercontent.com/simple-icons/simple-icons/74fa843c81f86d7fe28fe54b6f206efb3104af13/icons/debian.svg
[si-kalilinux]: https://raw.githubusercontent.com/simple-icons/simple-icons/74fa843c81f86d7fe28fe54b6f206efb3104af13/icons/kalilinux.svg
[si-opensuse]: https://raw.githubusercontent.com/simple-icons/simple-icons/74fa843c81f86d7fe28fe54b6f206efb3104af13/icons/opensuse.svg
[mdi-fedora]: https://raw.githubusercontent.com/Templarian/MaterialDesign/2424e748e0cc63ab7b9c095a099b9fe239b737c0/svg/fedora.svg
[si-fedora]: https://raw.githubusercontent.com/simple-icons/simple-icons/74fa843c81f86d7fe28fe54b6f206efb3104af13/icons/fedora.svg
[si-almalinux]: https://raw.githubusercontent.com/simple-icons/simple-icons/74fa843c81f86d7fe28fe54b6f206efb3104af13/icons/almalinux.svg
[si-rockylinux]: https://raw.githubusercontent.com/simple-icons/simple-icons/74fa843c81f86d7fe28fe54b6f206efb3104af13/icons/rockylinux.svg
[mdi-centos]: https://raw.githubusercontent.com/Templarian/MaterialDesign/2424e748e0cc63ab7b9c095a099b9fe239b737c0/svg/centos.svg
[si-centos]: https://raw.githubusercontent.com/simple-icons/simple-icons/74fa843c81f86d7fe28fe54b6f206efb3104af13/icons/centos.svg
[mdi-redhat]: https://raw.githubusercontent.com/Templarian/MaterialDesign/2424e748e0cc63ab7b9c095a099b9fe239b737c0/svg/redhat.svg
[tabler-brand-redhat]: https://raw.githubusercontent.com/tabler/tabler-icons/4581ad20d8d13e24f6c3be75caaea1c4ad6c4b35/icons/outline/brand-redhat.svg
[si-redhat]: https://raw.githubusercontent.com/simple-icons/simple-icons/74fa843c81f86d7fe28fe54b6f206efb3104af13/icons/redhat.svg
[si-archlinux]: https://raw.githubusercontent.com/simple-icons/simple-icons/74fa843c81f86d7fe28fe54b6f206efb3104af13/icons/archlinux.svg
[mdi-gentoo]: https://raw.githubusercontent.com/Templarian/MaterialDesign/2424e748e0cc63ab7b9c095a099b9fe239b737c0/svg/gentoo.svg
[si-gentoo]: https://raw.githubusercontent.com/simple-icons/simple-icons/74fa843c81f86d7fe28fe54b6f206efb3104af13/icons/gentoo.svg
[si-nixos]: https://raw.githubusercontent.com/simple-icons/simple-icons/74fa843c81f86d7fe28fe54b6f206efb3104af13/icons/nixos.svg
[mdi-linux-mint]: https://raw.githubusercontent.com/Templarian/MaterialDesign/2424e748e0cc63ab7b9c095a099b9fe239b737c0/svg/linux-mint.svg
[si-linuxmint]: https://raw.githubusercontent.com/simple-icons/simple-icons/74fa843c81f86d7fe28fe54b6f206efb3104af13/icons/linuxmint.svg
[mdi-linux]: https://raw.githubusercontent.com/Templarian/MaterialDesign/2424e748e0cc63ab7b9c095a099b9fe239b737c0/svg/linux.svg
[si-linux]: https://raw.githubusercontent.com/simple-icons/simple-icons/74fa843c81f86d7fe28fe54b6f206efb3104af13/icons/linux.svg
[mdi-powershell]: https://raw.githubusercontent.com/Templarian/MaterialDesign/2424e748e0cc63ab7b9c095a099b9fe239b737c0/svg/powershell.svg
[mdi-console]: https://raw.githubusercontent.com/Templarian/MaterialDesign/2424e748e0cc63ab7b9c095a099b9fe239b737c0/svg/console.svg
[mdi-console-line]: https://raw.githubusercontent.com/Templarian/MaterialDesign/2424e748e0cc63ab7b9c095a099b9fe239b737c0/svg/console-line.svg
[tabler-brand-powershell]: https://raw.githubusercontent.com/tabler/tabler-icons/4581ad20d8d13e24f6c3be75caaea1c4ad6c4b35/icons/outline/brand-powershell.svg
[tabler-terminal]: https://raw.githubusercontent.com/tabler/tabler-icons/4581ad20d8d13e24f6c3be75caaea1c4ad6c4b35/icons/outline/terminal.svg
[tabler-terminal-2]: https://raw.githubusercontent.com/tabler/tabler-icons/4581ad20d8d13e24f6c3be75caaea1c4ad6c4b35/icons/outline/terminal-2.svg


## Selected implementation

The user selected **Tabler outline** from the actual-size comparison. Automatic PowerShell
and Command Prompt icons therefore use `brand-powershell` and `terminal-2`, with all WSL
distros from Simple Icons as shown in that comparison. Shared 16 px sidebar sizing and
theme-aware accents are used. Devicon choices remain available for manual selection.
Native GPUI checks passed for the default shell sidebar in dark and light themes, the
Local Shell list, Tabler icon search, and automatic artwork for a configured Ubuntu WSL
shell. The full library test suite and SVG decoder checks passed. No WSL shell was launched.
