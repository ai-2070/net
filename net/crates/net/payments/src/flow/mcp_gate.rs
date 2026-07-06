//! `net_mcp::serve::PaymentFlow` implementation for the caller flow —
//! the glue that lets `gated_invoke` clear paid capabilities through
//! this crate's spend engine and lifecycle.
//!
//! Mapping only: every decision was made by [`CallerPaymentFlow`] and
//! the spend engine; this impl names the fields for the adapter's
//! vocabulary and nothing else (the same thinness rule bindings follow).

use async_trait::async_trait;
use net_mcp::serve::{CapabilityId, PaymentFlow, PaymentFlowDecision};
use serde_json::Value;

use super::{CallerDecision, CallerPaymentFlow};

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
            CallerDecision::Paid { proof } => PaymentFlowDecision::Paid { proof },
            CallerDecision::RequiresPaymentApproval { quote_id, policy_reason, approve_hint } => {
                PaymentFlowDecision::RequiresPaymentApproval {
                    quote_id,
                    policy_reason,
                    approve_hint,
                }
            }
            CallerDecision::Denied { policy_reason } => {
                PaymentFlowDecision::Denied { policy_reason }
            }
            CallerDecision::Failed { message, retryable } => {
                PaymentFlowDecision::Failed { message, retryable }
            }
        }
    }
}
