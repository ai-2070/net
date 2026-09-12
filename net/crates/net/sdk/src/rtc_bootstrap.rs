//! The **browser bootstrap listener** (plan §5 Layer 0, Stage 4
//! bullet 3; Stage 4b).
//!
//! A browser cannot send a Net packet, cannot join the mesh, and
//! cannot be handed a UDP socket. Its first contact is therefore
//! ordinary HTTPS against an anchor:
//!
//! ```text
//! POST /rtc/offer          credential + claimed node id + SDP offer  ->  SDP answer
//! GET  /rtc/trickle?…      upgraded to a WebSocket; candidates both ways
//! GET  /rtc/anchor         the announcement fields, so the browser can
//!                          check the credential's pinned key against
//!                          the live anchor
//! ```
//!
//! # What this module does NOT do
//!
//! It does not decide admission, it does not promote anything, and
//! it does not implement a second dialog engine. An HTTP-originated
//! Offer **is an Offer**: it goes into the same
//! [`MeshNode::accept_bootstrap_offer`] → `handle_signal` →
//! `spawn_dialog_completion` path a `0x0D02` offer takes, and the
//! session that results is Provisional under the §12 contract Stage
//! 4a landed, exactly like every other browser-facing session. The
//! only differences are the two effects a browser cannot receive
//! over the mesh: the answer rides the HTTP response, and the
//! anchor's candidate rides the trickle socket.
//!
//! # Trust
//!
//! - The **credential** ([`crate::bootstrap_credential`]) is checked
//!   before a dialog is created: both lifetimes, and the trust
//!   domain against the PSK this anchor actually holds. A credential
//!   for another transport trust domain is refused here rather than
//!   failing later inside a handshake.
//! - The **claimed node id** in the offer body is a claim, and is
//!   treated as one: it rides into the Noise prologue, so a browser
//!   that claims an id it cannot handshake as fails the handshake.
//! - The **anchor's Noise key** the browser pins comes from the
//!   credential. `GET /rtc/anchor` publishes the live key so a
//!   browser (or an operator) can *compare*; a browser that trusted
//!   the response instead of the credential would have no MITM
//!   protection at all, which is why the plan's MITM witness
//!   substitutes the responder rather than mutating this field.
//! - **TLS is browser-trusted or nothing**: operator-supplied PEM,
//!   or ACME. There is no self-signed mode and no
//!   certificate-ignore flag, because either would make the CI
//!   witness prove something no deployment can rely on.
//! - **Origin** is validated on the WebSocket, and CORS is an
//!   explicit allow-list with no wildcard. Credentials never ride a
//!   query string — the offer body carries them, over TLS.

use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{ConnectInfo, Query, State};
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use net::adapter::net::MeshNode;
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use tower_http::cors::CorsLayer;

use crate::bootstrap_credential::{BrowserBootstrapCredential, Psk};

/// Default ceiling on `POST /rtc/offer` per source IP per minute.
///
/// The plan's §5 "bootstrap endpoint budget": a per-source-IP rate
/// limit whose rejections are typed and fast. Every offer costs the
/// anchor an ICE agent, so this is the cheapest place to say no.
pub const DEFAULT_OFFERS_PER_IP_PER_MINUTE: u32 = 10;

/// Window the rate limit counts over.
const RATE_WINDOW: Duration = Duration::from_secs(60);

/// Cap on the offer body. An SDP is a few kilobytes; the credential
/// is bounded by its own format.
const MAX_OFFER_BODY_BYTES: usize = 64 * 1024;

/// How this listener obtains a **browser-trusted** certificate.
///
/// There is deliberately no self-signed variant: a browser refuses
/// one, so shipping the option would only produce deployments that
/// fail in the field and tests that pass by ignoring certificate
/// errors.
#[derive(Debug, Clone)]
pub enum BootstrapTls {
    /// Operator-supplied PEM chain + private key.
    Operator {
        /// PEM file holding the certificate chain, leaf first.
        cert_pem: PathBuf,
        /// PEM file holding the private key (PKCS#8, SEC1 or PKCS#1).
        key_pem: PathBuf,
    },
    /// ACME with HTTP-01 served by this same listener.
    ///
    /// The listener answers `/.well-known/acme-challenge/{token}`
    /// from [`AcmeState`], which the ordering client populates.
    Acme(AcmeConfig),
}

