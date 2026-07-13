//! SI-2a (SENSING_INTEREST_COALESCING_PLAN v4.3): the first slice
//! wiring the sensing plane onto live `MeshNode` dispatch —
//! `SUBPROTOCOL_SENSING_INTEREST` (0x0C02) frames land in the
//! receiver's per-hop [`InterestTable`] under the authenticated
//! session identity.
//!
//! Topology: two real nodes, A ↔ B. A is the consumer (and, in v1
//! owner-root terms, the OWNER — its entity commitment is the fleet
//! root); B is the provider target, configured with
//! `with_sensing_owner_root(root(A))` so sessions proving A's root
//! are in-scope (plan §4.10). A's `register_sensing_interest` puts
//! the LOCAL row in A's own table and propagates a coalesced
//! `ProviderRegistration` upstream toward `next_hop(B)` — which is B
//! itself here — over the encrypted per-peer subprotocol path.
//!
//! Timer parking (the route_withdraw.rs trick): session timeouts are
//! 10 s, so failure detection (~30 s) and dead-peer eviction
//! (300 s) cannot fire inside any test window; heartbeats tick at
//! 100 ms so the SENSING SWEEP — the only row-expiry path — runs
//! promptly. Any row that disappears in-window was swept, and any
//! row that appears was dispatched.
//!
//! UDP delivery is best-effort, so senders retry in a poll loop —
//! re-registration is a soft-state refresh, so retries are
//! semantically free (and `register_sensing_interest` documents the
//! anti-entropy re-send exactly for this).
//!
//! Run: `cargo test --features net --test sensing_dispatch`

#![cfg(feature = "net")]

mod common;
use common::*;

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use net::adapter::net::behavior::capability::CapabilitySet;
use net::adapter::net::behavior::sensing::{
    AudienceScopeCommitment, CanonicalConstraints, CapabilityId, DisclosureClass, DownstreamId,
    InterestSpec, ProviderInterestKey, ProviderSelector, RegisterOutcome, ResultMode,
    SensingCounters, WorkLatencyEnvelope,
};
use net::adapter::net::{
    EntityKeypair, MeshNode, MeshNodeConfig, SensingRegistrationError, SocketBufferConfig,
};
use net::adapter::Adapter;

const TTL: Duration = Duration::from_secs(30);
const D: Duration = Duration::from_millis(100);

fn base_config() -> MeshNodeConfig {
    let addr: SocketAddr = "127.0.0.1:0".parse().unwrap();
    let mut cfg = MeshNodeConfig::new(addr, CHAOS_PSK)
        .with_heartbeat_interval(Duration::from_millis(100))
        .with_session_timeout(Duration::from_secs(10))
        .with_handshake(3, Duration::from_secs(2));
    cfg.socket_buffers = SocketBufferConfig {
        send_buffer_size: CHAOS_BUFFER_SIZE,
        recv_buffer_size: CHAOS_BUFFER_SIZE,
    };
    cfg
}

/// A node built from an explicit keypair (the tests need the entity
/// to compute owner-root commitments before the peer node's config
/// exists).
async fn build_with_keypair(keypair: EntityKeypair, cfg: MeshNodeConfig) -> Arc<MeshNode> {
    Arc::new(MeshNode::new(keypair, cfg).await.expect("MeshNode::new"))
}

/// Handshake (A initiates — the started node must be the handshake
/// initiator, per the route_withdraw.rs pattern), start both, then
/// exchange empty capability announcements so each side TOFU-pins
/// the other's `EntityId` — the sensing dispatch arm derives the
/// sender's owner root from that pin and drops frames from unpinned
/// sessions.
async fn bring_up(a: &Arc<MeshNode>, b: &Arc<MeshNode>) {
    connect_pair(a, b).await;
    a.start();
    b.start();
    a.announce_capabilities(CapabilitySet::new())
        .await
        .expect("A announce");
    b.announce_capabilities(CapabilitySet::new())
        .await
        .expect("B announce");
    let a_id = a.node_id();
    let b_id = b.node_id();
    await_condition(Duration::from_secs(5), "entity pins established", || {
        a.peer_entity_id(b_id).is_some() && b.peer_entity_id(a_id).is_some()
    })
    .await;
}

