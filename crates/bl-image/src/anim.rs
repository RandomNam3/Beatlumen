//! Animated images (GIF, APNG, animated WebP).
//!
//! Frames are decoded one at a time. Up to [`FULL_CANVAS_PIXELS`] (a bit
//! more than 4K) they are composed (blending, disposal) on a canvas of the
//! source size, which is exact, and each finished frame is scaled to the
//! target size. Larger animations are scaled while their frames are
//! decoded and composed on a premultiplied canvas of the target size: memory
//! then depends only on the target size, at the price of small errors where
//! partly transparent frames meet detail of earlier frames.
//!
//! An animation plays at most [`MAX_FRAMES`] frames with at most
//! [`MAX_ANIMATION_PIXELS`] pixels in all; a longer one loops after the
//! frames that fit, so a crafted file cannot keep a core busy decoding
//! huge frames without end.

use crate::scale::{self, Blend, Canvas, Dest, RowSink, Scaler};
use crate::{gif, png, webp, Error, Fit, Format, Image};

/// One displayed frame.
#[derive(Debug, Clone)]
pub struct Frame {
    pub image: Image,
    /// How long the frame stays on screen.
    pub delay_ms: u32,
}

enum Kind {
    Gif(gif::Parsed),
    Png(Box<png::Parsed>),
    WebP(webp::Parsed),
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Dispose {
    Keep,
    Background,
    Previous,
}

/// What the file says about one frame.
struct FrameInfo {
    rect: (usize, usize, usize, usize),
    delay_ms: u32,
    dispose: Dispose,
    blend: Blend,
}

/// Largest animation composed at its own size (64 MiB of RGBA).
pub const FULL_CANVAS_PIXELS: u64 = 1 << 24;

/// Most frames read from an animated file (the decoders stop listing frames
/// here, so a file of tiny frames cannot fill memory with frame headers).
pub const MAX_FRAMES: usize = 10_000;

/// Most pixels of all frames of one loop together (their rectangles, at
/// source size): 500 frames of 4K, 2000 of 1080p or 32 of 16K.
pub const MAX_ANIMATION_PIXELS: u64 = 1 << 32;

/// How many of these frame rectangles fit in [`MAX_FRAMES`] and
/// [`MAX_ANIMATION_PIXELS`].
fn frames_within(rects: impl Iterator<Item = Rect>) -> usize {
    let mut total = 0u64;
    let mut n = 0;
    for (_, _, w, h) in rects.take(MAX_FRAMES) {
        total = total.saturating_add((w as u64).saturating_mul(h as u64));
        if total > MAX_ANIMATION_PIXELS {
            break;
        }
        n += 1;
    }
    n
}

/// A frame rectangle moved and cut to lie inside a canvas of `cw` x `ch`
/// (one that lies outside keeps no area).
fn clip(rect: Rect, cw: usize, ch: usize) -> Rect {
    let (left, top, w, h) = rect;
    let (left, top) = (left.min(cw), top.min(ch));
    (left, top, w.min(cw - left), h.min(ch - top))
}

type Rect = (usize, usize, usize, usize);

enum Compose {
    /// Source-size straight RGBA canvas; frames are scaled when finished.
    Full {
        canvas: Image,
        /// Rectangle and pixels under a frame disposed to "previous".
        saved: Option<(Rect, Vec<u8>)>,
        target: (u32, u32),
    },
    /// Target-size premultiplied canvas; frames are scaled while decoded.
    Scaled {
        canvas: Canvas,
        saved: Option<Canvas>,
        /// Canvas pixels under the last frame before it was drawn (disposal
        /// to background).
        under: Option<Vec<u16>>,
    },
}

/// Draws frame rows onto a source-size canvas.
struct Blit<'a> {
    canvas: &'a mut Image,
    rect: Rect,
    blend: Blend,
}

