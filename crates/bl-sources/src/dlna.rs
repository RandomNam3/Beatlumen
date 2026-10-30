//! DLNA / UPnP AV media servers (Fritz!Box, NAS systems, media servers on
//! TVs and receivers): the audio items of the server's ContentDirectory,
//! browsed container by container, with their metadata, stream addresses
//! and album art. Servers are found with SSDP only while the user searches
//! ([`discover`]); an address can also be entered.
//!
//! All addresses the server names (its control address, streams and album
//! art) must be on the same host and port, with the same scheme, as its
//! device description; items elsewhere are left out, so a media server
//! cannot send requests to other machines (or this one).

use crate::{MusicSource, RemoteTrack, SourceConfig};
use bl_net::client::{self, HttpStream, Options, Url};
use bl_upnp::{xml, Renderer};
use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr, SocketAddr, SocketAddrV4, UdpSocket};
use std::sync::Mutex;
use std::time::{Duration, Instant};

const CONTENT_DIRECTORY: &str = "urn:schemas-upnp-org:service:ContentDirectory:1";
pub const MEDIA_SERVER: &str = "urn:schemas-upnp-org:device:MediaServer:1";
const PAGE: usize = 200;
const MAX_CONTAINERS: usize = 20_000;
const MAX_ITEMS: usize = 200_000;
const MAX_DEPTH: usize = 24;
/// Pages read of one container.
const MAX_PAGES: usize = MAX_ITEMS / PAGE;
/// Browse requests of one listing.
const MAX_CALLS: usize = 2 * MAX_CONTAINERS;
/// Search answers (distinct device descriptions) looked at.
const MAX_ANSWERS: usize = 32;
/// Largest device description read while searching.
const MAX_FOUND_DESCRIPTION: usize = 64 << 10;

/// A media server found in the network.
#[derive(Clone, Debug, PartialEq)]
pub struct Found {
    pub name: String,
    pub location: String,
}

/// Searches the local network for media servers for `wait`.
pub fn discover(wait: Duration) -> Vec<Found> {
    let group = SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::new(239, 255, 255, 250), 1900));
    discover_at(group, wait)
}

/// [`discover`] with the search sent to `target`.
fn discover_at(target: SocketAddr, wait: Duration) -> Vec<Found> {
    let opts = Options {
        connect_timeout: Duration::from_secs(3),
        io_timeout: Duration::from_secs(5),
        max_body: MAX_FOUND_DESCRIPTION,
        max_redirects: 0,
    };
    let mut out = Vec::new();
    for location in search(target, wait).unwrap_or_default() {
        if let Ok(r) = client::request("GET", &location, &[], &[], &opts) {
            let text = String::from_utf8_lossy(&r.body);
            if r.status == 200 && text.contains(CONTENT_DIRECTORY) {
                out.push(Found {
                    name: xml::text(&text, "friendlyName").unwrap_or_else(|| location.clone()),
                    location,
                });
            }
        }
    }
    out
}

/// The distinct device description addresses of the media servers
/// answering an SSDP search sent to `target` within `wait` (at most
/// [`MAX_ANSWERS`]).
fn search(target: SocketAddr, wait: Duration) -> std::io::Result<Vec<String>> {
    let socket = UdpSocket::bind("0.0.0.0:0")?;
    socket.set_multicast_ttl_v4(4)?;
    socket.set_read_timeout(Some(Duration::from_millis(100)))?;
    let mx = wait.as_secs().clamp(1, 5);
    let msg = format!(
        "M-SEARCH * HTTP/1.1\r\nHOST: 239.255.255.250:1900\r\nMAN: \"ssdp:discover\"\r\nMX: {mx}\r\nST: {MEDIA_SERVER}\r\nUSER-AGENT: Beatlumen/1.0 UPnP/1.1\r\n\r\n"
    );
    let start = Instant::now();
    let mut sends = [0u64, 300, 1000].into_iter().peekable();
    let mut out: Vec<String> = Vec::new();
    let mut buf = [0u8; 4096];
    while start.elapsed() < wait && out.len() < MAX_ANSWERS {
        if let Some(&at) = sends.peek() {
            if start.elapsed() >= Duration::from_millis(at) {
                sends.next();
                let _ = socket.send_to(msg.as_bytes(), target);
            }
        }
        let Ok((n, peer)) = socket.recv_from(&mut buf) else {
            continue;
        };
        if let Some(location) = answer_location(&String::from_utf8_lossy(&buf[..n]), peer.ip()) {
            if !out.contains(&location) {
                out.push(location);
            }
        }
    }
    Ok(out)
}

