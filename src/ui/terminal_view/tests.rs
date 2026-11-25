use super::{
    Activity, CachedGlyph, GlyphCache, GridCache, GridFont, GridLook, INLINE_CLUSTER, RowTextMetrics, Shaper,
    layout_grid,
};
use crate::theme::rgb_to_hsla;
use alacritty_terminal::vte::ansi::Rgb;
use std::ops::Range;

#[test]
fn the_clipboard_question_says_how_much_and_shows_the_start() {
    use super::clipboard_preview;
    assert_eq!(clipboard_preview("rm -rf ~"), ("8 characters".to_string(), "rm -rf ~".to_string()));
    let (summary, preview) = clipboard_preview("curl x | sh\nexit\n");
    assert_eq!(summary, "2 lines");
    assert_eq!(preview, "curl x | sh⏎exit⏎");
    // Escape sequences can't restyle the question.
    assert_eq!(clipboard_preview("a\u{1b}[31mb").1, "a [31mb");
    assert_eq!(clipboard_preview(&"x".repeat(5000)).1.chars().count(), 200);
}

#[test]
fn pasting_as_one_line_turns_every_line_break_into_a_space() {
    use super::one_line;
    assert_eq!(one_line("cd /srv\r\nls -la\nexit\r\n"), "cd /srv ls -la exit");
    assert_eq!(one_line("a\rb"), "a b");
    assert_eq!(one_line("single"), "single");
}

#[test]
fn a_paste_counts_every_line_break_the_shell_will_see() {
    use super::pasted_lines;
    assert_eq!(pasted_lines("ls\n"), 1);
    assert_eq!(pasted_lines("ls\r\npwd\r\n"), 2);
    // A bare CR is Enter to the shell, so it must trigger the multi-line question.
    assert_eq!(pasted_lines("ls\rrm -rf build"), 2);
    assert_eq!(pasted_lines(""), 1);
}
use crate::highlight::Highlighter;
use crate::session::ssh::Output;
use crate::session::{EventProxy, GridSize, SharedTerm, new_term};
use crate::theme::TerminalPalette;
use alacritty_terminal::grid::{Dimensions as _, Scroll};
use alacritty_terminal::index::{Column, Line, Point as GridPoint, Side};
use alacritty_terminal::selection::{Selection, SelectionType};
use gpui::{Pixels, px};
use std::cell::Cell;
use std::sync::Arc;
use std::time::{Duration, Instant};

#[test]
fn width_preparation_coalesces_frames_and_rejects_obsolete_completions() {
    use super::{Ordering, WidthPreparation};
    let mut preparation = WidthPreparation::default();
    let (_, first) = preparation.request(120).unwrap();
    assert!(preparation.request(120).is_none());
    assert!(!preparation.ready(120));
    let (_, second) = preparation.request(150).unwrap();
    assert!(first.load(Ordering::Relaxed));
    preparation.finish(&first, true);
    assert!(!preparation.ready(150));
    preparation.finish(&second, true);
    assert!(preparation.ready(150));
    // Collapse, expand, then collapse to the same width before an old worker finishes.
    preparation.pending = None;
    assert!(second.load(Ordering::Relaxed));
    let (_, third) = preparation.request(150).unwrap();
    preparation.finish(&second, true);
    assert!(!preparation.ready(150));
    preparation.finish(&third, false);
    assert!(preparation.request(150).is_some(), "a failed preparation must be retryable");
}

#[test]
fn divider_movements_share_preparation_for_nearby_widths() {
    use super::{Ordering, WidthPreparation};
    let mut preparation = WidthPreparation::default();
    let (target, work) = preparation.request(121).unwrap();
    assert_eq!(target, 128);
    for cols in 122..=128 {
        assert!(preparation.request(cols).is_none(), "column {cols} restarted preparation");
    }
    assert!(!work.load(Ordering::Relaxed));
    preparation.finish(&work, true);
    for cols in 121..=128 {
        assert!(preparation.ready(cols));
    }
    assert!(preparation.request(129).is_some());
    assert!(work.load(Ordering::Relaxed));
    let (target, work) = preparation.request(u16::MAX).unwrap();
    assert_eq!(target, u16::MAX);
    preparation.finish(&work, true);
    assert!(preparation.ready(u16::MAX));
}

#[test]
fn slow_reflow_does_not_leave_a_long_redraw_cooldown() {
    use super::Reflow;
    let start = Instant::now();
    let slow = Reflow { at: Some(start), took: Duration::from_millis(250), woken: false };
    assert!(slow.wait(start + Duration::from_millis(250)).is_none());
    let normal = Reflow { at: Some(start), took: Duration::from_millis(20), woken: false };
    assert!(normal.wait(start + Duration::from_millis(40)).is_none());
    let tiny = Reflow { at: Some(start), took: Duration::from_millis(1), woken: false };
    assert!(tiny.wait(start + Duration::from_millis(16)).is_none());
}

