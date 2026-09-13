//! R4a — a **cold-cache ACME order** against a local directory,
//! followed by a verifying TLS client.
//!
//! Gated on `acme-pebble` and on `PEBBLE_DIRECTORY_URL` being set,
//! because it needs a real ACME server: CI runs
//! [pebble](https://github.com/letsencrypt/pebble) as a service
//! container and points this at it. There is no mock — the defect
//! this covers is that `serve_bootstrap` used to wait for a
//! certificate before binding anything, so on a cold cache HTTP-01
//! could never be answered and the order could never complete.
//!
//! Run: `cargo test -p net-mesh-sdk --features "rtc-bootstrap acme-pebble" --test acme_cold_start`
#![cfg(all(feature = "rtc-bootstrap", feature = "acme-pebble"))]

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use net::adapter::net::rtc::RtcConfig;
use net::adapter::net::{EntityKeypair, MeshNode, MeshNodeConfig};
use net_sdk::bootstrap_credential::Psk;
use net_sdk::identity::Identity;
use net_sdk::rtc_bootstrap::{serve_bootstrap, AcmeConfig, BootstrapConfig, BootstrapTls};

const PSK: [u8; 32] = [0x7Au8; 32];

/// The domain pebble is configured to resolve back to this host, and
/// the port its HTTP-01 validation dials.
fn domain() -> String {
    std::env::var("PEBBLE_DOMAIN").unwrap_or_else(|_| "anchor.test".to_string())
}

fn challenge_addr() -> SocketAddr {
    std::env::var("PEBBLE_HTTP01_ADDR")
        .unwrap_or_else(|_| "0.0.0.0:5002".to_string())
        .parse()
        .expect("PEBBLE_HTTP01_ADDR")
}

async fn anchor() -> Arc<MeshNode> {
    let mut cfg = MeshNodeConfig::new("127.0.0.1:0".parse().expect("addr"), PSK);
    cfg.rtc = Some(RtcConfig::new().with_bind_addr("127.0.0.1:0".parse().expect("addr")));
    let node = Arc::new(
        MeshNode::new(EntityKeypair::generate(), cfg)
            .await
            .expect("MeshNode::new"),
    );
    node.start_arc();
    node
}

/// The whole cold-start path: empty cache, order placed, HTTP-01
/// answered by the ingress this listener bound BEFORE ordering, TLS
/// bound with the issued certificate, and a client that verifies
/// against pebble's own root fetches `/rtc/anchor`.
#[tokio::test(flavor = "multi_thread", worker_threads = 6)]
async fn a_cold_cache_orders_a_certificate_and_then_serves_tls() {
    let directory_url = std::env::var("PEBBLE_DIRECTORY_URL")
        .expect("PEBBLE_DIRECTORY_URL must point at a local ACME directory");
    let cache = tempfile::tempdir().expect("cache dir");
    let anchor = anchor().await;
    let issuer = Identity::generate();

    let acme = AcmeConfig::new(
        directory_url,
        domain(),
        "operator@example.invalid",
        cache.path().to_path_buf(),
    );
    let mut cfg = BootstrapConfig::new(
        "0.0.0.0:0".parse().expect("addr"),
        Psk::new(PSK),
        issuer.entity_id().clone(),
        BootstrapTls::Acme(acme),
        format!("https://{}", domain()),
    );
    cfg.acme_challenge_addr = challenge_addr();

    // The cache is empty: this call must place a real order, answer
    // its own HTTP-01 challenge, and only then bind TLS.
    let handle = serve_bootstrap(Arc::clone(&anchor), cfg)
        .await
        .expect("a cold-cache order completes and the listener binds");
    let addr = handle.local_addr();

    // The certificate landed in the PER-DOMAIN cache (R4b).
    let domain_dir = cache.path().join(domain());
    assert!(
        domain_dir.join("bootstrap-cert.pem").exists(),
        "the issued certificate is cached under its own domain"
    );

    // And a verifying client can use it. Pebble's root is trusted
    // through `PEBBLE_ROOT_PEM`; nothing here ignores certificate
    // errors.
    let roots_pem = std::fs::read(std::env::var("PEBBLE_ROOT_PEM").expect("PEBBLE_ROOT_PEM"))
        .expect("pebble root");
    let mut roots = rustls::RootCertStore::empty();
    for cert in rustls_pemfile::certs(&mut roots_pem.as_slice()) {
        roots.add(cert.expect("root cert")).expect("add root");
    }
    let client = Arc::new(
        rustls::ClientConfig::builder_with_provider(Arc::new(
            rustls::crypto::ring::default_provider(),
        ))
        .with_safe_default_protocol_versions()
        .expect("versions")
        .with_root_certificates(roots)
        .with_no_client_auth(),
    );
    let body = tokio::time::timeout(
        Duration::from_secs(30),
        https_get(addr, &domain(), client, "/rtc/anchor"),
    )
    .await
    .expect("the request finishes")
    .expect("the verified request succeeds");
    assert!(
        body.contains("noise_pubkey"),
        "the anchor answered over the ACME-issued certificate: {body}"
    );

    handle.shutdown().await;
}

async fn https_get(
    addr: SocketAddr,
    server_name: &str,
    config: Arc<rustls::ClientConfig>,
    path: &str,
) -> Result<String, String> {
    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

    let connector = tokio_rustls::TlsConnector::from(config);
    let stream = tokio::net::TcpStream::connect(addr)
        .await
        .map_err(|e| e.to_string())?;
    let name = rustls::pki_types::ServerName::try_from(server_name.to_string())
        .map_err(|e| e.to_string())?;
    let mut tls = connector
        .connect(name, stream)
        .await
        .map_err(|e| e.to_string())?;
    let request =
        format!("GET {path} HTTP/1.1\r\nHost: {server_name}\r\nConnection: close\r\n\r\n");
    tls.write_all(request.as_bytes())
        .await
        .map_err(|e| e.to_string())?;
    let mut response = Vec::new();
    tls.read_to_end(&mut response)
        .await
        .map_err(|e| e.to_string())?;
    Ok(String::from_utf8_lossy(&response).to_string())
}
