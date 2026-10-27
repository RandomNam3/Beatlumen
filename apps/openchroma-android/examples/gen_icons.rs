//! Host-side helper for `build-android.sh`: renders the launcher icons from
//! `oc_app::icon_rgba` into an Android resource directory.
//!
//! Usage: `cargo run -p openchroma-android --example gen_icons -- <res-dir>`
//!
//! Writes legacy `mipmap-*dpi/ic_launcher.png`, adaptive icon foregrounds
//! (`ic_launcher_foreground.png`, 108 dp with the logo inside the safe
//! zone), `mipmap-anydpi-v26/ic_launcher.xml` and the background colour.

use std::path::Path;

/// (density folder, scale factor relative to mdpi)
const DENSITIES: [(&str, u32); 5] = [("mdpi", 2), ("hdpi", 3), ("xhdpi", 4), ("xxhdpi", 6), ("xxxhdpi", 8)];

/// Colour matching the logo's inner disc (see `oc_app::icon_rgba`).
const BACKGROUND: &str = "#1E1E28";

fn write(path: &Path, data: &[u8]) -> Result<(), String> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    }
    std::fs::write(path, data).map_err(|e| format!("{}: {e}", path.display()))
}

fn png(size: u32, pixels: Vec<u8>) -> Vec<u8> {
    oc_image::encode_png(&oc_image::Image { width: size, height: size, pixels })
}

/// The logo (diameter `logo` px) centred on a transparent `canvas`.
fn centered(canvas: u32, logo: u32) -> Vec<u8> {
    let icon = oc_app::icon_rgba(logo);
    let mut out = vec![0u8; (canvas * canvas * 4) as usize];
    let off = (canvas - logo) / 2;
    for y in 0..logo {
        let src = (y * logo * 4) as usize;
        let dst = (((y + off) * canvas + off) * 4) as usize;
        out[dst..dst + (logo * 4) as usize].copy_from_slice(&icon[src..src + (logo * 4) as usize]);
    }
    out
}

fn run() -> Result<(), String> {
    let res = std::env::args().nth(1).ok_or("usage: gen_icons <res-dir>")?;
    let res = Path::new(&res);
    for (name, f) in DENSITIES {
        let dir = res.join(format!("mipmap-{name}"));
        // Legacy icon: 48 dp.
        let legacy = 24 * f;
        write(&dir.join("ic_launcher.png"), &png(legacy, oc_app::icon_rgba(legacy)))?;
        // Adaptive foreground: 108 dp canvas, logo ~70 dp (the 66 dp safe
        // circle keeps the ring visible under every mask shape).
        let canvas = 54 * f;
        let logo = 35 * f;
        write(&dir.join("ic_launcher_foreground.png"), &png(canvas, centered(canvas, logo)))?;
    }
    let adaptive = "<?xml version=\"1.0\" encoding=\"utf-8\"?>\n\
        <adaptive-icon xmlns:android=\"http://schemas.android.com/apk/res/android\">\n\
        \x20   <background android:drawable=\"@color/ic_launcher_background\"/>\n\
        \x20   <foreground android:drawable=\"@mipmap/ic_launcher_foreground\"/>\n\
        </adaptive-icon>\n";
    write(&res.join("mipmap-anydpi-v26").join("ic_launcher.xml"), adaptive.as_bytes())?;
    let colors = format!(
        "<?xml version=\"1.0\" encoding=\"utf-8\"?>\n<resources>\n    <color name=\"ic_launcher_background\">{BACKGROUND}</color>\n</resources>\n"
    );
    write(&res.join("values").join("colors.xml"), colors.as_bytes())?;
    Ok(())
}

fn main() {
    if let Err(e) = run() {
        eprintln!("gen_icons: {e}");
        std::process::exit(1);
    }
}
