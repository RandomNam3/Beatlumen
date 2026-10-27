//! Matroska / WebM demuxer.
//!
//! EBML parsing of the Segment (SeekHead, Info, Tracks, Cues, Tags,
//! Attachments) and streaming of Clusters (SimpleBlock / BlockGroup with
//! Xiph, EBML and fixed-size lacing, DiscardPadding, header-stripping
//! content compression). Supported audio codecs: A_OPUS (CodecDelay /
//! pre-skip), A_VORBIS (Xiph-laced CodecPrivate), A_AAC (with or without
//! CodecPrivate), A_FLAC, A_ALAC and A_PCM/*. Seeking uses Cues (or a
//! cluster scan when there are none) plus codec pre-roll and decode-and-
//! discard; positions come from block timestamps, so accuracy is limited by
//! the TimecodeScale (exact for 48 kHz audio with the default 1 ms scale).

use std::collections::VecDeque;
use std::io::{self, BufReader, Read, Seek, SeekFrom};
use std::sync::{Arc, Mutex};

use crate::alac::AlacDecoder;
use crate::pcm::SampleFormat;
use crate::tags::{apply_common, make_picture, parse_number_pair, set_str, vorbis_comment::parse_vorbis_comment};
use crate::util::{bitrate_kbps, emit_window, invalid, mul_div, read_up_to, read_vec, stream_len, unsupported};
use crate::{AudioSpec, Decoder, Error, MediaInfo, Picture, Source, Tags};

const ID_EBML: u32 = 0x1A45_DFA3;
const ID_SEGMENT: u32 = 0x1853_8067;
const ID_SEEKHEAD: u32 = 0x114D_9B74;
const ID_SEEK: u32 = 0x4DBB;
const ID_SEEKID: u32 = 0x53AB;
const ID_SEEKPOS: u32 = 0x53AC;
const ID_INFO: u32 = 0x1549_A966;
const ID_TIMECODESCALE: u32 = 0x2A_D7B1;
const ID_DURATION: u32 = 0x4489;
const ID_TITLE: u32 = 0x7BA9;
const ID_TRACKS: u32 = 0x1654_AE6B;
const ID_TRACKENTRY: u32 = 0xAE;
const ID_TRACKNUMBER: u32 = 0xD7;
const ID_TRACKTYPE: u32 = 0x83;
const ID_CODECID: u32 = 0x86;
const ID_CODECPRIVATE: u32 = 0x63A2;
const ID_CODECDELAY: u32 = 0x56AA;
const ID_SEEKPREROLL: u32 = 0x56BB;
const ID_AUDIO: u32 = 0xE1;
const ID_SAMPLINGFREQ: u32 = 0xB5;
const ID_OUTSAMPLINGFREQ: u32 = 0x78B5;
const ID_CHANNELS: u32 = 0x9F;
const ID_BITDEPTH: u32 = 0x6264;
const ID_CONTENTENCODINGS: u32 = 0x6D80;
const ID_CONTENTENCODING: u32 = 0x6240;
const ID_CONTENTCOMPRESSION: u32 = 0x5034;
const ID_CONTENTCOMPALGO: u32 = 0x4254;
const ID_CONTENTCOMPSETTINGS: u32 = 0x4255;
const ID_CLUSTER: u32 = 0x1F43_B675;
const ID_TIMECODE: u32 = 0xE7;
const ID_SIMPLEBLOCK: u32 = 0xA3;
const ID_BLOCKGROUP: u32 = 0xA0;
const ID_BLOCK: u32 = 0xA1;
const ID_DISCARDPADDING: u32 = 0x75A2;
const ID_CUES: u32 = 0x1C53_BB6B;
const ID_CUEPOINT: u32 = 0xBB;
const ID_CUETIME: u32 = 0xB3;
const ID_CUETRACKPOS: u32 = 0xB7;
const ID_CUETRACK: u32 = 0xF7;
const ID_CUECLUSTERPOS: u32 = 0xF1;
const ID_TAGS: u32 = 0x1254_C367;
const ID_TAG: u32 = 0x7373;
const ID_TARGETS: u32 = 0x63C0;
const ID_TARGETTYPEVALUE: u32 = 0x68CA;
const ID_SIMPLETAG: u32 = 0x67C8;
const ID_TAGNAME: u32 = 0x45A3;
const ID_TAGSTRING: u32 = 0x4487;
const ID_ATTACHMENTS: u32 = 0x1941_A469;
const ID_ATTACHEDFILE: u32 = 0x61A7;
const ID_FILENAME: u32 = 0x466E;
const ID_FILEMIME: u32 = 0x4660;
const ID_FILEDATA: u32 = 0x465C;
const ID_FILEDESC: u32 = 0x467E;

/// Top-level (Segment child) element IDs; seeing one ends an
/// unknown-sized cluster.
const LEVEL1: [u32; 8] = [ID_SEEKHEAD, ID_INFO, ID_TRACKS, ID_CLUSTER, ID_CUES, ID_TAGS, ID_ATTACHMENTS, 0x1043_A770];

const MAX_ELEMENT: u64 = 64 << 20;

/// Element header read from a stream.
#[derive(Debug, Clone, Copy)]
struct Header {
    id: u32,
    /// `None` for unknown-sized elements.
    size: Option<u64>,
    pos: u64,
    data: u64,
}

fn read_byte<R: Read + ?Sized>(r: &mut R) -> Result<Option<u8>, Error> {
    let mut b = [0u8; 1];
    Ok(if read_up_to(r, &mut b)? == 1 { Some(b[0]) } else { None })
}

/// Reads an element header at the current position; `Ok(None)` at EOF.
fn read_header<R: Read + Seek + ?Sized>(r: &mut R) -> Result<Option<Header>, Error> {
    let pos = r.stream_position()?;
    let Some(b0) = read_byte(r)? else { return Ok(None) };
    let len = b0.leading_zeros() as usize + 1;
    if len > 4 {
        return invalid("bad EBML element ID");
    }
    let mut id = b0 as u32;
    for _ in 1..len {
        let Some(b) = read_byte(r)? else { return Ok(None) };
        id = (id << 8) | b as u32;
    }
    let Some(s0) = read_byte(r)? else { return Ok(None) };
    let slen = s0.leading_zeros() as usize + 1;
    if slen > 8 {
        return invalid("bad EBML size");
    }
    let mut size = (s0 as u64) & (0xff >> slen);
    let mut all_ones = size == (0xff >> slen) as u64;
    for _ in 1..slen {
        let Some(b) = read_byte(r)? else { return Ok(None) };
        size = (size << 8) | b as u64;
        all_ones &= b == 0xff;
    }
    let data = pos + len as u64 + slen as u64;
    Ok(Some(Header { id, size: (!all_ones).then_some(size), pos, data }))
}

/// Iterates over child elements of an in-memory master element.
struct Children<'a> {
    d: &'a [u8],
    pos: usize,
}

fn children(d: &[u8]) -> Children<'_> {
    Children { d, pos: 0 }
}

/// Reads a variable length integer from a slice (marker removed).
fn vint(d: &[u8], pos: &mut usize) -> Option<(u64, usize)> {
    let b0 = *d.get(*pos)?;
    let len = b0.leading_zeros() as usize + 1;
    if len > 8 {
        return None;
    }
    let mut v = (b0 as u64) & (0xff >> len) as u64;
    for i in 1..len {
        v = (v << 8) | *d.get(*pos + i)? as u64;
    }
    *pos += len;
    Some((v, len))
}

