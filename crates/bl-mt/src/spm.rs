//! SentencePiece unigram tokenizer (the `source.spm` / `target.spm`
//! files of the OPUS-MT models): splits text into the pieces of its
//! vocabulary with the highest total score, and joins pieces back into
//! text.
//!
//! Normalisation is simplified: the models carry a precompiled
//! character map (NFKC plus a few clean-ups) that is not applied here.
//! Text is used as it is, except that whitespace characters become
//! spaces, control characters are dropped, runs of spaces are collapsed
//! (with the model's `remove_extra_whitespaces`), a space is put in
//! front (`add_dummy_prefix`) and spaces are written as U+2581. For the
//! usual song texts (already composed Unicode) the result is the same;
//! characters NFKC would change (full-width forms, ligatures) may end up
//! as unknown pieces.

use crate::proto::{Field, Reader};
use std::collections::HashMap;

/// The whitespace marker of SentencePiece.
pub const SPACE: char = '\u{2581}';
/// Most pieces of a model.
const MAX_PIECES: usize = 1 << 20;
/// Longest piece taking part in segmentation (characters).
const MAX_PIECE_CHARS: usize = 64;
/// Score penalty of an unknown character below the lowest piece score.
const UNK_PENALTY: f32 = 10.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Normal,
    Unknown,
    Control,
    UserDefined,
    Unused,
    Byte,
}

