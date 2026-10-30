//! The client against a real OpenSSH server (see `common`). Ignored by
//! default because they need sshd; to run them:
//!
//! ```text
//! apt-get install openssh-server   # or your system's package
//! cargo test -p bl-ssh --test sshd -- --ignored
//! ```
//!
//! Each test starts its own sshd on a free port of 127.0.0.1 with keys in
//! a temporary folder and logs in as the current user.

mod common;

use bl_ssh::{Auth, Cipher, Error, Options, PrivateKey, Session, Sftp};
use common::Sshd;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

fn connect_with(s: &Sshd, pin: &str, opts: &Options) -> Result<Sftp, Error> {
    let auth = Auth::from_secret(&s.key)?;
    Session::connect_with("127.0.0.1", s.port, pin, &s.user, &auth, opts)?.sftp()
}

fn connect(s: &Sshd) -> Sftp {
    connect_with(s, &s.host_fp, &Options::default()).unwrap_or_else(|e| panic!("{e}\n{}", s.log()))
}

/// Not quite random file contents.
fn pattern(len: usize, seed: u32) -> Vec<u8> {
    let mut x = seed.wrapping_mul(2_654_435_761).max(1);
    (0..len)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 17;
            x ^= x << 5;
            x as u8
        })
        .collect()
}

fn path(p: &Path) -> String {
    p.to_str().unwrap().to_string()
}

#[test]
#[ignore = "needs sshd"]
fn host_key_is_pinned() {
    let s = Sshd::start("ed25519", "");
    // No pin: the error carries the fingerprint ssh-keygen shows.
    let e = connect_with(&s, "", &Options::default()).err().unwrap();
    assert_eq!(e, Error::UnknownHostKey(s.host_fp.clone()));
    assert!(e.to_string().contains(&s.host_fp), "{e}");
    let wrong = "SHA256:47DEQpj8HBSa+/TImW+5JCeuQeRkm5NMpJWZG3hSuFU";
    match connect_with(&s, wrong, &Options::default()) {
        Err(Error::HostKeyChanged { expected, found }) => {
            assert_eq!(expected, wrong);
            assert_eq!(found, s.host_fp);
        }
        other => panic!("{:?}", other.err()),
    }
    let sftp = connect(&s);
    assert_eq!(sftp.fingerprint(), s.host_fp);
    // Without the prefix as well.
    connect_with(
        &s,
        s.host_fp.trim_start_matches("SHA256:"),
        &Options::default(),
    )
    .unwrap();
}

#[test]
#[ignore = "needs sshd"]
fn ecdsa_and_rsa_host_keys() {
    for (kind, extra) in [
        ("ecdsa", ""),
        ("rsa", ""),
        ("rsa", "HostKeyAlgorithms rsa-sha2-256"),
    ] {
        let s = Sshd::start(kind, extra);
        let sftp = connect_with(&s, &s.host_fp, &Options::default())
            .unwrap_or_else(|e| panic!("{kind} {extra}: {e}\n{}", s.log()));
        assert!(sftp.realpath(".").unwrap().starts_with('/'));
    }
}

#[test]
#[ignore = "needs sshd"]
fn wrong_credentials_are_refused() {
    let s = Sshd::start("ed25519", "");
    let other_dir = common::temp_dir("key");
    std::process::Command::new("ssh-keygen")
        .args(["-q", "-t", "ed25519", "-N", "", "-f"])
        .arg(other_dir.join("k"))
        .status()
        .unwrap();
    let other =
        PrivateKey::from_openssh(&std::fs::read_to_string(other_dir.join("k")).unwrap()).unwrap();
    let _ = std::fs::remove_dir_all(&other_dir);
    let e = Session::connect("127.0.0.1", s.port, &s.host_fp, &s.user, &Auth::key(other))
        .err()
        .unwrap();
    assert!(matches!(e, Error::Auth(_)), "{e}");
    let e = Session::connect(
        "127.0.0.1",
        s.port,
        &s.host_fp,
        &s.user,
        &Auth::password("not the password"),
    )
    .err()
    .unwrap();
    assert!(matches!(e, Error::Auth(_)), "{e}");
    // Nobody listening.
    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let e = Session::connect("127.0.0.1", port, "x", "u", &Auth::password("p"))
        .err()
        .unwrap();
    assert!(matches!(e, Error::Io(_)), "{e}");
}

