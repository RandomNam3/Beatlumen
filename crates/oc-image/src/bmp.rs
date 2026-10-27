//! BMP decoder: OS/2 core and Windows info headers (v1-v5), 1/4/8-bit
//! palette, 16/24/32-bit direct color, BI_RGB / BI_BITFIELDS /
//! BI_ALPHABITFIELDS, RLE8 / RLE4, bottom-up and top-down rows, embedded
//! JPEG / PNG.

use crate::{alloc_image, Error, Image};

fn u16_at(d: &[u8], p: usize) -> Result<u16, Error> {
    d.get(p..p + 2).map(|b| u16::from_le_bytes([b[0], b[1]])).ok_or(Error::InvalidData("bmp: truncated header"))
}

fn u32_at(d: &[u8], p: usize) -> Result<u32, Error> {
    d.get(p..p + 4)
        .map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
        .ok_or(Error::InvalidData("bmp: truncated header"))
}

/// A color channel described by a bit mask.
#[derive(Clone, Copy)]
struct Channel {
    mask: u32,
    shift: u32,
    bits: u32,
}

impl Channel {
    fn new(mask: u32) -> Channel {
        if mask == 0 {
            return Channel { mask: 0, shift: 0, bits: 0 };
        }
        let shift = mask.trailing_zeros();
        let bits = (mask >> shift).trailing_ones();
        Channel { mask, shift, bits }
    }

    /// Extracts the channel and scales it to 8 bits by bit replication.
    #[inline]
    fn get(&self, px: u32) -> u8 {
        if self.bits == 0 {
            return 0;
        }
        let v = (px & self.mask) >> self.shift;
        if self.bits >= 8 {
            return (v >> (self.bits - 8)) as u8;
        }
        let mut r = 0u32;
        let mut sh = 8 - self.bits as i32;
        while sh > -(self.bits as i32) {
            r |= if sh >= 0 { v << sh } else { v >> -sh };
            sh -= self.bits as i32;
        }
        r as u8
    }
}

