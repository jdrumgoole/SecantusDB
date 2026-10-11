//! Server-side TLS: a certificate and key on disk become the acceptor the
//! accept loop hands to every connection, and the policy each connection's
//! login is held to.
//!
//! PostgreSQL negotiates TLS in-band. A client that wants it opens with an
//! `SSLRequest`; the server answers one byte, `S` and then a handshake, or
//! `N` and the connection carries on in the clear. With no certificate
//! configured this server answers `N`, which is what PostgreSQL does with
//! `ssl = off`, and a client that insists (`sslmode=require`) gives up with
//! "server does not support SSL". With one configured it answers `S`.
//!
//! PostgreSQL decides who may connect, and how, in `pg_hba.conf`. This
//! server has no such file; [`TlsConfig`] holds the three choices it offers
//! in its place, each named for the `pg_hba.conf` line it stands for:
//!
//! - A plaintext connection is accepted beside TLS, as under a `host` line,
//!   unless [`TlsConfig::require_tls`] is set, which is `hostssl` alone.
//! - With a CA file ([`TlsConfig::ca_file`], PostgreSQL's `ssl_ca_file`) a
//!   client certificate is asked for and, when one is sent, must be signed
//!   by that CA. [`ClientCert`] says whether one must be sent
//!   (`clientcert=verify-ca`), must also name the user
//!   (`clientcert=verify-full`), or stands in for the password (`cert`).
//! - A password login over TLS is offered SCRAM channel binding
//!   (`SCRAM-SHA-256-PLUS`), as PostgreSQL offers it.
//!
//! Not here: a certificate revocation list, a user name map
//! (`pg_ident.conf`), and matching on the certificate's whole DN
//! (`clientname=DN`).

use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use rustls_pki_types::pem::PemObject;
use rustls_pki_types::{CertificateDer, PrivateKeyDer};
use tokio_rustls::rustls::crypto::ring::default_provider;
use tokio_rustls::rustls::server::WebPkiClientVerifier;
use tokio_rustls::rustls::{RootCertStore, ServerConfig};
use tokio_rustls::TlsAcceptor;

/// The ALPN protocol a PostgreSQL 17 client offers, and the only one a
/// direct-TLS connection (`sslnegotiation=direct`) may select.
const ALPN_POSTGRESQL: &[u8] = b"postgresql";

/// What a connection must show by way of a client certificate. Every choice
/// but [`ClientCert::Optional`] needs a CA file and refuses a connection that
/// is not TLS, as PostgreSQL allows `clientcert` on a `hostssl` line only.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ClientCert {
    /// None needed. One that is sent must still be signed by the CA.
    #[default]
    Optional,
    /// One signed by the CA: `clientcert=verify-ca`.
    VerifyCa,
    /// One signed by the CA whose common name is the user name, on top of
    /// the password a role with one must still give:
    /// `clientcert=verify-full`.
    VerifyFull,
    /// One signed by the CA whose common name is the user name, and no
    /// password: the `cert` authentication method.
    Cert,
}

impl std::str::FromStr for ClientCert {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "verify-ca" => Ok(ClientCert::VerifyCa),
            "verify-full" => Ok(ClientCert::VerifyFull),
            "cert" => Ok(ClientCert::Cert),
            other => Err(format!(
                "client certificate mode {other:?} is not verify-ca, verify-full or cert"
            )),
        }
    }
}

/// Where the server's certificate chain and private key are, both PEM, and
/// what a connection is held to.
///
/// The certificate file holds the server's certificate first and any
/// intermediates after it, as PostgreSQL's `ssl_cert_file` does. The key is
/// PKCS#8, PKCS#1 (RSA) or SEC1 (EC), and not passphrase-protected.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct TlsConfig {
    pub cert_file: PathBuf,
    pub key_file: PathBuf,
    /// The certificate authorities a client certificate must chain to, PEM.
    pub ca_file: Option<PathBuf>,
    pub client_cert: ClientCert,
    /// Refuse a connection that is not TLS.
    pub require_tls: bool,
}

impl TlsConfig {
    pub fn new(cert_file: impl Into<PathBuf>, key_file: impl Into<PathBuf>) -> Self {
        TlsConfig {
            cert_file: cert_file.into(),
            key_file: key_file.into(),
            ca_file: None,
            client_cert: ClientCert::Optional,
            require_tls: false,
        }
    }

    /// Ask each TLS client for a certificate, and refuse one that is not
    /// signed by a CA in this PEM file. A client that sends none still
    /// connects unless [`TlsConfig::client_cert`] says otherwise.
    pub fn ca_file(mut self, ca_file: impl Into<PathBuf>) -> Self {
        self.ca_file = Some(ca_file.into());
        self
    }

