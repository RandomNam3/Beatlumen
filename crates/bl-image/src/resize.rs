//! Separable resampling: area averaging (box filter with fractional
//! coverage) when shrinking, bilinear interpolation when enlarging; each axis
//! is handled independently. Filtering happens on premultiplied alpha in
//! 16-bit fixed point (sRGB values are averaged directly).

use crate::Image;
use std::borrow::Cow;

/// Returns the pixel buffer padded / truncated to exactly width*height*4
/// bytes, so that callers can index without checks even when the public
/// fields of `Image` are inconsistent.
pub(crate) fn checked_pixels(img: &Image) -> Cow<'_, [u8]> {
    let n = img.width as usize * img.height as usize * 4;
    if img.pixels.len() >= n {
        Cow::Borrowed(&img.pixels[..n])
    } else {
        let mut v = img.pixels.clone();
        v.resize(n, 0);
        Cow::Owned(v)
    }
}

const WBITS: u32 = 14;
const ONE: i64 = 1 << WBITS;

/// Filter taps for one axis: output `i` reads `count[i]` consecutive input
/// samples starting at `first[i]` with weights `w[offs[i]..]` (sum = 2^14).
struct Taps {
    first: Vec<usize>,
    count: Vec<usize>,
    offs: Vec<usize>,
    w: Vec<u32>,
}

impl Taps {
    /// `extent` (<= n_in) is the part of the input in pixels that maps onto
    /// the output; the last input pixel may be partially covered.
    fn new(n_in: usize, extent: f64, n_out: usize) -> Taps {
        let mut t = Taps {
            first: Vec::with_capacity(n_out),
            count: Vec::new(),
            offs: Vec::new(),
            w: Vec::new(),
        };
        let extent = extent.clamp(f64::MIN_POSITIVE, n_in as f64);
        let scale = extent / n_out as f64;
        let mut fw: Vec<f64> = Vec::new();
        for o in 0..n_out {
            fw.clear();
            let first;
            if (n_out as f64) < extent {
                // Area averaging over [lo, hi) in input coordinates.
                let lo = o as f64 * scale;
                let hi = ((o + 1) as f64 * scale).min(extent);
                first = (lo.floor() as usize).min(n_in - 1);
                let last = (hi.ceil() as usize).clamp(first + 1, n_in);
                for i in first..last {
                    let ov = hi.min(i as f64 + 1.0) - lo.max(i as f64);
                    fw.push(ov.max(0.0) / scale);
                }
                if fw.iter().sum::<f64>() <= 0.0 {
                    fw.iter_mut().for_each(|x| *x = 1.0);
                }
            } else {
                // Bilinear with pixel centers aligned.
                let src = ((o as f64 + 0.5) * scale - 0.5).clamp(0.0, (n_in - 1) as f64);
                let i0 = (src.floor() as usize).min(n_in - 1);
                let f = src - i0 as f64;
                first = i0;
                fw.push(1.0 - f);
                if i0 + 1 < n_in && f > 0.0 {
                    fw.push(f);
                }
            }
            // Quantize, then fix rounding so the weights sum to exactly ONE.
            let start = t.w.len();
            let total: f64 = fw.iter().sum();
            let mut sum = 0i64;
            for &x in &fw {
                let q = (x / total * ONE as f64).round() as i64;
                t.w.push(q as u32);
                sum += q;
            }
            let (imax, _) = fw
                .iter()
                .enumerate()
                .fold((0, -1.0), |m, (i, &x)| if x > m.1 { (i, x) } else { m });
            let adj = t.w[start + imax] as i64 + (ONE - sum);
            t.w[start + imax] = adj.max(0) as u32;
            t.first.push(first);
            t.count.push(fw.len());
            t.offs.push(start);
        }
        t
    }

    #[inline]
    fn weights(&self, o: usize) -> &[u32] {
        &self.w[self.offs[o]..self.offs[o] + self.count[o]]
    }
}

pub fn resize(img: &Image, width: u32, height: u32) -> Image {
    resize_extent(img, img.width as f64, img.height as f64, width, height)
}

