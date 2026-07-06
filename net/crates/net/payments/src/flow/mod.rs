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

#[cfg(feature = "http-facilitator")]
pub mod http402;
#[cfg(feature = "mcp-gate")]
pub mod mcp_gate;
#[cfg(feature = "mesh")]
pub mod mesh;
pub mod signer;

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

        // -- [4] author the x402 payload for the quoted scheme. Nonces
        //    derive from the quote id, so a same-quote retry reuses the
        //    same payload (idempotent) while distinct quotes never
        //    collide.
        let payload = match self.author_payload(&quote).await {
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

    /// Can this caller author a payment for these requirements? The
    /// mock network always; a real network's `exact` entry when its
    /// CAIP-2 namespace (`eip155`, `solana`) has a registered signer.
    fn can_settle(&self, requirements: &PaymentRequirements) -> bool {
        if requirements.network.starts_with("mock:") {
            return true;
        }
        let namespace = requirements.network.split(':').next().unwrap_or_default();
        requirements.scheme == "exact"
            && matches!(namespace, "eip155" | "solana")
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
        } else if self.can_settle(requirements)
            && requirements.network.starts_with("eip155:")
        {
            // exact / eip155: EIP-3009 typed data through the signer.
            let signer = self
                .signers
                .get("eip155")
                .ok_or_else(|| "no eip155 signer configured".to_string())?;
            let auth = exact_evm_authorization_for_quote(quote, &signer.address());
            let typed = crate::x402::schemes::exact_evm::typed_data(requirements, &auth)
                .map_err(|e| e.to_string())?;
            let signature =
                signer.sign_typed_data(&typed).await.map_err(|e| e.to_string())?;
            crate::x402::schemes::exact_evm::payload_object(&auth, &signature)
        } else if self.can_settle(requirements)
            && requirements.network.starts_with("solana:")
        {
            // exact / solana: the wallet authors a partially-signed SPL
            // transfer for the intent derived from the quoted
            // requirements. Retry honesty: the wallet may bind a fresh
            // blockhash, so same-quote retries can produce *different*
            // payload bytes — idempotency holds at the quote (a served
            // quote returns its original billing event), not at
            // payload byte-identity as on eip155.
            let signer = self
                .signers
                .get("solana")
                .ok_or_else(|| "no solana signer configured".to_string())?;
            let intent = crate::x402::schemes::exact_svm::transfer_intent(requirements)
                .map_err(|e| e.to_string())?;
            let transaction =
                signer.sign_svm_transfer(&intent).await.map_err(|e| e.to_string())?;
            crate::x402::schemes::exact_svm::payload_object(&transaction)
                .map_err(|e| e.to_string())?
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
    crate::x402::schemes::exact_evm::ExactEvmAuthorization {
        from: from.to_string(),
        to: requirements.pay_to.clone(),
        value: requirements.amount.clone(),
        valid_after: (quote.issued_at_ns / 1_000_000_000).saturating_sub(60),
        valid_before: quote.expires_at_ns / 1_000_000_000,
        nonce,
    }
}
