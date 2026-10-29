//! Release tools: the integrity manifest of the web client and its
//! signing key (see "Web client integrity" in `docs/ENCRYPTION.md`).

use bl_vault::manifest::{self, Manifest};
use std::path::Path;

fn collect(
    dir: &Path,
    rel: &str,
    depth: usize,
    out: &mut Vec<(String, Vec<u8>)>,
) -> Result<(), String> {
    if depth > 16 {
        return Ok(());
    }
    let mut entries: Vec<_> = std::fs::read_dir(dir)
        .map_err(|e| format!("{}: {e}", dir.display()))?
        .flatten()
        .collect();
    entries.sort_by_key(|e| e.file_name());
    for e in entries {
        let name = e.file_name().to_string_lossy().into_owned();
        let r = if rel.is_empty() {
            name.clone()
        } else {
            format!("{rel}/{name}")
        };
        if name.starts_with('.') || r == manifest::MANIFEST || r == manifest::SIGNATURE {
            continue;
        }
        let t = e.file_type().map_err(|e| e.to_string())?;
        if t.is_dir() {
            collect(&e.path(), &r, depth + 1, out)?;
        } else if t.is_file() {
            out.push((r, std::fs::read(e.path()).map_err(|e| e.to_string())?));
        }
    }
    Ok(())
}

/// Writes `manifest.json` for the web client in `dir`, and `manifest.sig`
/// when `BEATLUMEN_RELEASE_SIGNING_KEY` (base64 seed) is set. Compressed
/// copies (`x.gz`) must match `x` and are not listed themselves.
pub fn write_manifest(dir: &Path, version: &str) -> Result<String, String> {
    let mut files = Vec::new();
    collect(dir, "", 0, &mut files)?;
    let plain: std::collections::HashMap<String, Vec<u8>> = files.iter().cloned().collect();
    let mut listed = Vec::new();
    for (name, data) in &files {
        if let Some(base) = name.strip_suffix(".gz") {
            if let Some(p) = plain.get(base) {
                let un = crate::web::gunzip_for_check(data).ok_or(format!("{name}: not gzip"))?;
                if &un != p {
                    return Err(format!("{name} does not match {base}"));
                }
                continue;
            }
        }
        listed.push((name.clone(), data.clone()));
    }
    let m = Manifest::new(version, &listed);
    let json = m.to_json();
    std::fs::write(dir.join(manifest::MANIFEST), &json).map_err(|e| e.to_string())?;
    let mut msg = format!(
        "{}: {} files",
        dir.join(manifest::MANIFEST).display(),
        listed.len()
    );
    let _ = std::fs::remove_file(dir.join(manifest::SIGNATURE));
    if let Some(seed) = std::env::var("BEATLUMEN_RELEASE_SIGNING_KEY")
        .ok()
        .filter(|s| !s.trim().is_empty())
    {
        let seed = bl_net::base64_decode(seed.trim())
            .and_then(|s| <[u8; 32]>::try_from(s).ok())
            .ok_or("BEATLUMEN_RELEASE_SIGNING_KEY must be 32 bytes in base64")?;
        let sig = manifest::base64_encode(&manifest::sign(&seed, json.as_bytes()));
        std::fs::write(dir.join(manifest::SIGNATURE), sig + "\n").map_err(|e| e.to_string())?;
        let public: String = manifest::public_key(&seed)
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        msg.push_str(&format!(", signed (release key {public})"));
    }
    Ok(msg)
}

/// A new release signing key: (secret seed base64, public key hex).
pub fn keygen() -> Result<(String, String), String> {
    let mut seed = [0u8; 32];
    crate::random::fill(&mut seed).map_err(|e| e.to_string())?;
    let public: String = manifest::public_key(&seed)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    Ok((bl_net::base64_encode(&seed), public))
}
