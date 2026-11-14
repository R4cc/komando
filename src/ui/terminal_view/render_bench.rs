//! The grid's work for one frame, without a window: copying the screen out of the terminal and
//! laying out its rows (`layout_grid`), with GPUI's Linux text system shaping the glyphs. Painting
//! is left out: it needs a window.
//!
//! The benchmarks are ignored by default. Run them with an optimized build:
//!
//! ```text
//! cargo test --release --lib render_bench_ -- --ignored --nocapture --test-threads=1
//! ```
//!
//! `KOMANDO_RENDER_FRAMES` sets the frames timed per scenario (default 400).

use std::sync::Arc;
use std::time::{Duration, Instant};

use gpui::{point, px};

use super::{GridCache, GridFont, GridLook, layout_grid};
use crate::highlight::{self, Highlighter};
use crate::session::ssh::Output;
use crate::session::{EventProxy, GridSize, SharedTerm, new_term};
use crate::theme::TerminalPalette;

/// GPUI's text system as Linux has it, with Komando's bundled fonts.
pub(super) fn text_system() -> gpui::WindowTextSystem {
    let platform = Arc::new(gpui_wgpu::CosmicTextSystem::new("Cascadia Mono"));
    let text_system = Arc::new(gpui::TextSystem::new(platform));
    text_system
        .add_fonts(crate::FONTS.iter().map(|bytes| std::borrow::Cow::Borrowed(*bytes)).collect())
        .expect("bundled fonts");
    gpui::WindowTextSystem::new(text_system)
}

/// A terminal and everything a frame of its grid is laid out with.
pub(super) struct Harness {
    pub term: SharedTerm,
    proxy: EventProxy,
    output: Output,
    pub size: GridSize,
    pub text_system: gpui::WindowTextSystem,
    pub font: GridFont,
    pub palette: TerminalPalette,
    pub highlighter: Arc<Highlighter>,
    pub focused: bool,
    pub cache: GridCache,
}

impl Harness {
    pub fn new(cols: u16, rows: u16, text_system: gpui::WindowTextSystem) -> Self {
        let (tx, _rx) = futures::channel::mpsc::unbounded();
        let proxy = EventProxy::new(tx);
        let font = GridFont::new(&text_system, "Cascadia Mono".into(), px(14.));
        let size = GridSize { cols, rows, cell_width: 8, cell_height: 18 };
        let term = new_term(size, 10_000, proxy.clone());
        // The bundled set of highlight rules, as a first run has it.
        let sets = highlight::load(std::path::Path::new("/nonexistent/komando-render-bench"));
        Self {
            term,
            proxy,
            output: Output::default(),
            size,
            text_system,
            font,
            palette: TerminalPalette::dark(),
            highlighter: Arc::new(Highlighter::from_sets(&sets, &[])),
            focused: true,
            cache: GridCache::default(),
        }
    }

    pub fn feed(&mut self, bytes: &[u8]) {
        self.output.feed(&self.term, &self.proxy, bytes);
        self.proxy.clear_dirty();
    }

    /// Lay out one frame, as the terminal view's prepaint does.
    pub fn frame(&mut self) {
        let look = GridLook {
            font: &self.font,
            origin: point(px(12.), px(8.)),
            scale: 1.5,
            palette: &self.palette,
            highlighter: &self.highlighter,
            focused: self.focused,
        };
        layout_grid(&self.term, &self.text_system, &look, &mut self.cache);
    }
}

/// Log line `i`, about 96 bytes: a dim timestamp, a coloured level, a service and a message.
pub(super) fn log_line(i: u64, out: &mut Vec<u8>) {
    use std::io::Write;
    let (color, level) = match i % 16 {
        0 => ("31", "ERROR"),
        1 | 2 => ("33", "WARN "),
        3 => ("35", "DEBUG"),
        _ => ("32", "INFO "),
    };
    let services = ["api", "worker", "auth", "billing", "scheduler"];
    let service = services[(i % 5) as usize];
    let _ = write!(
        out,
        "\x1b[2m2026-11-13 {:02}:{:02}:{:02}.{:03}\x1b[0m \x1b[{color}m{level}\x1b[0m \x1b[36m{service:<9}\x1b[0m GET /api/items/{} {} in {}ms\r\n",
        (i / 3_600_000) % 24,
        (i / 60_000) % 60,
        (i / 1000) % 60,
        i % 1000,
        i % 100_000,
        if i.is_multiple_of(16) { 500 } else { 200 },
        i % 997,
    );
}

pub(super) fn log_lines(from: u64, count: u64) -> Vec<u8> {
    let mut out = Vec::new();
    for i in from..from + count {
        log_line(i, &mut out);
    }
    out
}

/// A screen of text in many scripts: accented letters written with combining marks, CJK (two
/// cells wide), emoji with variation selectors, box drawing and symbols, in four styles.
pub(super) fn unicode_screen(rows: u16) -> Vec<u8> {
    let words = [
        "Tie\u{0302}\u{0301}ng Vie\u{0323}\u{0302}t",
        "日本語のテキスト",
        "中文字符",
        "한국어",
        "\u{2764}\u{fe0f}",
        "\u{2705}",
        "\u{26a0}\u{fe0f}",
        "cafe\u{0301}",
        "na\u{0303}o",
        "\u{2500}\u{2500}\u{2524}",
        "\u{03a9}\u{2248}\u{221e}",
        "a\u{0308}o\u{0308}u\u{0308}",
        "\u{1f600}",
    ];
    let styles = ["0", "1", "3", "1;3"];
    let mut out = Vec::new();
    for row in 0..rows as usize {
        let mut line = String::new();
        let mut i = row;
        while line.chars().count() < 100 {
            line.push_str(&format!("\x1b[{}m{} ", styles[i % 4], words[i % words.len()]));
            i += 1;
        }
        out.extend_from_slice(line.as_bytes());
        out.extend_from_slice(b"\x1b[0m\r\n");
    }
    out
}

