//! Task #13 — the read-only-branch audit for `accept_payment`'s claim
//! transaction and `check_and_reserve`. A transaction that does not change
//! durable state must not rewrite the store (serialize + fsync + rename);
//! one that does must persist exactly its mutation. "Did/did not rewrite" is
//! witnessed by the store inode on unix (a save renames a fresh temp over
//! the file → new inode), per
//! `docs/performance/payments-redeem-write-amplification.md`.
//!
//! These are plain engine/spend tests (no mesh gate), so they run on every
//! build. The deeper invariants (concurrent-settle-once, republish, release-
//! on-verify-fail, no-overspend, fail-closed) are covered by the existing
//! adversarial / spend_policy / native_tool_gate suites, which must stay
//! green alongside this change.

#![cfg(unix)]

use std::os::unix::fs::MetadataExt as _;
use std::path::Path;
use std::sync::Arc;

use net::adapter::net::identity::EntityKeypair;
use net_payments::core::quote::PaymentQuote;
use net_payments::core::registry::{default_mock_registry, AssetEntry};
use net_payments::core::units::AtomicAmount;
use net_payments::core::verification::VerificationTier;
use net_payments::engine::{AdmitAll, PaymentDecision, PaymentEngine, RejectReason};
use net_payments::facilitator::mock::{MockFacilitator, MockMode, MOCK_NETWORK, MOCK_SCHEME};
use net_payments::policy::spend::{SpendDecision, SpendPolicyEngine, SpendProfile};
use net_payments::x402::caip::AssetId;
use net_payments::x402::payload::PaymentPayload;
use net_payments::x402::requirements::PaymentRequirements;
use net_payments::x402::X402Carry;

const NOW: u64 = 1_000_000_000_000_000;
const NS_PER_DAY: u64 = 86_400_000_000_000;
const TTL: u64 = 60_000_000_000;
const CAP: &str = "fixture-provider/fixture-tool";
const USDC_BASE: &str = "eip155:8453/erc20:0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913";

fn ino(p: &Path) -> u64 {
    std::fs::metadata(p).expect("state file exists").ino()
}

fn mock_reqs(amount: &str) -> X402Carry<PaymentRequirements> {
    X402Carry::author(&PaymentRequirements {
        scheme: MOCK_SCHEME.into(),
        network: MOCK_NETWORK.into(),
        amount: amount.into(),
        asset: "musd".into(),
        pay_to: "mock-provider-settle-addr".into(),
        max_timeout_seconds: 60,
        extra: None,
    })
    .unwrap()
}

fn payload(quote: &PaymentQuote, nonce: &str) -> X402Carry<PaymentPayload> {
    X402Carry::author(&PaymentPayload {
        x402_version: 2,
        resource: None,
        accepted: quote.requirements.view().clone(),
        payload: serde_json::json!({ "mock_authorization": nonce }),
        extensions: None,
    })
    .unwrap()
}

// ============================================================================
// accept_payment — the claim transaction.
// ============================================================================

