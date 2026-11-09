//! Inline images through the full terminal: kitty graphics + sixel placement,
//! anchoring to text as it scrolls/reflows, and cleanup rules.

mod common;
use accela_vt::Term;
use accela_vt::util::base64_encode;
use common::{reply, term};

fn kitty_rgba(t: &mut Term, extra: &str, w: u32, h: u32) -> String {
    let px = base64_encode(&vec![200u8; (w * h * 4) as usize]);
    reply(t, &format!("\x1b_Ga=T,f=32,s={w},v={h},{extra};{px}\x1b\\"))
}

fn setup() -> Term {
    let mut t = term(40, 10);
    t.set_cell_size(10, 20);
    t
}

#[test]
fn kitty_image_is_placed_at_cursor_and_moves_it() {
    let mut t = setup();
    t.feed(b"ab");
    let r = kitty_rgba(&mut t, "i=1", 30, 40); // 3 cols x 2 rows
    assert_eq!(r, "\x1b_Gi=1;OK\x1b\\");
    let v = t.visible_placements();
    assert_eq!(v.len(), 1);
    let (row, p) = v[0];
    assert_eq!((row, p.col, p.cols, p.rows), (0, 2, 3, 2));
    assert_eq!(t.cursor(), (1, 5));
    assert_eq!(t.image(1).unwrap().width, 30);
}

#[test]
fn cursor_stays_with_c1() {
    let mut t = setup();
    kitty_rgba(&mut t, "i=1,C=1", 30, 40);
    assert_eq!(t.cursor(), (0, 0));
}

#[test]
fn image_scrolls_with_text_and_is_pruned_with_history() {
    let mut t = accela_vt::Term::with_scrollback(40, 5, 3);
    t.set_cell_size(10, 20);
    kitty_rgba(&mut t, "i=1", 10, 20);
    t.feed(b"\r\n1\r\n2");
    assert_eq!(t.visible_placements()[0].0, 0);
    t.feed(b"\r\n3\r\n4\r\n5");
    // Scrolled into history: not visible, but still anchored.
    assert!(t.visible_placements().is_empty());
    t.scroll_display(1);
    assert_eq!(t.visible_placements()[0].0, 0);
    t.scroll_to_bottom();
    // Push it out of the 3-line scrollback: the placement is dropped.
    t.feed(b"\r\n6\r\n7\r\n8\r\n9");
    t.scroll_display(100);
    assert!(t.visible_placements().is_empty());
}

#[test]
fn clear_screen_and_reset_remove_images() {
    let mut t = setup();
    kitty_rgba(&mut t, "i=1", 10, 20);
    t.feed(b"\x1b[2J");
    assert!(t.visible_placements().is_empty());
    kitty_rgba(&mut t, "i=2", 10, 20);
    t.feed(b"\x1bc");
    assert!(t.visible_placements().is_empty());
    assert!(t.image(2).is_none());
}

#[test]
fn alt_screen_images_are_separate() {
    let mut t = setup();
    kitty_rgba(&mut t, "i=1", 10, 20);
    t.feed(b"\x1b[?1049h");
    assert!(t.visible_placements().is_empty());
    kitty_rgba(&mut t, "i=2", 10, 20);
    assert_eq!(t.visible_placements()[0].1.image_id, 2);
    t.feed(b"\x1b[?1049l");
    assert_eq!(t.visible_placements()[0].1.image_id, 1);
}

#[test]
fn reflow_keeps_image_on_its_line() {
    let mut t = setup();
    t.feed(b"0123456789012345678901234567890123456789abc\r\nline2\r\n");
    kitty_rgba(&mut t, "i=1,C=1", 10, 20);
    // 43 chars at 40 columns take two rows; "line2" is row 2; the image is on row 3.
    assert_eq!(t.visible_placements()[0].0, 3);
    t.resize(20, 10);
    // The first logical line now takes 3 rows, so the image moves down one.
    assert_eq!(t.visible_placements()[0].0, 4);
}

#[test]
fn delete_by_id() {
    let mut t = setup();
    kitty_rgba(&mut t, "i=1", 10, 20);
    kitty_rgba(&mut t, "i=2", 10, 20);
    t.feed(b"\x1b_Ga=d,d=i,i=1\x1b\\");
    let ids: Vec<u32> = t.visible_placements().iter().map(|(_, p)| p.image_id).collect();
    assert_eq!(ids, [2]);
}

#[test]
fn sixel_image_is_placed_and_cursor_moves_below() {
    let mut t = setup();
    t.feed(b"x");
    // 2x12 pixels: two sixel rows of a red column pair.
    t.feed(b"\x1bP0;1q#1;2;100;0;0#1!2~-!2~\x1b\\");
    let v = t.visible_placements();
    assert_eq!(v.len(), 1);
    let (row, p) = v[0];
    assert_eq!((row, p.col, p.cols, p.rows), (0, 1, 1, 1));
    assert_eq!(t.cursor(), (1, 1));
    let img = t.image(p.image_id).unwrap();
    assert_eq!((img.width, img.height), (2, 12));
}

#[test]
fn graphics_queries() {
    let mut t = setup();
    assert_eq!(reply(&mut t, "\x1b[?1;1;0S"), "\x1b[?1;0;256S");
    assert_eq!(reply(&mut t, "\x1b[?2;1;0S"), "\x1b[?2;0;400;200S");
    let px = base64_encode(&[0u8; 3]);
    assert_eq!(reply(&mut t, &format!("\x1b_Ga=q,i=31,s=1,v=1,f=24;{px}\x1b\\")), "\x1b_Gi=31;OK\x1b\\");
    assert!(t.image(31).is_none());
}

#[test]
fn oversized_apc_is_ignored() {
    let mut t = setup();
    t.set_image_limits(1024, 4096);
    let huge = "A".repeat(10_000);
    t.feed(format!("\x1b_Ga=T,f=32,s=1,v=1,i=9;{huge}\x1b\\").as_bytes());
    assert!(t.image(9).is_none());
    assert_eq!(reply(&mut t, "\x1b[5n"), "\x1b[0n");
}
