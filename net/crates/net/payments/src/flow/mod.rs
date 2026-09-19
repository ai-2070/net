//! The caller-side paid-invocation flow (Workstream 4, demand side).
//!
//! When the gateway meets a priced capability, this flow runs the whole
//! Mode A lifecycle under policy: pick an announced accepts[] entry →
//! obtain the provider-signed quote → caller spend policy
//! (check-and-reserve) → author the x402 payload → deliver payment →
//! map the provider's decision. The model requests invocation; this flow
//! and the spend engine decide — approval prompts render in agent UX and
//! the decision lives in the shared policy store.
//!
//! The provider sits behind [`ProviderChannel`]: [`InProcessProvider`]
//! wraps a local [`PaymentEngine`] (tests, single-process demos, and the
//! provider side of the mesh service); a mesh-RPC channel implements the
//! same trait for cross-machine callers. The wire vocabulary
//! ([`PayResponse`]) is serializable from day one so the RPC channel is
//! config, not redesign.
//!
//! Quote discipline (P0, static pricing): the quote must instantiate the
//! announced template **byte-identically** — a provider quoting anything
//! other than its announced price is refused before policy even runs.
//! Never pay more than what discovery showed.

use std::sync::Arc;

use net::adapter::net::identity::{EntityId, EntityKeypair};
use serde::{Deserialize, Serialize};

use crate::core::billing_event::BillingEvent;
use crate::core::quote::PaymentQuote;
use crate::core::registry::AssetRegistry;
use crate::core::terms::PricingTerms;
use crate::core::verification::VerificationTier;
use crate::engine::{PaymentDecision, PaymentEngine};
use crate::policy::spend::{ApprovalOutcome, SpendDecision, SpendError, SpendPolicyEngine};
use crate::x402::payload::PaymentPayload;
use crate::x402::requirements::PaymentRequirements;
use crate::x402::{X402Carry, X402_VERSION};

#[cfg(feature = "mesh")]
pub mod a2a;
#[cfg(feature = "http-facilitator")]
pub mod http402;
#[cfg(feature = "mcp-gate")]
pub mod mcp_gate;
#[cfg(feature = "mesh")]
pub mod mesh;
pub mod signer;

/// Render a typed redeem denial into both caller-facing renderings at
/// once: the human message (the reason's `Display` — the exact strings
/// the wire has always carried) and the failure schematic. The single
/// render site for BOTH gates, so message and schematic are minted
/// together from the same typed reason and can never drift; the
/// mapping rows are pinned as `FailureSchematic`'s doc (the
/// caller-facing contract) and by the tests below.
#[cfg(any(feature = "mesh", feature = "mcp-gate"))]
fn denial_for(
    reason: &crate::engine::RedeemDenialReason,
    tool_id: &str,
    quote_id: &str,
) -> net_sdk::tool_payment::GateDenial {
    use crate::engine::RedeemDenialReason as R;
    use net_sdk::tool_payment::failure_vocab as v;
    // (class, actor, retryable, safe_to_retry, safe_to_requote,
    //  funds_moved, prior_payment, next_action)
    #[allow(clippy::type_complexity)]
    let (class, actor, retryable, retry, requote, funds, prior, next): (
        &str,
        &str,
        bool,
        bool,
        bool,
        &str,
        &str,
        Option<&str>,
    ) = match reason {
        R::UnknownQuote => (
            v::CLASS_NEW_QUOTE_REQUIRED,
            v::ACTOR_CALLER_AGENT,
            false,
            false,
            true,
            v::FUNDS_NO,
            v::PRIOR_NONE,
            Some("request_new_quote"),
        ),
        R::BindingMalformed => (
            v::CLASS_CALLER_CONFIGURATION_ERROR,
            v::ACTOR_CALLER_OPERATOR,
            false,
            false,
            false,
            v::FUNDS_UNKNOWN,
            v::PRIOR_UNKNOWN,
            Some("fix_payment_client"),
        ),
        // Not a security row: the caller sent no possession proof, which
        // for a provider that requires one is a client-configuration gap
        // and not evidence of anyone attacking anything. It is safe to
        // retry once the client signs the binding, but a fresh quote will
        // not help — the quote is fine, the client is not — so
        // `safe_to_requote` stays false and the action names the fix.
        R::BindingRequired => (
            v::CLASS_CALLER_CONFIGURATION_ERROR,
            v::ACTOR_CALLER_OPERATOR,
            false,
            false,
            false,
            v::FUNDS_UNKNOWN,
            v::PRIOR_UNKNOWN,
            Some("fix_payment_client"),
        ),
        // Security rows advise nothing: do not retry, do not just buy
        // another quote — report the mismatch.
        //
        // `WrongToolBinding` reports `funds_moved=unknown` /
        // `prior_payment=unknown` even though the engine only reaches it
        // AFTER the billing guard (the payment for the bound tool did
        // settle and bill). This is deliberate, not a stale/lossy read:
        // reporting `yes`/`consumed` here would confirm to a caller
        // redeeming against the WRONG tool that a real, paid quote
        // exists for some other capability. The conservative `unknown`
        // withholds that — grouping it with the binding-failure rows.
        //
        // `InputBindingMismatch` joins them for the same reason and with
        // the same posture: the presented proof authorizes a different
        // unit of work, and whether *that* other purchase was paid is
        // none of this caller's business — `unknown` withholds it. There
        // is nothing to advise: a fresh quote for the same wrong input
        // would mismatch identically, so `safe_to_requote` is false too.
        R::BindingRejected | R::WrongToolBinding { .. } | R::InputBindingMismatch => (
            v::CLASS_SECURITY_VIOLATION,
            v::ACTOR_CALLER_OPERATOR,
            false,
            false,
            false,
            v::FUNDS_UNKNOWN,
            v::PRIOR_UNKNOWN,
            None,
        ),
        R::PayerRecordCorrupt => (
            v::CLASS_PROVIDER_CONFIGURATION_ERROR,
            v::ACTOR_PROVIDER_OPERATOR,
            false,
            false,
            false,
            v::FUNDS_UNKNOWN,
            v::PRIOR_UNKNOWN,
            Some("contact_provider_operator"),
        ),
        // A freeze often signals replay/wrong-chain/reorg — typed
        // subreasons are reserved; until then the conservative verdict.
        R::QuoteFrozen { .. } => (
            v::CLASS_NON_RECOVERABLE,
            v::ACTOR_CALLER_OPERATOR,
            false,
            false,
            false,
            v::FUNDS_UNKNOWN,
            v::PRIOR_UNKNOWN,
            None,
        ),
        R::NotSettled => (
            v::CLASS_PAYMENT_REQUIRED,
            v::ACTOR_CALLER_AGENT,
            true,
            true,
            true,
            v::FUNDS_NO,
            v::PRIOR_NONE,
            Some("complete_payment"),
        ),
        R::SettlementPending => (
            v::CLASS_AUTOMATIC_RETRY,
            v::ACTOR_CALLER_AGENT,
            true,
            true,
            true,
            v::FUNDS_UNKNOWN,
            v::PRIOR_PENDING,
            Some("retry_after_reverification"),
        ),
        R::AlreadyRedeemed => (
            v::CLASS_NEW_QUOTE_REQUIRED,
            v::ACTOR_CALLER_AGENT,
            false,
            false,
            true,
            v::FUNDS_YES,
            v::PRIOR_CONSUMED,
            Some("request_new_quote"),
        ),
    };
    // The human body carries the full `Display` (byte-identical to the
    // pre-schematic wire); the schematic's copy is the redaction-safe
    // rendering — for `QuoteFrozen` that drops the free-form freeze text
    // so it never rides the structured header.
    let message = reason.to_string();
    let schematic_message = reason.schematic_message();
    net_sdk::tool_payment::GateDenial {
        schematic: net_sdk::tool_payment::FailureSchematic {
            object: net_sdk::tool_payment::TAG_PAYMENT_FAILURE.to_string(),
            code: v::CODE_PAYMENT.to_string(),
            stage: v::STAGE_REDEEM.to_string(),
            reason: reason.wire_reason().to_string(),
            message: net_sdk::tool_payment::FailureSchematic::cap_message(&schematic_message),
            retryable,
            recovery: net_sdk::tool_payment::Recovery {
                class: class.to_string(),
                actor: actor.to_string(),
                safe_to_retry: retry,
                safe_to_requote: requote,
                next_action: next.map(str::to_string),
            },
            handler_executed: false,
            funds_moved: funds.to_string(),
            prior_payment: prior.to_string(),
            quote_id: Some(quote_id.to_string()),
            tool_id: Some(tool_id.to_string()),
            extra: Default::default(),
        },
        message,
    }
}