/// ACME (HTTP-01) parameters.
#[derive(Debug, Clone)]
pub struct AcmeConfig {
    /// Directory URL (Let's Encrypt production or staging, or a
    /// local Pebble).
    pub directory_url: String,
    /// The DNS name to issue for — the name in the credential's
    /// bootstrap URL.
    pub domain: String,
    /// Contact e-mail for the ACME account.
    pub contact_email: String,
    /// Where the issued certificate and key are cached, so a restart
    /// does not re-order.
    pub cache_dir: PathBuf,
}

/// Tokens the HTTP-01 challenge route serves.
///
/// Separate from the ordering client so the *listener* half is
/// testable without an ACME server: a test can install a token and
/// fetch it over the running listener, which is the part of ACME
/// that lives in this crate.
#[derive(Debug, Default, Clone)]
pub struct AcmeState {
    tokens: Arc<Mutex<HashMap<String, String>>>,
}

impl AcmeState {
    /// A fresh, empty challenge store.
    pub fn new() -> Self {
        Self::default()
    }

    /// Install (or replace) the key authorization served for `token`.
    pub fn set_challenge(&self, token: impl Into<String>, key_authorization: impl Into<String>) {
        self.tokens
            .lock()
            .insert(token.into(), key_authorization.into());
    }

    /// Forget a challenge once the order is finalized.
    pub fn clear_challenge(&self, token: &str) {
        self.tokens.lock().remove(token);
    }

    fn key_authorization(&self, token: &str) -> Option<String> {
        self.tokens.lock().get(token).cloned()
    }
}

/// Configuration for [`serve_bootstrap`].
#[derive(Debug, Clone)]
pub struct BootstrapConfig {
    /// Address to bind the HTTPS listener on.
    pub bind_addr: SocketAddr,
    /// The transport trust domain's PSK — the one credentials for
    /// this anchor were minted against. Used only to derive the
    /// trust-domain id a presented credential is checked against.
    pub psk: Psk,
    /// How to get a browser-trusted certificate.
    pub tls: BootstrapTls,
    /// Exact origins allowed to call the HTTP endpoints. **No
    /// wildcard**: `Access-Control-Allow-Origin: *` on an endpoint
    /// that takes a credential is a mistake this API does not let an
    /// operator make.
    pub allowed_origins: Vec<String>,
    /// Exact origins allowed to open the trickle WebSocket. Browsers
    /// send `Origin` on WebSocket handshakes but do **not** apply
    /// CORS to them, so this list is enforced by hand.
    pub ws_allowed_origins: Vec<String>,
    /// Per-source-IP `POST /rtc/offer` ceiling per minute.
    pub offers_per_ip_per_minute: u32,
    /// ACME challenge store; ignored unless [`BootstrapTls::Acme`].
    pub acme: AcmeState,
}

impl BootstrapConfig {
    /// A config with the defaults an operator would otherwise repeat:
    /// one origin, one certificate, the default rate ceiling.
    pub fn new(
        bind_addr: SocketAddr,
        psk: Psk,
        tls: BootstrapTls,
        origin: impl Into<String>,
    ) -> Self {
        let origin = origin.into();
        Self {
            bind_addr,
            psk,
            tls,
            allowed_origins: vec![origin.clone()],
            ws_allowed_origins: vec![origin],
            offers_per_ip_per_minute: DEFAULT_OFFERS_PER_IP_PER_MINUTE,
            acme: AcmeState::new(),
        }
    }
}

