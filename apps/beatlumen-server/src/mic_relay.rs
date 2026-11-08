//! Phones as wireless microphones for the karaoke mode: a phone sends its
//! singing (Opus packets, each stamped with the server's clock when it was
//! recorded) to the server for a device of the same login, which fetches
//! it with long polling. Nobody opens a port; the server keeps the last
//! few seconds in memory per target device and stores nothing.
//!
//! * `POST /api/mic/<target>?from=<device>`: packets, each `[u64 LE
//!   recorded at, µs on the server's clock][u16 LE length][Opus packet]`
//!   → `{"now": µs}` (for the phone's clock).
//! * `GET /api/mic/<device>?after=<seq>`: waits up to 2 s for packets
//!   after `seq`, each `[u64 seq][u64 recorded at][u8 length][sender
//!   id][u16 length][packet]`; an empty body when none came.
//! * `GET /api/mic/time` → `{"now": µs}`.

use bl_json::Value;
use bl_net::server::{Request, Response};
use std::collections::{HashMap, VecDeque};
use std::sync::{Condvar, Mutex};
use std::time::{Duration, Instant};

/// Packets kept per target device (seconds).
const KEEP: Duration = Duration::from_secs(3);
/// Longest wait of a fetch.
const WAIT: Duration = Duration::from_secs(2);
/// Largest Opus packet and request accepted.
const MAX_PACKET: usize = 1500;
const MAX_BODY: usize = 64 * 1024;
/// Most phones sending to one device.
const MAX_SENDERS: usize = 8;

struct Packet {
    seq: u64,
    from: String,
    at_us: u64,
    data: Vec<u8>,
    came: Instant,
}

#[derive(Default)]
struct Target {
    packets: VecDeque<Packet>,
}

#[derive(Default)]
struct Inner {
    targets: HashMap<String, Target>,
    seq: u64,
}

#[derive(Default)]
pub struct MicRelay {
    inner: Mutex<Inner>,
    cv: Condvar,
}

/// Microseconds since the Unix epoch.
pub fn now_us() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_micros() as u64)
}

/// Splits a phone's request body into (recorded at, packet).
pub fn parse_packets(body: &[u8]) -> Result<Vec<(u64, &[u8])>, &'static str> {
    let mut out = Vec::new();
    let mut at = 0;
    while at < body.len() {
        if body.len() < at + 10 {
            return Err("truncated packet");
        }
        let ts = u64::from_le_bytes(body[at..at + 8].try_into().expect("8 bytes"));
        let len = u16::from_le_bytes([body[at + 8], body[at + 9]]) as usize;
        at += 10;
        if len == 0 || len > MAX_PACKET || body.len() < at + len {
            return Err("bad packet length");
        }
        out.push((ts, &body[at..at + len]));
        at += len;
    }
    Ok(out)
}

impl MicRelay {
    /// `/api/mic/...` for a login of `scope`; `scope_of` gives a device's
    /// scope (None: unknown device).
    pub fn handle(
        &self,
        req: &Request,
        segs: &[&str],
        scope: &str,
        scope_of: &dyn Fn(&str) -> Option<String>,
    ) -> Response {
        let same = |id: &str| scope_of(id).is_some_and(|s| s == scope);
        match (req.method.as_str(), segs) {
            ("GET", ["mic", "time"]) => Response::json(200, &Value::object().with("now", now_us())),
            ("POST", ["mic", target]) => {
                let Some(from) = req.query("from") else {
                    return Response::error(400, "no sending device");
                };
                if !same(target) || !same(from) || from == *target {
                    return Response::error(404, "no such device");
                }
                if req.body.len() > MAX_BODY {
                    return Response::error(413, "too much at once");
                }
                let packets = match parse_packets(&req.body) {
                    Ok(p) => p,
                    Err(e) => return Response::error(400, e),
                };
                let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
                let mut seq = inner.seq;
                let t = inner.targets.entry(target.to_string()).or_default();
                while t.packets.front().is_some_and(|p| p.came.elapsed() > KEEP) {
                    t.packets.pop_front();
                }
                let senders: std::collections::HashSet<&str> =
                    t.packets.iter().map(|p| p.from.as_str()).collect();
                if !senders.contains(from) && senders.len() >= MAX_SENDERS {
                    return Response::error(429, "too many phones for this device");
                }
                for (at_us, data) in packets {
                    seq += 1;
                    t.packets.push_back(Packet {
                        seq,
                        from: from.to_string(),
                        at_us,
                        data: data.to_vec(),
                        came: Instant::now(),
                    });
                }
                inner.seq = seq;
                drop(inner);
                self.cv.notify_all();
                Response::json(200, &Value::object().with("now", now_us()))
            }
            ("GET", ["mic", device]) => {
                if !same(device) {
                    return Response::error(404, "no such device");
                }
                let after: u64 = req.query("after").and_then(|v| v.parse().ok()).unwrap_or(0);
                let start = Instant::now();
                let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
                loop {
                    let newest = inner.seq;
                    let fresh: Vec<&Packet> = inner
                        .targets
                        .get(*device)
                        .map(|t| {
                            t.packets
                                .iter()
                                .filter(|p| p.seq > after && p.came.elapsed() <= KEEP)
                                .collect()
                        })
                        .unwrap_or_default();
                    if !fresh.is_empty() {
                        let mut out = Vec::new();
                        for p in fresh {
                            out.extend_from_slice(&p.seq.to_le_bytes());
                            out.extend_from_slice(&p.at_us.to_le_bytes());
                            out.push(p.from.len().min(255) as u8);
                            out.extend_from_slice(&p.from.as_bytes()[..p.from.len().min(255)]);
                            out.extend_from_slice(&(p.data.len() as u16).to_le_bytes());
                            out.extend_from_slice(&p.data);
                        }
                        return Response::bytes(200, "application/octet-stream", out)
                            .with_header("Cache-Control", "no-store");
                    }
                    // A fetch from before a restart starts over.
                    if after > newest {
                        return Response::bytes(200, "application/octet-stream", Vec::new())
                            .with_header("X-Mic-Seq", newest.to_string());
                    }
                    let left = WAIT.saturating_sub(start.elapsed());
                    if left.is_zero() {
                        return Response::bytes(200, "application/octet-stream", Vec::new());
                    }
                    inner = self
                        .cv
                        .wait_timeout(inner, left)
                        .unwrap_or_else(|e| e.into_inner())
                        .0;
                }
            }
            _ => Response::error(405, "method not allowed"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn packets_are_read() {
        let mut b = Vec::new();
        for (ts, p) in [(5u64, &b"abc"[..]), (7, &b"de"[..])] {
            b.extend_from_slice(&ts.to_le_bytes());
            b.extend_from_slice(&(p.len() as u16).to_le_bytes());
            b.extend_from_slice(p);
        }
        let p = parse_packets(&b).unwrap();
        assert_eq!(p, vec![(5, &b"abc"[..]), (7, &b"de"[..])]);
        assert!(parse_packets(&b[..b.len() - 1]).is_err());
        assert!(parse_packets(&[0; 10]).is_err());
    }
}
