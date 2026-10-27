//! Ogg container support for Vorbis, Opus and FLAC-in-Ogg.
//!
//! The first logical stream with a supported codec is decoded. Output
//! positions are derived from granule positions: after opening or seeking,
//! decoded audio is buffered until a page granule tells where it ends, so
//! beginning trimming (Opus pre-skip, Vorbis start offsets), end trimming
//! (final granule) and sample-accurate seeking all use the same mechanism.
//! Seeking bisects to a page well before the target (Opus: 80 ms pre-roll
//! as required by RFC 7845 plus one maximum packet), then decodes and
//! discards up to the exact frame.

use std::io::{BufReader, Read, Seek, SeekFrom};

use bl_ogg::{read_page, PacketReader};

use crate::flac::parse_streaminfo;
use crate::tags::vorbis_comment::{parse_flac_picture, parse_vorbis_comment};
use crate::util::{be16, bitrate_kbps, emit_window, frames_to_ms, invalid, le32, stream_len, unsupported};
use crate::{AudioSpec, Decoder, Error, MediaInfo, Source, Tags};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum OggCodec {
    Vorbis,
    Opus,
    Flac,
}

fn identify(p: &[u8]) -> Option<OggCodec> {
    if p.len() >= 7 && &p[..7] == b"\x01vorbis" {
        Some(OggCodec::Vorbis)
    } else if p.starts_with(b"OpusHead") {
        Some(OggCodec::Opus)
    } else if p.len() >= 13 && p.starts_with(b"\x7fFLAC") {
        Some(OggCodec::Flac)
    } else {
        None
    }
}

/// Finds the first logical stream (among the BOS pages) with a supported
/// codec.
fn find_stream<R: Read + Seek + ?Sized>(r: &mut R) -> Result<(u32, OggCodec), Error> {
    r.seek(SeekFrom::Start(0))?;
    let mut r = r;
    let mut pages = 0;
    while let Some(page) = read_page(&mut r)? {
        pages += 1;
        if !page.is_bos() || pages > 64 {
            break;
        }
        let len: usize = page.lacing.iter().take_while(|&&l| l == 255).count() * 255
            + page.lacing.iter().find(|&&l| l < 255).map_or(0, |&l| l as usize);
        if let Some(c) = identify(&page.data[..len.min(page.data.len())]) {
            return Ok((page.serial, c));
        }
    }
    unsupported("no supported codec in Ogg stream")
}

/// Reads the header packets of the selected stream. Leaves the reader at
/// the first audio page and returns (headers, data_start).
fn read_headers<R: Read + Seek>(pr: &mut PacketReader<R>, codec: OggCodec) -> Result<(Vec<Vec<u8>>, u64), Error> {
    let mut headers: Vec<Vec<u8>> = Vec::new();
    let mut needed = match codec {
        OggCodec::Vorbis => 3,
        OggCodec::Opus => 2,
        OggCodec::Flac => 1,
    };
    while headers.len() < needed {
        let Some(p) = pr.next_packet()? else { return invalid("truncated Ogg headers") };
        if headers.is_empty() && codec == OggCodec::Flac {
            let n = be16(&p.data, 7).unwrap_or(0) as usize;
            needed = 1 + n.min(1024);
        }
        if codec == OggCodec::Flac && !headers.is_empty() && p.data.first() == Some(&0xff) {
            break; // audio frame: fewer header packets than announced
        }
        headers.push(p.data);
    }
    match codec {
        OggCodec::Vorbis => {
            if headers[1].get(..7) != Some(b"\x03vorbis") || headers[2].get(..7) != Some(b"\x05vorbis") {
                return invalid("bad Vorbis header packets");
            }
        }
        OggCodec::Opus => {
            if !headers[1].starts_with(b"OpusTags") {
                return invalid("missing OpusTags");
            }
        }
        OggCodec::Flac => {}
    }
    let data_start = pr.get_mut().stream_position()?;
    Ok((headers, data_start))
}

