//! Checks the server's web client from outside (native apps): downloads
//! the integrity manifest, verifies its signature with the release key
//! built into the app and compares every file. Unlike the server's own
//! check, this one also catches an admin who changed the server.

use crate::core::{Core, Req};
use crate::host::{Host, HttpRequest, HttpResponse};
use bl_json::Value;
use bl_vault::manifest::{self, Manifest};

#[derive(Debug, Clone, PartialEq, Default)]
pub(crate) enum WebCheck {
    #[default]
    Idle,
    Running {
        done: usize,
        total: usize,
    },
    /// Signature and all files verified.
    Verified(String),
    /// Files match, but this build has no release key to check the signature.
    Unsigned,
    Failed(String),
}

#[derive(Default)]
pub(crate) struct WebCheckState {
    pub result: WebCheck,
    manifest: Option<Manifest>,
    manifest_bytes: Vec<u8>,
    pending: usize,
    bad: Vec<String>,
}

#[derive(Debug, Clone)]
pub(crate) enum WebReq {
    Manifest,
    Signature,
    File(String),
}

impl Core {
    fn webcheck_get(&mut self, host: &mut dyn Host, path: &str, req: WebReq) {
        let id = host.http_request(HttpRequest {
            method: "GET".into(),
            url: self.server.url(path),
            headers: self.server.headers(),
            body: Vec::new(),
        });
        self.requests.insert(id, Req::WebCheck(req));
    }

    pub(crate) fn webcheck_start(&mut self, host: &mut dyn Host) {
        self.webcheck = WebCheckState {
            result: WebCheck::Running { done: 0, total: 0 },
            ..Default::default()
        };
        self.webcheck_get(host, &format!("/{}", manifest::MANIFEST), WebReq::Manifest);
    }

    fn webcheck_fail(&mut self, msg: String) {
        self.webcheck.result = WebCheck::Failed(msg);
        self.webcheck.pending = 0;
    }

    pub(crate) fn webcheck_http(
        &mut self,
        host: &mut dyn Host,
        req: WebReq,
        res: Result<HttpResponse, String>,
    ) {
        if !matches!(self.webcheck.result, WebCheck::Running { .. }) {
            return;
        }
        let body = match res {
            Ok(r) if r.status == 200 => r.body,
            // Unsigned (unofficial) builds have no signature file.
            Ok(r)
                if r.status == 404
                    && matches!(req, WebReq::Signature)
                    && manifest::release_key().is_none() =>
            {
                Vec::new()
            }
            Ok(r) if r.status == 404 && matches!(req, WebReq::Manifest) => {
                return self.webcheck_fail(
                    self.tr("The server's web app has no integrity manifest")
                        .into(),
                );
            }
            Ok(r) => return self.webcheck_fail(format!("{:?}: HTTP {}", req, r.status)),
            Err(e) => return self.webcheck_fail(e),
        };
        match req {
            WebReq::Manifest => match Manifest::parse(&body) {
                Ok(m) => {
                    self.webcheck.manifest = Some(m);
                    self.webcheck.manifest_bytes = body;
                    self.webcheck_get(
                        host,
                        &format!("/{}", manifest::SIGNATURE),
                        WebReq::Signature,
                    );
                }
                Err(e) => self.webcheck_fail(e.to_string()),
            },
            WebReq::Signature => {
                if let Some(key) = manifest::release_key() {
                    let sig = String::from_utf8_lossy(&body);
                    if manifest::verify(&key, &self.webcheck.manifest_bytes, &sig).is_err() {
                        return self.webcheck_fail(
                            self.tr("The manifest is not signed with the release key")
                                .into(),
                        );
                    }
                }
                let files: Vec<String> = self
                    .webcheck
                    .manifest
                    .as_ref()
                    .map(|m| m.files.keys().cloned().collect())
                    .unwrap_or_default();
                self.webcheck.pending = files.len();
                self.webcheck.result = WebCheck::Running {
                    done: 0,
                    total: files.len(),
                };
                for f in files {
                    let path = format!(
                        "/{}",
                        f.split('/')
                            .map(bl_net::http::percent_encode)
                            .collect::<Vec<_>>()
                            .join("/")
                    );
                    self.webcheck_get(host, &path, WebReq::File(f));
                }
            }
            WebReq::File(name) => {
                let ok = self
                    .webcheck
                    .manifest
                    .as_ref()
                    .is_some_and(|m| m.matches(&name, &body));
                if !ok {
                    self.webcheck.bad.push(name);
                }
                self.webcheck.pending = self.webcheck.pending.saturating_sub(1);
                if let WebCheck::Running { done, .. } = &mut self.webcheck.result {
                    *done += 1;
                }
                if self.webcheck.pending == 0 {
                    let version = self
                        .webcheck
                        .manifest
                        .as_ref()
                        .map(|m| m.version.clone())
                        .unwrap_or_default();
                    self.webcheck.result = if !self.webcheck.bad.is_empty() {
                        WebCheck::Failed(format!(
                            "{} {}",
                            self.tr("Modified files:"),
                            self.webcheck.bad.join(", ")
                        ))
                    } else if manifest::release_key().is_some() {
                        WebCheck::Verified(version)
                    } else {
                        WebCheck::Unsigned
                    };
                }
            }
        }
    }

    pub(crate) fn webcheck_json(&self) -> Value {
        let (state, detail) = match &self.webcheck.result {
            WebCheck::Idle => ("idle", String::new()),
            WebCheck::Running { done, total } => ("running", format!("{done}/{total}")),
            WebCheck::Verified(v) => ("verified", v.clone()),
            WebCheck::Unsigned => ("unsigned", String::new()),
            WebCheck::Failed(e) => ("failed", e.clone()),
        };
        Value::object().with("state", state).with("detail", detail)
    }
}
