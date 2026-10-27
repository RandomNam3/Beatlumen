//! individual_channel_stream parsing (ics_info, section data, scalefactors,
//! pulse data, TNS data, spectral data) and inverse quantisation
//! (ISO/IEC 14496-3 4.4.2.7, 4.6.1 - 4.6.3).

use crate::bits::BitReader;
use crate::filterbank::EIGHT_SHORT;
use crate::huffman::{books, Books};
use crate::tables::{swb_offset_long, swb_offset_short, tns_max_bands};
use crate::Error;
use std::sync::OnceLock;

pub(crate) const ZERO_HCB: u8 = 0;
pub(crate) const ESC_HCB: u8 = 11;
pub(crate) const NOISE_HCB: u8 = 13;
pub(crate) const INTENSITY_HCB2: u8 = 14;
pub(crate) const INTENSITY_HCB: u8 = 15;

/// Maximum scalefactor bands per group (long windows have at most 51).
pub(crate) const MAX_SFB: usize = 64;

const TNS_MAX_ORDER_LONG: usize = 12;
const TNS_MAX_ORDER_SHORT: usize = 7;

#[derive(Clone, Copy)]
pub(crate) struct IcsInfo {
    pub window_sequence: u8,
    pub window_shape: u8,
    pub max_sfb: usize,
    pub num_windows: usize,
    pub num_groups: usize,
    pub group_len: [u8; 8],
    pub num_swb: usize,
    pub swb: &'static [u16],
}

impl Default for IcsInfo {
    fn default() -> Self {
        IcsInfo {
            window_sequence: 0,
            window_shape: 0,
            max_sfb: 0,
            num_windows: 1,
            num_groups: 1,
            group_len: [1, 0, 0, 0, 0, 0, 0, 0],
            num_swb: 0,
            swb: &[0],
        }
    }
}

impl IcsInfo {
    pub fn is_short(&self) -> bool {
        self.window_sequence == EIGHT_SHORT
    }

    pub fn parse(r: &mut BitReader, sf_index: usize) -> Result<IcsInfo, Error> {
        if r.read_bit() {
            return Err(Error::InvalidData("ics reserved bit set"));
        }
        let window_sequence = r.read(2) as u8;
        let window_shape = r.read(1) as u8;
        let mut info = IcsInfo { window_sequence, window_shape, ..Default::default() };
        if window_sequence == EIGHT_SHORT {
            info.max_sfb = r.read(4) as usize;
            let grouping = r.read(7);
            info.num_windows = 8;
            info.num_groups = 1;
            info.group_len = [1, 0, 0, 0, 0, 0, 0, 0];
            for i in 0..7 {
                if grouping & (1 << (6 - i)) != 0 {
                    info.group_len[info.num_groups - 1] += 1;
                } else {
                    info.num_groups += 1;
                    info.group_len[info.num_groups - 1] = 1;
                }
            }
            info.swb = swb_offset_short(sf_index);
        } else {
            info.max_sfb = r.read(6) as usize;
            if r.read_bit() {
                return Err(Error::Unsupported("predictor data (AAC Main/LTP)"));
            }
            info.swb = swb_offset_long(sf_index);
        }
        info.num_swb = info.swb.len() - 1;
        if info.max_sfb > info.num_swb {
            return Err(Error::InvalidData("max_sfb exceeds number of scalefactor bands"));
        }
        Ok(info)
    }
}

#[derive(Clone, Copy, Default)]
pub(crate) struct TnsFilter {
    pub length: u8,
    pub order: u8,
    pub direction: bool,
    /// LPC coefficients a[1..=order] (a[0] = 1 implied).
    pub lpc: [f32; 20],
}

#[derive(Clone, Default)]
pub(crate) struct Tns {
    pub present: bool,
    pub n_filt: [u8; 8],
    pub filters: [[TnsFilter; 4]; 8],
}

/// Decoded data of one channel of an element.
pub(crate) struct ChannelData {
    pub info: IcsInfo,
    pub global_gain: i32,
    /// Band type per `g * MAX_SFB + sfb`.
    pub band_type: [u8; 8 * MAX_SFB],
    /// Scalefactor / intensity position / noise energy per band.
    pub sf: [i32; 8 * MAX_SFB],
    pub tns: Tns,
    pub spec: Vec<f32>,
    quant: Vec<i32>,
}

