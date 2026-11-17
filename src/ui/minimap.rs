//! The minimap: the Scrollback in miniature on the terminal's right edge, like the minimap in
//! VS Code. Each terminal line is 2px high and each character a sliver, coloured like the text
//! (Highlight Rules included), so errors and long blocks of output stand out. A slider marks
//! the lines on screen: drag it to scroll, or click anywhere else to jump there.
//! Wide terminal rows are compressed horizontally so highlights near the end stay visible.
//!
//! It has two placements:
//! - **Beside the output** (the default, and what VS Code does): the minimap has a column of its
//!   own, so output never runs underneath it, and it is always shown.
//! - **Over the output**: the terminal keeps its full width and the minimap is laid over its
//!   right edge only while wanted. A thin handle in the terminal's right padding shows where you
//!   are in the Scrollback; resting the pointer on it (or clicking it) opens the minimap, which
//!   stays open while the pointer is on it and fades away a moment after it leaves.
//!
//! When the Scrollback is taller than the strip, the strip scrolls with the terminal, the same
//! way VS Code's minimap does: its position follows the viewport proportionally.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use alacritty_terminal::grid::{Dimensions, Scroll};
use alacritty_terminal::index::{Column, Line};
use alacritty_terminal::sync::FairMutex;
use alacritty_terminal::term::cell::Flags;
use alacritty_terminal::term::color::{self, Colors};
use alacritty_terminal::term::{Term, TermMode};
use alacritty_terminal::vte::ansi::{Color, Rgb};
use gpui::{
    AnyElement, Bounds, Context, Hsla, MouseButton, MouseDownEvent, MouseUpEvent, Pixels, Stateful, canvas, div,
    fill, point, prelude::*, px, relative, size,
};

use crate::highlight::{self, Highlighter};
use crate::search::{self, Matches, Snapshot};
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
/// Maximum width of one character; wide terminal rows shrink to fit the strip.
const CHAR_W: f32 = 1.;
const PAD_X: f32 = 8.;
/// How long an open overlay waits after the pointer leaves it before it fades, so a pointer
/// that slips off for a moment (or lets go of the slider just past its edge) doesn't lose it.
pub const HIDE_DELAY: Duration = Duration::from_millis(700);
const FADE_IN: Duration = Duration::from_millis(120);
const FADE_OUT: Duration = Duration::from_millis(200);
/// Gap between the overlay handle and the terminal's edge, and its width at rest and when hot.
const HANDLE_INSET: f32 = 3.;
const HANDLE_W: f32 = 3.;
const HANDLE_HOT_W: f32 = 5.;
const HANDLE_MIN_H: f32 = 24.;

/// A timer the overlay asks its view to start.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Timer {
    /// Open after the hover delay, unless the pointer left the handle in the meantime.
    Show(u64),
    /// Fade after `HIDE_DELAY`, unless the pointer came back.
    Hide(u64),
}

/// Whether the overlay is open, and its fade. Timers carry a generation: a timer from an
/// earlier visit (the pointer left and came back, or started selecting text) does nothing.
#[derive(Default)]
struct Reveal {
    open: bool,
    /// Opacity when the current fade started, and when that was. `None`: not fading.
    fade: Option<(f32, Instant)>,
    /// Fades are skipped when Windows asks for reduced motion.
    still: bool,
    pending: Option<u64>,
    leaving: Option<u64>,
    generation: u64,
}

impl Reveal {
    fn opacity(&self, now: Instant) -> f32 {
        let target = if self.open { 1. } else { 0. };
        let Some((from, at)) = self.fade.filter(|_| !self.still) else {
            return target;
        };
        let duration = if self.open { FADE_IN } else { FADE_OUT };
        let t = (now.saturating_duration_since(at).as_secs_f32() / duration.as_secs_f32()).clamp(0., 1.);
        let eased = 1. - (1. - t).powi(3);
        from + (target - from) * eased
    }

    fn animating(&self, now: Instant) -> bool {
        let target = if self.open { 1. } else { 0. };
        !self.still && self.fade.is_some() && self.opacity(now) != target
    }

    /// On screen: open, or still fading out.
    fn shown(&self, now: Instant) -> bool {
        self.open || self.opacity(now) > 0.
    }

    fn set_open(&mut self, open: bool, now: Instant) {
        if self.open != open {
            self.fade = Some((self.opacity(now), now));
            self.open = open;
        }
    }

    fn next(&mut self) -> u64 {
        self.generation += 1;
        self.generation
    }

    /// The pointer moved. `at_edge`: over the handle at the terminal's right edge; `over_map`:
    /// over the minimap's footprint; `blocked`: something else has the pointer (a selection, a
    /// question, an inactive window).
    fn track(&mut self, at_edge: bool, over_map: bool, blocked: bool, now: Instant) -> Option<Timer> {
        if blocked {
            self.close(now);
            return None;
        }
        let shown = self.shown(now);
        if at_edge || (shown && over_map) {
            self.leaving = None;
            if shown {
                // Coming back while it fades out brings it straight back.
                self.pending = None;
                self.set_open(true, now);
            } else if self.pending.is_none() {
                let generation = self.next();
                self.pending = Some(generation);
                return Some(Timer::Show(generation));
            }
            return None;
        }
        self.pending = None;
        if self.open && self.leaving.is_none() {
            let generation = self.next();
            self.leaving = Some(generation);
            return Some(Timer::Hide(generation));
        }
        None
    }

    fn show(&mut self, generation: u64, now: Instant) -> bool {
        if self.pending != Some(generation) {
            return false;
        }
        self.pending = None;
        self.set_open(true, now);
        true
    }

    fn leave(&mut self, generation: u64, now: Instant) -> bool {
        if self.leaving != Some(generation) {
            return false;
        }
        self.leaving = None;
        self.set_open(false, now);
        true
    }

    fn open_now(&mut self, now: Instant) {
        self.pending = None;
        self.leaving = None;
        self.set_open(true, now);
    }

    /// Fade out and forget any timers.
    fn close(&mut self, now: Instant) {
        self.pending = None;
        self.leaving = None;
        self.set_open(false, now);
    }

    /// Gone at once, no fade.
    fn hide(&mut self) {
        let still = self.still;
        *self = Reveal { generation: self.generation, still, ..Reveal::default() };
    }
}

/// The overlay's handle is the terminal's right padding (never output) and, for the pane at the
/// window's edge, the window gutter beyond it. Once open, the pointer may move left onto the map.
/// Returns (over the handle, over the minimap's footprint).
pub fn hover_region(
    position: gpui::Point<Pixels>,
    terminal: Bounds<Pixels>,
    window_width: Pixels,
    padding: f32,
) -> (bool, bool) {
    let in_rows = position.y >= terminal.top() && position.y < terminal.bottom();
    let in_window = position.x >= px(0.) && position.x < window_width;
    // The gutter between the island and the window edge belongs to the rightmost pane.
    let right = if window_width - terminal.right() <= px(24.) { window_width } else { terminal.right() };
    let at_edge = in_rows && in_window && position.x >= terminal.right() - px(padding) && position.x < right;
    let over_map = in_rows
        && in_window
        && position.x < right
        && position.x >= terminal.right() - px(width_for(terminal.size.width.into()));
    (at_edge, over_map)
}

/// What the last paint showed, so a click can be turned into a line.
#[derive(Clone, Copy, Default, Debug)]
struct Geometry {
    top: f32,
    /// First Scrollback line (counted from the oldest) shown at the top of the strip.
    start: usize,
    history: usize,
    screen: usize,
    /// Lines with output, and lines the strip has room for.
    total: usize,
    capacity: usize,
    /// The slider's top and bottom, when there is something to scroll.
    slider: Option<(f32, f32)>,
}

/// A slider being dragged: where the pointer grabbed it and the first line on screen then.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Drag {
    y: f32,
    top: f32,
}

impl Geometry {
    fn line_at(&self, y: f32) -> f32 {
        self.start as f32 + ((y - self.top) / LINE_H).max(0.)
    }

    /// How far the slider travels while the view goes from the oldest line to the newest.
    fn travel(&self) -> f32 {
        let lines = if self.total <= self.capacity {
            self.history
        } else {
            (self.capacity + self.history).saturating_sub(self.total)
        };
        (lines as f32 * LINE_H).max(LINE_H)
    }

    /// A press at `y`. On the slider it is picked up where it is, as in VS Code; elsewhere the
    /// view first jumps so the line under the pointer is in the middle of the screen.
    /// Returns the drag and the first line to show, if it changes.
    fn grab(&self, y: f32, viewport_top: usize) -> (Drag, Option<usize>) {
        if self.slider.is_some_and(|(top, bottom)| y >= top && y < bottom.max(top + LINE_H)) {
            return (Drag { y, top: viewport_top as f32 }, None);
        }
        let top = (self.line_at(y) - self.screen as f32 / 2.).round().clamp(0., self.history as f32);
        (Drag { y, top }, Some(top as usize))
    }

