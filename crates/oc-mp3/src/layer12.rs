//! Layer I and Layer II decoding (bit allocation, scale factors, sample
//! dequantization incl. grouped codes, joint-stereo band sharing).
//!
//! Ported from minimp3 (<https://github.com/lieff/minimp3>), dedicated to the
//! public domain under CC0 1.0 by its author(s). The allocation tables
//! follow ISO/IEC 11172-3 tables B.2 / 13818-3 table B.1.

use crate::bits::BitReader;
use crate::header::{ChannelMode, FrameHeader};
use crate::synth::SynthState;

/// One run of subbands sharing an allocation code table.
#[derive(Clone, Copy)]
struct Alloc {
    tab_offset: u8,
    code_width: u8,
    band_count: u8,
}

const fn a(tab_offset: u8, code_width: u8, band_count: u8) -> Alloc {
    Alloc {
        tab_offset,
        code_width,
        band_count,
    }
}

const ALLOC_L1: [Alloc; 1] = [a(76, 4, 32)];
const ALLOC_L2_LSF: [Alloc; 3] = [a(60, 4, 4), a(44, 3, 7), a(44, 2, 19)];
const ALLOC_L2: [Alloc; 4] = [a(0, 4, 3), a(16, 4, 8), a(32, 3, 12), a(40, 2, 7)];
const ALLOC_L2_LOW: [Alloc; 2] = [a(44, 4, 2), a(44, 3, 10)];

/// Allocation code -> quantizer: 0 = none, 2..=16 bits per sample,
/// 17/18/19 = grouped 3/5/9 levels.
const BITALLOC_CODES: [u8; 92] = [
    0, 17, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, //
    0, 17, 18, 3, 19, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 16, //
    0, 17, 18, 3, 19, 4, 5, 16, //
    0, 17, 18, 16, //
    0, 17, 18, 19, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, //
    0, 17, 18, 3, 19, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, //
    0, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16,
];

macro_rules! dq {
    ($($x:expr),*) => {
        [$(9.53674316e-07 / $x, 7.56931807e-07 / $x, 6.00777173e-07 / $x),*]
    };
}

/// Dequantization step per quantizer (from 2 bits on) and scale factor
/// fraction (2^0, 2^-1/3, 2^-2/3), scaled to the synthesis output range.
const DEQ: [f32; 54] = dq!(
    3., 7., 15., 31., 63., 127., 255., 511., 1023., 2047., 4095., 8191., 16383., 32767., 65535.,
    3., 5., 9.
);

struct ScaleInfo {
    scf: [f32; 192],
    total_bands: usize,
    stereo_bands: usize,
    bitalloc: [u8; 64],
    scfcod: [u8; 64],
}

fn alloc_table(h: &FrameHeader) -> (&'static [Alloc], usize, usize) {
    let stereo_bands = match h.mode {
        ChannelMode::Mono => 0,
        ChannelMode::JointStereo => ((h.mode_ext as usize) << 2) + 4,
        _ => 32,
    };
    let (alloc, nbands): (&'static [Alloc], usize) = if h.layer == 1 {
        (&ALLOC_L1, 32)
    } else if !h.is_mpeg1() {
        (&ALLOC_L2_LSF, 30)
    } else {
        let mut kbps = h.bitrate_kbps() >> u32::from(h.mode != ChannelMode::Mono);
        if kbps == 0 {
            kbps = 192; // free format
        }
        if kbps < 56 {
            (&ALLOC_L2_LOW, if h.sample_rate_index == 2 { 12 } else { 8 })
        } else if kbps >= 96 && h.sample_rate_index != 1 {
            (&ALLOC_L2, 30)
        } else {
            (&ALLOC_L2, 27)
        }
    };
    (alloc, nbands, stereo_bands.min(nbands))
}