/// Output every `every` ms from `from` to `to` (ms after `t0`).
fn stream(a: &mut Activity, t0: Instant, from: u64, to: u64, every: u64) {
    for ms in (from..=to).step_by(every as usize) {
        a.output(t0 + Duration::from_millis(ms));
    }
}

#[test]
fn steady_output_is_busy_after_a_second_and_stops_after_a_pause() {
    let t0 = Instant::now();
    let at = |ms| t0 + Duration::from_millis(ms);
    let mut a = Activity::default();
    a.input(t0); // Enter on `tree /`
    stream(&mut a, t0, 20, 1400, 20);
    // Output in the first half second is the command's echo; the stretch starts after it.
    assert!(!a.busy(at(1400)));
    stream(&mut a, t0, 1420, 3000, 20);
    assert!(a.busy(at(1600)));
    assert!(a.busy(at(3000)));
    // The output ends: still busy across a short pause, not after a long one.
    assert!(a.busy(at(6500)));
    assert!(!a.busy(at(7100)));
    assert!(a.settled(at(7100)));
}

#[test]
fn a_quick_command_and_typing_are_not_activity() {
    let t0 = Instant::now();
    let at = |ms| t0 + Duration::from_millis(ms);
    let mut a = Activity::default();
    // `ls`: Enter, then a burst of output within a fraction of a second.
    a.input(t0);
    stream(&mut a, t0, 600, 700, 10);
    assert!(!a.busy(at(800)) && !a.busy(at(1500)) && !a.busy(at(3000)));
    // Typing: each key's echo comes right after it.
    let mut a = Activity::default();
    for ms in (0..5000).step_by(150) {
        a.input(at(ms));
        a.output(at(ms + 40));
    }
    assert!(!a.busy(at(5000)));
    assert!(a.settled(at(5000)));
}

#[test]
fn a_followed_log_stays_busy_while_lines_keep_coming() {
    let t0 = Instant::now();
    // Two quick commands, each output arriving a little after its Enter (a slow server).
    let mut a = Activity::default();
    a.input(t0);
    stream(&mut a, t0, 600, 700, 50);
    a.input(t0 + Duration::from_millis(2000));
    stream(&mut a, t0, 2600, 2700, 50);
    assert!(!a.busy(t0 + Duration::from_millis(2800)));
    // A busy session stays busy while the user types.
    let mut a = Activity::default();
    stream(&mut a, t0, 0, 3000, 100);
    a.input(t0 + Duration::from_millis(3000));
    assert!(a.busy(t0 + Duration::from_millis(3100)));

    let mut a = Activity::default();
    // `top`: a redraw every 3 s.
    stream(&mut a, t0, 0, 9000, 3000);
    assert!(a.busy(t0 + Duration::from_millis(9500)));
    assert!(a.busy(t0 + Duration::from_millis(11500)));
}

#[test]
fn terminal_text_stays_centered_in_cursor_row_with_signed_descent() {
    // Windows DirectWrite reports a negative descent. Other backends may
    // report a positive distance. Both must place text in the same row.
    for line_h in [18., 24., 36.] {
        let ascent = px(line_h * 0.7);
        let depth = px(line_h * 0.2);
        for descent in [depth, -depth] {
            let metrics = RowTextMetrics::new(px(line_h), ascent, descent);
            let top_gap = metrics.baseline - ascent;
            let bottom_gap = px(line_h) - metrics.baseline - depth;
            assert!(
                f32::from(top_gap - bottom_gap).abs() < 0.001,
                "text must be centered within the cursor row: top={top_gap:?}, bottom={bottom_gap:?}, descent={descent:?}"
            );
            assert!(metrics.underline_y > metrics.baseline);
            assert!(metrics.underline_y < px(line_h));
            assert!(metrics.strike_y > top_gap);
            assert!(metrics.strike_y < metrics.baseline);
        }
    }
}

/// A stand-in for shaping: one glyph per character, numbered by the character, counted.
fn fake_shape(count: &Cell<usize>) -> impl Fn(&str) -> Box<[CachedGlyph]> + '_ {
    move |text| {
        count.set(count.get() + 1);
        text.chars()
            .map(|c| CachedGlyph { font_id: gpui::FontId(0), id: gpui::GlyphId(c as u32), x: px(0.), y: px(0.), emoji: false })
            .collect()
    }
}

fn ids(glyphs: &[CachedGlyph]) -> Vec<u32> {
    glyphs.iter().map(|g| g.id.0).collect()
}

