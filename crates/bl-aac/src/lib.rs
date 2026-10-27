//! AAC decoder (MPEG-4 AAC-LC; HE-AAC SBR/PS optional), std only. Packet
//! level: callers provide raw_data_block payloads (from MP4 samples or ADTS
//! frames with the header stripped).
//!
//! Output is interleaved `f32` in [-1, 1], WAVE/SMPTE channel order
//! (L R C LFE BL BR SL SR) — AAC channel configurations are remapped.
//!
//! # Frame sizes, delay and priming
//!
//! Every decoded frame yields 1024 PCM frames per channel for AAC-LC and
//! 2048 when SBR (HE-AAC) is active (output at twice the core rate). The
//! output is the codec's natural output: the first frame contains the
//! encoder priming / filterbank delay. Trimming priming samples (e.g. from
//! an MP4 edit list or iTunSMPB) is the container layer's job.
//!
//! # Implicit HE-AAC signalling
//!
//! When the AudioSpecificConfig does not signal SBR (e.g. a config derived
//! from an ADTS header), SBR is detected in-band: if the first decoded frame
//! carries SBR data, [`AacDecoder::sample_rate`] doubles (and a mono stream
//! becomes stereo, as parametric stereo may follow) from that frame on.
//! Callers should therefore query `sample_rate()`/`channels()` after the
//! first successful [`AacDecoder::decode_frame`]. SBR appearing only after
//! the first frame is ignored (the core is still decoded).
//!
//! Supported: AAC-LC core (all window shapes/sequences, M/S, intensity,
//! PNS, TNS, pulse data), CCE/DSE/PCE/FIL parsing, HE-AAC v1 (SBR) and v2
//! (PS). Not supported: AAC Main / SSR / LTP, ER object types and the
//! 960-sample frame length (these return [`Error::Unsupported`]).

#![forbid(unsafe_code)]

mod bits;
mod config;
mod fft;
mod filterbank;
mod huffman;
mod huffman_tables;
mod ics;
mod mdct;
mod ps;
mod sbr;
mod sbr_huffman_tables;
mod stereo;
mod tables;

pub use config::{AudioSpecificConfig, ProgramConfig};

use bits::BitReader;
use config::{aot, Role};
use filterbank::{ChannelState, Filterbank};
use ics::{ChannelData, MAX_SFB};
use sbr::{SbrElement, Slot, SLOTS};
use stereo::NoiseGen;

#[derive(Debug)]
pub enum Error {
    InvalidData(&'static str),
    Unsupported(&'static str),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::InvalidData(s) => write!(f, "invalid aac data: {s}"),
            Error::Unsupported(s) => write!(f, "unsupported aac feature: {s}"),
        }
    }
}

impl std::error::Error for Error {}

/// Parsed ADTS header.
#[derive(Debug, Clone, Copy)]
pub struct AdtsHeader {
    /// Audio object type (1 = Main, 2 = LC, ...), i.e. profile + 1.
    pub object_type: u8,
    pub sample_rate_index: u8,
    pub channel_config: u8,
    /// Total frame length including the header.
    pub frame_length: usize,
    pub header_length: usize,
    pub raw_blocks: u8,
}

impl AdtsHeader {
    pub fn parse(data: &[u8]) -> Option<AdtsHeader> {
        if data.len() < 7 || data[0] != 0xff || data[1] & 0xf6 != 0xf0 {
            return None;
        }
        let protection_absent = data[1] & 1 != 0;
        let object_type = (data[2] >> 6) + 1;
        let sample_rate_index = (data[2] >> 2) & 0xf;
        if sample_rate_index > 12 {
            return None;
        }
        let channel_config = ((data[2] & 1) << 2) | (data[3] >> 6);
        let frame_length = (((data[3] & 3) as usize) << 11) | ((data[4] as usize) << 3) | ((data[5] >> 5) as usize);
        let header_length = if protection_absent { 7 } else { 9 };
        if frame_length < header_length {
            return None;
        }
        Some(AdtsHeader { object_type, sample_rate_index, channel_config, frame_length, header_length, raw_blocks: data[6] & 3 })
    }

