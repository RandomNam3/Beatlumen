//! Frame header parsing (RFC 9639 section 9.1) and frame decoding: subframes
//! (9.2), coded residuals (9.2.7), prediction and stereo decorrelation (4.2).
//!
//! Decoding works on a byte slice that starts with the frame header and
//! contains (at least) the complete frame. If the slice ends early,
//! [`FrameError::Eof`] is returned so the caller can supply more data and
//! retry.

use crate::bitreader::{BitReader, Eof};
use crate::crc::{crc16, crc8};

/// Why a frame could not be decoded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrameError {
    /// The input ended before the frame was complete.
    Eof,
    /// Not a frame header, or corrupt / unsupported frame content.
    Invalid(&'static str),
    /// Frame footer CRC-16 mismatch.
    Crc,
}

impl From<Eof> for FrameError {
    fn from(_: Eof) -> Self {
        FrameError::Eof
    }
}

/// Longest possible frame header: sync (2) + codes (2) + coded number (7) +
/// uncommon block size (2) + uncommon sample rate (2) + CRC-8 (1).
pub const MAX_HEADER_LEN: usize = 16;

/// A parsed frame header.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FrameHeader {
    /// Blocking strategy bit: `true` = variable block size (coded number is
    /// a sample number), `false` = fixed (coded number is a frame number).
    pub variable: bool,
    pub block_size: u32,
    /// Sample rate in Hz, 0 when only given by STREAMINFO.
    pub sample_rate: u32,
    /// Raw channel assignment code (0..=10).
    pub assignment: u8,
    pub channels: u32,
    /// Bits per sample, 0 when only given by STREAMINFO.
    pub bps: u32,
    /// Frame number (fixed block size) or sample number (variable).
    pub number: u64,
    /// Header length in bytes including the CRC-8.
    pub len: usize,
}

impl FrameHeader {
    /// Number of the first sample of the frame.
    pub fn first_sample(&self, fixed_block_size: u32) -> u64 {
        if self.variable {
            self.number
        } else {
            self.number.saturating_mul(fixed_block_size as u64)
        }
    }
}

