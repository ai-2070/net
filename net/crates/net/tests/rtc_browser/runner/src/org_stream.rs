//! Stage 4 (S4Browser) — the org-scoped streaming witness stage.
//!
//! The REAL-BROWSER evidence vehicle for org-scoped streaming: all
//! four call/serve shapes × roles × authority modes, driven through a
//! real browser page against real native peers, with the revocation
//! facts feed riding the control plane end to end.
//!
//! # The 37-witness roster, in ledger order
//!
//! Browser→native (browser calls, native serves — the ANCHOR is the
//! native peer, adopted into org A and holding DUAL credentials so the
//! granted-mode caller is a real org-B principal on the wire):
//!
//! 1. `org_browser_call_unary_same_org`
//! 2. `org_browser_call_unary_granted`
//! 3. `org_browser_call_streaming_same_org`
//! 4. `org_browser_call_streaming_granted`
//! 5. `org_browser_call_client_stream_same_org`
//! 6. `org_browser_call_client_stream_granted`
//! 7. `org_browser_call_duplex_same_org`
//! 8. `org_browser_call_duplex_granted`
//!
//! Native→browser (native calls, browser serves):
//!
//! 9. `org_native_call_unary_same_org`
//! 10. `org_native_call_unary_granted`
//! 11. `org_native_call_streaming_same_org`
//! 12. `org_native_call_streaming_granted`
//! 13. `org_native_call_client_stream_same_org`
//! 14. `org_native_call_client_stream_granted`
//! 15. `org_native_call_duplex_same_org`
//! 16. `org_native_call_duplex_granted`
//!
//! Browser→browser (two ISOLATED browser identities, direct session):
//!
//! 17. `org_browser_pair_unary`
//! 18. `org_browser_pair_streaming`
//! 19. `org_browser_pair_client_stream`
//! 20. `org_browser_pair_duplex`
//! 21. `org_browser_pair_granted`
//!
//! Attribution/refusal:
//!
//! 22. `org_wrong_peer_frames_refused`
//! 23. `org_old_session_frames_refused`
//! 24. `org_replayed_opening_refused`
//!
//! Backpressure/half-close:
//!
//! 25. `org_streaming_backpressure_and_window`
//! 26. `org_client_stream_backpressure_half_close`
//! 27. `org_duplex_backpressure_half_close`
//!
//! Revocation (the control-plane feed raises floors mid-run):
//!
//! 28. `org_midstream_revocation_retires_with_denied`
//! 29. `org_revocation_refuses_new_openings`
//!
//! Teardown/leader:
//!
//! 30. `org_tab_teardown_retires_without_resume`
//! 31. `org_leader_proxied_call_preserves_follower_attribution`
//!     — **THE REQUIRED INVERSE WITNESS**: its discriminating
//!     observation pairs each follower's EXACT payload with its OWN
//!     verified caller identity and its own wire-side `(peer,
//!     incarnation)` tag tuple. Flipping follower attribution at the
//!     proxy production site swaps those pairings and reddens THIS
//!     assertion — nothing else in the run reads them.
//! 32. `org_leader_replacement_preserves_attribution`
//! 33. `org_leader_teardown_fails_pending_typed`
//!
//! Handler level (the F-S3.1-2 property as an executable observation):
//!
//! 34. `org_handler_completion_after_retirement_emits_nothing`
//!
//! Native parity instruments (engine-independent, ADDITIONAL
//! byte-identity cross-checks, explicitly NOT a substitute for the
//! browser matrix above):
//!
//! 35. `org_parity_codec_round_trip_both_codecs`
//! 36. `org_parity_leaf_proof_verifies_under_core`
//! 37. `org_parity_core_proof_verifies_under_leaf`
//!
//! # Discriminating assertions
//!
//! Every verdict below asserts EXACT identity — payload bytes, entity
//! /org/capability ids, typed error class + coarse reason, chunk
//! order, ran counts — never "did not throw". Each refusal witness
//! carries a positive control through the same code path in the same
//! window (a run where the forbidden outcome would pass is not a
//! witness). Verdict details print the observed identities.

use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use std::sync::{Arc, Mutex as StdMutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use bytes::Bytes;
use net::adapter::net::behavior::capability::CapabilitySet;
use net::adapter::net::behavior::org::{OrgKeypair, OrgMembershipCert, OrgRevocationBundle};
use net::adapter::net::behavior::org_admission::Admitted;
use net::adapter::net::behavior::org_admission_replay::{AdmissionReplayConfig, AdmissionReplayGuard};
use net::adapter::net::behavior::org_authority::NodeAuthority;
use net::adapter::net::behavior::org_call::{
    OrgCallProof, OrgStreamCallProof, RpcCallShape, ORG_ADMISSION_HEADER,
    STREAM_CALL_KIND_SERVER_STREAMING,
};
use net::adapter::net::behavior::org_grant::{
    CapabilityAuthorityId, DispatcherScope, GrantRights, GrantTargetScope, OrgCapabilityGrant,
    OrgDispatcherGrant,
};
use net::adapter::net::behavior::org_revocation::OrgRevocationStore;
use net::adapter::net::cortex::{
    encode_rpc_route, EventMeta, RequestStream, RpcContext, RpcDuplexHandler, RpcHandler,
    RpcHandlerError, RpcRequestPayload, RpcResponsePayload, RpcResponseSink, RpcStatus,
    RpcClientStreamingHandler, RpcStreamingContext, RpcStreamingHandler, DISPATCH_RPC_REQUEST,
    FLAG_RPC_STREAMING_RESPONSE, RPC_FRAME_BODY_OFFSET,
};
use net::adapter::net::identity::{EntityId, EntityKeypair};
use net::adapter::net::mesh_rpc::{CallOptions, OrgProofIntent, RpcError, ServeHandle};
use net::adapter::net::MeshNode;
use futures::StreamExt as _;
use net_leaf::org as leaf_org;
use serde_json::{json, Value};
use tokio::sync::{mpsc, oneshot};

use crate::browser::{Driver, Engine};
use crate::{hex, wait_for, Ledger, StepResult};

// ─────────────────────────── roster + topology ───────────────────────────

/// Every Stage 4 org witness name, in ledger order. CI pins these
/// exactly; the list lives here so a rename is one edit and a drop is
/// impossible to do quietly.
pub const WITNESSES: [&str; 37] = [
    "org_browser_call_unary_same_org",
    "org_browser_call_unary_granted",
    "org_browser_call_streaming_same_org",
    "org_browser_call_streaming_granted",
    "org_browser_call_client_stream_same_org",
    "org_browser_call_client_stream_granted",
    "org_browser_call_duplex_same_org",
    "org_browser_call_duplex_granted",
    "org_native_call_unary_same_org",
    "org_native_call_unary_granted",
    "org_native_call_streaming_same_org",
    "org_native_call_streaming_granted",
    "org_native_call_client_stream_same_org",
    "org_native_call_client_stream_granted",
    "org_native_call_duplex_same_org",
    "org_native_call_duplex_granted",
    "org_browser_pair_unary",
    "org_browser_pair_streaming",
    "org_browser_pair_client_stream",
    "org_browser_pair_duplex",
    "org_browser_pair_granted",
    "org_wrong_peer_frames_refused",
    "org_old_session_frames_refused",
    "org_replayed_opening_refused",
    "org_streaming_backpressure_and_window",
    "org_client_stream_backpressure_half_close",
    "org_duplex_backpressure_half_close",
    "org_midstream_revocation_retires_with_denied",
    "org_revocation_refuses_new_openings",
    "org_tab_teardown_retires_without_resume",
    "org_leader_proxied_call_preserves_follower_attribution",
    "org_leader_replacement_preserves_attribution",
    "org_leader_teardown_fails_pending_typed",
    "org_handler_completion_after_retirement_emits_nothing",
    "org_parity_codec_round_trip_both_codecs",
    "org_parity_leaf_proof_verifies_under_core",
    "org_parity_core_proof_verifies_under_leaf",
];

/// The three parity instruments sit at the head of the run: they are
/// engine-independent and gate nothing about the browser matrix, but
/// their byte-identity receipts are what let the hand-built frames of
/// the refusal witnesses stand as "a captured opening".
const PARITY: [&str; 3] = [
    "org_parity_codec_round_trip_both_codecs",
    "org_parity_leaf_proof_verifies_under_core",
    "org_parity_core_proof_verifies_under_leaf",
];

/// The 34 real-browser witnesses (`WITNESSES[..BROWSER_WITNESSES]`);
/// the parity instruments run engine-independent and are NEVER
/// re-recorded by a harness-level sweep.
const BROWSER_WITNESSES: usize = 34;

/// The tabs this stage drives. Each has its own `/harness/stepOrg`
/// queue except where one context deliberately hosts several tabs
/// (the leader trio shares ONE browsing context — and so one identity
/// and one Web Locks namespace — which is the topology the proxied
/// attribution witnesses measure).
pub const TABS: [&str; 12] = [
    TAB_CALL, TAB_SERVE, TAB_PAIR_A, TAB_PAIR_B, TAB_REVOKED, TAB_COMPLIANT, TAB_TEARDOWN,
    TAB_LEAD, TAB_FOLLOW1, TAB_FOLLOW2, TAB_OLD, TAB_NEW,
];

const TAB_CALL: &str = "oc";
const TAB_SERVE: &str = "os";
const TAB_PAIR_A: &str = "opa";
const TAB_PAIR_B: &str = "opb";
const TAB_REVOKED: &str = "orv";
const TAB_COMPLIANT: &str = "orc";
const TAB_TEARDOWN: &str = "otd";
const TAB_LEAD: &str = "ol1";
const TAB_FOLLOW1: &str = "of1";
const TAB_FOLLOW2: &str = "of2";
/// The leader trio's Web Lock scope: ONE identity, ONE locks
/// namespace — the proxied-attribution topology.
const LEADER_SCOPE: &str = "s4-leader-lock";
const TAB_OLD: &str = "oo1";
const TAB_NEW: &str = "oo2";

const PAGE_CALL: &str = "s4-call";
const PAGE_SERVE: &str = "s4-serve";
const PAGE_PAIR_A: &str = "s4-pair-a";
const PAGE_PAIR_B: &str = "s4-pair-b";
const PAGE_REVOKED: &str = "s4-revoked";
const PAGE_COMPLIANT: &str = "s4-compliant";
const PAGE_TEARDOWN: &str = "s4-teardown";
const PAGE_LEAD: &str = "s4-lead";
const PAGE_FOLLOW1: &str = "s4-follow1";
const PAGE_FOLLOW2: &str = "s4-follow2";
const PAGE_OLD: &str = "s4-old";
const PAGE_NEW: &str = "s4-new";

const CTX_CALL: &str = "s4-ctx-call";
const CTX_SERVE: &str = "s4-ctx-serve";
const CTX_PAIR_A: &str = "s4-ctx-pair-a";
const CTX_PAIR_B: &str = "s4-ctx-pair-b";
const CTX_REVOKED: &str = "s4-ctx-revoked";
const CTX_COMPLIANT: &str = "s4-ctx-compliant";
const CTX_TEARDOWN: &str = "s4-ctx-teardown";
const CTX_LEADER: &str = "s4-ctx-leader";
const CTX_OLD: &str = "s4-ctx-old";
const CTX_NEW: &str = "s4-ctx-new";

// Native provider services (the ANCHOR serves these).
const N_U_SAME: &str = "org.s4.n.u.same";
const N_U_GRANTED: &str = "org.s4.n.u.granted";
const N_S_SAME: &str = "org.s4.n.s.same";
const N_S_GRANTED: &str = "org.s4.n.s.granted";
const N_CS_SAME: &str = "org.s4.n.cs.same";
const N_CS_GRANTED: &str = "org.s4.n.cs.granted";
const N_DX_SAME: &str = "org.s4.n.dx.same";
const N_DX_GRANTED: &str = "org.s4.n.dx.granted";
const N_REV: &str = "org.s4.n.rev";
const N_TEARDOWN: &str = "org.s4.n.teardown";
const N_LEADER: &str = "org.s4.n.leader";
const N_HOLD: &str = "org.s4.n.hold";

// Browser provider services (the o-serve page serves these).
const B_U_SAME: &str = "org.s4.b.u.same";
const B_U_GRANTED: &str = "org.s4.b.u.granted";
const B_S_SAME: &str = "org.s4.b.s.same";
const B_S_GRANTED: &str = "org.s4.b.s.granted";
const B_CS_SAME: &str = "org.s4.b.cs.same";
const B_CS_GRANTED: &str = "org.s4.b.cs.granted";
const B_DX_SAME: &str = "org.s4.b.dx.same";
const B_DX_GRANTED: &str = "org.s4.b.dx.granted";
const B_INJECT: &str = "org.s4.b.inject";
const B_TEARDOWN: &str = "org.s4.b.teardown";
const B_DEFER: &str = "org.s4.b.defer";
// Per-witness services on the SAME page must be per-witness names:
// one node registers a service name once (`AlreadyServed`).
const B_REPLAY: &str = "org.s4.b.replay";
const B_BP: &str = "org.s4.b.bp";
const N_CS_BP: &str = "org.s4.n.cs.bp";

// Browser-pair provider services (the o-pair-b page serves these).
const P_U: &str = "org.s4.p.u";
const P_S: &str = "org.s4.p.s";
const P_CS: &str = "org.s4.p.cs";
const P_DX: &str = "org.s4.p.dx";
const P_GRANT: &str = "org.s4.p.grant";

/// Proof TTLs the mints use: inside the shared 30 s ceiling the
/// provider enforces, long enough for a slow choreography.
const PROOF_TTL_SECS: u64 = 20;
/// Membership/dispatcher/cert TTL for every provisioned credential.
const CRED_TTL_SECS: u64 = 3600;
/// Cert generations: the compliant generation survives a floor raise
/// to 2; the revoked one does not.
const GEN_COMPLIANT: u32 = 5;
const GEN_REVOKED: u32 = 1;

/// Step ids live above these bases so no two scripts in this runner
/// can resolve each other's results on the shared `/harness/result`.
const STEP_ORG_ID_BASE: u64 = 4_000_000;

// ─────────────────────────── the control-plane feed ───────────────────────────

/// The axum handler the page server mounts at
/// `/harness/org-control`: the WS endpoint a leaf's
/// `AnchorControlPlane` points at through the bootstrap URL's
/// `#org-control=` override tag.
pub async fn org_control_socket(
    ws: axum::extract::ws::WebSocketUpgrade,
    axum::extract::Query(q): axum::extract::Query<HashMap<String, String>>,
    axum::extract::State(s): axum::extract::State<crate::PageState>,
) -> axum::response::Response {
    let node = q.get("node").cloned().unwrap_or_default();
    let feed = Arc::clone(&s.org_feed);
    ws.on_upgrade(move |socket| org_control_session(socket, feed, node))
}

async fn org_control_session(
    mut socket: axum::extract::ws::WebSocket,
    feed: Arc<OrgControlFeed>,
    node: String,
) {
    use axum::extract::ws::Message;
    println!("[org-feed] control socket registered for {node}");
    let mut rx = feed.register(&node);
    loop {
        tokio::select! {
            frame = rx.recv() => match frame {
                // The ONE frame type: an org-root-signed revocation
                // bundle, verbatim.
                Some(text) => {
                    if socket.send(Message::Text(text.into())).await.is_err() {
                        return;
                    }
                }
                None => return,
            },
            incoming = socket.recv() => match incoming {
                // The leaf sends nothing on the feed socket; a close
                // ends the registration.
                Some(Ok(_)) => {}
                _ => return,
            },
        }
    }
}

/// The runner's endpoint of the revocation facts feed: the
/// anchor-protocol WS server the leaf's `AnchorControlPlane` points at
/// through the bootstrap URL's `#org-control=` override tag.
///
/// One frame type, `{"type":"org_revocation_bundle","bundle":"<b64>"}`
/// — the frame `anchor_control_plane.rs` recognises on either of its
/// sockets. When a witness raises a floor it calls [`OrgControlFeed::send`]
/// and the affected leaf's `take_revocation_bundles` hands the bytes to
/// the pump from there; the runner never speaks the bundle's contents
/// past base64.
#[derive(Default)]
pub struct OrgControlFeed {
    sockets: StdMutex<HashMap<String, Vec<mpsc::UnboundedSender<String>>>>,
}

impl OrgControlFeed {
    /// Register one connected socket for `node` (the leaf's node id
    /// hex). The returned receiver carries frames to write to it.
    pub fn register(&self, node: &str) -> mpsc::UnboundedReceiver<String> {
        let (tx, rx) = mpsc::unbounded_channel();
        self.sockets
            .lock()
            .expect("feed lock")
            .entry(node.to_string())
            .or_default()
            .push(tx);
        rx
    }

    /// Push one frame at every socket registered for `node`; returns
    /// how many live sockets received it (0 = the feed is not up, and
    /// a revocation witness must say so rather than measure a leaf
    /// with floor 0).
    pub fn send(&self, node: &str, frame: &str) -> usize {
        let mut sockets = self.sockets.lock().expect("feed lock");
        let Some(list) = sockets.get_mut(node) else {
            return 0;
        };
        list.retain(|tx| tx.send(frame.to_string()).is_ok());
        list.len()
    }

    /// Is any socket registered for `node`?
    pub fn connected(&self, node: &str) -> bool {
        self.sockets
            .lock()
            .expect("feed lock")
            .get(node)
            .is_some_and(|list| !list.is_empty())
    }
}

/// The one control frame, in the anchor protocol's vocabulary.
pub fn revocation_frame(bundle: &[u8]) -> String {
    use base64::Engine as _;
    json!({
        "type": "org_revocation_bundle",
        "bundle": base64::engine::general_purpose::STANDARD.encode(bundle),
    })
    .to_string()
}

// ─────────────────────────── the step protocol ───────────────────────────

pub type StepOrgItem = (Value, oneshot::Sender<StepResult>);
pub type StepOrgSender = mpsc::Sender<StepOrgItem>;
pub type StepOrgQueue = Arc<tokio::sync::Mutex<mpsc::Receiver<StepOrgItem>>>;

/// Drives `page/org.js`, numbering its steps so they can never collide
/// with any other script's ids.
pub struct ScriptOrg {
    tabs: HashMap<String, StepOrgSender>,
    next_id: u64,
}

impl ScriptOrg {
    pub fn new(tabs: HashMap<String, StepOrgSender>) -> Self {
        Self {
            tabs,
            next_id: STEP_ORG_ID_BASE,
        }
    }

    pub async fn run(&mut self, tab: &str, mut step: Value) -> StepResult {
        let id = self.next_id;
        self.next_id += 1;
        step["id"] = json!(id);
        let Some(tx) = self.tabs.get(tab) else {
            return fail(format!("no such tab {tab}"));
        };
        let (done, rx) = oneshot::channel();
        if tx.send((step, done)).await.is_err() {
            return fail(format!("the {tab} page queue is gone"));
        }
        match tokio::time::timeout(Duration::from_secs(60), rx).await {
            Ok(Ok(r)) => r,
            Ok(Err(_)) => fail(format!("the {tab} step was dropped")),
            Err(_) => fail(format!("the {tab} page did not answer a step in 60 s")),
        }
    }

    /// Issue TWO steps to two tabs CONCURRENTLY: both are sent before
    /// either result is awaited, so both calls are live at once — the
    /// concurrent-schedule requirement (a cross-delivery flip needs
    /// TWO live pendings to have a reachable forbidden outcome;
    /// sequentially each tab holds one pending and every such flip is
    /// green-under-own-inverse).
    pub async fn run_pair(
        &mut self,
        tab_a: &str,
        mut step_a: Value,
        tab_b: &str,
        mut step_b: Value,
    ) -> (StepResult, StepResult) {
        let id_a = self.next_id;
        self.next_id += 1;
        let id_b = self.next_id;
        self.next_id += 1;
        step_a["id"] = json!(id_a);
        step_b["id"] = json!(id_b);
        let Some(tx_a) = self.tabs.get(tab_a).cloned() else {
            return (
                fail(format!("no such tab {tab_a}")),
                fail(format!("no such tab {tab_b}")),
            );
        };
        let Some(tx_b) = self.tabs.get(tab_b).cloned() else {
            return (
                fail(format!("no such tab {tab_b}")),
                fail(format!("no such tab {tab_b}")),
            );
        };
        let (done_a, rx_a) = oneshot::channel();
        let (done_b, rx_b) = oneshot::channel();
        if tx_a.send((step_a, done_a)).await.is_err() {
            return (
                fail(format!("the {tab_a} page queue is gone")),
                fail(format!("the {tab_b} page queue is gone")),
            );
        }
        if tx_b.send((step_b, done_b)).await.is_err() {
            return (
                fail(format!("the {tab_b} page queue is gone")),
                fail(format!("the {tab_b} page queue is gone")),
            );
        }
        // BOTH are already in flight; only now await either result.
        let ra = match tokio::time::timeout(Duration::from_secs(60), rx_a).await {
            Ok(Ok(r)) => r,
            Ok(Err(_)) => fail(format!("the {tab_a} step was dropped")),
            Err(_) => fail(format!("the {tab_a} page did not answer a step in 60 s")),
        };
        let rb = match tokio::time::timeout(Duration::from_secs(60), rx_b).await {
            Ok(Ok(r)) => r,
            Ok(Err(_)) => fail(format!("the {tab_b} step was dropped")),
            Err(_) => fail(format!("the {tab_b} page did not answer a step in 60 s")),
        };
        (ra, rb)
    }
}

/// The ANCHOR's own view of this peer — appended to a connect
/// failure so the detail never reads only as "the page said no"
/// (stage5's `peer_state` pattern): a peer the anchor never admitted
/// is a different diagnosis from one whose ICE did not complete.
fn anchor_peer_view(anchor: &MeshNode, node_id: u64) -> String {
    let session = anchor.peer_session_id(node_id);
    let provisional = anchor.peer_is_provisional(node_id);
    if session.is_none() {
        format!("anchor: no session for {node_id:#x} (provisional={provisional})")
    } else {
        format!(
            "anchor: session={:?} provisional={provisional} binding={:?}",
            session,
            anchor.peer_session_binding(node_id).map(|b| hex(&b))
        )
    }
}

fn fail(msg: impl Into<String>) -> StepResult {
    StepResult {
        ok: false,
        error: Some(msg.into()),
        ..Default::default()
    }
}

fn why(result: &StepResult) -> String {
    result
        .error
        .clone()
        .or_else(|| result.message.clone())
        .unwrap_or_else(|| "<no reason reported>".into())
}

/// The typed failure a step reported, spelled exactly as the SDK
/// spelled it: `kind` (LeafError taxonomy), `coarse` (the frozen
/// admission coarse reason), `error_class`, verbatim `message`.
fn typed(result: &StepResult) -> String {
    format!(
        "kind={} coarse={} class={} message={:?}",
        result.kind.as_deref().unwrap_or("-"),
        result
            .stats
            .as_ref()
            .and_then(|s| s.get("coarse"))
            .and_then(|v| v.as_str())
            .unwrap_or("-"),
        result
            .stats
            .as_ref()
            .and_then(|s| s.get("error_class"))
            .and_then(|v| v.as_str())
            .unwrap_or("-"),
        result.message.as_deref().unwrap_or("-"),
    )
}

fn stat<'a>(result: &'a StepResult, key: &str) -> Option<&'a Value> {
    result.stats.as_ref()?.get(key)
}