    /// Builds the equivalent 2-byte AudioSpecificConfig.
    pub fn audio_specific_config(&self) -> [u8; 2] {
        [(self.object_type << 3) | (self.sample_rate_index >> 1), ((self.sample_rate_index & 1) << 7) | (self.channel_config << 3)]
    }
}

pub const SAMPLE_RATES: [u32; 13] = [96000, 88200, 64000, 48000, 44100, 32000, 24000, 22050, 16000, 12000, 11025, 8000, 7350];

const ID_SCE: u32 = 0;
const ID_CPE: u32 = 1;
const ID_CCE: u32 = 2;
const ID_LFE: u32 = 3;
const ID_DSE: u32 = 4;
const ID_PCE: u32 = 5;
const ID_FIL: u32 = 6;
const ID_END: u32 = 7;

/// Upper bound on channel elements per frame (robustness).
const MAX_ELEMENTS: usize = 64;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum ElementKind {
    Sce,
    Cpe,
    Lfe,
}

impl ElementKind {
    fn channels(self) -> usize {
        if self == ElementKind::Cpe {
            2
        } else {
            1
        }
    }
}

/// Persistent per-element state (filterbank overlap, SBR).
struct ElementSlot {
    channels: Vec<ChannelState>,
    sbr: Option<Box<SbrElement>>,
}

impl ElementSlot {
    fn new(kind: ElementKind) -> Self {
        ElementSlot { channels: (0..kind.channels()).map(|_| ChannelState::new()).collect(), sbr: None }
    }
}

const EXT_SBR_DATA: u32 = 13;
const EXT_SBR_DATA_CRC: u32 = 14;

#[derive(Clone, Debug, PartialEq)]
struct LayoutElement {
    kind: ElementKind,
    tag: u8,
    /// Index of the element's first channel among the decoded channels.
    first_channel: usize,
}

/// Channel layout: which syntactic elements carry which channels and how
/// decoded channels map to output channels.
#[derive(Clone, Debug, PartialEq)]
struct Layout {
    elements: Vec<LayoutElement>,
    /// Match elements by instance tag (PCE) instead of order of appearance.
    by_tag: bool,
    /// Number of decoded (bitstream) channels.
    decoded: usize,
    /// `perm[out] = decoded channel index`.
    perm: Vec<usize>,
}

impl Layout {
    fn new(kinds: &[(ElementKind, u8)], by_tag: bool, roles: Option<Vec<Role>>) -> Layout {
        let mut elements = Vec::new();
        let mut ch = 0;
        for &(kind, tag) in kinds {
            elements.push(LayoutElement { kind, tag, first_channel: ch });
            ch += kind.channels();
        }
        let perm = match roles {
            Some(r) if r.len() == ch && !r.contains(&Role::Other) => config::output_permutation(&r),
            _ => (0..ch).collect(),
        };
        Layout { elements, by_tag, decoded: ch, perm }
    }

    fn from_config(channel_config: u8) -> Option<Layout> {
        use ElementKind::*;
        let kinds: &[ElementKind] = match channel_config {
            1 => &[Sce],
            2 => &[Cpe],
            3 => &[Sce, Cpe],
            4 => &[Sce, Cpe, Sce],
            5 => &[Sce, Cpe, Cpe],
            6 => &[Sce, Cpe, Cpe, Lfe],
            7 => &[Sce, Cpe, Cpe, Cpe, Lfe],
            _ => return None,
        };
        let kinds: Vec<(ElementKind, u8)> = kinds.iter().map(|&k| (k, 0)).collect();
        Some(Layout::new(&kinds, false, config::config_roles(channel_config)))
    }