/// Why a bootstrap request was refused. Typed and fast, per §5.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BootstrapRefusal {
    /// The credential did not parse.
    MalformedCredential,
    /// One of the credential's two lifetimes has passed.
    ExpiredCredential,
    /// The credential belongs to a different transport trust domain.
    WrongTrustDomain,
    /// This source IP is over its offer rate.
    RateLimited,
    /// The anchor is at its §12 provisional bound.
    AtCapacity,
    /// The offer itself was refused (bad SDP, dialog budget).
    OfferRefused,
    /// The `Origin` header is not on the allow-list.
    ForbiddenOrigin,
    /// The named dialog does not exist on this anchor.
    UnknownDialog,
}

impl BootstrapRefusal {
    /// The HTTP status this refusal is reported with.
    pub fn status(self) -> StatusCode {
        match self {
            Self::RateLimited => StatusCode::TOO_MANY_REQUESTS,
            Self::AtCapacity => StatusCode::SERVICE_UNAVAILABLE,
            Self::ForbiddenOrigin => StatusCode::FORBIDDEN,
            Self::UnknownDialog => StatusCode::NOT_FOUND,
            _ => StatusCode::BAD_REQUEST,
        }
    }

    /// The WebSocket close code this refusal is reported with. 4403
    /// / 4404 are in the private range RFC 6455 reserves for the
    /// application, so a browser can tell them apart.
    pub fn close_code(self) -> u16 {
        match self {
            Self::ForbiddenOrigin => 4403,
            Self::UnknownDialog => 4404,
            _ => 4400,
        }
    }
}

/// `POST /rtc/offer` request body.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OfferRequest {
    /// The `net-bootstrap:` credential string.
    pub credential: String,
    /// The browser's self-generated node id, as a decimal or `0x`
    /// hex string (JSON numbers cannot hold a `u64` exactly).
    pub node_id: String,
    /// The SDP offer.
    pub sdp: String,
}

/// `POST /rtc/offer` success body.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OfferResponse {
    /// The dialog id the anchor assigned; the browser passes it to
    /// the trickle socket.
    pub dialog: u64,
    /// The SDP answer.
    pub sdp: String,
    /// The anchor's host candidate, so a browser that never opens
    /// the trickle socket can still form a pair.
    pub candidate: String,
}

/// `GET /rtc/anchor` body — the announcement fields, over HTTP.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnchorInfo {
    /// This anchor's node id (hex).
    pub node_id: String,
    /// Its Noise static public key (hex) — for **comparison** with
    /// the credential's pinned key, never as a source of it.
    pub noise_pubkey: String,
    /// The public RTC/STUN socket, when the operator configured one.
    pub rtc_addr: Option<String>,
    /// The trust domain this anchor serves.
    pub trust_domain: String,
    /// §12 provisional capacity: bound and current occupancy.
    pub max_provisional: usize,
    /// How many provisional sessions are installed right now.
    pub provisional: usize,
}

/// An error body. Machine-readable `refusal`, human `message`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ErrorBody {
    /// The typed refusal.
    pub refusal: BootstrapRefusal,
    /// What happened, for an operator reading a log.
    pub message: String,
}

/// The trickle socket's query string.
#[derive(Debug, Clone, Deserialize)]
pub struct TrickleQuery {
    /// The dialog assigned by `POST /rtc/offer`.
    pub dialog: u64,
    /// The same claimed node id the offer carried.
    pub node_id: String,
}

/// A running listener.
pub struct BootstrapHandle {
    local_addr: SocketAddr,
    shutdown: tokio::sync::oneshot::Sender<()>,
    task: tokio::task::JoinHandle<()>,
}

impl BootstrapHandle {
    /// The address actually bound (useful when the config asked for
    /// port 0).
    pub fn local_addr(&self) -> SocketAddr {
        self.local_addr
    }

    /// Stop serving and wait for the accept loop to finish.
    pub async fn shutdown(self) {
        let _ = self.shutdown.send(());
        let _ = self.task.await;
    }
}