impl<'a> Iterator for Children<'a> {
    type Item = (u32, &'a [u8]);
    fn next(&mut self) -> Option<Self::Item> {
        let d = self.d;
        let b0 = *d.get(self.pos)?;
        let ilen = b0.leading_zeros() as usize + 1;
        if ilen > 4 || self.pos + ilen > d.len() {
            self.pos = d.len();
            return None;
        }
        let id = d[self.pos..self.pos + ilen].iter().fold(0u32, |a, &b| (a << 8) | b as u32);
        let mut p = self.pos + ilen;
        let (size, slen) = vint(d, &mut p)?;
        let unknown = size == (1u64 << (7 * slen)) - 1;
        let end = if unknown { d.len() } else { p.checked_add(size as usize).filter(|&e| e <= d.len())? };
        self.pos = end;
        Some((id, &d[p..end]))
    }
}

fn uint(d: &[u8]) -> u64 {
    d.iter().take(8).fold(0u64, |a, &b| (a << 8) | b as u64)
}

fn sint(d: &[u8]) -> i64 {
    if d.is_empty() || d.len() > 8 {
        return 0;
    }
    let v = uint(d);
    let shift = 64 - 8 * d.len() as u32;
    ((v << shift) as i64) >> shift
}

fn float(d: &[u8]) -> f64 {
    match d.len() {
        4 => f32::from_be_bytes(d.try_into().unwrap()) as f64,
        8 => f64::from_be_bytes(d.try_into().unwrap()),
        _ => 0.0,
    }
}

fn string(d: &[u8]) -> String {
    let end = d.iter().position(|&b| b == 0).unwrap_or(d.len());
    String::from_utf8_lossy(&d[..end]).into_owned()
}

#[derive(Debug, Clone, Default)]
struct Track {
    number: u64,
    kind: u64,
    codec_id: String,
    private: Vec<u8>,
    codec_delay_ns: u64,
    seek_preroll_ns: u64,
    rate: f64,
    out_rate: f64,
    channels: u16,
    bit_depth: u32,
    /// Header-stripping compression: bytes to prepend to every frame.
    strip: Vec<u8>,
    unsupported_encoding: bool,
}

fn parse_track(d: &[u8]) -> Track {
    let mut t = Track { channels: 1, rate: 8000.0, ..Default::default() };
    for (id, b) in children(d) {
        match id {
            ID_TRACKNUMBER => t.number = uint(b),
            ID_TRACKTYPE => t.kind = uint(b),
            ID_CODECID => t.codec_id = string(b),
            ID_CODECPRIVATE => t.private = b.to_vec(),
            ID_CODECDELAY => t.codec_delay_ns = uint(b),
            ID_SEEKPREROLL => t.seek_preroll_ns = uint(b),
            ID_AUDIO => {
                for (id, b) in children(b) {
                    match id {
                        ID_SAMPLINGFREQ => t.rate = float(b),
                        ID_OUTSAMPLINGFREQ => t.out_rate = float(b),
                        ID_CHANNELS => t.channels = uint(b).min(255) as u16,
                        ID_BITDEPTH => t.bit_depth = uint(b) as u32,
                        _ => {}
                    }
                }
            }
            ID_CONTENTENCODINGS => {
                for (_, enc) in children(b).filter(|(id, _)| *id == ID_CONTENTENCODING) {
                    for (id, c) in children(enc) {
                        if id == ID_CONTENTCOMPRESSION {
                            let mut algo = 0;
                            let mut settings = Vec::new();
                            for (id, v) in children(c) {
                                match id {
                                    ID_CONTENTCOMPALGO => algo = uint(v),
                                    ID_CONTENTCOMPSETTINGS => settings = v.to_vec(),
                                    _ => {}
                                }
                            }
                            if algo == 3 {
                                t.strip = settings;
                            } else {
                                t.unsupported_encoding = true;
                            }
                        } else if id == 0x5035 {
                            t.unsupported_encoding = true; // encryption
                        }
                    }
                }
            }
            _ => {}
        }
    }
    t
}

fn codec_name(id: &str) -> &'static str {
    match id {
        "A_OPUS" => "opus",
        "A_VORBIS" => "vorbis",
        "A_FLAC" => "flac",
        "A_ALAC" => "alac",
        "A_MPEG/L3" => "mp3",
        "A_MPEG/L2" => "mp2",
        "A_MPEG/L1" => "mp1",
        s if s.starts_with("A_AAC") => "aac",
        s if s.starts_with("A_PCM/") => "pcm",
        _ => "unknown",
    }
}

/// Splits Xiph-laced codec private data (Vorbis headers).
fn xiph_split(d: &[u8]) -> Option<Vec<&[u8]>> {
    let count = *d.first()? as usize + 1;
    let mut pos = 1usize;
    let mut sizes = Vec::new();
    for _ in 0..count - 1 {
        let mut s = 0usize;
        loop {
            let b = *d.get(pos)?;
            pos += 1;
            s += b as usize;
            if b != 255 {
                break;
            }
        }
        sizes.push(s);
    }
    let mut out = Vec::new();
    for s in sizes {
        out.push(d.get(pos..pos + s)?);
        pos += s;
    }
    out.push(d.get(pos..)?);
    Some(out)
}

/// Segment level information.
struct Segment {
    seg_data: u64,
    seg_end: u64,
    first_cluster: Option<u64>,
    timecode_scale: u64,
    duration: Option<f64>,
    title: Option<String>,
    tracks: Vec<Track>,
    /// (CueTime in timecode units, absolute cluster position, track)
    cues: Vec<(u64, u64, u64)>,
    tags: Tags,
    pictures: Vec<Picture>,
}

fn parse_segment<R: Read + Seek + ?Sized>(r: &mut R, want_tags: bool, want_pictures: bool) -> Result<Segment, Error> {
    let file_len = stream_len(r)?;
    r.seek(SeekFrom::Start(0))?;
    let ebml = read_header(r)?.ok_or_else(|| Error::InvalidData("empty file".into()))?;
    if ebml.id != ID_EBML {
        return invalid("not an EBML file");
    }
    let mut pos = ebml.data + ebml.size.unwrap_or(0);
    // Find the Segment.
    let seg = loop {
        r.seek(SeekFrom::Start(pos))?;
        let Some(h) = read_header(r)? else { return invalid("no Segment") };
        if h.id == ID_SEGMENT {
            break h;
        }
        match h.size {
            Some(s) => pos = h.data + s,
            None => return invalid("no Segment"),
        }
    };
    let seg_data = seg.data;
    let seg_end = seg.size.map_or(file_len, |s| (seg_data + s).min(file_len));
    let mut s = Segment {
        seg_data,
        seg_end,
        first_cluster: None,
        timecode_scale: 1_000_000,
        duration: None,
        title: None,
        tracks: Vec::new(),
        cues: Vec::new(),
        tags: Tags::default(),
        pictures: Vec::new(),
    };
    let mut seek_map: Vec<(u32, u64)> = Vec::new();
    let mut seen: Vec<u32> = Vec::new();
    let mut pos = seg_data;
    while pos < seg_end {
        r.seek(SeekFrom::Start(pos))?;
        let Some(h) = read_header(r)? else { break };
        if h.id == ID_CLUSTER {
            s.first_cluster = Some(h.pos);
            break;
        }
        let Some(size) = h.size else { break };
        handle_level1(r, &h, size, &mut s, &mut seek_map, want_tags, want_pictures)?;
        seen.push(h.id);
        pos = h.data + size;
    }
    // Elements located after the clusters (typical for Cues and Tags).
    let mut wanted = vec![ID_INFO, ID_TRACKS, ID_CUES];
    if want_tags {
        wanted.push(ID_TAGS);
    }
    if want_pictures {
        wanted.push(ID_ATTACHMENTS);
    }
    for (id, at) in seek_map.clone() {
        if !wanted.contains(&id) || seen.contains(&id) || at >= seg_end {
            continue;
        }
        r.seek(SeekFrom::Start(at))?;
        if let Ok(Some(h)) = read_header(r) {
            if h.id == id {
                if let Some(size) = h.size {
                    let _ = handle_level1(r, &h, size, &mut s, &mut seek_map, want_tags, want_pictures);
                    seen.push(id);
                }
            }
        }
    }
    if s.tracks.is_empty() {
        return invalid("no Tracks element");
    }
    Ok(s)
}