#[test]
fn glyph_cache_shapes_each_cell_text_once_per_style() {
    let shaped = Cell::new(0);
    let shape = fake_shape(&shaped);
    let mut cache = GlyphCache::default();
    let family = ['\u{1f468}', '\u{200d}', '\u{1f469}', '\u{200d}', '\u{1f467}', '\u{200d}', '\u{1f466}'];
    let long: Vec<char> = std::iter::repeat_n('\u{0301}', INLINE_CLUSTER + 3).collect();
    let cells: [(char, &[char]); 6] = [
        ('a', &[]),
        ('\u{65e5}', &[]),
        ('e', &['\u{0301}']),
        ('\u{2764}', &['\u{fe0f}']),
        ('\u{1f468}', &family[1..]),
        ('x', &long),
    ];
    for _ in 0..3 {
        cache.prepare(&"Mono".into(), px(14.), [gpui::FontWeight::NORMAL, gpui::FontWeight::BOLD]);
        for style in 0..4 {
            for (c, extra) in cells {
                let glyphs = cache.get_with(style, c, extra, &shape);
                let want: Vec<u32> = std::iter::once(c).chain(extra.iter().copied()).map(|c| c as u32).collect();
                assert_eq!(ids(glyphs), want);
            }
        }
    }
    assert_eq!(shaped.get(), 6 * 4, "every text in every style is shaped once, then found");
    assert_eq!(cache.len(), 6 * 4);
    // A base character alone and with a mark are different entries.
    assert_eq!(ids(cache.get_with(0, 'e', &[], &shape)), vec!['e' as u32]);
    assert_eq!(shaped.get(), 6 * 4 + 1);

    // Another font or size starts over.
    cache.prepare(&"Mono".into(), px(15.), [gpui::FontWeight::NORMAL, gpui::FontWeight::BOLD]);
    assert_eq!(cache.len(), 0);
    cache.get_with(0, 'a', &[], &shape);
    cache.get_with(0, 'x', &long, &shape);
    assert_eq!(shaped.get(), 6 * 4 + 3);
}

#[test]
fn glyph_cache_keeps_a_frame_bigger_than_its_limit_and_stays_bounded() {
    let shaped = Cell::new(0);
    let shape = fake_shape(&shaped);
    let mut cache = GlyphCache { limit: 100, ..GlyphCache::default() };
    let marks: Vec<char> = ('\u{0300}'..='\u{036f}').collect();
    let marks = &marks;
    let screen = |seed: usize| (seed..seed + 1000).map(move |i| (char::from(b'a' + (i % 26) as u8), marks[i / 26 % marks.len()]));

    // The same 1,000 clusters on screen frame after frame, ten times the limit: shaped once.
    for _ in 0..5 {
        cache.prepare(&"Mono".into(), px(14.), [gpui::FontWeight::NORMAL, gpui::FontWeight::BOLD]);
        for (c, mark) in screen(0) {
            cache.get_with(0, c, &[mark], &shape);
        }
    }
    assert_eq!(shaped.get(), 1000);

    // New clusters every frame: those no longer on screen go two generations later.
    for frame in 1..=20 {
        cache.prepare(&"Mono".into(), px(14.), [gpui::FontWeight::NORMAL, gpui::FontWeight::BOLD]);
        for (c, mark) in screen(frame * 1000 % 2912) {
            cache.get_with(1, c, &[mark], &shape);
        }
        assert!(cache.len() <= 2 * 1000 + 100 + 1, "{} entries", cache.len());
    }
    // Long clusters age the same way.
    let long: Vec<char> = std::iter::repeat_n('\u{0301}', INLINE_CLUSTER).collect();
    for frame in 0..20u32 {
        cache.prepare(&"Mono".into(), px(14.), [gpui::FontWeight::NORMAL, gpui::FontWeight::BOLD]);
        for i in 0..50u32 {
            cache.get_with(2, char::from_u32(0x4e00 + frame * 50 + i).unwrap(), &long, &shape);
        }
        assert!(cache.long.len() <= 2 * 50 + 100 / 16 + 1, "{} entries", cache.long.len());
    }
}

/// Shapes every character as a glyph numbered by it, in a font numbered by the style.
struct FakeShaper;

impl Shaper for FakeShaper {
    fn shape(&self, font: &gpui::Font, _: Pixels, text: &str) -> Box<[CachedGlyph]> {
        let style = (font.weight == gpui::FontWeight::BOLD) as usize + 2 * (font.style == gpui::FontStyle::Italic) as usize;
        text.chars()
            .enumerate()
            .map(|(i, c)| CachedGlyph {
                font_id: gpui::FontId(style),
                id: gpui::GlyphId(c as u32),
                x: px(i as f32),
                y: px(0.),
                emoji: false,
            })
            .collect()
    }
}

fn grid_font(size: f32) -> GridFont {
    let metrics = RowTextMetrics::new(px(size * 1.32).round(), px(size * 0.9), px(-size * 0.25));
    GridFont::with_metrics(gpui::font("Mono"), gpui::FontWeight::BOLD, px(size), px(size * 0.6), px(size * 1.32).round(), metrics)
}

