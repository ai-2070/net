//! Shared in-process two-node harness for the nRPC benchmark
//! suite. Every bench file under `benches/nrpc_*.rs` uses these
//! helpers via `#[path = "nrpc_common/mod.rs"] mod nrpc_common;`.
//!
//! Why a shared module: setting up a real `Mesh` peer pair
//! (build + handshake + start + capability announce + discovery
//! wait) is ~30 lines and identical across every bench. Doing it
//! once per file means six near-identical copies that drift out
//! of sync; once here, every bench sees the same setup cost.
//!
//! The harness intentionally lives behind `benches/nrpc_common/`
//! (as a directory) rather than `benches/nrpc_common.rs` so
//! Cargo's bench auto-discovery doesn't pick it up as a bench
//! target. `autobenches = false` in `sdk/Cargo.toml` reinforces
//! this — each bench is registered explicitly with `[[bench]]`.

#![allow(dead_code)] // each bench uses only a subset of helpers

use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use net::adapter::net::behavior::org_admission_replay::AdmissionReplayConfig;
use net::adapter::net::{ChannelConfigRegistry, MeshNode, MeshNodeConfig};
use net_sdk::capabilities::CapabilitySet;
use net_sdk::identity::EntityKeypair;
use net_sdk::mesh::{Mesh, MeshBuilder};
use net_sdk::mesh_rpc::{
    CallOptions, CallOptionsTyped, Codec, RpcContext, RpcHandler, RpcHandlerError,
    RpcResponsePayload, RpcStatus,
};
use net_sdk::org::{
    CapabilityAuthorityId, DispatcherScope, NodeAuthority, OrgAdmission, OrgDispatcherGrant,
    OrgKeypair, OrgMembershipCert, OrgProofIntent,
};
use serde::{Deserialize, Serialize};
use tokio::runtime::{Builder as RtBuilder, Runtime};

// ============================================================================
// Service names. Each codec gets its own service so the same
// `Pair` can serve all three side by side and a bench can pick
// whichever it's measuring without re-registering.
// ============================================================================

pub const SVC_JSON: &str = "bench_echo_json";
/// Prefix for the per-shard JSON echo services registered by
/// [`Pair::new_sharded`]. Shard `i` is served as
/// `"{SVC_JSON_SHARD_PREFIX}{i}"`. Used by `nrpc_qps_shard.rs` to
/// spread load across N independent channels (each gets its own
/// bridge task + fold mutex).
pub const SVC_JSON_SHARD_PREFIX: &str = "bench_echo_json_shard_";
pub const SVC_POSTCARD: &str = "bench_echo_postcard";
pub const SVC_RAW: &str = "bench_echo_raw";
pub const SVC_JSON_STREAM: &str = "bench_stream_json";
pub const SVC_JSON_CLIENT_STREAM: &str = "bench_client_stream_json";
pub const SVC_JSON_DUPLEX: &str = "bench_duplex_json";
/// The PROTECTED (org-admitted) unary echo service registered by
/// [`Pair::protected`]. Raw bytes in / raw bytes out — the same
/// [`RawEchoHandler`] the public [`SVC_RAW`] service uses, so the
/// difference between the two bars is admission, not codec.
pub const SVC_PROTECTED_RAW: &str = "bench_echo_protected_raw";

// ============================================================================
// Echo wire types — the same logical `String` body across all
// three codecs so the comparison reflects codec cost, not payload
// shape. ASCII content keeps JSON honest (no Vec<u8> → JSON-array
// blow-up) while still letting postcard / raw deliver the same
// bytes.
// ============================================================================

#[derive(Serialize, Deserialize, Clone)]
pub struct EchoReq {
    pub body: String,
}

#[derive(Serialize, Deserialize, Clone)]
pub struct EchoResp {
    pub body: String,
}

/// One ASCII byte ('A') repeated `n` times. Cheap to allocate,
/// stable across runs, no random-source overhead during the bench
/// loop.
pub fn payload(n: usize) -> String {
    "A".repeat(n)
}

