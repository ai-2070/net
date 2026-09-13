//! Stage 4b — the **Chromium harness**.
//!
//! One command starts a real native anchor (`net-mesh` with
//! `webrtc`), the real Stage 4b bootstrap listener
//! (`net_sdk::rtc_bootstrap::serve_bootstrap`) over a locally issued
//! CA/leaf certificate, a real `BrowserBootstrapCredential`, a real
//! headless Chromium, and drives the six §12 admission witnesses, the
//! MITM witness and the mDNS measurement through that browser. Every
//! observable is read **on the anchor** (`RtcStats`,
//! `peer_is_provisional`, `provisional_count`, handler invocation
//! counts) and reported as one `RTCB PASS`/`RTCB FAIL` line per
//! witness. Any failure exits non-zero.
//!
//! ```text
//!   cargo run --release --manifest-path \
//!     net/crates/net/tests/rtc_browser/runner/Cargo.toml
//! ```
//!
//! # TLS
//!
//! The listener serves a leaf issued by a CA this harness generates
//! and installs into the browser's trust store (Windows: the
//! current user's `Root` store via `certutil -addstore -user Root`,
//! removed again on exit; Linux: the NSS database Chromium reads,
//! `certutil -d sql:$HOME/.pki/nssdb`). **There is no
//! `--ignore-certificate-errors` anywhere**, and there cannot be:
//! `BootstrapTls` has no self-signed variant, so a harness that
//! skipped verification would be proving something no deployment
//! can rely on.
//!
//! # WHAT THE BROWSER DOES AND DOES NOT DO — read this before
//! # believing a witness
//!
//! The browser owns the **transport**, end to end:
//!
//! - it POSTs the credential and a real SDP offer to the real
//!   `POST /rtc/offer`, opens the real `wss /rtc/trickle`, and
//!   exchanges ICE candidates over it;
//! - it runs the NKpsk0 handshake as **initiator** against the key
//!   the *credential* pins, using `net-mesh-wire` compiled to wasm —
//!   the production wire crate, not a spike copy;
//! - it builds and seals every outbound Net packet in wasm
//!   (`PacketBuilder::build_subprotocol`, ChaCha20-Poly1305) and
//!   opens every inbound one.
//!
//! The browser is **not** an nRPC client. That is Stage 5. The
//! *payload bytes* of the enrollment REQUEST, the membership
//! Subscribe, the capability announcement and the routing envelope
//! are constructed **on the native side with the production
//! encoders** (`EventMeta`, `encode_rpc_route`,
//! `RpcRequestPayload`, `channel::membership::encode`,
//! `CapabilityAnnouncement` via the node's own announcement bytes,
//! `RoutingHeader`) and handed to the page over plain HTTP; the page
//! Noise-encrypts them, sends them on the DataChannel, and posts the
//! decrypted reply back. So: **the browser exercises the real
//! transport, not a real nRPC client.** Every §12 gate under test
//! sits above the transport and below nRPC's client half, which is
//! exactly the surface this arrangement exercises honestly.
//!
//! # The page is served over plain `http://localhost`
//!
//! `http://localhost` is a secure context, so `RTCPeerConnection` and
//! wasm work there. Only the *page* rides that sibling server; every
//! **anchor** endpoint the page talks to is the real HTTPS listener.

use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr, SocketAddr, UdpSocket};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use axum::extract::State;
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use bytes::Bytes;
use serde::{Deserialize, Serialize};
use tokio::sync::{mpsc, oneshot, Mutex};

use net::adapter::net::behavior::capability::CapabilitySet;
use net::adapter::net::channel::membership::{self, MembershipMsg};
use net::adapter::net::channel::{ChannelId, ChannelName, SUBPROTOCOL_CHANNEL_MEMBERSHIP};
use net::adapter::net::cortex::{
    encode_rpc_route, EventMeta, RpcContext, RpcHandler, RpcHandlerError, RpcRequestPayload,
    RpcResponsePayload, RpcStatus, DISPATCH_RPC_REQUEST, DISPATCH_RPC_RESPONSE, EVENT_META_SIZE,
    RPC_FRAME_BODY_OFFSET, RPC_ROUTE_V1_SIZE,
};
use net::adapter::net::rtc::{enroll_reply_channel, RtcConfig, ENROLL_SERVICE};
use net::adapter::net::{EntityKeypair, MeshNode, MeshNodeConfig, PeerAddr};
use net_sdk::bootstrap_credential::{BrowserBootstrapCredential, Psk};
use net_sdk::enrollment::InviteToken;
use net_sdk::identity::Identity;
use net_sdk::rtc_bootstrap::{serve_bootstrap, BootstrapConfig, BootstrapTls};

/// The transport trust domain's PSK. One value for the anchor, the
/// impostor and every credential: the MITM witness must fail on the
/// **static key**, not on a trust-domain mismatch that would refuse
/// the credential before a handshake ever ran.
const PSK: [u8; 32] = [0x4Bu8; 32];

/// `SUBPROTOCOL_CAPABILITY_ANN`, re-stated because the constant is
/// behind `behavior::broadcast`.
const SUBPROTOCOL_CAPABILITY_ANN: u16 = 0x0C00;

/// The service a provisional peer may NOT call (§12 gate 5).
const OTHER_SERVICE: &str = "app.orders.place";
/// The service an ENROLLED peer may not invoke without a grant.
const PROTECTED_SERVICE: &str = "org.protected.invoke";
/// `send_transit_probe_for_test`'s stream id, reproduced so the
/// browser's envelope is the same shape the 4a witness sends.
const TRANSIT_STREAM_ID: u64 = 0x0F00;

// ===================================================================
// Verdicts
// ===================================================================

#[derive(Debug)]
struct Verdict {
    name: &'static str,
    pass: bool,
    detail: String,
}

#[derive(Default)]
struct Ledger(Vec<Verdict>);

impl Ledger {
    fn record(&mut self, name: &'static str, pass: bool, detail: impl Into<String>) {
        let detail = detail.into();
        println!(
            "RTCB {} {name} — {detail}",
            if pass { "PASS" } else { "FAIL" }
        );
        self.0.push(Verdict {
            name,
            pass,
            detail,
        });
    }

    fn failed(&self) -> usize {
        self.0.iter().filter(|v| !v.pass).count()
    }
}

// ===================================================================
// nRPC handlers registered on the anchor
// ===================================================================

/// `JoinOutcome`'s pinned wire form (`sdk/src/enrollment.rs`:
/// `b"NMO1"`, then `0` Admitted / `1` Rejected). The core's promotion
/// gate parses this; the SDK pins the same prefix.
fn admitted_outcome() -> Bytes {
    let mut buf = Vec::from(*b"NMO1");
    buf.push(0);
    let chain = b"harness-delegation-chain";
    buf.extend_from_slice(&(chain.len() as u32).to_le_bytes());
    buf.extend_from_slice(chain);
    Bytes::from(buf)
}

struct Enrollment;

#[async_trait::async_trait]
impl RpcHandler for Enrollment {
    async fn call(&self, _ctx: RpcContext) -> Result<RpcResponsePayload, RpcHandlerError> {
        Ok(RpcResponsePayload {
            status: RpcStatus::Ok,
            headers: vec![],
            body: admitted_outcome(),
        })
    }
}

/// A handler whose **invocation count** is the observable.
struct Counting(Arc<AtomicUsize>);

#[async_trait::async_trait]
impl RpcHandler for Counting {
    async fn call(&self, _ctx: RpcContext) -> Result<RpcResponsePayload, RpcHandlerError> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Ok(RpcResponsePayload {
            status: RpcStatus::Ok,
            headers: vec![],
            body: Bytes::from_static(b"served"),
        })
    }
}

