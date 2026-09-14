//! Stage 5's browser witnesses, in the same runner and the same
//! ledger as Stage 4b's.
//!
//! # What is under test here, and what was under test in 4b
//!
//! Stage 4b's page owned the transport and nothing above it: it spoke
//! the bootstrap protocol itself, built every packet in wasm from
//! payload bytes the runner had encoded natively, and was explicitly
//! **not** an nRPC client. These witnesses are the other half. The
//! page calls `connect()` and then uses the `@net-mesh/browser`
//! surface — `call`, `openStream`, `announce`, `query` — and every
//! observable is still read **on the anchor**: a real nRPC handler
//! running, `find_best_node` resolving, `peer_session_id` not moving.
//! A witness here failing is the leaf crate or the wrapper failing,
//! never the harness guessing.
//!
//! # The Stage 5 bundle is a HARD dependency
//!
//! These witnesses need `net-mesh-leaf` built to wasm and
//! `@net-mesh/browser` built to a browser-loadable ESM bundle. When
//! either is absent every witness below is recorded **FAIL — absent**
//! with the exact paths and build commands. It is never skipped: a
//! browser job that silently emitted 12 witnesses instead of 19 is
//! indistinguishable from one whose Stage 5 half regressed, and the
//! CI floor exists precisely to make that impossible.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use serde::Serialize;
use tokio::sync::{mpsc, oneshot};

use net::adapter::net::behavior::capability::{CapabilityFilter, CapabilityRequirement};
use net::adapter::net::cortex::{
    RpcContext, RpcHandler, RpcHandlerError, RpcResponsePayload, RpcStatus,
};
use net::adapter::net::MeshNode;

use crate::browser::{Driver, Engine, LaunchSpec};
use crate::udp_block::UdpProfile;
use crate::{hex, rpc_request_frame, unhex, wait_for, Ledger, StepResult};

/// The capability tag the browser announces and the anchor resolves.
const STAGE5_TAG: &str = "stage5.browser";
/// The native service the leaf's nRPC client calls.
const ECHO_SERVICE: &str = "app.stage5.echo";
/// A channel both tabs declare at `openSession`, so a promoted
/// follower has something to restore.
const RESTORE_CHANNEL: &str = "stage5.restore";
/// The native service the fire-and-forget stream's events dispatch to.
const SINK_SERVICE: &str = "app.stage5.sink";

/// How many round trips the reliable witness makes. Enough that a
/// reordering or a dropped-and-not-retransmitted frame shows up, few
/// enough to stay inside one HTTP step.
const RELIABLE_ROUND_TRIPS: usize = 64;
/// Events the fire-and-forget witness sends, and the drop period.
const FAF_EVENTS: usize = 40;
const FAF_DROP_EVERY: u64 = 4;

/// Every Stage 5 witness name, in ledger order. The CI job pins these
/// exactly; the list is here so a rename is one edit and a drop is
/// impossible to do quietly.
pub const WITNESSES: [&str; 7] = [
    "stage5_leaf_handshake_over_the_real_listener",
    "stage5_reliable_round_trip",
    "stage5_nrpc_call_to_a_native_service",
    "stage5_fire_and_forget_tolerates_injected_loss",
    "stage5_find_best_node_returns_the_browser_node",
    "stage5_two_tabs_share_one_identity_without_eviction",
    "stage5_udp_blocked_surfaces_a_typed_failure",
];

// ===================================================================
// The step protocol the Stage 5 page executes
// ===================================================================

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Step5 {
    /// `connect(opts)` through `@net-mesh/browser`.
    Connect {
        id: u64,
        session: String,
        credential: String,
        bootstrap_url: String,
        origin: String,
        /// The anchor's published `rtc_addr`, which is what the
        /// leaf's STUN probe targets.
        anchor_rtc_addr: String,
        stun: Option<String>,
        /// Custodial identity injection (`MeshNodeConfig::entity_keypair`'s
        /// shape). Both tabs get the SAME pair, which is what makes
        /// "one identity in two tabs" a fact rather than a wish.
        entity_secret_hex: Option<String>,
        noise_secret_hex: Option<String>,
        /// Open through §8's `openSession` (Web Lock election,
        /// leader/follower roles, generation fencing) instead of the
        /// plain `connect`. The returned session carries the same
        /// `call`/`subscribe`/`publish`/`announce`/`query`/
        /// `openStream`/`close` surface, so every other step works
        /// unchanged against it.
        use_session: bool,
        /// Declared up front so the new leader can restore them.
        capabilities: Vec<String>,
        subscriptions: Vec<String>,
        /// Pins the Web Lock name, so two tabs contend deliberately
        /// rather than by accident of origin.
        lock_scope: Option<String>,
        /// A `connect` that must reject with a typed failure.
        expect_failure: bool,
    },
    /// `nodeIdHex()` and, if the leaf has a leadership surface, the
    /// tab's role.
    Info {
        id: u64,
        session: String,
    },
    /// One `call(service, payload, timeoutMs)`.
    Call {
        id: u64,
        session: String,
        service: String,
        payload: String,
        timeout_ms: u64,
    },
    /// `payloads.len()` sequential calls, so an ordering assertion
    /// costs one HTTP round trip instead of one per call.
    CallMany {
        id: u64,
        session: String,
        service: String,
        payloads: Vec<String>,
        timeout_ms: u64,
    },
    /// `openStream(opts)` + one `send` per payload, with the page's
    /// DataChannel drop hook armed at `drop_every`.
    StreamSend {
        id: u64,
        session: String,
        reliable: bool,
        /// The publish contract's ids, so the anchor dispatches
        /// these events to a real handler.
        stream_id: String,
        channel_hash: u16,
        payloads: Vec<String>,
        drop_every: u64,
    },
    Announce {
        id: u64,
        session: String,
        capabilities: Vec<String>,
    },
    Query {
        id: u64,
        session: String,
        capability: String,
    },
    /// `probeStunBinding(addr)` — the falsifier for the
    /// `UdpBlocked` typing, exported by `@net-mesh/browser`. The
    /// outcome (`reflexive` | `stunError` | `unanswered`) comes
    /// back in `info`.
    StunProbe {
        id: u64,
        addr: String,
    },
    Close {
        id: u64,
        session: String,
    },
    Done {
        id: u64,
    },
}

impl Step5 {
    pub fn id_mut(&mut self) -> &mut u64 {
        match self {
            Self::Connect { id, .. }
            | Self::Info { id, .. }
            | Self::Call { id, .. }
            | Self::CallMany { id, .. }
            | Self::StreamSend { id, .. }
            | Self::Announce { id, .. }
            | Self::Query { id, .. }
            | Self::StunProbe { id, .. }
            | Self::Close { id, .. }
            | Self::Done { id } => id,
        }
    }