fn handle_level1<R: Read + Seek + ?Sized>(
    r: &mut R,
    h: &Header,
    size: u64,
    s: &mut Segment,
    seek_map: &mut Vec<(u32, u64)>,
    want_tags: bool,
    want_pictures: bool,
) -> Result<(), Error> {
    let read = |r: &mut R| -> Result<Vec<u8>, Error> {
        r.seek(SeekFrom::Start(h.data))?;
        read_vec(r, size, MAX_ELEMENT)
    };
    match h.id {
        ID_SEEKHEAD => {
            let d = read(r)?;
            for (_, seek) in children(&d).filter(|(id, _)| *id == ID_SEEK) {
                let mut id = 0u32;
                let mut at = None;
                for (cid, v) in children(seek) {
                    match cid {
                        ID_SEEKID => id = uint(v) as u32,
                        ID_SEEKPOS => at = Some(uint(v)),
                        _ => {}
                    }
                }
                if let Some(at) = at {
                    seek_map.push((id, s.seg_data.saturating_add(at)));
                }
            }
        }
        ID_INFO => {
            let d = read(r)?;
            for (id, v) in children(&d) {
                match id {
                    ID_TIMECODESCALE => s.timecode_scale = uint(v).max(1),
                    ID_DURATION => s.duration = Some(float(v)).filter(|d| d.is_finite() && *d >= 0.0),
                    ID_TITLE => s.title = Some(string(v)),
                    _ => {}
                }
            }
        }
        ID_TRACKS => {
            let d = read(r)?;
            s.tracks = children(&d).filter(|(id, _)| *id == ID_TRACKENTRY).map(|(_, b)| parse_track(b)).collect();
        }
        ID_CUES => {
            let d = read(r)?;
            for (_, cp) in children(&d).filter(|(id, _)| *id == ID_CUEPOINT) {
                let mut time = 0;
                for (id, v) in children(cp) {
                    match id {
                        ID_CUETIME => time = uint(v),
                        ID_CUETRACKPOS => {
                            let (mut track, mut cpos) = (0, None);
                            for (id, v) in children(v) {
                                match id {
                                    ID_CUETRACK => track = uint(v),
                                    ID_CUECLUSTERPOS => cpos = Some(uint(v)),
                                    _ => {}
                                }
                            }
                            if let Some(p) = cpos {
                                s.cues.push((time, s.seg_data.saturating_add(p), track));
                            }
                        }
                        _ => {}
                    }
                }
            }
        }
        ID_TAGS if want_tags => {
            let d = read(r)?;
            parse_tags(&d, &mut s.tags);
        }
        ID_ATTACHMENTS if want_pictures => {
            let d = read(r)?;
            for (_, f) in children(&d).filter(|(id, _)| *id == ID_ATTACHEDFILE) {
                let (mut name, mut mime, mut desc, mut data) = (String::new(), String::new(), String::new(), None);
                for (id, v) in children(f) {
                    match id {
                        ID_FILENAME => name = string(v),
                        ID_FILEMIME => mime = string(v),
                        ID_FILEDESC => desc = string(v),
                        ID_FILEDATA => data = Some(v),
                        _ => {}
                    }
                }
                if let Some(data) = data {
                    if mime.starts_with("image/") {
                        let kind = if name.to_ascii_lowercase().starts_with("cover") { 3 } else { 0 };
                        let d = if desc.is_empty() { name } else { desc };
                        s.pictures.push(make_picture(&mime, kind, &d, data.to_vec()));
                    }
                }
            }
        }
        _ => {}
    }
    Ok(())
}

fn parse_tags(d: &[u8], tags: &mut Tags) {
    let mut album = Tags::default();
    for (_, tag) in children(d).filter(|(id, _)| *id == ID_TAG) {
        let mut level = 0u64;
        for (id, v) in children(tag) {
            if id == ID_TARGETS {
                for (id, v) in children(v) {
                    if id == ID_TARGETTYPEVALUE {
                        level = uint(v);
                    }
                }
            }
        }
        for (_, st) in children(tag).filter(|(id, _)| *id == ID_SIMPLETAG) {
            let (mut name, mut value) = (String::new(), None);
            for (id, v) in children(st) {
                match id {
                    ID_TAGNAME => name = string(v).to_ascii_uppercase(),
                    ID_TAGSTRING => value = Some(string(v)),
                    _ => {}
                }
            }
            let Some(value) = value else { continue };
            if level >= 50 {
                // Album level: TITLE is the album, ARTIST the album artist.
                match name.as_str() {
                    "TITLE" => set_str(&mut album.album, &value),
                    "ARTIST" => set_str(&mut album.album_artist, &value),
                    "TOTAL_PARTS" => album.track_total = parse_number_pair(&value).0,
                    "PART_NUMBER" => album.disc = parse_number_pair(&value).0,
                    _ => {
                        apply_common(&mut album, &name, &value);
                    }
                }
            } else {
                match name.as_str() {
                    "PART_NUMBER" => {
                        let (n, t) = parse_number_pair(&value);
                        tags.track = tags.track.or(n);
                        tags.track_total = tags.track_total.or(t);
                    }
                    "TOTAL_PARTS" => tags.track_total = tags.track_total.or(parse_number_pair(&value).0),
                    _ => {
                        apply_common(tags, &name, &value);
                    }
                }
            }
        }
    }
    crate::tags::merge_missing(tags, &album);
}

/// Picks the first audio track with a supported codec (or the first audio
/// track at all).
fn pick_track(s: &Segment) -> Option<&Track> {
    let audio = || s.tracks.iter().filter(|t| t.kind == 2 || (t.kind == 0 && t.codec_id.starts_with("A_")));
    audio().find(|t| codec_name(&t.codec_id) != "unknown").or_else(|| audio().next())
}

fn track_rate(t: &Track) -> u32 {
    match codec_name(&t.codec_id) {
        "opus" => 48000,
        _ => {
            let r = if t.out_rate > 0.0 { t.out_rate } else { t.rate };
            if r.is_finite() && (1.0..4.0e9).contains(&r) {
                r.round() as u32
            } else {
                0
            }
        }
    }
}

pub(crate) fn read_info<R: Read + Seek + ?Sized>(r: &mut R, with_pictures: bool) -> Result<MediaInfo, Error> {
    let file_len = stream_len(r)?;
    let mut s = parse_segment(r, true, with_pictures)?;
    let t = pick_track(&s).ok_or_else(|| Error::Unsupported("no audio track".into()))?.clone();
    if let Some(title) = s.title.take() {
        set_str(&mut s.tags.title, &title);
    }
    let codec = codec_name(&t.codec_id);
    let mut tags = s.tags;
    let mut pictures = s.pictures;
    if t.codec_id == "A_VORBIS" || (t.codec_id == "A_FLAC" && tags == Tags::default()) {
        read_private_comments(&t, &mut tags, with_pictures.then_some(&mut pictures));
    }
    let duration_ms = s.duration.map(|d| (d * s.timecode_scale as f64 / 1e6).round() as u64);
    let audio_bytes = s.seg_end.min(file_len).saturating_sub(s.first_cluster.unwrap_or(s.seg_data));
    let mut channels = t.channels;
    if codec == "opus" {
        if let Ok(h) = oc_opus::OpusHead::parse(&t.private) {
            channels = h.channels as u16;
        }
    }
    Ok(MediaInfo {
        container: "matroska",
        codec,
        sample_rate: track_rate(&t),
        channels,
        duration_ms,
        bitrate_kbps: duration_ms.filter(|&d| d > 0).and_then(|d| bitrate_kbps(audio_bytes, d, 1000)),
        tags,
        pictures,
    })
}