#[derive(Clone)]
struct AppState {
    node: Arc<MeshNode>,
    psk: Arc<Psk>,
    ws_allowed_origins: Arc<Vec<String>>,
    rate: Arc<RateLimiter>,
    acme: AcmeState,
    dialogs: Arc<AtomicU64>,
}

/// Fixed-window per-source-IP counter. Deliberately not a token
/// bucket: the requirement is a fast, typed refusal, and a window is
/// the cheapest thing that cannot be walked past by pacing.
#[derive(Debug)]
struct RateLimiter {
    per_minute: u32,
    windows: Mutex<HashMap<IpAddr, (Instant, u32)>>,
}

impl RateLimiter {
    fn new(per_minute: u32) -> Self {
        Self {
            per_minute,
            windows: Mutex::new(HashMap::new()),
        }
    }

    /// `true` when this request is allowed.
    fn allow(&self, ip: IpAddr, now: Instant) -> bool {
        let mut windows = self.windows.lock();
        let entry = windows.entry(ip).or_insert((now, 0));
        if now.duration_since(entry.0) >= RATE_WINDOW {
            *entry = (now, 0);
        }
        if entry.1 >= self.per_minute {
            return false;
        }
        entry.1 += 1;
        true
    }
}

/// Build the router. Exposed so a test can drive the endpoints over
/// plain TCP without standing up a certificate authority — the TLS
/// layer is `serve_bootstrap`'s, and is witnessed separately.
pub fn bootstrap_router(node: Arc<MeshNode>, config: &BootstrapConfig) -> Router {
    // `AllowOrigin::list`, not repeated `allow_origin(value)` calls:
    // the latter REPLACES, and a single static value is echoed to
    // every caller — which is a wildcard wearing one origin's name.
    // A list matches the request's own `Origin` and answers nothing
    // when it is not on it.
    let origins: Vec<HeaderValue> = config
        .allowed_origins
        .iter()
        .filter_map(|origin| HeaderValue::from_str(origin).ok())
        .collect();
    let cors = CorsLayer::new()
        .allow_methods([Method::GET, Method::POST, Method::OPTIONS])
        .allow_headers([axum::http::header::CONTENT_TYPE])
        .allow_origin(tower_http::cors::AllowOrigin::list(origins));
    let state = AppState {
        node,
        psk: Arc::new(config.psk.clone()),
        ws_allowed_origins: Arc::new(config.ws_allowed_origins.clone()),
        rate: Arc::new(RateLimiter::new(config.offers_per_ip_per_minute)),
        acme: config.acme.clone(),
        dialogs: Arc::new(AtomicU64::new(1)),
    };
    // The trickle socket's `Origin` check is a LAYER, not a line in
    // the handler: an axum extractor rejection (`426`, "not a
    // WebSocket request") would otherwise answer a foreign origin
    // before the check ran, and a refusal has to name the reason it
    // actually is.
    let ws_origins = Arc::clone(&state.ws_allowed_origins);
    let trickle = get(get_trickle).layer(axum::middleware::from_fn(
        move |request: axum::extract::Request, next: axum::middleware::Next| {
            let allowed = Arc::clone(&ws_origins);
            async move {
                if origin_allowed(request.headers(), &allowed) {
                    next.run(request).await
                } else {
                    refuse(
                        BootstrapRefusal::ForbiddenOrigin,
                        "this origin may not open the trickle socket",
                    )
                }
            }
        },
    ));
    Router::new()
        .route("/rtc/offer", post(post_offer))
        .route("/rtc/anchor", get(get_anchor))
        .route("/rtc/trickle", trickle)
        .route(
            "/.well-known/acme-challenge/{token}",
            get(get_acme_challenge),
        )
        .layer(cors)
        .with_state(state)
}

