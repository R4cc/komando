//! Laying out the terminal grid for painting: a snapshot of the cells taken under the lock,
//! rows shaped into glyphs with caches that survive frames, links and highlights under the
//! pointer, and box-drawing primitives.

use super::*;

/// One cell copied out of the grid, so text layout runs without holding the terminal lock
/// (the parser keeps reading output meanwhile).
pub(super) struct SnapCell {
    /// Row in the view and column.
    pub(super) row: i32,
    pub(super) col: usize,
    pub(super) c: char,
    /// Zero-width characters on this cell (combining accents, variation selectors, ZWJ): where
    /// they are in the `Snapshot`'s `extras`.
    pub(super) extra: Range<u32>,
    pub(super) fg: Rgb,
    pub(super) bg: Rgb,
    pub(super) underline: Option<Rgb>,
    pub(super) flags: Flags,
    pub(super) selected: bool,
}

#[derive(Clone, Copy)]
pub(super) struct CursorSnap {
    pub(super) row: i32,
    pub(super) col: usize,
    pub(super) shape: CursorShape,
    pub(super) wide: bool,
    pub(super) color: Rgb,
}

/// Vertical text positions within a terminal row, shared by glyphs and decorations.
pub(super) struct RowTextMetrics {
    pub(super) baseline: Pixels,
    pub(super) underline_y: Pixels,
    pub(super) strike_y: Pixels,
}

impl RowTextMetrics {
    pub(super) fn new(line_h: Pixels, ascent: Pixels, descent: Pixels) -> Self {
        // GPUI's Windows backend returns a signed descent below the baseline.
        // Row layout needs its magnitude, otherwise text shifts down relative
        // to the cell backgrounds and cursor, and underlines move above it.
        let descent = descent.abs();
        let padding_top = (line_h - ascent - descent) / 2.;
        Self {
            baseline: padding_top + ascent,
            underline_y: padding_top + ascent + descent * 0.618,
            strike_y: padding_top + ascent * 0.65,
        }
    }
}

/// Everything a row needs to be laid out, shared by all rows of a frame.
pub(super) struct RowContext<'a> {
    pub(super) shaper: &'a dyn Shaper,
    /// Regular, bold, italic, bold italic.
    pub(super) fonts: [gpui::Font; 4],
    pub(super) font_size: Pixels,
    pub(super) origin: gpui::Point<Pixels>,
    pub(super) cell_w: Pixels,
    pub(super) line_h: Pixels,
    pub(super) scale: f32,
    /// From the top of a row: the text baseline, underline and strikethrough.
    pub(super) baseline: Pixels,
    pub(super) underline_y: Pixels,
    pub(super) strike_y: Pixels,
    pub(super) highlighter: Option<&'a highlight::Highlighter>,
    pub(super) dark: bool,
    pub(super) default_bg: Rgb,
    pub(super) selection: Hsla,
    pub(super) selection_foreground: Rgb,
    /// The current search match's cells, (row, columns), and the colour of its text: it sits
    /// on a solid accent background (see `search_colors`).
    pub(super) current_match: &'a [(i32, Range<usize>)],
    pub(super) match_foreground: Rgb,
    pub(super) cursor: Option<CursorSnap>,
    pub(super) focused: bool,
}

/// Buffers `layout_row` reuses from row to row, so a frame does not allocate per row.
#[derive(Default)]
pub(super) struct RowScratch {
    /// The row's text and where each cell starts in it, for highlight rules.
    pub(super) text: String,
    pub(super) starts: Vec<usize>,
    /// The style highlight rules give each byte of `text`.
    pub(super) styles: Vec<Option<highlight::Style>>,
    /// (first column, end column, colour)
    pub(super) backgrounds: Vec<(usize, usize, Rgb)>,
    pub(super) selected: Vec<(usize, usize)>,
    pub(super) prims: Vec<Prim>,
}

/// Bytes `start..end` of a row's text, coloured by a highlight rule.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct StyledSpan {
    pub(super) start: u32,
    pub(super) end: u32,
    pub(super) style: highlight::Style,
}

/// Row texts a generation of the highlight cache grows to before it ages (see `Generations`):
/// a few screens.
pub(super) const HIGHLIGHT_GENERATION: usize = 512;

/// What the highlight rules make of a row's text, worked out once per text. Output scrolling
/// the screen damages every row, so each frame lays out every row again; without this, every
/// rule ran on every row each frame although only the rows that came in were new.
#[derive(Default)]
pub(super) struct HighlightCache {
    /// The rules and the background (dark or not) the entries were worked out for.
    pub(super) key: Option<(ByAddress<highlight::Highlighter>, bool)>,
    pub(super) rows: Generations<Box<str>, Box<[StyledSpan]>>,
    #[cfg(test)]
    pub(super) scans: usize,
}

impl HighlightCache {
    /// Start a frame highlighted by `highlighter` on a dark or light background: forget
    /// everything if either changed, and keep the cache bounded.
    pub(super) fn prepare(&mut self, highlighter: &Arc<highlight::Highlighter>, dark: bool) {
        if !self.key.as_ref().is_some_and(|(h, d)| Arc::ptr_eq(&h.0, highlighter) && *d == dark) {
            self.key = Some((ByAddress(highlighter.clone()), dark));
            self.rows.clear();
        }
        self.rows.age(HIGHLIGHT_GENERATION);
    }

    /// The coloured spans of `text`, from `work_out` if it isn't cached.
    pub(super) fn spans(&mut self, text: &str, work_out: impl FnOnce() -> Box<[StyledSpan]>) -> &[StyledSpan] {
        let Generations { current, previous } = &mut self.rows;
        if !current.contains_key(text) {
            let spans = previous.remove(text).unwrap_or_else(|| {
                #[cfg(test)]
                { self.scans += 1; }
                work_out()
            });
            current.insert(text.into(), spans);
        }
        &current[text]
    }
}

