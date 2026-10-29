//! A UPnP AV media renderer (a DLNA "DMR"): the app shows up as a speaker
//! for control points (Windows "Play to", BubbleUPnP, TVs, …), which tell
//! it to play a URL. This side speaks the protocol; the app plays.
//!
//! * SSDP: announces the device and answers searches.
//! * HTTP: device and service descriptions, SOAP control of AVTransport,
//!   RenderingControl and ConnectionManager, GENA event subscriptions
//!   (LastChange).
//!
//! Commands from control points are queued ([`Renderer::poll`]); the app
//! reports its playback with [`Renderer::set_status`].

use crate::xml;
use bl_net::http::{read_headers, read_line};
use std::io::{BufReader, Read, Write};
use std::net::{IpAddr, Ipv4Addr, SocketAddr, SocketAddrV4, TcpListener, TcpStream, UdpSocket};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const SSDP_GROUP: Ipv4Addr = Ipv4Addr::new(239, 255, 255, 250);
const SSDP_PORT: u16 = 1900;
const MAX_AGE: u32 = 1800;
const MAX_BODY: usize = 1024 * 1024;
const MAX_SUBSCRIPTIONS: usize = 32;

const DEVICE_TYPE: &str = "urn:schemas-upnp-org:device:MediaRenderer:1";
const AVT: &str = "urn:schemas-upnp-org:service:AVTransport:1";
const RCS: &str = "urn:schemas-upnp-org:service:RenderingControl:1";
const CMS: &str = "urn:schemas-upnp-org:service:ConnectionManager:1";

#[derive(Debug, Clone)]
pub struct Config {
    pub name: String,
    /// "uuid:…", stable across starts.
    pub udn: String,
    /// What the app plays ("http-get:*:audio/mpeg:*", …).
    pub sink_protocols: Vec<String>,
}

/// Title and the like from a control point's DIDL-Lite.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Meta {
    pub title: String,
    pub artist: String,
    pub album: String,
    pub art_url: String,
    pub duration: Option<f64>,
    pub mime: String,
}

