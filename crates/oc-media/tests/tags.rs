//! Tag and cover-art reading on ffmpeg-generated files of every container.

mod common;

use common::*;

const META: &[&str] = &[
    "-metadata",
    "title=Tïtle ünicode",
    "-metadata",
    "artist=Test Artist",
    "-metadata",
    "album=Test Album",
    "-metadata",
    "album_artist=Various",
    "-metadata",
    "composer=Some Composer",
    "-metadata",
    "date=2021",
    "-metadata",
    "track=3/12",
    "-metadata",
    "disc=1/2",
    "-metadata",
    "genre=Rock",
    "-metadata",
    "comment=A comment",
    "-metadata",
    "REPLAYGAIN_TRACK_GAIN=-6.50 dB",
    "-metadata",
    "REPLAYGAIN_TRACK_PEAK=0.988",
];

struct Case {
    file: &'static str,
    args: Vec<&'static str>,
    with_cover: bool,
    /// Fields the container/muxer is expected to carry.
    expect_album_artist: bool,
    expect_track_total: bool,
    expect_replaygain: bool,
    expect_picture: bool,
}

fn make_tagged(c: &Case) -> Option<std::path::PathBuf> {
    let mut key = c.args.clone();
    key.push(if c.with_cover { "cover" } else { "" });
    key.extend_from_slice(META);
    let p = fixture_dir().join(cache_name(c.file, &key));
    if p.exists() {
        return Some(p);
    }
    let m = master("stereo44");
    let cover = cover_png();
    let tmp = fixture_dir().join(format!("tmp_{}", c.file));
    let mut a: Vec<&str> = vec!["-i", m.to_str().unwrap()];
    if c.with_cover {
        a.extend_from_slice(&["-i", cover.to_str().unwrap(), "-map", "0", "-map", "1", "-c:v", "copy", "-disposition:v", "attached_pic"]);
    }
    a.extend_from_slice(META);
    let cover_s = cover.to_str().unwrap().to_string();
    a.extend(c.args.iter().map(|&x| if x == "COVER" { cover_s.as_str() } else { x }));
    let t = tmp.to_str().unwrap().to_string();
    a.push(&t);
    if !ffmpeg(&a) {
        return None;
    }
    std::fs::rename(&tmp, &p).ok()?;
    Some(p)
}

#[test]
fn tags_and_pictures() {
    if !have_ffmpeg() {
        eprintln!("ffmpeg not found: skipping");
        return;
    }
    let case = |file, args: &[&'static str], with_cover, aa, tt, rg, pic| Case {
        file,
        args: args.to_vec(),
        with_cover,
        expect_album_artist: aa,
        expect_track_total: tt,
        expect_replaygain: rg,
        expect_picture: pic,
    };
    let cases = vec![
        case("tag_v23.mp3", &["-c:a", "libmp3lame", "-id3v2_version", "3"], true, true, true, true, true),
        case("tag_v24.mp3", &["-c:a", "libmp3lame", "-id3v2_version", "4"], true, true, true, true, true),
        case("tag.flac", &["-c:a", "flac"], true, true, true, true, true),
        case("tag.ogg", &["-c:a", "libvorbis"], false, true, true, true, false),
        case("tag.opus", &["-c:a", "libopus"], false, true, true, true, false),
        case("tag.m4a", &["-c:a", "aac"], true, true, true, false, true),
        case("tag_mdta.m4a", &["-c:a", "aac", "-movflags", "use_metadata_tags"], false, true, true, true, false),
        case("tag_alac.m4a", &["-c:a", "alac"], true, true, true, false, true),
        case("tag.mka", &["-c:a", "libopus"], false, true, true, true, false),
        case("tag_attach.mka", &["-c:a", "libopus", "-attach", "COVER", "-metadata:s:t", "mimetype=image/png"], false, true, true, true, true),
        case("tag.wav", &["-c:a", "pcm_s16le"], false, false, false, false, false),
        case("tag.aiff", &["-c:a", "pcm_s16be", "-write_id3v2", "1"], true, true, true, true, true),
        case("tag.aac", &["-c:a", "aac", "-f", "adts", "-write_id3v2", "1", "-write_apetag", "1"], false, true, true, true, false),
    ];
    let mut failures = Vec::new();
    for c in &cases {
        let Some(p) = make_tagged(c) else {
            eprintln!("{}: fixture generation failed", c.file);
            continue;
        };
        let mut f = std::fs::File::open(&p).unwrap();
        let info = match oc_media::read_info(&mut f, Some(&ext_of(&p)), true) {
            Ok(i) => i,
            Err(e) => {
                failures.push(format!("{}: {e}", c.file));
                continue;
            }
        };
        let t = &info.tags;
        println!("{:<14} {:?}", c.file, t);
        for pic in &info.pictures {
            println!("{:<14}   picture {} kind {} desc {:?} {} bytes", "", pic.mime, pic.kind, pic.description, pic.data.len());
        }
        let mut check = |ok: bool, what: &str| {
            if !ok {
                failures.push(format!("{}: {what} wrong: {t:?}", c.file));
            }
        };
        check(t.title.as_deref() == Some("Tïtle ünicode"), "title");
        check(t.artist.as_deref() == Some("Test Artist"), "artist");
        check(t.album.as_deref() == Some("Test Album"), "album");
        check(t.year == Some(2021), "year");
        check(t.track == Some(3), "track");
        check(t.genre.as_deref() == Some("Rock"), "genre");
        if c.expect_album_artist {
            check(t.album_artist.as_deref() == Some("Various"), "album artist");
            check(t.composer.as_deref() == Some("Some Composer"), "composer");
            check(t.disc == Some(1), "disc");
        }
        if c.expect_track_total {
            check(t.track_total == Some(12), "track total");
        }
        if c.expect_replaygain {
            check(t.replaygain_track_gain == Some(-6.5), "replaygain gain");
            check(t.replaygain_track_peak.is_some_and(|p| (p - 0.988).abs() < 1e-6), "replaygain peak");
        }
        if c.expect_picture {
            let png = std::fs::read(cover_png()).unwrap();
            let ok = info.pictures.iter().any(|p| p.mime == "image/png" && p.data == png);
            check(ok, "picture");
        }
        // Without pictures requested, none are returned.
        let mut f = std::fs::File::open(&p).unwrap();
        let info2 = oc_media::read_info(&mut f, Some(&ext_of(&p)), false).unwrap();
        check(info2.pictures.is_empty() && info2.tags == info.tags, "with_pictures=false");
    }
    assert!(failures.is_empty(), "failures:\n{}", failures.join("\n"));
}