/// Start the listener. Returns once the socket is bound, so a caller
/// can publish the URL without racing the first request.
pub async fn serve_bootstrap(
    node: Arc<MeshNode>,
    config: BootstrapConfig,
) -> Result<BootstrapHandle, BootstrapError> {
    let tls = tls_acceptor(&config).await?;
    let router = bootstrap_router(node, &config);
    let listener = tokio::net::TcpListener::bind(config.bind_addr)
        .await
        .map_err(|e| BootstrapError::Bind(e.to_string()))?;
    let local_addr = listener
        .local_addr()
        .map_err(|e| BootstrapError::Bind(e.to_string()))?;
    let (shutdown_tx, mut shutdown_rx) = tokio::sync::oneshot::channel();
    let task = tokio::spawn(async move {
        let service = router.into_make_service_with_connect_info::<SocketAddr>();
        loop {
            let accepted = tokio::select! {
                _ = &mut shutdown_rx => break,
                accepted = listener.accept() => accepted,
            };
            let Ok((stream, remote)) = accepted else {
                continue;
            };
            let tls = tls.clone();
            let service = service.clone();
            tokio::spawn(async move {
                serve_one(stream, remote, tls, service).await;
            });
        }
    });
    Ok(BootstrapHandle {
        local_addr,
        shutdown: shutdown_tx,
        task,
    })
}

async fn serve_one(
    stream: tokio::net::TcpStream,
    remote: SocketAddr,
    tls: Arc<tokio_rustls::TlsAcceptor>,
    mut service: axum::extract::connect_info::IntoMakeServiceWithConnectInfo<Router, SocketAddr>,
) {
    use hyper_util::rt::TokioIo;
    use tower::Service as _;

    let Ok(stream) = tls.accept(stream).await else {
        // A browser that refuses our certificate lands here. There is
        // nothing to say back over a handshake that did not complete.
        return;
    };
    // `IntoMakeServiceWithConnectInfo`'s error is `Infallible`, so
    // this cannot fail; unwrapping the `Ok` keeps that visible
    // rather than hiding a branch that can never run.
    let Ok(tower_service) = service.call(remote).await;
    let hyper_service =
        hyper::service::service_fn(move |request: axum::http::Request<hyper::body::Incoming>| {
            tower_service.clone().call(request)
        });
    let _ = hyper_util::server::conn::auto::Builder::new(hyper_util::rt::TokioExecutor::new())
        .serve_connection_with_upgrades(TokioIo::new(stream), hyper_service)
        .await;
}

/// Everything that can go wrong starting a listener.
#[derive(Debug, thiserror::Error)]
pub enum BootstrapError {
    /// The TCP socket could not be bound.
    #[error("binding the bootstrap listener failed: {0}")]
    Bind(String),
    /// The certificate or key could not be read or parsed.
    #[error("bootstrap TLS: {0}")]
    Tls(String),
    /// ACME could not obtain a certificate.
    #[error("bootstrap ACME: {0}")]
    Acme(String),
}