/// Parses a frame header at the start of `d` and verifies its CRC-8.
pub fn parse_header(d: &[u8]) -> Result<FrameHeader, FrameError> {
    use FrameError::Invalid;
    if d.len() < 2 {
        return Err(FrameError::Eof);
    }
    if d[0] != 0xff || d[1] & 0xfe != 0xf8 {
        return Err(Invalid("no frame sync"));
    }
    if d.len() < 5 {
        return Err(FrameError::Eof);
    }
    let variable = d[1] & 1 != 0;
    let bs_code = d[2] >> 4;
    let sr_code = d[2] & 15;
    let assignment = d[3] >> 4;
    let bps_code = (d[3] >> 1) & 7;
    if bs_code == 0 {
        return Err(Invalid("reserved block size code"));
    }
    if sr_code == 15 {
        return Err(Invalid("forbidden sample rate code"));
    }
    if assignment > 10 {
        return Err(Invalid("reserved channel assignment"));
    }
    if bps_code == 3 {
        return Err(Invalid("reserved bit depth code"));
    }
    if d[3] & 1 != 0 {
        return Err(Invalid("reserved header bit set"));
    }

    // Coded number (UTF-8 like, up to 7 bytes / 36 bits).
    let first = d[4];
    let ones = first.leading_ones();
    let (mut number, extra) = match ones {
        0 => (first as u64, 0usize),
        2..=7 => ((first & (0x7f >> ones)) as u64, ones as usize - 1),
        _ => return Err(Invalid("bad coded number")),
    };
    let mut i = 5;
    for _ in 0..extra {
        let b = *d.get(i).ok_or(FrameError::Eof)?;
        if b & 0xc0 != 0x80 {
            return Err(Invalid("bad coded number"));
        }
        number = (number << 6) | (b & 0x3f) as u64;
        i += 1;
    }
    if !variable && number >= 1 << 31 {
        return Err(Invalid("frame number too large"));
    }

    let block_size = match bs_code {
        1 => 192,
        2..=5 => 144u32 << bs_code,
        6 => {
            let b = *d.get(i).ok_or(FrameError::Eof)?;
            i += 1;
            b as u32 + 1
        }
        7 => {
            let b = d.get(i..i + 2).ok_or(FrameError::Eof)?;
            i += 2;
            let v = u16::from_be_bytes([b[0], b[1]]) as u32 + 1;
            if v == 65536 {
                return Err(Invalid("forbidden block size"));
            }
            v
        }
        _ => 1u32 << bs_code,
    };

    let sample_rate = match sr_code {
        0 => 0,
        1 => 88200,
        2 => 176400,
        3 => 192000,
        4 => 8000,
        5 => 16000,
        6 => 22050,
        7 => 24000,
        8 => 32000,
        9 => 44100,
        10 => 48000,
        11 => 96000,
        12 => {
            let b = *d.get(i).ok_or(FrameError::Eof)?;
            i += 1;
            b as u32 * 1000
        }
        _ => {
            let b = d.get(i..i + 2).ok_or(FrameError::Eof)?;
            i += 2;
            let v = u16::from_be_bytes([b[0], b[1]]) as u32;
            if sr_code == 14 {
                v * 10
            } else {
                v
            }
        }
    };

    let crc = *d.get(i).ok_or(FrameError::Eof)?;
    if crc8(&d[..i]) != crc {
        return Err(Invalid("frame header CRC mismatch"));
    }
    let bps = match bps_code {
        0 => 0,
        1 => 8,
        2 => 12,
        4 => 16,
        5 => 20,
        6 => 24,
        _ => 32,
    };
    let channels = if assignment < 8 {
        assignment as u32 + 1
    } else {
        2
    };
    Ok(FrameHeader {
        variable,
        block_size,
        sample_rate,
        assignment,
        channels,
        bps,
        number,
        len: i + 1,
    })
}

/// Reusable frame decoder holding the decoded samples of the last frame.
#[derive(Debug, Default)]
pub struct FrameDecoder {
    /// Decoded samples per channel (only the first `block_size` entries of
    /// the first `channels` vectors are meaningful).
    chans: Vec<Vec<i32>>,
    /// Side channel of 32-bit stereo (33-bit samples).
    wide: Vec<i64>,
    /// Residual scratch buffer for the wide path.
    resid: Vec<i32>,
    pub block_size: usize,
    pub channels: usize,
    pub bps: u32,
}

impl FrameDecoder {
    pub fn new() -> Self {
        Self::default()
    }

