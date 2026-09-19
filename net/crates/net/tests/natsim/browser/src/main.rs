//! **The Stage 6 deterministic NAT conformance runner** — two
//! headless browsers behind simulated NATs, one anchor, one row.
//!
//! ```text
//!   natsim-browser-matrix --scenario browser_cone_symmetric \
//!     --state /tmp/natsim.X --nat-a cone-ar --nat-b symmetric \
//!     --expect direct --engine-a chromium --engine-b chromium \
//!     --anchor-ip 10.99.0.10 --netns-a nsim_a --netns-b nsim_b
//! ```
//!
//! Launched by `tests/natsim/run_scenario.sh` inside `nsim_wan` — the
//! simulated internet — after `setup.sh` has built the two NAT
//! gateways. It writes `<state>/browser_outcome.json`, which
//! `tests/natsim.rs` asserts against the row table in
//! `tests/natsim/rows.rs`.
//!
//! # Topology
//!
//! ```text
//!        nsim_wan  10.99.0.10  this process
//!                              ├─ anchor MeshNode (mesh :7000)
//!                              ├─ RTC socket + STUN responder (:7100)
//!                              ├─ bootstrap listener, HTTPS (:8443)
//!                              └─ matrix control plane, HTTP (:8081)
//!          |                                   |
//!      nsim_gwa (cone-ar|cone-pr|symmetric) nsim_gwb  ← the row
//!          |                                   |
//!      nsim_a: driver → browser → page      nsim_b: same
//!              + page server 127.0.0.1:8080
//! ```
//!
//! # What runs where, and why it matters
//!
//! Each browser and **its entire control stack** live inside the NAT'd
//! namespace: `ip netns exec nsim_a node driver.mjs` puts Playwright,
//! the browser and every UDP socket ICE opens behind that gateway,
//! while the NDJSON control channel rides the stdio pipes this process
//! already holds. The alternative — one driver out here launching
//! browsers through an `executablePath` wrapper — puts the browser in
//! the namespace and leaves Playwright's sockets outside it, so every
//! later "why did this page reach that" is a question about the
//! wrapper rather than about the NAT.
//!
//! The **page** is served from `http://localhost:8080` by a
//! `page-server` child of this binary, inside each browser's own
//! namespace. `http://localhost` is a secure context — which
//! `RTCPeerConnection` and wasm require — and it is the only
//! mechanism that needs no per-engine flag: Chromium's
//! `--unsafely-treat-insecure-origin-as-secure` has no Firefox
//! counterpart, so a page served over plain HTTP from the anchor's
//! namespace would have cost the control row. The **anchor's**
//! endpoints stay on the real `serve_bootstrap` HTTPS listener, which
//! is the production shape anyway.
//!
//! # What the rows measure, and what they deliberately do not
//!
//! A row measures the **disposition of one signalling dialog** between
//! two browser leaves behind two simulated NATs: direct, or routed
//! through the anchor and typed as such — **and whether an
//! application payload actually crossed it**. It reads four
//! independent witnesses:
//!
//! 1. the typed `kind` of `connectPeer`'s settled result, at the page
//!    surface;
//! 2. the §10 ICE counters on **both leaves** and on the anchor, with
//!    the identity `direct + relayed + failed + udp_blocked ==
//!    attempted` and `pending == 0` asserted per side;
//! 3. the gateways' own conntrack tables — written by
//!    `run_scenario.sh`, not by any party to the session, and each
//!    side reporting which reader produced its numbers so an
//!    unreadable table is a refusal rather than an absence of flow;
//! 4. a **nonce-correlated bidirectional application exchange** on
//!    the public peer-addressed stream surface, with the anchor's own
//!    per-pair `forwarded_app_packets` read in this process either
//!    side of it: flat both ways on a direct row, moving both ways on
//!    a relayed one.
//!
//! Witness 4 is this round's addition, and the first three are why it
//! was needed: none of them observes a payload. Conntrack reply
//! traffic on a direct row can be ICE or Noise, and a relayed
//! DISPOSITION only says the routed session was kept — so a direct
//! row whose application bytes the anchor carried satisfies all three
//! and fails only the fourth. The runner mints both nonces; neither
//! page chooses one, so "B decoded exactly what A sent" cannot be
//! satisfied by one side alone.
//!
//! What a row still does not measure is the §10 three-part witness's
//! unrelated-pair liveness leg, which needs a third context and
//! belongs in the browser runner where the tabs share one origin.
//!
//! # `udp_blocked` on the anchor
//!
//! Native `RtcStats` has no `udp_blocked` term and should not: a node
//! that signals over UDP cannot have UDP blocked. The verdict reports
//! the anchor's term as a constant `0` so the four-term partition is
//! well formed on both sides of the boundary; the term is the leaf's,
//! where `UdpBlockedEvidence` can actually establish it.

mod driver;

#[path = "../../rows.rs"]
mod rows;

use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::extract::{Path as AxPath, Query, State};
use axum::http::{header, HeaderName, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::Router;
use serde::{Deserialize, Serialize};
use tokio::sync::{mpsc, oneshot, Mutex};

use bytes::Bytes;
use net::adapter::net::cortex::{
    RpcContext, RpcHandler, RpcHandlerError, RpcResponsePayload, RpcStatus,
};
use net::adapter::net::rtc::{RtcConfig, ENROLL_SERVICE};
use net::adapter::net::{EntityKeypair, MeshNode, MeshNodeConfig};
use net_sdk::bootstrap_credential::{BrowserBootstrapCredential, Psk};
use net_sdk::enrollment::InviteToken;
use net_sdk::identity::Identity;
use net_sdk::rtc_bootstrap::{serve_bootstrap, BootstrapConfig, BootstrapTls};

use driver::Driver;
use rows::{Disposition, Enumeration, IceCounters, Row};

/// The transport trust domain's PSK — one value for the anchor and
/// every credential this run mints.
const PSK: [u8; 32] = [0x6Au8; 32];

/// The capability both leaves announce and A queries for. §9 step 1:
/// A learns B's entity and Noise keys from B's **signed**
/// announcement, which is the only place `connectPeer` can get them —
/// it takes a peer id and nothing else.
const CAPABILITY: &str = "natsim.matrix.peer";

/// The one service this anchor has to serve.
///
/// `connect()` ENROLS, and an anchor that serves nothing leaves that
/// call unanswered until the leaf's own 30 s deadline, which comes
/// out of `connect` as `rpc: the call's deadline elapsed` and reads
/// like a broken transport. It is not optional for a conformance row
/// either: §12 gate 4 refuses a capability announcement from a
/// PROVISIONAL session, so an unenrolled leaf could never announce
/// and the two tabs could never discover each other — every row would
/// die two steps later with a cause that no longer names itself.
///
/// Observed, not deduced: run 35053041109's Firefox row got its
/// DataChannel open and the anchor counted `direct=2`, then both tabs
/// failed with exactly that RPC deadline thirty seconds later.
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

/// `JoinOutcome`'s pinned wire form (`sdk/src/enrollment.rs`: `NMO1`,
/// then `0` Admitted / `1` Rejected, then a length-prefixed
/// delegation chain). The core's promotion gate parses exactly this.
fn admitted_outcome() -> Bytes {
    let mut buf = Vec::from(*b"NMO1");
    buf.push(0);
    let chain = b"natsim-matrix-delegation-chain";
    buf.extend_from_slice(&(chain.len() as u32).to_le_bytes());
    buf.extend_from_slice(chain);
    Bytes::from(buf)
}

const CA_COMMON_NAME: &str = "net-mesh natsim harness CA";

/// A useful step label for a timeout message: the serialized `kind`,
/// which is the word the page switches on.
fn step_label(step: &Step) -> String {
    serde_json::to_value(step)
        .ok()
        .and_then(|v| {
            v.get("kind")
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned)
        })
        .unwrap_or_else(|| "(unserializable step)".to_owned())
}

// ===================================================================
// Arguments
// ===================================================================

