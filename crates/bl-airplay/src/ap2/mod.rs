//! AirPlay 2 audio receiving, next to the AirPlay 1 (RAOP) protocol on the
//! same RTSP port: senders that find the `_airplay._tcp` service pair
//! ([`pairing`], transient with PIN 3939 or with the receiver's password),
//! continue on an encrypted connection ([`cipher`]), describe the stream in
//! binary property lists ([`plist`]) and send buffered audio over TCP
//! ([`buffered`]) or realtime audio over UDP (as AirPlay 1 does, with
//! ChaCha20-Poly1305). Senders that only speak AirPlay 1 keep using RAOP.
//!
//! Not supported: FairPlay (`/fp-setup`), MFi authentication and PTP clock
//! synchronisation; buffered playback follows the sender's start/pause
//! anchors with a fixed delay instead of its clock.

pub mod buffered;
pub mod cipher;
pub mod pairing;
pub mod plist;
pub mod tlv8;

use crate::crypto::{base64_encode, hash::hex, sha512::sha512};
use plist::Value;
use std::io::Read;
use std::net::{IpAddr, TcpListener};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

/// Announced features: AirPlay audio (9), redundant audio (11), FairPlay
/// authentication (14), metadata as artwork, progress and DAAP text
/// (15-17), PCM/ALAC/AAC-LC audio (18-20), unencrypted audio (22), unified
/// advertising info (30), unified media control (38), buffered audio (40),
/// PTP (41), transient pairing (47, 48). The set senders are known to
/// accept from other open receivers.
pub const FEATURES: u64 = 0x0001_8340_405F_CA00;
/// Version of the AirPlay software we answer as (decides the sender's
/// choice of buffered audio and PTP).
pub const SOURCE_VERSION: &str = "366.0";
/// Buffered stream formats (/info `supportedFormats`): AAC-LC 44.1 kHz
/// stereo, which senders transcode everything to.
pub const BUFFER_FORMATS: u64 = 0x40_0000;
/// Realtime stream formats: ALAC 44.1 kHz 16-bit stereo.
pub const REALTIME_FORMATS: u64 = 0x4_0000;
/// Model name shown on senders.
pub const MODEL: &str = "Beatlumen";

/// Status flags: audio output attached (bit 2), password required (7).
pub fn status_flags(password: bool) -> u32 {
    if password {
        0x84
    } else {
        0x04
    }
}

/// "AA:BB:CC:DD:EE:FF" from the device ID.
pub fn device_id(id: &[u8; 6]) -> String {
    id.iter()
        .map(|b| format!("{b:02X}"))
        .collect::<Vec<_>>()
        .join(":")
}

/// A stable UUID string derived from the device ID.
pub fn uuid(label: &str, id: &[u8; 6]) -> String {
    let h = hex(&sha512(&[b"beatlumen-airplay-", label.as_bytes(), id]));
    format!(
        "{}-{}-4{}-a{}-{}",
        &h[0..8],
        &h[8..12],
        &h[13..16],
        &h[17..20],
        &h[20..32]
    )
}

/// What the TXT record and /info describe.
pub struct Description<'a> {
    pub name: &'a str,
    pub id: &'a [u8; 6],
    pub public_key: &'a [u8; 32],
    pub password: bool,
}

