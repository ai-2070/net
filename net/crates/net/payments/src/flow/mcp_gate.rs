//! The `net_mcp` gate seams, implemented over this crate's engine and
//! flow — the glue that lets `gated_invoke` clear paid capabilities
//! (caller side) and `WrapInvokeHandler` redeem them (provider side).
//!
//! Mapping only: every decision was made by [`CallerPaymentFlow`] / the
//! spend engine / [`PaymentEngine`]; these impls name the fields for the
//! adapter's vocabulary and nothing else (the same thinness rule
//! bindings follow).

use std::sync::Arc;

use async_trait::async_trait;
use net_mcp::serve::{CapabilityId, PaymentAdmission, PaymentFlow, PaymentFlowDecision};
use serde_json::Value;

use super::{CallerDecision, CallerPaymentFlow};
use crate::engine::{PaymentEngine, RedeemDecision};

#[async_trait]
impl PaymentFlow for CallerPaymentFlow {
    async fn pay(
        &self,
        id: &CapabilityId,
        pricing_terms: &str,
        _tool_args: &Value,
    ) -> PaymentFlowDecision {
        // P0 static pricing ignores the arguments (no input-bound quotes
        // until RFQ maps onto x402 v2 dynamic pricing).
        match self.run(&id.display(), pricing_terms).await {
            CallerDecision::Paid {
                quote_id,
                binding_sig,
                proof,
            } => PaymentFlowDecision::Paid {
                quote_id,
                binding_sig,
                proof,
            },
            CallerDecision::RequiresPaymentApproval {
                quote_id,
                policy_reason,
                approve_hint,
            } => PaymentFlowDecision::RequiresPaymentApproval {
                quote_id,
                policy_reason,
                approve_hint,
            },
            CallerDecision::Denied { policy_reason } => {
                PaymentFlowDecision::Denied { policy_reason }
            }
            CallerDecision::Failed { message, retryable } => {
                PaymentFlowDecision::Failed { message, retryable }
            }
        }
    }
}

/// The provider-side gate: `WrapInvokeHandler` redeems each paid
/// invoke's quote against the [`PaymentEngine`] through this adapter.
/// Wire it via `WrapConfig.payment_admission`.
pub struct EnginePaymentAdmission {
    engine: Arc<PaymentEngine>,
}

impl EnginePaymentAdmission {
    pub fn new(engine: Arc<PaymentEngine>) -> Self {
        Self { engine }
    }
}

#[async_trait]
impl PaymentAdmission for EnginePaymentAdmission {
    async fn redeem(
        &self,
        tool_id: &str,
        quote_id: &str,
        binding: Option<&[u8]>,
    ) -> Result<(), String> {
        match self
            .engine
            .redeem_for_invocation(tool_id, quote_id, binding)
            .await
        {
            Ok(RedeemDecision::Admitted) => Ok(()),
            Ok(RedeemDecision::Denied { reason }) => Err(reason),
            // Engine/store failure is fail-closed: never serve on an
            // unverifiable payment.
            Err(e) => Err(format!("payment engine unavailable (fail-closed): {e}")),
        }
    }
}
