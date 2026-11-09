//! Screen-model conformance: cursor motion, editing, scrolling, wrapping, charsets.
//! Each test states the VT behavior it pins down; expectations follow xterm.

mod common;
use accela_vt::{Attrs, Color, Underline, flags};
use common::{screen, term};

#[test]
fn autowrap_defers_until_next_print() {
    // Printing in the last column leaves the cursor there (pending wrap).
    let mut t = term(5, 2);
    t.feed(b"abcde");
    assert_eq!(t.cursor(), (0, 4));
    t.feed(b"f");
    assert_eq!(t.screen_text(), ["abcde", "f"]);
    assert!(t.screen()[0].wrapped);
    assert!(!t.screen()[1].wrapped);
}

#[test]
fn cr_clears_pending_wrap() {
    assert_eq!(screen(5, 2, "abcde\rX"), ["Xbcde", ""]);
}

#[test]
fn autowrap_off_overwrites_last_column() {
    assert_eq!(screen(5, 2, "\x1b[?7labcdefg"), ["abcdg", ""]);
}

#[test]
fn cursor_movement_is_clamped() {
    let mut t = term(10, 5);
    t.feed(b"\x1b[100;100H");
    assert_eq!(t.cursor(), (4, 9));
    t.feed(b"\x1b[50A\x1b[50D");
    assert_eq!(t.cursor(), (0, 0));
    t.feed(b"\x1b[3B\x1b[4C");
    assert_eq!(t.cursor(), (3, 4));
    t.feed(b"\x1b[2G\x1b[5d");
    assert_eq!(t.cursor(), (4, 1));
    t.feed(b"\x1b[2F");
    assert_eq!(t.cursor(), (2, 0));
    t.feed(b"\x1b[E");
    assert_eq!(t.cursor(), (3, 0));
    t.feed(b"\x1b[H");
    assert_eq!(t.cursor(), (0, 0));
    // Zero means one.
    t.feed(b"\x1b[0B\x1b[0C");
    assert_eq!(t.cursor(), (1, 1));
}

#[test]
fn erase_in_line_and_display() {
    assert_eq!(screen(6, 2, "abcdef\x1b[1;3H\x1b[K"), ["ab", ""]);
    assert_eq!(screen(6, 2, "abcdef\x1b[1;3H\x1b[1K"), ["   def", ""]);
    assert_eq!(screen(6, 2, "abcdef\x1b[1;3H\x1b[2K"), ["", ""]);
    assert_eq!(screen(3, 3, "abc\r\ndef\r\nghi\x1b[2;2H\x1b[J"), ["abc", "d", ""]);
    assert_eq!(screen(3, 3, "abc\r\ndef\r\nghi\x1b[2;2H\x1b[1J"), ["", "  f", "ghi"]);
    assert_eq!(screen(3, 3, "abc\r\ndef\r\nghi\x1b[2J"), ["", "", ""]);
}

#[test]
fn erase_uses_current_background() {
    let mut t = term(4, 1);
    t.feed(b"\x1b[41m\x1b[2K");
    assert!(t.screen()[0].cells.iter().all(|c| c.attrs.bg == Color::Indexed(1)));
}

#[test]
fn ed3_clears_scrollback_only() {
    let mut t = term(3, 2);
    t.feed(b"1\r\n2\r\n3\x1b[3J");
    assert_eq!(t.scrollback().len(), 0);
    assert_eq!(t.screen_text(), ["2", "3"]);
}

#[test]
fn insert_delete_erase_characters() {
    assert_eq!(screen(6, 1, "abcdef\x1b[1;2H\x1b[2@"), ["a  bcd"]);
    assert_eq!(screen(6, 1, "abcdef\x1b[1;2H\x1b[2P"), ["adef"]);
    assert_eq!(screen(6, 1, "abcdef\x1b[1;2H\x1b[2X"), ["a  def"]);
    assert_eq!(screen(6, 1, "abcdef\x1b[1;2H\x1b[99P"), ["a"]);
}

