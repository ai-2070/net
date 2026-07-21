//! Kyra OA3 review, Finding 3 witness — an exposure REVOCATION must reach peers
//! promptly even when it lands inside the origin-side announce rate-limit
//! window, and even on a node started with the bare `start()` shape that has no
//! deferred-flush task.
//!
//! The contract the OA3 send gate exists to uphold is not merely "never
//! serialize stale bytes" — refusing to send is only half of it. If the node
//! refuses (or coalesces away) the send that would SUPERSEDE the revoked
//! exposure, peers keep holding the previous announcement, which still names the
//! capability in the clear. Silence is not safety here: the stale plaintext is
//! already out there.
//!
//! Shape:
//!   * `min_announce_interval` is deliberately long, so the post-revocation
//!     announce lands squarely inside the window;
//!   * the node is started with `start()`, NOT `start_arc()` — the deferral arm
//!     is a silent drop on that shape, so nothing can rescue a coalesced send;
//!   * the revocation is a real `ServeHandle` retirement, not a synthetic bump.
//!
//! Against a build that only restores the rate-limit timestamp after a refused
//! send, the final assertion fails: the peer keeps `nrpc:secret-svc` for the
//! whole window.

#![cfg(all(feature = "net", feature = "cortex"))]

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use net::adapter::net::behavior::capability::{CapabilityFilter, CapabilitySet};
use net::adapter::net::cortex::{
    RpcContext, RpcHandler, RpcHandlerError, RpcResponsePayload, RpcStatus,
};
use net::adapter::net::{EntityKeypair, MeshNode, MeshNodeConfig, SocketBufferConfig};

const PSK: [u8; 32] = [0x42u8; 32];
const TEST_BUFFER_SIZE: usize = 256 * 1024;

/// Long enough that every announce after the first lands in-window.
const LONG_ANNOUNCE_WINDOW: Duration = Duration::from_secs(120);

struct TrivialHandler;

#[async_trait::async_trait]
impl RpcHandler for TrivialHandler {
    async fn call(&self, _ctx: RpcContext) -> Result<RpcResponsePayload, RpcHandlerError> {
        Ok(RpcResponsePayload {
            status: RpcStatus::Ok,
            headers: vec![],
            body: Bytes::new(),
        })
    }
}

fn test_config() -> MeshNodeConfig {
    let addr: SocketAddr = "127.0.0.1:0".parse().unwrap();
    let mut cfg = MeshNodeConfig::new(addr, PSK)
        .with_heartbeat_interval(Duration::from_millis(200))
        .with_session_timeout(Duration::from_secs(5))
        .with_handshake(3, Duration::from_secs(2))
        .with_min_announce_interval(LONG_ANNOUNCE_WINDOW);
    cfg.socket_buffers = SocketBufferConfig {
        send_buffer_size: TEST_BUFFER_SIZE,
        recv_buffer_size: TEST_BUFFER_SIZE,
    };
    cfg
}

async fn build_node() -> Arc<MeshNode> {
    Arc::new(
        MeshNode::new(EntityKeypair::generate(), test_config())
            .await
            .expect("MeshNode::new"),
    )
}

