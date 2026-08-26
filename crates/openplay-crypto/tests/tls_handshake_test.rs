//! End-to-end proof that `server_config()` and `client_config_pinned()`
//! interoperate: a real TLS handshake over a loopback socket.

use openplay_crypto::{client_config_pinned, CertificateManager};
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio_rustls::{TlsAcceptor, TlsConnector};

/// Serves exactly one TLS connection, echoing a byte. Returns the bound port.
async fn spawn_server(mgr: &CertificateManager) -> u16 {
    let acceptor = TlsAcceptor::from(mgr.server_config().unwrap());
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();

    tokio::spawn(async move {
        if let Ok((tcp, _)) = listener.accept().await {
            if let Ok(mut tls) = acceptor.accept(tcp).await {
                let _ = tls.write_all(b"k").await;
                let _ = tls.flush().await;
            }
        }
    });

    port
}

async fn try_connect(port: u16, pinned: &str) -> anyhow::Result<u8> {
    let connector = TlsConnector::from(client_config_pinned(pinned)?);
    let tcp = TcpStream::connect(("127.0.0.1", port)).await?;
    let server_name = rustls::pki_types::ServerName::try_from("127.0.0.1")?;
    let mut tls = connector.connect(server_name, tcp).await?;

    let mut buf = [0u8; 1];
    tls.read_exact(&mut buf).await?;
    Ok(buf[0])
}

#[tokio::test]
async fn handshake_succeeds_when_the_fingerprint_matches() {
    let mgr = CertificateManager::generate().unwrap();
    let port = spawn_server(&mgr).await;

    let byte = try_connect(port, mgr.fingerprint())
        .await
        .expect("handshake should succeed against the pinned certificate");
    assert_eq!(byte, b'k');
}

#[tokio::test]
async fn handshake_fails_when_a_different_certificate_is_presented() {
    let receiver = CertificateManager::generate().unwrap();
    let impostor = CertificateManager::generate().unwrap();
    let port = spawn_server(&impostor).await;

    // Sender pinned the real receiver's fingerprint from mDNS; an impostor answers.
    let err = try_connect(port, receiver.fingerprint())
        .await
        .expect_err("handshake must fail when the presented cert is not the pinned one");

    let msg = err.to_string();
    assert!(
        msg.contains("fingerprint mismatch") || msg.contains("certificate"),
        "expected a certificate rejection, got: {msg}"
    );
}

/// A self-signed cert has no CA and the address is a bare IP, so a stock
/// verifier would reject it. This is what makes pinning necessary rather than
/// merely convenient.
#[tokio::test]
async fn the_default_verifier_would_reject_the_same_certificate() {
    let mgr = CertificateManager::generate().unwrap();
    let port = spawn_server(&mgr).await;

    let roots = rustls::RootCertStore::empty();
    let config = rustls::ClientConfig::builder()
        .with_root_certificates(roots)
        .with_no_client_auth();

    let connector = TlsConnector::from(Arc::new(config));
    let tcp = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
    let name = rustls::pki_types::ServerName::try_from("127.0.0.1").unwrap();

    assert!(
        connector.connect(name, tcp).await.is_err(),
        "an empty trust store must reject this self-signed certificate"
    );
}
