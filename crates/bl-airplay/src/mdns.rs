//! A small mDNS/DNS-SD responder (RFC 6762/6763) that announces the
//! receiver's services (AirPlay 1 and 2) in the local network and answers
//! queries for them. Used on desktop systems; Android registers the
//! service through the system (NsdManager) instead.

use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4, UdpSocket};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

const GROUP: Ipv4Addr = Ipv4Addr::new(224, 0, 0, 251);
const PORT: u16 = 5353;
const T_A: u16 = 1;
const T_PTR: u16 = 12;
const T_TXT: u16 = 16;
const T_SRV: u16 = 33;
const T_ANY: u16 = 255;
const CACHE_FLUSH: u16 = 0x8000;

/// A service to announce.
#[derive(Clone, Debug)]
pub struct Service {
    /// Instance name (one DNS label, e.g. "0A1B2C3D4E5F@Beatlumen").
    pub instance: String,
    /// Service type, e.g. "_raop._tcp".
    pub service_type: String,
    pub port: u16,
    /// TXT strings ("key=value").
    pub txt: Vec<String>,
}

pub struct Responder {
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Responder {
    /// Starts announcing `service` with the host name `<host>.local`.
    pub fn start(service: Service, host: String) -> std::io::Result<Responder> {
        Responder::start_all(vec![service], host)
    }

    /// Starts announcing several services of this host (e.g.
    /// [`Receiver::services`](crate::Receiver::services)).
    pub fn start_all(services: Vec<Service>, host: String) -> std::io::Result<Responder> {
        let socket = bl_net::udp::bind_shared_v4(PORT)?;
        socket.join_multicast_v4(&GROUP, &Ipv4Addr::UNSPECIFIED)?;
        socket.set_multicast_loop_v4(true)?;
        socket.set_multicast_ttl_v4(255)?;
        socket.set_read_timeout(Some(Duration::from_millis(250)))?;
        let stop = Arc::new(AtomicBool::new(false));
        let flag = stop.clone();
        let thread = std::thread::Builder::new()
            .name("bl-mdns".into())
            .spawn(move || run(socket, services, host, flag))?;
        Ok(Responder {
            stop,
            thread: Some(thread),
        })
    }
}

impl Drop for Responder {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

/// The address other devices reach this one at (the interface of the
/// default route).
pub fn local_ipv4() -> Option<Ipv4Addr> {
    let s = UdpSocket::bind("0.0.0.0:0").ok()?;
    s.connect(SocketAddrV4::new(GROUP, PORT)).ok()?;
    match s.local_addr().ok()? {
        SocketAddr::V4(a) if !a.ip().is_unspecified() => Some(*a.ip()),
        _ => None,
    }
}

/// Records of one announced service.
struct Svc {
    service: Vec<String>,
    instance: Vec<String>,
    port: u16,
    txt: Vec<String>,
}

impl Svc {
    fn new(s: Service) -> Svc {
        let service = labels(&format!("{}.local", s.service_type));
        let mut instance = vec![s.instance];
        instance.extend(service.iter().cloned());
        Svc {
            service,
            instance,
            port: s.port,
            txt: s.txt,
        }
    }
}

struct Records {
    services: Vec<Svc>,
    host: Vec<String>,
    meta: Vec<String>,
    ip: Option<Ipv4Addr>,
}

fn labels(name: &str) -> Vec<String> {
    name.split('.')
        .filter(|l| !l.is_empty())
        .map(String::from)
        .collect()
}

fn run(socket: UdpSocket, services: Vec<Service>, host: String, stop: Arc<AtomicBool>) {
    let mut rec = Records {
        services: services.into_iter().map(Svc::new).collect(),
        host: labels(&format!("{host}.local")),
        meta: labels("_services._dns-sd._udp.local"),
        ip: local_ipv4(),
    };
    let group = SocketAddr::V4(SocketAddrV4::new(GROUP, PORT));
    // Announce three times (RFC 6762 8.3), then on address changes.
    let start = Instant::now();
    let mut announcements = [0u64, 1, 3].into_iter().peekable();
    let mut last_ip_check = Instant::now();
    let mut buf = [0u8; 4096];
    while !stop.load(Ordering::Relaxed) {
        if let Some(&at) = announcements.peek() {
            if start.elapsed() >= Duration::from_secs(at) {
                announcements.next();
                let _ = socket.send_to(&announcement(&rec, 4500, 120), group);
            }
        }
        if last_ip_check.elapsed() > Duration::from_secs(10) {
            last_ip_check = Instant::now();
            let ip = local_ipv4();
            if ip != rec.ip {
                rec.ip = ip;
                let _ = socket.send_to(&announcement(&rec, 4500, 120), group);
            }
        }
        let Ok((n, from)) = socket.recv_from(&mut buf) else {
            continue;
        };
        if let Some((reply, unicast)) = answer(&rec, &buf[..n], from.port() != PORT) {
            let _ = socket.send_to(&reply, if unicast { from } else { group });
        }
    }
    // Goodbye: the same records with TTL 0.
    let _ = socket.send_to(&announcement(&rec, 0, 0), group);
}

struct Msg {
    out: Vec<u8>,
    answers: u16,
    additional: u16,
}

impl Msg {
    fn new(id: u16) -> Msg {
        let mut out = Vec::with_capacity(512);
        out.extend_from_slice(&id.to_be_bytes());
        out.extend_from_slice(&0x8400u16.to_be_bytes()); // response, authoritative
        out.extend_from_slice(&[0; 8]);
        Msg {
            out,
            answers: 0,
            additional: 0,
        }
    }

