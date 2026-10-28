//! PNG decoder (all color types and bit depths, palette + tRNS, Adam7
//! interlacing, APNG default image) and encoder (RGB8/RGBA8, adaptive
//! filtering).
//!
//! The decoder is lenient: CRCs and the zlib checksum are not verified,
//! unknown chunks are skipped and truncated image data decodes partially
//! (missing rows are transparent black). Gamma / color profile chunks are
//! ignored.

use crate::{alloc_image, crc32, deflate, inflate, Error, Image};

const SIGNATURE: &[u8; 8] = b"\x89PNG\r\n\x1a\n";

/// Adam7 passes: (x0, y0, dx, dy).
const ADAM7: [(usize, usize, usize, usize); 7] = [
    (0, 0, 8, 8),
    (4, 0, 8, 8),
    (0, 4, 4, 8),
    (2, 0, 4, 4),
    (0, 2, 2, 4),
    (1, 0, 2, 2),
    (0, 1, 1, 2),
];

#[derive(Clone, Copy)]
struct Header {
    width: usize,
    height: usize,
    bit_depth: u8,
    color_type: u8,
    interlaced: bool,
}

impl Header {
    fn channels(&self) -> usize {
        match self.color_type {
            0 | 3 => 1,
            2 => 3,
            4 => 2,
            _ => 4,
        }
    }
    fn bits_per_pixel(&self) -> usize {
        self.channels() * self.bit_depth as usize
    }
    fn row_bytes(&self, w: usize) -> usize {
        // u64 so that the bit count cannot overflow on 32-bit targets (the
        // byte count fits after the MAX_PIXELS check).
        (w as u64 * self.bits_per_pixel() as u64).div_ceil(8) as usize
    }
    /// Bytes per complete pixel for filtering (at least 1).
    fn filter_bpp(&self) -> usize {
        self.bits_per_pixel().div_ceil(8)
    }
}

/// Per-image conversion state (palette and transparency).
struct Converter {
    hdr: Header,
    palette: [[u8; 4]; 256],
    /// tRNS color key for gray / RGB images (sample values at native depth).
    key: Option<[u16; 3]>,
}