/// Per-byte styles (see `Highlighter::styles_for_background_into`) as spans.
pub(super) fn styled_spans(styles: &[Option<highlight::Style>]) -> Box<[StyledSpan]> {
    let mut spans: Vec<StyledSpan> = Vec::new();
    for (i, style) in styles.iter().enumerate() {
        let Some(style) = *style else { continue };
        match spans.last_mut() {
            Some(span) if span.end as usize == i && span.style == style => span.end += 1,
            _ => spans.push(StyledSpan { start: i as u32, end: i as u32 + 1, style }),
        }
    }
    spans.into()
}

#[derive(Debug, PartialEq)]
pub(super) struct GlyphPaint {
    pub(super) origin: gpui::Point<Pixels>,
    pub(super) font_id: gpui::FontId,
    pub(super) id: gpui::GlyphId,
    pub(super) color: Hsla,
    pub(super) emoji: bool,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) enum DecorationKind {
    Underline,
    Double,
    Curly,
    Strike,
}

#[derive(Debug, PartialEq)]
pub(super) struct Decoration {
    pub(super) kind: DecorationKind,
    pub(super) x0: Pixels,
    pub(super) x1: Pixels,
    pub(super) y: Pixels,
    pub(super) color: Hsla,
}

/// One row of the grid laid out, ready to paint. Rows are kept from frame to frame in
/// `GridCache`, their buffers reused.
#[derive(Default, Debug, PartialEq)]
pub(super) struct RowPaint {
    /// Backgrounds, selection and the block cursor, under the text.
    pub(super) rects: Vec<(Bounds<Pixels>, Hsla)>,
    /// Box drawing and block elements.
    pub(super) prims: Vec<(Prim, Hsla)>,
    pub(super) glyphs: Vec<GlyphPaint>,
    pub(super) decorations: Vec<Decoration>,
    /// Bar and underline cursors, over the text.
    pub(super) cursor_bars: Vec<(Bounds<Pixels>, Hsla)>,
    pub(super) cursor_outline: Option<(Bounds<Pixels>, Hsla)>,
}

impl RowPaint {
    pub(super) fn clear(&mut self) {
        self.rects.clear();
        self.prims.clear();
        self.glyphs.clear();
        self.decorations.clear();
        self.cursor_bars.clear();
        self.cursor_outline = None;
    }
}

/// How Scrollback Search marks its matches in the output, from the Theme's accent.
pub(super) struct SearchColors {
    /// The current match's background, and the colour of its text on it.
    pub(super) current: Hsla,
    pub(super) current_text: Rgb,
    /// Every other match: a tint light enough that coloured text stays readable on it (a
    /// heavier one turns a dark terminal mid-grey, where red, blue and magenta all but vanish),
    /// and an edge that makes it easy to spot.
    pub(super) other: Hsla,
    pub(super) other_edge: Hsla,
}

pub(super) fn search_colors(app: crate::theme::AppPalette) -> SearchColors {
    let text = crate::theme::packed(app.on_brand);
    SearchColors {
        current: Hsla { a: 1., ..app.brand },
        current_text: Rgb { r: (text >> 16) as u8, g: (text >> 8) as u8, b: text as u8 },
        other: Hsla { a: if app.dark { 0.15 } else { 0.25 }, ..app.brand },
        other_edge: Hsla { a: if app.dark { 0.7 } else { 1. }, ..app.brand },
    }
}

/// What a frame draws besides the rows (which `GridCache` holds).
#[derive(Default)]
pub(super) struct Frame {
    /// The row of the Command Mark jumped to, lit under everything.
    pub(super) flash: Option<(Bounds<Pixels>, Hsla)>,
    /// Search matches: over the cells' backgrounds and the selection, under the text.
    pub(super) matches: Vec<(Bounds<Pixels>, Hsla)>,
    /// The edges of the search matches other than the current one, over the text.
    pub(super) outlines: Vec<(Bounds<Pixels>, Hsla)>,
    /// What the IME is composing, over the cursor: its area, text and background.
    pub(super) preedit: Option<(Bounds<Pixels>, ShapedLine, Hsla)>,
    /// The hovered link's underline: where each row of it starts, and how wide it is.
    pub(super) links: Vec<(gpui::Point<Pixels>, Pixels)>,
    pub(super) link_color: Hsla,
    pub(super) font_size: Pixels,
}

