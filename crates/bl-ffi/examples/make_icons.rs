//! Writes the application icon PNGs: `make_icons <out dir>`.
//! Produces icon.png (512), icon-<size>.png and the Android launcher icons.

fn write(path: &std::path::Path, size: u32, background: bool) {
    let img = bl_image::Image {
        width: size,
        height: size,
        pixels: bl_core::icon::icon_rgba(size, background),
    };
    if let Some(p) = path.parent() {
        std::fs::create_dir_all(p).expect("create dir");
    }
    std::fs::write(path, bl_image::encode_png(&img)).expect("write png");
}

fn main() {
    let out = std::path::PathBuf::from(std::env::args().nth(1).unwrap_or_else(|| ".".into()));
    write(&out.join("icon.png"), 512, true);
    for s in [16, 32, 48, 64, 128, 256] {
        write(&out.join(format!("icon-{s}.png")), s, true);
    }
    for (dir, s) in [
        ("mdpi", 48),
        ("hdpi", 72),
        ("xhdpi", 96),
        ("xxhdpi", 144),
        ("xxxhdpi", 192),
    ] {
        write(
            &out.join(format!("android/res/mipmap-{dir}/ic_launcher.png")),
            s,
            true,
        );
    }
}