/// The fail-closed engine-failure denial, rendered from NOTHING but the
/// generic verdict: the raw `EngineError` (file paths, serde detail,
/// facilitator responses) is logged server-side by the caller of this
/// function and never reaches it — the scrub survives by construction.
#[cfg(any(feature = "mesh", feature = "mcp-gate"))]
fn engine_unavailable_denial(tool_id: &str, quote_id: &str) -> net_sdk::tool_payment::GateDenial {
    use net_sdk::tool_payment::failure_vocab as v;
    let message = "payment engine unavailable (fail-closed)".to_string();
    net_sdk::tool_payment::GateDenial {
        schematic: net_sdk::tool_payment::FailureSchematic {
            object: net_sdk::tool_payment::TAG_PAYMENT_FAILURE.to_string(),
            code: v::CODE_PAYMENT.to_string(),
            stage: v::STAGE_REDEEM.to_string(),
            reason: "engine_unavailable".to_string(),
            message: message.clone(),
            // Retry is permitted but nothing stronger is promised — the
            // scrub can't distinguish transient from broken, and the
            // caller can't fix engine availability: the actor is the
            // provider operator.
            retryable: true,
            recovery: net_sdk::tool_payment::Recovery {
                class: v::CLASS_PROVIDER_CONFIGURATION_ERROR.to_string(),
                actor: v::ACTOR_PROVIDER_OPERATOR.to_string(),
                safe_to_retry: true,
                safe_to_requote: true,
                next_action: Some("retry_later".to_string()),
            },
            handler_executed: false,
            funds_moved: v::FUNDS_UNKNOWN.to_string(),
            prior_payment: v::PRIOR_UNKNOWN.to_string(),
            quote_id: Some(quote_id.to_string()),
            tool_id: Some(tool_id.to_string()),
            extra: Default::default(),
        },
        message,
    }
}

/// Redeem a paid quote against the engine for one invocation and map the
/// outcome to the provider-gate vocabulary: `Ok(())` admits,
/// `Err(denial)` refuses — the denial's message travels to the caller as
/// the error body (byte-identical to the pre-schematic wire) and its
/// schematic rides the `net-failure-schematic` reply header. Engine/store
/// failure is fail-closed — never serve on an unverifiable payment.
///
/// Single-sourced so the SDK-native gate (`mesh::EngineToolPaymentGate`)
/// and the MCP adapter gate (`mcp_gate::EnginePaymentAdmission`) cannot
/// drift: both are thin trait wrappers over this one mapping. (Plain
/// spans, not intra-doc links: each gate module is behind its own feature
/// while this fn compiles under `any(mesh, mcp-gate)`, so a link to the
/// other module would dangle when docs build with only one feature on.)
//
// `result_large_err`: the `Err` type is not ours to shrink here. A
// `GateDenial` (message + schematic) is the refusal type of the SDK's
// public `ToolPaymentGate` and the MCP adapter's `PaymentAdmission`,
// and both of this function's callers are thin impls of those traits —
// boxing the denial here would only be unboxed one frame up. The path
// is a cold refusal, taken once per rejected invocation, never on the
// admit path.
#[cfg(any(feature = "mesh", feature = "mcp-gate"))]
#[allow(clippy::result_large_err)]
pub(crate) async fn redeem_via_engine(
    engine: &PaymentEngine,
    tool_id: &str,
    quote_id: &str,
    binding: Option<&[u8]>,
) -> Result<(), net_sdk::tool_payment::GateDenial> {
    use crate::engine::RedeemDecision;
    match engine
        .redeem_for_invocation(tool_id, quote_id, binding)
        .await
    {
        Ok(RedeemDecision::Admitted { .. }) => Ok(()),
        Ok(RedeemDecision::Denied { reason }) => {
            let denial = denial_for(&reason, tool_id, quote_id);
            // Typed fields at the emission point: operators grep
            // verdicts, not prose.
            tracing::info!(
                reason = %denial.schematic.reason,
                stage = %denial.schematic.stage,
                recovery_class = %denial.schematic.recovery.class,
                tool_id,
                "payment redemption denied"
            );
            Err(denial)
        }
        Err(e) => {
            // Fail-closed — but the raw `EngineError` wraps StoreError /
            // EnvelopeError / X402Error, which can carry file paths, I/O
            // detail, or facilitator responses. Log the specifics
            // server-side; hand the caller only the generic verdict
            // (message AND schematic — `engine_unavailable_denial` never
            // sees the error).
            tracing::error!(error = %e, "payment engine unavailable (fail-closed)");
            Err(engine_unavailable_denial(tool_id, quote_id))
        }
    }
}

/// Redeem a paid quote against the engine for one **task admission** and
/// map the outcome to the A2A gate's vocabulary:
/// `Ok(TaskPaymentEvidence)` admits the reservation the submission
/// arrived against, `Err(denial)` refuses it.
///
/// The task twin of [`redeem_via_engine`], and the **single** denial
/// render site for the A2A seam — it shares `denial_for`, so the task
/// path cannot grow a second spelling of `input_binding_mismatch` (or of
/// any other reason) that drifts from the tool path's.
///
/// Three differences from the tool path, all of them the engine's:
/// `binding` is mandatory (a paid task is a long-running side effect, so
/// bearer presentation is never enough), the quote must have been issued
/// for `expected_input_hash` (the provider's own purchase hash, never a
/// value read off the request), and redemption is idempotent *for that
/// same purchase hash* so a provider that crashed between the engine
/// write and its journal write reconciles instead of charging twice.
///
/// `payer` travels out on the evidence: a serving path with a verified
/// end-to-end principal matches it against the admitted caller, so one
/// entity's payment cannot admit another's task.
//
// `result_large_err`: as `redeem_via_engine` — `GateDenial` is the
// refusal type of the SDK's public `TaskAdmissionGate`, and the only
// caller is a thin impl of that trait, so boxing here would be unboxed
// one frame up. Cold refusal path.
#[cfg(feature = "mesh")]
#[allow(clippy::result_large_err)]
pub(crate) async fn redeem_task_via_engine(
    engine: &PaymentEngine,
    claim: net_sdk::a2a_payment::TaskPaymentClaim<'_>,
) -> Result<net_sdk::a2a_payment::TaskPaymentEvidence, net_sdk::tool_payment::GateDenial> {
    use crate::engine::RedeemDecision;
    match engine
        .redeem_for_task(
            claim.tool_id,
            claim.quote_id,
            claim.binding,
            claim.expected_input_hash,
        )
        .await
    {
        Ok(RedeemDecision::Admitted { payer }) => Ok(net_sdk::a2a_payment::TaskPaymentEvidence {
            quote_id: claim.quote_id.to_string(),
            payer: *payer.as_bytes(),
        }),
        Ok(RedeemDecision::Denied { reason }) => {
            let denial = denial_for(&reason, claim.tool_id, claim.quote_id);
            tracing::info!(
                reason = %denial.schematic.reason,
                stage = %denial.schematic.stage,
                recovery_class = %denial.schematic.recovery.class,
                tool_id = claim.tool_id,
                "task admission redemption denied"
            );
            Err(denial)
        }
        Err(e) => {
            // Fail-closed, and the raw `EngineError` never reaches the
            // caller — same scrub as the invocation gate.
            tracing::error!(error = %e, "payment engine unavailable (fail-closed)");
            Err(engine_unavailable_denial(claim.tool_id, claim.quote_id))
        }
    }
}

/// A short, **non-authorizing** reference to a quote, for logs.
///
/// A quote id is a credential, not just an identifier: with bearer
/// redemption enabled (no invocation binding presented), possession of
/// the id is sufficient to consume the paid invocation — see
/// [`crate::engine::PaymentEngine::redeem_for_invocation`]. Logging it
/// in full puts a spendable credential in every log sink that scrapes
/// the process, and the sites that log it are failure paths, where the
/// quote is most likely to still be unredeemed.
///
/// Operators reading these lines need to *correlate* events, not to
/// reconstruct the id, so they get a truncated hash instead. The domain
/// separator keeps this from colliding with any other blake3 use in the
/// crate; 8 bytes is ample to correlate within one process's logs and
/// far too short to invert into a 32-byte transcript hash.
pub(crate) fn quote_ref(quote_id: &str) -> String {
    // Keyed, and the key never leaves the process.
    //
    // An unkeyed digest here would not be one-way in practice: a quote id
    // is `blake3(provider ‖ caller ‖ terms_hash ‖ issued_at_ns)`, and a
    // log reader knows the provider, the caller and the announced terms.
    // Only the issuance instant is unknown, and it is bounded by the log
    // line's own timestamp — a few hours of nanoseconds is a small enough
    // space to enumerate against a truncated digest. The "short hash"
    // would hand back the credential it was meant to withhold.
    //
    // A per-process key defeats that: correlation still works within one
    // process's logs, which is the entire operator need, while nobody
    // holding only the logs can invert or precompute.
    static KEY: std::sync::OnceLock<[u8; 32]> = std::sync::OnceLock::new();
    let key = KEY.get_or_init(|| {
        // Derived from `RandomState`, which the standard library seeds
        // per-process from the OS. Used rather than a new rng dependency
        // because this is a logging path, not the money path — the money
        // path deliberately carries no rng at all.
        use std::hash::{BuildHasher, Hasher as _};
        let state = std::collections::hash_map::RandomState::new();
        let mut key = [0u8; 32];
        for (i, chunk) in key.chunks_mut(8).enumerate() {
            let mut h = state.build_hasher();
            h.write_u64(i as u64);
            chunk.copy_from_slice(&h.finish().to_le_bytes());
        }
        key
    });
    let mut hasher = blake3::Hasher::new_keyed(key);
    hasher.update(b"net.payments.log_ref@1");
    hasher.update(quote_id.as_bytes());
    hex::encode(&hasher.finalize().as_bytes()[..8])
}

