//! Resize behavior: soft-wrapped lines reflow, the cursor follows its text, and
//! scrollback absorbs/releases rows as the window height changes.

mod common;
use common::term;
use lain_vt::Term;

#[test]
fn shrink_then_grow_restores_line() {
    let mut t = term(20, 4);
    t.feed(b"hello world");
    t.resize(5, 4);
    assert_eq!(t.screen_text(), ["hello", " worl", "d", ""]);
    assert_eq!(t.cursor(), (2, 1));
    assert!(t.screen()[0].wrapped && t.screen()[1].wrapped && !t.screen()[2].wrapped);
    t.resize(20, 4);
    assert_eq!(t.screen_text(), ["hello world", "", "", ""]);
    assert_eq!(t.cursor(), (0, 11));
}

#[test]
fn hard_newlines_are_not_joined() {
    let mut t = term(10, 3);
    t.feed(b"ab\r\ncd");
    t.resize(20, 3);
    assert_eq!(t.screen_text(), ["ab", "cd", ""]);
}

#[test]
fn cursor_after_trailing_spaces_is_kept() {
    // A prompt like "$ " ends in a space that isn't stored; the cursor must stay after it.
    let mut t = term(20, 2);
    t.feed(b"$ ");
    t.resize(10, 2);
    assert_eq!(t.cursor(), (0, 2));
}

#[test]
fn growing_height_pulls_from_scrollback() {
    let mut t = term(10, 3);
    t.feed(b"1\r\n2\r\n3\r\n4\r\n5");
    assert_eq!(t.scrollback().len(), 2);
    t.resize(10, 5);
    assert_eq!(t.screen_text(), ["1", "2", "3", "4", "5"]);
    assert_eq!(t.scrollback().len(), 0);
    assert_eq!(t.cursor(), (4, 1));
}

#[test]
fn shrinking_height_keeps_cursor_visible() {
    let mut t = term(10, 5);
    t.feed(b"1\r\n2\r\n3\r\n4\r\n5");
    t.resize(10, 2);
    assert_eq!(t.screen_text(), ["4", "5"]);
    assert_eq!(t.scrollback().len(), 3);
    assert_eq!(t.cursor(), (1, 1));
}

#[test]
fn shrinking_height_drops_blank_rows_below_cursor_first() {
    let mut t = term(10, 5);
    t.feed(b"a\r\nb");
    t.resize(10, 2);
    assert_eq!(t.screen_text(), ["a", "b"]);
    assert_eq!(t.scrollback().len(), 0);
}

#[test]
fn wide_chars_never_split_across_rows() {
    let mut t = term(3, 2);
    t.feed("a日".as_bytes());
    t.resize(2, 2);
    assert_eq!(t.screen_text(), ["a", "日"]);
    assert_eq!(t.cursor(), (1, 1));
    t.resize(3, 2);
    assert_eq!(t.screen_text(), ["a日", ""]);
}

#[test]
fn combining_marks_survive_reflow() {
    let mut t = term(4, 2);
    t.feed("ae\u{301}bc".as_bytes());
    t.resize(2, 3);
    assert_eq!(t.screen_text(), ["ae\u{301}", "bc", ""]);
    t.resize(4, 3);
    assert_eq!(t.screen_text()[0], "ae\u{301}bc");
}

#[test]
fn primary_reflows_while_alt_screen_is_active() {
    let mut t = term(10, 3);
    t.feed(b"hello world\x1b[?1049h\x1b[Hfullscreen app");
    t.resize(20, 3);
    assert_eq!(t.screen_text()[0], "fullscreen");
    t.feed(b"\x1b[?1049l");
    assert_eq!(t.screen_text(), ["hello world", "", ""]);
    assert_eq!(t.cursor(), (0, 11));
}

#[test]
fn reflow_respects_scrollback_limit() {
    let mut t = Term::with_scrollback(6, 2, 3);
    for i in 0..10 {
        t.feed(format!("line{i}\r\n").as_bytes());
    }
    t.resize(3, 2);
    assert!(t.scrollback().len() <= 3);
    assert_eq!(t.cursor().0, 1);
}

#[test]
fn prompt_marks_survive_reflow() {
    let mut t = term(10, 3);
    t.feed(b"\x1b]133;A\x07$ long command line\r\n");
    t.resize(5, 6);
    let marked = t.screen().iter().filter(|r| r.mark == lain_vt::Mark::PromptStart).count();
    assert_eq!(marked, 1);
}

#[test]
fn resize_is_noop_when_unchanged() {
    let mut t = term(10, 3);
    t.feed(b"abc");
    t.take_dirty();
    t.resize(10, 3);
    assert!(!t.take_dirty());
}
