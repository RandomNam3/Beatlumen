//! Baseline / extended sequential and progressive Huffman JPEG decoder.
//!
//! The decoder parses the marker stream, decodes each scan into per-component
//! sample planes (sequential mode: IDCT directly after decoding each block;
//! progressive mode: coefficients are accumulated and transformed at the end)
//! and finally upsamples chroma and converts to RGBA (see `color`).
//!
//! Damaged entropy-coded data does not fail the decode: decoding of the
//! affected scan stops and the remaining area keeps neutral gray.

use super::color::{self, ColorSpace};
use super::huffman::{BitReader, HuffTable, FAST_BITS};
use super::idct::{self, Dequant, ReducedBasis};
use super::ZIGZAG_EXT;
use crate::{exif, Error, Image};

#[derive(Default)]
pub(super) struct Component {
    pub id: u8,
    pub h: usize,
    pub v: usize,
    tq: usize,
    /// Blocks covering the component (without MCU padding).
    bw: usize,
    bh: usize,
    /// Blocks including MCU padding.
    bw_pad: usize,
    bh_pad: usize,
    /// Per-component DCT scaling denominator and output block size (8 / cs).
    /// Subsampled components are scaled less than the image so that they
    /// need little or no upsampling.
    cs: usize,
    bs: usize,
    /// Quantized coefficients (progressive mode only), natural order.
    coefs: Vec<i16>,
    /// Decoded samples, `stride` bytes per row.
    pub plane: Vec<u8>,
    pub stride: usize,
    dc_pred: i32,
    td: usize,
    ta: usize,
}

struct Frame {
    width: usize,
    height: usize,
    progressive: bool,
    comps: Vec<Component>,
    hmax: usize,
    vmax: usize,
    mcux: usize,
    mcuy: usize,
}

struct Scan {
    comps: Vec<usize>,
    ss: usize,
    se: usize,
    ah: u32,
    al: u32,
}

struct Decoder<'a> {
    data: &'a [u8],
    /// Quantization tables in natural order.
    qt: [[u16; 64]; 4],
    dc: [Option<HuffTable>; 4],
    ac: [Option<HuffTable>; 4],
    restart_interval: usize,
    frame: Option<Frame>,
    adobe_transform: Option<u8>,
    orientation: u16,
    /// DCT scaling denominator (1, 2, 4 or 8).
    scale: usize,
    eobrun: u32,
    basis: ReducedBasis,
    scans_decoded: usize,
}

/// Reads the image dimensions from the frame header.
pub fn dimensions(data: &[u8]) -> Result<(u32, u32), Error> {
    let mut pos = 2;
    while let Some((marker, seg_start, seg_end)) = next_segment(data, &mut pos)? {
        if matches!(marker, 0xc0..=0xcf) && !matches!(marker, 0xc4 | 0xc8 | 0xcc) {
            let s = &data[seg_start..seg_end];
            if s.len() < 5 {
                return Err(Error::InvalidData("jpeg: short frame header"));
            }
            let h = u16::from_be_bytes([s[1], s[2]]) as u32;
            let w = u16::from_be_bytes([s[3], s[4]]) as u32;
            return Ok((w, h));
        }
        if marker == 0xda {
            break;
        }
    }
    Err(Error::InvalidData("jpeg: no frame header"))
}

