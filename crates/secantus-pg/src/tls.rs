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
//! - A revocation list ([`TlsConfig::crl_file`], PostgreSQL's
//!   `ssl_crl_file`) refuses a client certificate it names, in the
//!   handshake.
//! - [`TlsConfig::client_name`] compares the certificate's whole subject
//!   with the user name (`clientname=DN`), and [`TlsConfig::user_map`]
//!   names a map in a `pg_ident.conf` to say which roles a certificate may
//!   log in as (`map=`).
//!
//! The files are read at startup and again on a reload ([`crate::Reloader`]),
//! which keeps what it had when the new files cannot be used.

use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};

use rustls_pki_types::pem::PemObject;
use rustls_pki_types::{CertificateDer, CertificateRevocationListDer, PrivateKeyDer, UnixTime};
use tokio_rustls::rustls::client::danger::HandshakeSignatureValid;
use tokio_rustls::rustls::crypto::ring::default_provider;
use tokio_rustls::rustls::server::danger::{ClientCertVerified, ClientCertVerifier};
use tokio_rustls::rustls::server::WebPkiClientVerifier;
use tokio_rustls::rustls::{
    CertificateError, DigitallySignedStruct, DistinguishedName, Error as TlsError, RootCertStore,
    ServerConfig, SignatureScheme,
};
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

/// What of a client certificate is compared with the user name, or handed
/// to the user name map: PostgreSQL's `clientname`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ClientName {
    /// The subject's common name: `clientname=CN`, the default.
    #[default]
    CommonName,
    /// The whole subject as RFC 2253 writes it, `CN=alice,O=Example`:
    /// `clientname=DN`. Seldom equal to a user name, so it goes with a
    /// user name map.
    DistinguishedName,
}

impl std::str::FromStr for ClientName {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "CN" => Ok(ClientName::CommonName),
            "DN" => Ok(ClientName::DistinguishedName),
            other => Err(format!("client name {other:?} is not CN or DN")),
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
    /// Certificate revocation lists, PEM: PostgreSQL's `ssl_crl_file`.
    pub crl_file: Option<PathBuf>,
    pub client_name: ClientName,
    /// A `pg_ident.conf` and the name of the map in it to use.
    pub user_map: Option<(PathBuf, String)>,
}

impl TlsConfig {
    pub fn new(cert_file: impl Into<PathBuf>, key_file: impl Into<PathBuf>) -> Self {
        TlsConfig {
            cert_file: cert_file.into(),
            key_file: key_file.into(),
            ca_file: None,
            client_cert: ClientCert::Optional,
            require_tls: false,
            crl_file: None,
            client_name: ClientName::CommonName,
            user_map: None,
        }
    }

    /// Refuse a client certificate that a revocation list in this PEM file
    /// names. Needs a CA file. As with PostgreSQL's `ssl_crl_file`, the
    /// file must then hold a list from every CA in a client certificate's
    /// chain, and none of them past its next update: a certificate whose
    /// CA has no list here, or an expired one, is refused. A list with a
    /// critical extension (one scoped by a distribution point, a delta
    /// list) is refused when the file is read.
    pub fn crl_file(mut self, crl_file: impl Into<PathBuf>) -> Self {
        self.crl_file = Some(crl_file.into());
        self
    }

    /// What of the certificate names the user. Other than the common name
    /// only where the certificate must name the user
    /// ([`ClientCert::VerifyFull`], [`ClientCert::Cert`]).
    pub fn client_name(mut self, name: ClientName) -> Self {
        self.client_name = name;
        self
    }

