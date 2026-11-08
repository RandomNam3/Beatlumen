//! AVIF: still images (and the first frame of image sequences) coded with
//! AV1, single images or grids of tiles, with alpha. The tiles of a grid
//! row are decoded side by side and every row goes through the colour
//! conversion into the streaming scaler, so a 16K image (a single frame
//! or a grid) is never held at full size.

use crate::av1::{parse_stream, FrameDecoder};
use crate::heif::{Heif, Picture, Props};
use crate::scale::{Dest, RowSink, Scaler};
use crate::yuv::{ColorInfo, Converter};
use crate::{exif, Error, Fit, Image};
use std::borrow::Cow;
use std::collections::VecDeque;

/// Size of the picture before transformations.
fn picture_size(h: &Heif, pic: &Picture, props: &Props) -> Result<(u32, u32), Error> {
    match pic {
        Picture::Grid { width, height, .. } => Ok((*width, *height)),
        Picture::Coded(id) => {
            let p = &h
                .item(*id)
                .ok_or(Error::InvalidData("avif: missing item"))?
                .props;
            if props.width > 0 && props.height > 0 {
                Ok((props.width, props.height))
            } else if p.width > 0 && p.height > 0 {
                Ok((p.width, p.height))
            } else {
                // No ispe: the frame size of the stream.
                let data = h.item_data(*id)?;
                let s = parse_stream(&data, &p.config)?;
                Ok((s.fh.upscaled_width as u32, s.fh.frame_height as u32))
            }
        }
        Picture::Sample => Ok((props.width, props.height)),
    }
}

/// The clean aperture as (left, top, width, height), when it is a valid
/// crop of a `w` x `h` image.
pub(crate) fn clean_aperture(props: &Props, w: u32, h: u32) -> (u32, u32, u32, u32) {
    let full = (0, 0, w, h);
    let Some(c) = props.clap else {
        return full;
    };
    if c.iter().any(|&(_, d)| d <= 0) {
        return full;
    }
    let cw = c[0].0 as f64 / c[0].1 as f64;
    let ch = c[1].0 as f64 / c[1].1 as f64;
    let ox = c[2].0 as f64 / c[2].1 as f64;
    let oy = c[3].0 as f64 / c[3].1 as f64;
    let left = (w as f64 - 1.0) / 2.0 + ox - (cw - 1.0) / 2.0;
    let top = (h as f64 - 1.0) / 2.0 + oy - (ch - 1.0) / 2.0;
    let (l, t, cw, ch) = (left.round(), top.round(), cw.round(), ch.round());
    if l < 0.0 || t < 0.0 || cw < 1.0 || ch < 1.0 || l + cw > w as f64 || t + ch > h as f64 {
        return full;
    }
    (l as u32, t as u32, cw as u32, ch as u32)
}

/// Upright size of an AVIF or HEIC file.
pub(crate) fn dimensions(data: &[u8]) -> Result<(u32, u32), Error> {
    let h = Heif::parse(data)?;
    let props = h.primary_props();
    let codec = match h.codec() {
        Some(crate::heif::Codec::Hevc) => b"hvc1",
        _ => b"av01",
    };
    let pic = if h.item(h.primary).is_some() {
        h.picture(h.primary, codec)?
    } else {
        Picture::Sample
    };
    let (w, hh) = match (&pic, codec) {
        (Picture::Coded(_), b"hvc1") => (props.width, props.height),
        _ => picture_size(&h, &pic, &props)?,
    };
    let (_, _, cw, ch) = clean_aperture(&props, w, hh);
    if cw == 0 || ch == 0 {
        return Err(Error::InvalidData("heif: zero image size"));
    }
    Ok(if props.rotation & 1 == 1 {
        (ch, cw)
    } else {
        (cw, ch)
    })
}

