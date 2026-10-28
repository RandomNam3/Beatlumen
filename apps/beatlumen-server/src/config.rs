//! Server configuration from command line flags and an optional JSON
//! configuration file.
//!
//! Precedence (lowest to highest): built-in defaults, the `--config` file,
//! `BEATLUMEN_*` environment variables (see [`ENV_VARS`]), command line
//! flags.
//! Repeatable flags (`--music`, `--images`) given on the command line replace
//! the lists from the configuration file. Relative paths in the
//! configuration file are resolved against the file's directory.

use bl_json::Value;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq)]
pub struct Config {
    /// Music library folders (scanned recursively).
    pub music_dirs: Vec<PathBuf>,
    /// Background image folders.
    pub image_dirs: Vec<PathBuf>,
    /// Caches, playlists, presets, uploads and tokens.
    pub data_dir: PathBuf,
    pub bind: String,
    /// When set, clients must log in.
    pub password: Option<String>,
    /// Static web client directory (explicit). When `None`, `<exe dir>/web`
    /// is used if it exists, else embedded files, else a placeholder page.
    pub web_dir: Option<PathBuf>,
    /// Seconds between automatic rescans (0 = off).
    pub rescan_interval: u64,
    pub name: String,
    /// Host names (besides IP addresses, localhost and local network names)
    /// the server answers to; protects password-less servers against DNS
    /// rebinding by web pages.
    pub allowed_hosts: Vec<String>,
    /// Where users get the source code of this server (AGPL section 13).
    pub source_url: String,
    /// Longest track (minutes) analysed on the server (0 = unlimited).
    pub analysis_max_minutes: u64,
    /// Worker threads used to read tags during scans (0 = automatic).
    pub scan_threads: usize,
    /// Log every request and per-file scan problems.
    pub verbose: bool,
    /// The secret mixed into account verifiers; default `<data>/vault/pepper`.
    pub vault_pepper_file: Option<PathBuf>,
    /// Encrypted storage per user in MiB (0 = unlimited).
    pub vault_quota_mb: u64,
    /// New accounts with invitation codes (false: registration is off).
    pub registration: bool,
    /// Push endpoints in private networks (and plain http) are allowed.
    pub push_allow_private: bool,
    /// The server's HTTPS address as users reach it; enables passkeys.
    pub public_url: Option<String>,
    /// SHA-256 fingerprints (hex) of Android app signing certificates whose
    /// apps may use passkeys for this server (besides the official one).
    pub android_certs: Vec<String>,
}

impl Config {
    /// Configured and built-in Android certificate fingerprints.
    pub fn all_android_certs(&self) -> Vec<String> {
        let mut v: Vec<String> = official_android_cert().map(str::to_string).into_iter().collect();
        for c in &self.android_certs {
            if !v.contains(c) {
                v.push(c.clone());
            }
        }
        v
    }
}

/// The signing certificate of the official Android app (built in).
pub fn official_android_cert() -> Option<&'static str> {
    option_env!("BEATLUMEN_ANDROID_CERT_SHA256").filter(|c| c.len() == 64)
}

impl Default for Config {
    fn default() -> Self {
        Config {
            music_dirs: Vec::new(),
            image_dirs: Vec::new(),
            data_dir: PathBuf::from("beatlumen-data"),
            bind: "0.0.0.0:7878".into(),
            password: None,
            web_dir: None,
            rescan_interval: 600,
            name: "Beatlumen Server".into(),
            source_url: bl_net::api::SOURCE_URL.into(),
            allowed_hosts: Vec::new(),
            analysis_max_minutes: 20,
            scan_threads: 0,
            verbose: false,
            vault_pepper_file: None,
            vault_quota_mb: 10 * 1024,
            registration: true,
            push_allow_private: false,
            public_url: None,
            android_certs: Vec::new(),
        }
    }
}