    /// Decodes the frame at the start of `data` whose header `hdr` was parsed
    /// from the same bytes. `bps` is the effective bit depth (from the header
    /// or STREAMINFO). Returns the total frame length in bytes. On error the
    /// previously decoded frame is invalidated.
    pub fn decode(
        &mut self,
        data: &[u8],
        hdr: &FrameHeader,
        bps: u32,
    ) -> Result<usize, FrameError> {
        self.block_size = 0;
        if !(4..=32).contains(&bps) {
            return Err(FrameError::Invalid("unsupported bit depth"));
        }
        let bs = hdr.block_size as usize;
        let nch = hdr.channels as usize;
        if self.chans.len() < nch {
            self.chans.resize_with(nch, Vec::new);
        }
        for c in &mut self.chans[..nch] {
            if c.len() < bs {
                c.resize(bs, 0);
            }
        }
        let body = data.get(hdr.len..).ok_or(FrameError::Eof)?;
        let mut br = BitReader::new(body);
        let mut wide_used = false;
        for c in 0..nch {
            let side = matches!((hdr.assignment, c), (8, 1) | (9, 0) | (10, 1));
            let sbps = bps + side as u32;
            if sbps > 32 {
                if self.wide.len() < bs {
                    self.wide.resize(bs, 0);
                    self.resid.resize(bs, 0);
                }
                decode_subframe_wide(&mut br, sbps, &mut self.wide[..bs], &mut self.resid[..bs])?;
                wide_used = true;
            } else {
                decode_subframe(&mut br, sbps, &mut self.chans[c][..bs])?;
            }
        }
        br.align();
        let end = hdr.len + br.byte_pos();
        let footer = data.get(end..end + 2).ok_or(FrameError::Eof)?;
        if crc16(&data[..end]) != u16::from_be_bytes([footer[0], footer[1]]) {
            return Err(FrameError::Crc);
        }
        if hdr.assignment >= 8 {
            let (a, b) = self.chans.split_at_mut(1);
            let (c0, c1) = (&mut a[0][..bs], &mut b[0][..bs]);
            if wide_used {
                decorrelate_wide(hdr.assignment, c0, c1, &self.wide[..bs]);
            } else {
                decorrelate(hdr.assignment, c0, c1);
            }
        }
        self.block_size = bs;
        self.channels = nch;
        self.bps = bps;
        Ok(end + 2)
    }

    /// Appends samples `start..block_size` of the last frame as interleaved
    /// `f32` normalised by 2^(bps-1).
    pub fn write_f32(&self, start: usize, out: &mut Vec<f32>) {
        let (bs, nch) = (self.block_size, self.channels);
        if start >= bs || nch == 0 {
            return;
        }
        let n = bs - start;
        let scale = 1.0 / (1u64 << (self.bps - 1)) as f32;
        let base = out.len();
        out.resize(base + n * nch, 0.0);
        let dst = &mut out[base..];
        match nch {
            1 => {
                for (d, &s) in dst.iter_mut().zip(&self.chans[0][start..bs]) {
                    *d = s as f32 * scale;
                }
            }
            2 => {
                let l = &self.chans[0][start..bs];
                let r = &self.chans[1][start..bs];
                for (d, (&a, &b)) in dst.chunks_exact_mut(2).zip(l.iter().zip(r)) {
                    d[0] = a as f32 * scale;
                    d[1] = b as f32 * scale;
                }
            }
            _ => {
                for (c, ch) in self.chans[..nch].iter().enumerate() {
                    for (d, &s) in dst[c..].iter_mut().step_by(nch).zip(&ch[start..bs]) {
                        *d = s as f32 * scale;
                    }
                }
            }
        }
    }

    /// Appends samples `start..block_size` of the last frame as interleaved
    /// integers at the frame's native bit depth.
    pub fn write_i32(&self, start: usize, out: &mut Vec<i32>) {
        let (bs, nch) = (self.block_size, self.channels);
        if start >= bs || nch == 0 {
            return;
        }
        let base = out.len();
        out.resize(base + (bs - start) * nch, 0);
        let dst = &mut out[base..];
        for (c, ch) in self.chans[..nch].iter().enumerate() {
            for (d, &s) in dst[c..].iter_mut().step_by(nch).zip(&ch[start..bs]) {
                *d = s;
            }
        }
    }
}

/// Reads a subframe header; returns (type code, wasted bits).
#[inline]
fn subframe_header(br: &mut BitReader, bps: u32) -> Result<(u32, u32), FrameError> {
    let h = br.read(8)?;
    if h & 0x80 != 0 {
        return Err(FrameError::Invalid("subframe padding bit set"));
    }
    let wasted = if h & 1 != 0 {
        br.read_unary()?.saturating_add(1)
    } else {
        0
    };
    if wasted >= bps {
        return Err(FrameError::Invalid("too many wasted bits"));
    }
    Ok(((h >> 1) & 0x3f, wasted))
}

