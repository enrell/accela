//! Small allocation-conscious helpers: base64 and percent-decoding.

/// Decode standard base64 (padding optional, whitespace ignored). Returns `None` on
/// invalid input or if the output would exceed `max_len`.
pub fn base64_decode(input: &[u8], max_len: usize) -> Option<Vec<u8>> {
    fn val(b: u8) -> Option<u32> {
        Some(match b {
            b'A'..=b'Z' => b - b'A',
            b'a'..=b'z' => b - b'a' + 26,
            b'0'..=b'9' => b - b'0' + 52,
            b'+' | b'-' => 62,
            b'/' | b'_' => 63,
            _ => return None,
        } as u32)
    }
    if input.len() / 4 * 3 > max_len.saturating_add(3) {
        return None;
    }
    let mut out = Vec::with_capacity(input.len() / 4 * 3);
    let mut acc = 0u32;
    let mut bits = 0;
    for &b in input {
        if b == b'=' {
            break;
        }
        if b.is_ascii_whitespace() {
            continue;
        }
        acc = (acc << 6) | val(b)?;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
            acc &= (1 << bits) - 1;
        }
    }
    if out.len() > max_len { None } else { Some(out) }
}

pub fn base64_encode(input: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(input.len().div_ceil(3) * 4);
    for chunk in input.chunks(3) {
        let b = [chunk[0], *chunk.get(1).unwrap_or(&0), *chunk.get(2).unwrap_or(&0)];
        let n = ((b[0] as u32) << 16) | ((b[1] as u32) << 8) | b[2] as u32;
        out.push(T[(n >> 18) as usize & 63] as char);
        out.push(T[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 { T[(n >> 6) as usize & 63] as char } else { '=' });
        out.push(if chunk.len() > 2 { T[n as usize & 63] as char } else { '=' });
    }
    out
}

/// Decode `%XX` escapes. Invalid escapes are kept literally.
pub fn percent_decode(s: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(s.len());
    let mut i = 0;
    while i < s.len() {
        if s[i] == b'%'
            && i + 2 < s.len()
            && let (Some(h), Some(l)) = (hex(s[i + 1]), hex(s[i + 2]))
        {
            out.push(h << 4 | l);
            i += 3;
            continue;
        }
        out.push(s[i]);
        i += 1;
    }
    out
}

fn hex(b: u8) -> Option<u8> {
    (b as char).to_digit(16).map(|v| v as u8)
}

/// Strip C0/C1 control characters (titles, notifications) and cap the length in chars.
pub fn sanitize_text(s: &str, max_chars: usize) -> String {
    s.chars().filter(|c| !c.is_control()).take(max_chars).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_roundtrip() {
        for s in ["", "f", "fo", "foo", "foob", "fooba", "foobar", "çãé🦀"] {
            let e = base64_encode(s.as_bytes());
            assert_eq!(base64_decode(e.as_bytes(), 1024).unwrap(), s.as_bytes(), "{s}");
        }
        assert_eq!(base64_encode(b"foobar"), "Zm9vYmFy");
        assert_eq!(base64_decode(b"Zm9v\nYmE=", 100).unwrap(), b"fooba");
    }

    #[test]
    fn base64_rejects_bad_input_and_oversize() {
        assert!(base64_decode(b"Zm9v!", 100).is_none());
        assert!(base64_decode(b"Zm9vYmFy", 3).is_none());
    }

    #[test]
    fn percent() {
        assert_eq!(percent_decode(b"/a%20b%2Fc"), b"/a b/c");
        assert_eq!(percent_decode(b"%zz%4"), b"%zz%4");
    }

    #[test]
    fn sanitize() {
        assert_eq!(sanitize_text("a\x1b]0;x\x07b\u{9b}c", 100), "a]0;xbc");
        assert_eq!(sanitize_text("abcdef", 3), "abc");
    }
}
