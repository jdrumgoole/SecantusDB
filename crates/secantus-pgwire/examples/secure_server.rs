use std::io::{Error as IOError, ErrorKind};
use std::sync::Arc;

use rustls_pki_types::pem::PemObject;
use rustls_pki_types::{CertificateDer, PrivateKeyDer};
use tokio::net::TcpListener;
use tokio_rustls::TlsAcceptor;
use tokio_rustls::rustls::ServerConfig;

use pgwire::api::PgWireServerHandlers;
use pgwire::api::auth::StartupHandler;
use pgwire::api::query::SimpleQueryHandler;
use pgwire::tokio::process_socket;

mod common;

fn setup_tls() -> Result<TlsAcceptor, IOError> {
    let cert = CertificateDer::pem_file_iter("examples/ssl/server.crt")
        .map_err(|e| IOError::new(ErrorKind::InvalidData, e))?
        .collect::<Result<Vec<CertificateDer>, _>>()
        .map_err(|e| IOError::new(ErrorKind::InvalidData, e))?;

    let key = PrivateKeyDer::from_pem_file("examples/ssl/server.key")
        .map_err(|e| IOError::new(ErrorKind::InvalidData, e))?;

    let mut config = ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(cert, key)
        .map_err(|err| IOError::new(ErrorKind::InvalidInput, err))?;

    config.alpn_protocols = vec![b"postgresql".to_vec()];

    Ok(TlsAcceptor::from(Arc::new(config)))
}

impl PgWireServerHandlers for common::DummyProcessorFactory {
    fn simple_query_handler(&self) -> Arc<impl SimpleQueryHandler> {
        self.handler.clone()
    }

    fn startup_handler(&self) -> Arc<impl StartupHandler> {
        self.handler.clone()
    }
}

#[tokio::main]
pub async fn main() {
    let factory = Arc::new(common::DummyProcessorFactory::new());

    let server_addr = "127.0.0.1:5433";
    let tls_acceptor = setup_tls().unwrap();
    let listener = TcpListener::bind(server_addr).await.unwrap();

    println!("Listening to {}", server_addr);
    loop {
        let incoming_socket = listener.accept().await.unwrap();
        let tls_acceptor_ref = tls_acceptor.clone();
        let factory_ref = factory.clone();
        tokio::spawn(async move {
            process_socket(incoming_socket.0, Some(tls_acceptor_ref), factory_ref).await
        });
    }
}