/// Vorbis comment headers embedded in CodecPrivate (Vorbis, FLAC).
fn read_private_comments(t: &Track, tags: &mut Tags, pictures: Option<&mut Vec<Picture>>) {
    if t.codec_id == "A_VORBIS" {
        if let Some(h) = xiph_split(&t.private) {
            if h.len() == 3 && h[1].len() > 7 && &h[1][..7] == b"\x03vorbis" {
                parse_vorbis_comment(&h[1][7..], tags, pictures);
            }
        }
    } else if t.codec_id == "A_FLAC" && t.private.starts_with(b"fLaC") {
        let d = &t.private;
        let mut pos = 4;
        while pos + 4 <= d.len() {
            let typ = d[pos] & 0x7f;
            let len = u32::from_be_bytes([0, d[pos + 1], d[pos + 2], d[pos + 3]]) as usize;
            let body = &d[pos + 4..(pos + 4 + len).min(d.len())];
            if typ == 4 {
                parse_vorbis_comment(body, tags, pictures);
                break;
            }
            if d[pos] & 0x80 != 0 {
                break;
            }
            pos += 4 + len;
        }
    }
}

/// A demuxed frame of the selected track.
struct MkvFrame {
    data: Vec<u8>,
    /// Timestamp in nanoseconds (first frame of a block only).
    ts: Option<i64>,
    /// Nanoseconds to drop from the end of this frame's output.
    discard_ns: i64,
}

/// Streams frames of one track from the clusters.
struct Demuxer {
    r: BufReader<Box<dyn Source>>,
    seg_end: u64,
    track: u64,
    strip: Vec<u8>,
    timecode_scale: u64,
    cluster_tc: u64,
    queue: VecDeque<MkvFrame>,
}

impl Demuxer {
    fn next_frame(&mut self) -> Result<Option<MkvFrame>, Error> {
        loop {
            if let Some(f) = self.queue.pop_front() {
                return Ok(Some(f));
            }
            let pos = self.r.stream_position()?;
            if pos >= self.seg_end {
                return Ok(None);
            }
            let Some(h) = read_header(&mut self.r)? else { return Ok(None) };
            match h.id {
                ID_CLUSTER => {
                    self.cluster_tc = 0;
                    continue; // descend into the cluster
                }
                ID_TIMECODE => {
                    let d = read_vec(&mut self.r, h.size.unwrap_or(0).min(8), 8)?;
                    self.cluster_tc = uint(&d);
                }
                ID_SIMPLEBLOCK | ID_BLOCKGROUP => {
                    let Some(size) = h.size else { return invalid("unknown-sized block") };
                    // Peek at the track number before reading large blocks.
                    let d = read_vec(&mut self.r, size, MAX_ELEMENT)?;
                    if h.id == ID_SIMPLEBLOCK {
                        self.parse_block(&d, 0)?;
                    } else {
                        let mut block = None;
                        let mut discard = 0i64;
                        for (id, v) in children(&d) {
                            match id {
                                ID_BLOCK => block = Some(v),
                                ID_DISCARDPADDING => discard = sint(v),
                                _ => {}
                            }
                        }
                        if let Some(b) = block {
                            self.parse_block(b, discard)?;
                        }
                    }
                }
                id => {
                    match h.size {
                        Some(size) if !LEVEL1.contains(&id) || id != ID_CLUSTER => {
                            self.r.seek(SeekFrom::Start(h.data.saturating_add(size)))?;
                        }
                        _ => return invalid("unknown-sized element inside cluster"),
                    }
                }
            }
        }
    }

    fn parse_block(&mut self, d: &[u8], discard_ns: i64) -> Result<(), Error> {
        let mut p = 0usize;
        let Some((track, _)) = vint(d, &mut p) else { return Ok(()) };
        if track != self.track || d.len() < p + 3 {
            return Ok(());
        }
        let rel = i16::from_be_bytes([d[p], d[p + 1]]) as i64;
        let flags = d[p + 2];
        p += 3;
        let ts = (self.cluster_tc as i64).saturating_add(rel).saturating_mul(self.timecode_scale as i64);
        let lacing = (flags >> 1) & 3;
        let mut sizes: Vec<usize> = Vec::new();
        if lacing != 0 {
            let Some(&n) = d.get(p) else { return invalid("truncated lace header") };
            p += 1;
            let count = n as usize + 1;
            match lacing {
                1 => {
                    for _ in 0..count - 1 {
                        let mut s = 0usize;
                        loop {
                            let Some(&b) = d.get(p) else { return invalid("truncated Xiph lacing") };
                            p += 1;
                            s += b as usize;
                            if b != 255 {
                                break;
                            }
                        }
                        sizes.push(s);
                    }
                }
                3 => {
                    let Some((first, _)) = vint(d, &mut p) else { return invalid("bad EBML lacing") };
                    let mut prev = first as i64;
                    sizes.push(first as usize);
                    for _ in 1..count - 1 {
                        let Some((raw, len)) = vint(d, &mut p) else { return invalid("bad EBML lacing") };
                        let bias = (1i64 << (7 * len - 1)) - 1;
                        prev += raw as i64 - bias;
                        if prev < 0 {
                            return invalid("negative lace size");
                        }
                        sizes.push(prev as usize);
                    }
                }
                _ => {
                    let rest = d.len() - p;
                    if rest % count > 0 {
                        return invalid("bad fixed lacing");
                    }
                    sizes = vec![rest / count; count - 1];
                }
            }
            let used: usize = sizes.iter().sum();
            if used > d.len() - p {
                return invalid("lace sizes exceed block");
            }
            sizes.push(d.len() - p - used);
        } else {
            sizes.push(d.len() - p);
        }
        let n = sizes.len();
        for (i, s) in sizes.into_iter().enumerate() {
            let mut data = Vec::with_capacity(self.strip.len() + s);
            data.extend_from_slice(&self.strip);
            data.extend_from_slice(&d[p..p + s]);
            p += s;
            self.queue.push_back(MkvFrame { data, ts: (i == 0).then_some(ts), discard_ns: if i + 1 == n { discard_ns } else { 0 } });
        }
        Ok(())
    }
}

enum PacketDecoder {
    Opus(oc_opus::OpusDecoder),
    Vorbis(Box<oc_vorbis::VorbisDecoder>),
    Aac(Box<oc_aac::AacDecoder>),
    Flac(oc_flac::FlacFrameDecoder),
    Alac(AlacDecoder),
    Pcm(SampleFormat),
}

impl PacketDecoder {
    fn decode(&mut self, p: &[u8], channels: usize, out: &mut Vec<f32>) -> Result<usize, Error> {
        match self {
            PacketDecoder::Opus(d) => d.decode_packet(p, out).map_err(Error::from),
            PacketDecoder::Vorbis(d) => d.decode_packet(p, out).map_err(Error::from),
            PacketDecoder::Aac(d) => d.decode_frame(p, out).map_err(Error::from),
            PacketDecoder::Flac(d) => d.decode(p, out).map_err(Error::from),
            PacketDecoder::Alac(d) => d.decode_packet(p, out),
            PacketDecoder::Pcm(f) => {
                let fb = f.bytes() * channels;
                let before = out.len();
                crate::pcm::convert(*f, channels, fb, p, out);
                Ok((out.len() - before) / channels.max(1))
            }
        }
    }
    fn reset(&mut self) {
        match self {
            PacketDecoder::Opus(d) => d.reset(),
            PacketDecoder::Vorbis(d) => d.reset(),
            PacketDecoder::Aac(d) => d.reset(),
            _ => {}
        }
    }
}

