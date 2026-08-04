//! Live-wiring integration test for the Thunderdome gang-claim
//! scheduler: the `IslandTopologyFold` is mounted on `MeshNode`
//! alongside the capability + reservation folds, and the match→claim
//! pipeline reads the node's wired folds end-to-end.
//!
//! Peer announcements are applied directly to the node's folds (the
//! same effect the inbound `SUBPROTOCOL_FOLD` dispatch produces, since
//! the island fold is now registered in the node's `FoldRegistry`),
//! then the scheduler runs over `node.capability_fold()` +
//! `node.island_fold()` and claims through `node.reservation_fold()`.

use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use net::adapter::net::behavior::capability::CapabilitySet;
use net::adapter::net::behavior::fold::{
    CapabilityFilter, CapabilityFold, CapabilityMembership, CapabilityQuery, EnvelopeMeta,
    FoldKind, IslandQuery, IslandRecord, IslandTopologyFold, NodeState, ReservationAnnouncement,
    ReservationFold, ReservationQuery, ReservationState, SignedAnnouncement, UnitSet,
};
use net::adapter::net::behavior::gang::{
    match_islands, single_island_claim, ClaimOutcome, MatchCriteria, NumericFilter, SelectionPolicy,
};
use net::adapter::net::{EntityKeypair, MeshNode, MeshNodeConfig};

const PSK: [u8; 32] = [0x5a; 32];

async fn build_node() -> Arc<MeshNode> {
    let addr: SocketAddr = "127.0.0.1:0".parse().unwrap();
    let cfg = MeshNodeConfig::new(addr, PSK)
        .with_heartbeat_interval(Duration::from_millis(200))
        .with_session_timeout(Duration::from_secs(5))
        .with_handshake(3, Duration::from_secs(2));
    Arc::new(
        MeshNode::new(EntityKeypair::generate(), cfg)
            .await
            .expect("MeshNode::new"),
    )
}

/// House pattern: handshake `a` → `b` (and accept on `b`).
async fn connect_pair(a: &Arc<MeshNode>, b: &Arc<MeshNode>) {
    let a_id = a.node_id();
    let b_pub = *b.public_key();
    let b_addr = b.local_addr();
    let b_id = b.node_id();
    let b_clone = b.clone();
    let accept = tokio::spawn(async move { b_clone.accept(a_id).await });
    a.connect(b_addr, &b_pub, b_id).await.expect("connect");
    accept.await.expect("accept task").expect("accept");
}

fn now_us() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_micros() as u64
}

/// Sign a capability announcement (peer carries `tags`, Idle) and
/// apply it to `node`'s capability fold — the effect inbound dispatch
/// has.
fn prime_capability(node: &MeshNode, kp: &EntityKeypair, node_id: u64, tags: Vec<String>) {
    let membership = CapabilityMembership {
        class_hash: 0x67_70_75,
        tags,
        hardware: None,
        state: NodeState::Idle,
        region: None,
        price_quote: None,
        reflex_addr: None,
        allowed_nodes: Vec::new(),
        allowed_subnets: Vec::new(),
        allowed_groups: Vec::new(),
        metadata: BTreeMap::new(),
        owner: None,
    };
    let ann = SignedAnnouncement::sign(
        kp,
        CapabilityFold::KIND_ID,
        membership.class_hash,
        node_id,
        1,
        EnvelopeMeta::default(),
        membership,
    )
    .expect("sign cap");
    node.capability_fold().apply(ann).expect("apply cap");
}

/// Sign an island record (hosted by `node_id`) and apply it to
/// `node`'s island fold.
fn prime_island(node: &MeshNode, kp: &EntityKeypair, node_id: u64, id: u64, load: f32) {
    let record = IslandRecord {
        id,
        units: UnitSet::new(vec![0, 1, 2, 3, 4, 5, 6, 7]),
        host: node_id,
        capabilities: vec!["model:a1".into()],
        load,
        p50_latency_us: 1_200,
    };
    let ann = SignedAnnouncement::sign(
        kp,
        IslandTopologyFold::KIND_ID,
        0,
        node_id,
        1,
        EnvelopeMeta::default(),
        record,
    )
    .expect("sign island");
    node.island_fold().apply(ann).expect("apply island");
}