/// Lay out one row: merged background runs, glyphs placed on their cells, procedurally
/// drawn line art, underlines and the cursor.
///
/// Glyphs are put on the grid one cell at a time instead of trusting the shaper's advances:
/// a character from a fallback font (CJK, emoji, symbols, Nerd Font icons) has its own
/// width, and would shift everything after it on the line out of its column.
#[allow(clippy::too_many_arguments)]
pub(super) fn layout_row(
    cells: &[SnapCell],
    extras: &[char],
    row: i32,
    input: Option<InputRow>,
    ctx: &RowContext,
    glyphs: &mut GlyphCache,
    highlights: &mut HighlightCache,
    frame: &mut RowPaint,
    scratch: &mut RowScratch,
) {
    let y = ctx.origin.y + ctx.line_h * row as f32;
    let col_x = |col: usize| ctx.origin.x + ctx.cell_w * col as f32;
    let cell_width = |cell: &SnapCell| if cell.flags.contains(Flags::WIDE_CHAR) { 2 } else { 1 };
    let extra = |cell: &SnapCell| &extras[cell.extra.start as usize..cell.extra.end as usize];

    let RowScratch { text, starts, styles, backgrounds, selected, prims } = scratch;
    backgrounds.clear();
    selected.clear();
    // Highlight rules match on the row's text; build it only when there are rules. What the
    // user typed is left as the shell shows it.
    let styles = ctx
        .highlighter
        .filter(|_| input != Some(InputRow::All))
        .is_some_and(|h| {
            text.clear();
            starts.clear();
            for cell in cells {
                starts.push(text.len());
                text.push(cell.c);
                text.extend(extra(cell).iter());
            }
            // On a command line, rules see only the prompt, so the command can't colour it.
            let end = match input {
                Some(InputRow::AfterPrompt) => crate::marks::prompt_end(text).unwrap_or(text.len()),
                _ => text.len(),
            };
            let spans = highlights.spans(&text[..end], || {
                if h.styles_for_background_into(&text[..end], ctx.dark, styles) { styled_spans(styles) } else { Box::default() }
            });
            if spans.is_empty() {
                return false;
            }
            styles.clear();
            styles.resize(text.len(), None);
            for span in spans {
                styles[span.start as usize..span.end as usize].fill(Some(span.style));
            }
            true
        })
        .then_some(&*styles);

    let cursor_here = ctx.cursor.filter(|k| k.row == row);
    let block_cursor = ctx.focused && cursor_here.is_some_and(|k| k.shape == CursorShape::Block);
    let current_match = ctx.current_match.iter().find(|(r, _)| *r == row).map(|(_, cols)| cols);

    for (i, cell) in cells.iter().enumerate() {
        let (mut fg, mut bg) = (cell.fg, cell.bg);
        let mut bold = cell.flags.contains(Flags::BOLD);
        if let Some(rule) = styles.and_then(|s| s[starts[i]]) {
            if let Some(c) = rule.fg {
                fg = Rgb { r: c.r, g: c.g, b: c.b };
            }
            if let Some(c) = rule.bg {
                bg = Rgb { r: c.r, g: c.g, b: c.b };
            }
            bold |= rule.bold;
        }
        if cell.selected { fg = ctx.selection_foreground; }
        let in_current_match = current_match.is_some_and(|cols| cols.contains(&cell.col));
        if in_current_match { fg = ctx.match_foreground; }
        let width = cell_width(cell);
        if block_cursor && cursor_here.is_some_and(|k| k.col == cell.col) {
            fg = bg;
            bg = cursor_here.map_or(bg, |k| k.color);
        }
        if bg != ctx.default_bg {
            match backgrounds.last_mut() {
                Some((_, end, color)) if *end == cell.col && *color == bg => *end += width,
                _ => backgrounds.push((cell.col, cell.col + width, bg)),
            }
        }
        if cell.selected {
            match selected.last_mut() {
                Some((_, end)) if *end == cell.col => *end += width,
                _ => selected.push((cell.col, cell.col + width)),
            }
        }

        let mut color = rgb_to_hsla(fg);
        if cell.flags.contains(Flags::DIM) && !cell.selected && !in_current_match {
            color.a *= 0.66;
        }

        let x = col_x(cell.col);
        let mut drawn = false;
        if matches!(cell.c, '\u{2500}'..='\u{259f}' | '\u{e0b0}'..='\u{e0b3}') {
            let geometry = box_drawing::Cell {
                x: f32::from(x),
                y: f32::from(y),
                w: f32::from(ctx.cell_w) * width as f32,
                h: f32::from(ctx.line_h),
                scale: ctx.scale,
            };
            drawn = box_drawing::draw(cell.c, geometry, prims);
            frame.prims.extend(prims.drain(..).map(|p| (p, color)));
        }
        if !drawn && (cell.c != ' ' || !cell.extra.is_empty()) {
            let style = bold as usize + 2 * cell.flags.contains(Flags::ITALIC) as usize;
            for g in glyphs.get(ctx, style, cell.c, extra(cell)) {
                frame.glyphs.push(GlyphPaint {
                    origin: point(x + g.x, y + ctx.baseline + g.y),
                    font_id: g.font_id,
                    id: g.id,
                    color,
                    emoji: g.emoji,
                });
            }
        }

        let underline = if cell.flags.contains(Flags::UNDERCURL) {
            Some(DecorationKind::Curly)
        } else if cell.flags.contains(Flags::DOUBLE_UNDERLINE) {
            Some(DecorationKind::Double)
        } else if cell.flags.intersects(Flags::ALL_UNDERLINES) {
            Some(DecorationKind::Underline)
        } else {
            None
        };
        let strike = cell.flags.contains(Flags::STRIKEOUT).then_some(DecorationKind::Strike);
        for (kind, color) in [
            (underline, cell.underline.map_or(color, rgb_to_hsla)),
            (strike, color),
        ] {
            let Some(kind) = kind else { continue };
            let (x0, x1) = (x, col_x(cell.col + width));
            match frame.decorations.last_mut() {
                Some(d) if d.kind == kind && d.color == color && d.y == y && d.x1 == x0 => d.x1 = x1,
                _ => frame.decorations.push(Decoration { kind, x0, x1, y, color }),
            }
        }
    }

    for &(start, end, color) in backgrounds.iter() {
        let bounds = Bounds::new(point(col_x(start), y), size(ctx.cell_w * (end - start) as f32, ctx.line_h));
        frame.rects.push((bounds, rgb_to_hsla(color)));
    }
    for &(start, end) in selected.iter() {
        let bounds = Bounds::new(point(col_x(start), y), size(ctx.cell_w * (end - start) as f32, ctx.line_h));
        frame.rects.push((bounds, ctx.selection));
    }

    if let Some(k) = cursor_here {
        let width = ctx.cell_w * if k.wide { 2. } else { 1. };
        let bounds = Bounds::new(point(col_x(k.col), y), size(width, ctx.line_h));
        let color = rgb_to_hsla(k.color);
        let bar = px((2. * ctx.scale).round().max(1.) / ctx.scale);
        match k.shape {
            CursorShape::Block if !ctx.focused => frame.cursor_outline = Some((bounds, color)),
            CursorShape::Block => {} // drawn as the cell's background above
            CursorShape::HollowBlock => frame.cursor_outline = Some((bounds, color)),
            CursorShape::Beam => frame.cursor_bars.push((Bounds::new(bounds.origin, size(bar, ctx.line_h)), color)),
            CursorShape::Underline => frame.cursor_bars.push((
                Bounds::new(point(bounds.origin.x, y + ctx.line_h - bar), size(width, bar)),
                color,
            )),
            CursorShape::Hidden => {}
        }
    }
}

