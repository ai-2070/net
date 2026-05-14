//! End-to-end integration tests for the Deck SDK's Phase 1
//! surface — `DeckClient` → `AdminCommands` → in-process
//! `MeshOsRuntime` → snapshot reflects the post-commit state.
//!
//! Each test pins one contract on the operator pipeline. The
//! per-module unit tests in `behavior::deck::tests` cover the
//! shape of each type; this file pins the cross-component
//! contract Deck-the-binary will compose against.
//!
//! Run: `cargo test --features meshos --test deck_pipeline`

#![cfg(feature = "meshos")]

use std::sync::Arc;
use std::time::Duration;

use futures::StreamExt;

use net::adapter::net::behavior::deck::{
    AdminError, DeckClient, DeckClientConfig, OperatorIdentity, OperatorRegistry, OperatorSignature,
};
use net::adapter::net::behavior::meshos::{
    ice_proposal_signing_payload, AdminVerifier, BlastWarning, IceActionProposal,
    LoggingDispatcher, MaintenanceStateSnapshot, MeshOsConfig, MeshOsEvent, MeshOsRuntime,
};

const THIS_NODE: u64 = 200;

fn fast_config() -> MeshOsConfig {
    MeshOsConfig::default()
        .with_this_node(THIS_NODE)
        .with_tick_interval(Duration::from_millis(15))
        .with_event_queue_capacity(64)
        .with_action_queue_capacity(64)
}

#[tokio::test]
async fn deck_client_enter_maintenance_flows_through_to_snapshot() {
    // Operator workflow: load identity, build a client, issue
    // `enter_maintenance`, observe the snapshot stream surface
    // the resulting `EnteringMaintenance` (or downstream) state.
    let dispatcher = Arc::new(LoggingDispatcher::new());
    let runtime = MeshOsRuntime::start(fast_config(), Arc::clone(&dispatcher));
    let deck = DeckClient::from_runtime(&runtime, OperatorIdentity::generate()).with_config(
        DeckClientConfig {
            snapshot_poll_interval: Duration::from_millis(20),
            ..DeckClientConfig::default()
        },
    );

    let commit = deck
        .admin()
        .enter_maintenance(THIS_NODE, None)
        .await
        .expect("admin commit");
    assert_eq!(commit.event_kind(), "enter_maintenance");
    assert_eq!(commit.operator_id(), deck.identity().operator_id());

    let mut stream = deck.snapshots();
    let mut saw_non_active = false;
    for _ in 0..20 {
        let snap = stream
            .next()
            .await
            .expect("stream item")
            .expect("snapshot ok");
        if !matches!(snap.local_maintenance, MaintenanceStateSnapshot::Active) {
            saw_non_active = true;
            break;
        }
    }
    assert!(
        saw_non_active,
        "snapshot stream should have surfaced a non-Active local_maintenance",
    );
    let _ = runtime.shutdown().await;
}

#[tokio::test]
async fn deck_client_drop_replicas_lands_admin_event_on_loop() {
    // We don't yet have a chain-side audit of "this admin event
    // landed"; the loop accepting the event without LoopClosed
    // is the Phase 1 observable contract.
    let dispatcher = Arc::new(LoggingDispatcher::new());
    let runtime = MeshOsRuntime::start(fast_config(), dispatcher);
    let deck = DeckClient::from_runtime(&runtime, OperatorIdentity::generate());

    let commit = deck
        .admin()
        .drop_replicas(THIS_NODE, vec![10, 20, 30])
        .await
        .expect("commit");
    assert_eq!(commit.event_kind(), "drop_replicas");
    let _ = runtime.shutdown().await;
}

#[tokio::test]
async fn deck_client_commit_after_shutdown_surfaces_loop_closed() {
    let dispatcher = Arc::new(LoggingDispatcher::new());
    let runtime = MeshOsRuntime::start(fast_config(), dispatcher);
    let deck = DeckClient::from_runtime(&runtime, OperatorIdentity::generate());
    let _ = runtime.shutdown().await;
    let err: AdminError = deck
        .admin()
        .cordon(THIS_NODE)
        .await
        .expect_err("post-shutdown publish should fail");
    assert_eq!(err.kind, "loop_closed");
}