/// What the command line asks for.
#[derive(Debug)]
pub enum Action {
    Run(Box<Config>),
    /// Checks that a server with this configuration answers (container
    /// health checks); exit status 0 when it does.
    HealthCheck(Box<Config>),
    /// Creates an account invitation valid for some days.
    Invite(Box<Config>, u64),
    /// Lists the accounts.
    Users(Box<Config>),
    /// Writes the integrity manifest of a web client folder (and signs it
    /// when `BEATLUMEN_RELEASE_SIGNING_KEY` is set).
    WebManifest { dir: PathBuf, version: String },
    /// Creates a release signing key.
    ReleaseKeygen,
    Help,
    Version,
}

/// Environment variables read at start (name, meaning), for containers.
pub const ENV_VARS: &[(&str, &str)] = &[
    (
        "BEATLUMEN_MUSIC",
        "music folders, separated like PATH (':' on Linux)",
    ),
    (
        "BEATLUMEN_IMAGES",
        "background image folders, separated like PATH",
    ),
    ("BEATLUMEN_DATA", "data folder"),
    ("BEATLUMEN_BIND", "listen address"),
    ("BEATLUMEN_PASSWORD", "login password"),
    (
        "BEATLUMEN_PASSWORD_FILE",
        "file containing the login password (secrets)",
    ),
    ("BEATLUMEN_WEB_DIR", "web client folder"),
    ("BEATLUMEN_NAME", "server name shown to clients"),
    (
        "BEATLUMEN_ALLOWED_HOSTS",
        "extra host names, separated by commas",
    ),
    ("BEATLUMEN_SOURCE_URL", "source code link (AGPL)"),
    (
        "BEATLUMEN_RESCAN_INTERVAL",
        "seconds between rescans, 0 = off",
    ),
    (
        "BEATLUMEN_ANALYSIS_MAX_MINUTES",
        "longest track analysed, 0 = unlimited",
    ),
    (
        "BEATLUMEN_SCAN_THREADS",
        "threads reading tags, 0 = automatic",
    ),
    ("BEATLUMEN_VERBOSE", "1/true: log every request"),
    (
        "BEATLUMEN_VAULT_PEPPER_FILE",
        "secret for account verifiers (default: in the data folder)",
    ),
    (
        "BEATLUMEN_VAULT_QUOTA_MB",
        "encrypted storage per user in MiB, 0 = unlimited",
    ),
    (
        "BEATLUMEN_REGISTRATION",
        "invite: new accounts with invitation codes; off: none",
    ),
    (
        "BEATLUMEN_PUSH_ALLOW_PRIVATE",
        "1/true: allow push services in private networks",
    ),
    (
        "BEATLUMEN_CA_FILE",
        "extra root certificates (PEM) for outgoing TLS",
    ),
    (
        "BEATLUMEN_PUBLIC_URL",
        "the server's https:// address as users reach it (passkeys)",
    ),
    (
        "BEATLUMEN_ANDROID_CERTS",
        "extra Android app certificate fingerprints for passkeys, comma separated",
    ),
];

fn registration(v: &str) -> Result<bool, String> {
    match v.trim().to_ascii_lowercase().as_str() {
        "invite" | "on" => Ok(true),
        "off" => Ok(false),
        other => Err(format!("registration: expected \"invite\" or \"off\", got \"{other}\"")),
    }
}

