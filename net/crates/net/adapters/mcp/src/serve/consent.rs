//! Shim-side consent gate (`MCP_BRIDGE_PLAN.md` Phase 2, `serve/consent.rs`).
//!
//! A capability with credential status `credentialed`, `external_api`, or
//! `unknown` is **not invocable** through `net_invoke_capability` until it is
//! either allowlisted in shim config or pinned with user approval. Search and
//! describe still show it — display never implies invocation (doctrine #3) —
//! marked `requires_approval` so the model knows the next step.
//!
//! This is *local client consent*, not remote authorization (Phase 3): an
//! approval here satisfies the shim for this user profile on this machine and
//! nothing wider; the remote wrapper's owner scope always wins on top. The
//! gating rule itself is the single one shared with the supply side —
//! [`CredentialStatus::requires_consent`] — so wrap and serve can never drift
//! on what counts as "spicy".

use std::collections::HashSet;

use crate::serve::backend::CapabilityId;
use crate::wrap::credentials::CredentialStatus;

/// Whether a capability may be invoked right now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConsentDecision {
    /// Freely invocable — either it carries no credentials, or the operator
    /// has allowlisted / pinned it.
    Allowed,
    /// Blocked pending local approval (allowlist entry or an approved pin).
    RequiresApproval,
}

impl ConsentDecision {
    /// Is this capability blocked pending approval?
    pub fn requires_approval(self) -> bool {
        matches!(self, ConsentDecision::RequiresApproval)
    }
}

/// The shim's consent state: the config allowlist plus the set of pinned
/// capabilities. Both admit an otherwise-gated capability; the distinction
/// (config vs. user-approved pin) matters for Phase 3, not for the gate here.
#[derive(Debug, Clone, Default)]
pub struct ConsentPolicy {
    /// Capabilities allowlisted in shim config — pre-approved by the operator.
    allowlist: HashSet<CapabilityId>,
    /// Capabilities with an approved pin (Phase 3). Persisted daemon-side in
    /// the real build; held here for the shim's lifetime.
    pinned: HashSet<CapabilityId>,
}

impl ConsentPolicy {
    /// An empty policy: nothing allowlisted or pinned. Only capabilities that
    /// need no approval (`credential_status: none`) are invocable.
    pub fn new() -> Self {
        Self::default()
    }

    /// Allowlist `id` (from shim config) — a standing pre-approval.
    pub fn allow(&mut self, id: CapabilityId) {
        self.allowlist.insert(id);
    }

    /// Record an approved pin for `id` (Phase 3 pin flow).
    pub fn pin(&mut self, id: CapabilityId) {
        self.pinned.insert(id);
    }

    /// Remove a pin.
    pub fn unpin(&mut self, id: &CapabilityId) {
        self.pinned.remove(id);
    }

    /// Is `id` pinned?
    pub fn is_pinned(&self, id: &CapabilityId) -> bool {
        self.pinned.contains(id)
    }

    /// The pinned capabilities, for `net_list_pinned_capabilities`.
    pub fn pinned(&self) -> impl Iterator<Item = &CapabilityId> {
        self.pinned.iter()
    }

    /// Decide whether `id`, with the given wire credential status, may be
    /// invoked. Every wire status is gated ([`CredentialStatus::from_wire`]
    /// never trusts a wire value to the ungated `None` — see its trust-boundary
    /// note), so a discovered capability is invocable only when the operator
    /// has allowlisted or pinned it.
    pub fn decide(&self, id: &CapabilityId, credential_status: &str) -> ConsentDecision {
        // Kept for robustness: `from_wire` never yields a non-consent status
        // today, so this branch does not fire for any wire value — but it keeps
        // the gate honest if a trusted-status path is ever added.
        if !CredentialStatus::from_wire(credential_status).requires_consent() {
            return ConsentDecision::Allowed;
        }
        if self.allowlist.contains(id) || self.pinned.contains(id) {
            return ConsentDecision::Allowed;
        }
        ConsentDecision::RequiresApproval
    }

    /// Convenience: does invoking `id` (with `credential_status`) require
    /// approval the operator has not granted?
    pub fn requires_approval(&self, id: &CapabilityId, credential_status: &str) -> bool {
        self.decide(id, credential_status).requires_approval()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cap(s: &str) -> CapabilityId {
        CapabilityId::parse(s).unwrap()
    }

    #[test]
    fn a_wire_none_is_gated_not_trusted() {
        // A discovered capability's self-declared `none` is not trusted across
        // the demand-side trust boundary — it is gated like any other status.
        let policy = ConsentPolicy::new();
        assert_eq!(
            policy.decide(&cap("b/echo"), "none"),
            ConsentDecision::RequiresApproval,
        );
    }

    #[test]
    fn spicy_statuses_require_approval_by_default() {
        let policy = ConsentPolicy::new();
        for status in ["credentialed", "external_api", "unknown", "none"] {
            assert_eq!(
                policy.decide(&cap("b/tool"), status),
                ConsentDecision::RequiresApproval,
                "{status} must be gated",
            );
        }
    }

    #[test]
    fn unrecognised_status_is_gated_like_unknown() {
        // A garbled / absent status must over-gate, never bypass.
        let policy = ConsentPolicy::new();
        assert!(policy.requires_approval(&cap("b/tool"), ""));
        assert!(policy.requires_approval(&cap("b/tool"), "bogus"));
    }

    #[test]
    fn allowlist_admits_a_gated_capability() {
        let mut policy = ConsentPolicy::new();
        let id = cap("b/github.create_issue");
        assert!(policy.requires_approval(&id, "credentialed"));
        policy.allow(id.clone());
        assert_eq!(policy.decide(&id, "credentialed"), ConsentDecision::Allowed);
        // A different capability is still gated.
        assert!(policy.requires_approval(&cap("b/other"), "credentialed"));
    }

    #[test]
    fn pin_admits_and_lists() {
        let mut policy = ConsentPolicy::new();
        let id = cap("b/slack.post");
        policy.pin(id.clone());
        assert!(policy.is_pinned(&id));
        assert_eq!(policy.decide(&id, "external_api"), ConsentDecision::Allowed);
        assert_eq!(policy.pinned().collect::<Vec<_>>(), vec![&id]);
        policy.unpin(&id);
        assert!(!policy.is_pinned(&id));
        assert!(policy.requires_approval(&id, "external_api"));
    }
}