#[test]
fn insert_mode_shifts_text() {
    assert_eq!(screen(6, 1, "abc\x1b[1;2H\x1b[4hXY\x1b[4lZ"), ["aXYZc"]);
}

#[test]
fn insert_delete_lines_respect_region() {
    let init = "1\r\n2\r\n3\r\n4\r\n5";
    assert_eq!(screen(3, 5, &format!("{init}\x1b[2;4r\x1b[2;1H\x1b[L")), ["1", "", "2", "3", "5"]);
    assert_eq!(screen(3, 5, &format!("{init}\x1b[2;4r\x1b[2;1H\x1b[M")), ["1", "3", "4", "", "5"]);
    // Outside the region IL/DL do nothing.
    assert_eq!(screen(3, 5, &format!("{init}\x1b[2;4r\x1b[5;1H\x1b[L")), ["1", "2", "3", "4", "5"]);
}

#[test]
fn scroll_region_linefeed_and_reverse_index() {
    assert_eq!(screen(3, 4, "a\r\nb\r\nc\r\nd\x1b[2;3r\x1b[3;1H\n"), ["a", "c", "", "d"]);
    assert_eq!(screen(3, 4, "a\r\nb\r\nc\r\nd\x1b[2;3r\x1b[2;1H\x1bM"), ["a", "", "b", "d"]);
}

#[test]
fn scroll_up_down_commands() {
    assert_eq!(screen(3, 3, "a\r\nb\r\nc\x1b[S"), ["b", "c", ""]);
    assert_eq!(screen(3, 3, "a\r\nb\r\nc\x1b[2T"), ["", "", "a"]);
}

#[test]
fn region_scroll_does_not_feed_scrollback() {
    let mut t = term(3, 4);
    t.feed(b"a\r\nb\r\nc\r\nd\x1b[2;4r\x1b[4;1H\n\n");
    assert_eq!(t.scrollback().len(), 0);
    // But a region starting at the top does (like xterm).
    t.feed(b"\x1b[1;3r\x1b[3;1H\n");
    assert_eq!(t.scrollback().len(), 1);
}

#[test]
fn origin_mode_addresses_relative_to_region() {
    let mut t = term(5, 5);
    t.feed(b"\x1b[2;4r\x1b[?6h\x1b[1;1HX\x1b[10;1HY");
    assert_eq!(t.screen_text(), ["", "X", "", "Y", ""]);
}

#[test]
fn tabs() {
    assert_eq!(screen(20, 1, "a\tb"), ["a       b"]);
    assert_eq!(screen(20, 1, "\x1b[3g\x1b[1;4H\x1bH\ra\tb"), ["a  b"]);
    assert_eq!(screen(20, 1, "a\x1b[2Ib"), ["a               b"]);
    assert_eq!(screen(20, 1, "\x1b[1;12H\x1b[Zx"), ["        x"]);
    // Tab never passes the last column.
    let mut t = term(10, 1);
    t.feed(b"\t\t\t");
    assert_eq!(t.cursor(), (0, 9));
}

#[test]
fn backspace_and_bell() {
    let mut t = term(5, 1);
    t.feed(b"ab\x08c\x07");
    assert_eq!(t.screen_text(), ["ac"]);
    assert_eq!(t.take_events(), [accela_vt::Event::Bell]);
}

#[test]
fn save_restore_cursor_with_attributes() {
    let mut t = term(10, 2);
    t.feed(b"\x1b[1;31m\x1b[1;3H\x1b7\x1b[0m\x1b[2;1Hx\x1b8y");
    assert_eq!(t.cursor(), (0, 3));
    let y = t.screen()[0].cells[2];
    assert_eq!(y.c, 'y');
    assert_eq!(y.attrs.fg, Color::Indexed(1));
    assert!(y.attrs.has(flags::BOLD));
}

