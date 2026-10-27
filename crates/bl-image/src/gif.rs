//! GIF decoder (first frame): LZW, global / local color tables,
//! transparency, interlacing. The frame is placed on a transparent canvas of
//! the logical screen size.

use crate::{alloc_image, Error, Image};

pub fn decode(data: &[u8]) -> Result<Image, Error> {
    if data.len() < 13 || !(data.starts_with(b"GIF87a") || data.starts_with(b"GIF89a")) {
        return Err(Error::InvalidData("gif: bad signature"));
    }
    let sw = u16::from_le_bytes([data[6], data[7]]) as u32;
    let sh = u16::from_le_bytes([data[8], data[9]]) as u32;
    let flags = data[10];
    let mut pos = 13;
    let mut global: Vec<[u8; 3]> = Vec::new();
    if flags & 0x80 != 0 {
        let n = 2usize << (flags & 7);
        global = read_palette(data, pos, n)?;
        pos += 3 * n;
    }
    let mut transparent: Option<u8> = None;
    loop {
        let block = *data.get(pos).ok_or(Error::InvalidData("gif: no image"))?;
        pos += 1;
        match block {
            0x21 => {
                let label = *data
                    .get(pos)
                    .ok_or(Error::InvalidData("gif: truncated extension"))?;
                pos += 1;
                if label == 0xf9 && data.get(pos) == Some(&4) && pos + 5 <= data.len() {
                    let packed = data[pos + 1];
                    transparent = (packed & 1 != 0).then_some(data[pos + 4]);
                }
                pos = skip_sub_blocks(data, pos)?;
            }
            0x2c => {
                if pos + 9 > data.len() {
                    return Err(Error::InvalidData("gif: truncated image descriptor"));
                }
                let d = &data[pos..pos + 9];
                let left = u16::from_le_bytes([d[0], d[1]]) as usize;
                let top = u16::from_le_bytes([d[2], d[3]]) as usize;
                let fw = u16::from_le_bytes([d[4], d[5]]) as usize;
                let fh = u16::from_le_bytes([d[6], d[7]]) as usize;
                let iflags = d[8];
                pos += 9;
                let mut palette = global;
                if iflags & 0x80 != 0 {
                    let n = 2usize << (iflags & 7);
                    palette = read_palette(data, pos, n)?;
                    pos += 3 * n;
                }
                if palette.is_empty() {
                    palette = (0..=255u8).map(|v| [v, v, v]).collect();
                }
                let min_code = *data
                    .get(pos)
                    .ok_or(Error::InvalidData("gif: truncated image"))?;
                pos += 1;
                let mut lzw = Vec::new();
                while let Some(&n) = data.get(pos) {
                    pos += 1;
                    if n == 0 {
                        break;
                    }
                    let end = (pos + n as usize).min(data.len());
                    lzw.extend_from_slice(&data[pos..end]);
                    pos = end;
                }
                // LZW expands at most ~4096 pixels per 3-bit code; reject
                // tiny files claiming huge frames before allocating.
                if (lzw.len() as u64 + 1) * 11_000 * 64 < fw as u64 * fh as u64 {
                    return Err(Error::InvalidData(
                        "gif: image data far too short for dimensions",
                    ));
                }
                // Canvas: logical screen, grown to contain the frame if the
                // screen size is bogus.
                let cw = (sw as usize).max(left + fw).max(1);
                let ch = (sh as usize).max(top + fh).max(1);
                let mut img = alloc_image(cw as u32, ch as u32)?;
                if fw == 0 || fh == 0 {
                    return Ok(img);
                }
                if fw as u64 * fh as u64 > crate::MAX_PIXELS {
                    return Err(Error::Unsupported("image too large"));
                }
                let mut idx = vec![0u8; fw * fh];
                let decoded = lzw_decode(&lzw, min_code, &mut idx)?;
                let interlaced = iflags & 0x40 != 0;
                let rows = decoded.div_ceil(fw).min(fh);
                for r in 0..rows {
                    let y = if interlaced { interlaced_row(r, fh) } else { r };
                    let n = fw.min(decoded - r * fw);
                    for x in 0..n {
                        let i = idx[r * fw + x];
                        if Some(i) == transparent {
                            continue;
                        }
                        let c = palette.get(i as usize).copied().unwrap_or([0, 0, 0]);
                        let o = ((top + y) * cw + left + x) * 4;
                        img.pixels[o..o + 4].copy_from_slice(&[c[0], c[1], c[2], 255]);
                    }
                }
                return Ok(img);
            }
            0x3b => return Err(Error::InvalidData("gif: no image")),
            _ => return Err(Error::InvalidData("gif: unknown block")),
        }
    }
}