/// The cells on screen, copied out of the terminal.
#[derive(Default)]
pub(super) struct Snapshot {
    /// A row at a time, from the left.
    pub(super) cells: Vec<SnapCell>,
    /// The zero-width characters of all cells (see `SnapCell::extra`).
    pub(super) extras: Vec<char>,
    /// The rows copied that are part of a command line, by row of the view.
    pub(super) input: Vec<(i32, InputRow)>,
}

/// How much of a row is a command the user typed, which highlight rules leave alone: the
/// line at the cursor and lines entered earlier (Command Marks), after their prompt.
#[derive(Clone, Copy, PartialEq, Debug)]
pub(super) enum InputRow {
    /// The first row of the line: what follows the prompt.
    AfterPrompt,
    /// A row the line wrapped onto: all of it.
    All,
}

impl Snapshot {
    /// Copy the cells of `rows` (rows of the view, top to bottom), colours resolved with
    /// `palette`, after what is copied already.
    pub(super) fn copy_rows<T: alacritty_terminal::event::EventListener>(
        &mut self,
        term: &Term<T>,
        content: &alacritty_terminal::term::RenderableContent,
        palette: &crate::theme::TerminalPalette,
        rows: impl IntoIterator<Item = usize>,
    ) {
        let grid = term.grid();
        let colors = content.colors;
        let offset = view_offset(term) as i32;
        let selection = content.selection;
        let typing = typing_start(term);
        for row in rows {
            let line = Line(row as i32 - offset);
            if let Some(input) = input_row(term, line, typing) {
                self.input.push((row as i32, input));
            }
            let cells = &grid[line];
            for col in 0..grid.columns() {
                let cell = &cells[Column(col)];
                if cell.flags.contains(Flags::WIDE_CHAR_SPACER) {
                    continue;
                }
                let mut fg = palette.resolve(cell.fg, colors);
                let mut bg = palette.resolve(cell.bg, colors);
                if cell.flags.contains(Flags::INVERSE) {
                    std::mem::swap(&mut fg, &mut bg);
                }
                let hidden = cell.flags.contains(Flags::HIDDEN);
                let extra_start = self.extras.len() as u32;
                if let Some(extra) = cell.zerowidth().filter(|_| !hidden) {
                    self.extras.extend_from_slice(extra);
                }
                self.cells.push(SnapCell {
                    row: row as i32,
                    col,
                    c: if hidden || cell.c == '\t' { ' ' } else { cell.c },
                    extra: extra_start..self.extras.len() as u32,
                    fg,
                    bg,
                    underline: cell.underline_color().map(|c| palette.resolve(c, colors)),
                    flags: cell.flags,
                    selected: selection.is_some_and(|s| s.contains(GridPoint::new(line, Column(col)))),
                });
            }
        }
    }

    pub(super) fn clear(&mut self) {
        self.cells.clear();
        self.extras.clear();
        self.input.clear();
    }

    pub(super) fn input(&self, row: i32) -> Option<InputRow> {
        self.input.iter().find(|(r, _)| *r == row).map(|&(_, input)| input)
    }
}

/// The first row of the line at the cursor, when it is being typed at a prompt.
pub(super) fn typing_start<T>(term: &Term<T>) -> Option<Line> {
    let grid = term.grid();
    let cursor = grid.cursor.point.line;
    (!term.mode().contains(TermMode::ALT_SCREEN) && cursor <= grid.bottommost_line())
        .then(|| crate::marks::logical_start(term, cursor))
        .filter(|&start| {
            let row = &grid[start];
            let text: String = (0..grid.columns()).map(|col| row[Column(col)].c).collect();
            crate::marks::prompt_end(&text).is_some()
        })
}

/// Whether row `line` is part of a command line (see `InputRow`): the one being typed, which
/// starts at `typing`, or one entered earlier.
pub(super) fn input_row<T>(term: &Term<T>, line: Line, typing: Option<Line>) -> Option<InputRow> {
    let start = crate::marks::logical_start(term, line);
    (Some(start) == typing || crate::marks::is_marked(&term.grid()[start][Column(0)]))
        .then_some(if start == line { InputRow::AfterPrompt } else { InputRow::All })
}

/// A link in the output: the address it opens and the cells it covers, row by row (a long
/// one wraps onto several).
#[derive(Clone, Debug, PartialEq)]
pub(super) struct TermLink {
    pub(super) url: String,
    pub(super) spans: Vec<(Line, Range<usize>)>,
}

/// Rows of a wrapped line read to find a link in it, either way from the pointer.
pub(super) const LINK_ROWS: i32 = 16;

/// The link at `at`: one a program marked (OSC 8) with a web address, or an address with its
/// scheme written in the text (see `crate::links`), read across rows a long line wrapped onto.
pub(super) fn link_at<T>(term: &Term<T>, at: GridPoint) -> Option<TermLink> {
    let grid = term.grid();
    let cols = grid.columns();
    if cols == 0 || at.line < grid.topmost_line() || at.line > grid.bottommost_line() || at.column.0 >= cols {
        return None;
    }
    let row = &grid[at.line];
    if let Some(link) = row[at.column].hyperlink() {
        if !crate::links::is_openable(link.uri()) {
            return None;
        }
        let same = |col: usize| row[Column(col)].hyperlink().as_ref() == Some(&link);
        let (mut start, mut end) = (at.column.0, at.column.0 + 1);
        while start > 0 && same(start - 1) {
            start -= 1;
        }
        while end < cols && same(end) {
            end += 1;
        }
        return Some(TermLink { url: link.uri().trim().to_string(), spans: vec![(at.line, start..end)] });
    }
    let wraps = |line: Line| grid[line][Column(cols - 1)].flags.contains(Flags::WRAPLINE);
    let mut first = at.line;
    while first > grid.topmost_line() && at.line.0 - first.0 < LINK_ROWS && wraps(Line(first.0 - 1)) {
        first = Line(first.0 - 1);
    }
    let mut last = at.line;
    while last < grid.bottommost_line() && last.0 - at.line.0 < LINK_ROWS && wraps(last) {
        last = Line(last.0 + 1);
    }
    // The line's text, and where each cell's character starts in it.
    let mut text = String::new();
    let mut cells: Vec<(usize, Line, usize, usize)> = vec![];
    let mut byte = None;
    for line in first.0..=last.0 {
        let line = Line(line);
        for col in 0..cols {
            let cell = &grid[line][Column(col)];
            if cell.flags.contains(Flags::WIDE_CHAR_SPACER) {
                continue;
            }
            if line == at.line && col <= at.column.0 {
                byte = Some(text.len());
            }
            let width = if cell.flags.contains(Flags::WIDE_CHAR) { 2 } else { 1 };
            cells.push((text.len(), line, col, width));
            let hidden = cell.flags.contains(Flags::HIDDEN);
            text.push(if hidden || cell.c == '\t' { ' ' } else { cell.c });
            if let Some(extra) = cell.zerowidth().filter(|_| !hidden) {
                text.extend(extra.iter());
            }
        }
    }
    let range = crate::links::link_at(&text, byte?)?;
    let mut spans: Vec<(Line, Range<usize>)> = vec![];
    for &(_, line, col, width) in cells.iter().filter(|(b, ..)| range.contains(b)) {
        match spans.last_mut() {
            Some((l, cols)) if *l == line => cols.end = col + width,
            _ => spans.push((line, col..col + width)),
        }
    }
    Some(TermLink { url: text[range].to_string(), spans })
}