/// Builds an AudioSpecificConfig for A_AAC/* tracks without CodecPrivate.
fn synth_asc(t: &Track) -> Option<Vec<u8>> {
    let aot: u8 = match t.codec_id.rsplit('/').next()? {
        "MAIN" => 1,
        "LC" | "SBR" => 2,
        "SSR" => 3,
        "LTP" => 4,
        _ => 2,
    };
    let rate = t.rate.round() as u32;
    let sfi = oc_aac::SAMPLE_RATES.iter().position(|&r| r == rate)? as u8;
    let ch = t.channels.min(7) as u8;
    Some(vec![(aot << 3) | (sfi >> 1), ((sfi & 1) << 7) | (ch << 3)])
}

struct MkvDecoder {
    dmx: Demuxer,
    dec: PacketDecoder,
    codec: &'static str,
    spec: AudioSpec,
    /// Decoder delay in output frames (Opus pre-skip / CodecDelay).
    delay: i64,
    preroll_ns: u64,
    total: Option<u64>,
    first_cluster: u64,
    cues: Vec<(u64, u64)>,
    clusters_scanned: bool,
    pos: Option<i64>,
    pending: Vec<f32>,
    pending_frames: i64,
    /// Constraints on the position of `pending[0]` from block timestamps.
    cons: Interval,
    /// `timestamp_samples - output_position` of a block's first output
    /// frame, calibrated from the start of the stream.
    ts_offset: Option<i64>,
    /// Calibration constraints on `ts_offset` (while decoding linearly
    /// from the start).
    calib: Interval,
    /// Packet grid observed during calibration: (position of the first
    /// output, GCD of packet durations). Block positions after a seek are
    /// snapped to it, which removes the timestamp rounding error.
    grid_origin: Option<i64>,
    grid_gcd: i64,
    discard_before: u64,
    tmp: Vec<f32>,
    ready: Vec<f32>,
}

/// Intersection of integer intervals derived from rounded timestamps.
#[derive(Clone, Copy, Debug)]
struct Interval {
    lo: i64,
    hi: i64,
    count: u32,
    /// Estimate from the most recent constraint (interval midpoint).
    last: i64,
}

impl Interval {
    const EMPTY: Interval = Interval { lo: i64::MIN, hi: i64::MAX, count: 0, last: 0 };

    fn add(&mut self, lo: i64, hi: i64) {
        self.lo = self.lo.max(lo);
        self.hi = self.hi.min(hi);
        self.count += 1;
        self.last = lo + (hi - lo) / 2;
    }

    fn resolved(&self, max_count: u32) -> bool {
        self.count > 0 && (self.hi <= self.lo || self.count >= max_count)
    }

    /// Best value: the latest estimate clamped into the intersection (the
    /// estimate alone if the constraints are inconsistent).
    fn value(&self) -> i64 {
        if self.lo <= self.hi {
            self.last.clamp(self.lo, self.hi)
        } else {
            self.last
        }
    }
}

pub(crate) fn open(src: Box<dyn Source>) -> Result<Box<dyn Decoder>, Error> {
    let mut r = BufReader::with_capacity(64 * 1024, src);
    let s = parse_segment(&mut r, false, false)?;
    let t = pick_track(&s).ok_or_else(|| Error::Unsupported("no audio track".into()))?.clone();
    if t.unsupported_encoding {
        return unsupported("compressed or encrypted Matroska track");
    }
    let rate = track_rate(&t);
    let ns_to_frames = |ns: u64| ((ns as u128 * rate as u128 + 500_000_000) / 1_000_000_000) as i64;
    let mut delay = ns_to_frames(t.codec_delay_ns);
    let mut preroll_ns = t.seek_preroll_ns.max(100_000_000);
    if matches!(t.codec_id.as_str(), "A_MPEG/L3" | "A_MPEG/L2" | "A_MPEG/L1") {
        return open_mpeg(r, &s, &t, delay);
    }
    let dec = match t.codec_id.as_str() {
        "A_OPUS" => {
            let head = oc_opus::OpusHead::parse(&t.private)?;
            if t.codec_delay_ns == 0 {
                delay = head.pre_skip as i64;
            }
            preroll_ns = t.seek_preroll_ns.max(80_000_000) + 120_000_000;
            PacketDecoder::Opus(oc_opus::OpusDecoder::new(&head, 48000)?)
        }
        "A_VORBIS" => {
            let h = xiph_split(&t.private).filter(|h| h.len() == 3).ok_or_else(|| Error::InvalidData("bad Vorbis CodecPrivate".into()))?;
            preroll_ns = preroll_ns.max(250_000_000);
            PacketDecoder::Vorbis(Box::new(oc_vorbis::VorbisDecoder::new(h[0], h[1], h[2])?))
        }
        "A_FLAC" => PacketDecoder::Flac(oc_flac::FlacFrameDecoder::new(&t.private)?),
        "A_ALAC" => PacketDecoder::Alac(AlacDecoder::new(&t.private)?),
        id if id.starts_with("A_AAC") => {
            let asc = if t.private.len() >= 2 { t.private.clone() } else { synth_asc(&t).ok_or_else(|| Error::InvalidData("cannot build AAC config".into()))? };
            PacketDecoder::Aac(Box::new(oc_aac::AacDecoder::new(&asc)?))
        }
        "A_PCM/INT/LIT" | "A_PCM/INT/BIG" | "A_PCM/FLOAT/IEEE" => {
            let bytes = (t.bit_depth as usize).div_ceil(8);
            let f = match (t.codec_id.as_str(), bytes) {
                ("A_PCM/FLOAT/IEEE", 4) => Some(SampleFormat::F32Le),
                ("A_PCM/FLOAT/IEEE", 8) => Some(SampleFormat::F64Le),
                ("A_PCM/INT/LIT", b) => SampleFormat::int(b, true, true),
                ("A_PCM/INT/BIG", b) => SampleFormat::int(b, false, false),
                _ => None,
            };
            PacketDecoder::Pcm(f.ok_or_else(|| Error::Unsupported("PCM sample format".into()))?)
        }
        other => return unsupported(format!("Matroska codec {other}")),
    };
    let spec = match &dec {
        PacketDecoder::Opus(d) => AudioSpec { sample_rate: d.sample_rate(), channels: d.channels() },
        PacketDecoder::Vorbis(d) => AudioSpec { sample_rate: d.sample_rate(), channels: d.channels() },
        PacketDecoder::Aac(d) => AudioSpec { sample_rate: d.sample_rate(), channels: d.channels() },
        PacketDecoder::Flac(d) => AudioSpec { sample_rate: d.sample_rate(), channels: d.channels() },
        PacketDecoder::Alac(d) => AudioSpec { sample_rate: d.sample_rate(), channels: d.channels() },
        PacketDecoder::Pcm(_) => AudioSpec { sample_rate: rate, channels: t.channels },
    };
    if spec.sample_rate == 0 || spec.channels == 0 {
        return invalid("decoder reported an empty stream format");
    }
    let total = s.duration.map(|d| {
        let ns = d * s.timecode_scale as f64;
        (ns * spec.sample_rate as f64 / 1e9).round().max(0.0) as u64
    });
    let first_cluster = s.first_cluster.unwrap_or(s.seg_end);
    let mut cues: Vec<(u64, u64)> = s.cues.iter().filter(|c| c.2 == t.number || c.2 == 0).map(|c| (c.0, c.1)).collect();
    if cues.is_empty() {
        cues = s.cues.iter().map(|c| (c.0, c.1)).collect();
    }
    cues.sort_unstable();
    let clusters_scanned = !cues.is_empty();
    r.seek(SeekFrom::Start(first_cluster))?;
    let dmx = Demuxer { r, seg_end: s.seg_end, track: t.number, strip: t.strip.clone(), timecode_scale: s.timecode_scale, cluster_tc: 0, queue: VecDeque::new() };
    Ok(Box::new(MkvDecoder {
        dmx,
        dec,
        codec: codec_name(&t.codec_id),
        spec,
        delay,
        preroll_ns,
        total,
        first_cluster,
        cues,
        clusters_scanned,
        pos: Some(-delay),
        pending: Vec::new(),
        pending_frames: 0,
        cons: Interval::EMPTY,
        ts_offset: None,
        calib: Interval::EMPTY,
        grid_origin: None,
        grid_gcd: 0,
        discard_before: 0,
        tmp: Vec::new(),
        ready: Vec::new(),
    }))
}