/// Applies `BEATLUMEN_*` variables (read through `get`) to `cfg`.
pub fn apply_env(cfg: &mut Config, get: impl Fn(&str) -> Option<String>) -> Result<(), String> {
    let get = |k: &str| get(k).filter(|v| !v.trim().is_empty());
    let num = |k: &str, v: &str| {
        v.trim()
            .parse::<u64>()
            .map_err(|_| format!("{k}: expected a non-negative integer, got \"{v}\""))
    };
    if let Some(v) = get("BEATLUMEN_MUSIC") {
        cfg.music_dirs = std::env::split_paths(&v).collect();
    }
    if let Some(v) = get("BEATLUMEN_IMAGES") {
        cfg.image_dirs = std::env::split_paths(&v).collect();
    }
    if let Some(v) = get("BEATLUMEN_DATA") {
        cfg.data_dir = PathBuf::from(v);
    }
    if let Some(v) = get("BEATLUMEN_BIND") {
        cfg.bind = v.trim().to_string();
    }
    if let Some(v) = get("BEATLUMEN_PASSWORD_FILE") {
        let text = std::fs::read_to_string(v.trim())
            .map_err(|e| format!("BEATLUMEN_PASSWORD_FILE: cannot read {}: {e}", v.trim()))?;
        let pw = text.trim_end_matches(['\r', '\n']).to_string();
        cfg.password = (!pw.is_empty()).then_some(pw);
    }
    if let Some(v) = get("BEATLUMEN_PASSWORD") {
        cfg.password = Some(v);
    }
    if let Some(v) = get("BEATLUMEN_WEB_DIR") {
        cfg.web_dir = Some(PathBuf::from(v));
    }
    if let Some(v) = get("BEATLUMEN_NAME") {
        cfg.name = v;
    }
    if let Some(v) = get("BEATLUMEN_ALLOWED_HOSTS") {
        cfg.allowed_hosts = v
            .split([',', ' '])
            .map(|h| h.trim().to_ascii_lowercase())
            .filter(|h| !h.is_empty())
            .collect();
    }
    if let Some(v) = get("BEATLUMEN_SOURCE_URL") {
        cfg.source_url = v;
    }
    if let Some(v) = get("BEATLUMEN_RESCAN_INTERVAL") {
        cfg.rescan_interval = num("BEATLUMEN_RESCAN_INTERVAL", &v)?;
    }
    if let Some(v) = get("BEATLUMEN_ANALYSIS_MAX_MINUTES") {
        cfg.analysis_max_minutes = num("BEATLUMEN_ANALYSIS_MAX_MINUTES", &v)?;
    }
    if let Some(v) = get("BEATLUMEN_SCAN_THREADS") {
        cfg.scan_threads = num("BEATLUMEN_SCAN_THREADS", &v)? as usize;
    }
    if let Some(v) = get("BEATLUMEN_VAULT_PEPPER_FILE") {
        cfg.vault_pepper_file = Some(PathBuf::from(v.trim()));
    }
    if let Some(v) = get("BEATLUMEN_VAULT_QUOTA_MB") {
        cfg.vault_quota_mb = num("BEATLUMEN_VAULT_QUOTA_MB", &v)?;
    }
    if let Some(v) = get("BEATLUMEN_PUBLIC_URL") {
        cfg.public_url = Some(v.trim().trim_end_matches('/').to_string());
    }
    if let Some(v) = get("BEATLUMEN_ANDROID_CERTS") {
        cfg.android_certs = v
            .split([',', ' '])
            .map(|c| c.trim().replace(':', "").to_ascii_lowercase())
            .filter(|c| !c.is_empty())
            .collect();
    }
    if let Some(v) = get("BEATLUMEN_PUSH_ALLOW_PRIVATE") {
        cfg.push_allow_private = matches!(v.trim().to_ascii_lowercase().as_str(), "1" | "true" | "yes" | "on");
    }
    if let Some(v) = get("BEATLUMEN_REGISTRATION") {
        cfg.registration = registration(&v)?;
    }
    if let Some(v) = get("BEATLUMEN_VERBOSE") {
        cfg.verbose = matches!(
            v.trim().to_ascii_lowercase().as_str(),
            "1" | "true" | "yes" | "on"
        );
    }
    Ok(())
}

pub const USAGE: &str = "\
beatlumen-server - serves local music folders to Beatlumen clients over HTTP

