//! The provider-side paid-invocation lifecycle engine (Workstream 2).
//!
//! Drives Mode A (pay-before-serve) against any [`Facilitator`]: quote →
//! payload → verify → settle → verification chain → billing event. The
//! engine owns the state that makes the lifecycle safe to retry:
//!
//! - **Consumed-payload replay index** — one payload satisfies exactly
//!   one quote, persisted through the locked store (pins pattern), so a
//!   replay across process restarts still bounces.
//! - **Idempotency** — the `{caller, provider, capability, quote}` key;
//!   same-key retry returns the *same* billing event id and never settles
//!   twice (one settle, one serve, one billing event).
//! - **Verification chains** — every facilitator answer becomes a signed
//!   [`VerificationEvent`] chained per quote; `invalidated {reorg}`
//!   freezes further serving against that quote, and billing events are
//!   never rewritten — later events reference them.
//! - **Fail-closed** — a facilitator failure is a structured, retryable
//!   decision for policy, never a silent serve.
//!
//! Provider admission runs at quote issuance and **only** there (never
//! quote a caller you'd deny — accepting a denied caller's payment creates
//! refund obligations P0 doesn't have). [`PaymentEngine::accept_payment`]
//! and [`PaymentEngine::redeem_for_invocation`] deliberately do not
//! re-run it: a signed quote is a commitment that stays redeemable until
//! it expires, so **the quote TTL is the revocation window** — see
//! [`ProviderAdmissionPolicy`]. The gate before the handler enforces
//! *payment* (settled, billed, unfrozen, bound to this tool, unredeemed),
//! not admission.
//!
//! The engine holds `Arc<dyn Facilitator>` — pointing P1 at a real
//! facilitator is construction config, zero interface changes (that's
//! the acceptance test of the design).
//!
//! Locks are held only across state mutations, never across facilitator
//! I/O; an `in_flight` mark keeps concurrent same-key retries from
//! double-settling in between.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;

use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine as _;
use net::adapter::net::identity::{EntityId, EntityKeypair};
use serde::{Deserialize, Serialize};

use crate::billing::{BillingError, BillingLog};
use crate::checker::{ChainChecker, ChainVerdict, TransferQuery};
use crate::core::billing_event::BillingEvent;
use crate::core::canonical::{EnvelopeError, ExtraFields, SignedEnvelope};
use crate::core::idempotency::IdempotencyScope;
use crate::core::quote::PaymentQuote;
use crate::core::registry::{AssetRegistry, RegistryError, RegistryRef};
use crate::core::units::AtomicAmount;
use crate::core::verification::{
    ExceptionKind, InvalidationReason, VerificationEvent, VerificationStatus, VerificationTier,
};
use crate::core::versioning::{TAG_BILLING_EVENT, TAG_PAYMENT_VERIFICATION};
use crate::facilitator::{Facilitator, FacilitatorErrorKind};
use crate::policy::store::{load_json, mutate_json, mutate_json_if_changed, StoreError};
use crate::x402::payload::PaymentPayload;
use crate::x402::requirements::PaymentRequirements;
use crate::x402::{X402Carry, X402Error};

/// Hard engine failures (store I/O, signing, decode). Domain outcomes are
/// [`PaymentDecision`], not errors.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum EngineError {
    #[error(transparent)]
    Store(#[from] StoreError),
    #[error(transparent)]
    Envelope(#[from] EnvelopeError),
    #[error(transparent)]
    Registry(#[from] RegistryError),
    #[error(transparent)]
    X402(#[from] X402Error),
    #[error("admission denied: {0}")]
    AdmissionDenied(String),
    #[error("engine state inconsistent: {0}")]
    State(String),
    /// The billing log could not record an emitted event. Loud and
    /// fail-closed: the event is already durable in engine state, but a
    /// provider whose billing stream is broken should stop serving, not
    /// serve unrecorded.
    #[error(transparent)]
    Billing(#[from] BillingError),
}

/// Terminal rejections of a payment attempt. Fail-closed: every variant
/// means the handler does not run.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RejectReason {
    #[error("quote expired")]
    QuoteExpired,
    #[error("quote is frozen: {0}")]
    QuoteFrozen(String),
    #[error("quote invalid: {0}")]
    BadQuote(String),
    #[error("payload does not accept the quoted requirements")]
    PayloadMismatch,
    #[error("payload already consumed by another quote")]
    Replay,
    #[error("quote already has a different payment attached")]
    QuoteAlreadyPaid,
    #[error("facilitator verify rejected: {0}")]
    VerifyRejected(String),
    #[error("facilitator settle failed: {0}")]
    SettleFailed(String),
}

/// The engine's answer to a payment attempt.
#[derive(Debug, Clone)]
pub enum PaymentDecision {
    /// Payment verified at (or above) the required tier — the handler may
    /// run. Same-key retries return this same billing event.
    Served {
        billing: Box<BillingEvent>,
        tier: VerificationTier,
    },
    /// Settled, but confidence hasn't reached the required tier yet.
    /// Re-verify later; the handler does not run.
    PendingTier {
        reached: VerificationTier,
        required: VerificationTier,
    },
    /// A verification exception (e.g. overpayment) for provider policy to
    /// handle manually. The verifier never auto-satisfies.
    Exception { kind: ExceptionKind },
    /// A previously-verified payment was withdrawn (reorg &c). The quote
    /// is frozen; nothing further serves against it.
    Invalidated { reason: InvalidationReason },
    /// Another attempt on the same key is mid-flight right now.
    InProgress,
    /// Terminal rejection.
    Rejected { reason: RejectReason },
    /// The facilitator could not answer. Fail-closed default; policy
    /// chooses retry / fallback. Nothing was consumed.
    FacilitatorFailure {
        kind: FacilitatorErrorKind,
        retryable: bool,
        message: String,
    },
}

/// Provider-side admission: never quote a caller you'd deny.
///
/// **Evaluated at quote issuance only.** There is exactly one call site
/// ([`PaymentEngine::issue_quote`]), and that is the design, not an
/// omission: a signed quote is a commitment, so it remains redeemable
/// for its full validity window even if the caller's admission status
/// changes afterwards. Consequences worth stating plainly:
///
/// - **The quote TTL is the revocation window.** Revoking a caller stops
///   *new* quotes, not outstanding ones. A provider that needs revocation
///   to bite within `N` seconds issues quotes with a TTL of `N`.
/// - Re-checking at settlement or redemption would mean refusing a caller
///   after they had paid, which needs a refund path the protocol does not
///   have (`net.payment.dispute@1` is reserved, with no semantics).
///
/// Admission is also **not** an authentication boundary: it is evaluated
/// against whatever caller identity the quote records. Establishing that
/// the requester *is* that caller is a separate concern, upstream of this
/// trait.
pub trait ProviderAdmissionPolicy: Send + Sync {
    /// `Err(reason)` refuses quote issuance for this caller/capability.
    fn admit(&self, caller: &EntityId, capability: &str) -> Result<(), String>;
}

/// Admit-everyone policy for tests and dev harnesses only — WS4 wires
/// real provider policy (caller allowlists, attestation, exposure caps).
pub struct AdmitAll;
impl ProviderAdmissionPolicy for AdmitAll {
    fn admit(&self, _caller: &EntityId, _capability: &str) -> Result<(), String> {
        Ok(())
    }
}

/// Why the invocation gate refused. The typed source of truth for both
/// renderings of a redeem denial: `Display` is the human message the
/// error body has always carried (the exact pre-existing strings —
/// pinned by test), and [`wire_reason`](Self::wire_reason) is the
/// stable `reason` token the gates render into a
/// `net.payment.failure@1` schematic. Never parsed back out of strings.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RedeemDenialReason {
    #[error("unknown quote — no payment exists for this invocation")]
    UnknownQuote,
    #[error("invocation binding is not a 64-byte signature")]
    BindingMalformed,
    #[error("invocation binding signature does not verify against the paying identity")]
    BindingRejected,
    #[error("payer identity corrupt in record")]
    PayerRecordCorrupt,
    #[error("quote is frozen ({freeze_reason}) — nothing serves against it")]
    QuoteFrozen { freeze_reason: String },
    /// A payment attempt was claimed but no settlement was ever
    /// recorded — mid-flight right now, or a crash-interrupted attempt
    /// awaiting TTL reclaim (the M3 state). The payment never
    /// completed. (A quote never attempted at all has no record —
    /// records are minted at claim time and a released claim removes
    /// them — so it denies as [`UnknownQuote`](Self::UnknownQuote).)
    #[error("quote is not settled/billed — the payment never completed")]
    NotSettled,
    /// A settlement is recorded but hasn't completed to billing
    /// (awaiting tier / re-verify, or held as an exception). Split from
    /// [`NotSettled`](Self::NotSettled): an incomplete payment and
    /// "paid, awaiting confidence" route differently.
    #[error("quote settlement is recorded but not yet billed — awaiting verification confidence")]
    SettlementPending,
    #[error("quote is bound to capability `{capability}`, not to tool `{tool_id}`")]
    WrongToolBinding { capability: String, tool_id: String },
    #[error("quote already redeemed — one payment, one serve")]
    AlreadyRedeemed,
    /// The provider requires the invocation binding and none was
    /// presented. Distinct from [`BindingMalformed`](Self::BindingMalformed)
    /// (a binding arrived but was not 64 bytes) and from
    /// [`BindingRejected`](Self::BindingRejected) (it verified against the
    /// wrong identity): this caller sent no proof of possession at all,
    /// which is a client-configuration gap, not an attack signal.
    #[error("this provider requires the invocation binding — no possession proof was presented")]
    BindingRequired,
    /// The quote does not commit to the input the provider is being asked
    /// to admit. Task-path only ([`PaymentEngine::redeem_for_task`]):
    /// admission there names one reservation of one owner's one unit of
    /// work, so a proof minted under a different reservation — or under
    /// none — computes a different expected hash here and is refused
    /// before the redeemed/idempotency arm is ever reached.
    #[error(
        "quote does not bind the input being admitted — this payment authorizes a different \
         unit of work"
    )]
    InputBindingMismatch,
}

impl RedeemDenialReason {
    /// The stable snake_case `reason` token for the failure schematic.
    /// Additive-only within `net.payment.failure@1`.
    pub fn wire_reason(&self) -> &'static str {
        match self {
            Self::UnknownQuote => "unknown_quote",
            Self::BindingMalformed => "binding_malformed",
            Self::BindingRejected => "binding_rejected",
            Self::PayerRecordCorrupt => "payer_record_corrupt",
            Self::QuoteFrozen { .. } => "quote_frozen",
            Self::NotSettled => "not_settled",
            Self::SettlementPending => "settlement_pending",
            Self::WrongToolBinding { .. } => "wrong_tool_binding",
            Self::AlreadyRedeemed => "already_redeemed",
            Self::BindingRequired => "binding_required",
            Self::InputBindingMismatch => "input_binding_mismatch",
        }
    }

    /// The redaction-safe rendering for the schematic's `message` field.
    /// Identical to `Display` for every reason whose text is built only
    /// from typed fields — but `QuoteFrozen`'s `Display` interpolates the
    /// free-form `freeze_reason` (provider- and facilitator-supplied
    /// invalidation text), which must not ride the structured header per
    /// the schematic's redaction contract. That text stays on the human
    /// error body (`Display`) alone; the schematic carries a generic
    /// frozen message. When typed freeze subreasons land
    /// (`quote_frozen_replay | _wrong_chain | _reorg | _amount`), the
    /// schematic's `reason` narrows and this generic message is replaced
    /// by the typed rendering.
    pub fn schematic_message(&self) -> String {
        match self {
            Self::QuoteFrozen { .. } => "quote is frozen — nothing serves against it".to_string(),
            other => other.to_string(),
        }
    }
}

/// The provider-side invocation gate's verdict
/// ([`PaymentEngine::redeem_for_invocation`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RedeemDecision {
    /// The quote's one invocation is admitted (and now consumed).
    ///
    /// `payer` is the identity that paid for this quote, decoded from the
    /// record's `caller_hex` — i.e. `PaymentQuote::caller`, the identity
    /// the provider admitted at issuance, signed into the quote, and
    /// billed. It is the same key the invocation binding was verified
    /// against, so on the binding-present path it is a proven payer and
    /// not merely a recorded one.
    Admitted { payer: EntityId },
    /// Fail-closed rejection; the typed reason renders the human
    /// message (its `Display`) that travels to the caller.
    Denied { reason: RedeemDenialReason },
}

/// The invocation-binding transcript: what the paying identity signs to
/// prove the invoker *is* the payer, not merely someone who saw the
/// quote id. Domain-separated + length-prefixed (no boundary
/// confusion); covers the quote and the tool being invoked.
pub fn invocation_binding_transcript(quote_id: &str, tool_id: &str) -> Vec<u8> {
    const DOMAIN: &[u8] = b"net.payments.invocation_binding@1";
    let mut out = Vec::with_capacity(DOMAIN.len() + 16 + quote_id.len() + tool_id.len());
    out.extend_from_slice(DOMAIN);
    for part in [quote_id.as_bytes(), tool_id.as_bytes()] {
        out.extend_from_slice(&(part.len() as u64).to_le_bytes());
        out.extend_from_slice(part);
    }
    out
}

/// Read-only snapshot of a quote's lifecycle state.
#[derive(Debug, Clone)]
pub struct QuoteStatus {
    pub frozen: Option<String>,
    pub served: bool,
    /// Highest verified tier reached, if any verification succeeded.
    pub tier: Option<VerificationTier>,
    pub billing_event_id: Option<String>,
    /// The full signed verification chain, in order.
    pub chain: Vec<VerificationEvent>,
}