impl ChannelData {
    pub fn new() -> Self {
        ChannelData {
            info: IcsInfo::default(),
            global_gain: 0,
            band_type: [0; 8 * MAX_SFB],
            sf: [0; 8 * MAX_SFB],
            tns: Tns::default(),
            spec: vec![0.0; 1024],
            quant: vec![0; 1024],
        }
    }
}

struct DequantTables {
    pow43: Vec<f32>,
    /// 2^((sf - 100) / 4) for sf in 0..=255.
    gain: [f32; 256],
}

fn dequant_tables() -> &'static DequantTables {
    static T: OnceLock<DequantTables> = OnceLock::new();
    T.get_or_init(|| {
        let pow43 = (0..8192).map(|i| (i as f64).powf(4.0 / 3.0) as f32).collect();
        let mut gain = [0f32; 256];
        for (i, g) in gain.iter_mut().enumerate() {
            *g = 2f64.powf((i as f64 - 100.0) / 4.0) as f32;
        }
        DequantTables { pow43, gain }
    })
}

#[inline]
fn pow43(t: &DequantTables, q: i32) -> f32 {
    let a = q.unsigned_abs() as usize;
    let v = if a < t.pow43.len() { t.pow43[a] } else { (a as f32).powf(4.0 / 3.0) };
    if q < 0 {
        -v
    } else {
        v
    }
}

/// Parses an individual_channel_stream. With `common` set (CPE with
/// common_window) the ics_info is taken from it instead of the bitstream.
/// Fills `ch.spec` with dequantised coefficients (noise / intensity bands
/// left at zero).
pub(crate) fn parse_ics(
    r: &mut BitReader,
    ch: &mut ChannelData,
    sf_index: usize,
    common: Option<&IcsInfo>,
) -> Result<(), Error> {
    let b = books();
    ch.global_gain = r.read(8) as i32;
    ch.info = match common {
        Some(i) => *i,
        None => IcsInfo::parse(r, sf_index)?,
    };
    let info = ch.info;
    parse_section_data(r, ch)?;
    parse_scalefactors(r, ch, b)?;
    let mut pulses: [(usize, i32); 4] = [(0, 0); 4];
    let mut num_pulses = 0;
    {
        if r.read_bit() {
            if info.is_short() {
                return Err(Error::InvalidData("pulse data in short window"));
            }
            num_pulses = r.read(2) as usize + 1;
            let start_sfb = r.read(6) as usize;
            if start_sfb >= info.num_swb {
                return Err(Error::InvalidData("pulse start sfb"));
            }
            let mut k = info.swb[start_sfb] as usize;
            for p in pulses.iter_mut().take(num_pulses) {
                k += r.read(5) as usize;
                let amp = r.read(4) as i32;
                if k >= 1024 {
                    return Err(Error::InvalidData("pulse offset"));
                }
                *p = (k, amp);
            }
        }
        ch.tns.present = r.read_bit();
        if ch.tns.present {
            parse_tns(r, &mut ch.tns, &info)?;
        }
        if r.read_bit() {
            return Err(Error::Unsupported("gain control data (AAC SSR)"));
        }
    }
    r.check()?;
    parse_spectral_data(r, ch, b)?;
    for &(k, amp) in &pulses[..num_pulses] {
        let q = &mut ch.quant[k];
        if *q > 0 {
            *q += amp;
        } else {
            *q -= amp;
        }
    }
    dequantize(ch);
    r.check()
}

fn parse_section_data(r: &mut BitReader, ch: &mut ChannelData) -> Result<(), Error> {
    let info = ch.info;
    let (bits, esc) = if info.is_short() { (3, 7) } else { (5, 31) };
    for g in 0..info.num_groups {
        let bt = &mut ch.band_type[g * MAX_SFB..(g + 1) * MAX_SFB];
        let mut k = 0;
        while k < info.max_sfb {
            let cb = r.read(4) as u8;
            if cb == 12 {
                return Err(Error::InvalidData("reserved codebook"));
            }
            let mut len = 0usize;
            loop {
                let inc = r.read(bits) as usize;
                len += inc;
                if inc != esc {
                    break;
                }
                if len > info.max_sfb {
                    return Err(Error::InvalidData("section length"));
                }
            }
            if k + len > info.max_sfb {
                return Err(Error::InvalidData("section exceeds max_sfb"));
            }
            r.check()?;
            bt[k..k + len].iter_mut().for_each(|v| *v = cb);
            k += len;
        }
        bt[info.max_sfb..].iter_mut().for_each(|v| *v = ZERO_HCB);
    }
    Ok(())
}

