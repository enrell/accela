//! Audit against the contract we advertise: `TERM=xterm-256color`.
//!
//! Programs using terminfo (ncurses, vim, less, htop...) only send what that entry
//! declares and only expect the keys it lists. This test reads the *installed* entry,
//! expands every capability with `tput`, and checks each one against the emulator:
//!
//! - every output capability is consumed cleanly (nothing leaks onto the screen and
//!   the parser is back in its ground state afterwards);
//! - the ones with a visible effect have that exact effect;
//! - every key capability matches our key encoder byte for byte (in keypad-transmit
//!   mode, `smkx`, which is what curses programs enable);
//! - every `acsc` character exists in our DEC special-graphics table.
//!
//! An unclassified capability fails the test, so a newer terminfo can't slip by.
//! Skipped when `infocmp`/`tput` aren't installed.

use accela_vt::input::{InputModes, Key, KeyAction, KeyEvent, KeypadKey, Mods, encode_key};
use accela_vt::mouse::{MouseAction, MouseButton, MouseEvent, encode_mouse};
use accela_vt::{Color, Term, Underline, flags};
use std::collections::BTreeMap;
use std::process::Command;

const TERM: &str = "xterm-256color";

#[derive(Debug)]
enum Cap {
    Bool,
    Num(i64),
    Str(String),
}

fn load() -> Option<BTreeMap<String, Cap>> {
    let out = Command::new("infocmp").args(["-1", "-x", TERM]).output().ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8(out.stdout).ok()?;
    let mut caps = BTreeMap::new();
    for line in text.lines().skip(1).filter(|l| l.starts_with('\t')) {
        let entry = line.trim().trim_end_matches(',');
        if let Some((k, v)) = entry.split_once('=') {
            caps.insert(k.to_string(), Cap::Str(v.to_string()));
        } else if let Some((k, v)) = entry.split_once('#') {
            let n = if let Some(h) = v.strip_prefix("0x") { i64::from_str_radix(h, 16) } else { v.parse() };
            caps.insert(k.to_string(), Cap::Num(n.unwrap_or(0)));
        } else {
            caps.insert(entry.to_string(), Cap::Bool);
        }
    }
    Some(caps)
}

/// The capability's bytes with parameters substituted (by ncurses itself).
fn tput(cap: &str, args: &[&str]) -> Vec<u8> {
    let out = Command::new("tput").arg("-T").arg(TERM).arg(cap).args(args).output().expect("tput");
    assert!(out.status.success(), "tput {cap} {args:?} failed: {}", String::from_utf8_lossy(&out.stderr));
    // Strip $<..> padding NULs if tput emitted any.
    out.stdout.into_iter().filter(|&b| b != 0).collect()
}

/// Parameters used to expand each parameterized output capability.
fn params(cap: &str) -> Option<&'static [&'static str]> {
    Some(match cap {
        "cup" => &["2", "3"],
        "csr" => &["1", "3"],
        "cub" | "cud" | "cuf" | "cuu" | "dch" | "dl" | "ech" | "ich" | "il" | "indn" | "rin" => &["2"],
        "hpa" | "vpa" => &["3"],
        "initc" => &["1", "1000", "0", "0"],
        "rep" => &["120", "3"],
        "setab" | "setaf" => &["1"],
        "sgr" => &["0", "1", "0", "0", "0", "1", "0", "0", "0"],
        "smglp" | "smgrp" => &["3"],
        "smglr" => &["2", "5"],
        "Ss" => &["6"],
        "XM" => &["1"],
        _ => return None,
    })
}

/// Capabilities whose value is a *response format* or data, not something sent to us.
const NOT_OUTPUT: &[(&str, &str)] = &[
    ("acsc", "character-set table, checked separately"),
    ("u6", "format of our cursor position report, checked separately"),
    ("u8", "format of our DA1 reply, checked separately"),
    ("rv", "xterm DA2 version pattern; we identify via XTVERSION instead (like kitty, foot, wezterm)"),
    ("xr", "xterm XTVERSION pattern; we reply `accela <version>`"),
    ("xm", "format of SGR mouse reports, checked through kmous/XM"),
];

