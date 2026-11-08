//! GIF decoder: LZW, global / local color tables, transparency,
//! interlacing and animation (all frames with delays and disposal). Frame
//! rows are decoded incrementally, so a large frame is scaled while it is
//! decoded.

use crate::scale::{Dest, RowSink, Scaler};
use crate::{Error, Fit, Image};

/// One frame as stored in the file.
pub(crate) struct GifFrame {
    pub rect: (usize, usize, usize, usize),
    pub delay_ms: u32,
    /// 0/1 keep, 2 restore to background, 3 restore to previous.
    pub dispose: u8,
    transparent: Option<u8>,
    interlaced: bool,
    /// Local color table (offset, entries), else the global one.
    palette: Option<(usize, usize)>,
    min_code: u8,
    /// Width as stored (before clipping to the canvas).
    stored_w: usize,
    /// Offset of the first data sub-block (its length byte).
    data: usize,
}

pub(crate) struct Parsed {
    pub width: usize,
    pub height: usize,
    global: Option<(usize, usize)>,
    pub frames: Vec<GifFrame>,
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

/// Parses the block structure (frame headers; image data is skipped).
pub(crate) fn parse(data: &[u8]) -> Result<Parsed, Error> {
    if data.len() < 13 || !(data.starts_with(b"GIF87a") || data.starts_with(b"GIF89a")) {
        return Err(Error::InvalidData("gif: bad signature"));
    }
    let sw = u16::from_le_bytes([data[6], data[7]]) as usize;
    let sh = u16::from_le_bytes([data[8], data[9]]) as usize;
    let flags = data[10];
    let mut pos = 13;
    let mut global = None;
    if flags & 0x80 != 0 {
        let n = 2usize << (flags & 7);
        if pos + 3 * n > data.len() {
            return Err(Error::InvalidData("gif: truncated color table"));
        }
        global = Some((pos, n));
        pos += 3 * n;
    }
    let mut frames = Vec::new();
    // Graphic control extension for the next frame.
    let mut gce: (u32, u8, Option<u8>) = (0, 0, None);
    while let Some(&block) = data.get(pos) {
        pos += 1;
        match block {
            0x21 => {
                let Some(&label) = data.get(pos) else { break };
                pos += 1;
                if label == 0xf9 && data.get(pos) == Some(&4) && pos + 5 <= data.len() {
                    let packed = data[pos + 1];
                    let delay = u16::from_le_bytes([data[pos + 2], data[pos + 3]]) as u32;
                    gce = (
                        delay,
                        (packed >> 2) & 7,
                        (packed & 1 != 0).then_some(data[pos + 4]),
                    );
                }
                match skip_sub_blocks(data, pos) {
                    Ok(p) => pos = p,
                    Err(_) => break,
                }
            }
            0x2c => {
                if pos + 10 > data.len() || frames.len() >= crate::anim::MAX_FRAMES {
                    break;
                }
                let d = &data[pos..pos + 9];
                let left = u16::from_le_bytes([d[0], d[1]]) as usize;
                let top = u16::from_le_bytes([d[2], d[3]]) as usize;
                let fw = u16::from_le_bytes([d[4], d[5]]) as usize;
                let fh = u16::from_le_bytes([d[6], d[7]]) as usize;
                let iflags = d[8];
                pos += 9;
                let mut palette = None;
                if iflags & 0x80 != 0 {
                    let n = 2usize << (iflags & 7);
                    if pos + 3 * n > data.len() {
                        break;
                    }
                    palette = Some((pos, n));
                    pos += 3 * n;
                }
                let Some(&min_code) = data.get(pos) else {
                    break;
                };
                pos += 1;
                let start = pos;
                let end = skip_sub_blocks(data, pos);
                // Browsers show delays below 20 ms as 100 ms.
                let delay_ms = if gce.0 < 2 { 100 } else { gce.0 * 10 };
                frames.push(GifFrame {
                    rect: (left, top, fw, fh),
                    delay_ms,
                    dispose: gce.1,
                    transparent: gce.2,
                    interlaced: iflags & 0x40 != 0,
                    palette,
                    min_code,
                    stored_w: fw,
                    data: start,
                });
                gce = (0, 0, None);
                match end {
                    Ok(p) => pos = p,
                    Err(_) => break, // truncated: keep the frame's partial data
                }
            }
            _ => break, // trailer (0x3b) or garbage
        }
    }
    frames.retain(|f| f.rect.2 > 0 && f.rect.3 > 0);
    let first = frames.first().ok_or(Error::InvalidData("gif: no image"))?;
    // Canvas: logical screen, grown to contain the first frame if the
    // screen size is bogus; later frames are clipped to it.
    let (l, t, w, h) = first.rect;
    let width = sw.max(l + w).max(1);
    let height = sh.max(t + h).max(1);
    if width as u64 * height as u64 > crate::MAX_SOURCE_PIXELS {
        return Err(Error::Unsupported("image too large"));
    }
    for f in frames.iter_mut() {
        let (l, t, w, h) = f.rect;
        f.rect = (
            l,
            t,
            w.min(width.saturating_sub(l)),
            h.min(height.saturating_sub(t)),
        );
    }
    Ok(Parsed {
        width,
        height,
        global,
        frames,
    })
}

impl Parsed {
    /// Decodes the first frame on a transparent canvas, scaled to `fit`.
    pub fn decode_fit(&self, data: &[u8], fit: &Fit) -> Result<Image, Error> {
        let (tw, th) = fit.target(self.width as u32, self.height as u32);
        let mut out = crate::alloc_image(tw, th)?;
        let rect = self.frames[0].rect;
        if rect.2 > 0 && rect.3 > 0 {
            let mut s = Scaler::frame(Dest::Image(&mut out), (self.width, self.height), rect, None);
            let r = self.frame_rows(data, 0, &mut s);
            s.finish();
            r?;
        }
        Ok(out)
    }

