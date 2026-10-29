//! SSDP discovery (UPnP Device Architecture 1.1, 1.3.2): an M-SEARCH to
//! the multicast group, answered by unicast to the asking port.

use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4, UdpSocket};
use std::time::{Duration, Instant};

const GROUP: Ipv4Addr = Ipv4Addr::new(239, 255, 255, 250);
const PORT: u16 = 1900;
pub const MEDIA_RENDERER: &str = "urn:schemas-upnp-org:device:MediaRenderer:1";

/// An answer: where the device description is, and the device's USN.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Answer {
    pub location: String,
    pub usn: String,
    pub server: String,
}

/// Searches for devices of type `st` for `wait`.
pub fn discover(st: &str, wait: Duration) -> std::io::Result<Vec<Answer>> {
    let socket = UdpSocket::bind("0.0.0.0:0")?;
    socket.set_multicast_ttl_v4(4)?;
    socket.set_read_timeout(Some(Duration::from_millis(100)))?;
    let group = SocketAddr::V4(SocketAddrV4::new(GROUP, PORT));
    let mx = wait.as_secs().clamp(1, 5);
    let msg = format!(
        "M-SEARCH * HTTP/1.1\r\nHOST: 239.255.255.250:1900\r\nMAN: \"ssdp:discover\"\r\nMX: {mx}\r\nST: {st}\r\nUSER-AGENT: Beatlumen/1.0 UPnP/1.1\r\n\r\n"
    );
    let start = Instant::now();
    let mut sends = [0u64, 300, 1000].into_iter().peekable();
    let mut out: Vec<Answer> = Vec::new();
    let mut buf = [0u8; 4096];
    while start.elapsed() < wait {
        if let Some(&at) = sends.peek() {
            if start.elapsed() >= Duration::from_millis(at) {
                sends.next();
                let _ = socket.send_to(msg.as_bytes(), group);
            }
        }
        let Ok((n, _)) = socket.recv_from(&mut buf) else {
            continue;
        };
        let text = String::from_utf8_lossy(&buf[..n]);
        if !text.starts_with("HTTP/1.1 200") {
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
        let (Some(location), Some(usn)) = (header("LOCATION"), header("USN")) else {
            continue;
        };
        // One answer per device (USN "uuid:…::type").
        let device = usn.split("::").next().unwrap_or(&usn).to_string();
        if out
            .iter()
            .any(|a| a.usn.split("::").next() == Some(device.as_str()))
        {
            continue;
        }
        out.push(Answer {
            location,
            usn,
            server: header("SERVER").unwrap_or_default(),
        });
    }
    Ok(out)
}