    pub fn id(&self) -> u64 {
        match self {
            Self::Connect { id, .. }
            | Self::Info { id, .. }
            | Self::Call { id, .. }
            | Self::CallMany { id, .. }
            | Self::StreamSend { id, .. }
            | Self::Announce { id, .. }
            | Self::Query { id, .. }
            | Self::StunProbe { id, .. }
            | Self::Close { id, .. }
            | Self::Done { id } => *id,
        }
    }
}

pub type Step5Item = (Step5, oneshot::Sender<StepResult>);
pub type Step5Sender = mpsc::Sender<Step5Item>;
pub type Step5Queue = Arc<tokio::sync::Mutex<mpsc::Receiver<Step5Item>>>;

/// The two tabs the Stage 5 witnesses drive. `a` carries every
/// single-tab witness; `b` exists for the shared-identity one.
pub const TABS: [&str; 2] = ["a", "b"];

/// Step ids live above this so they can never collide with the 4b
/// script's ids — both scripts post results to one `/harness/result`
/// keyed by id.
const STEP5_ID_BASE: u64 = 1_000_000;

/// The anchor's view of this peer, appended to every witness whose
/// failure would otherwise read only as "the call's deadline
/// elapsed".
///
/// §12 refuses a PROVISIONAL peer's calls, announcements and
/// subscriptions, and that refusal reaches the leaf as an elapsed
/// deadline — so a leaf whose `connect()` never ran the enrollment
/// exchange looks, from the page, exactly like a slow anchor. The
/// discriminator is on the anchor, so the ledger carries it.
fn peer_state(anchor: &MeshNode, node_id: u64) -> String {
    let session = anchor.peer_session_id(node_id);
    let provisional = anchor.peer_is_provisional(node_id);
    let reading = if session.is_none() {
        "NO session: the anchor has already reclaimed or evicted this peer, so nothing \
         above the transport could have worked"
    } else if provisional {
        "PROVISIONAL: §12 refuses this peer's calls, announcements and subscriptions, and \
         the refusal arrives at the leaf as an elapsed deadline — so an enrollment \
         exchange that did not run is indistinguishable from a slow anchor unless you \
         read this field"
    } else {
        "ENROLLED: §12 is NOT refusing this peer, so a failure above this line is a real \
         defect in the path under test and not an admission gate"
    };
    format!("session={session:?} provisional={provisional} — {reading}")
}

struct Script5 {
    tabs: HashMap<String, Step5Sender>,
    next_id: u64,
}

impl Script5 {
    async fn run(&mut self, tab: &str, mut step: Step5) -> StepResult {
        let id = self.next_id;
        self.next_id += 1;
        *step.id_mut() = id;
        let Some(tx) = self.tabs.get(tab) else {
            return fail(format!("no Stage 5 tab named {tab}"));
        };
        let (reply_tx, reply_rx) = oneshot::channel();
        if tx.send((step, reply_tx)).await.is_err() {
            return fail("the Stage 5 page server is gone");
        }
        match tokio::time::timeout(Duration::from_secs(180), reply_rx).await {
            Ok(Ok(r)) => r,
            Ok(Err(_)) => fail("the Stage 5 step was dropped"),
            Err(_) => fail("the Stage 5 page did not answer this step in 180 s"),
        }
    }
}

fn fail(msg: impl Into<String>) -> StepResult {
    StepResult {
        ok: false,
        error: Some(msg.into()),
        ..Default::default()
    }
}

// ===================================================================
// Native services the witnesses read
// ===================================================================

/// An echo provider whose ordered call log is the observable: the
/// reliable witness passes only if the bodies reached the handler in
/// the order the browser sent them.
struct Echo(Arc<std::sync::Mutex<Vec<Vec<u8>>>>);

#[async_trait::async_trait]
impl RpcHandler for Echo {
    async fn call(&self, ctx: RpcContext) -> Result<RpcResponsePayload, RpcHandlerError> {
        let body = ctx.payload.body.to_vec();
        self.0
            .lock()
            .expect("the echo log is never poisoned")
            .push(body.clone());
        let mut out = Vec::with_capacity(body.len() + 5);
        out.extend_from_slice(b"echo:");
        out.extend_from_slice(&body);
        Ok(RpcResponsePayload {
            status: RpcStatus::Ok,
            headers: vec![],
            body: Bytes::from(out),
        })
    }
}

/// A sink for the fire-and-forget events. Records every body that
/// reached the handler, in arrival order, and counts.
struct Sink {
    seen: Arc<std::sync::Mutex<Vec<Vec<u8>>>>,
    count: Arc<AtomicUsize>,
}

#[async_trait::async_trait]
impl RpcHandler for Sink {
    async fn call(&self, ctx: RpcContext) -> Result<RpcResponsePayload, RpcHandlerError> {
        self.count.fetch_add(1, Ordering::SeqCst);
        self.seen
            .lock()
            .expect("the sink log is never poisoned")
            .push(ctx.payload.body.to_vec());
        Ok(RpcResponsePayload {
            status: RpcStatus::Ok,
            headers: vec![],
            body: Bytes::from_static(b"sunk"),
        })
    }
}

// ===================================================================
// The bundle
// ===================================================================

/// Where the Stage 5 artefacts are expected, and how to build them.
pub struct Bundle {
    /// `@net-mesh/browser`'s `dist`, served at `/browser/`.
    pub dist: PathBuf,
    /// The leaf's `wasm-bindgen` output, for the size report and as
    /// a fallback when the wrapper's copy step has not run.
    pub wasm_pkg: PathBuf,
}

impl Bundle {
    /// `net/crates/net/{browser-ts/dist, leaf/pkg}` relative to the
    /// harness directory (`net/crates/net/tests/rtc_browser`).
    pub fn locate(harness_root: &Path) -> Self {
        let net = harness_root
            .parent()
            .and_then(Path::parent)
            .map(Path::to_path_buf)
            .unwrap_or_else(|| harness_root.to_path_buf());
        Self {
            dist: net.join("browser-ts/dist"),
            wasm_pkg: net.join("leaf/pkg"),
        }
    }

    /// `Ok(())`, or the exact reason the Stage 5 half cannot run.
    pub fn check(&self) -> Result<(), String> {
        let entry = self.dist.join("index.js");
        let glue = self.dist.join("net_leaf.js");
        let wasm = self.dist.join("net_leaf_bg.wasm");
        let mut missing: Vec<String> = Vec::new();
        for p in [&entry, &glue, &wasm] {
            if !p.exists() {
                missing.push(p.display().to_string());
            }
        }
        if missing.is_empty() {
            return Ok(());
        }
        Err(format!(
            "the Stage 5 bundle is ABSENT — missing {}. Build it with: \
             (1) cd net/crates/net/leaf && cargo build --release --target \
             wasm32-unknown-unknown && wasm-bindgen --target web --out-dir pkg \
             target/wasm32-unknown-unknown/release/net_leaf.wasm; \
             (2) cd net/crates/net/browser-ts && npm install && npm run build \
             (its build copies the leaf's pkg/ next to dist/index.js). \
             This is a FAILURE, not a skip: the CI floor of {} witnesses is what \
             keeps a regressed Stage 5 half from looking like a green Stage 4b one.",
            missing.join(", "),
            12 + WITNESSES.len()
        ))
    }