/// Finds the next marker at or after `pos`. Returns (marker, segment start,
/// segment end) for markers with a length field, (marker, pos, pos) for
/// standalone markers, None at the end of data.
fn next_segment(data: &[u8], pos: &mut usize) -> Result<Option<(u8, usize, usize)>, Error> {
    let mut p = *pos;
    // Skip garbage up to the next 0xFF, then fill bytes.
    while p < data.len() && data[p] != 0xff {
        p += 1;
    }
    while p < data.len() && data[p] == 0xff {
        p += 1;
    }
    if p >= data.len() {
        *pos = data.len();
        return Ok(None);
    }
    let marker = data[p];
    p += 1;
    if marker == 0 || matches!(marker, 0x01 | 0xd0..=0xd9) {
        *pos = p;
        return Ok(Some((marker, p, p)));
    }
    if p + 2 > data.len() {
        *pos = data.len();
        return Err(Error::InvalidData("jpeg: truncated segment"));
    }
    let len = u16::from_be_bytes([data[p], data[p + 1]]) as usize;
    if len < 2 {
        return Err(Error::InvalidData("jpeg: bad segment length"));
    }
    if p + len > data.len() {
        *pos = data.len();
        return Err(Error::InvalidData("jpeg: truncated segment"));
    }
    *pos = p + len;
    Ok(Some((marker, p + 2, p + len)))
}

/// Decodes a JPEG, scaling it down by `scale` (1, 2, 4 or 8) in the DCT
/// domain. EXIF orientation is applied.
pub fn decode(data: &[u8], scale: u32) -> Result<Image, Error> {
    decode_scaled(data, scale).map(|r| r.0)
}

/// Like [`decode`], but also returns the exact extent of the original image
/// in output pixels (width / scale, height / scale, after orientation); the
/// last row / column of a scaled image may cover less than a full pixel.
pub fn decode_scaled(data: &[u8], scale: u32) -> Result<(Image, f64, f64), Error> {
    let mut d = Decoder {
        data,
        qt: [[16; 64]; 4],
        dc: [None, None, None, None],
        ac: [None, None, None, None],
        restart_interval: 0,
        frame: None,
        adobe_transform: None,
        orientation: 1,
        scale: match scale {
            0 | 1 => 1,
            2 | 3 => 2,
            4..=7 => 4,
            _ => 8,
        },
        eobrun: 0,
        basis: ReducedBasis::new(),
        scans_decoded: 0,
    };
    d.run()?;
    let img = d.output()?;
    let (w, h) = d.frame.as_ref().map_or((1, 1), |f| (f.width, f.height));
    let (ew, eh) = (w as f64 / d.scale as f64, h as f64 / d.scale as f64);
    let (ew, eh) = if d.orientation >= 5 {
        (eh, ew)
    } else {
        (ew, eh)
    };
    Ok((exif::apply(img, d.orientation), ew, eh))
}

impl<'a> Decoder<'a> {
    fn run(&mut self) -> Result<(), Error> {
        let data = self.data;
        if data.len() < 4 || data[0] != 0xff || data[1] != 0xd8 {
            return Err(Error::InvalidData("jpeg: missing SOI"));
        }
        let mut pos = 2;
        loop {
            let seg = match next_segment(data, &mut pos) {
                Ok(Some(s)) => s,
                Ok(None) => break,
                Err(e) => {
                    if self.scans_decoded > 0 {
                        break; // truncated file: keep what was decoded
                    }
                    return Err(e);
                }
            };
            let (marker, start, end) = seg;
            let s = &data[start..end];
            match marker {
                0xd9 => break,
                0xc0..=0xc2 => {
                    if self.frame.is_some() {
                        return Err(Error::Unsupported("jpeg: multiple frames"));
                    }
                    self.parse_sof(s, marker == 0xc2)?;
                }
                0xc3 => return Err(Error::Unsupported("jpeg: lossless coding")),
                0xc5..=0xc7 => return Err(Error::Unsupported("jpeg: hierarchical coding")),
                0xc9..=0xcb | 0xcd..=0xcf => {
                    return Err(Error::Unsupported("jpeg: arithmetic coding"))
                }
                0xc4 => self.parse_dht(s)?,
                0xdb => self.parse_dqt(s)?,
                0xdd => {
                    if s.len() < 2 {
                        return Err(Error::InvalidData("jpeg: short DRI"));
                    }
                    self.restart_interval = u16::from_be_bytes([s[0], s[1]]) as usize;
                }
                0xda => {
                    let scan = self.parse_sos(s)?;
                    pos = self.decode_scan(end, &scan);
                    self.scans_decoded += 1;
                }
                0xe1 => {
                    if let Some(o) = exif::orientation(s) {
                        self.orientation = o;
                    }
                }
                0xee if s.len() >= 12 && s.starts_with(b"Adobe") => {
                    self.adobe_transform = Some(s[11])
                }
                _ => {}
            }
        }
        if self.frame.is_none() {
            return Err(Error::InvalidData("jpeg: no frame header"));
        }
        if self.scans_decoded == 0 {
            return Err(Error::InvalidData("jpeg: no image data"));
        }
        Ok(())
    }