#[derive(Debug, Clone)]
struct Matrix {
    scenario: String,
    state: PathBuf,
    nat_a: String,
    nat_b: String,
    expect: String,
    /// `granted` or `none`: whether the drivers grant the page's own
    /// origin camera+microphone before opening it. Part of the row
    /// (`rows.rs`), passed here, and echoed into the verdict as what
    /// the DRIVERS reported doing — never as this flag.
    media: String,
    engine_a: String,
    engine_b: String,
    anchor_ip: Ipv4Addr,
    /// The address of the run's STUN responder, which MUST NOT be the
    /// anchor's own RTC socket. See `run_row` — the whole of §6.12.
    /// Accepted and deliberately UNUSED since §6.12.2.
    ///
    /// The matrix used to run a separate STUN host here. The product
    /// now announces its own second endpoint and the leaf defaults to
    /// it, so nothing in a row needs this address — but the flag stays
    /// so an operator's existing invocation does not break, and its
    /// being unused is stated rather than left for someone to
    /// rediscover.
    #[allow(dead_code)]
    stun_ip: Ipv4Addr,
    netns_a: String,
    netns_b: String,
    mesh_port: u16,
    rtc_port: u16,
    /// The anchor's SECOND UDP port: STUN only, announced as
    /// `rtc_stun_addr`, never an ICE peer. Distinct from `rtc_port`
    /// because libwebrtc eats datagrams from a configured STUN
    /// server before pairing (§6.12).
    stun_port: u16,
    bootstrap_port: u16,
    control_port: u16,
    page_port: u16,
    /// How long one page step may take.
    step_timeout: Duration,
    /// How long a side's counters may take to settle after the dialog
    /// terminated, before the last sample is taken as final.
    settle: Duration,
    /// How long the nonce-correlated application exchange may take,
    /// end to end, once both sides have a session.
    ///
    /// Bounded and short: the exchange is a handful of frames on a
    /// fire-and-forget stream, and a pair that cannot deliver one
    /// nonce inside this has not delivered at all. It is NOT a
    /// retry budget around a failing assertion — the assertion is
    /// exact nonce equality, and a longer wait cannot turn the wrong
    /// nonce into the right one.
    app_exchange: Duration,
}

#[derive(Debug, Clone)]
struct PageServer {
    bind: SocketAddr,
    page: PathBuf,
    browser_dist: PathBuf,
    ready_marker: Option<PathBuf>,
}

enum Mode {
    Matrix(Matrix),
    Page(PageServer),
}

fn usage() -> String {
    "usage:\n  natsim-browser-matrix --scenario <name> --state <dir> --nat-a <mode> \
     --nat-b <mode> --expect <direct|relayed> --engine-a <engine> --engine-b <engine> \
     [--media <granted|none>] \
     --anchor-ip <ip> [--stun-ip <ip>] --netns-a <ns> --netns-b <ns>\n  \
     natsim-browser-matrix page-server \
     --bind <addr> --page <dir> --browser-dist <dir> [--ready <file>]"
        .to_owned()
}

fn parse_args() -> Result<Mode, String> {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    let mut flags: HashMap<String, String> = HashMap::new();
    let page_mode = argv.first().map(String::as_str) == Some("page-server");
    let mut i = usize::from(page_mode);
    while i < argv.len() {
        let key = argv[i]
            .strip_prefix("--")
            .ok_or_else(|| format!("unexpected argument {:?}\n{}", argv[i], usage()))?;
        let value = argv
            .get(i + 1)
            .ok_or_else(|| format!("--{key} wants a value\n{}", usage()))?;
        flags.insert(key.to_owned(), value.clone());
        i += 2;
    }
    let need = |key: &str| -> Result<String, String> {
        flags
            .get(key)
            .cloned()
            .ok_or_else(|| format!("--{key} is required\n{}", usage()))
    };
    if page_mode {
        return Ok(Mode::Page(PageServer {
            bind: need("bind")?.parse().map_err(|e| format!("--bind: {e}"))?,
            page: PathBuf::from(need("page")?),
            browser_dist: PathBuf::from(need("browser-dist")?),
            ready_marker: flags.get("ready").map(PathBuf::from),
        }));
    }
    let port = |key: &str, default: u16| -> Result<u16, String> {
        match flags.get(key) {
            None => Ok(default),
            Some(v) => v.parse().map_err(|e| format!("--{key}: {e}")),
        }
    };
    Ok(Mode::Matrix(Matrix {
        scenario: need("scenario")?,
        state: PathBuf::from(need("state")?),
        nat_a: need("nat-a")?,
        nat_b: need("nat-b")?,
        expect: need("expect")?,
        // Defaulted to the six rows' value so an existing invocation
        // keeps working, and VALIDATED here rather than at the
        // driver: a typo would otherwise arrive as "not the string
        // `none`", which the driver reads as a grant.
        media: match flags.get("media").map(String::as_str) {
            None | Some("granted") => "granted".to_owned(),
            Some("none") => "none".to_owned(),
            Some(other) => {
                return Err(format!(
                    "--media must be `granted` or `none`, got {other:?}\n{}",
                    usage()
                ))
            }
        },
        engine_a: need("engine-a")?,
        engine_b: need("engine-b")?,
        anchor_ip: need("anchor-ip")?
            .parse()
            .map_err(|e| format!("--anchor-ip: {e}"))?,
        // Defaulted, and passed explicitly by `run_scenario.sh`
        // because the lab's second public address is a fact of the
        // topology that file owns. 10.99.0.11 is `X`, the aux public
        // address `setup.sh` always adds to the wan bridge; the
        // browser rows launch no mesh helper, so nothing else binds
        // it.
        stun_ip: flags
            .get("stun-ip")
            .map_or(Ok(Ipv4Addr::new(10, 99, 0, 11)), |v| v.parse())
            .map_err(|e| format!("--stun-ip: {e}"))?,
        netns_a: need("netns-a")?,
        netns_b: need("netns-b")?,
        mesh_port: port("mesh-port", 7000)?,
        rtc_port: port("rtc-port", 7100)?,
        stun_port: port("stun-port", 7101)?,
        bootstrap_port: port("bootstrap-port", 8443)?,
        control_port: port("control-port", 8081)?,
        page_port: port("page-port", 8080)?,
        step_timeout: Duration::from_secs(90),
        settle: Duration::from_secs(10),
        app_exchange: Duration::from_secs(20),
    }))
}

// ===================================================================
// The step protocol the page executes
// ===================================================================

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum Step {
    Connect {
        id: u64,
        credential: String,
        bootstrap_url: String,
        origin: String,
        anchor_rtc_addr: String,
        entity_secret_hex: String,
        noise_secret_hex: String,
    },
    Announce {
        id: u64,
        capabilities: Vec<String>,
    },
    Query {
        id: u64,
        capability: String,
        expect_peer: String,
        timeout_ms: u64,
    },
    /// Arm the ANSWERER. Installs a `signal` listener and calls
    /// `acceptPeer` when it fires; issued before the offerer's step,
    /// because the offer arrives as a signalling envelope and
    /// `acceptPeer` answers the verified envelope the leaf already
    /// holds.
    ArmAccept {
        id: u64,
    },
    /// Collect the armed `acceptPeer` outcome.
    AcceptResult {
        id: u64,
        timeout_ms: u64,
    },
    ConnectPeer {
        id: u64,
        peer: String,
        timeout_ms: u64,
    },
    /// Open a peer-addressed stream, arm a receiver on it, and — the
    /// moment `expect_nonce` arrives — answer with `nonce`.
    ///
    /// The ANSWERER's half of the application exchange, armed before
    /// the offerer sends anything for the same reason `arm_accept`
    /// is: a receiver installed afterwards would miss frames that
    /// have already been delivered, and a fire-and-forget stream does
    /// not keep them.
    AppArm {
        id: u64,
        peer: String,
        stream_id: String,
        /// This side's nonce — what it echoes with.
        nonce: String,
        /// The peer's nonce — what it is waiting to decode.
        expect_nonce: String,
    },
    /// Send `nonce` on a peer-addressed stream until `expect_nonce`
    /// comes back, or the budget runs out.
    AppSend {
        id: u64,
        peer: String,
        stream_id: String,
        nonce: String,
        expect_nonce: String,
        timeout_ms: u64,
    },
    /// Collect the armed side's account of the same exchange.
    AppResult {
        id: u64,
        timeout_ms: u64,
    },
    Counters {
        id: u64,
    },
    Done {
        id: u64,
    },
}