impl RowSink for Blit<'_> {
    fn row(&mut self, y: usize, rgba: &[u8]) {
        let (left, top, w, h) = self.rect;
        let cw = self.canvas.width as usize;
        if y >= h || top + y >= self.canvas.height as usize {
            return;
        }
        let w = w.min(cw.saturating_sub(left)).min(rgba.len() / 4);
        let o = ((top + y) * cw + left) * 4;
        let dst = &mut self.canvas.pixels[o..o + w * 4];
        if self.blend == Blend::Replace {
            dst.copy_from_slice(&rgba[..w * 4]);
            return;
        }
        for (d, s) in dst.chunks_exact_mut(4).zip(rgba.chunks_exact(4)) {
            match s[3] {
                255 => d.copy_from_slice(s),
                0 => {}
                sa => {
                    // Straight-alpha "over".
                    let (sa, da) = (sa as u32, d[3] as u32);
                    let da = da * (255 - sa) / 255;
                    let a = sa + da;
                    for c in 0..3 {
                        d[c] = ((s[c] as u32 * sa + d[c] as u32 * da + a / 2) / a) as u8;
                    }
                    d[3] = a as u8;
                }
            }
        }
    }
}

fn clear_rect(canvas: &mut Image, rect: Rect) {
    let (left, top, w, h) = rect;
    let cw = canvas.width as usize;
    for y in top..(top + h).min(canvas.height as usize) {
        let o = (y * cw + left) * 4;
        let n = w.min(cw.saturating_sub(left)) * 4;
        canvas.pixels[o..o + n].fill(0);
    }
}

fn copy_rect(canvas: &Image, rect: Rect) -> Vec<u8> {
    let (left, top, w, h) = rect;
    let cw = canvas.width as usize;
    let n = w.min(cw.saturating_sub(left)) * 4;
    let mut v = Vec::with_capacity(n * h);
    for y in top..(top + h).min(canvas.height as usize) {
        let o = (y * cw + left) * 4;
        v.extend_from_slice(&canvas.pixels[o..o + n]);
    }
    v
}

fn paste_rect(canvas: &mut Image, rect: Rect, px: &[u8]) {
    let (left, top, w, h) = rect;
    let cw = canvas.width as usize;
    let n = w.min(cw.saturating_sub(left)) * 4;
    for (j, y) in (top..(top + h).min(canvas.height as usize)).enumerate() {
        let o = (y * cw + left) * 4;
        if let Some(src) = px.get(j * n..(j + 1) * n) {
            canvas.pixels[o..o + n].copy_from_slice(src);
        }
    }
}

/// A decoder that yields the frames of an animation in a loop.
pub struct Animation {
    data: Vec<u8>,
    kind: Kind,
    canvas_size: (usize, usize),
    compose: Compose,
    next: usize,
    /// Disposal of the frame shown last.
    pending: Option<(Dispose, (usize, usize, usize, usize))>,
}

/// Browsers show frames with (almost) no delay for 100 ms.
fn delay(ms: u32) -> u32 {
    if ms <= 10 {
        100
    } else {
        ms
    }
}

impl Animation {
    /// [`MAX_FRAMES`]: most frames played.
    pub const MAX_FRAMES: usize = MAX_FRAMES;
    /// [`MAX_ANIMATION_PIXELS`]: most pixels of all frames of a loop.
    pub const MAX_PIXELS: u64 = MAX_ANIMATION_PIXELS;

    /// Opens an animation scaled to `fit`; it keeps the file's bytes.
    /// Returns `None` for still images (and animations with one frame).
    pub fn new(mut data: Vec<u8>, fit: &Fit) -> Result<Option<Animation>, Error> {
        let kind = match crate::probe_format(&data) {
            Some(Format::Gif) => Kind::Gif(gif::parse(&data)?),
            Some(Format::Png) => {
                let mut p = png::parse(&data)?;
                if p.frames.len() > 1 {
                    p.compact_frames(&mut data);
                }
                Kind::Png(Box::new(p))
            }
            Some(Format::WebP) => Kind::WebP(webp::parse(&data)?),
            _ => return Ok(None),
        };
        let mut kind = kind;
        let size = match &kind {
            Kind::Gif(p) => (p.width, p.height),
            Kind::Png(p) => {
                let (w, h) = p.size();
                (w as usize, h as usize)
            }
            Kind::WebP(p) => (p.width, p.height),
        };
        // Only the frames that fit the limits play.
        let fits = |r: Rect| clip(r, size.0, size.1);
        let count = match &mut kind {
            Kind::Gif(p) => {
                let n = frames_within(p.frames.iter().map(|f| fits(f.rect)));
                p.frames.truncate(n);
                n
            }
            Kind::Png(p) => {
                let n = frames_within(p.frames.iter().map(|f| fits(f.rect)));
                p.frames.truncate(n);
                n
            }
            Kind::WebP(p) => {
                let n = frames_within(p.frames.iter().map(|f| fits(f.rect)));
                p.frames.truncate(n);
                n
            }
        };
        if count < 2 {
            return Ok(None);
        }
        let (tw, th) = fit.target(size.0 as u32, size.1 as u32);
        let compose = if size.0 as u64 * size.1 as u64 <= FULL_CANVAS_PIXELS {
            Compose::Full {
                canvas: Image::new(size.0 as u32, size.1 as u32),
                saved: None,
                target: (tw, th),
            }
        } else {
            Compose::Scaled {
                canvas: Canvas::new(tw as usize, th as usize),
                saved: None,
                under: None,
            }
        };
        Ok(Some(Animation {
            data,
            kind,
            canvas_size: size,
            compose,
            next: 0,
            pending: None,
        }))
    }

