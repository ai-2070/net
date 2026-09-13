//! R4a — a **cold-cache ACME order** against a local directory,
//! followed by a verifying TLS client.
//!
//! Gated on `acme-pebble` and on the `PEBBLE_*` environment being
//! set, because it needs a real ACME server:
//! `.github/workflows/ci.yml`'s `acme-cold-start` job runs
//! [pebble](https://github.com/letsencrypt/pebble) **on the host
//! network** and points this at it. There is no mock — the defect
//! this covers is that `serve_bootstrap` used to wait for a
//! certificate before binding anything, so on a cold cache HTTP-01
//! could never be answered and the order could never complete.
//!
//! Four things have to line up, and the test asserts all four
//! rather than settling for "the call returned Ok":
//!
//! 1. the ACME client trusts the directory's own CA
//!    (`PEBBLE_MINICA_PEM` -> `AcmeConfig::directory_roots`), which
//!    is what the hosted run used to fail on;
//! 2. the directory's validation authority actually reaches back to
//!    the challenge ingress, observed as an answered challenge
//!    fetch rather than inferred from issuance;
//! 3. the issued pair lands in the per-domain cache, covers the
//!    domain by SAN, and is live right now; and
//! 4. a TLS client that verifies against the issuing root
//!    (`PEBBLE_ROOT_PEM`) completes a handshake with the bootstrap
//!    listener. Nothing on either side ignores certificate errors.
//!
//! Run: `cargo test -p net-mesh-sdk --features "rtc-bootstrap acme-pebble" --test acme_cold_start`
#![cfg(all(feature = "rtc-bootstrap", feature = "acme-pebble"))]

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use net::adapter::net::rtc::RtcConfig;
use net::adapter::net::{EntityKeypair, MeshNode, MeshNodeConfig};
use net_sdk::bootstrap_credential::Psk;
use net_sdk::identity::Identity;
use net_sdk::rtc_bootstrap::{serve_bootstrap, AcmeConfig, BootstrapConfig, BootstrapTls};

const PSK: [u8; 32] = [0x7Au8; 32];

/// The domain pebble is configured to resolve back to this host, and
/// the port its HTTP-01 validation dials.
///
/// The CI job runs pebble with **host networking** precisely so this
/// name means the same interface on both sides: pebble's validation
/// authority resolves it in the same network namespace the challenge
/// ingress below binds in.
fn domain() -> String {
    std::env::var("PEBBLE_DOMAIN").unwrap_or_else(|_| "localhost".to_string())
}

fn challenge_addr() -> SocketAddr {
    std::env::var("PEBBLE_HTTP01_ADDR")
        .unwrap_or_else(|_| "0.0.0.0:5002".to_string())
        .parse()
        .expect("PEBBLE_HTTP01_ADDR")
}

/// The CA that signed the **directory's own** serving certificate —
/// pebble's `test/certs/pebble.minica.pem`, extracted from the same
/// image the job runs. Nothing else trusts it; see
/// [`AcmeConfig::directory_roots`].
fn directory_roots() -> Vec<PathBuf> {
    vec![PathBuf::from(std::env::var("PEBBLE_MINICA_PEM").expect(
        "PEBBLE_MINICA_PEM must point at the CA that signed the ACME directory's certificate",
    ))]
}

fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock")
        .as_secs()
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
    )
    // Pebble serves its directory over a certificate signed by its
    // own test CA. Trusting that CA *for this connection only* is
    // what makes the handshake succeed without any verifier being
    // relaxed — the previous shape of this job died at
    // `client error (Connect)` with pebble logging `unknown
    // certificate authority`.
    .with_directory_roots(directory_roots());
    let mut cfg = BootstrapConfig::new(
        "0.0.0.0:0".parse().expect("addr"),
        Psk::new(PSK),
        issuer.entity_id().clone(),
        BootstrapTls::Acme(acme),
        format!("https://{}", domain()),
    );
    cfg.acme_challenge_addr = challenge_addr();
    // The same store the ordering client writes into and the ingress
    // reads from: kept so the fetch count survives `cfg` being moved.
    let challenges = cfg.acme.clone();

    // The cache is empty: this call must place a real order, answer
    // its own HTTP-01 challenge, and only then bind TLS.
    let handle = serve_bootstrap(Arc::clone(&anchor), cfg)
        .await
        .expect("a cold-cache order completes and the listener binds");
    let addr = handle.local_addr();

    // **The directory actually came back over the reverse path.**
    // Issuance alone does not prove this: a directory told to
    // short-circuit validation issues without ever dialling the
    // ingress, which is exactly the hole a green job would have hidden
    // while the container could not reach the host at all.
    assert!(
        challenges.answered_challenges() >= 1,
        "the HTTP-01 challenge route was fetched and answered at least once, \
         but the counter is {} — the directory issued without validating, or \
         it could not reach {}",
        challenges.answered_challenges(),
        challenge_addr()
    );

    // The certificate landed in the PER-DOMAIN cache (R4b), covers the
    // domain by SAN, and is inside its validity window right now —
    // `read_cache`/`qualify_leaf` enforce all three, and this asserts
    // the issued material really satisfies them rather than trusting
    // that the startup path happened to.
    let domain_dir = cache.path().join(domain());
    let cert_path = domain_dir.join("bootstrap-cert.pem");
    assert!(
        cert_path.exists(),
        "the issued certificate is cached under its own domain"
    );
    assert!(
        domain_dir.join("bootstrap-key.pem").exists(),
        "its private key is cached beside it"
    );
    let cached_pem = std::fs::read(&cert_path).expect("read the cached certificate");
    let leaf = rustls_pemfile::certs(&mut cached_pem.as_slice())
        .next()
        .expect("the cache holds a leaf")
        .expect("the leaf parses");
    {
        use x509_parser::prelude::*;

        let (_, parsed) = X509Certificate::from_der(leaf.as_ref()).expect("the leaf is DER");
        let names: Vec<String> = parsed
            .subject_alternative_name()
            .ok()
            .flatten()
            .map(|san| {
                san.value
                    .general_names
                    .iter()
                    .filter_map(|name| match name {
                        GeneralName::DNSName(dns) => Some((*dns).to_string()),
                        _ => None,
                    })
                    .collect()
            })
            .unwrap_or_default();
        assert!(
            names.iter().any(|n| n.eq_ignore_ascii_case(&domain())),
            "the cached certificate covers {} by DNS SAN, got {names:?}",
            domain()
        );
        let not_before = parsed.validity().not_before.timestamp().max(0) as u64;
        let not_after = parsed.validity().not_after.timestamp().max(0) as u64;
        let now = now_unix();
        assert!(
            not_before <= now && now < not_after,
            "the cached certificate is live right now: not_before={not_before} \
             now={now} not_after={not_after}"
        );
    }

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