/// Applies irot and imir to the (scaled) image.
pub(crate) fn transform(img: Image, props: &Props) -> Image {
    let img = match props.rotation {
        1 => exif::apply(img, 8),
        2 => exif::apply(img, 3),
        3 => exif::apply(img, 6),
        _ => img,
    };
    match props.mirror {
        Some(0) => exif::apply(img, 2),
        Some(_) => exif::apply(img, 4),
        None => img,
    }
}

/// YUV rows of a picture (one coded image or a grid), decoded row of
/// tiles by row of tiles.
struct Source<'d> {
    datas: &'d [Cow<'d, [u8]>],
    config: &'d [u8],
    rows: usize,
    cols: usize,
    tile_w: usize,
    tile_h: usize,
    width: usize,
    height: usize,
    ci: ColorInfo,
    decs: Vec<FrameDecoder<'d>>,
    ready: Vec<usize>,
    grid_row: usize,
    fed: usize,
    planes: usize,
    row: [Vec<u16>; 3],
}

impl<'d> Source<'d> {
    fn new(
        datas: &'d [Cow<'d, [u8]>],
        config: &'d [u8],
        rows: usize,
        cols: usize,
        width: usize,
        height: usize,
    ) -> Result<Source<'d>, Error> {
        let first = parse_stream(&datas[0], config)?;
        let c = &first.seq.color;
        let ci = ColorInfo {
            bit_depth: c.bit_depth,
            ssx: c.ssx as usize,
            ssy: c.ssy as usize,
            mono: c.mono,
            full_range: c.full_range,
            primaries: c.color_primaries as u16,
            transfer: c.transfer as u16,
            matrix: c.matrix as u16,
        };
        let (tile_w, tile_h) = (first.fh.upscaled_width, first.fh.frame_height);
        if rows * tile_h < height.min(rows * tile_h) || tile_w == 0 || tile_h == 0 {
            return Err(Error::InvalidData("avif: bad tile size"));
        }
        if rows > 1 || cols > 1 {
            // Grid tiles must cover the output.
            if cols * tile_w < width || rows * tile_h < height {
                return Err(Error::InvalidData("avif: grid smaller than its image"));
            }
        }
        let planes = c.num_planes();
        Ok(Source {
            datas,
            config,
            rows,
            cols,
            tile_w,
            tile_h,
            width,
            height,
            ci,
            decs: Vec::new(),
            ready: Vec::new(),
            grid_row: 0,
            fed: 0,
            planes,
            row: [
                vec![0; width.max(1)],
                vec![0; width.max(1)],
                vec![0; width.max(1)],
            ],
        })
    }

    /// Decodes more rows and hands them to `sink(plane, y, row)`. Returns
    /// false when all rows have been handed out.
    fn advance(&mut self, sink: &mut dyn FnMut(usize, usize, &[u16])) -> Result<bool, Error> {
        if self.grid_row >= self.rows || self.grid_row * self.tile_h >= self.height {
            return Ok(false);
        }
        if self.decs.is_empty() {
            for c in 0..self.cols {
                let s = parse_stream(&self.datas[self.grid_row * self.cols + c], self.config)?;
                if s.fh.upscaled_width != self.tile_w || s.fh.frame_height != self.tile_h {
                    return Err(Error::InvalidData("avif: grid tiles differ in size"));
                }
                if s.seq.color.bit_depth != self.ci.bit_depth
                    || s.seq.color.ssx as usize != self.ci.ssx
                    || s.seq.color.ssy as usize != self.ci.ssy
                    || s.seq.color.mono != self.ci.mono
                {
                    return Err(Error::InvalidData("avif: grid tiles differ in format"));
                }
                self.decs.push(FrameDecoder::new(s));
            }
            self.ready = vec![0; self.cols];
            self.fed = 0;
        }
        let min = *self.ready.iter().min().unwrap_or(&0);
        for (i, d) in self.decs.iter_mut().enumerate() {
            if self.ready[i] == min && min < self.tile_h {
                match d.step()? {
                    Some((_, b)) => self.ready[i] = b,
                    None => self.ready[i] = self.tile_h,
                }
            }
        }
        let new_min = *self.ready.iter().min().unwrap_or(&0);
        let top = self.grid_row * self.tile_h;
        let done = new_min >= self.tile_h;
        // Luma rows.
        for y in self.fed..new_min {
            if top + y >= self.height {
                break;
            }
            self.compose(0, y, self.width, self.tile_w);
            sink(0, top + y, &self.row[0]);
        }
        if self.planes > 1 {
            let (ssx, ssy) = (self.ci.ssx, self.ci.ssy);
            let cw = (self.width + ssx) >> ssx;
            let tcw = (self.tile_w + ssx) >> ssx;
            let ctop = top >> ssy;
            let ch_total = (self.height + ssy) >> ssy;
            let a = self.fed >> ssy;
            let b = if done {
                (self.tile_h + ssy) >> ssy
            } else {
                new_min >> ssy
            };
            for cy in a..b {
                if ctop + cy >= ch_total {
                    break;
                }
                for p in 1..3 {
                    self.compose(p, cy, cw, tcw);
                    sink(p, ctop + cy, &self.row[p]);
                }
            }
            self.fed = if done {
                self.tile_h
            } else {
                (new_min >> ssy) << ssy
            };
        } else {
            self.fed = new_min;
        }
        if done {
            self.decs.clear();
            self.grid_row += 1;
        }
        Ok(true)
    }

    /// Row `y` (tile relative) of plane `p` across the tiles of the row.
    fn compose(&mut self, p: usize, y: usize, width: usize, tile_w: usize) {
        let row = &mut self.row[p];
        row.resize(width, 0);
        for (c, d) in self.decs.iter().enumerate() {
            let x0 = c * tile_w;
            if x0 >= width {
                break;
            }
            let n = tile_w.min(width - x0);
            row[x0..x0 + n].copy_from_slice(&d.out_row(p, y)[..n]);
        }
    }
}