    /// Size of the frames.
    pub fn size(&self) -> (u32, u32) {
        match &self.compose {
            Compose::Full { target, .. } => *target,
            Compose::Scaled { canvas, .. } => (canvas.width as u32, canvas.height as u32),
        }
    }

    pub fn frame_count(&self) -> usize {
        match &self.kind {
            Kind::Gif(p) => p.frames.len(),
            Kind::Png(p) => p.frames.len(),
            Kind::WebP(p) => p.frames.len(),
        }
    }

    /// Index of the frame the next call to [`Animation::next_frame`] returns.
    pub fn position(&self) -> usize {
        self.next
    }

    /// What the file says about frame `i`, its rectangle cut to the canvas.
    fn info(&self, i: usize) -> FrameInfo {
        let mut info = self.stored_info(i);
        info.rect = clip(info.rect, self.canvas_size.0, self.canvas_size.1);
        info
    }

    fn stored_info(&self, i: usize) -> FrameInfo {
        match &self.kind {
            Kind::Gif(p) => {
                let f = &p.frames[i];
                FrameInfo {
                    rect: f.rect,
                    delay_ms: f.delay_ms,
                    dispose: match f.dispose {
                        2 => Dispose::Background,
                        3 => Dispose::Previous,
                        _ => Dispose::Keep,
                    },
                    blend: Blend::Over,
                }
            }
            Kind::Png(p) => {
                let f = &p.frames[i];
                FrameInfo {
                    rect: f.rect,
                    delay_ms: f.delay_ms,
                    dispose: match f.dispose {
                        1 => Dispose::Background,
                        // "Previous" on the first frame means background.
                        2 if i > 0 => Dispose::Previous,
                        2 => Dispose::Background,
                        _ => Dispose::Keep,
                    },
                    blend: if f.blend == 1 {
                        Blend::Over
                    } else {
                        Blend::Replace
                    },
                }
            }
            Kind::WebP(p) => {
                let f = &p.frames[i];
                FrameInfo {
                    rect: f.rect,
                    delay_ms: f.delay_ms,
                    dispose: if f.dispose_bg {
                        Dispose::Background
                    } else {
                        Dispose::Keep
                    },
                    blend: if f.blend { Blend::Over } else { Blend::Replace },
                }
            }
        }
    }

