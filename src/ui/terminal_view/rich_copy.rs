//! Copying a selection with its colors: the plain text plus an HTML version that documents,
//! mail and chat programs paste with the terminal's colors, styles and font.

use alacritty_terminal::grid::Dimensions as _;
use alacritty_terminal::index::{Column, Line};
use alacritty_terminal::term::Term;
use alacritty_terminal::term::cell::Flags;
use alacritty_terminal::vte::ansi::Rgb;
use gpui::{App, ClipboardItem};

use crate::theme::TerminalPalette;

/// How a run of characters looks. `background` is `None` on the terminal's own background.
#[derive(Clone, Copy, PartialEq, Debug)]
struct Style {
    foreground: Rgb,
    background: Option<Rgb>,
    bold: bool,
    italic: bool,
    underline: bool,
    strike: bool,
}

type Row = Vec<(char, Style)>;

/// The selection as an HTML fragment, or `None` without a selection. Each row loses its
/// trailing blanks, as the plain text does; with `trim`, blank rows and leading spaces at
/// the start and blank rows at the end go too.
pub(super) fn selection_html<T: alacritty_terminal::event::EventListener>(
    term: &Term<T>,
    palette: &TerminalPalette,
    font_family: &str,
    font_size: f32,
    trim: bool,
) -> Option<String> {
    let range = term.selection.as_ref()?.to_range(term)?;
    let grid = term.grid();
    let colors = term.colors();
    let last_column = grid.columns() - 1;
    let mut rows: Vec<Row> = vec![Vec::new()];
    for line in range.start.line.0..=range.end.line.0 {
        let first = line == range.start.line.0;
        let last = line == range.end.line.0;
        let from = if first || range.is_block { range.start.column.0 } else { 0 };
        let to = if last || range.is_block { range.end.column.0 } else { last_column };
        let cells = &grid[Line(line)];
        for col in from..=to.min(last_column) {
            let cell = &cells[Column(col)];
            if cell.flags.intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER) {
                continue;
            }
            let (mut fg, mut bg) = (palette.resolve(cell.fg, colors), palette.resolve(cell.bg, colors));
            if cell.flags.contains(Flags::INVERSE) {
                std::mem::swap(&mut fg, &mut bg);
            }
            let style = Style {
                foreground: fg,
                background: (bg != palette.background).then_some(bg),
                bold: cell.flags.contains(Flags::BOLD),
                italic: cell.flags.contains(Flags::ITALIC),
                underline: cell.flags.intersects(Flags::ALL_UNDERLINES),
                strike: cell.flags.contains(Flags::STRIKEOUT),
            };
            let hidden = cell.flags.contains(Flags::HIDDEN);
            let row = rows.last_mut().expect("a row");
            row.push((if hidden || cell.c == '\t' { ' ' } else { cell.c }, style));
            if let Some(extra) = cell.zerowidth().filter(|_| !hidden) {
                row.extend(extra.iter().map(|&c| (c, style)));
            }
        }
        // A line that wraps onto the next continues it; otherwise a new row starts.
        let wraps = !range.is_block && to == last_column && cells[Column(last_column)].flags.contains(Flags::WRAPLINE);
        if !last && !wraps {
            rows.push(Vec::new());
        }
    }
    Some(fragment(rows, palette, font_family, font_size, trim))
}

fn fragment(mut rows: Vec<Row>, palette: &TerminalPalette, font_family: &str, font_size: f32, trim: bool) -> String {
    let blank = |&(c, style): &(char, Style)| c == ' ' && style.background.is_none();
    for row in &mut rows {
        while row.last().is_some_and(blank) {
            row.pop();
        }
    }
    if trim {
        while rows.first().is_some_and(Vec::is_empty) {
            rows.remove(0);
        }
        while rows.last().is_some_and(Vec::is_empty) {
            rows.pop();
        }
        if let Some(row) = rows.first_mut() {
            let lead = row.iter().take_while(|cell| blank(cell)).count();
            row.drain(..lead);
        }
    }
    let mut html = format!(
        "<pre style=\"font-family:'{}',Consolas,monospace;font-size:{}pt;color:{};background-color:{};padding:6px 8px;margin:0\">",
        escape(font_family),
        (font_size * 0.75 * 10.).round() / 10.,
        css(palette.foreground),
        css(palette.background),
    );
    for (i, row) in rows.iter().enumerate() {
        if i > 0 {
            html.push('\n');
        }
        let mut cells = row.iter().peekable();
        while let Some(&(c, style)) = cells.next() {
            let mut text = String::from(c);
            while let Some(&&(c, _)) = cells.peek().filter(|(_, next)| *next == style) {
                text.push(c);
                cells.next();
            }
            push_run(&mut html, &text, style, palette);
        }
    }
    html.push_str("</pre>");
    html
}

