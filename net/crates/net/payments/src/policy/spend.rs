//! Spend-policy vocabulary — Workstream 3 lands the decision engine here.
//!
//! Defaults are doctrine: **real networks deny** (the gate exists even
//! though P0 is mock-only); the mock network auto-allows **only** under a
//! dev/test profile or an explicit unsafe flag — demos must not train the
//! policy path wrong. Displaying a price never implies authorization to
//! spend it.

use serde::{Deserialize, Serialize};

use crate::core::units::AtomicAmount;

/// Per-scope spend limits. All amounts are atomic units of a specific
/// allowed asset; cross-asset budgets are out of scope for P0 (one mock
/// asset exists).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpendLimits {
    /// Deny any single call above this.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_per_call: Option<AtomicAmount>,
    /// Deny once the rolling per-day total would exceed this. The counter
    /// is a lock-held RMW on the shared store (v1-honest: coarse and
    /// correct beats clever and racy).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_per_day: Option<AtomicAmount>,
    /// CAIP-2 networks spending is allowed on. Empty = deny all networks
    /// (the fail-closed default).
    #[serde(default)]
    pub allowed_networks: Vec<String>,
    /// CAIP-19 asset ids spending is allowed in. Empty = deny all assets.
    #[serde(default)]
    pub allowed_assets: Vec<String>,
}

impl Default for SpendLimits {
    /// The fail-closed default: nothing is allowed anywhere.
    fn default() -> Self {
        Self {
            max_per_call: None,
            max_per_day: None,
            allowed_networks: Vec::new(),
            allowed_assets: Vec::new(),
        }
    }
}

/// The structured caller-side gate outcome, mirroring the consent shape.
/// The gateway surfaces this as `{status: "requires_payment_approval",
/// quote, policy_reason, approve_hint}` — same contract as
/// `requires_approval`, resolved through the SDK consent API.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SpendDecision {
    /// Policy admits the spend silently.
    Allowed,
    /// Policy wants a human: the quote, why, and how to approve.
    RequiresPaymentApproval {
        quote_id: String,
        policy_reason: String,
        approve_hint: String,
    },
    /// Policy denies outright (no approval path — e.g. a real network in
    /// P0).
    Denied { policy_reason: String },
}