    /// Composes and returns the next frame (after the last frame the
    /// animation starts over). A damaged frame leaves the canvas as far as
    /// it could be decoded.
    pub fn next_frame(&mut self) -> Frame {
        let (cw, ch) = self.canvas_size;
        let i = self.next;
        let info = self.info(i);
        let restart = i == 0;
        let pending = if restart { None } else { self.pending.take() };
        let data = &self.data;
        let kind = &self.kind;
        // A frame outside the canvas draws nothing.
        let empty = info.rect.2 == 0 || info.rect.3 == 0;
        let decode = |sink: &mut dyn RowSink| {
            if empty {
                return;
            }
            let _ = match kind {
                Kind::Gif(p) => p.frame_rows(data, i, sink),
                Kind::Png(p) => p.frame_rows(data, i, sink),
                Kind::WebP(p) => p.frame_rows(data, i, sink),
            };
        };
        let image = match &mut self.compose {
            Compose::Full {
                canvas,
                saved,
                target,
            } => {
                if restart {
                    canvas.pixels.fill(0);
                    *saved = None;
                }
                match pending {
                    Some((Dispose::Background, rect)) => clear_rect(canvas, rect),
                    Some((Dispose::Previous, _)) => {
                        if let Some((rect, px)) = saved.take() {
                            paste_rect(canvas, rect, &px);
                        }
                    }
                    _ => {}
                }
                if info.dispose == Dispose::Previous {
                    *saved = Some((info.rect, copy_rect(canvas, info.rect)));
                }
                decode(&mut Blit {
                    canvas,
                    rect: info.rect,
                    blend: info.blend,
                });
                scale::scale_image(canvas, target.0, target.1)
            }
            Compose::Scaled {
                canvas,
                saved,
                under,
            } => {
                if restart {
                    canvas.px.fill(0);
                    *saved = None;
                }
                match pending {
                    Some((Dispose::Background, rect)) => {
                        let u = under.take();
                        if rect.2 > 0 && rect.3 > 0 {
                            canvas.clear_rect(cw, ch, rect, u.as_deref());
                        }
                    }
                    Some((Dispose::Previous, _)) => {
                        if let Some(s) = saved.take() {
                            *canvas = s;
                        }
                    }
                    _ => {}
                }
                match info.dispose {
                    Dispose::Previous => *saved = Some(canvas.clone()),
                    Dispose::Background if !empty => {
                        *under = Some(canvas.save_rect(cw, ch, info.rect))
                    }
                    Dispose::Background | Dispose::Keep => {}
                }
                if !empty {
                    let mut s =
                        Scaler::frame(Dest::Canvas(canvas, info.blend), (cw, ch), info.rect, None);
                    decode(&mut s);
                    s.finish();
                }
                canvas.to_image()
            }
        };
        self.pending = Some((info.dispose, info.rect));
        self.next = (i + 1) % self.frame_count();
        Frame {
            image,
            delay_ms: delay(info.delay_ms),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A two-frame GIF built by hand: 4x2 canvas, frame 1 fills it red,
    /// frame 2 paints the right half blue and is disposed to background.
    fn two_frame_gif() -> Vec<u8> {
        let mut g = b"GIF89a".to_vec();
        g.extend_from_slice(&[4, 0, 2, 0, 0x81, 0, 0]); // 4 colors global table
        g.extend_from_slice(&[255, 0, 0, 0, 0, 255, 0, 0, 0, 0, 0, 0]);
        // NETSCAPE loop extension.
        g.extend_from_slice(b"\x21\xff\x0bNETSCAPE2.0\x03\x01\x00\x00\x00");
        let frame = |g: &mut Vec<u8>, left: u8, w: u8, color: u8, dispose: u8, delay: u8| {
            g.extend_from_slice(&[0x21, 0xf9, 4, dispose << 2, delay, 0, 0, 0]);
            g.extend_from_slice(&[0x2c, left, 0, 0, 0, w, 0, 2, 0, 0]);
            // LZW with min code 2: clear (4), then one index per pixel
            // (3-bit codes stay valid for up to 8 pixels), EOI (5).
            let n = w as usize * 2;
            let mut codes = vec![4u32];
            codes.extend(vec![color as u32; n]);
            codes.push(5);
            // Codes widen as the table fills; keep each string short by
            // clearing after every code.
            let mut bits = 0u64;
            let mut nb = 0;
            let mut bytes = Vec::new();
            let mut size = 3;
            let mut next = 6u32;
            let mut first = true;
            for c in codes {
                bits |= (c as u64) << nb;
                nb += size;
                while nb >= 8 {
                    bytes.push(bits as u8);
                    bits >>= 8;
                    nb -= 8;
                }
                if c < 4 {
                    if !first {
                        next += 1;
                        if next == 1 << size && size < 12 {
                            size += 1;
                        }
                    }
                    first = false;
                }
            }
            if nb > 0 {
                bytes.push(bits as u8);
            }
            g.push(2);
            g.push(bytes.len() as u8);
            g.extend_from_slice(&bytes);
            g.push(0);
        };
        frame(&mut g, 0, 4, 0, 1, 5);
        frame(&mut g, 2, 2, 1, 2, 7);
        g.push(0x3b);
        g
    }

    /// A GIF with empty frames (clear code, end code) at these rectangles
    /// (left, top, width, height, disposal).
    fn empty_frames_gif(canvas: (u16, u16), frames: &[(u16, u16, u16, u16, u8)]) -> Vec<u8> {
        let mut g = b"GIF89a".to_vec();
        g.extend_from_slice(&canvas.0.to_le_bytes());
        g.extend_from_slice(&canvas.1.to_le_bytes());
        g.extend_from_slice(&[0x81, 0, 0]);
        g.extend_from_slice(&[255, 0, 0, 0, 0, 255, 0, 0, 0, 0, 0, 0]);
        for &(l, t, w, h, dispose) in frames {
            g.extend_from_slice(&[0x21, 0xf9, 4, dispose << 2, 5, 0, 0, 0]);
            g.push(0x2c);
            for v in [l, t, w, h] {
                g.extend_from_slice(&v.to_le_bytes());
            }
            // No local table; min code 2; codes 4 (clear) and 5 (end).
            g.extend_from_slice(&[0, 2, 1, 0x2c, 0]);
        }
        g.push(0x3b);
        g
    }

    #[test]
    fn frames_outside_the_canvas_draw_nothing() {
        for dispose in [1, 2, 3] {
            let gif = empty_frames_gif(
                (4, 2),
                &[
                    (0, 0, 4, 2, 0),
                    (60_000, 1, 9, 9, dispose),
                    (1, 60_000, 2, 2, dispose),
                ],
            );
            for fit in [Fit::full(), Fit::within(2)] {
                let mut a = Animation::new(gif.clone(), &fit).unwrap().unwrap();
                for _ in 0..7 {
                    let f = a.next_frame();
                    let (w, h) = a.size();
                    assert_eq!(f.image.pixels.len(), w as usize * h as usize * 4);
                }
            }
        }
    }

    #[test]
    fn frame_count_and_pixels_are_limited() {
        let frames = vec![(0, 0, 2, 2, 0); MAX_FRAMES + 5];
        let a = Animation::new(empty_frames_gif((2, 2), &frames), &Fit::full())
            .unwrap()
            .unwrap();
        assert_eq!(a.frame_count(), MAX_FRAMES);
        // 16384 x 16384 frames: 16 of them make 2^32 pixels.
        let frames = vec![(0, 0, 16384, 16384, 0); 40];
        let a = Animation::new(empty_frames_gif((16384, 16384), &frames), &Fit::within(16))
            .unwrap()
            .unwrap();
        assert_eq!(a.frame_count(), (MAX_ANIMATION_PIXELS >> 28) as usize);
        // Exactly the limit fits, one pixel more does not.
        let big = (0, 0, 1 << 16, 1 << 16);
        assert_eq!(frames_within([big, (0, 0, 1, 1)].into_iter()), 1);
        assert_eq!(
            frames_within([(0, 0, (1 << 16) + 1, 1 << 16)].into_iter()),
            0
        );
    }

    #[test]
    fn gif_frames_and_disposal() {
        let mut a = Animation::new(two_frame_gif(), &Fit::full())
            .unwrap()
            .unwrap();
        assert_eq!(a.frame_count(), 2);
        let f1 = a.next_frame();
        assert_eq!(f1.delay_ms, 50);
        assert!(f1
            .image
            .pixels
            .chunks_exact(4)
            .all(|p| p == [255, 0, 0, 255]));
        let f2 = a.next_frame();
        assert_eq!(f2.delay_ms, 70);
        let px = |img: &Image, x: usize| img.pixels[x * 4..x * 4 + 4].to_vec();
        assert_eq!(px(&f2.image, 1), [255, 0, 0, 255]);
        assert_eq!(px(&f2.image, 3), [0, 0, 255, 255]);
        // Looping starts again from a cleared canvas.
        let f3 = a.next_frame();
        assert_eq!(f3.image.pixels, f1.image.pixels);
        // Scaled: 2x1 target, right pixel half red half blue.
        let mut a = Animation::new(two_frame_gif(), &Fit::within(2))
            .unwrap()
            .unwrap();
        assert_eq!(a.size(), (2, 1));
        a.next_frame();
        let f2 = a.next_frame();
        assert_eq!(&f2.image.pixels[4..8], &[0, 0, 255, 255]);
        assert_eq!(&f2.image.pixels[0..4], &[255, 0, 0, 255]);
    }
}