pub fn decode(data: &[u8]) -> Result<Image, Error> {
    if data.len() < 8 || &data[..8] != SIGNATURE {
        return Err(Error::InvalidData("png: bad signature"));
    }
    let mut pos = 8;
    let mut hdr: Option<Header> = None;
    let mut plte: &[u8] = &[];
    let mut trns: &[u8] = &[];
    let mut idat = Vec::new();
    while pos + 8 <= data.len() {
        let len =
            u32::from_be_bytes([data[pos], data[pos + 1], data[pos + 2], data[pos + 3]]) as usize;
        let ty = &data[pos + 4..pos + 8];
        let start = pos + 8;
        let end = start.saturating_add(len).min(data.len());
        let body = &data[start..end];
        match ty {
            b"IHDR" => {
                if body.len() < 13 {
                    return Err(Error::InvalidData("png: short IHDR"));
                }
                let w = u32::from_be_bytes([body[0], body[1], body[2], body[3]]);
                let h = u32::from_be_bytes([body[4], body[5], body[6], body[7]]);
                let (bd, ct) = (body[8], body[9]);
                let ok = match ct {
                    0 => matches!(bd, 1 | 2 | 4 | 8 | 16),
                    3 => matches!(bd, 1 | 2 | 4 | 8),
                    2 | 4 | 6 => matches!(bd, 8 | 16),
                    _ => false,
                };
                if !ok {
                    return Err(Error::InvalidData("png: invalid color type / bit depth"));
                }
                if body[10] != 0 || body[11] != 0 {
                    return Err(Error::Unsupported(
                        "png: unknown compression or filter method",
                    ));
                }
                if body[12] > 1 {
                    return Err(Error::InvalidData("png: invalid interlace method"));
                }
                if w == 0 || h == 0 || w > i32::MAX as u32 || h > i32::MAX as u32 {
                    return Err(Error::InvalidData("png: invalid dimensions"));
                }
                hdr = Some(Header {
                    width: w as usize,
                    height: h as usize,
                    bit_depth: bd,
                    color_type: ct,
                    interlaced: body[12] == 1,
                });
            }
            b"PLTE" => plte = body,
            b"tRNS" => trns = body,
            b"IDAT" => idat.extend_from_slice(body),
            b"IEND" => break,
            _ => {}
        }
        pos = start.saturating_add(len).saturating_add(4);
    }
    let hdr = hdr.ok_or(Error::InvalidData("png: missing IHDR"))?;
    if idat.is_empty() {
        return Err(Error::InvalidData("png: missing image data"));
    }
    if hdr.width as u64 * hdr.height as u64 > crate::MAX_PIXELS {
        return Err(Error::Unsupported("image too large"));
    }

    let mut conv = Converter {
        hdr,
        palette: [[0, 0, 0, 255]; 256],
        key: None,
    };
    if hdr.color_type == 3 {
        if plte.len() < 3 {
            return Err(Error::InvalidData("png: missing palette"));
        }
        for (i, c) in plte.chunks_exact(3).take(256).enumerate() {
            conv.palette[i] = [c[0], c[1], c[2], *trns.get(i).unwrap_or(&255)];
        }
    } else if hdr.color_type == 0 && trns.len() >= 2 {
        let g = u16::from_be_bytes([trns[0], trns[1]]);
        conv.key = Some([g, g, g]);
    } else if hdr.color_type == 2 && trns.len() >= 6 {
        conv.key = Some([
            u16::from_be_bytes([trns[0], trns[1]]),
            u16::from_be_bytes([trns[2], trns[3]]),
            u16::from_be_bytes([trns[4], trns[5]]),
        ]);
    }

    // Raw (filtered) size of all passes.
    let passes: Vec<(usize, usize, usize, usize, usize, usize)> = if hdr.interlaced {
        ADAM7
            .iter()
            .filter_map(|&(x0, y0, dx, dy)| {
                let pw = if hdr.width > x0 {
                    (hdr.width - x0).div_ceil(dx)
                } else {
                    0
                };
                let ph = if hdr.height > y0 {
                    (hdr.height - y0).div_ceil(dy)
                } else {
                    0
                };
                (pw > 0 && ph > 0).then_some((x0, y0, dx, dy, pw, ph))
            })
            .collect()
    } else {
        vec![(0, 0, 1, 1, hdr.width, hdr.height)]
    };
    let expected: usize = passes.iter().map(|p| p.5 * (1 + hdr.row_bytes(p.4))).sum();
    // Deflate expands at most ~1032:1; reject files that cannot hold even a
    // small fraction of the image (avoids huge allocations for tiny files).
    if (idat.len() as u64) * 1032 * 64 < expected as u64 {
        return Err(Error::InvalidData(
            "png: image data far too short for dimensions",
        ));
    }
    // Deflate cannot expand by more than ~1032:1, so cap the initial
    // allocation for files whose header promises far more than they hold.
    let hint = expected.min(idat.len().saturating_mul(1040).saturating_add(4096));
    let mut img = alloc_image(hdr.width as u32, hdr.height as u32)?;
    let (mut raw, err) = inflate::zlib_decompress_partial(&idat, hint, expected);
    if raw.is_empty() {
        return Err(err.unwrap_or(Error::InvalidData("png: empty image data")));
    }
    raw.resize(expected, 0);

    let bpp = hdr.filter_bpp();
    let mut offset = 0;
    let mut tmp = vec![0u8; hdr.width * 4];
    for &(x0, y0, dx, dy, pw, ph) in &passes {
        let rb = hdr.row_bytes(pw);
        let stride = rb + 1;
        let pass = &mut raw[offset..offset + ph * stride];
        offset += ph * stride;
        for y in 0..ph {
            let (before, rest) = pass.split_at_mut(y * stride);
            let prev = if y > 0 {
                Some(&before[(y - 1) * stride + 1..])
            } else {
                None
            };
            let (ft, cur) = rest[..stride].split_at_mut(1);
            unfilter(ft[0], cur, prev, bpp)?;
            if !hdr.interlaced {
                let out = &mut img.pixels[y * hdr.width * 4..(y + 1) * hdr.width * 4];
                conv.convert_row(cur, pw, out);
            } else {
                conv.convert_row(cur, pw, &mut tmp[..pw * 4]);
                let oy = y0 + y * dy;
                let row = &mut img.pixels[oy * hdr.width * 4..(oy + 1) * hdr.width * 4];
                for (i, px) in tmp[..pw * 4].chunks_exact(4).enumerate() {
                    let ox = (x0 + i * dx) * 4;
                    row[ox..ox + 4].copy_from_slice(px);
                }
            }
        }
    }
    Ok(img)
}