/// Time source. There is no global clock — every timestamp in the flow
/// comes from here, and tests inject fixed instants.
pub trait Clock: Send + Sync {
    fn now_ns(&self) -> u64;
}

/// Wall-clock nanoseconds since the Unix epoch.
pub struct SystemClock;
impl Clock for SystemClock {
    fn now_ns(&self) -> u64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| u64::try_from(d.as_nanos()).unwrap_or(u64::MAX))
            .unwrap_or(0)
    }
}

/// Transport failure at the provider boundary.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("provider channel: {message} (retryable={retryable})")]
pub struct ChannelError {
    pub message: String,
    pub retryable: bool,
}

/// The provider's wire answer to a payment delivery — a serializable
/// projection of [`PaymentDecision`] (billing events travel as their
/// canonical bytes so signatures survive the trip).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum PayResponse {
    Served {
        billing_event: String,
        transaction: Option<String>,
    },
    PendingTier {
        reached: String,
        required: String,
    },
    Rejected {
        reason: String,
    },
    Invalidated {
        reason: String,
    },
    Exception {
        kind: String,
    },
    InProgress,
    Failure {
        retryable: bool,
        message: String,
    },
}

impl PayResponse {
    /// Project an engine decision onto the wire vocabulary.
    pub fn from_decision(
        decision: &PaymentDecision,
    ) -> Result<Self, crate::core::canonical::EnvelopeError> {
        Ok(match decision {
            PaymentDecision::Served { billing, .. } => PayResponse::Served {
                billing_event: String::from_utf8(crate::core::canonical::canonical_bytes(
                    billing.as_ref(),
                )?)
                .unwrap_or_default(),
                transaction: billing.transaction.clone(),
            },
            PaymentDecision::PendingTier { reached, required } => PayResponse::PendingTier {
                reached: format!("{reached:?}"),
                required: format!("{required:?}"),
            },
            PaymentDecision::Rejected { reason } => PayResponse::Rejected {
                reason: reason.to_string(),
            },
            PaymentDecision::Invalidated { reason } => PayResponse::Invalidated {
                reason: format!("{reason:?}"),
            },
            PaymentDecision::Exception { kind } => PayResponse::Exception {
                kind: format!("{kind:?}"),
            },
            PaymentDecision::InProgress => PayResponse::InProgress,
            PaymentDecision::FacilitatorFailure {
                retryable, message, ..
            } => PayResponse::Failure {
                retryable: *retryable,
                message: message.clone(),
            },
        })
    }
}

/// The provider boundary: quote issuance + payment delivery. Quotes
/// travel as canonical envelope bytes (the flow decodes and verifies —
/// byte-preservation discipline holds across the channel).
#[async_trait::async_trait]
pub trait ProviderChannel: Send + Sync {
    /// Request a quote.
    ///
    /// `provider` is the identity the caller *intends* to pay, taken from
    /// the announced pricing terms. It is not decoration: the mesh
    /// channel binds it into the signed quote request, which is what
    /// stops a captured request being replayed to a different provider.
    ///
    /// `input_hash` binds the quote to one exact unit of work (blake3
    /// hex). It travels inside the caller-signed quote request, lands on
    /// the issued quote, and therefore in `terms_hash` → `quote_id` — so
    /// the caller's later invocation binding transitively proves *which*
    /// purchase was authorized. `None` is the capability-level shape and
    /// produces exactly the quote this trait issued before the parameter
    /// existed.
    async fn quote(
        &self,
        caller: &EntityId,
        provider: &EntityId,
        capability: &str,
        template: &X402Carry<PaymentRequirements>,
        input_hash: Option<&str>,
    ) -> Result<Vec<u8>, ChannelError>;

    async fn pay(
        &self,
        quote_bytes: &[u8],
        payload: &X402Carry<PaymentPayload>,
    ) -> Result<PayResponse, ChannelError>;
}

/// The provider side, in-process: wraps a [`PaymentEngine`]. This is the
/// implementation single-process tests and demos use, and the exact code
/// a mesh RPC handler delegates to.
pub struct InProcessProvider {
    engine: Arc<PaymentEngine>,
    clock: Arc<dyn Clock>,
    /// Quote validity window.
    ttl_ns: u64,
    /// The provider's confidence requirement before serving.
    required_tier: VerificationTier,
}

impl InProcessProvider {
    pub fn new(engine: Arc<PaymentEngine>, clock: Arc<dyn Clock>) -> Self {
        Self {
            engine,
            clock,
            ttl_ns: 60_000_000_000,
            required_tier: VerificationTier::Observed,
        }
    }

    pub fn with_quote_ttl_ns(mut self, ttl_ns: u64) -> Self {
        self.ttl_ns = ttl_ns;
        self
    }

    pub fn with_required_tier(mut self, tier: VerificationTier) -> Self {
        self.required_tier = tier;
        self
    }

    /// The provider identity quotes are signed with — the destination a
    /// `net.payment.quote_request@1` must be addressed to.
    pub fn provider_id(&self) -> &EntityId {
        self.engine.provider_id()
    }

    /// The flow's clock, so a wire handler stamps freshness from the same
    /// source the lifecycle does.
    pub fn now_ns(&self) -> u64 {
        self.clock.now_ns()
    }
}

#[async_trait::async_trait]
impl ProviderChannel for InProcessProvider {
    async fn quote(
        &self,
        caller: &EntityId,
        provider: &EntityId,
        capability: &str,
        template: &X402Carry<PaymentRequirements>,
        input_hash: Option<&str>,
    ) -> Result<Vec<u8>, ChannelError> {
        // In-process there is no wire, so no signed request and nothing to
        // forge — the caller identity is passed by the same process that
        // owns it, and this channel cannot misrepresent its own engine.
        //
        // Deliberately NOT re-checked against `self.engine.provider_id()`
        // here: the flow already compares the *signed quote's* provider
        // against the announced terms, which is the security-relevant
        // check (it catches a provider that signs a quote naming someone
        // else, which an engine-identity comparison cannot). Duplicating
        // it here only short-circuits that path and reclassifies a policy
        // `Denied` as a transport `Failed`.
        let _ = provider;
        // Provider policy runs inside issue_quote — never quote a caller
        // you'd deny.
        let quote = self
            .engine
            .issue_quote(
                caller.clone(),
                capability,
                template.clone(),
                input_hash,
                self.clock.now_ns(),
                self.ttl_ns,
            )
            .map_err(|e| ChannelError {
                message: e.to_string(),
                retryable: false,
            })?;
        crate::core::canonical::canonical_bytes(&quote).map_err(|e| ChannelError {
            message: e.to_string(),
            retryable: false,
        })
    }

    async fn pay(
        &self,
        quote_bytes: &[u8],
        payload: &X402Carry<PaymentPayload>,
    ) -> Result<PayResponse, ChannelError> {
        let quote = PaymentQuote::from_json_bytes(quote_bytes).map_err(|e| ChannelError {
            message: e.to_string(),
            retryable: false,
        })?;
        let decision = self
            .engine
            .accept_payment(&quote, payload, self.required_tier, self.clock.now_ns())
            .await
            .map_err(|e| ChannelError {
                message: e.to_string(),
                retryable: false,
            })?;
        PayResponse::from_decision(&decision).map_err(|e| ChannelError {
            message: e.to_string(),
            retryable: false,
        })
    }
}

/// The flow's structured outcome — payments-native; the `mcp-gate`
/// feature maps it 1:1 onto `net_mcp::serve::PaymentFlowDecision`.
#[derive(Debug, Clone)]
pub enum CallerDecision {
    /// Payment cleared: `quote_id` is the redemption binding the
    /// invocation must carry to the provider's gate, `binding_sig` the
    /// paying identity's possession proof over it, and `proof` the full
    /// payment context (settlement refs, the signed billing event).
    Paid {
        quote_id: String,
        binding_sig: Option<Vec<u8>>,
        proof: serde_json::Value,
    },
    RequiresPaymentApproval {
        quote_id: String,
        policy_reason: String,
        approve_hint: String,
    },
    Denied {
        policy_reason: String,
    },
    Failed {
        /// The quote this attempt was about, when one existed.
        ///
        /// `None` only before a quote could be obtained at all (bad
        /// terms, a channel error on the quote call). Once there is a
        /// quote id, a failure is *ambiguous about that specific
        /// purchase* — the payment may have landed — and a caller that
        /// wants to resume rather than re-quote needs the id to do it.
        quote_id: Option<String>,
        message: String,
        retryable: bool,
    },
}

