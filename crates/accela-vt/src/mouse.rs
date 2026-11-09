//! Mouse reporting: X10 / normal / button-event / any-event tracking with
//! X10, UTF-8 (1005), SGR (1006), urxvt (1015) and SGR-pixel (1016) encodings.

use crate::input::Mods;
use crate::term::{Modes, MouseEncoding, MouseTracking};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum MouseButton {
    Left,
    Middle,
    Right,
    /// Motion with no button held.
    None,
    WheelUp,
    WheelDown,
    WheelLeft,
    WheelRight,
    Back,
    Forward,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum MouseAction {
    Press,
    Release,
    Motion,
}

#[derive(Clone, Copy, Debug)]
pub struct MouseEvent {
    pub button: MouseButton,
    pub action: MouseAction,
    /// 0-based cell coordinates.
    pub col: usize,
    pub row: usize,
    /// Pixel coordinates inside the terminal area (for mode 1016).
    pub px: (usize, usize),
    pub mods: Mods,
}

impl MouseButton {
    fn code(self) -> u32 {
        match self {
            MouseButton::Left => 0,
            MouseButton::Middle => 1,
            MouseButton::Right => 2,
            MouseButton::None => 3,
            MouseButton::WheelUp => 64,
            MouseButton::WheelDown => 65,
            MouseButton::WheelLeft => 66,
            MouseButton::WheelRight => 67,
            MouseButton::Back => 128,
            MouseButton::Forward => 129,
        }
    }

    fn is_wheel(self) -> bool {
        matches!(self, MouseButton::WheelUp | MouseButton::WheelDown | MouseButton::WheelLeft | MouseButton::WheelRight)
    }
}

/// Encode a mouse event for the program, or `None` if the current mode doesn't report it.
pub fn encode_mouse(ev: &MouseEvent, modes: &Modes) -> Option<Vec<u8>> {
    use MouseAction::*;
    let tracking = modes.mouse;
    let reported = match (tracking, ev.action) {
        (MouseTracking::None, _) => false,
        (MouseTracking::X10, Press) => !ev.button.is_wheel(),
        (MouseTracking::X10, _) => false,
        (_, Press) => true,
        (_, Release) => !ev.button.is_wheel(),
        (MouseTracking::Normal, Motion) => false,
        (MouseTracking::ButtonEvent, Motion) => ev.button != MouseButton::None,
        (MouseTracking::AnyEvent, Motion) => true,
    };
    if !reported {
        return None;
    }

    let sgr = matches!(modes.mouse_encoding, MouseEncoding::Sgr | MouseEncoding::SgrPixels);
    let mut code = if ev.action == Release && !sgr { 3 } else { ev.button.code() };
    if ev.action == Motion {
        code += 32;
    }
    if tracking != MouseTracking::X10 {
        let m = ev.mods;
        code += 4 * m.shift as u32 + 8 * m.alt as u32 + 16 * m.ctrl as u32;
    }

    let (x, y) = (ev.col + 1, ev.row + 1);
    match modes.mouse_encoding {
        MouseEncoding::Sgr => {
            let f = if ev.action == Release { 'm' } else { 'M' };
            Some(format!("\x1b[<{code};{x};{y}{f}").into_bytes())
        }
        MouseEncoding::SgrPixels => {
            let f = if ev.action == Release { 'm' } else { 'M' };
            Some(format!("\x1b[<{code};{};{}{f}", ev.px.0 + 1, ev.px.1 + 1).into_bytes())
        }
        MouseEncoding::Urxvt => Some(format!("\x1b[{};{x};{y}M", code + 32).into_bytes()),
        MouseEncoding::X10 => {
            // Classic encoding can't represent coordinates past 223.
            if x > 223 || y > 223 || code > 223 {
                return None;
            }
            Some(vec![0x1b, b'[', b'M', (32 + code) as u8, (32 + x) as u8, (32 + y) as u8])
        }
        MouseEncoding::Utf8 => {
            if x > 2015 || y > 2015 {
                return None;
            }
            let mut out = b"\x1b[M".to_vec();
            for v in [32 + code, 32 + x as u32, 32 + y as u32] {
                let mut buf = [0; 4];
                out.extend_from_slice(char::from_u32(v)?.encode_utf8(&mut buf).as_bytes());
            }
            Some(out)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn modes(t: MouseTracking, e: MouseEncoding) -> Modes {
        Modes { mouse: t, mouse_encoding: e, ..Modes::default() }
    }

    fn ev(button: MouseButton, action: MouseAction, col: usize, row: usize) -> MouseEvent {
        MouseEvent { button, action, col, row, px: (col * 10, row * 20), mods: Mods::NONE }
    }

    fn enc(e: MouseEvent, m: &Modes) -> Option<String> {
        encode_mouse(&e, m).map(|b| String::from_utf8_lossy(&b).into_owned())
    }

    use MouseAction::*;
    use MouseButton::*;

    #[test]
    fn disabled_reports_nothing() {
        let m = Modes::default();
        assert!(enc(ev(Left, Press, 0, 0), &m).is_none());
    }

    #[test]
    fn sgr_press_release_motion() {
        let m = modes(MouseTracking::ButtonEvent, MouseEncoding::Sgr);
        assert_eq!(enc(ev(Left, Press, 4, 2), &m).unwrap(), "\x1b[<0;5;3M");
        assert_eq!(enc(ev(Left, Release, 4, 2), &m).unwrap(), "\x1b[<0;5;3m");
        assert_eq!(enc(ev(Left, Motion, 5, 2), &m).unwrap(), "\x1b[<32;6;3M");
        assert!(enc(ev(None, Motion, 5, 2), &m).is_none());
        assert_eq!(enc(ev(WheelDown, Press, 0, 0), &m).unwrap(), "\x1b[<65;1;1M");
        let ctrl = MouseEvent { mods: Mods { ctrl: true, ..Mods::NONE }, ..ev(Right, Press, 0, 0) };
        assert_eq!(enc(ctrl, &m).unwrap(), "\x1b[<18;1;1M");
    }

    #[test]
    fn any_event_reports_bare_motion() {
        let m = modes(MouseTracking::AnyEvent, MouseEncoding::Sgr);
        assert_eq!(enc(ev(None, Motion, 0, 0), &m).unwrap(), "\x1b[<35;1;1M");
    }

    #[test]
    fn x10_encoding_and_limits() {
        let m = modes(MouseTracking::Normal, MouseEncoding::X10);
        assert_eq!(encode_mouse(&ev(Left, Press, 0, 0), &m).unwrap(), b"\x1b[M !!");
        assert_eq!(encode_mouse(&ev(Left, Release, 0, 0), &m).unwrap(), b"\x1b[M#!!");
        assert!(encode_mouse(&ev(Left, Press, 300, 0), &m).is_none());
        let x10 = modes(MouseTracking::X10, MouseEncoding::X10);
        assert!(encode_mouse(&ev(Left, Release, 0, 0), &x10).is_none());
        assert!(encode_mouse(&ev(WheelUp, Press, 0, 0), &x10).is_none());
    }

    #[test]
    fn utf8_urxvt_pixels() {
        let m = modes(MouseTracking::Normal, MouseEncoding::Utf8);
        let out = encode_mouse(&ev(Left, Press, 300, 0), &m).unwrap();
        assert_eq!(&out[..4], b"\x1b[M ");
        assert_eq!(std::str::from_utf8(&out[4..]).unwrap().chars().next().unwrap() as u32, 32 + 301);
        let m = modes(MouseTracking::Normal, MouseEncoding::Urxvt);
        assert_eq!(enc(ev(Left, Press, 1, 1), &m).unwrap(), "\x1b[32;2;2M");
        let m = modes(MouseTracking::Normal, MouseEncoding::SgrPixels);
        assert_eq!(enc(ev(Left, Press, 1, 1), &m).unwrap(), "\x1b[<0;11;21M");
    }
}