/// String capabilities that take a string argument tput can't pass; expanded by hand.
fn manual(cap: &str) -> Option<Vec<u8>> {
    Some(match cap {
        "Cs" => b"\x1b]12;#ff0000\x07".to_vec(),
        "Ms" => b"\x1b]52;c;aGk=\x07".to_vec(),
        _ => return None,
    })
}

fn fresh() -> Term {
    let mut t = Term::new(20, 6);
    t.set_cell_size(10, 20);
    // Start in a known spot with known content around it.
    t.feed(b"\x1b[3;4H");
    t
}

fn filled() -> Term {
    let mut t = Term::new(20, 6);
    for r in 0..6 {
        t.feed(format!("\x1b[{};1H{}", r + 1, "abcdefghijklmnopqrst").as_bytes());
    }
    t.feed(b"\x1b[3;4H");
    t
}

/// Exact effect of a capability, given the screen state from `filled()`.
fn check_effect(cap: &str, bytes: &[u8]) {
    let mut t = filled();
    t.feed(bytes);
    let text = t.screen_text();
    fn a(t: &Term, col: usize) -> accela_vt::Attrs {
        t.screen()[2].cells[col].attrs
    }
    match cap {
        "cup" => assert_eq!(t.cursor(), (2, 3)),
        "home" => assert_eq!(t.cursor(), (0, 0)),
        "cub1" => assert_eq!(t.cursor(), (2, 2)),
        "cub" => assert_eq!(t.cursor(), (2, 1)),
        "cuf1" => assert_eq!(t.cursor(), (2, 4)),
        "cuf" => assert_eq!(t.cursor(), (2, 5)),
        "cuu1" => assert_eq!(t.cursor(), (1, 3)),
        "cuu" => assert_eq!(t.cursor(), (0, 3)),
        "cud1" | "ind" => assert_eq!(t.cursor(), (3, 3)),
        "cud" => assert_eq!(t.cursor(), (4, 3)),
        "hpa" => assert_eq!(t.cursor(), (2, 3)),
        "vpa" => assert_eq!(t.cursor(), (3, 3)),
        "cr" => assert_eq!(t.cursor(), (2, 0)),
        "nel" => assert_eq!(t.cursor(), (3, 0)),
        "ri" => assert_eq!(t.cursor(), (1, 3)),
        "ht" => assert_eq!(t.cursor(), (2, 8)),
        "cbt" => assert_eq!(t.cursor(), (2, 0)),
        "clear" => assert!(text.iter().all(|l| l.is_empty()) && t.cursor() == (0, 0)),
        "ed" => assert_eq!(text[2..], ["abc", "", "", ""]),
        "el" => assert_eq!(text[2], "abc"),
        "el1" => assert_eq!(text[2], "    efghijklmnopqrst"),
        "ech" => assert_eq!(text[2], "abc  fghijklmnopqrst"),
        "dch1" => assert_eq!(text[2], "abcefghijklmnopqrst"),
        "dch" => assert_eq!(text[2], "abcfghijklmnopqrst"),
        "ich" => assert_eq!(text[2], "abc  defghijklmnopqr"),
        "il1" => assert_eq!(text[2..4], ["", "abcdefghijklmnopqrst"]),
        "il" => assert_eq!(text[2..5], ["", "", "abcdefghijklmnopqrst"]),
        "dl1" | "dl" => assert_eq!(text[5], ""),
        "indn" => assert_eq!(text[4..], ["", ""]),
        "rin" => assert_eq!(text[..2], ["", ""]),
        "csr" => {
            // Region rows 2..4 (1-based); a linefeed at its bottom scrolls only inside it.
            t.feed(b"\x1b[4;1H\n");
            assert_eq!(
                t.screen_text()[1..5],
                ["abcdefghijklmnopqrst", "abcdefghijklmnopqrst", "", "abcdefghijklmnopqrst"]
            );
        }
        "sc" => {
            t.feed(b"\x1b[1;1H");
            t.feed(&tput("rc", &[]));
            assert_eq!(t.cursor(), (2, 3));
        }
        // Restore without a prior save goes to the home position.
        "rc" => assert_eq!(t.cursor(), (0, 0)),
        "hts" => {
            t.feed(b"\r\t");
            assert_eq!(t.cursor(), (2, 3));
        }
        "tbc" => {
            t.feed(b"\r\t");
            assert_eq!(t.cursor(), (2, 19));
        }
        "rep" => assert_eq!(text[2], "abcxxxghijklmnopqrst"),
        "bold" | "blink" | "dim" | "invis" | "rev" | "sitm" | "smso" | "smul" | "smxx" | "setaf" | "setab" | "sgr" => {
            t.feed(b"Z");
            let at = a(&t, 3);
            let ok = match cap {
                "bold" => at.has(flags::BOLD),
                "blink" => at.has(flags::BLINK),
                "dim" => at.has(flags::DIM),
                "invis" => at.has(flags::HIDDEN),
                "rev" | "smso" => at.has(flags::INVERSE),
                "sitm" => at.has(flags::ITALIC),
                "smul" => at.underline() == Underline::Single,
                "smxx" => at.has(flags::STRIKE),
                "setaf" => at.fg == Color::Indexed(1),
                "setab" => at.bg == Color::Indexed(1),
                "sgr" => at.has(flags::BOLD) && at.underline() == Underline::Single && !at.has(flags::INVERSE),
                _ => unreachable!(),
            };
            assert!(ok, "{cap}: {at:?}");
        }
        "sgr0" | "op" | "ritm" | "rmso" | "rmul" | "rmxx" => {
            t.feed(b"\x1b[1;3;4;7;9;31;42m");
            t.feed(bytes);
            t.feed(b"Z");
            let at = a(&t, 3);
            let cleared = match cap {
                "sgr0" => at == accela_vt::Attrs::default(),
                "op" => at.fg == Color::Default && at.bg == Color::Default,
                "ritm" => !at.has(flags::ITALIC),
                "rmso" => !at.has(flags::INVERSE),
                "rmul" => at.underline() == Underline::None,
                "rmxx" => !at.has(flags::STRIKE),
                _ => unreachable!(),
            };
            assert!(cleared, "{cap}: {at:?}");
        }
        "smacs" => {
            t.feed(b"q");
            assert_eq!(t.screen_text()[2].chars().nth(3), Some('─'));
        }
        "rmacs" => {
            t.feed(&tput("smacs", &[]));
            t.feed(bytes);
            t.feed(b"q");
            assert_eq!(t.screen_text()[2].chars().nth(3), Some('q'));
        }
        "smcup" => assert!(t.modes().alt_screen),
        "rmcup" => {
            t.feed(&tput("smcup", &[]));
            t.feed(bytes);
            assert!(!t.modes().alt_screen);
            assert_eq!(t.cursor(), (2, 3));
        }
        "civis" => assert!(!t.modes().cursor_visible),
        "cnorm" | "cvvis" => {
            assert!(t.modes().cursor_visible);
            if cap == "cvvis" {
                assert!(t.modes().cursor_blink);
            }
        }
        "smam" => assert!(t.modes().autowrap),
        "rmam" => assert!(!t.modes().autowrap),
        "smir" => assert!(t.modes().insert),
        "rmir" => assert!(!t.modes().insert),
        "smkx" => assert!(t.modes().app_cursor && t.modes().app_keypad),
        "rmkx" => assert!(!t.modes().app_cursor && !t.modes().app_keypad),
        "BE" => assert!(t.modes().bracketed_paste),
        "BD" => assert!(!t.modes().bracketed_paste),
        "fe" => assert!(t.modes().focus_events),
        "fd" => assert!(!t.modes().focus_events),
        "XM" => {
            assert_eq!(t.modes().mouse, accela_vt::MouseTracking::Normal);
            assert_eq!(t.modes().mouse_encoding, accela_vt::MouseEncoding::Sgr);
        }
        "Ss" => assert_eq!(t.cursor_shape(), accela_vt::CursorShape::Bar),
        "Se" => assert_eq!(t.cursor_shape(), accela_vt::CursorShape::Block),
        "Cs" => assert_eq!(t.cursor_color(), accela_vt::Rgb(255, 0, 0)),
        "Cr" => assert_eq!(t.cursor_color(), t.theme().cursor),
        "initc" => assert_eq!(t.palette(1), accela_vt::Rgb(255, 0, 0)),
        "oc" => assert_eq!(t.palette(1), t.theme().palette[1]),
        "Ms" => assert_eq!(t.take_events(), [accela_vt::Event::Clipboard { target: 'c', data: "hi".into() }]),
        "E3" => assert_eq!(t.scrollback().len(), 0),
        "smglr" | "smglp" | "smgrp" => {
            assert!(t.modes().lr_margins);
            assert_eq!(t.cursor(), (0, 0));
        }
        "mgc" => assert!(!t.modes().lr_margins),
        "u7" => assert_eq!(t.take_output(), b"\x1b[3;4R"),
        "u9" => assert!(t.take_output().starts_with(b"\x1b[?")),
        "RV" => assert!(t.take_output().starts_with(b"\x1b[>")),
        "XR" => assert!(t.take_output().starts_with(b"\x1bP>|accela")),
        "bel" => assert_eq!(t.take_events(), [accela_vt::Event::Bell]),
        "flash" => assert!(!t.modes().reverse_video, "flash must end with reverse video off"),
        "is2" | "rs2" | "rs1" => {
            assert!(t.modes().autowrap && !t.modes().insert && !t.modes().app_keypad);
            if cap == "rs1" {
                assert_eq!(t.cursor(), (0, 0));
            }
        }
        // Printer, meta-key and memory-lock controls: accepted and ignored.
        "mc0" | "mc4" | "mc5" | "smm" | "rmm" | "meml" | "memu" | "PS" | "PE" => {}
        other => panic!("output capability `{other}` has no effect check: classify it"),
    }
}

