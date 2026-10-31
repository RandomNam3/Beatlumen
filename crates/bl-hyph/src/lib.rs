//! Syllables of words, from hyphenation patterns: Liang's algorithm with the
//! TeX patterns (hyph-utf8) of English, German, French, Spanish, Italian,
//! Dutch and Portuguese (see `patterns/README.md` for their licences).
//!
//! Song texts use this to fill a sung word syllable by syllable. The
//! patterns are built into the program and read on first use.

#![forbid(unsafe_code)]

use std::collections::HashMap;
use std::sync::OnceLock;

struct Language {
    /// Letters of a pattern (with `.` at a word's edge) and the values
    /// between them (one more than letters).
    patterns: HashMap<Vec<char>, Vec<u8>>,
    /// Longest pattern (letters).
    longest: usize,
    /// Words hyphenated by hand ("as-so-ciate").
    exceptions: HashMap<String, Vec<String>>,
    /// Letters kept together at a word's start and end.
    left: usize,
    right: usize,
}

/// (code, patterns, exceptions, left, right)
const SOURCES: &[(&str, &str, &str, usize, usize)] = &[
    (
        "en",
        include_str!("../patterns/en.pat.txt"),
        include_str!("../patterns/en.hyp.txt"),
        2,
        3,
    ),
    ("de", include_str!("../patterns/de.pat.txt"), "", 2, 2),
    ("fr", include_str!("../patterns/fr.pat.txt"), "", 2, 2),
    ("es", include_str!("../patterns/es.pat.txt"), "", 2, 2),
    ("it", include_str!("../patterns/it.pat.txt"), "", 2, 2),
    (
        "nl",
        include_str!("../patterns/nl.pat.txt"),
        include_str!("../patterns/nl.hyp.txt"),
        2,
        2,
    ),
    (
        "pt",
        include_str!("../patterns/pt.pat.txt"),
        include_str!("../patterns/pt.hyp.txt"),
        2,
        3,
    ),
];

/// The languages with patterns (ISO 639-1 codes).
pub fn languages() -> impl Iterator<Item = &'static str> {
    SOURCES.iter().map(|s| s.0)
}

fn language(code: &str) -> Option<&'static Language> {
    static LOADED: OnceLock<Vec<OnceLock<Language>>> = OnceLock::new();
    let slots = LOADED.get_or_init(|| SOURCES.iter().map(|_| OnceLock::new()).collect());
    let code = code.get(..2).unwrap_or(code).to_ascii_lowercase();
    let i = SOURCES.iter().position(|s| s.0 == code)?;
    let (_, pat, hyp, left, right) = SOURCES[i];
    Some(slots[i].get_or_init(|| parse(pat, hyp, left, right)))
}

fn parse(pat: &str, hyp: &str, left: usize, right: usize) -> Language {
    let mut patterns = HashMap::new();
    let mut longest = 0;
    for p in pat.split_whitespace() {
        let mut letters = Vec::new();
        let mut values = vec![0u8];
        for c in p.chars() {
            match c.to_digit(10) {
                Some(d) => *values.last_mut().expect("one value") = d as u8,
                None => {
                    letters.push(c);
                    values.push(0);
                }
            }
        }
        longest = longest.max(letters.len());
        patterns.insert(letters, values);
    }
    let exceptions = hyp
        .split_whitespace()
        .map(|w| {
            let parts: Vec<String> = w.split('-').map(str::to_string).collect();
            (parts.concat(), parts)
        })
        .collect();
    Language {
        patterns,
        longest,
        exceptions,
        left,
        right,
    }
}