#[tokio::test]
async fn deck_client_two_commits_carry_monotonic_commit_ids() {
    let dispatcher = Arc::new(LoggingDispatcher::new());
    let runtime = MeshOsRuntime::start(fast_config(), dispatcher);
    let deck = DeckClient::from_runtime(&runtime, OperatorIdentity::generate());

    let a = deck.admin().cordon(THIS_NODE).await.unwrap();
    let b = deck.admin().uncordon(THIS_NODE).await.unwrap();
    let c = deck.admin().invalidate_placement(THIS_NODE).await.unwrap();

    assert!(b.commit_id() > a.commit_id());
    assert!(c.commit_id() > b.commit_id());
    assert_eq!(c.operator_id(), a.operator_id());
    let _ = runtime.shutdown().await;
}

#[tokio::test]
async fn deck_client_freeze_cluster_lands_in_snapshot_and_thaw_clears() {
    let dispatcher = Arc::new(LoggingDispatcher::new());
    let runtime = MeshOsRuntime::start(fast_config(), dispatcher);
    let deck = DeckClient::from_runtime(&runtime, OperatorIdentity::generate()).with_config(
        DeckClientConfig {
            snapshot_poll_interval: Duration::from_millis(15),
            ..DeckClientConfig::default()
        },
    );

    // Freeze for 10s; observe `freeze_remaining_ms` surface
    // through the snapshot stream.
    let commit = deck
        .admin()
        .freeze_cluster(Duration::from_secs(10))
        .await
        .expect("freeze commit");
    assert_eq!(commit.event_kind(), "freeze_cluster");

    let mut stream = deck.snapshots();
    let mut saw_freeze = false;
    for _ in 0..20 {
        let snap = stream.next().await.expect("next").expect("ok");
        if snap.freeze_remaining_ms.is_some() {
            saw_freeze = true;
            break;
        }
    }
    assert!(
        saw_freeze,
        "snapshot stream should surface freeze_remaining_ms after freeze_cluster commit",
    );

    // Thaw — `freeze_remaining_ms` should clear within a few
    // stream frames.
    let _ = deck.admin().thaw_cluster().await.expect("thaw commit");
    let mut saw_thaw = false;
    for _ in 0..20 {
        let snap = stream.next().await.expect("next").expect("ok");
        if snap.freeze_remaining_ms.is_none() {
            saw_thaw = true;
            break;
        }
    }
    assert!(
        saw_thaw,
        "thaw should clear freeze_remaining_ms from the snapshot",
    );
    let _ = runtime.shutdown().await;
}

#[tokio::test]
async fn ice_proposal_simulate_then_commit_lands_freeze_through_pipeline() {
    // Full operator-side ICE workflow: build a proposal, run
    // the mandatory simulate(), sign, commit. The snapshot
    // stream surfaces `freeze_remaining_ms` as the loop folds
    // the underlying admin event.
    let dispatcher = Arc::new(LoggingDispatcher::new());
    let runtime = MeshOsRuntime::start(fast_config(), dispatcher);
    let deck = DeckClient::from_runtime(&runtime, OperatorIdentity::generate()).with_config(
        DeckClientConfig {
            snapshot_poll_interval: Duration::from_millis(15),
            ..DeckClientConfig::default()
        },
    );

    let proposal = deck.ice().freeze_cluster(Duration::from_secs(45));
    let blast = proposal.simulate().await.expect("simulate");
    assert_eq!(blast.estimated_drain_delay, Some(Duration::from_secs(45)));
    assert!(blast
        .warnings
        .iter()
        .any(|w| matches!(w, BlastWarning::ClusterFreezeBlocksOperatorActions)));

    let sig = deck.identity().sign_proposal(proposal.action(), proposal.issued_at_ms(), &proposal.blast_hash());
    let commit = proposal.commit(&[sig]).await.expect("commit");
    assert_eq!(commit.event_kind(), "freeze_cluster");

    let mut stream = deck.snapshots();
    let mut saw_freeze = false;
    for _ in 0..20 {
        let snap = stream.next().await.expect("next").expect("ok");
        if snap.freeze_remaining_ms.is_some() {
            saw_freeze = true;
            break;
        }
    }
    assert!(
        saw_freeze,
        "ICE freeze proposal commit should land the freeze in the snapshot",
    );
    let _ = runtime.shutdown().await;
}