/// Hard ceiling on how long a single `call_*_retrying` will spin on
/// transport backpressure before giving up. The retry loop yields on
/// `RpcError::Transport(_)` so other in-flight callers can flush; under a
/// *sustainable* offered load the queue drains in well under a second.
/// Without a ceiling, an offered concurrency the transport CANNOT sustain
/// (e.g. c128 here) makes the call spin forever — the bar never converges
/// and Criterion runs it for minutes (long enough to trip unrelated TTLs).
/// 20 s converts that livelock into a fast, clearly-labeled failure.
const RETRY_DEADLINE: std::time::Duration = std::time::Duration::from_secs(20);

/// Yield on transport backpressure, but panic once [`RETRY_DEADLINE`] has
/// elapsed since `deadline_base` — so a saturated bar fails fast and
/// legibly instead of livelocking. `ctx` names the call site.
async fn backpressure_yield(deadline_base: std::time::Instant, ctx: &str) {
    if deadline_base.elapsed() >= RETRY_DEADLINE {
        panic!(
            "{ctx}: transport saturated — no progress within {RETRY_DEADLINE:?} of \
             backpressure retries. The offered concurrency exceeds what this transport \
             sustains on this host; this bar is not measurable here (don't chase it)."
        );
    }
    tokio::task::yield_now().await;
}

// ============================================================================
// Pair — two `Mesh` nodes in one process, fully handshaken,
// every echo service pre-registered, discovery primed.
// ============================================================================

pub struct Pair {
    pub server: Mesh,
    pub caller: Mesh,
    pub server_node_id: u64,
    /// Service names registered by [`Pair::new_sharded`], in shard
    /// order. Empty for a [`Pair::new`] pair. [`call_json_shard_retrying`]
    /// indexes into this to fan calls across channels.
    pub shard_services: Vec<String>,
    /// The owner-delegated intent [`Pair::protected`] minted, which a
    /// protected call installs on [`CallOptions::org_proof_intent`].
    /// `None` for a public pair ([`Pair::new`] / [`Pair::new_sharded`]).
    pub org_intent: Option<OrgProofIntent>,
    // Keep ServeHandles alive for the lifetime of the Pair. The
    // RPC dispatcher unregisters on Drop, so binding to `_` would
    // tear the service down immediately (see nrpc_echo.rs:98).
    _handles: Vec<net_sdk::mesh_rpc::ServeHandle>,
}

/// Build two `Mesh` nodes on `127.0.0.1:0`, handshake them
/// (concurrent accept + connect), and start both. Shared by
/// [`Pair::new`] and [`Pair::new_sharded`] so the ~25-line build +
/// handshake dance lives in one place. Returns
/// `(server, caller, server_node_id)`.
async fn build_handshaken_pair() -> (Mesh, Mesh, u64) {
    let psk = [0x42u8; 32];
    let server = MeshBuilder::new("127.0.0.1:0", &psk)
        .expect("builder")
        .build()
        .await
        .expect("server build");
    let caller = MeshBuilder::new("127.0.0.1:0", &psk)
        .expect("builder")
        .build()
        .await
        .expect("caller build");

    let server_id = server.node_id();
    handshake_and_start(&server, &caller).await;
    (server, caller, server_id)
}

/// The handshake half of [`build_handshaken_pair`]: concurrent accept +
/// connect, then start both nodes. Extracted so [`Pair::protected`] —
/// whose nodes are built by hand (see [`build_bench_mesh`]) — runs the
/// exact same dance in the exact same order.
async fn handshake_and_start(server: &Mesh, caller: &Mesh) {
    let server_addr = server.local_addr().to_string();
    let server_pub = *server.public_key();
    let server_id = server.node_id();
    let caller_id = caller.node_id();

    // Concurrent accept + connect — matches nrpc_echo.rs:81.
    let (accept_res, connect_res) = tokio::join!(server.accept(caller_id), async {
        tokio::time::sleep(Duration::from_millis(50)).await;
        caller.connect(&server_addr, &server_pub, server_id).await
    });
    accept_res.expect("accept");
    connect_res.expect("connect");
    server.start();
    caller.start();
}