/// The syllables of `word` in language `lang` ("en", "de", …; a longer
/// code like "de-DE" works too). Joined they give the word as it was
/// (punctuation and case kept, it goes with the first or last syllable).
/// A word without patterns for its language, a short word or one with
/// characters other than letters inside comes back whole.
pub fn syllables(word: &str, lang: &str) -> Vec<String> {
    let Some(l) = language(lang) else {
        return vec![word.to_string()];
    };
    // Letters only (apostrophes inside belong to the syllable before).
    let start = word.find(|c: char| c.is_alphabetic()).unwrap_or(word.len());
    let end = word.rfind(|c: char| c.is_alphabetic()).map_or(start, |i| {
        i + word[i..].chars().next().map_or(1, char::len_utf8)
    });
    if start >= end {
        return vec![word.to_string()];
    }
    let core = &word[start..end];
    if core
        .chars()
        .any(|c| !c.is_alphabetic() && c != '\'' && c != '’')
    {
        return vec![word.to_string()];
    }
    let letters: Vec<char> = core.chars().collect();
    let lower: Vec<char> = core.chars().flat_map(char::to_lowercase).collect();
    if lower.len() != letters.len() || letters.len() < l.left + l.right {
        return vec![word.to_string()];
    }
    let breaks: Vec<usize> = match l.exceptions.get(&lower.iter().collect::<String>()) {
        Some(parts) => parts
            .iter()
            .scan(0, |n, p| {
                *n += p.chars().count();
                Some(*n)
            })
            .filter(|&n| n < letters.len())
            .collect(),
        None => liang(l, &lower),
    };
    let mut out = Vec::with_capacity(breaks.len() + 1);
    let mut from = 0;
    for b in breaks.into_iter().chain([letters.len()]) {
        out.push(letters[from..b].iter().collect::<String>());
        from = b;
    }
    out[0].insert_str(0, &word[..start]);
    out.last_mut().expect("a syllable").push_str(&word[end..]);
    out
}

/// Positions (letters before) where a lower-case word may break.
fn liang(l: &Language, word: &[char]) -> Vec<usize> {
    let mut w = Vec::with_capacity(word.len() + 2);
    w.push('.');
    // An apostrophe never decides a break.
    w.extend(word.iter().map(|&c| if c == '’' { '\'' } else { c }));
    w.push('.');
    let mut values = vec![0u8; w.len() + 1];
    for i in 0..w.len() {
        for j in i + 1..=(i + l.longest).min(w.len()) {
            if let Some(v) = l.patterns.get(&w[i..j]) {
                for (k, &d) in v.iter().enumerate() {
                    values[i + k] = values[i + k].max(d);
                }
            }
        }
    }
    // values[i + 1] lies between word letters i - 1 and i.
    (l.left..=word.len() - l.right)
        .filter(|&i| values[i + 1] % 2 == 1 && word[i - 1] != '\'' && word[i] != '\'')
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn split(word: &str, lang: &str) -> String {
        syllables(word, lang).join("-")
    }

    #[test]
    fn words_get_their_syllables() {
        assert_eq!(split("hyphenation", "en"), "hy-phen-ation");
        assert_eq!(split("friends", "en"), "friends");
        assert_eq!(split("together", "en"), "to-gether");
        assert_eq!(split("associate", "en"), "as-so-ciate");
        assert_eq!(split("Zukunft", "de"), "Zu-kunft");
        assert_eq!(split("programmieren", "de-DE"), "pro-gram-mie-ren");
        assert_eq!(split("Schmetterling", "de"), "Schmet-ter-ling");
    }

    #[test]
    fn edges_are_kept() {
        // Punctuation and case stay where they were.
        assert_eq!(split("\"Beautiful,", "en"), "\"Beau-ti-ful,");
        assert_eq!(split("Haven't", "en"), "Haven't");
        // Unknown language, digits, spelled words and short words: whole.
        assert_eq!(split("together", "xx"), "together");
        assert_eq!(split("F-R-I-E-N-D-S", "en"), "F-R-I-E-N-D-S");
        assert_eq!(split("ab12cd", "en"), "ab12cd");
        assert_eq!(split("ooh", "en"), "ooh");
        assert_eq!(split("", "en"), "");
        assert_eq!(split("...", "en"), "...");
    }

    #[test]
    fn all_languages_load() {
        for code in languages() {
            let l = language(code).expect("built in");
            assert!(l.patterns.len() > 100, "{code}");
        }
    }
}
