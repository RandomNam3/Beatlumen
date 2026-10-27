//! ISO base media file format (MP4 / M4A / M4B / MOV) demuxer.
//!
//! Parses `moov/trak/mdia/minf/stbl` (stsd with `mp4a`+`esds` or `alac`,
//! stts, stsc, stsz/stz2, stco/co64) of the first audio track, works with
//! `moov` before or after `mdat`, reads `udta/meta/ilst` tags, and applies
//! gapless trimming from the edit list (`elst` media time / segment
//! duration) or iTunes `iTunSMPB`. Seeking is sample accurate: the sample
//! table locates the packet, one packet of pre-roll (AAC) is decoded and
//! discarded, then output is trimmed to the requested frame. Fragmented
//! MP4 (`moof`) is not supported.

use std::io::{BufReader, Read, Seek, SeekFrom};

use crate::alac::AlacDecoder;
use crate::tags::mp4::{find_box, parse_ilst, parse_keys, BoxIter, ITunSmpb};
use crate::util::{be16, be32, be64, bitrate_kbps, emit_window, invalid, mul_div, seek_buffered, read_up_to, read_vec, stream_len, unsupported};
use crate::{AudioSpec, Decoder, Error, MediaInfo, Picture, Source, Tags};

const MAX_MOOV: u64 = 256 << 20;

/// Codec of the selected audio track.
#[derive(Debug, Clone)]
enum Codec {
    Aac { asc: Vec<u8> },
    Alac { cookie: Vec<u8> },
    Mp3,
    Other(String),
}

#[derive(Debug, Clone, Copy)]
struct SttsRun {
    first_sample: u64,
    first_time: u64,
    count: u32,
    delta: u32,
}

/// Raw sample table boxes of a track.
#[derive(Default, Debug)]
struct RawTables {
    stts: Vec<(u32, u32)>,
    stsc: Vec<(u32, u32)>,
    /// Constant sample size (if non-zero) and sample count.
    const_size: u32,
    sizes: Vec<u32>,
    count: u32,
    chunk_offsets: Vec<u64>,
}

#[derive(Debug)]
struct AudioTrack {
    timescale: u32,
    media_duration: u64,
    codec: Codec,
    channels: u16,
    entry_rate: u32,
    /// First non-empty edit: (segment duration in movie timescale, media time).
    edit: Option<(u64, i64)>,
    avg_bitrate: Option<u32>,
    raw: RawTables,
}

struct Mp4File {
    track: AudioTrack,
    movie_timescale: u32,
    movie_duration: u64,
    tags: Tags,
    pictures: Vec<Picture>,
    smpb: Option<ITunSmpb>,
}

/// Locates the top-level `moov` box and reads it into memory.
fn read_moov<R: Read + Seek + ?Sized>(r: &mut R) -> Result<Vec<u8>, Error> {
    let len = stream_len(r)?;
    let mut pos = 0u64;
    let mut saw_moof = false;
    while pos + 8 <= len {
        r.seek(SeekFrom::Start(pos))?;
        let mut h = [0u8; 16];
        let n = read_up_to(r, &mut h)?;
        if n < 8 {
            break;
        }
        let size32 = u32::from_be_bytes(h[0..4].try_into().unwrap()) as u64;
        let typ: [u8; 4] = h[4..8].try_into().unwrap();
        let (hdr, size) = match size32 {
            0 => (8, len - pos),
            1 if n >= 16 => (16, u64::from_be_bytes(h[8..16].try_into().unwrap())),
            1 => return invalid("truncated box header"),
            s => (8, s),
        };
        if size < hdr {
            return invalid("bad box size");
        }
        if &typ == b"moov" {
            r.seek(SeekFrom::Start(pos + hdr))?;
            let body_len = (size - hdr).min(len - pos - hdr);
            return read_vec(r, body_len, MAX_MOOV);
        }
        if &typ == b"moof" {
            saw_moof = true;
        }
        pos = pos.saturating_add(size);
    }
    if saw_moof {
        return unsupported("fragmented MP4 without moov");
    }
    invalid("no moov box")
}

