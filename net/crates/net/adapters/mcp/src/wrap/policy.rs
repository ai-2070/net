//! Invoke-path policy hook (V2 Phase 2 — "the enforcement hook ships anyway").
//!
//! This is the single in-root **toll booth**: a policy check that runs on the
//! *provider* after admission (owner-scope / delegation) and before the tool
//! executes. It exists as a real code path from day one — [`WrapInvokeHandler`]
//! calls it on every invoke it admits — so turning on allowlists, category
//! grants, or a provider-side approval flow later is **flipping a default**
//! (passing a different [`InvokePolicy`] in [`WrapConfig`]), never adding a
//! call site.
//!
//! The default preset is allow-all for an admitted (same-root) caller — the
//! mesh adds *reach*, not *authority*, so an in-root call is treated exactly as
//! a local one would be. The concrete policies (an allowlist; a dangerous-tool
//! approval that routes to the operator surface and fails closed with
//! `approval_unreachable`) plug in here without touching the invoke plumbing.
//!
//! [`WrapInvokeHandler`]: super::invoke::WrapInvokeHandler
//! [`WrapConfig`]: super::session::WrapConfig

/// What the invoke path knows about a call when it consults the policy. Owned
/// (cheap to build per invoke) so the async policy future has no borrow to
/// outlive.
#[derive(Debug, Clone)]
pub struct PolicyContext {
    /// The served tool id (the nRPC service name the caller invoked).
    pub tool_id: String,
    /// The AEAD-verified caller origin that was admitted.
    pub caller_origin: u64,
    /// Whether admission used a verified delegation chain (vs. the owner-scope
    /// origin allowlist). A policy that gates on *who* delegated can branch on
    /// this; the default preset ignores it.
    pub delegated: bool,
}

/// A policy's verdict for one invoke.
#[derive(Debug, Clone)]
pub enum PolicyDecision {
    /// Let the invoke proceed.
    Allow,
    /// Refuse the invoke; `reason` is surfaced to the caller as the policy
    /// rejection (mapped to a `denied` verdict on the demand side, like the
    /// owner-scope / delegation rejections — an authorization answer, not a
    /// tool bug).
    Deny {
        /// Why the invoke was refused.
        reason: String,
    },
}

impl PolicyDecision {
    /// Deny with a reason string, sugar for the common case.
    pub fn deny(reason: impl Into<String>) -> Self {
        PolicyDecision::Deny {
            reason: reason.into(),
        }
    }
}

/// The provider-side invoke policy. Implementors decide, per admitted call,
/// whether the tool may run. `Send + Sync` so an `Arc<dyn InvokePolicy>` rides
/// on a served handler across threads.
#[async_trait::async_trait]
pub trait InvokePolicy: Send + Sync {
    /// Decide whether the admitted invoke described by `ctx` may proceed.
    async fn check(&self, ctx: &PolicyContext) -> PolicyDecision;
}

/// The default preset: allow every admitted (same-root) caller. In-root, the
/// mesh adds reach, not authority — a call that passed admission is treated as
/// a local one. Passing this explicitly documents the preset; leaving
/// [`WrapConfig`](super::session::WrapConfig)'s policy unset is equivalent (the
/// invoke path simply skips the check).
pub struct AllowAllPolicy;

#[async_trait::async_trait]
impl InvokePolicy for AllowAllPolicy {
    async fn check(&self, _ctx: &PolicyContext) -> PolicyDecision {
        PolicyDecision::Allow
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx() -> PolicyContext {
        PolicyContext {
            tool_id: "echo".to_string(),
            caller_origin: 7,
            delegated: false,
        }
    }

    #[tokio::test]
    async fn allow_all_preset_admits_every_call() {
        assert!(matches!(
            AllowAllPolicy.check(&ctx()).await,
            PolicyDecision::Allow
        ));
    }

    #[test]
    fn deny_sugar_carries_the_reason() {
        match PolicyDecision::deny("nope") {
            PolicyDecision::Deny { reason } => assert_eq!(reason, "nope"),
            other => panic!("expected Deny, got {other:?}"),
        }
    }
}