/// Codec parameters extracted from the header packets (no decoder needed).
struct HeaderInfo {
    sample_rate: u32,
    channels: u16,
    /// Granule position of output frame 0 (Opus pre-skip).
    granule_offset: u64,
    nominal_bitrate: Option<u32>,
    /// Maximum packet duration in frames (used for seek pre-roll).
    max_packet: u64,
}

fn header_info(codec: OggCodec, headers: &[Vec<u8>]) -> Result<HeaderInfo, Error> {
    let h = &headers[0];
    match codec {
        OggCodec::Vorbis => {
            let (Some(rate), Some(nom)) = (le32(h, 12), le32(h, 20)) else { return invalid("short Vorbis ident header") };
            let channels = *h.get(11).unwrap_or(&0) as u16;
            if rate == 0 || channels == 0 {
                return invalid("bad Vorbis ident header");
            }
            let bs1 = 1u64 << (h.get(28).map_or(13, |b| b >> 4)).min(13);
            Ok(HeaderInfo {
                sample_rate: rate,
                channels,
                granule_offset: 0,
                nominal_bitrate: (nom as i32 > 0).then_some(nom / 1000),
                max_packet: bs1,
            })
        }
        OggCodec::Opus => {
            let head = bl_opus::OpusHead::parse(h)?;
            Ok(HeaderInfo {
                sample_rate: 48000,
                channels: head.channels as u16,
                granule_offset: head.pre_skip as u64,
                nominal_bitrate: None,
                max_packet: 5760,
            })
        }
        OggCodec::Flac => {
            let si = h.get(17..).and_then(parse_streaminfo).ok_or_else(|| Error::InvalidData("bad Ogg FLAC header".into()))?;
            let max_block = be16(h, 17 + 2).unwrap_or(4096).max(16) as u64;
            Ok(HeaderInfo { sample_rate: si.sample_rate, channels: si.channels, granule_offset: 0, nominal_bitrate: None, max_packet: max_block })
        }
    }
}

fn read_comments(codec: OggCodec, headers: &[Vec<u8>], tags: &mut Tags, pictures: &mut Vec<crate::Picture>, with_pictures: bool) {
    let pics = with_pictures.then_some(&mut *pictures);
    match codec {
        OggCodec::Vorbis => parse_vorbis_comment(&headers[1][7..], tags, pics),
        OggCodec::Opus => parse_vorbis_comment(&headers[1][8..], tags, pics),
        OggCodec::Flac => {
            let mut pics = pics;
            for b in &headers[1..] {
                let Some(&t) = b.first() else { continue };
                let body = b.get(4..).unwrap_or(&[]);
                match t & 0x7f {
                    4 => parse_vorbis_comment(body, tags, pics.as_deref_mut()),
                    6 => {
                        if let (Some(p), Some(pic)) = (pics.as_deref_mut(), parse_flac_picture(body)) {
                            p.push(pic);
                        }
                    }
                    _ => {}
                }
            }
        }
    }
}

fn codec_name(c: OggCodec) -> &'static str {
    match c {
        OggCodec::Vorbis => "vorbis",
        OggCodec::Opus => "opus",
        OggCodec::Flac => "flac",
    }
}

pub(crate) fn read_info<R: Read + Seek + ?Sized>(r: &mut R, with_pictures: bool) -> Result<MediaInfo, Error> {
    let file_len = stream_len(r)?;
    let (serial, codec) = find_stream(r)?;
    r.seek(SeekFrom::Start(0))?;
    let mut pr = PacketReader::new(&mut *r);
    pr.set_serial(Some(serial));
    let (headers, data_start) = read_headers(&mut pr, codec)?;
    let info = header_info(codec, &headers)?;
    let mut tags = Tags::default();
    let mut pictures = Vec::new();
    read_comments(codec, &headers, &mut tags, &mut pictures, with_pictures);
    let total = pr.last_granule(serial)?.map(|g| g.saturating_sub(info.granule_offset));
    let audio_bytes = file_len.saturating_sub(data_start);
    let bitrate = total.and_then(|t| bitrate_kbps(audio_bytes, t, info.sample_rate)).or(info.nominal_bitrate);
    Ok(MediaInfo {
        container: "ogg",
        codec: codec_name(codec),
        sample_rate: info.sample_rate,
        channels: info.channels,
        duration_ms: total.map(|t| frames_to_ms(t, info.sample_rate)),
        bitrate_kbps: bitrate,
        tags,
        pictures,
    })
}

