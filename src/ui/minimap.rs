//! The minimap: a narrow strip on the right of the terminal showing the Scrollback in
//! miniature, like the minimap in VS Code. Each terminal line is 2px high and each character a
//! sliver, coloured like the text (Highlight Rules included), so errors and long blocks of
//! output stand out. A box marks the lines on screen; click or drag to scroll there.
//!
//! When the Scrollback is taller than the strip, the strip scrolls with the terminal, the same
//! way VS Code's minimap does: its position follows the viewport proportionally.

use std::cell::Cell;
use std::rc::Rc;
use std::sync::Arc;

use alacritty_terminal::grid::{Dimensions, Scroll};
use alacritty_terminal::index::{Column, Line};
use alacritty_terminal::term::cell::Flags;
use alacritty_terminal::vte::ansi::Rgb;
use gpui::{
    AnyElement, Bounds, Context, Hsla, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, Pixels, canvas,
    div, fill, point, prelude::*, px, size,
};

use crate::highlight::{self, Highlighter};
use crate::session::SharedTerm;
use crate::theme::{AppPalette, TerminalPalette, rgb_to_hsla};
use crate::ui::terminal_view::TerminalView;

pub const WIDTH: f32 = 100.;
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

#[derive(Default)]
pub struct MinimapState {
    geometry: Cell<Geometry>,
    dragging: Cell<bool>,
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

pub fn minimap(
    term: SharedTerm,
    state: Rc<MinimapState>,
    palette: TerminalPalette,
    app: AppPalette,
    highlighter: Arc<Highlighter>,
    cx: &mut Context<TerminalView>,
) -> AnyElement {
    let paint_state = state.clone();
    let paint_term = term.clone();
    let strip = canvas(
        move |bounds, _window, _cx| {
            let term = paint_term.lock();
            let grid = term.grid();
            let history = grid.history_size();
            let screen = grid.screen_lines();
            let cols = grid.columns();
            let total = history + screen;
            let offset = grid.display_offset();
            let viewport_top = history - offset;

            let capacity = (f32::from(bounds.size.height) / LINE_H).floor().max(1.) as usize;
            let start = if total <= capacity {
                0
            } else {
                let scrollable = (total - screen).max(1) as f32;
                ((viewport_top as f32 / scrollable) * (total - capacity) as f32).round() as usize
            };
            paint_state.geometry.set(Geometry { top: f32::from(bounds.origin.y), start, history, screen });

            // Like VS Code: a fixed width per character, and long lines are cut off at the edge.
            // The start of a line (prompt, timestamp, log level) is what you scan for.
            let char_w = CHAR_W;
            let shown_cols = (((f32::from(bounds.size.width) - PAD_X * 2.) / char_w).floor() as usize).min(cols);
            let colors = term.colors();
            let highlight = highlighter.rule_count() > 0;
            let mut rects: Vec<(Bounds<Pixels>, Hsla)> = Vec::new();
            let end = total.min(start + capacity);
            let mut text = String::with_capacity(cols);
            let mut cells: Vec<(usize, char, Rgb)> = Vec::with_capacity(cols);
            for abs in start..end {
                let row = &grid[Line(abs as i32 - history as i32)];
                text.clear();
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
                    text.push(ch);
                    cells.push((col, ch, fg));
                }
                let styles = if highlight { highlighter.styles(&text) } else { None };
                let y = f32::from(bounds.origin.y) + (abs - start) as f32 * LINE_H;
                // One rectangle per run of visible characters in the same colour.
                let mut run: Option<(usize, usize, Rgb)> = None;
                let mut byte = 0;
                let flush = |run: &mut Option<(usize, usize, Rgb)>, rects: &mut Vec<(Bounds<Pixels>, Hsla)>| {
                    if let Some((from, to, color)) = run.take() {
                        let mut c = rgb_to_hsla(color);
                        c.a *= 0.7;
                        let x = f32::from(bounds.origin.x) + PAD_X + from as f32 * char_w;
                        rects.push((
                            Bounds::new(point(px(x), px(y)), size(px((to - from) as f32 * char_w), px(LINE_H - 0.5))),
                            c,
                        ));
                    }
                };
                for &(col, ch, fg) in &cells {
                    let rule = styles.as_ref().and_then(|s| s[byte]);
                    byte += ch.len_utf8();
                    if ch.is_whitespace() {
                        flush(&mut run, &mut rects);
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
                            flush(&mut run, &mut rects);
                            run = Some((col, col + 1, color));
                        }
                    }
                }
                flush(&mut run, &mut rects);
            }

            // The box over the lines that are on screen.
            let viewport = Bounds::new(
                point(
                    bounds.origin.x,
                    bounds.origin.y + px((viewport_top as f32 - start as f32) * LINE_H),
                ),
                size(bounds.size.width, px(screen as f32 * LINE_H)),
            );
            (rects, viewport)
        },
        move |_bounds, (rects, viewport), window, _cx| {
            window.paint_quad(fill(viewport, app.hover));
            for (bounds, color) in rects {
                window.paint_quad(fill(bounds, color));
            }
        },
    )
    .size_full();

    let (down_term, move_term) = (term.clone(), term);
    let (down_state, move_state, up_state) = (state.clone(), state.clone(), state);
    div()
        .id("minimap")
        .w(px(WIDTH))
        .h_full()
        .flex_shrink_0()
        .py(px(8.))
        .border_l_1()
        .border_color(app.border)
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
        .into_any_element()
}