/// A terminal and its grid, laid out frame after frame. Every frame is checked against
/// laying out every row afresh.
struct Grid {
    term: SharedTerm,
    proxy: EventProxy,
    output: Output,
    font: GridFont,
    origin: gpui::Point<Pixels>,
    scale: f32,
    palette: TerminalPalette,
    highlighter: Arc<Highlighter>,
    focused: bool,
    cursor_visible: bool,
    current_match: Vec<(i32, Range<usize>)>,
    cache: GridCache,
}

impl Grid {
    fn new(cols: u16, rows: u16) -> Self {
        let (tx, _rx) = futures::channel::mpsc::unbounded();
        let proxy = EventProxy::new(tx);
        let term = new_term(GridSize { cols, rows, cell_width: 8, cell_height: 18 }, 1000, proxy.clone());
        let sets = crate::highlight::load(std::path::Path::new("/nonexistent/komando-grid-test")).0;
        Self {
            term,
            proxy,
            output: Output::default(),
            font: grid_font(14.),
            origin: gpui::point(px(12.), px(8.)),
            scale: 1.5,
            palette: TerminalPalette::dark(),
            highlighter: Arc::new(Highlighter::from_sets(&sets, &[])),
            focused: true,
            cursor_visible: true,
            current_match: Vec::new(),
            cache: GridCache::default(),
        }
    }

    fn feed(&mut self, bytes: impl AsRef<[u8]>) {
        self.output.feed(&self.term, &self.proxy, bytes.as_ref());
    }

    /// Lay out a frame and check it against a fresh layout. Returns how many rows it laid out.
    fn frame(&mut self, what: &str) -> usize {
        let Grid { term, font, origin, scale, palette, highlighter, focused, cursor_visible, current_match, cache, .. } = self;
        let look = GridLook {
            font,
            origin: *origin,
            scale: *scale,
            palette,
            highlighter,
            focused: *focused,
            cursor_visible: *cursor_visible,
            current_match,
            match_foreground: MATCH_TEXT,
        };
        layout_grid(term, &FakeShaper, &look, cache);
        let mut laid_out: Vec<i32> = cache.snapshot.cells.iter().map(|c| c.row).collect();
        laid_out.dedup();
        let mut fresh = GridCache::default();
        layout_grid(term, &FakeShaper, &look, &mut fresh);
        assert_eq!(cache.rows.len(), fresh.rows.len(), "{what}: rows");
        assert_eq!(fresh.rows.len(), term.lock().screen_lines(), "{what}: rows");
        for (row, (kept, new)) in cache.rows.iter().zip(&fresh.rows).enumerate() {
            assert!(kept == new, "{what}: row {row} is not what it shows now\nkept: {kept:?}\nnow: {new:?}");
        }
        laid_out.len()
    }

    fn select(&mut self, ty: SelectionType, from: (i32, usize), to: (i32, usize)) {
        let mut selection = Selection::new(ty, GridPoint::new(Line(from.0), Column(from.1)), Side::Left);
        selection.update(GridPoint::new(Line(to.0), Column(to.1)), Side::Right);
        self.term.lock().selection = Some(selection);
    }
}

#[test]
fn a_right_click_finds_the_rule_colouring_the_word() {
    let mut g = Grid::new(60, 5);
    g.feed("got OOM\r\n$ cat OOM");
    let term = g.term.lock();
    let at = |line: i32, col: usize| super::highlighted_at(&*term, &g.highlighter, GridPoint::new(Line(line), Column(col)));
    let hit = at(0, 5).expect("OOM is highlighted");
    assert_eq!(hit.matched, "OOM");
    assert!(g.highlighter.rule_at("OOM", 0).is_some_and(|h| h.source == hit.source));
    assert_eq!(at(0, 1), None, "plain text");
    assert_eq!(at(1, 7), None, "the command being typed isn't highlighted");
    assert_eq!(at(0, 50), None, "past the end of the line");
}

#[test]
fn links_are_found_across_wrapped_rows_and_from_programs() {
    let mut g = Grid::new(20, 6);
    // 33 characters: wraps after 20.
    g.feed("see https://example.com/a/long/path. ok\r\n");
    g.feed("\x1b]8;;https://docs.lan/x\x07docs\x1b]8;;\x07 and \x1b]8;;file:///etc\x07file\x1b]8;;\x07\r\n");
    g.feed("from 10.0.0.5 port 22");
    let term = g.term.lock();
    let at = |line: i32, col: usize| super::link_at(&*term, GridPoint::new(Line(line), Column(col)));
    let wrapped = at(1, 3).expect("the second row of the link");
    assert_eq!(wrapped.url, "https://example.com/a/long/path");
    assert_eq!(wrapped.spans, [(Line(0), 4..20), (Line(1), 0..15)]);
    assert_eq!(at(0, 5), Some(wrapped));
    assert_eq!(at(0, 1), None, "plain text");
    assert_eq!(at(1, 17), None, "after the link");
    let program = at(2, 1).expect("an OSC 8 link");
    assert_eq!((program.url.as_str(), program.spans.clone()), ("https://docs.lan/x", vec![(Line(2), 0..4)]));
    assert_eq!(at(2, 11), None, "only web addresses open");
    assert_eq!(at(3, 7), None, "an IP address alone isn't a link");
}

