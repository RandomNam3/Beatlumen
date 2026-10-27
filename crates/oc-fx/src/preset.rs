//! Effect presets (`.hfx` JSON files, compatible with Hyperchroma 0.9.x).
//!
//! Effects are stored as JSON values (so unknown fields survive a load/save
//! round trip); typed accessors read values with schema defaults.

use crate::rng::Rng;
use crate::schema::{self, EffectSpec, Field, FieldKind, TriggerKind};
use oc_analysis::{AnalysisConfig, NoveltyConfig, Spectrum};
use oc_json::{Map, Value};

pub const PRESET_VERSION: &str = "1.0.0";

#[derive(Debug, Clone, PartialEq)]
pub struct Effect {
    pub id: String,
    pub name: String,
    pub effect_type: String,
    pub ty: String,
    pub disabled: bool,
    pub config: Value,
    pub trigger: Value,
    pub camera: Value,
    /// Unknown top level members, preserved on save.
    pub extra: Map,
}

fn default_value(f: &Field) -> Value {
    match f.kind {
        FieldKind::Easing | FieldKind::Enum(_) => Value::from(f.default_str),
        _ => Value::from(f.default),
    }
}

/// Inserts defaults for missing fields and coerces wrong types.
fn fill(obj: &mut Value, fields: &[Field]) {
    if !matches!(obj, Value::Object(_)) {
        *obj = Value::object();
    }
    for f in fields {
        let cur = obj.get(f.key);
        let ok = match f.kind {
            FieldKind::Easing | FieldKind::Enum(_) => cur.as_str().is_some(),
            _ => cur.as_f64().is_some(),
        };
        if !ok {
            obj.set(f.key, default_value(f));
        }
    }
}

impl Effect {
    /// Creates an effect of the given type with default values.
    pub fn new(spec: &EffectSpec, rng: &mut Rng) -> Effect {
        let mut e = Effect {
            id: rng.id(),
            name: spec.default_name.to_string(),
            effect_type: spec.effect_type.to_string(),
            ty: spec.ty.to_string(),
            disabled: false,
            config: Value::object(),
            trigger: Value::Null,
            camera: Value::Null,
            extra: Map::new(),
        };
        e.normalize();
        e
    }

    pub fn spec(&self) -> Option<&'static EffectSpec> {
        schema::find(&self.effect_type, &self.ty)
    }

    /// Fills in missing fields with defaults according to the schema.
    pub fn normalize(&mut self) {
        let Some(spec) = self.spec() else { return };
        fill(&mut self.config, spec.config);
        match spec.trigger {
            TriggerKind::None => self.trigger = Value::Null,
            TriggerKind::Novelty => {
                fill(&mut self.trigger, schema::NOVELTY_TRIGGER);
                fill(&mut self.trigger, spec.targets);
            }
            TriggerKind::Transition => fill(&mut self.trigger, schema::TRANSITION_TRIGGER),
        }
        if spec.camera {
            if !matches!(self.camera, Value::Object(_)) {
                self.camera = Value::object();
            }
            if self.camera.get("speed").as_f64().is_none() {
                self.camera.set("speed", 0.0);
            }
            for (key, fields) in [("exit", schema::CAMERA_EXIT), ("enter", schema::CAMERA_ENTER), ("finish", schema::CAMERA_FINISH)] {
                let mut block = self.camera.get(key).clone();
                fill(&mut block, fields);
                self.camera.set(key, block);
            }
        }
    }

    /// Parses an effect; returns `None` for unknown effect types (which are
    /// skipped, like the original application does).
    pub fn from_json(v: &Value, rng: &mut Rng) -> Option<Effect> {
        let effect_type = v.get("effectType").as_str()?.to_string();
        let ty = v.get("type").as_str()?.to_string();
        let spec = schema::find(&effect_type, &ty)?;
        let mut extra = Map::new();
        if let Some(o) = v.as_object() {
            for (k, val) in o.iter() {
                if !matches!(k, "id" | "name" | "effectType" | "type" | "disabled" | "config" | "trigger" | "camera") {
                    extra.insert(k, val.clone());
                }
            }
        }
        let mut e = Effect {
            id: v.get("id").as_str().map(str::to_string).unwrap_or_else(|| rng.id()),
            name: v.get("name").as_str().unwrap_or(spec.default_name).to_string(),
            effect_type,
            ty,
            disabled: v.get("disabled").as_bool().unwrap_or(false),
            config: v.get("config").clone(),
            trigger: v.get("trigger").clone(),
            camera: v.get("camera").clone(),
            extra,
        };
        e.normalize();
        Some(e)
    }

    pub fn to_json(&self) -> Value {
        let mut m = Map::new();
        m.insert("id", self.id.as_str());
        m.insert("effectType", self.effect_type.as_str());
        m.insert("type", self.ty.as_str());
        m.insert("name", self.name.as_str());
        m.insert("disabled", self.disabled);
        m.insert("config", self.config.clone());
        if !self.trigger.is_null() {
            m.insert("trigger", self.trigger.clone());
        }
        if !self.camera.is_null() {
            m.insert("camera", self.camera.clone());
        }
        for (k, v) in self.extra.iter() {
            m.insert(k, v.clone());
        }
        Value::Object(m)
    }

    fn field_default(fields: &[Field], key: &str) -> f32 {
        fields.iter().find(|f| f.key == key).map(|f| f.default as f32).unwrap_or(0.0)
    }

    /// Numeric config value (schema default when missing).
    pub fn cfg(&self, key: &str) -> f32 {
        self.config.get(key).as_f32().unwrap_or_else(|| self.spec().map(|s| Self::field_default(s.config, key)).unwrap_or(0.0))
    }

    pub fn cfg_str(&self, key: &str) -> &str {
        self.config.get(key).as_str().unwrap_or("")
    }

    /// Numeric trigger value (schema default when missing).
    pub fn trig(&self, key: &str) -> f32 {
        self.trigger.get(key).as_f32().unwrap_or_else(|| {
            let base = Self::field_default(schema::NOVELTY_TRIGGER, key);
            self.spec().and_then(|s| s.targets.iter().find(|f| f.key == key)).map(|f| f.default as f32).unwrap_or(base)
        })
    }

    pub fn trigger_type(&self) -> &str {
        self.trigger.get("type").as_str().unwrap_or("None")
    }

    pub fn is_camera_transition(&self) -> bool {
        self.effect_type == "Transition" && (self.ty == "CameraMove" || self.ty == "CameraSwitch")
    }
}

