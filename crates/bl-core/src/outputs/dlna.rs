//! Playing on a DLNA/UPnP renderer: the renderer fetches a live stream of
//! the playback from this device ([`super::stream`]) and is told to play,
//! pause and change its volume over UPnP. Its delay is measured from the
//! position it reports, so the visuals follow what is heard.

use super::stream::{local_ip_towards, Format, LiveStream, RATE};
use super::{Job, Pump, Report};
use bl_upnp::Renderer;
use std::net::{IpAddr, ToSocketAddrs};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Audio produced ahead of real time (frames); a new connection also gets
/// this much at once, so the renderer starts quickly.
const LEAD: u64 = RATE as u64 / 2;
/// Assumed delay until the renderer reports its position (seconds).
const DEFAULT_DELAY: f64 = 2.5;

fn renderer_ip(r: &Renderer) -> Option<IpAddr> {
    let host = r.host()?;
    host.parse().ok().or_else(|| {
        (host.as_str(), 80)
            .to_socket_addrs()
            .ok()?
            .next()
            .map(|a| a.ip())
    })
}

/// The format the renderer takes: WAV where listed, else raw L16.
fn pick_format(sinks: &[String]) -> Format {
    let has = |m: &str| sinks.iter().any(|s| s.to_ascii_lowercase().contains(m));
    if has("audio/wav")
        || has("audio/x-wav")
        || has("audio/wave")
        || sinks.is_empty()
        || has(":*:*")
    {
        Format::Wav
    } else if has("audio/l16") {
        Format::L16
    } else {
        Format::Wav
    }
}

pub(super) fn run(renderer: Renderer, job: Job) {
    let fail = |e: String| job.reports.post(Report::Failed(e));
    let Some(ip) = renderer_ip(&renderer).and_then(local_ip_towards) else {
        return fail("no network path to the renderer".into());
    };
    let stream = match LiveStream::start(LEAD) {
        Ok(s) => Arc::new(s),
        Err(e) => return fail(e.to_string()),
    };
    let format = pick_format(&renderer.sink_protocols().unwrap_or_default());
    let url = stream.url(ip, format);
    let info = format!("http-get:*:{}:*", format.mime());
    let didl = bl_upnp::didl("Beatlumen", "", &url, &info);
    // Some renderers refuse a new stream while playing another one.
    let _ = renderer.stop();
    if let Err(e) = renderer.set_uri(&url, &didl).and_then(|_| renderer.play()) {
        return fail(e);
    }
    job.reports.post(Report::Connected);
    job.output.set_external(true);

    // The renderer's delay, measured in the background (its answers may be
    // slow and must not hold up the audio).
    let delay = Arc::new(AtomicU64::new(DEFAULT_DELAY.to_bits()));
    let measuring = Arc::new(AtomicBool::new(true));
    let paused = Arc::new(AtomicBool::new(false));
    let meter = {
        let (r, s, d, m, p) = (
            renderer.clone(),
            stream.clone(),
            delay.clone(),
            measuring.clone(),
            paused.clone(),
        );
        std::thread::Builder::new()
            .name("bl-dlna-position".into())
            .spawn(move || measure(&r, &s, &d, &m, &p))
            .ok()
    };

    let mut pump = Pump::new(&job);
    let mut produced: u64 = 0;
    // Real time at `anchor.0` corresponds to frame `anchor.1`.
    let mut anchor = (Instant::now(), 0u64);
    let mut silent = false;
    while !job.stopped() {
        let pending = job.take_control();
        if let Some(v) = pending.volume {
            let _ = renderer.set_volume((v.clamp(0.0, 1.0) * 100.0).round() as u32);
        }
        pump.reset_seen(&job.output);
        if job.output.is_silent() {
            if !silent {
                silent = true;
                paused.store(true, Ordering::Relaxed);
                let _ = renderer.pause();
            }
            std::thread::sleep(Duration::from_millis(50));
            continue;
        }
        if silent {
            silent = false;
            let _ = renderer.play();
            paused.store(false, Ordering::Relaxed);
            anchor = (Instant::now(), produced.saturating_sub(LEAD));
        }
        let now = anchor.1 + (anchor.0.elapsed().as_secs_f64() * RATE as f64) as u64;
        let due = (now + LEAD).saturating_sub(produced);
        if due >= 441 {
            // The newest frame is heard after the measured delay.
            let latency = f64::from_bits(delay.load(Ordering::Relaxed));
            let pcm = pump.take(&job.output, due as usize, latency);
            produced += pcm.len() as u64 / 2;
            stream.push(pcm);
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    measuring.store(false, Ordering::Relaxed);
    if let Some(t) = meter {
        let _ = t.join();
    }
    let _ = renderer.stop();
    job.output.set_external(false);
}

/// Delay = audio produced − (where the renderer's connection started +
/// what it says it has played), smoothed. Many renderers report whole
/// seconds only: then only the moments the second changes are used.
fn measure(
    r: &Renderer,
    s: &LiveStream,
    delay: &AtomicU64,
    running: &AtomicBool,
    paused: &AtomicBool,
) {
    let mut smoothed: Option<f64> = None;
    let mut last_value: Option<f64> = None;
    while running.load(Ordering::Relaxed) {
        std::thread::sleep(Duration::from_millis(250));
        if paused.load(Ordering::Relaxed) {
            last_value = None;
            continue;
        }
        let asked = Instant::now();
        let before = s.progress().0;
        let Ok(Some(reported)) = r.position() else {
            continue;
        };
        let (end, Some(from)) = s.progress() else {
            continue;
        };
        // Answers that took long would make the measure imprecise.
        if asked.elapsed() > Duration::from_millis(200) || reported <= 0.5 {
            continue;
        }
        let whole = reported.fract() == 0.0;
        let changed = last_value.is_some_and(|v| v != reported);
        last_value = Some(reported);
        let played = if !whole {
            reported
        } else if changed {
            // The second just began (within the last poll interval).
            reported + 0.125
        } else {
            continue;
        };
        let produced = (before + end) as f64 / 2.0;
        let d = (produced - from as f64) / RATE as f64 - played;
        if !(0.0..30.0).contains(&d) {
            continue;
        }
        let v = match smoothed {
            Some(p) => p + (d - p) * 0.3,
            None => d,
        };
        smoothed = Some(v);
        delay.store(v.to_bits(), Ordering::Relaxed);
        if std::env::var_os("BL_DEBUG_DLNA").is_some() {
            eprintln!(
                "dlna: produced {:.2} s, renderer played {played:.2} s, delay {d:.2} -> {v:.2}",
                (produced - from as f64) / RATE as f64
            );
        }
    }
}
