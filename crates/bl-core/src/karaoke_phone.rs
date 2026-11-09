//! Phones as wireless microphones for the karaoke mode, through the
//! Beatlumen server (`/api/mic`, see the server's `mic_relay.rs`):
//!
//! - **Sending** (`karaoke.send {target, stream}`): the phone reads its own
//!   microphone stream, encodes 20 ms Opus packets (`bl-opus`), stamps each
//!   with the server's clock at the moment it was recorded and posts them
//!   about ten times a second to the device that plays.
//! - **Receiving** (`karaoke.receive {on}`, while the karaoke page is
//!   open): the playing device fetches the packets with long polling,
//!   decodes them and hands them to an input "phone:<device>" of its own
//!   (`mic::push_pcm`) with the time they were recorded, on its own clock.
//!   Points go by those times, not by when the packets arrive.
//!
//! The clocks are matched up with the server by round trips (the shortest
//! of the last ones counts: half of it is the uncertainty).

use crate::core::{Core, Req};
use crate::host::Host;
use crate::mic;
use bl_json::Value;

/// Bits per second of the singing (mono, 48 kHz).
const BITRATE: u32 = 64_000;
/// Samples in an Opus packet (20 ms at 48 kHz).
const FRAME: usize = 960;
/// Packets are sent at least this often (seconds) …
const SEND_EVERY: f64 = 0.1;
/// … and at most this many wait for a slow connection.
const MAX_WAITING: usize = 100;
/// A phone is offered as a microphone this long after its last packet.
const PHONE_SHOWN_SECS: f64 = 15.0;
/// The receiver asks for the server's clock this often (seconds).
const CLOCK_EVERY: f64 = 5.0;

/// What a request to `/api/mic` was for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum PhoneReq {
    /// A clock request sent at this time (µs on `mic::clock_us`), by the
    /// phone or by the receiving device.
    Time(i64),
    ReceiverTime(i64),
    /// Packets sent at this time.
    Post(i64),
    /// A fetch of the packets for this device (receiving generation).
    Fetch(u64),
}

/// This device's clock against the server's: the offset (server µs minus
/// `mic::clock_us`) from the round trip that took least time lately.
#[derive(Debug, Clone, Default)]
pub(crate) struct Clock {
    offset_us: Option<f64>,
    rtt_us: f64,
    /// When the best round trip was (µs), to let an old one go.
    at_us: i64,
}

impl Clock {
    /// Takes a round trip: sent and answered at these local times, the
    /// server's clock in the answer.
    pub(crate) fn take(&mut self, sent_us: i64, back_us: i64, server_us: f64) {
        let rtt = (back_us - sent_us).max(0) as f64;
        let offset = server_us - (sent_us + back_us) as f64 / 2.0;
        // A shorter round trip, or the best one is old (the clocks drift).
        let stale = back_us - self.at_us > 20_000_000;
        if self.offset_us.is_none() || rtt <= self.rtt_us * 1.2 || stale {
            self.offset_us = Some(match self.offset_us {
                Some(o) if !stale && rtt > self.rtt_us => o * 0.7 + offset * 0.3,
                _ => offset,
            });
            self.rtt_us = rtt;
            self.at_us = back_us;
        }
    }

    pub(crate) fn offset(&self) -> Option<f64> {
        self.offset_us
    }
}

/// Opus packets with the server time they were recorded at, as posted.
pub(crate) fn pack(packets: &[(u64, Vec<u8>)]) -> Vec<u8> {
    let mut out = Vec::new();
    for (at, p) in packets {
        out.extend_from_slice(&at.to_le_bytes());
        out.extend_from_slice(&(p.len() as u16).to_le_bytes());
        out.extend_from_slice(p);
    }
    out
}

