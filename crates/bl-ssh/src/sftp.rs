//! SFTP version 3 (draft-ietf-secsh-filexfer-02), reading only: paths,
//! attributes, folders and files.
//!
//! [`SftpFile`] keeps up to 8 READ requests of 32 KiB in flight while it
//! is read sequentially; the number starts at one after each seek and
//! doubles with each in-order answer, so reading a few tag bytes and
//! seeking costs no more than needed while streaming runs at full speed.

use crate::conn::{Conn, LOCAL_ID, LOCAL_MAX_PACKET, LOCAL_WINDOW};
use crate::wire::{Put, Reader};
use crate::{msg, status, Cipher, Error, Session};
use std::collections::VecDeque;
use std::io::{Read, Seek, SeekFrom};
use std::sync::Arc;

const INIT: u8 = 1;
const OPEN: u8 = 3;
const CLOSE: u8 = 4;
const READ: u8 = 5;
const LSTAT: u8 = 7;
const FSTAT: u8 = 8;
const OPENDIR: u8 = 11;
const READDIR: u8 = 12;
const REALPATH: u8 = 16;
const STAT: u8 = 17;
const STATUS: u8 = 101;
const HANDLE: u8 = 102;
const DATA: u8 = 103;
const NAME: u8 = 104;
const ATTRS: u8 = 105;

const OPEN_READ: u32 = 1;
const CHUNK: u32 = 32 * 1024;
const MAX_AHEAD: usize = 8;
const MAX_PATH: usize = 4096;
const MAX_HANDLE: usize = 256;
/// Entries of one folder.
const MAX_DIR_ENTRIES: usize = 500_000;

/// File attributes; servers may leave any of them out.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Metadata {
    pub size: Option<u64>,
    pub uid: Option<u32>,
    pub gid: Option<u32>,
    /// Unix mode bits including the file type.
    pub permissions: Option<u32>,
    pub atime: Option<u32>,
    /// Seconds since the Unix epoch.
    pub mtime: Option<u32>,
}

const S_IFMT: u32 = 0o170_000;

impl Metadata {
    fn file_type(&self) -> Option<u32> {
        self.permissions.map(|p| p & S_IFMT)
    }

    pub fn is_dir(&self) -> bool {
        self.file_type() == Some(0o040_000)
    }

    pub fn is_file(&self) -> bool {
        self.file_type() == Some(0o100_000)
    }

    pub fn is_symlink(&self) -> bool {
        self.file_type() == Some(0o120_000)
    }

    fn parse(r: &mut Reader) -> Result<Metadata, Error> {
        let flags = r.u32()?;
        let mut m = Metadata::default();
        if flags & 0x1 != 0 {
            m.size = Some(r.u64()?);
        }
        if flags & 0x2 != 0 {
            m.uid = Some(r.u32()?);
            m.gid = Some(r.u32()?);
        }
        if flags & 0x4 != 0 {
            m.permissions = Some(r.u32()?);
        }
        if flags & 0x8 != 0 {
            m.atime = Some(r.u32()?);
            m.mtime = Some(r.u32()?);
        }
        if flags & 0x8000_0000 != 0 {
            // Each pair takes at least 8 bytes, so the data bounds the loop.
            for _ in 0..r.u32()? {
                r.string()?;
                r.string()?;
            }
        }
        Ok(m)
    }
}

/// An entry of a folder ("." and ".." left out).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DirEntry {
    pub name: String,
    pub is_dir: bool,
    /// The entry is a symbolic link (`is_dir` and `size` are the link's;
    /// [`Sftp::stat`] follows it).
    pub is_symlink: bool,
    /// Bytes (0 = unknown).
    pub size: u64,
    /// Seconds since the Unix epoch (0 = unknown).
    pub mtime: u64,
}

/// An SFTP connection; clones share it.
#[derive(Clone)]
pub struct Sftp {
    conn: Arc<Conn>,
}

fn packet(typ: u8, id: u32, body: &[u8]) -> Vec<u8> {
    let mut p = Vec::with_capacity(9 + body.len());
    p.u32(5 + body.len() as u32);
    p.u8(typ);
    p.u32(id);
    p.extend_from_slice(body);
    p
}

