//! Damaged safetensors files and lyrics heads (`.head`, from a server):
//! opening and loading end with weights or an error, without allocating
//! what their headers claim.

#[path = "../../bl-json/tests/fuzz/mod.rs"]
mod fuzz;

use bl_align::head::Head;
use bl_align::weights::SafeTensors;
use bl_align::Keep;
use std::path::PathBuf;

/// A safetensors file: header length, JSON header, data.
fn file(header: &str, data: &[u8]) -> Vec<u8> {
    let mut out = (header.len() as u64).to_le_bytes().to_vec();
    out.extend_from_slice(header.as_bytes());
    out.extend_from_slice(data);
    out
}

/// A tiny head: 4 wide encoder frames, 3 hidden, the letters and the
/// layer for any language.
fn tiny_head() -> Vec<u8> {
    let (dim, hidden, letters) = (4usize, 3usize, 29usize);
    let tensors: Vec<(&str, Vec<usize>)> = vec![
        ("norm.weight", vec![dim]),
        ("norm.bias", vec![dim]),
        ("a.weight", vec![hidden, dim, 5]),
        ("a.bias", vec![hidden]),
        ("b.weight", vec![hidden, hidden, 5]),
        ("b.bias", vec![hidden]),
        ("out.any.weight", vec![letters, hidden]),
        ("out.any.bias", vec![letters]),
        ("out.de.weight", vec![letters, hidden]),
        ("out.de.bias", vec![letters]),
    ];
    let mut header = String::from("{\"__metadata__\":{\"format\":\"pt\"}");
    let mut data = Vec::new();
    for (name, shape) in tensors {
        let n: usize = shape.iter().product();
        let start = data.len();
        for i in 0..n {
            data.extend_from_slice(&((i % 7) as f32 * 0.1).to_le_bytes());
        }
        header += &format!(
            ",\"{name}\":{{\"dtype\":\"F32\",\"shape\":{shape:?},\"data_offsets\":[{start},{}]}}",
            data.len()
        );
    }
    header.push('}');
    file(&header, &data)
}

fn scratch(name: &str) -> PathBuf {
    // One folder per test: tests run at the same time.
    let dir = std::env::temp_dir().join(format!("bl-align-{name}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir.join(name)
}

#[test]
fn the_tiny_head_loads_and_lying_shapes_do_not() {
    let path = scratch("ok.head");
    std::fs::write(&path, tiny_head()).unwrap();
    assert!(Head::load(&path).is_ok());
    // Shapes that do not fit each other, or are far too wide.
    for (from, to) in [
        ("[3,4,5]", "[3,4,6]"),
        (
            "\"norm.weight\":{\"dtype\":\"F32\",\"shape\":[4]",
            "\"norm.weight\":{\"dtype\":\"F32\",\"shape\":[]",
        ),
        ("\"shape\":[29,3]", "\"shape\":[28,3]"),
    ] {
        let text = String::from_utf8_lossy(&tiny_head()).replacen(from, to, 1);
        std::fs::write(&path, text.as_bytes()).unwrap();
        assert!(Head::load(&path).is_err(), "{to}");
    }
    // A header that claims a huge tensor with a little data behind it.
    let huge = file(
        r#"{"w":{"dtype":"F32","shape":[1073741824,4],"data_offsets":[0,17179869184]}}"#,
        &[0; 64],
    );
    std::fs::write(&path, huge).unwrap();
    assert!(SafeTensors::open(&path).is_err());
    let wide = file(
        r#"{"norm.weight":{"dtype":"F32","shape":[1000000],"data_offsets":[0,4000000]},
            "b.bias":{"dtype":"F32","shape":[1000000],"data_offsets":[0,4000000]}}"#,
        &vec![0; 4_000_000],
    );
    std::fs::write(&path, wide).unwrap();
    assert!(Head::load(&path).is_err());
    let _ = std::fs::remove_dir_all(path.parent().unwrap());
}

#[test]
fn damaged_heads_end_cleanly() {
    let path = scratch("fuzz.head");
    let tokens: &[&[u8]] = &[
        b"{",
        b"}",
        b"[",
        b"]",
        b",",
        b"\"shape\":",
        b"\"data_offsets\":",
        b"\"F16\"",
        b"\"BF16\"",
        b"18446744073709551615",
        b"4294967296",
        b"0",
        &[0xff; 8],
        &[0; 8],
    ];
    fuzz::run("safetensors", &[tiny_head()], tokens, 5000, |d| {
        std::fs::write(&path, d).unwrap();
        if let Ok(f) = SafeTensors::open(&path) {
            let names: Vec<String> = f.names().map(str::to_string).collect();
            for n in &names {
                let _ = f.shape(n);
                for keep in [Keep::F32, Keep::F16] {
                    let _ = f.load(&[(n.as_str(), keep)]);
                }
            }
        }
        let _ = Head::load(&path);
    });
    let _ = std::fs::remove_dir_all(path.parent().unwrap());
}

/// A real head loads (opt-in: `BL_SPEECH_HEAD=ggml-x.head`).
#[test]
fn a_real_head_still_loads() {
    let Ok(path) = std::env::var("BL_SPEECH_HEAD") else {
        return;
    };
    Head::load(&path).unwrap();
}
