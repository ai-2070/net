//! OLB-0 §4.3 external witnesses: the node-global sensing-interest lease
//! actually drives REMOTE registration state over the wire.
//!
//! `sensing_lease.rs` proves the single-node registry/refcount behavior;
//! these are the two-node proofs Kyra's checkpoint requires — that a lease
//! acquisition installs a row at the provider, a stricter acquisition
//! tightens the provider-side cadence, the strictest release relaxes it, and
//! a last release deregisters the provider-side row promptly (not merely by
//! the ~ttl soft-state sweep).
//!
//! Topology mirrors `sensing_dispatch`: A is consumer + owner root; B is the
//! exact provider, configured to accept A's owner root. Delivery is loopback
//! with generous buffers and no chaos injection, so a single send arrives;
//! assertions poll with a timeout for scheduling. The lease does not run a
//! ttl/2 refresh loop in this slice, so each transition is a single send —
//! the tighten/relax sends are spaced past the upstream damper's min-gap.
//!
//! Run: `cargo test --features net --test sensing_lease_wire`

#![cfg(feature = "net")]

mod common;
use common::*;

use std::sync::Arc;
use std::time::Duration;

use net::adapter::net::behavior::capability::CapabilitySet;
use net::adapter::net::behavior::sensing::{
    AudienceScopeCommitment, CanonicalConstraints, CapabilityId, DisclosureClass, DownstreamId,
    InterestSpec, ProviderInterestKey, ProviderSelector, ResultMode, WorkLatencyEnvelope,
};
use net::adapter::net::{EntityKeypair, MeshNode, MeshNodeConfig, SocketBufferConfig};
use net::adapter::Adapter;

const D: Duration = Duration::from_millis(200);
const STRICT: Duration = Duration::from_millis(50);
/// Comfortably past the upstream damper min-gap (100 ms) so a spaced
/// re-register is not leading-edge suppressed.
const PAST_MIN_GAP: Duration = Duration::from_millis(180);
const POLL: Duration = Duration::from_secs(3);

fn base_config() -> MeshNodeConfig {
    let addr = "127.0.0.1:0".parse().unwrap();
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

async fn build_with_keypair(keypair: EntityKeypair, cfg: MeshNodeConfig) -> Arc<MeshNode> {
    Arc::new(MeshNode::new(keypair, cfg).await.expect("MeshNode::new"))
}

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

fn spec_for(owner: AudienceScopeCommitment, provider: u64) -> InterestSpec {
    InterestSpec {
        capability_id: CapabilityId::new("gpu.infer"),
        constraints: CanonicalConstraints::from_entries([("model", "llama-70b")]).unwrap(),
        work_latency: WorkLatencyEnvelope::start_within(Duration::from_secs(2)),
        providers: ProviderSelector::Node(provider),
        result_mode: ResultMode::Any,
        disclosure_class: DisclosureClass::Owner,
        audience: owner,
    }
}

/// The provider-side registered cadence for A's interest, if the row exists.
fn peer_interval(b: &Arc<MeshNode>, key: &ProviderInterestKey, a_id: u64) -> Option<Duration> {
    b.sensing_downstream_entry(key, DownstreamId::Peer(a_id))
        .map(|row| row.requested_sample_interval)
}

/// A lease acquisition installs a provider-side row; a stricter acquisition
/// tightens the provider's cadence; the strictest release relaxes it.
#[tokio::test]
async fn lease_tighten_and_relax_reach_the_provider() {
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
    let spec = spec_for(owner, b_id);
    let key = ProviderInterestKey::new(spec.key(), b_id);

    // Loose lease → the provider gains the row at the loose cadence.
    let loose = a
        .acquire_sensing_interest_lease(&spec, b_id, D)
        .expect("loose acquire registers");
    assert!(
        poll_until(POLL, || peer_interval(&b, &key, a_id) == Some(D)).await,
        "provider never saw the loose registration at {D:?}"
    );

    // A stricter holder joins (spaced past the damper min-gap) → the provider
    // tightens to the new minimum.
    tokio::time::sleep(PAST_MIN_GAP).await;
    let strict = a
        .acquire_sensing_interest_lease(&spec, b_id, STRICT)
        .expect("strict acquire re-registers tighter");
    assert!(
        poll_until(POLL, || peer_interval(&b, &key, a_id) == Some(STRICT)).await,
        "provider never tightened to {STRICT:?}"
    );

    // The strictest holder releases (spaced past the min-gap) → the provider
    // relaxes back to the surviving loose cadence.
    tokio::time::sleep(PAST_MIN_GAP).await;
    a.release_sensing_interest_lease(strict);
    assert!(
        poll_until(POLL, || peer_interval(&b, &key, a_id) == Some(D)).await,
        "provider never relaxed back to {D:?}"
    );

    a.release_sensing_interest_lease(loose);
    a.shutdown().await.expect("shutdown A");
    b.shutdown().await.expect("shutdown B");
}

/// The last release deregisters the provider-side row PROMPTLY over the wire
/// — well inside the ~ttl soft-state sweep window, so a fast disappearance
/// proves the explicit `Deregister` frame was delivered.
#[tokio::test]
async fn last_release_deregisters_the_provider_row() {
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
    let spec = spec_for(owner, b_id);
    let key = ProviderInterestKey::new(spec.key(), b_id);

    let ticket = a
        .acquire_sensing_interest_lease(&spec, b_id, D)
        .expect("acquire registers");
    assert!(
        poll_until(POLL, || peer_interval(&b, &key, a_id).is_some()).await,
        "provider never gained the row"
    );

    a.release_sensing_interest_lease(ticket);
    assert!(
        poll_until(POLL, || peer_interval(&b, &key, a_id).is_none()).await,
        "provider row was not deregistered promptly (would only expire by sweep)"
    );

    a.shutdown().await.expect("shutdown A");
    b.shutdown().await.expect("shutdown B");
}