#[inline(always)]
fn paeth(a: u8, b: u8, c: u8) -> u8 {
    let (ia, ib, ic) = (a as i16, b as i16, c as i16);
    let pa = (ib - ic).abs();
    let pb = (ia - ic).abs();
    let pc = (ia + ib - 2 * ic).abs();
    if pa <= pb && pa <= pc {
        a
    } else if pb <= pc {
        b
    } else {
        c
    }
}

/// Reverses the PNG filter of one row in place. `prev` is the previous
/// unfiltered row (None for the first row of a pass).
fn unfilter(ft: u8, cur: &mut [u8], prev: Option<&[u8]>, bpp: usize) -> Result<(), Error> {
    let n = cur.len();
    match (ft, prev) {
        (0, _) | (2, None) => {}
        (1, _) | (4, None) => {
            for i in bpp..n {
                cur[i] = cur[i].wrapping_add(cur[i - bpp]);
            }
        }
        (2, Some(p)) => {
            for (c, &u) in cur.iter_mut().zip(p) {
                *c = c.wrapping_add(u);
            }
        }
        (3, None) => {
            for i in bpp..n {
                cur[i] = cur[i].wrapping_add(cur[i - bpp] >> 1);
            }
        }
        (3, Some(p)) => {
            let p = &p[..n];
            for i in 0..bpp.min(n) {
                cur[i] = cur[i].wrapping_add(p[i] >> 1);
            }
            for i in bpp..n {
                cur[i] = cur[i].wrapping_add(((cur[i - bpp] as u16 + p[i] as u16) >> 1) as u8);
            }
        }
        (4, Some(p)) => {
            let p = &p[..n];
            for i in 0..bpp.min(n) {
                cur[i] = cur[i].wrapping_add(p[i]);
            }
            match bpp {
                3 => unpaeth_fixed::<3>(cur, p),
                4 => unpaeth_fixed::<4>(cur, p),
                _ => {
                    for i in bpp..n {
                        cur[i] = cur[i].wrapping_add(paeth(cur[i - bpp], p[i], p[i - bpp]));
                    }
                }
            }
        }
        _ => return Err(Error::InvalidData("png: invalid filter type")),
    }
    Ok(())
}

/// Paeth reconstruction for 3/4-byte pixels (first pixel already done),
/// keeping the left / upper-left pixels in registers.
fn unpaeth_fixed<const B: usize>(cur: &mut [u8], prev: &[u8]) {
    if cur.len() < B {
        return;
    }
    let mut a = [0u8; B];
    let mut c = [0u8; B];
    a.copy_from_slice(&cur[..B]);
    c.copy_from_slice(&prev[..B]);
    for (cx, px) in cur[B..].chunks_exact_mut(B).zip(prev[B..].chunks_exact(B)) {
        for k in 0..B {
            let v = cx[k].wrapping_add(paeth(a[k], px[k], c[k]));
            cx[k] = v;
            a[k] = v;
            c[k] = px[k];
        }
    }
}

#[inline(always)]
fn scale16(v: u16) -> u8 {
    ((v as u32 * 255 + 32895) >> 16) as u8
}

