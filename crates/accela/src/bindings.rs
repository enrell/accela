//! Keyboard shortcuts: actions, defaults and the `bind = <keys> <action>` config syntax.

use std::collections::HashMap;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Action {
    // Window level (handled before the terminal sees the key).
    NewTab,
    NewWindow,
    ClosePane,
    SplitRight,
    SplitDown,
    FocusLeft,
    FocusDown,
    FocusUp,
    FocusRight,
    ResizeLeft,
    ResizeDown,
    ResizeUp,
    ResizeRight,
    NextTab,
    PrevTab,
    MoveTabLeft,
    MoveTabRight,
    // Terminal level.
    Copy,
    Paste,
    PastePrimary,
    Search,
    ZoomIn,
    ZoomOut,
    ZoomReset,
    ScrollPageUp,
    ScrollPageDown,
    ScrollTop,
    ScrollBottom,
    PrevPrompt,
    NextPrompt,
}

const NAMES: &[(&str, Action)] = &[
    ("new_tab", Action::NewTab),
    ("new_window", Action::NewWindow),
    ("close_pane", Action::ClosePane),
    ("split_right", Action::SplitRight),
    ("split_down", Action::SplitDown),
    ("focus_left", Action::FocusLeft),
    ("focus_down", Action::FocusDown),
    ("focus_up", Action::FocusUp),
    ("focus_right", Action::FocusRight),
    ("resize_left", Action::ResizeLeft),
    ("resize_down", Action::ResizeDown),
    ("resize_up", Action::ResizeUp),
    ("resize_right", Action::ResizeRight),
    ("next_tab", Action::NextTab),
    ("prev_tab", Action::PrevTab),
    ("move_tab_left", Action::MoveTabLeft),
    ("move_tab_right", Action::MoveTabRight),
    ("copy", Action::Copy),
    ("paste", Action::Paste),
    ("paste_primary", Action::PastePrimary),
    ("search", Action::Search),
    ("zoom_in", Action::ZoomIn),
    ("zoom_out", Action::ZoomOut),
    ("zoom_reset", Action::ZoomReset),
    ("scroll_page_up", Action::ScrollPageUp),
    ("scroll_page_down", Action::ScrollPageDown),
    ("scroll_top", Action::ScrollTop),
    ("scroll_bottom", Action::ScrollBottom),
    ("prev_prompt", Action::PrevPrompt),
    ("next_prompt", Action::NextPrompt),
];

impl Action {
    pub fn from_name(s: &str) -> Option<Action> {
        NAMES.iter().find(|(n, _)| *n == s).map(|(_, a)| *a)
    }

    pub fn name(self) -> &'static str {
        NAMES.iter().find(|(_, a)| *a == self).map(|(n, _)| *n).expect("every action has a name")
    }

    /// Whether the window (tabs/splits) handles this action rather than a terminal.
    pub fn is_window_level(self) -> bool {
        matches!(
            self,
            Action::NewTab
                | Action::NewWindow
                | Action::ClosePane
                | Action::SplitRight
                | Action::SplitDown
                | Action::FocusLeft
                | Action::FocusDown
                | Action::FocusUp
                | Action::FocusRight
                | Action::ResizeLeft
                | Action::ResizeDown
                | Action::ResizeUp
                | Action::ResizeRight
                | Action::NextTab
                | Action::PrevTab
                | Action::MoveTabLeft
                | Action::MoveTabRight
        )
    }
}

/// A key chord, normalized: lowercase key name as GPUI reports it, plus modifiers.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Chord {
    pub key: String,
    pub ctrl: bool,
    pub shift: bool,
    pub alt: bool,
    pub super_: bool,
}

