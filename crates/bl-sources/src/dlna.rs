//! DLNA / UPnP AV media servers (Fritz!Box, NAS systems, media servers on
//! TVs and receivers): the audio items of the server's ContentDirectory,
//! browsed container by container, with their metadata, stream addresses
//! and album art. Servers are found with SSDP only while the user searches
//! ([`discover`]); an address can also be entered.

use crate::{MusicSource, RemoteTrack, SourceConfig};
use bl_net::client::{self, HttpStream, Options, Url};
use bl_upnp::{xml, Renderer};
use std::collections::HashMap;
use std::sync::Mutex;
use std::time::Duration;

const CONTENT_DIRECTORY: &str = "urn:schemas-upnp-org:service:ContentDirectory:1";
pub const MEDIA_SERVER: &str = "urn:schemas-upnp-org:device:MediaServer:1";
const PAGE: usize = 200;
const MAX_CONTAINERS: usize = 20_000;
const MAX_ITEMS: usize = 200_000;
const MAX_DEPTH: usize = 24;

/// A media server found in the network.
#[derive(Clone, Debug, PartialEq)]
pub struct Found {
    pub name: String,
    pub location: String,
}

/// Searches the local network for media servers for `wait`.
pub fn discover(wait: Duration) -> Vec<Found> {
    let answers = bl_upnp::ssdp::discover(MEDIA_SERVER, wait).unwrap_or_default();
    let mut out: Vec<Found> = Vec::new();
    for a in answers {
        if out.iter().any(|f| f.location == a.location) {
            continue;
        }
        if let Ok(r) = client::request("GET", &a.location, &[], &[], &Options::default()) {
            let text = String::from_utf8_lossy(&r.body);
            if text.contains(CONTENT_DIRECTORY) {
                out.push(Found {
                    name: xml::text(&text, "friendlyName").unwrap_or_else(|| a.location.clone()),
                    location: a.location,
                });
            }
        }
    }
    out
}

struct Item {
    url: String,
    art: Option<String>,
}

pub struct Dlna {
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
        let r = client::request("GET", &cfg.url, &[], &[], &Options::default())?;
        if r.status != 200 {
            return Err(format!("HTTP {}", r.status));
        }
        let text = String::from_utf8_lossy(&r.body).into_owned();
        let base = xml::text(&text, "URLBase")
            .and_then(|u| Url::parse(&u).ok())
            .map_or_else(|| Url::parse(&cfg.url), Ok)?;
        let control = xml::elements(&text, "service")
            .into_iter()
            .find(|s| {
                xml::text(s, "serviceType").is_some_and(|t| {
                    t.starts_with("urn:schemas-upnp-org:service:ContentDirectory:")
                })
            })
            .and_then(|s| xml::text(s, "controlURL"))
            .map(|u| base.join(u.trim()))
            .ok_or("not a media server (no ContentDirectory)")?;
        Ok(Dlna {
            control,
            items: Mutex::new(HashMap::new()),
        })
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
        // The first resource with an http address.
        let res = xml::elements(el, "res")
            .into_iter()
            .zip(res_attrs(el))
            .find(|(r, _)| r.trim().starts_with("http"))?;
        let url = xml::unescape(res.0.trim());
        let protocol = attr(&res.1, "protocolInfo").unwrap_or_default();
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
        let art = xml::text(el, "albumArtURI");
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
            duration_ms: attr(&res.1, "duration").map_or(0, |d| duration_ms(&d)),
            size: attr(&res.1, "size")
                .and_then(|s| s.parse().ok())
                .unwrap_or(0),
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
        let mut out = Vec::new();
        let mut stack = vec![("0".to_string(), 0usize)];
        let mut containers = 0usize;
        while let Some((id, depth)) = stack.pop() {
            containers += 1;
            if containers > MAX_CONTAINERS {
                break;
            }
            let mut start = 0;
            loop {
                let (didl, returned, total) = match self.browse(&id, start) {
                    Ok(r) => r,
                    Err(e) if id == "0" => return Err(e),
                    Err(_) => break,
                };
                for (attrs, _) in children(&didl, "container") {
                    if depth < MAX_DEPTH {
                        if let Some(cid) = attr(&attrs, "id") {
                            stack.push((cid, depth + 1));
                        }
                    }
                }
                for (attrs, el) in children(&didl, "item") {
                    out.extend(self.item(el, &attrs));
                    if out.len() >= MAX_ITEMS || !progress(out.len()) {
                        return Ok(out);
                    }
                }
                start += returned;
                if returned == 0 || start >= total {
                    break;
                }
            }
        }
        Ok(out)
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
        let r = client::request("GET", &art, &[], &[], &Options::default()).ok()?;
        (r.status == 200).then_some(r.body)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn didl_items() {
        let didl = r#"<DIDL-Lite xmlns="urn:schemas-upnp-org:metadata-1-0/DIDL-Lite/" xmlns:dc="http://purl.org/dc/elements/1.1/" xmlns:upnp="urn:schemas-upnp-org:metadata-1-0/upnp/">
<container id="c1" parentID="0" restricted="1"><dc:title>Music</dc:title><upnp:class>object.container</upnp:class></container>
<item id="i1" parentID="c1" restricted="1"><dc:title>Song &amp; Dance</dc:title><upnp:artist>Band</upnp:artist><upnp:album>Album</upnp:album><upnp:originalTrackNumber>3</upnp:originalTrackNumber><upnp:class>object.item.audioItem.musicTrack</upnp:class><upnp:albumArtURI>http://h/art.jpg</upnp:albumArtURI><res protocolInfo="http-get:*:audio/flac:*" size="1234" duration="0:03:25.500">http://h/song.flac</res></item>
<item id="v1" parentID="c1"><dc:title>Clip</dc:title><upnp:class>object.item.videoItem</upnp:class><res protocolInfo="http-get:*:video/mp4:*">http://h/v.mp4</res></item>
</DIDL-Lite>"#;
        let d = Dlna {
            control: String::new(),
            items: Mutex::new(HashMap::new()),
        };
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
}
