//! The minimap: the Scrollback in miniature, laid over the terminal's right edge like the
//! minimap in VS Code. Each terminal line is 2px high and each character a sliver, coloured like
//! the text (Highlight Rules included), so errors and long blocks of output stand out. A box
//! marks the lines on screen; click or drag to scroll there.
//!
//! It takes no columns from the terminal. Its width follows the terminal's, its height the
//! output's (up to the terminal's), and it only shows once there is Scrollback to move through:
//! with everything on screen there is nothing to scroll.
//!
//! When the Scrollback is taller than the strip, the strip scrolls with the terminal, the same
//! way VS Code's minimap does: its position follows the viewport proportionally.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;

use alacritty_terminal::grid::{Dimensions, Scroll};
use alacritty_terminal::index::{Column, Line};
use alacritty_terminal::term::Term;
use alacritty_terminal::term::cell::Flags;
use alacritty_terminal::vte::ansi::{Color, Rgb};
use gpui::{
    AnyElement, Bounds, Context, Hsla, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, Pixels, canvas,
    div, fill, point, prelude::*, px, size,
};

use crate::highlight::{self, Highlighter};
use crate::session::SharedTerm;
use crate::theme::{AppPalette, TerminalPalette, rgb_to_hsla};
use crate::ui::terminal_view::TerminalView;

/// Narrowest and widest the minimap gets; in between it is a tenth of the terminal's width.
const MIN_WIDTH: f32 = 56.;
const MAX_WIDTH: f32 = 110.;
/// Space above and below the lines.
const PAD_Y: f32 = 8.;
/// Height of one terminal line on the minimap.
const LINE_H: f32 = 2.;
/// Width of one character.
const CHAR_W: f32 = 1.;
const PAD_X: f32 = 8.;

/// What the last paint showed, so a click can be turned into a line.
#[derive(Clone, Copy, Default)]
struct Geometry {
    top: f32,
    /// First Scrollback line (counted from the oldest) shown at the top of the strip.
    start: usize,
    history: usize,
    screen: usize,
}

/// One run of same-coloured characters on a minimap line, in columns.
#[derive(Clone, Copy)]
struct Run {
    from: u16,
    to: u16,
    color: Hsla,
}

/// Runs of recently drawn lines, keyed by a hash of the line's characters and colours. Highlight
/// Rules are regexes; running them on every visible line at every repaint is what made the
/// minimap slow on long logs. With the cache a line is only worked out again when it changes.
#[derive(Default)]
struct LineCache {
    lines: HashMap<u64, Rc<[Run]>>,
    /// What the runs depend on besides the line itself (the Highlight Rules and the theme);
    /// when it changes, everything is worked out again.
    key: Option<(usize, bool)>,
}

/// Lines kept in the cache before it starts over; a few screens' worth of minimap.
const CACHE_LINES: usize = 8192;

#[derive(Default)]
pub struct MinimapState {
    geometry: Cell<Geometry>,
    dragging: Cell<bool>,
    cache: RefCell<LineCache>,
}

/// Scroll so the line under `y` is in the middle of the screen.
fn scroll_to(term: &SharedTerm, state: &MinimapState, y: Pixels) {
    let g = state.geometry.get();
    let line = g.start as f32 + ((f32::from(y) - g.top) / LINE_H).max(0.);
    let top = (line - g.screen as f32 / 2.).round().clamp(0., g.history as f32) as i32;
    let mut term = term.lock();
    let offset = term.grid().display_offset() as i32;
    let target = g.history as i32 - top;
    if target != offset {
        term.scroll_display(Scroll::Delta(target - offset));
    }
}

/// The minimap's width for a terminal `width` pixels wide.
pub fn width_for(width: f32) -> f32 {
    (width * 0.1).clamp(MIN_WIDTH, MAX_WIDTH)
}