/// MP3 with only trailing tags: APEv2 (preferred) followed by ID3v1.
#[test]
fn ape_and_id3v1_at_end() {
    if !have_ffmpeg() {
        eprintln!("ffmpeg not found: skipping");
        return;
    }
    let Some(p) = make("plain.mp3", "stereo44", &["-c:a", "libmp3lame", "-id3v2_version", "0", "-write_xing", "0"]) else { return };
    let mut data = std::fs::read(&p).unwrap();
    let audio_len = data.len();
    // APEv2 tag (footer only).
    let items: &[(&str, &str)] = &[("Title", "Ape Title"), ("Artist", "Ape Artist"), ("Track", "5/9"), ("REPLAYGAIN_TRACK_GAIN", "+1.25 dB")];
    let mut body = Vec::new();
    for (k, v) in items {
        body.extend_from_slice(&(v.len() as u32).to_le_bytes());
        body.extend_from_slice(&0u32.to_le_bytes());
        body.extend_from_slice(k.as_bytes());
        body.push(0);
        body.extend_from_slice(v.as_bytes());
    }
    data.extend_from_slice(&body);
    data.extend_from_slice(b"APETAGEX");
    data.extend_from_slice(&2000u32.to_le_bytes());
    data.extend_from_slice(&((body.len() + 32) as u32).to_le_bytes());
    data.extend_from_slice(&(items.len() as u32).to_le_bytes());
    data.extend_from_slice(&[0u8; 12]);
    // ID3v1.1 tag.
    let mut v1 = vec![0u8; 128];
    v1[..3].copy_from_slice(b"TAG");
    v1[3..11].copy_from_slice(b"V1 Title");
    v1[63..71].copy_from_slice(b"V1 Album");
    v1[93..97].copy_from_slice(b"1999");
    v1[126] = 7;
    v1[127] = 13; // Pop
    data.extend_from_slice(&v1);
    let mut c = std::io::Cursor::new(data);
    let info = oc_media::read_info(&mut c, Some("mp3"), false).unwrap();
    let t = &info.tags;
    assert_eq!(t.title.as_deref(), Some("Ape Title"));
    assert_eq!(t.artist.as_deref(), Some("Ape Artist"));
    assert_eq!(t.album.as_deref(), Some("V1 Album"));
    assert_eq!((t.track, t.track_total), (Some(5), Some(9)));
    assert_eq!(t.year, Some(1999));
    assert_eq!(t.genre.as_deref(), Some("Pop"));
    assert_eq!(t.replaygain_track_gain, Some(1.25));
    // The trailing tags must not change the duration estimate.
    let mut f = std::fs::File::open(&p).unwrap();
    let plain = oc_media::read_info(&mut f, Some("mp3"), false).unwrap();
    assert_eq!(plain.duration_ms, info.duration_ms);
    // Decoding must stop before the tags.
    let mut d = oc_media::open(Box::new(c), Some("mp3")).unwrap();
    let mut out = Vec::new();
    while d.next_chunk(&mut out).unwrap() {}
    let mut d2 = oc_media::open(Box::new(std::fs::File::open(&p).unwrap()), Some("mp3")).unwrap();
    let mut out2 = Vec::new();
    while d2.next_chunk(&mut out2).unwrap() {}
    assert_eq!(out.len(), out2.len(), "audio length changed by trailing tags ({audio_len} bytes audio)");
}
