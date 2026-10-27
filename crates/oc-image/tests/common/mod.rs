//! Shared helpers for fixture-based tests: fixtures are generated at test
//! time with ffmpeg / ImageMagick (development tools only); tests skip with a
//! note when the tools are missing.
#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

pub fn tool_available(name: &str) -> bool {
    Command::new(name)
        .arg("-version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

pub fn have_ffmpeg() -> bool {
    let ok = tool_available("ffmpeg") && tool_available("ffprobe");
    if !ok {
        eprintln!("note: ffmpeg/ffprobe not found, skipping fixture test");
    }
    ok
}

pub fn have_convert() -> bool {
    tool_available("convert") && tool_available("identify")
}

/// Per-test fixture directory under the system temp dir, removed on drop.
pub struct FixtureDir(PathBuf);

impl std::ops::Deref for FixtureDir {
    type Target = Path;
    fn deref(&self) -> &Path {
        &self.0
    }
}

impl Drop for FixtureDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
        if let Some(parent) = self.0.parent() {
            let _ = std::fs::remove_dir(parent); // only succeeds once empty
        }
    }
}

pub fn fixture_dir(test: &str) -> FixtureDir {
    let d = std::env::temp_dir().join(format!("oc-image-tests-{}", std::process::id())).join(test);
    std::fs::create_dir_all(&d).expect("create fixture dir");
    FixtureDir(d)
}

/// Runs ffmpeg with the given arguments (quiet, overwrite). Returns success.
pub fn ffmpeg(args: &[&str]) -> bool {
    let mut cmd = Command::new("ffmpeg");
    cmd.args(["-v", "error", "-y"]).args(args).stdout(Stdio::null());
    match cmd.output() {
        Ok(o) if o.status.success() => true,
        Ok(o) => {
            eprintln!("ffmpeg {:?} failed: {}", args, String::from_utf8_lossy(&o.stderr));
            false
        }
        Err(_) => false,
    }
}

pub fn convert(args: &[&str]) -> bool {
    match Command::new("convert").args(args).output() {
        Ok(o) if o.status.success() => true,
        Ok(o) => {
            eprintln!("convert {:?} failed: {}", args, String::from_utf8_lossy(&o.stderr));
            false
        }
        Err(_) => false,
    }
}

/// Decodes the first frame of `path` to RGBA with ffmpeg.
pub fn ffmpeg_rgba(path: &Path) -> Option<(u32, u32, Vec<u8>)> {
    let p = path.to_str()?;
    let probe = Command::new("ffprobe")
        .args(["-v", "error", "-select_streams", "v:0", "-show_entries", "stream=width,height", "-of", "csv=p=0", p])
        .output()
        .ok()?;
    let s = String::from_utf8_lossy(&probe.stdout);
    let mut it = s.trim().split(',');
    let w: u32 = it.next()?.trim().parse().ok()?;
    let h: u32 = it.next()?.trim().parse().ok()?;
    let out = Command::new("ffmpeg")
        .args(["-v", "error", "-i", p, "-frames:v", "1", "-f", "rawvideo", "-pix_fmt", "rgba", "-"])
        .output()
        .ok()?;
    if !out.status.success() || out.stdout.len() != (w * h * 4) as usize {
        return None;
    }
    Some((w, h, out.stdout))
}

/// Decodes `path` to RGBA with ImageMagick (libjpeg-turbo / libpng /
/// libwebp); `extra` options are inserted before the output.
pub fn convert_rgba(path: &Path, extra: &[&str]) -> Option<(u32, u32, Vec<u8>)> {
    let p = format!("{}[0]", path.to_str()?);
    let mut args: Vec<&str> = vec![p.as_str()];
    args.extend_from_slice(extra);
    let info = Command::new("convert").args(&args).args(["-format", "%w %h", "info:"]).output().ok()?;
    let s = String::from_utf8_lossy(&info.stdout);
    let mut it = s.split_whitespace();
    let w: u32 = it.next()?.parse().ok()?;
    let h: u32 = it.next()?.parse().ok()?;
    let out = Command::new("convert").args(&args).args(["-depth", "8", "rgba:-"]).output().ok()?;
    if !out.status.success() || out.stdout.len() != (w * h * 4) as usize {
        return None;
    }
    Some((w, h, out.stdout))
}

pub struct Diff {
    pub mean: f64,
    pub max: u8,
    pub psnr: f64,
}

pub fn diff(a: &[u8], b: &[u8]) -> Diff {
    assert_eq!(a.len(), b.len());
    let mut sum = 0u64;
    let mut sq = 0u64;
    let mut max = 0u8;
    for (&x, &y) in a.iter().zip(b) {
        let d = x.abs_diff(y);
        sum += d as u64;
        sq += (d as u64) * (d as u64);
        max = max.max(d);
    }
    let n = a.len().max(1) as f64;
    let mse = sq as f64 / n;
    let psnr = if mse == 0.0 { f64::INFINITY } else { 10.0 * (255.0 * 255.0 / mse).log10() };
    Diff { mean: sum as f64 / n, max, psnr }
}

/// Deterministic xorshift PRNG.
pub struct Rng(pub u64);

impl Rng {
    pub fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    pub fn below(&mut self, n: usize) -> usize {
        (self.next() % n.max(1) as u64) as usize
    }
}