impl Meta {
    pub fn parse(didl: &str) -> Meta {
        let text = |t: &str| xml::text(didl, t).unwrap_or_default();
        let res_tag = didl
            .find("<res")
            .and_then(|i| didl[i..].find('>').map(|j| &didl[i..i + j]))
            .unwrap_or("");
        let attr = |name: &str| -> String {
            let key = format!("{name}=\"");
            res_tag
                .find(&key)
                .and_then(|i| {
                    let rest = &res_tag[i + key.len()..];
                    rest.find('"').map(|j| xml::unescape(&rest[..j]))
                })
                .unwrap_or_default()
        };
        let artist = xml::text(didl, "artist").or_else(|| xml::text(didl, "creator"));
        Meta {
            title: text("title"),
            artist: artist.unwrap_or_default(),
            album: text("album"),
            art_url: text("albumArtURI"),
            duration: crate::parse_time(&attr("duration")),
            mime: attr("protocolInfo")
                .split(':')
                .nth(2)
                .unwrap_or("")
                .to_string(),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Command {
    /// A new stream (`next`: to play after the current one).
    SetUri {
        uri: String,
        meta: Meta,
        next: bool,
    },
    Play,
    Pause,
    Stop,
    /// Seconds into the track.
    Seek(f64),
    /// 0..100
    Volume(u32),
    Mute(bool),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum State {
    #[default]
    NoMedia,
    Stopped,
    Playing,
    Paused,
    Transitioning,
}

impl State {
    fn name(self) -> &'static str {
        match self {
            State::NoMedia => "NO_MEDIA_PRESENT",
            State::Stopped => "STOPPED",
            State::Playing => "PLAYING",
            State::Paused => "PAUSED_PLAYBACK",
            State::Transitioning => "TRANSITIONING",
        }
    }
}

/// The app's playback as the renderer reports it.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Status {
    pub state: State,
    pub position: f64,
    pub duration: f64,
    pub volume: u32,
    pub mute: bool,
}

struct Subscription {
    sid: String,
    callbacks: Vec<String>,
    service: &'static str,
    expires: Instant,
    seq: u32,
}

#[derive(Default)]
struct Media {
    uri: String,
    didl: String,
    next_uri: String,
    next_didl: String,
}

struct Shared {
    config: Config,
    port: u16,
    media: Mutex<Media>,
    status: Mutex<Status>,
    commands: Mutex<Vec<Command>>,
    subs: Mutex<Vec<Subscription>>,
    /// Events to send: service whose state changed.
    pending: Mutex<Vec<&'static str>>,
    stop: AtomicBool,
}

pub struct Renderer {
    shared: Arc<Shared>,
    threads: Vec<std::thread::JoinHandle<()>>,
}

fn spawn(
    name: &str,
    f: impl FnOnce() + Send + 'static,
) -> std::io::Result<std::thread::JoinHandle<()>> {
    std::thread::Builder::new().name(name.into()).spawn(f)
}

fn hms(secs: f64) -> String {
    let s = secs.max(0.0) as u64;
    format!("{}:{:02}:{:02}", s / 3600, s / 60 % 60, s % 60)
}

/// The local address `peer` reaches this device at.
fn local_ip_for(peer: IpAddr) -> Option<IpAddr> {
    let s = UdpSocket::bind("0.0.0.0:0").ok()?;
    s.connect(SocketAddr::new(peer, 9)).ok()?;
    s.local_addr()
        .ok()
        .map(|a| a.ip())
        .filter(|ip| !ip.is_unspecified())
}

impl Renderer {
    pub fn start(config: Config) -> std::io::Result<Renderer> {
        let listener = TcpListener::bind("0.0.0.0:0")?;
        listener.set_nonblocking(true)?;
        let port = listener.local_addr()?.port();
        let ssdp = bl_net::udp::bind_shared_v4(SSDP_PORT)?;
        ssdp.join_multicast_v4(&SSDP_GROUP, &Ipv4Addr::UNSPECIFIED)?;
        ssdp.set_multicast_ttl_v4(4)?;
        ssdp.set_read_timeout(Some(Duration::from_millis(250)))?;
        let shared = Arc::new(Shared {
            config,
            port,
            media: Mutex::new(Media::default()),
            status: Mutex::new(Status {
                volume: 50,
                ..Default::default()
            }),
            commands: Mutex::new(Vec::new()),
            subs: Mutex::new(Vec::new()),
            pending: Mutex::new(Vec::new()),
            stop: AtomicBool::new(false),
        });
        let mut threads = Vec::new();
        {
            let s = shared.clone();
            threads.push(spawn("bl-upnp-http", move || {
                while !s.stop.load(Ordering::Relaxed) {
                    match listener.accept() {
                        Ok((conn, peer)) => {
                            let s = s.clone();
                            let _ = spawn("bl-upnp-conn", move || {
                                let _ = handle(&s, conn, peer);
                            });
                        }
                        Err(_) => std::thread::sleep(Duration::from_millis(50)),
                    }
                }
            })?);
        }
        {
            let s = shared.clone();
            threads.push(spawn("bl-upnp-ssdp", move || ssdp_loop(&s, ssdp))?);
        }
        {
            let s = shared.clone();
            threads.push(spawn("bl-upnp-events", move || {
                while !s.stop.load(Ordering::Relaxed) {
                    std::thread::sleep(Duration::from_millis(100));
                    let services: Vec<&'static str> =
                        std::mem::take(&mut *s.pending.lock().unwrap_or_else(|e| e.into_inner()));
                    let mut seen = Vec::new();
                    for svc in services {
                        if !seen.contains(&svc) {
                            seen.push(svc);
                            notify_all(&s, svc);
                        }
                    }
                }
            })?);
        }
        Ok(Renderer { shared, threads })
    }

    pub fn port(&self) -> u16 {
        self.shared.port
    }

    /// Commands from control points since the last call.
    pub fn poll(&self) -> Vec<Command> {
        std::mem::take(
            &mut *self
                .shared
                .commands
                .lock()
                .unwrap_or_else(|e| e.into_inner()),
        )
    }

    /// Reports the app's playback; subscribers hear about changes.
    pub fn set_status(&self, status: Status) {
        let mut st = self.shared.status.lock().unwrap_or_else(|e| e.into_inner());
        let avt = st.state != status.state || (st.duration - status.duration).abs() > 0.5;
        let rcs = st.volume != status.volume || st.mute != status.mute;
        *st = status;
        drop(st);
        let mut p = self
            .shared
            .pending
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        if avt {
            p.push(AVT);
        }
        if rcs {
            p.push(RCS);
        }
    }

    /// The stream a control point set (URI and its DIDL-Lite).
    pub fn current(&self) -> (String, Meta) {
        let m = self.shared.media.lock().unwrap_or_else(|e| e.into_inner());
        (m.uri.clone(), Meta::parse(&m.didl))
    }

    /// Moves the "next" stream to the current one (when the current ended).
    pub fn advance(&self) -> Option<(String, Meta)> {
        let mut m = self.shared.media.lock().unwrap_or_else(|e| e.into_inner());
        if m.next_uri.is_empty() {
            return None;
        }
        m.uri = std::mem::take(&mut m.next_uri);
        m.didl = std::mem::take(&mut m.next_didl);
        let r = (m.uri.clone(), Meta::parse(&m.didl));
        drop(m);
        self.shared
            .pending
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(AVT);
        Some(r)
    }
}

impl Drop for Renderer {
    fn drop(&mut self) {
        self.shared.stop.store(true, Ordering::Relaxed);
        for t in self.threads.drain(..) {
            let _ = t.join();
        }
    }
}

// ---- SSDP ----------------------------------------------------------------------

fn targets(udn: &str) -> Vec<(String, String)> {
    // (NT/ST, USN)
    let mut v = vec![
        (
            "upnp:rootdevice".to_string(),
            format!("{udn}::upnp:rootdevice"),
        ),
        (udn.to_string(), udn.to_string()),
        (DEVICE_TYPE.to_string(), format!("{udn}::{DEVICE_TYPE}")),
    ];
    for s in [AVT, RCS, CMS] {
        v.push((s.to_string(), format!("{udn}::{s}")));
    }
    v
}

fn location(s: &Shared, ip: IpAddr) -> String {
    format!("http://{ip}:{}/description.xml", s.port)
}

const SERVER: &str = "Beatlumen/1.0 UPnP/1.0 DLNADOC/1.50";

fn notify(s: &Shared, sock: &UdpSocket, alive: bool) {
    let group = SocketAddr::V4(SocketAddrV4::new(SSDP_GROUP, SSDP_PORT));
    let Some(ip) = local_ip_for(IpAddr::V4(SSDP_GROUP)) else {
        return;
    };
    for (nt, usn) in targets(&s.config.udn) {
        let msg = if alive {
            format!(
                "NOTIFY * HTTP/1.1\r\nHOST: 239.255.255.250:1900\r\nCACHE-CONTROL: max-age={MAX_AGE}\r\nLOCATION: {}\r\nNT: {nt}\r\nNTS: ssdp:alive\r\nSERVER: {SERVER}\r\nUSN: {usn}\r\n\r\n",
                location(s, ip)
            )
        } else {
            format!("NOTIFY * HTTP/1.1\r\nHOST: 239.255.255.250:1900\r\nNT: {nt}\r\nNTS: ssdp:byebye\r\nUSN: {usn}\r\n\r\n")
        };
        let _ = sock.send_to(msg.as_bytes(), group);
    }
}

fn ssdp_loop(s: &Shared, sock: UdpSocket) {
    let start = Instant::now();
    let mut announce = [0u64, 1, 3].into_iter().peekable();
    let mut last = Instant::now();
    let mut buf = [0u8; 2048];
    while !s.stop.load(Ordering::Relaxed) {
        if let Some(&at) = announce.peek() {
            if start.elapsed() >= Duration::from_secs(at) {
                announce.next();
                notify(s, &sock, true);
                last = Instant::now();
            }
        } else if last.elapsed() > Duration::from_secs(MAX_AGE as u64 / 3) {
            notify(s, &sock, true);
            last = Instant::now();
        }
        let Ok((n, from)) = sock.recv_from(&mut buf) else {
            continue;
        };
        let text = String::from_utf8_lossy(&buf[..n]);
        if !text.starts_with("M-SEARCH") {
            continue;
        }
        let header = |name: &str| {
            text.lines().find_map(|l| {
                let (k, v) = l.split_once(':')?;
                k.trim()
                    .eq_ignore_ascii_case(name)
                    .then(|| v.trim().to_string())
            })
        };
        let st = header("ST").unwrap_or_default();
        let Some(ip) = local_ip_for(from.ip()) else {
            continue;
        };
        for (nt, usn) in targets(&s.config.udn) {
            if st == "ssdp:all" || st == nt {
                let msg = format!(
                    "HTTP/1.1 200 OK\r\nCACHE-CONTROL: max-age={MAX_AGE}\r\nEXT:\r\nLOCATION: {}\r\nSERVER: {SERVER}\r\nST: {nt}\r\nUSN: {usn}\r\n\r\n",
                    location(s, ip)
                );
                let _ = sock.send_to(msg.as_bytes(), from);
            }
        }
    }
    notify(s, &sock, false);
}

// ---- HTTP ------------------------------------------------------------------------

fn respond(
    conn: &mut TcpStream,
    status: &str,
    ctype: &str,
    extra: &str,
    body: &[u8],
) -> std::io::Result<()> {
    let head = format!(
        "HTTP/1.1 {status}\r\nContent-Type: {ctype}\r\nContent-Length: {}\r\nServer: {SERVER}\r\nConnection: close\r\n{extra}\r\n",
        body.len()
    );
    conn.write_all(head.as_bytes())?;
    conn.write_all(body)
}

fn handle(s: &Shared, conn: TcpStream, peer: SocketAddr) -> std::io::Result<()> {
    conn.set_read_timeout(Some(Duration::from_secs(10)))?;
    conn.set_write_timeout(Some(Duration::from_secs(10)))?;
    let mut reader = BufReader::new(conn.try_clone()?);
    let mut limit = 32 * 1024;
    let Some(line) = read_line(&mut reader, &mut limit)? else {
        return Ok(());
    };
    let headers = read_headers(&mut reader, &mut limit)?;
    let len = headers.content_length().unwrap_or(0) as usize;
    if len > MAX_BODY {
        return respond(
            &mut conn.try_clone()?,
            "413 Payload Too Large",
            "text/plain",
            "",
            b"",
        );
    }
    let mut body = vec![0u8; len];
    reader.read_exact(&mut body)?;
    let mut parts = line.split_whitespace();
    let (method, path) = (parts.next().unwrap_or(""), parts.next().unwrap_or(""));
    let mut conn = conn;
    let xml_type = "text/xml; charset=\"utf-8\"";
    match (method, path) {
        ("GET", "/description.xml") => {
            respond(&mut conn, "200 OK", xml_type, "", description(s).as_bytes())
        }
        ("GET", "/AVTransport.xml") => respond(
            &mut conn,
            "200 OK",
            xml_type,
            "",
            scpd(AVT_ACTIONS, AVT_VARS).as_bytes(),
        ),
        ("GET", "/RenderingControl.xml") => respond(
            &mut conn,
            "200 OK",
            xml_type,
            "",
            scpd(RCS_ACTIONS, RCS_VARS).as_bytes(),
        ),
        ("GET", "/ConnectionManager.xml") => respond(
            &mut conn,
            "200 OK",
            xml_type,
            "",
            scpd(CMS_ACTIONS, CMS_VARS).as_bytes(),
        ),
        ("POST", p) if p.starts_with("/ctl/") => {
            let service = match p {
                "/ctl/AVTransport" => AVT,
                "/ctl/RenderingControl" => RCS,
                "/ctl/ConnectionManager" => CMS,
                _ => return respond(&mut conn, "404 Not Found", "text/plain", "", b""),
            };
            let body = String::from_utf8_lossy(&body).into_owned();
            let action = headers
                .get("SOAPAction")
                .and_then(|a| {
                    a.trim()
                        .trim_matches('"')
                        .rsplit('#')
                        .next()
                        .map(str::to_string)
                })
                .unwrap_or_default();
            match control(s, service, &action, &body) {
                Ok(args) => {
                    let mut out = format!(
                        "<?xml version=\"1.0\" encoding=\"utf-8\"?>\n<s:Envelope xmlns:s=\"http://schemas.xmlsoap.org/soap/envelope/\" s:encodingStyle=\"http://schemas.xmlsoap.org/soap/encoding/\"><s:Body><u:{action}Response xmlns:u=\"{service}\">"
                    );
                    for (k, v) in args {
                        out.push_str(&format!("<{k}>{}</{k}>", xml::escape(&v)));
                    }
                    out.push_str(&format!("</u:{action}Response></s:Body></s:Envelope>"));
                    respond(&mut conn, "200 OK", xml_type, "EXT:\r\n", out.as_bytes())
                }
                Err((code, desc)) => {
                    let out = format!(
                        "<?xml version=\"1.0\" encoding=\"utf-8\"?>\n<s:Envelope xmlns:s=\"http://schemas.xmlsoap.org/soap/envelope/\" s:encodingStyle=\"http://schemas.xmlsoap.org/soap/encoding/\"><s:Body><s:Fault><faultcode>s:Client</faultcode><faultstring>UPnPError</faultstring><detail><UPnPError xmlns=\"urn:schemas-upnp-org:control-1-0\"><errorCode>{code}</errorCode><errorDescription>{desc}</errorDescription></UPnPError></detail></s:Fault></s:Body></s:Envelope>"
                    );
                    respond(
                        &mut conn,
                        "500 Internal Server Error",
                        xml_type,
                        "EXT:\r\n",
                        out.as_bytes(),
                    )
                }
            }
        }
        ("SUBSCRIBE", p) | ("UNSUBSCRIBE", p) if p.starts_with("/evt/") => {
            let service = match p {
                "/evt/AVTransport" => AVT,
                "/evt/RenderingControl" => RCS,
                "/evt/ConnectionManager" => CMS,
                _ => return respond(&mut conn, "404 Not Found", "text/plain", "", b""),
            };
            subscription(s, &mut conn, method, service, &headers, peer)
        }
        _ => respond(&mut conn, "404 Not Found", "text/plain", "", b""),
    }
}

fn description(s: &Shared) -> String {
    let service = |ty: &str, name: &str| {
        format!(
            "<service><serviceType>{ty}</serviceType><serviceId>urn:upnp-org:serviceId:{name}</serviceId><SCPDURL>/{name}.xml</SCPDURL><controlURL>/ctl/{name}</controlURL><eventSubURL>/evt/{name}</eventSubURL></service>"
        )
    };
    format!(
        "<?xml version=\"1.0\" encoding=\"utf-8\"?>\n<root xmlns=\"urn:schemas-upnp-org:device-1-0\" xmlns:dlna=\"urn:schemas-dlna-org:device-1-0\"><specVersion><major>1</major><minor>0</minor></specVersion><device><deviceType>{DEVICE_TYPE}</deviceType><friendlyName>{}</friendlyName><manufacturer>Beatlumen</manufacturer><manufacturerURL>https://github.com/RandomNam3/Beatlumen</manufacturerURL><modelDescription>Music player with visuals</modelDescription><modelName>Beatlumen</modelName><modelNumber>1</modelNumber><UDN>{}</UDN><dlna:X_DLNADOC>DMR-1.50</dlna:X_DLNADOC><serviceList>{}{}{}</serviceList></device></root>",
        xml::escape(&s.config.name),
        xml::escape(&s.config.udn),
        service(AVT, "AVTransport"),
        service(RCS, "RenderingControl"),
        service(CMS, "ConnectionManager"),
    )
}

// ---- service descriptions ------------------------------------------------------

/// (action, [(argument, in?, related state variable)])
type Actions = &'static [(&'static str, &'static [(&'static str, bool, &'static str)])];
/// (variable, type, evented, allowed values)
type Vars = &'static [(&'static str, &'static str, bool, &'static [&'static str])];

const AVT_ACTIONS: Actions = &[
    (
        "SetAVTransportURI",
        &[
            ("InstanceID", true, "A_ARG_TYPE_InstanceID"),
            ("CurrentURI", true, "AVTransportURI"),
            ("CurrentURIMetaData", true, "AVTransportURIMetaData"),
        ],
    ),
    (
        "SetNextAVTransportURI",
        &[
            ("InstanceID", true, "A_ARG_TYPE_InstanceID"),
            ("NextURI", true, "NextAVTransportURI"),
            ("NextURIMetaData", true, "NextAVTransportURIMetaData"),
        ],
    ),
    (
        "GetMediaInfo",
        &[
            ("InstanceID", true, "A_ARG_TYPE_InstanceID"),
            ("NrTracks", false, "NumberOfTracks"),
            ("MediaDuration", false, "CurrentMediaDuration"),
            ("CurrentURI", false, "AVTransportURI"),
            ("CurrentURIMetaData", false, "AVTransportURIMetaData"),
            ("NextURI", false, "NextAVTransportURI"),
            ("NextURIMetaData", false, "NextAVTransportURIMetaData"),
            ("PlayMedium", false, "PlaybackStorageMedium"),
            ("RecordMedium", false, "RecordStorageMedium"),
            ("WriteStatus", false, "RecordMediumWriteStatus"),
        ],
    ),
    (
        "GetTransportInfo",
        &[
            ("InstanceID", true, "A_ARG_TYPE_InstanceID"),
            ("CurrentTransportState", false, "TransportState"),
            ("CurrentTransportStatus", false, "TransportStatus"),
            ("CurrentSpeed", false, "TransportPlaySpeed"),
        ],
    ),
    (
        "GetPositionInfo",
        &[
            ("InstanceID", true, "A_ARG_TYPE_InstanceID"),
            ("Track", false, "CurrentTrack"),
            ("TrackDuration", false, "CurrentTrackDuration"),
            ("TrackMetaData", false, "CurrentTrackMetaData"),
            ("TrackURI", false, "CurrentTrackURI"),
            ("RelTime", false, "RelativeTimePosition"),
            ("AbsTime", false, "AbsoluteTimePosition"),
            ("RelCount", false, "RelativeCounterPosition"),
            ("AbsCount", false, "AbsoluteCounterPosition"),
        ],
    ),
    (
        "GetDeviceCapabilities",
        &[
            ("InstanceID", true, "A_ARG_TYPE_InstanceID"),
            ("PlayMedia", false, "PossiblePlaybackStorageMedia"),
            ("RecMedia", false, "PossibleRecordStorageMedia"),
            ("RecQualityModes", false, "PossibleRecordQualityModes"),
        ],
    ),
    (
        "GetTransportSettings",
        &[
            ("InstanceID", true, "A_ARG_TYPE_InstanceID"),
            ("PlayMode", false, "CurrentPlayMode"),
            ("RecQualityMode", false, "CurrentRecordQualityMode"),
        ],
    ),
    (
        "GetCurrentTransportActions",
        &[
            ("InstanceID", true, "A_ARG_TYPE_InstanceID"),
            ("Actions", false, "CurrentTransportActions"),
        ],
    ),
    ("Stop", &[("InstanceID", true, "A_ARG_TYPE_InstanceID")]),
    (
        "Play",
        &[
            ("InstanceID", true, "A_ARG_TYPE_InstanceID"),
            ("Speed", true, "TransportPlaySpeed"),
        ],
    ),
    ("Pause", &[("InstanceID", true, "A_ARG_TYPE_InstanceID")]),
    (
        "Seek",
        &[
            ("InstanceID", true, "A_ARG_TYPE_InstanceID"),
            ("Unit", true, "A_ARG_TYPE_SeekMode"),
            ("Target", true, "A_ARG_TYPE_SeekTarget"),
        ],
    ),
    ("Next", &[("InstanceID", true, "A_ARG_TYPE_InstanceID")]),
    ("Previous", &[("InstanceID", true, "A_ARG_TYPE_InstanceID")]),
];

const AVT_VARS: Vars = &[
    (
        "TransportState",
        "string",
        false,
        &[
            "STOPPED",
            "PLAYING",
            "PAUSED_PLAYBACK",
            "TRANSITIONING",
            "NO_MEDIA_PRESENT",
        ],
    ),
    (
        "TransportStatus",
        "string",
        false,
        &["OK", "ERROR_OCCURRED"],
    ),
    ("TransportPlaySpeed", "string", false, &["1"]),
    (
        "PlaybackStorageMedium",
        "string",
        false,
        &["NETWORK", "NONE"],
    ),
    ("RecordStorageMedium", "string", false, &["NOT_IMPLEMENTED"]),
    ("PossiblePlaybackStorageMedia", "string", false, &[]),
    ("PossibleRecordStorageMedia", "string", false, &[]),
    (
        "RecordMediumWriteStatus",
        "string",
        false,
        &["NOT_IMPLEMENTED"],
    ),
    ("PossibleRecordQualityModes", "string", false, &[]),
    (
        "CurrentRecordQualityMode",
        "string",
        false,
        &["NOT_IMPLEMENTED"],
    ),
    ("CurrentPlayMode", "string", false, &["NORMAL"]),
    ("NumberOfTracks", "ui4", false, &[]),
    ("CurrentTrack", "ui4", false, &[]),
    ("CurrentTrackDuration", "string", false, &[]),
    ("CurrentMediaDuration", "string", false, &[]),
    ("CurrentTrackMetaData", "string", false, &[]),
    ("CurrentTrackURI", "string", false, &[]),
    ("AVTransportURI", "string", false, &[]),
    ("AVTransportURIMetaData", "string", false, &[]),
    ("NextAVTransportURI", "string", false, &[]),
    ("NextAVTransportURIMetaData", "string", false, &[]),
    ("RelativeTimePosition", "string", false, &[]),
    ("AbsoluteTimePosition", "string", false, &[]),
    ("RelativeCounterPosition", "i4", false, &[]),
    ("AbsoluteCounterPosition", "i4", false, &[]),
    ("CurrentTransportActions", "string", false, &[]),
    ("LastChange", "string", true, &[]),
    (
        "A_ARG_TYPE_SeekMode",
        "string",
        false,
        &["REL_TIME", "ABS_TIME", "TRACK_NR"],
    ),
    ("A_ARG_TYPE_SeekTarget", "string", false, &[]),
    ("A_ARG_TYPE_InstanceID", "ui4", false, &[]),
];

const RCS_ACTIONS: Actions = &[
    (
        "ListPresets",
        &[
            ("InstanceID", true, "A_ARG_TYPE_InstanceID"),
            ("CurrentPresetNameList", false, "PresetNameList"),
        ],
    ),
    (
        "SelectPreset",
        &[
            ("InstanceID", true, "A_ARG_TYPE_InstanceID"),
            ("PresetName", true, "A_ARG_TYPE_PresetName"),
        ],
    ),
    (
        "GetVolume",
        &[
            ("InstanceID", true, "A_ARG_TYPE_InstanceID"),
            ("Channel", true, "A_ARG_TYPE_Channel"),
            ("CurrentVolume", false, "Volume"),
        ],
    ),
    (
        "SetVolume",
        &[
            ("InstanceID", true, "A_ARG_TYPE_InstanceID"),
            ("Channel", true, "A_ARG_TYPE_Channel"),
            ("DesiredVolume", true, "Volume"),
        ],
    ),
    (
        "GetMute",
        &[
            ("InstanceID", true, "A_ARG_TYPE_InstanceID"),
            ("Channel", true, "A_ARG_TYPE_Channel"),
            ("CurrentMute", false, "Mute"),
        ],
    ),
    (
        "SetMute",
        &[
            ("InstanceID", true, "A_ARG_TYPE_InstanceID"),
            ("Channel", true, "A_ARG_TYPE_Channel"),
            ("DesiredMute", true, "Mute"),
        ],
    ),
];

const RCS_VARS: Vars = &[
    ("PresetNameList", "string", false, &[]),
    ("Volume", "ui2", false, &[]),
    ("Mute", "boolean", false, &[]),
    ("LastChange", "string", true, &[]),
    ("A_ARG_TYPE_Channel", "string", false, &["Master"]),
    ("A_ARG_TYPE_InstanceID", "ui4", false, &[]),
    (
        "A_ARG_TYPE_PresetName",
        "string",
        false,
        &["FactoryDefaults"],
    ),
];

const CMS_ACTIONS: Actions = &[
    (
        "GetProtocolInfo",
        &[
            ("Source", false, "SourceProtocolInfo"),
            ("Sink", false, "SinkProtocolInfo"),
        ],
    ),
    (
        "GetCurrentConnectionIDs",
        &[("ConnectionIDs", false, "CurrentConnectionIDs")],
    ),
    (
        "GetCurrentConnectionInfo",
        &[
            ("ConnectionID", true, "A_ARG_TYPE_ConnectionID"),
            ("RcsID", false, "A_ARG_TYPE_RcsID"),
            ("AVTransportID", false, "A_ARG_TYPE_AVTransportID"),
            ("ProtocolInfo", false, "A_ARG_TYPE_ProtocolInfo"),
            (
                "PeerConnectionManager",
                false,
                "A_ARG_TYPE_ConnectionManager",
            ),
            ("PeerConnectionID", false, "A_ARG_TYPE_ConnectionID"),
            ("Direction", false, "A_ARG_TYPE_Direction"),
            ("Status", false, "A_ARG_TYPE_ConnectionStatus"),
        ],
    ),
];

const CMS_VARS: Vars = &[
    ("SourceProtocolInfo", "string", true, &[]),
    ("SinkProtocolInfo", "string", true, &[]),
    ("CurrentConnectionIDs", "string", true, &[]),
    (
        "A_ARG_TYPE_ConnectionStatus",
        "string",
        false,
        &[
            "OK",
            "ContentFormatMismatch",
            "InsufficientBandwidth",
            "UnreliableChannel",
            "Unknown",
        ],
    ),
    ("A_ARG_TYPE_ConnectionManager", "string", false, &[]),
    (
        "A_ARG_TYPE_Direction",
        "string",
        false,
        &["Input", "Output"],
    ),
    ("A_ARG_TYPE_ProtocolInfo", "string", false, &[]),
    ("A_ARG_TYPE_ConnectionID", "i4", false, &[]),
    ("A_ARG_TYPE_AVTransportID", "i4", false, &[]),
    ("A_ARG_TYPE_RcsID", "i4", false, &[]),
];

fn scpd(actions: Actions, vars: Vars) -> String {
    let mut out = String::from("<?xml version=\"1.0\" encoding=\"utf-8\"?>\n<scpd xmlns=\"urn:schemas-upnp-org:service-1-0\"><specVersion><major>1</major><minor>0</minor></specVersion><actionList>");
    for (name, args) in actions {
        out.push_str(&format!("<action><name>{name}</name><argumentList>"));
        for (arg, input, var) in *args {
            out.push_str(&format!(
                "<argument><name>{arg}</name><direction>{}</direction><relatedStateVariable>{var}</relatedStateVariable></argument>",
                if *input { "in" } else { "out" }
            ));
        }
        out.push_str("</argumentList></action>");
    }
    out.push_str("</actionList><serviceStateTable>");
    for (name, ty, evented, allowed) in vars {
        out.push_str(&format!(
            "<stateVariable sendEvents=\"{}\"><name>{name}</name><dataType>{ty}</dataType>",
            if *evented { "yes" } else { "no" }
        ));
        if !allowed.is_empty() {
            out.push_str("<allowedValueList>");
            for v in *allowed {
                out.push_str(&format!("<allowedValue>{v}</allowedValue>"));
            }
            out.push_str("</allowedValueList>");
        }
        if *name == "Volume" {
            out.push_str("<allowedValueRange><minimum>0</minimum><maximum>100</maximum><step>1</step></allowedValueRange>");
        }
        out.push_str("</stateVariable>");
    }
    out.push_str("</serviceStateTable></scpd>");
    out
}

// ---- control ---------------------------------------------------------------------

type Fault = (u32, &'static str);

fn control(
    s: &Shared,
    service: &str,
    action: &str,
    body: &str,
) -> Result<Vec<(&'static str, String)>, Fault> {
    let arg = |name: &str| xml::text(body, name).unwrap_or_default();
    let push = |c: Command| s.commands.lock().unwrap_or_else(|e| e.into_inner()).push(c);
    let st = s.status.lock().unwrap_or_else(|e| e.into_inner()).clone();
    if service != CMS && arg("InstanceID").trim() != "0" {
        return Err((718, "Invalid InstanceID"));
    }
    let actions = "Play,Pause,Stop,Seek".to_string();
    Ok(match (service, action) {
        (AVT, "SetAVTransportURI") | (AVT, "SetNextAVTransportURI") => {
            let next = action == "SetNextAVTransportURI";
            let (uri, didl) = if next {
                (arg("NextURI"), arg("NextURIMetaData"))
            } else {
                (arg("CurrentURI"), arg("CurrentURIMetaData"))
            };
            let uri = uri.trim().to_string();
            if !next && !(uri.starts_with("http://") || uri.starts_with("https://")) {
                return Err((716, "Resource not found"));
            }
            {
                let mut m = s.media.lock().unwrap_or_else(|e| e.into_inner());
                if next {
                    m.next_uri = uri.clone();
                    m.next_didl = didl.clone();
                } else {
                    m.uri = uri.clone();
                    m.didl = didl.clone();
                }
            }
            push(Command::SetUri {
                uri,
                meta: Meta::parse(&didl),
                next,
            });
            s.pending
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .push(AVT);
            vec![]
        }
        (AVT, "Play") => {
            push(Command::Play);
            vec![]
        }
        (AVT, "Pause") => {
            push(Command::Pause);
            vec![]
        }
        (AVT, "Stop") => {
            push(Command::Stop);
            vec![]
        }
        (AVT, "Seek") => {
            let unit = arg("Unit");
            if unit != "REL_TIME" && unit != "ABS_TIME" {
                return Err((710, "Seek mode not supported"));
            }
            let t = crate::parse_time(&arg("Target")).ok_or((711, "Illegal seek target"))?;
            push(Command::Seek(t));
            vec![]
        }
        (AVT, "Next") | (AVT, "Previous") => vec![],
        (AVT, "GetTransportInfo") => vec![
            ("CurrentTransportState", st.state.name().into()),
            ("CurrentTransportStatus", "OK".into()),
            ("CurrentSpeed", "1".into()),
        ],
        (AVT, "GetPositionInfo") => {
            let m = s.media.lock().unwrap_or_else(|e| e.into_inner());
            vec![
                ("Track", if m.uri.is_empty() { "0" } else { "1" }.into()),
                ("TrackDuration", hms(st.duration)),
                ("TrackMetaData", m.didl.clone()),
                ("TrackURI", m.uri.clone()),
                ("RelTime", hms(st.position)),
                ("AbsTime", hms(st.position)),
                ("RelCount", "2147483647".into()),
                ("AbsCount", "2147483647".into()),
            ]
        }
        (AVT, "GetMediaInfo") => {
            let m = s.media.lock().unwrap_or_else(|e| e.into_inner());
            vec![
                ("NrTracks", if m.uri.is_empty() { "0" } else { "1" }.into()),
                ("MediaDuration", hms(st.duration)),
                ("CurrentURI", m.uri.clone()),
                ("CurrentURIMetaData", m.didl.clone()),
                ("NextURI", m.next_uri.clone()),
                ("NextURIMetaData", m.next_didl.clone()),
                ("PlayMedium", "NETWORK".into()),
                ("RecordMedium", "NOT_IMPLEMENTED".into()),
                ("WriteStatus", "NOT_IMPLEMENTED".into()),
            ]
        }
        (AVT, "GetDeviceCapabilities") => vec![
            ("PlayMedia", "NETWORK".into()),
            ("RecMedia", "NOT_IMPLEMENTED".into()),
            ("RecQualityModes", "NOT_IMPLEMENTED".into()),
        ],
        (AVT, "GetTransportSettings") => vec![
            ("PlayMode", "NORMAL".into()),
            ("RecQualityMode", "NOT_IMPLEMENTED".into()),
        ],
        (AVT, "GetCurrentTransportActions") => vec![("Actions", actions)],
        (RCS, "GetVolume") => vec![("CurrentVolume", st.volume.to_string())],
        (RCS, "SetVolume") => {
            let v: u32 = arg("DesiredVolume")
                .trim()
                .parse()
                .map_err(|_| (402, "Invalid Args"))?;
            push(Command::Volume(v.min(100)));
            vec![]
        }
        (RCS, "GetMute") => vec![("CurrentMute", if st.mute { "1" } else { "0" }.into())],
        (RCS, "SetMute") => {
            let v = arg("DesiredMute");
            push(Command::Mute(
                v.trim() == "1" || v.trim().eq_ignore_ascii_case("true"),
            ));
            vec![]
        }
        (RCS, "ListPresets") => vec![("CurrentPresetNameList", "FactoryDefaults".into())],
        (RCS, "SelectPreset") => vec![],
        (CMS, "GetProtocolInfo") => vec![
            ("Source", String::new()),
            ("Sink", s.config.sink_protocols.join(",")),
        ],
        (CMS, "GetCurrentConnectionIDs") => vec![("ConnectionIDs", "0".into())],
        (CMS, "GetCurrentConnectionInfo") => vec![
            ("RcsID", "0".into()),
            ("AVTransportID", "0".into()),
            ("ProtocolInfo", String::new()),
            ("PeerConnectionManager", String::new()),
            ("PeerConnectionID", "-1".into()),
            ("Direction", "Input".into()),
            ("Status", "OK".into()),
        ],
        _ => return Err((401, "Invalid Action")),
    })
}

// ---- events (GENA) ---------------------------------------------------------------

fn subscription(
    s: &Shared,
    conn: &mut TcpStream,
    method: &str,
    service: &'static str,
    headers: &bl_net::http::Headers,
    peer: SocketAddr,
) -> std::io::Result<()> {
    let timeout = headers
        .get("TIMEOUT")
        .and_then(|t| t.trim().strip_prefix("Second-"))
        .and_then(|t| t.parse::<u64>().ok())
        .unwrap_or(1800)
        .clamp(60, 3600);
    let mut subs = s.subs.lock().unwrap_or_else(|e| e.into_inner());
    subs.retain(|x| x.expires > Instant::now());
    if method == "UNSUBSCRIBE" {
        let sid = headers.get("SID").unwrap_or("").trim().to_string();
        let before = subs.len();
        subs.retain(|x| x.sid != sid);
        let removed = subs.len() < before;
        drop(subs);
        let status = if removed {
            "200 OK"
        } else {
            "412 Precondition Failed"
        };
        return respond(conn, status, "text/plain", "", b"");
    }
    if let Some(sid) = headers.get("SID") {
        // Renewal.
        let sid = sid.trim().to_string();
        return match subs.iter_mut().find(|x| x.sid == sid) {
            Some(x) => {
                x.expires = Instant::now() + Duration::from_secs(timeout);
                drop(subs);
                respond(
                    conn,
                    "200 OK",
                    "text/plain",
                    &format!("SID: {sid}\r\nTIMEOUT: Second-{timeout}\r\n"),
                    b"",
                )
            }
            None => {
                drop(subs);
                respond(conn, "412 Precondition Failed", "text/plain", "", b"")
            }
        };
    }
    // CALLBACK: <http://host:port/path><…>; only to the subscriber's own
    // address, so the device cannot be used to send requests elsewhere.
    let callbacks: Vec<String> = headers
        .get("CALLBACK")
        .unwrap_or("")
        .split('>')
        .filter_map(|c| c.trim().strip_prefix('<'))
        .filter(|u| {
            bl_net::client::Url::parse(u).ok().is_some_and(|url| {
                url.scheme == "http" && url.host.parse::<IpAddr>().ok() == Some(peer.ip())
            })
        })
        .map(str::to_string)
        .collect();
    if callbacks.is_empty() || subs.len() >= MAX_SUBSCRIPTIONS {
        drop(subs);
        return respond(conn, "412 Precondition Failed", "text/plain", "", b"");
    }
    let sid = format!("uuid:{}", uuid_like());
    subs.push(Subscription {
        sid: sid.clone(),
        callbacks,
        service,
        expires: Instant::now() + Duration::from_secs(timeout),
        seq: 0,
    });
    drop(subs);
    respond(
        conn,
        "200 OK",
        "text/plain",
        &format!("SID: {sid}\r\nTIMEOUT: Second-{timeout}\r\n"),
        b"",
    )?;
    // The initial event with the full state.
    s.pending
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .push(service);
    Ok(())
}

fn uuid_like() -> String {
    let mut b = [0u8; 16];
    // Unpredictable enough for subscription ids (not a secret).
    let t = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let h = {
        use std::collections::hash_map::RandomState;
        use std::hash::{BuildHasher, Hasher};
        let mut h = RandomState::new().build_hasher();
        h.write_u128(t);
        h.finish()
    };
    b[..8].copy_from_slice(&h.to_le_bytes());
    b[8..].copy_from_slice(&(t as u64).to_le_bytes());
    let x: String = b.iter().map(|v| format!("{v:02x}")).collect();
    format!(
        "{}-{}-{}-{}-{}",
        &x[..8],
        &x[8..12],
        &x[12..16],
        &x[16..20],
        &x[20..]
    )
}

fn last_change(s: &Shared, service: &str) -> String {
    let st = s.status.lock().unwrap_or_else(|e| e.into_inner()).clone();
    let attr = |v: &str| xml::escape(v);
    match service {
        AVT => {
            let m = s.media.lock().unwrap_or_else(|e| e.into_inner());
            format!(
                "<Event xmlns=\"urn:schemas-upnp-org:metadata-1-0/AVT/\"><InstanceID val=\"0\"><TransportState val=\"{}\"/><TransportStatus val=\"OK\"/><TransportPlaySpeed val=\"1\"/><CurrentPlayMode val=\"NORMAL\"/><NumberOfTracks val=\"{}\"/><CurrentTrack val=\"{}\"/><AVTransportURI val=\"{}\"/><AVTransportURIMetaData val=\"{}\"/><CurrentTrackURI val=\"{}\"/><CurrentTrackMetaData val=\"{}\"/><NextAVTransportURI val=\"{}\"/><CurrentTrackDuration val=\"{}\"/><CurrentMediaDuration val=\"{}\"/><CurrentTransportActions val=\"Play,Pause,Stop,Seek\"/><PlaybackStorageMedium val=\"NETWORK\"/></InstanceID></Event>",
                st.state.name(),
                if m.uri.is_empty() { 0 } else { 1 },
                if m.uri.is_empty() { 0 } else { 1 },
                attr(&m.uri),
                attr(&m.didl),
                attr(&m.uri),
                attr(&m.didl),
                attr(&m.next_uri),
                hms(st.duration),
                hms(st.duration),
            )
        }
        _ => format!(
            "<Event xmlns=\"urn:schemas-upnp-org:metadata-1-0/RCS/\"><InstanceID val=\"0\"><Volume channel=\"Master\" val=\"{}\"/><Mute channel=\"Master\" val=\"{}\"/><PresetNameList val=\"FactoryDefaults\"/></InstanceID></Event>",
            st.volume,
            if st.mute { 1 } else { 0 }
        ),
    }
}

fn notify_all(s: &Shared, service: &'static str) {
    let body = if service == CMS {
        format!(
            "<?xml version=\"1.0\" encoding=\"utf-8\"?>\n<e:propertyset xmlns:e=\"urn:schemas-upnp-org:event-1-0\"><e:property><SourceProtocolInfo></SourceProtocolInfo></e:property><e:property><SinkProtocolInfo>{}</SinkProtocolInfo></e:property><e:property><CurrentConnectionIDs>0</CurrentConnectionIDs></e:property></e:propertyset>",
            xml::escape(&s.config.sink_protocols.join(","))
        )
    } else {
        format!(
            "<?xml version=\"1.0\" encoding=\"utf-8\"?>\n<e:propertyset xmlns:e=\"urn:schemas-upnp-org:event-1-0\"><e:property><LastChange>{}</LastChange></e:property></e:propertyset>",
            xml::escape(&last_change(s, service))
        )
    };
    let targets: Vec<(String, Vec<String>, u32)> = {
        let mut subs = s.subs.lock().unwrap_or_else(|e| e.into_inner());
        subs.retain(|x| x.expires > Instant::now());
        subs.iter_mut()
            .filter(|x| x.service == service)
            .map(|x| {
                let seq = x.seq;
                x.seq = x.seq.wrapping_add(1).max(1);
                (x.sid.clone(), x.callbacks.clone(), seq)
            })
            .collect()
    };
    let opts = bl_net::client::Options {
        connect_timeout: Duration::from_secs(2),
        io_timeout: Duration::from_secs(3),
        max_body: 64 * 1024,
        max_redirects: 0,
    };
    for (sid, callbacks, seq) in targets {
        let headers = vec![
            (
                "Content-Type".to_string(),
                "text/xml; charset=\"utf-8\"".to_string(),
            ),
            ("NT".to_string(), "upnp:event".to_string()),
            ("NTS".to_string(), "upnp:propchange".to_string()),
            ("SID".to_string(), sid),
            ("SEQ".to_string(), seq.to_string()),
        ];
        for url in callbacks {
            if bl_net::client::request("NOTIFY", &url, &headers, body.as_bytes(), &opts).is_ok() {
                break;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn didl_metadata() {
        let didl = r#"<DIDL-Lite xmlns="urn:schemas-upnp-org:metadata-1-0/DIDL-Lite/" xmlns:dc="http://purl.org/dc/elements/1.1/" xmlns:upnp="urn:schemas-upnp-org:metadata-1-0/upnp/"><item id="1" parentID="0" restricted="1"><dc:title>Song &amp; Dance</dc:title><dc:creator>Someone</dc:creator><upnp:album>Album</upnp:album><upnp:albumArtURI>http://x/cover.jpg</upnp:albumArtURI><res protocolInfo="http-get:*:audio/flac:*" duration="0:03:25.500">http://x/a.flac</res></item></DIDL-Lite>"#;
        let m = Meta::parse(didl);
        assert_eq!(m.title, "Song & Dance");
        assert_eq!(m.artist, "Someone");
        assert_eq!(m.album, "Album");
        assert_eq!(m.art_url, "http://x/cover.jpg");
        assert_eq!(m.duration, Some(205.5));
        assert_eq!(m.mime, "audio/flac");
    }

    #[test]
    fn service_descriptions_are_consistent() {
        for (actions, vars) in [
            (AVT_ACTIONS, AVT_VARS),
            (RCS_ACTIONS, RCS_VARS),
            (CMS_ACTIONS, CMS_VARS),
        ] {
            for (_, args) in actions {
                for (_, _, var) in *args {
                    assert!(vars.iter().any(|(n, ..)| n == var), "{var} not declared");
                }
            }
        }
        assert_eq!(hms(3725.9), "1:02:05");
    }
}
