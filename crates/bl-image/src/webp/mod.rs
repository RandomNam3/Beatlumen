//! WebP decoding: RIFF container (simple lossy / lossless and extended
//! VP8X files, first frame of animations), lossless VP8L, lossy VP8 key
//! frames and the ALPH alpha chunk.

mod lossless;
mod vp8;
mod vp8_tables;

use crate::jpeg::color::{self, ColorSpace, Plane};
use crate::{alloc_image, Error, Image};

/// Iterates RIFF chunks: (fourcc, payload).
fn chunks(mut d: &[u8]) -> impl Iterator<Item = (&[u8], &[u8])> {
    std::iter::from_fn(move || {
        if d.len() < 8 {
            return None;
        }
        let id = &d[..4];
        let size = u32::from_le_bytes([d[4], d[5], d[6], d[7]]) as usize;
        let end = 8usize.saturating_add(size).min(d.len());
        let payload = &d[8..end];
        let next = 8usize
            .saturating_add(size)
            .saturating_add(size & 1)
            .min(d.len());
        d = &d[next..];
        Some((id, payload))
    })
}

pub fn decode(data: &[u8]) -> Result<Image, Error> {
    if data.len() < 16 || &data[..4] != b"RIFF" || &data[8..12] != b"WEBP" {
        return Err(Error::InvalidData("webp: bad RIFF header"));
    }
    let riff_size = u32::from_le_bytes([data[4], data[5], data[6], data[7]]) as usize;
    let body = &data[12..(8usize.saturating_add(riff_size)).clamp(12, data.len())];
    let mut canvas: Option<(usize, usize)> = None;
    let mut alpha: Option<&[u8]> = None;
    for (id, payload) in chunks(body) {
        match id {
            b"VP8X" => {
                if payload.len() >= 10 {
                    let w = 1
                        + (payload[4] as usize
                            | (payload[5] as usize) << 8
                            | (payload[6] as usize) << 16);
                    let h = 1
                        + (payload[7] as usize
                            | (payload[8] as usize) << 8
                            | (payload[9] as usize) << 16);
                    canvas = Some((w, h));
                }
            }
            b"ALPH" => alpha = Some(payload),
            b"VP8 " => return decode_lossy(payload, alpha),
            b"VP8L" => return decode_lossless(payload),
            b"ANMF" => return decode_frame(payload, canvas),
            _ => {}
        }
    }
    Err(Error::InvalidData("webp: no image data"))
}

/// First animation frame, composited onto a transparent canvas.
fn decode_frame(payload: &[u8], canvas: Option<(usize, usize)>) -> Result<Image, Error> {
    if payload.len() < 16 {
        return Err(Error::InvalidData("webp: short ANMF chunk"));
    }
    let u24 = |p: &[u8]| p[0] as usize | (p[1] as usize) << 8 | (p[2] as usize) << 16;
    let (fx, fy) = (u24(&payload[0..3]) * 2, u24(&payload[3..6]) * 2);
    let mut alpha = None;
    let mut frame = None;
    for (id, p) in chunks(&payload[16..]) {
        match id {
            b"ALPH" => alpha = Some(p),
            b"VP8 " => {
                frame = Some(decode_lossy(p, alpha)?);
                break;
            }
            b"VP8L" => {
                frame = Some(decode_lossless(p)?);
                break;
            }
            _ => {}
        }
    }
    let frame = frame.ok_or(Error::InvalidData("webp: empty animation frame"))?;
    let (fw, fh) = (frame.width as usize, frame.height as usize);
    let (cw, ch) = canvas.unwrap_or((fx + fw, fy + fh));
    if (cw, ch) == (fw, fh) && fx == 0 && fy == 0 {
        return Ok(frame);
    }
    let mut img = alloc_image(cw as u32, ch as u32)?;
    for y in 0..fh.min(ch.saturating_sub(fy)) {
        let n = fw.min(cw.saturating_sub(fx));
        let src = &frame.pixels[y * fw * 4..(y * fw + n) * 4];
        let o = ((fy + y) * cw + fx) * 4;
        img.pixels[o..o + n * 4].copy_from_slice(src);
    }
    Ok(img)
}

fn decode_lossless(payload: &[u8]) -> Result<Image, Error> {
    let (w, h, argb) = lossless::decode(payload)?;
    let mut img = alloc_image(w, h)?;
    for (o, &p) in img.pixels.chunks_exact_mut(4).zip(&argb) {
        o.copy_from_slice(&[(p >> 16) as u8, (p >> 8) as u8, p as u8, (p >> 24) as u8]);
    }
    Ok(img)
}

fn decode_lossy(payload: &[u8], alpha: Option<&[u8]>) -> Result<Image, Error> {
    let f = vp8::decode(payload)?;
    let mut img = alloc_image(f.width as u32, f.height as u32)?;
    let cw = f.width.div_ceil(2);
    let ch = f.height.div_ceil(2);
    let planes = [
        Plane {
            data: &f.y,
            stride: f.y_stride,
            width: f.width,
            height: f.height,
            hn: 1,
            hd: 1,
            vn: 1,
            vd: 1,
        },
        Plane {
            data: &f.u,
            stride: f.uv_stride,
            width: cw,
            height: ch,
            hn: 1,
            hd: 2,
            vn: 1,
            vd: 2,
        },
        Plane {
            data: &f.v,
            stride: f.uv_stride,
            width: cw,
            height: ch,
            hn: 1,
            hd: 2,
            vn: 1,
            vd: 2,
        },
    ];
    color::convert(&planes, ColorSpace::YuvLimited, &mut img);
    if let Some(a) = alpha {
        // A broken alpha chunk leaves the image opaque rather than failing.
        if let Ok(values) = decode_alpha(a, f.width, f.height) {
            for (p, &v) in img.pixels.chunks_exact_mut(4).zip(&values) {
                p[3] = v;
            }
        }
    }
    Ok(img)
}

/// Decodes an ALPH chunk into width*height alpha values.
fn decode_alpha(chunk: &[u8], w: usize, h: usize) -> Result<Vec<u8>, Error> {
    let hdr = *chunk
        .first()
        .ok_or(Error::InvalidData("webp: empty ALPH chunk"))?;
    let method = hdr & 3;
    let filter = (hdr >> 2) & 3;
    let mut a = match method {
        0 => {
            let mut v = chunk[1..].to_vec();
            v.resize(w * h, 0);
            v
        }
        1 => lossless::decode_headerless(&chunk[1..], w, h)?
            .iter()
            .map(|&p| (p >> 8) as u8)
            .collect(),
        _ => return Err(Error::InvalidData("webp: unknown alpha compression")),
    };
    // Reverse the prediction filter.
    if filter != 0 {
        for y in 0..h {
            for x in 0..w {
                let i = y * w + x;
                let pred =
                    if y == 0 {
                        if x == 0 {
                            0
                        } else {
                            a[i - 1]
                        }
                    } else if x == 0 {
                        a[i - w]
                    } else {
                        match filter {
                            1 => a[i - 1],
                            2 => a[i - w],
                            _ => (a[i - 1] as i32 + a[i - w] as i32 - a[i - w - 1] as i32)
                                .clamp(0, 255) as u8,
                        }
                    };
                a[i] = a[i].wrapping_add(pred);
            }
        }
    }
    Ok(a)
}