/// The highlight rule colouring the cell at `at`, read the way rows are drawn (`layout_row`).
pub(super) fn highlighted_at<T>(term: &Term<T>, highlighter: &highlight::Highlighter, at: GridPoint) -> Option<highlight::RuleHit> {
    let grid = term.grid();
    if term.mode().contains(TermMode::ALT_SCREEN) || at.line < grid.topmost_line() || at.line > grid.bottommost_line() {
        return None;
    }
    let row = &grid[at.line];
    let mut text = String::new();
    let mut byte = None;
    for col in 0..grid.columns() {
        let cell = &row[Column(col)];
        if cell.flags.contains(Flags::WIDE_CHAR_SPACER) {
            continue;
        }
        if col <= at.column.0 {
            byte = Some(text.len());
        }
        let hidden = cell.flags.contains(Flags::HIDDEN);
        text.push(if hidden || cell.c == '\t' { ' ' } else { cell.c });
        if let Some(extra) = cell.zerowidth().filter(|_| !hidden) {
            text.extend(extra.iter());
        }
    }
    let byte = byte?;
    let end = match input_row(term, at.line, typing_start(term)) {
        Some(InputRow::All) => return None,
        Some(InputRow::AfterPrompt) => crate::marks::prompt_end(&text).unwrap_or(text.len()),
        None => text.len(),
    };
    (byte < end).then(|| highlighter.rule_at(&text[..end], byte)).flatten()
}

/// `text` cut to `max` characters, with an ellipsis when it was longer.
pub(super) fn ellipsize(text: &str, max: usize) -> String {
    let text = text.trim();
    match text.char_indices().nth(max) {
        Some((i, _)) => format!("{}…", &text[..i]),
        None => text.to_string(),
    }
}

/// How far the view is scrolled up into the Scrollback.
///
/// Not always the terminal's display offset: while the view is scrolled up, a program scrolling
/// a region that doesn't start at the top raises the offset without adding to the Scrollback
/// (alacritty_terminal 0.26), and the view would start above the oldest line.
pub(super) fn view_offset<T>(term: &Term<T>) -> usize {
    term.grid().display_offset().min(term.grid().history_size())
}

/// The cursor, if it is shown in the view.
pub(super) fn cursor_snap<T: alacritty_terminal::event::EventListener>(
    term: &Term<T>,
    content: &alacritty_terminal::term::RenderableContent,
    palette: &crate::theme::TerminalPalette,
) -> Option<CursorSnap> {
    let point = content.cursor.point;
    let row = point.line.0 + view_offset(term) as i32;
    (content.cursor.shape != CursorShape::Hidden && (0..term.screen_lines() as i32).contains(&row)).then(|| CursorSnap {
        row,
        col: point.column.0,
        shape: content.cursor.shape,
        wide: term.grid()[point].flags.contains(Flags::WIDE_CHAR),
        color: palette.resolve(Color::Named(NamedColor::Cursor), content.colors),
    })
}

/// The terminal font at one size: what the grid is measured and laid out with.
pub(super) struct GridFont {
    pub(super) family: SharedString,
    pub(super) size: Pixels,
    /// Regular, bold, italic, bold italic.
    pub(super) fonts: [gpui::Font; 4],
    pub(super) cell_w: Pixels,
    pub(super) line_h: Pixels,
    pub(super) metrics: RowTextMetrics,
}

impl GridFont {
    /// `weights`: of regular and of bold text.
    pub(super) fn new(text_system: &gpui::WindowTextSystem, family: SharedString, size: Pixels, weights: [FontWeight; 2]) -> Self {
        let mut regular = gpui::font(family);
        regular.weight = weights[0];
        regular.features = gpui::FontFeatures::disable_ligatures();
        let font_id = text_system.resolve_font(&regular);
        let cell_w = text_system.advance(font_id, size, 'm').map(|s| s.width).unwrap_or(px(8.));
        let line_h = (size * 1.32).round();
        let metrics = RowTextMetrics::new(line_h, text_system.ascent(font_id, size), text_system.descent(font_id, size));
        Self::with_metrics(regular, weights[1], size, cell_w, line_h, metrics)
    }

    pub(super) fn with_metrics(regular: gpui::Font, bold_weight: FontWeight, size: Pixels, cell_w: Pixels, line_h: Pixels, metrics: RowTextMetrics) -> Self {
        let bold = |mut f: gpui::Font| {
            f.weight = bold_weight;
            f
        };
        let italic = |mut f: gpui::Font| {
            f.style = FontStyle::Italic;
            f
        };
        let family = regular.family.clone();
        let fonts = [regular.clone(), bold(regular.clone()), italic(regular.clone()), bold(italic(regular))];
        Self { family, size, fonts, cell_w, line_h, metrics }
    }
}

