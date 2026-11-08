//! YUV to RGBA conversion of decoded AV1 images: matrix coefficients and
//! range (from the colr box or the sequence header), 8 to 12 bits, and
//! "fancy" (triangle filter) chroma upsampling with chroma sited between
//! the luma samples. HDR transfer functions (PQ, HLG) are tone mapped to
//! SDR so that such images do not look washed out.
//!
//! Rows arrive in order (luma and chroma separately, as the decoder
//! finishes them); a luma row is converted as soon as the chroma rows it
//! interpolates from are there.

use std::collections::VecDeque;

#[derive(Debug, Clone, Copy)]
pub(crate) struct ColorInfo {
    pub bit_depth: u32,
    pub ssx: usize,
    pub ssy: usize,
    pub mono: bool,
    pub full_range: bool,
    pub primaries: u16,
    pub transfer: u16,
    pub matrix: u16,
}

pub(crate) struct Converter {
    ci: ColorInfo,
    width: usize,
    height: usize,
    cw: usize,
    ch: usize,
    luma: VecDeque<(usize, Vec<u16>)>,
    chroma: [VecDeque<(usize, Vec<u16>)>; 2],
    next: usize,
    // Fixed point (16 fraction bits) coefficients.
    ky: i64,
    yo: i64,
    co: i64,
    r_cr: i64,
    g_cb: i64,
    g_cr: i64,
    b_cb: i64,
    identity: bool,
    ycgco: bool,
    hdr: Option<Box<Hdr>>,
    u: Vec<i32>,
    v: Vec<i32>,
}

/// Tone mapping of PQ / HLG images: lookup from the non-linear signal (12
/// bit index) to linear light, and from tone mapped linear light to sRGB.
struct Hdr {
    to_linear: Vec<f32>,
    to_srgb: Vec<u8>,
    bt2020: bool,
}

impl Converter {
    pub fn new(ci: ColorInfo, width: usize, height: usize) -> Converter {
        let s = (1i64 << ci.bit_depth) as f64;
        let max = s - 1.0;
        let (ky, yo, kc) = if ci.full_range {
            (255.0 / max, 0.0, 255.0 / max)
        } else {
            let k = s / 256.0;
            (255.0 / (219.0 * k), 16.0 * k, 255.0 / (224.0 * k))
        };
        let (kr, kb) = match ci.matrix {
            1 => (0.2126, 0.0722),
            4 => (0.30, 0.11),
            5 | 6 => (0.299, 0.114),
            7 => (0.212, 0.087),
            9 | 10 => (0.2627, 0.0593),
            12 | 13 => match ci.primaries {
                1 => (0.2126, 0.0722),
                9 => (0.2627, 0.0593),
                _ => (0.299, 0.114),
            },
            _ => (0.299, 0.114),
        };
        let kg = 1.0 - kr - kb;
        let f = 65536.0;
        let fx = |v: f64| (v * f).round() as i64;
        let hdr = if ci.transfer == 16 || ci.transfer == 18 {
            Some(Box::new(Hdr::new(ci.transfer, ci.primaries == 9)))
        } else {
            None
        };
        Converter {
            width,
            height,
            cw: (width + ci.ssx) >> ci.ssx,
            ch: (height + ci.ssy) >> ci.ssy,
            luma: VecDeque::new(),
            chroma: [VecDeque::new(), VecDeque::new()],
            next: 0,
            ky: fx(ky),
            yo: (yo * f) as i64,
            co: (s / 2.0) as i64,
            r_cr: fx(kc * (2.0 - 2.0 * kr)),
            g_cb: fx(kc * (2.0 * kb * (1.0 - kb) / kg)),
            g_cr: fx(kc * (2.0 * kr * (1.0 - kr) / kg)),
            b_cb: fx(kc * (2.0 - 2.0 * kb)),
            identity: ci.matrix == 0,
            ycgco: ci.matrix == 8,
            hdr,
            u: vec![0; width],
            v: vec![0; width],
            ci,
        }
    }

    pub fn push_luma(&mut self, y: usize, row: &[u16]) {
        if y >= self.next {
            self.luma.push_back((y, row[..self.width].to_vec()));
        }
    }

    /// `p` is 1 (U) or 2 (V).
    pub fn push_chroma(&mut self, p: usize, y: usize, row: &[u16]) {
        let q = &mut self.chroma[p - 1];
        q.push_back((y, row[..self.cw].to_vec()));
        // Keep the rows the next luma row can still need.
        let need = (self.next >> self.ci.ssy).saturating_sub(1);
        while q.len() > 1 && q[0].0 < need {
            q.pop_front();
        }
    }