fn read_scale_info(h: &FrameHeader, br: &mut BitReader) -> ScaleInfo {
    let (alloc, total_bands, stereo_bands) = alloc_table(h);
    let mut sci = ScaleInfo {
        scf: [0.0; 192],
        total_bands,
        stereo_bands,
        bitalloc: [0; 64],
        scfcod: [0; 64],
    };
    let mut k = 0;
    let mut ai = 0;
    let mut bits = 0;
    let mut code_off = 0;
    for i in 0..total_bands {
        if i == k {
            let al = alloc[ai.min(alloc.len() - 1)];
            k += al.band_count as usize;
            bits = u32::from(al.code_width);
            code_off = al.tab_offset as usize;
            ai += 1;
        }
        let ba = BITALLOC_CODES[code_off + br.read(bits) as usize];
        sci.bitalloc[2 * i] = ba;
        let ba1 = if i < stereo_bands {
            BITALLOC_CODES[code_off + br.read(bits) as usize]
        } else {
            ba
        };
        sci.bitalloc[2 * i + 1] = if stereo_bands > 0 { ba1 } else { 0 };
    }
    for i in 0..2 * total_bands {
        sci.scfcod[i] = if sci.bitalloc[i] == 0 {
            6
        } else if h.layer == 1 {
            2
        } else {
            br.read(2) as u8
        };
    }
    for i in 0..2 * total_bands {
        let ba = sci.bitalloc[i] as usize;
        let mask = if ba != 0 {
            4 + ((19 >> sci.scfcod[i]) & 3)
        } else {
            0
        };
        let mut s = 0f32;
        for (j, m) in [4, 2, 1].into_iter().enumerate() {
            if mask & m != 0 {
                let b = br.read(6) as usize;
                s = DEQ[ba * 3 - 6 + b % 3] * ((1u32 << 21) >> (b / 3)) as f32;
            }
            sci.scf[3 * i + j] = s;
        }
    }
    for i in stereo_bands..total_bands {
        sci.bitalloc[2 * i + 1] = 0;
    }
    sci
}

/// Reads `4 * group` samples per allocated subband starting at time slot
/// `t0` of `g`.
fn dequantize_granule(
    g: &mut [f32; 1152],
    t0: usize,
    br: &mut BitReader,
    sci: &ScaleInfo,
    group: usize,
) -> usize {
    for j in 0..4 {
        for i in 0..2 * sci.total_bands {
            let ba = u32::from(sci.bitalloc[i]);
            if ba == 0 {
                continue;
            }
            let dst = (i & 1) * 576 + (i >> 1) * 18 + t0 + group * j;
            if ba < 17 {
                let half = (1i32 << (ba - 1)) - 1;
                for k in 0..group {
                    g[dst + k] = (br.read(ba) as i32 - half) as f32;
                }
            } else {
                let m = (2u32 << (ba - 17)) + 1; // 3, 5, 9
                let mut code = br.read(m + 2 - (m >> 3)); // 5, 7, 10 bits
                for k in 0..group {
                    g[dst + k] = ((code % m) as i32 - (m / 2) as i32) as f32;
                    code /= m;
                }
            }
        }
    }
    group * 4
}

fn apply_scf_384(sci: &ScaleInfo, part: usize, g: &mut [f32; 1152]) {
    let sb = sci.stereo_bands * 18;
    let n = (sci.total_bands - sci.stereo_bands) * 18;
    g.copy_within(sb..sb + n, 576 + sb);
    for i in 0..sci.total_bands {
        let s0 = sci.scf[i * 6 + part];
        let s1 = sci.scf[i * 6 + 3 + part];
        for k in 0..12 {
            g[i * 18 + k] *= s0;
            g[576 + i * 18 + k] *= s1;
        }
    }
}

/// Decodes one Layer I/II frame into `pcm`. Returns false if the frame was
/// truncated/corrupt (the remainder is silence).
pub(crate) fn decode_frame(
    syn: &mut SynthState,
    h: &FrameHeader,
    frame: &[u8],
    pcm: &mut [f32],
) -> bool {
    let nch = h.channels();
    let start = 4 + if h.crc { 2 } else { 0 };
    let data = frame.get(start..).unwrap_or(&[]);
    let limit = data.len() * 8;
    let mut br = BitReader::new(data);
    let sci = read_scale_info(h, &mut br);
    let group = if h.layer == 1 { 1 } else { 3 };
    syn.grbuf.fill(0.0);
    let mut t = 0;
    let mut out = 0;
    for part in 0..3 {
        t += dequantize_granule(&mut syn.grbuf, t, &mut br, &sci, group);
        if br.position() > limit {
            pcm[out..].fill(0.0);
            return false;
        }
        if t == 12 {
            t = 0;
            apply_scf_384(&sci, part, &mut syn.grbuf);
            syn.run(12, nch, &mut pcm[out..out + 384 * nch]);
            syn.grbuf.fill(0.0);
            out += 384 * nch;
        }
    }
    true
}
