//! Conformance tests against the libopus reference decoder (via ffmpeg).
//! Fixtures are generated at test time; tests are skipped (pass) when ffmpeg
//! with libopus is not available.

mod common;
use common::*;

struct Case {
    name: &'static str,
    signal: Signal,
    rate: u32,
    channels: usize,
    args: &'static [&'static str],
    min_snr: f64,
}

/// ffmpeg's libopus wrapper reorders 3..8 channel streams from Vorbis to its
/// native order even for mapping family 255 (whose channels are officially
/// unidentified); oc-opus leaves family 255 in stream order. Apply ffmpeg's
/// permutation to our output for comparison.
fn ffmpeg_family255_order(pcm: &[f32], ch: usize) -> Vec<f32> {
    const OFF: [[usize; 8]; 8] = [
        [0, 0, 0, 0, 0, 0, 0, 0],
        [0, 1, 0, 0, 0, 0, 0, 0],
        [0, 2, 1, 0, 0, 0, 0, 0],
        [0, 1, 2, 3, 0, 0, 0, 0],
        [0, 2, 1, 3, 4, 0, 0, 0],
        [0, 2, 1, 5, 3, 4, 0, 0],
        [0, 2, 1, 6, 5, 3, 4, 0],
        [0, 2, 1, 7, 5, 6, 3, 4],
    ];
    let mut out = vec![0f32; pcm.len()];
    for (o, i) in out.chunks_exact_mut(ch).zip(pcm.chunks_exact(ch)) {
        for k in 0..ch {
            o[k] = i[OFF[ch - 1][k]];
        }
    }
    out
}

fn run_cases(cases: &[Case]) {
    if !ffmpeg_available() {
        eprintln!("NOTE: ffmpeg with libopus not found, skipping reference comparison");
        return;
    }
    let dir = work_dir();
    let mut failures = Vec::new();
    for c in cases {
        let raw = dir.join(format!("{}.in.raw", c.name));
        let opus = dir.join(format!("{}.opus", c.name));
        let refp = dir.join(format!("{}.ref.raw", c.name));
        let input = gen_signal(c.signal, c.rate, c.channels, 3.0);
        write_f32(&raw, &input);
        if !encode(&raw, c.rate, c.channels, c.args, &opus) {
            eprintln!("{}: encoding failed, skipping", c.name);
            continue;
        }
        let Some(reference) = ref_decode(&opus, &refp) else {
            eprintln!("{}: reference decoding failed, skipping", c.name);
            continue;
        };
        let ogg = read_ogg_opus(&opus);
        let mut ours = our_decode(&ogg, 48000);
        if ogg.head.mapping_family == 255 && (3..=8).contains(&c.channels) {
            ours = ffmpeg_family255_order(&ours, c.channels);
        }
        let (snr, maxe, n) = compare(&reference, &ours);
        let ok = snr >= c.min_snr && ours.len() == reference.len();
        println!(
            "{:<28} {} ch={} len ours/ref={}/{} SNR={:>7.2} dB maxerr={:.3e} [{}]",
            c.name,
            if ok { "ok  " } else { "FAIL" },
            c.channels,
            ours.len(),
            reference.len(),
            snr,
            maxe,
            toc_stats(&ogg.packets)
        );
        let _ = n;
        if !ok {
            failures.push(c.name);
        }
        let _ = std::fs::remove_file(&raw);
        let _ = std::fs::remove_file(&refp);
    }
    assert!(failures.is_empty(), "failed cases: {failures:?}");
}

