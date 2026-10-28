//! Optional build-time embedding of the web client.
//!
//! When the environment variable `BEATLUMEN_EMBED_WEB=<dir>` is set while
//! building, every regular file below `<dir>` is embedded into the binary
//! (via a generated `include_bytes!` table) and served when no `--web-dir`
//! is configured and no `web/` directory exists next to the executable.
//! Without the variable an empty table is generated, so the build never
//! depends on the web client being built first.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

fn collect(dir: &Path, rel: &str, depth: usize, out: &mut Vec<(String, PathBuf)>) {
    if depth > 32 {
        return;
    }
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    let mut items: Vec<_> = rd.flatten().collect();
    items.sort_by_key(|e| e.file_name());
    for e in items {
        let name = e.file_name().to_string_lossy().into_owned();
        if name.starts_with('.') {
            continue;
        }
        let path = e.path();
        let child_rel = if rel.is_empty() {
            name.clone()
        } else {
            format!("{rel}/{name}")
        };
        let Ok(meta) = std::fs::metadata(&path) else {
            continue;
        };
        if meta.is_dir() {
            collect(&path, &child_rel, depth + 1, out);
        } else if meta.is_file() {
            out.push((child_rel, path));
        }
    }
}

fn main() {
    println!("cargo:rerun-if-env-changed=BEATLUMEN_EMBED_WEB");
    let out_dir = PathBuf::from(std::env::var_os("OUT_DIR").expect("OUT_DIR"));
    let mut files = Vec::new();
    if let Some(dir) = std::env::var_os("BEATLUMEN_EMBED_WEB").filter(|d| !d.is_empty()) {
        let dir = PathBuf::from(dir);
        let dir = std::fs::canonicalize(&dir).unwrap_or(dir);
        println!("cargo:rerun-if-changed={}", dir.display());
        collect(&dir, "", 0, &mut files);
        if files.is_empty() {
            println!(
                "cargo:warning=BEATLUMEN_EMBED_WEB={} contains no files; nothing embedded",
                dir.display()
            );
        }
        for (_, p) in &files {
            println!("cargo:rerun-if-changed={}", p.display());
        }
    }
    let mut src =
        String::from("/// Embedded web client files (path relative to the web root, contents).\n");
    src.push_str("pub static EMBEDDED_WEB: &[(&str, &[u8])] = &[\n");
    for (rel, path) in &files {
        let abs = path.to_string_lossy();
        let _ = writeln!(src, "    ({rel:?}, include_bytes!({abs:?})),");
    }
    src.push_str("];\n");
    std::fs::write(out_dir.join("embedded_web.rs"), src).expect("write embedded_web.rs");
}
