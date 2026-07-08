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
//! Provider policy runs at quote issuance (never quote a caller you'd
//! deny — accepting a denied caller's payment creates refund obligations
//! P0 doesn't have); the WS4 `payment_gate` re-checks before the handler.
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
use crate::policy::store::{load_json, mutate_json, StoreError};
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
    Admitted,
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
    /// Whether the paid invocation was executed against this quote —
    /// set (at most once) by [`PaymentEngine::redeem_for_invocation`],
    /// the provider-side gate's check. Additive: pre-existing records
    /// default to unredeemed.
    #[serde(default)]
    redeemed: bool,
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
}

enum Claim {
    Fresh,
    AlreadySettled,
    AlreadyServed(Box<BillingEvent>, Option<VerificationTier>, bool),
    InProgress,
    Frozen(String),
    ReplayOtherQuote,
    QuoteAlreadyPaid,
}

fn last_verified_tier(chain: &[VerificationEvent]) -> Option<VerificationTier> {
    chain
        .iter()
        .rev()
        .find(|e| matches!(e.status, VerificationStatus::Verified))
        .map(|e| e.tier)
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
    /// Optional billing stream: every freshly-emitted billing event is
    /// appended (durable JSONL + in-process subscribers). Idempotent
    /// retries republish nothing — one event per idempotency key.
    billing_log: Option<Arc<BillingLog>>,
}

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
            billing_log: None,
        })
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

    /// Issue a signed quote. Provider policy runs **here**, before any
    /// value can be accepted; the registry check is the pre-sign
    /// hard-reject.
    pub fn issue_quote(
        &self,
        caller: EntityId,
        capability: &str,
        requirements: X402Carry<PaymentRequirements>,
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
            None,
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
        if now_ns >= quote.expires_at_ns.saturating_add(self.expiry_tolerance_ns) {
            return Ok(PaymentDecision::Rejected {
                reason: RejectReason::QuoteExpired,
            });
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
        let idem = IdempotencyScope {
            caller: quote.caller.clone(),
            provider: quote.provider.clone(),
            capability: quote.capability.clone(),
            quote_id: quote.quote_id.clone(),
        };

        // -- claim: check-and-mark under the lock, then release it before
        // any facilitator I/O.
        let quote_id = quote.quote_id.clone();
        let in_flight_ttl_ns = self.in_flight_ttl_ns;
        let claim = {
            let payload_hash = payload_hash.clone();
            let record = QuoteRecord {
                idempotency_key: idem.key(),
                payload_hash: payload_hash.clone(),
                capability: quote.capability.clone(),
                caller_hex: hex::encode(quote.caller.as_bytes()),
                requirements_b64: BASE64.encode(quote.requirements.bytes()),
                payload_b64: BASE64.encode(payload.bytes()),
                in_flight: true,
                in_flight_since_ns: Some(now_ns),
                frozen: None,
                served: false,
                redeemed: false,
                chain: Vec::new(),
                billing: None,
                billing_published: false,
            };
            mutate_json::<EngineState, _, _>(&self.state_path, move |s| {
                if let Some(rec) = s.quotes.get_mut(&quote_id) {
                    if let Some(reason) = &rec.frozen {
                        return Claim::Frozen(reason.clone());
                    }
                    if rec.payload_hash != payload_hash {
                        return Claim::QuoteAlreadyPaid;
                    }
                    if let Some(billing) = &rec.billing {
                        return Claim::AlreadyServed(
                            Box::new(billing.clone()),
                            last_verified_tier(&rec.chain),
                            rec.billing_published,
                        );
                    }
                    if rec.in_flight {
                        // Completion is atomic (chain push + in_flight=false
                        // in one commit), so an in_flight record has an
                        // empty chain: a prior attempt claimed it and then
                        // crashed (or is still running) before completing.
                        // Reclaim only after the TTL, refreshing the clock
                        // so a concurrent retry still sees InProgress and
                        // only one attempt re-runs verify/settle.
                        let stale = rec
                            .in_flight_since_ns
                            .map(|since| now_ns.saturating_sub(since) >= in_flight_ttl_ns)
                            .unwrap_or(true);
                        if !stale {
                            return Claim::InProgress;
                        }
                        rec.in_flight_since_ns = Some(now_ns);
                        return Claim::Fresh;
                    }
                    if !rec.chain.is_empty() {
                        return Claim::AlreadySettled;
                    }
                    rec.in_flight = true;
                    rec.in_flight_since_ns = Some(now_ns);
                    return Claim::Fresh;
                }
                if let Some(other) = s.consumed.get(&payload_hash) {
                    if *other != quote_id {
                        return Claim::ReplayOtherQuote;
                    }
                }
                s.consumed.insert(payload_hash, quote_id.clone());
                s.quotes.insert(quote_id.clone(), record);
                Claim::Fresh
            })
            .await?
        };

        match claim {
            Claim::Frozen(reason) => {
                return Ok(PaymentDecision::Rejected {
                    reason: RejectReason::QuoteFrozen(reason),
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
        let tier = settle.tier;
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

    /// Re-run facilitator verification for a settled quote: tier upgrades
    /// (late finality) or invalidation (reorg) land here.
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
        let tier = verify.tier;
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
                            last_verified_tier(&rec.chain).unwrap_or(VerificationTier::Observed);
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
    /// `binding`, when present, must be the paying identity's ed25519
    /// signature over [`invocation_binding_transcript`] — possession
    /// proof that the invoker is the payer. Present-but-invalid rejects;
    /// absent falls back to bearer semantics (the quote id is
    /// content-derived and unguessable), kept in P1 for pre-binding
    /// callers.
    pub async fn redeem_for_invocation(
        &self,
        tool_id: &str,
        quote_id: &str,
        binding: Option<&[u8]>,
    ) -> Result<RedeemDecision, EngineError> {
        let binding = binding.map(<[u8]>::to_vec);
        let tool_id = tool_id.to_string();
        let quote_id = quote_id.to_string();
        let decision = mutate_json::<EngineState, _, _>(&self.state_path, move |s| {
            let Some(rec) = s.quotes.get_mut(&quote_id) else {
                return RedeemDecision::Denied {
                    reason: RedeemDenialReason::UnknownQuote,
                };
            };
            if let Some(sig) = &binding {
                let Ok(sig_bytes) = <&[u8; 64]>::try_from(sig.as_slice()) else {
                    return RedeemDecision::Denied {
                        reason: RedeemDenialReason::BindingMalformed,
                    };
                };
                let payer = hex::decode(&rec.caller_hex)
                    .ok()
                    .and_then(|b| <[u8; 32]>::try_from(b).ok())
                    .map(EntityId::from_bytes);
                let Some(payer) = payer else {
                    return RedeemDecision::Denied {
                        reason: RedeemDenialReason::PayerRecordCorrupt,
                    };
                };
                let transcript = invocation_binding_transcript(&quote_id, &tool_id);
                if payer.verify_bytes(&transcript, sig_bytes).is_err() {
                    return RedeemDecision::Denied {
                        reason: RedeemDenialReason::BindingRejected,
                    };
                }
            }
            if let Some(reason) = &rec.frozen {
                return RedeemDecision::Denied {
                    reason: RedeemDenialReason::QuoteFrozen {
                        freeze_reason: reason.clone(),
                    },
                };
            }
            if rec.billing.is_none() {
                // "Never paid" and "paid, awaiting confidence" route
                // differently: an empty event chain means no settlement
                // was ever recorded; a non-empty one means the payment
                // exists but hasn't completed to billing (pending tier /
                // re-verify, or held as an exception).
                let reason = if rec.chain.is_empty() {
                    RedeemDenialReason::NotSettled
                } else {
                    RedeemDenialReason::SettlementPending
                };
                return RedeemDecision::Denied { reason };
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
                return RedeemDecision::Denied {
                    reason: RedeemDenialReason::WrongToolBinding {
                        capability: rec.capability.clone(),
                        tool_id: tool_id.clone(),
                    },
                };
            }
            if rec.redeemed {
                return RedeemDecision::Denied {
                    reason: RedeemDenialReason::AlreadyRedeemed,
                };
            }
            rec.redeemed = true;
            RedeemDecision::Admitted
        })
        .await?;
        Ok(decision)
    }

    /// Read-only lifecycle snapshot for gates and tests.
    pub async fn status(&self, quote_id: &str) -> Result<Option<QuoteStatus>, EngineError> {
        let state: EngineState = load_json(&self.state_path).await?;
        Ok(state.quotes.get(quote_id).map(|rec| QuoteStatus {
            frozen: rec.frozen.clone(),
            served: rec.served,
            tier: last_verified_tier(&rec.chain),
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
