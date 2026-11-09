//! VT500-series parser state machine.
//!
//! Based on Paul Williams' DEC ANSI parser diagram (https://vt100.net/emu/dec_ansi_parser),
//! with UTF-8 decoding in the ground state. The parser never allocates on the hot path
//! except for OSC payloads.

const MAX_PARAMS: usize = 32;
const MAX_INTERMEDIATES: usize = 2;
const MAX_OSC: usize = 4096;

/// Receiver of parsed actions.
pub trait Perform {
    /// A printable character.
    fn print(&mut self, c: char);
    /// A run of printable ASCII (0x20..=0x7e). Override for a faster bulk path.
    fn print_ascii(&mut self, s: &[u8]) {
        for &b in s {
            self.print(b as char);
        }
    }
    /// A C0/C1 control byte (BEL, BS, HT, LF, CR...).
    fn execute(&mut self, byte: u8);
    /// `CSI params intermediates final`. `params` holds sub-params flattened; see [`Params`].
    fn csi_dispatch(&mut self, params: &Params, intermediates: &[u8], ignore: bool, action: u8);
    /// `ESC intermediates final`.
    fn esc_dispatch(&mut self, intermediates: &[u8], ignore: bool, byte: u8);
    /// `OSC p1 ; p2 ; ... ST`.
    fn osc_dispatch(&mut self, params: &[&[u8]], bell_terminated: bool);
    /// Start of a DCS string; payload follows through [`Perform::dcs_put`].
    fn dcs_hook(&mut self, _params: &Params, _intermediates: &[u8], _ignore: bool, _action: u8) {}
    fn dcs_put(&mut self, _byte: u8) {}
    fn dcs_unhook(&mut self) {}
    /// Start of an APC string (kitty graphics); payload follows through [`Perform::apc_put`].
    fn apc_start(&mut self) {}
    fn apc_put(&mut self, _byte: u8) {}
    fn apc_end(&mut self) {}
}

/// CSI parameters. Each param may have colon-separated sub-params (e.g. `38:2::r:g:b`).
#[derive(Default, Clone)]
pub struct Params {
    values: [u16; MAX_PARAMS],
    /// For each param index, how many sub-params follow it (including itself).
    groups: [u8; MAX_PARAMS],
    len: usize,
    group_count: usize,
}

impl Params {
    pub fn is_empty(&self) -> bool {
        self.group_count == 0
    }

    /// Number of top-level (semicolon separated) params.
    #[allow(clippy::misnamed_getters)] // `len` counts groups, not raw values.
    pub fn len(&self) -> usize {
        self.group_count
    }

    /// Iterate top-level params; each item is the slice of `param:sub:sub`.
    pub fn iter(&self) -> ParamsIter<'_> {
        ParamsIter { params: self, group: 0, pos: 0 }
    }

    /// First value of the n-th param, or `default` when missing or zero.
    pub fn get(&self, n: usize, default: u16) -> u16 {
        match self.iter().nth(n) {
            Some(g) if g[0] != 0 => g[0],
            _ => default,
        }
    }

    fn clear(&mut self) {
        self.len = 0;
        self.group_count = 0;
    }

    fn push(&mut self, v: u16) {
        if self.len < MAX_PARAMS {
            self.values[self.len] = v;
            self.groups[self.group_count] = 1;
            self.len += 1;
            self.group_count += 1;
        }
    }

    fn push_sub(&mut self, v: u16) {
        if self.len < MAX_PARAMS && self.group_count > 0 {
            self.values[self.len] = v;
            self.groups[self.group_count - 1] += 1;
            self.len += 1;
        }
    }

    fn last_mut(&mut self) -> Option<&mut u16> {
        if self.len == 0 { None } else { Some(&mut self.values[self.len - 1]) }
    }
}

pub struct ParamsIter<'a> {
    params: &'a Params,
    group: usize,
    pos: usize,
}

