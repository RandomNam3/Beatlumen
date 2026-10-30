//! AirPlay 2 requests of a control connection: /info, pairing, the plist
//! SETUP of the event channel and of buffered (TCP) or realtime (UDP)
//! audio streams, start/pause anchors, flushes and teardown. Everything
//! but /info and pairing needs a paired (encrypted) connection; AirPlay 1
//! requests go on as before.

use super::{Connection, Event};
use crate::ap2::buffered::{alac_cookie, Buffered, Codec};
use crate::ap2::cipher::Sealer;
use crate::ap2::pairing::{PairSetup, PairVerify, Reply};
use crate::ap2::plist::{self, Value};
use crate::ap2::EventPort;
use crate::meta::{self, StreamFormat};
use crate::rtsp::{Request, Response};
use crate::session::Session;
use std::net::{SocketAddr, UdpSocket};
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

/// Delay from a start anchor until its first frame is heard (the
/// sender's PTP clock is not followed).
const START_DELAY: Duration = Duration::from_millis(250);
/// Buffer size announced to senders (how far ahead they may send).
const AUDIO_BUFFER: i128 = 8 * 1024 * 1024;
const BPLIST: &str = "application/x-apple-binary-plist";
/// Transient pairing PIN of receivers without a password.
const TRANSIENT_PIN: &str = "3939";

#[derive(Default)]
pub(super) struct State {
    setup: PairSetup,
    verify: PairVerify,
    /// Pairing completed on this connection.
    paired: bool,
    /// Secret to encrypt the connection with after the current response.
    pub(super) pending_secret: Option<Vec<u8>>,
    pub(super) sealer: Option<Sealer>,
    pub(super) events: Option<EventPort>,
    stream: Option<AudioStream>,
    /// RECORD was received.
    record: bool,
    /// Generation of the stream announced with `Event::Started`.
    started: Option<u64>,
    /// The sender's rate is 1 (playing).
    playing: bool,
}

struct AudioStream {
    kind: Kind,
    generation: u64,
    sample_rate: u32,
}

enum Kind {
    /// Type 103, with the stream's UDP control port (not used by buffered
    /// streams, but senders expect one).
    Buffered {
        buffered: Buffered,
        _control: UdpSocket,
    },
    /// Type 96: RTP over UDP, paced and synchronised by the sender.
    Realtime { _session: Session },
}

impl State {
    pub(super) fn end_stream(&mut self) {
        self.stream = None;
        self.started = None;
        self.playing = false;
    }
}

fn int(v: impl Into<i128>) -> Value {
    Value::Int(v.into())
}

fn bplist(v: &Value) -> Response {
    Response::new(200).body(BPLIST, plist::write(v))
}

