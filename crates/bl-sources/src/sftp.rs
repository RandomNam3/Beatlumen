//! SFTP folders (any SSH server: NAS systems, Linux servers) through
//! bl-ssh: the audio files below a folder, their tags read from the files
//! (several files at once over the one connection), covers from folder
//! images or the files.
//!
//! The server's host key is pinned (`SourceConfig::pin`, its SHA-256
//! fingerprint). Without a pin connecting fails with an error that names
//! the key's fingerprint; [`pin_in_error`] takes it out for the user to
//! confirm and save. `secret` is a password, or an Ed25519 private key in
//! OpenSSH's format. A connection that broke is opened again on the next
//! use.

use crate::{tags, MusicSource, RemoteTrack, SourceConfig};
use bl_ssh::{Auth, Session, Sftp};
use std::collections::HashSet;
use std::io::Read;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Mutex;

/// Folders deep, entries in total.
const MAX_DEPTH: usize = 32;
const MAX_ENTRIES: usize = 200_000;
/// Files whose tags are read at the same time (hides the network delay).
const TAG_READERS: usize = 4;
const COVER_NAMES: &[&str] = &[
    "cover.jpg",
    "folder.jpg",
    "front.jpg",
    "cover.png",
    "folder.png",
];
const MAX_COVER: u64 = 16 << 20;

pub struct SftpSource {
    cfg: SourceConfig,
    sftp: Mutex<Sftp>,
    /// The folder, absolute (as the server's REALPATH gave it).
    root: String,
}

/// Host, port and user name ("" if none) of `sftp://[user@]host[:port]`.
pub fn parse_url(url: &str) -> Result<(String, u16, String), String> {
    let url = url.trim();
    let rest = match url.split_once("://") {
        Some((scheme, rest))
            if scheme.eq_ignore_ascii_case("sftp") || scheme.eq_ignore_ascii_case("ssh") =>
        {
            rest
        }
        _ => return Err("the address must start with sftp://".into()),
    };
    let authority = rest.split('/').next().unwrap_or("");
    let (user, host_port) = match authority.rsplit_once('@') {
        Some((u, h)) => (crate::decode(u), h),
        None => (String::new(), authority),
    };
    let bad = || format!("invalid address {url}");
    let (host, port) = if let Some(v6) = host_port.strip_prefix('[') {
        let (host, after) = v6.split_once(']').ok_or_else(bad)?;
        match after.strip_prefix(':') {
            Some(p) => (host, p.parse::<u16>().map_err(|_| bad())?),
            None if after.is_empty() => (host, 22),
            None => return Err(bad()),
        }
    } else {
        match host_port.split_once(':') {
            Some((host, p)) => (host, p.parse::<u16>().map_err(|_| bad())?),
            None => (host_port, 22),
        }
    };
    if host.is_empty() || port == 0 {
        return Err(bad());
    }
    Ok((host.to_string(), port, user))
}

/// The server's host key fingerprint named in a connection error ("unknown
/// host key …" or "host key changed …"), to show the user.
pub fn pin_in_error(err: &str) -> Option<String> {
    let at = err.find("SHA256:")? + 7;
    let b64: String = err[at..]
        .chars()
        .take_while(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '/'))
        .collect();
    (!b64.is_empty()).then(|| format!("SHA256:{b64}"))
}

fn open_session(cfg: &SourceConfig) -> Result<Sftp, String> {
    let (host, port, url_user) = parse_url(&cfg.url)?;
    let user = if cfg.user.is_empty() {
        url_user
    } else {
        cfg.user.clone()
    };
    if user.is_empty() {
        return Err("the user name is missing".into());
    }
    let auth = Auth::from_secret(&cfg.secret).map_err(|e| e.to_string())?;
    Session::connect(&host, port, &cfg.pin, &user, &auth)
        .and_then(Session::sftp)
        .map_err(|e| e.to_string())
}

impl SftpSource {
    pub fn connect(cfg: &SourceConfig) -> Result<SftpSource, String> {
        let sftp = open_session(cfg)?;
        let folder = cfg.path.trim();
        let root = sftp
            .realpath(if folder.is_empty() { "." } else { folder })
            .map_err(|e| format!("folder {folder}: {e}"))?;
        if !sftp.stat(&root).map_err(|e| e.to_string())?.is_dir() {
            return Err(format!("{folder} is not a folder"));
        }
        Ok(SftpSource {
            cfg: cfg.clone(),
            sftp: Mutex::new(sftp),
            root: root.trim_end_matches('/').to_string(),
        })
    }

