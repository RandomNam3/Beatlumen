//! Where the music plays: this device, or a speaker in the network
//! (AirPlay 1 receivers, DLNA/UPnP renderers). A network output takes the
//! player's audio instead of the sound card: a thread renders it at the
//! speaker's pace, resamples it to 44.1 kHz and streams it. The visuals
//! stay in sync by reporting the speaker's delay as the output latency.

mod dlna;
mod stream;

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

/// How far ahead of the speaker's clock AirPlay audio is sent (frames).
const LEAD: u32 = SAMPLE_RATE / 4;

#[derive(Clone, Debug, PartialEq)]
enum Kind {
    AirPlay {
        addr: SocketAddr,
        /// Supported encryptions ("et").
        et: String,
    },
    Dlna(Box<bl_upnp::Renderer>),
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Device {
    /// "airplay:<address>:<port>" or "dlna:<udn>"
    pub id: String,
    pub name: String,
    pub model: String,
    kind: Kind,
    pub needs_password: bool,
}

impl Device {
    fn kind_name(&self) -> &'static str {
        match self.kind {
            Kind::AirPlay { .. } => "airplay",
            Kind::Dlna(_) => "dlna",
        }
    }
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

/// What an output thread works with.
struct Job {
    output: Arc<Output>,
    device_rate: u32,
    control: Arc<Mutex<Control>>,
    stop: Arc<AtomicBool>,
    reports: Mailbox<Report>,
}

impl Job {
    fn take_control(&self) -> Control {
        std::mem::take(&mut *self.control.lock().unwrap_or_else(|e| e.into_inner()))
    }

    fn stopped(&self) -> bool {
        self.stop.load(Ordering::Relaxed)
    }
}

/// Renders the player's audio as 44.1 kHz 16-bit stereo.
struct Pump {
    resampler: Option<bl_media::Resampler>,
    device_rate: u32,
    render: Vec<f32>,
    resampled: Vec<f32>,
    pcm: Vec<i16>,
    resets: u64,
}

impl Pump {
    fn new(job: &Job) -> Pump {
        Pump {
            resampler: (job.device_rate != SAMPLE_RATE)
                .then(|| bl_media::Resampler::new(job.device_rate, SAMPLE_RATE, 2)),
            device_rate: job.device_rate,
            render: Vec::new(),
            resampled: Vec::new(),
            pcm: Vec::new(),
            resets: job.output.resets(),
        }
    }

    /// True once after a seek or a new track.
    fn reset_seen(&mut self, output: &Output) -> bool {
        let r = output.resets();
        if r == self.resets {
            return false;
        }
        self.resets = r;
        if let Some(rs) = self.resampler.as_mut() {
            rs.reset();
        }
        true
    }