fn full_box_body(b: &[u8]) -> &[u8] {
    b.get(4..).unwrap_or(&[])
}

/// Reads an MPEG-4 descriptor length (up to 4 bytes, 7 bits each).
fn desc_len(d: &[u8], pos: &mut usize) -> Option<usize> {
    let mut len = 0usize;
    for _ in 0..4 {
        let b = *d.get(*pos)?;
        *pos += 1;
        len = (len << 7) | (b & 0x7f) as usize;
        if b & 0x80 == 0 {
            break;
        }
    }
    Some(len)
}

/// Parses `esds`: returns (objectTypeIndication, DecoderSpecificInfo, avg bitrate).
fn parse_esds(b: &[u8]) -> Option<(u8, Vec<u8>, u32)> {
    let d = full_box_body(b);
    let mut pos = 0usize;
    let mut oti = 0u8;
    let mut avg = 0u32;
    let mut dsi = Vec::new();
    while pos < d.len() {
        let tag = d[pos];
        pos += 1;
        let len = desc_len(d, &mut pos)?;
        let end = (pos + len).min(d.len());
        match tag {
            0x03 => {
                // ES_Descriptor: nested descriptors follow its header.
                pos += 2;
                let flags = *d.get(pos)?;
                pos += 1;
                if flags & 0x80 != 0 {
                    pos += 2;
                }
                if flags & 0x40 != 0 {
                    pos += 1 + *d.get(pos)? as usize;
                }
                if flags & 0x20 != 0 {
                    pos += 2;
                }
            }
            0x04 => {
                oti = *d.get(pos)?;
                avg = be32(d, pos + 9).unwrap_or(0);
                pos += 13;
            }
            0x05 => {
                dsi = d.get(pos..end)?.to_vec();
                pos = end;
            }
            _ => pos = end,
        }
    }
    Some((oti, dsi, avg))
}

/// Parsed AudioSpecificConfig fields.
#[derive(Debug, Clone, Copy)]
pub(crate) struct AscInfo {
    #[allow(dead_code)]
    pub object_type: u32,
    pub sample_rate: u32,
    pub sbr_rate: Option<u32>,
    pub channels: u16,
}

pub(crate) fn parse_asc(asc: &[u8]) -> Option<AscInfo> {
    let mut bits = 0u64;
    for i in 0..8 {
        bits = (bits << 8) | *asc.get(i).unwrap_or(&0) as u64;
    }
    if asc.len() < 2 {
        return None;
    }
    let mut pos = 0u32;
    let mut take = |n: u32| -> u32 {
        let v = ((bits << pos) >> (64 - n)) as u32;
        pos += n;
        v
    };
    let mut aot = take(5);
    if aot == 31 {
        aot = 32 + take(6);
    }
    let rate_of = |idx: u32, take: &mut dyn FnMut(u32) -> u32| -> Option<u32> {
        if idx == 15 {
            Some(take(24))
        } else {
            bl_aac::SAMPLE_RATES.get(idx as usize).copied()
        }
    };
    let sfi = take(5 - 1);
    let sample_rate = rate_of(sfi, &mut take)?;
    let chan = take(4);
    let mut sbr_rate = None;
    if aot == 5 || aot == 29 {
        let e = take(4);
        sbr_rate = rate_of(e, &mut take);
    }
    let channels = match chan {
        1..=6 => chan as u16,
        7 => 8,
        _ => 0,
    };
    Some(AscInfo { object_type: aot, sample_rate, sbr_rate, channels })
}