pub fn decode(data: &[u8]) -> Result<Image, Error> {
    if data.len() < 18 || &data[..2] != b"BM" {
        return Err(Error::InvalidData("bmp: bad signature"));
    }
    let off_bits = u32_at(data, 10)? as usize;
    let hsize = u32_at(data, 14)? as usize;
    let (width, height, bpp, compression, clr_used);
    let core = hsize == 12;
    if core {
        width = u16_at(data, 18)? as i64;
        height = u16_at(data, 20)? as i64;
        bpp = u16_at(data, 24)? as u32;
        compression = 0;
        clr_used = 0;
    } else if hsize >= 40 {
        width = u32_at(data, 18)? as i32 as i64;
        height = u32_at(data, 22)? as i32 as i64;
        bpp = u16_at(data, 28)? as u32;
        compression = u32_at(data, 30)?;
        clr_used = u32_at(data, 46)? as usize;
    } else {
        return Err(Error::Unsupported("bmp: unknown header version"));
    }
    if matches!(compression, 4 | 5) {
        // Embedded JPEG / PNG.
        let body = data.get(off_bits..).ok_or(Error::InvalidData("bmp: bad data offset"))?;
        return crate::decode(body);
    }
    if width <= 0 || height == 0 || width > i32::MAX as i64 || height.unsigned_abs() > i32::MAX as u64 {
        return Err(Error::InvalidData("bmp: invalid dimensions"));
    }
    let top_down = height < 0;
    let (w, h) = (width as usize, height.unsigned_abs() as usize);
    if !matches!(bpp, 1 | 4 | 8 | 16 | 24 | 32) {
        return Err(Error::Unsupported("bmp: unsupported bit depth"));
    }
    match (compression, bpp) {
        (0, _) | (1, 8) | (2, 4) | (3, 16 | 32) | (6, 16 | 32) => {}
        _ => return Err(Error::Unsupported("bmp: unsupported compression")),
    }

    // Channel masks.
    let mut masks_end = 14 + hsize;
    let (mut r, mut g, mut b, mut a) = match bpp {
        16 => (0x7c00, 0x03e0, 0x001f, 0),
        _ => (0x00ff_0000, 0x0000_ff00, 0x0000_00ff, 0),
    };
    if compression == 3 || compression == 6 {
        if hsize >= 52 {
            r = u32_at(data, 54)?;
            g = u32_at(data, 58)?;
            b = u32_at(data, 62)?;
            if hsize >= 56 {
                a = u32_at(data, 66)?;
            }
        } else {
            r = u32_at(data, 54)?;
            g = u32_at(data, 58)?;
            b = u32_at(data, 62)?;
            masks_end += 12;
            if compression == 6 {
                a = u32_at(data, 66)?;
                masks_end += 4;
            }
        }
    }
    let (cr, cg, cb, ca) = (Channel::new(r), Channel::new(g), Channel::new(b), Channel::new(a));

    // Palette.
    let mut palette = [[0u8, 0, 0, 255]; 256];
    if bpp <= 8 {
        let entry = if core { 3 } else { 4 };
        let max = 1usize << bpp;
        let n = if clr_used == 0 || clr_used > max { max } else { clr_used };
        for (i, p) in palette.iter_mut().enumerate().take(n) {
            let o = masks_end + i * entry;
            if let Some(e) = data.get(o..o + 3) {
                *p = [e[2], e[1], e[0], 255];
            }
        }
    }

    let pix = data.get(off_bits..).unwrap_or(&[]);
    // RLE packs at most 255 pixels into 2 bytes; reject tiny files that
    // claim huge dimensions before allocating.
    if (pix.len() as u64 + 1) * 128 * 64 < w as u64 * h as u64 {
        return Err(Error::InvalidData("bmp: pixel data far too short for dimensions"));
    }
    let mut img = alloc_image(w as u32, h as u32)?;
    if compression == 1 || compression == 2 {
        decode_rle(pix, compression == 2, &palette, top_down, &mut img);
        return Ok(img);
    }
    let stride = ((w as u64 * bpp as u64).div_ceil(32) * 4) as usize;
    let mut any_alpha = false;
    for row in 0..h {
        let y = if top_down { row } else { h - 1 - row };
        let Some(src) = pix.get(row * stride..row * stride + stride) else {
            break; // truncated: remaining rows stay transparent
        };
        let out = &mut img.pixels[y * w * 4..(y + 1) * w * 4];
        match bpp {
            1 | 4 | 8 => {
                let mask = (1u32 << bpp) - 1;
                for (x, o) in out.chunks_exact_mut(4).enumerate() {
                    let bit = x * bpp as usize;
                    let idx = (src[bit / 8] as u32 >> (8 - bpp - (bit % 8) as u32)) & mask;
                    o.copy_from_slice(&palette[idx as usize]);
                }
            }
            24 => {
                for (o, s) in out.chunks_exact_mut(4).zip(src.chunks_exact(3)) {
                    o.copy_from_slice(&[s[2], s[1], s[0], 255]);
                }
            }
            16 | 32 => {
                let bytes = bpp as usize / 8;
                for (o, s) in out.chunks_exact_mut(4).zip(src.chunks_exact(bytes)) {
                    let px = if bytes == 2 {
                        u16::from_le_bytes([s[0], s[1]]) as u32
                    } else {
                        u32::from_le_bytes([s[0], s[1], s[2], s[3]])
                    };
                    let al = if ca.bits > 0 { ca.get(px) } else { 255 };
                    any_alpha |= al != 0;
                    o.copy_from_slice(&[cr.get(px), cg.get(px), cb.get(px), al]);
                }
            }
            _ => {}
        }
    }
    if ca.bits > 0 && !any_alpha {
        // An alpha mask with all-zero alpha almost always means "opaque".
        for p in img.pixels.chunks_exact_mut(4) {
            p[3] = 255;
        }
    }
    Ok(img)
}

/// RLE8 / RLE4 decoding. Pixels skipped by deltas or end-of-line codes stay
/// transparent.
fn decode_rle(src: &[u8], four: bool, pal: &[[u8; 4]; 256], top_down: bool, img: &mut Image) {
    let (w, h) = (img.width as usize, img.height as usize);
    let (mut x, mut row) = (0usize, 0usize);
    let mut i = 0;
    let put = |x: usize, row: usize, idx: u8, img: &mut Image| {
        if x < w && row < h {
            let y = if top_down { row } else { h - 1 - row };
            let o = (y * w + x) * 4;
            img.pixels[o..o + 4].copy_from_slice(&pal[idx as usize]);
        }
    };
    while i + 1 < src.len() && row < h {
        let (n, v) = (src[i] as usize, src[i + 1]);
        i += 2;
        if n > 0 {
            for k in 0..n {
                let idx = if four { if k % 2 == 0 { v >> 4 } else { v & 15 } } else { v };
                put(x, row, idx, img);
                x += 1;
            }
            continue;
        }
        match v {
            0 => {
                x = 0;
                row += 1;
            }
            1 => break,
            2 => {
                if i + 1 >= src.len() {
                    break;
                }
                x += src[i] as usize;
                row += src[i + 1] as usize;
                i += 2;
            }
            n => {
                let n = n as usize;
                let bytes = if four { n.div_ceil(2) } else { n };
                for k in 0..n {
                    let Some(&b) = src.get(i + if four { k / 2 } else { k }) else { break };
                    let idx = if four { if k % 2 == 0 { b >> 4 } else { b & 15 } } else { b };
                    put(x, row, idx, img);
                    x += 1;
                }
                i += bytes + (bytes & 1);
            }
        }
    }
}
