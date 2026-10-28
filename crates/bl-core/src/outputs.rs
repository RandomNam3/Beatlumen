//! Where the music plays: this device, or a speaker in the network
//! (AirPlay 1 receivers). A network output takes the player's audio
//! instead of the sound card: a thread renders it at the speaker's pace,
//! resamples it to 44.1 kHz and streams it. The visuals stay in sync by
//! reporting the speaker's delay as the output latency.

use crate::core::Core;
use crate::host::Host;
use crate::jobs::Mailbox;
use crate::player::Output;
use bl_airplay::sender::{self, Options, Sender, SAMPLE_RATE};
use bl_json::Value;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// How far ahead of the speaker's clock audio is sent (frames at 44.1 kHz).
const LEAD: u32 = SAMPLE_RATE / 4;

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Device {
    /// "airplay:<address>:<port>"
    pub id: String,
    pub name: String,
    pub model: String,
    addr: SocketAddr,
    /// Supported encryptions ("et").
    et: String,
    pub needs_password: bool,
}

/// What the core tells the output thread.
#[derive(Default)]
struct Control {
    volume: Option<f32>,
    metadata: Option<(String, String, String)>,
    artwork: Option<Vec<u8>>,
    progress: Option<(f64, f64)>,
}

enum Report {
    Connected,
    Failed(String),
}

struct Active {
    device: Device,
    stop: Arc<AtomicBool>,
    control: Arc<Mutex<Control>>,
    thread: Option<std::thread::JoinHandle<()>>,
    connected: bool,
}