/// Reads the LPC parameters following the warm-up samples: returns
/// (precision, shift) and fills `coefs[..order]`.
#[inline]
fn lpc_params(br: &mut BitReader, coefs: &mut [i32]) -> Result<(u32, u32), FrameError> {
    let precision = br.read(4)? + 1;
    if precision == 16 {
        return Err(FrameError::Invalid("invalid LPC coefficient precision"));
    }
    let shift = br.read_signed(5)?;
    if shift < 0 {
        return Err(FrameError::Invalid("negative LPC shift"));
    }
    for c in coefs.iter_mut() {
        *c = br.read_signed(precision)?;
    }
    Ok((precision, shift as u32))
}

/// Decodes one subframe with at most 32 bits per sample into `out`
/// (`out.len()` = block size).
fn decode_subframe(br: &mut BitReader, bps: u32, out: &mut [i32]) -> Result<(), FrameError> {
    let bs = out.len();
    let (typ, wasted) = subframe_header(br, bps)?;
    let sbps = bps - wasted;
    match typ {
        0 => {
            let v = br.read_signed(sbps)?;
            out.fill(v);
        }
        1 => {
            for o in out.iter_mut() {
                *o = br.read_signed(sbps)?;
            }
        }
        8..=12 => {
            let order = (typ - 8) as usize;
            if order > bs {
                return Err(FrameError::Invalid("predictor order exceeds block size"));
            }
            for o in &mut out[..order] {
                *o = br.read_signed(sbps)?;
            }
            decode_residual(br, order, out)?;
            restore_fixed(order, out);
        }
        32..=63 => {
            let order = (typ - 31) as usize;
            if order > bs {
                return Err(FrameError::Invalid("predictor order exceeds block size"));
            }
            for o in &mut out[..order] {
                *o = br.read_signed(sbps)?;
            }
            let mut coefs = [0i32; 32];
            let (_, shift) = lpc_params(br, &mut coefs[..order])?;
            decode_residual(br, order, out)?;
            restore_lpc(&coefs[..order], shift, sbps, out);
        }
        _ => return Err(FrameError::Invalid("reserved subframe type")),
    }
    if wasted > 0 {
        for o in out.iter_mut() {
            *o <<= wasted;
        }
    }
    Ok(())
}

/// Decodes one subframe of up to 33 bits per sample (the side channel of
/// 32-bit stereo) using 64-bit arithmetic.
fn decode_subframe_wide(
    br: &mut BitReader,
    bps: u32,
    out: &mut [i64],
    resid: &mut [i32],
) -> Result<(), FrameError> {
    let bs = out.len();
    let (typ, wasted) = subframe_header(br, bps)?;
    let sbps = bps - wasted;
    match typ {
        0 => {
            let v = br.read_signed_i64(sbps)?;
            out.fill(v);
        }
        1 => {
            for o in out.iter_mut() {
                *o = br.read_signed_i64(sbps)?;
            }
        }
        8..=12 => {
            let order = (typ - 8) as usize;
            if order > bs {
                return Err(FrameError::Invalid("predictor order exceeds block size"));
            }
            for o in &mut out[..order] {
                *o = br.read_signed_i64(sbps)?;
            }
            decode_residual(br, order, resid)?;
            for i in order..bs {
                let p = match order {
                    0 => 0,
                    1 => out[i - 1],
                    2 => out[i - 1].wrapping_mul(2).wrapping_sub(out[i - 2]),
                    3 => out[i - 1]
                        .wrapping_sub(out[i - 2])
                        .wrapping_mul(3)
                        .wrapping_add(out[i - 3]),
                    _ => out[i - 1]
                        .wrapping_add(out[i - 3])
                        .wrapping_mul(4)
                        .wrapping_sub(out[i - 2].wrapping_mul(6))
                        .wrapping_sub(out[i - 4]),
                };
                out[i] = p.wrapping_add(resid[i] as i64);
            }
        }
        32..=63 => {
            let order = (typ - 31) as usize;
            if order > bs {
                return Err(FrameError::Invalid("predictor order exceeds block size"));
            }
            for o in &mut out[..order] {
                *o = br.read_signed_i64(sbps)?;
            }
            let mut coefs = [0i32; 32];
            let (_, shift) = lpc_params(br, &mut coefs[..order])?;
            decode_residual(br, order, resid)?;
            for i in order..bs {
                let mut sum = 0i64;
                for (j, &c) in coefs[..order].iter().enumerate() {
                    sum = sum.wrapping_add((c as i64).wrapping_mul(out[i - 1 - j]));
                }
                out[i] = (sum >> shift).wrapping_add(resid[i] as i64);
            }
        }
        _ => return Err(FrameError::Invalid("reserved subframe type")),
    }
    if wasted > 0 {
        for o in out.iter_mut() {
            *o <<= wasted;
        }
    }
    Ok(())
}

