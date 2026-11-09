//! Throughput and memory benchmark for the emulator core.
//!
//!     cargo run --release -p lain-vt --example bench
//!
//! Each workload is generated once, then parsed repeatedly; we report MB/s.

use lain_vt::Term;
use std::time::Instant;

fn ascii_lines(n: usize) -> Vec<u8> {
    let mut v = Vec::new();
    for i in 0..n {
        v.extend_from_slice(
            format!("{i:>8} the quick brown fox jumps over the lazy dog 0123456789 abcdefghijklmnop\r\n").as_bytes(),
        );
    }
    v
}

/// Very short lines (`seq`): per-line costs dominate.
fn short_lines(n: usize) -> Vec<u8> {
    let mut v = Vec::new();
    for i in 0..n {
        v.extend_from_slice(format!("{i}\r\n").as_bytes());
    }
    v
}

fn colored_lines(n: usize) -> Vec<u8> {
    let mut v = Vec::new();
    for i in 0..n {
        v.extend_from_slice(
            format!(
                "\x1b[38;5;{}m{i:>6}\x1b[0m \x1b[1;32mINFO\x1b[0m \x1b[38;2;{};{};200mmodule::path\x1b[0m message with \x1b[4mstyle\x1b[24m and more text\r\n",
                i % 256,
                i % 200,
                (i * 7) % 256
            )
            .as_bytes(),
        );
    }
    v
}

fn unicode_lines(n: usize) -> Vec<u8> {
    let mut v = Vec::new();
    for i in 0..n {
        v.extend_from_slice(
            format!("{i} ação café naïve 日本語のテキスト 🦀🚀 ─│┌┐└┘ e\u{301} Ελληνικά русский\r\n").as_bytes(),
        );
    }
    v
}

/// Full-screen redraws with absolute cursor positioning (like vim/htop).
fn cursor_heavy(frames: usize, cols: usize, rows: usize) -> Vec<u8> {
    let mut v = Vec::new();
    for f in 0..frames {
        v.extend_from_slice(b"\x1b[?2026h\x1b[H");
        for r in 0..rows {
            v.extend_from_slice(format!("\x1b[{};1H\x1b[{}m", r + 1, 31 + (r + f) % 7).as_bytes());
            for c in 0..cols {
                v.push(b'a' + ((r + c + f) % 26) as u8);
            }
        }
        v.extend_from_slice(b"\x1b[0m\x1b[?2026l");
    }
    v
}

fn bench(name: &str, data: &[u8], iters: usize) {
    let mut t = Term::new(200, 50);
    t.feed(data); // warm up
    let start = Instant::now();
    for _ in 0..iters {
        t.feed(data);
        std::hint::black_box(t.cursor());
    }
    let secs = start.elapsed().as_secs_f64();
    let mb = (data.len() * iters) as f64 / (1024.0 * 1024.0);
    println!("{name:<16} {:>8.1} MB/s   ({mb:.0} MB in {secs:.2}s)", mb / secs);
}

fn main() {
    println!("lain-vt throughput (200x50, 10k scrollback):");
    bench("ascii", &ascii_lines(100_000), 5);
    bench("short lines", &short_lines(1_000_000), 3);
    bench("colored", &colored_lines(100_000), 5);
    bench("unicode", &unicode_lines(50_000), 5);
    bench("cursor/redraw", &cursor_heavy(500, 200, 50), 5);

    // Memory: full scrollback of typical 80-column lines.
    let mut t = Term::new(200, 50);
    t.feed(&ascii_lines(20_000));
    let cells: usize = t.scrollback().iter().map(|r| r.cells.capacity()).sum::<usize>()
        + t.screen().iter().map(|r| r.cells.capacity()).sum::<usize>();
    let bytes = cells * std::mem::size_of::<lain_vt::Cell>();
    println!(
        "memory: {} scrollback rows ≈ {:.1} MB of cells ({} B/cell, trailing blanks trimmed)",
        t.scrollback().len(),
        bytes as f64 / (1024.0 * 1024.0),
        std::mem::size_of::<lain_vt::Cell>()
    );
}
