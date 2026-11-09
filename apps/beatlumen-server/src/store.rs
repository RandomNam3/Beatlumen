//! The server's own data in one embedded database: `<data>/beatlumen.redb`
//! (redb, pure Rust, no database server).
//!
//! What lives in it: the accounts and their files' metadata, logins and
//! sessions, the admin password and settings, playlists, favorites, listening
//! history, labels, presets and what else belongs to an account or the server
//! password. Large contents and caches stay files: encrypted file contents,
//! covers, analyses, scaled images, uploads, song texts, the library index and
//! speech models.
//!
//! One table maps keys like paths (`vault/user/<name>`, `acct/<name>/favorite/<id>`)
//! to bytes, mostly JSON. The modules keep what they read in memory as before
//! and write through [`Store::write`]: one transaction that is committed (and
//! on disk) when the closure returns `Ok`, and leaves nothing behind
//! otherwise, also when the process dies in the middle of it.
//!
//! While the server runs, the database is locked against other processes.
//! It writes a consistent copy to `backup/beatlumen.redb` within an hour of a
//! change (see [`Store::copy_to`]), for backups of a running server.

use bl_json::Value;
use redb::{Database, ReadableDatabase, ReadableTable, TableDefinition};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, RwLock, RwLockReadGuard, Weak};

/// The database file in the data folder.
pub const FILE_NAME: &str = "beatlumen.redb";
/// The copy for backups, in the data folder.
pub const BACKUP_FILE: &str = "backup/beatlumen.redb";

const DATA: TableDefinition<&str, &[u8]> = TableDefinition::new("data");
/// The layout of the keys and values; a server refuses a database with a
/// higher one (written by a newer version).
const FORMAT: u64 = 1;
const FORMAT_KEY: &str = "meta/format";
const HEALTH_KEY: &str = "meta/health";
/// Pages kept in memory (the modules hold their data themselves).
const CACHE_BYTES: usize = 32 * 1024 * 1024;

/// What went wrong reading or writing the database.
#[derive(Debug, Clone, PartialEq)]
pub struct Error(pub String);

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl Error {
    /// Ends a transaction without a change (from inside [`Store::write`]).
    pub fn abort(why: impl Into<String>) -> Error {
        Error(why.into())
    }
}

fn err(e: impl std::fmt::Display) -> Error {
    Error(e.to_string())
}

/// The keys starting with `prefix`: from `prefix` up to the first key that
/// is greater than all of them.
fn upper_bound(prefix: &str) -> Option<String> {
    let mut b = prefix.as_bytes().to_vec();
    while let Some(last) = b.pop() {
        if last < 0x7f {
            b.push(last + 1);
            // Keys are UTF-8; an ASCII byte keeps it so.
            return String::from_utf8(b).ok();
        }
    }
    None
}

/// The changes of one transaction.
pub struct Tx<'t> {
    table: redb::Table<'t, &'static str, &'static [u8]>,
}

impl Tx<'_> {
    pub fn put(&mut self, key: &str, value: &[u8]) -> Result<(), Error> {
        self.table.insert(key, value).map(|_| ()).map_err(err)
    }

    pub fn put_json(&mut self, key: &str, v: &Value) -> Result<(), Error> {
        self.put(key, v.to_json().as_bytes())
    }

    /// Whether it was there.
    pub fn delete(&mut self, key: &str) -> Result<bool, Error> {
        self.table.remove(key).map(|old| old.is_some()).map_err(err)
    }

    /// Removes every key that starts with `prefix`; how many there were.
    pub fn delete_prefix(&mut self, prefix: &str) -> Result<usize, Error> {
        let mut n = 0usize;
        let keep = |k: &str, _: &[u8]| {
            let gone = k.starts_with(prefix);
            n += gone as usize;
            !gone
        };
        match upper_bound(prefix) {
            Some(end) => self.table.retain_in(prefix..end.as_str(), keep),
            None => self.table.retain_in(prefix.., keep),
        }
        .map_err(err)?;
        Ok(n)
    }

    /// What the transaction sees (its own changes included).
    pub fn get(&self, key: &str) -> Result<Option<Vec<u8>>, Error> {
        Ok(self
            .table
            .get(key)
            .map_err(err)?
            .map(|v| v.value().to_vec()))
    }

    /// The keys (without `prefix`) and values under `prefix`, in key order.
    pub fn list(&self, prefix: &str) -> Result<Vec<(String, Vec<u8>)>, Error> {
        list_in(&self.table, prefix)
    }
}