    fn from_pce(p: &ProgramConfig) -> Layout {
        let mut kinds = Vec::new();
        for &(cpe, tag) in p.front.iter().chain(&p.side).chain(&p.back) {
            kinds.push((if cpe { ElementKind::Cpe } else { ElementKind::Sce }, tag));
        }
        for &tag in &p.lfe {
            kinds.push((ElementKind::Lfe, tag));
        }
        Layout::new(&kinds, true, Some(config::pce_roles(p)))
    }

    /// Layout inferred from the elements of a frame (no configuration).
    fn from_elements(seen: &[(ElementKind, u8)]) -> Layout {
        Layout::new(seen, false, None)
    }

    /// Finds the layout element for an element of the given kind and tag.
    fn find(&self, kind: ElementKind, tag: u8, used: &[bool]) -> Option<usize> {
        let mut candidates = self.elements.iter().enumerate().filter(|(i, e)| e.kind == kind && !used[*i]);
        if self.by_tag {
            let mut c2 = candidates.clone();
            c2.find(|(_, e)| e.tag == tag).or_else(|| candidates.next()).map(|(i, _)| i)
        } else {
            candidates.next().map(|(i, _)| i)
        }
    }
}

pub struct AacDecoder {
    cfg: AudioSpecificConfig,
    sf_index: usize,
    layout: Option<Layout>,
    slots: Vec<ElementSlot>,
    fb: Filterbank,
    ch: [ChannelData; 2],
    ms_used: Vec<bool>,
    rng: NoiseGen,
    /// Core time-domain output per decoded channel (1024 samples each).
    core_out: Vec<Vec<f32>>,
    /// Layout elements decoded in the current frame.
    used: Vec<bool>,
    /// Elements seen in the current frame (kind, tag).
    seen: Vec<(ElementKind, u8)>,
    /// The layout was inferred from the stream's elements.
    inferred: bool,
    frames_decoded: u64,
    /// SBR may be decoded (not explicitly signalled absent, core rate ok).
    sbr_allowed: bool,
    /// SBR is decoded (output at twice the core rate).
    sbr_active: bool,
    /// Mono stream upmixed by parametric stereo.
    ps_out: bool,
    /// Downsampled SBR (output at the core rate, signalled by an extension
    /// sampling rate equal to the core rate).
    sbr_downsampled: bool,
    /// Layout slot of the last decoded channel element (for SBR payloads).
    last_slot: Option<usize>,
    /// SBR output per decoded channel (2048 samples), plus the PS right
    /// channel for mono streams.
    sbr_out: Vec<Vec<f32>>,
    qmf: [Vec<Slot>; 3],
}

impl AacDecoder {
    /// Creates a decoder from an MPEG-4 AudioSpecificConfig (e.g. from the
    /// MP4 `esds` box, or [`AdtsHeader::audio_specific_config`]).
    pub fn new(audio_specific_config: &[u8]) -> Result<Self, Error> {
        let cfg = AudioSpecificConfig::parse(audio_specific_config)?;
        Self::with_config(cfg)
    }