impl Connection<'_> {
    /// Sample rate of the current stream (AirPlay 1 or 2).
    pub(super) fn sample_rate(&self) -> Option<u32> {
        self.format
            .as_ref()
            .map(|f| f.sample_rate)
            .or_else(|| self.ap2.stream.as_ref().map(|s| s.sample_rate))
    }

    /// Handles AirPlay 2 requests; `None` for others.
    pub(super) fn handle_ap2(
        &mut self,
        req: &Request,
        local: Option<SocketAddr>,
    ) -> Option<Response> {
        let plist_body = req.content_type().starts_with(BPLIST);
        Some(match (req.method.as_str(), req.uri.as_str()) {
            ("GET", "/info") => self.info(),
            ("POST", "/pair-setup") => {
                let pin = self
                    .shared
                    .config
                    .password
                    .as_deref()
                    .unwrap_or(TRANSIENT_PIN);
                let r = self.ap2.setup.handle(
                    &req.body,
                    pin,
                    &self.shared.identity,
                    &self.shared.pairings,
                );
                self.paired(r)
            }
            ("POST", "/pair-verify") => {
                let r =
                    self.ap2
                        .verify
                        .handle(&req.body, &self.shared.identity, &self.shared.pairings);
                self.paired(r)
            }
            // FairPlay and MFi authentication are not available.
            ("POST", "/fp-setup" | "/auth-setup") => Response::new(501),
            _ if !self.ap2.paired => return None,
            ("SETUP", _) if plist_body => self.setup_plist(req, local),
            // (Paired senders may also stream the AirPlay 1 way.)
            ("RECORD", _) if self.session.is_none() => {
                self.ap2.record = true;
                self.announce();
                Response::new(200)
            }
            ("SETRATEANCHORTIME", _) => self.anchor(req),
            ("FLUSHBUFFERED", _) => self.flush_buffered(req),
            ("SETPEERS" | "SETPEERSX", _) => Response::new(200),
            ("POST", "/command" | "/audioMode" | "/feedback") => Response::new(200),
            ("TEARDOWN", _) => self.teardown(req),
            _ => return None,
        })
    }

    fn info(&self) -> Response {
        let d = self.shared.description();
        bplist(&d.info(&self.peer.to_string(), self.volume_db))
    }

    fn paired(&mut self, r: Reply) -> Response {
        if r.failed {
            // Slows down guessing a password.
            std::thread::sleep(Duration::from_secs(1));
        }
        if let Some(secret) = r.secret {
            self.ap2.paired = true;
            self.ap2.pending_secret = Some(secret);
        }
        Response::new(200).body("application/octet-stream", r.body)
    }

    fn setup_plist(&mut self, req: &Request, local: Option<SocketAddr>) -> Response {
        let Ok(p) = plist::parse(&req.body) else {
            return Response::new(400);
        };
        if let Some(name) = p.get("name").and_then(Value::as_str) {
            self.client = name.chars().take(64).collect();
        }
        let Some(streams) = p.get("streams").and_then(Value::as_array) else {
            return self.setup_session(&p, local);
        };
        let mut out = Vec::new();
        for s in streams.iter().take(4) {
            match s.get("type").and_then(Value::as_u64) {
                Some(103) => match self.setup_buffered(s) {
                    Ok(v) => out.push(v),
                    Err(status) => return Response::new(status),
                },
                Some(96) => match self.setup_realtime(s) {
                    Ok(v) => out.push(v),
                    Err(status) => return Response::new(status),
                },
                // Remote control (130) and other streams.
                _ => return Response::new(501),
            }
        }
        bplist(&Value::dict([("streams", Value::Array(out))]))
    }

    /// The first SETUP: the event channel and timing.
    fn setup_session(&mut self, p: &Value, local: Option<SocketAddr>) -> Response {
        if self.ap2.events.is_none() {
            match EventPort::start(self.peer.ip()) {
                Ok(e) => self.ap2.events = Some(e),
                Err(_) => return Response::new(500),
            }
        }
        let port = self.ap2.events.as_ref().map_or(0, |e| e.port);
        let mut out = vec![
            ("eventPort".to_string(), int(port)),
            ("timingPort".to_string(), int(0)),
        ];
        if p.get("timingPeerInfo").is_some() {
            let ip = local.map(|l| l.ip().to_string()).unwrap_or_default();
            out.push((
                "timingPeerInfo".to_string(),
                Value::dict([
                    ("Addresses", Value::Array(vec![Value::String(ip.clone())])),
                    ("ID", Value::String(ip)),
                ]),
            ));
        }
        bplist(&Value::Dict(out))
    }

    fn setup_buffered(&mut self, s: &Value) -> Result<Value, u16> {
        let key: [u8; 32] = s
            .get("shk")
            .and_then(Value::as_data)
            .and_then(|d| d.try_into().ok())
            .ok_or(400u16)?;
        let rate = s.get("sr").and_then(Value::as_u64).unwrap_or(44100) as u32;
        let codec = s
            .get("audioFormat")
            .and_then(Value::as_u64)
            .and_then(Codec::from_audio_format)
            .or(match s.get("ct").and_then(Value::as_u64) {
                Some(2) => Some(Codec::Alac {
                    rate,
                    bits: 16,
                    channels: 2,
                }),
                _ => None,
            })
            .unwrap_or(Codec::AacLc { rate, channels: 2 });
        let stream = &self.shared.stream;
        // Take over from any previous sender.
        self.shared.active.store(self.id, Ordering::SeqCst);
        self.ap2.end_stream();
        let generation = stream.begin(codec.format());
        stream.hold();
        let peer = self.peer.ip();
        let buffered =
            Buffered::start(key, codec, stream.clone(), generation, peer).map_err(|_| 500u16)?;
        let control = UdpSocket::bind(if peer.is_ipv6() {
            "[::]:0"
        } else {
            "0.0.0.0:0"
        })
        .map_err(|_| 500u16)?;
        let control_port = control.local_addr().map_err(|_| 500u16)?.port();
        let reply = Value::dict([
            ("type", int(103)),
            ("dataPort", int(buffered.port)),
            ("controlPort", int(control_port)),
            ("audioBufferSize", Value::Int(AUDIO_BUFFER)),
        ]);
        self.ap2.stream = Some(AudioStream {
            kind: Kind::Buffered {
                buffered,
                _control: control,
            },
            generation,
            sample_rate: codec.format().sample_rate,
        });
        self.announce();
        Ok(reply)
    }

    /// A realtime stream: ALAC over UDP like AirPlay 1, encrypted with
    /// ChaCha20-Poly1305.
    fn setup_realtime(&mut self, s: &Value) -> Result<Value, u16> {
        let key: [u8; 32] = s
            .get("shk")
            .and_then(Value::as_data)
            .and_then(|d| d.try_into().ok())
            .ok_or(400u16)?;
        let rate = s.get("sr").and_then(Value::as_u64).unwrap_or(44100) as u32;
        let spf = s.get("spf").and_then(Value::as_u64).unwrap_or(352) as u32;
        if s.get("ct").and_then(Value::as_u64).unwrap_or(2) != 2
            || !(8000..=96000).contains(&rate)
            || !(1..=4096).contains(&spf)
        {
            return Err(415);
        }
        let format = StreamFormat {
            codec: meta::Codec::Alac(alac_cookie(spf, 16, 2, rate)),
            sample_rate: rate,
            channels: 2,
            frames_per_packet: spf,
            aes: None,
        };
        let peer_control = s
            .get("controlPort")
            .and_then(Value::as_u64)
            .and_then(|p| u16::try_from(p).ok());
        self.shared.active.store(self.id, Ordering::SeqCst);
        self.ap2.end_stream();
        let session = Session::start(
            &format,
            self.shared.stream.clone(),
            self.peer.ip(),
            peer_control,
            Some(key),
        )
        .map_err(|_| 500u16)?;
        let reply = Value::dict([
            ("type", int(96)),
            ("dataPort", int(session.server_port)),
            ("controlPort", int(session.control_port)),
        ]);
        self.ap2.stream = Some(AudioStream {
            generation: session.generation,
            kind: Kind::Realtime { _session: session },
            sample_rate: rate,
        });
        self.announce();
        Ok(reply)
    }

    /// Reports the stream to the app once it is set up and recording.
    fn announce(&mut self) {
        let Some(s) = &self.ap2.stream else {
            return;
        };
        if !self.ap2.record || self.ap2.started == Some(s.generation) || !self.owns_stream() {
            return;
        }
        self.ap2.started = Some(s.generation);
        self.recording = true;
        let client = if self.client.is_empty() {
            self.peer.ip().to_string()
        } else {
            self.client.clone()
        };
        self.shared.emit(Event::Started { client });
    }

    /// SETRATEANCHORTIME: rate 1 starts playback with `rtpTime` (after a
    /// short delay), rate 0 pauses.
    fn anchor(&mut self, req: &Request) -> Response {
        let Ok(p) = plist::parse(&req.body) else {
            return Response::new(400);
        };
        // Realtime streams are synchronised by their control packets.
        let Some(
            s @ AudioStream {
                kind: Kind::Buffered { .. },
                ..
            },
        ) = &self.ap2.stream
        else {
            return Response::new(200);
        };
        if !self.owns_stream() {
            return Response::new(200);
        }
        let stream = &self.shared.stream;
        let rtp = p.get("rtpTime").and_then(Value::as_u64).map(|t| t as u32);
        let playing = p
            .get("rate")
            .and_then(Value::as_u64)
            .map_or(rtp.is_some(), |r| r & 1 == 1);
        if !playing {
            stream.hold();
            self.ap2.playing = false;
        } else if let (Some(rtp), false) = (rtp, self.ap2.playing) {
            stream.flush(Some(rtp));
            stream.sync(s.generation, rtp, Instant::now() + START_DELAY);
            self.ap2.playing = true;
        }
        Response::new(200)
    }

    /// FLUSHBUFFERED: drops audio now (skip, seek) or a range later.
    fn flush_buffered(&mut self, req: &Request) -> Response {
        let Ok(p) = plist::parse(&req.body) else {
            return Response::new(400);
        };
        let Some(AudioStream {
            kind: Kind::Buffered { buffered, .. },
            ..
        }) = &self.ap2.stream
        else {
            return Response::new(200);
        };
        if !self.owns_stream() {
            return Response::new(200);
        }
        let num = |k| p.get(k).and_then(Value::as_u64).map(|v| v as u32);
        let until_seq = num("flushUntilSeq");
        match (
            num("flushFromSeq"),
            num("flushFromTS"),
            until_seq,
            num("flushUntilTS"),
        ) {
            (Some(from), Some(from_ts), Some(until), Some(until_ts)) => {
                buffered.flush(Some(from), until);
                self.shared.stream.remove_range(from_ts, until_ts);
            }
            _ => {
                if let Some(until) = until_seq {
                    buffered.flush(None, until);
                }
                self.shared.stream.flush(None);
                self.shared.stream.hold();
                self.ap2.playing = false;
            }
        }
        Response::new(200)
    }

    /// TEARDOWN: with a "streams" list only the stream ends, otherwise the
    /// whole session.
    fn teardown(&mut self, req: &Request) -> Response {
        let streams_only = plist::parse(&req.body)
            .ok()
            .is_some_and(|p| p.get("streams").is_some());
        if streams_only {
            let had = self.ap2.stream.is_some();
            self.ap2.end_stream();
            if had && self.recording && self.owns_stream() {
                self.shared.stream.end();
                self.shared.emit(Event::Ended);
            }
            self.recording = false;
        } else {
            self.end();
            self.close = true;
        }
        Response::new(200)
    }
}