fn list_in(
    table: &impl ReadableTable<&'static str, &'static [u8]>,
    prefix: &str,
) -> Result<Vec<(String, Vec<u8>)>, Error> {
    let range = match upper_bound(prefix) {
        Some(end) => table.range(prefix..end.as_str()),
        None => table.range(prefix..),
    }
    .map_err(err)?;
    let mut out = Vec::new();
    for item in range {
        let (k, v) = item.map_err(err)?;
        if let Some(rest) = k.value().strip_prefix(prefix) {
            out.push((rest.to_string(), v.value().to_vec()));
        }
    }
    Ok(out)
}

/// The open database.
pub struct Store {
    /// `None` once closed ([`Store::close`]): then everything fails.
    db: RwLock<Option<Database>>,
    path: PathBuf,
    /// It was not closed properly last time and redb repaired it on open.
    repaired: bool,
    /// Something was written since the last copy for backups.
    changed: AtomicBool,
    /// Which file it is (another one may replace it under the same name).
    #[cfg(unix)]
    file_id: (u64, u64),
}

/// Databases open in this process: a second server on the same data folder
/// (the tests start several) shares the handle, as it shared the files
/// before. redb allows one handle per file.
static OPEN: Mutex<Vec<(PathBuf, Weak<Store>)>> = Mutex::new(Vec::new());

#[cfg(unix)]
fn file_id(path: &Path) -> Option<(u64, u64)> {
    use std::os::unix::fs::MetadataExt;
    std::fs::metadata(path).ok().map(|m| (m.dev(), m.ino()))
}

impl Store {
    /// The database of the data folder `data` (created when missing).
    pub fn open(data: &Path) -> Result<Arc<Store>, String> {
        let path = data.join(FILE_NAME);
        let mut open = OPEN.lock().unwrap_or_else(|e| e.into_inner());
        open.retain(|(_, w)| w.strong_count() > 0);
        if let Some(s) = open
            .iter()
            .filter(|(p, _)| *p == path)
            .find_map(|(_, w)| w.upgrade())
        {
            #[cfg(unix)]
            let same = file_id(&path) == Some(s.file_id);
            #[cfg(not(unix))]
            let same = true;
            if same {
                return Ok(s);
            }
        }
        let s = Arc::new(Store::open_path(&path)?);
        open.push((path, Arc::downgrade(&s)));
        Ok(s)
    }