    /// The first line to show with the slider dragged to `y`: it stays under the pointer.
    fn dragged(&self, drag: Drag, y: f32) -> usize {
        let top = drag.top + (y - drag.y) * self.history as f32 / self.travel();
        top.round().clamp(0., self.history as f32) as usize
    }
}

/// One run of same-coloured characters on a minimap line, in columns.
#[derive(Clone, Copy)]
struct Run {
    from: u16,
    to: u16,
    color: Hsla,
    attention: u8,
}

/// Runs of recently drawn lines, keyed by a hash of the line's characters and colours. Highlight
/// Rules are regexes; running them on every visible line at every repaint is what made the
/// minimap slow on long logs. With the cache a line is only worked out again when it changes.
#[derive(Default)]
struct LineCache {
    lines: HashMap<u64, Rc<[Run]>>,
    /// The generation before `lines`: kept for one more round so lines still on the minimap
    /// aren't worked out again just because `lines` filled up. Moved back to `lines` when used.
    old: HashMap<u64, Rc<[Run]>>,
    /// What the runs depend on besides the line itself: the Highlight Rules, the Terminal
    /// Colors and the colours programs set (OSC 4). When it changes, everything is worked out
    /// again: a line's hash has its colours as the terminal stores them, not as they show.
    /// The `Arc` itself rather than its address: a freed address can be reused by new rules.
    key: Option<(Arc<Highlighter>, TerminalPalette, Colors)>,
}

/// Lines kept in a cache generation before it becomes the old one; a few screens' worth.
const CACHE_LINES: usize = 8192;

impl LineCache {
    fn get(&mut self, hash: u64) -> Option<Rc<[Run]>> {
        if let Some(runs) = self.lines.get(&hash) {
            return Some(runs.clone());
        }
        let runs = self.old.remove(&hash)?;
        self.insert(hash, runs.clone());
        Some(runs)
    }

    fn insert(&mut self, hash: u64, runs: Rc<[Run]>) {
        if self.lines.len() >= CACHE_LINES {
            self.old = std::mem::take(&mut self.lines);
        }
        self.lines.insert(hash, runs);
    }
}

/// A minimap line as read from the terminal: its runs, or its cells to work them out from
/// once the terminal is unlocked.
enum Pending {
    Ready(Rc<[Run]>),
    New { hash: u64, cells: std::ops::Range<usize> },
}

/// What `collect` reads under the terminal lock, for `finish` to turn into rectangles after it.
struct Collected {
    geometry: Geometry,
    char_w: f32,
    lines: Vec<Pending>,
    /// The cells of the new lines, one after another.
    cells: Vec<(usize, char, Rgb)>,
    slider: Option<Bounds<Pixels>>,
}

/// What the strip shows, kept from paint to paint until something it shows changes.
struct Strip {
    geometry: Geometry,
    rects: Quads,
    slider: Option<Bounds<Pixels>>,
}

/// Everything the strip is worked out from. The cells, the cursor and the colours programs set
/// change only with output, which `MinimapState::output` counts; the rest is read at each paint.
struct StripKey {
    output: u64,
    bounds: Bounds<Pixels>,
    palette: TerminalPalette,
    highlighter: Arc<Highlighter>,
    history: usize,
    offset: usize,
    screen: usize,
    columns: usize,
}

impl PartialEq for StripKey {
    fn eq(&self, other: &Self) -> bool {
        self.output == other.output
            && self.bounds == other.bounds
            && self.palette == other.palette
            && Arc::ptr_eq(&self.highlighter, &other.highlighter)
            && (self.history, self.offset, self.screen, self.columns)
                == (other.history, other.offset, other.screen, other.columns)
    }
}

#[derive(Default)]
pub struct MinimapState {
    geometry: Cell<Geometry>,
    drag: Cell<Option<Drag>>,
    /// The pointer is over the minimap: the slider brightens.
    hovered: Cell<bool>,
    /// The terminal's bounds at the last paint, for the overlay's hover region.
    terminal: Cell<Option<Bounds<Pixels>>>,
    cache: RefCell<LineCache>,
    /// Batches of output the terminal has taken (see `output`).
    output: Cell<u64>,
    /// The strip at the last paint, and what it was worked out from. Repaints that change
    /// nothing on it (the cursor blinking, a selection, the pointer moving) reuse it rather
    /// than read every line on it again under the terminal lock.
    strip: RefCell<Option<(StripKey, Rc<Strip>)>>,
    reveal: RefCell<Reveal>,
    /// The plain scrollbar shown instead while the minimap is off (see `scrollbar`).
    bar: ScrollbarState,
}

/// The plain scrollbar's track at the last paint, and its thumb while it is held.
#[derive(Default)]
struct ScrollbarState {
    track: Cell<Option<Bounds<Pixels>>>,
    /// Where the thumb was picked up, in pixels below its top.
    grab: Cell<Option<f32>>,
    hovered: Cell<bool>,
}

impl MinimapState {
    /// Output reached the terminal (a Wakeup): any line on the strip may have changed.
    pub fn output(&self) {
        self.output.set(self.output.get().wrapping_add(1));
    }

    pub fn dragging(&self) -> bool {
        self.drag.get().is_some()
    }

    pub fn release(&self) {
        self.drag.set(None);
    }

    pub fn set_terminal(&self, bounds: Bounds<Pixels>) {
        self.terminal.set(Some(bounds));
    }

    pub fn terminal(&self) -> Option<Bounds<Pixels>> {
        self.terminal.get()
    }

    /// (open, about to open): what the overlay shows besides its fade.
    pub fn look(&self) -> (bool, bool) {
        let reveal = self.reveal.borrow();
        (reveal.open, reveal.pending.is_some())
    }

    pub fn opacity(&self, now: Instant) -> f32 {
        self.reveal.borrow().opacity(now)
    }

    pub fn animating(&self, now: Instant) -> bool {
        self.reveal.borrow().animating(now)
    }

    pub fn set_still(&self, still: bool) {
        self.reveal.borrow_mut().still = still;
    }

    pub fn track(&self, at_edge: bool, over_map: bool, blocked: bool, now: Instant) -> Option<Timer> {
        self.reveal.borrow_mut().track(at_edge, over_map, blocked, now)
    }

    pub fn show(&self, generation: u64, now: Instant) -> bool {
        self.reveal.borrow_mut().show(generation, now)
    }

    pub fn leave(&self, generation: u64, now: Instant) -> bool {
        self.reveal.borrow_mut().leave(generation, now)
    }

    pub fn open_now(&self, now: Instant) {
        self.reveal.borrow_mut().open_now(now);
    }

    pub fn close(&self, now: Instant) {
        self.reveal.borrow_mut().close(now);
    }

    /// The overlay goes at once (the setting changed, or it isn't the overlay any more).
    pub fn hide(&self) {
        self.reveal.borrow_mut().hide();
    }

    pub fn cancel_pending(&self) {
        self.reveal.borrow_mut().pending = None;
    }

    /// The scrollbar's thumb is held.
    pub fn bar_dragging(&self) -> bool {
        self.bar.grab.get().is_some()
    }

    pub fn bar_release(&self) {
        self.bar.grab.set(None);
    }
}

fn scroll_to<T: alacritty_terminal::event::EventListener>(term: &mut Term<T>, top: usize) {
    let history = term.grid().history_size();
    let target = (history - top.min(history)) as i32;
    let offset = term.grid().display_offset() as i32;
    if target != offset {
        term.scroll_display(Scroll::Delta(target - offset));
    }
}

/// A press on the minimap at `y`: pick up the slider, or jump there and pick it up.
pub fn press(term: &SharedTerm, state: &MinimapState, y: Pixels) {
    let g = state.geometry.get();
    if g.history == 0 {
        return;
    }
    let mut term = term.lock();
    let grid = term.grid();
    let viewport_top = grid.history_size() - grid.display_offset();
    let (drag, jump) = g.grab(f32::from(y), viewport_top);
    if let Some(top) = jump {
        scroll_to(&mut *term, top);
    }
    state.drag.set(Some(drag));
}

/// The pointer moved with the slider held.
pub fn drag(term: &SharedTerm, state: &MinimapState, y: Pixels) {
    if let Some(drag) = state.drag.get() {
        let top = state.geometry.get().dragged(drag, f32::from(y));
        scroll_to(&mut *term.lock(), top);
    }
}

/// The minimap's width for a terminal `width` pixels wide: a tenth, within limits. The layout
/// does the same with `relative(0.1)`, so the overlay's footprint matches what is drawn.
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

/// Rectangles to fill, and their colours.
type Quads = Vec<(Bounds<Pixels>, Hsla)>;

/// Use the same horizontal scale for output and search matches, including long rows.
fn char_width(bounds: Bounds<Pixels>, columns: usize) -> f32 {
    ((f32::from(bounds.size.width) - PAD_X * 2.).max(0.) / columns.max(1) as f32).min(CHAR_W)
}

