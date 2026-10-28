//! Recovery code: 256 random bits shown as Crockford base32 in groups of
//! four, with two check characters, e.g. `7K2Q-…-XY`. Typing errors are
//! caught by the check characters; `O`, `I` and `L` are read as `0`, `1`.

use crate::{random, sha256, Error, Secret};

const ALPHABET: &[u8; 32] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";

pub struct RecoveryCode(Secret);

impl RecoveryCode {
    pub fn generate() -> Result<RecoveryCode, Error> {
        let mut b = [0u8; 32];
        random(&mut b)?;
        Ok(RecoveryCode(Secret::from_bytes(b)))
    }

    #[cfg(test)]
    pub(crate) fn from_secret(s: Secret) -> RecoveryCode {
        RecoveryCode(s)
    }

    pub fn secret(&self) -> &Secret {
        &self.0
    }

    /// The code as shown to the user (52 data + 2 check characters).
    pub fn display(&self) -> String {
        let mut chars = encode(self.0.as_bytes());
        let check = sha256(&[b"beatlumen recovery check v1", self.0.as_bytes()]);
        chars.push(ALPHABET[(check[0] & 31) as usize] as char);
        chars.push(ALPHABET[(check[1] & 31) as usize] as char);
        chars
            .as_bytes()
            .chunks(4)
            .map(|c| std::str::from_utf8(c).expect("ascii"))
            .collect::<Vec<_>>()
            .join("-")
    }

    /// Parses a code as typed by the user.
    pub fn parse(text: &str) -> Result<RecoveryCode, Error> {
        let mut clean = String::new();
        for c in text.chars() {
            let c = c.to_ascii_uppercase();
            let c = match c {
                'O' => '0',
                'I' | 'L' => '1',
                '-' | ' ' | '\t' | '\n' | '\r' => continue,
                c => c,
            };
            if !ALPHABET.contains(&(c as u8)) || !c.is_ascii() {
                return Err(Error::Format("invalid character in recovery code"));
            }
            clean.push(c);
        }
        if clean.len() != 54 {
            return Err(Error::Format("recovery code length"));
        }
        let bytes = decode(&clean[..52]).ok_or(Error::Format("recovery code"))?;
        let code = RecoveryCode(Secret::from_bytes(bytes));
        let expected = code.display().replace('-', "");
        if expected[52..] != clean[52..] {
            return Err(Error::Format("recovery code check characters"));
        }
        Ok(code)
    }
}

fn encode(data: &[u8; 32]) -> String {
    let mut out = String::with_capacity(52);
    let mut acc = 0u32;
    let mut bits = 0;
    for &b in data {
        acc = acc << 8 | b as u32;
        bits += 8;
        while bits >= 5 {
            bits -= 5;
            out.push(ALPHABET[(acc >> bits & 31) as usize] as char);
        }
    }
    if bits > 0 {
        out.push(ALPHABET[(acc << (5 - bits) & 31) as usize] as char);
    }
    out
}

fn decode(text: &str) -> Option<[u8; 32]> {
    let mut out = Vec::with_capacity(33);
    let mut acc = 0u32;
    let mut bits = 0;
    for c in text.bytes() {
        let v = ALPHABET.iter().position(|&a| a == c)? as u32;
        acc = acc << 5 | v;
        bits += 5;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
        }
    }
    out.truncate(32);
    out.try_into().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_parse() {
        let c = RecoveryCode::generate().unwrap();
        let shown = c.display();
        assert_eq!(shown.len(), 54 + 13);
        let back = RecoveryCode::parse(&shown).unwrap();
        assert_eq!(back.secret(), c.secret());
        // Lower case, spaces, O for 0 are accepted.
        let typed = shown.to_lowercase().replace('-', " ").replace('0', "o");
        assert_eq!(RecoveryCode::parse(&typed).unwrap().secret(), c.secret());
        // A typo is caught by the check characters (almost always).
        let mut chars: Vec<char> = shown.chars().collect();
        chars[0] = if chars[0] == 'A' { 'B' } else { 'A' };
        let typo: String = chars.into_iter().collect();
        assert!(RecoveryCode::parse(&typo).is_err() || RecoveryCode::parse(&typo).unwrap().secret() != c.secret());
        assert!(RecoveryCode::parse("too-short").is_err());
        assert!(RecoveryCode::parse(&shown.replace('-', "U")).is_err());
    }
}