USAGE:
    beatlumen-server [OPTIONS]
    beatlumen-server invite [--days N] [OPTIONS]   Create an account invitation
    beatlumen-server users [OPTIONS]               List the accounts
    beatlumen-server web-manifest DIR [--version V]
                                    Write the integrity manifest of a web client
                                    (signed when BEATLUMEN_RELEASE_SIGNING_KEY is set)
    beatlumen-server release-keygen  Create a release signing key

OPTIONS:
    --music DIR             Music folder to serve (repeatable)
    --images DIR            Background image folder (repeatable)
    --data DIR              Data folder for caches, playlists, presets, uploads
                            and login tokens [default: ./beatlumen-data]
    --bind ADDR             Listen address [default: 0.0.0.0:7878]
    --password PW           Require clients to log in with this password
                            (or set the BEATLUMEN_PASSWORD environment variable,
                            which keeps it out of the process list)
    --web-dir DIR           Web client files [default: <exe dir>/web if present,
                            else files embedded at build time, else a placeholder]
    --rescan-interval SECS  Seconds between automatic library rescans; 0 = off
                            [default: 600]
    --name NAME             Server name shown to clients [default: Beatlumen Server]
    --allow-host NAME       Host name the server may be reached by (repeatable), e.g.
                            the domain of a reverse proxy. IP addresses, localhost
                            and names ending in .local/.lan/.home.arpa/.internal
                            are always accepted
    --source-url URL        Source code of this server, shown to users. Required by
                            the AGPL when you run a modified version for others
                            [default: the official repository]
    --analysis-max-minutes M
                            Longest track analysed on the server; 0 = unlimited
                            [default: 20]
    --scan-threads N        Threads reading tags during scans [default: automatic]
    --config FILE           JSON configuration file (keys: music, images, data,
                            bind, password, webDir, rescanInterval, name, sourceUrl, allowedHosts,
                            analysisMaxMinutes, scanThreads, vaultPepperFile,
                            vaultQuotaMb, registration, pushAllowPrivate, publicUrl,
                            androidCerts, verbose)
    --vault-pepper-file FILE
                            Secret mixed into the account verifiers, at least 32
                            bytes; keep it outside the data folder (e.g. a
                            container secret) so a copy of the data alone is useless
                            [default: <data>/vault/pepper, created on first start]
    --vault-quota-mb MB     Encrypted storage per user; 0 = unlimited [default: 10240]
    --registration MODE     invite: new accounts with an invitation code from
                            `beatlumen-server invite`; off: no new accounts [default: invite]
    --public-url URL        The server's https:// address as users reach it (e.g. the
                            reverse proxy's). Enables passkeys, whose relying party
                            is this host
    --android-cert SHA256   Fingerprint of an Android app's signing certificate that
                            may use passkeys for this server (repeatable; the official
                            app is built in)
    --push-allow-private    Allow push services (UnifiedPush) in private networks and
                            over plain http, e.g. ntfy in the home network
    --verbose               Log every request and per-file scan problems
    --health-check          Check that the server configured this way answers
                            (exit status 0), for container health checks
    -h, --help              Print this help
    -V, --version           Print the version

ENVIRONMENT:
    BEATLUMEN_MUSIC, BEATLUMEN_IMAGES (folders separated like PATH),
    BEATLUMEN_DATA, BEATLUMEN_BIND, BEATLUMEN_PASSWORD,
    BEATLUMEN_PASSWORD_FILE, BEATLUMEN_WEB_DIR, BEATLUMEN_NAME,
    BEATLUMEN_ALLOWED_HOSTS (comma separated), BEATLUMEN_SOURCE_URL,
    BEATLUMEN_RESCAN_INTERVAL, BEATLUMEN_ANALYSIS_MAX_MINUTES,
    BEATLUMEN_SCAN_THREADS, BEATLUMEN_VAULT_PEPPER_FILE,
    BEATLUMEN_VAULT_QUOTA_MB, BEATLUMEN_REGISTRATION,
    BEATLUMEN_PUSH_ALLOW_PRIVATE, BEATLUMEN_CA_FILE, BEATLUMEN_PUBLIC_URL,
    BEATLUMEN_ANDROID_CERTS, BEATLUMEN_VERBOSE
    They override the --config file; command line flags override them.

