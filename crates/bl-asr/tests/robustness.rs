//! Speech models in the ggml format from untrusted places (a server hands
//! them to the apps): damaged and lying files fail to load cleanly, without
//! allocating what their headers claim.

#[path = "../../bl-json/tests/fuzz/mod.rs"]
mod fuzz;

use bl_asr::model::Model;
use std::io::{Cursor, ErrorKind};

fn put(b: &mut Vec<u8>, v: i32) {
    b.extend_from_slice(&v.to_le_bytes());
}

/// A tensor record: dims, name length, type, sizes, name, data.
fn tensor(b: &mut Vec<u8>, name: &str, ne: &[i32], ttype: i32, data: &[u8]) {
    put(b, ne.len() as i32);
    put(b, name.len() as i32);
    put(b, ttype);
    for &n in ne {
        put(b, n);
    }
    b.extend_from_slice(name.as_bytes());
    b.extend_from_slice(data);
}

fn f32s(n: usize) -> Vec<u8> {
    (0..n)
        .flat_map(|i| (i as f32 * 0.25).to_le_bytes())
        .collect()
}

fn f16s(n: usize) -> Vec<u8> {
    (0..n).flat_map(|_| 0x3c00u16.to_le_bytes()).collect()
}

/// A tiny but well-formed model: 4 wide, one layer each, 2 mel bands.
fn tiny_model() -> Vec<u8> {
    let mut b = 0x6767_6d6cu32.to_le_bytes().to_vec();
    // vocab, audio ctx/state/heads/layers, text ctx/state/heads/layers, mels
    for v in [4, 2, 4, 1, 1, 2, 4, 1, 1, 2] {
        put(&mut b, v);
    }
    put(&mut b, 1); // ftype
    put(&mut b, 2); // mel bands
    put(&mut b, 201); // bins
    b.extend(f32s(2 * 201));
    put(&mut b, 4);
    for w in ["a", "bc", "", "def"] {
        put(&mut b, w.len() as i32);
        b.extend_from_slice(w.as_bytes());
    }
    tensor(&mut b, "encoder.conv1.weight", &[3, 2, 4], 0, &f32s(24));
    tensor(&mut b, "encoder.conv1.bias", &[1, 4], 0, &f32s(4));
    tensor(&mut b, "encoder.positional_embedding", &[4, 2], 1, &f16s(8));
    tensor(
        &mut b,
        "encoder.blocks.0.attn.query.weight",
        &[4, 4],
        1,
        &f16s(16),
    );
    tensor(
        &mut b,
        "encoder.blocks.0.mlp.0.weight",
        &[4, 16],
        1,
        &f16s(64),
    );
    tensor(&mut b, "encoder.blocks.0.mlp.0.bias", &[16], 0, &f32s(16));
    tensor(
        &mut b,
        "encoder.blocks.0.mlp.2.weight",
        &[16, 4],
        0,
        &f32s(64),
    );
    tensor(
        &mut b,
        "decoder.token_embedding.weight",
        &[4, 4],
        1,
        &f16s(16),
    );
    tensor(
        &mut b,
        "decoder.blocks.0.cross_attn.key.weight",
        &[4, 4],
        0,
        &f32s(16),
    );
    // Not used by the network: kept as stored (two q8_0 rows).
    let mut q = Vec::new();
    for _ in 0..2 {
        q.extend_from_slice(&0x3c00u16.to_le_bytes());
        q.extend((0..32).map(|i| i as u8));
    }
    tensor(&mut b, "extra.conv", &[32, 2], 8, &q);
    b
}

fn load(data: &[u8]) -> std::io::Result<Model> {
    Model::load(&mut Cursor::new(data))
}

#[test]
fn the_tiny_model_loads() {
    let m = load(&tiny_model()).unwrap();
    assert_eq!(m.params.n_audio_state, 4);
    assert_eq!(m.vocab.len(), 4);
    assert_eq!(m.mel_filters.len(), 2 * 201);
    // Vectors, convolutions and position embeddings become f32.
    assert!(m
        .get("encoder.positional_embedding")
        .unwrap()
        .as_f32()
        .is_some());
    assert!(m
        .get("encoder.blocks.0.attn.query.weight")
        .unwrap()
        .as_f32()
        .is_none());
    assert!(m.get("extra.conv").unwrap().as_f32().is_none());
}