/// A read-only claim outcome (QuoteAlreadyPaid) must not rewrite the store;
/// a Fresh admission must.
#[tokio::test]
async fn accept_read_only_denial_is_clean_but_fresh_admission_persists() {
    let provider = Arc::new(EntityKeypair::generate());
    let caller = EntityKeypair::generate();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("engine.json");
    let engine = Arc::new(
        PaymentEngine::new(
            provider.clone(),
            Arc::new(MockFacilitator::new()),
            Arc::new(AdmitAll),
            default_mock_registry(provider.entity_id().clone()),
            path.clone(),
        )
        .unwrap(),
    );

    // Settle a quote (a Fresh claim + completion → durable writes).
    let quote = engine
        .issue_quote(caller.entity_id().clone(), CAP, mock_reqs("2500"), NOW, TTL)
        .unwrap();
    let p1 = payload(&quote, &quote.quote_id);
    assert!(matches!(
        engine.accept_payment(&quote, &p1, VerificationTier::Observed, NOW + 1).await.unwrap(),
        PaymentDecision::Served { .. }
    ));
    let ino0 = ino(&path);

    // Read-only claim outcome: the SAME quote with a DIFFERENT payload hits
    // `rec.payload_hash != payload_hash` → QuoteAlreadyPaid, no mutation.
    let p2 = payload(&quote, "a-different-nonce");
    let denied = engine
        .accept_payment(&quote, &p2, VerificationTier::Observed, NOW + 2)
        .await
        .unwrap();
    assert!(matches!(denied, PaymentDecision::Rejected { reason: RejectReason::QuoteAlreadyPaid }));
    assert_eq!(ino(&path), ino0, "QuoteAlreadyPaid must not rewrite the store");

    // Dirty witness: a genuinely fresh admission persists (rename → new inode).
    let quote2 = engine
        .issue_quote(caller.entity_id().clone(), CAP, mock_reqs("2500"), NOW + 1000, TTL)
        .unwrap();
    let pf = payload(&quote2, &quote2.quote_id);
    assert!(matches!(
        engine.accept_payment(&quote2, &pf, VerificationTier::Observed, NOW + 1001).await.unwrap(),
        PaymentDecision::Served { .. }
    ));
    assert_ne!(ino(&path), ino0, "a fresh admission must persist the claim + completion");
}

/// Regression guard for Kyra's caveat: `verify_rejected` is NOT a dirty-flag
/// target — its Fresh claim and its release are both semantically real writes
/// (in-flight persistence for concurrency; release because value did not
/// move and a retry may succeed). It must still rewrite the store.
#[tokio::test]
async fn accept_verify_rejected_still_persists_claim_and_release() {
    let provider = Arc::new(EntityKeypair::generate());
    let caller = EntityKeypair::generate();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("engine.json");
    let engine = Arc::new(
        PaymentEngine::new(
            provider.clone(),
            Arc::new(MockFacilitator::new().with_default_mode(MockMode::WrongAmount)),
            Arc::new(AdmitAll),
            default_mock_registry(provider.entity_id().clone()),
            path.clone(),
        )
        .unwrap(),
    );

    // First verify_rejected: the claim (a Fresh write) + release write the
    // store into existence — `ino` panics if nothing was persisted.
    let q1 = engine
        .issue_quote(caller.entity_id().clone(), CAP, mock_reqs("2500"), NOW, TTL)
        .unwrap();
    let p1 = payload(&q1, &q1.quote_id);
    assert!(matches!(
        engine.accept_payment(&q1, &p1, VerificationTier::Observed, NOW + 1).await.unwrap(),
        PaymentDecision::Rejected { reason: RejectReason::VerifyRejected(_) }
    ));
    let ino0 = ino(&path);

    // A second verify_rejected still writes (claim + release) → inode moves.
    let q2 = engine
        .issue_quote(caller.entity_id().clone(), CAP, mock_reqs("2500"), NOW + 1000, TTL)
        .unwrap();
    let p2 = payload(&q2, &q2.quote_id);
    assert!(matches!(
        engine.accept_payment(&q2, &p2, VerificationTier::Observed, NOW + 1001).await.unwrap(),
        PaymentDecision::Rejected { reason: RejectReason::VerifyRejected(_) }
    ));
    assert_ne!(
        ino(&path),
        ino0,
        "verify_rejected must persist its claim + release (not optimized away)"
    );
}

// ============================================================================
// check_and_reserve — the spend transaction.
// ============================================================================

/// Build a production engine (unsafe mock auto-allow, so a mock spend lands
/// silently) whose registry also knows USDC-on-Base — so a real-network deny
/// is the enablement deny, not a registry miss.
fn spend_engine(path: &Path) -> (SpendPolicyEngine, net_payments::core::registry::AssetRegistry) {
    let provider = EntityKeypair::generate();
    let mut registry = default_mock_registry(provider.entity_id().clone());
    registry.assets.push(AssetEntry {
        id: AssetId::parse(USDC_BASE).unwrap(),
        x402_asset: "0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913".into(),
        decimals: 6,
        symbol: "USDC".into(),
        display_name: None,
        equivalence_class: None,
    });
    let engine =
        SpendPolicyEngine::new(path, SpendProfile::Production).with_unsafe_mock_auto_allow(true);
    (engine, registry)
}