#[cfg(test)]
mod tests {
    //! A minimal AirPlay 2 sender against the receiver: transient pairing,
    //! encrypted control connection, SETUP of a buffered AAC stream, audio
    //! packets over TCP, anchors, flush and teardown.

    use crate::ap2::buffered::encode_packet;
    use crate::ap2::cipher::{Channel, Opener, Sealer};
    use crate::ap2::pairing::client;
    use crate::ap2::plist::{self, Value};
    use crate::crypto::sha512::hkdf_key;
    use crate::{Config, Event, Receiver};
    use bl_media::Decoder;
    use std::io::{BufRead, BufReader, Read, Write};
    use std::net::TcpStream;
    use std::time::{Duration, Instant};

    struct Sender {
        reader: BufReader<Channel>,
        writer: TcpStream,
        sealer: Option<Sealer>,
        cseq: u32,
    }

    struct Reply {
        status: u16,
        body: Vec<u8>,
    }

    impl Sender {
        fn connect(port: u16) -> Sender {
            let s = TcpStream::connect(("127.0.0.1", port)).unwrap();
            s.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
            Sender {
                reader: BufReader::new(Channel::new(s.try_clone().unwrap())),
                writer: s,
                sealer: None,
                cseq: 0,
            }
        }

        fn request(&mut self, method: &str, uri: &str, ct: &str, body: &[u8]) -> Reply {
            self.cseq += 1;
            let mut msg = format!(
                "{method} {uri} RTSP/1.0\r\nCSeq: {}\r\nUser-Agent: AirPlay/665.13.1\r\n",
                self.cseq
            );
            if !body.is_empty() {
                msg.push_str(&format!(
                    "Content-Type: {ct}\r\nContent-Length: {}\r\n",
                    body.len()
                ));
            }
            msg.push_str("\r\n");
            let mut bytes = msg.into_bytes();
            bytes.extend_from_slice(body);
            if let Some(s) = &mut self.sealer {
                bytes = s.seal(&bytes);
            }
            self.writer.write_all(&bytes).unwrap();
            self.response()
        }

