//! Server configuration from command line flags and an optional JSON
//! configuration file.
//!
//! Precedence (lowest to highest): built-in defaults, the `--config` file,
//! the `BEATLUMEN_PASSWORD` environment variable, command line flags.
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
    /// Longest track (minutes) analysed on the server (0 = unlimited).
    pub analysis_max_minutes: u64,
    /// Worker threads used to read tags during scans (0 = automatic).
    pub scan_threads: usize,
    /// Log every request and per-file scan problems.
    pub verbose: bool,
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
            analysis_max_minutes: 20,
            scan_threads: 0,
            verbose: false,
        }
    }
}

/// What the command line asks for.
#[derive(Debug)]
pub enum Action {
    Run(Box<Config>),
    Help,
    Version,
}

pub const USAGE: &str = "\
beatlumen-server - serves local music folders to Beatlumen clients over HTTP

USAGE:
    beatlumen-server [OPTIONS]

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
    --analysis-max-minutes M
                            Longest track analysed on the server; 0 = unlimited
                            [default: 20]
    --scan-threads N        Threads reading tags during scans [default: automatic]
    --config FILE           JSON configuration file (keys: music, images, data,
                            bind, password, webDir, rescanInterval, name,
                            analysisMaxMinutes, scanThreads, verbose)
    --verbose               Log every request and per-file scan problems
    -h, --help              Print this help
    -V, --version           Print the version

The server never opens outgoing network connections. It does not provide TLS:
for access over the internet put it behind a reverse proxy (e.g. Caddy or
nginx) with HTTPS, or use a VPN.
";

fn path_list(v: &Value, base: &Path) -> Result<Vec<PathBuf>, String> {
    match v {
        Value::Null => Ok(Vec::new()),
        Value::String(s) => Ok(vec![resolve(base, s)]),
        Value::Array(a) => a.iter().map(|x| x.as_str().map(|s| resolve(base, s)).ok_or_else(|| "expected a list of paths".to_string())).collect(),
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
    let obj = v.as_object().ok_or("configuration file must contain a JSON object")?;
    for (k, val) in obj.iter() {
        let err = |what: &str| format!("config key \"{k}\": {what}");
        match k {
            "music" => cfg.music_dirs = path_list(val, base).map_err(|e| err(&e))?,
            "images" => cfg.image_dirs = path_list(val, base).map_err(|e| err(&e))?,
            "data" => cfg.data_dir = resolve(base, val.as_str().ok_or_else(|| err("expected a path"))?),
            "webDir" => cfg.web_dir = Some(resolve(base, val.as_str().ok_or_else(|| err("expected a path"))?)),
            "bind" => cfg.bind = val.as_str().ok_or_else(|| err("expected a string"))?.to_string(),
            "password" => {
                cfg.password = match val {
                    Value::Null => None,
                    Value::String(s) if s.is_empty() => None,
                    Value::String(s) => Some(s.clone()),
                    _ => return Err(err("expected a string")),
                }
            }
            "name" => cfg.name = val.as_str().ok_or_else(|| err("expected a string"))?.to_string(),
            "rescanInterval" => cfg.rescan_interval = val.as_u64().ok_or_else(|| err("expected a non-negative integer"))?,
            "analysisMaxMinutes" => cfg.analysis_max_minutes = val.as_u64().ok_or_else(|| err("expected a non-negative integer"))?,
            "scanThreads" => cfg.scan_threads = val.as_u64().ok_or_else(|| err("expected a non-negative integer"))? as usize,
            "verbose" => cfg.verbose = val.as_bool().ok_or_else(|| err("expected true or false"))?,
            _ => crate::warn!("ignoring unknown configuration key \"{k}\""),
        }
    }
    Ok(())
}

/// Parses command line arguments (without the program name).
pub fn parse_args<I: IntoIterator<Item = String>>(args: I) -> Result<Action, String> {
    // Split `--flag=value` and collect (flag, value) pairs first.
    let mut items: Vec<(String, Option<String>)> = Vec::new();
    let mut it = args.into_iter();
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
                | "--config"
                | "--analysis-max-minutes"
                | "--scan-threads"
        );
        match flag.as_str() {
            "-h" | "--help" => return Ok(Action::Help),
            "-V" | "--version" => return Ok(Action::Version),
            "--verbose" => items.push((flag, None)),
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
        let text = std::fs::read(&p).map_err(|e| format!("cannot read config file {}: {e}", p.display()))?;
        let v = bl_json::parse_bytes(&text).map_err(|e| format!("invalid config file {}: {e}", p.display()))?;
        let base = p.parent().map(Path::to_path_buf).unwrap_or_default();
        apply_json(&mut cfg, &v, &base)?;
    }
    if let Ok(pw) = std::env::var("BEATLUMEN_PASSWORD") {
        if !pw.is_empty() {
            cfg.password = Some(pw);
        }
    }

    let num = |flag: &str, v: &str| v.trim().parse::<u64>().map_err(|_| format!("{flag}: expected a non-negative integer, got \"{v}\""));
    let mut music_cli = Vec::new();
    let mut images_cli = Vec::new();
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
            "--verbose" => cfg.verbose = true,
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
    Ok(Action::Run(Box::new(cfg)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(args: &[&str]) -> Config {
        match parse_args(args.iter().map(|s| s.to_string())).unwrap() {
            Action::Run(c) => *c,
            a => panic!("unexpected {a:?}"),
        }
    }

    #[test]
    fn flags() {
        let c = run(&["--music", "/a", "--music=/b", "--images", "/i", "--bind", "127.0.0.1:1", "--rescan-interval", "0", "--name", "N", "--verbose"]);
        assert_eq!(c.music_dirs, vec![PathBuf::from("/a"), PathBuf::from("/b")]);
        assert_eq!(c.image_dirs, vec![PathBuf::from("/i")]);
        assert_eq!((c.bind.as_str(), c.rescan_interval, c.name.as_str(), c.verbose), ("127.0.0.1:1", 0, "N", true));
        assert!(matches!(parse_args(["--help".to_string()]), Ok(Action::Help)));
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
