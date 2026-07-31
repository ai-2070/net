//! Engine retention — the terminal-record sweep.
//!
//! Retention removes *fat lifecycle bookkeeping*, never replay protection.
//! Three states, three policies (see
//! `docs/internal/misc/PERF_AUDIT_2026_07_31_PAYMENTS_HOT_PATH.md` §1):
//!
//! | state | policy |
//! |---|---|
//! | terminal `QuoteRecord` | pruned 6h past authoritative quote expiry |
//! | `consumed` payload hash | pruned atomically with its record, owner-checked |
//! | `consumed_transactions` | **retained indefinitely** — permanent uniqueness index |
//!
//! The load-bearing property is that deletion never becomes resurrection:
//! a record only retires well after the quote that minted it stopped being
//! redeemable, and an old settlement stays rejected forever regardless.
//!
//! These are plain engine tests (no mesh gate), so they run on every build.
//! The one assertion that needs an inode witness — that a sweep makes an
//! otherwise read-only transaction persist — is unix-gated, matching
//! `read_only_writes_audit.rs`.

use std::sync::Arc;

use net::adapter::net::identity::EntityKeypair;
use net_payments::billing::BillingLog;
use net_payments::core::quote::PaymentQuote;
use net_payments::core::registry::default_mock_registry;
use net_payments::core::verification::{InvalidationReason, VerificationTier};
use net_payments::engine::{
    AdmitAll, PaymentDecision, PaymentEngine, RedeemDecision, RejectReason,
    DEFAULT_TERMINAL_RECORD_RETENTION_NS,
};
use net_payments::facilitator::mock::{MockFacilitator, MOCK_NETWORK, MOCK_SCHEME};
use net_payments::x402::payload::PaymentPayload;
use net_payments::x402::requirements::PaymentRequirements;
use net_payments::x402::X402Carry;

const NOW: u64 = 1_000_000_000_000_000;
const TTL: u64 = 60_000_000_000;
const CAP: &str = "fixture-provider/fixture-tool";
const TOOL: &str = "fixture-tool";

/// A moment safely past the retention horizon for a quote issued at `NOW`.
fn past_horizon() -> u64 {
    NOW + TTL + DEFAULT_TERMINAL_RECORD_RETENTION_NS + 1
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

struct Fixture {
    engine: Arc<PaymentEngine>,
    provider: Arc<EntityKeypair>,
    caller: EntityKeypair,
    path: std::path::PathBuf,
    _dir: tempfile::TempDir,
}

impl Fixture {
    /// A second engine over the **same store** with a **fresh** mock
    /// facilitator.
    ///
    /// The mock keeps its own settled-payload index, so re-presenting a
    /// payload to the same instance is refused by the facilitator before
    /// the engine ever reasons about it. That shadows the engine's own
    /// guard — and the engine's guard is the one that matters, because
    /// the facilitator is deliberately not in the trust root. A fresh
    /// instance models exactly the case the tombstone defends against: a
    /// restarted, forgetful, or defective facilitator presenting an old
    /// settlement.
    fn with_forgetful_facilitator(&self) -> PaymentEngine {
        PaymentEngine::new(
            self.provider.clone(),
            Arc::new(MockFacilitator::new()),
            Arc::new(AdmitAll),
            default_mock_registry(self.provider.entity_id().clone()),
            self.path.clone(),
        )
        .unwrap()
    }
}

/// A fixture whose engine carries an explicit compaction setting.
fn fixture_with_retention(retention_ns: Option<u64>) -> Fixture {
    let f = fixture();
    let engine = PaymentEngine::new(
        f.provider.clone(),
        Arc::new(MockFacilitator::new()),
        Arc::new(AdmitAll),
        default_mock_registry(f.provider.entity_id().clone()),
        f.path.clone(),
    )
    .unwrap()
    .with_billing_log(Arc::new(BillingLog::new(
        f.path.with_file_name("billing2.jsonl"),
    )))
    .with_terminal_record_retention_ns(retention_ns);
    Fixture {
        engine: Arc::new(engine),
        ..f
    }
}

/// An engine with a billing log attached — required for a record to reach
/// `billing_published`, and therefore to ever become prunable.
fn fixture() -> Fixture {
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
        .unwrap()
        .with_billing_log(Arc::new(BillingLog::new(dir.path().join("billing.jsonl")))),
    );
    Fixture {
        engine,
        provider,
        caller,
        path,
        _dir: dir,
    }
}

