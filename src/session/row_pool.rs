//! Rows for the Scrollback, built and cleared ahead on a thread of their own.
//!
//! Every Scrollback row is a full-width row of cells (177 columns are about 4 KB). While the
//! Scrollback fills, the terminal allocates its new rows a thousand at a time, on the thread
//! that parses output and with the terminal locked: the allocations and the fresh memory pages
//! they touch were most of what a filling Scrollback cost over a full one. A [`RowPool`] builds
//! those rows ahead on a worker thread, and the terminal takes them from it as it needs them
//! (see `RowSource` in the patched alacritty_terminal). Whatever the pool doesn't have ready,
//! the terminal builds itself as before, so the parser never waits for the pool.
//!
//! Once the Scrollback is full, each new line reuses its oldest row, which was last touched a
//! whole Scrollback ago and is in no CPU cache: clearing it made the parser wait for main memory
//! on every line. The terminal hands those rows to the pool instead (`RowSource::exchange`),
//! and takes blank rows the worker has cleared, a batch at a time. The worker keeps a few
//! hundred blank rows ready (about 1 MB at 177 columns), few enough to stay in the CPU's caches.
//!
//! The worker runs while output needs it: a request for rows (a growing Scrollback) or for
//! blank ones (a full one) starts it, and it gives every row back and stops after a second
//! without requests, or when the terminal goes away. An idle Session holds no rows here.

use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::time::Duration;

use alacritty_terminal::grid::{Row, RowSource};
use alacritty_terminal::term::cell::Cell;

/// The most memory the rows held ready may take: about 2,000 rows at 177 columns.
const BUDGET: usize = 8 << 20;
/// Bounds on the rows held ready whatever the width. The terminal asks for 1,000 at a time.
const MIN_ROWS: usize = 1_000;
const MAX_ROWS: usize = 2_048;
/// Rows the worker builds between looks at the pool, so the parser can take them early.
const BATCH: usize = 64;
/// How long the worker keeps ready rows without a request before it gives them back.
const IDLE: Duration = Duration::from_secs(1);
/// Blank rows held ready to swap in for rows recycled from a full Scrollback, and the most
/// handed over at a time: about 512 KB at 177 columns, enough to stay in the CPU's caches.
const RECYCLE_ROWS: usize = 256;
const RECYCLE_BATCH: usize = 64;

/// The most rows held ready for rows of `columns` cells.
fn bound(columns: usize) -> usize {
    (BUDGET / (columns.max(1) * size_of::<Cell>())).clamp(MIN_ROWS, MAX_ROWS)
}

/// Fresh Scrollback rows of the terminal's width, built ahead on a worker thread.
pub struct RowPool {
    shared: Arc<Shared>,
}

struct Shared {
    state: Mutex<State>,
    wake: Condvar,
    /// How long the worker keeps ready rows without a request.
    idle: Duration,
}

impl Default for RowPool {
    fn default() -> Self {
        Self::with_idle(IDLE)
    }
}

impl RowPool {
    fn with_idle(idle: Duration) -> Self {
        Self { shared: Arc::new(Shared { state: Mutex::default(), wake: Condvar::new(), idle }) }
    }
}

#[derive(Default)]
struct State {
    /// The width of the rows wanted. Every row in `ready` is this wide.
    columns: usize,
    ready: Vec<Row<Cell>>,
    /// How many rows to hold ready: none until the terminal asks for rows.
    target: usize,
    /// Whether the worker thread is running. Without it, `ready` is empty.
    worker: bool,
    /// Whether the terminal asked for rows since the worker last went to sleep.
    asked: bool,
    /// Rows recycled from the top of a full Scrollback, to clear, and blank rows to swap in
    /// for the next ones (see `RowSource::exchange`).
    dirty: Vec<Row<Cell>>,
    clean: Vec<Row<Cell>>,
    /// Whether rows are being recycled: the worker keeps `clean` stocked.
    recycling: bool,
    /// Whether the worker is waiting to be woken.
    waiting: bool,
    /// Set when the pool is dropped: the worker gives everything back and stops.
    closed: bool,
}

