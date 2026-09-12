//! Stage 4b — the browser bootstrap listener's witnesses.
//!
//! Two surfaces are exercised:
//!
//! - the **router**, driven directly with `tower::ServiceExt::oneshot`,
//!   which is where the protocol decisions live (credential checks,
//!   the typed refusals, CORS, the WebSocket `Origin` gate, and the
//!   offer reaching the anchor's real dialog path); and
//! - the **listener**, started for real with a certificate issued by
//!   a CA the test client trusts, which is the only way to show that
//!   an anchor serves browser-trusted TLS. Nothing here passes an
//!   ignore-certificate flag, because nothing here can: the client
//!   verifies, and the certificate is issued.
//!
//! Run: `cargo test -p net-mesh-sdk --test rtc_bootstrap_listener --features rtc-bootstrap`
#![cfg(feature = "rtc-bootstrap")]

use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use net::adapter::net::rtc::RtcConfig;
use net::adapter::net::{EntityKeypair, MeshNode, MeshNodeConfig};
use net_sdk::bootstrap_credential::{BrowserBootstrapCredential, Psk};
use net_sdk::enrollment::InviteToken;
use net_sdk::identity::Identity;
use net_sdk::rtc_bootstrap::{
    bootstrap_router, serve_bootstrap, AnchorInfo, BootstrapConfig, BootstrapTls, ErrorBody,
    OfferResponse,
};
use tower::ServiceExt as _;

const PSK: [u8; 32] = [0x4Bu8; 32];
const OTHER_PSK: [u8; 32] = [0x77u8; 32];
const ORIGIN: &str = "https://app.example";

fn rtc_config() -> RtcConfig {
    RtcConfig {
        ice_deadline: Duration::from_secs(2),
        ..RtcConfig::new().with_bind_addr("127.0.0.1:0".parse().expect("addr"))
    }
}

async fn anchor() -> Arc<MeshNode> {
    let mut cfg = MeshNodeConfig::new("127.0.0.1:0".parse().expect("addr"), PSK);
    cfg.rtc = Some(rtc_config());
    let node = Arc::new(
        MeshNode::new(EntityKeypair::generate(), cfg)
            .await
            .expect("MeshNode::new"),
    );
    node.start_arc();
    node
}

/// A second native node, used only as an SDP offer generator: it has
/// a real ICE stack, so the offers this test POSTs are real ones.
async fn offerer() -> Arc<MeshNode> {
    let mut cfg = MeshNodeConfig::new("127.0.0.1:0".parse().expect("addr"), PSK);
    cfg.rtc = Some(rtc_config());
    let node = Arc::new(
        MeshNode::new(EntityKeypair::generate(), cfg)
            .await
            .expect("MeshNode::new"),
    );
    node.start_arc();
    node
}

fn credential_for(psk: [u8; 32], invite_ttl: Duration) -> BrowserBootstrapCredential {
    let root = Identity::generate().entity_id().clone();
    let invite = InviteToken::mint(&root, "https://anchor.example", invite_ttl);
    BrowserBootstrapCredential::mint(
        invite,
        [3u8; 32],
        Psk::new(psk),
        "https://anchor.example",
        Duration::from_secs(86_400),
    )
}

fn config(psk: [u8; 32]) -> BootstrapConfig {
    BootstrapConfig::new(
        "127.0.0.1:0".parse().expect("addr"),
        Psk::new(psk),
        BootstrapTls::Operator {
            // Never read on the router path; the TLS witness below
            // uses a real pair.
            cert_pem: "unused".into(),
            key_pem: "unused".into(),
        },
        ORIGIN,
    )
}

fn offer_body(credential: &BrowserBootstrapCredential, node_id: u64, sdp: &str) -> Body {
    Body::from(
        serde_json::json!({
            "credential": credential.encode(),
            "node_id": format!("{node_id:#x}"),
            "sdp": sdp,
        })
        .to_string(),
    )
}

async fn post_offer(
    router: &axum::Router,
    credential: &BrowserBootstrapCredential,
    node_id: u64,
    sdp: &str,
) -> (StatusCode, Vec<u8>) {
    let request = Request::builder()
        .method("POST")
        .uri("/rtc/offer")
        .header(header::CONTENT_TYPE, "application/json")
        .header(header::ORIGIN, ORIGIN)
        .body(offer_body(credential, node_id, sdp))
        .expect("request");
    let response = router
        .clone()
        .into_make_service_with_connect_info::<std::net::SocketAddr>()
        .oneshot("203.0.113.9:5000".parse::<std::net::SocketAddr>().unwrap())
        .await
        .expect("make service")
        .oneshot(request)
        .await
        .expect("response");
    let status = response.status();
    let body = axum::body::to_bytes(response.into_body(), 1 << 20)
        .await
        .expect("body")
        .to_vec();
    (status, body)
}