    fn parse_sof(&mut self, s: &[u8], progressive: bool) -> Result<(), Error> {
        if s.len() < 6 {
            return Err(Error::InvalidData("jpeg: short frame header"));
        }
        if s[0] != 8 {
            return Err(Error::Unsupported(
                "jpeg: sample precision other than 8 bits",
            ));
        }
        let height = u16::from_be_bytes([s[1], s[2]]) as usize;
        let width = u16::from_be_bytes([s[3], s[4]]) as usize;
        let n = s[5] as usize;
        if height == 0 {
            return Err(Error::Unsupported("jpeg: DNL-defined height"));
        }
        if width == 0 {
            return Err(Error::InvalidData("jpeg: zero width"));
        }
        if !(n == 1 || n == 3 || n == 4) {
            return Err(Error::Unsupported("jpeg: unsupported number of components"));
        }
        if s.len() < 6 + 3 * n {
            return Err(Error::InvalidData("jpeg: short frame header"));
        }
        if width as u64 * height as u64 > crate::MAX_PIXELS {
            return Err(Error::Unsupported("image too large"));
        }
        // Every block needs at least one bit of entropy-coded data; reject
        // tiny files that claim enormous dimensions before allocating.
        let min_blocks = (width.div_ceil(8) * height.div_ceil(8)) as u64;
        if min_blocks > self.data.len() as u64 * 8 * 64 {
            return Err(Error::InvalidData(
                "jpeg: file far too short for dimensions",
            ));
        }
        let mut comps = Vec::with_capacity(n);
        for i in 0..n {
            let c = &s[6 + 3 * i..9 + 3 * i];
            let (mut h, mut v) = ((c[1] >> 4) as usize, (c[1] & 15) as usize);
            if !(1..=4).contains(&h) || !(1..=4).contains(&v) || c[2] > 3 {
                return Err(Error::InvalidData("jpeg: bad component parameters"));
            }
            if n == 1 {
                // A single-component frame is always non-interleaved.
                (h, v) = (1, 1);
            }
            comps.push(Component {
                id: c[0],
                h,
                v,
                tq: c[2] as usize,
                ..Default::default()
            });
        }
        let hmax = comps.iter().map(|c| c.h).max().unwrap_or(1);
        let vmax = comps.iter().map(|c| c.v).max().unwrap_or(1);
        let mcux = width.div_ceil(8 * hmax);
        let mcuy = height.div_ceil(8 * vmax);
        let scale = self.scale;
        for c in comps.iter_mut() {
            let lim = (scale * c.h / hmax).min(scale * c.v / vmax).max(1);
            c.cs = [8, 4, 2, 1].into_iter().find(|&p| p <= lim).unwrap_or(1);
            c.bs = 8 / c.cs;
            let bs = c.bs;
            c.bw = (width * c.h).div_ceil(hmax).div_ceil(8);
            c.bh = (height * c.v).div_ceil(vmax).div_ceil(8);
            c.bw_pad = if n == 1 { c.bw } else { mcux * c.h };
            c.bh_pad = if n == 1 { c.bh } else { mcuy * c.v };
            c.stride = c.bw_pad * bs;
            c.plane = vec![128; c.stride * c.bh_pad * bs];
            if progressive {
                c.coefs = vec![0; c.bw_pad * c.bh_pad * 64];
            }
        }
        self.frame = Some(Frame {
            width,
            height,
            progressive,
            comps,
            hmax,
            vmax,
            mcux,
            mcuy,
        });
        Ok(())
    }