enum PacketDecoder {
    Vorbis(Box<bl_vorbis::VorbisDecoder>),
    Opus(bl_opus::OpusDecoder),
    Flac(bl_flac::FlacFrameDecoder),
}

impl PacketDecoder {
    /// Decodes an audio packet; header packets are ignored (0 frames).
    fn decode(&mut self, p: &[u8], out: &mut Vec<f32>) -> Result<usize, Error> {
        match self {
            PacketDecoder::Vorbis(d) => {
                if p.first().map_or(true, |b| b & 1 == 1) {
                    return Ok(0);
                }
                d.decode_packet(p, out).map_err(Error::from)
            }
            PacketDecoder::Opus(d) => {
                if p.is_empty() || p.starts_with(b"OpusHead") || p.starts_with(b"OpusTags") {
                    return Ok(0);
                }
                d.decode_packet(p, out).map_err(Error::from)
            }
            PacketDecoder::Flac(d) => {
                if p.first() != Some(&0xff) {
                    return Ok(0);
                }
                d.decode(p, out).map_err(Error::from)
            }
        }
    }
    fn reset(&mut self) {
        match self {
            PacketDecoder::Vorbis(d) => d.reset(),
            PacketDecoder::Opus(d) => d.reset(),
            PacketDecoder::Flac(_) => {}
        }
    }
}

struct OggDecoder {
    pr: PacketReader<BufReader<Box<dyn Source>>>,
    serial: u32,
    dec: PacketDecoder,
    codec: OggCodec,
    spec: AudioSpec,
    data_start: u64,
    granule_offset: u64,
    seek_preroll: u64,
    total: Option<u64>,
    /// Output position of the next decoded frame, once known.
    pos: Option<i64>,
    /// Decoded audio whose position is not known yet.
    pending: Vec<f32>,
    pending_frames: i64,
    /// Output position where decoding restarted, if known (stream start).
    known_start: Option<i64>,
    /// Granule of the page preceding the restart point.
    restart_granule: u64,
    discard_before: u64,
    /// Trimmed output ready to be returned.
    ready: Vec<f32>,
    finished: bool,
    tmp: Vec<f32>,
}

pub(crate) fn open(src: Box<dyn Source>) -> Result<Box<dyn Decoder>, Error> {
    let mut r = BufReader::with_capacity(64 * 1024, src);
    let (serial, codec) = find_stream(&mut r)?;
    r.seek(SeekFrom::Start(0))?;
    let mut pr = PacketReader::new(r);
    pr.set_serial(Some(serial));
    let (headers, data_start) = read_headers(&mut pr, codec)?;
    let info = header_info(codec, &headers)?;
    let dec = match codec {
        OggCodec::Vorbis => PacketDecoder::Vorbis(Box::new(bl_vorbis::VorbisDecoder::new(&headers[0], &headers[1], &headers[2])?)),
        OggCodec::Opus => {
            let head = bl_opus::OpusHead::parse(&headers[0])?;
            PacketDecoder::Opus(bl_opus::OpusDecoder::new(&head, 48000)?)
        }
        OggCodec::Flac => PacketDecoder::Flac(bl_flac::FlacFrameDecoder::new(&headers[0])?),
    };
    let spec = match &dec {
        PacketDecoder::Vorbis(d) => AudioSpec { sample_rate: d.sample_rate(), channels: d.channels() },
        PacketDecoder::Opus(d) => AudioSpec { sample_rate: d.sample_rate(), channels: d.channels() },
        PacketDecoder::Flac(d) => AudioSpec { sample_rate: d.sample_rate(), channels: d.channels() },
    };
    if spec.sample_rate == 0 || spec.channels == 0 {
        return invalid("decoder reported an empty stream format");
    }
    let total = pr.last_granule(serial)?.map(|g| g.saturating_sub(info.granule_offset));
    let seek_preroll = match codec {
        OggCodec::Opus => 3840 + info.max_packet,
        _ => 2 * info.max_packet,
    };
    let granule_offset = info.granule_offset;
    Ok(Box::new(OggDecoder {
        pr,
        serial,
        dec,
        codec,
        spec,
        data_start,
        granule_offset,
        seek_preroll,
        total,
        pos: None,
        pending: Vec::new(),
        pending_frames: 0,
        known_start: Some(-(granule_offset as i64)),
        restart_granule: 0,
        discard_before: 0,
        ready: Vec::new(),
        finished: false,
        tmp: Vec::new(),
    }))
}