fn status_error(data: &[u8]) -> Error {
    let mut r = Reader::new(data);
    let code = r.u32().unwrap_or(status::FAILURE);
    // Version 3 servers may leave out the message.
    let text = r
        .string()
        .map(|s| String::from_utf8_lossy(&s[..s.len().min(200)]).into_owned())
        .unwrap_or_default();
    Error::Sftp(code, text)
}

fn unexpected(typ: u8) -> Error {
    Error::Protocol(format!("unexpected SFTP answer {typ}"))
}

/// The body of an answer of type `want`, or the error a STATUS carries.
fn expect((typ, data): (u8, Vec<u8>), want: u8) -> Result<Vec<u8>, Error> {
    match typ {
        t if t == want => Ok(data),
        STATUS => Err(status_error(&data)),
        t => Err(unexpected(t)),
    }
}

fn path_arg(path: &str) -> Result<Vec<u8>, Error> {
    if path.len() > MAX_PATH {
        return Err(Error::Sftp(status::BAD_MESSAGE, "path too long".into()));
    }
    let mut b = Vec::with_capacity(4 + path.len());
    b.string(path.as_bytes());
    Ok(b)
}

impl Sftp {
    /// Opens the channel, starts the subsystem and agrees on version 3.
    pub(crate) fn start(mut s: Session) -> Result<Sftp, Error> {
        let mut m = vec![msg::CHANNEL_OPEN];
        m.string(b"session");
        m.u32(LOCAL_ID);
        m.u32(LOCAL_WINDOW);
        m.u32(LOCAL_MAX_PACKET);
        s.tx.send(&m)?;
        let r = s.next()?;
        let mut rd = Reader::new(&r[1..]);
        let (remote_id, mut window, max_packet) = match r[0] {
            msg::CHANNEL_OPEN_CONFIRMATION => {
                if rd.u32()? != LOCAL_ID {
                    return Err(Error::Protocol("confirmation of an unknown channel".into()));
                }
                (rd.u32()?, rd.u32()? as u64, rd.u32()?)
            }
            msg::CHANNEL_OPEN_FAILURE => {
                rd.u32()?;
                rd.u32()?;
                let why = String::from_utf8_lossy(rd.string().unwrap_or_default()).into_owned();
                return Err(Error::Protocol(format!(
                    "the server refused a session: {why}"
                )));
            }
            t => return Err(Error::Protocol(format!("unexpected message {t}"))),
        };
        if max_packet == 0 {
            return Err(Error::Protocol("invalid channel packet size".into()));
        }
        let mut m = vec![msg::CHANNEL_REQUEST];
        m.u32(remote_id);
        m.string(b"subsystem");
        m.bool(true);
        m.string(b"sftp");
        s.tx.send(&m)?;
        loop {
            let r = s.next()?;
            let mut rd = Reader::new(&r[1..]);
            match r[0] {
                msg::CHANNEL_SUCCESS => break,
                msg::CHANNEL_FAILURE => {
                    return Err(Error::Protocol("the server does not offer SFTP".into()))
                }
                msg::CHANNEL_WINDOW_ADJUST => {
                    rd.u32()?;
                    window = (window + rd.u32()? as u64).min(u32::MAX as u64);
                }
                msg::CHANNEL_REQUEST => {
                    rd.u32()?;
                    rd.string()?;
                    if rd.bool()? {
                        let mut a = vec![msg::CHANNEL_FAILURE];
                        a.u32(remote_id);
                        s.tx.send(&a)?;
                    }
                }
                msg::CHANNEL_EOF | msg::CHANNEL_CLOSE => {
                    return Err(Error::Io("the server closed the SFTP channel".into()))
                }
                t => return Err(Error::Protocol(format!("unexpected message {t}"))),
            }
        }
        let sock = s.tx.socket()?;
        let conn = Arc::new(Conn::new(s, remote_id, window, max_packet, sock));
        let mut init = Vec::new();
        init.u32(5);
        init.u8(INIT);
        init.u32(3);
        conn.send(&init)?;
        let version = conn.wait(|st| st.version.take())?;
        if version < 3 {
            return Err(Error::Protocol(format!(
                "SFTP version {version} is not supported"
            )));
        }
        Ok(Sftp { conn })
    }

    /// The server's host key fingerprint (`SHA256:…`).
    pub fn fingerprint(&self) -> &str {
        &self.conn.fingerprint
    }

