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
use std::fmt;
use std::net::{IpAddr, SocketAddr};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{ConnectInfo, State};
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use net::adapter::net::MeshNode;
use parking_lot::{Mutex, RwLock};
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
    /// **Additional trust roots for the ACME DIRECTORY connection
    /// only.**
    ///
    /// Each entry is a PEM file of one or more CA certificates. When
    /// this list is non-empty, the HTTPS connection this client
    /// makes *to `directory_url`* is verified against exactly these
    /// roots instead of the platform trust store. That is the real
    /// shape of the need: an operator running a private ACME CA
    /// (step-ca, Boulder/Pebble in a lab, an internal Smallstep
    /// deployment) has a closed PKI, and folding the public web PKI
    /// in beside it would only widen the set of issuers allowed to
    /// impersonate the directory. Leave it empty for Let's Encrypt
    /// or any other publicly-trusted directory.
    ///
    /// What this does **not** relax, at all:
    ///
    /// - It does not weaken verification of that connection. The
    ///   directory's certificate must still chain to one of these
    ///   roots, still match the hostname in `directory_url`, and
    ///   still be inside its validity window. There is no
    ///   "accept invalid certificate" mode here or anywhere else in
    ///   this module.
    /// - It says nothing about the certificate the directory
    ///   **issues**. That one is presented to browsers, which trust
    ///   their own root program and neither know nor care what is in
    ///   this list.
    /// - It is not installed process-globally and does not touch the
    ///   bootstrap listener's own TLS, the mesh transport, or any
    ///   other outbound connection this process makes.
    pub directory_roots: Vec<PathBuf>,
}

impl AcmeConfig {
    /// An ACME config for one domain. The HTTP-01 ingress address
    /// and the renewal horizon live on [`BootstrapConfig`], because
    /// they are the listener's, not the directory's.
    pub fn new(
        directory_url: impl Into<String>,
        domain: impl Into<String>,
        contact_email: impl Into<String>,
        cache_dir: PathBuf,
    ) -> Self {
        Self {
            directory_url: directory_url.into(),
            domain: domain.into(),
            contact_email: contact_email.into(),
            cache_dir,
            directory_roots: Vec::new(),
        }
    }

    /// Verify the directory connection against these roots instead
    /// of the platform store — see [`Self::directory_roots`].
    pub fn with_directory_roots(mut self, roots: impl IntoIterator<Item = PathBuf>) -> Self {
        self.directory_roots = roots.into_iter().collect();
        self
    }
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
    answered: Arc<AtomicU64>,
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

    /// How many HTTP-01 challenge fetches this store has **answered**
    /// with a key authorization.
    ///
    /// The cold-start witness needs to distinguish "the directory
    /// issued a certificate" from "the directory came back to this
    /// process for the token it issued against". A directory
    /// configured to short-circuit validation (pebble's
    /// `PEBBLE_VA_ALWAYS_VALID=1`) issues happily without ever
    /// dialling the ingress, so issuance alone is not evidence that
    /// the HTTP-01 reverse path works. This counter is.
    pub fn answered_challenges(&self) -> u64 {
        self.answered.load(Ordering::Relaxed)
    }