    fn parse_dht(&mut self, mut s: &[u8]) -> Result<(), Error> {
        while !s.is_empty() {
            if s.len() < 17 {
                return Err(Error::InvalidData("jpeg: short DHT"));
            }
            let (tc, th) = (s[0] >> 4, (s[0] & 15) as usize);
            if tc > 1 || th > 3 {
                return Err(Error::InvalidData("jpeg: bad DHT table id"));
            }
            let mut counts = [0u8; 16];
            counts.copy_from_slice(&s[1..17]);
            let total: usize = counts.iter().map(|&c| c as usize).sum();
            if total > 256 || s.len() < 17 + total {
                return Err(Error::InvalidData("jpeg: bad DHT"));
            }
            let t = HuffTable::new(&counts, &s[17..17 + total], tc == 1)?;
            if tc == 0 {
                self.dc[th] = Some(t);
            } else {
                self.ac[th] = Some(t);
            }
            s = &s[17 + total..];
        }
        Ok(())
    }

    fn parse_dqt(&mut self, mut s: &[u8]) -> Result<(), Error> {
        while !s.is_empty() {
            let (pq, tq) = (s[0] >> 4, (s[0] & 15) as usize);
            if tq > 3 || pq > 1 {
                return Err(Error::InvalidData("jpeg: bad DQT"));
            }
            let n = if pq == 0 { 64 } else { 128 };
            if s.len() < 1 + n {
                return Err(Error::InvalidData("jpeg: short DQT"));
            }
            for k in 0..64 {
                let v = if pq == 0 {
                    s[1 + k] as u16
                } else {
                    u16::from_be_bytes([s[1 + 2 * k], s[2 + 2 * k]])
                };
                self.qt[tq][ZIGZAG_EXT[k]] = v;
            }
            s = &s[1 + n..];
        }
        Ok(())
    }

    fn parse_sos(&mut self, s: &[u8]) -> Result<Scan, Error> {
        let frame = self
            .frame
            .as_mut()
            .ok_or(Error::InvalidData("jpeg: scan before frame"))?;
        let n = *s.first().ok_or(Error::InvalidData("jpeg: short SOS"))? as usize;
        if n == 0 || n > 4 || s.len() < 4 + 2 * n {
            return Err(Error::InvalidData("jpeg: bad SOS"));
        }
        let mut comps = Vec::with_capacity(n);
        for i in 0..n {
            let id = s[1 + 2 * i];
            let t = s[2 + 2 * i];
            let ci = frame
                .comps
                .iter()
                .position(|c| c.id == id)
                .ok_or(Error::InvalidData(
                    "jpeg: scan references unknown component",
                ))?;
            if comps.contains(&ci) {
                return Err(Error::InvalidData("jpeg: duplicate scan component"));
            }
            frame.comps[ci].td = (t >> 4) as usize & 3;
            frame.comps[ci].ta = (t & 15) as usize & 3;
            comps.push(ci);
        }
        let p = 1 + 2 * n;
        let (ss, se, ah, al) = (
            s[p] as usize,
            s[p + 1] as usize,
            (s[p + 2] >> 4) as u32,
            (s[p + 2] & 15) as u32,
        );
        if frame.progressive {
            if ss > se
                || se > 63
                || (ss == 0 && se != 0)
                || (ss > 0 && n != 1)
                || al > 13
                || ah > 13
            {
                return Err(Error::InvalidData("jpeg: bad progressive scan parameters"));
            }
            Ok(Scan {
                comps,
                ss,
                se,
                ah,
                al,
            })
        } else {
            Ok(Scan {
                comps,
                ss: 0,
                se: 63,
                ah: 0,
                al: 0,
            })
        }
    }