impl Drop for Active {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

#[derive(Default)]
pub(crate) struct Outputs {
    devices: Vec<Device>,
    scanning: bool,
    found: Mailbox<Vec<Device>>,
    reports: Mailbox<Report>,
    active: Option<Active>,
    error: Option<String>,
    /// What was last sent to the speaker (to send changes only).
    sent_track: Option<String>,
    sent_volume: Option<f32>,
}

fn device_from(f: &bl_airplay::mdns::Found) -> Device {
    // "MACADDRESS@Name"
    let name = f
        .instance
        .split_once('@')
        .map(|(_, n)| n.to_string())
        .unwrap_or_else(|| f.instance.clone());
    Device {
        id: format!("airplay:{}:{}", f.addr, f.port),
        name,
        model: f.txt_value("am").unwrap_or("").to_string(),
        addr: SocketAddr::new(f.addr.into(), f.port),
        et: f.txt_value("et").unwrap_or("0").to_string(),
        needs_password: f
            .txt_value("pw")
            .is_some_and(|v| v.eq_ignore_ascii_case("true")),
    }
}

/// A speaker given by address ("airplay:<ip>:<port>").
fn device_from_id(id: &str) -> Option<Device> {
    let addr: SocketAddr = id.strip_prefix("airplay:")?.parse().ok()?;
    Some(Device {
        id: id.to_string(),
        name: addr.ip().to_string(),
        model: String::new(),
        addr,
        et: "0,1".into(),
        needs_password: false,
    })
}

/// The output thread: connects, then streams what the player renders.
#[allow(clippy::too_many_arguments)]
fn run(
    device: Device,
    password: Option<String>,
    name: String,
    output: Arc<Output>,
    device_rate: u32,
    control: Arc<Mutex<Control>>,
    stop: Arc<AtomicBool>,
    reports: Mailbox<Report>,
) {
    let opts = Options {
        password,
        encrypt: true,
        name,
    };
    let mut tx = match Sender::connect(device.addr, &device.et, &opts) {
        Ok(tx) => tx,
        Err(e) => {
            let msg = match e {
                sender::Error::Auth => "password".to_string(),
                e => e.to_string(),
            };
            reports.post(Report::Failed(msg));
            return;
        }
    };
    reports.post(Report::Connected);
    output.set_external(true);
    let mut resampler =
        (device_rate != SAMPLE_RATE).then(|| bl_media::Resampler::new(device_rate, SAMPLE_RATE, 2));
    // Heard this much later than rendered (device frames).
    let latency =
        ((sender::LATENCY + LEAD / 2) as u64 * device_rate as u64 / SAMPLE_RATE as u64) as u32;
    let mut resets = output.resets();
    let mut flushed = false;
    let mut render = Vec::new();
    let mut resampled = Vec::new();
    let mut pcm: Vec<i16> = Vec::new();
    let mut failed = None;
    while !stop.load(Ordering::Relaxed) {
        let pending = std::mem::take(&mut *control.lock().unwrap_or_else(|e| e.into_inner()));
        let result = (|| -> Result<(), sender::Error> {
            if let Some(v) = pending.volume {
                tx.set_volume(v)?;
            }
            if let Some((t, a, al)) = &pending.metadata {
                tx.set_metadata(t, a, al)?;
            }
            if let Some(art) = &pending.artwork {
                tx.set_artwork(art)?;
            }
            if let Some((pos, dur)) = pending.progress {
                tx.set_progress(pos, dur)?;
            }
            // A seek or a new track: drop what the speaker has buffered.
            let r = output.resets();
            if r != resets {
                resets = r;
                if !flushed {
                    tx.flush()?;
                }
                if let Some(rs) = resampler.as_mut() {
                    rs.reset();
                }
            }
            if output.is_silent() {
                if !flushed {
                    tx.flush()?;
                    flushed = true;
                }
                tx.keepalive()?;
                return Ok(());
            }
            flushed = false;
            let due = tx.frames_due(LEAD);
            if due > 0 {
                let frames =
                    (due as u64 * device_rate as u64).div_ceil(SAMPLE_RATE as u64) as usize;
                render.resize(frames * 2, 0.0);
                output.render_external(&mut render, latency);
                let samples: &[f32] = match resampler.as_mut() {
                    Some(rs) => {
                        resampled.clear();
                        rs.process(&render, &mut resampled);
                        &resampled
                    }
                    None => &render,
                };
                pcm.clear();
                pcm.extend(
                    samples
                        .iter()
                        .map(|v| (v.clamp(-1.0, 1.0) * 32767.0) as i16),
                );
                tx.send(&pcm)?;
            }
            tx.keepalive()
        })();
        if let Err(e) = result {
            failed = Some(e.to_string());
            break;
        }
        std::thread::sleep(Duration::from_millis(if flushed { 50 } else { 8 }));
    }
    output.set_external(false);
    if let Some(e) = failed {
        reports.post(Report::Failed(e));
    }
}

impl Core {
    /// Looks for speakers in the network (a few seconds).
    pub(crate) fn outputs_scan(&mut self, host: &mut dyn Host) {
        if self.outputs.scanning || !host.has_threads() || host.platform() == "web" {
            return;
        }
        self.outputs.scanning = true;
        let mailbox = self.outputs.found.clone();
        let own = self.airplay_instance();
        host.spawn(Box::new(move || {
            let found =
                bl_airplay::mdns::browse("_raop._tcp", Duration::from_secs(3)).unwrap_or_default();
            mailbox.post(
                found
                    .iter()
                    .filter(|f| Some(&f.instance) != own.as_ref())
                    .map(device_from)
                    .collect(),
            );
        }));
    }

    /// Plays on a speaker ("local": this device again).
    pub(crate) fn outputs_select(&mut self, host: &mut dyn Host, id: &str, password: &str) {
        self.outputs.active = None;
        self.outputs.error = None;
        self.outputs.sent_track = None;
        self.outputs.sent_volume = None;
        self.player.output().set_external(false);
        if id == "local" || id.is_empty() {
            return;
        }
        let known = self.outputs.devices.iter().find(|d| d.id == id).cloned();
        let Some(device) = known.or_else(|| device_from_id(id)) else {
            self.outputs.error = Some(self.tr("The speaker is not available").into());
            return;
        };
        let stop = Arc::new(AtomicBool::new(false));
        let control = Arc::new(Mutex::new(Control::default()));
        let (output, rate) = (self.player.output(), self.player.device_rate());
        let (s, c, r) = (stop.clone(), control.clone(), self.outputs.reports.clone());
        let d = device.clone();
        let password = Some(password.to_string()).filter(|p| !p.is_empty());
        let name = "Beatlumen".to_string();
        let thread = std::thread::Builder::new()
            .name("bl-output".into())
            .spawn(move || run(d, password, name, output, rate, c, s, r))
            .ok();
        if thread.is_none() {
            return;
        }
        self.outputs.active = Some(Active {
            device,
            stop,
            control,
            thread,
            connected: false,
        });
        let _ = host;
    }