    /// Creates a decoder from an already parsed [`AudioSpecificConfig`].
    pub fn with_config(cfg: AudioSpecificConfig) -> Result<Self, Error> {
        match cfg.object_type {
            aot::AAC_LC => {}
            aot::AAC_MAIN => return Err(Error::Unsupported("AAC Main profile")),
            aot::AAC_SSR => return Err(Error::Unsupported("AAC SSR profile")),
            aot::AAC_LTP => return Err(Error::Unsupported("AAC LTP profile")),
            _ => return Err(Error::Unsupported("audio object type")),
        }
        if cfg.frame_length_960 {
            return Err(Error::Unsupported("960-sample frames"));
        }
        let layout = match cfg.channel_config {
            0 => cfg.pce.as_ref().map(Layout::from_pce),
            1..=7 => Layout::from_config(cfg.channel_config),
            _ => return Err(Error::Unsupported("channel configuration")),
        };
        if let Some(l) = &layout {
            if l.decoded == 0 {
                return Err(Error::InvalidData("program config without channels"));
            }
        }
        let mut dec = AacDecoder {
            sf_index: cfg.sf_index as usize,
            cfg,
            layout: None,
            slots: Vec::new(),
            fb: Filterbank::new(),
            ch: [ChannelData::new(), ChannelData::new()],
            ms_used: vec![false; 8 * MAX_SFB],
            rng: NoiseGen { state: 0x2545_f491 },
            core_out: Vec::new(),
            used: Vec::new(),
            seen: Vec::new(),
            inferred: false,
            frames_decoded: 0,
            sbr_allowed: false,
            sbr_active: false,
            ps_out: false,
            sbr_downsampled: false,
            last_slot: None,
            sbr_out: Vec::new(),
            qmf: std::array::from_fn(|_| vec![[fft::Complex::default(); 64]; SLOTS + 6]),
        };
        dec.sbr_allowed = dec.cfg.sbr != Some(false) && dec.cfg.sample_rate <= 48000;
        dec.sbr_downsampled = dec.cfg.sbr == Some(true) && dec.cfg.ext_sample_rate == Some(dec.cfg.sample_rate);
        if let Some(l) = layout {
            dec.set_layout(l);
        }
        if dec.cfg.sbr == Some(true) && dec.sbr_allowed {
            dec.activate_sbr();
        }
        Ok(dec)
    }

    fn set_layout(&mut self, l: Layout) {
        if self.layout.as_ref() == Some(&l) {
            return;
        }
        self.slots = l.elements.iter().map(|e| ElementSlot::new(e.kind)).collect();
        self.core_out = (0..l.decoded).map(|_| vec![0.0; 1024]).collect();
        self.used = vec![false; l.elements.len()];
        self.layout = Some(l);
        self.update_ps_out();
    }

    fn activate_sbr(&mut self) {
        self.sbr_active = true;
        self.update_ps_out();
    }

    /// Parametric stereo upmix applies to mono (single SCE) HE-AAC streams
    /// unless PS is explicitly signalled absent.
    fn update_ps_out(&mut self) {
        self.ps_out = self.sbr_active
            && self.cfg.ps != Some(false)
            && self.layout.as_ref().map_or(false, |l| l.decoded == 1 && l.elements[0].kind == ElementKind::Sce);
    }

    /// The parsed AudioSpecificConfig.
    pub fn config(&self) -> &AudioSpecificConfig {
        &self.cfg
    }

    /// Output sample rate (doubled when SBR is decoded).
    pub fn sample_rate(&self) -> u32 {
        if self.sbr_active && !self.sbr_downsampled {
            self.cfg.sample_rate * 2
        } else {
            self.cfg.sample_rate
        }
    }

    /// Number of output channels (0 until known for channel configuration 0
    /// streams without a PCE in the AudioSpecificConfig).
    pub fn channels(&self) -> u16 {
        self.layout.as_ref().map_or(0, |l| l.perm.len() as u16) + self.ps_out as u16
    }

    /// Whether SBR (HE-AAC) is being decoded.
    pub fn sbr_active(&self) -> bool {
        self.sbr_active
    }

    /// PCM frames per channel produced by each decoded frame.
    pub fn frame_size(&self) -> usize {
        if self.sbr_active && !self.sbr_downsampled {
            2048
        } else {
            1024
        }
    }

    /// Resets overlap state (after seeking).
    pub fn reset(&mut self) {
        for s in &mut self.slots {
            for c in &mut s.channels {
                c.reset();
            }
            if let Some(sbr) = &mut s.sbr {
                sbr.reset();
            }
        }
    }