/// Per-caller admission-replay ceiling for the protected provider.
///
/// The provider retains one `(caller, call_id)` guard entry per ADMITTED
/// call until the proof's expiry PLUS the hard `MAX_TOKEN_CLOCK_SKEW_SECS`
/// ceiling — 300 s past expiry, deliberately, so a runtime skew widening
/// cannot reopen a used proof (`behavior/org_admission.rs:605-614`).
/// Nothing an in-process bench admits ever expires during the run, so the
/// guard grows monotonically with the call count and the SHIPPED per-caller
/// ceiling (`DEFAULT_MAX_REPLAY_ENTRIES_PER_CALLER` = 4096) denies call
/// 4097 with `AdmissionDenied`.
///
/// That bound is real production behavior, not a bench artifact — one
/// caller identity gets 4096 protected calls per ~5 min against one
/// provider — but it is a hard stop for a Criterion loop that issues tens
/// of thousands. Raising it here keeps the measurement on the admission
/// path instead of on the guard filling up. Entries are allocated on
/// demand (`ReplayState::default()` pre-allocates nothing), so a ceiling
/// this size costs nothing until the calls actually happen.
const BENCH_REPLAY_ENTRIES_PER_CALLER: usize = 8_000_000;

/// The protected provider's replay ceilings. `max_entries_per_caller` must
/// be STRICTLY below `max_entries`, and `owner_reserved_entries` strictly
/// below it too (`AdmissionReplayConfig::validate`); the external quota is
/// left at the shipped default because this pair has no external org.
fn bench_replay_config() -> AdmissionReplayConfig {
    AdmissionReplayConfig {
        max_entries: BENCH_REPLAY_ENTRIES_PER_CALLER * 2,
        max_entries_per_caller: BENCH_REPLAY_ENTRIES_PER_CALLER,
        owner_reserved_entries: BENCH_REPLAY_ENTRIES_PER_CALLER,
        ..AdmissionReplayConfig::default()
    }
}

/// One `Mesh` around a hand-built `MeshNodeConfig`.
///
/// `MeshBuilder` exposes no seam for the admission-replay ceilings (see
/// [`BENCH_REPLAY_ENTRIES_PER_CALLER`]) and none for pinning a known
/// `EntityKeypair`, both of which [`Pair::protected`] needs — the caller's
/// node identity must BE the proof subject. So the protected pair goes
/// through `MeshNode::new` + `Mesh::from_node_arc`, the same public seam
/// the SDK's own live org tests use (`sdk/src/org/tests_live.rs:176-222`).
/// Every other knob is copied from `MeshBuilder::build`
/// (`sdk/src/mesh.rs:409-413`), so a protected node differs from a
/// [`Pair::new`] node only in those ceilings.
async fn build_bench_mesh(keypair: EntityKeypair, replay: AdmissionReplayConfig) -> Mesh {
    let addr = "127.0.0.1:0".parse().expect("bench bind addr");
    let config = MeshNodeConfig::new(addr, [0x42u8; 32])
        .with_heartbeat_interval(Duration::from_secs(5))
        .with_session_timeout(Duration::from_secs(30))
        .with_num_shards(4)
        .with_handshake(3, Duration::from_secs(5))
        .with_admission_replay_config(replay);
    let mut node = MeshNode::new(keypair, config)
        .await
        .expect("protected node build");
    let channel_configs = Arc::new(ChannelConfigRegistry::new());
    node.set_channel_configs(channel_configs.clone());
    Mesh::from_node_arc(Arc::new(node), channel_configs, None)
}