// ===================================================================
// The step protocol the page executes
// ===================================================================

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum Step {
    /// Offer → trickle → DataChannel → NKpsk0, against `base`.
    Connect {
        id: u64,
        session: String,
        base: String,
        node_id: String,
        psk: String,
        anchor_pub: String,
        anchor_node_id: String,
        credential: String,
        /// `stun:host:port`, or absent for `iceServers: []`.
        stun: Option<String>,
        /// How long to wait for the DataChannel + msg2.
        timeout_ms: u64,
        /// A handshake that must FAIL (the MITM witness).
        expect_failure: bool,
    },
    /// Build one packet in wasm and send it on the DataChannel.
    Send {
        id: u64,
        session: String,
        /// Hex bytes prepended verbatim to the built packet — the
        /// routing envelope header, when there is one.
        prefix: String,
        stream_id: String,
        subprotocol: u16,
        channel_hash: u16,
        origin_hash: String,
        reliable: bool,
        payload: String,
    },
    /// Wait for one inbound frame on `subprotocol` and return it.
    /// `0` is the event plane; a control subprotocol (membership,
    /// capability announcement) carries its own id. Filtering here
    /// is what keeps a membership Ack from being mistaken for an
    /// nRPC RESPONSE.
    Expect {
        id: u64,
        session: String,
        subprotocol: u16,
        timeout_ms: u64,
    },
    /// `RTCPeerConnection.getStats()` — the selected candidate pair.
    Stats { id: u64, session: String },
    /// Stop polling.
    Done { id: u64 },
}

