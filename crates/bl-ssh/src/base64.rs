//! Base64 (RFC 4648, standard alphabet): fingerprints and key files.

const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

/// Encodes with `=` padding when `pad`, without it otherwise (fingerprints).
pub fn encode(data: &[u8], pad: bool) -> String {
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for c in data.chunks(3) {
        let n = (c[0] as u32) << 16
            | (*c.get(1).unwrap_or(&0) as u32) << 8
            | *c.get(2).unwrap_or(&0) as u32;
        for i in 0..=c.len() {
            out.push(ALPHABET[(n >> (18 - 6 * i)) as usize & 63] as char);
        }
        if pad {
            for _ in c.len()..3 {
                out.push('=');
            }
        }
    }
    out
}

/// Decodes, ignoring whitespace (line breaks in key files); padding is
/// optional. None for any other character or a malformed end.
pub fn decode(text: &str) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(text.len() / 4 * 3);
    let mut acc = 0u32;
    let mut bits = 0u32;
    let mut ended = false;
    for b in text.bytes() {
        let v = match b {
            b'A'..=b'Z' => b - b'A',
            b'a'..=b'z' => b - b'a' + 26,
            b'0'..=b'9' => b - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            b'=' => {
                ended = true;
                continue;
            }
            b' ' | b'\t' | b'\r' | b'\n' => continue,
            _ => return None,
        };
        if ended {
            return None;
        }
        acc = acc << 6 | v as u32;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
            acc &= (1 << bits) - 1;
        }
    }
    // Six leftover bits cannot come from whole bytes; others must be zero.
    if bits >= 6 || acc != 0 {
        return None;
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rfc4648_vectors() {
        let cases = [
            ("", ""),
            ("f", "Zg=="),
            ("fo", "Zm8="),
            ("foo", "Zm9v"),
            ("foob", "Zm9vYg=="),
            ("fooba", "Zm9vYmE="),
            ("foobar", "Zm9vYmFy"),
        ];
        for (plain, enc) in cases {
            assert_eq!(encode(plain.as_bytes(), true), enc);
            assert_eq!(encode(plain.as_bytes(), false), enc.trim_end_matches('='));
            assert_eq!(decode(enc).unwrap(), plain.as_bytes());
            assert_eq!(decode(enc.trim_end_matches('=')).unwrap(), plain.as_bytes());
        }
        assert_eq!(decode("Zm9v\nYmFy\r\n").unwrap(), b"foobar");
        assert!(decode("Zm9v!").is_none());
        assert!(decode("Z").is_none());
        assert!(decode("Zg==Zg").is_none());
    }
}