/// Drive one quote all the way to terminal: settled, billed, published,
/// redeemed. Returns the quote and its settlement transaction id.
async fn terminal_quote(f: &Fixture, amount: &str, nonce: &str) -> (PaymentQuote, String) {
    let quote = f
        .engine
        .issue_quote(
            f.caller.entity_id().clone(),
            CAP,
            mock_reqs(amount),
            NOW,
            TTL,
        )
        .unwrap();
    let decision = f
        .engine
        .accept_payment(
            &quote,
            &payload(&quote, nonce),
            VerificationTier::Observed,
            NOW,
        )
        .await
        .unwrap();
    assert!(
        matches!(decision, PaymentDecision::Served { .. }),
        "fixture quote must settle: {decision:?}"
    );
    let transaction = f
        .engine
        .status(&quote.quote_id)
        .await
        .unwrap()
        .expect("record exists")
        .chain
        .last()
        .and_then(|e| e.transaction.clone())
        .expect("a settled quote has a transaction");
    assert_eq!(
        f.engine
            .redeem_for_invocation(TOOL, &quote.quote_id, None)
            .await
            .unwrap(),
        RedeemDecision::Admitted
    );
    (quote, transaction)
}

/// Read the raw store, for assertions about state the public API hides
/// (the replay maps).
async fn raw_state(path: &std::path::Path) -> serde_json::Value {
    let bytes = tokio::fs::read(path).await.expect("store exists");
    serde_json::from_slice(&bytes).expect("store parses")
}

// ============================================================================
// The expiry floor
// ============================================================================

/// A terminal record is NOT removed before its quote's expiry horizon —
/// even though it is billed, published, and redeemed. The floor is what
/// keeps deletion from becoming resurrection.
#[tokio::test]
async fn a_terminal_record_survives_until_the_horizon() {
    let f = fixture();
    let (quote, _) = terminal_quote(&f, "2500", "n1").await;

    // Immediately after: terminal, but nowhere near the horizon.
    assert_eq!(f.engine.prune_terminal_records(NOW).await.unwrap(), 0);
    assert!(f.engine.status(&quote.quote_id).await.unwrap().is_some());

    // One nanosecond before the horizon: still retained.
    let horizon = quote.expires_at_ns + DEFAULT_TERMINAL_RECORD_RETENTION_NS;
    assert_eq!(
        f.engine.prune_terminal_records(horizon - 1).await.unwrap(),
        0
    );
    assert!(
        f.engine.status(&quote.quote_id).await.unwrap().is_some(),
        "a record must not retire one tick early"
    );

    // At the horizon: retired.
    assert_eq!(f.engine.prune_terminal_records(horizon).await.unwrap(), 1);
    assert!(f.engine.status(&quote.quote_id).await.unwrap().is_none());
}

/// Only *terminal* records retire. A settled-and-billed quote that was
/// never redeemed stays, however old it is — retention is not a way to
/// forget an incomplete lifecycle.
#[tokio::test]
async fn an_unredeemed_record_is_never_pruned() {
    let f = fixture();
    let quote = f
        .engine
        .issue_quote(
            f.caller.entity_id().clone(),
            CAP,
            mock_reqs("2500"),
            NOW,
            TTL,
        )
        .unwrap();
    f.engine
        .accept_payment(
            &quote,
            &payload(&quote, "n1"),
            VerificationTier::Observed,
            NOW,
        )
        .await
        .unwrap();
    // Settled and billed, but never redeemed.
    assert_eq!(
        f.engine
            .prune_terminal_records(past_horizon())
            .await
            .unwrap(),
        0
    );
    assert!(f.engine.status(&quote.quote_id).await.unwrap().is_some());
}