// ---------------------------------------------------------------------
// Persistent state (locked-store backed)
// ---------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
struct QuoteRecord {
    idempotency_key: String,
    payload_hash: String,
    capability: String,
    caller_hex: String,
    /// The exact bytes needed to re-verify later, byte-preserved.
    requirements_b64: String,
    payload_b64: String,
    /// The quote's **authoritative** expiry — the envelope's, never x402's
    /// advisory `maxTimeoutSeconds`. Retention's hard floor: a terminal
    /// record is only ever removed well after the quote that minted it
    /// stopped being redeemable (see [`QuoteRecord::is_prunable_at`]).
    ///
    /// Optional for migration, and deliberately so in both directions: a
    /// record written by an older build carries no expiry, and a
    /// mandatory `u64` would make those stores fail to deserialize
    /// (`StoreError::Corrupt` — loud, but total). Defaulting to `0`
    /// instead would be worse: every legacy record would look infinitely
    /// expired and become immediately prunable. `None` therefore means
    /// **never prunable** — retention fails closed rather than guessing.
    #[serde(default)]
    expires_at_ns: Option<u64>,
    in_flight: bool,
    /// When `in_flight` was last asserted (engine time, ns). A crash
    /// between claim and completion — verify/settle run with no lock held
    /// — would otherwise strand the quote `in_flight` forever; a retry
    /// after `in_flight_ttl_ns` reclaims it. `None` on a legacy record is
    /// treated as immediately stale so old stuck records can recover.
    #[serde(default)]
    in_flight_since_ns: Option<u64>,
    frozen: Option<String>,
    served: bool,
    /// The quote's input commitment (`PaymentQuote::input_hash`), carried
    /// onto the record so the task-admission gate can compare a presented
    /// purchase hash against what the payer actually bought. `None` for
    /// every capability-level quote (the P0 static-pricing shape) and for
    /// every record written before the field existed.
    #[serde(default)]
    input_hash: Option<String>,
    /// Whether the paid invocation was executed against this quote —
    /// set (at most once) by [`PaymentEngine::redeem_for_invocation`],
    /// the provider-side gate's check. Additive: pre-existing records
    /// default to unredeemed.
    #[serde(default)]
    redeemed: bool,
    /// *Which* admission consumed this quote, for the task path only:
    /// the `expected_input_hash` [`PaymentEngine::redeem_for_task`]
    /// admitted. It is what makes that path idempotent per purchase
    /// rather than strictly at-most-once — a provider that crashed
    /// between the engine write and its own journal write reconciles on
    /// retry instead of being told `already_redeemed`. `None` on a record
    /// redeemed through [`PaymentEngine::redeem_for_invocation`] (which
    /// stays strictly at-most-once) and on every legacy record.
    #[serde(default)]
    redeemed_for: Option<String>,
    #[serde(default)]
    chain: Vec<VerificationEvent>,
    #[serde(default)]
    billing: Option<BillingEvent>,
    /// Whether `billing` has been durably appended to the attached billing
    /// log. The event is committed to state at completion but the log
    /// append happens after the lock (and to a different file); if that
    /// append is lost (I/O failure or a crash before this flag is set) an
    /// idempotent retry re-publishes it. `false` on a legacy record simply
    /// means a retry will (idempotently) try once more.
    #[serde(default)]
    billing_published: bool,
}

/// What survives retention of a **redeemed** record: the minimum needed
/// to answer one question truthfully forever — "was this quote paid, and
/// which admission consumed it?"
///
/// Retention exists because a full `QuoteRecord` carries the preserved
/// requirement/payload carries and the whole verification chain, and
/// once the billing event is in the `BillingLog` that bulk is
/// bookkeeping. But the record was *also* the redemption gate's memory,
/// and the provider's admission write is a **second** durable step in a
/// **different** file: `redeem_for_task` marks the quote consumed, then
/// the provider journals `Paid`. A crash in between leaves a settled
/// payment whose admission is not yet recorded anywhere, and the
/// engine's own record became prunable the moment it was marked
/// redeemed. Compacting it there answered the provider's retry
/// `unknown_quote` — "no funds moved, buy another quote" — about a
/// payment that had settled, been billed, and been consumed by exactly
/// this admission. The offer's admission retention is measured in days;
/// the engine's horizon in hours, so the window is real rather than
/// theoretical.
///
/// Bounded on purpose: six small fields, no carries, no chain, no
/// billing event. The charge itself lives in the `BillingLog` (which is
/// *why* a record is allowed to retire at all), so this is not a second
/// copy of the evidence — it is the redemption authority, kept so a
/// compacted purchase can still be re-admitted for the admission that
/// bought it and can never be described as "never paid".
///
/// Permanent, for the same reason `consumed_transactions` is: "this
/// payment was made and this admission consumed it" does not stop being
/// true, and there is no engine-level invariant that says when it is
/// safe to forget. One entry per retired paid+redeemed quote.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct RedemptionTombstone {
    /// The capability the payment bought — the tool-binding check, which
    /// a compacted record must still apply.
    capability: String,
    /// The payer, lowercase hex. A presented invocation binding is
    /// verified against it exactly as the live record did.
    caller_hex: String,
    /// The quote's input commitment, if it had one. Compared before the
    /// consumed-by check, so a proof for other work learns nothing about
    /// this purchase.
    input_hash: Option<String>,
    /// Which admission consumed the quote (`redeem_for_task`'s
    /// `expected_input_hash`), or `None` when the strictly-at-most-once
    /// bearer invocation gate consumed it.
    redeemed_for: Option<String>,
    /// The billing event id, so an operator handed this quote can find
    /// the charge in the `BillingLog` the record was retired in favour
    /// of.
    billing_id: Option<String>,
    /// When retention retired the record (engine time, ns).
    retired_at_ns: u64,
}

/// Struct wrapper (not a bare map) so a schema-version field can land
/// without a breaking format change — same rationale as the pin store.
#[derive(Debug, Default, Serialize, Deserialize)]
struct EngineState {
    /// payload content hash → the one quote it satisfies.
    #[serde(default)]
    consumed: BTreeMap<String, String>,
    /// `network|transaction` → the one quote that settlement satisfies.
    /// The facilitator-receipt-replay guard: a facilitator (or a replayed
    /// response) presenting the same transaction for a second quote is
    /// invalidated — one on-chain settlement never serves twice.
    #[serde(default)]
    consumed_transactions: BTreeMap<String, String>,
    /// quote_id → lifecycle record.
    #[serde(default)]
    quotes: BTreeMap<String, QuoteRecord>,
    /// quote_id → the redemption authority left behind when retention
    /// retires a redeemed record. Written only by `prune_terminal`, read
    /// only by the two redemption gates.
    #[serde(default)]
    redemptions: BTreeMap<String, RedemptionTombstone>,
}

impl RedemptionTombstone {
    /// [`PaymentEngine::redeem_preconditions`], restricted to the checks
    /// a compacted record can still make — and it can still make every
    /// one that could fail here.
    ///
    /// Binding shape, payer identity and binding signature are all
    /// carried. The two remaining live-record gates are satisfied by
    /// construction rather than skipped: `is_prunable_at` refuses to
    /// retire a **frozen** record (so a tombstone never stands for one)
    /// and requires `billing.is_some()` (so `NotSettled` and
    /// `SettlementPending` are unreachable). The tool binding is
    /// re-derived from the same capability string by the same rule.
    fn preconditions(
        &self,
        quote_id: &str,
        tool_id: &str,
        binding: Option<&[u8]>,
    ) -> Result<EntityId, RedeemDenialReason> {
        let sig_bytes = match binding {
            Some(sig) => {
                Some(<&[u8; 64]>::try_from(sig).map_err(|_| RedeemDenialReason::BindingMalformed)?)
            }
            None => None,
        };
        let payer = hex::decode(&self.caller_hex)
            .ok()
            .and_then(|b| <[u8; 32]>::try_from(b).ok())
            .map(EntityId::from_bytes)
            .ok_or(RedeemDenialReason::PayerRecordCorrupt)?;
        if let Some(sig_bytes) = sig_bytes {
            let transcript = invocation_binding_transcript(quote_id, tool_id);
            if payer.verify_bytes(&transcript, sig_bytes).is_err() {
                return Err(RedeemDenialReason::BindingRejected);
            }
        }
        let bound_tool = self
            .capability
            .split_once('/')
            .map(|(_, tool)| tool)
            .unwrap_or(self.capability.as_str());
        if bound_tool != tool_id {
            return Err(RedeemDenialReason::WrongToolBinding {
                capability: self.capability.clone(),
                tool_id: tool_id.to_string(),
            });
        }
        Ok(payer)
    }
}

impl QuoteRecord {
    /// Whether retention may remove this record at `now_ns`.
    ///
    /// Terminal means the payment fully completed *and* its billing event
    /// is already durable somewhere retention cannot destroy: the
    /// `BillingLog` (`billing_published`), which is the audit surface.
    /// The record itself is then engine bookkeeping, not evidence.
    ///
    /// "Not evidence" is a claim about the *charge*, which the
    /// `BillingLog` holds. It was never true of the **redemption
    /// authority** the same record carried, so that part is retained
    /// past the horizon instead of dying with it — see
    /// [`RedemptionTombstone`].
    ///
    /// The expiry floor is what keeps deletion from becoming
    /// resurrection. If a record were removed while its signed quote were
    /// still valid, re-presenting that quote would miss in `s.quotes` and
    /// mint a *fresh* record (`accept_payment`'s claim closure), and the
    /// lifecycle could eventually serve a second time — the replay maps
    /// do not carry the terminal/idempotency outcome, so they cannot
    /// stand in for the record here. (`accept_payment` also refuses an
    /// expired quote that has no authoritative outcome to recover —
    /// `Claim::Expired`, decided inside the claim transaction — so this
    /// is the second of two independent guards, not the only one.)
    ///
    /// **A frozen record is never terminal**, whatever else is set on it.
    /// Freezing is not confined to the pre-redemption lifecycle: a
    /// `re_verify_with_checker` pass that finds the settlement REVERTED
    /// freezes a record that is already billed, published, and redeemed —
    /// so without this the record would satisfy every other condition and
    /// retire on the ordinary horizon. What it takes with it is the one
    /// thing worth keeping: the evidence that this provider served against
    /// a settlement the chain later said never landed. "Fully completed"
    /// and "completed, then found invalid" are not the same lifecycle, and
    /// only the first is redundant with the `BillingLog`.
    fn is_prunable_at(&self, now_ns: u64, retention_ns: u64, tolerance_ns: u64) -> bool {
        // A record with no authoritative expiry (written by a build
        // before the field existed) has no floor to measure from and is
        // never prunable. Fail closed; never infer one from the x402
        // `maxTimeoutSeconds`, which is advisory.
        let Some(expiry) = self.expires_at_ns else {
            return false;
        };
        self.frozen.is_none()
            && self.billing.is_some()
            && self.billing_published
            && self.redeemed
            && !self.in_flight
            && now_ns
                >= expiry
                    .saturating_add(tolerance_ns)
                    .saturating_add(retention_ns)
    }
}

/// Retention sweep: drop terminal quote records past the horizon, plus the
/// payload replay entry each one owns, leaving a [`RedemptionTombstone`]
/// behind for each. Returns **how many records were
/// retired**, counted as they are removed rather than derived from a
/// before/after size difference — the caller folds a non-zero count into
/// its `dirty` flag, because a sweep is a real mutation and must persist
/// even when the surrounding operation was otherwise read-only (the P5e
/// discipline; see
/// `docs/internal/performance/payments-spend-contention.md`).
///
/// **`consumed_transactions` is never touched.** It is a permanent
/// uniqueness index, not retention state: a settlement that occurred stays
/// a settlement that occurred, and the generic engine/checker path does not
/// universally establish that a transaction fell inside a given quote's
/// validity interval. Expiring these entries would mean one payment can
/// serve twice provided the attacker waits long enough. Bounding *their*
/// storage needs a scheme-level invariant about when a settlement identity
/// may be forgotten, and bounding their *lookup cost* belongs to the
/// indexed store — neither is retention's business.
fn prune_terminal(
    s: &mut EngineState,
    now_ns: u64,
    retention_ns: Option<u64>,
    tolerance_ns: u64,
) -> usize {
    // Compaction explicitly disabled: keep every terminal record.
    let Some(retention_ns) = retention_ns else {
        return 0;
    };
    let retiring: Vec<String> = s
        .quotes
        .iter()
        .filter(|(_, rec)| rec.is_prunable_at(now_ns, retention_ns, tolerance_ns))
        .map(|(id, _)| id.clone())
        .collect();
    let mut retired = 0;
    for quote_id in &retiring {
        // One lookup: the removal yields the record, so the co-prune below
        // reads the payload hash it owns without a second `get` or a clone.
        let Some(rec) = s.quotes.remove(quote_id) else {
            continue;
        };
        retired += 1;
        // The redemption authority outlives the record. `is_prunable_at`
        // only retires a record that is settled, billed, published AND
        // redeemed, so this always describes a real consumed payment —
        // and the provider's own admission write is a separate durable
        // step that may not have landed yet.
        s.redemptions.insert(
            quote_id.clone(),
            RedemptionTombstone {
                capability: rec.capability.clone(),
                caller_hex: rec.caller_hex.clone(),
                input_hash: rec.input_hash.clone(),
                redeemed_for: rec.redeemed_for.clone(),
                billing_id: rec.billing.as_ref().map(|b| b.billing_event_id.clone()),
                retired_at_ns: now_ns,
            },
        );
        // Co-prune the payload guard this record owns — but only if it is
        // still *this* record's. If ownership has diverged (corruption, or
        // an unexpected migration state) the entry belongs to some other
        // quote's replay protection: leave it. Never erase another quote's
        // guard merely because the retiring record names that hash.
        //
        // What retiring it costs, stated exactly. The payload hash is the
        // claim-time guard, effective before any settlement transaction is
        // known. Afterwards two things stand behind it, and only the second
        // is universal:
        //
        //   [a] the transaction tombstone — but that catches a replay only
        //       when it resolves to the SAME transaction id. It is what
        //       `a_retired_settlement_transaction_is_still_rejected_later`
        //       exercises, because the mock facilitator derives its
        //       transaction id from the payload bytes;
        //   [b] the scheme's own single-use authorization — the EIP-3009
        //       nonce, the SVM transaction's recent blockhash + signature,
        //       the XRPL sequence number. On a real rail this is the guard
        //       that actually holds: re-presenting one authorization does
        //       not settle twice, it fails.
        //
        // So this leans on a scheme-level property, which the paragraph
        // above declines to lean on for `consumed_transactions`. The
        // asymmetry is deliberate and worth naming: "this authorization
        // cannot be spent twice" is a property every supported scheme
        // already provides and the engine merely benefits from, whereas
        // "this settlement identity may now be forgotten" is a claim about
        // when it is safe to STOP remembering — nothing in the schemes
        // establishes it, and being wrong there means one payment serves
        // twice. Relying on the first is not licence to assume the second.
        if s.consumed
            .get(&rec.payload_hash)
            .is_some_and(|owner| owner == quote_id)
        {
            s.consumed.remove(&rec.payload_hash);
        }
    }
    retired
}