    pub fn cipher(&self) -> Option<Cipher> {
        self.conn.cipher
    }

    /// False once the connection failed (every request then fails).
    pub fn is_alive(&self) -> bool {
        self.conn.is_alive()
    }

    fn call(&self, typ: u8, body: &[u8]) -> Result<(u8, Vec<u8>), Error> {
        let id = self.conn.request(|id| packet(typ, id, body))?;
        self.conn.response(id)
    }

    /// Sends CLOSE without waiting for the answer.
    fn close_handle(&self, handle: &[u8]) {
        let mut body = Vec::new();
        body.string(handle);
        if let Ok(id) = self.conn.request(|id| packet(CLOSE, id, &body)) {
            self.conn.forget(id);
        }
    }

    fn handle(&self, typ: u8, body: &[u8]) -> Result<Vec<u8>, Error> {
        let data = expect(self.call(typ, body)?, HANDLE)?;
        let mut r = Reader::new(&data);
        let h = r.string()?;
        if h.len() > MAX_HANDLE {
            return Err(Error::Protocol("invalid handle".into()));
        }
        Ok(h.to_vec())
    }

    /// The absolute, canonical form of `path` ("." = the login folder).
    pub fn realpath(&self, path: &str) -> Result<String, Error> {
        let data = expect(self.call(REALPATH, &path_arg(path)?)?, NAME)?;
        let mut r = Reader::new(&data);
        if r.u32()? < 1 {
            return Err(Error::Protocol("empty REALPATH answer".into()));
        }
        String::from_utf8(r.string()?.to_vec())
            .map_err(|_| Error::Protocol("the path is not UTF-8".into()))
    }

    /// Attributes, following symbolic links.
    pub fn stat(&self, path: &str) -> Result<Metadata, Error> {
        let data = expect(self.call(STAT, &path_arg(path)?)?, ATTRS)?;
        Metadata::parse(&mut Reader::new(&data))
    }

    /// Attributes of a symbolic link itself.
    pub fn lstat(&self, path: &str) -> Result<Metadata, Error> {
        let data = expect(self.call(LSTAT, &path_arg(path)?)?, ATTRS)?;
        Metadata::parse(&mut Reader::new(&data))
    }

    /// The entries of a folder. Names that are not UTF-8 or that contain
    /// '/' are left out.
    pub fn read_dir(&self, path: &str) -> Result<Vec<DirEntry>, Error> {
        let handle = self.handle(OPENDIR, &path_arg(path)?)?;
        let mut body = Vec::new();
        body.string(&handle);
        let mut out = Vec::new();
        let res = loop {
            let data = match self.call(READDIR, &body) {
                Ok((NAME, data)) => data,
                Ok((STATUS, data)) => match status_error(&data) {
                    Error::Sftp(status::EOF, _) => break Ok(()),
                    e => break Err(e),
                },
                Ok((t, _)) => break Err(unexpected(t)),
                Err(e) => break Err(e),
            };
            if let Err(e) = parse_names(&data, &mut out) {
                break Err(e);
            }
            if out.len() > MAX_DIR_ENTRIES {
                break Err(Error::Protocol("too many entries in a folder".into()));
            }
        };
        self.close_handle(&handle);
        res.map(|_| out)
    }

    /// Opens a file for reading.
    pub fn open(&self, path: &str) -> Result<SftpFile, Error> {
        let mut body = path_arg(path)?;
        body.u32(OPEN_READ);
        body.u32(0); // no attributes
        let handle = self.handle(OPEN, &body)?;
        Ok(SftpFile {
            sftp: self.clone(),
            handle,
            pos: 0,
            size: None,
            buf: Vec::new(),
            buf_start: 0,
            ahead: VecDeque::new(),
            next: 0,
            depth: 1,
        })
    }
}