/// The key and modifiers a terminfo key capability describes.
fn key_for(cap: &str) -> Option<(Key, Mods)> {
    let m = |shift, alt, ctrl| Mods { shift, alt, ctrl, super_: false };
    // xterm modifier suffixes: 2 shift, 3 alt, 4 shift+alt, 5 ctrl, 6 ctrl+shift, 7 ctrl+alt.
    let suffix = |n: u8| match n {
        2 => m(true, false, false),
        3 => m(false, true, false),
        4 => m(true, true, false),
        5 => m(false, false, true),
        6 => m(true, false, true),
        7 => m(false, true, true),
        _ => unreachable!(),
    };
    let named = [
        ("kDC", Key::Delete),
        ("kDN", Key::Down),
        ("kEND", Key::End),
        ("kHOM", Key::Home),
        ("kIC", Key::Insert),
        ("kLFT", Key::Left),
        ("kNXT", Key::PageDown),
        ("kPRV", Key::PageUp),
        ("kRIT", Key::Right),
        ("kUP", Key::Up),
    ];
    for (prefix, key) in named {
        if let Some(rest) = cap.strip_prefix(prefix) {
            return match rest {
                "" => Some((key, suffix(2))),
                n => n.parse().ok().filter(|n| (3..=7).contains(n)).map(|n| (key, suffix(n))),
            };
        }
    }
    if let Some(n) = cap.strip_prefix("kf").and_then(|n| n.parse::<u8>().ok()) {
        let (base, mods) = ((n - 1) % 12 + 1, (n - 1) / 12);
        let mods = match mods {
            0 => Mods::NONE,
            1 => suffix(2),
            2 => suffix(5),
            3 => suffix(6),
            4 => suffix(3),
            5 => suffix(4),
            _ => return None,
        };
        return Some((Key::F(base), mods));
    }
    let none = Mods::NONE;
    Some(match cap {
        "kbs" => (Key::Backspace, none),
        "kcbt" => (Key::Tab, suffix(2)),
        "kcub1" => (Key::Left, none),
        "kcud1" => (Key::Down, none),
        "kcuf1" => (Key::Right, none),
        "kcuu1" => (Key::Up, none),
        "kdch1" => (Key::Delete, none),
        "kend" => (Key::End, none),
        "khome" => (Key::Home, none),
        "kich1" => (Key::Insert, none),
        "knp" => (Key::PageDown, none),
        "kpp" => (Key::PageUp, none),
        "kind" => (Key::Down, suffix(2)),
        "kri" => (Key::Up, suffix(2)),
        "kbeg" | "kp5" => (Key::Keypad(KeypadKey::Begin), none),
        "kpADD" => (Key::Keypad(KeypadKey::Add), none),
        "kpSUB" => (Key::Keypad(KeypadKey::Subtract), none),
        "kpMUL" => (Key::Keypad(KeypadKey::Multiply), none),
        "kpDIV" => (Key::Keypad(KeypadKey::Divide), none),
        "kpDOT" => (Key::Keypad(KeypadKey::Decimal), none),
        "kpCMA" => (Key::Keypad(KeypadKey::Separator), none),
        "kent" => (Key::Keypad(KeypadKey::Enter), none),
        // Keypad digits (NumLock on): ka1/ka3/kb2/kc1/kc3 are the corners and center.
        "kpZRO" => (Key::Keypad(KeypadKey::Digit(0)), none),
        "kc1" => (Key::Keypad(KeypadKey::Digit(1)), none),
        "kc2" => (Key::Keypad(KeypadKey::Digit(2)), none),
        "kc3" => (Key::Keypad(KeypadKey::Digit(3)), none),
        "kb1" => (Key::Keypad(KeypadKey::Digit(4)), none),
        "kb2" => (Key::Keypad(KeypadKey::Digit(5)), none),
        "kb3" => (Key::Keypad(KeypadKey::Digit(6)), none),
        "ka1" => (Key::Keypad(KeypadKey::Digit(7)), none),
        "ka2" => (Key::Keypad(KeypadKey::Digit(8)), none),
        "ka3" => (Key::Keypad(KeypadKey::Digit(9)), none),
        _ => return None,
    })
}