/// Decodes the coded residual of a subframe into `out[order..]`.
fn decode_residual(br: &mut BitReader, order: usize, out: &mut [i32]) -> Result<(), FrameError> {
    let bs = out.len();
    let param_bits = match br.read(2)? {
        0 => 4,
        1 => 5,
        _ => return Err(FrameError::Invalid("reserved residual coding method")),
    };
    let escape = (1u32 << param_bits) - 1;
    let po = br.read(4)?;
    let psize = bs >> po;
    if psize << po != bs || psize < order {
        return Err(FrameError::Invalid("invalid partition order"));
    }
    let mut start = order;
    for p in 1..=(1usize << po) {
        let end = p * psize;
        let part = &mut out[start..end];
        let k = br.read(param_bits)?;
        if k == escape {
            let n = br.read(5)?;
            if n == 0 {
                part.fill(0);
            } else {
                for x in part.iter_mut() {
                    *x = br.read_signed(n)?;
                }
            }
        } else {
            br.read_rice_block(part, k)?;
        }
        start = end;
    }
    Ok(())
}

/// Undoes fixed prediction in place (`s[..order]` are warm-up samples, the
/// rest residuals). Wrapping 32-bit arithmetic is exact whenever the
/// decoded samples fit 32 bits, which holds for valid streams.
fn restore_fixed(order: usize, s: &mut [i32]) {
    if s.len() <= order {
        return;
    }
    match order {
        1 => {
            let mut a = s[0];
            for x in &mut s[1..] {
                a = x.wrapping_add(a);
                *x = a;
            }
        }
        2 => {
            let (mut a, mut b) = (s[1], s[0]);
            for x in &mut s[2..] {
                let v = x.wrapping_add(a.wrapping_mul(2).wrapping_sub(b));
                *x = v;
                b = a;
                a = v;
            }
        }
        3 => {
            let (mut a, mut b, mut c) = (s[2], s[1], s[0]);
            for x in &mut s[3..] {
                let p = a.wrapping_sub(b).wrapping_mul(3).wrapping_add(c);
                let v = x.wrapping_add(p);
                *x = v;
                c = b;
                b = a;
                a = v;
            }
        }
        4 => {
            let (mut a, mut b, mut c, mut d) = (s[3], s[2], s[1], s[0]);
            for x in &mut s[4..] {
                let p = a
                    .wrapping_add(c)
                    .wrapping_mul(4)
                    .wrapping_sub(b.wrapping_mul(6))
                    .wrapping_sub(d);
                let v = x.wrapping_add(p);
                *x = v;
                d = c;
                c = b;
                b = a;
                a = v;
            }
        }
        _ => {}
    }
}

