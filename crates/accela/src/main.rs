//! accela: a fast GPU terminal.

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

/// Environment variables accela sets for itself that shells must not inherit.
pub static PRIVATE_ENV: std::sync::OnceLock<Vec<&'static str>> = std::sync::OnceLock::new();

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
accela — a fast GPU terminal

usage: accela [-d DIR] [-e PROGRAM [ARGS...]]

  -e, --command PROGRAM ARGS...   run PROGRAM instead of the shell (must be last)
  -d, --working-directory DIR     start in DIR
  -V, --version                   print version
  -h, --help                      print this help

config: $XDG_CONFIG_HOME/accela/config (or $ACCELA_CONFIG)";

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
            app_id: Some("accela".into()),
            ..Default::default()
        },
        move |window, cx| cx.new(|cx| Workspace::new(config, launch, window, cx)),
    );
    if let Err(e) = opened {
        eprintln!("accela: cannot open window: {e}");
    }
}

/// The Vulkan loader loads *every* installed driver to probe it — including Mesa
/// drivers for hardware that isn't there and lavapipe (CPU rendering) — and several of
/// them pull in LLVM (~40 MB). Select only the drivers for the GPUs actually present.
///
/// `vendors` are the PCI vendor ids of the DRM render nodes. Returns the value for
/// `VK_LOADER_DRIVERS_SELECT`, or `None` to leave the loader alone (user override, no
/// GPU, or a vendor we don't know — then software rendering stays available).
fn vulkan_driver_filter(
    user_configured: bool,
    vendors: &[u32],
    nvidia_proprietary: bool,
    gpu: config::Gpu,
) -> Option<String> {
    if user_configured || vendors.is_empty() {
        return None;
    }
    // Hybrid laptops: the integrated GPU drives the display; waking the discrete one for
    // a terminal costs battery and ~100 MB of driver libraries.
    let hybrid = vendors.contains(&0x10de) && vendors.iter().any(|v| *v != 0x10de);
    let vendors: Vec<u32> = match gpu {
        config::Gpu::Auto if hybrid => vendors.iter().copied().filter(|v| *v != 0x10de).collect(),
        config::Gpu::Discrete if hybrid => vec![0x10de],
        _ => vendors.to_vec(),
    };
    let mut globs: Vec<&str> = Vec::new();
    for v in &vendors {
        let g: &[&str] = match v {
            0x8086 => &["*intel*"],
            0x1002 => &["*radeon*"],
            0x10de if nvidia_proprietary => &["*nvidia*"],
            0x10de => &["*nouveau*"],
            0x1af4 => &["*virtio*"],
            _ => return None,
        };
        for x in g {
            if !globs.contains(x) {
                globs.push(x);
            }
        }
    }
    Some(globs.join(","))
}

fn gpu_vendors() -> Vec<u32> {
    let Ok(dir) = std::fs::read_dir("/sys/class/drm") else { return Vec::new() };
    let mut v: Vec<u32> = dir
        .flatten()
        .filter(|e| e.file_name().to_string_lossy().starts_with("renderD"))
        .filter_map(|e| std::fs::read_to_string(e.path().join("device/vendor")).ok())
        .filter_map(|s| u32::from_str_radix(s.trim().trim_start_matches("0x"), 16).ok())
        .collect();
    v.sort_unstable();
    v.dedup();
    v
}

fn main() {
    let user_configured =
        ["VK_LOADER_DRIVERS_DISABLE", "VK_LOADER_DRIVERS_SELECT", "VK_ICD_FILENAMES", "VK_DRIVER_FILES"]
            .iter()
            .any(|v| std::env::var_os(v).is_some());
    let config = config::load();
    let nvidia = std::path::Path::new("/dev/nvidia0").exists();
    if let Some(filter) = vulkan_driver_filter(user_configured, &gpu_vendors(), nvidia, config.gpu) {
        // SAFETY: still single-threaded: nothing has spawned a thread yet.
        unsafe { std::env::set_var("VK_LOADER_DRIVERS_SELECT", filter) };
        // Only for our own renderer: programs started in the terminal must see the
        // user's real environment.
        let _ = PRIVATE_ENV.set(vec!["VK_LOADER_DRIVERS_SELECT"]);
    }
    let launch = match parse_args(std::env::args().skip(1)) {
        Ok(Cli::Run(l)) => l,
        Ok(Cli::Version) => {
            println!("accela {}", env!("CARGO_PKG_VERSION"));
            return;
        }
        Ok(Cli::Help) => {
            println!("{HELP}");
            return;
        }
        Err(e) => {
            eprintln!("accela: {e}\n\n{HELP}");
            std::process::exit(2);
        }
    };
    let config = Arc::new(config);
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
    fn vulkan_drivers_match_present_gpus() {
        use config::Gpu::*;
        let f = |user, v: &[u32], nv, g| vulkan_driver_filter(user, v, nv, g);
        // Hybrid Intel + NVIDIA laptop: integrated by default, discrete or both on request.
        assert_eq!(f(false, &[0x10de, 0x8086], true, Auto).as_deref(), Some("*intel*"));
        assert_eq!(f(false, &[0x10de, 0x8086], true, Discrete).as_deref(), Some("*nvidia*"));
        assert_eq!(f(false, &[0x10de, 0x8086], true, Any).as_deref(), Some("*nvidia*,*intel*"));
        // Single GPU: use it.
        assert_eq!(f(false, &[0x10de], false, Auto).as_deref(), Some("*nouveau*"));
        assert_eq!(f(false, &[0x10de], true, Auto).as_deref(), Some("*nvidia*"));
        assert_eq!(f(false, &[0x1002], false, Auto).as_deref(), Some("*radeon*"));
        // No GPU, unknown vendor, or user override: leave the loader alone (lavapipe stays).
        assert_eq!(f(false, &[], false, Auto), None);
        assert_eq!(f(false, &[0x8086, 0x1234], false, Auto), None);
        assert_eq!(f(true, &[0x8086], false, Auto), None);
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