#[tokio::test]
async fn island_fold_is_wired_and_scheduler_matches_and_claims_over_node_folds() {
    let node = build_node().await;

    // Two GPU peers fold into the node's capability + island folds.
    let peer_a = EntityKeypair::generate();
    let peer_b = EntityKeypair::generate();
    let na = peer_a.entity_id().node_id();
    let nb = peer_b.entity_id().node_id();
    prime_capability(&node, &peer_a, na, vec!["gpu:h100".into()]);
    prime_capability(&node, &peer_b, nb, vec!["gpu:h100".into()]);
    prime_island(&node, &peer_a, na, 0xA0, 0.7);
    prime_island(&node, &peer_b, nb, 0xB0, 0.2);

    let criteria = MatchCriteria {
        capability: CapabilityQuery::Composite(CapabilityFilter {
            tags_all: vec!["gpu:h100".into()],
            ..Default::default()
        }),
        numeric: NumericFilter {
            min_units: 8,
            ..Default::default()
        },
        selection: SelectionPolicy::LeastLoaded,
        prefer_capability: None,
    };

    // The scheduler reads the node's wired folds: both islands match,
    // least-loaded first (B at 0.2 before A at 0.7).
    let order = match_islands(
        node.capability_fold(),
        node.island_fold(),
        &criteria,
        &std::collections::HashSet::new(),
    );
    assert_eq!(
        order,
        vec![0xB0, 0xA0],
        "scheduler ranks over the node's island fold"
    );

    // Claim the top island through the node's reservation fold.
    let claimant = EntityKeypair::generate();
    let cn = claimant.entity_id().node_id();
    let deadline = now_us() + 60_000_000;
    let got = single_island_claim(
        node.reservation_fold(),
        &claimant,
        cn,
        1,
        order[0],
        deadline,
    )
    .expect("claim");
    assert_eq!(got, ClaimOutcome::Won);
    assert_eq!(
        node.reservation_fold()
            .query(net::adapter::net::behavior::fold::ReservationQuery::State(
                0xB0
            ))[0]
            .1
            .holder(),
        Some(cn),
    );
}

/// MeshOS ↔ Scheduler Projection 4 end-to-end on the public node API:
/// `set_liveness_down` makes `MeshNode::match_islands` prune a downed
/// host's islands, and clearing it restores them — no fold mutation.
#[tokio::test]
async fn set_liveness_down_prunes_a_dead_hosts_islands_from_match_islands() {
    let node = build_node().await;
    let peer_a = EntityKeypair::generate();
    let peer_b = EntityKeypair::generate();
    let na = peer_a.entity_id().node_id();
    let nb = peer_b.entity_id().node_id();
    prime_capability(&node, &peer_a, na, vec!["gpu:h100".into()]);
    prime_capability(&node, &peer_b, nb, vec!["gpu:h100".into()]);
    prime_island(&node, &peer_a, na, 0xA0, 0.7);
    prime_island(&node, &peer_b, nb, 0xB0, 0.2);

    let criteria = MatchCriteria {
        capability: CapabilityQuery::Composite(CapabilityFilter {
            tags_all: vec!["gpu:h100".into()],
            ..Default::default()
        }),
        numeric: NumericFilter {
            min_units: 8,
            ..Default::default()
        },
        selection: SelectionPolicy::LeastLoaded,
        prefer_capability: None,
    };

    // Default (nothing down): both islands, least-loaded first.
    assert_eq!(node.match_islands(&criteria), vec![0xB0, 0xA0]);

    // Mark host B down → its island 0xB0 is pruned from the public match.
    node.set_liveness_down([nb].into_iter().collect());
    assert_eq!(
        node.match_islands(&criteria),
        vec![0xA0],
        "a downed host's island leaves the candidate set",
    );

    // Clear the down-set → both islands return.
    node.set_liveness_down(std::collections::HashSet::new());
    assert_eq!(node.match_islands(&criteria), vec![0xB0, 0xA0]);
}