/// What a fetch brings: (seq, recorded at, sender, packet).
pub(crate) fn unpack(body: &[u8]) -> Vec<(u64, u64, String, Vec<u8>)> {
    let mut out = Vec::new();
    let mut at = 0;
    while body.len() >= at + 17 {
        let seq = u64::from_le_bytes(body[at..at + 8].try_into().expect("8 bytes"));
        let ts = u64::from_le_bytes(body[at + 8..at + 16].try_into().expect("8 bytes"));
        let n = body[at + 16] as usize;
        at += 17;
        if body.len() < at + n + 2 {
            break;
        }
        let from = String::from_utf8_lossy(&body[at..at + n]).into_owned();
        at += n;
        let len = u16::from_le_bytes([body[at], body[at + 1]]) as usize;
        at += 2;
        if body.len() < at + len {
            break;
        }
        out.push((seq, ts, from, body[at..at + len].to_vec()));
        at += len;
    }
    out
}

/// The phone's own microphone on its way to the device that plays.
pub(crate) struct Sender {
    target: String,
    stream: u32,
    next: u64,
    rate: u32,
    resampler: Option<bl_media::Resampler>,
    /// Samples at 48 kHz not yet encoded, and the time (µs on
    /// `mic::clock_us`) of the first.
    pcm: Vec<f32>,
    pcm_t0: Option<f64>,
    encoder: bl_opus::OpusEncoder,
    /// Packets waiting to be sent, stamped with the server's clock.
    waiting: Vec<(u64, Vec<u8>)>,
    in_flight: bool,
    last_post: f64,
    clock: Clock,
    clock_asked: bool,
    sent: u64,
    error: Option<String>,
}

/// A phone sending to this device.
struct Phone {
    id: String,
    last_seen: f64,
    decoder: Option<bl_opus::OpusDecoder>,
    /// When the last packet was recorded (server µs), to start again after
    /// a gap.
    last_at: u64,
}

#[derive(Default)]
pub(crate) struct PhoneState {
    pub sender: Option<Sender>,
    receiving: bool,
    fetching: bool,
    generation: u64,
    after: u64,
    phones: Vec<Phone>,
    clock: Clock,
    clock_asked: bool,
    next_clock: f64,
    /// The phones offered as inputs now: (input id, name).
    pub inputs: Vec<(String, String)>,
}

fn opus_head() -> bl_opus::OpusHead {
    bl_opus::OpusHead {
        channels: 1,
        pre_skip: 0,
        input_sample_rate: 48000,
        output_gain: 0,
        mapping_family: 0,
        stream_count: 1,
        coupled_count: 0,
        mapping: vec![0],
    }
}

impl Core {
    /// `karaoke.send {target, stream}` / `karaoke.sendStop` /
    /// `karaoke.receive {on}`.
    pub(crate) fn karaoke_phone_command(
        &mut self,
        host: &mut dyn Host,
        name: &str,
        c: &Value,
    ) -> Value {
        match name {
            "karaoke.send" => {
                let target = c.str_or("target", "").to_string();
                let Some(stream) = c.get("stream").as_u64().map(|s| s as u32) else {
                    return Value::object().with("error", "no stream");
                };
                if target.is_empty() || !self.server.is_connected() {
                    return Value::object().with("error", self.tr("Not connected to the server"));
                }
                let encoder = match bl_opus::OpusEncoder::new(1, BITRATE) {
                    Ok(e) => e,
                    Err(e) => return Value::object().with("error", e.to_string()),
                };
                self.phones.sender = Some(Sender {
                    target,
                    stream,
                    next: mic::latest(stream, 0).map_or(0, |b| b.index),
                    rate: 0,
                    resampler: None,
                    pcm: Vec::new(),
                    pcm_t0: None,
                    encoder,
                    waiting: Vec::new(),
                    in_flight: false,
                    last_post: 0.0,
                    clock: Clock::default(),
                    clock_asked: false,
                    sent: 0,
                    error: None,
                });
                let _ = host;
                Value::object().with("ok", true)
            }
            "karaoke.sendStop" => {
                self.phones.sender = None;
                Value::object().with("ok", true)
            }
            "karaoke.receive" => {
                let on = c.bool_or("on", true);
                if on != self.phones.receiving {
                    self.phones.receiving = on;
                    self.phones.generation += 1;
                    self.phones.fetching = false;
                    if !on {
                        self.phones.phones.clear();
                    }
                }
                Value::object().with("ok", true)
            }
            "karaoke.sending" => self.karaoke_sending_json(),
            _ => Value::object().with("error", "unknown command"),
        }
    }

