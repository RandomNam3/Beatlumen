//! HEIC (HEVC in HEIF). HEVC is covered by patent pools, so this crate has
//! no HEVC decoder: the container is read here (size, grid, alpha,
//! transformations) and every coded image is handed, as a small HEIC file
//! of its own, to a decoder of the operating system that the application
//! registers ([`set_system_decoder`]): Android's ImageDecoder, Windows
//! Imaging Component, the system's libheif. Grid tiles are decoded one row
//! of tiles at a time and scaled while they arrive.

use crate::avif::{clean_aperture, transform};
use crate::heif::{Heif, Picture, Props};
use crate::scale::{Dest, RowSink, Scaler};
use crate::{Error, Fit, Image};
use std::sync::RwLock;

/// Decodes a HEIC file of a single image into RGBA. The size arguments are
/// the smallest size the caller needs (the decoder may decode at a reduced
/// size that still covers it, or at full size).
pub type SystemDecoder = dyn Fn(&[u8], u32, u32) -> Option<Image> + Send + Sync;

static DECODER: RwLock<Option<Box<SystemDecoder>>> = RwLock::new(None);

/// Registers (or with `None` removes) the decoder used for HEVC coded
/// images.
pub fn set_system_decoder(f: Option<Box<SystemDecoder>>) {
    if let Ok(mut d) = DECODER.write() {
        *d = f;
    }
}

/// True when a system decoder for HEIC is registered.
pub fn has_system_decoder() -> bool {
    DECODER.read().is_ok_and(|d| d.is_some())
}

fn put_box(out: &mut Vec<u8>, kind: &[u8; 4], body: &[u8]) {
    out.extend_from_slice(&((body.len() + 8) as u32).to_be_bytes());
    out.extend_from_slice(kind);
    out.extend_from_slice(body);
}

/// A HEIC file holding one hvc1 item: `hvcc` is the whole hvcC box.
pub(crate) fn single_image_file(
    hvcc: &[u8],
    w: u32,
    h: u32,
    nclx: Option<(u16, u16, u16, bool)>,
    data: &[u8],
) -> Vec<u8> {
    let mut ftyp = Vec::new();
    ftyp.extend_from_slice(b"heic");
    ftyp.extend_from_slice(&0u32.to_be_bytes());
    ftyp.extend_from_slice(b"mif1heic");
    let mut meta = vec![0u8; 4];
    // FullBox header, pre_defined, handler type, 3 reserved, empty name.
    let mut hdlr = vec![0u8; 8];
    hdlr.extend_from_slice(b"pict");
    hdlr.extend_from_slice(&[0u8; 13]);
    put_box(&mut meta, b"hdlr", &hdlr);
    put_box(&mut meta, b"pitm", &[0, 0, 0, 0, 0, 1]);
    let mut infe = vec![2, 0, 0, 0, 0, 1, 0, 0];
    infe.extend_from_slice(b"hvc1\0");
    let mut iinf = vec![0, 0, 0, 0, 0, 1];
    put_box(&mut iinf, b"infe", &infe);
    put_box(&mut meta, b"iinf", &iinf);
    // iloc with a placeholder offset, patched below.
    let mut iloc = vec![0, 0, 0, 0, 0x44, 0x00, 0, 1, 0, 1, 0, 0, 0, 1];
    let off_pos_in_iloc = iloc.len();
    iloc.extend_from_slice(&0u32.to_be_bytes());
    iloc.extend_from_slice(&(data.len() as u32).to_be_bytes());
    let iloc_pos = meta.len() + 8;
    put_box(&mut meta, b"iloc", &iloc);
    let mut ipco = Vec::new();
    ipco.extend_from_slice(hvcc);
    let mut ispe = vec![0u8; 4];
    ispe.extend_from_slice(&w.to_be_bytes());
    ispe.extend_from_slice(&h.to_be_bytes());
    put_box(&mut ipco, b"ispe", &ispe);
    let mut assoc = vec![0x81, 0x02];
    if let Some((p, t, m, f)) = nclx {
        let mut colr = b"nclx".to_vec();
        colr.extend_from_slice(&p.to_be_bytes());
        colr.extend_from_slice(&t.to_be_bytes());
        colr.extend_from_slice(&m.to_be_bytes());
        colr.push(if f { 0x80 } else { 0 });
        put_box(&mut ipco, b"colr", &colr);
        assoc.push(0x03);
    }
    let mut iprp = Vec::new();
    put_box(&mut iprp, b"ipco", &ipco);
    let mut ipma = vec![0, 0, 0, 0, 0, 0, 0, 1, 0, 1, assoc.len() as u8];
    ipma.extend_from_slice(&assoc);
    put_box(&mut iprp, b"ipma", &ipma);
    put_box(&mut meta, b"iprp", &iprp);
    let mut out = Vec::with_capacity(data.len() + 512);
    put_box(&mut out, b"ftyp", &ftyp);
    let meta_start = out.len();
    put_box(&mut out, b"meta", &meta);
    let mdat_payload = out.len() + 8;
    let p = meta_start + 8 + iloc_pos + off_pos_in_iloc;
    out[p..p + 4].copy_from_slice(&(mdat_payload as u32).to_be_bytes());
    put_box(&mut out, b"mdat", data);
    out
}

