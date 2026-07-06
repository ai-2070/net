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

use crate::core::quote::PaymentQuote;
use crate::core::registry::AssetRegistry;
use crate::core::terms::PricingTerms;
use crate::core::verification::VerificationTier;
use crate::engine::{PaymentDecision, PaymentEngine};
use crate::policy::spend::{SpendDecision, SpendPolicyEngine};
use crate::x402::payload::PaymentPayload;
use crate::x402::requirements::PaymentRequirements;
use crate::x402::{X402Carry, X402_VERSION};

#[cfg(feature = "mcp-gate")]
pub mod mcp_gate;
#[cfg(feature = "mesh")]
pub mod mesh;

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
    PendingTier { reached: String, required: String },
    Rejected { reason: String },
    Invalidated { reason: String },
    Exception { kind: String },
    InProgress,
    Failure { retryable: bool, message: String },
}

impl PayResponse {
    /// Project an engine decision onto the wire vocabulary.
    pub fn from_decision(decision: &PaymentDecision) -> Result<Self, crate::core::canonical::EnvelopeError> {
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
            PaymentDecision::Rejected { reason } => {
                PayResponse::Rejected { reason: reason.to_string() }
            }
            PaymentDecision::Invalidated { reason } => {
                PayResponse::Invalidated { reason: format!("{reason:?}") }
            }
            PaymentDecision::Exception { kind } => {
                PayResponse::Exception { kind: format!("{kind:?}") }
            }
            PaymentDecision::InProgress => PayResponse::InProgress,
            PaymentDecision::FacilitatorFailure { retryable, message, .. } => {
                PayResponse::Failure { retryable: *retryable, message: message.clone() }
            }
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
            .map_err(|e| ChannelError { message: e.to_string(), retryable: false })?;
        crate::core::canonical::canonical_bytes(&quote)
            .map_err(|e| ChannelError { message: e.to_string(), retryable: false })
    }

    async fn pay(
        &self,
        quote_bytes: &[u8],
        payload: &X402Carry<PaymentPayload>,
    ) -> Result<PayResponse, ChannelError> {
        let quote = PaymentQuote::from_json_bytes(quote_bytes)
            .map_err(|e| ChannelError { message: e.to_string(), retryable: false })?;
        let decision = self
            .engine
            .accept_payment(&quote, payload, self.required_tier, self.clock.now_ns())
            .await
            .map_err(|e| ChannelError { message: e.to_string(), retryable: false })?;
        PayResponse::from_decision(&decision)
            .map_err(|e| ChannelError { message: e.to_string(), retryable: false })
    }
}

/// The flow's structured outcome — payments-native; the `mcp-gate`
/// feature maps it 1:1 onto `net_mcp::serve::PaymentFlowDecision`.
#[derive(Debug, Clone)]
pub enum CallerDecision {
    /// Payment cleared: `quote_id` is the redemption binding the
    /// invocation must carry to the provider's gate; `proof` is the
    /// full payment context (settlement refs, the signed billing event).
    Paid { quote_id: String, proof: serde_json::Value },
    RequiresPaymentApproval {
        quote_id: String,
        policy_reason: String,
        approve_hint: String,
    },
    Denied { policy_reason: String },
    Failed { message: String, retryable: bool },
}

/// The caller-side payment flow: one per caller identity + policy store.
pub struct CallerPaymentFlow {
    caller: Arc<EntityKeypair>,
    spend: SpendPolicyEngine,
    registry: AssetRegistry,
    provider: Arc<dyn ProviderChannel>,
    clock: Arc<dyn Clock>,
}

impl CallerPaymentFlow {
    pub fn new(
        caller: Arc<EntityKeypair>,
        spend: SpendPolicyEngine,
        registry: AssetRegistry,
        provider: Arc<dyn ProviderChannel>,
        clock: Arc<dyn Clock>,
    ) -> Self {
        Self { caller, spend, registry, provider, clock }
    }

    /// Run the paid lifecycle for `capability` (display form,
    /// `provider/capability`) against its announced `pricing_terms`.
    pub async fn run(&self, capability: &str, pricing_terms: &str) -> CallerDecision {
        // -- [1] parse the announced terms; pick an accepts[] entry the
        //    caller can settle: P0 is mock-only, so the first mock-network
        //    template. Real networks are P1 — their entries are skipped,
        //    and terms offering nothing else are denied outright.
        let terms = match PricingTerms::from_json_bytes(pricing_terms.as_bytes()) {
            Ok(t) => t,
            Err(e) => {
                return CallerDecision::Denied {
                    policy_reason: format!("announced pricing terms are invalid: {e}"),
                }
            }
        };
        let Some(template) = terms
            .accepts
            .iter()
            .find(|t| t.view().network.starts_with("mock:"))
        else {
            return CallerDecision::Denied {
                policy_reason: "no mock-network accepts[] entry; real-network settlement is P1"
                    .to_string(),
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
                match self.provider.quote(self.caller.entity_id(), capability, template).await {
                    Ok(b) => b,
                    Err(e) => {
                        return CallerDecision::Failed { message: e.message, retryable: e.retryable }
                    }
                }
            }
            Err(e) => return CallerDecision::Failed { message: e.to_string(), retryable: false },
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
        match self.spend.check_and_reserve(&quote, &self.registry, now_ns).await {
            Ok(SpendDecision::Allowed) => {}
            Ok(SpendDecision::RequiresPaymentApproval { quote_id, policy_reason, approve_hint }) => {
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
                return CallerDecision::Failed { message: e.to_string(), retryable: false }
            }
        }

        // -- [4] author the x402 payload (mock scheme). The nonce derives
        //    from the quote id, so a same-quote retry reuses the same
        //    payload (idempotent) while distinct quotes never collide.
        let payload = match self.author_payload(&quote) {
            Ok(p) => p,
            Err(message) => {
                self.release(&quote, now_ns).await;
                return CallerDecision::Failed { message, retryable: false };
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
                CallerDecision::Paid {
                    quote_id: quote.quote_id.clone(),
                    proof: serde_json::json!({
                        "quote_id": quote.quote_id,
                        "transaction": transaction,
                        "billing_event": billing_event,
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
                self.release(&quote, now_ns).await;
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
                self.release(&quote, now_ns).await;
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

    fn author_payload(&self, quote: &PaymentQuote) -> Result<X402Carry<PaymentPayload>, String> {
        let nonce = {
            let mut hasher = blake3::Hasher::new();
            hasher.update(b"net.payments.mock.payload_nonce@1");
            hasher.update(quote.quote_id.as_bytes());
            hasher.update(self.caller.entity_id().as_bytes());
            hex::encode(hasher.finalize().as_bytes())
        };
        X402Carry::author(&PaymentPayload {
            x402_version: X402_VERSION,
            resource: None,
            accepted: quote.requirements.view().clone(),
            payload: serde_json::json!({
                "mock_authorization": hex::encode(self.caller.entity_id().as_bytes()),
                "nonce": nonce,
            }),
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