fn refusal_of(body: &[u8]) -> String {
    let parsed: ErrorBody = serde_json::from_slice(body).expect("an error body");
    serde_json::to_value(parsed.refusal)
        .unwrap()
        .as_str()
        .unwrap()
        .to_string()
}

/// The whole point of the slice: an HTTP-originated offer is an
/// Offer. It reaches the anchor's **production** dialog path — the
/// same `handle_signal` + completion owner a `0x0D02` offer takes —
/// and the anchor answers with a real SDP and holds a dialog against
/// the same per-sender budget.
///
/// Inverse: make `accept_bootstrap_offer` skip `handle_signal` and
/// synthesise an answer — the dialog is not held and the assertion
/// on `open_signal_dialogs` fails.
#[tokio::test(flavor = "multi_thread", worker_threads = 6)]
async fn an_http_offer_reaches_the_production_dialog_path_and_is_answered() {
    let anchor = anchor().await;
    let offerer = offerer().await;
    let router = bootstrap_router(Arc::clone(&anchor), &config(PSK));
    let credential = credential_for(PSK, Duration::from_secs(600));
    let browser_node_id = offerer.node_id();
    let sdp = offerer
        .rtc_driver()
        .expect("driver")
        .create_offer()
        .await
        .expect("offer")
        .1;

    let (status, body) = post_offer(&router, &credential, browser_node_id, &sdp).await;
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&body));
    let answer: OfferResponse = serde_json::from_slice(&body).expect("an offer response");
    assert!(
        answer.sdp.starts_with("v=0"),
        "the anchor's own ICE stack produced the answer: {}",
        &answer.sdp[..answer.sdp.len().min(40)]
    );
    assert!(
        answer.candidate.contains("typ host"),
        "the answer carries the anchor's host candidate for a browser that never \
         opens the trickle socket: {}",
        answer.candidate
    );
    assert_eq!(
        anchor.open_signal_dialogs(browser_node_id),
        1,
        "the dialog is held against the SAME per-sender budget an over-the-mesh \
         offer spends — the listener did not open a second, unbounded path"
    );
}

/// A credential minted for another transport trust domain is refused
/// **before** a dialog exists, and the refusal names the domain, not
/// the lifetime.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_credential_from_another_trust_domain_is_refused_before_any_dialog() {
    let anchor = anchor().await;
    let router = bootstrap_router(Arc::clone(&anchor), &config(PSK));
    let foreign = credential_for(OTHER_PSK, Duration::from_secs(600));

    let (status, body) = post_offer(&router, &foreign, 0x1234, "v=0").await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(refusal_of(&body), "wrong_trust_domain");
    assert_eq!(
        anchor.open_signal_dialogs(0x1234),
        0,
        "nothing was allocated for a credential this anchor does not serve"
    );
}

/// An expired invite nonce is refused, and the refusal is the
/// lifetime one — not "malformed", which would send an operator to
/// the wrong place.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_expired_credential_is_refused_as_expired() {
    let anchor = anchor().await;
    let router = bootstrap_router(Arc::clone(&anchor), &config(PSK));
    let stale = credential_for(PSK, Duration::from_secs(1));
    tokio::time::sleep(Duration::from_millis(1100)).await;

    let (status, body) = post_offer(&router, &stale, 0x99, "v=0").await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(refusal_of(&body), "expired_credential");
}

/// Garbage in the credential field is refused as malformed, and an
/// invite string is not a credential.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_credential_that_is_not_one_is_refused_as_malformed() {
    let anchor = anchor().await;
    let router = bootstrap_router(Arc::clone(&anchor), &config(PSK));
    let root = Identity::generate().entity_id().clone();
    let invite = InviteToken::mint(&root, "rz", Duration::from_secs(600));

    for raw in ["", "net-bootstrap:not-base64!!", &invite.encode()] {
        let request = Request::builder()
            .method("POST")
            .uri("/rtc/offer")
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(
                serde_json::json!({ "credential": raw, "node_id": "1", "sdp": "v=0" }).to_string(),
            ))
            .expect("request");
        let response = router
            .clone()
            .into_make_service_with_connect_info::<std::net::SocketAddr>()
            .oneshot("203.0.113.10:5000".parse::<std::net::SocketAddr>().unwrap())
            .await
            .unwrap()
            .oneshot(request)
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST, "{raw:?}");
    }
}

