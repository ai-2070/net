// SPDX-License-Identifier: MIT OR Apache-2.0
//! Local policy for membership-only invitations; not an authorization verifier.
//!
//! These values neither encode a signed invite nor issue credentials. The future
//! durable enrollment owner must also verify issuer/scope/device proofs, enforce
//! single use and current authority, and honor explicit pending approvals.
//! Legacy delegation-based enrollment remains unchanged.

use std::time::Duration;

/// Default first-redemption lifetime: 24 hours, not a credential or PSK lifetime.
pub const DEFAULT_INVITATION_TTL: Duration = Duration::from_secs(86_400);

/// Whether invitation creation supplies authorization or a later decision is needed.
/// This mode alone never constitutes proof of authorization.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ApprovalMode {
    /// Creation authorizes the stated scope; no second human approval by default.
    #[default]
    Preauthorized,
    /// The durable owner must obtain approval for the exact claimant and intent.
    RequireApproval,
}

/// Immutable creation-time policy. Inspection cannot restart the expiry clock.
///
/// No wire or persistence representation is specified by this type. A successful
/// time check is not permission to issue or invoke; see the module-level contract.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct InvitationPolicy {
    created_at: u64,
    expires_at: u64,
    approval: ApprovalMode,
}

/// Invalid policy construction or a first-redemption time outside its window.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum PolicyError {
    /// TTL must be positive and representable as an exact number of seconds.
    #[error("invitation TTL must be a positive whole number of seconds")]
    InvalidTtl,
    /// Creation time plus TTL exceeds the timestamp range.
    #[error("invitation expiry timestamp overflow")]
    ExpiryOverflow,
    /// The supplied time precedes creation. No implicit clock-skew allowance.
    #[error("invitation is not yet valid")]
    NotYetValid,
    /// First issuance is forbidden at or after expiry, including late approval.
    #[error("invitation has expired")]
    Expired,
}

impl InvitationPolicy {
    /// Create a preauthorized, 24-hour policy at Unix-seconds `created_at`.
    pub fn new(created_at: u64) -> Result<Self, PolicyError> {
        Self::with_options(created_at, DEFAULT_INVITATION_TTL, ApprovalMode::default())
    }

    /// Create a policy with an explicit TTL and approval mode.
    ///
    /// Fractional seconds are rejected rather than silently rounded. No product
    /// TTL ceiling is imposed here; checked addition rejects timestamp overflow.
    pub fn with_options(
        created_at: u64,
        ttl: Duration,
        approval: ApprovalMode,
    ) -> Result<Self, PolicyError> {
        if ttl.is_zero() || ttl.subsec_nanos() != 0 {
            return Err(PolicyError::InvalidTtl);
        }
        let expires_at = created_at
            .checked_add(ttl.as_secs())
            .ok_or(PolicyError::ExpiryOverflow)?;
        Ok(Self {
            created_at,
            expires_at,
            approval,
        })
    }

    /// Rebuild a persisted policy; `None` unless the expiry follows creation.
    pub(crate) fn from_stored(
        created_at: u64,
        expires_at: u64,
        approval: ApprovalMode,
    ) -> Option<Self> {
        (expires_at > created_at).then_some(Self {
            created_at,
            expires_at,
            approval,
        })
    }

    /// Creation time in Unix seconds.
    pub fn created_at(&self) -> u64 {
        self.created_at
    }

    /// Exclusive first-issuance deadline in Unix seconds.
    pub fn expires_at(&self) -> u64 {
        self.expires_at
    }

    /// Stored mode; inspecting it neither approves nor consumes an invitation.
    pub fn approval_mode(&self) -> ApprovalMode {
        self.approval
    }

    /// Check only the first-redemption time window and return the required mode.
    ///
    /// `Ok(Preauthorized)` is **not** evidence of a valid issuer, device, scope,
    /// unspent invite or current authority. `Ok(RequireApproval)` does not mean
    /// approval occurred. The durable owner must check all those separately and
    /// recheck time before issuance. Committed receipt recovery is a different
    /// operation; this method neither extends expiry nor permits reissuance.
    pub fn check_redemption_at(&self, now: u64) -> Result<ApprovalMode, PolicyError> {
        if now < self.created_at {
            return Err(PolicyError::NotYetValid);
        }
        if now >= self.expires_at {
            return Err(PolicyError::Expired);
        }
        Ok(self.approval)
    }
}