/// A provider-signed quote the caller has verified against the announced
/// terms, plus everything needed to resume the purchase later.
///
/// The bytes are kept beside the decoded quote deliberately: they are the
/// exact canonical envelope the provider signed, they are what
/// [`CallerPaymentFlow::pay_exact`] must present back, and re-encoding a
/// decoded quote is the one thing byte-preservation discipline forbids.
/// A caller that persists a purchase across a restart persists
/// [`Self::quote_bytes`] (and the authored payload bytes), never a
/// re-serialization.
#[derive(Debug, Clone)]
pub struct BoundQuote {
    /// The decoded, verified quote.
    pub quote: PaymentQuote,
    /// Its canonical envelope bytes, exactly as the provider signed them.
    pub quote_bytes: Vec<u8>,
    /// The approval hold this quote was resumed from, if any. A
    /// successful payment consumes it; nothing else should.
    pub approval_id: Option<String>,
}

/// The caller-side payment flow: one per caller identity + policy store.
pub struct CallerPaymentFlow {
    caller: Arc<EntityKeypair>,
    spend: SpendPolicyEngine,
    registry: AssetRegistry,
    provider: Arc<dyn ProviderChannel>,
    clock: Arc<dyn Clock>,
    /// Settlement signers by CAIP-2 namespace (`eip155`, …). A real
    /// network's accepts entry is settleable only when its namespace
    /// has a signer; nothing here can hold key material (see
    /// [`signer::SchemeSigner`]).
    signers: std::collections::BTreeMap<String, Arc<dyn signer::SchemeSigner>>,
}

impl CallerPaymentFlow {
    pub fn new(
        caller: Arc<EntityKeypair>,
        spend: SpendPolicyEngine,
        registry: AssetRegistry,
        provider: Arc<dyn ProviderChannel>,
        clock: Arc<dyn Clock>,
    ) -> Self {
        Self {
            caller,
            spend,
            registry,
            provider,
            clock,
            signers: std::collections::BTreeMap::new(),
        }
    }

    /// Register a settlement signer for a CAIP-2 namespace
    /// (e.g. `eip155`). Without one, that namespace's accepts entries
    /// are not settleable and are skipped at selection.
    pub fn with_signer(
        mut self,
        namespace: impl Into<String>,
        signer: Arc<dyn signer::SchemeSigner>,
    ) -> Self {
        self.signers.insert(namespace.into(), signer);
        self
    }

    /// Run the paid lifecycle for `capability` (display form,
    /// `provider/capability`) against its announced `pricing_terms`.
    ///
    /// A composition of the four staged verbs —
    /// [`quote_bound`](Self::quote_bound) → [`reserve_spend`](Self::reserve_spend)
    /// → [`author`](Self::author) → [`pay_exact`](Self::pay_exact) — and
    /// nothing else. Callers that must survive a crash between authoring
    /// and paying drive the stages themselves and persist the bytes in
    /// between; callers that do not, use this.
    pub async fn run(&self, capability: &str, pricing_terms: &str) -> CallerDecision {
        let bound = match self.quote_bound(capability, pricing_terms, None).await {
            Ok(bound) => bound,
            Err(decision) => return decision,
        };
        let quote_id = bound.quote.quote_id.clone();

        match self.reserve_spend(&bound.quote).await {
            Ok(SpendDecision::Allowed) => {}
            Ok(SpendDecision::RequiresPaymentApproval {
                quote_id,
                policy_reason,
                approve_hint,
            }) => {
                return CallerDecision::RequiresPaymentApproval {
                    quote_id,
                    policy_reason,
                    approve_hint,
                }
            }
            Ok(SpendDecision::Denied { policy_reason }) => {
                return CallerDecision::Denied { policy_reason }
            }
            Err(e) => {
                return CallerDecision::Failed {
                    quote_id: Some(quote_id),
                    message: e.to_string(),
                    retryable: false,
                }
            }
        }

        let payload = match self.author(&bound.quote).await {
            Ok(p) => p,
            Err(message) => {
                // Nothing was exposed: the payload never left this
                // process, so the reservation goes back.
                self.release(&bound.quote, self.clock.now_ns()).await;
                return CallerDecision::Failed {
                    quote_id: Some(quote_id),
                    message,
                    retryable: false,
                };
            }
        };

        let decision = self.pay_exact(&bound.quote_bytes, payload.bytes()).await;
        // A redeemed approval is consumed by the successful pay, and only
        // by that: an ambiguous or refused outcome leaves the human's
        // approval in place for the resume.
        if matches!(decision, CallerDecision::Paid { .. }) {
            if let Some(held_id) = bound.approval_id {
                let _ = self.spend.clear_approval(&held_id).await;
            }
        }
        decision
    }

    /// Stage 1 — obtain a provider-signed quote for `capability` under
    /// its announced `pricing_terms`, bound to `input_hash` when the
    /// caller is buying one exact unit of work.
    ///
    /// Picks the first announced `accepts[]` entry this caller can
    /// *settle* (the mock network always; a real network only when its
    /// CAIP-2 namespace has a configured signer — whether policy
    /// *permits* the spend is [`reserve_spend`](Self::reserve_spend)'s
    /// job), prefers an operator-approved held quote over a fresh one,
    /// and verifies whatever it gets against the announced terms before
    /// returning it. **No money moves and no budget is reserved here**,
    /// which is what makes it safe to call merely to display a price.
    ///
    /// The held-quote path is per-purchase, and *keyed* per purchase:
    /// the store is asked for the hold bound to this exact
    /// `input_hash`, so a sibling purchase's approved hold is neither
    /// returned nor disturbed (it is still valid for *its* purchase)
    /// and cannot mask this one's. Only this purchase's own expired or
    /// unparseable hold is cleared.
    ///
    /// The returned quote's `input_hash` must **equal** the requested
    /// one, exactly. Verifying signature, parties, capability,
    /// requirements and expiry says the quote is genuine; only this
    /// comparison says it prices *this* purchase, and a caller that
    /// skips it can settle a real payment for evidence its reservation
    /// will refuse.
    ///
    /// `Err` carries the terminal [`CallerDecision`] the flow would
    /// return, so `run` composes by `?`-shaped early exit and the two can
    /// never disagree about how a quote failure reads.
    pub async fn quote_bound(
        &self,
        capability: &str,
        pricing_terms: &str,
        input_hash: Option<&str>,
    ) -> Result<BoundQuote, CallerDecision> {
        // -- [1] parse the announced terms; pick the first accepts[]
        //    entry this caller can *settle*: the mock network always, a
        //    real network only when its namespace has a configured
        //    settlement signer. (Whether policy *permits* the spend is
        //    [3]'s job — settleability is about capability, not
        //    authorization.)
        let terms = match PricingTerms::from_json_bytes(pricing_terms.as_bytes()) {
            Ok(t) => t,
            Err(e) => {
                return Err(CallerDecision::Denied {
                    policy_reason: format!("announced pricing terms are invalid: {e}"),
                })
            }
        };
        let Some(template) = terms.accepts.iter().find(|t| self.can_settle(t.view())) else {
            let offered: Vec<String> = terms
                .accepts
                .iter()
                .map(|t| format!("({}, {})", t.view().scheme, t.view().network))
                .collect();
            return Err(CallerDecision::Denied {
                policy_reason: format!(
                    "no settleable accepts[] entry: terms offer {offered:?}; this caller \
                     settles mock:* always and exact/eip155 when a signer is configured"
                ),
            });
        };

        // -- [2] the quote: an approved held quote first (the human's
        //    approval applies to the exact quote they saw — this is the
        //    retry-after-approval path), else a fresh provider-signed one.
        // Asked for by *purchase*, not by capability. Two A2A purchases
        // of one capability can both be held and both be approved; a
        // capability-only lookup answers with whichever quote id sorts
        // first, so the purchase that is not it declines to ride a hold
        // that is not its own (correct) and then quotes fresh — leaving
        // its own granted approval on file and asking the human again.
        let held = match self
            .spend
            .approved_quote_for_input(capability, input_hash)
            .await
        {
            Ok(held) => held,
            Err(e) => {
                return Err(CallerDecision::Failed {
                    quote_id: None,
                    message: e.to_string(),
                    retryable: false,
                })
            }
        };
        let mut approval_id = None;
        let mut resumed: Option<Vec<u8>> = None;
        if let Some((held_id, held_bytes)) = held {
            match PaymentQuote::from_json_bytes(&held_bytes) {
                Ok(held_quote) if !held_quote.is_expired_at(self.clock.now_ns()) => {
                    // The lookup already selected on the record's
                    // denormalized binding; this re-reads it off the
                    // provider-**signed** envelope, which is the only
                    // copy an edited policy file cannot lie about. A
                    // hold that disagrees is not this purchase's, so
                    // it is left on file rather than ridden.
                    if held_quote.input_hash.as_deref() == input_hash {
                        approval_id = Some(held_id);
                        resumed = Some(held_bytes);
                    }
                }
                // Expired or unparseable hold: drop it and fall
                // through to a fresh quote (which will hold again if
                // policy still objects — a new approval for a new
                // quote, never a silent carry-over).
                _ => {
                    let _ = self.spend.clear_approval(&held_id).await;
                }
            }
        }
        let quote_bytes = match resumed {
            Some(bytes) => bytes,
            None => match self
                .provider
                .quote(
                    self.caller.entity_id(),
                    &terms.provider,
                    capability,
                    template,
                    input_hash,
                )
                .await
            {
                Ok(b) => b,
                Err(e) => {
                    return Err(CallerDecision::Failed {
                        quote_id: None,
                        message: e.message,
                        retryable: e.retryable,
                    })
                }
            },
        };
        let quote = match PaymentQuote::from_json_bytes(&quote_bytes) {
            Ok(q) => q,
            Err(e) => {
                return Err(CallerDecision::Denied {
                    policy_reason: format!("provider quote failed verification: {e}"),
                })
            }
        };
        if quote.caller != *self.caller.entity_id() {
            return Err(CallerDecision::Denied {
                policy_reason: "quote was issued to a different caller".to_string(),
            });
        }
        if quote.capability != capability {
            return Err(CallerDecision::Denied {
                policy_reason: "quote binds a different capability".to_string(),
            });
        }
        if quote.provider != terms.provider {
            return Err(CallerDecision::Denied {
                policy_reason: "quote provider does not match the announced terms provider"
                    .to_string(),
            });
        }
        if quote.requirements.bytes() != template.bytes() {
            return Err(CallerDecision::Denied {
                policy_reason: "quote deviates from the announced terms — never pay more \
                                than discovery showed"
                    .to_string(),
            });
        }
        // The quote must price **the work that was asked about**. Every
        // other check above establishes who/what/how-much; this one
        // establishes *which unit of work*, and it is the only thing
        // standing between a caller and paying for evidence its
        // reservation cannot redeem. A provider that answers a bound
        // request with an unbound quote (`input_hash: None`) or with
        // somebody else's hash is refused here — before spend policy,
        // before authoring, before any money moves. The comparison is
        // exact in both directions: an unbound request must not come
        // back bound either, or the caller would be paying under a
        // commitment it never made.
        if quote.input_hash.as_deref() != input_hash {
            return Err(CallerDecision::Denied {
                policy_reason: format!(
                    "quote is bound to input {:?} but this purchase asked for {:?} — \
                     never pay for work the quote does not price",
                    quote.input_hash.as_deref(),
                    input_hash
                ),
            });
        }
        if quote.is_expired_at(self.clock.now_ns()) {
            return Err(CallerDecision::Failed {
                quote_id: Some(quote.quote_id.clone()),
                message: "provider quote arrived already expired".to_string(),
                retryable: true,
            });
        }

        Ok(BoundQuote {
            quote,
            quote_bytes,
            approval_id,
        })
    }

