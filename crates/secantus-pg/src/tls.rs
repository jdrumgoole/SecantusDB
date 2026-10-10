//! Server-side TLS: a certificate and key on disk become the acceptor the
//! accept loop hands to every connection.
//!
//! PostgreSQL negotiates TLS in-band. A client that wants it opens with an
//! `SSLRequest`; the server answers one byte, `S` and then a handshake, or
//! `N` and the connection carries on in the clear. With no certificate
//! configured this server answers `N`, which is what PostgreSQL does with
//! `ssl = off`, and a client that insists (`sslmode=require`) gives up with
//! "server does not support SSL". With one configured it answers `S`.
//!
//! A plaintext connection is still accepted when TLS is configured, as
//! PostgreSQL accepts one under a `host` line in `pg_hba.conf`: refusing it
//! is what `hostssl` does, and this server has no `pg_hba.conf`.
//!
//! Not here: client certificates (`clientcert=verify-ca` / `verify-full`, the
//! `cert` method) and SCRAM channel binding (`SCRAM-SHA-256-PLUS`). A client
//! that requires either is refused by its own library.

use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use rustls_pki_types::pem::PemObject;
use rustls_pki_types::{CertificateDer, PrivateKeyDer};
use tokio_rustls::rustls::crypto::ring::default_provider;
use tokio_rustls::rustls::ServerConfig;
use tokio_rustls::TlsAcceptor;

/// The ALPN protocol a PostgreSQL 17 client offers, and the only one a
/// direct-TLS connection (`sslnegotiation=direct`) may select.
const ALPN_POSTGRESQL: &[u8] = b"postgresql";

/// Where the server's certificate chain and private key are, both PEM.
///
/// The certificate file holds the server's certificate first and any
/// intermediates after it, as PostgreSQL's `ssl_cert_file` does. The key is
/// PKCS#8, PKCS#1 (RSA) or SEC1 (EC), and not passphrase-protected.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TlsConfig {
    pub cert_file: PathBuf,
    pub key_file: PathBuf,
}

impl TlsConfig {
    pub fn new(cert_file: impl Into<PathBuf>, key_file: impl Into<PathBuf>) -> Self {
        TlsConfig {
            cert_file: cert_file.into(),
            key_file: key_file.into(),
        }
    }

    /// Read both files and build the acceptor. Every failure names the file
    /// it came from: a server that starts without the TLS it was asked for
    /// would serve in the clear to a client that only prefers TLS, so a bad
    /// path or an unusable key stops the server from starting at all.
    pub(crate) fn acceptor(&self) -> io::Result<TlsAcceptor> {
        let certs = read_certs(&self.cert_file)?;
        let key = read_key(&self.key_file)?;
        let mut config = ServerConfig::builder_with_provider(Arc::new(default_provider()))
            .with_safe_default_protocol_versions()
            .map_err(|e| invalid(&self.cert_file, &e))?
            .with_no_client_auth()
            .with_single_cert(certs, key)
            .map_err(|e| {
                invalid_data(format!(
                    "TLS certificate {} and key {} cannot be used together: {e}",
                    self.cert_file.display(),
                    self.key_file.display()
                ))
            })?;
        config.alpn_protocols = vec![ALPN_POSTGRESQL.to_vec()];
        Ok(TlsAcceptor::from(Arc::new(config)))
    }
}

fn read_certs(path: &Path) -> io::Result<Vec<CertificateDer<'static>>> {
    let certs = CertificateDer::pem_file_iter(path)
        .map_err(|e| invalid(path, &e))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| invalid(path, &e))?;
    if certs.is_empty() {
        return Err(invalid_data(format!(
            "TLS certificate file {} holds no certificate",
            path.display()
        )));
    }
    Ok(certs)
}

fn read_key(path: &Path) -> io::Result<PrivateKeyDer<'static>> {
    PrivateKeyDer::from_pem_file(path).map_err(|e| invalid(path, &e))
}

fn invalid(path: &Path, e: &dyn std::fmt::Display) -> io::Error {
    invalid_data(format!("TLS file {}: {e}", path.display()))
}

fn invalid_data(message: String) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

#[cfg(test)]
mod tests {
    use super::TlsConfig;

    #[test]
    fn a_missing_certificate_file_is_named_in_the_error() {
        let dir = tempfile::tempdir().expect("tempdir");
        let cert = dir.path().join("absent.crt");
        let err = TlsConfig::new(&cert, dir.path().join("absent.key"))
            .acceptor()
            .err()
            .expect("no such file");
        assert!(err.to_string().contains("absent.crt"), "{err}");
    }

    #[test]
    fn a_certificate_file_with_no_certificate_is_refused() {
        let dir = tempfile::tempdir().expect("tempdir");
        let cert = dir.path().join("empty.crt");
        std::fs::write(&cert, "not a certificate\n").expect("write");
        let err = TlsConfig::new(&cert, dir.path().join("absent.key"))
            .acceptor()
            .err()
            .expect("no certificate");
        assert!(err.to_string().contains("holds no certificate"), "{err}");
    }
}
