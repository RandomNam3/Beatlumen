//! WebDAV folders (Nextcloud, ownCloud, NAS systems): the audio files
//! below a folder (PROPFIND, depth 1, folder by folder), their tags read
//! from the files with range requests, covers from folder images or the
//! files, and Nextcloud's login flow v2, which yields an app password the
//! user can revoke in Nextcloud.

use crate::{tags, MusicSource, RemoteTrack, SourceConfig};
use bl_net::client::{self, HttpStream, Options, Url};
use bl_upnp::xml;

/// Folders deep, entries in total.
const MAX_DEPTH: usize = 32;
const MAX_ENTRIES: usize = 200_000;
const COVER_NAMES: &[&str] = &[
    "cover.jpg",
    "folder.jpg",
    "front.jpg",
    "cover.png",
    "folder.png",
];

pub struct WebDav {
    /// The folder's URL (ends with `/`).
    root: Url,
    auth: Vec<(String, String)>,
}

struct Entry {
    /// Path below the root ('/' separated, decoded).
    path: String,
    is_dir: bool,
    size: u64,
    modified: u64,
}

const PROPFIND: &str = r#"<?xml version="1.0" encoding="utf-8"?>
<d:propfind xmlns:d="DAV:"><d:prop><d:resourcetype/><d:getcontentlength/><d:getlastmodified/></d:prop></d:propfind>"#;

/// Seconds since the epoch of an HTTP date (`Sun, 06 Nov 1994 08:49:37 GMT`).
pub fn http_date(s: &str) -> Option<u64> {
    let p: Vec<&str> = s.split_whitespace().collect();
    if p.len() < 5 {
        return None;
    }
    let day: i64 = p[1].parse().ok()?;
    let month = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ]
    .iter()
    .position(|m| m.eq_ignore_ascii_case(p[2]))? as i64
        + 1;
    let year: i64 = p[3].parse().ok()?;
    let hms: Vec<i64> = p[4].split(':').filter_map(|x| x.parse().ok()).collect();
    if hms.len() != 3 {
        return None;
    }
    // Days from the civil date (H. Hinnant).
    let y = if month <= 2 { year - 1 } else { year };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (month + 9) % 12;
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    u64::try_from(days * 86_400 + hms[0] * 3600 + hms[1] * 60 + hms[2]).ok()
}

impl WebDav {
    pub fn connect(cfg: &SourceConfig) -> Result<WebDav, String> {
        if !(cfg.url.starts_with("http://") || cfg.url.starts_with("https://")) {
            return Err("the address must start with http:// or https://".into());
        }
        let mut url = cfg.url.clone();
        let folder = cfg.path.trim_matches('/');
        if !folder.is_empty() {
            url = format!("{url}/{}", crate::encode_path(folder));
        }
        url.push('/');
        let mut auth = Vec::new();
        if !cfg.user.is_empty() {
            let pair = format!("{}:{}", cfg.user, cfg.secret);
            auth.push((
                "Authorization".to_string(),
                format!("Basic {}", bl_net::base64_encode(pair.as_bytes())),
            ));
        }
        let dav = WebDav {
            root: Url::parse(&url)?,
            auth,
        };
        dav.propfind("")?;
        Ok(dav)
    }

    fn url_of(&self, path: &str) -> String {
        format!(
            "{}{}",
            self.root_string(),
            crate::encode_path(path.trim_start_matches('/'))
        )
    }

    fn root_string(&self) -> String {
        let port = match (self.root.scheme.as_str(), self.root.port) {
            ("http", 80) | ("https", 443) => String::new(),
            (_, p) => format!(":{p}"),
        };
        format!(
            "{}://{}{port}{}",
            self.root.scheme, self.root.host, self.root.path
        )
    }

    /// The entries of a folder (relative path, "" = the root).
    fn propfind(&self, dir: &str) -> Result<Vec<Entry>, String> {
        let mut url = self.url_of(dir);
        if !url.ends_with('/') {
            url.push('/');
        }
        let mut headers = self.auth.clone();
        headers.push(("Depth".into(), "1".into()));
        headers.push((
            "Content-Type".into(),
            "application/xml; charset=utf-8".into(),
        ));
        let opts = Options {
            max_body: 32 << 20,
            ..Default::default()
        };
        let r = client::request("PROPFIND", &url, &headers, PROPFIND.as_bytes(), &opts)?;
        match r.status {
            207 => {}
            401 => return Err("wrong user name or password".into()),
            404 => return Err("folder not found".into()),
            s => return Err(format!("HTTP {s}")),
        }
        let text = String::from_utf8_lossy(&r.body);
        let root_path = crate::decode(&self.root.path);
        let mut out = Vec::new();
        for resp in xml::elements(&text, "response") {
            let Some(href) = xml::text(resp, "href") else {
                continue;
            };
            // The href is a path (or a full URL); make it relative to the root.
            let path = href
                .split_once("://")
                .map(|(_, rest)| rest.find('/').map_or("/", |i| &rest[i..]).to_string())
                .unwrap_or(href);
            let path = crate::decode(&path);
            let Some(rel) = path.strip_prefix(root_path.as_str()) else {
                continue;
            };
            let rel = rel.trim_matches('/').to_string();
            if rel == dir.trim_matches('/') {
                continue;
            }
            let is_dir = !xml::elements(resp, "collection").is_empty();
            out.push(Entry {
                path: rel,
                is_dir,
                size: xml::text(resp, "getcontentlength")
                    .and_then(|s| s.parse().ok())
                    .unwrap_or(0),
                modified: xml::text(resp, "getlastmodified")
                    .and_then(|s| http_date(&s))
                    .unwrap_or(0),
            });
        }
        Ok(out)
    }
}