#[test]
fn celt_only() {
    run_cases(&[
        Case {
            name: "celt_fb_stereo_128k_20ms",
            signal: Signal::Music,
            rate: 48000,
            channels: 2,
            args: &["-b:a", "128k", "-application", "audio"],
            min_snr: 60.0,
        },
        Case {
            name: "celt_fb_stereo_64k_20ms",
            signal: Signal::Music,
            rate: 48000,
            channels: 2,
            args: &["-b:a", "64k"],
            min_snr: 60.0,
        },
        Case {
            name: "celt_fb_stereo_256k_10ms",
            signal: Signal::Music,
            rate: 48000,
            channels: 2,
            args: &["-b:a", "256k", "-frame_duration", "10"],
            min_snr: 60.0,
        },
        Case {
            name: "celt_fb_mono_64k_5ms",
            signal: Signal::Music,
            rate: 48000,
            channels: 1,
            args: &["-b:a", "64k", "-frame_duration", "5"],
            min_snr: 60.0,
        },
        Case {
            name: "celt_fb_stereo_96k_2.5ms",
            signal: Signal::Music,
            rate: 48000,
            channels: 2,
            args: &["-b:a", "96k", "-frame_duration", "2.5"],
            min_snr: 60.0,
        },
        Case {
            name: "celt_lowdelay_24k_20ms",
            signal: Signal::Music,
            rate: 48000,
            channels: 2,
            args: &["-b:a", "24k", "-application", "lowdelay"],
            min_snr: 60.0,
        },
        Case {
            name: "celt_lowdelay_12k_mono",
            signal: Signal::Music,
            rate: 48000,
            channels: 1,
            args: &["-b:a", "12k", "-application", "lowdelay"],
            min_snr: 60.0,
        },
        Case {
            name: "celt_cbr_64k_40ms",
            signal: Signal::Music,
            rate: 48000,
            channels: 2,
            args: &["-b:a", "64k", "-vbr", "off", "-frame_duration", "40"],
            min_snr: 60.0,
        },
        Case {
            name: "celt_cvbr_160k_60ms",
            signal: Signal::Music,
            rate: 48000,
            channels: 2,
            args: &[
                "-b:a",
                "160k",
                "-vbr",
                "constrained",
                "-frame_duration",
                "60",
            ],
            min_snr: 60.0,
        },
        Case {
            name: "celt_noinv_48k_stereo",
            signal: Signal::Music,
            rate: 48000,
            channels: 2,
            args: &[
                "-b:a",
                "48k",
                "-apply_phase_inv",
                "0",
                "-application",
                "lowdelay",
            ],
            min_snr: 60.0,
        },
        Case {
            name: "celt_120ms_96k",
            signal: Signal::Music,
            rate: 48000,
            channels: 2,
            args: &["-b:a", "96k", "-frame_duration", "120"],
            min_snr: 60.0,
        },
        Case {
            name: "celt_80ms_cbr_48k",
            signal: Signal::Music,
            rate: 48000,
            channels: 1,
            args: &["-b:a", "48k", "-frame_duration", "80", "-vbr", "off"],
            min_snr: 60.0,
        },
        Case {
            name: "celt_lowdelay_2.5ms_cbr",
            signal: Signal::Mixed,
            rate: 48000,
            channels: 2,
            args: &[
                "-b:a",
                "64k",
                "-application",
                "lowdelay",
                "-frame_duration",
                "2.5",
                "-vbr",
                "off",
            ],
            min_snr: 60.0,
        },
        Case {
            name: "celt_24kin_32k",
            signal: Signal::Music,
            rate: 24000,
            channels: 2,
            args: &["-b:a", "32k", "-application", "lowdelay"],
            min_snr: 60.0,
        },
    ]);
}

