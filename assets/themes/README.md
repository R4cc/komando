# Native themes

`catalog.json` contains the original light and dark style tokens from the nine user-selected
tweakcn pages. Source links are stored beside each theme. Claude + is displayed as
**Komando Default**; its original name and colors are retained in the export.

A theme is only its colors. The exports also list fonts, corner radii, shadows and spacing;
those are ignored, so every theme uses Geist for the interface, JetBrains Mono for
monospace text, an 8px base radius and no shadows. The native adapter converts hex and OKLCH colors once and preserves
separate semantic foreground/background pairs.
Foregrounds below 4.5:1 contrast are minimally adjusted for native labels and controls;
the original tokens are unchanged. Terminal colors are chosen separately (see
`src/theme/terminal_colors.rs`); with **Match theme**, the terminal derives its foreground,
background, cursor and selection from the same variant, retaining semantic ANSI colors and
the standard xterm 256-color cube. Selection text uses its own readable foreground.

Inter and JetBrains Mono are bundled under `assets/fonts/themes` from [Google Fonts](https://github.com/google/fonts/tree/main/ofl) with the upstream SIL Open
Font License, alongside other families that stay available as terminal fonts. The terminal
uses bundled Cascadia Mono unless a font is set in Settings.

FL-Theme's source supplies a light-colored background in both variants; its dark variant
is replaced by a true dark palette in `refinements.json`. Terminal highlight contrast
follows the actual background brightness.

Surfaces are solid by default. Optional Mica remains a separate appearance setting.

The dropdown swatches show the refined background, sidebar, primary and accent colors,
in that order. Selecting a theme persists it separately for
light and dark mode; removed or unknown IDs resolve to Komando Default.
