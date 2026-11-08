//! PNG decoder (all color types and bit depths, palette + tRNS, Adam7
//! interlacing, APNG frames), streamed row by row into the scaler, and an encoder (RGB8/RGBA8, adaptive
//! filtering).
//!
//! The decoder is lenient: CRCs and the zlib checksum are not verified,
//! unknown chunks are skipped and truncated image data decodes partially
//! (missing rows are transparent black). Gamma / color profile chunks are
//! ignored.

use crate::inflate::InflateStream;
use crate::scale::{self, Dest, RowSink, Scaler};
use crate::{crc32, deflate, Error, Image};

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

/// Parsed chunk structure of a PNG file (image data stays in place).
pub(crate) struct Parsed {
    hdr: Header,
    conv: Converter,
    /// Byte ranges of the IDAT payloads.
    idat: Vec<(usize, usize)>,
    /// APNG frames (empty for still images).
    pub frames: Vec<ApngFrame>,
}

/// One APNG frame: fcTL fields and the byte ranges of its zlib stream.
pub(crate) struct ApngFrame {
    pub rect: (usize, usize, usize, usize),
    pub delay_ms: u32,
    /// 0 none, 1 background, 2 previous.
    pub dispose: u8,
    /// 0 source (replace), 1 over.
    pub blend: u8,
    data: Vec<(usize, usize)>,
}

fn be32(b: &[u8]) -> u32 {
    u32::from_be_bytes([b[0], b[1], b[2], b[3]])
}

/// Parses the chunks of a PNG file.
pub(crate) fn parse(data: &[u8]) -> Result<Parsed, Error> {
    if data.len() < 8 || &data[..8] != SIGNATURE {
        return Err(Error::InvalidData("png: bad signature"));
    }
    let mut pos = 8;
    let mut hdr: Option<Header> = None;
    let mut plte: &[u8] = &[];
    let mut trns: &[u8] = &[];
    let mut idat = Vec::new();
    let mut actl = false;
    let mut frames: Vec<ApngFrame> = Vec::new();
    // The current fcTL collects the following IDAT / fdAT chunks.
    let mut open: Option<ApngFrame> = None;
    while pos + 8 <= data.len() {
        let len = be32(&data[pos..]) as usize;
        let ty = &data[pos + 4..pos + 8];
        let start = pos + 8;
        let end = start.saturating_add(len).min(data.len());
        let body = &data[start..end];
        match ty {
            b"IHDR" => {
                if body.len() < 13 {
                    return Err(Error::InvalidData("png: short IHDR"));
                }
                let w = be32(body);
                let h = be32(&body[4..]);
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
            b"acTL" => actl = true,
            b"fcTL" if body.len() >= 26 => {
                if let Some(f) = open.take() {
                    frames.push(f);
                }
                let (num, den) = (
                    u16::from_be_bytes([body[20], body[21]]) as u32,
                    u16::from_be_bytes([body[22], body[23]]) as u32,
                );
                let den = if den == 0 { 100 } else { den };
                open = Some(ApngFrame {
                    rect: (
                        be32(&body[12..]) as usize,
                        be32(&body[16..]) as usize,
                        be32(&body[4..]) as usize,
                        be32(&body[8..]) as usize,
                    ),
                    delay_ms: (num as u64 * 1000 / den as u64) as u32,
                    dispose: body[24].min(2),
                    blend: body[25].min(1),
                    data: Vec::new(),
                });
            }
            b"IDAT" => {
                idat.push((start, end));
                if let Some(f) = open.as_mut() {
                    f.data.push((start, end));
                }
            }
            b"fdAT" if body.len() > 4 => {
                if let Some(f) = open.as_mut() {
                    f.data.push((start + 4, end));
                }
            }
            b"IEND" => break,
            _ => {}
        }
        pos = start.saturating_add(len).saturating_add(4);
    }
    if let Some(f) = open.take() {
        frames.push(f);
    }
    let hdr = hdr.ok_or(Error::InvalidData("png: missing IHDR"))?;
    if idat.is_empty() {
        return Err(Error::InvalidData("png: missing image data"));
    }
    if hdr.width as u64 * hdr.height as u64 > crate::MAX_SOURCE_PIXELS {
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
    // Keep only usable frames: inside the canvas, with data.
    if !actl {
        frames.clear();
    }
    frames.retain(|f| {
        let (x, y, w, h) = f.rect;
        w > 0
            && h > 0
            && !f.data.is_empty()
            && x.saturating_add(w) <= hdr.width
            && y.saturating_add(h) <= hdr.height
    });
    Ok(Parsed {
        hdr,
        conv,
        idat,
        frames,
    })
}

/// Concatenates the payloads of several chunks (borrowed when there is
/// only one).
fn joined<'a>(data: &'a [u8], parts: &[(usize, usize)]) -> std::borrow::Cow<'a, [u8]> {
    if let [(s, e)] = parts {
        return std::borrow::Cow::Borrowed(&data[*s..*e]);
    }
    let mut v = Vec::with_capacity(parts.iter().map(|(s, e)| e - s).sum());
    for &(s, e) in parts {
        v.extend_from_slice(&data[s..e]);
    }
    std::borrow::Cow::Owned(v)
}

