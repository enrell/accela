//! `~/.config/accela/config`: a tiny `key = value` format. Unknown keys and invalid
//! values produce warnings and fall back to defaults; the terminal always starts.

#[cfg(test)]
use accela_vt::color::Rgb;
use accela_vt::color::{Theme, parse_spec};
use std::path::PathBuf;

#[derive(Clone, Debug, PartialEq)]
pub struct Config {
    pub font_family: String,
    pub font_size: f32,
    pub line_height: f32,
    pub padding: f32,
    pub scrollback: usize,
    pub shell: Option<String>,
    pub cursor_blink: bool,
    pub bold_is_bright: bool,
    pub theme: Theme,
    pub bindings: crate::bindings::Bindings,
    pub gpu: Gpu,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            font_family: "JetBrainsMono Nerd Font Mono".into(),
            font_size: 15.0,
            line_height: 1.25,
            padding: 6.0,
            scrollback: 10_000,
            shell: None,
            cursor_blink: true,
            bold_is_bright: false,
            theme: Theme::default(),
            bindings: Default::default(),
            gpu: Gpu::Auto,
        }
    }
}

/// Which GPU renders the terminal on machines with more than one.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Gpu {
    /// Integrated on hybrid laptops (saves battery), otherwise whatever is present.
    #[default]
    Auto,
    Discrete,
    /// Let the Vulkan loader offer every driver.
    Any,
}

pub fn config_path() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("ACCELA_CONFIG") {
        return Some(PathBuf::from(p));
    }
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))?;
    Some(base.join("accela").join("config"))
}

/// Load the config file; a missing file means defaults. Warnings go to stderr.
pub fn load() -> Config {
    let Some(path) = config_path() else { return Config::default() };
    match std::fs::read_to_string(&path) {
        Ok(text) => {
            let (cfg, warnings) = parse(&text);
            for w in warnings {
                eprintln!("accela: {}: {w}", path.display());
            }
            cfg
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Config::default(),
        Err(e) => {
            eprintln!("accela: {}: {e}", path.display());
            Config::default()
        }
    }
}

pub fn parse(text: &str) -> (Config, Vec<String>) {
    let mut c = Config::default();
    let mut warnings = Vec::new();
    for (n, raw) in text.lines().enumerate() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            warnings.push(format!("line {}: expected `key = value`", n + 1));
            continue;
        };
        let (key, value) = (key.trim(), value.trim());
        let mut bad = |what: &str| warnings.push(format!("line {}: invalid {what} for `{key}`: {value:?}", n + 1));
        let num = |lo: f32, hi: f32| value.parse::<f32>().ok().filter(|v| (lo..=hi).contains(v));
        let boolean = || match value {
            "true" | "yes" | "on" => Some(true),
            "false" | "no" | "off" => Some(false),
            _ => None,
        };
        let color = || parse_spec(value.as_bytes());
        match key {
            "font_family" if !value.is_empty() => c.font_family = value.to_string(),
            "font_size" => match num(4.0, 200.0) {
                Some(v) => c.font_size = v,
                None => bad("number (4..200)"),
            },
            "line_height" => match num(0.8, 3.0) {
                Some(v) => c.line_height = v,
                None => bad("number (0.8..3)"),
            },
            "padding" => match num(0.0, 200.0) {
                Some(v) => c.padding = v,
                None => bad("number (0..200)"),
            },
            "scrollback" => match value.parse::<usize>().ok().filter(|v| *v <= 1_000_000) {
                Some(v) => c.scrollback = v,
                None => bad("line count (0..1000000)"),
            },
            "shell" if !value.is_empty() => c.shell = Some(value.to_string()),
            "gpu" => match value {
                "auto" | "integrated" => c.gpu = Gpu::Auto,
                "discrete" => c.gpu = Gpu::Discrete,
                "any" => c.gpu = Gpu::Any,
                _ => bad("gpu (auto, discrete, any)"),
            },
            "bind" => {
                if let Err(e) = c.bindings.apply(value) {
                    warnings.push(format!("line {}: bind: {e}", n + 1));
                }
            }
            "cursor_blink" => match boolean() {
                Some(v) => c.cursor_blink = v,
                None => bad("boolean"),
            },
            "bold_is_bright" => match boolean() {
                Some(v) => c.bold_is_bright = v,
                None => bad("boolean"),
            },
            "foreground" | "background" | "cursor" | "selection" => match color() {
                Some(v) => match key {
                    "foreground" => c.theme.foreground = v,
                    "background" => c.theme.background = v,
                    "cursor" => c.theme.cursor = v,
                    _ => c.theme.selection = v,
                },
                None => bad("color"),
            },
            k if k.starts_with("color") => match (k[5..].parse::<u8>(), color()) {
                (Ok(i), Some(v)) => c.theme.palette[i as usize] = v,
                (Err(_), _) => warnings.push(format!("line {}: unknown key `{key}`", n + 1)),
                (_, None) => bad("color"),
            },
            _ => warnings.push(format!("line {}: unknown key `{key}`", n + 1)),
        }
    }
    (c, warnings)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_is_default() {
        let (c, w) = parse("");
        assert_eq!(c, Config::default());
        assert!(w.is_empty());
    }

    #[test]
    fn parses_all_keys() {
        let (c, w) = parse(
            "# comment\nfont_family = Iosevka Term\nfont_size=13.5\nline_height = 1.1\npadding = 0\n\
             scrollback = 500\nshell = /bin/zsh\ncursor_blink = off\nbold_is_bright = yes\n\
             background = #000000\nforeground = rgb:ff/ff/ff\ncursor=#f00\nselection = #112233\ncolor1 = #ff0000\ncolor255 = #010203\n",
        );
        assert!(w.is_empty(), "{w:?}");
        assert_eq!(c.font_family, "Iosevka Term");
        assert_eq!(c.font_size, 13.5);
        assert_eq!(c.line_height, 1.1);
        assert_eq!(c.padding, 0.0);
        assert_eq!(c.scrollback, 500);
        assert_eq!(c.shell.as_deref(), Some("/bin/zsh"));
        assert!(!c.cursor_blink);
        assert!(c.bold_is_bright);
        assert_eq!(c.theme.background, Rgb(0, 0, 0));
        assert_eq!(c.theme.foreground, Rgb(255, 255, 255));
        assert_eq!(c.theme.cursor, Rgb(0xf0, 0, 0));
        assert_eq!(c.theme.palette[1], Rgb(255, 0, 0));
        assert_eq!(c.theme.palette[255], Rgb(1, 2, 3));
    }

    #[test]
    fn key_bindings() {
        assert_eq!(parse("gpu = discrete").0.gpu, Gpu::Discrete);
        assert_eq!(parse("gpu = turbo").1.len(), 1);
        let (c, w) = parse("bind = alt+t new_tab\nbind = ctrl+shift+t none\nbind = ctrl+x explode\n");
        assert_eq!(w.len(), 1, "{w:?}");
        let m = |ctrl, shift, alt| gpui::Modifiers { control: ctrl, shift, alt, ..Default::default() };
        assert_eq!(c.bindings.get("t", &m(false, false, true)), Some(crate::bindings::Action::NewTab));
        assert_eq!(c.bindings.get("t", &m(true, true, false)), None);
    }

    #[test]
    fn invalid_values_warn_and_keep_defaults() {
        let (c, w) = parse(
            "font_size = 0\nscrollback = -1\ncolor1 = nope\ncolor300 = #fff\nwat = 1\njunk line\ncursor_blink = maybe",
        );
        assert_eq!(c, Config::default());
        assert_eq!(w.len(), 7, "{w:?}");
    }
}
