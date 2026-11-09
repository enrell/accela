//! Theme colors, the 256-color palette and X11 color-spec parsing (OSC 4/10/11/12).

#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
pub struct Rgb(pub u8, pub u8, pub u8);

impl Rgb {
    pub fn to_u32(self) -> u32 {
        ((self.0 as u32) << 16) | ((self.1 as u32) << 8) | self.2 as u32
    }

    pub fn from_u32(v: u32) -> Self {
        Rgb((v >> 16) as u8, (v >> 8) as u8, v as u8)
    }

    /// `rgb:rrrr/gggg/bbbb`, the format xterm uses in color query replies.
    pub fn to_xparse(self) -> String {
        format!("rgb:{0:02x}{0:02x}/{1:02x}{1:02x}/{2:02x}{2:02x}", self.0, self.1, self.2)
    }

    /// Perceived luminance in 0..=255; used to answer "is the theme dark?".
    pub fn luma(self) -> u8 {
        ((self.0 as u32 * 299 + self.1 as u32 * 587 + self.2 as u32 * 114) / 1000) as u8
    }
}

/// Parse `#rgb`, `#rrggbb`, `#rrrgggbbb`, `#rrrrggggbbbb` and `rgb:r/g/b` (1–4 hex digits each).
pub fn parse_spec(s: &[u8]) -> Option<Rgb> {
    let s = std::str::from_utf8(s).ok()?;
    let scale = |h: &str| -> Option<u8> {
        if h.is_empty() || h.len() > 4 {
            return None;
        }
        let v = u32::from_str_radix(h, 16).ok()?;
        let max = (1u32 << (4 * h.len())) - 1;
        Some(((v * 255 + max / 2) / max) as u8)
    };
    if let Some(rest) = s.strip_prefix("rgb:") {
        let mut it = rest.split('/');
        let (r, g, b) = (it.next()?, it.next()?, it.next()?);
        if it.next().is_some() {
            return None;
        }
        return Some(Rgb(scale(r)?, scale(g)?, scale(b)?));
    }
    if let Some(hex) = s.strip_prefix('#') {
        if hex.is_empty() || hex.len() % 3 != 0 || hex.len() > 12 || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
            return None;
        }
        let n = hex.len() / 3;
        // `#` forms are left-aligned: #f00 means 0xf0, not 0xff.
        let part = |i: usize| -> Option<u8> {
            let h = &hex[i * n..(i + 1) * n];
            let v = u32::from_str_radix(h, 16).ok()?;
            Some(if n == 1 { (v << 4) as u8 } else { (v >> (4 * (n - 2))) as u8 })
        };
        return Some(Rgb(part(0)?, part(1)?, part(2)?));
    }
    None
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Theme {
    pub palette: [Rgb; 256],
    pub foreground: Rgb,
    pub background: Rgb,
    pub cursor: Rgb,
    pub selection: Rgb,
}

const TOKYO_NIGHT: [u32; 16] = [
    0x15161e, 0xf7768e, 0x9ece6a, 0xe0af68, 0x7aa2f7, 0xbb9af7, 0x7dcfff, 0xa9b1d6, 0x414868, 0xff899d, 0x9fe044,
    0xfaba4a, 0x8db0ff, 0xc7a9ff, 0xa4daff, 0xc0caf5,
];

/// The standard xterm 256-color cube and grayscale ramp for indices 16..=255.
pub fn xterm_256(i: u8) -> Rgb {
    match i {
        0..=15 => Rgb::from_u32(TOKYO_NIGHT[i as usize]),
        16..=231 => {
            let i = i - 16;
            let step = |v: u8| if v == 0 { 0 } else { 55 + v * 40 };
            Rgb(step(i / 36), step((i / 6) % 6), step(i % 6))
        }
        232..=255 => {
            let v = 8 + (i - 232) * 10;
            Rgb(v, v, v)
        }
    }
}

impl Default for Theme {
    fn default() -> Self {
        let mut palette = [Rgb(0, 0, 0); 256];
        for (i, p) in palette.iter_mut().enumerate() {
            *p = xterm_256(i as u8);
        }
        Self {
            palette,
            foreground: Rgb::from_u32(0xc0caf5),
            background: Rgb::from_u32(0x1a1b26),
            cursor: Rgb::from_u32(0xc0caf5),
            selection: Rgb::from_u32(0x33467c),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spec_formats() {
        assert_eq!(parse_spec(b"#ff8000"), Some(Rgb(255, 128, 0)));
        assert_eq!(parse_spec(b"#f80"), Some(Rgb(0xf0, 0x80, 0)));
        assert_eq!(parse_spec(b"rgb:ff/80/00"), Some(Rgb(255, 128, 0)));
        assert_eq!(parse_spec(b"rgb:ffff/0000/8080"), Some(Rgb(255, 0, 128)));
        assert_eq!(parse_spec(b"rgb:f/0/8"), Some(Rgb(255, 0, 136)));
        assert_eq!(parse_spec(b"#12345"), None);
        assert_eq!(parse_spec(b"rgb:1/2"), None);
        assert_eq!(parse_spec(b"red"), None);
    }

    #[test]
    fn xparse_roundtrip() {
        let c = Rgb(0x12, 0xab, 0xff);
        assert_eq!(c.to_xparse(), "rgb:1212/abab/ffff");
        assert_eq!(parse_spec(c.to_xparse().as_bytes()), Some(c));
    }

    #[test]
    fn cube_and_ramp() {
        assert_eq!(xterm_256(16), Rgb(0, 0, 0));
        assert_eq!(xterm_256(231), Rgb(255, 255, 255));
        assert_eq!(xterm_256(196), Rgb(255, 0, 0));
        assert_eq!(xterm_256(232), Rgb(8, 8, 8));
        assert_eq!(xterm_256(255), Rgb(238, 238, 238));
    }
}
