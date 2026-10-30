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
    /// SHA-256 fingerprint (hex) of the server's TLS certificate the user
    /// chose to trust (self-signed certificates on home servers).
    pub server_cert_pin: String,
    pub use_server: bool,
    /// Local library folders (native platforms).
    pub library_folders: Vec<String>,
    pub preset: String,
    pub audio_queue: Vec<String>,
    pub audio_index: Option<usize>,
    /// Where the current track was (seconds), to continue after a restart.
    pub audio_position: f64,
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
    /// Shifts the effects against the audio in milliseconds (positive =
    /// earlier), for outputs with unreported latency (e.g. Bluetooth).
    pub sync_offset_ms: f32,
    /// Live sources: effects on the predicted beats.
    pub beat_prediction: bool,
    /// Most space offline copies may take (MB, 0 = no limit).
    pub offline_limit_mb: u64,
    /// Server songs converted to Opus at this bitrate (kbit/s, 0 = the
    /// original files), on Wi-Fi/cable and on mobile data.
    pub stream_kbps: u32,
    pub stream_kbps_metered: u32,
    /// Speech model for timing song texts on the device ("" = none).
    pub speech_model: String,
    /// Translate song texts into the app's language (on the device).
    pub translate: bool,
    /// Adapt effects automatically to the energy and sections of the track.
    pub adaptive: bool,
    /// Play all tracks at a similar loudness (EBU R128 measurement).
    pub normalize: bool,
    /// Crossfade between tracks in seconds (0 = gapless).
    pub crossfade: f32,
    /// Skip silence at the end and start of tracks when moving on.
    pub skip_silence: bool,
    /// Offer this device as an AirPlay speaker in the local network.
    pub airplay: bool,
    /// Also announce the AirPlay 2 receiver (experimental).
    pub airplay2: bool,
    /// Reachable as a DLNA/UPnP speaker (media renderer).
    pub dlna_receiver: bool,
    /// Control points allowed to play here (addresses); others are asked
    /// for first.
    pub dlna_allowed: Vec<String>,
    /// Where songs from server extensions go: "" (ask), "library", "vault".
    pub extension_target: String,
    /// The renderer's stable UPnP device id.
    pub dlna_uuid: String,
    /// Name shown on AirPlay senders (empty = "Beatlumen").
    /// Account name last used on the server (prefilled when unlocking).
    /// Push notifications (UnifiedPush) when files are shared.
    pub notifications: bool,
    pub vault_user: String,
    /// Keep the vault unlocked on this device (platform's protected store).
    pub vault_remember: bool,
    pub airplay_name: String,
    /// Password AirPlay senders must enter (empty = none).
    pub airplay_password: String,
    /// Random device ID announced instead of a hardware address (hex).
    pub airplay_id: String,
    /// This device among the devices of a server login (random, hex).
    pub device_id: String,
    /// Its name there (empty: the platform's name).
    pub device_name: String,
    /// Other devices may control this one.
    pub remote_control: bool,
    /// Settings that follow the account: {key: {value, t}} as last
    /// synced, and the account they belong to.
    pub sync_items: Value,
    pub sync_user: String,
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
            server_cert_pin: String::new(),
            use_server: false,
            library_folders: Vec::new(),
            preset: "Balanced".into(),
            audio_queue: Vec::new(),
            audio_index: None,
            audio_position: 0.0,
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
            sync_offset_ms: 0.0,
            beat_prediction: true,
            offline_limit_mb: 0,
            stream_kbps: 0,
            stream_kbps_metered: 0,
            speech_model: String::new(),
            translate: false,
            adaptive: true,
            normalize: false,
            crossfade: 0.0,
            skip_silence: true,
            airplay: false,
            dlna_receiver: false,
            airplay2: false,
            dlna_allowed: Vec::new(),
            extension_target: String::new(),
            dlna_uuid: String::new(),
            airplay_name: String::new(),
            vault_user: String::new(),
            vault_remember: true,
            notifications: true,
            airplay_password: String::new(),
            airplay_id: String::new(),
            device_id: String::new(),
            device_name: String::new(),
            remote_control: true,
            sync_items: Value::object(),
            sync_user: String::new(),
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
            server_cert_pin: v.str_or("serverCertPin", "").to_string(),
            use_server: v.bool_or("useServer", false),
            library_folders: strs(v.get("libraryFolders")),
            preset: v.str_or("preset", &d.preset).to_string(),
            audio_queue: strs(v.get("audioQueue")),
            audio_index: idx("audioIndex"),
            audio_position: v.f64_or("audioPosition", 0.0).max(0.0),
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
            sync_offset_ms: v
                .f32_or("syncOffsetMs", d.sync_offset_ms)
                .clamp(-500.0, 500.0),
            beat_prediction: v.bool_or("beatPrediction", d.beat_prediction),
            offline_limit_mb: v.get("offlineLimitMb").as_u64().unwrap_or(0).min(1 << 24),
            stream_kbps: stream_kbps(v.get("streamKbps").as_u64()),
            stream_kbps_metered: stream_kbps(v.get("streamKbpsMetered").as_u64()),
            speech_model: v.str_or("speechModel", "").chars().take(64).collect(),
            translate: v.bool_or("translate", false),
            adaptive: v.bool_or("adaptive", d.adaptive),
            normalize: v.bool_or("normalize", d.normalize),
            crossfade: v.f32_or("crossfade", d.crossfade).clamp(0.0, 12.0),
            skip_silence: v.bool_or("skipSilence", d.skip_silence),
            airplay: v.bool_or("airplay", false),
            dlna_receiver: v.bool_or("dlnaReceiver", false),
            airplay2: v.bool_or("airplay2", false),
            dlna_allowed: strs(v.get("dlnaAllowed")).into_iter().take(64).collect(),
            extension_target: match v.str_or("extensionTarget", "") {
                t @ ("library" | "vault") => t.to_string(),
                _ => String::new(),
            },
            dlna_uuid: v.str_or("dlnaUuid", "").chars().take(48).collect(),
            airplay_name: v.str_or("airplayName", "").chars().take(48).collect(),
            vault_user: v.str_or("vaultUser", "").chars().take(32).collect(),
            vault_remember: v.bool_or("vaultRemember", true),
            notifications: v.bool_or("notifications", true),
            airplay_password: v.str_or("airplayPassword", "").chars().take(64).collect(),
            airplay_id: v.str_or("airplayId", "").to_string(),
            device_id: v
                .str_or("deviceId", "")
                .chars()
                .filter(|c| c.is_ascii_hexdigit())
                .take(32)
                .collect(),
            device_name: v.str_or("deviceName", "").trim().chars().take(48).collect(),
            remote_control: v.bool_or("remoteControl", true),
            sync_items: match v.get("syncItems") {
                o @ Value::Object(_) => o.clone(),
                _ => Value::object(),
            },
            sync_user: v.str_or("syncUser", "").chars().take(32).collect(),
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
            .with("serverCertPin", self.server_cert_pin.as_str())
            .with("useServer", self.use_server)
            .with("libraryFolders", self.library_folders.clone())
            .with("preset", self.preset.as_str())
            .with("audioQueue", self.audio_queue.clone())
            .with("audioIndex", self.audio_index)
            .with("audioPosition", self.audio_position)
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
            .with("syncOffsetMs", self.sync_offset_ms)
            .with("beatPrediction", self.beat_prediction)
            .with("offlineLimitMb", self.offline_limit_mb)
            .with("streamKbps", self.stream_kbps)
            .with("streamKbpsMetered", self.stream_kbps_metered)
            .with("speechModel", self.speech_model.as_str())
            .with("translate", self.translate)
            .with("adaptive", self.adaptive)
            .with("normalize", self.normalize)
            .with("crossfade", self.crossfade)
            .with("skipSilence", self.skip_silence)
            .with("airplay", self.airplay)
            .with("dlnaReceiver", self.dlna_receiver)
            .with("airplay2", self.airplay2)
            .with("dlnaAllowed", self.dlna_allowed.clone())
            .with("extensionTarget", self.extension_target.as_str())
            .with("dlnaUuid", self.dlna_uuid.as_str())
            .with("airplayName", self.airplay_name.as_str())
            .with("vaultUser", self.vault_user.as_str())
            .with("vaultRemember", self.vault_remember)
            .with("notifications", self.notifications)
            .with("airplayPassword", self.airplay_password.as_str())
            .with("deviceId", self.device_id.as_str())
            .with("deviceName", self.device_name.as_str())
            .with("remoteControl", self.remote_control)
            .with("syncItems", self.sync_items.clone())
            .with("syncUser", self.sync_user.as_str())
            .with("airplayId", self.airplay_id.as_str())
            .with("ui", self.ui.clone())
    }
}

/// A stream bitrate: 0 (original files) or 24..=256 kbit/s.
fn stream_kbps(v: Option<u64>) -> u32 {
    match v.unwrap_or(0) {
        0 => 0,
        k => k.clamp(24, 256) as u32,
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