    /// Stage 2 — caller spend policy: check + reserve, one locked RMW.
    ///
    /// The verdict is the spend engine's own, unmapped: a staged caller
    /// has to tell `RequiresPaymentApproval` (park the purchase, wait for
    /// a human) from `Denied` (never going to happen) from a store error
    /// (retry the read), and collapsing those into one enum here would
    /// only make it guess.
    pub async fn reserve_spend(&self, quote: &PaymentQuote) -> Result<SpendDecision, SpendError> {
        self.spend
            .check_and_reserve(quote, &self.registry, self.clock.now_ns())
            .await
    }

    /// Give back a reservation this flow took but will not spend.
    ///
    /// The counterpart to [`reserve_spend`](Self::reserve_spend) for a
    /// staged caller: [`run`](Self::run) releases internally on the paths
    /// where nothing was exposed, but a caller driving the stages itself
    /// can lose a race *after* reserving (two attempts on one purchase,
    /// one of which never authors) and must hand its holder back or the
    /// day counter keeps budget nobody is spending.
    ///
    /// Refcounted and owner-checked by the spend engine: the claim is
    /// only returned to the counter when the last holder releases.
    pub async fn release_spend(&self, quote: &PaymentQuote) {
        self.release(quote, self.clock.now_ns()).await;
    }

    /// Does this caller still hold a spend reservation for `quote_id`?
    ///
    /// An *observation*, for a staged caller that has to record whether a
    /// refusal left the budget claimed: [`pay_exact`](Self::pay_exact)
    /// decides that per scheme (see `reject_releases_reservation`), and
    /// reading the store back is the difference between recording what
    /// happened and re-deriving what should have.
    pub async fn spend_reservation_held(&self, quote_id: &str) -> Result<bool, SpendError> {
        Ok(self.spend.reservation(quote_id).await?.is_some())
    }

    /// Where the operator approval for `quote_id` stands.
    ///
    /// A staged caller parked on an approval needs the three-way answer
    /// (see [`ApprovalOutcome`]), because re-running
    /// [`reserve_spend`](Self::reserve_spend) would record a *new*
    /// pending hold and hide whatever the operator decided.
    ///
    /// **`Gone` means the hold is no longer on file — not that the
    /// operator said no.** A hold is also removed when the payment it
    /// authorized lands ([`Self::clear_approval`] on the paid path), so
    /// a caller whose sibling attempt paid while it was parked reads
    /// `Gone` for a purchase that succeeded. Treating `Gone` as a
    /// rejection on its own therefore contradicts the record: the
    /// caller's own purchase state is the authority on whether money
    /// moved, and this answer only says no approval is being held.
    pub async fn approval_state(&self, quote_id: &str) -> Result<ApprovalOutcome, SpendError> {
        self.spend.approval_state(quote_id).await
    }

    /// Drop the approval hold on `quote_id`.
    ///
    /// Best-effort by design, and called on exactly two occasions: the
    /// payment the human approved has landed (the hold is spent), or the
    /// quote it was for is being replaced (a new quote needs a new
    /// approval). A failure leaves a stale hold that
    /// [`quote_bound`](Self::quote_bound) discards on sight, so it
    /// cannot authorize anything.
    pub async fn clear_approval(&self, quote_id: &str) {
        if let Err(e) = self.spend.clear_approval(quote_id).await {
            tracing::warn!(quote_ref = %quote_ref(quote_id), error = %e, "clearing the approval hold failed");
        }
    }

    /// The identity this flow pays as — what a caller-side purchase
    /// record is keyed by (one authoritative attempt per caller, provider
    /// and task).
    pub fn caller(&self) -> &EntityId {
        self.caller.entity_id()
    }

    /// Stage 3 — author the x402 payload for the quoted scheme.
    ///
    /// Nonces derive from the quote id, so a same-quote retry re-authors
    /// the *same* payload (idempotent at the provider) while distinct
    /// quotes never collide. The returned carry's
    /// [`bytes`](X402Carry::bytes) are what a resumable caller persists
    /// and later hands back to [`pay_exact`](Self::pay_exact) unchanged.
    ///
    /// Dispatches on the quoted scheme/network; the selection guard in
    /// [`quote_bound`](Self::quote_bound) makes the fall-through
    /// unreachable in practice, and it fails closed anyway.
    pub async fn author(&self, quote: &PaymentQuote) -> Result<X402Carry<PaymentPayload>, String> {
        let requirements = quote.requirements.view();
        let payload_object = if requirements.network.starts_with("mock:") {
            let nonce = {
                let mut hasher = blake3::Hasher::new();
                hasher.update(b"net.payments.mock.payload_nonce@1");
                hasher.update(quote.quote_id.as_bytes());
                hasher.update(self.caller.entity_id().as_bytes());
                hex::encode(hasher.finalize().as_bytes())
            };
            serde_json::json!({
                "mock_authorization": hex::encode(self.caller.entity_id().as_bytes()),
                "nonce": nonce,
            })
        } else if self.can_settle(requirements) && requirements.network.starts_with("eip155:") {
            // exact / eip155: EIP-3009 typed data through the signer.
            let signer = self
                .signers
                .get("eip155")
                .ok_or_else(|| "no eip155 signer configured".to_string())?;
            let auth = exact_evm_authorization_for_quote(quote, &signer.address());
            let typed = crate::x402::schemes::exact_evm::typed_data(requirements, &auth)
                .map_err(|e| e.to_string())?;
            let signature = signer
                .sign_typed_data(&typed)
                .await
                .map_err(|e| e.to_string())?;
            crate::x402::schemes::exact_evm::payload_object(&auth, &signature)
        } else if self.can_settle(requirements)
            && OPAQUE_BLOB_NAMESPACES
                .contains(&requirements.network.split(':').next().unwrap_or_default())
        {
            // exact / solana | xrpl: the wallet authors the opaque blob
            // (partially-signed SPL transfer / presigned XRPL Payment) from
            // the intent derived from the quoted requirements, via the
            // shared `author_opaque_blob_payload` (kept symmetric with the
            // HTTP door). Retry honesty on this mesh path: idempotency holds
            // at the quote — a re-signed SPL blob binds a fresh blockhash; an
            // xrpl retry must re-present the IDENTICAL blob (never re-sign
            // with a fresh Sequence), an expired LastLedgerSequence means a
            // fresh quote, not a fresh signature.
            let namespace = requirements.network.split(':').next().unwrap_or_default();
            let signer = self
                .signers
                .get(namespace)
                .ok_or_else(|| format!("no {namespace} signer configured"))?;
            author_opaque_blob_payload(namespace, requirements, signer).await?
        } else {
            return Err(format!(
                "no payload author for scheme `{}` on `{}` (fail-closed)",
                requirements.scheme, requirements.network
            ));
        };
        X402Carry::author(&PaymentPayload {
            x402_version: X402_VERSION,
            resource: None,
            accepted: requirements.clone(),
            payload: payload_object,
            extensions: None,
        })
        .map_err(|e| e.to_string())
    }