impl Step {
    fn id(&self) -> u64 {
        match self {
            Self::Connect { id, .. }
            | Self::Announce { id, .. }
            | Self::Query { id, .. }
            | Self::ConnectPeer { id, .. }
            | Self::ArmAccept { id }
            | Self::AcceptResult { id, .. }
            | Self::AppArm { id, .. }
            | Self::AppSend { id, .. }
            | Self::AppResult { id, .. }
            | Self::Counters { id }
            | Self::Done { id } => *id,
        }
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
struct StepResult {
    #[serde(default)]
    id: u64,
    #[serde(default)]
    ok: bool,
    #[serde(default)]
    page_type: Option<String>,
    #[serde(default)]
    kind: Option<String>,
    #[serde(default)]
    detail: Option<String>,
    #[serde(default)]
    node_id: Option<String>,
    #[serde(default)]
    counters: Option<serde_json::Value>,
    #[serde(default)]
    peers: Option<serde_json::Value>,
    #[serde(default)]
    elapsed_ms: Option<f64>,
    /// The page's own view of every `RTCPeerConnection` the leaf
    /// built: the state each one reached, the candidates it gathered
    /// and was given, and `getStats`' candidate pairs at the moment
    /// the step settled. The anchor's counters and the page's typed
    /// outcome are both statements ABOUT ICE; this is the engine's.
    #[serde(default)]
    rtc: Option<serde_json::Value>,
    /// The nonce this side DECODED from the peer's frames, and its
    /// own send/receive counts, from the application exchange.
    ///
    /// `seen_nonce` is the whole point: a count says bytes arrived,
    /// and only a nonce the OTHER side minted says whose bytes they
    /// were.
    #[serde(default)]
    seen_nonce: Option<String>,
    #[serde(default)]
    app_sent: Option<u64>,
    #[serde(default)]
    app_received: Option<u64>,
}

impl StepResult {
    fn detail(&self) -> String {
        self.detail.clone().unwrap_or_default()
    }
}

/// One tab's refusal, in the shape the verdict's `errors` carries.
fn step_failure(tab: &str, r: &StepResult) -> Option<String> {
    if r.ok {
        return None;
    }
    Some(format!(
        "tab {tab}: {} {}",
        r.kind.clone().unwrap_or_else(|| "failed".to_owned()),
        r.detail()
    ))
}

type Queue = Arc<Mutex<mpsc::Receiver<(Step, oneshot::Sender<StepResult>)>>>;
type Sender = mpsc::Sender<(Step, oneshot::Sender<StepResult>)>;
type Pending = Arc<Mutex<HashMap<u64, oneshot::Sender<StepResult>>>>;

#[derive(Clone)]
struct Control {
    queues: Arc<HashMap<String, Queue>>,
    pending: Pending,
    /// The exact page origin, echoed as `Access-Control-Allow-Origin`.
    /// Not a wildcard: this control plane hands out a bootstrap
    /// credential, and an API that does that behind `*` is a mistake
    /// worth not making even in a harness.
    origin: String,
}

/// One tab's handle: the queue its page long-polls.
struct Tab {
    name: &'static str,
    tx: Sender,
    ids: Arc<AtomicU64>,
    timeout: Duration,
}

impl Tab {
    async fn run(&self, make: impl FnOnce(u64) -> Step) -> Result<StepResult, String> {
        let id = self.ids.fetch_add(1, Ordering::Relaxed);
        let step = make(id);
        let label = step_label(&step);
        let (tx, rx) = oneshot::channel();
        self.tx
            .send((step, tx))
            .await
            .map_err(|_| format!("tab {} queue closed", self.name))?;
        let result = tokio::time::timeout(self.timeout, rx)
            .await
            .map_err(|_| {
                format!(
                    "tab {} step {id} ({label}) produced no result within {:?}",
                    self.name, self.timeout
                )
            })?
            .map_err(|_| format!("tab {} step {id} dropped", self.name))?;
        // Printed the moment it arrives, not folded into the verdict:
        // a row that dies two steps later still needs the engine's
        // own account of the step that actually went wrong, and
        // `runner.log` is the only file that survives every path.
        if let Some(rtc) = &result.rtc {
            println!(
                "[runner] tab {} step {id} ({label}) ice: {}",
                self.name,
                serde_json::to_string(rtc).unwrap_or_else(|e| format!("(unserializable: {e})"))
            );
        }
        Ok(result)
    }

    /// A step that must succeed, with the page's own reason when it
    /// does not.
    async fn require(&self, make: impl FnOnce(u64) -> Step) -> Result<StepResult, String> {
        let r = self.run(make).await?;
        if let Some(e) = step_failure(self.name, &r) {
            return Err(e);
        }
        Ok(r)
    }
}

// ===================================================================
// The control plane
// ===================================================================

fn cors(origin: &str) -> [(HeaderName, HeaderValue); 1] {
    [(
        header::ACCESS_CONTROL_ALLOW_ORIGIN,
        HeaderValue::from_str(origin).unwrap_or_else(|_| HeaderValue::from_static("null")),
    )]
}

#[derive(Debug, Deserialize)]
struct TabQuery {
    tab: String,
}

/// Long-poll for the next step. 204 when the window elapses, so the
/// page re-asks instead of holding a connection open forever (and so a
/// row that hangs hangs in the RUNNER's step timeout, where the
/// failure message names the step).
async fn next_step(State(s): State<Control>, Query(q): Query<TabQuery>) -> Response {
    let Some(queue) = s.queues.get(&q.tab) else {
        return (StatusCode::NOT_FOUND, cors(&s.origin), "no such tab").into_response();
    };
    let mut rx = queue.lock().await;
    match tokio::time::timeout(Duration::from_secs(20), rx.recv()).await {
        Ok(Some((step, reply))) => {
            s.pending.lock().await.insert(step.id(), reply);
            let body =
                serde_json::to_string(&step).unwrap_or_else(|e| format!("{{\"err\":\"{e}\"}}"));
            (
                StatusCode::OK,
                cors(&s.origin),
                [(header::CONTENT_TYPE, "application/json")],
                body,
            )
                .into_response()
        }
        Ok(None) => (StatusCode::GONE, cors(&s.origin), "queue closed").into_response(),
        Err(_) => (StatusCode::NO_CONTENT, cors(&s.origin)).into_response(),
    }
}

/// A step result. The body is posted as `text/plain` on purpose: a
/// `application/json` content type would make it a non-simple
/// cross-origin request and require a preflight, which is machinery
/// this harness has no reason to own.
async fn step_result(State(s): State<Control>, body: String) -> Response {
    match serde_json::from_str::<StepResult>(&body) {
        Ok(result) => {
            if let Some(reply) = s.pending.lock().await.remove(&result.id) {
                let _ = reply.send(result);
            }
            (StatusCode::OK, cors(&s.origin)).into_response()
        }
        Err(e) => {
            println!("[control] unparseable result {body:?}: {e}");
            (StatusCode::BAD_REQUEST, cors(&s.origin)).into_response()
        }
    }
}

async fn page_log(State(s): State<Control>, body: String) -> Response {
    println!("[page] {body}");
    (StatusCode::OK, cors(&s.origin)).into_response()
}

async fn serve_control(bind: SocketAddr, state: Control) -> Result<(), String> {
    let router = Router::new()
        .route("/matrix/step", get(next_step))
        .route("/matrix/result", post(step_result))
        .route("/matrix/log", post(page_log))
        .with_state(state);
    let listener = tokio::net::TcpListener::bind(bind)
        .await
        .map_err(|e| format!("control plane bind {bind}: {e}"))?;
    tokio::spawn(async move {
        let _ = axum::serve(listener, router).await;
    });
    Ok(())
}

// ===================================================================
// The page server (a child of this binary, inside each namespace)
// ===================================================================

#[derive(Clone)]
struct PageState {
    page: PathBuf,
    browser_dist: PathBuf,
}

fn file_response(path: &Path, content_type: &str) -> Response {
    match std::fs::read(path) {
        Ok(bytes) => (
            StatusCode::OK,
            [(header::CONTENT_TYPE, content_type)],
            bytes,
        )
            .into_response(),
        Err(e) => (StatusCode::NOT_FOUND, format!("{}: {e}", path.display())).into_response(),
    }
}

async fn index(State(s): State<PageState>) -> Response {
    file_response(&s.page.join("matrix.html"), "text/html; charset=utf-8")
}

async fn matrix_js(State(s): State<PageState>) -> Response {
    file_response(&s.page.join("matrix.js"), "text/javascript; charset=utf-8")
}

/// `@net-mesh/browser`'s built bundle under one prefix, so the entry's
/// own `new URL('./net_leaf.js', import.meta.url)` resolves without an
/// import map.
async fn browser_asset(State(s): State<PageState>, AxPath(rel): AxPath<String>) -> Response {
    let path = Path::new(&rel);
    if path
        .components()
        .any(|c| !matches!(c, std::path::Component::Normal(_)))
    {
        return (StatusCode::BAD_REQUEST, "no traversal").into_response();
    }
    let content_type = match path.extension().and_then(|e| e.to_str()) {
        Some("js" | "mjs" | "cjs") => "text/javascript; charset=utf-8",
        Some("wasm") => "application/wasm",
        Some("json" | "map") => "application/json",
        _ => "application/octet-stream",
    };
    file_response(&s.browser_dist.join(path), content_type)
}

async fn run_page_server(cfg: PageServer) -> Result<(), String> {
    let router = Router::new()
        .route("/", get(index))
        .route("/matrix.js", get(matrix_js))
        .route("/browser/{*path}", get(browser_asset))
        .with_state(PageState {
            page: cfg.page,
            browser_dist: cfg.browser_dist,
        });
    let listener = tokio::net::TcpListener::bind(cfg.bind)
        .await
        .map_err(|e| format!("page server bind {}: {e}", cfg.bind))?;
    // The readiness marker is a FILE because the runner is in another
    // network namespace and cannot connect to this socket to probe it,
    // while the filesystem is shared. A runner that raced this would
    // navigate to a refused connection.
    if let Some(marker) = &cfg.ready_marker {
        std::fs::write(marker, format!("{}\n", cfg.bind))
            .map_err(|e| format!("write ready marker {}: {e}", marker.display()))?;
    }
    println!("[page-server] listening on {}", cfg.bind);
    axum::serve(listener, router)
        .await
        .map_err(|e| format!("page server: {e}"))
}

// ===================================================================
// Certificates
// ===================================================================

struct Ca {
    ca_pem: String,
    cert_pem_path: PathBuf,
    key_pem_path: PathBuf,
    /// `base64(SHA-256(SubjectPublicKeyInfo DER))` of the LEAF key —
    /// exactly what Chromium's `--ignore-certificate-errors-spki-list`
    /// takes, and exactly one key wide.
    spki_pin: String,
}

/// Issue a CA and a leaf valid for the anchor's **IP address**.
///
/// The browsers reach the listener at `https://10.99.0.10:8443`, so
/// the leaf carries an `iPAddress` SAN. A DNS-only certificate would
/// have needed a resolvable name inside two namespaces with no
/// resolver, which is a second mechanism to get wrong.
fn issue_certificate(dir: &Path, ip: Ipv4Addr) -> Result<Ca, String> {
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

    let mut leaf_params =
        rcgen::CertificateParams::new(vec![ip.to_string()]).map_err(|e| e.to_string())?;
    leaf_params
        .distinguished_name
        .push(rcgen::DnType::CommonName, ip.to_string());
    leaf_params.subject_alt_names = vec![
        rcgen::SanType::IpAddress(IpAddr::V4(ip)),
        rcgen::SanType::DnsName("localhost".try_into().map_err(|_| "localhost san")?),
    ];
    let leaf_key = rcgen::KeyPair::generate().map_err(|e| e.to_string())?;
    let leaf = leaf_params
        .signed_by(&leaf_key, &issuer)
        .map_err(|e| e.to_string())?;

    let spki_pin = base64::Engine::encode(
        &base64::engine::general_purpose::STANDARD,
        <sha2::Sha256 as sha2::Digest>::digest(rcgen::PublicKeyData::subject_public_key_info(
            &leaf_key,
        )),
    );
    let ca_pem = ca.pem();
    let cert_pem_path = dir.join("cert.pem");
    let key_pem_path = dir.join("key.pem");
    std::fs::write(&cert_pem_path, format!("{}{}", leaf.pem(), ca_pem))
        .map_err(|e| e.to_string())?;
    std::fs::write(&key_pem_path, leaf_key.serialize_pem()).map_err(|e| e.to_string())?;
    Ok(Ca {
        ca_pem,
        cert_pem_path,
        key_pem_path,
        spki_pin,
    })
}

// ===================================================================
// Verdict
// ===================================================================

/// One side's sampled counters plus how long they took to settle.
#[derive(Debug, Clone, Default)]
struct SideReport {
    node_id: String,
    counters: serde_json::Value,
    settle_ms: u64,
    samples: u32,
}

impl SideReport {
    fn to_json(&self) -> serde_json::Value {
        serde_json::json!({
            "node_id": self.node_id,
            "counters": self.counters,
            "settle_ms": self.settle_ms,
            "samples": self.samples,
        })
    }
}

/// The row's **application-delivery** witness, as this runner
/// measured it.
///
/// Two nonces the runner mints — neither page chooses one — plus the
/// anchor's own per-pair application-forwarding counters, sampled in
/// this process either side of the exchange. `rows.rs` asserts the
/// nonces crossed in both directions AND that the anchor's counter
/// did the row-appropriate thing while they did: flat on a direct
/// row, moving in both directions on a relayed one.
///
/// Every field is emitted unconditionally; `rows.rs` refuses a
/// verdict that omits one rather than reading the omission as "no
/// forwarding observed", which on a direct row is indistinguishable
/// from the property under test.
#[derive(Debug, Clone, Default)]
struct AppExchangeReport {
    nonce_a: String,
    nonce_b: String,
    seen_at_a: String,
    seen_at_b: String,
    sent_a_to_b: u64,
    sent_b_to_a: u64,
    received_at_a: u64,
    received_at_b: u64,
    forwarded_pre_ab: u64,
    forwarded_pre_ba: u64,
    forwarded_post_ab: u64,
    forwarded_post_ba: u64,
}

impl AppExchangeReport {
    fn to_json(&self) -> serde_json::Value {
        serde_json::json!({
            "nonce_a": self.nonce_a,
            "nonce_b": self.nonce_b,
            "seen_at_a": self.seen_at_a,
            "seen_at_b": self.seen_at_b,
            // Decimal strings, like every other counter this verdict
            // carries: `JSON.parse` rounds a u64 above 2^53 and the
            // identity these feed is exact arithmetic.
            "sent_a_to_b": self.sent_a_to_b.to_string(),
            "sent_b_to_a": self.sent_b_to_a.to_string(),
            "received_at_a": self.received_at_a.to_string(),
            "received_at_b": self.received_at_b.to_string(),
            "forwarded_pre_ab": self.forwarded_pre_ab.to_string(),
            "forwarded_pre_ba": self.forwarded_pre_ba.to_string(),
            "forwarded_post_ab": self.forwarded_post_ab.to_string(),
            "forwarded_post_ba": self.forwarded_post_ba.to_string(),
        })
    }
}

#[derive(Default)]
struct Verdict {
    /// A's `PeerConnectOutcome.type`.
    page_type: String,
    /// B's `acceptPeer` outcome type — the answerer's half. One pair
    /// has one disposition, so the row asserts the two agree.
    peer_page_type: String,
    page_detail: String,
    a: SideReport,
    b: SideReport,
    anchor: serde_json::Value,
    errors: Vec<String>,
    engine_trust_a: String,
    engine_trust_b: String,
    connect_peer_ms: f64,
    /// What the two drivers reported GRANTING their pages, which the
    /// row's own `media` field is asserted against. Empty until both
    /// pages have opened.
    media: String,
    /// What each Chromium tab's allocator enumerated, per tab:
    /// `{"real":N,"wildcard":N,"nets":[…]}`. Recorded on every path
    /// because it is the row's `Enumeration` witness — load-bearing
    /// in both directions (`rows.rs`), and for the permission-free
    /// row it is the finding itself.
    enumeration: serde_json::Value,
    app: AppExchangeReport,
}

impl Verdict {
    fn to_json(&self, m: &Matrix) -> serde_json::Value {
        serde_json::json!({
            "scenario": m.scenario,
            "nat_a": m.nat_a,
            "nat_b": m.nat_b,
            "expect": m.expect,
            "engine_a": m.engine_a,
            "engine_b": m.engine_b,
            // Attempt counts are NOT reported here. An attempt is one
            // signalling dialog and a leaf's dialog with its anchor is
            // one of them, so the expected count is a property of the
            // ROW (`rows.rs`: two per participant), not a constant this
            // runner could state without restating the derivation.
            "page_type": self.page_type,
            "peer_page_type": self.peer_page_type,
            "page_detail": self.page_detail,
            "connect_peer_ms": self.connect_peer_ms,
            "trust_a": self.engine_trust_a,
            "trust_b": self.engine_trust_b,
            // What the DRIVERS said they granted, not `--media`. A
            // runner that echoed its own flag here could not detect a
            // driver that ignored it.
            "media": self.media,
            "enumeration": self.enumeration,
            "app": self.app.to_json(),
            "a": self.a.to_json(),
            "b": self.b.to_json(),
            "anchor": { "counters": self.anchor },
            "errors": self.errors,
        })
    }
}

// ===================================================================
// main
// ===================================================================

#[tokio::main(flavor = "multi_thread", worker_threads = 4)]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn")),
        )
        .init();
    let mode = match parse_args() {
        Ok(m) => m,
        Err(e) => {
            eprintln!("natsim-browser-matrix: {e}");
            std::process::exit(2);
        }
    };
    match mode {
        Mode::Page(cfg) => {
            if let Err(e) = run_page_server(cfg).await {
                eprintln!("natsim-browser-matrix: {e}");
                std::process::exit(1);
            }
        }
        Mode::Matrix(m) => {
            let mut verdict = Verdict::default();
            if let Err(e) = run_row(&m, &mut verdict).await {
                verdict.errors.push(e);
            }
            // The verdict is written on EVERY path, including a
            // failure before the first step. A row that dies silently
            // shows up in `run_scenario.sh` as a 120 s timeout naming
            // nothing; a verdict with a populated `errors` array fails
            // the row in `tests/natsim.rs` naming the cause.
            let path = m.state.join("browser_outcome.json");
            let body = serde_json::to_vec_pretty(&verdict.to_json(&m)).expect("serialize verdict");
            if let Err(e) = std::fs::write(&path, &body) {
                eprintln!("natsim-browser-matrix: write {}: {e}", path.display());
                std::process::exit(1);
            }
            println!("[runner] verdict: {}", path.display());
            if !verdict.errors.is_empty() {
                for e in &verdict.errors {
                    eprintln!("[runner] error: {e}");
                }
                std::process::exit(1);
            }
        }
    }
}

