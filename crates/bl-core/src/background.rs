//! Background images: how large they are decoded and how animations play.
//!
//! Still images are decoded to cover the screen with some room for the
//! camera's zoom, limited by the GPU's texture size; very large files (up
//! to 16K) are scaled while they are decoded and never exist at full size.
//! Animated GIF, APNG and WebP files are decoded frame by frame on a worker
//! thread at screen size; frames are kept when the whole animation fits a
//! memory budget, so that later loops cost no decoding at all.

use bl_image::{Animation, Fit, Image};
use std::sync::mpsc::{sync_channel, Receiver};
use std::sync::Arc;

/// Screen and GPU limits a background is decoded for.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct BgTarget {
    /// Framebuffer size in physical pixels.
    pub screen: (u32, u32),
    /// GL_MAX_TEXTURE_SIZE.
    pub max_texture: u32,
    pub mobile: bool,
}

/// Room for the camera, which shows only part of the image when it zooms
/// in.
const ZOOM_ROOM: f64 = 1.25;

impl BgTarget {
    /// Still images: cover the screen (plus zoom room).
    pub fn still(&self) -> Fit {
        let (w, h) = self.screen;
        let max_pixels: u64 = if self.mobile { 16 << 20 } else { 32 << 20 };
        Fit::cover(
            (w as f64 * ZOOM_ROOM).round() as u32,
            (h as f64 * ZOOM_ROOM).round() as u32,
            self.max_texture.clamp(1024, 8192),
            max_pixels,
        )
    }

    /// Animations: cover the screen, at most about 1080p on phones and
    /// tablets and 1440p elsewhere, so that every frame stays cheap to
    /// decode and upload.
    pub fn animated(&self) -> Fit {
        let (w, h) = self.screen;
        let max_pixels: u64 = if self.mobile {
            1920 * 1080
        } else {
            2560 * 1440
        };
        Fit::cover(w, h, self.max_texture.clamp(1024, 4096), max_pixels)
    }

    /// Memory for keeping all frames of an animation.
    pub fn frame_budget(&self) -> usize {
        if self.mobile {
            128 << 20
        } else {
            384 << 20
        }
    }

    /// The longest side a server should scale a still image to.
    pub fn server_size(&self) -> u32 {
        let (w, h) = self.screen;
        ((w.max(h) as f64 * ZOOM_ROOM).round() as u32)
            .clamp(1024, self.max_texture.clamp(1024, 8192))
    }
}

/// A decoded background.
pub(crate) enum BgImage {
    Still(Image),
    Animated {
        first: Arc<Image>,
        player: AnimPlayer,
    },
}

impl BgImage {
    pub fn size(&self) -> (u32, u32) {
        match self {
            BgImage::Still(i) => (i.width, i.height),
            BgImage::Animated { first, .. } => (first.width, first.height),
        }
    }
}

/// Decodes a background image (on a worker thread where there is one).
/// Returns the image and the size of the source.
pub(crate) fn decode(
    data: Vec<u8>,
    target: &BgTarget,
    threads: bool,
) -> Result<(BgImage, (u32, u32)), String> {
    let info = bl_image::info(&data).map_err(|e| e.to_string())?;
    let size = (info.width, info.height);
    if info.is_animated() {
        if let Some(mut anim) =
            Animation::new(data, &target.animated()).map_err(|e| e.to_string())?
        {
            let first = anim.next_frame();
            let first_img = Arc::new(first.image);
            let player = AnimPlayer::new(
                anim,
                first_img.clone(),
                first.delay_ms,
                target.frame_budget(),
                threads,
            );
            return Ok((
                BgImage::Animated {
                    first: first_img,
                    player,
                },
                size,
            ));
        }
        return Err("animation without frames".into());
    }
    let img = bl_image::decode_fit_owned(data, &target.still()).map_err(|e| e.to_string())?;
    Ok((BgImage::Still(img), size))
}

/// Recently made thumbnails: (path, modification time, length) and image.
type ThumbCache = Vec<((String, u128, u64), Arc<Image>)>;
static THUMBS: std::sync::Mutex<ThumbCache> = std::sync::Mutex::new(Vec::new());
const THUMB_CACHE: usize = 64;

/// A small picture of a local image file for lists (covering `w` x `h`),
/// decoded with the image decoders of the core: any supported format and
/// size (a 16K picture is scaled while it is read), first frame of
/// animations. From any thread; the last 64 are kept.
pub fn thumbnail(path: &str, w: u32, h: u32) -> Option<Arc<Image>> {
    let meta = std::fs::metadata(path).ok()?;
    let mtime = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_nanos());
    let key = (format!("{path}\u{0}{w}x{h}"), mtime, meta.len());
    if let Ok(cache) = THUMBS.lock() {
        if let Some((_, img)) = cache.iter().find(|(k, _)| *k == key) {
            return Some(img.clone());
        }
    }
    let data = std::fs::read(path).ok()?;
    let fit = Fit::cover(w.clamp(1, 2048), h.clamp(1, 2048), 2048, 1 << 22);
    let img = std::panic::catch_unwind(move || bl_image::decode_fit_owned(data, &fit).ok())
        .ok()
        .flatten()?;
    let img = Arc::new(img);
    if let Ok(mut cache) = THUMBS.lock() {
        cache.retain(|(k, _)| k.0 != key.0);
        if cache.len() >= THUMB_CACHE {
            cache.remove(0);
        }
        cache.push((key, img.clone()));
    }
    Some(img)
}

