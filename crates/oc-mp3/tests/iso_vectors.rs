//! Optional ISO/IEC 11172-4 / 13818-4 conformance check. The bitstreams
//! are not redistributed with this crate; point `OC_MP3_VECTORS` at a
//! directory with `<name>.bit` streams and 16-bit `<name>.pcm` reference
//! outputs (e.g. the `vectors` directory of the minimp3 repository).

mod common;

use common::decode_bytes;

#[test]
fn iso_conformance_vectors() {
    let Ok(dir) = std::env::var("OC_MP3_VECTORS") else {
        eprintln!("OC_MP3_VECTORS not set, skipping");
        return;
    };
    // he_mode switches the channel count mid-stream and sin1k0db starts
    // without bit reservoir data (the reference drops those frames).
    let skip = ["l3-he_mode", "l3-sin1k0db", "ILL2_overalloc2"];
    let mut checked = 0;
    let mut entries: Vec<_> = std::fs::read_dir(&dir)
        .unwrap()
        .flatten()
        .map(|e| e.path())
        .collect();
    entries.sort();
    for bit in entries {
        if bit.extension().and_then(|e| e.to_str()) != Some("bit") {
            continue;
        }
        let name = bit.file_stem().unwrap().to_string_lossy().to_string();
        let pcm = bit.with_extension("pcm");
        if skip.contains(&name.as_str()) || !pcm.exists() {
            continue;
        }
        let ours = decode_bytes(&std::fs::read(&bit).unwrap());
        let reference: Vec<f32> = std::fs::read(&pcm)
            .unwrap()
            .chunks_exact(2)
            .map(|c| f32::from(i16::from_le_bytes([c[0], c[1]])) / 32768.0)
            .collect();
        let n = ours.pcm.len().min(reference.len());
        let (mut se, mut max) = (0f64, 0f64);
        for (o, r) in ours.pcm[..n].iter().zip(&reference[..n]) {
            let e = f64::from(o - r).abs();
            se += e * e;
            max = max.max(e);
        }
        let rms = (se / n.max(1) as f64).sqrt();
        println!(
            "{name}: {n} samples, rms {rms:.2e}, max {:.2} LSB",
            max * 32768.0
        );
        // 16-bit rounding alone gives rms 2^-15/sqrt(12) = 8.8e-6.
        assert!(rms < 1.2e-5, "{name}: rms error {rms:.2e}");
        assert!(
            max * 32768.0 <= 1.5,
            "{name}: max error {:.2} LSB",
            max * 32768.0
        );
        checked += 1;
    }
    println!("{checked} conformance streams checked");
}