    /// Decodes an ADTS frame (header included), handling multiple raw data
    /// blocks per frame. Appends interleaved samples to `out`.
    pub fn decode_adts_frame(&mut self, frame: &[u8], out: &mut Vec<f32>) -> Result<usize, Error> {
        let h = AdtsHeader::parse(frame).ok_or(Error::InvalidData("bad ADTS header"))?;
        let end = h.frame_length.min(frame.len());
        if h.raw_blocks == 0 {
            return self.decode_frame(&frame[h.header_length.min(end)..end], out);
        }
        // Several raw data blocks: with CRC protection the header carries
        // their positions; without, blocks are decoded back to back.
        let nblocks = h.raw_blocks as usize + 1;
        let mut total = 0;
        if h.header_length == 9 {
            let mut starts = vec![h.header_length + 2 * (nblocks - 1)];
            for i in 0..nblocks - 1 {
                let p = 7 + 2 * i;
                if p + 2 > end {
                    return Err(Error::InvalidData("truncated ADTS block positions"));
                }
                // raw_data_block_position is relative to the header start.
                starts.push(u16::from_be_bytes([frame[p], frame[p + 1]]) as usize);
            }
            for b in 0..nblocks {
                let s = starts[b].min(end);
                let e = if b + 1 < nblocks { starts[b + 1].clamp(s, end) } else { end };
                total += self.decode_frame(&frame[s..e], out)?;
            }
        } else {
            let mut pos = h.header_length.min(end);
            for _ in 0..nblocks {
                let (n, used) = self.decode_block(&frame[pos..end], out)?;
                total += n;
                pos = (pos + used).min(end);
            }
        }
        Ok(total)
    }

    /// Decodes one raw_data_block, appending interleaved samples to `out`.
    /// Returns the number of PCM frames appended.
    pub fn decode_frame(&mut self, data: &[u8], out: &mut Vec<f32>) -> Result<usize, Error> {
        self.decode_block(data, out).map(|(n, _)| n)
    }

    /// Decodes one raw_data_block; returns (frames, bytes consumed).
    fn decode_block(&mut self, data: &[u8], out: &mut Vec<f32>) -> Result<(usize, usize), Error> {
        match self.decode_block_inner(data, out)? {
            Some(v) => Ok(v),
            None => {
                // First frame does not match the signalled configuration:
                // adopt the stream's element structure and decode again.
                let l = Layout::from_elements(&self.seen);
                self.set_layout(l);
                self.inferred = true;
                self.decode_block_inner(data, out)?.ok_or(Error::InvalidData("inconsistent element structure"))
            }
        }
    }

