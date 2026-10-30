//! The admin pages (`/admin/`): built into the server program, so they work
//! without the web client and match the server's version. A strict content
//! security policy allows only the page's own script and style.

use bl_net::server::{Request, Response};

const INDEX: &str = include_str!("../admin/index.html");
const SCRIPT: &str = include_str!("../admin/admin.js");
const STYLE: &str = include_str!("../admin/admin.css");

const CSP: &str = "default-src 'none'; script-src 'self'; style-src 'self'; img-src 'self' data:; \
connect-src 'self'; form-action 'none'; base-uri 'none'; frame-ancestors 'none'";

fn page(body: &'static str, kind: &str) -> Response {
    let mut r = Response::bytes(200, kind, body.as_bytes().to_vec())
        .with_header("Cache-Control", "no-cache");
    if kind.starts_with("text/html") {
        r = r
            .with_header("Content-Security-Policy", CSP)
            .with_header("X-Robots-Tag", "noindex");
    }
    r
}

/// `/admin/...`
pub fn serve(req: &Request, segs: &[&str]) -> Response {
    if !matches!(req.method.as_str(), "GET" | "HEAD") {
        return Response::error(405, "method not allowed").with_header("Allow", "GET, HEAD");
    }
    match segs {
        // Relative links need the trailing slash.
        [] if !req.path.ends_with('/') => Response::new(308).with_header("Location", "/admin/"),
        [] | [""] | ["index.html"] => page(INDEX, "text/html; charset=utf-8"),
        ["admin.js"] => page(SCRIPT, "text/javascript; charset=utf-8"),
        ["admin.css"] => page(STYLE, "text/css; charset=utf-8"),
        _ => Response::error(404, "not found"),
    }
}