    /// Stage 4 — deliver exactly these bytes and map the provider's
    /// answer, including what it means for the spend reservation.
    ///
    /// Takes bytes rather than typed values on purpose: this is the verb
    /// a caller re-enters after a lost reply or a restart, and the
    /// contract is that it re-sends the **identical** payload. The
    /// provider's acceptance is payload-idempotent, so a repeat resolves
    /// to the original verdict rather than paying twice — which only
    /// holds if the bytes are the ones that were sent, not a
    /// re-serialization of them.
    ///
    /// **Reservation discipline lives here.** A terminal refusal releases
    /// the caller's spend reservation only for a scheme whose claimed
    /// non-settlement is trustworthy (see
    /// `reject_releases_reservation`); every real scheme authors a
    /// self-contained bearer pull authorization the counterparty could
    /// settle regardless of what it reports back, so its "rejected" is
    /// not proof and the reservation stands — exactly as on transport
    /// ambiguity.
    ///
    /// A failure that happens **before the wire** is a different case
    /// and releases unconditionally: if the stored payload will not
    /// even decode, nothing was ever sent, so no scheme's bearer
    /// authorization is outstanding and the claim would otherwise sit
    /// against `max_per_day` with nothing left to release it. (The
    /// quote itself failing to decode is the one path that cannot
    /// release — there is no quote to identify the claim by.)
    pub async fn pay_exact(&self, quote_bytes: &[u8], payload_bytes: &[u8]) -> CallerDecision {
        let quote = match PaymentQuote::from_json_bytes(quote_bytes) {
            Ok(q) => q,
            Err(e) => {
                return CallerDecision::Failed {
                    quote_id: None,
                    message: format!("stored quote failed verification: {e}"),
                    retryable: false,
                }
            }
        };
        let payload: X402Carry<PaymentPayload> = match X402Carry::from_bytes(payload_bytes.to_vec())
        {
            Ok(p) => p,
            Err(e) => {
                // Terminal, and terminal *before the wire*: the bytes
                // never became a request, so no authorization reached
                // the provider and nothing can settle. That is a
                // stronger guarantee than the `Rejected` arm's — which
                // has to keep the reservation because a provider
                // holding a bearer authorization could settle it while
                // claiming otherwise — so this release is
                // unconditional, not gated on the scheme.
                //
                // Without it the claim is orphaned: `release_spend` is
                // never called for this attempt, the quote is dead, and
                // the amount stays committed against `max_per_day`
                // until retention ages the counter out days later.
                self.release(&quote, self.clock.now_ns()).await;
                return CallerDecision::Failed {
                    quote_id: Some(quote.quote_id.clone()),
                    message: format!("stored payment payload is not a valid x402 payload: {e}"),
                    retryable: false,
                };
            }
        };
        let quote_id = quote.quote_id.clone();

        match self.provider.pay(quote_bytes, &payload).await {
            Ok(PayResponse::Served { billing_event, transaction }) => {
                // Verify the provider-supplied billing event before recording
                // it as dispute/audit evidence: from_json_bytes checks tag +
                // id-derivation + scope + signature, and we additionally
                // require it to bind THIS quote, caller, and provider. The
                // payment already served (money moved), so a bad evidence blob
                // is not a fund loss — but it must not be recorded as
                // trustworthy: drop it from the proof and warn.
                let verified_billing = match BillingEvent::from_json_bytes(billing_event.as_bytes())
                {
                    Ok(ev)
                        if ev.quote_id == quote.quote_id
                            && ev.payer == *self.caller.entity_id()
                            && ev.payee == quote.provider =>
                    {
                        serde_json::Value::String(billing_event)
                    }
                    Ok(_) => {
                        tracing::warn!(
                            quote_ref = %quote_ref(&quote.quote_id),
                            "provider billing event does not bind this quote/caller/provider — dropped from proof"
                        );
                        serde_json::Value::Null
                    }
                    Err(e) => {
                        tracing::warn!(
                            quote_ref = %quote_ref(&quote.quote_id),
                            error = %e,
                            "provider billing event failed verification — dropped from proof"
                        );
                        serde_json::Value::Null
                    }
                };
                // Sign the invocation binding: the provider's gate can
                // then require that the invoker IS the payer. A public-
                // only caller identity degrades to bearer mode.
                let capability = quote.capability.as_str();
                let tool = capability.split_once('/').map(|(_, t)| t).unwrap_or(capability);
                let binding_sig = self
                    .caller
                    .try_sign(&crate::engine::invocation_binding_transcript(
                        &quote.quote_id,
                        tool,
                    ))
                    .ok()
                    .map(|sig| sig.to_bytes().to_vec());
                CallerDecision::Paid {
                    quote_id,
                    binding_sig,
                    proof: serde_json::json!({
                        "quote_id": quote.quote_id,
                        "transaction": transaction,
                        "billing_event": verified_billing,
                    }),
                }
            }
            Ok(PayResponse::PendingTier { reached, required }) => CallerDecision::Failed {
                quote_id: Some(quote_id),
                message: format!(
                    "settled but confidence pending (reached {reached}, provider requires {required})"
                ),
                retryable: true,
            },
            Ok(PayResponse::InProgress) => CallerDecision::Failed {
                quote_id: Some(quote_id),
                message: "another attempt on this quote is in flight".to_string(),
                retryable: true,
            },
            Ok(PayResponse::Rejected { reason }) => {
                // A provider holding a self-contained bearer authorization
                // (exact/EIP-3009, exact/SPL) can claim "rejected" while
                // still settling it — its claim is not proof the money
                // stayed put. Keep the reservation for such schemes
                // (fail-closed accounting, as on transport ambiguity);
                // releasing it would reset the per-day counter every cycle
                // and defeat `max_per_day` as a loss bound.
                if reject_releases_reservation(&quote) {
                    self.release(&quote, self.clock.now_ns()).await;
                }
                CallerDecision::Denied {
                    policy_reason: format!("provider rejected the payment: {reason}"),
                }
            }
            Ok(PayResponse::Invalidated { reason }) => CallerDecision::Failed {
                quote_id: Some(quote_id),
                message: format!("payment invalidated: {reason}"),
                retryable: false,
            },
            Ok(PayResponse::Exception { kind }) => CallerDecision::Failed {
                quote_id: Some(quote_id),
                message: format!("verification exception ({kind}) — provider policy handles manually"),
                retryable: false,
            },
            Ok(PayResponse::Failure { retryable, message }) => {
                // Same bearer-authorization reasoning as `Rejected`: a
                // claimed failure from a provider that holds the signed
                // pull authorization is not proof of non-settlement.
                if reject_releases_reservation(&quote) {
                    self.release(&quote, self.clock.now_ns()).await;
                }
                CallerDecision::Failed { quote_id: Some(quote_id), message, retryable }
            }
            Err(e) => {
                // Transport ambiguity: the payment MAY have landed. Keep
                // the reservation (fail-closed accounting) and retry the
                // same quote — the provider side is idempotent.
                CallerDecision::Failed { quote_id: Some(quote_id), message: e.message, retryable: e.retryable }
            }
        }
    }

    /// Can this caller author a payment for these requirements? The
    /// mock network always; a real network's `exact` entry when its
    /// CAIP-2 namespace (`eip155`, `solana`) has a registered signer.
    fn can_settle(&self, requirements: &PaymentRequirements) -> bool {
        if requirements.network.starts_with("mock:") {
            return true;
        }
        let namespace = requirements.network.split(':').next().unwrap_or_default();
        requirements.scheme == "exact"
            && (namespace == "eip155" || OPAQUE_BLOB_NAMESPACES.contains(&namespace))
            && self.signers.contains_key(namespace)
    }

    /// Release the spend reservation after a terminal failure where value
    /// verifiably did not move. Best-effort: a release failure only
    /// over-counts the day budget (fail-closed direction).
    async fn release(&self, quote: &PaymentQuote, now_ns: u64) {
        if let Err(e) = self.spend.release_reservation(quote, now_ns).await {
            tracing::warn!(quote_ref = %quote_ref(&quote.quote_id), error = %e, "spend reservation release failed");
        }
    }
}

/// Whether a provider's *claimed* rejection/failure is trustworthy enough
/// to release the caller's spend reservation. Only the chainless mock
/// test scheme qualifies. Every real scheme here authors a self-contained
/// bearer pull authorization (exact/EIP-3009, exact/SPL) the counterparty
/// could settle on-chain regardless of what it reports back, so a claimed
/// non-settlement is not proof — the reservation must stand (fail-closed),
/// exactly as on transport ambiguity. Releasing it would let a lying
/// provider settle while resetting the per-day counter, defeating
/// `max_per_day` as the wallet's loss bound.
pub(crate) fn reject_releases_reservation(quote: &PaymentQuote) -> bool {
    quote.requirements.view().network.starts_with("mock:")
}

