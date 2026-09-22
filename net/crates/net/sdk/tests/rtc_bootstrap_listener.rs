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
    node.start();
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
    node.start();
    node
}

/// The fixture issuer. Fixed seed so `config()` can name the same
/// public half the credentials are signed with (R3).
fn issuer() -> Identity {
    Identity::from_seed([0x51u8; 32])
}

fn credential_for(psk: [u8; 32], invite_ttl: Duration) -> BrowserBootstrapCredential {
    let root = Identity::generate().entity_id().clone();
    let invite = InviteToken::mint(&root, "https://anchor.example", invite_ttl);
    BrowserBootstrapCredential::mint(
        &issuer(),
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
        issuer().entity_id().clone(),
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

    // The control: the configured origin is not refused *for its
    // origin*. Since R1 it must also present an attempt token, so
    // the allowed origin without one is refused — and the refusal
    // names the token, not the origin, which is how these two
    // independent gates stay distinguishable. The token holder's
    // path to the upgrade is
    // `the_attempt_token_holder_can_trickle_and_abandon_its_own_attempt`.
    let (status, body) = handshake(Some(ORIGIN)).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let message: ErrorBody = serde_json::from_slice(&body).expect("an error body");
    assert!(
        message.message.contains("attempt token"),
        "an allowed origin is refused for the TOKEN, not the origin: {}",
        message.message
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
/// ordering client installed, on the same listener, and **counts the
/// fetches it answers** — the cold-start witness reads that counter
/// to tell "the directory validated against this process" apart from
/// "the directory issued without ever asking".
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

    assert_eq!(cfg.acme.answered_challenges(), 0);
    assert_eq!(
        fetch("tok-1").await,
        (StatusCode::OK, "tok-1.thumbprint".to_string())
    );
    assert_eq!(cfg.acme.answered_challenges(), 1);
    assert_eq!(fetch("tok-2").await.0, StatusCode::NOT_FOUND);
    assert_eq!(
        cfg.acme.answered_challenges(),
        1,
        "a fetch for a token this process never installed is not evidence \
         that the directory reached it"
    );
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

/// One hand-written WebSocket upgrade request — the exact headers a
/// browser sends, including the attempt token presented as the
/// `net-bootstrap-attempt.<token>` subprotocol (R1).
fn ws_upgrade_request(
    addr: std::net::SocketAddr,
    dialog: u64,
    node: u64,
    token: Option<&str>,
) -> String {
    let protocol = token
        .map(|token| format!("Sec-WebSocket-Protocol: net-bootstrap-attempt.{token}\r\n"))
        .unwrap_or_default();
    format!(
        "GET /rtc/trickle?dialog={dialog}&node_id={node} HTTP/1.1\r\n\
         Host: {addr}\r\n\
         Connection: Upgrade\r\n\
         Upgrade: websocket\r\n\
         Sec-WebSocket-Version: 13\r\n\
         Sec-WebSocket-Key: AQEBAQEBAQEBAQEBAQEBAQ==\r\n\
         {protocol}\
         Origin: {ORIGIN}\r\n\r\n"
    )
}

/// Read one response's header block off a raw socket (a 101 leaves
/// the stream open, so this stops at the blank line, not at EOF).
async fn read_http_headers(socket: &mut tokio::net::TcpStream) -> String {
    let mut headers = Vec::new();
    tokio::time::timeout(Duration::from_secs(2), async {
        while !headers.ends_with(b"\r\n\r\n") && headers.len() < 8192 {
            headers.push(socket.read_u8().await.unwrap());
        }
    })
    .await
    .expect("response headers");
    String::from_utf8_lossy(&headers).into_owned()
}

/// One masked client→server WebSocket frame (RFC 6455 §5.3): the
/// hand-rolled client MUST mask, and this witness is about the exact
/// bytes a browser sends. `opcode` is `0x1` text, `0x8` close.
fn ws_client_frame(opcode: u8, payload: &[u8]) -> Vec<u8> {
    const MASK: [u8; 4] = [0x11, 0x22, 0x33, 0x44];
    let mut frame = vec![0x80 | opcode];
    let len = payload.len();
    assert!(len < 65_536, "the probe frames fit the 16-bit length form");
    if len < 126 {
        frame.push(0x80 | len as u8);
    } else {
        frame.push(0x80 | 126);
        frame.extend_from_slice(&(len as u16).to_be_bytes());
    }
    frame.extend_from_slice(&MASK);
    frame.extend(
        payload
            .iter()
            .zip(MASK.iter().cycle())
            .map(|(byte, mask)| byte ^ mask),
    );
    frame
}

// ===================================================================
// Kyra's Stage 4b probes, landed VERBATIM (assertions untouched).
//
// Source: `spikes/kyra/kyra_4b_listener_probes.rs` and
// `spikes/kyra/kyra_4b_candidate_probes.rs`. They reproduced 0/6 at
// `f9ddd2543`; each repair below is written against the probe that
// names it, and the probes stay in this file as the regression.
// ===================================================================

// Parent-authored Stage4B probes. All credentials are ephemeral fixtures.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn kyra_acme_cache_must_match_the_requested_domain() {
    let anchor = kyra_long_lived_anchor().await;
    let dir = tempfile::tempdir().unwrap();
    let (_, cert, key) = issue_localhost_certificate();
    std::fs::write(dir.path().join("bootstrap-cert.pem"), cert).unwrap();
    std::fs::write(dir.path().join("bootstrap-key.pem"), key).unwrap();
    let mut cfg = config(PSK);
    cfg.tls = BootstrapTls::Acme(net_sdk::rtc_bootstrap::AcmeConfig::new(
        "http://127.0.0.1:9/directory",
        "different.example",
        "review@example.invalid",
        dir.path().into(),
    ));
    let result = serve_bootstrap(Arc::clone(&anchor), cfg).await;
    let accepted_wrong_name = result.is_ok();
    if let Ok(handle) = result {
        handle.shutdown().await;
    }
    anchor.shutdown().await.unwrap();
    assert!(
        !accepted_wrong_name,
        "ACME returned a cached localhost certificate for different.example without ordering"
    );
}

use net::adapter::Adapter;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

#[test]
fn kyra_offer_debug_must_not_expose_the_encoded_credential() {
    let credential = credential_for(PSK, Duration::from_secs(600));
    let encoded = credential.encode();
    let request = net_sdk::rtc_bootstrap::OfferRequest {
        credential: encoded.clone(),
        node_id: "7".into(),
        sdp: "v=0".into(),
    };
    let exposed = format!("{request:?}").contains(&encoded);
    // Never print fixture credentials, even when this expected failure fires.
    assert!(
        !exposed,
        "OfferRequest Debug includes the complete PSK-bearing credential"
    );
}

async fn kyra_long_lived_anchor() -> Arc<MeshNode> {
    let mut cfg = MeshNodeConfig::new("127.0.0.1:0".parse().unwrap(), PSK);
    cfg.rtc = Some(RtcConfig {
        ice_deadline: Duration::from_secs(30),
        ..rtc_config()
    });
    let node = Arc::new(MeshNode::new(EntityKeypair::generate(), cfg).await.unwrap());
    node.start();
    node
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn kyra_uncredentialed_websocket_cannot_retire_another_offer() {
    let anchor = kyra_long_lived_anchor().await;
    let offerer = offerer().await;
    let router = bootstrap_router(Arc::clone(&anchor), &config(PSK));
    let credential = credential_for(PSK, Duration::from_secs(600));
    let sdp = offerer
        .rtc_driver()
        .unwrap()
        .create_offer()
        .await
        .unwrap()
        .1;
    let (status, body) = post_offer(&router, &credential, offerer.node_id(), &sdp).await;
    assert_eq!(status, StatusCode::OK);
    let offered: OfferResponse = serde_json::from_slice(&body).unwrap();
    assert_eq!(anchor.open_signal_dialogs(offerer.node_id()), 1);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let serve_router = router.clone();
    let server = tokio::spawn(async move {
        axum::serve(
            listener,
            serve_router.into_make_service_with_connect_info::<std::net::SocketAddr>(),
        )
        .await
        .unwrap();
    });
    let mut socket = tokio::net::TcpStream::connect(addr).await.unwrap();
    // A distinct client: no credential, no session cookie, only a guessed
    // sequential dialog and public/known victim node id. Origin is NOT auth.
    let request = format!("GET /rtc/trickle?dialog={}&node_id={} HTTP/1.1\r\nHost: {}\r\nConnection: Upgrade\r\nUpgrade: websocket\r\nSec-WebSocket-Version: 13\r\nSec-WebSocket-Key: AQEBAQEBAQEBAQEBAQEBAQ==\r\nOrigin: {}\r\n\r\n", offered.dialog, offerer.node_id(), addr, ORIGIN);
    socket.write_all(request.as_bytes()).await.unwrap();
    let mut headers = Vec::new();
    tokio::time::timeout(Duration::from_secs(2), async {
        while !headers.ends_with(b"\r\n\r\n") && headers.len() < 8192 {
            headers.push(socket.read_u8().await.unwrap());
        }
    })
    .await
    .unwrap();
    let upgraded = String::from_utf8_lossy(&headers).starts_with("HTTP/1.1 101");
    if upgraded {
        socket.write_all(&[0x88, 0x80, 1, 2, 3, 4]).await.unwrap();
    }
    let until = tokio::time::Instant::now() + Duration::from_secs(1);
    while anchor.open_signal_dialogs(offerer.node_id()) == 1 && tokio::time::Instant::now() < until
    {
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let retained = anchor.open_signal_dialogs(offerer.node_id());
    eprintln!("kyra_trickle: uncredentialed_upgrade={upgraded} victim_dialogs_after={retained}");
    drop(socket);
    server.abort();
    let _ = server.await;
    anchor.shutdown().await.unwrap();
    offerer.shutdown().await.unwrap();
    assert_eq!(
        retained, 1,
        "a credential-free second client retired the victim's live ICE attempt"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn kyra_expired_credential_holder_cannot_extend_its_own_deadlines() {
    let anchor = kyra_long_lived_anchor().await;
    let offerer = offerer().await;
    let router = bootstrap_router(Arc::clone(&anchor), &config(PSK));
    let mut credential = credential_for(PSK, Duration::from_secs(600));
    credential.invite.expires_at = 1;
    credential.psk_expires_at = 1;
    let sdp = offerer
        .rtc_driver()
        .unwrap()
        .create_offer()
        .await
        .unwrap()
        .1;
    let (before, body) = post_offer(&router, &credential, offerer.node_id(), &sdp).await;
    assert_eq!(before, StatusCode::BAD_REQUEST);
    assert_eq!(refusal_of(&body), "expired_credential");
    // Alter only the two expiry fields; do not possess any issuer key.
    credential.invite.expires_at = u64::MAX;
    credential.psk_expires_at = u64::MAX;
    let (after, _) = post_offer(&router, &credential, offerer.node_id(), &sdp).await;
    eprintln!("kyra_expiry: original_status={before} edited_status={after}");
    anchor.shutdown().await.unwrap();
    offerer.shutdown().await.unwrap();
    assert_ne!(
        after,
        StatusCode::OK,
        "caller-edited lifetimes revive the expired bootstrap credential"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn kyra_bootstrap_candidates_must_keep_the_native_size_bound() {
    use net::adapter::net::rtc::RtcSignalMsg;
    const MAX_SDP_BYTES: usize = 16 * 1024; // pinned source signal.rs:51; native decoder is the control
    let anchor = kyra_long_lived_anchor().await;
    let offerer = offerer().await;
    let sdp = offerer
        .rtc_driver()
        .unwrap()
        .create_offer()
        .await
        .unwrap()
        .1;
    anchor
        .accept_bootstrap_offer(offerer.node_id(), 71, sdp)
        .await
        .unwrap();
    let candidate = offerer.bootstrap_host_candidate().unwrap();
    let mid = "0".repeat(MAX_SDP_BYTES + 1);
    let native = RtcSignalMsg::Candidate {
        dialog: 71,
        candidate: candidate.clone(),
        mid: mid.clone(),
    };
    assert!(
        RtcSignalMsg::from_bytes(&native.to_bytes().unwrap()).is_err(),
        "native decoder must refuse this oversized frame"
    );
    let accepted = anchor
        .apply_bootstrap_candidate(offerer.node_id(), 71, candidate, mid)
        .await
        .is_ok();
    anchor.shutdown().await.unwrap();
    offerer.shutdown().await.unwrap();
    assert!(
        !accepted,
        "bootstrap candidate hook accepted a frame rejected by the native codec size bound"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn kyra_bootstrap_candidates_must_keep_the_native_frame_budget() {
    use net::adapter::net::rtc::{RtcSignalMsg, SignalAdmit, SignalBudget};
    const MAX_FRAMES_PER_WINDOW: u32 = 64; // pinned source signal.rs:41; real SignalBudget is the control
    let anchor = kyra_long_lived_anchor().await;
    let offerer = offerer().await;
    let sdp = offerer
        .rtc_driver()
        .unwrap()
        .create_offer()
        .await
        .unwrap()
        .1;
    anchor
        .accept_bootstrap_offer(offerer.node_id(), 72, sdp)
        .await
        .unwrap();
    let candidate = offerer.bootstrap_host_candidate().unwrap();
    let mut native = SignalBudget::new();
    let now = std::time::Instant::now();
    native.admit(
        offerer.node_id(),
        &RtcSignalMsg::Offer {
            dialog: 72,
            sdp: String::new(),
        },
        now,
    );
    let start = tokio::time::Instant::now();
    let mut bootstrap_ok = 0;
    let mut native_ok = 0;
    let mut native_refused = 0;
    for _ in 0..=MAX_FRAMES_PER_WINDOW {
        let msg = RtcSignalMsg::Candidate {
            dialog: 72,
            candidate: candidate.clone(),
            mid: "0".into(),
        };
        match native.admit(offerer.node_id(), &msg, now) {
            SignalAdmit::Refused(_) => native_refused += 1,
            _ => native_ok += 1,
        }
        if anchor
            .apply_bootstrap_candidate(offerer.node_id(), 72, candidate.clone(), "0".into())
            .await
            .is_ok()
        {
            bootstrap_ok += 1;
        }
    }
    let elapsed = start.elapsed();
    anchor.shutdown().await.unwrap();
    offerer.shutdown().await.unwrap();
    assert!(
        elapsed < Duration::from_secs(10),
        "probe did not fit in one real budget window"
    );
    assert!(
        native_refused > 0,
        "native positive control must reach refusal"
    );
    eprintln!("kyra_candidate_budget: bootstrap_accepted={bootstrap_ok} native_ok={native_ok} native_refused={native_refused} elapsed_ms={}",elapsed.as_millis());
    assert!(
        bootstrap_ok < MAX_FRAMES_PER_WINDOW,
        "bootstrap hook applied every candidate beyond the shared 64-frame limit"
    );
    // The admission SIDE of the same claim: the bootstrap ingress
    // must admit exactly what the native frame budget admits. The
    // upper bound alone passed at `bootstrap_ok == 0` — an
    // over-refusing or unwired `admit_signal_frame` kept this
    // witness green while legitimate trickling was dead.
    assert_eq!(
        bootstrap_ok, native_ok,
        "the bootstrap ingress must admit exactly what the native frame \
         budget admits — one bound, two callers"
    );
}

// ===================================================================
// R1 / R2 — the repairs' own witnesses (the probes above are the
// negatives; these are the controls and the attribution facts).
// ===================================================================

/// R1 positive control: the socket that HOLDS the attempt token
/// upgrades, trickles, and its close retires its own attempt.
///
/// Without this, "a foreign socket cannot retire" could be
/// satisfied by a trickle socket nobody can use.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_attempt_token_holder_can_trickle_and_abandon_its_own_attempt() {
    let anchor = kyra_long_lived_anchor().await;
    let offerer = offerer().await;
    let router = bootstrap_router(Arc::clone(&anchor), &config(PSK));
    let credential = credential_for(PSK, Duration::from_secs(600));
    let sdp = offerer
        .rtc_driver()
        .expect("driver")
        .create_offer()
        .await
        .expect("offer")
        .1;
    let (status, body) = post_offer(&router, &credential, offerer.node_id(), &sdp).await;
    assert_eq!(status, StatusCode::OK);
    let offered: OfferResponse = serde_json::from_slice(&body).expect("offer response");
    assert_eq!(offered.attempt_token.len(), 64, "32 random bytes, hex");
    assert_eq!(anchor.open_signal_dialogs(offerer.node_id()), 1);
    let node = offerer.node_id();
    let key = anchor
        .bootstrap_attempt_key(node, offered.dialog)
        .expect("the attempt's accounting key");

    // The token upgrades where the probe's tokenless socket did not.
    let upgrade = |token: Option<String>, dialog: u64| {
        let router = router.clone();
        let node = offerer.node_id();
        async move {
            let mut builder = Request::builder()
                .uri(format!("/rtc/trickle?dialog={dialog}&node_id={node}"))
                .header(header::ORIGIN, ORIGIN)
                .header(header::CONNECTION, "upgrade")
                .header(header::UPGRADE, "websocket")
                .header(header::SEC_WEBSOCKET_VERSION, "13")
                .header(header::SEC_WEBSOCKET_KEY, "dGhlIHNhbXBsZSBub25jZQ==");
            if let Some(token) = token {
                builder = builder.header(
                    header::SEC_WEBSOCKET_PROTOCOL,
                    format!("net-bootstrap-attempt.{token}"),
                );
            }
            router
                .oneshot(builder.body(Body::empty()).unwrap())
                .await
                .unwrap()
                .status()
        }
    };

    // Every way of not holding the token is refused BEFORE the
    // upgrade; the token holder's own socket drives for real below.
    assert_eq!(upgrade(None, offered.dialog).await, StatusCode::FORBIDDEN);
    assert_eq!(
        upgrade(Some("00".repeat(32)), offered.dialog).await,
        StatusCode::NOT_FOUND,
        "an invented token names no attempt"
    );
    assert_eq!(
        upgrade(Some(offered.attempt_token.clone()), offered.dialog + 1).await,
        StatusCode::NOT_FOUND,
        "a real token for a different dialog is as good as no token"
    );

    // A REAL upgrade over real TCP: the token holder's socket
    // completes the handshake (`101`) — the part the old body never
    // reached, its `oneshot` harness being unable to complete an
    // upgrade.
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let serve_router = router.clone();
    let server = tokio::spawn(async move {
        axum::serve(
            listener,
            serve_router.into_make_service_with_connect_info::<std::net::SocketAddr>(),
        )
        .await
        .unwrap();
    });
    let mut socket = tokio::net::TcpStream::connect(addr).await.unwrap();
    socket
        .write_all(
            ws_upgrade_request(addr, offered.dialog, node, Some(&offered.attempt_token)).as_bytes(),
        )
        .await
        .unwrap();
    let headers = read_http_headers(&mut socket).await;
    assert!(
        headers.starts_with("HTTP/1.1 101"),
        "the token holder's socket upgrades for real: {headers}"
    );

    // TRICKLE: a candidate sent over the socket reaches the shared
    // signalling ingress (`admit_signal_frame`) exactly once — the
    // frame the old body never trickled.
    let delivered = anchor.rtc_driver().unwrap().stats().signal_delivered();
    let candidate = serde_json::json!({
        "type": "candidate",
        "candidate": offerer.bootstrap_host_candidate().unwrap(),
        "mid": "0",
    })
    .to_string();
    socket
        .write_all(&ws_client_frame(0x1, candidate.as_bytes()))
        .await
        .unwrap();
    let until = tokio::time::Instant::now() + Duration::from_secs(2);
    while anchor.rtc_driver().unwrap().stats().signal_delivered() == delivered
        && tokio::time::Instant::now() < until
    {
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(
        anchor.rtc_driver().unwrap().stats().signal_delivered(),
        delivered + 1,
        "the trickled candidate must reach the shared signalling ingress — once"
    );

    // ABANDON: the holder closes its socket, and its close retires
    // its own attempt — the dialog ends, the accounting is released,
    // and the token stops authorizing.
    socket.write_all(&ws_client_frame(0x8, &[])).await.unwrap();
    drop(socket);
    let until = tokio::time::Instant::now() + Duration::from_secs(2);
    while anchor.open_signal_dialogs(node) == 1 && tokio::time::Instant::now() < until {
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(
        anchor.open_signal_dialogs(node),
        0,
        "the holder's close retires its own attempt"
    );
    assert!(
        !anchor.bootstrap_attempt_is_live(node, offered.dialog, key),
        "and releases the attempt's accounting"
    );
    assert_eq!(
        upgrade(Some(offered.attempt_token.clone()), offered.dialog).await,
        StatusCode::NOT_FOUND,
        "a retired attempt's token stops authorizing"
    );

    server.abort();
    let _ = server.await;
    anchor.shutdown().await.expect("shutdown");
    offerer.shutdown().await.expect("shutdown");
}

/// R1, the second repair: a stale trickle socket's delayed close
/// must not end its successor's live attempt. The token RE-UPGRADES
/// — a reconnecting browser reopens the same attempt over a new
/// socket — so "whoever retires the token first" let the old socket's
/// delayed TCP close kill the very attempt its successor was
/// trickling into. Inverse: without the socket generation, the first
/// close below retires the shared token and ends the dialog.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_stale_trickle_socket_cannot_end_its_successors_attempt() {
    let anchor = kyra_long_lived_anchor().await;
    let offerer = offerer().await;
    let router = bootstrap_router(Arc::clone(&anchor), &config(PSK));
    let credential = credential_for(PSK, Duration::from_secs(600));
    let node = offerer.node_id();
    let sdp = offerer
        .rtc_driver()
        .expect("driver")
        .create_offer()
        .await
        .expect("offer")
        .1;
    let (status, body) = post_offer(&router, &credential, node, &sdp).await;
    assert_eq!(status, StatusCode::OK);
    let offered: OfferResponse = serde_json::from_slice(&body).expect("offer response");
    let key = anchor
        .bootstrap_attempt_key(node, offered.dialog)
        .expect("the attempt's accounting key");

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let serve_router = router.clone();
    let server = tokio::spawn(async move {
        axum::serve(
            listener,
            serve_router.into_make_service_with_connect_info::<std::net::SocketAddr>(),
        )
        .await
        .unwrap();
    });

    // The FIRST socket holds the attempt…
    let mut first = tokio::net::TcpStream::connect(addr).await.unwrap();
    first
        .write_all(
            ws_upgrade_request(addr, offered.dialog, node, Some(&offered.attempt_token)).as_bytes(),
        )
        .await
        .unwrap();
    let headers = read_http_headers(&mut first).await;
    assert!(
        headers.starts_with("HTTP/1.1 101"),
        "the first socket upgrades: {headers}"
    );

    // …and the browser's RECONNECT re-upgrades with the same token.
    let mut second = tokio::net::TcpStream::connect(addr).await.unwrap();
    second
        .write_all(
            ws_upgrade_request(addr, offered.dialog, node, Some(&offered.attempt_token)).as_bytes(),
        )
        .await
        .unwrap();
    let headers = read_http_headers(&mut second).await;
    assert!(
        headers.starts_with("HTTP/1.1 101"),
        "the reconnecting socket re-upgrades with the same token: {headers}"
    );

    // The STALE socket goes away first: its close must not reach the
    // successor's attempt.
    first.write_all(&ws_client_frame(0x8, &[])).await.unwrap();
    drop(first);
    tokio::time::sleep(Duration::from_millis(250)).await;
    assert_eq!(
        anchor.open_signal_dialogs(node),
        1,
        "a stale socket's close must not end its successor's live attempt"
    );
    assert!(
        anchor.bootstrap_attempt_is_live(node, offered.dialog, key),
        "the attempt is still live for its successor"
    );

    // The CURRENT socket's close does end it.
    second.write_all(&ws_client_frame(0x8, &[])).await.unwrap();
    drop(second);
    let until = tokio::time::Instant::now() + Duration::from_secs(2);
    while anchor.open_signal_dialogs(node) == 1 && tokio::time::Instant::now() < until {
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(
        anchor.open_signal_dialogs(node),
        0,
        "the current socket's close retires the attempt"
    );
    assert!(!anchor.bootstrap_attempt_is_live(node, offered.dialog, key));

    server.abort();
    let _ = server.await;
    anchor.shutdown().await.expect("shutdown");
    offerer.shutdown().await.expect("shutdown");
}

/// R2 attribution: a credential holder cannot spend another node's
/// signalling budget.
///
/// The listener charges each attempt to the token's own random
/// identity, so an HTTP caller claiming node X leaves X's budget
/// untouched — before the repair the offer was charged to the
/// claimed id, which is a claim.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_http_caller_cannot_spend_the_claimed_nodes_budget() {
    let anchor = kyra_long_lived_anchor().await;
    let victim = offerer().await;
    let router = bootstrap_router(Arc::clone(&anchor), &config(PSK));
    let credential = credential_for(PSK, Duration::from_secs(600));

    // Four offers, all claiming the victim's node id: the per-peer
    // dialog bound is four, so a claim-keyed budget would be full.
    let mut tokens = Vec::new();
    for _ in 0..4 {
        let sdp = victim
            .rtc_driver()
            .expect("driver")
            .create_offer()
            .await
            .expect("offer")
            .1;
        let (status, body) = post_offer(&router, &credential, victim.node_id(), &sdp).await;
        assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&body));
        let offered: OfferResponse = serde_json::from_slice(&body).expect("offer response");
        tokens.push(offered.attempt_token);
    }

    // The victim's OWN budget is untouched: it can still open a
    // native dialog, which is what "another peer cannot spend my
    // allowance" means operationally.
    let native = anchor.admit_signal_frame(
        victim.node_id(),
        &net::adapter::net::rtc::RtcSignalMsg::Offer {
            dialog: 0xBEEF,
            sdp: "v=0".into(),
        },
    );
    assert!(
        native.is_ok(),
        "four HTTP attempts claiming this node consumed its own signalling budget: {native:?}"
    );
    assert_eq!(tokens.len(), 4);

    anchor.shutdown().await.expect("shutdown");
    victim.shutdown().await.expect("shutdown");
}

/// R3 at the listener: a credential this anchor's issuer did not
/// sign is refused, and a freshly issued one is accepted.
///
/// The probe above covers the recipient editing its own deadlines.
/// This covers the other half: a *valid* credential from someone
/// else's issuer is not this anchor's to honour.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_credential_from_another_issuer_is_refused() {
    let anchor = anchor().await;
    let offerer = offerer().await;
    let router = bootstrap_router(Arc::clone(&anchor), &config(PSK));

    // Same PSK, same trust domain, same everything — except the key
    // that signed it.
    let other_issuer = Identity::from_seed([0xEEu8; 32]);
    let root = Identity::generate().entity_id().clone();
    let invite = InviteToken::mint(&root, "https://anchor.example", Duration::from_secs(600));
    let foreign = BrowserBootstrapCredential::mint(
        &other_issuer,
        invite,
        [3u8; 32],
        Psk::new(PSK),
        "https://anchor.example",
        Duration::from_secs(86_400),
    );
    let sdp = offerer
        .rtc_driver()
        .expect("driver")
        .create_offer()
        .await
        .expect("offer")
        .1;

    let (status, body) = post_offer(&router, &foreign, offerer.node_id(), &sdp).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let message: ErrorBody = serde_json::from_slice(&body).expect("an error body");
    assert!(
        message.message.contains("issued by"),
        "the refusal must name the issuer, not the lifetime: {}",
        message.message
    );
    assert_eq!(
        anchor.open_signal_dialogs(offerer.node_id()),
        0,
        "nothing was allocated for a credential this anchor's issuer did not sign"
    );

    // The control: our issuer's credential, same path, accepted.
    let ours = credential_for(PSK, Duration::from_secs(600));
    let sdp = offerer
        .rtc_driver()
        .expect("driver")
        .create_offer()
        .await
        .expect("offer")
        .1;
    let (status, _) = post_offer(&router, &ours, offerer.node_id(), &sdp).await;
    assert_eq!(status, StatusCode::OK);
}

// ===================================================================
// Kyra's second round: the attempt's OWNER, not the token alone
// ===================================================================

/// R1 (round 2): a token whose CORE attempt is gone stops
/// authorizing a socket.
///
/// The review's probe watched the production candidate path report
/// "no core dialog" after expiry while the same token still passed
/// the middleware and reached the upgrade. A token proves who minted
/// it; only the anchor's own row proves the attempt exists. The row
/// is keyed by the accounting identity the acceptance recorded, so
/// this is the exact accepted attempt and not a same-tuple
/// successor.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_attempt_that_ended_on_the_anchor_stops_authorizing_its_token() {
    let anchor = kyra_long_lived_anchor().await;
    let offerer = offerer().await;
    let router = bootstrap_router(Arc::clone(&anchor), &config(PSK));
    let credential = credential_for(PSK, Duration::from_secs(600));
    let sdp = offerer
        .rtc_driver()
        .expect("driver")
        .create_offer()
        .await
        .expect("offer")
        .1;
    let (status, body) = post_offer(&router, &credential, offerer.node_id(), &sdp).await;
    assert_eq!(status, StatusCode::OK);
    let offered: OfferResponse = serde_json::from_slice(&body).expect("offer response");

    let upgrade = |token: String, dialog: u64| {
        let router = router.clone();
        let node = offerer.node_id();
        async move {
            let request = Request::builder()
                .uri(format!("/rtc/trickle?dialog={dialog}&node_id={node}"))
                .header(header::ORIGIN, ORIGIN)
                .header(header::CONNECTION, "upgrade")
                .header(header::UPGRADE, "websocket")
                .header(header::SEC_WEBSOCKET_VERSION, "13")
                .header(header::SEC_WEBSOCKET_KEY, "dGhlIHNhbXBsZSBub25jZQ==")
                .header(
                    header::SEC_WEBSOCKET_PROTOCOL,
                    format!("net-bootstrap-attempt.{token}"),
                );
            router
                .oneshot(request.body(Body::empty()).unwrap())
                .await
                .unwrap()
                .status()
        }
    };

    // While the attempt is live the token reaches the upgrade.
    assert_eq!(
        upgrade(offered.attempt_token.clone(), offered.dialog).await,
        StatusCode::UPGRADE_REQUIRED,
    );
    assert!(
        !anchor.bootstrap_attempt_is_live(offerer.node_id(), offered.dialog, 0),
        "the row is keyed by the accounting identity, not by a guessable zero",
    );

    // The attempt ends on the anchor — the same terminal path expiry
    // and completion take.
    anchor
        .end_bootstrap_dialog(offerer.node_id(), offered.dialog)
        .await;

    assert_eq!(
        upgrade(offered.attempt_token.clone(), offered.dialog).await,
        StatusCode::NOT_FOUND,
        "the token outlived its attempt and must stop authorizing",
    );

    anchor.shutdown().await.expect("shutdown");
    offerer.shutdown().await.expect("shutdown");
}

/// R2 (round 2): the terminal path releases the reservation the
/// ingress actually took.
///
/// The review opened a real socket, sent an owner Close, watched the
/// token retire — and the RANDOM-key reservation still admitted a
/// candidate. Offer/candidate admission charge the attempt's random
/// identity while the core released `(claimed node, dialog)`, so the
/// two never met. The observable here is the budget itself, not a
/// counter: after the attempt ends, a frame charged to the same
/// accounting identity is refused as an unknown dialog.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn ending_an_attempt_releases_the_identity_its_ingress_was_charged_to() {
    let anchor = kyra_long_lived_anchor().await;
    let offerer = offerer().await;
    let router = bootstrap_router(Arc::clone(&anchor), &config(PSK));
    let credential = credential_for(PSK, Duration::from_secs(600));
    let sdp = offerer
        .rtc_driver()
        .expect("driver")
        .create_offer()
        .await
        .expect("offer")
        .1;
    let (status, body) = post_offer(&router, &credential, offerer.node_id(), &sdp).await;
    assert_eq!(status, StatusCode::OK);
    let offered: OfferResponse = serde_json::from_slice(&body).expect("offer response");

    // One live attempt, counted from the owner rows rather than a
    // side counter.
    assert_eq!(anchor.open_signal_dialogs(offerer.node_id()), 1);

    // A candidate on the live attempt is admitted.
    assert!(
        anchor
            .apply_bootstrap_candidate_checked(
                offered_budget_key(&anchor, offerer.node_id(), offered.dialog),
                offerer.node_id(),
                offered.dialog,
                "candidate:1 1 udp 2113937151 192.0.2.9 41234 typ host".into(),
                "0".into(),
            )
            .await
            .is_ok(),
        "the live attempt takes candidates",
    );

    let key = offered_budget_key(&anchor, offerer.node_id(), offered.dialog);
    anchor
        .end_bootstrap_dialog(offerer.node_id(), offered.dialog)
        .await;

    // The reservation is gone: the same accounting identity no longer
    // has that dialog open.
    let refused = anchor.admit_signal_frame(
        key,
        &net::adapter::net::rtc::RtcSignalMsg::Candidate {
            dialog: offered.dialog,
            candidate: "candidate:2 1 udp 2113937151 192.0.2.9 41235 typ host".into(),
            mid: "0".into(),
        },
    );
    assert!(
        refused.is_err(),
        "the random-key reservation outlived the attempt: {refused:?}",
    );
    assert_eq!(
        anchor.open_signal_dialogs(offerer.node_id()),
        0,
        "and the anchor reports no open attempt for that peer",
    );

    anchor.shutdown().await.expect("shutdown");
    offerer.shutdown().await.expect("shutdown");
}

/// The accounting identity the anchor recorded for an accepted
/// attempt, found by asking which key the row answers to.
///
/// The listener hands the browser a token, not the key; a test that
/// wants to charge the same identity has to discover it. Brute force
/// is impossible (64 bits of CSPRNG), so the anchor exposes the
/// liveness question and this walks the one candidate it has: the
/// key recorded at acceptance, read back through the fixtures-only
/// accessor.
fn offered_budget_key(anchor: &Arc<MeshNode>, node_id: u64, dialog: u64) -> u64 {
    anchor
        .bootstrap_attempt_key(node_id, dialog)
        .expect("the anchor recorded an owner for this attempt")
}

/// R4 (round two): a failed startup does not keep its challenge
/// port.
///
/// The ingress is bound BEFORE ordering, which is what makes a cold
/// start possible — but the handle was dropped when the order
/// failed, and dropping a `JoinHandle` DETACHES the task. The review
/// watched the port stay bound for the life of the process. The
/// observable is the port itself: after the failure, it binds.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_failed_acme_startup_releases_its_challenge_port() {
    let anchor = kyra_long_lived_anchor().await;
    // A port nobody else holds, released before the listener claims
    // it. Binding it again at the end is the whole verdict.
    let probe = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("probe bind");
    let challenge_addr = probe.local_addr().expect("probe addr");
    drop(probe);

    let cache = tempfile::tempdir().expect("cache dir");
    let mut config = config(PSK);
    // A directory that refuses connections: ordering fails, and it
    // fails AFTER the challenge ingress is up.
    config.tls = BootstrapTls::Acme(net_sdk::rtc_bootstrap::AcmeConfig::new(
        "http://127.0.0.1:1/directory",
        "localhost",
        "operator@example.invalid",
        cache.path().into(),
    ));
    config.acme_challenge_addr = challenge_addr;

    let started = serve_bootstrap(Arc::clone(&anchor), config).await;
    assert!(
        started.is_err(),
        "the premise: a directory on a closed port cannot issue",
    );

    let rebound = tokio::net::TcpListener::bind(challenge_addr).await;
    assert!(
        rebound.is_ok(),
        "the failed startup left {challenge_addr} bound: {:?}",
        rebound.err(),
    );

    anchor.shutdown().await.expect("shutdown");
}

/// Stage 5 (found by the browser harness's UDP-blocked control leg):
/// a client whose attempt FAILED must be able to start another one.
///
/// The browser gets a typed failure, abandons the attempt, and
/// immediately reconnects — which is exactly what a page does after
/// `RtcError::UdpBlocked`. The second attempt's trickle socket was
/// being refused, which the browser reports as a bare 1006.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_client_whose_attempt_was_abandoned_can_open_another_one() {
    let anchor = kyra_long_lived_anchor().await;
    let offerer = offerer().await;
    let router = bootstrap_router(Arc::clone(&anchor), &config(PSK));
    let credential = credential_for(PSK, Duration::from_secs(600));

    let upgrade = |token: String, dialog: u64| {
        let router = router.clone();
        let node = offerer.node_id();
        async move {
            let request = Request::builder()
                .uri(format!("/rtc/trickle?dialog={dialog}&node_id={node}"))
                .header(header::ORIGIN, ORIGIN)
                .header(header::CONNECTION, "upgrade")
                .header(header::UPGRADE, "websocket")
                .header(header::SEC_WEBSOCKET_VERSION, "13")
                .header(header::SEC_WEBSOCKET_KEY, "dGhlIHNhbXBsZSBub25jZQ==")
                .header(
                    header::SEC_WEBSOCKET_PROTOCOL,
                    format!("net-bootstrap-attempt.{token}"),
                );
            router
                .oneshot(request.body(Body::empty()).unwrap())
                .await
                .unwrap()
                .status()
        }
    };

    let mut statuses = Vec::new();
    for _ in 0..3 {
        let sdp = offerer
            .rtc_driver()
            .expect("driver")
            .create_offer()
            .await
            .expect("offer")
            .1;
        let (status, body) = post_offer(&router, &credential, offerer.node_id(), &sdp).await;
        assert_eq!(status, StatusCode::OK, "each offer is accepted");
        let offered: OfferResponse = serde_json::from_slice(&body).expect("offer response");
        statuses.push(upgrade(offered.attempt_token.clone(), offered.dialog).await);
        // The page abandons it, exactly as `LeafNode::connect` now
        // does on a post-offer failure.
        anchor
            .end_bootstrap_dialog(offerer.node_id(), offered.dialog)
            .await;
    }

    assert!(
        statuses.iter().all(|s| *s == StatusCode::UPGRADE_REQUIRED),
        "every attempt's own token must reach its upgrade; got {statuses:?}",
    );

    anchor.shutdown().await.expect("shutdown");
    offerer.shutdown().await.expect("shutdown");
}

/// Stage 5 (the browser harness's UDP-blocked CONTROL leg, and two
/// Stage 4b witnesses on Linux CI): an attempt whose DataChannel has
/// just opened is still IN FLIGHT — its own trickle socket must
/// still be able to upgrade, and the candidates it is still
/// trickling must still be admitted.
///
/// # What actually broke
///
/// The anchor's host candidate rides the offer BODY — str0m adds the
/// local candidate in `new_session` before it answers — so on a fast
/// path (loopback, or a warm LAN) ICE completes before the trickle
/// socket's own fresh TCP+TLS handshake lands. The completion owner
/// released the attempt's reservation at channel-open, the R1 layer
/// then found no live attempt and answered **HTTP 404**, and a
/// browser reports a refused WebSocket HANDSHAKE as a bare `1006`
/// with no code and no reason. Measured in the harness:
///
/// ```text
/// [cand mdns-loopback/no-stun] iceConnectionState=connected
/// [trickle mdns-loopback/no-stun] closed code=1006 clean=false
/// [main] error: WebSocket connection to 'wss://…/rtc/trickle?dialog=3…'
///        failed: Error during WebSocket handshake: Unexpected response code: 404
/// ```
///
/// The same merge, seen from the socket that DID upgrade: every
/// candidate trickled after the channel opened became "signalling
/// frame for an unknown dialog", the listener closed the socket with
/// a typed `4404`, and the handshake riding that DataChannel died —
/// `browser_enrollment_survives_replacement` and
/// `mitm_anchor_fails_the_handshake_and_installs_nothing` both
/// failed on `timeout: noise msg2` behind exactly that close.
///
/// "The accounting slot may be given back" and "this attempt is
/// over" are different facts. The retirement now happens when the
/// attempt is terminal — and a completed install is terminal, which
/// is the last thing asserted here.
///
/// # Why this level
///
/// The router-level sequence (offer, upgrade, `end_bootstrap_dialog`,
/// repeat) passes both before and after the repair: it never opens a
/// DataChannel, so it never reaches the completion owner, which is
/// where the two meanings were merged. This drives the REAL path —
/// a real offer, the real answer, real ICE over loopback, the real
/// completion owner — and uses the client's raw driver rather than
/// its dialog engine so that nothing sends Noise `msg1`: the
/// anchor's owner then parks in `accept_rtc` for the whole
/// `ice_deadline`, which makes "the channel is open and the install
/// has not completed" a window a test can stand in rather than a
/// race it has to win.
///
/// Inverse: move `release_signal_budget` back above the install in
/// `spawn_dialog_completion` — the late candidate is refused as an
/// unknown dialog and the upgrade answers 404.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_attempt_whose_channel_just_opened_is_not_over_yet() {
    let anchor = kyra_long_lived_anchor().await;
    let client = offerer().await;
    let router = bootstrap_router(Arc::clone(&anchor), &config(PSK));
    let credential = credential_for(PSK, Duration::from_secs(600));
    let driver = client.rtc_driver().expect("driver");

    // A real offer from a real ICE stack, through the real route.
    let (peer, sdp) = driver.create_offer().await.expect("offer");
    let (status, body) = post_offer(&router, &credential, client.node_id(), &sdp).await;
    assert_eq!(status, StatusCode::OK);
    let offered: OfferResponse = serde_json::from_slice(&body).expect("offer response");
    let node_id = client.node_id();
    let key = offered_budget_key(&anchor, node_id, offered.dialog);

    let upgrade = |token: String, dialog: u64| {
        let router = router.clone();
        async move {
            let request = Request::builder()
                .uri(format!("/rtc/trickle?dialog={dialog}&node_id={node_id}"))
                .header(header::ORIGIN, ORIGIN)
                .header(header::CONNECTION, "upgrade")
                .header(header::UPGRADE, "websocket")
                .header(header::SEC_WEBSOCKET_VERSION, "13")
                .header(header::SEC_WEBSOCKET_KEY, "dGhlIHNhbXBsZSBub25jZQ==")
                .header(
                    header::SEC_WEBSOCKET_PROTOCOL,
                    format!("net-bootstrap-attempt.{token}"),
                );
            router
                .oneshot(request.body(Body::empty()).unwrap())
                .await
                .unwrap()
                .status()
        }
    };

    // Both halves of the exchange the browser's page does: the
    // answer, the anchor's candidate out of the offer body, and this
    // side's host candidate through the same checked ingress the
    // trickle socket uses.
    driver
        .accept_answer(peer, offered.sdp.clone())
        .await
        .expect("answer");
    driver
        .remote_candidate(peer, offered.candidate.clone())
        .await
        .expect("the anchor's candidate");
    let ours = client
        .bootstrap_host_candidate()
        .expect("this side's host candidate");
    anchor
        .apply_bootstrap_candidate_checked(key, node_id, offered.dialog, ours, "0".into())
        .await
        .expect("the live attempt takes candidates");

    driver
        .await_open(peer)
        .await
        .expect("the DataChannel opens");

    // **The barrier.** The completion owner's first act after the
    // channel opens is to take the dialog out of the EXPIRY table
    // (R4-A), which it does either way — so this is the one
    // observable that says "the window under test has been entered"
    // without being the thing under test. Without it this test could
    // pass by asking its question too early.
    let mut owner_ran = false;
    for _ in 0..80 {
        if !anchor.holds_bootstrap_dialog(node_id, offered.dialog).await {
            owner_ran = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    assert!(
        owner_ran,
        "the completion owner never reached its channel-open step, so the \
         window under test was never entered",
    );

    // A browser goes on trickling while it gathers, and the engine
    // has no row left to place the candidate in. That is LATE, not
    // unknown: refusing it closed the trickle socket with a typed
    // 4404 in the middle of Noise, and the anchor's own log carried
    // `signalling frame for an unknown dialog` while the handshake
    // died on `timeout: noise msg2`.
    anchor
        .apply_bootstrap_candidate_checked(
            key,
            node_id,
            offered.dialog,
            "candidate:9 1 udp 2113937151 192.0.2.9 41299 typ host".into(),
            "0".into(),
        )
        .await
        .expect(
            "a candidate trickled after the channel opened must be admitted and \
             dropped, not refused as an unknown dialog",
        );

    // THE REGRESSION. The channel is open, the install is parked
    // waiting for a `msg1` nothing will send, and the attempt is
    // still in flight — so its own token must still reach the
    // upgrade. This answered 404 before the repair.
    assert!(
        anchor.bootstrap_attempt_is_live(node_id, offered.dialog, key),
        "an attempt whose channel opened is still in flight",
    );
    assert_eq!(
        upgrade(offered.attempt_token.clone(), offered.dialog).await,
        StatusCode::UPGRADE_REQUIRED,
        "the trickle socket lost a race with ICE and was refused; a browser \
         can only read that as a bare 1006",
    );

    // And the other meaning, unchanged: a COMPLETED install is
    // terminal. The client runs the Noise half its own completion
    // owner would, the anchor's parked `accept_rtc` finishes, and
    // the token stops authorizing.
    client
        .connect_rtc(peer, anchor.public_key(), anchor.node_id())
        .await
        .expect("Noise over the DataChannel");
    let mut retired = StatusCode::UPGRADE_REQUIRED;
    for _ in 0..80 {
        retired = upgrade(offered.attempt_token.clone(), offered.dialog).await;
        if retired == StatusCode::NOT_FOUND {
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    assert_eq!(
        retired,
        StatusCode::NOT_FOUND,
        "an installed session owns the peer; the attempt is over and its token \
         must stop authorizing",
    );

    anchor.shutdown().await.expect("shutdown");
    client.shutdown().await.expect("shutdown");
}
