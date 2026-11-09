//! Deterministic fuzzing: random byte streams, resizes and UI calls must never panic
//! and must keep every structural invariant. Seeds are fixed, so failures reproduce.
//! Scale with `LAIN_FUZZ_CASES=<n>` (default keeps the suite fast).

use lain_vt::input::{InputModes, Key, KeyAction, KeyEvent, Mods, encode_key, encode_paste};
use lain_vt::mouse::{MouseAction, MouseButton, MouseEvent, encode_mouse};
use lain_vt::selection::{Point, Selection, SelectionKind};
use lain_vt::{Term, flags};

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        // xorshift64*
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_f491_4f6c_dd1d)
    }
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n.max(1)
    }
    fn pick<'a, T>(&mut self, xs: &'a [T]) -> &'a T {
        &xs[self.below(xs.len() as u64) as usize]
    }
}

const FINALS: &[u8] = b"ABCDEFGHIJKLMPSTXZ@`abcdefghlmnqrstu";
const PRIVATE: &[&[u8]] = &[b"", b"?", b">", b"<", b"=", b"?$", b"$", b" ", b"!"];
const MODES: &[u16] =
    &[1, 4, 5, 6, 7, 12, 20, 25, 47, 66, 69, 1000, 1002, 1003, 1004, 1006, 1047, 1048, 1049, 2004, 2026];
const TEXT: &[&str] = &["a", "Z", " ", "é", "日", "本", "🦀", "\u{301}", "\u{200d}", "ab", "─", "\u{fe0f}", "\u{ffff}"];

fn gen_chunk(r: &mut Rng, out: &mut Vec<u8>) {
    match r.below(16) {
        0..=4 => {
            for _ in 0..r.below(40) {
                out.extend_from_slice(r.pick(TEXT).as_bytes());
            }
        }
        5 => out.push(*r.pick(&[0x07, 0x08, 0x09, 0x0a, 0x0b, 0x0c, 0x0d, 0x0e, 0x0f, 0x18, 0x1a, 0x7f])),
        6..=8 => {
            out.extend_from_slice(b"\x1b[");
            out.extend_from_slice(r.pick(PRIVATE));
            for i in 0..r.below(5) {
                if i > 0 {
                    out.push(if r.below(6) == 0 { b':' } else { b';' });
                }
                let v = match r.below(4) {
                    0 => r.below(3),
                    1 => r.below(300),
                    2 => *r.pick(MODES) as u64,
                    _ => r.below(100_000),
                };
                out.extend_from_slice(v.to_string().as_bytes());
            }
            out.push(*r.pick(FINALS));
        }
        9 => {
            // SGR with extended colors.
            let s = *r.pick(&[
                "\x1b[38;5;196m",
                "\x1b[48;2;1;2;3m",
                "\x1b[4:3m",
                "\x1b[58:2::9:9:9m",
                "\x1b[0m",
                "\x1b[1;7m",
            ]);
            out.extend_from_slice(s.as_bytes());
        }
        10 => {
            let s = *r.pick(&[
                "\x1b]0;title\x07",
                "\x1b]8;;https://x.y\x1b\\",
                "\x1b]8;;\x07",
                "\x1b]52;c;aGk=\x07",
                "\x1b]4;3;?\x07",
                "\x1b]11;?\x1b\\",
                "\x1b]133;A\x07",
                "\x1b]7;file:///tmp\x07",
                "\x1b]104\x07",
                "\x1b]",
            ]);
            out.extend_from_slice(s.as_bytes());
        }
        11 => {
            let s = *r.pick(&[
                "\x1bP$qm\x1b\\",
                "\x1b_Ga=T,f=24,s=1,v=1,i=1;AAAA\x1b\\",
                "\x1b_Ga=T,f=32,s=2,v=1,c=3,r=2;AAAAAAAAAAA=\x1b\\",
                "\x1b_Ga=t,f=24,s=1,v=1,i=2,m=1;AA\x1b\\\x1b_Gm=0;AA\x1b\\",
                "\x1b_Ga=p,i=1,C=1\x1b\\",
                "\x1b_Ga=d,d=A\x1b\\",
                "\x1bP0;1q\"1;1;8;12#1;2;100;0;0!8~-#2~~\x1b\\",
                "\x1bPq#9;1;120;50;100!3?$-!9999~\x1b\\",
                "\x1bP+q544e\x1b\\",
                "\x1bPq#0;2;0;0;0\x1b\\",
                "\x1b_Gf=24;AAAA\x1b\\",
                "\x1bP",
                "\x1b_",
            ]);
            out.extend_from_slice(s.as_bytes());
        }
        12 => {
            out.push(0x1b);
            out.push(*r.pick(b"78DEHMNOcno=>#(0)0*0+B"));
        }
        13 => {
            // Raw garbage, including invalid UTF-8.
            for _ in 0..r.below(16) {
                out.push(r.below(256) as u8);
            }
        }
        14 => {
            if r.below(2) == 0 {
                out.extend_from_slice(b"\x1b(0lqqk\x1b(B");
            } else {
                // Left/right margins, often with scrolling right after.
                let (l, w) = (1 + r.below(20), 1 + r.below(30));
                out.extend_from_slice(format!("\x1b[?69h\x1b[{l};{}s", l + w).as_bytes());
            }
        }
        _ => out.extend_from_slice(b"\r\n"),
    }
}

