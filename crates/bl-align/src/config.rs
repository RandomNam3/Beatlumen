//! The model description: `config.json` (the network) and
//! `preprocessor_config.json` (how the input is prepared), as written by
//! Hugging Face `transformers` for `Wav2Vec2ForCTC`.

use bl_json::Value;

/// Largest supported sizes (sanity limits for `config.json`).
const MAX_DIM: usize = 8192;
const MAX_LAYERS: usize = 64;
const MAX_CONV_LAYERS: usize = 16;
const MAX_KERNEL: usize = 1024;
const MAX_VOCAB: usize = 1 << 16;

/// The sample rate the models take.
pub const SAMPLE_RATE: u32 = 16_000;

/// How the feature encoder's convolutions are normalised.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FeatNorm {
    /// Group norm with one group per channel, on the first layer only
    /// (the base models).
    Group,
    /// Layer norm over the channels after every layer (the large models).
    Layer,
}

/// One convolution of the feature encoder.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Conv {
    pub dim: usize,
    pub kernel: usize,
    pub stride: usize,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Config {
    pub conv: Vec<Conv>,
    pub conv_bias: bool,
    pub feat_norm: FeatNorm,
    /// Pre-norm transformer layers with the final norm after them (the
    /// large models); otherwise post-norm layers with the norm before.
    pub stable_layer_norm: bool,
    pub hidden: usize,
    pub layers: usize,
    pub heads: usize,
    pub ffn: usize,
    pub pos_kernel: usize,
    pub pos_groups: usize,
    pub vocab_size: usize,
    /// The CTC blank (the pad token).
    pub blank: usize,
    /// Scale the input to zero mean and unit variance.
    pub normalize: bool,
}

/// A field of a JSON object (`None` if missing or null).
fn field<'a>(v: &'a Value, key: &str) -> Option<&'a Value> {
    Some(v.get(key)).filter(|v| !v.is_null())
}

fn usize_of(v: &Value, key: &str, max: usize) -> Result<usize, String> {
    field(v, key)
        .and_then(Value::as_u64)
        .and_then(|n| usize::try_from(n).ok())
        .filter(|&n| n >= 1 && n <= max)
        .ok_or_else(|| format!("config: bad or missing {key}"))
}

fn list_of(v: &Value, key: &str, max: usize) -> Result<Vec<usize>, String> {
    let a = field(v, key)
        .and_then(Value::as_array)
        .filter(|a| !a.is_empty() && a.len() <= MAX_CONV_LAYERS)
        .ok_or_else(|| format!("config: bad or missing {key}"))?;
    a.iter()
        .map(|n| {
            n.as_u64()
                .and_then(|n| usize::try_from(n).ok())
                .filter(|&n| n >= 1 && n <= max)
                .ok_or_else(|| format!("config: bad value in {key}"))
        })
        .collect()
}

/// Accepts only GELU (the exact, erf-based one is what these models use).
fn check_gelu(v: &Value, key: &str) -> Result<(), String> {
    match field(v, key).and_then(Value::as_str) {
        Some("gelu") | None => Ok(()),
        Some(other) => Err(format!("config: unsupported {key} {other}")),
    }
}

impl Config {
    /// Reads `config.json` and the preprocessor's settings (`None` if the
    /// model has no such file: the defaults then apply).
    pub fn parse(config: &str, preprocessor: Option<&str>) -> Result<Config, String> {
        let v = bl_json::parse(config).map_err(|e| format!("config.json: {e}"))?;
        let p = match preprocessor {
            Some(s) => bl_json::parse(s).map_err(|e| format!("preprocessor config: {e}"))?,
            None => Value::object(),
        };
        if let Some(t) = field(&v, "model_type").and_then(Value::as_str) {
            if t != "wav2vec2" {
                return Err(format!("config: unsupported model_type {t}"));
            }
        }
        check_gelu(&v, "hidden_act")?;
        check_gelu(&v, "feat_extract_activation")?;
        // The layer norms of `bl_asr::ops` use an epsilon of 1e-5.
        if let Some(eps) = field(&v, "layer_norm_eps").and_then(Value::as_f64) {
            if (eps - 1e-5).abs() > 1e-9 {
                return Err("config: unsupported layer_norm_eps".into());
            }
        }
        for flag in ["add_adapter", "use_weighted_layer_sum"] {
            if field(&v, flag).and_then(Value::as_bool) == Some(true) {
                return Err(format!("config: {flag} is not supported"));
            }
        }
        let dims = list_of(&v, "conv_dim", MAX_DIM)?;
        let kernels = list_of(&v, "conv_kernel", MAX_KERNEL)?;
        let strides = list_of(&v, "conv_stride", MAX_KERNEL)?;
        if dims.len() != kernels.len() || dims.len() != strides.len() {
            return Err("config: conv_dim, conv_kernel and conv_stride differ in length".into());
        }
        let conv = dims
            .iter()
            .zip(&kernels)
            .zip(&strides)
            .map(|((&dim, &kernel), &stride)| Conv {
                dim,
                kernel,
                stride,
            })
            .collect();
        let feat_norm = match field(&v, "feat_extract_norm").and_then(Value::as_str) {
            Some("group") | None => FeatNorm::Group,
            Some("layer") => FeatNorm::Layer,
            Some(other) => return Err(format!("config: unsupported feat_extract_norm {other}")),
        };
        let vocab_size = usize_of(&v, "vocab_size", MAX_VOCAB)?;
        let blank = field(&v, "pad_token_id")
            .and_then(Value::as_u64)
            .map_or(0, |n| n as usize);
        if blank >= vocab_size {
            return Err("config: pad_token_id outside the vocabulary".into());
        }
        if let Some(rate) = field(&p, "sampling_rate").and_then(Value::as_u64) {
            if rate != SAMPLE_RATE as u64 {
                return Err(format!(
                    "preprocessor config: unsupported sampling_rate {rate}"
                ));
            }
        }
        let c = Config {
            conv,
            conv_bias: field(&v, "conv_bias").and_then(Value::as_bool) == Some(true),
            feat_norm,
            stable_layer_norm: field(&v, "do_stable_layer_norm").and_then(Value::as_bool)
                == Some(true),
            hidden: usize_of(&v, "hidden_size", MAX_DIM)?,
            layers: usize_of(&v, "num_hidden_layers", MAX_LAYERS)?,
            heads: usize_of(&v, "num_attention_heads", MAX_DIM)?,
            ffn: usize_of(&v, "intermediate_size", 8 * MAX_DIM)?,
            pos_kernel: usize_of(&v, "num_conv_pos_embeddings", MAX_KERNEL)?,
            pos_groups: usize_of(&v, "num_conv_pos_embedding_groups", MAX_DIM)?,
            vocab_size,
            blank,
            normalize: field(&p, "do_normalize").and_then(Value::as_bool) != Some(false),
        };
        // (`is_multiple_of` needs Rust 1.87.)
        #[allow(clippy::manual_is_multiple_of)]
        let split = c.hidden % c.heads == 0 && c.hidden % c.pos_groups == 0;
        if !split {
            return Err("config: hidden_size is not a multiple of the heads or groups".into());
        }
        Ok(c)
    }

