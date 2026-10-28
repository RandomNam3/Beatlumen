//! Persisted application settings (`settings.json` in host storage).

use bl_json::Value;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Repeat {
    None,
    Queue,
    Item,
}

impl Repeat {
    pub fn name(self) -> &'static str {
        match self {
            Repeat::None => "None",
            Repeat::Queue => "Queue",
            Repeat::Item => "Item",
        }
    }
    pub fn parse(s: &str) -> Repeat {
        match s {
            "Queue" => Repeat::Queue,
            "Item" => Repeat::Item,
            _ => Repeat::None,
        }
    }
    pub fn next(self) -> Repeat {
        match self {
            Repeat::None => Repeat::Queue,
            Repeat::Queue => Repeat::Item,
            Repeat::Item => Repeat::None,
        }
    }
}

/// When the background queue advances.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackgroundMode {
    Manual,
    OnTrackFinish,
    OnTrackChange,
}

impl BackgroundMode {
    pub const ALL: [BackgroundMode; 3] = [
        BackgroundMode::Manual,
        BackgroundMode::OnTrackFinish,
        BackgroundMode::OnTrackChange,
    ];
    pub fn name(self) -> &'static str {
        match self {
            BackgroundMode::Manual => "Manual",
            BackgroundMode::OnTrackFinish => "OnTrackFinish",
            BackgroundMode::OnTrackChange => "OnTrackChange",
        }
    }
    pub fn parse(s: &str) -> BackgroundMode {
        match s {
            "Manual" => BackgroundMode::Manual,
            "OnTrackFinish" => BackgroundMode::OnTrackFinish,
            _ => BackgroundMode::OnTrackChange,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VisualizerKind {
    None,
    Spectrum,
    Waveform,
}

impl VisualizerKind {
    pub const ALL: [VisualizerKind; 3] = [
        VisualizerKind::None,
        VisualizerKind::Spectrum,
        VisualizerKind::Waveform,
    ];
    pub fn name(self) -> &'static str {
        match self {
            VisualizerKind::None => "None",
            VisualizerKind::Spectrum => "Spectrum",
            VisualizerKind::Waveform => "Waveform",
        }
    }
    pub fn parse(s: &str) -> VisualizerKind {
        match s {
            "None" => VisualizerKind::None,
            "Waveform" => VisualizerKind::Waveform,
            _ => VisualizerKind::Spectrum,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Settings {
    pub language: String,
    pub volume: f32,
    pub muted: bool,
    /// Extra UI zoom on top of the platform DPI scale (0.5..2).
    /// Internal resolution of the visualizer relative to the window.
    pub render_scale: f32,
    pub server_url: String,
    pub server_token: String,
    pub use_server: bool,
    /// Local library folders (native platforms).
    pub library_folders: Vec<String>,
    pub preset: String,
    pub audio_queue: Vec<String>,
    pub audio_index: Option<usize>,
    pub shuffle: bool,
    pub repeat: Repeat,
    pub backgrounds: Vec<String>,
    pub background_index: Option<usize>,
    pub background_shuffle: bool,
    pub background_repeat: Repeat,
    pub background_mode: BackgroundMode,
    /// Use the album cover as background when the background queue is empty.
    pub cover_fallback: bool,
    pub visualizer: VisualizerKind,
    pub spectrum_min_freq: f32,
    pub spectrum_max_freq: f32,
    pub spectrum_smoothing: f32,
    pub bar_width: f32,
    pub bar_gap: f32,
    pub waveform_thickness: f32,
    /// Global effect intensity multiplier (0 = calm, 1 = as designed, 2 = wild).
    pub intensity: f32,
    /// Adapt effects automatically to the energy and sections of the track.
    pub adaptive: bool,
    /// Play all tracks at a similar loudness (EBU R128 measurement).
    pub normalize: bool,
    /// Settings owned by the user interface (theme, accent colour, …),
    /// stored as given.
    pub ui: Value,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            language: String::new(),
            volume: 0.6,
            muted: false,
            render_scale: 1.0,
            server_url: String::new(),
            server_token: String::new(),
            use_server: false,
            library_folders: Vec::new(),
            preset: "Balanced".into(),
            audio_queue: Vec::new(),
            audio_index: None,
            shuffle: false,
            repeat: Repeat::None,
            backgrounds: Vec::new(),
            background_index: None,
            background_shuffle: false,
            background_repeat: Repeat::Queue,
            background_mode: BackgroundMode::OnTrackChange,
            cover_fallback: true,
            visualizer: VisualizerKind::Spectrum,
            spectrum_min_freq: 0.0,
            spectrum_max_freq: 2000.0,
            spectrum_smoothing: 0.8,
            bar_width: 2.0,
            bar_gap: 5.0,
            waveform_thickness: 0.3,
            intensity: 1.0,
            adaptive: true,
            normalize: false,
            ui: Value::object(),
        }
    }
}

fn strs(v: &Value) -> Vec<String> {
    v.as_array()
        .map(|a| {
            a.iter()
                .filter_map(|x| x.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

impl Settings {
    pub const STORAGE_KEY: &'static str = "settings.json";

    pub fn from_json(v: &Value) -> Settings {
        let d = Settings::default();
        let idx = |k: &str| v.get(k).as_u64().map(|x| x as usize);
        Settings {
            language: v.str_or("language", "").to_string(),
            volume: v.f32_or("volume", d.volume).clamp(0.0, 1.0),
            muted: v.bool_or("muted", false),
            render_scale: v.f32_or("renderScale", 1.0).clamp(0.25, 1.0),
            server_url: v.str_or("serverUrl", "").to_string(),
            server_token: v.str_or("serverToken", "").to_string(),
            use_server: v.bool_or("useServer", false),
            library_folders: strs(v.get("libraryFolders")),
            preset: v.str_or("preset", &d.preset).to_string(),
            audio_queue: strs(v.get("audioQueue")),
            audio_index: idx("audioIndex"),
            shuffle: v.bool_or("shuffle", false),
            repeat: Repeat::parse(v.str_or("repeat", "None")),
            backgrounds: strs(v.get("backgrounds")),
            background_index: idx("backgroundIndex"),
            background_shuffle: v.bool_or("backgroundShuffle", false),
            background_repeat: Repeat::parse(v.str_or("backgroundRepeat", "Queue")),
            background_mode: BackgroundMode::parse(v.str_or("backgroundMode", "OnTrackChange")),
            cover_fallback: v.bool_or("coverFallback", true),
            visualizer: VisualizerKind::parse(v.str_or("visualizer", "Spectrum")),
            spectrum_min_freq: v.f32_or("spectrumMinFreq", d.spectrum_min_freq),
            spectrum_max_freq: v.f32_or("spectrumMaxFreq", d.spectrum_max_freq),
            spectrum_smoothing: v
                .f32_or("spectrumSmoothing", d.spectrum_smoothing)
                .clamp(0.0, 0.99),
            bar_width: v.f32_or("barWidth", d.bar_width).max(1.0),
            bar_gap: v.f32_or("barGap", d.bar_gap).max(0.0),
            waveform_thickness: v
                .f32_or("waveformThickness", d.waveform_thickness)
                .clamp(0.0, 0.99),
            intensity: v.f32_or("intensity", d.intensity).clamp(0.0, 2.0),
            adaptive: v.bool_or("adaptive", d.adaptive),
            normalize: v.bool_or("normalize", d.normalize),
            ui: if v.get("ui").as_object().is_some() {
                v.get("ui").clone()
            } else {
                Value::object()
            },
        }
    }

    pub fn to_json(&self) -> Value {
        Value::object()
            .with("version", "1.0.0")
            .with("language", self.language.as_str())
            .with("volume", self.volume)
            .with("muted", self.muted)
            .with("renderScale", self.render_scale)
            .with("serverUrl", self.server_url.as_str())
            .with("serverToken", self.server_token.as_str())
            .with("useServer", self.use_server)
            .with("libraryFolders", self.library_folders.clone())
            .with("preset", self.preset.as_str())
            .with("audioQueue", self.audio_queue.clone())
            .with("audioIndex", self.audio_index)
            .with("shuffle", self.shuffle)
            .with("repeat", self.repeat.name())
            .with("backgrounds", self.backgrounds.clone())
            .with("backgroundIndex", self.background_index)
            .with("backgroundShuffle", self.background_shuffle)
            .with("backgroundRepeat", self.background_repeat.name())
            .with("backgroundMode", self.background_mode.name())
            .with("coverFallback", self.cover_fallback)
            .with("visualizer", self.visualizer.name())
            .with("spectrumMinFreq", self.spectrum_min_freq)
            .with("spectrumMaxFreq", self.spectrum_max_freq)
            .with("spectrumSmoothing", self.spectrum_smoothing)
            .with("barWidth", self.bar_width)
            .with("barGap", self.bar_gap)
            .with("waveformThickness", self.waveform_thickness)
            .with("intensity", self.intensity)
            .with("adaptive", self.adaptive)
            .with("normalize", self.normalize)
            .with("ui", self.ui.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip() {
        let mut s = Settings::default();
        s.audio_queue = vec!["a".into(), "b".into()];
        s.audio_index = Some(1);
        s.repeat = Repeat::Item;
        s.background_mode = BackgroundMode::Manual;
        let back = Settings::from_json(&bl_json::parse(&s.to_json().to_json()).unwrap());
        assert_eq!(s, back);
    }
}
