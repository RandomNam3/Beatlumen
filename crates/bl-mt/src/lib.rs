//! Offline translation of song texts with the OPUS-MT models of the
//! University of Helsinki (MarianMT models as published on Hugging Face,
//! e.g. `Helsinki-NLP/opus-mt-en-de`), implemented here without external
//! libraries: a safe reader for PyTorch checkpoints, the SentencePiece
//! tokenizer, the transformer and beam search.
//!
//! A model directory holds `config.json`, `pytorch_model.bin`,
//! `source.spm`, `target.spm` and `vocab.json` (optionally
//! `generation_config.json` and, for models with separate vocabularies,
//! `target_vocab.json`). Each line is translated on its own.
//!
//! ```no_run
//! let t = bl_mt::Translator::load("models/opus-mt-en-de").unwrap();
//! let out = t.translate(&["Who let the dogs out?"]);
//! ```

#![forbid(unsafe_code)]

pub mod generate;
pub mod marian;
pub mod pickle;
pub mod proto;
pub mod spm;
pub mod torch;

pub use generate::Search;
use marian::{Config, Model};
use spm::SentencePiece;
use std::collections::HashMap;
use std::path::Path;
pub use torch::Keep;

/// Largest text file read (config, vocabulary, tokenizer models).
const MAX_FILE: u64 = 64 << 20;

/// A loaded translation model with its tokenizers.
pub struct Translator {
    model: Model,
    source: SentencePiece,
    target: SentencePiece,
    /// Source pieces to token ids.
    vocab: HashMap<String, u32>,
    /// Token ids to target pieces.
    pieces: Vec<Option<String>>,
    unk: u32,
}

fn read(dir: &Path, name: &str) -> Result<Vec<u8>, String> {
    let path = dir.join(name);
    let len = std::fs::metadata(&path)
        .map_err(|e| format!("{name}: {e}"))?
        .len();
    if len > MAX_FILE {
        return Err(format!("{name}: too large"));
    }
    std::fs::read(&path).map_err(|e| format!("{name}: {e}"))
}

fn read_text(dir: &Path, name: &str) -> Result<String, String> {
    String::from_utf8(read(dir, name)?).map_err(|_| format!("{name}: not UTF-8"))
}

/// A vocabulary file: piece -> id (every id below `size`).
fn vocab(text: &str, size: usize) -> Result<HashMap<String, u32>, String> {
    let v = bl_json::parse(text).map_err(|e| format!("vocabulary: {e}"))?;
    let bl_json::Value::Object(map) = v else {
        return Err("vocabulary: not an object".into());
    };
    map.iter()
        .map(|(k, v)| {
            v.as_u64()
                .filter(|&i| i < size as u64)
                .map(|i| (k.to_string(), i as u32))
                .ok_or_else(|| format!("vocabulary: bad id for {k}"))
        })
        .collect()
}

impl Translator {
    /// Loads a model directory, keeping the weight matrices as f16 (half
    /// the memory of f32, practically the same translations).
    pub fn load(dir: impl AsRef<Path>) -> Result<Translator, String> {
        Self::load_with(dir, Keep::F16)
    }

    /// Loads a model directory, keeping the weight matrices as `keep`.
    pub fn load_with(dir: impl AsRef<Path>, keep: Keep) -> Result<Translator, String> {
        let dir = dir.as_ref();
        let generation = match read_text(dir, "generation_config.json") {
            Ok(s) => Some(s),
            Err(_) if !dir.join("generation_config.json").exists() => None,
            Err(e) => return Err(e),
        };
        let config = Config::parse(&read_text(dir, "config.json")?, generation.as_deref())?;
        let source = SentencePiece::from_bytes(&read(dir, "source.spm")?)?;
        let target = SentencePiece::from_bytes(&read(dir, "target.spm")?)?;
        let size = config.vocab_size;
        let vocab = vocab(&read_text(dir, "vocab.json")?, size)?;
        let target_vocab = if dir.join("target_vocab.json").exists() {
            self::vocab(&read_text(dir, "target_vocab.json")?, size)?
        } else {
            vocab.clone()
        };
        let mut pieces = vec![None; size];
        for (p, &i) in &target_vocab {
            pieces[i as usize] = Some(p.clone());
        }
        let unk = *vocab.get("<unk>").ok_or("vocabulary: no <unk>")?;
        let ckpt = torch::Checkpoint::open(dir.join("pytorch_model.bin"))?;
        let model = Model::load(config, &ckpt, keep)?;
        Ok(Translator {
            model,
            source,
            target,
            vocab,
            pieces,
            unk,
        })
    }

    pub fn config(&self) -> &Config {
        &self.model.config
    }

    /// Memory taken by the weights (bytes).
    pub fn weight_bytes(&self) -> usize {
        self.model.weight_bytes()
    }

    /// The token ids of a line: its pieces and the end token. A leading
    /// target language code like `>>fra<<` (multilingual models) is kept
    /// as its own token when the vocabulary has it.
    pub fn encode(&self, line: &str) -> Vec<u32> {
        let mut ids = Vec::new();
        let mut text = line.trim();
        if let Some(rest) = text.strip_prefix(">>") {
            if let Some((code, rest)) = rest.split_once("<<") {
                if let Some(&id) = self.vocab.get(&format!(">>{code}<<")) {
                    ids.push(id);
                    text = rest;
                }
            }
        }
        for p in self.source.encode(text) {
            ids.push(self.vocab.get(p).copied().unwrap_or(self.unk));
        }
        // Room for the end token within the position embeddings.
        ids.truncate(self.model.max_positions() - 1);
        ids.push(self.model.config.eos);
        ids
    }

    /// The text of output tokens (special tokens left out).
    pub fn decode(&self, ids: &[u32]) -> String {
        let c = &self.model.config;
        let pieces: Vec<&str> = ids
            .iter()
            .filter(|&&t| t != c.eos && t != c.pad && t != c.decoder_start && t != self.unk)
            .filter_map(|&t| self.pieces.get(t as usize)?.as_deref())
            .collect();
        self.target.decode(&pieces).trim().to_string()
    }

    /// Translates each line on its own with the model's beam search
    /// (empty lines stay empty).
    pub fn translate(&self, lines: &[&str]) -> Vec<String> {
        let search = Search::Beam(self.model.config.num_beams);
        lines
            .iter()
            .map(|l| self.translate_line(l, search))
            .collect()
    }

    /// Translates one line with the given search.
    pub fn translate_line(&self, line: &str, search: Search) -> String {
        if line.trim().is_empty() {
            return String::new();
        }
        let ids = self.encode(line);
        let enc = self.model.encode(&ids);
        let max_len = (3 * ids.len() + 10).min(self.model.config.max_length);
        let out = generate::generate(&self.model, &enc, search, max_len);
        self.decode(&out)
    }
}