// ============================================================================
// Resurrection
// ============================================================================

/// The load-bearing safety property: after a record is pruned, its own
/// (now long-expired) quote cannot recreate lifecycle state. The expiry
/// check in `accept_payment` runs before the claim transaction, so the
/// pruned quote is refused outright rather than minting a fresh record.
#[tokio::test]
async fn a_pruned_quote_cannot_recreate_lifecycle_state() {
    let f = fixture();
    let (quote, _) = terminal_quote(&f, "2500", "n1").await;
    let later = past_horizon();
    assert_eq!(f.engine.prune_terminal_records(later).await.unwrap(), 1);

    // Re-present the very same quote + payload after the prune.
    let decision = f
        .engine
        .accept_payment(
            &quote,
            &payload(&quote, "n1"),
            VerificationTier::Observed,
            later,
        )
        .await
        .unwrap();
    assert!(
        matches!(
            decision,
            PaymentDecision::Rejected {
                reason: RejectReason::QuoteExpired
            }
        ),
        "a pruned quote must be refused, not re-admitted: {decision:?}"
    );
    assert!(
        f.engine.status(&quote.quote_id).await.unwrap().is_none(),
        "the refused attempt must not have minted a record"
    );

    // And the gate still refuses to serve it.
    assert_eq!(
        f.engine
            .redeem_for_invocation(TOOL, &quote.quote_id, None)
            .await
            .unwrap(),
        RedeemDecision::Denied {
            reason: net_payments::engine::RedeemDenialReason::UnknownQuote
        }
    );
}

// ============================================================================
// The permanent uniqueness index
// ============================================================================

/// **The invariant retention exists to not break.** A settlement
/// transaction stays claimed forever, so replaying it against a *fresh,
/// unexpired* quote is still invalidated long after the original record
/// and its payload guard are gone. Without the permanent tombstone this
/// is exactly "one payment serves twice, provided the attacker waits out
/// retention".
///
/// Driven end to end, not asserted on state: the mock derives its
/// transaction id from the payload bytes, and the payload binds the
/// requirements but not the quote id — so an identical payload presented
/// against a different quote genuinely re-settles the same transaction.
/// Pruning removed the payload replay guard that would otherwise catch it
/// first, which is precisely what leaves the tombstone as the last line.
#[tokio::test]
async fn a_retired_settlement_transaction_is_still_rejected_later() {
    let f = fixture();
    let (old, transaction) = terminal_quote(&f, "2500", "n1").await;
    let later = past_horizon();
    assert_eq!(f.engine.prune_terminal_records(later).await.unwrap(), 1);

    // The record and its payload guard are gone...
    let state = raw_state(&f.path).await;
    assert!(state["quotes"].get(&old.quote_id).is_none());
    assert!(state["consumed"].as_object().unwrap().is_empty());
    // ...but the tombstone remains, still attributing the tx to its quote.
    let tombstones = state["consumed_transactions"].as_object().unwrap();
    assert_eq!(tombstones.len(), 1, "the tombstone survives retention");
    assert!(tombstones.keys().any(|k| k.contains(&transaction)));
    assert!(tombstones.values().any(|v| v == &old.quote_id));

    // A fresh, unexpired quote — same requirements, same payload bytes,
    // therefore the same settlement transaction.
    let fresh = f
        .engine
        .issue_quote(
            f.caller.entity_id().clone(),
            CAP,
            mock_reqs("2500"),
            later,
            TTL,
        )
        .unwrap();
    assert_ne!(fresh.quote_id, old.quote_id);

    let replaying = f.with_forgetful_facilitator();
    let decision = replaying
        .accept_payment(
            &fresh,
            &payload(&fresh, "n1"),
            VerificationTier::Observed,
            later,
        )
        .await
        .unwrap();
    assert!(
        matches!(
            decision,
            PaymentDecision::Invalidated {
                reason: InvalidationReason::Replay
            }
        ),
        "a retired settlement must still be refused against a fresh quote: {decision:?}"
    );

    // Fail-closed: the fresh quote is frozen and never serves.
    let status = f
        .engine
        .status(&fresh.quote_id)
        .await
        .unwrap()
        .expect("the fresh quote has a record");
    assert!(status.frozen.is_some(), "the replaying quote must freeze");
    assert!(!status.served);
    assert_eq!(
        f.engine
            .redeem_for_invocation(TOOL, &fresh.quote_id, None)
            .await
            .unwrap(),
        RedeemDecision::Denied {
            reason: net_payments::engine::RedeemDenialReason::QuoteFrozen {
                freeze_reason: status.frozen.clone().unwrap()
            }
        },
        "no serve against a quote that replayed a retired settlement"
    );
    // The tombstone still belongs to the original quote — a replay never
    // reassigns it.
    assert!(raw_state(&f.path).await["consumed_transactions"]
        .as_object()
        .unwrap()
        .values()
        .any(|v| v == &old.quote_id));
}