impl Chord {
    /// Parse `ctrl+shift+t`, `alt+enter`, `ctrl+plus`, `shift+insert`...
    pub fn parse(s: &str) -> Result<Chord, String> {
        let mut c = Chord { key: String::new(), ctrl: false, shift: false, alt: false, super_: false };
        let parts: Vec<&str> = s.split('+').map(str::trim).collect();
        // "ctrl++" means the plus key.
        let (mods, key) = match parts.as_slice() {
            [rest @ .., "", ""] => (rest, "+"),
            [rest @ .., key] => (rest, *key),
            [] => return Err("empty shortcut".into()),
        };
        for m in mods {
            match m.to_ascii_lowercase().as_str() {
                "ctrl" | "control" => c.ctrl = true,
                "shift" => c.shift = true,
                "alt" | "meta" | "opt" => c.alt = true,
                "super" | "cmd" | "win" | "logo" => c.super_ = true,
                other => return Err(format!("unknown modifier `{other}`")),
            }
        }
        let key = key.to_ascii_lowercase();
        c.key = match key.as_str() {
            "" => return Err("missing key".into()),
            "plus" => "+".into(),
            "minus" => "-".into(),
            "equal" | "equals" => "=".into(),
            "return" => "enter".into(),
            "esc" => "escape".into(),
            "pgup" | "page_up" => "pageup".into(),
            "pgdn" | "page_down" => "pagedown".into(),
            "comma" => ",".into(),
            "period" => ".".into(),
            k if k.chars().count() == 1 || is_named_key(k) => k.to_string(),
            k => return Err(format!("unknown key `{k}`")),
        };
        Ok(c)
    }

    pub fn from_keystroke(key: &str, m: &gpui::Modifiers) -> Chord {
        Chord { key: key.to_string(), ctrl: m.control, shift: m.shift, alt: m.alt, super_: m.platform }
    }
}

fn is_named_key(k: &str) -> bool {
    matches!(
        k,
        "enter"
            | "tab"
            | "space"
            | "backspace"
            | "escape"
            | "delete"
            | "insert"
            | "home"
            | "end"
            | "pageup"
            | "pagedown"
            | "up"
            | "down"
            | "left"
            | "right"
    ) || (k.len() >= 2 && k.starts_with('f') && k[1..].parse::<u8>().is_ok_and(|n| (1..=24).contains(&n)))
}

#[derive(Clone, Debug, PartialEq)]
pub struct Bindings {
    map: HashMap<Chord, Action>,
}

const DEFAULTS: &[(&str, Action)] = &[
    ("ctrl+shift+t", Action::NewTab),
    ("ctrl+shift+n", Action::NewWindow),
    ("ctrl+shift+w", Action::ClosePane),
    ("ctrl+shift+d", Action::SplitRight),
    ("ctrl+shift+e", Action::SplitDown),
    ("ctrl+shift+h", Action::FocusLeft),
    ("ctrl+shift+j", Action::FocusDown),
    ("ctrl+shift+k", Action::FocusUp),
    ("ctrl+shift+l", Action::FocusRight),
    ("ctrl+shift+alt+h", Action::ResizeLeft),
    ("ctrl+shift+alt+j", Action::ResizeDown),
    ("ctrl+shift+alt+k", Action::ResizeUp),
    ("ctrl+shift+alt+l", Action::ResizeRight),
    ("ctrl+tab", Action::NextTab),
    ("ctrl+shift+tab", Action::PrevTab),
    ("ctrl+shift+pagedown", Action::NextTab),
    ("ctrl+shift+pageup", Action::PrevTab),
    ("ctrl+shift+alt+pageup", Action::MoveTabLeft),
    ("ctrl+shift+alt+pagedown", Action::MoveTabRight),
    ("ctrl+shift+c", Action::Copy),
    ("ctrl+shift+v", Action::Paste),
    ("shift+insert", Action::PastePrimary),
    ("ctrl+shift+f", Action::Search),
    ("ctrl+=", Action::ZoomIn),
    ("ctrl++", Action::ZoomIn),
    ("ctrl+shift+=", Action::ZoomIn),
    ("ctrl+shift++", Action::ZoomIn),
    ("ctrl+-", Action::ZoomOut),
    ("ctrl+0", Action::ZoomReset),
    ("shift+pageup", Action::ScrollPageUp),
    ("shift+pagedown", Action::ScrollPageDown),
    ("shift+home", Action::ScrollTop),
    ("shift+end", Action::ScrollBottom),
    ("ctrl+shift+up", Action::PrevPrompt),
    ("ctrl+shift+down", Action::NextPrompt),
];