    pub(crate) fn karaoke_sending_json(&self) -> Value {
        match &self.phones.sender {
            None => Value::object().with("sending", false),
            Some(s) => Value::object()
                .with("sending", true)
                .with("target", s.target.as_str())
                .with("stream", s.stream as u64)
                .with("packets", s.sent)
                .with("clock", s.clock.offset().is_some())
                .with("roundTripMs", (s.clock.rtt_us / 1000.0).round())
                .with("error", s.error.clone()),
        }
    }

    /// The phones that sent lately, as microphones of this device: (input
    /// id, name).
    pub(crate) fn karaoke_phone_inputs(&self, host: &dyn Host) -> Vec<(String, String)> {
        let now = host.now();
        self.phones
            .phones
            .iter()
            .filter(|p| now - p.last_seen < PHONE_SHOWN_SECS)
            .map(|p| {
                let name = self
                    .devices
                    .list
                    .iter()
                    .find(|d| d.str_or("id", "") == p.id)
                    .map(|d| d.str_or("name", "").to_string())
                    .filter(|n| !n.is_empty())
                    .unwrap_or_else(|| self.tr("Phone").to_string());
                (
                    format!("phone:{}", p.id),
                    format!("{name} ({})", self.tr("phone")),
                )
            })
            .collect()
    }

    /// Sends and fetches (from `tick`).
    pub(crate) fn poll_karaoke_phones(&mut self, host: &mut dyn Host) {
        self.poll_phone_sender(host);
        self.poll_phone_receiver(host);
        if !self.phones.phones.is_empty() || !self.phones.inputs.is_empty() {
            self.phones.inputs = self.karaoke_phone_inputs(host);
        }
    }

    fn poll_phone_sender(&mut self, host: &mut dyn Host) {
        let now = host.now();
        let me = self.device_id();
        let Some(s) = &mut self.phones.sender else {
            return;
        };
        // The server's clock first.
        if s.clock.offset().is_none() {
            if !s.clock_asked {
                s.clock_asked = true;
                let rid = self.device_request(host, "GET", "/api/mic/time", Vec::new(), None);
                self.requests
                    .insert(rid, Req::KaraokePhone(PhoneReq::Time(mic::clock_us())));
            }
            // Nothing is sent without it; the microphone is read on.
            let s = self.phones.sender.as_mut().expect("checked");
            if let Some(b) = mic::latest(s.stream, 0) {
                s.next = b.index;
            }
            return;
        }
        let offset = s.clock.offset().unwrap_or(0.0);
        while let Some(b) = mic::read(s.stream, s.next, 8192) {
            if b.samples.is_empty() {
                break;
            }
            if b.rate != s.rate || b.index != s.next {
                // A gap or another rate: start the packets again.
                s.rate = b.rate;
                s.resampler = (b.rate != 48000).then(|| bl_media::Resampler::new(b.rate, 48000, 1));
                s.pcm.clear();
                s.pcm_t0 = None;
            }
            s.next = b.index + b.samples.len() as u64;
            if s.pcm_t0.is_none() {
                s.pcm_t0 = Some(b.time_us);
            }
            match &mut s.resampler {
                Some(r) => r.process(&b.samples, &mut s.pcm),
                None => s.pcm.extend_from_slice(&b.samples),
            }
            while s.pcm.len() >= FRAME {
                let t0 = s.pcm_t0.unwrap_or(b.time_us);
                let mut packet = Vec::with_capacity(400);
                if s.encoder.encode(&s.pcm[..FRAME], &mut packet).is_ok() {
                    let at = (t0 + offset).max(0.0) as u64;
                    s.waiting.push((at, packet));
                }
                s.pcm.drain(..FRAME);
                s.pcm_t0 = Some(t0 + FRAME as f64 * 1e6 / 48000.0);
            }
        }
        if s.waiting.len() > MAX_WAITING {
            let drop = s.waiting.len() - MAX_WAITING;
            s.waiting.drain(..drop);
        }
        if s.in_flight || s.waiting.is_empty() || now - s.last_post < SEND_EVERY {
            return;
        }
        s.in_flight = true;
        s.last_post = now;
        let body = pack(&s.waiting);
        s.sent += s.waiting.len() as u64;
        s.waiting.clear();
        let path = format!(
            "/api/mic/{}?from={me}",
            bl_net::http::percent_encode(&s.target)
        );
        let rid = self.device_request(host, "POST", &path, body, Some("application/octet-stream"));
        self.requests
            .insert(rid, Req::KaraokePhone(PhoneReq::Post(mic::clock_us())));
    }