        fn response(&mut self) -> Reply {
            let mut line = String::new();
            self.reader.read_line(&mut line).unwrap();
            let status = line.split(' ').nth(1).unwrap().parse().unwrap();
            let mut len = 0;
            loop {
                let mut h = String::new();
                self.reader.read_line(&mut h).unwrap();
                let h = h.trim_end();
                if h.is_empty() {
                    break;
                }
                if let Some(v) = h.strip_prefix("Content-Length: ") {
                    len = v.parse().unwrap();
                }
            }
            let mut body = vec![0; len];
            self.reader.read_exact(&mut body).unwrap();
            Reply { status, body }
        }

        fn plist(&mut self, method: &str, uri: &str, v: &Value) -> Reply {
            self.request(
                method,
                uri,
                "application/x-apple-binary-plist",
                &plist::write(v),
            )
        }

        fn encrypt(&mut self, secret: &[u8]) {
            let ahead = self.reader.buffer().to_vec();
            self.reader.consume(ahead.len());
            let opener = Opener::new(hkdf_key(
                secret,
                "Control-Salt",
                "Control-Read-Encryption-Key",
            ));
            self.reader.get_mut().encrypt(opener, &ahead).unwrap();
            self.sealer = Some(Sealer::new(hkdf_key(
                secret,
                "Control-Salt",
                "Control-Write-Encryption-Key",
            )));
        }
    }