/// Undoes LPC prediction in place. Uses a 32-bit accumulator when the
/// prediction provably fits (|sum| <= sum|coef| * 2^(bps-1) < 2^31), a
/// 64-bit accumulator otherwise.
fn restore_lpc(coefs: &[i32], shift: u32, bps: u32, s: &mut [i32]) {
    let sum_abs: u64 = coefs.iter().map(|c| c.unsigned_abs() as u64).sum();
    let fits32 = (sum_abs << (bps - 1)) < (1u64 << 31);
    macro_rules! dispatch {
        ($f:ident; $($n:literal)*) => {
            match coefs.len() {
                $($n => $f::<$n>(coefs, shift, s),)*
                _ => {}
            }
        };
    }
    if fits32 {
        dispatch!(lpc_i32; 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15 16 17 18 19 20 21 22 23 24 25 26 27 28 29 30 31 32);
    } else {
        dispatch!(lpc_i64; 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15 16 17 18 19 20 21 22 23 24 25 26 27 28 29 30 31 32);
    }
}

/// Reverses the coefficient order so that `c[j]` multiplies `s[i - N + j]`.
#[inline(always)]
fn reversed<const N: usize>(coefs: &[i32]) -> [i32; N] {
    let mut c = [0i32; N];
    for (d, &v) in c.iter_mut().zip(coefs.iter().rev()) {
        *d = v;
    }
    c
}

fn lpc_i32<const N: usize>(coefs: &[i32], shift: u32, s: &mut [i32]) {
    let c = reversed::<N>(coefs);
    for i in N..s.len() {
        let Ok(w) = <&[i32; N]>::try_from(&s[i - N..i]) else {
            return;
        };
        let mut sum = 0i32;
        for j in 0..N {
            sum = sum.wrapping_add(c[j].wrapping_mul(w[j]));
        }
        s[i] = s[i].wrapping_add(sum >> shift);
    }
}

fn lpc_i64<const N: usize>(coefs: &[i32], shift: u32, s: &mut [i32]) {
    let c = reversed::<N>(coefs);
    for i in N..s.len() {
        let Ok(w) = <&[i32; N]>::try_from(&s[i - N..i]) else {
            return;
        };
        let mut sum = 0i64;
        for j in 0..N {
            sum = sum.wrapping_add(c[j] as i64 * w[j] as i64);
        }
        s[i] = s[i].wrapping_add((sum >> shift) as i32);
    }
}

/// Restores left/right from left-side (8), side-right (9) or mid-side (10).
fn decorrelate(assignment: u8, c0: &mut [i32], c1: &mut [i32]) {
    match assignment {
        8 => {
            for (l, s) in c0.iter().zip(c1.iter_mut()) {
                *s = l.wrapping_sub(*s);
            }
        }
        9 => {
            for (s, r) in c0.iter_mut().zip(c1.iter()) {
                *s = s.wrapping_add(*r);
            }
        }
        _ => {
            for (m, s) in c0.iter_mut().zip(c1.iter_mut()) {
                let side = *s;
                // left = mid + ceil(side / 2) (exact, no 33-bit intermediate)
                let l = m.wrapping_add(side >> 1).wrapping_add(side & 1);
                *m = l;
                *s = l.wrapping_sub(side);
            }
        }
    }
}

/// Like [`decorrelate`] for 32-bit audio where the side channel (33 bits)
/// lives in `side`.
fn decorrelate_wide(assignment: u8, c0: &mut [i32], c1: &mut [i32], side: &[i64]) {
    match assignment {
        8 => {
            for ((l, r), &s) in c0.iter().zip(c1.iter_mut()).zip(side) {
                *r = (*l as i64).wrapping_sub(s) as i32;
            }
        }
        9 => {
            for ((l, r), &s) in c0.iter_mut().zip(c1.iter()).zip(side) {
                *l = s.wrapping_add(*r as i64) as i32;
            }
        }
        _ => {
            for ((m, r), &s) in c0.iter_mut().zip(c1.iter_mut()).zip(side) {
                let l = (*m as i64).wrapping_add(s >> 1).wrapping_add(s & 1);
                *m = l as i32;
                *r = l.wrapping_sub(s) as i32;
            }
        }
    }
}