    fn poll_phone_receiver(&mut self, host: &mut dyn Host) {
        if !self.phones.receiving || !self.server.is_connected() {
            return;
        }
        let now = host.now();
        if !self.phones.clock_asked && now >= self.phones.next_clock {
            self.phones.clock_asked = true;
            self.phones.next_clock = now + CLOCK_EVERY;
            let rid = self.device_request(host, "GET", "/api/mic/time", Vec::new(), None);
            self.requests.insert(
                rid,
                Req::KaraokePhone(PhoneReq::ReceiverTime(mic::clock_us())),
            );
        }
        if self.phones.fetching {
            return;
        }
        self.phones.fetching = true;
        let me = self.device_id();
        let path = format!("/api/mic/{me}?after={}", self.phones.after);
        let rid = self.device_request(host, "GET", &path, Vec::new(), None);
        self.requests.insert(
            rid,
            Req::KaraokePhone(PhoneReq::Fetch(self.phones.generation)),
        );
    }

    /// Answers of `/api/mic`.
    pub(crate) fn karaoke_phone_response(
        &mut self,
        host: &mut dyn Host,
        req: PhoneReq,
        res: &Result<crate::host::HttpResponse, String>,
    ) {
        let back = mic::clock_us();
        let ok = |res: &Result<crate::host::HttpResponse, String>| match res {
            Ok(r) if (200..300).contains(&r.status) => Some(r.body.clone()),
            _ => None,
        };
        match req {
            PhoneReq::ReceiverTime(sent) => {
                self.phones.clock_asked = false;
                let now = ok(res)
                    .and_then(|b| bl_json::parse_bytes(&b).ok())
                    .and_then(|v| v.get("now").as_f64());
                if let Some(n) = now {
                    self.phones.clock.take(sent, back, n);
                }
            }
            PhoneReq::Time(sent) | PhoneReq::Post(sent) => {
                let body = ok(res);
                let now = body
                    .as_deref()
                    .and_then(|b| bl_json::parse_bytes(b).ok())
                    .and_then(|v| v.get("now").as_f64());
                let Some(s) = &mut self.phones.sender else {
                    return;
                };
                if matches!(req, PhoneReq::Post(_)) {
                    s.in_flight = false;
                } else {
                    s.clock_asked = false;
                }
                match (now, res) {
                    (Some(n), _) => {
                        s.clock.take(sent, back, n);
                        s.error = None;
                    }
                    (None, Ok(r)) => s.error = Some(format!("HTTP {}", r.status)),
                    (None, Err(e)) => s.error = Some(e.clone()),
                }
            }
            PhoneReq::Fetch(generation) => {
                if generation != self.phones.generation {
                    return;
                }
                self.phones.fetching = false;
                let Some(body) = ok(res) else {
                    return;
                };
                if let Ok(r) = res {
                    if let Some(seq) = r
                        .headers
                        .iter()
                        .find(|(k, _)| k.eq_ignore_ascii_case("x-mic-seq"))
                        .and_then(|(_, v)| v.parse::<u64>().ok())
                    {
                        self.phones.after = seq;
                    }
                }
                self.karaoke_phone_packets(host, &body);
            }
        }
    }