/// A preset: ordered effects plus analysis settings.
#[derive(Debug, Clone, PartialEq)]
pub struct Preset {
    pub name: String,
    pub effects: Vec<Effect>,
    /// `analysisConfig` object (always normalised).
    pub analysis: Value,
    /// `noveltyConfigs` object with `Low`, `Mid`, `High` (always normalised).
    pub novelty: Value,
}

impl Default for Preset {
    fn default() -> Self {
        let mut p = Preset { name: "New Preset".into(), effects: Vec::new(), analysis: Value::Null, novelty: Value::Null };
        p.normalize();
        p
    }
}

impl Preset {
    pub fn normalize(&mut self) {
        fill(&mut self.analysis, schema::ANALYSIS);
        if !matches!(self.novelty, Value::Object(_)) {
            self.novelty = Value::object();
        }
        for (key, spectrum, norm, start, end) in schema::NOVELTY_DEFAULTS {
            let mut v = self.novelty.get(key).clone();
            if !matches!(v, Value::Object(_)) {
                v = Value::object()
                    .with("spectrum", spectrum)
                    .with("normalization", norm)
                    .with("startFreq", start)
                    .with("endFreq", end);
            }
            fill(&mut v, schema::NOVELTY);
            self.novelty.set(key, v);
        }
    }

    pub fn from_json(v: &Value, name: &str, rng: &mut Rng) -> Result<Preset, String> {
        if !matches!(v, Value::Object(_)) {
            return Err("preset must be a JSON object".into());
        }
        let effects = match v.get("effects") {
            Value::Array(a) => a.iter().filter_map(|e| Effect::from_json(e, rng)).collect(),
            Value::Null => Vec::new(),
            _ => return Err("\"effects\" must be an array".into()),
        };
        let mut p = Preset { name: name.to_string(), effects, analysis: v.get("analysisConfig").clone(), novelty: v.get("noveltyConfigs").clone() };
        p.normalize();
        Ok(p)
    }

    pub fn parse(bytes: &[u8], name: &str, rng: &mut Rng) -> Result<Preset, String> {
        let v = oc_json::parse_bytes(bytes).map_err(|e| e.to_string())?;
        Preset::from_json(&v, name, rng)
    }

    pub fn to_json(&self) -> Value {
        Value::object()
            .with("version", PRESET_VERSION)
            .with("effects", Value::Array(self.effects.iter().map(Effect::to_json).collect()))
            .with("analysisConfig", self.analysis.clone())
            .with("noveltyConfigs", self.novelty.clone())
    }

    /// Serializes with two-space indentation (as `.hfx` files are written).
    pub fn to_string_pretty(&self) -> String {
        self.to_json().to_json_pretty()
    }