fn parse_names(data: &[u8], out: &mut Vec<DirEntry>) -> Result<(), Error> {
    let mut r = Reader::new(data);
    // Each entry takes at least 12 bytes, so the data bounds the loop.
    for _ in 0..r.u32()? {
        let name = r.string()?;
        let long = r.string()?;
        let m = Metadata::parse(&mut r)?;
        let Ok(name) = std::str::from_utf8(name) else {
            continue;
        };
        if name.is_empty() || name == "." || name == ".." || name.contains(['/', '\0']) {
            continue;
        }
        // Without permissions, the `ls -l` style long name tells the type.
        let (is_dir, is_symlink) = match m.permissions {
            Some(_) => (m.is_dir(), m.is_symlink()),
            None => (long.first() == Some(&b'd'), long.first() == Some(&b'l')),
        };
        out.push(DirEntry {
            name: name.to_string(),
            is_dir,
            is_symlink,
            size: m.size.unwrap_or(0),
            mtime: m.mtime.unwrap_or(0) as u64,
        });
    }
    Ok(())
}

struct Ahead {
    id: u32,
    offset: u64,
    len: u32,
}

/// A file open for reading; seeking only moves the position.
pub struct SftpFile {
    sftp: Sftp,
    handle: Vec<u8>,
    pos: u64,
    /// From FSTAT, asked when first needed.
    size: Option<u64>,
    /// The last answer's data and where it starts in the file.
    buf: Vec<u8>,
    buf_start: u64,
    /// READ requests in flight, in file order.
    ahead: VecDeque<Ahead>,
    /// Where the next request starts.
    next: u64,
    /// Requests to keep in flight.
    depth: usize,
}

impl SftpFile {
    /// The file size (asked once).
    pub fn len(&mut self) -> Result<u64, Error> {
        if let Some(s) = self.size {
            return Ok(s);
        }
        let mut body = Vec::new();
        body.string(&self.handle);
        let data = expect(self.sftp.call(FSTAT, &body)?, ATTRS)?;
        let size = Metadata::parse(&mut Reader::new(&data))?
            .size
            .ok_or_else(|| Error::Protocol("the server does not tell the file size".into()))?;
        self.size = Some(size);
        Ok(size)
    }

    pub fn is_empty(&mut self) -> Result<bool, Error> {
        Ok(self.len()? == 0)
    }

    fn cancel(&mut self) {
        for a in self.ahead.drain(..) {
            self.sftp.conn.forget(a.id);
        }
    }

    /// Tops up the requests in flight (beyond a known end only one, which
    /// finds the end).
    fn fill(&mut self) -> Result<(), Error> {
        while self.ahead.len() < self.depth
            && (self.ahead.is_empty() || self.size.map_or(true, |s| self.next < s))
        {
            let mut body = Vec::with_capacity(4 + self.handle.len() + 12);
            body.string(&self.handle);
            body.u64(self.next);
            body.u32(CHUNK);
            let id = self.sftp.conn.request(|id| packet(READ, id, &body))?;
            self.ahead.push_back(Ahead {
                id,
                offset: self.next,
                len: CHUNK,
            });
            self.next = self.next.saturating_add(CHUNK as u64);
        }
        Ok(())
    }

    fn read_some(&mut self, out: &mut [u8]) -> Result<usize, Error> {
        loop {
            let end = self.buf_start.saturating_add(self.buf.len() as u64);
            if self.pos >= self.buf_start && self.pos < end {
                let at = (self.pos - self.buf_start) as usize;
                let n = out.len().min(self.buf.len() - at);
                out[..n].copy_from_slice(&self.buf[at..at + n]);
                self.pos += n as u64;
                return Ok(n);
            }
            // Requests must continue at the position, else start over there.
            let in_order = match self.ahead.front() {
                Some(a) => a.offset == self.pos,
                None => self.next == self.pos,
            };
            if !in_order {
                self.cancel();
                self.next = self.pos;
                self.depth = 1;
            }
            self.fill()?;
            let Some(a) = self.ahead.pop_front() else {
                return Ok(0);
            };
            let (typ, mut data) = match self.sftp.conn.response(a.id) {
                Ok(r) => r,
                Err(e) => {
                    self.cancel();
                    return Err(e);
                }
            };
            match typ {
                DATA => {
                    let n = Reader::new(&data).u32()? as usize;
                    if n != data.len() - 4 || n > a.len as usize {
                        return Err(Error::Protocol("invalid READ answer".into()));
                    }
                    if n == 0 {
                        self.cancel();
                        return Ok(0);
                    }
                    data.drain(..4);
                    self.buf = data;
                    self.buf_start = a.offset;
                    if n < a.len as usize {
                        // A short answer: what follows was asked for at the
                        // wrong offsets.
                        self.cancel();
                        self.next = a.offset.saturating_add(n as u64);
                    } else {
                        self.depth = (self.depth * 2).min(MAX_AHEAD);
                    }
                }
                STATUS => {
                    self.cancel();
                    self.next = a.offset;
                    return match status_error(&data) {
                        Error::Sftp(status::EOF, _) => Ok(0),
                        e => Err(e),
                    };
                }
                t => return Err(unexpected(t)),
            }
        }
    }
}