    /// Opens (or creates) the database file at `path`, without sharing.
    pub fn open_path(path: &Path) -> Result<Store, String> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)
                .map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
        }
        let (db, repaired) = Self::create(path)?;
        let store = Store {
            db: RwLock::new(Some(db)),
            path: path.to_path_buf(),
            repaired,
            changed: AtomicBool::new(false),
            #[cfg(unix)]
            file_id: file_id(path).unwrap_or_default(),
        };
        store
            .prepare()
            .map_err(|e| format!("{}: {e}", path.display()))?;
        Ok(store)
    }

    /// Opens or creates the file; whether redb repaired it.
    fn create(path: &Path) -> Result<(Database, bool), String> {
        let repairing = Arc::new(AtomicBool::new(false));
        let seen = repairing.clone();
        let shown = path.display().to_string();
        let db = Database::builder()
            .set_cache_size(CACHE_BYTES)
            .set_repair_callback(move |_| {
                if !seen.swap(true, Ordering::Relaxed) {
                    crate::warn!("the database {shown} was not closed properly: checking and repairing it");
                }
            })
            .create(path)
            .map_err(|e| match e {
                redb::DatabaseError::DatabaseAlreadyOpen => format!(
                    "the database {} is in use by another process (another server on this data folder?)",
                    path.display()
                ),
                e => format!("cannot open the database {}: {e}", path.display()),
            })?;
        crate::util::make_private(path);
        Ok((db, repairing.load(Ordering::Relaxed)))
    }

    /// Creates the table and checks the format.
    fn prepare(&self) -> Result<(), Error> {
        self.write(|tx| match tx.get(FORMAT_KEY)? {
            None => tx.put(FORMAT_KEY, FORMAT.to_string().as_bytes()),
            Some(v) => {
                let f: u64 = std::str::from_utf8(&v)
                    .ok()
                    .and_then(|s| s.parse().ok())
                    .unwrap_or(u64::MAX);
                if f > FORMAT {
                    Err(Error::abort(format!(
                        "written by a newer server (format {f}, this one reads up to {FORMAT})"
                    )))
                } else {
                    Ok(())
                }
            }
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// redb had to repair it on open (it was not closed properly).
    pub fn was_repaired(&self) -> bool {
        self.repaired
    }

    fn db(&self) -> Result<RwLockReadGuard<'_, Option<Database>>, Error> {
        let g = self.db.read().unwrap_or_else(|e| e.into_inner());
        if g.is_none() {
            return Err(Error("the database is closed".into()));
        }
        Ok(g)
    }

    /// Closes the database after the transactions that run now (when the
    /// server stops), so that the next start need not repair it. Everything
    /// fails afterwards.
    pub fn close(&self) -> Result<(), String> {
        let mut g = self.db.write().unwrap_or_else(|e| e.into_inner());
        match g.take() {
            Some(db) => db.close().map_err(|e| e.to_string()),
            None => Ok(()),
        }
    }

    /// Opens it again after [`Self::close`] (a restart that failed).
    pub fn reopen(&self) -> Result<(), String> {
        let mut g = self.db.write().unwrap_or_else(|e| e.into_inner());
        if g.is_none() {
            *g = Some(Self::create(&self.path)?.0);
        }
        Ok(())
    }

    /// Runs `f` in one write transaction: everything it changed is stored
    /// together when it returns `Ok`, nothing when it returns an error or
    /// panics. One transaction at a time; reads go on meanwhile.
    pub fn write<T>(&self, f: impl FnOnce(&mut Tx) -> Result<T, Error>) -> Result<T, Error> {
        let g = self.db()?;
        let txn = g.as_ref().expect("open").begin_write().map_err(err)?;
        let out = {
            let table = txn.open_table(DATA).map_err(err)?;
            let mut tx = Tx { table };
            f(&mut tx)?
        };
        txn.commit().map_err(err)?;
        self.changed.store(true, Ordering::Relaxed);
        Ok(out)
    }

    pub fn put(&self, key: &str, value: &[u8]) -> Result<(), Error> {
        self.write(|tx| tx.put(key, value))
    }

    pub fn put_json(&self, key: &str, v: &Value) -> Result<(), Error> {
        self.write(|tx| tx.put_json(key, v))
    }

    pub fn delete(&self, key: &str) -> Result<bool, Error> {
        self.write(|tx| tx.delete(key))
    }

    pub fn delete_prefix(&self, prefix: &str) -> Result<usize, Error> {
        self.write(|tx| tx.delete_prefix(prefix))
    }

    fn read<T>(
        &self,
        f: impl FnOnce(&redb::ReadOnlyTable<&'static str, &'static [u8]>) -> Result<T, Error>,
    ) -> Result<T, Error> {
        let g = self.db()?;
        let txn = g.as_ref().expect("open").begin_read().map_err(err)?;
        let table = txn.open_table(DATA).map_err(err)?;
        f(&table)
    }

    /// A value; `None` when missing or unreadable (logged).
    pub fn get(&self, key: &str) -> Option<Vec<u8>> {
        self.read(|t| Ok(t.get(key).map_err(err)?.map(|v| v.value().to_vec())))
            .unwrap_or_else(|e| {
                crate::error!("cannot read {key} from the database: {e}");
                None
            })
    }

    pub fn get_json(&self, key: &str) -> Option<Value> {
        let data = self.get(key)?;
        match bl_json::parse_bytes(&data) {
            Ok(v) => Some(v),
            Err(e) => {
                crate::warn!("ignoring {key} in the database: {e}");
                None
            }
        }
    }

    /// Whether there is a value.
    pub fn contains(&self, key: &str) -> bool {
        self.read(|t| Ok(t.get(key).map_err(err)?.is_some()))
            .unwrap_or(false)
    }

    /// The keys (without `prefix`) and values under `prefix`, in key order;
    /// empty when unreadable (logged).
    pub fn list(&self, prefix: &str) -> Vec<(String, Vec<u8>)> {
        self.read(|t| list_in(t, prefix)).unwrap_or_else(|e| {
            crate::error!("cannot read {prefix}* from the database: {e}");
            Vec::new()
        })
    }

    /// As [`Self::list`], parsed as JSON (values that are not are left out).
    pub fn list_json(&self, prefix: &str) -> Vec<(String, Value)> {
        self.list(prefix)
            .into_iter()
            .filter_map(|(k, v)| match bl_json::parse_bytes(&v) {
                Ok(v) => Some((k, v)),
                Err(e) => {
                    crate::warn!("ignoring {prefix}{k} in the database: {e}");
                    None
                }
            })
            .collect()
    }

    /// For the health check: the database takes a write transaction.
    /// (Committed without waiting for the disk, which the data folder's own
    /// check covers, so that it does not keep disks awake.)
    pub fn check(&self) -> Result<(), String> {
        let g = self.db().map_err(|e| e.0)?;
        let mut txn = g
            .as_ref()
            .expect("open")
            .begin_write()
            .map_err(|e| e.to_string())?;
        txn.set_durability(redb::Durability::None)
            .map_err(|e| e.to_string())?;
        {
            let mut t = txn.open_table(DATA).map_err(|e| e.to_string())?;
            t.insert(HEALTH_KEY, crate::util::now_secs().to_string().as_bytes())
                .map_err(|e| e.to_string())?;
        }
        txn.commit().map_err(|e| e.to_string())
    }

    /// Writes a consistent copy (one read transaction: what was committed
    /// when it began) to `dest`, a new redb database, through a temporary
    /// file renamed into place. Returns the number of entries.
    pub fn copy_to(&self, dest: &Path) -> Result<usize, String> {
        let dir = dest.parent().unwrap_or(Path::new("."));
        std::fs::create_dir_all(dir)
            .map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
        let name = dest
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let tmp = dir.join(format!(".{name}.{}.tmp", std::process::id()));
        let _ = std::fs::remove_file(&tmp);
        let result = (|| -> Result<usize, String> {
            let copy = Database::builder()
                .set_cache_size(4 * 1024 * 1024)
                .create(&tmp)
                .map_err(|e| e.to_string())?;
            crate::util::make_private(&tmp);
            let g = self.db().map_err(|e| e.0)?;
            let rtx = g
                .as_ref()
                .expect("open")
                .begin_read()
                .map_err(|e| e.to_string())?;
            let from = rtx.open_table(DATA).map_err(|e| e.to_string())?;
            let wtx = copy.begin_write().map_err(|e| e.to_string())?;
            let mut n = 0usize;
            {
                let mut to = wtx.open_table(DATA).map_err(|e| e.to_string())?;
                for item in from.iter().map_err(|e| e.to_string())? {
                    let (k, v) = item.map_err(|e| e.to_string())?;
                    if k.value() == HEALTH_KEY {
                        continue;
                    }
                    to.insert(k.value(), v.value()).map_err(|e| e.to_string())?;
                    n += 1;
                }
            }
            wtx.commit().map_err(|e| e.to_string())?;
            copy.close().map_err(|e| e.to_string())?;
            std::fs::rename(&tmp, dest).map_err(|e| e.to_string())?;
            Ok(n)
        })();
        if result.is_err() {
            let _ = std::fs::remove_file(&tmp);
        }
        result.map_err(|e| format!("cannot copy the database to {}: {e}", dest.display()))
    }

    /// The copy for backups in `data`, when something changed since the
    /// last one (or there is none yet).
    pub fn backup_if_changed(&self, data: &Path) {
        let dest = data.join(BACKUP_FILE);
        if !self.changed.swap(false, Ordering::Relaxed) && dest.exists() {
            return;
        }
        match self.copy_to(&dest) {
            Ok(n) => crate::debug!("database copied to {} ({n} entries)", dest.display()),
            Err(e) => {
                self.changed.store(true, Ordering::Relaxed);
                crate::warn!("{e}");
            }
        }
    }
}

/// Closes the databases open in this process (when it stops: see
/// [`Store::close`]).
pub fn close_all() {
    let open: Vec<Arc<Store>> = OPEN
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .iter()
        .filter_map(|(_, w)| w.upgrade())
        .collect();
    for s in open {
        if let Err(e) = s.close() {
            crate::warn!("cannot close the database {}: {e}", s.path.display());
        }
    }
}

/// Where the data of a login is: the shared server password's, or an
/// account's (user names are a-z, 0-9, `.`, `_`, `-`).
pub fn scope(account: &str) -> String {
    if account.is_empty() {
        "shared/".to_string()
    } else {
        format!("acct/{account}/")
    }
}

/// One store's part of the database: the keys under a prefix, such as
/// `acct/bob/favorite/`.
#[derive(Clone)]
pub struct Part {
    store: Arc<Store>,
    prefix: String,
}

impl Part {
    pub fn new(store: &Arc<Store>, prefix: impl Into<String>) -> Part {
        Part {
            store: store.clone(),
            prefix: prefix.into(),
        }
    }

    pub fn store(&self) -> &Store {
        &self.store
    }

    pub fn prefix(&self) -> &str {
        &self.prefix
    }

    /// The full key of `name`.
    pub fn key(&self, name: &str) -> String {
        format!("{}{name}", self.prefix)
    }

    pub fn list(&self) -> Vec<(String, Vec<u8>)> {
        self.store.list(&self.prefix)
    }

    pub fn list_json(&self) -> Vec<(String, Value)> {
        self.store.list_json(&self.prefix)
    }

    pub fn get(&self, name: &str) -> Option<Vec<u8>> {
        self.store.get(&self.key(name))
    }

    pub fn put(&self, name: &str, value: &[u8]) -> Result<(), Error> {
        self.store.put(&self.key(name), value)
    }

    pub fn put_json(&self, name: &str, v: &Value) -> Result<(), Error> {
        self.store.put_json(&self.key(name), v)
    }

    pub fn delete(&self, name: &str) -> Result<bool, Error> {
        self.store.delete(&self.key(name))
    }

    /// Removes all of it.
    pub fn clear(&self) -> Result<usize, Error> {
        self.store.delete_prefix(&self.prefix)
    }
}

/// A database of its own, for tests. Its file is deleted at once (it stays
/// usable while open), so nothing is left in the temporary folder, which
/// may be memory.
#[cfg(test)]
pub fn temp_store(name: &str) -> Arc<Store> {
    let dir = std::env::temp_dir().join(format!("bl-db-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let s = Store::open(&dir).expect("temporary database");
    // (Windows keeps an open file: then it stays.)
    let _ = std::fs::remove_dir_all(&dir);
    s
}

/// Files of the server before 0.13.0, which kept its data as JSON files.
/// Found ones are not read (see the CHANGELOG); the server says so once.
pub fn legacy_files(data: &Path) -> Vec<String> {
    [
        "tokens.json",
        "admin.json",
        "settings.json",
        "playlists.json",
        "favorites.json",
        "hidden.json",
        "history.jsonl",
        "labels.json",
        "sources.json",
        "presets",
        "extensions/imported.json",
        "vault/users",
        "vault/released.json",
        "vault/sessions.json",
        "vault/userdata",
        "vault/push",
        "vault/links",
    ]
    .into_iter()
    .filter(|f| data.join(f).exists())
    .map(str::to_string)
    .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::{Command, Stdio};

    /// A folder that is removed again at the end of the test.
    struct TempDir(PathBuf);

    impl std::ops::Deref for TempDir {
        type Target = Path;
        fn deref(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn temp(name: &str) -> TempDir {
        let dir = std::env::temp_dir().join(format!("bl-store-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        TempDir(dir)
    }

    fn text(s: &Store, key: &str) -> Option<String> {
        s.get(key).map(|v| String::from_utf8(v).unwrap())
    }

    #[test]
    fn values_and_prefixes() {
        let dir = temp("basic");
        let s = Store::open_path(&dir.join(FILE_NAME)).unwrap();
        s.put("a/1", b"one").unwrap();
        s.put("a/2", b"two").unwrap();
        s.put("a/2/x", b"deeper").unwrap();
        s.put("ab", b"next to it").unwrap();
        s.put_json("j", &Value::object().with("k", 5u64)).unwrap();
        assert_eq!(text(&s, "a/1").as_deref(), Some("one"));
        assert_eq!(s.get_json("j").unwrap().get("k").as_u64(), Some(5));
        let keys: Vec<String> = s.list("a/").into_iter().map(|(k, _)| k).collect();
        assert_eq!(keys, ["1", "2", "2/x"]);
        assert_eq!(s.delete_prefix("a/").unwrap(), 3);
        assert!(s.list("a/").is_empty());
        assert_eq!(text(&s, "ab").as_deref(), Some("next to it"));
        assert!(s.delete("ab").unwrap());
        assert!(!s.delete("ab").unwrap());
        assert!(!s.contains("ab"));
        drop(s);
        // Kept over a restart, with the format noted.
        let s = Store::open_path(&dir.join(FILE_NAME)).unwrap();
        assert!(s.contains("j"));
        assert_eq!(text(&s, FORMAT_KEY).as_deref(), Some("1"));
    }

    #[test]
    fn files_of_an_older_server_are_noticed() {
        let dir = temp("legacy");
        assert!(legacy_files(&dir).is_empty());
        std::fs::write(dir.join("tokens.json"), "{}").unwrap();
        std::fs::create_dir_all(dir.join("vault/users")).unwrap();
        std::fs::create_dir_all(dir.join("vault/blobs")).unwrap();
        assert_eq!(legacy_files(&dir), ["tokens.json", "vault/users"]);
    }

    #[test]
    fn prefix_ends() {
        assert_eq!(upper_bound("acct/bob/").as_deref(), Some("acct/bob0"));
        assert_eq!(upper_bound("a\u{7f}").as_deref(), Some("b"));
        assert_eq!(upper_bound(""), None);
    }

    #[test]
    fn a_failed_transaction_changes_nothing() {
        let dir = temp("abort");
        let s = Store::open_path(&dir.join(FILE_NAME)).unwrap();
        s.write(|tx| {
            tx.put("acct/bob/a", b"1")?;
            tx.put("acct/bob/b", b"2")?;
            tx.put("vault/user/bob", b"{}")
        })
        .unwrap();
        let r: Result<(), Error> = s.write(|tx| {
            tx.delete_prefix("acct/bob/")?;
            tx.delete("vault/user/bob")?;
            // It sees its own changes.
            assert!(tx.get("vault/user/bob")?.is_none());
            Err(Error::abort("changed my mind"))
        });
        assert_eq!(r.unwrap_err().0, "changed my mind");
        assert_eq!(s.list("acct/bob/").len(), 2);
        assert!(s.contains("vault/user/bob"));
        // A panic in the middle aborts as well.
        let caught = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _ = s.write(|tx| -> Result<(), Error> {
                tx.delete("vault/user/bob")?;
                panic!("boom");
            });
        }));
        assert!(caught.is_err());
        assert!(s.contains("vault/user/bob"));
        // And the database still takes the next one.
        s.write(|tx| tx.delete("vault/user/bob").map(|_| ()))
            .unwrap();
        assert!(!s.contains("vault/user/bob"));
    }

    #[test]
    fn a_newer_format_is_refused() {
        let dir = temp("format");
        let path = dir.join(FILE_NAME);
        Store::open_path(&path)
            .unwrap()
            .put(FORMAT_KEY, b"99")
            .unwrap();
        let e = Store::open_path(&path).err().unwrap();
        assert!(e.contains("newer server"), "{e}");
    }

    #[test]
    fn one_handle_per_folder_in_a_process() {
        let dir = temp("shared");
        let a = Store::open(&dir).unwrap();
        let b = Store::open(&dir).unwrap();
        assert!(Arc::ptr_eq(&a, &b));
        drop((a, b));
        // Closed with the last handle: opens again.
        let c = Store::open(&dir).unwrap();
        c.put("x", b"1").unwrap();
        // Another process (or handle) cannot have it at the same time.
        let e = Store::open_path(&dir.join(FILE_NAME)).err().unwrap();
        assert!(e.contains("in use by another process"), "{e}");
    }

    #[test]
    fn closed_properly_it_needs_no_repair() {
        let dir = temp("close");
        // (Not close_all: the other tests' databases are open as well.)
        let s = Store::open_path(&dir.join(FILE_NAME)).unwrap();
        s.put("a", b"1").unwrap();
        s.close().unwrap();
        assert!(s.put("b", b"2").unwrap_err().0.contains("closed"));
        assert!(s.get("a").is_none());
        assert!(s.check().is_err());
        s.close().unwrap();
        // Another handle can have it now; back again when that one is gone.
        let other = Store::open_path(&dir.join(FILE_NAME)).unwrap();
        assert!(!other.was_repaired());
        assert!(s.reopen().is_err());
        drop(other);
        s.reopen().unwrap();
        s.put("b", b"2").unwrap();
        s.close().unwrap();
        drop(s);
        let s = Store::open_path(&dir.join(FILE_NAME)).unwrap();
        assert!(!s.was_repaired());
        assert_eq!(text(&s, "a").as_deref(), Some("1"));
    }

    #[test]
    fn health_check_and_copy() {
        let dir = temp("copy");
        let s = Store::open_path(&dir.join(FILE_NAME)).unwrap();
        s.put("vault/user/a", b"{\"name\":\"a\"}").unwrap();
        s.put("acct/a/favorite/0000000000000001", b"5").unwrap();
        s.check().unwrap();
        s.backup_if_changed(&dir);
        let copy = dir.join(BACKUP_FILE);
        assert!(copy.exists());
        // Unchanged: not written again.
        let t0 = std::fs::metadata(&copy).unwrap().modified().unwrap();
        std::thread::sleep(std::time::Duration::from_millis(20));
        s.backup_if_changed(&dir);
        assert_eq!(std::fs::metadata(&copy).unwrap().modified().unwrap(), t0);
        // The copy opens on its own, without the health probe.
        let c = Store::open_path(&copy).unwrap();
        assert_eq!(
            text(&c, "vault/user/a").as_deref(),
            Some("{\"name\":\"a\"}")
        );
        assert_eq!(c.list("acct/a/").len(), 1);
        assert!(!c.contains(HEALTH_KEY));
        assert!(std::fs::read_dir(dir.join("backup"))
            .unwrap()
            .flatten()
            .all(|e| !e.file_name().to_string_lossy().ends_with(".tmp")));
    }

    /// Runs this test binary again for one test, with `BL_STORE_CHILD` set.
    fn child(test: &str, dir: &Path) -> Command {
        let mut c = Command::new(std::env::current_exe().unwrap());
        c.args([test, "--exact", "--nocapture", "--test-threads=1"])
            .env("BL_STORE_CHILD", dir);
        c
    }

    #[test]
    fn a_crash_in_the_middle_of_a_transaction_leaves_the_old_state() {
        if let Some(dir) = std::env::var_os("BL_STORE_CHILD") {
            // The child: changes a lot, then dies before the commit.
            let s = Store::open_path(&Path::new(&dir).join(FILE_NAME)).unwrap();
            let _ = s.write(|tx| -> Result<(), Error> {
                tx.put("acct/bob/playlist/1", b"changed")?;
                tx.delete("vault/user/bob")?;
                for i in 0..2000 {
                    tx.put(&format!("acct/bob/history/{i:08}"), &[7u8; 512])?;
                }
                std::process::abort();
            });
            unreachable!();
        }
        let dir = temp("crash");
        {
            let s = Store::open_path(&dir.join(FILE_NAME)).unwrap();
            s.write(|tx| {
                tx.put("acct/bob/playlist/1", b"before")?;
                tx.put("vault/user/bob", b"{}")
            })
            .unwrap();
        }
        let status = child(
            "store::tests::a_crash_in_the_middle_of_a_transaction_leaves_the_old_state",
            &dir,
        )
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .unwrap();
        assert!(!status.success(), "the child should have died");
        let s = Store::open_path(&dir.join(FILE_NAME)).unwrap();
        assert_eq!(text(&s, "acct/bob/playlist/1").as_deref(), Some("before"));
        assert!(s.contains("vault/user/bob"));
        assert!(s.list("acct/bob/history/").is_empty());
        // And it takes changes again.
        s.put("acct/bob/playlist/1", b"after").unwrap();
    }

    #[test]
    fn killed_while_committing_every_transaction_is_whole() {
        if let Some(dir) = std::env::var_os("BL_STORE_CHILD") {
            // The child: transactions that change two keys together, telling
            // the parent about every commit, until it is killed.
            let s = Store::open_path(&Path::new(&dir).join(FILE_NAME)).unwrap();
            let big = vec![1u8; 64 * 1024];
            for i in 1u64.. {
                s.write(|tx| {
                    tx.put("pair/a", i.to_string().as_bytes())?;
                    tx.put(&format!("blob/{i:08}"), &big)?;
                    if i > 3 {
                        tx.delete(&format!("blob/{:08}", i - 3))?;
                    }
                    tx.put("pair/b", i.to_string().as_bytes())
                })
                .unwrap();
                println!("committed {i}");
            }
            unreachable!();
        }
        use std::io::BufRead;
        for round in 0..3u64 {
            let dir = temp(&format!("kill{round}"));
            let mut c = child(
                "store::tests::killed_while_committing_every_transaction_is_whole",
                &dir,
            )
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
            let out = c.stdout.take().unwrap();
            let mut lines = std::io::BufReader::new(out).lines();
            let mut done = 0u64;
            while done < 20 + round * 15 {
                let line = lines.next().unwrap().unwrap();
                if let Some(n) = line.strip_prefix("committed ") {
                    done = n.parse().unwrap();
                }
            }
            // Somewhere in the middle of the next commits.
            std::thread::sleep(std::time::Duration::from_millis(round * 3));
            c.kill().unwrap();
            c.wait().unwrap();
            let s = Store::open_path(&dir.join(FILE_NAME)).unwrap();
            assert!(s.was_repaired(), "killed: repaired on open");
            let a: u64 = text(&s, "pair/a").unwrap().parse().unwrap();
            let b: u64 = text(&s, "pair/b").unwrap().parse().unwrap();
            assert_eq!(a, b, "a transaction was stored in part");
            assert!(a >= done, "a confirmed commit is missing: {a} < {done}");
            let blobs: Vec<String> = s.list("blob/").into_iter().map(|(k, _)| k).collect();
            let want: Vec<String> = (a.saturating_sub(2).max(1)..=a)
                .map(|i| format!("{i:08}"))
                .collect();
            assert_eq!(blobs, want);
        }
    }
}