/// Lines with output: the Scrollback plus the screen down to its last written line (or the
/// cursor), not the blank rows below it.
fn content_lines<T>(term: &Term<T>) -> usize {
    let grid = term.grid();
    let cols = grid.columns();
    let mut used = grid.cursor.point.line.0.max(0) as usize + 1;
    for row in (used..grid.screen_lines()).rev() {
        let line = &grid[Line(row as i32)];
        if (0..cols).any(|c| line[Column(c)].c != ' ') {
            used = row + 1;
            break;
        }
    }
    grid.history_size() + used.min(grid.screen_lines())
}

/// What the minimap shows for `bounds`: where it starts in the Scrollback, one rectangle per
/// run of characters, and the box over the lines on screen.
fn layout<T>(
    term: &Term<T>,
    cache: &mut LineCache,
    palette: &TerminalPalette,
    highlighter: &Arc<Highlighter>,
    bounds: Bounds<Pixels>,
) -> (Geometry, Vec<(Bounds<Pixels>, Hsla)>, Bounds<Pixels>) {
    let grid = term.grid();
    let history = grid.history_size();
    let screen = grid.screen_lines();
    let cols = grid.columns();
    let total = content_lines(term);
    let offset = grid.display_offset();
    let viewport_top = history - offset;

    let capacity = (f32::from(bounds.size.height) / LINE_H).floor().max(1.) as usize;
    let start = if total <= capacity {
        0
    } else {
        let scrollable = history.max(1) as f32;
        ((viewport_top as f32 / scrollable) * (total - capacity) as f32).round() as usize
    };
    let geometry = Geometry { top: f32::from(bounds.origin.y), start, history, screen };

    let key = (Arc::as_ptr(highlighter) as usize, palette.dark);
    if cache.key != Some(key) || cache.lines.len() > CACHE_LINES {
        cache.lines.clear();
        cache.key = Some(key);
    }

    // Like VS Code: a fixed width per character, and long lines are cut off at the edge.
    // The start of a line (prompt, timestamp, log level) is what you scan for.
    let shown_cols = (((f32::from(bounds.size.width) - PAD_X * 2.) / CHAR_W).floor() as usize).min(cols);
    let colors = term.colors();
    let mut rects: Vec<(Bounds<Pixels>, Hsla)> = Vec::new();
    let end = total.min(start + capacity);
    let mut cells: Vec<(usize, char, Rgb)> = Vec::with_capacity(shown_cols);
    for abs in start..end {
        let row = &grid[Line(abs as i32 - history as i32)];
        // Identify the line by its raw cells: characters, colours as the terminal stores them
        // and the flags that change how they show. Cheap enough for every line at every
        // repaint; the colours are only resolved and the rules only run when it's new.
        let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
        for col in 0..shown_cols {
            let cell = &row[Column(col)];
            if cell.c == ' ' && !cell.flags.intersects(Flags::INVERSE) {
                continue;
            }
            let flags = (cell.flags & (Flags::INVERSE | Flags::HIDDEN | Flags::WIDE_CHAR_SPACER)).bits() as u64;
            let color = |c: Color| match c {
                Color::Spec(rgb) => (rgb.r as u64) << 16 | (rgb.g as u64) << 8 | rgb.b as u64,
                Color::Named(n) => 1 << 24 | n as u64,
                Color::Indexed(i) => 2 << 24 | i as u64,
            };
            for word in [col as u64 | (cell.c as u64) << 16 | flags << 40, color(cell.fg) | color(cell.bg) << 26] {
                hash = (hash ^ word).wrapping_mul(0x0000_0100_0000_01b3).rotate_left(23);
            }
        }
        let runs = match cache.lines.get(&hash) {
            Some(runs) => runs.clone(),
            None => {
                cells.clear();
                for col in 0..shown_cols {
                    let cell = &row[Column(col)];
                    if cell.flags.contains(Flags::WIDE_CHAR_SPACER) {
                        continue;
                    }
                    let mut fg = palette.resolve(cell.fg, colors);
                    if cell.flags.contains(Flags::INVERSE) {
                        fg = palette.resolve(cell.bg, colors);
                    }
                    let ch = if cell.flags.contains(Flags::HIDDEN) { ' ' } else { cell.c };
                    cells.push((col, ch, fg));
                }
                let runs = line_runs(&cells, palette, highlighter);
                cache.lines.insert(hash, runs.clone());
                runs
            }
        };
        let y = f32::from(bounds.origin.y) + (abs - start) as f32 * LINE_H;
        for run in runs.iter() {
            let x = f32::from(bounds.origin.x) + PAD_X + run.from as f32 * CHAR_W;
            rects.push((
                Bounds::new(point(px(x), px(y)), size(px((run.to - run.from) as f32 * CHAR_W), px(LINE_H - 0.5))),
                run.color,
            ));
        }
    }

    // The box over the lines that are on screen, cut off where the output ends.
    let box_top = (viewport_top as f32 - start as f32) * LINE_H;
    let box_bottom = ((viewport_top + screen).min(total) as f32 - start as f32) * LINE_H;
    let box_bottom = box_bottom.min(f32::from(bounds.size.height)).max(box_top);
    let viewport = Bounds::new(
        point(bounds.origin.x, bounds.origin.y + px(box_top)),
        size(bounds.size.width, px(box_bottom - box_top)),
    );
    (geometry, rects, viewport)
}