    /// Decodes frame `i` (rows of its rectangle, transparent pixels with
    /// alpha 0) into `sink`.
    pub fn frame_rows(&self, data: &[u8], i: usize, sink: &mut dyn RowSink) -> Result<(), Error> {
        let f = &self.frames[i];
        let (_, _, fw, fh) = f.rect;
        if fw == 0 || fh == 0 {
            return Ok(());
        }
        // Rows are stored at full width and clipped to the canvas.
        let stored_w = f.stored_w;
        let (pos, n) = f.palette.or(self.global).unwrap_or_default();
        let mut pal = [[0u8, 0, 0, 255]; 256];
        if n == 0 {
            for (v, p) in pal.iter_mut().enumerate() {
                *p = [v as u8, v as u8, v as u8, 255];
            }
        }
        for (j, c) in data[pos..pos + 3 * n].chunks_exact(3).enumerate().take(256) {
            pal[j] = [c[0], c[1], c[2], 255];
        }
        if let Some(t) = f.transparent {
            pal[t as usize] = [0, 0, 0, 0];
        }
        let mut lzw = Lzw::new(data, f.data, f.min_code)?;
        let mut idx = vec![0u8; stored_w.max(fw)];
        let mut rgba = vec![0u8; fw * 4];
        let to_rgba = |idx: &[u8], rgba: &mut [u8]| {
            for (o, &i) in rgba.chunks_exact_mut(4).zip(idx) {
                o.copy_from_slice(&pal[i as usize]);
            }
        };
        if !f.interlaced {
            for y in 0..fh {
                if lzw.fill(&mut idx[..stored_w]) < stored_w {
                    break;
                }
                to_rgba(&idx[..fw], &mut rgba);
                sink.row(y, &rgba);
            }
            return Ok(());
        }
        // Interlaced: rows come in four passes; assemble the frame first.
        if stored_w as u64 * fh as u64 > crate::MAX_PIXELS {
            return Err(Error::Unsupported("image too large"));
        }
        let mut all = vec![0u8; stored_w * fh];
        let mut filled = vec![false; fh];
        for r in 0..fh {
            let y = interlaced_row(r, fh);
            if lzw.fill(&mut all[y * stored_w..(y + 1) * stored_w]) < stored_w {
                break;
            }
            filled[y] = true;
        }
        for y in 0..fh {
            if filled[y] {
                to_rgba(&all[y * stored_w..y * stored_w + fw], &mut rgba);
                sink.row(y, &rgba);
            }
        }
        Ok(())
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

/// Incremental GIF LZW decoder reading the data sub-blocks in place.
struct Lzw<'a> {
    data: &'a [u8],
    /// Next byte and bytes left in the current sub-block.
    pos: usize,
    left: usize,
    ended: bool,
    min_code: u32,
    clear: u32,
    size: u32,
    next: u32,
    prev: Option<u32>,
    bits: u32,
    nbits: u32,
    prefix: Box<[u16; 4096]>,
    suffix: Box<[u8; 4096]>,
    first: Box<[u8; 4096]>,
    len: Box<[u16; 4096]>,
    /// Decoded string not yet handed out (`pend[pstart..pend_len]`).
    pend: Box<[u8; 4097]>,
    pstart: usize,
    pend_len: usize,
}

impl<'a> Lzw<'a> {
    fn new(data: &'a [u8], pos: usize, min_code: u8) -> Result<Self, Error> {
        if !(1..=11).contains(&min_code) {
            return Err(Error::InvalidData("gif: bad LZW code size"));
        }
        let min_code = min_code.max(2) as u32;
        let clear = 1u32 << min_code;
        let mut l = Lzw {
            data,
            pos,
            left: 0,
            ended: false,
            min_code,
            clear,
            size: min_code + 1,
            next: clear + 2,
            prev: None,
            bits: 0,
            nbits: 0,
            prefix: Box::new([0; 4096]),
            suffix: Box::new([0; 4096]),
            first: Box::new([0; 4096]),
            len: Box::new([0; 4096]),
            pend: Box::new([0; 4097]),
            pstart: 0,
            pend_len: 0,
        };
        for i in 0..clear as usize {
            l.suffix[i] = i as u8;
            l.first[i] = i as u8;
            l.len[i] = 1;
        }
        Ok(l)
    }

