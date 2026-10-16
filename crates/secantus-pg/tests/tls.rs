//! TLS on the embedded server: what a client that asks for it gets, with and
//! without a certificate configured.
//!
//! The client here speaks the first bytes of the protocol by hand --
//! `SSLRequest`, the one-byte answer, a rustls handshake, then a startup
//! packet and one query -- because that one byte IS the behaviour under
//! test, and a client library hides it behind its own `sslmode` policy.

use std::path::Path;
use std::sync::Arc;

use rustls_pki_types::{CertificateDer, ServerName};
use secantus_pg::{ClientCert, Error, PgServer, TlsConfig};
use tempfile::TempDir;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio_rustls::rustls::crypto::ring::default_provider;
use tokio_rustls::rustls::{ClientConfig, RootCertStore};
use tokio_rustls::TlsConnector;

/// `SSLRequest`: length 8, then the request code 80877103.
const SSL_REQUEST: [u8; 8] = [0, 0, 0, 8, 0x04, 0xd2, 0x16, 0x2f];

struct Certificate {
    dir: TempDir,
    der: CertificateDer<'static>,
}

impl Certificate {
    /// A self-signed certificate for `localhost`, written as the PEM pair the
    /// server reads.
    fn new() -> Certificate {
        let made = rcgen::generate_simple_self_signed(vec!["localhost".to_string()])
            .expect("generate a certificate");
        let dir = TempDir::new().expect("tempdir");
        std::fs::write(dir.path().join("server.crt"), made.cert.pem()).expect("write cert");
        std::fs::write(dir.path().join("server.key"), made.key_pair.serialize_pem())
            .expect("write key");
        Certificate {
            dir,
            der: made.cert.der().clone(),
        }
    }

    fn cert_file(&self) -> std::path::PathBuf {
        self.dir.path().join("server.crt")
    }

    fn key_file(&self) -> std::path::PathBuf {
        self.dir.path().join("server.key")
    }

    /// A connector that trusts this certificate and nothing else.
    fn connector(&self) -> TlsConnector {
        let mut roots = RootCertStore::empty();
        roots.add(self.der.clone()).expect("trust the certificate");
        let config = ClientConfig::builder_with_provider(Arc::new(default_provider()))
            .with_safe_default_protocol_versions()
            .expect("protocol versions")
            .with_root_certificates(roots)
            .with_no_client_auth();
        TlsConnector::from(Arc::new(config))
    }
}

/// Send `SSLRequest` and return the server's one-byte answer.
async fn ssl_request(stream: &mut TcpStream) -> u8 {
    stream.write_all(&SSL_REQUEST).await.expect("SSLRequest");
    let mut answer = [0u8; 1];
    stream.read_exact(&mut answer).await.expect("the answer");
    answer[0]
}

/// One backend message: its type byte and its body.
async fn read_message<S: AsyncRead + Unpin>(stream: &mut S) -> (u8, Vec<u8>) {
    let kind = stream.read_u8().await.expect("message type");
    let len = stream.read_u32().await.expect("message length") as usize;
    let mut body = vec![0u8; len - 4];
    stream.read_exact(&mut body).await.expect("message body");
    (kind, body)
}

/// Read up to the next `ReadyForQuery`, returning the messages before it.
async fn until_ready<S: AsyncRead + Unpin>(stream: &mut S) -> Vec<(u8, Vec<u8>)> {
    let mut seen = Vec::new();
    loop {
        let message = read_message(stream).await;
        if message.0 == b'Z' {
            return seen;
        }
        assert_ne!(message.0, b'E', "{}", String::from_utf8_lossy(&message.1));
        seen.push(message);
    }
}

/// Send the startup packet of a login as `postgres`.
async fn send_startup<S: AsyncWrite + Unpin>(stream: &mut S) {
    send_startup_as(stream, "postgres").await;
}

async fn send_startup_as<S: AsyncWrite + Unpin>(stream: &mut S, user: &str) {
    let mut body = Vec::new();
    body.extend_from_slice(&196_608u32.to_be_bytes());
    body.extend_from_slice(format!("user\0{user}\0database\0postgres\0\0").as_bytes());
    stream
        .write_all(&((body.len() + 4) as u32).to_be_bytes())
        .await
        .expect("startup length");
    stream.write_all(&body).await.expect("startup");
}

/// Log in as `postgres`, with no password.
async fn startup<S: AsyncRead + AsyncWrite + Unpin>(stream: &mut S) {
    send_startup(stream).await;
    until_ready(stream).await;
}

/// Try to log in as `postgres` and return the error the server refuses with.
async fn refusal<S: AsyncRead + AsyncWrite + Unpin>(stream: &mut S) -> String {
    send_startup(stream).await;
    let (kind, body) = read_message(stream).await;
    assert_eq!(kind, b'E', "an ErrorResponse");
    String::from_utf8_lossy(&body).into_owned()
}