/// An open search's matches, for the minimap to mark (see `ui::terminal_view::search`).
#[derive(Clone)]
pub struct SearchMarks {
    pub snapshot: Arc<Snapshot>,
    pub matches: Arc<Matches>,
    /// Rows that dropped off the top of a full Scrollback since the copy was made: the copy's
    /// row `r` is minimap line `r - moved`.
    pub moved: usize,
    /// The match stepped to, an index into `matches.ranges`.
    pub current: Option<usize>,
}

/// Width of the tick beside a line with a match, in the strip's right padding.
const MATCH_TICK_W: f32 = 4.;

/// Marks for the search matches on the lines the strip shows: the matched characters in the
/// accent colour, a tick in the right padding so the lines stand out, and a band across the
/// line of the match stepped to. A line whose text changed since it was searched gets none.
fn match_quads<T>(term: &Term<T>, geometry: &Geometry, bounds: Bounds<Pixels>, marks: &SearchMarks, app: &AppPalette) -> Quads {
    let grid = term.grid();
    let history = grid.history_size();
    let char_w = char_width(bounds, grid.columns());
    let (start, end) = (geometry.start, geometry.total.min(geometry.start + geometry.capacity));
    let SearchMarks { snapshot, matches, moved, current } = marks;
    let first = matches.starts.partition_point(|&row| row < start + moved);
    let mut same: Vec<Option<bool>> = vec![None; end.saturating_sub(start)];
    let (mut quads, mut ticked): (Quads, Option<usize>) = (Vec::new(), None);
    let (left, right) = (f32::from(bounds.origin.x), f32::from(bounds.right()));
    for (i, range) in matches.ranges.iter().enumerate().skip(first) {
        if matches.starts[i] >= end + moved {
            break;
        }
        let is_current = *current == Some(i);
        for (row, bytes) in snapshot.spans(range.clone()) {
            let Some(abs) = row.checked_sub(*moved).filter(|abs| (start..end).contains(abs)) else { continue };
            let line = Line(abs as i32 - history as i32);
            if !*same[abs - start].get_or_insert_with(|| search::row_equals(&grid[line], snapshot.row_text(row))) {
                continue;
            }
            let y = f32::from(bounds.origin.y) + (abs - start) as f32 * LINE_H;
            let quad = |x: f32, w: f32, color: Hsla| (Bounds::new(point(px(x), px(y)), size(px(w), px(LINE_H))), color);
            if is_current {
                quads.push(quad(left, right - left, Hsla { a: 0.22, ..app.brand }));
            }
            if ticked != Some(abs) || is_current {
                quads.push(quad(right - PAD_X / 2. - MATCH_TICK_W / 2., MATCH_TICK_W, app.brand));
                ticked = Some(abs);
            }
            if let Some(cols) = search::columns(&grid[line], bytes) {
                let x = left + PAD_X + cols.start as f32 * char_w;
                let width = ((cols.end - cols.start) as f32 * char_w).max(2.).min((right - PAD_X - x).max(0.));
                quads.push(quad(x, width, app.brand));
            }
        }
    }
    quads
}

/// Ticks along the overlay's handle for every match, where each is in the whole Scrollback.
/// Matches a pixel apart share one tick; the current match's is drawn last, on top.
fn handle_ticks(bounds: Bounds<Pixels>, marks: &SearchMarks, lines: usize, app: &AppPalette) -> Quads {
    let (top, height) = (f32::from(bounds.origin.y), f32::from(bounds.size.height));
    let x = f32::from(bounds.right()) - HANDLE_INSET - HANDLE_HOT_W;
    let total = lines.max(1) as f32;
    let y_of = |row: usize| row.checked_sub(marks.moved).map(|abs| top + (abs as f32 / total).min(1.) * (height - 2.));
    let tick = |y: f32, h: f32, color: Hsla| (Bounds::new(point(px(x), px(y)), size(px(HANDLE_HOT_W), px(h))), color);
    let mut quads = Quads::new();
    let mut last = None;
    for &row in &marks.matches.starts {
        let Some(y) = y_of(row) else { continue };
        if last != Some(y.floor()) {
            last = Some(y.floor());
            quads.push(tick(y, 2., Hsla { a: 0.75, ..app.brand }));
        }
    }
    if let Some(y) = marks.current.and_then(|i| marks.matches.starts.get(i)).and_then(|&row| y_of(row)) {
        quads.push(tick(y - 0.5, 3., app.brand));
    }
    quads
}

/// What the minimap shows for `bounds`: where it starts in the Scrollback, one rectangle per
/// run of characters, and the slider over the lines on screen (none while there is nothing to
/// scroll, as in VS Code).
#[cfg(test)]
fn layout<T>(
    term: &Term<T>,
    cache: &mut LineCache,
    palette: &TerminalPalette,
    highlighter: &Arc<Highlighter>,
    bounds: Bounds<Pixels>,
) -> (Geometry, Quads, Option<Bounds<Pixels>>) {
    let collected = collect(term, cache, palette, highlighter, bounds);
    finish(collected, cache, palette, highlighter, bounds)
}