impl<'a> Iterator for ParamsIter<'a> {
    type Item = &'a [u16];
    fn next(&mut self) -> Option<Self::Item> {
        if self.group >= self.params.group_count {
            return None;
        }
        let n = self.params.groups[self.group] as usize;
        let s = &self.params.values[self.pos..self.pos + n];
        self.group += 1;
        self.pos += n;
        Some(s)
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum State {
    Ground,
    Escape,
    EscapeIntermediate,
    CsiEntry,
    CsiParam,
    CsiIntermediate,
    CsiIgnore,
    DcsEntry,
    DcsParam,
    DcsIntermediate,
    DcsPassthrough,
    DcsIgnore,
    OscString,
    SosPmString,
    ApcString,
}

pub struct Parser {
    state: State,
    params: Params,
    /// Whether a digit was seen for the current param (to distinguish `;;` from `;0;`).
    param_started: bool,
    intermediates: [u8; MAX_INTERMEDIATES],
    intermediate_len: usize,
    ignoring: bool,
    osc: Vec<u8>,
    osc_seen_esc: bool,
    utf8_buf: [u8; 4],
    utf8_len: usize,
    utf8_need: usize,
}

impl Default for Parser {
    fn default() -> Self {
        Self::new()
    }
}

impl Parser {
    pub fn new() -> Self {
        Self {
            state: State::Ground,
            params: Params::default(),
            param_started: false,
            intermediates: [0; MAX_INTERMEDIATES],
            intermediate_len: 0,
            ignoring: false,
            osc: Vec::with_capacity(256),
            osc_seen_esc: false,
            utf8_buf: [0; 4],
            utf8_len: 0,
            utf8_need: 0,
        }
    }

    pub fn advance<P: Perform>(&mut self, p: &mut P, bytes: &[u8]) {
        let mut i = 0;
        while i < bytes.len() {
            // Fast path: runs of printable ASCII in ground state.
            if self.state == State::Ground && self.utf8_need == 0 {
                let start = i;
                while i < bytes.len() && (0x20..0x7f).contains(&bytes[i]) {
                    i += 1;
                }
                if i > start {
                    p.print_ascii(&bytes[start..i]);
                }
                if i >= bytes.len() {
                    break;
                }
            }
            self.byte(p, bytes[i]);
            i += 1;
        }
    }

    fn clear(&mut self) {
        self.params.clear();
        self.param_started = false;
        self.intermediate_len = 0;
        self.ignoring = false;
    }

    fn collect(&mut self, b: u8) {
        if self.intermediate_len < MAX_INTERMEDIATES {
            self.intermediates[self.intermediate_len] = b;
            self.intermediate_len += 1;
        } else {
            self.ignoring = true;
        }
    }

    fn param(&mut self, b: u8) {
        match b {
            b'0'..=b'9' => {
                if !self.param_started {
                    self.params.push(0);
                    self.param_started = true;
                }
                if let Some(v) = self.params.last_mut() {
                    *v = v.saturating_mul(10).saturating_add((b - b'0') as u16);
                }
            }
            b';' => {
                if !self.param_started {
                    self.params.push(0);
                }
                self.param_started = false;
            }
            b':' => {
                if !self.param_started {
                    self.params.push(0);
                }
                self.params.push_sub(0);
                // Sub-param digits accumulate into the newly pushed slot.
                self.param_started = true;
            }
            _ => {}
        }
    }

    fn intermediates(&self) -> &[u8] {
        &self.intermediates[..self.intermediate_len]
    }

    fn print_utf8<P: Perform>(&mut self, p: &mut P, b: u8) {
        if self.utf8_need == 0 {
            let need = match b {
                0xc2..=0xdf => 2,
                0xe0..=0xef => 3,
                0xf0..=0xf4 => 4,
                _ => {
                    p.print('\u{FFFD}');
                    return;
                }
            };
            self.utf8_buf[0] = b;
            self.utf8_len = 1;
            self.utf8_need = need;
            return;
        }
        if b & 0xc0 != 0x80 {
            // Invalid continuation: emit replacement and reprocess this byte.
            self.utf8_need = 0;
            p.print('\u{FFFD}');
            self.byte(p, b);
            return;
        }
        self.utf8_buf[self.utf8_len] = b;
        self.utf8_len += 1;
        if self.utf8_len == self.utf8_need {
            self.utf8_need = 0;
            let c = std::str::from_utf8(&self.utf8_buf[..self.utf8_len])
                .ok()
                .and_then(|s| s.chars().next())
                .unwrap_or('\u{FFFD}');
            p.print(c);
        }
    }

    fn byte<P: Perform>(&mut self, p: &mut P, b: u8) {
        // Mid UTF-8 sequence in ground state.
        if self.state == State::Ground && self.utf8_need != 0 {
            self.print_utf8(p, b);
            return;
        }

        // "Anywhere" transitions.
        match b {
            0x18 | 0x1a => {
                self.end_string(p);
                p.execute(b);
                self.state = State::Ground;
                return;
            }
            0x1b => {
                if self.state == State::OscString {
                    self.osc_seen_esc = true;
                    return;
                }
                // ESC terminates DCS/APC strings; `ESC \\` then dispatches as a no-op ESC.
                self.end_string(p);
                self.clear();
                self.state = State::Escape;
                return;
            }
            _ => {}
        }

        match self.state {
            State::Ground => match b {
                0x00..=0x1f => p.execute(b),
                0x20..=0x7e => p.print(b as char),
                0x7f => {}
                _ => self.print_utf8(p, b),
            },
            State::Escape => match b {
                0x00..=0x1f => p.execute(b),
                0x20..=0x2f => {
                    self.collect(b);
                    self.state = State::EscapeIntermediate;
                }
                b'[' => {
                    self.clear();
                    self.state = State::CsiEntry;
                }
                b']' => {
                    self.osc.clear();
                    self.osc_seen_esc = false;
                    self.state = State::OscString;
                }
                b'P' => {
                    self.clear();
                    self.state = State::DcsEntry;
                }
                b'X' | b'^' => self.state = State::SosPmString,
                b'_' => {
                    p.apc_start();
                    self.state = State::ApcString;
                }
                0x30..=0x7e => {
                    p.esc_dispatch(self.intermediates(), self.ignoring, b);
                    self.state = State::Ground;
                }
                _ => {}
            },
            State::EscapeIntermediate => match b {
                0x00..=0x1f => p.execute(b),
                0x20..=0x2f => self.collect(b),
                0x30..=0x7e => {
                    p.esc_dispatch(self.intermediates(), self.ignoring, b);
                    self.state = State::Ground;
                }
                _ => {}
            },
            State::CsiEntry => match b {
                0x00..=0x1f => p.execute(b),
                0x20..=0x2f => {
                    self.collect(b);
                    self.state = State::CsiIntermediate;
                }
                b'0'..=b'9' | b';' | b':' => {
                    self.param(b);
                    self.state = State::CsiParam;
                }
                0x3c..=0x3f => {
                    // Private marker: < = > ?
                    self.collect(b);
                    self.state = State::CsiParam;
                }
                0x40..=0x7e => self.csi_end(p, b),
                _ => {}
            },
            State::CsiParam => match b {
                0x00..=0x1f => p.execute(b),
                b'0'..=b'9' | b';' | b':' => self.param(b),
                0x3c..=0x3f => self.state = State::CsiIgnore,
                0x20..=0x2f => {
                    self.collect(b);
                    self.state = State::CsiIntermediate;
                }
                0x40..=0x7e => self.csi_end(p, b),
                _ => {}
            },
            State::CsiIntermediate => match b {
                0x00..=0x1f => p.execute(b),
                0x20..=0x2f => self.collect(b),
                0x30..=0x3f => self.state = State::CsiIgnore,
                0x40..=0x7e => self.csi_end(p, b),
                _ => {}
            },
            State::CsiIgnore => match b {
                0x00..=0x1f => p.execute(b),
                0x40..=0x7e => self.state = State::Ground,
                _ => {}
            },
            State::DcsEntry => match b {
                0x20..=0x2f => {
                    self.collect(b);
                    self.state = State::DcsIntermediate;
                }
                b'0'..=b'9' | b';' | b':' => {
                    self.param(b);
                    self.state = State::DcsParam;
                }
                0x3c..=0x3f => {
                    self.collect(b);
                    self.state = State::DcsParam;
                }
                0x40..=0x7e => self.dcs_hook(p, b),
                _ => {}
            },
            State::DcsParam => match b {
                b'0'..=b'9' | b';' | b':' => self.param(b),
                0x3c..=0x3f => self.state = State::DcsIgnore,
                0x20..=0x2f => {
                    self.collect(b);
                    self.state = State::DcsIntermediate;
                }
                0x40..=0x7e => self.dcs_hook(p, b),
                _ => {}
            },
            State::DcsIntermediate => match b {
                0x20..=0x2f => self.collect(b),
                0x30..=0x3f => self.state = State::DcsIgnore,
                0x40..=0x7e => self.dcs_hook(p, b),
                _ => {}
            },
            State::DcsPassthrough => {
                if b != 0x7f {
                    p.dcs_put(b);
                }
            }
            State::ApcString => {
                if b != 0x7f {
                    p.apc_put(b);
                }
            }
            State::DcsIgnore | State::SosPmString => {}
            State::OscString => {
                if self.osc_seen_esc {
                    // ESC \ (ST) or any ESC terminates the OSC.
                    self.osc_seen_esc = false;
                    self.osc_end(p, false);
                    self.state = State::Ground;
                    if b != b'\\' {
                        self.clear();
                        self.state = State::Escape;
                        self.byte(p, b);
                    }
                    return;
                }
                match b {
                    0x07 => {
                        self.osc_end(p, true);
                        self.state = State::Ground;
                    }
                    0x00..=0x1f => {}
                    _ => {
                        if self.osc.len() < MAX_OSC {
                            self.osc.push(b);
                        }
                    }
                }
            }
        }
    }

    fn dcs_hook<P: Perform>(&mut self, p: &mut P, b: u8) {
        let params = std::mem::take(&mut self.params);
        p.dcs_hook(&params, self.intermediates(), self.ignoring, b);
        self.params = params;
        self.state = State::DcsPassthrough;
    }

    /// Close a DCS/APC string if one is open.
    fn end_string<P: Perform>(&mut self, p: &mut P) {
        match self.state {
            State::DcsPassthrough => p.dcs_unhook(),
            State::ApcString => p.apc_end(),
            _ => {}
        }
    }

    fn csi_end<P: Perform>(&mut self, p: &mut P, b: u8) {
        let ignore = self.ignoring;
        let params = std::mem::take(&mut self.params);
        p.csi_dispatch(&params, self.intermediates(), ignore, b);
        self.params = params;
        self.state = State::Ground;
    }

    fn osc_end<P: Perform>(&mut self, p: &mut P, bell: bool) {
        let mut parts: [&[u8]; 16] = [&[]; 16];
        let mut n = 0;
        for part in self.osc.split(|&c| c == b';') {
            if n == parts.len() {
                break;
            }
            parts[n] = part;
            n += 1;
        }
        p.osc_dispatch(&parts[..n], bell);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Default)]
    struct Log(Vec<String>);

    impl Perform for Log {
        fn print(&mut self, c: char) {
            self.0.push(format!("p{c}"));
        }
        fn execute(&mut self, b: u8) {
            self.0.push(format!("x{b:02x}"));
        }
        fn csi_dispatch(&mut self, params: &Params, inter: &[u8], _ig: bool, a: u8) {
            let ps: Vec<String> =
                params.iter().map(|g| g.iter().map(|v| v.to_string()).collect::<Vec<_>>().join(":")).collect();
            self.0.push(format!("csi{}[{}]{}", String::from_utf8_lossy(inter), ps.join(";"), a as char));
        }
        fn esc_dispatch(&mut self, inter: &[u8], _ig: bool, b: u8) {
            self.0.push(format!("esc{}{}", String::from_utf8_lossy(inter), b as char));
        }
        fn osc_dispatch(&mut self, params: &[&[u8]], _bell: bool) {
            let ps: Vec<_> = params.iter().map(|p| String::from_utf8_lossy(p).into_owned()).collect();
            self.0.push(format!("osc[{}]", ps.join(";")));
        }
        fn dcs_hook(&mut self, params: &Params, inter: &[u8], _ig: bool, a: u8) {
            self.0.push(format!("dcs{}{}{}", String::from_utf8_lossy(inter), params.len(), a as char));
        }
        fn dcs_put(&mut self, b: u8) {
            self.0.push(format!("d{}", b as char));
        }
        fn dcs_unhook(&mut self) {
            self.0.push("unhook".into());
        }
        fn apc_start(&mut self) {
            self.0.push("apc".into());
        }
        fn apc_put(&mut self, b: u8) {
            self.0.push(format!("a{}", b as char));
        }
        fn apc_end(&mut self) {
            self.0.push("apc_end".into());
        }
    }

    fn run(input: &[u8]) -> Vec<String> {
        let mut p = Parser::new();
        let mut log = Log::default();
        p.advance(&mut log, input);
        log.0
    }

    #[test]
    fn prints_and_controls() {
        assert_eq!(run(b"a\r\n"), ["pa", "x0d", "x0a"]);
    }

    #[test]
    fn utf8() {
        assert_eq!(run("é→🦀".as_bytes()), ["pé", "p→", "p🦀"]);
    }

    #[test]
    fn csi_params() {
        assert_eq!(run(b"\x1b[1;31m"), ["csi[1;31]m"]);
        assert_eq!(run(b"\x1b[H"), ["csi[]H"]);
        assert_eq!(run(b"\x1b[;5H"), ["csi[0;5]H"]);
        assert_eq!(run(b"\x1b[?1049h"), ["csi?[1049]h"]);
        assert_eq!(run(b"\x1b[38:2::1:2:3m"), ["csi[38:2:0:1:2:3]m"]);
    }

    #[test]
    fn osc_title() {
        assert_eq!(run(b"\x1b]0;hello\x07"), ["osc[0;hello]"]);
        assert_eq!(run(b"\x1b]2;hi\x1b\\x"), ["osc[2;hi]", "px"]);
    }

    #[test]
    fn dcs_and_apc() {
        assert_eq!(run(b"\x1bP$qm\x1b\\"), ["dcs$0q", "dm", "unhook", "esc\\"]);
        assert_eq!(run(b"\x1b_Gab\x1b\\x"), ["apc", "aG", "aa", "ab", "apc_end", "esc\\", "px"]);
        // CAN aborts a DCS string.
        assert_eq!(run(b"\x1bPqab\x18z"), ["dcs0q", "da", "db", "unhook", "x18", "pz"]);
    }

    #[test]
    fn utf8_invalid_bytes_become_replacement() {
        assert_eq!(run(b"\xffa"), ["p\u{FFFD}", "pa"]);
        assert_eq!(run(b"\xc3a"), ["p\u{FFFD}", "pa"]);
    }

    #[test]
    fn params_are_capped_and_saturate() {
        let mut long = b"\x1b[".to_vec();
        for _ in 0..100 {
            long.extend_from_slice(b"1;");
        }
        long.extend_from_slice(b"99999999999m");
        let out = run(&long);
        assert_eq!(out.len(), 1);
        assert!(out[0].starts_with("csi[1;1;"));
    }

    #[test]
    fn esc() {
        assert_eq!(run(b"\x1b7\x1b(B"), ["esc7", "esc(B"]);
    }
}
