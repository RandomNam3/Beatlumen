//! TLS for the server's outgoing connections (push services, later remote
//! music sources), with rustls and the Mozilla root certificates built in,
//! since the container image has no certificate store. Extra roots (a
//! private CA) can be added with `BEATLUMEN_CA_FILE` (PEM).

use bl_net::client::Conn;
use rustls::pki_types::pem::PemObject;
use rustls::pki_types::{CertificateDer, ServerName};
use rustls::{ClientConfig, ClientConnection, RootCertStore, StreamOwned};
use std::io;
use std::net::{TcpStream, ToSocketAddrs};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

fn config() -> Arc<ClientConfig> {
    static CONFIG: OnceLock<Arc<ClientConfig>> = OnceLock::new();
    CONFIG
        .get_or_init(|| {
            let mut roots = RootCertStore {
                roots: webpki_roots::TLS_SERVER_ROOTS.to_vec(),
            };
            if let Some(file) = std::env::var_os("BEATLUMEN_CA_FILE").filter(|f| !f.is_empty()) {
                match CertificateDer::pem_file_iter(&file) {
                    Ok(certs) => {
                        let (added, _) = roots.add_parsable_certificates(certs.flatten());
                        crate::info!(
                            "TLS: {added} extra root certificates from {}",
                            file.to_string_lossy()
                        );
                    }
                    Err(e) => crate::warn!("TLS: cannot read {}: {e}", file.to_string_lossy()),
                }
            }
            let provider = Arc::new(rustls::crypto::ring::default_provider());
            Arc::new(
                ClientConfig::builder_with_provider(provider)
                    .with_safe_default_protocol_versions()
                    .expect("default TLS versions")
                    .with_root_certificates(roots)
                    .with_no_client_auth(),
            )
        })
        .clone()
}

/// [`bl_net::client::TlsConnector`] for the server.
fn connect(
    host: &str,
    port: u16,
    timeout: Duration,
    pin: Option<&str>,
) -> io::Result<Box<dyn Conn>> {
    if pin.is_some() {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "certificate pins are not used by the server",
        ));
    }
    let name = ServerName::try_from(host.to_string())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "invalid host name"))?;
    let mut last = io::Error::new(io::ErrorKind::NotFound, "no address");
    for addr in (host, port).to_socket_addrs()? {
        match TcpStream::connect_timeout(&addr, timeout) {
            Ok(tcp) => {
                tcp.set_read_timeout(Some(timeout))?;
                tcp.set_write_timeout(Some(timeout))?;
                let conn = ClientConnection::new(config(), name).map_err(io::Error::other)?;
                return Ok(Box::new(StreamOwned::new(conn, tcp)));
            }
            Err(e) => last = e,
        }
    }
    Err(last)
}

/// Makes `https://` URLs work in `bl_net::client`.
pub fn install() {
    bl_net::client::set_tls_connector(connect);
}