/// §5's bootstrap budget: a per-source-IP ceiling whose rejection is
/// typed and fast. Every offer costs the anchor an ICE agent, so the
/// refusal must land before the credential is even considered.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_per_source_ip_rate_limit_refuses_typed_and_early() {
    let anchor = anchor().await;
    let mut cfg = config(PSK);
    cfg.offers_per_ip_per_minute = 2;
    let router = bootstrap_router(Arc::clone(&anchor), &cfg);
    // A credential from ANOTHER domain: past the ceiling the answer
    // must be `rate_limited`, proving the limit is checked first.
    let foreign = credential_for(OTHER_PSK, Duration::from_secs(600));

    let mut refusals = Vec::new();
    for _ in 0..3 {
        let (status, body) = post_offer(&router, &foreign, 0x1, "v=0").await;
        refusals.push((status, refusal_of(&body)));
    }
    assert_eq!(refusals[0].1, "wrong_trust_domain");
    assert_eq!(refusals[1].1, "wrong_trust_domain");
    assert_eq!(refusals[2].0, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(refusals[2].1, "rate_limited");
}

/// CORS is an explicit allow-list: the configured origin is echoed,
/// and an unlisted one gets no allow-origin header at all — never a
/// wildcard on an endpoint that takes a credential.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn cors_names_one_origin_and_never_a_wildcard() {
    let anchor = anchor().await;
    let router = bootstrap_router(Arc::clone(&anchor), &config(PSK));

    let preflight = |origin: &'static str| {
        let router = router.clone();
        async move {
            let request = Request::builder()
                .method("OPTIONS")
                .uri("/rtc/offer")
                .header(header::ORIGIN, origin)
                .header(header::ACCESS_CONTROL_REQUEST_METHOD, "POST")
                .body(Body::empty())
                .unwrap();
            router.oneshot(request).await.unwrap()
        }
    };

    let allowed = preflight(ORIGIN).await;
    assert_eq!(
        allowed
            .headers()
            .get(header::ACCESS_CONTROL_ALLOW_ORIGIN)
            .map(|v| v.to_str().unwrap().to_string()),
        Some(ORIGIN.to_string())
    );
    let denied = preflight("https://evil.example").await;
    assert!(
        denied
            .headers()
            .get(header::ACCESS_CONTROL_ALLOW_ORIGIN)
            .is_none(),
        "an unlisted origin gets no allow-origin header"
    );
}

/// The trickle socket validates `Origin` **before** the upgrade: a
/// socket that opens and then closes has already let a foreign
/// origin hold anchor state. Browsers send `Origin` on WebSocket
/// handshakes but do not apply CORS to them, so this is enforced by
/// hand — as a route LAYER, so that an extractor rejection cannot
/// answer a foreign origin before the check runs.
///
/// What the two outcomes mean here: `403` is the layer refusing;
/// `426` is the layer having passed the request through to the
/// upgrade extractor, which cannot complete in a `oneshot` harness
/// because there is no connection to upgrade. So `403` vs `426`
/// discriminates exactly the property under test. A real socket
/// carrying candidates is the Chromium harness's job, not this
/// test's.
///
/// Inverse: drop the layer — the foreign origin reaches the
/// extractor and gets `426` instead of `403`.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_trickle_socket_refuses_a_foreign_origin_before_upgrading() {
    let anchor = anchor().await;
    let router = bootstrap_router(Arc::clone(&anchor), &config(PSK));

    let handshake = |origin: Option<&'static str>| {
        let router = router.clone();
        async move {
            let mut builder = Request::builder()
                .uri("/rtc/trickle?dialog=1&node_id=0x1")
                .header(header::CONNECTION, "upgrade")
                .header(header::UPGRADE, "websocket")
                .header(header::SEC_WEBSOCKET_VERSION, "13")
                .header(header::SEC_WEBSOCKET_KEY, "dGhlIHNhbXBsZSBub25jZQ==");
            if let Some(origin) = origin {
                builder = builder.header(header::ORIGIN, origin);
            }
            let response = router
                .oneshot(builder.body(Body::empty()).unwrap())
                .await
                .unwrap();
            let status = response.status();
            let body = axum::body::to_bytes(response.into_body(), 1 << 16)
                .await
                .unwrap()
                .to_vec();
            (status, body)
        }
    };

    let (status, body) = handshake(Some("https://evil.example")).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(refusal_of(&body), "forbidden_origin");

    // A handshake with no Origin at all is not a browser, and is
    // refused too.
    assert_eq!(handshake(None).await.0, StatusCode::FORBIDDEN);

    // The control: the configured origin is NOT refused — it reaches
    // the upgrade extractor.
    assert_eq!(
        handshake(Some(ORIGIN)).await.0,
        StatusCode::UPGRADE_REQUIRED,
        "the allowed origin passes the layer and reaches the upgrade"
    );
}

