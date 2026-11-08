//! WebP decoding: RIFF container (simple lossy / lossless and extended
//! VP8X files with animation), lossless VP8L, lossy VP8 key frames and the
//! ALPH alpha chunk. All of them deliver rows in order, so images are
//! scaled while they are decoded.

mod lossless;
mod vp8;
mod vp8_tables;

use crate::scale::{Dest, RowSink, Scaler};
use crate::{Error, Fit, Image};

/// Iterates RIFF chunks: (fourcc, payload offset, payload).
fn chunks(d: &[u8], base: usize) -> impl Iterator<Item = (&[u8], usize, &[u8])> {
    let mut pos = 0usize;
    std::iter::from_fn(move || {
        let rest = d.get(pos..)?;
        if rest.len() < 8 {
            return None;
        }
        let id = &rest[..4];
        let size = u32::from_le_bytes([rest[4], rest[5], rest[6], rest[7]]) as usize;
        let end = 8usize.saturating_add(size).min(rest.len());
        let payload = &rest[8..end];
        let start = base + pos + 8;
        pos = pos
            .saturating_add(8)
            .saturating_add(size)
            .saturating_add(size & 1);
        Some((id, start, payload))
    })
}

/// One animation frame (byte ranges into the file).
pub(crate) struct AnimFrame {
    pub rect: (usize, usize, usize, usize),
    pub delay_ms: u32,
    /// Dispose to background after display.
    pub dispose_bg: bool,
    /// Alpha-blend over the canvas (else replace the rectangle).
    pub blend: bool,
    image: Bitstream,
}

/// Where the image bitstream of a still image or frame is.
#[derive(Clone, Copy)]
struct Bitstream {
    lossless: bool,
    data: (usize, usize),
    alpha: Option<(usize, usize)>,
}

/// Parsed container.
pub(crate) struct Parsed {
    pub width: usize,
    pub height: usize,
    still: Option<Bitstream>,
    pub frames: Vec<AnimFrame>,
}

fn u24(p: &[u8]) -> usize {
    p[0] as usize | (p[1] as usize) << 8 | (p[2] as usize) << 16
}

/// Finds the bitstream (and alpha) among the chunks of a frame or file.
fn bitstream(d: &[u8], base: usize) -> Option<Bitstream> {
    let mut alpha = None;
    for (id, start, p) in chunks(d, base) {
        match id {
            b"ALPH" => alpha = Some((start, start + p.len())),
            b"VP8 " => {
                return Some(Bitstream {
                    lossless: false,
                    data: (start, start + p.len()),
                    alpha,
                })
            }
            b"VP8L" => {
                return Some(Bitstream {
                    lossless: true,
                    data: (start, start + p.len()),
                    alpha: None,
                })
            }
            _ => {}
        }
    }
    None
}

fn bitstream_size(data: &[u8], b: &Bitstream) -> Result<(u32, u32), Error> {
    let d = &data[b.data.0..b.data.1];
    if b.lossless {
        lossless::dimensions(d)
    } else {
        vp8::dimensions(d)
    }
}

pub(crate) fn parse(data: &[u8]) -> Result<Parsed, Error> {
    if data.len() < 16 || &data[..4] != b"RIFF" || &data[8..12] != b"WEBP" {
        return Err(Error::InvalidData("webp: bad RIFF header"));
    }
    let riff_size = u32::from_le_bytes([data[4], data[5], data[6], data[7]]) as usize;
    let body = &data[12..(8usize.saturating_add(riff_size)).clamp(12, data.len())];
    let mut canvas: Option<(usize, usize)> = None;
    let mut frames = Vec::new();
    for (id, start, p) in chunks(body, 12) {
        match id {
            b"VP8X" if p.len() >= 10 => {
                canvas = Some((1 + u24(&p[4..7]), 1 + u24(&p[7..10])));
            }
            b"ANMF" if p.len() >= 16 => {
                let Some(image) = bitstream(&p[16..], start + 16) else {
                    continue;
                };
                let flags = p[15];
                frames.push(AnimFrame {
                    rect: (
                        u24(&p[0..3]) * 2,
                        u24(&p[3..6]) * 2,
                        1 + u24(&p[6..9]),
                        1 + u24(&p[9..12]),
                    ),
                    delay_ms: u24(&p[12..15]) as u32,
                    dispose_bg: flags & 1 != 0,
                    blend: flags & 2 == 0,
                    image,
                });
            }
            _ => {}
        }
    }
    let still = if frames.is_empty() {
        bitstream(body, 12)
    } else {
        None
    };
    let (width, height) = match (canvas, &still, frames.first()) {
        (Some(c), _, _) => c,
        (None, Some(b), _) => {
            let (w, h) = bitstream_size(data, b)?;
            (w as usize, h as usize)
        }
        (None, None, Some(f)) => (f.rect.0 + f.rect.2, f.rect.1 + f.rect.3),
        _ => return Err(Error::InvalidData("webp: no image data")),
    };
    if still.is_none() && frames.is_empty() {
        return Err(Error::InvalidData("webp: no image data"));
    }
    if width == 0 || height == 0 {
        return Err(Error::InvalidData("webp: zero dimension"));
    }
    if width as u64 * height as u64 > crate::MAX_SOURCE_PIXELS {
        return Err(Error::Unsupported("image too large"));
    }
    // Frames must lie inside the canvas.
    frames.retain(|f| f.rect.0 + f.rect.2 <= width && f.rect.1 + f.rect.3 <= height);
    Ok(Parsed {
        width,
        height,
        still,
        frames,
    })
}

