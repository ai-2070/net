//! Integration tests for stage 4a of `docs/NAT_TRAVERSAL_PLAN.md`:
//! `MeshNodeConfig::reflex_override`.
//!
//! A reflex override is for operators who already know their
//! node's public `SocketAddr` — port-forwarded servers, manually
//! configured VPN endpoints, or (stage 4b) a successful UPnP /
//! NAT-PMP mapping. Setting one short-circuits the classifier's
//! multi-peer sweep and starts the node in `NatClass::Open` with
//! the supplied address advertised on capability announcements.
//!
//! **Framing reminder** (plan §4): the override is an
//! optimization surface — a node with no override still reaches
//! every peer through the routed-handshake path. Tests assert
//! classifier + announcement semantics; they do NOT assert that
//! the mesh is otherwise broken without the override.
//!
//! # Properties under test
//!
//! - **Construction honors the override.** A mesh built with
//!   `with_reflex_override(addr)` reports `NatClass::Open` and
//!   `reflex_addr() == Some(addr)` immediately — no probes fired,
//!   no peers required.
//! - **Override propagates via capability announcement.** After
//!   A announces its capabilities, B's index sees A's reflex as
//!   the override value (not A's bind address).
//! - **`reclassify_nat` is a no-op when override is set.**
//!   Calling reclassify doesn't replace the overridden values
//!   with observed reflexes — the override is load-bearing even
//!   when the node has plenty of peers.
//! - **No override → normal classifier path.** A mesh without
//!   an override behaves exactly as before — Unknown until
//!   classified, then Open via observation on localhost.
//!
//! Run: `cargo test --features net,nat-traversal --test reflex_override`

#![cfg(all(feature = "net", feature = "nat-traversal"))]

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use net::adapter::net::behavior::capability::{CapabilityFilter, CapabilitySet};
use net::adapter::net::traversal::classify::NatClass;
use net::adapter::net::{EntityKeypair, MeshNode, MeshNodeConfig, SocketBufferConfig};
const TEST_BUFFER_SIZE: usize = 256 * 1024;
const PSK: [u8; 32] = [0x42u8; 32];

/// Bind via `127.0.0.1:0` so the OS picks a free port — no
/// pre-bind reservation, no TOCTOU race with parallel tests.
fn test_config() -> MeshNodeConfig {
    let addr: SocketAddr = "127.0.0.1:0".parse().unwrap();
    let mut cfg = MeshNodeConfig::new(addr, PSK)
        .with_heartbeat_interval(Duration::from_millis(200))
        .with_session_timeout(Duration::from_secs(5))
        .with_handshake(3, Duration::from_secs(2));
    cfg.socket_buffers = SocketBufferConfig {
        send_buffer_size: TEST_BUFFER_SIZE,
        recv_buffer_size: TEST_BUFFER_SIZE,
    };
    cfg
}

async fn build_mesh_with_override(external: SocketAddr) -> Arc<MeshNode> {
    let cfg = test_config().with_reflex_override(external);
    Arc::new(
        MeshNode::new(EntityKeypair::generate(), cfg)
            .await
            .expect("MeshNode::new"),
    )
}

async fn build_mesh_plain() -> Arc<MeshNode> {
    let cfg = test_config();
    Arc::new(
        MeshNode::new(EntityKeypair::generate(), cfg)
            .await
            .expect("MeshNode::new"),
    )
}

async fn connect_pair(a: &Arc<MeshNode>, b: &Arc<MeshNode>) {
    let a_id = a.node_id();
    let b_pub = *b.public_key();
    let b_addr = b.local_addr();
    let b_id = b.node_id();
    let b_clone = b.clone();
    let accept = tokio::spawn(async move { b_clone.accept(a_id).await });
    a.connect(b_addr, &b_pub, b_id)
        .await
        .expect("connect failed");
    accept
        .await
        .expect("accept task panicked")
        .expect("accept failed");
}