/// A frame to show and for how long.
type Shown = (Arc<Image>, u32);

enum Source {
    /// Frames come from a worker thread (which stops when the player is
    /// dropped).
    Worker(Receiver<Shown>),
    /// Frames are decoded here when they are due (no threads, e.g. the web).
    Inline(Box<Inline>),
}

struct Inline {
    anim: Animation,
    cache: FrameCache,
}

/// Keeps the frames of one loop when they fit the budget.
struct FrameCache {
    frames: Vec<Shown>,
    budget: usize,
    /// All frames are cached; playback reads from here.
    complete: bool,
    pos: usize,
}

impl FrameCache {
    fn new(frame_bytes: usize, count: usize, budget: usize) -> FrameCache {
        let keep = frame_bytes.saturating_mul(count) <= budget;
        FrameCache {
            frames: Vec::new(),
            budget: if keep { budget } else { 0 },
            complete: false,
            pos: 0,
        }
    }

    /// The next frame: from the cache once complete, else decoded.
    fn next(&mut self, anim: &mut Animation, first: &Arc<Image>, first_delay: u32) -> Shown {
        if self.complete {
            self.pos = (self.pos + 1) % self.frames.len();
            return self.frames[self.pos].clone();
        }
        if self.budget > 0 && self.frames.is_empty() {
            self.frames.push((first.clone(), first_delay));
        }
        let f = anim.next_frame();
        let shown: Shown = (Arc::new(f.image), f.delay_ms);
        if self.budget > 0 {
            if anim.position() == 1 {
                // Wrapped around: frame 0 again, every frame is cached.
                self.complete = true;
                self.pos = 0;
                return self.frames[0].clone();
            }
            self.frames.push(shown.clone());
        }
        shown
    }
}

/// Plays an animation: hands out frames when their time has come.
pub(crate) struct AnimPlayer {
    source: Source,
    first: Arc<Image>,
    first_delay: u32,
    /// Time the current frame has been on screen, and its delay.
    shown_for: f64,
    delay: f64,
    /// A frame that is due but was not ready yet.
    late: bool,
}

impl AnimPlayer {
    fn new(
        anim: Animation,
        first: Arc<Image>,
        first_delay: u32,
        budget: usize,
        threads: bool,
    ) -> AnimPlayer {
        let (w, h) = anim.size();
        let frame_bytes = w as usize * h as usize * 4;
        let mut cache = FrameCache::new(frame_bytes, anim.frame_count(), budget);
        let source = if threads {
            // Two frames ahead: the thread blocks until a frame was taken,
            // so decoding runs only as fast as frames are shown and stops
            // while the app is hidden; it ends with the player.
            let (tx, rx) = sync_channel::<Shown>(2);
            let first_w = first.clone();
            let mut anim = anim;
            std::thread::Builder::new()
                .name("bg-animation".into())
                .spawn(move || loop {
                    let f = cache.next(&mut anim, &first_w, first_delay);
                    if tx.send(f).is_err() {
                        return;
                    }
                })
                .ok();
            Source::Worker(rx)
        } else {
            Source::Inline(Box::new(Inline { anim, cache }))
        };
        AnimPlayer {
            source,
            first,
            first_delay,
            shown_for: 0.0,
            delay: first_delay.max(10) as f64 / 1000.0,
            late: false,
        }
    }