impl Parsed {
    /// Decodes the still image (or the first frame of an animation on a
    /// transparent canvas) scaled to `fit`.
    pub fn decode_fit(&self, data: &[u8], fit: &Fit) -> Result<Image, Error> {
        let (tw, th) = fit.target(self.width as u32, self.height as u32);
        let mut out = crate::alloc_image(tw, th)?;
        let canvas = (self.width, self.height);
        let (rect, b) = match (&self.still, self.frames.first()) {
            (Some(b), _) => {
                let (w, h) = bitstream_size(data, b)?;
                ((0, 0, w as usize, h as usize), *b)
            }
            (None, Some(f)) => (f.rect, f.image),
            _ => return Err(Error::InvalidData("webp: no image data")),
        };
        let mut s = Scaler::frame(Dest::Image(&mut out), canvas, rect, None);
        let r = decode_bitstream(data, &b, &mut s);
        s.finish();
        r.map(|_| out)
    }

    /// Decodes animation frame `i` (rows of its rectangle) into `sink`.
    pub fn frame_rows(&self, data: &[u8], i: usize, sink: &mut dyn RowSink) -> Result<(), Error> {
        decode_bitstream(data, &self.frames[i].image, sink)
    }
}

/// Decodes a VP8 / VP8L bitstream (with optional alpha) into `sink`.
fn decode_bitstream(data: &[u8], b: &Bitstream, sink: &mut dyn RowSink) -> Result<(), Error> {
    let d = &data[b.data.0..b.data.1];
    if b.lossless {
        let mut rows = lossless::rows(d)?;
        let w = rows.width();
        let mut argb = vec![0u32; w];
        let mut rgba = vec![0u8; w * 4];
        let mut y = 0;
        while rows.next_row(&mut argb) {
            for (o, &p) in rgba.chunks_exact_mut(4).zip(&argb) {
                o.copy_from_slice(&[(p >> 16) as u8, (p >> 8) as u8, p as u8, (p >> 24) as u8]);
            }
            sink.row(y, &rgba);
            y += 1;
        }
        if y == 0 {
            rows.result()?;
        }
        return Ok(());
    }
    let (w, h) = vp8::dimensions(d)?;
    // A broken alpha chunk leaves the image opaque rather than failing.
    let mut alpha = b
        .alpha
        .and_then(|(s, e)| Alpha::new(&data[s..e], w as usize, h as usize).ok());
    vp8::decode_rows(
        d,
        alpha.as_mut().map(|a| a as &mut dyn vp8::AlphaRows),
        sink,
    )
}

/// Rows of an ALPH chunk: raw or lossless-compressed, with the prediction
/// filter reversed row by row.
struct Alpha<'a> {
    source: AlphaSource<'a>,
    filter: u8,
    w: usize,
    y: usize,
    prev: Vec<u8>,
    argb: Vec<u32>,
}

enum AlphaSource<'a> {
    Raw(&'a [u8]),
    Lossless(Box<lossless::Rows<'a>>),
}

impl<'a> Alpha<'a> {
    fn new(chunk: &'a [u8], w: usize, h: usize) -> Result<Self, Error> {
        let hdr = *chunk
            .first()
            .ok_or(Error::InvalidData("webp: empty ALPH chunk"))?;
        let source = match hdr & 3 {
            0 => AlphaSource::Raw(&chunk[1..]),
            1 => AlphaSource::Lossless(Box::new(lossless::rows_headerless(&chunk[1..], w, h)?)),
            _ => return Err(Error::InvalidData("webp: unknown alpha compression")),
        };
        Ok(Alpha {
            source,
            filter: (hdr >> 2) & 3,
            w,
            y: 0,
            prev: vec![0; w],
            argb: vec![0; w],
        })
    }
}

impl vp8::AlphaRows for Alpha<'_> {
    fn next_alpha(&mut self, out: &mut [u8]) -> bool {
        let w = self.w;
        let out = &mut out[..w];
        match &mut self.source {
            AlphaSource::Raw(d) => {
                let Some(row) = d.get(self.y * w..(self.y + 1) * w) else {
                    return false;
                };
                out.copy_from_slice(row);
            }
            AlphaSource::Lossless(r) => {
                if !r.next_row(&mut self.argb) {
                    return false;
                }
                for (o, &p) in out.iter_mut().zip(&self.argb) {
                    *o = (p >> 8) as u8;
                }
            }
        }
        // Reverse the prediction filter.
        if self.filter != 0 {
            let y = self.y;
            for x in 0..w {
                let pred = if y == 0 {
                    if x == 0 {
                        0
                    } else {
                        out[x - 1]
                    }
                } else if x == 0 {
                    self.prev[0]
                } else {
                    match self.filter {
                        1 => out[x - 1],
                        2 => self.prev[x],
                        _ => (out[x - 1] as i32 + self.prev[x] as i32 - self.prev[x - 1] as i32)
                            .clamp(0, 255) as u8,
                    }
                };
                out[x] = out[x].wrapping_add(pred);
            }
        }
        self.prev.copy_from_slice(out);
        self.y += 1;
        true
    }
}

/// Decodes the still image (or first frame) at full size.
pub fn decode(data: &[u8]) -> Result<Image, Error> {
    let p = parse(data)?;
    if p.width as u64 * p.height as u64 > crate::MAX_PIXELS {
        return Err(Error::Unsupported("image too large"));
    }
    p.decode_fit(data, &Fit::full())
}
