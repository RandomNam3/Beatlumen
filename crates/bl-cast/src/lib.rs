//! A Google Cast sender (CastV2): connects to a Chromecast (or a TV or
//! speaker with Chromecast built in), starts the Default Media Receiver
//! and lets it play a stream URL.
//!
//! The connection is TLS to port 8009; the caller opens it (the platform's
//! TLS) and hands it over. Everything happens on the caller's thread:
//! [`Client::poll`] reads what arrived (reads must time out quickly),
//! answers the receiver's heartbeat and keeps the session and media state.

pub mod proto;

use bl_json::Value;
use proto::Message;
use std::io::{ErrorKind, Read, Write};
use std::time::{Duration, Instant};

/// Google's Default Media Receiver.
pub const DEFAULT_MEDIA_RECEIVER: &str = "CC1AD845";

const NS_CONNECTION: &str = "urn:x-cast:com.google.cast.tp.connection";
const NS_HEARTBEAT: &str = "urn:x-cast:com.google.cast.tp.heartbeat";
const NS_RECEIVER: &str = "urn:x-cast:com.google.cast.receiver";
const NS_MEDIA: &str = "urn:x-cast:com.google.cast.media";
const SENDER: &str = "sender-0";
const PLATFORM: &str = "receiver-0";

#[derive(Debug, Clone, PartialEq)]
pub struct MediaStatus {
    pub session: i64,
    /// "PLAYING", "PAUSED", "BUFFERING", "IDLE"
    pub state: String,
    /// Seconds played of the stream.
    pub time: f64,
    /// When the status arrived.
    pub at: Instant,
    /// Why playback ended ("ERROR", "FINISHED", …) when idle.
    pub idle_reason: String,
}

pub struct Client<S: Read + Write> {
    conn: S,
    buf: Vec<u8>,
    request: u64,
    last_ping: Instant,
    last_heard: Instant,
    /// The running app's session and transport (its "destination").
    pub session: Option<String>,
    transport: Option<String>,
    pub media: Option<MediaStatus>,
    /// The receiver refused a request (LAUNCH_ERROR, LOAD_FAILED, …).
    pub error: Option<String>,
}

fn obj(kind: &str) -> Value {
    Value::object().with("type", kind)
}

impl<S: Read + Write> Client<S> {
    /// Takes an open connection and greets the device.
    pub fn new(conn: S) -> std::io::Result<Client<S>> {
        let mut c = Client {
            conn,
            buf: Vec::new(),
            request: 0,
            last_ping: Instant::now(),
            last_heard: Instant::now(),
            session: None,
            transport: None,
            media: None,
            error: None,
        };
        c.send(
            PLATFORM,
            NS_CONNECTION,
            obj("CONNECT").with("userAgent", "Beatlumen"),
        )?;
        c.request_receiver_status()?;
        Ok(c)
    }

    fn send(&mut self, dest: &str, ns: &str, payload: Value) -> std::io::Result<()> {
        let m = Message {
            source: SENDER.into(),
            destination: dest.into(),
            namespace: ns.into(),
            payload: payload.to_json(),
        };
        self.conn.write_all(&m.encode())?;
        self.conn.flush()
    }

    fn next_request(&mut self) -> u64 {
        self.request += 1;
        self.request
    }

    pub fn request_receiver_status(&mut self) -> std::io::Result<()> {
        let id = self.next_request();
        self.send(
            PLATFORM,
            NS_RECEIVER,
            obj("GET_STATUS").with("requestId", id),
        )
    }

    /// Starts an app (e.g. [`DEFAULT_MEDIA_RECEIVER`]); [`Client::ready`]
    /// once it runs.
    pub fn launch(&mut self, app: &str) -> std::io::Result<()> {
        let id = self.next_request();
        self.send(
            PLATFORM,
            NS_RECEIVER,
            obj("LAUNCH").with("appId", app).with("requestId", id),
        )
    }

    /// The launched app runs and can be talked to.
    pub fn ready(&self) -> bool {
        self.transport.is_some()
    }

    /// Plays `url` (a live stream of `content_type`).
    pub fn load(
        &mut self,
        url: &str,
        content_type: &str,
        title: &str,
        artist: &str,
    ) -> std::io::Result<()> {
        let (Some(t), Some(s)) = (self.transport.clone(), self.session.clone()) else {
            return Err(std::io::Error::other("no receiver app"));
        };
        let id = self.next_request();
        let media = Value::object()
            .with("contentId", url)
            .with("contentType", content_type)
            .with("streamType", "LIVE")
            .with(
                "metadata",
                Value::object()
                    .with("metadataType", 3u64)
                    .with("title", title)
                    .with("artist", artist),
            );
        self.send(
            &t,
            NS_MEDIA,
            obj("LOAD")
                .with("requestId", id)
                .with("sessionId", s)
                .with("media", media)
                .with("autoplay", true),
        )
    }

    fn media_command(&mut self, kind: &str) -> std::io::Result<()> {
        let (Some(t), Some(m)) = (
            self.transport.clone(),
            self.media.as_ref().map(|m| m.session),
        ) else {
            return Ok(());
        };
        let id = self.next_request();
        self.send(
            &t,
            NS_MEDIA,
            obj(kind).with("requestId", id).with("mediaSessionId", m),
        )
    }

    pub fn play(&mut self) -> std::io::Result<()> {
        self.media_command("PLAY")
    }

    pub fn pause(&mut self) -> std::io::Result<()> {
        self.media_command("PAUSE")
    }

