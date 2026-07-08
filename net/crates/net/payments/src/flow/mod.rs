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
use crate::policy::spend::{SpendDecision, SpendPolicyEngine};
use crate::x402::payload::PaymentPayload;
use crate::x402::requirements::PaymentRequirements;
use crate::x402::{X402Carry, X402_VERSION};

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
        // Security rows advise nothing: do not retry, do not just buy
        // another quote — report the mismatch.
        R::BindingRejected | R::WrongToolBinding { .. } => (
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
    let message = reason.to_string();
    net_sdk::tool_payment::GateDenial {
        schematic: net_sdk::tool_payment::FailureSchematic {
            object: net_sdk::tool_payment::TAG_PAYMENT_FAILURE.to_string(),
            code: v::CODE_PAYMENT.to_string(),
            stage: v::STAGE_REDEEM.to_string(),
            reason: reason.wire_reason().to_string(),
            message: net_sdk::tool_payment::FailureSchematic::cap_message(&message),
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
#[cfg(any(feature = "mesh", feature = "mcp-gate"))]
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
        Ok(RedeemDecision::Admitted) => Ok(()),
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
    async fn quote(
        &self,
        caller: &EntityId,
        capability: &str,
        template: &X402Carry<PaymentRequirements>,
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
}

#[async_trait::async_trait]
impl ProviderChannel for InProcessProvider {
    async fn quote(
        &self,
        caller: &EntityId,
        capability: &str,
        template: &X402Carry<PaymentRequirements>,
    ) -> Result<Vec<u8>, ChannelError> {
        // Provider policy runs inside issue_quote — never quote a caller
        // you'd deny.
        let quote = self
            .engine
            .issue_quote(
                caller.clone(),
                capability,
                template.clone(),
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
        message: String,
        retryable: bool,
    },
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
    pub async fn run(&self, capability: &str, pricing_terms: &str) -> CallerDecision {
        // -- [1] parse the announced terms; pick the first accepts[]
        //    entry this caller can *settle*: the mock network always, a
        //    real network only when its namespace has a configured
        //    settlement signer. (Whether policy *permits* the spend is
        //    [3]'s job — settleability is about capability, not
        //    authorization.)
        let terms = match PricingTerms::from_json_bytes(pricing_terms.as_bytes()) {
            Ok(t) => t,
            Err(e) => {
                return CallerDecision::Denied {
                    policy_reason: format!("announced pricing terms are invalid: {e}"),
                }
            }
        };
        let Some(template) = terms.accepts.iter().find(|t| self.can_settle(t.view())) else {
            let offered: Vec<String> = terms
                .accepts
                .iter()
                .map(|t| format!("({}, {})", t.view().scheme, t.view().network))
                .collect();
            return CallerDecision::Denied {
                policy_reason: format!(
                    "no settleable accepts[] entry: terms offer {offered:?}; this caller \
                     settles mock:* always and exact/eip155 when a signer is configured"
                ),
            };
        };

        // -- [2] the quote: an approved held quote first (the human's
        //    approval applies to the exact quote they saw — this is the
        //    retry-after-approval path), else a fresh provider-signed one.
        let mut redeeming_approval: Option<String> = None;
        let quote_bytes = match self.spend.approved_quote(capability).await {
            Ok(Some((held_id, held_bytes))) => {
                match PaymentQuote::from_json_bytes(&held_bytes) {
                    Ok(held) if !held.is_expired_at(self.clock.now_ns()) => {
                        redeeming_approval = Some(held_id);
                        held_bytes
                    }
                    // Expired or unparseable hold: drop it and fall
                    // through to a fresh quote (which will hold again if
                    // policy still objects — a new approval for a new
                    // quote, never a silent carry-over).
                    _ => {
                        let _ = self.spend.clear_approval(&held_id).await;
                        match self
                            .provider
                            .quote(self.caller.entity_id(), capability, template)
                            .await
                        {
                            Ok(b) => b,
                            Err(e) => {
                                return CallerDecision::Failed {
                                    message: e.message,
                                    retryable: e.retryable,
                                }
                            }
                        }
                    }
                }
            }
            Ok(None) => {
                match self
                    .provider
                    .quote(self.caller.entity_id(), capability, template)
                    .await
                {
                    Ok(b) => b,
                    Err(e) => {
                        return CallerDecision::Failed {
                            message: e.message,
                            retryable: e.retryable,
                        }
                    }
                }
            }
            Err(e) => {
                return CallerDecision::Failed {
                    message: e.to_string(),
                    retryable: false,
                }
            }
        };
        let quote = match PaymentQuote::from_json_bytes(&quote_bytes) {
            Ok(q) => q,
            Err(e) => {
                return CallerDecision::Denied {
                    policy_reason: format!("provider quote failed verification: {e}"),
                }
            }
        };
        if quote.caller != *self.caller.entity_id() {
            return CallerDecision::Denied {
                policy_reason: "quote was issued to a different caller".to_string(),
            };
        }
        if quote.capability != capability {
            return CallerDecision::Denied {
                policy_reason: "quote binds a different capability".to_string(),
            };
        }
        if quote.provider != terms.provider {
            return CallerDecision::Denied {
                policy_reason: "quote provider does not match the announced terms provider"
                    .to_string(),
            };
        }
        if quote.requirements.bytes() != template.bytes() {
            return CallerDecision::Denied {
                policy_reason: "quote deviates from the announced terms — never pay more \
                                than discovery showed"
                    .to_string(),
            };
        }
        let now_ns = self.clock.now_ns();
        if quote.is_expired_at(now_ns) {
            return CallerDecision::Failed {
                message: "provider quote arrived already expired".to_string(),
                retryable: true,
            };
        }

        // -- [3] caller spend policy: check + reserve, one locked RMW.
        match self
            .spend
            .check_and_reserve(&quote, &self.registry, now_ns)
            .await
        {
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
                    message: e.to_string(),
                    retryable: false,
                }
            }
        }

        // -- [4] author the x402 payload for the quoted scheme. Nonces
        //    derive from the quote id, so a same-quote retry reuses the
        //    same payload (idempotent) while distinct quotes never
        //    collide.
        let payload = match self.author_payload(&quote).await {
            Ok(p) => p,
            Err(message) => {
                self.release(&quote, now_ns).await;
                return CallerDecision::Failed {
                    message,
                    retryable: false,
                };
            }
        };

        // -- [5] deliver; map the provider's decision. Terminal failures
        //    release the spend reservation — the money never moved.
        match self.provider.pay(&quote_bytes, &payload).await {
            Ok(PayResponse::Served { billing_event, transaction }) => {
                // A redeemed approval is consumed by the successful pay.
                if let Some(held_id) = redeeming_approval {
                    let _ = self.spend.clear_approval(&held_id).await;
                }
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
                            quote = %quote.quote_id,
                            "provider billing event does not bind this quote/caller/provider — dropped from proof"
                        );
                        serde_json::Value::Null
                    }
                    Err(e) => {
                        tracing::warn!(
                            quote = %quote.quote_id,
                            error = %e,
                            "provider billing event failed verification — dropped from proof"
                        );
                        serde_json::Value::Null
                    }
                };
                // Sign the invocation binding: the provider's gate can
                // then require that the invoker IS the payer. A public-
                // only caller identity degrades to bearer mode.
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
                    quote_id: quote.quote_id.clone(),
                    binding_sig,
                    proof: serde_json::json!({
                        "quote_id": quote.quote_id,
                        "transaction": transaction,
                        "billing_event": verified_billing,
                    }),
                }
            }
            Ok(PayResponse::PendingTier { reached, required }) => CallerDecision::Failed {
                message: format!(
                    "settled but confidence pending (reached {reached}, provider requires {required})"
                ),
                retryable: true,
            },
            Ok(PayResponse::InProgress) => CallerDecision::Failed {
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
                    self.release(&quote, now_ns).await;
                }
                CallerDecision::Denied {
                    policy_reason: format!("provider rejected the payment: {reason}"),
                }
            }
            Ok(PayResponse::Invalidated { reason }) => CallerDecision::Failed {
                message: format!("payment invalidated: {reason}"),
                retryable: false,
            },
            Ok(PayResponse::Exception { kind }) => CallerDecision::Failed {
                message: format!("verification exception ({kind}) — provider policy handles manually"),
                retryable: false,
            },
            Ok(PayResponse::Failure { retryable, message }) => {
                // Same bearer-authorization reasoning as `Rejected`: a
                // claimed failure from a provider that holds the signed
                // pull authorization is not proof of non-settlement.
                if reject_releases_reservation(&quote) {
                    self.release(&quote, now_ns).await;
                }
                CallerDecision::Failed { message, retryable }
            }
            Err(e) => {
                // Transport ambiguity: the payment MAY have landed. Keep
                // the reservation (fail-closed accounting) and retry the
                // same quote — the provider side is idempotent.
                CallerDecision::Failed { message: e.message, retryable: e.retryable }
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

    /// Author the scheme payload for a quote. Dispatches on the quoted
    /// scheme/network; the selection guard ([`Self::can_settle`]) makes
    /// the fall-through unreachable in practice, and it fails closed
    /// anyway.
    async fn author_payload(
        &self,
        quote: &PaymentQuote,
    ) -> Result<X402Carry<PaymentPayload>, String> {
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

    /// Release the spend reservation after a terminal failure where value
    /// verifiably did not move. Best-effort: a release failure only
    /// over-counts the day budget (fail-closed direction).
    async fn release(&self, quote: &PaymentQuote, now_ns: u64) {
        if let Err(e) = self.spend.release_reservation(quote, now_ns).await {
            tracing::warn!(quote = %quote.quote_id, error = %e, "spend reservation release failed");
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

#[cfg(all(test, any(feature = "mesh", feature = "mcp-gate")))]
mod denial_render_tests {
    use super::*;
    use crate::engine::RedeemDenialReason as R;

    fn all_reasons() -> Vec<R> {
        vec![
            R::UnknownQuote,
            R::BindingMalformed,
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
}