async fn tls_acceptor(
    config: &BootstrapConfig,
) -> Result<Arc<tokio_rustls::TlsAcceptor>, BootstrapError> {
    let (chain, key) = match &config.tls {
        BootstrapTls::Operator { cert_pem, key_pem } => read_pem_pair(cert_pem, key_pem)?,
        BootstrapTls::Acme(acme) => {
            crate::rtc_bootstrap_acme::obtain_certificate(acme, &config.acme).await?
        }
    };
    // The payments crate's rule, for the reason its module doc gives:
    // build with an explicit provider, NEVER
    // `CryptoProvider::install_default`, which is process-global and
    // would leak into every other rustls user in this process.
    let server_config = rustls::ServerConfig::builder_with_provider(Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_safe_default_protocol_versions()
    .map_err(|e| BootstrapError::Tls(e.to_string()))?
    .with_no_client_auth()
    .with_single_cert(chain, key)
    .map_err(|e| BootstrapError::Tls(e.to_string()))?;
    Ok(Arc::new(tokio_rustls::TlsAcceptor::from(Arc::new(
        server_config,
    ))))
}

pub(crate) fn read_pem_pair(
    cert_pem: &PathBuf,
    key_pem: &PathBuf,
) -> Result<
    (
        Vec<rustls::pki_types::CertificateDer<'static>>,
        rustls::pki_types::PrivateKeyDer<'static>,
    ),
    BootstrapError,
> {
    let cert_bytes = std::fs::read(cert_pem)
        .map_err(|e| BootstrapError::Tls(format!("reading {}: {e}", cert_pem.display())))?;
    let key_bytes = std::fs::read(key_pem)
        .map_err(|e| BootstrapError::Tls(format!("reading {}: {e}", key_pem.display())))?;
    let chain = rustls_pemfile::certs(&mut cert_bytes.as_slice())
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| BootstrapError::Tls(format!("parsing the certificate chain: {e}")))?;
    if chain.is_empty() {
        return Err(BootstrapError::Tls(format!(
            "{} contains no certificate",
            cert_pem.display()
        )));
    }
    let key = rustls_pemfile::private_key(&mut key_bytes.as_slice())
        .map_err(|e| BootstrapError::Tls(format!("parsing the private key: {e}")))?
        .ok_or_else(|| BootstrapError::Tls(format!("{} contains no key", key_pem.display())))?;
    Ok((chain, key))
}

fn refuse(refusal: BootstrapRefusal, message: impl Into<String>) -> Response {
    (
        refusal.status(),
        Json(ErrorBody {
            refusal,
            message: message.into(),
        }),
    )
        .into_response()
}

fn parse_node_id(raw: &str) -> Option<u64> {
    let raw = raw.trim();
    match raw.strip_prefix("0x").or_else(|| raw.strip_prefix("0X")) {
        Some(hex) => u64::from_str_radix(hex, 16).ok(),
        None => raw.parse().ok(),
    }
}

/// Validate a presented credential against this anchor: it must
/// parse, both lifetimes must be live, and it must belong to this
/// anchor's transport trust domain.
fn check_credential(raw: &str, psk: &Psk) -> Result<BrowserBootstrapCredential, Response> {
    let credential = BrowserBootstrapCredential::decode(raw).map_err(|e| {
        refuse(
            BootstrapRefusal::MalformedCredential,
            format!("the credential did not parse: {e}"),
        )
    })?;
    // Domain first: telling a caller from another trust domain that
    // their nonce expired would send them to the wrong knob.
    credential.check_trust_domain(psk).map_err(|e| {
        refuse(
            BootstrapRefusal::WrongTrustDomain,
            format!("this anchor does not serve that trust domain: {e}"),
        )
    })?;
    credential.validate().map_err(|e| {
        refuse(
            BootstrapRefusal::ExpiredCredential,
            format!("the credential is not presentable: {e}"),
        )
    })?;
    Ok(credential)
}

async fn post_offer(
    State(state): State<AppState>,
    ConnectInfo(remote): ConnectInfo<SocketAddr>,
    body: String,
) -> Response {
    if body.len() > MAX_OFFER_BODY_BYTES {
        return refuse(
            BootstrapRefusal::MalformedCredential,
            "the offer body is over the bound",
        );
    }
    let Ok(request) = serde_json::from_str::<OfferRequest>(&body) else {
        return refuse(
            BootstrapRefusal::MalformedCredential,
            "the offer body is not the expected JSON",
        );
    };
    // Rate first: it is the cheapest check, and the one an attacker
    // is trying to spend.
    if !state.rate.allow(remote.ip(), Instant::now()) {
        return refuse(
            BootstrapRefusal::RateLimited,
            "too many bootstrap offers from this address",
        );
    }
    let credential = match check_credential(&request.credential, &state.psk) {
        Ok(credential) => credential,
        Err(response) => return response,
    };
    let Some(node_id) = parse_node_id(&request.node_id) else {
        return refuse(
            BootstrapRefusal::MalformedCredential,
            "node_id is not a u64",
        );
    };
    // §12's global provisional bound, read rather than re-derived: an
    // offer accepted past it produces a session admission would shed
    // immediately, so refusing here is both cheaper and honest.
    let max_provisional = state.node.rtc_max_provisional();
    if max_provisional > 0 && state.node.provisional_count() >= max_provisional {
        return refuse(
            BootstrapRefusal::AtCapacity,
            "this anchor is at its provisional-session bound",
        );
    }

    let dialog = state.dialogs.fetch_add(1, Ordering::Relaxed);
    match state
        .node
        .accept_bootstrap_offer(node_id, dialog, request.sdp)
        .await
    {
        Ok(sdp) => {
            let candidate = state.node.bootstrap_host_candidate().unwrap_or_default();
            tracing::debug!(
                dialog,
                node = format!("{node_id:#x}"),
                domain = %credential.trust_domain,
                "bootstrap offer accepted"
            );
            Json(OfferResponse {
                dialog,
                sdp,
                candidate,
            })
            .into_response()
        }
        Err(e) => refuse(BootstrapRefusal::OfferRefused, e.to_string()),
    }
}