/// The single value the last statement of `sql` answers.
async fn query<S: AsyncRead + AsyncWrite + Unpin>(stream: &mut S, sql: &str) -> String {
    let mut message = vec![b'Q'];
    message.extend_from_slice(&((sql.len() + 5) as u32).to_be_bytes());
    message.extend_from_slice(sql.as_bytes());
    message.push(0);
    stream.write_all(&message).await.expect("query");
    let rows: Vec<_> = until_ready(stream)
        .await
        .into_iter()
        .filter(|(kind, _)| *kind == b'D')
        .collect();
    assert_eq!(rows.len(), 1, "one row from {sql}");
    // DataRow: int16 column count, then per column an int32 length and bytes.
    let row = &rows[0].1;
    assert_eq!(u16::from_be_bytes([row[0], row[1]]), 1);
    let len = u32::from_be_bytes([row[2], row[3], row[4], row[5]]) as usize;
    String::from_utf8(row[6..6 + len].to_vec()).expect("utf-8")
}

fn tls_server(cert: &Certificate) -> PgServer {
    PgServer::builder()
        .tls(cert.cert_file(), cert.key_file())
        .start()
        .expect("start with TLS")
}

#[tokio::test(flavor = "multi_thread")]
async fn a_client_that_asks_for_tls_gets_it() {
    let cert = Certificate::new();
    let server = tls_server(&cert);

    let mut tcp = TcpStream::connect(server.address()).await.expect("connect");
    assert_eq!(ssl_request(&mut tcp).await, b'S');
    let name = ServerName::try_from("localhost").expect("server name");
    let mut tls = cert
        .connector()
        .connect(name, tcp)
        .await
        .expect("the handshake, verified against the server's certificate");
    startup(&mut tls).await;
    assert_eq!(query(&mut tls, "show ssl").await, "on");
    assert_eq!(query(&mut tls, "select 40 + 2").await, "42");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_client_that_does_not_ask_is_still_served_in_the_clear() {
    let cert = Certificate::new();
    let server = tls_server(&cert);

    let mut tcp = TcpStream::connect(server.address()).await.expect("connect");
    // `ssl` is the server's setting, not this connection's: it reads `on`.
    startup(&mut tcp).await;
    assert_eq!(query(&mut tcp, "show ssl").await, "on");
    // And it is not a session value a RESET can take back to `off`.
    assert_eq!(query(&mut tcp, "reset all; show ssl").await, "on");
}

#[tokio::test(flavor = "multi_thread")]
async fn without_a_certificate_a_tls_request_is_declined() {
    let server = PgServer::start().expect("start");

    let mut tcp = TcpStream::connect(server.address()).await.expect("connect");
    assert_eq!(ssl_request(&mut tcp).await, b'N');
    // The connection carries on in the clear, as PostgreSQL's does.
    startup(&mut tcp).await;
    assert_eq!(query(&mut tcp, "show ssl").await, "off");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_client_that_rejects_the_certificate_does_not_disturb_the_server() {
    let cert = Certificate::new();
    let server = tls_server(&cert);

    // A client that trusts a DIFFERENT certificate abandons the handshake.
    let stranger = Certificate::new();
    let mut tcp = TcpStream::connect(server.address()).await.expect("connect");
    assert_eq!(ssl_request(&mut tcp).await, b'S');
    let name = ServerName::try_from("localhost").expect("server name");
    assert!(stranger.connector().connect(name, tcp).await.is_err());

    // The next client is served.
    let mut tcp = TcpStream::connect(server.address()).await.expect("connect");
    assert_eq!(ssl_request(&mut tcp).await, b'S');
    let name = ServerName::try_from("localhost").expect("server name");
    let mut tls = cert
        .connector()
        .connect(name, tcp)
        .await
        .expect("handshake");
    startup(&mut tls).await;
    assert_eq!(query(&mut tls, "select 1").await, "1");
}

fn start_error(cert_file: &Path, key_file: &Path) -> String {
    match PgServer::builder().tls(cert_file, key_file).start() {
        Err(Error::Config(message)) => message,
        Err(other) => panic!("expected a configuration error, got {other:?}"),
        Ok(_) => panic!("the server started without the TLS it was asked for"),
    }
}

#[test]
fn a_server_that_cannot_use_its_certificate_does_not_start() {
    let cert = Certificate::new();
    let other = Certificate::new();

    let missing = cert.dir.path().join("absent.crt");
    assert!(start_error(&missing, &cert.key_file()).contains("absent.crt"));
    // A key that belongs to another certificate.
    assert!(start_error(&cert.cert_file(), &other.key_file()).contains("cannot be used together"));
    // The key file where the certificate should be.
    assert!(start_error(&cert.key_file(), &cert.key_file()).contains("holds no certificate"));
}

fn server_with(tls: TlsConfig) -> PgServer {
    PgServer::builder()
        .tls_config(tls)
        .start()
        .expect("start with TLS")
}

#[tokio::test]
async fn a_server_that_requires_tls_refuses_a_connection_in_the_clear() {
    let cert = Certificate::new();
    let server = server_with(TlsConfig::new(cert.cert_file(), cert.key_file()).require_tls(true));

    let mut plain = TcpStream::connect(server.address()).await.expect("connect");
    let message = refusal(&mut plain).await;
    // PostgreSQL's words for a connection no `pg_hba.conf` line matches,
    // when the only lines are `hostssl`.
    assert!(message.contains("C28000"), "{message}");
    assert!(
        message.contains(
            "no pg_hba.conf entry for host \"127.0.0.1\", user \"postgres\", \
             database \"postgres\", no encryption"
        ),
        "{message}"
    );

    let mut stream = TcpStream::connect(server.address()).await.expect("connect");
    assert_eq!(ssl_request(&mut stream).await, b'S');
    let name = ServerName::try_from("localhost").expect("a DNS name");
    let mut tls = cert
        .connector()
        .connect(name, stream)
        .await
        .expect("handshake");
    startup(&mut tls).await;
    assert_eq!(query(&mut tls, "select 1").await, "1");
}

#[tokio::test]
async fn a_required_client_certificate_that_is_not_sent_is_refused() {
    let cert = Certificate::new();
    // The server certificate doubles as the CA here: no client in this test
    // presents anything for it to check.
    let server = server_with(
        TlsConfig::new(cert.cert_file(), cert.key_file())
            .ca_file(cert.cert_file())
            .client_cert(ClientCert::VerifyCa),
    );

    let stream = TcpStream::connect(server.address()).await.expect("connect");
    let mut stream = stream;
    assert_eq!(ssl_request(&mut stream).await, b'S');
    let name = ServerName::try_from("localhost").expect("a DNS name");
    // The handshake completes; it is the login that is refused, with a
    // message the client can read.
    let mut tls = cert
        .connector()
        .connect(name, stream)
        .await
        .expect("handshake");
    let message = refusal(&mut tls).await;
    assert!(
        message.contains("connection requires a valid client certificate"),
        "{message}"
    );

    // And a client certificate is a TLS matter, so the clear is refused too.
    let mut plain = TcpStream::connect(server.address()).await.expect("connect");
    assert!(refusal(&mut plain).await.contains("no encryption"));
}

#[tokio::test]
async fn pg_stat_ssl_shows_each_backends_session() {
    let cert = Certificate::new();
    let server = tls_server(&cert);
    let mine = "from pg_stat_ssl where pid = pg_backend_pid()";

    let mut plain = TcpStream::connect(server.address()).await.expect("connect");
    startup(&mut plain).await;
    assert_eq!(query(&mut plain, &format!("select ssl {mine}")).await, "f");

    let mut stream = TcpStream::connect(server.address()).await.expect("connect");
    assert_eq!(ssl_request(&mut stream).await, b'S');
    let name = ServerName::try_from("localhost").expect("a DNS name");
    let mut tls = cert
        .connector()
        .connect(name, stream)
        .await
        .expect("handshake");
    startup(&mut tls).await;
    assert_eq!(query(&mut tls, &format!("select ssl {mine}")).await, "t");
    assert_eq!(
        query(&mut tls, &format!("select version {mine}")).await,
        "TLSv1.3"
    );
    // OpenSSL's name for the suite, as PostgreSQL reports it.
    let cipher = query(&mut tls, &format!("select cipher {mine}")).await;
    assert!(cipher.starts_with("TLS_"), "{cipher}");
    // One row a backend. (The whole view is not counted: the tests of this
    // file share a process, and so a backend registry.)
    assert_eq!(
        query(&mut tls, &format!("select count(*) {mine}")).await,
        "1"
    );
}

/// The first message a login as `user` is answered with: its type byte and,
/// for an authentication request, the request code.
async fn first_answer(server: &PgServer, user: &str) -> (u8, u32) {
    let mut stream = TcpStream::connect(server.address()).await.expect("connect");
    send_startup_as(&mut stream, user).await;
    let (kind, body) = read_message(&mut stream).await;
    (
        kind,
        u32::from_be_bytes([body[0], body[1], body[2], body[3]]),
    )
}

#[tokio::test]
async fn a_user_there_is_no_role_for_is_refused_unless_the_server_is_told_to_trust_it() {
    // AuthenticationOk is request 0; AuthenticationSASL, the start of a
    // password exchange, is 10.
    let refusing = PgServer::builder().start().expect("start");
    // Asked for a password, as PostgreSQL asks: it does not say there is no
    // such role.
    assert_eq!(first_answer(&refusing, "nobody").await, (b'R', 10));
    // A role with no password is still trusted, `postgres` among them.
    assert_eq!(first_answer(&refusing, "postgres").await, (b'R', 0));

    let trusting = PgServer::builder()
        .trust_unknown_users(true)
        .start()
        .expect("start");
    assert_eq!(first_answer(&trusting, "nobody").await, (b'R', 0));
}

#[test]
fn a_client_certificate_cannot_be_required_without_a_ca_to_check_it() {
    let cert = Certificate::new();
    let tls = TlsConfig::new(cert.cert_file(), cert.key_file()).client_cert(ClientCert::Cert);
    match PgServer::builder().tls_config(tls).start() {
        Err(Error::Config(message)) => assert!(message.contains("CA file"), "{message}"),
        Err(other) => panic!("expected a configuration error, got {other:?}"),
        Ok(_) => panic!("the server started with a requirement it cannot check"),
    }
}