impl Parsed {
    pub fn size(&self) -> (u32, u32) {
        (self.hdr.width as u32, self.hdr.height as u32)
    }

    /// Decodes the default image, scaled to `tw` x `th`.
    pub fn decode_fit(&self, data: &[u8], tw: u32, th: u32) -> Result<Image, Error> {
        let z = joined(data, &self.idat);
        self.decode_stream(&z, tw, th)
    }

    /// Like [`Parsed::decode_fit`], but joins the IDAT payloads inside the
    /// file's own buffer instead of copying them.
    pub fn decode_fit_owned(&self, mut data: Vec<u8>, tw: u32, th: u32) -> Result<Image, Error> {
        let start = self.idat[0].0;
        let mut end = start;
        for &(s, e) in &self.idat {
            data.copy_within(s..e, end);
            end += e - s;
        }
        self.decode_stream(&data[start..end], tw, th)
    }

    fn decode_stream(&self, z: &[u8], tw: u32, th: u32) -> Result<Image, Error> {
        let (w, h) = (self.hdr.width, self.hdr.height);
        // Deflate expands at most ~1032:1; reject files that cannot hold
        // even a small fraction of the image.
        let expected = h as u64 * (1 + self.hdr.row_bytes(w) as u64);
        if (z.len() as u64) * 1032 * 64 < expected {
            return Err(Error::InvalidData(
                "png: image data far too short for dimensions",
            ));
        }
        let mut out = crate::alloc_image(tw, th)?;
        if !self.hdr.interlaced {
            let mut s = Scaler::new(Dest::Image(&mut out), w, h);
            let r = self.rows(z, w, h, &mut s);
            s.finish();
            return r.map(|_| out);
        }
        // Interlaced: the passes cover the image in a coarse-to-fine grid
        // (every 8th, 4th, 2nd pixel, then all). The image is assembled
        // from the finest grid that has at most four times the pixels of
        // the result (at least 8 MP), then scaled.
        let limit = (4 * tw as u64 * th as u64).clamp(8 << 20, crate::MAX_PIXELS);
        let grid = [1usize, 2, 4, 8]
            .into_iter()
            .find(|&g| (w.div_ceil(g) * h.div_ceil(g)) as u64 <= limit)
            .unwrap_or(8);
        let img = self.interlaced(z, w, h, grid)?;
        let extent = (w as f64 / grid as f64, h as f64 / grid as f64);
        let (rw, rh) = (img.width as usize, img.height as usize);
        let mut s = Scaler::frame(
            Dest::Image(&mut out),
            (rw, rh),
            (0, 0, rw, rh),
            Some(extent),
        );
        scale::push_image(&img, &mut s);
        s.finish();
        Ok(out)
    }

    /// Moves the data of every APNG frame together inside the file's own
    /// buffer (frames then decode without copying their chunks). The
    /// default image's IDAT ranges are no longer valid afterwards.
    pub fn compact_frames(&mut self, data: &mut [u8]) {
        let mut end = self
            .frames
            .first()
            .and_then(|f| f.data.first())
            .map_or(0, |r| r.0);
        for f in self.frames.iter_mut() {
            let start = end;
            for &(s, e) in &f.data {
                data.copy_within(s..e, end);
                end += e - s;
            }
            f.data = vec![(start, end)];
        }
        self.idat.clear();
    }

