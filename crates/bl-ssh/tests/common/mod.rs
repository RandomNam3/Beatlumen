//! A throwaway OpenSSH server for tests: its own host key, an authorized
//! Ed25519 key (made by ssh-keygen) and settings in a temporary folder,
//! listening on a free port of 127.0.0.1, logging in the current user
//! (root may log in when the tests run as root). SFTP is sshd's built-in
//! `internal-sftp`.
//!
//! Needs `sshd` and `ssh-keygen` (Debian/Ubuntu: `apt-get install
//! openssh-server`). Also used by bl-sources' tests.

#![allow(dead_code)]

use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

pub const SSHD: &str = "/usr/sbin/sshd";

pub struct Sshd {
    pub dir: PathBuf,
    pub port: u16,
    pub user: String,
    /// Host key fingerprint as `ssh-keygen -l` prints it.
    pub host_fp: String,
    /// The authorized private key (OpenSSH format).
    pub key: String,
    child: Child,
}

/// Whether the tests can run (sshd installed).
pub fn available() -> bool {
    Path::new(SSHD).exists()
        && Command::new("ssh-keygen")
            .arg("-?")
            .stderr(Stdio::null())
            .status()
            .is_ok()
}

fn run(cmd: &mut Command) -> String {
    let out = cmd.output().expect("run command");
    assert!(
        out.status.success(),
        "{cmd:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn keygen(kind: &str, path: &Path) {
    let mut c = Command::new("ssh-keygen");
    c.args(["-q", "-t", kind, "-N", "", "-C", "test", "-f"]);
    c.arg(path);
    if kind == "rsa" {
        c.args(["-b", "3072"]);
    }
    run(&mut c);
}

/// A fresh temporary folder.
pub fn temp_dir(tag: &str) -> PathBuf {
    static N: AtomicUsize = AtomicUsize::new(0);
    let dir = std::env::temp_dir().join(format!(
        "bl-ssh-{tag}-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::SeqCst)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

impl Sshd {
    /// Starts sshd with a host key of `host_key` type ("ed25519", "ecdsa",
    /// "rsa") and `extra` lines of configuration (e.g. "Ciphers …"; sshd
    /// takes the first value of a setting, so these override the defaults).
    pub fn start(host_key: &str, extra: &str) -> Sshd {
        let dir = temp_dir("sshd");
        keygen(host_key, &dir.join("host_key"));
        keygen("ed25519", &dir.join("user_key"));
        std::fs::copy(dir.join("user_key.pub"), dir.join("authorized_keys")).unwrap();
        let fp = run(Command::new("ssh-keygen")
            .arg("-lf")
            .arg(dir.join("host_key.pub")));
        let host_fp = fp.split_whitespace().nth(1).unwrap().to_string();
        let key = std::fs::read_to_string(dir.join("user_key")).unwrap();
        let user = run(&mut Command::new("whoami"));
        if user == "root" {
            // Privilege separation needs this folder.
            let _ = std::fs::create_dir_all("/run/sshd");
        }
        let port = TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        let d = dir.display();
        let config = format!(
            "ListenAddress 127.0.0.1\n\
             Port {port}\n\
             HostKey {d}/host_key\n\
             PidFile {d}/sshd.pid\n\
             AuthorizedKeysFile {d}/authorized_keys\n\
             UsePAM no\n\
             StrictModes no\n\
             PubkeyAuthentication yes\n\
             PasswordAuthentication yes\n\
             KbdInteractiveAuthentication no\n\
             PermitRootLogin yes\n\
             Subsystem sftp internal-sftp\n\
             {extra}\n\
             LogLevel VERBOSE\n"
        );
        std::fs::write(dir.join("sshd_config"), config).unwrap();
        let log = std::fs::File::create(dir.join("sshd.log")).unwrap();
        let child = Command::new(SSHD)
            .arg("-D")
            .arg("-e")
            .arg("-f")
            .arg(dir.join("sshd_config"))
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(log)
            .spawn()
            .expect("start sshd");
        let s = Sshd {
            dir,
            port,
            user,
            host_fp,
            key,
            child,
        };
        let start = Instant::now();
        while TcpStream::connect(("127.0.0.1", port)).is_err() {
            assert!(
                start.elapsed() < Duration::from_secs(10),
                "sshd did not start: {}",
                s.log()
            );
            std::thread::sleep(Duration::from_millis(50));
        }
        s
    }

    /// What sshd logged (for failing assertions).
    pub fn log(&self) -> String {
        std::fs::read_to_string(self.dir.join("sshd.log")).unwrap_or_default()
    }
}

impl Drop for Sshd {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// A 16-bit mono PCM WAV of `frames` samples with LIST/INFO tags.
pub fn wav(frames: usize, title: &str, artist: &str, album: &str) -> Vec<u8> {
    fn chunk(id: &[u8], data: &[u8]) -> Vec<u8> {
        let mut c = id.to_vec();
        c.extend_from_slice(&(data.len() as u32).to_le_bytes());
        c.extend_from_slice(data);
        if data.len() % 2 == 1 {
            c.push(0);
        }
        c
    }
    let mut fmt = Vec::new();
    fmt.extend_from_slice(&1u16.to_le_bytes()); // PCM
    fmt.extend_from_slice(&1u16.to_le_bytes()); // mono
    fmt.extend_from_slice(&8000u32.to_le_bytes());
    fmt.extend_from_slice(&16000u32.to_le_bytes());
    fmt.extend_from_slice(&2u16.to_le_bytes());
    fmt.extend_from_slice(&16u16.to_le_bytes());
    let mut info = b"INFO".to_vec();
    for (id, v) in [(b"INAM", title), (b"IART", artist), (b"IPRD", album)] {
        if !v.is_empty() {
            info.extend(chunk(id, format!("{v}\0").as_bytes()));
        }
    }
    let pcm: Vec<u8> = (0..frames)
        .flat_map(|i| (((i * 7919) % 65536) as u16).to_le_bytes())
        .collect();
    let mut body = b"WAVE".to_vec();
    body.extend(chunk(b"fmt ", &fmt));
    body.extend(chunk(b"LIST", &info));
    body.extend(chunk(b"data", &pcm));
    chunk(b"RIFF", &body)
}
