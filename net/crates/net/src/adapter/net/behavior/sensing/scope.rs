//! Authority: the v1 owner-root boundary, enforced from session
//! identity (plan §4.10).
//!
//! On-path forwarding solves integrity, not *disclosure*: a
//! downstream may route through this hop yet not be entitled to
//! sense anything, and a relay re-registering upstream is a confused
//! deputy risk. The v1 rule set:
//!
//! - The subscriber's root is **derived from the authenticated
//!   session identity — never trusted from the wire field**. "Wire
//!   says root R" is accepted only when the session identity proves
//!   root R; the wire field exists so tightening later isn't a wire
//!   break, but it is cross-checked, not load-bearing. A claim the
//!   session does not back is protocol-invalid input (security
//!   counter), not merely an unauthorized request.
//! - **Owner-root-only**: a session proving a foreign root is
//!   refused outright — no cross-root claims in v1 (delegation
//!   proofs are the scoped-capabilities follow-up).
//! - The interest digest's audience commitment must equal the proven
//!   root: digest inclusion *separates semantic identities after
//!   validation* — it never replaces this check.
//!
//! The function returns the PROVEN root, and that value — never the
//! wire claim — is what the interest table stores per downstream row
//! (`DownstreamEntry::owner_root`).

use std::sync::atomic::Ordering;

use super::evaluator::SensingCounters;
use super::identity::AudienceScopeCommitment;

/// Why a subscriber's registration was refused at scope validation.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ScopeError {
    /// The wire-claimed root is not the root the session proves —
    /// malformed or forged protocol input (security-relevant): an
    /// honest subscriber's stack always claims its own root.
    WireClaimMismatch,
    /// The session proves a root other than this hop's owner root —
    /// an honest foreign subscriber, refused by the v1 boundary.
    CrossRootRefused,
    /// The interest's audience commitment names a different root
    /// than the session proves — the subscriber asked for an
    /// identity it cannot receive.
    AudienceMismatch,
}

impl ScopeError {
    /// Whether this refusal must increment the protocol-invalid/
    /// security counter (plan §4.10): only the wire claim a session
    /// does not back — cross-root and audience refusals are
    /// authorization outcomes, not protocol violations.
    pub const fn is_security_relevant(self) -> bool {
        matches!(self, Self::WireClaimMismatch)
    }
}

impl std::fmt::Display for ScopeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::WireClaimMismatch => {
                f.write_str("wire-claimed root not backed by session identity")
            }
            Self::CrossRootRefused => f.write_str("cross-root subscriber refused (v1 boundary)"),
            Self::AudienceMismatch => {
                f.write_str("interest audience does not match the session-proven root")
            }
        }
    }
}

impl std::error::Error for ScopeError {}

/// Validate one downstream registration against the v1 owner-root
/// boundary (plan §4.10) and return the **proven** root to store in
/// the table row.
///
/// - `session_root` — derived from the authenticated session
///   identity (v1: [`AudienceScopeCommitment::owner_root`] of the
///   session's `EntityId`); the only load-bearing input.
/// - `claimed_root` — the wire scope field; cross-checked only.
/// - `local_root` — this hop's owner root.
/// - `interest_audience` — the audience commitment the interest
///   digest binds.
pub fn validate_subscriber_scope(
    session_root: &AudienceScopeCommitment,
    claimed_root: &AudienceScopeCommitment,
    local_root: &AudienceScopeCommitment,
    interest_audience: &AudienceScopeCommitment,
    counters: &SensingCounters,
) -> Result<AudienceScopeCommitment, ScopeError> {
    let refuse = |error: ScopeError| {
        counters.scope_refusals.fetch_add(1, Ordering::Relaxed);
        if error.is_security_relevant() {
            counters.protocol_invalid.fetch_add(1, Ordering::Relaxed);
        }
        Err(error)
    };
    if claimed_root != session_root {
        return refuse(ScopeError::WireClaimMismatch);
    }
    if session_root != local_root {
        return refuse(ScopeError::CrossRootRefused);
    }
    if interest_audience != session_root {
        return refuse(ScopeError::AudienceMismatch);
    }
    Ok(*session_root)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn root(byte: u8) -> AudienceScopeCommitment {
        AudienceScopeCommitment::from_bytes([byte; 32])
    }

    fn count(counter: &std::sync::atomic::AtomicU64) -> u64 {
        SensingCounters::get(counter)
    }

    #[test]
    fn happy_path_returns_the_proven_root() {
        let counters = SensingCounters::default();
        let owner = root(0xAA);
        let proven = validate_subscriber_scope(&owner, &owner, &owner, &owner, &counters).unwrap();
        assert_eq!(proven, owner);
        assert_eq!(count(&counters.scope_refusals), 0);
        assert_eq!(count(&counters.protocol_invalid), 0);
    }

    #[test]
    fn wire_claim_is_never_load_bearing() {
        // SI-0 item 5: the session proves the LOCAL owner root — a
        // perfectly authorized subscriber — but the wire field
        // claims something else. The claim must be rejected as
        // protocol-invalid input; being otherwise authorized does
        // not launder a forged field.
        let counters = SensingCounters::default();
        let owner = root(0xAA);
        let forged = root(0xEE);
        assert_eq!(
            validate_subscriber_scope(&owner, &forged, &owner, &owner, &counters),
            Err(ScopeError::WireClaimMismatch),
        );
        assert_eq!(count(&counters.scope_refusals), 1);
        assert_eq!(count(&counters.protocol_invalid), 1);
    }

    #[test]
    fn cross_root_sessions_are_refused_without_security_noise() {
        // An HONEST foreign subscriber: wire claim matches its own
        // proven root, but that root is not ours. Refused by the v1
        // boundary — an authorization outcome, not a protocol
        // violation, so the security counter stays put.
        let counters = SensingCounters::default();
        let ours = root(0xAA);
        let theirs = root(0xBB);
        assert_eq!(
            validate_subscriber_scope(&theirs, &theirs, &ours, &theirs, &counters),
            Err(ScopeError::CrossRootRefused),
        );
        assert_eq!(count(&counters.scope_refusals), 1);
        assert_eq!(count(&counters.protocol_invalid), 0);
    }

    #[test]
    fn interest_audience_must_match_the_proven_root() {
        // The digest separates identities AFTER validation — it
        // never substitutes for it: a local subscriber asking for an
        // interest whose audience commitment names another root is
        // refused.
        let counters = SensingCounters::default();
        let owner = root(0xAA);
        let other_audience = root(0xCC);
        assert_eq!(
            validate_subscriber_scope(&owner, &owner, &owner, &other_audience, &counters),
            Err(ScopeError::AudienceMismatch),
        );
        assert_eq!(count(&counters.scope_refusals), 1);
        assert_eq!(count(&counters.protocol_invalid), 0);
    }
}
