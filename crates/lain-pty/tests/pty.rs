//! Real-process PTY tests. Each runs a short /bin/sh script; no sleeps are used for
//! synchronization (we wait on output or EOF), so they're fast and deterministic.

use lain_pty::{Command, Pty};
use std::io::{Read, Write};
use std::sync::mpsc;
use std::time::Duration;

const TIMEOUT: Duration = Duration::from_secs(10);

/// Read everything until EOF (or EIO, which Linux returns once the child is gone).
fn read_all(pty: &Pty) -> String {
    let mut r = pty.reader().unwrap();
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut out = Vec::new();
        let mut buf = [0u8; 4096];
        loop {
            match r.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => out.extend_from_slice(&buf[..n]),
            }
        }
        let _ = tx.send(out);
    });
    String::from_utf8_lossy(&rx.recv_timeout(TIMEOUT).expect("child output timed out")).into_owned()
}

/// Read until `needle` appears.
fn read_until(r: &mut std::fs::File, needle: &str) -> String {
    let mut out = Vec::new();
    let mut buf = [0u8; 1024];
    let start = std::time::Instant::now();
    while !String::from_utf8_lossy(&out).contains(needle) {
        assert!(start.elapsed() < TIMEOUT, "timed out waiting for {needle:?}, got {:?}", String::from_utf8_lossy(&out));
        let n = r.read(&mut buf).expect("read");
        assert!(n > 0, "EOF before {needle:?}");
        out.extend_from_slice(&buf[..n]);
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn sh(script: &str) -> Command {
    Command::new("/bin/sh").arg("-c").arg(script)
}

#[test]
fn runs_command_and_captures_output() {
    let mut p = Pty::spawn(&sh("printf 'hello pty'"), 80, 24).unwrap();
    assert!(read_all(&p).contains("hello pty"));
    assert_eq!(p.wait().unwrap(), 0);
}

#[test]
fn exit_status_is_reported() {
    let mut p = Pty::spawn(&sh("exit 3"), 80, 24).unwrap();
    read_all(&p);
    assert_eq!(p.wait().unwrap(), 3);
}

#[test]
fn killed_child_reports_signal() {
    let mut p = Pty::spawn(&sh("kill -9 $$"), 80, 24).unwrap();
    read_all(&p);
    assert_eq!(p.wait().unwrap(), 128 + 9);
}

#[test]
fn terminal_environment() {
    let p = Pty::spawn(&sh("echo \"[$TERM][$COLORTERM][$TERM_PROGRAM][$EXTRA]\"").env("EXTRA", "x"), 80, 24).unwrap();
    assert!(read_all(&p).contains("[xterm-256color][truecolor][lain][x]"));
}

#[test]
fn has_controlling_tty_and_size() {
    let p = Pty::spawn(&sh("tty >/dev/null && echo has-tty; stty size"), 100, 30).unwrap();
    let out = read_all(&p);
    assert!(out.contains("has-tty"), "{out}");
    assert!(out.contains("30 100"), "{out}");
}

#[test]
fn resize_reaches_the_child() {
    let p = Pty::spawn(&sh("read x; stty size"), 80, 24).unwrap();
    p.resize(132, 43, 0, 0).unwrap();
    assert_eq!(p.size().unwrap(), (132, 43));
    p.writer().unwrap().write_all(b"\n").unwrap();
    assert!(read_all(&p).contains("43 132"));
}

#[test]
fn input_round_trip() {
    let p = Pty::spawn(&sh("read line; echo \"got:$line\""), 80, 24).unwrap();
    p.writer().unwrap().write_all("çã 日本\n".as_bytes()).unwrap();
    assert!(read_all(&p).contains("got:çã 日本"));
}

#[test]
fn cwd_is_applied() {
    let p = Pty::spawn(&sh("pwd").cwd("/tmp"), 80, 24).unwrap();
    assert!(read_all(&p).contains("/tmp"));
}

#[test]
fn parent_fds_do_not_leak() {
    // An fd opened *without* O_CLOEXEC must still not reach the child.
    let fd = unsafe { libc::open(c"/dev/null".as_ptr(), libc::O_RDONLY) };
    assert!(fd > 2);
    let p = Pty::spawn(&sh(&format!("[ -e /proc/$$/fd/{fd} ] && echo LEAKED || echo clean")), 80, 24).unwrap();
    let out = read_all(&p);
    unsafe { libc::close(fd) };
    assert!(out.contains("clean"), "{out}");
}

#[test]
fn signal_mask_is_cleared() {
    // Block SIGINT in this thread; the child must not inherit it.
    unsafe {
        let mut set: libc::sigset_t = std::mem::zeroed();
        libc::sigemptyset(&mut set);
        libc::sigaddset(&mut set, libc::SIGINT);
        libc::pthread_sigmask(libc::SIG_BLOCK, &set, std::ptr::null_mut());
    }
    let p = Pty::spawn(&sh("grep SigBlk /proc/self/status"), 80, 24).unwrap();
    assert!(read_all(&p).contains("0000000000000000"));
}

#[test]
fn missing_program_is_an_error() {
    let err = Pty::spawn(&Command::new("definitely-not-a-real-program-lain"), 80, 24).err().unwrap();
    assert_eq!(err.kind(), std::io::ErrorKind::NotFound);
}

#[test]
fn nul_in_argument_is_rejected() {
    assert!(Pty::spawn(&sh("echo a\0b"), 80, 24).is_err());
}

#[test]
fn ctrl_c_interrupts_foreground_job() {
    let p = Pty::spawn(&sh("echo ready; sleep 30; echo not-interrupted"), 80, 24).unwrap();
    let mut r = p.reader().unwrap();
    read_until(&mut r, "ready");
    p.writer().unwrap().write_all(&[0x03]).unwrap();
    let mut rest = Vec::new();
    let _ = r.read_to_end(&mut rest);
    assert!(!String::from_utf8_lossy(&rest).contains("not-interrupted"));
}

#[test]
fn drop_terminates_and_reaps_child() {
    let p = Pty::spawn(&sh("echo up; sleep 30"), 80, 24).unwrap();
    let mut r = p.reader().unwrap();
    read_until(&mut r, "up");
    let pid = p.pid();
    drop(p);
    // The reaper thread collects the child shortly after SIGHUP.
    let start = std::time::Instant::now();
    loop {
        let gone = unsafe { libc::kill(pid, 0) } != 0;
        if gone {
            break;
        }
        assert!(start.elapsed() < TIMEOUT, "child {pid} still alive (or zombie) after drop");
        std::thread::yield_now();
    }
}