/// Item data of a picture: one entry per coded image.
fn picture_datas<'a>(
    h: &Heif<'a>,
    pic: &Picture,
) -> Result<(Vec<Cow<'a, [u8]>>, Vec<u8>, usize, usize), Error> {
    match pic {
        Picture::Coded(id) => {
            let cfg = h
                .item(*id)
                .map(|i| i.props.config.clone())
                .unwrap_or_default();
            Ok((vec![h.item_data(*id)?], cfg, 1, 1))
        }
        Picture::Grid {
            rows, cols, tiles, ..
        } => {
            let mut v = Vec::with_capacity(tiles.len());
            for &t in tiles {
                v.push(h.item_data(t)?);
            }
            let cfg = h
                .item(tiles[0])
                .map(|i| i.props.config.clone())
                .unwrap_or_default();
            Ok((v, cfg, *rows, *cols))
        }
        Picture::Sample => {
            let s = h.sample().ok_or(Error::InvalidData("avif: no sample"))?;
            let cfg = h
                .track
                .as_ref()
                .map(|t| t.0.config.clone())
                .unwrap_or_default();
            Ok((vec![Cow::Borrowed(s)], cfg, 1, 1))
        }
    }
}

/// Decodes an AVIF scaled to the size `fit` gives (transformations
/// applied to the result).
pub(crate) fn decode_fit(h: &Heif, fit: &Fit) -> Result<Image, Error> {
    let props = h.primary_props();
    let pic = if h.item(h.primary).is_some() {
        h.picture(h.primary, b"av01")?
    } else {
        Picture::Sample
    };
    let (w, hh) = picture_size(h, &pic, &props)?;
    if w == 0 || hh == 0 {
        return Err(Error::InvalidData("avif: zero image size"));
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
    let alpha_pic = match h.item(h.primary).and_then(|_| h.alpha_of(h.primary)) {
        Some(a) => Some((a, h.picture(a, b"av01")?)),
        None => None,
    };
    let (cdatas, ccfg, rows, cols) = picture_datas(h, &pic)?;
    let alpha_parts = match &alpha_pic {
        Some((_, p)) => Some(picture_datas(h, p)?),
        None => None,
    };
    let premultiplied = alpha_pic
        .as_ref()
        .is_some_and(|(a, _)| h.premultiplied(h.primary, *a));
    let (w, hh) = (w as usize, hh as usize);
    let mut color = Source::new(&cdatas, &ccfg, rows, cols, w, hh)?;
    let mut alpha = match &alpha_parts {
        Some((d, c, r, k)) => Some(Source::new(d, c, *r, *k, w, hh)?),
        None => None,
    };
    let mut ci = color.ci;
    if let Some((p, t, m, full)) = props.nclx {
        ci.primaries = p;
        ci.transfer = t;
        ci.matrix = m;
        ci.full_range = full;
    }
    let mut conv = Converter::new(ci, w, hh);
    let mut out = crate::alloc_image(tw, th)?;
    {
        let mut scaler = Scaler::new(Dest::Image(&mut out), cw as usize, ch as usize);
        let mut rgba = vec![0u8; w * 4];
        let mut alpha_rows: VecDeque<(usize, Vec<u8>)> = VecDeque::new();
        let (a_bd, a_full) = alpha
            .as_ref()
            .map_or((8, true), |a| (a.ci.bit_depth, a.ci.full_range));
        let mut alpha_done = alpha.is_none();
        let mut color_done = false;
        loop {
            while let Some(y) = conv.pop(&mut rgba) {
                if let Some(asrc) = alpha.as_mut() {
                    while !alpha_done && alpha_rows.back().map_or(true, |r| r.0 < y) {
                        let q = &mut alpha_rows;
                        let more = asrc.advance(&mut |p, ay, row| {
                            if p == 0 {
                                q.push_back((ay, alpha_bytes(row, a_bd, a_full)));
                            }
                        })?;
                        if !more {
                            alpha_done = true;
                        }
                    }
                    while alpha_rows.front().is_some_and(|r| r.0 < y) {
                        alpha_rows.pop_front();
                    }
                    if let Some((ay, a)) = alpha_rows.front() {
                        if *ay == y {
                            for x in 0..w {
                                let av = a[x];
                                rgba[x * 4 + 3] = av;
                                if premultiplied && av < 255 {
                                    for c in 0..3 {
                                        let v = &mut rgba[x * 4 + c];
                                        *v = if av == 0 {
                                            0
                                        } else {
                                            ((*v as u32 * 255 + av as u32 / 2) / av as u32).min(255)
                                                as u8
                                        };
                                    }
                                }
                            }
                        }
                    }
                }
                let (x0, y0) = (x0 as usize, y0 as usize);
                if y >= y0 && y < y0 + ch as usize {
                    scaler.row(y - y0, &rgba[x0 * 4..(x0 + cw as usize) * 4]);
                }
            }
            if color_done {
                break;
            }
            let c = &mut conv;
            if !color.advance(&mut |p, y, row| {
                if p == 0 {
                    c.push_luma(y, row);
                } else {
                    c.push_chroma(p, y, row);
                }
            })? {
                color_done = true;
            }
        }
        scaler.finish();
    }
    Ok(transform(out, &props))
}

fn alpha_bytes(row: &[u16], bd: u32, full: bool) -> Vec<u8> {
    let max = (1u32 << bd) - 1;
    if full {
        row.iter()
            .map(|&v| ((v as u32 * 255 + max / 2) / max).min(255) as u8)
            .collect()
    } else {
        let s = 1u32 << (bd - 8);
        row.iter()
            .map(|&v| {
                let v = (v as i64 - 16 * s as i64).max(0) as u32;
                ((v * 255 + 219 * s / 2) / (219 * s)).min(255) as u8
            })
            .collect()
    }
}