/// The current search match's text colour in these tests.
const MATCH_TEXT: Rgb = Rgb { r: 0x10, g: 0x20, b: 0x30 };

#[test]
fn the_current_search_match_has_its_own_text_colour_and_is_laid_out_again_when_it_moves() {
    let glyph_color = |g: &Grid, row: usize, col: usize| {
        let x = g.origin.x + g.font.cell_w * col as f32;
        g.cache.rows[row].glyphs.iter().find(|gl| (gl.origin.x - x).abs() < g.font.cell_w * 0.5).map(|gl| gl.color).unwrap()
    };
    let mut g = Grid::new(30, 4);
    g.cursor_visible = false;
    g.feed("\x1b[31merror\x1b[0m one\r\n\x1b[2merror\x1b[0m two");
    g.frame("no search");
    let red = glyph_color(&g, 0, 0);
    assert_ne!(red, rgb_to_hsla(MATCH_TEXT));
    // The first is current: only its cells change colour.
    g.current_match = vec![(0, 0..5)];
    g.frame("first match current");
    assert_eq!(glyph_color(&g, 0, 0), rgb_to_hsla(MATCH_TEXT));
    assert_eq!(glyph_color(&g, 0, 6), glyph_color(&g, 1, 6), "after the match");
    // The next one: the first goes back, and dim text isn't dimmed on the solid background.
    g.current_match = vec![(1, 0..5)];
    g.frame("second match current");
    assert_eq!(glyph_color(&g, 0, 0), red);
    assert_eq!(glyph_color(&g, 1, 0), rgb_to_hsla(MATCH_TEXT));
    g.current_match.clear();
    g.frame("search closed");
}

#[test]
fn the_current_search_match_reads_on_its_background_in_every_theme() {
    for theme in crate::theme::themes(false).iter().chain(crate::theme::themes(true)) {
        let app = crate::theme::AppPalette::from_theme(theme);
        let colors = super::search_colors(app);
        let text = rgb_to_hsla(colors.current_text);
        let ratio = crate::theme::contrast(crate::theme::packed(text), crate::theme::packed(colors.current));
        assert!(ratio >= 4.5, "{}: {ratio:.2}", theme.name);
    }
}

#[test]
fn terminal_selection_uses_the_theme_foreground_even_for_dim_text() {
    for theme in crate::theme::themes(false).iter().chain(crate::theme::themes(true)) {
        let mut grid = Grid::new(30, 3);
        grid.palette = TerminalPalette::from_theme(theme);
        grid.cursor_visible = false;
        grid.feed("\x1b[2mSelected\x1b[0m");
        grid.select(SelectionType::Simple, (0, 0), (0, 7));
        grid.frame("selected dim text");
        let expected = crate::theme::rgb_to_hsla(grid.palette.selection_foreground);
        let glyph = grid.cache.rows[0].glyphs.iter().find(|g| g.id == gpui::GlyphId('S' as u32)).unwrap();
        assert_eq!(glyph.color, expected, "{} selection text", theme.name);
    }
}