/// A freshly-built mesh with `reflex_override` set reports
/// `Open` + the override as its reflex_addr *before* any peers
/// are connected and *before* start() is called. No classification
/// probes happened — the override is load-bearing at init time.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn override_forces_open_at_construction() {
    let external: SocketAddr = "203.0.113.42:9001".parse().unwrap();
    let node = build_mesh_with_override(external).await;

    assert_eq!(
        node.nat_class(),
        NatClass::Open,
        "override should force Open at construction",
    );
    assert_eq!(
        node.reflex_addr(),
        Some(external),
        "reflex_addr should reflect the override",
    );
}

/// `reclassify_nat` is a no-op when the override is set. Even
/// with enough peers to run the sweep, the override is preserved.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn reclassify_is_noop_when_override_set() {
    let external: SocketAddr = "203.0.113.42:9001".parse().unwrap();
    let a = build_mesh_with_override(external).await;
    let b = build_mesh_plain().await;
    let c = build_mesh_plain().await;

    connect_pair(&a, &b).await;
    connect_pair(&a, &c).await;
    a.start();
    b.start();
    c.start();

    // A has ≥2 peers; normally reclassify_nat would fire probes
    // and (on localhost) land on Open with reflex == bind. The
    // override must preempt that result.
    a.reclassify_nat().await;

    assert_eq!(a.nat_class(), NatClass::Open);
    assert_eq!(
        a.reflex_addr(),
        Some(external),
        "reflex_addr must stay at the override; reclassify must not overwrite it",
    );
}

/// Override propagates through the capability broadcast: B's
/// index, after receiving A's announcement, sees the override as
/// A's reflex, not A's bind address.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn override_propagates_through_capability_broadcast() {
    let external: SocketAddr = "198.51.100.7:54321".parse().unwrap();
    let a = build_mesh_with_override(external).await;
    let b = build_mesh_plain().await;

    connect_pair(&a, &b).await;
    a.start();
    b.start();

    a.announce_capabilities(CapabilitySet::new())
        .await
        .expect("A announce");

    // Allow the announcement to reach B and land in its index.
    let a_id = a.node_id();
    let mut ready = false;
    for _ in 0..30 {
        if b.peer_reflex_addr(a_id) == Some(external) {
            ready = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(
        ready,
        "B should see A's override reflex (not bind); got {:?}",
        b.peer_reflex_addr(a_id),
    );

    // B should also see A's nat:open tag (override implies Open).
    let peers = b.find_nodes_by_filter(
        &net::adapter::net::behavior::capability::CapabilityFilter::new().require_tag("nat:open"),
    );
    assert!(
        peers.contains(&a_id),
        "B's index should tag A as nat:open; got peers = {peers:?}",
    );
}

/// Runtime setter: a node that started without an override can
/// have one installed mid-session (the future stage-4b
/// PortMapper path). After install, `nat_class` flips to `Open`,
/// `reflex_addr` returns the installed address, and reclassify
/// becomes a no-op.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn runtime_set_reflex_override_flips_open() {
    let a = build_mesh_plain().await;
    let b = build_mesh_plain().await;
    let c = build_mesh_plain().await;

    connect_pair(&a, &b).await;
    connect_pair(&a, &c).await;
    a.start();
    b.start();
    c.start();

    // Pre-override: plain classifier path. Start Unknown,
    // classify on demand to Open via localhost reflex.
    assert_eq!(a.nat_class(), NatClass::Unknown);
    a.reclassify_nat().await;
    assert_eq!(a.nat_class(), NatClass::Open);
    assert_eq!(a.reflex_addr(), Some(a.local_addr()));

    // Install a runtime override. `nat_class` stays Open but
    // `reflex_addr` switches to the operator-supplied value.
    let external: SocketAddr = "203.0.113.99:4242".parse().unwrap();
    a.set_reflex_override(external);
    assert_eq!(a.nat_class(), NatClass::Open);
    assert_eq!(a.reflex_addr(), Some(external));

    // Reclassify is now a no-op — the override is load-bearing
    // regardless of how we got there.
    a.reclassify_nat().await;
    assert_eq!(a.nat_class(), NatClass::Open);
    assert_eq!(a.reflex_addr(), Some(external));
}