enum Claim {
    Fresh,
    AlreadySettled,
    AlreadyServed(Box<BillingEvent>, Option<VerificationTier>, bool),
    InProgress,
    Frozen(String),
    ReplayOtherQuote,
    QuoteAlreadyPaid,
    /// The quote is past its authoritative expiry (plus tolerance) and
    /// this record holds **no** authoritative outcome to recover — so
    /// there is nothing to reconcile and a fresh settlement is refused.
    ///
    /// Decided inside the claim transaction rather than before it, and
    /// strictly after the settled/billed arms, because the two questions
    /// are not independent: "may this payload settle now" is about the
    /// quote's validity window, while "what did this exact purchase
    /// already do" is a fact the record already holds. Asking the first
    /// one first answered a reconciliation of a completed purchase with
    /// `QuoteExpired` — a statement that no money moved, about a payment
    /// that had settled and billed.
    ///
    /// Read-only, like every other non-`Fresh` arm: an expired retry
    /// mints no record.
    Expired,
}

/// The best confidence this record has actually reached.
///
/// A high-water mark, not the last event — because the chain's tiers are
/// not monotonic. `re_verify` mints `Observed` and nothing higher (a
/// facilitator receipt justifies no depth claim), so a facilitator
/// re-check run after `re_verify_with_checker` had established
/// `Confirmed(n)` or `Final` appends a *lower* tier than the record had
/// already earned. Reading the last event reported that as a downgrade —
/// through `QuoteStatus::tier`, whose own doc promises the highest tier
/// reached, and through every idempotent `accept_payment` retry.
///
/// An `Invalidated` event resets the mark. Withdrawing the verifications
/// before it is what invalidation *means*, so a reorged record must not
/// keep advertising confidence its settlement no longer has.
///
/// `Exception` events neither raise nor reset it: an overpayment is an
/// outcome for provider policy, not a statement about chain depth.
fn best_verified_tier(chain: &[VerificationEvent]) -> Option<VerificationTier> {
    chain.iter().fold(None, |best, e| match e.status {
        VerificationStatus::Verified => Some(match best {
            Some(b) if b.satisfies(&e.tier) => b,
            _ => e.tier,
        }),
        VerificationStatus::Invalidated { .. } => None,
        VerificationStatus::Exception { .. } => best,
    })
}

// ---------------------------------------------------------------------
// The engine
// ---------------------------------------------------------------------

/// One provider's payment lifecycle engine.
pub struct PaymentEngine {
    provider: Arc<EntityKeypair>,
    facilitator: Arc<dyn Facilitator>,
    admission: Arc<dyn ProviderAdmissionPolicy>,
    registry: AssetRegistry,
    registry_ref: RegistryRef,
    state_path: PathBuf,
    /// Bounded policy tolerance added to quote expiry (no global clock;
    /// expiry uses signer timestamps).
    expiry_tolerance_ns: u64,
    /// How long a claimed-but-uncompleted quote may stay `in_flight`
    /// before a retry is allowed to reclaim it (crash recovery). Default 5
    /// minutes — comfortably longer than any verify+settle round-trip, so
    /// a genuinely in-progress attempt is never reclaimed out from under
    /// itself, while a crashed one eventually frees up.
    in_flight_ttl_ns: u64,
    /// How long a terminal quote record is kept past its authoritative
    /// expiry before compaction may remove it, or `None` to keep terminal
    /// records indefinitely. Default `Some(6 hours)` — see
    /// [`DEFAULT_TERMINAL_RECORD_RETENTION_NS`].
    terminal_record_retention_ns: Option<u64>,
    /// Optional billing stream: every freshly-emitted billing event is
    /// appended (durable JSONL + in-process subscribers). Idempotent
    /// retries republish nothing — one event per idempotency key.
    billing_log: Option<Arc<BillingLog>>,
    /// Whether [`redeem_for_invocation`](Self::redeem_for_invocation)
    /// refuses a redemption that presents no invocation binding.
    /// See [`with_require_invocation_binding`](Self::with_require_invocation_binding).
    require_invocation_binding: bool,
}

/// Default retention for terminal quote records: 6 hours past authoritative
/// quote expiry (plus the engine's expiry tolerance).
///
/// Sized as an **operational re-verification grace**, not as proof that
/// every reorg was observed — it is deliberately comfortably above the
/// Base pack's ~1h final-depth posture (`FINAL_DEPTH_BASE` = 1800 L2 blocks
/// ≈ 1h at 2s/block), while Solana and XRPL reach deterministic finality
/// faster and configure no depth at all. Compaction does not perform
/// re-verification; it only declines to discard a record something might
/// still want to re-check.
///
/// **The horizon IS the re-verification window, and it is the only gate.**
/// `QuoteRecord::is_prunable_at` does not consult the record's verified
/// tier, so a record served at `observed` — the facilitator's receipt
/// alone, and the facilitator is deliberately not in the trust root —
/// retires on the same clock as one an independent checker drove to
/// `final`. Once it is gone,
/// [`re_verify_with_checker`](PaymentEngine::re_verify_with_checker)
/// answers `BadQuote("unknown quote")` and a revert or reorg can no longer
/// be attributed to that payment at all.
///
/// A `final`-tier precondition was considered and **rejected**: a
/// facilitator receipt caps at `observed`, so a provider that never runs a
/// [`ChainChecker`] — the common deployment, and every mock one — would
/// hold every record at `observed` forever and compaction would silently
/// become a no-op, which is the exact failure it was introduced to fix. The
/// assumption is therefore stated rather than enforced, and it is the
/// operator's:
///
/// > A deployment that re-verifies out of band, on a slower rail, or at a
/// > raised `FINAL_DEPTH_*` must widen this window past its own
/// > re-verification period — or pass `None` to keep records indefinitely.
///
/// Pinned by `engine_retention.rs`'s
/// `an_observed_tier_record_retires_on_the_same_clock_as_a_final_one`, so
/// the tradeoff is red-coupled rather than incidental: a future tier gate
/// breaks that test by design, and its author is meant to read this.
///
/// At 1 000 **redeemed** calls/day this retains ~250 records (~0.8 MB)
/// instead of letting months of fat records ride every whole-file
/// transaction.
///
/// **What this does NOT bound.** That figure is the redeemed steady state,
/// and compaction is not a bound on store size in general — two classes of
/// record are permanent by construction (see `QuoteRecord::is_prunable_at`):
///
/// - **settled, billed, published, never redeemed** — a normal outcome, not
///   an error: the caller pays and then crashes, times out, or simply never
///   invokes. These are full-fat records (both preserved base64 carries);
/// - **frozen** — every invalidation, whether it lands before redemption
///   (network mismatch, transaction replay, amount mismatch) or after one
///   (a checker finding the settlement reverted).
///
/// Neither is an oversight. `redeem_for_invocation` applies no expiry, so a
/// paid entitlement never lapses, and retiring its record would destroy
/// something the caller paid for; a frozen record is evidence. But it means
/// a deployment whose callers pay and abandon accumulates fat records
/// forever, at the linear per-transaction cost compaction exists to
/// contain. Making that class prunable needs a *redemption deadline* — a
/// payment-semantics decision, not a retention one. Until then the growth is
/// at least visible: see [`ENGINE_STORE_SIZE_WARN_RECORDS`].
///
/// **Default-on, explicit opt-out.** Making compaction opt-in would leave
/// most deployments silently accumulating multi-kilobyte terminal records
/// forever and degrading continuously. Past expiry, durable billing
/// publication, and redemption, the full record is redundant lifecycle
/// material — not active authority state.
pub const DEFAULT_TERMINAL_RECORD_RETENTION_NS: u64 = 6 * 3_600 * 1_000_000_000;

/// Record count at which the engine store warns once, on the way up.
///
/// Every engine operation parses and (when dirty) re-serializes the whole
/// file, so store size is a latency term on every payment. Compaction keeps
/// the redeemed population flat, but the classes it cannot retire — see
/// [`DEFAULT_TERMINAL_RECORD_RETENTION_NS`] — grow without bound, and their
/// only symptom is payments getting gradually slower. That is exactly the
/// failure an operator cannot diagnose from the outside, so it gets a log
/// line rather than nothing.
///
/// Emitted from the one site that grows the map, on the single transition
/// `len() == ENGINE_STORE_SIZE_WARN_RECORDS`, so a store sitting above the
/// threshold does not warn on every payment. A store oscillating across it
/// re-warns, which is informative rather than noisy.
pub const ENGINE_STORE_SIZE_WARN_RECORDS: usize = 10_000;

impl PaymentEngine {
    pub fn new(
        provider: Arc<EntityKeypair>,
        facilitator: Arc<dyn Facilitator>,
        admission: Arc<dyn ProviderAdmissionPolicy>,
        registry: AssetRegistry,
        state_path: impl Into<PathBuf>,
    ) -> Result<Self, EngineError> {
        let registry_ref = registry.reference()?;
        Ok(Self {
            provider,
            facilitator,
            admission,
            registry,
            registry_ref,
            state_path: state_path.into(),
            expiry_tolerance_ns: 0,
            in_flight_ttl_ns: 300_000_000_000,
            terminal_record_retention_ns: Some(DEFAULT_TERMINAL_RECORD_RETENTION_NS),
            billing_log: None,
            require_invocation_binding: true,
        })
    }

    /// Set how long terminal quote records are retained past authoritative
    /// quote expiry (default `Some(`[`DEFAULT_TERMINAL_RECORD_RETENTION_NS`]`)`,
    /// 6 hours).
    ///
    /// This is a **narrow lifecycle-compaction policy**, not general
    /// retention configuration:
    ///
    /// - `Some(6h)` — the default;
    /// - `Some(longer)` — a deployment wanting a larger local
    ///   re-verification / forensic window;
    /// - `None` — explicitly keep full terminal records indefinitely;
    /// - `Some(0)` — **refused**, and normalized to the default (see below).
    ///
    /// The safety conditions are **not** configurable at any setting:
    /// nothing prunes before authoritative expiry plus tolerance; only
    /// billed, durably published, redeemed, non-in-flight records are ever
    /// eligible; the payload guard retires atomically and owner-safely with
    /// its record; legacy records lacking an authoritative expiry are never
    /// pruned; and **settlement-transaction tombstones are never removed.**
    /// There is deliberately no knob for tombstone expiry — exposing
    /// "retain transaction ids for N days" would turn a security invariant
    /// into a deployment preference.
    ///
    /// `Some(0)` is refused rather than honored because zero conventionally
    /// reads as "off", while here it would mean the *most* aggressive
    /// setting — compaction the instant a quote expires. An operator
    /// reaching for "disable this" would get the opposite of what they
    /// meant, so the value is rejected, normalized to the default, and
    /// logged. `None` is the way to turn compaction off.
    pub fn with_terminal_record_retention_ns(mut self, retention_ns: Option<u64>) -> Self {
        self.terminal_record_retention_ns = match retention_ns {
            Some(0) => {
                tracing::warn!(
                    default_ns = DEFAULT_TERMINAL_RECORD_RETENTION_NS,
                    "terminal-record retention of 0 is refused (0 reads as \"off\" but would \
                     mean immediate compaction at quote expiry) — normalized to the default; \
                     pass `None` to keep terminal records indefinitely"
                );
                Some(DEFAULT_TERMINAL_RECORD_RETENTION_NS)
            }
            other => other,
        };
        self
    }

    /// Set the expiry comparison tolerance (default 0).
    pub fn with_expiry_tolerance_ns(mut self, tolerance_ns: u64) -> Self {
        self.expiry_tolerance_ns = tolerance_ns;
        self
    }

    /// Set the in-flight reclaim TTL (default 5 minutes). A claimed quote
    /// whose completion never landed (process crash mid verify/settle) is
    /// reclaimable by a retry once this much engine time has passed.
    pub fn with_in_flight_ttl_ns(mut self, ttl_ns: u64) -> Self {
        self.in_flight_ttl_ns = ttl_ns;
        self
    }

    /// Attach the billing stream/export surface.
    pub fn with_billing_log(mut self, log: Arc<BillingLog>) -> Self {
        self.billing_log = Some(log);
        self
    }

    /// Require the invocation binding: refuse to redeem a quote unless the
    /// invoker presents the paying identity's signature over
    /// [`invocation_binding_transcript`].
    ///
    /// **What this closes.** Without it, possession of the quote id is
    /// sufficient to consume the paid invocation, and the quote id is not
    /// a secret in practice: it rides a request header on every paid
    /// invoke, appears in the caller's payment proof, and is carried on
    /// the billing event. Anything that learns one — a log sink, a
    /// support bundle, an audit export — can spend it, and redemption is
    /// at-most-once, so the legitimate payer then gets
    /// `already_redeemed`. Requiring the binding means only the identity
    /// that paid can redeem.
    ///
    /// **What this does not close.** The binding transcript covers the
    /// quote and the tool, and the signature travels beside the quote id
    /// on the invocation itself. An intermediary that observes the *paid
    /// invocation* can copy both headers and front-run it. Closing that
    /// needs channel binding or an authenticated transport identity, not
    /// a bigger transcript — a visible, transferable signature cannot fix
    /// it. Do not document this flag as protection against an on-path
    /// observer.
    ///
    /// **Defaults to `true`.** Bearer redemption is the exposure, so it
    /// is not the default posture — a provider that wants it has to ask,
    /// and the asking is visible at the call site.
    ///
    /// Pass `false` only for a deployment whose callers predate the
    /// binding. A caller built on [`crate::flow::CallerPaymentFlow`]
    /// always signs one when its identity can sign, so that set is small
    /// and shrinking.
    pub fn with_require_invocation_binding(mut self, require: bool) -> Self {
        self.require_invocation_binding = require;
        self
    }

    /// Issue a signed quote. Provider policy runs **here**, before any
    /// value can be accepted; the registry check is the pre-sign
    /// hard-reject.
    ///
    /// `input_hash` binds the quote to one exact unit of work: it feeds
    /// `terms_hash` and therefore the quote id, so two purchases of two
    /// different inputs can never share a quote, and
    /// [`Self::redeem_for_task`] can refuse a proof presented for work
    /// other than the work that was bought. `None` is the capability-level
    /// (static pricing) shape and produces exactly the quote earlier
    /// builds issued.
    #[allow(clippy::too_many_arguments)]
    pub fn issue_quote(
        &self,
        caller: EntityId,
        capability: &str,
        requirements: X402Carry<PaymentRequirements>,
        input_hash: Option<&str>,
        now_ns: u64,
        ttl_ns: u64,
    ) -> Result<PaymentQuote, EngineError> {
        self.admission
            .admit(&caller, capability)
            .map_err(EngineError::AdmissionDenied)?;
        self.registry.check_requirements(requirements.view())?;
        let mut quote = PaymentQuote::new(
            self.provider.entity_id().clone(),
            caller,
            capability,
            input_hash.map(str::to_string),
            requirements,
            self.registry_ref.clone(),
            now_ns,
            now_ns.saturating_add(ttl_ns),
        );
        quote.sign_with(&self.provider)?;
        Ok(quote)
    }