/// The payload replay guard retires *with* its record — it protects
/// claim-time concurrency before a transaction is known, and the enduring
/// guard is the tombstone.
#[tokio::test]
async fn the_payload_guard_retires_with_its_record() {
    let f = fixture();
    let (_quote, _) = terminal_quote(&f, "2500", "n1").await;
    assert_eq!(
        raw_state(&f.path).await["consumed"]
            .as_object()
            .unwrap()
            .len(),
        1
    );

    assert_eq!(
        f.engine
            .prune_terminal_records(past_horizon())
            .await
            .unwrap(),
        1
    );
    assert!(
        raw_state(&f.path).await["consumed"]
            .as_object()
            .unwrap()
            .is_empty(),
        "the payload guard retires with the record that owned it"
    );
}

/// Owner-check: a payload entry that has diverged to some *other* quote is
/// never erased by an unrelated record's prune. Corruption or an
/// unexpected migration state must not cost another quote its replay
/// protection.
#[tokio::test]
async fn a_payload_guard_owned_by_another_quote_survives_the_prune() {
    let f = fixture();
    let (quote, _) = terminal_quote(&f, "2500", "n1").await;

    // Rewrite the guard so it names a different owner than the record
    // that is about to retire.
    let mut state = raw_state(&f.path).await;
    let consumed = state["consumed"].as_object_mut().unwrap();
    let hash = consumed.keys().next().unwrap().clone();
    consumed.insert(hash.clone(), serde_json::json!("some-other-quote"));
    tokio::fs::write(&f.path, serde_json::to_vec(&state).unwrap())
        .await
        .unwrap();

    assert_eq!(
        f.engine
            .prune_terminal_records(past_horizon())
            .await
            .unwrap(),
        1
    );

    let after = raw_state(&f.path).await;
    assert!(
        after["quotes"].get(&quote.quote_id).is_none(),
        "the terminal record still retires"
    );
    assert_eq!(
        after["consumed"][&hash], "some-other-quote",
        "another quote's replay guard must survive an unrelated prune"
    );
}

// ============================================================================
// Configuration — a narrow lifecycle-compaction policy
// ============================================================================

/// Compaction is **default-on at 6 hours**. An opt-in default would leave
/// most deployments silently accumulating terminal records forever.
#[tokio::test]
async fn compaction_is_on_by_default_at_six_hours() {
    assert_eq!(
        DEFAULT_TERMINAL_RECORD_RETENTION_NS,
        6 * 60 * 60 * 1_000_000_000
    );
    let f = fixture(); // constructed without touching retention
    let (quote, _) = terminal_quote(&f, "2500", "n1").await;
    assert_eq!(
        f.engine
            .prune_terminal_records(past_horizon())
            .await
            .unwrap(),
        1,
        "an engine nobody configured must still compact"
    );
    assert!(f.engine.status(&quote.quote_id).await.unwrap().is_none());
}

/// `None` is the explicit opt-out: terminal records are kept indefinitely.
#[tokio::test]
async fn none_disables_compaction_entirely() {
    let f = fixture_with_retention(None);
    let (quote, _) = terminal_quote(&f, "2500", "n1").await;
    assert_eq!(
        f.engine.prune_terminal_records(u64::MAX).await.unwrap(),
        0,
        "None must keep terminal records forever"
    );
    assert!(f.engine.status(&quote.quote_id).await.unwrap().is_some());
}