impl Converter {
    /// Converts one unfiltered row of `w` pixels to RGBA8.
    fn convert_row(&self, row: &[u8], w: usize, out: &mut [u8]) {
        let h = &self.hdr;
        let out = &mut out[..w * 4];
        match (h.color_type, h.bit_depth) {
            (6, 8) => out.copy_from_slice(&row[..w * 4]),
            (2, 8) => {
                for (o, s) in out.chunks_exact_mut(4).zip(row.chunks_exact(3)) {
                    o[0] = s[0];
                    o[1] = s[1];
                    o[2] = s[2];
                    o[3] = 255;
                }
                if let Some(k) = self.key {
                    for o in out.chunks_exact_mut(4) {
                        if [o[0] as u16, o[1] as u16, o[2] as u16] == k {
                            o[3] = 0;
                        }
                    }
                }
            }
            (0, 8) => {
                let key = self.key.map(|k| k[0]);
                for (o, &g) in out.chunks_exact_mut(4).zip(row) {
                    let a = if key == Some(g as u16) { 0 } else { 255 };
                    o.copy_from_slice(&[g, g, g, a]);
                }
            }
            (4, 8) => {
                for (o, s) in out.chunks_exact_mut(4).zip(row.chunks_exact(2)) {
                    o.copy_from_slice(&[s[0], s[0], s[0], s[1]]);
                }
            }
            (6, 16) => {
                for (o, s) in out.chunks_exact_mut(4).zip(row.chunks_exact(8)) {
                    for k in 0..4 {
                        o[k] = scale16(u16::from_be_bytes([s[2 * k], s[2 * k + 1]]));
                    }
                }
            }
            (2, 16) => {
                for (o, s) in out.chunks_exact_mut(4).zip(row.chunks_exact(6)) {
                    let v = [
                        u16::from_be_bytes([s[0], s[1]]),
                        u16::from_be_bytes([s[2], s[3]]),
                        u16::from_be_bytes([s[4], s[5]]),
                    ];
                    let a = if self.key == Some(v) { 0 } else { 255 };
                    o.copy_from_slice(&[scale16(v[0]), scale16(v[1]), scale16(v[2]), a]);
                }
            }
            (0, 16) => {
                let key = self.key.map(|k| k[0]);
                for (o, s) in out.chunks_exact_mut(4).zip(row.chunks_exact(2)) {
                    let v = u16::from_be_bytes([s[0], s[1]]);
                    let g = scale16(v);
                    let a = if key == Some(v) { 0 } else { 255 };
                    o.copy_from_slice(&[g, g, g, a]);
                }
            }
            (4, 16) => {
                for (o, s) in out.chunks_exact_mut(4).zip(row.chunks_exact(4)) {
                    let g = scale16(u16::from_be_bytes([s[0], s[1]]));
                    let a = scale16(u16::from_be_bytes([s[2], s[3]]));
                    o.copy_from_slice(&[g, g, g, a]);
                }
            }
            (3, bd) => {
                let bd = bd as usize;
                let mask = ((1u16 << bd) - 1) as u8;
                for (x, o) in out.chunks_exact_mut(4).enumerate() {
                    let idx = if bd == 8 {
                        row[x]
                    } else {
                        let bit = x * bd;
                        (row[bit / 8] >> (8 - bd - bit % 8)) & mask
                    };
                    o.copy_from_slice(&self.palette[idx as usize]);
                }
            }
            (0, bd) => {
                // 1, 2 or 4 bit grayscale.
                let bd = bd as usize;
                let mask = (1u8 << bd) - 1;
                let scale = 255 / mask;
                let key = self.key.map(|k| k[0]);
                for (x, o) in out.chunks_exact_mut(4).enumerate() {
                    let bit = x * bd;
                    let v = (row[bit / 8] >> (8 - bd - bit % 8)) & mask;
                    let g = v * scale;
                    let a = if key == Some(v as u16) { 0 } else { 255 };
                    o.copy_from_slice(&[g, g, g, a]);
                }
            }
            _ => {}
        }
    }
}

fn write_chunk(out: &mut Vec<u8>, ty: &[u8; 4], body: &[u8]) {
    out.extend_from_slice(&(body.len() as u32).to_be_bytes());
    let start = out.len();
    out.extend_from_slice(ty);
    out.extend_from_slice(body);
    let crc = crc32::update(0, &out[start..]);
    out.extend_from_slice(&crc.to_be_bytes());
}

