//! Integration test for the R5c TLS transport: bind a TLS-enabled Rust server
//! with a self-signed cert (via `rcgen`) over a fake storage, connect a rustls
//! client, and run a `hello` handshake end-to-end over the encrypted channel.
//!
//! Pure WT-free: the server crate links no WiredTiger, so this runs in the
//! `rust` CI job and the dev sandbox.

use std::io::{Read, Write};
use std::net::TcpStream;
use std::sync::Arc;

use bson::{doc, Document};
use secantus_commands::storage::{RawHint, Storage, StorageError, UpdateOutcome};
use secantus_commands::CursorRegistry;
use secantus_server::{bind, ServerConfig, TlsOptions};

/// A do-nothing storage — `hello` never touches it, but `bind` needs one.
struct NoStorage;

impl Storage for NoStorage {
    fn insert(
        &self,
        _db: &str,
        _coll: &str,
        _docs: Vec<Vec<u8>>,
        _ordered: bool,
    ) -> Result<(usize, Vec<Document>), StorageError> {
        Ok((0, Vec::new()))
    }
    fn update_matching(
        &self,
        _db: &str,
        _coll: &str,
        _filter: &Document,
        _update: &Document,
        _multi: bool,
        _upsert: bool,
    ) -> Result<UpdateOutcome, StorageError> {
        Ok(UpdateOutcome::default())
    }
    fn delete_matching(
        &self,
        _db: &str,
        _coll: &str,
        _filter: &Document,
        _limit: usize,
    ) -> Result<usize, StorageError> {
        Ok(0)
    }
    fn count_matching(
        &self,
        _db: &str,
        _coll: &str,
        _filter: &Document,
    ) -> Result<usize, StorageError> {
        Ok(0)
    }
    fn find(
        &self,
        _db: &str,
        _coll: &str,
        _filter: &Document,
        _sort: Option<&Document>,
        _hint: Option<RawHint<'_>>,
    ) -> Result<Vec<Vec<u8>>, StorageError> {
        Ok(Vec::new())
    }
}

/// Write a self-signed cert + key for `localhost` into `dir`, return their paths.
fn write_self_signed(dir: &std::path::Path) -> (String, String) {
    let certified = rcgen::generate_simple_self_signed(vec!["localhost".to_string()]).unwrap();
    let cert_path = dir.join("cert.pem");
    let key_path = dir.join("key.pem");
    std::fs::write(&cert_path, certified.cert.pem()).unwrap();
    std::fs::write(&key_path, certified.key_pair.serialize_pem()).unwrap();
    (
        cert_path.to_str().unwrap().to_string(),
        key_path.to_str().unwrap().to_string(),
    )
}

/// Build a rustls client that trusts `cert_pem`.
fn client_config(cert_pem: &str) -> Arc<rustls::ClientConfig> {
    let mut roots = rustls::RootCertStore::empty();
    let mut reader = std::io::BufReader::new(std::fs::File::open(cert_pem).unwrap());
    for cert in rustls_pemfile::certs(&mut reader) {
        roots.add(cert.unwrap()).unwrap();
    }
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let config = rustls::ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_root_certificates(roots)
        .with_no_client_auth();
    Arc::new(config)
}

/// Build a client OP_MSG request frame carrying `body`.
fn op_msg_request(request_id: i32, body: &Document) -> Vec<u8> {
    let mut body_bytes = Vec::new();
    body.to_writer(&mut body_bytes).unwrap();
    // response_to = 0 for a request; reuse the reply builder (same wire shape).
    secantus_wire::build_op_msg_reply(0, request_id, &body_bytes, 0)
}

/// Read one framed reply from `stream` and return its kind-0 body document.
fn read_op_msg_reply<S: Read>(stream: &mut S) -> Document {
    let mut header = [0u8; secantus_wire::HEADER_SIZE];
    stream.read_exact(&mut header).unwrap();
    let h = secantus_wire::Header::unpack(&header).unwrap();
    let mut body = vec![0u8; h.body_len().unwrap()];
    stream.read_exact(&mut body).unwrap();
    match secantus_wire::parse_body(h.op_code, &body).unwrap() {
        secantus_wire::Op::Msg(msg) => Document::from_reader(&mut &msg.body[..]).unwrap(),
        _ => panic!("expected OP_MSG reply"),
    }
}

#[test]
fn tls_hello_roundtrip() {
    let tmp = std::env::temp_dir().join(format!("secantus-tls-{}", std::process::id()));
    std::fs::create_dir_all(&tmp).unwrap();
    let (cert, key) = write_self_signed(&tmp);

    let config = ServerConfig {
        replica_set_name: None,
        require_auth: false,
        tls: Some(TlsOptions {
            cert_file: cert.clone(),
            key_file: key,
            ca_file: None,
            require_client_cert: false,
        }),
        ..ServerConfig::default()
    };
    let storage: Arc<dyn Storage> = Arc::new(NoStorage);
    let cursors = Arc::new(CursorRegistry::new());
    let mut server = bind("127.0.0.1:0", config, storage, cursors).unwrap();
    let addr = server.address();

    // Connect a rustls client over TCP and run `hello` through the TLS channel.
    let client_cfg = client_config(&cert);
    let server_name = rustls::pki_types::ServerName::try_from("localhost").unwrap();
    let mut conn = rustls::ClientConnection::new(client_cfg, server_name).unwrap();
    let mut sock = TcpStream::connect(addr).unwrap();
    let mut tls = rustls::Stream::new(&mut conn, &mut sock);

    let req = op_msg_request(1, &doc! {"hello": 1, "$db": "admin"});
    tls.write_all(&req).unwrap();
    tls.flush().unwrap();

    let reply = read_op_msg_reply(&mut tls);
    assert_eq!(reply.get_f64("ok").unwrap(), 1.0, "{reply:?}");
    assert!(reply.get_bool("isWritablePrimary").unwrap());

    server.stop();
    let _ = std::fs::remove_dir_all(&tmp);
}
