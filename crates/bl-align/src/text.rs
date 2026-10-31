//! From a text to the model's characters: each word (split at white
//! space) becomes the vocabulary tokens of its characters, in the
//! vocabulary's case, with letters the model does not know replaced by
//! their plain form (`é` -> `e`, `ß` -> `ss`) and the rest (digits,
//! punctuation) left out. Every character keeps a link to where it came
//! from, so the times of the tokens go back to the words as written.

use std::collections::HashMap;

/// The model's characters: single-character tokens of `vocab.json`.
#[derive(Debug, Clone)]
pub struct Vocab {
    chars: HashMap<char, u32>,
    /// The word delimiter token (`|`), if the model has one.
    pub delimiter: Option<u32>,
    /// Token texts by id (for showing what the model hears).
    pub texts: Vec<String>,
}

/// A word of the input with the tokens of each of its characters.
#[derive(Debug, Clone, PartialEq)]
pub struct Word {
    pub text: String,
    /// Each character and its tokens (none if it is not aligned).
    pub chars: Vec<(char, Vec<u32>)>,
}

impl Word {
    pub fn alignable(&self) -> bool {
        self.chars.iter().any(|(_, t)| !t.is_empty())
    }
}

/// The token sequence to align and where each token came from.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Target {
    pub tokens: Vec<u32>,
    /// (word, character) of each token; `None` for word delimiters.
    pub owners: Vec<Option<(usize, usize)>>,
}

/// Letters with marks, ligatures and other letters to plain lowercase
/// Latin letters, for vocabularies without them.
const FOLD: &[(char, &str)] = &[
    ('à', "a"),
    ('á', "a"),
    ('â', "a"),
    ('ã', "a"),
    ('ä', "a"),
    ('å', "a"),
    ('ā', "a"),
    ('ą', "a"),
    ('æ', "ae"),
    ('ç', "c"),
    ('ć', "c"),
    ('č', "c"),
    ('ď', "d"),
    ('đ', "d"),
    ('è', "e"),
    ('é', "e"),
    ('ê', "e"),
    ('ë', "e"),
    ('ē', "e"),
    ('ė', "e"),
    ('ę', "e"),
    ('ě', "e"),
    ('ğ', "g"),
    ('ì', "i"),
    ('í', "i"),
    ('î', "i"),
    ('ï', "i"),
    ('ī', "i"),
    ('į', "i"),
    ('ı', "i"),
    ('ł', "l"),
    ('ñ', "n"),
    ('ń', "n"),
    ('ň', "n"),
    ('ò', "o"),
    ('ó', "o"),
    ('ô', "o"),
    ('õ', "o"),
    ('ö', "o"),
    ('ø', "o"),
    ('ō', "o"),
    ('ő', "o"),
    ('œ', "oe"),
    ('ŕ', "r"),
    ('ř', "r"),
    ('ś', "s"),
    ('š', "s"),
    ('ş', "s"),
    ('ș', "s"),
    ('ß', "ss"),
    ('ť', "t"),
    ('ţ', "t"),
    ('ț', "t"),
    ('ù', "u"),
    ('ú', "u"),
    ('û', "u"),
    ('ü', "u"),
    ('ū', "u"),
    ('ů', "u"),
    ('ű', "u"),
    ('ý', "y"),
    ('ÿ', "y"),
    ('ź', "z"),
    ('ż', "z"),
    ('ž', "z"),
];

/// Apostrophes and hyphens as the vocabularies write them.
fn canonical(c: char) -> char {
    match c {
        '\u{2018}' | '\u{2019}' | '\u{02bc}' | '`' | '\u{00b4}' => '\'',
        '\u{2010}' | '\u{2011}' => '-',
        c => c,
    }
}