/// The `LOCATION` of a search answer from `peer`: only an http:// address
/// on the peer itself, so an answer cannot point the search elsewhere.
fn answer_location(text: &str, peer: IpAddr) -> Option<String> {
    if !text.starts_with("HTTP/1.1 200") {
        return None;
    }
    let location = text.lines().find_map(|l| {
        let (k, v) = l.split_once(':')?;
        k.trim()
            .eq_ignore_ascii_case("location")
            .then(|| v.trim().to_string())
    })?;
    let u = Url::parse(&location).ok()?;
    let host = u.host.parse::<IpAddr>().ok()?;
    (u.scheme == "http" && host == peer).then_some(location)
}

/// Whether `url` is on the same host, with the same scheme, as `origin`.
fn same_origin(origin: &Url, url: &str) -> bool {
    Url::parse(url).is_ok_and(|u| {
        // Any port: some servers send the files from a second one.
        u.scheme == origin.scheme && u.host.eq_ignore_ascii_case(&origin.host)
    })
}

struct Item {
    url: String,
    art: Option<String>,
}

pub struct Dlna {
    /// The device description's address: every other address must be on
    /// its host.
    origin: Url,
    control: String,
    /// Item id -> stream and album art (from the listing).
    items: Mutex<HashMap<String, Item>>,
}

/// Seconds of a DIDL duration (`h:mm:ss.fff`).
fn duration_ms(s: &str) -> u64 {
    bl_upnp::parse_time(s).map_or(0, |t| (t * 1000.0) as u64)
}

impl Dlna {
    pub fn connect(cfg: &SourceConfig) -> Result<Dlna, String> {
        let origin = Url::parse(&cfg.url)?;
        let r = client::request("GET", &cfg.url, &[], &[], &crate::opts(crate::MAX_SMALL))?;
        if r.status != 200 {
            return Err(format!("HTTP {}", r.status));
        }
        let text = String::from_utf8_lossy(&r.body).into_owned();
        // A URLBase on another host is ignored.
        let base = xml::text(&text, "URLBase")
            .filter(|u| same_origin(&origin, u))
            .and_then(|u| Url::parse(&u).ok())
            .unwrap_or_else(|| origin.clone());
        let control = xml::elements(&text, "service")
            .into_iter()
            .find(|s| {
                xml::text(s, "serviceType").is_some_and(|t| {
                    t.starts_with("urn:schemas-upnp-org:service:ContentDirectory:")
                })
            })
            .and_then(|s| xml::text(s, "controlURL"))
            .map(|u| base.join(&u))
            .ok_or("not a media server (no ContentDirectory)")?;
        if !same_origin(&origin, &control) {
            return Err("the media server's control address is on another host".into());
        }
        Ok(Dlna {
            origin,
            control,
            items: Mutex::new(HashMap::new()),
        })
    }

    /// A stream or album art address (unescaped) of the listing, resolved
    /// against the device description; None when it is on another host.
    fn own_url(&self, url: &str) -> Option<String> {
        let url = self.origin.join(url.trim());
        same_origin(&self.origin, &url).then_some(url)
    }