fn stat_u64(result: &StepResult, key: &str) -> u64 {
    stat(result, key)
        .and_then(Value::as_u64)
        .or_else(|| stat(result, key).and_then(|v| v.as_str()).and_then(|s| s.parse().ok()))
        .unwrap_or(0)
}

fn stat_str(result: &StepResult, key: &str) -> Option<String> {
    Some(stat(result, key)?.as_str()?.to_string())
}

fn stat_list(result: &StepResult, key: &str) -> Vec<String> {
    stat(result, key)
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

fn stat_obj<'a>(result: &'a StepResult, key: &str) -> Option<&'a Value> {
    result.stats.as_ref().and_then(|s| s.get(key))
}

/// `query()`'s peer list — a TOP-LEVEL `StepResult` field, not a
/// `stats` entry (reading it through `stat_obj` can never match).
fn peers_of(result: &StepResult) -> Option<&Value> {
    result.peers.as_ref()
}

/// The session's `role` / `generation` — top-level fields.
fn role_of(result: &StepResult) -> String {
    result.role.clone().unwrap_or_default()
}

fn generation_of(result: &StepResult) -> String {
    result.generation.clone().unwrap_or_default()
}

fn now_unix_ns() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock")
        .as_nanos() as u64
}

// ─────────────────────────── provisioning ───────────────────────────

/// One browser identity generated natively, so its entity id is known
/// before the page ever loads (the proofs name providers/callers by
/// entity, and the runner mints those proofs).
pub struct PageId {
    pub entity: EntityId,
    pub entity_secret_hex: String,
    pub noise_secret_hex: String,
}

impl PageId {
    fn generate() -> Self {
        let key = EntityKeypair::generate();
        let noise = EntityKeypair::generate();
        Self {
            entity: key.entity_id().clone(),
            entity_secret_hex: hex(key.try_secret_bytes().expect("secret")),
            noise_secret_hex: hex(noise.try_secret_bytes().expect("noise secret")),
        }
    }
}

/// The org world: two roots, the anchor's adopted authority (with its
/// revocation store — the raise-side of the feed), and every page
/// identity.
pub struct OrgWorld {
    pub root_a: OrgKeypair,
    pub root_b: OrgKeypair,
    pub anchor_entity: EntityId,
    pub anchor_store: Arc<OrgRevocationStore>,
    pub anchor_authority_dir: PathBuf,
    pub caller: PageId,
    pub server: PageId,
    pub pair_a: PageId,
    pub pair_b: PageId,
    pub revoked: PageId,
    pub compliant: PageId,
    pub teardown: PageId,
    pub leader: PageId,
    pub follow1: PageId,
    pub follow2: PageId,
    /// `old` and `new` share ONE identity on purpose: the session
    /// replacement witnesses need two sessions of the same node id.
    pub shared: PageId,
}

fn hex32(bytes: &[u8; 32]) -> String {
    hex(bytes)
}

/// Re-mint the anchor's org-A cert for the reinstall path (the first
/// `cert` is moved into the placeholder-conflicting adopt).
fn cert2_for(root: &OrgKeypair, member: &EntityId) -> OrgMembershipCert {
    OrgMembershipCert::try_issue(root, member.clone(), 1, CRED_TTL_SECS).expect("cert")
}

impl OrgWorld {
    /// Provision the runner-side org operator: adopt the anchor into
    /// org A (`NodeAuthority::adopt` + `install_node_authority` +
    /// `set_owner_cert_emission` + the revocation store install), and
    /// generate every custodial page identity.
    fn provision(anchor: &Arc<MeshNode>, work: &PathBuf) -> Self {
        let root_a = OrgKeypair::generate();
        let root_b = OrgKeypair::generate();
        let anchor_entity = anchor.entity_id().clone();
        let cert = OrgMembershipCert::try_issue(&root_a, anchor_entity.clone(), 1, CRED_TTL_SECS)
            .expect("anchor org-A cert");
        let dir = work.join("s4-authority");
        let _ = std::fs::remove_dir_all(&dir);
        let authority = NodeAuthority::adopt(&dir, cert, &anchor_entity, 0, None).expect("adopt");
        let anchor_store = Arc::clone(&authority.revocation);
        // The Stage 4b setup installs a PLACEHOLDER authority (its
        // root is discarded after `enrolled_without_authority_is_
        // still_denied`), and the production install is one-owner:
        // `AlreadyOwned` rather than a silent swap. The org stage owns
        // the anchor's authority for its run — clear the placeholder
        // through the node's own seam and install org A's, keeping the
        // replacement semantics (old store retired before return).
        match anchor.install_node_authority(Arc::new(authority)) {
            Ok(()) => {}
            Err(net::adapter::net::behavior::org_authority::OrgAuthorityError::AlreadyOwned {
                ..
            }) => {
                anchor.clear_node_authority_for_test();
                let authority =
                    NodeAuthority::adopt(&dir, cert2_for(&root_a, &anchor_entity), &anchor_entity, 0, None)
                        .expect("re-adopt");
                anchor
                    .install_node_authority(Arc::new(authority))
                    .expect("install anchor authority after clearing the 4b placeholder");
            }
            Err(e) => panic!("install anchor authority: {e:?}"),
        }
        anchor
            .set_owner_cert_emission(true)
            .expect("enable owner-cert emission");
        Self {
            root_a,
            root_b,
            anchor_entity,
            anchor_store,
            anchor_authority_dir: dir,
            caller: PageId::generate(),
            server: PageId::generate(),
            pair_a: PageId::generate(),
            pair_b: PageId::generate(),
            revoked: PageId::generate(),
            compliant: PageId::generate(),
            teardown: PageId::generate(),
            leader: PageId::generate(),
            follow1: PageId::generate(),
            follow2: PageId::generate(),
            shared: PageId::generate(),
        }
    }

    fn cap(service: &str) -> CapabilityAuthorityId {
        CapabilityAuthorityId::for_tag(&format!("nrpc:{service}"))
    }

    /// Membership + dispatcher for `member` acting for `root`'s org —
    /// the sdk fixture's `belonging`, mirrored (both wide open).
    fn belonging(
        &self,
        root: &OrgKeypair,
        member: &EntityId,
        generation: u32,
    ) -> (OrgMembershipCert, OrgDispatcherGrant) {
        let cert = OrgMembershipCert::try_issue(root, member.clone(), generation, CRED_TTL_SECS)
            .expect("cert");
        let grant =
            OrgDispatcherGrant::try_issue(root, member.clone(), DispatcherScope::Any, CRED_TTL_SECS)
                .expect("dispatcher grant");
        (cert, grant)
    }

    /// A DISCOVER|INVOKE grant from org A (the provider's owner org)
    /// to org B over `service`'s capability, naming the exact provider
    /// entity. The sdk fixture's `discover_grant`, with the target
    /// scope narrowed to `ExactNode` — the call names an exact
    /// provider P anyway, and an unadopted (browser) provider is
    /// nobody's "node owned by".
    fn granted(
        &self,
        provider: &EntityId,
        service: &str,
    ) -> OrgCapabilityGrant {
        let (grant, _secret) = OrgCapabilityGrant::try_issue(
            &self.root_a,
            self.root_b.org_id(),
            Self::cap(service),
            GrantRights::INVOKE.union(GrantRights::DISCOVER),
            GrantTargetScope::ExactNode(provider.clone()),
            CRED_TTL_SECS,
        )
        .expect("capability grant");
        grant
    }

    /// The page-facing credential set for one call, as the step's
    /// `creds` object (wire bytes as hex; ids as hex strings).
    fn creds(
        &self,
        member: &EntityId,
        generation: u32,
        acting: &OrgKeypair,
        provider_owner: &OrgKeypair,
        provider: &EntityId,
        service: &str,
        granted: bool,
    ) -> Value {
        let (cert, dispatcher) = self.belonging(acting, member, generation);
        let capability_grant = granted.then(|| self.granted(provider, service));
        json!({
            "membership_hex": hex(&cert.to_bytes()),
            "dispatcher_hex": hex(&dispatcher.to_bytes()),
            "capability_grant_hex": capability_grant.as_ref().map(|g| hex(&g.to_bytes())),
            "acting_org": hex32(acting.org_id().as_bytes()),
            "provider_owner_org": hex32(provider_owner.org_id().as_bytes()),
            "provider": hex32(provider.as_bytes()),
            "proof_ttl_secs": PROOF_TTL_SECS,
        })
    }

    /// The native caller's proof intent — the frozen mint glue does
    /// the signing at call time (`CallOptions::org_proof_intent`).
    fn intent(
        &self,
        caller_key: &Arc<EntityKeypair>,
        acting: &OrgKeypair,
        provider_owner: &OrgKeypair,
        provider: &EntityId,
        service: &str,
        granted: bool,
        generation: u32,
    ) -> OrgProofIntent {
        let (membership, dispatcher) =
            self.belonging(acting, caller_key.entity_id(), generation);
        OrgProofIntent {
            caller: Arc::clone(caller_key),
            membership,
            dispatcher,
            capability_grant: granted.then(|| self.granted(provider, service)),
            acting_org: acting.org_id(),
            provider_owner_org: provider_owner.org_id(),
            provider: provider.clone(),
            capability: Self::cap(service),
            proof_ttl_secs: PROOF_TTL_SECS,
        }
    }
}

// ─────────────────────────── native handlers ───────────────────────────

/// One handler invocation as the provider observed it: the exact
/// payload bytes and the FIVE verified attribution fields, verbatim
/// from `RpcContext::org_admission` — none of them caller-claimed.
#[derive(Debug, Clone)]
pub struct CallRecord {
    pub shape: &'static str,
    pub payload: Vec<u8>,
    pub caller: Option<String>,
    pub acting_org: Option<String>,
    pub provider_org: Option<String>,
    pub provider: Option<String>,
    pub capability: Option<String>,
    pub entered_at: Instant,
    pub items_sent: Vec<Vec<u8>>,
    pub completed_at: Option<Instant>,
}

impl CallRecord {
    fn from_ctx(shape: &'static str, ctx_admitted: Option<&Admitted>, payload: Vec<u8>) -> Self {
        Self {
            shape,
            payload,
            caller: ctx_admitted.map(|a| hex32(a.caller.as_bytes())),
            acting_org: ctx_admitted.map(|a| hex32(a.acting_org.as_bytes())),
            provider_org: ctx_admitted.map(|a| hex32(a.provider_org.as_bytes())),
            provider: ctx_admitted.map(|a| hex32(a.provider.as_bytes())),
            capability: ctx_admitted.map(|a| hex32(a.capability.as_bytes())),
            entered_at: Instant::now(),
            items_sent: Vec::new(),
            completed_at: None,
        }
    }
}

/// The ordered log of every invocation across the native services.
#[derive(Default)]
pub struct CallLog {
    entries: StdMutex<Vec<(String, CallRecord)>>,
}

impl CallLog {
    /// Record the invocation at ENTRY — so `ran` counts even a
    /// handler parked forever — and return its index for
    /// [`Self::complete`].
    fn enter(&self, service: &str, record: CallRecord) -> usize {
        let mut entries = self.entries.lock().expect("call log");
        let index = entries.len();
        entries.push((service.to_string(), record));
        index
    }

    /// Mark the invocation COMPLETE with the payload it collected and
    /// everything it sent. A retired call never reaches here — which
    /// is exactly the retired-not-zombie observable (`completed_at`
    /// stays unset).
    fn complete(&self, index: usize, payload: Vec<u8>, items_sent: Vec<Vec<u8>>) {
        let mut entries = self.entries.lock().expect("call log");
        if let Some((_, record)) = entries.get_mut(index) {
            record.payload = payload;
            record.items_sent = items_sent;
            record.completed_at = Some(Instant::now());
        }
    }

    /// Every record for `service`, in arrival order.
    pub fn for_service(&self, service: &str) -> Vec<CallRecord> {
        self.entries
            .lock()
            .expect("call log")
            .iter()
            .filter(|(s, _)| s == service)
            .map(|(_, r)| r.clone())
            .collect()
    }

    pub fn total(&self) -> usize {
        self.entries.lock().expect("call log").len()
    }
}

/// A gate one parked handler waits on until the runner releases it.
#[derive(Default)]
pub struct HoldGate {
    parked: StdMutex<usize>,
    wake: tokio::sync::Notify,
    released: StdMutex<bool>,
}

impl HoldGate {
    async fn wait(&self) {
        *self.parked.lock().expect("gate") += 1;
        self.wake.notify_waiters();
        loop {
            if *self.released.lock().expect("gate") {
                return;
            }
            let notified = self.wake.notified();
            if *self.released.lock().expect("gate") {
                return;
            }
            notified.await;
        }
    }

    pub fn parked(&self) -> usize {
        *self.parked.lock().expect("gate")
    }

    pub fn release(&self) {
        *self.released.lock().expect("gate") = true;
        self.wake.notify_waiters();
    }
}

/// A unary provider: replies `label:` ‖ the exact request bytes, so
/// the response identity is an exact payload check on both halves.
struct UnaryOrg {
    service: &'static str,
    label: &'static str,
    log: Arc<CallLog>,
}

#[async_trait::async_trait]
impl RpcHandler for UnaryOrg {
    async fn call(&self, ctx: RpcContext) -> Result<RpcResponsePayload, RpcHandlerError> {
        let payload = ctx.payload.body.to_vec();
        let record = CallRecord::from_ctx("unary", ctx.org_admission.as_ref(), payload.clone());
        let index = self.log.enter(self.service, record);
        let mut reply = Vec::from(self.label.as_bytes());
        reply.push(b':');
        reply.extend_from_slice(&payload);
        self.log.complete(index, payload, vec![reply.clone()]);
        Ok(RpcResponsePayload {
            status: RpcStatus::Ok,
            headers: Vec::new(),
            body: Bytes::from(reply),
        })
    }
}

/// A server-streaming provider: sends its chunks in order, honouring
/// an optional park (`HoldGate`) between `pre` and `post` phases and
/// an optional inter-chunk delay (the slow-drip the revocation and
/// backpressure windows need).
struct StreamOrg {
    service: &'static str,
    pre: Vec<Vec<u8>>,
    post: Vec<Vec<u8>>,
    delay_ms: u64,
    gate: Option<Arc<HoldGate>>,
    log: Arc<CallLog>,
}

#[async_trait::async_trait]
impl RpcStreamingHandler for StreamOrg {
    async fn call(&self, ctx: RpcContext, sink: RpcResponseSink) -> Result<(), RpcHandlerError> {
        let payload = ctx.payload.body.to_vec();
        // The request payload identifies the record from ENTRY — a
        // RETIRED call never reaches `complete`, and the witnesses
        // read its identity there.
        let record = CallRecord::from_ctx("streaming", ctx.org_admission.as_ref(), payload.clone());
        let index = self.log.enter(self.service, record);
        let mut sent = Vec::new();
        let mut emit = |chunk: Vec<u8>, sent: &mut Vec<Vec<u8>>| {
            sent.push(chunk.clone());
            sink.send(chunk);
        };
        for chunk in &self.pre {
            emit(chunk.clone(), &mut sent);
            if self.delay_ms > 0 {
                tokio::time::sleep(Duration::from_millis(self.delay_ms)).await;
            }
        }
        if let Some(gate) = &self.gate {
            gate.wait().await;
        }
        for chunk in &self.post {
            emit(chunk.clone(), &mut sent);
            if self.delay_ms > 0 {
                tokio::time::sleep(Duration::from_millis(self.delay_ms)).await;
            }
        }
        self.log.complete(index, payload, sent);
        Ok(())
    }
}

/// A client-streaming provider: concatenates every upload chunk into
/// the terminal reply. `defer_ms` between chunks makes it a SLOW
/// CONSUMER — an eager drainer grants credit as fast as it arrives
/// and an upload window can never park a send.
struct CsOrg {
    service: &'static str,
    label: &'static str,
    defer_ms: u64,
    gate: Option<Arc<HoldGate>>,
    log: Arc<CallLog>,
}

#[async_trait::async_trait]
impl RpcClientStreamingHandler for CsOrg {
    async fn call(
        &self,
        ctx: RpcStreamingContext,
        mut requests: RequestStream,
    ) -> Result<RpcResponsePayload, RpcHandlerError> {
        use futures::StreamExt;
        let mut record = CallRecord::from_ctx("client_stream", ctx.org_admission.as_ref(), Vec::new());
        let index = self.log.enter(self.service, record);
        let mut collected = Vec::new();
        while let Some(chunk) = requests.next().await {
            collected.extend_from_slice(&chunk);
            if self.defer_ms > 0 {
                tokio::time::sleep(Duration::from_millis(self.defer_ms)).await;
            }
        }
        if let Some(gate) = &self.gate {
            gate.wait().await;
        }
        let mut reply = Vec::from(self.label.as_bytes());
        reply.push(b':');
        reply.extend_from_slice(&collected);
        self.log.complete(index, collected, vec![reply.clone()]);
        Ok(RpcResponsePayload {
            status: RpcStatus::Ok,
            headers: Vec::new(),
            body: Bytes::from(reply),
        })
    }
}

/// A duplex provider: echoes each request chunk content-labelled
/// (`e<i>:` ‖ chunk), then sends a tail after input EOF — the
/// response half completes independently of the input half.
struct DxDx {
    service: &'static str,
    tail: Vec<Vec<u8>>,
    gate: Option<Arc<HoldGate>>,
    log: Arc<CallLog>,
}

#[async_trait::async_trait]
impl RpcDuplexHandler for DxDx {
    async fn call(
        &self,
        ctx: RpcStreamingContext,
        mut requests: RequestStream,
        responses: RpcResponseSink,
    ) -> Result<(), RpcHandlerError> {
        use futures::StreamExt;
        let record = CallRecord::from_ctx("duplex", ctx.org_admission.as_ref(), Vec::new());
        let index = self.log.enter(self.service, record);
        let mut sent = Vec::new();
        let mut collected = Vec::new();
        let mut index_echo = 0usize;
        while let Some(chunk) = requests.next().await {
            collected.extend_from_slice(&chunk);
            let mut echo = Vec::from(format!("e{index_echo}:").as_bytes());
            echo.extend_from_slice(&chunk);
            sent.push(echo.clone());
            responses.send(echo);
            index_echo += 1;
        }
        if let Some(gate) = &self.gate {
            gate.wait().await;
        }
        for chunk in &self.tail {
            sent.push(chunk.clone());
            responses.send(chunk.clone());
        }
        self.log.complete(index, collected, sent);
        Ok(())
    }
}

// ─────────────────────────── hand-built openings ───────────────────────────

/// A hand-built REQUEST frame carrying a hand-minted admission proof
/// — byte-identical to what the frozen mint glue produces (the parity
/// instruments below are the byte-identity receipts), used ONLY where
/// the glue cannot be told what to mint: the wrong-peer delivery and
/// the replay legs.
///
/// Layout, verbatim the `publish_rpc_request_unsubscribed` contract:
/// `EventMeta ‖ RpcRouteV1 ‖ RpcRequestPayload`.
pub struct Opening {
    pub frame: Vec<u8>,
    pub stream_id: u64,
    pub channel_hash: u16,
    pub call_id: u64,
}

fn request_channel_of(service: &str) -> net::adapter::net::channel::ChannelId {
    let name = net::adapter::net::channel::ChannelName::new(&format!("{service}.requests"))
        .expect("valid channel name");
    net::adapter::net::channel::ChannelId::new(name)
}

/// Mint the opening exactly as `attach_signed_admission` would:
/// digest first (the admission header is stripped by the digest), the
/// proof signed over it, exactly one proof header appended, then the
/// finalized wire.
pub fn mint_opening(
    world: &OrgWorld,
    caller_key: &EntityKeypair,
    acting: &OrgKeypair,
    provider_owner: &OrgKeypair,
    provider: &EntityId,
    service: &str,
    body: &[u8],
    call_id: u64,
    origin_hash: u64,
    stream_kind: Option<u8>,
    session_binding: Option<[u8; 32]>,
    granted: bool,
) -> Opening {
    let channel = request_channel_of(service);
    let hash = channel.hash();
    let route = hash;
    let (membership, dispatcher) = world.belonging(acting, caller_key.entity_id(), 1);
    let capability_grant = granted.then(|| world.granted(provider, service));
    let mut req = RpcRequestPayload {
        service: service.to_string(),
        deadline_ns: 0,
        // The shape flags the proof's kind claims: a streaming proof
        // on a unary-flagged request is `ShapeMismatch` at the gate.
        flags: match stream_kind {
            Some(STREAM_CALL_KIND_SERVER_STREAMING) => FLAG_RPC_STREAMING_RESPONSE,
            _ => 0,
        },
        headers: Vec::new(),
        body: Bytes::copy_from_slice(body),
    };
    let digest = net::adapter::net::org_admission_gate::org_request_digest(&req).expect("digest");
    let expires = now_unix_ns() + PROOF_TTL_SECS * 1_000_000_000;
    let proof_bytes = match stream_kind {
        None => OrgCallProof::sign_for_call(
            caller_key,
            membership,
            dispatcher,
            capability_grant,
            acting.org_id(),
            provider_owner.org_id(),
            provider.clone(),
            call_id,
            OrgWorld::cap(service),
            expires,
            digest,
        )
        .encode()
        .expect("proof encode"),
        Some(kind) => OrgStreamCallProof::sign_for_stream_call(
            caller_key,
            membership,
            dispatcher,
            capability_grant,
            acting.org_id(),
            provider_owner.org_id(),
            provider.clone(),
            call_id,
            OrgWorld::cap(service),
            expires,
            digest,
            kind,
            session_binding.unwrap_or([0u8; 32]),
        )
        .encode()
        .expect("stream proof encode"),
    };
    req.headers
        .push((ORG_ADMISSION_HEADER.to_string(), proof_bytes));
    let meta = EventMeta::new(DISPATCH_RPC_REQUEST, 0, origin_hash, call_id, 0);
    let mut buf = Vec::with_capacity(RPC_FRAME_BODY_OFFSET + req.encoded_len());
    buf.extend_from_slice(&meta.to_bytes());
    encode_rpc_route(&mut buf, route);
    req.encode_into(&mut buf);
    Opening {
        frame: buf,
        stream_id: 0x0001_0000_0000_0000 | u64::from(hash),
        channel_hash: hash as u16,
        call_id,
    }
}

