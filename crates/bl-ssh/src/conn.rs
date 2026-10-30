//! The connection once the SFTP channel is open, shared by threads.
//!
//! Sending holds the `tx` lock for one whole SFTP message, so messages of
//! different threads never interleave. A thread waiting for something
//! (an answer, more send window) becomes the reader when nobody else
//! reads: it takes the receiving half out of the state, reads one packet
//! without holding the state lock, puts it back, files the packet (SFTP
//! answers by request id) and wakes the others, who check whether their
//! answer arrived or take over reading. A rekeying the server starts is
//! run by the reader while it holds `tx`, so nothing else is sent
//! meanwhile.
//!
//! Lock order: `st` before `tx`, never the other way.

use crate::kex::{self, KexContext};
use crate::session::global_request_reply;
use crate::transport::{RecvHalf, SendHalf};
use crate::wire::{Put, Reader};
use crate::{msg, Cipher, Error, Session};
use std::collections::HashMap;
use std::net::TcpStream;
use std::sync::{Condvar, Mutex, MutexGuard};

/// Our channel number (the only channel).
pub(crate) const LOCAL_ID: u32 = 0;
/// Bytes the server may send before we adjust the window.
pub(crate) const LOCAL_WINDOW: u32 = 2 * 1024 * 1024;
pub(crate) const LOCAL_MAX_PACKET: u32 = 64 * 1024;
/// Largest SFTP message accepted (OpenSSH sends at most 256 KiB).
const MAX_SFTP_MESSAGE: usize = 1024 * 1024;
const SFTP_VERSION: u8 = 2;

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

struct Tx {
    half: SendHalf,
    remote_id: u32,
    /// Bytes the server lets us send.
    window: u64,
    max_packet: u32,
}

impl Tx {
    fn send_data(&mut self, data: &[u8]) -> Result<(), Error> {
        for chunk in data.chunks(self.max_packet as usize) {
            let mut m = Vec::with_capacity(9 + chunk.len());
            m.push(msg::CHANNEL_DATA);
            m.u32(self.remote_id);
            m.string(chunk);
            self.half.send(&m)?;
            self.window -= chunk.len() as u64;
        }
        Ok(())
    }
}

struct Rx {
    half: RecvHalf,
    kex: KexContext,
}

enum Slot {
    Waiting,
    Ready(u8, Vec<u8>),
    /// The asker gave up; drop the answer when it comes.
    Discard,
}

pub(crate) struct State {
    /// The receiving half; None while a thread reads.
    rx: Option<Box<Rx>>,
    dead: Option<Error>,
    local_window: u32,
    /// Counts window adjustments from the server.
    window_gen: u64,
    /// Channel data not yet a whole SFTP message.
    inbuf: Vec<u8>,
    pending: HashMap<u32, Slot>,
    next_id: u32,
    /// The server's SFTP version once it answered INIT.
    pub version: Option<u32>,
}

impl State {
    /// Files complete SFTP messages from `inbuf`.
    fn deliver(&mut self) -> Result<(), Error> {
        let mut start = 0;
        while self.inbuf.len() - start >= 4 {
            let b = &self.inbuf[start..];
            let len = u32::from_be_bytes([b[0], b[1], b[2], b[3]]) as usize;
            if !(5..=MAX_SFTP_MESSAGE).contains(&len) {
                // Typically text a login script printed into the stream.
                return Err(Error::Protocol(
                    "invalid SFTP message (does a login script on the server print text?)".into(),
                ));
            }
            if b.len() < 4 + len {
                break;
            }
            let typ = b[4];
            let mut r = Reader::new(&b[5..4 + len]);
            let id = r.u32()?;
            if typ == SFTP_VERSION {
                self.version = Some(id);
            } else if let Some(slot) = self.pending.get_mut(&id) {
                match slot {
                    Slot::Waiting => *slot = Slot::Ready(typ, r.rest().to_vec()),
                    Slot::Discard => {
                        self.pending.remove(&id);
                    }
                    // A second answer: ignored.
                    Slot::Ready(..) => {}
                }
            }
            start += 4 + len;
        }
        self.inbuf.drain(..start);
        Ok(())
    }
}

