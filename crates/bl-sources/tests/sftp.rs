//! The SFTP source against a real OpenSSH server (bl-ssh's test server).
//! Ignored by default because it needs sshd; to run it:
//!
//! ```text
//! apt-get install openssh-server   # or your system's package
//! cargo test -p bl-sources --test sftp -- --ignored
//! ```

#[path = "../../bl-ssh/tests/common/mod.rs"]
mod common;

use bl_sources::{sftp, RemoteTrack, SourceConfig};
use std::io::{Read, Seek, SeekFrom};

fn config(s: &common::Sshd, folder: &str, pin: &str) -> SourceConfig {
    SourceConfig {
        kind: "sftp".into(),
        url: format!("sftp://127.0.0.1:{}", s.port),
        user: s.user.clone(),
        secret: s.key.clone(),
        path: folder.into(),
        pin: pin.into(),
    }
}

#[test]
#[ignore = "needs sshd"]
fn sftp_source() {
    let s = common::Sshd::start("ed25519", "");
    let root = s.dir.join("Music");
    std::fs::create_dir_all(root.join("Band/Record")).unwrap();
    std::fs::create_dir_all(root.join(".trash")).unwrap();
    let song = common::wav(16_000, "First Song", "Anna; Ben", "Record");
    std::fs::write(root.join("Band/Record/01 First.wav"), &song).unwrap();
    std::fs::write(root.join("Band/Record/cover.JPG"), b"jpeg bytes").unwrap();
    std::fs::write(root.join("loose.wav"), common::wav(800, "", "", "")).unwrap();
    std::fs::write(root.join("notes.txt"), b"not audio").unwrap();
    std::fs::write(root.join(".trash/old.wav"), &song).unwrap();
    // A second way to the same folder and a loop: both end.
    std::os::unix::fs::symlink(root.join("Band"), root.join("link")).unwrap();
    std::os::unix::fs::symlink(&root, root.join("Band/loop")).unwrap();
    let folder = root.to_str().unwrap();

    // Trust on first use: the error names the fingerprint to confirm.
    let e = bl_sources::connect(&config(&s, folder, "")).err().unwrap();
    assert_eq!(
        sftp::pin_in_error(&e).as_deref(),
        Some(s.host_fp.as_str()),
        "{e}"
    );
    let e = bl_sources::connect(&config(
        &s,
        folder,
        "SHA256:47DEQpj8HBSa+/TImW+5JCeuQeRkm5NMpJWZG3hSuFU",
    ))
    .err()
    .unwrap();
    assert!(e.contains("host key changed"), "{e}");
    let mut wrong = config(&s, folder, &s.host_fp);
    wrong.secret = "not the password".into();
    assert!(bl_sources::connect(&wrong).is_err());
    assert!(bl_sources::connect(&config(&s, &format!("{folder}/missing"), &s.host_fp)).is_err());

    let src = bl_sources::connect(&config(&s, folder, &s.host_fp))
        .unwrap_or_else(|e| panic!("{e}\n{}", s.log()));
    let mut counts = Vec::new();
    let tracks = src
        .list(&|_, _, _| None, &mut |n| {
            counts.push(n);
            true
        })
        .unwrap();
    let keys: Vec<&str> = tracks.iter().map(|t| t.key.as_str()).collect();
    // A folder's files, then its subfolders.
    assert_eq!(keys, ["loose.wav", "Band/Record/01 First.wav"]);
    assert_eq!(counts.last(), Some(&2));
    let t = &tracks[1];
    assert_eq!(t.title, "First Song");
    assert_eq!(t.artists, ["Anna", "Ben"]);
    assert_eq!(t.album, "Record");
    assert_eq!(t.ext, "wav");
    assert_eq!(t.size, song.len() as u64);
    assert!(t.modified > 1_600_000_000);
    assert_eq!(t.duration_ms, 2000);
    assert_eq!(tracks[0].title, "loose");

    // Unchanged files are not read again.
    let cached = RemoteTrack {
        title: "cached".into(),
        ..t.clone()
    };
    let again = src
        .list(
            &|key, size, modified| {
                (key == t.key && size == t.size && modified == t.modified).then(|| cached.clone())
            },
            &mut |_| true,
        )
        .unwrap();
    assert_eq!(again[0].title, "loose");
    assert_eq!(again[1].title, "cached");
    // Stopping early.
    let some = src.list(&|_, _, _| None, &mut |_| false).unwrap();
    assert!(some.len() < 2);

    let mut f = src.open("Band/Record/01 First.wav").unwrap();
    let mut got = Vec::new();
    f.read_to_end(&mut got).unwrap();
    assert_eq!(got, song);
    f.seek(SeekFrom::Start(44)).unwrap();
    let mut b = [0u8; 8];
    f.read_exact(&mut b).unwrap();
    assert_eq!(b, song[44..52]);
    // The file decodes like a local one.
    let mut dec = bl_media::open(src.open("loose.wav").unwrap(), Some("wav")).unwrap();
    let mut pcm = Vec::new();
    while dec.next_chunk(&mut pcm).unwrap() {}
    assert_eq!(pcm.len(), 800);
    for bad in ["../x.wav", "/etc/passwd", "a//b", "", "Band/./x"] {
        assert!(src.open(bad).is_err(), "{bad}");
    }

    assert_eq!(
        src.cover("Band/Record/01 First.wav", 300).as_deref(),
        Some(&b"jpeg bytes"[..])
    );
    assert_eq!(src.cover("loose.wav", 300), None);
}