/// An owner-scoped interest spec targeting `provider` explicitly
/// (candidate resolution is SI-2b; SI-2a names the branch). The
/// `marker` constraint value differentiates interest identities.
fn spec_for(owner: AudienceScopeCommitment, provider: u64, marker: &str) -> InterestSpec {
    InterestSpec {
        capability_id: CapabilityId::new("print.document"),
        constraints: CanonicalConstraints::from_entries([("media", marker)]).unwrap(),
        work_latency: WorkLatencyEnvelope::start_within(Duration::from_secs(5)),
        providers: ProviderSelector::Node(provider),
        result_mode: ResultMode::Any,
        disclosure_class: DisclosureClass::Owner,
        audience: owner,
    }
}

fn counter_snapshot(counters: &SensingCounters) -> [u64; 4] {
    [
        SensingCounters::get(&counters.invalid_constraints),
        SensingCounters::get(&counters.protocol_invalid),
        SensingCounters::get(&counters.cadence_refusals),
        SensingCounters::get(&counters.scope_refusals),
    ]
}

/// (a) Flag ON: A's registration for a Node(B) interest lands in
/// B's table as a `Peer(A)` row under the validated key with the
/// session-proven root — and A's own table holds the LOCAL row.
#[tokio::test]
async fn provider_registration_lands_a_peer_row_with_proven_root() {
    let a_kp = EntityKeypair::generate();
    let owner = AudienceScopeCommitment::owner_root(a_kp.entity_id());
    let a = build_with_keypair(a_kp, base_config().with_sensing_coalescing(true)).await;
    let b = build_with_keypair(
        EntityKeypair::generate(),
        base_config()
            .with_sensing_coalescing(true)
            .with_sensing_owner_root(owner),
    )
    .await;
    bring_up(&a, &b).await;

    let a_id = a.node_id();
    let b_id = b.node_id();
    let spec = spec_for(owner, b_id, "a4");
    let key = ProviderInterestKey::new(spec.key(), b_id);

    // Retry loop: each call refreshes A's LOCAL row and re-sends the
    // coalesced aggregate upstream (anti-entropy over UDP).
    let mut landed = false;
    for _ in 0..40 {
        let outcome = a
            .register_sensing_interest(&spec, b_id, D, TTL)
            .expect("A's local registration is in-scope");
        assert!(matches!(outcome, RegisterOutcome::Registered(_)));
        if poll_until(Duration::from_millis(250), || {
            !b.sensing_downstreams(&key).is_empty()
        })
        .await
        {
            landed = true;
            break;
        }
    }
    assert!(landed, "B never gained a row for A's interest");

    // The row is keyed under the VALIDATED key (re-derived digest),
    // attributed to the authenticated session peer A, and carries
    // the session-proven root — never a wire-claimed one.
    assert_eq!(b.sensing_downstreams(&key), vec![DownstreamId::Peer(a_id)]);
    let row = b
        .sensing_downstream_entry(&key, DownstreamId::Peer(a_id))
        .expect("row present");
    assert_eq!(row.owner_root, owner, "the proven root is stored");
    assert_eq!(row.requested_sample_interval, D);
    assert_eq!(b.sensing_interest_count(), 1, "exactly one branch key");

    // A holds its own LOCAL row for the same key.
    assert_eq!(a.sensing_downstreams(&key), vec![DownstreamId::Local]);

    // Nothing on this happy path was protocol-invalid at B.
    let counters = b.sensing_counters();
    assert_eq!(SensingCounters::get(&counters.protocol_invalid), 0);
    assert_eq!(SensingCounters::get(&counters.scope_refusals), 0);

    a.shutdown().await.expect("shutdown A");
    b.shutdown().await.expect("shutdown B");
}