#[tokio::test]
async fn ice_proposal_commit_without_simulate_is_rejected_before_publish() {
    // Locked decision #4: simulate() is mandatory before commit().
    // Confirm the SDK gate keeps the publish from firing — the
    // loop never sees an admin event.
    let dispatcher = Arc::new(LoggingDispatcher::new());
    let runtime = MeshOsRuntime::start(fast_config(), dispatcher);
    let deck = DeckClient::from_runtime(&runtime, OperatorIdentity::generate());

    let proposal = deck.ice().freeze_cluster(Duration::from_secs(10));
    let sig = deck.identity().sign_proposal(proposal.action(), proposal.issued_at_ms(), &proposal.blast_hash());
    let err = proposal
        .commit(&[sig])
        .await
        .expect_err("commit without simulate should fail");
    assert_eq!(err.kind, "simulation_required");

    // The snapshot should NOT show a freeze — the SDK didn't
    // publish the underlying admin event.
    tokio::time::sleep(Duration::from_millis(50)).await;
    let snap = runtime.snapshot();
    assert!(snap.freeze_remaining_ms.is_none());
    let _ = runtime.shutdown().await;
}

#[tokio::test]
async fn substrate_admin_verifier_rejects_tampered_signed_ice_commit() {
    // A malicious SDK that bypasses its own gate but submits a
    // tampered signature bundle must be rejected by the loop's
    // verifier — the inner AdminEvent must not fold.
    use std::sync::Arc as SArc;
    let dispatcher = Arc::new(LoggingDispatcher::new());
    let op = OperatorIdentity::generate();
    let mut registry = OperatorRegistry::new();
    registry.register(op.keypair());
    let verifier = SArc::new(AdminVerifier::new(SArc::new(registry), 1));
    let runtime = MeshOsRuntime::start_with_all(
        fast_config(),
        dispatcher,
        Default::default(),
        Default::default(),
        SArc::new(net::adapter::net::compute::DaemonRegistry::new()),
        None,
        Some(verifier),
    );

    // Bypass the SDK and publish a tampered SignedIceCommit
    // directly via the handle.
    let proposal = IceActionProposal::FreezeCluster {
        ttl: Duration::from_secs(20),
    };
    let issued_at_ms = net::adapter::net::behavior::meshos::now_ms_since_unix_epoch();
    let blast = net::adapter::net::behavior::meshos::simulate_ice_proposal(
        &runtime.snapshot(),
        &proposal,
    );
    let blast_hash = net::adapter::net::behavior::meshos::blast_radius_hash(&blast);
    let mut sig =
        OperatorSignature::sign(op.keypair(), &proposal, issued_at_ms, &blast_hash);
    sig.signature[5] ^= 0xAA; // tamper

    runtime
        .handle()
        .publish(MeshOsEvent::SignedIceCommit {
            proposal: proposal.clone(),
            signatures: vec![sig],
            issued_at_ms,
            blast_hash,
        })
        .await
        .unwrap();

    // Give the loop time to process + reject.
    tokio::time::sleep(Duration::from_millis(80)).await;
    let snap = runtime.snapshot();
    assert!(
        snap.freeze_remaining_ms.is_none(),
        "loop verifier should have rejected the tampered bundle; freeze should not be in effect",
    );
    let _ = runtime.shutdown().await;
}