/// How the grid is drawn this frame, apart from what is in the terminal.
pub(super) struct GridLook<'a> {
    pub(super) font: &'a GridFont,
    /// The top left corner of the grid's first cell.
    pub(super) origin: gpui::Point<Pixels>,
    pub(super) scale: f32,
    pub(super) palette: &'a crate::theme::TerminalPalette,
    pub(super) highlighter: &'a Arc<highlight::Highlighter>,
    pub(super) focused: bool,
    pub(super) cursor_visible: bool,
    /// The current search match on screen, (row, columns), and its text colour.
    pub(super) current_match: &'a [(i32, Range<usize>)],
    pub(super) match_foreground: Rgb,
}

/// What the grid keeps from one frame to the next: the rows laid out, and buffers reused.
///
/// A row is laid out again only when something it shows has changed: its cells (the terminal's
/// damage), the selection on it, the current search match on it or leaving it, or the cursor
/// on it or leaving it. Anything else that changes
/// how rows look (font, theme, highlight rules, size, position, scrolling) lays out every row.
///
/// The view paints its rows from here, so each view draws one grid per frame (as its `origin`
/// and `cell` already assume).
#[derive(Default)]
pub(super) struct GridCache {
    pub(super) glyphs: GlyphCache,
    pub(super) highlights: HighlightCache,
    pub(super) snapshot: Snapshot,
    pub(super) scratch: RowScratch,
    /// The rows of the view, laid out.
    pub(super) rows: Vec<RowPaint>,
    /// What `rows` were laid out with, besides the cells (`None` until the first frame).
    pub(super) looked: Option<LaidOutWith>,
    /// The selection and the row of the cursor that `rows` show.
    pub(super) selection: Option<SelectionRange>,
    pub(super) cursor_row: Option<usize>,
    /// The current search match that `rows` show.
    pub(super) current_match: Vec<(i32, Range<usize>)>,
    /// The rows to lay out this frame.
    pub(super) dirty: Vec<bool>,
}

/// What every row's layout depends on besides its cells, the selection and the cursor.
#[derive(PartialEq)]
pub(super) struct LaidOutWith {
    pub(super) family: SharedString,
    pub(super) font_size: Pixels,
    pub(super) cell_w: Pixels,
    pub(super) line_h: Pixels,
    pub(super) origin: gpui::Point<Pixels>,
    pub(super) scale: f32,
    pub(super) palette: crate::theme::TerminalPalette,
    pub(super) match_foreground: Rgb,
    /// The rules colouring output, `None` while highlighting is off; compared by identity
    /// (and held, so the address cannot come back with other rules).
    pub(super) highlighter: Option<ByAddress<highlight::Highlighter>>,
    pub(super) columns: usize,
    pub(super) lines: usize,
    /// How far the view is scrolled up (see `view_offset`).
    pub(super) offset: usize,
}

/// An `Arc` compared by what it points to, not by value.
pub(super) struct ByAddress<T>(Arc<T>);

impl<T> PartialEq for ByAddress<T> {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}

/// Lay out the grid for one frame into `cache.rows`: copy what changed on screen and let go of
/// the terminal, then lay out those rows (the parser keeps reading output meanwhile).
///
/// This is the only reader of the terminal's damage, and resets it: the rows of a `GridCache`
/// are only right if nothing else does.
pub(super) fn layout_grid(term: &SharedTerm, shaper: &dyn Shaper, look: &GridLook, cache: &mut GridCache) {
    let GridCache { glyphs, highlights, snapshot, scratch, rows, looked, selection, cursor_row, current_match, dirty } = cache;
    let font = look.font;
    snapshot.clear();
    let (cursor, highlight) = {
        let mut term = term.lock();
        let lines = term.screen_lines();
        dirty.clear();
        dirty.resize(lines, false);
        // Rows the terminal changed since the last frame (it covers the cursor's moves too).
        let mut all = match term.damage() {
            TermDamage::Full => true,
            TermDamage::Partial(damaged) => {
                for line in damaged {
                    if let Some(row) = dirty.get_mut(line.line) {
                        *row = true;
                    }
                }
                false
            }
        };
        term.reset_damage();
        let content = term.renderable_content();
        let offset = view_offset(&*term);
        // The damage is for the terminal's own offset (see `view_offset`).
        all |= offset != content.display_offset;
        // Highlight rules colour plain output; full-screen programs (vim, htop) only when asked.
        let full_screen_ok = look.highlighter.full_screen || !content.mode.contains(TermMode::ALT_SCREEN);
        let highlight = (full_screen_ok && look.highlighter.rule_count() > 0)
            .then(|| look.highlighter.clone());
        let now = LaidOutWith {
            family: font.family.clone(),
            font_size: font.size,
            cell_w: font.cell_w,
            line_h: font.line_h,
            origin: look.origin,
            scale: look.scale,
            palette: look.palette.clone(),
            match_foreground: look.match_foreground,
            highlighter: highlight.clone().map(ByAddress),
            columns: term.columns(),
            lines,
            offset,
        };
        if looked.as_ref() != Some(&now) {
            *looked = Some(now);
            all = true;
        }
        let cursor = cursor_snap(&term, &content, look.palette).filter(|_| look.cursor_visible);
        if all {
            dirty.fill(true);
        } else {
            // The selection isn't part of the damage: the rows it covered and covers.
            if content.selection != *selection {
                for range in [*selection, content.selection].into_iter().flatten() {
                    let first = (range.start.line.0 + offset as i32).max(0);
                    let last = (range.end.line.0 + offset as i32).min(lines as i32 - 1);
                    for row in first..=last {
                        dirty[row as usize] = true;
                    }
                }
            }
            // Nor is the current search match, whose text has its own colour: its rows before and now.
            if current_match.as_slice() != look.current_match {
                for &(row, _) in current_match.iter().chain(look.current_match) {
                    if let Some(row) = usize::try_from(row).ok().and_then(|row| dirty.get_mut(row)) {
                        *row = true;
                    }
                }
            }
            // Nor is the cursor's look (shape, colour, focus): the row it was drawn on, and its row.
            for row in [*cursor_row, cursor.map(|k| k.row as usize)].into_iter().flatten() {
                if let Some(row) = dirty.get_mut(row) {
                    *row = true;
                }
            }
        }
        *selection = content.selection;
        *cursor_row = cursor.map(|k| k.row as usize);
        if current_match.as_slice() != look.current_match {
            *current_match = look.current_match.to_vec();
        }
        rows.resize_with(lines, RowPaint::default);
        let changed = dirty.iter().enumerate().filter_map(|(row, dirty)| dirty.then_some(row));
        snapshot.copy_rows(&term, &content, look.palette, changed);
        (cursor, highlight)
    };
    let ctx = RowContext {
        shaper,
        fonts: font.fonts.clone(),
        font_size: font.size,
        origin: look.origin,
        cell_w: font.cell_w,
        line_h: font.line_h,
        scale: look.scale,
        baseline: font.metrics.baseline,
        underline_y: font.metrics.underline_y,
        strike_y: font.metrics.strike_y,
        highlighter: highlight.as_deref(),
        dark: look.palette.dark,
        default_bg: look.palette.background,
        selection: look.palette.selection,
        selection_foreground: look.palette.selection_foreground,
        current_match: look.current_match,
        match_foreground: look.match_foreground,
        cursor,
        focused: look.focused,
    };
    glyphs.prepare(&font.family, font.size, [font.fonts[0].weight, font.fonts[1].weight]);
    if let Some(highlighter) = &highlight {
        highlights.prepare(highlighter, look.palette.dark);
    }
    let cells = &snapshot.cells;
    let mut start = 0;
    while start < cells.len() {
        let row = cells[start].row;
        let end = cells[start..].iter().position(|c| c.row != row).map_or(cells.len(), |n| start + n);
        let paint = &mut rows[row as usize];
        paint.clear();
        let input = snapshot.input(row);
        layout_row(&cells[start..end], &snapshot.extras, row, input, &ctx, glyphs, highlights, paint, scratch);
        for d in &mut paint.decorations {
            d.y += if d.kind == DecorationKind::Strike { ctx.strike_y } else { ctx.underline_y };
        }
        start = end;
    }
}