/// A screen where (almost) every cell is a different base letter with a combining mark, in four
/// styles: more clusters than one frame has ever needed before.
pub(super) fn distinct_clusters_screen(cols: u16, rows: u16, seed: usize) -> Vec<u8> {
    let letters: Vec<char> = ('a'..='z').chain('A'..='Z').collect();
    let marks: Vec<char> = ('\u{0300}'..='\u{036f}').collect();
    let styles = ["0", "1", "3", "1;3"];
    let mut out = String::new();
    let mut i = seed;
    for row in 0..rows {
        if row > 0 {
            out.push_str("\r\n");
        }
        for _ in 0..cols {
            let letter = letters[i % letters.len()];
            let mark = marks[(i / letters.len()) % marks.len()];
            let style = styles[(i / (letters.len() * marks.len())) % styles.len()];
            out.push_str(&format!("\x1b[{style}m{letter}{mark}"));
            i += 1;
        }
    }
    out.push_str("\x1b[0m");
    out.into_bytes()
}

fn frames() -> usize {
    std::env::var("KOMANDO_RENDER_FRAMES").ok().and_then(|v| v.parse().ok()).unwrap_or(400)
}

fn report(name: &str, mut samples: Vec<Duration>, shaped: usize) {
    samples.sort();
    let at = |q: f64| samples[((samples.len() - 1) as f64 * q).round() as usize];
    let mean = samples.iter().sum::<Duration>() / samples.len() as u32;
    eprintln!(
        "{name:<28} median {:>8.1} µs  p95 {:>8.1} µs  mean {:>8.1} µs  shaped/frame {:>7.1}",
        at(0.5).as_secs_f64() * 1e6,
        at(0.95).as_secs_f64() * 1e6,
        mean.as_secs_f64() * 1e6,
        shaped as f64 / samples.len() as f64,
    );
}

/// Time `frames` frames of `h`, calling `between` before each one (not timed).
fn run(name: &str, h: &mut Harness, mut between: impl FnMut(&mut Harness, usize)) {
    // Warm up: the glyph cache fills, allocations reach their size.
    for i in 0..20 {
        between(h, i);
        h.frame();
    }
    let frames = frames();
    let mut samples = Vec::with_capacity(frames);
    let shaped_before = super::shaped_count();
    for i in 0..frames {
        between(h, 20 + i);
        let start = Instant::now();
        h.frame();
        samples.push(start.elapsed());
    }
    report(name, samples, super::shaped_count() - shaped_before);
}

/// A shell prompt at the bottom of a screen of logs.
fn logs_with_prompt(h: &mut Harness) {
    let rows = h.size.rows as u64;
    h.feed(&log_lines(0, rows * 3));
    h.feed(b"\x1b[1;32muser@host\x1b[0m:\x1b[1;34m~\x1b[0m$ ");
}

#[test]
#[ignore]
fn render_bench_grid() {
    let (cols, rows) = (177, 42);
    let ts = text_system;
    eprintln!("grid {cols}x{rows}, {} frames per scenario", frames());

    // (a) Nothing changes between frames (a cursor blink, a hover, another pane's output).
    let mut h = Harness::new(cols, rows, ts());
    logs_with_prompt(&mut h);
    run("static", &mut h, |_, _| {});
    // The same without highlight rules.
    h.highlighter = Arc::new(Highlighter::default());
    run("static, no highlights", &mut h, |_, _| {});

    // (b) One character typed at the prompt per frame.
    let mut h = Harness::new(cols, rows, ts());
    logs_with_prompt(&mut h);
    run("typing", &mut h, |h, i| {
        if i % 60 == 59 {
            h.feed(b"\r\n\x1b[1;32muser@host\x1b[0m:\x1b[1;34m~\x1b[0m$ ");
        } else {
            h.feed(&[b'a' + (i % 26) as u8]);
        }
    });

    // (c) A log flood: more than two screens of new lines between frames.
    let mut h = Harness::new(cols, rows, ts());
    logs_with_prompt(&mut h);
    let mut next = 0;
    run("flood", &mut h, |h, _| {
        h.feed(&log_lines(next, 100));
        next += 100;
    });

    // (d) Many scripts and emoji on screen, static and scrolling.
    let mut h = Harness::new(cols, rows, ts());
    h.feed(&unicode_screen(rows));
    run("unicode static", &mut h, |_, _| {});
    h.highlighter = Arc::new(Highlighter::default());
    run("unicode static, no hl", &mut h, |_, _| {});
    let mut h = Harness::new(cols, rows, ts());
    let screen = unicode_screen(rows);
    run("unicode scrolling", &mut h, |h, _| h.feed(&screen));

    // (e) Thousands of distinct combining sequences on screen: more than the glyph cache
    // used to keep.
    let mut h = Harness::new(cols, rows, ts());
    h.feed(&distinct_clusters_screen(cols, rows, 0));
    run("distinct clusters static", &mut h, |_, _| {});
    h.highlighter = Arc::new(Highlighter::default());
    run("distinct clusters, no hl", &mut h, |_, _| {});
}