/// A longer window is honored — a deployment wanting a bigger local
/// re-verification / forensic window gets exactly that.
#[tokio::test]
async fn a_longer_window_is_honored() {
    let week = 7 * 24 * 60 * 60 * 1_000_000_000u64;
    let f = fixture_with_retention(Some(week));
    let (quote, _) = terminal_quote(&f, "2500", "n1").await;

    // Past the default horizon, but well inside the configured one.
    assert_eq!(
        f.engine
            .prune_terminal_records(past_horizon())
            .await
            .unwrap(),
        0
    );
    assert!(f.engine.status(&quote.quote_id).await.unwrap().is_some());

    assert_eq!(
        f.engine
            .prune_terminal_records(quote.expires_at_ns + week)
            .await
            .unwrap(),
        1
    );
}

/// `Some(0)` is refused and normalized to the default, never honored as
/// "compact immediately at expiry" — zero reads as "off" to an operator,
/// so honoring it would do the opposite of what they meant. `None` is the
/// opt-out.
#[tokio::test]
async fn zero_is_refused_and_normalized_to_the_default() {
    let f = fixture_with_retention(Some(0));
    let (quote, _) = terminal_quote(&f, "2500", "n1").await;

    // If 0 had been honored, this would already be prunable.
    let just_past_expiry = quote.expires_at_ns + 1;
    assert_eq!(
        f.engine
            .prune_terminal_records(just_past_expiry)
            .await
            .unwrap(),
        0,
        "Some(0) must not become immediate compaction"
    );
    assert!(f.engine.status(&quote.quote_id).await.unwrap().is_some());

    // It behaves exactly as the default does.
    assert_eq!(
        f.engine
            .prune_terminal_records(past_horizon())
            .await
            .unwrap(),
        1
    );
}

/// Settlement tombstones are permanent at **every** setting — including a
/// deliberately tiny window. There is no configuration path to expiring
/// them, by construction: no such knob exists.
#[tokio::test]
async fn tombstones_are_permanent_under_every_retention_setting() {
    for retention in [None, Some(1u64), Some(DEFAULT_TERMINAL_RECORD_RETENTION_NS)] {
        let f = fixture_with_retention(retention);
        terminal_quote(&f, "2500", "n1").await;
        f.engine.prune_terminal_records(u64::MAX).await.unwrap();
        assert_eq!(
            raw_state(&f.path).await["consumed_transactions"]
                .as_object()
                .unwrap()
                .len(),
            1,
            "the settlement tombstone must survive retention = {retention:?}"
        );
    }
}

// ============================================================================
// Migration
// ============================================================================

/// A record written before `expires_at_ns` existed has no authoritative
/// floor to measure from, so it is never prunable — retention fails closed
/// rather than inferring an expiry (notably never from the x402
/// `maxTimeoutSeconds`, which is advisory).
#[tokio::test]
async fn legacy_records_without_an_expiry_are_never_pruned() {
    let f = fixture();
    let (quote, _) = terminal_quote(&f, "2500", "n1").await;

    // Age the store back to the pre-field schema.
    let mut state = raw_state(&f.path).await;
    state["quotes"][&quote.quote_id]
        .as_object_mut()
        .unwrap()
        .remove("expires_at_ns");
    tokio::fs::write(&f.path, serde_json::to_vec(&state).unwrap())
        .await
        .unwrap();

    assert_eq!(
        f.engine.prune_terminal_records(u64::MAX).await.unwrap(),
        0,
        "a record with no authoritative expiry is never prunable"
    );
    assert!(
        f.engine.status(&quote.quote_id).await.unwrap().is_some(),
        "the legacy record is intact"
    );
}

// ============================================================================
// Idempotence + the P5e dirty discipline
// ============================================================================

