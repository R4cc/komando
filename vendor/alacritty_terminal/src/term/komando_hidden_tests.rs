//! Komando patch: a clear hides the Scrollback instead of erasing it, and it can come back.

use crate::event::VoidListener;
use crate::grid::Dimensions;
use crate::index::{Column, Line};
use crate::term::test::TermSize;
use crate::term::{Config, Term};
use crate::vte::ansi::Processor;

fn term(columns: usize, lines: usize, history: usize) -> (Term<VoidListener>, Processor) {
    let config = Config { scrolling_history: history, ..Config::default() };
    (Term::new(config, &TermSize::new(columns, lines), VoidListener), Processor::new())
}

fn feed(term: &mut Term<VoidListener>, parser: &mut Processor, text: &str) {
    parser.advance(term, text.as_bytes());
}

fn row(term: &Term<VoidListener>, line: i32) -> String {
    let row = &term.grid()[Line(line)];
    (0..term.columns()).map(|c| row[Column(c)].c).collect::<String>().trim_end().to_string()
}

fn print_rows(term: &mut Term<VoidListener>, parser: &mut Processor, range: std::ops::Range<usize>) {
    for i in range {
        feed(term, parser, &format!("row {i}\r\n"));
    }
}

#[test]
fn erase_display_hides_the_scrollback_until_restored() {
    let (mut term, mut p) = term(20, 4, 100);
    print_rows(&mut term, &mut p, 0..10);
    feed(&mut term, &mut p, "\x1b[H\x1b[2J$ ");
    assert_eq!(term.grid().history_size(), 0);
    assert_eq!(term.grid().hidden_history(), 10);
    assert_eq!(row(&term, 0), "$");

    print_rows(&mut term, &mut p, 10..15);
    let history = term.grid().history_size();
    assert!(history > 0);

    term.grid_mut().restore_history();
    assert_eq!(term.grid().hidden_history(), 0);
    assert_eq!(term.grid().history_size(), history + 10);
    let top = -(term.grid().history_size() as i32);
    assert_eq!(row(&term, top), "row 0");
    assert_eq!(row(&term, top + 9), "row 9");
    assert_eq!(row(&term, top + 10), "$ row 10");
}

#[test]
fn erase_scrollback_hides_it_too() {
    let (mut term, mut p) = term(20, 4, 100);
    print_rows(&mut term, &mut p, 0..10);
    feed(&mut term, &mut p, "\x1b[H\x1b[2J\x1b[3J");
    assert_eq!(term.grid().history_size(), 0);
    assert_eq!(term.grid().hidden_history(), 10);
    term.grid_mut().restore_history();
    assert_eq!(row(&term, -(term.grid().history_size() as i32)), "row 0");
}

#[test]
fn new_output_pushes_out_the_oldest_hidden_rows() {
    let (mut term, mut p) = term(20, 4, 20);
    print_rows(&mut term, &mut p, 0..30);
    feed(&mut term, &mut p, "\x1b[H\x1b[2J");
    assert_eq!(term.grid().hidden_history(), 20);
    print_rows(&mut term, &mut p, 30..42);
    assert_eq!(term.grid().history_size() + term.grid().hidden_history(), 20);
    term.grid_mut().restore_history();
    assert_eq!(term.grid().history_size(), 20);
}

#[test]
fn hidden_rows_rewrap_on_resize_and_stay_apart() {
    let (mut term, mut p) = term(10, 4, 100);
    feed(&mut term, &mut p, "abcdefghijklmno\r\n");
    print_rows(&mut term, &mut p, 0..6);
    feed(&mut term, &mut p, "\x1b[H\x1b[2Jxy");
    let hidden = term.grid().hidden_history();
    term.resize(TermSize::new(20, 4));
    // The 15 characters fit on one row now.
    assert_eq!(term.grid().hidden_history(), hidden - 1);
    term.resize(TermSize::new(5, 4));
    assert_eq!(term.grid().hidden_history(), hidden + 1);

    term.resize(TermSize::new(10, 4));
    assert_eq!(term.grid().hidden_history(), hidden);
    term.grid_mut().restore_history();
    let top = -(term.grid().history_size() as i32);
    assert_eq!(row(&term, top), "abcdefghij");
    assert_eq!(row(&term, top + 1), "klmno");
    assert_eq!(row(&term, 0), "xy");
}

#[test]
fn a_reset_erases_the_hidden_rows() {
    let (mut term, mut p) = term(20, 4, 100);
    print_rows(&mut term, &mut p, 0..10);
    feed(&mut term, &mut p, "\x1b[2J\x1bc");
    assert_eq!(term.grid().hidden_history(), 0);
}