    /// Raw sizes of the artefacts, for the report. Gzipped sizes are
    /// the leaf/wrapper slices' own exit criterion and are measured
    /// where the bundle is built; this runner records what it serves.
    pub fn sizes(&self) -> Vec<(String, u64)> {
        let mut out = Vec::new();
        for name in [
            "index.js",
            "index.bundle.js",
            "net_leaf.js",
            "net_leaf_bg.wasm",
        ] {
            if let Ok(md) = std::fs::metadata(self.dist.join(name)) {
                out.push((format!("dist/{name}"), md.len()));
            }
        }
        // The leaf's own wasm-bindgen output, which is what the
        // wrapper's build copies into `dist`. Recorded separately so
        // a stale copy in `dist` is visible as a size mismatch.
        for name in ["net_leaf.js", "net_leaf_bg.wasm"] {
            if let Ok(md) = std::fs::metadata(self.wasm_pkg.join(name)) {
                out.push((format!("leaf/pkg/{name}"), md.len()));
            }
        }
        out
    }
}

// ===================================================================
// The witnesses
// ===================================================================

pub struct Cx<'a> {
    pub driver: &'a Driver,
    pub engine: Engine,
    pub anchor: &'a Arc<MeshNode>,
    pub anchor_rtc_addr: SocketAddr,
    pub credential: String,
    pub bootstrap_url: String,
    pub origin: String,
    /// `http://localhost:PORT` — the page server's origin.
    pub page_origin: String,
    pub stun: Option<String>,
    pub bundle: Bundle,
    pub tabs: HashMap<String, Step5Sender>,
    /// How the run's browser was launched, so the UDP profile can
    /// relaunch the SAME browser with exactly one field changed.
    pub launch: LaunchSpec,
}

/// Relaunch the run's browser with its non-proxied UDP on or off, and
/// reopen the Stage 5 tab on it.
///
/// The UDP-blocked profile on a host whose firewall cannot filter
/// same-host traffic is an ENGINE setting, and an engine setting is
/// fixed at launch — so establishing it means a relaunch, and undoing
/// it means another. Every step-queue consumer is re-created with the
/// page, and the sessions from earlier witnesses are gone with it,
/// which is why this witness runs last.
async fn relaunch(cx: &Cx<'_>, udp_off: bool, url: &str, what: &str) -> Result<String, String> {
    let _ = cx.driver.close_page("leaf5-a").await;
    cx.driver.shutdown_browser().await?;
    let mut spec = cx.launch.clone();
    spec.webrtc_udp_off = udp_off;
    let launched = cx.driver.launch(&spec).await?;
    cx.driver.open_page("leaf5-a", url).await?;
    Ok(format!(
        "{} {} was relaunched {what} — UDP: {}",
        cx.engine.as_str(),
        launched.version,
        launched.udp
    ))
}

