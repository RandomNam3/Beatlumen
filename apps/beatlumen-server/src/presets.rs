//! Shared effect presets stored as `data/presets/<name>.hfx`.

use crate::util;
use bl_json::Value;
use std::path::PathBuf;
use std::sync::Mutex;

/// Largest accepted preset document.
pub const MAX_PRESET_BYTES: usize = 4 * 1024 * 1024;

pub type Failure = (u16, String);

pub struct Presets {
    dir: PathBuf,
    write_lock: Mutex<()>,
}

impl Presets {
    pub fn new(dir: PathBuf) -> Presets {
        Presets {
            dir,
            write_lock: Mutex::new(()),
        }
    }

    fn path(&self, name: &str) -> Result<PathBuf, Failure> {
        let name =
            util::validate_name(name).map_err(|e| (400, format!("invalid preset name: {e}")))?;
        Ok(self.dir.join(format!("{name}.hfx")))
    }

    /// `[{"name": ...}]`, sorted case-insensitively.
    pub fn list(&self) -> Value {
        let mut names: Vec<String> = std::fs::read_dir(&self.dir)
            .into_iter()
            .flatten()
            .flatten()
            .filter_map(|e| {
                let name = e.file_name().into_string().ok()?;
                let stem = name
                    .strip_suffix(".hfx")
                    .or_else(|| name.strip_suffix(".HFX"))?;
                (util::validate_name(stem).is_ok() && e.file_type().ok()?.is_file())
                    .then(|| stem.to_string())
            })
            .collect();
        names.sort_by_key(|n| n.to_lowercase());
        Value::Array(
            names
                .into_iter()
                .map(|n| Value::object().with("name", n))
                .collect(),
        )
    }

    pub fn get(&self, name: &str) -> Result<Vec<u8>, Failure> {
        let p = self.path(name)?;
        std::fs::read(&p).map_err(|_| (404, "preset not found".to_string()))
    }

    /// Stores a preset. Returns true when it was newly created.
    pub fn put(&self, name: &str, body: &[u8]) -> Result<bool, Failure> {
        let p = self.path(name)?;
        if body.len() > MAX_PRESET_BYTES {
            return Err((413, "preset too large".into()));
        }
        let v = bl_json::parse_bytes(body).map_err(|e| (400, format!("invalid JSON: {e}")))?;
        if v.as_object().is_none() {
            return Err((400, "preset must be a JSON object".into()));
        }
        if v.get("effects").as_array().is_none() {
            return Err((400, "preset must contain an \"effects\" array".into()));
        }
        let _g = self.write_lock.lock().unwrap_or_else(|e| e.into_inner());
        let existed = p.exists();
        util::write_atomic(&p, body).map_err(|e| {
            crate::error!("cannot write {}: {e}", p.display());
            (500, "cannot save preset".to_string())
        })?;
        Ok(!existed)
    }

    pub fn delete(&self, name: &str) -> Result<(), Failure> {
        let p = self.path(name)?;
        let _g = self.write_lock.lock().unwrap_or_else(|e| e.into_inner());
        std::fs::remove_file(&p).map_err(|e| match e.kind() {
            std::io::ErrorKind::NotFound => (404, "preset not found".to_string()),
            _ => (500, "cannot delete preset".to_string()),
        })
    }
}