    /// Asks for the media status (the answer updates [`Client::media`]).
    pub fn request_media_status(&mut self) -> std::io::Result<()> {
        let Some(t) = self.transport.clone() else {
            return Ok(());
        };
        let id = self.next_request();
        self.send(&t, NS_MEDIA, obj("GET_STATUS").with("requestId", id))
    }

    /// The device's volume, 0..1.
    pub fn set_volume(&mut self, level: f32) -> std::io::Result<()> {
        let id = self.next_request();
        let v = Value::object()
            .with("level", level.clamp(0.0, 1.0) as f64)
            .with("muted", level <= 0.0);
        self.send(
            PLATFORM,
            NS_RECEIVER,
            obj("SET_VOLUME").with("requestId", id).with("volume", v),
        )
    }

    /// Ends the app on the device.
    pub fn stop(&mut self) -> std::io::Result<()> {
        let Some(s) = self.session.clone() else {
            return Ok(());
        };
        let id = self.next_request();
        let r = self.send(
            PLATFORM,
            NS_RECEIVER,
            obj("STOP").with("requestId", id).with("sessionId", s),
        );
        if let Some(t) = self.transport.take() {
            let _ = self.send(&t, NS_CONNECTION, obj("CLOSE"));
        }
        let _ = self.send(PLATFORM, NS_CONNECTION, obj("CLOSE"));
        r
    }

    /// Reads what arrived (the connection's read timeout bounds the wait),
    /// answers heartbeats and updates the state. Errors when the
    /// connection is gone or the device stopped answering.
    pub fn poll(&mut self) -> std::io::Result<()> {
        let mut chunk = [0u8; 8192];
        loop {
            match self.conn.read(&mut chunk) {
                Ok(0) => {
                    return Err(std::io::Error::new(
                        ErrorKind::UnexpectedEof,
                        "the device closed the connection",
                    ))
                }
                Ok(n) => {
                    self.buf.extend_from_slice(&chunk[..n]);
                    if n < chunk.len() {
                        break;
                    }
                }
                Err(e) if matches!(e.kind(), ErrorKind::TimedOut | ErrorKind::WouldBlock) => break,
                Err(e) => return Err(e),
            }
        }
        let messages = proto::take_messages(&mut self.buf).map_err(std::io::Error::other)?;
        for m in messages {
            self.last_heard = Instant::now();
            self.handle(m)?;
        }
        if self.last_ping.elapsed() > Duration::from_secs(5) {
            self.last_ping = Instant::now();
            self.send(PLATFORM, NS_HEARTBEAT, obj("PING"))?;
        }
        if self.last_heard.elapsed() > Duration::from_secs(20) {
            return Err(std::io::Error::new(
                ErrorKind::TimedOut,
                "the device stopped answering",
            ));
        }
        Ok(())
    }

    fn handle(&mut self, m: Message) -> std::io::Result<()> {
        let Ok(v) = bl_json::parse(&m.payload) else {
            return Ok(());
        };
        match (m.namespace.as_str(), v.str_or("type", "")) {
            (NS_HEARTBEAT, "PING") => self.send(&m.source, NS_HEARTBEAT, obj("PONG"))?,
            (NS_CONNECTION, "CLOSE") if Some(&m.source) == self.transport.as_ref() => {
                self.transport = None;
                self.session = None;
                self.error
                    .get_or_insert_with(|| "the receiver app ended".into());
            }
            (NS_RECEIVER, "RECEIVER_STATUS") => {
                let app = v
                    .get("status")
                    .get("applications")
                    .as_array()
                    .and_then(|apps| {
                        apps.iter()
                            .find(|a| a.str_or("appId", "") == DEFAULT_MEDIA_RECEIVER)
                            .cloned()
                    });
                match app {
                    Some(a) => {
                        let transport = a.str_or("transportId", "").to_string();
                        if self.transport.as_deref() != Some(transport.as_str())
                            && !transport.is_empty()
                        {
                            self.send(
                                &transport,
                                NS_CONNECTION,
                                obj("CONNECT").with("userAgent", "Beatlumen"),
                            )?;
                            self.transport = Some(transport);
                        }
                        self.session = Some(a.str_or("sessionId", "").to_string());
                    }
                    None if self.transport.is_some() => {
                        // Someone else took over the device.
                        self.transport = None;
                        self.session = None;
                        self.error
                            .get_or_insert_with(|| "another app took over the device".into());
                    }
                    None => {}
                }
            }
            (NS_RECEIVER, "LAUNCH_ERROR") => {
                self.error = Some(format!("launch failed: {}", v.str_or("reason", "unknown")));
            }
            (NS_MEDIA, "MEDIA_STATUS") => {
                if let Some(s) = v.get("status").as_array().and_then(|a| a.first()) {
                    let session = s.get("mediaSessionId").as_f64().map(|x| x as i64);
                    if let Some(session) = session.or(self.media.as_ref().map(|m| m.session)) {
                        self.media = Some(MediaStatus {
                            session,
                            state: s.str_or("playerState", "").to_string(),
                            time: s.f64_or("currentTime", 0.0),
                            at: Instant::now(),
                            idle_reason: s.str_or("idleReason", "").to_string(),
                        });
                    }
                }
            }
            (NS_MEDIA, "LOAD_FAILED" | "LOAD_CANCELLED" | "INVALID_REQUEST") => {
                self.error = Some(format!(
                    "the device could not play the stream ({})",
                    v.str_or("type", "")
                ));
            }
            _ => {}
        }
        Ok(())
    }
}