    /// Builds the analysis configuration for `oc-analysis`.
    pub fn analysis_config(&self) -> AnalysisConfig {
        let a = &self.analysis;
        let int = |k: &str, d: usize| a.get(k).as_f64().map(|v| v.max(1.0) as usize).unwrap_or(d);
        let novelty = ["Low", "Mid", "High"]
            .iter()
            .map(|k| {
                let n = self.novelty.get(k);
                NoveltyConfig {
                    spectrum: Spectrum::from_name(n.str_or("spectrum", "Percussive")).unwrap_or(Spectrum::Percussive),
                    normalization: n.f32_or("normalization", 1.0),
                    start_freq: n.f32_or("startFreq", 0.0),
                    end_freq: n.f32_or("endFreq", 300.0),
                }
            })
            .collect();
        AnalysisConfig {
            fft_size: 4096,
            spectral_smoothing: int("spectralSmoothing", 4),
            spectral_separation: a.f32_or("spectralSeparation", 3.0),
            chroma_smoothing: int("chromaSmoothing", 30),
            chroma_down_sampling: int("chromaDownSampling", 10),
            major_transition_window: int("majorTransitionWindow", 30),
            minor_transition_window: int("minorTransitionWindow", 5),
            novelty,
        }
    }

    pub fn find_effect(&self, id: &str) -> Option<&Effect> {
        self.effects.iter().find(|e| e.id == id)
    }

    pub fn find_effect_mut(&mut self, id: &str) -> Option<&mut Effect> {
        self.effects.iter_mut().find(|e| e.id == id)
    }

    /// Returns ids of enabled Camera/Filter effects that are shadowed by a
    /// later effect of the same type (only the last instance takes effect).
    pub fn shadowed_effects(&self) -> Vec<String> {
        let mut out = Vec::new();
        for (i, e) in self.effects.iter().enumerate() {
            if e.disabled || !(e.effect_type == "Camera" || e.effect_type == "Filter") {
                continue;
            }
            if self.effects[i + 1..].iter().any(|o| !o.disabled && o.effect_type == e.effect_type && o.ty == e.ty) {
                out.push(e.id.clone());
            }
        }
        out
    }
}

/// Built-in presets shipped with the application (name, JSON source).
pub const BUILTIN: &[(&str, &str)] = &[
    ("Balanced", include_str!("../../../presets/balanced.hfx")),
    ("Energetic", include_str!("../../../presets/energetic.hfx")),
    ("Calm", include_str!("../../../presets/calm.hfx")),
];

pub fn builtin(name: &str, rng: &mut Rng) -> Option<Preset> {
    BUILTIN.iter().find(|(n, _)| *n == name).and_then(|(n, src)| Preset::parse(src.as_bytes(), n, rng).ok())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtins_parse_and_roundtrip() {
        let mut rng = Rng::new(7);
        for (name, src) in BUILTIN {
            let p = Preset::parse(src.as_bytes(), name, &mut rng).unwrap();
            assert!(!p.effects.is_empty(), "{name}");
            let again = Preset::parse(p.to_string_pretty().as_bytes(), name, &mut rng).unwrap();
            assert_eq!(p, again);
            let cfg = p.analysis_config();
            assert_eq!(cfg.novelty.len(), 3);
        }
    }

    #[test]
    fn defaults_are_filled() {
        let mut rng = Rng::new(1);
        let src = r#"{"version":"1.0.0","effects":[{"effectType":"Filter","type":"Bloom","config":{}},{"effectType":"Bogus","type":"X"},
            {"effectType":"Transition","type":"Wipe","config":{"easing":"BouceIn"},"trigger":{"type":"Major"}, "custom": 5}]}"#;
        let p = Preset::parse(src.as_bytes(), "t", &mut rng).unwrap();
        assert_eq!(p.effects.len(), 2);
        assert_eq!(p.effects[0].cfg("passes"), 1.0);
        assert_eq!(p.effects[0].trig("smoothing"), 0.8);
        assert_eq!(p.effects[1].camera.get("finish").str_or("easing", ""), "CubicIn");
        assert_eq!(p.effects[1].extra.get("custom").and_then(|v| v.as_i64()), Some(5));
        assert_eq!(p.novelty.get("Mid").f64_or("endFreq", 0.0), 800.0);
    }

    #[test]
    fn every_spec_constructs() {
        let mut rng = Rng::new(3);
        for spec in crate::schema::EFFECTS {
            let e = Effect::new(spec, &mut rng);
            let v = e.to_json();
            let back = Effect::from_json(&v, &mut rng).unwrap();
            assert_eq!(e, back);
        }
    }
}