fn push_run(html: &mut String, text: &str, style: Style, palette: &TerminalPalette) {
    let mut css_text = String::new();
    if style.foreground != palette.foreground {
        css_text.push_str(&format!("color:{};", css(style.foreground)));
    }
    if let Some(bg) = style.background {
        css_text.push_str(&format!("background-color:{};", css(bg)));
    }
    if style.bold {
        css_text.push_str("font-weight:bold;");
    }
    if style.italic {
        css_text.push_str("font-style:italic;");
    }
    match (style.underline, style.strike) {
        (true, true) => css_text.push_str("text-decoration:underline line-through;"),
        (true, false) => css_text.push_str("text-decoration:underline;"),
        (false, true) => css_text.push_str("text-decoration:line-through;"),
        (false, false) => {}
    }
    if css_text.is_empty() {
        html.push_str(&escape(text));
    } else {
        html.push_str(&format!("<span style=\"{}\">{}</span>", css_text.trim_end_matches(';'), escape(text)));
    }
}

fn css(c: Rgb) -> String {
    format!("#{:02x}{:02x}{:02x}", c.r, c.g, c.b)
}

fn escape(text: &str) -> String {
    text.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

/// Put `text` on the clipboard, with `html` beside it where the platform can hold both.
pub(super) fn write_clipboard(text: String, html: Option<String>, cx: &mut App) {
    #[cfg(windows)]
    if let Some(html) = &html {
        match write_with_html(&text, html) {
            Ok(()) => return,
            Err(error) => log::warn!("Copying with colors failed, copying the text alone: {error}"),
        }
    }
    let _ = html;
    cx.write_to_clipboard(ClipboardItem::new_string(text));
}

#[cfg(windows)]
fn write_with_html(text: &str, html: &str) -> anyhow::Result<()> {
    use clipboard_win::{formats::Html, options::NoClear, raw};
    let error = |e| anyhow::anyhow!("clipboard: {e}");
    let _clipboard = clipboard_win::Clipboard::new_attempts(10).map_err(error)?;
    raw::empty().map_err(error)?;
    raw::set_string_with(text, NoClear).map_err(error)?;
    let format = Html::new().ok_or_else(|| anyhow::anyhow!("clipboard: no HTML format"))?;
    raw::set_html(format.code(), html).map_err(error)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn style(palette: &TerminalPalette) -> Style {
        Style { foreground: palette.foreground, background: None, bold: false, italic: false, underline: false, strike: false }
    }

    fn row(text: &str, style: Style) -> Row {
        text.chars().map(|c| (c, style)).collect()
    }

    #[test]
    fn runs_keep_colors_and_styles_and_escape_markup() {
        let palette = TerminalPalette::dark();
        let plain = style(&palette);
        let red = Style { foreground: palette.ansi[1], bold: true, ..plain };
        let mut first = row("ok ", plain);
        first.extend(row("<err>", red));
        let html = fragment(vec![first, row("a & b", plain)], &palette, "JetBrains Mono", 14., false);
        assert!(html.starts_with("<pre style=\"font-family:'JetBrains Mono',Consolas,monospace;font-size:10.5pt;"), "{html}");
        assert!(html.contains(&format!("ok <span style=\"color:{};font-weight:bold\">&lt;err&gt;</span>\na &amp; b</pre>", css(palette.ansi[1]))), "{html}");
    }

    #[test]
    fn a_selected_wrapped_line_keeps_its_colors_and_joins_without_a_break() {
        use super::super::*;
        use alacritty_terminal::vte::ansi::Processor;
        let (tx, _rx) = mpsc::unbounded();
        let term = new_term(GridSize { cols: 10, rows: 4, cell_width: 8, cell_height: 16 }, 100, EventProxy::new(tx));
        let mut term = term.lock();
        Processor::<alacritty_terminal::vte::ansi::StdSyncHandler>::new()
            .advance(&mut *term, b"\x1b[1;31merror\x1b[0m: disk full\r\nnext");
        let mut selection = Selection::new(SelectionType::Lines, GridPoint::new(Line(0), Column(0)), Side::Left);
        selection.update(GridPoint::new(Line(1), Column(0)), Side::Right);
        term.selection = Some(selection);
        let palette = TerminalPalette::dark();
        let html = selection_html(&term, &palette, "Mono", 14., true).unwrap();
        let red = css(palette.ansi[1]);
        assert!(html.contains(&format!("<span style=\"color:{red};font-weight:bold\">error</span>: disk full</pre>")), "{html}");
    }

    #[test]
    fn trimming_drops_blank_rows_leading_spaces_and_trailing_blanks() {
        let palette = TerminalPalette::dark();
        let plain = style(&palette);
        let rows = vec![row("", plain), row("   $ ls   ", plain), row("file   ", plain), row("", plain)];
        assert!(fragment(rows.clone(), &palette, "Mono", 14., true).ends_with(">$ ls\nfile</pre>"));
        assert!(fragment(rows, &palette, "Mono", 14., false).ends_with(">\n   $ ls\nfile\n</pre>"));
    }
}