/// Runtime clear: a previously-installed override drops back to
/// `Unknown` + `None`, and the classifier resumes producing real
/// observations on the next sweep. This is the port-mapper
/// revoke path — a failed renewal yanks the override so the
/// mesh doesn't keep advertising a defunct reflex.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn runtime_clear_reflex_override_resumes_classifier() {
    let a = build_mesh_plain().await;
    let b = build_mesh_plain().await;
    let c = build_mesh_plain().await;

    connect_pair(&a, &b).await;
    connect_pair(&a, &c).await;
    a.start();
    b.start();
    c.start();

    // Install, then clear.
    let external: SocketAddr = "203.0.113.99:4242".parse().unwrap();
    a.set_reflex_override(external);
    assert_eq!(a.nat_class(), NatClass::Open);
    assert_eq!(a.reflex_addr(), Some(external));

    a.clear_reflex_override();
    // Immediately after clear: back to Unknown / None. The
    // override value is gone so a concurrent announce can't
    // stamp the defunct reflex onto an outbound packet.
    assert_eq!(a.nat_class(), NatClass::Unknown);
    assert!(a.reflex_addr().is_none());

    // Classifier can now run and produce a real observation.
    a.reclassify_nat().await;
    assert_eq!(a.nat_class(), NatClass::Open);
    assert_eq!(a.reflex_addr(), Some(a.local_addr()));
}

/// `clear_reflex_override` is a no-op when no override is
/// active. Shutdown / revoke paths can call it unconditionally
/// without first checking state.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn runtime_clear_is_noop_when_no_override() {
    let a = build_mesh_plain().await;

    // State before: Unknown / None.
    assert_eq!(a.nat_class(), NatClass::Unknown);
    assert!(a.reflex_addr().is_none());

    a.clear_reflex_override();

    // State after: unchanged.
    assert_eq!(a.nat_class(), NatClass::Unknown);
    assert!(a.reflex_addr().is_none());
}

/// A plain mesh (no override) still uses the classifier path
/// unchanged — Unknown until sweep, then Open via real probes.
/// Regression guard: adding the override should not affect the
/// default path.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn no_override_uses_classifier_path() {
    let a = build_mesh_plain().await;
    let b = build_mesh_plain().await;
    let c = build_mesh_plain().await;

    connect_pair(&a, &b).await;
    connect_pair(&a, &c).await;
    a.start();
    b.start();
    c.start();

    // Pre-sweep: Unknown / None.
    assert_eq!(a.nat_class(), NatClass::Unknown);
    assert!(a.reflex_addr().is_none());

    a.reclassify_nat().await;

    // Post-sweep on localhost: Open, reflex == bind.
    assert_eq!(a.nat_class(), NatClass::Open);
    assert_eq!(a.reflex_addr(), Some(a.local_addr()));
}

