//! Raw AAC in ADTS framing (`.aac`): optional ID3v2 prefix and trailing
//! APE/ID3v1 tags, header-only frame scan for the duration and a frame
//! index for sample-accurate seeking (one frame of decoder pre-roll).

use std::io::{BufReader, Read, Seek, SeekFrom};

use oc_aac::AdtsHeader;

use crate::tags::{self, id3};
use crate::util::{bitrate_kbps, emit_window, seek_buffered, frames_to_ms, invalid, read_up_to, stream_len, unsupported};
use crate::{AudioSpec, Decoder, Error, MediaInfo, Source, Tags};

/// A located ADTS frame.
#[derive(Clone, Copy)]
struct Frame {
    offset: u64,
    len: u32,
    header_len: u8,
}

fn parse_at(d: &[u8]) -> Option<AdtsHeader> {
    if d.len() < 7 || d[1] & 0x06 != 0 {
        return None;
    }
    AdtsHeader::parse(d)
}

/// Scans ADTS frame headers in `[start, end)`, resynchronising after
/// garbage. Only frames whose successor also has a valid header (or that
/// end exactly at `end`) are accepted after a resync.
fn scan<R: Read + Seek + ?Sized>(r: &mut R, start: u64, end: u64) -> Result<(Vec<Frame>, Option<AdtsHeader>), Error> {
    const BLOCK: usize = 256 * 1024;
    let mut buf = vec![0u8; BLOCK];
    let mut buf_start = start;
    let mut filled = 0usize;
    let mut pos = start;
    let mut frames = Vec::new();
    let mut first: Option<AdtsHeader> = None;
    let mut synced = false;
    loop {
        if pos + 9 > buf_start + filled as u64 {
            if pos >= end {
                break;
            }
            r.seek(SeekFrom::Start(pos))?;
            let want = ((end - pos) as usize).min(BLOCK);
            filled = read_up_to(r, &mut buf[..want])?;
            buf_start = pos;
            if filled < 7 {
                break;
            }
        }
        let i = (pos - buf_start) as usize;
        let d = &buf[i..filled];
        let h = parse_at(d).filter(|h| match first {
            Some(f) => f.sample_rate_index == h.sample_rate_index && f.object_type == h.object_type,
            None => true,
        });
        let ok = match h {
            Some(h) if synced => pos + h.frame_length as u64 <= end,
            Some(h) => {
                // Confirm with the next header when (re)synchronising.
                let next = i + h.frame_length;
                pos + h.frame_length as u64 == end || (next + 7 <= filled && parse_at(&buf[next..filled]).is_some()) || next + 7 > filled
            }
            None => false,
        };
        if let (true, Some(h)) = (ok, h) {
            first.get_or_insert(h);
            frames.push(Frame { offset: pos, len: h.frame_length as u32, header_len: h.header_length as u8 });
            pos += h.frame_length as u64;
            synced = true;
        } else {
            synced = false;
            pos += 1;
        }
    }
    Ok((frames, first))
}

fn rate_of(h: &AdtsHeader) -> u32 {
    oc_aac::SAMPLE_RATES.get(h.sample_rate_index as usize).copied().unwrap_or(0)
}

fn channels_of(h: &AdtsHeader) -> u16 {
    match h.channel_config {
        1..=6 => h.channel_config as u16,
        7 => 8,
        _ => 2,
    }
}

pub(crate) fn read_info<R: Read + Seek + ?Sized>(r: &mut R, with_pictures: bool) -> Result<MediaInfo, Error> {
    let file_len = stream_len(r)?;
    r.seek(SeekFrom::Start(0))?;
    let mut tags = Tags::default();
    let mut pictures = Vec::new();
    let start = id3::read_id3v2_tags(r, &mut tags, with_pictures.then_some(&mut pictures))?;
    let end = tags::read_end_tags(r, file_len, with_pictures.then_some(&mut pictures))?;
    tags::merge_missing(&mut tags, &end.ape);
    tags::merge_missing(&mut tags, &end.id3v1);
    let (frames, first) = scan(r, start, end.audio_end)?;
    let Some(h) = first else { return invalid("no ADTS frames") };
    let rate = rate_of(&h);
    let total = frames.len() as u64 * 1024 * (h.raw_blocks as u64 + 1);
    let bytes: u64 = frames.iter().map(|f| f.len as u64).sum();
    Ok(MediaInfo {
        container: "adts",
        codec: "aac",
        sample_rate: rate,
        channels: channels_of(&h),
        duration_ms: Some(frames_to_ms(total, rate)),
        bitrate_kbps: bitrate_kbps(bytes, total, rate),
        tags,
        pictures,
    })
}