fn read_palette(data: &[u8], pos: usize, n: usize) -> Result<Vec<[u8; 3]>, Error> {
    let raw = data
        .get(pos..pos + 3 * n)
        .ok_or(Error::InvalidData("gif: truncated color table"))?;
    Ok(raw.chunks_exact(3).map(|c| [c[0], c[1], c[2]]).collect())
}

fn skip_sub_blocks(data: &[u8], mut pos: usize) -> Result<usize, Error> {
    loop {
        let n = *data
            .get(pos)
            .ok_or(Error::InvalidData("gif: truncated sub-blocks"))? as usize;
        pos += 1 + n;
        if n == 0 {
            return Ok(pos);
        }
    }
}

/// Maps the r-th stored row of an interlaced image to its display row.
fn interlaced_row(r: usize, h: usize) -> usize {
    let p1 = h.div_ceil(8);
    let p2 = (h + 3) / 8;
    let p3 = (h + 1) / 4;
    if r < p1 {
        r * 8
    } else if r < p1 + p2 {
        (r - p1) * 8 + 4
    } else if r < p1 + p2 + p3 {
        (r - p1 - p2) * 4 + 2
    } else {
        (r - p1 - p2 - p3) * 2 + 1
    }
}

/// Decodes GIF LZW data into `out` (stops when it is full). Returns the
/// number of indices produced; corrupt or truncated data ends decoding early.
fn lzw_decode(src: &[u8], min_code: u8, out: &mut [u8]) -> Result<usize, Error> {
    if !(1..=11).contains(&min_code) {
        return Err(Error::InvalidData("gif: bad LZW code size"));
    }
    let min_code = min_code.max(2) as u32;
    let clear = 1u32 << min_code;
    let eoi = clear + 1;
    let mut prefix = [0u16; 4096];
    let mut suffix = [0u8; 4096];
    let mut first = [0u8; 4096];
    let mut len = [0u16; 4096];
    for i in 0..clear as usize {
        suffix[i] = i as u8;
        first[i] = i as u8;
        len[i] = 1;
    }
    let mut size = min_code + 1;
    let mut next = clear + 2;
    let mut prev: Option<u32> = None;
    let (mut bits, mut nbits, mut sp) = (0u32, 0u32, 0usize);
    let mut op = 0usize;
    while op < out.len() {
        while nbits < size {
            let Some(&b) = src.get(sp) else { return Ok(op) };
            bits |= (b as u32) << nbits;
            nbits += 8;
            sp += 1;
        }
        let code = bits & ((1 << size) - 1);
        bits >>= size;
        nbits -= size;
        if code == clear {
            size = min_code + 1;
            next = clear + 2;
            prev = None;
            continue;
        }
        if code == eoi {
            break;
        }
        let Some(p) = prev else {
            if code >= clear {
                return Ok(op);
            }
            out[op] = code as u8;
            op += 1;
            prev = Some(code);
            continue;
        };
        // Determine the string to output and the new entry.
        let (out_code, extra) = if code < next {
            (code, None)
        } else if code == next && next < 4096 {
            (p, Some(first[p as usize]))
        } else {
            return Ok(op); // corrupt
        };
        let l = len[out_code as usize] as usize;
        let total = l + extra.is_some() as usize;
        // Write the string backwards (clipped to the output).
        let mut c = out_code as usize;
        for i in (0..l).rev() {
            if op + i < out.len() {
                out[op + i] = suffix[c];
            }
            c = prefix[c] as usize;
        }
        if let Some(e) = extra {
            if op + l < out.len() {
                out[op + l] = e;
            }
        }
        op = (op + total).min(out.len());
        if next < 4096 {
            let n = next as usize;
            prefix[n] = p as u16;
            first[n] = first[p as usize];
            suffix[n] = if code < next {
                first[code as usize]
            } else {
                first[p as usize]
            };
            len[n] = len[p as usize] + 1;
            next += 1;
            if next == 1 << size && size < 12 {
                size += 1;
            }
        }
        prev = Some(code);
    }
    Ok(op)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interlace_mapping_is_permutation() {
        for h in 1..40 {
            let mut seen = vec![false; h];
            for r in 0..h {
                let y = interlaced_row(r, h);
                assert!(!seen[y]);
                seen[y] = true;
            }
        }
    }
}