impl Description<'_> {
    fn features_txt(&self) -> String {
        format!("0x{:X},0x{:X}", FEATURES & 0xffff_ffff, FEATURES >> 32)
    }

    /// TXT record of the `_airplay._tcp` service.
    pub fn txt(&self) -> Vec<String> {
        let pi = uuid("pi", self.id);
        vec![
            "acl=0".into(),
            format!("deviceid={}", device_id(self.id)),
            format!("fex={}", base64_encode(&FEATURES.to_le_bytes())),
            format!("features={}", self.features_txt()),
            format!("flags=0x{:X}", status_flags(self.password)),
            format!("gid={pi}"),
            "igl=0".into(),
            "gcgl=0".into(),
            format!("model={MODEL}"),
            "protovers=1.1".into(),
            format!("pi={pi}"),
            format!("psi={}", uuid("psi", self.id)),
            format!("pk={}", hex(self.public_key)),
            format!("srcvers={SOURCE_VERSION}"),
            "vv=2".into(),
        ]
    }

    /// The /info response.
    pub fn info(&self, sender: &str, volume_db: f32) -> Value {
        let mut txt = Vec::new();
        for s in self.txt() {
            let b = s.as_bytes();
            txt.push(b.len().min(255) as u8);
            txt.extend_from_slice(&b[..b.len().min(255)]);
        }
        Value::dict([
            ("deviceID", Value::String(device_id(self.id))),
            ("features", Value::Int(FEATURES as i128)),
            (
                "featuresEx",
                Value::String(base64_encode(&FEATURES.to_le_bytes())),
            ),
            (
                "statusFlags",
                Value::Int(status_flags(self.password) as i128),
            ),
            ("name", Value::String(self.name.to_string())),
            ("model", Value::String(MODEL.into())),
            ("manufacturer", Value::String(MODEL.into())),
            ("pi", Value::String(uuid("pi", self.id))),
            ("psi", Value::String(uuid("psi", self.id))),
            ("pk", Value::Data(self.public_key.to_vec())),
            ("protocolVersion", Value::String("1.1".into())),
            ("sourceVersion", Value::String(SOURCE_VERSION.into())),
            ("vv", Value::Int(2)),
            ("senderAddress", Value::String(sender.to_string())),
            ("initialVolume", Value::Real(volume_db as f64)),
            ("keepAliveLowPower", Value::Bool(true)),
            ("keepAliveSendStatsAsBody", Value::Bool(false)),
            ("canRecordScreenStream", Value::Bool(false)),
            ("screenDemoMode", Value::Bool(false)),
            (
                "playbackCapabilities",
                Value::dict([
                    ("supportsInterstitials", Value::Bool(false)),
                    ("supportsFPSSecureStop", Value::Bool(false)),
                    ("supportsUIForAudioOnlyContent", Value::Bool(false)),
                ]),
            ),
            (
                "supportedFormats",
                Value::dict([
                    ("audioStream", Value::Int(REALTIME_FORMATS as i128)),
                    ("bufferStream", Value::Int(BUFFER_FORMATS as i128)),
                ]),
            ),
            ("txtAirPlay", Value::Data(txt)),
        ])
    }
}

/// The event channel: senders connect to it after the first SETUP; what
/// they send (encrypted keep-alives) is read and discarded.
pub struct EventPort {
    pub port: u16,
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl EventPort {
    pub fn start(peer: IpAddr) -> std::io::Result<EventPort> {
        let listener = TcpListener::bind(if peer.is_ipv6() {
            "[::]:0"
        } else {
            "0.0.0.0:0"
        })?;
        listener.set_nonblocking(true)?;
        let port = listener.local_addr()?.port();
        let stop = Arc::new(AtomicBool::new(false));
        let flag = stop.clone();
        let thread = std::thread::Builder::new()
            .name("bl-airplay-events".into())
            .spawn(move || {
                let mut conn = None;
                let mut buf = [0u8; 1024];
                while !flag.load(Ordering::SeqCst) {
                    match &mut conn {
                        None => match listener.accept() {
                            Ok((s, from)) if from.ip() == peer => {
                                let _ = s.set_nonblocking(false);
                                let _ = s.set_read_timeout(Some(Duration::from_millis(200)));
                                conn = Some(s);
                            }
                            _ => std::thread::sleep(Duration::from_millis(50)),
                        },
                        Some(s) => match s.read(&mut buf) {
                            Ok(0) => break,
                            Ok(_) => {}
                            Err(e)
                                if matches!(
                                    e.kind(),
                                    std::io::ErrorKind::WouldBlock
                                        | std::io::ErrorKind::TimedOut
                                        | std::io::ErrorKind::Interrupted
                                ) => {}
                            Err(_) => break,
                        },
                    }
                }
            })?;
        Ok(EventPort {
            port,
            stop,
            thread: Some(thread),
        })
    }
}

impl Drop for EventPort {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn description() {
        let id = [0x02, 0x11, 0x22, 0x33, 0x44, 0x55];
        let pk = [7u8; 32];
        let d = Description {
            name: "Beatlumen",
            id: &id,
            public_key: &pk,
            password: false,
        };
        let txt = d.txt();
        assert!(txt.contains(&"deviceid=02:11:22:33:44:55".to_string()));
        assert!(txt.contains(&"features=0x405FCA00,0x18340".to_string()));
        assert!(txt.contains(&format!("pk={}", "07".repeat(32))));
        assert!(txt.contains(&"flags=0x4".to_string()));
        let pi = uuid("pi", &id);
        assert_eq!(pi.len(), 36);
        assert_eq!(pi, uuid("pi", &id));
        assert_ne!(pi, uuid("psi", &id));
        let info = plist::parse(&plist::write(&d.info("10.0.0.2:5000", -20.0))).unwrap();
        assert_eq!(info.get("features").unwrap().as_u64(), Some(FEATURES));
        assert_eq!(info.get("pk").unwrap().as_data(), Some(&pk[..]));
        let raw = info.get("txtAirPlay").unwrap().as_data().unwrap();
        assert_eq!(raw[0] as usize, "acl=0".len());
    }
}