pub(crate) struct Conn {
    tx: Mutex<Tx>,
    st: Mutex<State>,
    cv: Condvar,
    /// To end the connection without the locks.
    sock: TcpStream,
    pub fingerprint: String,
    pub cipher: Option<Cipher>,
}

impl Conn {
    pub fn new(s: Session, remote_id: u32, window: u64, max_packet: u32, sock: TcpStream) -> Conn {
        let fingerprint = s.kex.fingerprint.clone();
        let cipher = s.kex.cipher;
        Conn {
            tx: Mutex::new(Tx {
                half: s.tx,
                remote_id,
                window,
                max_packet,
            }),
            st: Mutex::new(State {
                rx: Some(Box::new(Rx {
                    half: s.rx,
                    kex: s.kex,
                })),
                dead: None,
                local_window: LOCAL_WINDOW,
                window_gen: 0,
                inbuf: Vec::new(),
                pending: HashMap::new(),
                next_id: 1,
                version: None,
            }),
            cv: Condvar::new(),
            sock,
            fingerprint,
            cipher,
        }
    }

    pub fn is_alive(&self) -> bool {
        lock(&self.st).dead.is_none()
    }

    /// Ends the connection for every thread with `e`.
    fn fail(&self, st: &mut State, e: Error) {
        if st.dead.is_none() {
            st.dead = Some(e);
            let _ = self.sock.shutdown(std::net::Shutdown::Both);
        }
        self.cv.notify_all();
    }

    /// Sends one SFTP message as channel data, whole.
    pub fn send(&self, data: &[u8]) -> Result<(), Error> {
        loop {
            let gen = {
                let st = lock(&self.st);
                if let Some(e) = &st.dead {
                    return Err(e.clone());
                }
                st.window_gen
            };
            {
                let mut tx = lock(&self.tx);
                if tx.window >= data.len() as u64 {
                    let r = tx.send_data(data);
                    drop(tx);
                    if let Err(e) = &r {
                        self.fail(&mut lock(&self.st), e.clone());
                    }
                    return r;
                }
            }
            // Too little window: wait until the server adjusts it.
            self.wait(|st| (st.window_gen != gen).then_some(()))?;
        }
    }

    /// Registers a request id, then sends the message `build` makes for it.
    pub fn request(&self, build: impl FnOnce(u32) -> Vec<u8>) -> Result<u32, Error> {
        let id = {
            let mut st = lock(&self.st);
            if let Some(e) = &st.dead {
                return Err(e.clone());
            }
            let mut id = st.next_id;
            while st.pending.contains_key(&id) {
                id = id.wrapping_add(1);
            }
            st.next_id = id.wrapping_add(1);
            st.pending.insert(id, Slot::Waiting);
            id
        };
        if let Err(e) = self.send(&build(id)) {
            lock(&self.st).pending.remove(&id);
            return Err(e);
        }
        Ok(id)
    }

    /// Waits for the answer to request `id`: its type and the rest after
    /// the id.
    pub fn response(&self, id: u32) -> Result<(u8, Vec<u8>), Error> {
        let r = self.wait(|st| match st.pending.remove(&id) {
            Some(Slot::Ready(t, data)) => Some((t, data)),
            Some(other) => {
                st.pending.insert(id, other);
                None
            }
            None => Some((0, Vec::new())),
        });
        if r.is_err() {
            lock(&self.st).pending.remove(&id);
        }
        match r {
            Ok((0, _)) => Err(Error::Protocol("unknown request".into())),
            r => r,
        }
    }

    /// Gives up on request `id` (its answer is dropped).
    pub fn forget(&self, id: u32) {
        let mut st = lock(&self.st);
        if let Some(slot) = st.pending.get_mut(&id) {
            if matches!(slot, Slot::Waiting) {
                *slot = Slot::Discard;
            } else {
                st.pending.remove(&id);
            }
        }
    }

    /// Waits until `f` finds what it wants in the state, reading packets
    /// meanwhile (or letting another thread read them).
    pub fn wait<T>(&self, mut f: impl FnMut(&mut State) -> Option<T>) -> Result<T, Error> {
        let mut st = lock(&self.st);
        loop {
            if let Some(v) = f(&mut st) {
                return Ok(v);
            }
            if let Some(e) = &st.dead {
                return Err(e.clone());
            }
            st = match st.rx.take() {
                Some(rx) => self.read_one(st, rx),
                None => self.cv.wait(st).unwrap_or_else(|e| e.into_inner()),
            };
        }
    }