struct AdtsDecoder {
    r: BufReader<Box<dyn Source>>,
    /// Current stream position of `r`.
    cur: u64,
    frames: Vec<Frame>,
    dec: oc_aac::AacDecoder,
    spec: AudioSpec,
    /// Output frames per ADTS frame (1024, or 2048 with SBR).
    out_spf: u64,
    next: usize,
    discard_before: u64,
    buf: Vec<u8>,
    tmp: Vec<f32>,
}

pub(crate) fn open(src: Box<dyn Source>) -> Result<Box<dyn Decoder>, Error> {
    let mut r = BufReader::with_capacity(64 * 1024, src);
    let file_len = stream_len(&mut r)?;
    r.seek(SeekFrom::Start(0))?;
    let start = id3::skip_id3v2(&mut r)?;
    let end = tags::read_end_tags(&mut r, file_len, None)?;
    let (frames, first) = scan(&mut r, start, end.audio_end)?;
    let Some(h) = first else { return invalid("no ADTS frames") };
    if h.raw_blocks != 0 {
        return unsupported("ADTS frames with multiple raw data blocks");
    }
    let dec = oc_aac::AacDecoder::new(&h.audio_specific_config())?;
    let spec = AudioSpec { sample_rate: dec.sample_rate(), channels: dec.channels() };
    if spec.sample_rate == 0 || spec.channels == 0 {
        return invalid("decoder reported an empty stream format");
    }
    let core = rate_of(&h).max(1) as u64;
    let out_spf = 1024 * spec.sample_rate as u64 / core;
    let cur = r.stream_position()?;
    Ok(Box::new(AdtsDecoder { r, cur, frames, dec, spec, out_spf: out_spf.max(1), next: 0, discard_before: 0, buf: Vec::new(), tmp: Vec::new() }))
}

impl Decoder for AdtsDecoder {
    fn spec(&self) -> AudioSpec {
        self.spec
    }
    fn total_frames(&self) -> Option<u64> {
        Some(self.frames.len() as u64 * self.out_spf)
    }
    fn next_chunk(&mut self, out: &mut Vec<f32>) -> Result<bool, Error> {
        loop {
            let Some(&f) = self.frames.get(self.next) else { return Ok(false) };
            let pos = self.next as u64 * self.out_spf;
            self.next += 1;
            seek_buffered(&mut self.r, &mut self.cur, f.offset)?;
            self.buf.resize(f.len as usize, 0);
            let got = read_up_to(&mut self.r, &mut self.buf)?;
            self.cur += got as u64;
            if got < f.len as usize {
                self.next = self.frames.len();
                return invalid("truncated ADTS frame");
            }
            self.tmp.clear();
            let n = self.dec.decode_frame(&self.buf[f.header_len as usize..], &mut self.tmp)?;
            let ch = self.spec.channels as usize;
            self.tmp.truncate(n * ch);
            if n > 0 && n as u64 != self.out_spf && self.next == 1 {
                self.out_spf = n as u64; // implicit SBR detected on the first frame
            }
            if emit_window(&self.tmp, ch, pos as i64, self.discard_before, None, out) > 0 {
                return Ok(true);
            }
        }
    }
    fn seek(&mut self, frame: u64) -> Result<u64, Error> {
        let total = self.frames.len() as u64 * self.out_spf;
        let frame = frame.min(total);
        let k = (frame / self.out_spf) as usize;
        self.next = k.saturating_sub(1);
        self.dec.reset();
        self.discard_before = frame;
        Ok(frame)
    }
    fn codec_name(&self) -> &'static str {
        "aac"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn frame(len: usize) -> Vec<u8> {
        // AAC-LC, 44.1 kHz, stereo, no CRC.
        let mut f = vec![0xff, 0xf1, 0x50, 0x80, 0, 0x1f, 0xfc];
        f[3] |= ((len >> 11) & 3) as u8;
        f[4] = ((len >> 3) & 0xff) as u8;
        f[5] = (((len & 7) << 5) as u8) | 0x1f;
        f.resize(len, 0x11);
        f
    }

    #[test]
    fn scan_with_garbage() {
        let mut d = b"junk".to_vec();
        for i in 0..10 {
            d.extend(frame(200 + i));
        }
        d.extend_from_slice(&[0xff, 0xf1, 0x00]); // truncated tail
        let (frames, first) = scan(&mut Cursor::new(&d), 0, d.len() as u64).unwrap();
        assert_eq!(frames.len(), 10);
        assert_eq!(frames[0].offset, 4);
        assert_eq!(rate_of(&first.unwrap()), 44100);
    }
}
