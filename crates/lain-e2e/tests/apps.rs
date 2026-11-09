//! Real applications must render and respond correctly. Each test skips when the
//! program isn't installed and waits on screen state, so the suite stays fast.

use lain_e2e::{Session, require};
use lain_vt::input::{Key, Mods};

const CTRL: Mods = Mods { ctrl: true, ..Mods::NONE };

#[test]
fn tput_cursor_and_attributes() {
    require!("tput");
    let mut s = Session::spawn(
        "/bin/sh",
        &[
            "-c",
            "tput clear; tput cup 5 10; printf X; tput cup 7 0; tput bold; tput setaf 1; printf R; tput sgr0; printf N; read x",
        ],
        40,
        10,
    );
    s.wait_text("RN");
    assert_eq!(s.term.screen_text()[5], format!("{}X", " ".repeat(10)));
    let r = s.term.screen()[7].cells[0];
    assert!(r.attrs.has(lain_vt::flags::BOLD));
    assert_eq!(r.attrs.fg, lain_vt::Color::Indexed(1));
    assert_eq!(s.term.screen()[7].cells[1].attrs, lain_vt::Attrs::default());
    s.send(b"\n");
    s.wait_exit();
}

#[test]
fn bash_readline_and_bracketed_paste() {
    require!("bash");
    let mut s = Session::spawn("bash", &["--norc", "--noprofile", "-i"], 60, 10);
    s.type_text("PS1='lain$ '\r");
    s.wait_text("lain$");
    s.type_text("echo $((6*7))\r");
    s.wait_text("42");
    // Readline enables bracketed paste; a pasted newline must not execute by itself.
    s.wait_for("bracketed paste", |t| t.modes().bracketed_paste);
    s.paste("echo pasted-$((1+1))\n");
    s.type_text("\r");
    s.wait_text("pasted-2");
    s.type_text("exit\r");
    assert_eq!(s.wait_exit(), 0);
}

#[test]
fn fish_starts_without_query_timeouts() {
    require!("fish");
    let start = std::time::Instant::now();
    let mut s = Session::spawn("fish", &["--no-config", "-i"], 80, 12);
    s.type_text("echo lain-(math 40 + 2)\r");
    s.wait_text("lain-42");
    // fish waits for terminal replies at startup; unanswered queries cost seconds.
    assert!(start.elapsed().as_secs() < 3, "fish took {:?}", start.elapsed());
    let screen = s.term.screen_text().join("\n");
    assert!(!screen.contains("Device Attribute"), "{screen}");
    s.type_text("exit\r");
    s.wait_exit();
}

#[test]
fn vim_alt_screen_edit_and_quit() {
    require!("vim");
    let mut s = Session::spawn("vim", &["-u", "NONE", "-N", "-i", "NONE"], 60, 12);
    s.wait_for("alt screen", |t| t.modes().alt_screen);
    s.wait_text("~");
    s.type_text("ihello lain");
    s.key(Key::Escape, Mods::NONE);
    s.wait_for("text on row 0", |t| t.screen_text()[0].starts_with("hello lain"));
    s.type_text(":q!\r");
    s.wait_exit();
    assert!(!s.term.modes().alt_screen);
}

#[test]
fn neovim_with_modern_protocols() {
    require!("nvim");
    let mut s = Session::spawn("nvim", &["--clean"], 70, 14);
    s.wait_for("alt screen", |t| t.modes().alt_screen);
    s.wait_text("~");
    s.type_text("iUnicode: çãé 日本 🦀");
    s.key(Key::Escape, Mods::NONE);
    s.wait_text("Unicode: çãé 日本 🦀");
    // Ctrl-based mapping still works whatever keyboard protocol nvim negotiated.
    s.key(Key::Char('a'), CTRL);
    s.type_text(":q!\r");
    s.wait_exit();
}

#[test]
fn less_paging_and_resize() {
    require!("less");
    let mut s = Session::spawn("/bin/sh", &["-c", "seq 1 500 | less"], 40, 10);
    s.wait_for("first page", |t| t.screen_text()[0] == "1");
    s.type_text("G");
    s.wait_text("500");
    s.resize(50, 20);
    // less drops typeahead while handling SIGWINCH: wait for the redraw first.
    s.wait_for("redraw at new size", |t| t.screen_text()[19] == "(END)");
    s.type_text("g");
    s.wait_for("top after resize", |t| t.screen_text()[0] == "1" && t.screen_text().iter().any(|l| l == "19"));
    s.type_text("q");
    s.wait_exit();
}

#[test]
fn htop_draws_and_quits() {
    require!("htop");
    let mut s = Session::spawn("htop", &[], 100, 30);
    s.wait_text("Load average");
    s.type_text("q");
    s.wait_exit();
}

#[test]
fn btop_truecolor_box_drawing() {
    require!("btop");
    let mut s = Session::spawn("btop", &["--force-utf"], 120, 40);
    s.wait_for("box drawing", |t| t.screen_text().iter().any(|l| l.contains('─')));
    s.type_text("q");
    s.wait_exit();
}

#[test]
fn nano_edit_prompt() {
    require!("nano");
    let mut s = Session::spawn("nano", &["--ignorercfiles"], 60, 12);
    s.wait_text("GNU nano");
    s.type_text("hi");
    s.key(Key::Char('x'), CTRL);
    s.wait_text("Save modified buffer");
    s.type_text("n");
    s.wait_exit();
}

#[test]
fn python_curses_colors_and_acs() {
    require!("python3");
    let script = r#"
import curses
def main(w):
    curses.start_color(); curses.use_default_colors()
    curses.init_pair(1, curses.COLOR_RED, -1)
    w.border()
    w.addstr(1, 2, "colored", curses.color_pair(1) | curses.A_BOLD)
    w.refresh(); w.getch()
curses.wrapper(main)
"#;
    let mut s = Session::spawn("python3", &["-c", script], 30, 6);
    // Output arrives in chunks: wait for the whole frame (text + both border corners).
    s.wait_for("full curses frame", |t| {
        let grid = t.screen_text().join("\n");
        grid.contains("colored") && grid.contains('┌') && grid.contains('┘')
    });
    let c = s.term.screen()[1].cells[2];
    assert_eq!(c.attrs.fg, lain_vt::Color::Indexed(1));
    s.type_text("x");
    assert_eq!(s.wait_exit(), 0);
    assert!(!s.term.modes().alt_screen);
}

#[test]
fn git_log_graph_with_pager() {
    require!("git");
    require!("less");
    let mut s = Session::spawn(
        "/bin/sh",
        &[
            "-c",
            "git init -q r && cd r && git -c user.name=t -c user.email=t@t commit -q --allow-empty -m first && git -c user.name=t -c user.email=t@t commit -q --allow-empty -m second && git -c color.ui=always log --graph --oneline",
        ],
        60,
        10,
    );
    s.wait_text("second");
    s.type_text("q");
    s.wait_exit();
}
