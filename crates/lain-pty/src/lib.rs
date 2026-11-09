//! lain-pty: minimal, hardened Unix PTY spawning on top of libc.
//!
//! Security notes:
//! - Every fd we create is `O_CLOEXEC`, and the child additionally closes every fd
//!   above 2 before `exec`, so no parent descriptor (sockets, GPU, files) leaks into
//!   the shell.
//! - The child gets a clean signal mask and default dispositions.
//! - Everything that allocates happens before `fork`; the child only performs
//!   async-signal-safe calls.

use std::ffi::{CString, OsStr};
use std::fs::File;
use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::ffi::OsStrExt;
use std::path::PathBuf;
use std::time::Duration;

/// What to run inside the PTY.
#[derive(Clone, Debug)]
pub struct Command {
    pub program: String,
    pub args: Vec<String>,
    pub cwd: Option<PathBuf>,
    /// Extra environment variables (set after the built-in TERM variables).
    pub env: Vec<(String, String)>,
}

impl Command {
    pub fn new(program: impl Into<String>) -> Self {
        Self { program: program.into(), args: Vec::new(), cwd: None, env: Vec::new() }
    }

    pub fn arg(mut self, a: impl Into<String>) -> Self {
        self.args.push(a.into());
        self
    }

    pub fn cwd(mut self, dir: impl Into<PathBuf>) -> Self {
        self.cwd = Some(dir.into());
        self
    }

    pub fn env(mut self, k: impl Into<String>, v: impl Into<String>) -> Self {
        self.env.push((k.into(), v.into()));
        self
    }

    /// The user's shell from `$SHELL`, falling back to `/bin/sh`.
    pub fn default_shell() -> Self {
        Self::new(std::env::var("SHELL").ok().filter(|s| !s.is_empty()).unwrap_or_else(|| "/bin/sh".into()))
    }
}

pub struct Pty {
    master: OwnedFd,
    pid: libc::pid_t,
    reaped: bool,
}

fn cvt(r: libc::c_int) -> io::Result<libc::c_int> {
    if r < 0 { Err(io::Error::last_os_error()) } else { Ok(r) }
}

fn cstr(s: &[u8]) -> io::Result<CString> {
    CString::new(s).map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "nul byte in argument"))
}

fn winsize(cols: u16, rows: u16, px_w: u16, px_h: u16) -> libc::winsize {
    libc::winsize { ws_row: rows, ws_col: cols, ws_xpixel: px_w, ws_ypixel: px_h }
}

/// Close every fd >= 3. Async-signal-safe.
unsafe fn close_fds_from_3() {
    // close_range(2) is available on Linux >= 5.9; fall back to a bounded loop.
    let r = unsafe { libc::syscall(libc::SYS_close_range, 3u32, u32::MAX, 0u32) };
    if r != 0 {
        let max = unsafe { libc::sysconf(libc::_SC_OPEN_MAX) };
        let max = if max <= 0 { 4096 } else { max.min(65536) } as libc::c_int;
        for fd in 3..max {
            unsafe { libc::close(fd) };
        }
    }
}

