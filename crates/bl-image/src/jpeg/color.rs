//! Chroma upsampling ("fancy" triangle filter, i.e. bilinear interpolation
//! with co-sited centres, for any integer or fractional ratio) and color
//! conversion (YCbCr, RGB, gray, Adobe CMYK / YCCK) to RGBA.

use crate::Image;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ColorSpace {
    Gray,
    Rgb,
    YCbCr,
    /// Adobe (inverted) CMYK.
    Cmyk,
    /// Adobe YCCK (YCbCr-coded inverted CMY + K).
    Ycck,
    /// BT.601 limited range Y'CbCr (VP8 / WebP).
    YuvLimited,
}

pub struct Plane<'a> {
    pub data: &'a [u8],
    pub stride: usize,
    /// Valid samples (edge samples are replicated beyond these).
    pub width: usize,
    pub height: usize,
    /// Plane samples per output pixel: hn / hd horizontally, vn / vd
    /// vertically.
    pub hn: usize,
    pub hd: usize,
    pub vn: usize,
    pub vd: usize,
}

/// Interpolation taps: output `o` = (s[i0] * (256 - w1) + s[i1] * w1) / 256.
#[derive(Clone, Copy)]
struct Tap {
    i0: u32,
    i1: u32,
    w1: u32,
}

fn taps(n_out: usize, n_in: usize, factor: usize, max: usize) -> Vec<Tap> {
    (0..n_out)
        .map(|o| {
            // Source position (o + 0.5) * factor / max - 0.5.
            let num = (2 * o + 1) as i64 * factor as i64 - max as i64;
            let den = 2 * max as i64;
            if num <= 0 {
                return Tap {
                    i0: 0,
                    i1: 0,
                    w1: 0,
                };
            }
            let i0 = (num / den) as usize;
            let w1 = ((num % den) * 256 + den / 2) / den;
            let i0c = i0.min(n_in - 1);
            let i1c = (i0 + 1).min(n_in - 1);
            Tap {
                i0: i0c as u32,
                i1: i1c as u32,
                w1: w1 as u32,
            }
        })
        .collect()
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    /// Same resolution as the output: rows are used in place.
    Full,
    /// libjpeg-compatible "fancy" (triangle) filters for 2:1 ratios.
    H2V1,
    H2V2,
    H1V2,
    /// Bilinear interpolation for any other ratio.
    Generic,
}

/// Produces full-resolution rows of one plane.
struct Upsampler<'a> {
    p: &'a Plane<'a>,
    kind: Kind,
    /// Rounding bias for odd output columns of the h2v2 filter: libjpeg
    /// alternates 8 / 7 to avoid a systematic bias, libwebp always uses 8.
    odd_bias: u16,
    htaps: Vec<Tap>,
    vtaps: Vec<Tap>,
    vrow: Vec<u16>,
    out: Vec<u8>,
}

impl<'a> Upsampler<'a> {
    fn new(p: &'a Plane<'a>, w: usize, h: usize, odd_bias: u16) -> Self {
        let hr = (p.hn, p.hd);
        let vr = (p.vn, p.vd);
        let same = |(n, d): (usize, usize)| n == d;
        let half = |(n, d): (usize, usize)| 2 * n == d;
        let kind = if same(hr) && same(vr) && p.width >= w && p.height >= h {
            Kind::Full
        } else if half(hr) && same(vr) && p.width * 2 >= w && p.height >= h {
            Kind::H2V1
        } else if half(hr) && half(vr) && p.width * 2 >= w && p.height * 2 >= h {
            Kind::H2V2
        } else if same(hr) && half(vr) && p.width >= w && p.height * 2 >= h {
            Kind::H1V2
        } else {
            Kind::Generic
        };
        let generic = kind == Kind::Generic;
        Upsampler {
            p,
            kind,
            odd_bias,
            htaps: if generic {
                taps(w, p.width, p.hn, p.hd)
            } else {
                Vec::new()
            },
            vtaps: if generic {
                taps(h, p.height, p.vn, p.vd)
            } else {
                Vec::new()
            },
            vrow: vec![0; p.width],
            out: vec![0; (p.width * 2).max(w)],
        }
    }