#[tokio::test]
async fn substrate_admin_verifier_accepts_a_valid_signed_ice_commit_and_folds_it() {
    // The valid bundle path: a properly signed proposal folds
    // into state via the loop's verifier just like an unsigned
    // AdminEvent would.
    use std::sync::Arc as SArc;
    let dispatcher = Arc::new(LoggingDispatcher::new());
    let op = OperatorIdentity::generate();
    let mut registry = OperatorRegistry::new();
    registry.register(op.keypair());
    let verifier = SArc::new(AdminVerifier::new(SArc::new(registry), 1));
    let runtime = MeshOsRuntime::start_with_all(
        fast_config(),
        dispatcher,
        Default::default(),
        Default::default(),
        SArc::new(net::adapter::net::compute::DaemonRegistry::new()),
        None,
        Some(verifier),
    );

    let proposal = IceActionProposal::FreezeCluster {
        ttl: Duration::from_secs(30),
    };
    let issued_at_ms = net::adapter::net::behavior::meshos::now_ms_since_unix_epoch();
    let blast = net::adapter::net::behavior::meshos::simulate_ice_proposal(
        &runtime.snapshot(),
        &proposal,
    );
    let blast_hash = net::adapter::net::behavior::meshos::blast_radius_hash(&blast);
    let payload = ice_proposal_signing_payload(&proposal, issued_at_ms, &blast_hash);
    let sig = OperatorSignature::sign(op.keypair(), &proposal, issued_at_ms, &blast_hash);
    // Sanity: payload + signature match what the SDK would
    // produce.
    assert_eq!(sig.operator_id, op.operator_id());
    assert!(!payload.is_empty());

    runtime
        .handle()
        .publish(MeshOsEvent::SignedIceCommit {
            proposal: proposal.clone(),
            signatures: vec![sig],
            issued_at_ms,
            blast_hash,
        })
        .await
        .unwrap();

    tokio::time::sleep(Duration::from_millis(80)).await;
    let snap = runtime.snapshot();
    assert!(
        snap.freeze_remaining_ms.is_some(),
        "verified SignedIceCommit should fold its inner FreezeCluster admin event",
    );
    let _ = runtime.shutdown().await;
}

#[tokio::test]
async fn substrate_admin_verifier_rejects_tampered_signed_admin_commit() {
    // Malicious SDK bypasses any local gate and publishes a
    // tampered SignedAdminCommit. The loop's verifier must
    // reject; the inner event must NOT fold (cordon would
    // otherwise show up downstream).
    use net::adapter::net::behavior::meshos::AdminEvent;
    use std::sync::Arc as SArc;
    let dispatcher = Arc::new(LoggingDispatcher::new());
    let op = OperatorIdentity::generate();
    let mut registry = OperatorRegistry::new();
    registry.register(op.keypair());
    let verifier = SArc::new(AdminVerifier::new(SArc::new(registry), 1));
    let runtime = MeshOsRuntime::start_with_all(
        fast_config(),
        dispatcher,
        Default::default(),
        Default::default(),
        SArc::new(net::adapter::net::compute::DaemonRegistry::new()),
        None,
        Some(verifier),
    );

    let event = AdminEvent::Cordon { node: THIS_NODE };
    let issued_at_ms = net::adapter::net::behavior::meshos::now_ms_since_unix_epoch();
    let mut signature = OperatorSignature::sign_admin(op.keypair(), &event, issued_at_ms);
    signature.signature[3] ^= 0xAA; // tamper

    runtime
        .handle()
        .publish(MeshOsEvent::SignedAdminCommit {
            event: event.clone(),
            signature,
            issued_at_ms,
        })
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(80)).await;

    // Audit ring should record the rejected attempt.
    let snap = runtime.snapshot();
    assert_eq!(snap.admin_audit.len(), 1);
    assert!(matches!(
        snap.admin_audit[0].outcome,
        net::adapter::net::behavior::meshos::VerificationOutcome::Rejected { .. }
    ));
    let _ = runtime.shutdown().await;
}

