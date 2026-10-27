//! Client side of the OpenChroma server connection.

use crate::host::{Host, HttpRequest};
use oc_net::api::{paths, ServerInfo};

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
}

impl ServerClient {
    pub fn new(base: &str, token: &str) -> ServerClient {
        ServerClient { base: base.trim().trim_end_matches('/').to_string(), token: token.to_string(), state: ServerState::Disabled }
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
            u.push_str(&oc_net::http::percent_encode(&self.token));
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
        host.http_request(HttpRequest { method: "GET".into(), url: self.url(path), headers: self.headers(), body: Vec::new() })
    }

    pub fn send(&self, host: &mut dyn Host, method: &str, path: &str, body: Vec<u8>, content_type: &str) -> u64 {
        let mut headers = self.headers();
        headers.push(("Content-Type".into(), content_type.into()));
        host.http_request(HttpRequest { method: method.into(), url: self.url(path), headers, body })
    }

    pub fn request_info(&mut self, host: &mut dyn Host) -> u64 {
        self.state = ServerState::Connecting;
        self.get(host, paths::INFO)
    }

    pub fn request_login(&self, host: &mut dyn Host, password: &str) -> u64 {
        let body = oc_json::Value::object().with("password", password).to_json().into_bytes();
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
        paths::track_analysis(id, &oc_net::http::percent_encode(cfg_json))
    }
}