    /// One second of a 1 kHz tone as raw AAC-LC frames, made by ffmpeg.
    fn aac_frames() -> Option<Vec<Vec<u8>>> {
        let path = std::env::temp_dir().join(format!(
            "bl-airplay-test-{}-{:?}.aac",
            std::process::id(),
            std::thread::current().id()
        ));
        let ok = std::process::Command::new("ffmpeg")
            .args([
                "-hide_banner",
                "-loglevel",
                "error",
                "-y",
                "-f",
                "lavfi",
                "-i",
            ])
            .arg("sine=frequency=1000:sample_rate=44100:duration=1")
            .args(["-ac", "2", "-c:a", "aac", "-b:a", "128k", "-f", "adts"])
            .arg(&path)
            .status()
            .ok()?
            .success();
        let data = std::fs::read(&path).ok();
        let _ = std::fs::remove_file(&path);
        let data = data.filter(|_| ok)?;
        let mut frames = Vec::new();
        let mut i = 0;
        while i + 7 <= data.len() {
            let len = ((data[i + 3] as usize & 3) << 11)
                | (data[i + 4] as usize) << 3
                | (data[i + 5] as usize) >> 5;
            let header = if data[i + 1] & 1 == 1 { 7 } else { 9 };
            if len < header || i + len > data.len() {
                break;
            }
            frames.push(data[i + header..i + len].to_vec());
            i += len;
        }
        Some(frames)
    }