impl OggDecoder {
    fn emit(&mut self, from_pending: bool, pos: i64) {
        let ch = self.spec.channels as usize;
        let chunk = if from_pending { &self.pending } else { &self.tmp };
        emit_window(chunk, ch, pos, self.discard_before, self.total, &mut self.ready);
    }

    fn restart(&mut self) {
        self.dec.reset();
        self.pending.clear();
        self.pending_frames = 0;
        self.pos = None;
        self.ready.clear();
        self.finished = false;
    }

    /// Repositions to just after the page with granule `< target`.
    fn reposition(&mut self, target: u64) -> Result<(), Error> {
        let g = if target == 0 {
            self.pr.get_mut().seek(SeekFrom::Start(self.data_start))?;
            self.pr.reset();
            0
        } else {
            self.pr.seek_granule(self.serial, target, self.data_start)?
        };
        self.restart_granule = g;
        self.known_start = (g == 0).then_some(-(self.granule_offset as i64));
        self.restart();
        Ok(())
    }

    /// Processes one packet. Returns false at the end of the stream.
    fn step(&mut self) -> Result<bool, Error> {
        if self.finished {
            return Ok(false);
        }
        let Some(pkt) = self.pr.next_packet()? else {
            if self.pos.is_none() && !self.pending.is_empty() {
                let start = self.known_start.unwrap_or(0);
                self.emit(true, start);
                self.pending.clear();
            }
            self.finished = true;
            return Ok(false);
        };
        self.tmp.clear();
        let ch = self.spec.channels as usize;
        let n = self.dec.decode(&pkt.data, &mut self.tmp)?;
        self.tmp.truncate(n * ch);
        match self.pos {
            Some(p) => {
                self.emit(false, p);
                self.pos = Some(p + n as i64);
            }
            None => {
                self.pending.extend_from_slice(&self.tmp);
                self.pending_frames += n as i64;
                if let Some(g) = pkt.granule {
                    let end = g as i64 - self.granule_offset as i64;
                    let start = if pkt.eos {
                        match self.known_start {
                            Some(s) => s,
                            None => {
                                // The final page carries an end-trimmed
                                // granule: restart one page earlier.
                                let target = self.restart_granule;
                                self.reposition(target)?;
                                return Ok(true);
                            }
                        }
                    } else {
                        end - self.pending_frames
                    };
                    self.emit(true, start);
                    self.pos = Some(start + self.pending_frames);
                    self.pending.clear();
                    self.pending_frames = 0;
                }
            }
        }
        Ok(true)
    }
}

impl Decoder for OggDecoder {
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
        let frame = self.total.map_or(frame, |t| frame.min(t));
        let target = (frame + self.granule_offset).saturating_sub(self.seek_preroll);
        self.reposition(target)?;
        self.discard_before = frame;
        // Decode until the position is established so the actual frame can
        // be reported (the output is kept in `ready`).
        let mut guard = 0;
        while self.pos.is_none() && !self.finished && guard < 100_000 {
            self.step()?;
            guard += 1;
        }
        let actual = match self.pos {
            Some(p) => {
                let first = p - (self.ready.len() / self.spec.channels as usize) as i64;
                first.max(frame as i64) as u64
            }
            None => frame,
        };
        Ok(actual.max(frame))
    }
    fn codec_name(&self) -> &'static str {
        codec_name(self.codec)
    }
}