The server connects only to remote sources you configure (no update checks,
telemetry or metadata lookups). It does not provide TLS:
for access over the internet use a VPN, or a reverse proxy with HTTPS
together with --allow-host.

ACCOUNTS:
    Users with an account store end-to-end encrypted files on the server and
    share them with each other; the server cannot read them. Accounts are
    created in the apps with an invitation code from `beatlumen-server invite`
    (in a container: `docker exec <container> /beatlumen-server invite`).
";

fn path_list(v: &Value, base: &Path) -> Result<Vec<PathBuf>, String> {
    match v {
        Value::Null => Ok(Vec::new()),
        Value::String(s) => Ok(vec![resolve(base, s)]),
        Value::Array(a) => a
            .iter()
            .map(|x| {
                x.as_str()
                    .map(|s| resolve(base, s))
                    .ok_or_else(|| "expected a list of paths".to_string())
            })
            .collect(),
        _ => Err("expected a path or a list of paths".into()),
    }
}

fn resolve(base: &Path, s: &str) -> PathBuf {
    let p = PathBuf::from(s);
    if p.is_absolute() {
        p
    } else {
        base.join(p)
    }
}

/// Applies a JSON configuration document to `cfg`.
pub fn apply_json(cfg: &mut Config, v: &Value, base: &Path) -> Result<(), String> {
    let obj = v
        .as_object()
        .ok_or("configuration file must contain a JSON object")?;
    for (k, val) in obj.iter() {
        let err = |what: &str| format!("config key \"{k}\": {what}");
        match k {
            "music" => cfg.music_dirs = path_list(val, base).map_err(|e| err(&e))?,
            "images" => cfg.image_dirs = path_list(val, base).map_err(|e| err(&e))?,
            "data" => {
                cfg.data_dir = resolve(base, val.as_str().ok_or_else(|| err("expected a path"))?)
            }
            "webDir" => {
                cfg.web_dir = Some(resolve(
                    base,
                    val.as_str().ok_or_else(|| err("expected a path"))?,
                ))
            }
            "bind" => {
                cfg.bind = val
                    .as_str()
                    .ok_or_else(|| err("expected a string"))?
                    .to_string()
            }
            "password" => {
                cfg.password = match val {
                    Value::Null => None,
                    Value::String(s) if s.is_empty() => None,
                    Value::String(s) => Some(s.clone()),
                    _ => return Err(err("expected a string")),
                }
            }
            "name" => {
                cfg.name = val
                    .as_str()
                    .ok_or_else(|| err("expected a string"))?
                    .to_string()
            }
            "allowedHosts" => {
                cfg.allowed_hosts = val
                    .as_array()
                    .ok_or_else(|| err("expected an array of strings"))?
                    .iter()
                    .map(|v| {
                        v.as_str()
                            .map(|s| s.to_ascii_lowercase())
                            .ok_or_else(|| err("expected an array of strings"))
                    })
                    .collect::<Result<_, _>>()?
            }
            "sourceUrl" => {
                cfg.source_url = val
                    .as_str()
                    .ok_or_else(|| err("expected a string"))?
                    .to_string()
            }
            "rescanInterval" => {
                cfg.rescan_interval = val
                    .as_u64()
                    .ok_or_else(|| err("expected a non-negative integer"))?
            }
            "analysisMaxMinutes" => {
                cfg.analysis_max_minutes = val
                    .as_u64()
                    .ok_or_else(|| err("expected a non-negative integer"))?
            }
            "scanThreads" => {
                cfg.scan_threads = val
                    .as_u64()
                    .ok_or_else(|| err("expected a non-negative integer"))?
                    as usize
            }
            "verbose" => {
                cfg.verbose = val.as_bool().ok_or_else(|| err("expected true or false"))?
            }
            "vaultPepperFile" => {
                cfg.vault_pepper_file = Some(resolve(
                    base,
                    val.as_str().ok_or_else(|| err("expected a path"))?,
                ))
            }
            "publicUrl" => {
                cfg.public_url = Some(
                    val.as_str()
                        .ok_or_else(|| err("expected a string"))?
                        .trim_end_matches('/')
                        .to_string(),
                )
            }
            "androidCerts" => {
                cfg.android_certs = val
                    .as_array()
                    .ok_or_else(|| err("expected an array of strings"))?
                    .iter()
                    .map(|v| {
                        v.as_str()
                            .map(|s| s.replace(':', "").to_ascii_lowercase())
                            .ok_or_else(|| err("expected an array of strings"))
                    })
                    .collect::<Result<_, _>>()?
            }
            "pushAllowPrivate" => {
                cfg.push_allow_private = val.as_bool().ok_or_else(|| err("expected true or false"))?
            }
            "registration" => {
                cfg.registration =
                    registration(val.as_str().ok_or_else(|| err("expected \"invite\" or \"off\""))?)
                        .map_err(|e| err(&e))?
            }
            "vaultQuotaMb" => {
                cfg.vault_quota_mb = val
                    .as_u64()
                    .ok_or_else(|| err("expected a non-negative integer"))?
            }
            _ => crate::warn!("ignoring unknown configuration key \"{k}\""),
        }
    }
    Ok(())
}