/// Run every Stage 5 witness. Returns `Err` only for a harness fault
/// (a browser that would not open a tab); a witness that failed is
/// recorded and the run continues, exactly like the 4b half.
#[expect(clippy::too_many_lines, reason = "one linear witness script")]
pub async fn run(cx: Cx<'_>, ledger: &mut Ledger) -> Result<(), String> {
    if let Err(why) = cx.bundle.check() {
        for name in WITNESSES {
            ledger.record(name, false, why.clone());
        }
        return Ok(());
    }
    for (name, bytes) in cx.bundle.sizes() {
        println!("[stage5] bundle {name}: {bytes} bytes raw");
    }

    // --- the native services the witnesses read --------------------
    let echo_log: Arc<std::sync::Mutex<Vec<Vec<u8>>>> = Arc::new(std::sync::Mutex::new(Vec::new()));
    let _echo = cx
        .anchor
        .serve_rpc(ECHO_SERVICE, Arc::new(Echo(Arc::clone(&echo_log))))
        .map_err(|e| format!("serve {ECHO_SERVICE}: {e}"))?;
    let sink_log: Arc<std::sync::Mutex<Vec<Vec<u8>>>> = Arc::new(std::sync::Mutex::new(Vec::new()));
    let sink_count = Arc::new(AtomicUsize::new(0));
    let _sink = cx
        .anchor
        .serve_rpc(
            SINK_SERVICE,
            Arc::new(Sink {
                seen: Arc::clone(&sink_log),
                count: Arc::clone(&sink_count),
            }),
        )
        .map_err(|e| format!("serve {SINK_SERVICE}: {e}"))?;

    let mut script = Script5 {
        tabs: cx.tabs.clone(),
        next_id: STEP5_ID_BASE,
    };

    // Tab `a` carries every single-tab witness.
    let url_a = format!("{}/leaf5.html?tab=a", cx.page_origin);
    cx.driver
        .open_page("leaf5-a", &url_a)
        .await
        .map_err(|e| format!("opening the Stage 5 tab: {e}"))?;

    // What this engine actually offers the leaf, read in the page
    // rather than assumed from the engine name. S0b's finding — that
    // `RTCPeerConnection` is undefined in a worker — is the reason
    // the leaf runs on the main thread, so the same fact is worth
    // recording per engine rather than inferred.
    match cx
        .driver
        .eval(
            "leaf5-a",
            "({ ua: navigator.userAgent, \
               pc: typeof RTCPeerConnection, \
               dc: typeof RTCDataChannel, \
               locks: typeof navigator.locks, \
               idb: typeof indexedDB, \
               crypto: typeof (crypto && crypto.subtle) })",
        )
        .await
    {
        Ok(v) => println!("[stage5] engine surface: {v}"),
        Err(e) => println!("[stage5] engine surface unreadable: {e}"),
    }

    // ONE custodial identity for the whole Stage 5 run, injected the
    // way `MeshNodeConfig::entity_keypair` is. Without it every tab
    // mints its own keypair and "two tabs share one identity" could
    // only ever report "they didn't"; with it, both tabs really do
    // present the same node id and the witness measures the thing it
    // is named after.
    let entity_secret = hex(&random_32());
    let noise_secret = hex(&random_32());
    // The Web Lock the two tabs contend for, pinned so the
    // contention is deliberate.
    let lock_scope = format!("net-mesh/stage5/{}", &entity_secret[..16]);
    let connect_as = |session: &str, expect_failure: bool, use_session: bool| Step5::Connect {
        id: 0,
        session: session.to_string(),
        credential: cx.credential.clone(),
        bootstrap_url: cx.bootstrap_url.clone(),
        origin: cx.origin.clone(),
        anchor_rtc_addr: cx.anchor_rtc_addr.to_string(),
        stun: cx.stun.clone(),
        entity_secret_hex: Some(entity_secret.clone()),
        noise_secret_hex: Some(noise_secret.clone()),
        use_session,
        capabilities: vec![STAGE5_TAG.to_string()],
        subscriptions: vec![RESTORE_CHANNEL.to_string()],
        lock_scope: Some(lock_scope.clone()),
        expect_failure,
    };
    let connect_step =
        |session: &str, expect_failure: bool| connect_as(session, expect_failure, false);

    // ================================================================
    // 1 — handshake through the REAL listener, driven by the leaf
    //
    // WHAT THIS WITNESS DOES NOT PROVE. A session forming does not
    // prove the trickle path works: the anchor learns the browser
    // peer-reflexively from the incoming STUN check, so a leaf whose
    // trickled candidates are ALL discarded by the listener still
    // reaches a DataChannel on a same-host or same-LAN run. (That
    // was a live defect — the leaf's candidates carried no
    // `"type":"candidate"` field — found by reading the leaf, not by
    // this witness.) The candidate path is what
    // `mdns_on_pair_formed` measures, and it measures it on the 4b
    // page; this witness is about the leaf driving the whole
    // sequence, not about which candidate won.
    // ================================================================
    let connected = script.run("a", connect_step("main", false)).await;
    let node_hex = connected.node_id.clone().unwrap_or_default();
    let node_id = u64::from_str_radix(node_hex.trim_start_matches("0x"), 16).unwrap_or(0);
    // The leaf's ORIGIN HASH, which is NOT its node id. Every event
    // payload this harness encodes natively and hands the leaf to
    // send must carry it in the `EventMeta`: the anchor drops a
    // direct peer's frame whose packet-header origin and payload
    // origin disagree ("a direct peer must not run the fold under a
    // forged payload origin"), and the header's origin is the leaf's
    // own. Encoding the node id there instead cost a whole witness —
    // 40 events sent, 40 dropped before admission, and the only
    // artifact was a handler that never ran.
    let origin_hex = connected.origin_hash.clone().unwrap_or_default();
    let origin_hash = u64::from_str_radix(origin_hex.trim_start_matches("0x"), 16).unwrap_or(0);
    {
        let session = if node_id == 0 {
            None
        } else {
            cx.anchor.peer_session_id(node_id)
        };
        let provisional = node_id != 0 && cx.anchor.peer_is_provisional(node_id);
        ledger.record(
            WITNESSES[0],
            connected.ok && node_id != 0 && session.is_some(),
            format!(
                "connect() through @net-mesh/browser {} in {:.0} ms; the leaf reported node \
                 {node_hex}; the ANCHOR has a session for that node id: {session:?} \
                 (provisional={provisional}); no bootstrap protocol, no packet building and \
                 no handshake ran in the page — all of it is net-mesh-leaf{}",
                if connected.ok { "resolved" } else { "FAILED" },
                connected.elapsed_ms.unwrap_or(f64::NAN),
                connected
                    .error
                    .as_deref()
                    .map(|e| format!("; error: {e}"))
                    .unwrap_or_default(),
            ),
        );
    }

    // Nothing below can mean anything without a session.
    if !connected.ok || node_id == 0 {
        let why = format!(
            "no leaf session: {}",
            connected
                .error
                .unwrap_or_else(|| "connect() did not report a node id".into())
        );
        for name in &WITNESSES[1..] {
            ledger.record(name, false, why.clone());
        }
        let _ = cx.driver.close_page("leaf5-a").await;
        return Ok(());
    }

    // ================================================================
    // 2 — a reliable round trip, 64 of them, in order
    //
    // `call()` is the leaf's nRPC client over a RELIABLE stream. The
    // round trip is proven on both sides at once: the browser gets
    // the echo body back for every call, and the anchor's handler
    // saw the bodies in the order they were sent. Ordering is the
    // property a reliable stream adds over a fire-and-forget one, so
    // it is the property asserted.
    // ================================================================
    {
        echo_log.lock().expect("echo log").clear();
        let payloads: Vec<String> = (0..RELIABLE_ROUND_TRIPS)
            .map(|i| hex(format!("s5-reliable-{i:04}").as_bytes()))
            .collect();
        let r = script
            .run(
                "a",
                Step5::CallMany {
                    id: 0,
                    session: "main".into(),
                    service: ECHO_SERVICE.into(),
                    payloads: payloads.clone(),
                    timeout_ms: 20_000,
                },
            )
            .await;
        let replies = r.replies.clone().unwrap_or_default();
        let replies_correct = replies.len() == payloads.len()
            && replies.iter().zip(&payloads).all(|(got, sent)| {
                let want = {
                    let mut v = b"echo:".to_vec();
                    v.extend_from_slice(&unhex(sent));
                    hex(&v)
                };
                *got == want
            });
        let delivered = echo_log.lock().expect("echo log").clone();
        let in_order = delivered.len() == payloads.len()
            && delivered
                .iter()
                .zip(&payloads)
                .all(|(got, sent)| *got == unhex(sent));
        ledger.record(
            WITNESSES[1],
            r.ok && replies_correct && in_order,
            format!(
                "{RELIABLE_ROUND_TRIPS} sequential leaf nRPC round trips on one session: the \
                 browser received {} reply(ies) and every one carried `echo:` + its own \
                 request body={replies_correct}; the ANCHOR's handler ran {} time(s) and saw \
                 the bodies in exactly the order sent={in_order}{}. ANCHOR STATE: {}",
                replies.len(),
                delivered.len(),
                r.error
                    .as_deref()
                    .map(|e| format!("; error: {e}"))
                    .unwrap_or_default(),
                peer_state(cx.anchor, node_id),
            ),
        );
    }

    // ================================================================
    // 3 — nRPC to a native service, correlated
    //
    // The half Stage 4b explicitly did not have: the leaf's own nRPC
    // CLIENT. One call, one typed reply, and the native handler's own
    // record of the body — so a reply the wrapper synthesised
    // locally cannot pass.
    // ================================================================
    {
        echo_log.lock().expect("echo log").clear();
        let nonce = format!("s5-nrpc-{:016x}", rand_u64());
        let r = script
            .run(
                "a",
                Step5::Call {
                    id: 0,
                    session: "main".into(),
                    service: ECHO_SERVICE.into(),
                    payload: hex(nonce.as_bytes()),
                    timeout_ms: 20_000,
                },
            )
            .await;
        let want = {
            let mut v = b"echo:".to_vec();
            v.extend_from_slice(nonce.as_bytes());
            hex(&v)
        };
        let reply_ok = r.reply.as_deref() == Some(want.as_str());
        let handler_saw = echo_log
            .lock()
            .expect("echo log")
            .iter()
            .any(|b| b.as_slice() == nonce.as_bytes());
        ledger.record(
            WITNESSES[2],
            r.ok && reply_ok && handler_saw,
            format!(
                "one `call({ECHO_SERVICE}, {nonce})` through the leaf's own nRPC client: the \
                 browser got the expected typed reply={reply_ok}; the native handler RAN with \
                 exactly that body={handler_saw}. A locally synthesised reply cannot satisfy \
                 the second half{}. ANCHOR STATE: {}",
                r.error
                    .as_deref()
                    .map(|e| format!("; error: {e}"))
                    .unwrap_or_default(),
                peer_state(cx.anchor, node_id),
            ),
        );
    }

    // ================================================================
    // 4 — fire-and-forget under injected DataChannel loss
    //
    // The page's drop hook (`RTCDataChannel.prototype.send`) elides
    // every 4th outbound datagram. On a FIRE-AND-FORGET stream the
    // wire retains no retransmit descriptor, so an elided datagram
    // is genuinely lost. Three facts make the witness:
    //
    //   * the anchor's handler ran for the events that WERE sent and
    //     not for the elided ones — real loss, not a counter;
    //   * the LAST event arrived, so the receiver did not stall at
    //     the first sequence gap (the property that separates
    //     fire-and-forget from reliable);
    //   * the session survived, so loss is tolerated rather than
    //     escalated into a teardown.
    //
    // The payloads are nRPC REQUESTs built with the PRODUCTION
    // encoders on the native side and handed to `openStream(...)
    // .send()` verbatim, under the publish contract's own stream id
    // and channel hash — which is what makes a real handler, not a
    // frame counter, the observable.
    // ================================================================
    {
        sink_log.lock().expect("sink log").clear();
        sink_count.store(0, Ordering::SeqCst);
        // The leaf's own origin, read from the leaf — see the note
        // where it is captured. A payload built under any other
        // origin is dropped by the anchor before admission.
        let mut payloads = Vec::with_capacity(FAF_EVENTS);
        let mut bodies = Vec::with_capacity(FAF_EVENTS);
        let mut stream_id = String::new();
        let mut channel_hash = 0u16;
        for i in 0..FAF_EVENTS {
            let body = format!("s5-faf-{i:04}");
            let frame = rpc_request_frame(
                SINK_SERVICE,
                origin_hash,
                0x5000 + i as u64,
                body.as_bytes(),
            );
            stream_id = format!("{}", frame.stream_id);
            channel_hash = frame.channel_hash_u16;
            payloads.push(hex(&frame.payload));
            bodies.push(body);
        }
        let r = script
            .run(
                "a",
                Step5::StreamSend {
                    id: 0,
                    session: "main".into(),
                    reliable: false,
                    stream_id,
                    channel_hash,
                    payloads,
                    drop_every: FAF_DROP_EVERY,
                },
            )
            .await;
        let dropped = r.dropped.unwrap_or(0);
        // Settle: the surviving events are in flight.
        let expected = FAF_EVENTS as u64 - dropped;
        let _ = wait_for(
            || sink_count.load(Ordering::SeqCst) as u64 >= expected,
            Duration::from_secs(15),
        )
        .await;
        let seen = sink_log.lock().expect("sink log").clone();
        let seen_strings: Vec<String> = seen
            .iter()
            .map(|b| String::from_utf8_lossy(b).to_string())
            .collect();
        // "The receiver did not stall at the first gap" is the
        // property fire-and-forget has and reliable does not, so it
        // is asserted as exactly that: the events AFTER the first
        // lost one arrived.
        //
        // It used to assert that the LAST event arrived, which this
        // arrangement can never satisfy: the hook elides every
        // `FAF_DROP_EVERY`-th outbound datagram and `FAF_EVENTS` is a
        // multiple of it, so the final event is always one of the
        // elided ones. The old criterion was unachievable by
        // construction, not a defect in the path under test.
        let first_gap = bodies.iter().position(|b| !seen_strings.contains(b));
        let arrived_after_gap = first_gap.map_or(0, |gap| {
            bodies[gap + 1..]
                .iter()
                .filter(|b| seen_strings.contains(b))
                .count()
        });
        let lost: Vec<&String> = bodies
            .iter()
            .filter(|b| !seen_strings.contains(b))
            .collect();
        let session_alive = cx.anchor.peer_session_id(node_id).is_some();
        // The drop hook may also elide a control datagram, so the
        // arithmetic is a bound, not an equality: at least
        // `FAF_EVENTS - dropped` events must arrive and at least one
        // must be missing, because a run in which NOTHING was lost
        // proves nothing about loss tolerance.
        let pass = r.ok
            && origin_hash != 0
            && dropped > 0
            && !lost.is_empty()
            && seen_strings.len() as u64 >= expected.saturating_sub(1)
            && arrived_after_gap > 0
            && session_alive;
        ledger.record(
            WITNESSES[3],
            pass,
            format!(
                "{FAF_EVENTS} nRPC REQUEST events on ONE fire-and-forget stream \
                 (reliability=fireAndForget → no retransmit descriptor retained), with the \
                 page's RTCDataChannel.send hook eliding every {FAF_DROP_EVERY}th outbound \
                 datagram: {dropped} datagram(s) elided; the anchor's REAL handler ran {} \
                 time(s); {} event(s) never arrived ({}), so the loss is observed as missing \
                 handler invocations rather than a counter; {arrived_after_gap} event(s) sent \
                 AFTER the first gap ({:?}) still arrived, so the receiver did not stall at \
                 that gap — which is the property fire-and-forget has and a reliable stream \
                 does not; \
                 the session survived the loss={session_alive}{}. The payloads were encoded \
                 under the LEAF's origin {origin_hex:?} (not its node id {node_hex:?}), which \
                 is what the anchor checks an EventMeta origin against — a leaf surface that \
                 does not report its origin leaves this 0 and fails this witness rather than \
                 sending frames that are dropped before admission. ANCHOR STATE: {}",
                seen_strings.len(),
                lost.len(),
                lost.iter()
                    .take(6)
                    .map(|s| s.as_str())
                    .collect::<Vec<_>>()
                    .join(","),
                first_gap.map(|g| bodies[g].clone()),
                r.error
                    .as_deref()
                    .map(|e| format!("; error: {e}"))
                    .unwrap_or_default(),
                peer_state(cx.anchor, node_id),
            ),
        );
    }

    // ================================================================
    // 5 — a native peer's `find_best_node` returns the browser node
    //
    // The anchor is the browser's native peer, and the only one it
    // has: a leaf holds exactly one anchor session by construction,
    // so a second-hop native peer would be testing announcement
    // relay (Stage 6), not the browser leaf. The announcement is the
    // leaf's own, over the transport, and the resolution is the
    // core's own capability index — nothing here is harness-built.
    // ================================================================
    {
        let announced = script
            .run(
                "a",
                Step5::Announce {
                    id: 0,
                    session: "main".into(),
                    capabilities: vec![STAGE5_TAG.to_string()],
                },
            )
            .await;
        let req =
            CapabilityRequirement::from_filter(CapabilityFilter::new().require_tag(STAGE5_TAG));
        let resolved = wait_for(
            || cx.anchor.find_best_node(&req) == Some(node_id),
            Duration::from_secs(20),
        )
        .await;
        let got = cx.anchor.find_best_node(&req);
        // The leaf's own view, for the same fact from the other end.
        let queried = script
            .run(
                "a",
                Step5::Query {
                    id: 0,
                    session: "main".into(),
                    capability: STAGE5_TAG.into(),
                },
            )
            .await;
        ledger.record(
            WITNESSES[4],
            announced.ok && resolved,
            format!(
                "the leaf announced tag `{STAGE5_TAG}` over its own transport \
                 (announce ok={}); the NATIVE anchor's find_best_node(require_tag) \
                 resolved to {got:?} and the browser node is {node_id:#018x} \
                 (match={resolved}); the leaf's own query({STAGE5_TAG}) returned {}{}. \
                 ANCHOR STATE: {}",
                announced.ok,
                queried
                    .peers
                    .as_ref()
                    .map(std::string::ToString::to_string)
                    .unwrap_or_else(|| "nothing".into()),
                announced
                    .error
                    .as_deref()
                    .map(|e| format!("; error: {e}"))
                    .unwrap_or_default(),
                peer_state(cx.anchor, node_id),
            ),
        );
    }

    // ================================================================
    // 6 — two tabs, one identity, no eviction
    //
    // §8: one node per ORIGIN. Both tabs open through `openSession`,
    // which runs the Web Lock election: the holder is the LEADER and
    // runs the node, the other is a FOLLOWER attached over
    // BroadcastChannel and seeing the same API. The criterion is
    // three facts, all read where they are observable:
    //
    //   * both tabs report the SAME node id (one identity),
    //   * exactly one leader and one follower at the SAME generation
    //     (one node, not two racing ones),
    //   * the ANCHOR's session id is unchanged across the second
    //     tab's arrival (no eviction — a replacement session is an
    //     eviction under another name).
    //
    // What this witness does NOT gate on: whether `call()` works. It
    // records both tabs' call outcomes as evidence, because a
    // follower that cannot reach the leader is a real §8 defect —
    // but the reply-channel defect that currently reds every call is
    // a different witness's subject, and gating here would make this
    // one a duplicate of that one.
    // ================================================================
    {
        // Tab a must actually HOLD a session for "tab b did not
        // evict it" to mean anything. The witnesses above deliberately
        // push traffic a provisional peer is not allowed to send, so
        // this session can legitimately be gone by now — reclaimed by
        // §12's bounds or its TTL. Re-establish it and SAY SO, rather
        // than reporting "did not evict = false" about a session that
        // nothing evicted because nothing was there.
        //
        // Tab a is re-opened through `openSession` so both tabs
        // contend for the same Web Lock; the plain `connect` session
        // from witness 1 holds no lock and would make tab b the only
        // claimant.
        let _ = script
            .run(
                "a",
                Step5::Close {
                    id: 0,
                    session: "main".into(),
                },
            )
            .await;
        let leader = script.run("a", connect_as("lead", false, true)).await;
        let leader_hex = leader.node_id.clone().unwrap_or_default();
        let leader_node = u64::from_str_radix(leader_hex.trim_start_matches("0x"), 16).unwrap_or(0);
        let session_before = if leader_node == 0 {
            None
        } else {
            cx.anchor.peer_session_id(leader_node)
        };

        let url_b = format!("{}/leaf5.html?tab=b", cx.page_origin);
        let opened = cx.driver.open_page("leaf5-b", &url_b).await;
        let second = if opened.is_ok() {
            script.run("b", connect_as("second", false, true)).await
        } else {
            fail(format!(
                "the second tab would not open: {}",
                opened.as_ref().err().cloned().unwrap_or_default()
            ))
        };
        let second_hex = second.node_id.clone().unwrap_or_default();
        let same_identity = !leader_hex.is_empty() && second_hex == leader_hex;
        let session_after = if leader_node == 0 {
            None
        } else {
            cx.anchor.peer_session_id(leader_node)
        };
        let no_eviction = session_before.is_some() && session_before == session_after;

        // Roles and generations, read from both tabs.
        let info_a = script
            .run(
                "a",
                Step5::Info {
                    id: 0,
                    session: "lead".into(),
                },
            )
            .await;
        let info_b = script
            .run(
                "b",
                Step5::Info {
                    id: 0,
                    session: "second".into(),
                },
            )
            .await;
        let role_a = info_a.role.clone().unwrap_or_default();
        let role_b = info_b.role.clone().unwrap_or_default();
        let gen_a = info_a.generation.clone().unwrap_or_default();
        let gen_b = info_b.generation.clone().unwrap_or_default();
        // Exactly one leader, and both at the same generation: two
        // leaders, or one leader and a follower fenced at a stale
        // generation, are both "two nodes" wearing one identity.
        let one_leader = (role_a == "leader" && role_b == "follower")
            || (role_a == "follower" && role_b == "leader");
        let same_generation = !gen_a.is_empty() && gen_a == gen_b;

        // Evidence, not a gate — see the header.
        let call_a = script
            .run(
                "a",
                Step5::Call {
                    id: 0,
                    session: "lead".into(),
                    service: ECHO_SERVICE.into(),
                    payload: hex(b"tab-a-after"),
                    timeout_ms: 15_000,
                },
            )
            .await;
        let call_b = if same_identity {
            script
                .run(
                    "b",
                    Step5::Call {
                        id: 0,
                        session: "second".into(),
                        service: ECHO_SERVICE.into(),
                        payload: hex(b"tab-b-after"),
                        timeout_ms: 15_000,
                    },
                )
                .await
        } else {
            fail("not attempted: the second tab did not share the first tab's identity")
        };

        // The frozen-tab leg, with a MEASURED correction from
        // slice 3: on current Chromium a frozen tab KEEPS its Web
        // Lock, so freezing the leader must NOT promote the
        // follower. That is what is asserted — a promotion here
        // would mean the fence can be jumped by a tab the browser
        // merely suspended.
        let frozen = cx.driver.set_lifecycle("leaf5-a", "frozen").await;
        let frozen_info_b = if frozen.is_ok() {
            script
                .run(
                    "b",
                    Step5::Info {
                        id: 0,
                        session: "second".into(),
                    },
                )
                .await
        } else {
            fail("not attempted")
        };
        let thawed = cx.driver.set_lifecycle("leaf5-a", "active").await;
        let still_follower = frozen_info_b.role.as_deref() == Some("follower")
            && frozen_info_b.generation.as_deref() == Some(gen_b.as_str());
        let freeze_leg = if let (Ok(()), Ok(())) = (&frozen, &thawed) {
            format!(
                "tab a was FROZEN through CDP Page.setWebLifecycleState and thawed again; \
                 tab b stayed role={:?} generation={:?} while it was frozen \
                 (still_follower={still_follower}) — which is the CORRECT behaviour and a \
                 correction to the plan's D2: a frozen tab KEEPS its Web Lock on this \
                 engine, so a suspended leader must NOT hand the identity over. Promotion \
                 needs the tab CLOSED, and that path is the leader-lifecycle witness's \
                 subject, not this one's",
                frozen_info_b.role, frozen_info_b.generation
            )
        } else {
            format!(
                "the frozen-tab leg is UNPROVEN on engine {}: {}",
                cx.engine.as_str(),
                frozen
                    .clone()
                    .err()
                    .or_else(|| thawed.clone().err())
                    .unwrap_or_else(|| "no reason reported".into())
            )
        };
        // A leg that could not run does not gate; a leg that ran and
        // reported a promotion does.
        let freeze_pass = frozen.is_err() || thawed.is_err() || still_follower;

        ledger.record(
            WITNESSES[5],
            same_identity && one_leader && same_generation && no_eviction && freeze_pass,
            format!(
                "both tabs opened through §8's openSession on one Web Lock scope. Tab a is \
                 node {} role={role_a:?} generation={gen_a:?}; tab b reported {}{} \
                 role={role_b:?} generation={gen_b:?} — same identity={same_identity}, \
                 exactly one leader and one follower={one_leader}, same \
                 generation={same_generation}. The ANCHOR's session for that node id was \
                 {session_before:?} before tab b arrived and {session_after:?} after, so tab \
                 b did NOT evict tab a's session={no_eviction} (a replacement session would \
                 be an eviction under another name). {freeze_leg}. EVIDENCE, not gated here: \
                 tab a call ok={}, tab b call ok={}{}. ANCHOR STATE: {}",
                if leader_hex.is_empty() {
                    "nothing"
                } else {
                    leader_hex.as_str()
                },
                if second_hex.is_empty() {
                    "nothing"
                } else {
                    second_hex.as_str()
                },
                second
                    .error
                    .as_deref()
                    .map(|e| format!(" (error: {e})"))
                    .unwrap_or_default(),
                call_a.ok,
                call_b.ok,
                call_b
                    .error
                    .as_deref()
                    .map(|e| format!("; tab b error: {e}"))
                    .unwrap_or_default(),
                peer_state(cx.anchor, leader_node),
            ),
        );
        // Close the second session through its own API before the
        // tab goes away, so the anchor sees an orderly departure
        // rather than a transport that vanished.
        if same_identity {
            let _ = script
                .run(
                    "b",
                    Step5::Close {
                        id: 0,
                        session: "second".into(),
                    },
                )
                .await;
        }
        let _ = cx.driver.close_page("leaf5-b").await;
    }

    // ================================================================
    // 7 — a prompt TYPED failure under the UDP-blocked profile
    //
    // LAST, because the profile takes UDP away for as long as it is
    // installed — a host-wide firewall rule on Linux, and this
    // browser's WebRTC UDP on Windows. See `udp_block.rs` for which
    // mechanism runs where and why natsim was rejected.
    //
    // # The falsifier, twice
    //
    // `UdpBlocked` is produced from ONE piece of evidence: the HTTPS
    // bootstrap succeeded while a STUN binding to the anchor's
    // published `rtc_addr` went unanswered. That inference is only
    // sound if a HEALTHY anchor DOES answer an unauthenticated
    // binding — an anchor that silently drops them would make every
    // ICE timeout anywhere look like blocked UDP. So the witness
    // probes the healthy anchor FIRST and requires `reflexive` or
    // `stunError`; `unanswered` there fails this witness with that as
    // the reason, because the typing would be unfalsifiable.
    //
    // Then, with the profile installed, the SAME probe must come back
    // `unanswered`. That is the second half of the same falsifier and
    // it is what proves the profile is actually in force: a
    // relaunched browser that quietly kept its UDP would otherwise
    // let a `connect` that failed for an unrelated reason pass as
    // "typed failure under a blocked network".
    // ================================================================
    {
        let name = WITNESSES[6];
        // Hand the previous witness's session back FIRST. The
        // shared-identity witness leaves tab a's leader session open,
        // and this witness connects the SAME identity twice more
        // (blocked, then the control). An anchor still holding a live
        // peer entry for that node id closes the next bootstrap
        // dialog — observed as `the anchor closed the bootstrap
        // dialog: 1006` on the control leg, which is one identity
        // arriving twice, not a defect in the typed failure. Closing
        // through the session's own API is an orderly departure; the
        // browser relaunch below would otherwise just make the
        // transport vanish.
        let _ = script
            .run(
                "a",
                Step5::Close {
                    id: 0,
                    session: "lead".into(),
                },
            )
            .await;
        // Closing is asynchronous ON THE ANCHOR: the peer entry goes
        // when the transport actually drops, not when the page
        // returns. The control leg below presents the SAME custodial
        // identity, and an anchor still holding a session for it
        // closes the new bootstrap dialog with `1006` — reddening
        // this witness for a reason that has nothing to do with UDP
        // (measured on Chromium: every other condition passed and
        // only the control leg failed). So wait for the identity to
        // be free, and say so if it never is.
        let identity_freed = wait_for(
            || cx.anchor.peer_session_id(node_id).is_none(),
            Duration::from_secs(20),
        )
        .await;
        if !identity_freed {
            println!(
                "[stage5] WARNING: the anchor still holds a session for {node_id:#018x} \
                 20 s after the page closed it; the control leg may fail with 1006 for \
                 that reason rather than for anything about UDP"
            );
        }
        // Witness 7's legs keep the SHARED custodial identity, on
        // purpose and by ruling.
        //
        // Giving them a fresh one makes the control leg green, and
        // was reverted: it would hide the case this witness must be
        // able to see — a page that got a typed failure and RETRIES
        // being refused. The `1006` observed on the control leg is
        // not an identity collision anyway; it is a refused WebSocket
        // HANDSHAKE (HTTP 404 from the trickle route, which a browser
        // reports as a bare 1006 with no reason) caused by the
        // completion owner retiring the bootstrap attempt at
        // DataChannel-open, before the install commits — the same
        // `no such dialog on this anchor` the 4b sweep logs. That
        // repair is core+sdk and is owned elsewhere; this witness's
        // job is to keep failing until it lands.
        let probe_step = || Step5::StunProbe {
            id: 0,
            addr: cx.anchor_rtc_addr.to_string(),
        };
        let healthy = script.run("a", probe_step()).await;
        let healthy_outcome = healthy
            .info
            .clone()
            .unwrap_or_else(|| "no outcome reported".into());
        let probe_is_sound =
            healthy.ok && matches!(healthy_outcome.as_str(), "reflexive" | "stunError");
        match UdpProfile::install(cx.anchor_rtc_addr, cx.engine).await {
            Err(why) => ledger.record(name, false, why),
            Ok(profile) => {
                // The engine-policy profile needs the browser
                // relaunched; the firewall one does not. `establish`
                // is a no-op for the latter, so the sequence below
                // reads the same for both.
                let established = match &profile {
                    UdpProfile::Firewall(_) => Ok(String::from(
                        "the kernel rule needs no relaunch: it applies to the running browser",
                    )),
                    UdpProfile::EngineUdpOff { .. } => {
                        relaunch(&cx, true, &url_a, "with its non-proxied UDP taken away").await
                    }
                };
                let blocked_probe = if established.is_ok() {
                    script.run("a", probe_step()).await
                } else {
                    fail("not attempted: the profile was not established")
                };
                let blocked_outcome = blocked_probe
                    .info
                    .clone()
                    .unwrap_or_else(|| "no outcome reported".into());
                let profile_in_force = blocked_probe.ok && blocked_outcome == "unanswered";
                let blocked = if established.is_ok() {
                    script.run("a", connect_step("blocked", true)).await
                } else {
                    fail("not attempted: the profile was not established")
                };
                profile.remove().await;
                let kind = blocked.kind.clone().unwrap_or_default();
                let message = blocked.message.clone().unwrap_or_default();
                let elapsed = blocked.elapsed_ms.unwrap_or(f64::NAN);
                // The evidence pair `UdpBlocked` is defined by.
                let bootstrap_ok = blocked
                    .evidence
                    .as_ref()
                    .and_then(|e| e.get("bootstrapOk"))
                    .and_then(serde_json::Value::as_bool)
                    == Some(true);
                let stun_failed = blocked
                    .evidence
                    .as_ref()
                    .and_then(|e| e.get("stunProbeFailed"))
                    .and_then(serde_json::Value::as_bool)
                    == Some(true);
                // Both halves: the kind the wrapper reports AND the
                // Rust Display it claims to carry. `IceTimeout`'s
                // text also mentions UDP, so the prefix is matched,
                // never a substring.
                let typed = kind == "udp-blocked"
                    && message.starts_with("rtc: UDP appears blocked:")
                    && !blocked.test_error.unwrap_or(false);
                let prompt = elapsed.is_finite() && elapsed < 45_000.0;
                // The control: with the profile gone the same connect
                // must succeed, so the typed failure is attributable
                // to the missing UDP and not to a broken anchor. For
                // the engine profile "gone" means relaunched without
                // the flag, which is also what undoes it.
                let restored = match &profile {
                    UdpProfile::Firewall(_) => Ok(String::from("the kernel rule was removed")),
                    UdpProfile::EngineUdpOff { .. } => {
                        relaunch(&cx, false, &url_a, "with its UDP restored").await
                    }
                };
                let control = if restored.is_ok() {
                    script.run("a", connect_step("unblocked", false)).await
                } else {
                    fail("not attempted: the browser could not be restored")
                };
                ledger.record(
                    name,
                    typed
                        && prompt
                        && bootstrap_ok
                        && stun_failed
                        && control.ok
                        && probe_is_sound
                        && profile_in_force,
                    format!(
                        "FALSIFIER FIRST: against the HEALTHY anchor an unauthenticated STUN \
                         binding to {} came back {healthy_outcome:?}, so the probe the typing \
                         rests on can distinguish blocked from merely-timed-out \
                         (sound={probe_is_sound}; `unanswered` here would make every ICE \
                         timeout look like blocked UDP). UDP-blocked profile then installed \
                         (never natsim, never a simulated failure): {}. Establishing it: {}. \
                         The SAME probe then came back {blocked_outcome:?}, so the profile \
                         was really in force (in_force={profile_in_force}) rather than a \
                         browser that quietly kept its UDP. connect() rejected in \
                         {elapsed:.0} ms (prompt={prompt}) with kind={kind:?} and \
                         Display={message:?} — the narrow type, matched on the full prefix \
                         rather than a substring because IceTimeout's text also mentions UDP \
                         (typed={typed}); the leaf's own evidence: bootstrapOk={bootstrap_ok}, \
                         stunProbeFailed={stun_failed}. CONTROL: {}; the same connect then \
                         succeeded={} — so the typed failure is attributable to the absent \
                         UDP and not to an unhealthy anchor{}",
                        cx.anchor_rtc_addr,
                        profile.how(),
                        established.as_ref().map_or_else(
                            |e| format!("FAILED — {e}"),
                            std::string::ToString::to_string
                        ),
                        restored.as_ref().map_or_else(
                            |e| format!("the browser could NOT be restored — {e}"),
                            std::string::ToString::to_string
                        ),
                        control.ok,
                        control
                            .error
                            .as_deref()
                            .map(|e| format!("; control error: {e}"))
                            .unwrap_or_default(),
                    ),
                );
            }
        }
    }

    let _ = script.run("a", Step5::Done { id: 0 }).await;
    let _ = cx.driver.close_page("leaf5-a").await;
    println!(
        "[stage5] engine {} ({})",
        cx.engine.as_str(),
        if cx.engine.is_gate() {
            "gate"
        } else {
            "best effort, recorded"
        }
    );
    Ok(())
}

/// A nonce without pulling in a dependency: the wall clock's low bits
/// mixed with the address of a local, which is enough for a body that
/// only has to be unlike the last run's.
fn rand_u64() -> u64 {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0);
    let local = 0u8;
    now ^ ((&raw const local as usize as u64) << 13)
}

/// 32 bytes for a custodial secret. Not cryptographic quality and it
/// does not need to be: it only has to be a fresh, valid scalar that
/// two tabs in ONE harness run share and no other run reuses.
fn random_32() -> [u8; 32] {
    let mut out = [0u8; 32];
    let mut state = rand_u64() | 1;
    for chunk in out.chunks_mut(8) {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        chunk.copy_from_slice(&state.to_le_bytes());
    }
    // Clamp away the two values Ed25519/X25519 scalars must not be.
    out[0] &= 0xF8;
    out[31] = (out[31] & 0x7F) | 0x40;
    out
}