    /// Let a certificate log in as the roles the map called `map` in the
    /// `pg_ident.conf` at `ident_file` gives it, in place of the one role
    /// it names. Under [`ClientCert::Cert`] only, as PostgreSQL takes
    /// `map=` for the `cert` method and not for `clientcert=verify-full`.
    pub fn user_map(mut self, ident_file: impl Into<PathBuf>, map: impl Into<String>) -> Self {
        self.user_map = Some((ident_file.into(), map.into()));
        self
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
        if self.crl_file.is_some() && self.ca_file.is_none() {
            return Err(invalid_data(
                "a revocation list can only be checked with a CA file".to_string(),
            ));
        }
        let names_user = matches!(self.client_cert, ClientCert::VerifyFull | ClientCert::Cert);
        if self.client_name != ClientName::CommonName && !names_user {
            return Err(invalid_data(
                "a client name other than CN needs a client certificate mode of verify-full or cert"
                    .to_string(),
            ));
        }
        if self.user_map.is_some() && self.client_cert != ClientCert::Cert {
            return Err(invalid_data(
                "a user name map needs a client certificate mode of cert".to_string(),
            ));
        }
        let user_map = match &self.user_map {
            Some((file, map)) => Some(crate::ident::read(file, map)?),
            None => None,
        };
        let certs = read_certs(&self.cert_file)?;
        let key = read_key(&self.key_file)?;
        let end_point = crate::x509::end_point(&certs[0]);
        let provider = Arc::new(default_provider());
        let builder = ServerConfig::builder_with_provider(provider.clone())
            .with_safe_default_protocol_versions()
            .map_err(|e| invalid(&self.cert_file, &e))?;
        let builder = match &self.ca_file {
            None => builder.with_no_client_auth(),
            Some(ca_file) => {
                let authorities = read_certs(ca_file)?;
                let revocation = match &self.crl_file {
                    Some(crl_file) => Some(crate::crl::Revocation::new(
                        read_crls(crl_file)?,
                        authorities.iter().map(|ca| ca.to_vec()).collect(),
                        provider.signature_verification_algorithms,
                    )),
                    None => None,
                };
                let mut roots = RootCertStore::empty();
                for ca in authorities {
                    roots.add(ca).map_err(|e| invalid(ca_file, &e))?;
                }
                // A client with no certificate completes the handshake, as
                // it does with PostgreSQL: whether one was needed is the
                // login's question (`ClientCert`), and its answer is an
                // error message the client can read.
                let chain = WebPkiClientVerifier::builder_with_provider(roots.into(), provider)
                    .allow_unauthenticated()
                    .build()
                    .map_err(|e| invalid(ca_file, &e))?;
                builder.with_client_cert_verifier(Arc::new(ClientVerifier { chain, revocation }))
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
            end_point,
            client_name: self.client_name,
            user_map,
            client_cert: self.client_cert,
            require_tls: self.require_tls || self.client_cert != ClientCert::Optional,
        };
        Ok((TlsAcceptor::from(Arc::new(config)), Arc::new(policy)))
    }
}

/// What every connection to a server with TLS is held to at login.
#[derive(Debug)]
pub(crate) struct TlsPolicy {
    /// What a SCRAM channel binding to the server certificate carries.
    /// `None` for a certificate `x509` cannot read, and channel binding is
    /// then not offered.
    pub end_point: Option<crate::x509::EndPoint>,
    pub client_name: ClientName,
    pub user_map: Option<crate::ident::UserMap>,
    pub client_cert: ClientCert,
    /// Refuse a connection that is not TLS.
    pub require_tls: bool,
}

/// The TLS a running server offers, which a reload replaces.
pub(crate) struct LiveTls {
    config: TlsConfig,
    current: RwLock<(TlsAcceptor, Arc<TlsPolicy>)>,
}

impl LiveTls {
    pub(crate) fn new(config: &TlsConfig) -> io::Result<Self> {
        Ok(LiveTls {
            config: config.clone(),
            current: RwLock::new(config.build()?),
        })
    }

    /// What a connection accepted now is offered and held to.
    pub(crate) fn current(&self) -> (TlsAcceptor, Arc<TlsPolicy>) {
        self.current
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// Read the files again. A connection accepted from here on gets what
    /// they hold now; one already open keeps what it has. When they cannot
    /// be used the server keeps what it had, as PostgreSQL does ("SSL
    /// configuration was not reloaded").
    pub(crate) fn reload(&self) -> io::Result<()> {
        let fresh = self.config.build()?;
        *self.current.write().unwrap_or_else(|e| e.into_inner()) = fresh;
        Ok(())
    }
}

/// rustls's client certificate verifier for the chain, and then this
/// crate's own check of the revocation lists (see `crl`).
#[derive(Debug)]
struct ClientVerifier {
    chain: Arc<dyn ClientCertVerifier>,
    revocation: Option<crate::crl::Revocation>,
}

impl ClientCertVerifier for ClientVerifier {
    fn offer_client_auth(&self) -> bool {
        self.chain.offer_client_auth()
    }

