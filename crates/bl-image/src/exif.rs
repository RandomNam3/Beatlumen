//! Minimal EXIF parsing (orientation tag) and orientation transforms.

use crate::Image;

/// Returns the orientation (1..=8) from the payload of an APP1 segment
/// starting with "Exif\0\0", if present.
pub fn orientation(app1: &[u8]) -> Option<u16> {
    let tiff = app1.strip_prefix(b"Exif\0\0")?;
    let le = match tiff.get(..4)? {
        [b'I', b'I', 42, 0] => true,
        [b'M', b'M', 0, 42] => false,
        _ => return None,
    };
    let u16_at = |p: usize| -> Option<u16> {
        let b = tiff.get(p..p + 2)?;
        Some(if le {
            u16::from_le_bytes([b[0], b[1]])
        } else {
            u16::from_be_bytes([b[0], b[1]])
        })
    };
    let u32_at = |p: usize| -> Option<u32> {
        let b = tiff.get(p..p + 4)?;
        let a = [b[0], b[1], b[2], b[3]];
        Some(if le {
            u32::from_le_bytes(a)
        } else {
            u32::from_be_bytes(a)
        })
    };
    let ifd = u32_at(4)? as usize;
    let n = u16_at(ifd)? as usize;
    for i in 0..n {
        let e = ifd + 2 + i * 12;
        if u16_at(e)? == 0x0112 {
            let ty = u16_at(e + 2)?;
            let v = match ty {
                3 => u16_at(e + 8)?,
                4 => u32_at(e + 8)? as u16,
                _ => return None,
            };
            return (1..=8).contains(&v).then_some(v);
        }
    }
    None
}

/// Applies an EXIF orientation so that the image is displayed upright.
pub fn apply(img: Image, orientation: u16) -> Image {
    if !(2..=8).contains(&orientation) {
        return img;
    }
    let (w, h) = (img.width as usize, img.height as usize);
    let swap = orientation >= 5;
    let (ow, oh) = if swap { (h, w) } else { (w, h) };
    let src = &img.pixels;
    let mut out = vec![0u8; ow * oh * 4];
    for y in 0..oh {
        for x in 0..ow {
            let (sx, sy) = match orientation {
                2 => (w - 1 - x, y),
                3 => (w - 1 - x, h - 1 - y),
                4 => (x, h - 1 - y),
                5 => (y, x),
                6 => (y, h - 1 - x),
                7 => (w - 1 - y, h - 1 - x),
                _ => (w - 1 - y, x),
            };
            let s = (sy * w + sx) * 4;
            let d = (y * ow + x) * 4;
            out[d..d + 4].copy_from_slice(&src[s..s + 4]);
        }
    }
    Image {
        width: ow as u32,
        height: oh as u32,
        pixels: out,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rotations() {
        // 2x1 image: A B
        let img = Image {
            width: 2,
            height: 1,
            pixels: vec![1, 1, 1, 1, 2, 2, 2, 2],
        };
        let r6 = apply(img.clone(), 6); // 90 CW -> column A over B
        assert_eq!((r6.width, r6.height), (1, 2));
        assert_eq!(r6.pixels, vec![1, 1, 1, 1, 2, 2, 2, 2]);
        let r8 = apply(img.clone(), 8); // 90 CCW -> B over A
        assert_eq!(r8.pixels, vec![2, 2, 2, 2, 1, 1, 1, 1]);
        let r2 = apply(img, 2);
        assert_eq!(r2.pixels, vec![2, 2, 2, 2, 1, 1, 1, 1]);
    }

    #[test]
    fn parses_orientation() {
        let mut d = b"Exif\0\0MM\0\x2a\0\0\0\x08".to_vec();
        d.extend_from_slice(&[0, 1, 0x01, 0x12, 0, 3, 0, 0, 0, 1, 0, 6, 0, 0, 0, 0]);
        assert_eq!(orientation(&d), Some(6));
    }
}
