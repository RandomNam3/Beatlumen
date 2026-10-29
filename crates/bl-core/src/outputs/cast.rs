//! Playing on a Chromecast (or a TV or speaker with Chromecast built in):
//! the device runs Google's Default Media Receiver, which fetches the live
//! stream of the playback from this device ([`super::stream`]). Pause,
//! play and volume go over the Cast connection; the device's delay is
//! measured from the play time it reports.

use super::stream::{live_loop, local_ip_towards, Format, LiveStream, LiveTarget, LEAD, RATE};
use super::{Job, Report};
use bl_cast::{Client, DEFAULT_MEDIA_RECEIVER};
use bl_net::client::Conn;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};

const DEFAULT_DELAY: f64 = 3.0;
/// Reads wait this long, so heartbeats and commands are not held up.
const READ_TIMEOUT: Duration = Duration::from_millis(60);

/// TLS to the device. Cast devices have self-signed certificates: the
/// fingerprint seen on the first attempt is accepted (nothing secret goes
/// over this connection; the audio stream is fetched separately).
fn connect(addr: SocketAddr) -> Result<Box<dyn Conn>, String> {
    let host = addr.ip().to_string();
    if !bl_net::client::tls_available() {
        // Without a TLS implementation (tests): plain TCP.
        let s = std::net::TcpStream::connect_timeout(&addr, Duration::from_secs(5))
            .map_err(|e| e.to_string())?;
        s.set_read_timeout(Some(READ_TIMEOUT))
            .map_err(|e| e.to_string())?;
        return Ok(Box::new(s));
    }
    let open = |pin: Option<&str>| {
        bl_net::client::tls_connect(
            &host,
            addr.port(),
            Duration::from_secs(5),
            READ_TIMEOUT,
            pin,
        )
    };
    match open(None) {
        Ok(c) => Ok(c),
        Err(e) => {
            let msg = e.to_string();
            let pin = msg
                .split_once(bl_net::UNTRUSTED_CERT_MARKER)
                .and_then(|(_, rest)| rest.split(']').next())
                .filter(|p| !p.is_empty())
                .ok_or(msg.clone())?;
            open(Some(pin)).map_err(|e| e.to_string())
        }
    }
}

/// Polls until `done` holds, the device reports an error or `secs` pass.
fn wait<S: std::io::Read + std::io::Write>(
    client: &mut Client<S>,
    job: &Job,
    secs: u64,
    what: &str,
    done: impl Fn(&Client<S>) -> bool,
) -> Result<(), String> {
    let start = Instant::now();
    while !done(client) {
        if job.stopped() {
            return Err("stopped".into());
        }
        client.poll().map_err(|e| e.to_string())?;
        if let Some(e) = client.error.take() {
            return Err(e);
        }
        if start.elapsed() > Duration::from_secs(secs) {
            return Err(format!("the device did not {what}"));
        }
    }
    Ok(())
}

pub(super) fn run(addr: SocketAddr, job: Job) {
    let result = (|| -> Result<(), String> {
        let ip = local_ip_towards(addr.ip()).ok_or("no network path to the device")?;
        let conn = connect(addr)?;
        let mut client = Client::new(conn).map_err(|e| e.to_string())?;
        client
            .launch(DEFAULT_MEDIA_RECEIVER)
            .map_err(|e| e.to_string())?;
        wait(&mut client, &job, 20, "start the media receiver", |c| {
            c.ready()
        })?;
        let stream = Arc::new(LiveStream::start(LEAD).map_err(|e| e.to_string())?);
        let url = stream.url(ip, Format::Wav);
        client
            .load(&url, Format::Wav.mime(), "Beatlumen", "")
            .map_err(|e| e.to_string())?;
        wait(&mut client, &job, 20, "start playing", |c| {
            c.media.is_some()
        })?;
        job.reports.post(Report::Connected);
        job.output.set_external(true);
        let mut target = Cast {
            client,
            stream: stream.clone(),
            delay: None,
            paused: false,
            last_status: Instant::now(),
        };
        let r = live_loop(&job, &stream, &mut target);
        let _ = target.client.stop();
        r
    })();
    job.output.set_external(false);
    if let Err(e) = result {
        if e != "stopped" {
            job.reports.post(Report::Failed(e));
        }
    }
}

struct Cast<S: std::io::Read + std::io::Write> {
    client: Client<S>,
    stream: Arc<LiveStream>,
    delay: Option<f64>,
    paused: bool,
    last_status: Instant,
}

impl<S: std::io::Read + std::io::Write> LiveTarget for Cast<S> {
    fn set_volume(&mut self, volume: f32) {
        let _ = self.client.set_volume(volume);
    }

    fn set_paused(&mut self, paused: bool) {
        self.paused = paused;
        let _ = if paused {
            self.client.pause()
        } else {
            self.client.play()
        };
    }

    fn tick(&mut self) -> Result<(), String> {
        self.client.poll().map_err(|e| e.to_string())?;
        if let Some(e) = self.client.error.take() {
            return Err(e);
        }
        if let Some(m) = &self.client.media {
            if m.state == "IDLE" && !m.idle_reason.is_empty() && m.idle_reason != "INTERRUPTED" {
                return Err(format!("the device stopped playing ({})", m.idle_reason));
            }
        }
        if self.last_status.elapsed() >= Duration::from_secs(1) {
            self.last_status = Instant::now();
            // Delay from the last answer: produced since the device's
            // connection started, minus what it says it has played.
            if let (Some(m), (end, Some(from))) = (&self.client.media, self.stream.progress()) {
                if m.state == "PLAYING" && m.time > 0.5 && !self.paused {
                    let played = m.time + m.at.elapsed().as_secs_f64();
                    let d = (end - from) as f64 / RATE as f64 - played;
                    if (0.0..30.0).contains(&d) {
                        self.delay = Some(match self.delay {
                            Some(p) => p + (d - p) * 0.3,
                            None => d,
                        });
                    }
                }
            }
            self.client
                .request_media_status()
                .map_err(|e| e.to_string())?;
        }
        Ok(())
    }

    fn delay(&self) -> f64 {
        self.delay.unwrap_or(DEFAULT_DELAY)
    }
}
