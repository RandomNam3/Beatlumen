//! Client side of the Beatlumen server connection.

use crate::host::{Host, HttpRequest};
use bl_net::api::{paths, ServerInfo};

#[derive(Debug, Clone, PartialEq)]
pub enum ServerState {
    Disabled,
    Connecting,
    Connected(ServerInfo),
    NeedsLogin(ServerInfo),
    Error(String),
}

pub struct ServerClient {
    /// Base URL without trailing slash (empty on the web = same origin).
    pub base: String,
    pub token: String,
    pub state: ServerState,
    /// Images are loaded by the UI's "server" image provider (native apps:
    /// through this crate's HTTP client and its TLS), not by Qt's network.
    pub image_provider: bool,
    /// The browser build's page origin (`https://host`): its image URLs must
    /// be absolute, since Qt resolves a bare path against its own resources
    /// (`qrc:/api/…`) instead of the page.
    pub page_origin: String,
}

impl ServerClient {
    pub fn new(base: &str, token: &str) -> ServerClient {
        ServerClient {
            base: base.trim().trim_end_matches('/').to_string(),
            token: token.to_string(),
            state: ServerState::Disabled,
            image_provider: false,
            page_origin: String::new(),
        }
    }

    /// URL of a server image for the UI.
    pub fn image_url(&self, path: &str) -> String {
        let u = self.media_url(path);
        if self.image_provider {
            format!("image://server/{}", bl_net::http::percent_encode(&u))
        } else if u.starts_with('/') {
            format!("{}{u}", self.page_origin)
        } else {
            u
        }
    }

    pub fn is_connected(&self) -> bool {
        matches!(self.state, ServerState::Connected(_))
    }

    pub fn url(&self, path: &str) -> String {
        format!("{}{}", self.base, path)
    }

    /// URL with the token as query parameter (for media streams).
    pub fn media_url(&self, path: &str) -> String {
        let mut u = self.url(path);
        if !self.token.is_empty() {
            u.push(if u.contains('?') { '&' } else { '?' });
            u.push_str("token=");
            u.push_str(&bl_net::http::percent_encode(&self.token));
        }
        u
    }

    pub fn headers(&self) -> Vec<(String, String)> {
        if self.token.is_empty() {
            Vec::new()
        } else {
            vec![("Authorization".into(), format!("Bearer {}", self.token))]
        }
    }

    pub fn get(&self, host: &mut dyn Host, path: &str) -> u64 {
        host.http_request(HttpRequest {
            method: "GET".into(),
            url: self.url(path),
            headers: self.headers(),
            body: Vec::new(),
        })
    }

    pub fn send(
        &self,
        host: &mut dyn Host,
        method: &str,
        path: &str,
        body: Vec<u8>,
        content_type: &str,
    ) -> u64 {
        let mut headers = self.headers();
        headers.push(("Content-Type".into(), content_type.into()));
        host.http_request(HttpRequest {
            method: method.into(),
            url: self.url(path),
            headers,
            body,
        })
    }

    pub fn request_info(&mut self, host: &mut dyn Host) -> u64 {
        self.state = ServerState::Connecting;
        self.get(host, paths::INFO)
    }

    pub fn request_login(&self, host: &mut dyn Host, password: &str) -> u64 {
        let body = bl_json::Value::object()
            .with("password", password)
            .to_json()
            .into_bytes();
        host.http_request(HttpRequest {
            method: "POST".into(),
            url: self.url(paths::LOGIN),
            headers: vec![("Content-Type".into(), "application/json".into())],
            body,
        })
    }

    pub fn track_stream_url(&self, id: &str) -> String {
        self.media_url(&paths::track_stream(id))
    }

    pub fn analysis_path(&self, id: &str, cfg_json: &str) -> String {
        paths::track_analysis(id, &bl_net::http::percent_encode(cfg_json))
    }
}

/// Whether `code` looks like an account invitation code (groups of hex
/// digits joined by dashes, as the server makes them).
pub fn valid_invite(code: &str) -> bool {
    (4..=64).contains(&code.len())
        && code.bytes().all(|b| b.is_ascii_hexdigit() || b == b'-')
        && code.bytes().any(|b| b.is_ascii_hexdigit())
}

/// Splits an invitation link (`https://server/#invite=CODE`, also with
/// `?invite=`) into the server address and the code; other addresses come
/// back unchanged with no code.
pub fn split_invite(url: &str) -> (String, Option<String>) {
    for key in ["#invite=", "?invite=", "&invite="] {
        if let Some(at) = url.find(key) {
            let code: String = url[at + key.len()..]
                .chars()
                .take_while(|c| *c != '&' && *c != '#')
                .collect();
            let code = code.trim().to_ascii_lowercase();
            if valid_invite(&code) {
                let base = url[..at].split(['?', '#']).next().unwrap_or("").to_string();
                return (base, Some(code));
            }
        }
    }
    (url.to_string(), None)
}

#[cfg(test)]
mod image_url_tests {
    use super::*;

    #[test]
    fn browser_image_urls_are_absolute() {
        let mut s = ServerClient::new("", "tok");
        s.page_origin = "https://music.example.org".into();
        assert_eq!(
            s.image_url("/api/tracks/a/cover?size=512"),
            "https://music.example.org/api/tracks/a/cover?size=512&token=tok"
        );
        // Native apps: through the image provider.
        let mut n = ServerClient::new("https://music.example.org", "tok");
        n.image_provider = true;
        assert!(n.image_url("/api/x").starts_with("image://server/https%3A"));
    }
}

#[cfg(test)]
mod invite_tests {
    use super::*;

    #[test]
    fn invitation_links_give_server_and_code() {
        assert_eq!(
            split_invite("https://music.example.org/#invite=1a2b-3c4d-5e6f-7a8b-9c0d-1e2f"),
            ("https://music.example.org/".to_string(), Some("1a2b-3c4d-5e6f-7a8b-9c0d-1e2f".to_string()))
        );
        assert_eq!(
            split_invite("http://192.168.1.10:7878?invite=ABCD-ef01"),
            ("http://192.168.1.10:7878".to_string(), Some("abcd-ef01".to_string()))
        );
        assert_eq!(
            split_invite("https://h/?room=x&invite=abcd-0123"),
            ("https://h/".to_string(), Some("abcd-0123".to_string()))
        );
        // Not an invitation: unchanged.
        assert_eq!(split_invite("https://music.example.org"), ("https://music.example.org".to_string(), None));
        assert_eq!(split_invite("https://h/#invite=<script>"), ("https://h/#invite=<script>".to_string(), None));
        assert!(!valid_invite("----"));
        assert!(!valid_invite(""));
    }
}