    /// One page of a container's children: (DIDL-Lite, number returned,
    /// total).
    fn browse(&self, id: &str, start: usize) -> Result<(String, usize, usize), String> {
        let start_s = start.to_string();
        let count = PAGE.to_string();
        let answer = Renderer::soap(
            &self.control,
            CONTENT_DIRECTORY,
            "Browse",
            &[
                ("ObjectID", id),
                ("BrowseFlag", "BrowseDirectChildren"),
                ("Filter", "*"),
                ("StartingIndex", &start_s),
                ("RequestedCount", &count),
                ("SortCriteria", ""),
            ],
        )?;
        let didl = xml::text(&answer, "Result").unwrap_or_default();
        let returned = xml::text(&answer, "NumberReturned")
            .and_then(|n| n.parse().ok())
            .unwrap_or(0);
        let total = xml::text(&answer, "TotalMatches")
            .and_then(|n| n.parse().ok())
            .unwrap_or(0);
        Ok((didl, returned, total))
    }

    fn item(&self, el: &str, attrs: &str) -> Option<RemoteTrack> {
        let class = xml::text(el, "class").unwrap_or_default();
        if !class.starts_with("object.item.audioItem") {
            return None;
        }
        let id = attr(attrs, "id")?;
        // The first resource with an http address on the server itself.
        let (url, ra) = xml::elements(el, "res")
            .into_iter()
            .zip(res_attrs(el))
            .find_map(|(r, a)| {
                let r = xml::unescape(r.trim());
                if r.is_empty() {
                    return None;
                }
                Some((self.own_url(&r)?, a))
            })?;
        let protocol = attr(&ra, "protocolInfo").unwrap_or_default();
        let mime = protocol.split(':').nth(2).unwrap_or("");
        let ext = match mime {
            "audio/mpeg" => "mp3",
            "audio/flac" | "audio/x-flac" => "flac",
            "audio/mp4" | "audio/x-m4a" => "m4a",
            "audio/ogg" | "application/ogg" => "ogg",
            "audio/wav" | "audio/x-wav" | "audio/wave" => "wav",
            "audio/aac" => "aac",
            _ => "",
        };
        // Album art elsewhere is left out (the song stays).
        let art = xml::text(el, "albumArtURI").and_then(|a| self.own_url(&a));
        let track = RemoteTrack {
            key: id.clone(),
            title: xml::text(el, "title").unwrap_or_default(),
            artists: xml::text(el, "artist")
                .or_else(|| xml::text(el, "creator"))
                .into_iter()
                .collect(),
            album: xml::text(el, "album").unwrap_or_default(),
            album_artist: String::new(),
            track: xml::text(el, "originalTrackNumber")
                .and_then(|n| n.parse().ok())
                .unwrap_or(0),
            disc: 0,
            year: xml::text(el, "date")
                .and_then(|d| d.get(..4)?.parse().ok())
                .unwrap_or(0),
            genre: xml::text(el, "genre").unwrap_or_default(),
            duration_ms: attr(&ra, "duration").map_or(0, |d| duration_ms(&d)),
            size: attr(&ra, "size").and_then(|s| s.parse().ok()).unwrap_or(0),
            modified: 0,
            ext: if ext.is_empty() {
                crate::ext_of(&url)
            } else {
                ext.to_string()
            },
            has_cover: art.is_some(),
        };
        self.items
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(id, Item { url, art });
        Some(track)
    }
}

/// An attribute of an element's start tag (`name="value"`).
fn attr(attrs: &str, name: &str) -> Option<String> {
    let pat = format!("{name}=\"");
    let mut from = 0;
    while let Some(i) = attrs[from..].find(&pat) {
        let at = from + i;
        // A whole attribute name, not the end of a longer one.
        if at == 0 || attrs.as_bytes()[at - 1].is_ascii_whitespace() {
            let rest = &attrs[at + pat.len()..];
            return rest.find('"').map(|j| xml::unescape(&rest[..j]));
        }
        from = at + pat.len();
    }
    None
}

/// The start tags (attributes) of the `<res …>` elements of an item.
fn res_attrs(el: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut from = 0;
    while let Some(i) = el[from..].find("<res") {
        let start = from + i + 4;
        let Some(end) = el[start..].find('>') else {
            break;
        };
        out.push(el[start..start + end].to_string());
        from = start + end;
    }
    out
}