fn parse_sample_entry(stsd: &[u8]) -> Option<(Codec, u16, u32, Option<u32>)> {
    let d = full_box_body(stsd);
    let entries = BoxIter::new(d.get(4..)?);
    let (typ, e) = entries.into_iter().next()?;
    if e.len() < 28 {
        return None;
    }
    let version = be16(e, 8)?;
    let channels = be16(e, 16)?;
    let rate = be32(e, 24)? >> 16;
    let children_at = match version {
        1 => 28 + 16,
        2 => 28 + 36,
        _ => 28,
    };
    let children = e.get(children_at..).unwrap_or(&[]);
    let mut avg = None;
    let codec = match &typ {
        b"mp4a" => {
            let esds = find_box(children, b"esds").or_else(|| find_box(children, b"wave").and_then(|w| find_box(w, b"esds")));
            match esds.and_then(parse_esds) {
                Some((oti, dsi, br)) => {
                    avg = (br > 0).then_some(br);
                    match oti {
                        0x40 | 0x66 | 0x67 | 0x68 if !dsi.is_empty() => Codec::Aac { asc: dsi },
                        0x69 | 0x6b => Codec::Mp3,
                        _ => Codec::Other(format!("mp4a object type 0x{oti:02x}")),
                    }
                }
                None => Codec::Other("mp4a without esds".into()),
            }
        }
        b"alac" => {
            let cfg = find_box(children, b"alac").or_else(|| find_box(children, b"wave").and_then(|w| find_box(w, b"alac")));
            match cfg {
                Some(c) if c.len() >= 28 => Codec::Alac { cookie: c[4..].to_vec() },
                Some(c) if c.len() >= 24 => Codec::Alac { cookie: c.to_vec() },
                _ => Codec::Other("alac without config".into()),
            }
        }
        b".mp3" | b"mp3 " => Codec::Mp3,
        other => Codec::Other(String::from_utf8_lossy(other).into_owned()),
    };
    Some((codec, channels, rate, avg))
}

fn parse_stbl(stbl: &[u8]) -> RawTables {
    let mut t = RawTables::default();
    for (typ, b) in BoxIter::new(stbl) {
        let d = full_box_body(b);
        let count = be32(d, 0).unwrap_or(0) as usize;
        match &typ {
            b"stts" => {
                t.stts = (0..count).map_while(|i| Some((be32(d, 4 + i * 8)?, be32(d, 8 + i * 8)?))).collect();
            }
            b"stsc" => {
                t.stsc = (0..count).map_while(|i| Some((be32(d, 4 + i * 12)?, be32(d, 8 + i * 12)?))).collect();
            }
            b"stsz" => {
                t.const_size = be32(d, 0).unwrap_or(0);
                t.count = be32(d, 4).unwrap_or(0);
                if t.const_size == 0 {
                    t.sizes = (0..t.count as usize).map_while(|i| be32(d, 8 + i * 4)).collect();
                    t.count = t.sizes.len() as u32;
                }
            }
            b"stz2" => {
                let field = d.get(3).copied().unwrap_or(0);
                t.count = be32(d, 4).unwrap_or(0);
                let data = d.get(8..).unwrap_or(&[]);
                t.sizes = match field {
                    4 => (0..t.count as usize).map_while(|i| data.get(i / 2).map(|b| if i % 2 == 0 { (b >> 4) as u32 } else { (b & 15) as u32 })).collect(),
                    8 => (0..t.count as usize).map_while(|i| data.get(i).map(|&b| b as u32)).collect(),
                    16 => (0..t.count as usize).map_while(|i| be16(data, i * 2).map(|v| v as u32)).collect(),
                    _ => Vec::new(),
                };
                t.count = t.sizes.len() as u32;
            }
            b"stco" => t.chunk_offsets = (0..count).map_while(|i| be32(d, 4 + i * 4).map(|v| v as u64)).collect(),
            b"co64" => t.chunk_offsets = (0..count).map_while(|i| be64(d, 4 + i * 8)).collect(),
            _ => {}
        }
    }
    t
}