/// Resizes the region [0, ext_w) x [0, ext_h) of `img` (fractional extents
/// allow for partially covered last rows / columns, as produced by
/// DCT-scaled JPEG decoding).
pub(crate) fn resize_extent(img: &Image, ext_w: f64, ext_h: f64, width: u32, height: u32) -> Image {
    let (iw, ih) = (img.width as usize, img.height as usize);
    let (ow, oh) = (width as usize, height as usize);
    if ow == 0 || oh == 0 {
        return Image {
            width,
            height,
            pixels: Vec::new(),
        };
    }
    if iw == 0 || ih == 0 {
        return Image::new(width, height);
    }
    let src = checked_pixels(img);
    if iw == ow && ih == oh && ext_w == iw as f64 && ext_h == ih as f64 {
        return Image {
            width,
            height,
            pixels: src.into_owned(),
        };
    }
    let tx = Taps::new(iw, ext_w, ow);
    let ty = Taps::new(ih, ext_h, oh);

    // Ring buffer of horizontally resampled rows (premultiplied u16).
    let k = ty.count.iter().copied().max().unwrap_or(1);
    let mut cache: Vec<Vec<u16>> = vec![vec![0u16; ow * 4]; k];
    let mut cached_row: Vec<usize> = vec![usize::MAX; k];
    let mut premul = vec![0u16; iw * 4];
    let mut acc = vec![0u32; ow * 4];
    let mut out = vec![0u8; ow * oh * 4];

    for oy in 0..oh {
        let first = ty.first[oy];
        let wy = ty.weights(oy);
        acc.iter_mut().for_each(|a| *a = 1 << (WBITS - 1));
        for (j, &w) in wy.iter().enumerate() {
            let iy = first + j;
            let slot = iy % k;
            if cached_row[slot] != iy {
                let row = &src[iy * iw * 4..(iy + 1) * iw * 4];
                for (p, s) in premul.chunks_exact_mut(4).zip(row.chunks_exact(4)) {
                    let a = s[3] as u16;
                    p[0] = s[0] as u16 * a;
                    p[1] = s[1] as u16 * a;
                    p[2] = s[2] as u16 * a;
                    p[3] = a * 255;
                }
                resample_row(&premul, &mut cache[slot], &tx);
                cached_row[slot] = iy;
            }
            for (a, &v) in acc.iter_mut().zip(&cache[slot]) {
                *a += w * v as u32;
            }
        }
        let orow = &mut out[oy * ow * 4..(oy + 1) * ow * 4];
        for (o, a) in orow.chunks_exact_mut(4).zip(acc.chunks_exact(4)) {
            let al = a[3] >> WBITS;
            if al == 0 {
                o.copy_from_slice(&[0, 0, 0, 0]);
            } else if al >= 65025 {
                for c in 0..3 {
                    o[c] = (((a[c] >> WBITS) + 127) / 255).min(255) as u8;
                }
                o[3] = 255;
            } else {
                for c in 0..3 {
                    o[c] = (((a[c] >> WBITS) * 255 + al / 2) / al).min(255) as u8;
                }
                o[3] = ((al + 127) / 255) as u8;
            }
        }
    }
    Image {
        width,
        height,
        pixels: out,
    }
}

fn resample_row(src: &[u16], dst: &mut [u16], tx: &Taps) {
    for (ox, d) in dst.chunks_exact_mut(4).enumerate() {
        let first = tx.first[ox];
        let mut acc = [1u32 << (WBITS - 1); 4];
        for (j, &w) in tx.weights(ox).iter().enumerate() {
            let p = &src[(first + j) * 4..(first + j) * 4 + 4];
            acc[0] += w * p[0] as u32;
            acc[1] += w * p[1] as u32;
            acc[2] += w * p[2] as u32;
            acc[3] += w * p[3] as u32;
        }
        for c in 0..4 {
            d[c] = (acc[c] >> WBITS).min(65025) as u16;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn box_downscale_averages() {
        let mut img = Image::new(4, 2);
        for (i, p) in img.pixels.chunks_exact_mut(4).enumerate() {
            let v = if i % 2 == 0 { 0 } else { 200 };
            p.copy_from_slice(&[v, v, v, 255]);
        }
        let r = resize(&img, 2, 1);
        assert_eq!(r.pixels, vec![100, 100, 100, 255, 100, 100, 100, 255]);
    }

    #[test]
    fn premultiplied_alpha() {
        // Transparent red next to opaque blue: the result must not be purple.
        let img = Image {
            width: 2,
            height: 1,
            pixels: vec![255, 0, 0, 0, 0, 0, 255, 255],
        };
        let r = resize(&img, 1, 1);
        assert_eq!(r.pixels, vec![0, 0, 255, 128]);
    }

    #[test]
    fn upscale_and_identity() {
        let img = Image {
            width: 2,
            height: 1,
            pixels: vec![0, 0, 0, 255, 255, 255, 255, 255],
        };
        let r = resize(&img, 4, 1);
        let g: Vec<u8> = r.pixels.chunks(4).map(|p| p[0]).collect();
        assert_eq!(g, vec![0, 64, 191, 255]);
        assert_eq!(resize(&img, 2, 1).pixels, img.pixels);
        let r = resize(&img, 3, 5);
        assert_eq!((r.width, r.height, r.pixels.len()), (3, 5, 60));
    }
}