#[test]
fn sgr_attributes() {
    let mut t = term(20, 1);
    t.feed(b"\x1b[1;2;3;4;5;7;8;9;53ma\x1b[22;23;24;25;27;28;29;55mb");
    let a = t.screen()[0].cells[0].attrs;
    for f in [
        flags::BOLD,
        flags::DIM,
        flags::ITALIC,
        flags::BLINK,
        flags::INVERSE,
        flags::HIDDEN,
        flags::STRIKE,
        flags::OVERLINE,
    ] {
        assert!(a.has(f), "flag {f:#x}");
    }
    assert_eq!(a.underline(), Underline::Single);
    assert_eq!(t.screen()[0].cells[1].attrs, Attrs::default());
}

#[test]
fn sgr_colors_all_forms() {
    let mut t = term(20, 1);
    t.feed(b"\x1b[31;42ma\x1b[91;102mb\x1b[38;5;200;48;2;1;2;3mc\x1b[38:2::4:5:6md\x1b[38:2:7:8:9me\x1b[39;49mf");
    let c = |i: usize| t.screen()[0].cells[i].attrs;
    assert_eq!((c(0).fg, c(0).bg), (Color::Indexed(1), Color::Indexed(2)));
    assert_eq!((c(1).fg, c(1).bg), (Color::Indexed(9), Color::Indexed(10)));
    assert_eq!((c(2).fg, c(2).bg), (Color::Indexed(200), Color::Rgb(1, 2, 3)));
    assert_eq!(c(3).fg, Color::Rgb(4, 5, 6));
    assert_eq!(c(4).fg, Color::Rgb(7, 8, 9));
    assert_eq!((c(5).fg, c(5).bg), (Color::Default, Color::Default));
}

#[test]
fn sgr_underline_styles_and_color() {
    let mut t = term(10, 1);
    t.feed(b"\x1b[4:3;58;2;255;0;0ma\x1b[21mb\x1b[4:0mc");
    let r = &t.screen()[0];
    assert_eq!(r.cells[0].attrs.underline(), Underline::Curly);
    assert_eq!(t.ext(r.cells[0].attrs.ext).underline_color, Some(Color::Rgb(255, 0, 0)));
    assert_eq!(r.cells[1].attrs.underline(), Underline::Double);
    assert_eq!(r.cells[2].attrs.underline(), Underline::None);
}

#[test]
fn malformed_sgr_is_harmless() {
    let mut t = term(10, 1);
    t.feed(b"\x1b[38;5m\x1b[38;2;1m\x1b[48;9m\x1b[38;5;999mx");
    assert_eq!(t.screen_text(), ["x"]);
}

#[test]
fn wide_characters() {
    let mut t = term(4, 2);
    t.feed("a日本".as_bytes());
    let r = &t.screen()[0];
    assert!(r.cells[1].attrs.has(flags::WIDE));
    assert!(r.cells[2].attrs.has(flags::WIDE_SPACER));
    // Doesn't fit in the last column: wraps, leaving padding.
    assert_eq!(t.screen_text(), ["a日", "本"]);
    assert!(t.screen()[0].wrapped);
}

#[test]
fn overwriting_half_of_wide_char_clears_other_half() {
    let mut t = term(4, 1);
    t.feed("日本\x1b[1;2Hx".as_bytes());
    let r = &t.screen()[0];
    assert_eq!(r.cells[0].c, ' ');
    assert!(!r.cells[0].attrs.has(flags::WIDE));
    assert_eq!(r.cells[1].c, 'x');
    t.feed("\x1b[1;3Hy".as_bytes());
    assert_eq!(t.screen_text(), [" xy"]);
}

#[test]
fn combining_marks_attach_to_previous_cell() {
    let mut t = term(5, 1);
    t.feed("e\u{301}x".as_bytes());
    assert_eq!(t.screen_text(), ["e\u{301}x"]);
    assert_eq!(t.cursor(), (0, 2));
    // Zero-width joiners don't advance the cursor.
    let mut t = term(5, 1);
    t.feed("a\u{200d}b".as_bytes());
    assert_eq!(t.cursor(), (0, 2));
    assert_eq!(t.screen_text(), ["a\u{200d}b"]);
    // A zero-width char at column 0 with nothing before it is dropped, not a panic.
    let mut t = term(5, 1);
    t.feed("\u{301}x".as_bytes());
    assert_eq!(t.screen_text(), ["x"]);
}

