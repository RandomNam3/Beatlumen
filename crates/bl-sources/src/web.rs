//! Music on a web server without any API: a link to a folder whose
//! directory listing (Apache, nginx `autoindex`, lighttpd, Caddy, `python
//! -m http.server`, …) links to audio files, or a link to a single file.
//!
//! The listing is read as plain HTML: no scripts run, only `href` links
//! are followed, and only links below the starting address on the same
//! host (same scheme and port), with limits on depth, pages and size.
//! Each file's size and date come from a HEAD request, so tags are read
//! only from new or changed files.

use crate::{tags, MusicSource, RemoteTrack, SourceConfig};
use bl_net::client::{self, HttpStream, Options, Url};

const MAX_DEPTH: usize = 8;
const MAX_PAGES: usize = 2000;
const MAX_FILES: usize = 50_000;
/// Largest listing page read.
const MAX_PAGE: usize = 4 << 20;
/// Longest path followed (decoded, bytes).
const MAX_PATH: usize = 4096;
/// Links read from one page.
const MAX_LINKS: usize = 10_000;
/// Paths remembered while crawling (folders and files).
const MAX_SEEN: usize = MAX_PAGES + MAX_FILES;
/// Bytes of the paths remembered while crawling.
const MAX_SEEN_BYTES: usize = 32 << 20;
const COVER_NAMES: &[&str] = &[
    "cover.jpg",
    "folder.jpg",
    "front.jpg",
    "cover.png",
    "folder.png",
];

pub struct Web {
    /// The origin (`scheme://host[:port]`).
    origin: String,
    /// The starting folder's path, decoded, ending with `/` (or the single
    /// file's path).
    root: String,
    /// The address is one file, not a folder.
    single: bool,
    auth: Vec<(String, String)>,
}

/// `scheme://host[:port]` of a parsed URL.
fn origin(u: &Url) -> String {
    let port = match (u.scheme.as_str(), u.port) {
        ("http", 80) | ("https", 443) => String::new(),
        (_, p) => format!(":{p}"),
    };
    let host = if u.host.contains(':') {
        format!("[{}]", u.host)
    } else {
        u.host.clone()
    };
    format!("{}://{host}{port}", u.scheme)
}

/// Removes `.` and `..` segments of a path starting with `/`.
fn normalize(path: &str) -> String {
    let mut out: Vec<&str> = Vec::new();
    let trailing = path.ends_with('/');
    for seg in path.split('/') {
        match seg {
            "" | "." => {}
            ".." => {
                out.pop();
            }
            s => out.push(s),
        }
    }
    let mut p = format!("/{}", out.join("/"));
    if trailing && p != "/" {
        p.push('/');
    }
    p
}

/// The decoded path of a link on the page at `page` (a decoded path), or
/// None when it leaves `origin`, carries a query or is not http(s).
pub fn link_path(origin: &str, page: &str, href: &str) -> Option<String> {
    let href = href.trim();
    let href = href.split('#').next()?;
    if href.is_empty() || href.contains('?') {
        return None;
    }
    let lower = href.to_ascii_lowercase();
    let path = if lower.starts_with("http://") || lower.starts_with("https://") {
        let rest = href.split_once("://")?.1;
        let (authority, path) = rest.split_at(rest.find('/').unwrap_or(rest.len()));
        let scheme = &lower[..lower.find("://")?];
        if !same_origin(origin, &format!("{scheme}://{authority}")) {
            return None;
        }
        if path.is_empty() {
            "/".to_string()
        } else {
            crate::decode(path)
        }
    } else if let Some(rest) = href.strip_prefix("//") {
        let scheme = origin.split("://").next()?;
        let (authority, path) = rest.split_at(rest.find('/').unwrap_or(rest.len()));
        if !same_origin(origin, &format!("{scheme}://{authority}")) {
            return None;
        }
        crate::decode(path)
    } else if lower
        .find(':')
        .is_some_and(|c| !lower.find('/').is_some_and(|s| s < c))
    {
        // mailto:, javascript:, data: and the like.
        return None;
    } else if href.starts_with('/') {
        crate::decode(href)
    } else {
        let dir = &page[..page.rfind('/').map_or(0, |i| i + 1)];
        format!("{dir}{}", crate::decode(href))
    };
    Some(normalize(&path))
}

/// Whether two `scheme://authority` strings name the same origin (default
/// ports and case aside).
fn same_origin(a: &str, b: &str) -> bool {
    let canon = |s: &str| -> Option<String> {
        let u = Url::parse(&format!("{}/", s.trim_end_matches('/'))).ok()?;
        Some(origin(&u).to_ascii_lowercase())
    };
    matches!((canon(a), canon(b)), (Some(x), Some(y)) if x == y)
}

