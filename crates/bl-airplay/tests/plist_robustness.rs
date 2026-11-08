//! Damaged binary plists from AirPlay peers: parsing ends with a value or an
//! error; what parses writes back and parses the same.

#[path = "../../bl-json/tests/fuzz/mod.rs"]
mod fuzz;

use bl_airplay::ap2::plist::{parse, write, Value};

fn samples() -> Vec<Vec<u8>> {
    let inner = Value::Array(vec![
        Value::Int(0),
        Value::Int(300),
        Value::Int(70_000),
        Value::Int(1 << 40),
        Value::Int(-5),
        Value::Int(i128::MAX),
        Value::Real(1.5),
        Value::Date(700_000_000.0),
        Value::Bool(true),
        Value::Null,
        Value::Uid(7),
    ]);
    let setup = Value::Dict(vec![
        ("streams".into(), Value::Array(vec![inner.clone(), inner])),
        ("deviceID".into(), Value::String("AA:BB:CC:DD:EE:FF".into())),
        ("name".into(), Value::String("Wohnzimmer — Küche 🎵".into())),
        ("ekey".into(), Value::Data((0..72).collect())),
        ("big".into(), Value::Data(vec![0x5a; 300])),
        (
            "timingPeerInfo".into(),
            Value::Dict(vec![(
                "Addresses".into(),
                Value::Array(vec![Value::String("192.168.1.2".into()); 20]),
            )]),
        ),
    ]);
    vec![
        write(&setup),
        write(&Value::Dict(vec![("rate".into(), Value::Real(1.0))])),
        write(&Value::String("x".repeat(40))),
    ]
}

#[test]
fn damaged_plists_end_cleanly() {
    let tokens: &[&[u8]] = &[
        b"bplist00",
        &[0xdf],
        &[0xaf],
        &[0x5f, 0x13],
        &[0x4f, 0x10, 0xff],
        &[0x14],
        &[0x80],
        &[0xff; 8],
        &[0; 8],
        &[0x00, 0x00, 0x00, 0x00, 0x00, 0x08, 0x08, 0x08],
    ];
    fuzz::run("plist", &samples(), tokens, 20_000, |d| {
        if let Ok(v) = parse(d) {
            let again = parse(&write(&v)).expect("written plist parses");
            // NaN is not equal to itself; compare the writing instead.
            assert_eq!(write(&again), write(&v));
        }
    });
}

/// Changes inside the objects only (the trailer and offsets stay), so that
/// the damage reaches the object reader instead of the trailer check.
#[test]
fn damaged_objects_end_cleanly() {
    let samples = samples();
    fuzz::run("plist-objects", &samples, &[], 20_000, |d| {
        // The fuzzer's input only seeds the change.
        let seed = d.iter().fold(0xcbf2_9ce4_8422_2325u64, |h, &b| {
            (h ^ b as u64).wrapping_mul(0x100_0000_01b3)
        });
        let mut rng = fuzz::Rng(seed | 1);
        let mut m = samples[rng.below(samples.len())].clone();
        let objects = 8..m.len() - 32;
        for _ in 0..1 + rng.below(8) {
            let i = if rng.below(8) == 0 {
                rng.below(m.len())
            } else {
                objects.start + rng.below(objects.len())
            };
            m[i] = rng.next() as u8;
        }
        if let Ok(v) = parse(&m) {
            let again = parse(&write(&v)).expect("written plist parses");
            assert_eq!(write(&again), write(&v));
        }
    });
}