/// Folders, attributes and reads with seeks, with the given cipher.
fn exercise(s: &Sshd, sftp: &Sftp) {
    let root = s.dir.join("music");
    std::fs::create_dir_all(root.join("Album")).unwrap();
    let small = common::wav(100, "Small", "Artist", "Album");
    let big = pattern(1_000_000, 7);
    std::fs::write(root.join("a.wav"), &small).unwrap();
    std::fs::write(root.join("Album/big.bin"), &big).unwrap();
    std::fs::write(root.join(".hidden"), b"x").unwrap();

    let canonical = sftp.realpath(&path(&root)).unwrap();
    assert_eq!(canonical, path(&root.canonicalize().unwrap()));
    let mut entries = sftp.read_dir(&canonical).unwrap();
    entries.sort_by(|a, b| a.name.cmp(&b.name));
    let names: Vec<(&str, bool)> = entries
        .iter()
        .map(|e| (e.name.as_str(), e.is_dir))
        .collect();
    assert_eq!(
        names,
        [(".hidden", false), ("Album", true), ("a.wav", false)]
    );
    assert_eq!(entries[2].size, small.len() as u64);
    assert!(entries[2].mtime > 1_600_000_000);

    let m = sftp.stat(&format!("{canonical}/Album/big.bin")).unwrap();
    assert!(m.is_file());
    assert_eq!(m.size, Some(big.len() as u64));
    assert!(sftp.stat(&format!("{canonical}/Album")).unwrap().is_dir());
    let e = sftp.stat(&format!("{canonical}/missing")).unwrap_err();
    assert!(e.is_not_found(), "{e}");
    assert!(sftp.open(&format!("{canonical}/missing")).is_err());

    let mut f = sftp.open(&format!("{canonical}/a.wav")).unwrap();
    let mut got = Vec::new();
    f.read_to_end(&mut got).unwrap();
    assert_eq!(got, small);

    let mut f = sftp.open(&format!("{canonical}/Album/big.bin")).unwrap();
    assert_eq!(f.len().unwrap(), big.len() as u64);
    let mut got = Vec::new();
    f.read_to_end(&mut got).unwrap();
    assert!(got == big, "whole file differs");
    for (at, n) in [
        (100_000u64, 1000usize),
        (5, 70_000),
        (999_990, 10),
        (40_000, 1),
    ] {
        f.seek(SeekFrom::Start(at)).unwrap();
        let mut buf = vec![0u8; n];
        f.read_exact(&mut buf).unwrap();
        assert!(buf[..] == big[at as usize..at as usize + n], "at {at}");
    }
    assert_eq!(f.seek(SeekFrom::End(-3)).unwrap(), big.len() as u64 - 3);
    let mut tail = Vec::new();
    f.read_to_end(&mut tail).unwrap();
    assert_eq!(tail, big[big.len() - 3..]);
    f.seek(SeekFrom::Current(-10)).unwrap();
    let mut buf = [0u8; 4];
    f.read_exact(&mut buf).unwrap();
    assert_eq!(buf, big[big.len() - 10..big.len() - 6]);
    f.seek(SeekFrom::Start(5_000_000)).unwrap();
    assert_eq!(f.read(&mut buf).unwrap(), 0);
    drop(f);

    // Several threads reading at once over the one connection.
    let path = format!("{canonical}/Album/big.bin");
    std::thread::scope(|scope| {
        for t in 0..4u64 {
            let sftp = sftp.clone();
            let (path, big) = (&path, &big);
            scope.spawn(move || {
                let mut f = sftp.open(path).unwrap();
                for i in 0..10u64 {
                    let at = (t * 200_000 + i * 17_000) % 900_000;
                    f.seek(SeekFrom::Start(at)).unwrap();
                    let mut buf = vec![0u8; 50_000];
                    f.read_exact(&mut buf).unwrap();
                    assert!(buf[..] == big[at as usize..at as usize + 50_000]);
                }
            });
        }
    });
    assert!(sftp.is_alive());
}

#[test]
#[ignore = "needs sshd"]
fn sftp_with_each_cipher() {
    let s = Sshd::start("ed25519", "");
    for c in Cipher::ALL {
        let opts = Options {
            ciphers: vec![c],
            ..Options::default()
        };
        let sftp =
            connect_with(&s, &s.host_fp, &opts).unwrap_or_else(|e| panic!("{e}\n{}", s.log()));
        assert_eq!(sftp.cipher(), Some(c));
        exercise(&s, &sftp);
    }
}

#[test]
#[ignore = "needs sshd"]
fn server_side_cipher_choice() {
    let s = Sshd::start("ed25519", "Ciphers aes256-gcm@openssh.com");
    let sftp = connect(&s);
    assert_eq!(sftp.cipher(), Some(Cipher::Aes256Gcm));
    exercise(&s, &sftp);
}

/// The server renews the keys every 256 KiB (and the session goes on).
#[test]
#[ignore = "needs sshd"]
fn rekeying() {
    let s = Sshd::start("ed25519", "RekeyLimit 256K\nLogLevel DEBUG1");
    let data = pattern(3_000_000, 3);
    let file = s.dir.join("big.bin");
    std::fs::write(&file, &data).unwrap();
    for c in Cipher::ALL {
        let opts = Options {
            ciphers: vec![c],
            ..Options::default()
        };
        let sftp = connect_with(&s, &s.host_fp, &opts).unwrap();
        for _ in 0..2 {
            let mut got = Vec::new();
            sftp.open(&path(&file))
                .unwrap()
                .read_to_end(&mut got)
                .unwrap_or_else(|e| panic!("{c:?}: {e}\n{}", s.log()));
            assert!(got == data, "{c:?}");
        }
        assert!(sftp.is_alive());
    }
    let rekeys = s.log().matches("SSH2_MSG_NEWKEYS received").count();
    // One initial exchange per connection, the rest are renewals.
    assert!(rekeys > 10, "{rekeys} key exchanges");
}