    /// Reads and files one packet (as the reader).
    fn read_one<'a>(&'a self, st: MutexGuard<'a, State>, mut rx: Box<Rx>) -> MutexGuard<'a, State> {
        drop(st);
        let res = loop {
            match rx.half.recv() {
                Ok(m) if m[0] == msg::KEXINIT => {
                    let mut tx = lock(&self.tx);
                    if let Err(e) = kex::run(&mut tx.half, &mut rx.half, &mut rx.kex, Some(m)) {
                        break Err(e);
                    }
                }
                Ok(m)
                    if matches!(
                        m[0],
                        msg::IGNORE | msg::DEBUG | msg::UNIMPLEMENTED | msg::EXT_INFO
                    ) => {}
                other => break other,
            }
        };
        let mut st = lock(&self.st);
        st.rx = Some(rx);
        if let Err(e) = res.and_then(|m| self.dispatch(&mut st, &m)) {
            self.fail(&mut st, e);
        }
        self.cv.notify_all();
        st
    }

    fn dispatch(&self, st: &mut State, m: &[u8]) -> Result<(), Error> {
        let mut r = Reader::new(&m[1..]);
        match m[0] {
            msg::DISCONNECT => Err(kex::disconnected(m)),
            msg::GLOBAL_REQUEST => {
                if let Some(reply) = global_request_reply(m)? {
                    lock(&self.tx).half.send(&reply)?;
                }
                Ok(())
            }
            msg::CHANNEL_WINDOW_ADJUST => {
                r.u32()?;
                let n = r.u32()? as u64;
                let mut tx = lock(&self.tx);
                tx.window = (tx.window + n).min(u32::MAX as u64);
                st.window_gen += 1;
                Ok(())
            }
            msg::CHANNEL_DATA | msg::CHANNEL_EXTENDED_DATA => {
                if r.u32()? != LOCAL_ID {
                    return Err(Error::Protocol("data for an unknown channel".into()));
                }
                let stderr = m[0] == msg::CHANNEL_EXTENDED_DATA;
                if stderr {
                    r.u32()?;
                }
                let data = r.string()?;
                if data.len() > st.local_window as usize {
                    return Err(Error::Protocol(
                        "the server ignored the channel window".into(),
                    ));
                }
                st.local_window -= data.len() as u32;
                // Error output of the SFTP server is dropped.
                if !stderr {
                    st.inbuf.extend_from_slice(data);
                    st.deliver()?;
                }
                if st.local_window < LOCAL_WINDOW / 2 {
                    let mut tx = lock(&self.tx);
                    let mut a = vec![msg::CHANNEL_WINDOW_ADJUST];
                    a.u32(tx.remote_id);
                    a.u32(LOCAL_WINDOW - st.local_window);
                    tx.half.send(&a)?;
                    st.local_window = LOCAL_WINDOW;
                }
                Ok(())
            }
            msg::CHANNEL_REQUEST => {
                r.u32()?;
                r.string()?;
                if r.bool()? {
                    let mut tx = lock(&self.tx);
                    let mut a = vec![msg::CHANNEL_FAILURE];
                    a.u32(tx.remote_id);
                    tx.half.send(&a)?;
                }
                Ok(())
            }
            msg::CHANNEL_SUCCESS | msg::CHANNEL_FAILURE => Ok(()),
            msg::CHANNEL_EOF | msg::CHANNEL_CLOSE => {
                Err(Error::Io("the server closed the SFTP channel".into()))
            }
            t => Err(Error::Protocol(format!("unexpected message {t}"))),
        }
    }
}

impl Drop for Conn {
    fn drop(&mut self) {
        let alive = lock(&self.st).dead.is_none();
        if alive {
            let tx = self.tx.get_mut().unwrap_or_else(|e| e.into_inner());
            let mut m = vec![msg::DISCONNECT];
            m.u32(11); // by application
            m.string(b"");
            m.string(b"");
            let _ = tx.half.send(&m);
        }
        let _ = self.sock.shutdown(std::net::Shutdown::Both);
    }
}