    fn key_authorization(&self, token: &str) -> Option<String> {
        let found = self.tokens.lock().get(token).cloned();
        if found.is_some() {
            self.answered.fetch_add(1, Ordering::Relaxed);
        }
        found
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
    /// The **issuer** whose signature this anchor accepts on a
    /// credential (R3). Public half only: the anchor verifies, it
    /// does not mint.
    ///
    /// This is what makes the two lifetimes enforceable. A recipient
    /// holds the PSK, so anything keyed on the PSK is forgeable by
    /// every recipient; only a key they do NOT hold can bind the
    /// deadlines they would like to extend.
    pub credential_issuer: crate::identity::EntityId,
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
    /// Address for the **plaintext** HTTP-01 challenge ingress
    /// (R4a), bound BEFORE any order is placed and kept for
    /// renewals. Defaults to `0.0.0.0:80`, the port a directory
    /// dials.
    ///
    /// The brief said "HTTP-01 on the same listener"; HTTP-01 is a
    /// plaintext protocol and the bootstrap listener is TLS-only, so
    /// on a cold cache there was no way to answer the challenge that
    /// produces the certificate the TLS listener needs. Same
    /// process, same challenge store, second socket — reconciled in
    /// the report.
    pub acme_challenge_addr: SocketAddr,
    /// Renew this long before the certificate expires (R4c).
    pub acme_renewal_horizon: Duration,
}

impl BootstrapConfig {
    /// A config with the defaults an operator would otherwise repeat:
    /// one origin, one certificate, the default rate ceiling.
    pub fn new(
        bind_addr: SocketAddr,
        psk: Psk,
        credential_issuer: crate::identity::EntityId,
        tls: BootstrapTls,
        origin: impl Into<String>,
    ) -> Self {
        let origin = origin.into();
        Self {
            bind_addr,
            psk,
            credential_issuer,
            tls,
            allowed_origins: vec![origin.clone()],
            ws_allowed_origins: vec![origin],
            offers_per_ip_per_minute: DEFAULT_OFFERS_PER_IP_PER_MINUTE,
            acme: AcmeState::new(),
            acme_challenge_addr: SocketAddr::from(([0, 0, 0, 0], 80)),
            acme_renewal_horizon: crate::rtc_bootstrap_acme::DEFAULT_RENEWAL_HORIZON,
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
///
/// Its [`Debug`] is hand-written: the `credential` field is the
/// whole PSK-bearing credential string, and a derived `Debug` put it
/// into any log line, panic message or `tracing` field that
/// formatted a request (R5a).
#[derive(Clone, Serialize, Deserialize)]
pub struct OfferRequest {
    /// The `net-bootstrap:` credential string.
    pub credential: String,
    /// The browser's self-generated node id, as a decimal or `0x`
    /// hex string (JSON numbers cannot hold a `u64` exactly).
    pub node_id: String,
    /// The SDP offer.
    pub sdp: String,
}

impl fmt::Debug for OfferRequest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // The credential is a bearer secret. Its LENGTH is the most
        // that helps a diagnosis; the trust domain and the issuer are
        // available from the parsed credential, which redacts its own
        // PSK.
        f.debug_struct("OfferRequest")
            .field(
                "credential",
                &format_args!("<{} redacted bytes>", self.credential.len()),
            )
            .field("node_id", &self.node_id)
            .field("sdp_bytes", &self.sdp.len())
            .finish()
    }
}

/// `POST /rtc/offer` success body.
#[derive(Clone, Serialize, Deserialize)]
pub struct OfferResponse {
    /// **The attempt token** (R1): 32 random bytes, hex, minted for
    /// THIS accepted offer and bound to its `(node, dialog,
    /// incarnation)`. The trickle socket will not upgrade without
    /// it, so a second client that guesses the dialog id cannot
    /// trickle into, or retire, someone else's live attempt.
    ///
    /// It is carried as the WebSocket **subprotocol**, never in a
    /// URL: a query string lands in proxy and browser history, and
    /// this is a bearer credential for an in-flight ICE attempt.
    pub attempt_token: String,
    /// The dialog id the anchor assigned; the browser passes it to
    /// the trickle socket.
    pub dialog: u64,
    /// The SDP answer.
    pub sdp: String,
    /// The anchor's host candidate, so a browser that never opens
    /// the trickle socket can still form a pair.
    pub candidate: String,
}

/// The response carries a bearer credential too (R5).
///
/// `attempt_token` authorizes the trickle socket for a live ICE
/// attempt: whoever holds it can trickle into that attempt or retire
/// it. The derived `Debug` printed it in full, which is the same
/// class of defect as the request printing its credential — a
/// response logged on an error path hands out pending-attempt
/// control. It is not the domain PSK, and no production logging call
/// is known to print it; it is redacted anyway.
impl std::fmt::Debug for OfferResponse {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OfferResponse")
            .field("attempt_token", &"<redacted>")
            .field("dialog", &self.dialog)
            .field("sdp_bytes", &self.sdp.len())
            .field("candidate", &self.candidate)
            .finish()
    }
}

/// `GET /rtc/anchor` body — the announcement fields, over HTTP.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnchorInfo {
    /// This anchor's node id (hex).
    pub node_id: String,
    /// Its Noise static public key (hex) — for **comparison** with
    /// the credential's pinned key, never as a source of it.
    pub noise_pubkey: String,
    /// The public RTC socket, when the operator configured one.
    /// The diagnostic STUN probe's only legitimate target.
    pub rtc_addr: Option<String>,
    /// The **separately announced** STUN endpoint, when the operator
    /// configured one: a second UDP socket, distinct from
    /// [`Self::rtc_addr`], which a leaf's default `iceServers` points
    /// at. `None` means nothing was announced, and a leaf then
    /// configures no ICE servers at all.
    pub stun_addr: Option<String>,
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

/// One accepted offer's authorization to trickle (R1).
///
/// The token is the identity: `(node, dialog)` are caller-visible
/// and guessable, `incarnation` distinguishes two attempts that
/// reuse a dialog id, and the token itself is what a socket must
/// present. Nothing here is derived from anything the caller chose.
#[derive(Debug, Clone)]
struct Attempt {
    node_id: u64,
    dialog: u64,
    incarnation: u64,
    /// The pre-authentication identity this attempt's ingress is
    /// accounted against (R2): the token's own incarnation, NOT the
    /// unverified `node_id` the caller claimed.
    budget_id: u64,
    /// Which trickle socket currently owns this attempt's lifecycle
    /// (R1): bumped at every authorized upgrade. The token
    /// re-upgrades — a reconnecting browser reuses it — so "whoever
    /// retires the token first" is not "whoever abandoned the
    /// attempt": a stale socket's delayed close must not end its
    /// successor's live attempt. Only the CURRENT socket generation
    /// may end the dialog.
    socket: u64,
}

/// The live attempts, keyed by token.
#[derive(Default)]
struct Attempts {
    by_token: Mutex<HashMap<String, Attempt>>,
}

/// Redacting `Debug` (MR#16's class): the map keys **are** the live
/// attempt tokens (R1) — whoever holds one can trickle into that
/// attempt or retire it — so the derived `Debug` over this
/// token-keyed map handed out pending-attempt control from any log
/// line, panic message or `tracing` field that formatted the state.
/// It is not the domain PSK, and no production logging call is known
/// to print it; it is redacted anyway (the same treatment
/// [`OfferResponse`] gives `attempt_token`). The attempts' own
/// fields — which dialog, whose node, which socket generation — stay
/// visible for diagnosis.
impl fmt::Debug for Attempts {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let guard = self.by_token.lock();
        f.debug_map()
            .entries(guard.iter().map(|(_, attempt)| ("<redacted>", attempt)))
            .finish()
    }
}

impl Attempts {
    /// Mint a token for this attempt. The **budget id is derived
    /// from the token's own random bytes** (R2): uniform, unrelated
    /// to anything the caller sent, and therefore not another peer's
    /// node id.
    fn mint(&self, node_id: u64, dialog: u64, incarnation: u64) -> Option<(String, u64)> {
        let mut raw = [0u8; 32];
        if getrandom::fill(&mut raw).is_err() {
            // A predictable attempt token is a credential anyone can
            // guess. Refuse to mint rather than mint a weak one.
            return None;
        }
        let budget_id = u64::from_le_bytes(raw[..8].try_into().ok()?);
        let token: String = raw.iter().map(|b| format!("{b:02x}")).collect();
        self.by_token.lock().insert(
            token.clone(),
            Attempt {
                node_id,
                dialog,
                incarnation,
                budget_id,
                socket: 0,
            },
        );
        Some((token, budget_id))
    }