/// The set of CAIP-2 namespaces whose `exact` payload is an **opaque
/// wallet blob** authored the same way on every path: derive the typed
/// intent from the quoted requirements, hand it to the wallet, wrap the
/// returned blob in the pinned payload object. eip155 (EIP-712 typed data)
/// and the mock scheme author differently and are *not* here.
pub(crate) const OPAQUE_BLOB_NAMESPACES: [&str; 2] = ["solana", "xrpl"];

/// Author an opaque-blob exact-scheme payload (`solana`, `xrpl`) from the
/// quoted requirements. Shared by the mesh flow ([`CallerPaymentFlow`]) and
/// the outbound HTTP-402 door (`X402HttpFlow`) so the two dispatch sites
/// cannot drift — the seam inventory's "both `can_settle` arms, kept
/// symmetric" rule made structural instead of maintained by comment. The
/// wallet owns the key, the chain-specific serialization, and the
/// nonce/blockhash/`Sequence` bookkeeping; this only builds documents.
///
/// Retry honesty differs only in *consequence*, not code: the mesh flow's
/// provider-side idempotency keys on the quote (a re-signed SPL blob binds
/// a fresh blockhash; an xrpl retry must re-present the identical blob),
/// while the HTTP door has no provider idempotency (one `fetch_paid` = one
/// attempt). Both are documented at their call sites.
pub(crate) async fn author_opaque_blob_payload(
    namespace: &str,
    requirements: &PaymentRequirements,
    signer: &std::sync::Arc<dyn signer::SchemeSigner>,
) -> Result<serde_json::Value, String> {
    match namespace {
        "solana" => {
            let intent = crate::x402::schemes::exact_svm::transfer_intent(requirements)
                .map_err(|e| e.to_string())?;
            let transaction = signer
                .sign_svm_transfer(&intent)
                .await
                .map_err(|e| e.to_string())?;
            crate::x402::schemes::exact_svm::payload_object(&transaction).map_err(|e| e.to_string())
        }
        "xrpl" => {
            let intent = crate::x402::schemes::exact_xrpl::payment_intent(requirements)
                .map_err(|e| e.to_string())?;
            let blob = signer
                .sign_xrpl_payment(&intent)
                .await
                .map_err(|e| e.to_string())?;
            crate::x402::schemes::exact_xrpl::payload_object(&blob).map_err(|e| e.to_string())
        }
        other => Err(format!(
            "no opaque-blob payload author for namespace `{other}`"
        )),
    }
}

/// The EIP-3009 authorization a quote implies for payer `from`: recipient
/// and value from the quoted requirements, the validity window from the
/// quote's authoritative timestamps (60s of pre-validity tolerance — no
/// global clock), and a nonce derived from the quote id so a same-quote
/// retry re-presents the identical authorization (idempotent at the
/// provider and at the token contract's replay guard) while distinct
/// quotes never collide.
pub fn exact_evm_authorization_for_quote(
    quote: &PaymentQuote,
    from: &str,
) -> crate::x402::schemes::exact_evm::ExactEvmAuthorization {
    let nonce = {
        let mut hasher = blake3::Hasher::new();
        hasher.update(b"net.payments.exact_evm.nonce@1");
        hasher.update(quote.quote_id.as_bytes());
        hasher.update(from.as_bytes());
        format!("0x{}", hex::encode(hasher.finalize().as_bytes()))
    };
    let requirements = quote.requirements.view();
    let valid_after = (quote.issued_at_ns / 1_000_000_000).saturating_sub(60);
    // `validBefore` derives from the provider/server-controlled quote
    // expiry. Clamp the authorization lifetime so an abusively long expiry
    // cannot mint a long-lived single-use bearer authorization. A normal
    // quote (seconds to minutes) is well under the cap and unaffected; the
    // deterministic nonce already makes the authorization single-use
    // on-chain, so this is defense in depth on the time dimension.
    const MAX_AUTH_LIFETIME_SECS: u64 = 3600;
    let valid_before = (quote.expires_at_ns / 1_000_000_000)
        .min(valid_after.saturating_add(MAX_AUTH_LIFETIME_SECS));
    crate::x402::schemes::exact_evm::ExactEvmAuthorization {
        from: from.to_string(),
        to: requirements.pay_to.clone(),
        value: requirements.amount.clone(),
        valid_after,
        valid_before,
        nonce,
    }
}

#[cfg(test)]
mod reservation_release_tests {
    use super::*;
    use crate::core::canonical::canonical_bytes;
    use crate::core::registry::default_mock_registry;
    use crate::core::terms::PricingTerms;
    use crate::engine::AdmitAll;
    use crate::facilitator::mock::{MockFacilitator, MOCK_NETWORK, MOCK_SCHEME};
    use crate::policy::spend::SpendProfile;

    const NOW: u64 = 1_000_000_000_000_000;
    const CAPABILITY: &str = "prov/tool";

    struct FixedClock(u64);
    impl Clock for FixedClock {
        fn now_ns(&self) -> u64 {
            self.0
        }
    }

    fn requirements() -> X402Carry<PaymentRequirements> {
        X402Carry::author(&PaymentRequirements {
            scheme: MOCK_SCHEME.into(),
            network: MOCK_NETWORK.into(),
            amount: "2500".into(),
            asset: "musd".into(),
            pay_to: "mock-provider-settle-addr".into(),
            max_timeout_seconds: 60,
            extra: None,
        })
        .expect("author requirements")
    }

    /// A staged caller persists its authored payload and re-enters
    /// `pay_exact` with it. If what comes back does not decode, the
    /// attempt is terminal **before the wire** — no authorization
    /// reached the provider, so unlike a provider's claimed rejection
    /// this really is proof the money stayed put.
    ///
    /// The reservation has to go back. Nothing else will ever release
    /// it: the quote is dead, the caller is told `retryable: false`,
    /// and the amount would sit against `max_per_day` until retention
    /// ages the counter out days later — a wallet quietly losing budget
    /// to payments it never made.
    #[tokio::test]
    async fn a_payload_that_never_left_the_process_hands_the_reservation_back() {
        let dir = tempfile::tempdir().expect("tempdir");
        let provider_keys = Arc::new(EntityKeypair::generate());
        let registry = default_mock_registry(provider_keys.entity_id().clone());
        let engine = Arc::new(
            PaymentEngine::new(
                provider_keys.clone(),
                Arc::new(MockFacilitator::new()),
                Arc::new(AdmitAll),
                registry.clone(),
                dir.path().join("engine.json"),
            )
            .expect("engine"),
        );
        let clock: Arc<dyn Clock> = Arc::new(FixedClock(NOW));
        let terms = PricingTerms::new(
            provider_keys.entity_id().clone(),
            CAPABILITY,
            vec![requirements()],
            registry.reference().expect("registry ref"),
        );
        let terms_json =
            String::from_utf8(canonical_bytes(&terms).expect("canonical")).expect("utf8");
        // dev/test auto-allows the mock spend, so a reservation is
        // actually taken — which is the precondition under test.
        let flow = CallerPaymentFlow::new(
            Arc::new(EntityKeypair::generate()),
            SpendPolicyEngine::new(dir.path().join("spend-policy.json"), SpendProfile::DevTest),
            registry,
            Arc::new(InProcessProvider::new(engine, clock.clone())),
            clock,
        );

        let bound = flow
            .quote_bound(CAPABILITY, &terms_json, None)
            .await
            .expect("quote");
        assert!(matches!(
            flow.reserve_spend(&bound.quote).await.expect("reserve"),
            SpendDecision::Allowed
        ));
        assert!(
            flow.spend_reservation_held(&bound.quote.quote_id)
                .await
                .expect("read back"),
            "setup: the budget must actually be claimed"
        );

        let decision = flow
            .pay_exact(&bound.quote_bytes, b"{\"not\":\"an x402 payload\"}")
            .await;
        assert!(
            matches!(
                decision,
                CallerDecision::Failed {
                    retryable: false,
                    ..
                }
            ),
            "a corrupt stored payload is terminal: {decision:?}"
        );
        assert!(
            !flow
                .spend_reservation_held(&bound.quote.quote_id)
                .await
                .expect("read back"),
            "a payment that never left the process must not keep holding budget"
        );
    }
}

#[cfg(all(test, any(feature = "mesh", feature = "mcp-gate")))]
mod denial_render_tests {
    use super::*;
    use crate::engine::RedeemDenialReason as R;

    fn all_reasons() -> Vec<R> {
        vec![
            R::UnknownQuote,
            R::BindingMalformed,
            R::BindingRequired,
            R::BindingRejected,
            R::PayerRecordCorrupt,
            R::QuoteFrozen {
                // Freeze reasons are free-form strings (facilitator
                // invalidation reasons among them) — budget-test a fat one.
                freeze_reason: "settlement reported on `eip155:1` , quote is on `eip155:84532` \
                                — and a very long facilitator diagnostic follows"
                    .repeat(4),
            },
            R::NotSettled,
            R::SettlementPending,
            R::WrongToolBinding {
                capability: "some-provider/some-tool-with-a-longish-name".into(),
                tool_id: "another-tool-with-a-longish-name".into(),
            },
            R::AlreadyRedeemed,
            R::InputBindingMismatch,
        ]
    }