#[test]
fn silk_only() {
    run_cases(&[
        Case {
            name: "silk_nb_mono_8k_20ms",
            signal: Signal::Speech,
            rate: 8000,
            channels: 1,
            args: &["-b:a", "8k", "-application", "voip"],
            min_snr: 60.0,
        },
        Case {
            name: "silk_nb_mono_6k_60ms",
            signal: Signal::Speech,
            rate: 8000,
            channels: 1,
            args: &[
                "-b:a",
                "6k",
                "-application",
                "voip",
                "-frame_duration",
                "60",
            ],
            min_snr: 60.0,
        },
        Case {
            name: "silk_mb_mono_12k_10ms",
            signal: Signal::Speech,
            rate: 12000,
            channels: 1,
            args: &[
                "-b:a",
                "12k",
                "-application",
                "voip",
                "-frame_duration",
                "10",
            ],
            min_snr: 60.0,
        },
        Case {
            name: "silk_wb_mono_16k_40ms",
            signal: Signal::Speech,
            rate: 16000,
            channels: 1,
            args: &[
                "-b:a",
                "16k",
                "-application",
                "voip",
                "-frame_duration",
                "40",
            ],
            min_snr: 60.0,
        },
        Case {
            name: "silk_wb_stereo_24k_20ms",
            signal: Signal::Speech,
            rate: 16000,
            channels: 2,
            args: &["-b:a", "24k", "-application", "voip"],
            min_snr: 60.0,
        },
        Case {
            name: "silk_wb_fec_12k",
            signal: Signal::Speech,
            rate: 16000,
            channels: 1,
            args: &[
                "-b:a",
                "12k",
                "-application",
                "voip",
                "-packet_loss",
                "20",
                "-fec",
                "1",
            ],
            min_snr: 60.0,
        },
        Case {
            name: "silk_wb_stereo_fec_60ms",
            signal: Signal::Speech,
            rate: 16000,
            channels: 2,
            args: &[
                "-b:a",
                "20k",
                "-application",
                "voip",
                "-packet_loss",
                "10",
                "-fec",
                "1",
                "-frame_duration",
                "60",
            ],
            min_snr: 60.0,
        },
        Case {
            name: "silk_nb_100ms",
            signal: Signal::Speech,
            rate: 8000,
            channels: 1,
            args: &[
                "-b:a",
                "8k",
                "-application",
                "voip",
                "-frame_duration",
                "100",
            ],
            min_snr: 60.0,
        },
        Case {
            name: "silk_mb_stereo_120ms",
            signal: Signal::Speech,
            rate: 12000,
            channels: 2,
            args: &[
                "-b:a",
                "16k",
                "-application",
                "voip",
                "-frame_duration",
                "120",
            ],
            min_snr: 60.0,
        },
        Case {
            name: "silk_cbr_10k",
            signal: Signal::Speech,
            rate: 16000,
            channels: 1,
            args: &["-b:a", "10k", "-application", "voip", "-vbr", "off"],
            min_snr: 60.0,
        },
    ]);
}

#[test]
fn hybrid_and_transitions() {
    run_cases(&[
        Case {
            name: "hybrid_swb_mono_24k",
            signal: Signal::Speech,
            rate: 24000,
            channels: 1,
            args: &["-b:a", "24k", "-application", "voip"],
            min_snr: 60.0,
        },
        Case {
            name: "hybrid_fb_stereo_32k",
            signal: Signal::Speech,
            rate: 48000,
            channels: 2,
            args: &["-b:a", "32k", "-application", "voip"],
            min_snr: 60.0,
        },
        Case {
            name: "hybrid_fb_mono_20k_10ms",
            signal: Signal::Speech,
            rate: 48000,
            channels: 1,
            args: &[
                "-b:a",
                "20k",
                "-application",
                "voip",
                "-frame_duration",
                "10",
            ],
            min_snr: 60.0,
        },
        Case {
            name: "hybrid_fb_stereo_40ms_fec",
            signal: Signal::Speech,
            rate: 48000,
            channels: 2,
            args: &[
                "-b:a",
                "28k",
                "-frame_duration",
                "40",
                "-packet_loss",
                "10",
                "-fec",
                "1",
            ],
            min_snr: 60.0,
        },
        Case {
            name: "mixed_audio_24k",
            signal: Signal::Mixed,
            rate: 48000,
            channels: 1,
            args: &["-b:a", "24k", "-application", "audio"],
            min_snr: 60.0,
        },
        Case {
            name: "mixed_audio_stereo_32k",
            signal: Signal::Mixed,
            rate: 48000,
            channels: 2,
            args: &["-b:a", "32k", "-application", "audio"],
            min_snr: 60.0,
        },
        Case {
            name: "mixed_audio_48k_10ms",
            signal: Signal::Mixed,
            rate: 48000,
            channels: 2,
            args: &["-b:a", "48k", "-frame_duration", "10"],
            min_snr: 60.0,
        },
        Case {
            name: "mixed_voip_16k_mono",
            signal: Signal::Mixed,
            rate: 48000,
            channels: 1,
            args: &["-b:a", "16k", "-application", "voip"],
            min_snr: 60.0,
        },
        Case {
            name: "mixed_audio_20k_60ms",
            signal: Signal::Mixed,
            rate: 48000,
            channels: 2,
            args: &["-b:a", "20k", "-frame_duration", "60"],
            min_snr: 60.0,
        },
    ]);
}