    /// Advances by `dt` seconds; returns the next frame when it is due.
    pub fn tick(&mut self, dt: f64) -> Option<Arc<Image>> {
        self.shown_for += dt;
        if self.shown_for < self.delay && !self.late {
            return None;
        }
        let next = match &mut self.source {
            Source::Worker(rx) => rx.try_recv().ok(),
            Source::Inline(i) => {
                let Inline { anim, cache } = &mut **i;
                Some(cache.next(anim, &self.first, self.first_delay))
            }
        };
        let Some((img, delay_ms)) = next else {
            // Not decoded yet: show it as soon as it is.
            self.late = true;
            return None;
        };
        self.late = false;
        // Keep the rhythm, but never try to catch up on a backlog.
        self.shown_for = (self.shown_for - self.delay).clamp(0.0, self.delay);
        self.delay = delay_ms.max(10) as f64 / 1000.0;
        Some(img)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn target(mobile: bool) -> BgTarget {
        BgTarget {
            screen: (2960, 1848),
            max_texture: 16384,
            mobile,
        }
    }

    /// An APNG with solid 8x8 frames (red, green, blue) and delays of
    /// 50, 100 and 150 ms (CRCs are not checked by the decoder).
    fn apng() -> Vec<u8> {
        fn chunk(out: &mut Vec<u8>, ty: &[u8], body: &[u8]) {
            out.extend_from_slice(&(body.len() as u32).to_be_bytes());
            out.extend_from_slice(ty);
            out.extend_from_slice(body);
            out.extend_from_slice(&[0; 4]);
        }
        let mut png = b"\x89PNG\r\n\x1a\n".to_vec();
        let mut ihdr = Vec::new();
        ihdr.extend_from_slice(&8u32.to_be_bytes());
        ihdr.extend_from_slice(&8u32.to_be_bytes());
        ihdr.extend_from_slice(&[8, 6, 0, 0, 0]);
        chunk(&mut png, b"IHDR", &ihdr);
        let mut actl = 3u32.to_be_bytes().to_vec();
        actl.extend_from_slice(&0u32.to_be_bytes());
        chunk(&mut png, b"acTL", &actl);
        let mut seq = 0u32;
        for (i, color) in [[255u8, 0, 0, 255], [0, 255, 0, 255], [0, 0, 255, 255]]
            .iter()
            .enumerate()
        {
            let mut fctl = seq.to_be_bytes().to_vec();
            seq += 1;
            for v in [8u32, 8, 0, 0] {
                fctl.extend_from_slice(&v.to_be_bytes());
            }
            fctl.extend_from_slice(&(50 * (i as u16 + 1)).to_be_bytes());
            fctl.extend_from_slice(&1000u16.to_be_bytes());
            fctl.extend_from_slice(&[0, 0]);
            chunk(&mut png, b"fcTL", &fctl);
            let mut raw = Vec::new();
            for _ in 0..8 {
                raw.push(0);
                for _ in 0..8 {
                    raw.extend_from_slice(color);
                }
            }
            let z = bl_image::deflate::zlib_compress(&raw, 6);
            if i == 0 {
                chunk(&mut png, b"IDAT", &z);
            } else {
                let mut fdat = seq.to_be_bytes().to_vec();
                seq += 1;
                fdat.extend_from_slice(&z);
                chunk(&mut png, b"fdAT", &fdat);
            }
        }
        chunk(&mut png, b"IEND", &[]);
        png
    }

    #[test]
    fn animation_plays_in_time_and_loops() {
        for threads in [false, true] {
            let (bg, size) = decode(apng(), &target(false), threads).unwrap();
            assert_eq!(size, (8, 8));
            let BgImage::Animated { first, mut player } = bg else {
                panic!("not animated");
            };
            assert_eq!(&first.pixels[..4], &[255, 0, 0, 255]);
            // Frame 0 stays 50 ms, frame 1 100 ms, frame 2 150 ms.
            let mut shown = Vec::new();
            let mut t = 0.0;
            while t < 0.9 {
                let step = 0.005;
                t += step;
                // Give the worker time to decode ahead.
                if threads {
                    std::thread::sleep(std::time::Duration::from_millis(1));
                }
                if let Some(img) = player.tick(step) {
                    shown.push(((t * 1000.0).round() as u32, img.pixels[1], img.pixels[2]));
                }
            }
            let colors: Vec<(u8, u8)> = shown.iter().map(|s| (s.1, s.2)).collect();
            assert_eq!(
                &colors[..5],
                &[(255, 0), (0, 255), (0, 0), (255, 0), (0, 255)],
                "threads {threads}: {shown:?}"
            );
            // Switch times: 50, 150, 300, 350, 450 ms (within a few ms).
            for (s, want) in shown.iter().zip([50u32, 150, 300, 350, 450]) {
                assert!(s.0.abs_diff(want) <= 10, "threads {threads}: {shown:?}");
            }
        }
    }

    #[test]
    fn thumbnails_are_small_and_kept() {
        let dir = std::env::temp_dir().join(format!("bl-thumb-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("a.png");
        std::fs::write(&p, bl_image::encode_png(&Image::new(400, 300))).unwrap();
        let path = p.to_str().unwrap();
        let a = thumbnail(path, 320, 200).unwrap();
        assert_eq!((a.width, a.height), (320, 240));
        let b = thumbnail(path, 320, 200).unwrap();
        assert!(Arc::ptr_eq(&a, &b), "second call from the cache");
        // Animations: the first frame.
        std::fs::write(&p, apng()).unwrap();
        let c = thumbnail(path, 4, 4).unwrap();
        assert_eq!(&c.pixels[..4], &[255, 0, 0, 255]);
        assert!(thumbnail(dir.join("missing.png").to_str().unwrap(), 8, 8).is_none());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn targets_follow_the_screen() {
        let t = target(true);
        // A 16K image on the tablet: covers 3700 x 2310.
        assert_eq!(t.still().target(15360, 8640), (4107, 2310));
        // Small images are never enlarged.
        assert_eq!(t.still().target(800, 600), (800, 600));
        // Animations: at most about 1080p on mobile.
        let (w, h) = t.animated().target(3840, 2160);
        assert!(w as u64 * h as u64 <= 1920 * 1080 && w > 1800, "{w}x{h}");
        // A GPU with 4096 texels.
        let t = BgTarget {
            screen: (7680, 4320),
            max_texture: 4096,
            mobile: false,
        };
        assert_eq!(t.still().target(15360, 8640), (4096, 2304));
        assert_eq!(t.server_size(), 4096);
    }
}