fn check_invariants(t: &Term, ctx: &str) {
    let (cols, rows) = (t.cols(), t.rows());
    assert_eq!(t.screen().len(), rows, "{ctx}: screen height");
    let (cr, cc) = t.cursor();
    assert!(cr < rows && cc < cols, "{ctx}: cursor ({cr},{cc}) out of {cols}x{rows}");
    assert!(t.scrollback().len() <= t.scrollback_limit(), "{ctx}: scrollback limit");
    assert!(t.display_offset() <= t.scrollback().len(), "{ctx}: display offset");
    for (i, row) in t.screen().iter().enumerate() {
        assert_eq!(row.len(), cols, "{ctx}: row {i} width");
        for (c, cell) in row.cells.iter().enumerate() {
            if cell.attrs.has(flags::WIDE) {
                assert!(
                    c + 1 < cols && row.cells[c + 1].attrs.has(flags::WIDE_SPACER),
                    "{ctx}: wide char at row {i} col {c} without spacer"
                );
            }
            if cell.attrs.has(flags::WIDE_SPACER) {
                assert!(
                    (c > 0 && row.cells[c - 1].attrs.has(flags::WIDE)) || c + 1 == cols,
                    "{ctx}: orphan spacer at row {i} col {c}"
                );
            }
        }
        for (c, _) in &row.combining {
            assert!((*c as usize) < cols, "{ctx}: combining past end on row {i}");
        }
        let d = row.dirty_end();
        assert!(
            row.cells[d..].iter().all(|c| *c == lain_vt::Cell::default()),
            "{ctx}: row {i} has content past its high-water mark {d}"
        );
    }
    for i in 0..rows {
        let _ = t.visible_row(i);
    }
}

fn run_case(seed: u64, steps: usize) {
    let mut r = Rng(seed.wrapping_mul(0x9e37_79b9_7f4a_7c15) | 1);
    let mut t = Term::with_scrollback(1 + r.below(100) as usize, 1 + r.below(40) as usize, r.below(200) as usize);
    let mut buf = Vec::new();
    for step in 0..steps {
        buf.clear();
        for _ in 0..1 + r.below(8) {
            gen_chunk(&mut r, &mut buf);
        }
        // Split at a random point to exercise state carried across feeds.
        let cut = r.below(buf.len() as u64 + 1) as usize;
        if std::env::var_os("LAIN_FUZZ_BYTEWISE").is_some() {
            // Debug aid: pinpoint the exact byte that breaks an invariant.
            for (i, b) in buf.iter().enumerate() {
                t.feed(&[*b]);
                check_invariants(
                    &t,
                    &format!("seed {seed} step {step} byte {i} of {:?}", String::from_utf8_lossy(&buf[..=i])),
                );
            }
        } else {
            t.feed(&buf[..cut]);
            t.feed(&buf[cut..]);
        }
        let _ = t.take_output();
        let _ = t.take_events();
        check_invariants(&t, &format!("seed {seed} step {step} after feeding {:?}", String::from_utf8_lossy(&buf)));

        let op = r.below(20);
        match op {
            0 => t.resize(1 + r.below(120) as usize, 1 + r.below(50) as usize),
            1 => t.scroll_display(r.below(50) as isize - 25),
            2 => {
                let lines = t.line_id_range();
                let p = |r: &mut Rng| Point {
                    line: lines.0 + r.below(lines.1 - lines.0 + 2),
                    col: r.below(t.cols() as u64 + 2) as usize,
                };
                let kind =
                    *r.pick(&[SelectionKind::Simple, SelectionKind::Word, SelectionKind::Line, SelectionKind::Block]);
                let mut s = Selection::new(kind, p(&mut r));
                s.update(p(&mut r));
                if let Some(range) = s.range(&t) {
                    let _ = range.text(&t);
                }
            }
            3 => {
                let modes = InputModes::from_term(&t);
                let key = *r.pick(&[Key::Char('a'), Key::Enter, Key::Up, Key::F(3), Key::F(24), Key::Escape, Key::Tab]);
                let mods = Mods { shift: r.below(2) == 0, alt: r.below(2) == 0, ctrl: r.below(2) == 0, super_: false };
                let action = *r.pick(&[KeyAction::Press, KeyAction::Repeat, KeyAction::Release]);
                let _ = encode_key(&KeyEvent { key, mods, text: Some("A"), action }, &modes);
                let _ = encode_paste("x\x1b[201~y\n", t.modes().bracketed_paste);
                let ev = MouseEvent {
                    button: MouseButton::Left,
                    action: MouseAction::Motion,
                    col: r.below(300) as usize,
                    row: r.below(300) as usize,
                    px: (0, 0),
                    mods,
                };
                let _ = encode_mouse(&ev, t.modes());
            }
            _ => {}
        }
        check_invariants(&t, &format!("seed {seed} step {step} after op {op}"));
    }
}

#[test]
fn random_streams_keep_invariants() {
    let cases: u64 = std::env::var("LAIN_FUZZ_CASES").ok().and_then(|v| v.parse().ok()).unwrap_or(150);
    for seed in 0..cases {
        run_case(seed, 300);
    }
}

#[test]
fn every_single_byte_after_every_prefix_state() {
    // Drive the parser into each interesting state, then hit it with every byte value.
    let prefixes: &[&[u8]] = &[
        b"",
        b"\x1b",
        b"\x1b[",
        b"\x1b[?",
        b"\x1b[1;",
        b"\x1b]",
        b"\x1b]0;",
        b"\x1bP",
        b"\x1bP$q",
        b"\x1b_",
        b"\x1b(",
        b"\xe6",
        b"\xf0\x9f",
    ];
    for p in prefixes {
        for b in 0..=255u8 {
            let mut t = Term::new(10, 3);
            t.feed(p);
            t.feed(&[b]);
            t.feed(b"x");
            check_invariants(&t, &format!("prefix {p:?} byte {b:#x}"));
        }
    }
}
