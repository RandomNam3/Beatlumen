//! Effect presets of a login: one database entry per preset
//! (`<scope>preset/<name>`: the preset document as the app sent it).

use crate::store::Part;
use crate::util;
use bl_json::Value;
use std::sync::Mutex;

/// Largest accepted preset document.
pub const MAX_PRESET_BYTES: usize = 4 * 1024 * 1024;

pub type Failure = (u16, String);

pub struct Presets {
    part: Part,
    write_lock: Mutex<()>,
}

impl Presets {
    pub fn new(part: Part) -> Presets {
        Presets {
            part,
            write_lock: Mutex::new(()),
        }
    }

    fn name(name: &str) -> Result<&str, Failure> {
        util::validate_name(name).map_err(|e| (400, format!("invalid preset name: {e}")))
    }

    /// `[{"name": ...}]`, sorted case-insensitively.
    pub fn list(&self) -> Value {
        let mut names: Vec<String> = self
            .part
            .list()
            .into_iter()
            .map(|(name, _)| name)
            .filter(|n| util::validate_name(n).is_ok())
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
        let name = Self::name(name)?;
        self.part
            .get(name)
            .ok_or((404, "preset not found".to_string()))
    }

    /// Stores a preset. Returns true when it was newly created.
    pub fn put(&self, name: &str, body: &[u8]) -> Result<bool, Failure> {
        let name = Self::name(name)?;
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
        let existed = self.part.get(name).is_some();
        self.part.put(name, body).map_err(|e| {
            crate::error!("cannot save preset {name}: {e}");
            (500, "cannot save preset".to_string())
        })?;
        Ok(!existed)
    }

    pub fn delete(&self, name: &str) -> Result<(), Failure> {
        let name = Self::name(name)?;
        let _g = self.write_lock.lock().unwrap_or_else(|e| e.into_inner());
        match self.part.delete(name) {
            Ok(true) => Ok(()),
            Ok(false) => Err((404, "preset not found".to_string())),
            Err(e) => {
                crate::error!("cannot delete preset {name}: {e}");
                Err((500, "cannot delete preset".to_string()))
            }
        }
    }
}