impl Default for Bindings {
    fn default() -> Self {
        let map = DEFAULTS.iter().map(|(k, a)| (Chord::parse(k).expect("valid default"), *a)).collect();
        Bindings { map }
    }
}

impl Bindings {
    pub fn get(&self, key: &str, m: &gpui::Modifiers) -> Option<Action> {
        self.map.get(&Chord::from_keystroke(key, m)).copied()
    }

    /// Apply one `bind` value: `<chord> <action>` or `<chord> none` (the key then goes
    /// to the program), or `clear` to drop every default.
    pub fn apply(&mut self, value: &str) -> Result<(), String> {
        let value = value.trim();
        if value == "clear" {
            self.map.clear();
            return Ok(());
        }
        let (chord, action) = value.split_once(char::is_whitespace).ok_or("expected `<keys> <action>`")?;
        let chord = Chord::parse(chord)?;
        match action.trim() {
            "none" | "unbind" => {
                self.map.remove(&chord);
            }
            name => {
                let a = Action::from_name(name).ok_or_else(|| format!("unknown action `{name}`"))?;
                self.map.insert(chord, a);
            }
        }
        Ok(())
    }

    /// Chords bound to `action`, for help/documentation.
    #[cfg(test)]
    pub fn chords(&self, action: Action) -> Vec<&Chord> {
        self.map.iter().filter(|(_, a)| **a == action).map(|(c, _)| c).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mods(ctrl: bool, shift: bool, alt: bool) -> gpui::Modifiers {
        gpui::Modifiers { control: ctrl, shift, alt, ..Default::default() }
    }

    #[test]
    fn chord_parsing() {
        let c = Chord::parse("Ctrl+Shift+T").unwrap();
        assert!(c.ctrl && c.shift && !c.alt && c.key == "t");
        assert_eq!(Chord::parse("ctrl++").unwrap().key, "+");
        assert_eq!(Chord::parse("ctrl+plus").unwrap().key, "+");
        assert_eq!(Chord::parse("alt+Return").unwrap().key, "enter");
        assert_eq!(Chord::parse("super+f12").unwrap().key, "f12");
        assert!(Chord::parse("hyper+t").is_err());
        assert!(Chord::parse("ctrl+wat").is_err());
        assert!(Chord::parse("ctrl+f99").is_err());
        assert!(Chord::parse("").is_err());
    }

    #[test]
    fn defaults_are_valid_and_distinct_per_level() {
        let b = Bindings::default();
        assert_eq!(b.get("t", &mods(true, true, false)), Some(Action::NewTab));
        assert_eq!(b.get("f", &mods(true, true, false)), Some(Action::Search));
        assert_eq!(b.get("pageup", &mods(false, true, false)), Some(Action::ScrollPageUp));
        // Keys that belong to programs are never bound by default.
        for (k, m) in
            [("c", mods(true, false, false)), ("t", mods(true, false, false)), ("a", mods(false, false, false))]
        {
            assert_eq!(b.get(k, &m), None, "{k}");
        }
    }

    #[test]
    fn every_action_has_a_default_and_a_unique_name() {
        let b = Bindings::default();
        for (name, a) in NAMES {
            assert_eq!(Action::from_name(name), Some(*a));
            assert_eq!(a.name(), *name);
            assert!(!b.chords(*a).is_empty(), "{name} has no default");
        }
        let mut names: Vec<_> = NAMES.iter().map(|(n, _)| n).collect();
        names.dedup();
        assert_eq!(names.len(), NAMES.len());
    }

    #[test]
    fn rebind_unbind_clear() {
        let mut b = Bindings::default();
        b.apply("alt+t new_tab").unwrap();
        assert_eq!(b.get("t", &mods(false, false, true)), Some(Action::NewTab));
        b.apply("ctrl+shift+t none").unwrap();
        assert_eq!(b.get("t", &mods(true, true, false)), None);
        b.apply("ctrl+shift+d split_down").unwrap();
        assert_eq!(b.get("d", &mods(true, true, false)), Some(Action::SplitDown));
        assert!(b.apply("ctrl+x frobnicate").is_err());
        assert!(b.apply("ctrl+x").is_err());
        b.apply("clear").unwrap();
        assert_eq!(b.get("f", &mods(true, true, false)), None);
    }
}