/// The coloured runs of one line: Highlight Rules applied, plain text faint and coloured text
/// strong, so errors and warnings stand out in the strip.
fn line_runs(cells: &[(usize, char, Rgb)], palette: &TerminalPalette, highlighter: &Highlighter) -> Rc<[Run]> {
    let plain_alpha = if palette.dark { 0.3 } else { 0.25 };
    let styles = if highlighter.rule_count() > 0 {
        let text: String = cells.iter().map(|c| c.1).collect();
        highlighter.styles(&text)
    } else {
        None
    };
    let mut runs: Vec<Run> = Vec::new();
    let mut run: Option<(usize, usize, Rgb)> = None;
    let flush = |run: &mut Option<(usize, usize, Rgb)>, runs: &mut Vec<Run>| {
        if let Some((from, to, color)) = run.take() {
            let mut c = rgb_to_hsla(color);
            c.a *= if c.s < 0.2 { plain_alpha } else { 0.95 };
            runs.push(Run { from: from as u16, to: to as u16, color: c });
        }
    };
    let mut byte = 0;
    for &(col, ch, fg) in cells {
        let rule = styles.as_ref().and_then(|s| s[byte]);
        byte += ch.len_utf8();
        if ch.is_whitespace() {
            flush(&mut run, &mut runs);
            continue;
        }
        let color = match rule.and_then(|r| r.fg) {
            Some(c) => {
                let c = highlight::readable_on(c, palette.dark);
                Rgb { r: c.r, g: c.g, b: c.b }
            }
            None => fg,
        };
        match &mut run {
            Some((_, to, c)) if *c == color && *to == col => *to = col + 1,
            _ => {
                flush(&mut run, &mut runs);
                run = Some((col, col + 1, color));
            }
        }
    }
    flush(&mut run, &mut runs);
    runs.into()
}