#[test]
fn dec_special_graphics() {
    assert_eq!(screen(5, 1, "\x1b(0lqk\x1b(Bq"), ["┌─┐q"]);
    // SO/SI switch to G1.
    assert_eq!(screen(5, 1, "\x1b)0\x0eqx\x0fq"), ["─│q"]);
    // Single shift affects one char.
    assert_eq!(screen(5, 1, "\x1b*0\x1bNqq"), ["─q"]);
}

#[test]
fn repeat_last_character() {
    assert_eq!(screen(10, 1, "ab\x1b[3b"), ["abbbb"]);
    // REP is capped by the screen size, so a huge count can't hang the terminal.
    let mut t = term(10, 2);
    t.feed(b"x\x1b[65535b");
    assert_eq!(t.screen_text(), ["xxxxxxxxxx", "x"]);
}

#[test]
fn decaln_fills_screen() {
    assert_eq!(screen(3, 2, "\x1b#8"), ["EEE", "EEE"]);
}

#[test]
fn alternate_screen_1049() {
    let mut t = term(10, 3);
    t.feed(b"shell$ \x1b[?1049h\x1b[Hvim\x1b[2;1Hstuff");
    assert!(t.modes().alt_screen);
    assert_eq!(t.screen_text(), ["vim", "stuff", ""]);
    t.feed(b"\x1b[?1049l");
    assert!(!t.modes().alt_screen);
    assert_eq!(t.screen_text(), ["shell$", "", ""]);
    assert_eq!(t.cursor(), (0, 7));
}

#[test]
fn alternate_screen_does_not_touch_scrollback() {
    let mut t = term(5, 2);
    t.feed(b"\x1b[?1049h1\r\n2\r\n3\r\n4");
    assert_eq!(t.scrollback().len(), 0);
}

#[test]
fn alt_47_keeps_contents_1047_clears_on_exit() {
    let mut t = term(5, 1);
    t.feed(b"\x1b[?47hA\x1b[?47l\x1b[?47h");
    assert_eq!(t.screen_text(), ["A"]);
    let mut t = term(5, 1);
    t.feed(b"\x1b[?1047hA\x1b[?1047l\x1b[?1047h");
    assert_eq!(t.screen_text(), [""]);
}

#[test]
fn newline_mode() {
    assert_eq!(screen(5, 2, "\x1b[20hab\ncd"), ["ab", "cd"]);
}

#[test]
fn full_reset() {
    let mut t = term(5, 2);
    t.feed(b"\x1b[31mabc\x1b[?25l\x1b[2;2r\x1bc");
    assert_eq!(t.screen_text(), ["", ""]);
    assert!(t.modes().cursor_visible);
    assert_eq!(t.cursor(), (0, 0));
    t.feed(b"x");
    assert_eq!(t.screen()[0].cells[0].attrs, Attrs::default());
}

#[test]
fn soft_reset_keeps_screen() {
    let mut t = term(5, 2);
    t.feed(b"abc\x1b[?25l\x1b[1m\x1b[!p");
    assert_eq!(t.screen_text(), ["abc", ""]);
    assert!(t.modes().cursor_visible);
    t.feed(b"d");
    assert_eq!(t.screen()[0].cells[3].attrs, Attrs::default());
}

#[test]
fn cursor_style_and_visibility() {
    use accela_vt::CursorShape;
    let mut t = term(5, 1);
    t.feed(b"\x1b[6 q");
    assert_eq!(t.cursor_shape(), CursorShape::Bar);
    assert!(!t.modes().cursor_blink);
    t.feed(b"\x1b[3 q");
    assert_eq!(t.cursor_shape(), CursorShape::Underline);
    assert!(t.modes().cursor_blink);
    t.feed(b"\x1b[?25l");
    assert_eq!(t.cursor_in_view(), None);
}