/// `GET /rtc/anchor` publishes the LIVE announcement fields so a
/// browser can compare them with the credential it holds. The
/// comparison is the point: a browser that took its pinned key from
/// this response would have no MITM protection at all.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_anchor_endpoint_publishes_the_live_key_for_comparison() {
    let anchor = anchor().await;
    let router = bootstrap_router(Arc::clone(&anchor), &config(PSK));

    let response = router
        .oneshot(
            Request::builder()
                .uri("/rtc/anchor")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = axum::body::to_bytes(response.into_body(), 1 << 16)
        .await
        .unwrap();
    let info: AnchorInfo = serde_json::from_slice(&body).unwrap();

    let live: String = anchor
        .public_key()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    assert_eq!(info.noise_pubkey, live, "the LIVE key, not a stored one");
    assert_eq!(info.trust_domain, Psk::new(PSK).trust_domain().to_string());
    assert_eq!(info.max_provisional, anchor.rtc_max_provisional());

    // And the credential this test mints pins a DIFFERENT key — the
    // browser's pin is the credential's, so a mismatch here is
    // exactly what a browser must be able to detect.
    let credential = credential_for(PSK, Duration::from_secs(600));
    let pinned: String = credential
        .anchor_noise_pubkey
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    assert_ne!(pinned, info.noise_pubkey);
}

/// The ACME HTTP-01 challenge route serves exactly the tokens the
/// ordering client installed, on the same listener. (The ordering
/// half needs a live directory and is a named gap in the report.)
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_acme_challenge_route_serves_installed_tokens_only() {
    let anchor = anchor().await;
    let cfg = config(PSK);
    cfg.acme.set_challenge("tok-1", "tok-1.thumbprint");
    let router = bootstrap_router(Arc::clone(&anchor), &cfg);

    let fetch = |token: &'static str| {
        let router = router.clone();
        async move {
            let response = router
                .oneshot(
                    Request::builder()
                        .uri(format!("/.well-known/acme-challenge/{token}"))
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            let status = response.status();
            let body = axum::body::to_bytes(response.into_body(), 1 << 16)
                .await
                .unwrap();
            (status, String::from_utf8(body.to_vec()).unwrap())
        }
    };

    assert_eq!(
        fetch("tok-1").await,
        (StatusCode::OK, "tok-1.thumbprint".to_string())
    );
    assert_eq!(fetch("tok-2").await.0, StatusCode::NOT_FOUND);
}

