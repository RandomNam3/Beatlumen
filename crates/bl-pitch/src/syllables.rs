//! Timed words into timed syllables (one note each).

use crate::notes::Syllable;

/// Splits pieces that are whole words into syllables (hyphenation
/// patterns of `lang`), spreading each word's time over its syllables by
/// their letters. A word's text keeps the space after it, a piece within a
/// word has none (as in the song text); words already in pieces stay as
/// they are.
pub fn syllabify(pieces: &[Syllable], lang: &str) -> Vec<Syllable> {
    let mut out = Vec::with_capacity(pieces.len() * 2);
    let mut word_start = 0;
    for i in 0..pieces.len() {
        let ends_word = i + 1 == pieces.len() || pieces[i].text.ends_with(char::is_whitespace);
        if !ends_word {
            continue;
        }
        if i == word_start {
            split_word(&pieces[i], lang, &mut out);
        } else {
            out.extend_from_slice(&pieces[word_start..=i]);
        }
        word_start = i + 1;
    }
    out
}

fn split_word(p: &Syllable, lang: &str, out: &mut Vec<Syllable>) {
    let word = p.text.trim_end();
    let space = &p.text[word.len()..];
    let parts = bl_hyph::syllables(word, lang);
    if parts.len() < 2 {
        out.push(p.clone());
        return;
    }
    let total: usize = parts.iter().map(|s| s.chars().count()).sum();
    let mut t = p.start;
    let mut done = 0usize;
    let last = parts.len() - 1;
    for (k, s) in parts.into_iter().enumerate() {
        done += s.chars().count();
        let end = if k == last {
            p.end
        } else {
            p.start + (p.end - p.start) * done as f64 / total.max(1) as f64
        };
        let text = if k == last { s + space } else { s };
        out.push(Syllable {
            start: t,
            end,
            text,
        });
        t = end;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(start: f64, end: f64, text: &str) -> Syllable {
        Syllable {
            start,
            end,
            text: text.into(),
        }
    }

    #[test]
    fn splits_whole_words_only() {
        let words = [
            s(0.0, 1.0, "wonderful "),
            s(1.0, 1.4, "hap"),
            s(1.4, 2.0, "py "),
            s(2.0, 2.5, "day"),
        ];
        let out = syllabify(&words, "en");
        let texts: Vec<&str> = out.iter().map(|x| x.text.as_str()).collect();
        assert_eq!(texts.concat(), "wonderful happy day");
        assert!(texts.len() >= 5, "{texts:?}");
        assert_eq!(texts[texts.len() - 3..], ["hap", "py ", "day"]);
        // Times cover the word, in order.
        assert_eq!(out[0].start, 0.0);
        let w_end = out.iter().position(|x| x.text.ends_with(' ')).unwrap();
        assert_eq!(out[w_end].end, 1.0);
        assert!(out.windows(2).all(|w| w[0].end <= w[1].start + 1e-12));
    }
}
