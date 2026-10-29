//! WebM/Opus to Ogg/Opus remuxing with cuts, tags and a cover, reading the
//! input in small pieces (as from a download in progress).

mod common;

use std::io::{Cursor, Read};

/// Hands out at most 700 bytes per read.
struct Trickle(Cursor<Vec<u8>>);

impl Read for Trickle {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let n = buf.len().min(700);
        self.0.read(&mut buf[..n])
    }
}

/// RMS of 20 ms windows around `t` seconds.
fn level(pcm: &[f32], t: f64) -> f32 {
    let i = (t * 48_000.0) as usize * 2;
    let w = &pcm[i.saturating_sub(960)..(i + 960).min(pcm.len())];
    (w.iter().map(|v| v * v).sum::<f32>() / w.len().max(1) as f32).sqrt()
}

#[test]
fn webm_opus_becomes_ogg_without_the_cut_parts() {
    if !common::have_ffmpeg() {
        eprintln!("ffmpeg not found, skipping");
        return;
    }
    let dir = common::fixture_dir();
    let webm = dir.join("remux_src.webm");
    // 0-3 s tone, 3-5 s silence (an "intro" in the middle), 5-12 s tone.
    assert!(common::ffmpeg(&[
        "-f", "lavfi", "-i", "sine=frequency=440:duration=3:sample_rate=48000",
        "-f", "lavfi", "-i", "anullsrc=r=48000:cl=stereo:d=2",
        "-f", "lavfi", "-i", "sine=frequency=660:duration=7:sample_rate=48000",
        "-filter_complex", "[0]aformat=channel_layouts=stereo[a];[2]aformat=channel_layouts=stereo[c];[a][1][c]concat=n=3:v=0:a=1",
        "-c:a", "libopus", "-b:a", "128k",
        webm.to_str().unwrap(),
    ]));
    let input = std::fs::read(&webm).unwrap();
    let cover = b"\xff\xd8\xff\xe0fake jpeg".to_vec();
    let tags = bl_media::RemuxTags {
        title: "Lied".into(),
        artist: "Band".into(),
        album: "Album".into(),
        date: "2021".into(),
        comments: vec![("SOURCE".into(), "test".into())],
        cover: Some(("image/jpeg".into(), cover.clone())),
    };
    let mut out = Vec::new();
    let r = bl_media::webm_opus_to_ogg(
        Box::new(Trickle(Cursor::new(input))),
        &mut out,
        &[(3.0, 5.0), (11.0, 12.5)],
        &tags,
    )
    .unwrap();
    assert!((r.duration_ms as i64 - 9000).abs() < 60, "{r:?}");
    assert!((r.removed_ms as i64 - 3000).abs() < 60, "{r:?}");

    let info = bl_media::read_info(&mut Cursor::new(out.clone()), Some("opus"), true).unwrap();
    assert_eq!(info.codec, "opus");
    assert_eq!(info.tags.title.as_deref(), Some("Lied"));
    assert_eq!(info.tags.artist.as_deref(), Some("Band"));
    assert_eq!(info.tags.year, Some(2021));
    assert_eq!(info.pictures.len(), 1);
    assert_eq!(info.pictures[0].data, cover);
    let d = info.duration_ms.unwrap() as i64;
    assert!((d - 9000).abs() < 60, "duration {d}");

    // Decodes: the silence is gone (tone right before and after 3 s).
    let mut dec = bl_media::open(Box::new(Cursor::new(out)), Some("opus")).unwrap();
    let mut pcm = Vec::new();
    while dec.next_chunk(&mut pcm).unwrap() {}
    assert!(
        (pcm.len() as i64 / 2 - 9 * 48_000).abs() < 3000,
        "{} frames",
        pcm.len() / 2
    );
    for t in [1.0, 2.9, 3.1, 5.0, 8.5] {
        assert!(level(&pcm, t) > 0.03, "level at {t}: {}", level(&pcm, t));
    }
}

#[test]
fn other_input_is_refused() {
    let mut out = Vec::new();
    let r = bl_media::webm_opus_to_ogg(
        Box::new(Cursor::new(b"ID3\x03\x00\x00\x00\x00\x00\x00".to_vec())),
        &mut out,
        &[],
        &bl_media::RemuxTags::default(),
    );
    assert!(r.is_err());
}