async fn wait_until<F: FnMut() -> bool>(mut cond: F, timeout: Duration) -> bool {
    let deadline = std::time::Instant::now() + timeout;
    while std::time::Instant::now() < deadline {
        if cond() {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    cond()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn retiring_a_public_service_supersedes_the_peer_view_inside_the_rate_window() {
    let server = build_node().await;
    let client = build_node().await;

    let server_id = server.node_id();
    let client_id = client.node_id();
    let server_pub = *server.public_key();
    let server_addr = server.local_addr();
    let server_clone = server.clone();
    let accept = tokio::spawn(async move { server_clone.accept(client_id).await });
    client
        .connect(server_addr, &server_pub, server_id)
        .await
        .expect("connect");
    accept.await.expect("accept task").expect("accept");
    // BARE start on purpose: `start()` leaves `self_weak` unset, so the RT-1
    // deferral arm has no task to spawn and silently drops. Any correctness that
    // depends on a trailing-edge flush is unavailable on this shape.
    server.start();
    client.start();

    // ---- the exposure ----
    let handle = server
        .serve_rpc("secret-svc", Arc::new(TrivialHandler))
        .expect("serve");
    server
        .announce_capabilities(CapabilitySet::new())
        .await
        .expect("announce");

    let exposed = CapabilityFilter::new().require_tag("nrpc:secret-svc");
    assert!(
        wait_until(
            || client.find_nodes_by_filter(&exposed).contains(&server_id),
            Duration::from_secs(10),
        )
        .await,
        "precondition: the client must first observe the PUBLIC nrpc:secret-svc tag",
    );

    // ---- the revocation, squarely inside the rate-limit window ----
    // A real retirement through the production Drop path. This is an exposure
    // revocation: the client is still holding an announcement that names the
    // service in the clear.
    drop(handle);

    // The application re-announces after changing its service set — the ordinary
    // thing to do. There is no RT-3 loop on a bare-start node, so this is the
    // ONLY send that can supersede the stale exposure. It lands well inside
    // `LONG_ANNOUNCE_WINDOW`, so a build that lets ordinary coalescing swallow it
    // leaves the client on the stale view until the keep-alive.
    server
        .announce_capabilities(CapabilitySet::new())
        .await
        .expect("post-revocation announce");

    assert!(
        wait_until(
            || !client.find_nodes_by_filter(&exposed).contains(&server_id),
            Duration::from_secs(10),
        )
        .await,
        "the client must stop seeing nrpc:secret-svc promptly after the retirement — \
         a revocation that lands inside the rate-limit window must not be coalesced \
         away (and on a bare-start node there is no flush task to rescue it)",
    );
}

/// Kyra OA3 review, Finding 3 witness — a send REFUSED on security grounds must
/// itself drive a corrective send. It cannot lean on the wake its own
/// invalidation fired: that wake may already have run, observed this sender's
/// in-window claim, and returned.
///
/// Determinism comes from the shape rather than from forcing an interleaving:
/// this node is bare-`start`ed with a long rate-limit window, so there is NO
/// RT-3 loop and NO deferred-flush task. Nothing whatsoever can rescue a
/// refused send except the refusing sender itself — strictly stronger than the
/// "B consumed the wake" race, because here there is no B at all.
///
/// The probe fires inside the send seqlock and advances the exposure epoch
/// exactly as a concurrent revocation would, so the first attempt is refused.
/// It is one-shot, so the corrective attempt proceeds against stable state.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_security_refused_send_drives_its_own_corrective_send() {
    let server = build_node().await;
    let client = build_node().await;

    let server_id = server.node_id();
    let client_id = client.node_id();
    let server_pub = *server.public_key();
    let server_addr = server.local_addr();
    let server_clone = server.clone();
    let accept = tokio::spawn(async move { server_clone.accept(client_id).await });
    client
        .connect(server_addr, &server_pub, server_id)
        .await
        .expect("connect");
    accept.await.expect("accept task").expect("accept");
    server.start();
    client.start();

    let _handle = server
        .serve_rpc("corrective-svc", Arc::new(TrivialHandler))
        .expect("serve");

    // One-shot: refuse the FIRST attempt from inside its send seqlock.
    let fired = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let server_probe = server.clone();
    let fired_probe = Arc::clone(&fired);
    let probe = move || {
        if !fired_probe.swap(true, std::sync::atomic::Ordering::SeqCst) {
            server_probe.test_advance_visibility_generation();
        }
    };

    server
        .announce_with_send_probe_for_test(CapabilitySet::new(), &probe)
        .await
        .expect("announce");

    assert!(
        fired.load(std::sync::atomic::Ordering::SeqCst),
        "the probe must actually have fired inside the send seqlock",
    );

    let served = CapabilityFilter::new().require_tag("nrpc:corrective-svc");
    assert!(
        wait_until(
            || client.find_nodes_by_filter(&served).contains(&server_id),
            Duration::from_secs(10),
        )
        .await,
        "the refused send must have driven a corrective send of its own — nothing \
         else exists on a bare-start node to carry the announcement",
    );
}

/// Kyra OA3 review, Finding 4 witness — a refused sender releasing its
/// broadcast-window claim must not clobber a NEWER claimant.
///
/// Both claims are taken at the SAME `Instant`, which is the case timestamp
/// equality decides wrongly: `last_broadcast_at == Some(my_now)` is true for A
/// even though B owns the slot, so A restores its own predecessor value over
/// B's live claim and reopens a window B is relying on. Two real claims can
/// share an `Instant` at platform clock resolution — coarse on Windows — so this
/// is not a contrived state. The monotonic claim token decides it correctly
/// regardless of the clock.
#[tokio::test]
async fn a_refused_sender_cannot_roll_back_over_a_newer_claim() {
    let node = build_node().await;

    let t0 = std::time::Instant::now();
    // Seed a prior broadcast so there is something for A to restore.
    let (seed_claim, _) = node.test_claim_broadcast_window_at(t0);
    let _ = seed_claim;
    let seeded = node.test_broadcast_window_at();
    assert_eq!(seeded, Some(t0));

    // A claims at t1 ...
    let t1 = std::time::Instant::now();
    let (claim_a, previous_a) = node.test_claim_broadcast_window_at(t1);
    assert_eq!(previous_a, Some(t0), "A must carry out what it displaced");

    // ... and B claims at the SAME instant, taking ownership from A.
    let (claim_b, previous_b) = node.test_claim_broadcast_window_at(t1);
    assert_ne!(claim_a, claim_b, "each claim gets a distinct token");
    assert_eq!(previous_b, Some(t1));
    let owned_by_b = node.test_broadcast_window_at();

    // A's send is now refused and it releases. The slot is B's, so nothing moves
    // — even though A's timestamp compares equal to what is stored.
    node.test_release_broadcast_claim(claim_a, previous_a);
    assert_eq!(
        node.test_broadcast_window_at(),
        owned_by_b,
        "a stale claimant must not restore over a newer claim (timestamp equality \
         would have wrongly permitted it here)",
    );

    // B releasing its OWN claim does restore, so the rule is not merely inert.
    node.test_release_broadcast_claim(claim_b, previous_b);
    assert_eq!(
        node.test_broadcast_window_at(),
        Some(t1),
        "the live claimant's release restores what it displaced",
    );
}

/// Kyra OA3 review (deferred-flush claim ownership) — the trailing-edge flush is
/// a broadcast-window CLAIMANT and must advance the ownership token like any
/// other. It previously wrote `last_broadcast_at` directly, which left it
/// invisible to the release rule: an immediate sender that claimed first and was
/// refused later still matched its own token and rolled its predecessor back
/// over the flush's newer window, silently reopening it.
///
/// Drives the REAL `flush_deferred_announce` rather than a mirror of its claim.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_deferred_flush_claim_cannot_be_rolled_back_by_an_earlier_sender() {
    let node = build_node().await;
    node.start();
    let _handle = node
        .serve_rpc("flush-svc", Arc::new(TrivialHandler))
        .expect("serve");
    node.announce_capabilities(CapabilitySet::new())
        .await
        .expect("announce");

    // Immediate sender A claims the window.
    let t_a = std::time::Instant::now();
    let (claim_a, previous_a) = node.test_claim_broadcast_window_at(t_a);

    // The deferred flush (B) fires and claims through the shared helper.
    let generation = node.test_arm_deferred_flush();
    node.test_flush_deferred_announce(generation, None).await;
    let owned_by_flush = node.test_broadcast_window_at();
    assert!(
        owned_by_flush.is_some() && owned_by_flush != Some(t_a),
        "the deferred flush must have taken the window",
    );

    // A's send is refused and it releases. The flush owns the slot now, so A's
    // release must be inert — it holds a superseded token.
    node.test_release_broadcast_claim(claim_a, previous_a);
    assert_eq!(
        node.test_broadcast_window_at(),
        owned_by_flush,
        "an earlier immediate claimant must not roll back over the deferred \
         flush's newer claim",
    );
}

/// Kyra OA3 review (reflex-reset claim invalidation) — a rate-limit-floor reset
/// invalidates outstanding ownership. Without advancing the token, a sender that
/// claimed before the reset and is refused after it would still compare equal
/// and restore its predecessor timestamp, silently re-arming the rate limit the
/// reset had just cleared.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_reflex_reset_invalidates_outstanding_broadcast_claims() {
    let node = build_node().await;

    // Seed a prior broadcast, then A claims.
    let t0 = std::time::Instant::now();
    node.test_claim_broadcast_window_at(t0);
    let t_a = std::time::Instant::now();
    let (claim_a, previous_a) = node.test_claim_broadcast_window_at(t_a);
    assert_eq!(previous_a, Some(t0));

    // The reset clears the floor so the next announce broadcasts unconditionally.
    node.test_invalidate_broadcast_window();
    assert_eq!(
        node.test_broadcast_window_at(),
        None,
        "the reset must clear the rate-limit floor",
    );

    // A is refused and releases. It must NOT undo the reset by restoring t0.
    node.test_release_broadcast_claim(claim_a, previous_a);
    assert_eq!(
        node.test_broadcast_window_at(),
        None,
        "a claim outstanding across a reset must not restore over it — doing so \
         re-arms the rate limit the reset deliberately cleared",
    );
}