impl Shared {
    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

impl std::fmt::Debug for RowPool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let state = self.shared.lock();
        f.debug_struct("RowPool").field("columns", &state.columns).field("ready", &state.ready.len()).finish()
    }
}

impl RowSource<Cell> for RowPool {
    fn take_rows(&self, columns: usize, count: usize, rows_left: usize, rows: &mut Vec<Row<Cell>>) {
        // Rows of another width, and rows the Scrollback will never need, dropped once unlocked.
        let mut unwanted = Vec::new();
        rows.reserve(count);
        let mut state = self.shared.lock();
        if state.columns != columns {
            state.columns = columns;
            unwanted = std::mem::take(&mut state.ready);
        }
        let taken = count.min(state.ready.len());
        let from = state.ready.len() - taken;
        rows.extend(state.ready.drain(from..));
        let limit = rows_left.min(bound(columns));
        if state.ready.len() > limit {
            unwanted.extend(state.ready.drain(limit..));
        }
        // Only a request for rows means the Scrollback is growing; a report only lowers the
        // number held ready.
        if count > 0 {
            state.target = limit;
            state.asked = true;
        } else {
            state.target = state.target.min(limit);
        }
        let start = !state.worker && state.ready.len() < state.target;
        if start {
            state.worker = true;
        }
        drop(state);
        self.shared.wake.notify_one();
        if start {
            let shared = self.shared.clone();
            let spawned = std::thread::Builder::new().name("scrollback rows".into()).spawn(move || work(&shared));
            if spawned.is_err() {
                // No thread: the terminal goes on building its rows itself.
                let mut state = self.shared.lock();
                state.worker = false;
                state.target = 0;
            }
        }
        drop(unwanted);
    }

    fn exchange(&self, columns: usize, used: &mut Vec<Row<Cell>>, fresh: &mut Vec<Row<Cell>>) {
        let mut unwanted = Vec::new();
        let mut state = self.shared.lock();
        if state.columns != columns {
            state.columns = columns;
            unwanted = std::mem::take(&mut state.ready);
            unwanted.append(&mut state.clean);
        }
        state.dirty.append(used);
        let taken = RECYCLE_BATCH.min(state.clean.len());
        let from = state.clean.len() - taken;
        fresh.extend(state.clean.drain(from..));
        state.recycling = true;
        state.asked = true;
        let start = !state.worker;
        if start {
            state.worker = true;
        }
        // Woken once half the blank rows are gone rather than on every exchange: waking it
        // costs a system call on both sides.
        let wake = state.waiting && state.clean.len() <= RECYCLE_ROWS / 2;
        drop(state);
        if wake {
            self.shared.wake.notify_one();
        }
        if start {
            let shared = self.shared.clone();
            let spawned = std::thread::Builder::new().name("scrollback rows".into()).spawn(move || work(&shared));
            if spawned.is_err() {
                let mut state = self.shared.lock();
                state.worker = false;
                state.recycling = false;
                unwanted.append(&mut state.dirty);
            }
        }
        drop(unwanted);
    }
}

impl Drop for RowPool {
    /// The pool lives as long as its terminal's grid, so this is also when a closed Session's
    /// Scrollback is freed: the allocator is asked to hand that memory back.
    fn drop(&mut self) {
        self.shared.lock().closed = true;
        self.shared.wake.notify_one();
        crate::memory::release_soon();
    }
}