impl MkvDecoder {
    fn ts_range(&self, ts_ns: i64) -> (i64, i64) {
        ts_range(ts_ns, self.spec.sample_rate, self.dmx.timecode_scale)
    }

    fn emit(&mut self, pending: bool, at: i64) {
        let ch = self.spec.channels as usize;
        let src = if pending { &self.pending } else { &self.tmp };
        emit_window(src, ch, at, self.discard_before, None, &mut self.ready);
    }

    fn resolve_pending(&mut self) {
        let start = if self.cons.count > 0 {
            let half_tick = ts_range(0, self.spec.sample_rate, self.dmx.timecode_scale).1;
            snap_to_grid(self.cons.value(), &self.cons, self.grid_origin, self.grid_gcd, half_tick)
        } else {
            self.discard_before as i64
        };
        self.emit(true, start);
        self.pos = Some(start + self.pending_frames);
        self.pending.clear();
        self.pending_frames = 0;
        self.cons = Interval::EMPTY;
    }

    /// Decodes one frame. Returns false at the end of the stream.
    fn step(&mut self) -> Result<bool, Error> {
        let Some(f) = self.dmx.next_frame()? else {
            if self.pos.is_none() && !self.pending.is_empty() {
                self.resolve_pending();
            }
            return Ok(false);
        };
        let ch = self.spec.channels as usize;
        self.tmp.clear();
        let mut n = self.dec.decode(&f.data, ch, &mut self.tmp)?;
        self.tmp.truncate(n * ch);
        if f.discard_ns > 0 {
            let cut = mul_div(f.discard_ns as u64, self.spec.sample_rate as u64, 1_000_000_000) as usize;
            n = n.saturating_sub(cut);
            self.tmp.truncate(n * ch);
        }
        match self.pos {
            Some(p) => {
                if let (true, None) = (n > 0, self.ts_offset) {
                    self.grid_origin.get_or_insert(p);
                    self.grid_gcd = gcd(self.grid_gcd, n as i64);
                    if let Some(ts) = f.ts {
                        let (lo, hi) = self.ts_range(ts);
                        self.calib.add(lo - p, hi - p);
                        if self.calib.resolved(16) {
                            self.ts_offset = Some(self.calib.value());
                        }
                    }
                }
                self.emit(false, p);
                self.pos = Some(p + n as i64);
            }
            None => {
                if let (Some(ts), true) = (f.ts, n > 0) {
                    let off = self.ts_offset.unwrap_or(self.delay) + self.pending_frames;
                    let (lo, hi) = self.ts_range(ts);
                    self.cons.add(lo - off, hi - off);
                }
                self.pending.extend_from_slice(&self.tmp);
                self.pending_frames += n as i64;
                if self.cons.resolved(16) || self.pending_frames > 2 * self.spec.sample_rate as i64 {
                    self.resolve_pending();
                }
            }
        }
        Ok(true)
    }

    /// Restarts decoding at a cluster position.
    fn restart_at(&mut self, cluster_pos: u64, from_start: bool) -> Result<(), Error> {
        self.dmx.r.seek(SeekFrom::Start(cluster_pos))?;
        self.dmx.queue.clear();
        self.dmx.cluster_tc = 0;
        self.dec.reset();
        self.pending.clear();
        self.pending_frames = 0;
        self.cons = Interval::EMPTY;
        self.ready.clear();
        self.pos = if from_start { Some(-self.delay) } else { None };
        Ok(())
    }

    /// Measures the timestamp offset by decoding the first packets.
    fn calibrate(&mut self) -> Result<(), Error> {
        self.restart_at(self.first_cluster, true)?;
        self.discard_before = u64::MAX;
        let mut guard = 0;
        while self.ts_offset.is_none() && guard < 64 && self.step()? {
            guard += 1;
        }
        if self.ts_offset.is_none() {
            self.ts_offset = Some(if self.calib.count > 0 { self.calib.value() } else { self.delay });
        }
        Ok(())
    }

    /// Builds a cluster index (time, position) by scanning cluster headers.
    fn scan_clusters(&mut self) -> Result<(), Error> {
        self.clusters_scanned = true;
        self.cues = scan_cluster_index(&mut self.dmx.r, self.first_cluster, self.dmx.seg_end)?;
        Ok(())
    }
}

/// Builds a cluster index `(timecode, position)` by walking cluster headers.
fn scan_cluster_index<R: Read + Seek + ?Sized>(r: &mut R, first_cluster: u64, seg_end: u64) -> Result<Vec<(u64, u64)>, Error> {
    let mut pos = first_cluster;
    let mut index = Vec::new();
    while pos < seg_end {
        r.seek(SeekFrom::Start(pos))?;
        let Some(h) = read_header(r)? else { break };
        let Some(size) = h.size else { break };
        if h.id == ID_CLUSTER {
            if let Ok(Some(c)) = read_header(r) {
                if c.id == ID_TIMECODE {
                    let d = read_vec(r, c.size.unwrap_or(0).min(8), 8)?;
                    index.push((uint(&d), h.pos));
                }
            }
        }
        pos = h.data.saturating_add(size);
    }
    Ok(index)
}

/// Sample positions compatible with a timestamp rounded to `tick` ns:
/// `[lo, hi]`.
fn ts_range(ts_ns: i64, rate: u32, tick: u64) -> (i64, i64) {
    let rate = rate as i128;
    let tick = tick as i128;
    let lo_num = (2 * ts_ns as i128 - tick) * rate;
    let hi_num = (2 * ts_ns as i128 + tick) * rate;
    let den = 2_000_000_000i128;
    let lo = lo_num.div_euclid(den) + if lo_num.rem_euclid(den) != 0 { 1 } else { 0 };
    let hi = hi_num.div_euclid(den);
    (lo as i64, hi.max(lo) as i64)
}

fn gcd(a: i64, b: i64) -> i64 {
    let (mut a, mut b) = (a.abs(), b.abs());
    while b != 0 {
        (a, b) = (b, a % b);
    }
    a
}

/// Snaps a timestamp-derived position estimate to the packet grid
/// `origin + k * g` when the grid is coarse enough and the snapped value is
/// compatible with the timestamp constraints.
fn snap_to_grid(est: i64, iv: &Interval, origin: Option<i64>, g: i64, half_tick: i64) -> i64 {
    let Some(o) = origin else { return est };
    if g < 32 {
        return est;
    }
    let c = o + (est - o + g / 2).div_euclid(g) * g;
    if iv.lo <= iv.hi && (c < iv.lo - half_tick || c > iv.hi + half_tick) {
        return est;
    }
    c
}

/// Picks the cluster to restart from for a seek to `target_ns` (minus
/// pre-roll). Returns (position, is_first_cluster).
fn pick_cluster(cues: &[(u64, u64)], first_cluster: u64, start_ns: u64, tick: u64) -> (u64, bool) {
    let start_tc = start_ns / tick.max(1);
    match cues.partition_point(|c| c.0 <= start_tc) {
        0 => (first_cluster, true),
        i => (cues[i - 1].1, cues[i - 1].1 <= first_cluster),
    }
}