    fn decode_block_inner(&mut self, data: &[u8], out: &mut Vec<f32>) -> Result<Option<(usize, usize)>, Error> {
        let mut r = BitReader::new(data);
        self.used.iter_mut().for_each(|u| *u = false);
        self.seen.clear();
        self.last_slot = None;
        for s in &mut self.slots {
            if let Some(sbr) = &mut s.sbr {
                sbr.begin_frame();
            }
        }
        let mut decoded_channels = 0usize;
        loop {
            if r.bits_left() < 3 {
                return Err(Error::InvalidData("missing END element"));
            }
            let id = r.read(3);
            match id {
                ID_SCE | ID_CPE | ID_LFE => {
                    if self.seen.len() >= MAX_ELEMENTS {
                        return Err(Error::InvalidData("too many elements"));
                    }
                    let tag = r.read(4) as u8;
                    let kind = match id {
                        ID_SCE => ElementKind::Sce,
                        ID_CPE => ElementKind::Cpe,
                        _ => ElementKind::Lfe,
                    };
                    self.seen.push((kind, tag));
                    let target = self.target_slot(kind, tag);
                    if kind == ElementKind::Cpe {
                        self.decode_cpe(&mut r, target)?;
                    } else {
                        self.decode_sce(&mut r, target, kind)?;
                    }
                    if target.is_some() {
                        decoded_channels += kind.channels();
                    }
                    self.last_slot = target;
                }
                ID_CCE => self.skip_cce(&mut r)?,
                ID_DSE => {
                    r.skip(4);
                    let align = r.read_bit();
                    let mut cnt = r.read(8) as usize;
                    if cnt == 255 {
                        cnt += r.read(8) as usize;
                    }
                    if align {
                        r.byte_align();
                    }
                    r.skip(8 * cnt);
                }
                ID_PCE => {
                    let pce = ProgramConfig::parse(&mut r)?;
                    if self.cfg.channel_config == 0 && pce.num_channels() > 0 && self.seen.is_empty() {
                        self.set_layout(Layout::from_pce(&pce));
                        self.inferred = false;
                        self.cfg.pce = Some(pce);
                    }
                }
                ID_FIL => {
                    let mut cnt = r.read(4) as usize;
                    if cnt == 15 {
                        cnt += r.read(8) as usize;
                        cnt -= 1;
                    }
                    if cnt > 0 {
                        let mut sub = r.sub(8 * cnt);
                        let ext = sub.read(4);
                        if ext == EXT_SBR_DATA || ext == EXT_SBR_DATA_CRC {
                            self.sbr_payload(&mut sub, ext == EXT_SBR_DATA_CRC);
                        }
                    }
                    r.skip(8 * cnt);
                }
                _ => {
                    debug_assert_eq!(id, ID_END);
                    break;
                }
            }
            r.check()?;
        }
        r.byte_align();
        let consumed = (r.position() / 8).min(data.len());
        if self.seen.is_empty() {
            return Err(Error::InvalidData("frame without audio channels"));
        }
        if self.frames_decoded == 0 && !self.inferred && self.used.iter().filter(|&&u| u).count() != self.seen.len() {
            return Ok(None);
        }
        if decoded_channels == 0 {
            return Err(Error::InvalidData("elements do not match the channel configuration"));
        }
        let layout = self.layout.as_ref().unwrap();
        // Channels of elements missing from this frame are silent.
        for (i, e) in layout.elements.iter().enumerate() {
            if !self.used[i] {
                for c in 0..e.kind.channels() {
                    self.core_out[e.first_channel + c].iter_mut().for_each(|v| *v = 0.0);
                }
            }
        }
        let frames = if self.sbr_active {
            self.run_sbr();
            if self.sbr_downsampled {
                1024
            } else {
                2048
            }
        } else {
            1024
        };
        let layout = self.layout.as_ref().unwrap();
        let src: &Vec<Vec<f32>> = if self.sbr_active { &self.sbr_out } else { &self.core_out };
        let mut order: Vec<usize> = layout.perm.clone();
        if self.ps_out {
            order.push(1);
        }
        let nout = order.len();
        let base = out.len();
        out.resize(base + frames * nout, 0.0);
        for (o, &ch) in order.iter().enumerate() {
            for (i, v) in src[ch][..frames].iter().enumerate() {
                out[base + i * nout + o] = clamp_sample(*v);
            }
        }
        self.frames_decoded += 1;
        Ok(Some((frames, consumed)))
    }

    /// Picks the layout slot for an element. While decoding the first frame
    /// of a stream without a usable configuration, the layout is inferred
    /// from the elements present.
    fn target_slot(&mut self, kind: ElementKind, tag: u8) -> Option<usize> {
        if let Some(l) = &self.layout {
            if let Some(i) = l.find(kind, tag, &self.used) {
                self.used[i] = true;
                return Some(i);
            }
            if !self.inferred || self.frames_decoded > 0 {
                return None;
            }
        } else if self.frames_decoded > 0 {
            return None;
        }
        // Grow the inferred layout (state of existing slots is kept).
        let l = Layout::from_elements(&self.seen);
        let i = l.elements.len() - 1;
        while self.slots.len() < l.elements.len() {
            let k = l.elements[self.slots.len()].kind;
            self.slots.push(ElementSlot::new(k));
        }
        while self.core_out.len() < l.decoded {
            self.core_out.push(vec![0.0; 1024]);
        }
        self.used.resize(l.elements.len(), false);
        self.used[i] = true;
        self.layout = Some(l);
        self.inferred = true;
        Some(i)
    }