impl Pty {
    /// Spawn `cmd` attached to a new PTY of the given size.
    pub fn spawn(cmd: &Command, cols: u16, rows: u16) -> io::Result<Self> {
        // Everything that allocates must happen before fork(): after it, only
        // async-signal-safe calls are allowed (no malloc, so no setenv/execvp).
        let env = build_env(cmd);
        let path_var = env.iter().find(|(k, _)| k.as_slice() == b"PATH").map(|(_, v)| v.clone());
        let program = resolve_program(&cmd.program, path_var.as_deref())
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, format!("{}: command not found", cmd.program)))?;
        let prog = cstr(program.as_os_str().as_bytes())?;
        let argv_c: Vec<CString> = std::iter::once(cstr(cmd.program.as_bytes()))
            .chain(cmd.args.iter().map(|a| cstr(a.as_bytes())))
            .collect::<io::Result<_>>()?;
        let mut argv: Vec<*const libc::c_char> = argv_c.iter().map(|s| s.as_ptr()).collect();
        argv.push(std::ptr::null());
        let envp_c: Vec<CString> = env
            .iter()
            .filter_map(|(k, v)| {
                let mut kv = k.clone();
                kv.push(b'=');
                kv.extend_from_slice(v);
                CString::new(kv).ok()
            })
            .collect();
        let mut envp: Vec<*const libc::c_char> = envp_c.iter().map(|s| s.as_ptr()).collect();
        envp.push(std::ptr::null());
        let cwd = cmd.cwd.as_ref().map(|p| cstr(OsStr::new(p).as_bytes())).transpose()?;

        unsafe {
            let master = cvt(libc::posix_openpt(libc::O_RDWR | libc::O_NOCTTY | libc::O_CLOEXEC))?;
            let master_fd = OwnedFd::from_raw_fd(master);
            cvt(libc::grantpt(master))?;
            cvt(libc::unlockpt(master))?;
            let mut name = [0 as libc::c_char; 128];
            cvt(libc::ptsname_r(master, name.as_mut_ptr(), name.len()))?;
            let ws = winsize(cols.max(1), rows.max(1), 0, 0);
            cvt(libc::ioctl(master, libc::TIOCSWINSZ, &ws))?;

            let slave = cvt(libc::open(name.as_ptr(), libc::O_RDWR | libc::O_NOCTTY | libc::O_CLOEXEC))?;
            let slave_fd = OwnedFd::from_raw_fd(slave);

            let pid = libc::fork();
            if pid < 0 {
                return Err(io::Error::last_os_error());
            }
            if pid == 0 {
                // Child: async-signal-safe calls only.
                let mut set: libc::sigset_t = std::mem::zeroed();
                libc::sigemptyset(&mut set);
                libc::sigprocmask(libc::SIG_SETMASK, &set, std::ptr::null_mut());
                for sig in 1..32 {
                    if sig != libc::SIGKILL && sig != libc::SIGSTOP {
                        libc::signal(sig, libc::SIG_DFL);
                    }
                }
                libc::setsid();
                libc::ioctl(slave, libc::TIOCSCTTY as _, 0);
                // dup2 clears O_CLOEXEC on the new descriptors.
                libc::dup2(slave, 0);
                libc::dup2(slave, 1);
                libc::dup2(slave, 2);
                close_fds_from_3();
                if let Some(dir) = &cwd {
                    libc::chdir(dir.as_ptr());
                }
                libc::execve(prog.as_ptr(), argv.as_ptr(), envp.as_ptr());
                libc::_exit(127);
            }
            drop(slave_fd);
            Ok(Pty { master: master_fd, pid, reaped: false })
        }
    }

    pub fn resize(&self, cols: u16, rows: u16, px_w: u16, px_h: u16) -> io::Result<()> {
        let ws = winsize(cols.max(1), rows.max(1), px_w, px_h);
        unsafe { cvt(libc::ioctl(self.master.as_raw_fd(), libc::TIOCSWINSZ, &ws)) }.map(|_| ())
    }

    /// Current (cols, rows) as the kernel sees it.
    pub fn size(&self) -> io::Result<(u16, u16)> {
        let mut ws = winsize(0, 0, 0, 0);
        unsafe { cvt(libc::ioctl(self.master.as_raw_fd(), libc::TIOCGWINSZ, &mut ws))? };
        Ok((ws.ws_col, ws.ws_row))
    }

    /// Independent handles for reading (blocking, own thread) and writing.
    pub fn reader(&self) -> io::Result<File> {
        Ok(File::from(self.master.try_clone()?))
    }
    pub fn writer(&self) -> io::Result<File> {
        Ok(File::from(self.master.try_clone()?))
    }

    pub fn pid(&self) -> libc::pid_t {
        self.pid
    }

    /// Wait for the child to exit and return its exit code (128 + signal if killed).
    pub fn wait(&mut self) -> io::Result<i32> {
        let mut status = 0;
        loop {
            let r = unsafe { libc::waitpid(self.pid, &mut status, 0) };
            if r == self.pid {
                break;
            }
            let e = io::Error::last_os_error();
            if e.kind() != io::ErrorKind::Interrupted {
                return Err(e);
            }
        }
        self.reaped = true;
        Ok(decode_status(status))
    }

    /// Non-blocking [`Pty::wait`].
    pub fn try_wait(&mut self) -> io::Result<Option<i32>> {
        let mut status = 0;
        let r = unsafe { libc::waitpid(self.pid, &mut status, libc::WNOHANG) };
        match r {
            0 => Ok(None),
            r if r == self.pid => {
                self.reaped = true;
                Ok(Some(decode_status(status)))
            }
            _ => Err(io::Error::last_os_error()),
        }
    }
}

/// The parent environment plus terminal variables and `cmd.env` overrides.
fn build_env(cmd: &Command) -> Vec<(Vec<u8>, Vec<u8>)> {
    let mut env: Vec<(Vec<u8>, Vec<u8>)> =
        std::env::vars_os().map(|(k, v)| (k.as_bytes().to_vec(), v.as_bytes().to_vec())).collect();
    let mut set = |k: &str, v: &str| {
        env.retain(|(ek, _)| ek.as_slice() != k.as_bytes());
        env.push((k.as_bytes().to_vec(), v.as_bytes().to_vec()));
    };
    set("TERM", "xterm-256color");
    set("COLORTERM", "truecolor");
    set("TERM_PROGRAM", "lain");
    set("TERM_PROGRAM_VERSION", env!("CARGO_PKG_VERSION"));
    for (k, v) in &cmd.env {
        set(k, v);
    }
    env
}

/// Resolve `program` like execvp would, but in the parent.
fn resolve_program(program: &str, path: Option<&[u8]>) -> Option<PathBuf> {
    use std::os::unix::fs::PermissionsExt;
    let is_exec = |p: &std::path::Path| p.metadata().is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0);
    if program.contains('/') {
        return Some(PathBuf::from(program));
    }
    let path = path.unwrap_or(b"/usr/local/bin:/usr/bin:/bin");
    path.split(|&b| b == b':')
        .filter(|d| !d.is_empty())
        .map(|d| std::path::Path::new(OsStr::from_bytes(d)).join(program))
        .find(|p| is_exec(p))
}

fn decode_status(status: libc::c_int) -> i32 {
    if libc::WIFEXITED(status) {
        libc::WEXITSTATUS(status)
    } else if libc::WIFSIGNALED(status) {
        128 + libc::WTERMSIG(status)
    } else {
        -1
    }
}

impl Drop for Pty {
    fn drop(&mut self) {
        if self.reaped {
            return;
        }
        let pid = self.pid;
        unsafe { libc::kill(pid, libc::SIGHUP) };
        let mut status = 0;
        if unsafe { libc::waitpid(pid, &mut status, libc::WNOHANG) } == pid {
            return;
        }
        // Reap in the background so the UI never blocks and no zombie is left.
        // Programs that ignore SIGHUP get SIGKILL after a grace period.
        let _ = std::thread::Builder::new().name("pty-reaper".into()).spawn(move || {
            for _ in 0..40 {
                if unsafe { libc::waitpid(pid, &mut status, libc::WNOHANG) } != 0 {
                    return;
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            unsafe {
                libc::kill(pid, libc::SIGKILL);
                libc::waitpid(pid, &mut status, 0);
            }
        });
    }
}
