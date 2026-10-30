//! A small SSH 2 client with SFTP version 3, made to read music files from
//! SSH servers (NAS systems, Linux servers).
//!
//! * [`Session::connect`]: TCP with timeouts, the version exchange, the key
//!   exchange and the login (password or Ed25519 key).
//! * [`Session::sftp`]: a session channel with the "sftp" subsystem.
//! * [`Sftp`]: folders, file attributes, and [`SftpFile`], a seekable
//!   reader with read-ahead (up to 8 requests of 32 KiB in flight).
//!
//! Only modern algorithms, the ones current OpenSSH prefers:
//! curve25519-sha256 key exchange with strict mode (Terrapin), host keys
//! Ed25519, ECDSA P-256 and RSA with SHA-2 (2048 bits or more), the AEAD
//! ciphers chacha20-poly1305@openssh.com and aes256-gcm@openssh.com, no
//! compression. The host key is pinned: the caller passes the expected
//! SHA-256 fingerprint; without one the connection fails with
//! [`Error::UnknownHostKey`] carrying the server's (verified) fingerprint
//! for the user to check and trust, and a different key fails with
//! [`Error::HostKeyChanged`]. Rekeying started by the server is answered.
//!
//! Threads: one connection carries one SFTP channel. [`Sftp`] is a cheap
//! handle (`Clone`, `Send`, `Sync`) that threads share; requests carry ids
//! and may be in flight from several threads at once. Sending takes a
//! short lock; whichever thread waits for an answer reads the socket for
//! everyone and hands answers to their threads (the others wait on a
//! condition variable). The first error ends the connection for all.
//!
//! Everything read from the network is bounded: packets (256 KiB), SFTP
//! messages (1 MiB), version lines, handles, entries per folder.

mod base64;
mod cipher;
mod conn;
mod hostkey;
mod kex;
mod key;
mod session;
mod sftp;
mod transport;
mod wire;

pub use cipher::Cipher;
pub use hostkey::fingerprint;
pub use key::{Auth, PrivateKey};
pub use session::{Options, Session};
pub use sftp::{DirEntry, Metadata, Sftp, SftpFile};

/// SSH message numbers (RFC 4250 section 4.1).
pub(crate) mod msg {
    pub const DISCONNECT: u8 = 1;
    pub const IGNORE: u8 = 2;
    pub const UNIMPLEMENTED: u8 = 3;
    pub const DEBUG: u8 = 4;
    pub const SERVICE_REQUEST: u8 = 5;
    pub const SERVICE_ACCEPT: u8 = 6;
    pub const EXT_INFO: u8 = 7;
    pub const KEXINIT: u8 = 20;
    pub const NEWKEYS: u8 = 21;
    pub const KEX_ECDH_INIT: u8 = 30;
    pub const KEX_ECDH_REPLY: u8 = 31;
    pub const USERAUTH_REQUEST: u8 = 50;
    pub const USERAUTH_FAILURE: u8 = 51;
    pub const USERAUTH_SUCCESS: u8 = 52;
    pub const USERAUTH_BANNER: u8 = 53;
    pub const USERAUTH_PASSWD_CHANGEREQ: u8 = 60;
    pub const GLOBAL_REQUEST: u8 = 80;
    pub const REQUEST_FAILURE: u8 = 82;
    pub const CHANNEL_OPEN: u8 = 90;
    pub const CHANNEL_OPEN_CONFIRMATION: u8 = 91;
    pub const CHANNEL_OPEN_FAILURE: u8 = 92;
    pub const CHANNEL_WINDOW_ADJUST: u8 = 93;
    pub const CHANNEL_DATA: u8 = 94;
    pub const CHANNEL_EXTENDED_DATA: u8 = 95;
    pub const CHANNEL_EOF: u8 = 96;
    pub const CHANNEL_CLOSE: u8 = 97;
    pub const CHANNEL_REQUEST: u8 = 98;
    pub const CHANNEL_SUCCESS: u8 = 99;
    pub const CHANNEL_FAILURE: u8 = 100;
}

/// SFTP status codes (draft-ietf-secsh-filexfer-02 section 7).
pub mod status {
    pub const EOF: u32 = 1;
    pub const NO_SUCH_FILE: u32 = 2;
    pub const PERMISSION_DENIED: u32 = 3;
    pub const FAILURE: u32 = 4;
    pub const BAD_MESSAGE: u32 = 5;
    pub const OP_UNSUPPORTED: u32 = 8;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    /// Network failure, timeout, or the server closed the connection.
    Io(String),
    /// The server broke the protocol or sent something unsupported.
    Protocol(String),
    /// No algorithm in common with the server.
    Negotiation(String),
    /// No pin given: the server's host key fingerprint (`SHA256:…`), for
    /// the user to compare and trust.
    UnknownHostKey(String),
    /// The host key is not the pinned one.
    HostKeyChanged { expected: String, found: String },
    /// The login was refused.
    Auth(String),
    /// The private key cannot be used.
    Key(String),
    /// An SFTP request failed: status code ([`status`]) and the server's
    /// message.
    Sftp(u32, String),
}

impl Error {
    pub fn is_not_found(&self) -> bool {
        matches!(self, Error::Sftp(status::NO_SUCH_FILE, _))
    }
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Io(m) | Error::Protocol(m) | Error::Negotiation(m) => write!(f, "{m}"),
            Error::UnknownHostKey(fp) => write!(
                f,
                "unknown host key {fp}: compare it with the server's and trust it to connect"
            ),
            Error::HostKeyChanged { expected, found } => write!(
                f,
                "host key changed: the server now shows {found} instead of the trusted {expected} \
                 (an attack, or the server was reinstalled)"
            ),
            Error::Auth(m) => write!(f, "login refused: {m}"),
            Error::Key(m) => write!(f, "{m}"),
            Error::Sftp(code, m) => {
                let what = match *code {
                    status::EOF => "end of file",
                    status::NO_SUCH_FILE => "no such file",
                    status::PERMISSION_DENIED => "permission denied",
                    status::OP_UNSUPPORTED => "not supported by the server",
                    _ => "SFTP request failed",
                };
                if m.is_empty() || m.eq_ignore_ascii_case(what) {
                    write!(f, "{what}")
                } else {
                    write!(f, "{what} ({m})")
                }
            }
        }
    }
}

impl std::error::Error for Error {}

impl From<Error> for std::io::Error {
    fn from(e: Error) -> std::io::Error {
        let kind = match &e {
            Error::Sftp(status::NO_SUCH_FILE, _) => std::io::ErrorKind::NotFound,
            Error::Sftp(status::PERMISSION_DENIED, _) => std::io::ErrorKind::PermissionDenied,
            _ => std::io::ErrorKind::Other,
        };
        std::io::Error::new(kind, e.to_string())
    }
}