/// The row's table entry, and a refusal if the script handed us a
/// topology the table does not agree with.
///
/// This is the run-time half of the drift guard whose compile-time
/// half is `tests/natsim_browser.rs`: the script could be edited on a
/// machine where that test was never run, and a row that provisions
/// one topology while asserting another is the one defect a perfect
/// verdict cannot reveal.
fn row_for(m: &Matrix) -> Result<Row, String> {
    let row = rows::all_scenarios()
        .into_iter()
        .find(|r| r.scenario == m.scenario)
        .ok_or_else(|| format!("scenario {} is not in the row table", m.scenario))?;
    if row.nat_a.mode() != m.nat_a || row.nat_b.mode() != m.nat_b {
        return Err(format!(
            "scenario {} was provisioned {} x {} but the table says {} x {}",
            m.scenario, m.nat_a, m.nat_b, row.nat_a, row.nat_b
        ));
    }
    if row.expect.as_str() != m.expect {
        return Err(format!(
            "scenario {} was launched with --expect {} but the table says {}",
            m.scenario, m.expect, row.expect
        ));
    }
    // The grant is part of the row for the same reason the topology
    // is: `browser_cone_cone` and `browser_cone_cone_nomedia` differ
    // in exactly this, so a script that launched one with the other's
    // flag would run the granted environment under the ungranted
    // row's name and every counter in the verdict would agree with
    // it.
    if row.media.flag() != m.media {
        return Err(format!(
            "scenario {} was launched with --media {} but the table says {}",
            m.scenario, m.media, row.media
        ));
    }
    Ok(row)
}