    #[inline]
    fn plane_row(&self, y: usize) -> &'a [u8] {
        let p = self.p;
        let y = y.min(p.height - 1);
        &p.data[y * p.stride..y * p.stride + p.width]
    }

    /// Computes output row `y` into the internal buffer (no-op for Full).
    fn fill(&mut self, y: usize, w: usize) {
        match self.kind {
            Kind::Full => {}
            Kind::H2V1 => {
                let r = self.plane_row(y);
                h2_fancy_u8(r, &mut self.out);
            }
            Kind::H2V2 => {
                let k = y / 2;
                let near = self.plane_row(k);
                let far = if y % 2 == 0 {
                    self.plane_row(k.saturating_sub(1))
                } else {
                    self.plane_row(k + 1)
                };
                for ((d, &a), &b) in self.vrow.iter_mut().zip(near).zip(far) {
                    *d = 3 * a as u16 + b as u16;
                }
                h2v2_fancy(&self.vrow, &mut self.out, self.odd_bias);
            }
            Kind::H1V2 => {
                let k = y / 2;
                let near = self.plane_row(k);
                let (far, bias) = if y % 2 == 0 {
                    (self.plane_row(k.saturating_sub(1)), 1)
                } else {
                    (self.plane_row(k + 1), 2)
                };
                for ((o, &a), &b) in self.out.iter_mut().zip(near).zip(far) {
                    *o = ((3 * a as u16 + b as u16 + bias) >> 2) as u8;
                }
            }
            Kind::Generic => {
                let p = self.p;
                let t = self.vtaps[y];
                let r0 = &p.data[t.i0 as usize * p.stride..t.i0 as usize * p.stride + p.width];
                let r1 = &p.data[t.i1 as usize * p.stride..t.i1 as usize * p.stride + p.width];
                let (w0, w1) = (256 - t.w1 as u16, t.w1 as u16);
                for ((d, &a), &b) in self.vrow.iter_mut().zip(r0).zip(r1) {
                    *d = a as u16 * w0 + b as u16 * w1;
                }
                for (o, t) in self.out[..w].iter_mut().zip(&self.htaps) {
                    let a = self.vrow[t.i0 as usize] as u32;
                    let b = self.vrow[t.i1 as usize] as u32;
                    *o = ((a * (256 - t.w1) + b * t.w1 + 32768) >> 16) as u8;
                }
            }
        }
    }

    /// The row computed by the last `fill(y, w)`.
    fn get(&self, y: usize, w: usize) -> &[u8] {
        match self.kind {
            Kind::Full => &self.plane_row(y)[..w],
            _ => &self.out[..w],
        }
    }
}

/// Horizontal 2x triangle upsampling (libjpeg h2v1 rounding).
fn h2_fancy_u8(r: &[u8], out: &mut [u8]) {
    let n = r.len();
    if n == 1 {
        out[0] = r[0];
        out[1] = r[0];
        return;
    }
    out[0] = r[0];
    out[1] = ((3 * r[0] as u16 + r[1] as u16 + 2) >> 2) as u8;
    for i in 1..n - 1 {
        let t = 3 * r[i] as u16;
        out[2 * i] = ((t + r[i - 1] as u16 + 1) >> 2) as u8;
        out[2 * i + 1] = ((t + r[i + 1] as u16 + 2) >> 2) as u8;
    }
    out[2 * n - 2] = ((3 * r[n - 1] as u16 + r[n - 2] as u16 + 1) >> 2) as u8;
    out[2 * n - 1] = r[n - 1];
}

/// Horizontal pass of h2v2 fancy upsampling on column sums (3*near + far).
fn h2v2_fancy(c: &[u16], out: &mut [u8], odd: u16) {
    let n = c.len();
    if n == 1 {
        out[0] = ((c[0] * 4 + 8) >> 4) as u8;
        out[1] = ((c[0] * 4 + odd) >> 4) as u8;
        return;
    }
    out[0] = ((c[0] * 4 + 8) >> 4) as u8;
    out[1] = ((c[0] * 3 + c[1] + odd) >> 4) as u8;
    let (body, _) = out[2..].split_at_mut(2 * (n - 2));
    for (o, w) in body.chunks_exact_mut(2).zip(c.windows(3)) {
        let t = w[1] * 3;
        o[0] = ((t + w[0] + 8) >> 4) as u8;
        o[1] = ((t + w[2] + odd) >> 4) as u8;
    }
    out[2 * n - 2] = ((c[n - 1] * 3 + c[n - 2] + 8) >> 4) as u8;
    out[2 * n - 1] = ((c[n - 1] * 4 + odd) >> 4) as u8;
}

