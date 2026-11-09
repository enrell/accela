//! GPUI keystrokes → accela-vt key events.

use accela_vt::input::{Key, KeypadKey, Mods};
use gpui::Keystroke;

pub fn mods(k: &Keystroke) -> Mods {
    let m = &k.modifiers;
    Mods { shift: m.shift, alt: m.alt, ctrl: m.control, super_: m.platform }
}

/// The terminal key for a keystroke, or `None` for keys the terminal doesn't encode
/// (bare modifiers, media keys...).
pub fn key(k: &Keystroke) -> Option<Key> {
    Some(match k.key.as_str() {
        "enter" => Key::Enter,
        "tab" => Key::Tab,
        "backspace" => Key::Backspace,
        "escape" => Key::Escape,
        "up" => Key::Up,
        "down" => Key::Down,
        "left" => Key::Left,
        "right" => Key::Right,
        "home" => Key::Home,
        "end" => Key::End,
        "pageup" => Key::PageUp,
        "pagedown" => Key::PageDown,
        "insert" => Key::Insert,
        "delete" => Key::Delete,
        "space" => Key::Char(' '),
        // GPUI names keypad keys after their keysym with the `kp_` prefix removed.
        "add" => Key::Keypad(KeypadKey::Add),
        "subtract" => Key::Keypad(KeypadKey::Subtract),
        "multiply" => Key::Keypad(KeypadKey::Multiply),
        "divide" => Key::Keypad(KeypadKey::Divide),
        "decimal" => Key::Keypad(KeypadKey::Decimal),
        "separator" => Key::Keypad(KeypadKey::Separator),
        "begin" => Key::Keypad(KeypadKey::Begin),
        "kp_enter" => Key::Keypad(KeypadKey::Enter),
        d if d.len() == 4 && d.starts_with("kp_") && d.as_bytes()[3].is_ascii_digit() => {
            Key::Keypad(KeypadKey::Digit(d.as_bytes()[3] - b'0'))
        }
        f if f.len() >= 2 && f.starts_with('f') && f[1..].bytes().all(|b| b.is_ascii_digit()) => {
            let n: u8 = f[1..].parse().ok()?;
            if !(1..=24).contains(&n) {
                return None;
            }
            Key::F(n)
        }
        other => {
            let mut chars = other.chars();
            let c = chars.next()?;
            if chars.next().is_some() {
                return None;
            }
            Key::Char(c.to_lowercase().next().unwrap_or(c))
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ks(s: &str) -> Keystroke {
        Keystroke::parse(s).unwrap()
    }

    #[test]
    fn named_keys() {
        assert_eq!(key(&ks("enter")), Some(Key::Enter));
        assert_eq!(key(&ks("pageup")), Some(Key::PageUp));
        assert_eq!(key(&ks("f12")), Some(Key::F(12)));
        assert_eq!(key(&ks("space")), Some(Key::Char(' ')));
        assert_eq!(key(&ks("a")), Some(Key::Char('a')));
        assert_eq!(key(&ks("shift-a")), Some(Key::Char('a')));
        assert_eq!(key(&ks("f99")), None);
        assert_eq!(key(&ks("add")), Some(Key::Keypad(KeypadKey::Add)));
        assert_eq!(key(&ks("begin")), Some(Key::Keypad(KeypadKey::Begin)));
        assert_eq!(key(&ks("kp_7")), Some(Key::Keypad(KeypadKey::Digit(7))));
        assert_eq!(key(&ks("kp_enter")), Some(Key::Keypad(KeypadKey::Enter)));
    }

    #[test]
    fn modifiers() {
        let m = mods(&ks("ctrl-shift-a"));
        assert!(m.ctrl && m.shift && !m.alt && !m.super_);
    }
}