    fn chroma_row(&self, p: usize, y: usize) -> Option<&[u16]> {
        self.chroma[p - 1]
            .iter()
            .find(|(r, _)| *r == y)
            .map(|(_, v)| v.as_slice())
    }

    /// Converts the next luma row if its inputs are there; `out` gets
    /// RGBA (alpha 255). Returns the row number.
    pub fn pop(&mut self, out: &mut [u8]) -> Option<usize> {
        let y = self.next;
        if y >= self.height {
            return None;
        }
        let (ly, _) = self.luma.front()?;
        if *ly != y {
            return None;
        }
        let w = self.width;
        if !self.ci.mono {
            if !self.upsample(y) {
                return None;
            }
        }
        let (_, lrow) = self.luma.pop_front()?;
        self.next += 1;
        if self.ci.mono {
            for x in 0..w {
                self.u[x] = self.co as i32;
                self.v[x] = self.co as i32;
            }
        }
        let bd = self.ci.bit_depth;
        if self.identity || self.ycgco {
            let max = (1i64 << bd) - 1;
            let (k, o) = if self.ci.full_range {
                (255.0 / max as f64, 0.0)
            } else {
                let s = (1i64 << bd) as f64 / 256.0;
                (255.0 / (219.0 * s), 16.0 * s)
            };
            let conv = |v: i64| -> u8 { ((v as f64 - o) * k).round().clamp(0.0, 255.0) as u8 };
            for x in 0..w {
                let (yy, cb, cr) = (lrow[x] as i64, self.u[x] as i64, self.v[x] as i64);
                let (r, g, b) = if self.identity {
                    (cr, yy, cb)
                } else {
                    let cg = cb - self.co;
                    let cov = cr - self.co;
                    let t = yy - cg;
                    (t + cov, yy + cg, t - cov)
                };
                let o4 = &mut out[x * 4..x * 4 + 4];
                o4[0] = conv(r);
                o4[1] = conv(g);
                o4[2] = conv(b);
                o4[3] = 255;
            }
            return Some(y);
        }
        if let Some(h) = &self.hdr {
            // Non-linear R'G'B' in [0, 1], then the transfer function.
            let scale = 1.0 / (255.0 * 65536.0);
            for x in 0..w {
                let yv = ((((lrow[x] as i64) << 16) - self.yo) * self.ky) >> 16;
                let cb = self.u[x] as i64 - self.co;
                let cr = self.v[x] as i64 - self.co;
                let r = (yv + self.r_cr * cr) as f32 * scale as f32;
                let g = (yv - self.g_cb * cb - self.g_cr * cr) as f32 * scale as f32;
                let b = (yv + self.b_cb * cb) as f32 * scale as f32;
                let px = h.map([r, g, b]);
                let o4 = &mut out[x * 4..x * 4 + 4];
                o4[..3].copy_from_slice(&px);
                o4[3] = 255;
            }
            return Some(y);
        }
        for x in 0..w {
            let yv = ((lrow[x] as i64) << 16) - self.yo;
            let yv = (yv * self.ky) >> 16;
            let cb = self.u[x] as i64 - self.co;
            let cr = self.v[x] as i64 - self.co;
            let r = yv + self.r_cr * cr;
            let g = yv - self.g_cb * cb - self.g_cr * cr;
            let b = yv + self.b_cb * cb;
            let o4 = &mut out[x * 4..x * 4 + 4];
            o4[0] = ((r + 32768) >> 16).clamp(0, 255) as u8;
            o4[1] = ((g + 32768) >> 16).clamp(0, 255) as u8;
            o4[2] = ((b + 32768) >> 16).clamp(0, 255) as u8;
            o4[3] = 255;
        }
        Some(y)
    }