    /// Input samples per output frame (the product of the strides: 320,
    /// i.e. 20 ms).
    pub fn hop(&self) -> usize {
        self.conv.iter().map(|c| c.stride).product()
    }

    /// Input samples one output frame sees (400 for the usual layers).
    pub fn receptive_field(&self) -> usize {
        let mut field = 1;
        let mut step = 1;
        for c in &self.conv {
            field += (c.kernel - 1) * step;
            step *= c.stride;
        }
        field
    }

    /// Output frames for `samples` input samples (0 if too short).
    pub fn frames(&self, samples: usize) -> usize {
        let mut n = samples;
        for c in &self.conv {
            if n < c.kernel {
                return 0;
            }
            n = (n - c.kernel) / c.stride + 1;
        }
        n
    }

    /// Seconds per output frame.
    pub fn frame_seconds(&self) -> f64 {
        self.hop() as f64 / SAMPLE_RATE as f64
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const BASE: &str = r#"{
        "conv_bias": false, "conv_dim": [512, 512, 512, 512, 512, 512, 512],
        "conv_kernel": [10, 3, 3, 3, 3, 2, 2], "conv_stride": [5, 2, 2, 2, 2, 2, 2],
        "do_stable_layer_norm": false, "feat_extract_activation": "gelu",
        "feat_extract_norm": "group", "hidden_act": "gelu", "hidden_size": 768,
        "intermediate_size": 3072, "layer_norm_eps": 1e-05, "model_type": "wav2vec2",
        "num_attention_heads": 12, "num_conv_pos_embedding_groups": 16,
        "num_conv_pos_embeddings": 128, "num_feat_extract_layers": 7,
        "num_hidden_layers": 12, "pad_token_id": 0, "vocab_size": 32
    }"#;

    #[test]
    fn parses_the_base_config() {
        let c = Config::parse(
            BASE,
            Some(r#"{"do_normalize": true, "sampling_rate": 16000}"#),
        )
        .unwrap();
        assert_eq!(c.conv.len(), 7);
        assert_eq!(
            c.conv[0],
            Conv {
                dim: 512,
                kernel: 10,
                stride: 5
            }
        );
        assert_eq!(c.feat_norm, FeatNorm::Group);
        assert!(!c.stable_layer_norm && !c.conv_bias && c.normalize);
        assert_eq!((c.hidden, c.layers, c.heads, c.ffn), (768, 12, 12, 3072));
        assert_eq!((c.pos_kernel, c.pos_groups), (128, 16));
        assert_eq!((c.vocab_size, c.blank), (32, 0));
        assert_eq!(c.hop(), 320);
        assert_eq!(c.receptive_field(), 400);
        assert_eq!(c.frames(16_000), 49);
        assert_eq!(c.frames(399), 0);
        assert_eq!(c.frames(400), 1);
        assert_eq!(c.frames(720), 2);
        assert!((c.frame_seconds() - 0.02).abs() < 1e-12);
    }

    #[test]
    fn reads_the_large_layout_and_rejects_others() {
        let large = BASE.replace("\"group\"", "\"layer\"").replace(
            "\"do_stable_layer_norm\": false",
            "\"do_stable_layer_norm\": true",
        );
        let c = Config::parse(&large, None).unwrap();
        assert_eq!(c.feat_norm, FeatNorm::Layer);
        assert!(c.stable_layer_norm && c.normalize);
        let bad = [
            BASE.replace("\"wav2vec2\"", "\"hubert\""),
            BASE.replace("\"hidden_act\": \"gelu\"", "\"hidden_act\": \"relu\""),
            BASE.replace("[5, 2, 2, 2, 2, 2, 2]", "[5, 2]"),
            BASE.replace("\"num_attention_heads\": 12", "\"num_attention_heads\": 7"),
            BASE.replace("\"pad_token_id\": 0", "\"pad_token_id\": 32"),
            BASE.replace("\"vocab_size\": 32", "\"vocab_size\": 0"),
        ];
        for b in &bad {
            assert!(Config::parse(b, None).is_err(), "{b}");
        }
        assert!(Config::parse(BASE, Some(r#"{"sampling_rate": 8000}"#)).is_err());
        let c = Config::parse(BASE, Some(r#"{"do_normalize": false}"#)).unwrap();
        assert!(!c.normalize);
    }
}