async fn get_anchor(State(state): State<AppState>) -> Response {
    Json(AnchorInfo {
        node_id: format!("{:#x}", state.node.node_id()),
        noise_pubkey: hex_of(state.node.public_key()),
        rtc_addr: state.node.rtc_public_addr().map(|a| a.to_string()),
        trust_domain: state.psk.trust_domain().to_string(),
        max_provisional: state.node.rtc_max_provisional(),
        provisional: state.node.provisional_count(),
    })
    .into_response()
}

async fn get_acme_challenge(
    State(state): State<AppState>,
    axum::extract::Path(token): axum::extract::Path<String>,
) -> Response {
    match state.acme.key_authorization(&token) {
        Some(key_authorization) => (StatusCode::OK, key_authorization).into_response(),
        None => (StatusCode::NOT_FOUND, "no such challenge").into_response(),
    }
}

/// Is `Origin` on the allow-list? A **missing** Origin is refused
/// too: every browser sends one on a WebSocket handshake, so its
/// absence is a non-browser client, which this endpoint does not
/// serve.
fn origin_allowed(headers: &HeaderMap, allowed: &[String]) -> bool {
    headers
        .get(axum::http::header::ORIGIN)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|origin| allowed.iter().any(|a| a == origin))
}

async fn get_trickle(
    State(state): State<AppState>,
    Query(query): Query<TrickleQuery>,
    ws: WebSocketUpgrade,
) -> Response {
    // The origin was already checked by this route's layer, before
    // the upgrade — a socket that opens and then closes has already
    // let a foreign origin hold anchor state.
    let Some(node_id) = parse_node_id(&query.node_id) else {
        return refuse(
            BootstrapRefusal::MalformedCredential,
            "node_id is not a u64",
        );
    };
    ws.on_upgrade(move |socket| trickle_socket(socket, state, node_id, query.dialog))
}

async fn trickle_socket(mut socket: WebSocket, state: AppState, node_id: u64, dialog: u64) {
    use axum::extract::ws::CloseFrame;

    // The anchor's own candidate goes first: the browser can start
    // checks against it while it is still gathering its own. S0b
    // measured trickle at 6.6x gather-complete at the floor, and
    // this is the half the anchor controls.
    if let Some(candidate) = state.node.bootstrap_host_candidate() {
        let frame = serde_json::json!({
            "type": "candidate",
            "dialog": dialog,
            "candidate": candidate,
            "mid": "0",
        });
        if socket
            .send(Message::Text(frame.to_string().into()))
            .await
            .is_err()
        {
            return;
        }
    }

    while let Some(Ok(message)) = socket.recv().await {
        let text = match message {
            Message::Text(text) => text,
            Message::Close(_) => break,
            // Candidates are the JSON form of the `0x0D02` message;
            // binary frames are not part of this protocol.
            _ => continue,
        };
        let Ok(value) = serde_json::from_str::<serde_json::Value>(&text) else {
            continue;
        };
        if value["type"] != "candidate" {
            continue;
        }
        let candidate = value["candidate"].as_str().unwrap_or_default().to_string();
        let mid = value["mid"].as_str().unwrap_or("0").to_string();
        if state
            .node
            .apply_bootstrap_candidate(node_id, dialog, candidate, mid)
            .await
            .is_err()
        {
            // A candidate for a dialog this anchor does not hold:
            // typed close, so the browser learns *which* thing was
            // wrong rather than seeing a silent hang.
            let _ = socket
                .send(Message::Close(Some(CloseFrame {
                    code: BootstrapRefusal::UnknownDialog.close_code(),
                    reason: "unknown dialog".into(),
                })))
                .await;
            return;
        }
    }
    // The browser went away before the channel opened; do not leave
    // the attempt holding a budget slot until its deadline.
    state.node.end_bootstrap_dialog(node_id, dialog).await;
}

