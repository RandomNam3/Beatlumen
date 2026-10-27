//! Networking for Beatlumen (std only):
//!
//! * [`http`]: HTTP/1.1 message primitives.
//! * [`client`]: blocking client and a seekable range-request stream
//!   (native platforms only).
//! * [`server`]: a small threaded HTTP/1.1 server.
//! * [`api`]: request/response types of the Beatlumen server API.

pub mod api;
#[cfg(not(target_arch = "wasm32"))]
pub mod client;
pub mod http;
#[cfg(not(target_arch = "wasm32"))]
pub mod server;

/// Encodes bytes as standard base64 (with padding).
pub fn base64_encode(data: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for c in data.chunks(3) {
        let b = [c[0], *c.get(1).unwrap_or(&0), *c.get(2).unwrap_or(&0)];
        let n = ((b[0] as u32) << 16) | ((b[1] as u32) << 8) | b[2] as u32;
        out.push(T[(n >> 18) as usize & 63] as char);
        out.push(T[(n >> 12) as usize & 63] as char);
        out.push(if c.len() > 1 {
            T[(n >> 6) as usize & 63] as char
        } else {
            '='
        });
        out.push(if c.len() > 2 {
            T[n as usize & 63] as char
        } else {
            '='
        });
    }
    out
}

/// Decodes standard or URL-safe base64 (padding optional, whitespace ignored).
pub fn base64_decode(s: &str) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(s.len() * 3 / 4);
    let mut acc = 0u32;
    let mut bits = 0;
    for c in s.bytes() {
        let v = match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' | b'-' => 62,
            b'/' | b'_' => 63,
            b'=' => break,
            b' ' | b'\n' | b'\r' | b'\t' => continue,
            _ => return None,
        };
        acc = (acc << 6) | v as u32;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    #[test]
    fn base64() {
        for s in ["", "f", "fo", "foo", "foob", "fooba", "foobar"] {
            let e = super::base64_encode(s.as_bytes());
            assert_eq!(super::base64_decode(&e).unwrap(), s.as_bytes());
        }
        assert_eq!(super::base64_encode(b"foobar"), "Zm9vYmFy");
    }
}