    /// Accept a payment against a quote: the full settle path, or the
    /// idempotent replay of an already-completed one.
    ///
    /// **Expiry is decided inside the claim transaction, not before it**
    /// (the internal `Claim::Expired` outcome). Re-presenting the exact
    /// payload of a purchase that already settled is authenticated
    /// reconciliation of a fact this engine holds, and it has to answer
    /// with that fact at any age; only a *fresh* payment is bounded by
    /// the quote's validity window.
    pub async fn accept_payment(
        &self,
        quote: &PaymentQuote,
        payload: &X402Carry<PaymentPayload>,
        required_tier: VerificationTier,
        now_ns: u64,
    ) -> Result<PaymentDecision, EngineError> {
        // -- static checks: nothing here touches state or the network.
        if let Err(e) = self.check_quote(quote) {
            return Ok(PaymentDecision::Rejected { reason: e });
        }
        if payload.view().accepted != *quote.requirements.view() {
            return Ok(PaymentDecision::Rejected {
                reason: RejectReason::PayloadMismatch,
            });
        }

        // Replay is keyed on the canonical payload, not the preserved carry
        // bytes: two encodings of one authorization must map to a single
        // replay identity so the "one payload → one quote" guard cannot be
        // sidestepped by re-serializing (M2).
        let payload_hash = match payload.replay_key() {
            Ok(k) => k,
            Err(e) => {
                return Ok(PaymentDecision::Rejected {
                    reason: RejectReason::BadQuote(e.to_string()),
                })
            }
        };
        // -- claim: check-and-mark under the lock, then release it before
        // any facilitator I/O.
        let quote_id = quote.quote_id.clone();
        let in_flight_ttl_ns = self.in_flight_ttl_ns;
        let terminal_record_retention_ns = self.terminal_record_retention_ns;
        let expiry_tolerance_ns = self.expiry_tolerance_ns;
        // Evaluated here, applied inside the claim block: the answer to
        // "is this quote still open for a NEW payment" is needed only on
        // the paths that would take one.
        let expired = now_ns >= quote.expires_at_ns.saturating_add(expiry_tolerance_ns);
        let claim = {
            let payload_hash = payload_hash.clone();
            // Only the three `Claim::Fresh` paths mutate state (insert a new
            // record, mark an existing record in_flight, or reclaim a stale
            // in_flight); every other outcome is a read-only inspection.
            // `mutate_json_if_changed` therefore skips the durable write on
            // the read-only outcomes (Frozen / QuoteAlreadyPaid / AlreadyServed
            // / InProgress / AlreadySettled / ReplayOtherQuote / Expired). The
            // dirty flag is `matches!(_, Fresh) || retired > 0`, each disjunct
            // derived from the SAME branch that mutated — so it can never
            // diverge from the mutation. The later writes (completion,
            // `release_claim`, billing republish) are separate calls and remain
            // unconditional, so a `verify_rejected` still persists both its
            // claim (a Fresh here) and its release.
            mutate_json_if_changed::<EngineState, _, _>(&self.state_path, move |s| {
                // Retention runs where records are minted, mirroring the spend
                // engine's counter prune in `check_and_reserve`: the operation
                // that grows the store is the one that retires from it. A
                // sweep is a real mutation, so it makes an otherwise read-only
                // outcome dirty (P5e) — a denial that pruned must persist.
                //
                // This cannot resurrect the quote being accepted: a record is
                // only prunable well past its quote's authoritative expiry,
                // and a quote that far past it has no `Fresh` path out of the
                // claim block below — `Claim::Expired` is the only outcome a
                // pruned-and-re-presented quote can reach. The two guards are
                // independent.
                let retired =
                    prune_terminal(s, now_ns, terminal_record_retention_ns, expiry_tolerance_ns);
                // Read before the `&mut` borrow below, which would
                // otherwise rule out touching `consumed` while holding it.
                let payload_consumed_elsewhere = s
                    .consumed
                    .get(&payload_hash)
                    .is_some_and(|owner| *owner != quote_id);
                // Set when a stale in-flight record is taken over by a
                // different payload: the replay index still names the dead
                // attempt's hash and has to move with the record. Applied
                // once the borrow ends.
                let mut rebind_consumed_from: Option<String> = None;
                let claim: Claim = 'claim: {
                    if let Some(rec) = s.quotes.get_mut(&quote_id) {
                        if let Some(reason) = &rec.frozen {
                            break 'claim Claim::Frozen(reason.clone());
                        }
                        let same_payload = rec.payload_hash == payload_hash;

                        // Commitment first, and only then the payload
                        // comparison.
                        //
                        // This used to be the other way round: any
                        // differing payload was `QuoteAlreadyPaid`, a
                        // terminal rejection, whether or not the record had
                        // actually paid anything. But the claim is taken
                        // *before* the facilitator verifies (verification is
                        // network I/O and does not hold the lock), so an
                        // unverified attempt held the quote against everyone
                        // else with a decision that reads "someone already
                        // paid this".
                        //
                        // The semantic replay key is derived from the
                        // authorization's `(from, nonce)` and scope — all of
                        // which an observer of a real authorization knows.
                        // So the attempt occupying the record need not be
                        // the payer: a forged payload with a garbage
                        // signature claims the same quote just as well, and
                        // the real payer was told their quote was spent.
                        //
                        // A record that has billed or settled *is* bound to
                        // the payload it holds, and a different one there is
                        // genuinely a second payment for one quote. Below
                        // that line, nothing is decided yet.
                        if let Some(billing) = &rec.billing {
                            if !same_payload {
                                break 'claim Claim::QuoteAlreadyPaid;
                            }
                            break 'claim Claim::AlreadyServed(
                                Box::new(billing.clone()),
                                best_verified_tier(&rec.chain),
                                rec.billing_published,
                            );
                        }
                        // Completion is atomic (chain push + in_flight=false
                        // in one commit), so a non-empty chain and
                        // `in_flight` are mutually exclusive and this order
                        // is free to put settlement first.
                        if !rec.chain.is_empty() {
                            if !same_payload {
                                break 'claim Claim::QuoteAlreadyPaid;
                            }
                            break 'claim Claim::AlreadySettled;
                        }
                        // Past this line the record holds no authoritative
                        // outcome, so every remaining arm would take (or wait
                        // on) a NEW settlement — which is exactly what an
                        // expired quote may not have. The two arms above are
                        // deliberately upstream of it: they report what this
                        // exact purchase already did, and that answer does not
                        // decay.
                        if expired {
                            break 'claim Claim::Expired;
                        }
                        if rec.in_flight {
                            // An attempt claimed this and has not finished:
                            // still running, or the process died before it
                            // could release. Reclaim only after the TTL,
                            // refreshing the clock so a concurrent retry
                            // still sees InProgress and only one attempt
                            // re-runs verify/settle.
                            //
                            // `InProgress` is retryable, which is the whole
                            // point — a caller told this comes back and
                            // finds the quote free once the attempt ahead of
                            // it fails verification and releases.
                            let stale = rec
                                .in_flight_since_ns
                                .map(|since| now_ns.saturating_sub(since) >= in_flight_ttl_ns)
                                .unwrap_or(true);
                            if !stale {
                                break 'claim Claim::InProgress;
                            }
                            // Stale, so the attempt holding it is gone.
                            // Taking the record over with a different
                            // payload rebinds it, and the replay index has
                            // to follow — otherwise the dead attempt's hash
                            // stays claimed forever and the live one is
                            // never registered.
                            if !same_payload {
                                if payload_consumed_elsewhere {
                                    break 'claim Claim::ReplayOtherQuote;
                                }
                                rebind_consumed_from = Some(std::mem::replace(
                                    &mut rec.payload_hash,
                                    payload_hash.clone(),
                                ));
                                rec.payload_b64 = BASE64.encode(payload.bytes());
                            }
                            rec.in_flight_since_ns = Some(now_ns);
                            break 'claim Claim::Fresh;
                        }
                        // Not in flight, nothing settled, nothing billed.
                        // `release_claim` removes an unsettled record
                        // outright, so this is unreachable in practice; take
                        // it as a fresh claim rather than invent a state for
                        // it.
                        rec.in_flight = true;
                        rec.in_flight_since_ns = Some(now_ns);
                        break 'claim Claim::Fresh;
                    }
                    // No record at all: nothing to reconcile, so an expired
                    // quote cannot mint one. This is also what keeps retention
                    // from becoming resurrection — a pruned record's quote is
                    // by construction far past its expiry floor.
                    if expired {
                        break 'claim Claim::Expired;
                    }
                    if payload_consumed_elsewhere {
                        break 'claim Claim::ReplayOtherQuote;
                    }
                    // Built here, not before the closure: the two base64
                    // encodes below cover the *whole* preserved carries
                    // (requirements + payload, the bulk of a record's bytes)
                    // and the idempotency key is a blake3 transcript hash.
                    // Every branch above discards the record, so building it
                    // eagerly spent all of that on each duplicate/retry —
                    // and under a duplicate storm all but one attempt takes
                    // one of those branches.
                    let record = QuoteRecord {
                        idempotency_key: IdempotencyScope {
                            caller: quote.caller.clone(),
                            provider: quote.provider.clone(),
                            capability: quote.capability.clone(),
                            quote_id: quote.quote_id.clone(),
                        }
                        .key(),
                        payload_hash: payload_hash.clone(),
                        capability: quote.capability.clone(),
                        caller_hex: hex::encode(quote.caller.as_bytes()),
                        requirements_b64: BASE64.encode(quote.requirements.bytes()),
                        payload_b64: BASE64.encode(payload.bytes()),
                        expires_at_ns: Some(quote.expires_at_ns),
                        in_flight: true,
                        in_flight_since_ns: Some(now_ns),
                        frozen: None,
                        served: false,
                        input_hash: quote.input_hash.clone(),
                        redeemed: false,
                        redeemed_for: None,
                        chain: Vec::new(),
                        billing: None,
                        billing_published: false,
                    };
                    s.consumed.insert(payload_hash.clone(), quote_id.clone());
                    s.quotes.insert(quote_id.clone(), record);
                    // The one site that grows the map, so the one site that
                    // can observe the store crossing the warn threshold.
                    // `==` rather than `>=`: this fires once per upward
                    // crossing, not on every payment thereafter. What it
                    // catches is the population compaction cannot retire
                    // (paid-but-never-redeemed, frozen), whose only other
                    // symptom is every payment getting slower.
                    if s.quotes.len() == ENGINE_STORE_SIZE_WARN_RECORDS {
                        tracing::warn!(
                            records = s.quotes.len(),
                            "payment engine store crossed \
                             ENGINE_STORE_SIZE_WARN_RECORDS — every operation parses and \
                             rewrites the whole file, so this is a latency term on every \
                             payment. Terminal records are compacted; records that are \
                             frozen, or paid but never redeemed, are retained \
                             indefinitely by design and are the likely cause"
                        );
                    }
                    Claim::Fresh
                };
                // The stale-takeover rebind, now that `rec` is released.
                if let Some(dead) = rebind_consumed_from {
                    s.consumed.remove(&dead);
                    s.consumed.insert(payload_hash.clone(), quote_id.clone());
                }
                let dirty = matches!(claim, Claim::Fresh) || retired > 0;
                (claim, dirty)
            })
            .await?
        };

        match claim {
            Claim::Frozen(reason) => {
                return Ok(PaymentDecision::Rejected {
                    reason: RejectReason::QuoteFrozen(reason),
                })
            }
            Claim::Expired => {
                return Ok(PaymentDecision::Rejected {
                    reason: RejectReason::QuoteExpired,
                })
            }
            Claim::QuoteAlreadyPaid => {
                return Ok(PaymentDecision::Rejected {
                    reason: RejectReason::QuoteAlreadyPaid,
                })
            }
            Claim::ReplayOtherQuote => {
                return Ok(PaymentDecision::Rejected {
                    reason: RejectReason::Replay,
                })
            }
            Claim::InProgress => return Ok(PaymentDecision::InProgress),
            Claim::AlreadyServed(billing, tier, published) => {
                // Idempotent completion: same billing event id, no settle.
                // The billing event is committed to state, but its log
                // append may have been lost (append failure or a crash
                // before the published-mark). Re-publish so the charge
                // still reaches accounting; the log dedups by id, so this
                // is safe to repeat until it lands.
                if !published {
                    self.publish_billing(&quote.quote_id, Some((*billing).clone()))
                        .await?;
                }
                return Ok(PaymentDecision::Served {
                    billing,
                    tier: tier.unwrap_or(VerificationTier::Observed),
                });
            }
            Claim::AlreadySettled => {
                // Settled on a prior attempt but the tier gate wasn't met:
                // this retry is a re-verify, never a second settle.
                return self.re_verify(&quote.quote_id, required_tier, now_ns).await;
            }
            Claim::Fresh => {}
        }

        // -- verify (facilitator I/O, no lock held).
        let verify = match self.facilitator.verify(payload, &quote.requirements).await {
            Ok(v) => v,
            Err(e) => {
                self.release_claim(&quote.quote_id, &payload_hash).await?;
                return Ok(PaymentDecision::FacilitatorFailure {
                    kind: e.kind,
                    retryable: e.retryable,
                    message: e.message,
                });
            }
        };
        if !verify.response.view().is_valid {
            let reason = verify
                .response
                .view()
                .invalid_reason
                .clone()
                .unwrap_or_else(|| "unspecified".to_string());
            self.release_claim(&quote.quote_id, &payload_hash).await?;
            return Ok(PaymentDecision::Rejected {
                reason: RejectReason::VerifyRejected(reason),
            });
        }

        // -- settle (facilitator I/O, no lock held).
        let settle = match self.facilitator.settle(payload, &quote.requirements).await {
            Ok(s) => s,
            Err(e) => {
                self.release_claim(&quote.quote_id, &payload_hash).await?;
                return Ok(PaymentDecision::FacilitatorFailure {
                    kind: e.kind,
                    retryable: e.retryable,
                    message: e.message,
                });
            }
        };
        if !settle.response.view().success {
            let reason = settle
                .response
                .view()
                .error_reason
                .clone()
                .unwrap_or_else(|| "unspecified".to_string());
            self.release_claim(&quote.quote_id, &payload_hash).await?;
            return Ok(PaymentDecision::Rejected {
                reason: RejectReason::SettleFailed(reason),
            });
        }

        // -- completion: amount policy + chain event + billing, one lock.
        let required: AtomicAmount = AtomicAmount::parse(&quote.requirements.view().amount)
            .map_err(|e| EngineError::State(e.to_string()))?;
        let delivered: AtomicAmount = match &settle.response.view().amount {
            Some(a) => AtomicAmount::parse(a).map_err(|e| EngineError::State(e.to_string()))?,
            None => required.clone(),
        };
        let transaction = settle.response.view().transaction.clone();
        let settle_network = settle.response.view().network.clone();
        let quoted_network = quote.requirements.view().network.clone();
        // A facilitator answer is `observed`, full stop — the engine mints
        // the tier rather than reading one off the response, so no
        // `Facilitator` implementation can promote its own receipt.
        // Anything above `observed` comes from `re_verify_with_checker`.
        let tier = VerificationTier::Observed;
        // The facilitator's settle-time payer claim, recorded below as a
        // chain fact. For schemes whose payload carries no on-chain payer
        // (exact-SVM's opaque wallet blob), a later independent re-check
        // binds delivery to THIS recorded claim — a facilitator that later
        // substitutes some other customer's transaction must find one whose
        // on-chain payer equals the payer it named when it first settled.
        // Weaker than the caller-signed `authorization.from` bind (which
        // wins when present), but it pins post-hoc substitution.
        let settle_payer = settle.response.view().payer.clone();

        let quote_id = quote.quote_id.clone();
        type Completion = Result<(PaymentDecision, Option<BillingEvent>), EngineError>;
        let (decision, fresh_billing) =
            mutate_json::<EngineState, Completion, _>(&self.state_path, |s| {
                // Facilitator-answer sanity, before any amount reasoning:
                // [a] the settlement must be on the QUOTED network — a
                //     receipt from some other chain is worth nothing here;
                // [b] the transaction must not already satisfy another quote
                //     (receipt replay: one on-chain settlement, one serve).
                // Both are misbehavior-of-the-money-machinery: invalidate
                // and freeze, never a retryable shrug.
                if settle_network != quoted_network {
                    let rec = s
                        .quotes
                        .get_mut(&quote_id)
                        .ok_or_else(|| EngineError::State("record vanished mid-settle".into()))?;
                    rec.in_flight = false;
                    let ev = self.build_event(
                        rec,
                        &quote_id,
                        Some(transaction.clone()),
                        tier,
                        VerificationStatus::Invalidated {
                            reason: InvalidationReason::Rejected,
                        },
                        now_ns,
                        &[(
                            "network_mismatch".to_string(),
                            serde_json::Value::String(settle_network.clone()),
                        )],
                    )?;
                    rec.chain.push(ev);
                    rec.frozen = Some(format!(
                        "settlement reported on `{settle_network}`, quote is on `{quoted_network}`"
                    ));
                    return Ok((
                        PaymentDecision::Invalidated {
                            reason: InvalidationReason::Rejected,
                        },
                        None,
                    ));
                }
                let tx_key = format!("{quoted_network}|{transaction}");
                match s.consumed_transactions.get(&tx_key) {
                    Some(owner) if *owner != quote_id => {
                        let rec = s.quotes.get_mut(&quote_id).ok_or_else(|| {
                            EngineError::State("record vanished mid-settle".into())
                        })?;
                        rec.in_flight = false;
                        let ev = self.build_event(
                            rec,
                            &quote_id,
                            Some(transaction.clone()),
                            tier,
                            VerificationStatus::Invalidated {
                                reason: InvalidationReason::Replay,
                            },
                            now_ns,
                            &[(
                                "transaction_already_satisfies".to_string(),
                                serde_json::Value::String(owner.clone()),
                            )],
                        )?;
                        rec.chain.push(ev);
                        rec.frozen =
                            Some("settlement transaction replayed across quotes".to_string());
                        return Ok((
                            PaymentDecision::Invalidated {
                                reason: InvalidationReason::Replay,
                            },
                            None,
                        ));
                    }
                    _ => {
                        s.consumed_transactions.insert(tx_key, quote_id.clone());
                    }
                }

                let rec = s
                    .quotes
                    .get_mut(&quote_id)
                    .ok_or_else(|| EngineError::State("record vanished mid-settle".into()))?;
                rec.in_flight = false;

                // Every completion event carries the settle-time payer claim
                // (when the facilitator reported one) so re-checks can bind
                // delivery to it — see `settle_payer` above.
                let mut completion_extra: Vec<(String, serde_json::Value)> = Vec::new();
                if let Some(p) = &settle_payer {
                    completion_extra
                        .push(("payer".to_string(), serde_json::Value::String(p.clone())));
                }

                use std::cmp::Ordering;
                match delivered.cmp(&required) {
                    Ordering::Less => {
                        // Money moved but short: the payment is invalid and the
                        // quote freezes — value was consumed, nothing serves.
                        let mut extra = completion_extra.clone();
                        extra.push((
                            "delivered".to_string(),
                            serde_json::Value::String(delivered.to_canonical_string()),
                        ));
                        let ev = self.build_event(
                            rec,
                            &quote_id,
                            Some(transaction.clone()),
                            tier,
                            VerificationStatus::Invalidated {
                                reason: InvalidationReason::AmountMismatch,
                            },
                            now_ns,
                            &extra,
                        )?;
                        rec.chain.push(ev);
                        rec.frozen = Some("amount_mismatch".to_string());
                        Ok((
                            PaymentDecision::Invalidated {
                                reason: InvalidationReason::AmountMismatch,
                            },
                            None,
                        ))
                    }
                    Ordering::Greater => {
                        // Overpayment: verification exception for provider
                        // policy, never auto-satisfied. Not frozen; no billing.
                        let mut extra = completion_extra.clone();
                        extra.push((
                            "delivered".to_string(),
                            serde_json::Value::String(delivered.to_canonical_string()),
                        ));
                        let ev = self.build_event(
                            rec,
                            &quote_id,
                            Some(transaction.clone()),
                            tier,
                            VerificationStatus::Exception {
                                kind: ExceptionKind::Overpayment,
                            },
                            now_ns,
                            &extra,
                        )?;
                        rec.chain.push(ev);
                        Ok((
                            PaymentDecision::Exception {
                                kind: ExceptionKind::Overpayment,
                            },
                            None,
                        ))
                    }
                    Ordering::Equal => {
                        let ev = self.build_event(
                            rec,
                            &quote_id,
                            Some(transaction.clone()),
                            tier,
                            VerificationStatus::Verified,
                            now_ns,
                            &completion_extra,
                        )?;
                        rec.chain.push(ev);
                        if tier.satisfies(&required_tier) {
                            let billing = self.build_billing(
                                rec,
                                &quote_id,
                                &transaction,
                                delivered.clone(),
                                now_ns,
                            )?;
                            rec.billing = Some(billing.clone());
                            rec.served = true;
                            Ok((
                                PaymentDecision::Served {
                                    billing: Box::new(billing.clone()),
                                    tier,
                                },
                                Some(billing),
                            ))
                        } else {
                            Ok((
                                PaymentDecision::PendingTier {
                                    reached: tier,
                                    required: required_tier,
                                },
                                None,
                            ))
                        }
                    }
                }
            })
            .await??;
        self.publish_billing(&quote_id, fresh_billing).await?;
        Ok(decision)
    }

    /// Re-run facilitator verification for a settled quote — the
    /// **invalidation** path (a reorg the facilitator now reports as
    /// invalid).
    ///
    /// It cannot raise confidence. A facilitator receipt justifies
    /// `observed` and nothing more (the v2 spec gives facilitators no way
    /// to report finality), so the tier is minted at this boundary rather
    /// than read off the response, and every valid answer here is
    /// `observed`. A caller waiting on `confirmed(n)` or `final` stays
    /// pending no matter how often this runs.
    ///
    /// Confidence upgrades come from
    /// [`Self::re_verify_with_checker`], which reads the chain
    /// independently and is the only producer of a higher tier.
    pub async fn re_verify(
        &self,
        quote_id: &str,
        required_tier: VerificationTier,
        now_ns: u64,
    ) -> Result<PaymentDecision, EngineError> {
        // Snapshot the carries without holding the lock across I/O.
        let state: EngineState = load_json(&self.state_path).await?;
        let rec = match state.quotes.get(quote_id) {
            Some(rec) => rec,
            None => {
                return Ok(PaymentDecision::Rejected {
                    reason: RejectReason::BadQuote("unknown quote".into()),
                })
            }
        };
        if let Some(reason) = &rec.frozen {
            return Ok(PaymentDecision::Rejected {
                reason: RejectReason::QuoteFrozen(reason.clone()),
            });
        }
        if rec.chain.is_empty() {
            return Ok(PaymentDecision::Rejected {
                reason: RejectReason::BadQuote("quote has no settlement to re-verify".into()),
            });
        }
        let requirements: X402Carry<PaymentRequirements> = X402Carry::from_bytes(
            BASE64
                .decode(&rec.requirements_b64)
                .map_err(|e| EngineError::State(e.to_string()))?,
        )?;
        let payload: X402Carry<PaymentPayload> = X402Carry::from_bytes(
            BASE64
                .decode(&rec.payload_b64)
                .map_err(|e| EngineError::State(e.to_string()))?,
        )?;

        let verify = match self.facilitator.verify(&payload, &requirements).await {
            Ok(v) => v,
            Err(e) => {
                return Ok(PaymentDecision::FacilitatorFailure {
                    kind: e.kind,
                    retryable: e.retryable,
                    message: e.message,
                })
            }
        };
        let is_valid = verify.response.view().is_valid;
        let facilitator_reason = verify.response.view().invalid_reason.clone();
        // Same as the settle path: the facilitator's answer is `observed`
        // regardless of what it would like to claim.
        let tier = VerificationTier::Observed;
        // The amount this quote requires: re-verify must re-apply the
        // under/over/exact policy against the delivered amount recorded at
        // settlement, not trust the facilitator's `is_valid` boolean alone.
        let required_amount = AtomicAmount::parse(&requirements.view().amount)
            .map_err(|e| EngineError::State(e.to_string()))?;

        let quote_id = quote_id.to_string();
        type Completion = Result<(PaymentDecision, Option<BillingEvent>), EngineError>;
        let (decision, fresh_billing) =
            mutate_json::<EngineState, Completion, _>(&self.state_path, |s| {
                let rec = s
                    .quotes
                    .get_mut(&quote_id)
                    .ok_or_else(|| EngineError::State("record vanished mid-verify".into()))?;
                if let Some(reason) = &rec.frozen {
                    return Ok((
                        PaymentDecision::Rejected {
                            reason: RejectReason::QuoteFrozen(reason.clone()),
                        },
                        None,
                    ));
                }
                let transaction = rec.chain.last().and_then(|e| e.transaction.clone());

                if !is_valid {
                    let reason_str =
                        facilitator_reason.unwrap_or_else(|| "unspecified".to_string());
                    let reason = InvalidationReason::from_facilitator_reason(&reason_str);
                    let ev = self.build_event(
                        rec,
                        &quote_id,
                        transaction,
                        tier,
                        VerificationStatus::Invalidated { reason },
                        now_ns,
                        &[(
                            "facilitator_reason".to_string(),
                            serde_json::Value::String(reason_str.clone()),
                        )],
                    )?;
                    rec.chain.push(ev);
                    // Freeze: nothing further serves against this quote. The
                    // billing event (if emitted) stands immutable — this event
                    // references the same quote/chain for the audit trail.
                    rec.frozen = Some(reason_str);
                    return Ok((PaymentDecision::Invalidated { reason }, None));
                }

                // The delivered amount recorded at settlement time. On the
                // fresh path this is written only for over/under settlements
                // (an exact `Verified` event carries no `delivered` extra).
                let recorded_delivered = rec
                    .chain
                    .first()
                    .and_then(|e| e.extra.get("delivered"))
                    .and_then(|v| v.as_str())
                    .map(AtomicAmount::parse)
                    .transpose()
                    .map_err(|e| EngineError::State(e.to_string()))?;

                // A record already billed keeps serving idempotently; this
                // re-verify only records the tier upgrade. The amount was
                // vetted when the billing event was minted, so no re-check.
                if let Some(billing) = rec.billing.clone() {
                    let ev = self.build_event(
                        rec,
                        &quote_id,
                        transaction,
                        tier,
                        VerificationStatus::Verified,
                        now_ns,
                        &[],
                    )?;
                    rec.chain.push(ev);
                    return Ok((
                        PaymentDecision::Served {
                            billing: Box::new(billing),
                            tier,
                        },
                        None,
                    ));
                }

                // Not yet billed: re-apply the under/over/exact amount policy
                // that `accept_payment` and `re_verify_with_checker` enforce.
                // Trusting only the facilitator's `is_valid` here would let a
                // retry auto-bill an overpayment (which the design routes to
                // manual provider policy) or promote a short-pay to a serve.
                use std::cmp::Ordering;
                if let Some(delivered) = &recorded_delivered {
                    match delivered.cmp(&required_amount) {
                        Ordering::Less => {
                            let ev = self.build_event(
                                rec,
                                &quote_id,
                                transaction,
                                tier,
                                VerificationStatus::Invalidated {
                                    reason: InvalidationReason::AmountMismatch,
                                },
                                now_ns,
                                &[(
                                    "delivered".to_string(),
                                    serde_json::Value::String(delivered.to_canonical_string()),
                                )],
                            )?;
                            rec.chain.push(ev);
                            rec.frozen = Some("amount_mismatch".to_string());
                            return Ok((
                                PaymentDecision::Invalidated {
                                    reason: InvalidationReason::AmountMismatch,
                                },
                                None,
                            ));
                        }
                        Ordering::Greater => {
                            let ev = self.build_event(
                                rec,
                                &quote_id,
                                transaction,
                                tier,
                                VerificationStatus::Exception {
                                    kind: ExceptionKind::Overpayment,
                                },
                                now_ns,
                                &[(
                                    "delivered".to_string(),
                                    serde_json::Value::String(delivered.to_canonical_string()),
                                )],
                            )?;
                            rec.chain.push(ev);
                            return Ok((
                                PaymentDecision::Exception {
                                    kind: ExceptionKind::Overpayment,
                                },
                                None,
                            ));
                        }
                        Ordering::Equal => {}
                    }
                }

                let ev = self.build_event(
                    rec,
                    &quote_id,
                    transaction.clone(),
                    tier,
                    VerificationStatus::Verified,
                    now_ns,
                    &[],
                )?;
                rec.chain.push(ev);

                if tier.satisfies(&required_tier) {
                    let tx = transaction.unwrap_or_default();
                    let amount = match recorded_delivered {
                        Some(a) => a,
                        None => self.required_amount_from(rec)?,
                    };
                    let billing = self.build_billing(rec, &quote_id, &tx, amount, now_ns)?;
                    rec.billing = Some(billing.clone());
                    rec.served = true;
                    Ok((
                        PaymentDecision::Served {
                            billing: Box::new(billing.clone()),
                            tier,
                        },
                        Some(billing),
                    ))
                } else {
                    Ok((
                        PaymentDecision::PendingTier {
                            reached: tier,
                            required: required_tier,
                        },
                        None,
                    ))
                }
            })
            .await??;
        self.publish_billing(&quote_id, fresh_billing).await?;
        Ok(decision)
    }

    /// Append a freshly-emitted billing event to the attached log, then
    /// mark the record `billing_published` so retries don't re-append. No
    /// log attached = the stream surface is simply off (state still holds
    /// the signed event, and the mark stays unset so a later-attached log
    /// can still receive it on a retry); an attached-but-failing log is a
    /// loud error, and the unset mark makes the next retry try again.
    async fn publish_billing(
        &self,
        quote_id: &str,
        fresh: Option<BillingEvent>,
    ) -> Result<(), EngineError> {
        let Some(event) = fresh else { return Ok(()) };
        let Some(log) = &self.billing_log else {
            return Ok(());
        };
        log.append(&event).await?;
        let quote_id = quote_id.to_string();
        mutate_json::<EngineState, (), _>(&self.state_path, move |s| {
            if let Some(rec) = s.quotes.get_mut(&quote_id) {
                rec.billing_published = true;
            }
        })
        .await?;
        Ok(())
    }

    /// Re-verify through the **independent chain checker** — the only
    /// path to `confirmed(n)`/`final` (a facilitator receipt caps at
    /// `observed`; the facilitator is never in the trust root above
    /// that). The checker's verdicts land as first-class chain events:
    /// inclusion upgrades the tier (and bills once the required tier is
    /// reached), a reverted settlement invalidates and freezes, and a
    /// delivered-amount mismatch — checked straight from the chain —
    /// invalidates likewise. `Pending` claims nothing either way.
    pub async fn re_verify_with_checker(
        &self,
        quote_id: &str,
        checker: &dyn ChainChecker,
        required_tier: VerificationTier,
        now_ns: u64,
    ) -> Result<PaymentDecision, EngineError> {
        // Snapshot without holding the lock across checker I/O.
        let state: EngineState = load_json(&self.state_path).await?;
        let Some(rec) = state.quotes.get(quote_id) else {
            return Ok(PaymentDecision::Rejected {
                reason: RejectReason::BadQuote("unknown quote".into()),
            });
        };
        if let Some(reason) = &rec.frozen {
            return Ok(PaymentDecision::Rejected {
                reason: RejectReason::QuoteFrozen(reason.clone()),
            });
        }
        let Some(transaction) = rec.chain.last().and_then(|e| e.transaction.clone()) else {
            return Ok(PaymentDecision::Rejected {
                reason: RejectReason::BadQuote("quote has no settlement to check".into()),
            });
        };
        let requirements: X402Carry<PaymentRequirements> = X402Carry::from_bytes(
            BASE64
                .decode(&rec.requirements_b64)
                .map_err(|e| EngineError::State(e.to_string()))?,
        )?;
        let network = requirements.view().network.clone();
        let required_amount = AtomicAmount::parse(&requirements.view().amount)
            .map_err(|e| EngineError::State(e.to_string()))?;
        // The authorized payer, so the checker binds delivery to *this*
        // quote's authorization and not merely to (token, recipient). For
        // exact-EVM this is `payload.authorization.from` — caller-signed,
        // the strongest bind. For schemes whose payload is an opaque
        // wallet blob (exact-SVM), fall back to the settle-time payer the
        // facilitator named, recorded as a chain fact on the first
        // settlement event: weaker (the facilitator's own claim), but it
        // pins post-hoc transaction substitution to the originally-named
        // payer. Neither present leaves the bind `None`.
        let payload: X402Carry<PaymentPayload> = X402Carry::from_bytes(
            BASE64
                .decode(&rec.payload_b64)
                .map_err(|e| EngineError::State(e.to_string()))?,
        )?;
        let payer_from = payload
            .view()
            .payload
            .get("authorization")
            .and_then(|a| a.get("from"))
            .and_then(|v| v.as_str())
            .map(str::to_owned)
            .or_else(|| {
                rec.chain
                    .first()
                    .and_then(|e| e.extra.get("payer"))
                    .and_then(|v| v.as_str())
                    .map(str::to_owned)
            });
        // Scheme-generic opaque-extra reads: a scheme that binds its
        // settlement to a per-quote reference (exact-XRPL's `invoiceId`,
        // carried on-ledger as MemoData/InvoiceID) or a recipient
        // sub-account tag (XRPL `DestinationTag`) carries them in
        // `requirements.extra`. The engine reads the keys generically and
        // never interprets their *meaning* — the checker adapter does;
        // schemes without them thread `None` (unchanged behavior).
        let req_extra = requirements.view().extra.clone();
        // Reference precedence — network-family-scoped, because the caller
        // authors the payload but only the provider authors the
        // requirements:
        //
        // - On eip155 networks the reference is the caller-signed EIP-3009
        //   `authorization.nonce` (the signature covers it — same trust
        //   class as `authorization.from`); the eip155 checker binds it to
        //   the token's `AuthorizationUsed` event. The bind is MANDATORY
        //   here: a missing or malformed nonce is refused (fail-closed),
        //   never silently downgraded to the weaker (token, from, to)
        //   check by threading `None`/`invoiceId` — the checker's
        //   `is_nonce_hex` filter would treat a non-nonce reference as "no
        //   nonce" and skip the bind, re-opening the H3 residual. Same
        //   fail-closed posture as the SVM unbound-payer guard. (Every
        //   eip155 `exact` settlement is EIP-3009, so a legitimate payload
        //   always carries a nonce; a future non-3009 eip155 scheme would
        //   surface loudly here rather than fail open.)
        // - Elsewhere the reference is the provider-authored
        //   `requirements.extra.invoiceId` (exact-XRPL's vocabulary). The
        //   invoiceId is deliberately NOT an eip155 fallback: off-EVM
        //   payloads sign only their wallet blob, and reading a
        //   caller-supplied `authorization.nonce` off-EVM would let a
        //   caller override the provider's invoice bind with an unsigned
        //   field. Schemes with neither thread `None`.
        let reference = if network.starts_with("eip155:") {
            match payload
                .view()
                .payload
                .get("authorization")
                .and_then(|a| a.get("nonce"))
                .and_then(|v| v.as_str())
            {
                Some(nonce) if crate::checker::is_eip3009_nonce(nonce) => Some(nonce.to_owned()),
                _ => {
                    return Ok(PaymentDecision::Rejected {
                        reason: RejectReason::BadQuote(
                            "eip155 settlement carries no valid EIP-3009 authorization.nonce — \
                             refusing to verify delivery without the authorization bind"
                                .into(),
                        ),
                    })
                }
            }
        } else {
            req_extra
                .as_ref()
                .and_then(|e| e.get("invoiceId"))
                .and_then(|v| v.as_str())
                .map(str::to_owned)
        };
        // The tag's *type* is validated here (M3): a present-but-malformed
        // `destinationTag` is a hard refusal — matching the authoring
        // seam's `exact_xrpl::optional_tag` — never a silent drop to
        // `None`. Silently dropping it would ask the checker to verify
        // against "no tag" (which now requires tag *absence*), quietly
        // discarding a sub-account routing the quote meant to bind.
        let to_tag =
            match req_extra.as_ref().and_then(|e| e.get("destinationTag")) {
                None | Some(serde_json::Value::Null) => None,
                Some(v) => Some(v.as_u64().and_then(|n| u32::try_from(n).ok()).ok_or_else(
                    || {
                        EngineError::State(
                            "requirements.extra.destinationTag is not a u32 sub-account tag".into(),
                        )
                    },
                )?),
            };
        let query = TransferQuery {
            token: requirements.view().asset.clone(),
            to: requirements.view().pay_to.clone(),
            from: payer_from,
            reference,
            to_tag,
        };

        let verdict = match checker.check(&network, &transaction, Some(&query)).await {
            Ok(v) => v,
            Err(e) => {
                return Ok(PaymentDecision::FacilitatorFailure {
                    kind: if e.retryable {
                        FacilitatorErrorKind::Unavailable
                    } else {
                        FacilitatorErrorKind::Protocol
                    },
                    retryable: e.retryable,
                    message: format!("chain checker: {}", e.message),
                })
            }
        };
        let verifier = checker.reference();

        let quote_id = quote_id.to_string();
        type Completion = Result<(PaymentDecision, Option<BillingEvent>), EngineError>;
        let (decision, fresh_billing) =
            mutate_json::<EngineState, Completion, _>(&self.state_path, |s| {
                let rec = s
                    .quotes
                    .get_mut(&quote_id)
                    .ok_or_else(|| EngineError::State("record vanished mid-check".into()))?;
                if let Some(reason) = &rec.frozen {
                    return Ok((
                        PaymentDecision::Rejected {
                            reason: RejectReason::QuoteFrozen(reason.clone()),
                        },
                        None,
                    ));
                }

                match verdict {
                    ChainVerdict::Pending => {
                        // No event: pending is the absence of an answer,
                        // and the chain stays an append-only record of
                        // *facts*.
                        let reached =
                            best_verified_tier(&rec.chain).unwrap_or(VerificationTier::Observed);
                        Ok((
                            PaymentDecision::PendingTier {
                                reached,
                                required: required_tier,
                            },
                            None,
                        ))
                    }
                    ChainVerdict::Reverted => {
                        let ev = self.build_event_with_verifier(
                            rec,
                            &quote_id,
                            Some(transaction.clone()),
                            VerificationTier::Observed,
                            VerificationStatus::Invalidated {
                                reason: InvalidationReason::Rejected,
                            },
                            verifier.clone(),
                            now_ns,
                            &[(
                                "chain_status".to_string(),
                                serde_json::Value::String("reverted".to_string()),
                            )],
                        )?;
                        rec.chain.push(ev);
                        rec.frozen = Some("settlement reverted on-chain".to_string());
                        Ok((
                            PaymentDecision::Invalidated {
                                reason: InvalidationReason::Rejected,
                            },
                            None,
                        ))
                    }
                    ChainVerdict::Included {
                        tier,
                        ref delivered,
                    } => {
                        // Delivered-amount cross-check, straight from the
                        // chain: the exact-amount policy's independent leg.
                        if let Some(delivered) = delivered {
                            let delivered = AtomicAmount::parse(delivered)
                                .map_err(|e| EngineError::State(e.to_string()))?;
                            use std::cmp::Ordering;
                            match delivered.cmp(&required_amount) {
                                Ordering::Less => {
                                    let ev = self.build_event_with_verifier(
                                        rec,
                                        &quote_id,
                                        Some(transaction.clone()),
                                        tier,
                                        VerificationStatus::Invalidated {
                                            reason: InvalidationReason::AmountMismatch,
                                        },
                                        verifier.clone(),
                                        now_ns,
                                        &[(
                                            "delivered".to_string(),
                                            serde_json::Value::String(
                                                delivered.to_canonical_string(),
                                            ),
                                        )],
                                    )?;
                                    rec.chain.push(ev);
                                    rec.frozen = Some("amount_mismatch".to_string());
                                    return Ok((
                                        PaymentDecision::Invalidated {
                                            reason: InvalidationReason::AmountMismatch,
                                        },
                                        None,
                                    ));
                                }
                                Ordering::Greater => {
                                    let ev = self.build_event_with_verifier(
                                        rec,
                                        &quote_id,
                                        Some(transaction.clone()),
                                        tier,
                                        VerificationStatus::Exception {
                                            kind: ExceptionKind::Overpayment,
                                        },
                                        verifier.clone(),
                                        now_ns,
                                        &[(
                                            "delivered".to_string(),
                                            serde_json::Value::String(
                                                delivered.to_canonical_string(),
                                            ),
                                        )],
                                    )?;
                                    rec.chain.push(ev);
                                    return Ok((
                                        PaymentDecision::Exception {
                                            kind: ExceptionKind::Overpayment,
                                        },
                                        None,
                                    ));
                                }
                                Ordering::Equal => {}
                            }
                        }

                        let ev = self.build_event_with_verifier(
                            rec,
                            &quote_id,
                            Some(transaction.clone()),
                            tier,
                            VerificationStatus::Verified,
                            verifier.clone(),
                            now_ns,
                            &[],
                        )?;
                        rec.chain.push(ev);

                        if let Some(billing) = &rec.billing {
                            return Ok((
                                PaymentDecision::Served {
                                    billing: Box::new(billing.clone()),
                                    tier,
                                },
                                None,
                            ));
                        }
                        if tier.satisfies(&required_tier) {
                            let billing = self.build_billing(
                                rec,
                                &quote_id,
                                &transaction,
                                required_amount.clone(),
                                now_ns,
                            )?;
                            rec.billing = Some(billing.clone());
                            rec.served = true;
                            Ok((
                                PaymentDecision::Served {
                                    billing: Box::new(billing.clone()),
                                    tier,
                                },
                                Some(billing),
                            ))
                        } else {
                            Ok((
                                PaymentDecision::PendingTier {
                                    reached: tier,
                                    required: required_tier,
                                },
                                None,
                            ))
                        }
                    }
                }
            })
            .await??;
        self.publish_billing(&quote_id, fresh_billing).await?;
        Ok(decision)
    }

    /// The provider-side invocation gate: redeem a paid quote for its one
    /// invocation. Admits iff the quote is settled and billed, unfrozen,
    /// bound to `tool_id` (the capability's tool segment), and never
    /// redeemed before — one payment, one serve, atomically under the
    /// store lock. Deliberately at-most-once: a paid invoke whose reply
    /// was lost is not re-servable on the same quote, matching the
    /// at-most-once retry safety of credentialed tools.
    ///
    /// `binding` must be the paying identity's ed25519 signature over
    /// [`invocation_binding_transcript`] — possession proof that the
    /// invoker is the payer. Present-but-invalid rejects.
    ///
    /// **Absent rejects too**, with
    /// [`RedeemDenialReason::BindingRequired`]. A default engine requires
    /// the binding: without it the quote id alone admits, and a quote id
    /// travels through logs, proxies, and the caller's own tooling — a
    /// bearer credential by accident rather than by decision.
    ///
    /// Bearer semantics survive only behind
    /// [`Self::with_require_invocation_binding(false)`](Self::with_require_invocation_binding),
    /// for pre-binding callers that cannot be upgraded yet. The quote id
    /// is content-derived and unguessable, so that mode is not *broken* —
    /// it is just a weaker claim than possession of the payer's key.
    pub async fn redeem_for_invocation(
        &self,
        tool_id: &str,
        quote_id: &str,
        binding: Option<&[u8]>,
    ) -> Result<RedeemDecision, EngineError> {
        if self.require_invocation_binding && binding.is_none() {
            // Refused before the store is touched: nothing to look up, and
            // a missing binding is a client-configuration fact that does
            // not depend on whether the quote exists.
            return Ok(RedeemDecision::Denied {
                reason: RedeemDenialReason::BindingRequired,
            });
        }
        let binding = binding.map(<[u8]>::to_vec);
        let tool_id = tool_id.to_string();
        let quote_id = quote_id.to_string();
        // Only the `Admitted` arm mutates state (`rec.redeemed = true`);
        // every `Denied{..}` arm is read-only. `mutate_json_if_changed` skips
        // the durable write (serialize + fsync + rename) on the read-only
        // arms — closing the write-amplification / DoS surface where a caller
        // could force one fsync per denied attempt — while keeping the
        // check-and-set atomic under the same lock (at-most-once is
        // unchanged). The `bool` in each returned tuple is `dirty`: it is
        // `true` on exactly the one arm that mutated. See
        // `docs/internal/performance/payments-redeem-write-amplification.md`.
        let decision = mutate_json_if_changed::<EngineState, _, _>(&self.state_path, move |s| {
            let Some(rec) = s.quotes.get_mut(&quote_id) else {
                // Retention may have compacted a paid, redeemed record.
                // The quote is not unknown then — it was paid and already
                // consumed, and saying `unknown_quote` would tell this
                // caller that no funds moved and a new quote is safe,
                // about a settlement that happened. This gate is strictly
                // at-most-once, so the answer is the same one a live
                // redeemed record gives: `already_redeemed`.
                let Some(tomb) = s.redemptions.get(&quote_id) else {
                    return (
                        RedeemDecision::Denied {
                            reason: RedeemDenialReason::UnknownQuote,
                        },
                        false,
                    );
                };
                let reason = match tomb.preconditions(&quote_id, &tool_id, binding.as_deref()) {
                    Ok(_) => RedeemDenialReason::AlreadyRedeemed,
                    Err(reason) => reason,
                };
                return (RedeemDecision::Denied { reason }, false);
            };
            let payer =
                match Self::redeem_preconditions(rec, &quote_id, &tool_id, binding.as_deref()) {
                    Ok(payer) => payer,
                    Err(reason) => return (RedeemDecision::Denied { reason }, false),
                };
            // Strictly at-most-once. Unlike [`Self::redeem_for_task`] there
            // is no purchase identity here to be idempotent *on*: a second
            // redemption of the same quote for the same tool is a second
            // serve, and the caller's at-most-once retry contract is the
            // whole reason this gate exists.
            if rec.redeemed {
                return (
                    RedeemDecision::Denied {
                        reason: RedeemDenialReason::AlreadyRedeemed,
                    },
                    false,
                );
            }
            rec.redeemed = true;
            (RedeemDecision::Admitted { payer }, true)
        })
        .await?;
        Ok(decision)
    }

    /// The task-admission gate: redeem a paid quote for one **purchase**
    /// of one unit of work, named by `expected_input_hash`.
    ///
    /// Shares every precondition with
    /// [`Self::redeem_for_invocation`] — see
    /// `Self::redeem_preconditions`, the one place they are written —
    /// and differs in exactly three ways, each of which is a consequence
    /// of admitting a *purchase* rather than an *invocation*:
    ///
    /// 1. **The binding is mandatory.** `binding` is `&[u8]`, not
    ///    `Option`, and `with_require_invocation_binding(false)` does not
    ///    reach this path. A bearer task admission would let anyone who
    ///    saw a quote id claim someone else's purchased work; the tool
    ///    path keeps bearer mode for pre-binding callers, this one never
    ///    had any.
    /// 2. **The quote must commit to this purchase.** After the tool
    ///    binding check, `rec.input_hash` must equal
    ///    `expected_input_hash` or the answer is
    ///    [`RedeemDenialReason::InputBindingMismatch`] — *before* the
    ///    redeemed arm, and with no durable write. Because the expected
    ///    hash names one provider-minted reservation of one owner's one
    ///    task, a proof replayed under another owner or another
    ///    reservation computes a different hash and dies here rather than
    ///    consuming the quote.
    /// 3. **Admission is idempotent per purchase hash.** A record already
    ///    redeemed *for this same hash* re-admits with no write; a record
    ///    redeemed for anything else (including through
    ///    [`Self::redeem_for_invocation`], which records no hash) is
    ///    [`RedeemDenialReason::AlreadyRedeemed`].
    ///
    /// That third point is safe because at-most-once **execution** is not
    /// owned here. The hash is idempotent on one reservation of one
    /// owner's one task, and the provider's own launch claim and ledger
    /// decide whether that task runs. Idempotent redemption exists only
    /// so a provider that crashed between this write and its journal
    /// write reconciles on retry instead of failing `already_redeemed` or
    /// charging a second time.
    ///
    /// **That window outlives the record.** Marking the quote redeemed is
    /// what makes it eligible for ordinary retention, and the journal
    /// write is a separate durable step — so the crash this idempotency
    /// exists for is precisely the crash after which compaction can
    /// remove the record. Every check above therefore also runs against
    /// the retained redemption tombstone, in the same order, so a
    /// compacted settled purchase re-admits for the admission that
    /// bought it and is never answered `unknown_quote` — which would
    /// claim no funds moved and recommend buying again.
    pub async fn redeem_for_task(
        &self,
        tool_id: &str,
        quote_id: &str,
        binding: &[u8],
        expected_input_hash: &str,
    ) -> Result<RedeemDecision, EngineError> {
        let binding = binding.to_vec();
        let tool_id = tool_id.to_string();
        let quote_id = quote_id.to_string();
        let expected = expected_input_hash.to_string();
        // Same write discipline as the invocation gate: the only dirty arm
        // is the one that first consumes the quote. The mismatch arm and
        // the idempotent re-admission arm are both read-only, so a caller
        // presenting wrong hashes cannot force an fsync per attempt.
        let decision = mutate_json_if_changed::<EngineState, _, _>(&self.state_path, move |s| {
            let Some(rec) = s.quotes.get_mut(&quote_id) else {
                // Retention may have compacted the record while the
                // provider's own admission write was still outstanding —
                // `redeem_for_task` marking the quote consumed is what
                // makes it prunable, and the journal `Paid` write is a
                // separate durable step in a separate file. The
                // redemption authority is retained for exactly this, and
                // resolves in the same order the live record does.
                let Some(tomb) = s.redemptions.get(&quote_id) else {
                    return (
                        RedeemDecision::Denied {
                            reason: RedeemDenialReason::UnknownQuote,
                        },
                        false,
                    );
                };
                let payer = match tomb.preconditions(&quote_id, &tool_id, Some(&binding)) {
                    Ok(payer) => payer,
                    Err(reason) => return (RedeemDecision::Denied { reason }, false),
                };
                if tomb.input_hash.as_deref() != Some(expected.as_str()) {
                    return (
                        RedeemDecision::Denied {
                            reason: RedeemDenialReason::InputBindingMismatch,
                        },
                        false,
                    );
                }
                if tomb.redeemed_for.as_deref() == Some(expected.as_str()) {
                    // The admission this payment bought, presented again.
                    // Re-admitted with no write, exactly as the live
                    // record would have — a compacted purchase is still a
                    // purchase.
                    return (RedeemDecision::Admitted { payer }, false);
                }
                return (
                    RedeemDecision::Denied {
                        reason: RedeemDenialReason::AlreadyRedeemed,
                    },
                    false,
                );
            };
            let payer = match Self::redeem_preconditions(rec, &quote_id, &tool_id, Some(&binding)) {
                Ok(payer) => payer,
                Err(reason) => return (RedeemDecision::Denied { reason }, false),
            };
            // Before the redeemed arm, deliberately: a proof for another
            // purchase must never be able to consume this one, and must
            // never learn from the answer whether this quote had been
            // redeemed.
            if rec.input_hash.as_deref() != Some(expected.as_str()) {
                return (
                    RedeemDecision::Denied {
                        reason: RedeemDenialReason::InputBindingMismatch,
                    },
                    false,
                );
            }
            if rec.redeemed {
                if rec.redeemed_for.as_deref() == Some(expected.as_str()) {
                    // The same purchase, admitted again: the provider is
                    // reconciling a crash, not buying a second time. No
                    // write — the record already says exactly this.
                    return (RedeemDecision::Admitted { payer }, false);
                }
                return (
                    RedeemDecision::Denied {
                        reason: RedeemDenialReason::AlreadyRedeemed,
                    },
                    false,
                );
            }
            rec.redeemed = true;
            rec.redeemed_for = Some(expected.clone());
            (RedeemDecision::Admitted { payer }, true)
        })
        .await?;
        Ok(decision)
    }

    /// Everything both redemption gates check, in the one order both use:
    /// binding shape → payer identity → binding signature → frozen →
    /// settled and billed → tool binding. Returns the payer on success so
    /// the caller can hand it to [`RedeemDecision::Admitted`].
    ///
    /// Pure and read-only: it takes `&QuoteRecord` precisely so no arm of
    /// it can leave a dirty store behind. What each gate does with the
    /// `redeemed` state afterwards is the only place they differ.
    fn redeem_preconditions(
        rec: &QuoteRecord,
        quote_id: &str,
        tool_id: &str,
        binding: Option<&[u8]>,
    ) -> Result<EntityId, RedeemDenialReason> {
        // Shape before identity: a binding that is not a signature at all
        // is a client bug, and saying so does not depend on the record.
        let sig_bytes = match binding {
            Some(sig) => {
                Some(<&[u8; 64]>::try_from(sig).map_err(|_| RedeemDenialReason::BindingMalformed)?)
            }
            None => None,
        };
        // Decoded unconditionally, because an admission has to name its
        // payer whether or not a binding was presented. On the bearer path
        // this is the only new way to be denied, and it fires solely on a
        // record whose `caller_hex` is not 32 hex-encoded bytes — which
        // the engine never writes.
        let payer = hex::decode(&rec.caller_hex)
            .ok()
            .and_then(|b| <[u8; 32]>::try_from(b).ok())
            .map(EntityId::from_bytes)
            .ok_or(RedeemDenialReason::PayerRecordCorrupt)?;
        if let Some(sig_bytes) = sig_bytes {
            let transcript = invocation_binding_transcript(quote_id, tool_id);
            if payer.verify_bytes(&transcript, sig_bytes).is_err() {
                return Err(RedeemDenialReason::BindingRejected);
            }
        }
        if let Some(reason) = &rec.frozen {
            return Err(RedeemDenialReason::QuoteFrozen {
                freeze_reason: reason.clone(),
            });
        }
        if rec.billing.is_none() {
            // "Never paid" and "paid, awaiting confidence" route
            // differently: an empty event chain means no settlement
            // was ever recorded; a non-empty one means the payment
            // exists but hasn't completed to billing (pending tier /
            // re-verify, or held as an exception).
            return Err(if rec.chain.is_empty() {
                RedeemDenialReason::NotSettled
            } else {
                RedeemDenialReason::SettlementPending
            });
        }
        // The capability binds `provider/tool`; the tool segment is
        // everything after the first `/` (tool ids may themselves
        // contain `/`).
        let bound_tool = rec
            .capability
            .split_once('/')
            .map(|(_, tool)| tool)
            .unwrap_or(rec.capability.as_str());
        if bound_tool != tool_id {
            return Err(RedeemDenialReason::WrongToolBinding {
                capability: rec.capability.clone(),
                tool_id: tool_id.to_string(),
            });
        }
        Ok(payer)
    }

    /// Run the retention sweep on demand, returning how many terminal
    /// quote records were retired.
    ///
    /// [`accept_payment`](Self::accept_payment) already sweeps as it
    /// claims, so a provider under steady load never needs this. It exists
    /// for the provider that stops accepting but keeps redeeming (or stops
    /// entirely) and would otherwise keep a full store forever, and for
    /// operators who want the sweep at a known time rather than at the
    /// next payment.
    ///
    /// Only terminal records past the horizon are affected, and settlement
    /// tombstones are never removed — see `prune_terminal`.
    pub async fn prune_terminal_records(&self, now_ns: u64) -> Result<usize, EngineError> {
        let retention_ns = self.terminal_record_retention_ns;
        let tolerance_ns = self.expiry_tolerance_ns;
        let removed = mutate_json_if_changed::<EngineState, _, _>(&self.state_path, move |s| {
            let retired = prune_terminal(s, now_ns, retention_ns, tolerance_ns);
            (retired, retired > 0)
        })
        .await?;
        Ok(removed)
    }

    /// The provider identity this engine signs with — the destination a
    /// `net.payment.quote_request@1` must be addressed to.
    pub fn provider_id(&self) -> &EntityId {
        self.provider.entity_id()
    }

    /// Check that this engine's settlement backend will actually settle
    /// every `(scheme, network)` in `requirements`, before they are
    /// announced.
    ///
    /// The registry check answers a different question — is this an
    /// asset the provider knows — and a provider that passes it can
    /// still publish a route its facilitator has never handled. The
    /// caller then picks that entry, signs an authorization, and the
    /// discovery happens at settle time with their signature already
    /// given away.
    ///
    /// A backend that cannot say what it supports (the mock, or any
    /// other [`Facilitator`] that does not answer) passes. Refusing on
    /// silence would turn every implementation without a discovery
    /// surface into a failure, which is a worse trade than the gap.
    ///
    /// Network I/O: call at publication or configuration time, never per
    /// payment.
    pub async fn check_settlement_routes(
        &self,
        requirements: &[X402Carry<PaymentRequirements>],
    ) -> Result<(), EngineError> {
        let Some(pairs) = self
            .facilitator
            .supported_pairs()
            .await
            .map_err(|e| EngineError::State(format!("facilitator /supported: {e}")))?
        else {
            return Ok(());
        };
        for requirement in requirements {
            let view = requirement.view();
            let offered = pairs
                .iter()
                .any(|(scheme, network)| *scheme == view.scheme && *network == view.network);
            if !offered {
                let offers = pairs
                    .iter()
                    .map(|(s, n)| format!("({s}, {n})"))
                    .collect::<Vec<_>>()
                    .join(", ");
                return Err(EngineError::State(format!(
                    "the settlement backend does not settle ({}, {}) — refusing to announce a \
                     price it cannot honour. It offers: [{offers}]",
                    view.scheme, view.network
                )));
            }
        }
        Ok(())
    }

    /// Read-only lifecycle snapshot for gates and tests.
    pub async fn status(&self, quote_id: &str) -> Result<Option<QuoteStatus>, EngineError> {
        let state: EngineState = load_json(&self.state_path).await?;
        Ok(state.quotes.get(quote_id).map(|rec| QuoteStatus {
            frozen: rec.frozen.clone(),
            served: rec.served,
            tier: best_verified_tier(&rec.chain),
            billing_event_id: rec.billing.as_ref().map(|b| b.billing_event_id.clone()),
            chain: rec.chain.clone(),
        }))
    }

    // -- internals -------------------------------------------------------

    fn check_quote(&self, quote: &PaymentQuote) -> Result<(), RejectReason> {
        quote
            .check_integrity()
            .map_err(|e| RejectReason::BadQuote(e.to_string()))?;
        quote
            .verify_signature()
            .map_err(|e| RejectReason::BadQuote(e.to_string()))?;
        if quote.provider != *self.provider.entity_id() {
            return Err(RejectReason::BadQuote(
                "quote issued by another provider".into(),
            ));
        }
        if quote.asset_registry != self.registry_ref {
            return Err(RejectReason::BadQuote(
                "quote pinned to a different registry revision".into(),
            ));
        }
        self.registry
            .check_requirements(quote.requirements.view())
            .map_err(|e| RejectReason::BadQuote(e.to_string()))?;
        Ok(())
    }

    async fn release_claim(&self, quote_id: &str, payload_hash: &str) -> Result<(), EngineError> {
        let quote_id = quote_id.to_string();
        let payload_hash = payload_hash.to_string();
        mutate_json::<EngineState, _, _>(&self.state_path, move |s| {
            // Only release an unsettled claim — once a chain exists, value
            // moved and the record is permanent.
            let unsettled = s
                .quotes
                .get(&quote_id)
                .is_some_and(|r| r.chain.is_empty() && r.billing.is_none());
            if unsettled {
                s.quotes.remove(&quote_id);
                s.consumed.remove(&payload_hash);
            }
        })
        .await?;
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    fn build_event(
        &self,
        rec: &QuoteRecord,
        quote_id: &str,
        transaction: Option<String>,
        tier: VerificationTier,
        status: VerificationStatus,
        now_ns: u64,
        extra: &[(String, serde_json::Value)],
    ) -> Result<VerificationEvent, EngineError> {
        self.build_event_with_verifier(
            rec,
            quote_id,
            transaction,
            tier,
            status,
            self.facilitator.reference(),
            now_ns,
            extra,
        )
    }

    /// Build + sign one chain event, recording *who* verified — the
    /// facilitator for receipt-driven events, the independent chain
    /// checker for everything above `observed`.
    #[allow(clippy::too_many_arguments)]
    fn build_event_with_verifier(
        &self,
        rec: &QuoteRecord,
        quote_id: &str,
        transaction: Option<String>,
        tier: VerificationTier,
        status: VerificationStatus,
        verifier: crate::core::verification::VerifierRef,
        now_ns: u64,
        extra: &[(String, serde_json::Value)],
    ) -> Result<VerificationEvent, EngineError> {
        let prev = match rec.chain.last() {
            Some(last) => Some(last.chain_hash()?),
            None => None,
        };
        let mut event = VerificationEvent {
            object: TAG_PAYMENT_VERIFICATION.to_string(),
            quote_id: quote_id.to_string(),
            transaction,
            tier,
            status,
            verifier,
            prev,
            checked_at_ns: now_ns,
            signer: self.provider.entity_id().clone(),
            signature: None,
            extra: extra.iter().cloned().collect::<ExtraFields>(),
        };
        event.sign_with(&self.provider)?;
        Ok(event)
    }

    fn build_billing(
        &self,
        rec: &QuoteRecord,
        quote_id: &str,
        transaction: &str,
        amount: AtomicAmount,
        now_ns: u64,
    ) -> Result<BillingEvent, EngineError> {
        let requirements: X402Carry<PaymentRequirements> = X402Carry::from_bytes(
            BASE64
                .decode(&rec.requirements_b64)
                .map_err(|e| EngineError::State(e.to_string()))?,
        )?;
        let payer_bytes: [u8; 32] = hex::decode(&rec.caller_hex)
            .ok()
            .and_then(|b| b.try_into().ok())
            .ok_or_else(|| EngineError::State("caller identity corrupt in record".into()))?;
        let verification_ref = match rec.chain.last() {
            Some(last) => Some(last.chain_hash()?),
            None => None,
        };
        let mut billing = BillingEvent {
            object: TAG_BILLING_EVENT.to_string(),
            billing_event_id: BillingEvent::derive_id(&rec.idempotency_key),
            idempotency_key: rec.idempotency_key.clone(),
            capability: rec.capability.clone(),
            invocation_id: None,
            quote_id: quote_id.to_string(),
            transaction: Some(transaction.to_string()),
            verification_ref,
            payer: EntityId::from_bytes(payer_bytes),
            payee: self.provider.entity_id().clone(),
            network: requirements.view().network.clone(),
            asset: requirements.view().asset.clone(),
            amount,
            occurred_at_ns: now_ns,
            signature: None,
            extra: ExtraFields::new(),
        };
        billing.sign_with(&self.provider)?;
        Ok(billing)
    }

    fn required_amount_from(&self, rec: &QuoteRecord) -> Result<AtomicAmount, EngineError> {
        let requirements: X402Carry<PaymentRequirements> = X402Carry::from_bytes(
            BASE64
                .decode(&rec.requirements_b64)
                .map_err(|e| EngineError::State(e.to_string()))?,
        )?;
        AtomicAmount::parse(&requirements.view().amount)
            .map_err(|e| EngineError::State(e.to_string()))
    }
}