fn hex_of(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_rate_limiter_refuses_past_the_ceiling_and_recovers_next_window() {
        let limiter = RateLimiter::new(3);
        let ip: IpAddr = "203.0.113.7".parse().unwrap();
        let t0 = Instant::now();
        assert!(limiter.allow(ip, t0));
        assert!(limiter.allow(ip, t0));
        assert!(limiter.allow(ip, t0));
        assert!(
            !limiter.allow(ip, t0),
            "the fourth offer in a window is refused"
        );
        // Another address is unaffected — the bound is per source.
        assert!(limiter.allow("198.51.100.1".parse().unwrap(), t0));
        // …and the window rolls.
        assert!(limiter.allow(ip, t0 + RATE_WINDOW + Duration::from_millis(1)));
    }

    #[test]
    fn a_node_id_may_be_decimal_or_hex_and_nothing_else() {
        assert_eq!(parse_node_id("42"), Some(42));
        assert_eq!(parse_node_id("0x2a"), Some(42));
        assert_eq!(parse_node_id(" 0X2A "), Some(42));
        assert_eq!(parse_node_id("nope"), None);
        assert_eq!(parse_node_id(""), None);
    }

    #[test]
    fn a_missing_origin_is_not_an_allowed_origin() {
        let allowed = vec!["https://app.example".to_string()];
        let mut headers = HeaderMap::new();
        assert!(
            !origin_allowed(&headers, &allowed),
            "a handshake with no Origin is not a browser"
        );
        headers.insert(
            axum::http::header::ORIGIN,
            HeaderValue::from_static("https://evil.example"),
        );
        assert!(!origin_allowed(&headers, &allowed));
        headers.insert(
            axum::http::header::ORIGIN,
            HeaderValue::from_static("https://app.example"),
        );
        assert!(origin_allowed(&headers, &allowed));
    }

    #[test]
    fn every_refusal_has_a_distinguishable_status_and_close_code() {
        assert_eq!(
            BootstrapRefusal::RateLimited.status(),
            StatusCode::TOO_MANY_REQUESTS
        );
        assert_eq!(
            BootstrapRefusal::AtCapacity.status(),
            StatusCode::SERVICE_UNAVAILABLE
        );
        assert_eq!(
            BootstrapRefusal::ForbiddenOrigin.status(),
            StatusCode::FORBIDDEN
        );
        assert_eq!(BootstrapRefusal::UnknownDialog.close_code(), 4404);
        assert_eq!(BootstrapRefusal::ForbiddenOrigin.close_code(), 4403);
    }

    #[test]
    fn the_acme_challenge_store_serves_only_installed_tokens() {
        let state = AcmeState::new();
        assert_eq!(state.key_authorization("tok"), None);
        state.set_challenge("tok", "tok.thumbprint");
        assert_eq!(
            state.key_authorization("tok"),
            Some("tok.thumbprint".to_string())
        );
        state.clear_challenge("tok");
        assert_eq!(state.key_authorization("tok"), None);
    }
}