    /// Delivers results and keeps the speaker informed (every tick).
    pub(crate) fn poll_outputs(&mut self, host: &mut dyn Host) {
        for found in self.outputs.found.take() {
            self.outputs.scanning = false;
            // Keep the active speaker listed even when it did not answer.
            let mut list = found;
            if let Some(a) = &self.outputs.active {
                if !list.iter().any(|d| d.id == a.device.id) {
                    list.push(a.device.clone());
                }
            }
            list.sort_by_key(|d| d.name.to_lowercase());
            self.outputs.devices = list;
        }
        for r in self.outputs.reports.take() {
            match r {
                Report::Connected => {
                    if let Some(a) = self.outputs.active.as_mut() {
                        a.connected = true;
                    }
                }
                Report::Failed(e) => {
                    let name = self
                        .outputs
                        .active
                        .as_ref()
                        .map(|a| a.device.name.clone())
                        .unwrap_or_default();
                    self.outputs.active = None;
                    self.player.output().set_external(false);
                    let msg = if e == "password" {
                        self.tr("Wrong or missing password for the speaker")
                            .to_string()
                    } else {
                        format!("{name}: {e}")
                    };
                    self.toast(msg.clone(), true);
                    self.outputs.error = Some(msg);
                }
            }
        }
        let Some(active) = self.outputs.active.as_ref().filter(|a| a.connected) else {
            return;
        };
        let control = active.control.clone();
        let volume = if self.settings.muted {
            0.0
        } else {
            self.settings.volume
        };
        let mut c = control.lock().unwrap_or_else(|e| e.into_inner());
        if self.outputs.sent_volume != Some(volume) {
            self.outputs.sent_volume = Some(volume);
            c.volume = Some(volume);
        }
        let key = self.current.as_ref().map(|t| t.id.clone());
        if key != self.outputs.sent_track {
            self.outputs.sent_track = key;
            if let Some(t) = &self.current {
                c.metadata = Some((t.title.clone(), t.artists.join(", "), t.album.clone()));
                c.progress = Some((0.0, t.duration_ms as f64 / 1000.0));
                if let Some(path) = crate::library::local_path(&t.id) {
                    c.artwork = crate::core::local_cover_bytes(path);
                }
            }
        }
        let _ = host;
    }

    pub(crate) fn outputs_json(&self) -> Value {
        let active = self.outputs.active.as_ref();
        Value::object()
            .with("scanning", self.outputs.scanning)
            .with(
                "active",
                active.map_or("local".to_string(), |a| a.device.id.clone()),
            )
            .with("activeName", active.map(|a| a.device.name.clone()))
            .with(
                "status",
                match active {
                    None => "local",
                    Some(a) if a.connected => "playing",
                    Some(_) => "connecting",
                },
            )
            .with("error", self.outputs.error.clone())
            .with(
                "devices",
                Value::Array(
                    self.outputs
                        .devices
                        .iter()
                        .map(|d| {
                            Value::object()
                                .with("id", d.id.as_str())
                                .with("name", d.name.as_str())
                                .with("kind", "airplay")
                                .with("model", d.model.as_str())
                                .with("needsPassword", d.needs_password)
                        })
                        .collect(),
                ),
            )
    }

    pub(crate) fn outputs_command(&mut self, host: &mut dyn Host, name: &str, c: &Value) -> Value {
        match name {
            "outputs.scan" => self.outputs_scan(host),
            "outputs.select" => {
                self.outputs_select(host, c.str_or("id", ""), c.str_or("password", ""))
            }
            _ => return Value::object().with("error", "unknown command"),
        }
        Value::object().with("ok", true)
    }
}