    fn client_auth_mandatory(&self) -> bool {
        self.chain.client_auth_mandatory()
    }

    fn root_hint_subjects(&self) -> &[DistinguishedName] {
        self.chain.root_hint_subjects()
    }

    fn verify_client_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        intermediates: &[CertificateDer<'_>],
        now: UnixTime,
    ) -> Result<ClientCertVerified, TlsError> {
        let verified = self
            .chain
            .verify_client_cert(end_entity, intermediates, now)?;
        let Some(revocation) = &self.revocation else {
            return Ok(verified);
        };
        let intermediates: Vec<&[u8]> = intermediates.iter().map(|c| c.as_ref()).collect();
        let now = i64::try_from(now.as_secs()).unwrap_or(i64::MAX);
        use crate::crl::Refused;
        // Each maps to the alert OpenSSL sends for it, which is what a
        // client of PostgreSQL reads.
        match revocation.check(end_entity.as_ref(), &intermediates, now) {
            Ok(()) => Ok(verified),
            Err(refused) => Err(TlsError::InvalidCertificate(match refused {
                Refused::Revoked => CertificateError::Revoked,
                Refused::ListExpired => CertificateError::Expired,
                Refused::ListNotYetValid => CertificateError::BadEncoding,
                Refused::NoList => CertificateError::UnknownIssuer,
                Refused::BadSignature => CertificateError::BadSignature,
                Refused::Unreadable => CertificateError::BadEncoding,
            })),
        }
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, TlsError> {
        self.chain.verify_tls12_signature(message, cert, dss)
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, TlsError> {
        self.chain.verify_tls13_signature(message, cert, dss)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.chain.supported_verify_schemes()
    }
}

fn read_crls(path: &Path) -> io::Result<Vec<crate::crl::RevocationList>> {
    let lists = CertificateRevocationListDer::pem_file_iter(path)
        .map_err(|e| invalid(path, &e))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| invalid(path, &e))?;
    if lists.is_empty() {
        return Err(invalid_data(format!(
            "TLS revocation list file {} holds no revocation list",
            path.display()
        )));
    }
    lists
        .iter()
        .enumerate()
        .map(|(index, der)| {
            crate::crl::RevocationList::parse(der).map_err(|reason| {
                invalid_data(format!(
                    "TLS revocation list file {}, list {}: {reason}",
                    path.display(),
                    index + 1
                ))
            })
        })
        .collect()
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
    use super::{ClientCert, ClientName, TlsConfig};

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

    #[test]
    fn an_option_that_needs_another_is_refused_before_any_file_is_read() {
        let dir = tempfile::tempdir().expect("tempdir");
        let base = || TlsConfig::new(dir.path().join("s.crt"), dir.path().join("s.key"));
        let with_ca = || base().ca_file(dir.path().join("ca.pem"));
        let cases = [
            (base().crl_file(dir.path().join("crl.pem")), "CA file"),
            (
                with_ca().client_name(ClientName::DistinguishedName),
                "verify-full or cert",
            ),
            (
                with_ca()
                    .client_cert(ClientCert::VerifyCa)
                    .client_name(ClientName::DistinguishedName),
                "verify-full or cert",
            ),
            (
                with_ca()
                    .client_cert(ClientCert::VerifyFull)
                    .user_map(dir.path().join("ident.conf"), "m"),
                "mode of cert",
            ),
        ];
        for (config, expected) in cases {
            let err = config.build().err().expect(expected);
            assert!(err.to_string().contains(expected), "{err}");
        }
        // A user name map file that is not there is named.
        let err = with_ca()
            .client_cert(ClientCert::Cert)
            .user_map(dir.path().join("ident.conf"), "m")
            .build()
            .err()
            .expect("no such file");
        assert!(err.to_string().contains("ident.conf"), "{err}");
    }

    #[test]
    fn a_client_name_is_read_by_its_postgresql_name() {
        assert_eq!("CN".parse(), Ok(ClientName::CommonName));
        assert_eq!("DN".parse(), Ok(ClientName::DistinguishedName));
        assert!("dn".parse::<ClientName>().is_err());
    }
}
