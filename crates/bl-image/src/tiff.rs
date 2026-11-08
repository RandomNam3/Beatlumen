//! TIFF decoder, written from the TIFF 6.0 specification (and the BigTIFF
//! extension): first image of the file, strips or tiles, uncompressed /
//! PackBits / LZW / Deflate, horizontal predictor, 1-16 bit gray and
//! palette, 8 / 16 bit RGB(A) and CMYK, chunky or planar samples,
//! associated or unassociated alpha and the orientation tag.
//!
//! Every strip, tile and plane is its own compressed stream; the rows of a
//! strip or tile row are read from all of its streams in lockstep, so the
//! image is scaled while it is decoded and never held at full size.
//! JPEG-compressed and floating point TIFFs are reported as unsupported.

use crate::inflate::InflateStream;
use crate::scale::{Dest, RowSink, Scaler};
use crate::{exif, Error, Fit, Image};

struct Reader<'a> {
    d: &'a [u8],
    le: bool,
    big: bool,
}

impl Reader<'_> {
    fn u16(&self, p: usize) -> Option<u16> {
        let b = self.d.get(p..p + 2)?;
        Some(if self.le {
            u16::from_le_bytes([b[0], b[1]])
        } else {
            u16::from_be_bytes([b[0], b[1]])
        })
    }
    fn u32(&self, p: usize) -> Option<u32> {
        let b = self.d.get(p..p + 4)?;
        let a = [b[0], b[1], b[2], b[3]];
        Some(if self.le {
            u32::from_le_bytes(a)
        } else {
            u32::from_be_bytes(a)
        })
    }
    fn u64(&self, p: usize) -> Option<u64> {
        let b = self.d.get(p..p + 8)?;
        let mut a = [0u8; 8];
        a.copy_from_slice(b);
        Some(if self.le {
            u64::from_le_bytes(a)
        } else {
            u64::from_be_bytes(a)
        })
    }
    fn off(&self, p: usize) -> Option<usize> {
        if self.big {
            self.u64(p).and_then(|v| usize::try_from(v).ok())
        } else {
            self.u32(p).map(|v| v as usize)
        }
    }

    /// Integer values of an IFD entry at `e`.
    fn values(&self, e: usize, limit: usize) -> Option<Vec<u64>> {
        let ty = self.u16(e + 2)?;
        let (count, inline_at, inline_len) = if self.big {
            (self.u64(e + 4)? as usize, e + 12, 8)
        } else {
            (self.u32(e + 4)? as usize, e + 8, 4)
        };
        let size = match ty {
            1 | 2 | 6 | 7 => 1,
            3 | 8 => 2,
            4 | 9 | 13 => 4,
            16 | 17 | 18 => 8,
            _ => return None,
        };
        let count = count.min(limit);
        let total = count.checked_mul(size)?;
        let at = if total <= inline_len {
            inline_at
        } else {
            self.off(inline_at)?
        };
        if at.checked_add(total)? > self.d.len() {
            return None;
        }
        (0..count)
            .map(|i| {
                let p = at + i * size;
                Some(match size {
                    1 => self.d[p] as u64,
                    2 => self.u16(p)? as u64,
                    4 => self.u32(p)? as u64,
                    _ => self.u64(p)?,
                })
            })
            .collect()
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Color {
    WhiteIsZero,
    BlackIsZero,
    Rgb,
    Palette,
    Cmyk,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Compression {
    None,
    Lzw,
    Deflate,
    PackBits,
}

pub(crate) struct Parsed {
    pub width: usize,
    pub height: usize,
    bits: usize,
    spp: usize,
    color: Color,
    compression: Compression,
    predictor: bool,
    planar: bool,
    /// Sample index of the alpha channel, and whether it is premultiplied.
    alpha: Option<(usize, bool)>,
    colormap: Vec<[u8; 3]>,
    /// Strips (tile_w = width) or tiles: offsets and byte counts.
    offsets: Vec<u64>,
    counts: Vec<u64>,
    tile_w: usize,
    tile_h: usize,
    pub orientation: u16,
    le: bool,
}

pub(crate) fn parse(data: &[u8]) -> Result<Parsed, Error> {
    let bad = || Error::InvalidData("tiff: truncated header");
    let le = match data.get(..2) {
        Some(b"II") => true,
        Some(b"MM") => false,
        _ => return Err(Error::InvalidData("tiff: bad byte order")),
    };
    let mut r = Reader {
        d: data,
        le,
        big: false,
    };
    let ifd = match r.u16(2).ok_or_else(bad)? {
        42 => r.u32(4).ok_or_else(bad)? as usize,
        43 => {
            r.big = true;
            if r.u16(4) != Some(8) {
                return Err(Error::Unsupported("tiff: BigTIFF offset size"));
            }
            r.off(8).ok_or_else(bad)?
        }
        _ => return Err(Error::InvalidData("tiff: bad magic number")),
    };
    let (n, first, entry) = if r.big {
        (r.u64(ifd).ok_or_else(bad)? as usize, ifd + 8, 20)
    } else {
        (r.u16(ifd).ok_or_else(bad)? as usize, ifd + 2, 12)
    };
    let mut tags: Vec<(u16, usize)> = Vec::new();
    for i in 0..n.min(4096) {
        let e = first + i * entry;
        let Some(tag) = r.u16(e) else { break };
        tags.push((tag, e));
    }
    const MAX_LIST: usize = 1 << 24;
    let get = |tag: u16, limit: usize| -> Option<Vec<u64>> {
        let &(_, e) = tags.iter().find(|t| t.0 == tag)?;
        r.values(e, limit)
    };
    let one = |tag: u16| get(tag, 1).and_then(|v| v.first().copied());
    let width = one(256).ok_or(Error::InvalidData("tiff: missing width"))? as usize;
    let height = one(257).ok_or(Error::InvalidData("tiff: missing height"))? as usize;
    if width == 0 || height == 0 {
        return Err(Error::InvalidData("tiff: zero dimension"));
    }
    if width > 1 << 20
        || height > 1 << 20
        || width as u64 * height as u64 > crate::MAX_SOURCE_PIXELS
    {
        return Err(Error::Unsupported("image too large"));
    }
    let spp = one(277).unwrap_or(1) as usize;
    if !(1..=16).contains(&spp) {
        return Err(Error::Unsupported("tiff: samples per pixel"));
    }
    let bits_list = get(258, 16).unwrap_or_else(|| vec![1]);
    let bits = bits_list.first().copied().unwrap_or(1) as usize;
    if bits_list.iter().any(|&b| b as usize != bits) {
        return Err(Error::Unsupported("tiff: mixed bits per sample"));
    }
    if one(339).is_some_and(|f| f == 3) {
        return Err(Error::Unsupported("tiff: floating point samples"));
    }
    let compression = match one(259).unwrap_or(1) {
        1 => Compression::None,
        5 => Compression::Lzw,
        8 | 32946 => Compression::Deflate,
        32773 => Compression::PackBits,
        6 | 7 => return Err(Error::Unsupported("tiff: JPEG compression")),
        _ => return Err(Error::Unsupported("tiff: unknown compression")),
    };
    let color = match one(262) {
        Some(0) => Color::WhiteIsZero,
        Some(1) | None => Color::BlackIsZero,
        Some(2) => Color::Rgb,
        Some(3) => Color::Palette,
        Some(5) => Color::Cmyk,
        Some(6) => return Err(Error::Unsupported("tiff: YCbCr samples")),
        _ => {
            return Err(Error::Unsupported(
                "tiff: unknown photometric interpretation",
            ))
        }
    };
    let base = match color {
        Color::Rgb => 3,
        Color::Cmyk => 4,
        _ => 1,
    };
    if spp < base {
        return Err(Error::InvalidData("tiff: too few samples per pixel"));
    }
    let ok_bits = match color {
        Color::WhiteIsZero | Color::BlackIsZero => matches!(bits, 1 | 2 | 4 | 8 | 16),
        Color::Palette => matches!(bits, 1 | 2 | 4 | 8),
        Color::Rgb | Color::Cmyk => matches!(bits, 8 | 16),
    };
    if !ok_bits {
        return Err(Error::Unsupported("tiff: bit depth"));
    }
    let extra = get(338, 16).unwrap_or_default();
    let alpha = match extra.first() {
        Some(&k @ (1 | 2)) if spp > base => Some((base, k == 1)),
        _ => None,
    };
    let predictor = match one(317).unwrap_or(1) {
        1 => false,
        2 => true,
        _ => return Err(Error::Unsupported("tiff: predictor")),
    };
    let planar = one(284).unwrap_or(1) == 2 && spp > 1;
    let mut colormap = Vec::new();
    if color == Color::Palette {
        let n = 1usize << bits;
        let cm = get(320, 3 * n).ok_or(Error::InvalidData("tiff: missing color map"))?;
        if cm.len() < 3 * n {
            return Err(Error::InvalidData("tiff: short color map"));
        }
        colormap = (0..n)
            .map(|i| {
                [
                    (cm[i] >> 8) as u8,
                    (cm[n + i] >> 8) as u8,
                    (cm[2 * n + i] >> 8) as u8,
                ]
            })
            .collect();
    }
    let tiled = tags.iter().any(|t| t.0 == 324);
    let (tile_w, tile_h, offsets, counts) = if tiled {
        let tw = one(322).ok_or(Error::InvalidData("tiff: missing tile width"))? as usize;
        let th = one(323).ok_or(Error::InvalidData("tiff: missing tile length"))? as usize;
        if tw == 0 || th == 0 || tw > 1 << 16 || th > 1 << 16 {
            return Err(Error::InvalidData("tiff: bad tile size"));
        }
        (tw, th, get(324, MAX_LIST), get(325, MAX_LIST))
    } else {
        let rps = one(278).unwrap_or(height as u64).clamp(1, height as u64) as usize;
        (width, rps, get(273, MAX_LIST), get(279, MAX_LIST))
    };
    let offsets = offsets.ok_or(Error::InvalidData("tiff: missing data offsets"))?;
    let mut counts = counts.unwrap_or_default();
    if counts.len() < offsets.len() {
        // Missing byte counts: up to the next offset or the end of the file.
        counts = offsets
            .iter()
            .map(|&o| data.len().saturating_sub(o as usize) as u64)
            .collect();
    }
    Ok(Parsed {
        width,
        height,
        bits,
        spp,
        color,
        compression,
        predictor,
        planar,
        alpha,
        colormap,
        offsets,
        counts,
        tile_w,
        tile_h,
        orientation: one(274).filter(|o| (1..=8).contains(o)).unwrap_or(1) as u16,
        le,
    })
}

impl Parsed {
    /// Decodes the image scaled to the size `fit` gives for its upright
    /// dimensions (orientation applied to the result).
    pub fn decode_fit(&self, data: &[u8], fit: &Fit) -> Result<Image, Error> {
        let (w, h) = (self.width as u32, self.height as u32);
        let (tw, th) = if self.orientation >= 5 {
            let (a, b) = fit.target(h, w);
            (b, a)
        } else {
            fit.target(w, h)
        };
        let mut out = crate::alloc_image(tw, th)?;
        {
            let mut s = Scaler::new(Dest::Image(&mut out), self.width, self.height);
            self.rows(data, &mut s)?;
            s.finish();
        }
        Ok(exif::apply(out, self.orientation))
    }

    /// Streams all rows into `sink` (truncated data ends the image early).
    fn rows(&self, data: &[u8], sink: &mut dyn RowSink) -> Result<(), Error> {
        let (w, h) = (self.width, self.height);
        let (tw, th) = (self.tile_w, self.tile_h);
        let across = w.div_ceil(tw);
        let down = h.div_ceil(th);
        let planes = if self.planar { self.spp } else { 1 };
        let per_plane = across * down;
        // Samples per pixel within one stream, and bytes per stream row.
        let sps = if self.planar { 1 } else { self.spp };
        let seg_bytes = (tw * sps * self.bits).div_ceil(8);
        let row_bytes = seg_bytes * across;
        let mut rows: Vec<Vec<u8>> = vec![vec![0u8; row_bytes]; planes];
        let mut rgba = vec![0u8; w * 4];
        for band in 0..down {
            let mut streams: Vec<Stream> = Vec::with_capacity(planes * across);
            for p in 0..planes {
                for t in 0..across {
                    let i = p * per_plane + band * across + t;
                    let (Some(&o), Some(&c)) = (self.offsets.get(i), self.counts.get(i)) else {
                        return Ok(());
                    };
                    let (o, c) = (o as usize, c as usize);
                    let chunk = data
                        .get(o..o.saturating_add(c).min(data.len()))
                        .unwrap_or(&[]);
                    streams.push(Stream::new(chunk, self.compression)?);
                }
            }
            let y0 = band * th;
            for y in y0..(y0 + th).min(h) {
                for (p, row) in rows.iter_mut().enumerate() {
                    for t in 0..across {
                        let seg = &mut row[t * seg_bytes..(t + 1) * seg_bytes];
                        if streams[p * across + t].read(seg) < seg.len() {
                            return Ok(());
                        }
                        if self.predictor {
                            self.undo_predictor(seg, sps);
                        }
                    }
                }
                self.convert(&rows, &mut rgba);
                sink.row(y, &rgba);
            }
        }
        Ok(())
    }

    /// Reverses horizontal differencing within one stream row.
    fn undo_predictor(&self, seg: &mut [u8], sps: usize) {
        match self.bits {
            8 => {
                for i in sps..seg.len() {
                    seg[i] = seg[i].wrapping_add(seg[i - sps]);
                }
            }
            16 => {
                let n = seg.len() / 2;
                let get = |s: &[u8], i: usize| {
                    if self.le {
                        u16::from_le_bytes([s[2 * i], s[2 * i + 1]])
                    } else {
                        u16::from_be_bytes([s[2 * i], s[2 * i + 1]])
                    }
                };
                for i in sps..n {
                    let v = get(seg, i).wrapping_add(get(seg, i - sps));
                    let b = if self.le {
                        v.to_le_bytes()
                    } else {
                        v.to_be_bytes()
                    };
                    seg[2 * i] = b[0];
                    seg[2 * i + 1] = b[1];
                }
            }
            _ => {}
        }
    }

    /// Sample `s` of pixel `x`, scaled to 8 bits.
    #[inline]
    fn sample(&self, rows: &[Vec<u8>], x: usize, s: usize) -> u8 {
        let (row, i) = if self.planar {
            (&rows[s], x)
        } else {
            (&rows[0], x * self.spp + s)
        };
        match self.bits {
            8 => row[i],
            16 => {
                let v = if self.le {
                    u16::from_le_bytes([row[2 * i], row[2 * i + 1]])
                } else {
                    u16::from_be_bytes([row[2 * i], row[2 * i + 1]])
                };
                ((v as u32 * 255 + 32895) >> 16) as u8
            }
            b => {
                let bit = i * b;
                let v = (row[bit / 8] >> (8 - b - bit % 8)) & ((1u8 << b) - 1);
                if self.color == Color::Palette {
                    v
                } else {
                    (v as u32 * 255 / ((1u32 << b) - 1)) as u8
                }
            }
        }
    }

    fn convert(&self, rows: &[Vec<u8>], out: &mut [u8]) {
        for (x, o) in out.chunks_exact_mut(4).enumerate() {
            let a = self.alpha.map_or(255, |(s, _)| self.sample(rows, x, s));
            let mut px = match self.color {
                Color::BlackIsZero => {
                    let g = self.sample(rows, x, 0);
                    [g, g, g]
                }
                Color::WhiteIsZero => {
                    let g = 255 - self.sample(rows, x, 0);
                    [g, g, g]
                }
                Color::Palette => {
                    let i = if self.bits == 8 {
                        rows[0][x]
                    } else {
                        self.sample(rows, x, 0)
                    };
                    self.colormap.get(i as usize).copied().unwrap_or([0, 0, 0])
                }
                Color::Rgb => [
                    self.sample(rows, x, 0),
                    self.sample(rows, x, 1),
                    self.sample(rows, x, 2),
                ],
                Color::Cmyk => {
                    let k = 255 - self.sample(rows, x, 3) as u32;
                    let f = |c: u8| ((255 - c as u32) * k / 255) as u8;
                    [
                        f(self.sample(rows, x, 0)),
                        f(self.sample(rows, x, 1)),
                        f(self.sample(rows, x, 2)),
                    ]
                }
            };
            if matches!(self.alpha, Some((_, true))) && a < 255 {
                // Associated (premultiplied) alpha.
                for c in px.iter_mut() {
                    *c = if a == 0 {
                        0
                    } else {
                        ((*c as u32 * 255 + a as u32 / 2) / a as u32).min(255) as u8
                    };
                }
            }
            o.copy_from_slice(&[px[0], px[1], px[2], a]);
        }
    }
}

/// A decompressing reader over one strip or tile.
enum Stream<'a> {
    Raw(&'a [u8], usize),
    PackBits(PackBits<'a>),
    Lzw(Box<Lzw<'a>>),
    Deflate(Box<InflateStream<'a>>),
}

impl<'a> Stream<'a> {
    fn new(d: &'a [u8], c: Compression) -> Result<Self, Error> {
        Ok(match c {
            Compression::None => Stream::Raw(d, 0),
            Compression::PackBits => Stream::PackBits(PackBits {
                d,
                pos: 0,
                run: 0,
                literal: false,
                value: 0,
            }),
            Compression::Lzw => Stream::Lzw(Box::new(Lzw::new(d))),
            Compression::Deflate => match InflateStream::zlib(d) {
                Ok(z) => Stream::Deflate(Box::new(z)),
                // Some writers emit raw deflate.
                Err(_) => Stream::Deflate(Box::new(InflateStream::new(d))),
            },
        })
    }

    fn read(&mut self, out: &mut [u8]) -> usize {
        match self {
            Stream::Raw(d, pos) => {
                let n = out.len().min(d.len().saturating_sub(*pos));
                out[..n].copy_from_slice(&d[*pos..*pos + n]);
                *pos += n;
                n
            }
            Stream::PackBits(p) => p.read(out),
            Stream::Lzw(l) => l.read(out),
            Stream::Deflate(z) => z.read(out),
        }
    }
}

struct PackBits<'a> {
    d: &'a [u8],
    pos: usize,
    /// Bytes left in the current run, literal or repeated `value`.
    run: usize,
    literal: bool,
    value: u8,
}

impl PackBits<'_> {
    fn read(&mut self, out: &mut [u8]) -> usize {
        let mut op = 0;
        while op < out.len() {
            if self.run == 0 {
                let Some(&n) = self.d.get(self.pos) else {
                    break;
                };
                self.pos += 1;
                match n {
                    0..=127 => {
                        self.run = n as usize + 1;
                        self.literal = true;
                    }
                    128 => continue,
                    _ => {
                        let Some(&v) = self.d.get(self.pos) else {
                            break;
                        };
                        self.pos += 1;
                        self.run = 257 - n as usize;
                        self.literal = false;
                        self.value = v;
                    }
                }
            }
            let n = self.run.min(out.len() - op);
            if self.literal {
                let avail = n.min(self.d.len().saturating_sub(self.pos));
                out[op..op + avail].copy_from_slice(&self.d[self.pos..self.pos + avail]);
                self.pos += avail;
                op += avail;
                if avail < n {
                    break;
                }
            } else {
                out[op..op + n].fill(self.value);
                op += n;
            }
            self.run -= n;
        }
        op
    }
}

/// TIFF LZW: MSB-first codes of 9..12 bits that widen one code early.
struct Lzw<'a> {
    d: &'a [u8],
    pos: usize,
    bits: u32,
    nbits: u32,
    size: u32,
    next: u32,
    prev: Option<u32>,
    ended: bool,
    prefix: [u16; 4096],
    suffix: [u8; 4096],
    first: [u8; 4096],
    len: [u16; 4096],
    pend: [u8; 4097],
    pstart: usize,
    pend_len: usize,
}

impl<'a> Lzw<'a> {
    fn new(d: &'a [u8]) -> Self {
        let mut l = Lzw {
            d,
            pos: 0,
            bits: 0,
            nbits: 0,
            size: 9,
            next: 258,
            prev: None,
            ended: false,
            prefix: [0; 4096],
            suffix: [0; 4096],
            first: [0; 4096],
            len: [0; 4096],
            pend: [0; 4097],
            pstart: 0,
            pend_len: 0,
        };
        for i in 0..256 {
            l.suffix[i] = i as u8;
            l.first[i] = i as u8;
            l.len[i] = 1;
        }
        l
    }

    fn read(&mut self, out: &mut [u8]) -> usize {
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

    fn decode_code(&mut self) -> bool {
        loop {
            while self.nbits < self.size {
                let Some(&b) = self.d.get(self.pos) else {
                    return false;
                };
                self.pos += 1;
                self.bits = (self.bits << 8) | b as u32;
                self.nbits += 8;
            }
            let code = (self.bits >> (self.nbits - self.size)) & ((1 << self.size) - 1);
            self.nbits -= self.size;
            self.bits &= (1u32 << self.nbits).wrapping_sub(1);
            if code == 256 {
                self.size = 9;
                self.next = 258;
                self.prev = None;
                continue;
            }
            if code == 257 {
                return false;
            }
            let Some(p) = self.prev else {
                if code > 255 {
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
                return false;
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
                // "Early change": widen when the next code would need it.
                if self.next + 1 == 1 << self.size && self.size < 12 {
                    self.size += 1;
                }
            }
            self.prev = Some(code);
            return true;
        }
    }
}

/// Decodes the first image at full size.
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

    /// Minimal little-endian TIFF writer for tests: one strip.
    fn tiff(
        w: u32,
        h: u32,
        spp: u16,
        bits: u16,
        photometric: u16,
        extra: &[(u16, u16, u32, u32)],
        pixels: &[u8],
    ) -> Vec<u8> {
        let mut entries: Vec<(u16, u16, u32, u32)> = vec![
            (256, 4, 1, w),
            (257, 4, 1, h),
            (258, 3, 1, bits as u32),
            (259, 3, 1, 1),
            (262, 3, 1, photometric as u32),
            (273, 4, 1, 0), // patched
            (277, 3, 1, spp as u32),
            (278, 4, 1, h),
            (279, 4, 1, pixels.len() as u32),
        ];
        entries.extend_from_slice(extra);
        entries.sort_by_key(|e| e.0);
        let ifd_len = 2 + entries.len() * 12 + 4;
        let data_off = 8 + ifd_len as u32;
        let mut out = b"II\x2a\x00\x08\x00\x00\x00".to_vec();
        out.extend_from_slice(&(entries.len() as u16).to_le_bytes());
        for (tag, ty, count, val) in entries {
            let val = if tag == 273 { data_off } else { val };
            out.extend_from_slice(&tag.to_le_bytes());
            out.extend_from_slice(&ty.to_le_bytes());
            out.extend_from_slice(&count.to_le_bytes());
            if ty == 3 {
                out.extend_from_slice(&(val as u16).to_le_bytes());
                out.extend_from_slice(&[0, 0]);
            } else {
                out.extend_from_slice(&val.to_le_bytes());
            }
        }
        out.extend_from_slice(&[0; 4]);
        out.extend_from_slice(pixels);
        out
    }

    #[test]
    fn gray_rgb_and_orientation() {
        let g = tiff(3, 2, 1, 8, 1, &[], &[0, 128, 255, 10, 20, 30]);
        let img = decode(&g).unwrap();
        assert_eq!((img.width, img.height), (3, 2));
        assert_eq!(&img.pixels[4..8], &[128, 128, 128, 255]);
        // WhiteIsZero inverts.
        let g = tiff(1, 1, 1, 8, 0, &[], &[0]);
        assert_eq!(decode(&g).unwrap().pixels, [255, 255, 255, 255]);
        // RGB with orientation 6 (rotate 90 degrees clockwise).
        let px: Vec<u8> = (0..2 * 3 * 3).map(|i| i as u8 * 10).collect();
        let t = tiff(2, 3, 3, 8, 2, &[(274, 3, 1, 6)], &px);
        let img = decode(&t).unwrap();
        assert_eq!((img.width, img.height), (3, 2));
        // Top-left of the rotated image is the bottom-left source pixel.
        assert_eq!(&img.pixels[..3], &px[4 * 3..4 * 3 + 3]);
    }

    #[test]
    fn packbits_and_lzw_streams() {
        // PackBits: literal run of 3, then 4 repeats of 9.
        let mut p = Stream::new(&[2, 1, 2, 3, 253, 9], Compression::PackBits).unwrap();
        let mut out = [0u8; 7];
        assert_eq!(p.read(&mut out), 7);
        assert_eq!(out, [1, 2, 3, 9, 9, 9, 9]);
        // LZW: Clear, 'A', 'B', code 258 ("AB"), EOI in 9-bit MSB-first.
        let codes = [256u32, 65, 66, 258, 257];
        let mut bits = 0u64;
        let mut n = 0;
        let mut bytes = Vec::new();
        for c in codes {
            bits = (bits << 9) | c as u64;
            n += 9;
            while n >= 8 {
                bytes.push((bits >> (n - 8)) as u8);
                n -= 8;
            }
        }
        if n > 0 {
            bytes.push((bits << (8 - n)) as u8);
        }
        let mut l = Stream::new(&bytes, Compression::Lzw).unwrap();
        let mut out = [0u8; 8];
        assert_eq!(l.read(&mut out), 4);
        assert_eq!(&out[..4], b"ABAB");
    }
}