/// 2-node broadcast: a host publishes its island topology and it
/// converges into a connected peer's island fold over the wire —
/// proving the island fold is registered in the live dispatch path
/// (`publish_island_topology` → `SUBPROTOCOL_FOLD` → peer's fold).
///
/// The host first announces capabilities: the `SUBPROTOCOL_FOLD`
/// dispatch keys on `peer_entity_ids`, which the receiver populates
/// from the publisher's capability announcement (the entity
/// bootstrap). We then re-publish the island each poll so the
/// one-shot broadcast can't race ahead of that bootstrap.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn island_topology_broadcasts_to_a_connected_peer() {
    let host = build_node().await;
    let peer = build_node().await;
    connect_pair(&host, &peer).await;
    host.start();
    peer.start();

    let host_id = host.node_id();
    // Bootstrap: the peer learns host's EntityId from this.
    host.announce_capabilities(CapabilitySet::new())
        .await
        .expect("announce");

    let record = IslandRecord {
        id: 0xC0,
        units: UnitSet::new(vec![0, 1, 2, 3]),
        host: 0, // overwritten with host.node_id() by publish
        capabilities: vec!["model:a1".into()],
        load: 0.42,
        p50_latency_us: 900,
    };
    let peer_view = peer.clone();
    let mut converged = false;
    for _ in 0..50 {
        host.publish_island_topology(record.clone())
            .await
            .expect("publish island");
        tokio::time::sleep(Duration::from_millis(100)).await;
        if !peer_view
            .island_fold()
            .query(IslandQuery::Get(0xC0))
            .is_empty()
        {
            converged = true;
            break;
        }
    }
    assert!(converged, "peer should fold the host's island announcement");

    let row = peer.island_fold().query(IslandQuery::Get(0xC0));
    assert_eq!(row[0].1.host, host_id, "host stamped as the announcer");
    assert_eq!(row[0].1.load, 0.42);
}

/// A node that hosts GPU islands must see them in its OWN island fold
/// after `publish_island_topology` — the broadcast only reaches peers,
/// but the node's own scheduler (`match_islands` / `claim_island`)
/// reads the local fold. Without the self-apply a co-located
/// scheduler+host could never schedule onto its own hardware (review #1).
#[tokio::test]
async fn publish_island_topology_self_indexes_for_the_local_scheduler() {
    let node = build_node().await;
    let host_id = node.node_id();

    let record = IslandRecord {
        id: 0xE0,
        units: UnitSet::new(vec![0, 1, 2, 3, 4, 5, 6, 7]),
        host: 0, // overwritten with node.node_id() by publish
        capabilities: vec!["model:a1".into()],
        load: 0.1,
        p50_latency_us: 800,
    };
    node.publish_island_topology(record).await.expect("publish");

    // Visible locally with no peer and no wire round-trip.
    let row = node.island_fold().query(IslandQuery::Get(0xE0));
    assert_eq!(
        row.len(),
        1,
        "self-published island is visible in the node's own fold"
    );
    assert_eq!(row[0].1.host, host_id, "host stamped as this node");
    assert_eq!(row[0].1.load, 0.1);
}

