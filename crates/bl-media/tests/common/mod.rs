//! Shared helpers for the ffmpeg-driven integration tests.
#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::process::Command;

pub fn fixture_dir() -> PathBuf {
    let d = std::env::temp_dir().join("bl_media_fixtures_v2");
    std::fs::create_dir_all(&d).unwrap();
    d
}

pub fn have_ffmpeg() -> bool {
    Command::new("ffmpeg")
        .arg("-version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
        && Command::new("ffprobe")
            .arg("-version")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
}

/// Runs ffmpeg with the given arguments (output file last). Returns false on
/// failure.
pub fn ffmpeg(args: &[&str]) -> bool {
    let out = Command::new("ffmpeg")
        .args(["-y", "-hide_banner", "-loglevel", "error", "-nostdin"])
        .args(args)
        .output();
    match out {
        Ok(o) if o.status.success() => true,
        Ok(o) => {
            eprintln!(
                "ffmpeg {:?} failed: {}",
                args,
                String::from_utf8_lossy(&o.stderr)
            );
            false
        }
        Err(_) => false,
    }
}

/// Decodes a file with ffmpeg to interleaved f32 (native channel order).
pub fn ffmpeg_decode(path: &Path) -> Option<Vec<f32>> {
    let o = Command::new("ffmpeg")
        .args(["-hide_banner", "-loglevel", "error", "-nostdin", "-i"])
        .arg(path)
        .args(["-map", "0:a:0", "-f", "f32le", "-acodec", "pcm_f32le", "-"])
        .output()
        .ok()?;
    if !o.status.success() {
        return None;
    }
    Some(
        o.stdout
            .chunks_exact(4)
            .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
            .collect(),
    )
}

pub fn ffprobe_duration(path: &Path) -> Option<f64> {
    let o = Command::new("ffprobe")
        .args([
            "-v",
            "error",
            "-show_entries",
            "format=duration",
            "-of",
            "default=nw=1:nk=1",
        ])
        .arg(path)
        .output()
        .ok()?;
    String::from_utf8_lossy(&o.stdout).trim().parse().ok()
}

/// Source signals (generated once).
pub fn master(name: &str) -> PathBuf {
    let dir = fixture_dir();
    let p = dir.join(format!("master_{name}.wav"));
    if p.exists() {
        return p;
    }
    let tmp = dir.join(
        format!(
            "master_{name}.{}.{:?}.tmp.wav",
            std::process::id(),
            std::thread::current().id()
        )
        .replace(['(', ')'], ""),
    );
    let ok = match name {
        "stereo44" => ffmpeg(&[
            "-f",
            "lavfi",
            "-i",
            "aevalsrc=exprs='0.3*sin(2*PI*440*t)+0.1*sin(2*PI*3100*t)+0.1*sin(2*PI*(150*t+900*t*t))|0.3*sin(2*PI*554*t)+0.08*sin(2*PI*6000*t)+0.1*sin(2*PI*(300*t+700*t*t))':s=44100:d=3.3",
            "-c:a",
            "pcm_f32le",
            tmp.to_str().unwrap(),
        ]),
        "stereo48" => ffmpeg(&[
            "-f",
            "lavfi",
            "-i",
            "aevalsrc=exprs='0.3*sin(2*PI*440*t)+0.1*sin(2*PI*3100*t)+0.1*sin(2*PI*(150*t+900*t*t))|0.3*sin(2*PI*554*t)+0.08*sin(2*PI*6000*t)+0.1*sin(2*PI*(300*t+700*t*t))':s=48000:d=3.3",
            "-c:a",
            "pcm_f32le",
            tmp.to_str().unwrap(),
        ]),
        "surround51" => ffmpeg(&[
            "-f",
            "lavfi",
            "-i",
            "aevalsrc=exprs='0.3*sin(2*PI*300*t)+0.1*sin(2*PI*(150*t+900*t*t))|0.3*sin(2*PI*400*t)|0.3*sin(2*PI*500*t)|0.3*sin(2*PI*60*t)|0.3*sin(2*PI*700*t)|0.3*sin(2*PI*800*t)':s=48000:d=2:c=5.1",
            "-c:a",
            "pcm_f32le",
            tmp.to_str().unwrap(),
        ]),
        "mono22" => ffmpeg(&[
            "-f",
            "lavfi",
            "-i",
            "aevalsrc=exprs='0.4*sin(2*PI*1000*t)+0.1*sin(2*PI*(150*t+900*t*t))':s=22050:d=2.5",
            "-c:a",
            "pcm_f32le",
            tmp.to_str().unwrap(),
        ]),
        _ => false,
    };
    assert!(ok, "could not generate master {name}");
    publish(&tmp, &p);
    p
}

pub fn cover_png() -> PathBuf {
    let p = fixture_dir().join("cover.png");
    if !p.exists() {
        let tmp = fixture_dir().join(
            format!(
                "cover.{}.{:?}.tmp.png",
                std::process::id(),
                std::thread::current().id()
            )
            .replace(['(', ')'], ""),
        );
        assert!(ffmpeg(&[
            "-f",
            "lavfi",
            "-i",
            "color=c=red:s=32x32",
            "-frames:v",
            "1",
            tmp.to_str().unwrap()
        ]));
        publish(&tmp, &p);
    }
    p
}

/// Cache file name for a fixture: includes a hash of the generating
/// arguments so changed fixtures are regenerated.
pub fn cache_name(name: &str, args: &[&str]) -> String {
    let mut h: u64 = 0xcbf29ce484222325;
    for a in args {
        for b in a.bytes().chain(std::iter::once(0)) {
            h = (h ^ b as u64).wrapping_mul(0x100000001b3);
        }
    }
    format!("{:08x}_{name}", h as u32)
}

/// Builds a fixture `name` from master `src` with extra ffmpeg output args.
pub fn make(name: &str, src: &str, args: &[&str]) -> Option<PathBuf> {
    let p = fixture_dir().join(cache_name(name, args));
    if p.exists() {
        return Some(p);
    }
    let m = master(src);
    let tmp = fixture_dir().join(
        format!(
            "tmp_{}_{:?}_{name}",
            std::process::id(),
            std::thread::current().id()
        )
        .replace(['(', ')'], ""),
    );
    let mut a: Vec<&str> = vec!["-i", m.to_str().unwrap()];
    a.extend_from_slice(args);
    // Keep the extension so ffmpeg picks the right muxer.
    let tmp_s = tmp.to_str().unwrap().to_string();
    a.push(&tmp_s);
    if !ffmpeg(&a) {
        return None;
    }
    publish(&tmp, &p);
    p.exists().then_some(p)
}

/// Moves a generated fixture into place. Tests run in parallel and may
/// generate the same fixture; on Windows a file another test has open
/// cannot be replaced, and the copy already there is just as good.
fn publish(tmp: &Path, p: &Path) {
    if std::fs::rename(tmp, p).is_err() {
        let _ = std::fs::remove_file(tmp);
        assert!(p.exists(), "could not store {}", p.display());
    }
}

pub fn ext_of(p: &Path) -> String {
    p.extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_string()
}

/// (spec, total_frames, samples, codec name)
pub type Decoded = (bl_media::AudioSpec, Option<u64>, Vec<f32>, &'static str);

/// Opens and fully decodes a file with bl-media.
pub fn decode_all(p: &Path) -> Result<Decoded, bl_media::Error> {
    let f = std::fs::File::open(p).unwrap();
    let ext = ext_of(p);
    let mut d = bl_media::open(Box::new(f), Some(&ext))?;
    let mut out = Vec::new();
    while d.next_chunk(&mut out)? {}
    Ok((d.spec(), d.total_frames(), out, d.codec_name()))
}

/// Decodes `len` frames after seeking to `frame`.
pub fn decode_after_seek(
    p: &Path,
    frame: u64,
    len: usize,
) -> Result<(u64, Vec<f32>), bl_media::Error> {
    let f = std::fs::File::open(p).unwrap();
    let ext = ext_of(p);
    let mut d = bl_media::open(Box::new(f), Some(&ext))?;
    let ch = d.spec().channels as usize;
    let at = d.seek(frame)?;
    let mut out = Vec::new();
    while out.len() < len * ch {
        if !d.next_chunk(&mut out)? {
            break;
        }
    }
    out.truncate(len * ch);
    Ok((at, out))
}

/// (max abs error, SNR in dB) of `a` against reference `b` over the common length.
pub fn compare(a: &[f32], b: &[f32]) -> (f32, f64) {
    let n = a.len().min(b.len());
    let mut max = 0f32;
    let (mut sig, mut err) = (0f64, 0f64);
    for i in 0..n {
        let e = a[i] - b[i];
        max = max.max(e.abs());
        sig += (b[i] as f64).powi(2);
        err += (e as f64).powi(2);
    }
    let snr = if err == 0.0 {
        f64::INFINITY
    } else {
        10.0 * (sig / err).log10()
    };
    (max, snr)
}

/// Finds the offset (in frames, `a` relative to `b`) maximising correlation.
pub fn best_offset(a: &[f32], b: &[f32], ch: usize, max_shift: i64) -> i64 {
    let fa = a.len() / ch;
    let fb = b.len() / ch;
    let win = 32768.min(fa / 2).min(fb / 2) as i64;
    let start = (fb as i64 / 3).max(max_shift);
    let mut best = (f64::MIN, 0i64);
    for s in -max_shift..=max_shift {
        let mut acc = 0f64;
        for i in 0..win {
            let ib = start + i;
            let ia = ib + s;
            if ia < 0 || ia as usize >= fa || ib as usize >= fb {
                continue;
            }
            acc += a[ia as usize * ch] as f64 * b[ib as usize * ch] as f64;
        }
        if acc > best.0 {
            best = (acc, s);
        }
    }
    best.1
}

pub fn is_unsupported(e: &bl_media::Error) -> bool {
    matches!(e, bl_media::Error::Unsupported(_))
}

pub struct Fixture {
    pub file: &'static str,
    pub master: &'static str,
    pub args: &'static [&'static str],
    pub lossless: bool,
    pub codec: &'static str,
    pub container: &'static str,
    /// Frames ffmpeg outputs before our first frame (ffmpeg does not apply
    /// Matroska CodecDelay for AAC).
    pub ref_skip: usize,
    /// ffmpeg does not trim the end padding (MP4 AAC edit list end): our
    /// output must instead have the master's length.
    pub master_len: bool,
    /// Expected offset of our output against the source signal (`None`:
    /// not checked); non-zero where the format has no gapless information.
    pub align: Option<i64>,
}

pub fn fixtures() -> Vec<Fixture> {
    let f = |file, master, args, lossless, codec, container| Fixture {
        file,
        master,
        args,
        lossless,
        codec,
        container,
        ref_skip: 0,
        master_len: false,
        align: Some(0),
    };
    let mut v = vec![
        f(
            "s16.wav",
            "stereo44",
            &["-c:a", "pcm_s16le"],
            true,
            "pcm",
            "wav",
        ),
        f(
            "s24.wav",
            "stereo44",
            &["-c:a", "pcm_s24le"],
            true,
            "pcm",
            "wav",
        ),
        f(
            "s32.wav",
            "stereo44",
            &["-c:a", "pcm_s32le"],
            true,
            "pcm",
            "wav",
        ),
        f("u8.wav", "mono22", &["-c:a", "pcm_u8"], true, "pcm", "wav"),
        f(
            "f32.wav",
            "stereo44",
            &["-c:a", "pcm_f32le"],
            true,
            "pcm",
            "wav",
        ),
        f(
            "f64.wav",
            "stereo44",
            &["-c:a", "pcm_f64le"],
            true,
            "pcm",
            "wav",
        ),
        f(
            "ext51.wav",
            "surround51",
            &["-c:a", "pcm_s16le"],
            true,
            "pcm",
            "wav",
        ),
        f(
            "rf64.wav",
            "stereo44",
            &["-c:a", "pcm_s16le", "-rf64", "always"],
            true,
            "pcm",
            "wav",
        ),
        f(
            "s16.aiff",
            "stereo44",
            &["-c:a", "pcm_s16be"],
            true,
            "pcm",
            "aiff",
        ),
        f(
            "s24.aiff",
            "stereo44",
            &["-c:a", "pcm_s24be"],
            true,
            "pcm",
            "aiff",
        ),
        f(
            "sowt.aifc",
            "stereo44",
            &["-c:a", "pcm_s16le"],
            true,
            "pcm",
            "aiff",
        ),
        f(
            "fl64.aifc",
            "stereo44",
            &["-c:a", "pcm_f64be"],
            true,
            "pcm",
            "aiff",
        ),
        f(
            "fl32.aifc",
            "stereo44",
            &["-c:a", "pcm_f32be"],
            true,
            "pcm",
            "aiff",
        ),
        f(
            "s16.flac",
            "stereo44",
            &["-c:a", "flac"],
            true,
            "flac",
            "flac",
        ),
        f(
            "s24.flac",
            "stereo44",
            &[
                "-c:a",
                "flac",
                "-sample_fmt",
                "s32",
                "-bits_per_raw_sample",
                "24",
            ],
            true,
            "flac",
            "flac",
        ),
        f(
            "ch6.flac",
            "surround51",
            &["-c:a", "flac"],
            true,
            "flac",
            "flac",
        ),
        f(
            "flac.oga",
            "stereo44",
            &["-c:a", "flac", "-f", "ogg"],
            true,
            "flac",
            "ogg",
        ),
        f(
            "vorbis.ogg",
            "stereo44",
            &["-c:a", "libvorbis", "-q:a", "5"],
            false,
            "vorbis",
            "ogg",
        ),
        f(
            "vorbis_mono.ogg",
            "mono22",
            &["-c:a", "libvorbis"],
            false,
            "vorbis",
            "ogg",
        ),
        f(
            "opus.opus",
            "stereo48",
            &["-c:a", "libopus", "-b:a", "128k"],
            false,
            "opus",
            "ogg",
        ),
        f(
            "opus44.opus",
            "stereo44",
            &["-c:a", "libopus", "-b:a", "96k", "-ar", "48000"],
            false,
            "opus",
            "ogg",
        ),
        f(
            "opus51.opus",
            "surround51",
            &["-c:a", "libopus", "-b:a", "256k", "-mapping_family", "1"],
            false,
            "opus",
            "ogg",
        ),
        f(
            "aac_fast.m4a",
            "stereo44",
            &["-c:a", "aac", "-aac_pns", "0", "-movflags", "+faststart"],
            false,
            "aac",
            "mp4",
        ),
        f(
            "aac.m4a",
            "stereo44",
            &["-c:a", "aac", "-b:a", "160k"],
            false,
            "aac",
            "mp4",
        ),
        f(
            "aac_nopns.m4a",
            "stereo44",
            &["-c:a", "aac", "-b:a", "160k", "-aac_pns", "0"],
            false,
            "aac",
            "mp4",
        ),
        f(
            "adts_nopns.aac",
            "stereo44",
            &["-c:a", "aac", "-b:a", "128k", "-aac_pns", "0", "-f", "adts"],
            false,
            "aac",
            "adts",
        ),
        f(
            "aac_mono.m4a",
            "mono22",
            &["-c:a", "aac", "-b:a", "64k"],
            false,
            "aac",
            "mp4",
        ),
        f(
            "alac16.m4a",
            "stereo44",
            &["-c:a", "alac", "-sample_fmt", "s16p"],
            true,
            "alac",
            "mp4",
        ),
        f(
            "alac24.m4a",
            "stereo44",
            &[
                "-c:a",
                "alac",
                "-sample_fmt",
                "s32p",
                "-bits_per_raw_sample",
                "24",
            ],
            true,
            "alac",
            "mp4",
        ),
        f(
            "alac51.m4a",
            "surround51",
            &["-c:a", "alac", "-sample_fmt", "s16p"],
            true,
            "alac",
            "mp4",
        ),
        f(
            "adts.aac",
            "stereo44",
            &["-c:a", "aac", "-b:a", "128k", "-f", "adts"],
            false,
            "aac",
            "adts",
        ),
        f(
            "opus.mka",
            "stereo48",
            &["-c:a", "libopus", "-b:a", "128k"],
            false,
            "opus",
            "matroska",
        ),
        f(
            "opus.webm",
            "stereo48",
            &["-c:a", "libopus", "-b:a", "96k"],
            false,
            "opus",
            "matroska",
        ),
        f(
            "vorbis.mka",
            "stereo44",
            &["-c:a", "libvorbis"],
            false,
            "vorbis",
            "matroska",
        ),
        f(
            "vorbis.webm",
            "stereo48",
            &["-c:a", "libvorbis"],
            false,
            "vorbis",
            "matroska",
        ),
        f(
            "flac.mka",
            "stereo44",
            &["-c:a", "flac"],
            true,
            "flac",
            "matroska",
        ),
        f(
            "aac.mka",
            "stereo44",
            &["-c:a", "aac"],
            false,
            "aac",
            "matroska",
        ),
        f(
            "pcm.mka",
            "stereo44",
            &["-c:a", "pcm_s16le"],
            true,
            "pcm",
            "matroska",
        ),
        f(
            "mp3.mka",
            "stereo44",
            &["-c:a", "libmp3lame", "-b:a", "128k"],
            false,
            "mp3",
            "matroska",
        ),
        f(
            "opus_cl.mka",
            "stereo48",
            &["-c:a", "libopus", "-cluster_time_limit", "300"],
            false,
            "opus",
            "matroska",
        ),
        f(
            "vorbis_cl.mka",
            "stereo44",
            &["-c:a", "libvorbis", "-cluster_time_limit", "300"],
            false,
            "vorbis",
            "matroska",
        ),
        f(
            "aac_cl.mka",
            "stereo44",
            &["-c:a", "aac", "-aac_pns", "0", "-cluster_time_limit", "300"],
            false,
            "aac",
            "matroska",
        ),
        f(
            "mp3_cl.mka",
            "stereo44",
            &["-c:a", "libmp3lame", "-cluster_time_limit", "300"],
            false,
            "mp3",
            "matroska",
        ),
        f(
            "flac_cl.mka",
            "stereo44",
            &["-c:a", "flac", "-cluster_time_limit", "300"],
            true,
            "flac",
            "matroska",
        ),
        f(
            "opus_live.webm",
            "stereo48",
            &[
                "-c:a",
                "libopus",
                "-live",
                "1",
                "-cluster_time_limit",
                "300",
            ],
            false,
            "opus",
            "matroska",
        ),
        f(
            "vorbis_live.webm",
            "stereo44",
            &[
                "-c:a",
                "libvorbis",
                "-live",
                "1",
                "-cluster_time_limit",
                "300",
            ],
            false,
            "vorbis",
            "matroska",
        ),
        f(
            "alac.mka",
            "stereo44",
            &["-c:a", "alac", "-sample_fmt", "s16p"],
            true,
            "alac",
            "matroska",
        ),
        f(
            "cbr.mp3",
            "stereo44",
            &["-c:a", "libmp3lame", "-b:a", "192k"],
            false,
            "mp3",
            "mp3",
        ),
        f(
            "vbr.mp3",
            "stereo44",
            &["-c:a", "libmp3lame", "-q:a", "2"],
            false,
            "mp3",
            "mp3",
        ),
        f(
            "noxing.mp3",
            "stereo44",
            &["-c:a", "libmp3lame", "-b:a", "128k", "-write_xing", "0"],
            false,
            "mp3",
            "mp3",
        ),
        f(
            "mono22.mp3",
            "mono22",
            &["-c:a", "libmp3lame", "-b:a", "64k"],
            false,
            "mp3",
            "mp3",
        ),
        f(
            "layer2.mp2",
            "stereo48",
            &["-c:a", "mp2", "-b:a", "192k"],
            false,
            "mp2",
            "mp3",
        ),
    ];
    for fx in &mut v {
        match fx.file {
            "aac.m4a" | "aac_mono.m4a" | "aac_nopns.m4a" => fx.master_len = true,
            "aac.mka" | "aac_cl.mka" => fx.ref_skip = 1024,
            "mp3.mka" | "mp3_cl.mka" => fx.ref_skip = 1105,
            "aac_fast.m4a" => fx.master_len = true,
            "adts.aac" | "adts_nopns.aac" => fx.align = Some(1024),
            "noxing.mp3" => fx.align = Some(1105),
            "layer2.mp2" => fx.align = Some(481),
            "opus44.opus" => fx.align = None,
            _ => {}
        }
    }
    v
}

pub fn master_frames(fx: &Fixture) -> usize {
    match fx.master {
        "stereo44" => 145530,
        "stereo48" => 158400,
        "surround51" => 96000,
        _ => 55125,
    }
}