/// The minimap for a terminal `width` pixels wide, laid over its right edge. `None` while there
/// is no Scrollback: everything is on screen and there is nothing to scroll.
pub fn minimap(
    term: SharedTerm,
    state: Rc<MinimapState>,
    palette: TerminalPalette,
    app: AppPalette,
    highlighter: Arc<Highlighter>,
    width: f32,
    cx: &mut Context<TerminalView>,
) -> Option<AnyElement> {
    let lines = {
        let term = term.lock();
        (term.grid().history_size() > 0).then(|| content_lines(&term))?
    };
    // As tall as the output, up to the terminal's height.
    let height = lines as f32 * LINE_H + PAD_Y * 2.;
    let backdrop = palette.background_hsla();
    let paint_state = state.clone();
    let paint_box_state = state.clone();
    let paint_term = term.clone();
    let strip = canvas(
        move |bounds, _window, _cx| {
            let term = paint_term.lock();
            let mut cache = paint_state.cache.borrow_mut();
            let (geometry, rects, viewport) =
                crate::perf::time("minimap layout", || layout(&term, &mut cache, &palette, &highlighter, bounds));
            paint_state.geometry.set(geometry);
            (rects, viewport)
        },
        move |_bounds, (rects, viewport), window, _cx| {
            // The box over the lines on screen: a fill plus a hairline outline, brighter while
            // it is being dragged.
            let dragging = paint_box_state.dragging.get();
            window.paint_quad(
                fill(viewport, if dragging { app.active.opacity(1.5) } else { app.active })
                    .border_widths(px(1.))
                    .border_color(if dragging { app.text_faint } else { app.border.opacity(2.) }),
            );
            for (bounds, color) in rects {
                window.paint_quad(fill(bounds, color));
            }
        },
    )
    .size_full();

    let (down_term, move_term) = (term.clone(), term);
    let (down_state, move_state, up_state) = (state.clone(), state.clone(), state);
    let element = div()
        .id("minimap")
        .absolute()
        .top_0()
        .right_0()
        .w(px(width))
        .h(px(height))
        .max_h_full()
        .py(px(PAD_Y))
        // Over the text: the terminal's own colour, mostly opaque, so the miniature reads and the
        // text underneath still shows through faintly.
        .bg(backdrop.opacity(0.82))
        .rounded_bl(px(8.))
        .cursor_default()
        // Clicks here scroll; they must not start a text selection in the terminal.
        .on_mouse_down(
            MouseButton::Left,
            cx.listener(move |_, event: &MouseDownEvent, _, cx| {
                cx.stop_propagation();
                down_state.dragging.set(true);
                scroll_to(&down_term, &down_state, event.position.y);
                cx.notify();
            }),
        )
        .on_mouse_move(cx.listener(move |_, event: &MouseMoveEvent, _, cx| {
            if move_state.dragging.get() && event.pressed_button == Some(MouseButton::Left) {
                cx.stop_propagation();
                scroll_to(&move_term, &move_state, event.position.y);
                cx.notify();
            }
        }))
        .on_mouse_up(
            MouseButton::Left,
            cx.listener(move |_, _: &MouseUpEvent, _, _| up_state.dragging.set(false)),
        )
        .child(strip)
        .into_any_element();
    Some(element)
}

#[cfg(test)]
mod tests {
    use super::*;
    use alacritty_terminal::event::VoidListener;
    use alacritty_terminal::term::Config;
    use alacritty_terminal::term::test::TermSize;
    use alacritty_terminal::vte::ansi::Processor;
    use std::time::Instant;

    /// A terminal holding `lines` lines of coloured log output, like `journalctl` on a busy box.
    fn busy_log(lines: usize) -> Term<VoidListener> {
        let config = Config { scrolling_history: lines, ..Config::default() };
        let mut term = Term::new(config, &TermSize::new(160, 50), VoidListener);
        let mut parser: Processor = Processor::new();
        let mut text = String::new();
        for i in 0..lines {
            let level = match i % 37 {
                0 => "\x1b[31mERROR\x1b[0m",
                5 | 17 => "\x1b[33mWARN \x1b[0m",
                _ => "\x1b[32mINFO \x1b[0m",
            };
            text.push_str(&format!(
                "\x1b[2m2026-11-11T09:{:02}:{:02}Z\x1b[0m {level} http GET /v1/orders?page={} 200 {}ms user={} trace={:x}\r\n",
                i / 60 % 60,
                i % 60,
                i % 12,
                5 + i % 40,
                i % 997,
                i * 2654435761 % 1_000_003
            ));
        }
        parser.advance(&mut term, text.as_bytes());
        term
    }