    fn wait_event(rx: &Receiver, want: impl Fn(&Event) -> bool) -> Event {
        let end = Instant::now() + Duration::from_secs(5);
        while Instant::now() < end {
            if let Some(e) = rx.poll().into_iter().find(|e| want(e)) {
                return e;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        panic!("event did not arrive");
    }

    fn receiver() -> Receiver {
        Receiver::start(Config {
            name: "Test".into(),
            id: [2, 1, 2, 3, 4, 5],
            password: None,
        })
        .unwrap()
    }

    /// Pairs transiently and sets up the session; the encrypted sender.
    fn paired_session(rx: &Receiver) -> Sender {
        let mut tx = Sender::connect(rx.port());
        let secret = client::transient_setup(
            &mut |body| {
                tx.request("POST", "/pair-setup", "application/octet-stream", body)
                    .body
            },
            "3939",
        )
        .expect("transient pairing");
        tx.encrypt(&secret);
        let r = tx.plist(
            "SETUP",
            "rtsp://127.0.0.1/1",
            &Value::dict([
                ("timingProtocol", Value::String("PTP".into())),
                ("name", Value::String("Test Mac".into())),
            ]),
        );
        assert_eq!(r.status, 200);
        tx
    }

    #[test]
    fn realtime_alac_stream() {
        let rx = receiver();
        rx.stream().set_output_latency(0.0);
        let mut tx = paired_session(&rx);
        let sender_control = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
        let key = [0x33u8; 32];
        let setup = tx.plist(
            "SETUP",
            "rtsp://127.0.0.1/1",
            &Value::dict([(
                "streams",
                Value::Array(vec![Value::dict([
                    ("type", Value::Int(96)),
                    ("shk", Value::Data(key.to_vec())),
                    ("ct", Value::Int(2)),
                    ("sr", Value::Int(44100)),
                    ("spf", Value::Int(352)),
                    ("audioFormat", Value::Int(0x4_0000)),
                    (
                        "controlPort",
                        Value::Int(sender_control.local_addr().unwrap().port() as i128),
                    ),
                ])]),
            )]),
        );
        assert_eq!(setup.status, 200);
        let setup = plist::parse(&setup.body).unwrap();
        let s = &setup.get("streams").unwrap().as_array().unwrap()[0];
        let data_port = s.get("dataPort").unwrap().as_u64().unwrap() as u16;
        let control_port = s.get("controlPort").unwrap().as_u64().unwrap() as u16;
        assert_eq!(
            tx.request("RECORD", "rtsp://127.0.0.1/1", "", &[]).status,
            200
        );
        wait_event(&rx, |e| {
            *e == Event::Started {
                client: "Test Mac".into(),
            }
        });

        // Frame 0 is heard in 0.3 s (sync), then 0.5 s of a 441 Hz tone.
        let udp = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
        let mut sync = [0u8; 28];
        sync[0] = 0x90;
        sync[1] = 0xd7;
        let base = 100_000u32;
        sync[4..8].copy_from_slice(&(base - 13230).to_be_bytes());
        udp.send_to(&sync, ("127.0.0.1", control_port)).unwrap();
        for p in 0..63u32 {
            let pcm: Vec<i16> = (0..352u32)
                .flat_map(|i| {
                    let t = (p * 352 + i) as f32 / 44100.0;
                    let v = ((t * 441.0 * std::f32::consts::TAU).sin() * 16000.0) as i16;
                    [v, v]
                })
                .collect();
            let frame = crate::sender::alac_frame(&pcm);
            let packet = encode_packet(&key, p, base + p * 352, 0x1234, &frame, p as u64);
            udp.send_to(&packet[2..], ("127.0.0.1", data_port)).unwrap();
        }
        let mut dec = rx.stream().decoder();
        let mut pcm = Vec::new();
        let end = Instant::now() + Duration::from_secs(5);
        while pcm.len() < 2 * 20000 && Instant::now() < end {
            assert!(dec.next_chunk(&mut pcm).unwrap());
        }
        let left: Vec<f32> = pcm
            .chunks(2)
            .map(|c| c[0])
            .filter(|v| v.abs() > 0.01)
            .collect();
        assert!(left.len() > 15000, "{} loud frames", left.len());
        let peak = left.iter().fold(0.0f32, |m, v| m.max(v.abs()));
        let crossings = left
            .windows(2)
            .filter(|w| (w[0] < 0.0) != (w[1] < 0.0))
            .count() as f64
            * 44100.0
            / left.len() as f64;
        assert!(peak > 0.4, "peak {peak}");
        assert!((crossings - 882.0).abs() < 30.0, "{crossings} crossings/s");
    }

    #[test]
    fn unpaired_requests_are_refused() {
        let rx = receiver();
        let mut tx = Sender::connect(rx.port());
        assert_eq!(tx.request("GET", "/info", "", &[]).status, 200);
        let r = tx.plist(
            "SETUP",
            "rtsp://127.0.0.1/1",
            &Value::dict([("timingProtocol", Value::String("PTP".into()))]),
        );
        assert_ne!(r.status, 200);
        assert_ne!(tx.request("SETRATEANCHORTIME", "*", "", &[]).status, 200);
    }

    #[test]
    fn password_pairing_then_pair_verify() {
        use crate::crypto::ed25519::SigningKey;
        let rx = Receiver::start(Config {
            name: "Test".into(),
            id: [2, 1, 2, 3, 4, 6],
            password: Some("geheim".into()),
        })
        .unwrap();
        assert!(rx.airplay_txt().contains(&"flags=0x84".to_string()));
        // Transient pairing with the default PIN fails.
        let mut tx = Sender::connect(rx.port());
        let r = client::transient_setup(
            &mut |b| {
                tx.request("POST", "/pair-setup", "application/octet-stream", b)
                    .body
            },
            "3939",
        );
        assert!(r.is_none());
        // Pairing with the password, then pair-verify on a new connection.
        let ltsk = SigningKey::from_seed(&[8; 32]);
        let mut tx = Sender::connect(rx.port());
        let accessory = client::full_setup(
            &mut |b| {
                tx.request("POST", "/pair-setup", "application/octet-stream", b)
                    .body
            },
            "geheim",
            b"sender-1",
            &ltsk,
        )
        .expect("pairing with the password");
        let mut tx = Sender::connect(rx.port());
        let secret = client::verify(
            &mut |b| {
                tx.request("POST", "/pair-verify", "application/octet-stream", b)
                    .body
            },
            b"sender-1",
            &ltsk,
            &accessory,
        )
        .expect("pair-verify");
        tx.encrypt(&secret);
        let r = tx.plist(
            "SETUP",
            "rtsp://127.0.0.1/1",
            &Value::dict([("timingProtocol", Value::String("PTP".into()))]),
        );
        assert_eq!(r.status, 200);
        // Verified connections need no HTTP digest for AirPlay 1 requests.
        let r = tx.request(
            "SET_PARAMETER",
            "rtsp://127.0.0.1/1",
            "text/parameters",
            b"volume: -10.000000\r\n",
        );
        assert_eq!(r.status, 200);
    }

    #[test]
    fn buffered_aac_stream_from_a_paired_sender() {
        let Some(frames) = aac_frames() else {
            eprintln!("ffmpeg with AAC not available: skipped");
            return;
        };
        assert!(frames.len() > 30);
        let rx = receiver();
        rx.stream().set_output_latency(0.0);
        let mut tx = Sender::connect(rx.port());

        let info = plist::parse(&tx.request("GET", "/info", "", &[]).body).unwrap();
        assert_eq!(
            info.get("pk").and_then(Value::as_data).map(<[u8]>::len),
            Some(32)
        );

        let secret = client::transient_setup(
            &mut |body| {
                tx.request("POST", "/pair-setup", "application/octet-stream", body)
                    .body
            },
            "3939",
        )
        .expect("transient pairing");
        tx.encrypt(&secret);
        // The encrypted connection works.
        assert_eq!(tx.request("GET", "/info", "", &[]).status, 200);

        let session = tx.plist(
            "SETUP",
            "rtsp://127.0.0.1/1",
            &Value::dict([
                ("timingProtocol", Value::String("PTP".into())),
                ("name", Value::String("Test iPhone".into())),
                ("timingPeerInfo", Value::dict([])),
            ]),
        );
        assert_eq!(session.status, 200);
        let session = plist::parse(&session.body).unwrap();
        let event_port = session.get("eventPort").unwrap().as_u64().unwrap() as u16;
        assert!(session.get("timingPeerInfo").is_some());
        let _events = TcpStream::connect(("127.0.0.1", event_port)).unwrap();

        let key = [0x5au8; 32];
        let setup = tx.plist(
            "SETUP",
            "rtsp://127.0.0.1/1",
            &Value::dict([(
                "streams",
                Value::Array(vec![Value::dict([
                    ("type", Value::Int(103)),
                    ("shk", Value::Data(key.to_vec())),
                    ("audioFormat", Value::Int(0x40_0000)),
                    ("ct", Value::Int(4)),
                    ("spf", Value::Int(1024)),
                    ("sr", Value::Int(44100)),
                ])]),
            )]),
        );
        assert_eq!(setup.status, 200);
        let setup = plist::parse(&setup.body).unwrap();
        let stream = &setup.get("streams").unwrap().as_array().unwrap()[0];
        assert_eq!(stream.get("type").unwrap().as_u64(), Some(103));
        let data_port = stream.get("dataPort").unwrap().as_u64().unwrap() as u16;

        assert_eq!(
            tx.request("RECORD", "rtsp://127.0.0.1/1", "", &[]).status,
            200
        );
        let started = wait_event(&rx, |e| matches!(e, Event::Started { .. }));
        assert_eq!(
            started,
            Event::Started {
                client: "Test iPhone".into()
            }
        );
        let r = tx.request(
            "SET_PARAMETER",
            "rtsp://127.0.0.1/1",
            "text/parameters",
            b"volume: 0.000000\r\n",
        );
        assert_eq!(r.status, 200);
        wait_event(&rx, |e| matches!(e, Event::Volume(_)));

        // Audio ahead of time, then the start anchor.
        let mut data = TcpStream::connect(("127.0.0.1", data_port)).unwrap();
        for (i, f) in frames.iter().enumerate() {
            let p = encode_packet(&key, i as u32, i as u32 * 1024, 0x1600_0000, f, i as u64);
            data.write_all(&p).unwrap();
        }
        let r = tx.plist(
            "SETRATEANCHORTIME",
            "rtsp://127.0.0.1/1",
            &Value::dict([
                ("rate", Value::Int(1)),
                ("rtpTime", Value::Int(0)),
                ("networkTimeSecs", Value::Int(1)),
                ("networkTimeFrac", Value::Int(0)),
            ]),
        );
        assert_eq!(r.status, 200);

        let mut dec = rx.stream().decoder();
        assert_eq!(dec.spec().sample_rate, 44100);
        let mut pcm = Vec::new();
        let end = Instant::now() + Duration::from_secs(5);
        while pcm.len() < 2 * 44100 && Instant::now() < end {
            assert!(dec.next_chunk(&mut pcm).unwrap());
        }
        assert!(pcm.len() >= 2 * 44100, "decoded {} samples", pcm.len());
        // A 1 kHz tone at full volume, with about 2000 zero crossings a second
        // (skipping the encoder's start).
        let left: Vec<f32> = pcm.chunks(2).skip(4096).take(30000).map(|c| c[0]).collect();
        let peak = left.iter().fold(0.0f32, |m, v| m.max(v.abs()));
        let crossings = left
            .windows(2)
            .filter(|w| (w[0] < 0.0) != (w[1] < 0.0))
            .count() as f64
            * 44100.0
            / left.len() as f64;
        assert!(peak > 0.08, "peak {peak}"); // ffmpeg's sine: 1/8 full scale
        assert!(
            (crossings - 2000.0).abs() < 100.0,
            "{crossings} crossings/s"
        );

        // Pause, flush and end the stream.
        let r = tx.plist(
            "SETRATEANCHORTIME",
            "rtsp://127.0.0.1/1",
            &Value::dict([("rate", Value::Int(0))]),
        );
        assert_eq!(r.status, 200);
        let r = tx.plist(
            "FLUSHBUFFERED",
            "rtsp://127.0.0.1/1",
            &Value::dict([
                ("flushUntilSeq", Value::Int(1000)),
                ("flushUntilTS", Value::Int(1000 * 1024)),
            ]),
        );
        assert_eq!(r.status, 200);
        let r = tx.plist(
            "TEARDOWN",
            "rtsp://127.0.0.1/1",
            &Value::dict([(
                "streams",
                Value::Array(vec![Value::dict([("type", Value::Int(103))])]),
            )]),
        );
        assert_eq!(r.status, 200);
        wait_event(&rx, |e| *e == Event::Ended);
        // The session stays until the final TEARDOWN.
        assert_eq!(tx.request("GET", "/info", "", &[]).status, 200);
        assert_eq!(
            tx.plist("TEARDOWN", "rtsp://127.0.0.1/1", &Value::dict([]))
                .status,
            200
        );
    }
}