/// (b) Flag OFF — the DEFAULT — is inert: identical traffic leaves
/// the receiver's table empty and moves ZERO counters (the frame
/// drops before decode, exactly like an unknown subprotocol id).
/// Pins both the default and the dark-launch invariant.
#[tokio::test]
async fn disabled_receiver_is_inert_and_moves_no_counters() {
    // Pin the plan §5 default: the plane ships dark.
    let addr: SocketAddr = "127.0.0.1:0".parse().unwrap();
    assert!(
        !MeshNodeConfig::new(addr, CHAOS_PSK).enable_sensing_coalescing,
        "enable_sensing_coalescing must default to false",
    );

    let a_kp = EntityKeypair::generate();
    let owner = AudienceScopeCommitment::owner_root(a_kp.entity_id());
    // A sends (flag ON); B is a stock default-config node (flag
    // OFF) — the owner root knob is irrelevant on a dark plane and
    // deliberately left unset.
    let a = build_with_keypair(a_kp, base_config().with_sensing_coalescing(true)).await;
    let b = build_with_keypair(EntityKeypair::generate(), base_config()).await;
    bring_up(&a, &b).await;

    let b_id = b.node_id();
    let spec = spec_for(owner, b_id, "a4");

    // A's local registration itself is refused when A is dark —
    // pin the emit-side gate on a third, default-config node.
    let dark = build_with_keypair(EntityKeypair::generate(), base_config()).await;
    assert_eq!(
        dark.register_sensing_interest(&spec, b_id, D, TTL),
        Err(SensingRegistrationError::Disabled),
    );

    // Hammer B with real registrations for a while.
    for _ in 0..10 {
        a.register_sensing_interest(&spec, b_id, D, TTL)
            .expect("A registers");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    tokio::time::sleep(Duration::from_millis(500)).await;

    assert!(
        b.sensing_table_is_empty(),
        "a dark receiver must never gain sensing rows",
    );
    assert_eq!(
        counter_snapshot(&b.sensing_counters()),
        [0, 0, 0, 0],
        "a dark receiver must move zero sensing counters",
    );
    assert_eq!(b.sensing_over_cap_refusals(), 0);

    a.shutdown().await.expect("shutdown A");
    b.shutdown().await.expect("shutdown B");
    dark.shutdown().await.expect("shutdown dark");
}

/// (c) A wire scope claim the session does not back is rejected as
/// protocol-invalid input: no row, security counter bumped
/// (plan §4.10 — the claim is cross-checked, never load-bearing).
#[tokio::test]
async fn unbacked_scope_claim_is_rejected_as_protocol_invalid() {
    // A operates under a FOREIGN root (an owner entity that is not
    // A's own), so its frames honestly claim that root — but A's
    // SESSION proves only A's entity commitment. B is configured to
    // serve A's entity root, so the ONLY failing check is the wire
    // claim vs. the session (WireClaimMismatch), isolated from the
    // cross-root refusal.
    let a_kp = EntityKeypair::generate();
    let a_session_root = AudienceScopeCommitment::owner_root(a_kp.entity_id());
    let foreign = AudienceScopeCommitment::owner_root(EntityKeypair::generate().entity_id());
    let a = build_with_keypair(
        a_kp,
        base_config()
            .with_sensing_coalescing(true)
            .with_sensing_owner_root(foreign),
    )
    .await;
    let b = build_with_keypair(
        EntityKeypair::generate(),
        base_config()
            .with_sensing_coalescing(true)
            .with_sensing_owner_root(a_session_root),
    )
    .await;
    bring_up(&a, &b).await;

    let b_id = b.node_id();
    // The spec's audience is the foreign root — internally
    // consistent at A (its local root IS the foreign root), so the
    // digest re-derivation passes at B and scope validation is what
    // must refuse.
    let spec = spec_for(foreign, b_id, "a4");

    let counters = b.sensing_counters();
    let mut rejected = false;
    for _ in 0..40 {
        a.register_sensing_interest(&spec, b_id, D, TTL)
            .expect("locally in-scope at A");
        if poll_until(Duration::from_millis(250), || {
            SensingCounters::get(&counters.protocol_invalid) >= 1
        })
        .await
        {
            rejected = true;
            break;
        }
    }
    assert!(rejected, "B never rejected the unbacked scope claim");
    assert!(
        SensingCounters::get(&counters.scope_refusals) >= 1,
        "scope refusal counter moved",
    );
    assert!(
        b.sensing_table_is_empty(),
        "a rejected registration must record nothing",
    );

    a.shutdown().await.expect("shutdown A");
    b.shutdown().await.expect("shutdown B");
}

/// (d) Per-peer cap: cap+1 distinct interests from one peer — the
/// table refuses the overflow (`OverCap`, surfaced through the
/// dispatch tally) and the row count holds at exactly the cap,
/// while refreshes of admitted rows keep succeeding.
#[tokio::test]
async fn per_peer_cap_bounds_inbound_interests() {
    const CAP: usize = 4;
    let a_kp = EntityKeypair::generate();
    let owner = AudienceScopeCommitment::owner_root(a_kp.entity_id());
    let a = build_with_keypair(a_kp, base_config().with_sensing_coalescing(true)).await;
    let b = build_with_keypair(
        EntityKeypair::generate(),
        base_config()
            .with_sensing_coalescing(true)
            .with_sensing_owner_root(owner)
            .with_max_interests_per_peer(CAP),
    )
    .await;
    bring_up(&a, &b).await;

    let b_id = b.node_id();
    let specs: Vec<InterestSpec> = (0..=CAP)
        .map(|i| spec_for(owner, b_id, &format!("m{i}")))
        .collect();

    // Drive all CAP+1 registrations until B holds CAP rows AND has
    // refused at least one over-cap registration.
    let mut converged = false;
    for _ in 0..40 {
        for spec in &specs {
            a.register_sensing_interest(spec, b_id, D, TTL)
                .expect("in-scope at A");
        }
        if poll_until(Duration::from_millis(250), || {
            b.sensing_interest_count() == CAP && b.sensing_over_cap_refusals() >= 1
        })
        .await
        {
            converged = true;
            break;
        }
    }
    assert!(
        converged,
        "B never converged to cap rows + a surfaced OverCap refusal \
         (rows: {}, over-cap: {})",
        b.sensing_interest_count(),
        b.sensing_over_cap_refusals(),
    );

    // Keep hammering: the count must hold at the cap — refreshes of
    // admitted rows succeed, new rows stay refused.
    for _ in 0..5 {
        for spec in &specs {
            a.register_sensing_interest(spec, b_id, D, TTL)
                .expect("in-scope at A");
        }
        tokio::time::sleep(Duration::from_millis(150)).await;
        assert_eq!(
            b.sensing_interest_count(),
            CAP,
            "row count must never exceed the per-peer cap",
        );
    }
    // Exactly one of the CAP+1 keys is absent.
    let present = specs
        .iter()
        .filter(|spec| {
            !b.sensing_downstreams(&ProviderInterestKey::new(spec.key(), b_id))
                .is_empty()
        })
        .count();
    assert_eq!(present, CAP, "exactly cap keys admitted");

    a.shutdown().await.expect("shutdown A");
    b.shutdown().await.expect("shutdown B");
}

/// (e) TTL expiry: a registered row with a short ttl and no refresh
/// is swept off on the heartbeat tick and the table returns to
/// empty. Causality: `InterestTable::expire` runs ONLY on the
/// heartbeat sweep, and every other remover (peer Deregister
/// frames, failure eviction) is parked/absent — so the emptying IS
/// the sweep.
#[tokio::test]
async fn short_ttl_rows_are_swept_on_the_heartbeat_tick() {
    const SHORT_TTL: Duration = Duration::from_millis(300);
    let a_kp = EntityKeypair::generate();
    let owner = AudienceScopeCommitment::owner_root(a_kp.entity_id());
    let a = build_with_keypair(a_kp, base_config().with_sensing_coalescing(true)).await;
    // B's heartbeat (and hence its sweep) ticks at 100 ms — well
    // inside the row's 300 ms lifetime.
    let b = build_with_keypair(
        EntityKeypair::generate(),
        base_config()
            .with_sensing_coalescing(true)
            .with_sensing_owner_root(owner),
    )
    .await;
    bring_up(&a, &b).await;

    let b_id = b.node_id();
    let spec = spec_for(owner, b_id, "a4");
    let key = ProviderInterestKey::new(spec.key(), b_id);

    // Land the row (the frame carries SHORT_TTL; B caps inbound
    // ttls at its 30 s config bound, so the SHORTER request rules).
    let mut landed = false;
    for _ in 0..40 {
        a.register_sensing_interest(&spec, b_id, D, SHORT_TTL)
            .expect("in-scope at A");
        if poll_until(Duration::from_millis(200), || {
            !b.sensing_downstreams(&key).is_empty()
        })
        .await
        {
            landed = true;
            break;
        }
    }
    assert!(landed, "precondition: the row never landed at B");

    // No refresh: two missed ttl/2 refreshes later the sweep drops
    // the row and the entry (zero idle cost). Generous CI budget.
    assert!(
        poll_until(Duration::from_secs(5), || b.sensing_table_is_empty()).await,
        "B's sweep never expired the unrefreshed row",
    );

    // A's own LOCAL row expires on A's sweep the same way.
    assert!(
        poll_until(Duration::from_secs(5), || a.sensing_table_is_empty()).await,
        "A's sweep never expired its LOCAL row",
    );

    a.shutdown().await.expect("shutdown A");
    b.shutdown().await.expect("shutdown B");
}