#[test]
fn highlight_rules_leave_the_command_being_typed_alone() {
    fn color(g: &Grid, row: usize, c: char) -> gpui::Hsla {
        let glyphs = &g.cache.rows[row].glyphs;
        glyphs.iter().find(|gl| gl.id == gpui::GlyphId(c as u32)).map(|gl| gl.color).unwrap()
    }
    let mut g = Grid::new(60, 5);
    // Output that looks like a prompt, plain output, then the line being typed.
    g.feed("$ OOM
OOM
$ cat OOM");
    g.frame("typing");
    let plain = color(&g, 2, 'c');
    assert_ne!(color(&g, 1, 'O'), plain, "output is highlighted");
    assert_ne!(color(&g, 0, 'O'), plain, "output after something like a prompt is highlighted");
    assert_eq!(color(&g, 2, 'O'), plain, "the command being typed is not");
    // Entered (a Command Mark), it stays as typed once the cursor moves on.
    crate::marks::Marks::default().entered(&mut *g.term.lock());
    g.feed("
OOM
$ ");
    g.frame("entered");
    assert_eq!(color(&g, 2, 'O'), plain, "an entered command is not highlighted");
    assert_ne!(color(&g, 3, 'O'), plain, "its output is");
}


#[test]
fn grid_lays_out_only_the_rows_that_change() {
    let mut g = Grid::new(40, 10);
    for i in 0..30 {
        g.feed(format!("line {i} INFO \x1b[32mok\x1b[0m ERROR healthy\r\n"));
    }
    g.feed("\x1b[1;32muser@host\x1b[0m:~$ ");
    assert_eq!(g.frame("first frame"), 10);
    assert!(g.frame("nothing changed") <= 1);
    for c in "ls -la".chars() {
        g.feed(c.to_string());
        assert!(g.frame("typing") <= 1);
    }
    // A program redraws one line elsewhere and puts the cursor back.
    g.feed("\x1b7\x1b[3;5Hstatus: \x1b[1;31mERROR\x1b[0m\x1b8");
    assert!(g.frame("redraw") <= 2);
    g.feed("\x1b[5;1H");
    assert!(g.frame("cursor moved") <= 2);
    g.feed("\x1b[7mreverse\x1b[0m \x1b[4munder\x1b[9mstrike\x1b[0m e\u{301} \u{65e5}\u{672c} \u{2764}\u{fe0f} \u{2500}\u{2524}");
    assert!(g.frame("attributes, marks, wide characters") <= 1);
    g.feed("\x1b[5;3H");
    assert!(g.frame("cursor on a wide character") <= 1);

    g.focused = false;
    assert!(g.frame("unfocused") <= 1);
    g.focused = true;
    assert!(g.frame("focused") <= 1);
    g.cursor_visible = false;
    assert!(g.frame("blink off") <= 1);
    assert!(g.cache.rows.iter().all(|row| row.cursor_bars.is_empty() && row.cursor_outline.is_none()));
    g.cursor_visible = true;
    assert!(g.frame("blink on") <= 1);
    assert!(g.cache.rows.iter().any(|row| !row.cursor_bars.is_empty()));
    g.feed("\x1b[6 q");
    assert!(g.frame("bar cursor") <= 1);
    g.feed("\x1b[?25l");
    assert!(g.frame("cursor hidden") <= 1);
    g.feed("\x1b[?25h\x1b[2 q");
    assert!(g.frame("cursor shown") <= 1);

    g.select(SelectionType::Simple, (2, 3), (4, 7));
    assert!(g.frame("selection") <= 4);
    g.select(SelectionType::Block, (6, 1), (7, 9));
    assert!(g.frame("other selection") <= 6);
    g.select(SelectionType::Lines, (-3, 0), (0, 0));
    g.frame("selection into the Scrollback");
    g.term.lock().selection = None;
    assert!(g.frame("no selection") <= 2);

    // Output at the bottom scrolls every row.
    g.feed("\x1b[10;1H\r\nnext");
    assert_eq!(g.frame("scrolled by output"), 10);

    g.term.lock().scroll_display(Scroll::Delta(5));
    assert_eq!(g.frame("view scrolled up"), 10);
    assert!(g.frame("still scrolled up") <= 1);
    g.feed("x");
    g.frame("typing below the view");
    g.feed("\r\nmore output\r\n");
    g.frame("output while scrolled up");
    g.term.lock().scroll_display(Scroll::Delta(2));
    g.feed("\x1b[1;1Hy");
    g.frame("a change on screen while scrolled up");
    g.term.lock().grid_mut().clear_history();
    g.frame("Scrollback dropped while scrolled up");
    g.term.lock().scroll_display(Scroll::Bottom);
    g.frame("back at the bottom");
    // Scrolled up, a region below the top scrolling raises the terminal's offset past its
    // Scrollback (see `view_offset`).
    g.feed("\r\none\r\ntwo\r\n");
    g.term.lock().scroll_display(Scroll::Top);
    g.frame("scrolled to the top");
    g.feed("\x1b7\x1b[3;8r\x1b[8;1H\n\n\n\n\n\n\x1b[r\x1b8");
    g.frame("a region scrolled while scrolled up");
    g.feed("z");
    g.frame("typing with the offset past the Scrollback");
    g.term.lock().scroll_display(Scroll::Bottom);
    g.frame("back at the bottom again");

    crate::session::resize(&mut *g.term.lock(), GridSize { cols: 30, rows: 8, cell_width: 8, cell_height: 18 });
    assert_eq!(g.frame("narrower and shorter"), 8);
    crate::session::resize(&mut *g.term.lock(), GridSize { cols: 50, rows: 12, cell_width: 8, cell_height: 18 });
    assert_eq!(g.frame("wider and taller"), 12);

    g.palette = TerminalPalette::light();
    assert_eq!(g.frame("light theme"), 12);
    g.highlighter = Arc::new(Highlighter::default());
    assert_eq!(g.frame("no highlight rules"), 12);
    let sets = crate::highlight::load(std::path::Path::new("/nonexistent/komando-grid-test")).0;
    g.highlighter = Arc::new(Highlighter::from_sets(&sets, &[]));
    assert_eq!(g.frame("highlight rules back"), 12);
    g.font = grid_font(16.);
    assert_eq!(g.frame("bigger font"), 12);
    g.origin = gpui::point(px(40.), px(8.));
    assert_eq!(g.frame("moved"), 12);
    g.scale = 2.;
    assert_eq!(g.frame("other display"), 12);
    assert!(g.frame("nothing changed") <= 1);

    g.feed("\x1b]4;1;rgb:12/34/56\x07\x1b[31mred\x1b[0m");
    g.frame("palette colour changed by the program");
    g.feed("\x1b]12;rgb:ff/00/00\x07");
    g.frame("cursor colour changed by the program");

    g.feed("\x1b[?1049h\x1b[2J\x1b[1;1Htop - 10:00 ERROR\x1b[3;1HPID USER");
    assert_eq!(g.frame("alternate screen"), 12);
    g.feed("\x1b[3;5H99");
    assert!(g.frame("full-screen program updates a cell") <= 2);
    g.feed("\x1b[?1049l");
    assert_eq!(g.frame("back from the alternate screen"), 12);

    g.feed("\x1b[?2026h\x1b[2;1Hhalf drawn");
    g.frame("inside a synchronized update");
    g.feed("\x1b[?2026l");
    g.frame("synchronized update done");
    g.feed("\x1b[4hins\x1b[4l");
    g.frame("insert mode");
    g.feed("\x1b[2;5r\x1b[5;1H\n\n\x1b[r");
    g.frame("scroll region");
    g.feed("\x1b[3;1H\x1b[K\x1b[1J");
    g.frame("erased");
    g.feed("\x1bc");
    g.frame("reset");
}

#[test]
fn grid_rows_from_damage_match_a_full_layout_under_random_output() {
    let mut g = Grid::new(24, 8);
    let mut seed = 0x2545_f491_4f6c_dd1d_u64;
    let mut next = move || {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        seed
    };
    let pieces: &[&str] = &[
        "hello", " ", "\r\n", "\n", "\r", "\t", "\x08", "ERROR", "INFO healthy", "e\u{301}", "\u{65e5}\u{672c}",
        "\u{2764}\u{fe0f}", "\u{2500}\u{2502}", "\x1b[K", "\x1b[1K", "\x1b[2K", "\x1b[J", "\x1b[1J", "\x1b[2J",
        "\x1b[3L", "\x1b[2M", "\x1b[4@", "\x1b[3P", "\x1b[5X", "\x1bM", "\x1bD", "\x1bE", "\x1b[2S", "\x1b[T",
        "\x1b[3;6r", "\x1b[r", "\x1b[1;31m", "\x1b[0m", "\x1b[7m", "\x1b[4:3m", "\x1b[8m", "\x1b[2m", "\x1b[3m",
        "\x1b[48;5;22m", "\x1b[?25l", "\x1b[?25h", "\x1b[4h", "\x1b[4l", "\x1b[?1049h", "\x1b[?1049l", "\x1b[2 q",
        "\x1b[5 q", "\x1b#8", "\x1b[?2026hsync\x1b[?2026l", "\x1b]4;2;rgb:aa/bb/cc\x07", "\x1b]104\x07",
        "\x1b]11;rgb:10/20/30\x07", "\x1b[3G", "\x1b[10C", "\x1b[2A", "\x1b[3B", "\x1b[2D", "\x1b7", "\x1b8",
        "\x1b[?6h", "\x1b[?6l", "\x1b[?7l", "\x1b[?7h", "\x1b[3g", "\x1b[3J", "\x1bc",
    ];
    let mut partial = 0;
    for step in 0..3000 {
        for _ in 0..next() % 4 + 1 {
            let n = next();
            match n % 40 {
                0 => g.feed(format!("\x1b[{};{}H", n / 40 % 10, n / 400 % 30)),
                1 => {
                    let delta = (n / 40 % 7) as i32 - 3;
                    g.term.lock().scroll_display(Scroll::Delta(delta));
                }
                2 => {
                    let row = |n: u64| (n % 12) as i32 - 4;
                    let ty = [SelectionType::Simple, SelectionType::Block, SelectionType::Lines][(n / 40 % 3) as usize];
                    g.select(ty, (row(n / 120), (n / 1440 % 24) as usize), (row(n / 17280), (n / 207360 % 24) as usize));
                }
                3 => g.term.lock().selection = None,
                4 => g.focused = !g.focused,
                5 if n / 40 % 8 == 0 => {
                    let size = GridSize { cols: 10 + (n / 320 % 20) as u16, rows: 3 + (n / 6400 % 8) as u16, cell_width: 8, cell_height: 18 };
                    crate::session::resize(&mut *g.term.lock(), size);
                }
                6 if n / 40 % 8 == 0 => g.palette = if g.palette.dark { TerminalPalette::light() } else { TerminalPalette::dark() },
                _ => g.feed(pieces[(n / 40) as usize % pieces.len()]),
            }
        }
        let lines = g.term.lock().screen_lines();
        if g.frame(&format!("step {step}")) < lines {
            partial += 1;
        }
    }
    // Many frames kept rows: the test is about those.
    assert!(partial > 3000 / 4, "{partial} of 3000 frames kept rows");
}

#[test]
fn scrolling_reuses_highlights_and_keeps_the_cache_bounded() {
    let mut g = Grid::new(60, 10);
    for i in 0..30 {
        g.feed(format!("line {i} ERROR 日本 e\u{301}\r\n"));
    }
    g.frame("first paint");
    let scans = g.cache.highlights.scans;
    for i in 30..1230 {
        let before = g.cache.highlights.scans;
        g.feed(format!("line {i} ERROR 日本 e\u{301}\r\n"));
        assert_eq!(g.frame("one new line, other rows moved"), 10);
        assert!(g.cache.highlights.scans - before <= 2, "moved rows ran the rules again");
        assert!(g.cache.highlights.rows.len() <= 2 * (super::HIGHLIGHT_GENERATION + 10));
    }
    assert!(g.cache.highlights.scans > scans, "new output still runs the rules");
}

#[test]
fn highlight_cache_uses_exact_text_and_invalidates_rules_and_background() {
    let mut cache = super::HighlightCache::default();
    let rules = Arc::new(Highlighter::default());
    let calls = Cell::new(0);
    let compute = || { calls.set(calls.get() + 1); Box::default() };
    cache.prepare(&rules, true);
    cache.spans("ERROR 日本", compute);
    cache.spans("ERROR 日本", compute);
    assert_eq!(calls.get(), 1);
    cache.spans("ERROR 日木", compute);
    assert_eq!(calls.get(), 2, "different text must not reuse a result");
    cache.prepare(&rules, false);
    cache.spans("ERROR 日本", compute);
    assert_eq!(calls.get(), 3, "background-dependent colors");
    cache.prepare(&Arc::new(Highlighter::default()), false);
    cache.spans("ERROR 日本", compute);
    assert_eq!(calls.get(), 4, "new rules");
}

/// Frames of a log scrolling a few lines at a time: the terminal damages every row when it
/// scrolls, so every row is laid out again each frame. Times them without highlight rules,
/// with them, and with them but each row's highlights worked out afresh (as before
/// `HighlightCache`). Reports timings without thresholds: `cargo test --lib
/// bench_grid_frames -- --ignored --nocapture`, in release mode for real numbers.
#[test]
#[ignore]
fn bench_grid_frames_while_output_scrolls() {
    let line = |i: u64| {
        let level = ["\x1b[31mERROR", "\x1b[33mWARN ", "\x1b[32mINFO ", "\x1b[32mINFO "][(i % 4) as usize];
        format!(
            "\x1b[2m2026-11-13 10:{:02}:{:02}.{:03}\x1b[0m {level}\x1b[0m api GET /api/items/{i} from 10.0.{}.{} {} in {}ms healthy\r\n",
            i / 60 % 60,
            i % 60,
            i % 1000,
            i % 256,
            i * 7 % 256,
            if i % 16 == 0 { 500 } else { 200 },
            i % 997,
        )
    };
    let sets = crate::highlight::load(std::path::Path::new("/nonexistent/komando-grid-bench")).0;
    let rules = Arc::new(Highlighter::from_sets(&sets, &[]));
    let cases = [
        ("no rules", Arc::new(Highlighter::default()), true),
        ("default rules", rules.clone(), true),
        ("default rules, uncached", rules, false),
    ];
    for (name, highlighter, cached) in cases {
        let mut g = Grid::new(160, 45);
        g.highlighter = highlighter;
        let mut i = 0;
        for _ in 0..200 {
            g.feed(line(i));
            i += 1;
        }
        let frames = 1000;
        let mut took = Duration::ZERO;
        for _ in 0..frames {
            for _ in 0..3 {
                g.feed(line(i));
                i += 1;
            }
            if !cached {
                g.cache.highlights = Default::default();
            }
            let Grid { term, font, origin, scale, palette, highlighter, focused, cursor_visible, cache, .. } = &mut g;
            let look = GridLook {
                font,
                origin: *origin,
                scale: *scale,
                palette,
                highlighter,
                focused: *focused,
                cursor_visible: *cursor_visible,
                current_match: &[],
                match_foreground: MATCH_TEXT,
            };
            let start = Instant::now();
            layout_grid(term, &FakeShaper, &look, cache);
            took += start.elapsed();
        }
        eprintln!("{name}: {:?} per 160x45 frame scrolled by output", took / frames);
    }
}
