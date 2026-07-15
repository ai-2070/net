//! Regression guard for the redemption write-amplification fix
//! (`docs/performance/payments-redeem-write-amplification.md`).
//!
//! `redeem_for_invocation` must perform a durable store write on exactly one
//! outcome — `Admitted`, which flips `rec.redeemed`. Every `Denied{..}`
//! outcome is read-only and must NOT rewrite the store; otherwise an
//! unauthenticated caller spraying quote ids forces a global-lock + fsync
//! per attempt (a DoS surface).
//!
//! "Did not rewrite" is checked by the store inode on unix: a save renames a
//! fresh temp over the file (new inode), so a denial that leaves the inode
//! unchanged is proof no serialize/fsync/rename occurred. This test is a
//! plain engine test (no mesh gate), so it runs on every build.

#![cfg(unix)]

use std::os::unix::fs::MetadataExt as _;
use std::sync::Arc;

use net::adapter::net::identity::EntityKeypair;
use net_payments::core::registry::default_mock_registry;
use net_payments::core::verification::VerificationTier;
use net_payments::engine::{AdmitAll, PaymentDecision, PaymentEngine, RedeemDecision};
use net_payments::facilitator::mock::{MockFacilitator, MOCK_NETWORK, MOCK_SCHEME};
use net_payments::x402::payload::PaymentPayload;
use net_payments::x402::requirements::PaymentRequirements;
use net_payments::x402::X402Carry;

const NOW: u64 = 1_000_000_000_000_000;
const CAPABILITY: &str = "fixture-provider/fixture-tool";
const TOOL_ID: &str = "fixture-tool";

fn ino(path: &std::path::Path) -> u64 {
    std::fs::metadata(path).expect("state file exists").ino()
}

/// Issue + settle a quote, asserting it Served; return its quote id.
async fn mint_settled(engine: &Arc<PaymentEngine>, caller: &EntityKeypair) -> String {
    let requirements = X402Carry::author(&PaymentRequirements {
        scheme: MOCK_SCHEME.into(),
        network: MOCK_NETWORK.into(),
        amount: "2500".into(),
        asset: "musd".into(),
        pay_to: "mock-provider-settle-addr".into(),
        max_timeout_seconds: 60,
        extra: None,
    })
    .expect("author requirements");
    let quote = engine
        .issue_quote(caller.entity_id().clone(), CAPABILITY, requirements, NOW, 60_000_000_000)
        .expect("issue_quote");
    let payload = X402Carry::author(&PaymentPayload {
        x402_version: 2,
        resource: None,
        accepted: quote.requirements.view().clone(),
        payload: serde_json::json!({ "mock_authorization": quote.quote_id }),
        extensions: None,
    })
    .expect("author payload");
    let decision = engine
        .accept_payment(&quote, &payload, VerificationTier::Observed, NOW + 1)
        .await
        .expect("accept_payment");
    assert!(matches!(decision, PaymentDecision::Served { .. }));
    quote.quote_id
}

#[tokio::test]
async fn redemption_denials_do_not_rewrite_the_store_but_admission_does() {
    let provider = Arc::new(EntityKeypair::generate());
    let caller = EntityKeypair::generate();
    let dir = tempfile::tempdir().expect("tempdir");
    let state_path = dir.path().join("engine.json");
    let engine = Arc::new(
        PaymentEngine::new(
            provider.clone(),
            Arc::new(MockFacilitator::new()),
            Arc::new(AdmitAll),
            default_mock_registry(provider.entity_id().clone()),
            state_path.clone(),
        )
        .expect("engine"),
    );

    // Settling writes the store; from here the inode is our write sentinel.
    let quote_id = mint_settled(&engine, &caller).await;
    let ino_settled = ino(&state_path);

    // Read-only denials: none may rewrite (rename) the store.
    // 1) unknown quote — earliest exit, touches no record.
    let d = engine
        .redeem_for_invocation(TOOL_ID, "no-such-quote", None)
        .await
        .expect("redeem unknown");
    assert!(matches!(d, RedeemDecision::Denied { .. }));
    // 2) wrong tool — the settled quote redeemed for a different tool.
    let d = engine
        .redeem_for_invocation("some-other-tool", &quote_id, None)
        .await
        .expect("redeem wrong tool");
    assert!(matches!(d, RedeemDecision::Denied { .. }));
    // 3) invalid binding — a 64-byte signature that does not verify.
    let d = engine
        .redeem_for_invocation(TOOL_ID, &quote_id, Some(&[0u8; 64]))
        .await
        .expect("redeem bad binding");
    assert!(matches!(d, RedeemDecision::Denied { .. }));

    assert_eq!(
        ino(&state_path),
        ino_settled,
        "read-only redemption denials must not rewrite the store"
    );

    // The one write: a valid admission flips `redeemed` and must persist
    // (rename → new inode), or at-most-once would not survive a restart.
    let d = engine
        .redeem_for_invocation(TOOL_ID, &quote_id, None)
        .await
        .expect("redeem valid");
    assert!(matches!(d, RedeemDecision::Admitted));
    let ino_admitted = ino(&state_path);
    assert_ne!(
        ino_admitted, ino_settled,
        "a valid admission must persist the redeemed flag"
    );

    // Redeeming again is now AlreadyRedeemed — a denial, read-only again.
    let d = engine
        .redeem_for_invocation(TOOL_ID, &quote_id, None)
        .await
        .expect("redeem already-redeemed");
    assert!(matches!(d, RedeemDecision::Denied { .. }));
    assert_eq!(
        ino(&state_path),
        ino_admitted,
        "an already-redeemed denial must not rewrite the store"
    );
}