impl Vocab {
    /// The vocabulary from `vocab.json` (token -> id, every id below
    /// `size`) and the word delimiter's text.
    pub fn parse(text: &str, size: usize, delimiter: &str) -> Result<Vocab, String> {
        let v = bl_json::parse(text).map_err(|e| format!("vocab.json: {e}"))?;
        let map = v.as_object().ok_or("vocab.json: not an object")?;
        let mut chars = HashMap::new();
        let mut texts = vec![String::new(); size];
        let mut delim = None;
        for (k, id) in map.iter() {
            let id = id
                .as_u64()
                .filter(|&i| i < size as u64)
                .ok_or_else(|| format!("vocab.json: bad id for {k}"))? as u32;
            texts[id as usize] = k.to_string();
            if k == delimiter {
                delim = Some(id);
                continue;
            }
            let mut it = k.chars();
            if let (Some(c), None) = (it.next(), it.next()) {
                chars.insert(c, id);
            }
        }
        Ok(Vocab {
            chars,
            delimiter: delim,
            texts,
        })
    }

    /// A vocabulary of the given characters (ids from 1; 0 is the
    /// blank), for tests.
    pub fn from_chars(chars: &str, delimiter: Option<char>) -> Vocab {
        let mut texts = vec!["<pad>".to_string()];
        let mut map = HashMap::new();
        let mut delim = None;
        for c in delimiter.into_iter().chain(chars.chars()) {
            let id = texts.len() as u32;
            texts.push(c.to_string());
            if Some(c) == delimiter && delim.is_none() {
                delim = Some(id);
            } else {
                map.insert(c, id);
            }
        }
        Vocab {
            chars: map,
            delimiter: delim,
            texts,
        }
    }

    /// The tokens of all characters of `s`, each in the vocabulary as it
    /// is or in the other case (`None` if one is missing).
    fn spell(&self, s: &str) -> Option<Vec<u32>> {
        let get = |s: String| -> Option<Vec<u32>> {
            s.chars().map(|o| self.chars.get(&o).copied()).collect()
        };
        let mut out = Vec::new();
        for c in s.chars() {
            let t = match self.chars.get(&c) {
                Some(&t) => vec![t],
                // (Upper case may be longer: `ß` -> `SS`.)
                None => {
                    get(c.to_lowercase().collect()).or_else(|| get(c.to_uppercase().collect()))?
                }
            };
            out.extend(t);
        }
        Some(out)
    }

    /// The tokens of one letter: as it is, in the other case, or its
    /// plain form (empty if the model cannot say it).
    pub fn letter(&self, c: char) -> Vec<u32> {
        let c = canonical(c);
        let mut buf = [0u8; 4];
        if let Some(t) = self.spell(c.encode_utf8(&mut buf)) {
            return t;
        }
        let lower: String = c.to_lowercase().collect();
        let plain: String = lower
            .chars()
            .map(|l| {
                FOLD.iter()
                    .find(|(f, _)| *f == l)
                    .map_or_else(|| l.to_string(), |(_, p)| p.to_string())
            })
            .collect();
        self.spell(&plain).unwrap_or_default()
    }

    /// The words of `text` with the tokens of their characters.
    /// Apostrophes and hyphens are only kept between two letters.
    pub fn words(&self, text: &str) -> Vec<Word> {
        text.split_whitespace()
            .map(|w| {
                let cs: Vec<char> = w.chars().collect();
                let chars = cs
                    .iter()
                    .enumerate()
                    .map(|(i, &c)| {
                        let joiner = matches!(canonical(c), '\'' | '-')
                            && i > 0
                            && cs[i - 1].is_alphabetic()
                            && cs.get(i + 1).is_some_and(|n| n.is_alphabetic());
                        let tokens = if c.is_alphabetic() || joiner {
                            self.letter(c)
                        } else {
                            Vec::new()
                        };
                        (c, tokens)
                    })
                    .collect();
                Word {
                    text: w.to_string(),
                    chars,
                }
            })
            .collect()
    }

