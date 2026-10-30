//! The RTSP control connection of AirPlay senders and the receiver's
//! public interface. AirPlay 2 requests on the same connection are
//! handled in [`ap2`].

mod ap2;

use crate::ap2::cipher::{self, Channel};
use crate::ap2::pairing::{Identity, Pairings};
use crate::ap2::Description;
use crate::crypto;
use crate::mdns::Service;
use crate::meta::{self, StreamFormat};
use crate::rtsp::{self, Request, Response};
use crate::session::Session;
use crate::stream::LiveStream;
use std::io::{BufRead, BufReader, Write};
use std::net::{IpAddr, SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{channel, Receiver as Rx, Sender};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// Most control connections at the same time.
const MAX_CONNECTIONS: usize = 8;

#[derive(Clone, Debug)]
pub struct Config {
    /// Name shown on the senders.
    pub name: String,
    /// Device ID in place of a MAC address (part of the announced name).
    pub id: [u8; 6],
    /// Required password (none = open to the local network).
    pub password: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Event {
    /// A sender started streaming.
    Started {
        client: String,
    },
    /// The stream ended (sender stopped or disconnected).
    Ended,
    /// Sender volume (linear gain).
    Volume(f32),
    Track(meta::TrackInfo),
    Artwork {
        mime: String,
        data: Vec<u8>,
    },
    Progress {
        elapsed: f64,
        duration: f64,
    },
}

struct Shared {
    config: Config,
    stream: LiveStream,
    events: Mutex<Sender<Event>>,
    /// The connection that owns the current stream.
    active: AtomicU64,
    next_id: AtomicU64,
    connections: AtomicU64,
    /// Open control connections, to close the active one on disconnect.
    sockets: Mutex<Vec<(u64, TcpStream)>>,
    stop: AtomicBool,
    /// AirPlay 2 identity (a new key per start) and paired controllers.
    identity: Identity,
    pairings: Pairings,
}

impl Shared {
    fn emit(&self, e: Event) {
        if let Ok(tx) = self.events.lock() {
            let _ = tx.send(e);
        }
    }

    fn description(&self) -> Description<'_> {
        Description {
            name: &self.config.name,
            id: &self.config.id,
            public_key: self.identity.public_key_ref(),
            password: self.config.password.is_some(),
        }
    }
}

/// A running receiver: listens for senders until dropped.
pub struct Receiver {
    shared: Arc<Shared>,
    events: Rx<Event>,
    port: u16,
    accept: Option<std::thread::JoinHandle<()>>,
}

impl Receiver {
    pub fn start(config: Config) -> std::io::Result<Receiver> {
        let listener = TcpListener::bind("0.0.0.0:0")?;
        listener.set_nonblocking(true)?;
        let port = listener.local_addr()?.port();
        let (tx, rx) = channel();
        let seed: [u8; 32] = crypto::random_bytes(32).try_into().expect("32 bytes");
        let identity = Identity::new(crate::ap2::device_id(&config.id), &seed);
        let shared = Arc::new(Shared {
            identity,
            pairings: Pairings::default(),
            config,
            stream: LiveStream::new(),
            events: Mutex::new(tx),
            active: AtomicU64::new(0),
            next_id: AtomicU64::new(1),
            connections: AtomicU64::new(0),
            sockets: Mutex::new(Vec::new()),
            stop: AtomicBool::new(false),
        });
        let s = shared.clone();
        let accept = std::thread::Builder::new()
            .name("bl-airplay".into())
            .spawn(move || accept_loop(listener, s))?;
        Ok(Receiver {
            shared,
            events: rx,
            port,
            accept: Some(accept),
        })
    }

    pub fn port(&self) -> u16 {
        self.port
    }

    /// The service instance name ("<ID>@<name>").
    pub fn instance_name(&self) -> String {
        let c = &self.shared.config;
        format!("{}@{}", crypto::hash::hex(&c.id).to_uppercase(), c.name)
    }

    /// The `_airplay._tcp` (AirPlay 2) service instance name.
    pub fn airplay_instance_name(&self) -> String {
        self.shared.config.name.clone()
    }

    /// TXT record of the `_airplay._tcp` service.
    pub fn airplay_txt(&self) -> Vec<String> {
        self.shared.description().txt()
    }

    /// Both services to announce: `_raop._tcp` (AirPlay 1) and
    /// `_airplay._tcp` (AirPlay 2) on the receiver's port.
    pub fn services(&self) -> Vec<Service> {
        vec![
            Service {
                instance: self.instance_name(),
                service_type: "_raop._tcp".into(),
                port: self.port,
                txt: self.txt(),
            },
            Service {
                instance: self.airplay_instance_name(),
                service_type: "_airplay._tcp".into(),
                port: self.port,
                txt: self.airplay_txt(),
            },
        ]
    }

    /// TXT record of the `_raop._tcp` service.
    pub fn txt(&self) -> Vec<String> {
        let pw = self.shared.config.password.is_some();
        [
            "txtvers=1",
            "ch=2",
            "cn=0,1",
            "et=0,1",
            "ek=1",
            "sv=false",
            "da=true",
            "sr=44100",
            "ss=16",
            if pw { "pw=true" } else { "pw=false" },
            "vn=65537",
            "tp=UDP",
            "md=0,1,2",
            "vs=130.14",
            "sm=false",
            "am=Beatlumen",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect()
    }

    pub fn stream(&self) -> &LiveStream {
        &self.shared.stream
    }

    pub fn poll(&self) -> Vec<Event> {
        self.events.try_iter().collect()
    }

    /// Ends the current stream (the sender sees the speaker disconnect).
    pub fn disconnect(&self) {
        let old = self.shared.active.swap(
            self.shared.next_id.fetch_add(1, Ordering::SeqCst),
            Ordering::SeqCst,
        );
        self.shared.stream.end();
        if let Ok(socks) = self.shared.sockets.lock() {
            for (id, s) in socks.iter() {
                if *id == old || self.shared.stop.load(Ordering::SeqCst) {
                    let _ = s.shutdown(std::net::Shutdown::Both);
                }
            }
        }
    }
}

impl Drop for Receiver {
    fn drop(&mut self) {
        self.shared.stop.store(true, Ordering::SeqCst);
        self.disconnect();
        if let Some(t) = self.accept.take() {
            let _ = t.join();
        }
    }
}

fn accept_loop(listener: TcpListener, shared: Arc<Shared>) {
    while !shared.stop.load(Ordering::SeqCst) {
        match listener.accept() {
            Ok((stream, peer)) => {
                if shared.connections.load(Ordering::SeqCst) >= MAX_CONNECTIONS as u64 {
                    continue;
                }
                shared.connections.fetch_add(1, Ordering::SeqCst);
                let s = shared.clone();
                let spawned = std::thread::Builder::new()
                    .name("bl-airplay-rtsp".into())
                    .spawn(move || {
                        let id = s.next_id.fetch_add(1, Ordering::SeqCst);
                        Connection::new(id, peer, &s).run(stream);
                        s.connections.fetch_sub(1, Ordering::SeqCst);
                    });
                if spawned.is_err() {
                    shared.connections.fetch_sub(1, Ordering::SeqCst);
                }
            }
            Err(_) => std::thread::sleep(Duration::from_millis(100)),
        }
    }
}

struct Connection<'a> {
    id: u64,
    peer: SocketAddr,
    shared: &'a Shared,
    format: Option<StreamFormat>,
    session: Option<Session>,
    nonce: String,
    authorized: bool,
    volume_db: f32,
    client: String,
    recording: bool,
    /// Close the connection after the current response.
    close: bool,
    ap2: ap2::State,
}

impl<'a> Connection<'a> {
    fn new(id: u64, peer: SocketAddr, shared: &'a Shared) -> Connection<'a> {
        Connection {
            id,
            peer,
            shared,
            format: None,
            session: None,
            nonce: crypto::hash::hex(&crypto::random_bytes(16)),
            authorized: shared.config.password.is_none(),
            volume_db: -15.0,
            client: String::new(),
            recording: false,
            close: false,
            ap2: ap2::State::default(),
        }
    }

    fn owns_stream(&self) -> bool {
        self.shared.active.load(Ordering::SeqCst) == self.id
    }

    fn run(mut self, stream: TcpStream) {
        let _ = stream.set_nonblocking(false);
        let _ = stream.set_read_timeout(Some(Duration::from_secs(300)));
        let _ = stream.set_nodelay(true);
        let local = stream.local_addr().ok();
        let Ok(mut writer) = stream.try_clone() else {
            return;
        };
        if let (Ok(mut socks), Ok(c)) = (self.shared.sockets.lock(), stream.try_clone()) {
            socks.push((self.id, c));
        }
        let mut reader = BufReader::new(Channel::new(stream));
        while !self.shared.stop.load(Ordering::SeqCst) {
            let req = match rtsp::read_request(&mut reader) {
                Ok(r) => r,
                Err(_) => break,
            };
            // A newer sender took over: this one is done.
            if self.recording && !self.owns_stream() {
                break;
            }
            let resp = self.handle(&req, local);
            let mut bytes = resp.to_bytes(req.header("CSeq"));
            if let Some(s) = &mut self.ap2.sealer {
                bytes = s.seal(&bytes);
            }
            if writer.write_all(&bytes).is_err() {
                break;
            }
            // Pairing completed: everything after this response is
            // encrypted (data read ahead already is, too).
            if let Some(secret) = self.ap2.pending_secret.take() {
                let (opener, sealer) = cipher::control_keys(&secret);
                let ahead = reader.buffer().to_vec();
                reader.consume(ahead.len());
                if reader.get_mut().encrypt(opener, &ahead).is_err() {
                    break;
                }
                self.ap2.sealer = Some(sealer);
                self.authorized = true;
            }
            if self.close {
                break;
            }
        }
        self.end();
        if let Ok(mut socks) = self.shared.sockets.lock() {
            socks.retain(|(id, _)| *id != self.id);
        }
    }

    fn end(&mut self) {
        self.session = None;
        self.ap2.end_stream();
        self.ap2.events = None;
        if self.recording && self.owns_stream() {
            self.shared.stream.end();
            self.shared.emit(Event::Ended);
        }
        self.recording = false;
    }

    fn handle(&mut self, req: &Request, local: Option<SocketAddr>) -> Response {
        let mut resp = Response::new(200).header("Audio-Jack-Status", "connected; type=analog");
        if let Some(challenge) = req.header("Apple-Challenge") {
            if let Some(r) = local.and_then(|l| self.challenge_response(challenge, l.ip())) {
                resp = resp.header("Apple-Response", r);
            }
        }
        if let Some(r) = self.handle_ap2(req, local) {
            return r;
        }
        if req.method != "OPTIONS" && !self.check_auth(req) {
            return Response::new(401).header(
                "WWW-Authenticate",
                format!("Digest realm=\"raop\", nonce=\"{}\"", self.nonce),
            );
        }
        if let Some(name) = req.header("X-Apple-Client-Name") {
            self.client = name.chars().take(64).collect();
        }
        match req.method.as_str() {
            "OPTIONS" => resp.header(
                "Public",
                "ANNOUNCE, SETUP, RECORD, PAUSE, FLUSH, FLUSHBUFFERED, TEARDOWN, OPTIONS, GET_PARAMETER, SET_PARAMETER, POST, GET",
            ),
            "ANNOUNCE" => {
                let sdp = String::from_utf8_lossy(&req.body);
                match meta::parse_sdp(&sdp) {
                    Ok(f) => {
                        self.format = Some(f);
                        resp
                    }
                    Err(_) => Response::new(415),
                }
            }
            "SETUP" => self.setup(req, resp),
            "RECORD" => {
                if self.session.is_none() {
                    return Response::new(455);
                }
                if !self.recording {
                    self.recording = true;
                    let client = if self.client.is_empty() {
                        self.peer.ip().to_string()
                    } else {
                        self.client.clone()
                    };
                    self.shared.emit(Event::Started { client });
                }
                resp
            }
            "FLUSH" => {
                if self.owns_stream() {
                    let until = req
                        .header("RTP-Info")
                        .and_then(|h| rtsp::param(h, "rtptime"))
                        .and_then(|v| v.parse::<u32>().ok());
                    self.shared.stream.flush(until);
                }
                resp
            }
            "TEARDOWN" => {
                self.end();
                self.close = true;
                resp
            }
            "SET_PARAMETER" => {
                self.set_parameter(req);
                resp
            }
            "GET_PARAMETER" => {
                let body = String::from_utf8_lossy(&req.body);
                if body.contains("volume") {
                    resp.body("text/parameters", format!("volume: {:.6}\r\n", self.volume_db).into_bytes())
                } else {
                    resp
                }
            }
            // Keep-alive of some senders.
            "POST" if req.uri == "/feedback" => resp,
            "PAUSE" => resp,
            _ => Response::new(501),
        }
    }

    fn setup(&mut self, req: &Request, resp: Response) -> Response {
        let Some(format) = &self.format else {
            return Response::new(455);
        };
        let transport = req.header("Transport").unwrap_or("");
        let control_port = rtsp::param(transport, "control_port").and_then(|v| v.parse().ok());
        // Take over from any previous sender.
        self.shared.active.store(self.id, Ordering::SeqCst);
        self.session = None;
        match Session::start(
            format,
            self.shared.stream.clone(),
            self.peer.ip(),
            control_port,
            None,
        ) {
            Ok(s) => {
                let t = format!(
                    "RTP/AVP/UDP;unicast;mode=record;server_port={};control_port={};timing_port={}",
                    s.server_port, s.control_port, s.timing_port
                );
                self.session = Some(s);
                resp.header("Transport", t).header("Session", "1")
            }
            Err(_) => Response::new(500),
        }
    }

    fn set_parameter(&mut self, req: &Request) {
        let ct = req.content_type();
        if ct.starts_with("text/parameters") {
            let text = String::from_utf8_lossy(&req.body);
            for line in text.lines() {
                if let Some(gain) = meta::volume_gain(line) {
                    if let Some(db) = line.trim().strip_prefix("volume:") {
                        self.volume_db = db.trim().parse().unwrap_or(self.volume_db);
                    }
                    if self.owns_stream() {
                        self.shared.stream.set_volume(gain);
                        self.shared.emit(Event::Volume(gain));
                    }
                } else if let Some(rate) = self.sample_rate() {
                    if let Some((elapsed, duration)) = meta::parse_progress(line, rate) {
                        self.shared.emit(Event::Progress { elapsed, duration });
                    }
                }
            }
        } else if ct.starts_with("application/x-dmap-tagged") {
            self.shared.emit(Event::Track(meta::parse_dmap(&req.body)));
        } else if ct.starts_with("image/") {
            self.shared.emit(Event::Artwork {
                mime: ct.to_string(),
                data: req.body.clone(),
            });
        }
    }

    /// Signs challenge + our address + device ID with the protocol key.
    fn challenge_response(&self, challenge: &str, local: IpAddr) -> Option<String> {
        let chall = crypto::base64_decode(challenge)?;
        if chall.len() > 16 {
            return None;
        }
        let mut buf = chall;
        match local {
            IpAddr::V4(a) => buf.extend_from_slice(&a.octets()),
            IpAddr::V6(a) => match a.to_ipv4_mapped() {
                Some(v4) => buf.extend_from_slice(&v4.octets()),
                None => buf.extend_from_slice(&a.octets()),
            },
        }
        buf.extend_from_slice(&self.shared.config.id);
        if buf.len() < 32 {
            buf.resize(32, 0);
        }
        crypto::rsa_sign_raw(&buf).map(|s| crypto::base64_encode(&s))
    }

    /// HTTP digest authentication (RFC 2617 without qop, as senders use it).
    fn check_auth(&mut self, req: &Request) -> bool {
        if self.authorized {
            return true;
        }
        let Some(password) = &self.shared.config.password else {
            return true;
        };
        let Some(h) = req.header("Authorization") else {
            return false;
        };
        let field = |k| rtsp::digest_field(h, k).unwrap_or_default();
        if field("nonce") != self.nonce {
            return false;
        }
        let md5 = |s: String| crypto::hash::hex(&crypto::hash::md5(s.as_bytes()));
        let ha1 = md5(format!(
            "{}:{}:{}",
            field("username"),
            field("realm"),
            password
        ));
        let ha2 = md5(format!("{}:{}", req.method, field("uri")));
        let expected = md5(format!("{ha1}:{}:{ha2}", self.nonce));
        let given = field("response").to_ascii_lowercase();
        // Compare without an early exit.
        let ok = expected.len() == given.len()
            && expected
                .bytes()
                .zip(given.bytes())
                .fold(0u8, |acc, (a, b)| acc | (a ^ b))
                == 0;
        self.authorized = ok;
        ok
    }
}