/// Node-level claim round-trip: a scheduler node folds a GPU peer's
/// capability + island (primed here as already-converged), runs
/// `claim_island` against its OWN folds, and the resulting
/// reservation broadcasts to a connected peer's reservation fold.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn claim_island_reserves_and_broadcasts_to_peer() {
    let scheduler = build_node().await;
    let peer = build_node().await;
    connect_pair(&scheduler, &peer).await;
    scheduler.start();
    peer.start();

    // Bootstrap: the peer learns the scheduler's EntityId so the
    // scheduler's reservation broadcasts will dispatch into the
    // peer's fold.
    scheduler
        .announce_capabilities(CapabilitySet::new())
        .await
        .expect("announce");

    // A GPU peer's capability + island already folded into the
    // scheduler's view (fold→fold convergence is exercised by the
    // broadcast test above).
    let gpu = EntityKeypair::generate();
    let gn = gpu.entity_id().node_id();
    prime_capability(&scheduler, &gpu, gn, vec!["gpu:h100".into()]);
    prime_island(&scheduler, &gpu, gn, 0xD0, 0.1);

    let criteria = MatchCriteria {
        capability: CapabilityQuery::Composite(CapabilityFilter {
            tags_all: vec!["gpu:h100".into()],
            ..Default::default()
        }),
        numeric: NumericFilter {
            min_units: 8,
            ..Default::default()
        },
        selection: SelectionPolicy::LeastLoaded,
        prefer_capability: None,
    };

    // First claim establishes the local hold (optimistic AP view).
    let claimed = scheduler
        .claim_island(&criteria, now_us() + 60_000_000)
        .await
        .expect("claim_island");
    assert_eq!(claimed, Some(0xD0));
    let sched_id = scheduler.node_id();
    assert_eq!(
        scheduler
            .reservation_fold()
            .query(ReservationQuery::State(0xD0))[0]
            .1
            .holder(),
        Some(sched_id),
    );

    // Re-broadcast the reservation (a legal self-extend) each poll so
    // it converges on the peer once the entity bootstrap has landed.
    let peer_view = peer.clone();
    let mut converged = false;
    for _ in 0..50 {
        scheduler
            .reserve_island(0xD0, now_us() + 60_000_000)
            .await
            .expect("reserve");
        tokio::time::sleep(Duration::from_millis(100)).await;
        if peer_view
            .reservation_fold()
            .query(ReservationQuery::State(0xD0))
            .first()
            .and_then(|(_, s)| s.holder())
            == Some(sched_id)
        {
            converged = true;
            break;
        }
    }
    assert!(
        converged,
        "peer should see the scheduler's reservation converge"
    );
}

/// Sign `state` for `island` under `kp` and apply it directly to
/// `node`'s reservation fold — the effect an inbound wire announcement
/// from that publisher has, used to pre-converge a foreign holder.
fn prime_reservation(
    node: &MeshNode,
    kp: &EntityKeypair,
    node_id: u64,
    island: u64,
    state: ReservationState,
    generation: u64,
) {
    let ann = SignedAnnouncement::sign(
        kp,
        ReservationFold::KIND_ID,
        0,
        node_id,
        generation,
        EnvelopeMeta::default(),
        ReservationAnnouncement {
            resource_id: island,
            state,
        },
    )
    .expect("sign reservation");
    node.reservation_fold().apply(ann).expect("apply res");
}

fn holder_of_island(node: &MeshNode, island: u64) -> Option<u64> {
    node.reservation_fold()
        .query(ReservationQuery::State(island))
        .first()
        .and_then(|(_, s)| s.holder())
}