#[test]
fn xterm_256color_terminfo_is_fully_supported() {
    let Some(caps) = load() else {
        eprintln!("skipping: infocmp not available");
        return;
    };
    assert!(caps.len() > 100, "suspiciously small terminfo entry");
    // smkx: what curses programs run in.
    let keypad_mode = InputModes { app_cursor: true, app_keypad: true, ..Default::default() };
    let mut checked = (0, 0, 0);

    for (name, cap) in &caps {
        let Cap::Str(raw) = cap else { continue };
        if NOT_OUTPUT.iter().any(|(n, _)| n == name) {
            continue;
        }
        if name.starts_with('k') {
            // ---- input ----
            let expected = tput(name, &[]);
            match name.as_str() {
                "kmous" => {
                    let ev = MouseEvent {
                        button: MouseButton::Left,
                        action: MouseAction::Press,
                        col: 0,
                        row: 0,
                        px: (0, 0),
                        mods: Mods::NONE,
                    };
                    let mut t = Term::new(10, 2);
                    t.feed(&tput("XM", &["1"]));
                    let got = encode_mouse(&ev, t.modes()).unwrap();
                    assert!(got.starts_with(&expected), "kmous: {got:?}");
                }
                "kxIN" | "kxOUT" => {
                    let mut t = Term::new(10, 2);
                    t.feed(&tput("fe", &[]));
                    t.focus_changed(name == "kxIN");
                    assert_eq!(t.take_output(), expected, "{name}");
                }
                n => {
                    let (key, mods) = key_for(n).unwrap_or_else(|| panic!("key capability `{n}` ({raw}) is unmapped"));
                    let ev = KeyEvent { key, mods, text: None, action: KeyAction::Press };
                    let got = encode_key(&ev, &keypad_mode).unwrap_or_default();
                    assert_eq!(
                        String::from_utf8_lossy(&got),
                        String::from_utf8_lossy(&expected),
                        "key `{n}` ({key:?} {mods:?})"
                    );
                }
            }
            checked.1 += 1;
            continue;
        }
        // ---- output ----
        let bytes = match (params(name), manual(name)) {
            (_, Some(b)) => b,
            (Some(p), None) => tput(name, p),
            (None, None) => tput(name, &[]),
        };
        // 1. Clean consumption: nothing leaks, parser returns to ground.
        let mut t = fresh();
        t.feed(&bytes);
        let printed: String = t.screen_text().concat().chars().filter(|c| *c != ' ').collect();
        let allowed = if name == "rep" { "xxx" } else { "" };
        assert_eq!(printed, allowed, "`{name}` ({raw}) leaked text onto the screen");
        t.feed(b"\x1b[1;1H\x1b(B\x1b[0mZ");
        assert_eq!(t.screen()[0].cells[0].c, 'Z', "`{name}` left the parser in a string/escape state");
        // 2. Exact effect.
        check_effect(name, &bytes);
        checked.0 += 1;
    }

    // ---- acsc: every alternate char must be in our DEC graphics set ----
    if let Some(Cap::Str(_)) = caps.get("acsc") {
        let acsc = tput("acsc", &[]);
        for pair in acsc.chunks(2) {
            let mut t = Term::new(4, 1);
            t.feed(&tput("smacs", &[]));
            t.feed(&pair[1..2]);
            let c = t.screen()[0].cells[0].c;
            assert!(!c.is_ascii() || c == ' ', "acsc char {:?} not mapped (got {c:?})", pair[1] as char);
        }
        checked.2 = acsc.len() / 2;
    }

    // ---- response formats ----
    let mut t = fresh();
    t.feed(&tput("u7", &[]));
    let cpr = String::from_utf8(t.take_output()).unwrap();
    assert!(
        cpr.starts_with("\x1b[") && cpr.ends_with('R') && cpr[2..cpr.len() - 1].contains(';'),
        "u6 format: {cpr:?}"
    );
    t.feed(&tput("u9", &[]));
    let da = String::from_utf8(t.take_output()).unwrap();
    assert!(da.starts_with("\x1b[?") && da.ends_with('c'), "u8 format: {da:?}");

    // Numeric/boolean caps that describe behavior we must have.
    assert!(matches!(caps.get("colors"), Some(Cap::Num(256))));
    for b in ["am", "bce", "ccc", "xenl", "mir", "msgr", "npc", "km", "AX", "XT"] {
        if caps.contains_key(b) {
            assert!(matches!(caps[b], Cap::Bool), "{b}");
        }
    }
    eprintln!("terminfo audit: {} output caps, {} key caps, {} acsc chars", checked.0, checked.1, checked.2);
}

#[test]
fn boolean_capabilities_hold() {
    // am + xenl: autowrap with the "eat newline glitch" (deferred wrap).
    let mut t = Term::new(5, 2);
    t.feed(b"abcde");
    assert_eq!(t.cursor(), (0, 4));
    t.feed(b"\r\n");
    assert_eq!(t.screen_text(), ["abcde", ""]);
    // bce: erase uses the current background.
    let mut t = Term::new(5, 1);
    t.feed(b"\x1b[44m\x1b[K");
    assert_eq!(t.screen()[0].cells[4].attrs.bg, Color::Indexed(4));
    // mir: cursor motion is safe in insert mode; msgr: and with attributes on.
    let mut t = Term::new(10, 1);
    t.feed(b"\x1b[4h\x1b[7mab\x1b[1Gc\x1b[5Gd");
    assert_eq!(t.screen_text(), ["cab d"]);
    // npc: no pad char needed — NUL is ignored.
    let mut t = Term::new(5, 1);
    t.feed(b"a\0b");
    assert_eq!(t.screen_text(), ["ab"]);
}