    /// Full-width chroma for luma row `y` into `self.u` / `self.v`.
    fn upsample(&mut self, y: usize) -> bool {
        let (ssx, ssy) = (self.ci.ssx, self.ci.ssy);
        let (near, far) = if ssy == 1 {
            let n = y >> 1;
            let f = if y & 1 == 0 {
                n.saturating_sub(1)
            } else {
                (n + 1).min(self.ch - 1)
            };
            (n, f)
        } else {
            (y, y)
        };
        let mut uv = [std::mem::take(&mut self.u), std::mem::take(&mut self.v)];
        let mut ok = true;
        for (p, dst) in uv.iter_mut().enumerate() {
            let (a, b) = match (self.chroma_row(p + 1, near), self.chroma_row(p + 1, far)) {
                (Some(a), Some(b)) => (a, b),
                _ => {
                    ok = false;
                    break;
                }
            };
            let cw = self.cw;
            if ssx == 0 {
                for x in 0..self.width {
                    dst[x] = if ssy == 1 {
                        (3 * a[x] as i32 + b[x] as i32 + 2) >> 2
                    } else {
                        a[x] as i32
                    };
                }
            } else {
                // Vertical blend (x4 when subsampled vertically), then the
                // horizontal triangle filter.
                let vert = |i: usize| -> i32 {
                    if ssy == 1 {
                        3 * a[i] as i32 + b[i] as i32
                    } else {
                        4 * a[i] as i32
                    }
                };
                for i in 0..cw {
                    let c = vert(i);
                    let l = vert(i.saturating_sub(1));
                    let r = vert((i + 1).min(cw - 1));
                    let x0 = 2 * i;
                    if x0 < self.width {
                        dst[x0] = (3 * c + l + 8) >> 4;
                    }
                    if x0 + 1 < self.width {
                        dst[x0 + 1] = (3 * c + r + 8) >> 4;
                    }
                }
            }
        }
        self.u = std::mem::take(&mut uv[0]);
        self.v = std::mem::take(&mut uv[1]);
        if self.u.len() < self.width {
            self.u.resize(self.width, 0);
            self.v.resize(self.width, 0);
        }
        ok
    }
}

impl Hdr {
    fn new(transfer: u16, bt2020: bool) -> Hdr {
        let n = 4096;
        let mut to_linear = Vec::with_capacity(n + 1);
        for i in 0..=n {
            let e = i as f64 / n as f64;
            // Linear light relative to SDR reference white (203 cd/m2).
            let l = if transfer == 16 {
                let m1 = 2610.0 / 16384.0;
                let m2 = 2523.0 / 4096.0 * 128.0;
                let c1 = 3424.0 / 4096.0;
                let c2 = 2413.0 / 4096.0 * 32.0;
                let c3 = 2392.0 / 4096.0 * 32.0;
                let p = e.powf(1.0 / m2);
                let v = ((p - c1).max(0.0) / (c2 - c3 * p)).powf(1.0 / m1);
                v * 10000.0 / 203.0
            } else {
                let (a, b, c) = (0.17883277, 0.28466892, 0.55991073);
                let v = if e <= 0.5 {
                    e * e / 3.0
                } else {
                    (((e - c) / a).exp() + b) / 12.0
                };
                // Scene light with a nominal peak of 1000 cd/m2.
                v * 1000.0 / 203.0
            };
            to_linear.push(l as f32);
        }
        let mut to_srgb = Vec::with_capacity(n + 1);
        for i in 0..=n {
            let v = i as f64 / n as f64;
            let s = if v <= 0.0031308 {
                12.92 * v
            } else {
                1.055 * v.powf(1.0 / 2.4) - 0.055
            };
            to_srgb.push((s * 255.0).round().clamp(0.0, 255.0) as u8);
        }
        Hdr {
            to_linear,
            to_srgb,
            bt2020,
        }
    }

    fn map(&self, rgb: [f32; 3]) -> [u8; 3] {
        let n = (self.to_linear.len() - 1) as f32;
        let lin = |v: f32| self.to_linear[(v.clamp(0.0, 1.0) * n) as usize];
        let (mut r, mut g, mut b) = (lin(rgb[0]), lin(rgb[1]), lin(rgb[2]));
        if self.bt2020 {
            let (r2, g2, b2) = (r, g, b);
            r = 1.6605 * r2 - 0.5876 * g2 - 0.0728 * b2;
            g = -0.1246 * r2 + 1.1329 * g2 - 0.0083 * b2;
            b = -0.0182 * r2 - 0.1006 * g2 + 1.1187 * b2;
        }
        // Extended Reinhard on the maximum channel, white at 4x reference.
        let m = r.max(g).max(b).max(1e-6);
        let w = 4.0f32;
        let t = m * (1.0 + m / (w * w)) / (1.0 + m);
        let k = t / m;
        let enc = |v: f32| self.to_srgb[((v * k).clamp(0.0, 1.0) * n) as usize];
        [enc(r), enc(g), enc(b)]
    }
}