#[tokio::test]
async fn admin_audit_ring_records_accepted_and_rejected_attempts() {
    // The substrate verifier records every SignedIceCommit it
    // sees — accepted AND rejected — on the snapshot's
    // admin_audit ring. Security review reads this to replay
    // every break-glass attempt regardless of outcome.
    use net::adapter::net::behavior::meshos::VerificationOutcome;
    use std::sync::Arc as SArc;
    let dispatcher = Arc::new(LoggingDispatcher::new());
    let op = OperatorIdentity::generate();
    let mut registry = OperatorRegistry::new();
    registry.register(op.keypair());
    let verifier = SArc::new(AdminVerifier::new(SArc::new(registry), 1));
    let runtime = MeshOsRuntime::start_with_all(
        fast_config(),
        dispatcher,
        Default::default(),
        Default::default(),
        SArc::new(net::adapter::net::compute::DaemonRegistry::new()),
        None,
        Some(verifier),
    );

    // One accepted commit.
    let good = IceActionProposal::FreezeCluster {
        ttl: Duration::from_secs(30),
    };
    let good_ts = net::adapter::net::behavior::meshos::now_ms_since_unix_epoch();
    let good_blast = net::adapter::net::behavior::meshos::simulate_ice_proposal(
        &runtime.snapshot(),
        &good,
    );
    let good_hash = net::adapter::net::behavior::meshos::blast_radius_hash(&good_blast);
    let good_sig = OperatorSignature::sign(op.keypair(), &good, good_ts, &good_hash);
    runtime
        .handle()
        .publish(MeshOsEvent::SignedIceCommit {
            proposal: good.clone(),
            signatures: vec![good_sig],
            issued_at_ms: good_ts,
            blast_hash: good_hash,
        })
        .await
        .unwrap();

    // One rejected commit (tampered signature bytes).
    let bad = IceActionProposal::ThawCluster;
    let bad_ts = net::adapter::net::behavior::meshos::now_ms_since_unix_epoch();
    let bad_blast = net::adapter::net::behavior::meshos::simulate_ice_proposal(
        &runtime.snapshot(),
        &bad,
    );
    let bad_hash = net::adapter::net::behavior::meshos::blast_radius_hash(&bad_blast);
    let mut bad_sig = OperatorSignature::sign(op.keypair(), &bad, bad_ts, &bad_hash);
    bad_sig.signature[0] ^= 0xFF;
    runtime
        .handle()
        .publish(MeshOsEvent::SignedIceCommit {
            proposal: bad.clone(),
            signatures: vec![bad_sig],
            issued_at_ms: bad_ts,
            blast_hash: bad_hash,
        })
        .await
        .unwrap();

    tokio::time::sleep(Duration::from_millis(80)).await;
    let snap = runtime.snapshot();
    assert_eq!(
        snap.admin_audit.len(),
        2,
        "every SignedIceCommit should land on the audit ring; got {}",
        snap.admin_audit.len(),
    );
    let accepted = snap
        .admin_audit
        .iter()
        .filter(|r| matches!(r.outcome, VerificationOutcome::Accepted))
        .count();
    let rejected = snap
        .admin_audit
        .iter()
        .filter(|r| matches!(r.outcome, VerificationOutcome::Rejected { .. }))
        .count();
    assert_eq!(accepted, 1, "exactly one accepted commit");
    assert_eq!(rejected, 1, "exactly one rejected commit");
    // The rejected entry carries the operator id even though
    // the signature failed verification.
    let rej = snap
        .admin_audit
        .iter()
        .find(|r| matches!(r.outcome, VerificationOutcome::Rejected { .. }))
        .unwrap();
    assert_eq!(rej.operator_ids, vec![op.operator_id()]);
    let _ = runtime.shutdown().await;
}