/// **Browser-trusted TLS, actually served.** A certificate issued by
/// a CA the client trusts, over a real socket, with a client that
/// verifies — the same shape the Chromium harness uses (its CA goes
/// into the browser's store). No ignore-certificate flag exists on
/// either side.
///
/// Inverse: hand the client an empty root store — the handshake
/// fails, which is what a browser does to a self-signed anchor.
#[tokio::test(flavor = "multi_thread", worker_threads = 6)]
async fn the_listener_serves_browser_trusted_tls_and_a_client_verifies_it() {
    let anchor = anchor().await;
    let dir = tempfile::tempdir().unwrap();
    let (ca_der, cert_pem, key_pem) = issue_localhost_certificate();
    let cert_path = dir.path().join("cert.pem");
    let key_path = dir.path().join("key.pem");
    std::fs::write(&cert_path, cert_pem).unwrap();
    std::fs::write(&key_path, key_pem).unwrap();

    let mut cfg = config(PSK);
    cfg.tls = BootstrapTls::Operator {
        cert_pem: cert_path,
        key_pem: key_path,
    };
    let handle = serve_bootstrap(Arc::clone(&anchor), cfg)
        .await
        .expect("the listener starts");
    let addr = handle.local_addr();

    // A verifying client that trusts our CA and nothing else.
    let mut roots = rustls::RootCertStore::empty();
    roots.add(ca_der.clone()).unwrap();
    let client_config = rustls::ClientConfig::builder_with_provider(Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_safe_default_protocol_versions()
    .unwrap()
    .with_root_certificates(roots)
    .with_no_client_auth();
    let body = https_get(addr, "localhost", client_config.into(), "/rtc/anchor")
        .await
        .expect("the verified request succeeds");
    assert!(
        body.contains("\"noise_pubkey\""),
        "the listener answered over TLS: {body}"
    );

    // The inverse, in the same test because it is the same client:
    // trusting nothing must fail, or the assertion above proves
    // nothing about verification.
    let empty = rustls::ClientConfig::builder_with_provider(Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_safe_default_protocol_versions()
    .unwrap()
    .with_root_certificates(rustls::RootCertStore::empty())
    .with_no_client_auth();
    assert!(
        https_get(addr, "localhost", empty.into(), "/rtc/anchor")
            .await
            .is_err(),
        "a client that trusts no CA must refuse this certificate — that is what a \
         browser does to a self-signed anchor"
    );

    handle.shutdown().await;
}

/// A CA and a leaf for `localhost`, in PEM, plus the CA in DER for
/// the client's root store.
fn issue_localhost_certificate() -> (rustls::pki_types::CertificateDer<'static>, String, String) {
    let mut ca_params = rcgen::CertificateParams::new(Vec::<String>::new()).unwrap();
    ca_params.is_ca = rcgen::IsCa::Ca(rcgen::BasicConstraints::Unconstrained);
    ca_params
        .distinguished_name
        .push(rcgen::DnType::CommonName, "net-mesh test CA");
    let ca_key = rcgen::KeyPair::generate().unwrap();
    let ca = ca_params.clone().self_signed(&ca_key).unwrap();
    let issuer = rcgen::Issuer::new(ca_params, ca_key);

    let mut leaf_params = rcgen::CertificateParams::new(vec!["localhost".to_string()]).unwrap();
    leaf_params
        .distinguished_name
        .push(rcgen::DnType::CommonName, "localhost");
    let leaf_key = rcgen::KeyPair::generate().unwrap();
    let leaf = leaf_params.signed_by(&leaf_key, &issuer).unwrap();

    let chain_pem = format!("{}{}", leaf.pem(), ca.pem());
    (
        rustls::pki_types::CertificateDer::from(ca.der().to_vec()),
        chain_pem,
        leaf_key.serialize_pem(),
    )
}

/// A client that trusts exactly the CA we issued with.
fn client_config_trusting(
    ca: &rustls::pki_types::CertificateDer<'static>,
) -> Arc<rustls::ClientConfig> {
    let mut roots = rustls::RootCertStore::empty();
    roots.add(ca.clone()).unwrap();
    Arc::new(
        rustls::ClientConfig::builder_with_provider(Arc::new(
            rustls::crypto::ring::default_provider(),
        ))
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_root_certificates(roots)
        .with_no_client_auth(),
    )
}

/// A minimal verifying HTTPS GET — enough to prove the TLS layer
/// serves, without pulling an HTTP client into the dev-dependencies.
async fn https_get(
    addr: std::net::SocketAddr,
    server_name: &str,
    config: Arc<rustls::ClientConfig>,
    path: &str,
) -> Result<String, String> {
    https_request(addr, server_name, config, "GET", path, "", "").await
}

/// One hand-written HTTPS request. Hand-written on purpose: the
/// WebSocket witness is about the exact headers a browser sends, and
/// a client library would paper over them.
async fn https_request(
    addr: std::net::SocketAddr,
    server_name: &str,
    config: Arc<rustls::ClientConfig>,
    method: &str,
    path: &str,
    extra_headers: &str,
    body: &str,
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
    // No `Content-Length` on an empty request: hyper rejects a GET
    // that carries both an upgrade and a body framing header.
    let framing = if body.is_empty() {
        String::new()
    } else {
        format!("Content-Length: {}\r\n", body.len())
    };
    let request = format!(
        "{method} {path} HTTP/1.1\r\nHost: {server_name}\r\n{extra_headers}{framing}\r\n{body}"
    );
    tls.write_all(request.as_bytes())
        .await
        .map_err(|e| e.to_string())?;
    // Read what is there: a 101 leaves the socket open, so reading to
    // EOF would hang. One read is enough for a status line + headers.
    let mut buf = vec![0u8; 8192];
    let n = tokio::time::timeout(Duration::from_secs(5), tls.read(&mut buf))
        .await
        .map_err(|_| "timed out reading the response".to_string())?
        .map_err(|e| e.to_string())?;
    Ok(String::from_utf8_lossy(&buf[..n]).to_string())
}