/// Kyra OA3 review (deferred security correction) — when the DEFERRED flush's
/// own send is refused by the seqlock it must release its claim and drive the
/// same bounded corrective pass the immediate path does.
///
/// Previously it just returned: window consumed, nothing sent, no correction,
/// on-wire exposure generation untouched. An exposure revocation would usually
/// wake a security-priority immediate announce, but an authority or
/// provider-grant invalidation moves no exposure epoch, so nothing was
/// guaranteed to carry the supersession on this path.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_security_refused_deferred_flush_drives_a_corrective_send() {
    let server = build_node().await;
    let client = build_node().await;

    let server_id = server.node_id();
    let client_id = client.node_id();
    let server_pub = *server.public_key();
    let server_addr = server.local_addr();
    let server_clone = server.clone();
    let accept = tokio::spawn(async move { server_clone.accept(client_id).await });
    client
        .connect(server_addr, &server_pub, server_id)
        .await
        .expect("connect");
    accept.await.expect("accept task").expect("accept");
    server.start();
    client.start();

    // §T6: seed BEFORE the service exists, so the seed announce cannot carry
    // `nrpc:deferred-svc`. The tag can then only reach the client via the
    // corrective pass — which is what this test is named for.
    //
    // Previously the service was registered first and the seed announce
    // carried the tag, so the test asserted as a PRECONDITION that the client
    // could see it and then, after the action, asserted the identical
    // predicate again. Nothing in between could remove the entry, so the final
    // assertion held whether or not a corrective pass ever happened: making
    // the refused-flush branch `return` early left it green. The code had
    // drifted from the comment directly above it, which already stated the
    // right intent.
    server
        .announce_capabilities(CapabilitySet::new())
        .await
        .expect("seed announce");

    let served = CapabilityFilter::new().require_tag("nrpc:deferred-svc");
    assert!(
        wait_until(
            || client
                .find_nodes_by_filter(&CapabilityFilter::new())
                .contains(&server_id),
            Duration::from_secs(10),
        )
        .await,
        "precondition: the client sees the server at all",
    );
    assert!(
        !client.find_nodes_by_filter(&served).contains(&server_id),
        "precondition: the client must NOT yet know the tag — otherwise the          final assertion cannot distinguish a corrective pass from the seed",
    );

    // NOW register the service. This is what the deferred flush will carry.
    let _handle = server
        .serve_rpc("deferred-svc", Arc::new(TrivialHandler))
        .expect("serve");

    // Now force the DEFERRED flush's send to be refused, one-shot.
    let fired = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let server_probe = server.clone();
    let fired_probe = Arc::clone(&fired);
    let probe = move || {
        if !fired_probe.swap(true, std::sync::atomic::Ordering::SeqCst) {
            server_probe.test_advance_visibility_generation();
        }
    };

    let generation = server.test_arm_deferred_flush();
    server
        .test_flush_deferred_announce(generation, Some(&probe))
        .await;

    assert!(
        fired.load(std::sync::atomic::Ordering::SeqCst),
        "the probe must actually have fired inside the flush's send seqlock",
    );
    // The corrective pass must have published a SUPERSEDING announcement. The
    // emission the flush refused is gone; only a fresh one can be on the wire,
    // and the node must be able to serialize one right now.
    assert!(
        wait_until(
            || server.announcement_bytes_for_send_for_test().is_some(),
            Duration::from_secs(10),
        )
        .await,
        "the refused deferred flush must have driven a corrective pass that \
         republished a coherent emission",
    );
    assert!(
        wait_until(
            || client.find_nodes_by_filter(&served).contains(&server_id),
            Duration::from_secs(10),
        )
        .await,
        "the tag must reach the client via the CORRECTIVE pass — the seed          announce predates the service, so this is the only way it can arrive",
    );
}
