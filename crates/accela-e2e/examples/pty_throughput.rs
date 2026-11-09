//! The PTY ceiling: how fast the kernel delivers a program's output to *any* terminal,
//! with and without accela-vt parsing it. Compare the app against these numbers.
//!
//!     cargo run --release -p accela-e2e --example pty_throughput -- seq 1 3000000
//!     cargo run --release -p accela-e2e --example pty_throughput -- cat big.txt

use accela_pty::{Command, Pty};
use std::io::Read;

fn run(argv: &[String], feed: bool) {
    let cmd = argv[1..].iter().fold(Command::new(argv[0].clone()), |c, a| c.arg(a.clone()));
    let p = Pty::spawn(&cmd, 200, 50).expect("spawn");
    let mut r = p.reader().expect("reader");
    let mut t = accela_vt::Term::new(200, 50);
    let mut buf = vec![0u8; 256 * 1024];
    let (mut total, mut reads) = (0usize, 0usize);
    let start = std::time::Instant::now();
    loop {
        match r.read(&mut buf) {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                total += n;
                reads += 1;
                if feed {
                    t.feed(&buf[..n]);
                }
            }
        }
    }
    let secs = start.elapsed().as_secs_f64();
    println!(
        "{:<12} {:>6.1} MB in {:>6.0} ms = {:>5.0} MB/s ({} reads, avg {} B)",
        if feed { "pty + parse" } else { "pty only" },
        total as f64 / 1e6,
        secs * 1e3,
        total as f64 / 1e6 / secs,
        reads,
        total / reads.max(1)
    );
}

fn main() {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    let argv = if argv.is_empty() { vec!["seq".into(), "1".into(), "3000000".into()] } else { argv };
    run(&argv, false);
    run(&argv, true);
}