/// Regression test for a cubic-flagged P1 bug: the three
/// traversal atomics (`nat_class`, `reflex_addr`,
/// `reflex_override_active`) were written independently by
/// `set_reflex_override` / `clear_reflex_override` /
/// `commit_reclassify_observations`, and read independently by
/// `announce_capabilities_with`. A concurrent announce could
/// interleave between writes and publish a torn state — e.g.
/// the new override's reflex paired with the pre-override
/// `nat_class`, or the cleared flag paired with a not-yet-
/// reset reflex.
///
/// The fix wraps the multi-field writes + the
/// `(nat_class, reflex_addr)` read in `announce` under a
/// shared `traversal_publish_mu`. This test stresses the race:
/// two tasks run in parallel, one toggling the override, the
/// other announcing as fast as it can. After each announce we
/// inspect `local_announcement` and assert the (class, reflex)
/// pair is *coherent* — i.e. matches one of the two valid
/// steady states, never the torn combinations the bug allowed.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn override_set_clear_is_atomic_with_announce_read() {
    use std::sync::atomic::{AtomicBool, Ordering};

    let a = build_mesh_plain().await;
    let override_addr: SocketAddr = "203.0.113.88:55555".parse().unwrap();

    // Seed a stable pre-override state: classifier sweep gives
    // Open + bind addr. From here the toggler flips Open+bind
    // (cleared) ↔ Open+override (set). Note `Unknown` is only
    // transient during the classifier path; once we've swept
    // the steady states are both class=Open, distinguished by
    // reflex.
    //
    // Actually we can't easily pre-sweep without peers; instead
    // start in the override-set state and toggle between
    // {override set} and {override cleared (Unknown + None)}.
    let stop = Arc::new(AtomicBool::new(false));

    let node_toggler = a.clone();
    let stop_toggler = stop.clone();
    let toggler = tokio::spawn(async move {
        for _ in 0..2_000 {
            if stop_toggler.load(Ordering::Relaxed) {
                break;
            }
            node_toggler.set_reflex_override(override_addr);
            node_toggler.clear_reflex_override();
        }
    });

    let node_announcer = a.clone();
    let stop_announcer = stop.clone();
    let announcer = tokio::spawn(async move {
        let mut torn = 0u64;
        for _ in 0..1_000 {
            if stop_announcer.load(Ordering::Relaxed) {
                break;
            }
            node_announcer
                .announce_capabilities(CapabilitySet::new())
                .await
                .expect("announce");
            // Inspect the announcement `announce_capabilities`
            // *actually published*, not the separate atomic
            // accessors (those are lock-free and can observe a
            // torn pair under concurrent mutation — that's
            // reader-side torn, not the publish-side race
            // cubic flagged). The stored announcement captures
            // its (class, reflex) pair under the publication
            // mutex, so it's a coherent snapshot by construction.
            let Some(ann) = node_announcer.local_announcement_for_test() else {
                continue;
            };
            let class = ann
                .capabilities
                .tags
                .iter()
                // Post-Phase-A.5.N.3: tags are typed (`Tag`) not
                // strings; render to wire form for the legacy
                // `NatClass::from_tag(&str)` API.
                .find_map(|t| NatClass::from_tag(&t.to_string()))
                .unwrap_or(NatClass::Unknown);
            let reflex = ann.reflex_addr;
            // Steady states:
            //   (Open, Some(override_addr))  — override active
            //   (Unknown, None)              — override cleared
            // Anything else is a torn read from the race.
            let coherent = (class == NatClass::Open && reflex == Some(override_addr))
                || (class == NatClass::Unknown && reflex.is_none());
            if !coherent {
                torn += 1;
            }
        }
        torn
    });

    let torn = announcer.await.expect("announcer task panicked");
    stop.store(true, Ordering::Relaxed);
    toggler.await.expect("toggler task panicked");

    // Any torn snapshot is a failure. Under the pre-fix bug
    // this test reliably produced torn reads on a stressed
    // runner. Post-fix, the publication mutex rules them out.
    assert_eq!(
        torn, 0,
        "observed {torn} torn (nat_class, reflex_addr) snapshots from \
         concurrent announces racing set/clear override — the three \
         traversal atomics must publish + read atomically as a group",
    );
}