fn real_quote(
    registry: &net_payments::core::registry::AssetRegistry,
    issued: u64,
) -> PaymentQuote {
    let reqs = X402Carry::author(&PaymentRequirements {
        scheme: "exact".into(),
        network: "eip155:8453".into(),
        amount: "10000".into(),
        asset: "0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913".into(),
        pay_to: "0x209693Bc6afc0C5328bA36FaF03C514EF312287C".into(),
        max_timeout_seconds: 60,
        extra: None,
    })
    .unwrap();
    PaymentQuote::new(
        EntityKeypair::generate().entity_id().clone(),
        EntityKeypair::generate().entity_id().clone(),
        CAP,
        None,
        reqs,
        registry.reference().unwrap(),
        issued,
        issued + TTL,
    )
}

fn mock_quote(
    registry: &net_payments::core::registry::AssetRegistry,
    issued: u64,
) -> PaymentQuote {
    PaymentQuote::new(
        EntityKeypair::generate().entity_id().clone(),
        EntityKeypair::generate().entity_id().clone(),
        CAP,
        None,
        mock_reqs("2500"),
        registry.reference().unwrap(),
        issued,
        issued + TTL,
    )
}

/// A hard spend denial that changes nothing (and prunes nothing) must not
/// rewrite; a successful reservation must.
#[tokio::test]
async fn spend_hard_denial_is_clean_but_reservation_persists() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("policy.json");
    let (engine, registry) = spend_engine(&path);
    // configure() establishes the file (its own unconditional write).
    engine
        .configure(|d, _| d.max_per_call = Some(AtomicAmount::from_u128(50_000)))
        .await
        .unwrap();
    let ino0 = ino(&path);

    // Clean: a real network not in allowed_networks → hard Denied. Fresh
    // counters, so housekeeping prunes nothing.
    let denied = engine
        .check_and_reserve(&real_quote(&registry, NOW), &registry, NOW)
        .await
        .unwrap();
    assert!(matches!(denied, SpendDecision::Denied { .. }), "got {denied:?}");
    assert_eq!(ino(&path), ino0, "a hard spend denial (no prune) must not rewrite");

    // Dirty: a mock spend auto-allows and reserves the day counter.
    let allowed = engine
        .check_and_reserve(&mock_quote(&registry, NOW), &registry, NOW)
        .await
        .unwrap();
    assert_eq!(allowed, SpendDecision::Allowed);
    assert_ne!(ino(&path), ino0, "a reservation must persist the counter");
}

/// The housekeeping trap: an otherwise-clean hard denial is dirty when
/// retention pruned a stale counter — the prune must persist.
#[tokio::test]
async fn spend_housekeeping_prune_persists_on_an_otherwise_clean_denial() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("policy.json");
    let (engine, registry) = spend_engine(&path);

    // Seed a stale counter: a mock allow at "today".
    assert_eq!(
        engine
            .check_and_reserve(&mock_quote(&registry, NOW), &registry, NOW)
            .await
            .unwrap(),
        SpendDecision::Allowed
    );
    let ino0 = ino(&path);

    // Ten days later, a hard real-network denial (normally clean) — but
    // housekeeping prunes the now-stale counter, so the transaction is dirty
    // and the prune must be persisted.
    let future = NOW + 10 * NS_PER_DAY;
    let denied = engine
        .check_and_reserve(&real_quote(&registry, future), &registry, future)
        .await
        .unwrap();
    assert!(matches!(denied, SpendDecision::Denied { .. }), "got {denied:?}");
    assert_ne!(
        ino(&path),
        ino0,
        "housekeeping that pruned a stale counter must persist even on a denial (the trap)"
    );
}