/// The worker: build rows until the pool holds its target, then wait for the next request.
/// Gives every row back and stops when nothing more is wanted.
fn work(shared: &Shared) {
    let mut state = shared.lock();
    loop {
        if state.closed || (state.target == 0 && !state.recycling) {
            break;
        }
        // Recycled rows first: the terminal is waiting to swap them out on every new line.
        if state.recycling && (!state.dirty.is_empty() || state.clean.len() < RECYCLE_ROWS) {
            let columns = state.columns;
            let take = state.dirty.len().min(BATCH);
            let from = state.dirty.len() - take;
            let mut rows: Vec<Row<Cell>> = state.dirty.drain(from..).collect();
            let room = RECYCLE_ROWS.saturating_sub(state.clean.len());
            drop(state);
            let template = Cell::default();
            // Rows of another width (from before a resize) are dropped; new ones fill in.
            rows.retain(|row| row.len() == columns);
            for row in &mut rows {
                row.reset(&template);
            }
            while rows.len() < room.min(BATCH) {
                rows.push(Row::new(columns));
            }
            state = shared.lock();
            if state.columns == columns && !state.closed {
                let room = RECYCLE_ROWS.saturating_sub(state.clean.len());
                let extra = rows.split_off(room.min(rows.len()));
                state.clean.append(&mut rows);
                if !extra.is_empty() {
                    drop(state);
                    drop(extra);
                    state = shared.lock();
                }
            } else {
                drop(state);
                drop(rows);
                state = shared.lock();
            }
            continue;
        }
        let missing = state.target.saturating_sub(state.ready.len());
        if missing == 0 {
            state.asked = false;
            state.waiting = true;
            let (next, wait) = shared.wake.wait_timeout(state, shared.idle).unwrap_or_else(|poisoned| poisoned.into_inner());
            state = next;
            state.waiting = false;
            if wait.timed_out() && !state.asked {
                break;
            }
            continue;
        }
        let columns = state.columns;
        drop(state);
        let mut batch: Vec<Row<Cell>> = (0..missing.min(BATCH)).map(|_| Row::new(columns)).collect();
        state = shared.lock();
        if state.columns == columns && !state.closed {
            let room = state.target.saturating_sub(state.ready.len());
            let extra = batch.split_off(room.min(batch.len()));
            state.ready.append(&mut batch);
            if !extra.is_empty() {
                drop(state);
                drop(extra);
                state = shared.lock();
            }
        } else {
            drop(state);
            drop(batch);
            state = shared.lock();
        }
    }
    let mut rows = std::mem::take(&mut state.ready);
    rows.append(&mut state.clean);
    rows.append(&mut state.dirty);
    state.worker = false;
    state.target = 0;
    state.recycling = false;
    drop(state);
    drop(rows);
}

#[cfg(test)]
impl RowPool {
    /// A view of the pool that outlives it, for tests: (rows ready, worker running).
    pub(crate) fn watch(&self) -> impl Fn() -> (usize, bool) + use<> {
        let shared = self.shared.clone();
        move || {
            let state = shared.lock();
            (state.ready.len(), state.worker)
        }
    }