fn parse_trak(trak: &[u8]) -> Option<AudioTrack> {
    let mdia = find_box(trak, b"mdia")?;
    let hdlr = find_box(mdia, b"hdlr")?;
    if hdlr.get(8..12)? != b"soun" {
        return None;
    }
    let mdhd = find_box(mdia, b"mdhd")?;
    let (timescale, media_duration) = if mdhd.first() == Some(&1) {
        (be32(mdhd, 20)?, be64(mdhd, 24)?)
    } else {
        (be32(mdhd, 12)?, be32(mdhd, 16)? as u64)
    };
    let stbl = find_box(find_box(mdia, b"minf")?, b"stbl")?;
    let (codec, channels, entry_rate, avg_bitrate) = parse_sample_entry(find_box(stbl, b"stsd")?)?;
    let raw = parse_stbl(stbl);
    let mut edit = None;
    if let Some(elst) = find_box(trak, b"edts").and_then(|e| find_box(e, b"elst")) {
        let v1 = elst.first() == Some(&1);
        let d = full_box_body(elst);
        let n = be32(d, 0).unwrap_or(0) as usize;
        let es = if v1 { 20 } else { 12 };
        for i in 0..n.min(64) {
            let o = 4 + i * es;
            let (dur, mt) = if v1 {
                (be64(d, o), be64(d, o + 8).map(|v| v as i64))
            } else {
                (be32(d, o).map(|v| v as u64), be32(d, o + 4).map(|v| v as i32 as i64))
            };
            let (Some(dur), Some(mt)) = (dur, mt) else { break };
            if mt >= 0 {
                edit = Some((dur, mt));
                break;
            }
        }
    }
    if timescale == 0 {
        return None;
    }
    Some(AudioTrack { timescale, media_duration, codec, channels, entry_rate, edit, avg_bitrate, raw })
}

fn parse_file<R: Read + Seek + ?Sized>(r: &mut R, want_tags: bool, want_pictures: bool) -> Result<Mp4File, Error> {
    let moov = read_moov(r)?;
    let mut movie_timescale = 0u32;
    let mut movie_duration = 0u64;
    if let Some(mvhd) = find_box(&moov, b"mvhd") {
        if mvhd.first() == Some(&1) {
            movie_timescale = be32(mvhd, 20).unwrap_or(0);
            movie_duration = be64(mvhd, 24).unwrap_or(0);
        } else {
            movie_timescale = be32(mvhd, 12).unwrap_or(0);
            movie_duration = be32(mvhd, 16).unwrap_or(0) as u64;
        }
    }
    let track = BoxIter::new(&moov)
        .filter(|(t, _)| t == b"trak")
        .find_map(|(_, b)| parse_trak(b))
        .ok_or_else(|| Error::Unsupported("no audio track".into()))?;
    let mut tags = Tags::default();
    let mut pictures = Vec::new();
    let mut smpb = None;
    // Tags are also needed by the decoder for iTunSMPB gapless info.
    let metas = [
        find_box(&moov, b"udta").and_then(|u| find_box(u, b"meta")),
        find_box(&moov, b"meta"),
    ];
    for meta in metas.into_iter().flatten() {
        // `meta` is a full box in ISO files but not in QuickTime files.
        let body = if meta.get(4..8) == Some(b"hdlr") { meta } else { full_box_body(meta) };
        if let Some(ilst) = find_box(body, b"ilst") {
            let keys = find_box(body, b"keys").map(parse_keys).unwrap_or_default();
            let mut t = Tags::default();
            let pics = (want_tags && want_pictures).then_some(&mut pictures);
            let s = parse_ilst(ilst, &keys, &mut t, pics);
            smpb = smpb.or(s);
            if want_tags {
                crate::tags::merge_missing(&mut tags, &t);
            }
        }
    }
    Ok(Mp4File { track, movie_timescale, movie_duration, tags, pictures, smpb })
}

/// Gapless layout in media timescale units.
struct Gapless {
    priming: u64,
    duration: Option<u64>,
}

fn gapless(f: &Mp4File, out_rate: u32) -> Gapless {
    let t = &f.track;
    if let Some((seg, mt)) = t.edit {
        let duration = if f.movie_timescale > 0 && seg > 0 {
            Some(mul_div(seg, t.timescale as u64, f.movie_timescale as u64))
        } else {
            None
        };
        return Gapless { priming: mt as u64, duration };
    }
    if let (Some(s), true) = (f.smpb, out_rate > 0) {
        let scale = |v: u64| mul_div(v, t.timescale as u64, out_rate as u64);
        return Gapless { priming: scale(s.delay as u64), duration: (s.samples > 0).then(|| scale(s.samples)) };
    }
    Gapless { priming: 0, duration: None }
}

