//! Query/response protocols and the security rules around them.

mod common;
use common::{reply, reply_fresh, term};
use lain_vt::{Event, MouseEncoding, MouseTracking, Rgb};

// ---- device reports ----

#[test]
fn device_attributes() {
    assert_eq!(reply_fresh("\x1b[c"), "\x1b[?62;4;22;52c");
    assert_eq!(reply_fresh("\x1b[0c"), "\x1b[?62;4;22;52c");
    assert_eq!(reply_fresh("\x1b[>c"), "\x1b[>1;10;0c");
    assert_eq!(reply_fresh("\x1b[=c"), "\x1bP!|00000000\x1b\\");
}

#[test]
fn xtversion() {
    assert_eq!(reply_fresh("\x1b[>q"), format!("\x1bP>|lain {}\x1b\\", env!("CARGO_PKG_VERSION")));
}

#[test]
fn status_and_cursor_reports() {
    assert_eq!(reply_fresh("\x1b[5n"), "\x1b[0n");
    assert_eq!(reply_fresh("\x1b[3;7H\x1b[6n"), "\x1b[3;7R");
    assert_eq!(reply_fresh("\x1b[3;7H\x1b[?6n"), "\x1b[?3;7;1R");
    // Relative to the scroll region in origin mode.
    assert_eq!(reply_fresh("\x1b[5;10r\x1b[?6h\x1b[2;1H\x1b[6n"), "\x1b[2;1R");
}

#[test]
fn window_size_reports() {
    let mut t = term(80, 24);
    t.set_cell_size(9, 18);
    assert_eq!(reply(&mut t, "\x1b[18t"), "\x1b[8;24;80t");
    assert_eq!(reply(&mut t, "\x1b[16t"), "\x1b[6;18;9t");
    assert_eq!(reply(&mut t, "\x1b[14t"), "\x1b[4;432;720t");
}

#[test]
fn title_report_is_refused() {
    // CSI 21 t would echo an attacker-chosen title into the shell's input.
    let mut t = term(80, 24);
    assert_eq!(reply(&mut t, "\x1b]2;rm -rf ~\x07\x1b[21t"), "");
}

// ---- modes ----

#[test]
fn decrqm_reports_mode_state() {
    let mut t = term(80, 24);
    assert_eq!(reply(&mut t, "\x1b[?2004$p"), "\x1b[?2004;2$y");
    assert_eq!(reply(&mut t, "\x1b[?2004h\x1b[?2004$p"), "\x1b[?2004;1$y");
    assert_eq!(reply(&mut t, "\x1b[?2026$p"), "\x1b[?2026;2$y");
    assert_eq!(reply(&mut t, "\x1b[?7$p"), "\x1b[?7;1$y");
    assert_eq!(reply(&mut t, "\x1b[?9999$p"), "\x1b[?9999;0$y");
    assert_eq!(reply(&mut t, "\x1b[4$p"), "\x1b[4;2$y");
}

#[test]
fn synchronized_output_mode() {
    let mut t = term(10, 2);
    t.feed(b"\x1b[?2026h");
    assert!(t.modes().synchronized);
    t.feed(b"\x1b[?2026l");
    assert!(!t.modes().synchronized);
}

#[test]
fn mouse_modes() {
    let mut t = term(10, 2);
    t.feed(b"\x1b[?1002h\x1b[?1006h");
    assert_eq!(t.modes().mouse, MouseTracking::ButtonEvent);
    assert_eq!(t.modes().mouse_encoding, MouseEncoding::Sgr);
    t.feed(b"\x1b[?1003h");
    assert_eq!(t.modes().mouse, MouseTracking::AnyEvent);
    t.feed(b"\x1b[?1003l\x1b[?1006l");
    assert_eq!(t.modes().mouse, MouseTracking::None);
    assert_eq!(t.modes().mouse_encoding, MouseEncoding::X10);
}

#[test]
fn focus_reporting() {
    let mut t = term(10, 2);
    t.focus_changed(true);
    assert!(t.take_output().is_empty());
    t.feed(b"\x1b[?1004h");
    t.focus_changed(false);
    t.focus_changed(true);
    assert_eq!(t.take_output(), b"\x1b[O\x1b[I");
}

// ---- DCS queries ----