/// The start tag attributes and content of each `<tag …>` in `didl`.
fn children<'a>(didl: &'a str, tag: &str) -> Vec<(String, &'a str)> {
    let contents = xml::elements(didl, tag);
    let mut starts = Vec::new();
    let mut from = 0;
    let open = format!("<{tag}");
    while let Some(i) = didl[from..].find(&open) {
        let start = from + i + open.len();
        let Some(end) = didl[start..].find('>') else {
            break;
        };
        starts.push(didl[start..start + end].to_string());
        from = start + end;
    }
    starts.into_iter().zip(contents).collect()
}

impl MusicSource for Dlna {
    fn list(
        &self,
        _known: crate::Known,
        progress: &mut dyn FnMut(usize) -> bool,
    ) -> Result<Vec<RemoteTrack>, String> {
        self.walk(&mut |id, start| self.browse(id, start), progress)
    }

    fn open(&self, key: &str) -> Result<Box<dyn bl_media::Source>, String> {
        let url = self
            .items
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(key)
            .map(|i| i.url.clone())
            .ok_or("unknown item (list the server first)")?;
        Ok(Box::new(HttpStream::open(&url, &[])?))
    }

    fn cover(&self, key: &str, _size: u32) -> Option<Vec<u8>> {
        let art = self
            .items
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(key)?
            .art
            .clone()?;
        let r = client::request("GET", &art, &[], &[], &crate::opts(crate::MAX_COVER)).ok()?;
        (r.status == 200).then_some(r.body)
    }
}

/// One page of a container's children: (DIDL-Lite, number returned,
/// total), as [`Dlna::browse`] gives it.
type Browse<'a> = dyn FnMut(&str, usize) -> Result<(String, usize, usize), String> + 'a;