    /// The attempt this token authorizes, if it names exactly this
    /// `(node, dialog)`. A token for another tuple is as good as no
    /// token.
    ///
    /// This upgrade SUPERSEDES any earlier socket's ownership (R1):
    /// the returned `socket` is this upgrade's generation, and only
    /// it may retire the attempt when its socket closes.
    fn authorize(&self, token: &str, node_id: u64, dialog: u64) -> Option<Attempt> {
        let mut guard = self.by_token.lock();
        let attempt = guard.get_mut(token)?;
        if attempt.node_id != node_id || attempt.dialog != dialog {
            return None;
        }
        attempt.socket += 1;
        Some(attempt.clone())
    }

    /// Retire a token; `true` when this call owned the removal.
    fn retire(&self, token: &str) -> bool {
        self.by_token.lock().remove(token).is_some()
    }

    /// Retire a token on behalf of socket generation `socket`;
    /// `true` when this call owned the removal. Only the CURRENT
    /// socket generation may end its attempt: a stale socket's
    /// delayed close used to `retire` first and end its successor's
    /// live attempt mid-ICE.
    fn retire_socket(&self, token: &str, socket: u64) -> bool {
        let mut guard = self.by_token.lock();
        let owns = guard.get(token).is_some_and(|a| a.socket == socket);
        owns && guard.remove(token).is_some()
    }
}

#[derive(Clone)]
struct AppState {
    node: Arc<MeshNode>,
    psk: Arc<Psk>,
    issuer: Arc<crate::identity::EntityId>,
    ws_allowed_origins: Arc<Vec<String>>,
    rate: Arc<RateLimiter>,
    acme: AcmeState,
    dialogs: Arc<AtomicU64>,
    attempts: Arc<Attempts>,
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
        issuer: Arc::new(config.credential_issuer.clone()),
        ws_allowed_origins: Arc::new(config.ws_allowed_origins.clone()),
        rate: Arc::new(RateLimiter::new(config.offers_per_ip_per_minute)),
        acme: config.acme.clone(),
        dialogs: Arc::new(AtomicU64::new(1)),
        attempts: Arc::new(Attempts::default()),
    };
    // The trickle socket's `Origin` check is a LAYER, not a line in
    // the handler: an axum extractor rejection (`426`, "not a
    // WebSocket request") would otherwise answer a foreign origin
    // before the check ran, and a refusal has to name the reason it
    // actually is.
    let ws_origins = Arc::clone(&state.ws_allowed_origins);
    let ws_attempts = Arc::clone(&state.attempts);
    let ws_node_outer = Arc::clone(&state.node);
    let trickle = get(get_trickle).layer(axum::middleware::from_fn(
        move |request: axum::extract::Request, next: axum::middleware::Next| {
            let allowed = Arc::clone(&ws_origins);
            let attempts = Arc::clone(&ws_attempts);
            let ws_node = Arc::clone(&ws_node_outer);
            async move {
                // **Every refusal below is logged** because a
                // refused WebSocket *handshake* is invisible to the
                // page: a browser reports it as a bare `1006` with
                // no code and no reason, so the anchor's log is the
                // only place the actual reason can be read.
                if !origin_allowed(request.headers(), &allowed) {
                    tracing::debug!(
                        origin = ?request.headers().get(axum::http::header::ORIGIN),
                        "trickle upgrade refused: origin not on the allow-list"
                    );
                    return refuse(
                        BootstrapRefusal::ForbiddenOrigin,
                        "this origin may not open the trickle socket",
                    );
                }
                // **R1: the attempt token, decided at the upgrade.**
                // In the layer rather than the handler for the same
                // reason the origin check is: an axum extractor
                // rejection would otherwise answer first, and a
                // socket that upgrades and is then closed has
                // already held anchor state. `(node, dialog)` are
                // guessable; the token is not.
                let Some((node_id, dialog)) = trickle_ids(request.uri()) else {
                    tracing::debug!(
                        query = ?request.uri().query(),
                        "trickle upgrade refused: no node_id/dialog in the query"
                    );
                    return refuse(
                        BootstrapRefusal::MalformedCredential,
                        "the trickle socket needs node_id and dialog",
                    );
                };
                let Some(token) = presented_token(request.headers()) else {
                    tracing::debug!(
                        node = format!("{node_id:#x}"),
                        dialog,
                        "trickle upgrade refused: no attempt token subprotocol"
                    );
                    return refuse(
                        BootstrapRefusal::ForbiddenOrigin,
                        "the trickle socket requires the attempt token from \
                         POST /rtc/offer, presented as the \
                         `net-bootstrap-attempt.<token>` subprotocol",
                    );
                };
                let Some(attempt) = attempts.authorize(&token, node_id, dialog) else {
                    tracing::debug!(
                        node = format!("{node_id:#x}"),
                        dialog,
                        "trickle upgrade refused: that token names no such attempt"
                    );
                    return refuse(
                        BootstrapRefusal::UnknownDialog,
                        "no such attempt for that token, node and dialog",
                    );
                };
                // **The token is not the attempt** (R1). A token
                // proves who minted it; the anchor's own row proves
                // the attempt still exists. An offer whose core
                // attempt has expired, been rejected or already
                // completed kept authorizing a socket — the review
                // observed the core reporting no dialog while the
                // same token still upgraded. The key is the one the
                // acceptance recorded, so this is the exact accepted
                // attempt and not a same-tuple successor.
                if !ws_node.bootstrap_attempt_is_live(node_id, dialog, attempt.budget_id) {
                    tracing::debug!(
                        node = format!("{node_id:#x}"),
                        dialog,
                        incarnation = attempt.incarnation,
                        "trickle upgrade refused: the attempt is no longer live on this anchor"
                    );
                    attempts.retire(&token);
                    return refuse(
                        BootstrapRefusal::UnknownDialog,
                        "that attempt is no longer live on this anchor",
                    );
                }
                let mut request = request;
                request
                    .extensions_mut()
                    .insert(Authorized { attempt, token });
                next.run(request).await
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
    let (initial, challenge_task) = tls_acceptor(&config).await?;
    // **R4c: the acceptor is swappable.** A static one meant the
    // certificate a process started with was the certificate it died
    // with; ACME certificates expire in weeks.
    let tls: Arc<RwLock<Arc<tokio_rustls::TlsAcceptor>>> = Arc::new(RwLock::new(initial));
    let renewal = spawn_renewal(&config, Arc::clone(&tls));
    let router = bootstrap_router(node, &config);
    // A TLS bind that fails AFTER the challenge ingress and the
    // renewal owner exist must not detach them either (R4, round
    // two): same reasoning as the ordering failure, same remedy.
    let bound = tokio::net::TcpListener::bind(config.bind_addr)
        .await
        .and_then(|listener| listener.local_addr().map(|addr| (listener, addr)));
    let (listener, local_addr) = match bound {
        Ok(pair) => pair,
        Err(e) => {
            if let Some(renewal) = renewal {
                abort_and_join(renewal).await;
            }
            if let Some(challenge) = challenge_task {
                abort_and_join(challenge).await;
            }
            return Err(BootstrapError::Bind(e.to_string()));
        }
    };
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
            // Read the CURRENT acceptor per connection, so a
            // renewal swap takes effect on the next handshake
            // without dropping live ones (R4c).
            let tls = Arc::clone(&*tls.read());
            let service = service.clone();
            tokio::spawn(async move {
                serve_one(stream, remote, tls, service).await;
            });
        }
        // Settled, not merely signalled: `shutdown().await`
        // returning must mean the challenge port is free.
        if let Some(renewal) = renewal {
            abort_and_join(renewal).await;
        }
        if let Some(challenge) = challenge_task {
            abort_and_join(challenge).await;
        }
    });
    Ok(BootstrapHandle {
        local_addr,
        shutdown: shutdown_tx,
        task,
    })
}