    /// Every redeem denial renders a schematic that fits the wire's
    /// header budget, carries the typed reason's wire token, states the
    /// invariant (`handler_executed: false`), and keeps the human
    /// message byte-identical to the reason's `Display`.
    #[test]
    fn every_redeem_denial_renders_within_the_header_budget() {
        let quote_id = "q_0123456789abcdef0123456789abcdef0123456789abcdef";
        for reason in all_reasons() {
            let denial = denial_for(&reason, "some-provider/some-tool", quote_id);
            assert_eq!(denial.message, reason.to_string());
            let s = &denial.schematic;
            assert_eq!(s.reason, reason.wire_reason());
            assert_eq!(s.stage, "redeem");
            assert!(!s.handler_executed);
            assert!(
                s.header_entry().is_some(),
                "`{}` must fit the wire budget",
                s.reason
            );
        }
        let engine = engine_unavailable_denial("some-tool", quote_id);
        assert_eq!(engine.message, "payment engine unavailable (fail-closed)");
        assert_eq!(engine.schematic.reason, "engine_unavailable");
        assert!(engine.schematic.header_entry().is_some());
    }

    /// The risk-table pin: security rows never advise a retry or a
    /// fresh quote — "do not just buy another quote and try again."
    #[test]
    fn security_rows_pin_no_retry_no_requote() {
        let rows = [
            R::BindingRejected,
            R::WrongToolBinding {
                capability: "p/a".into(),
                tool_id: "b".into(),
            },
            R::InputBindingMismatch,
        ];
        for reason in rows {
            let d = denial_for(&reason, "b", "q");
            assert_eq!(d.schematic.recovery.class, "security_violation");
            assert!(!d.schematic.retryable);
            assert!(!d.schematic.recovery.safe_to_retry);
            assert!(!d.schematic.recovery.safe_to_requote);
            assert!(d.schematic.recovery.next_action.is_none());
        }
    }

    /// The A2A row, pinned field by field against the contract the SDK's
    /// `FailureSchematic` doc table publishes: `input_binding_mismatch`
    /// is the `wrong_tool_binding` posture verbatim.
    ///
    /// It withholds the instrument facts deliberately. Telling a caller
    /// redeeming against the wrong purchase that funds *did* move would
    /// confirm that someone else's paid quote exists; `unknown` says only
    /// what this caller is entitled to know.
    #[test]
    fn input_binding_mismatch_renders_the_wrong_tool_binding_posture() {
        let d = denial_for(&R::InputBindingMismatch, "net.a2a.task/summarize", "q");
        let reference = denial_for(
            &R::WrongToolBinding {
                capability: "7/net.a2a.task/summarize".into(),
                tool_id: "net.a2a.task/summarize".into(),
            },
            "net.a2a.task/summarize",
            "q",
        );
        assert_eq!(d.schematic.reason, "input_binding_mismatch");
        assert_eq!(d.schematic.stage, "redeem");
        assert_eq!(d.schematic.recovery.class, "security_violation");
        assert_eq!(d.schematic.recovery.actor, "caller_operator");
        assert!(!d.schematic.retryable);
        assert!(!d.schematic.recovery.safe_to_retry);
        assert!(!d.schematic.recovery.safe_to_requote);
        assert!(d.schematic.recovery.next_action.is_none());
        assert_eq!(d.schematic.funds_moved, "unknown");
        assert_eq!(d.schematic.prior_payment, "unknown");
        // Same posture as the row it was grouped with, differing only in
        // the token and the human message.
        assert_eq!(d.schematic.recovery, reference.schematic.recovery);
        assert_eq!(d.schematic.funds_moved, reference.schematic.funds_moved);
        assert_eq!(d.schematic.prior_payment, reference.schematic.prior_payment);
        assert_ne!(d.schematic.reason, reference.schematic.reason);
    }

    /// `binding_required` is a caller-configuration row, not a security
    /// one.
    ///
    /// A caller that sent no possession proof to a provider that requires
    /// one has a misconfigured client; it is not evidence that anyone is
    /// attacking anything, and treating it as a security violation would
    /// bury the real ones. It is safe to retry once the client signs —
    /// but not safe to requote, because the quote was never the problem.
    #[test]
    fn binding_required_is_a_configuration_row_not_a_security_one() {
        let d = denial_for(&R::BindingRequired, "t", "q");
        assert_eq!(d.schematic.reason, "binding_required");
        assert_eq!(
            d.schematic.recovery.class, "caller_configuration_error",
            "a missing proof is a client gap, not an attack"
        );
        assert_eq!(d.schematic.recovery.actor, "caller_operator");
        assert!(
            !d.schematic.recovery.safe_to_requote,
            "a fresh quote does not fix an unsigned client"
        );
        // And it stays distinct from the security rows.
        let rejected = denial_for(&R::BindingRejected, "t", "q");
        assert_eq!(rejected.schematic.recovery.class, "security_violation");
        assert_ne!(d.schematic.reason, rejected.schematic.reason);
    }

    /// The review's split, rendered: an incomplete payment routes to
    /// "pay it, then retry"; a pending settlement routes to "wait and
    /// retry" — and the instrument fact differs (`none` vs `pending`).
    #[test]
    fn not_settled_and_settlement_pending_route_differently() {
        let unpaid = denial_for(&R::NotSettled, "t", "q");
        assert_eq!(unpaid.schematic.recovery.class, "payment_required");
        assert_eq!(unpaid.schematic.prior_payment, "none");
        let pending = denial_for(&R::SettlementPending, "t", "q");
        assert_eq!(pending.schematic.recovery.class, "automatic_retry");
        assert_eq!(pending.schematic.prior_payment, "pending");
        assert!(pending.schematic.retryable && pending.schematic.recovery.safe_to_retry);
    }

    /// Redaction: the free-form freeze reason (provider- and
    /// facilitator-supplied invalidation text) rides the human error
    /// body alone — the structured schematic carries a generic frozen
    /// message, never the free-form text, so nothing leaks onto the
    /// `net-failure-schematic` header.
    #[test]
    fn a_frozen_denial_keeps_the_free_form_reason_off_the_schematic() {
        let free_form =
            "settlement reported on `eip155:1`, quote on `eip155:84532` — facilitator diag 0xdeadbeef";
        let denial = denial_for(
            &R::QuoteFrozen {
                freeze_reason: free_form.to_string(),
            },
            "t",
            "q",
        );
        // The human body keeps the full reason (byte-identical to Display).
        assert_eq!(
            denial.message,
            format!("quote is frozen ({free_form}) — nothing serves against it")
        );
        assert!(denial.message.contains(free_form), "{}", denial.message);
        // The schematic scrubs it: generic message, no free-form text.
        assert_eq!(denial.schematic.reason, "quote_frozen");
        assert_eq!(
            denial.schematic.message,
            "quote is frozen — nothing serves against it"
        );
        assert!(!denial.schematic.message.contains("facilitator"));
        assert!(!denial.schematic.message.contains("eip155"));
    }

    /// The `next_action` column of the caller-facing mapping table
    /// (`FailureSchematic`'s doc) — pinned here so the table and
    /// `denial_for` cannot drift. Security and non-recoverable rows
    /// advise nothing (`None`).
    #[test]
    fn next_action_hints_match_the_mapping_table() {
        let na = |r: &R| denial_for(r, "t", "q").schematic.recovery.next_action;
        assert_eq!(na(&R::UnknownQuote).as_deref(), Some("request_new_quote"));
        assert_eq!(
            na(&R::BindingMalformed).as_deref(),
            Some("fix_payment_client")
        );
        // Missing a binding is a client gap, not an attack signal: it
        // routes to the same fix, and NOT to a requote — the quote is
        // fine, the client is not.
        assert_eq!(
            na(&R::BindingRequired).as_deref(),
            Some("fix_payment_client")
        );
        assert_eq!(na(&R::BindingRejected), None);
        assert_eq!(
            na(&R::PayerRecordCorrupt).as_deref(),
            Some("contact_provider_operator")
        );
        assert_eq!(
            na(&R::QuoteFrozen {
                freeze_reason: "x".into()
            }),
            None
        );
        assert_eq!(na(&R::NotSettled).as_deref(), Some("complete_payment"));
        assert_eq!(
            na(&R::SettlementPending).as_deref(),
            Some("retry_after_reverification")
        );
        assert_eq!(
            na(&R::WrongToolBinding {
                capability: "p/a".into(),
                tool_id: "b".into()
            }),
            None
        );
        assert_eq!(
            na(&R::AlreadyRedeemed).as_deref(),
            Some("request_new_quote")
        );
        assert_eq!(
            engine_unavailable_denial("t", "q")
                .schematic
                .recovery
                .next_action
                .as_deref(),
            Some("retry_later")
        );
    }
}