#[test]
fn decrqss() {
    let mut t = term(80, 24);
    assert_eq!(reply(&mut t, "\x1b[1;38;5;100m\x1bP$qm\x1b\\"), "\x1bP1$r0;1;38;5;100m\x1b\\");
    assert_eq!(reply(&mut t, "\x1b[3;20r\x1bP$qr\x1b\\"), "\x1bP1$r3;20r\x1b\\");
    assert_eq!(reply(&mut t, "\x1b[5 q\x1bP$q q\x1b\\"), "\x1bP1$r5 q\x1b\\");
    assert_eq!(reply(&mut t, "\x1bP$qzz\x1b\\"), "\x1bP0$r\x1b\\");
}

#[test]
fn xtgettcap() {
    let hex = |s: &str| s.bytes().map(|b| format!("{b:02X}")).collect::<String>();
    let mut t = term(80, 24);
    assert_eq!(
        reply(&mut t, &format!("\x1bP+q{}\x1b\\", hex("TN"))),
        format!("\x1bP1+r{}={}\x1b\\", hex("TN"), hex("xterm-256color"))
    );
    assert_eq!(reply(&mut t, &format!("\x1bP+q{}\x1b\\", hex("Tc"))), format!("\x1bP1+r{}\x1b\\", hex("Tc")));
    assert_eq!(reply(&mut t, &format!("\x1bP+q{}\x1b\\", hex("nope"))), format!("\x1bP0+r{}\x1b\\", hex("nope")));
    // Several caps in one request get one reply each.
    let r = reply(&mut t, &format!("\x1bP+q{};{}\x1b\\", hex("Co"), hex("RGB")));
    assert_eq!(r.matches("\x1bP1+r").count(), 2);
}

#[test]
fn oversized_dcs_is_dropped() {
    let mut t = term(80, 24);
    let big = "A".repeat(100_000);
    assert_eq!(reply(&mut t, &format!("\x1bP$q{big}\x1b\\")), "");
    // The parser is still in a sane state afterwards.
    assert_eq!(reply(&mut t, "\x1b[5n"), "\x1b[0n");
}

// ---- colors ----

#[test]
fn color_queries_follow_request_terminator() {
    let mut t = term(80, 24);
    let bg = t.background().to_xparse();
    assert_eq!(reply(&mut t, "\x1b]11;?\x07"), format!("\x1b]11;{bg}\x07"));
    assert_eq!(reply(&mut t, "\x1b]11;?\x1b\\"), format!("\x1b]11;{bg}\x1b\\"));
    let fg = t.foreground().to_xparse();
    // "10;?;?" asks for fg then bg.
    assert_eq!(reply(&mut t, "\x1b]10;?;?\x07"), format!("\x1b]10;{fg}\x07\x1b]11;{bg}\x07"));
}

#[test]
fn palette_set_query_reset() {
    let mut t = term(80, 24);
    t.feed(b"\x1b]4;1;#102030\x07");
    assert_eq!(t.palette(1), Rgb(0x10, 0x20, 0x30));
    assert_eq!(reply(&mut t, "\x1b]4;1;?\x07"), "\x1b]4;1;rgb:1010/2020/3030\x07");
    t.feed(b"\x1b]104;1\x07");
    assert_eq!(t.palette(1), t.theme().palette[1]);
    t.feed(b"\x1b]11;rgb:ff/ff/ff\x07");
    assert_eq!(t.background(), Rgb(255, 255, 255));
    t.feed(b"\x1b]111\x07");
    assert_eq!(t.background(), t.theme().background);
}

#[test]
fn color_scheme_query() {
    // Default theme is dark.
    assert_eq!(reply_fresh("\x1b[?996n"), "\x1b[?997;1n");
    let mut t = term(80, 24);
    t.feed(b"\x1b]11;#ffffff\x07");
    assert_eq!(reply(&mut t, "\x1b[?996n"), "\x1b[?997;2n");
}

#[test]
fn color_scheme_change_notifications() {
    let mut t = term(80, 24);
    t.feed(b"\x1b[?2031h");
    let mut light = t.theme().clone();
    light.background = Rgb(250, 250, 250);
    t.set_theme(light);
    assert_eq!(t.take_output(), b"\x1b[?997;2n");
}

// ---- OSC ----

#[test]
fn title_is_sanitized_and_capped() {
    let mut t = term(80, 24);
    t.feed(b"\x1b]0;hi\x08there\x07");
    assert_eq!(t.title(), "hithere");
    t.feed(format!("\x1b]2;{}\x07", "x".repeat(5000)).as_bytes());
    assert!(t.title().chars().count() <= 1024);
    assert!(t.take_events().iter().any(|e| matches!(e, Event::Title(_))));
}

