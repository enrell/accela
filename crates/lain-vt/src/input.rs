//! Keyboard → bytes. Implements legacy xterm encoding and the kitty keyboard protocol
//! (<https://sw.kovidgoyal.net/kitty/keyboard-protocol/>), plus safe paste encoding.

use crate::term::Term;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Key {
    /// A text key, identified by its unshifted character (e.g. `'a'` for Shift+A).
    Char(char),
    Enter,
    Tab,
    Backspace,
    Escape,
    Up,
    Down,
    Left,
    Right,
    Home,
    End,
    PageUp,
    PageDown,
    Insert,
    Delete,
    /// F1..=F24.
    F(u8),
    /// Numeric-keypad keys that are distinguishable from the main block.
    Keypad(KeypadKey),
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum KeypadKey {
    Add,
    Subtract,
    Multiply,
    Divide,
    Decimal,
    Separator,
    /// The center key with NumLock off.
    Begin,
}

impl KeypadKey {
    fn text(self) -> &'static str {
        match self {
            KeypadKey::Add => "+",
            KeypadKey::Subtract => "-",
            KeypadKey::Multiply => "*",
            KeypadKey::Divide => "/",
            KeypadKey::Decimal => ".",
            KeypadKey::Separator => ",",
            KeypadKey::Begin => "",
        }
    }

    /// Final byte of the SS3 sequence sent in application keypad mode (DECKPAM).
    fn ss3(self) -> u8 {
        match self {
            KeypadKey::Add => b'k',
            KeypadKey::Subtract => b'm',
            KeypadKey::Multiply => b'j',
            KeypadKey::Divide => b'o',
            KeypadKey::Decimal => b'n',
            KeypadKey::Separator => b'l',
            KeypadKey::Begin => b'E',
        }
    }

    /// Kitty keyboard protocol key code.
    fn kitty_code(self) -> u32 {
        match self {
            KeypadKey::Decimal => 57409,
            KeypadKey::Divide => 57410,
            KeypadKey::Multiply => 57411,
            KeypadKey::Subtract => 57412,
            KeypadKey::Add => 57413,
            KeypadKey::Separator => 57416,
            KeypadKey::Begin => 57427,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Mods {
    pub shift: bool,
    pub alt: bool,
    pub ctrl: bool,
    pub super_: bool,
}

impl Mods {
    pub const NONE: Mods = Mods { shift: false, alt: false, ctrl: false, super_: false };

    /// xterm/kitty modifier parameter (1 = none).
    pub fn param(self) -> u8 {
        1 + self.shift as u8 + 2 * self.alt as u8 + 4 * self.ctrl as u8 + 8 * self.super_ as u8
    }

    fn any(self) -> bool {
        self.param() > 1
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum KeyAction {
    #[default]
    Press,
    Repeat,
    Release,
}

#[derive(Clone, Copy, Debug)]
pub struct KeyEvent<'a> {
    pub key: Key,
    pub mods: Mods,
    /// Text the key produces with the current layout/modifiers (e.g. `"A"`, `"é"`).
    pub text: Option<&'a str>,
    pub action: KeyAction,
}

/// The terminal modes that affect key encoding.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct InputModes {
    pub app_cursor: bool,
    pub app_keypad: bool,
    pub newline: bool,
    pub kitty_flags: u8,
}

impl InputModes {
    pub fn from_term(t: &Term) -> Self {
        let m = t.modes();
        Self {
            app_cursor: m.app_cursor,
            app_keypad: m.app_keypad,
            newline: m.newline,
            kitty_flags: t.kitty_keyboard_flags(),
        }
    }
}

pub mod kitty {
    pub const DISAMBIGUATE: u8 = 1;
    pub const REPORT_EVENTS: u8 = 2;
    pub const REPORT_ALTERNATES: u8 = 4;
    pub const REPORT_ALL: u8 = 8;
    pub const REPORT_TEXT: u8 = 16;
}

/// How a non-text key is written in CSI form: `CSI <num> ~` or `CSI 1 ; mods <letter>`.
enum Functional {
    Tilde(u16),
    Letter(u8),
}

fn functional(key: Key) -> Option<Functional> {
    use Functional::*;
    Some(match key {
        Key::Up => Letter(b'A'),
        Key::Down => Letter(b'B'),
        Key::Right => Letter(b'C'),
        Key::Left => Letter(b'D'),
        Key::Home => Letter(b'H'),
        Key::End => Letter(b'F'),
        Key::Insert => Tilde(2),
        Key::Delete => Tilde(3),
        Key::PageUp => Tilde(5),
        Key::PageDown => Tilde(6),
        Key::F(1) => Letter(b'P'),
        Key::F(2) => Letter(b'Q'),
        Key::F(3) => Tilde(13),
        Key::F(4) => Letter(b'S'),
        Key::F(n @ 5..=24) => {
            Tilde([15, 17, 18, 19, 20, 21, 23, 24, 25, 26, 28, 29, 31, 32, 33, 34, 42, 43, 44, 45][(n - 5) as usize])
        }
        _ => return None,
    })
}

pub fn encode_key(ev: &KeyEvent, modes: &InputModes) -> Option<Vec<u8>> {
    if modes.kitty_flags != 0 {
        encode_kitty(ev, modes.kitty_flags)
    } else if ev.action == KeyAction::Release {
        None
    } else {
        encode_legacy(ev, modes)
    }
}

fn with_alt(alt: bool, mut bytes: Vec<u8>) -> Vec<u8> {
    if alt {
        bytes.insert(0, 0x1b);
    }
    bytes
}

fn ctrl_byte(c: char) -> Option<u8> {
    Some(match c.to_ascii_lowercase() {
        c @ 'a'..='z' => c as u8 - b'a' + 1,
        ' ' | '@' | '2' => 0,
        '[' | '3' => 0x1b,
        '\\' | '4' => 0x1c,
        ']' | '5' => 0x1d,
        '^' | '6' | '~' => 0x1e,
        '_' | '-' | '7' | '/' => 0x1f,
        '8' | '?' => 0x7f,
        _ => return None,
    })
}

fn encode_legacy(ev: &KeyEvent, modes: &InputModes) -> Option<Vec<u8>> {
    let m = ev.mods;
    let mp = m.param();
    let letter = |l: u8, ss3: bool| -> Vec<u8> {
        if mp > 1 {
            format!("\x1b[1;{mp}{}", l as char).into_bytes()
        } else if ss3 {
            vec![0x1b, b'O', l]
        } else {
            vec![0x1b, b'[', l]
        }
    };
    let tilde = |n: u16| -> Vec<u8> {
        if mp > 1 { format!("\x1b[{n};{mp}~").into_bytes() } else { format!("\x1b[{n}~").into_bytes() }
    };
    Some(match ev.key {
        Key::Enter => with_alt(m.alt, if modes.newline { b"\r\n".to_vec() } else { vec![b'\r'] }),
        Key::Backspace => with_alt(m.alt, vec![if m.ctrl { 0x08 } else { 0x7f }]),
        Key::Tab => {
            if m.shift {
                b"\x1b[Z".to_vec()
            } else {
                with_alt(m.alt, vec![b'\t'])
            }
        }
        Key::Escape => with_alt(m.alt, vec![0x1b]),
        Key::Up | Key::Down | Key::Right | Key::Left | Key::Home | Key::End => {
            let Some(Functional::Letter(l)) = functional(ev.key) else { unreachable!() };
            letter(l, modes.app_cursor)
        }
        Key::F(n @ 1..=4) => letter(b'P' + n - 1, true),
        Key::Keypad(KeypadKey::Begin) => letter(b'E', modes.app_keypad),
        Key::Keypad(k) => {
            if modes.app_keypad && !m.any() {
                vec![0x1b, b'O', k.ss3()]
            } else if modes.app_keypad {
                format!("\x1bO{mp}{}", k.ss3() as char).into_bytes()
            } else {
                with_alt(m.alt, k.text().as_bytes().to_vec())
            }
        }
        Key::Char(c) => {
            if m.super_ {
                return None;
            }
            if m.ctrl {
                with_alt(m.alt, vec![ctrl_byte(c)?])
            } else {
                let text = match ev.text {
                    Some(t) if !t.is_empty() => t.as_bytes().to_vec(),
                    _ => c.to_string().into_bytes(),
                };
                with_alt(m.alt, text)
            }
        }
        k => match functional(k)? {
            Functional::Tilde(n) => tilde(n),
            Functional::Letter(l) => letter(l, false),
        },
    })
}

fn encode_kitty(ev: &KeyEvent, flags: u8) -> Option<Vec<u8>> {
    use kitty::*;
    let report_events = flags & REPORT_EVENTS != 0;
    let all = flags & REPORT_ALL != 0;
    let m = ev.mods;
    let action = match ev.action {
        KeyAction::Repeat if !report_events => KeyAction::Press,
        a => a,
    };
    if action == KeyAction::Release && !report_events {
        return None;
    }

    // Keys that keep their legacy encoding unless "report all keys" is on.
    if !all {
        match ev.key {
            Key::Char(c) if !m.ctrl && !m.alt && !m.super_ => {
                if action == KeyAction::Release {
                    // Release of a text key is still reported as an escape code.
                } else {
                    let text = ev.text.filter(|t| !t.is_empty()).map(str::to_owned).unwrap_or_else(|| c.to_string());
                    return Some(text.into_bytes());
                }
            }
            // Keypad text keys behave like text: their character unless modified.
            Key::Keypad(k) if k != KeypadKey::Begin && !m.ctrl && !m.alt && !m.super_ => {
                if action != KeyAction::Release {
                    return Some(k.text().as_bytes().to_vec());
                }
            }
            Key::Enter | Key::Tab | Key::Backspace if !m.any() => {
                if action == KeyAction::Release {
                    return None;
                }
                let modes = InputModes::default();
                return encode_legacy(&KeyEvent { action: KeyAction::Press, ..*ev }, &modes);
            }
            _ => {}
        }
    }

    let ev_suffix = match action {
        KeyAction::Press => "",
        KeyAction::Repeat => ":2",
        KeyAction::Release => ":3",
    };
    let mp = m.param();
    let mods_field = if mp > 1 || !ev_suffix.is_empty() { format!("{mp}{ev_suffix}") } else { String::new() };

    let code: u32 = match ev.key {
        Key::Char(c) => c.to_lowercase().next().unwrap_or(c) as u32,
        Key::Enter => 13,
        Key::Tab => 9,
        Key::Backspace => 127,
        Key::Escape => 27,
        // Begin keeps the legacy CSI E form (like the cursor keys) unless reporting all keys.
        Key::Keypad(KeypadKey::Begin) if !all => {
            return Some(
                if mods_field.is_empty() { "\x1b[E".to_string() } else { format!("\x1b[1;{mods_field}E") }.into_bytes(),
            );
        }
        Key::Keypad(k) => k.kitty_code(),
        k => {
            return Some(
                match functional(k)? {
                    Functional::Letter(l) => {
                        if mods_field.is_empty() {
                            format!("\x1b[{}", l as char)
                        } else {
                            format!("\x1b[1;{mods_field}{}", l as char)
                        }
                    }
                    Functional::Tilde(n) => {
                        if mods_field.is_empty() {
                            format!("\x1b[{n}~")
                        } else {
                            format!("\x1b[{n};{mods_field}~")
                        }
                    }
                }
                .into_bytes(),
            );
        }
    };

    let mut key_field = code.to_string();
    if flags & REPORT_ALTERNATES != 0
        && m.shift
        && let Some(shifted) = ev.text.and_then(|t| t.chars().next())
        && shifted as u32 != code
    {
        key_field.push_str(&format!(":{}", shifted as u32));
    }

    let text_field = if flags & REPORT_TEXT != 0 && all && action != KeyAction::Release {
        ev.text
            .filter(|t| !t.is_empty() && !t.chars().any(char::is_control))
            .map(|t| t.chars().map(|c| (c as u32).to_string()).collect::<Vec<_>>().join(":"))
    } else {
        None
    };

    let mut out = format!("\x1b[{key_field}");
    match (&text_field, mods_field.is_empty()) {
        (Some(t), true) => out.push_str(&format!(";1;{t}")),
        (Some(t), false) => out.push_str(&format!(";{mods_field};{t}")),
        (None, false) => out.push_str(&format!(";{mods_field}")),
        (None, true) => {}
    }
    out.push('u');
    Some(out.into_bytes())
}

/// Encode pasted text. Control characters are stripped (except tab and newlines, which
/// become CR) so a paste can never smuggle escape sequences — in particular it cannot
/// terminate bracketed paste early with `ESC [ 201 ~`.
pub fn encode_paste(text: &str, bracketed: bool) -> Vec<u8> {
    let mut out = Vec::with_capacity(text.len() + 12);
    if bracketed {
        out.extend_from_slice(b"\x1b[200~");
    }
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\r' => {
                if chars.peek() == Some(&'\n') {
                    chars.next();
                }
                out.push(b'\r');
            }
            '\n' => out.push(b'\r'),
            '\t' => out.push(b'\t'),
            c if c.is_control() => {}
            c => {
                let mut buf = [0; 4];
                out.extend_from_slice(c.encode_utf8(&mut buf).as_bytes());
            }
        }
    }
    if bracketed {
        out.extend_from_slice(b"\x1b[201~");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn press(key: Key, mods: Mods, text: Option<&str>) -> KeyEvent<'_> {
        KeyEvent { key, mods, text, action: KeyAction::Press }
    }

    fn legacy(ev: KeyEvent) -> Vec<u8> {
        encode_key(&ev, &InputModes::default()).unwrap_or_default()
    }

    fn kitty(ev: KeyEvent, flags: u8) -> String {
        String::from_utf8(encode_key(&ev, &InputModes { kitty_flags: flags, ..Default::default() }).unwrap_or_default())
            .unwrap()
    }

    const CTRL: Mods = Mods { ctrl: true, ..Mods::NONE };
    const ALT: Mods = Mods { alt: true, ..Mods::NONE };
    const SHIFT: Mods = Mods { shift: true, ..Mods::NONE };

    #[test]
    fn legacy_text_and_controls() {
        assert_eq!(legacy(press(Key::Char('a'), Mods::NONE, Some("a"))), b"a");
        assert_eq!(legacy(press(Key::Char('a'), SHIFT, Some("A"))), b"A");
        assert_eq!(legacy(press(Key::Char('e'), Mods::NONE, Some("é"))), "é".as_bytes());
        assert_eq!(legacy(press(Key::Char('c'), CTRL, None)), [3]);
        assert_eq!(legacy(press(Key::Char(' '), CTRL, None)), [0]);
        assert_eq!(legacy(press(Key::Char('['), CTRL, None)), [0x1b]);
        assert_eq!(legacy(press(Key::Char('b'), ALT, Some("b"))), b"\x1bb");
        assert_eq!(legacy(press(Key::Char('x'), Mods { ctrl: true, alt: true, ..Mods::NONE }, None)), b"\x1b\x18");
    }

    #[test]
    fn legacy_special_keys() {
        assert_eq!(legacy(press(Key::Enter, Mods::NONE, None)), b"\r");
        assert_eq!(legacy(press(Key::Backspace, Mods::NONE, None)), [0x7f]);
        assert_eq!(legacy(press(Key::Backspace, CTRL, None)), [0x08]);
        assert_eq!(legacy(press(Key::Tab, SHIFT, None)), b"\x1b[Z");
        assert_eq!(legacy(press(Key::Up, Mods::NONE, None)), b"\x1b[A");
        assert_eq!(legacy(press(Key::Left, CTRL, None)), b"\x1b[1;5D");
        assert_eq!(legacy(press(Key::Delete, Mods::NONE, None)), b"\x1b[3~");
        assert_eq!(legacy(press(Key::PageUp, SHIFT, None)), b"\x1b[5;2~");
        assert_eq!(legacy(press(Key::F(1), Mods::NONE, None)), b"\x1bOP");
        assert_eq!(legacy(press(Key::F(5), Mods::NONE, None)), b"\x1b[15~");
        assert_eq!(legacy(press(Key::F(12), CTRL, None)), b"\x1b[24;5~");
    }

    #[test]
    fn app_cursor_and_newline_modes() {
        let m = InputModes { app_cursor: true, newline: true, ..Default::default() };
        assert_eq!(encode_key(&press(Key::Up, Mods::NONE, None), &m).unwrap(), b"\x1bOA");
        assert_eq!(encode_key(&press(Key::Up, CTRL, None), &m).unwrap(), b"\x1b[1;5A");
        assert_eq!(encode_key(&press(Key::Enter, Mods::NONE, None), &m).unwrap(), b"\r\n");
    }

    #[test]
    fn legacy_ignores_release() {
        let ev = KeyEvent { action: KeyAction::Release, ..press(Key::Char('a'), Mods::NONE, Some("a")) };
        assert!(encode_key(&ev, &InputModes::default()).is_none());
    }

    #[test]
    fn kitty_disambiguate() {
        let f = kitty::DISAMBIGUATE;
        assert_eq!(kitty(press(Key::Char('a'), Mods::NONE, Some("a")), f), "a");
        assert_eq!(kitty(press(Key::Escape, Mods::NONE, None), f), "\x1b[27u");
        assert_eq!(kitty(press(Key::Char('c'), CTRL, None), f), "\x1b[99;5u");
        assert_eq!(kitty(press(Key::Char('i'), CTRL, None), f), "\x1b[105;5u");
        assert_eq!(kitty(press(Key::Enter, Mods::NONE, None), f), "\r");
        assert_eq!(kitty(press(Key::Enter, SHIFT, None), f), "\x1b[13;2u");
        assert_eq!(kitty(press(Key::Up, Mods::NONE, None), f), "\x1b[A");
        assert_eq!(kitty(press(Key::Up, ALT, None), f), "\x1b[1;3A");
        assert_eq!(kitty(press(Key::F(3), Mods::NONE, None), f), "\x1b[13~");
        assert_eq!(kitty(press(Key::Delete, CTRL, None), f), "\x1b[3;5~");
    }

    #[test]
    fn kitty_events_and_all_keys() {
        use kitty::*;
        let rel = KeyEvent { action: KeyAction::Release, ..press(Key::Char('a'), Mods::NONE, Some("a")) };
        assert_eq!(kitty(rel, DISAMBIGUATE), "");
        assert_eq!(kitty(rel, DISAMBIGUATE | REPORT_EVENTS), "\x1b[97;1:3u");
        let rep = KeyEvent { action: KeyAction::Repeat, ..press(Key::Char('a'), CTRL, None) };
        assert_eq!(kitty(rep, DISAMBIGUATE | REPORT_EVENTS), "\x1b[97;5:2u");
        assert_eq!(kitty(rep, DISAMBIGUATE), "\x1b[97;5u");
        assert_eq!(kitty(press(Key::Char('a'), Mods::NONE, Some("a")), REPORT_ALL), "\x1b[97u");
        assert_eq!(kitty(press(Key::Enter, Mods::NONE, None), REPORT_ALL), "\x1b[13u");
        let enter_rel = KeyEvent { action: KeyAction::Release, ..press(Key::Enter, Mods::NONE, None) };
        assert_eq!(kitty(enter_rel, REPORT_EVENTS), "");
        assert_eq!(kitty(enter_rel, REPORT_EVENTS | REPORT_ALL), "\x1b[13;1:3u");
    }

    #[test]
    fn kitty_alternates_and_text() {
        use kitty::*;
        let shift_a = press(Key::Char('a'), SHIFT, Some("A"));
        assert_eq!(kitty(shift_a, REPORT_ALL | REPORT_ALTERNATES), "\x1b[97:65;2u");
        assert_eq!(kitty(shift_a, REPORT_ALL | REPORT_TEXT), "\x1b[97;2;65u");
        assert_eq!(kitty(press(Key::Char('e'), Mods::NONE, Some("é")), REPORT_ALL | REPORT_TEXT), "\x1b[101;1;233u");
    }

    #[test]
    fn keypad_keys() {
        use KeypadKey::*;
        let app = InputModes { app_keypad: true, ..Default::default() };
        let enc = |k, mods, m: &InputModes| encode_key(&press(Key::Keypad(k), mods, None), m).unwrap();
        assert_eq!(enc(Add, Mods::NONE, &InputModes::default()), b"+");
        assert_eq!(enc(Add, Mods::NONE, &app), b"\x1bOk");
        assert_eq!(enc(Subtract, Mods::NONE, &app), b"\x1bOm");
        assert_eq!(enc(Multiply, Mods::NONE, &app), b"\x1bOj");
        assert_eq!(enc(Divide, Mods::NONE, &app), b"\x1bOo");
        assert_eq!(enc(Decimal, Mods::NONE, &app), b"\x1bOn");
        assert_eq!(enc(Separator, Mods::NONE, &app), b"\x1bOl");
        assert_eq!(enc(Add, CTRL, &app), b"\x1bO5k");
        assert_eq!(enc(Begin, Mods::NONE, &InputModes::default()), b"\x1b[E");
        assert_eq!(enc(Begin, Mods::NONE, &app), b"\x1bOE");
        assert_eq!(enc(Begin, SHIFT, &app), b"\x1b[1;2E");
        assert_eq!(enc(Divide, ALT, &InputModes::default()), b"\x1b/");
        // Kitty: text unless modified or reporting all keys.
        assert_eq!(kitty(press(Key::Keypad(Add), Mods::NONE, None), kitty::DISAMBIGUATE), "+");
        assert_eq!(kitty(press(Key::Keypad(Add), CTRL, None), kitty::DISAMBIGUATE), "\x1b[57413;5u");
        assert_eq!(kitty(press(Key::Keypad(Add), Mods::NONE, None), kitty::REPORT_ALL), "\x1b[57413u");
        assert_eq!(kitty(press(Key::Keypad(Begin), Mods::NONE, None), kitty::DISAMBIGUATE), "\x1b[E");
    }

    #[test]
    fn paste_is_sanitized() {
        assert_eq!(encode_paste("a\nb\r\nc", false), b"a\rb\rc");
        assert_eq!(encode_paste("x\x1b[201~rm -rf /\n", true), b"\x1b[200~x[201~rm -rf /\r\x1b[201~");
        assert_eq!(encode_paste("tab\there\u{7}\u{9b}", false), b"tab\there");
    }
}