/// One glyph of a shaped cell, relative to the cell's left edge and the baseline.
#[derive(Clone, Copy)]
pub(super) struct CachedGlyph {
    pub(super) font_id: gpui::FontId,
    pub(super) id: gpui::GlyphId,
    pub(super) x: Pixels,
    pub(super) y: Pixels,
    pub(super) emoji: bool,
}

/// Clusters of up to this many characters (a base character and the zero-width ones on its cell)
/// are looked up without allocating. Longer ones, such as family emoji joined with ZWJ, are rare.
pub(super) const INLINE_CLUSTER: usize = 8;

/// Entries a generation of the glyph cache grows to before it ages (see `Generations`). Two
/// generations are kept, so this many entries or more are always there: a screen in four styles
/// with thousands of different characters, CJK or combining marks fits many times over.
pub(super) const GLYPH_GENERATION: usize = 16_384;

/// A cluster and its style, inline.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub(super) struct ClusterKey {
    pub(super) style: u8,
    pub(super) len: u8,
    pub(super) chars: [char; INLINE_CLUSTER],
}

/// Cache entries in two generations: those used since the cache last aged, and those before.
/// Aging drops the older generation and starts a new one, so whatever was used in the last
/// frame is still there in the next, however many it was: a screen with more different
/// characters than a generation holds is never shaped again frame after frame.
pub(super) struct Generations<K, V = Box<[CachedGlyph]>> {
    pub(super) current: FxHashMap<K, V>,
    pub(super) previous: FxHashMap<K, V>,
}

impl<K, V> Default for Generations<K, V> {
    fn default() -> Self {
        Self { current: FxHashMap::default(), previous: FxHashMap::default() }
    }
}

impl<K: Hash + Eq, V> Generations<K, V> {
    /// Start a new generation once the current one has more than `limit` entries.
    pub(super) fn age(&mut self, limit: usize) {
        if self.current.len() > limit {
            // The old generation's table is cleared and reused for the new one.
            std::mem::swap(&mut self.current, &mut self.previous);
            self.current.clear();
        }
    }

    pub(super) fn clear(&mut self) {
        self.current.clear();
        self.previous.clear();
    }

    #[cfg(test)]
    pub(super) fn len(&self) -> usize {
        self.current.len() + self.previous.len()
    }
}

/// The glyphs for each character (or base character with its combining marks) in each style,
/// shaped once and reused. Text is placed cell by cell, so shaping whole rows every frame
/// would be thrown away; during heavy output, when every row is new each frame, that
/// shaping was most of the time spent drawing.
///
/// Whatever shaping gives is kept, including a character no font has (drawn as the missing
/// glyph): it is not looked for in the fonts again.
pub(super) struct GlyphCache {
    /// The font family, size, and regular and bold weights the entries were shaped with.
    pub(super) font: Option<(SharedString, Pixels, [FontWeight; 2])>,
    /// ASCII characters in each style, by style and code: most cells of most screens.
    pub(super) ascii: Box<[Option<Box<[CachedGlyph]>>]>,
    pub(super) clusters: Generations<ClusterKey>,
    /// Clusters longer than `INLINE_CLUSTER`, the style first (as a character).
    pub(super) long: Generations<Box<[char]>>,
    /// Where a long cluster's key is put together for a lookup.
    pub(super) long_key: Vec<char>,
    /// Entries a generation grows to before it ages.
    pub(super) limit: usize,
}

impl Default for GlyphCache {
    fn default() -> Self {
        Self {
            font: None,
            ascii: std::iter::repeat_with(|| None).take(4 * 128).collect(),
            clusters: Generations::default(),
            long: Generations::default(),
            long_key: Vec::new(),
            limit: GLYPH_GENERATION,
        }
    }
}

