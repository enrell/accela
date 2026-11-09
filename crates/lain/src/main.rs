//! lain: a fast GPU terminal.

mod bindings;
mod boxdraw;
mod config;
mod frame;
mod images;
mod keymap;
mod panes;
mod view;
mod workspace;

use config::Config;
use gpui::{App, AppContext, Application, Bounds, WindowBounds, WindowOptions, px, size};
use std::sync::Arc;
use workspace::Workspace;

/// What a new window runs.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Launch {
    /// Program and arguments (`-e`); `None` means the shell.
    pub command: Option<Vec<String>>,
    pub cwd: Option<String>,
}

#[derive(Debug, PartialEq)]
enum Cli {
    Run(Launch),
    Version,
    Help,
}

const HELP: &str = "\
lain — a fast GPU terminal

usage: lain [-d DIR] [-e PROGRAM [ARGS...]]

  -e, --command PROGRAM ARGS...   run PROGRAM instead of the shell (must be last)
  -d, --working-directory DIR     start in DIR
  -V, --version                   print version
  -h, --help                      print this help

config: $XDG_CONFIG_HOME/lain/config (or $LAIN_CONFIG)";

fn parse_args(args: impl IntoIterator<Item = String>) -> Result<Cli, String> {
    let mut launch = Launch::default();
    let mut it = args.into_iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "-V" | "--version" => return Ok(Cli::Version),
            "-h" | "--help" => return Ok(Cli::Help),
            "-d" | "--working-directory" => launch.cwd = Some(it.next().ok_or("missing directory after -d")?),
            "-e" | "--command" => {
                let rest: Vec<String> = it.by_ref().collect();
                if rest.is_empty() {
                    return Err("missing program after -e".into());
                }
                launch.command = Some(rest);
            }
            other => return Err(format!("unknown argument: {other}")),
        }
    }
    Ok(Cli::Run(launch))
}

/// Open a new terminal window.
pub fn open_window(config: Arc<Config>, launch: Launch, cx: &mut App) {
    let bounds = Bounds::centered(None, size(px(960.0), px(600.0)), cx);
    let opened = cx.open_window(
        WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            app_id: Some("lain".into()),
            ..Default::default()
        },
        move |window, cx| cx.new(|cx| Workspace::new(config, launch, window, cx)),
    );
    if let Err(e) = opened {
        eprintln!("lain: cannot open window: {e}");
    }
}

fn main() {
    let launch = match parse_args(std::env::args().skip(1)) {
        Ok(Cli::Run(l)) => l,
        Ok(Cli::Version) => {
            println!("lain {}", env!("CARGO_PKG_VERSION"));
            return;
        }
        Ok(Cli::Help) => {
            println!("{HELP}");
            return;
        }
        Err(e) => {
            eprintln!("lain: {e}\n\n{HELP}");
            std::process::exit(2);
        }
    };
    let config = Arc::new(config::load());
    Application::new().run(move |cx: &mut App| {
        cx.on_window_closed(|cx| {
            if cx.windows().is_empty() {
                cx.quit();
            }
        })
        .detach();
        open_window(config, launch, cx);
        cx.activate(true);
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(a: &[&str]) -> Result<Cli, String> {
        parse_args(a.iter().map(|s| s.to_string()))
    }

    #[test]
    fn cli() {
        assert_eq!(args(&[]), Ok(Cli::Run(Launch::default())));
        assert_eq!(args(&["-V"]), Ok(Cli::Version));
        assert_eq!(
            args(&["-d", "/tmp", "-e", "htop", "-d", "x"]),
            Ok(Cli::Run(Launch {
                command: Some(vec!["htop".into(), "-d".into(), "x".into()]),
                cwd: Some("/tmp".into())
            }))
        );
        assert!(args(&["-e"]).is_err());
        assert!(args(&["-d"]).is_err());
        assert!(args(&["--bogus"]).is_err());
    }
}