/// Deterministic per-row, per-side secret material.
///
/// Derived rather than random so a re-run of one row reproduces the
/// same two identities, which is what makes a log from two different
/// runs comparable. Any 32 bytes are a valid Ed25519 or X25519 secret.
fn secret_hex(scenario: &str, tab: &str, purpose: &str) -> String {
    let digest = <sha2::Sha256 as sha2::Digest>::digest(
        format!("natsim/{scenario}/{tab}/{purpose}").as_bytes(),
    );
    digest.iter().map(|b| format!("{b:02x}")).collect()
}

/// The stream id both sides open for the application exchange.
///
/// Bit 49 is the leaf's stream discriminator and bit 48 (a channel
/// publication) is deliberately clear, exactly as the Stage 5 ABI
/// witnesses and the browser demo pin theirs. One id on both sides,
/// because a peer-addressed stream is one stream and a mismatch would
/// present as silence rather than as a refusal.
const APP_STREAM_ID: &str = "0x0002000000006012";

/// One direction's nonce: 16 hex digits, derived per row and side.
///
/// Minted HERE and never chosen by a page. A page that picked its own
/// nonce could report a value it had also sent, and the assertion
/// `B decoded exactly what A sent` would then be satisfiable by one
/// side alone. Derived rather than random so a re-run of one row
/// produces the same two values and two logs are comparable.
fn nonce(scenario: &str, tab: &str) -> String {
    secret_hex(scenario, tab, "app-nonce")[..16].to_owned()
}

/// A leaf's node id as the page reports it: 16 lowercase hex digits.
fn parse_node_id(hex: &str) -> Result<u64, String> {
    u64::from_str_radix(hex.trim_start_matches("0x"), 16)
        .map_err(|e| format!("node id {hex:?} is not 16 hex digits: {e}"))
}

/// The anchor's own per-pair APPLICATION forwarding counters for this
/// exact ordered pair, A → B and B → A.
///
/// `forwarded_app_packets` is keyed by the `RoutingHeader`'s `src_id`
/// — a u32 projection of the sender's node id, the same arithmetic
/// `rtc_signalling.rs` does — and the full u64 destination. It
/// EXCLUDES `0x0D02` signalling (`mesh.rs`, the
/// `inner_sub != SUBPROTOCOL_RTC_SIGNAL` arm), so it cannot move
/// because a candidate was trickled, and it cannot go flat because
/// signalling stopped.
fn pair_counts(anchor: &MeshNode, a: u64, b: u64) -> (u64, u64) {
    let a32 = (a & 0xFFFF_FFFF) as u32;
    let b32 = (b & 0xFFFF_FFFF) as u32;
    (
        anchor.forwarded_app_packets(a32, b),
        anchor.forwarded_app_packets(b32, a),
    )
}

/// Create a directory only this (root) user may enter.
///
/// The browsers' `HOME` and `XDG_RUNTIME_DIR`. `0700` is not
/// decoration: Firefox reads the ownership of `$XDG_RUNTIME_DIR` and
/// refuses to run as root when it belongs to somebody else, and a
/// world-writable runtime dir under `/tmp` would be a handle on the
/// browser's profile for any local user.
fn private_dir(path: &Path) -> Result<(), String> {
    std::fs::create_dir_all(path).map_err(|e| format!("create {}: {e}", path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))
            .map_err(|e| format!("chmod 700 {}: {e}", path.display()))?;
    }
    Ok(())
}

