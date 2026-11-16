# Native themes

`catalog.json` contains the original light and dark style tokens from the nine user-selected
tweakcn pages. Source links are stored beside each theme. Claude + is displayed as
**Komando Default**; its original name and colors are retained in the export.

The native adapter converts hex and OKLCH colors once, preserves separate semantic
foreground/background pairs, and maps UI fonts, base corner radii and shadow dimensions.
Foregrounds below 4.5:1 contrast are minimally adjusted for native labels and controls;
the original tokens are unchanged. The terminal derives its foreground, background,
cursor and selection from the same variant, retaining semantic ANSI colors and the
standard xterm 256-color cube. Selection text uses its own readable foreground.

Fonts are bundled under `assets/fonts/themes` with the upstream SIL Open Font License
for each family. Their upstream files come from
[Google Fonts](https://github.com/google/fonts/tree/main/ofl): Outfit, Plus Jakarta Sans,
DM Sans, Inter, Geist Mono, JetBrains Mono, IBM Plex Mono and Fira Code. Salesforce's
Segoe UI uses the Windows system font. Its platform-specific mono stack falls back to
bundled JetBrains Mono. FL-Theme specifies Inter for mono text; the native terminal uses
JetBrains Mono so its grid remains fixed width. Explicit terminal font settings take precedence.

FL-Theme's source supplies a light-colored background in both variants; its dark variant
is replaced by a true dark palette in `refinements.json`. Terminal highlight contrast
follows the actual background brightness.

No font sizes, spacing or pane layout are imported. Existing native proportions are
retained while their corner radii follow the theme's base radius. Surfaces are solid by
default. Optional Mica remains a separate appearance setting,
independent of a theme’s shadows.

The dropdown swatches show the refined background, sidebar, primary and accent colors,
in that order. Selecting a theme persists it separately for
light and dark mode; removed or unknown IDs resolve to Komando Default.