/// Parses command line arguments (without the program name).
pub fn parse_args<I: IntoIterator<Item = String>>(args: I) -> Result<Action, String> {
    // Split `--flag=value` and collect (flag, value) pairs first.
    let mut items: Vec<(String, Option<String>)> = Vec::new();
    let mut it = args.into_iter().peekable();
    // Release tools.
    match it.peek().map(String::as_str) {
        Some("release-keygen") => return Ok(Action::ReleaseKeygen),
        Some("web-manifest") => {
            it.next();
            let mut dir = None;
            let mut version = env!("CARGO_PKG_VERSION").to_string();
            while let Some(a) = it.next() {
                match a.as_str() {
                    "--version" => version = it.next().ok_or("--version needs a value")?,
                    _ if dir.is_none() && !a.starts_with("--") => dir = Some(PathBuf::from(a)),
                    _ => return Err(format!("unknown argument: {a}")),
                }
            }
            let dir = dir.ok_or("usage: beatlumen-server web-manifest DIR [--version V]")?;
            return Ok(Action::WebManifest { dir, version });
        }
        _ => {}
    }
    let command = match it.peek().map(String::as_str) {
        Some(c @ ("invite" | "users")) => {
            let c = c.to_string();
            it.next();
            Some(c)
        }
        _ => None,
    };
    while let Some(a) = it.next() {
        let (flag, inline) = match a.split_once('=') {
            Some((f, v)) if f.starts_with("--") => (f.to_string(), Some(v.to_string())),
            _ => (a.clone(), None),
        };
        let takes_value = matches!(
            flag.as_str(),
            "--music"
                | "--images"
                | "--data"
                | "--bind"
                | "--password"
                | "--web-dir"
                | "--rescan-interval"
                | "--name"
                | "--source-url"
                | "--allow-host"
                | "--config"
                | "--analysis-max-minutes"
                | "--scan-threads"
                | "--vault-pepper-file"
                | "--vault-quota-mb"
                | "--registration"
                | "--public-url"
                | "--android-cert"
                | "--days"
        );
        match flag.as_str() {
            "-h" | "--help" => return Ok(Action::Help),
            "-V" | "--version" => return Ok(Action::Version),
            "--verbose" | "--health-check" | "--push-allow-private" => items.push((flag, None)),
            _ if takes_value => {
                let v = match inline {
                    Some(v) => v,
                    None => it.next().ok_or_else(|| format!("{flag} needs a value"))?,
                };
                items.push((flag, Some(v)));
            }
            _ => return Err(format!("unknown argument: {a} (see --help)")),
        }
    }

    let mut cfg = Config::default();
    if let Some((_, Some(path))) = items.iter().rev().find(|(f, _)| f == "--config") {
        let p = PathBuf::from(path);
        let text = std::fs::read(&p)
            .map_err(|e| format!("cannot read config file {}: {e}", p.display()))?;
        let v = bl_json::parse_bytes(&text)
            .map_err(|e| format!("invalid config file {}: {e}", p.display()))?;
        let base = p.parent().map(Path::to_path_buf).unwrap_or_default();
        apply_json(&mut cfg, &v, &base)?;
    }
    apply_env(&mut cfg, |k| std::env::var(k).ok())?;

    let num = |flag: &str, v: &str| {
        v.trim()
            .parse::<u64>()
            .map_err(|_| format!("{flag}: expected a non-negative integer, got \"{v}\""))
    };
    let mut music_cli = Vec::new();
    let mut images_cli = Vec::new();
    let mut health_check = false;
    let mut days = crate::vault::INVITE_DAYS;
    for (flag, v) in items {
        let v = v.unwrap_or_default();
        match flag.as_str() {
            "--music" => music_cli.push(PathBuf::from(v)),
            "--images" => images_cli.push(PathBuf::from(v)),
            "--data" => cfg.data_dir = PathBuf::from(v),
            "--bind" => cfg.bind = v,
            "--password" => cfg.password = (!v.is_empty()).then_some(v),
            "--web-dir" => cfg.web_dir = Some(PathBuf::from(v)),
            "--rescan-interval" => cfg.rescan_interval = num(&flag, &v)?,
            "--analysis-max-minutes" => cfg.analysis_max_minutes = num(&flag, &v)?,
            "--scan-threads" => cfg.scan_threads = num(&flag, &v)? as usize,
            "--name" => cfg.name = v,
            "--source-url" => cfg.source_url = v,
            "--allow-host" => cfg.allowed_hosts.push(v.to_ascii_lowercase()),
            "--verbose" => cfg.verbose = true,
            "--health-check" => health_check = true,
            "--vault-pepper-file" => cfg.vault_pepper_file = Some(PathBuf::from(v)),
            "--vault-quota-mb" => cfg.vault_quota_mb = num(&flag, &v)?,
            "--registration" => cfg.registration = registration(&v)?,
            "--push-allow-private" => cfg.push_allow_private = true,
            "--public-url" => cfg.public_url = Some(v.trim_end_matches('/').to_string()),
            "--android-cert" => cfg.android_certs.push(v.replace(':', "").to_ascii_lowercase()),
            "--days" if command.as_deref() == Some("invite") => days = num(&flag, &v)?.clamp(1, 365),
            "--days" => return Err("--days only applies to `invite`".into()),
            _ => {}
        }
    }
    if !music_cli.is_empty() {
        cfg.music_dirs = music_cli;
    }
    if !images_cli.is_empty() {
        cfg.image_dirs = images_cli;
    }
    if cfg.name.trim().is_empty() {
        cfg.name = Config::default().name;
    }
    match command.as_deref() {
        Some("invite") => return Ok(Action::Invite(Box::new(cfg), days)),
        Some("users") => return Ok(Action::Users(Box::new(cfg))),
        _ => {}
    }
    if health_check {
        return Ok(Action::HealthCheck(Box::new(cfg)));
    }
    Ok(Action::Run(Box::new(cfg)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn environment() {
        let dir = std::env::temp_dir().join(format!("bl-env-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let pw = dir.join("pw");
        std::fs::write(&pw, "geheim\n").unwrap();
        let env: std::collections::HashMap<&str, String> = [
            ("BEATLUMEN_MUSIC", "/m1:/m2".to_string()),
            ("BEATLUMEN_DATA", "/data".into()),
            ("BEATLUMEN_PASSWORD_FILE", pw.display().to_string()),
            (
                "BEATLUMEN_ALLOWED_HOSTS",
                "Music.Example.org, other.example".into(),
            ),
            ("BEATLUMEN_RESCAN_INTERVAL", "0".into()),
            ("BEATLUMEN_VERBOSE", "true".into()),
            ("BEATLUMEN_NAME", "".into()),
        ]
        .into_iter()
        .collect();
        let mut c = Config::default();
        apply_env(&mut c, |k| env.get(k).cloned()).unwrap();
        assert_eq!(
            c.music_dirs,
            vec![PathBuf::from("/m1"), PathBuf::from("/m2")]
        );
        assert_eq!(c.data_dir, PathBuf::from("/data"));
        assert_eq!(c.password.as_deref(), Some("geheim"));
        assert_eq!(c.allowed_hosts, vec!["music.example.org", "other.example"]);
        assert_eq!(c.rescan_interval, 0);
        assert!(c.verbose);
        assert_eq!(c.name, Config::default().name);
        let bad = |k: &'static str, v: &'static str| {
            let mut c = Config::default();
            apply_env(&mut c, |x| (x == k).then(|| v.to_string())).is_err()
        };
        assert!(bad("BEATLUMEN_SCAN_THREADS", "many"));
        assert!(bad("BEATLUMEN_PASSWORD_FILE", "/nonexistent/pw"));
        std::fs::remove_dir_all(&dir).ok();
    }

    fn run(args: &[&str]) -> Config {
        match parse_args(args.iter().map(|s| s.to_string())).unwrap() {
            Action::Run(c) => *c,
            a => panic!("unexpected {a:?}"),
        }
    }

    #[test]
    fn flags() {
        let c = run(&[
            "--music",
            "/a",
            "--music=/b",
            "--images",
            "/i",
            "--bind",
            "127.0.0.1:1",
            "--rescan-interval",
            "0",
            "--name",
            "N",
            "--verbose",
        ]);
        assert_eq!(c.music_dirs, vec![PathBuf::from("/a"), PathBuf::from("/b")]);
        assert_eq!(c.image_dirs, vec![PathBuf::from("/i")]);
        assert_eq!(
            (
                c.bind.as_str(),
                c.rescan_interval,
                c.name.as_str(),
                c.verbose
            ),
            ("127.0.0.1:1", 0, "N", true)
        );
        assert!(matches!(
            parse_args(["--help".to_string()]),
            Ok(Action::Help)
        ));
        assert!(parse_args(["--bogus".to_string()]).is_err());
        assert!(parse_args(["--music".to_string()]).is_err());
        assert!(parse_args(["--rescan-interval".to_string(), "x".to_string()]).is_err());
    }

    #[test]
    fn config_file() {
        let dir = std::env::temp_dir().join(format!("ocs-config-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("server.json");
        std::fs::write(&file, r#"{"music":["m1","/abs/m2"],"images":"img","data":"d","password":"pw","rescanInterval":5,"name":"Home"}"#).unwrap();
        let c = run(&["--config", file.to_str().unwrap(), "--rescan-interval", "7"]);
        assert_eq!(c.music_dirs, vec![dir.join("m1"), PathBuf::from("/abs/m2")]);
        assert_eq!(c.image_dirs, vec![dir.join("img")]);
        assert_eq!(c.data_dir, dir.join("d"));
        assert_eq!(c.password.as_deref(), Some("pw"));
        assert_eq!((c.rescan_interval, c.name.as_str()), (7, "Home"));
        let c = run(&["--config", file.to_str().unwrap(), "--music", "/x"]);
        assert_eq!(c.music_dirs, vec![PathBuf::from("/x")]);
        std::fs::write(&file, "[1]").unwrap();
        assert!(parse_args(["--config".to_string(), file.to_string_lossy().into_owned()]).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