/// The shortest gap between two SUCCESSFUL renewals (R4, round
/// two). Long enough that a short-lived certificate cannot spin the
/// loop, far shorter than any real renewal horizon.
const MIN_RENEWAL_INTERVAL: Duration = Duration::from_secs(300);

/// The renewal owner (R4c): re-orders at
/// `not_after - renewal_horizon` and swaps the acceptor in place.
///
/// `None` for operator-supplied PEM — that lifecycle is the
/// operator's, and the documented path is to restart or reload with
/// the new files.
fn spawn_renewal(
    config: &BootstrapConfig,
    tls: Arc<RwLock<Arc<tokio_rustls::TlsAcceptor>>>,
) -> Option<tokio::task::JoinHandle<()>> {
    let BootstrapTls::Acme(acme) = config.tls.clone() else {
        return None;
    };
    let challenges = config.acme.clone();
    let horizon = config.acme_renewal_horizon;
    Some(tokio::spawn(async move {
        loop {
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0);
            let renew_at = crate::rtc_bootstrap_acme::renew_at(&acme, horizon);
            let sleep_for = match renew_at {
                // Nothing cached, or already inside the horizon:
                // re-order now rather than sleeping on a certificate
                // that is about to stop working.
                Some(at) if at > now => Duration::from_secs(at - now),
                _ => Duration::ZERO,
            };
            if !sleep_for.is_zero() {
                tokio::time::sleep(sleep_for).await;
            }
            match crate::rtc_bootstrap_acme::renew_certificate(&acme, &challenges).await {
                Ok((chain, key)) => match server_config(chain, key) {
                    Ok(acceptor) => {
                        *tls.write() = acceptor;
                        tracing::info!(domain = %acme.domain, "bootstrap certificate renewed");
                        // **Successful orders are paced too** (R4,
                        // round two). Only errors used to back off,
                        // so a certificate issued with a lifetime at
                        // or below the horizon — a short-lived test
                        // CA, a directory that trims validity — put
                        // this loop straight back into `renew_at <=
                        // now` and ordered again immediately, for
                        // ever. A floor between SUCCESSFUL orders
                        // bounds that without weakening the horizon:
                        // the next wake still respects `renew_at`
                        // when it is further out.
                        tokio::time::sleep(MIN_RENEWAL_INTERVAL).await;
                    }
                    Err(e) => tracing::error!(error = %e, "renewed certificate did not load"),
                },
                Err(e) => {
                    tracing::error!(error = %e, domain = %acme.domain, "certificate renewal failed");
                    // Back off rather than spin against a directory
                    // that is refusing us.
                    tokio::time::sleep(Duration::from_secs(3600)).await;
                }
            }
        }
    }))
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

/// Cancel a task and WAIT for it to be gone.
///
/// `abort()` requests cancellation, it does not perform it — and for
/// a task that owns a listening socket the difference is whether the
/// port is free when the caller returns (R4, round two).
async fn abort_and_join(task: tokio::task::JoinHandle<()>) {
    task.abort();
    let _ = task.await;
}