/// The part of `layout` that reads the terminal, kept short because the parser waits for the
/// lock meanwhile: lines already in the cache are looked up, new ones only copied. Running the
/// Highlight Rules on them (`finish`) waits until the lock is released; during a flood nearly
/// every line on the minimap is new at every repaint.
fn collect<T>(
    term: &Term<T>,
    cache: &mut LineCache,
    palette: &TerminalPalette,
    highlighter: &Arc<Highlighter>,
    bounds: Bounds<Pixels>,
) -> Collected {
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
    let mut geometry = Geometry { top: f32::from(bounds.origin.y), start, history, screen, total, capacity, slider: None };

    let colors = term.colors();
    let same = cache.key.as_ref().is_some_and(|(h, p, c)| {
        Arc::ptr_eq(h, highlighter) && p == palette && (0..color::COUNT).all(|i| c[i] == colors[i])
    });
    if !same {
        cache.lines.clear();
        cache.old.clear();
        cache.key = Some((highlighter.clone(), palette.clone(), *colors));
    }

    // Match the whole row: forwarded logs can put INFO near the start and an error much
    // later. Keep column positions in the cache and compress them only when drawing.
    let char_w = char_width(bounds, cols);
    let end = total.min(start + capacity);
    let mut lines = Vec::with_capacity(end.saturating_sub(start));
    let mut cells: Vec<(usize, char, Rgb)> = Vec::new();
    for abs in start..end {
        let row = &grid[Line(abs as i32 - history as i32)];
        // Identify the line by its raw cells: characters, colours as the terminal stores them
        // and the flags that change how they show. Cheap enough for every line at every
        // repaint; the colours are only resolved and the rules only run when it's new.
        let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
        for col in 0..cols {
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
        match cache.get(hash) {
            Some(runs) => lines.push(Pending::Ready(runs)),
            None => {
                let from = cells.len();
                for col in 0..cols {
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
                lines.push(Pending::New { hash, cells: from..cells.len() });
            }
        }
    }

    // The slider over the lines that are on screen, cut off where the output ends.
    let slider = (history > 0).then(|| {
        let box_top = (viewport_top as f32 - start as f32) * LINE_H;
        let box_bottom = ((viewport_top + screen).min(total) as f32 - start as f32) * LINE_H;
        let box_bottom = box_bottom.min(f32::from(bounds.size.height)).max(box_top);
        geometry.slider = Some((geometry.top + box_top, geometry.top + box_bottom));
        Bounds::new(
            point(bounds.origin.x, bounds.origin.y + px(box_top)),
            size(bounds.size.width, px(box_bottom - box_top)),
        )
    });
    Collected { geometry, char_w, lines, cells, slider }
}

/// The strip for `bounds`: the last one while nothing it shows has changed, or else worked
/// out again (`collect` under the terminal lock, `finish` after it). `marks` gives the search
/// marks' quads under the same lock.
fn layout_strip<T>(
    term: &FairMutex<Term<T>>,
    state: &MinimapState,
    palette: &TerminalPalette,
    highlighter: &Arc<Highlighter>,
    bounds: Bounds<Pixels>,
    marks: impl FnOnce(&Term<T>, &Geometry) -> Quads,
) -> (Rc<Strip>, Quads) {
    let term = term.lock();
    let grid = term.grid();
    let key = StripKey {
        output: state.output.get(),
        bounds,
        palette: palette.clone(),
        highlighter: highlighter.clone(),
        history: grid.history_size(),
        offset: grid.display_offset(),
        screen: grid.screen_lines(),
        columns: grid.columns(),
    };
    let kept = state.strip.borrow().as_ref().filter(|(shown, _)| *shown == key).map(|(_, strip)| strip.clone());
    if let Some(strip) = kept {
        let found = marks(&term, &strip.geometry);
        return (strip, found);
    }
    let mut cache = state.cache.borrow_mut();
    let collected = crate::perf::time("minimap layout", || collect(&term, &mut cache, palette, highlighter, bounds));
    let found = marks(&term, &collected.geometry);
    drop(term);
    let (geometry, rects, slider) =
        crate::perf::time("minimap runs", || finish(collected, &mut cache, palette, highlighter, bounds));
    let strip = Rc::new(Strip { geometry, rects, slider });
    *state.strip.borrow_mut() = Some((key, strip.clone()));
    (strip, found)
}

/// The rest of `layout`, with the terminal unlocked: the runs of new lines, then a rectangle
/// per run.
fn finish(
    collected: Collected,
    cache: &mut LineCache,
    palette: &TerminalPalette,
    highlighter: &Highlighter,
    bounds: Bounds<Pixels>,
) -> (Geometry, Quads, Option<Bounds<Pixels>>) {
    let Collected { geometry, char_w, lines, cells, slider } = collected;
    let mut rects: Quads = Vec::new();
    for (i, line) in lines.into_iter().enumerate() {
        let runs = match line {
            Pending::Ready(runs) => runs,
            Pending::New { hash, cells: range } => {
                let runs = line_runs(&cells[range], palette, highlighter);
                cache.insert(hash, runs.clone());
                runs
            }
        };
        let y = f32::from(bounds.origin.y) + i as f32 * LINE_H;
        // Paint the faint text first, then emphasize matches at their scaled columns.
        // Leave a gap between rows even in a wall of errors so other levels remain distinct.
        // Red matches go last so nearby enlarged highlights cannot cover them.
        for attention in 0..=2 {
            let emphasized = attention > 0;
            for run in runs.iter().filter(|run| run.attention == attention) {
                let x = f32::from(bounds.origin.x) + PAD_X + run.from as f32 * char_w;
                let natural_width = (run.to - run.from) as f32 * char_w;
                let width = if emphasized {
                    natural_width.max(if run.attention == 2 { 8. } else { 6. })
                } else {
                    natural_width
                }.min((f32::from(bounds.right()) - PAD_X - x).max(0.));
                let height = if emphasized { LINE_H - 0.25 } else { LINE_H - 0.5 };
                rects.push((
                    Bounds::new(point(px(x), px(y)), size(px(width), px(height))),
                    if emphasized { Hsla { a: 1., ..run.color } } else { run.color },
                ));
            }
        }
    }
    (geometry, rects, slider)
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
    let mut run: Option<(usize, usize, Rgb, u8)> = None;
    let flush = |run: &mut Option<(usize, usize, Rgb, u8)>, runs: &mut Vec<Run>| {
        if let Some((from, to, color, attention)) = run.take() {
            let mut c = rgb_to_hsla(color);
            // A theme's ordinary foreground can be tinted without being a highlight.
            c.a *= if color == palette.foreground || c.s < 0.2 { plain_alpha } else { 0.95 };
            runs.push(Run { from: from as u16, to: to as u16, color: c, attention });
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
        // Emphasize actual colored rule matches in place. Information gets the same
        // thickness as failures; red matches get only a little extra width.
        let attention = rule.and_then(|r| r.fg).map_or(0, |c| {
            let (r, g, b) = (u16::from(c.r), u16::from(c.g), u16::from(c.b));
            if r > g * 16 / 10 && r > b * 13 / 10 { 2 }
            else if r.max(g).max(b) - r.min(g).min(b) > r.max(g).max(b) / 4 { 1 }
            else { 0 }
        });
        let color = match rule.and_then(|r| r.fg) {
            Some(c) => {
                let c = highlight::readable_on(c, palette.dark);
                Rgb { r: c.r, g: c.g, b: c.b }
            }
            None => fg,
        };
        match &mut run {
            Some((_, to, c, a)) if *c == color && *a == attention && *to == col => *to = col + 1,
            _ => {
                flush(&mut run, &mut runs);
                run = Some((col, col + 1, color, attention));
            }
        }
    }
    flush(&mut run, &mut runs);
    runs.into()
}

/// The slider's colour, like VS Code's: faint at rest, stronger under the pointer and while held.
fn slider_color(app: &AppPalette, hovered: bool, dragging: bool) -> Hsla {
    let a = if dragging {
        0.2
    } else if hovered {
        0.13
    } else {
        0.08
    };
    Hsla { a: a * if app.dark { 1. } else { 0.85 }, ..app.text }
}

/// The lines in miniature and the slider.
fn strip(
    term: SharedTerm,
    state: Rc<MinimapState>,
    palette: TerminalPalette,
    app: AppPalette,
    highlighter: Arc<Highlighter>,
    marks: Option<SearchMarks>,
) -> impl IntoElement {
    let paint_state = state.clone();
    canvas(
        move |bounds, _window, _cx| {
            let (strip, found) = layout_strip(&term, &state, &palette, &highlighter, bounds, |term, geometry| {
                marks.map(|marks| match_quads(term, geometry, bounds, &marks, &app)).unwrap_or_default()
            });
            state.geometry.set(strip.geometry);
            (strip, found)
        },
        move |_bounds, (strip, found), window, _cx| {
            if let Some(slider) = strip.slider {
                // Float inside the strip like the other scroll thumbs. Keep the original
                // geometry for dragging, including when a long log makes the thumb tiny.
                let thumb = slider.inset(px(2.).min(slider.size.height / 4.));
                let radius = px(6.).min(thumb.size.height / 2.);
                window.paint_quad(
                    fill(thumb, slider_color(&app, paint_state.hovered.get(), paint_state.dragging()))
                        .corner_radii(radius),
                );
            }
            for &(bounds, color) in strip.rects.iter().chain(&found) {
                window.paint_quad(fill(bounds, color));
            }
        },
    )
    .size_full()
}

/// Press, drag and hover on the minimap. Presses here scroll; they never start a text selection.
fn controls(el: Stateful<gpui::Div>, state: Rc<MinimapState>, cx: &mut Context<TerminalView>) -> Stateful<gpui::Div> {
    el.cursor_default()
        .on_hover(cx.listener(move |_, hovered: &bool, _, cx| {
            if state.hovered.replace(*hovered) != *hovered {
                cx.notify();
            }
        }))
        .on_mouse_down(
            MouseButton::Left,
            cx.listener(|this, event: &MouseDownEvent, window, cx| {
                if this.minimap_pressed(event.position.y, window, cx) {
                    cx.stop_propagation();
                }
            }),
        )
        // A right-click opens the terminal's menu, led by Hide minimap (see `context_menu`).
        .on_mouse_down(MouseButton::Right, cx.listener(|this, _: &MouseDownEvent, _, _| this.minimap_right_press = true))
        .on_mouse_up(
            MouseButton::Left,
            cx.listener(|this, _: &MouseUpEvent, window, cx| {
                if this.minimap_released(window, cx) {
                    cx.stop_propagation();
                }
            }),
        )
        .on_mouse_up_out(
            MouseButton::Left,
            cx.listener(|this, _: &MouseUpEvent, window, cx| {
                if this.minimap_released(window, cx) {
                    cx.stop_propagation();
                }
            }),
        )
}

fn sized(el: Stateful<gpui::Div>) -> Stateful<gpui::Div> {
    el.w(relative(0.1)).min_w(px(MIN_WIDTH)).max_w(px(MAX_WIDTH))
}

/// The minimap in a column of its own beside the output: always there, so output never runs
/// underneath it. Full-screen programs (vim, htop) get an empty column; it stays reserved so
/// their width doesn't change when they start or quit.
pub fn beside(
    term: SharedTerm,
    state: Rc<MinimapState>,
    palette: TerminalPalette,
    app: AppPalette,
    highlighter: Arc<Highlighter>,
    marks: Option<SearchMarks>,
    cx: &mut Context<TerminalView>,
) -> AnyElement {
    let column = sized(div().id("minimap")).flex_none().h_full();
    let lines = {
        let term = term.lock();
        (!term.mode().contains(TermMode::ALT_SCREEN)).then(|| content_lines(&term))
    };
    let Some(lines) = lines else {
        state.hovered.set(false);
        return column.into_any_element();
    };
    controls(column, state.clone(), cx)
        .child(
            div()
                .w_full()
                .h(px(lines as f32 * LINE_H + PAD_Y * 2.))
                .max_h_full()
                .py(px(PAD_Y))
                .child(strip(term, state, palette, app, highlighter, marks)),
        )
        .into_any_element()
}

/// The minimap laid over the output's right edge while it is open; otherwise the handle in the
/// terminal's right padding (`padding`: its width and the inset at the top and bottom). Nothing
/// while there is no Scrollback: everything is on screen and there is nothing to scroll.
pub fn over(
    term: SharedTerm,
    state: Rc<MinimapState>,
    palette: TerminalPalette,
    app: AppPalette,
    highlighter: Arc<Highlighter>,
    padding: (f32, f32),
    marks: Option<SearchMarks>,
    cx: &mut Context<TerminalView>,
) -> Option<AnyElement> {
    let (lines, viewport_top, screen) = {
        let term = term.lock();
        let grid = term.grid();
        if term.mode().contains(TermMode::ALT_SCREEN) || grid.history_size() == 0 {
            return None;
        }
        (content_lines(&term), grid.history_size() - grid.display_offset(), grid.screen_lines())
    };
    let opacity = state.opacity(Instant::now());
    if opacity <= 0. {
        state.hovered.set(false);
        return Some(handle(state, &app, lines, viewport_top, screen, padding, marks, cx));
    }
    let card = sized(div().id("minimap"))
        .absolute()
        .top_0()
        // Slides in from the edge as it fades in.
        .right(px((opacity - 1.) * 8.))
        .h(px(lines as f32 * LINE_H + PAD_Y * 2.))
        .max_h_full()
        .py(px(PAD_Y))
        // Opaque: the output underneath would make both unreadable.
        .bg(crate::theme::rgb_to_hsla(palette.background))
        .border_l_1()
        .border_b_1()
        .border_color(app.border)
        .rounded_bl(app.corner(10.))
        .rounded_tr(app.corner(crate::ui::widgets::ISLAND_RADIUS - 1.))
        .shadow(crate::ui::widgets::elevation(app))
        .opacity(opacity);
    Some(controls(card, state.clone(), cx).child(strip(term, state, palette, app, highlighter, marks)).into_any_element())
}

/// A thin scroll thumb in the terminal's right padding: where you are in the Scrollback, and
/// where the minimap opens. It turns the accent colour and widens while the minimap is about
/// to open; a click opens it at once.
fn handle(
    state: Rc<MinimapState>,
    app: &AppPalette,
    lines: usize,
    viewport_top: usize,
    screen: usize,
    (width, inset): (f32, f32),
    marks: Option<SearchMarks>,
    cx: &mut Context<TerminalView>,
) -> AnyElement {
    let ticks = marks.map(|marks| tick_marks(marks, lines, *app));
    let (_, hot) = state.look();
    let total = lines.max(1) as f32;
    let top = viewport_top as f32 / total;
    let height = (screen as f32 / total).min(1.);
    let thumb = div()
        .absolute()
        .right(px(HANDLE_INSET))
        .w(px(if hot { HANDLE_HOT_W } else { HANDLE_W }))
        .h(relative(height))
        .min_h(px(HANDLE_MIN_H))
        .rounded_full()
        .bg(if hot { app.brand } else { Hsla { a: if app.dark { 0.16 } else { 0.2 }, ..app.text } });
    // At the bottom it is pinned there, so its minimum height doesn't push it past the end.
    let thumb = if top + height >= 0.999 { thumb.bottom_0() } else { thumb.top(relative(top)) };
    div()
        .id("minimap-handle")
        .absolute()
        .top(px(inset))
        .bottom(px(inset))
        .right_0()
        .w(px(width))
        .cursor_default()
        .on_mouse_down(
            MouseButton::Left,
            cx.listener(|this, _: &MouseDownEvent, window, cx| {
                if this.minimap_open_now(window, cx) {
                    cx.stop_propagation();
                }
            }),
        )
        .on_mouse_down(MouseButton::Right, cx.listener(|this, _: &MouseDownEvent, _, _| this.minimap_right_press = true))
        .child(thumb)
        .children(ticks)
        .into_any_element()
}

/// The search matches' ticks (see `handle_ticks`) over the handle or the scrollbar. Laid over
/// the thumb, so matches on screen stay visible.
fn tick_marks(marks: SearchMarks, lines: usize, app: AppPalette) -> impl IntoElement {
    canvas(
        move |bounds, _, _| handle_ticks(bounds, &marks, lines, &app),
        |_, quads, window, _| {
            for (bounds, color) in quads {
                window.paint_quad(fill(bounds, color));
            }
        },
    )
    .absolute()
    .top_0()
    .left_0()
    .size_full()
}

/// Narrowest the scrollbar's thumb gets, so a long Scrollback still leaves something to grab.
const BAR_MIN_H: f32 = 24.;
/// The thumb's width at rest, and while the pointer is on the scrollbar or holds it.
const BAR_W: f32 = 6.;
const BAR_HOT_W: f32 = 8.;
/// Room between the thumb and the terminal's right edge.
const BAR_INSET: f32 = 3.;

/// The thumb's top and height in a track `track` pixels tall, for a view starting at line
/// `viewport_top` of `history` lines of Scrollback with `screen` lines on screen.
fn bar_thumb(history: usize, viewport_top: usize, screen: usize, track: f32) -> (f32, f32) {
    let total = (history + screen).max(1) as f32;
    let height = (screen as f32 / total * track).max(BAR_MIN_H).min(track);
    let top = if history == 0 { 0. } else { viewport_top.min(history) as f32 / history as f32 * (track - height) };
    (top, height)
}

/// The first line of the view for the thumb's top at `top` pixels down the track.
fn bar_line(history: usize, screen: usize, track: f32, top: f32) -> usize {
    let (_, height) = bar_thumb(history, 0, screen, track);
    let room = track - height;
    if room <= 0. {
        return history;
    }
    ((top / room).clamp(0., 1.) * history as f32).round() as usize
}

/// A press on the scrollbar at `y`: pick up the thumb, or centre it there and pick it up.
pub fn bar_press(term: &SharedTerm, state: &MinimapState, y: Pixels) {
    let Some(track) = state.bar.track.get() else { return };
    let mut term = term.lock();
    let grid = term.grid();
    let (history, screen) = (grid.history_size(), grid.screen_lines());
    if history == 0 {
        return;
    }
    let length = f32::from(track.size.height);
    let (top, height) = bar_thumb(history, history - grid.display_offset(), screen, length);
    let at = f32::from(y - track.origin.y);
    let grab = if (top..top + height).contains(&at) {
        at - top
    } else {
        scroll_to(&mut *term, bar_line(history, screen, length, at - height / 2.));
        height / 2.
    };
    state.bar.grab.set(Some(grab));
}

/// The pointer moved with the scrollbar's thumb held.
pub fn bar_drag(term: &SharedTerm, state: &MinimapState, y: Pixels) {
    let (Some(track), Some(grab)) = (state.bar.track.get(), state.bar.grab.get()) else { return };
    let mut term = term.lock();
    let grid = term.grid();
    let (history, screen) = (grid.history_size(), grid.screen_lines());
    let line = bar_line(history, screen, f32::from(track.size.height), f32::from(y - track.origin.y) - grab);
    scroll_to(&mut *term, line);
}

/// A plain scrollbar in the terminal's right padding, for while the minimap is off: a thumb
/// to drag, a track to click, and search matches marked beside it. Nothing while there is
/// nothing to scroll (no Scrollback, or a full-screen program).
pub fn scrollbar(
    term: SharedTerm,
    state: Rc<MinimapState>,
    app: AppPalette,
    (width, inset): (f32, f32),
    marks: Option<SearchMarks>,
    cx: &mut Context<TerminalView>,
) -> Option<AnyElement> {
    let (history, viewport_top, screen, lines) = {
        let term = term.lock();
        let grid = term.grid();
        if term.mode().contains(TermMode::ALT_SCREEN) || grid.history_size() == 0 {
            state.bar.hovered.set(false);
            return None;
        }
        (grid.history_size(), grid.history_size() - grid.display_offset(), grid.screen_lines(), content_lines(&term))
    };
    let hot = state.bar.hovered.get() || state.bar_dragging();
    let color = Hsla { a: if state.bar_dragging() { 0.42 } else if hot { 0.3 } else if app.dark { 0.18 } else { 0.22 }, ..app.text };
    let paint_state = state.clone();
    let track = canvas(
        move |bounds, _, _| {
            paint_state.bar.track.set(Some(bounds));
            let (top, height) = bar_thumb(history, viewport_top, screen, f32::from(bounds.size.height));
            let w = if hot { BAR_HOT_W } else { BAR_W };
            Bounds::new(point(bounds.right() - px(BAR_INSET + w), bounds.origin.y + px(top)), size(px(w), px(height)))
        },
        move |_, thumb, window, _| window.paint_quad(fill(thumb, color).corner_radii(thumb.size.width / 2.)),
    )
    .size_full();
    let hover_state = state.clone();
    Some(
        div()
            .id("scrollbar")
            .absolute()
            .top(px(inset))
            .bottom(px(inset))
            .right_0()
            .w(px(width))
            .cursor_default()
            .on_hover(cx.listener(move |_, hovered: &bool, _, cx| {
                if hover_state.bar.hovered.replace(*hovered) != *hovered {
                    cx.notify();
                }
            }))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, event: &MouseDownEvent, window, cx| {
                    if this.scrollbar_pressed(event.position.y, window, cx) {
                        cx.stop_propagation();
                    }
                }),
            )
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, _: &MouseUpEvent, _, cx| {
                    if this.scrollbar_released(cx) {
                        cx.stop_propagation();
                    }
                }),
            )
            .on_mouse_up_out(
                MouseButton::Left,
                cx.listener(|this, _: &MouseUpEvent, _, cx| {
                    if this.scrollbar_released(cx) {
                        cx.stop_propagation();
                    }
                }),
            )
            .child(track)
            .children(marks.map(|marks| tick_marks(marks, lines, app)))
            .into_any_element(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scrollbar_thumb_spans_the_track_and_maps_back() {
        // 950 lines of Scrollback and 50 on screen in a 500 px track: a 25 px thumb.
        assert_eq!(bar_thumb(950, 950, 50, 500.), (475., 25.));
        assert_eq!(bar_thumb(950, 0, 50, 500.), (0., 25.));
        assert_eq!(bar_line(950, 50, 500., 475.), 950);
        assert_eq!(bar_line(950, 50, 500., 0.), 0);
        assert_eq!(bar_line(950, 50, 500., -40.), 0);
        assert_eq!(bar_line(950, 50, 500., 9999.), 950);
        // A short Scrollback: the thumb is most of the track.
        assert_eq!(bar_thumb(10, 10, 40, 400.), (80., 320.));
    }
    use alacritty_terminal::event::VoidListener;
    use alacritty_terminal::term::Config;
    use alacritty_terminal::term::test::TermSize;
    use alacritty_terminal::vte::ansi::Processor;

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

    #[test]
    fn ordinary_text_stays_faint_in_every_theme() {
        let rules = bundled_rules();
        for theme in crate::theme::DARK_THEMES.iter().chain(crate::theme::LIGHT_THEMES.iter()) {
            let palette = TerminalPalette::from_theme(theme);
            let cells: Vec<_> = "plain ERROR WARN"
                .chars()
                .enumerate()
                .map(|(col, ch)| (col, ch, palette.foreground))
                .collect();
            let runs = line_runs(&cells, &palette, &rules);
            assert_eq!(runs.len(), 3);
            assert!(runs[0].color.a <= 0.3, "{}: ordinary text competes with highlights", theme.name);
            assert!(runs[1].color.a >= 0.9, "{}: error should stand out", theme.name);
            assert!(runs[2].color.a >= 0.9, "{}: warning should stand out", theme.name);
        }
    }

    #[test]
    fn severity_highlights_stay_vivid_in_the_minimap() {
        let rules = bundled_rules();
        for palette in [TerminalPalette::dark(), TerminalPalette::light()] {
            for word in ["ERROR", "FATAL", "PANIC", "CRITICAL", "WARN", "WARNING"] {
                let cells: Vec<_> = word
                    .chars()
                    .enumerate()
                    .map(|(col, ch)| (col, ch, Rgb { r: 180, g: 180, b: 180 }))
                    .collect();
                let runs = line_runs(&cells, &palette, &rules);
                assert!(!runs.is_empty());
                for run in runs.iter() {
                    let color: gpui::Rgba = run.color.into();
                    let max = color.r.max(color.g).max(color.b);
                    let min = color.r.min(color.g).min(color.b);
                    assert!((max - min) / max >= 0.65, "{word} is too pale on dark={}", palette.dark);
                    assert!(color.a >= 0.9, "{word} should stand out from plain output");
                }
            }
        }
    }

    fn strip() -> Bounds<Pixels> {
        Bounds::new(point(px(0.), px(0.)), size(px(MAX_WIDTH), px(800.)))
    }

    #[test]
    fn errors_after_info_remain_visible_across_the_full_line() {
        for palette in [TerminalPalette::dark(), TerminalPalette::light()] {
            let prefixes = ["INFO;".to_string(), format!("INFO;{}", "x".repeat(120)), format!("INFO;{}", "x".repeat(220))];
            for prefix in prefixes {
                let mut term = Term::new(Config::default(), &TermSize::new(240, 24), VoidListener);
                let mut parser: Processor = Processor::new();
                parser.advance(&mut term, format!("{prefix} [error]\r\n").as_bytes());
                for width in [MIN_WIDTH, MAX_WIDTH] {
                    let bounds = Bounds::new(strip().origin, size(px(width), strip().size.height));
                    let (geometry, quads, _) = layout(&term, &mut LineCache::default(), &palette, &bundled_rules(), bounds);
                    let emphasized: Vec<_> = quads.iter().filter(|(quad, c)| c.a == 1. && quad.size.width > px(0.)).collect();
                    let colors: Vec<gpui::Rgba> = emphasized.iter().map(|(_, c)| (*c).into()).collect();
                    assert!(colors.iter().any(|c| c.b > c.r), "INFO should remain visible");
                    assert!(colors.iter().any(|c| c.r > c.b), "errors after INFO disappeared (prefix length {})", prefix.len());
                    assert!(emphasized.iter().all(|(quad, _)| {
                        quad.origin.x >= bounds.origin.x + px(PAD_X) && quad.right() <= bounds.right() - px(PAD_X)
                    }));
                    let marks = search_marks(&term, "error", None);
                    let found = match_quads(&term, &geometry, bounds, &marks, &AppPalette::dark());
                    let error = emphasized.iter().zip(&colors).find(|(_, c)| c.r > c.b).unwrap().0;
                    assert!(found.iter().any(|(quad, _)| quad.origin == error.0.origin), "search must align with the error");
                }
            }
        }
    }

    #[test]
    fn compressed_info_cannot_paint_over_a_nearby_error() {
        let mut term = Term::new(Config::default(), &TermSize::new(240, 24), VoidListener);
        let mut parser: Processor = Processor::new();
        parser.advance(&mut term, b"ERROR INFO\r\n");
        let (_, quads, _) = layout(&term, &mut LineCache::default(), &TerminalPalette::dark(), &bundled_rules(), strip());
        let last = quads.iter().filter(|(_, color)| color.a == 1.).last().unwrap();
        let color: gpui::Rgba = last.1.into();
        assert!(color.r > color.b, "red should be on top where enlarged highlights overlap");
    }

    #[test]
    fn changes_near_the_end_of_a_row_invalidate_cached_highlights() {
        let mut term = Term::new(Config::default(), &TermSize::new(240, 24), VoidListener);
        let mut parser: Processor = Processor::new();
        let palette = TerminalPalette::dark();
        let rules = bundled_rules();
        let mut cache = LineCache::default();
        parser.advance(&mut term, format!("INFO;{} [info]", "x".repeat(120)).as_bytes());
        let (_, before, _) = layout(&term, &mut cache, &palette, &rules, strip());
        parser.advance(&mut term, b"\x1b[1;127H[error]");
        let (_, after, _) = layout(&term, &mut cache, &palette, &rules, strip());
        let (_, fresh, _) = layout(&term, &mut LineCache::default(), &palette, &rules, strip());
        assert_ne!(before, after, "a late error must update the minimap");
        assert_eq!(after, fresh);
    }

    /// The strip for a repaint, checked against working it out afresh. Returns it, and whether
    /// it is the one from the last repaint.
    fn repaint(
        term: &FairMutex<Term<VoidListener>>,
        state: &MinimapState,
        palette: &TerminalPalette,
        rules: &Arc<Highlighter>,
        bounds: Bounds<Pixels>,
    ) -> bool {
        let last = state.strip.borrow().as_ref().map(|(_, strip)| strip.clone());
        let (strip, _) = layout_strip(term, state, palette, rules, bounds, |_, _| Quads::new());
        let (geometry, rects, slider) = layout(&term.lock(), &mut LineCache::default(), palette, rules, bounds);
        assert_eq!(format!("{:?}", strip.geometry), format!("{geometry:?}"));
        assert!(strip.rects == rects, "the strip is not what the terminal shows now");
        assert_eq!(strip.slider, slider);
        last.is_some_and(|last| Rc::ptr_eq(&last, &strip))
    }

    #[test]
    fn the_strip_is_kept_until_something_it_shows_changes() {
        let term = FairMutex::new(busy_log(2_000));
        let state = MinimapState::default();
        let mut palette = TerminalPalette::dark();
        let mut rules = bundled_rules();
        let mut bounds = strip();
        let mut parser: Processor = Processor::new();
        assert!(!repaint(&term, &state, &palette, &rules, bounds), "first paint");
        assert!(repaint(&term, &state, &palette, &rules, bounds), "nothing changed");

        let mut output = |text: &str| {
            parser.advance(&mut *term.lock(), text.as_bytes());
            state.output();
        };
        output("\x1b[31mERROR\x1b[0m new line\r\n");
        assert!(!repaint(&term, &state, &palette, &rules, bounds), "output");
        assert!(repaint(&term, &state, &palette, &rules, bounds), "nothing changed since");
        output("\x1b[3;1Hredrawn on screen");
        assert!(!repaint(&term, &state, &palette, &rules, bounds), "a change on screen");
        // A program changes a colour lines already on the strip are drawn with.
        output("\x1b]4;1;rgb:12/34/56\x07");
        assert!(!repaint(&term, &state, &palette, &rules, bounds), "a colour set by a program");

        term.lock().scroll_display(Scroll::Delta(300));
        assert!(!repaint(&term, &state, &palette, &rules, bounds), "scrolled up");
        term.lock().scroll_display(Scroll::Bottom);
        assert!(!repaint(&term, &state, &palette, &rules, bounds), "back at the bottom");
        crate::session::resize(&mut *term.lock(), crate::session::GridSize { cols: 120, rows: 40, cell_width: 8, cell_height: 18 });
        assert!(!repaint(&term, &state, &palette, &rules, bounds), "resized");
        term.lock().grid_mut().clear_history();
        assert!(!repaint(&term, &state, &palette, &rules, bounds), "Scrollback cleared");

        bounds.size.height = px(500.);
        assert!(!repaint(&term, &state, &palette, &rules, bounds), "shorter strip");
        // Another dark theme: the lines kept in the cache are coloured by the old one.
        palette.ansi[1] = Rgb { r: 200, g: 10, b: 60 };
        palette.foreground = Rgb { r: 90, g: 200, b: 90 };
        assert!(!repaint(&term, &state, &palette, &rules, bounds), "other Terminal Colors");
        rules = Arc::new(Highlighter::default());
        assert!(!repaint(&term, &state, &palette, &rules, bounds), "no Highlight Rules");
        rules = bundled_rules();
        assert!(!repaint(&term, &state, &palette, &rules, bounds), "Highlight Rules back");
        assert!(repaint(&term, &state, &palette, &rules, bounds), "nothing changed");
    }

    /// Compare the existing per-line cache with reuse of the whole strip on idle repaints.
    /// `cargo test --lib bench_idle_minimap -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn bench_idle_minimap() {
        let term = FairMutex::new(busy_log(2_000));
        let state = MinimapState::default();
        let palette = TerminalPalette::dark();
        let rules = bundled_rules();
        let mut cache = LineCache::default();
        for reuse in [false, true] {
            let start = Instant::now();
            for _ in 0..500 {
                if reuse {
                    std::hint::black_box(layout_strip(&term, &state, &palette, &rules, strip(), |_, _| Quads::new()));
                } else {
                    std::hint::black_box(layout(&term.lock(), &mut cache, &palette, &rules, strip()));
                }
            }
            eprintln!("idle minimap, reuse={reuse}: {:?} per repaint", start.elapsed() / 500);
        }
    }

    #[test]
    fn highlights_are_emphasized_in_place_and_stay_inside_their_rows() {
        for palette in [TerminalPalette::dark(), TerminalPalette::light()] {
            let mut term = Term::new(Config::default(), &TermSize::new(100, 24), VoidListener);
            let mut parser: Processor = Processor::new();
            let mut text = String::new();
            for row in 0..20 {
                text.push_str(if row == 10 { "prefix INFO detail\r\n" } else { "prefix ERROR detail\r\n" });
            }
            parser.advance(&mut term, text.as_bytes());
            let (_, quads, _) = layout(&term, &mut LineCache::default(), &palette, &bundled_rules(), strip());
            let highlights: Vec<_> = quads.iter().filter(|(_, c)| c.a == 1.).collect();
            assert_eq!(highlights.len(), 20);
            for (row, (bounds, _)) in highlights.iter().enumerate() {
                assert_eq!(bounds.origin.x, px(PAD_X + 7. * char_width(strip(), 100)), "keep the matched column");
                assert_eq!(bounds.origin.y, px(row as f32 * LINE_H));
                assert!(bounds.size.height < px(LINE_H), "dense errors must not cover adjacent rows");
                assert_eq!(bounds.size.width, px(if row == 10 { 6. } else { 8. }));
            }
            assert_ne!(highlights[10].1, highlights[9].1, "info stays distinct between errors");
            let (_, disabled, _) = layout(&term, &mut LineCache::default(), &palette, &Arc::new(Highlighter::default()), strip());
            assert!(disabled.iter().all(|(_, c)| c.a < 1.), "disabled rules must not leave emphasis");
        }
    }

    #[test]
    fn the_handle_is_the_right_padding_and_the_window_gutter() {
        // The terminal ends 8px before the window edge and has 12px of padding on the right.
        let terminal = Bounds::new(point(px(200.), px(40.)), size(px(792.), px(600.)));
        let region = |x, y| hover_region(point(px(x), px(y)), terminal, px(1000.), 12.);
        assert_eq!(region(900., 100.), (false, false)); // Output.
        assert_eq!(region(920., 100.), (false, true)); // Output under the minimap's footprint.
        assert_eq!(region(979., 100.), (false, true)); // The last output column.
        assert_eq!(region(981., 100.), (true, true)); // The terminal's padding.
        assert_eq!(region(995., 100.), (true, true)); // The window gutter.
        assert_eq!(region(999., 20.), (false, false)); // Title bar.
        assert_eq!(region(999., 650.), (false, false)); // Below the terminal.
        assert_eq!(region(1001., 100.), (false, false)); // Outside the window.

        // A pane on the left of a split: its handle stops at its own edge.
        let left = Bounds::new(point(px(200.), px(40.)), size(px(400.), px(600.)));
        let region = |x| hover_region(point(px(x), px(100.)), left, px(1000.), 12.);
        assert_eq!(region(595.), (true, true));
        assert_eq!(region(605.), (false, false));
    }

    #[test]
    fn it_opens_after_the_delay_and_stays_while_the_pointer_is_on_it() {
        let t0 = Instant::now();
        let mut reveal = Reveal::default();
        assert_eq!(reveal.track(false, true, false, t0), None); // The footprint alone opens nothing.
        let Some(Timer::Show(timer)) = reveal.track(true, true, false, t0) else { panic!() };
        assert!(!reveal.open);
        assert_eq!(reveal.track(true, true, false, t0), None); // Motion doesn't restart the wait.
        assert!(reveal.show(timer, t0));
        assert!(reveal.open);
        assert_eq!(reveal.opacity(t0), 0.);
        assert_eq!(reveal.opacity(t0 + FADE_IN), 1.);
        // Onto the map and around on it: it stays, however long.
        for _ in 0..100 {
            assert_eq!(reveal.track(false, true, false, t0), None);
        }
        assert!(reveal.open);
    }

    #[test]
    fn leaving_waits_and_coming_back_keeps_it() {
        let t0 = Instant::now();
        let mut reveal = Reveal::default();
        reveal.open_now(t0);
        let t1 = t0 + FADE_IN; // Fully faded in.
        let Some(Timer::Hide(first)) = reveal.track(false, false, false, t1) else { panic!() };
        assert!(reveal.open); // Still open: it only fades once the timer runs out.
        assert_eq!(reveal.track(false, false, false, t1), None); // One timer per departure.
        reveal.track(false, true, false, t1); // Back on the map in time.
        assert!(!reveal.leave(first, t1));
        assert!(reveal.open);

        let Some(Timer::Hide(second)) = reveal.track(false, false, false, t1) else { panic!() };
        assert!(reveal.leave(second, t1));
        assert!(!reveal.open);
        let midway = t1 + FADE_OUT / 2;
        assert!(reveal.shown(midway)); // Fading out.
        assert!(reveal.animating(midway));
        // Back on it while it fades: straight back from where the fade got to, no new wait.
        let faded = reveal.opacity(midway);
        assert_eq!(reveal.track(false, true, false, midway), None);
        assert!(reveal.open);
        assert_eq!(reveal.opacity(midway), faded);
        // Gone once faded: the footprint no longer counts, only the handle.
        reveal.close(midway);
        let later = midway + FADE_OUT * 4;
        assert!(!reveal.shown(later));
        assert!(!reveal.animating(later));
        assert_eq!(reveal.track(false, true, false, later), None);
        assert!(!reveal.open);
    }

    #[test]
    fn leaving_the_handle_early_cancels_the_wait() {
        let t0 = Instant::now();
        let mut reveal = Reveal::default();
        let Some(Timer::Show(first)) = reveal.track(true, false, false, t0) else { panic!() };
        reveal.track(false, false, false, t0);
        let Some(Timer::Show(second)) = reveal.track(true, false, false, t0) else { panic!() };
        assert!(!reveal.show(first, t0)); // The first timer runs out during the second visit.
        assert!(!reveal.open);
        assert!(reveal.show(second, t0));
    }

    #[test]
    fn selecting_or_clicking_cancels_a_pending_reveal() {
        let t0 = Instant::now();
        let state = MinimapState::default();
        let Some(Timer::Show(timer)) = state.track(true, true, false, t0) else { panic!() };
        state.track(true, true, true, t0); // A selection reaches the window edge.
        assert!(!state.show(timer, t0));
        assert_eq!(state.track(true, true, true, t0), None);
        assert_eq!(state.look(), (false, false));

        let Some(Timer::Show(timer)) = state.track(true, true, false, t0) else { panic!() };
        assert_eq!(state.look(), (false, true)); // The handle lights up while it waits.
        state.cancel_pending(); // Mouse down with no pointer motion after it.
        assert!(!state.show(timer, t0));
        assert_eq!(state.look(), (false, false));
    }

    #[test]
    fn hiding_is_immediate_and_reduced_motion_skips_fades() {
        let t0 = Instant::now();
        let state = MinimapState::default();
        state.open_now(t0);
        state.hide();
        assert_eq!(state.opacity(t0), 0.);
        state.set_still(true);
        state.open_now(t0);
        assert_eq!(state.opacity(t0), 1.);
        assert!(!state.animating(t0));
        state.close(t0);
        assert_eq!(state.opacity(t0), 0.);
    }

    #[test]
    fn the_slider_stays_under_the_pointer_while_dragged() {
        let mut term = busy_log(5_000);
        let mut cache = LineCache::default();
        let (palette, rules) = (TerminalPalette::dark(), bundled_rules());
        term.scroll_display(Scroll::Delta(2_000));
        let (g, _, slider) = layout(&term, &mut cache, &palette, &rules, strip());
        let slider = slider.unwrap();
        let viewport_top = term.grid().history_size() - term.grid().display_offset();
        // Picked up in its middle: no jump.
        let grab_y = f32::from(slider.center().y);
        let (drag, jump) = g.grab(grab_y, viewport_top);
        assert_eq!(jump, None);
        assert_eq!(g.dragged(drag, grab_y), viewport_top);
        // Dragged 100px down: the slider is 100px further down too.
        let top = g.dragged(drag, grab_y + 100.);
        scroll_to(&mut term, top);
        let (_, _, moved) = layout(&term, &mut cache, &palette, &rules, strip());
        let moved_by = f32::from(moved.unwrap().top() - slider.top());
        assert!((moved_by - 100.).abs() <= LINE_H, "moved {moved_by}px");
        // Dragged far past either end: it stops at the ends.
        assert_eq!(g.dragged(drag, -10_000.), 0);
        assert_eq!(g.dragged(drag, 10_000.), g.history);
    }

    #[test]
    fn a_click_beside_the_slider_centres_that_line() {
        let mut term = busy_log(300);
        let (g, _, slider) =
            layout(&term, &mut LineCache::default(), &TerminalPalette::dark(), &bundled_rules(), strip());
        let slider = slider.unwrap();
        let viewport_top = term.grid().history_size() - term.grid().display_offset();
        // Well above the slider, which is at the bottom.
        let y = f32::from(slider.top()) - 200.;
        let (_, jump) = g.grab(y, viewport_top);
        let top = jump.unwrap();
        assert_eq!(top as f32, (g.line_at(y) - g.screen as f32 / 2.).round().max(0.));
        scroll_to(&mut term, top);
        assert_eq!(term.grid().history_size() - term.grid().display_offset(), top);
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
    fn the_slider_appears_once_there_is_something_to_scroll() {
        // 30 lines + the empty prompt row, of a 50-row screen: nothing to scroll, so no slider.
        let term = busy_log(30);
        let (_, rects, slider) =
            layout(&term, &mut LineCache::default(), &TerminalPalette::dark(), &bundled_rules(), strip());
        assert!(!rects.is_empty());
        assert!(slider.is_none());
        // Once output scrolls off, the slider covers the screen's lines.
        let term = busy_log(80);
        let (_, _, slider) =
            layout(&term, &mut LineCache::default(), &TerminalPalette::dark(), &bundled_rules(), strip());
        let on_screen = content_lines(&term) - term.grid().history_size();
        assert_eq!(f32::from(slider.unwrap().size.height), on_screen as f32 * LINE_H);
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

    fn search_marks(term: &Term<VoidListener>, text: &str, current: Option<usize>) -> SearchMarks {
        let snapshot = Arc::new(Snapshot::copy(term));
        let query = crate::search::Query { text: text.into(), ..Default::default() };
        let matches = Arc::new(Matches::find(&snapshot, &query.compile().unwrap().unwrap()));
        SearchMarks { snapshot, matches, moved: 0, current }
    }

    #[test]
    fn search_matches_are_marked_on_their_lines() {
        let term = busy_log(300);
        let app = AppPalette::dark();
        let (geometry, _, _) = layout(&term, &mut LineCache::default(), &TerminalPalette::dark(), &bundled_rules(), strip());
        assert_eq!(geometry.start, 0, "the whole log fits");
        let marks = search_marks(&term, "ERROR", Some(2));
        let quads = match_quads(&term, &geometry, strip(), &marks, &app);
        let tick_x = MAX_WIDTH - PAD_X / 2. - MATCH_TICK_W / 2.;
        let mut ticks: Vec<f32> = quads.iter().filter(|(b, _)| f32::from(b.origin.x) == tick_x).map(|(b, _)| f32::from(b.origin.y)).collect();
        ticks.dedup();
        // An ERROR every 37 lines.
        let expected: Vec<f32> = (0..300).step_by(37).map(|line| line as f32 * LINE_H).collect();
        assert_eq!(ticks, expected);
        // The characters themselves: "2026-11-11T09:00:00Z " comes first.
        let char_w = char_width(strip(), term.columns());
        assert!(quads.iter().any(|(b, _)| f32::from(b.origin.x) == PAD_X + 21. * char_w && f32::from(b.size.width) == 5. * char_w));
        // The current match's line has a band across the strip.
        let bands: Vec<f32> = quads.iter().filter(|(b, _)| f32::from(b.size.width) == MAX_WIDTH).map(|(b, _)| f32::from(b.origin.y)).collect();
        assert_eq!(bands, vec![74. * LINE_H]);
    }

    #[test]
    fn a_copy_that_no_longer_fits_the_lines_marks_nothing() {
        let term = busy_log(300);
        let (geometry, _, _) = layout(&term, &mut LineCache::default(), &TerminalPalette::dark(), &bundled_rules(), strip());
        // As if a line had dropped off the top since the copy: every line is checked and differs.
        let marks = SearchMarks { moved: 1, ..search_marks(&term, "ERROR", None) };
        assert!(match_quads(&term, &geometry, strip(), &marks, &AppPalette::dark()).is_empty());
    }

    #[test]
    fn the_handle_ticks_every_match_once_per_pixel() {
        let term = busy_log(3_000);
        let app = AppPalette::dark();
        let lines = content_lines(&term);
        let track = Bounds::new(point(px(0.), px(0.)), size(px(12.), px(100.)));
        // INFO is on most lines: they share the track's 100 pixels.
        let marks = search_marks(&term, "INFO", Some(0));
        let quads = handle_ticks(track, &marks, lines, &app);
        assert!(marks.matches.total > 2_500);
        assert!(quads.len() <= 101, "{} ticks", quads.len());
        // The current match's tick is drawn last, at the top.
        let (current, _) = quads.last().unwrap();
        assert_eq!(f32::from(current.size.height), 3.);
        assert!(f32::from(current.origin.y) < 1.);
        // Rare matches each get a tick of their own.
        let marks = search_marks(&term, "ERROR", None);
        assert_eq!(handle_ticks(track, &marks, lines, &app).len(), marks.matches.total);
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

    /// A flood: between repaints, 300 new lines arrive, so nearly every minimap line is new.
    /// Times the part of a repaint that holds the terminal lock (`collect`) and the rest
    /// (`finish`); before they were split, the lock was held for both. `KOMANDO_HIGHLIGHT_RULES`
    /// names another rules file. `cargo test --release --lib minimap_during_a_flood -- --ignored
    /// --nocapture` prints the numbers.
    #[test]
    #[ignore]
    fn minimap_during_a_flood() {
        let rules = match std::env::var("KOMANDO_HIGHLIGHT_RULES") {
            Ok(path) => {
                let sets = crate::highlight::parse(&std::fs::read_to_string(path).unwrap()).unwrap();
                Arc::new(Highlighter::from_sets(&sets, &[]))
            }
            Err(_) => bundled_rules(),
        };
        for (name, message) in [("ascii", "http GET /v1/orders"), ("non-ascii", "Größe prüfen ЗАПРОС 获取")] {
            let config = Config { scrolling_history: 10_000, ..Config::default() };
            let mut term = Term::new(config, &TermSize::new(177, 42), VoidListener);
            let mut parser: Processor = Processor::new();
            let palette = TerminalPalette::dark();
            let mut cache = LineCache::default();
            let (mut locked, mut unlocked) = (std::time::Duration::ZERO, std::time::Duration::ZERO);
            let frames = 60;
            let mut i = 0u64;
            for frame in 0..frames + 10 {
                let mut text = String::new();
                for _ in 0..300 {
                    i += 1;
                    let level = if i % 37 == 0 { "\x1b[31mERROR\x1b[0m" } else { "\x1b[32mINFO \x1b[0m" };
                    text.push_str(&format!(
                        "\x1b[2m2026-11-11T09:{:02}:{:02}Z\x1b[0m {level} {message} page={i} took {}ms trace={:x}\r\n",
                        i / 60 % 60,
                        i % 60,
                        5 + i % 40,
                        i * 2654435761 % 1_000_003
                    ));
                }
                parser.advance(&mut term, text.as_bytes());
                let t = Instant::now();
                let collected = collect(&term, &mut cache, &palette, &rules, strip());
                let t1 = Instant::now();
                std::hint::black_box(finish(collected, &mut cache, &palette, &rules, strip()));
                if frame >= 10 {
                    locked += t1 - t;
                    unlocked += t1.elapsed();
                }
            }
            println!(
                "{name}: {} rules, per repaint {:?} under the lock, {:?} after it",
                rules.rule_count(),
                locked / frames,
                unlocked / frames
            );
        }
    }
}