    /// The connection, opened again when it broke.
    fn sftp(&self) -> Result<Sftp, String> {
        let mut s = self.sftp.lock().unwrap_or_else(|e| e.into_inner());
        if !s.is_alive() {
            *s = open_session(&self.cfg)?;
        }
        Ok(s.clone())
    }

    /// Runs `f`, once more on a new connection if the connection broke.
    fn retry<T>(&self, f: impl Fn(&Sftp) -> Result<T, bl_ssh::Error>) -> Result<T, String> {
        let sftp = self.sftp()?;
        match f(&sftp) {
            Err(_) if !sftp.is_alive() => f(&self.sftp()?).map_err(|e| e.to_string()),
            r => r.map_err(|e| e.to_string()),
        }
    }

    /// The server path of a file or folder below the root.
    fn path_of(&self, rel: &str) -> Result<String, String> {
        // Keys come from listings; still, nothing may lead out of the folder.
        if rel
            .split('/')
            .any(|p| p.is_empty() || p == "." || p == "..")
        {
            return Err(format!("invalid path {rel}"));
        }
        Ok(format!("{}/{rel}", self.root))
    }

    /// Calls `each` with the key, size and modification time of the audio
    /// files below the root, in folder order, until it returns false.
    /// Links are followed, folders by their real path so that loops and
    /// repeats end.
    fn walk(
        &self,
        sftp: &Sftp,
        each: &mut dyn FnMut(String, u64, u64) -> bool,
    ) -> Result<(), String> {
        let mut visited = HashSet::from([self.root.clone()]);
        let mut stack = vec![(String::new(), self.root.clone(), 0usize)];
        let mut seen = 0usize;
        while let Some((dir, real, depth)) = stack.pop() {
            let path = if dir.is_empty() {
                self.root.clone()
            } else {
                self.path_of(&dir)?
            };
            let mut entries = match sftp.read_dir(&path) {
                Ok(e) => e,
                // The root must answer; subfolders may fail on their own.
                Err(e) if dir.is_empty() || !sftp.is_alive() => return Err(e.to_string()),
                Err(_) => continue,
            };
            entries.sort_by(|a, b| a.name.cmp(&b.name));
            let mut subdirs = Vec::new();
            for e in entries {
                seen += 1;
                if seen > MAX_ENTRIES {
                    return Ok(());
                }
                if e.name.starts_with('.') {
                    continue;
                }
                let key = if dir.is_empty() {
                    e.name.clone()
                } else {
                    format!("{dir}/{}", e.name)
                };
                let (is_dir, size, mtime, real_child) = if e.is_symlink {
                    let target = format!("{path}/{}", e.name);
                    match sftp.stat(&target) {
                        Ok(m) if m.is_dir() => match sftp.realpath(&target) {
                            Ok(r) => (true, 0, 0, r),
                            Err(_) => continue,
                        },
                        Ok(m) if m.is_file() => (
                            false,
                            m.size.unwrap_or(0),
                            m.mtime.unwrap_or(0) as u64,
                            String::new(),
                        ),
                        _ => continue,
                    }
                } else {
                    (e.is_dir, e.size, e.mtime, format!("{real}/{}", e.name))
                };
                if is_dir {
                    if depth < MAX_DEPTH && visited.insert(real_child.clone()) {
                        subdirs.push((key, real_child, depth + 1));
                    }
                } else if bl_media::is_supported_extension(&crate::ext_of(&e.name))
                    && !each(key, size, mtime)
                {
                    return Ok(());
                }
            }
            // Depth first, in name order.
            stack.extend(subdirs.into_iter().rev());
        }
        Ok(())
    }
}

