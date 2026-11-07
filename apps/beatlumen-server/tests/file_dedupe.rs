//! Music in the encrypted files is stored once, however many accounts have it,
//! and a deleted file waits in its account's trash. Everyone keeps their own
//! names, nobody can tell whether others have the same file, and a song leaves
//! the shared library per account.

mod common;

use bl_json::Value;
use bl_net::client;
use common::fake_extension::{extension, media, Asked};
use common::{wav, App};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

const DAY: u64 = 24 * 3600;

struct World {
    dir: PathBuf,
    data: PathBuf,
    server: beatlumen_server::Server,
    url: String,
}

impl World {
    fn new(name: &str) -> World {
        let dir = std::env::temp_dir().join(format!("bl-dedupe-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let data = dir.join("data");
        let server = beatlumen_server::start(beatlumen_server::Config {
            data_dir: data.clone(),
            bind: "127.0.0.1:0".into(),
            rescan_interval: 0,
            ..Default::default()
        })
        .unwrap();
        let url = format!("http://{}", server.addr);
        World {
            dir,
            data,
            server,
            url,
        }
    }

    fn account(&self, name: &str) -> App {
        let invite = beatlumen_server::vault::create_invite(&self.data, 1).unwrap();
        // Tests run side by side: a folder of its own for each world's accounts.
        let world = self.dir.file_name().unwrap().to_string_lossy().into_owned();
        let mut a = App::new(&format!("{world}-{name}"));
        a.connect(&self.url);
        a.cmd(
            Value::object()
                .with("cmd", "vault.register")
                .with("invite", invite)
                .with("user", name)
                .with("password", "a long password 12"),
        );
        assert_eq!(a.ready().get("vault").str_or("error", ""), "");
        a.cmd(Value::object().with("cmd", "vault.dismissCode"));
        // The list brings the server's salt.
        a.cmd(Value::object().with("cmd", "vault.refresh"));
        settle(&mut a);
        a
    }

    /// Stored bytes on the server's disk.
    fn blobs(&self) -> Vec<PathBuf> {
        std::fs::read_dir(self.data.join("vault").join("blobs"))
            .map(|d| d.flatten().map(|e| e.path()).collect())
            .unwrap_or_default()
    }

    fn purge(&self, days_later: u64) {
        let now = beatlumen_server::util::now_secs() + days_later * DAY;
        self.server.state.vault.purge(now);
    }
}

impl Drop for World {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn settle(a: &mut App) {
    for _ in 0..15 {
        a.state();
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn until(a: &mut App, what: &str, pred: impl Fn(&mut App) -> bool) {
    let t0 = Instant::now();
    while !pred(a) {
        a.state();
        assert!(t0.elapsed() < Duration::from_secs(30), "timeout: {what}");
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn upload(a: &mut App, name: &str, data: &[u8]) -> String {
    let path = a.dir.join(name);
    std::fs::write(&path, data).unwrap();
    let before = a.files().len();
    a.cmd(
        Value::object()
            .with("cmd", "vault.upload")
            .with("paths", vec![path.display().to_string()]),
    );
    until(a, "the upload", move |a| {
        a.state()
            .get("vault")
            .get("uploads")
            .as_array()
            .is_some_and(|u| u.is_empty())
            && a.files().len() == before + 1
            && a.files().iter().all(|f| f.bool_or("complete", false))
    });
    a.files()
        .iter()
        .find(|f| f.str_or("name", "") == name)
        .unwrap()
        .str_or("id", "")
        .trim_start_matches("vault:")
        .to_string()
}

fn names(a: &mut App) -> Vec<String> {
    a.files()
        .iter()
        .map(|f| f.str_or("name", "").to_string())
        .collect()
}

fn trash(a: &mut App) -> Vec<String> {
    a.cmd(Value::object().with("cmd", "vault.trash"))
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|f| f.str_or("name", "").to_string())
        .collect()
}

fn cmd(a: &mut App, name: &str, id: &str) {
    a.cmd(Value::object().with("cmd", name).with("id", id));
    settle(a);
    a.cmd(Value::object().with("cmd", "vault.refresh"));
    settle(a);
}

fn plays(a: &mut App, id: &str) {
    a.cmd(
        Value::object()
            .with("cmd", "play")
            .with("ids", vec![format!("vault:{id}")])
            .with("start", 0u64),
    );
    until(a, "playing", |a| a.state().str_or("play", "") == "playing");
    a.cmd(Value::object().with("cmd", "stop"));
}

fn size_on_disk(p: &Path) -> u64 {
    std::fs::metadata(p).map_or(0, |m| m.len())
}

#[test]
fn the_same_song_is_stored_once_and_everyone_keeps_their_own_name() {
    let w = World::new("once");
    let mut alice = w.account("alice");
    let mut bob = w.account("bob");
    let song = wav(4);

    let a = upload(&mut alice, "Mein Lied.wav", &song);
    assert_eq!(w.blobs().len(), 1);
    let stored = size_on_disk(&w.blobs()[0]);
    let b = upload(&mut bob, "track01 (bobs copy).wav", &song);
    assert_ne!(a, b, "two files");
    assert_eq!(w.blobs().len(), 1, "one copy of the bytes");
    assert_eq!(size_on_disk(&w.blobs()[0]), stored);

    // Each sees their own name, and the song plays for both.
    assert_eq!(names(&mut alice), ["Mein Lied.wav"]);
    assert_eq!(names(&mut bob), ["track01 (bobs copy).wav"]);
    plays(&mut alice, &a);
    plays(&mut bob, &b);

    // It counts in full for each: the quota tells nobody anything.
    alice.cmd(Value::object().with("cmd", "vault.refresh"));
    bob.cmd(Value::object().with("cmd", "vault.refresh"));
    settle(&mut alice);
    settle(&mut bob);
    let used = |a: &mut App| a.state().get("vault").get("used").as_u64().unwrap_or(0);
    assert_eq!(used(&mut alice), used(&mut bob));
    assert!(used(&mut alice) > 0);

    // The stored bytes are encrypted, and a file record gives no name away.
    let bytes = std::fs::read(&w.blobs()[0]).unwrap();
    assert_ne!(
        &bytes[60..60 + song.len().min(1000)],
        &song[..song.len().min(1000)]
    );
    for e in std::fs::read_dir(w.data.join("vault").join("files"))
        .unwrap()
        .flatten()
    {
        let text = std::fs::read_to_string(e.path()).unwrap();
        assert!(!text.contains("Lied") && !text.contains("bobs"), "{text}");
    }
}

#[test]
fn other_files_and_other_songs_are_not_merged() {
    let w = World::new("apart");
    let mut alice = w.account("alice");
    let mut bob = w.account("bob");
    // Not music: a key of its own every time.
    upload(&mut alice, "notes.txt", b"the same words");
    upload(&mut bob, "notes.txt", b"the same words");
    assert_eq!(w.blobs().len(), 2, "not music, not merged");
    // Another song.
    upload(&mut alice, "a.wav", &wav(2));
    upload(&mut bob, "b.wav", &wav(3));
    assert_eq!(w.blobs().len(), 4);
}

#[test]
fn deleting_leaves_the_others_their_file_and_it_can_come_back() {
    let w = World::new("trash");
    let mut alice = w.account("alice");
    let mut bob = w.account("bob");
    let song = wav(3);
    let a = upload(&mut alice, "a.wav", &song);
    let b = upload(&mut bob, "b.wav", &song);

    cmd(&mut alice, "vault.delete", &a);
    assert!(names(&mut alice).is_empty());
    assert_eq!(trash(&mut alice), ["a.wav"]);
    assert_eq!(names(&mut bob), ["b.wav"], "bob keeps his");
    plays(&mut bob, &b);

    // Back it comes.
    cmd(&mut alice, "vault.restore", &a);
    assert_eq!(names(&mut alice), ["a.wav"]);
    assert!(trash(&mut alice).is_empty());
    plays(&mut alice, &a);
}

#[test]
fn twenty_nine_days_in_the_trash_and_the_bytes_go_a_day_later() {
    let w = World::new("days");
    let mut alice = w.account("alice");
    let a = upload(&mut alice, "a.wav", &wav(3));
    cmd(&mut alice, "vault.delete", &a);

    w.purge(28);
    alice.cmd(Value::object().with("cmd", "vault.refresh"));
    settle(&mut alice);
    assert_eq!(trash(&mut alice), ["a.wav"], "still there on day 28");

    w.purge(29);
    alice.cmd(Value::object().with("cmd", "vault.refresh"));
    settle(&mut alice);
    assert!(trash(&mut alice).is_empty(), "no way back from day 29");
    assert_eq!(w.blobs().len(), 1, "but the bytes stay a day longer");

    w.purge(30);
    assert!(w.blobs().is_empty(), "and then they are gone");
}

#[test]
fn the_way_back_says_nothing_about_others_having_the_file() {
    // With and without someone else holding the same song, Alice sees the same.
    for bob_has_it in [false, true] {
        let w = World::new(if bob_has_it { "with" } else { "without" });
        let mut alice = w.account("alice");
        let mut bob = w.account("bob");
        let song = wav(3);
        let a = upload(&mut alice, "a.wav", &song);
        if bob_has_it {
            upload(&mut bob, "b.wav", &song);
        }
        cmd(&mut alice, "vault.delete", &a);
        w.purge(28);
        cmd(&mut alice, "vault.restore", &a);
        assert_eq!(names(&mut alice), ["a.wav"], "bob has it: {bob_has_it}");
        cmd(&mut alice, "vault.delete", &a);
        w.purge(29);
        alice.cmd(Value::object().with("cmd", "vault.refresh"));
        settle(&mut alice);
        assert!(trash(&mut alice).is_empty(), "bob has it: {bob_has_it}");
        assert!(names(&mut alice).is_empty(), "bob has it: {bob_has_it}");
        // The other's file is untouched.
        if bob_has_it {
            w.purge(365);
            assert_eq!(w.blobs().len(), 1);
            assert_eq!(names(&mut bob), ["b.wav"]);
        }
    }
}

#[test]
fn the_last_one_to_delete_starts_the_time() {
    let w = World::new("last");
    let mut alice = w.account("alice");
    let mut bob = w.account("bob");
    let song = wav(3);
    let a = upload(&mut alice, "a.wav", &song);
    let b = upload(&mut bob, "b.wav", &song);
    cmd(&mut alice, "vault.delete", &a);
    w.purge(31);
    assert_eq!(w.blobs().len(), 1, "bob still has it");
    cmd(&mut bob, "vault.delete", &b);
    w.purge(29);
    assert_eq!(w.blobs().len(), 1, "a day longer than his trash");
    w.purge(30);
    assert!(w.blobs().is_empty());
}

#[test]
fn deleting_for_good_ends_the_entry_but_not_the_bytes_at_once() {
    let w = World::new("forever");
    let mut alice = w.account("alice");
    let a = upload(&mut alice, "a.wav", &wav(3));
    cmd(&mut alice, "vault.delete", &a);
    cmd(&mut alice, "vault.deleteForever", &a);
    assert!(trash(&mut alice).is_empty());
    assert_eq!(w.blobs().len(), 1);
    w.purge(30);
    assert!(w.blobs().is_empty());
}

#[test]
fn nobody_can_get_at_a_file_by_guessing_or_asking() {
    let w = World::new("access");
    let mut alice = w.account("alice");
    let a = upload(&mut alice, "a.wav", &wav(3));
    // There is no way to ask for stored bytes by their hash.
    let hash = w.blobs()[0]
        .file_name()
        .unwrap()
        .to_str()
        .unwrap()
        .to_string();
    for path in [
        format!("/api/vault/blobs/{hash}"),
        format!("/api/vault/files/{hash}/data"),
        format!("/api/vault/files/{a}/data"),
    ] {
        let r = client::request(
            "GET",
            &format!("{}{path}", w.url),
            &[],
            &[],
            &client::Options::default(),
        )
        .unwrap();
        assert!(r.status == 401 || r.status == 404, "{path}: {}", r.status);
    }
}

// ---- the shared library -------------------------------------------------------

fn library_ids(a: &mut App) -> Vec<String> {
    a.cmd(Value::object().with("cmd", "library.tracks"))
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|t| t.str_or("id", "").to_string())
        .collect()
}

fn library_cmd(a: &mut App, name: &str, id: &str) {
    a.cmd(Value::object().with("cmd", name).with("ids", vec![id]));
    settle(a);
}

/// Two accounts on a server with one song of its own and a fake extension.
fn library_world(name: &str) -> Option<(World, App, App, PathBuf)> {
    let dir = std::env::temp_dir().join(format!("bl-dedupe-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("music")).unwrap();
    let (webm, _) = media(&dir)?;
    let own = dir.join("music").join("own.wav");
    std::fs::write(&own, wav(2)).unwrap();
    let ext = extension(2, Some(webm), Arc::new(Asked::default()));
    let data = dir.join("data");
    let server = beatlumen_server::start(beatlumen_server::Config {
        data_dir: data.clone(),
        music_dirs: vec![dir.join("music")],
        bind: "127.0.0.1:0".into(),
        rescan_interval: 0,
        extensions: vec![("tube".into(), ext)],
        ..Default::default()
    })
    .unwrap();
    assert!(server.wait_for_scan(Duration::from_secs(10)));
    let url = format!("http://{}", server.addr);
    let w = World {
        dir,
        data,
        server,
        url,
    };
    let mut alice = w.account("alice");
    let mut bob = w.account("bob");
    until(&mut alice, "the library", |a| library_ids(a).len() == 1);
    until(&mut bob, "the library", |a| library_ids(a).len() == 1);
    Some((w, alice, bob, own))
}

#[test]
fn removing_a_song_from_the_library_is_for_one_account_and_the_folder_stays() {
    let Some((w, mut alice, mut bob, own)) = library_world("own") else {
        eprintln!("ffmpeg not found, skipping");
        return;
    };
    let id = library_ids(&mut alice)[0].clone();
    library_cmd(&mut alice, "library.remove", &id);
    assert!(library_ids(&mut alice).is_empty(), "gone for alice");
    let t = alice.cmd(Value::object().with("cmd", "library.trash"));
    assert_eq!(t.as_array().unwrap().len(), 1);
    assert_eq!(library_ids(&mut bob), [id.clone()], "not for bob");
    // Nor when bob's app asks the server again.
    bob.cmd(Value::object().with("cmd", "library.rescan"));
    settle(&mut bob);
    assert_eq!(library_ids(&mut bob), [id.clone()], "still not for bob");
    let t = bob.cmd(Value::object().with("cmd", "library.trash"));
    assert!(t.as_array().unwrap().is_empty(), "nothing in bob's trash");

    // The admin's folder is never touched, however long it takes.
    let later = beatlumen_server::util::now_secs() + 1000 * DAY;
    w.server.state.extensions.cleanup(&w.server.state, later);
    assert!(own.is_file());

    // It can always come back, also long after the 29 days.
    let state = &w.server.state;
    let list = beatlumen_server::routes::trash_list(state, "alice", later);
    let items = list.get("items").as_array().unwrap();
    assert_eq!(items.len(), 1);
    assert!(items[0].bool_or("lasting", false));
    assert_eq!(items[0].f64_or("secondsLeft", 1.0), 0.0);

    // It stays hidden after a restart of the app, and it can come back.
    alice.cmd(Value::object().with("cmd", "library.rescan"));
    settle(&mut alice);
    assert!(library_ids(&mut alice).is_empty());
    library_cmd(&mut alice, "library.restore", &id);
    assert_eq!(library_ids(&mut alice), [id]);
    let t = alice.cmd(Value::object().with("cmd", "library.trash"));
    assert!(t.as_array().unwrap().is_empty());
}

fn add(a: &mut App, item: &str) -> String {
    a.cmd(
        Value::object()
            .with("cmd", "extensions.search")
            .with("query", "anything"),
    );
    until(a, "results", |a| {
        !a.state().get("extensions").bool_or("searching", true)
    });
    let before = library_ids(a).len();
    a.cmd(
        Value::object()
            .with("cmd", "extensions.add")
            .with("ids", vec![format!("extitem:tube:{item}")]),
    );
    // Known only after a search shows it; that is what the fake extension offers.
    until(a, "the song in the library", move |a| {
        library_ids(a).len() > before
    });
    let ids = library_ids(a);
    ids.into_iter()
        .find(|i| {
            a.cmd(
                Value::object()
                    .with("cmd", "library.tracks")
                    .with("ids", vec![i.as_str()]),
            )
            .as_array()
            .is_some_and(|t| t[0].str_or("title", "") != "own")
        })
        .unwrap()
}

fn import_file(w: &World) -> Vec<PathBuf> {
    std::fs::read_dir(w.data.join("extensions").join("library").join("tube"))
        .map(|d| d.flatten().map(|e| e.path()).collect())
        .unwrap_or_default()
}

#[test]
fn a_song_of_an_extension_goes_when_it_is_in_no_library_any_more() {
    let Some((w, mut alice, mut bob, _)) = library_world("module") else {
        eprintln!("ffmpeg not found, skipping");
        return;
    };
    let state = &w.server.state;
    let later = |days: u64| beatlumen_server::util::now_secs() + days * DAY;
    let id = add(&mut alice, "s:1");
    // A song of an extension is in the library of those who brought it in.
    bob.cmd(Value::object().with("cmd", "library.rescan"));
    settle(&mut bob);
    assert_eq!(
        library_ids(&mut bob).len(),
        1,
        "nothing of alice's in bob's library"
    );
    add(&mut bob, "s:1");
    assert_eq!(library_ids(&mut bob).len(), 2, "until he adds it himself");
    assert_eq!(import_file(&w).len(), 1, "one file for both");

    // Alice takes it out: bob still has it, so it stays long past her trash.
    library_cmd(&mut alice, "library.remove", &id);
    assert_eq!(state.extensions.cleanup(state, later(40)), 0);
    assert_eq!(import_file(&w).len(), 1);

    // Her trash ends after 29 days as if the file were gone: she cannot tell.
    let in_trash = |days: u64| {
        let list = beatlumen_server::routes::trash_list(state, "alice", later(days));
        list.get("items").as_array().unwrap().len()
    };
    assert_eq!(in_trash(28), 1);
    assert_eq!(in_trash(30), 0);
    beatlumen_server::routes::restore_tracks(state, "alice", &[id.as_str()], later(30));
    assert_eq!(in_trash(28), 1, "too late to bring it back");

    // Bob as well: 30 days from then, not a day sooner.
    library_cmd(&mut bob, "library.remove", &id);
    assert_eq!(state.extensions.cleanup(state, later(29)), 0);
    assert_eq!(import_file(&w).len(), 1, "the trash still holds it");
    assert_eq!(state.extensions.cleanup(state, later(31)), 1);
    assert!(import_file(&w).is_empty());
}

#[test]
fn getting_a_song_back_from_the_trash_keeps_it_going() {
    let Some((w, mut alice, _, _)) = library_world("back") else {
        eprintln!("ffmpeg not found, skipping");
        return;
    };
    let state = &w.server.state;
    let later = |days: u64| beatlumen_server::util::now_secs() + days * DAY;
    let id = add(&mut alice, "s:1");
    library_cmd(&mut alice, "library.remove", &id);
    library_cmd(&mut alice, "library.restore", &id);
    assert_eq!(
        library_ids(&mut alice).iter().filter(|i| **i == id).count(),
        1
    );
    assert_eq!(
        state.extensions.cleanup(state, later(1000)),
        0,
        "it was wanted"
    );
    assert_eq!(import_file(&w).len(), 1);
}