#[inline]
fn read_sf_delta(r: &mut BitReader, b: &Books) -> Result<i32, Error> {
    match b.scalefactor.decode(r) {
        Some(v) => Ok(v as i32 - 60),
        None => Err(Error::InvalidData("invalid scalefactor code")),
    }
}

fn parse_scalefactors(r: &mut BitReader, ch: &mut ChannelData, b: &Books) -> Result<(), Error> {
    let info = ch.info;
    let mut sf = ch.global_gain;
    let mut is_pos = 0i32;
    let mut noise = ch.global_gain - 90;
    let mut noise_first = true;
    for g in 0..info.num_groups {
        for sfb in 0..info.max_sfb {
            let i = g * MAX_SFB + sfb;
            ch.sf[i] = match ch.band_type[i] {
                ZERO_HCB => 0,
                INTENSITY_HCB | INTENSITY_HCB2 => {
                    is_pos += read_sf_delta(r, b)?;
                    is_pos.clamp(-155, 100)
                }
                NOISE_HCB => {
                    if noise_first {
                        noise_first = false;
                        noise += r.read(9) as i32 - 256;
                    } else {
                        noise += read_sf_delta(r, b)?;
                    }
                    noise.clamp(-155, 100)
                }
                _ => {
                    sf += read_sf_delta(r, b)?;
                    if !(0..=255).contains(&sf) {
                        return Err(Error::InvalidData("scalefactor out of range"));
                    }
                    sf
                }
            };
        }
    }
    Ok(())
}

fn parse_tns(r: &mut BitReader, tns: &mut Tns, info: &IcsInfo) -> Result<(), Error> {
    let short = info.is_short();
    let (nf_bits, len_bits, ord_bits, max_order) =
        if short { (1, 4, 3, TNS_MAX_ORDER_SHORT) } else { (2, 6, 5, TNS_MAX_ORDER_LONG) };
    for w in 0..info.num_windows {
        let n_filt = r.read(nf_bits) as usize;
        tns.n_filt[w] = n_filt as u8;
        if n_filt == 0 {
            continue;
        }
        let coef_res = r.read(1);
        for f in 0..n_filt {
            let filt = &mut tns.filters[w][f];
            filt.length = r.read(len_bits) as u8;
            let order = r.read(ord_bits) as usize;
            if order > max_order {
                return Err(Error::InvalidData("tns order"));
            }
            filt.order = order as u8;
            if order == 0 {
                continue;
            }
            filt.direction = r.read_bit();
            let compress = r.read(1);
            let coef_bits = 3 + coef_res - compress;
            let res_bits = 3 + coef_res;
            let iqfac = ((1 << (res_bits - 1)) as f64 - 0.5) / std::f64::consts::FRAC_PI_2;
            let iqfac_m = ((1 << (res_bits - 1)) as f64 + 0.5) / std::f64::consts::FRAC_PI_2;
            let mut parcor = [0f64; 20];
            for p in parcor.iter_mut().take(order) {
                let raw = r.read(coef_bits) as i32;
                // Sign-extend.
                let c = (raw << (32 - coef_bits)) >> (32 - coef_bits);
                *p = (c as f64 / if c >= 0 { iqfac } else { iqfac_m }).sin();
            }
            // Parcor -> LPC (step-up recursion).
            let mut a = [0f64; 21];
            a[0] = 1.0;
            for m in 1..=order {
                let mut b = a;
                for i in 1..m {
                    b[i] = a[i] + parcor[m - 1] * a[m - i];
                }
                b[m] = parcor[m - 1];
                a = b;
            }
            for i in 0..order {
                filt.lpc[i] = a[i + 1] as f32;
            }
        }
    }
    Ok(())
}