#[test]
fn scrollback_and_viewport() {
    let mut t = term(5, 2);
    t.feed(b"1\r\n2\r\n3\r\n4");
    assert_eq!(t.scrollback().len(), 2);
    t.scroll_display(1);
    assert_eq!(t.display_offset(), 1);
    let mut s = String::new();
    t.visible_row(0).text(0, 5, &mut s);
    assert_eq!(s.trim_end(), "2");
    // New output keeps the viewport on the same content.
    t.feed(b"\r\n5");
    assert_eq!(t.display_offset(), 2);
    t.scroll_display(100);
    assert_eq!(t.display_offset(), 3);
    t.scroll_to_bottom();
    assert_eq!(t.display_offset(), 0);
    // The cursor is hidden while it's scrolled out of view.
    t.scroll_display(3);
    assert_eq!(t.cursor_in_view(), None);
}

#[test]
fn scrollback_limit_and_compaction() {
    let mut t = accela_vt::Term::with_scrollback(80, 2, 3);
    for i in 0..10 {
        t.feed(format!("{i}\r\n").as_bytes());
    }
    assert_eq!(t.scrollback().len(), 3);
    // Trailing blanks are not stored.
    assert!(t.scrollback().iter().all(|r| r.cells.len() <= 1));
}

#[test]
fn prompt_marks_allow_jumping() {
    let mut t = term(10, 3);
    for i in 0..4 {
        t.feed(format!("\x1b]133;A\x07$ cmd{i}\r\nout\r\n").as_bytes());
    }
    assert!(t.scroll_to_prompt(true));
    let mut s = String::new();
    t.visible_row(0).text(0, 10, &mut s);
    assert_eq!(s.trim_end(), "$ cmd2");
    assert!(t.scroll_to_prompt(true));
    assert!(t.scroll_to_prompt(false));
}

#[test]
fn scroll_to_line_centers_offscreen_lines_only() {
    let mut t = term(10, 4);
    for i in 0..20 {
        t.feed(format!("{i}\r\n").as_bytes());
    }
    let target = t.line_id_range().0 + 2; // "2", deep in history
    t.scroll_to_line(target);
    let rows: Vec<u64> = (0..4).map(|r| t.visible_line_id(r)).collect();
    assert!(rows.contains(&target));
    // Already visible: the viewport doesn't move.
    let before = t.display_offset();
    t.scroll_to_line(rows[1]);
    assert_eq!(t.display_offset(), before);
    // Lines on the live screen scroll back to the bottom.
    t.scroll_to_line(t.line_id_range().1 - 1);
    assert_eq!(t.display_offset(), 0);
}

// ---- left/right margins (DECLRMM ?69 + DECSLRM) ----

/// 10x4 screen filled with distinct rows, margins at columns 3..=6 (1-based 4..7).
fn margined() -> accela_vt::Term {
    let mut t = term(10, 4);
    t.feed(b"0123456789\r\nabcdefghij\r\nABCDEFGHIJ\r\nklmnopqrst");
    t.feed(b"\x1b[?69h\x1b[4;7s");
    t
}

#[test]
fn csi_s_is_save_cursor_without_declrmm() {
    let mut t = term(10, 2);
    t.feed(b"\x1b[1;5H\x1b[s\x1b[2;1H\x1b[u");
    assert_eq!(t.cursor(), (0, 4));
}

#[test]
fn decslrm_homes_cursor_and_wraps_inside_margins() {
    let mut t = margined();
    assert_eq!(t.cursor(), (0, 0));
    t.feed(b"\x1b[1;4HWXYZQ");
    // Printing wraps at the right margin (col 6) back to the left margin (col 3).
    assert_eq!(t.screen_text()[0], "012WXYZ789");
    assert_eq!(t.screen_text()[1], "abcQefghij");
    assert!(!t.screen()[0].wrapped);
}