#[test]
fn title_stack() {
    let mut t = term(80, 24);
    t.feed(b"\x1b]2;one\x07\x1b[22t\x1b]2;two\x07\x1b[23t");
    assert_eq!(t.title(), "one");
}

#[test]
fn clipboard_write_allowed_read_refused() {
    let mut t = term(80, 24);
    t.feed(b"\x1b]52;c;aGVsbG8=\x07");
    assert_eq!(t.take_events(), [Event::Clipboard { target: 'c', data: "hello".into() }]);
    // Query: no reply, no event.
    assert_eq!(reply(&mut t, "\x1b]52;c;?\x07"), "");
    assert!(t.take_events().is_empty());
    // Invalid base64 is ignored.
    t.feed(b"\x1b]52;c;!!!\x07");
    assert!(t.take_events().is_empty());
}

#[test]
fn hyperlinks() {
    let mut t = term(20, 1);
    t.feed(b"\x1b]8;id=x;https://example.com\x1b\\link\x1b]8;;\x1b\\ plain");
    let l = t.hyperlink_at(0, 0).expect("link");
    assert_eq!(l.uri, "https://example.com");
    assert_eq!(l.id.as_deref(), Some("x"));
    assert!(t.hyperlink_at(0, 3).is_some());
    assert!(t.hyperlink_at(0, 5).is_none());
}

#[test]
fn hyperlink_table_is_bounded() {
    let mut t = term(20, 1);
    for i in 0..10_000 {
        t.feed(format!("\x1b]8;;https://e.com/{i}\x07x\x1b]8;;\x07").as_bytes());
    }
    // No panic, and oversized URIs are rejected outright.
    t.feed(format!("\r\x1b[2K\x1b]8;;https://{}\x07y", "a".repeat(5000)).as_bytes());
    assert_eq!(t.screen_text(), ["y"]);
    assert!(t.hyperlink_at(0, 0).is_none());
}

#[test]
fn cwd_and_notifications() {
    let mut t = term(80, 24);
    t.feed(b"\x1b]7;file://host/home/me/my%20dir\x07");
    assert_eq!(t.cwd(), Some("/home/me/my dir"));
    t.feed(b"\x1b]9;build done\x07\x1b]9;4;1;50\x07\x1b]777;notify;Title;Body\x07");
    let ev = t.take_events();
    assert!(ev.contains(&Event::Notify { title: String::new(), body: "build done".into() }));
    assert!(ev.contains(&Event::Notify { title: "Title".into(), body: "Body".into() }));
    assert_eq!(ev.iter().filter(|e| matches!(e, Event::Notify { .. })).count(), 2);
}

// ---- kitty keyboard protocol ----

#[test]
fn kitty_keyboard_stack() {
    let mut t = term(80, 24);
    assert_eq!(reply(&mut t, "\x1b[?u"), "\x1b[?0u");
    t.feed(b"\x1b[>1u\x1b[>3u");
    assert_eq!(t.kitty_keyboard_flags(), 3);
    t.feed(b"\x1b[<u");
    assert_eq!(t.kitty_keyboard_flags(), 1);
    t.feed(b"\x1b[=8;2u");
    assert_eq!(t.kitty_keyboard_flags(), 9);
    t.feed(b"\x1b[=1;3u");
    assert_eq!(t.kitty_keyboard_flags(), 8);
    t.feed(b"\x1b[<99u");
    assert_eq!(t.kitty_keyboard_flags(), 0);
}

#[test]
fn kitty_keyboard_stacks_are_per_screen() {
    let mut t = term(80, 24);
    t.feed(b"\x1b[>1u\x1b[?1049h");
    assert_eq!(t.kitty_keyboard_flags(), 0);
    t.feed(b"\x1b[>8u\x1b[?1049l");
    assert_eq!(t.kitty_keyboard_flags(), 1);
}

#[test]
fn kitty_keyboard_stack_is_bounded() {
    let mut t = term(80, 24);
    for _ in 0..1000 {
        t.feed(b"\x1b[>1u");
    }
    t.feed(b"\x1b[<16u");
    assert_eq!(t.kitty_keyboard_flags(), 0);
}

// ---- limits ----

#[test]
fn resize_is_clamped() {
    // A UI bug (or a bogus window size) must not allocate gigabytes.
    let mut t = term(80, 24);
    t.resize(1_000_000, 2);
    assert_eq!(t.cols(), 4096);
    t.resize(2, 1_000_000);
    assert_eq!(t.rows(), 2048);
    t.resize(0, 0);
    assert_eq!((t.cols(), t.rows()), (1, 1));
    t.feed("日本語".as_bytes());
}