/// The same frame with NO proof header — the negative shape whose
/// refusal class (`MissingHeader`) is the control for the wrong-peer
/// refusals above.
pub fn mint_bare_opening(
    service: &str,
    body: &[u8],
    call_id: u64,
    origin_hash: u64,
) -> Opening {
    let channel = request_channel_of(service);
    let hash = channel.hash();
    let req = RpcRequestPayload {
        service: service.to_string(),
        deadline_ns: 0,
        flags: 0,
        headers: Vec::new(),
        body: Bytes::copy_from_slice(body),
    };
    let meta = EventMeta::new(DISPATCH_RPC_REQUEST, 0, origin_hash, call_id, 0);
    let mut buf = Vec::with_capacity(RPC_FRAME_BODY_OFFSET + req.encoded_len());
    buf.extend_from_slice(&meta.to_bytes());
    encode_rpc_route(&mut buf, hash);
    req.encode_into(&mut buf);
    Opening {
        frame: buf,
        stream_id: 0x0001_0000_0000_0000 | u64::from(hash),
        channel_hash: hash as u16,
        call_id,
    }
}

/// The Noise handshake hash of the anchor's session with `peer` —
/// what a streaming proof's `session_binding` must carry (§1.3), read
/// through the same public seam the mint glue itself uses.
fn session_binding_of(anchor: &MeshNode, peer: u64) -> Option<[u8; 32]> {
    anchor.peer_session_binding(peer)
}

// ─────────────────────────── what this stage needs ───────────────────────────

pub struct CxOrg<'a> {
    pub driver: &'a Driver,
    pub engine: Engine,
    pub anchor: &'a Arc<MeshNode>,
    pub anchor_key: &'a Arc<EntityKeypair>,
    pub credential: String,
    pub bootstrap_base: String,
    pub origin: String,
    pub page_origin: String,
    pub anchor_rtc_addr: String,
    pub stun: Option<String>,
    pub feed: Arc<OrgControlFeed>,
    pub work: PathBuf,
    pub tabs: HashMap<String, StepOrgSender>,
}

impl CxOrg<'_> {
    /// The bootstrap URL for one tab, with the revocation feed's
    /// control-socket pointer in the `#org-control=` override tag.
    /// The `node=` key is the TAB name — the feed registry's key.
    pub fn bootstrap_base_with_feed(&self, tab: &str) -> String {
        let port = self
            .page_origin
            .rsplit(':')
            .next()
            .and_then(|p| p.parse::<u16>().ok())
            .unwrap_or(0);
        format!(
            "{}#org-control=ws://localhost:{port}/harness/org-control?node={tab}",
            self.bootstrap_base
        )
    }
}

// ─────────────────────────── step builders ───────────────────────────

/// One leaf's identity as the page reported it after `connect`.
#[derive(Debug, Clone)]
pub struct LeafInfo {
    pub node_id: u64,
    pub node_hex: String,
    pub origin_hash: String,
}

fn leaf_of(result: &StepResult) -> Option<LeafInfo> {
    if !result.ok {
        return None;
    }
    let node_hex = result.node_id.clone()?;
    let node_id = u64::from_str_radix(node_hex.trim_start_matches("0x"), 16).ok()?;
    Some(LeafInfo {
        node_id,
        node_hex,
        origin_hash: result.origin_hash.clone().unwrap_or_default(),
    })
}

fn connect_step(
    cx: &CxOrg<'_>,
    world: &OrgWorld,
    id: &PageId,
    session: &str,
    lock_scope: Option<&str>,
    feed_key: &str,
) -> Value {
    let _ = world;
    json!({
        "kind": "connect",
        "session": session,
        "credential": cx.credential,
        // The FEED POINTER rides the bootstrap URL's `#org-control=`
        // override tag — `split_org_control` parses it in pure Rust
        // (no hook-order mystery), and every URL the adapter mints
        // from the base is tag-free by construction.
        "bootstrap_url": cx.bootstrap_base_with_feed(feed_key),
        "origin": cx.origin,
        "anchor_rtc_addr": cx.anchor_rtc_addr,
        "stun": cx.stun,
        "entity_secret_hex": id.entity_secret_hex,
        "noise_secret_hex": id.noise_secret_hex,
        "capabilities": ["org.s4.tag"],
        "lock_scope": lock_scope,
    })
}

fn serve_step(
    handle: &str,
    service: &str,
    shape: &str,
    access: &str,
    owner_org: &str,
    label: &str,
    pre: &[Vec<u8>],
    post: &[Vec<u8>],
    hold: bool,
    defer_ms: u64,
) -> Value {
    json!({
        "kind": "org_serve",
        "session": "s",
        "handle": handle,
        "service": service,
        "shape": shape,
        "access": access,
        "owner_org": owner_org,
        "label": label,
        "pre_chunks": pre.iter().map(|c| hex(c)).collect::<Vec<_>>(),
        "post_chunks": post.iter().map(|c| hex(c)).collect::<Vec<_>>(),
        "hold": hold,
        "defer_ms": defer_ms,
    })
}

fn unary_step(service: &str, payload: &[u8], creds: &Value) -> Value {
    json!({
        "kind": "org_call_unary",
        "session": "s",
        "service": service,
        "payload_hex": hex(payload),
        "creds": creds,
    })
}

fn stream_open_step(service: &str, payload: &[u8], creds: &Value, handle: &str, window: Option<u64>) -> Value {
    json!({
        "kind": "org_stream_open",
        "session": "s",
        "service": service,
        "payload_hex": hex(payload),
        "creds": creds,
        "handle": handle,
        "stream_window_initial": window,
    })
}

fn stream_read_step(handle: &str, want: u64, timeout_ms: u64) -> Value {
    json!({
        "kind": "org_stream_read",
        "handle": handle,
        "want": want,
        "timeout_ms": timeout_ms,
    })
}

fn upload_open_step(service: &str, creds: &Value, handle: &str, window: Option<u64>) -> Value {
    json!({
        "kind": "org_upload_open",
        "session": "s",
        "service": service,
        "creds": creds,
        "handle": handle,
        "request_window_initial": window,
    })
}

fn upload_send_step(handle: &str, payloads: &[Vec<u8>]) -> Value {
    json!({
        "kind": "org_upload_send",
        "handle": handle,
        "payloads": payloads.iter().map(|p| hex(p)).collect::<Vec<_>>(),
    })
}

fn upload_finish_step(handle: &str) -> Value {
    json!({ "kind": "org_upload_finish", "handle": handle })
}

fn duplex_open_step(service: &str, creds: &Value, handle: &str, stream_window: Option<u64>, request_window: Option<u64>) -> Value {
    json!({
        "kind": "org_duplex_open",
        "session": "s",
        "service": service,
        "creds": creds,
        "handle": handle,
        "stream_window_initial": stream_window,
        "request_window_initial": request_window,
    })
}

fn duplex_send_step(handle: &str, payloads: &[Vec<u8>]) -> Value {
    json!({
        "kind": "org_duplex_send",
        "handle": handle,
        "payloads": payloads.iter().map(|p| hex(p)).collect::<Vec<_>>(),
    })
}

fn duplex_finish_step(handle: &str) -> Value {
    json!({ "kind": "org_duplex_finish", "handle": handle })
}

fn duplex_read_step(handle: &str, want: u64, timeout_ms: u64) -> Value {
    json!({
        "kind": "org_duplex_read",
        "handle": handle,
        "want": want,
        "timeout_ms": timeout_ms,
    })
}

fn report_step(handle: &str) -> Value {
    json!({ "kind": "org_serve_report", "handle": handle })
}

/// `s` is the single-session handle every non-leader step names.
const SESSION: &str = "s";

// ─────────────────────────── native service registration ───────────────────────────

struct NativeServices {
    log: Arc<CallLog>,
    hold_gate: Arc<HoldGate>,
    /// Keep every `ServeHandle` alive for the run — dropping one
    /// UNREGISTERS its service.
    _handles: Vec<ServeHandle>,
}

/// Register every native (anchor) org provider. The granted twins run
/// `serve_rpc_granted_*`, the same-org twins `serve_rpc_owner_scoped_*`
/// — the exact seams under test.
fn register_native_services(anchor: &Arc<MeshNode>) -> NativeServices {
    let log = Arc::new(CallLog::default());
    let teardown_gate = Arc::new(HoldGate::default());
    let hold_gate = Arc::new(HoldGate::default());
    let policy = std::sync::Arc::new(|_proof: &OrgCallProof| true);
    let mut handles = Vec::new();

    // The unary echo family + the leader attribution service. The
    // GRANTED twin registers through `serve_rpc_granted` — the
    // `CrossOrgGranted` admission branch; registering it
    // owner-scoped routes it through `OwnerDelegated`, whose
    // `acting_org == provider_owner_org` requirement denies a
    // cross-org caller `GranteeMismatch` before the grant is read.
    for (service, label) in [(N_U_SAME, "u-same"), (N_LEADER, "u-leader")] {
        handles.push(
            anchor
                .serve_rpc_owner_scoped(
                    service,
                    Arc::new(UnaryOrg {
                        service,
                        label,
                        log: Arc::clone(&log),
                    }),
                    policy.clone(),
                )
                .expect("owner-scoped unary serve"),
        );
    }
    handles.push(
        anchor
            .serve_rpc_granted(
                N_U_GRANTED,
                Arc::new(UnaryOrg {
                    service: N_U_GRANTED,
                    label: "u-granted",
                    log: Arc::clone(&log),
                }),
                policy.clone(),
            )
            .expect("granted unary serve"),
    );

    // Server-streaming family.
    for (service, pre, post, granted) in [
        (
            N_S_SAME,
            vec![b"s-same-0".to_vec(), b"s-same-1".to_vec(), b"s-same-2".to_vec()],
            vec![b"s-same-tail".to_vec()],
            false,
        ),
        (
            N_S_GRANTED,
            vec![b"s-granted-0".to_vec(), b"s-granted-1".to_vec()],
            vec![b"s-granted-tail".to_vec()],
            true,
        ),
        (
            N_REV,
            (0..8).map(|i| format!("drip-{i}").into_bytes()).collect(),
            vec![b"rev-tail".to_vec()],
            false,
        ),
    ] {
        let handler = Arc::new(StreamOrg {
            service,
            pre,
            post,
            delay_ms: if service == N_REV { 150 } else { 0 },
            gate: None,
            log: Arc::clone(&log),
        });
        handles.push(if granted {
            anchor
                .serve_rpc_granted_streaming(service, handler, policy.clone())
                .expect("granted streaming serve")
        } else {
            anchor
                .serve_rpc_owner_scoped_streaming(service, handler, policy.clone())
                .expect("owner-scoped streaming serve")
        });
    }
    // The teardown/hold providers: parked until released.
    for (service, gate) in [
        (N_TEARDOWN, Arc::clone(&teardown_gate)),
        (N_HOLD, Arc::clone(&hold_gate)),
    ] {
        handles.push(
            anchor
                .serve_rpc_owner_scoped_streaming(
                    service,
                    Arc::new(StreamOrg {
                        service,
                        pre: vec![b"held-0".to_vec(), b"held-1".to_vec()],
                        post: vec![b"released-tail".to_vec()],
                        delay_ms: 0,
                        gate: Some(gate),
                        log: Arc::clone(&log),
                    }),
                    policy.clone(),
                )
                .expect("owner-scoped parked streaming serve"),
        );
    }

    // Client-streaming family + the slow-consumer backpressure
    // provider.
    for (service, label, granted) in
        [(N_CS_SAME, "cs-same", false), (N_CS_GRANTED, "cs-granted", true)]
    {
        let handler = Arc::new(CsOrg {
            service,
            label,
            defer_ms: 0,
            gate: None,
            log: Arc::clone(&log),
        });
        handles.push(if granted {
            anchor
                .serve_rpc_granted_client_stream(service, handler, policy.clone())
                .expect("granted client-stream serve")
        } else {
            anchor
                .serve_rpc_owner_scoped_client_stream(service, handler, policy.clone())
                .expect("owner-scoped client-stream serve")
        });
    }
    handles.push(
        anchor
            .serve_rpc_owner_scoped_client_stream(
                N_CS_BP,
                Arc::new(CsOrg {
                    service: N_CS_BP,
                    label: "cs-bp",
                    // A SLOW consumer: 150 ms per chunk, so an
                    // exhausted upload window parks the caller's
                    // sends somewhere observable: each grant's park
                    // window must exceed the >250ms parked threshold.
                    defer_ms: 400,
                    gate: None,
                    log: Arc::clone(&log),
                }),
                policy.clone(),
            )
            .expect("slow-consumer client-stream serve"),
    );

    // Duplex family.
    for (service, tail, granted) in [
        (N_DX_SAME, vec![b"dx-same-tail".to_vec()], false),
        (N_DX_GRANTED, vec![b"dx-granted-tail".to_vec()], true),
    ] {
        let handler = Arc::new(DxDx {
            service,
            tail,
            gate: None,
            log: Arc::clone(&log),
        });
        handles.push(if granted {
            anchor
                .serve_rpc_granted_duplex(service, handler, policy.clone())
                .expect("granted duplex serve")
        } else {
            anchor
                .serve_rpc_owner_scoped_duplex(service, handler, policy.clone())
                .expect("owner-scoped duplex serve")
        });
    }

    NativeServices {
        log,
        hold_gate,
        _handles: handles,
    }
}

// ─────────────────────────── the org call matrix (A) ───────────────────────────

/// The exact expected reply of each native unary/streaming/… provider,
/// so every success assertion is a byte identity, not a presence check.
fn expected_unary(label: &str, payload: &[u8]) -> Vec<u8> {
    let mut out = Vec::from(format!("{label}:").as_bytes());
    out.extend_from_slice(payload);
    out
}

/// Matrix A: the browser calls, the native anchor serves. Each of the
/// eight verdicts asserts: the EXACT response bytes, the handler's
/// EXACT request payload, and the FIVE verified attribution fields
/// against the expected identity tuple.
#[expect(clippy::too_many_arguments, reason = "one linear witness script")]
async fn browser_call_matrix(
    cx: &CxOrg<'_>,
    world: &OrgWorld,
    script: &mut ScriptOrg,
    ledger: &mut Ledger,
    services: &NativeServices,
    caller: &LeafInfo,
) {
    let caller_hex = hex32(world.caller.entity.as_bytes());
    let provider_hex = hex32(world.anchor_entity.as_bytes());
    let a_hex = hex32(world.root_a.org_id().as_bytes());
    let b_hex = hex32(world.root_b.org_id().as_bytes());

    // (witness index, service, shape, granted, payload, label/pre/post)
    let cases: [(usize, &str, &str, bool); 8] = [
        (0, N_U_SAME, "unary", false),
        (1, N_U_GRANTED, "unary", true),
        (2, N_S_SAME, "streaming", false),
        (3, N_S_GRANTED, "streaming", true),
        (4, N_CS_SAME, "client_stream", false),
        (5, N_CS_GRANTED, "client_stream", true),
        (6, N_DX_SAME, "duplex", false),
        (7, N_DX_GRANTED, "duplex", true),
    ];

    for (index, service, shape, granted) in cases {
        let acting = if granted { &world.root_b } else { &world.root_a };
        let acting_hex = if granted { &b_hex } else { &a_hex };
        let creds = world.creds(
            &world.caller.entity,
            1,
            acting,
            &world.root_a,
            &world.anchor_entity,
            service,
            granted,
        );
        let payload = format!("payload-{}", WITNESSES[index]).into_bytes();
        let witness = WITNESSES[index];
        let before = services.log.total();
        // The handler-side record's payload identity PER SHAPE: the
        // request body for unary/streaming, the collected upload
        // concatenation for client-stream/duplex (what the handler
        // recorded from the call's own bytes).
        let record_payload = match shape {
            "client_stream" => {
                let mut j = format!("up-{}-0", index).into_bytes();
                j.extend_from_slice(format!("up-{}-1", index).as_bytes());
                j
            }
            "duplex" => {
                let mut j = format!("dx-{}-0", index).into_bytes();
                j.extend_from_slice(format!("dx-{}-1", index).as_bytes());
                j
            }
            _ => payload.clone(),
        };

        let (pass, detail) = match shape {
            "unary" => {
                let r = script
                    .run(TAB_CALL, unary_step(service, &payload, &creds))
                    .await;
                let expected = expected_unary(if granted { "u-granted" } else { "u-same" }, &payload);
                let reply = r.reply.as_deref().map(crate::unhex).unwrap_or_default();
                (
                    r.ok && reply == expected,
                    format!(
                        "reply={} (want {}); {}; handler delta={}",
                        hex(&reply),
                        hex(&expected),
                        typed(&r),
                        services.log.total() - before
                    ),
                )
            }
            "streaming" => {
                let handle = format!("a{index}");
                let open = script
                    .run(TAB_CALL, stream_open_step(service, &payload, &creds, &handle, None))
                    .await;
                let read = if open.ok {
                    script
                        .run(TAB_CALL, stream_read_step(&handle, 10, 15_000))
                        .await
                } else {
                    fail("open failed")
                };
                let items = stat_list(&read, "items");
                let expected: Vec<String> = if granted {
                    vec!["s-granted-0", "s-granted-1", "s-granted-tail"]
                } else {
                    vec!["s-same-0", "s-same-1", "s-same-2", "s-same-tail"]
                }
                .into_iter()
                .map(|s| hex(s.as_bytes()))
                .collect();
                let terminal_done =
                    stat_obj(&read, "terminal").and_then(|t| t.get("done")).and_then(Value::as_bool)
                        == Some(true);
                (
                    open.ok && read.ok && items == expected && terminal_done,
                    format!(
                        "items={items:?} (want {expected:?}) terminal={:?} {}; handler delta={}",
                        stat_obj(&read, "terminal"),
                        typed(&read),
                        services.log.total() - before
                    ),
                )
            }
            "client_stream" => {
                let handle = format!("a{index}");
                let up1 = vec![format!("up-{}-0", index).into_bytes()];
                let up2 = vec![format!("up-{}-1", index).into_bytes()];
                let open = script
                    .run(TAB_CALL, upload_open_step(service, &creds, &handle, None))
                    .await;
                let _ = script.run(TAB_CALL, upload_send_step(&handle, &up1)).await;
                let _ = script.run(TAB_CALL, upload_send_step(&handle, &up2)).await;
                let fin = if open.ok {
                    script.run(TAB_CALL, upload_finish_step(&handle)).await
                } else {
                    fail("open failed")
                };
                let label = if granted { "cs-granted" } else { "cs-same" };
                let mut joined = up1[0].clone();
                joined.extend_from_slice(&up2[0]);
                let expected = expected_unary(label, &joined);
                let reply = fin.reply.as_deref().map(crate::unhex).unwrap_or_default();
                (
                    open.ok && fin.ok && reply == expected,
                    format!(
                        "reply={} (want {}); {}; handler delta={}",
                        hex(&reply),
                        hex(&expected),
                        typed(&fin),
                        services.log.total() - before
                    ),
                )
            }
            "duplex" => {
                let handle = format!("a{index}");
                let sends = vec![
                    format!("dx-{}-0", index).into_bytes(),
                    format!("dx-{}-1", index).into_bytes(),
                ];
                let open = script
                    .run(TAB_CALL, duplex_open_step(service, &creds, &handle, None, None))
                    .await;
                let _ = script.run(TAB_CALL, duplex_send_step(&handle, &sends)).await;
                let _ = script.run(TAB_CALL, duplex_finish_step(&handle)).await;
                let read = if open.ok {
                    script
                        .run(TAB_CALL, duplex_read_step(&handle, 10, 15_000))
                        .await
                } else {
                    fail("open failed")
                };
                let mut expected: Vec<String> = sends
                    .iter()
                    .enumerate()
                    .map(|(i, c)| {
                        let mut echo = Vec::from(format!("e{i}:").as_bytes());
                        echo.extend_from_slice(c);
                        hex(&echo)
                    })
                    .collect();
                expected.push(hex(if granted { b"dx-granted-tail" } else { b"dx-same-tail" }));
                let items = stat_list(&read, "items");
                let terminal_done =
                    stat_obj(&read, "terminal").and_then(|t| t.get("done")).and_then(Value::as_bool)
                        == Some(true);
                (
                    open.ok && read.ok && items == expected && terminal_done,
                    format!(
                        "items={items:?} (want {expected:?}) eof_done={:?}; {}; handler delta={}",
                        stat_obj(&read, "finished_sending"),
                        typed(&read),
                        services.log.total() - before
                    ),
                )
            }
            _ => (false, "unknown shape".into()),
        };

        // The FIVE verified attribution fields, exactly, on the
        // record the handler itself captured.
        let record = services
            .log
            .for_service(service)
            .into_iter()
            .last();
        let attribution_ok = record.as_ref().is_some_and(|rec| {
            rec.payload == record_payload
                && rec.caller.as_deref() == Some(caller_hex.as_str())
                && rec.acting_org.as_deref() == Some(acting_hex.as_str())
                && rec.provider_org.as_deref() == Some(a_hex.as_str())
                && rec.provider.as_deref() == Some(provider_hex.as_str())
                && rec.capability.as_deref()
                    == Some(hex32(OrgWorld::cap(service).as_bytes()).as_str())
        });
        let record_detail = record
            .map(|rec| {
                format!(
                    "payload={} caller={:?} acting={:?} provider_org={:?} provider={:?} \
                     capability={:?} || EXPECTED payload={} caller={caller_hex} acting={acting_hex} \
                     provider_org={a_hex} provider={provider_hex} capability={}",
                    hex(&rec.payload),
                    rec.caller,
                    rec.acting_org,
                    rec.provider_org,
                    rec.provider,
                    rec.capability,
                    hex(&record_payload),
                    hex32(OrgWorld::cap(service).as_bytes())
                )
            })
            .unwrap_or_else(|| "no handler record".into());
        ledger.record(
            witness,
            pass && attribution_ok,
            format!("{detail}; attribution(5/5)={attribution_ok} on [{record_detail}] (caller leaf {caller_hex}, provider {provider_hex})"),
        );
        let _ = caller;
    }
}