#[derive(Debug, Clone, Deserialize, Default)]
struct StepResult {
    #[serde(default)]
    ok: bool,
    #[serde(default)]
    error: Option<String>,
    #[serde(default)]
    session_id: Option<String>,
    #[serde(default)]
    open_ms: Option<f64>,
    #[serde(default)]
    frame: Option<String>,
    #[serde(default)]
    stats: Option<serde_json::Value>,
    #[serde(default)]
    info: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ResultBody {
    id: u64,
    #[serde(flatten)]
    result: StepResult,
}

type Pending = Arc<Mutex<HashMap<u64, oneshot::Sender<StepResult>>>>;

#[derive(Clone)]
struct PageState {
    dist: PathBuf,
    page: PathBuf,
    steps: Arc<Mutex<mpsc::Receiver<(Step, oneshot::Sender<StepResult>)>>>,
    pending: Pending,
}

struct Script {
    tx: mpsc::Sender<(Step, oneshot::Sender<StepResult>)>,
    next_id: u64,
}

impl Script {
    async fn run(&mut self, mut step: Step) -> StepResult {
        let id = self.next_id;
        self.next_id += 1;
        match &mut step {
            Step::Connect { id: s, .. }
            | Step::Send { id: s, .. }
            | Step::Expect { id: s, .. }
            | Step::Stats { id: s, .. }
            | Step::Done { id: s } => *s = id,
        }
        let (tx, rx) = oneshot::channel();
        if self.tx.send((step, tx)).await.is_err() {
            return StepResult {
                ok: false,
                error: Some("the page server is gone".into()),
                ..Default::default()
            };
        }
        match tokio::time::timeout(Duration::from_secs(120), rx).await {
            Ok(Ok(r)) => r,
            Ok(Err(_)) => StepResult {
                ok: false,
                error: Some("the step was dropped".into()),
                ..Default::default()
            },
            Err(_) => StepResult {
                ok: false,
                error: Some("the browser did not answer this step in 120 s".into()),
                ..Default::default()
            },
        }
    }
}

/// The fixed half of a `Connect` step for the primary anchor, so the
/// witnesses below read as one line each.
struct Connector {
    base: String,
    credential: String,
    anchor_pub: String,
    psk: String,
    anchor_node: u64,
    stun: Option<String>,
}

impl Connector {
    async fn connect(&self, script: &mut Script, session: &str, node_id: u64) -> StepResult {
        script
            .run(Step::Connect {
                id: 0,
                session: session.to_string(),
                base: self.base.clone(),
                node_id: format!("{node_id:016x}"),
                psk: self.psk.clone(),
                anchor_pub: self.anchor_pub.clone(),
                anchor_node_id: format!("{:016x}", self.anchor_node),
                credential: self.credential.clone(),
                stun: self.stun.clone(),
                timeout_ms: 30_000,
                expect_failure: false,
            })
            .await
    }
}

// ===================================================================
// Page server
// ===================================================================

async fn serve_page(state: PageState) -> std::io::Result<(SocketAddr, tokio::task::JoinHandle<()>)> {
    let router = Router::new()
        .route("/", get(index))
        .route("/app.js", get(app_js))
        .route("/leaf.js", get(leaf_js))
        .route("/leaf_bg.wasm", get(leaf_wasm))
        .route("/harness/step", get(next_step))
        .route("/harness/result", post(step_result))
        .route("/harness/log", post(browser_log))
        .with_state(state);
    let listener = tokio::net::TcpListener::bind(SocketAddr::new(
        IpAddr::V4(Ipv4Addr::LOCALHOST),
        0,
    ))
    .await?;
    let addr = listener.local_addr()?;
    let task = tokio::spawn(async move {
        let _ = axum::serve(listener, router).await;
    });
    Ok((addr, task))
}

fn file_response(path: &Path, content_type: &str) -> Response {
    match std::fs::read(path) {
        Ok(bytes) => (
            StatusCode::OK,
            [(header::CONTENT_TYPE, content_type)],
            bytes,
        )
            .into_response(),
        Err(e) => (
            StatusCode::NOT_FOUND,
            format!("{}: {e}", path.display()),
        )
            .into_response(),
    }
}

async fn index(State(s): State<PageState>) -> Response {
    file_response(&s.page.join("index.html"), "text/html; charset=utf-8")
}
async fn app_js(State(s): State<PageState>) -> Response {
    file_response(&s.page.join("app.js"), "text/javascript; charset=utf-8")
}
async fn leaf_js(State(s): State<PageState>) -> Response {
    file_response(&s.dist.join("leaf.js"), "text/javascript; charset=utf-8")
}
async fn leaf_wasm(State(s): State<PageState>) -> Response {
    file_response(&s.dist.join("leaf_bg.wasm"), "application/wasm")
}

async fn next_step(State(s): State<PageState>) -> Response {
    let mut rx = s.steps.lock().await;
    match tokio::time::timeout(Duration::from_secs(25), rx.recv()).await {
        Ok(Some((step, reply))) => {
            let id = match &step {
                Step::Connect { id, .. }
                | Step::Send { id, .. }
                | Step::Expect { id, .. }
                | Step::Stats { id, .. }
                | Step::Done { id } => *id,
            };
            s.pending.lock().await.insert(id, reply);
            Json(step).into_response()
        }
        // No step ready, or the script finished: tell the page to
        // poll again rather than leaving it hanging.
        _ => Json(serde_json::json!({ "kind": "idle", "id": 0, "millis": 50 })).into_response(),
    }
}

async fn step_result(State(s): State<PageState>, Json(body): Json<ResultBody>) -> StatusCode {
    if let Some(tx) = s.pending.lock().await.remove(&body.id) {
        let _ = tx.send(body.result);
    }
    StatusCode::OK
}

async fn browser_log(body: String) -> StatusCode {
    for line in body.lines() {
        println!("[browser] {line}");
    }
    StatusCode::OK
}

// ===================================================================
// Certificates
// ===================================================================

struct Ca {
    ca_pem: String,
    cert_pem_path: PathBuf,
    key_pem_path: PathBuf,
    ca_pem_path: PathBuf,
}

fn issue_localhost_certificate(dir: &Path) -> Result<Ca, String> {
    let mut ca_params =
        rcgen::CertificateParams::new(Vec::<String>::new()).map_err(|e| e.to_string())?;
    ca_params.is_ca = rcgen::IsCa::Ca(rcgen::BasicConstraints::Unconstrained);
    ca_params
        .distinguished_name
        .push(rcgen::DnType::CommonName, CA_COMMON_NAME);
    let ca_key = rcgen::KeyPair::generate().map_err(|e| e.to_string())?;
    let ca = ca_params
        .clone()
        .self_signed(&ca_key)
        .map_err(|e| e.to_string())?;
    let issuer = rcgen::Issuer::new(ca_params, ca_key);

    let mut leaf_params = rcgen::CertificateParams::new(vec!["localhost".to_string()])
        .map_err(|e| e.to_string())?;
    leaf_params
        .distinguished_name
        .push(rcgen::DnType::CommonName, "localhost");
    let leaf_key = rcgen::KeyPair::generate().map_err(|e| e.to_string())?;
    let leaf = leaf_params
        .signed_by(&leaf_key, &issuer)
        .map_err(|e| e.to_string())?;

    let ca_pem = ca.pem();
    let cert_pem_path = dir.join("cert.pem");
    let key_pem_path = dir.join("key.pem");
    let ca_pem_path = dir.join("ca.pem");
    std::fs::write(&cert_pem_path, format!("{}{}", leaf.pem(), ca_pem))
        .map_err(|e| e.to_string())?;
    std::fs::write(&key_pem_path, leaf_key.serialize_pem()).map_err(|e| e.to_string())?;
    std::fs::write(&ca_pem_path, &ca_pem).map_err(|e| e.to_string())?;
    Ok(Ca {
        ca_pem,
        cert_pem_path,
        key_pem_path,
        ca_pem_path,
    })
}

const CA_COMMON_NAME: &str = "net-mesh stage-4b harness CA";
const NSS_NICKNAME: &str = "net-mesh-stage4b-harness-ca";

/// Install the CA where **this browser** will look for it. Returns a
/// description of what was done, and whether an uninstall is owed.
fn install_ca(ca: &Ca) -> (String, bool) {
    if cfg!(windows) {
        // Chromium on Windows reads the OS trust store; a fresh
        // `--user-data-dir` does not isolate roots. The current
        // user's store is the narrowest thing that works, and it is
        // removed again in `uninstall_ca`.
        //
        // `-addstore -user Root` raises the system's
        // add-a-root-certificate confirmation, which on a
        // non-interactive desktop can come back as
        // `ERROR_CANCELLED` (0x800704C7). That is transient, so it
        // is retried — but never worked around: if every attempt
        // fails, the harness stops rather than weakening TLS.
        let mut last = String::from("FAILED: certutil never ran");
        for attempt in 1..=3 {
            // A leftover from a previous run makes the add a no-op
            // that still prompts; clear it first.
            uninstall_ca();
            match Command::new("certutil")
                .args(["-addstore", "-user", "Root"])
                .arg(&ca.ca_pem_path)
                .output()
            {
                Ok(o) if o.status.success() => {
                    return (
                        format!(
                            "certutil -addstore -user Root on attempt {attempt} \
                             (current user's Root store; removed on exit)"
                        ),
                        true,
                    );
                }
                Ok(o) => {
                    last = format!(
                        "FAILED: certutil -addstore -user Root exited {:?}: {}",
                        o.status.code(),
                        String::from_utf8_lossy(&o.stdout).trim()
                    );
                }
                Err(e) => last = format!("FAILED: certutil not runnable: {e}"),
            }
            std::thread::sleep(Duration::from_millis(500));
        }
        (last, false)
    } else {
        let home = std::env::var("HOME").unwrap_or_else(|_| "/root".into());
        let db = format!("{home}/.pki/nssdb");
        let _ = std::fs::create_dir_all(&db);
        // An absent database is created empty; an existing one is
        // left alone.
        let _ = Command::new("certutil")
            .args(["-N", "--empty-password", "-d", &format!("sql:{db}")])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
        let out = Command::new("certutil")
            .args([
                "-d",
                &format!("sql:{db}"),
                "-A",
                "-t",
                "C,,",
                "-n",
                NSS_NICKNAME,
                "-i",
            ])
            .arg(&ca.ca_pem_path)
            .output();
        match out {
            Ok(o) if o.status.success() => (
                format!("certutil -d sql:{db} -A -t C,, (Chromium's NSS store; removed on exit)"),
                true,
            ),
            Ok(o) => (
                format!(
                    "FAILED: certutil -A exited {:?}: {}",
                    o.status.code(),
                    String::from_utf8_lossy(&o.stderr)
                ),
                false,
            ),
            Err(e) => (
                format!("FAILED: certutil (libnss3-tools) not runnable: {e}"),
                false,
            ),
        }
    }
}

fn uninstall_ca() {
    if cfg!(windows) {
        let _ = Command::new("certutil")
            .args(["-delstore", "-user", "Root", CA_COMMON_NAME])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    } else {
        let home = std::env::var("HOME").unwrap_or_else(|_| "/root".into());
        let _ = Command::new("certutil")
            .args([
                "-d",
                &format!("sql:{home}/.pki/nssdb"),
                "-D",
                "-n",
                NSS_NICKNAME,
            ])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
}

// ===================================================================
// Build the wasm leaf
// ===================================================================

fn build_leaf(root: &Path) -> Result<PathBuf, String> {
    let leaf = root.join("leaf");
    let dist = leaf.join("dist");
    println!("[harness] building the wasm leaf against net-mesh-wire");
    let status = Command::new("cargo")
        .current_dir(&leaf)
        .args(["build", "--release", "--target", "wasm32-unknown-unknown"])
        .status()
        .map_err(|e| format!("cargo: {e}"))?;
    if !status.success() {
        return Err("the wasm leaf did not build".into());
    }
    let wasm = leaf
        .join("target/wasm32-unknown-unknown/release/rtc_browser_leaf.wasm");
    let status = Command::new("wasm-bindgen")
        .current_dir(&leaf)
        .args(["--target", "web", "--out-dir"])
        .arg(&dist)
        .args(["--out-name", "leaf"])
        .arg(&wasm)
        .status()
        .map_err(|e| {
            format!("wasm-bindgen: {e} (install with `cargo install wasm-bindgen-cli --version 0.2.128`)")
        })?;
    if !status.success() {
        return Err("wasm-bindgen failed (CLI/crate version mismatch?)".into());
    }
    Ok(dist)
}

// ===================================================================
// Frame payloads — built with the PRODUCTION encoders
// ===================================================================

/// `MeshNode::publish_stream_id`, which is `pub(super)`. The formula
/// is the wire contract for a channel-keyed publisher stream; a drift
/// here would show up as the anchor never dispatching the frame, so
/// the witness fails loudly rather than silently.
fn publish_stream_id(channel: &ChannelId) -> u64 {
    0x0001_0000_0000_0000 | channel.hash()
}

struct RpcFrame {
    stream_id: u64,
    channel_hash_u16: u16,
    payload: Vec<u8>,
}

/// The exact bytes `MeshNode::publish_rpc_request_unsubscribed`
/// publishes: `EventMeta ‖ RpcRouteV1 ‖ RpcRequestPayload`.
fn rpc_request_frame(service: &str, origin_hash: u64, call_id: u64, body: &[u8]) -> RpcFrame {
    let name = ChannelName::new(&format!("{service}.requests")).expect("a valid channel name");
    let channel = ChannelId::new(name);
    let hash = channel.hash();
    let req = RpcRequestPayload {
        service: service.to_string(),
        deadline_ns: 0,
        flags: 0,
        headers: Vec::new(),
        body: Bytes::copy_from_slice(body),
    };
    let meta = EventMeta::new(DISPATCH_RPC_REQUEST, 0, origin_hash, call_id, 0);
    let mut buf = Vec::with_capacity(EVENT_META_SIZE + RPC_ROUTE_V1_SIZE + req.encoded_len());
    buf.extend_from_slice(&meta.to_bytes());
    encode_rpc_route(&mut buf, hash);
    req.encode_into(&mut buf);
    RpcFrame {
        stream_id: publish_stream_id(&channel),
        channel_hash_u16: hash as u16,
        payload: buf,
    }
}

/// A membership Subscribe, through `channel::membership::encode`.
fn subscribe_payload(channel: &str, nonce: u64) -> Vec<u8> {
    membership::encode(&MembershipMsg::Subscribe {
        channel: ChannelName::new(channel).expect("a valid channel name"),
        nonce,
        token: None,
        queue_group: None,
    })
}

/// A routing envelope header addressed at `dest_node_id`, exactly as
/// `send_transit_probe_for_test` builds it.
fn routing_prefix(dest_node_id: u64, src_node_id: u64) -> Vec<u8> {
    net_wire::route_codec::RoutingHeader::new(dest_node_id, src_node_id as u32, 8)
        .to_bytes()
        .to_vec()
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn unhex(s: &str) -> Vec<u8> {
    (0..s.len())
        .step_by(2)
        .filter_map(|i| u8::from_str_radix(s.get(i..i + 2)?, 16).ok())
        .collect()
}

// ===================================================================
// Nodes
// ===================================================================

async fn spawn_node(rtc: Option<RtcConfig>) -> Arc<MeshNode> {
    let mut cfg = MeshNodeConfig::new("127.0.0.1:0".parse().expect("addr"), PSK);
    cfg.rtc = rtc;
    let node = Arc::new(
        MeshNode::new(EntityKeypair::generate(), cfg)
            .await
            .expect("MeshNode::new"),
    );
    node.start_arc();
    node
}

fn anchor_rtc(bind: SocketAddr) -> RtcConfig {
    RtcConfig {
        serve_bootstrap: true,
        serve_stun: true,
        // **This must be strictly larger than the page's per-step
        // budget (30 s).** R4-A made the attempt deadline ONE
        // absolute budget covering DataChannel open + Noise + the
        // fenced install, and it starts when the offer is accepted —
        // the same instant the page starts its own wait. With both
        // set to 30 s, a slow ICE on a loaded runner eats the
        // anchor's Noise budget, the completion owner correctly
        // retires the attempt, and the page reports
        // `timeout: noise msg2` — a symptom that cannot be told
        // apart from an anchor that never answered. That is the CI
        // failure in run 34737303... at head f44088908 (2 of 9
        // witnesses).
        //
        // 90 s keeps the anchor's budget three times the page's, so
        // a `noise msg2` timeout means what it says.
        ice_deadline: Duration::from_secs(90),
        ..RtcConfig::new().with_bind_addr(bind)
    }
}

/// The host's routable IPv4, discovered without sending anything: a
/// connected UDP socket only installs a route.
fn lan_ipv4() -> Option<Ipv4Addr> {
    let sock = UdpSocket::bind("0.0.0.0:0").ok()?;
    sock.connect("8.8.8.8:80").ok()?;
    match sock.local_addr().ok()?.ip() {
        IpAddr::V4(v4) if !v4.is_loopback() && !v4.is_unspecified() => Some(v4),
        _ => None,
    }
}

// ===================================================================
// Chromium
// ===================================================================

fn find_chromium(explicit: Option<String>) -> Option<PathBuf> {
    if let Some(p) = explicit {
        return Some(PathBuf::from(p));
    }
    if let Ok(p) = std::env::var("CHROME_PATH") {
        if !p.is_empty() {
            return Some(PathBuf::from(p));
        }
    }
    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Ok(local) = std::env::var("LOCALAPPDATA") {
        let root = PathBuf::from(local).join("ms-playwright");
        if let Ok(entries) = std::fs::read_dir(&root) {
            let mut dirs: Vec<PathBuf> = entries
                .filter_map(|e| e.ok())
                .map(|e| e.path())
                .filter(|p| {
                    p.file_name()
                        .and_then(|n| n.to_str())
                        .is_some_and(|n| n.starts_with("chromium-"))
                })
                .collect();
            dirs.sort();
            dirs.reverse();
            for d in dirs {
                candidates.push(d.join("chrome-win64").join("chrome.exe"));
            }
        }
    }
    if let Ok(home) = std::env::var("HOME") {
        let root = PathBuf::from(&home).join(".cache/ms-playwright");
        if let Ok(entries) = std::fs::read_dir(&root) {
            let mut dirs: Vec<PathBuf> = entries
                .filter_map(|e| e.ok())
                .map(|e| e.path())
                .filter(|p| {
                    p.file_name()
                        .and_then(|n| n.to_str())
                        .is_some_and(|n| n.starts_with("chromium-"))
                })
                .collect();
            dirs.sort();
            dirs.reverse();
            for d in dirs {
                candidates.push(d.join("chrome-linux").join("chrome"));
            }
        }
    }
    candidates.push(PathBuf::from("/usr/bin/chromium"));
    candidates.push(PathBuf::from("/usr/bin/chromium-browser"));
    candidates.push(PathBuf::from("/usr/bin/google-chrome"));
    candidates.into_iter().find(|p| p.exists())
}

/// Launch Chromium.
///
/// `hide_local_ips_with_mdns = true` is Chromium's **default** and is
/// what slice 3 measures: no `--disable-features=…` flag is passed.
/// The second launch, if one happens, disables it and says so.
fn launch_chromium(
    chrome: &Path,
    profile: &Path,
    url: &str,
    disable_mdns: bool,
    log: &Path,
) -> std::io::Result<Child> {
    let _ = std::fs::remove_dir_all(profile);
    let _ = std::fs::create_dir_all(profile);
    let mut cmd = Command::new(chrome);
    cmd.arg("--headless=new")
        .arg("--no-sandbox")
        .arg("--disable-gpu")
        .arg(format!("--user-data-dir={}", profile.display()))
        .arg("--enable-logging=stderr")
        .arg("--v=0")
        .arg("--no-first-run")
        .arg("--no-default-browser-check")
        .arg("--disable-background-timer-throttling")
        .arg("--disable-renderer-backgrounding");
    if disable_mdns {
        cmd.arg("--disable-features=WebRtcHideLocalIpsWithMdns");
    }
    cmd.arg(url);
    let err = std::fs::File::create(log)?;
    cmd.stdout(Stdio::null()).stderr(Stdio::from(err));
    cmd.spawn()
}

// ===================================================================
// main
// ===================================================================

#[tokio::main(flavor = "multi_thread", worker_threads = 8)]
async fn main() {
    let mut chrome_arg = None;
    let mut inverse = String::new();
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--chrome" => chrome_arg = args.next(),
            // Deliberate defect injection, so a witness can be shown
            // to be capable of failing:
            //   mitm-pins-the-live-key  — the browser pins the
            //     IMPOSTOR's own static key instead of the
            //     credential's, so the MITM handshake succeeds and
            //     the MITM witness must FAIL.
            //   skip-enrollment-request — the enrollment REQUEST is
            //     never sent, so nothing may be promoted and the
            //     enrollment witness must FAIL.
            "--inverse" => inverse = args.next().unwrap_or_default(),
            _ => {}
        }
    }
    if !inverse.is_empty() {
        println!("[harness] INVERSE MODE: {inverse}");
    }

    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("runner has a parent")
        .to_path_buf();
    let work = root.join("work");
    let _ = std::fs::create_dir_all(&work);

    let mut ledger = Ledger::default();
    let code = match run(&root, &work, chrome_arg, &inverse, &mut ledger).await {
        Ok(()) => {
            println!();
            println!("=== Stage 4b browser-harness ledger ===");
            for v in &ledger.0 {
                println!("  {:<6} {}", if v.pass { "PASS" } else { "FAIL" }, v.name);
                if !v.pass {
                    println!("         {}", v.detail);
                }
            }
            let failed = ledger.failed();
            println!(
                "{} witness(es), {} failed",
                ledger.0.len(),
                failed
            );
            i32::from(failed > 0)
        }
        Err(e) => {
            println!("RTCB FAIL harness — {e}");
            println!();
            println!("=== Stage 4b browser-harness ledger (incomplete) ===");
            for v in &ledger.0 {
                println!("  {:<6} {}", if v.pass { "PASS" } else { "FAIL" }, v.name);
                if !v.pass {
                    println!("         {}", v.detail);
                }
            }
            1
        }
    };
    uninstall_ca();
    std::process::exit(code);
}

#[expect(clippy::too_many_lines, reason = "one linear harness script")]
async fn run(
    root: &Path,
    work: &Path,
    chrome_arg: Option<String>,
    inverse: &str,
    ledger: &mut Ledger,
) -> Result<(), String> {
    // --- 0. tools ---------------------------------------------------
    let chrome = find_chromium(chrome_arg)
        .ok_or_else(|| "no Chromium found; pass --chrome <path> or set CHROME_PATH".to_string())?;
    println!("[harness] chromium: {}", chrome.display());
    let dist = build_leaf(root)?;

    // --- 1. certificate --------------------------------------------
    let ca = issue_localhost_certificate(work)?;
    let (how, _owed) = install_ca(&ca);
    println!("[harness] CA install: {how}");
    if how.starts_with("FAILED") {
        return Err(format!(
            "the CA could not be installed into the browser trust store: {how}. \
             This harness will not fall back to --ignore-certificate-errors."
        ));
    }
    println!(
        "[harness] CA fingerprint source: {} ({} bytes)",
        ca.ca_pem_path.display(),
        ca.ca_pem.len()
    );

    // --- 2. nodes ---------------------------------------------------
    let lan = lan_ipv4();
    let anchor_bind: SocketAddr = match lan {
        Some(v4) => SocketAddr::new(IpAddr::V4(v4), 0),
        None => "127.0.0.1:0".parse().expect("addr"),
    };
    let anchor = spawn_node(Some(anchor_rtc(anchor_bind))).await;
    let loop_anchor = spawn_node(Some(anchor_rtc("127.0.0.1:0".parse().expect("addr")))).await;
    let impostor = spawn_node(Some(anchor_rtc("127.0.0.1:0".parse().expect("addr")))).await;
    let third_party = spawn_node(None).await;
    // The identity whose *signed* capability announcement witness (b)
    // replays: the announcement's `node_id` has to be the id the
    // browser claims, or the anchor would drop it for a reason other
    // than §12 gate 4.
    let ident = spawn_node(None).await;
    ident
        .announce_capabilities(CapabilitySet::new())
        .await
        .map_err(|e| format!("announce: {e}"))?;
    let announcement = ident
        .announcement_bytes_for_send_for_test()
        .ok_or_else(|| "the identity node produced no announcement bytes".to_string())?;

    let anchor_rtc_addr = anchor
        .rtc_driver()
        .ok_or_else(|| "the anchor has no rtc driver".to_string())?
        .local_addr();
    let loop_rtc_addr = loop_anchor
        .rtc_driver()
        .ok_or_else(|| "the loopback anchor has no rtc driver".to_string())?
        .local_addr();
    println!("[harness] anchor rtc socket: {anchor_rtc_addr} (stun responder on)");
    println!("[harness] loopback anchor rtc socket: {loop_rtc_addr}");

    // --- 3. services on the anchor ---------------------------------
    let _enroll = anchor
        .serve_rpc(ENROLL_SERVICE, Arc::new(Enrollment))
        .map_err(|e| format!("serve {ENROLL_SERVICE}: {e}"))?;
    let other_calls = Arc::new(AtomicUsize::new(0));
    let _other = anchor
        .serve_rpc(OTHER_SERVICE, Arc::new(Counting(Arc::clone(&other_calls))))
        .map_err(|e| format!("serve {OTHER_SERVICE}: {e}"))?;

    // A real protected provider, so "enrolled is not authorized" is
    // observed as a handler that never ran — not as an absent
    // service.
    let protected_calls = Arc::new(AtomicUsize::new(0));
    let authority_dir = work.join("authority");
    let _ = std::fs::remove_dir_all(&authority_dir);
    let _protected = {
        use net::adapter::net::behavior::org::{OrgKeypair, OrgMembershipCert};
        use net::adapter::net::behavior::org_admission::OrgAdmission;
        use net::adapter::net::behavior::org_authority::NodeAuthority;
        let org = OrgKeypair::generate();
        let entity = anchor.entity_id().clone();
        let cert = OrgMembershipCert::try_issue(&org, entity.clone(), 1, 3600)
            .map_err(|e| format!("cert: {e:?}"))?;
        let authority = NodeAuthority::adopt(&authority_dir, cert, &entity, 0, None)
            .map_err(|e| format!("adopt: {e:?}"))?;
        anchor
            .install_node_authority(Arc::new(authority))
            .map_err(|e| format!("install authority: {e:?}"))?;
        anchor
            .serve_rpc_protected(
                PROTECTED_SERVICE,
                Arc::new(Counting(Arc::clone(&protected_calls))),
                OrgAdmission::OwnerDelegated,
                Arc::new(|_| true),
            )
            .map_err(|e| format!("serve {PROTECTED_SERVICE}: {e}"))?
    };

    // --- 4. the page server (its origin is the CORS allow-list) ----
    let (step_tx, step_rx) = mpsc::channel(4);
    let page_state = PageState {
        dist,
        page: root.join("page"),
        steps: Arc::new(Mutex::new(step_rx)),
        pending: Arc::new(Mutex::new(HashMap::new())),
    };
    let (page_addr, _page_task) = serve_page(page_state)
        .await
        .map_err(|e| format!("page server: {e}"))?;
    let page_url = format!("http://localhost:{}/", page_addr.port());
    let origin = format!("http://localhost:{}", page_addr.port());
    println!("[harness] page: {page_url}");

    // --- 5. the bootstrap listeners --------------------------------
    let tls = BootstrapTls::Operator {
        cert_pem: ca.cert_pem_path.clone(),
        key_pem: ca.key_pem_path.clone(),
    };
    let mut cfg = BootstrapConfig::new(
        "127.0.0.1:0".parse().expect("addr"),
        Psk::new(PSK),
        tls.clone(),
        origin.clone(),
    );
    cfg.offers_per_ip_per_minute = 200;
    let anchor_listener = serve_bootstrap(Arc::clone(&anchor), cfg.clone())
        .await
        .map_err(|e| format!("anchor listener: {e}"))?;
    let loop_listener = serve_bootstrap(Arc::clone(&loop_anchor), cfg.clone())
        .await
        .map_err(|e| format!("loopback listener: {e}"))?;
    let impostor_listener = serve_bootstrap(Arc::clone(&impostor), cfg)
        .await
        .map_err(|e| format!("impostor listener: {e}"))?;
    let anchor_base = format!("https://localhost:{}", anchor_listener.local_addr().port());
    let loop_base = format!("https://localhost:{}", loop_listener.local_addr().port());
    let impostor_base = format!("https://localhost:{}", impostor_listener.local_addr().port());
    println!("[harness] anchor   {anchor_base}");
    println!("[harness] impostor {impostor_base} (fresh Noise keypair)");

    // --- 6. the credential -----------------------------------------
    let root_entity = Identity::generate().entity_id().clone();
    let credential = |anchor_pub: [u8; 32], url: &str| {
        let invite = InviteToken::mint(&root_entity, url, Duration::from_secs(600));
        BrowserBootstrapCredential::mint(
            invite,
            anchor_pub,
            Psk::new(PSK),
            url,
            Duration::from_secs(86_400),
        )
        .encode()
    };
    // ONE credential, pinning the REAL anchor's static key. The MITM
    // step presents this same credential to the impostor.
    let anchor_cred = credential(*anchor.public_key(), &anchor_base);
    let loop_cred = credential(*loop_anchor.public_key(), &loop_base);
    let anchor_pub_hex = hex(anchor.public_key());
    let psk_hex = hex(&PSK);

    // --- 7. Chromium ------------------------------------------------
    let profile = work.join("chrome-profile");
    let chrome_log = work.join("chrome.log");
    let mut child = launch_chromium(&chrome, &profile, &page_url, false, &chrome_log)
        .map_err(|e| format!("chromium: {e}"))?;
    println!("[harness] chromium launched WITHOUT --disable-features=WebRtcHideLocalIpsWithMdns");

    let mut script = Script {
        tx: step_tx,
        next_id: 1,
    };

    // ================================================================
    // Slice 3 — the mDNS measurement
    // ================================================================
    let mut mdns_lines: Vec<String> = Vec::new();
    let mut working_stun: Option<Option<String>> = None;

    struct Probe {
        label: &'static str,
        base: String,
        cred: String,
        pub_hex: String,
        anchor_node: u64,
        stun: Option<String>,
        node_id: u64,
    }
    // Ordering is deliberate: the STUN variant runs FIRST on
    // loopback and SECOND on the interface. If a failure tracked
    // position rather than configuration, the two would fail
    // together on the same position — they do not, so a failure is
    // the `iceServers` configuration and not "the second session
    // on this anchor".
    let mut probes = vec![
        Probe {
            label: "loopback/anchor-stun",
            base: loop_base.clone(),
            cred: loop_cred.clone(),
            pub_hex: hex(loop_anchor.public_key()),
            anchor_node: loop_anchor.node_id(),
            stun: Some(format!("stun:{loop_rtc_addr}")),
            node_id: 0xB0B0_0102,
        },
        Probe {
            label: "loopback/no-stun",
            base: loop_base.clone(),
            cred: loop_cred.clone(),
            pub_hex: hex(loop_anchor.public_key()),
            anchor_node: loop_anchor.node_id(),
            stun: None,
            node_id: 0xB0B0_0101,
        },
    ];
    if lan.is_some() {
        probes.push(Probe {
            label: "interface/no-stun",
            base: anchor_base.clone(),
            cred: anchor_cred.clone(),
            pub_hex: anchor_pub_hex.clone(),
            anchor_node: anchor.node_id(),
            stun: None,
            node_id: 0xB0B0_0103,
        });
        probes.push(Probe {
            label: "interface/anchor-stun",
            base: anchor_base.clone(),
            cred: anchor_cred.clone(),
            pub_hex: anchor_pub_hex.clone(),
            anchor_node: anchor.node_id(),
            stun: Some(format!("stun:{anchor_rtc_addr}")),
            node_id: 0xB0B0_0104,
        });
    } else {
        mdns_lines.push(
            "interface: SKIPPED — this host exposed no routable non-loopback IPv4".into(),
        );
    }

    for probe in &probes {
        let node = if probe.base == loop_base {
            &loop_anchor
        } else {
            &anchor
        };
        let r = script
            .run(Step::Connect {
                id: 0,
                session: format!("mdns-{}", probe.label),
                base: probe.base.clone(),
                node_id: format!("{:016x}", probe.node_id),
                psk: psk_hex.clone(),
                anchor_pub: probe.pub_hex.clone(),
                anchor_node_id: format!("{:016x}", probe.anchor_node),
                credential: probe.cred.clone(),
                stun: probe.stun.clone(),
                timeout_ms: 35_000,
                expect_failure: false,
            })
            .await;
        if !r.ok {
            mdns_lines.push(format!(
                "{}: NO PAIR — {}",
                probe.label,
                r.error.clone().unwrap_or_else(|| "unknown".into())
            ));
            continue;
        }
        if working_stun.is_none() {
            working_stun = Some(probe.stun.clone());
        }
        let stats = script
            .run(Step::Stats {
                id: 0,
                session: format!("mdns-{}", probe.label),
            })
            .await;
        let browser_pair = stats
            .stats
            .map(|v| v.to_string())
            .unwrap_or_else(|| "<no getStats>".into());
        // The anchor-side half. str0m 0.23.1 exposes no candidate
        // TYPE; `selected_pair` reports the address traffic is going
        // to and whether that address was ever signalled.
        let anchor_pair = match node.peer_endpoint(probe.node_id) {
            Some(PeerAddr::Rtc(id)) => match node
                .rtc_driver()
                .expect("rtc driver")
                .selected_pair(id)
                .await
            {
                Some((local, remote, learned)) => {
                    format!("local={local} remote={remote} learned={learned}")
                }
                None => "<no transmit destination recorded yet>".into(),
            },
            _ => "<no rtc endpoint on the anchor>".into(),
        };
        mdns_lines.push(format!(
            "{}: PAIR FORMED in {:.0} ms; browser getStats {browser_pair}; anchor {anchor_pair}",
            probe.label,
            r.open_ms.unwrap_or(f64::NAN)
        ));
    }

    // If nothing formed with mDNS on, relaunch the browser with the
    // obfuscation off so the §12 witnesses can still run, and SAY SO.
    let mut mdns_needed_disabling = false;
    if working_stun.is_none() {
        mdns_needed_disabling = true;
        mdns_lines.push(
            "NO configuration formed a pair with Chromium's mDNS obfuscation ON; \
             relaunching with --disable-features=WebRtcHideLocalIpsWithMdns so the \
             §12 witnesses can run. The plan's answer (c) — an mDNS client on the \
             anchor — is therefore REQUIRED on this host."
                .into(),
        );
        let _ = child.kill();
        let _ = child.wait();
        child = launch_chromium(&chrome, &profile, &page_url, true, &chrome_log)
            .map_err(|e| format!("chromium relaunch: {e}"))?;
        working_stun = Some(Some(format!("stun:{anchor_rtc_addr}")));
    }
    let stun = working_stun.clone().unwrap_or(None);

    for line in &mdns_lines {
        println!("[mdns] {line}");
    }
    ledger.record(
        "mdns_candidate_pair_measured",
        !mdns_lines.is_empty(),
        mdns_lines.join(" | "),
    );

    // ================================================================
    // The §12 witnesses
    // ================================================================
    let conn = Connector {
        base: anchor_base.clone(),
        credential: anchor_cred.clone(),
        anchor_pub: anchor_pub_hex.clone(),
        psk: psk_hex.clone(),
        anchor_node: anchor.node_id(),
        stun: stun.clone(),
    };

    // ---- (a) the permitted enrollment exchange --------------------
    let enroll_node: u64 = 0xB0B0_0001;
    let enroll_origin: u64 = 0xE1E1_0000_0000_0001;
    {
        let r = conn.connect(&mut script, "enroll", enroll_node).await;
        if !r.ok {
            ledger.record(
                "enrollment_exchange_promotes_this_session",
                false,
                format!(
                    "the browser could not establish a session: {}",
                    r.error.unwrap_or_default()
                ),
            );
        } else {
            let provisional_at_install = anchor.peer_is_provisional(enroll_node);
            let session_at_install = anchor.peer_session_id(enroll_node);
            let promoted_before = anchor.rtc_stats().admission_promoted();
            // Both halves derive the session id from the handshake;
            // if the browser's differs, the two are not the same
            // session and every observable below is about something
            // else.
            let same_session = r.session_id.as_deref()
                == session_at_install.map(|s| format!("{s:016x}")).as_deref();

            // 1. subscribe to our own enrollment reply channel — the
            //    one subscription §12 permits.
            let reply_channel = enroll_reply_channel(enroll_origin);
            let sub = subscribe_payload(&reply_channel, 0x5B5B_0001);
            let s = script
                .run(Step::Send {
                    id: 0,
                    session: "enroll".into(),
                    prefix: String::new(),
                    stream_id: format!("{:x}", SUBPROTOCOL_CHANNEL_MEMBERSHIP as u64),
                    subprotocol: SUBPROTOCOL_CHANNEL_MEMBERSHIP,
                    channel_hash: 0,
                    origin_hash: format!("{enroll_origin:016x}"),
                    reliable: false,
                    payload: hex(&sub),
                })
                .await;
            if !s.ok {
                return Err(format!("subscribe send failed: {:?}", s.error));
            }

            // The publisher's membership Ack, so "the subscription
            // was accepted" is an observed fact rather than an
            // assumption the missing reply would silently hide.
            let ack = script
                .run(Step::Expect {
                    id: 0,
                    session: "enroll".into(),
                    subprotocol: SUBPROTOCOL_CHANNEL_MEMBERSHIP,
                    timeout_ms: 10_000,
                })
                .await;
            let subscribe_accepted = ack
                .frame
                .as_deref()
                .map(unhex)
                .and_then(|f| membership::decode(&f).ok())
                .is_some_and(|m| matches!(m, MembershipMsg::Ack { accepted: true, .. }));

            // 2. the enrollment REQUEST.
            let frame = rpc_request_frame(ENROLL_SERVICE, enroll_origin, 0xC0FF_EE01, b"join");
            if inverse == "skip-enrollment-request" {
                println!("[harness] INVERSE: NOT sending the enrollment REQUEST");
            } else {
                let s = script
                    .run(Step::Send {
                        id: 0,
                        session: "enroll".into(),
                        prefix: String::new(),
                        stream_id: format!("{:x}", frame.stream_id),
                        subprotocol: 0,
                        channel_hash: frame.channel_hash_u16,
                        origin_hash: format!("{enroll_origin:016x}"),
                        reliable: true,
                        payload: hex(&frame.payload),
                    })
                    .await;
                if !s.ok {
                    return Err(format!("enrollment REQUEST send failed: {:?}", s.error));
                }
            }

            // 3. the RESPONSE, decrypted in the browser and handed
            //    back for decoding here. Event plane only — the
            //    membership Ack rides its own subprotocol and must
            //    not be mistaken for the reply.
            let reply = script
                .run(Step::Expect {
                    id: 0,
                    session: "enroll".into(),
                    subprotocol: 0,
                    timeout_ms: 20_000,
                })
                .await;
            let reply_ok = reply
                .frame
                .as_deref()
                .map(unhex)
                .map(|f| response_carries_admitted_outcome(&f))
                .unwrap_or(false);

            // `peer_is_provisional` going false is NOT by itself
            // promotion: a reclaimed session is also "not
            // provisional", because it is not there at all. The
            // counter is the transition; the session-id equality
            // below is what says it happened to THIS incarnation.
            let mut promoted = false;
            for _ in 0..100 {
                if anchor.rtc_stats().admission_promoted() > promoted_before
                    && !anchor.peer_is_provisional(enroll_node)
                {
                    promoted = true;
                    break;
                }
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
            let session_after = anchor.peer_session_id(enroll_node);
            let promoted_delta = anchor.rtc_stats().admission_promoted() - promoted_before;
            let pass = provisional_at_install
                && same_session
                && subscribe_accepted
                && reply_ok
                && promoted
                && promoted_delta == 1
                && session_at_install.is_some()
                && session_at_install == session_after;
            ledger.record(
                "enrollment_exchange_promotes_this_session",
                pass,
                format!(
                    "installed provisional={provisional_at_install}; browser and anchor \
                     agree on the session id={same_session}; reply-channel Subscribe \
                     acked accepted={subscribe_accepted}; the BROWSER decrypted an \
                     Admitted JoinOutcome={reply_ok} ({}); peer_is_provisional now {}; \
                     admission_promoted +{promoted_delta}; session {:?} -> {:?}",
                    reply.error.clone().unwrap_or_else(|| "reply received".into()),
                    anchor.peer_is_provisional(enroll_node),
                    session_at_install,
                    session_after
                ),
            );

            // ---- the sixth §12 item: enrolled is not authorized ----
            let before = protected_calls.load(Ordering::SeqCst);
            let frame = rpc_request_frame(PROTECTED_SERVICE, enroll_origin, 0xC0FF_EE02, b"{}");
            let _ = script
                .run(Step::Send {
                    id: 0,
                    session: "enroll".into(),
                    prefix: String::new(),
                    stream_id: format!("{:x}", frame.stream_id),
                    subprotocol: 0,
                    channel_hash: frame.channel_hash_u16,
                    origin_hash: format!("{enroll_origin:016x}"),
                    reliable: true,
                    payload: hex(&frame.payload),
                })
                .await;
            tokio::time::sleep(Duration::from_millis(1200)).await;
            let after = protected_calls.load(Ordering::SeqCst);
            // The caller must still BE there and be enrolled: a
            // session that was reclaimed also never invokes the
            // handler, and would make this pass for the wrong
            // reason.
            let still_live = anchor.peer_session_id(enroll_node) == session_at_install;
            ledger.record(
                "enrolled_without_authority_is_still_denied",
                promoted && still_live && after == before,
                format!(
                    "the caller is ENROLLED and still the same live session \
                     (promoted={promoted}, session unchanged={still_live}, \
                     peer_is_provisional={}); the registered protected handler ran {} \
                     time(s)",
                    anchor.peer_is_provisional(enroll_node),
                    after - before
                ),
            );
        }
    }

    // ---- (b) a provisional capability announcement ----------------
    {
        let node_id = ident.node_id();
        let r = conn.connect(&mut script, "announce", node_id).await;
        let before = anchor.rtc_stats().admission_refused_announce();
        if !r.ok {
            ledger.record(
                "provisional_announcement_is_refused",
                false,
                format!("no session: {}", r.error.unwrap_or_default()),
            );
        } else {
            let _ = script
                .run(Step::Send {
                    id: 0,
                    session: "announce".into(),
                    prefix: String::new(),
                    stream_id: format!("{:x}", SUBPROTOCOL_CAPABILITY_ANN as u64),
                    subprotocol: SUBPROTOCOL_CAPABILITY_ANN,
                    channel_hash: 0,
                    origin_hash: format!("{:016x}", ident.origin_hash()),
                    reliable: false,
                    payload: hex(&announcement),
                })
                .await;
            let hit = wait_for(
                || anchor.rtc_stats().admission_refused_announce() > before,
                Duration::from_secs(10),
            )
            .await;
            ledger.record(
                "provisional_announcement_is_refused",
                hit && anchor.peer_is_provisional(node_id),
                format!(
                    "admission_refused_announce {} -> {}",
                    before,
                    anchor.rtc_stats().admission_refused_announce()
                ),
            );
        }
    }

    // ---- (c) a provisional Subscribe to an unrelated channel ------
    {
        let node_id: u64 = 0xB0B0_0003;
        let r = conn.connect(&mut script, "subscribe", node_id).await;
        let before = anchor.rtc_stats().admission_refused_subscribe();
        if !r.ok {
            ledger.record(
                "provisional_subscribe_to_an_unrelated_channel_is_refused",
                false,
                format!("no session: {}", r.error.unwrap_or_default()),
            );
        } else {
            let sub = subscribe_payload("app.events.orders", 0x5B5B_0003);
            let _ = script
                .run(Step::Send {
                    id: 0,
                    session: "subscribe".into(),
                    prefix: String::new(),
                    stream_id: format!("{:x}", SUBPROTOCOL_CHANNEL_MEMBERSHIP as u64),
                    subprotocol: SUBPROTOCOL_CHANNEL_MEMBERSHIP,
                    channel_hash: 0,
                    origin_hash: format!("{node_id:016x}"),
                    reliable: false,
                    payload: hex(&sub),
                })
                .await;
            let hit = wait_for(
                || anchor.rtc_stats().admission_refused_subscribe() > before,
                Duration::from_secs(10),
            )
            .await;
            ledger.record(
                "provisional_subscribe_to_an_unrelated_channel_is_refused",
                hit,
                format!(
                    "admission_refused_subscribe {} -> {}",
                    before,
                    anchor.rtc_stats().admission_refused_subscribe()
                ),
            );
        }
    }

    // ---- (d) a provisional call to another service ----------------
    {
        let node_id: u64 = 0xB0B0_0004;
        let origin: u64 = 0xE1E1_0000_0000_0004;
        let r = conn.connect(&mut script, "other-service", node_id).await;
        let before = anchor.rtc_stats().admission_refused_deliver();
        let calls_before = other_calls.load(Ordering::SeqCst);
        if !r.ok {
            ledger.record(
                "provisional_call_to_another_service_is_refused",
                false,
                format!("no session: {}", r.error.unwrap_or_default()),
            );
        } else {
            let frame = rpc_request_frame(OTHER_SERVICE, origin, 0xC0FF_EE04, b"{}");
            let _ = script
                .run(Step::Send {
                    id: 0,
                    session: "other-service".into(),
                    prefix: String::new(),
                    stream_id: format!("{:x}", frame.stream_id),
                    subprotocol: 0,
                    channel_hash: frame.channel_hash_u16,
                    origin_hash: format!("{origin:016x}"),
                    reliable: true,
                    payload: hex(&frame.payload),
                })
                .await;
            let hit = wait_for(
                || anchor.rtc_stats().admission_refused_deliver() > before,
                Duration::from_secs(10),
            )
            .await;
            let calls_after = other_calls.load(Ordering::SeqCst);
            ledger.record(
                "provisional_call_to_another_service_is_refused",
                hit && calls_after == calls_before,
                format!(
                    "admission_refused_deliver {} -> {}; the registered {OTHER_SERVICE} \
                     handler ran {} time(s)",
                    before,
                    anchor.rtc_stats().admission_refused_deliver(),
                    calls_after - calls_before
                ),
            );
        }
    }

    // ---- (e) + (f) transit, local delivery, redirection -----------
    {
        let node_id: u64 = 0xB0B0_0005;
        let r = conn.connect(&mut script, "transit", node_id).await;
        if !r.ok {
            ledger.record(
                "provisional_transit_is_refused",
                false,
                format!("no session: {}", r.error.unwrap_or_default()),
            );
            ledger.record(
                "local_envelope_accepted_redirected_denied",
                false,
                "no session",
            );
        } else {
            // (f) part 1 — an envelope whose dest IS the anchor is
            // local delivery, never transit.
            let before_local = anchor.rtc_stats().admission_refused_transit();
            let _ = script
                .run(Step::Send {
                    id: 0,
                    session: "transit".into(),
                    prefix: hex(&routing_prefix(anchor.node_id(), node_id)),
                    stream_id: format!("{TRANSIT_STREAM_ID:x}"),
                    subprotocol: 0,
                    channel_hash: 0,
                    origin_hash: format!("{node_id:016x}"),
                    reliable: false,
                    payload: hex(b"transit"),
                })
                .await;
            tokio::time::sleep(Duration::from_millis(800)).await;
            let after_local = anchor.rtc_stats().admission_refused_transit();

            // (e) + (f) part 2 — the same envelope redirected at a
            // third party.
            let _ = script
                .run(Step::Send {
                    id: 0,
                    session: "transit".into(),
                    prefix: hex(&routing_prefix(third_party.node_id(), node_id)),
                    stream_id: format!("{TRANSIT_STREAM_ID:x}"),
                    subprotocol: 0,
                    channel_hash: 0,
                    origin_hash: format!("{node_id:016x}"),
                    reliable: false,
                    payload: hex(b"transit"),
                })
                .await;
            let refused = wait_for(
                || anchor.rtc_stats().admission_refused_transit() > after_local,
                Duration::from_secs(10),
            )
            .await;
            ledger.record(
                "provisional_transit_is_refused",
                refused,
                format!(
                    "admission_refused_transit {} -> {} for a third-party dest_id",
                    after_local,
                    anchor.rtc_stats().admission_refused_transit()
                ),
            );
            ledger.record(
                "local_envelope_accepted_redirected_denied",
                after_local == before_local && refused,
                format!(
                    "locally-addressed envelope: refusals {before_local} -> {after_local} \
                     (unchanged means it was delivered locally, not refused); \
                     redirected envelope refused = {refused}"
                ),
            );
        }
    }

    // ================================================================
    // Slice 5 — the MITM witness
    // ================================================================
    {
        let node_id: u64 = 0xB0B0_0006;
        let peers_before = impostor.peer_count();
        let prov_before = impostor.provisional_count();
        // The SAME credential — so the impostor accepts it, answers
        // the offer, and the DataChannel opens. The browser pins the
        // REAL anchor's static key, which the impostor does not hold.
        let r = script
            .run(Step::Connect {
                id: 0,
                session: "mitm".into(),
                base: impostor_base.clone(),
                node_id: format!("{node_id:016x}"),
                psk: psk_hex.clone(),
                anchor_pub: if inverse == "mitm-pins-the-live-key" {
                    // The defect the credential exists to prevent:
                    // pinning whatever key the reached anchor holds.
                    hex(impostor.public_key())
                } else {
                    anchor_pub_hex.clone()
                },
                anchor_node_id: format!("{:016x}", impostor.node_id()),
                credential: anchor_cred.clone(),
                stun: stun.clone(),
                timeout_ms: 15_000,
                expect_failure: true,
            })
            .await;
        tokio::time::sleep(Duration::from_millis(1500)).await;
        let peers_after = impostor.peer_count();
        let prov_after = impostor.provisional_count();
        let handshake_failed = r.ok; // `expect_failure` inverts `ok`
        ledger.record(
            "mitm_anchor_fails_the_handshake_and_installs_nothing",
            handshake_failed && peers_after == peers_before && prov_after == prov_before,
            format!(
                "browser handshake against the credential-pinned key failed = \
                 {handshake_failed} ({}); impostor peer_count {peers_before} -> \
                 {peers_after}, provisional {prov_before} -> {prov_after}",
                r.info.unwrap_or_else(|| "no detail".into())
            ),
        );
    }

    // --- done -------------------------------------------------------
    let _ = script.run(Step::Done { id: 0 }).await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    let _ = child.kill();
    let _ = child.wait();
    anchor_listener.shutdown().await;
    loop_listener.shutdown().await;
    impostor_listener.shutdown().await;
    let _ = std::fs::remove_dir_all(&authority_dir);
    if mdns_needed_disabling {
        println!(
            "[mdns] NOTE: the §12 witnesses above ran with Chromium's mDNS \
             obfuscation DISABLED, because no pair formed with it on."
        );
    }
    Ok(())
}

/// Does this decrypted event-plane frame carry an nRPC RESPONSE whose
/// body is an **Admitted** `JoinOutcome`?
fn response_carries_admitted_outcome(frame: &[u8]) -> bool {
    let Some(meta) = EventMeta::from_bytes(frame) else {
        return false;
    };
    if meta.dispatch != DISPATCH_RPC_RESPONSE {
        return false;
    }
    let Some(body) = frame.get(RPC_FRAME_BODY_OFFSET..) else {
        return false;
    };
    let Ok(resp) = RpcResponsePayload::decode(Bytes::copy_from_slice(body)) else {
        return false;
    };
    resp.body.as_ref() == admitted_outcome().as_ref()
}

async fn wait_for<F: Fn() -> bool>(predicate: F, within: Duration) -> bool {
    let deadline = tokio::time::Instant::now() + within;
    while tokio::time::Instant::now() < deadline {
        if predicate() {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    predicate()
}