impl Dlna {
    /// The audio items below the root, container by container, with
    /// `browse` reading the pages. Bounded however the server answers:
    /// containers, depth, pages per container and Browse requests.
    fn walk(
        &self,
        browse: &mut Browse,
        progress: &mut dyn FnMut(usize) -> bool,
    ) -> Result<Vec<RemoteTrack>, String> {
        let mut out = Vec::new();
        let mut stack = vec![("0".to_string(), 0usize)];
        // Containers taken from the stack and Browse requests sent.
        let (mut containers, mut calls) = (0usize, 0usize);
        while let Some((id, depth)) = stack.pop() {
            containers += 1;
            let mut start = 0usize;
            for _ in 0..MAX_PAGES {
                if calls >= MAX_CALLS {
                    return Ok(out);
                }
                calls += 1;
                let (didl, returned, total) = match browse(&id, start) {
                    Ok(r) => r,
                    Err(e) if id == "0" => return Err(e),
                    Err(_) => break,
                };
                for (attrs, _) in children(&didl, "container") {
                    // Taken and waiting containers together stay bounded.
                    if depth >= MAX_DEPTH || containers + stack.len() >= MAX_CONTAINERS {
                        break;
                    }
                    if let Some(cid) = attr(&attrs, "id") {
                        stack.push((cid, depth + 1));
                    }
                }
                for (attrs, el) in children(&didl, "item") {
                    out.extend(self.item(el, &attrs));
                    if out.len() >= MAX_ITEMS || !progress(out.len()) {
                        return Ok(out);
                    }
                }
                start = start.saturating_add(returned);
                if returned == 0 || start >= total {
                    break;
                }
            }
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dlna(origin: &str) -> Dlna {
        Dlna {
            origin: Url::parse(origin).unwrap(),
            control: String::new(),
            items: Mutex::new(HashMap::new()),
        }
    }

    #[test]
    fn didl_items() {
        let didl = r#"<DIDL-Lite xmlns="urn:schemas-upnp-org:metadata-1-0/DIDL-Lite/" xmlns:dc="http://purl.org/dc/elements/1.1/" xmlns:upnp="urn:schemas-upnp-org:metadata-1-0/upnp/">
<container id="c1" parentID="0" restricted="1"><dc:title>Music</dc:title><upnp:class>object.container</upnp:class></container>
<item id="i1" parentID="c1" restricted="1"><dc:title>Song &amp; Dance</dc:title><upnp:artist>Band</upnp:artist><upnp:album>Album</upnp:album><upnp:originalTrackNumber>3</upnp:originalTrackNumber><upnp:class>object.item.audioItem.musicTrack</upnp:class><upnp:albumArtURI>http://h/art.jpg</upnp:albumArtURI><res protocolInfo="http-get:*:audio/flac:*" size="1234" duration="0:03:25.500">http://h/song.flac</res></item>
<item id="v1" parentID="c1"><dc:title>Clip</dc:title><upnp:class>object.item.videoItem</upnp:class><res protocolInfo="http-get:*:video/mp4:*">http://h/v.mp4</res></item>
</DIDL-Lite>"#;
        let d = dlna("http://h/desc.xml");
        let cs = children(didl, "container");
        assert_eq!(attr(&cs[0].0, "id").as_deref(), Some("c1"));
        let tracks: Vec<RemoteTrack> = children(didl, "item")
            .into_iter()
            .filter_map(|(a, e)| d.item(e, &a))
            .collect();
        assert_eq!(tracks.len(), 1);
        let t = &tracks[0];
        assert_eq!(
            (
                t.key.as_str(),
                t.title.as_str(),
                t.track,
                t.size,
                t.duration_ms
            ),
            ("i1", "Song & Dance", 3, 1234, 205_500)
        );
        assert_eq!(
            (t.ext.as_str(), t.artists.clone(), t.has_cover),
            ("flac", vec!["Band".to_string()], true)
        );
        assert_eq!(d.items.lock().unwrap()["i1"].url, "http://h/song.flac");
        // "parentID" is not "id".
        assert_eq!(attr(r#"parentID="p" id="x""#, "id").as_deref(), Some("x"));
    }

    /// Streams and album art must be on the device description's host.
    #[test]
    fn addresses_stay_on_the_server() {
        let item = |id: &str, res: &[&str], art: &str| {
            let mut x = format!(
                r#"<item id="{id}"><dc:title>T</dc:title><upnp:class>object.item.audioItem.musicTrack</upnp:class>"#
            );
            if !art.is_empty() {
                x.push_str(&format!("<upnp:albumArtURI>{art}</upnp:albumArtURI>"));
            }
            for r in res {
                x.push_str(&format!(
                    r#"<res protocolInfo="http-get:*:audio/mpeg:*">{r}</res>"#
                ));
            }
            x + "</item>"
        };
        let didl = [
            item("docker", &["http://127.0.0.1:2375/containers/json"], ""),
            item(
                "metadata",
                &["http://169.254.169.254/latest/meta-data/"],
                "",
            ),
            item("port", &["http://nas:8080/p.mp3"], ""),
            item("scheme", &["https://nas:8200/a.mp3"], ""),
            item("userinfo", &["http://nas:8200@127.0.0.1/a.mp3"], ""),
            item("rtsp", &["rtsp://nas:8200/a.mp3"], ""),
            item("empty", &[""], ""),
            item(
                "second",
                &["http://10.0.0.9/a.mp3", "http://NAS:8200/b.mp3?x=1&amp;y=2"],
                "http://169.254.169.254/art.jpg",
            ),
            item("relative", &["/media/c.mp3"], "art/c.jpg"),
        ]
        .concat();
        let d = dlna("http://nas:8200/rootDesc.xml");
        let tracks: Vec<RemoteTrack> = children(&didl, "item")
            .into_iter()
            .filter_map(|(a, e)| d.item(e, &a))
            .collect();
        let keys: Vec<&str> = tracks.iter().map(|t| t.key.as_str()).collect();
        // Another port on the same host is fine (files from a second one).
        assert_eq!(keys, ["port", "second", "relative"]);
        let items = d.items.lock().unwrap();
        assert_eq!(items["second"].url, "http://NAS:8200/b.mp3?x=1&y=2");
        assert_eq!(items["second"].art, None, "album art elsewhere is dropped");
        assert!(!tracks[1].has_cover);
        assert_eq!(items["relative"].url, "http://nas:8200/media/c.mp3");
        assert_eq!(
            items["relative"].art.as_deref(),
            Some("http://nas:8200/art/c.jpg")
        );
    }

    /// A server that answers every page with more containers, a song and
    /// an endless or absurd total: the walk still ends.
    #[test]
    fn walk_is_bounded() {
        let d = dlna("http://nas:8200/rootDesc.xml");
        let mut calls = 0usize;
        let mut next = 0usize;
        let mut browse = |_: &str, start: usize| {
            calls += 1;
            let mut didl = String::new();
            for _ in 0..50 {
                next += 1;
                didl.push_str(&format!(r#"<container id="c{next}"></container>"#));
            }
            didl.push_str(&format!(
                r#"<item id="i{calls}"><upnp:class>object.item.audioItem</upnp:class><res>http://nas:8200/{calls}.mp3</res></item>"#
            ));
            // Absurd counts: an overflow without saturation.
            let returned = if start > 0 { usize::MAX } else { 1 };
            Ok((didl, returned, usize::MAX))
        };
        let list = d.walk(&mut browse, &mut |_| true).unwrap();
        assert!(calls <= MAX_CALLS, "{calls}");
        assert_eq!(list.len(), calls);

        // One container with endless pages.
        let mut calls = 0usize;
        let mut browse = |_: &str, _: usize| {
            calls += 1;
            Ok((String::new(), 1, usize::MAX))
        };
        d.walk(&mut browse, &mut |_| true).unwrap();
        assert_eq!(calls, MAX_PAGES);

        // No more answers: the container ends.
        let mut calls = 0usize;
        let mut browse = |_: &str, _: usize| {
            calls += 1;
            Ok((String::new(), 0, 1000))
        };
        d.walk(&mut browse, &mut |_| true).unwrap();
        assert_eq!(calls, 1);
    }

    #[test]
    fn search_answers() {
        let peer: IpAddr = "192.168.1.20".parse().unwrap();
        let answer =
            |loc: &str| format!("HTTP/1.1 200 OK\r\nLOCATION: {loc}\r\nUSN: uuid:x\r\n\r\n");
        assert_eq!(
            answer_location(&answer("http://192.168.1.20:8200/rootDesc.xml"), peer).as_deref(),
            Some("http://192.168.1.20:8200/rootDesc.xml")
        );
        for loc in [
            "http://192.168.1.21:8200/rootDesc.xml",
            "http://127.0.0.1:2375/info",
            "http://169.254.169.254/latest/meta-data/",
            "https://192.168.1.20/rootDesc.xml",
            "http://nas.local/rootDesc.xml",
            "http://192.168.1.20@127.0.0.1/x",
        ] {
            assert_eq!(answer_location(&answer(loc), peer), None, "{loc}");
        }
        assert_eq!(
            answer_location(
                "NOTIFY * HTTP/1.1\r\nLOCATION: http://192.168.1.20/\r\n\r\n",
                peer
            ),
            None
        );
    }

    /// A peer answering the search with many devices and with addresses
    /// elsewhere: only its own, and at most MAX_ANSWERS.
    #[test]
    fn search_is_bounded() {
        let peer = UdpSocket::bind("127.0.0.1:0").unwrap();
        peer.set_read_timeout(Some(Duration::from_secs(3))).unwrap();
        let target = peer.local_addr().unwrap();
        std::thread::spawn(move || {
            let mut buf = [0u8; 2048];
            while let Ok((_, from)) = peer.recv_from(&mut buf) {
                let send = |loc: &str| {
                    let a =
                        format!("HTTP/1.1 200 OK\r\nLOCATION: {loc}\r\nUSN: uuid:{loc}\r\n\r\n");
                    let _ = peer.send_to(a.as_bytes(), from);
                };
                send("http://10.0.0.1/rootDesc.xml");
                send("https://127.0.0.1/rootDesc.xml");
                for i in 0..100 {
                    send(&format!("http://127.0.0.1:1/d{i}.xml"));
                }
            }
        });
        let found = search(target, Duration::from_secs(2)).unwrap();
        assert_eq!(found.len(), MAX_ANSWERS, "{found:?}");
        assert!(found.iter().all(|l| l.starts_with("http://127.0.0.1:1/d")));
    }
}