impl Read for SftpFile {
    fn read(&mut self, out: &mut [u8]) -> std::io::Result<usize> {
        if out.is_empty() {
            return Ok(0);
        }
        self.read_some(out).map_err(std::io::Error::from)
    }
}

impl Seek for SftpFile {
    fn seek(&mut self, to: SeekFrom) -> std::io::Result<u64> {
        let pos = match to {
            SeekFrom::Start(p) => Some(p),
            SeekFrom::Current(d) => self.pos.checked_add_signed(d),
            SeekFrom::End(d) => self.len()?.checked_add_signed(d),
        };
        let pos = pos.ok_or_else(|| {
            std::io::Error::new(std::io::ErrorKind::InvalidInput, "invalid seek position")
        })?;
        self.pos = pos;
        Ok(pos)
    }
}

impl Drop for SftpFile {
    fn drop(&mut self) {
        self.cancel();
        self.sftp.close_handle(&self.handle);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_and_attributes() {
        let mut d = Vec::new();
        d.u32(5);
        for (name, long, perm) in [
            (
                "song.flac",
                "-rw-r--r-- 1 a b 9 Jan 1 song.flac",
                Some(0o100_644),
            ),
            (
                "Album",
                "drwxr-xr-x 2 a b 4096 Jan 1 Album",
                Some(0o040_755),
            ),
            ("link", "lrwxrwxrwx 1 a b 5 Jan 1 link -> Album", None),
            ("..", "drwxr-xr-x 2 a b 4096 Jan 1 ..", Some(0o040_755)),
            ("a/b", "-rw-r--r-- 1 a b 9 Jan 1 a/b", Some(0o100_644)),
        ] {
            d.string(name.as_bytes());
            d.string(long.as_bytes());
            match perm {
                Some(p) => {
                    d.u32(0x1 | 0x4 | 0x8 | 0x8000_0000);
                    d.u64(1234);
                    d.u32(p);
                    d.u32(1);
                    d.u32(1_700_000_000);
                    d.u32(1);
                    d.string(b"ext@example.com");
                    d.string(b"value");
                }
                None => d.u32(0),
            }
        }
        let mut out = Vec::new();
        parse_names(&d, &mut out).unwrap();
        assert_eq!(
            out,
            [
                DirEntry {
                    name: "song.flac".into(),
                    is_dir: false,
                    is_symlink: false,
                    size: 1234,
                    mtime: 1_700_000_000
                },
                DirEntry {
                    name: "Album".into(),
                    is_dir: true,
                    is_symlink: false,
                    size: 1234,
                    mtime: 1_700_000_000
                },
                DirEntry {
                    name: "link".into(),
                    is_dir: false,
                    is_symlink: true,
                    size: 0,
                    mtime: 0
                },
            ]
        );
        // A count beyond the data fails instead of looping or allocating.
        let mut bad = Vec::new();
        bad.u32(u32::MAX);
        assert!(parse_names(&bad, &mut Vec::new()).is_err());
        let mut bad = Vec::new();
        bad.u32(0x8000_0000);
        bad.u32(u32::MAX);
        assert!(Metadata::parse(&mut Reader::new(&bad)).is_err());
    }

    #[test]
    fn status_messages() {
        let mut d = Vec::new();
        d.u32(status::NO_SUCH_FILE);
        d.string(b"No such file");
        d.string(b"en");
        let e = status_error(&d);
        assert!(e.is_not_found());
        assert_eq!(e.to_string(), "no such file");
        assert_eq!(std::io::Error::from(e).kind(), std::io::ErrorKind::NotFound);
        // Version 3 servers may send only the code.
        assert_eq!(status_error(&[0, 0, 0, 4]), Error::Sftp(4, String::new()));
    }
}
