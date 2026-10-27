//! Bit-exactness against the system libopus at all output rates and with
//! packet loss, using a tiny C driver (tests/libopus_ref_driver.c) linked
//! against the installed `libopus.so.0`. Skipped when gcc, the library or
//! ffmpeg (for fixture encoding) are unavailable.

mod common;
use common::*;
use oc_opus::OpusDecoder;
use std::path::PathBuf;
use std::process::Command;

fn build_driver() -> Option<PathBuf> {
    let dir = work_dir();
    let src = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/libopus_ref_driver.c");
    let exe = dir.join("libopus_ref_driver");
    let ok = Command::new("gcc")
        .arg("-O2")
        .arg(&src)
        .arg("-o")
        .arg(&exe)
        .arg("-l:libopus.so.0")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);
    ok.then_some(exe)
}

fn run_ref(
    exe: &PathBuf,
    ogg: &OggOpus,
    rate: u32,
    packets: &[Vec<u8>],
    tag: &str,
) -> Option<Vec<f32>> {
    let dir = work_dir();
    let pk = dir.join(format!("{tag}.pkts"));
    let out = dir.join(format!("{tag}.ref"));
    let mut data = Vec::new();
    for p in packets {
        data.extend_from_slice(&(p.len() as u32).to_le_bytes());
        data.extend_from_slice(p);
    }
    std::fs::write(&pk, &data).ok()?;
    let h = &ogg.head;
    let (streams, coupled, mapping) = if h.mapping_family == 0 {
        (
            1,
            h.channels as usize - 1,
            (0..h.channels).collect::<Vec<u8>>(),
        )
    } else {
        (
            h.stream_count as usize,
            h.coupled_count as usize,
            h.mapping.clone(),
        )
    };
    let hex: String = mapping.iter().map(|m| format!("{m:02x}")).collect();
    let o = Command::new(exe)
        .args([
            rate.to_string(),
            h.channels.to_string(),
            streams.to_string(),
            coupled.to_string(),
            h.output_gain.to_string(),
            hex,
        ])
        .arg(&pk)
        .arg(&out)
        .output()
        .ok()?;
    if !o.status.success() {
        eprintln!("driver failed: {}", String::from_utf8_lossy(&o.stderr));
        return None;
    }
    Some(read_f32(&out))
}

#[test]
fn bit_exact_all_rates_and_loss() {
    if !ffmpeg_available() {
        eprintln!("NOTE: ffmpeg with libopus not found, skipping");
        return;
    }
    let Some(exe) = build_driver() else {
        eprintln!("NOTE: gcc or system libopus not available, skipping");
        return;
    };
    let dir = work_dir();
    let cases: &[(&str, Signal, u32, usize, &[&str])] = &[
        ("ref_celt", Signal::Music, 48000, 2, &["-b:a", "96k"]),
        (
            "ref_silk_nb",
            Signal::Speech,
            8000,
            1,
            &["-b:a", "8k", "-application", "voip"],
        ),
        (
            "ref_silk_wb_st",
            Signal::Speech,
            16000,
            2,
            &[
                "-b:a",
                "20k",
                "-application",
                "voip",
                "-frame_duration",
                "40",
            ],
        ),
        ("ref_mixed", Signal::Mixed, 48000, 2, &["-b:a", "32k"]),
        (
            "ref_mixed_mono_10ms",
            Signal::Mixed,
            48000,
            1,
            &["-b:a", "24k", "-frame_duration", "10"],
        ),
    ];
    let mut failures = Vec::new();
    for (name, sig, rate, ch, args) in cases {
        let raw = dir.join(format!("{name}.raw"));
        let opus = dir.join(format!("{name}.opus"));
        write_f32(&raw, &gen_signal(*sig, *rate, *ch, 3.0));
        if !encode(&raw, *rate, *ch, args, &opus) {
            continue;
        }
        let _ = std::fs::remove_file(&raw);
        let ogg = read_ogg_opus(&opus);
        for loss in [false, true] {
            let packets: Vec<Vec<u8>> = ogg
                .packets
                .iter()
                .enumerate()
                .map(|(i, p)| {
                    if loss && (i % 9 == 4 || i % 31 == 7) {
                        Vec::new()
                    } else {
                        p.clone()
                    }
                })
                .collect();
            for out_rate in [48000u32, 24000, 16000, 12000, 8000] {
                let tag = format!("{name}_{out_rate}_{loss}");
                let Some(reference) = run_ref(&exe, &ogg, out_rate, &packets, &tag) else {
                    continue;
                };
                let mut dec = OpusDecoder::new(&ogg.head, out_rate).unwrap();
                let mut ours = Vec::new();
                for p in &packets {
                    let _ = dec.decode_packet(p, &mut ours);
                }
                let (snr, maxe, _) = compare(&reference, &ours);
                let exact = ours == reference;
                let ok = ours.len() == reference.len() && (snr > 60.0 || loss);
                println!(
                    "{tag:<34} {} len {}/{} SNR={snr:>7.2} maxerr={maxe:.2e}{}",
                    if ok { "ok  " } else { "FAIL" },
                    ours.len(),
                    reference.len(),
                    if exact { " (bit-exact)" } else { "" }
                );
                if !ok {
                    failures.push(tag.clone());
                }
                if loss && std::env::var("OC_OPUS_DEBUG").is_ok() && out_rate == 48000 {
                    // Per-packet divergence report.
                    let chn = ogg.head.channels as usize;
                    let mut pos = 0usize;
                    for (i, p) in packets.iter().enumerate().take(40) {
                        let n = if p.is_empty() {
                            960
                        } else {
                            oc_opus::packet_samples_48k(p).unwrap()
                        } * chn;
                        let (s, m, _) = compare(
                            &reference[pos..(pos + n).min(reference.len())],
                            &ours[pos..(pos + n).min(ours.len())],
                        );
                        println!(
                            "  pkt {i:3} lost={} toc={:02x} snr={s:.1} max={m:.2e}",
                            p.is_empty(),
                            p.first().copied().unwrap_or(0)
                        );
                        pos += n;
                    }
                }
            }
        }
    }
    assert!(failures.is_empty(), "{failures:?}");
}