    /// Handles an SBR extension payload attached to the last element.
    fn sbr_payload(&mut self, r: &mut BitReader, crc: bool) {
        if !self.sbr_allowed {
            return;
        }
        if !self.sbr_active {
            // Implicit signalling: accept SBR only from the first frame on.
            if self.cfg.sbr.is_none() && self.frames_decoded == 0 {
                self.activate_sbr();
            } else {
                return;
            }
        }
        let Some(slot) = self.last_slot else { return };
        let kind = self.layout.as_ref().unwrap().elements[slot].kind;
        if kind == ElementKind::Lfe {
            return;
        }
        let rate = self.cfg.sample_rate * 2;
        let with_ps = self.ps_out;
        let ds = self.sbr_downsampled;
        let sbr = self.slots[slot].sbr.get_or_insert_with(|| Box::new(SbrElement::new(rate, kind.channels())));
        sbr.downsampled = ds;
        // Errors leave the element without SBR data for this frame.
        let _ = sbr.parse(r, crc, with_ps);
    }

    /// Runs SBR (and PS) for all elements, filling `sbr_out`.
    fn run_sbr(&mut self) {
        let layout = self.layout.as_ref().unwrap();
        let rate = self.cfg.sample_rate * 2;
        let nbuf = layout.decoded + self.ps_out as usize;
        while self.sbr_out.len() < nbuf {
            self.sbr_out.push(vec![0.0; 2048]);
        }
        let [x, left, right] = &mut self.qmf;
        for (i, e) in layout.elements.iter().enumerate() {
            let slot = &mut self.slots[i];
            let sbr = slot.sbr.get_or_insert_with(|| Box::new(SbrElement::new(rate, e.kind.channels())));
            sbr.downsampled = self.sbr_downsampled;
            for c in 0..e.kind.channels() {
                let dst = e.first_channel + c;
                sbr.process_channel(c, &self.core_out[dst], x);
                if self.ps_out && i == 0 {
                    sbr.process_ps(x, left, right);
                    sbr.synthesize(0, left, &mut self.sbr_out[0]);
                    sbr.synthesize_ps_right(right, &mut self.sbr_out[1]);
                } else {
                    sbr.synthesize(c, x, &mut self.sbr_out[dst]);
                }
            }
            sbr.end_frame();
        }
    }

    fn decode_sce(&mut self, r: &mut BitReader, target: Option<usize>, kind: ElementKind) -> Result<(), Error> {
        let sf_index = self.sf_index;
        ics::parse_ics(r, &mut self.ch[0], sf_index, None)?;
        if kind == ElementKind::Lfe && self.ch[0].info.is_short() {
            return Err(Error::InvalidData("short window in LFE"));
        }
        let Some(slot) = target else { return Ok(()) };
        stereo::apply_pns(&mut self.ch[0], None, &mut self.rng);
        ics::apply_tns(&mut self.ch[0], sf_index);
        let first = self.layout.as_ref().unwrap().elements[slot].first_channel;
        let ch = &self.ch[0];
        let (seq, shape) = (ch.info.window_sequence, ch.info.window_shape);
        self.fb.synthesize(&ch.spec, seq, shape, &mut self.slots[slot].channels[0], &mut self.core_out[first]);
        Ok(())
    }

