//! Test harness: run a real program under a PTY, emulate it with [`Term`], answer its
//! queries exactly like the GUI does, and wait on screen conditions (never on sleeps).

use accela_pty::{Command, Pty};
use accela_vt::Term;
use accela_vt::input::{InputModes, Key, KeyAction, KeyEvent, Mods, encode_key, encode_paste};
use std::io::{Read, Write};
use std::sync::mpsc;
use std::time::{Duration, Instant};

pub const TIMEOUT: Duration = Duration::from_secs(15);

pub struct Session {
    pub term: Term,
    pty: Pty,
    writer: std::fs::File,
    rx: mpsc::Receiver<Vec<u8>>,
    pub exited: bool,
    _home: TempDir,
}

/// A throwaway $HOME so programs don't read or write the user's config.
struct TempDir(std::path::PathBuf);

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

pub fn have(program: &str) -> bool {
    std::env::var_os("PATH").map(|p| std::env::split_paths(&p).any(|d| d.join(program).is_file())).unwrap_or(false)
}

/// Skip (return early) when a program isn't installed.
#[macro_export]
macro_rules! require {
    ($p:expr) => {
        if !$crate::have($p) {
            eprintln!("skipping: {} not installed", $p);
            return;
        }
    };
}

impl Session {
    pub fn spawn(program: &str, args: &[&str], cols: usize, rows: usize) -> Self {
        static N: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let home = std::env::temp_dir().join(format!(
            "accela-e2e-{}-{}",
            std::process::id(),
            N.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&home).unwrap();
        let mut cmd = Command::new(program)
            .env("HOME", home.to_str().unwrap())
            .env("XDG_CONFIG_HOME", home.join(".config").to_str().unwrap())
            .env("XDG_DATA_HOME", home.join(".local/share").to_str().unwrap())
            .env("XDG_STATE_HOME", home.join(".local/state").to_str().unwrap())
            .env("XDG_CACHE_HOME", home.join(".cache").to_str().unwrap())
            .env("LANG", "C.UTF-8")
            .env("LC_ALL", "C.UTF-8")
            .cwd(&home);
        for a in args {
            cmd = cmd.arg(*a);
        }
        let pty = Pty::spawn(&cmd, cols as u16, rows as u16).expect("spawn");
        let mut reader = pty.reader().unwrap();
        let writer = pty.writer().unwrap();
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let mut buf = [0u8; 65536];
            loop {
                match reader.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        if tx.send(buf[..n].to_vec()).is_err() {
                            break;
                        }
                    }
                }
            }
        });
        let mut term = Term::new(cols, rows);
        term.set_cell_size(10, 20);
        Session { term, pty, writer, rx, exited: false, _home: TempDir(home) }
    }

    /// Feed whatever output arrived within `wait`, answering queries.
    fn pump(&mut self, wait: Duration) {
        match self.rx.recv_timeout(wait) {
            Ok(chunk) => {
                self.term.feed(&chunk);
                while let Ok(more) = self.rx.try_recv() {
                    self.term.feed(&more);
                }
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => self.exited = true,
            Err(mpsc::RecvTimeoutError::Timeout) => {}
        }
        let out = self.term.take_output();
        if !out.is_empty() {
            let _ = self.writer.write_all(&out);
        }
        let _ = self.term.take_events();
    }

    /// Pump until `pred` holds; panics with a screen dump on timeout.
    pub fn wait_for(&mut self, what: &str, pred: impl Fn(&Term) -> bool) {
        let start = Instant::now();
        while !pred(&self.term) {
            assert!(!self.exited || pred(&self.term), "program exited while waiting for {what}\n{}", self.dump());
            assert!(start.elapsed() < TIMEOUT, "timed out waiting for {what}\n{}", self.dump());
            self.pump(Duration::from_millis(20));
        }
    }

    pub fn wait_text(&mut self, needle: &str) {
        let n = needle.to_string();
        self.wait_for(&format!("{needle:?}"), move |t| t.screen_text().iter().any(|l| l.contains(&n)));
    }

    /// Wait until the program exits (PTY EOF) and return its exit code.
    pub fn wait_exit(&mut self) -> i32 {
        let start = Instant::now();
        while !self.exited {
            assert!(start.elapsed() < TIMEOUT, "program did not exit\n{}", self.dump());
            self.pump(Duration::from_millis(20));
        }
        self.pty.wait().unwrap()
    }

    pub fn send(&mut self, bytes: &[u8]) {
        self.writer.write_all(bytes).unwrap();
    }

    /// Type text through the real key encoder (respects kitty keyboard mode etc.).
    pub fn type_text(&mut self, s: &str) {
        for c in s.chars() {
            let text = c.to_string();
            let key = match c {
                '\r' | '\n' => Key::Enter,
                '\t' => Key::Tab,
                c => Key::Char(c.to_lowercase().next().unwrap_or(c)),
            };
            let shift = c.is_uppercase();
            self.key_event(key, Mods { shift, ..Mods::NONE }, Some(&text));
        }
    }

    pub fn key(&mut self, key: Key, mods: Mods) {
        self.key_event(key, mods, None);
    }

    fn key_event(&mut self, key: Key, mods: Mods, text: Option<&str>) {
        let modes = InputModes::from_term(&self.term);
        let ev = KeyEvent { key, mods, text, action: KeyAction::Press };
        if let Some(bytes) = encode_key(&ev, &modes) {
            self.send(&bytes);
        }
        if modes.kitty_flags & accela_vt::input::kitty::REPORT_EVENTS != 0 {
            let ev = KeyEvent { action: KeyAction::Release, ..ev };
            if let Some(bytes) = encode_key(&ev, &modes) {
                self.send(&bytes);
            }
        }
    }

    pub fn paste(&mut self, s: &str) {
        let bytes = encode_paste(s, self.term.modes().bracketed_paste);
        self.send(&bytes);
    }

    pub fn resize(&mut self, cols: usize, rows: usize) {
        self.term.resize(cols, rows);
        self.pty.resize(cols as u16, rows as u16, 0, 0).unwrap();
    }

    pub fn dump(&self) -> String {
        let mut s = format!(
            "--- screen {}x{} cursor {:?} alt={} kitty={} ---\n",
            self.term.cols(),
            self.term.rows(),
            self.term.cursor(),
            self.term.modes().alt_screen,
            self.term.kitty_keyboard_flags()
        );
        for l in self.term.screen_text() {
            s.push_str(&l);
            s.push('\n');
        }
        s
    }
}