/// Fixed-point (16 bit) JFIF YCbCr -> RGB lookup tables.
struct YccTables {
    cr_r: [i32; 256],
    cb_b: [i32; 256],
    cr_g: [i32; 256],
    cb_g: [i32; 256],
}

impl YccTables {
    fn new() -> Self {
        let mut t = YccTables {
            cr_r: [0; 256],
            cb_b: [0; 256],
            cr_g: [0; 256],
            cb_g: [0; 256],
        };
        for i in 0..256 {
            let x = i as i32 - 128;
            t.cr_r[i] = (91881 * x + 32768) >> 16;
            t.cb_b[i] = (116130 * x + 32768) >> 16;
            t.cr_g[i] = -46802 * x;
            t.cb_g[i] = -22554 * x + 32768;
        }
        t
    }

    #[inline(always)]
    fn rgb(&self, y: u8, cb: u8, cr: u8) -> [u8; 3] {
        let y = y as i32;
        let r = y + self.cr_r[cr as usize];
        let g = y + ((self.cb_g[cb as usize] + self.cr_g[cr as usize]) >> 16);
        let b = y + self.cb_b[cb as usize];
        [
            r.clamp(0, 255) as u8,
            g.clamp(0, 255) as u8,
            b.clamp(0, 255) as u8,
        ]
    }
}

/// Fixed-point (14 bit) BT.601 limited range Y'CbCr -> RGB tables.
struct LimitedTables {
    y: [i32; 256],
    v_r: [i32; 256],
    u_g: [i32; 256],
    v_g: [i32; 256],
    u_b: [i32; 256],
}

impl LimitedTables {
    fn new() -> Self {
        let mut t = LimitedTables {
            y: [0; 256],
            v_r: [0; 256],
            u_g: [0; 256],
            v_g: [0; 256],
            u_b: [0; 256],
        };
        let f = |x: f64| (x * 16384.0).round() as i32;
        for i in 0..256 {
            let c = i as f64 - 128.0;
            t.y[i] = f(1.164_383 * (i as f64 - 16.0)) + 8192;
            t.v_r[i] = f(1.596_027 * c);
            t.u_g[i] = f(-0.391_762 * c);
            t.v_g[i] = f(-0.812_968 * c);
            t.u_b[i] = f(2.017_232 * c);
        }
        t
    }

    #[inline(always)]
    fn rgb(&self, y: u8, u: u8, v: u8) -> [u8; 3] {
        let yy = self.y[y as usize];
        let r = (yy + self.v_r[v as usize]) >> 14;
        let g = (yy + self.u_g[u as usize] + self.v_g[v as usize]) >> 14;
        let b = (yy + self.u_b[u as usize]) >> 14;
        [
            r.clamp(0, 255) as u8,
            g.clamp(0, 255) as u8,
            b.clamp(0, 255) as u8,
        ]
    }
}

/// a * b / 255, rounded.
#[inline(always)]
fn mul255(a: u8, b: u8) -> u8 {
    let t = a as u32 * b as u32 + 128;
    ((t + (t >> 8)) >> 8) as u8
}

