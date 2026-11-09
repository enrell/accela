#![allow(dead_code)]
//! Shared helpers for lain-vt integration tests.

use lain_vt::Term;

pub fn term(cols: usize, rows: usize) -> Term {
    Term::new(cols, rows)
}

/// Feed `input` into a fresh terminal and return the screen lines.
pub fn screen(cols: usize, rows: usize, input: &str) -> Vec<String> {
    let mut t = term(cols, rows);
    t.feed(input.as_bytes());
    t.screen_text()
}

/// Feed `input` and return what the terminal wrote back to the program.
pub fn reply(t: &mut Term, input: &str) -> String {
    t.feed(input.as_bytes());
    String::from_utf8(t.take_output()).expect("replies are UTF-8")
}

pub fn reply_fresh(input: &str) -> String {
    reply(&mut term(80, 24), input)
}