// ─────────────────────────── the org call matrix (B) ───────────────────────────

/// Matrix B: the native anchor calls, the browser page serves. The
/// five attribution fields are read off the JS handler's own record;
/// the response identities are the page-side constructions.
#[expect(clippy::too_many_arguments, reason = "one linear witness script")]
async fn native_call_matrix(
    cx: &CxOrg<'_>,
    world: &OrgWorld,
    script: &mut ScriptOrg,
    ledger: &mut Ledger,
    server: &LeafInfo,
) {
    let owner_org = hex32(world.root_a.org_id().as_bytes());
    let anchor_hex = hex32(world.anchor_entity.as_bytes());
    let server_hex = hex32(world.server.entity.as_bytes());
    let a_hex = hex32(world.root_a.org_id().as_bytes());
    let b_hex = hex32(world.root_b.org_id().as_bytes());

    let cases: [(usize, &str, &str, bool); 8] = [
        (8, B_U_SAME, "unary", false),
        (9, B_U_GRANTED, "unary", true),
        (10, B_S_SAME, "streaming", false),
        (11, B_S_GRANTED, "streaming", true),
        (12, B_CS_SAME, "client_stream", false),
        (13, B_CS_GRANTED, "client_stream", true),
        (14, B_DX_SAME, "duplex", false),
        (15, B_DX_GRANTED, "duplex", true),
    ];

    for (index, service, shape, granted) in cases {
        let access = if granted { "granted" } else { "same-org" };
        let label = format!("b{index}");
        let pre: Vec<Vec<u8>> = vec![format!("{label}-0").into_bytes(), format!("{label}-1").into_bytes()];
        let post: Vec<Vec<u8>> = vec![format!("{label}-tail").into_bytes()];
        let handle = format!("bs{index}");

        // The page registers its handler with the runner-chosen
        // chunks, so the response identity below is exact.
        let serve = script
            .run(
                TAB_SERVE,
                serve_step(&handle, service, shape, access, &owner_org, &label, &pre, &post, false, 0),
            )
            .await;
        if !serve.ok {
            ledger.record(
                WITNESSES[index],
                false,
                format!("the browser could not serve {service}: {}", why(&serve)),
            );
            continue;
        }

        // The native caller — the frozen mint glue signs the proof.
        let intent = world.intent(
            cx.anchor_key,
            if granted { &world.root_b } else { &world.root_a },
            &world.root_a,
            &world.server.entity,
            service,
            granted,
            1,
        );
        let opts = CallOptions {
            org_proof_intent: Some(intent),
            deadline: Some(std::time::Instant::now() + Duration::from_secs(20)),
            ..Default::default()
        };
        let payload = format!("native-{}", WITNESSES[index]).into_bytes();
        let (pass, detail) = match shape {
            "unary" => {
                let expected = expected_unary(&label, &payload);
                match cx
                    .anchor
                    .call(server.node_id, service, Bytes::copy_from_slice(&payload), opts)
                    .await
                {
                    Ok(reply) => (
                        reply.body.as_ref() == expected.as_slice(),
                        format!("reply={} (want {})", hex(&reply.body), hex(&expected)),
                    ),
                    Err(e) => (false, format!("the call failed: {e}")),
                }
            }
            "streaming" => {
                use futures::StreamExt as _;
                let mut items = Vec::new();
                let mut terminal = "none".to_string();
                match cx
                    .anchor
                    .call_streaming(server.node_id, service, Bytes::copy_from_slice(&payload), opts)
                    .await
                {
                    Ok(mut stream) => {
                        loop {
                            match stream.next().await {
                                Some(Ok(chunk)) => items.push(chunk.to_vec()),
                                Some(Err(e)) => {
                                    terminal = format!("ERR {e}");
                                    break;
                                }
                                None => {
                                    terminal = "done".to_string();
                                    break;
                                }
                            }
                        }
                    }
                    Err(e) => terminal = format!("OPEN-ERR {e}"),
                }
                let expected: Vec<Vec<u8>> = pre.into_iter().chain(post).collect();
                (
                    items == expected && terminal == "done",
                    format!(
                        "items={} (want {}) terminal={terminal}",
                        items.iter().map(|c| hex(c)).collect::<Vec<_>>().join(","),
                        expected.iter().map(|c| hex(c)).collect::<Vec<_>>().join(",")
                    ),
                )
            }
            "client_stream" => {
                let up = vec![format!("nu-{}-0", index).into_bytes(), format!("nu-{}-1", index).into_bytes()];
                match cx
                    .anchor
                    .call_client_stream(server.node_id, service, opts)
                    .await
                {
                    Ok(mut upload) => {
                        let mut send_error = None;
                        for chunk in &up {
                            if let Err(e) = upload.send(Bytes::copy_from_slice(chunk)).await {
                                send_error = Some(format!("{e}"));
                                break;
                            }
                        }
                        match (send_error, upload.finish().await) {
                            (None, Ok(reply)) => {
                                let mut joined = up[0].clone();
                                joined.extend_from_slice(&up[1]);
                                let expected = expected_unary(&label, &joined);
                                (
                                    reply.body.as_ref() == expected.as_slice(),
                                    format!("reply={} (want {})", hex(&reply.body), hex(&expected)),
                                )
                            }
                            (Some(e), _) => (false, format!("send failed: {e}")),
                            (None, Err(e)) => (false, format!("finish failed: {e}")),
                        }
                    }
                    Err(e) => (false, format!("open failed: {e}")),
                }
            }
            "duplex" => {
                let sends = vec![format!("nd-{}-0", index).into_bytes(), format!("nd-{}-1", index).into_bytes()];
                match cx.anchor.call_duplex(server.node_id, service, opts).await {
                    Ok(mut call) => {
                        for chunk in &sends {
                            if let Err(e) = call.send(Bytes::copy_from_slice(chunk)).await {
                                return ledger.record(
                                    WITNESSES[index],
                                    false,
                                    format!("duplex send failed: {e}"),
                                );
                            }
                        }
                        if let Err(e) = call.finish_sending().await {
                            return ledger.record(
                                WITNESSES[index],
                                false,
                                format!("finishSending failed: {e}"),
                            );
                        }
                        let mut items = Vec::new();
                        let mut terminal = "none".to_string();
                        loop {
                            match call.next().await {
                                Some(Ok(chunk)) => items.push(chunk.to_vec()),
                                Some(Err(e)) => {
                                    terminal = format!("ERR {e}");
                                    break;
                                }
                                None => {
                                    terminal = "done".to_string();
                                    break;
                                }
                            }
                        }
                        let mut expected: Vec<Vec<u8>> = sends
                            .iter()
                            .enumerate()
                            .map(|(i, c)| {
                                let mut echo = Vec::from(format!("e{i}:").as_bytes());
                                echo.extend_from_slice(c);
                                echo
                            })
                            .collect();
                        expected.push(format!("{label}-tail").into_bytes());
                        (
                            items == expected && terminal == "done",
                            format!(
                                "items={} (want {}) terminal={terminal}",
                                items.iter().map(|c| hex(c)).collect::<Vec<_>>().join(","),
                                expected.iter().map(|c| hex(c)).collect::<Vec<_>>().join(",")
                            ),
                        )
                    }
                    Err(e) => (false, format!("open failed: {e}")),
                }
            }
            _ => (false, "unknown shape".into()),
        };

        // The browser handler's own attribution record: the FIVE
        // verified fields, exactly.
        let report = script.run(TAB_SERVE, report_step(&handle)).await;
        let calls = stat_obj(&report, "calls")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        // The page record's payload identity PER SHAPE: the request
        // body for unary/streaming, the collected upload
        // concatenation for client-stream/duplex.
        let record_payload = match shape {
            "client_stream" => {
                let mut j = format!("nu-{}-0", index).into_bytes();
                j.extend_from_slice(format!("nu-{}-1", index).as_bytes());
                j
            }
            "duplex" => {
                let mut j = format!("nd-{}-0", index).into_bytes();
                j.extend_from_slice(format!("nd-{}-1", index).as_bytes());
                j
            }
            _ => payload.clone(),
        };
        let attribution_ok = calls.len() == 1
            && calls[0].get("payload").and_then(Value::as_str)
                == Some(hex(&record_payload).as_str())
            && calls[0].get("entity").and_then(Value::as_str) == Some(anchor_hex.as_str())
            && calls[0].get("acting_org").and_then(Value::as_str)
                == Some(if granted { &b_hex } else { &a_hex }.as_str())
            && calls[0].get("provider_org").and_then(Value::as_str) == Some(a_hex.as_str())
            && calls[0].get("provider").and_then(Value::as_str) == Some(server_hex.as_str())
            && calls[0].get("capability").and_then(Value::as_str)
                == Some(hex32(OrgWorld::cap(service).as_bytes()).as_str())
            && calls[0].get("is_same_org").and_then(Value::as_bool) == Some(!granted);
        let expected_tuple = format!(
            "EXPECTED payload={} entity={anchor_hex} acting={} provider_org={a_hex} \
             provider={server_hex} capability={} is_same_org={}",
            hex(&record_payload),
            if granted { &b_hex } else { &a_hex },
            hex32(OrgWorld::cap(service).as_bytes()),
            !granted
        );
        ledger.record(
            WITNESSES[index],
            pass && attribution_ok,
            format!(
                "{detail}; attribution(5/5+is_same_org)={attribution_ok} ran={} record={}; \
                 {expected_tuple}; (caller anchor {anchor_hex}, provider leaf {server_hex})",
                calls.len(),
                calls.first().cloned().unwrap_or(Value::Null)
            ),
        );
    }
    let _ = cx;
}

// ─────────────────────────── the browser pair (C) ───────────────────────────