    /// Decodes APNG frame `i` (rows of its rectangle) into `sink`.
    pub fn frame_rows(&self, data: &[u8], i: usize, sink: &mut dyn RowSink) -> Result<(), Error> {
        let f = &self.frames[i];
        let (_, _, w, h) = f.rect;
        let z = joined(data, &f.data);
        if self.hdr.interlaced {
            if w as u64 * h as u64 > crate::MAX_PIXELS {
                return Err(Error::Unsupported("image too large"));
            }
            let img = self.interlaced(&z, w, h, 1)?;
            scale::push_image(&img, sink);
            return Ok(());
        }
        self.rows(&z, w, h, sink)
    }

    /// Streams the rows of a non-interlaced `w` x `h` image. Truncated or
    /// damaged data ends the image early (the rest stays transparent).
    fn rows(&self, z: &[u8], w: usize, h: usize, sink: &mut dyn RowSink) -> Result<(), Error> {
        let hdr = &self.hdr;
        let mut zs = InflateStream::zlib(z)?;
        let rb = hdr.row_bytes(w);
        let bpp = hdr.filter_bpp();
        let mut prev = vec![0u8; rb];
        let mut cur = vec![0u8; rb + 1];
        let mut rgba = vec![0u8; w * 4];
        for y in 0..h {
            if zs.read(&mut cur) < cur.len() {
                break;
            }
            let (ft, row) = cur.split_at_mut(1);
            if unfilter(ft[0], row, (y > 0).then_some(&prev[..]), bpp).is_err() {
                if y == 0 {
                    return Err(Error::InvalidData("png: invalid filter type"));
                }
                break;
            }
            self.conv.convert_row(row, w, &mut rgba);
            sink.row(y, &rgba);
            prev.copy_from_slice(row);
        }
        Ok(())
    }

    /// Decodes the Adam7 passes that fall on every `grid`-th pixel (1, 2,
    /// 4 or 8) into an image of ceil(w / grid) x ceil(h / grid).
    fn interlaced(&self, z: &[u8], w: usize, h: usize, grid: usize) -> Result<Image, Error> {
        let hdr = &self.hdr;
        let npasses = match grid {
            1 => 7,
            2 => 5,
            4 => 3,
            _ => 1,
        };
        let (gw, gh) = (w.div_ceil(grid), h.div_ceil(grid));
        let mut img = crate::alloc_image(gw as u32, gh as u32)?;
        let mut zs = InflateStream::zlib(z)?;
        let bpp = hdr.filter_bpp();
        let mut tmp = vec![0u8; w * 4];
        'passes: for &(x0, y0, dx, dy) in ADAM7.iter().take(npasses) {
            let pw = if w > x0 { (w - x0).div_ceil(dx) } else { 0 };
            let ph = if h > y0 { (h - y0).div_ceil(dy) } else { 0 };
            if pw == 0 || ph == 0 {
                continue;
            }
            let rb = hdr.row_bytes(pw);
            let mut prev = vec![0u8; rb];
            let mut cur = vec![0u8; rb + 1];
            for y in 0..ph {
                if zs.read(&mut cur) < cur.len() {
                    break 'passes;
                }
                let (ft, row) = cur.split_at_mut(1);
                if unfilter(ft[0], row, (y > 0).then_some(&prev[..]), bpp).is_err() {
                    break 'passes;
                }
                self.conv.convert_row(row, pw, &mut tmp[..pw * 4]);
                prev.copy_from_slice(row);
                let oy = (y0 + y * dy) / grid;
                let orow = &mut img.pixels[oy * gw * 4..(oy + 1) * gw * 4];
                for (i, px) in tmp[..pw * 4].chunks_exact(4).enumerate() {
                    let ox = (x0 + i * dx) / grid * 4;
                    orow[ox..ox + 4].copy_from_slice(px);
                }
            }
        }
        Ok(img)
    }
}

/// Decodes the default image at full size.
pub fn decode(data: &[u8]) -> Result<Image, Error> {
    let p = parse(data)?;
    let (w, h) = p.size();
    if w as u64 * h as u64 > crate::MAX_PIXELS {
        return Err(Error::Unsupported("image too large"));
    }
    p.decode_fit(data, w, h)
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