fn codec_name(c: &Codec) -> &'static str {
    match c {
        Codec::Aac { .. } => "aac",
        Codec::Alac { .. } => "alac",
        Codec::Mp3 => "mp3",
        Codec::Other(_) => "unknown",
    }
}

pub(crate) fn read_info<R: Read + Seek + ?Sized>(r: &mut R, with_pictures: bool) -> Result<MediaInfo, Error> {
    let f = parse_file(r, true, with_pictures)?;
    let t = &f.track;
    let (rate, channels) = match &t.codec {
        Codec::Aac { asc } => match parse_asc(asc) {
            Some(a) => {
                let rate = a.sbr_rate.unwrap_or(if t.timescale == a.sample_rate * 2 { t.timescale } else { a.sample_rate });
                (rate, if a.channels > 0 { a.channels } else { t.channels })
            }
            None => (t.entry_rate, t.channels),
        },
        Codec::Alac { cookie } => match crate::alac::AlacConfig::parse(cookie) {
            Ok(c) => (c.sample_rate, c.num_channels as u16),
            Err(_) => (t.entry_rate, t.channels),
        },
        _ => (t.entry_rate, t.channels),
    };
    let g = gapless(&f, rate);
    let media_total: u64 = t.raw.stts.iter().map(|&(c, d)| c as u64 * d as u64).sum();
    let dur_ts = g.duration.or_else(|| {
        let m = if t.media_duration > 0 { t.media_duration } else { media_total };
        (m > 0).then_some(m)
    });
    let duration_ms = dur_ts
        .map(|d| mul_div(d, 1000, t.timescale as u64))
        .or_else(|| (f.movie_timescale > 0).then(|| mul_div(f.movie_duration, 1000, f.movie_timescale as u64)));
    let bytes: u64 = if t.raw.const_size > 0 {
        t.raw.const_size as u64 * t.raw.count as u64
    } else {
        t.raw.sizes.iter().map(|&s| s as u64).sum()
    };
    let bitrate = match (t.avg_bitrate, duration_ms) {
        (Some(b), _) => Some(b / 1000),
        (None, Some(ms)) if ms > 0 => bitrate_kbps(bytes, ms, 1000),
        _ => None,
    };
    Ok(MediaInfo {
        container: "mp4",
        codec: codec_name(&t.codec),
        sample_rate: rate,
        channels,
        duration_ms,
        bitrate_kbps: bitrate,
        tags: f.tags,
        pictures: f.pictures,
    })
}

/// Flattened sample table.
struct SampleTable {
    offsets: Vec<u64>,
    sizes: Vec<u32>,
    runs: Vec<SttsRun>,
}

impl SampleTable {
    fn build(raw: &RawTables, file_len: u64) -> Result<SampleTable, Error> {
        let n = if raw.const_size > 0 { raw.count as usize } else { raw.sizes.len() };
        let size_of = |i: usize| if raw.const_size > 0 { raw.const_size } else { raw.sizes[i] };
        let mut offsets = Vec::new();
        let mut sizes = Vec::new();
        let mut stsc_i = 0usize;
        'chunks: for (ci, &off) in raw.chunk_offsets.iter().enumerate() {
            let chunk_no = ci as u32 + 1;
            while stsc_i + 1 < raw.stsc.len() && raw.stsc[stsc_i + 1].0 <= chunk_no {
                stsc_i += 1;
            }
            let spc = raw.stsc.get(stsc_i).map_or(0, |e| e.1);
            let mut o = off;
            for _ in 0..spc {
                let i = offsets.len();
                if i >= n {
                    break 'chunks;
                }
                let s = size_of(i);
                if o.saturating_add(s as u64) > file_len {
                    break 'chunks; // truncated file: stop at the last complete sample
                }
                offsets.push(o);
                sizes.push(s);
                o += s as u64;
            }
        }
        let mut runs = Vec::new();
        let (mut first_sample, mut first_time) = (0u64, 0u64);
        for &(count, delta) in &raw.stts {
            if count == 0 {
                continue;
            }
            runs.push(SttsRun { first_sample, first_time, count, delta });
            first_sample += count as u64;
            first_time += count as u64 * delta as u64;
        }
        if runs.is_empty() {
            return invalid("empty stts");
        }
        let total = first_sample.min(offsets.len() as u64) as usize;
        offsets.truncate(total);
        sizes.truncate(total);
        Ok(SampleTable { offsets, sizes, runs })
    }

    fn len(&self) -> usize {
        self.offsets.len()
    }

    fn run_of_sample(&self, i: u64) -> &SttsRun {
        let idx = self.runs.partition_point(|r| r.first_sample <= i).saturating_sub(1);
        &self.runs[idx]
    }

    fn time_of(&self, i: usize) -> u64 {
        let r = self.run_of_sample(i as u64);
        r.first_time + (i as u64 - r.first_sample) * r.delta as u64
    }

    fn duration_of(&self, i: usize) -> u64 {
        self.run_of_sample(i as u64).delta as u64
    }

    /// Index of the sample containing media time `t`.
    fn sample_at(&self, t: u64) -> usize {
        let ri = self.runs.partition_point(|r| r.first_time <= t).saturating_sub(1);
        let r = &self.runs[ri];
        let within = if r.delta == 0 { 0 } else { (t - r.first_time.min(t)) / r.delta as u64 };
        let i = r.first_sample + within.min(r.count as u64 - 1);
        (i as usize).min(self.len().saturating_sub(1))
    }
}