    fn bundled_rules() -> Arc<Highlighter> {
        let sets = crate::highlight::parse(include_str!("../../assets/highlights/default.json")).unwrap();
        Arc::new(Highlighter::from_sets(&sets, &[]))
    }

    fn strip() -> Bounds<Pixels> {
        Bounds::new(point(px(0.), px(0.)), size(px(MAX_WIDTH), px(800.)))
    }

    #[test]
    fn only_lines_with_output_count() {
        let config = Config { scrolling_history: 100, ..Config::default() };
        let mut term = Term::new(config, &TermSize::new(80, 24), VoidListener);
        let mut parser: Processor = Processor::new();
        // A fresh session: one prompt line, no Scrollback.
        parser.advance(&mut term, b"user@web01:~$ ");
        assert_eq!(content_lines(&term), 1);
        assert_eq!(term.grid().history_size(), 0);
        // Blank rows below the cursor don't count; the cursor's row does.
        parser.advance(&mut term, b"ls\r\na b c\r\n");
        assert_eq!(content_lines(&term), 3);
        // Once output scrolls off, the Scrollback counts too.
        for i in 0..40 {
            parser.advance(&mut term, format!("line {i}\r\n").as_bytes());
        }
        // 42 written rows plus the cursor's: 19 scrolled off a 24-row screen.
        assert_eq!(term.grid().history_size(), 43 - 24);
        assert_eq!(content_lines(&term), 43);
    }

    #[test]
    fn the_box_stops_where_the_output_ends() {
        let term = busy_log(30);
        let (_, _, viewport) = layout(&term, &mut LineCache::default(), &TerminalPalette::dark(), &bundled_rules(), strip());
        // 30 lines + the empty prompt row, of a 50-row screen: the box covers what's there.
        let lines = content_lines(&term);
        assert_eq!(f32::from(viewport.size.height), (lines as f32 * LINE_H).min(800.));
        assert_eq!(term.grid().display_offset(), 0);
    }

    #[test]
    fn width_follows_the_terminal() {
        assert_eq!(width_for(300.), MIN_WIDTH);
        assert_eq!(width_for(800.), 80.);
        assert_eq!(width_for(3000.), MAX_WIDTH);
    }

    #[test]
    fn cached_lines_draw_the_same_as_fresh_ones() {
        let mut term = busy_log(5_000);
        let palette = TerminalPalette::dark();
        let rules = bundled_rules();
        let mut cache = LineCache::default();
        term.scroll_display(Scroll::Delta(1_000));
        let (_, first, _) = layout(&term, &mut cache, &palette, &rules, strip());
        let (_, again, _) = layout(&term, &mut cache, &palette, &rules, strip());
        let (_, fresh, _) = layout(&term, &mut LineCache::default(), &palette, &rules, strip());
        assert!(!first.is_empty());
        assert_eq!(first, again);
        assert_eq!(first, fresh);
    }

    /// Scrolling through a long log: time 200 repaints with and without the line cache.
    /// `cargo test --lib minimap -- --ignored --nocapture` prints the numbers.
    #[test]
    #[ignore]
    fn scrolling_a_long_log() {
        let mut term = busy_log(100_000);
        let palette = TerminalPalette::dark();
        let rules = bundled_rules();
        let mut run = |cached: bool| {
            let mut cache = LineCache::default();
            let mut quads = 0;
            let t = Instant::now();
            for frame in 0..200 {
                term.scroll_display(Scroll::Delta(if frame < 100 { 3 } else { -3 }));
                if !cached {
                    cache = LineCache::default();
                }
                quads += layout(&term, &mut cache, &palette, &rules, strip()).1.len();
            }
            (t.elapsed() / 200, quads / 200)
        };
        let (fresh, quads) = run(false);
        let (cached, _) = run(true);
        println!("per repaint: {fresh:?} without the cache, {cached:?} with it, {quads} rectangles");
    }
}