pub fn convert(planes: &[Plane], cs: ColorSpace, img: &mut Image) {
    let w = img.width as usize;
    let h = img.height as usize;
    let mut ups: Vec<Upsampler> = planes
        .iter()
        .map(|p| Upsampler::new(p, w, h, if cs == ColorSpace::YuvLimited { 8 } else { 7 }))
        .collect();
    let ycc = YccTables::new();
    let limited = LimitedTables::new();
    for y in 0..h {
        for u in ups.iter_mut() {
            u.fill(y, w);
        }
        let mut rows: [&[u8]; 4] = [&[]; 4];
        for (r, u) in rows.iter_mut().zip(&ups) {
            *r = u.get(y, w);
        }
        let out = &mut img.pixels[y * w * 4..(y + 1) * w * 4];
        match cs {
            ColorSpace::Gray => {
                for (o, &g) in out.chunks_exact_mut(4).zip(rows[0]) {
                    o.copy_from_slice(&[g, g, g, 255]);
                }
            }
            ColorSpace::Rgb => {
                for (x, o) in out.chunks_exact_mut(4).enumerate() {
                    o.copy_from_slice(&[rows[0][x], rows[1][x], rows[2][x], 255]);
                }
            }
            ColorSpace::YCbCr => {
                let (ry, rb, rr) = (rows[0], rows[1], rows[2]);
                for (((o, &yy), &cb), &cr) in out.chunks_exact_mut(4).zip(ry).zip(rb).zip(rr) {
                    let [r, g, b] = ycc.rgb(yy, cb, cr);
                    o.copy_from_slice(&[r, g, b, 255]);
                }
            }
            ColorSpace::YuvLimited => {
                let t = &limited;
                let (ry, ru, rv) = (rows[0], rows[1], rows[2]);
                for (((o, &yy), &u), &v) in out.chunks_exact_mut(4).zip(ry).zip(ru).zip(rv) {
                    let [r, g, b] = t.rgb(yy, u, v);
                    o.copy_from_slice(&[r, g, b, 255]);
                }
            }
            ColorSpace::Cmyk => {
                for (x, o) in out.chunks_exact_mut(4).enumerate() {
                    let k = rows[3][x];
                    o.copy_from_slice(&[
                        mul255(rows[0][x], k),
                        mul255(rows[1][x], k),
                        mul255(rows[2][x], k),
                        255,
                    ]);
                }
            }
            ColorSpace::Ycck => {
                for (x, o) in out.chunks_exact_mut(4).enumerate() {
                    let [r, g, b] = ycc.rgb(rows[0][x], rows[1][x], rows[2][x]);
                    let k = rows[3][x];
                    o.copy_from_slice(&[
                        mul255(255 - r, k),
                        mul255(255 - g, k),
                        mul255(255 - b, k),
                        255,
                    ]);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn convert_one(cs: ColorSpace, samples: &[u8]) -> [u8; 4] {
        let planes: Vec<Vec<u8>> = samples.iter().map(|&s| vec![s]).collect();
        let p: Vec<Plane> = planes
            .iter()
            .map(|d| Plane {
                data: d,
                stride: 1,
                width: 1,
                height: 1,
                hn: 1,
                hd: 1,
                vn: 1,
                vd: 1,
            })
            .collect();
        let mut img = Image::new(1, 1);
        convert(&p, cs, &mut img);
        [img.pixels[0], img.pixels[1], img.pixels[2], img.pixels[3]]
    }

    #[test]
    fn cmyk_matches_ycck_relation() {
        // YCCK is verified against libjpeg-turbo; Adobe CMYK must give the
        // same result for the equivalent (inverted) CMY values.
        for (y, cb, cr, k) in [
            (10u8, 128u8, 128u8, 255u8),
            (200, 90, 170, 128),
            (128, 128, 128, 0),
            (76, 85, 255, 200),
        ] {
            let ycck = convert_one(ColorSpace::Ycck, &[y, cb, cr, k]);
            let [r, g, b, _] = convert_one(ColorSpace::YCbCr, &[y, cb, cr]);
            let cmyk = convert_one(ColorSpace::Cmyk, &[255 - r, 255 - g, 255 - b, k]);
            assert_eq!(ycck, cmyk);
        }
        assert_eq!(
            convert_one(ColorSpace::Cmyk, &[255, 255, 255, 255]),
            [255, 255, 255, 255]
        );
        assert_eq!(
            convert_one(ColorSpace::Cmyk, &[0, 255, 255, 255]),
            [0, 255, 255, 255]
        );
    }

    #[test]
    fn fancy_upsampling_edges() {
        let mut out = [0u8; 6];
        h2_fancy_u8(&[0, 100, 200], &mut out);
        assert_eq!(out, [0, 25, 75, 125, 175, 200]);
        let mut out = [0u8; 4];
        h2v2_fancy(&[400, 800], &mut out, 7); // column sums of constant rows 100, 200
        assert_eq!(out, [100, 125, 175, 200]);
    }
}