    /// The rows held for recycling (blank and to clear), for tests.
    pub(crate) fn watch_recycling(&self) -> impl Fn() -> usize + use<> {
        let shared = self.shared.clone();
        move || {
            let state = shared.lock();
            state.clean.len() + state.dirty.len()
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    use alacritty_terminal::grid::{Dimensions, Row, RowSource};
    use alacritty_terminal::index::{Column, Line};
    use alacritty_terminal::term::Config;
    use alacritty_terminal::term::cell::Cell;
    use alacritty_terminal::vte::ansi::Processor;
    use futures::channel::mpsc;

    use super::RowPool;
    use crate::session::{EventProxy, GridSize, SharedTerm, compact_rows, new_term, resize};

    fn wait_for(what: &str, mut done: impl FnMut() -> bool) {
        let start = Instant::now();
        while !done() {
            assert!(start.elapsed() < Duration::from_secs(10), "timed out waiting for {what}");
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    /// A terminal taking its rows from `source`, and one building them all itself.
    fn pair(size: GridSize, scrollback: usize, source: Arc<dyn RowSource<Cell>>) -> [SharedTerm; 2] {
        let ours = new_term(size, scrollback, EventProxy::new(mpsc::unbounded().0));
        ours.lock().grid_mut().set_row_source(Some(source));
        let plain = new_term(size, scrollback, EventProxy::new(mpsc::unbounded().0));
        plain.lock().grid_mut().set_row_source(None);
        [ours, plain]
    }

    /// `count` lines of coloured output, some wider than the screen, some with wide characters.
    fn output(seed: &mut u64, count: usize) -> Vec<u8> {
        let mut out = Vec::new();
        for _ in 0..count {
            *seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            let r = *seed >> 33;
            let len = (r % 140) as usize;
            out.extend_from_slice(format!("\x1b[3{}m{:06} ", r % 8, r % 1_000_000).as_bytes());
            for i in 0..len {
                match (r as usize + i) % 23 {
                    0 => out.extend_from_slice("漢".as_bytes()),
                    1 => out.extend_from_slice(b"\x1b[0m"),
                    2 => out.extend_from_slice(b"\x1b[44m"),
                    n => out.push(b'a' + (n as u8 % 26)),
                }
            }
            out.extend_from_slice(b"\x1b[0m\r\n");
        }
        out
    }

    fn feed(terms: &[SharedTerm; 2], parsers: &mut [Processor; 2], bytes: &[u8]) {
        for (term, parser) in terms.iter().zip(parsers.iter_mut()) {
            for chunk in bytes.chunks(4096) {
                parser.advance(&mut *term.lock(), chunk);
            }
        }
    }

    fn assert_same(terms: &[SharedTerm; 2], when: &str) {
        let (ours, plain) = (terms[0].lock(), terms[1].lock());
        let (a, b) = (ours.grid(), plain.grid());
        assert_eq!(a.history_size(), b.history_size(), "{when}: history");
        assert_eq!(
            (a.columns(), a.screen_lines(), a.display_offset()),
            (b.columns(), b.screen_lines(), b.display_offset()),
            "{when}"
        );
        assert_eq!(a.cursor.point, b.cursor.point, "{when}: cursor");
        for line in -(a.history_size() as i32)..a.screen_lines() as i32 {
            let (x, y) = (&a[Line(line)], &b[Line(line)]);
            assert_eq!(x.len(), a.columns(), "{when}: width of line {line}");
            assert!(x == y, "{when}: line {line} differs");
        }
    }

    #[test]
    fn the_grid_is_the_same_with_the_pool() {
        let size = GridSize { cols: 80, rows: 24, cell_width: 9, cell_height: 17 };
        let pool = Arc::new(RowPool::with_idle(Duration::from_secs(600)));
        let watch = pool.watch();
        let recycling = pool.watch_recycling();
        let terms = pair(size, 20_000, pool);
        let mut parsers = [Processor::new(), Processor::new()];
        let mut seed = 7;
        let resize_both = |cols: u16| {
            for term in &terms {
                resize(&mut *term.lock(), GridSize { cols, ..size });
                compact_rows(term, cols as usize);
            }
        };

        // An idle terminal holds no rows ready, nor does one whose history doesn't grow.
        assert_eq!(watch(), (0, false));
        feed(&terms, &mut parsers, &output(&mut seed, 10));
        assert_eq!(watch(), (0, false));

        feed(&terms, &mut parsers, &output(&mut seed, 500));
        assert_same(&terms, "filling");
        wait_for("rows ready", || watch().0 > 0);
        feed(&terms, &mut parsers, &output(&mut seed, 1_500));
        assert_same(&terms, "filling further");

        // Resizes while the pool holds rows of the old width.
        wait_for("rows ready", || watch().0 > 0);
        resize_both(60);
        assert_same(&terms, "narrower");
        feed(&terms, &mut parsers, &output(&mut seed, 800));
        assert_same(&terms, "filling, narrower");
        wait_for("rows ready", || watch().0 > 0);
        resize_both(100);
        assert_same(&terms, "wider");
        feed(&terms, &mut parsers, &output(&mut seed, 300));
        assert_same(&terms, "filling, wider");

        // Full, and overflowing: the pool gives back the rows it held for a growing Scrollback,
        // and from then on swaps blank rows in for recycled ones.
        feed(&terms, &mut parsers, &output(&mut seed, 16_000));
        assert_same(&terms, "full");
        assert_eq!(terms[0].lock().grid().history_size(), 20_000);
        wait_for("the rows for a growing Scrollback to go once it is full", || watch().0 == 0);
        feed(&terms, &mut parsers, &output(&mut seed, 500));
        assert_same(&terms, "overflowing");
        wait_for("blank rows ready for recycling", || recycling() > 0);
        // Lines on a coloured background: new rows take the cursor's background, whether
        // cleared here or swapped in blank.
        feed(&terms, &mut parsers, b"\x1b[41m\r\n\r\nred\r\n\x1b[0m");
        feed(&terms, &mut parsers, &output(&mut seed, 3_000));
        assert_same(&terms, "overflowing, rows recycled");
        assert_eq!(watch().0, 0);
        assert!(recycling() <= 2 * super::RECYCLE_ROWS + super::RECYCLE_BATCH, "{} rows held for recycling", recycling());

        // Resizes while blank rows of the old width are held, by the pool and by the grid.
        wait_for("blank rows ready for recycling", || recycling() > 0);
        resize_both(70);
        assert_same(&terms, "recycling, narrower");
        feed(&terms, &mut parsers, &output(&mut seed, 1_000));
        assert_same(&terms, "recycling after a resize");
        resize_both(80);
        feed(&terms, &mut parsers, &output(&mut seed, 1_000));
        assert_same(&terms, "recycling after resizing back");

        // Cleared (ESC [ 3 J), it fills again.
        feed(&terms, &mut parsers, b"\x1b[3J");
        assert_same(&terms, "cleared");
        assert_eq!(terms[0].lock().grid().history_size(), 0);
        feed(&terms, &mut parsers, &output(&mut seed, 2_000));
        assert_same(&terms, "filling after clearing");

        // A smaller Scrollback, then a bigger one.
        wait_for("rows ready", || watch().0 > 0);
        let config = |lines| Config { scrolling_history: lines, ..Config::default() };
        for term in &terms {
            term.lock().set_options(config(1_200));
        }
        assert_same(&terms, "smaller Scrollback");
        wait_for("the rows for a growing Scrollback to go once the smaller one is full", || watch().0 == 0);
        feed(&terms, &mut parsers, &output(&mut seed, 700));
        assert_same(&terms, "smaller Scrollback, overflowing");
        for term in &terms {
            term.lock().set_options(config(30_000));
        }
        feed(&terms, &mut parsers, &output(&mut seed, 2_500));
        assert_same(&terms, "bigger Scrollback");
        wait_for("rows ready", || watch().0 > 0);

        // Reset (ESC c), then filling again.
        feed(&terms, &mut parsers, b"\x1bc");
        assert_same(&terms, "reset");
        feed(&terms, &mut parsers, &output(&mut seed, 25_000));
        assert_same(&terms, "filled after a reset");
        assert_eq!(terms[0].lock().grid().history_size(), 30_000);
        wait_for("the rows for a growing Scrollback to go once it is full", || watch().0 == 0);
    }

    #[test]
    fn the_pool_never_hands_out_rows_of_another_width() {
        let pool = RowPool::with_idle(Duration::from_secs(600));
        let watch = pool.watch();
        let mut rows = Vec::new();
        let mut handed_out = 0;
        for round in 0..200 {
            let columns = 40 + (round / 10) % 3;
            rows.clear();
            pool.take_rows(columns, 300, 100_000, &mut rows);
            assert!(rows.len() <= 300);
            handed_out += rows.len();
            for row in &rows {
                assert_eq!(row.len(), columns);
                assert!(*row == Row::new(columns), "rows are fresh");
            }
            if round % 10 == 0 {
                wait_for("rows ready", || watch().0 > 0);
            }
        }
        assert!(handed_out > 10_000, "only {handed_out} rows handed out");
        wait_for("rows ready", || watch().0 > 0);
        // After a change of width, none of the rows ready are of any use.
        rows.clear();
        pool.take_rows(90, 1_000, 100_000, &mut rows);
        assert!(rows.is_empty());
    }

    /// The grid checks what a source hands over, and builds rows itself rather than use any of
    /// another width or more than it asked for. Blank rows for recycled ones of another width
    /// are refused too, and the recycled rows cleared in place.
    #[test]
    fn the_grid_refuses_rows_of_another_width() {
        #[derive(Debug)]
        struct Wrong(usize, usize);
        impl RowSource<Cell> for Wrong {
            fn take_rows(&self, columns: usize, count: usize, _: usize, rows: &mut Vec<Row<Cell>>) {
                let mut row: Row<Cell> = Row::new(columns + self.0);
                row[Column(0)].c = 'x';
                rows.extend((0..count + self.1).map(|_| row.clone()));
            }

            fn exchange(&self, columns: usize, used: &mut Vec<Row<Cell>>, fresh: &mut Vec<Row<Cell>>) {
                fresh.extend((0..used.len().max(8) + self.1).map(|_| Row::new(columns + self.0)));
                used.clear();
            }
        }
        for wrong in [Wrong(1, 0), Wrong(0, 1)] {
            let size = GridSize { cols: 50, rows: 10, cell_width: 9, cell_height: 17 };
            let terms = pair(size, 5_000, Arc::new(wrong));
            let mut parsers = [Processor::new(), Processor::new()];
            let mut seed = 3;
            feed(&terms, &mut parsers, &output(&mut seed, 2_000));
            assert_same(&terms, "after rows of another width or too many");
            feed(&terms, &mut parsers, b"\x1b[3J");
            feed(&terms, &mut parsers, &output(&mut seed, 7_000));
            assert_same(&terms, "full, rows recycled");
        }
    }

    #[test]
    fn the_pool_stops_when_the_terminal_goes() {
        let size = GridSize { cols: 120, rows: 30, cell_width: 9, cell_height: 17 };
        let pool = Arc::new(RowPool::with_idle(Duration::from_secs(600)));
        let watch = pool.watch();
        let terms = pair(size, 50_000, pool);
        let mut parsers = [Processor::new(), Processor::new()];
        feed(&terms, &mut parsers, &output(&mut 1, 1_500));
        wait_for("rows ready", || watch().0 > 0);
        drop(terms);
        wait_for("the pool to empty once the terminal is gone", || watch() == (0, false));
    }

    #[test]
    fn recycling_gives_its_rows_back_when_output_stops() {
        let size = GridSize { cols: 120, rows: 30, cell_width: 9, cell_height: 17 };
        let idle = Duration::from_millis(300);
        let pool = Arc::new(RowPool::with_idle(idle));
        let (watch, recycling) = (pool.watch(), pool.watch_recycling());
        let terms = pair(size, 5_000, pool);
        let mut parsers = [Processor::new(), Processor::new()];
        feed(&terms, &mut parsers, &output(&mut 1, 7_000));
        wait_for("blank rows ready for recycling", || recycling() > 0);
        assert_same(&terms, "recycling");
        wait_for("the pool to empty once output stops", || watch() == (0, false) && recycling() == 0);
        // Output resumes: recycling starts again.
        feed(&terms, &mut parsers, &output(&mut 1, 2_000));
        wait_for("blank rows ready for recycling", || recycling() > 0);
        feed(&terms, &mut parsers, &output(&mut 1, 2_000));
        assert_same(&terms, "after output resumed");
    }

    #[test]
    fn the_pool_gives_its_rows_back_when_output_stops() {
        let size = GridSize { cols: 120, rows: 30, cell_width: 9, cell_height: 17 };
        let idle = Duration::from_millis(300);
        let pool = Arc::new(RowPool::with_idle(idle));
        let watch = pool.watch();
        let terms = pair(size, 50_000, pool);
        let mut parsers = [Processor::new(), Processor::new()];
        feed(&terms, &mut parsers, &output(&mut 1, 1_500));
        wait_for("rows ready", || watch().0 > 0);
        let start = Instant::now();
        wait_for("the pool to empty once output stops", || watch() == (0, false));
        assert!(start.elapsed() >= idle / 2, "not before output has stopped for a while");
        // Output resumes: the pool fills again.
        feed(&terms, &mut parsers, &output(&mut 1, 1_000));
        wait_for("rows ready", || watch().0 > 0);
        assert_same(&terms, "after output resumed");
    }
}