/// **Only locally accepted reservation-fold transitions are publishable.**
///
/// A losing reserve used to be signed, rejected by the local CAS, and
/// broadcast anyway — so the claimant kept `A` held by `H` while an
/// observer installed the losing claimant. The node published a
/// reservation state it did not itself believe. ICB-4's W4 witnessed
/// exactly that and disclaimed it as orthogonal; it is not orthogonal,
/// and this pins the fix.
///
/// Everything else about a rejected attempt is deliberately unchanged
/// and asserted here: still applied, still metered
/// (`applies_rejected`), still generation-consuming. Only the wire
/// output is suppressed.
///
/// # Not asserted: audit emission
///
/// `ReservationFold` does not implement [`FoldKind::audit_event`], so
/// it emits nothing to an installed sink — and neither does any other
/// production fold in the crate (only a test fold in `fold/tests.rs`
/// does). An "a Rejected audit event is still emitted" assertion would
/// therefore be vacuous in both directions, so it is left out rather
/// than written to pass for the wrong reason. The durable guarantee is
/// the one the metric pins: `Fold::apply` still ran, and audit
/// emission is downstream of that inside `apply`, so it would follow
/// automatically if the fold ever gains an `audit_event` impl.
///
/// [`FoldKind::audit_event`]: net::adapter::net::behavior::fold::FoldKind::audit_event
///
/// # Why the free island is in the fixture
///
/// The load-bearing assertion is a negative — "`O` never sees `A` held
/// by `C`" — and a negative needs a positive control on the same link,
/// or a dead session, a missed entity bootstrap, or a too-short settle
/// window would all pass it vacuously. So `C` walks past held `A` onto
/// free `B` in the same `claim_island` call, and `B`'s reservation is
/// required to converge on `O`. Once it has, a suppressed `A`
/// broadcast has demonstrably had at least as long to arrive as the
/// `B` broadcast that did.
///
/// # Why `A` is pre-converged on `C` ONLY
///
/// Priming `Reserved{H}` onto the observer as well — the obvious way
/// to write "both nodes still see `H`" — makes the test insensitive to
/// the bug it exists for. `ReservationFold::merge` already refuses a
/// foreign publisher's steal of a *fresh* reservation, so an observer
/// holding `Reserved{H}` rejects the losing `Reserved{C}` on its own
/// and the assertion passes whether or not the broadcast was sent.
/// Verified: with the unconditional broadcast restored, that version
/// of this test still passed.
///
/// So the observer starts with **no entry** for `A`, exactly as in
/// ICB-4's fixture — which is why W4 saw a divergence there. An
/// arriving losing announcement therefore `Insert`s cleanly, and
/// "`O` has no entry for `A`" is a true zero-announcements witness.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_locally_rejected_reservation_is_never_broadcast() {
    const ISLAND_A: u64 = 0xE0; // held by H, ranks first (lower load)
    const ISLAND_B: u64 = 0xE1; // free fallback

    let claimant = build_node().await;
    let observer = build_node().await;
    connect_pair(&claimant, &observer).await;
    claimant.start();
    observer.start();

    // Bootstrap so the observer knows the claimant's EntityId and will
    // dispatch its fold broadcasts.
    claimant
        .announce_capabilities(CapabilitySet::new())
        .await
        .expect("announce");

    // A GPU host publishing two islands, pre-converged into the
    // claimant's view.
    let gpu = EntityKeypair::generate();
    let gn = gpu.entity_id().node_id();
    prime_capability(&claimant, &gpu, gn, vec!["gpu:h100".into()]);
    prime_island(&claimant, &gpu, gn, ISLAND_A, 0.1);
    prime_island(&claimant, &gpu, gn, ISLAND_B, 0.2);

    // `H` — a pre-existing holder identity. `A` is pre-converged as
    // `Reserved{H}` on the CLAIMANT ONLY (see the doc comment), with a
    // deadline far enough out that no takeover is legal.
    let h = EntityKeypair::generate();
    let h_id = h.entity_id().node_id();
    prime_reservation(
        &claimant,
        &h,
        h_id,
        ISLAND_A,
        ReservationState::Reserved {
            holder: h_id,
            until_unix_us: now_us() + 600_000_000,
        },
        1,
    );
    assert_eq!(holder_of_island(&claimant, ISLAND_A), Some(h_id));
    assert_eq!(
        holder_of_island(&observer, ISLAND_A),
        None,
        "the observer must start with no entry for A, or the negative below is vacuous",
    );

    let criteria = MatchCriteria {
        capability: CapabilityQuery::Composite(CapabilityFilter {
            tags_all: vec!["gpu:h100".into()],
            ..Default::default()
        }),
        numeric: NumericFilter {
            min_units: 8,
            ..Default::default()
        },
        selection: SelectionPolicy::LeastLoaded,
        prefer_capability: None,
    };

    let rejected_before = claimant.reservation_fold().stats().applies_rejected;

    // A ranks first (load 0.1 < 0.2) and is held → Lost, walk to B.
    let claimed = claimant
        .claim_island(&criteria, now_us() + 60_000_000)
        .await
        .expect("claim_island");
    assert_eq!(
        claimed,
        Some(ISLAND_B),
        "the walk must reject held A and win free B",
    );

    // The rejected attempt stayed local, and stayed observable.
    let rejected_delta = claimant.reservation_fold().stats().applies_rejected - rejected_before;
    assert_eq!(
        rejected_delta, 1,
        "the losing attempt on A must still be applied and metered — suppressing \
         the broadcast must not turn into skipping the CAS",
    );
    // Positive control: B's WINNING reservation must converge on the
    // observer. Re-broadcast per poll (a legal self-extend) so the
    // entity bootstrap has time to land, matching the house pattern.
    let claimant_id = claimant.node_id();
    let mut converged = false;
    for _ in 0..50 {
        claimant
            .reserve_island(ISLAND_B, now_us() + 60_000_000)
            .await
            .expect("reserve B");
        tokio::time::sleep(Duration::from_millis(100)).await;
        if holder_of_island(&observer, ISLAND_B) == Some(claimant_id) {
            converged = true;
            break;
        }
    }
    assert!(
        converged,
        "positive control failed: the observer never saw the WINNING reservation on B, \
         so the negative assertion below would be vacuous",
    );

    // The negative, now meaningful: nothing for A ever reached the
    // observer, over a window in which a real broadcast demonstrably
    // did. An arriving `Reserved{C}` would have `Insert`ed here.
    assert_eq!(
        holder_of_island(&observer, ISLAND_A),
        None,
        "the observer received a reservation announcement for A — a locally rejected \
         reservation must never be replicated (pre-fix it installed the LOSING claimant)",
    );
    // And the claimant's own view is unchanged: it still believes H
    // holds A, which is the belief the suppressed broadcast contradicted.
    assert_eq!(
        holder_of_island(&claimant, ISLAND_A),
        Some(h_id),
        "the claimant must still see A held by H",
    );
}