    fn name(&mut self, labels: &[String]) {
        for l in labels {
            let b = l.as_bytes();
            let len = b.len().min(63);
            self.out.push(len as u8);
            self.out.extend_from_slice(&b[..len]);
        }
        self.out.push(0);
    }

    fn record(&mut self, name: &[String], ty: u16, class: u16, ttl: u32, data: &[u8]) {
        self.name(name);
        self.out.extend_from_slice(&ty.to_be_bytes());
        self.out.extend_from_slice(&(1 | class).to_be_bytes());
        self.out.extend_from_slice(&ttl.to_be_bytes());
        self.out
            .extend_from_slice(&(data.len() as u16).to_be_bytes());
        self.out.extend_from_slice(data);
    }

    fn finish(mut self) -> Vec<u8> {
        self.out[6..8].copy_from_slice(&self.answers.to_be_bytes());
        self.out[10..12].copy_from_slice(&self.additional.to_be_bytes());
        self.out
    }
}

fn name_bytes(labels: &[String]) -> Vec<u8> {
    let mut m = Msg {
        out: Vec::new(),
        answers: 0,
        additional: 0,
    };
    m.name(labels);
    m.out
}

/// A record; services are referred to by index.
#[derive(Clone, Copy, PartialEq)]
enum Rr {
    Meta(usize),
    Ptr(usize),
    Srv(usize),
    Txt(usize),
    A,
}

fn write_rr(m: &mut Msg, rec: &Records, rr: Rr, long_ttl: u32, short_ttl: u32) -> bool {
    match rr {
        Rr::Meta(i) => {
            let svc = &rec.services[i];
            m.record(&rec.meta, T_PTR, 0, long_ttl, &name_bytes(&svc.service));
        }
        Rr::Ptr(i) => {
            let svc = &rec.services[i];
            m.record(&svc.service, T_PTR, 0, long_ttl, &name_bytes(&svc.instance));
        }
        Rr::Srv(i) => {
            let svc = &rec.services[i];
            let mut d = vec![0, 0, 0, 0];
            d.extend_from_slice(&svc.port.to_be_bytes());
            d.extend(name_bytes(&rec.host));
            m.record(&svc.instance, T_SRV, CACHE_FLUSH, short_ttl, &d);
        }
        Rr::Txt(i) => {
            let svc = &rec.services[i];
            let mut d = Vec::new();
            for s in &svc.txt {
                let b = s.as_bytes();
                let len = b.len().min(255);
                d.push(len as u8);
                d.extend_from_slice(&b[..len]);
            }
            m.record(&svc.instance, T_TXT, CACHE_FLUSH, long_ttl, &d);
        }
        Rr::A => match rec.ip {
            Some(ip) => m.record(&rec.host, T_A, CACHE_FLUSH, short_ttl, &ip.octets()),
            None => return false,
        },
    }
    true
}

fn announcement(rec: &Records, long_ttl: u32, short_ttl: u32) -> Vec<u8> {
    let mut m = Msg::new(0);
    let mut rrs = Vec::new();
    for i in 0..rec.services.len() {
        rrs.extend([Rr::Ptr(i), Rr::Srv(i), Rr::Txt(i)]);
    }
    rrs.push(Rr::A);
    rrs.extend((0..rec.services.len()).map(Rr::Meta));
    for rr in rrs {
        if write_rr(&mut m, rec, rr, long_ttl, short_ttl) {
            m.answers += 1;
        }
    }
    m.finish()
}

/// Reads a (possibly compressed) name at `pos`; returns it and the
/// position after it.
fn read_name(msg: &[u8], mut pos: usize) -> Option<(Vec<String>, usize)> {
    let mut labels = Vec::new();
    let mut end = None;
    let mut jumps = 0;
    loop {
        let len = *msg.get(pos)? as usize;
        if len == 0 {
            pos += 1;
            break;
        }
        if len & 0xc0 == 0xc0 {
            let ptr = (len & 0x3f) << 8 | *msg.get(pos + 1)? as usize;
            end.get_or_insert(pos + 2);
            jumps += 1;
            if jumps > 16 || ptr >= msg.len() {
                return None;
            }
            pos = ptr;
            continue;
        }
        if len > 63 {
            return None;
        }
        let label = msg.get(pos + 1..pos + 1 + len)?;
        labels.push(String::from_utf8_lossy(label).into_owned());
        pos += 1 + len;
        if labels.len() > 32 {
            return None;
        }
    }
    Some((labels, end.unwrap_or(pos)))
}

fn same(a: &[String], b: &[String]) -> bool {
    a.len() == b.len() && a.iter().zip(b).all(|(x, y)| x.eq_ignore_ascii_case(y))
}

/// Builds the answer to a query; the flag tells whether to answer by
/// unicast (legacy queries from other ports, or the "QU" bit).
fn answer(rec: &Records, msg: &[u8], legacy: bool) -> Option<(Vec<u8>, bool)> {
    if msg.len() < 12 || msg[2] & 0x80 != 0 {
        return None; // not a query
    }
    let id = u16::from_be_bytes([msg[0], msg[1]]);
    let qdcount = u16::from_be_bytes([msg[4], msg[5]]);
    let mut pos = 12;
    let mut answers: Vec<Rr> = Vec::new();
    let mut unicast = legacy;
    let mut questions = Vec::new();
    for _ in 0..qdcount.min(32) {
        let (name, next) = read_name(msg, pos)?;
        let ty = u16::from_be_bytes([*msg.get(next)?, *msg.get(next + 1)?]);
        let class = u16::from_be_bytes([*msg.get(next + 2)?, *msg.get(next + 3)?]);
        questions.push(msg[pos..next + 4].to_vec());
        pos = next + 4;
        if class & 0x8000 != 0 {
            unicast = true;
        }
        let any = ty == T_ANY;
        let mut add = |rr: Rr| {
            if !answers.contains(&rr) {
                answers.push(rr);
            }
        };
        for (i, svc) in rec.services.iter().enumerate() {
            if same(&name, &svc.service) && (ty == T_PTR || any) {
                add(Rr::Ptr(i));
            } else if same(&name, &svc.instance) {
                if ty == T_SRV || any {
                    add(Rr::Srv(i));
                }
                if ty == T_TXT || any {
                    add(Rr::Txt(i));
                }
            } else if same(&name, &rec.meta) && (ty == T_PTR || any) {
                add(Rr::Meta(i));
            }
        }
        // No IPv6 address: AAAA queries get no answer.
        if same(&name, &rec.host) && (ty == T_A || any) {
            add(Rr::A);
        }
    }
    if answers.is_empty() {
        return None;
    }
    let mut m = Msg::new(if legacy { id } else { 0 });
    if legacy {
        // Legacy unicast replies repeat the questions.
        m.out[4..6].copy_from_slice(&(questions.len() as u16).to_be_bytes());
        for q in &questions {
            m.out.extend_from_slice(q);
        }
    }
    for &rr in &answers {
        if write_rr(&mut m, rec, rr, 4500, 120) {
            m.answers += 1;
        }
    }
    // Additional records a browser needs to connect.
    let mut extra = Vec::new();
    for &rr in &answers {
        match rr {
            Rr::Ptr(i) => extra.extend([Rr::Srv(i), Rr::Txt(i), Rr::A]),
            Rr::Srv(_) => extra.push(Rr::A),
            _ => {}
        }
    }
    let mut written = answers.clone();
    for rr in extra {
        if !written.contains(&rr) && write_rr(&mut m, rec, rr, 4500, 120) {
            written.push(rr);
            m.additional += 1;
        }
    }
    Some((m.finish(), unicast))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn records() -> Records {
        Records {
            services: vec![
                Svc::new(Service {
                    instance: "0A1B2C3D4E5F@Beatlumen".into(),
                    service_type: "_raop._tcp".into(),
                    port: 7000,
                    txt: vec!["txtvers=1".into(), "cn=0,1".into()],
                }),
                Svc::new(Service {
                    instance: "Beatlumen".into(),
                    service_type: "_airplay._tcp".into(),
                    port: 7000,
                    txt: vec!["deviceid=0A:1B:2C:3D:4E:5F".into()],
                }),
            ],
            host: labels("beatlumen-0a1b.local"),
            meta: labels("_services._dns-sd._udp.local"),
            ip: Some(Ipv4Addr::new(192, 168, 1, 20)),
        }
    }

    fn query(name: &str, ty: u16) -> Vec<u8> {
        let mut q = vec![0x12, 0x34, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0];
        q.extend(name_bytes(&labels(name)));
        q.extend_from_slice(&ty.to_be_bytes());
        q.extend_from_slice(&1u16.to_be_bytes());
        q
    }

    /// Parses the answer records of a response: (name, type, rdata).
    fn records_of(msg: &[u8]) -> Vec<(Vec<String>, u16, Vec<u8>)> {
        let qd = u16::from_be_bytes([msg[4], msg[5]]);
        let an = u16::from_be_bytes([msg[6], msg[7]]) + u16::from_be_bytes([msg[10], msg[11]]);
        let mut pos = 12;
        for _ in 0..qd {
            pos = read_name(msg, pos).unwrap().1 + 4;
        }
        let mut out = Vec::new();
        for _ in 0..an {
            let (name, p) = read_name(msg, pos).unwrap();
            let ty = u16::from_be_bytes([msg[p], msg[p + 1]]);
            let len = u16::from_be_bytes([msg[p + 8], msg[p + 9]]) as usize;
            out.push((name, ty, msg[p + 10..p + 10 + len].to_vec()));
            pos = p + 10 + len;
        }
        out
    }

    #[test]
    fn answers_a_browse_with_everything_needed() {
        let rec = records();
        let (reply, unicast) = answer(&rec, &query("_raop._tcp.local", T_PTR), false).unwrap();
        assert!(!unicast);
        let rrs = records_of(&reply);
        let types: Vec<u16> = rrs.iter().map(|r| r.1).collect();
        assert_eq!(types, vec![T_PTR, T_SRV, T_TXT, T_A]);
        assert_eq!(rrs[3].2, vec![192, 168, 1, 20]);
        assert_eq!(&rrs[1].2[4..6], &7000u16.to_be_bytes());
        // Case-insensitive, and nothing for other services.
        assert!(answer(&rec, &query("_RAOP._TCP.LOCAL", T_PTR), false).is_some());
        assert!(answer(&rec, &query("_googlecast._tcp.local", T_PTR), false).is_none());
        // The AirPlay 2 service of the same host.
        let (reply, _) = answer(&rec, &query("_airplay._tcp.local", T_PTR), false).unwrap();
        let rrs = records_of(&reply);
        assert_eq!(rrs[0].0, labels("_airplay._tcp.local"));
        assert_eq!(rrs[0].2, name_bytes(&rec.services[1].instance));
        assert_eq!(rrs[2].2[1..], *b"deviceid=0A:1B:2C:3D:4E:5F");
        // Service enumeration lists both.
        let (reply, _) =
            answer(&rec, &query("_services._dns-sd._udp.local", T_PTR), false).unwrap();
        assert_eq!(records_of(&reply).len(), 2);
    }

    #[test]
    fn announcement_contains_both_services() {
        let rrs = records_of(&announcement(&records(), 4500, 120));
        let types: Vec<u16> = rrs.iter().map(|r| r.1).collect();
        assert_eq!(
            types,
            vec![T_PTR, T_SRV, T_TXT, T_PTR, T_SRV, T_TXT, T_A, T_PTR, T_PTR]
        );
    }

    #[test]
    fn legacy_queries_get_unicast_replies_with_the_question() {
        let rec = records();
        let (reply, unicast) = answer(&rec, &query("beatlumen-0a1b.local", T_A), true).unwrap();
        assert!(unicast);
        assert_eq!(&reply[..2], &[0x12, 0x34]);
        assert_eq!(u16::from_be_bytes([reply[4], reply[5]]), 1);
    }

    #[test]
    fn malformed_packets_are_ignored() {
        let rec = records();
        assert!(answer(&rec, &[0; 5], false).is_none());
        // Pointer loop.
        let mut q = vec![0, 0, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0xc0, 12, 0, 12, 0, 1];
        assert!(answer(&rec, &q, false).is_none());
        q.truncate(13);
        assert!(answer(&rec, &q, false).is_none());
    }
}

// ---- browsing ----------------------------------------------------------------

/// A service instance found in the local network.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Found {
    /// Instance name (the first label, e.g. "0A1B2C3D4E5F@Kitchen").
    pub instance: String,
    pub addr: Ipv4Addr,
    pub port: u16,
    /// TXT strings ("key=value").
    pub txt: Vec<String>,
}

impl Found {
    /// A TXT value.
    pub fn txt_value(&self, key: &str) -> Option<&str> {
        self.txt.iter().find_map(|t| {
            let (k, v) = t.split_once('=')?;
            k.eq_ignore_ascii_case(key).then_some(v)
        })
    }
}

fn query(questions: &[(Vec<String>, u16)]) -> Vec<u8> {
    let mut out = vec![0u8; 12];
    out[4..6].copy_from_slice(&(questions.len() as u16).to_be_bytes());
    for (name, ty) in questions {
        out.extend(name_bytes(name));
        out.extend_from_slice(&ty.to_be_bytes());
        // Class IN, unicast response wanted ("QU").
        out.extend_from_slice(&0x8001u16.to_be_bytes());
    }
    out
}

#[derive(Default)]
struct Seen {
    instances: Vec<Vec<String>>,
    srv: Vec<(Vec<String>, u16, Vec<String>)>,
    txt: Vec<(Vec<String>, Vec<String>)>,
    a: Vec<(Vec<String>, Ipv4Addr)>,
    /// Instance -> address the answer came from (when no A record follows).
    from: Vec<(Vec<String>, Ipv4Addr)>,
}

fn parse_response(msg: &[u8], service: &[String], from: Ipv4Addr, seen: &mut Seen) -> Option<()> {
    if msg.len() < 12 || msg[2] & 0x80 == 0 {
        return None; // not a response
    }
    let count = |i: usize| u16::from_be_bytes([msg[i], msg[i + 1]]) as usize;
    let (qd, records) = (count(4), count(6) + count(8) + count(10));
    let mut pos = 12;
    for _ in 0..qd.min(32) {
        let (_, next) = read_name(msg, pos)?;
        pos = next + 4;
    }
    for _ in 0..records.min(64) {
        let (name, next) = read_name(msg, pos)?;
        let ty = u16::from_be_bytes([*msg.get(next)?, *msg.get(next + 1)?]);
        let len = u16::from_be_bytes([*msg.get(next + 8)?, *msg.get(next + 9)?]) as usize;
        let start = next + 10;
        let data = msg.get(start..start + len)?;
        pos = start + len;
        match ty {
            T_PTR if same(&name, service) => {
                let (inst, _) = read_name(msg, start)?;
                if !seen.instances.iter().any(|i| same(i, &inst)) {
                    seen.from.push((inst.clone(), from));
                    seen.instances.push(inst);
                }
            }
            T_SRV if len >= 7 => {
                let port = u16::from_be_bytes([data[4], data[5]]);
                let (target, _) = read_name(msg, start + 6)?;
                seen.srv.retain(|(n, _, _)| !same(n, &name));
                seen.srv.push((name, port, target));
            }
            T_TXT => {
                let mut strings = Vec::new();
                let mut i = 0;
                while i < data.len() {
                    let l = data[i] as usize;
                    if let Some(s) = data.get(i + 1..i + 1 + l) {
                        if !s.is_empty() {
                            strings.push(String::from_utf8_lossy(s).into_owned());
                        }
                    }
                    i += 1 + l;
                }
                seen.txt.retain(|(n, _)| !same(n, &name));
                seen.txt.push((name, strings));
            }
            T_A if len == 4 => {
                let ip = Ipv4Addr::new(data[0], data[1], data[2], data[3]);
                seen.a.retain(|(n, _)| !same(n, &name));
                seen.a.push((name, ip));
            }
            _ => {}
        }
    }
    Some(())
}

/// Looks for instances of `service_type` (e.g. "_raop._tcp") in the local
/// network for `wait`. The query goes out from an ordinary port, so
/// responders answer by unicast (RFC 6762 6.7) and port 5353 may be used
/// by the system's own responder.
pub fn browse(service_type: &str, wait: Duration) -> std::io::Result<Vec<Found>> {
    let service = labels(&format!("{service_type}.local"));
    let socket = UdpSocket::bind("0.0.0.0:0")?;
    socket.set_multicast_ttl_v4(255)?;
    socket.set_read_timeout(Some(Duration::from_millis(100)))?;
    let group = SocketAddr::V4(SocketAddrV4::new(GROUP, PORT));
    let start = Instant::now();
    let mut seen = Seen::default();
    let mut sends = [0u64, 400, 1200, 2500].into_iter().peekable();
    let mut buf = [0u8; 9000];
    while start.elapsed() < wait {
        if let Some(&at) = sends.peek() {
            if start.elapsed() >= Duration::from_millis(at) {
                sends.next();
                // The service, plus what is still missing of known instances.
                let mut q = vec![(service.clone(), T_PTR)];
                for inst in &seen.instances {
                    if !seen.srv.iter().any(|(n, _, _)| same(n, inst)) {
                        q.push((inst.clone(), T_SRV));
                    }
                    if !seen.txt.iter().any(|(n, _)| same(n, inst)) {
                        q.push((inst.clone(), T_TXT));
                    }
                }
                let _ = socket.send_to(&query(&q), group);
            }
        }
        if let Ok((n, SocketAddr::V4(from))) = socket.recv_from(&mut buf) {
            parse_response(&buf[..n], &service, *from.ip(), &mut seen);
        }
    }
    let mut out = Vec::new();
    for inst in &seen.instances {
        let Some((_, port, target)) = seen.srv.iter().find(|(n, _, _)| same(n, inst)) else {
            continue;
        };
        let addr = seen
            .a
            .iter()
            .find(|(n, _)| same(n, target))
            .map(|(_, ip)| *ip)
            .or_else(|| {
                seen.from
                    .iter()
                    .find(|(n, _)| same(n, inst))
                    .map(|(_, ip)| *ip)
            });
        let Some(addr) = addr else { continue };
        out.push(Found {
            instance: inst.first().cloned().unwrap_or_default(),
            addr,
            port: *port,
            txt: seen
                .txt
                .iter()
                .find(|(n, _)| same(n, inst))
                .map(|(_, t)| t.clone())
                .unwrap_or_default(),
        });
    }
    Ok(out)
}
