//! Connecting: TCP, versions, the first key exchange and the login
//! (RFC 4252: password, or public key with the signature sent right away).

use crate::kex::{self, KexContext};
use crate::transport::{self, RecvHalf, SendHalf};
use crate::wire::{Put, Reader};
use crate::{msg, Auth, Cipher, Error, Sftp};
use std::time::Duration;
use zeroize::Zeroizing;

/// Connection settings.
#[derive(Clone, Debug)]
pub struct Options {
    pub connect_timeout: Duration,
    /// Longest wait for the server once connected.
    pub io_timeout: Duration,
    /// Ciphers offered, in order of preference.
    pub ciphers: Vec<Cipher>,
}

impl Default for Options {
    fn default() -> Options {
        Options {
            connect_timeout: Duration::from_secs(10),
            io_timeout: Duration::from_secs(30),
            ciphers: Cipher::ALL.to_vec(),
        }
    }
}

/// A connected and logged in SSH connection, before its channel opens.
pub struct Session {
    pub(crate) tx: SendHalf,
    pub(crate) rx: RecvHalf,
    pub(crate) kex: KexContext,
}

/// The answer to a global request the server wants one for (we accept
/// none; OpenSSH's host key announcements need none).
pub(crate) fn global_request_reply(m: &[u8]) -> Result<Option<Vec<u8>>, Error> {
    let mut r = Reader::new(&m[1..]);
    r.string()?;
    Ok(r.bool()?.then(|| vec![msg::REQUEST_FAILURE]))
}

impl Session {
    /// Connects to `host:port` and logs in as `user`. `pin` is the
    /// expected host key fingerprint (`SHA256:…`); when it is empty the
    /// connection fails with [`Error::UnknownHostKey`].
    pub fn connect(
        host: &str,
        port: u16,
        pin: &str,
        user: &str,
        auth: &Auth,
    ) -> Result<Session, Error> {
        Session::connect_with(host, port, pin, user, auth, &Options::default())
    }

    pub fn connect_with(
        host: &str,
        port: u16,
        pin: &str,
        user: &str,
        auth: &Auth,
        opts: &Options,
    ) -> Result<Session, Error> {
        if opts.ciphers.is_empty() {
            return Err(Error::Negotiation("no cipher allowed".into()));
        }
        let sock = transport::connect(host, port, opts.connect_timeout, opts.io_timeout)?;
        let (mut tx, mut rx) = transport::split(sock)?;
        let v_s = transport::exchange_versions(&mut tx, &mut rx)?;
        let mut s = Session {
            tx,
            rx,
            kex: KexContext::new(v_s, pin, &opts.ciphers),
        };
        let r = kex::run(&mut s.tx, &mut s.rx, &mut s.kex, None)
            .and_then(|_| s.authenticate(user, auth));
        if let Err(e) = r {
            // Reasons from RFC 4253 section 11.1.
            let code = match e {
                Error::UnknownHostKey(_) | Error::HostKeyChanged { .. } => 9,
                Error::Auth(_) | Error::Key(_) => 14,
                Error::Negotiation(_) => 3,
                _ => 2,
            };
            s.disconnect(code);
            return Err(e);
        }
        Ok(s)
    }

    /// The server's host key fingerprint (`SHA256:…`).
    pub fn fingerprint(&self) -> &str {
        &self.kex.fingerprint
    }

    /// The cipher in use (client to server).
    pub fn cipher(&self) -> Option<Cipher> {
        self.kex.cipher
    }

    /// Opens the SFTP channel; the connection then serves only it.
    pub fn sftp(self) -> Result<Sftp, Error> {
        Sftp::start(self)
    }

    pub(crate) fn disconnect(&mut self, code: u32) {
        let mut m = vec![msg::DISCONNECT];
        m.u32(code);
        m.string(b"");
        m.string(b"");
        let _ = self.tx.send(&m);
        self.tx.shutdown();
    }

    /// The next message of the connection layer; transport messages
    /// (ignore, debug, global requests, rekeying) are handled here.
    pub(crate) fn next(&mut self) -> Result<Vec<u8>, Error> {
        loop {
            let m = self.rx.recv()?;
            match m[0] {
                msg::IGNORE | msg::DEBUG | msg::UNIMPLEMENTED | msg::EXT_INFO => {}
                msg::DISCONNECT => return Err(kex::disconnected(&m)),
                msg::KEXINIT => kex::run(&mut self.tx, &mut self.rx, &mut self.kex, Some(m))?,
                msg::GLOBAL_REQUEST => {
                    if let Some(reply) = global_request_reply(&m)? {
                        self.tx.send(&reply)?;
                    }
                }
                _ => return Ok(m),
            }
        }
    }

    fn authenticate(&mut self, user: &str, auth: &Auth) -> Result<(), Error> {
        let mut m = vec![msg::SERVICE_REQUEST];
        m.string(b"ssh-userauth");
        self.tx.send(&m)?;
        if self.next()?[0] != msg::SERVICE_ACCEPT {
            return Err(Error::Protocol(
                "the server refused the login service".into(),
            ));
        }
        let mut m = Zeroizing::new(vec![msg::USERAUTH_REQUEST]);
        m.string(user.as_bytes());
        m.string(b"ssh-connection");
        match auth {
            Auth::Password(p) => {
                m.string(b"password");
                m.bool(false);
                m.string(p.as_bytes());
            }
            Auth::Key(k) => {
                m.string(b"publickey");
                m.bool(true);
                m.string(k.algorithm().as_bytes());
                m.string(&k.public_blob());
                // Signed: the session id and the request so far.
                let session_id = self.kex.session_id.as_deref().unwrap_or_default();
                let mut data = Vec::with_capacity(4 + session_id.len() + m.len());
                data.string(session_id);
                data.extend_from_slice(&m);
                let sig = k.sign(&data);
                m.string(&sig);
            }
        }
        self.tx.send(&m)?;
        loop {
            let r = self.next()?;
            match r[0] {
                msg::USERAUTH_SUCCESS => return Ok(()),
                msg::USERAUTH_BANNER => {}
                msg::USERAUTH_FAILURE => {
                    let mut rd = Reader::new(&r[1..]);
                    let methods = rd.name_list()?;
                    let partial = rd.bool()?;
                    let wanted = match auth {
                        Auth::Password(_) => "password",
                        Auth::Key(_) => "publickey",
                    };
                    return Err(Error::Auth(if partial {
                        "the server asks for a further login step, which is not supported".into()
                    } else if !methods.contains(&wanted) {
                        format!(
                            "the server does not accept {} (it accepts: {})",
                            if wanted == "password" {
                                "passwords"
                            } else {
                                "keys"
                            },
                            methods.join(", ")
                        )
                    } else if wanted == "password" {
                        "wrong user name or password".into()
                    } else {
                        "the server does not accept this key for this user".into()
                    }));
                }
                msg::USERAUTH_PASSWD_CHANGEREQ => {
                    return Err(Error::Auth("the password has expired".into()))
                }
                t => return Err(Error::Protocol(format!("unexpected message {t} at login"))),
            }
        }
    }
}