/// The `href` values of a page (attribute quoted with `"` or `'`, or
/// bare), at most 10 000.
pub fn hrefs(html: &str) -> Vec<String> {
    let lower = html.to_ascii_lowercase();
    let b = html.as_bytes();
    let mut out = Vec::new();
    let mut at = 0;
    while out.len() < MAX_LINKS {
        let Some(i) = lower[at..].find("href") else {
            break;
        };
        let mut j = at + i + 4;
        at = j;
        // Only the attribute (preceded by whitespace), not e.g. "data-href".
        if at < 5 || !b[at - 5].is_ascii_whitespace() {
            continue;
        }
        while j < b.len() && b[j].is_ascii_whitespace() {
            j += 1;
        }
        if j >= b.len() || b[j] != b'=' {
            continue;
        }
        j += 1;
        while j < b.len() && b[j].is_ascii_whitespace() {
            j += 1;
        }
        if j >= b.len() {
            break;
        }
        let (start, end) = match b[j] {
            q @ (b'"' | b'\'') => {
                let Some(e) = html[j + 1..].find(q as char) else {
                    break;
                };
                (j + 1, j + 1 + e)
            }
            _ => {
                let e = html[j..]
                    .find(|c: char| c.is_ascii_whitespace() || c == '>')
                    .unwrap_or(html.len() - j);
                (j, j + e)
            }
        };
        out.push(unescape(&html[start..end]));
        at = end;
    }
    out
}

/// The few HTML entities that appear in links.
fn unescape(s: &str) -> String {
    s.replace("&amp;", "&")
        .replace("&#39;", "'")
        .replace("&quot;", "\"")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
}

impl Web {
    pub fn connect(cfg: &SourceConfig) -> Result<Web, String> {
        if !(cfg.url.starts_with("http://") || cfg.url.starts_with("https://")) {
            return Err("the address must start with http:// or https://".into());
        }
        let u = Url::parse(&cfg.url)?;
        if u.path.contains('?') {
            return Err("the address must not have a query (?…)".into());
        }
        let mut auth = Vec::new();
        if !cfg.user.is_empty() {
            let pair = format!("{}:{}", cfg.user, cfg.secret);
            auth.push((
                "Authorization".to_string(),
                format!("Basic {}", bl_net::base64_encode(pair.as_bytes())),
            ));
        }
        let path = normalize(&crate::decode(&u.path));
        let single = bl_media::is_supported_extension(&crate::ext_of(&path));
        let mut w = Web {
            origin: origin(&u),
            root: path,
            single,
            auth,
        };
        if !w.single && !w.root.ends_with('/') {
            w.root.push('/');
        }
        let r = w.request(if w.single { "HEAD" } else { "GET" }, &w.root)?;
        match r.status {
            200 => Ok(w),
            401 | 403 => Err("access denied (user name or password?)".into()),
            404 => Err("not found".into()),
            s => Err(format!("HTTP {s}")),
        }
    }

    fn url(&self, path: &str) -> String {
        format!("{}{}", self.origin, crate::encode_path(path))
    }

    fn request(&self, method: &str, path: &str) -> Result<client::Response, String> {
        let opts = Options {
            max_body: MAX_PAGE,
            ..Default::default()
        };
        client::request(method, &self.url(path), &self.auth, &[], &opts)
    }

    /// Size and date of a file (0 when the server does not say).
    fn head(&self, path: &str) -> (u64, u64) {
        match self.request("HEAD", path) {
            Ok(r) if r.status == 200 => (
                r.headers
                    .get("content-length")
                    .and_then(|v| v.trim().parse().ok())
                    .unwrap_or(0),
                r.headers
                    .get("last-modified")
                    .and_then(crate::webdav::http_date)
                    .unwrap_or(0),
            ),
            _ => (0, 0),
        }
    }

    /// The key of a path: relative to the starting folder.
    fn key(&self, path: &str) -> String {
        if self.single {
            return path.rsplit('/').next().unwrap_or(path).to_string();
        }
        path.strip_prefix(&self.root).unwrap_or(path).to_string()
    }

    fn path_of(&self, key: &str) -> Option<String> {
        if self.single {
            return Some(self.root.clone());
        }
        if key
            .split('/')
            .any(|s| s.is_empty() || s == "." || s == "..")
        {
            return None;
        }
        Some(format!("{}{key}", self.root))
    }