    /// Decodes the entropy-coded data of a scan starting at `pos`; returns
    /// the position from which to continue parsing markers.
    fn decode_scan(&mut self, pos: usize, scan: &Scan) -> usize {
        let mut r = BitReader::new(self.data, pos);
        let _ = self.decode_scan_inner(&mut r, scan);
        r.position()
    }

    fn decode_scan_inner(&mut self, r: &mut BitReader, scan: &Scan) -> Result<(), Error> {
        let Decoder {
            frame,
            dc,
            ac,
            qt,
            restart_interval,
            eobrun,
            scale,
            basis,
            ..
        } = self;
        let frame = frame.as_mut().ok_or(Error::InvalidData("jpeg: no frame"))?;
        let _ = scale;
        // Check that the needed tables exist.
        for &ci in &scan.comps {
            let c = &frame.comps[ci];
            let need_dc = scan.ss == 0 && scan.ah == 0;
            let need_ac = scan.se > 0;
            if (need_dc && dc[c.td].is_none()) || (need_ac && ac[c.ta].is_none()) {
                return Err(Error::InvalidData("jpeg: missing huffman table"));
            }
        }
        let dequant: Vec<Dequant> = scan
            .comps
            .iter()
            .map(|&ci| Dequant::new(&qt[frame.comps[ci].tq]))
            .collect();
        for c in frame.comps.iter_mut() {
            c.dc_pred = 0;
        }
        *eobrun = 0;
        let single = scan.comps.len() == 1;
        let (mw, mh) = if single {
            let c = &frame.comps[scan.comps[0]];
            (c.bw, c.bh)
        } else {
            (frame.mcux, frame.mcuy)
        };
        let progressive = frame.progressive;
        let interval = *restart_interval;
        let mut todo = interval;
        let mut block = [0i16; 64];
        for m in 0..mw * mh {
            if interval > 0 {
                if todo == 0 {
                    if !r.restart() {
                        return Err(Error::InvalidData("jpeg: missing restart marker"));
                    }
                    for c in frame.comps.iter_mut() {
                        c.dc_pred = 0;
                    }
                    *eobrun = 0;
                    todo = interval;
                }
                todo -= 1;
            }
            let (mx, my) = (m % mw, m / mw);
            for (si, &ci) in scan.comps.iter().enumerate() {
                let c = &mut frame.comps[ci];
                let (nh, nv) = if single { (1, 1) } else { (c.h, c.v) };
                for by in 0..nv {
                    for bx in 0..nh {
                        let (x, y) = (mx * nh + bx, my * nv + by);
                        if !progressive {
                            let dct = dc[c.td]
                                .as_ref()
                                .ok_or(Error::InvalidData("jpeg: missing table"))?;
                            let act = ac[c.ta]
                                .as_ref()
                                .ok_or(Error::InvalidData("jpeg: missing table"))?;
                            decode_block(r, dct, act, &mut c.dc_pred, &mut block)?;
                            let off = y * c.bs * c.stride + x * c.bs;
                            if c.bs == 8 {
                                idct::idct8(&block, &dequant[si], &mut c.plane, off, c.stride);
                            } else {
                                idct::idct_reduced(
                                    c.bs,
                                    &block,
                                    &dequant[si],
                                    basis,
                                    &mut c.plane,
                                    off,
                                    c.stride,
                                );
                            }
                        } else {
                            let bi = (y * c.bw_pad + x) * 64;
                            let Some(coefs) = c.coefs.get_mut(bi..bi + 64) else {
                                continue;
                            };
                            let coefs: &mut [i16; 64] =
                                coefs.try_into().map_err(|_| Error::InvalidData("jpeg"))?;
                            if scan.ss == 0 {
                                if scan.ah == 0 {
                                    let dct = dc[c.td]
                                        .as_ref()
                                        .ok_or(Error::InvalidData("jpeg: missing table"))?;
                                    r.refill();
                                    let s = r.decode(dct)? as u32;
                                    if s > 15 {
                                        return Err(Error::InvalidData("jpeg: bad DC size"));
                                    }
                                    c.dc_pred = c.dc_pred.wrapping_add(r.receive_extend(s));
                                    coefs[0] = (c.dc_pred << scan.al) as i16;
                                } else if r.bit() {
                                    coefs[0] |= 1 << scan.al;
                                }
                            } else {
                                let act = ac[c.ta]
                                    .as_ref()
                                    .ok_or(Error::InvalidData("jpeg: missing table"))?;
                                if scan.ah == 0 {
                                    decode_ac_first(r, act, coefs, scan, eobrun)?;
                                } else {
                                    decode_ac_refine(r, act, coefs, scan, eobrun)?;
                                }
                            }
                        }
                    }
                }
            }
        }
        Ok(())
    }