enum PacketDecoder {
    Aac(Box<bl_aac::AacDecoder>),
    Alac(AlacDecoder),
}

impl PacketDecoder {
    fn decode(&mut self, data: &[u8], out: &mut Vec<f32>) -> Result<usize, Error> {
        match self {
            PacketDecoder::Aac(d) => d.decode_frame(data, out).map_err(Error::from),
            PacketDecoder::Alac(d) => d.decode_packet(data, out),
        }
    }
    fn reset(&mut self) {
        match self {
            PacketDecoder::Aac(d) => d.reset(),
            PacketDecoder::Alac(_) => {}
        }
    }
}

struct Mp4Decoder {
    r: BufReader<Box<dyn Source>>,
    /// Current stream position of `r`.
    cur: u64,
    table: SampleTable,
    dec: PacketDecoder,
    codec: &'static str,
    spec: AudioSpec,
    timescale: u32,
    priming: u64,
    total: Option<u64>,
    next: usize,
    discard_before: u64,
    preroll: usize,
    buf: Vec<u8>,
    tmp: Vec<f32>,
}

pub(crate) fn open(src: Box<dyn Source>) -> Result<Box<dyn Decoder>, Error> {
    let mut r = BufReader::with_capacity(64 * 1024, src);
    let file_len = stream_len(&mut r)?;
    let f = parse_file(&mut r, false, false)?;
    let t = &f.track;
    let (dec, preroll) = match &t.codec {
        Codec::Aac { asc } => (PacketDecoder::Aac(Box::new(bl_aac::AacDecoder::new(asc)?)), 1),
        Codec::Alac { cookie } => (PacketDecoder::Alac(AlacDecoder::new(cookie)?), 0),
        Codec::Mp3 => return unsupported("MP3 in MP4"),
        Codec::Other(s) => return unsupported(format!("MP4 codec {s}")),
    };
    let spec = match &dec {
        PacketDecoder::Aac(d) => AudioSpec { sample_rate: d.sample_rate(), channels: d.channels() },
        PacketDecoder::Alac(d) => AudioSpec { sample_rate: d.sample_rate(), channels: d.channels() },
    };
    if spec.sample_rate == 0 || spec.channels == 0 {
        return invalid("decoder reported an empty stream format");
    }
    let table = SampleTable::build(&t.raw, file_len)?;
    let g = gapless(&f, spec.sample_rate);
    let to_frames = |ts: u64| mul_div(ts, spec.sample_rate as u64, t.timescale as u64);
    let media_end = if table.len() > 0 { table.time_of(table.len() - 1) + table.duration_of(table.len() - 1) } else { 0 };
    let available = to_frames(media_end.saturating_sub(g.priming));
    let total = Some(g.duration.map_or(available, |d| to_frames(d).min(available)));
    let cur = r.stream_position()?;
    Ok(Box::new(Mp4Decoder {
        r,
        cur,
        table,
        dec,
        codec: codec_name(&t.codec),
        spec,
        timescale: t.timescale,
        priming: g.priming,
        total,
        next: 0,
        discard_before: 0,
        preroll,
        buf: Vec::new(),
        tmp: Vec::new(),
    }))
}