    /// Audio files found below the starting folder (decoded paths).
    fn crawl(&self, progress: &mut dyn FnMut(usize) -> bool) -> Result<Vec<String>, String> {
        if self.single {
            return Ok(vec![self.root.clone()]);
        }
        let mut files = Vec::new();
        let mut seen = std::collections::HashSet::new();
        let mut queue = std::collections::VecDeque::from([(self.root.clone(), 0usize)]);
        seen.insert(self.root.clone());
        // Bytes of the paths in `seen`.
        let mut kept = self.root.len();
        let mut pages = 0;
        while let Some((page, depth)) = queue.pop_front() {
            pages += 1;
            if pages > MAX_PAGES {
                break;
            }
            let r = match self.request("GET", &page) {
                Ok(r) if r.status == 200 => r,
                // The starting page must answer; others may fail.
                Ok(r) if page == self.root => return Err(format!("HTTP {}", r.status)),
                Err(e) if page == self.root => return Err(e),
                _ => continue,
            };
            let html = String::from_utf8_lossy(&r.body);
            for href in hrefs(&html) {
                // Percent-encoded, a path of MAX_PATH bytes is at most
                // three times as long.
                if href.len() > 3 * MAX_PATH {
                    continue;
                }
                let Some(path) = link_path(&self.origin, &page, &href) else {
                    continue;
                };
                // Only below the starting folder, never upwards or aside.
                if path.len() > MAX_PATH
                    || !path.starts_with(&self.root)
                    || path == page
                    || seen.contains(&path)
                {
                    continue;
                }
                kept += path.len();
                if seen.len() >= MAX_SEEN || kept > MAX_SEEN_BYTES {
                    return Ok(files);
                }
                seen.insert(path.clone());
                let name = path.trim_end_matches('/').rsplit('/').next().unwrap_or("");
                if name.starts_with('.') {
                    continue;
                }
                if path.ends_with('/') {
                    // Folders beyond the page limit are never read.
                    if depth < MAX_DEPTH && pages + queue.len() < MAX_PAGES {
                        queue.push_back((path, depth + 1));
                    }
                } else if bl_media::is_supported_extension(&crate::ext_of(&path)) {
                    files.push(path);
                    if files.len() >= MAX_FILES || !progress(0) {
                        return Ok(files);
                    }
                }
            }
        }
        Ok(files)
    }
}

impl MusicSource for Web {
    fn list(
        &self,
        known: crate::Known,
        progress: &mut dyn FnMut(usize) -> bool,
    ) -> Result<Vec<RemoteTrack>, String> {
        let files = self.crawl(&mut |_| true)?;
        let mut out = Vec::new();
        for path in files {
            let key = self.key(&path);
            let (size, modified) = self.head(&path);
            let track = match known(&key, size, modified) {
                Some(t) => t,
                None => match self.open(&key) {
                    Ok(mut src) => tags::read(src.as_mut(), &key, size, modified),
                    Err(_) => continue,
                },
            };
            out.push(track);
            if !progress(out.len()) {
                break;
            }
        }
        Ok(out)
    }

    fn open(&self, key: &str) -> Result<Box<dyn bl_media::Source>, String> {
        let path = self.path_of(key).ok_or("invalid song")?;
        Ok(Box::new(HttpStream::open(&self.url(&path), &self.auth)?))
    }

    fn cover(&self, key: &str, _size: u32) -> Option<Vec<u8>> {
        let path = self.path_of(key)?;
        let dir = &path[..path.rfind('/')? + 1];
        for name in COVER_NAMES {
            if let Ok(r) = self.request("GET", &format!("{dir}{name}")) {
                let image = r
                    .headers
                    .get("content-type")
                    .is_some_and(|t| t.starts_with("image/"));
                if r.status == 200 && image && !r.body.is_empty() {
                    return Some(r.body);
                }
            }
        }
        let mut src = self.open(key).ok()?;
        let info = bl_media::read_info(src.as_mut(), Some(&crate::ext_of(key)), true).ok()?;
        info.pictures.into_iter().next().map(|p| p.data)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn links() {
        let o = "http://music.example.org";
        let page = "/Music/Rock/";
        assert_eq!(
            link_path(o, page, "a%20b.mp3").unwrap(),
            "/Music/Rock/a b.mp3"
        );
        assert_eq!(link_path(o, page, "../").unwrap(), "/Music/");
        assert_eq!(link_path(o, page, "/Music/Pop/").unwrap(), "/Music/Pop/");
        assert_eq!(
            link_path(o, page, "http://MUSIC.example.org:80/x.ogg").unwrap(),
            "/x.ogg"
        );
        assert_eq!(link_path(o, page, "//music.example.org/y/").unwrap(), "/y/");
        assert!(link_path(o, page, "https://music.example.org/x.ogg").is_none());
        assert!(link_path(o, page, "http://other.example.org/x.ogg").is_none());
        assert!(link_path(o, page, "?C=N;O=D").is_none());
        assert!(link_path(o, page, "javascript:alert(1)").is_none());
        assert!(link_path(o, page, "mailto:a@b").is_none());
        assert_eq!(link_path(o, page, "x.mp3#t").unwrap(), "/Music/Rock/x.mp3");
        assert_eq!(normalize("/a/./b/../../../c/"), "/c/");
    }

    #[test]
    fn hrefs_of_listings() {
        let html = r#"<a href="Song%201.mp3">Song 1</a> <A HREF='Sub/'>Sub</A>
            <a data-href="no.mp3">x</a> <a href=bare.flac>b</a> <a href="a&amp;b.ogg">c</a>"#;
        assert_eq!(
            hrefs(html),
            ["Song%201.mp3", "Sub/", "bare.flac", "a&b.ogg"]
        );
        let many = r#"<a href="x">x</a> "#.repeat(MAX_LINKS + 5);
        assert_eq!(hrefs(&many).len(), MAX_LINKS);
    }
}