fn decode_item(
    h: &Heif,
    id: u32,
    need_w: u32,
    need_h: u32,
    nclx: Option<(u16, u16, u16, bool)>,
) -> Result<Image, Error> {
    let it = h.item(id).ok_or(Error::InvalidData("heic: missing item"))?;
    if !it.props.has_config {
        return Err(Error::InvalidData("heic: item without hvcC"));
    }
    let data = h.item_data(id)?;
    let file = single_image_file(
        &it.props.config,
        it.props.width,
        it.props.height,
        nclx,
        &data,
    );
    let guard = DECODER
        .read()
        .map_err(|_| Error::Unsupported("heic: no system decoder"))?;
    let dec = guard
        .as_ref()
        .ok_or(Error::Unsupported("heic: no HEVC decoder on this system"))?;
    let img =
        dec(&file, need_w, need_h).ok_or(Error::Unsupported("heic: the system decoder failed"))?;
    if img.width == 0
        || img.height == 0
        || img.pixels.len() != img.width as usize * img.height as usize * 4
    {
        return Err(Error::InvalidData(
            "heic: bad image from the system decoder",
        ));
    }
    Ok(img)
}

/// Decodes a HEIC scaled to the size `fit` gives (transformations applied
/// to the result).
pub(crate) fn decode_fit(h: &Heif, fit: &Fit) -> Result<Image, Error> {
    if !has_system_decoder() {
        return Err(Error::Unsupported("heic: no HEVC decoder on this system"));
    }
    let props: Props = h.primary_props();
    let pic = h.picture(h.primary, b"hvc1")?;
    let (w, hh) = match &pic {
        Picture::Grid { width, height, .. } => (*width, *height),
        _ => (props.width, props.height),
    };
    if w == 0 || hh == 0 {
        return Err(Error::InvalidData("heic: zero image size"));
    }
    if w as u64 * hh as u64 > crate::MAX_SOURCE_PIXELS {
        return Err(Error::Unsupported("image too large"));
    }
    let (x0, y0, cw, ch) = clean_aperture(&props, w, hh);
    let swap = props.rotation & 1 == 1;
    let (tw, th) = if swap {
        let (a, b) = fit.target(ch, cw);
        (b, a)
    } else {
        fit.target(cw, ch)
    };
    let alpha = h.alpha_of(h.primary);
    let alpha_pic = match alpha {
        Some(a) => Some(h.picture(a, b"hvc1")?),
        None => None,
    };
    let nclx = props.nclx;
    let mut out = crate::alloc_image(tw, th)?;
    match &pic {
        Picture::Coded(id) => {
            // Ask for a size that still covers the target after cropping.
            let sx = cw as f64 / tw as f64;
            let sy = ch as f64 / th as f64;
            let need_w = ((w as f64 / sx).ceil() as u32).clamp(1, w);
            let need_h = ((hh as f64 / sy).ceil() as u32).clamp(1, hh);
            let mut img = decode_item(h, *id, need_w, need_h, nclx)?;
            if let Some(Picture::Coded(a)) = &alpha_pic {
                if let Ok(ai) = decode_item(h, *a, need_w, need_h, None) {
                    let ai = if (ai.width, ai.height) != (img.width, img.height) {
                        crate::resize(&ai, img.width, img.height)
                    } else {
                        ai
                    };
                    for (p, a) in img
                        .pixels
                        .chunks_exact_mut(4)
                        .zip(ai.pixels.chunks_exact(4))
                    {
                        p[3] = a[0];
                    }
                }
            }
            // Crop in the coordinates of the decoded size.
            let fx = img.width as f64 / w as f64;
            let fy = img.height as f64 / hh as f64;
            let cx0 = ((x0 as f64 * fx).round() as usize).min(img.width as usize - 1);
            let cy0 = ((y0 as f64 * fy).round() as usize).min(img.height as usize - 1);
            let ccw = ((cw as f64 * fx).round() as usize).clamp(1, img.width as usize - cx0);
            let cch = ((ch as f64 * fy).round() as usize).clamp(1, img.height as usize - cy0);
            let mut s = Scaler::new(Dest::Image(&mut out), ccw, cch);
            let stride = img.width as usize * 4;
            for y in 0..cch {
                let o = (cy0 + y) * stride + cx0 * 4;
                s.row(y, &img.pixels[o..o + ccw * 4]);
            }
            s.finish();
        }
        Picture::Grid {
            rows, cols, tiles, ..
        } => {
            let first = h
                .item(tiles[0])
                .ok_or(Error::InvalidData("heic: missing tile"))?;
            let (tw0, th0) = (first.props.width as usize, first.props.height as usize);
            if tw0 == 0
                || th0 == 0
                || tw0 * cols < w as usize
                || th0 * rows < hh as usize
                || (cols - 1) * tw0 >= w as usize
                || (rows - 1) * th0 >= hh as usize
            {
                return Err(Error::InvalidData("heic: grid tiles do not fit the image"));
            }
            let alpha_tiles = match &alpha_pic {
                Some(Picture::Grid {
                    rows: ar,
                    cols: ac,
                    tiles: at,
                    ..
                }) if ar == rows && ac == cols => Some(at.clone()),
                _ => None,
            };
            let mut s = Scaler::new(Dest::Image(&mut out), cw as usize, ch as usize);
            let mut row = vec![0u8; w as usize * 4];
            for r in 0..*rows {
                let top = r * th0;
                if top >= (y0 + ch) as usize {
                    break;
                }
                if top + th0 <= y0 as usize {
                    continue;
                }
                let mut imgs = Vec::with_capacity(*cols);
                for c in 0..*cols {
                    let id = tiles[r * cols + c];
                    let mut img = decode_item(h, id, tw0 as u32, th0 as u32, nclx)?;
                    if (img.width as usize, img.height as usize) != (tw0, th0) {
                        img = crate::resize(&img, tw0 as u32, th0 as u32);
                    }
                    if let Some(at) = &alpha_tiles {
                        if let Ok(ai) =
                            decode_item(h, at[r * cols + c], tw0 as u32, th0 as u32, None)
                        {
                            let ai = if (ai.width as usize, ai.height as usize) != (tw0, th0) {
                                crate::resize(&ai, tw0 as u32, th0 as u32)
                            } else {
                                ai
                            };
                            for (p, a) in img
                                .pixels
                                .chunks_exact_mut(4)
                                .zip(ai.pixels.chunks_exact(4))
                            {
                                p[3] = a[0];
                            }
                        }
                    }
                    imgs.push(img);
                }
                for y in 0..th0 {
                    let gy = top + y;
                    if gy < y0 as usize || gy >= (y0 + ch) as usize {
                        continue;
                    }
                    for (c, img) in imgs.iter().enumerate() {
                        let x = c * tw0;
                        if x >= w as usize {
                            break;
                        }
                        let n = tw0.min(w as usize - x);
                        row[x * 4..(x + n) * 4]
                            .copy_from_slice(&img.pixels[y * tw0 * 4..(y * tw0 + n) * 4]);
                    }
                    s.row(
                        gy - y0 as usize,
                        &row[x0 as usize * 4..(x0 + cw) as usize * 4],
                    );
                }
            }
            s.finish();
        }
        Picture::Sample => return Err(Error::Unsupported("heic: image sequence")),
    }
    Ok(transform(out, &props))
}