impl MusicSource for WebDav {
    fn list(
        &self,
        known: crate::Known,
        progress: &mut dyn FnMut(usize) -> bool,
    ) -> Result<Vec<RemoteTrack>, String> {
        let mut out = Vec::new();
        let mut stack = vec![(String::new(), 0usize)];
        let mut seen = 0usize;
        while let Some((dir, depth)) = stack.pop() {
            let entries = match self.propfind(&dir) {
                Ok(e) => e,
                // The root must answer; subfolders may fail on their own.
                Err(e) if dir.is_empty() => return Err(e),
                Err(_) => continue,
            };
            for e in entries {
                seen += 1;
                if seen > MAX_ENTRIES {
                    return Ok(out);
                }
                let name = e.path.rsplit('/').next().unwrap_or(&e.path);
                if name.starts_with('.') {
                    continue;
                }
                if e.is_dir {
                    if depth < MAX_DEPTH {
                        stack.push((e.path, depth + 1));
                    }
                    continue;
                }
                if !bl_media::is_supported_extension(&crate::ext_of(&e.path)) {
                    continue;
                }
                let track = match known(&e.path, e.size, e.modified) {
                    Some(t) => t,
                    None => match self.open(&e.path) {
                        Ok(mut src) => tags::read(src.as_mut(), &e.path, e.size, e.modified),
                        Err(_) => continue,
                    },
                };
                out.push(track);
                if !progress(out.len()) {
                    return Ok(out);
                }
            }
        }
        Ok(out)
    }

    fn open(&self, key: &str) -> Result<Box<dyn bl_media::Source>, String> {
        Ok(Box::new(HttpStream::open(&self.url_of(key), &self.auth)?))
    }

    fn cover(&self, key: &str, _size: u32) -> Option<Vec<u8>> {
        let dir = key.rsplit_once('/').map_or("", |(d, _)| d);
        for name in COVER_NAMES {
            let path = if dir.is_empty() {
                name.to_string()
            } else {
                format!("{dir}/{name}")
            };
            if let Ok(r) = client::request(
                "GET",
                &self.url_of(&path),
                &self.auth,
                &[],
                &crate::opts(crate::MAX_COVER),
            ) {
                if r.status == 200 && !r.body.is_empty() {
                    return Some(r.body);
                }
            }
        }
        let mut src = self.open(key).ok()?;
        let info = bl_media::read_info(src.as_mut(), Some(&crate::ext_of(key)), true).ok()?;
        info.pictures.into_iter().next().map(|p| p.data)
    }
}

/// Nextcloud login flow v2: the URL the user opens to allow access, and
/// what [`nextcloud_poll`] asks with.
pub struct NextcloudLogin {
    pub login_url: String,
    pub poll_endpoint: String,
    pub token: String,
}

pub fn nextcloud_start(server: &str) -> Result<NextcloudLogin, String> {
    let server = server.trim().trim_end_matches('/');
    let r = client::request(
        "POST",
        &format!("{server}/index.php/login/v2"),
        &[("User-Agent".into(), "Beatlumen".into())],
        &[],
        &crate::opts(crate::MAX_SMALL),
    )?;
    if r.status != 200 {
        return Err(format!("HTTP {}", r.status));
    }
    let v = bl_json::parse_bytes(&r.body).map_err(|e| format!("invalid answer: {e}"))?;
    let login = NextcloudLogin {
        login_url: v.str_or("login", "").to_string(),
        poll_endpoint: v.get("poll").str_or("endpoint", "").to_string(),
        token: v.get("poll").str_or("token", "").to_string(),
    };
    if login.login_url.is_empty() || login.poll_endpoint.is_empty() || login.token.is_empty() {
        return Err("not a Nextcloud server".into());
    }
    Ok(login)
}

/// The result of the login once the user allowed it: (server, user name,
/// app password); None while it is still open.
pub fn nextcloud_poll(login: &NextcloudLogin) -> Result<Option<(String, String, String)>, String> {
    let body = format!("token={}", crate::encode(&login.token));
    let r = client::request(
        "POST",
        &login.poll_endpoint,
        &[(
            "Content-Type".into(),
            "application/x-www-form-urlencoded".into(),
        )],
        body.as_bytes(),
        &crate::opts(crate::MAX_SMALL),
    )?;
    match r.status {
        404 => Ok(None),
        200 => {
            let v = bl_json::parse_bytes(&r.body).map_err(|e| format!("invalid answer: {e}"))?;
            Ok(Some((
                v.str_or("server", "").to_string(),
                v.str_or("loginName", "").to_string(),
                v.str_or("appPassword", "").to_string(),
            )))
        }
        s => Err(format!("HTTP {s}")),
    }
}

/// The WebDAV address of a Nextcloud user's files.
pub fn nextcloud_dav_url(server: &str, user: &str) -> String {
    format!(
        "{}/remote.php/dav/files/{}",
        server.trim_end_matches('/'),
        crate::encode(user)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dates() {
        assert_eq!(
            http_date("Sun, 06 Nov 1994 08:49:37 GMT"),
            Some(784_111_777)
        );
        assert_eq!(http_date("Thu, 01 Jan 1970 00:00:00 GMT"), Some(0));
        assert_eq!(http_date("garbage"), None);
    }
}