async fn run_row(m: &Matrix, verdict: &mut Verdict) -> Result<(), String> {
    let row = row_for(m)?;
    println!(
        "[runner] row {} — {} x {}, expecting {} ({})",
        row.scenario, m.nat_a, m.nat_b, row.expect, row.why
    );

    let work = m.state.join("browser");
    std::fs::create_dir_all(&work).map_err(|e| format!("create {}: {e}", work.display()))?;

    // --- 1. the anchor ---------------------------------------------
    let rtc_bind = SocketAddr::new(IpAddr::V4(m.anchor_ip), m.rtc_port);
    let mut cfg = MeshNodeConfig::new(SocketAddr::new(IpAddr::V4(m.anchor_ip), m.mesh_port), PSK);
    cfg.rtc = Some(RtcConfig {
        serve_bootstrap: true,
        serve_stun: true,
        // **Stage 6 §6.12.2: the anchor's OWN second STUN socket.**
        //
        // Kyra's acceptance item 2 is that both engines connect with
        // the PRODUCT-ADVERTISED configuration and no harness search
        // for whichever one works. So the STUN endpoint this matrix
        // uses is the one the product announces — a second UDP port
        // on the anchor itself, distinct from `rtc_addr`, which the
        // leaf reads out of `GET /rtc/anchor` and puts in its own
        // default `iceServers`. The page configures nothing.
        //
        // A different PORT on the same host is enough, and is what
        // the rule requires: libwebrtc keys its gathering-path
        // shortcut on the source address AND port of a configured
        // STUN server, so an endpoint that is not the peer's ICE
        // socket is not eaten. The previous shape — a whole separate
        // STUN node on another host — proved the mechanism but was
        // exactly the harness search this acceptance item forbids.
        stun_addr: Some(SocketAddr::new(IpAddr::V4(m.anchor_ip), m.stun_port)),
        stun_public_addr: Some(SocketAddr::new(IpAddr::V4(m.anchor_ip), m.stun_port)),
        // The anchor is NOT behind a NAT here: it lives on the
        // simulated internet, and its bind address is the address the
        // browsers reach. `public_addr` is still set explicitly so the
        // announced `rtc_addr` is a fact of this configuration rather
        // than of whatever the socket happened to bind.
        public_addr: Some(rtc_bind),
        // Comfortably above the page's own step budget so a slow ICE
        // in a namespace reports as a page-side timeout naming the
        // step, not as an anchor that retired the attempt underneath
        // it.
        ice_deadline: Duration::from_secs(60),
        ..RtcConfig::new().with_bind_addr(rtc_bind)
    });

    let anchor = Arc::new(
        MeshNode::new(EntityKeypair::generate(), cfg)
            .await
            .map_err(|e| format!("anchor MeshNode::new: {e}"))?,
    );
    anchor.start();
    println!(
        "[runner] anchor node {:016x} rtc {rtc_bind}",
        anchor.node_id()
    );
    // Held for the row's lifetime: dropping the guard unregisters the
    // service, and a leaf that reconnects would find nothing serving
    // its enrolment call again.
    let _enrollment = anchor
        .serve_rpc(ENROLL_SERVICE, Arc::new(Enrollment))
        .map_err(|e| format!("serve {ENROLL_SERVICE}: {e}"))?;

    // --- 1b. the announced STUN endpoint: the anchor's OWN -------
    //
    // **S6_REPORT.md §6.12, closed; §6.12.2, the product fix.**
    // libwebrtc eats every datagram that arrives on an ICE port from
    // an address the port was configured with as a STUN server
    // (`webrtc/p2p/base/stun_port.cc`, `UDPPort::OnReadPacket`,
    // verbatim):
    //
    //     // Look for a response from the STUN server.
    //     if (server_addresses_.find(packet.source_address()) !=
    //         server_addresses_.end()) {
    //       request_manager_.CheckResponse(packet.payload());
    //       return;
    //     }
    //     if (Connection* conn = GetConnection(packet.source_address()))
    //
    // The page's only `iceServers` entry used to be
    // `stun:<the anchor's own rtc_addr>`, and the anchor's ICE host
    // candidate IS that address and port. So every connectivity-check
    // RESPONSE the anchor sent, and every Binding REQUEST it sent, was
    // consumed by the gathering path and never reached the candidate
    // pair: the `return` is before `GetConnection`. Firefox's nICEr
    // reads its sockets directly and has no such rule, which is the
    // whole engine asymmetry.
    //
    // A previous shape of this harness ran a whole separate STUN node
    // on another host. It proved the mechanism, but it was a HARNESS
    // SEARCH for a configuration that works — the thing Kyra's
    // acceptance item 2 forbids. The product now announces its own
    // second endpoint (`stun_addr` / `stun_public_addr` above), the
    // leaf reads it from `GET /rtc/anchor` and defaults its own
    // `iceServers` to it, and this matrix configures NOTHING. What
    // the rows exercise is what an integrator gets.
    //
    // `serve_stun` stays on for the anchor's primary socket too: the
    // leaf's `UdpBlocked` evidence probes the published `rtc_addr`
    // and must keep being answered.

    // --- 2. TLS + the bootstrap listener ---------------------------
    let ca = issue_certificate(&work, m.anchor_ip)?;
    let page_origin = format!("http://localhost:{}", m.page_port);
    let bootstrap_bind = SocketAddr::new(IpAddr::V4(m.anchor_ip), m.bootstrap_port);
    let issuer = Identity::generate();
    let mut boot = BootstrapConfig::new(
        bootstrap_bind,
        Psk::new(PSK),
        issuer.entity_id().clone(),
        BootstrapTls::Operator {
            cert_pem: ca.cert_pem_path.clone(),
            key_pem: ca.key_pem_path.clone(),
        },
        page_origin.clone(),
    );
    // Both namespaces serve their page on `localhost:<page_port>`, so
    // ONE origin covers both browsers.
    boot.offers_per_ip_per_minute = 200;
    let listener = serve_bootstrap(Arc::clone(&anchor), boot)
        .await
        .map_err(|e| format!("bootstrap listener: {e}"))?;
    let bootstrap_url = format!("https://{}:{}", m.anchor_ip, listener.local_addr().port());
    println!("[runner] bootstrap {bootstrap_url}");

    let root_entity = Identity::generate().entity_id().clone();
    let credential = BrowserBootstrapCredential::mint(
        &issuer,
        InviteToken::mint(&root_entity, &bootstrap_url, Duration::from_secs(600)),
        *anchor.public_key(),
        Psk::new(PSK),
        &bootstrap_url,
        Duration::from_secs(86_400),
    )
    .encode();

    // --- 3. the control plane --------------------------------------
    let mut queues: HashMap<String, Queue> = HashMap::new();
    let mut senders: HashMap<&'static str, Sender> = HashMap::new();
    for tab in ["a", "b"] {
        let (tx, rx) = mpsc::channel(4);
        queues.insert(tab.to_owned(), Arc::new(Mutex::new(rx)));
        senders.insert(tab, tx);
    }
    let control_bind = SocketAddr::new(IpAddr::V4(m.anchor_ip), m.control_port);
    serve_control(
        control_bind,
        Control {
            queues: Arc::new(queues),
            pending: Arc::new(Mutex::new(HashMap::new())),
            origin: page_origin.clone(),
        },
    )
    .await?;
    let control_url = format!("http://{}:{}", m.anchor_ip, m.control_port);
    println!("[runner] control {control_url}");

    let ids = Arc::new(AtomicU64::new(1));
    let tab_a = Tab {
        name: "a",
        tx: senders["a"].clone(),
        ids: Arc::clone(&ids),
        timeout: m.step_timeout,
    };
    let tab_b = Tab {
        name: "b",
        tx: senders["b"].clone(),
        ids: Arc::clone(&ids),
        timeout: m.step_timeout,
    };

    // --- 4. page servers + browsers, one per namespace -------------
    let here = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let page_dir = here.join("page");
    let driver_dir = here.join("driver");
    // `net/crates/net/browser-ts/dist`, the built `@net-mesh/browser`
    // bundle, four levels up from `tests/natsim/browser`.
    let browser_dist = here
        .parent()
        .and_then(Path::parent)
        .and_then(Path::parent)
        .map(|net| net.join("browser-ts/dist"))
        .ok_or("locate browser-ts/dist")?;
    for required in [
        browser_dist.join("index.js"),
        browser_dist.join("net_leaf.js"),
        browser_dist.join("net_leaf_bg.wasm"),
    ] {
        if !required.exists() {
            return Err(format!(
                "{} is missing — build the leaf and the wrapper first \
                 (cargo build --release --target wasm32-unknown-unknown in leaf/, \
                 wasm-bindgen --target web, then npm run build in browser-ts/)",
                required.display()
            ));
        }
    }

    let mut page_children = Vec::new();
    for (tab, netns) in [("a", &m.netns_a), ("b", &m.netns_b)] {
        let marker = m.state.join(format!("page_{tab}.ready"));
        let _ = std::fs::remove_file(&marker);
        let exe = std::env::current_exe().map_err(|e| format!("current_exe: {e}"))?;
        let child = tokio::process::Command::new("ip")
            .args(["netns", "exec", netns])
            .arg(&exe)
            .arg("page-server")
            .args(["--bind", &format!("127.0.0.1:{}", m.page_port)])
            .arg("--page")
            .arg(&page_dir)
            .arg("--browser-dist")
            .arg(&browser_dist)
            .arg("--ready")
            .arg(&marker)
            .stdout(Stdio::null())
            .stderr(Stdio::inherit())
            .kill_on_drop(true)
            .spawn()
            .map_err(|e| format!("spawn page server in {netns}: {e}"))?;
        page_children.push(child);
        let deadline = Instant::now() + Duration::from_secs(20);
        while !marker.exists() {
            if Instant::now() > deadline {
                return Err(format!(
                    "page server in {netns} never wrote {}",
                    marker.display()
                ));
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        println!("[runner] page server up in {netns}");
    }

    // A root-owned HOME and XDG_RUNTIME_DIR for the browsers, inside
    // the run's own state dir (`mktemp`'d 700 and root-owned by
    // `run_scenario.sh`). Firefox refuses to start as root while
    // `$XDG_RUNTIME_DIR` belongs to another user, and under `sudo` on
    // the GitHub runner it inherits `/run/user/1001`, owned by
    // `runner` — which is how the control row died with Playwright's
    // `Target page, context or browser has been closed`.
    let browser_home = work.join("home");
    let browser_runtime = work.join("xdg-runtime");
    for dir in [&browser_home, &browser_runtime] {
        private_dir(dir)?;
    }
    let mut driver_a = Driver::spawn(
        &m.netns_a,
        &driver_dir,
        "driver-a",
        &browser_home,
        &browser_runtime,
    )
    .await?;
    let mut driver_b = Driver::spawn(
        &m.netns_b,
        &driver_dir,
        "driver-b",
        &browser_home,
        &browser_runtime,
    )
    .await?;
    verdict.engine_trust_a = driver_a
        .launch(
            &m.engine_a,
            &ca.spki_pin,
            &ca.ca_pem,
            &work.join("profile-a"),
        )
        .await?;
    verdict.engine_trust_b = driver_b
        .launch(
            &m.engine_b,
            &ca.spki_pin,
            &ca.ca_pem,
            &work.join("profile-b"),
        )
        .await?;

    let page_url = |tab: &str| {
        format!(
            "http://localhost:{}/?tab={tab}&control={}",
            m.page_port,
            urlencode(&control_url)
        )
    };
    // The grant, as each DRIVER reports performing it. Both sides
    // must agree: two tabs of one row in two different permission
    // environments is not a row, it is two halves of two rows, and
    // the verdict could only carry one label for them.
    let media_a = driver_a.open(&page_url("a"), &m.media).await?;
    let media_b = driver_b.open(&page_url("b"), &m.media).await?;
    if media_a != media_b {
        return Err(format!(
            "tab a opened with media {media_a} and tab b with {media_b} — the two halves of one \
             row ran in different permission environments"
        ));
    }
    verdict.media = media_a;
    println!("[runner] media grant: {}", verdict.media);

    // --- 5–7. the §9 sequence --------------------------------------
    //
    // Split out of the provisioning above for one reason: §8 and the
    // shutdown below now run on the FAILING path too. The first red
    // run returned at the first bad step, so every failing row's
    // verdict carried `"anchor": {"counters": null}` — and the one
    // question those rows raised ("the page says ICE never connected;
    // what does the anchor say about the same dialog?") was the one
    // question the evidence could not answer.
    let outcome = drive_sequence(
        m,
        verdict,
        &tab_a,
        &tab_b,
        &credential,
        &bootstrap_url,
        &page_origin,
        // The anchor itself, because the row's application witness
        // reads ITS per-pair forwarding counter in this process,
        // either side of the exchange. A page cannot report that
        // number and an endpoint has no business being asked for it.
        &anchor,
    )
    .await;

    // --- 8. the anchor's own ledger --------------------------------
    //
    // Read on EVERY path. On a failing row this is the anchor's own
    // answer to whatever the page claimed: a page reporting an ICE
    // timeout against an anchor whose `direct` term advanced is a
    // contradiction that names the layer to look at, and a page
    // reporting one against `attempted=1 pending=1` says the dialog
    // was opened and never completed on EITHER side.
    match anchor.rtc_ice_stats() {
        Some(ice) => {
            verdict.anchor = serde_json::json!({
                "ice_attempted": ice.attempted.to_string(),
                "ice_direct": ice.direct.to_string(),
                "ice_relayed": ice.relayed.to_string(),
                "ice_failed": ice.failed.to_string(),
                // Native has no such term and should not: a node that
                // signals over UDP cannot have UDP blocked. Reported
                // as a constant so the four-term partition is well
                // formed on both sides.
                "udp_blocked": "0",
            });
            println!(
                "[runner] anchor ice attempted={} direct={} relayed={} failed={} pending={}",
                ice.attempted,
                ice.direct,
                ice.relayed,
                ice.failed,
                ice.pending()
            );
        }
        None => println!("[runner] the anchor has no RTC driver — `webrtc` feature?"),
    }

    for tab in [&tab_a, &tab_b] {
        let _ = tab.run(|id| Step::Done { id }).await;
    }
    let networks_a = driver_a.shutdown().await;
    let networks_b = driver_b.shutdown().await;
    // THE CHROMIUM ENUMERATION OBSERVATION — REQUIRED of a granted
    // row (§6.12), RECORDED on a permission-free one (§11.8).
    //
    // `Row::enumeration()` derives the arm from the row's own `media`
    // field, so the split is one lookup in the table rather than a
    // policy this loop invents. The two arms are different KINDS of
    // statement, not two strengths of one check:
    //
    // * GRANTED rows: a port on `Net[eth0:192.168.10x.x/24:…]` is
    //   REQUIRED and the refusal below is unchanged, verbatim. A
    //   Chromium row that loses interface enumeration otherwise
    //   reports an ICE timeout sixty seconds later, indistinguishable
    //   from a real ICE failure; that one line is what diagnosed
    //   §6.12 and nothing here weakens it.
    //
    // * PERMISSION-FREE rows: `real=0 wildcard=N` is the EXPLANATORY
    //   OBSERVATION, not the outcome under investigation, and the row
    //   is decided by authenticated application delivery instead.
    //   Refusing on it would (a) pin one Chromium build's gating
    //   policy as though it were a promise of this product, and (b)
    //   discard a measurement that §11.8 shows is a WORKING one: a
    //   denied, wildcard-allocated pair reached the STUN endpoint,
    //   gathered srflx, solved `direct` and delivered nonces both
    //   ways. The observation is kept — in the verdict, verbatim, on
    //   every path — rather than erased or promoted to a criterion.
    //
    // Firefox's nICEr has no enumerator stage and logs no `Net[…]` at
    // all; requiring one of it would assert nothing.
    let mut enumeration = serde_json::Map::new();
    for (tab, engine, nets) in [
        ("a", m.engine_a.as_str(), &networks_a),
        ("b", m.engine_b.as_str(), &networks_b),
    ] {
        if engine != "chromium" {
            continue;
        }
        enumeration.insert(
            tab.to_owned(),
            serde_json::json!({
                "real": nets.real,
                "wildcard": nets.wildcard,
                "nets": nets.nets,
                // Which arm of the table this tab was read under, so
                // the artifact says whether the numbers beside it
                // were a criterion or a record.
                "expectation": match row.enumeration() {
                    Enumeration::Required => "required",
                    Enumeration::Observed => "observed",
                },
            }),
        );
        if !row.enumeration().satisfied_by(nets.real) {
            // The CRITERION is unchanged: `real > 0`, of every
            // Chromium tab on a granted row. The causal clause is
            // corrected — §11.8 measured a wildcard-allocated pair
            // delivering application payloads, so "every inbound
            // datagram is dropped before STUN parsing" is not a
            // statement this harness can make. What is left is the
            // observation and the environment the row was built for.
            verdict.errors.push(format!(
                "tab {tab}: {engine} allocated no port on an enumerated network \
                 (real={} wildcard={} nets={:?}) — this row grants camera+microphone and \
                 REQUIRES the enumerated interfaces that grant produces; a tab that lost \
                 them is not the environment it was built for (S6_REPORT.md §6.12, and \
                 §11.8 for why the permission-free rows record this instead)",
                nets.real, nets.wildcard, nets.nets
            ));
        }
        if matches!(row.enumeration(), Enumeration::Observed) {
            println!(
                "[runner] tab {tab}: {engine} ran permission-free and allocated real={} \
                 wildcard={} nets={:?} — RECORDED, not required; this row is decided by \
                 authenticated application delivery (S6_REPORT.md §11.8)",
                nets.real, nets.wildcard, nets.nets
            );
        }
    }
    // Written on every path, pass or fail: for a permission-free row
    // these counters ARE the finding, beside whatever disposition the
    // row reached, so the row's own artifact has to carry them rather
    // than leaving them in a runner log the report cannot cite.
    verdict.enumeration = serde_json::Value::Object(enumeration);
    for mut child in page_children {
        let _ = child.kill().await;
    }

    // Only now: the sequence's own verdict, with the anchor's ledger
    // already in the outcome file beside it.
    outcome?;

    // The runner does NOT decide the row — `tests/natsim.rs` does,
    // against the table. But a runner that noticed the disposition is
    // wrong and said nothing would make the scenario log useless, so
    // the observation is printed here and asserted there.
    let landed = match verdict.page_type.as_str() {
        t if t == Disposition::Direct.page_type() => Some(Disposition::Direct),
        t if t == Disposition::Relayed.page_type() => Some(Disposition::Relayed),
        _ => None,
    };
    match landed {
        Some(d) if d == row.expect => {
            println!("[runner] row {} landed {d}, as expected", row.scenario)
        }
        Some(d) => println!(
            "[runner] row {} landed {d}, expected {} — {}",
            row.scenario, row.expect, row.why
        ),
        None => println!(
            "[runner] row {} reached no disposition: {:?}",
            row.scenario, verdict.page_type
        ),
    }
    Ok(())
}

/// The §9 sequence: connect both leaves, announce, discover, run the
/// dialog under test, exchange application payloads over it, and
/// settle the counters.
///
/// Everything this touches already exists; nothing here provisions.
/// That split is what lets `run_row` read the anchor's own ledger and
/// shut the browsers down on the failing path as well as the good one.
async fn drive_sequence(
    m: &Matrix,
    verdict: &mut Verdict,
    tab_a: &Tab,
    tab_b: &Tab,
    credential: &str,
    bootstrap_url: &str,
    page_origin: &str,
    anchor: &MeshNode,
) -> Result<(), String> {
    let anchor_rtc_addr = format!("{}:{}", m.anchor_ip, m.rtc_port);
    let connect_step = |tab: &'static str| {
        let credential = credential.to_owned();
        let bootstrap_url = bootstrap_url.to_owned();
        let origin = page_origin.to_owned();
        let anchor_rtc_addr = anchor_rtc_addr.clone();
        let scenario = m.scenario.clone();
        move |id: u64| Step::Connect {
            id,
            credential,
            bootstrap_url,
            origin,
            anchor_rtc_addr,
            entity_secret_hex: secret_hex(&scenario, tab, "entity"),
            noise_secret_hex: secret_hex(&scenario, tab, "noise"),
        }
    };

    // B first: its signed announcement has to exist before A can
    // learn B's keys from it, which is the only way A gets them.
    //
    // BOTH sides are driven before either failure is reported. B
    // failing used to end the row on the spot, and "was tab a ever
    // driven at all?" was then unanswerable from the log — the two
    // tabs sit behind two different gateways, so whether the symptom
    // is one-sided is the first thing a NAT row has to say.
    let b_connected = tab_b.run(connect_step("b")).await?;
    let a_connected = tab_a.run(connect_step("a")).await?;
    let refused: Vec<String> = [("b", &b_connected), ("a", &a_connected)]
        .into_iter()
        .filter_map(|(tab, r)| step_failure(tab, r))
        .collect();
    if !refused.is_empty() {
        return Err(refused.join("; "));
    }
    verdict.b.node_id = b_connected
        .node_id
        .clone()
        .ok_or("tab b reported no node id")?;
    verdict.a.node_id = a_connected
        .node_id
        .clone()
        .ok_or("tab a reported no node id")?;
    if verdict.a.node_id == verdict.b.node_id {
        return Err(format!(
            "both browsers came up as node {} — the two sides must be two nodes",
            verdict.a.node_id
        ));
    }
    println!("[runner] a={} b={}", verdict.a.node_id, verdict.b.node_id);

    for tab in [tab_b, tab_a] {
        tab.require(|id| Step::Announce {
            id,
            capabilities: vec![CAPABILITY.to_owned()],
        })
        .await?;
    }

    let peer = verdict.b.node_id.clone();
    let expect_peer = peer.clone();
    let discovered = tab_a
        .require(move |id| Step::Query {
            id,
            capability: CAPABILITY.to_owned(),
            expect_peer,
            timeout_ms: 20_000,
        })
        .await?;
    // §9 step 1, observed: A learned B from a SIGNED announcement, by
    // capability query. Logged because it is the step whose absence
    // produces a `noAnnouncement` outcome two steps later, where the
    // cause is no longer visible.
    println!(
        "[runner] a discovered {} for {CAPABILITY}",
        discovered
            .peers
            .as_ref()
            .map_or_else(|| "(nothing reported)".to_owned(), ToString::to_string)
    );

    // --- 6. the dialog under test ----------------------------------
    //
    // The ANSWERER is armed first, and that ordering is load-bearing:
    // `acceptPeer` answers the verified offer envelope the leaf
    // already holds, so the listener that triggers it has to exist
    // before the offer is sent. Armed after, the offer's `signal`
    // event is delivered to nobody, B never answers, and A reports an
    // `iceTimeout` that says nothing whatever about NAT — a row that
    // would have looked like a relayed result on every flavor.
    tab_b.require(|id| Step::ArmAccept { id }).await?;

    let connected = tab_a
        .require(move |id| Step::ConnectPeer {
            id,
            peer,
            timeout_ms: 60_000,
        })
        .await?;
    verdict.page_type = connected
        .page_type
        .clone()
        .ok_or("tab a reported no connectPeer outcome type")?;
    verdict.page_detail = connected.detail();
    verdict.connect_peer_ms = connected.elapsed_ms.unwrap_or_default();

    let accepted = tab_b
        .require(|id| Step::AcceptResult {
            id,
            timeout_ms: 30_000,
        })
        .await?;
    verdict.peer_page_type = accepted
        .page_type
        .clone()
        .ok_or("tab b reported no acceptPeer outcome type")?;
    println!(
        "[runner] connectPeer settled as {:?} in {:.0} ms; acceptPeer as {:?}",
        verdict.page_type, verdict.connect_peer_ms, verdict.peer_page_type
    );

    // --- 6b. the APPLICATION EXCHANGE ------------------------------
    //
    // The witness the rows did not have. Everything above establishes
    // the disposition of a *dialog*: the typed outcome on both
    // halves, the ICE ledgers, and (in `run_scenario.sh`) the
    // gateways' own conntrack. None of it observes an application
    // payload, and conntrack reply traffic on a direct row can be
    // ICE or Noise rather than anything a page sent. On a relayed row
    // the disposition is not delivery at all: `iceTimeout` says the
    // routed session was KEPT, not that a byte ever crossed it.
    //
    // So: two nonces this runner mints, one per direction, over the
    // public peer-addressed stream surface; and the ANCHOR's own
    // per-pair application counter read here, in the anchor's
    // process, either side of it. `rows.rs` asserts the nonces
    // crossed both ways AND that the counter did the row's own thing
    // while they did — flat on a direct row, moving both ways on a
    // relayed one.
    //
    // The answerer is armed FIRST, for the same reason `arm_accept`
    // is armed before the offer: a fire-and-forget stream does not
    // keep frames for a receiver that is not there yet.
    let a_id = parse_node_id(&verdict.a.node_id)?;
    let b_id = parse_node_id(&verdict.b.node_id)?;
    let nonce_a = nonce(&m.scenario, "a");
    let nonce_b = nonce(&m.scenario, "b");
    verdict.app.nonce_a = nonce_a.clone();
    verdict.app.nonce_b = nonce_b.clone();
    let (pre_ab, pre_ba) = pair_counts(anchor, a_id, b_id);
    verdict.app.forwarded_pre_ab = pre_ab;
    verdict.app.forwarded_pre_ba = pre_ba;

    let arm = {
        let peer = verdict.a.node_id.clone();
        let nonce = nonce_b.clone();
        let expect = nonce_a.clone();
        move |id: u64| Step::AppArm {
            id,
            peer,
            stream_id: APP_STREAM_ID.to_owned(),
            nonce,
            expect_nonce: expect,
        }
    };
    tab_b.require(arm).await?;

    let send = {
        let peer = verdict.b.node_id.clone();
        let nonce = nonce_a.clone();
        let expect = nonce_b.clone();
        let budget = u64::try_from(m.app_exchange.as_millis()).unwrap_or(u64::MAX);
        move |id: u64| Step::AppSend {
            id,
            peer,
            stream_id: APP_STREAM_ID.to_owned(),
            nonce,
            expect_nonce: expect,
            timeout_ms: budget,
        }
    };
    let sent = tab_a.require(send).await?;
    verdict.app.seen_at_a = sent.seen_nonce.clone().unwrap_or_default();
    verdict.app.sent_a_to_b = sent.app_sent.unwrap_or_default();
    verdict.app.received_at_a = sent.app_received.unwrap_or_default();

    let echoed = tab_b
        .require(|id| Step::AppResult {
            id,
            timeout_ms: 5_000,
        })
        .await?;
    verdict.app.seen_at_b = echoed.seen_nonce.clone().unwrap_or_default();
    verdict.app.sent_b_to_a = echoed.app_sent.unwrap_or_default();
    verdict.app.received_at_b = echoed.app_received.unwrap_or_default();

    let (post_ab, post_ba) = pair_counts(anchor, a_id, b_id);
    verdict.app.forwarded_post_ab = post_ab;
    verdict.app.forwarded_post_ba = post_ba;
    println!(
        "[runner] app exchange: a sent {} received {} (saw {:?}), b sent {} received {} (saw \
         {:?}); anchor pair a→b {pre_ab} → {post_ab}, b→a {pre_ba} → {post_ba}",
        verdict.app.sent_a_to_b,
        verdict.app.received_at_a,
        verdict.app.seen_at_a,
        verdict.app.sent_b_to_a,
        verdict.app.received_at_b,
        verdict.app.seen_at_b,
    );

    // --- 7. the counters, once each side has settled ---------------
    //
    // A bounded settle loop, not a widened wait: the assertion is
    // unchanged and exact, and a side that never reaches a terminated
    // attempt reports its LAST sample, which then fails the row with
    // the numbers that were actually there. `samples` and `settle_ms`
    // ride the verdict so "it settled immediately" and "it never
    // settled" are distinguishable afterwards.
    verdict.a = settle(tab_a, verdict.a.node_id.clone(), m.settle).await?;
    verdict.b = settle(tab_b, verdict.b.node_id.clone(), m.settle).await?;
    Ok(())
}

/// Sample one side's counters until every attempt has terminated, or
/// the settle budget runs out.
async fn settle(tab: &Tab, node_id: String, budget: Duration) -> Result<SideReport, String> {
    let started = Instant::now();
    let mut samples = 0u32;
    loop {
        let r = tab.require(|id| Step::Counters { id }).await?;
        samples += 1;
        let last = r.counters.clone().unwrap_or(serde_json::Value::Null);
        let settled = IceCounters::from_json(&last)
            .ok()
            .is_some_and(|c| c.attempted > 0 && c.pending().unwrap_or(1) == 0);
        if settled || started.elapsed() >= budget {
            return Ok(SideReport {
                node_id,
                counters: last,
                settle_ms: u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
                samples,
            });
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}

/// Percent-encode the few characters a URL query value cannot carry
/// verbatim. The only value that goes through here is the control
/// plane's own `http://ip:port` base, so this is deliberately minimal
/// rather than a general encoder nobody audits.
fn urlencode(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 8);
    for ch in s.chars() {
        match ch {
            'A'..='Z' | 'a'..='z' | '0'..='9' | '-' | '.' | '_' | '~' => out.push(ch),
            _ => {
                let mut buf = [0u8; 4];
                for byte in ch.encode_utf8(&mut buf).as_bytes() {
                    out.push_str(&format!("%{byte:02X}"));
                }
            }
        }
    }
    out
}