fn parse_spectral_data(r: &mut BitReader, ch: &mut ChannelData, b: &Books) -> Result<(), Error> {
    let info = ch.info;
    let q = &mut ch.quant[..];
    q.iter_mut().for_each(|v| *v = 0);
    let wlen = if info.is_short() { 128 } else { 1024 };
    let mut win = 0;
    for g in 0..info.num_groups {
        let glen = info.group_len[g] as usize;
        for sfb in 0..info.max_sfb {
            let cb = ch.band_type[g * MAX_SFB + sfb];
            if cb == ZERO_HCB || cb >= NOISE_HCB {
                continue;
            }
            let book = &b.spectral[cb as usize - 1];
            let start = info.swb[sfb] as usize;
            let end = info.swb[sfb + 1] as usize;
            for w in win..win + glen {
                let base = w * wlen;
                let mut k = base + start;
                let stop = base + end;
                while k < stop {
                    let sym = match book.table.decode(r) {
                        Some(s) => s as usize,
                        None => return Err(Error::InvalidData("invalid spectral code")),
                    };
                    let vals = book.values[sym];
                    let dim = book.dim;
                    let mut v = [0i32; 4];
                    for d in 0..dim {
                        v[d] = vals[d] as i32;
                    }
                    if !book.signed {
                        for x in v.iter_mut().take(dim) {
                            if *x != 0 && r.read_bit() {
                                *x = -*x;
                            }
                        }
                        if cb == ESC_HCB {
                            for x in v.iter_mut().take(2) {
                                if x.abs() == 16 {
                                    let mut n = 4;
                                    while r.read_bit() {
                                        n += 1;
                                        if n > 12 {
                                            return Err(Error::InvalidData("escape sequence too long"));
                                        }
                                    }
                                    let mag = (1i32 << n) + r.read(n) as i32;
                                    *x = if *x < 0 { -mag } else { mag };
                                }
                            }
                        }
                    }
                    if k + dim > q.len() {
                        return Err(Error::InvalidData("spectral overflow"));
                    }
                    q[k..k + dim].copy_from_slice(&v[..dim]);
                    k += dim;
                }
            }
            if r.overrun() {
                return Err(Error::InvalidData("truncated spectral data"));
            }
        }
        win += glen;
    }
    Ok(())
}

fn dequantize(ch: &mut ChannelData) {
    let t = dequant_tables();
    let info = ch.info;
    let wlen = if info.is_short() { 128 } else { 1024 };
    ch.spec.iter_mut().for_each(|v| *v = 0.0);
    let mut win = 0;
    for g in 0..info.num_groups {
        let glen = info.group_len[g] as usize;
        for sfb in 0..info.max_sfb {
            let i = g * MAX_SFB + sfb;
            let cb = ch.band_type[i];
            if cb == ZERO_HCB || cb >= NOISE_HCB {
                continue;
            }
            let gain = t.gain[ch.sf[i].clamp(0, 255) as usize];
            let start = info.swb[sfb] as usize;
            let end = info.swb[sfb + 1] as usize;
            for w in win..win + glen {
                let base = w * wlen;
                for k in base + start..base + end {
                    ch.spec[k] = pow43(t, ch.quant[k]) * gain;
                }
            }
        }
        win += glen;
    }
}

/// Applies TNS synthesis filtering to `ch.spec`.
pub(crate) fn apply_tns(ch: &mut ChannelData, sf_index: usize) {
    if !ch.tns.present {
        return;
    }
    let info = ch.info;
    let short = info.is_short();
    let wlen = if short { 128 } else { 1024 };
    let max_bands = tns_max_bands(sf_index, short).min(info.max_sfb);
    for w in 0..info.num_windows {
        let mut top = info.num_swb;
        for f in 0..ch.tns.n_filt[w] as usize {
            let filt = ch.tns.filters[w][f];
            let bottom = top.saturating_sub(filt.length as usize);
            let order = filt.order as usize;
            let (b0, t0) = (bottom.min(max_bands), top.min(max_bands));
            top = bottom;
            if order == 0 {
                continue;
            }
            let start = info.swb[b0] as usize;
            let end = info.swb[t0] as usize;
            if end <= start {
                continue;
            }
            let x = &mut ch.spec[w * wlen + start..w * wlen + end];
            let a = &filt.lpc[..order];
            let mut state = [0f32; 20];
            // state[0] holds y[n-1], state[1] y[n-2], ...
            let mut filter = |v: &mut f32| {
                let mut y = *v;
                for i in 0..order {
                    y -= a[i] * state[i];
                }
                for i in (1..order).rev() {
                    state[i] = state[i - 1];
                }
                state[0] = y;
                *v = y;
            };
            if filt.direction {
                x.iter_mut().rev().for_each(&mut filter);
            } else {
                x.iter_mut().for_each(&mut filter);
            }
        }
    }
}