    fn decode_cpe(&mut self, r: &mut BitReader, target: Option<usize>) -> Result<(), Error> {
        let sf_index = self.sf_index;
        let common_window = r.read_bit();
        let mut ms_present = false;
        let common = if common_window {
            let info = ics::IcsInfo::parse(r, sf_index)?;
            let mask = r.read(2);
            self.ms_used.iter_mut().for_each(|v| *v = false);
            match mask {
                0 => {}
                1 => {
                    ms_present = true;
                    for g in 0..info.num_groups {
                        for sfb in 0..info.max_sfb {
                            self.ms_used[g * MAX_SFB + sfb] = r.read_bit();
                        }
                    }
                }
                2 => {
                    ms_present = true;
                    for g in 0..info.num_groups {
                        for sfb in 0..info.max_sfb {
                            self.ms_used[g * MAX_SFB + sfb] = true;
                        }
                    }
                }
                _ => return Err(Error::InvalidData("reserved ms_mask_present")),
            }
            Some(info)
        } else {
            self.ms_used.iter_mut().for_each(|v| *v = false);
            None
        };
        let [ch0, ch1] = &mut self.ch;
        ics::parse_ics(r, ch0, sf_index, common.as_ref())?;
        ics::parse_ics(r, ch1, sf_index, common.as_ref())?;
        let Some(slot) = target else { return Ok(()) };
        if ms_present {
            stereo::apply_ms(ch0, ch1, &self.ms_used);
        }
        stereo::apply_pns(ch0, None, &mut self.rng);
        if ms_present {
            stereo::apply_pns(ch1, Some((ch0, &self.ms_used)), &mut self.rng);
        } else {
            stereo::apply_pns(ch1, None, &mut self.rng);
        }
        if common_window {
            stereo::apply_intensity(ch0, ch1, &self.ms_used, ms_present);
        }
        ics::apply_tns(ch0, sf_index);
        ics::apply_tns(ch1, sf_index);
        let first = self.layout.as_ref().unwrap().elements[slot].first_channel;
        let (out0, out1) = {
            let (a, b) = self.core_out.split_at_mut(first + 1);
            (&mut a[first], &mut b[0])
        };
        let states = &mut self.slots[slot].channels;
        let (s0, s1) = states.split_at_mut(1);
        let [ch0, ch1] = &self.ch;
        self.fb.synthesize(&ch0.spec, ch0.info.window_sequence, ch0.info.window_shape, &mut s0[0], out0);
        self.fb.synthesize(&ch1.spec, ch1.info.window_sequence, ch1.info.window_shape, &mut s1[0], out1);
        Ok(())
    }

    /// Parses (and discards) a coupling channel element.
    fn skip_cce(&mut self, r: &mut BitReader) -> Result<(), Error> {
        r.skip(4);
        let ind_sw = r.read_bit();
        let num_coupled = r.read(3) as usize;
        let mut num_gain_lists = 0;
        for _ in 0..=num_coupled {
            num_gain_lists += 1;
            let is_cpe = r.read_bit();
            r.skip(4);
            if is_cpe {
                let l = r.read_bit();
                let rr = r.read_bit();
                if l && rr {
                    num_gain_lists += 1;
                }
            }
        }
        r.skip(1 + 1 + 2); // cc_domain, gain_element_sign, gain_element_scale
        let sf_index = self.sf_index;
        ics::parse_ics(r, &mut self.ch[1], sf_index, None)?;
        let books = huffman::books();
        let ch = &self.ch[1];
        for _ in 1..num_gain_lists {
            let cge = if ind_sw { true } else { r.read_bit() };
            if cge {
                books.scalefactor.decode(r).ok_or(Error::InvalidData("bad CCE gain code"))?;
            } else {
                for g in 0..ch.info.num_groups {
                    for sfb in 0..ch.info.max_sfb {
                        if ch.band_type[g * MAX_SFB + sfb] != ics::ZERO_HCB {
                            books.scalefactor.decode(r).ok_or(Error::InvalidData("bad CCE gain code"))?;
                        }
                    }
                }
            }
            r.check()?;
        }
        Ok(())
    }
}

#[inline]
fn clamp_sample(v: f32) -> f32 {
    if v.is_nan() {
        0.0
    } else {
        v.clamp(-1.0, 1.0)
    }
}