#[test]
fn multistream() {
    run_cases(&[
        Case {
            name: "ms_5.1_family1_256k",
            signal: Signal::Music,
            rate: 48000,
            channels: 6,
            args: &["-b:a", "256k", "-mapping_family", "1"],
            min_snr: 60.0,
        },
        Case {
            name: "ms_5.1_family1_96k",
            signal: Signal::Mixed,
            rate: 48000,
            channels: 6,
            args: &["-b:a", "96k", "-mapping_family", "1"],
            min_snr: 60.0,
        },
        Case {
            name: "ms_7.1_family1_320k",
            signal: Signal::Music,
            rate: 48000,
            channels: 8,
            args: &["-b:a", "320k", "-mapping_family", "1"],
            min_snr: 60.0,
        },
        Case {
            name: "ms_quad_family1",
            signal: Signal::Music,
            rate: 48000,
            channels: 4,
            args: &["-b:a", "160k", "-mapping_family", "1"],
            min_snr: 60.0,
        },
        Case {
            name: "ms_3ch_family1",
            signal: Signal::Music,
            rate: 48000,
            channels: 3,
            args: &["-b:a", "128k", "-mapping_family", "1"],
            min_snr: 60.0,
        },
        Case {
            name: "ms_stereo_family255",
            signal: Signal::Music,
            rate: 48000,
            channels: 2,
            args: &["-b:a", "96k", "-mapping_family", "255"],
            min_snr: 60.0,
        },
        Case {
            name: "ms_5.1_family255",
            signal: Signal::Music,
            rate: 48000,
            channels: 6,
            args: &["-b:a", "192k", "-mapping_family", "255"],
            min_snr: 60.0,
        },
    ]);
}

/// Output gain from the OpusHead (Q7.8 dB) must match libopus (OPUS_SET_GAIN,
/// which ffmpeg applies from the header).
#[test]
fn output_gain() {
    if !ffmpeg_available() {
        eprintln!("NOTE: ffmpeg with libopus not found, skipping");
        return;
    }
    let dir = work_dir();
    let raw = dir.join("gain.in.raw");
    let opus = dir.join("gain.opus");
    write_f32(&raw, &gen_signal(Signal::Mixed, 48000, 2, 2.0));
    assert!(encode(&raw, 48000, 2, &["-b:a", "48k"], &opus));
    let ogg = read_ogg_opus(&opus);
    for gain in [-1234i16, 512, 3000] {
        // Re-mux with a modified OpusHead.
        let mut head = Vec::new();
        head.extend_from_slice(b"OpusHead");
        head.push(1);
        head.push(ogg.head.channels);
        head.extend_from_slice(&ogg.head.pre_skip.to_le_bytes());
        head.extend_from_slice(&ogg.head.input_sample_rate.to_le_bytes());
        head.extend_from_slice(&gain.to_le_bytes());
        head.push(0);
        let tags = b"OpusTags\x08\x00\x00\x00oc-opus \x00\x00\x00\x00".to_vec();
        let mut file = Vec::new();
        let serial = 0x1234;
        oc_ogg::write_page(&mut file, 2, 0, serial, 0, &[&head]);
        oc_ogg::write_page(&mut file, 0, 0, serial, 1, &[&tags]);
        let mut granule = 0i64;
        let n = ogg.packets.len();
        for (i, p) in ogg.packets.iter().enumerate() {
            granule += oc_opus::packet_samples_48k(p).unwrap() as i64;
            let last = i + 1 == n;
            let g = if last {
                ogg.last_granule as i64
            } else {
                granule
            };
            oc_ogg::write_page(
                &mut file,
                if last { 4 } else { 0 },
                g,
                serial,
                2 + i as u32,
                &[p],
            );
        }
        let path = dir.join(format!("gain{gain}.opus"));
        std::fs::write(&path, &file).unwrap();
        let reference = ref_decode(&path, &dir.join("gain.ref.raw")).unwrap();
        let ogg2 = read_ogg_opus(&path);
        assert_eq!(ogg2.head.output_gain, gain);
        let ours = our_decode(&ogg2, 48000);
        let (snr, maxe, _) = compare(&reference, &ours);
        println!(
            "gain {gain:>5} (Q8 dB): SNR={snr:.2} dB maxerr={maxe:.3e} len {}/{}",
            ours.len(),
            reference.len()
        );
        assert_eq!(ours.len(), reference.len());
        assert!(snr > 60.0);
    }
}