impl MusicSource for SftpSource {
    fn list(
        &self,
        known: crate::Known,
        progress: &mut dyn FnMut(usize) -> bool,
    ) -> Result<Vec<RemoteTrack>, String> {
        // The server may have closed an idle connection.
        self.retry(|s| s.stat(&self.root))?;
        let sftp = self.sftp()?;
        // Known songs right away; the others' tags are read afterwards.
        let mut files = Vec::new();
        let mut out: Vec<Option<RemoteTrack>> = Vec::new();
        let mut found = 0;
        let mut stopped = false;
        self.walk(&sftp, &mut |key, size, mtime| {
            let t = known(&key, size, mtime);
            found += t.is_some() as usize;
            out.push(t);
            files.push((key, size, mtime));
            stopped = !progress(found);
            !stopped
        })?;
        if stopped {
            return Ok(out.into_iter().flatten().collect());
        }
        let todo: Vec<usize> = (0..out.len()).filter(|&i| out[i].is_none()).collect();
        // Tag readers share the connection; the answers come back here.
        let next = AtomicUsize::new(0);
        let stop = AtomicBool::new(false);
        let (send, results) = std::sync::mpsc::channel();
        std::thread::scope(|scope| {
            for _ in 0..TAG_READERS.min(todo.len()) {
                let (send, sftp) = (send.clone(), sftp.clone());
                let (todo, files, next, stop) = (&todo, &files, &next, &stop);
                scope.spawn(move || {
                    while !stop.load(Ordering::Relaxed) {
                        let Some(&i) = todo.get(next.fetch_add(1, Ordering::Relaxed)) else {
                            break;
                        };
                        let (key, size, mtime) = &files[i];
                        // Files that cannot be opened are left out.
                        let track = self
                            .path_of(key)
                            .ok()
                            .and_then(|p| sftp.open(&p).ok())
                            .map(|mut f| tags::read(&mut f, key, *size, *mtime));
                        if send.send((i, track)).is_err() {
                            break;
                        }
                    }
                });
            }
            drop(send);
            for (i, track) in results {
                if track.is_some() {
                    out[i] = track;
                    found += 1;
                }
                if !progress(found) {
                    stop.store(true, Ordering::Relaxed);
                    break;
                }
            }
        });
        // Tags read over a broken connection would be incomplete.
        if !sftp.is_alive() {
            return Err("the connection to the server broke".into());
        }
        Ok(out.into_iter().flatten().collect())
    }

    fn open(&self, key: &str) -> Result<Box<dyn bl_media::Source>, String> {
        let path = self.path_of(key)?;
        let f = self.retry(|s| s.open(&path))?;
        Ok(Box::new(f))
    }

    fn cover(&self, key: &str, _size: u32) -> Option<Vec<u8>> {
        let path = self.path_of(key).ok()?;
        let dir = path.rsplit_once('/').map_or("", |(d, _)| d);
        let dir = if dir.is_empty() { "/" } else { dir };
        // A folder image, whatever its case.
        let entries = self.retry(|s| s.read_dir(dir));
        let sftp = self.sftp().ok()?;
        if let Ok(entries) = entries {
            for want in COVER_NAMES {
                let Some(e) = entries
                    .iter()
                    .find(|e| !e.is_dir && e.name.eq_ignore_ascii_case(want))
                else {
                    continue;
                };
                let mut data = Vec::new();
                if let Ok(f) = sftp.open(&format!("{}/{}", dir.trim_end_matches('/'), e.name)) {
                    if f.take(MAX_COVER).read_to_end(&mut data).is_ok() && !data.is_empty() {
                        return Some(data);
                    }
                }
            }
        }
        let mut f = sftp.open(&path).ok()?;
        let info = bl_media::read_info(&mut f, Some(&crate::ext_of(key)), true).ok()?;
        let front = info.pictures.iter().position(|p| p.kind == 3).unwrap_or(0);
        info.pictures.into_iter().nth(front).map(|p| p.data)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn addresses() {
        assert_eq!(
            parse_url("sftp://nas.local").unwrap(),
            ("nas.local".into(), 22, String::new())
        );
        assert_eq!(
            parse_url("SFTP://anna@nas.local:2222/").unwrap(),
            ("nas.local".into(), 2222, "anna".into())
        );
        assert_eq!(
            parse_url("sftp://[fe80::1]:23").unwrap(),
            ("fe80::1".into(), 23, String::new())
        );
        assert_eq!(parse_url("sftp://[::1]").unwrap().1, 22);
        for bad in [
            "http://nas",
            "sftp://",
            "sftp://nas:0",
            "sftp://nas:x",
            "sftp://[::1",
            "sftp://[::1]x",
        ] {
            assert!(parse_url(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn pins_from_errors() {
        let e = bl_ssh::Error::UnknownHostKey("SHA256:ab+/9Z".into()).to_string();
        assert_eq!(pin_in_error(&e).as_deref(), Some("SHA256:ab+/9Z"));
        let e = bl_ssh::Error::HostKeyChanged {
            expected: "SHA256:old".into(),
            found: "SHA256:new".into(),
        }
        .to_string();
        assert_eq!(pin_in_error(&e).as_deref(), Some("SHA256:new"));
        assert_eq!(pin_in_error("login refused"), None);
    }
}