impl Decoder for MkvDecoder {
    fn spec(&self) -> AudioSpec {
        self.spec
    }
    fn total_frames(&self) -> Option<u64> {
        self.total
    }
    fn next_chunk(&mut self, out: &mut Vec<f32>) -> Result<bool, Error> {
        loop {
            if !self.ready.is_empty() {
                out.append(&mut self.ready);
                return Ok(true);
            }
            if !self.step()? {
                if !self.ready.is_empty() {
                    continue;
                }
                return Ok(false);
            }
        }
    }
    fn seek(&mut self, frame: u64) -> Result<u64, Error> {
        if !self.clusters_scanned {
            self.scan_clusters()?;
        }
        if self.ts_offset.is_none() {
            self.calibrate()?;
        }
        let off = self.ts_offset.unwrap_or(self.delay);
        let target_ns = mul_div((frame as i64 + off).max(0) as u64, 1_000_000_000, self.spec.sample_rate as u64);
        let start_ns = target_ns.saturating_sub(self.preroll_ns);
        let (cluster_pos, from_start) = pick_cluster(&self.cues, self.first_cluster, start_ns, self.dmx.timecode_scale);
        self.restart_at(cluster_pos, from_start)?;
        self.discard_before = frame;
        // Establish the position now so the actual frame is known.
        let mut guard = 0;
        while self.pos.is_none() && guard < 100_000 && self.step()? {
            guard += 1;
        }
        Ok(frame)
    }
    fn codec_name(&self) -> &'static str {
        self.codec
    }
}

/// What the elementary stream learned about the frames pulled since the
/// last restart.
#[derive(Default)]
struct StreamNotes {
    /// `(frame index, timestamp ns)` of the first frames.
    stamps: Vec<(u64, i64)>,
    /// `(frame index, nanoseconds)` of a frame with DiscardPadding.
    discard: Option<(u64, i64)>,
}

type Stamps = Arc<Mutex<StreamNotes>>;

/// Presents the frames of an MPEG audio track as one contiguous byte
/// stream for the stream-level [`oc_mp3::Mp3Decoder`]. Only forward seeks
/// and seeks within the last 256 KiB are possible (`SeekFrom::End` fails,
/// which the MP3 decoder treats as a stream of unknown length).
struct ElementaryStream {
    dmx: Demuxer,
    buf: Vec<u8>,
    base: u64,
    pos: u64,
    eof: bool,
    frames: u64,
    stamps: Stamps,
}

impl ElementaryStream {
    fn restart(&mut self, cluster: u64) -> io::Result<()> {
        self.dmx.r.seek(SeekFrom::Start(cluster))?;
        self.dmx.queue.clear();
        self.dmx.cluster_tc = 0;
        self.buf.clear();
        self.base = 0;
        self.pos = 0;
        self.eof = false;
        self.frames = 0;
        *self.stamps.lock().unwrap_or_else(|e| e.into_inner()) = StreamNotes::default();
        Ok(())
    }

    fn pull(&mut self) -> io::Result<()> {
        match self.dmx.next_frame() {
            Ok(Some(f)) => {
                let mut st = self.stamps.lock().unwrap_or_else(|e| e.into_inner());
                if let Some(ts) = f.ts {
                    if st.stamps.len() < 64 {
                        st.stamps.push((self.frames, ts));
                    }
                }
                if f.discard_ns > 0 {
                    st.discard = Some((self.frames, f.discard_ns));
                }
                drop(st);
                self.frames += 1;
                self.buf.extend_from_slice(&f.data);
            }
            Ok(None) => self.eof = true,
            Err(Error::Io(e)) => return Err(e),
            Err(e) => {
                // Treat demuxing errors as the end of the stream.
                let _ = e;
                self.eof = true;
            }
        }
        Ok(())
    }
}

impl Read for ElementaryStream {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        while self.pos >= self.base + self.buf.len() as u64 && !self.eof {
            self.pull()?;
        }
        let end = self.base + self.buf.len() as u64;
        if self.pos >= end || out.is_empty() {
            return Ok(0);
        }
        let i = (self.pos - self.base) as usize;
        let n = out.len().min(self.buf.len() - i);
        out[..n].copy_from_slice(&self.buf[i..i + n]);
        self.pos += n as u64;
        const KEEP: usize = 256 * 1024;
        let consumed = (self.pos - self.base) as usize;
        if consumed > 2 * KEEP {
            let drop = consumed - KEEP;
            self.buf.drain(..drop);
            self.base += drop as u64;
        }
        Ok(n)
    }
}

impl Seek for ElementaryStream {
    fn seek(&mut self, to: SeekFrom) -> io::Result<u64> {
        let t = match to {
            SeekFrom::Start(n) => n as i128,
            SeekFrom::Current(k) => self.pos as i128 + k as i128,
            SeekFrom::End(_) => return Err(io::Error::new(io::ErrorKind::Unsupported, "stream length unknown")),
        };
        if t < self.base as i128 {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "seek before retained data"));
        }
        self.pos = t.min(u64::MAX as i128) as u64;
        Ok(self.pos)
    }
}

/// MPEG audio (A_MPEG/L1..L3) in Matroska, decoded by the stream-level MP3
/// decoder; positions after a seek come from block timestamps.
struct MkvMpegDecoder {
    dec: Option<oc_mp3::Mp3Decoder<ElementaryStream>>,
    stamps: Stamps,
    spec: AudioSpec,
    codec: &'static str,
    total: Option<u64>,
    delay: i64,
    spf: i64,
    tick: u64,
    first_cluster: u64,
    cues: Vec<(u64, u64)>,
    clusters_scanned: bool,
    /// `timestamp_samples - output position` of a frame start.
    ts_offset: i64,
    pos: i64,
    /// Output position of the first frame of the current byte stream.
    origin: i64,
    discard_before: u64,
    pending: Vec<f32>,
    tmp: Vec<f32>,
}

impl MkvMpegDecoder {
    /// End of output from a DiscardPadding element, once seen.
    fn end(&self) -> Option<u64> {
        let notes = self.stamps.lock().unwrap_or_else(|e| e.into_inner());
        notes.discard.map(|(k, ns)| {
            let cut = ((ns as i128 * self.spec.sample_rate as i128 + 500_000_000) / 1_000_000_000) as i64;
            (self.origin + (k as i64 + 1) * self.spf - cut).max(0) as u64
        })
    }
}

fn open_mpeg(mut r: BufReader<Box<dyn Source>>, s: &Segment, t: &Track, delay: i64) -> Result<Box<dyn Decoder>, Error> {
    let first_cluster = s.first_cluster.unwrap_or(s.seg_end);
    r.seek(SeekFrom::Start(first_cluster))?;
    let mut dmx = Demuxer { r, seg_end: s.seg_end, track: t.number, strip: t.strip.clone(), timecode_scale: s.timecode_scale, cluster_tc: 0, queue: VecDeque::new() };
    // Calibrate timestamps against the frame grid from the first frames.
    let mut header = None;
    let mut calib = Interval::EMPTY;
    let mut k = 0i64;
    while k < 32 {
        let Some(f) = dmx.next_frame()? else { break };
        let h = match header {
            Some(h) => h,
            None => match crate::mp3::parse_header(&f.data) {
                Some(h) => {
                    header = Some(h);
                    h
                }
                None => continue,
            },
        };
        if let Some(ts) = f.ts {
            let (lo, hi) = ts_range(ts, h.sample_rate, s.timecode_scale);
            let p = k * h.samples as i64 - delay;
            calib.add(lo - p, hi - p);
        }
        k += 1;
    }
    let h = header.ok_or_else(|| Error::InvalidData("no MPEG audio frame in track".into()))?;
    let ts_offset = if calib.count > 0 { calib.value() } else { delay };
    dmx.r.seek(SeekFrom::Start(first_cluster))?;
    dmx.queue.clear();
    let stamps: Stamps = Arc::new(Mutex::new(StreamNotes::default()));
    let stream = ElementaryStream { dmx, buf: Vec::new(), base: 0, pos: 0, eof: false, frames: 0, stamps: stamps.clone() };
    let dec = oc_mp3::Mp3Decoder::new(stream)?;
    let spec = AudioSpec { sample_rate: dec.sample_rate(), channels: dec.channels() };
    if spec.sample_rate == 0 || spec.channels == 0 {
        return invalid("decoder reported an empty stream format");
    }
    let total = s.duration.map(|d| ((d * s.timecode_scale as f64 * spec.sample_rate as f64 / 1e9).round().max(0.0) as u64).saturating_sub(delay.max(0) as u64));
    let mut cues: Vec<(u64, u64)> = s.cues.iter().map(|c| (c.0, c.1)).collect();
    cues.sort_unstable();
    Ok(Box::new(MkvMpegDecoder {
        dec: Some(dec),
        stamps,
        spec,
        codec: codec_name(&t.codec_id),
        total,
        delay,
        spf: h.samples as i64,
        tick: s.timecode_scale,
        first_cluster,
        clusters_scanned: !cues.is_empty(),
        cues,
        ts_offset,
        pos: -delay,
        origin: -delay,
        discard_before: 0,
        pending: Vec::new(),
        tmp: Vec::new(),
    }))
}