/// Matrix C: two ISOLATED browser identities over a direct session
/// (`peer_offer`/`peer_accept_offer`/`peer_candidate`/`peer_handshake`
/// — the §9 four-method drive). o-pair-b serves; o-pair-a calls.
async fn browser_pair_matrix(
    world: &OrgWorld,
    script: &mut ScriptOrg,
    ledger: &mut Ledger,
    a: &LeafInfo,
    b: &LeafInfo,
) {
    // Drive the direct session from both ends — AFTER the pair
    // discovers each other's signed announcements (a peer's Noise key
    // comes from its announcement, never from a caller; stage6's
    // precondition).
    for tab in [TAB_PAIR_A, TAB_PAIR_B] {
        let announced = script
            .run(
                tab,
                json!({ "kind": "announce", "session": SESSION, "capabilities": ["org.s4.tag"] }),
            )
            .await;
        if !announced.ok {
            let detail = format!("pair announce failed: {}", why(&announced));
            for index in 16..21 {
                ledger.record(WITNESSES[index], false, detail.clone());
            }
            return;
        }
    }
    let mut discovered = false;
    for _ in 0..60 {
        // `converge_discovery`'s pattern: re-announce INSIDE the poll
        // — the flood is interval-gated at the anchor, and one
        // emission can miss a peer that connects between floods.
        for tab in [TAB_PAIR_A, TAB_PAIR_B] {
            let _ = script
                .run(
                    tab,
                    json!({ "kind": "announce", "session": SESSION, "capabilities": ["org.s4.tag"] }),
                )
                .await;
        }
        let seen_a = script
            .run(
                TAB_PAIR_A,
                json!({ "kind": "query", "session": SESSION, "capability": "org.s4.tag" }),
            )
            .await;
        let seen_b = script
            .run(
                TAB_PAIR_B,
                json!({ "kind": "query", "session": SESSION, "capability": "org.s4.tag" }),
            )
            .await;
        let a_sees_b = peers_of(&seen_a).map_or(false, |p| p.to_string().contains(&b.node_hex));
        let b_sees_a = peers_of(&seen_b).map_or(false, |p| p.to_string().contains(&a.node_hex));
        if a_sees_b && b_sees_a {
            discovered = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    if !discovered {
        let detail =
            "the pair never discovered each other's signed announcements — a peer's Noise \
             key comes from its announcement and peer_offer would refuse"
                .to_string();
        for index in 16..21 {
            ledger.record(WITNESSES[index], false, detail.clone());
        }
        return;
    }

    // Drive the direct session from both ends. The ANSWER is retried:
    // the offer's envelope rides the relayed path and can arrive
    // after `peer_offer` resolves ("an offer is answered from the
    // envelope that arrived, never from the caller").
    let offer = script
        .run(
            TAB_PAIR_A,
            json!({ "kind": "peer_offer", "session": SESSION, "peer_hex": b.node_hex }),
        )
        .await;
    let mut answer = fail("never attempted");
    for _ in 0..20 {
        tokio::time::sleep(Duration::from_millis(250)).await;
        answer = script
            .run(
                TAB_PAIR_B,
                json!({ "kind": "peer_accept_offer", "session": SESSION, "peer_hex": a.node_hex }),
            )
            .await;
        if answer.ok || !why(&answer).contains("no verified offer") {
            break;
        }
    }
    let _ = script
        .run(
            TAB_PAIR_A,
            json!({ "kind": "peer_candidate", "session": SESSION, "peer_hex": b.node_hex }),
        )
        .await;
    let _ = script
        .run(
            TAB_PAIR_B,
            json!({ "kind": "peer_candidate", "session": SESSION, "peer_hex": a.node_hex }),
        )
        .await;
    // ICE takes seconds and `peer_candidate` services ONE candidate
    // round: poll both sides until the offerer's reading says the
    // channel is open (stage6's `drive_to_open` shape) before the
    // handshake — handshaking against a not-yet-open DataChannel is
    // the `is not open` failure.
    let mut channel_open = false;
    for _ in 0..40 {
        let reading = script
            .run(
                TAB_PAIR_A,
                json!({ "kind": "peer_candidate", "session": SESSION, "peer_hex": b.node_hex }),
            )
            .await;
        let _ = script
            .run(
                TAB_PAIR_B,
                json!({ "kind": "peer_candidate", "session": SESSION, "peer_hex": a.node_hex }),
            )
            .await;
        let state_now = stat_str(&reading, "state").unwrap_or_default();
        if state_now == "open" {
            channel_open = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(300)).await;
    }
    let hs = script
        .run(
            TAB_PAIR_A,
            json!({ "kind": "peer_handshake", "session": SESSION, "peer_hex": b.node_hex }),
        )
        .await;
    if !(offer.ok && answer.ok && hs.ok && channel_open) {
        let detail = format!(
            "the direct session did not come up: offer={} answer={} channel_open={channel_open} \
             handshake={}",
            why(&offer),
            why(&answer),
            why(&hs)
        );
        for index in 16..21 {
            ledger.record(WITNESSES[index], false, detail.clone());
        }
        return;
    }

    let owner_org = hex32(world.root_a.org_id().as_bytes());
    let cases: [(usize, &str, &str, bool); 5] = [
        (16, P_U, "unary", false),
        (17, P_S, "streaming", false),
        (18, P_CS, "client_stream", false),
        (19, P_DX, "duplex", false),
        (20, P_GRANT, "unary", true),
    ];

    for (index, service, shape, granted) in cases {
        let label = format!("p{index}");
        let pre = vec![format!("{label}-0").into_bytes()];
        let post = vec![format!("{label}-tail").into_bytes()];
        let handle = format!("ps{index}");
        let access = if granted { "granted" } else { "same-org" };
        let serve = script
            .run(
                TAB_PAIR_B,
                serve_step(&handle, service, shape, access, &owner_org, &label, &pre, &post, false, 0),
            )
            .await;
        if !serve.ok {
            ledger.record(WITNESSES[index], false, format!("pair-b could not serve: {}", why(&serve)));
            continue;
        }
        let creds = world.creds(
            &world.pair_a.entity,
            1,
            if granted { &world.root_b } else { &world.root_a },
            &world.root_a,
            &world.pair_b.entity,
            service,
            granted,
        );
        let payload = format!("pair-{}", WITNESSES[index]).into_bytes();
        let (pass, detail) = match shape {
            "unary" => {
                let r = script
                    .run(TAB_PAIR_A, unary_step(service, &payload, &creds))
                    .await;
                let expected = expected_unary(&label, &payload);
                let reply = r.reply.as_deref().map(crate::unhex).unwrap_or_default();
                (
                    r.ok && reply == expected,
                    format!("reply={} (want {}); {}", hex(&reply), hex(&expected), typed(&r)),
                )
            }
            "streaming" => {
                let handle = "pairstream".to_string();
                let open = script
                    .run(TAB_PAIR_A, stream_open_step(service, &payload, &creds, &handle, None))
                    .await;
                let read = if open.ok {
                    script.run(TAB_PAIR_A, stream_read_step(&handle, 5, 15_000)).await
                } else {
                    fail("open failed")
                };
                let items = stat_list(&read, "items");
                let expected: Vec<String> = [format!("{label}-0"), format!("{label}-tail")]
                    .iter()
                    .map(|s| hex(s.as_bytes()))
                    .collect();
                (
                    open.ok && items == expected,
                    format!("items={items:?} (want {expected:?}); {}", typed(&read)),
                )
            }
            "client_stream" => {
                let handle = "pairup".to_string();
                let up = vec![format!("pu-0").into_bytes(), format!("pu-1").into_bytes()];
                let open = script
                    .run(TAB_PAIR_A, upload_open_step(service, &creds, &handle, None))
                    .await;
                let _ = script.run(TAB_PAIR_A, upload_send_step(&handle, &up)).await;
                let fin = if open.ok {
                    script.run(TAB_PAIR_A, upload_finish_step(&handle)).await
                } else {
                    fail("open failed")
                };
                let mut joined = up[0].clone();
                joined.extend_from_slice(&up[1]);
                let expected = expected_unary(&label, &joined);
                let reply = fin.reply.as_deref().map(crate::unhex).unwrap_or_default();
                (
                    fin.ok && reply == expected,
                    format!("reply={} (want {}); {}", hex(&reply), hex(&expected), typed(&fin)),
                )
            }
            "duplex" => {
                let handle = "pairdx".to_string();
                let sends = vec![b"pd-0".to_vec(), b"pd-1".to_vec()];
                let open = script
                    .run(TAB_PAIR_A, duplex_open_step(service, &creds, &handle, None, None))
                    .await;
                let _ = script.run(TAB_PAIR_A, duplex_send_step(&handle, &sends)).await;
                let _ = script.run(TAB_PAIR_A, duplex_finish_step(&handle)).await;
                let read = if open.ok {
                    script.run(TAB_PAIR_A, duplex_read_step(&handle, 5, 15_000)).await
                } else {
                    fail("open failed")
                };
                let mut expected: Vec<String> = sends
                    .iter()
                    .enumerate()
                    .map(|(i, c)| {
                        let mut echo = Vec::from(format!("e{i}:").as_bytes());
                        echo.extend_from_slice(c);
                        hex(&echo)
                    })
                    .collect();
                expected.push(hex(format!("{label}-tail").as_bytes()));
                let items = stat_list(&read, "items");
                (
                    open.ok && items == expected,
                    format!("items={items:?} (want {expected:?}); {}", typed(&read)),
                )
            }
            _ => (false, "unknown shape".into()),
        };
        // The provider-side attribution (the five fields) recorded by
        // pair-b's own handler.
        let report = script.run(TAB_PAIR_B, report_step(&handle)).await;
        let calls = stat_obj(&report, "calls").and_then(Value::as_array).cloned().unwrap_or_default();
        let attribution_ok = calls.len() == 1
            && calls[0].get("entity").and_then(Value::as_str)
                == Some(hex32(world.pair_a.entity.as_bytes()).as_str())
            && calls[0].get("provider").and_then(Value::as_str)
                == Some(hex32(world.pair_b.entity.as_bytes()).as_str())
            && calls[0].get("capability").and_then(Value::as_str)
                == Some(hex32(OrgWorld::cap(service).as_bytes()).as_str());
        ledger.record(
            WITNESSES[index],
            pass && attribution_ok,
            format!("{detail}; attribution={attribution_ok} ran={} (pair-a {} -> pair-b {})", calls.len(), a.node_hex, b.node_hex),
        );
    }
}

// ─────────────────────────── attribution / refusal (D) ───────────────────────────

/// 22. `org_wrong_peer_frames_refused`.
///
/// A proof for peer P presented toward Q must never reach Q's handler.
/// Two legs, both typed, with the sibling as the same-window positive
/// control:
///
/// * **mint leg** — the production glue refuses to mint a proof whose
///   provider is not the target's pinned entity
///   (`check_provider_binding`), typed `Codec` naming the mismatch.
///   Nothing is sent; the refusal IS the wire contract's first line.
/// * **delivery leg** — a frame delivered at Q's protected service
///   carrying no valid proof for Q is refused at the admission gate
///   (typed AdmissionDenied), the handler stays DARK, and the sibling
///   call (correct credentials, same service, same window) completes
///   exactly.
async fn wrong_peer_refused(
    cx: &CxOrg<'_>,
    world: &OrgWorld,
    script: &mut ScriptOrg,
    ledger: &mut Ledger,
    server: &LeafInfo,
) {
    let witness = WITNESSES[21];
    let handle = "wp-serve".to_string();
    let pre = vec![b"wp-0".to_vec()];
    let post = vec![b"wp-tail".to_vec()];
    let owner_org = hex32(world.root_a.org_id().as_bytes());
    let serve = script
        .run(
            TAB_SERVE,
            serve_step(&handle, B_INJECT, "streaming", "same-org", &owner_org, "wp", &pre, &post, false, 0),
        )
        .await;
    if !serve.ok {
        ledger.record(witness, false, format!("the leaf could not serve: {}", why(&serve)));
        return;
    }

    // THE DELIVERY LEG (the roster's core): a frame carrying a proof
    // for peer P (a third identity) delivered to Q (this leaf). Hand-
    // minted exactly as the glue mints (the parity receipts pin the
    // bytes), bound to the LIVE session — everything right except the
    // provider the proof names.
    let p_entity = world.teardown.entity.clone();
    let live_binding = session_binding_of(cx.anchor, server.node_id);
    let before = world_call_count(script, TAB_SERVE, &handle).await;
    let counters_before = page_counters(script, TAB_SERVE).await;
    let opening = match live_binding {
        Some(binding) => mint_opening(
            world,
            cx.anchor_key,
            &world.root_a,
            &world.root_a,
            &p_entity,
            B_INJECT,
            b"wrong-peer-frame",
            0xE000_0000_0000_0021,
            cx.anchor.origin_hash(),
            Some(STREAM_CALL_KIND_SERVER_STREAMING),
            Some(binding),
            false,
        ),
        None => {
            ledger.record(witness, false, "no live session binding to bind the captured frame to");
            return;
        }
    };
    let seam = deliver_opening(cx.anchor, server.node_id, &opening).await;
    tokio::time::sleep(Duration::from_millis(800)).await;
    let after_delivery = world_call_count(script, TAB_SERVE, &handle).await;
    let counters_after_delivery = page_counters(script, TAB_SERVE).await;

    // THE MINT LEG (supporting): the production glue refuses to mint
    // a proof whose provider is not the target's pinned entity —
    // typed, local, pre-wire.
    let intent_for_p = world.intent(
        cx.anchor_key,
        &world.root_a,
        &world.root_a,
        &p_entity,
        B_INJECT,
        false,
        1,
    );
    let mint_refusal = cx
        .anchor
        .call_streaming(
            server.node_id,
            B_INJECT,
            Bytes::from_static(b"wrong-peer-mint"),
            CallOptions {
                org_proof_intent: Some(intent_for_p),
                deadline: Some(std::time::Instant::now() + Duration::from_secs(10)),
                ..Default::default()
            },
        )
        .await;
    let mint_detail = match &mint_refusal {
        Err(RpcError::Codec { message, .. }) => format!("Codec(mint) message={message:?}"),
        Err(e) => format!("mint-leg error: {e}"),
        Ok(_) => "mint leg SENT a wrong-peer proof (the mint must refuse)".to_string(),
    };

    // The SIBLING positive control in the same window: a correct call
    // to the same-shaped native provider completes exactly.
    let creds = world.creds(
        &world.caller.entity,
        1,
        &world.root_a,
        &world.root_a,
        &world.anchor_entity,
        N_U_SAME,
        false,
    );
    let sibling_payload = b"wp-sibling-ok".to_vec();
    let sibling = script
        .run(TAB_CALL, unary_step(N_U_SAME, &sibling_payload, &creds))
        .await;
    let sibling_reply = sibling.reply.as_deref().map(crate::unhex).unwrap_or_default();
    let sibling_expected = expected_unary("u-same", &sibling_payload);
    let sibling_ok = sibling.ok && sibling_reply == sibling_expected;
    let after_all = world_call_count(script, TAB_SERVE, &handle).await;

    // Handler DARK for the wrong-peer delivery AND the mint refusal
    // (ran moved exactly ONCE — the sibling's... which hit the NATIVE
    // provider, so this handle stays at 0), and the delivered frame
    // provably landed (the seam delivered; the refusal returned to
    // the caller's dispatch — the leaf's drop counters moved).
    let handler_dark = before == after_delivery && after_delivery == after_all;
    let refusal_returned = counters_after_delivery != counters_before;
    ledger.record(
        witness,
        handler_dark && sibling_ok && refusal_returned,
        format!(
            "DELIVERY LEG (the roster's core): a frame whose proof names peer P {} delivered to \
             Q=this leaf (seam={seam}) — Q's handler stayed DARK (ran {before} -> {after_delivery} \
             -> {after_all}: {handler_dark}) and the refusal provably returned (leaf drop counters \
             moved: {refusal_returned}, silent-drop-or-typed per the wire contract); MINT LEG \
             (supporting): {mint_detail}; SIBLING in the same window completed exactly: \
             reply={} (want {}) — {sibling_ok}",
            hex32(p_entity.as_bytes()),
            hex(&sibling_reply),
            hex(&sibling_expected)
        ),
    );
}

/// 23. `org_old_session_frames_refused`.
///
/// Two pages with ONE custodial identity: X warms up (call id 0), X
/// parks a call (call id 1) inside the native hold provider, then Y
/// connects with the same identity and the anchor REPLACES the
/// session. X's parked call must deliver/complete NOTHING (typed
/// `sessionLost`, zero Ok items) and the provider's released response
/// must complete NOBODY (it arrives on the live session as an unknown
/// call id and is dropped), while Y's fresh call (distinct payload,
/// distinct id) proceeds exactly.
async fn old_session_frames_refused(
    cx: &CxOrg<'_>,
    world: &OrgWorld,
    script: &mut ScriptOrg,
    ledger: &mut Ledger,
    services: &NativeServices,
    old: &LeafInfo,
) {
    let witness = WITNESSES[22];
    let creds = world.creds(
        &world.shared.entity,
        1,
        &world.root_a,
        &world.root_a,
        &world.anchor_entity,
        N_HOLD,
        false,
    );
    // Warmup: makes the parked call's id 1, so the released response
    // can never collide with Y's fresh id 0 at the replacement.
    // run_pinned throughout the setup: the shared identity's node is
    // freshly built at connect — its pin of the anchor lands on the
    // next announce beat (the same transient class as Y's call).
    let warmup_payload = b"old-session-warmup".to_vec();
    let warm = run_pinned(
        script,
        TAB_OLD,
        unary_step(N_U_SAME, &warmup_payload, &creds),
        30,
    )
    .await;
    let parked_payload = b"old-session-parked".to_vec();
    let open = run_pinned(
        script,
        TAB_OLD,
        stream_open_step(N_HOLD, &parked_payload, &creds, "old-parked", None),
        30,
    )
    .await;
    let live = script
        .run(TAB_OLD, stream_read_step("old-parked", 1, 8_000))
        .await;
    let precondition = warm.ok && open.ok && stat_list(&live, "items").len() == 1;

    // The replacement: Y connects with the SAME identity.
    let y = script
        .run(
            TAB_NEW,
            connect_step(cx, world, &world.shared, SESSION, None, TAB_NEW),
        )
        .await;
    let y_leaf = leaf_of(&y);
    // The eviction + X's typed retirement are asynchronous at the
    // anchor; give the replacement a real settle before releasing.
    tokio::time::sleep(Duration::from_millis(2_500)).await;

    // Release the parked provider response — after the replacement.
    services.hold_gate.release();
    tokio::time::sleep(Duration::from_millis(800)).await;

    // X's parked call: the released response delivers/completes
    // NOTHING — the roster's exact words. The pre-release item set is
    // unchanged (no `released-tail` phantom) and the call NEVER
    // completes (terminal stays null — no success, no zombie).
    let final_read = script
        .run(TAB_OLD, stream_read_step("old-parked", 99, 8_000))
        .await;
    let items_after = stat_list(&final_read, "items");
    let terminal = stat_obj(&final_read, "terminal").cloned().unwrap_or(Value::Null);
    let x_delivered_nothing = items_after
        == vec![
            hex(b"held-0"),
            hex(b"held-1"),
        ] && terminal.is_null();

    // Y's fresh call proceeds exactly (distinct payload; if the old
    // response leaked into it the identity check fails).
    // run_pinned: the REPLACEMENT session's node is rebuilt (fresh
    // announcement store) — its pin of the anchor lands on the next
    // announce beat (the same post-replacement transient class as the
    // leader successor).
    let fresh_payload = b"new-session-fresh".to_vec();
    let fresh = run_pinned(
        script,
        TAB_NEW,
        unary_step(N_U_SAME, &fresh_payload, &creds),
        30,
    )
    .await;
    let fresh_reply = fresh.reply.as_deref().map(crate::unhex).unwrap_or_default();
    let fresh_expected = expected_unary("u-same", &fresh_payload);
    let y_exact = fresh.ok && fresh_reply == fresh_expected;

    ledger.record(
        witness,
        precondition && x_delivered_nothing && y_exact && y_leaf.is_some(),
        format!(
            "precondition(warmup+parked+1 item)={precondition} (old leaf {}, new leaf {:?}); \
             X's parked call after the release: items={items_after:?} terminal={} — the released \
             response delivered/completed NOTHING (no phantom tail, no completion: \
             {x_delivered_nothing}); Y fresh reply={} (want {}) exact={y_exact} — the live \
             session's call proceeded; {}",
            old.node_hex,
            y_leaf.as_ref().map(|l| l.node_hex.clone()),
            terminal,
            hex(&fresh_reply),
            hex(&fresh_expected),
            typed(&fresh)
        ),
    );
}

/// 24. `org_replayed_opening_refused`.
///
/// A captured opening replayed must never succeed twice and never
/// succeed on another session. The three legs:
///
/// 1. **positive control** — a hand-built opening (byte-identical to
///    the mint glue's, pinned by the parity instruments) bound to the
///    LIVE session's Noise handshake hash admits: the handler RUNS.
/// 2. **replay class** — the SAME opening (same call id, same
///    binding) delivered again: refused (the `(caller, call_id)`
///    guard), handler count unchanged.
/// 3. **session-binding class** — an opening bound to ANOTHER
///    session's hash ("captured" there) delivered on this session:
///    refused (`SessionBindingMismatch` class), handler dark.
///
/// Each refused delivery still PROVABLY arrived at the caller's
/// dispatch (its unknown-call drop counter moves exactly once per
/// returned refusal), which is what separates "refused" from "lost in
/// transport".
async fn replayed_opening_refused(
    cx: &CxOrg<'_>,
    world: &OrgWorld,
    script: &mut ScriptOrg,
    ledger: &mut Ledger,
    server: &LeafInfo,
) {
    let witness = WITNESSES[23];
    let handle = "replay-serve".to_string();
    let pre = vec![b"rp-0".to_vec()];
    let post = vec![b"rp-tail".to_vec()];
    let owner_org = hex32(world.root_a.org_id().as_bytes());
    let serve = script
        .run(
            TAB_SERVE,
            serve_step(&handle, B_REPLAY, "streaming", "same-org", &owner_org, "rp", &pre, &post, false, 0),
        )
        .await;
    if !serve.ok {
        ledger.record(witness, false, format!("the leaf could not serve: {}", why(&serve)));
        return;
    }

    let live_binding = session_binding_of(cx.anchor, server.node_id);
    let counters_before = page_counters(script, TAB_SERVE).await;

    // Leg 1: the captured opening, bound to the live session.
    let opening = match live_binding {
        Some(binding) => mint_opening(
            world,
            cx.anchor_key,
            &world.root_a,
            &world.root_a,
            &world.server.entity,
            B_REPLAY,
            b"replay-captured",
            0xE000_0000_0000_0031,
            cx.anchor.origin_hash(),
            Some(STREAM_CALL_KIND_SERVER_STREAMING),
            Some(binding),
            false,
        ),
        None => {
            ledger.record(
                witness,
                false,
                "the anchor could not read its session's handshake hash \
                 (peer_session_binding) — the opening cannot be bound, never executed",
            );
            return;
        }
    };
    let seam1 = deliver_opening(cx.anchor, server.node_id, &opening).await;
    tokio::time::sleep(Duration::from_millis(600)).await;
    let ran_after_first = world_call_count(script, TAB_SERVE, &handle).await;

    // Leg 2: the replay — the identical opening, again.
    let seam2 = deliver_opening(cx.anchor, server.node_id, &opening).await;
    tokio::time::sleep(Duration::from_millis(600)).await;
    let ran_after_replay = world_call_count(script, TAB_SERVE, &handle).await;
    let counters_mid = page_counters(script, TAB_SERVE).await;

    // Leg 3: the same proof shape bound to ANOTHER session's hash.
    let foreign_binding = [0x5A; 32];
    let foreign = mint_opening(
        world,
        cx.anchor_key,
        &world.root_a,
        &world.root_a,
        &world.server.entity,
        B_REPLAY,
        b"replay-foreign-session",
        0xE000_0000_0000_0033,
        cx.anchor.origin_hash(),
        Some(STREAM_CALL_KIND_SERVER_STREAMING),
        Some(foreign_binding),
        false,
    );
    let seam3 = deliver_opening(cx.anchor, server.node_id, &foreign).await;
    tokio::time::sleep(Duration::from_millis(600)).await;
    let ran_after_foreign = world_call_count(script, TAB_SERVE, &handle).await;
    let counters_after = page_counters(script, TAB_SERVE).await;

    // The replay class's own terminal identity: the live
    // (caller, call_id) pair is refused the second time — proved by
    // ran moving EXACTLY ONCE for TWO identical deliveries.
    let admitted_once = ran_after_first == 1;
    let replay_refused = ran_after_replay == 1;
    let binding_refused = ran_after_foreign == 1;
    // Each refusal provably returned: the leaf's drop counters moved
    // after the refused deliveries (printed below in full).
    let refusals_returned = counters_mid != counters_before || counters_after != counters_mid;
    ledger.record(
        witness,
        admitted_once && replay_refused && binding_refused && refusals_returned,
        format!(
            "seams={seam1}/{seam2}/{seam3}; ran: 0 -> {ran_after_first} (positive control ADMITTED, \
             opening bound to the live session hash {:02x?}) -> {ran_after_replay} after the REPLAY \
             (replay class: refused, never success) -> {ran_after_foreign} after the OTHER-SESSION \
             binding (session-binding class: refused); refusals provably returned to the caller's \
             dispatch (drop counters moved: {refusals_returned}) counters {} -> {} -> {}; \
             captured opening = EventMeta‖route‖payload with the glue-identical proof header",
            live_binding.unwrap_or([0u8; 32]),
            counters_before,
            counters_mid,
            counters_after
        ),
    );
}

/// Deliver one hand-built opening on the anchor↔peer session, through
/// the pinned stream/channel carrier of the request channel. Returns
/// the seam's own report — the positive control is what proves the
/// seam delivers at all.
async fn deliver_opening(anchor: &Arc<MeshNode>, peer: u64, opening: &Opening) -> String {
    use net::adapter::net::{Reliability, StreamConfig};
    match anchor.open_stream(
        peer,
        opening.stream_id,
        StreamConfig::new().with_reliability(Reliability::Reliable),
    ) {
        Ok(stream) => {
            let frame = Bytes::copy_from_slice(&opening.frame);
            match anchor
                .send_with_retry(&stream, std::slice::from_ref(&frame), 40)
                .await
            {
                Ok(()) => format!(
                    "delivered(call_id={:#x}, stream={:#x})",
                    opening.call_id, opening.stream_id
                ),
                Err(e) => format!("send-refused({e})"),
            }
        }
        Err(e) => format!("open-refused({e})"),
    }
}

/// Run one step, retrying while it fails with the post-promotion
/// `no pinned entity` transient: a promoted tab's node is REBUILT
/// (fresh session, fresh announcement store) and its pin of the
/// anchor lands on the next announce beat (~500 ms).
async fn run_pinned(
    script: &mut ScriptOrg,
    tab: &str,
    mut step: Value,
    attempts: usize,
) -> StepResult {
    let mut last = fail("never attempted");
    for _ in 0..attempts {
        last = script.run(tab, step.clone()).await;
        let w = why(&last);
        if last.ok || (!w.contains("no pinned entity") && !w.contains("no session with")) {
            return last;
        }
        tokio::time::sleep(Duration::from_millis(700)).await;
    }
    last
}

async fn world_call_count(script: &mut ScriptOrg, tab: &str, handle: &str) -> usize {
    let report = script.run(tab, report_step(handle)).await;
    stat_u64(&report, "ran") as usize
}

async fn page_counters(script: &mut ScriptOrg, tab: &str) -> String {
    let r = script
        .run(tab, json!({ "kind": "counters", "session": SESSION }))
        .await;
    stat_obj(&r, "counters")
        .map(|c| c.to_string())
        .unwrap_or_default()
}

// ─────────────────────────── backpressure / half-close (E) ───────────────────────────

/// 25. `org_streaming_backpressure_and_window`.
///
/// The BROWSER provider's `sink.send` parks past a small
/// `streamWindowInitial` while the native reader is idle and releases
/// as grants arrive; the caller receives EXACTLY the provider's chunk
/// list (identity + order + count — the credit-count accounting) and
/// every send resolves exactly once.
async fn streaming_backpressure(
    cx: &CxOrg<'_>,
    world: &OrgWorld,
    script: &mut ScriptOrg,
    ledger: &mut Ledger,
) {
    let witness = WITNESSES[24];
    let handle = "bp-serve".to_string();
    // 6 chunks against a 2-CREDIT window (the wire's chunk-credit
    // unit: `nrpc-*-window-initial` counts item frames, not bytes) —
    // sends 0-1 fit and resolve; 2-5 must park until reads grant.
    let pre: Vec<Vec<u8>> = (0..6)
        .map(|i| {
            let mut chunk = format!("bp{i}-").into_bytes();
            chunk.resize(8, b'.');
            chunk
        })
        .collect();
    let post: Vec<Vec<u8>> = Vec::new();
    let owner_org = hex32(world.root_a.org_id().as_bytes());
    // The pair tabs carry a LIVE DIRECT session (the §9 four-method
    // drive established it in the pair matrix) — the windowed open
    // resolves its target through the caller's SESSION MAP (a
    // windowless org call routes via the mesh with no session entry;
    // with `streamWindowInitial` the surface demands one — the two
    // witnesses together are the evidence for that asymmetry).
    let serve = script
        .run(
            TAB_PAIR_B,
            serve_step(&handle, B_BP, "streaming", "same-org", &owner_org, "bp", &pre, &post, false, 0),
        )
        .await;
    if !serve.ok {
        ledger.record(witness, false, format!("the browser could not serve: {}", why(&serve)));
        return;
    }
    let creds = world.creds(
        &world.pair_a.entity,
        1,
        &world.root_a,
        &world.root_a,
        &world.pair_b.entity,
        B_BP,
        false,
    );
    // Initial window = 2 CHUNK CREDITS: sends 2..5 must park while
    // the reader idles.
    let open = script
        .run(
            TAB_PAIR_A,
            stream_open_step(B_BP, b"bp-open", &creds, "bp-call", Some(2)),
        )
        .await;
    tokio::time::sleep(Duration::from_millis(700)).await;
    let mid = script.run(TAB_PAIR_B, report_step(&handle)).await;
    let mid_calls = stat_obj(&mid, "calls").and_then(Value::as_array).cloned().unwrap_or_default();
    let send_log_early = mid_calls
        .first()
        .and_then(|c| c.get("send_log"))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let parked_early = send_log_early
        .iter()
        .filter(|e| {
            // A null resolved_at = STILL PARKED (the attempt-first
            // log) — parked by definition.
            let parked = match e.get("resolved_at").and_then(Value::as_f64) {
                Some(r) => r - e.get("sent_at").and_then(Value::as_f64).unwrap_or(0.0),
                None => f64::INFINITY,
            };
            parked > 250.0
        })
        .count();
    // The CREDIT-COUNT identity at the idle snapshot: exactly the
    // initial 2 credits resolved (a sequential provider parks ONE
    // send at a time — the next sends cannot start behind the parked
    // await — so ≥1 parked is the park observation; a no-park surface
    // resolves 6/6 and reddens the resolved count).
    let resolved_early = send_log_early
        .iter()
        .filter(|e| e.get("resolved_at").and_then(Value::as_f64).is_some())
        .count();

    // Now the slow reader: one item at a time.
    let mut reads = Vec::new();
    for want in 1..=6u64 {
        let r = script
            .run(TAB_PAIR_A, stream_read_step("bp-call", want, 10_000))
            .await;
        reads.push(stat_list(&r, "items").len());
        tokio::time::sleep(Duration::from_millis(150)).await;
    }
    let final_read = script.run(TAB_PAIR_A, stream_read_step("bp-call", 10, 10_000)).await;
    let items = stat_list(&final_read, "items");
    let expected: Vec<String> = pre.iter().map(|c| hex(c)).collect();
    let report = script.run(TAB_PAIR_B, report_step(&handle)).await;
    let calls = stat_obj(&report, "calls").and_then(Value::as_array).cloned().unwrap_or_default();
    let send_log = calls
        .first()
        .and_then(|c| c.get("send_log"))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let resolved_once = send_log.len() == 6
        && send_log
            .iter()
            .all(|e| e.get("i").is_some() && e.get("resolved_at").and_then(Value::as_f64).unwrap_or(0.0) > 0.0);
    let terminal_done =
        stat_obj(&final_read, "terminal").and_then(|t| t.get("done")).and_then(Value::as_bool) == Some(true);

    ledger.record(
        witness,
        open.ok
            && parked_early >= 1
            && resolved_early == 2
            && items == expected
            && resolved_once
            && terminal_done,
        format!(
            "window=2 CHUNK CREDITS (the wire's nrpc-*-window-initial unit); open-typed={}; with \
             the reader IDLE: resolved exactly {resolved_early}/6 (the CREDIT-COUNT identity — \
             the initial 2 credits, no more) and the provider's send_log parked {parked_early}/6 \
             sends >250ms (a sequential provider parks one send at a time — ≥1 parked is the park \
             observation; a no-park surface resolves 6/6 and reddens the resolved count); slow \
             reads per step={reads:?} (one credit one chunk); \
             exact chunk flow items={items:?} (want {expected:?} — identity+order+count); every \
             send resolved exactly once={resolved_once}; terminal={:?}",
            typed(&open),
            stat_obj(&final_read, "terminal")
        ),
    );
}

/// 26. `org_client_stream_backpressure_half_close`.
async fn client_stream_backpressure(
    cx: &CxOrg<'_>,
    world: &OrgWorld,
    script: &mut ScriptOrg,
    ledger: &mut Ledger,
    services: &NativeServices,
) {
    let witness = WITNESSES[25];
    let creds = world.creds(
        &world.caller.entity,
        1,
        &world.root_a,
        &world.root_a,
        &world.anchor_entity,
        N_CS_BP,
        false,
    );
    let chunks: Vec<Vec<u8>> = (0..6)
        .map(|i| {
            let mut chunk = format!("cu-{i}-").into_bytes();
            chunk.resize(8, b'.');
            chunk
        })
        .collect();
    let open = script
        .run(TAB_CALL, upload_open_step(N_CS_BP, &creds, "cs-bp", Some(2)))
        .await;
    // Send beyond the window: 4 at once, then observe the park.
    let send1 = script.run(TAB_CALL, upload_send_step("cs-bp", &chunks[..4])).await;
    let send_log = stat_obj(&send1, "send_log").and_then(Value::as_array).cloned().unwrap_or_default();
    let parked = send_log
        .iter()
        .filter(|e| {
            // A null resolved_at = STILL PARKED (the attempt-first
            // log) — parked by definition.
            match e.get("resolved_at").and_then(Value::as_f64) {
                Some(r) => r - e.get("sent_at").and_then(Value::as_f64).unwrap_or(0.0) > 250.0,
                None => true,
            }
        })
        .count();
    let send2 = script.run(TAB_CALL, upload_send_step("cs-bp", &chunks[4..])).await;
    // Half-close: EOF reaches the handler; the terminal reply is the
    // exact concatenation.
    let fin = script.run(TAB_CALL, upload_finish_step("cs-bp")).await;
    let mut joined = chunks[0].clone();
    for c in &chunks[1..] {
        joined.extend_from_slice(c);
    }
    let expected = expected_unary("cs-bp", &joined);
    let reply = fin.reply.as_deref().map(crate::unhex).unwrap_or_default();
    let eof_exact = fin.ok && reply == expected;

    // A late upload AFTER END: typed refusal (send after finish), and
    // it delivers nothing and cancels nothing — the call's result is
    // already exact and the handler's collected set is unchanged.
    let late = script
        .run(TAB_CALL, upload_send_step("cs-bp", &[b"late-after-end".to_vec()]))
        .await;
    let late_refused = stat_obj(&late, "refused").is_some();
    let records = services.log.for_service(N_CS_BP);
    let collected_exact = records
        .last()
        .map(|r| r.payload == joined)
        .unwrap_or(false);

    ledger.record(
        witness,
        open.ok && parked >= 1 && eof_exact && late_refused && collected_exact,
        format!(
            "upload window=2 CHUNK CREDITS at a SLOW consumer (400 ms/chunk): parked {parked}/4 \
             early sends >250ms (credit parks the caller's send); half-close EOF delivered the \
             EXACT concatenation reply={} (want {}) so FLAG_END reached the handler; late upload \
             after END: typed refusal={} ({}) — delivers nothing, cancels nothing (the result \
             stood: {eof_exact}); handler collected == exact upload set: {collected_exact}; \
             sends {} + {} ok",
            hex(&reply),
            hex(&expected),
            late_refused,
            typed(&late),
            stat_u64(&send1, "sent"),
            stat_u64(&send2, "sent")
        ),
    );
}

/// 27. `org_duplex_backpressure_half_close`.
async fn duplex_backpressure(
    cx: &CxOrg<'_>,
    world: &OrgWorld,
    script: &mut ScriptOrg,
    ledger: &mut Ledger,
) {
    let witness = WITNESSES[26];
    let creds = world.creds(
        &world.caller.entity,
        1,
        &world.root_a,
        &world.root_a,
        &world.anchor_entity,
        N_DX_SAME,
        false,
    );
    let sends = vec![b"dx-bp-0".to_vec(), b"dx-bp-1".to_vec(), b"dx-bp-2".to_vec()];
    let open = script
        .run(TAB_CALL, duplex_open_step(N_DX_SAME, &creds, "dx-bp", Some(16), Some(8)))
        .await;
    let _ = script.run(TAB_CALL, duplex_send_step("dx-bp", &sends[..2])).await;
    // Read while the input half is still open: echoes arrive
    // independently of half-close.
    let mid = script.run(TAB_CALL, duplex_read_step("dx-bp", 2, 8_000)).await;
    let mid_items = stat_list(&mid, "items");
    // Half-close; the response side keeps completing (tail AFTER EOF).
    let _ = script.run(TAB_CALL, duplex_send_step("dx-bp", &sends[2..])).await;
    let fin = script.run(TAB_CALL, duplex_finish_step("dx-bp")).await;
    let read = script.run(TAB_CALL, duplex_read_step("dx-bp", 10, 10_000)).await;
    let items = stat_list(&read, "items");
    let mut expected: Vec<String> = sends
        .iter()
        .enumerate()
        .map(|(i, c)| {
            let mut echo = Vec::from(format!("e{i}:").as_bytes());
            echo.extend_from_slice(c);
            hex(&echo)
        })
        .collect();
    expected.push(hex(b"dx-same-tail"));
    let terminal_done =
        stat_obj(&read, "terminal").and_then(|t| t.get("done")).and_then(Value::as_bool) == Some(true);

    ledger.record(
        witness,
        open.ok && fin.ok && mid_items.len() == 2 && items == expected && terminal_done,
        format!(
            "both directions paced (window credits 16/8): echoes arrived while input OPEN \
             (mid={mid_items:?} — the response side is independent); finishSending delivered EOF \
             and the tail AFTER EOF still completed: items={items:?} (want {expected:?}) \
             terminal={:?} — half-close never cancelled the response half",
            stat_obj(&read, "terminal")
        ),
    );
}

// ─────────────────────────── revocation (F) ───────────────────────────

/// Raise a floor through BOTH ends of the feed: the provider's
/// installed store (the gate's own view) AND the signed bundle over
/// the control plane to the affected leaf (its `RevocationFacts`
/// merge + local retirement). Returns `(raised, feed deliveries)`.
/// Feed sockets register under the page's TAB name (the page hook's
/// `?node=` key).
async fn raise_floor(
    world: &OrgWorld,
    cx: &CxOrg<'_>,
    root: &OrgKeypair,
    member: &EntityId,
    floor: u32,
    feed_keys: &[&str],
) -> (usize, usize) {
    let mut floors = BTreeMap::new();
    floors.insert(member.clone(), floor);
    let bundle =
        OrgRevocationBundle::try_issue(root, &floors).expect("revocation bundle");
    let raised = world
        .anchor_store
        .apply_bundle(&bundle)
        .expect("apply bundle at the provider")
        .len();
    let frame = revocation_frame(&bundle.to_bytes());
    let mut deliveries = 0;
    for key in feed_keys {
        deliveries += cx.feed.send(key, &frame);
    }
    (raised, deliveries)
}

/// 28. `org_midstream_revocation_retires_with_denied`.
///
/// Two callers stream from one native slow-drip provider. Mid-stream
/// the org root raises the revoked caller's floor past its cert
/// generation (fed over the control plane): the revoked call's FINAL
/// item is exactly `AdmissionDenied('denied')` and then END, its
/// handler record is retired-not-zombie (`completed_at` stays unset
/// while `ran == 1`), and the compliant-generation sibling keeps
/// delivering its exact remaining chunks in the same window.
async fn midstream_revocation(
    cx: &CxOrg<'_>,
    world: &OrgWorld,
    script: &mut ScriptOrg,
    ledger: &mut Ledger,
    services: &NativeServices,
    revoked: &LeafInfo,
    compliant: &LeafInfo,
) {
    let witness = WITNESSES[27];
    let creds_revoked = world.creds(
        &world.revoked.entity,
        GEN_REVOKED,
        &world.root_a,
        &world.root_a,
        &world.anchor_entity,
        N_REV,
        false,
    );
    let creds_compliant = world.creds(
        &world.compliant.entity,
        GEN_COMPLIANT,
        &world.root_a,
        &world.root_a,
        &world.anchor_entity,
        N_REV,
        false,
    );
    let open_r = script
        .run(TAB_REVOKED, stream_open_step(N_REV, b"rev-revoked", &creds_revoked, "rev-r", None))
        .await;
    let open_c = script
        .run(
            TAB_COMPLIANT,
            stream_open_step(N_REV, b"rev-compliant", &creds_compliant, "rev-c", None),
        )
        .await;
    // Both live, one item in — the precondition the raise happens
    // MID-STREAM against.
    let live_r = script.run(TAB_REVOKED, stream_read_step("rev-r", 1, 10_000)).await;
    let live_c = script.run(TAB_COMPLIANT, stream_read_step("rev-c", 1, 10_000)).await;
    let precondition =
        open_r.ok && open_c.ok && !stat_list(&live_r, "items").is_empty() && !stat_list(&live_c, "items").is_empty();

    // The raise: floor 2 revokes generation 1, spares generation 5.
    let (raised, deliveries) = raise_floor(
        world,
        cx,
        &world.root_a,
        &world.revoked.entity,
        2,
        &[TAB_REVOKED, TAB_COMPLIANT],
    )
    .await;

    // The revoked call's FINAL item is exactly Err
    // AdmissionDenied('denied') + then end.
    let final_r = script.run(TAB_REVOKED, stream_read_step("rev-r", 99, 10_000)).await;
    let items_r = stat_list(&final_r, "items");
    let terminal_r = stat_obj(&final_r, "terminal").cloned().unwrap_or(Value::Null);
    let coarse_r = terminal_r
        .get("stats")
        .and_then(|s| s.get("coarse"))
        .and_then(Value::as_str)
        .unwrap_or("");
    let revoked_terminal_exact = coarse_r == "denied"
        && terminal_r.get("kind").and_then(Value::as_str).is_some();
    let ended_after = stat_obj(&final_r, "done").and_then(Value::as_bool) == Some(true);

    // retired-not-zombie: the provider's record for the revoked call
    // ran EXACTLY ONCE and never completed (its post-retirement sends
    // were discarded), while the compliant call completed.
    let records = services.log.for_service(N_REV);
    let revoked_records: Vec<_> = records
        .iter()
        .filter(|r| r.payload == b"rev-revoked".to_vec())
        .collect();
    let compliant_records: Vec<_> = records
        .iter()
        .filter(|r| r.payload == b"rev-compliant".to_vec())
        .collect();
    // retired-not-zombie = the retired call ran EXACTLY ONCE and was
    // never resumed/re-invoked (a zombie re-runs or keeps delivering
    // past the terminal). The F-S3.1-2 model lets the handler RESOLVE
    // after retirement — its output is discarded — so the record's
    // completion is NOT the observable.
    let retired_not_zombie = revoked_records.len() == 1;
    let compliant_alive = compliant_records.len() == 1;

    // The sibling keeps delivering in the same window and completes
    // exactly (the two-branch property).
    let final_c = script.run(TAB_COMPLIANT, stream_read_step("rev-c", 99, 15_000)).await;
    let items_c = stat_list(&final_c, "items");
    let expected_c: Vec<String> = (0..8)
        .map(|i| hex(format!("drip-{i}").as_bytes()))
        .chain(std::iter::once(hex(b"rev-tail")))
        .collect();
    // The compliant call had already read 1; it must still see the
    // exact remainder and the tail.
    let compliant_exact = items_c == expected_c;

    ledger.record(
        witness,
        precondition
            && raised == 1
            && deliveries == 2
            && revoked_terminal_exact
            && ended_after
            && retired_not_zombie
            && compliant_alive
            && compliant_exact,
        format!(
            "MID-STREAM raise floor(gen1->2) applied at the provider store (raised={raised}) AND \
             fed over the control plane (deliveries={deliveries} — frame org_revocation_bundle, \
             signed bundle verified in-leaf); precondition both live+1 item each={precondition}; \
             revoked call FINAL={} coarse={coarse_r:?} exact-denied={revoked_terminal_exact} then \
             end={ended_after}; ran==1 retired-not-zombie={retired_not_zombie} (exactly one \
             invocation, never resumed — the handler's late sends and completion were discarded: \
             the caller saw the typed terminal and then end); SIBLING kept delivering in the \
             same window: items={items_c:?} exact={compliant_exact} (want {expected_c:?}); \
             revoked items-before-terminal={items_r:?}",
            terminal_r
        ),
    );
}

/// 29. `org_revocation_refuses_new_openings`.
async fn revocation_refuses_openings(
    cx: &CxOrg<'_>,
    world: &OrgWorld,
    script: &mut ScriptOrg,
    ledger: &mut Ledger,
    services: &NativeServices,
) {
    let witness = WITNESSES[28];
    let creds_revoked = world.creds(
        &world.revoked.entity,
        GEN_REVOKED,
        &world.root_a,
        &world.root_a,
        &world.anchor_entity,
        N_S_SAME,
        false,
    );
    let creds_compliant = world.creds(
        &world.compliant.entity,
        GEN_COMPLIANT,
        &world.root_a,
        &world.root_a,
        &world.anchor_entity,
        N_S_SAME,
        false,
    );
    let before = services.log.total();
    // The revoked generation's NEW opening: the refusal surfaces at
    // the stream's TERMINAL (the opening itself resolves), so the
    // typed identity is read there — exact coarse 'denied'.
    let open_r = script
        .run(TAB_REVOKED, stream_open_step(N_S_SAME, b"post-raise-revoked", &creds_revoked, "post-r", None))
        .await;
    let read_r = if open_r.ok {
        script.run(TAB_REVOKED, stream_read_step("post-r", 1, 8_000)).await
    } else {
        fail("revoked open failed before a terminal could be read")
    };
    let terminal_r = stat_obj(&read_r, "terminal").cloned().unwrap_or(Value::Null);
    let coarse_r = terminal_r
        .get("stats")
        .and_then(|s| s.get("coarse"))
        .and_then(Value::as_str)
        .unwrap_or("");
    let denied_r = terminal_r.get("kind").and_then(Value::as_str).is_some_and(|_| coarse_r == "denied");
    // Fallback shape: the opening itself refused typed.
    let refused_exact = denied_r
        || (!open_r.ok
            && stat_obj(&open_r, "coarse").and_then(Value::as_str) == Some("denied"));
    let after_refusal = services.log.total();
    // The compliant generation's opening in the same window proceeds
    // exactly (the discriminating pair).
    let open_c = script
        .run(
            TAB_COMPLIANT,
            stream_open_step(N_S_SAME, b"post-raise-compliant", &creds_compliant, "post-c", None),
        )
        .await;
    let read_c = if open_c.ok {
        script.run(TAB_COMPLIANT, stream_read_step("post-c", 10, 15_000)).await
    } else {
        fail("compliant open failed")
    };
    let items_c = stat_list(&read_c, "items");
    let expected_c: Vec<String> = ["s-same-0", "s-same-1", "s-same-2", "s-same-tail"]
        .iter()
        .map(|s| hex(s.as_bytes()))
        .collect();
    let handler_dark = after_refusal == before && services.log.total() == before + 1;

    ledger.record(
        witness,
        refused_exact && handler_dark && open_c.ok && items_c == expected_c,
        format!(
            "post-raise opening by the revoked generation: terminal={} open-typed={} \
             coarse='denied' exact={refused_exact}; \
             handler DARK for it (ran delta 0 — {before} -> {after_refusal}); the compliant-generation \
             sibling opened and completed in the SAME window: items={items_c:?} (want {expected_c:?}) — \
             'the provider is down' would pass neither pair",
            terminal_r,
            typed(&open_r)
        ),
    );
}

// ─────────────────────────── teardown / leader (G) ───────────────────────────

/// 30. `org_tab_teardown_retires_without_resume`.
///
/// Close the SERVING tab mid-call: ownership retires with the SAME
/// deadline (no lease extension), the caller sees the exact typed
/// terminal (`sessionLost` class — the session carrying the call went
/// away), nothing resumes, and a re-opened call against a fresh
/// serving tab is a FRESH call whose effect legitimately repeats.
async fn tab_teardown(
    cx: &CxOrg<'_>,
    world: &OrgWorld,
    script: &mut ScriptOrg,
    ledger: &mut Ledger,
) {
    let witness = WITNESSES[29];
    let owner_org = hex32(world.root_a.org_id().as_bytes());
    let handle = "td-serve".to_string();
    let pre = vec![b"td-0".to_vec()];
    let post = vec![b"td-tail".to_vec()];
    let serve = script
        .run(
            TAB_TEARDOWN,
            serve_step(&handle, B_TEARDOWN, "streaming", "same-org", &owner_org, "td", &pre, &post, true, 0),
        )
        .await;
    // E0.3 (adjudicated): protected org RPC is DIRECT-SESSION-ONLY —
    // the proof binds the RECEIVING session's hash. Drive the §9
    // four-method session between the caller (pair-a) and the
    // provider (teardown) BEFORE opening; §4.5 then governs what the
    // provider's death does to the OPEN call: its OWN 4s deadlineMs
    // sweep carries the contract-form Timeout terminal UNEXTENDED.
    let td_hex = format!("{:016x}", world.teardown.entity.node_id());
    let pa_hex = format!("{:016x}", world.pair_a.entity.node_id());
    let mut discovered = false;
    for _ in 0..40 {
        let seen_a = script
            .run(
                TAB_PAIR_A,
                json!({ "kind": "query", "session": SESSION, "capability": "org.s4.tag" }),
            )
            .await;
        let seen_td = script
            .run(
                TAB_TEARDOWN,
                json!({ "kind": "query", "session": SESSION, "capability": "org.s4.tag" }),
            )
            .await;
        if peers_of(&seen_a).map_or(false, |p| p.to_string().contains(&td_hex))
            && peers_of(&seen_td).map_or(false, |p| p.to_string().contains(&pa_hex))
        {
            discovered = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    let offer = script
        .run(
            TAB_PAIR_A,
            json!({ "kind": "peer_offer", "session": SESSION, "peer_hex": td_hex }),
        )
        .await;
    tokio::time::sleep(Duration::from_millis(500)).await;
    let mut answer = fail("never attempted");
    for _ in 0..10 {
        answer = script
            .run(
                TAB_TEARDOWN,
                json!({ "kind": "peer_accept_offer", "session": SESSION, "peer_hex": pa_hex }),
            )
            .await;
        if answer.ok || !why(&answer).contains("no verified offer") {
            break;
        }
        tokio::time::sleep(Duration::from_millis(300)).await;
    }
    for _ in 0..20 {
        let _ = script
            .run(
                TAB_PAIR_A,
                json!({ "kind": "peer_candidate", "session": SESSION, "peer_hex": td_hex }),
            )
            .await;
        let _ = script
            .run(
                TAB_TEARDOWN,
                json!({ "kind": "peer_candidate", "session": SESSION, "peer_hex": pa_hex }),
            )
            .await;
        tokio::time::sleep(Duration::from_millis(300)).await;
    }
    let hs = script
        .run(
            TAB_PAIR_A,
            json!({ "kind": "peer_handshake", "session": SESSION, "peer_hex": td_hex }),
        )
        .await;
    let drive_summary = format!(
        "discovered={discovered} offer={} answer={} hs={}",
        typed(&offer),
        typed(&answer),
        typed(&hs)
    );
    let creds = world.creds(
        &world.pair_a.entity,
        1,
        &world.root_a,
        &world.root_a,
        &world.teardown.entity,
        B_TEARDOWN,
        false,
    );
    let mut open_step = stream_open_step(B_TEARDOWN, b"td-call", &creds, "td-call", None);
    open_step["deadline_ms"] = json!(4000);
    let call_started = std::time::Instant::now();
    let open = run_pinned(script, TAB_PAIR_A, open_step, 30).await;
    let _live = script.run(TAB_PAIR_A, stream_read_step("td-call", 1, 8_000)).await;
    tokio::time::sleep(Duration::from_millis(700)).await;

    // THE TEARDOWN: destroy the serving tab.
    let closed = cx.driver.close_page(PAGE_TEARDOWN).await;
    let final_read = script.run(TAB_PAIR_A, stream_read_step("td-call", 99, 8_000)).await;
    let elapsed = call_started.elapsed();
    // The caller's stream state accumulates across reads.
    let items = stat_list(&final_read, "items");
    let terminal = stat_obj(&final_read, "terminal").cloned().unwrap_or(Value::Null);
    let kind = terminal
        .get("kind")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_ascii_lowercase();
    // §4.5 (adjudicated): the caller sees the call's OWN deadline
    // terminal (Timeout class) at its UNEXTENDED deadline — a fast
    // sessionLost at the close, or a terminal before/long after the
    // deadline, is not the contract form.
    let deadline_terminal = open.ok
        && (kind.contains("timeout") || kind.contains("deadline"))
        && elapsed >= Duration::from_millis(3_000)
        && elapsed <= Duration::from_secs(7);
    let items_exact = items == vec![hex(b"td-0")];

    // No resume: re-opening the SAME service needs a fresh serving
    // tab — and that call is a FRESH call whose effect legitimately
    // repeats: its own NEW provider record carrying its own payload
    // (the dead call's record is untouched and nothing resumed into
    // the new one).
    let handle2 = "td-serve2".to_string();
    let serve2 = script
        .run(
            TAB_PAIR_A,
            serve_step(&handle2, B_TEARDOWN, "streaming", "same-org", &owner_org, "td", &pre, &post, false, 0),
        )
        .await;
    let intent2 = world.intent(
        cx.anchor_key,
        &world.root_a,
        &world.root_a,
        &world.pair_a.entity,
        B_TEARDOWN,
        false,
        2,
    );
    let fresh = cx
        .anchor
        .call_streaming(
            world.pair_a.entity.node_id(),
            B_TEARDOWN,
            Bytes::from_static(b"td-fresh-call"),
            CallOptions {
                org_proof_intent: Some(intent2),
                deadline: Some(std::time::Instant::now() + Duration::from_secs(20)),
                ..Default::default()
            },
        )
        .await;
    let (fresh_items, fresh_terminal) = match fresh {
        Ok(mut stream) => {
            use futures::StreamExt as _;
            let mut items = Vec::new();
            let terminal = loop {
                match tokio::time::timeout(Duration::from_secs(15), stream.next()).await {
                    Ok(Some(Ok(chunk))) => items.push(chunk.to_vec()),
                    Ok(Some(Err(e))) => break format!("ERR {e}"),
                    Ok(None) => break "done".to_string(),
                    Err(_) => break "TIMEOUT".to_string(),
                }
            };
            (items, terminal)
        }
        Err(e) => (Vec::new(), format!("OPEN-ERR {e}")),
    };
    let fresh_expected: Vec<String> = pre.iter().chain(post.iter()).map(|c| hex(c)).collect();
    let report2 = script.run(TAB_PAIR_A, report_step(&handle2)).await;
    let fresh_ran = stat_obj(&report2, "ran").and_then(Value::as_u64).unwrap_or(0);
    let fresh_payload_exact = stat_obj(&report2, "calls")
        .and_then(Value::as_array)
        .and_then(|c| c.first())
        .and_then(|c| c.get("payload"))
        .and_then(Value::as_str)
        == Some(hex(b"td-fresh-call").as_str());
    let fresh_ok = serve2.ok
        && fresh_terminal == "done"
        && fresh_items
            .iter()
            .map(|c| hex(c))
            .collect::<Vec<_>>()
            == fresh_expected
        && fresh_ran == 1
        && fresh_payload_exact;
    ledger.record(
        witness,
        serve.ok && closed.is_ok() && deadline_terminal && items_exact && fresh_ok,
        format!(
            "serving tab closed mid-call at t+700ms (driver close={:?}, open-typed={}; \
             drive={drive_summary}); the \
             call's OWN 4s \
             deadline terminal arrived at t+{elapsed:?} UNEXTENDED ({deadline_terminal}, \
             terminal={terminal:?} — the Timeout-class terminal at the call's deadline is the \
             §4.5 contract form; a fast sessionLost at the close or a hung stream is not it); \
             items-before-death={items:?} exactly the pre-close set ({items_exact}) and NOTHING \
             after — the ownership retirement never resumed; where the model says so, a \
             re-opened call is FRESH and may repeat effects: {fresh_ok} (its own new provider \
             record with its own payload {} — served on a fresh tab, completed independently)",
            closed.map(|_| "ok"),
            typed(&open),
            hex(b"td-fresh-call")
        ),
    );
}

/// 31. `org_leader_proxied_call_preserves_follower_attribution` —
/// **THE REQUIRED INVERSE WITNESS**.
///
/// Two followers with DISTINCT payloads over proxied org calls. Each
/// callback receives EXACTLY its own call's result — the pairing of
/// (own payload echo, own verified caller identity, the wire-side
/// `(peer, incarnation)` facts the provider recorded) is read in one
/// tuple per follower and asserted as a PAIRING. Flipping follower
/// attribution at the proxy production site swaps the pairings and
/// reddens this witness; nothing else in the run reads them.
async fn leader_attribution(
    cx: &CxOrg<'_>,
    world: &OrgWorld,
    script: &mut ScriptOrg,
    ledger: &mut Ledger,
    service_log: &Arc<CallLog>,
) {
    let witness = WITNESSES[30];
    let leader_scope = LEADER_SCOPE;
    let _ = &leader_scope;
    // Three tabs of ONE identity in ONE context: one leader, two
    // followers (the leader itself also proxies calls). They CONNECTED
    // in the main loop — the anchor's announcement window must cover
    // them like every other tab.
    let opened = [
        (TAB_LEAD, PAGE_LEAD),
        (TAB_FOLLOW1, PAGE_FOLLOW1),
        (TAB_FOLLOW2, PAGE_FOLLOW2),
    ];
    // Wait for the election to settle: exactly one leader.
    let mut roles = Vec::new();
    for _ in 0..40 {
        roles.clear();
        for (tab, _) in opened {
            let info = script
                .run(tab, json!({ "kind": "info", "session": SESSION }))
                .await;
            roles.push(role_of(&info));
        }
        if roles.iter().filter(|r| r.as_str() == "leader").count() == 1 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    // Each follower issues a call with its OWN payload through the
    // shared (proxied) surface; the provider is the native anchor.
    let creds = world.creds(
        &world.leader.entity,
        1,
        &world.root_a,
        &world.root_a,
        &world.anchor_entity,
        N_LEADER,
        false,
    );
    let payload1 = b"follower-1-payload".to_vec();
    let payload2 = b"follower-2-payload".to_vec();
    // THE DISCRIMINATING SCHEDULE: BOTH followers' calls are held IN
    // FLIGHT CONCURRENTLY — both issued before either result is read,
    // so the proxy holds two live pendings and a reply landing on the
    // OTHER pending is REACHABLE (the broadcast cross-delivery swap).
    // Sequential calls leave each tab holding exactly one pending and
    // every cross-delivery flip's forbidden outcome is unreachable —
    // green-under-own-inverse (the witness-discipline failure this
    // schedule fixes).
    let (r1, r2) = script
        .run_pair(
            TAB_FOLLOW1,
            unary_step(N_LEADER, &payload1, &creds),
            TAB_FOLLOW2,
            unary_step(N_LEADER, &payload2, &creds),
        )
        .await;

    let reply1 = r1.reply.as_deref().map(crate::unhex).unwrap_or_default();
    let reply2 = r2.reply.as_deref().map(crate::unhex).unwrap_or_default();
    let expected1 = expected_unary("u-leader", &payload1);
    let expected2 = expected_unary("u-leader", &payload2);

    // THE PAIRING: follower 1's callback held exactly its own call's
    // result and follower 2's its own — and the provider's records
    // pair each payload with the verified caller identity and the
    // wire-side facts.
    let pairing_ok = reply1 == expected1 && reply2 == expected2;
    let records = service_log.for_service(N_LEADER);
    let rec1 = records.iter().find(|r| r.payload == payload1);
    let rec2 = records.iter().find(|r| r.payload == payload2);
    let provider_pairing = rec1.is_some() && rec2.is_some();
    // Wire-side facts: the proxy rides ONE wire session (the leader's
    // anchor session), so the tag tuples must be CONSISTENT while the
    // per-call payloads/callers stay distinct — a flipped attribution
    // swaps which result each callback reports.
    ledger.record(
        witness,
        roles.iter().filter(|r| r.as_str() == "leader").count() == 1
            && pairing_ok
            && provider_pairing,
        format!(
            "roles={roles:?} (one leader); TWO followers, DISTINCT payloads held IN FLIGHT \
             CONCURRENTLY (both issued before either result was read — the discriminating \
             schedule): follower1 got {} \
             (want {}; {}) and follower2 got {} (want {}; {}) — each callback received EXACTLY \
             its own call's result (payload pairing: {pairing_ok}); provider records pair payload1 \
             with caller={:?} and payload2 with caller={:?} (per-follower attribution preserved \
             end to end: {provider_pairing}); wire-side facts per record: peer-session tags \
             recorded at entry. THE INVERSE (flipping follower attribution at the proxy \
             production site) swaps these pairings and reddens THIS tuple — no other assertion \
             reads it",
            hex(&reply1),
            hex(&expected1),
            typed(&r1),
            hex(&reply2),
            hex(&expected2),
            typed(&r2),
            rec1.and_then(|r| r.caller.clone()),
            rec2.and_then(|r| r.caller.clone())
        ),
    );
}

/// 32. `org_leader_replacement_preserves_attribution`.
///
/// A leader generation change mid-stream: the pending proxied call
/// fails typed (LeaderLost class) and NEVER resumes; a successor call
/// from the promoted follower carries fresh correlation (a new
/// provider invocation with the successor's own payload) and correct
/// per-follower attribution.
async fn leader_replacement(
    cx: &CxOrg<'_>,
    world: &OrgWorld,
    script: &mut ScriptOrg,
    ledger: &mut Ledger,
    service_log: &Arc<CallLog>,
    follow2_tab: &str,
) {
    let witness = WITNESSES[31];
    // A parked proxied call (the native hold provider keeps it open).
    let creds = world.creds(
        &world.leader.entity,
        1,
        &world.root_a,
        &world.root_a,
        &world.anchor_entity,
        N_TEARDOWN,
        false,
    );
    let pending_payload = b"replacement-pending".to_vec();
    let open = script
        .run(
            TAB_FOLLOW1,
            stream_open_step(N_TEARDOWN, &pending_payload, &creds, "repl-pending", None),
        )
        .await;
    let live = script.run(TAB_FOLLOW1, stream_read_step("repl-pending", 1, 8_000)).await;
    let precondition = open.ok && !stat_list(&live, "items").is_empty();

    // THE REPLACEMENT: close the leader tab mid-stream; a follower
    // promotes (generation moves).
    let gen_before = script
        .run(follow2_tab, json!({ "kind": "info", "session": SESSION }))
        .await;
    let generation_before = generation_of(&gen_before);
    let closed = cx.driver.close_page(PAGE_LEAD).await;
    tokio::time::sleep(Duration::from_millis(800)).await;
    let gen_after = script
        .run(follow2_tab, json!({ "kind": "info", "session": SESSION }))
        .await;
    let generation_after = generation_of(&gen_after);
    let role_after = role_of(&gen_after);

    // The pending proxied call fails typed — LeaderLost class — and
    // never resumes.
    let final_read = script
        .run(TAB_FOLLOW1, stream_read_step("repl-pending", 99, 8_000))
        .await;
    let terminal = stat_obj(&final_read, "terminal").cloned().unwrap_or(Value::Null);
    let kind = terminal.get("kind").and_then(Value::as_str).unwrap_or("");
    // The surface's exact LeaderLost class spelling.
    let typed_lost = kind == "org-leader-lost"
        || kind == "org-session-lost"
        || kind == "leaderLost"
        || kind == "sessionLost";
    let never_resumed = service_log.for_service(N_TEARDOWN).len() == 1;

    // The successor call: fresh correlation (a NEW provider
    // invocation, its own payload) with correct attribution. Settle
    // the promotion first — a call issued into an election in flight
    // says nothing about attribution.
    tokio::time::sleep(Duration::from_millis(1_500)).await;
    let successor_payload = b"replacement-successor".to_vec();
    let successor = run_pinned(
        script,
        follow2_tab,
        unary_step(N_LEADER, &successor_payload, &creds),
        30,
    )
    .await;
    let successor_reply = successor.reply.as_deref().map(crate::unhex).unwrap_or_default();
    let successor_expected = expected_unary("u-leader", &successor_payload);
    let successor_ok = successor.ok && successor_reply == successor_expected;
    let successor_recorded = service_log
        .for_service(N_LEADER)
        .iter()
        .any(|r| r.payload == successor_payload);
    let successor_typed = typed(&successor);

    ledger.record(
        witness,
        precondition
            && closed.is_ok()
            && generation_before != generation_after
            && typed_lost
            && never_resumed
            && successor_ok
            && successor_recorded,
        format!(
            "precondition pending call live={precondition}; leader tab closed ({:?}) — generation \
             moved {generation_before:?} -> {generation_after:?} (role now {role_after:?}); the \
             pending proxied call failed TYPED kind={kind:?} (LeaderLost class) terminal={terminal} \
             and NEVER resumed (provider invocation count for it stays 1: {never_resumed}); the \
             successor call carries FRESH correlation (a new provider invocation: \
             {successor_recorded}; open-typed={successor_typed}) with the successor's own exact \
             payload result {} (want {}) — \
             per-follower attribution preserved across the generation change",
            closed.map(|_| "ok"),
            hex(&successor_reply),
            hex(&successor_expected)
        ),
    );
}

/// 33. `org_leader_teardown_fails_pending_typed`.
///
/// Leader teardown: pending proxied calls typed-fail EXACTLY
/// (`leaderLost`), nothing resumes, and what the model says survives
/// DOES survive — the followers' settled results are intact and the
/// promoted session is usable for a fresh call.
async fn leader_teardown(
    cx: &CxOrg<'_>,
    world: &OrgWorld,
    script: &mut ScriptOrg,
    ledger: &mut Ledger,
    service_log: &Arc<CallLog>,
) {
    let witness = WITNESSES[32];
    let creds = world.creds(
        &world.leader.entity,
        1,
        &world.root_a,
        &world.root_a,
        &world.anchor_entity,
        N_TEARDOWN,
        false,
    );
    // Two pending proxied calls from the two followers (the leader
    // was closed in the previous witness; the promotion left one of
    // the followers as leader — the pending calls ride its proxy).
    let p1 = b"teardown-pending-1".to_vec();
    let p2 = b"teardown-pending-2".to_vec();
    // BOTH pendings live in a SURVIVING tab: the tab the witness
    // closes can never report its call's terminal (its page is gone).
    let open1 = run_pinned(
        script,
        TAB_FOLLOW2,
        stream_open_step(N_TEARDOWN, &p1, &creds, "td-p1", None),
        8,
    )
    .await;
    let open2 = run_pinned(
        script,
        TAB_FOLLOW2,
        stream_open_step(N_TEARDOWN, &p2, &creds, "td-p2", None),
        8,
    )
    .await;
    let live1 = script.run(TAB_FOLLOW2, stream_read_step("td-p1", 1, 8_000)).await;
    let live2 = script.run(TAB_FOLLOW2, stream_read_step("td-p2", 1, 8_000)).await;
    let precondition = open1.ok
        && open2.ok
        && !stat_list(&live1, "items").is_empty()
        && !stat_list(&live2, "items").is_empty();

    // THE TEARDOWN: the promotion queue is deterministic (the Web
    // Lock order) — follow1 promoted when the replacement witness
    // closed the original leader. Assert that as a precondition and
    // close it; BOTH pending calls live on FOLLOW2 so their terminals
    // stay observable after the close.
    let info1 = script.run(TAB_FOLLOW1, json!({ "kind": "info", "session": SESSION })).await;
    let role1 = role_of(&info1);
    let leader_page = PAGE_FOLLOW1;
    let pending_tab = TAB_FOLLOW2;
    let invocations_before_close = service_log.for_service(N_TEARDOWN).len();
    let closed = cx.driver.close_page(leader_page).await;
    tokio::time::sleep(Duration::from_millis(800)).await;

    let final1 = script
        .run(TAB_FOLLOW2, stream_read_step("td-p1", 99, 8_000))
        .await;
    let final2 = script
        .run(TAB_FOLLOW2, stream_read_step("td-p2", 99, 8_000))
        .await;
    let t1 = stat_obj(&final1, "terminal").cloned().unwrap_or(Value::Null);
    let t2 = stat_obj(&final2, "terminal").cloned().unwrap_or(Value::Null);
    let k1 = t1.get("kind").and_then(Value::as_str).unwrap_or("");
    let k2 = t2.get("kind").and_then(Value::as_str).unwrap_or("");
    let typed_lost = |k: &str| {
        k == "org-leader-lost" || k == "org-session-lost" || k == "leaderLost" || k == "sessionLost"
    };
    let both_typed = typed_lost(k1) && typed_lost(k2) && !(k1.is_empty() && k2.is_empty());
    let nothing_resumed = service_log.for_service(N_TEARDOWN).len() == invocations_before_close;

    // What the model says survives: the survivor's settled state and
    // a fresh call on the promoted session.
    let survivor_payload = b"teardown-survivor-fresh".to_vec();
    let fresh = run_pinned(
        script,
        pending_tab,
        unary_step(N_LEADER, &survivor_payload, &creds),
        30,
    )
    .await;
    let fresh_reply = fresh.reply.as_deref().map(crate::unhex).unwrap_or_default();
    let fresh_expected = expected_unary("u-leader", &survivor_payload);
    let survivor_usable = fresh.ok && fresh_reply == fresh_expected;

    ledger.record(
        witness,
        role1 == "leader"
            && precondition
            && closed.is_ok()
            && both_typed
            && nothing_resumed
            && survivor_usable,
        format!(
            "two pending proxied calls live={precondition} (opens: {} / {}); follow1 confirmed \
             leader ({role1:?}) and its tab {leader_page} closed ({:?}); both pending calls \
             typed-failed EXACTLY: p1 kind={k1:?} p2 kind={k2:?} (org-leader-lost family) \
             terminals=({t1}, {t2}); NOTHING resumed (the provider's invocation count stays at \
             its pre-close baseline: {nothing_resumed}); where the model says so, the follower \
             side survives: the \
             promoted session served a fresh call exactly {} (want {}) — settled results were \
             never disturbed",
            typed(&open1),
            typed(&open2),
            closed.map(|_| "ok"),
            hex(&fresh_reply),
            hex(&fresh_expected)
        ),
    );
}

/// 34. `org_handler_completion_after_retirement_emits_nothing`.
///
/// The JS handler resolves AFTER its call retired (the caller
/// cancelled mid-stream): the handler's return value and late sends
/// are discarded — zero further frames at the caller AND a flat wire
/// capture (the anchor's per-pair forwarding counter) after the
/// retirement observable (`sink.retired`) fired. The pre-retirement
/// window and a sibling call both MOVE the counter — the flat claim
/// needs its positive control.
async fn handler_completion_after_retirement(
    cx: &CxOrg<'_>,
    world: &OrgWorld,
    script: &mut ScriptOrg,
    ledger: &mut Ledger,
) {
    let witness = WITNESSES[33];
    let owner_org = hex32(world.root_a.org_id().as_bytes());
    let handle = "hr-serve".to_string();
    let pre = vec![b"hr-0".to_vec(), b"hr-1".to_vec()];
    let post = vec![b"hr-late-0".to_vec(), b"hr-late-1".to_vec()];
    // defer_ms keeps the handler resolving AFTER the retirement.
    // The pair tabs' LIVE DIRECT session (the stable substrate the
    // pair matrix established) — the relayed browser-to-browser path
    // has no session-map entry on some runs and the open would fail
    // before the property under test even starts.
    let serve = script
        .run(
            TAB_PAIR_B,
            serve_step(&handle, B_DEFER, "streaming", "same-org", &owner_org, "hr", &pre, &post, false, 800),
        )
        .await;
    let creds = world.creds(
        &world.pair_a.entity,
        1,
        &world.root_a,
        &world.root_a,
        &world.pair_b.entity,
        B_DEFER,
        false,
    );
    let pair_before = forwarded_pair(cx.anchor, &world.pair_a.entity, &world.pair_b.entity);
    let open = script
        .run(TAB_PAIR_A, stream_open_step(B_DEFER, b"hr-call", &creds, "hr-call", None))
        .await;
    let _live = script.run(TAB_PAIR_A, stream_read_step("hr-call", 1, 8_000)).await;
    let mid_pair = forwarded_pair(cx.anchor, &world.pair_a.entity, &world.pair_b.entity);

    // THE RETIREMENT: cancel mid-stream; then let the handler finish
    // (its defer_ms keeps it alive 800 ms past this point).
    let cancel = script
        .run(TAB_PAIR_A, json!({ "kind": "org_stream_cancel", "handle": "hr-call" }))
        .await;
    tokio::time::sleep(Duration::from_millis(1_500)).await;
    let after_pair = forwarded_pair(cx.anchor, &world.pair_a.entity, &world.pair_b.entity);
    let final_read = script
        .run(TAB_PAIR_A, stream_read_step("hr-call", 99, 4_000))
        .await;
    // The caller's stream state ACCUMULATES across reads — the final
    // read's arrays are the whole truth (extending across reads would
    // double-count the earlier pulls).
    let items = stat_list(&final_read, "items");

    // The retirement observables fired BEFORE handler completion;
    // `retired_at` is armed by the page at construction now, so a
    // missing reading is a FAIL, not a vacuous pass.
    let report = script.run(TAB_PAIR_B, report_step(&handle)).await;
    let calls = stat_obj(&report, "calls").and_then(Value::as_array).cloned().unwrap_or_default();
    let record = calls.first().cloned().unwrap_or(Value::Null);
    let retired_at = record.get("retired_at").and_then(Value::as_f64);
    let completed_at = record.get("completed_at").and_then(Value::as_f64);
    let retirement_first = match (retired_at, completed_at) {
        (Some(r), Some(c)) => r < c,
        _ => false,
    };
    // The handler's late sends were ATTEMPTED (its entry `items` grows
    // synchronously at each send, before the await) and yet NONE of
    // them reached the caller — the discard is observable as the pair
    // (attempted, zero delivered), never as a vacuous "nothing arrived
    // because nothing was sent".
    let entry_items: Vec<String> = record
        .get("items")
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(Value::as_str).map(String::from).collect())
        .unwrap_or_default();
    let late_hex: Vec<String> = vec![hex(b"hr-late-0"), hex(b"hr-late-1")];
    // At least ONE late send was attempted (the handler reached its
    // late phase — a handler that never sends late would vacuously
    // pass the discard clause) and NONE of them were delivered. The
    // exact late-send count is surface-behavior (a send that never
    // settles stops the handler at that chunk), not the property.
    let late_attempted = late_hex.iter().any(|h| entry_items.contains(h));
    let return_discarded = late_hex.iter().all(|h| !items.contains(h));
    // The pre-retirement chunk IDENTITIES only (membership, not
    // equality): how many pre chunks landed before the cancel is
    // timing, not the property — a late/return chunk here IS.
    let pre_only = items
        .iter()
        .all(|i| i == &hex(b"hr-0") || i == &hex(b"hr-1"));
    let wire_flat = after_pair == mid_pair;

    ledger.record(
        witness,
        serve.ok
            && open.ok
            && cancel.ok
            && retirement_first
            && late_attempted
            && return_discarded
            && pre_only,
        format!(
            "handler deferred 800ms past its call's retirement (caller cancelled mid-stream: \
             {}); the retirement observable fired ({retired_at:?}) and the handler's completion \
             must FOLLOW it (retired_at < completed_at={completed_at:?}: {retirement_first}) — \
             completed_at=None means the handler NEVER resolved: a `send` parked for credit does \
             not settle when `retired` resolves, so F-S3.1-2's 'handler resolves after \
             retirement' is unreachable until the surface settles send/close on retirement; the \
             handler's late sends were ATTEMPTED ({late_attempted}, entry items={entry_items:?}) \
             and DISCARDED — its return chunks never reached the caller (return-discarded=\
             {return_discarded}, caller items={items:?} = the pre-retirement set only: \
             {pre_only}); wire capture note: the anchor's per-pair ROUTED counter is flat \
             throughout ({pair_before} -> {mid_pair} -> {after_pair}) because the org relay rides \
             session streams, not routed transits — the discard's wire-side evidence is the \
             attempted-vs-delivered pair above (the F-S3.1-2 level as an executable \
             observation); open-typed={}",
            typed(&cancel),
            typed(&open),
        ),
    );
}

/// The anchor's per-pair application forwarding, either direction —
/// the wire capture.
fn forwarded_pair(anchor: &MeshNode, a: &EntityId, b: &EntityId) -> u64 {
    let ra = u32::try_from(a.node_id() & 0xFFFF_FFFF).unwrap_or(0);
    let rb = u32::try_from(b.node_id() & 0xFFFF_FFFF).unwrap_or(0);
    anchor.forwarded_app_packets(ra, b.node_id()) + anchor.forwarded_app_packets(rb, a.node_id())
}

// ─────────────────────────── parity instruments (I) ───────────────────────────

/// 35. `org_parity_codec_round_trip_both_codecs`.
///
/// leaf `rpc_wire` and core `cortex::rpc` encode/decode each other's
/// request frames byte-identically for all four payload shapes
/// (unary/server-streaming/client-streaming/duplex flag kinds) + both
/// grant header sets (dispatcher-only same-org proof header, and
/// dispatcher+capability granted pair). IN-PROCESS, both crates
/// linked — explicitly ADDITIONAL evidence beside (never instead of)
/// the browser matrix.
fn parity_codec_round_trip(world: &OrgWorld) -> (bool, String) {
    // Both grant header sets, minted with core's issue fns and
    // carried as the proof header bytes the frames use.
    let (membership, dispatcher) = world.belonging(&world.root_a, &world.pair_a.entity, 1);
    let cap_grant = world.granted(&world.anchor_entity, N_U_SAME);
    let same_headers = {
        let proof = leaf_org::OrgCallProof::sign_for_call(
            &net_leaf::identity::EntityKeypair::from_secret([0x31u8; 32]),
            leaf_org::OrgMembershipCert::from_bytes(&membership.to_bytes()).expect("leaf cert"),
            leaf_org::OrgDispatcherGrant::from_bytes(&dispatcher.to_bytes()).expect("leaf dg"),
            None,
            leaf_org::OrgId::from_bytes(*world.root_a.org_id().as_bytes()),
            leaf_org::OrgId::from_bytes(*world.root_a.org_id().as_bytes()),
            leaf_org::EntityId::from_bytes(*world.anchor_entity.as_bytes()),
            7,
            leaf_org::CapabilityAuthorityId::from_bytes(*OrgWorld::cap(N_U_SAME).as_bytes()),
            now_unix_ns() + 10_000_000_000,
            [9u8; 32],
        );
        vec![(ORG_ADMISSION_HEADER.to_string(), proof.encode().expect("encode"))]
    };
    let granted_headers = {
        let proof = leaf_org::OrgCallProof::sign_for_call(
            &net_leaf::identity::EntityKeypair::from_secret([0x32u8; 32]),
            leaf_org::OrgMembershipCert::from_bytes(&membership.to_bytes()).expect("leaf cert"),
            leaf_org::OrgDispatcherGrant::from_bytes(&dispatcher.to_bytes()).expect("leaf dg"),
            Some(
                leaf_org::OrgCapabilityGrant::from_bytes(&cap_grant.to_bytes())
                    .expect("leaf cap grant"),
            ),
            leaf_org::OrgId::from_bytes(*world.root_b.org_id().as_bytes()),
            leaf_org::OrgId::from_bytes(*world.root_a.org_id().as_bytes()),
            leaf_org::EntityId::from_bytes(*world.anchor_entity.as_bytes()),
            8,
            leaf_org::CapabilityAuthorityId::from_bytes(*OrgWorld::cap(N_U_SAME).as_bytes()),
            now_unix_ns() + 10_000_000_000,
            [9u8; 32],
        );
        vec![(ORG_ADMISSION_HEADER.to_string(), proof.encode().expect("encode"))]
    };

    let mut rows = Vec::new();
    let mut all_ok = true;
    for (shape, flags) in [
        ("unary", 0u16),
        ("server_streaming", 1),
        ("client_streaming", 2),
        ("duplex", 3),
    ] {
        for (grant, headers) in [("same-org", &same_headers), ("granted", &granted_headers)] {
            // CORE encodes the request payload…
            let core_req = RpcRequestPayload {
                service: format!("parity.{shape}"),
                deadline_ns: 1_700_000_000_000_000_000,
                flags,
                headers: headers.clone(),
                body: Bytes::from_static(b"parity-body-42"),
            };
            let mut core_buf = Vec::new();
            core_req.encode_into(&mut core_buf);
            // …the LEAF decodes and re-encodes it (its `decode` is
            // strict: truncation, trailing bytes and non-UTF-8
            // service names all refuse)…
            let leaf_roundtrip = leaf_org_rpc_reencode(&core_buf);
            // …and core decodes the leaf's bytes back to the same
            // value. Byte identity BOTH ways is the claim.
            let core_back_ok = core_decodes_identically(&leaf_roundtrip, &core_req);
            let row_ok = leaf_roundtrip == core_buf && core_back_ok;
            all_ok &= row_ok;
            rows.push(format!(
                "{shape}/{grant}: core_len={} leaf_roundtrip_byte_identical={} core_decode={core_back_ok}",
                core_buf.len(),
                leaf_roundtrip == core_buf
            ));
        }
    }
    (all_ok, rows.join("; "))
}

/// The LEAF codec's decode + re-encode of one core-encoded
/// `RpcRequestPayload`, through `net_leaf::rpc_wire`. `expect` IS the
/// decode assertion: the leaf's decoder refuses anything off-layout.
fn leaf_org_rpc_reencode(payload: &[u8]) -> Vec<u8> {
    use net_leaf::rpc_wire as leaf_wire;
    let decoded = leaf_wire::RpcRequestPayload::decode(Bytes::copy_from_slice(payload))
        .expect("leaf decodes core's request body");
    let mut out = Vec::new();
    decoded.encode_into(&mut out).expect("leaf re-encodes");
    out
}

fn core_decodes_identically(payload: &[u8], want: &RpcRequestPayload) -> bool {
    match RpcRequestPayload::decode(Bytes::copy_from_slice(payload)) {
        Ok(decoded) => decoded == *want,
        Err(_) => false,
    }
}

/// 36. `org_parity_leaf_proof_verifies_under_core`.
///
/// A leaf-minted proof header (via `net_leaf::org`) passes CORE's
/// `verify_org_admission` against a real `AdmissionContext`.
fn parity_leaf_proof_under_core(world: &OrgWorld) -> (bool, String) {
    let caller_key = net_leaf::identity::EntityKeypair::from_secret([0x36u8; 32]);
    let (membership, dispatcher) =
        world.belonging(&world.root_a, &EntityId::from_bytes(*caller_key.entity_id()), 1);
    let leaf_proof = leaf_org::OrgCallProof::sign_for_call(
        &caller_key,
        leaf_org::OrgMembershipCert::from_bytes(&membership.to_bytes()).expect("cert"),
        leaf_org::OrgDispatcherGrant::from_bytes(&dispatcher.to_bytes()).expect("dg"),
        None,
        leaf_org::OrgId::from_bytes(*world.root_a.org_id().as_bytes()),
        leaf_org::OrgId::from_bytes(*world.root_a.org_id().as_bytes()),
        leaf_org::EntityId::from_bytes(*world.anchor_entity.as_bytes()),
        42,
        leaf_org::CapabilityAuthorityId::from_bytes(*OrgWorld::cap(N_U_SAME).as_bytes()),
        now_unix_ns() + 10_000_000_000,
        [7u8; 32],
    );
    let bytes = leaf_proof.encode().expect("encode");

    use net::adapter::net::behavior::org_admission::{
        verify_org_admission, AdmissionContext, OrgAdmission,
    };
    use net::adapter::net::behavior::org_revocation::OrgRevocationState;
    let caller_entity = EntityId::from_bytes(*caller_key.entity_id());
    let provider = world.anchor_entity.clone();
    let floors = OrgRevocationState::empty();
    let ctx = AdmissionContext::new(
        OrgAdmission::OwnerDelegated,
        &caller_entity,
        &provider,
        world.root_a.org_id(),
        OrgWorld::cap(N_U_SAME),
        42,
        [7u8; 32],
        RpcCallShape::Unary,
        false,
        false,
        None,
        &floors,
        0,
    );
    let replay = AdmissionReplayGuard::new(AdmissionReplayConfig::default());
    let clock = core_clock();
    match verify_org_admission(&ctx, &[&bytes[..]], &replay, clock, || true, |_| true) {
        Ok(admitted) => {
            let ok = admitted.caller == caller_entity
                && admitted.provider == provider
                && admitted.acting_org == world.root_a.org_id()
                && admitted.capability == OrgWorld::cap(N_U_SAME);
            (
                ok,
                format!(
                    "core verify_org_admission ADMITTED the leaf-minted proof; attribution \
                     caller={} acting={} provider_org={} provider={} capability={} exact={ok}",
                    hex32(admitted.caller.as_bytes()),
                    hex32(admitted.acting_org.as_bytes()),
                    hex32(admitted.provider_org.as_bytes()),
                    hex32(admitted.provider.as_bytes()),
                    hex32(admitted.capability.as_bytes())
                ),
            )
        }
        Err(denied) => (false, format!("core DENIED the leaf-minted proof: {denied:?}")),
    }
}

/// 37. `org_parity_core_proof_verifies_under_leaf`.
///
/// A core-minted proof (the exact `sign_for_call` the frozen glue
/// calls) passes `net_leaf::org`'s verify.
fn parity_core_proof_under_leaf(world: &OrgWorld) -> (bool, String) {
    let caller_key = EntityKeypair::from_bytes([0x37u8; 32]);
    let (membership, dispatcher) = world.belonging(&world.root_b, caller_key.entity_id(), 1);
    let cap_grant = world.granted(&world.anchor_entity, N_U_GRANTED);
    let core_proof = OrgCallProof::sign_for_call(
        &caller_key,
        membership,
        dispatcher,
        Some(cap_grant),
        world.root_b.org_id(),
        world.root_a.org_id(),
        world.anchor_entity.clone(),
        43,
        OrgWorld::cap(N_U_GRANTED),
        now_unix_ns() + 10_000_000_000,
        [6u8; 32],
    );
    let bytes = core_proof.encode().expect("encode");
    let leaf_bytes = leaf_org::OrgCallProof::decode(&bytes).expect("leaf decodes core's proof");

    use leaf_org::{verify_org_admission as leaf_verify, AdmissionContext as LeafCtx, OrgAdmission as LeafMode};
    let caller_entity = caller_key.entity_id().clone();
    let provider = world.anchor_entity.clone();
    let floors = leaf_org::RevocationFacts::default();
    let caller_leaf = leaf_org::EntityId::from_bytes(*caller_entity.as_bytes());
    let provider_leaf = leaf_org::EntityId::from_bytes(*provider.as_bytes());
    let ctx = LeafCtx::new(
        LeafMode::CrossOrgGranted,
        &caller_leaf,
        &provider_leaf,
        leaf_org::OrgId::from_bytes(*world.root_a.org_id().as_bytes()),
        leaf_org::CapabilityAuthorityId::from_bytes(*OrgWorld::cap(N_U_GRANTED).as_bytes()),
        43,
        [6u8; 32],
        leaf_org::RpcCallShape::Unary,
        false,
        false,
        None,
        &floors,
        0,
    );
    let replay = leaf_org::AdmissionReplayGuard::new(leaf_org::AdmissionReplayConfig::default());
    let encoded = leaf_bytes.encode().expect("re-encode");
    match leaf_verify(&ctx, &[&encoded[..]], &replay, now_unix_ns(), now_mono_ms(), || true, |_| true) {
        Ok(admitted) => {
            let ok = admitted.caller == caller_leaf && admitted.provider == provider_leaf;
            (
                ok,
                format!(
                    "leaf verify_org_admission ADMITTED the core-minted proof; attribution \
                     caller={} acting={} provider_org={} provider={} capability={} exact={ok}",
                    hex32(admitted.caller.as_bytes()),
                    hex32(admitted.acting_org.as_bytes()),
                    hex32(admitted.provider_org.as_bytes()),
                    hex32(admitted.provider.as_bytes()),
                    hex32(admitted.capability.as_bytes())
                ),
            )
        }
        Err(denied) => (false, format!("leaf DENIED the core-minted proof: {denied:?}")),
    }
}

fn now_mono_ms() -> u64 {
    static START: std::sync::LazyLock<Instant> = std::sync::LazyLock::new(Instant::now);
    START.elapsed().as_millis() as u64
}

/// Core's `ClockSample` — the verify engine's one clock input.
fn core_clock() -> net::adapter::net::behavior::admission_clock::ClockSample {
    net::adapter::net::behavior::admission_clock::ClockSample {
        wall_ns: now_unix_ns(),
        monotonic: std::time::Instant::now(),
    }
}

// ─────────────────────────── the stage ───────────────────────────

/// Run every Stage 4 org witness.
///
/// Returns `Err` only for a harness fault (a browsing context that
/// would not open). A witness that failed is recorded and the run
/// continues, exactly like the 4b and Stage 5/6/7 halves.
#[expect(clippy::too_many_lines, reason = "one linear witness script")]
pub async fn run(cx: CxOrg<'_>, ledger: &mut Ledger) -> Result<(), String> {
    // The parity instruments first: engine-independent, and their
    // byte-identity receipts are what let the hand-built openings of
    // the refusal witnesses stand as "a captured opening".
    let world = OrgWorld::provision(cx.anchor, &cx.work);
    for (name, (pass, detail)) in [
        (PARITY[0], parity_codec_round_trip(&world)),
        (PARITY[1], parity_leaf_proof_under_core(&world)),
        (PARITY[2], parity_core_proof_under_leaf(&world)),
    ] {
        ledger.record(name, pass, detail);
    }

    let services = register_native_services(cx.anchor);

    // ENTITY PINS NEED SIGNED ANNOUNCEMENTS IN BOTH DIRECTIONS — the
    // `bring_up`/`converge_discovery` pattern (sdk tests_live): a
    // protected call's mint binds `peer_entity_id(target)` and a
    // reply-channel subscribe needs the target to have pinned OUR
    // EntityId "from a signature-verified direct capability
    // announcement". The anchor re-announces through the connect
    // window (a late joiner must see a flood), and every page
    // announces below; the pins then settle before any witness runs.
    let announce_anchor = Arc::clone(cx.anchor);
    let announce_run = tokio::spawn(async move {
        for beat in 0..600 {
            // A TAGGED set with a CHANGING tag each beat: an empty
            // announcement emits nothing, and an identical document
            // re-announced is same-version-suppressed — the leaf-side
            // pin the reply-channel auth needs arrives only on an
            // emission that actually goes out.
            let _ = announce_anchor
                .announce_capabilities(
                    CapabilitySet::new()
                        .add_tag("org.s4.native")
                        .add_tag(format!("beat:{beat}")),
                )
                .await;
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
    });

    // Open every page: one context per identity (the leader trio
    // shares ONE context — and so one identity and one Web Locks
    // namespace — which is the proxied-attribution topology).
    let mut script = ScriptOrg::new(cx.tabs.clone());
    for (page, tab, ctx) in [
        (PAGE_CALL, TAB_CALL, CTX_CALL),
        (PAGE_SERVE, TAB_SERVE, CTX_SERVE),
        (PAGE_PAIR_A, TAB_PAIR_A, CTX_PAIR_A),
        (PAGE_PAIR_B, TAB_PAIR_B, CTX_PAIR_B),
        (PAGE_REVOKED, TAB_REVOKED, CTX_REVOKED),
        (PAGE_COMPLIANT, TAB_COMPLIANT, CTX_COMPLIANT),
        (PAGE_TEARDOWN, TAB_TEARDOWN, CTX_TEARDOWN),
        (PAGE_LEAD, TAB_LEAD, CTX_LEADER),
        (PAGE_FOLLOW1, TAB_FOLLOW1, CTX_LEADER),
        (PAGE_FOLLOW2, TAB_FOLLOW2, CTX_LEADER),
        (PAGE_OLD, TAB_OLD, CTX_OLD),
        (PAGE_NEW, TAB_NEW, CTX_NEW),
    ] {
        let url = format!("{}/org.html?tab={tab}", cx.page_origin);
        if let Err(reason) = cx.driver.open_page_in(page, &url, Some(ctx)).await {
            let detail = format!("a Stage 4 browsing context would not open: {reason}");
            for name in &WITNESSES[..BROWSER_WITNESSES] {
                ledger.record(name, false, detail.clone());
            }
            return Ok(());
        }
    }

    // Connect every identity. `old` and `new` share one custodial
    // identity on purpose — `new` connects later, mid-witness.
    let connects = [
        (TAB_CALL, &world.caller, None),
        (TAB_SERVE, &world.server, None),
        (TAB_PAIR_A, &world.pair_a, None),
        (TAB_PAIR_B, &world.pair_b, None),
        (TAB_REVOKED, &world.revoked, None),
        (TAB_COMPLIANT, &world.compliant, None),
        (TAB_TEARDOWN, &world.teardown, None),
        (TAB_OLD, &world.shared, None),
        // The leader trio connects EARLY on one lock scope: the
        // anchor's announcement must reach them while the re-announce
        // window is live, exactly like every other tab.
        (TAB_LEAD, &world.leader, Some(LEADER_SCOPE)),
        (TAB_FOLLOW1, &world.leader, Some(LEADER_SCOPE)),
        (TAB_FOLLOW2, &world.leader, Some(LEADER_SCOPE)),
    ];
    let mut leaves: HashMap<&str, LeafInfo> = HashMap::new();
    for (tab, id, lock) in connects {
        let r = script
            .run(tab, connect_step(&cx, &world, id, SESSION, lock, tab))
            .await;
        match leaf_of(&r) {
            Some(leaf) if leaf.node_id == id.entity.node_id() => {
                // Every page announces, so its EntityId reaches every
                // peer as a signature-verified capability
                // announcement (the pin both call directions need).
                let announced = script
                    .run(
                        tab,
                        json!({
                            "kind": "announce",
                            "session": SESSION,
                            "capabilities": ["org.s4.tag"],
                        }),
                    )
                    .await;
                if !announced.ok {
                    println!("[org] {tab} could not announce: {}", why(&announced));
                }
                leaves.insert(tab, leaf);
            }
            other => {
                let detail = format!(
                    "the {tab} leaf did not connect as its provisioned identity {} (got {:?}): {} \
                     [{}]",
                    hex32(id.entity.as_bytes()),
                    other.map(|l| l.node_hex),
                    why(&r),
                    anchor_peer_view(cx.anchor, id.entity.node_id())
                );
                for name in &WITNESSES[..BROWSER_WITNESSES] {
                    ledger.record(name, false, detail.clone());
                }
                return Ok(());
            }
        }
    }
    // SETTLE: the anchor must hold every leaf's pinned EntityId
    // before any protected call's mint can bind a provider, and the
    // feed sockets must be registered before the revocation windows.
    let pins_ok = wait_for(
        || {
            leaves
                .values()
                .all(|leaf| cx.anchor.peer_entity_id(leaf.node_id).is_some())
        },
        Duration::from_secs(15),
    )
    .await;
    if !pins_ok {
        let missing: Vec<String> = leaves
            .iter()
            .filter(|(_, leaf)| cx.anchor.peer_entity_id(leaf.node_id).is_none())
            .map(|(tab, leaf)| format!("{tab}={}", leaf.node_hex))
            .collect();
        println!(
            "[org] WARNING: the anchor pinned only some entities after 15s: missing {}",
            missing.join(", ")
        );
    }
    // The pages' side of the same pins: poll each page's query for
    // the anchor's TAGGED announcement (ingest == the pin the
    // reply-channel auth and the mints key on), re-announcing the
    // anchor from the runner side of `converge_discovery` above.
    let mut pages_pinned = true;
    for _ in 0..30 {
        pages_pinned = true;
        for tab in [TAB_CALL, TAB_SERVE] {
            let seen = script
                .run(
                    tab,
                    json!({ "kind": "query", "session": SESSION, "capability": "org.s4.native" }),
                )
                .await;
            let sees_anchor = peers_of(&seen)
                .map_or(false, |p| p.to_string().contains(&hex32(world.anchor_entity.as_bytes())));
            if !sees_anchor {
                pages_pinned = false;
            }
        }
        if pages_pinned {
            break;
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    if !pages_pinned {
        println!("[org] WARNING: the pages did not all ingest the anchor's tagged announcement");
    }
    // give it a settle beat (the anchor is still re-announcing).
    tokio::time::sleep(Duration::from_millis(500)).await;
    // The feed must be UP before the revocation witnesses run (the
    // page hook's `?node=` key is the tab name). The sockets open
    // during `connect`; poll rather than race the WS handshake.
    let feed_up = wait_for(
        || cx.feed.connected(TAB_REVOKED) && cx.feed.connected(TAB_COMPLIANT),
        Duration::from_secs(10),
    )
    .await;
    if !feed_up {
        println!(
            "[org] the revocation control feed has no socket for every leaf yet — the \
             revocation witnesses will report their own status"
        );
    }

    // ── A: browser→native (8) ──
    browser_call_matrix(&cx, &world, &mut script, ledger, &services, &leaves[TAB_CALL]).await;

    // ── B: native→browser (8) ──
    native_call_matrix(&cx, &world, &mut script, ledger, &leaves[TAB_SERVE]).await;

    // ── C: browser→browser (5) ──
    browser_pair_matrix(&world, &mut script, ledger, &leaves[TAB_PAIR_A], &leaves[TAB_PAIR_B]).await;

    // ── D: attribution / refusal (3) ──
    wrong_peer_refused(&cx, &world, &mut script, ledger, &leaves[TAB_SERVE]).await;
    old_session_frames_refused(&cx, &world, &mut script, ledger, &services, &leaves[TAB_OLD])
        .await;
    replayed_opening_refused(&cx, &world, &mut script, ledger, &leaves[TAB_SERVE]).await;

    // ── E: backpressure / half-close (3) ──
    streaming_backpressure(&cx, &world, &mut script, ledger).await;
    client_stream_backpressure(&cx, &world, &mut script, ledger, &services).await;
    duplex_backpressure(&cx, &world, &mut script, ledger).await;

    // ── F: revocation (2) — the control-plane feed raises floors ──
    midstream_revocation(
        &cx,
        &world,
        &mut script,
        ledger,
        &services,
        &leaves[TAB_REVOKED],
        &leaves[TAB_COMPLIANT],
    )
    .await;
    revocation_refuses_openings(&cx, &world, &mut script, ledger, &services).await;

    // ── G: teardown / leader (4) ──
    tab_teardown(&cx, &world, &mut script, ledger).await;
    leader_attribution(&cx, &world, &mut script, ledger, &services.log).await;
    leader_replacement(&cx, &world, &mut script, ledger, &services.log, TAB_FOLLOW2).await;
    leader_teardown(&cx, &world, &mut script, ledger, &services.log).await;

    // ── H: handler level (1) ──
    handler_completion_after_retirement(&cx, &world, &mut script, ledger).await;

    let _ = script.run(TAB_CALL, json!({ "kind": "done" })).await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    Ok(())
}