/// Encodes RGBA8 (RGB8 when fully opaque) with per-row filter selection by
/// minimum sum of absolute differences.
pub fn encode(img: &Image) -> Vec<u8> {
    let w = img.width as usize;
    let h = img.height as usize;
    let pixels = crate::resize::checked_pixels(img);
    let opaque = img.is_opaque();
    let ch = if opaque { 3 } else { 4 };
    let rb = w * ch;
    let mut filtered = Vec::with_capacity(h * (rb + 1));
    let mut prev = vec![0u8; rb];
    let mut cur = vec![0u8; rb];
    let mut best = vec![0u8; rb];
    let mut cand = vec![0u8; rb];
    for y in 0..h {
        let src = &pixels[y * w * 4..(y + 1) * w * 4];
        if opaque {
            for (d, s) in cur.chunks_exact_mut(3).zip(src.chunks_exact(4)) {
                d.copy_from_slice(&s[..3]);
            }
        } else {
            cur.copy_from_slice(src);
        }
        let mut best_ft = 0u8;
        let mut best_sum = u64::MAX;
        for ft in 0..5u8 {
            if y == 0 && (ft == 2 || ft == 4) {
                continue; // identical to None / Sub on the first row
            }
            let sum = filter_row(ft, &cur, &prev, ch, &mut cand);
            if sum < best_sum {
                best_sum = sum;
                best_ft = ft;
                std::mem::swap(&mut best, &mut cand);
            }
        }
        filtered.push(best_ft);
        filtered.extend_from_slice(&best);
        std::mem::swap(&mut prev, &mut cur);
    }
    let z = deflate::zlib_compress(&filtered, 6);

    let mut out = Vec::with_capacity(z.len() + 128);
    out.extend_from_slice(SIGNATURE);
    let mut ihdr = Vec::with_capacity(13);
    ihdr.extend_from_slice(&img.width.to_be_bytes());
    ihdr.extend_from_slice(&img.height.to_be_bytes());
    ihdr.extend_from_slice(&[8, if opaque { 2 } else { 6 }, 0, 0, 0]);
    write_chunk(&mut out, b"IHDR", &ihdr);
    for part in z.chunks(1 << 20) {
        write_chunk(&mut out, b"IDAT", part);
    }
    if z.is_empty() {
        write_chunk(&mut out, b"IDAT", &[]);
    }
    write_chunk(&mut out, b"IEND", &[]);
    out
}

/// Applies filter `ft` to `cur` into `out`, returning the sum of absolute
/// values of the filtered bytes interpreted as signed.
fn filter_row(ft: u8, cur: &[u8], prev: &[u8], bpp: usize, out: &mut [u8]) -> u64 {
    let n = cur.len();
    let b = bpp.min(n);
    // The first `bpp` bytes have no left neighbour (a = c = 0).
    match ft {
        0 => out.copy_from_slice(cur),
        1 => {
            out[..b].copy_from_slice(&cur[..b]);
            for ((o, &x), &a) in out[b..].iter_mut().zip(&cur[b..]).zip(cur) {
                *o = x.wrapping_sub(a);
            }
        }
        2 => {
            for ((o, &x), &u) in out.iter_mut().zip(cur).zip(prev) {
                *o = x.wrapping_sub(u);
            }
        }
        3 => {
            for i in 0..b {
                out[i] = cur[i].wrapping_sub(prev[i] >> 1);
            }
            for (((o, &x), &a), &u) in out[b..].iter_mut().zip(&cur[b..]).zip(cur).zip(&prev[b..]) {
                *o = x.wrapping_sub(((a as u16 + u as u16) >> 1) as u8);
            }
        }
        _ => {
            for i in 0..b {
                out[i] = cur[i].wrapping_sub(prev[i]);
            }
            for i in b..n {
                out[i] = cur[i].wrapping_sub(paeth(cur[i - b], prev[i], prev[i - b]));
            }
        }
    }
    out.iter().map(|&v| (v as i8).unsigned_abs() as u64).sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encode_decode_round_trip() {
        for (w, h, alpha) in [(1, 1, false), (7, 3, true), (33, 17, false), (64, 9, true)] {
            let mut img = Image::new(w, h);
            for (i, p) in img.pixels.chunks_exact_mut(4).enumerate() {
                p[0] = (i * 7) as u8;
                p[1] = (i * 13 / 3) as u8;
                p[2] = (i ^ 0x55) as u8;
                p[3] = if alpha { (i * 31) as u8 } else { 255 };
            }
            let png = encode(&img);
            let back = decode(&png).unwrap();
            assert_eq!((back.width, back.height), (w, h));
            assert_eq!(back.pixels, img.pixels);
        }
    }
}