impl Mp4Decoder {
    /// Output frame position of the first sample of packet `i`.
    fn position_of(&self, i: usize) -> i64 {
        let t = self.table.time_of(i) as i64 - self.priming as i64;
        let scaled = (t as i128 * self.spec.sample_rate as i128) / self.timescale as i128;
        scaled as i64
    }
}

impl Decoder for Mp4Decoder {
    fn spec(&self) -> AudioSpec {
        self.spec
    }
    fn total_frames(&self) -> Option<u64> {
        self.total
    }
    fn next_chunk(&mut self, out: &mut Vec<f32>) -> Result<bool, Error> {
        loop {
            if self.next >= self.table.len() {
                return Ok(false);
            }
            let i = self.next;
            self.next += 1;
            let pos = self.position_of(i);
            if let Some(t) = self.total {
                if pos >= t as i64 {
                    self.next = self.table.len();
                    return Ok(false);
                }
            }
            let (off, size) = (self.table.offsets[i], self.table.sizes[i] as usize);
            seek_buffered(&mut self.r, &mut self.cur, off)?;
            self.buf.resize(size, 0);
            let got = read_up_to(&mut self.r, &mut self.buf)?;
            self.cur += got as u64;
            if got < size {
                self.next = self.table.len();
                return invalid("truncated sample data");
            }
            self.tmp.clear();
            let frames = self.dec.decode(&self.buf, &mut self.tmp)?;
            let ch = self.spec.channels as usize;
            self.tmp.truncate(frames * ch);
            if emit_window(&self.tmp, ch, pos, self.discard_before, self.total, out) > 0 {
                return Ok(true);
            }
        }
    }
    fn seek(&mut self, frame: u64) -> Result<u64, Error> {
        let frame = self.total.map_or(frame, |t| frame.min(t));
        if self.table.len() == 0 {
            return Ok(0);
        }
        let target_ts = mul_div(frame, self.timescale as u64, self.spec.sample_rate as u64) + self.priming;
        let idx = self.table.sample_at(target_ts);
        self.next = idx.saturating_sub(self.preroll);
        self.dec.reset();
        self.discard_before = frame;
        Ok(frame)
    }
    fn codec_name(&self) -> &'static str {
        self.codec
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn asc() {
        // AAC-LC 44.1 kHz stereo
        let a = parse_asc(&[0x12, 0x10]).unwrap();
        assert_eq!((a.object_type, a.sample_rate, a.channels, a.sbr_rate), (2, 44100, 2, None));
        // HE-AAC explicit: AOT 5, 22050 core, stereo, ext 44100, AOT 2
        let a = parse_asc(&[0x2b, 0x92, 0x08, 0x00]).unwrap();
        assert_eq!((a.object_type, a.sample_rate, a.sbr_rate), (5, 22050, Some(44100)));
    }

    #[test]
    fn esds() {
        let mut d = vec![0, 0, 0, 0];
        d.extend_from_slice(&[0x03, 0x19, 0x00, 0x01, 0x00]);
        d.extend_from_slice(&[0x04, 0x11, 0x40, 0x15, 0, 0, 0, 0, 0x01, 0xf4, 0, 0, 0x01, 0xf4, 0x00]);
        d.extend_from_slice(&[0x05, 0x02, 0x12, 0x10]);
        d.extend_from_slice(&[0x06, 0x01, 0x02]);
        let (oti, dsi, avg) = parse_esds(&d).unwrap();
        assert_eq!(oti, 0x40);
        assert_eq!(dsi, vec![0x12, 0x10]);
        assert_eq!(avg, 0x1f400);
    }
}