impl GlyphCache {
    /// Start a frame: forget everything if the font changed, and keep the cache bounded.
    pub(super) fn prepare(&mut self, family: &SharedString, size: Pixels, weights: [FontWeight; 2]) {
        if self.font.as_ref().is_none_or(|(f, s, w)| f != family || *s != size || *w != weights) {
            self.font = Some((family.clone(), size, weights));
            self.ascii.iter_mut().for_each(|slot| *slot = None);
            self.clusters.clear();
            self.long.clear();
        }
        self.clusters.age(self.limit);
        self.long.age(self.limit / 16);
    }

    /// The glyphs of a cell's text (`c` and the zero-width characters on its cell) in a style,
    /// shaped if they aren't cached yet.
    pub(super) fn get(&mut self, ctx: &RowContext, style: usize, c: char, extra: &[char]) -> &[CachedGlyph] {
        self.get_with(style, c, extra, |text| shape_cluster(ctx, style, text))
    }

    pub(super) fn get_with(
        &mut self,
        style: usize,
        c: char,
        extra: &[char],
        shape: impl FnOnce(&str) -> Box<[CachedGlyph]>,
    ) -> &[CachedGlyph] {
        let text = || std::iter::once(c).chain(extra.iter().copied()).collect::<String>();
        if extra.is_empty() && c.is_ascii() {
            return self.ascii[style * 128 + c as usize].get_or_insert_with(|| shape(c.encode_utf8(&mut [0; 4])));
        }
        let len = 1 + extra.len();
        if len <= INLINE_CLUSTER {
            let mut chars = ['\0'; INLINE_CLUSTER];
            chars[0] = c;
            chars[1..len].copy_from_slice(extra);
            let key = ClusterKey { style: style as u8, len: len as u8, chars };
            let Generations { current, previous } = &mut self.clusters;
            return match current.entry(key) {
                Entry::Occupied(entry) => entry.into_mut(),
                Entry::Vacant(entry) => entry.insert(previous.remove(&key).unwrap_or_else(|| shape(&text()))),
            };
        }
        self.long_key.clear();
        self.long_key.push(char::from(style as u8));
        self.long_key.push(c);
        self.long_key.extend_from_slice(extra);
        let Generations { current, previous } = &mut self.long;
        if !current.contains_key(self.long_key.as_slice()) {
            let glyphs = previous.remove(self.long_key.as_slice()).unwrap_or_else(|| shape(&text()));
            current.insert(self.long_key.as_slice().into(), glyphs);
        }
        &current[self.long_key.as_slice()]
    }

    #[cfg(test)]
    pub(super) fn len(&self) -> usize {
        self.ascii.iter().flatten().count() + self.clusters.len() + self.long.len()
    }
}

/// Shapes the text of a cell: GPUI's text system, or a stand-in in tests.
pub(super) trait Shaper {
    /// The glyphs of `text`, placed relative to the first one.
    fn shape(&self, font: &gpui::Font, size: Pixels, text: &str) -> Box<[CachedGlyph]>;
}

impl Shaper for gpui::WindowTextSystem {
    fn shape(&self, font: &gpui::Font, size: Pixels, text: &str) -> Box<[CachedGlyph]> {
        let run = TextRun {
            len: text.len(),
            font: font.clone(),
            color: Hsla::black(),
            background_color: None,
            underline: None,
            strikethrough: None,
        };
        let layout = self.layout_line(text, size, &[run], None);
        let base = layout.runs.iter().flat_map(|r| r.glyphs.first()).next().map_or(px(0.), |g| g.position.x);
        layout
            .runs
            .iter()
            .flat_map(|r| {
                r.glyphs.iter().map(move |g| CachedGlyph {
                    font_id: r.font_id,
                    id: g.id,
                    x: g.position.x - base,
                    y: g.position.y,
                    emoji: g.is_emoji,
                })
            })
            .collect()
    }
}

/// Shape one cell's text in a style.
pub(super) fn shape_cluster(ctx: &RowContext, style: usize, text: &str) -> Box<[CachedGlyph]> {
    #[cfg(test)]
    SHAPED.with(|n| n.set(n.get() + 1));
    ctx.shaper.shape(&ctx.fonts[style], ctx.font_size, text)
}

#[cfg(test)]
thread_local! {
    /// Clusters shaped on this thread, for tests and benchmarks of the glyph cache.
    static SHAPED: Cell<usize> = const { Cell::new(0) };
}

#[cfg(test)]
pub(super) fn shaped_count() -> usize {
    SHAPED.with(Cell::get)
}

pub(super) fn paint_prim(window: &mut Window, prim: &Prim, color: Hsla) {
    let p = |(x, y): (f32, f32)| point(px(x), px(y));
    let path = match *prim {
        Prim::Rect { x, y, w, h, alpha } => {
            window.paint_quad(fill(Bounds::new(p((x, y)), size(px(w), px(h))), Hsla { a: color.a * alpha, ..color }));
            return;
        }
        Prim::Triangle([a, b, c]) => {
            let mut path = PathBuilder::fill();
            path.move_to(p(a));
            path.line_to(p(b));
            path.line_to(p(c));
            path.close();
            path
        }
        Prim::Chevron { points: [a, b, c], width } => {
            let mut path = PathBuilder::stroke(px(width));
            path.move_to(p(a));
            path.line_to(p(b));
            path.line_to(p(c));
            path
        }
        Prim::Arc { from, start, corner, end, to, width } => {
            // A cubic Bézier with control points 55% of the way to the corner is a close
            // match for a quarter circle.
            const K: f32 = 0.5523;
            let toward = |a: (f32, f32)| (a.0 + (corner.0 - a.0) * K, a.1 + (corner.1 - a.1) * K);
            let mut path = PathBuilder::stroke(px(width));
            path.move_to(p(from));
            path.line_to(p(start));
            path.cubic_bezier_to(p(end), p(toward(start)), p(toward(end)));
            path.line_to(p(to));
            path
        }
    };
    if let Ok(path) = path.build() {
        window.paint_path(path, color);
    }
}