/// The plaintext HTTP-01 challenge ingress (R4a).
///
/// Bound BEFORE any order is placed, serving exactly one route from
/// the same [`AcmeState`] the ordering client writes into. The
/// brief said "HTTP-01 on the same listener"; HTTP-01 is a
/// plaintext protocol and the bootstrap listener is TLS-only, so on
/// a cold cache there was no way to answer the challenge that
/// produces the certificate the TLS listener needs. Same process,
/// same challenge store, second socket.
async fn serve_challenge_ingress(
    addr: SocketAddr,
    acme: AcmeState,
) -> Result<(tokio::task::JoinHandle<()>, SocketAddr), BootstrapError> {
    let router = Router::new()
        .route("/.well-known/acme-challenge/{token}", get(challenge_route))
        .with_state(acme);
    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .map_err(|e| BootstrapError::Bind(format!("acme http-01 ingress on {addr}: {e}")))?;
    let bound = listener
        .local_addr()
        .map_err(|e| BootstrapError::Bind(e.to_string()))?;
    let task = tokio::spawn(async move {
        let _ = axum::serve(listener, router).await;
    });
    Ok((task, bound))
}

async fn challenge_route(
    State(acme): State<AcmeState>,
    axum::extract::Path(token): axum::extract::Path<String>,
) -> Response {
    match acme.key_authorization(&token) {
        Some(auth) => (StatusCode::OK, auth).into_response(),
        None => (StatusCode::NOT_FOUND, "no such challenge").into_response(),
    }
}