#[test]
fn scrolling_moves_only_the_margin_rectangle() {
    let mut t = margined();
    t.feed(b"\x1b[S");
    assert_eq!(t.screen_text(), ["012defg789", "abcDEFGhij", "ABCnopqHIJ", "klm    rst"]);
    t.feed(b"\x1b[T");
    assert_eq!(t.screen_text(), ["012    789", "abcdefghij", "ABCDEFGHIJ", "klmnopqrst"]);
    // Nothing goes to scrollback.
    assert_eq!(t.scrollback().len(), 0);
}

#[test]
fn linefeed_at_bottom_scrolls_rectangle() {
    let mut t = margined();
    t.feed(b"\x1b[4;5H\n");
    assert_eq!(t.screen_text()[0], "012defg789");
    assert_eq!(t.screen_text()[3], "klm    rst");
}

#[test]
fn insert_delete_lines_and_chars_respect_margins() {
    let mut t = margined();
    t.feed(b"\x1b[2;5H\x1b[L");
    assert_eq!(t.screen_text(), ["0123456789", "abc    hij", "ABCdefgHIJ", "klmDEFGrst"]);
    let mut t = margined();
    t.feed(b"\x1b[2;5H\x1b[2P");
    assert_eq!(t.screen_text()[1], "abcdg  hij");
    let mut t = margined();
    t.feed(b"\x1b[2;5H\x1b[2@");
    assert_eq!(t.screen_text()[1], "abcd  ehij");
    // Outside the margins ICH/IL do nothing.
    let mut t = margined();
    t.feed(b"\x1b[2;1H\x1b[@\x1b[L");
    assert_eq!(t.screen_text()[1], "abcdefghij");
}

#[test]
fn cr_and_cursor_motion_stop_at_margins() {
    let mut t = margined();
    t.feed(b"\x1b[1;6H\r");
    assert_eq!(t.cursor(), (0, 3));
    t.feed(b"\x1b[1;2H\r");
    assert_eq!(t.cursor(), (0, 0));
    t.feed(b"\x1b[1;5H\x1b[99C");
    assert_eq!(t.cursor(), (0, 6));
    t.feed(b"\x1b[99D");
    assert_eq!(t.cursor(), (0, 3));
}

#[test]
fn origin_mode_is_relative_to_both_margins() {
    let mut t = margined();
    t.feed(b"\x1b[2;3r\x1b[?6h\x1b[1;1HX\x1b[9;9HY");
    assert_eq!(t.screen_text()[1], "abcXefghij");
    assert_eq!(t.screen_text()[2], "ABCDEFYHIJ");
}

#[test]
fn margins_reset_and_reports() {
    let mut t = margined();
    assert_eq!(common::reply(&mut t, "\x1bP$qs\x1b\\"), "\x1bP1$r4;7s\x1b\\");
    assert_eq!(common::reply(&mut t, "\x1b[?69$p"), "\x1b[?69;1$y");
    t.feed(b"\x1b[?69l");
    assert_eq!(common::reply(&mut t, "\x1bP$qs\x1b\\"), "\x1bP1$r1;10s\x1b\\");
    // DECSTR turns DECLRMM off.
    let mut t = margined();
    t.feed(b"\x1b[!p");
    assert!(!t.modes().lr_margins);
}

#[test]
fn wide_char_on_right_margin_is_cleared_as_a_pair() {
    // Found by the fuzzer: a wide char straddling the right margin, then another wide
    // char printed at the margin, used to leave an orphaned second half.
    let mut t = term(10, 3);
    t.feed("\x1b[1;5H日".as_bytes());
    t.feed("\x1b[?69h\x1b[2;5s\x1b[1;5H本".as_bytes());
    let r = &t.screen()[0];
    assert!(!r.cells[5].attrs.has(accela_vt::flags::WIDE_SPACER));
    assert_eq!(t.screen_text()[1], " 本");
}