    /// The tokens of the words, with the delimiter between words.
    pub fn target(&self, words: &[Word]) -> Target {
        let mut t = Target::default();
        for (wi, w) in words.iter().enumerate() {
            if !w.alignable() {
                continue;
            }
            if let (Some(d), false) = (self.delimiter, t.tokens.is_empty()) {
                t.tokens.push(d);
                t.owners.push(None);
            }
            for (ci, (_, tokens)) in w.chars.iter().enumerate() {
                for &tok in tokens {
                    t.tokens.push(tok);
                    t.owners.push(Some((wi, ci)));
                }
            }
        }
        t
    }

    /// The text of tokens (the delimiter as `|`).
    pub fn show(&self, tokens: &[u32]) -> String {
        tokens
            .iter()
            .map(|&t| self.texts.get(t as usize).map_or("?", String::as_str))
            .collect::<Vec<_>>()
            .join(" ")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn upper() -> Vocab {
        Vocab::from_chars("ETAONIHSRDLUMWCFGYPBVK'XJQZ", Some('|'))
    }

    fn german() -> Vocab {
        Vocab::from_chars("'-abcdefghijklmnopqrstuvwxyzäíóöü", Some('|'))
    }

    fn spelled(v: &Vocab, text: &str) -> String {
        let words = v.words(text);
        v.show(&v.target(&words).tokens).replace(' ', "")
    }

    #[test]
    fn spells_in_the_vocabulary_case() {
        let v = upper();
        assert_eq!(spelled(&v, "And so, my fellow"), "AND|SO|MY|FELLOW");
        assert_eq!(spelled(&v, "Don't  you'd"), "DON'T|YOU'D");
        assert_eq!(spelled(&german(), "Ich SEH' die"), "ich|seh|die");
    }

    #[test]
    fn folds_letters_the_model_lacks() {
        let v = upper();
        assert_eq!(spelled(&v, "Café straße Æon"), "CAFE|STRASSE|AEON");
        let g = german();
        assert_eq!(spelled(&g, "Straße Mädchen Ölçek"), "strasse|mädchen|ölcek");
        assert_eq!(
            spelled(&g, "Utopia-City rock’n’roll"),
            "utopia-city|rock'n'roll"
        );
    }

    #[test]
    fn keeps_unalignable_words_and_characters() {
        let v = upper();
        let words = v.words("1, 2, three - (go)");
        let texts: Vec<&str> = words.iter().map(|w| w.text.as_str()).collect();
        assert_eq!(texts, ["1,", "2,", "three", "-", "(go)"]);
        let able: Vec<bool> = words.iter().map(Word::alignable).collect();
        assert_eq!(able, [false, false, true, false, true]);
        let t = v.target(&words);
        assert_eq!(v.show(&t.tokens).replace(' ', ""), "THREE|GO");
        // "(go)": the brackets have no tokens; G and O point at 1 and 2.
        assert_eq!(t.owners[6], Some((4, 1)));
        assert_eq!(t.owners[7], Some((4, 2)));
        assert_eq!(t.owners[5], None);
        assert!(v.target(&v.words("")).tokens.is_empty());
        assert!(v.target(&v.words("42 !!")).tokens.is_empty());
    }

    #[test]
    fn a_vocabulary_without_delimiter() {
        let v = Vocab::from_chars("ab", None);
        assert_eq!(v.show(&v.target(&v.words("ab ba")).tokens), "a b b a");
    }

    #[test]
    fn parses_vocab_json() {
        let v = Vocab::parse(r#"{"<pad>": 0, "<s>": 1, "|": 2, "A": 3, "'": 4}"#, 5, "|").unwrap();
        assert_eq!(v.delimiter, Some(2));
        assert_eq!(v.letter('a'), vec![3]);
        assert_eq!(v.letter('’'), vec![4]);
        assert!(v.letter('b').is_empty());
        assert!(Vocab::parse(r#"{"A": 5}"#, 5, "|").is_err());
        assert!(Vocab::parse("[]", 5, "|").is_err());
    }
}