    /// Runs the deferred IDCT (progressive mode) and converts to RGBA.
    fn output(&mut self) -> Result<Image, Error> {
        let scale = self.scale;
        let adobe = self.adobe_transform;
        let frame = self
            .frame
            .as_mut()
            .ok_or(Error::InvalidData("jpeg: no frame"))?;
        if frame.progressive {
            for c in frame.comps.iter_mut() {
                let dq = Dequant::new(&self.qt[c.tq]);
                for by in 0..c.bh {
                    for bx in 0..c.bw {
                        let bi = (by * c.bw_pad + bx) * 64;
                        let blk: &[i16; 64] = match c.coefs[bi..bi + 64].try_into() {
                            Ok(b) => b,
                            Err(_) => continue,
                        };
                        let off = by * c.bs * c.stride + bx * c.bs;
                        if c.bs == 8 {
                            idct::idct8(blk, &dq, &mut c.plane, off, c.stride);
                        } else {
                            idct::idct_reduced(
                                c.bs,
                                blk,
                                &dq,
                                &self.basis,
                                &mut c.plane,
                                off,
                                c.stride,
                            );
                        }
                    }
                }
                c.coefs = Vec::new();
            }
        }
        let cs = match frame.comps.len() {
            1 => ColorSpace::Gray,
            3 => {
                let ids: Vec<u8> = frame.comps.iter().map(|c| c.id).collect();
                if adobe == Some(0) || (adobe.is_none() && ids == *b"RGB") {
                    ColorSpace::Rgb
                } else {
                    ColorSpace::YCbCr
                }
            }
            _ => {
                if adobe == Some(2) {
                    ColorSpace::Ycck
                } else {
                    ColorSpace::Cmyk
                }
            }
        };
        let out_w = frame.width.div_ceil(scale);
        let out_h = frame.height.div_ceil(scale);
        let mut img = crate::alloc_image(out_w as u32, out_h as u32)?;
        let planes: Vec<color::Plane> = frame
            .comps
            .iter()
            .map(|c| color::Plane {
                data: &c.plane,
                stride: c.stride,
                width: (frame.width * c.h)
                    .div_ceil(frame.hmax * c.cs)
                    .clamp(1, c.stride),
                height: (frame.height * c.v)
                    .div_ceil(frame.vmax * c.cs)
                    .clamp(1, c.bh_pad * c.bs),
                hn: c.h * scale,
                hd: frame.hmax * c.cs,
                vn: c.v * scale,
                vd: frame.vmax * c.cs,
            })
            .collect();
        color::convert(&planes, cs, &mut img);
        Ok(img)
    }
}