impl Pair {
    /// Build two `Mesh` instances on `127.0.0.1:0`, handshake
    /// them, register the three echo services, announce
    /// capabilities, and wait for the caller's capability index
    /// to learn about the JSON service (sentinel — all three
    /// land in the same announce).
    pub async fn new() -> Self {
        let (server, caller, server_id) = build_handshaken_pair().await;

        // Three unary echo services — one per codec.
        let h_json = server
            .serve_rpc_typed(SVC_JSON, Codec::Json, |req: EchoReq| async move {
                Ok::<_, String>(EchoResp { body: req.body })
            })
            .expect("serve json");

        let h_post = server
            .serve_rpc(SVC_POSTCARD, Arc::new(PostcardEchoHandler))
            .expect("serve postcard");

        let h_raw = server
            .serve_rpc(SVC_RAW, Arc::new(RawEchoHandler))
            .expect("serve raw");

        // Server-streaming echo — emits the same body N times,
        // N from the request. Used by `nrpc_streaming.rs`.
        let h_stream = server
            .serve_rpc_streaming_typed(
                SVC_JSON_STREAM,
                Codec::Json,
                |req: StreamReq, sink| async move {
                    let item = EchoResp { body: req.body };
                    for _ in 0..req.count {
                        if sink.send(&item).is_err() {
                            break;
                        }
                    }
                    Ok::<_, String>(())
                },
            )
            .expect("serve stream");

        // Client-streaming echo — collects N typed requests and
        // returns a count. Used by the Phase F client-streaming
        // bench.
        let h_client_stream = server
            .serve_rpc_client_stream_typed(
                SVC_JSON_CLIENT_STREAM,
                Codec::Json,
                |mut requests: net_sdk::mesh_rpc::RequestStreamTyped<EchoReq>| async move {
                    use futures::StreamExt;
                    let mut count = 0u64;
                    while let Some(item) = requests.next().await {
                        std::hint::black_box(item.map_err(|e| format!("decode: {e}"))?);
                        count += 1;
                    }
                    Ok::<_, String>(EchoResp {
                        body: count.to_string(),
                    })
                },
            )
            .expect("serve client_stream");

        // Duplex echo — emits one Resp per inbound Req. Used by
        // the Phase F duplex bench.
        let h_duplex = server
            .serve_rpc_duplex_typed(
                SVC_JSON_DUPLEX,
                Codec::Json,
                |mut requests: net_sdk::mesh_rpc::RequestStreamTyped<EchoReq>, sink| async move {
                    use futures::StreamExt;
                    while let Some(item) = requests.next().await {
                        let item: EchoReq = item.map_err(|e| format!("decode: {e}"))?;
                        sink.send(&EchoResp { body: item.body })?;
                    }
                    Ok::<_, String>(())
                },
            )
            .expect("serve duplex");

        // Announce + wait for discovery — required for the
        // `call_service_typed` (discovery) path.
        server
            .inner()
            .announce_capabilities(CapabilitySet::new())
            .await
            .expect("announce");
        let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
        while tokio::time::Instant::now() < deadline {
            if !caller.find_service_nodes(SVC_JSON).is_empty() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert!(
            !caller.find_service_nodes(SVC_JSON).is_empty(),
            "discovery did not converge within 2s"
        );

        Self {
            server,
            caller,
            server_node_id: server_id,
            shard_services: Vec::new(),
            org_intent: None,
            _handles: vec![h_json, h_post, h_raw, h_stream, h_client_stream, h_duplex],
        }
    }

    /// Build a handshaken pair serving `shards` independent JSON echo
    /// services (`{SVC_JSON_SHARD_PREFIX}{0..shards}`), each with its
    /// own bridge task + fold mutex on the server. Used by
    /// `nrpc_qps_shard.rs` (Phase 1 of the QPS concurrency-scaling
    /// plan) to test whether spreading load across channels lifts the
    /// single-channel throughput ceiling.
    ///
    /// `shards == 1` reproduces the single-channel `nrpc_qps` setup
    /// (every call hits one bridge/mutex) and serves as the in-bench
    /// baseline. Routing is direct (`call_typed` by node id), so the
    /// announce below exists only to keep the server's channel
    /// registration on the same proven path as `new()`; no discovery
    /// wait is needed for direct calls, but we wait for shard 0 to
    /// converge as a readiness sentinel.
    pub async fn new_sharded(shards: usize) -> Self {
        assert!(shards >= 1, "shards must be >= 1");
        let (server, caller, server_id) = build_handshaken_pair().await;

        let mut handles = Vec::with_capacity(shards);
        let mut shard_services = Vec::with_capacity(shards);
        for i in 0..shards {
            let svc = format!("{SVC_JSON_SHARD_PREFIX}{i}");
            let h = server
                .serve_rpc_typed(&svc, Codec::Json, |req: EchoReq| async move {
                    Ok::<_, String>(EchoResp { body: req.body })
                })
                .expect("serve json shard");
            handles.push(h);
            shard_services.push(svc);
        }

        server
            .inner()
            .announce_capabilities(CapabilitySet::new())
            .await
            .expect("announce");
        let sentinel = &shard_services[0];
        let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
        while tokio::time::Instant::now() < deadline {
            if !caller.find_service_nodes(sentinel).is_empty() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert!(
            !caller.find_service_nodes(sentinel).is_empty(),
            "shard discovery did not converge within 2s"
        );

        Self {
            server,
            caller,
            server_node_id: server_id,
            shard_services,
            org_intent: None,
            _handles: handles,
        }
    }

    /// Two `Mesh` nodes as in [`Pair::new`], plus everything ONE protected
    /// unary call needs (Stage 0 slice 0.1 of
    /// `ORG_SCOPED_STREAMING_PLAN.md`):
    ///
    /// 1. an installed [`NodeAuthority`] on the provider — org B owns P, so
    ///    a `serve_rpc_protected` registration is legal at all;
    /// 2. that registration, under [`OrgAdmission::OwnerDelegated`] with an
    ///    allow-all provider policy (`|_| true`: the final application veto
    ///    is out of scope for a transport measurement);
    /// 3. a minted [`OrgProofIntent`] the caller installs on
    ///    [`CallOptions::org_proof_intent`] — see [`call_protected_raw`].
    ///
    /// The recipe is `tests/integration_nrpc_protected.rs:71-459`
    /// (`bring_up` / `install_authority` / `owner_delegated_intent` /
    /// `live_two_node_owner_delegated_admit`), not a re-derivation: the
    /// caller node's identity IS the intent's caller identity, and BOTH
    /// nodes announce so each pins the other's entity — the caller-side
    /// proof binding needs `caller.peer_entity_id(server)` and the
    /// provider's `resolve_direct_caller` needs the reverse.
    pub async fn protected() -> Self {
        // The caller node and the proof subject are one entity: the
        // provider resolves the authenticated session peer and requires it
        // to equal the proof's caller.
        let caller_kp = EntityKeypair::generate();
        let intent_kp = caller_kp.clone();

        let server = build_bench_mesh(EntityKeypair::generate(), bench_replay_config()).await;
        let caller = build_bench_mesh(caller_kp, AdmissionReplayConfig::default()).await;
        handshake_and_start(&server, &caller).await;
        let server_id = server.node_id();
        let caller_id = caller.node_id();

        // (1) The provider's org-B node authority. `adopt` writes a scratch
        // dir which is deliberately LEFT BEHIND: `OrgRevocationStore` keys
        // a process-global registry by the revocation sidecar's
        // (device, inode), so deleting it while this core is registered
        // lets a recycled inode join this store's live view
        // (integration_nrpc_protected.rs:50-64).
        let provider = server.inner().entity_id().clone();
        let org = OrgKeypair::from_bytes([0x42u8; 32]);
        let node_cert =
            OrgMembershipCert::try_issue(&org, provider.clone(), 1, 3600).expect("node cert");
        let dir = std::env::temp_dir().join(format!(
            "net-bench-org-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        let authority =
            NodeAuthority::adopt(&dir, node_cert, &provider, 0, None).expect("adopt authority");
        server
            .inner()
            .install_node_authority(Arc::new(authority))
            .expect("install authority");

        // (2) The protected registration. Same handler as the public raw
        // echo, so the public/protected delta is admission alone.
        let handle = server
            .node()
            .serve_rpc_protected(
                SVC_PROTECTED_RAW,
                Arc::new(RawEchoHandler),
                OrgAdmission::OwnerDelegated,
                Arc::new(|_| true),
            )
            .expect("serve protected");

        // Both directions must pin, so BOTH nodes announce (unlike
        // `Pair::new`, where only the server does).
        for mesh in [&server, &caller] {
            mesh.inner()
                .announce_capabilities(CapabilitySet::new())
                .await
                .expect("announce");
        }
        let pinned = || {
            caller.inner().peer_entity_id(server_id).is_some()
                && server.inner().peer_entity_id(caller_id).is_some()
        };
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        while tokio::time::Instant::now() < deadline {
            if pinned() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert!(
            pinned(),
            "entity pins were not established in both directions within 5s"
        );

        // (3) The owner-delegated intent: the caller acts for org B, which
        // also owns the provider.
        let caller_entity = intent_kp.entity_id().clone();
        let capability = CapabilityAuthorityId::for_tag(&format!("nrpc:{SVC_PROTECTED_RAW}"));
        let membership = OrgMembershipCert::try_issue(&org, caller_entity.clone(), 1, 3600)
            .expect("caller membership");
        let dispatcher = OrgDispatcherGrant::try_issue(
            &org,
            caller_entity,
            DispatcherScope::Exact(capability),
            3600,
        )
        .expect("dispatcher grant");
        let org_intent = OrgProofIntent {
            caller: Arc::new(intent_kp),
            membership,
            dispatcher,
            capability_grant: None,
            acting_org: org.org_id(),
            provider_owner_org: org.org_id(),
            provider,
            capability,
            proof_ttl_secs: 30,
        };

        Self {
            server,
            caller,
            server_node_id: server_id,
            shard_services: Vec::new(),
            org_intent: Some(org_intent),
            _handles: vec![handle],
        }
    }
}

// ============================================================================
// Raw handlers — postcard + identity. Postcard goes through the
// raw `serve_rpc` path because `Codec` only exposes Json /
// JsonPretty (mesh_rpc.rs:79-89); the bench manually encodes
// the same `EchoReq` struct with postcard on both ends.
// ============================================================================

struct PostcardEchoHandler;

#[async_trait::async_trait]
impl RpcHandler for PostcardEchoHandler {
    async fn call(
        &self,
        ctx: RpcContext,
    ) -> std::result::Result<RpcResponsePayload, RpcHandlerError> {
        let req: EchoReq = postcard::from_bytes(&ctx.payload.body)
            .map_err(|e| RpcHandlerError::Internal(format!("postcard decode: {e}")))?;
        let resp = EchoResp { body: req.body };
        let bytes = postcard::to_allocvec(&resp)
            .map_err(|e| RpcHandlerError::Internal(format!("postcard encode: {e}")))?;
        Ok(RpcResponsePayload {
            status: RpcStatus::Ok,
            headers: vec![],
            body: bytes.into(),
        })
    }
}

struct RawEchoHandler;

#[async_trait::async_trait]
impl RpcHandler for RawEchoHandler {
    async fn call(
        &self,
        ctx: RpcContext,
    ) -> std::result::Result<RpcResponsePayload, RpcHandlerError> {
        Ok(RpcResponsePayload {
            status: RpcStatus::Ok,
            headers: vec![],
            body: ctx.payload.body,
        })
    }
}

// ============================================================================
// Streaming request shape — payload + count of emissions. Lives
// here so both `nrpc_common` (handler registration) and
// `nrpc_streaming.rs` (call site) see the same type.
// ============================================================================

#[derive(Serialize, Deserialize, Clone)]
pub struct StreamReq {
    pub body: String,
    pub count: u32,
}

// ============================================================================
// Caller-side helpers. Each routing path × codec gets one fn that
// the bench loop can call without re-stating the encode/decode
// dance.
// ============================================================================

/// Direct `call_typed` with `Codec::Json`. Routing path: known
/// target node id, no capability-index lookup.
pub async fn call_json_direct(pair: &Pair, req: &EchoReq) -> EchoResp {
    pair.caller
        .call_typed(
            pair.server_node_id,
            SVC_JSON,
            req,
            CallOptionsTyped::default(),
        )
        .await
        .expect("call json direct")
}

/// Same as [`call_json_direct`] but retries on transient transport
/// backpressure (`RpcError::Transport(...)`). The per-stream publish
/// budget fills up either at high concurrency (`nrpc_qps` /
/// `nrpc_tail`) or when a single large unary body chunks past the
/// budget (`nrpc_payload`) — mesh_rpc.rs:1158 classifies these as
/// retriable on purpose. The bench yields after each backpressure
/// hit so the transport flushes and other in-flight callers make
/// progress; the resulting wall-clock latency reflects real
/// saturation behavior rather than masking it as a panic.
pub async fn call_json_direct_retrying(pair: &Pair, req: &EchoReq) -> EchoResp {
    use net_sdk::mesh_rpc::RpcError;
    let deadline_base = std::time::Instant::now();
    loop {
        match pair
            .caller
            .call_typed::<_, EchoResp>(
                pair.server_node_id,
                SVC_JSON,
                req,
                CallOptionsTyped::default(),
            )
            .await
        {
            Ok(resp) => return resp,
            Err(RpcError::Transport(_)) => {
                backpressure_yield(deadline_base, "call_json_direct_retrying").await;
            }
            Err(e) => panic!("call json direct (retrying): {e}"),
        }
    }
}

/// Direct `call_typed` against shard `shard % shards` of a pair
/// built with [`Pair::new_sharded`]. Retries on transient transport
/// backpressure exactly like [`call_json_direct_retrying`]. The bench
/// loop passes the in-flight index as `shard` so calls round-robin
/// across channels; with `shards == 1` every call lands on the one
/// channel (the single-channel baseline).
pub async fn call_json_shard_retrying(pair: &Pair, req: &EchoReq, shard: usize) -> EchoResp {
    use net_sdk::mesh_rpc::RpcError;
    let svc = &pair.shard_services[shard % pair.shard_services.len()];
    let deadline_base = std::time::Instant::now();
    loop {
        match pair
            .caller
            .call_typed::<_, EchoResp>(pair.server_node_id, svc, req, CallOptionsTyped::default())
            .await
        {
            Ok(resp) => return resp,
            Err(RpcError::Transport(_)) => {
                backpressure_yield(deadline_base, "call_json_shard_retrying").await;
            }
            Err(e) => panic!("call json shard (retrying): {e}"),
        }
    }
}

/// Discovery `call_service_typed` with `Codec::Json`. Routing
/// path: capability-index lookup picks the server.
pub async fn call_json_discovery(pair: &Pair, req: &EchoReq) -> EchoResp {
    pair.caller
        .call_service_typed(SVC_JSON, req, CallOptionsTyped::default())
        .await
        .expect("call json discovery")
}

/// Direct raw `call` with postcard encode/decode applied by the
/// bench. Skips the typed wrapper to dodge the `Codec` enum's
/// JSON-only scope.
pub async fn call_postcard_direct(pair: &Pair, req: &EchoReq) -> EchoResp {
    let body = postcard::to_allocvec(req).expect("postcard encode");
    let reply = pair
        .caller
        .call(
            pair.server_node_id,
            SVC_POSTCARD,
            Bytes::from(body),
            CallOptions::default(),
        )
        .await
        .expect("call postcard");
    postcard::from_bytes(&reply.body).expect("postcard decode")
}

/// Same as [`call_postcard_direct`] but retries on transient
/// transport backpressure, mirroring [`call_json_direct_retrying`].
/// Used by `nrpc_payload` where large bodies chunk past the
/// per-stream publish budget. Re-encoding per attempt is intentional:
/// a backpressured `call` published nothing the server accepted, so
/// the retry is a fresh call, not a duplicate.
pub async fn call_postcard_direct_retrying(pair: &Pair, req: &EchoReq) -> EchoResp {
    use net_sdk::mesh_rpc::RpcError;
    let body = postcard::to_allocvec(req).expect("postcard encode");
    let deadline_base = std::time::Instant::now();
    loop {
        match pair
            .caller
            .call(
                pair.server_node_id,
                SVC_POSTCARD,
                Bytes::from(body.clone()),
                CallOptions::default(),
            )
            .await
        {
            Ok(reply) => return postcard::from_bytes(&reply.body).expect("postcard decode"),
            Err(RpcError::Transport(_)) => {
                backpressure_yield(deadline_base, "call_postcard_direct_retrying").await
            }
            Err(e) => panic!("call postcard (retrying): {e}"),
        }
    }
}

/// Direct raw `call` with no codec — body bytes round-trip
/// verbatim. The theoretical floor: every byte the bench
/// measures is genuine transport cost.
pub async fn call_raw_direct(pair: &Pair, body: Bytes) -> Bytes {
    pair.caller
        .call(pair.server_node_id, SVC_RAW, body, CallOptions::default())
        .await
        .expect("call raw")
        .body
}

/// Same as [`call_raw_direct`] but retries on transient transport
/// backpressure, mirroring [`call_json_direct_retrying`]. Used by
/// `nrpc_payload` where large bodies chunk past the per-stream
/// publish budget.
pub async fn call_raw_direct_retrying(pair: &Pair, body: Bytes) -> Bytes {
    use net_sdk::mesh_rpc::RpcError;
    let deadline_base = std::time::Instant::now();
    loop {
        match pair
            .caller
            .call(
                pair.server_node_id,
                SVC_RAW,
                body.clone(),
                CallOptions::default(),
            )
            .await
        {
            Ok(reply) => return reply.body,
            Err(RpcError::Transport(_)) => {
                backpressure_yield(deadline_base, "call_raw_direct_retrying").await
            }
            Err(e) => panic!("call raw (retrying): {e}"),
        }
    }
}

/// Direct raw `call` against the PROTECTED service of a
/// [`Pair::protected`] pair, carrying a freshly minted owner-delegated
/// admission proof.
///
/// The delta against [`call_raw_direct`] at the same payload is the org
/// OPENING cost and nothing else — same transport, same handler, same
/// body: caller-side proof mint (one ed25519 signature over the finalized
/// request), the proof header's bytes on the wire, and the provider's
/// §2.4 verification order (certificate + grant + call-binding signature
/// checks, revocation floors, and the atomic replay insert).
///
/// The intent is cloned per call because `CallOptions` owns it. That is
/// the production shape, not a bench tax: the facade's own caller builds a
/// fresh intent for every call (`sdk/src/org/call.rs:241`).
pub async fn call_protected_raw(pair: &Pair, body: Bytes) -> Bytes {
    let opts = CallOptions {
        org_proof_intent: Some(
            pair.org_intent
                .clone()
                .expect("protected calls need a Pair::protected() pair"),
        ),
        ..CallOptions::default()
    };
    pair.caller
        .call(pair.server_node_id, SVC_PROTECTED_RAW, body, opts)
        .await
        .expect("protected raw call")
        .body
}

// ============================================================================
// Runtime constructor — multi-threaded tokio runtime used by
// every bench. 4 workers matches the existing test/example
// setup (nrpc_echo.rs:60).
//
// The worker-thread count is overridable via the
// `NRPC_BENCH_WORKER_THREADS` env var so the concurrency-scaling
// sweep (Phase 0a of NRPC_QPS_CONCURRENCY_SCALING_PLAN.md) can run
// 4 / 8 / 16 workers without a recompile. Both nodes of a `Pair`
// share this single runtime, so the count caps the cores available
// to client + server combined. Unset / unparseable → 4 (the
// baseline every committed bench number was taken at).
// ============================================================================

pub fn worker_threads() -> usize {
    std::env::var("NRPC_BENCH_WORKER_THREADS")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .filter(|&n| n > 0)
        .unwrap_or(4)
}

pub fn runtime() -> Runtime {
    RtBuilder::new_multi_thread()
        .worker_threads(worker_threads())
        .enable_all()
        .build()
        .expect("tokio runtime")
}
