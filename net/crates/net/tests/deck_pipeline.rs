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

/// Poll the runtime's snapshot at a tight interval until
/// `predicate` returns true or the timeout expires. Returns
/// the matching snapshot on success or an error message if the
/// timeout elapses without the predicate ever holding.
///
/// Replaces the previous "`tokio::time::sleep(80ms)` then
/// assert" pattern, which over-waited on the fast path and
/// under-waited on slow hosts. The poll cadence
/// (`5ms`) is much shorter than the loop's tick (15ms), so
/// the test resolves within one tick after the loop publishes.
async fn wait_for_snapshot<F>(
    runtime: &MeshOsRuntime,
    timeout: Duration,
    mut predicate: F,
) -> Result<net::adapter::net::behavior::meshos::MeshOsSnapshot, String>
where
    F: FnMut(&net::adapter::net::behavior::meshos::MeshOsSnapshot) -> bool,
{
    let deadline = std::time::Instant::now() + timeout;
    loop {
        let snap = runtime.snapshot();
        if predicate(&snap) {
            return Ok(snap);
        }
        if std::time::Instant::now() >= deadline {
            return Err(format!(
                "predicate never held within {timeout:?}; last snapshot freeze={:?} admin_audit_len={} log_ring_len={}",
                snap.freeze_remaining_ms,
                snap.admin_audit.len(),
                snap.log_ring.len(),
            ));
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
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

    // Freeze for 10s through the ICE surface; observe
    // `freeze_remaining_ms` surface through the snapshot
    // stream. No operator registry is installed, so the SDK
    // routes via the unsigned admin path.
    let freeze = deck
        .ice()
        .freeze_cluster(Duration::from_secs(10))
        .simulate()
        .await
        .expect("simulate");
    let freeze_sig =
        deck.identity()
            .sign_proposal(freeze.action(), freeze.issued_at_ms(), &freeze.blast_hash());
    let commit = freeze.commit(&[freeze_sig]).await.expect("freeze commit");
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
    let thaw = deck
        .ice()
        .thaw_cluster()
        .simulate()
        .await
        .expect("thaw simulate");
    let thaw_sig =
        deck.identity()
            .sign_proposal(thaw.action(), thaw.issued_at_ms(), &thaw.blast_hash());
    let _ = thaw.commit(&[thaw_sig]).await.expect("thaw commit");
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
    let simulated = proposal.simulate().await.expect("simulate");
    assert_eq!(
        simulated.blast_radius().estimated_drain_delay,
        Some(Duration::from_secs(45))
    );
    assert!(simulated
        .blast_radius()
        .warnings
        .iter()
        .any(|w| matches!(w, BlastWarning::ClusterFreezeBlocksOperatorActions)));

    let sig = deck.identity().sign_proposal(
        simulated.action(),
        simulated.issued_at_ms(),
        &simulated.blast_hash(),
    );
    let commit = simulated.commit(&[sig]).await.expect("commit");
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

// Locked decision #4: simulate() must precede commit(). The
// SDK-side enforcement is now type-state — `IceProposal` does
// not expose `commit`, only the `SimulatedIceProposal`
// returned from `IceProposal::simulate` does. A runtime gate
// no longer exists to test; the type system rejects
// commit-without-simulate at compile time. The substrate-side
// gate (blast_hash == SIMULATION_REQUIRED_SENTINEL) still
// exists for adversarial direct-publish, exercised by the
// substrate verifier unit tests.

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
    let blast =
        net::adapter::net::behavior::meshos::simulate_ice_proposal(&runtime.snapshot(), &proposal);
    let blast_hash = net::adapter::net::behavior::meshos::blast_radius_hash(&blast);
    let mut sig = OperatorSignature::sign(op.keypair(), &proposal, issued_at_ms, &blast_hash);
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
    let blast =
        net::adapter::net::behavior::meshos::simulate_ice_proposal(&runtime.snapshot(), &proposal);
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

    wait_for_snapshot(&runtime, Duration::from_secs(2), |s| {
        s.freeze_remaining_ms.is_some()
    })
    .await
    .expect("verified SignedIceCommit should fold its inner FreezeCluster admin event");
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
    // Audit ring should record the rejected attempt.
    let snap = wait_for_snapshot(&runtime, Duration::from_secs(2), |s| {
        !s.admin_audit.is_empty()
    })
    .await
    .expect("audit ring should record the rejected attempt");
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
    let good_blast =
        net::adapter::net::behavior::meshos::simulate_ice_proposal(&runtime.snapshot(), &good);
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
    let bad_blast =
        net::adapter::net::behavior::meshos::simulate_ice_proposal(&runtime.snapshot(), &bad);
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

    let snap = wait_for_snapshot(&runtime, Duration::from_secs(2), |s| {
        s.admin_audit.len() >= 2
    })
    .await
    .expect("every SignedIceCommit should land on the audit ring");
    assert_eq!(snap.admin_audit.len(), 2, "got {}", snap.admin_audit.len(),);
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

#[tokio::test]
async fn substrate_admin_verifier_rejects_under_threshold_bundle() {
    // Plan's ICE-discipline contract through the integration
    // pipeline: a SignedIceCommit with fewer distinct operator
    // signatures than the cluster's threshold must be rejected
    // by the loop-side verifier.
    use net::adapter::net::behavior::deck::DeckClientConfig;
    use net::adapter::net::behavior::meshos::DEFAULT_ICE_COOLDOWN_WINDOW;
    use std::sync::Arc as SArc;
    let dispatcher = Arc::new(LoggingDispatcher::new());
    let op_a = OperatorIdentity::generate();
    let op_b = OperatorIdentity::generate();
    let mut registry = OperatorRegistry::new();
    registry.register(op_a.keypair());
    registry.register(op_b.keypair());
    // Threshold = 2. Avoid the cooldown gate so the rejection
    // is unambiguously "insufficient_signatures" rather than
    // "ice_cooldown_active."
    let verifier = SArc::new(
        net::adapter::net::behavior::meshos::AdminVerifier::with_full_policy(
            SArc::new(registry),
            2,
            Duration::from_secs(300),
            Duration::from_secs(30),
            DEFAULT_ICE_COOLDOWN_WINDOW,
        ),
    );
    let runtime = MeshOsRuntime::start_with_all(
        fast_config(),
        dispatcher,
        Default::default(),
        Default::default(),
        SArc::new(net::adapter::net::compute::DaemonRegistry::new()),
        None,
        Some(verifier),
    );

    // Bypass the SDK's local threshold gate by setting
    // ice_signature_threshold = 1, then submit a 1-sig bundle
    // that the loop's verifier (threshold = 2) must reject.
    let mut sdk_registry = OperatorRegistry::new();
    sdk_registry.register(op_a.keypair());
    sdk_registry.register(op_b.keypair());
    let deck = DeckClient::from_runtime(&runtime, op_a.clone())
        .with_operator_registry(sdk_registry)
        .with_config(DeckClientConfig {
            ice_signature_threshold: 1,
            ..DeckClientConfig::default()
        });

    let proposal = deck
        .ice()
        .freeze_cluster(Duration::from_secs(30))
        .simulate()
        .await
        .expect("simulate");
    let sig_a = op_a.sign_proposal(
        proposal.action(),
        proposal.issued_at_ms(),
        &proposal.blast_hash(),
    );
    let _ = proposal.commit(&[sig_a]).await.expect("publish");
    tokio::time::sleep(Duration::from_millis(80)).await;

    // The audit ring records the rejected attempt with kind
    // "insufficient_signatures"; the inner FreezeCluster must
    // NOT have folded — freeze_remaining_ms stays None.
    let snap = runtime.snapshot();
    assert!(
        snap.freeze_remaining_ms.is_none(),
        "under-threshold bundle should not fold the inner FreezeCluster",
    );
    let kind = snap
        .admin_audit
        .iter()
        .find_map(|r| match &r.outcome {
            net::adapter::net::behavior::meshos::VerificationOutcome::Rejected { kind, .. } => {
                Some(kind.clone())
            }
            _ => None,
        })
        .expect("audit ring should record the rejection");
    assert_eq!(kind, "insufficient_signatures");
    let _ = runtime.shutdown().await;
}

#[tokio::test]
async fn substrate_admin_verifier_rejects_duplicate_signatures_from_same_operator() {
    // Distinct-operator dedup at the integration boundary:
    // `[sig_A, sig_A]` against threshold = 2 fails even
    // though both signatures cryptographically verify.
    use std::sync::Arc as SArc;
    let dispatcher = Arc::new(LoggingDispatcher::new());
    let op = OperatorIdentity::generate();
    let mut registry = OperatorRegistry::new();
    registry.register(op.keypair());
    let verifier = SArc::new(net::adapter::net::behavior::meshos::AdminVerifier::new(
        SArc::new(registry),
        2,
    ));
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
    let blast =
        net::adapter::net::behavior::meshos::simulate_ice_proposal(&runtime.snapshot(), &proposal);
    let blast_hash = net::adapter::net::behavior::meshos::blast_radius_hash(&blast);
    let sig = OperatorSignature::sign(op.keypair(), &proposal, issued_at_ms, &blast_hash);
    runtime
        .handle()
        .publish(MeshOsEvent::SignedIceCommit {
            proposal: proposal.clone(),
            signatures: vec![sig.clone(), sig], // same operator twice
            issued_at_ms,
            blast_hash,
        })
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(80)).await;

    let snap = runtime.snapshot();
    assert!(
        snap.freeze_remaining_ms.is_none(),
        "duplicate-signature bundle must not fold",
    );
    let kind = snap
        .admin_audit
        .iter()
        .find_map(|r| match &r.outcome {
            net::adapter::net::behavior::meshos::VerificationOutcome::Rejected { kind, .. } => {
                Some(kind.clone())
            }
            _ => None,
        })
        .expect("audit should record the rejection");
    assert_eq!(kind, "insufficient_signatures");
    let _ = runtime.shutdown().await;
}

#[tokio::test]
async fn substrate_admin_verifier_arms_ice_cooldown_after_successful_commit() {
    // ICE cooldown contract end-to-end: a successful
    // ForceCutover against node N arms the cooldown; a second
    // ForceCutover against N inside the window fails with
    // kind="ice_cooldown_active". Use a short cooldown (1s)
    // so the test stays fast.
    use std::sync::Arc as SArc;
    let dispatcher = Arc::new(LoggingDispatcher::new());
    let op = OperatorIdentity::generate();
    let mut registry = OperatorRegistry::new();
    registry.register(op.keypair());
    let verifier = SArc::new(
        net::adapter::net::behavior::meshos::AdminVerifier::with_full_policy(
            SArc::new(registry),
            1,
            Duration::from_secs(300),
            Duration::from_secs(30),
            Duration::from_secs(60),
        ),
    );
    let runtime = MeshOsRuntime::start_with_all(
        fast_config(),
        dispatcher,
        Default::default(),
        Default::default(),
        SArc::new(net::adapter::net::compute::DaemonRegistry::new()),
        None,
        Some(verifier),
    );

    let proposal = IceActionProposal::ForceCutover {
        chain: 100,
        target: 42,
    };
    let issued_at_ms = net::adapter::net::behavior::meshos::now_ms_since_unix_epoch();
    let blast =
        net::adapter::net::behavior::meshos::simulate_ice_proposal(&runtime.snapshot(), &proposal);
    let blast_hash = net::adapter::net::behavior::meshos::blast_radius_hash(&blast);
    let sig = OperatorSignature::sign(op.keypair(), &proposal, issued_at_ms, &blast_hash);
    runtime
        .handle()
        .publish(MeshOsEvent::SignedIceCommit {
            proposal: proposal.clone(),
            signatures: vec![sig.clone()],
            issued_at_ms,
            blast_hash,
        })
        .await
        .unwrap();

    // Second commit against the same target inside the
    // cooldown window must be rejected.
    let issued_at_ms2 = issued_at_ms + 50;
    let sig2 = OperatorSignature::sign(op.keypair(), &proposal, issued_at_ms2, &blast_hash);
    runtime
        .handle()
        .publish(MeshOsEvent::SignedIceCommit {
            proposal,
            signatures: vec![sig2],
            issued_at_ms: issued_at_ms2,
            blast_hash,
        })
        .await
        .unwrap();
    let snap = wait_for_snapshot(&runtime, Duration::from_secs(2), |s| {
        s.admin_audit.iter().any(|r| match &r.outcome {
            net::adapter::net::behavior::meshos::VerificationOutcome::Rejected { kind, .. } => {
                kind == "ice_cooldown_active"
            }
            _ => false,
        })
    })
    .await
    .expect("second ICE commit should be audited as ice_cooldown_active");
    let cooldown_rejected = snap.admin_audit.iter().any(|r| match &r.outcome {
        net::adapter::net::behavior::meshos::VerificationOutcome::Rejected { kind, .. } => {
            kind == "ice_cooldown_active"
        }
        _ => false,
    });
    assert!(
        cooldown_rejected,
        "second ICE commit inside the cooldown window should be audited as ice_cooldown_active; got {:?}",
        snap.admin_audit
    );
    let _ = runtime.shutdown().await;
}

#[tokio::test]
async fn substrate_admin_verifier_rejects_simulation_required_sentinel() {
    // Cryptographic enforcement of simulate-before-commit:
    // a SignedIceCommit carrying SIMULATION_REQUIRED_SENTINEL
    // must be rejected before any signature math. Locked
    // decision #4.
    use net::adapter::net::behavior::meshos::SIMULATION_REQUIRED_SENTINEL;
    use std::sync::Arc as SArc;
    let dispatcher = Arc::new(LoggingDispatcher::new());
    let op = OperatorIdentity::generate();
    let mut registry = OperatorRegistry::new();
    registry.register(op.keypair());
    let verifier = SArc::new(net::adapter::net::behavior::meshos::AdminVerifier::new(
        SArc::new(registry),
        1,
    ));
    let runtime = MeshOsRuntime::start_with_all(
        fast_config(),
        dispatcher,
        Default::default(),
        Default::default(),
        SArc::new(net::adapter::net::compute::DaemonRegistry::new()),
        None,
        Some(verifier),
    );

    let proposal = IceActionProposal::ThawCluster;
    let issued_at_ms = net::adapter::net::behavior::meshos::now_ms_since_unix_epoch();
    let sig = OperatorSignature::sign(
        op.keypair(),
        &proposal,
        issued_at_ms,
        &SIMULATION_REQUIRED_SENTINEL,
    );
    runtime
        .handle()
        .publish(MeshOsEvent::SignedIceCommit {
            proposal,
            signatures: vec![sig],
            issued_at_ms,
            blast_hash: SIMULATION_REQUIRED_SENTINEL,
        })
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(80)).await;

    let snap = runtime.snapshot();
    let kind = snap
        .admin_audit
        .iter()
        .find_map(|r| match &r.outcome {
            net::adapter::net::behavior::meshos::VerificationOutcome::Rejected { kind, .. } => {
                Some(kind.clone())
            }
            _ => None,
        })
        .expect("audit should record the rejection");
    assert_eq!(kind, "simulation_required");
    let _ = runtime.shutdown().await;
}

#[tokio::test]
async fn runtime_epoch_id_changes_across_two_runtime_instances() {
    // SDK consumers that dedup against `since(seq)` watermarks
    // must be able to detect a runtime restart so they can
    // reset their watermark — otherwise post-restart records
    // (seq=1, 2, …) silently filter out as "smaller than my
    // last seq." Two MeshOsRuntime instances stamp distinct
    // `runtime_epoch_id` values on their snapshots.
    let dispatcher_a = Arc::new(LoggingDispatcher::new());
    let rt_a = MeshOsRuntime::start(fast_config(), dispatcher_a);
    let epoch_a = rt_a.snapshot().runtime_epoch_id;
    assert_ne!(epoch_a, 0, "runtime_epoch_id should be stamped non-zero");

    let dispatcher_b = Arc::new(LoggingDispatcher::new());
    let rt_b = MeshOsRuntime::start(fast_config(), dispatcher_b);
    let epoch_b = rt_b.snapshot().runtime_epoch_id;
    assert_ne!(
        epoch_a, epoch_b,
        "two distinct runtimes should stamp distinct runtime_epoch_id values",
    );

    let _ = rt_a.shutdown().await;
    let _ = rt_b.shutdown().await;
}

#[tokio::test]
async fn kill_migration_with_no_op_aborter_and_verifier_records_failure() {
    // Production-partial config detection: an admin verifier
    // is wired (operator-policy chain installed) but the
    // migration aborter is still the no-op default. A
    // KillMigration commit lands on the chain but the
    // orchestrator never actually aborts; the loop pushes a
    // FailureRecord so operators reading subscribe_failures
    // see the gap rather than green status.
    use net::adapter::net::behavior::deck::FailureStream;
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
    // The SDK needs to publish via the SignedIceCommit path
    // so the loop's verifier gate fires; register the op key
    // on a fresh registry installed on the client.
    let mut sdk_registry = OperatorRegistry::new();
    sdk_registry.register(op.keypair());
    let deck = DeckClient::from_runtime(&runtime, op.clone()).with_operator_registry(sdk_registry);

    let mut failures: FailureStream = deck.subscribe_failures(0);
    let kill = deck
        .ice()
        .kill_migration(0xDEAD_BEEF)
        .simulate()
        .await
        .expect("simulate");
    let sig = deck
        .identity()
        .sign_proposal(kill.action(), kill.issued_at_ms(), &kill.blast_hash());
    kill.commit(&[sig]).await.expect("commit");

    let record = tokio::time::timeout(Duration::from_secs(2), failures.next())
        .await
        .expect("failure stream timed out")
        .expect("failure stream closed")
        .expect("failure record");
    assert!(
        record.source.starts_with("kill-migration:"),
        "expected source kill-migration:* , got {}",
        record.source
    );
    assert!(
        record.reason.contains("no-op") || record.reason.contains("aborter"),
        "expected reason to mention the no-op aborter, got {}",
        record.reason
    );
    let _ = runtime.shutdown().await;
}

#[tokio::test]
async fn ordinary_admin_commits_are_rejected_during_cluster_freeze() {
    // Freeze gate: an ordinary admin commit that lands during
    // an in-effect cluster freeze must be audited as Rejected
    // with kind "freeze_in_effect" and the inner event must
    // NOT fold. ICE commits (force-ops, freeze, thaw) bypass
    // because operators need to thaw the cluster mid-freeze.
    use net::adapter::net::behavior::meshos::VerificationOutcome;
    let dispatcher = Arc::new(LoggingDispatcher::new());
    let runtime = MeshOsRuntime::start(fast_config(), dispatcher);
    let deck = DeckClient::from_runtime(&runtime, OperatorIdentity::generate());

    // 1. Freeze the cluster via the ICE path.
    let freeze = deck
        .ice()
        .freeze_cluster(Duration::from_secs(60))
        .simulate()
        .await
        .expect("simulate");
    let freeze_sig =
        deck.identity()
            .sign_proposal(freeze.action(), freeze.issued_at_ms(), &freeze.blast_hash());
    freeze.commit(&[freeze_sig]).await.expect("freeze commit");
    wait_for_snapshot(&runtime, Duration::from_secs(2), |s| {
        s.freeze_remaining_ms.is_some()
    })
    .await
    .expect("cluster should be frozen after the freeze commit");

    // 2. Attempt an ordinary Cordon during the freeze. It
    //    should land on the audit ring as Rejected with kind
    //    "freeze_in_effect" and NOT take effect on state.
    let _ = deck.admin().cordon(THIS_NODE).await.expect("publish");
    let snap = wait_for_snapshot(&runtime, Duration::from_secs(2), |s| {
        s.admin_audit.iter().any(|r| match &r.outcome {
            VerificationOutcome::Rejected { kind, .. } => kind == "freeze_in_effect",
            _ => false,
        })
    })
    .await
    .expect("cordon during freeze should be audited with kind freeze_in_effect");
    let rejected_freeze: Vec<_> = snap
        .admin_audit
        .iter()
        .filter(|r| match &r.outcome {
            VerificationOutcome::Rejected { kind, .. } => kind == "freeze_in_effect",
            _ => false,
        })
        .collect();
    assert!(
        !rejected_freeze.is_empty(),
        "Cordon during freeze should be audited with kind freeze_in_effect; got audit ring {:?}",
        snap.admin_audit
    );

    // 3. The ICE Thaw bypasses the freeze gate and unblocks
    //    ordinary admin commits.
    let thaw = deck
        .ice()
        .thaw_cluster()
        .simulate()
        .await
        .expect("simulate");
    let thaw_sig =
        deck.identity()
            .sign_proposal(thaw.action(), thaw.issued_at_ms(), &thaw.blast_hash());
    thaw.commit(&[thaw_sig]).await.expect("thaw commit");

    let _ = runtime.shutdown().await;
}