#[test]
fn claimed_sizes_are_not_believed() {
    let header_len = tiny_model().len();
    // A 4 GiB tensor with ten bytes behind it.
    let mut b = tiny_model();
    tensor(&mut b, "huge", &[1 << 20, 1 << 10], 0, &[0; 10]);
    assert_eq!(load(&b).err().unwrap().kind(), ErrorKind::UnexpectedEof);
    // The same as f16 and quantised.
    for ttype in [1, 8] {
        let mut b = tiny_model();
        tensor(&mut b, "huge", &[1 << 20, 1 << 10], ttype, &[0; 10]);
        assert_eq!(load(&b).err().unwrap().kind(), ErrorKind::UnexpectedEof);
    }
    // Every cut of the file fails cleanly.
    let full = tiny_model();
    for n in (0..full.len()).step_by(7) {
        assert!(load(&full[..n]).is_err(), "cut at {n} of {header_len}");
    }
}

#[test]
fn shapes_must_match_the_hyperparameters() {
    let with = |name: &str, ne: &[i32]| {
        let mut b = tiny_model();
        let n: i32 = ne.iter().product();
        tensor(&mut b, name, ne, 0, &f32s(n as usize));
        load(&b)
    };
    assert!(with("encoder.blocks.0.attn.key.weight", &[4, 4]).is_ok());
    assert!(with("encoder.blocks.0.attn.key.weight", &[4, 5]).is_err());
    assert!(with("encoder.conv2.weight", &[3, 4, 5]).is_err());
    assert!(with("encoder.positional_embedding", &[4, 3]).is_err());
    assert!(with("decoder.blocks.0.cross_attn.value.weight", &[3, 4]).is_err());
    assert!(with("encoder.blocks.0.mlp.0.bias", &[15]).is_err());
    // Layers the model does not have are not used and not checked.
    assert!(with("encoder.blocks.7.attn.key.weight", &[4, 5]).is_ok());
    // The mel filters must fit the 400-point transform.
    let mut b = tiny_model();
    let at = 4 + 11 * 4 + 4;
    b[at..at + 4].copy_from_slice(&400i32.to_le_bytes());
    assert!(load(&b).is_err());
}

#[test]
fn damaged_models_fail_cleanly() {
    let samples = vec![tiny_model()];
    let tokens: &[&[u8]] = &[
        &[0xff, 0xff, 0xff, 0x7f],
        &[0, 0, 0, 0x40],
        &[0, 0, 0x10, 0],
        b"encoder.blocks.0.mlp.0.weight",
        b"decoder.positional_embedding",
    ];
    fuzz::run("ggml", &samples, tokens, 3000, |d| {
        if let Ok(m) = load(d) {
            let _ = m.weight_bytes();
            let _ = m.multilingual();
            for t in m.tensors.values() {
                for r in 0..t.rows.min(4) {
                    let _ = t.row(r);
                }
            }
        }
    });
}

/// A real model loads as before (opt-in: `BL_SPEECH_MODEL=ggml-x.bin`).
#[test]
fn a_real_model_still_loads() {
    let Ok(path) = std::env::var("BL_SPEECH_MODEL") else {
        return;
    };
    let f = std::fs::File::open(&path).unwrap();
    let m = Model::load(&mut std::io::BufReader::new(f)).unwrap();
    println!(
        "{path}: {:?}, {} tensors, {} MB",
        m.params,
        m.tensors.len(),
        m.weight_bytes() >> 20
    );
    assert!(m.get("encoder.conv1.weight").is_ok());
    // Every vector is f32, as the network reads them.
    for (name, t) in &m.tensors {
        if t.rows == 1 || t.cols == 1 {
            assert!(t.as_f32().is_some(), "{name} is a vector but not f32");
        }
    }
}