    /// About `frames` frames (44.1 kHz); `latency` is the delay until they
    /// are heard, in seconds.
    fn take(&mut self, output: &Output, frames: usize, latency: f64) -> &[i16] {
        let dev = (frames as u64 * self.device_rate as u64).div_ceil(SAMPLE_RATE as u64) as usize;
        self.render.resize(dev * 2, 0.0);
        output.render_external(&mut self.render, (latency * self.device_rate as f64) as u32);
        let samples: &[f32] = match self.resampler.as_mut() {
            Some(rs) => {
                self.resampled.clear();
                rs.process(&self.render, &mut self.resampled);
                &self.resampled
            }
            None => &self.render,
        };
        self.pcm.clear();
        self.pcm.extend(
            samples
                .iter()
                .map(|v| (v.clamp(-1.0, 1.0) * 32767.0) as i16),
        );
        &self.pcm
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

fn airplay_device(f: &bl_airplay::mdns::Found) -> Device {
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
        kind: Kind::AirPlay {
            addr: SocketAddr::new(f.addr.into(), f.port),
            et: f.txt_value("et").unwrap_or("0").to_string(),
        },
        needs_password: f
            .txt_value("pw")
            .is_some_and(|v| v.eq_ignore_ascii_case("true")),
    }
}

fn dlna_device(r: bl_upnp::Renderer) -> Device {
    Device {
        id: format!("dlna:{}", r.udn),
        name: r.name.clone(),
        model: [r.manufacturer.as_str(), r.model.as_str()]
            .iter()
            .filter(|s| !s.is_empty())
            .copied()
            .collect::<Vec<_>>()
            .join(" "),
        kind: Kind::Dlna(Box::new(r)),
        needs_password: false,
    }
}

/// A speaker given by address: "airplay:<ip>:<port>" or
/// "dlna:<description URL>".
fn device_from_id(id: &str) -> Option<Device> {
    if let Some(url) = id.strip_prefix("dlna:").filter(|u| u.starts_with("http")) {
        return bl_upnp::Renderer::fetch(url).ok().map(dlna_device);
    }
    let addr: SocketAddr = id.strip_prefix("airplay:")?.parse().ok()?;
    Some(Device {
        id: id.to_string(),
        name: addr.ip().to_string(),
        model: String::new(),
        kind: Kind::AirPlay {
            addr,
            et: "0,1".into(),
        },
        needs_password: false,
    })
}

/// The AirPlay output thread: connects, then streams what the player renders.
fn run_airplay(addr: SocketAddr, et: String, password: Option<String>, job: Job) {
    let opts = Options {
        password,
        encrypt: true,
        name: "Beatlumen".into(),
    };
    let mut tx = match Sender::connect(addr, &et, &opts) {
        Ok(tx) => tx,
        Err(e) => {
            let msg = match e {
                sender::Error::Auth => "password".to_string(),
                e => e.to_string(),
            };
            job.reports.post(Report::Failed(msg));
            return;
        }
    };
    job.reports.post(Report::Connected);
    job.output.set_external(true);
    let mut pump = Pump::new(&job);
    // Heard this much later than rendered.
    let latency = (sender::LATENCY + LEAD / 2) as f64 / SAMPLE_RATE as f64;
    let mut flushed = false;
    let mut failed = None;
    while !job.stopped() {
        let pending = job.take_control();
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
            if pump.reset_seen(&job.output) && !flushed {
                tx.flush()?;
            }
            if job.output.is_silent() {
                if !flushed {
                    tx.flush()?;
                    flushed = true;
                }
                return tx.keepalive();
            }
            flushed = false;
            let due = tx.frames_due(LEAD);
            if due > 0 {
                let pcm = pump.take(&job.output, due, latency);
                tx.send(pcm)?;
            }
            tx.keepalive()
        })();
        if let Err(e) = result {
            failed = Some(e.to_string());
            break;
        }
        std::thread::sleep(Duration::from_millis(if flushed { 50 } else { 8 }));
    }
    job.output.set_external(false);
    if let Some(e) = failed {
        job.reports.post(Report::Failed(e));
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
            let wait = Duration::from_secs(3);
            let dlna = std::thread::spawn(move || {
                let answers = bl_upnp::ssdp::discover(bl_upnp::ssdp::MEDIA_RENDERER, wait)
                    .unwrap_or_default();
                answers
                    .iter()
                    .filter_map(|a| bl_upnp::Renderer::fetch(&a.location).ok())
                    .map(dlna_device)
                    .collect::<Vec<_>>()
            });
            let mut found: Vec<Device> = bl_airplay::mdns::browse("_raop._tcp", wait)
                .unwrap_or_default()
                .iter()
                .filter(|f| Some(&f.instance) != own.as_ref())
                .map(airplay_device)
                .collect();
            found.extend(dlna.join().unwrap_or_default());
            mailbox.post(found);
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
        let job = Job {
            output: self.player.output(),
            device_rate: self.player.device_rate(),
            control: control.clone(),
            stop: stop.clone(),
            reports: self.outputs.reports.clone(),
        };
        let password = Some(password.to_string()).filter(|p| !p.is_empty());
        let kind = device.kind.clone();
        let thread = std::thread::Builder::new()
            .name("bl-output".into())
            .spawn(move || match kind {
                Kind::AirPlay { addr, et } => run_airplay(addr, et, password, job),
                Kind::Dlna(r) => dlna::run(*r, job),
            })
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
                                .with("kind", d.kind_name())
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