/// Decodes one sequential-mode block (quantized coefficients, natural order).
#[inline]
fn decode_block(
    r: &mut BitReader,
    dc: &HuffTable,
    ac: &HuffTable,
    pred: &mut i32,
    block: &mut [i16; 64],
) -> Result<(), Error> {
    *block = [0; 64];
    r.refill();
    let s = r.decode(dc)? as u32;
    if s > 15 {
        return Err(Error::InvalidData("jpeg: bad DC size"));
    }
    *pred = pred.wrapping_add(r.receive_extend(s));
    block[0] = *pred as i16;
    let mut k = 1;
    while k < 64 {
        if r.available() < 32 {
            r.refill();
        }
        let f = ac.fast_ac[r.peek(FAST_BITS) as usize];
        if f != 0 {
            k += ((f >> 4) & 15) as usize;
            r.consume((f & 15) as u32);
            block[ZIGZAG_EXT[k]] = (f >> 16) as i16;
            k += 1;
            continue;
        }
        let rs = r.decode(ac)?;
        let (run, size) = ((rs >> 4) as usize, (rs & 15) as u32);
        if size == 0 {
            if run != 15 {
                break;
            }
            k += 16;
            continue;
        }
        k += run;
        block[ZIGZAG_EXT[k]] = r.receive_extend(size) as i16;
        k += 1;
    }
    Ok(())
}

fn decode_ac_first(
    r: &mut BitReader,
    ac: &HuffTable,
    coefs: &mut [i16; 64],
    scan: &Scan,
    eobrun: &mut u32,
) -> Result<(), Error> {
    if *eobrun > 0 {
        *eobrun -= 1;
        return Ok(());
    }
    let mut k = scan.ss;
    while k <= scan.se {
        r.refill();
        let rs = r.decode(ac)?;
        let (run, size) = ((rs >> 4) as u32, (rs & 15) as u32);
        if size == 0 {
            if run < 15 {
                *eobrun = (1 << run) - 1;
                if run > 0 {
                    *eobrun += r.get(run) as u32;
                }
                break;
            }
            k += 16;
            continue;
        }
        k += run as usize;
        let v = r.receive_extend(size);
        coefs[ZIGZAG_EXT[k.min(79)]] = (v << scan.al) as i16;
        k += 1;
    }
    Ok(())
}

fn decode_ac_refine(
    r: &mut BitReader,
    ac: &HuffTable,
    coefs: &mut [i16; 64],
    scan: &Scan,
    eobrun: &mut u32,
) -> Result<(), Error> {
    let p1: i16 = 1 << scan.al;
    let m1: i16 = (-1i16).wrapping_shl(scan.al);
    let mut k = scan.ss;
    let se = scan.se;
    // Refines a non-zero coefficient with one correction bit.
    let refine = |r: &mut BitReader, c: &mut i16| {
        if r.bit() && (*c & p1) == 0 {
            *c = if *c >= 0 {
                c.wrapping_add(p1)
            } else {
                c.wrapping_add(m1)
            };
        }
    };
    if *eobrun == 0 {
        while k <= se {
            r.refill();
            let rs = r.decode(ac)?;
            let (mut run, size) = ((rs >> 4) as i32, (rs & 15) as u32);
            let mut val: i16 = 0;
            if size == 0 {
                if run < 15 {
                    *eobrun = 1 << run;
                    if run > 0 {
                        *eobrun += r.get(run as u32) as u32;
                    }
                    break;
                }
                // run == 15: skip 16 zero coefficients (refining non-zero ones).
            } else {
                val = if r.bit() { p1 } else { m1 };
            }
            while k <= se {
                let z = ZIGZAG_EXT[k];
                if coefs[z] != 0 {
                    refine(r, &mut coefs[z]);
                } else {
                    if run == 0 {
                        if val != 0 {
                            coefs[z] = val;
                        }
                        k += 1;
                        break;
                    }
                    run -= 1;
                }
                k += 1;
            }
        }
    }
    if *eobrun > 0 {
        while k <= se {
            let z = ZIGZAG_EXT[k];
            if coefs[z] != 0 {
                refine(r, &mut coefs[z]);
            }
            k += 1;
        }
        *eobrun -= 1;
    }
    Ok(())
}