#[cfg(test)]
mod store_compat_tests {
    use super::*;

    /// A `payment-engine.json` exactly as a build **before**
    /// `input_hash` / `redeemed_for` existed wrote it: one settled,
    /// billed, redeemed quote with its payload-replay and
    /// settlement-replay entries. Captured from that build's own output,
    /// not hand-approximated — the point of the fixture is that it is the
    /// bytes an operator already has on disk.
    const PRE_CHANGE_STORE: &str = r#"{
  "consumed": {
    "cfec1dd25fde9d1d90faada90f75dc4ca893feb0fa6fcee64249128d13367270": "487dbe1038dfbc5aafda31d8e5ae4b822e12a59804eb78f33175787ed5e36c8c"
  },
  "consumed_transactions": {
    "mock:net|mock:4bc88bcf1d22a08a0bec88fa939408c7": "487dbe1038dfbc5aafda31d8e5ae4b822e12a59804eb78f33175787ed5e36c8c"
  },
  "quotes": {
    "487dbe1038dfbc5aafda31d8e5ae4b822e12a59804eb78f33175787ed5e36c8c": {
      "billing": {
        "amount": "2500",
        "asset": "musd",
        "billing_event_id": "c5487284229b4215e25139cf953278a55a2cddedec6a4e8ab0b457f550e89300",
        "capability": "fixture-provider/fixture-tool",
        "idempotency_key": "459eaef6ff5115ce69983d756960090f23a81fb36ebaac9a478653f2eb121837",
        "network": "mock:net",
        "object": "net.billing.event@1",
        "occurred_at_ns": 1000000000000001,
        "payee": "0e75bc30c35e647831ba21555a646bee038177270885f12850947804429bbaf9",
        "payer": "aa38250e253a301d9ed1905ed33a743267568697d8567b37904f708b83c17b0e",
        "quote_id": "487dbe1038dfbc5aafda31d8e5ae4b822e12a59804eb78f33175787ed5e36c8c",
        "signature": "b8d91a0ad6247492f4131f203c37df4233ac9859b173f16eb4b692e36279a3cbfac20a101ff26e52a3524b22d9345de733a44b0bb581d54dde757965e9aabc0e",
        "transaction": "mock:4bc88bcf1d22a08a0bec88fa939408c7",
        "verification_ref": "d9d24f8203e80a4cb1c24100176e37011ded9d8e71e9c80bcbe5988e33714415"
      },
      "billing_published": false,
      "caller_hex": "aa38250e253a301d9ed1905ed33a743267568697d8567b37904f708b83c17b0e",
      "capability": "fixture-provider/fixture-tool",
      "chain": [
        {
          "checked_at_ns": 1000000000000001,
          "object": "net.payment.verification@1",
          "quote_id": "487dbe1038dfbc5aafda31d8e5ae4b822e12a59804eb78f33175787ed5e36c8c",
          "signature": "4aa65b0a1153f819cfba1b2c119ed7be1fd2183ff269ba0c644f5a5135cf664c0d97404c061d667a5a4b015db1e83d9ff4d65b06f63cccad01708bd01beadf0c",
          "signer": "0e75bc30c35e647831ba21555a646bee038177270885f12850947804429bbaf9",
          "status": "verified",
          "tier": "observed",
          "transaction": "mock:4bc88bcf1d22a08a0bec88fa939408c7",
          "verifier": {
            "endpoint": "mock"
          }
        }
      ],
      "expires_at_ns": 1000060000000000,
      "frozen": null,
      "idempotency_key": "459eaef6ff5115ce69983d756960090f23a81fb36ebaac9a478653f2eb121837",
      "in_flight": false,
      "in_flight_since_ns": 1000000000000001,
      "payload_b64": "eyJ4NDAyVmVyc2lvbiI6MiwiYWNjZXB0ZWQiOnsic2NoZW1lIjoibW9jayIsIm5ldHdvcmsiOiJtb2NrOm5ldCIsImFtb3VudCI6IjI1MDAiLCJhc3NldCI6Im11c2QiLCJwYXlUbyI6Im1vY2stcHJvdmlkZXItc2V0dGxlLWFkZHIiLCJtYXhUaW1lb3V0U2Vjb25kcyI6NjB9LCJwYXlsb2FkIjp7Im1vY2tfYXV0aG9yaXphdGlvbiI6InBheWVyLTEifX0=",
      "payload_hash": "cfec1dd25fde9d1d90faada90f75dc4ca893feb0fa6fcee64249128d13367270",
      "redeemed": true,
      "requirements_b64": "eyJzY2hlbWUiOiJtb2NrIiwibmV0d29yayI6Im1vY2s6bmV0IiwiYW1vdW50IjoiMjUwMCIsImFzc2V0IjoibXVzZCIsInBheVRvIjoibW9jay1wcm92aWRlci1zZXR0bGUtYWRkciIsIm1heFRpbWVvdXRTZWNvbmRzIjo2MH0=",
      "served": true
    }
  }
}"#;

    const FIXTURE_QUOTE_ID: &str =
        "487dbe1038dfbc5aafda31d8e5ae4b822e12a59804eb78f33175787ed5e36c8c";

    /// Every scalar/array reachable in `expected` must be present and
    /// equal at the same path in `actual`. Additive keys in `actual` are
    /// fine; a dropped or altered one is not.
    fn assert_preserved(expected: &serde_json::Value, actual: &serde_json::Value, path: &str) {
        match expected {
            serde_json::Value::Object(fields) => {
                let actual = actual
                    .as_object()
                    .unwrap_or_else(|| panic!("{path}: expected an object, got {actual}"));
                for (key, value) in fields {
                    let found = actual
                        .get(key)
                        .unwrap_or_else(|| panic!("{path}/{key}: dropped by the round trip"));
                    assert_preserved(value, found, &format!("{path}/{key}"));
                }
            }
            other => assert_eq!(other, actual, "{path}: value changed across the round trip"),
        }
    }

    /// A store written before `QuoteRecord` grew `input_hash` and
    /// `redeemed_for` must still load, must still mean what it meant, and
    /// must not lose anything on the next write.
    ///
    /// The failure this guards is not hypothetical: a new field without
    /// `#[serde(default)]` makes the whole file `StoreError::Corrupt`, and
    /// every quote an operator already sold becomes unredeemable at once.
    #[test]
    fn a_pre_change_engine_store_loads_and_round_trips_without_losing_fields() {
        let state: EngineState =
            serde_json::from_str(PRE_CHANGE_STORE).expect("a pre-change store still deserializes");

        let rec = state
            .quotes
            .get(FIXTURE_QUOTE_ID)
            .expect("the quote record survived the load");
        // The record still says what it said: paid, billed, and consumed.
        assert!(rec.redeemed, "the legacy record is still redeemed");
        assert!(rec.billing.is_some(), "the legacy billing event survived");
        assert_eq!(rec.chain.len(), 1, "the legacy verification chain survived");
        assert_eq!(rec.capability, "fixture-provider/fixture-tool");
        assert_eq!(rec.expires_at_ns, Some(1_000_060_000_000_000));
        // ...and the new fields read as "this record predates purchases",
        // which is what makes a legacy quote unusable for task admission
        // (`redeem_for_task` demands a matching `input_hash`) while
        // remaining redeemable exactly as before for invocations.
        assert_eq!(rec.input_hash, None);
        assert_eq!(rec.redeemed_for, None);
        assert_eq!(state.consumed.len(), 1, "the payload replay index survived");
        assert_eq!(
            state.consumed_transactions.len(),
            1,
            "the settlement replay index survived"
        );

        let before: serde_json::Value =
            serde_json::from_str(PRE_CHANGE_STORE).expect("fixture is json");
        let after = serde_json::to_value(&state).expect("re-serialize");
        assert_preserved(&before, &after, "");
    }
}
