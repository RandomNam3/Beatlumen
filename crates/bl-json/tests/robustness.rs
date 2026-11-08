//! Damaged and hostile JSON: parsing ends with a value or an error, in
//! time linear in the input; what parses writes and parses back the same.

mod fuzz;

use bl_json::{parse_bytes, Value};

fn samples() -> Vec<Vec<u8>> {
    let mut out: Vec<Vec<u8>> = [
        r#"{"a":1,"b":[true,false,null],"c":"x\"y\\z\nä😀ä😀","d":-1.5e3,"e":{}}"#,
        r#"[{"id":"t1","title":"Song","artist":"Someone","duration":215.5,"tags":["x","y"]},{"id":"t2"}]"#,
        r#"{"format":"beatlumen-karaoke","version":1,"lines":[{"v":1,"notes":[{"t":1.5,"d":0.25,"p":62,"s":"la "}]}]}"#,
        r#"{"__metadata__":{"format":"pt"},"w":{"dtype":"F32","shape":[2,3],"data_offsets":[0,24]}}"#,
        "  [ 1 , -0 , 0.5e-3 , 1E+2 , \"\" , [ ] , { } ]  ",
    ]
    .iter()
    .map(|s| s.as_bytes().to_vec())
    .collect();
    // The effect presets that ship with the app.
    let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/../../presets");
    if let Ok(rd) = std::fs::read_dir(dir) {
        for e in rd.flatten().take(10) {
            if let Ok(d) = std::fs::read(e.path()) {
                out.push(d);
            }
        }
    }
    out
}

/// Every number in `v` is finite (those write and read back exactly).
fn finite(v: &Value) -> bool {
    match v {
        Value::Number(n) => n.is_finite(),
        Value::Array(a) => a.iter().all(finite),
        Value::Object(m) => m.iter().all(|(_, v)| finite(v)),
        _ => true,
    }
}

#[test]
fn damaged_documents_end_cleanly() {
    let tokens: &[&[u8]] = &[
        b"{",
        b"}",
        b"[",
        b"]",
        b"\"",
        b"\\",
        b":",
        b",",
        b"\"k\":",
        b"\"k\":1,",
        b"1e999",
        b"-",
        b"\\u",
        b"\\ud800",
        b"\\udc00",
        b"null",
        b"true",
        b"\xef\xbb\xbf",
        b"\xc3",
        b"\x00",
    ];
    fuzz::run("json", &samples(), tokens, 20_000, |d| {
        if let Ok(v) = parse_bytes(d) {
            let text = v.to_json();
            let again = bl_json::parse(&text).expect("written JSON parses");
            if finite(&v) {
                assert_eq!(again, v, "round trip");
            }
            let _ = v.to_json_pretty();
        }
    });
}

#[test]
fn hostile_shapes_are_cheap() {
    let deep = "[".repeat(100_000) + &"]".repeat(100_000);
    assert!(bl_json::parse(&deep).is_err());
    let deep_obj = "{\"a\":".repeat(100_000);
    assert!(bl_json::parse(&deep_obj).is_err());
    // Many repeated keys: one entry, the last value.
    let mut text = String::from("{");
    for i in 0..300_000 {
        text += &format!("\"same\":{i},");
    }
    text += "\"same\":-1}";
    let v = bl_json::parse(&text).unwrap();
    assert_eq!(v.as_object().unwrap().len(), 1);
    assert_eq!(v.get("same").as_i64(), Some(-1));
    // Long strings and numbers.
    let long = format!(
        "[\"{}\",{}]",
        "\\u00e4".repeat(200_000),
        "9".repeat(100_000)
    );
    let v = bl_json::parse(&long).unwrap();
    assert_eq!(
        v.as_array().unwrap()[0].as_str().unwrap().chars().count(),
        200_000
    );
}