    #[inline]
    fn byte(&mut self) -> Option<u8> {
        while self.left == 0 {
            let n = *self.data.get(self.pos)? as usize;
            if n == 0 {
                return None;
            }
            self.pos += 1;
            self.left = n;
        }
        let b = *self.data.get(self.pos)?;
        self.pos += 1;
        self.left -= 1;
        Some(b)
    }

    /// Fills `out` with indices; returns how many were produced (fewer
    /// only at the end of the data or at corrupt codes).
    fn fill(&mut self, out: &mut [u8]) -> usize {
        let mut op = 0;
        while op < out.len() {
            if self.pstart < self.pend_len {
                let n = (self.pend_len - self.pstart).min(out.len() - op);
                out[op..op + n].copy_from_slice(&self.pend[self.pstart..self.pstart + n]);
                self.pstart += n;
                op += n;
                continue;
            }
            if self.ended || !self.decode_code() {
                self.ended = true;
                break;
            }
        }
        op
    }

    /// Decodes the next code into the pending string; false at the end.
    fn decode_code(&mut self) -> bool {
        loop {
            while self.nbits < self.size {
                let Some(b) = self.byte() else { return false };
                self.bits |= (b as u32) << self.nbits;
                self.nbits += 8;
            }
            let code = self.bits & ((1 << self.size) - 1);
            self.bits >>= self.size;
            self.nbits -= self.size;
            if code == self.clear {
                self.size = self.min_code + 1;
                self.next = self.clear + 2;
                self.prev = None;
                continue;
            }
            if code == self.clear + 1 {
                return false;
            }
            let Some(p) = self.prev else {
                if code >= self.clear {
                    return false;
                }
                self.pend[0] = code as u8;
                self.pstart = 0;
                self.pend_len = 1;
                self.prev = Some(code);
                return true;
            };
            let (out_code, extra) = if code < self.next {
                (code, None)
            } else if code == self.next && self.next < 4096 {
                (p, Some(self.first[p as usize]))
            } else {
                return false; // corrupt
            };
            let l = self.len[out_code as usize] as usize;
            let mut c = out_code as usize;
            for i in (0..l).rev() {
                self.pend[i] = self.suffix[c];
                c = self.prefix[c] as usize;
            }
            let mut total = l;
            if let Some(e) = extra {
                self.pend[l] = e;
                total += 1;
            }
            self.pstart = 0;
            self.pend_len = total;
            if self.next < 4096 {
                let n = self.next as usize;
                self.prefix[n] = p as u16;
                self.first[n] = self.first[p as usize];
                self.suffix[n] = if code < self.next {
                    self.first[code as usize]
                } else {
                    self.first[p as usize]
                };
                self.len[n] = self.len[p as usize] + 1;
                self.next += 1;
                if self.next == 1 << self.size && self.size < 12 {
                    self.size += 1;
                }
            }
            self.prev = Some(code);
            return true;
        }
    }
}

/// Decodes the first frame at full canvas size.
pub fn decode(data: &[u8]) -> Result<Image, Error> {
    let p = parse(data)?;
    if p.width as u64 * p.height as u64 > crate::MAX_PIXELS {
        return Err(Error::Unsupported("image too large"));
    }
    p.decode_fit(data, &Fit::full())
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