    /// What a connection must show by way of a client certificate.
    pub fn client_cert(mut self, mode: ClientCert) -> Self {
        self.client_cert = mode;
        self
    }

    /// Refuse a connection that is not TLS.
    pub fn require_tls(mut self, require: bool) -> Self {
        self.require_tls = require;
        self
    }

    /// Read the files and build the acceptor and the policy. Every failure
    /// names the file it came from: a server that starts without the TLS it
    /// was asked for would serve in the clear to a client that only prefers
    /// TLS, so a bad path or an unusable key stops the server from starting
    /// at all.
    pub(crate) fn build(&self) -> io::Result<(TlsAcceptor, Arc<TlsPolicy>)> {
        if self.client_cert != ClientCert::Optional && self.ca_file.is_none() {
            return Err(invalid_data(
                "a client certificate can only be required with a CA file to check it against"
                    .to_string(),
            ));
        }
        let certs = read_certs(&self.cert_file)?;
        let key = read_key(&self.key_file)?;
        let end_point_hash = crate::x509::end_point_hash(&certs[0]);
        let provider = Arc::new(default_provider());
        let builder = ServerConfig::builder_with_provider(provider.clone())
            .with_safe_default_protocol_versions()
            .map_err(|e| invalid(&self.cert_file, &e))?;
        let builder = match &self.ca_file {
            None => builder.with_no_client_auth(),
            Some(ca_file) => {
                let mut roots = RootCertStore::empty();
                for ca in read_certs(ca_file)? {
                    roots.add(ca).map_err(|e| invalid(ca_file, &e))?;
                }
                // A client with no certificate completes the handshake, as
                // it does with PostgreSQL: whether one was needed is the
                // login's question (`ClientCert`), and its answer is an
                // error message the client can read.
                let verifier = WebPkiClientVerifier::builder_with_provider(roots.into(), provider)
                    .allow_unauthenticated()
                    .build()
                    .map_err(|e| invalid(ca_file, &e))?;
                builder.with_client_cert_verifier(verifier)
            }
        };
        let mut config = builder.with_single_cert(certs, key).map_err(|e| {
            invalid_data(format!(
                "TLS certificate {} and key {} cannot be used together: {e}",
                self.cert_file.display(),
                self.key_file.display()
            ))
        })?;
        config.alpn_protocols = vec![ALPN_POSTGRESQL.to_vec()];
        let policy = TlsPolicy {
            end_point_hash,
            client_cert: self.client_cert,
            require_tls: self.require_tls || self.client_cert != ClientCert::Optional,
        };
        Ok((TlsAcceptor::from(Arc::new(config)), Arc::new(policy)))
    }
}

/// What every connection to a server with TLS is held to at login.
#[derive(Debug)]
pub(crate) struct TlsPolicy {
    /// The server certificate's `tls-server-end-point` hash: what a SCRAM
    /// channel binding carries. `None` when the certificate's signature
    /// names no hash, and channel binding is then not offered.
    pub end_point_hash: Option<Vec<u8>>,
    pub client_cert: ClientCert,
    /// Refuse a connection that is not TLS.
    pub require_tls: bool,
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
    use super::{ClientCert, TlsConfig};

    #[test]
    fn a_missing_certificate_file_is_named_in_the_error() {
        let dir = tempfile::tempdir().expect("tempdir");
        let cert = dir.path().join("absent.crt");
        let err = TlsConfig::new(&cert, dir.path().join("absent.key"))
            .build()
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
            .build()
            .err()
            .expect("no certificate");
        assert!(err.to_string().contains("holds no certificate"), "{err}");
    }

    #[test]
    fn requiring_a_client_certificate_needs_a_ca_to_check_it() {
        let dir = tempfile::tempdir().expect("tempdir");
        let err = TlsConfig::new(dir.path().join("s.crt"), dir.path().join("s.key"))
            .client_cert(ClientCert::VerifyCa)
            .build()
            .err()
            .expect("no CA file");
        assert!(err.to_string().contains("CA file"), "{err}");
    }

    #[test]
    fn a_client_certificate_mode_is_read_by_its_postgresql_name() {
        assert_eq!("verify-ca".parse(), Ok(ClientCert::VerifyCa));
        assert_eq!("verify-full".parse(), Ok(ClientCert::VerifyFull));
        assert_eq!("cert".parse(), Ok(ClientCert::Cert));
        assert!("required".parse::<ClientCert>().is_err());
    }
}