impl Decoder for MkvMpegDecoder {
    fn spec(&self) -> AudioSpec {
        self.spec
    }
    fn total_frames(&self) -> Option<u64> {
        self.total
    }
    fn next_chunk(&mut self, out: &mut Vec<f32>) -> Result<bool, Error> {
        let ch = self.spec.channels as usize;
        let end = self.end();
        if !self.pending.is_empty() {
            let n = self.pending.len() / ch;
            let start = self.pos - n as i64;
            let emitted = emit_window(&self.pending, ch, start, self.discard_before, end, out);
            self.pending.clear();
            if emitted > 0 {
                return Ok(true);
            }
        }
        loop {
            let Some(dec) = self.dec.as_mut() else { return Ok(false) };
            self.tmp.clear();
            let more = dec.next_chunk(&mut self.tmp)?;
            let n = self.tmp.len() / ch;
            let end = self.end();
            let emitted = emit_window(&self.tmp, ch, self.pos, self.discard_before, end, out);
            self.pos += n as i64;
            if emitted > 0 {
                return Ok(true);
            }
            if !more {
                return Ok(false);
            }
        }
    }
    fn seek(&mut self, frame: u64) -> Result<u64, Error> {
        let Some(dec) = self.dec.take() else { return invalid("decoder unavailable after an earlier error") };
        let mut stream = dec.into_inner();
        if !self.clusters_scanned {
            self.clusters_scanned = true;
            self.cues = scan_cluster_index(&mut stream.dmx.r, self.first_cluster, stream.dmx.seg_end)?;
        }
        let rate = self.spec.sample_rate as u64;
        let target_ns = mul_div((frame as i64 + self.ts_offset).max(0) as u64, 1_000_000_000, rate);
        let start_ns = target_ns.saturating_sub(300_000_000);
        let (cluster, from_start) = pick_cluster(&self.cues, self.first_cluster, start_ns, self.tick);
        stream.restart(cluster)?;
        let mut dec = oc_mp3::Mp3Decoder::new(stream)?;
        self.pending.clear();
        self.discard_before = frame;
        let ch = self.spec.channels as usize;
        if from_start {
            self.pos = -self.delay;
            self.origin = self.pos;
        } else {
            // Decode ahead until enough timestamps constrain the position.
            loop {
                let have = self.stamps.lock().unwrap_or_else(|e| e.into_inner()).stamps.len();
                if have >= 16 || self.pending.len() / ch > 2 * rate as usize {
                    break;
                }
                if !dec.next_chunk(&mut self.pending)? {
                    break;
                }
            }
            let mut iv = Interval::EMPTY;
            for &(k, ts) in self.stamps.lock().unwrap_or_else(|e| e.into_inner()).stamps.iter() {
                let (lo, hi) = ts_range(ts, self.spec.sample_rate, self.tick);
                let off = self.ts_offset + k as i64 * self.spf;
                iv.add(lo - off, hi - off);
            }
            let half_tick = ts_range(0, self.spec.sample_rate, self.tick).1;
            let start = if iv.count > 0 { snap_to_grid(iv.value(), &iv, Some(-self.delay), self.spf, half_tick) } else { frame as i64 };
            self.origin = start;
            self.pos = start + (self.pending.len() / ch) as i64;
        }
        self.dec = Some(dec);
        Ok(frame)
    }
    fn codec_name(&self) -> &'static str {
        self.codec
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn demuxer(track: u64) -> Demuxer {
        let src: Box<dyn Source> = Box::new(std::io::Cursor::new(Vec::new()));
        Demuxer { r: BufReader::new(src), seg_end: 0, track, strip: Vec::new(), timecode_scale: 1_000_000, cluster_tc: 100, queue: VecDeque::new() }
    }

    #[test]
    fn block_lacing() {
        // Xiph lacing: 3 frames of 2, 300 and 4 bytes.
        let mut b = vec![0x81, 0x00, 0x05, 0x02, 0x02, 2, 255, 45];
        b.extend_from_slice(&[1; 2]);
        b.extend_from_slice(&[2; 300]);
        b.extend_from_slice(&[3; 4]);
        let mut d = demuxer(1);
        d.parse_block(&b, 0).unwrap();
        let sizes: Vec<usize> = d.queue.iter().map(|f| f.data.len()).collect();
        assert_eq!(sizes, vec![2, 300, 4]);
        assert_eq!(d.queue[0].ts, Some(105_000_000));
        assert_eq!(d.queue[1].ts, None);
        // EBML lacing: sizes 10, 12 (diff +2), 9 (rest).
        let mut b = vec![0x81, 0x00, 0x00, 0x06, 0x02, 0x8a, 0xc1];
        b.extend_from_slice(&[0; 31]);
        let mut d = demuxer(1);
        d.parse_block(&b, 7).unwrap();
        let sizes: Vec<usize> = d.queue.iter().map(|f| f.data.len()).collect();
        assert_eq!(sizes, vec![10, 12, 9]);
        assert_eq!(d.queue[2].discard_ns, 7);
        // Fixed lacing: 4 frames of 3 bytes; other tracks are ignored.
        let mut b = vec![0x81, 0x00, 0x00, 0x04, 0x03];
        b.extend_from_slice(&[0; 12]);
        let mut d = demuxer(1);
        d.parse_block(&b, 0).unwrap();
        assert_eq!(d.queue.len(), 4);
        let mut d = demuxer(2);
        d.parse_block(&b, 0).unwrap();
        assert!(d.queue.is_empty());
        // Corrupt lace sizes are errors, not panics.
        let mut d = demuxer(1);
        assert!(d.parse_block(&[0x81, 0, 0, 0x02, 0x01, 255, 255, 255], 0).is_err());
    }

    #[test]
    fn xiph() {
        let mut d = vec![2u8, 3, 255, 1];
        d.extend_from_slice(&[1, 1, 1]);
        d.extend_from_slice(&[2u8; 256]);
        d.extend_from_slice(&[3, 3]);
        let parts = xiph_split(&d).unwrap();
        assert_eq!(parts.len(), 3);
        assert_eq!(parts[0].len(), 3);
        assert_eq!(parts[1].len(), 256);
        assert_eq!(parts[2], &[3, 3]);
    }

    #[test]
    fn element_iteration() {
        // TimecodeScale = 1000000, Duration float32
        let d = [0x2A, 0xD7, 0xB1, 0x83, 0x0F, 0x42, 0x40, 0x44, 0x89, 0x84, 0x45, 0x9C, 0x40, 0x00];
        let v: Vec<_> = children(&d).collect();
        assert_eq!(v[0].0, ID_TIMECODESCALE);
        assert_eq!(uint(v[0].1), 1_000_000);
        assert_eq!(v[1].0, ID_DURATION);
        assert_eq!(float(v[1].1), 5000.0);
        assert_eq!(sint(&[0xff, 0xfe]), -2);
    }
}