/// The same invariant on the release path, which shares
/// `apply_and_broadcast_reservation`: a `Free` the local fold rejects
/// must not be published either.
///
/// `release_island` gates on holder identity first, so the ordinary
/// non-holder release returns `Lost` without reaching the fold at all
/// (review #5) — a *stronger* guarantee than the invariant, and
/// asserted as such: no fold apply of any outcome, and neither view
/// moves. Audit emission is not asserted, for the reason given on
/// [`a_locally_rejected_reservation_is_never_broadcast`].
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_non_holder_release_touches_neither_the_fold_nor_the_wire() {
    const ISLAND: u64 = 0xE7;

    let claimant = build_node().await;
    let observer = build_node().await;
    connect_pair(&claimant, &observer).await;
    claimant.start();
    observer.start();
    claimant
        .announce_capabilities(CapabilitySet::new())
        .await
        .expect("announce");

    // Held by H on both nodes; the claimant is NOT the holder.
    let h = EntityKeypair::generate();
    let h_id = h.entity_id().node_id();
    for node in [&claimant, &observer] {
        prime_reservation(
            node,
            &h,
            h_id,
            ISLAND,
            ReservationState::Reserved {
                holder: h_id,
                until_unix_us: now_us() + 600_000_000,
            },
            1,
        );
    }

    let before = claimant.reservation_fold().stats();

    assert_eq!(
        claimant.release_island(ISLAND).await.expect("release"),
        ClaimOutcome::Lost,
        "a non-holder release must report Lost",
    );

    let after = claimant.reservation_fold().stats();
    assert_eq!(
        (
            after.applies_inserted,
            after.applies_replaced,
            after.applies_rejected
        ),
        (
            before.applies_inserted,
            before.applies_replaced,
            before.applies_rejected
        ),
        "the holder gate must short-circuit before the fold apply",
    );

    // Settle, then confirm neither view moved.
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(holder_of_island(&observer, ISLAND), Some(h_id));
    assert_eq!(holder_of_island(&claimant, ISLAND), Some(h_id));
}