/// Build a TLS acceptor from a chain and key.
///
/// The payments crate's rule, for the reason its module doc gives:
/// an explicit provider, NEVER `CryptoProvider::install_default`,
/// which is process-global and would leak into every other rustls
/// user in this process.
fn server_config(
    chain: Vec<rustls::pki_types::CertificateDer<'static>>,
    key: rustls::pki_types::PrivateKeyDer<'static>,
) -> Result<Arc<tokio_rustls::TlsAcceptor>, BootstrapError> {
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

async fn tls_acceptor(
    config: &BootstrapConfig,
) -> Result<
    (
        Arc<tokio_rustls::TlsAcceptor>,
        Option<tokio::task::JoinHandle<()>>,
    ),
    BootstrapError,
> {
    let mut challenge_task = None;
    let (chain, key) = match &config.tls {
        BootstrapTls::Operator { cert_pem, key_pem } => read_pem_pair(cert_pem, key_pem)?,
        BootstrapTls::Acme(acme) => {
            // R4a: the challenge ingress exists before we order.
            let (task, bound) =
                serve_challenge_ingress(config.acme_challenge_addr, config.acme.clone()).await?;
            tracing::info!(
                %bound,
                domain = %acme.domain,
                "acme http-01 ingress bound before ordering"
            );
            match crate::rtc_bootstrap_acme::obtain_certificate(acme, &config.acme).await {
                Ok(pair) => {
                    challenge_task = Some(task);
                    pair
                }
                Err(e) => {
                    // **A failed startup owns its port** (R4, round
                    // two). Dropping the handle DETACHES the task:
                    // the challenge socket stayed bound for the life
                    // of the process, and the review could not
                    // rebind it after a failed order. `abort`
                    // requests cancellation; only the await makes
                    // the listener gone by the time this returns.
                    abort_and_join(task).await;
                    return Err(e);
                }
            }
        }
    };
    match server_config(chain, key) {
        Ok(acceptor) => Ok((acceptor, challenge_task)),
        Err(e) => {
            if let Some(task) = challenge_task {
                abort_and_join(task).await;
            }
            Err(e)
        }
    }
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
/// parse, carry **this issuer's** signature over its own canonical
/// bytes, belong to this anchor's transport trust domain, and have
/// both lifetimes live — in that order.
fn check_credential(
    raw: &str,
    psk: &Psk,
    issuer: &crate::identity::EntityId,
) -> Result<BrowserBootstrapCredential, Box<Response>> {
    let credential = BrowserBootstrapCredential::decode(raw).map_err(|e| {
        Box::new(refuse(
            BootstrapRefusal::MalformedCredential,
            format!("the credential did not parse: {e}"),
        ))
    })?;
    // **Refusal REPORTING prefers the operator-actionable cause.**
    // A credential whose own stated deadlines have already passed is
    // reported as expired whether or not its signature verifies:
    // both facts are things its holder already knows, and "mint a
    // fresh invite" is the actionable one. Acceptance is a different
    // question and is answered below — nothing is ever accepted
    // without the issuer signature.
    if let Err(e) = credential.validate() {
        return Err(Box::new(refuse(
            BootstrapRefusal::ExpiredCredential,
            format!("the credential is not presentable: {e}"),
        )));
    }
    // **The issuer signature (R3).** Everything else reads fields the
    // credential asserts about itself; until this verifies, those
    // fields are the caller's claims. The review edited the two
    // deadlines on an expired credential and was admitted.
    credential.verify_issuer(issuer).map_err(|e| {
        Box::new(refuse(
            BootstrapRefusal::MalformedCredential,
            format!("the credential's issuer did not verify: {e}"),
        ))
    })?;
    // Domain next: telling a caller from another trust domain that
    // their nonce expired would send them to the wrong knob.
    credential.check_trust_domain(psk).map_err(|e| {
        Box::new(refuse(
            BootstrapRefusal::WrongTrustDomain,
            format!("this anchor does not serve that trust domain: {e}"),
        ))
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
    let credential = match check_credential(&request.credential, &state.psk, &state.issuer) {
        Ok(credential) => credential,
        Err(response) => return *response,
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
    // The incarnation is this listener's own counter for the
    // attempt, so two attempts that reuse a dialog id are still
    // distinct identities (R1).
    let incarnation = state.dialogs.fetch_add(1, Ordering::Relaxed);
    // Mint FIRST: the token's random bytes are what the offer's
    // signalling budget is charged to (R2), so it has to exist
    // before the offer is accepted.
    let Some((attempt_token, budget_id)) = state.attempts.mint(node_id, dialog, incarnation) else {
        return refuse(
            BootstrapRefusal::OfferRefused,
            "the anchor could not mint an attempt token",
        );
    };
    match state
        .node
        .accept_bootstrap_offer_keyed(budget_id, node_id, dialog, request.sdp)
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
                attempt_token,
                dialog,
                sdp,
                candidate,
            })
            .into_response()
        }
        Err(e) => {
            state.attempts.retire(&attempt_token);
            refuse(BootstrapRefusal::OfferRefused, e.to_string())
        }
    }
}

async fn get_anchor(State(state): State<AppState>) -> Response {
    Json(AnchorInfo {
        node_id: format!("{:#x}", state.node.node_id()),
        noise_pubkey: hex_of(state.node.public_key()),
        rtc_addr: state.node.rtc_public_addr().map(|a| a.to_string()),
        // The resolved announced STUN endpoint, not the configured
        // bind: one resolution rule, in the node, so this handler
        // and the announcement emission point cannot disagree.
        stun_addr: state.node.rtc_public_stun_addr().map(|a| a.to_string()),
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

/// The subprotocol that carries the attempt token (R1). A browser
/// cannot set arbitrary headers on a WebSocket handshake, but it can
/// name a subprotocol — and a subprotocol is a header, not a URL, so
/// the token stays out of proxy logs and browser history.
const ATTEMPT_SUBPROTOCOL_PREFIX: &str = "net-bootstrap-attempt.";

/// The token a handshake presents, from
/// `Sec-WebSocket-Protocol: net-bootstrap-attempt.<hex>`.
fn presented_token(headers: &HeaderMap) -> Option<String> {
    headers
        .get(axum::http::header::SEC_WEBSOCKET_PROTOCOL)
        .and_then(|v| v.to_str().ok())?
        .split(',')
        .map(str::trim)
        .find_map(|p| p.strip_prefix(ATTEMPT_SUBPROTOCOL_PREFIX))
        .map(str::to_string)
}

async fn get_trickle(
    State(state): State<AppState>,
    axum::Extension(authorized): axum::Extension<Authorized>,
    ws: WebSocketUpgrade,
) -> Response {
    // Origin AND the attempt token were both decided by this route's
    // layer, before the upgrade. Reaching this function means the
    // socket is authorized for exactly one attempt.
    let Authorized { attempt, token } = authorized;
    let protocol = format!("{ATTEMPT_SUBPROTOCOL_PREFIX}{token}");
    ws.protocols([protocol])
        .on_upgrade(move |socket| trickle_socket(socket, state, attempt, token))
}

/// What the trickle layer proved before the upgrade.
#[derive(Clone)]
struct Authorized {
    attempt: Attempt,
    token: String,
}

/// `(node_id, dialog)` from the trickle URI, without an extractor —
/// the layer runs before extraction.
fn trickle_ids(uri: &axum::http::Uri) -> Option<(u64, u64)> {
    let query = uri.query()?;
    let mut node = None;
    let mut dialog = None;
    for pair in query.split('&') {
        let (k, v) = pair.split_once('=')?;
        match k {
            "node_id" => node = parse_node_id(v),
            "dialog" => dialog = v.parse().ok(),
            _ => {}
        }
    }
    Some((node?, dialog?))
}

async fn trickle_socket(mut socket: WebSocket, state: AppState, attempt: Attempt, token: String) {
    use axum::extract::ws::CloseFrame;

    let Attempt {
        node_id,
        dialog,
        incarnation,
        socket: socket_gen,
        ..
    } = attempt;
    tracing::debug!(
        node = format!("{node_id:#x}"),
        dialog,
        incarnation,
        "trickle socket authorized by its attempt token"
    );

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
        // **R2: the same bounds the native ingress applies**, run by
        // the same function, and charged to the attempt's own
        // identity rather than the node id the caller claimed.
        match state
            .node
            .apply_bootstrap_candidate_checked(attempt.budget_id, node_id, dialog, candidate, mid)
            .await
        {
            Ok(()) => {}
            Err(e) => {
                // A candidate this anchor will not apply — over a
                // bound, or for a dialog it does not hold: typed
                // close, so the browser learns WHICH thing was wrong
                // rather than seeing a silent hang.
                let refusal = if e.to_string().contains("bound") || e.to_string().contains("budget")
                {
                    BootstrapRefusal::RateLimited
                } else {
                    BootstrapRefusal::UnknownDialog
                };
                let _ = socket
                    .send(Message::Close(Some(CloseFrame {
                        code: refusal.close_code(),
                        reason: e.to_string().into(),
                    })))
                    .await;
                state.attempts.retire_socket(&token, socket_gen);
                return;
            }
        }
    }
    // The browser went away before the channel opened; do not leave
    // the attempt holding a budget slot until its deadline.
    //
    // **Only the CURRENT socket generation may do this** (R1). The
    // token re-upgrades — a reconnecting browser reopens the same
    // attempt over a new socket — so "whoever holds the token" is
    // not enough: a stale socket's delayed close must not end its
    // successor's live attempt. `retire_socket` returns whether THIS
    // call owned the removal, so a superseded socket — or a late
    // close after the attempt was already retired — ends nothing.
    if state.attempts.retire_socket(&token, socket_gen) {
        state.node.end_bootstrap_dialog(node_id, dialog).await;
    }
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

    /// `Attempts`' keys are hex; the decimal-spelling checks below
    /// need the raw bytes back.
    fn hex_to_bytes(s: &str) -> Vec<u8> {
        (0..s.len() / 2)
            .map(|i| u8::from_str_radix(&s[i * 2..i * 2 + 2], 16).unwrap())
            .collect()
    }

    /// Review #14's witness: a no-bearer-secret print over the SDK
    /// types that carry one. `BrowserBootstrapCredential`'s
    /// hand-written `Debug` redacts the PSK one field above `invite`,
    /// so the invite's derived `Debug` used to print the
    /// proof-of-invite nonce straight through the redaction (MR#16's
    /// shape) — and `Attempts`' derived `Debug` printed its live
    /// attempt tokens as map keys. Every secret is checked in BOTH
    /// spellings a formatter can produce (the decimal `{:?}` of its
    /// raw bytes and its lowercase hex) against `{:?}` of the
    /// credential, its invite, its PSK, a response carrying an
    /// attempt token, and the live attempt table. The positive
    /// control — root, bootstrap_url and dialog still print — proves
    /// the rendering reached the very fields the secrets sit beside,
    /// so a silent over-redaction cannot pass as a fix.
    #[test]
    fn the_invite_nonce_psk_bearer_and_attempt_tokens_never_appear_in_debug() {
        use crate::enrollment::InviteToken;
        use crate::identity::Identity;
        use std::time::Duration;

        const NOW: u64 = 1_700_000_000;
        let issuer = Identity::from_seed([0x2Au8; 32]);
        let root = Identity::from_seed([0x2Bu8; 32]).entity_id().clone();
        let invite = InviteToken::mint_at(
            &root,
            "rendezvous.example:8443",
            Duration::from_secs(600),
            NOW,
        );
        let credential = BrowserBootstrapCredential::mint_at(
            &issuer,
            invite,
            [7u8; 32],
            Psk::new([0xA5u8; 32]),
            "https://anchor.example/rtc",
            Duration::from_secs(30 * 86_400),
            NOW,
        );
        let attempts = Attempts::default();
        let (token, _budget) = attempts.mint(42, 4_242_424_242, 3).expect("mint");
        let response = OfferResponse {
            attempt_token: token.clone(),
            dialog: 4_242_424_242,
            sdp: "v=0".to_string(),
            candidate: "candidate:1".to_string(),
        };
        let rendered = [
            format!("{credential:?}"),
            format!("{:?}", credential.invite),
            format!("{:?}", credential.psk),
            format!("{response:?}"),
            format!("{attempts:?}"),
        ];
        let all = rendered.join("\n");

        let nonce = credential.invite.nonce;
        let psk = *credential.psk.expose_bytes();
        let token_raw = hex_to_bytes(&token);
        for (what, bytes, hex) in [
            ("invite nonce", nonce.as_slice(), hex_of(&nonce)),
            ("PSK", psk.as_slice(), hex_of(&psk)),
            ("attempt token", token_raw.as_slice(), token.clone()),
        ] {
            assert!(!all.contains(&hex), "the {what} leaked into Debug as hex");
            assert!(
                !all.contains(&format!("{bytes:?}")),
                "the {what} leaked into Debug in its decimal `{{:?}}` spelling"
            );
        }
        // …and the whole encoded bearer credential, prefix and body.
        let bearer = credential.encode();
        assert!(
            !all.contains(&bearer),
            "the encoded bearer leaked into Debug"
        );
        let (_, body) = bearer.split_once(':').expect("prefixed bearer");
        assert!(
            !all.contains(body),
            "the encoded bearer body leaked into Debug"
        );

        // Positive control: the same renderings still name the public
        // fields the secrets sit beside. The dialog assertions also
        // prove the attempt map's entry was rendered at all — without
        // that, the token's absence above would prove nothing.
        assert!(
            rendered[0].contains(&hex_of(root.as_bytes())),
            "root is no longer diagnosable through the credential"
        );
        assert!(
            rendered[1].contains(&hex_of(root.as_bytes())),
            "root is no longer diagnosable through the invite"
        );
        assert!(
            rendered[0].contains("https://anchor.example/rtc"),
            "bootstrap_url is no longer diagnosable"
        );
        assert!(
            rendered[3].contains("4242424242"),
            "dialog is no longer diagnosable through the response"
        );
        assert!(
            rendered[4].contains("4242424242"),
            "dialog is no longer diagnosable through the attempt table"
        );
    }
}

// ===================================================================
// The anchor directory service (R6)
// ===================================================================

/// The nRPC service an anchor serves so operator tooling can read
/// the anchors IT knows about.
pub const ANCHOR_DIRECTORY_SERVICE: &str = "net.mesh.anchors";

/// One row of [`ANCHOR_DIRECTORY_SERVICE`]'s reply.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnchorDirectoryRow {
    /// The anchor's node id, hex.
    pub node: String,
    /// Its announced public RTC socket — the endpoint a browser
    /// aims ICE at, and the target of the diagnostic STUN probe.
    pub rtc_addr: Option<String>,
    /// Its **separately announced STUN endpoint** (Stage 6), when
    /// configured: a second UDP endpoint, distinct from
    /// [`Self::rtc_addr`], that a connection pairing with this
    /// anchor gathers against. Absent — and omitted from the JSON —
    /// on an anchor that configured none, which is every anchor
    /// that has not opted in.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rtc_stun_addr: Option<String>,
    /// Its announced bootstrap listener URL.
    pub rtc_bootstrap: Option<String>,
    /// Its announced Noise static public key, hex.
    pub noise_pubkey: Option<String>,
}

/// Serve the anchor directory on `mesh`.
///
/// **Why a service rather than a query (R6).** The two address
/// fields are `#[serde(skip)]` projections in the capability fold —
/// every node fills them from the announcement it ingested itself,
/// and they deliberately do not travel in fold envelopes. So a
/// remote fold query cannot carry them, and a freshly attached
/// client has not ingested anything yet: the CLI's own view is
/// empty by construction, which is exactly the defect the review
/// found. The node that HAS ingested them answers instead.
///
/// Read-only, and it exposes nothing an announcement did not already
/// broadcast in the clear.
pub fn serve_anchor_directory(mesh: &crate::Mesh) -> Result<crate::mesh_rpc::ServeHandle, String> {
    let node = Arc::clone(mesh.node());
    mesh.serve_rpc_raw_bytes(ANCHOR_DIRECTORY_SERVICE, move |_request| {
        let node = Arc::clone(&node);
        async move {
            let rows: Vec<AnchorDirectoryRow> = node
                .rtc_anchors()
                .into_iter()
                .map(|row| AnchorDirectoryRow {
                    node: format!("{:#x}", row.node_id),
                    rtc_addr: row.rtc_addr.map(|a| a.to_string()),
                    rtc_stun_addr: row.rtc_stun_addr,
                    rtc_bootstrap: row.rtc_bootstrap,
                    noise_pubkey: row.noise_pubkey.as_ref().map(|k| hex_of(k)),
                })
                .collect();
            serde_json::to_vec(&rows).map_err(|e| e.to_string())
        }
    })
    .map_err(|e| e.to_string())
}

// ===================================================================
// The anchor ICE-stats service (Stage 6 slice 5)
// ===================================================================

/// The nRPC service an anchor serves so operator tooling can read
/// **its own** ICE attempt ledger — plan §10's
/// `ice_direct / ice_attempted` field telemetry.
pub const ANCHOR_ICE_STATS_SERVICE: &str = "net.mesh.anchor.ice";

/// [`ANCHOR_ICE_STATS_SERVICE`]'s reply: the answering anchor's ICE
/// attempt ledger.
///
/// **The denominator is attempts, not sessions.** [`Self::attempted`]
/// counts direct-path attempts — one per signalling dialog the
/// answering node drove, which on an anchor includes the bootstrap
/// dialog of every browser that arrived. A caller that retries after
/// a timeout spends two attempts; an ICE restart inside one dialog
/// is one.
///
/// The ratio is **not** a session success rate: a relayed session is
/// not a failed one. `direct + relayed + failed + pending ==
/// attempted` holds here, and plan §10's fourth outcome
/// `udp_blocked` is absent because a node signalling over UDP cannot
/// have UDP blocked — that term belongs to the browser leaf, which
/// can establish it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnchorIceStats {
    /// The answering node's id, hex — the ledger is its own and
    /// cannot be read across the mesh, so the reply names whose it
    /// is.
    pub node: String,
    /// `false` when the answering node has no RTC driver, in which
    /// case it keeps **no attempt ledger at all** and every counter
    /// below is a placeholder zero rather than an observation.
    ///
    /// This flag is the difference between "nothing has gone direct
    /// here" and "this node does not do direct paths", which a bare
    /// row of zeros cannot express.
    pub rtc_configured: bool,
    /// **The denominator.** Direct-path attempts started: one per
    /// signalling dialog.
    pub attempted: u64,
    /// Attempts that ended with an installed direct RTC endpoint.
    pub direct: u64,
    /// Attempts that reached their deadline with ICE never
    /// connected. For a peer dialog: the pair stayed on the anchor —
    /// a supported disposition, not a failure.
    pub relayed: u64,
    /// Attempts that ended without a direct path for a reason other
    /// than their deadline.
    pub failed: u64,
    /// Attempts counted in the denominator that have not reached any
    /// outcome yet. The four terms sum to [`Self::attempted`] only
    /// when this is zero.
    pub pending: u64,
    /// `direct / attempted`, or `null` when nothing has been
    /// attempted. **`null` is not zero**: a node that has never
    /// attempted a direct path has no direct-path ratio, and `0.0`
    /// would report total failure where nothing has happened.
    pub direct_ratio: Option<f64>,
}

/// Serve the anchor's own ICE attempt ledger on `mesh`.
///
/// **Why a service and not a local read.** Same reason the anchor
/// directory is one (R6): the CLI's in-process Deck client has no
/// `MeshNode`, so reading this locally would report the ledger of a
/// node the operator just created and which has attempted nothing.
/// An attempt ledger is also not announced — it is not fold state —
/// so the only node that can answer for it is the node that owns it.
///
/// Read-only. It publishes four counters about this node's own
/// direct-path attempts and nothing about any peer.
pub fn serve_anchor_ice_stats(mesh: &crate::Mesh) -> Result<crate::mesh_rpc::ServeHandle, String> {
    let node = Arc::clone(mesh.node());
    mesh.serve_rpc_raw_bytes(ANCHOR_ICE_STATS_SERVICE, move |_request| {
        let node = Arc::clone(&node);
        async move {
            let ledger = node.rtc_ice_stats();
            let reply = AnchorIceStats {
                node: format!("{:#x}", node.node_id()),
                rtc_configured: ledger.is_some(),
                attempted: ledger.as_ref().map(|s| s.attempted).unwrap_or(0),
                direct: ledger.as_ref().map(|s| s.direct).unwrap_or(0),
                relayed: ledger.as_ref().map(|s| s.relayed).unwrap_or(0),
                failed: ledger.as_ref().map(|s| s.failed).unwrap_or(0),
                pending: ledger.as_ref().map(|s| s.pending()).unwrap_or(0),
                direct_ratio: ledger.as_ref().and_then(|s| s.direct_ratio()),
            };
            serde_json::to_vec(&reply).map_err(|e| e.to_string())
        }
    })
    .map_err(|e| e.to_string())
}