    /// Decodes fetched packets into the phones' inputs.
    fn karaoke_phone_packets(&mut self, host: &mut dyn Host, body: &[u8]) {
        let now = host.now();
        let offset = self.phones.clock.offset();
        let streams: Vec<(String, u32)> = self.mic_phone_streams();
        for (seq, at, from, packet) in unpack(body) {
            self.phones.after = self.phones.after.max(seq);
            if !self.phones.phones.iter().any(|p| p.id == from) {
                if self.phones.phones.len() >= 8 {
                    continue;
                }
                self.phones.phones.push(Phone {
                    id: from.clone(),
                    last_seen: now,
                    decoder: None,
                    last_at: 0,
                });
                // A new phone among the microphones.
                self.karaoke.rev += 1;
            }
            let phone = self
                .phones
                .phones
                .iter_mut()
                .find(|p| p.id == from)
                .expect("added");
            phone.last_seen = now;
            let input = format!("phone:{from}");
            let Some(&(_, stream)) = streams.iter().find(|(d, _)| *d == input) else {
                continue;
            };
            let Some(offset) = offset else {
                continue;
            };
            // After a gap the decoder starts fresh.
            if phone.decoder.is_none() || at > phone.last_at + 200_000 || at < phone.last_at {
                phone.decoder = bl_opus::OpusDecoder::new(&opus_head(), 48000).ok();
            }
            phone.last_at = at;
            let Some(d) = &mut phone.decoder else {
                continue;
            };
            let mut pcm = Vec::with_capacity(FRAME);
            if d.decode_packet(&packet, &mut pcm).is_err() {
                continue;
            }
            let pcm16: Vec<i16> = pcm
                .iter()
                .map(|v| (v.clamp(-1.0, 1.0) * 32767.0) as i16)
                .collect();
            // The encoder's look-ahead delays what comes out.
            let lookahead = bl_opus::OpusEncoder::LOOKAHEAD as f64 * 1e6 / 48000.0;
            let local = (at as f64 - offset - lookahead).round() as i64;
            if local > 0 {
                mic::push_pcm(stream, &pcm16, 1, 48000, local);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clocks_take_the_shortest_round_trip() {
        let mut c = Clock::default();
        // Server 1 s ahead; a slow trip first (asymmetric), then a quick one.
        c.take(1_000_000, 1_300_000, 2_250_000.0);
        assert_eq!(c.offset(), Some(1_100_000.0));
        c.take(2_000_000, 2_010_000, 3_005_000.0);
        assert_eq!(c.offset(), Some(1_000_000.0));
        // A slower one later moves it a little at most.
        c.take(3_000_000, 3_200_000, 4_200_000.0);
        assert_eq!(c.offset(), Some(1_000_000.0));
    }

    #[test]
    fn packets_go_and_come() {
        let p = vec![(5u64, vec![1u8, 2, 3]), (25, vec![4])];
        let posted = pack(&p);
        assert_eq!(posted.len(), 10 + 3 + 10 + 1);
        // As the server hands them on (seq, at, sender, packet).
        let mut fetched = Vec::new();
        for (i, (at, data)) in p.iter().enumerate() {
            fetched.extend_from_slice(&(i as u64 + 1).to_le_bytes());
            fetched.extend_from_slice(&at.to_le_bytes());
            fetched.push(5);
            fetched.extend_from_slice(b"phone");
            fetched.extend_from_slice(&(data.len() as u16).to_le_bytes());
            fetched.extend_from_slice(data);
        }
        let got = unpack(&fetched);
        assert_eq!(got.len(), 2);
        assert_eq!(got[1], (2, 25, "phone".to_string(), vec![4]));
    }
}