/// Regression test for a cubic-flagged P2 bug: after
/// `set_reflex_override`, an immediate `announce_capabilities`
/// call could get coalesced by the `min_announce_interval`
/// rate limit if a prior announce had landed within the window —
/// so peers kept seeing the pre-override reflex until the next
/// scheduled announce.
///
/// The fix resets `last_announce_at` inside the setter so the
/// *next* announce broadcasts unconditionally. Callers that
/// want peers to see the new reflex still need to announce
/// themselves — the setter doesn't auto-broadcast — but that
/// announce is now guaranteed to land on the wire.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn set_reflex_override_resets_rate_limit_for_next_announce() {
    // Build A + B with a tight `min_announce_interval` so we
    // can reliably land two announces inside the window.
    let cfg_a = test_config().with_min_announce_interval(Duration::from_secs(5));
    let a = Arc::new(
        MeshNode::new(EntityKeypair::generate(), cfg_a)
            .await
            .expect("A"),
    );
    let b = build_mesh_plain().await;
    connect_pair(&a, &b).await;
    a.start();
    b.start();

    // First announce — pre-override state; sets
    // `last_announce_at`. Use a distinctive tag so we can tell
    // the two announces apart on B's index.
    a.announce_capabilities(CapabilitySet::new().add_tag("pre"))
        .await
        .expect("A announce 1");
    // Wait for B to index it.
    let a_id = a.node_id();
    for _ in 0..20 {
        if b.test_capability_fold_has(a_id) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    let external: SocketAddr = "203.0.113.88:55555".parse().unwrap();
    a.set_reflex_override(external);

    // Second announce — immediately after the setter, well
    // inside the 5 s rate-limit window. Under the pre-fix bug
    // this call would still update A's self-index but skip the
    // network broadcast, so B's index would NOT see the
    // override. With the fix, the setter reset
    // `last_announce_at = None`, so this announce broadcasts.
    a.announce_capabilities(CapabilitySet::new().add_tag("post"))
        .await
        .expect("A announce 2");

    // Wait for B to see the new announcement (new tag +
    // overridden reflex).
    let mut propagated = false;
    for _ in 0..40 {
        if b.test_capability_fold_has(a_id) {
            let caps = b.test_capability_fold_get(a_id);
            // Post-Phase-A.5.N.3: tags are typed; compare via
            // wire-string form. The synthetic announcement
            // injects "post" as a legacy tag.
            let has_post = caps.tags.iter().any(|t| t.to_string() == "post");
            let reflex = b.peer_reflex_addr(a_id);
            if has_post && reflex == Some(external) {
                propagated = true;
                break;
            }
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(
        propagated,
        "override did not propagate to B within the rate-limit window — \
         the setter's rate-limit reset regressed. Under the pre-fix bug, \
         the post-override announce was coalesced and peers kept seeing \
         the pre-override reflex.",
    );
}

/// RT-1 follow-up (cubic P2): a rate-limit-floor reset cancels any
/// pending trailing-edge flush. The reset's contract is "the next
/// explicit announce broadcasts unconditionally with the latest
/// caps"; letting the parked flush fire too would put a second
/// broadcast inside the fresh window. With the flush canceled and
/// no follow-up announce, the suppressed content must NOT reach
/// peers (pre-RT-1 semantics for the reset path).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn rate_limit_reset_cancels_pending_deferred_flush() {
    let cfg_a = test_config().with_min_announce_interval(Duration::from_secs(3));
    let a = Arc::new(
        MeshNode::new(EntityKeypair::generate(), cfg_a)
            .await
            .expect("MeshNode::new"),
    );
    let b = build_mesh_plain().await;
    connect_pair(&a, &b).await;
    // The flush task needs the owned-Arc path; start_arc must be
    // the first start call.
    a.start_arc();
    b.start();
    let a_id = a.node_id();

    // Leading edge broadcasts; the immediate second announce is
    // in-window and schedules the trailing-edge flush.
    a.announce_capabilities(CapabilitySet::new().add_tag("reset:v1"))
        .await
        .expect("announce v1");
    a.announce_capabilities(CapabilitySet::new().add_tag("reset:v2"))
        .await
        .expect("announce v2");

    // Reset the floor — must cancel the pending flush.
    a.set_reflex_override("203.0.113.99:4242".parse().unwrap());

    // Wait past the original window end plus slack: the canceled
    // flush must not deliver the suppressed v2.
    tokio::time::sleep(Duration::from_millis(3800)).await;
    let v2 = CapabilityFilter::new().require_tag("reset:v2");
    assert!(
        !b.find_nodes_by_filter(&v2).contains(&a_id),
        "the canceled trailing-edge flush still broadcast the \
         suppressed announce",
    );

    // Sanity: the documented follow-up announce is unconditional
    // and carries the newest caps right away.
    a.announce_capabilities(CapabilitySet::new().add_tag("reset:v3"))
        .await
        .expect("announce v3");
    let v3 = CapabilityFilter::new().require_tag("reset:v3");
    let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
    let mut seen = false;
    while tokio::time::Instant::now() < deadline {
        if b.find_nodes_by_filter(&v3).contains(&a_id) {
            seen = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    assert!(
        seen,
        "post-reset announce was not broadcast unconditionally"
    );
}

/// RT-1 review Finding 12: after a rate-limit-floor reset cancels a
/// pending flush, a FRESH in-window announce schedules a new deferral
/// (new `deferral_generation`) that flushes correctly. The task
/// orphaned by the reset must neither steal the new claim's slot nor
/// block it. (The bug's direct harm — a few-ms-early broadcast inside
/// the fresh window — is not observable end-to-end, so this guards the
/// claim-generation mechanism against breaking the legitimate flush.)
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn deferral_after_reset_still_flushes_new_content() {
    let window = Duration::from_secs(2);
    let cfg_a = test_config().with_min_announce_interval(window);
    let a = Arc::new(
        MeshNode::new(EntityKeypair::generate(), cfg_a)
            .await
            .expect("MeshNode::new"),
    );
    let b = build_mesh_plain().await;
    connect_pair(&a, &b).await;
    a.start_arc();
    b.start();
    let a_id = a.node_id();

    // v1 leading edge; v2 in-window (deferred — this deferral, gen N,
    // is the one the reset orphans).
    a.announce_capabilities(CapabilitySet::new().add_tag("gen:v1"))
        .await
        .expect("announce v1");
    a.announce_capabilities(CapabilitySet::new().add_tag("gen:v2"))
        .await
        .expect("announce v2");

    // Reset cancels the v2 deferral.
    a.set_reflex_override("203.0.113.7:4242".parse().unwrap());

    // v3 broadcasts unconditionally (opens a fresh window); wait until
    // B sees it so the next announce is squarely in-window.
    a.announce_capabilities(CapabilitySet::new().add_tag("gen:v3"))
        .await
        .expect("announce v3");
    let v3 = CapabilityFilter::new().require_tag("gen:v3");
    let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
    while tokio::time::Instant::now() < deadline && !b.find_nodes_by_filter(&v3).contains(&a_id) {
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    assert!(
        b.find_nodes_by_filter(&v3).contains(&a_id),
        "precondition: B never saw the unconditional v3 broadcast",
    );

    // v4 is in-window of the fresh window → a NEW deferral (gen N+1).
    a.announce_capabilities(CapabilitySet::new().add_tag("gen:v4"))
        .await
        .expect("announce v4");

    // The fresh deferral must flush v4 at the window end.
    let v4 = CapabilityFilter::new().require_tag("gen:v4");
    let deadline = tokio::time::Instant::now() + window + Duration::from_secs(2);
    let mut saw_v4 = false;
    while tokio::time::Instant::now() < deadline {
        if b.find_nodes_by_filter(&v4).contains(&a_id) {
            saw_v4 = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    assert!(
        saw_v4,
        "the fresh post-reset deferral never flushed — the orphaned \
         task blocked or stole its claim",
    );

    // The cancelled v2 deferral must never have leaked to the wire.
    let v2 = CapabilityFilter::new().require_tag("gen:v2");
    assert!(
        !b.find_nodes_by_filter(&v2).contains(&a_id),
        "cancelled deferral content leaked to a peer",
    );
}