/// The reported count is the number of records actually removed, not a
/// before/after size difference — so a sweep that retires several reports
/// all of them. Every other count assertion in this suite is 0 or 1, which
/// a `bool`-shaped sweep would satisfy just as well.
#[tokio::test]
async fn a_sweep_reports_every_record_it_retires() {
    let f = fixture();
    for (amount, nonce) in [("2500", "n1"), ("2600", "n2"), ("2700", "n3")] {
        terminal_quote(&f, amount, nonce).await;
    }
    assert_eq!(
        raw_state(&f.path).await["quotes"]
            .as_object()
            .unwrap()
            .len(),
        3
    );

    assert_eq!(
        f.engine
            .prune_terminal_records(past_horizon())
            .await
            .unwrap(),
        3,
        "the sweep reports each record it retired"
    );
    assert!(raw_state(&f.path).await["quotes"]
        .as_object()
        .unwrap()
        .is_empty());
}

/// A second identical sweep finds nothing and reports nothing removed.
#[tokio::test]
async fn a_second_sweep_is_a_no_op() {
    let f = fixture();
    terminal_quote(&f, "2500", "n1").await;
    let later = past_horizon();
    assert_eq!(f.engine.prune_terminal_records(later).await.unwrap(), 1);
    assert_eq!(
        f.engine.prune_terminal_records(later).await.unwrap(),
        0,
        "the sweep is idempotent"
    );
}

/// The P5e trap: a sweep is a real mutation, so it must persist even when
/// the surrounding transaction was otherwise read-only — and once clean,
/// an equivalent transaction must not rewrite. Witnessed by the store
/// inode (a save renames a fresh temp over the file), so unix-only,
/// matching `read_only_writes_audit.rs`.
#[cfg(unix)]
#[tokio::test]
async fn a_sweep_persists_on_an_otherwise_clean_pass_then_settles() {
    use std::os::unix::fs::MetadataExt as _;
    let f = fixture();
    terminal_quote(&f, "2500", "n1").await;
    let later = past_horizon();

    let before = std::fs::metadata(&f.path).unwrap().ino();
    assert_eq!(f.engine.prune_terminal_records(later).await.unwrap(), 1);
    let after_prune = std::fs::metadata(&f.path).unwrap().ino();
    assert_ne!(
        before, after_prune,
        "a sweep that removed a record must persist"
    );

    // Nothing left to prune: the equivalent pass is clean and must not
    // rewrite the store.
    assert_eq!(f.engine.prune_terminal_records(later).await.unwrap(), 0);
    assert_eq!(
        std::fs::metadata(&f.path).unwrap().ino(),
        after_prune,
        "a sweep with nothing to remove must not rewrite the store"
    );
}

/// Retention runs where records are minted: accepting a new payment
/// retires eligible old ones in the same locked transaction, so a provider
/// under steady load never needs the explicit sweep.
#[tokio::test]
async fn accepting_a_payment_sweeps_eligible_records() {
    let f = fixture();
    let (old, _) = terminal_quote(&f, "2500", "n1").await;
    let later = past_horizon();

    // A fresh quote, issued long after the first one expired.
    let fresh = f
        .engine
        .issue_quote(
            f.caller.entity_id().clone(),
            CAP,
            mock_reqs("2500"),
            later,
            TTL,
        )
        .unwrap();
    let decision = f
        .engine
        .accept_payment(
            &fresh,
            &payload(&fresh, "n2"),
            VerificationTier::Observed,
            later,
        )
        .await
        .unwrap();
    assert!(matches!(decision, PaymentDecision::Served { .. }));

    assert!(
        f.engine.status(&old.quote_id).await.unwrap().is_none(),
        "accepting a payment retires eligible terminal records"
    );
    assert!(
        f.engine.status(&fresh.quote_id).await.unwrap().is_some(),
        "the quote being accepted is untouched"
    );
    // The retired quote's settlement is still claimed.
    assert_eq!(
        raw_state(&f.path).await["consumed_transactions"]
            .as_object()
            .unwrap()
            .len(),
        2,
        "tombstones accumulate across retention — both settlements stay claimed"
    );
}