impl Kind {
    fn from_proto(v: u64) -> Option<Kind> {
        Some(match v {
            1 => Kind::Normal,
            2 => Kind::Unknown,
            3 => Kind::Control,
            4 => Kind::UserDefined,
            5 => Kind::Unused,
            6 => Kind::Byte,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone)]
pub struct Piece {
    pub text: String,
    pub score: f32,
    pub kind: Kind,
}

pub struct SentencePiece {
    pieces: Vec<Piece>,
    index: HashMap<String, usize>,
    /// Longest piece used for segmentation (characters).
    max_chars: usize,
    unk: usize,
    /// Score of an unknown character.
    unk_score: f32,
    /// Highest score of a normal piece (user-defined pieces are scored
    /// from it, so they always win).
    max_score: f32,
    add_dummy_prefix: bool,
    remove_extra_whitespaces: bool,
}

/// Reads a SentencePiece message.
fn piece(b: &[u8]) -> Result<Piece, String> {
    let mut p = Piece {
        text: String::new(),
        score: 0.0,
        kind: Kind::Normal,
    };
    let mut r = Reader::new(b);
    while let Some((n, f)) = r.next()? {
        match (n, f) {
            (1, Field::Bytes(s)) => {
                p.text = std::str::from_utf8(s)
                    .map_err(|_| "sentencepiece: piece is not UTF-8")?
                    .to_string();
            }
            (2, f) => p.score = f.as_f32().ok_or("sentencepiece: bad score")?,
            (3, Field::Varint(v)) => {
                p.kind = Kind::from_proto(v).ok_or("sentencepiece: unknown piece type")?
            }
            _ => {}
        }
    }
    Ok(p)
}

fn flag(f: Field) -> Result<bool, String> {
    match f {
        Field::Varint(v) => Ok(v != 0),
        _ => Err("sentencepiece: bad flag".into()),
    }
}

impl SentencePiece {
    /// Reads a model (a serialised `ModelProto`).
    pub fn from_bytes(b: &[u8]) -> Result<SentencePiece, String> {
        let mut pieces = Vec::new();
        let mut add_dummy_prefix = true;
        let mut remove_extra_whitespaces = true;
        let mut r = Reader::new(b);
        while let Some((n, f)) = r.next()? {
            match (n, f) {
                (1, Field::Bytes(m)) => {
                    if pieces.len() >= MAX_PIECES {
                        return Err("sentencepiece: too many pieces".into());
                    }
                    pieces.push(piece(m)?);
                }
                // TrainerSpec: only the model type matters (1 = unigram).
                (2, Field::Bytes(m)) => {
                    let mut t = Reader::new(m);
                    while let Some((n, f)) = t.next()? {
                        if n == 3 && f != Field::Varint(1) {
                            return Err("sentencepiece: not a unigram model".into());
                        }
                    }
                }
                // NormalizerSpec.
                (3, Field::Bytes(m)) => {
                    let mut t = Reader::new(m);
                    while let Some((n, f)) = t.next()? {
                        match n {
                            3 => add_dummy_prefix = flag(f)?,
                            4 => remove_extra_whitespaces = flag(f)?,
                            _ => {}
                        }
                    }
                }
                _ => {}
            }
        }
        Self::new(pieces, add_dummy_prefix, remove_extra_whitespaces)
    }

    /// A model of the given pieces (one of them of kind `Unknown`).
    pub fn new(
        pieces: Vec<Piece>,
        add_dummy_prefix: bool,
        remove_extra_whitespaces: bool,
    ) -> Result<SentencePiece, String> {
        let unk = pieces
            .iter()
            .position(|p| p.kind == Kind::Unknown)
            .ok_or("sentencepiece: no unknown piece")?;
        let mut index = HashMap::with_capacity(pieces.len());
        let mut max_chars = 1;
        let (mut min_score, mut max_score) = (f32::MAX, f32::MIN);
        for (i, p) in pieces.iter().enumerate() {
            if !p.score.is_finite() {
                return Err("sentencepiece: bad score".into());
            }
            // The first piece of a text wins (as in SentencePiece).
            index.entry(p.text.clone()).or_insert(i);
            if matches!(p.kind, Kind::Normal | Kind::UserDefined) {
                let chars = p.text.chars().count();
                if chars <= MAX_PIECE_CHARS {
                    max_chars = max_chars.max(chars);
                }
            }
            if p.kind == Kind::Normal {
                min_score = min_score.min(p.score);
                max_score = max_score.max(p.score);
            }
        }
        if min_score > max_score {
            (min_score, max_score) = (0.0, 0.0);
        }
        Ok(SentencePiece {
            pieces,
            index,
            max_chars,
            unk,
            unk_score: min_score - UNK_PENALTY,
            max_score,
            add_dummy_prefix,
            remove_extra_whitespaces,
        })
    }

    pub fn pieces(&self) -> &[Piece] {
        &self.pieces
    }

    /// The text as the model sees it (see the module documentation).
    pub fn normalize(&self, text: &str) -> String {
        let mut s = String::with_capacity(text.len() + 3);
        if self.add_dummy_prefix {
            s.push(SPACE);
        }
        let prefix = s.len();
        let mut space = false;
        for c in text.chars() {
            if c.is_whitespace() {
                if self.remove_extra_whitespaces {
                    space = true;
                } else {
                    s.push(SPACE);
                }
            } else if !c.is_control() {
                // A run of whitespace between words becomes one space.
                if space && s.len() > prefix {
                    s.push(SPACE);
                }
                space = false;
                s.push(c);
            }
        }
        if s.len() == prefix {
            s.clear();
        }
        s
    }

    /// The pieces of `text` with the highest total score. Characters no
    /// piece covers become the unknown piece (runs of them one piece).
    pub fn encode(&self, text: &str) -> Vec<&str> {
        let s = self.normalize(text);
        let bounds: Vec<usize> = s
            .char_indices()
            .map(|(i, _)| i)
            .chain(std::iter::once(s.len()))
            .collect();
        let n = bounds.len() - 1;
        // Best score up to each character boundary, with the piece ending
        // there (`None`: an unknown character) and where it starts.
        let mut best: Vec<(f32, usize, Option<usize>)> = vec![(f32::NEG_INFINITY, 0, None); n + 1];
        best[0].0 = 0.0;
        for i in 0..n {
            let base = best[i].0;
            if base == f32::NEG_INFINITY {
                continue;
            }
            let mut single = false;
            for l in 1..=self.max_chars.min(n - i) {
                let Some(&id) = self.index.get(&s[bounds[i]..bounds[i + l]]) else {
                    continue;
                };
                let p = &self.pieces[id];
                let score = match p.kind {
                    Kind::Normal => p.score,
                    Kind::UserDefined => l as f32 * self.max_score - 0.1,
                    _ => continue,
                };
                single |= l == 1;
                if base + score > best[i + l].0 {
                    best[i + l] = (base + score, i, Some(id));
                }
            }
            if !single && base + self.unk_score > best[i + 1].0 {
                best[i + 1] = (base + self.unk_score, i, None);
            }
        }
        let mut ids = Vec::new();
        let mut end = n;
        while end > 0 {
            let (_, start, id) = best[end];
            // Consecutive unknown characters become one unknown piece.
            let id = id.unwrap_or(self.unk);
            if !(id == self.unk && ids.last() == Some(&self.unk)) {
                ids.push(id);
            }
            end = start;
        }
        ids.reverse();
        ids.into_iter()
            .map(|i| self.pieces[i].text.as_str())
            .collect()
    }

    /// Joins pieces into text: control and unknown pieces are left out,
    /// byte pieces (`<0x41>`) become their bytes, U+2581 becomes a space
    /// and the leading space is removed.
    pub fn decode(&self, pieces: &[&str]) -> String {
        let mut bytes = Vec::new();
        for p in pieces {
            match self.index.get(*p).map(|&i| self.pieces[i].kind) {
                Some(Kind::Control | Kind::Unknown) => {}
                Some(Kind::Byte) => {
                    let hex = p.strip_prefix("<0x").and_then(|h| h.strip_suffix('>'));
                    if let Some(b) = hex.and_then(|h| u8::from_str_radix(h, 16).ok()) {
                        bytes.push(b);
                    }
                }
                _ => bytes.extend_from_slice(p.as_bytes()),
            }
        }
        let s = String::from_utf8_lossy(&bytes).replace(SPACE, " ");
        match s.strip_prefix(' ') {
            Some(rest) if self.add_dummy_prefix => rest.to_string(),
            _ => s,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(text: &str, score: f32, kind: Kind) -> Piece {
        Piece {
            text: text.to_string(),
            score,
            kind,
        }
    }

    fn model() -> SentencePiece {
        SentencePiece::new(
            vec![
                p("<unk>", 0.0, Kind::Unknown),
                p("<s>", 0.0, Kind::Control),
                p("▁", -2.0, Kind::Normal),
                p("▁he", -3.0, Kind::Normal),
                p("▁hell", -4.0, Kind::Normal),
                p("llo", -3.0, Kind::Normal),
                p("o", -2.0, Kind::Normal),
                p("▁hello", -6.5, Kind::Normal),
                p("h", -5.0, Kind::Normal),
                p("e", -5.0, Kind::Normal),
                p("l", -5.0, Kind::Normal),
                p("▁w", -3.0, Kind::Normal),
                p("orld", -3.0, Kind::Normal),
                p("<0x41>", 0.0, Kind::Byte),
            ],
            true,
            true,
        )
        .unwrap()
    }

    #[test]
    fn normalizes_spaces() {
        let m = model();
        assert_eq!(m.normalize("  hello \t world\n"), "▁hello▁world");
        assert_eq!(m.normalize("a\u{7}b"), "▁ab");
        assert_eq!(m.normalize(""), "");
    }

    #[test]
    fn finds_the_best_segmentation() {
        let m = model();
        // ▁he + llo (-6) beats ▁hello (-6.5); ▁hell + o ties at -6, and
        // the segmentation found first stays.
        assert_eq!(m.encode("hello world"), vec!["▁he", "llo", "▁w", "orld"]);
        // A run of unknown characters becomes one unknown piece.
        assert_eq!(m.encode("hexyz o"), vec!["▁he", "<unk>", "▁", "o"]);
        assert_eq!(m.encode(""), Vec::<&str>::new());
    }

    #[test]
    fn matches_only_normal_pieces() {
        // The control piece "<s>" is not matched in text.
        assert_eq!(model().encode("<s>"), vec!["▁", "<unk>"]);
    }

    #[test]
    fn decodes_pieces() {
        let m = model();
        assert_eq!(
            m.decode(&["▁he", "llo", "<s>", "▁w", "orld", "<0x41>", "<unk>"]),
            "hello worldA"
        );
    }

    /// A protobuf `ModelProto` with the given pieces.
    fn proto(pieces: &[(&str, f32, u8)]) -> Vec<u8> {
        let mut b = Vec::new();
        for (text, score, kind) in pieces {
            let mut m = vec![0x0a, text.len() as u8];
            m.extend_from_slice(text.as_bytes());
            m.push(0x15);
            m.extend_from_slice(&score.to_le_bytes());
            m.extend_from_slice(&[0x18, *kind]);
            b.push(0x0a);
            b.push(m.len() as u8);
            b.extend_from_slice(&m);
        }
        // TrainerSpec { model_type: UNIGRAM }, NormalizerSpec { name, add_dummy_prefix: true }.
        b.extend_from_slice(&[0x12, 2, 0x18, 1]);
        b.extend_from_slice(&[0x1a, 8, 0x0a, 4, b'n', b'f', b'k', b'c', 0x18, 1]);
        b
    }

    #[test]
    fn reads_a_model_file() {
        let b = proto(&[("<unk>", 0.0, 2), ("▁a", -1.0, 1), ("b", -2.0, 1)]);
        let m = SentencePiece::from_bytes(&b).unwrap();
        assert_eq!(m.pieces().len(), 3);
        assert_eq!(m.pieces()[1].text, "▁a");
        assert_eq!(m.pieces()[2].score, -2.0);
        assert_eq!(m.encode("ab"), vec!["▁a", "b"]);
        // No unknown piece; a BPE model; truncated.
        assert!(SentencePiece::from_bytes(&proto(&[("▁a", -1.0, 1)])).is_err());
        let mut bpe = proto(&[("<unk>", 0.0, 2)]);
        bpe.extend_from_slice(&[0x12, 2, 0x18, 2]);
        assert!(SentencePiece::from_bytes(&bpe).is_err());
        assert!(SentencePiece::from_bytes(&b[..b.len() - 3]).is_err());
    }
}
